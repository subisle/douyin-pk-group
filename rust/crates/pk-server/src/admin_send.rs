//! 管理员消息下发（docs/23 §5-§6）：收件人解析 + outbox 队列 + 限速 worker
//! + 展示端「我的」页弹窗消息接口。
//!
//! 触发方是管理员手动动作（/admin/send 页），与自动推送互不相干；
//! 发送不写 last_report_date / last_lineage_date，不顶掉当日自动推送的去重。
//!
//! 结构：
//! - §5.2 TargetSpec 解析与校验（纯函数，可单测）
//! - §5.5 管理端 API（targets / send / messages / enable-lineage，走 admin 鉴权）
//! - §5.4 发送 worker（占行 pending→sending → wxbot 渠道限速发送 → finish）
//! - §6 公开接口（我的页未读弹窗 / 已读回执 / 按需渲染图片，走 X-My-Token）
//!
//! 鉴权口径（与 public_teams::wx::my_ident 同强度）：my token 是
//! public_teams 模块私有的内存/Redis 会话，本模块通过它的 pub(crate)
//! 只读入口 [`public_teams::my_token_peek`] / [`public_teams::my_token_restore`]
//! 解析（peek 语义就是「只读一眼不消费」，访问日志中间件同款），校验失败同样 401；
//! 公开接口一律按 (recipient_name, person_id) 双重过滤，绝不下发他人消息。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::pgrs;
use crate::routes::ApiError;
use crate::state::AppState;
use crate::store::{NewMessage, OutboxRow};

type ApiResult = Result<Json<Value>, ApiError>;
type ArcState = Arc<AppState>;

/* ------------------------------ 小工具 ------------------------------ */

/// bot.rs 的 mgr 是模块私有，这里保持同一语义的最小等价物：
/// 机器人模块没初始化时管理端照常报错（学 bot.rs handlers 第一行的写法）。
fn mgr(st: &AppState) -> anyhow::Result<Arc<crate::bot::BotManager>> {
    st.bots
        .get()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("机器人模块未初始化"))
}

/// 管理员微信机器人 transport（wxbot 行没有扫码凭证时的兜底通道）
fn wechat_transport(st: &AppState) -> Option<Arc<crate::bot::weixin::WxTransport>> {
    st.bots
        .get()
        .and_then(|m| m.wechat.as_ref())
        .map(Arc::clone)
}

/// 日期白名单（bot.rs::valid_date / push.rs::is_biz_date 同口径，两处都是模块私有
/// 不能引）：`YYYY-MM-DD` 且是真实日历日期。长度/字符检查在前——chrono 对
/// "1-1-1" 这类短串照样 Ok。
fn valid_date_str(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 {
        return false;
    }
    for (i, &c) in b.iter().enumerate() {
        let ok = match i {
            4 | 7 => c == b'-',
            _ => c.is_ascii_digit(),
        };
        if !ok {
            return false;
        }
    }
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

/// 今天的业务日（服务器本地时区，与导入运维同时区）
fn today_str() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/* ------------------------------ §5.2 收件人意图 ------------------------------ */

/// 收件人意图（客户端只传意图，展开与校验全在服务端，docs/23 §5.2）
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TargetSpec {
    /// 多选：person.id 列表（去重、过滤已删除）
    Persons(Vec<u64>),
    /// 按师傅：直系徒弟一起收；include_master 默认 true（师傅本人也收一份）
    Masters { ids: Vec<u64>, include_master: bool },
    /// 按团批量：male / female
    Team(&'static str),
    /// 全部在职
    All,
}

/// 展开上限（docs/23 §5.2：防误操作全团轰炸）
pub(crate) const RECIPIENT_CAP: usize = 500;

/// 收件人数上限检查（纯函数，>500 拒绝，文案带人数）
fn check_recipient_cap(n: usize) -> Result<(), ApiError> {
    if n > RECIPIENT_CAP {
        return Err(ApiError::bad_request(format!(
            "收件人过多（{n} 人），请缩小范围"
        )));
    }
    Ok(())
}

/// id 列表解析：只认正整数（0 不是合法 person id），去重保序
fn parse_ids(v: Option<&Value>) -> Result<Vec<u64>, ApiError> {
    let arr = v
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad_request("ids 必须是数字数组"))?;
    let mut out: Vec<u64> = Vec::with_capacity(arr.len());
    let mut seen = std::collections::HashSet::new();
    for x in arr {
        if let Some(id) = x.as_u64() {
            if id > 0 && seen.insert(id) {
                out.push(id);
            }
        }
    }
    Ok(out)
}

/// 解析 body.target（§5.2）。非法 type / 非法团别 / 空 ids 一律 400。
fn parse_target_spec(body: &Value) -> Result<TargetSpec, ApiError> {
    let t = body
        .get("target")
        .ok_or_else(|| ApiError::bad_request("缺少收件人范围 target"))?;
    let ty = t
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("缺少 target.type（persons/masters/team/all）"))?;
    match ty {
        "persons" => {
            let ids = parse_ids(t.get("ids"))?;
            if ids.is_empty() {
                return Err(ApiError::bad_request("请选择收件人"));
            }
            Ok(TargetSpec::Persons(ids))
        }
        "masters" => {
            let ids = parse_ids(t.get("ids"))?;
            if ids.is_empty() {
                return Err(ApiError::bad_request("请选择要按师傅圈人的主播"));
            }
            // includeMaster 缺省 true：按师傅圈人时师傅本人也收一份（docs/23 §5.2）
            let include_master = t
                .get("includeMaster")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            Ok(TargetSpec::Masters {
                ids,
                include_master,
            })
        }
        "team" => {
            let team = t
                .get("team")
                .and_then(Value::as_str)
                .ok_or_else(|| ApiError::bad_request("缺少 target.team（male/female）"))?;
            match team {
                "male" => Ok(TargetSpec::Team("male")),
                "female" => Ok(TargetSpec::Team("female")),
                _ => Err(ApiError::bad_request("target.team 只接受 male/female")),
            }
        }
        "all" => Ok(TargetSpec::All),
        other => Err(ApiError::bad_request(format!(
            "不支持的收件人类型：{other}"
        ))),
    }
}

