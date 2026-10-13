//! 管理端数据域（B 阶段）：主播台账 + 导入预览/提交/审计
//!
//! 边界（docs/00 §4/§5）：
//! - 读走 LOCAL 只读池（pk_reader），写走 douyin_writer 池（`st.writer`），分账号隔离
//! - SQL 全部集中在 dialect.rs 常量 / 本文件的静态常量，值经 bind 绑定
//! - 导入幂等：daily_metric 按 uk_person_date upsert；批次落 import_batch 审计
//! - 团别 male/female 是导入与台账的必填口径，冲突行标红由调用方确认

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use calamine::{open_workbook_auto_from_rs, Reader};
use chrono::NaiveDate;
use pk_core::roster::{parse_roster, ParseOptions};
use pk_core::types::ParsedRow;
use pk_core::{default_aliases, normalize_name};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};

use crate::dialect;
use crate::pgrs;
use crate::pgrs::PgResultExt;
use crate::routes::ApiError;
use crate::state::AppState;
use crate::store::WxPushAccount;
use crate::writer;

type ApiResult = Result<Json<Value>, ApiError>;
type ArcState = Arc<AppState>;

/// `.xlsx` / `.xls` → TSV，交给现有 CSV / 文本解析器。
/// 运营经常直接转发 Excel 附件；按 UTF-8 读二进制会产生乱码行，必须在解析前转成表格文本。
pub fn spreadsheet_to_text(bytes: &[u8]) -> anyhow::Result<String> {
    let mut workbook = open_workbook_auto_from_rs(std::io::Cursor::new(bytes))
        .map_err(|e| anyhow::anyhow!("无法读取 Excel 文件：{e}"))?;
    let first = workbook
        .worksheet_range_at(0)
        .ok_or_else(|| anyhow::anyhow!("Excel 文件里没有工作表"))?
        .map_err(|e| anyhow::anyhow!("无法读取 Excel 工作表：{e}"))?;
    let mut out = String::new();
    for row in first.rows() {
        let cells: Vec<String> = row
            .iter()
            .map(|cell: &calamine::Data| cell.to_string().replace(['\n', '\r', '\t'], " "))
            .collect();
        out.push_str(&cells.join("\t"));
        out.push('\n');
    }
    anyhow::ensure!(out.trim().len() < 16 * 1024 * 1024, "Excel 转文本后过大");
    Ok(out)
}

pub fn maybe_spreadsheet_text(filename: &str, bytes: &[u8]) -> anyhow::Result<Option<String>> {
    let is_xlsx = bytes.len() >= 4 && bytes[..4] == [0x50, 0x4b, 0x03, 0x04];
    let ext = filename
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if is_xlsx || matches!(ext.as_str(), "xlsx" | "xlsm" | "xls") {
        return spreadsheet_to_text(bytes).map(Some);
    }
    Ok(None)
}

/* ------------------------------- 公共小工具 ------------------------------- */

fn read_u64(row: &sqlx::postgres::PgRow, col: &str) -> Option<u64> {
    row.try_get::<i64, _>(col).ok().map(|v| v.max(0) as u64)
}

fn read_opt_u64(row: &sqlx::postgres::PgRow, col: &str) -> Option<u64> {
    row.try_get::<Option<i64>, _>(col)
        .ok()
        .flatten()
        .map(|v| v.max(0) as u64)
}

fn read_opt_i64(row: &sqlx::postgres::PgRow, col: &str) -> Option<i64> {
    row.try_get::<Option<i64>, _>(col).ok().flatten()
}

fn read_opt_str(row: &sqlx::postgres::PgRow, col: &str) -> Option<String> {
    row.try_get::<Option<String>, _>(col).ok().flatten()
}

fn read_str(row: &sqlx::postgres::PgRow, col: &str) -> String {
    row.try_get::<String, _>(col).unwrap_or_default()
}

/// 管理端只对本地库操作；没有 LOCAL 数据源直接报错
fn require_local(st: &AppState) -> Result<(), ApiError> {
    if st.sources.iter().any(|s| s.id == "LOCAL") {
        Ok(())
    } else {
        Err(
            anyhow::anyhow!("管理端需要 LOCAL 数据源（douyin_writer 对应的本地 PostgreSQL）")
                .into(),
        )
    }
}

fn writer_pool(st: &AppState) -> Result<&PgPool, ApiError> {
    st.writer.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "写库不可用：douyin_writer 账号未配置或连不上（检查 DB_WRITER_* 环境变量），只读功能不受影响"
        )
        .into()
    })
}

/// 「不分男女团」导入：team 传 auto / mixed / 空 都走自动 ——
/// 团别按主播档案逐行判定（一个文件里男女混着也能导），不再要求运营分两个文件。
pub fn is_auto_team(team: &str) -> bool {
    matches!(
        team.trim().to_ascii_lowercase().as_str(),
        "" | "auto" | "mixed" | "mix" | "all"
    )
}

/// 团别白名单（P1-6 审计后改 pub：bot/mod.rs 的 push_run_now 入口复用同一套校验）
pub fn valid_team(team: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        matches!(team, "male" | "female"),
        "团别必须是 male 或 female，收到：{team}"
    );
    Ok(())
}

/// ApiResult 函数里用的前置校验（ensure! 的宏展开是 anyhow::Error，不自动转 ApiError）
fn ensure(cond: bool, msg: impl std::fmt::Display) -> Result<(), ApiError> {
    if cond {
        Ok(())
    } else {
        Err(anyhow::anyhow!("{msg}").into())
    }
}

/// 2026-10-12 审计 L3：导入上传的扩展名白名单。
///
/// 以解析端实际支持的格式为准：CSV/TSV/文本兜底走 `parse_any_text`，
/// Excel 走 `maybe_spreadsheet_text`（xlsx/xlsm/xls，含 PK 头嗅探）。
/// 白名单收口到「csv / tsv / txt / xlsx / xlsm / xls」，其余一律 400 拒收，
/// 防止把上传口当任意文件投递通道。判断只看扩展名（大小写不敏感）。
fn allowed_upload_ext(filename: &str) -> bool {
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(ext.as_str(), "csv" | "tsv" | "txt" | "xlsx" | "xlsm" | "xls")
}

/// multipart 字段读取失败 → ApiError：保留 axum 已分类的状态码（超限 → 413）。
///
/// 之前用 `anyhow::anyhow!("...: {e}")` 包一层，只留下 Display 文本，
/// body-limit 的语义丢了，超限请求掉进兜底 500（审计 L3 实测 5MB 文件返回 500）。
fn multipart_error_to_api(e: axum::extract::multipart::MultipartError) -> ApiError {
    if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
        // 全局上限是 main.rs 里的 DefaultBodyLimit::max(4*1024*1024)，文案跟着它写
        ApiError::with_status(StatusCode::PAYLOAD_TOO_LARGE, "文件超过 4MB 上限")
    } else {
        ApiError::with_status(e.status(), format!("读取上传内容失败: {e}"))
    }
}

/// 业务日期格式白名单（P1-6 审计后改 pub：bot/mod.rs 的 push_run_now 入口复用同一套校验）
pub fn valid_biz_date(date: &str) -> anyhow::Result<NaiveDate> {
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|_| anyhow::anyhow!("业务日期格式必须是 YYYY-MM-DD，收到：{date}"))
}

async fn merged_aliases(st: &AppState) -> HashMap<String, String> {
    let mut m = default_aliases();
    m.extend(crate::store::list_aliases(st).await);
    m
}

/* ------------------------------ 主播台账 CRUD ------------------------------ */

#[derive(Deserialize)]
pub struct StreamerQuery {
    pub q: Option<String>,
    pub team: Option<String>,
}

/// 台账行查询：016 起带 `phone` / `has_account`，库没跑 016（甚至 015）时自动退回
/// [`dialect::STREAMERS_SQL_LEGACY`]。
///
/// 为什么不直接改 SQL 让迁移强制跑：生产 RK3318 的迁移由 `pk-server migrate` 显式触发，
/// 服务先于迁移重启是常态（docs/12 §6.2 的 G0~G2 灰度），台账页是运营**唯一**的入口页面，
/// 一条 Unknown column 就整页打不开了。两套 SQL 差异只在列清单，行序与过滤条件完全一致。
async fn streamer_rows(st: &AppState) -> anyhow::Result<Vec<sqlx::postgres::PgRow>> {
    match pgrs::query(&st.pools, "LOCAL", dialect::STREAMERS_SQL).await {
        Ok(rows) => Ok(rows),
        Err(e) if is_missing_phone_column(&e) => {
            tracing::warn!(
                "person.phone / my_accounts.person_id 列不存在（016/015 未跑），台账按旧列清单返回"
            );
            pgrs::query(&st.pools, "LOCAL", dialect::STREAMERS_SQL_LEGACY).await
        }
        Err(e) => Err(e),
    }
}

/// 错误是不是「016/015 的列还没有」（与 dialect::is_missing_month_table 同款判定）
fn is_missing_phone_column(e: &anyhow::Error) -> bool {
    let m = e.to_string();
    m.contains("Unknown column")
        || m.contains("doesn't exist")
        || m.contains("does not exist")
        || m.contains("Unknown table")
}

/// 台账「抖音号个数」：一条 GROUP BY 把全量 account 计数捞成 map，避免逐行查库（2026-10-12）。
///
/// 查询失败只记 warn 并返回空 map —— 台账是运营唯一入口页，不能因为一个计数列整页 500。
/// 空 map 时调用方按 0 兜底（与"没有账号 = 0"语义一致，只是暂时少显示）。
async fn douyin_counts(st: &AppState) -> HashMap<u64, i64> {
    match pgrs::query(
        &st.pools,
        "LOCAL",
        "SELECT person_id, COUNT(*) AS cnt FROM account GROUP BY person_id",
    )
    .await
    {
        Ok(rows) => rows
            .iter()
            .filter_map(|r| Some((read_u64(r, "person_id")?, read_opt_i64(r, "cnt")?)))
            .collect(),
        Err(e) => {
            tracing::warn!("account 分组计数（douyinCount）查询失败，一律按 0 兜底：{e}");
            HashMap::new()
        }
    }
}

/// 从全量 `wx_push_accounts` 里挑出属于某个主播的那一行（2026-10-12）。
///
/// 归属优先级：
/// 1. `bot.person_id == person_id`（且 person_id > 0）：018 之后的自助扫码数据带 person_id，最准；
/// 2. 兜底：`bot.person_id == 0 && bot.name == person_name`。015 之前的老数据没有 person_id，
///    0 是「未绑定 / 管理员观看账号」哨兵，只能退化成按展示端账号名对。
///
/// 同一个人命中多行时取 `bound_at` 最新的一行（018 之后理论上不会，但存量可能有）。
fn pick_wxbot<'a>(
    bots: &'a [WxPushAccount],
    person_id: u64,
    person_name: &str,
) -> Option<&'a WxPushAccount> {
    if person_id > 0 {
        let exact = newest_by_bound_at(bots.iter().filter(|b| b.person_id == person_id));
        if exact.is_some() {
            return exact;
        }
    }
    newest_by_bound_at(
        bots.iter()
            .filter(|b| b.person_id == 0 && b.name == person_name),
    )
}

/// 在一批候选里取 `bound_at` 最新的一行；空串视为「最旧」永远排最后（2026-10-12）。
///
/// bound_at 是 `YYYY-MM-DD HH:MM:SS` 定长格式，字典序即时间序，直接比字符串即可。
fn newest_by_bound_at<'a, I>(cands: I) -> Option<&'a WxPushAccount>
where
    I: Iterator<Item = &'a WxPushAccount>,
{
    fn key(s: &str) -> Option<&str> {
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
    cands.max_by(|a, b| key(&a.bound_at).cmp(&key(&b.bound_at)))
}

/// GET /api/admin/streamers?q=&team=：主播台账（含主号账号 + 业务库别名合并）
pub async fn streamers(State(st): State<ArcState>, Query(q): Query<StreamerQuery>) -> ApiResult {
    require_local(&st)?;
    let rows = streamer_rows(&st).await?;
    let alias_map = merged_aliases(&st).await;
    // wxbot：全量捞一次（失败已返回空 Vec）+ 一条 GROUP BY 建 map，循环里只查内存（2026-10-12）
    let bots = crate::store::wx_push_accounts_all(&st).await;
    let douyin_count = douyin_counts(&st).await;
    let mut items: Vec<Value> = Vec::with_capacity(rows.len());
    for r in &rows {
        let name = read_str(r, "name");
        let team = read_str(r, "gender");
        if let Some(want) = q.team.as_deref().filter(|s| !s.is_empty()) {
            if team != want {
                continue;
            }
        }
        if let Some(w) = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let n = normalize_name(&name);
            let key = normalize_name(w);
            if !n.contains(&key) && !key.contains(&n) {
                continue;
            }
        }
        let own_aliases: Vec<&String> = alias_map
            .iter()
            .filter(|(_, real)| normalize_name(real) == normalize_name(&name))
            .map(|(a, _)| a)
            .collect();
        let person_id = read_u64(r, "id").unwrap_or(0);
        // 「扫了哪个 wxbot」看的是 wx_push_accounts，不是 push_bindings 的推送开关（2026-10-12）
        let wxbot = pick_wxbot(&bots, person_id, &name).map(|b| {
            json!({
                "bound": true,
                "activated": !b.conversation_id.is_empty(),
                "nickname": b.nickname,
                "boundAt": b.bound_at,
            })
        });
        items.push(json!({
            "personId": person_id,
            "name": name,
            "team": team,
            "masterId": read_opt_u64(r, "master_id"),
            "masterName": read_opt_str(r, "master_name"),
            "disciples": read_u64(r, "disciple_count").unwrap_or(0),
            "generation": read_opt_i64(r, "generation"),
            "groupName": read_opt_str(r, "group_name"),
            "avatar": read_opt_str(r, "avatar_url"),
            "status": read_opt_str(r, "status").unwrap_or_else(|| "active".into()),
            "hideInDailyReport": read_opt_i64(r, "hide_in_daily_report").unwrap_or(0) != 0,
            "joinedAt": read_opt_str(r, "joined"),
            "anchorId": read_opt_str(r, "anchor_id"),
            "douyinNo": read_opt_str(r, "douyin_no"),
            "douyinNos": read_opt_str(r, "all_douyin_nos"),
            // 该主播 account 行数（含没登记抖音号的号）；没账号 = 0（2026-10-12）
            "douyinCount": douyin_count.get(&person_id).copied().unwrap_or(0),
            "anchorName": read_opt_str(r, "anchor_name"),
            // 主播手机号（016 起；老库没这列 = 空串，前端照常显示"未登记"）
            "phone": read_opt_str(r, "phone").unwrap_or_default(),
            // 是否已开「展示端登录账号」（docs/13：104 人里现在只有 1 人有号）
            "hasAccount": read_u64(r, "has_account").unwrap_or(0) > 0,
            // 微信机器人绑定状态（push_bindings 按姓名 1:1；没绑过 = false）
            "wechatBound": read_opt_i64(r, "wechat_bound").unwrap_or(0) != 0,
            "wechatPush": read_opt_i64(r, "wechat_push").unwrap_or(0) != 0,
            // wxbot = wx_push_accounts 里属于这个人的那一行（有没有扫过码）；没扫过 = null（2026-10-12）
            "wxbot": wxbot,
            "aliases": own_aliases,
        }));
    }
    Ok(Json(json!({ "items": items, "total": items.len() })))
}

#[derive(Deserialize)]
pub struct StreamerBody {
    pub name: Option<String>,
    pub team: Option<String>,
    #[serde(rename = "douyinNo")]
    pub douyin_no: Option<String>,
    #[serde(rename = "anchorId")]
    pub anchor_id: Option<String>,
    #[serde(rename = "masterId")]
    pub master_id: Option<u64>,
    pub generation: Option<i64>,
    #[serde(rename = "groupName")]
    pub group_name: Option<String>,
    pub status: Option<String>,
    #[serde(rename = "joinedAt")]
    pub joined_at: Option<String>,
    /// 主播手机号（016 起，选填）。运营在台账里登记，是批量开号的默认来源。
    /// 库没跑 016 时写入会被拒（person_set_phone 翻成「先跑迁移」提示）。
    pub phone: Option<String>,
    /// 每日报告显隐（2026-10-09）：true = 不出现在对外榜单（接口不下发这个人）
    #[serde(rename = "hideInDailyReport")]
    pub hide_in_daily_report: Option<bool>,
}

/// POST /api/admin/streamers：建档（name + team 必填；douyinNo 全局唯一）
pub async fn create_streamer(
    State(st): State<ArcState>,
    Json(body): Json<StreamerBody>,
) -> ApiResult {
    require_local(&st)?;
    let pool = writer_pool(&st)?;
    let name = body
        .name
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("名字必填"))?;
    let team = body
        .team
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("团别必填（male / female）"))?;
    valid_team(&team)?;
    ensure(
        body.status.is_none()
            || matches!(
                body.status.as_deref(),
                Some("active") | Some("left") | Some("paused")
            ),
        "status 只能是 active / left / paused",
    )?;

    // 名字唯一：同名主播靠别名区分，不靠重名
    let dup = pgrs::query_with_binds(&st.pools, "LOCAL", dialect::STREAMER_BY_NAME_SQL, |q| {
        q.bind(name.clone())
    })
    .await?;
    if let Some(row) = dup.into_iter().next() {
        let exist = read_str(&row, "name");
        return Err(anyhow::anyhow!("同名主播「{exist}」已存在，请改用别名映射").into());
    }

    // 抖音号全局唯一（docs/06：重复时行内红字提示）
    let douyin_no = body
        .douyin_no
        .clone()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    check_douyin_no_free(&st, douyin_no.as_deref(), None).await?;

    // 手机号先校验（格式 + 占用），不合法在建档前就拒掉，别建了人再报错
    let phone = body
        .phone
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if let Some(p) = phone.as_deref() {
        crate::store::validate_my_phone(p).map_err(|e| anyhow::anyhow!(e))?;
        // 016 没跑时这条查询会报 Unknown column：当「查不到冲突」处理。
        // 建档失败比"手机号没存上"严重得多（运营要重录一遍），不能让少跑的迁移挡住建档。
        let taken =
            pgrs::query_with_binds(&st.pools, "LOCAL", dialect::PERSON_PHONE_TAKEN_SQL, |q| {
                q.bind(p.to_string()).bind(0i64)
            })
            .await
            .unwrap_or_default();
        if let Some(row) = taken.into_iter().next() {
            let owner = read_str(&row, "name");
            return Err(anyhow::anyhow!("手机号已被在职主播「{owner}」登记").into());
        }
    }

    let mut tx = pool.begin().await?;
    let res = sqlx::query(&crate::pq::rewrite(
        "INSERT INTO person (name, gender, master_id, generation, group_name, status, joined_at)
         VALUES (?,?,?,?,?,?,?) RETURNING id",
    ))
    .bind(&name)
    .bind(&team)
    .bind(body.master_id.map(|v| v as i64))
    .bind(body.generation.map(|v| v as i32))
    .bind(&body.group_name)
    .bind(body.status.as_deref().unwrap_or("active"))
    .bind(&body.joined_at)
    .fetch_one(&mut *tx)
    .await?;
    let person_id = res.try_get::<i64, _>(0)? as u64;

    if let Some(no) = douyin_no.or_else(|| {
        body.anchor_id
            .clone()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }) {
        let anchor_id = body
            .anchor_id
            .clone()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| no.clone());
        sqlx::query(&crate::pq::rewrite(
            "INSERT INTO account (person_id, anchor_id, douyin_no, anchor_name, is_primary)
             VALUES (?,?,?,?,1)",
        ))
        .bind(person_id as i64)
        .bind(&anchor_id)
        .bind(&no)
        .bind(&name)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_account_err(e, &anchor_id))?;
    }
    tx.commit().await?;

    /* 手机号单独写（016 才有的列）：库没跑迁移时只 warn，不回滚已建成的主播 ——
    建档失败会让运营丢一遍录入，比"手机号没存上"严重得多。 */
    if let Some(p) = phone.as_deref() {
        if let Err(e) = crate::store::person_set_phone(&st, person_id, p).await {
            tracing::warn!(person_id, error = %e, "建档后写手机号失败（016 迁移没跑？）");
        }
    }

    // 建档后把暂存区里这个抖音号 / 采集 ID 的行补账进 daily_metric（「建档后才显示」）
    let absorbed = absorb_pending_best_effort(&st, person_id, "create_streamer").await;

    Ok(Json(
        json!({ "ok": true, "personId": person_id, "absorbed": absorbed }),
    ))
}

async fn check_douyin_no_free(
    st: &AppState,
    douyin_no: Option<&str>,
    except_person: Option<u64>,
) -> Result<(), ApiError> {
    let Some(no) = douyin_no else {
        return Ok(());
    };
    let rows = pgrs::query_with_binds(&st.pools, "LOCAL", dialect::DOUYIN_NO_TAKEN_SQL, |q| {
        q.bind(no.to_string())
    })
    .await?;
    for r in rows {
        let taken_id = read_u64(&r, "id").unwrap_or(0);
        if except_person != Some(taken_id) {
            let holder = read_str(&r, "name");
            return Err(
                anyhow::anyhow!("抖音号 {no} 已被「{holder}」使用，抖音号必须全局唯一").into(),
            );
        }
    }
    Ok(())
}

fn map_account_err(e: sqlx::Error, anchor_id: &str) -> anyhow::Error {
    let msg = e.to_string();
    if (msg.contains("duplicate key") || msg.contains("Duplicate entry")) && msg.contains(anchor_id)
    {
        anyhow::anyhow!("采集 ID {anchor_id} 已存在（anchor_id 唯一），请检查是否重复建档")
    } else {
        anyhow::anyhow!("账号写入失败: {e}")
    }
}

/// UPDATE 语句 SET 子句的值绑定（列名白名单，值全部 bind）
enum Bind {
    S(String),
    U(Option<i64>),
    I(Option<i64>),
}

impl Bind {
    fn into_query(
        self,
        query: sqlx::query::Query<'_, sqlx::Postgres, sqlx::postgres::PgArguments>,
    ) -> sqlx::query::Query<'_, sqlx::Postgres, sqlx::postgres::PgArguments> {
        match self {
            Bind::S(s) => query.bind(s),
            Bind::U(u) => query.bind(u),
            Bind::I(i) => query.bind(i),
        }
    }
}

/// PUT /api/admin/streamers/{id}：编辑（masterId=0 表示清空师傅）
pub async fn update_streamer(
    State(st): State<ArcState>,
    Path(id): Path<u64>,
    Json(body): Json<StreamerBody>,
) -> ApiResult {
    require_local(&st)?;
    let pool = writer_pool(&st)?;
    ensure(id > 0, "无效的主播 id")?;
    if let Some(team) = body.team.as_deref() {
        valid_team(team)?;
    }
    ensure(
        body.status.is_none()
            || matches!(
                body.status.as_deref(),
                Some("active") | Some("left") | Some("paused")
            ),
        "status 只能是 active / left / paused",
    )?;
    let douyin_no = body
        .douyin_no
        .clone()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    check_douyin_no_free(&st, douyin_no.as_deref(), Some(id)).await?;

    let mut sets: Vec<&'static str> = Vec::new();
    let mut binds: Vec<Bind> = Vec::new();
    if let Some(v) = body
        .name
        .clone()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        sets.push("name = ?");
        binds.push(Bind::S(v));
    }
    if let Some(v) = body.team.clone() {
        sets.push("gender = ?");
        binds.push(Bind::S(v));
    }
    if let Some(v) = body.master_id {
        // 0 = 清空师傅；否则写主键
        sets.push("master_id = ?");
        binds.push(Bind::U((v > 0).then_some(v as i64)));
    }
    if let Some(v) = body.generation {
        sets.push("generation = ?");
        binds.push(Bind::I(Some(v)));
    }
    if let Some(v) = body.group_name.clone() {
        sets.push("group_name = ?");
        binds.push(Bind::S(v));
    }
    if let Some(v) = body.status.clone() {
        sets.push("status = ?");
        binds.push(Bind::S(v));
    }
    if let Some(v) = body.hide_in_daily_report {
        sets.push("hide_in_daily_report = ?");
        binds.push(Bind::I(Some(v as i64)));
    }
    /* 手机号不走上面的 SET 拼接：它要过格式校验 + 两侧查重 + 「016 没跑」的中文提示，
    单独在下面调 person_set_phone（失败 = 整条编辑失败，运营要看见原因）。 */
    let phone_in = body.phone.clone();
    if !sets.is_empty() {
        let sql = format!("UPDATE person SET {} WHERE id = ?", sets.join(", "));
        writer::write_guard(&sql)?;
        let sql = crate::pq::rewrite(&sql);
        let mut query = sqlx::query(&sql);
        for b in binds {
            query = b.into_query(query);
        }
        query.bind(id as i64).execute(pool).await?;
    }

    if let Some(p) = phone_in.as_deref() {
        crate::store::person_set_phone(&st, id, p)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
    }

    // 抖音号：upsert 到主号账号行
    if let Some(no) = douyin_no {
        let anchor: Option<String> = sqlx::query(&crate::pq::rewrite(dialect::PRIMARY_ANCHOR_SQL))
            .bind(id as i64)
            .map(|r: sqlx::postgres::PgRow| r.try_get::<String, _>(0).ok())
            .fetch_optional(pool)
            .await?
            .flatten();
        match anchor {
            Some(a) => {
                sqlx::query(&crate::pq::rewrite(
                    "UPDATE account SET douyin_no = ? WHERE anchor_id = ?",
                ))
                .bind(&no)
                .bind(&a)
                .execute(pool)
                .await?;
            }
            None => {
                sqlx::query(&crate::pq::rewrite(
                    "INSERT INTO account (person_id, anchor_id, douyin_no, anchor_name, is_primary)
                     VALUES (?,?,?,?,1)",
                ))
                .bind(id as i64)
                .bind(&no) // 没有独立采集 ID 时用抖音号兜底
                .bind(&no)
                .bind(body.name.clone().unwrap_or_default())
                .execute(pool)
                .await
                .map_err(|e| map_account_err(e, &no))?;
            }
        }
        // 改了抖音号 → 暂存区里这个号的行现在能对上号了，补账
        let absorbed = absorb_pending_best_effort(&st, id, "update_streamer").await;
        return Ok(Json(
            json!({ "ok": true, "personId": id, "absorbed": absorbed }),
        ));
    }

    Ok(Json(json!({ "ok": true, "personId": id })))
}

