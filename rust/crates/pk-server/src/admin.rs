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
use axum::Json;
use chrono::NaiveDate;
use pk_core::roster::{parse_roster, ParseOptions};
use pk_core::types::ParsedRow;
use pk_core::{default_aliases, normalize_name};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{MySqlPool, Row};

use crate::dialect;
use crate::mysql;
use crate::routes::ApiError;
use crate::state::AppState;
use crate::writer;

type ApiResult = Result<Json<Value>, ApiError>;
type ArcState = Arc<AppState>;

/* ------------------------------- 公共小工具 ------------------------------- */

fn read_u64(row: &sqlx::mysql::MySqlRow, col: &str) -> Option<u64> {
    if let Ok(v) = row.try_get::<u64, _>(col) {
        return Some(v);
    }
    if let Ok(v) = row.try_get::<i64, _>(col) {
        return v.try_into().ok();
    }
    None
}

fn read_opt_u64(row: &sqlx::mysql::MySqlRow, col: &str) -> Option<u64> {
    if let Ok(v) = row.try_get::<Option<u64>, _>(col) {
        return v;
    }
    if let Ok(v) = row.try_get::<Option<i64>, _>(col) {
        return v.and_then(|x| x.try_into().ok());
    }
    None
}

fn read_opt_i64(row: &sqlx::mysql::MySqlRow, col: &str) -> Option<i64> {
    // INT UNSIGNED 列用 i64/i32 解码会失败，先试无符号
    if let Ok(v) = row.try_get::<Option<u64>, _>(col) {
        return v.and_then(|x| x.try_into().ok());
    }
    if let Ok(v) = row.try_get::<Option<i64>, _>(col) {
        return v;
    }
    row.try_get::<Option<i32>, _>(col)
        .ok()
        .flatten()
        .map(i64::from)
}

fn read_opt_str(row: &sqlx::mysql::MySqlRow, col: &str) -> Option<String> {
    row.try_get::<Option<String>, _>(col).ok().flatten()
}

fn read_str(row: &sqlx::mysql::MySqlRow, col: &str) -> String {
    row.try_get::<String, _>(col).unwrap_or_default()
}

/// 管理端只对本地库操作；没有 LOCAL 数据源直接报错
fn require_local(st: &AppState) -> Result<(), ApiError> {
    if st.sources.iter().any(|s| s.id == "LOCAL") {
        Ok(())
    } else {
        Err(anyhow::anyhow!("管理端需要 LOCAL 数据源（douyin_writer 对应的本地 MariaDB）").into())
    }
}

fn writer_pool(st: &AppState) -> Result<&MySqlPool, ApiError> {
    st.writer.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "写库不可用：douyin_writer 账号未配置或连不上（检查 DB_WRITER_* 环境变量），只读功能不受影响"
        )
        .into()
    })
}