const IMAGE_KINDS: [&str; 4] = ["none", "lineage", "daily", "month"];
const CHANNELS_ALLOWED: [&str; 2] = ["wxbot", "popup"];

/// POST /api/admin/send 请求体（解析 + 校验，全部纯函数可单测）
#[derive(Debug, Clone)]
pub(crate) struct MsgInput {
    pub title: String,
    pub content: String,
    /// none | lineage | daily | month（白名单内）
    pub image_kind: String,
    /// 图片业务日 YYYY-MM-DD（缺省 = 今天；lineage 渲染用它）
    pub image_date: String,
    /// 去重后的渠道（非空）
    pub channels: Vec<String>,
    pub target: TargetSpec,
    /// 原始请求 JSON（审计可回放，原样落 target_ids）
    pub raw_target: String,
}

/// 解析并校验发送请求体（docs/23 §5.4 / §7 内容上限）。
/// 校验：title ≤64 字、content 非空 ≤1000 字、imageKind 白名单、
/// imageDate 过日期白名单（缺省今天）、channels ⊆ {wxbot, popup} 非空（去重）。
fn parse_send_body(body: &Value) -> Result<MsgInput, ApiError> {
    let title = body
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if title.is_empty() {
        return Err(ApiError::bad_request("请输入消息标题"));
    }
    if title.chars().count() > 64 {
        return Err(ApiError::bad_request("标题太长（最多 64 字）"));
    }
    let content = body
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if content.is_empty() {
        return Err(ApiError::bad_request("请输入消息内容"));
    }
    if content.chars().count() > 1000 {
        return Err(ApiError::bad_request("内容太长（最多 1000 字）"));
    }
    let image_kind = body
        .get("imageKind")
        .and_then(Value::as_str)
        .unwrap_or("none");
    if !IMAGE_KINDS.contains(&image_kind) {
        return Err(ApiError::bad_request(format!(
            "不支持的图片类型：{image_kind}"
        )));
    }
    let image_date = match body.get("imageDate").and_then(Value::as_str).map(str::trim) {
        Some(d) if !d.is_empty() => {
            if !valid_date_str(d) {
                return Err(ApiError::bad_request("图片日期格式应为 YYYY-MM-DD"));
            }
            d.to_string()
        }
        // 缺省 = 今天（服务器本地时区）
        _ => today_str(),
    };
    let channels_raw = body
        .get("channels")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad_request("请至少选择一个发送渠道"))?;
    let mut channels: Vec<String> = Vec::with_capacity(channels_raw.len());
    for c in channels_raw {
        let c = c
            .as_str()
            .ok_or_else(|| ApiError::bad_request("渠道名必须是字符串"))?;
        if !CHANNELS_ALLOWED.contains(&c) {
            return Err(ApiError::bad_request(format!("不支持的渠道：{c}")));
        }
        if !channels.iter().any(|x| x == c) {
            channels.push(c.to_string());
        }
    }
    if channels.is_empty() {
        return Err(ApiError::bad_request("请至少选择一个发送渠道"));
    }
    let target = parse_target_spec(body)?;
    let raw_target = body
        .get("target")
        .map(Value::to_string)
        .unwrap_or_else(|| "null".to_string());
    Ok(MsgInput {
        title,
        content,
        image_kind: image_kind.to_string(),
        image_date,
        channels,
        target,
        raw_target,
    })
}

/* ------------------------------ §5.2 服务端展开 ------------------------------ */

#[derive(Debug, Clone)]
struct Recipient {
    person_id: u64,
    name: String,
}

type PgQuery<'q> = sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>;

async fn persons_by_sql(
    st: &AppState,
    sql: &str,
    bind: impl FnOnce(PgQuery<'_>) -> PgQuery<'_>,
) -> Result<Vec<Recipient>, ApiError> {
    let rows = pgrs::query_with_binds(&st.pools, "LOCAL", sql, bind).await?;
    Ok(rows
        .iter()
        .map(|r| Recipient {
            person_id: r
                .try_get::<i64, _>("id")
                .map(|v| v.max(0) as u64)
                .unwrap_or(0),
            name: r
                .try_get::<Option<String>, _>("name")
                .ok()
                .flatten()
                .unwrap_or_default(),
        })
        .filter(|p| p.person_id > 0)
        .collect())
}

/// 把意图展开成收件人列表（>500 人 400）。口径全部服务端权威（docs/23 §5.2）：
/// 统一过滤 deleted_at IS NULL；批量口径再排除离职（status <> 'left'）。
async fn expand_targets(st: &AppState, spec: &TargetSpec) -> Result<Vec<Recipient>, ApiError> {
    let mut list: Vec<Recipient> = match spec {
        TargetSpec::Persons(ids) => {
            let pids: Vec<i64> = ids.iter().map(|v| *v as i64).collect();
            persons_by_sql(
                st,
                "SELECT id, name FROM person WHERE id = ANY(?) AND deleted_at IS NULL",
                |q| q.bind(pids),
            )
            .await?
        }
        TargetSpec::Masters { ids, include_master } => {
            let mids: Vec<i64> = ids.iter().map(|v| *v as i64).collect();
            let mut merged: std::collections::BTreeMap<u64, String> = std::collections::BTreeMap::new();
            // 直系徒弟（离职/删除不发，docs/23 §5.2 与 §3.1 同口径）
            for p in persons_by_sql(
                st,
                "SELECT id, name FROM person \
                 WHERE master_id = ANY(?) AND deleted_at IS NULL AND status <> 'left'",
                |q| q.bind(mids.clone()),
            )
            .await?
            {
                merged.insert(p.person_id, p.name);
            }
            if *include_master {
                // 师傅本人也收一份（显式选中的，只过滤已删除）
                for p in persons_by_sql(
                    st,
                    "SELECT id, name FROM person WHERE id = ANY(?) AND deleted_at IS NULL",
                    |q| q.bind(mids.clone()),
                )
                .await?
                {
                    merged.insert(p.person_id, p.name);
                }
            }
            merged
                .into_iter()
                .map(|(person_id, name)| Recipient { person_id, name })
                .collect()
        }
        TargetSpec::Team(team) => {
            persons_by_sql(
                st,
                "SELECT id, name FROM person \
                 WHERE deleted_at IS NULL AND status <> 'left' AND gender = ?",
                |q| q.bind(*team),
            )
            .await?
        }
        TargetSpec::All => {
            persons_by_sql(
                st,
                "SELECT id, name FROM person WHERE deleted_at IS NULL AND status <> 'left'",
                |q| q,
            )
            .await?
        }
    };
    // 去重（Persons/Masters 已天然去重；防御性收口）
    let mut seen = std::collections::HashSet::new();
    list.retain(|p| seen.insert(p.person_id));
    check_recipient_cap(list.len())?;
    Ok(list)
}

fn target_type_name(spec: &TargetSpec) -> &'static str {
    match spec {
        TargetSpec::Persons(_) => "persons",
        TargetSpec::Masters { .. } => "masters",
        TargetSpec::Team(_) => "team",
        TargetSpec::All => "all",
    }
}