/// DELETE /api/admin/streamers/{id}：软删（deleted_at 置时间，历史数据保留）
pub async fn delete_streamer(State(st): State<ArcState>, Path(id): Path<u64>) -> ApiResult {
    require_local(&st)?;
    let pool = writer_pool(&st)?;
    let res = sqlx::query(&crate::pq::rewrite(
        "UPDATE person SET deleted_at = now() WHERE id = ? AND deleted_at IS NULL",
    ))
    .bind(id as i64)
    .execute(pool)
    .await?;
    if res.affected() == 0 {
        return Err(anyhow::anyhow!("主播不存在或已删除").into());
    }
    Ok(Json(json!({ "ok": true })))
}

/* ----------------------------- 导入：解析 + 匹配 ----------------------------- */

/// 一行导入数据的原始解析结果
#[derive(Clone, Debug, Default, Deserialize, serde::Serialize)]
pub struct ImportRow {
    pub name: String,
    #[serde(rename = "douyinNo")]
    pub douyin_no: Option<String>,
    /// 采集 ID / 主播 id（运营文件里的「主播id / uid / anchor_id」列）。
    /// 与抖音号二选一即可认人 —— 用户口径 2026-10-10：「只要有抖音号或者 id 都导入」。
    #[serde(rename = "anchorId")]
    pub anchor_id: Option<String>,
    /// 当日音浪（直接给值时视为可信）
    pub wave: Option<f64>,
    /// 当日累计总音浪（快照口径；给了它就按差分算 wave）
    #[serde(rename = "cumulativeWave")]
    pub cumulative_wave: Option<f64>,
    pub minutes: Option<f64>,
    #[serde(rename = "isLive")]
    pub is_live: Option<bool>,
    /// 解析失败的原因（时长/音浪列有值但解析不过）。
    ///
    /// 月度时长导入里这个字段必须活到落库阶段：以前 `parse_duration_only` 直接把
    /// 解析失败的行 `filter` 掉，运营只见「预览少了一个人」却没有任何线索可查，
    /// 而漏一个人 = 他整月时长在月榜上是 0。现在带原因进 skipped 报告。
    #[serde(skip_deserializing)]
    pub err: Option<String>,
}

/// 审计/暂存文本列宽（UTF-8 字符数）。import_batch.note VARCHAR(512)、
/// filename VARCHAR(255)、import_pending.name VARCHAR(128) / douyin_no VARCHAR(64)。
/// STRICT_TRANS_TABLES 下超长是 1406 直接炸掉整个导入事务 —— 2026-10-10 覆盖导入踩过，
/// 摘要/备注这类「审计文本」一律先截断再 bind。
const NOTE_MAX_CHARS: usize = 500;
const FILENAME_MAX_CHARS: usize = 255;
const PENDING_NAME_MAX_CHARS: usize = 128;
const IDENT_MAX_CHARS: usize = 64;
/// 失败摘要在备注里最多列几个名字（其余用「等」收口，配合 NOTE_MAX_CHARS 双保险）
const NOTE_MAX_NAMES: usize = 30;

/// 按字符截断（CJK / emoji 安全），超出部分用 … 收口
fn truncate_chars(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// 行里的「认人标识」：抖音号优先，其次采集 ID。都没有 = None（暂存也没意义）。
fn row_ident(row: &ImportRow) -> Option<String> {
    [row.douyin_no.as_deref(), row.anchor_id.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(|s| truncate_chars(s, IDENT_MAX_CHARS))
}

#[derive(Clone, serde::Serialize)]
struct ResolvedRow {
    raw: ImportRow,
    #[serde(rename = "personId")]
    person_id: Option<u64>,
    name: String,
    matched: bool,
    #[serde(rename = "teamConflict")]
    team_conflict: Option<String>,
    candidates: Vec<Value>,
    /// 命中方式：true = 按抖音号精确匹配（false = 按名字）
    #[serde(rename = "matchedByDouyin")]
    matched_by_douyin: bool,
}

fn token_is_bool(t: &str) -> Option<bool> {
    match t {
        "开播" | "直播" | "true" | "是" | "yes" | "y" | "1" => Some(true),
        "未开播" | "false" | "否" | "no" | "n" | "0" => Some(false),
        _ => None,
    }
}

fn token_num(t: &str) -> Option<f64> {
    let cleaned: String = t
        .chars()
        .filter(|c| !matches!(c, ',' | '，' | ' '))
        .collect();
    if cleaned.is_empty()
        || !cleaned
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == '-')
    {
        return None;
    }
    cleaned.parse().ok()
}

/// 表头行关键词：字段名级别的词（不含「开播」这类会出现在值里的词）
const HEADER_KEYWORDS: [&str; 8] = [
    "名字",
    "姓名",
    "抖音号",
    "当日音浪",
    "累计音浪",
    "直播时长",
    "name",
    "wave",
];

/// 判断一行里的每个 ASCII 逗号是否都是数字千分位（形如 `11,370,000`）。
/// 全部是 → 返回剔除千分位逗号后的行；有任意一个不是 → 原样返回（逗号当分隔符）。
fn strip_thousand_commas(line: &str) -> String {
    let b = line.as_bytes();
    let is_thousand = |i: usize| -> bool {
        i > 0
            && b[i - 1].is_ascii_digit()
            && i + 3 < b.len()
            && b[i + 1].is_ascii_digit()
            && b[i + 2].is_ascii_digit()
            && b[i + 3].is_ascii_digit()
            && (i + 4 == b.len() || !b[i + 4].is_ascii_digit())
    };
    let comma_positions: Vec<usize> = b
        .iter()
        .enumerate()
        .filter(|(_, &c)| c == b',')
        .map(|(i, _)| i)
        .collect();
    if !comma_positions.is_empty() && comma_positions.iter().all(|&i| is_thousand(i)) {
        line.chars().filter(|&c| c != ',').collect()
    } else {
        line.to_string()
    }
}

/// 粘贴文本解析：每行一人，制表符 / 中文逗号 / 空格分隔（ASCII 逗号优先按千分位理解）。
/// 列序不严格：第一个 token 是名字；数字按 当日音浪 → 累计 → 分钟 依次吸收；
/// 开播类 token 解析成 is_live；其余非数字 token 视为抖音号。
pub fn parse_import_text(text: &str) -> Vec<ImportRow> {
    let mut rows = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        // 表头行：含字段名关键词就跳过
        let lower = line.to_lowercase();
        if HEADER_KEYWORDS.iter().any(|k| lower.contains(k)) {
            continue;
        }
        let line = strip_thousand_commas(line);
        // 千分位逗号已剔除；剩下的 ASCII 逗号 / 中文逗号 / 制表符都是分隔符
        let tokens: Vec<&str> = if line.contains('\t') {
            line.split('\t').collect()
        } else if line.contains('，') {
            line.split('，').collect()
        } else if line.contains(',') {
            line.split(',').collect()
        } else {
            line.split_whitespace().collect()
        };
        let mut row = ImportRow {
            name: tokens.first().unwrap_or(&"").trim().to_string(),
            ..Default::default()
        };
        let mut num_slot = 0; // 0=wave 1=cumulative 2=minutes
        for t in tokens.iter().skip(1) {
            let t = t.trim();
            if t.is_empty() {
                continue;
            }
            if let Some(b) = token_is_bool(t) {
                row.is_live = Some(b);
            } else if let Some(n) = token_num(t) {
                match num_slot {
                    0 => row.wave = Some(n),
                    1 => row.cumulative_wave = Some(n),
                    _ => row.minutes = Some(n),
                }
                num_slot += 1;
            } else {
                row.douyin_no = Some(t.to_string());
            }
        }
        if !row.name.is_empty() {
            rows.push(row);
        }
    }
    rows
}

struct LocalStreamer {
    id: u64,
    name: String,
    gender: String,
}

/// LOCAL 主播名册（导入匹配 + 团别冲突判断的数据源）
async fn load_local_streamers(st: &AppState) -> anyhow::Result<Vec<LocalStreamer>> {
    let rows = streamer_rows(st).await?;
    Ok(rows
        .iter()
        .map(|r| LocalStreamer {
            id: read_u64(r, "id").unwrap_or(0),
            name: read_str(r, "name"),
            gender: read_str(r, "gender"),
        })
        .collect())
}

/// 导入匹配上下文：person 台账 + 抖音号/采集ID → person 映射。
/// 一人多账号：同一人的多个抖音号都指向同一 person_id（多号归一）。
struct MatchCtx {
    streamers: Vec<LocalStreamer>,
    by_douyin: HashMap<String, u64>,
    /// 采集 ID（anchor_id）→ person：运营文件里只给「主播id」时也认得出
    by_anchor: HashMap<String, u64>,
}

impl MatchCtx {
    #[cfg(test)]
    fn from_streamers(streamers: Vec<LocalStreamer>) -> Self {
        Self {
            streamers,
            by_douyin: HashMap::new(),
            by_anchor: HashMap::new(),
        }
    }
}

/// 加载匹配上下文（台账 + 抖音号/采集ID 映射）
async fn load_match_ctx(st: &AppState) -> anyhow::Result<MatchCtx> {
    let streamers = load_local_streamers(st).await?;
    let rows = pgrs::query(&st.pools, "LOCAL", dialect::ACCOUNT_DOUYIN_MAP_SQL).await?;
    let mut by_douyin: HashMap<String, u64> = HashMap::new();
    for r in &rows {
        if let (Some(pid), Some(no)) = (read_opt_u64(r, "person_id"), read_opt_str(r, "douyin_no"))
        {
            let no = no.trim().to_string();
            if !no.is_empty() {
                by_douyin.insert(no, pid);
            }
        }
    }
    let anchor_rows = pgrs::query(&st.pools, "LOCAL", dialect::ACCOUNT_ANCHOR_MAP_SQL).await?;
    let mut by_anchor: HashMap<String, u64> = HashMap::new();
    for r in &anchor_rows {
        if let (Some(pid), Some(aid)) = (read_opt_u64(r, "person_id"), read_opt_str(r, "anchor_id"))
        {
            let aid = aid.trim().to_string();
            if !aid.is_empty() {
                by_anchor.insert(aid, pid);
            }
        }
    }
    Ok(MatchCtx {
        streamers,
        by_douyin,
        by_anchor,
    })
}

fn resolve_row(
    row: &ImportRow,
    ctx: &MatchCtx,
    aliases: &HashMap<String, String>,
    expect_team: Option<&str>,
) -> ResolvedRow {
    let streamers = &ctx.streamers;
    // 名字先过别名表
    let name = aliases
        .get(row.name.trim())
        .cloned()
        .unwrap_or_else(|| row.name.trim().to_string());
    let key = normalize_name(&name);
    /* 匹配优先级：抖音号精确 → 采集 ID 精确（两列都试）→ 名字归一化 → 名字原文。
    2026-10-10 月度时长导入踩到：运营文件里「抖音号 + 主播ID」两列都有时老实现只看抖音号，
    而台账里的账号可能只有采集 ID、没有抖音号（早期采集数据），整批人就白白匹配不上。
    两个列都是身份证，谁命中都算命中；`matchedByDouyin` 仍表示「按号码命中」而不是按名字。 */
    let ident_hit = row
        .douyin_no
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|no| ctx.by_douyin.get(no).or_else(|| ctx.by_anchor.get(no)))
        .or_else(|| {
            row.anchor_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .and_then(|aid| ctx.by_anchor.get(aid).or_else(|| ctx.by_douyin.get(aid)))
        });
    let douyin_hit = ident_hit.and_then(|pid| streamers.iter().find(|s| s.id == *pid));
    let hit = douyin_hit
        .or_else(|| streamers.iter().find(|s| normalize_name(&s.name) == key))
        .or_else(|| streamers.iter().find(|s| s.name == name));
    let team_conflict = match (hit, expect_team) {
        (Some(s), Some(team)) if s.gender != team => Some(format!(
            "主播「{}」团别是 {}，与导入团别 {team} 不符",
            s.name, s.gender
        )),
        _ => None,
    };
    let candidates: Vec<Value> = if hit.is_none() {
        streamers
            .iter()
            .filter(|s| {
                let n = normalize_name(&s.name);
                n.contains(&key) || key.contains(&n)
            })
            .take(5)
            .map(|s| json!({ "personId": s.id, "name": s.name, "team": s.gender }))
            .collect()
    } else {
        Vec::new()
    };
    ResolvedRow {
        raw: row.clone(),
        person_id: hit.map(|s| s.id),
        name,
        matched: hit.is_some(),
        team_conflict,
        candidates,
        matched_by_douyin: douyin_hit.is_some(),
    }
}

/// 统一解析入口：带表头的运营 CSV（别名/单位）走 csvparse，否则回落裸列序解析
/// 文本里是不是明显混进了二进制（xlsx / zip / 老 xls / 其它压缩流）。
///
/// 为什么需要：xlsx 转文本漏判时，整段 ZIP 流会被当成「名字 / 抖音号」塞进
/// import_pending，后台暂存区显示成一屏乱码，且**每一个乱码都会变成一个待建档主播**。
/// 判据是控制字符密度——正常名单文本里几乎不会有 0x00-0x08 这类字节。
pub fn looks_like_binary(text: &str) -> bool {
    let sample: &[u8] = &text.as_bytes()[..text.len().min(4096)];
    if sample.is_empty() {
        return false;
    }
    // ZIP/xlsx 的文件头，见到直接判死
    if sample.len() >= 4 && sample[..4] == [0x50, 0x4b, 0x03, 0x04] {
        return true;
    }
    let bad = sample
        .iter()
        .filter(|&&b| b < 0x09 || (0x0e..0x20).contains(&b))
        .count();
    bad * 100 / sample.len() > 5
}

pub fn parse_any_text(text: &str) -> Vec<ImportRow> {
    // 二进制护栏：宁可整批解析不出让运营重发，也不让一屏乱码变成一批待建档主播
    if looks_like_binary(text) {
        return Vec::new();
    }
    if let Ok((kind, out)) = crate::csvparse::parse(text) {
        if kind != crate::csvparse::Kind::Unknown && !out.rows.is_empty() {
            let mapped: Vec<ImportRow> = out
                .rows
                .into_iter()
                .map(|r| ImportRow {
                    name: if r.name.is_empty() {
                        r.anchor_id.clone()
                    } else {
                        r.name
                    },
                    wave: r.wave,
                    cumulative_wave: None,
                    minutes: r.minutes,
                    is_live: None,
                    douyin_no: (!r.douyin_no.is_empty()).then_some(r.douyin_no),
                    // 「主播id / uid / anchor_id」列也留着认人（用户口径：有 id 就导入）
                    anchor_id: (!r.anchor_id.is_empty()).then_some(r.anchor_id),
                    err: None,
                })
                .filter(|r| !r.name.is_empty())
                .collect();
            // csvparse 认出了表头、却一行可用名字都没剩下，说明类型判错了：
            // 「名字,当日音浪,累计,开播」里的「开播」会被 detect_kind 判成时长表，
            // 名字列对不上 → 整批行丢空。这时不能直接返回空，回落裸列序解析，
            // 前端提示承诺的「列序不严格 / 开播未开播自动识别」才成立。
            if !mapped.is_empty() {
                return mapped;
            }
        }
    }
    parse_import_text(text)
}

/// 月度时长文件解析（用户口径 2026-10-10：时长按月导入）。
///
/// 文件样例（抖音公会后台导出「9月份时长.csv」）：
/// `主播ID,主播昵称,抖音号,…,分成比-直播音浪收益,开播有效时长,主播标签`
/// → 只取「时长」列（75小时29分钟41秒 = 整月累计），绝不把「分成比-直播音浪收益」
/// 当音浪读进来（那会让每人当月音浪变成 35）。
///
/// 不回落裸列序解析：月度文件里数字多（粉丝数 / 分成比 / 时间），
/// 按列序猜会把粉丝数当成音浪、分成比当成累计，宁可解析不出让运营换文件。
pub fn parse_month_duration_text(text: &str) -> Vec<ImportRow> {
    if looks_like_binary(text) {
        return Vec::new();
    }
    crate::csvparse::parse_duration_only(text)
        .into_iter()
        .map(|r| ImportRow {
            name: if r.name.is_empty() {
                r.anchor_id.clone()
            } else {
                r.name
            },
            douyin_no: (!r.douyin_no.is_empty()).then_some(r.douyin_no),
            anchor_id: (!r.anchor_id.is_empty()).then_some(r.anchor_id),
            minutes: r.minutes,
            // CsvRow.err 是 String（空串 = 没问题），转成 Option 好让上层区分
            // 「没填时长」和「填了但解析不过」——后者是运营要立刻知道的数据问题。
            err: (!r.err.is_empty()).then_some(r.err),
            ..Default::default()
        })
        .filter(|r| !r.name.is_empty())
        .collect()
}

/// 月度时长导入的月份：`YYYY-MM`
fn valid_period(period: &str) -> anyhow::Result<()> {
    let p = period.trim();
    let ok = p.len() == 7
        && p.as_bytes()[4] == b'-'
        && p[..4].chars().all(|c| c.is_ascii_digit())
        && p[5..].chars().all(|c| c.is_ascii_digit())
        && matches!(p[5..].parse::<u32>(), Ok(1..=12));
    anyhow::ensure!(ok, "月份格式应为 YYYY-MM（例如 2026-09）");
    Ok(())
}

/// 月度时长导入 / 删除时，业务日期（import_batch.biz_date 是 DATE）用当月 1 号占位：
/// 审计表按天存，月报没有「哪一天」的概念，1 号 + 文件名里的月份足够定位。
fn period_first_day(period: &str) -> String {
    format!("{}-01", period.trim())
}

async fn body_rows(
    text: &Option<String>,
    rows: &Option<Vec<ImportRow>>,
) -> anyhow::Result<(Vec<ImportRow>, usize)> {
    let mut out: Vec<ImportRow> = Vec::new();
    if let Some(rs) = rows {
        for r in rs {
            let mut r = r.clone();
            r.name = r.name.trim().to_string();
            if !r.name.is_empty() {
                out.push(r);
            }
        }
    }
    if out.is_empty() {
        let text = text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("缺少导入内容：text（粘贴文本）或 rows（结构化行）至少给一个")
            })?;
        out = parse_any_text(text);
    }
    let dropped = strip_minutes(&mut out);
    Ok((out, dropped))
}

/// 按日导入不再写时长（用户口径 2026-10-10：「每日不能导入时长，只有月时长」）。
/// 时长只有一个月度来源（monthly_duration，走 /import/month/*）；按日导入这里把行里的
/// minutes 抹掉并计数，让预览/提交都能明确告诉运营「文件里的时长列已忽略」。
fn strip_minutes(rows: &mut [ImportRow]) -> usize {
    let mut n = 0;
    for r in rows.iter_mut() {
        if r.minutes.is_some() {
            r.minutes = None;
            n += 1;
        }
    }
    n
}

/// 月度时长导入的行：text → 只认「时长」列（parse_month_duration_text）；
/// rows → 前端回传的结构化行，没有时长的行直接丢掉（月度导入没有时长就没意义）。
async fn month_body_rows(
    text: &Option<String>,
    rows: &Option<Vec<ImportRow>>,
) -> anyhow::Result<Vec<ImportRow>> {
    let mut out: Vec<ImportRow> = Vec::new();
    if let Some(rs) = rows {
        for r in rs {
            let mut r = r.clone();
            r.name = r.name.trim().to_string();
            if !r.name.is_empty() && r.minutes.is_some() {
                out.push(r);
            }
        }
    }
    if out.is_empty() {
        let text = text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("缺少导入内容：text（文件内容）或 rows（结构化行）至少给一个")
            })?;
        out = parse_month_duration_text(text);
    }
    Ok(out)
}

/* ----------------------------- 导入：预览 / 提交 ----------------------------- */

#[derive(Deserialize)]
pub struct ImportBody {
    pub text: Option<String>,
    pub rows: Option<Vec<ImportRow>>,
    #[serde(rename = "bizDate")]
    pub biz_date: Option<String>,
    /// 月度时长导入的业务月份 YYYY-MM（/import/month/* 用；按日导入不看它）
    pub period: Option<String>,
    pub team: Option<String>,
    /// merge（幂等覆盖，默认）/ reject（当日已有数据则整批拒绝）
    pub mode: Option<String>,
    pub filename: Option<String>,
}

/// 从数据行推断团别：所有行命中的主播是同一团别 → 返回该团别；
/// 混合团别或全部未匹配 → None（需要人工指定）。一天两个文件（男团/女团各一）
/// 的场景下大多数文件可自动判断，不再追问。
pub async fn infer_team(st: &AppState, rows: &[ImportRow]) -> Option<String> {
    let ctx = load_match_ctx(st).await.ok()?;
    let aliases = merged_aliases(st).await;
    let mut gender: Option<String> = None;
    for r in rows {
        let resolved = resolve_row(r, &ctx, &aliases, None);
        let Some(pid) = resolved.person_id else {
            continue;
        };
        let g = ctx
            .streamers
            .iter()
            .find(|s| s.id == pid)
            .map(|s| s.gender.clone())?;
        if g != "male" && g != "female" {
            return None;
        }
        match &gender {
            None => gender = Some(g),
            Some(prev) if *prev == g => {}
            _ => return None, // 混合团别
        }
    }
    gender
}

/// POST /api/admin/import/preview：解析 + 匹配 + 团别冲突预检，不落库
pub async fn import_preview(State(st): State<ArcState>, Json(body): Json<ImportBody>) -> ApiResult {
    require_local(&st)?;
    let team = body.team.as_deref().unwrap_or("");
    if !team.is_empty() {
        valid_team(team)?;
    }
    let (raw_rows, dropped_minutes) = body_rows(&body.text, &body.rows).await?;
    ensure(!raw_rows.is_empty(), "没有解析到任何数据行")?;
    let ctx = load_match_ctx(&st).await?;
    let aliases = merged_aliases(&st).await;
    let resolved: Vec<ResolvedRow> = raw_rows
        .iter()
        .map(|r| {
            resolve_row(
                r,
                &ctx,
                &aliases,
                if team.is_empty() { None } else { Some(team) },
            )
        })
        .collect();
    let matched = resolved.iter().filter(|r| r.matched).count();
    Ok(Json(json!({
        "total": resolved.len(),
        "matched": matched,
        "unmatched": resolved.len() - matched,
        "bizDate": body.biz_date,
        "team": team,
        // 按日导入不写时长（用户口径 2026-10-10）：文件里有「时长 / 开播有效时长」列时，
        // 这里报出被忽略的行数，前端直接提示「要导时长请切到按月」。
        "droppedMinutes": dropped_minutes,
        "rows": resolved,
    })))
}

/// POST /api/admin/import/commit：事务写入 daily_metric + import_batch 审计
pub async fn import_commit(State(st): State<ArcState>, Json(body): Json<ImportBody>) -> ApiResult {
    require_local(&st)?;
    let biz_date = body
        .biz_date
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("缺少业务日期 bizDate"))?;
    // 不分男女团是默认：team 不传 / 传 auto 都按主播档案逐行归属
    let team = body.team.as_deref().unwrap_or("auto");
    let mode = body.mode.as_deref().unwrap_or("merge");
    let (rows, dropped_minutes) = body_rows(&body.text, &body.rows).await?;
    let report = commit_import(
        &st,
        team,
        biz_date,
        rows,
        mode,
        body.filename.as_deref().unwrap_or("粘贴导入"),
    )
    .await?;
    Ok(Json(json!({
        "ok": true,
        "batchId": report.batch_id,
        "total": report.total,
        "okRows": report.ok_rows,
        "unchanged": report.unchanged,
        "failedRows": report.failures.len(),
        "failures": report.failures,
        "pending": report.pending,
        "skipped": report.skipped,
        "missing": report.missing,
        "droppedMinutes": dropped_minutes.max(report.dropped_minutes as usize),
    })))
}

