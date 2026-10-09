//! 运行时状态存储（2026-10-10 单库化，并入 MySQL）：别名 / 分组方案 / 名单草稿 /
//! 配置 / 机器人会话 / 展示端账号 / 抖音号绑定审计。
//!
//! 历史上这批表放在本地 SQLite（rusqlite 同步 API）。单库决策后统一并入业务库
//! （本机 douyin_dev / 生产 douyin），与业务事实源同库——部署时 `upload-db` 一次带全，
//! 生产不再需要单独维护运行时数据。
//!
//! - 读走 douyin_writer 写池（表都很小，读写分离没有意义；writer 缺失时读返回空、写报错）
//! - 旧 SQLite 文件只作为一次性搬家来源：`pk-server migrate` 里调
//!   [`carry_legacy_sqlite`] 把数据幂等搬进来（config 表 sqlite_carried=1 后跳过）
//! - 表结构见 migrations/003_runtime.sql；列名差异（key→cfg_key、text→body）都是方言原因

use anyhow::Result;
use sqlx::MySqlPool;
use sqlx::Row;

use crate::state::AppState;

/* ------------------------------ 内部工具 ------------------------------ */

/// writer 池。运行时表都在业务库，读写统一走 writer（缺配置时读返回空、写报错）。
fn w(st: &AppState) -> Result<&MySqlPool> {
    st.writer
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("写库不可用：douyin_writer 未配置"))
}

/// 赛事类型白名单（2026-10-09 用户定名）：
/// - 初级争霸赛小组赛（月初）、中级争霸赛小组赛（月中）—— 白天的分组赛，第 N 组后打一场 20 分钟复活赛
/// - 月终争霸赛（月末）—— 只分组，没有小组赛 / 复活赛 / 晋级赛流程
/// - 初级争霸赛晋级赛 / 中级争霸赛晋级赛 —— 晚上晋级赛，按级别拆两页
/// 值直接用中文落库，前后端共用同一套字符串，避免 key/文案两套映射各错各的。
pub const EVENT_TYPES: [&str; 5] = [
    "初级争霸赛小组赛",
    "中级争霸赛小组赛",
    "月终争霸赛",
    "初级争霸赛晋级赛",
    "中级争霸赛晋级赛",
];

/// 老赛事类型 → 新赛事类型（2026-10-09 改名前落库的值）。
/// 万一漏网，`normalize_event_type` 会在读取侧归一（老库升级已在搬家时处理）。
pub const LEGACY_EVENT_RENAMES: [(&str, &str); 6] = [
    ("初级", "初级争霸赛小组赛"),
    ("中级争霸赛", "中级争霸赛小组赛"),
    ("月底争霸赛", "月终争霸赛"),
    ("初级晋级赛", "初级争霸赛晋级赛"),
    ("中级晋级赛", "中级争霸赛晋级赛"),
    ("晋级赛", "初级争霸赛晋级赛"),
];

/// 归一化赛事类型：白名单内的值原样返回，老值（见 LEGACY_EVENT_RENAMES）归一到新值，
/// 其它一律落成空串（= 未分类，老方案兼容）。
pub fn normalize_event_type(v: &str) -> String {
    let t = v.trim();
    if EVENT_TYPES.contains(&t) {
        return t.to_string();
    }
    match LEGACY_EVENT_RENAMES.iter().find(|(old, _)| *old == t) {
        Some((_, new)) => (*new).to_string(),
        None => String::new(),
    }
}

/* ------------------------------- 别名 ------------------------------- */

pub type AliasMap = std::collections::HashMap<String, String>;

/// 全量别名（导入匹配的兜底映射；查询失败按空表处理并记日志）
pub async fn list_aliases(st: &AppState) -> AliasMap {
    let Ok(pool) = w(st) else {
        return AliasMap::new();
    };
    match sqlx::query("SELECT alias, real_name FROM aliases")
        .fetch_all(pool)
        .await
    {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|r| {
                Some((
                    r.try_get::<String, _>(0).ok()?,
                    r.try_get::<String, _>(1).ok()?,
                ))
            })
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "查别名失败，按空表处理");
            AliasMap::new()
        }
    }
}

pub async fn upsert_alias(st: &AppState, alias: &str, real_name: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO aliases (alias, real_name) VALUES (?, ?)
         ON DUPLICATE KEY UPDATE real_name = VALUES(real_name)",
    )
    .bind(alias)
    .bind(real_name)
    .execute(w(st)?)
    .await?;
    Ok(())
}

pub async fn delete_alias(st: &AppState, alias: &str) -> Result<()> {
    sqlx::query("DELETE FROM aliases WHERE alias = ?")
        .bind(alias)
        .execute(w(st)?)
        .await?;
    Ok(())
}

/* --------------------------------- 名单 ---------------------------------- */

/// 读某赛事类型的当前名单。`None` 表示「这个类型从来没保存过」，
/// 前端据此清空输入框让运营重填（初级小组赛/中级小组赛/月终争霸的名单本来就不一样）。
/// 这里不能用空串表示未保存：空串在语义上是「名单被清空」，两者混在一起
/// 会让用户不小心清空一次之后就再也回不来。
pub async fn load_roster(st: &AppState, event_type: &str) -> Option<(String, String)> {
    let pool = w(st).ok()?;
    let row = match sqlx::query("SELECT body, updated_at FROM roster_state WHERE event_type = ?")
        .bind(event_type)
        .fetch_optional(pool)
        .await
    {
        Ok(Some(r)) => r,
        _ => return None,
    };
    Some((
        row.try_get::<String, _>("body").ok()?,
        row.try_get::<String, _>("updated_at").ok()?,
    ))
}

/// 覆盖写入某赛事类型的名单，返回写入时间（RFC3339）。
pub async fn save_roster(st: &AppState, event_type: &str, text: &str) -> String {
    let updated_at = chrono::Utc::now().to_rfc3339();
    let res = async {
        let pool = w(st)?;
        sqlx::query(
            "INSERT INTO roster_state (event_type, body, updated_at) VALUES (?, ?, ?)
             ON DUPLICATE KEY UPDATE body = VALUES(body), updated_at = VALUES(updated_at)",
        )
        .bind(event_type)
        .bind(text)
        .bind(&updated_at)
        .execute(pool)
        .await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(e) = res {
        tracing::error!(error = %e, "写名单失败");
    }
    updated_at
}

/* ------------------------------ 分组方案 ------------------------------ */

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct PlanRow {
    pub id: i64,
    pub title: String,
    pub metric: String,
    pub days: i64,
    pub payload: String,
    pub created_at: String,
    /// 是否已发布到对外展示页
    pub published: bool,
    /// 赛事类型（初级争霸赛小组赛/中级争霸赛小组赛/月终争霸赛/两级晋级赛），空串 = 未分类的老方案
    #[serde(rename = "eventType")]
    pub event_type: String,
}

const PLAN_COLS: &str = "id, title, metric, days, payload, created_at, published, event_type";

fn map_plan(r: &sqlx::mysql::MySqlRow) -> Result<PlanRow> {
    Ok(PlanRow {
        id: r.try_get::<u64, _>(0)? as i64,
        title: r.try_get::<String, _>(1)?,
        metric: r.try_get::<String, _>(2)?,
        days: r.try_get::<i64, _>(3)?,
        payload: r.try_get::<String, _>(4)?,
        created_at: r.try_get::<String, _>(5)?,
        published: r.try_get::<i8, _>(6)? != 0,
        event_type: r.try_get::<String, _>(7).unwrap_or_default(),
    })
}

pub async fn save_plan(
    st: &AppState,
    title: &str,
    metric: &str,
    days: i64,
    payload: &str,
    event_type: &str,
) -> Result<i64> {
    let res = sqlx::query(
        "INSERT INTO plans (title, metric, days, payload, created_at, published, event_type)
         VALUES (?, ?, ?, ?, ?, 0, ?)",
    )
    .bind(title)
    .bind(metric)
    .bind(days)
    .bind(payload)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(normalize_event_type(event_type))
    .execute(w(st)?)
    .await?;
    Ok(res.last_insert_id() as i64)
}

pub async fn list_plans(st: &AppState) -> Vec<PlanRow> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    match sqlx::query(&format!(
        "SELECT {PLAN_COLS} FROM plans ORDER BY id DESC LIMIT 50"
    ))
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows.iter().map(map_plan).filter_map(Result::ok).collect(),
        Err(e) => {
            tracing::error!(error = %e, "查方案失败，按空表处理");
            Vec::new()
        }
    }
}