/* ------------------------------ §5.5 管理端 API ------------------------------ */

#[derive(Deserialize)]
pub struct SendQuery {
    pub q: Option<String>,
    pub limit: Option<i64>,
}

/// GET /api/admin/send/targets?q= —— 收件人候选（姓名模糊匹配，上限 200 行）。
/// hasWx / hasPopup 用 EXISTS 子查询现场判定；discipleCount 供「按师傅圈人」估人数。
/// 不下发手机号等内部字段（docs/23 §10 集成验收口径）。
pub async fn send_targets(State(st): State<ArcState>, Query(q): Query<SendQuery>) -> ApiResult {
    mgr(&st)?;
    let key = q.q.unwrap_or_default();
    let key = key.trim().to_string();
    let like = format!("%{key}%");
    let sql = "SELECT p.id, p.name, p.gender, p.master_id, m.name AS master_name, \
               EXISTS (SELECT 1 FROM push_bindings b \
                       WHERE b.person_id = p.id AND b.conversation_id <> '') AS has_wx, \
               EXISTS (SELECT 1 FROM my_accounts a \
                       WHERE a.person_id = p.id AND a.bound = 1) AS has_popup, \
               (SELECT COUNT(*) FROM person c \
                WHERE c.master_id = p.id AND c.deleted_at IS NULL AND c.status <> 'left') AS disciples \
        FROM person p \
        LEFT JOIN person m ON m.id = p.master_id \
        WHERE p.deleted_at IS NULL AND p.status <> 'left' AND (? = '' OR p.name LIKE ?) \
        ORDER BY p.id LIMIT 200";
    let rows =
        pgrs::query_with_binds(&st.pools, "LOCAL", sql, |qb| qb.bind(key.clone()).bind(like))
            .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "personId": r.try_get::<i64, _>("id").map(|v| v.max(0) as u64).unwrap_or(0),
                "name": r.try_get::<Option<String>, _>("name").ok().flatten().unwrap_or_default(),
                "team": r.try_get::<String, _>("gender").unwrap_or_default(),
                "masterId": r.try_get::<Option<i64>, _>("master_id").ok().flatten().map(|v| v.max(0) as u64),
                "masterName": r.try_get::<Option<String>, _>("master_name").ok().flatten(),
                "hasWx": r.try_get::<bool, _>("has_wx").unwrap_or(false),
                "hasPopup": r.try_get::<bool, _>("has_popup").unwrap_or(false),
                "discipleCount": r.try_get::<i64, _>("disciples").unwrap_or(0),
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

/// POST /api/admin/send —— 落主档 + 展开 × 渠道写 outbox + 起 worker，立即返回。
/// 各渠道可达性（绑没绑微信 bot / 开没开我的页）在写入时判定：
/// 不可达直接写 skipped + 原因（不占 pending 名额，worker 只发可达行）；
/// popup 行没有投递动作，写入即终态 sent，弹窗未读由 reads 表对账。
pub async fn send_create(State(st): State<ArcState>, Json(body): Json<Value>) -> ApiResult {
    mgr(&st)?;
    let input = parse_send_body(&body)?;
    let recipients = expand_targets(&st, &input.target).await?;
    if recipients.is_empty() {
        return Err(ApiError::bad_request(
            "展开后没有可发送的收件人（选中的人可能已删除或离职）",
        ));
    }

    // 渠道可达性一次查全（两类各一条 ANY 查询，不逐人往返）
    let pids: Vec<i64> = recipients.iter().map(|p| p.person_id as i64).collect();
    let want_wx = input.channels.iter().any(|c| c == "wxbot");
    let want_popup = input.channels.iter().any(|c| c == "popup");
    let wx_map: HashMap<u64, String> = if want_wx {
        binding_names_for(&st, &pids).await
    } else {
        HashMap::new()
    };
    let popup_map: HashMap<u64, String> = if want_popup {
        popup_names_for(&st, &pids).await
    } else {
        HashMap::new()
    };

    let m = NewMessage {
        title: input.title.clone(),
        content: input.content.clone(),
        image_kind: input.image_kind.clone(),
        image_date: Some(input.image_date.clone()),
        channels: input.channels.join(","),
        target_type: target_type_name(&input.target).to_string(),
        target_ids: input.raw_target.clone(),
        created_by: "admin".to_string(),
    };
    let message_id = crate::store::admin_message_insert(&st, &m).await?;

    // 收件人 × 勾选渠道 = 一行 outbox（失败互不影响，docs/23 §5.3）
    let mut rows: Vec<OutboxRow> = Vec::with_capacity(recipients.len() * input.channels.len());
    for p in &recipients {
        if want_wx {
            let (status, error) = match wx_map.get(&p.person_id) {
                Some(_) => ("pending", ""),
                None => ("skipped", "未绑定微信 bot"),
            };
            rows.push(OutboxRow {
                id: 0,
                message_id,
                person_id: p.person_id,
                recipient_name: p.name.clone(),
                channel: "wxbot".into(),
                status: status.into(),
                error: error.into(),
            });
        }
        if want_popup {
            let (status, error) = match popup_map.get(&p.person_id) {
                Some(_) => ("sent", ""),
                None => ("skipped", "未开通我的页账号"),
            };
            rows.push(OutboxRow {
                id: 0,
                message_id,
                person_id: p.person_id,
                recipient_name: popup_map.get(&p.person_id).cloned().unwrap_or_default(),
                channel: "popup".into(),
                status: status.into(),
                error: error.into(),
            });
        }
    }
    let queued = crate::store::admin_outbox_insert_batch(&st, message_id, &rows).await?;
    tracing::info!(
        message_id,
        recipients = recipients.len(),
        queued,
        channels = %input.channels.join(","),
        "管理员消息已创建，worker 启动"
    );
    tokio::spawn(worker(st.clone()));
    Ok(Json(json!({
        "ok": true,
        "messageId": message_id,
        "recipients": recipients.len(),
    })))
}

/// 这些 person 的微信绑定名（conversation_id 非空才算绑了 bot）：person_id → 绑定名
async fn binding_names_for(st: &AppState, pids: &[i64]) -> HashMap<u64, String> {
    let mut map = HashMap::new();
    let Ok(rows) = pgrs::query_with_binds(
        &st.pools,
        "LOCAL",
        "SELECT person_id, name FROM push_bindings \
         WHERE person_id = ANY(?) AND conversation_id <> ''",
        |q| q.bind(pids.to_vec()),
    )
    .await
    else {
        return map;
    };
    for r in &rows {
        let pid = r
            .try_get::<i64, _>("person_id")
            .map(|v| v.max(0) as u64)
            .unwrap_or(0);
        let name = r.try_get::<String, _>("name").unwrap_or_default();
        if pid > 0 && !name.is_empty() {
            map.insert(pid, name);
        }
    }
    map
}

/// 这些 person 的我的页账号名（bound = 1）：person_id → 账号名
async fn popup_names_for(st: &AppState, pids: &[i64]) -> HashMap<u64, String> {
    let mut map = HashMap::new();
    let Ok(rows) = pgrs::query_with_binds(
        &st.pools,
        "LOCAL",
        "SELECT person_id, name FROM my_accounts WHERE person_id = ANY(?) AND bound = 1",
        |q| q.bind(pids.to_vec()),
    )
    .await
    else {
        return map;
    };
    for r in &rows {
        let pid = r
            .try_get::<i64, _>("person_id")
            .map(|v| v.max(0) as u64)
            .unwrap_or(0);
        let name = r.try_get::<String, _>("name").unwrap_or_default();
        if pid > 0 && !name.is_empty() {
            map.insert(pid, name);
        }
    }
    map
}

/// GET /api/admin/send/messages?limit= —— 发送历史（主档 + 四态计数）
pub async fn send_messages(State(st): State<ArcState>, Query(q): Query<SendQuery>) -> ApiResult {
    mgr(&st)?;
    let limit = q.limit.unwrap_or(50);
    let items = crate::store::admin_message_list(&st, limit).await?;
    Ok(Json(json!({ "items": items })))
}

/// GET /api/admin/send/messages/{id} —— 详情：主档 + 每收件人每渠道状态（含弹窗已读）
pub async fn send_message_detail(State(st): State<ArcState>, Path(id): Path<i64>) -> ApiResult {
    mgr(&st)?;
    let Some(message) = crate::store::admin_message_get(&st, id).await? else {
        return Err(ApiError::with_status(
            axum::http::StatusCode::NOT_FOUND,
            "消息不存在",
        ));
    };
    let outbox = crate::store::admin_outbox_for_message(&st, id).await?;
    Ok(Json(json!({ "message": message, "outbox": outbox })))
}

/// POST /api/admin/send/enable-lineage —— 一键为所有师傅开启师门日报（docs/23 §4.3）
pub async fn enable_lineage(State(st): State<ArcState>) -> ApiResult {
    mgr(&st)?;
    let enabled = crate::store::admin_lineage_enable_all(&st).await?;
    tracing::info!(enabled, "已为名下有徒弟的绑定批量开启师门日报");
    Ok(Json(json!({ "ok": true, "enabled": enabled })))
}

/* ------------------------------ §5.4 发送 worker ------------------------------ */

/// 行间限速：1.5s + 随机 0..2s 抖动（纳秒取模即可，防 iLink 风控；docs/23 §5.4）
async fn send_gap_sleep() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let jitter = nanos % 2000;
    tokio::time::sleep(std::time::Duration::from_millis(1500 + jitter)).await;
}