/// 导入提交结果（HTTP 与机器人共用）
pub struct CommitReport {
    pub batch_id: i64,
    pub total: usize,
    pub ok_rows: u32,
    /// 库里已有该人该日、且值完全一样 → 没动它（用户口径 2026-10-10：相同不覆盖，不同才覆盖）
    pub unchanged: u32,
    pub failures: Vec<Value>,
    /// 暂存行：有抖音号 / ID 但台账还没有这个主播 —— 只落 import_pending，
    /// 不写 daily_metric，所以不进台账 / 榜单 / 统计；建档或绑号后自动补账。
    pub pending: Vec<Value>,
    /// 跳过行：命中主播但当天一行数值都没有（不算失败，交给缺数据完整度报告）
    pub skipped: Vec<String>,
    /// 团内当天还没有数据的主播（完整度报告，docs：统一日期所有抖音号都要导入）
    pub missing: Vec<String>,
    /// 按日导入里被忽略的时长行数（用户口径 2026-10-10：每日不能导入时长，只有月时长）。
    /// HTTP 端在 body_rows 里就抹掉了 minutes（预览/提交都看得到），机器人直连这里时由本字段兜住。
    pub dropped_minutes: u32,
    /// 本次写入的时长合计（分钟）：月度时长导入才有值，按日导入恒为 0
    pub minutes_total: i64,
}

/// 一行导入数据在提交阶段的去向（纯函数，方便单测覆盖）
#[derive(Debug, PartialEq, Eq)]
enum RowClass {
    /// 命中主播且有数值 → 写 daily_metric
    Ok,
    /// 未命中但有抖音号 / ID → import_pending 暂存
    Pending,
    /// 命中主播但一行数值都没有 → 跳过
    Skipped,
    /// 没命中、也没有任何 ID → 失败
    Unmatched,
    /// 团别冲突 → 失败
    TeamConflict(String),
}

fn classify_row(r: &ResolvedRow) -> RowClass {
    if !r.matched {
        return match row_ident(&r.raw) {
            Some(_) => RowClass::Pending,
            None => RowClass::Unmatched,
        };
    }
    if let Some(reason) = &r.team_conflict {
        return RowClass::TeamConflict(reason.clone());
    }
    if r.raw.wave.is_none() && r.raw.cumulative_wave.is_none() && r.raw.minutes.is_none() {
        return RowClass::Skipped;
    }
    RowClass::Ok
}

/// 导入落库成功后异步触发一次异地备份（脚本有指纹/并发去重，重复触发无害；
/// 脚本不存在或执行失败只记日志，绝不影响导入本身）。按日导入与月度时长导入共用。
fn spawn_post_import_backup() {
    if let Ok(mut child) = tokio::process::Command::new("/opt/pk-server/backup-to-remote.sh")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        tokio::spawn(async move {
            match child.wait().await {
                Ok(s) if s.success() => tracing::info!("导入后自动备份完成"),
                Ok(s) => tracing::warn!("导入后自动备份退出码异常: {s}"),
                Err(e) => tracing::warn!("导入后自动备份执行失败: {e}"),
            }
        });
    }
}

/// 导入提交核心：匹配 + 校验 + 事务写入 + 批次审计 + 清缓存。
/// bot 命令与 HTTP 端共用，保证同一套口径与校验。
/// 异常文件防御（2026-10-13 QQ 机器人导入口径）：全部数据行音浪完全相同（≥3 行）
/// → 拒绝导入（像拿错文件/列错位/把某日快照当全员当日）。抽成纯函数方便单测。
pub(crate) fn ensure_not_all_same_wave(rows: &[ImportRow]) -> anyhow::Result<()> {
    let wave_values: Vec<i64> = rows
        .iter()
        .filter_map(|r| r.wave.map(|w| w.round() as i64))
        .collect();
    if wave_values.len() >= 3 {
        let first = wave_values[0];
        if wave_values.iter().all(|&w| w == first) {
            anyhow::bail!(
                "异常文件：全部 {} 行的音浪值都相同（{}）——像是一份与日期无关的全员同值文件，                 已拒绝导入。请核对文件里的日期列与音浪列后再试；确实要导入请用管理端手工分批。",
                wave_values.len(),
                first
            );
        }
    }
    Ok(())
}

pub async fn commit_import(
    st: &Arc<AppState>,
    team: &str,
    biz_date: &str,
    mut rows: Vec<ImportRow>,
    mode: &str,
    filename: &str,
) -> anyhow::Result<CommitReport> {
    // 时长不随按日导入（用户口径 2026-10-10）：机器人 / 粘贴文本 / 任何直连调用都在这里兜一道，
    // 只有 /import/month/* 的月度时长导入会写 monthly_duration。
    let dropped_minutes = strip_minutes(&mut rows) as u32;
    anyhow::ensure!(
        st.sources.iter().any(|s| s.id == "LOCAL"),
        "管理端需要 LOCAL 数据源（douyin_writer 对应的本地 PostgreSQL）"
    );
    let pool = st.writer.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "写库不可用：douyin_writer 账号未配置或连不上（检查 DB_WRITER_* 环境变量），只读功能不受影响"
        )
    })?;
    let date = valid_biz_date(biz_date)?;
    let auto_team = is_auto_team(team);
    if !auto_team {
        valid_team(team)?;
    }
    // 批次审计里的团别列：自动导入记 mixed（库列注释本来就支持 male/female/mixed）
    let team_col = if auto_team { "mixed" } else { team };
    anyhow::ensure!(
        mode == "merge" || mode == "reject",
        "mode 只能是 merge / reject"
    );
    anyhow::ensure!(!rows.is_empty(), "没有解析到任何数据行");

    /* 异常文件防御（2026-10-13 QQ 机器人导入口径）：
       文件里所有数据行的音浪值完全相同（≥3 行）→ 几乎可以断定是拿错了文件 /
       列错位（例如把固定值列当成了音浪，或把「某一日」的快照当成全员当日数据）。
       这种文件一旦导入，全团所有人的当日音浪会变成同一个数——拒绝并提示运营。 */
    ensure_not_all_same_wave(&rows)?;

    let ctx = load_match_ctx(st).await?;
    let aliases = merged_aliases(st).await;
    // 自动模式不做团别冲突校验：谁是哪个团由主播档案说了算
    let team_filter = if auto_team { None } else { Some(team) };
    let resolved: Vec<ResolvedRow> = rows
        .iter()
        .map(|r| resolve_row(r, &ctx, &aliases, team_filter))
        .collect();

    /* 一人多号合并音浪（2026-10-10）：运营按抖音号导出，一个主播挂 2~3 个号就会在
    文件里出现 2~3 行。同一个 person_id 的行必须合成一行后再入库，
    否则后一行会把前一行覆盖掉（daily_metric 按 person+date 唯一），
    主播看到的音浪只是他最后一个号的。
    合并口径：当日音浪求和；累计取最大（多个号的累计快照不是同一个池子，
    求和会虚高，取最大是保守且不重复计数的做法）；时长求和；有任一号开播 = 开播。 */
    let merged = merge_rows_by_person(&resolved);

    /* 行分类（2026-10-10 用户口径：只要有抖音号或者 id 都导入，但不进榜单和统计，
    主播在台账建档 / 绑号之后才显示）：
       - 命中主播且行里有数值 → 正常入库
       - 未命中但有抖音号 / 采集 ID → 暂存（import_pending）：不写 daily_metric，
         所以台账、榜单、统计、对外展示、缺数据报告全都看不见它
       - 未命中且没有任何 ID → 失败（连人都认不出来，暂存也没意义）
       - 命中主播但一行数值都没有 → 跳过：当天没播，交给「缺数据」完整度报告，
         不再算失败刷屏 */
    let mut failures: Vec<Value> = Vec::new();
    let mut ok_rows: Vec<&ResolvedRow> = Vec::new();
    let mut pending_rows: Vec<&ResolvedRow> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for r in &merged {
        match classify_row(r) {
            RowClass::Ok => ok_rows.push(r),
            RowClass::Pending => pending_rows.push(r),
            RowClass::Skipped => skipped.push(truncate_chars(&r.raw.name, 64)),
            RowClass::Unmatched => failures.push(json!({
                "name": r.raw.name,
                "reason": "未匹配到主播，也没有抖音号/ID 可以暂存；请先在主播台账建档或绑定"
            })),
            RowClass::TeamConflict(reason) => {
                failures.push(json!({ "name": r.raw.name, "reason": reason }))
            }
        }
    }

    let mut tx = pool.begin().await?;

    if mode == "reject" {
        let cnt: i64 = sqlx::query(&crate::pq::rewrite(if auto_team {
            dialect::DATE_COUNT_SQL
        } else {
            dialect::TEAM_DATE_COUNT_SQL
        }))
        .bind(biz_date)
        .bind(team)
        .map(|r: sqlx::postgres::PgRow| r.try_get::<i64, _>(0).unwrap_or(0))
        .fetch_one(&mut *tx)
        .await?;
        if cnt > 0 {
            anyhow::bail!(
                "reject 模式：{} {biz_date} 已有 {cnt} 行数据，请改用 merge（幂等覆盖）或先处理旧数据",
                if auto_team { "该日" } else { team }
            );
        }
    }

    let staged_rows: Vec<&ResolvedRow> = pending_rows.clone();
    let batch = sqlx::query(&crate::pq::rewrite(
        "INSERT INTO import_batch (source, filename, biz_date, team, imported_rows, ok_rows, failed_rows, note)
         VALUES ('admin-import', ?, ?::date, ?, ?, 0, 0, ?) RETURNING id",
    ))
    // filename 也是 VARCHAR(255)：超长同样 1406（机器人转发长文件名踩过），先截断
    .bind(truncate_chars(filename, FILENAME_MAX_CHARS))
    .bind(biz_date)
    .bind(team_col)
    .bind(merged.len() as i64)
    .bind(batch_note(&failures, staged_rows.len(), &skipped))
    .fetch_one(&mut *tx)
    .await?;
    let batch_id = batch.try_get::<i64, _>(0)?;

    let mut ok_count: u32 = 0;
    let mut unchanged: u32 = 0;
    for r in &ok_rows {
        match write_daily_row(&mut tx, batch_id, biz_date, date, r).await {
            Ok(WriteOutcome::Written) => ok_count += 1,
            Ok(WriteOutcome::Unchanged) => unchanged += 1,
            Err(e) => {
                tracing::error!(name = %r.raw.name, error = %e, "写入 daily_metric 失败");
                failures.push(json!({ "name": r.raw.name, "reason": e.to_string() }));
            }
        }
    }

    /* 暂存：未建档但有抖音号 / ID 的行 → import_pending。
    同一「号码 + 日期」重导按最新值覆盖（幂等，跟 daily_metric 的 merge 口径一致），
    运营删号重发同一个文件不会翻倍。 */
    let mut pending_count: u32 = 0;
    for r in &staged_rows {
        let Some(no) = row_ident(&r.raw) else {
            continue;
        };
        let res = sqlx::query(&crate::pq::rewrite(
            "INSERT INTO import_pending
                 (name, douyin_no, biz_date, wave, cumulative_wave, minutes, is_live, source_batch_id)
             VALUES (?,?,?::date,?,?,?,?,?)
             ON CONFLICT (douyin_no, biz_date) DO UPDATE SET
                 name = EXCLUDED.name, wave = EXCLUDED.wave,
                 cumulative_wave = EXCLUDED.cumulative_wave, minutes = EXCLUDED.minutes,
                 is_live = EXCLUDED.is_live, source_batch_id = EXCLUDED.source_batch_id",
        ))
        .bind(truncate_chars(&r.raw.name, PENDING_NAME_MAX_CHARS))
        .bind(&no)
        .bind(biz_date)
        .bind(r.raw.wave.map(|v| v as i64))
        .bind(r.raw.cumulative_wave.map(|v| v as i64))
        .bind(r.raw.minutes.map(|v| v as i64))
        .bind(r.raw.is_live.map(|b| b as i16))
        .bind(batch_id as i64)
        .execute(&mut *tx)
        .await;
        // 暂存区写不进去（例如线上还没跑 006 迁移 → 表不存在）不能拖垮整批导入：
        // 记日志、这行不暂存，其余行照常入库。
        match res {
            Ok(_) => pending_count += 1,
            Err(e) => {
                tracing::warn!(error = %e, name = %r.raw.name, "暂存写入失败（该行不进暂存区）");
            }
        }
    }

    // 备注在写库后再刷一次：写入阶段的行级错误也要进摘要（同样先截断，别炸事务）
    let note = batch_note(&failures, pending_count as usize, &skipped);
    sqlx::query(&crate::pq::rewrite(
        "UPDATE import_batch SET ok_rows = ?, failed_rows = ?, note = ? WHERE id = ?",
    ))
    .bind(ok_count as i64)
    .bind(failures.len() as i64)
    .bind(&note)
    .bind(batch_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    st.clear_cache();
    // 数据变更后自动备份（2026-10-10）：导入落库成功就异步触发一次异地备份。
    // 脚本内部有指纹去重（数据没变不推）和 flock 并发去重，重复触发无害；
    // 脚本不存在/执行失败只记日志，绝不影响导入本身。
    spawn_post_import_backup();
    // 通知所有后台页面自动刷新（机器人导入也走这里，所以手机 QQ 一导入电脑就会跳）
    crate::events::emit(
        crate::events::SCOPE_IMPORT,
        format!(
            "{} {} 导入 {} 行{}{}{}",
            if auto_team {
                "不分男女团"
            } else if team == "female" {
                "女团"
            } else {
                "男团"
            },
            biz_date,
            ok_count,
            if unchanged > 0 {
                format!("（{unchanged} 行值相同未改动）")
            } else {
                String::new()
            },
            if pending_count > 0 {
                format!("（{pending_count} 行未建档已暂存，建档/绑号后自动补账）")
            } else {
                String::new()
            },
            if filename.is_empty() {
                String::new()
            } else {
                format!("（{}）", truncate_chars(filename, 80))
            }
        ),
    );

    // 完整度报告：该团当天还没有数据的主播（同日多文件导入时逐步收敛，
    // 全部到齐则 missing 为空）
    let missing = if auto_team {
        // 不分团：该日还没数据的在册主播（男女一起算）
        sqlx::query(&crate::pq::rewrite(dialect::MISSING_ALL_SQL))
            .bind(biz_date)
            .map(|r: sqlx::postgres::PgRow| r.try_get::<String, _>(0).unwrap_or_default())
            .fetch_all(pool)
            .await
            .unwrap_or_default()
    } else {
        sqlx::query(&crate::pq::rewrite(dialect::MISSING_SQL))
            .bind(biz_date)
            .bind(team)
            .map(|r: sqlx::postgres::PgRow| r.try_get::<String, _>(0).unwrap_or_default())
            .fetch_all(pool)
            .await
            .unwrap_or_default()
    };

    // 「我的」页微信推送（2026-10-10 改延迟触发）：数据入库后只排队，
    // 到点（默认 30 分钟后，PUSH_DELAY_MINUTES）由 push::scheduler_loop 统一推。
    // 延迟是为了等一轮导入的几个文件（男女团 / 多抖音号）都到齐，别把半成品榜单推出去。
    // 不分团导入（auto）：男女团各排一队，两边订阅者都能收到。
    if auto_team {
        crate::push::schedule_after_import(st, "male", biz_date);
        crate::push::schedule_after_import(st, "female", biz_date);
    } else {
        crate::push::schedule_after_import(st, team, biz_date);
    }

    Ok(CommitReport {
        batch_id,
        // total 报合并后的行数：运营看到的行数跟落库的行数才对得上
        total: merged.len(),
        ok_rows: ok_count,
        unchanged,
        failures,
        // 暂存行：有抖音号/ID 但台账还没有这个主播，不进 daily_metric / 榜单 / 统计
        pending: staged_rows
            .iter()
            .map(|r| {
                json!({
                    "name": truncate_chars(&r.raw.name, 64),
                    "douyinNo": row_ident(&r.raw).unwrap_or_default(),
                })
            })
            .collect(),
        // 跳过行：命中主播但当天一行数值都没有（不算失败，交给缺数据报告）
        skipped,
        missing,
        dropped_minutes,
        // 按日导入不写时长（时长只按月导入）
        minutes_total: 0,
    })
}

/// 按 person_id 合并同一人的多行（一人多个抖音号时运营文件里会有多行）。
///
/// 合并口径：
/// - 当日音浪 `wave`：**求和**（每个号当天各自播的，加起来才是这个人当天的量）
/// - 累计 `cumulative_wave`：**取最大**（各号的累计快照不是同一个池子，求和会虚高；
///   取最大既不重复计数也不会漏算，是保守且唯一合理的做法）
/// - 时长 `minutes`：求和；开播 `is_live`：任一为 true 即 true
/// - 名字取第一个出现的；`matchedByDouyin` 任一行按抖音号命中即为 true
///
/// 只合并「都匹配上了且没有团别冲突」的行：匹配失败的行要保持原样一条一条报出来，
/// 否则运营在预览里找不到到底哪一行没匹配上。
fn merge_rows_by_person(rows: &[ResolvedRow]) -> Vec<ResolvedRow> {
    let mut out: Vec<ResolvedRow> = Vec::with_capacity(rows.len());
    // person_id → 在 out 里的下标
    let mut index: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for r in rows {
        let mergeable = r.matched && r.team_conflict.is_none();
        let Some(pid) = r.person_id.filter(|_| mergeable) else {
            out.push(r.clone());
            continue;
        };
        match index.get(&pid).copied() {
            Some(i) => {
                let base = &mut out[i];
                base.matched_by_douyin |= r.matched_by_douyin;
                base.raw.wave = match (base.raw.wave, r.raw.wave) {
                    (Some(a), Some(b)) => Some(a + b),
                    (a, b) => a.or(b),
                };
                base.raw.cumulative_wave = match (base.raw.cumulative_wave, r.raw.cumulative_wave) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
                base.raw.minutes = match (base.raw.minutes, r.raw.minutes) {
                    (Some(a), Some(b)) => Some(a + b),
                    (a, b) => a.or(b),
                };
                base.raw.is_live = match (base.raw.is_live, r.raw.is_live) {
                    (Some(a), Some(b)) => Some(a || b),
                    (a, b) => a.or(b),
                };
            }
            None => {
                index.insert(pid, out.len());
                out.push(r.clone());
            }
        }
    }
    out
}

/// 失败摘要（写进 import_batch.note 的审计文本）。
/// **必须限长**：note 是 VARCHAR(512)，STRICT_TRANS_TABLES 下超长直接 1406 炸掉
/// 整个导入事务（2026-10-10 覆盖导入踩过：一次几十行未匹配就把事务打回）。
fn failures_summary(failures: &[Value]) -> Option<String> {
    if failures.is_empty() {
        return None;
    }
    let names: Vec<String> = failures
        .iter()
        .filter_map(|f| f.get("name").and_then(|v| v.as_str()).map(String::from))
        .take(NOTE_MAX_NAMES)
        .collect();
    let mut out = format!("失败 {} 行: {}", failures.len(), names.join("、"));
    if failures.len() > names.len() {
        out.push_str(&format!(" 等 {} 行", failures.len()));
    }
    Some(truncate_chars(&out, NOTE_MAX_CHARS))
}

/// 批次备注：失败 + 暂存 + 跳过，三者都限长（见 failures_summary 的说明）
fn batch_note(failures: &[Value], pending: usize, skipped: &[String]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(f) = failures_summary(failures) {
        parts.push(f);
    }
    if pending > 0 {
        parts.push(format!("暂存 {pending} 行（未建档，建档后自动补账）"));
    }
    if !skipped.is_empty() {
        let names = skipped
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("、");
        parts.push(format!("跳过 {} 行（当天无数值）: {names}", skipped.len()));
    }
    if parts.is_empty() {
        return None;
    }
    Some(truncate_chars(&parts.join("；"), NOTE_MAX_CHARS))
}

/// 单行写入 daily_metric。口径（docs/00 §4）：
/// - 给了 cumulativeWave → 与上一业务日快照差分；无上一日 = 基线日语义，wave=累计值且 reliable=0
/// - 只给 wave → 直接采用（trusted）；累计值回填 prev + wave（拿不到 prev 就 0）
/// - 差分跨天 > 1（漏采）或为负（回退）→ wave_reliable=0
///
/// 单行写入结果：真写/覆盖了，还是库里已经有一样的值（没动）
enum WriteOutcome {
    Written,
    Unchanged,
}

/// 已有行的当前值（比对用）
/// 确保该主播有一个主号（P2-11 收敛，2026-10-10）：
/// 有 account 行但全是 is_primary=0 时（生产实测 person 1000001 哈基勺 / 1000004 狼森），
/// PRIMARY_ANCHOR_SQL 取不到 → 导入回填 anchor_id=''。导入回填 anchor_id 前调用：
/// 没有主号就把最早（id 最小）的号置为主号，再按 PRIMARY_ANCHOR_SQL 取。
/// 幂等：已有主号（或根本没有账号）时一条 UPDATE 都不发，直接返回。
/// 失败只记日志不阻断导入：宁缺 anchor_id，不能让当天数据写不进去。
async fn ensure_primary(conn: &mut sqlx::PgConnection, person_id: u64) {
    let has_primary: i64 = sqlx::query(&crate::pq::rewrite(
        "SELECT COUNT(*) FROM account WHERE person_id = ? AND is_primary = 1",
    ))
    .bind(person_id as i64)
    .fetch_one(&mut *conn)
    .await
    .map(|r| r.try_get::<i64, _>(0).unwrap_or(0))
    .unwrap_or(1); // 查询失败按「有主号」处理：不做任何写入
    if has_primary > 0 {
        return;
    }
    let has_any: i64 = sqlx::query(&crate::pq::rewrite(
        "SELECT COUNT(*) FROM account WHERE person_id = ?",
    ))
    .bind(person_id as i64)
    .fetch_one(&mut *conn)
    .await
    .map(|r| r.try_get::<i64, _>(0).unwrap_or(0))
    .unwrap_or(0);
    if has_any == 0 {
        return; // 连账号都没有：没有可提的号，anchor_id 保持空
    }
    match sqlx::query(&crate::pq::rewrite(
        "UPDATE account SET is_primary = 1 WHERE person_id = ? AND id = \
           (SELECT min_id FROM (SELECT MIN(id) AS min_id FROM account WHERE person_id = ?) t)",
    ))
    .bind(person_id as i64)
    .bind(person_id as i64)
    .execute(&mut *conn)
    .await
    {
        Ok(res) if res.affected() > 0 => {
            tracing::warn!(
                person_id,
                "该主播有抖音号但没有主号（is_primary 全 0），已把最早的一个号收敛为主号"
            );
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, person_id, "收敛主号失败（导入继续，anchor_id 可能为空）")
        }
    }
}

async fn existing_day_row(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    person_id: u64,
    biz_date: &str,
) -> anyhow::Result<Option<(i64, i64, i64, i16)>> {
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT wave, cumulative_wave, minutes, is_live FROM daily_metric
         WHERE person_id = ? AND biz_date = ?::date",
    ))
    .bind(person_id as i64)
    .bind(biz_date)
    .map(|row: sqlx::postgres::PgRow| {
        let num = |col: &str| -> i64 {
            row.try_get::<Option<i64>, _>(col)
                .ok()
                .flatten()
                .or_else(|| row.try_get::<f64, _>(col).ok().map(|v| v as i64))
                .unwrap_or(0)
        };
        (
            num("wave"),
            num("cumulative_wave"),
            num("minutes"),
            row.try_get::<i16, _>("is_live").unwrap_or(0),
        )
    })
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row)
}