pub async fn get_plan(st: &AppState, id: i64) -> Option<PlanRow> {
    let pool = w(st).ok()?;
    let row = match sqlx::query(&format!("SELECT {PLAN_COLS} FROM plans WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await
    {
        Ok(Some(r)) => r,
        _ => return None,
    };
    Some(map_plan(&row).ok()?)
}

/// 发布 / 取消发布。未发布时对外展示页返回 404。
/// 上架互斥按赛事类型（2026-10-09 起）：展示端每个赛事类型一个独立分组页
/// （初级小组赛/中级小组赛/月终争霸/两级晋级赛），同类型同一时刻只显示一场，
/// 新方案上架时自动下架同类型的其它方案；不同类型互不影响。
pub async fn set_published(st: &AppState, id: i64, published: bool) -> bool {
    let Ok(pool) = w(st) else {
        return false;
    };
    let mut tx = match pool.begin().await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "上架事务开启失败");
            return false;
        }
    };
    if published {
        if let Err(e) = sqlx::query(
            "UPDATE plans SET published = 0 WHERE published = 1 AND id != ? \
             AND COALESCE(event_type, '') = COALESCE((SELECT event_type FROM (SELECT event_type FROM plans WHERE id = ?) t), '')",
        )
        .bind(id)
        .bind(id)
        .execute(&mut *tx)
        .await
        {
            tracing::error!(error = %e, "下架同类方案失败");
            return false;
        }
    }
    match sqlx::query("UPDATE plans SET published = ? WHERE id = ?")
        .bind(if published { 1i8 } else { 0i8 })
        .bind(id)
        .execute(&mut *tx)
        .await
    {
        Ok(res) => {
            if tx.commit().await.is_err() {
                return false;
            }
            res.rows_affected() > 0
        }
        Err(e) => {
            tracing::error!(error = %e, "上架更新失败");
            false
        }
    }
}

/// 对外展示页用：只允许取已发布的方案
pub async fn get_published_plan(st: &AppState, id: i64) -> Option<PlanRow> {
    get_plan(st, id).await.filter(|p| p.published)
}

pub async fn delete_plan(st: &AppState, id: i64) -> Result<()> {
    sqlx::query("DELETE FROM plans WHERE id = ?")
        .bind(id)
        .execute(w(st)?)
        .await?;
    Ok(())
}

/* ------------------------------- 配置项 ------------------------------- */

/// 读配置项（机器人 token 等跨重启保留的状态）；读不到/出错 → None
pub async fn config_get(st: &AppState, key: &str) -> Option<String> {
    let pool = w(st).ok()?;
    sqlx::query("SELECT cfg_value FROM config WHERE cfg_key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()?
        .into_iter()
        .next()
        .and_then(|r| r.try_get::<String, _>(0).ok())
}

/// 写配置项（出错只记日志，与旧 SQLite 行为一致——配置写失败不应打断主流程）
pub async fn config_set(st: &AppState, key: &str, value: &str) {
    let res = async {
        let pool = w(st)?;
        sqlx::query(
            "INSERT INTO config (cfg_key, cfg_value, updated_at) VALUES (?, ?, ?)
             ON DUPLICATE KEY UPDATE cfg_value = VALUES(cfg_value), updated_at = VALUES(updated_at)",
        )
        .bind(key)
        .bind(value)
        .bind(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string())
        .execute(pool)
        .await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(e) = res {
        tracing::error!(error = %e, key, "写配置失败");
    }
}

/* ---------------------------- 机器人会话 ---------------------------- */

/// 登记机器人会话（每条入站消息都刷新，含非管理员）
pub async fn session_upsert(
    st: &AppState,
    channel: &str,
    conversation_id: &str,
    kind: &str,
    last_text: &str,
) {
    let res = async {
        let pool = w(st)?;
        sqlx::query(
            "INSERT INTO bot_sessions (channel, conversation_id, kind, last_text, last_seen, is_admin)
             VALUES (?, ?, ?, ?, ?, 0)
             ON DUPLICATE KEY UPDATE kind = VALUES(kind), last_text = VALUES(last_text), last_seen = VALUES(last_seen)",
        )
        .bind(channel)
        .bind(conversation_id)
        .bind(kind)
        .bind(&last_text.chars().take(100).collect::<String>())
        .bind(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string())
        .execute(pool)
        .await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(e) = res {
        tracing::error!(error = %e, "登记机器人会话失败");
    }
}

/// 会话是否被标记为管理员
pub async fn session_is_admin(st: &AppState, channel: &str, conversation_id: &str) -> bool {
    let Ok(pool) = w(st) else {
        return false;
    };
    matches!(
        sqlx::query("SELECT is_admin FROM bot_sessions WHERE channel = ? AND conversation_id = ?")
            .bind(channel)
            .bind(conversation_id)
            .fetch_optional(pool)
            .await,
        Ok(Some(r)) if r.try_get::<i8, _>(0).unwrap_or(0) == 1
    )
}

/// 会话列表（管理界面展示）
pub async fn session_list(st: &AppState) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    match sqlx::query(
        "SELECT channel, conversation_id, kind, IFNULL(last_text,''), last_seen, is_admin
         FROM bot_sessions ORDER BY last_seen DESC LIMIT 200",
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "channel": r.try_get::<String, _>(0).unwrap_or_default(),
                    "conversationId": r.try_get::<String, _>(1).unwrap_or_default(),
                    "kind": r.try_get::<String, _>(2).unwrap_or_default(),
                    "lastText": r.try_get::<String, _>(3).unwrap_or_default(),
                    "lastSeen": r.try_get::<String, _>(4).unwrap_or_default(),
                    "isAdmin": r.try_get::<i8, _>(5).unwrap_or(0) == 1,
                })
            })
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "查机器人会话失败");
            Vec::new()
        }
    }
}

/// 勾选/取消管理员会话
pub async fn session_set_admin(
    st: &AppState,
    channel: &str,
    conversation_id: &str,
    is_admin: bool,
) {
    if let Ok(pool) = w(st) {
        let _ = sqlx::query(
            "UPDATE bot_sessions SET is_admin = ? WHERE channel = ? AND conversation_id = ?",
        )
        .bind(if is_admin { 1i8 } else { 0i8 })
        .bind(channel)
        .bind(conversation_id)
        .execute(pool)
        .await;
    }
}

/* --------------------------- 抖音号绑定校验（纯函数） --------------------------- */

/// 抖音号格式：6~20 位字母 / 数字 / 下划线 / 点（抖音号官方口径的宽松版）
pub fn validate_douyin_no(v: &str) -> std::result::Result<String, String> {
    let v = v.trim();
    let ok = (6..=20).contains(&v.len())
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if !ok {
        return Err("抖音号格式不对：6~20 位字母、数字、下划线或点".into());
    }
    Ok(v.to_string())
}

/* ------------------------------ 我的页面账号 ------------------------------ */