/// worker 渲染/发送要用的消息核心字段
#[derive(Debug, Clone)]
struct MsgCore {
    title: String,
    content: String,
    image_kind: String,
    image_date: String,
}

/// 主档 → 核心字段（复用 admin_message_get 的投影 JSON，不重复写 SQL）
async fn msg_core_of(st: &AppState, message_id: i64) -> Option<MsgCore> {
    let v = crate::store::admin_message_get(st, message_id).await.ok()??;
    Some(MsgCore {
        title: v
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        content: v
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        image_kind: v
            .get("imageKind")
            .and_then(Value::as_str)
            .unwrap_or("none")
            .to_string(),
        image_date: v
            .get("imageDate")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

/// worker 主循环：占行取 pending（20 一批）→ 逐行发送 → 队列空即退出。
/// - 占行（pending → sending）防重启双发；卡死在 sending 的行不自动重发（宁漏不重）
/// - 单行失败只写 finish(false) 继续，不影响其它行
/// - 每轮排空**全部** pending 行（不限本条消息）：进程重启后遗留的 pending 行
///   由下一次 POST /api/admin/send 顺带续发（docs/23 §8 的 bootstrap 补拉语义）
pub(crate) async fn worker(st: Arc<AppState>) {
    let wx_global = wechat_transport(&st);
    loop {
        let rows = crate::store::admin_outbox_take_pending(&st, 20).await;
        if rows.is_empty() {
            break;
        }
        // 同一批里同消息主档只查一次
        let mut msgs: HashMap<i64, Option<MsgCore>> = HashMap::new();
        for r in &rows {
            let msg = match msgs.get(&r.message_id) {
                Some(v) => v.clone(),
                None => {
                    let v = msg_core_of(&st, r.message_id).await;
                    msgs.insert(r.message_id, v.clone());
                    v
                }
            };
            let (ok, error) = match msg {
                Some(msg) => match r.channel.as_str() {
                    "wxbot" => send_wxbot_row(&st, r, &msg, wx_global.as_ref()).await,
                    // popup 行写入时已是终态，理论上不会被占行；防御性按已发处理
                    _ => (true, String::new()),
                },
                None => (false, "消息主档不存在".to_string()),
            };
            if !error.is_empty() {
                tracing::info!(
                    outbox = r.id,
                    message = r.message_id,
                    channel = %r.channel,
                    recipient = %r.recipient_name,
                    error = %error,
                    "消息下发行完成（带备注）"
                );
            }
            crate::store::admin_outbox_finish(&st, r.id, ok, &error).await;
            send_gap_sleep().await;
        }
    }
}

/// 消息文本：标题 + 正文（wxbot 只发纯文本，docs/23 §7 注入口径）
fn message_text(title: &str, content: &str) -> String {
    let t = title.trim();
    let c = content.trim();
    if t.is_empty() {
        c.to_string()
    } else {
        format!("{t}\n{c}")
    }
}

/// 该人在 push_bindings 里的可用绑定（conversation_id 非空）：绑定名 + 会话 id
async fn push_binding_for_person(st: &AppState, person_id: u64) -> Option<(String, String)> {
    let rows = pgrs::query_with_binds(
        &st.pools,
        "LOCAL",
        "SELECT name, conversation_id FROM push_bindings \
         WHERE person_id = ? AND conversation_id <> '' LIMIT 1",
        |q| q.bind(person_id as i64),
    )
    .await
    .ok()?;
    let r = rows.first()?;
    let name = r.try_get::<String, _>("name").ok()?;
    let conv = r
        .try_get::<String, _>("conversation_id")
        .unwrap_or_default();
    (!name.is_empty() && !conv.is_empty()).then_some((name, conv))
}

/// 会话上下文解析（push.rs::push_ctx 的最小等价物，不改 push.rs）：
/// to_user 取会话表 / 扫码账号 ilink_user_id 里非空者；
/// 缺 context_token 报可读错误（与现有推送文案一致）。
async fn wx_session_ctx(
    st: &AppState,
    acct: Option<&crate::store::WxPushAccount>,
    conv: &str,
) -> anyhow::Result<(String, String)> {
    let (to_user, context_token) =
        crate::store::wx_session_get(st, conv).await.unwrap_or_default();
    let to_user = if to_user.is_empty() {
        acct.map(|a| a.ilink_user_id.clone()).unwrap_or_default()
    } else {
        to_user
    };
    anyhow::ensure!(
        !context_token.is_empty(),
        "等待你在微信里给机器人发送一条消息以激活推送"
    );
    Ok((to_user, context_token))
}

/// wxbot 渠道发一条文本：优先扫码账号（user_send_text），没有凭证时回落管理员机器人
/// （send_proactive，push.rs::wx_proactive 同款语义）。
async fn wx_send_text(
    st: &AppState,
    acct: Option<&crate::store::WxPushAccount>,
    wx_global: Option<&Arc<crate::bot::weixin::WxTransport>>,
    conv: &str,
    text: &str,
) -> anyhow::Result<()> {
    if let Some(a) = acct {
        let (to_user, ctx) = wx_session_ctx(st, Some(a), conv).await?;
        return crate::bot::weixin::user_send_text(&a.base_url, &a.bot_token, &to_user, &ctx, text)
            .await;
    }
    let Some(wx) = wx_global else {
        anyhow::bail!("没有可用的微信推送通道（请先在「我的」页扫码绑定）");
    };
    wx.send_proactive(conv, text).await
}

/// 渲染并逐页发送师门日报图（正文已单独发过，首图不带说明文字；
/// 第 2 页起带页码提示，与 push.rs::send_binding_images 的分页口径一致）。
/// 返回发送页数。
async fn wx_send_lineage_images(
    st: &AppState,
    a: &crate::store::WxPushAccount,
    conv: &str,
    rep: &crate::lineage_report::LineageReport,
    watermark: &str,
) -> anyhow::Result<usize> {
    let (to_user, ctx) = wx_session_ctx(st, Some(a), conv).await?;
    let svgs = crate::lineage_report::render_svg_pages(rep, 40, watermark);
    anyhow::ensure!(!svgs.is_empty(), "师门日报没有可渲染的页面");
    let mut pages = 0usize;
    for (i, svg) in svgs.iter().enumerate() {
        let png = match crate::push::svg_to_png(svg) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(page = i + 1, error = %e, "师门日报 SVG 转 PNG 失败，跳过该页");
                continue;
            }
        };
        let cap = if i == 0 {
            String::new()
        } else {
            format!("📄 师门日报第 {} 页（接上条）", i + 1)
        };
        crate::store::wxmedia::user_send_image(&a.base_url, &a.bot_token, &to_user, &ctx, &cap, &png)
            .await?;
        pages += 1;
    }
    anyhow::ensure!(pages > 0, "全部页面转 PNG 失败");
    Ok(pages)
}

/// 发送一行 wxbot 任务。返回 (是否成功, error 备注)。
///
/// 语义（docs/23 §5.4 / §8 边界与降级）：
/// - 文本先发（消息正文是主载荷）；正文失败 = 整行失败
/// - image_kind=lineage：正文后按收件人实时渲染发图；无徒弟 / 构建失败 / 转码失败
///   一律降级为已发的纯文本，原因记进 outbox.error（状态仍是 sent）
/// - image_kind=daily/month：本期图片未开放，降级纯文本并备注
async fn send_wxbot_row(
    st: &Arc<AppState>,
    r: &OutboxRow,
    msg: &MsgCore,
    wx_global: Option<&Arc<crate::bot::weixin::WxTransport>>,
) -> (bool, String) {
    let Some((bname, bconv)) = push_binding_for_person(st, r.person_id).await else {
        return (false, "未绑定微信 bot".into());
    };
    let acct = crate::store::wx_push_account_get(st, &bname)
        .await
        .filter(|a| !a.bot_token.is_empty());
    // 会话优先扫码账号自己记的；账号没记时用绑定行的
    let conv = acct
        .as_ref()
        .map(|a| a.conversation_id.clone())
        .filter(|c| !c.is_empty())
        .unwrap_or(bconv);
    let text = message_text(&msg.title, &msg.content);

    if let Err(e) = wx_send_text(st, acct.as_ref(), wx_global, &conv, &text).await {
        return (false, e.to_string());
    }

    match msg.image_kind.as_str() {
        "lineage" => {
            if r.person_id == 0 {
                return (true, "管理员观看账号没有师门数据，已发纯文本".into());
            }
            let Some(a) = acct.as_ref() else {
                return (
                    true,
                    "图片推送需要扫码账号（请在「我的」页重新扫码），已发纯文本".into(),
                );
            };
            let date = if valid_date_str(&msg.image_date) {
                msg.image_date.clone()
            } else {
                today_str()
            };
            match crate::lineage_report::build(st, &date, r.person_id).await {
                Ok(rep) if rep.disciple_count() > 0 => {
                    match wx_send_lineage_images(st, a, &conv, &rep, &r.recipient_name).await {
                        Ok(_) => (true, String::new()),
                        Err(e) => (true, format!("图片发送失败，已发纯文本：{e}")),
                    }
                }
                Ok(_) => (true, "名下没有在职徒弟，已降级纯文本".into()),
                Err(e) => (true, format!("图片生成失败，已发纯文本：{e}")),
            }
        }
        // daily / month 渲染器本期未接，一律降级纯文本并备注（docs/23 §5.4）
        "daily" | "month" => (true, "图片类型暂未开放".into()),
        _ => (true, String::new()),
    }
}

/* ------------------------------ §6 公开接口（我的页弹窗） ------------------------------ */

/// 从请求头 / query / body 里取 my token。优先 X-My-Token 头（唯一正规入口）；
/// query `?token=` 兜底（图片端点等带不了自定义头的场景，只在本模块三个公开口上认）；
/// body.token 仅 read 接口用（POST JSON body 不进 URL/日志）。
fn my_token_of(
    headers: &HeaderMap,
    query_token: Option<&str>,
    body_token: Option<&str>,
) -> Option<String> {
    headers
        .get("x-my-token")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| {
            query_token
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
        .or_else(|| {
            body_token
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
}

/// my token → (personId, 团别, 账号姓名)。内存 miss 时走 Redis 回填
/// （public_teams::my_token_peek / my_token_restore，都是 pub(crate) 只读入口）。
async fn my_ident(st: &AppState, token: &str) -> Result<(u64, String, String), ApiError> {
    if let Some(v) = crate::public_teams::my_token_peek(token) {
        return Ok(v);
    }
    if let Some(v) = crate::public_teams::my_token_restore(st, token).await {
        return Ok(v);
    }
    Err(ApiError::auth("登录已过期，请重新登录"))
}

#[derive(Deserialize)]
pub struct MyMsgQuery {
    pub token: Option<String>,
}

/// GET /api/public/my/messages —— 该人未读弹窗消息（最新在前）。
/// 按 (recipient_name, person_id) 双重过滤：同名不同人也绝不串。
pub async fn my_messages(
    State(st): State<ArcState>,
    Query(q): Query<MyMsgQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let token = my_token_of(&headers, q.token.as_deref(), None)
        .ok_or_else(|| ApiError::auth("缺少验证 token，请先登录"))?;
    let (person_id, _team, name) = my_ident(&st, &token).await?;
    let items = crate::store::admin_unread_for(&st, &name, person_id).await?;
    Ok(Json(json!({ "items": items })))
}

/// POST /api/public/my/messages/{id}/read —— 写已读回执（幂等）。
/// 请求体可空（token 走头 / query）；先校验该人确实是这条消息的 popup 收件人。
pub async fn my_message_read(
    State(st): State<ArcState>,
    Path(id): Path<i64>,
    Query(q): Query<MyMsgQuery>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult {
    let body_val: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    let token = my_token_of(
        &headers,
        q.token.as_deref(),
        body_val.get("token").and_then(Value::as_str),
    )
    .ok_or_else(|| ApiError::auth("缺少验证 token，请先登录"))?;
    let (person_id, _team, name) = my_ident(&st, &token).await?;
    let recipient = crate::store::admin_popup_recipient_exists(&st, id, &name, person_id).await?;
    if !recipient {
        return Err(ApiError::with_status(
            axum::http::StatusCode::NOT_FOUND,
            "消息不存在或不属于你",
        ));
    }
    crate::store::admin_message_read_mark(&st, id, &name, person_id).await?;
    Ok(Json(json!({ "ok": true })))
}

/// GET /api/public/my/messages/{id}/image.png —— 按收件人实时渲染消息配图。
/// 水印 = 本人姓名；只对 imageKind=lineage 出图（daily/month 本期未开放），
/// 渲染失败 / 非收件人 / 无图消息一律 404，不挡正文。
pub async fn my_message_image(
    State(st): State<ArcState>,
    Path(id): Path<i64>,
    Query(q): Query<MyMsgQuery>,
    headers: HeaderMap,
) -> Response {
    let not_found = || {
        (
            axum::http::StatusCode::NOT_FOUND,
            "图片不可用",
        )
            .into_response()
    };
    let Some(token) = my_token_of(&headers, q.token.as_deref(), None) else {
        return (axum::http::StatusCode::UNAUTHORIZED, "请先登录").into_response();
    };
    let Ok((person_id, _team, name)) = my_ident(&st, &token).await else {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            "登录已过期，请重新登录",
        )
            .into_response();
    };
    let Ok(recipient) = crate::store::admin_popup_recipient_exists(&st, id, &name, person_id).await
    else {
        return not_found();
    };
    if !recipient {
        return not_found();
    }
    let Ok(Some(msg)) = crate::store::admin_message_get(&st, id).await else {
        return not_found();
    };
    let kind = msg.get("imageKind").and_then(Value::as_str).unwrap_or("none");
    if kind != "lineage" {
        return not_found();
    }
    let date = {
        let d = msg
            .get("imageDate")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if valid_date_str(d) {
            d.to_string()
        } else {
            today_str()
        }
    };
    if person_id == 0 {
        return not_found();
    }
    let Ok(rep) = crate::lineage_report::build(&st, &date, person_id).await else {
        return not_found();
    };
    let svgs = crate::lineage_report::render_svg_pages(&rep, 40, &name);
    let Some(svg) = svgs.first() else {
        return not_found();
    };
    match crate::push::svg_to_png(svg) {
        Ok(png) => (
            [(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("image/png"),
            )],
            png,
        )
            .into_response(),
        Err(_) => not_found(),
    }
}

/* ------------------------------ 单测（纯函数） ------------------------------ */

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(body: Value) -> Result<MsgInput, ApiError> {
        parse_send_body(&body)
    }

    /* ---------- target 解析 ---------- */

    #[test]
    fn target_missing_rejected() {
        let e = parse(json!({
            "title": "t", "content": "c", "channels": ["wxbot"]
        }));
        assert!(e.is_err(), "缺 target 必须 400");
    }

    #[test]
    fn target_type_illegal_rejected() {
        let e = parse(json!({
            "title": "t", "content": "c", "channels": ["wxbot"],
            "target": { "type": "nonsense", "ids": [1] }
        }));
        assert!(e.is_err(), "非法 type 必须 400");
    }

    #[test]
    fn target_type_missing_rejected() {
        let e = parse(json!({
            "title": "t", "content": "c", "channels": ["wxbot"], "target": { "ids": [1] }
        }));
        assert!(e.is_err());
    }

    #[test]
    fn persons_dedupe_keeps_order() {
        let m = parse(json!({
            "title": "t", "content": "c", "channels": ["wxbot"],
            "target": { "type": "persons", "ids": [7, 3, 7, 1, 3, 0, "x", 9] }
        }))
        .ok()
        .unwrap();
        // 去重保序；0 与非数字丢弃
        assert_eq!(m.target, TargetSpec::Persons(vec![7, 3, 1, 9]));
    }

    #[test]
    fn persons_empty_ids_rejected() {
        let e = parse(json!({
            "title": "t", "content": "c", "channels": ["wxbot"],
            "target": { "type": "persons", "ids": [] }
        }));
        assert!(e.is_err(), "空 ids 必须 400");
    }

    #[test]
    fn masters_include_master_defaults_true() {
        let m = parse(json!({
            "title": "t", "content": "c", "channels": ["wxbot"],
            "target": { "type": "masters", "ids": [5] }
        }))
        .ok()
        .unwrap();
        match m.target {
            TargetSpec::Masters { ids, include_master } => {
                assert_eq!(ids, vec![5]);
                assert!(include_master, "includeMaster 缺省应为 true");
            }
            other => panic!("应为 Masters：{other:?}"),
        }
    }

    #[test]
    fn masters_include_master_explicit_false_dedupes_ids() {
        let m = parse(json!({
            "title": "t", "content": "c", "channels": ["wxbot"],
            "target": { "type": "masters", "ids": [5, 5, 6], "includeMaster": false }
        }))
        .ok()
        .unwrap();
        match m.target {
            TargetSpec::Masters { ids, include_master } => {
                assert_eq!(ids, vec![5, 6], "ids 去重保序");
                assert!(!include_master);
            }
            other => panic!("应为 Masters：{other:?}"),
        }
    }

    #[test]
    fn team_whitelist() {
        let ok = parse(json!({
            "title": "t", "content": "c", "channels": ["popup"],
            "target": { "type": "team", "team": "female" }
        }))
        .ok()
        .unwrap();
        assert_eq!(ok.target, TargetSpec::Team("female"));
        let bad = parse(json!({
            "title": "t", "content": "c", "channels": ["popup"],
            "target": { "type": "team", "team": "both" }
        }));
        assert!(bad.is_err(), "team 只接受 male/female");
        let missing = parse(json!({
            "title": "t", "content": "c", "channels": ["popup"],
            "target": { "type": "team" }
        }));
        assert!(missing.is_err());
    }

    #[test]
    fn all_ok() {
        let m = parse(json!({
            "title": "t", "content": "c", "channels": ["wxbot", "popup"],
            "target": { "type": "all" }
        }))
        .ok()
        .unwrap();
        assert_eq!(m.target, TargetSpec::All);
    }

    /* ---------- 内容校验 ---------- */

    #[test]
    fn title_over_limit_rejected() {
        let long = "标".repeat(65);
        let e = parse(json!({
            "title": long, "content": "c", "channels": ["wxbot"],
            "target": { "type": "all" }
        }));
        assert!(e.is_err(), "title > 64 字必须 400");
    }

    #[test]
    fn title_64_chars_ok() {
        let t = "标".repeat(64);
        let m = parse(json!({
            "title": t, "content": "c", "channels": ["wxbot"],
            "target": { "type": "all" }
        }))
        .ok()
        .unwrap();
        assert_eq!(m.title.chars().count(), 64);
    }

    #[test]
    fn content_limits() {
        // 非空
        let e = parse(json!({
            "title": "t", "content": "   ", "channels": ["wxbot"],
            "target": { "type": "all" }
        }));
        assert!(e.is_err(), "纯空白 content 必须 400");
        // 超限（1001 字）
        let long = "字".repeat(1001);
        let e = parse(json!({
            "title": "t", "content": long, "channels": ["wxbot"],
            "target": { "type": "all" }
        }));
        assert!(e.is_err(), "content > 1000 字必须 400");
        // 恰好 1000 字通过
        let ok = "字".repeat(1000);
        let m = parse(json!({
            "title": "t", "content": ok, "channels": ["wxbot"],
            "target": { "type": "all" }
        }))
        .ok()
        .unwrap();
        assert_eq!(m.content.chars().count(), 1000);
    }

    /* ---------- 图片字段 ---------- */

    #[test]
    fn image_kind_whitelist() {
        let bad = parse(json!({
            "title": "t", "content": "c", "channels": ["wxbot"], "imageKind": "bogus",
            "target": { "type": "all" }
        }));
        assert!(bad.is_err(), "imageKind 不在白名单必须 400");
        let ok = parse(json!({
            "title": "t", "content": "c", "channels": ["wxbot"], "imageKind": "month",
            "target": { "type": "all" }
        }))
        .ok()
        .unwrap();
        assert_eq!(ok.image_kind, "month");
    }

    #[test]
    fn image_kind_defaults_none_and_date_defaults_today() {
        let m = parse(json!({
            "title": "t", "content": "c", "channels": ["wxbot"],
            "target": { "type": "all" }
        }))
        .ok()
        .unwrap();
        assert_eq!(m.image_kind, "none", "imageKind 缺省 none");
        assert!(
            valid_date_str(&m.image_date),
            "imageDate 缺省应为今天的合法日期：{}",
            m.image_date
        );
    }

    #[test]
    fn image_date_whitelist() {
        let bad = parse(json!({
            "title": "t", "content": "c", "channels": ["wxbot"], "imageDate": "2026-02-30",
            "target": { "type": "all" }
        }));
        assert!(bad.is_err(), "不存在的日历日必须 400");
        let bad2 = parse(json!({
            "title": "t", "content": "c", "channels": ["wxbot"], "imageDate": "1-1-1",
            "target": { "type": "all" }
        }));
        assert!(bad2.is_err(), "短串必须 400（chrono 对它照样 Ok）");
        let bad3 = parse(json!({
            "title": "t", "content": "c", "channels": ["wxbot"], "imageDate": "2026-10-14; DROP TABLE x",
            "target": { "type": "all" }
        }));
        assert!(bad3.is_err(), "带杂质的日期必须 400");
        let ok = parse(json!({
            "title": "t", "content": "c", "channels": ["wxbot"], "imageDate": "2026-10-14",
            "target": { "type": "all" }
        }))
        .ok()
        .unwrap();
        assert_eq!(ok.image_date, "2026-10-14");
        // 空串 = 缺省今天
        let ok2 = parse(json!({
            "title": "t", "content": "c", "channels": ["wxbot"], "imageDate": "",
            "target": { "type": "all" }
        }))
        .ok()
        .unwrap();
        assert!(valid_date_str(&ok2.image_date));
    }

    /* ---------- 渠道白名单 ---------- */

    #[test]
    fn channels_whitelist() {
        let bad = parse(json!({
            "title": "t", "content": "c", "channels": ["qq"],
            "target": { "type": "all" }
        }));
        assert!(bad.is_err(), "渠道不在 wxbot/popup 必须 400");
        let empty = parse(json!({
            "title": "t", "content": "c", "channels": [],
            "target": { "type": "all" }
        }));
        assert!(empty.is_err(), "空渠道必须 400");
        let missing = parse(json!({
            "title": "t", "content": "c",
            "target": { "type": "all" }
        }));
        assert!(missing.is_err(), "缺 channels 必须 400");
        let non_str = parse(json!({
            "title": "t", "content": "c", "channels": [1],
            "target": { "type": "all" }
        }));
        assert!(non_str.is_err(), "非字符串渠道必须 400");
        let ok = parse(json!({
            "title": "t", "content": "c", "channels": ["wxbot", "popup", "wxbot"],
            "target": { "type": "all" }
        }))
        .ok()
        .unwrap();
        assert_eq!(ok.channels, vec!["wxbot", "popup"], "渠道去重保序");
    }

    /* ---------- 上限 ---------- */

    #[tokio::test]
    async fn recipient_cap_rejects_over_500() {
        assert!(check_recipient_cap(500).is_ok(), "恰好 500 放行");
        let e = check_recipient_cap(501).err().unwrap();
        // ApiError 的字段私有、也不实现 Display：转成响应再读 JSON error 文案
        let resp = e.into_response();
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap(),
        )
        .unwrap();
        let msg = body.get("error").and_then(Value::as_str).unwrap_or_default();
        assert!(msg.contains("501"), "错误文案要带人数：{msg}");
        assert!(msg.contains("缩小范围"));
    }

    /* ---------- 日期 / 文本小工具 ---------- */

    #[test]
    fn valid_date_str_matches_bot_valid_date_semantics() {
        assert!(valid_date_str("2026-10-14"));
        assert!(valid_date_str("2024-02-29")); // 闰年
        assert!(!valid_date_str("2023-02-29")); // 平年
        assert!(!valid_date_str("2026-10-1"));
        assert!(!valid_date_str("2026/10/14"));
        assert!(!valid_date_str(""));
    }

    #[test]
    fn message_text_joins_title_and_content() {
        assert_eq!(message_text("标题", "正文"), "标题\n正文");
        assert_eq!(message_text("", "正文"), "正文");
        assert_eq!(message_text("  标题  ", " 正文 "), "标题\n正文");
    }
}