async fn write_daily_row(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    batch_id: i64,
    biz_date: &str,
    date: NaiveDate,
    r: &ResolvedRow,
) -> anyhow::Result<WriteOutcome> {
    let person_id = r.person_id.expect("调用前已保证 matched");
    let prev: Option<(f64, String)> =
        sqlx::query(&crate::pq::rewrite(dialect::PREV_CUMULATIVE_SQL))
            .bind(person_id as i64)
            .bind(biz_date)
            .map(|row: sqlx::postgres::PgRow| {
                let cum = row
                    .try_get::<Option<f64>, _>(0)
                    .ok()
                    .flatten()
                    .or_else(|| row.try_get::<i64, _>(0).ok().map(|v| v as f64))
                    .unwrap_or(0.0);
                let d = row
                    .try_get::<Option<String>, _>(1)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                (cum, d)
            })
            .fetch_optional(&mut **tx)
            .await?;

    // P2-11 收敛：有账号但无主号（生产 1000001/1000004）时 PRIMARY_ANCHOR_SQL 取不到，
    // 先把最早的号提为主号再取（幂等，正常情况零开销）。
    ensure_primary(tx, person_id).await;

    let anchor: String = sqlx::query(&crate::pq::rewrite(dialect::PRIMARY_ANCHOR_SQL))
        .bind(person_id as i64)
        .map(|row: sqlx::postgres::PgRow| row.try_get::<String, _>(0).unwrap_or_default())
        .fetch_optional(&mut **tx)
        .await?
        .unwrap_or_default();

    let (wave, cumulative, prev_date, span, reliable): (i64, i64, Option<String>, i16, i16) =
        match (r.raw.wave, r.raw.cumulative_wave) {
            // 两个都给了：以「当日音浪」为准（运营文件里这一列就是当天值），累计只当快照。
            // 否则清库/新部署后的第一天没有前一天快照，会被算成「累计值」当成当日音浪（踩过）。
            (Some(w), Some(cum)) => (
                w as i64,
                cum as i64,
                prev.as_ref().map(|(_, d)| d.clone()),
                1,
                1,
            ),
            (Some(w), None) => (
                w as i64,
                prev.as_ref().map(|(p, _)| p + w).unwrap_or(0.0) as i64,
                prev.as_ref().map(|(_, d)| d.clone()),
                1,
                1,
            ),
            (None, Some(cum)) => match &prev {
                Some((pcum, pd)) => {
                    let diff = (cum - pcum) as i64;
                    let days = NaiveDate::parse_from_str(pd, "%Y-%m-%d")
                        .map(|p| (date - p).num_days())
                        .unwrap_or(1);
                    let span = days.clamp(1, 365) as i16;
                    let reliable = if diff >= 0 && days <= 1 { 1 } else { 0 };
                    (diff, cum as i64, Some(pd.clone()), span, reliable)
                }
                None => (cum as i64, cum as i64, None, 1, 0), // 基线日：只记累计
            },
            // 没有音浪（时长 CSV / 只有开播状态的行）：不写音浪列，走下面的 minutes-only 分支。
            // 以前这里直接 bail「没有音浪数据」，导致时长行永远进不来（2026-10-10 修）。
            (None, None) => (0, 0, None, 1, 1),
        };

    let minutes = r.raw.minutes.unwrap_or(0.0) as i64;
    let is_live = r.raw.is_live.map(|b| b as i16).unwrap_or(0);

    // 时长-only 行（时长 CSV）：只更新 minutes/is_live，不把已导入的音浪覆盖成 0
    if r.raw.wave.is_none() && r.raw.cumulative_wave.is_none() {
        // 连分钟和开播都没给（暂存补账时可能出现的「只有名字」行）：库里已有当日行就什么都不动
        let presence_only = r.raw.minutes.is_none() && r.raw.is_live.is_none();
        if let Some((_, _, m, live)) = existing_day_row(tx, person_id, biz_date).await? {
            if presence_only || (m == minutes && live == is_live) {
                return Ok(WriteOutcome::Unchanged);
            }
        }
        sqlx::query(&crate::pq::rewrite(
            "INSERT INTO daily_metric
                 (person_id, anchor_id, biz_date, minutes, is_live, source_batch_id)
             VALUES (?,?,?::date,?,?,?)
             ON CONFLICT (person_id, biz_date) DO UPDATE SET
                 minutes = EXCLUDED.minutes, is_live = EXCLUDED.is_live,
                 source_batch_id = EXCLUDED.source_batch_id",
        ))
        .bind(person_id as i64)
        .bind(&anchor)
        .bind(biz_date)
        .bind(minutes)
        .bind(is_live)
        .bind(batch_id as i64)
        .execute(&mut **tx)
        .await?;
        return Ok(WriteOutcome::Written);
    }

    // 库里已有该人该日、且算出来的值与库里完全一致 → 不动它（幂等：同一天重复发同一个文件不产生任何写入）
    if let Some((w, c, m, live)) = existing_day_row(tx, person_id, biz_date).await? {
        if w == wave && c == cumulative && m == minutes && live == is_live {
            return Ok(WriteOutcome::Unchanged);
        }
    }

    sqlx::query(&crate::pq::rewrite(
        "INSERT INTO daily_metric
             (person_id, anchor_id, biz_date, wave, cumulative_wave, prev_snapshot_date,
              wave_span, wave_reliable, minutes, is_live, source_batch_id)
         VALUES (?,?,?::date,?,?,?::date,?,?,?,?,?)
         ON CONFLICT (person_id, biz_date) DO UPDATE SET
             wave = EXCLUDED.wave, cumulative_wave = EXCLUDED.cumulative_wave,
             prev_snapshot_date = EXCLUDED.prev_snapshot_date, wave_span = EXCLUDED.wave_span,
             wave_reliable = EXCLUDED.wave_reliable, minutes = EXCLUDED.minutes,
             is_live = EXCLUDED.is_live, source_batch_id = EXCLUDED.source_batch_id",
    ))
    .bind(person_id as i64)
    .bind(&anchor)
    .bind(biz_date)
    .bind(wave)
    .bind(cumulative)
    .bind(prev_date)
    .bind(span)
    .bind(reliable)
    .bind(minutes)
    .bind(is_live)
    .bind(batch_id as i64)
    .execute(&mut **tx)
    .await?;
    Ok(WriteOutcome::Written)
}

/* ------------------------- 暂存区：导入暂存 → 建档补账 ------------------------- */
/*
   用户口径（2026-10-10）：「只要有抖音号或者 id 都导入，但是不能显示在表里和统计里，
   只有当主播页面添加了主播才显示。」

   导入时未匹配到台账、但行里有抖音号 / 采集 ID 的行落在 import_pending：
     - 不在 person / account / daily_metric → 台账、榜单、统计、对外展示、缺数据报告都看不见
   运营在「主播台账」建档或绑定该抖音号之后，下面的 absorb 才把它们补进 daily_metric，
   数据从这一刻起才出现在榜单和统计里。
*/

/// 一条暂存行（补账用）
struct PendingRow {
    id: u64,
    name: String,
    ident: String,
    biz_date: String,
    wave: Option<i64>,
    cumulative_wave: Option<i64>,
    minutes: Option<i64>,
    is_live: Option<i16>,
    batch_id: u64,
}

/// 把暂存区里「已经能对上号」的行补账进 daily_metric（建档 / 绑号 / 绑定审核通过后调用）。
///
/// 命中条件：暂存行的号码出现在该主播的 account.douyin_no 或 account.anchor_id 里。
/// 已有当日数据时按一人多号口径叠加（音浪求和、累计取最大、时长求和、开播取或），
/// 不覆盖该主播其它账号已经导入的量；补账和标记 absorbed 在同一个事务里，避免重复补。
/// 返回补账行数。写库不可用时返回 0（不阻塞建档本身）。
pub async fn absorb_pending(st: &AppState, person_id: u64) -> anyhow::Result<u32> {
    let Some(pool) = st.writer.as_ref() else {
        return Ok(0);
    };
    let rows = sqlx::query(&crate::pq::rewrite(dialect::PENDING_FOR_PERSON_SQL))
        .bind(person_id as i64)
        .bind(person_id as i64)
        .map(|r: sqlx::postgres::PgRow| PendingRow {
            id: read_u64(&r, "id").unwrap_or(0),
            name: read_str(&r, "name"),
            ident: read_str(&r, "douyin_no"),
            biz_date: read_str(&r, "bd"),
            wave: read_opt_i64(&r, "wave"),
            cumulative_wave: read_opt_i64(&r, "cumulative_wave"),
            minutes: read_opt_i64(&r, "minutes"),
            is_live: r.try_get::<Option<i16>, _>("is_live").ok().flatten(),
            batch_id: read_opt_u64(&r, "source_batch_id").unwrap_or(0),
        })
        .fetch_all(pool)
        .await?;
    if rows.is_empty() {
        return Ok(0);
    }

    let mut tx = pool.begin().await?;
    let mut absorbed: u32 = 0;
    for p in &rows {
        let Some(date) = NaiveDate::parse_from_str(&p.biz_date, "%Y-%m-%d").ok() else {
            continue; // 日期列坏了：留着人工处理，别把事务搞挂
        };
        // 一行数值都没有（只有名字 + 号）：没有可写的指标，直接标记已认领即可
        let has_values = p.wave.is_some()
            || p.cumulative_wave.is_some()
            || p.minutes.is_some()
            || p.is_live.is_some();
        if has_values {
            match existing_day_row(&mut tx, person_id, &p.biz_date).await? {
                // 当日已有数据（可能是这个主播的另一个号）：叠加，不覆盖
                Some((w, c, m, live)) => {
                    let new_wave = w + p.wave.unwrap_or(0);
                    let new_cum = c.max(p.cumulative_wave.unwrap_or(0));
                    let new_min = m + p.minutes.unwrap_or(0);
                    let new_live = live | p.is_live.unwrap_or(0);
                    sqlx::query(&crate::pq::rewrite(
                        "UPDATE daily_metric SET wave = ?, cumulative_wave = ?, minutes = ?, is_live = ?
                         WHERE person_id = ? AND biz_date = ?::date",
                    ))
                    .bind(new_wave)
                    .bind(new_cum)
                    .bind(new_min)
                    .bind(new_live)
                    .bind(person_id as i64)
                    .bind(&p.biz_date)
                    .execute(&mut *tx)
                    .await?;
                }
                // 当日还没有数据：按正常导入口径写（累计差分 / 基线日语义都在 write_daily_row 里）
                None => {
                    let resolved = ResolvedRow {
                        raw: ImportRow {
                            name: p.name.clone(),
                            douyin_no: Some(p.ident.clone()),
                            anchor_id: None,
                            wave: p.wave.map(|v| v as f64),
                            cumulative_wave: p.cumulative_wave.map(|v| v as f64),
                            minutes: p.minutes.map(|v| v as f64),
                            is_live: p.is_live.map(|v| v != 0),
                            err: None,
                        },
                        person_id: Some(person_id),
                        name: p.name.clone(),
                        matched: true,
                        team_conflict: None,
                        candidates: Vec::new(),
                        matched_by_douyin: true,
                    };
                    write_daily_row(&mut tx, p.batch_id as i64, &p.biz_date, date, &resolved)
                        .await?;
                }
            }
        }
        sqlx::query(&crate::pq::rewrite(
            "UPDATE import_pending SET absorbed_person_id = ?, absorbed_at = now() WHERE id = ?",
        ))
        .bind(person_id as i64)
        .bind(p.id as i64)
        .execute(&mut *tx)
        .await?;
        absorbed += 1;
    }
    tx.commit().await?;

    if absorbed > 0 {
        st.clear_cache();
        crate::events::emit(
            crate::events::SCOPE_IMPORT,
            format!(
                "暂存补账：{} 行并入「{}」",
                absorbed,
                person_name_of_id(st, person_id).await
            ),
        );
    }
    Ok(absorbed)
}

/// 建档 / 绑号后补账的「不阻塞」包装：失败只记日志，不影响建档结果
async fn absorb_pending_best_effort(st: &AppState, person_id: u64, ctx: &str) -> u32 {
    match absorb_pending(st, person_id).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(person_id, ctx, error = %e, "暂存补账失败（不影响建档/绑号）");
            0
        }
    }
}

#[derive(Deserialize)]
pub struct PendingQuery {
    /// 丢弃单行
    pub id: Option<u64>,
    /// 丢弃某个号码（或采集 ID）的全部暂存行
    #[serde(rename = "douyinNo")]
    pub douyin_no: Option<String>,
}

/// GET /api/admin/import/pending：暂存区列表（未认领的导入行）
pub async fn import_pending_list(State(st): State<ArcState>) -> ApiResult {
    require_local(&st)?;
    let rows = pgrs::query(&st.pools, "LOCAL", dialect::PENDING_LIST_SQL).await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": read_u64(r, "id").unwrap_or(0),
                "name": read_str(r, "name"),
                "douyinNo": read_str(r, "douyin_no"),
                "bizDate": read_opt_str(r, "bd"),
                "wave": read_opt_i64(r, "wave"),
                "cumulativeWave": read_opt_i64(r, "cumulative_wave"),
                "minutes": read_opt_i64(r, "minutes"),
                "isLive": r.try_get::<Option<i16>, _>("is_live").ok().flatten().map(|v| v != 0),
                "batchId": read_opt_u64(r, "source_batch_id"),
                "createdAt": read_opt_str(r, "ts"),
            })
        })
        .collect();
    Ok(Json(json!({ "items": items, "total": items.len() })))
}

/// DELETE /api/admin/import/pending?id= 或 ?douyinNo=：丢弃暂存行（不入库）
pub async fn import_pending_delete(
    State(st): State<ArcState>,
    Query(q): Query<PendingQuery>,
) -> ApiResult {
    require_local(&st)?;
    let pool = writer_pool(&st)?;
    let affected = match (q.id, q.douyin_no.as_deref().map(str::trim)) {
        (Some(id), _) if id > 0 => sqlx::query(&crate::pq::rewrite(
            "DELETE FROM import_pending WHERE id = ? AND absorbed_at IS NULL",
        ))
        .bind(id as i64)
        .execute(pool)
        .await?
        .affected(),
        (_, Some(no)) if !no.is_empty() => sqlx::query(&crate::pq::rewrite(
            "DELETE FROM import_pending WHERE douyin_no = ? AND absorbed_at IS NULL",
        ))
        .bind(no)
        .execute(pool)
        .await?
        .affected(),
        _ => return Err(anyhow::anyhow!("要给 id 或 douyinNo 才能丢弃暂存行").into()),
    };
    Ok(Json(json!({ "ok": true, "deleted": affected })))
}

/// GET /api/admin/import/day-status?bizDate=YYYY-MM-DD&team=male
/// 前端选日期 / 切团别时实时查询：该日该团已入库行数 + 最近一次导入批次，
/// 用来提示「这次导入是覆盖还是新增」（merge = 覆盖，reject = 有数据就拒绝）。
#[derive(Deserialize)]
pub struct DayStatusQuery {
    #[serde(rename = "bizDate")]
    pub biz_date: Option<String>,
    pub team: Option<String>,
}

pub async fn import_day_status(
    State(st): State<ArcState>,
    Query(q): Query<DayStatusQuery>,
) -> ApiResult {
    require_local(&st)?;
    let biz_date = q.biz_date.as_deref().map(str::trim).unwrap_or_default();
    let team = q.team.as_deref().map(str::trim).unwrap_or_default();
    if biz_date.is_empty() {
        return Ok(Json(json!({ "checked": false })));
    }
    let auto_team = is_auto_team(team);
    if !auto_team {
        valid_team(team)?;
    }
    valid_biz_date(biz_date)?;
    let pool = writer_pool(&st)?;
    let rows: i64 = if auto_team {
        sqlx::query(&crate::pq::rewrite(dialect::DATE_COUNT_SQL))
            .bind(biz_date)
            .map(|r: sqlx::postgres::PgRow| r.try_get::<i64, _>(0).unwrap_or(0))
            .fetch_one(pool)
            .await?
    } else {
        sqlx::query(&crate::pq::rewrite(dialect::TEAM_DATE_COUNT_SQL))
            .bind(biz_date)
            .bind(team)
            .map(|r: sqlx::postgres::PgRow| r.try_get::<i64, _>(0).unwrap_or(0))
            .fetch_one(pool)
            .await?
    };
    let last_sql = if auto_team {
        "SELECT filename, TO_CHAR(created_at, 'YYYY-MM-DD HH24:MI') AS ts
         FROM import_batch WHERE biz_date = ?::date ORDER BY id DESC LIMIT 1"
    } else {
        "SELECT filename, TO_CHAR(created_at, 'YYYY-MM-DD HH24:MI') AS ts
         FROM import_batch WHERE biz_date = ?::date AND team = ? ORDER BY id DESC LIMIT 1"
    };
    let last_sql = crate::pq::rewrite(last_sql);
    let mut last_q = sqlx::query(&last_sql).bind(biz_date);
    if !auto_team {
        last_q = last_q.bind(team);
    }
    let last = last_q
        .map(|r: sqlx::postgres::PgRow| {
            json!({
                "filename": r.try_get::<String, _>("filename").unwrap_or_default(),
                "at": r.try_get::<String, _>("ts").unwrap_or_default(),
            })
        })
        .fetch_optional(pool)
        .await?;
    Ok(Json(json!({
        "checked": true,
        "bizDate": biz_date,
        "team": if auto_team { "auto" } else { team },
        "rows": rows,
        "lastBatch": last,
    })))
}

/// GET /api/admin/import/batches：最近导入批次
pub async fn import_batches(State(st): State<ArcState>) -> ApiResult {
    require_local(&st)?;
    let rows = pgrs::query(&st.pools, "LOCAL", dialect::BATCH_LIST_SQL).await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": read_u64(r, "id").unwrap_or(0),
                "source": read_str(r, "source"),
                "filename": read_str(r, "filename"),
                "bizDate": read_opt_str(r, "bd"),
                "team": read_opt_str(r, "team"),
                "importedRows": read_opt_i64(r, "imported_rows").unwrap_or(0),
                "okRows": read_opt_i64(r, "ok_rows").unwrap_or(0),
                "failedRows": read_opt_i64(r, "failed_rows").unwrap_or(0),
                "note": read_opt_str(r, "note"),
                "createdAt": read_opt_str(r, "ts"),
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub struct DayQuery {
    #[serde(rename = "bizDate")]
    pub biz_date: String,
    pub team: String,
}

/// DELETE /api/admin/import/day?bizDate=&team=：删除某天某团已入库的音浪数据
/// 只删 daily_metric 对应日/团的数据，并同步清理导入批次记录，便于重新导入。
pub async fn import_day_delete(
    State(st): State<ArcState>,
    axum::extract::Query(q): axum::extract::Query<DayQuery>,
) -> ApiResult {
    require_local(&st)?;
    let team = q.team.trim();
    let auto_team = is_auto_team(team);
    if !auto_team {
        valid_team(team)?;
    }
    valid_biz_date(&q.biz_date)?;
    let pool = writer_pool(&st)?;
    let mut tx = pool.begin().await?;
    let metrics = if auto_team {
        sqlx::query(&crate::pq::rewrite(
            "DELETE FROM daily_metric WHERE TO_CHAR(biz_date, 'YYYY-MM-DD') = ?",
        ))
        .bind(&q.biz_date)
        .execute(&mut *tx)
        .await?
        .affected()
    } else {
        sqlx::query(&crate::pq::rewrite(
            // PG 方言：DELETE … USING（MySQL 的 DELETE d FROM … JOIN 在 PG 语法不通）
            "DELETE FROM daily_metric d \
             USING person p \
             WHERE p.id = d.person_id \
               AND TO_CHAR(d.biz_date, 'YYYY-MM-DD') = ? AND p.gender = ?",
        ))
        .bind(&q.biz_date)
        .bind(team)
        .execute(&mut *tx)
        .await?
        .affected()
    };
    // 只删按日导入的批次：月度时长批次也用当月 1 号当 biz_date 占位，
    // 少了 source 过滤，删「某一天」会连坐删掉整月的月报审计记录（审计栈断裂）。
    let batches = if auto_team {
        sqlx::query(&crate::pq::rewrite(
            "DELETE FROM import_batch WHERE source <> 'admin-import-month' AND biz_date = ?::date",
        ))
        .bind(&q.biz_date)
        .execute(&mut *tx)
        .await?
        .affected()
    } else {
        sqlx::query(&crate::pq::rewrite(
            "DELETE FROM import_batch WHERE source <> 'admin-import-month' AND biz_date = ?::date AND team = ?",
        ))
        .bind(&q.biz_date)
        .bind(team)
        .execute(&mut *tx)
        .await?
        .affected()
    };
    tx.commit().await?;
    crate::events::emit(
        crate::events::SCOPE_IMPORT,
        format!(
            "已删除「{}」{} 的音浪数据（{} 条）",
            q.biz_date,
            if auto_team {
                "全部团别"
            } else if team == "male" {
                "男团"
            } else {
                "女团"
            },
            metrics
        ),
    );
    Ok(Json(
        json!({ "ok": true, "metrics": metrics, "batches": batches }),
    ))
}

/* ------------------------ 月度时长导入（按平台月报） ------------------------ */
/*
   用户口径（2026-10-10）：「每日不能导入时长，只有月时长」。
   时长只有「按月报整月导入」一个来源 —— 按日导入不再写 daily_metric.minutes（见 strip_minutes），
   月度文件一人一行（「9月份时长.csv」里的「开播有效时长」= 整月累计，如 75小时29分钟41秒），
   落 monthly_duration(person_id, period, minutes)；月榜 / 个人月卡优先用这个整月值，
   某月没有整月记录（历史月份）才回落到 daily_metric 求和。
*/

/// monthly_duration 表还没建（库没跑 007 迁移）时，给一句能照做的提示，别只报 SQL 错
fn month_table_err(e: sqlx::Error) -> anyhow::Error {
    month_missing_hint(e.into())
}

/// 把「表不存在」翻译成可照做的提示（anyhow 版本，供 pgrs::query 的返回用）
fn month_missing_hint(e: anyhow::Error) -> anyhow::Error {
    let msg = e.to_string();
    if msg.contains("monthly_duration") || msg.contains("doesn't exist") {
        return anyhow::anyhow!(
            "月度时长表不存在：先在库里执行 `pk-server migrate`（007_monthly_duration）再导入"
        );
    }
    e
}

/// 某月已导入的整月时长 →（行数, 分钟合计）。period 已过 valid_period（纯数字 + '-'）白名单。
async fn month_existing(st: &AppState, period: &str) -> anyhow::Result<(i64, i64)> {
    let sql = format!(
        "SELECT COUNT(*) AS n, (COALESCE(SUM(minutes), 0))::BIGINT AS m \
         FROM monthly_duration WHERE period = '{period}'"
    );
    let row = pgrs::query(&st.pools, "LOCAL", &sql)
        .await
        .map_err(month_missing_hint)?
        .into_iter()
        .next();
    let (n, m) = match row {
        Some(r) => (
            r.try_get::<i64, _>("n").unwrap_or(0),
            r.try_get::<i64, _>("m").unwrap_or(0),
        ),
        None => (0, 0),
    };
    Ok((n, m))
}

/// 某月「日明细时长」求和（person_id → 分钟）。只用于对账提示：
/// 月度口径以平台月报为准，这里的数字只是告诉运营「日明细加起来差多少」。
async fn month_daily_minutes(st: &AppState, period: &str) -> anyhow::Result<HashMap<u64, i64>> {
    let sql = format!(
        "SELECT person_id, (SUM(minutes))::BIGINT AS m FROM daily_metric \
         WHERE TO_CHAR(biz_date, 'YYYY-MM') = '{period}' GROUP BY person_id"
    );
    let rows = pgrs::query(&st.pools, "LOCAL", &sql).await?;
    let mut out: HashMap<u64, i64> = HashMap::new();
    for r in &rows {
        let pid = read_u64(r, "person_id").unwrap_or(0);
        if pid > 0 {
            out.insert(pid, read_opt_i64(r, "m").unwrap_or(0));
        }
    }
    Ok(out)
}

/// 月度批次备注：失败 / 跳过的摘要（note 列 VARCHAR(512)，先截断）
fn month_note(failures: &[Value], skipped: &[String]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(s) = failures_summary(failures) {
        parts.push(s);
    }
    if !skipped.is_empty() {
        parts.push(format!("跳过 {} 人（文件里没有时长）", skipped.len()));
    }
    (!parts.is_empty()).then(|| truncate_chars(&parts.join("；"), NOTE_MAX_CHARS))
}

/// POST /api/admin/import/month/preview：月度时长预览（解析 + 匹配 + 对账），不落库
pub async fn import_month_preview(
    State(st): State<ArcState>,
    Json(body): Json<ImportBody>,
) -> ApiResult {
    require_local(&st)?;
    let period = body.period.as_deref().unwrap_or("").trim().to_string();
    valid_period(&period)?;
    let team = body.team.as_deref().unwrap_or("");
    if !team.is_empty() && !is_auto_team(team) {
        valid_team(team)?;
    }
    let raw_rows = month_body_rows(&body.text, &body.rows).await?;
    ensure(
        !raw_rows.is_empty(),
        "没有解析到时长列：确认文件里有「时长 / 开播有效时长」这一列（月度时长只认时长列）",
    )?;
    let ctx = load_match_ctx(&st).await?;
    let aliases = merged_aliases(&st).await;
    let resolved: Vec<ResolvedRow> = raw_rows
        .iter()
        .map(|r| {
            resolve_row(
                r,
                &ctx,
                &aliases,
                if team.is_empty() || is_auto_team(team) {
                    None
                } else {
                    Some(team)
                },
            )
        })
        .collect();
    let merged = merge_rows_by_person(&resolved);
    let matched = merged.iter().filter(|r| r.matched).count();
    let minutes_total: i64 = merged
        .iter()
        .filter(|r| r.matched)
        .filter_map(|r| r.raw.minutes)
        .map(|m| m as i64)
        .sum();
    let (existing_rows, existing_minutes) = month_existing(&st, &period).await?;
    let daily = month_daily_minutes(&st, &period).await?;
    // 对账：该月**已有日明细时长**、且与月报不一致的人（月报优先，这里只提示，不改口径）。
    // 日明细为 0 的不列进来：每日不导时长，0 是常态，全列出来只会刷屏。
    let diffs: Vec<Value> = merged
        .iter()
        .filter_map(|r| {
            let pid = r.person_id?;
            let imported = r.raw.minutes? as i64;
            let daily_minutes = daily.get(&pid).copied().unwrap_or(0);
            (daily_minutes > 0 && daily_minutes != imported).then(|| {
                json!({
                    "personId": pid,
                    "name": truncate_chars(&r.raw.name, 64),
                    "imported": imported,
                    "daily": daily_minutes,
                })
            })
        })
        .take(30)
        .collect();
    Ok(Json(json!({
        "period": period,
        "team": team,
        "total": merged.len(),
        "matched": matched,
        "unmatched": merged.len() - matched,
        "minutesTotal": minutes_total,
        "existingRows": existing_rows,
        "existingMinutes": existing_minutes,
        "dailyMinutes": daily.values().copied().sum::<i64>(),
        "diffs": diffs,
        "rows": merged,
    })))
}

/// POST /api/admin/import/month/commit：整月时长写 monthly_duration + import_batch 审计
pub async fn import_month_commit(
    State(st): State<ArcState>,
    Json(body): Json<ImportBody>,
) -> ApiResult {
    require_local(&st)?;
    let period = body.period.as_deref().unwrap_or("").trim().to_string();
    valid_period(&period)?;
    let team = body.team.as_deref().unwrap_or("auto");
    let mode = body.mode.as_deref().unwrap_or("merge");
    let rows = month_body_rows(&body.text, &body.rows).await?;
    let report = commit_month_duration(
        &st,
        &period,
        team,
        rows,
        mode,
        body.filename.as_deref().unwrap_or("月度时长导入"),
    )
    .await?;
    Ok(Json(json!({
        "ok": true,
        "period": period,
        "batchId": report.batch_id,
        "total": report.total,
        "okRows": report.ok_rows,
        "unchanged": report.unchanged,
        "failedRows": report.failures.len(),
        "failures": report.failures,
        "skipped": report.skipped,
        "minutesTotal": report.minutes_total,
        "pending": report.pending,
        // 月度时长没有「当日缺数据」概念，字段留着保持前端结构一致
        "missing": report.missing,
    })))
}

/// 月度时长提交核心：匹配 + 合并（一人多号时长求和）+ 事务写入 + 批次审计。
pub async fn commit_month_duration(
    st: &Arc<AppState>,
    period: &str,
    team: &str,
    rows: Vec<ImportRow>,
    mode: &str,
    filename: &str,
) -> anyhow::Result<CommitReport> {
    anyhow::ensure!(
        st.sources.iter().any(|s| s.id == "LOCAL"),
        "管理端需要 LOCAL 数据源（douyin_writer 对应的本地 PostgreSQL）"
    );
    let pool = st.writer.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "写库不可用：douyin_writer 账号未配置或连不上（检查 DB_WRITER_* 环境变量），只读功能不受影响"
        )
    })?;
    valid_period(period)?;
    let auto_team = is_auto_team(team);
    if !auto_team {
        valid_team(team)?;
    }
    let team_col = if auto_team { "mixed" } else { team };
    anyhow::ensure!(
        mode == "merge" || mode == "reject",
        "mode 只能是 merge / reject"
    );
    anyhow::ensure!(!rows.is_empty(), "没有解析到任何数据行");
    let biz_date = period_first_day(period);

    let ctx = load_match_ctx(st).await?;
    let aliases = merged_aliases(st).await;
    let team_filter = if auto_team { None } else { Some(team) };
    let resolved: Vec<ResolvedRow> = rows
        .iter()
        .map(|r| resolve_row(r, &ctx, &aliases, team_filter))
        .collect();
    // 一人多号（一个主播挂 2~3 个抖音号）→ 整月时长求和，与按日导入的合并口径一致
    let merged = merge_rows_by_person(&resolved);

    let mut failures: Vec<Value> = Vec::new();
    let mut ok_rows: Vec<&ResolvedRow> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for r in &merged {
        if !r.matched {
            // 月度值不进暂存区：import_pending 是按「某一天」补账的，整月值摊到一天会失真。
            // 有抖音号 / ID 也要先建档，建完再重导这个月的文件。
            failures.push(json!({
                "name": truncate_chars(&r.raw.name, 64),
                "reason": "未匹配到主播（月度时长不进暂存区）：先在主播台账建档或绑定抖音号，再重导该月文件"
            }));
            continue;
        }
        if let Some(reason) = &r.team_conflict {
            failures.push(json!({ "name": r.raw.name, "reason": reason }));
            continue;
        }
        if r.raw.minutes.is_none() {
            // 时长列有值但解析失败（如「未开播」「1.5h」「abc」）时 row.err 里带着原因，
            // 一并报出来——否则运营只见预览人数少了一个，没有任何线索可查。
            skipped.push(truncate_chars(
                &match r.raw.err.as_deref().filter(|e| !e.is_empty()) {
                    Some(e) => format!("{}（{}）", r.raw.name, e),
                    None => format!("{}（时长列为空）", r.raw.name),
                },
                96,
            ));
            continue;
        }
        ok_rows.push(r);
    }

    let mut tx = pool.begin().await?;
    if mode == "reject" {
        let cnt: i64 = sqlx::query(&crate::pq::rewrite(
            "SELECT COUNT(*) AS n FROM monthly_duration md JOIN person p ON p.id = md.person_id \
             WHERE md.period = ?",
        ))
        .bind(period)
        .map(|r: sqlx::postgres::PgRow| r.try_get::<i64, _>("n").unwrap_or(0))
        .fetch_one(&mut *tx)
        .await
        .map_err(month_table_err)?;
        if cnt > 0 {
            anyhow::bail!(
                "reject 模式：{period} 已有 {cnt} 条月度时长，请改用 merge（幂等覆盖）或先删除该月时长"
            );
        }
    }

    let batch = sqlx::query(&crate::pq::rewrite(
        "INSERT INTO import_batch (source, filename, biz_date, team, imported_rows, ok_rows, failed_rows, note)
         VALUES ('admin-import-month', ?, ?::date, ?, ?, 0, 0, ?) RETURNING id",
    ))
    .bind(truncate_chars(filename, FILENAME_MAX_CHARS))
    .bind(&biz_date)
    .bind(team_col)
    .bind(merged.len() as i64)
    .bind(month_note(&failures, &skipped))
    .fetch_one(&mut *tx)
    .await?;
    let batch_id = batch.try_get::<i64, _>(0)?;

    let mut ok_count: u32 = 0;
    let mut unchanged: u32 = 0;
    let mut minutes_total: i64 = 0;
    for r in &ok_rows {
        let person_id = r.person_id.expect("调用前已保证 matched");
        let minutes = r.raw.minutes.unwrap_or(0.0) as i64;
        // 相同不覆盖（与按日导入同一口径）：值一样就一行不写，updated_at 也不动
        let cur: Option<i64> = sqlx::query(&crate::pq::rewrite(
            "SELECT minutes FROM monthly_duration WHERE person_id = ? AND period = ?",
        ))
        .bind(person_id as i64)
        .bind(period)
        .map(|row: sqlx::postgres::PgRow| row.try_get::<i64, _>(0).unwrap_or(0))
        .fetch_optional(&mut *tx)
        .await
        .map_err(month_table_err)?;
        if cur == Some(minutes) {
            unchanged += 1;
            minutes_total += minutes;
            continue;
        }
        sqlx::query(&crate::pq::rewrite(
            "INSERT INTO monthly_duration (person_id, period, minutes, source_batch_id)
             VALUES (?,?,?,?)
             ON CONFLICT (person_id, period) DO UPDATE SET minutes = EXCLUDED.minutes, source_batch_id = EXCLUDED.source_batch_id",
        ))
        .bind(person_id as i64)
        .bind(period)
        .bind(minutes)
        .bind(batch_id as i64)
        .execute(&mut *tx)
        .await
        .map_err(month_table_err)?;
        ok_count += 1;
        minutes_total += minutes;
    }

    let note = month_note(&failures, &skipped);
    sqlx::query(&crate::pq::rewrite(
        "UPDATE import_batch SET ok_rows = ?, failed_rows = ?, note = ? WHERE id = ?",
    ))
    .bind(ok_count as i64)
    .bind(failures.len() as i64)
    .bind(&note)
    .bind(batch_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    st.clear_cache();
    spawn_post_import_backup();
    crate::events::emit(
        crate::events::SCOPE_IMPORT,
        format!(
            "{} 月度时长导入 {ok_count} 人（时长合计 {} 小时{}分）{}",
            period,
            minutes_total / 60,
            minutes_total % 60,
            if unchanged > 0 {
                format!("，另有 {unchanged} 人值没变未覆盖")
            } else {
                String::new()
            }
        ),
    );

    Ok(CommitReport {
        batch_id,
        total: merged.len(),
        ok_rows: ok_count,
        unchanged,
        failures,
        // 月度值不进暂存区（见上面的失败原因）
        pending: Vec::new(),
        skipped,
        missing: Vec::new(),
        dropped_minutes: 0,
        minutes_total,
    })
}