/// 校验姓名/密码。Err(错误文案)，Ok((姓名, 密码))。
/// **密码为 4~20 位字母或数字（2026-10-10 放宽，原为纯数字）**：
/// 去首尾空格后必须全是 ASCII 字母/数字，不允许空格、横线等分隔符。
pub fn validate_my_account(
    name: &str,
    pass: &str,
) -> std::result::Result<(String, String), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("姓名不能为空".into());
    }
    if name.chars().count() > 20 {
        return Err("姓名太长（最多 20 字）".into());
    }
    let pass = pass.trim();
    if pass.is_empty() {
        return Err("密码不能为空".into());
    }
    if !pass.chars().all(|c| c.is_ascii_alphanumeric()) || pass.len() < 4 || pass.len() > 20 {
        return Err("密码必须是 4~20 位字母或数字".into());
    }
    Ok((name.to_string(), pass.to_string()))
}

/// 手机号归一化（选填）：去空格横线后须为 5~20 位纯数字；空串 = 未设置
fn validate_my_phone(phone: &str) -> std::result::Result<String, String> {
    let p: String = phone
        .trim()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    if p.is_empty() {
        return Ok(String::new());
    }
    if !p.chars().all(|c| c.is_ascii_digit()) || p.len() < 5 || p.len() > 20 {
        return Err("手机号格式不对：应为 5~20 位数字".into());
    }
    Ok(p)
}

/// 全部账号（管理端列表用），按添加时间倒序
pub async fn my_account_list(st: &AppState) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    match sqlx::query(
        "SELECT name, phone, pass, is_admin, created_at FROM my_accounts ORDER BY created_at DESC, name ASC",
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.try_get::<String, _>(0).unwrap_or_default(),
                    "phone": r.try_get::<String, _>(1).unwrap_or_default(),
                    "pass": r.try_get::<String, _>(2).unwrap_or_default(),
                    "isAdmin": r.try_get::<i8, _>(3).unwrap_or(0) == 1,
                    "createdAt": r.try_get::<String, _>(4).unwrap_or_default(),
                })
            })
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "查展示端账号失败");
            Vec::new()
        }
    }
}

/// 手动设置账号（右键主播 / 管理端卡片）：存在则覆盖更新手机号+密码，不存在则新建。
/// 一个姓名对应一个密码（1:1，唯一键兜底 + 代码级查重）；手机号选填，填了则一个手机号也只绑一个姓名。
pub async fn my_account_set(
    st: &AppState,
    name: &str,
    phone: &str,
    pass: &str,
) -> std::result::Result<(), String> {
    my_account_set_pool(w(st).map_err(|e| e.to_string())?, name, phone, pass).await
}

async fn my_account_set_pool(
    pool: &MySqlPool,
    name: &str,
    phone: &str,
    pass: &str,
) -> std::result::Result<(), String> {
    let (name, pass) = validate_my_account(name, pass)?;
    let phone = validate_my_phone(phone)?;
    // 密码 1:1：不允许两个姓名共用一个密码（自己更新自己不算冲突）
    let dup_pass: bool = sqlx::query("SELECT 1 FROM my_accounts WHERE pass = ? AND name != ?")
        .bind(&pass)
        .bind(&name)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("查重失败：{e}"))?
        .is_some();
    if dup_pass {
        return Err("密码已存在（已绑定其它姓名）".into());
    }
    if !phone.is_empty() {
        let dup_phone: bool =
            sqlx::query("SELECT 1 FROM my_accounts WHERE phone = ? AND name != ? AND phone != ''")
                .bind(&phone)
                .bind(&name)
                .fetch_optional(pool)
                .await
                .map_err(|e| format!("查重失败：{e}"))?
                .is_some();
        if dup_phone {
            return Err("手机号已存在（已绑定其它姓名）".into());
        }
    }
    sqlx::query(
        "INSERT INTO my_accounts (name, phone, pass, is_admin, created_at) VALUES (?, ?, ?, 0, ?)
         ON DUPLICATE KEY UPDATE phone = VALUES(phone), pass = VALUES(pass)",
    )
    .bind(&name)
    .bind(&phone)
    .bind(&pass)
    .bind(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string())
    .execute(pool)
    .await
    .map_err(|e| {
        let msg = e.to_string();
        if msg.contains("Duplicate entry") && msg.contains("uk_my_pass") {
            "密码已存在（已绑定其它姓名）".into()
        } else {
            format!("写入失败：{msg}")
        }
    })?;
    Ok(())
}

/// 单个账号（右键弹窗回填用）；没有则 None
pub async fn my_account_get(st: &AppState, name: &str) -> Option<serde_json::Value> {
    let pool = w(st).ok()?;
    let row = match sqlx::query(
        "SELECT name, phone, pass, is_admin, created_at FROM my_accounts WHERE name = ?",
    )
    .bind(name.trim())
    .fetch_optional(pool)
    .await
    {
        Ok(Some(r)) => r,
        _ => return None,
    };
    let name = row.try_get::<String, _>("name").ok()?;
    Some(serde_json::json!({
        "name": name,
        "phone": row.try_get::<String, _>("phone").unwrap_or_default(),
        "pass": row.try_get::<String, _>("pass").unwrap_or_default(),
        "isAdmin": row.try_get::<i8, _>("is_admin").unwrap_or(0) == 1,
        "createdAt": row.try_get::<String, _>("created_at").unwrap_or_default(),
    }))
}

/// 删除账号，返回是否存在
pub async fn my_account_delete(st: &AppState, name: &str) -> bool {
    matches!(w(st), Ok(pool) if sqlx::query("DELETE FROM my_accounts WHERE name = ?")
        .bind(name.trim())
        .execute(pool)
        .await
        .map(|r| r.rows_affected() > 0)
        .unwrap_or(false))
}

/// 设置/取消展示端管理员标记（管理员观看账号：不对应台账主播也能登录看全站）
pub async fn my_account_set_admin(st: &AppState, name: &str, admin: bool) -> bool {
    matches!(w(st), Ok(pool) if sqlx::query("UPDATE my_accounts SET is_admin = ? WHERE name = ?")
        .bind(if admin { 1i8 } else { 0i8 })
        .bind(name.trim())
        .execute(pool)
        .await
        .map(|r| r.rows_affected() > 0)
        .unwrap_or(false))
}

/// 是否展示端管理员账号（verify 在台账里找不到同名主播时据此放行）
pub async fn my_account_is_admin(st: &AppState, name: &str) -> bool {
    let Ok(pool) = w(st) else {
        return false;
    };
    matches!(
        sqlx::query("SELECT is_admin FROM my_accounts WHERE name = ?")
            .bind(name.trim())
            .fetch_optional(pool)
            .await,
        Ok(Some(r)) if r.try_get::<i8, _>(0).unwrap_or(0) == 1
    )
}

/* ------------------------- 内置展示端账号 ------------------------- */

/// 内置展示端账号：账号与密码相同，`is_admin=1`（管理员观看账号：台账里没有同名主播也放行，
/// 全站看得到、个人数据页为空）。跟着代码走，不依赖任何一份库 dump —— 生产换库、
/// 全新部署、库被清空后重启，这几个账号都还在。
pub const BUILTIN_MY_ACCOUNTS: [(&str, &str); 1] = [("shuai", "shuai")];