fn valid_team(team: &str) -> anyhow::Result<()> {
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

fn valid_biz_date(date: &str) -> anyhow::Result<NaiveDate> {
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

/// GET /api/admin/streamers?q=&team=：主播台账（含主号账号 + SQLite 别名合并）
pub async fn streamers(State(st): State<ArcState>, Query(q): Query<StreamerQuery>) -> ApiResult {
    require_local(&st)?;
    let rows = mysql::query(&st.pools, "LOCAL", dialect::STREAMERS_SQL).await?;
    let alias_map = merged_aliases(&st).await;
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
        items.push(json!({
            "personId": read_u64(r, "id").unwrap_or(0),
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
            "anchorName": read_opt_str(r, "anchor_name"),
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
    let dup = mysql::query_with_binds(&st.pools, "LOCAL", dialect::STREAMER_BY_NAME_SQL, |q| {
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

    let mut tx = pool.begin().await?;
    let res = sqlx::query(
        "INSERT INTO person (name, gender, master_id, generation, group_name, status, joined_at)
         VALUES (?,?,?,?,?,?,?)",
    )
    .bind(&name)
    .bind(&team)
    .bind(body.master_id)
    .bind(body.generation.map(|v| v as i32))
    .bind(&body.group_name)
    .bind(body.status.as_deref().unwrap_or("active"))
    .bind(&body.joined_at)
    .execute(&mut *tx)
    .await?;
    let person_id = res.last_insert_id();

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
        sqlx::query(
            "INSERT INTO account (person_id, anchor_id, douyin_no, anchor_name, is_primary)
             VALUES (?,?,?,?,1)",
        )
        .bind(person_id)
        .bind(&anchor_id)
        .bind(&no)
        .bind(&name)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_account_err(e, &anchor_id))?;
    }
    tx.commit().await?;

    Ok(Json(json!({ "ok": true, "personId": person_id })))
}

async fn check_douyin_no_free(
    st: &AppState,
    douyin_no: Option<&str>,
    except_person: Option<u64>,
) -> Result<(), ApiError> {
    let Some(no) = douyin_no else {
        return Ok(());
    };
    let rows = mysql::query_with_binds(&st.pools, "LOCAL", dialect::DOUYIN_NO_TAKEN_SQL, |q| {
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
    if msg.contains("Duplicate entry") && msg.contains(anchor_id) {
        anyhow::anyhow!("采集 ID {anchor_id} 已存在（anchor_id 唯一），请检查是否重复建档")
    } else {
        anyhow::anyhow!("账号写入失败: {e}")
    }
}

/// UPDATE 语句 SET 子句的值绑定（列名白名单，值全部 bind）
enum Bind {
    S(String),
    U(Option<u64>),
    I(Option<i64>),
}

impl Bind {
    fn into_query(
        self,
        query: sqlx::query::Query<'_, sqlx::MySql, sqlx::mysql::MySqlArguments>,
    ) -> sqlx::query::Query<'_, sqlx::MySql, sqlx::mysql::MySqlArguments> {
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
        binds.push(Bind::U((v > 0).then_some(v)));
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
    if !sets.is_empty() {
        let sql = format!("UPDATE person SET {} WHERE id = ?", sets.join(", "));
        writer::write_guard(&sql)?;
        let mut query = sqlx::query(&sql);
        for b in binds {
            query = b.into_query(query);
        }
        query.bind(id as i64).execute(pool).await?;
    }

    // 抖音号：upsert 到主号账号行
    if let Some(no) = douyin_no {
        let anchor: Option<String> = sqlx::query(dialect::PRIMARY_ANCHOR_SQL)
            .bind(id as i64)
            .map(|r: sqlx::mysql::MySqlRow| r.try_get::<String, _>(0).ok())
            .fetch_optional(pool)
            .await?
            .flatten();
        match anchor {
            Some(a) => {
                sqlx::query("UPDATE account SET douyin_no = ? WHERE anchor_id = ?")
                    .bind(&no)
                    .bind(&a)
                    .execute(pool)
                    .await?;
            }
            None => {
                sqlx::query(
                    "INSERT INTO account (person_id, anchor_id, douyin_no, anchor_name, is_primary)
                     VALUES (?,?,?,?,1)",
                )
                .bind(id as i64)
                .bind(&no) // 没有独立采集 ID 时用抖音号兜底
                .bind(&no)
                .bind(body.name.clone().unwrap_or_default())
                .execute(pool)
                .await
                .map_err(|e| map_account_err(e, &no))?;
            }
        }
    }

    Ok(Json(json!({ "ok": true, "personId": id })))
}

/// DELETE /api/admin/streamers/{id}：软删（deleted_at 置时间，历史数据保留）
pub async fn delete_streamer(State(st): State<ArcState>, Path(id): Path<u64>) -> ApiResult {
    require_local(&st)?;
    let pool = writer_pool(&st)?;
    let res =
        sqlx::query("UPDATE person SET deleted_at = NOW(3) WHERE id = ? AND deleted_at IS NULL")
            .bind(id as i64)
            .execute(pool)
            .await?;
    if res.rows_affected() == 0 {
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
    /// 当日音浪（直接给值时视为可信）
    pub wave: Option<f64>,
    /// 当日累计总音浪（快照口径；给了它就按差分算 wave）
    #[serde(rename = "cumulativeWave")]
    pub cumulative_wave: Option<f64>,
    pub minutes: Option<f64>,
    #[serde(rename = "isLive")]
    pub is_live: Option<bool>,
}

#[derive(serde::Serialize)]
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
    let rows = mysql::query(&st.pools, "LOCAL", dialect::STREAMERS_SQL).await?;
    Ok(rows
        .iter()
        .map(|r| LocalStreamer {
            id: read_u64(r, "id").unwrap_or(0),
            name: read_str(r, "name"),
            gender: read_str(r, "gender"),
        })
        .collect())
}

/// 导入匹配上下文：person 台账 + 抖音号→person 映射。
/// 一人多账号：同一人的多个抖音号都指向同一 person_id（多号归一）。
struct MatchCtx {
    streamers: Vec<LocalStreamer>,
    by_douyin: HashMap<String, u64>,
}

impl MatchCtx {
    #[cfg(test)]
    fn from_streamers(streamers: Vec<LocalStreamer>) -> Self {
        Self {
            streamers,
            by_douyin: HashMap::new(),
        }
    }
}

/// 加载匹配上下文（台账 + 抖音号映射）
async fn load_match_ctx(st: &AppState) -> anyhow::Result<MatchCtx> {
    let streamers = load_local_streamers(st).await?;
    let rows = mysql::query(&st.pools, "LOCAL", dialect::ACCOUNT_DOUYIN_MAP_SQL).await?;
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
    Ok(MatchCtx {
        streamers,
        by_douyin,
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
    // 匹配优先级：抖音号精确命中 > 名字归一化 > 名字原文
    let douyin_hit = row
        .douyin_no
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|no| ctx.by_douyin.get(no))
        .and_then(|pid| streamers.iter().find(|s| s.id == *pid));
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
pub fn parse_any_text(text: &str) -> Vec<ImportRow> {
    if let Ok((kind, rows)) = crate::csvparse::parse(text) {
        if kind != crate::csvparse::Kind::Unknown && !rows.is_empty() {
            let mapped: Vec<ImportRow> = rows
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

async fn body_rows(
    text: &Option<String>,
    rows: &Option<Vec<ImportRow>>,
) -> anyhow::Result<Vec<ImportRow>> {
    if let Some(rs) = rows {
        let mut out = Vec::new();
        for r in rs {
            let mut r = r.clone();
            r.name = r.name.trim().to_string();
            if !r.name.is_empty() {
                out.push(r);
            }
        }
        if !out.is_empty() {
            return Ok(out);
        }
    }
    let text = text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("缺少导入内容：text（粘贴文本）或 rows（结构化行）至少给一个")
        })?;
    Ok(parse_any_text(text))
}

/* ----------------------------- 导入：预览 / 提交 ----------------------------- */

#[derive(Deserialize)]
pub struct ImportBody {
    pub text: Option<String>,
    pub rows: Option<Vec<ImportRow>>,
    #[serde(rename = "bizDate")]
    pub biz_date: Option<String>,
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
    let raw_rows = body_rows(&body.text, &body.rows).await?;
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
    let team = body
        .team
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("缺少团别 team（male / female）"))?;
    let mode = body.mode.as_deref().unwrap_or("merge");
    let rows = body_rows(&body.text, &body.rows).await?;
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
        "missing": report.missing,
    })))
}

/// 导入提交结果（HTTP 与机器人共用）
pub struct CommitReport {
    pub batch_id: u64,
    pub total: usize,
    pub ok_rows: u32,
    /// 库里已有该人该日、且值完全一样 → 没动它（用户口径 2026-10-10：相同不覆盖，不同才覆盖）
    pub unchanged: u32,
    pub failures: Vec<Value>,
    /// 团内当天还没有数据的主播（完整度报告，docs：统一日期所有抖音号都要导入）
    pub missing: Vec<String>,
}

/// 导入提交核心：匹配 + 校验 + 事务写入 + 批次审计 + 清缓存。
/// bot 命令与 HTTP 端共用，保证同一套口径与校验。
pub async fn commit_import(
    st: &Arc<AppState>,
    team: &str,
    biz_date: &str,
    rows: Vec<ImportRow>,
    mode: &str,
    filename: &str,
) -> anyhow::Result<CommitReport> {
    anyhow::ensure!(
        st.sources.iter().any(|s| s.id == "LOCAL"),
        "管理端需要 LOCAL 数据源（douyin_writer 对应的本地 MariaDB）"
    );
    let pool = st.writer.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "写库不可用：douyin_writer 账号未配置或连不上（检查 DB_WRITER_* 环境变量），只读功能不受影响"
        )
    })?;
    let date = valid_biz_date(biz_date)?;
    valid_team(team)?;
    anyhow::ensure!(
        mode == "merge" || mode == "reject",
        "mode 只能是 merge / reject"
    );
    anyhow::ensure!(!rows.is_empty(), "没有解析到任何数据行");

    let ctx = load_match_ctx(st).await?;
    let aliases = merged_aliases(st).await;
    let resolved: Vec<ResolvedRow> = rows
        .iter()
        .map(|r| resolve_row(r, &ctx, &aliases, Some(team)))
        .collect();

    let mut failures: Vec<Value> = Vec::new();
    let mut ok_rows: Vec<&ResolvedRow> = Vec::new();
    for r in &resolved {
        if !r.matched {
            failures.push(
                json!({ "name": r.raw.name, "reason": "未匹配到主播，请先在主播台账建档或绑定" }),
            );
        } else if let Some(reason) = &r.team_conflict {
            failures.push(json!({ "name": r.raw.name, "reason": reason }));
        } else if r.raw.wave.is_none() && r.raw.cumulative_wave.is_none() && r.raw.minutes.is_none()
        {
            failures.push(json!({ "name": r.raw.name, "reason": "没有数据（wave / cumulativeWave / minutes 至少给一个）" }));
        } else {
            ok_rows.push(r);
        }
    }

    let mut tx = pool.begin().await?;

    if mode == "reject" {
        let cnt: i64 = sqlx::query(dialect::TEAM_DATE_COUNT_SQL)
            .bind(biz_date)
            .bind(team)
            .map(|r: sqlx::mysql::MySqlRow| r.try_get::<i64, _>(0).unwrap_or(0))
            .fetch_one(&mut *tx)
            .await?;
        if cnt > 0 {
            anyhow::bail!(
                "reject 模式：{team} 团 {biz_date} 已有 {cnt} 行数据，请改用 merge（幂等覆盖）或先处理旧数据"
            );
        }
    }

    let batch = sqlx::query(
        "INSERT INTO import_batch (source, filename, biz_date, team, imported_rows, ok_rows, failed_rows, note)
         VALUES ('admin-import', ?, ?, ?, ?, 0, 0, ?)",
    )
    .bind(filename)
    .bind(biz_date)
    .bind(team)
    .bind(resolved.len() as i64)
    .bind(failures_summary(&failures))
    .execute(&mut *tx)
    .await?;
    let batch_id = batch.last_insert_id();

    let mut ok_count: u32 = 0;
    let mut unchanged: u32 = 0;
    for r in &ok_rows {
        match write_daily_row(&mut tx, batch_id, biz_date, date, r).await {
            Ok(WriteOutcome::Written) => ok_count += 1,
            Ok(WriteOutcome::Unchanged) => unchanged += 1,
            Err(e) => failures.push(json!({ "name": r.raw.name, "reason": e.to_string() })),
        }
    }

    sqlx::query("UPDATE import_batch SET ok_rows = ?, failed_rows = ? WHERE id = ?")
        .bind(ok_count)
        .bind(failures.len() as i64)
        .bind(batch_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    st.clear_cache();
    // 通知所有后台页面自动刷新（机器人导入也走这里，所以手机 QQ 一导入电脑就会跳）
    crate::events::emit(
        crate::events::SCOPE_IMPORT,
        format!(
            "{} {} 导入 {} 行{}{}",
            if team == "female" { "女团" } else { "男团" },
            biz_date,
            ok_count,
            if unchanged > 0 {
                format!("（{unchanged} 行值相同未改动）")
            } else {
                String::new()
            },
            if filename.is_empty() {
                String::new()
            } else {
                format!("（{filename}）")
            }
        ),
    );

    // 完整度报告：该团当天还没有数据的主播（同日多文件导入时逐步收敛，
    // 全部到齐则 missing 为空）
    let missing = sqlx::query(dialect::MISSING_SQL)
        .bind(biz_date)
        .bind(team)
        .map(|r: sqlx::mysql::MySqlRow| r.try_get::<String, _>(0).unwrap_or_default())
        .fetch_all(pool)
        .await
        .unwrap_or_default();

    // 「我的」页微信推送（2026-10-09）：数据入库后算追分目标 + 每日报告，
    // 后台异步发送，不阻塞导入响应（渠道没配 / 没人绑定就什么都不做）
    crate::push::spawn_after_import(st, team, biz_date);

    Ok(CommitReport {
        batch_id,
        total: resolved.len(),
        ok_rows: ok_count,
        unchanged,
        failures,
        missing,
    })
}

fn failures_summary(failures: &[Value]) -> Option<String> {
    if failures.is_empty() {
        return None;
    }
    let names: Vec<String> = failures
        .iter()
        .filter_map(|f| f.get("name").and_then(|v| v.as_str()).map(String::from))
        .collect();
    Some(format!("失败 {} 行: {}", failures.len(), names.join("、")))
}

/// 单行写入 daily_metric。口径（docs/00 §4）：
/// - 给了 cumulativeWave → 与上一业务日快照差分；无上一日 = 基线日语义，wave=累计值且 reliable=0
/// - 只给 wave → 直接采用（trusted）；累计值回填 prev + wave（拿不到 prev 就 0）
/// - 差分跨天 > 1（漏采）或为负（回退）→ wave_reliable=0
/// 单行写入结果：真写/覆盖了，还是库里已经有一样的值（没动）
enum WriteOutcome {
    Written,
    Unchanged,
}

/// 已有行的当前值（比对用）
async fn existing_day_row(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    person_id: u64,
    biz_date: &str,
) -> anyhow::Result<Option<(i64, i64, i64, i8)>> {
    let row = sqlx::query(
        "SELECT wave, cumulative_wave, minutes, is_live FROM daily_metric
         WHERE person_id = ? AND biz_date = ?",
    )
    .bind(person_id as i64)
    .bind(biz_date)
    .map(|row: sqlx::mysql::MySqlRow| {
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
            row.try_get::<i8, _>("is_live").unwrap_or(0),
        )
    })
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row)
}

async fn write_daily_row(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    batch_id: u64,
    biz_date: &str,
    date: NaiveDate,
    r: &ResolvedRow,
) -> anyhow::Result<WriteOutcome> {
    let person_id = r.person_id.expect("调用前已保证 matched");
    let prev: Option<(f64, String)> = sqlx::query(dialect::PREV_CUMULATIVE_SQL)
        .bind(person_id as i64)
        .bind(biz_date)
        .map(|row: sqlx::mysql::MySqlRow| {
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

    let anchor: String = sqlx::query(dialect::PRIMARY_ANCHOR_SQL)
        .bind(person_id as i64)
        .map(|row: sqlx::mysql::MySqlRow| row.try_get::<String, _>(0).unwrap_or_default())
        .fetch_optional(&mut **tx)
        .await?
        .unwrap_or_default();

    let (wave, cumulative, prev_date, span, reliable): (i64, i64, Option<String>, i16, i8) =
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
            (None, None) => anyhow::bail!("没有音浪数据"),
        };

    let minutes = r.raw.minutes.unwrap_or(0.0) as i64;
    let is_live = r.raw.is_live.map(|b| b as i8).unwrap_or(0);

    // 时长-only 行（时长 CSV）：只更新 minutes/is_live，不把已导入的音浪覆盖成 0
    if r.raw.wave.is_none() && r.raw.cumulative_wave.is_none() {
        if let Some((_, _, m, live)) = existing_day_row(tx, person_id, biz_date).await? {
            if m == minutes && live == is_live {
                return Ok(WriteOutcome::Unchanged);
            }
        }
        sqlx::query(
            "INSERT INTO daily_metric
                 (person_id, anchor_id, biz_date, minutes, is_live, source_batch_id)
             VALUES (?,?,?,?,?,?)
             ON DUPLICATE KEY UPDATE
                 minutes = VALUES(minutes), is_live = VALUES(is_live),
                 source_batch_id = VALUES(source_batch_id)",
        )
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

    sqlx::query(
        "INSERT INTO daily_metric
             (person_id, anchor_id, biz_date, wave, cumulative_wave, prev_snapshot_date,
              wave_span, wave_reliable, minutes, is_live, source_batch_id)
         VALUES (?,?,?,?,?,?,?,?,?,?,?)
         ON DUPLICATE KEY UPDATE
             wave = VALUES(wave), cumulative_wave = VALUES(cumulative_wave),
             prev_snapshot_date = VALUES(prev_snapshot_date), wave_span = VALUES(wave_span),
             wave_reliable = VALUES(wave_reliable), minutes = VALUES(minutes),
             is_live = VALUES(is_live), source_batch_id = VALUES(source_batch_id)",
    )
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
    if biz_date.is_empty() || team.is_empty() {
        return Ok(Json(json!({ "checked": false })));
    }
    valid_team(&team)?;
    valid_biz_date(biz_date)?;
    let pool = writer_pool(&st)?;
    let rows: i64 = sqlx::query(dialect::TEAM_DATE_COUNT_SQL)
        .bind(biz_date)
        .bind(team)
        .map(|r: sqlx::mysql::MySqlRow| r.try_get::<i64, _>(0).unwrap_or(0))
        .fetch_one(pool)
        .await?;
    let last = sqlx::query(
        "SELECT filename, DATE_FORMAT(created_at, '%Y-%m-%d %H:%i') AS ts
         FROM import_batch WHERE biz_date = ? AND team = ? ORDER BY id DESC LIMIT 1",
    )
    .bind(biz_date)
    .bind(team)
    .map(|r: sqlx::mysql::MySqlRow| {
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
        "team": team,
        "rows": rows,
        "lastBatch": last,
    })))
}

/// GET /api/admin/import/batches：最近导入批次
pub async fn import_batches(State(st): State<ArcState>) -> ApiResult {
    require_local(&st)?;
    let rows = mysql::query(&st.pools, "LOCAL", dialect::BATCH_LIST_SQL).await?;
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
    valid_team(team)?;
    valid_biz_date(&q.biz_date)?;
    let pool = writer_pool(&st)?;
    let mut tx = pool.begin().await?;
    let metrics = sqlx::query(
        "DELETE d FROM daily_metric d \
         JOIN person p ON p.id = d.person_id \
         WHERE DATE_FORMAT(d.biz_date, '%Y-%m-%d') = ? AND p.gender = ?",
    )
    .bind(&q.biz_date)
    .bind(team)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let batches = sqlx::query("DELETE FROM import_batch WHERE biz_date = ? AND team = ?")
        .bind(&q.biz_date)
        .bind(team)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    crate::events::emit(
        crate::events::SCOPE_IMPORT,
        format!(
            "已删除「{}」{} 的音浪数据（{} 条）",
            q.biz_date,
            if team == "male" { "男团" } else { "女团" },
            metrics
        ),
    );
    Ok(Json(
        json!({ "ok": true, "metrics": metrics, "batches": batches }),
    ))
}

/* --------------------------------- 单测 --------------------------------- */

/// GET /api/admin/streamers/{id}/detail：主播详情（含全部账号）
pub async fn streamer_detail(State(st): State<ArcState>, Path(id): Path<u64>) -> ApiResult {
    require_local(&st)?;
    let base = mysql::query_with_binds(&st.pools, "LOCAL", dialect::STREAMER_BY_ID_SQL, |q| {
        q.bind(id as i64)
    })
    .await?;
    let row = base
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("主播不存在或已删除"))?;
    let accounts =
        mysql::query_with_binds(&st.pools, "LOCAL", dialect::ACCOUNTS_BY_PERSON_SQL, |q| {
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
                "isPrimary": r.try_get::<i8, _>("is_primary").unwrap_or(0) == 1,
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

/// POST /api/admin/streamers/{id}/accounts：绑定账号（anchor_id 唯一；主号可换绑）
pub async fn bind_account(
    State(st): State<ArcState>,
    Path(id): Path<u64>,
    Json(body): Json<BindAccountBody>,
) -> ApiResult {
    require_local(&st)?;
    let pool = writer_pool(&st)?;
    ensure(id > 0, "无效的主播 id")?;
    let anchor_id = body
        .anchor_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("anchorId 必填（采集 ID，全局唯一）"))?;
    let douyin_no = body
        .douyin_no
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| anchor_id.clone());
    let is_primary = body.is_primary.unwrap_or(false);

    // 主号换绑：先把该主播现有主号降级
    let mut tx = pool.begin().await?;
    if is_primary {
        sqlx::query("UPDATE account SET is_primary = 0 WHERE person_id = ? AND is_primary = 1")
            .bind(id as i64)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "INSERT INTO account (person_id, anchor_id, douyin_no, anchor_name, is_primary)
         VALUES (?,?,?,?,?)
         ON DUPLICATE KEY UPDATE
           person_id = VALUES(person_id), douyin_no = VALUES(douyin_no),
           anchor_name = VALUES(anchor_name), is_primary = VALUES(is_primary)",
    )
    .bind(id as i64)
    .bind(&anchor_id)
    .bind(&douyin_no)
    .bind(body.anchor_name.as_deref().unwrap_or(""))
    .bind(is_primary as i8)
    .execute(&mut *tx)
    .await
    .map_err(|e| map_account_err(e, &anchor_id))?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true, "anchorId": anchor_id })))
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
    let me = mysql::query_with_binds(&st.pools, "LOCAL", dialect::STREAMER_BY_ID_SQL, |q| {
        q.bind(id as i64)
    })
    .await?;
    let me = me
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("主播不存在或已删除"))?;
    let me_name = read_str(&me, "name");

    let mut master_name: Option<String> = None;
    if let Some(mid) = master_id {
        ensure(mid != id, "不能把自己设为师傅")?;

        // 师傅必须是有效主播
        let mrow = mysql::query_with_binds(&st.pools, "LOCAL", dialect::STREAMER_BY_ID_SQL, |q| {
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
            let link =
                mysql::query_with_binds(&st.pools, "LOCAL", dialect::MASTER_CHAIN_SQL, |q| {
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

    sqlx::query(dialect::SET_MASTER_SQL)
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
    let rows = mysql::query(
        &st.pools,
        "LOCAL",
        "SELECT id, name, master_id, generation FROM person WHERE deleted_at IS NULL",
    )
    .await?;
    let mut id_by_name: HashMap<String, u64> = HashMap::new();
    let mut name_counts: NameCounts = HashMap::new();
    let mut name_of: HashMap<u64, String> = HashMap::new();
    let mut master_of: HashMap<u64, Option<u64>> = HashMap::new();
    let mut gen_of: HashMap<u64, Option<i64>> = HashMap::new();
    for r in &rows {
        let id = read_u64(r, "id").unwrap_or(0);
        let name = read_str(r, "name");
        if id == 0 || name.is_empty() {
            continue;
        }
        *name_counts.entry(name.clone()).or_insert(0) += 1;
        id_of(&mut id_by_name, &name, id);
        name_of.insert(id, name);
        master_of.insert(id, read_opt_u64(r, "master_id").filter(|m| *m != id));
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
                match sqlx::query(
                    "UPDATE person SET master_id = ?, generation = ? WHERE id = ? AND deleted_at IS NULL",
                )
                .bind(*mid as i64)
                .bind(gen)
                .bind(*pid as i64)
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
                let master_res: anyhow::Result<u64> = (|| async {
                    if let Some(id) = p.master.as_deref().and_then(|m| id_by_name.get(m)) {
                        return Ok(*id);
                    }
                    let mname = p.master.clone().unwrap_or_default();
                    let r = sqlx::query(
                        "INSERT INTO person (name, gender, master_id, generation, status) \
                         VALUES (?, 'unknown', NULL, NULL, 'active')",
                    )
                    .bind(&mname)
                    .execute(pool)
                    .await?;
                    Ok(r.last_insert_id())
                })()
                .await;
                let mid = match master_res {
                    Ok(id) => {
                        if id_by_name
                            .get(p.master.as_deref().unwrap_or_default())
                            .is_none()
                        {
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
                let me_known = p.person_known && id_by_name.get(&p.name).is_some();
                let res = if me_known {
                    let pid = id_by_name[&p.name];
                    sqlx::query(
                        "UPDATE person SET master_id = ?, generation = ? WHERE id = ? AND deleted_at IS NULL",
                    )
                    .bind(mid as i64)
                    .bind(gen)
                    .bind(pid as i64)
                    .execute(pool)
                    .await
                    .map(|_| pid)
                } else {
                    sqlx::query(
                        "INSERT INTO person (name, gender, master_id, generation, status) \
                         VALUES (?, 'unknown', ?, ?, 'active')",
                    )
                    .bind(&p.name)
                    .bind(mid as i64)
                    .bind(gen)
                    .execute(pool)
                    .await
                    .map(|r| r.last_insert_id())
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
fn id_of(map: &mut HashMap<String, u64>, name: &str, id: u64) {
    map.entry(name.to_string()).or_insert(id);
}

/// GET /api/admin/streamers/duplicates：同名 / 同抖音号重复检测
pub async fn streamer_duplicates(State(st): State<ArcState>) -> ApiResult {
    require_local(&st)?;
    let names = mysql::query(&st.pools, "LOCAL", dialect::DUP_NAMES_SQL).await?;
    let douyins = mysql::query(&st.pools, "LOCAL", dialect::DUP_DOUYIN_SQL).await?;
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
        let r = sqlx::query(
            "UPDATE person SET deleted_at = NOW(3) WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(*id as i64)
        .execute(&mut *tx)
        .await?;
        ok += r.rows_affected();
    }
    tx.commit().await?;
    Ok(Json(json!({ "ok": true, "deleted": ok })))
}

/// POST /api/admin/streamers/{id}/merge {sourceId}：把 source 并入 target
/// ——账号改绑、日指标按日期求和并入（同日冲突求和）、别名留 SQLite、source 软删
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
    let src_accounts = sqlx::query("SELECT id, anchor_id FROM account WHERE person_id = ?")
        .bind(source as i64)
        .fetch_all(&mut *tx)
        .await?;
    for a in &src_accounts {
        let anchor: String = a.try_get("anchor_id").unwrap_or_default();
        let exists: i64 =
            sqlx::query("SELECT COUNT(*) FROM account WHERE anchor_id = ? AND person_id = ?")
                .bind(&anchor)
                .bind(id as i64)
                .fetch_one(&mut *tx)
                .await?
                .try_get(0)
                .unwrap_or(0);
        if exists == 0 {
            sqlx::query("UPDATE account SET person_id = ? WHERE id = ?")
                .bind(id as i64)
                .bind(a.try_get::<i64, _>("id").unwrap_or(0))
                .execute(&mut *tx)
                .await?;
        }
    }

    // 日指标：先并同日冲突（求和进 target），再整批改绑
    let src_dates = sqlx::query(
        "SELECT DATE_FORMAT(biz_date, '%Y-%m-%d') AS d, SUM(wave) AS w, SUM(minutes) AS m, MAX(is_live) AS live
         FROM daily_metric WHERE person_id = ? GROUP BY d",
    )
    .bind(source as i64)
    .fetch_all(&mut *tx)
    .await?;
    for r in &src_dates {
        let d: String = r.try_get("d").unwrap_or_default();
        let exists: i64 = sqlx::query(
            "SELECT COUNT(*) FROM daily_metric WHERE person_id = ? AND DATE_FORMAT(biz_date, '%Y-%m-%d') = ?",
        )
        .bind(id as i64)
        .bind(&d)
        .fetch_one(&mut *tx)
        .await?
        .try_get(0)
        .unwrap_or(0);
        if exists > 0 {
            // 同日冲突：target 累加 source 的值
            sqlx::query(
                "UPDATE daily_metric t
                 JOIN (SELECT SUM(wave) AS w, SUM(minutes) AS m, MAX(is_live) AS live
                       FROM daily_metric WHERE person_id = ? AND DATE_FORMAT(biz_date, '%Y-%m-%d') = ?) s
                 SET t.wave = t.wave + s.w, t.minutes = t.minutes + s.m,
                     t.is_live = GREATEST(t.is_live, s.live)
                 WHERE t.person_id = ? AND DATE_FORMAT(t.biz_date, '%Y-%m-%d') = ?",
            )
            .bind(source as i64)
            .bind(&d)
            .bind(id as i64)
            .bind(&d)
            .execute(&mut *tx)
            .await?;
        }
    }
    sqlx::query("UPDATE daily_metric SET person_id = ? WHERE person_id = ?")
        .bind(id as i64)
        .bind(source as i64)
        .execute(&mut *tx)
        .await?;

    // source 软删
    sqlx::query("UPDATE person SET deleted_at = NOW(3) WHERE id = ? AND deleted_at IS NULL")
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
        let total = mysql::query(&st.pools, "LOCAL", &sql)
            .await?
            .into_iter()
            .next()
            .and_then(|r| r.try_get::<i64, _>(0).ok())
            .unwrap_or(0);
        let missing_sql = format!(
            "SELECT COUNT(*) AS n FROM person p \
             LEFT JOIN daily_metric d ON d.person_id = p.id \
               AND DATE_FORMAT(d.biz_date, '%Y-%m-%d') = '{latest}' \
             WHERE p.deleted_at IS NULL AND p.gender = '{team}' AND d.id IS NULL"
        );
        let missing = if latest.is_empty() {
            total
        } else {
            mysql::query(&st.pools, "LOCAL", &missing_sql)
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
    let gaps = mysql::query(&st.pools, "LOCAL", gap_sql)
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
        .map_err(|e| anyhow::anyhow!("读取上传内容失败: {e}"))?
    {
        match field.name() {
            Some("filename") => {
                if let Ok(s) = field.text().await {
                    filename = s;
                }
            }
            Some("file") => {
                let fname = field.file_name().unwrap_or("上传文件").to_string();
                filename = fname;
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| anyhow::anyhow!("读取文件失败: {e}"))?;
                text = String::from_utf8_lossy(&bytes).into_owned();
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
    ensure(!rows.is_empty(), "文件里没有解析到任何数据行")?;
    Ok(Json(json!({
        "filename": filename,
        "total": rows.len(),
        "rows": rows,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(rows[0].wave, Some(25000.0), "「2.5万」必须走 csvparse 的单位换算");
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
            "UPDATE person SET deleted_at = NOW(3) WHERE id = ? AND deleted_at IS NULL",
            "INSERT INTO person (name, gender) VALUES (?,?)",
            "INSERT INTO account (person_id, anchor_id) VALUES (?,?)",
            "UPDATE account SET douyin_no = ? WHERE anchor_id = ?",
            "UPDATE import_batch SET ok_rows = ?, failed_rows = ? WHERE id = ?",
            "INSERT INTO import_batch (source, filename) VALUES (?,?)",
            "INSERT INTO daily_metric (person_id) VALUES (?) ON DUPLICATE KEY UPDATE wave = VALUES(wave)",
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