#[derive(Deserialize)]
pub struct MonthQuery {
    /// 业务月份 YYYY-MM
    pub period: String,
    /// male / female / auto（auto = 不限团别）
    pub team: String,
}

/// GET /api/admin/import/month-status?period=YYYY-MM&team=
pub async fn import_month_status(
    State(st): State<ArcState>,
    axum::extract::Query(q): axum::extract::Query<MonthQuery>,
) -> ApiResult {
    require_local(&st)?;
    let period = q.period.trim().to_string();
    valid_period(&period)?;
    let (rows, minutes) = month_existing(&st, &period).await?;
    let biz_date = period_first_day(&period);
    let last = pgrs::query(
        &st.pools,
        "LOCAL",
        &format!(
            "SELECT filename, TO_CHAR(created_at, 'YYYY-MM-DD HH24:MI:SS') AS ts \
             FROM import_batch WHERE source = 'admin-import-month' AND biz_date = '{biz_date}' \
             ORDER BY id DESC LIMIT 1"
        ),
    )
    .await
    .unwrap_or_default()
    .into_iter()
    .next()
    .map(|r| {
        json!({
            "filename": r.try_get::<String, _>("filename").unwrap_or_default(),
            "at": r.try_get::<String, _>("ts").unwrap_or_default(),
        })
    });
    Ok(Json(json!({
        "period": period,
        "rows": rows,
        "minutes": minutes,
        "lastBatch": last,
    })))
}

/// DELETE /api/admin/import/month?period=YYYY-MM&team=：删除某月已导入的整月时长
pub async fn import_month_delete(
    State(st): State<ArcState>,
    axum::extract::Query(q): axum::extract::Query<MonthQuery>,
) -> ApiResult {
    require_local(&st)?;
    let period = q.period.trim().to_string();
    valid_period(&period)?;
    let team = q.team.trim().to_string();
    let auto_team = is_auto_team(&team);
    if !auto_team {
        valid_team(&team)?;
    }
    let biz_date = period_first_day(&period);
    let pool = writer_pool(&st)?;
    let mut tx = pool.begin().await?;
    let rows = if auto_team {
        sqlx::query(&crate::pq::rewrite(
            "DELETE FROM monthly_duration WHERE period = ?",
        ))
        .bind(&period)
        .execute(&mut *tx)
        .await
        .map_err(month_table_err)?
        .affected()
    } else {
        sqlx::query(&crate::pq::rewrite(
            // PG 方言：DELETE … USING（MySQL 的 DELETE md FROM … JOIN 在 PG 语法不通）
            "DELETE FROM monthly_duration md \
             USING person p \
             WHERE p.id = md.person_id AND md.period = ? AND p.gender = ?",
        ))
        .bind(&period)
        .bind(&team)
        .execute(&mut *tx)
        .await
        .map_err(month_table_err)?
        .affected()
    };
    let batches = if auto_team {
        sqlx::query(&crate::pq::rewrite(
            "DELETE FROM import_batch WHERE source = 'admin-import-month' AND biz_date = ?::date",
        ))
        .bind(&biz_date)
        .execute(&mut *tx)
        .await?
        .affected()
    } else {
        sqlx::query(&crate::pq::rewrite(
            "DELETE FROM import_batch WHERE source = 'admin-import-month' AND biz_date = ?::date AND team = ?",
        ))
        .bind(&biz_date)
        .bind(&team)
        .execute(&mut *tx)
        .await?
        .affected()
    };
    tx.commit().await?;
    st.clear_cache();
    crate::events::emit(
        crate::events::SCOPE_IMPORT,
        format!(
            "已删除「{period}」{} 的月度时长（{} 条）",
            if auto_team { "全部团别" } else { &team },
            rows
        ),
    );
    Ok(Json(
        json!({ "ok": true, "rows": rows, "batches": batches }),
    ))
}

/* --------------------------------- 单测 --------------------------------- */

/// 按主键取主播行：016 起带 `phone`，库没跑 016 时退回
/// [`dialect::STREAMER_BY_ID_SQL_LEGACY`]（与 [`streamer_rows`] 同一套兜底思路）。
/// 详情页 / 设师傅都在用它 —— 一条 Unknown column 会让编辑弹窗直接打不开。
async fn streamer_row_by_id(st: &AppState, id: u64) -> anyhow::Result<sqlx::postgres::PgRow> {
    let rows = match pgrs::query_with_binds(&st.pools, "LOCAL", dialect::STREAMER_BY_ID_SQL, |q| {
        q.bind(id as i64)
    })
    .await
    {
        Ok(rows) => rows,
        Err(e) if is_missing_phone_column(&e) => {
            pgrs::query_with_binds(
                &st.pools,
                "LOCAL",
                dialect::STREAMER_BY_ID_SQL_LEGACY,
                |q| q.bind(id as i64),
            )
            .await?
        }
        Err(e) => return Err(e),
    };
    rows.into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("主播不存在或已删除"))
}

/// GET /api/admin/streamers/{id}/detail：主播详情（含全部账号）
pub async fn streamer_detail(State(st): State<ArcState>, Path(id): Path<u64>) -> ApiResult {
    require_local(&st)?;
    let row = streamer_row_by_id(&st, id).await?;
    let accounts =
        pgrs::query_with_binds(&st.pools, "LOCAL", dialect::ACCOUNTS_BY_PERSON_SQL, |q| {
            q.bind(id as i64)
        })
        .await?;
    let accounts: Vec<Value> = accounts
        .iter()
        .map(|r| {
            json!({
                "id": read_u64(r, "id"),
                "anchorId": read_opt_str(r, "anchor_id"),
                "douyinNo": read_opt_str(r, "douyin_no"),
                "anchorName": read_opt_str(r, "anchor_name"),
                "isPrimary": r.try_get::<i16, _>("is_primary").unwrap_or(0) == 1,
                "status": read_opt_str(r, "status").unwrap_or_else(|| "active".into()),
                "createdAt": read_opt_str(r, "created"),
            })
        })
        .collect();
    Ok(Json(json!({
        "personId": read_u64(&row, "id"),
        "name": read_str(&row, "name"),
        "team": read_str(&row, "gender"),
        "masterId": read_opt_u64(&row, "master_id"),
        "generation": read_opt_i64(&row, "generation"),
        "groupName": read_opt_str(&row, "group_name"),
        "status": read_opt_str(&row, "status").unwrap_or_else(|| "active".into()),
        "phone": read_opt_str(&row, "phone").unwrap_or_default(),
        "accounts": accounts,
    })))
}

#[derive(Deserialize)]
pub struct BindAccountBody {
    #[serde(rename = "anchorId")]
    pub anchor_id: Option<String>,
    #[serde(rename = "douyinNo")]
    pub douyin_no: Option<String>,
    #[serde(rename = "anchorName")]
    pub anchor_name: Option<String>,
    #[serde(rename = "isPrimary")]
    pub is_primary: Option<bool>,
}

/* ------------------------ 批量开号（016，docs/13 §3） ------------------------ */

#[derive(Deserialize)]
pub struct BatchAccountsBody {
    /// `[{ personId, phone? }]`；phone 留空 = 沿用台账上已登记的手机号
    pub items: Option<Vec<BatchAccountItem>>,
    /// 全部条目的兜底口令：某个人既没手机号、台账也没登记时用它当初始密码。
    /// 不传 = 这类人直接 skipped（不强行开一个登不进去的号）
    #[serde(rename = "defaultPass")]
    pub default_pass: Option<String>,
    /// true = 已存在的账号也把密码重置成初始密码（补开号 / 全量重置场景）。
    /// **默认 false**：重跑一遍批量不该把 104 个人的密码全刷掉。
    #[serde(rename = "forceReset")]
    pub force_reset: Option<bool>,
    /// true = 只算账不写库（运营先看清"会开几个、谁会失败"再提交）
    pub dry_run: Option<bool>,
}

#[derive(Deserialize)]
pub struct BatchAccountItem {
    #[serde(rename = "personId")]
    pub person_id: Option<i64>,
    pub phone: Option<String>,
}

/// POST /api/admin/my-accounts/batch：给一批主播开「展示端登录账号」。
///
/// 为什么必须有这个接口（docs/13 §3）：104 个在职主播只有 1 个有登录账号，
/// 单条 `POST /api/admin/my-accounts` 要运营点 104 次、每次手打姓名 + 手机号 + 密码，
/// 而且单条接口**不带 person_id**（015 之前的模型），开出来的人改名就失联。
///
/// 与单条接口的关系：**单条接口一行不改**（向后兼容，前端 BotPage / StreamerPage 照常）。
/// 本接口只是把同样的写入逻辑批量化，并补上 person_id 锚点。
///
/// 返回 `{ ok, created, updated, skipped, failed, items[] }` —— items 逐条给结论与原因，
/// 前端直接渲染成结果表（运营要把初始密码抄给主播）。
pub async fn batch_my_accounts(
    State(st): State<ArcState>,
    Json(body): Json<BatchAccountsBody>,
) -> ApiResult {
    require_local(&st)?;
    let items = body.items.unwrap_or_default();
    if items.is_empty() {
        return Err(anyhow::anyhow!("名单为空：至少选一个主播").into());
    }
    if items.len() > crate::store::BATCH_ACCOUNT_MAX {
        return Err(anyhow::anyhow!(
            "一次最多开 {} 个账号（本次 {} 个，请分批提交）",
            crate::store::BATCH_ACCOUNT_MAX,
            items.len()
        )
        .into());
    }
    let inputs: Vec<crate::store::BatchAccountInput> = items
        .iter()
        .filter_map(|i| {
            let pid = i.person_id?;
            (pid > 0).then(|| crate::store::BatchAccountInput {
                person_id: pid,
                phone: i
                    .phone
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            })
        })
        .collect();
    if inputs.is_empty() {
        return Err(anyhow::anyhow!("名单里没有有效的主播 id").into());
    }

    /* dry-run：不写库，只回一条"会怎么走"的汇总（打开页面时的预览）。
    真跑一次再回滚是不可接受的（密码已经换了），所以预览必须真的不写。 */
    if body.dry_run.unwrap_or(false) {
        let total = inputs.len();
        return Ok(Json(json!({
            "ok": true, "dryRun": true, "total": total,
            "note": format!("预览：将对 {total} 个主播开号（初始密码 = 手机号后四位；无手机号时用 defaultPass）"),
        })));
    }

    let rows = crate::store::my_accounts_batch_open(
        &st,
        &inputs,
        body.default_pass.as_deref(),
        body.force_reset.unwrap_or(false),
    )
    .await
    .map_err(|e| anyhow::anyhow!(e))?;

    let (mut created, mut updated, mut skipped, mut failed) = (0usize, 0usize, 0usize, 0usize);
    for r in &rows {
        match r.status {
            "created" => created += 1,
            "updated" => updated += 1,
            "skipped" => skipped += 1,
            _ => failed += 1,
        }
    }
    tracing::info!(
        created,
        updated,
        skipped,
        failed,
        "批量开号完成（source=batch）"
    );
    crate::events::emit(
        crate::events::SCOPE_ROSTER,
        format!("批量开号：新增 {created} 个、更新 {updated} 个（失败 {failed}）"),
    );
    Ok(Json(json!({
        "ok": true,
        "created": created,
        "updated": updated,
        "skipped": skipped,
        "failed": failed,
        "items": rows,
    })))
}

/// PUT /api/admin/streamers/{id}/phone：只改主播手机号（台账编辑弹窗里单独一项）。
///
/// 单独开口子的原因：手机号要过「格式 + 台账查重 + 登录账号查重」三道校验，
/// 失败要给出准确原因（"被谁占了"），混在 PUT /api/admin/streamers/{id} 里只能
/// 返回一句通用的"保存失败"。
#[derive(Deserialize)]
pub struct PersonPhoneBody {
    pub phone: Option<String>,
}

pub async fn set_person_phone(
    State(st): State<ArcState>,
    Path(id): Path<u64>,
    Json(body): Json<PersonPhoneBody>,
) -> ApiResult {
    require_local(&st)?;
    ensure(id > 0, "无效的主播 id")?;
    let phone = body
        .phone
        .ok_or_else(|| anyhow::anyhow!("缺少 phone 字段"))?;
    crate::store::person_set_phone(&st, id, &phone)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    /* 写后回读校验（2026-10-13）：与 my_account_set 同口径——杜绝「返回 ok 但库里
       没写入」的静默失败（64 环境曾出现运营以为绑上了实际没绑）。 */
    let saved_phone = sqlx::query(&crate::pq::rewrite(
        "SELECT phone FROM person WHERE id = ? AND deleted_at IS NULL",
    ))
    .bind(id as i64)
    .fetch_optional(st.writer.as_ref().ok_or_else(|| anyhow::anyhow!("写库不可用"))?)
    .await
    .map_err(|e| anyhow::anyhow!("回读失败：{e}"))?
    .and_then(|r| r.try_get::<String, _>("phone").ok())
    .unwrap_or_default();
    if saved_phone != phone.trim() {
        return Err(anyhow::anyhow!(
            "手机号「{}」写入后校验失败（库里读到的是「{}」），请重试",
            phone.trim(),
            saved_phone
        )
        .into());
    }
    Ok(Json(
        json!({ "ok": true, "personId": id, "phone": phone.trim() }),
    ))
}

/// POST /api/admin/streamers/{id}/accounts：绑定抖音号（一人可挂多个）
///
/// 2026-10-10：只填抖音号也能绑 —— 采集 ID（anchor_id）在没有采集系统 ID 时用抖音号兜底。
/// 抖音号全局唯一（一个人只能对应一个主播），被别人占了直接报错，不静默改绑。
pub async fn bind_account(
    State(st): State<ArcState>,
    Path(id): Path<u64>,
    Json(body): Json<BindAccountBody>,
) -> ApiResult {
    require_local(&st)?;
    let pool = writer_pool(&st)?;
    ensure(id > 0, "无效的主播 id")?;
    let douyin_no = body
        .douyin_no
        .as_deref()
        .or(body.anchor_id.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("抖音号必填（一人可挂多个；抖音号全局唯一）"))?;
    let no = crate::store::validate_douyin_no(douyin_no).map_err(|e| anyhow::anyhow!(e))?;
    // 抖音号被别人占了 → 直接拒（不静默改绑，否则等于把别人的号抢过来）
    check_douyin_no_free(&st, Some(&no), Some(id)).await?;
    // 同一个人重复挂同一个号：幂等，不报错也不新增。
    // 但显式要求设主号时要真的切（页面「设为主号」走的就是这条）
    let existing: Option<(i64, i16)> = sqlx::query(&crate::pq::rewrite(
        "SELECT id, is_primary FROM account WHERE person_id = ? AND douyin_no = ? LIMIT 1",
    ))
    .bind(id as i64)
    .bind(&no)
    .map(|r: sqlx::postgres::PgRow| {
        (
            r.try_get::<i64, _>("id").unwrap_or(0),
            r.try_get::<i16, _>("is_primary").unwrap_or(0),
        )
    })
    .fetch_optional(pool)
    .await?;
    if let Some((acc_id, was_primary)) = existing {
        if body.is_primary == Some(true) && was_primary == 0 {
            sqlx::query(&crate::pq::rewrite(
                "UPDATE account SET is_primary = 0 WHERE person_id = ? AND is_primary = 1",
            ))
            .bind(id as i64)
            .execute(pool)
            .await?;
            sqlx::query(&crate::pq::rewrite(
                "UPDATE account SET is_primary = 1 WHERE id = ?",
            ))
            .bind(acc_id)
            .execute(pool)
            .await?;
            let absorbed = absorb_pending_best_effort(&st, id, "bind_account").await;
            return Ok(Json(
                json!({ "ok": true, "douyinNo": no, "promoted": true, "absorbed": absorbed }),
            ));
        }
        // 号本来就在这个人名下：暂存区里如果有这个号的未认领行，一样要补账
        let absorbed = absorb_pending_best_effort(&st, id, "bind_account").await;
        return Ok(Json(
            json!({ "ok": true, "douyinNo": no, "dedup": true, "absorbed": absorbed }),
        ));
    }

    // 采集 ID 没单独给就用抖音号兜底（anchor_id 是唯一键，必须非空）
    let anchor_id = body
        .anchor_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| no.clone());
    // 第一次绑号 = 主号（daily_metric.anchor_id 回填要用）
    let has_any: bool = sqlx::query(&crate::pq::rewrite(
        "SELECT 1 FROM account WHERE person_id = ? LIMIT 1",
    ))
    .bind(id as i64)
    .fetch_optional(pool)
    .await?
    .is_some();
    let is_primary = body.is_primary.unwrap_or(!has_any);

    let mut tx = pool.begin().await?;
    if is_primary {
        sqlx::query(&crate::pq::rewrite(
            "UPDATE account SET is_primary = 0 WHERE person_id = ? AND is_primary = 1",
        ))
        .bind(id as i64)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(&crate::pq::rewrite(
        "INSERT INTO account (person_id, anchor_id, douyin_no, anchor_name, is_primary)
         VALUES (?,?,?,?,?)
         ON CONFLICT (anchor_id) DO UPDATE SET
           person_id = EXCLUDED.person_id, douyin_no = EXCLUDED.douyin_no,
           anchor_name = EXCLUDED.anchor_name, is_primary = EXCLUDED.is_primary",
    ))
    .bind(id as i64)
    .bind(&anchor_id)
    .bind(&no)
    .bind(body.anchor_name.as_deref().unwrap_or(""))
    .bind(is_primary as i16)
    .execute(&mut *tx)
    .await
    .map_err(|e| map_account_err(e, &anchor_id))?;
    tx.commit().await?;
    crate::events::emit(
        crate::events::SCOPE_ROSTER,
        format!("{} 绑定抖音号 {no}", person_name_of_id(&st, id).await),
    );
    // 绑号后：暂存区里这个号（或采集 ID）的导入行补账进 daily_metric
    let absorbed = absorb_pending_best_effort(&st, id, "bind_account").await;
    Ok(Json(
        json!({ "ok": true, "douyinNo": no, "anchorId": anchor_id, "absorbed": absorbed }),
    ))
}