/// 启动时补齐内置账号（幂等）：
/// - 同名账号已存在 → 只把 `is_admin` 拉回 1，**密码保持运营自己设置的样子**，不覆盖
/// - 密码被别的姓名占用（`uk_my_pass` 1:1 唯一键）→ 跳过并告警，不能顶掉别人的登录
/// - 表还不存在（没跑过 migrate）→ 告警后跳过，不影响服务启动
///
/// 返回 (新建数, 跳过数)。
pub async fn ensure_builtin_my_accounts(st: &AppState) -> (usize, usize) {
    let Ok(pool) = w(st) else {
        tracing::warn!("内置展示端账号未写入：写库不可用（douyin_writer 未配置）");
        return (0, BUILTIN_MY_ACCOUNTS.len());
    };
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut created = 0usize;
    let mut skipped = 0usize;

    for (name, pass) in BUILTIN_MY_ACCOUNTS {
        let exists = match sqlx::query("SELECT name FROM my_accounts WHERE name = ?")
            .bind(name)
            .fetch_optional(pool)
            .await
        {
            Ok(row) => row.is_some(),
            Err(e) => {
                tracing::warn!(error = %e, name, "内置展示端账号查不到（表还没建？先跑 pk-server migrate）");
                skipped += 1;
                continue;
            }
        };
        if exists {
            if let Err(e) = sqlx::query("UPDATE my_accounts SET is_admin = 1 WHERE name = ?")
                .bind(name)
                .execute(pool)
                .await
            {
                tracing::warn!(error = %e, name, "内置展示端账号管理员标记更新失败");
                skipped += 1;
            }
            continue;
        }

        // 密码 1:1：已被别的姓名占用就不能插（否则会顶掉那个人的登录）
        let pass_taken = sqlx::query("SELECT name FROM my_accounts WHERE pass = ?")
            .bind(pass)
            .fetch_optional(pool)
            .await
            .map(|r| r.is_some())
            .unwrap_or(false);
        if pass_taken {
            tracing::warn!(name, "内置展示端账号未创建：密码已被其它姓名占用，请先改那个账号的密码");
            skipped += 1;
            continue;
        }

        match sqlx::query(
            "INSERT INTO my_accounts (name, phone, pass, is_admin, created_at)
             VALUES (?, '', ?, 1, ?)",
        )
        .bind(name)
        .bind(pass)
        .bind(&now)
        .execute(pool)
        .await
        {
            Ok(_) => {
                created += 1;
                tracing::info!(name, "已写入内置展示端账号（管理员观看账号）");
            }
            Err(e) => {
                tracing::warn!(error = %e, name, "内置展示端账号写入失败");
                skipped += 1;
            }
        }
    }
    (created, skipped)
}

/// 手机号归一化：去空格 / 横线 / 点，与 my_account_set 的写入口径一致
pub fn normalize_phone(v: &str) -> String {
    v.chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != '.')
        .collect()
}

/// 展示端登录（2026-10-10 起支持手机号+密码，姓名+密码继续兼容）：
/// 输入「账号」先按手机号查（纯数字 5~20 位才当手机号），没命中再按姓名查。
/// 返回命中的账号姓名（拿去台账里找人），全都没对上 → None。
pub async fn my_account_check_login(st: &AppState, account: &str, pass: &str) -> Option<String> {
    let pool = w(st).ok()?;
    let pass = pass.trim();
    if pass.is_empty() {
        return None;
    }
    let acct = account.trim();
    if acct.is_empty() {
        return None;
    }
    // 1) 手机号路径：纯数字 5~20 位
    let phone = normalize_phone(acct);
    let as_phone = phone.chars().all(|c| c.is_ascii_digit()) && (5..=20).contains(&phone.len());
    if as_phone {
        let hit = sqlx::query("SELECT name FROM my_accounts WHERE phone = ? AND pass = ?")
            .bind(&phone)
            .bind(pass)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .and_then(|r| r.try_get::<String, _>(0).ok());
        if hit.is_some() {
            return hit;
        }
    }
    // 2) 姓名路径（兼容老的姓名+密码登录）
    sqlx::query("SELECT name FROM my_accounts WHERE name = ? AND pass = ?")
        .bind(acct)
        .bind(pass)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get::<String, _>(0).ok())
        .filter(|_| !acct.is_empty())
        .map(|_| acct.to_string())
}

/* --------------------- 抖音号绑定申请（展示端提交 → 管理端审计） --------------------- */

/// 提交绑定申请。Err(错误文案)；Ok(申请 id)。
/// 规则：一人同时只能有一笔待审；同抖音号已通过过就不再收。
pub async fn anchor_claim_insert(
    st: &AppState,
    person_id: i64,
    person_name: &str,
    team: &str,
    douyin_no: &str,
    screenshot: &[u8],
    mime: &str,
) -> std::result::Result<i64, String> {
    let no = validate_douyin_no(douyin_no)?;
    if screenshot.is_empty() {
        return Err("缺少截图：请上传抖音主页截图".into());
    }
    if screenshot.len() > 1_500_000 {
        return Err(
            "截图超过大小限制（最多 1.5MB），请换小一点的图，格式支持 JPEG/PNG/WebP".into(),
        );
    }
    let mime = match mime {
        "image/jpeg" | "image/png" | "image/webp" => mime,
        _ => "image/jpeg",
    };
    let pool = w(st).map_err(|e| e.to_string())?;
    if sqlx::query("SELECT 1 FROM anchor_claims WHERE person_id = ? AND status = 'pending'")
        .bind(person_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("查重失败：{e}"))?
        .is_some()
    {
        return Err("已存在待审核的提交，请等管理员审核".into());
    }
    if sqlx::query(
        "SELECT 1 FROM anchor_claims WHERE person_id = ? AND status = 'approved' AND douyin_no = ?",
    )
    .bind(person_id)
    .bind(&no)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("查重失败：{e}"))?
    .is_some()
    {
        return Err("该抖音号已存在绑定记录，不用重复提交".into());
    }
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let res = sqlx::query(
        "INSERT INTO anchor_claims
           (person_id, person_name, team, douyin_no, screenshot, mime, status, created_at)
         VALUES (?, ?, ?, ?, ?, ?, 'pending', ?)",
    )
    .bind(person_id)
    .bind(person_name)
    .bind(team)
    .bind(&no)
    .bind(screenshot)
    .bind(mime)
    .bind(&now)
    .execute(pool)
    .await
    .map_err(|e| format!("写入失败：{e}"))?;
    Ok(res.last_insert_id() as i64)
}

/// 某人最近一笔申请（展示端「我的」显示状态用）
pub async fn anchor_claim_mine(st: &AppState, person_id: i64) -> Option<serde_json::Value> {
    let pool = w(st).ok()?;
    let row = match sqlx::query(
        "SELECT id, douyin_no, status, reject_reason, created_at, decided_at, person_name, team
         FROM anchor_claims WHERE person_id = ? ORDER BY id DESC LIMIT 1",
    )
    .bind(person_id)
    .fetch_optional(pool)
    .await
    {
        Ok(Some(r)) => r,
        _ => return None,
    };
    Some(serde_json::json!({
        "id": row.try_get::<u64, _>("id").ok()? as i64,
        "douyinNo": row.try_get::<String, _>("douyin_no").unwrap_or_default(),
        "status": row.try_get::<String, _>("status").unwrap_or_default(),
        "rejectReason": row.try_get::<String, _>("reject_reason").unwrap_or_default(),
        "createdAt": row.try_get::<String, _>("created_at").unwrap_or_default(),
        "decidedAt": row.try_get::<String, _>("decided_at").unwrap_or_default(),
        "personName": row.try_get::<String, _>("person_name").unwrap_or_default(),
        "team": row.try_get::<String, _>("team").unwrap_or_default(),
    }))
}