/// 名字（事件文案用；查不到返回 #id）
async fn person_name_of_id(st: &AppState, id: u64) -> String {
    pgrs::query_with_binds(
        &st.pools,
        "LOCAL",
        "SELECT name FROM person WHERE id = ? LIMIT 1",
        |q| q.bind(id as i64),
    )
    .await
    .ok()
    .and_then(|rows| {
        rows.into_iter()
            .next()
            .and_then(|r| r.try_get::<String, _>("name").ok())
    })
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| format!("#{id}"))
}

/// DELETE /api/admin/streamers/{id}/accounts/{accountId}：解绑一个抖音号
///（主号解绑后，若该主播还有别的号，自动把最早那个提为主号，避免 anchor_id 回填断链）
pub async fn unbind_account(
    State(st): State<ArcState>,
    Path((id, account_id)): Path<(u64, u64)>,
) -> ApiResult {
    require_local(&st)?;
    let pool = writer_pool(&st)?;
    ensure(id > 0 && account_id > 0, "无效的 id")?;
    let was_primary: bool = sqlx::query(&crate::pq::rewrite(
        "SELECT is_primary FROM account WHERE id = ? AND person_id = ?",
    ))
    .bind(account_id as i64)
    .bind(id as i64)
    .fetch_optional(pool)
    .await?
    .and_then(|r| r.try_get::<i16, _>("is_primary").ok())
    .unwrap_or(0)
        == 1;

    /* P2-10（2026-10-10）：先在事务里读出这个账号的 anchor_id / douyin_no，再删行。
    wave_snapshot / duration_snapshot 按 anchor_id 关联（表里没有 person_id），
    直接删 account 会让快照变成孤儿——WAVES_A / SERIES_A 都是 JOIN account 取归属，
    JOIN 不上就没人统计得到：副号历史音浪追溯性消失。
    归属/保留策略（选最小可靠做法，不新增列/迁移）：
    - anchor_id 唯一且没有别的账号复用它 → 快照**保留**（库里的真实历史，删了找不回），
      改而把快照挪进 import_pending（ absorbed='unbind' 前缀），台账里能看见这段历史
      等该号重新绑人（同号重绑 anchor_id 相同）时快照自动重新归队，不丢数
    - anchor_id 已被其它账号占用（同号重绑后的旧行/脏数据）→ 快照本来归新账号，
      什么都不用动
    删账号与快照处理在同一个事务里，避免删了账号还没处理快照时被并发读到半个状态。 */
    let mut tx = pool.begin().await?;
    let acc: Option<(String, String)> = sqlx::query(&crate::pq::rewrite(
        "SELECT anchor_id, douyin_no FROM account WHERE id = ? AND person_id = ?",
    ))
    .bind(account_id as i64)
    .bind(id as i64)
    .map(|r: sqlx::postgres::PgRow| {
        (
            r.try_get::<String, _>("anchor_id").unwrap_or_default(),
            r.try_get::<String, _>("douyin_no").unwrap_or_default(),
        )
    })
    .fetch_optional(&mut *tx)
    .await?;
    let Some((anchor_id, douyin_no)) = acc else {
        return Err(anyhow::anyhow!("这个抖音号不存在或不属于该主播").into());
    };
    let res = sqlx::query(&crate::pq::rewrite(
        "DELETE FROM account WHERE id = ? AND person_id = ?",
    ))
    .bind(account_id as i64)
    .bind(id as i64)
    .execute(&mut *tx)
    .await?;
    if res.affected() == 0 {
        return Err(anyhow::anyhow!("这个抖音号不存在或不属于该主播").into());
    }
    // anchor_id 是否还有别的账号行在用（同号重绑过的旧行：快照继续归它，不算孤儿）
    let anchor_still_used: i64 = sqlx::query(&crate::pq::rewrite(
        "SELECT COUNT(*) FROM account WHERE anchor_id = ? AND id <> ?",
    ))
    .bind(&anchor_id)
    .bind(account_id as i64)
    .fetch_one(&mut *tx)
    .await?
    .try_get(0)
    .unwrap_or(0);
    if anchor_still_used == 0 && !anchor_id.is_empty() {
        // 快照有数才落暂存区：没有历史就不制造空行
        let snap: (i64, i64) = sqlx::query(&crate::pq::rewrite(
            "SELECT COUNT(*), COALESCE(SUM(wave_value), 0) FROM wave_snapshot WHERE anchor_id = ?",
        ))
        .bind(&anchor_id)
        .map(|r: sqlx::postgres::PgRow| {
            (
                r.try_get::<i64, _>(0).unwrap_or(0),
                r.try_get::<i64, _>(1).unwrap_or(0),
            )
        })
        .fetch_one(&mut *tx)
        .await?;
        let (n, sum) = snap;
        if n > 0 {
            // 一行汇总快照（每天不展开）：解除绑定的事实 + 快照条数与合计，够运营对账。
            // import_pending 以 douyin_no 认领（bind_account 后 absorb 自动补进 daily_metric
            // 的是暂存行里的 wave 值）；这里 wave 写合计，单日快照明细仍在 wave_snapshot 表里。
            let _ = sqlx::query(&crate::pq::rewrite(
                "INSERT INTO import_pending (name, douyin_no, biz_date, wave, source_batch_id)
                 VALUES (?,?,?::date,?,NULL)",
            ))
            .bind(format!(
                "（解绑留档）{} 副号快照 {} 条，音浪合计 {}",
                douyin_no, n, sum
            ))
            .bind(&anchor_id)
            .bind(chrono::Local::now().format("%Y-%m-%d").to_string())
            .bind(sum)
            .execute(&mut *tx)
            .await;
            tracing::info!(
                person_id = id,
                anchor_id = %anchor_id,
                snapshots = n,
                "解绑抖音号：wave_snapshot 保留，已汇总落 import_pending 留档"
            );
        }
    }
    tx.commit().await?;
    if was_primary {
        // 把剩下最早的一个提为主号（ON DUPLICATE 不需要，直接 UPDATE 最小 id 那条）
        let _ = sqlx::query(&crate::pq::rewrite(
            "UPDATE account SET is_primary = 1 WHERE person_id = ? AND id = \
               (SELECT min_id FROM (SELECT MIN(id) AS min_id FROM account WHERE person_id = ?) t)",
        ))
        .bind(id as i64)
        .bind(id as i64)
        .execute(pool)
        .await;
    }
    crate::events::emit(
        crate::events::SCOPE_ROSTER,
        format!("{} 解绑了一个抖音号", person_name_of_id(&st, id).await),
    );
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct SetMasterBody {
    /// 要设为师傅的 person.id；null / 缺省 → 清除师傅
    #[serde(rename = "masterId")]
    pub master_id: Option<u64>,
}

/// PUT /api/admin/streamers/{id}/master：设置/清除师傅（person.master_id 自引用）
pub async fn set_master(
    State(st): State<ArcState>,
    Path(id): Path<u64>,
    Json(body): Json<SetMasterBody>,
) -> ApiResult {
    require_local(&st)?;
    let pool = writer_pool(&st)?;
    ensure(id > 0, "无效的主播 id")?;
    let master_id = body.master_id.filter(|m| *m > 0);

    // 本人必须存在（顺带取名字做回显/报错）
    let me = streamer_row_by_id(&st, id).await?;
    let me_name = read_str(&me, "name");

    let mut master_name: Option<String> = None;
    if let Some(mid) = master_id {
        ensure(mid != id, "不能把自己设为师傅")?;

        // 师傅必须是有效主播
        let mrow = pgrs::query_with_binds(&st.pools, "LOCAL", dialect::STREAMER_BY_ID_SQL, |q| {
            q.bind(mid as i64)
        })
        .await?;
        let mrow = mrow
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("要设为师傅的主播不存在或已删除（id={mid}）"))?;
        master_name = Some(read_str(&mrow, "name"));

        // 防环：沿师傅链向上走，若回到本人即成环（师徒关系必须是森林）
        let mut cursor = mid;
        for _ in 0..50 {
            let link = pgrs::query_with_binds(&st.pools, "LOCAL", dialect::MASTER_CHAIN_SQL, |q| {
                q.bind(cursor as i64)
            })
            .await?;
            let link = match link.into_iter().next() {
                Some(r) => r,
                None => break,
            };
            match read_opt_u64(&link, "master_id") {
                Some(next) if next == id => {
                    return Err(anyhow::anyhow!(
                        "不能设置：「{me_name}」已在「{}」的师傅链上，会形成环",
                        master_name.as_deref().unwrap_or_default()
                    )
                    .into())
                }
                Some(next) => cursor = next,
                None => break,
            }
        }
    }

    sqlx::query(&crate::pq::rewrite(dialect::SET_MASTER_SQL))
        .bind(master_id.map(|m| m as i64))
        .bind(id as i64)
        .execute(pool)
        .await?;
    Ok(Json(json!({
        "ok": true,
        "personId": id,
        "masterId": master_id,
        "masterName": master_name,
    })))
}

/* ----------------------- 批量登记师门（微信群接龙粘贴） ----------------------- */

/// 名字 -> 师傅名（师傅在册才有边；规划过程中会写入批内临时边用于防环）
pub type MasterGraph = HashMap<String, String>;
/// 名字 -> 在册同名人数（>1 即重名，重名行一律拦下让人工处理）
pub type NameCounts = HashMap<String, usize>;

/// 规划结果的一行。action：attach=挂接在册双方 / create=至少一方需要新建 /
/// unchanged=已经是这层师徒关系 / skip=没写师傅等 / error=这行有问题不执行
pub struct MasterPlanRow {
    pub raw: String,
    pub name: String,
    pub master: Option<String>,
    pub action: &'static str,
    pub person_known: bool,
    pub master_known: bool,
    pub note: String,
}

/// 把接龙解析结果规划成师门登记动作（纯函数，不碰库）。
///
/// 规则：
/// - 只认「名字 师傅」两段（leader 的第一个词是师傅，其余忽略）；
/// - 没写师傅 / 带分数（那是音浪名单）/ 命中排除表的行 → skip，绝不动师傅字段；
/// - 重名（同名>1）行一律拦下让人工处理，不猜；
/// - 防环：沿「名字 -> 师傅名」链向上走（含本批将要写入的边），回到自己即拒；
/// - `create_missing=false` 时缺主播或缺师傅都报错；true 时规划为 create。
pub fn plan_master_batch(
    parsed: &[ParsedRow],
    graph: &mut MasterGraph,
    name_counts: &NameCounts,
    create_missing: bool,
) -> Vec<MasterPlanRow> {
    let mut out = Vec::with_capacity(parsed.len());
    for row in parsed {
        let plan = |action: &'static str, name: String, master: Option<String>, note: String| {
            MasterPlanRow {
                raw: row.raw.clone(),
                person_known: name_counts.contains_key(&name),
                master_known: master.as_ref().is_some_and(|m| name_counts.contains_key(m)),
                action,
                name,
                master,
                note,
            }
        };
        if row.excluded {
            out.push(plan(
                "skip",
                row.name.clone(),
                None,
                "命中排除表，跳过".into(),
            ));
            continue;
        }
        if row.score.is_some() {
            out.push(plan(
                "skip",
                row.name.clone(),
                None,
                "像是「名字 分数」的音浪名单，不是师门登记".into(),
            ));
            continue;
        }
        // leader = 「名字」后面的所有词，取第一个当师傅；多人标注只认第一个
        let master = row
            .leader
            .as_deref()
            .and_then(|s| s.split_whitespace().next())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let Some(master) = master else {
            out.push(plan(
                "skip",
                row.name.clone(),
                None,
                "没写师傅，跳过（不会清掉现有师傅）".into(),
            ));
            continue;
        };
        let name = row.name.clone();
        if name == master {
            out.push(plan(
                "error",
                name,
                Some(master),
                "自己不能当自己的师傅".into(),
            ));
            continue;
        }
        let n_me = name_counts.get(&name).copied().unwrap_or(0);
        if n_me > 1 {
            out.push(plan(
                "error",
                name,
                Some(master),
                format!("有 {n_me} 个同名主播，请先在主播管理里合并/改名"),
            ));
            continue;
        }
        let n_m = name_counts.get(&master).copied().unwrap_or(0);
        if n_m > 1 {
            out.push(plan(
                "error",
                name,
                Some(master.clone()),
                format!("师傅名「{master}」有 {n_m} 个同名，请人工处理"),
            ));
            continue;
        }
        let known = n_me == 1;
        let master_known = n_m == 1;
        if !known && !create_missing {
            out.push(plan(
                "error",
                name,
                Some(master),
                "主播不在册（可勾选「自动创建」）".into(),
            ));
            continue;
        }
        if !master_known && !create_missing {
            out.push(plan(
                "error",
                name,
                Some(master.clone()),
                format!("师傅「{master}」不在册（可勾选「自动创建」）"),
            ));
            continue;
        }
        // 防环：沿师傅链向上走（含本批临时边）
        let mut cur = master.clone();
        let mut cycle = false;
        for _ in 0..50 {
            if cur == name {
                cycle = true;
                break;
            }
            match graph.get(&cur) {
                Some(next) => cur = next.clone(),
                None => break,
            }
        }
        if cycle {
            out.push(plan(
                "error",
                name,
                Some(master),
                "沿师傅链会绕回自己，形成环，已拦下".into(),
            ));
            continue;
        }
        // 已是这层关系 → 不写库
        if known && graph.get(&name).map(|m| m == &master).unwrap_or(false) {
            out.push(plan(
                "unchanged",
                name,
                Some(master),
                "已经是他的徒弟，无需改动".into(),
            ));
            continue;
        }
        // 登记本批临时边：同一批里「A B」「B A」第二行也能被拦住
        let prev_master = graph.get(&name).cloned();
        graph.insert(name.clone(), master.clone());
        let action = if known && master_known {
            "attach"
        } else {
            "create"
        };
        let note = match (known, master_known) {
            (true, true) => match prev_master {
                Some(prev) => format!("师傅由「{prev}」改为「{master}」"),
                None => format!("新挂师傅「{master}」"),
            },
            (false, true) => format!("新登记主播，师傅设为「{master}」"),
            (true, false) => format!("师傅「{master}」不在册，将新建后挂接"),
            (false, false) => "主播与师傅都不在册，将新建后挂接".to_string(),
        };
        out.push(plan(action, name, Some(master), note));
    }
    out
}

#[derive(Deserialize)]
pub struct MasterBatchBody {
    /// 接龙原文：每行「名字 师傅」，序号/加号/括号分隔都认（复用接龙名单解析）
    pub text: String,
    /// 库里没有的主播/师傅自动创建（gender=unknown，团别稍后人工补）；缺省 false = 报出来人工处理
    #[serde(rename = "createMissing", default)]
    pub create_missing: bool,
}

/// POST /api/admin/streamers/master-batch：把群接龙文本批量登记成师门关系。
///
/// 每行独立处理：一行失败不影响其他行；全部走 writer 池。完成后发 roster 事件，
/// 族谱页 / 主播管理会自动刷新。
pub async fn master_batch(
    State(st): State<ArcState>,
    Json(body): Json<MasterBatchBody>,
) -> ApiResult {
    require_local(&st)?;
    ensure(!body.text.trim().is_empty(), "文本是空的")?;
    let pool = writer_pool(&st)?;

    // 在册人员快照（只读 LOCAL）：id/name/master_id/generation
    let rows = pgrs::query(
        &st.pools,
        "LOCAL",
        "SELECT id, name, master_id, generation FROM person WHERE deleted_at IS NULL",
    )
    .await?;
    let mut id_by_name: HashMap<String, i64> = HashMap::new();
    let mut name_counts: NameCounts = HashMap::new();
    let mut name_of: HashMap<i64, String> = HashMap::new();
    let mut master_of: HashMap<i64, Option<i64>> = HashMap::new();
    let mut gen_of: HashMap<i64, Option<i64>> = HashMap::new();
    for r in &rows {
        let id = r.try_get::<i64, _>("id").unwrap_or(0);
        let name = read_str(r, "name");
        if id == 0 || name.is_empty() {
            continue;
        }
        *name_counts.entry(name.clone()).or_insert(0) += 1;
        id_of(&mut id_by_name, &name, id);
        name_of.insert(id, name);
        master_of.insert(id, read_opt_i64(r, "master_id").filter(|m| *m != id));
        gen_of.insert(id, read_opt_i64(r, "generation"));
    }
    // 名字 -> 师傅名（师傅在册才有边；自指忽略）
    let mut graph: MasterGraph = HashMap::new();
    for (id, m) in &master_of {
        let Some(mid) = m else { continue };
        let (Some(me), Some(mn)) = (name_of.get(id), name_of.get(mid)) else {
            continue;
        };
        graph.insert(me.clone(), mn.clone());
    }

    // 解析 + 规划
    let opts = ParseOptions::with_defaults();
    let parsed = parse_roster(&body.text, &opts);
    let plans = plan_master_batch(&parsed, &mut graph, &name_counts, body.create_missing);

    // 执行：每行独立，失败记下继续
    let mut updated = 0usize;
    let mut created = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut report: Vec<Value> = Vec::with_capacity(plans.len());
    for p in &plans {
        let mut item = json!({
            "raw": p.raw,
            "name": p.name,
            "master": p.master,
            "action": p.action,
            "note": p.note,
        });
        match p.action {
            "attach" => {
                let (Some(pid), Some(mid)) = (
                    id_by_name.get(&p.name),
                    p.master.as_deref().and_then(|m| id_by_name.get(m)),
                ) else {
                    failed += 1;
                    item["note"] = json!("内部错误：id 缺失");
                    report.push(item);
                    continue;
                };
                let gen = gen_of.get(mid).copied().flatten().map(|g| g + 1);
                match sqlx::query(&crate::pq::rewrite(
                    "UPDATE person SET master_id = ?, generation = ? WHERE id = ? AND deleted_at IS NULL",
                ))
                .bind(*mid)
                .bind(gen)
                .bind(*pid)
                .execute(pool)
                .await
                {
                    Ok(_) => {
                        updated += 1;
                        gen_of.insert(*pid, gen);
                        master_of.insert(*pid, Some(*mid));
                    }
                    Err(e) => {
                        failed += 1;
                        item["note"] = json!(format!("写入失败：{e}"));
                    }
                }
            }
            "create" => {
                // 先保证师傅在（不在则新建），再保证徒弟在（不在则建并挂上；在则补一次挂接）
                let master_res: anyhow::Result<i64> = async {
                    if let Some(id) = p.master.as_deref().and_then(|m| id_by_name.get(m)) {
                        return Ok(*id);
                    }
                    let mname = p.master.clone().unwrap_or_default();
                    let r = sqlx::query(&crate::pq::rewrite(
                        "INSERT INTO person (name, gender, master_id, generation, status) \
                         VALUES (?, 'unknown', NULL, NULL, 'active') RETURNING id",
                    ))
                    .bind(&mname)
                    .fetch_one(pool)
                    .await?;
                    Ok(r.try_get::<i64, _>(0)?)
                }
                .await;
                let mid = match master_res {
                    Ok(id) => {
                        if !id_by_name.contains_key(p.master.as_deref().unwrap_or_default()) {
                            created += 1;
                            name_of.insert(id, p.master.clone().unwrap_or_default());
                            gen_of.insert(id, None);
                            master_of.insert(id, None);
                            if let Some(mn) = &p.master {
                                id_by_name.insert(mn.clone(), id);
                            }
                        }
                        id
                    }
                    Err(e) => {
                        failed += 1;
                        item["note"] = json!(format!("新建师傅失败：{e}"));
                        report.push(item);
                        continue;
                    }
                };
                let gen = gen_of.get(&mid).copied().flatten().map(|g| g + 1);
                // 徒弟：在册 → 补一次 UPDATE；不在册 → INSERT
                let me_known = p.person_known && id_by_name.contains_key(&p.name);
                let res: anyhow::Result<i64> = if me_known {
                    let pid = id_by_name[&p.name];
                    sqlx::query(&crate::pq::rewrite(
                        "UPDATE person SET master_id = ?, generation = ? WHERE id = ? AND deleted_at IS NULL",
                    ))
                    .bind(mid)
                    .bind(gen)
                    .bind(pid)
                    .execute(pool)
                    .await
                    .map(|_| pid as i64)
                    .map_err(anyhow::Error::from)
                } else {
                    sqlx::query(&crate::pq::rewrite(
                        "INSERT INTO person (name, gender, master_id, generation, status) \
                         VALUES (?, 'unknown', ?, ?, 'active') RETURNING id",
                    ))
                    .bind(&p.name)
                    .bind(mid)
                    .bind(gen)
                    .fetch_one(pool)
                    .await
                    .map(|r| r.try_get::<i64, _>(0).map_err(anyhow::Error::from))
                    .map_err(anyhow::Error::from)
                    .flatten()
                };
                match res {
                    Ok(pid) => {
                        if me_known {
                            updated += 1;
                        } else {
                            created += 1;
                        }
                        gen_of.insert(pid, gen);
                        master_of.insert(pid, Some(mid));
                        id_by_name.insert(p.name.clone(), pid);
                    }
                    Err(e) => {
                        failed += 1;
                        item["note"] = json!(format!("写入失败：{e}"));
                    }
                }
            }
            "unchanged" | "skip" => skipped += 1,
            _ => failed += 1,
        }
        report.push(item);
    }

    if updated > 0 || created > 0 {
        crate::events::emit(
            crate::events::SCOPE_ROSTER,
            format!("批量登记师门：挂接 {updated} 人，新建 {created} 人"),
        );
    }
    Ok(Json(json!({
        "ok": true,
        "updated": updated,
        "created": created,
        "skipped": skipped,
        "failed": failed,
        "report": report,
    })))
}

/// map 插入的小帮手：重名时保留首个（重名行在规划阶段已拦下，不会走到执行）
fn id_of(map: &mut HashMap<String, i64>, name: &str, id: i64) {
    map.entry(name.to_string()).or_insert(id);
}

/// GET /api/admin/streamers/duplicates：同名 / 同抖音号重复检测
pub async fn streamer_duplicates(State(st): State<ArcState>) -> ApiResult {
    require_local(&st)?;
    let names = pgrs::query(&st.pools, "LOCAL", dialect::DUP_NAMES_SQL).await?;
    let douyins = pgrs::query(&st.pools, "LOCAL", dialect::DUP_DOUYIN_SQL).await?;
    let map_names: Vec<Value> = names
        .iter()
        .map(|r| {
            json!({
                "name": read_str(r, "name"),
                "count": read_u64(r, "n"),
                "personIds": read_str(r, "ids"),
            })
        })
        .collect();
    let map_douyin: Vec<Value> = douyins
        .iter()
        .map(|r| {
            json!({
                "douyinNo": read_str(r, "douyin_no"),
                "count": read_u64(r, "n"),
                "personIds": read_str(r, "ids"),
                "names": read_str(r, "names"),
            })
        })
        .collect();
    Ok(Json(
        json!({ "sameName": map_names, "sameDouyinNo": map_douyin }),
    ))
}

/// POST /api/admin/streamers/batch-delete {ids: [..]}：批量软删
#[derive(Deserialize)]
pub struct BatchDeleteBody {
    pub ids: Option<Vec<u64>>,
}

pub async fn batch_delete(
    State(st): State<ArcState>,
    Json(body): Json<BatchDeleteBody>,
) -> ApiResult {
    require_local(&st)?;
    let pool = writer_pool(&st)?;
    let ids = body.ids.ok_or_else(|| anyhow::anyhow!("缺少 ids"))?;
    if ids.is_empty() || ids.len() > 100 {
        return Err(anyhow::anyhow!("ids 为空或超过 100 个").into());
    }
    let mut tx = pool.begin().await?;
    let mut ok = 0u64;
    for id in &ids {
        let r = sqlx::query(&crate::pq::rewrite(
            "UPDATE person SET deleted_at = now() WHERE id = ? AND deleted_at IS NULL",
        ))
        .bind(*id as i64)
        .execute(&mut *tx)
        .await?;
        ok += r.affected();
    }
    tx.commit().await?;
    Ok(Json(json!({ "ok": true, "deleted": ok })))
}

/// POST /api/admin/streamers/{id}/merge {sourceId}：把 source 并入 target
/// ——账号改绑、日指标按日期求和并入（同日冲突求和）、别名留在 aliases 表、source 软删
#[derive(Deserialize)]
pub struct MergeBody {
    #[serde(rename = "sourceId")]
    pub source_id: Option<u64>,
}

pub async fn merge_person(
    State(st): State<ArcState>,
    Path(id): Path<u64>,
    Json(body): Json<MergeBody>,
) -> ApiResult {
    require_local(&st)?;
    let pool = writer_pool(&st)?;
    let source = body
        .source_id
        .ok_or_else(|| anyhow::anyhow!("缺少 sourceId"))?;
    if id == 0 || source == 0 || source == id {
        return Err(anyhow::anyhow!("无效的合并 id（不能和自己合并）").into());
    }

    let mut tx = pool.begin().await?;

    // 账号：先解 anchor_id 唯一冲突——target 已有同 anchor 的行保持不动，其余改绑
    let src_accounts = sqlx::query(&crate::pq::rewrite(
        "SELECT id, anchor_id FROM account WHERE person_id = ?",
    ))
    .bind(source as i64)
    .fetch_all(&mut *tx)
    .await?;
    for a in &src_accounts {
        let anchor: String = a.try_get("anchor_id").unwrap_or_default();
        let exists: i64 = sqlx::query(&crate::pq::rewrite(
            "SELECT COUNT(*) FROM account WHERE anchor_id = ? AND person_id = ?",
        ))
        .bind(&anchor)
        .bind(id as i64)
        .fetch_one(&mut *tx)
        .await?
        .try_get(0)
        .unwrap_or(0);
        if exists == 0 {
            sqlx::query(&crate::pq::rewrite(
                "UPDATE account SET person_id = ? WHERE id = ?",
            ))
            .bind(id as i64)
            .bind(a.try_get::<i64, _>("id").unwrap_or(0))
            .execute(&mut *tx)
            .await?;
        }
    }

    /* 日指标（P1-9 修，2026-10-10）：不能「先并同日冲突、再整批改绑」——target 该日
    已有行时，把 source 行一并 UPDATE person_id 过去会撞 uk_person_date（person_id, biz_date
    唯一键），整个合并 500。改成对 source 的每一天先「合并进 target 再删源行」：
    - target 该日已有行 → source 该日合计并进 target（音浪求和、时长求和、开播取或、
      累计取最大），然后删掉 source 该日行
    - target 该日没有行 → 走后面的整批 UPDATE 改绑（不会撞唯一键）
    两人任一天都有数据（一人多号最常见场景）也能正常合并。 */
    let src_dates = sqlx::query(&crate::pq::rewrite(
        "SELECT DISTINCT TO_CHAR(biz_date, 'YYYY-MM-DD') AS d
         FROM daily_metric WHERE person_id = ?",
    ))
    .bind(source as i64)
    .fetch_all(&mut *tx)
    .await?;
    for r in &src_dates {
        let d: String = r.try_get("d").unwrap_or_default();
        let exists: i64 = sqlx::query(&crate::pq::rewrite(
            "SELECT COUNT(*) FROM daily_metric WHERE person_id = ? AND TO_CHAR(biz_date, 'YYYY-MM-DD') = ?",
        ))
        .bind(id as i64)
        .bind(&d)
        .fetch_one(&mut *tx)
        .await?
        .try_get(0)
        .unwrap_or(0);
        if exists > 0 {
            // 同日冲突：source 该日合计并进 target 该日行，然后删源行（不再撞唯一键）
            // PG 方言：UPDATE … FROM（MySQL 的 UPDATE…JOIN 在 PG 里语法不通）
            sqlx::query(&crate::pq::rewrite(
                "UPDATE daily_metric t
                 SET wave = t.wave + s.w, minutes = t.minutes + s.m,
                     is_live = GREATEST(t.is_live, s.live),
                     cumulative_wave = GREATEST(t.cumulative_wave, s.cum)
                 FROM (SELECT SUM(wave) AS w, SUM(minutes) AS m, MAX(is_live) AS live,
                              MAX(cumulative_wave) AS cum
                       FROM daily_metric WHERE person_id = ? AND TO_CHAR(biz_date, 'YYYY-MM-DD') = ?) s
                 WHERE t.person_id = ? AND TO_CHAR(t.biz_date, 'YYYY-MM-DD') = ?",
            ))
            .bind(source as i64)
            .bind(&d)
            .bind(id as i64)
            .bind(&d)
            .execute(&mut *tx)
            .await?;
            sqlx::query(&crate::pq::rewrite(
                "DELETE FROM daily_metric
                 WHERE person_id = ? AND TO_CHAR(biz_date, 'YYYY-MM-DD') = ?",
            ))
            .bind(source as i64)
            .bind(&d)
            .execute(&mut *tx)
            .await?;
        }
    }
    // 剩下的都是 target 没有同日数据的行，整批改绑不会撞 uk_person_date
    sqlx::query(&crate::pq::rewrite(
        "UPDATE daily_metric SET person_id = ? WHERE person_id = ?",
    ))
    .bind(id as i64)
    .bind(source as i64)
    .execute(&mut *tx)
    .await?;

    // source 软删
    sqlx::query(&crate::pq::rewrite(
        "UPDATE person SET deleted_at = now() WHERE id = ? AND deleted_at IS NULL",
    ))
    .bind(source as i64)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(
        json!({ "ok": true, "mergedFrom": source, "into": id }),
    ))
}

/// GET /api/admin/collect/status：采集链路状态
/// —— 数据新鲜度、每团最新日缺数人数、跨天漏采计数（wave_span > 1）。
/// DouyinLang 推 API 对接评估中（docs/00 阶段 E），当前数据入口为导入流程。
pub async fn collect_status(State(st): State<ArcState>) -> ApiResult {
    require_local(&st)?;
    let dates = crate::repos::get_available_dates(&st, Some("LOCAL"), false).await?;
    let latest = dates.first().cloned().unwrap_or_default();
    let mut teams = serde_json::Map::new();
    for team in ["male", "female"] {
        let sql = format!(
            "SELECT COUNT(*) AS n FROM person p WHERE p.deleted_at IS NULL AND p.gender = '{team}'"
        );
        let total = pgrs::query(&st.pools, "LOCAL", &sql)
            .await?
            .into_iter()
            .next()
            .and_then(|r| r.try_get::<i64, _>(0).ok())
            .unwrap_or(0);
        let missing_sql = format!(
            "SELECT COUNT(*) AS n FROM person p \
             LEFT JOIN daily_metric d ON d.person_id = p.id \
               AND TO_CHAR(d.biz_date, 'YYYY-MM-DD') = '{latest}' \
             WHERE p.deleted_at IS NULL AND p.gender = '{team}' AND d.id IS NULL"
        );
        let missing = if latest.is_empty() {
            total
        } else {
            pgrs::query(&st.pools, "LOCAL", &missing_sql)
                .await?
                .into_iter()
                .next()
                .and_then(|r| r.try_get::<i64, _>(0).ok())
                .unwrap_or(0)
        };
        teams.insert(
            team.to_string(),
            json!({ "total": total, "missingLatest": missing }),
        );
    }
    let gap_sql = "SELECT COUNT(*) AS n FROM daily_metric WHERE wave_span > 1";
    let gaps = pgrs::query(&st.pools, "LOCAL", gap_sql)
        .await?
        .into_iter()
        .next()
        .and_then(|r| r.try_get::<i64, _>(0).ok())
        .unwrap_or(0);
    Ok(Json(json!({
        "latestDate": latest,
        "availableDates": dates.len(),
        "teams": teams,
        "spanGaps": gaps,
        "collector": "manual-import（DouyinLang 推 API 对接评估中）",
    })))
}

/// POST /api/admin/import/upload：CSV/文本文件上传，解析后返回行预览
/// （前端确认后走 /api/admin/import/commit 的 rows 提交）
pub async fn import_upload(
    State(st): State<ArcState>,
    mut multipart: axum::extract::Multipart,
) -> ApiResult {
    require_local(&st)?;
    let mut filename = String::from("上传文件");
    let mut text = String::new();
    while let Some(field) = multipart
        .next_field()
        .await
        // 2026-10-12 审计 L3：全局 DefaultBodyLimit(4MB) 的超限错误在字段读取
        // 时爆出来，axum 的 MultipartError 已经把 body-limit（LengthLimitError）
        // 分类为 413，这里按状态码透传，别让它掉进兜底 500。
        .map_err(multipart_error_to_api)?
    {
        match field.name() {
            Some("filename") => {
                if let Ok(s) = field.text().await {
                    filename = s;
                }
            }
            Some("file") => {
                let fname = field.file_name().unwrap_or("上传文件").to_string();
                ensure(allowed_upload_ext(&fname), format_args!("不支持的文件类型：{fname}，请上传 csv 或 Excel 文件"))?;
                filename = fname;
                let bytes = field.bytes().await.map_err(multipart_error_to_api)?;
                text = match maybe_spreadsheet_text(&filename, &bytes)? {
                    Some(text) => text,
                    None => String::from_utf8_lossy(&bytes).into_owned(),
                };
            }
            _ => {
                let _ = field.text().await;
            }
        }
    }
    ensure(
        !text.trim().is_empty(),
        "文件内容为空或不是文本（请上传 CSV / TSV / 粘贴文本）",
    )?;
    let rows = parse_any_text(&text);
    // 月度时长文件（「9月份时长.csv」这类）单独解析一份：只认时长列，
    // 前端切到「按月」时直接拿这份预览 / 提交，不用回传原始文本。
    let month_rows = parse_month_duration_text(&text);
    ensure(
        !rows.is_empty() || !month_rows.is_empty(),
        "文件里没有解析到任何数据行",
    )?;
    let suggest = month_suggest(&filename, &month_rows, &rows);
    Ok(Json(json!({
        "filename": filename,
        "total": rows.len(),
        "rows": rows,
        "monthRows": month_rows,
        "suggest": suggest,
    })))
}

/// 附件是不是「月度时长文件」：有「时长」列、但没有音浪数值列。
/// 用户口径 2026-10-10：每日不能导入时长，只有月时长 —— 所以「有时长、没音浪」= 平台月报文件。
/// 管理端上传后的模式建议（month_suggest）与机器人附件分流都用这一条判据。
pub fn is_month_duration_file(month_rows: &[ImportRow], rows: &[ImportRow]) -> bool {
    !month_rows.is_empty()
        && !rows
            .iter()
            .any(|r| r.wave.is_some() || r.cumulative_wave.is_some())
}