/// 审计列表（管理端）。status 空 = 全部；不含截图本体。
pub async fn anchor_claim_list(st: &AppState, status: &str, limit: i64) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    match sqlx::query(
        "SELECT id, person_id, person_name, team, douyin_no, status,
                reject_reason, created_at, decided_at, LENGTH(screenshot)
         FROM anchor_claims
         WHERE (? = '' OR status = ?)
         ORDER BY (status = 'pending') DESC, id DESC LIMIT ?",
    )
    .bind(status)
    .bind(status)
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.try_get::<u64, _>(0).unwrap_or(0) as i64,
                    "personId": r.try_get::<u64, _>(1).unwrap_or(0) as i64,
                    "personName": r.try_get::<String, _>(2).unwrap_or_default(),
                    "team": r.try_get::<String, _>(3).unwrap_or_default(),
                    "douyinNo": r.try_get::<String, _>(4).unwrap_or_default(),
                    "status": r.try_get::<String, _>(5).unwrap_or_default(),
                    "rejectReason": r.try_get::<String, _>(6).unwrap_or_default(),
                    "createdAt": r.try_get::<String, _>(7).unwrap_or_default(),
                    "decidedAt": r.try_get::<String, _>(8).unwrap_or_default(),
                    "screenshotBytes": r.try_get::<i64, _>(9).unwrap_or(0),
                })
            })
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "查绑定申请列表失败");
            Vec::new()
        }
    }
}

/// 截图本体（管理端预览）
pub async fn anchor_claim_image(st: &AppState, id: i64) -> Option<(Vec<u8>, String)> {
    let pool = w(st).ok()?;
    let row = match sqlx::query("SELECT screenshot, mime FROM anchor_claims WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
    {
        Ok(Some(r)) => r,
        _ => return None,
    };
    Some((
        row.try_get::<Vec<u8>, _>("screenshot").ok()?,
        row.try_get::<String, _>("mime").unwrap_or_default(),
    ))
}

/// 审计裁决：approve=true 通过 / false 驳回。返回是否存在 pending 的这笔。
pub async fn anchor_claim_decide(
    st: &AppState,
    id: i64,
    approve: bool,
    reject_reason: &str,
    decided_by: &str,
) -> Option<(i64, String, String)> {
    let pool = w(st).ok()?;
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let status = if approve { "approved" } else { "rejected" };
    let n = sqlx::query(
        "UPDATE anchor_claims SET status = ?, reject_reason = ?, decided_at = ?, decided_by = ?
         WHERE id = ? AND status = 'pending'",
    )
    .bind(status)
    .bind(reject_reason)
    .bind(&now)
    .bind(decided_by)
    .bind(id)
    .execute(pool)
    .await
    .ok()?
    .rows_affected();
    if n == 0 {
        return None;
    }
    let row = match sqlx::query(
        "SELECT person_id, douyin_no, person_name FROM anchor_claims WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    {
        Ok(Some(r)) => r,
        _ => return None,
    };
    Some((
        row.try_get::<u64, _>("person_id").ok()? as i64,
        row.try_get::<String, _>("douyin_no").ok()?,
        row.try_get::<String, _>("person_name").ok()?,
    ))
}

/// 裁决回滚：通过但账号写入失败时，把申请退回 pending，管理员可重试或改判
pub async fn anchor_claim_revert(st: &AppState, id: i64) {
    if let Ok(pool) = w(st) {
        let _ = sqlx::query(
            "UPDATE anchor_claims SET status = 'pending', decided_at = '', decided_by = ''
             WHERE id = ? AND status = 'approved'",
        )
        .bind(id)
        .execute(pool)
        .await;
    }
}

/* --------------------- 展示端「我的」页微信推送（绑定 + 追分目标） --------------------- */
/*
   2026-10-09 新增。链路：
     展示端「我的」页生成绑定码 → 用户扫码加微信机器人 → 把绑定码发给机器人
     → 私聊会话 id 与展示端账号绑定（push_bindings）→ 数据导入完成后按目标推送
   iLink 只能被动回复，所以每次私聊入站都把 to_user_id / context_token 落到 wx_sessions，
   主动推送时从这里取（服务重启不丢会话上下文）。
*/

/// 绑定关系（推送引擎用的完整行）
#[derive(Clone, Debug)]
pub struct PushBinding {
    pub name: String,
    pub team: String,
    pub person_id: u64,
    pub conversation_id: String,
    pub notify_target: bool,
    pub daily_report: bool,
}

fn now_str() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 展示端「我的」页看到的推送状态（不含会话 id 等内部字段）
pub async fn push_state(st: &AppState, name: &str) -> serde_json::Value {
    let Some(pool) = w(st).ok() else {
        return serde_json::json!({ "bound": false, "code": "", "codeExpires": "" });
    };
    let row = sqlx::query(
        "SELECT conversation_id, nickname, bind_code, code_expires, bound_at,
                notify_target, daily_report, last_push, last_error
         FROM push_bindings WHERE name = ?",
    )
    .bind(name.trim())
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let Some(r) = row else {
        return serde_json::json!({
            "bound": false, "code": "", "codeExpires": "",
            "notifyTarget": false, "dailyReport": false,
        });
    };
    let conv: String = r
        .try_get::<String, _>("conversation_id")
        .unwrap_or_default();
    serde_json::json!({
        "bound": !conv.is_empty(),
        "nickname": r.try_get::<String, _>("nickname").unwrap_or_default(),
        "code": r.try_get::<String, _>("bind_code").unwrap_or_default(),
        "codeExpires": r.try_get::<String, _>("code_expires").unwrap_or_default(),
        "boundAt": r.try_get::<String, _>("bound_at").unwrap_or_default(),
        "notifyTarget": r.try_get::<i8, _>("notify_target").unwrap_or(0) == 1,
        "dailyReport": r.try_get::<i8, _>("daily_report").unwrap_or(0) == 1,
        "lastPush": r.try_get::<String, _>("last_push").unwrap_or_default(),
        "lastError": r.try_get::<String, _>("last_error").unwrap_or_default(),
    })
}

/// 写入/刷新绑定码（保留已有绑定；重新生成只在未绑定时用于首次绑定）。
/// 已绑定时也允许生成新码（用于换微信号重绑），绑定成功会覆盖旧会话。
pub async fn push_set_code(
    st: &AppState,
    name: &str,
    team: &str,
    person_id: u64,
    code: &str,
) -> Result<()> {
    let pool = w(st)?;
    let expires = (chrono::Local::now() + chrono::Duration::minutes(30))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    sqlx::query(
        "INSERT INTO push_bindings (name, team, person_id, bind_code, code_expires, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON DUPLICATE KEY UPDATE team = VALUES(team), person_id = VALUES(person_id),
           bind_code = VALUES(bind_code), code_expires = VALUES(code_expires),
           updated_at = VALUES(updated_at)",
    )
    .bind(name.trim())
    .bind(team)
    .bind(person_id)
    .bind(code)
    .bind(expires)
    .bind(now_str())
    .execute(pool)
    .await?;
    Ok(())
}

/// 绑定码 → 绑定：把微信私聊会话接到展示端账号上。
/// 返回 Some((账号姓名, 团别))，没命中/已过期返回 None。码一次性，用掉即清。
pub async fn push_bind_by_code(
    st: &AppState,
    code: &str,
    conversation_id: &str,
    to_user_id: &str,
) -> Option<(String, String)> {
    let pool = w(st).ok()?;
    let code = code.trim().to_ascii_uppercase();
    if code.is_empty() {
        return None;
    }
    let row = sqlx::query(
        "SELECT name, team, code_expires FROM push_bindings WHERE bind_code = ? AND bind_code <> ''",
    )
    .bind(&code)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    let expires: String = row.try_get::<String, _>("code_expires").unwrap_or_default();
    if !expires.is_empty() {
        let ok = chrono::NaiveDateTime::parse_from_str(&expires, "%Y-%m-%d %H:%M:%S")
            .map(|t| t > chrono::Local::now().naive_local())
            .unwrap_or(false);
        if !ok {
            return None;
        }
    }
    let name: String = row.try_get::<String, _>("name").ok()?;
    let team: String = row.try_get::<String, _>("team").unwrap_or_default();
    let res = sqlx::query(
        "UPDATE push_bindings SET conversation_id = ?, to_user_id = ?, nickname = ?,
           bind_code = '', code_expires = '', bound_at = ?, last_error = '', updated_at = ?
         WHERE name = ?",
    )
    .bind(conversation_id)
    .bind(to_user_id)
    .bind("")
    .bind(now_str())
    .bind(now_str())
    .bind(&name)
    .execute(pool)
    .await;
    if let Err(e) = res {
        tracing::error!(error = %e, "写入微信绑定失败");
        return None;
    }
    Some((name, team))
}

/// 解绑（保留目标配置，只断开微信）
pub async fn push_unbind(st: &AppState, name: &str) -> bool {
    matches!(w(st), Ok(pool) if sqlx::query(
        "UPDATE push_bindings SET conversation_id = '', to_user_id = '', bound_at = '',
           bind_code = '', code_expires = '', updated_at = ? WHERE name = ?")
        .bind(now_str())
        .bind(name.trim())
        .execute(pool)
        .await
        .map(|r| r.rows_affected() > 0)
        .unwrap_or(false))
}

/// 按微信会话解绑（机器人里回复「解绑」）：返回被解绑的账号姓名
pub async fn push_unbind_by_conversation(st: &AppState, conversation_id: &str) -> Option<String> {
    let pool = w(st).ok()?;
    let row = sqlx::query("SELECT name FROM push_bindings WHERE conversation_id = ?")
        .bind(conversation_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()?;
    let name: String = row.try_get::<String, _>("name").ok()?;
    let _ = sqlx::query(
        "UPDATE push_bindings SET conversation_id = '', to_user_id = '', bound_at = '',
           bind_code = '', code_expires = '', updated_at = ? WHERE name = ?",
    )
    .bind(now_str())
    .bind(&name)
    .execute(pool)
    .await;
    Some(name)
}

/// 两个推送开关
pub async fn push_set_flags(
    st: &AppState,
    name: &str,
    notify_target: bool,
    daily_report: bool,
) -> bool {
    matches!(w(st), Ok(pool) if sqlx::query(
        "UPDATE push_bindings SET notify_target = ?, daily_report = ?, updated_at = ? WHERE name = ?")
        .bind(if notify_target { 1i8 } else { 0i8 })
        .bind(if daily_report { 1i8 } else { 0i8 })
        .bind(now_str())
        .bind(name.trim())
        .execute(pool)
        .await
        .map(|r| r.rows_affected() > 0)
        .unwrap_or(false))
}

/// 某团已绑定且至少开了一个推送开关的账号（推送引擎用）
pub async fn push_bindings_for_team(st: &AppState, team: &str) -> Vec<PushBinding> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    match sqlx::query(
        "SELECT name, team, person_id, conversation_id, notify_target, daily_report
         FROM push_bindings
         WHERE team = ? AND conversation_id <> '' AND (notify_target = 1 OR daily_report = 1)",
    )
    .bind(team)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| PushBinding {
                name: r.try_get::<String, _>("name").unwrap_or_default(),
                team: r.try_get::<String, _>("team").unwrap_or_default(),
                person_id: r
                    .try_get::<u64, _>("person_id")
                    .or_else(|_| r.try_get::<i64, _>("person_id").map(|v| v as u64))
                    .unwrap_or(0),
                conversation_id: r
                    .try_get::<String, _>("conversation_id")
                    .unwrap_or_default(),
                notify_target: r.try_get::<i8, _>("notify_target").unwrap_or(0) == 1,
                daily_report: r.try_get::<i8, _>("daily_report").unwrap_or(0) == 1,
            })
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "查推送绑定失败");
            Vec::new()
        }
    }
}

/// 记录一次推送结果（成功清错误，失败留原因）
pub async fn push_touch(st: &AppState, name: &str, error: &str) {
    if let Ok(pool) = w(st) {
        let _ =
            sqlx::query("UPDATE push_bindings SET last_push = ?, last_error = ? WHERE name = ?")
                .bind(now_str())
                .bind(&error.chars().take(200).collect::<String>())
                .bind(name.trim())
                .execute(pool)
                .await;
    }
}

/* --------------------------- 追分目标 --------------------------- */

/// 某账号的目标列表（展示端用）
pub async fn targets_list(st: &AppState, name: &str) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    match sqlx::query(
        "SELECT id, kind, target_person_id, target_name, target_value, hit, last_gap, created_at
         FROM my_targets WHERE name = ? ORDER BY id",
    )
    .bind(name.trim())
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.try_get::<u64, _>("id")
                        .or_else(|_| r.try_get::<i64, _>("id").map(|v| v as u64))
                        .unwrap_or(0),
                    "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                    "targetPersonId": r.try_get::<u64, _>("target_person_id")
                        .or_else(|_| r.try_get::<i64, _>("target_person_id").map(|v| v as u64))
                        .unwrap_or(0),
                    "targetName": r.try_get::<String, _>("target_name").unwrap_or_default(),
                    "targetValue": r.try_get::<i64, _>("target_value").unwrap_or(0),
                    "hit": r.try_get::<i8, _>("hit").unwrap_or(0) == 1,
                    "lastGap": r.try_get::<i64, _>("last_gap").unwrap_or(0),
                    "createdAt": r.try_get::<String, _>("created_at").unwrap_or_default(),
                })
            })
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "查追分目标失败");
            Vec::new()
        }
    }
}

/// 目标条数（上限校验用）
pub async fn target_count(st: &AppState, name: &str) -> i64 {
    let Ok(pool) = w(st) else {
        return 0;
    };
    sqlx::query("SELECT COUNT(*) AS n FROM my_targets WHERE name = ?")
        .bind(name.trim())
        .fetch_one(pool)
        .await
        .ok()
        .and_then(|r| r.try_get::<i64, _>("n").ok())
        .unwrap_or(0)
}

/// 目标行（推送引擎用的最小集合）
#[derive(Clone, Debug)]
pub struct PushTarget {
    pub id: u64,
    pub kind: String,
    pub target_person_id: u64,
    pub target_name: String,
    pub target_value: i64,
    pub hit: bool,
    pub last_notice_date: String,
}

pub async fn targets_for_push(st: &AppState, name: &str) -> Vec<PushTarget> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    match sqlx::query(
        "SELECT id, kind, target_person_id, target_name, target_value, hit, last_notice_date
         FROM my_targets WHERE name = ? ORDER BY id",
    )
    .bind(name.trim())
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| PushTarget {
                id: r
                    .try_get::<u64, _>("id")
                    .or_else(|_| r.try_get::<i64, _>("id").map(|v| v as u64))
                    .unwrap_or(0),
                kind: r.try_get::<String, _>("kind").unwrap_or_default(),
                target_person_id: r
                    .try_get::<u64, _>("target_person_id")
                    .or_else(|_| r.try_get::<i64, _>("target_person_id").map(|v| v as u64))
                    .unwrap_or(0),
                target_name: r.try_get::<String, _>("target_name").unwrap_or_default(),
                target_value: r.try_get::<i64, _>("target_value").unwrap_or(0),
                hit: r.try_get::<i8, _>("hit").unwrap_or(0) == 1,
                last_notice_date: r
                    .try_get::<String, _>("last_notice_date")
                    .unwrap_or_default(),
            })
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "查追分目标失败");
            Vec::new()
        }
    }
}