/// 上传后给前端的模式建议：命中「月度时长文件」特征就预选「按月」，运营在弹窗里仍可改。
///
/// 判据见 [`is_month_duration_file`]：文件里有时长列（时长 / 开播有效时长 …），
/// 且**没有音浪数值列**。年 / 月先按文件名猜（「9月份时长.csv」→ 当年 9 月），
/// 猜不到就用当前月，弹窗里能改。
fn month_suggest(filename: &str, month_rows: &[ImportRow], rows: &[ImportRow]) -> Value {
    if !is_month_duration_file(month_rows, rows) {
        return json!({ "kind": "daily", "year": Value::Null, "month": Value::Null });
    }
    let from_name = crate::csvparse::month_from_filename(filename);
    let (year, month) = from_name.unwrap_or_else(|| {
        let now = chrono::Local::now();
        (
            now.format("%Y").to_string().parse().unwrap_or(2026),
            now.format("%m").to_string().parse().unwrap_or(1),
        )
    });
    json!({
        "kind": "month-duration",
        "year": year,
        "month": month,
        "fromFilename": from_name.is_some(),
        "reason": "文件里只有「时长」列、没有音浪列 → 按月度时长导入（时长只按月导入）",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /* ---------------- 全员同值文件防御（2026-10-13 QQ 机器人导入口径） ---------------- */

    fn mk_import_row(w: Option<f64>) -> ImportRow {
        ImportRow {
            name: "测试".into(),
            douyin_no: None,
            anchor_id: Some("a".into()),
            wave: w,
            cumulative_wave: None,
            minutes: None,
            is_live: Some(false),
            err: None,
        }
    }

    #[test]
    fn all_same_wave_rejected() {
        let rows = vec![
            mk_import_row(Some(100.0)),
            mk_import_row(Some(100.0)),
            mk_import_row(Some(100.0)),
        ];
        assert!(ensure_not_all_same_wave(&rows).is_err(), "全员同值应拒绝");
        let rows = vec![
            mk_import_row(Some(100.0)),
            mk_import_row(Some(200.0)),
            mk_import_row(Some(100.0)),
        ];
        assert!(ensure_not_all_same_wave(&rows).is_ok(), "值不同应放行");
        let rows = vec![mk_import_row(Some(100.0)), mk_import_row(Some(100.0))];
        assert!(ensure_not_all_same_wave(&rows).is_ok(), "不足 3 行放行");
        let rows = vec![
            mk_import_row(Some(100.0)),
            mk_import_row(None),
            mk_import_row(None),
            mk_import_row(None),
        ];
        assert!(ensure_not_all_same_wave(&rows).is_ok(), "缺失值不参与判定");
    }

    /* ---------------- 上传扩展名白名单（2026-10-12 审计 L3） ---------------- */

    #[test]
    fn upload_allowlist_accepts_supported_formats() {
        for name in [
            "9月份时长.csv",
            "2026-10-07-男团.CSV",
            "data.tsv",
            "notes.txt",
            "月报.xlsx",
            "月报.XLSX",
            "老格式.xls",
            "带宏.xlsm",
            "no_dir/x.csv",
        ] {
            assert!(allowed_upload_ext(name), "{name} 应该放行");
        }
    }

    #[test]
    fn upload_allowlist_rejects_others() {
        for name in [
            "payload.exe",
            "script.sh",
            "doc.pdf",
            "archive.zip",
            "noext",
            ".csv", // 纯扩展名式隐藏文件：Path::extension 为 None → 拒
            "data.csv.exe",
        ] {
            assert!(!allowed_upload_ext(name), "{name} 应该拒收");
        }
    }

    /* ---------------- wxbot 归属判定（2026-10-12） ---------------- */

    /// 造一行 wx_push_accounts（只填判定用得到的列）
    fn wbot(
        person_id: u64,
        name: &str,
        nickname: &str,
        conv: &str,
        bound_at: &str,
    ) -> WxPushAccount {
        WxPushAccount {
            name: name.into(),
            person_id,
            nickname: nickname.into(),
            conversation_id: conv.into(),
            bound_at: bound_at.into(),
            ..Default::default()
        }
    }

    #[test]
    fn pick_wxbot_person_id_wins_over_name_fallback() {
        // 本人有带 person_id 的行；另有一行 person_id=0 同名的旧数据（更晚 bound_at）
        let bots = vec![
            wbot(7, "阿猫", "新猫", "c-1", "2026-10-11 09:00:00"),
            wbot(0, "阿猫", "旧猫", "", "2026-10-12 09:00:00"),
        ];
        let got = pick_wxbot(&bots, 7, "阿猫").expect("person_id 精确命中优先");
        assert_eq!(got.nickname, "新猫");
    }

    #[test]
    fn pick_wxbot_takes_latest_bound_at_among_person_id_rows() {
        let bots = vec![
            wbot(7, "阿猫", "早", "c-0", "2026-10-11 09:00:00"),
            wbot(7, "阿猫", "晚", "c-1", "2026-10-12 09:00:00"),
            wbot(7, "阿猫", "更早", "c-2", "2026-10-10 09:00:00"),
        ];
        let got = pick_wxbot(&bots, 7, "阿猫").expect("该人有行");
        assert_eq!(got.nickname, "晚");
    }

    #[test]
    fn pick_wxbot_empty_bound_at_sorts_last() {
        let bots = vec![
            wbot(7, "阿猫", "空", "c-1", ""),
            wbot(7, "阿猫", "有", "c-2", "2026-10-11 09:00:00"),
        ];
        let got = pick_wxbot(&bots, 7, "阿猫").expect("该人有行");
        assert_eq!(got.nickname, "有", "bound_at 为空的行要排最后");
    }

    #[test]
    fn pick_wxbot_name_fallback_only_for_person_id_zero() {
        let bots = vec![
            // 别人的行，名字恰好也叫阿猫（person_id 有效，不该被名字兜底抢来）
            wbot(9, "阿猫", "别人", "c-9", "2026-10-12 09:00:00"),
            // 015 之前的老数据：person_id=0 哨兵，只能按 name 兜底
            wbot(0, "阿猫", "老猫", "", "2026-10-01 09:00:00"),
        ];
        let got = pick_wxbot(&bots, 7, "阿猫").expect("person_id=0 的行按 name 兜底");
        assert_eq!(got.nickname, "老猫");
        // 同一个人站在别人视角看：person_id 精确命中自己的行
        assert_eq!(pick_wxbot(&bots, 9, "别的名字").unwrap().nickname, "别人");
    }

    #[test]
    fn pick_wxbot_none_when_no_match() {
        let bots = vec![wbot(0, "别人", "", "", "2026-10-11 09:00:00")];
        assert!(pick_wxbot(&bots, 7, "阿猫").is_none(), "对不上就是没扫过码");
        assert!(pick_wxbot(&[], 7, "阿猫").is_none());
    }

    /* ---------------- 一人多号音浪合并（2026-10-10） ---------------- */

    fn row(
        pid: Option<u64>,
        wave: Option<f64>,
        cum: Option<f64>,
        minutes: Option<f64>,
    ) -> ResolvedRow {
        ResolvedRow {
            raw: ImportRow {
                name: format!("号-{:?}", wave),
                douyin_no: None,
                anchor_id: None,
                wave,
                cumulative_wave: cum,
                minutes,
                is_live: Some(true),
                err: None,
            },
            person_id: pid,
            name: "某人".into(),
            matched: pid.is_some(),
            team_conflict: None,
            candidates: Vec::new(),
            matched_by_douyin: true,
        }
    }

    /* ---------------- 审计文本限长（2026-10-10 覆盖导入 1406） ---------------- */

    #[test]
    fn truncate_chars_is_cjk_safe() {
        assert_eq!(truncate_chars("玖安🎹", 10), "玖安🎹");
        // 「玖安🎹」按字符算是 3 个（emoji 算一个）→ max=3 不截断，要验截断得给更长的串
        assert_eq!(truncate_chars("玖安🎹", 3), "玖安🎹");
        // 4 个字截到 3：留 max-1 个字 + 省略号，结果字符数绝不超过 max
        assert_eq!(truncate_chars("玖安🎹🎤", 3), "玖安…");
        assert_eq!(truncate_chars("玖安🎹", 0), "");
        // 截断后按字符数算，绝不超过上限
        let long = "狼驰🎹🎤【浩艺】".repeat(50);
        assert!(truncate_chars(&long, 64).chars().count() <= 64);
    }

    #[test]
    fn failures_summary_never_overflows_note_column() {
        // 几百行未匹配时，摘要必须被截到 note VARCHAR(512) 以内（否则 1406 把整批导入打回）
        let failures: Vec<Value> = (0..400)
            .map(|i| json!({ "name": format!("主播{i}🎹🎤【浩艺】"), "reason": "未匹配到主播" }))
            .collect();
        let s = failures_summary(&failures).expect("有失败就该有摘要");
        assert!(
            s.chars().count() <= NOTE_MAX_CHARS,
            "摘要 {} 字，超过上限 {NOTE_MAX_CHARS}",
            s.chars().count()
        );
        assert!(s.contains("等 400 行"), "超出的名字要收口：{s}");
    }

    #[test]
    fn batch_note_carries_pending_and_skipped_within_limit() {
        let failures: Vec<Value> = (0..200)
            .map(|i| json!({ "name": format!("未匹配{i}"), "reason": "x" }))
            .collect();
        let skipped: Vec<String> = (0..50).map(|i| format!("跳过{i}")).collect();
        let note = batch_note(&failures, 37, &skipped).unwrap();
        assert!(note.chars().count() <= NOTE_MAX_CHARS);
        assert!(note.contains("暂存 37 行"));
        assert!(note.contains("跳过 50 行"));
        // 没有失败/暂存/跳过时不该写备注
        assert!(batch_note(&[], 0, &[]).is_none());
    }

    /* ---------------- 行分类：暂存 / 跳过（2026-10-10 用户口径） ---------------- */

    fn raw_row(name: &str, douyin_no: Option<&str>, wave: Option<f64>) -> ImportRow {
        ImportRow {
            name: name.into(),
            douyin_no: douyin_no.map(str::to_string),
            anchor_id: None,
            wave,
            ..Default::default()
        }
    }

    fn resolved(raw: ImportRow, pid: Option<u64>, conflict: Option<&str>) -> ResolvedRow {
        ResolvedRow {
            name: raw.name.clone(),
            raw,
            person_id: pid,
            matched: pid.is_some(),
            team_conflict: conflict.map(str::to_string),
            candidates: Vec::new(),
            matched_by_douyin: false,
        }
    }

    #[test]
    fn unmatched_row_with_id_goes_to_pending() {
        // 没建档但有抖音号 → 暂存（不进统计），等建档/绑号后补账
        let r = resolved(raw_row("玖俏", Some("123456"), Some(100.0)), None, None);
        assert_eq!(classify_row(&r), RowClass::Pending);
        // 只给采集 ID 也一样认
        let mut raw = raw_row("啸泉", None, Some(50.0));
        raw.anchor_id = Some("7543117871536407610".into());
        assert_eq!(classify_row(&resolved(raw, None, None)), RowClass::Pending);
        // 什么 ID 都没有 → 失败
        let r = resolved(raw_row("：：", None, Some(1.0)), None, None);
        assert_eq!(classify_row(&r), RowClass::Unmatched);
    }

    #[test]
    fn matched_row_without_values_is_skipped_not_failed() {
        // 玖安🎹 这种：人已建档、文件里一行数值都没给 → 跳过，交给缺数据报告
        let r = resolved(raw_row("玖安🎹", Some("123456"), None), Some(1036), None);
        assert_eq!(classify_row(&r), RowClass::Skipped);
        // 有任意一个数值就是正常行
        let r = resolved(
            raw_row("玖安🎹", Some("123456"), Some(0.0)),
            Some(1036),
            None,
        );
        assert_eq!(classify_row(&r), RowClass::Ok);
        // 时长-only 行也算有数据（2026-10-10 修了 write_daily_row 里挡路的分支）
        let mut raw = raw_row("玖安🎹", Some("123456"), None);
        raw.minutes = Some(120.0);
        assert_eq!(classify_row(&resolved(raw, Some(1036), None)), RowClass::Ok);
    }

    #[test]
    fn team_conflict_still_fails() {
        let r = resolved(
            raw_row("玖安", Some("123456"), Some(1.0)),
            Some(1036),
            Some("主播「玖安」团别是 male，与导入团别 female 不符"),
        );
        assert!(matches!(classify_row(&r), RowClass::TeamConflict(_)));
    }

    #[test]
    fn row_ident_prefers_douyin_no_then_anchor_id() {
        let mut raw = raw_row("某人", Some(" dy-01 "), Some(1.0));
        raw.anchor_id = Some("anchor-9".into());
        assert_eq!(row_ident(&raw).as_deref(), Some("dy-01"));
        raw.douyin_no = None;
        assert_eq!(row_ident(&raw).as_deref(), Some("anchor-9"));
        raw.anchor_id = None;
        assert!(row_ident(&raw).is_none());
    }

    /* ---------------- 月度时长导入（2026-10-10：时长只按月导入） ---------------- */

    /// 抖音公会后台「9月份时长.csv」的真实表头（只截了用得上的列）。
    const MONTH_CSV: &str =
        "主播ID,主播昵称,抖音号,粉丝数,签约类型,分成比-直播音浪收益,开播有效时长,主播标签\n\
         2436948398193464,浩龙🎹,cyl765,19526,金牌主播,35,75小时29分钟41秒,\n\
         7616689691311686705,玖玉🎹🎤,37181712726,4702,普通主播,35,120小时45分钟1秒,\n";

    #[test]
    fn month_duration_reads_only_duration_column() {
        let rows = parse_month_duration_text(MONTH_CSV);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "浩龙🎹");
        assert_eq!(rows[0].minutes, Some((75 * 60 + 29) as f64));
        // 「分成比-直播音浪收益」= 35 绝不能被读成音浪
        assert_eq!(rows[0].wave, None);
        assert_eq!(rows[1].minutes, Some((120 * 60 + 45) as f64));
        assert_eq!(rows[0].douyin_no.as_deref(), Some("cyl765"));
        assert_eq!(rows[0].anchor_id.as_deref(), Some("2436948398193464"));
        // 没有时长列的文件解析不出行：前端据此提示「换个文件 / 切回按日」
        assert!(parse_month_duration_text("主播昵称,音浪\n浩龙,1000\n").is_empty());
    }

    /// 二进制护栏：xlsx 转文本漏判时，ZIP 流会被当成名字塞进暂存区，
    /// 后台显示成一屏乱码、每个乱码还变成一个「待建档主播」。这里守住两件事：
    /// 二进制一律解析不出，正常中文/emoji 一律不被误伤。
    #[test]
    fn binary_content_is_rejected_not_imported() {
        // 真实 xlsx 的 ZIP 头 + 后续压缩流
        let zip =
            "\u{50}\u{4b}\u{3}\u{4}\u{14}\u{0}\u{2}\u{8}\u{8}\u{0}\u{75}\u{0}\u{0}\u{0}\u{0}\u{0}";
        assert!(looks_like_binary(zip), "xlsx 二进制必须被识别");
        assert!(parse_any_text(zip).is_empty(), "二进制绝不能解析出待导入行");
        assert!(
            parse_month_duration_text(zip).is_empty(),
            "月度入口同样拒绝二进制"
        );
    }

    #[test]
    fn normal_text_is_not_flagged_as_binary() {
        // 中文 + emoji + 括号 + 下划线：都不能被控制字符判据误伤
        for s in [
            "浩龙🎹,55555\n南方楠,66666\n",
            "（解绑留档）__e2e合并_no_unbind 副号快照 2 条",
            "主播昵称,开播有效时长\n玖玉🎹,75小时29分钟41秒\n",
            "名字,当日音浪,累计,开播\n张三,1000,2000,开播\n",
        ] {
            assert!(!looks_like_binary(s), "正常文本不该被判为二进制：{s}");
            assert!(!parse_any_text(s).is_empty(), "正常文本要能解析出行：{s}");
        }
    }

    #[test]
    fn strip_minutes_drops_daily_duration() {
        // 用户口径 2026-10-10：每日不能导入时长 —— 按日导入的行一律抹掉 minutes
        let mut rows = vec![
            ImportRow {
                name: "浩龙".into(),
                wave: Some(1000.0),
                minutes: Some(120.0),
                ..Default::default()
            },
            ImportRow {
                name: "玖玉".into(),
                wave: Some(2000.0),
                ..Default::default()
            },
            ImportRow {
                name: "只有时长".into(),
                minutes: Some(60.0),
                ..Default::default()
            },
        ];
        assert_eq!(strip_minutes(&mut rows), 2);
        assert!(rows.iter().all(|r| r.minutes.is_none()));
        // 只剩「只有时长」的行会被抹成没有任何数值 → 分类为跳过（不写库，也不算失败）
        assert_eq!(
            classify_row(&resolved(rows[2].clone(), Some(7), None)),
            RowClass::Skipped
        );
    }

    #[test]
    fn month_file_detection_requires_duration_without_wave() {
        // 判据：有时长列、没有音浪列 → 月度时长文件（管理端预选「按月」+ 机器人附件分流共用）
        let month_rows = parse_month_duration_text(MONTH_CSV);
        let file_rows = parse_any_text(MONTH_CSV);
        assert!(is_month_duration_file(&month_rows, &file_rows));
        // 文件里出现音浪数值 → 不是月度文件（按日文件带时长列也不切模式）
        let mut with_wave = file_rows.clone();
        with_wave.push(ImportRow {
            name: "浩龙".into(),
            wave: Some(1.0),
            ..Default::default()
        });
        assert!(!is_month_duration_file(&month_rows, &with_wave));
        // 没有时长列 → 不是
        assert!(!is_month_duration_file(&[], &file_rows));
    }

    #[test]
    fn month_suggest_switches_only_for_duration_only_files() {
        let month_rows = parse_month_duration_text(MONTH_CSV);
        // 同一份月度文件走「按日解析」：解析器不该从「分成比-直播音浪收益」里读出音浪
        let file_rows = parse_any_text(MONTH_CSV);
        assert!(file_rows.iter().all(|r| r.wave.is_none()));
        let now_year: i32 = chrono::Local::now()
            .format("%Y")
            .to_string()
            .parse()
            .unwrap();
        // 只有时长列 + 文件名带月份 → 按月，月份取自文件名
        let s = month_suggest("9月份时长.csv", &month_rows, &file_rows);
        assert_eq!(s["kind"], "month-duration");
        assert_eq!(s["month"], 9);
        assert_eq!(s["year"], now_year);
        assert_eq!(s["fromFilename"], true);
        // 按日文件（有音浪、没时长）→ 按日
        let daily_rows = parse_any_text("名字,当日音浪\n浩龙,1000\n");
        let s = month_suggest("2026-10-07-男团.csv", &[], &daily_rows);
        assert_eq!(s["kind"], "daily");
        // 同时有时长和音浪（罕见）→ 不自动切，交给运营在弹窗里选
        let mut mixed = file_rows.clone();
        mixed.push(ImportRow {
            name: "浩龙".into(),
            wave: Some(1000.0),
            minutes: Some(10.0),
            ..Default::default()
        });
        assert_eq!(
            month_suggest("9月份时长.csv", &month_rows, &mixed)["kind"],
            "daily"
        );
    }

    /// 台账账号可能只有采集 ID（没有抖音号）：文件里两列都给时，抖音号没命中要用采集 ID 兜住。
    #[test]
    fn resolve_row_tries_douyin_then_anchor_id() {
        let ctx = MatchCtx {
            streamers: vec![LocalStreamer {
                id: 7,
                name: "狼澈".into(),
                gender: "male".into(),
            }],
            by_douyin: HashMap::new(),
            by_anchor: HashMap::from([("2262157006222251".to_string(), 7u64)]),
        };
        let mut raw = raw_row("狼澈🎤🎹", Some("63665387875"), Some(1.0));
        raw.anchor_id = Some("2262157006222251".into());
        let r = resolve_row(&raw, &ctx, &HashMap::new(), None);
        assert_eq!(r.person_id, Some(7), "抖音号没命中时要用采集 ID 兜住");
        assert!(r.matched_by_douyin);

        // 抖音号命中优先（同一个 id，两个列都能用）
        let ctx2 = MatchCtx {
            streamers: vec![LocalStreamer {
                id: 9,
                name: "浩龙".into(),
                gender: "male".into(),
            }],
            by_douyin: HashMap::from([("cyl765".to_string(), 9u64)]),
            by_anchor: HashMap::new(),
        };
        let mut raw = raw_row("浩龙🎹", Some("cyl765"), Some(1.0));
        raw.anchor_id = Some("2436948398193464".into());
        let r = resolve_row(&raw, &ctx2, &HashMap::new(), None);
        assert_eq!(r.person_id, Some(9));
        assert!(r.matched_by_douyin);

        // 两个号码都没有 → 仍按名字归一化命中（去空格）
        let raw = raw_row("狼 澈", None, Some(1.0));
        let r = resolve_row(&raw, &ctx, &HashMap::new(), None);
        assert_eq!(r.person_id, Some(7));
        assert!(!r.matched_by_douyin, "按名字命中不算号码命中");
    }

    /* ---------------------- 批量开号入参（016，docs/13） ---------------------- */

    /// 前端传的 items 里可能混着 personId=0 / 缺失 / 空手机号，解析层要全部收敛掉，
    /// 不能让它们变成一条 `INSERT ... person_id = 0`（那是"管理员观看账号"的哨兵值）。
    #[test]
    fn batch_items_drop_invalid_person_ids() {
        let body: BatchAccountsBody = serde_json::from_str(
            r#"{"items":[{"personId":1027,"phone":"13354463148"},
                          {"personId":0,"phone":"13900000000"},
                          {"phone":"13900000001"},
                          {"personId":1028,"phone":"  "}]}"#,
        )
        .expect("解析批量开号入参");
        let inputs: Vec<crate::store::BatchAccountInput> = body
            .items
            .unwrap_or_default()
            .iter()
            .filter_map(|i| {
                let pid = i.person_id?;
                (pid > 0).then(|| crate::store::BatchAccountInput {
                    person_id: pid,
                    phone: i
                        .phone
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                })
            })
            .collect();
        assert_eq!(inputs.len(), 2, "personId=0 / 缺失的条目必须被丢弃");
        assert_eq!(inputs[0].person_id, 1027);
        assert_eq!(inputs[0].phone.as_deref(), Some("13354463148"));
        assert_eq!(
            inputs[1].phone, None,
            "纯空格手机号 == 没填（走「沿用台账手机号」分支）"
        );
    }

    /// forceReset 默认 false：重跑一遍批量不能把 104 个人的密码全刷掉
    #[test]
    fn batch_force_reset_defaults_to_false() {
        let body: BatchAccountsBody =
            serde_json::from_str(r#"{"items":[{"personId":1}]}"#).unwrap();
        assert!(!body.force_reset.unwrap_or(false));
        assert!(!body.dry_run.unwrap_or(false));
        assert!(body.default_pass.is_none());
        let forced: BatchAccountsBody = serde_json::from_str(
            r#"{"items":[{"personId":1}],"forceReset":true,"defaultPass":"6688"}"#,
        )
        .unwrap();
        assert!(forced.force_reset.unwrap_or(false));
        assert_eq!(forced.default_pass.as_deref(), Some("6688"));
    }

    #[test]
    fn valid_period_accepts_only_yyyy_mm() {
        assert!(valid_period("2026-09").is_ok());
        assert!(valid_period("2026-12").is_ok());
        assert!(valid_period("2026-13").is_err(), "13 月不存在");
        assert!(valid_period("2026-00").is_err());
        assert!(valid_period("2026-9").is_err(), "必须补零");
        assert!(valid_period("202609").is_err());
        assert!(valid_period("2026-09-01").is_err());
        assert_eq!(period_first_day("2026-09"), "2026-09-01");
    }

    #[test]
    fn merges_douyin_accounts_of_one_person() {
        // 一个人三个号：当日音浪求和，累计取最大，时长求和
        let rows = vec![
            row(Some(7), Some(100.0), Some(5000.0), Some(60.0)),
            row(Some(7), Some(200.0), Some(3000.0), Some(30.0)),
            row(Some(8), Some(50.0), None, None),
        ];
        let out = merge_rows_by_person(&rows);
        assert_eq!(out.len(), 2, "同一个人三行要合成一行");
        let a = &out[0];
        assert_eq!(a.person_id, Some(7));
        assert_eq!(a.raw.wave, Some(300.0), "当日音浪求和");
        assert_eq!(
            a.raw.cumulative_wave,
            Some(5000.0),
            "累计取最大（不是求和）"
        );
        assert_eq!(a.raw.minutes, Some(90.0), "时长求和");
        assert_eq!(out[1].person_id, Some(8));
    }

    #[test]
    fn merge_keeps_unmatched_rows_separate() {
        // 匹配失败的行不能合并：运营要在预览里看到到底哪一行没匹配上
        let rows = vec![
            row(None, Some(10.0), None, None),
            row(None, Some(20.0), None, None),
        ];
        let out = merge_rows_by_person(&rows);
        assert_eq!(out.len(), 2, "未匹配的行一条一条报出来");
    }

    #[test]
    fn merge_leaves_single_rows_untouched() {
        let rows = vec![row(Some(7), Some(1.0), Some(2.0), None)];
        let out = merge_rows_by_person(&rows);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].raw.wave, Some(1.0));
        assert_eq!(out[0].raw.minutes, None, "两边都没有时长时不能凭空造 0");
    }

    #[test]
    fn parses_tab_separated_with_header() {
        let text = "名字\t当日音浪\t累计\t开播\n玖玉\t560000\t1700000\t开播\n";
        let rows = parse_import_text(text);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "玖玉");
        assert_eq!(rows[0].wave, Some(560000.0));
        assert_eq!(rows[0].cumulative_wave, Some(1700000.0));
        assert_eq!(rows[0].is_live, Some(true));
    }

    /// 回归：csvparse 优先，但判错类型、一行可用名字都没剩时必须回落裸列序解析。
    /// 「开播」列会让 detect_kind 认为是时长表 → 名字列对不上 → 以前整批丢空，
    /// 导入页表现为「文件里没有解析到任何数据行」，而前端提示写着「开播自动识别」。
    #[test]
    fn parse_any_text_falls_back_when_header_kind_is_wrong() {
        // 表头 + 开播列（前端提示的格式）
        let rows = parse_any_text("名字,当日音浪,累计,开播\n玖玉,560000,1700000,开播\n");
        assert_eq!(rows.len(), 1, "开播列表头不能再丢空");
        assert_eq!(rows[0].name, "玖玉");
        assert_eq!(rows[0].wave, Some(560000.0));
        assert_eq!(rows[0].cumulative_wave, Some(1700000.0));
        assert_eq!(rows[0].is_live, Some(true));

        // 无表头 + 开播列（粘贴/手写文本的老格式）
        let rows = parse_any_text("凤九 500000 1200000 开播\n");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "凤九");
        assert_eq!(rows[0].wave, Some(500000.0));
        assert_eq!(rows[0].is_live, Some(true));
    }

    /// csvparse 认得出的时候仍是它说了算：万 / 时间单位只有它读得懂
    #[test]
    fn parse_any_text_keeps_header_csv_rules() {
        let rows = parse_any_text("主播昵称,音浪,直播时长\n凤九,2.5万,1:30\n");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "凤九");
        assert_eq!(
            rows[0].wave,
            Some(25000.0),
            "「2.5万」必须走 csvparse 的单位换算"
        );
        assert!(rows[0].minutes.is_some(), "直播时长要解析成分钟");
    }

    #[test]
    fn parses_comma_and_chinese_comma() {
        let rows = parse_import_text("浩龙,1610000,3000000,未开播,30\n啸安，800000，1200000\n");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].wave, Some(1610000.0));
        assert_eq!(rows[0].cumulative_wave, Some(3000000.0));
        assert_eq!(rows[0].is_live, Some(false));
        assert_eq!(rows[0].minutes, Some(30.0));
        assert_eq!(rows[1].wave, Some(800000.0));
        assert_eq!(rows[1].cumulative_wave, Some(1200000.0));
    }

    #[test]
    fn non_numeric_tail_becomes_douyin_no() {
        let rows = parse_import_text("阿楠 dy123456 50000");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].douyin_no.as_deref(), Some("dy123456"));
        assert_eq!(rows[0].wave, Some(50000.0));
    }

    #[test]
    fn thousand_separator_parsed() {
        let rows = parse_import_text("南方楠 11,370,000");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].wave, Some(11370000.0));
    }

    #[test]
    fn resolve_marks_team_conflict() {
        let ctx = MatchCtx::from_streamers(vec![LocalStreamer {
            id: 7,
            name: "玖玉".into(),
            gender: "female".into(),
        }]);
        let r = resolve_row(
            &ImportRow {
                name: "玖玉".into(),
                wave: Some(1.0),
                ..Default::default()
            },
            &ctx,
            &HashMap::new(),
            Some("male"),
        );
        assert!(r.matched);
        assert!(r.team_conflict.is_some(), "女团主播按 male 导入要标红");
        let ok = resolve_row(
            &ImportRow {
                name: "玖玉".into(),
                wave: Some(1.0),
                ..Default::default()
            },
            &ctx,
            &HashMap::new(),
            Some("female"),
        );
        assert!(ok.team_conflict.is_none());
    }

    #[test]
    fn resolve_applies_alias() {
        let ctx = MatchCtx::from_streamers(vec![LocalStreamer {
            id: 9,
            name: "南方楠".into(),
            gender: "male".into(),
        }]);
        let mut aliases = HashMap::new();
        aliases.insert("阿楠".to_string(), "南方楠".to_string());
        let r = resolve_row(
            &ImportRow {
                name: "阿楠".into(),
                ..Default::default()
            },
            &ctx,
            &aliases,
            None,
        );
        assert!(r.matched);
        assert_eq!(r.person_id, Some(9));
    }

    #[test]
    fn resolve_by_douyin_no_first() {
        // 一人多账号：文件里任意一个抖音号都归到同一人；抖音号优先于名字
        let ctx = MatchCtx {
            streamers: vec![LocalStreamer {
                id: 86,
                name: "狼佑".into(),
                gender: "male".into(),
            }],
            by_douyin: HashMap::from([
                ("39863161947".to_string(), 86u64),
                ("dy_jiuyu01".to_string(), 86u64),
            ]),
            by_anchor: HashMap::new(),
        };
        // 主号抖音号 + 错误的别名名字 → 仍按抖音号命中
        let r = resolve_row(
            &ImportRow {
                name: "佑佑".into(),
                douyin_no: Some("39863161947".into()),
                wave: Some(1.0),
                ..Default::default()
            },
            &ctx,
            &HashMap::new(),
            None,
        );
        assert!(r.matched);
        assert!(r.matched_by_douyin);
        assert_eq!(r.person_id, Some(86));
        // 第二个账号的号 → 同一人
        let r2 = resolve_row(
            &ImportRow {
                name: "狼佑".into(),
                douyin_no: Some("dy_jiuyu01".into()),
                wave: Some(2.0),
                ..Default::default()
            },
            &ctx,
            &HashMap::new(),
            None,
        );
        assert!(r2.matched && r2.matched_by_douyin);
        assert_eq!(r2.person_id, Some(86));
        // 名字命中但抖音号不在映射里（未建档的号）→ 回落名字匹配，不按抖音号
        let r3 = resolve_row(
            &ImportRow {
                name: "狼佑".into(),
                douyin_no: Some("unknown_no".into()),
                wave: Some(3.0),
                ..Default::default()
            },
            &ctx,
            &HashMap::new(),
            None,
        );
        assert!(r3.matched && !r3.matched_by_douyin);
        assert_eq!(r3.person_id, Some(86));
    }

    #[test]
    fn admin_static_sql_passes_write_guard() {
        // 管理端走写池的静态 SQL 必须全部过写守卫
        let admin_sql = [
            "UPDATE person SET name = ? WHERE id = ?",
            "UPDATE person SET deleted_at = now() WHERE id = ? AND deleted_at IS NULL",
            "INSERT INTO person (name, gender) VALUES (?,?)",
            "INSERT INTO account (person_id, anchor_id) VALUES (?,?)",
            "UPDATE account SET douyin_no = ? WHERE anchor_id = ?",
            "UPDATE import_batch SET ok_rows = ?, failed_rows = ? WHERE id = ?",
            "INSERT INTO import_batch (source, filename) VALUES (?,?)",
            "INSERT INTO daily_metric (person_id) VALUES (?) ON CONFLICT (person_id, biz_date) DO UPDATE SET wave = EXCLUDED.wave",
        ];
        for s in admin_sql {
            writer::write_guard(s).expect(s);
        }
    }

    #[test]
    fn validates_team_and_date() {
        assert!(valid_team("male").is_ok());
        assert!(valid_team("female").is_ok());
        assert!(valid_team("boy").is_err());
        assert!(valid_biz_date("2026-10-04").is_ok());
        assert!(valid_biz_date("10-04").is_err());
    }
}

#[cfg(test)]
mod master_batch_tests {
    use super::{plan_master_batch, MasterGraph, NameCounts};
    use pk_core::roster::{parse_roster, ParseOptions};

    fn plan(
        text: &str,
        graph: &mut MasterGraph,
        counts: &NameCounts,
        create: bool,
    ) -> Vec<super::MasterPlanRow> {
        let parsed = parse_roster(text, &ParseOptions::with_defaults());
        plan_master_batch(&parsed, graph, counts, create)
    }

    fn counts_of(names: &[&str]) -> NameCounts {
        let mut c = NameCounts::new();
        for n in names {
            c.insert(n.to_string(), 1);
        }
        c
    }

    #[test]
    fn attach_known_pair_and_skip_masterless_rows() {
        let mut g = MasterGraph::new();
        g.insert("啸安".into(), "狼兴".into()); // 在册关系：啸安 的师傅已是 狼兴
        let counts = counts_of(&["啸安", "狼兴", "浩龙", "狼博"]);
        let rows = plan("1. 啸安➕狼兴\n2. 狼博", &mut g, &counts, false);
        assert_eq!(rows[0].action, "unchanged", "已是徒弟不应重复写库");
        assert_eq!(rows[1].action, "skip", "没写师傅的行不能动他的现有师傅");
    }

    #[test]
    fn attach_two_known_people() {
        let mut g = MasterGraph::new();
        let counts = counts_of(&["狼博", "浩龙"]);
        let rows = plan("狼博 浩龙", &mut g, &counts, false);
        assert_eq!(rows[0].action, "attach");
        assert_eq!(rows[0].master.as_deref(), Some("浩龙"));
    }

    #[test]
    fn score_rows_are_skipped() {
        // 「名字 分数」是音浪名单格式，不是师门登记
        let mut g = MasterGraph::new();
        let counts = counts_of(&["狼哲"]);
        let rows = plan("1. 狼哲 71259", &mut g, &counts, false);
        assert_eq!(rows[0].action, "skip");
    }

    #[test]
    fn missing_people_error_unless_create_missing() {
        // 两人都不在 name_counts 里 = 不在册
        let mut g = MasterGraph::new();
        let counts = counts_of(&["别的某人"]);
        let rows = plan("新人甲 新人乙", &mut g, &counts, false);
        assert_eq!(rows[0].action, "error");
        let rows = plan("新人甲 新人乙", &mut g, &counts, true);
        assert_eq!(rows[0].action, "create");
    }

    #[test]
    fn duplicate_names_are_rejected_either_side() {
        let mut g = MasterGraph::new();
        let mut counts = counts_of(&["浩龙"]);
        counts.insert("重名".to_string(), 2);
        let rows = plan("重名 浩龙", &mut g, &counts, false);
        assert_eq!(rows[0].action, "error");

        let mut counts2 = counts_of(&["啸安"]);
        counts2.insert("重名".to_string(), 2);
        let rows = plan("啸安 重名", &mut g, &counts2, false);
        assert_eq!(rows[0].action, "error");
    }

    #[test]
    fn cycle_is_blocked_in_store_and_in_batch() {
        // 在册：浩龙→狼征；提交「狼征 浩龙」会把祖师挂到徒孙下面，成环
        let mut g = MasterGraph::new();
        g.insert("浩龙".into(), "狼征".into());
        let counts = counts_of(&["浩龙", "狼征"]);
        let rows = plan("狼征 浩龙", &mut g, &counts, false);
        assert_eq!(rows[0].action, "error");

        // 批内互挂：两行都新建也会成环，第二行必须被拦
        let mut g2 = MasterGraph::new();
        let c2 = NameCounts::new();
        let rows = plan("甲 乙\n乙 甲", &mut g2, &c2, true);
        assert_eq!(rows[0].action, "create");
        assert_eq!(rows[1].action, "error");
    }

    #[test]
    fn self_master_is_swallowed_by_parser() {
        // 「X X」第二个同名 token 在解析层就被丢弃（roster.rs 的去重规则），
        // 到不了规划层 → 表现为「没写师傅」skip，绝不会写库，这正是我们要的安全行为。
        // 真正可达的自指场景是别名归一（如 阿楠→南方楠 且写「阿楠 南方楠」），走 error 分支。
        let mut g = MasterGraph::new();
        let counts = counts_of(&["啸安"]);
        let rows = plan("啸安 啸安", &mut g, &counts, false);
        assert_eq!(rows[0].action, "skip");

        let mut g2 = MasterGraph::new();
        let mut counts2 = counts_of(&["南方楠"]);
        counts2.insert("阿楠".to_string(), 0); // 阿楠 是别名，不在册
        let rows = plan("阿楠 南方楠", &mut g2, &counts2, false);
        assert_eq!(rows[0].action, "error", "别名归一后的自指必须拦下");
    }
}

#[cfg(test)]
mod spreadsheet_tests {
    use super::{parse_any_text, spreadsheet_to_text};

    #[test]
    fn xlsx_keeps_chinese_names() {
        let path = std::env::var("XLSX_TEST_FILE").unwrap_or_default();
        if path.is_empty() || !std::path::Path::new(&path).exists() {
            return;
        }
        let bytes = std::fs::read(path).expect("read xlsx");
        let text = spreadsheet_to_text(&bytes).expect("convert xlsx");
        let rows = parse_any_text(&text);
        eprintln!("converted_rows={}", rows.len());
        for row in rows.iter().take(5) {
            eprintln!("name={:?}, wave={:?}", row.name, row.wave);
        }
        assert!(!rows.is_empty(), "Excel 内容应解析出数据行");
        assert!(
            rows.iter().any(|r| r.wave.is_some()),
            "至少应解析出一个音浪值"
        );
        assert!(
            rows.iter()
                .all(|r| r.name.chars().any(|c| !c.is_ascii_control())),
            "姓名不应是二进制乱码"
        );
    }
}