/// 新增目标（kind = person 追人 / value 追数值）
pub async fn target_add(
    st: &AppState,
    name: &str,
    team: &str,
    kind: &str,
    target_person_id: u64,
    target_name: &str,
    target_value: i64,
) -> Result<u64> {
    let pool = w(st)?;
    let r = sqlx::query(
        "INSERT INTO my_targets (name, team, kind, target_person_id, target_name, target_value, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(name.trim())
    .bind(team)
    .bind(kind)
    .bind(target_person_id)
    .bind(target_name)
    .bind(target_value)
    .bind(now_str())
    .execute(pool)
    .await?;
    Ok(r.last_insert_id())
}

/// 删除目标（只能删自己的）
pub async fn target_delete(st: &AppState, name: &str, id: u64) -> bool {
    matches!(w(st), Ok(pool) if sqlx::query("DELETE FROM my_targets WHERE id = ? AND name = ?")
        .bind(id)
        .bind(name.trim())
        .execute(pool)
        .await
        .map(|r| r.rows_affected() > 0)
        .unwrap_or(false))
}

/// 推送后回写目标状态（是否已追上 / 上次差距 / 上次提醒的业务日）
pub async fn target_set_state(st: &AppState, id: u64, hit: bool, gap: i64, notice_date: &str) {
    if let Ok(pool) = w(st) {
        let _ = sqlx::query(
            "UPDATE my_targets SET hit = ?, last_gap = ?, last_notice_date = ? WHERE id = ?",
        )
        .bind(if hit { 1i8 } else { 0i8 })
        .bind(gap)
        .bind(notice_date)
        .bind(id)
        .execute(pool)
        .await;
    }
}

/// 本团主播搜索（展示端追分选择用）：只回 id + 名字，不下发抖音号等敏感字段
pub async fn team_anchor_search(
    st: &AppState,
    source: &str,
    team: &str,
    q: &str,
    limit: i64,
) -> Vec<serde_json::Value> {
    let sql = "SELECT id, name FROM person \
               WHERE deleted_at IS NULL AND gender = ? AND status <> 'left' AND name LIKE ? \
               ORDER BY name LIMIT ?";
    let like = format!("%{}%", q.trim());
    match crate::mysql::query_with_binds(&st.pools, source, sql, |qb| {
        qb.bind(team.to_string()).bind(like.clone()).bind(limit)
    })
    .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "personId": r.try_get::<u64, _>("id")
                        .or_else(|_| r.try_get::<i64, _>("id").map(|v| v as u64))
                        .unwrap_or(0),
                    "name": r.try_get::<String, _>("name").unwrap_or_default(),
                })
            })
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "本团主播搜索失败");
            Vec::new()
        }
    }
}

/* --------------------------- 微信私聊会话上下文 --------------------------- */

/// 每次私聊入站刷新（主动推送时取 to_user_id + context_token）
pub async fn wx_session_upsert(
    st: &AppState,
    conversation_id: &str,
    to_user_id: &str,
    context_token: &str,
) {
    if let Ok(pool) = w(st) {
        let _ = sqlx::query(
            "INSERT INTO wx_sessions (conversation_id, to_user_id, context_token, updated_at)
             VALUES (?, ?, ?, ?)
             ON DUPLICATE KEY UPDATE to_user_id = VALUES(to_user_id),
               context_token = VALUES(context_token), updated_at = VALUES(updated_at)",
        )
        .bind(conversation_id)
        .bind(to_user_id)
        .bind(context_token)
        .bind(now_str())
        .execute(pool)
        .await;
    }
}

/// 取某会话的发送上下文（to_user_id, context_token）
pub async fn wx_session_get(st: &AppState, conversation_id: &str) -> Option<(String, String)> {
    let pool = w(st).ok()?;
    let r =
        sqlx::query("SELECT to_user_id, context_token FROM wx_sessions WHERE conversation_id = ?")
            .bind(conversation_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()?;
    let to_user = r.try_get::<String, _>("to_user_id").unwrap_or_default();
    let token = r.try_get::<String, _>("context_token").unwrap_or_default();
    if to_user.is_empty() || token.is_empty() {
        return None;
    }
    Some((to_user, token))
}

/* -------------------- 旧 SQLite 数据一次性搬家（migrate 触发） -------------------- */

/// 把旧 SQLite（SQLITE_PATH）里的运行时数据幂等搬进 MySQL。
/// 只搬一次：config 表 sqlite_carried=1 后直接跳过（SQLite 文件保持原样不删，作留底）。
/// 表缺列/文件不存在都按「没得搬」处理，绝不阻塞 migrate。
pub async fn carry_legacy_sqlite(pool: &MySqlPool, sqlite_path: &str) -> Result<()> {
    use sqlx::Row;

    // 1. 已搬过 → 跳过
    let carried = sqlx::query("SELECT cfg_value FROM config WHERE cfg_key = 'sqlite_carried'")
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    if let Some(r) = carried {
        if r.try_get::<String, _>(0).map(|v| v == "1").unwrap_or(false) {
            tracing::info!("旧 SQLite 数据已搬家过，跳过（config.sqlite_carried=1）");
            return Ok(());
        }
    }
    if !std::path::Path::new(sqlite_path).exists() {
        tracing::info!(path = sqlite_path, "没有旧 SQLite 文件，跳过搬家");
        return Ok(());
    }
    let conn = match rusqlite::Connection::open_with_flags(
        sqlite_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = sqlite_path, error = %e, "旧 SQLite 打不开，跳过搬家");
            return Ok(());
        }
    };
    let has_col = |table: &str, col: &str| -> bool {
        let mut stmt = match conn.prepare(&format!("PRAGMA table_info({table})")) {
            Ok(s) => s,
            Err(_) => return false,
        };
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default();
        cols.iter().any(|c| c == col)
    };
    let has_table = |table: &str| -> bool {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |_| Ok(()),
        )
        .is_ok()
    };
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // 2. aliases
    if has_table("aliases") {
        let mut stmt = conn.prepare("SELECT alias, real_name FROM aliases")?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(Result::ok)
            .collect();
        for (a, rn) in &rows {
            sqlx::query(
                "INSERT INTO aliases (alias, real_name) VALUES (?, ?)
                 ON DUPLICATE KEY UPDATE real_name = VALUES(real_name)",
            )
            .bind(&a)
            .bind(&rn)
            .execute(pool)
            .await?;
        }
        tracing::info!(n = rows.len(), "搬家：aliases");
    }

    // 3. roster_state
    if has_table("roster_state") {
        let mut stmt = conn.prepare("SELECT event_type, text, updated_at FROM roster_state")?;
        let rows: Vec<(String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .filter_map(Result::ok)
            .collect();
        for (et, body, at) in &rows {
            sqlx::query(
                "INSERT INTO roster_state (event_type, body, updated_at) VALUES (?, ?, ?)
                 ON DUPLICATE KEY UPDATE body = VALUES(body), updated_at = VALUES(updated_at)",
            )
            .bind(&et)
            .bind(&body)
            .bind(&at)
            .execute(pool)
            .await?;
        }
        tracing::info!(n = rows.len(), "搬家：roster_state");
    }

    // 4. config（跳过搬家标记自身）
    if has_table("config") {
        let mut stmt = conn.prepare("SELECT key, value FROM config")?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(Result::ok)
            .collect();
        for (k, v) in &rows {
            if k == "sqlite_carried" {
                continue;
            }
            sqlx::query(
                "INSERT INTO config (cfg_key, cfg_value, updated_at) VALUES (?, ?, ?)
                 ON DUPLICATE KEY UPDATE cfg_value = VALUES(cfg_value), updated_at = VALUES(updated_at)",
            )
            .bind(&k)
            .bind(&v)
            .bind(&now)
            .execute(pool)
            .await?;
        }
        tracing::info!(n = rows.len(), "搬家：config");
    }

    // 5. bot_sessions
    if has_table("bot_sessions") {
        let mut stmt = conn.prepare(
            "SELECT channel, conversation_id, kind, IFNULL(last_text,''), last_seen, is_admin FROM bot_sessions",
        )?;
        let rows: Vec<(String, String, String, String, String, i64)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .filter_map(Result::ok)
            .collect();
        for (c, cid, kind, lt, ls, adm) in &rows {
            sqlx::query(
                "INSERT INTO bot_sessions (channel, conversation_id, kind, last_text, last_seen, is_admin)
                 VALUES (?, ?, ?, ?, ?, ?)
                 ON DUPLICATE KEY UPDATE kind = VALUES(kind), last_text = VALUES(last_text),
                   last_seen = VALUES(last_seen), is_admin = VALUES(is_admin)",
            )
            .bind(&c)
            .bind(&cid)
            .bind(&kind)
            .bind(&lt)
            .bind(&ls)
            .bind(*adm as i8)
            .execute(pool)
            .await?;
        }
        tracing::info!(n = rows.len(), "搬家：bot_sessions");
    }

    // 6. plans（published 列在老库里可能没有）
    if has_table("plans") {
        let pub_col = has_col("plans", "published");
        let et_col = has_col("plans", "event_type");
        let sql = if pub_col && et_col {
            "SELECT id, title, metric, days, payload, created_at, published, event_type FROM plans"
                .to_string()
        } else if et_col {
            "SELECT id, title, metric, days, payload, created_at, 0, event_type FROM plans"
                .to_string()
        } else {
            "SELECT id, title, metric, days, payload, created_at, 0, '' FROM plans".to_string()
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows: Vec<(i64, String, String, i64, String, String, i64, String)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            })?
            .filter_map(Result::ok)
            .collect();
        for (id, title, metric, days, payload, created, published, et) in &rows {
            sqlx::query(
                "INSERT INTO plans (id, title, metric, days, payload, created_at, published, event_type)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                 ON DUPLICATE KEY UPDATE title = VALUES(title), metric = VALUES(metric),
                   days = VALUES(days), payload = VALUES(payload), created_at = VALUES(created_at),
                   published = VALUES(published), event_type = VALUES(event_type)",
            )
            .bind(id)
            .bind(&title)
            .bind(&metric)
            .bind(days)
            .bind(&payload)
            .bind(&created)
            .bind(*published as i8)
            .bind(&et)
            .execute(pool)
            .await?;
        }
        tracing::info!(n = rows.len(), "搬家：plans");
    }

    // 7. my_accounts（is_admin 列可能没有）
    if has_table("my_accounts") {
        let admin_col = has_col("my_accounts", "is_admin");
        let sql = if admin_col {
            "SELECT name, phone, pass, is_admin, created_at FROM my_accounts".to_string()
        } else {
            "SELECT name, phone, pass, 0, created_at FROM my_accounts".to_string()
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows: Vec<(String, String, String, i64, String)> = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .filter_map(Result::ok)
            .collect();
        for (name, phone, pass, is_admin, created) in &rows {
            sqlx::query(
                "INSERT INTO my_accounts (name, phone, pass, is_admin, created_at)
                 VALUES (?, ?, ?, ?, ?)
                 ON DUPLICATE KEY UPDATE phone = VALUES(phone), pass = VALUES(pass),
                   is_admin = VALUES(is_admin), created_at = VALUES(created_at)",
            )
            .bind(&name)
            .bind(&phone)
            .bind(&pass)
            .bind(*is_admin as i8)
            .bind(&created)
            .execute(pool)
            .await?;
        }
        tracing::info!(n = rows.len(), "搬家：my_accounts");
    }

    // 8. anchor_claims（截图 BLOB 原样搬）
    if has_table("anchor_claims") {
        let mut stmt = conn.prepare(
            "SELECT id, person_id, person_name, team, douyin_no, screenshot, mime, status,
                    IFNULL(reject_reason,''), created_at, IFNULL(decided_at,''), IFNULL(decided_by,'')
             FROM anchor_claims",
        )?;
        let rows: Vec<(
            i64,
            i64,
            String,
            String,
            String,
            Vec<u8>,
            String,
            String,
            String,
            String,
            String,
            String,
        )> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                ))
            })?
            .filter_map(Result::ok)
            .collect();
        for row in &rows {
            sqlx::query(
                "INSERT INTO anchor_claims
                   (id, person_id, person_name, team, douyin_no, screenshot, mime, status,
                    reject_reason, created_at, decided_at, decided_by)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON DUPLICATE KEY UPDATE person_id = VALUES(person_id), person_name = VALUES(person_name),
                   team = VALUES(team), douyin_no = VALUES(douyin_no), screenshot = VALUES(screenshot),
                   mime = VALUES(mime), status = VALUES(status), reject_reason = VALUES(reject_reason),
                   created_at = VALUES(created_at), decided_at = VALUES(decided_at), decided_by = VALUES(decided_by)",
            )
            .bind(row.0)
            .bind(row.1)
            .bind(&row.2)
            .bind(&row.3)
            .bind(&row.4)
            .bind(&row.5)
            .bind(&row.6)
            .bind(&row.7)
            .bind(&row.8)
            .bind(&row.9)
            .bind(&row.10)
            .bind(&row.11)
            .execute(pool)
            .await?;
        }
        tracing::info!(n = rows.len(), "搬家：anchor_claims");
    }

    // 9. 落搬家标记
    sqlx::query(
        "INSERT INTO config (cfg_key, cfg_value, updated_at) VALUES ('sqlite_carried', '1', ?)
         ON DUPLICATE KEY UPDATE cfg_value = '1', updated_at = VALUES(updated_at)",
    )
    .bind(&now)
    .execute(pool)
    .await?;
    tracing::info!("旧 SQLite 运行时数据已全部并入 MySQL（sqlite_carried=1）");
    Ok(())
}

/* --------------------------------- 单元测试（纯函数） --------------------------------- */

#[cfg(test)]
mod tests {
    use super::*;

    /// 赛事类型归一化：白名单内的值原样返回，老值（见 LEGACY_EVENT_RENAMES）归一到新值
    #[test]
    fn normalize_event_type_whitelists() {
        for t in EVENT_TYPES {
            assert_eq!(normalize_event_type(t), t);
        }
        assert_eq!(
            normalize_event_type(" 初级争霸赛小组赛 "),
            "初级争霸赛小组赛"
        );
        assert_eq!(normalize_event_type("淘汰赛"), "");
        assert_eq!(normalize_event_type(""), "");
    }

    /// 改名归一：老值一律映射到新值，不是被丢掉
    #[test]
    fn normalize_event_type_maps_legacy_names() {
        for (old, new) in LEGACY_EVENT_RENAMES {
            assert_eq!(normalize_event_type(old), new, "{old} 应归一到 {new}");
            assert!(EVENT_TYPES.contains(&new), "{new} 必须在新白名单里");
        }
    }

    /// 抖音号格式：6~20 位字母/数字/下划线/点
    #[test]
    fn douyin_no_validation() {
        assert!(validate_douyin_no("wolves_99").is_ok());
        assert!(
            validate_douyin_no(" ab12. ").is_err(),
            "trim 后不足 6 位 → 拒绝"
        );
        assert!(validate_douyin_no("has space1").is_err(), "含空格 → 拒绝");
        assert!(validate_douyin_no("中文抖音号").is_err(), "中文 → 拒绝");
        assert!(validate_douyin_no(&"a".repeat(21)).is_err(), "超长 → 拒绝");
    }

    /// 密码规则：4~20 位字母或数字（2026-10-10 放宽）；符号/空格/太短仍拒绝
    #[test]
    fn my_account_password_rule() {
        assert!(validate_my_account("shuai4444", "shuai4444").is_ok());
        assert!(validate_my_account("狼哲", "13312345678").is_ok());
        assert!(
            validate_my_account("啸帆", "133-1234-5678").is_err(),
            "带横线 → 拒绝"
        );
        assert!(
            validate_my_account("啸帆", "ab_123").is_err(),
            "带下划线 → 拒绝"
        );
        assert!(
            validate_my_account("啸帆", "123").is_err(),
            "少于 4 位 → 拒绝"
        );
        assert!(validate_my_account("", "13312345678").is_err());
        assert!(validate_my_account("啸帆", "").is_err());
    }
}
