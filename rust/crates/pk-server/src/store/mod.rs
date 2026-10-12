//! store 子模块：运行时状态表（PG）数据访问层。
//!
//! 拆分自单体 store.rs（按业务域）：
//! - [`self`]（mod.rs）别名 / 名单 / 分组方案 / 配置项 / 机器人会话 / 工具
//! - [`my_account`] 展示端账号：台账、批量开号、登录、注册
//! - [`bind_requests`] 绑定申请审核 + 抖音号绑定审计
//! - [`wxpush`] 「我的」页微信推送 / 手机号绑定 / 扫码账号
//! - [`push_jobs`] 延迟推送队列 + 追分目标
//! - [`wx_sessions`] 微信私聊会话上下文
//! - [`legacy`] 旧 SQLite 数据一次性搬家
//!
//! 对外 API 不变：全部通过本模块 re-export（`store::xxx` 照旧可用）。
//! 运行时状态存储（2026-10-10 单库化，并入 MySQL）：别名 / 分组方案 / 名单草稿 /
//! 配置 / 机器人会话 / 展示端账号 / 抖音号绑定审计。
//!
//! 这批表早期放在本地 SQLite（rusqlite 同步 API），单库决策（2026-10-10）后统一并入
//! 业务库（本机 douyin_dev / 生产 douyin），与业务事实源同库——部署时 `upload-db` 一次带全，
//! 生产不再需要单独维护运行时数据。
//!
//! - 读走 douyin_writer 写池（表都很小，读写分离没有意义；writer 缺失时读返回空、写报错）
//! - 旧 SQLite 文件只作为一次性搬家来源：`pk-server migrate` 里调
//!   [`carry_legacy_sqlite`] 把数据幂等搬进来（config 表 sqlite_carried=1 后跳过）
//! - 表结构见 migrations/003_runtime.sql；列名差异（key→cfg_key、text→body）都是方言原因


mod bind_requests;
mod legacy;
pub mod my_account;
mod push_jobs;
mod wx_sessions;
mod wxpush;

pub use bind_requests::*;
pub use legacy::*;
pub use my_account::*;
pub use push_jobs::*;
pub use wx_sessions::*;
pub use wxpush::*;


use anyhow::Result;
use sqlx::PgPool;
use sqlx::Row;

use crate::state::AppState;
use crate::pgrs::PgResultExt;

/* ------------------------------ 内部工具 ------------------------------ */

/// writer 池。运行时表都在业务库，读写统一走 writer（缺配置时读返回空、写报错）。
fn w(st: &AppState) -> Result<&PgPool> {
    st.writer
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("写库不可用：douyin_writer 未配置"))
}

/// 赛事类型白名单（2026-10-09 用户定名）：
/// - 初级争霸赛小组赛（月初）、中级争霸赛小组赛（月中）—— 白天的分组赛，第 N 组后打一场 20 分钟复活赛
/// - 月终争霸赛（月末）—— 只分组，没有小组赛 / 复活赛 / 晋级赛流程
/// - 初级争霸赛晋级赛 / 中级争霸赛晋级赛 —— 晚上晋级赛，按级别拆两页
///
/// 值直接用中文落库，前后端共用同一套字符串，避免 key/文案两套映射各错各的。
pub const EVENT_TYPES: [&str; 5] = [
    "初级争霸赛小组赛",
    "中级争霸赛小组赛",
    "月终争霸赛",
    "初级争霸赛晋级赛",
    "中级争霸赛晋级赛",
];

/// 改名前的赛事类型 → 现用赛事类型（2026-10-09 改名，改名前列的值要认）。
/// 万一漏网，`normalize_event_type` 会在读取侧归一。
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
pub(crate) fn now_str() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

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
         ON CONFLICT (alias) DO UPDATE SET real_name = EXCLUDED.real_name",
    )
    .bind(alias)
    .bind(real_name)
    .execute(w(st)?)
    .await?;
    Ok(())
}

pub async fn delete_alias(st: &AppState, alias: &str) -> Result<()> {
    sqlx::query(&crate::pq::rewrite("DELETE FROM aliases WHERE alias = ?"))
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
    let row = match sqlx::query(&crate::pq::rewrite("SELECT body, updated_at FROM roster_state WHERE event_type = ?"))
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
             ON CONFLICT (event_type) DO UPDATE SET body = EXCLUDED.body, updated_at = EXCLUDED.updated_at",
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

fn map_plan(r: &sqlx::postgres::PgRow) -> Result<PlanRow> {
    Ok(PlanRow {
        id: r.try_get::<i64, _>(0)? as i64,
        title: r.try_get::<String, _>(1)?,
        metric: r.try_get::<String, _>(2)?,
        days: r.try_get::<i64, _>(3)?,
        payload: r.try_get::<String, _>(4)?,
        created_at: r.try_get::<String, _>(5)?,
        published: r.try_get::<i16, _>(6)? != 0,
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
    let row = sqlx::query(&crate::pq::rewrite(
        "INSERT INTO plans (title, metric, days, payload, created_at, published, event_type)
         VALUES (?, ?, ?, ?, ?, 0, ?) RETURNING id",
    ))
    .bind(title)
    .bind(metric)
    .bind(days)
    .bind(payload)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(normalize_event_type(event_type))
    .fetch_one(w(st)?)
    .await?;
    Ok(row.try_get::<i64, _>(0)?)
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
    let row = match sqlx::query(&crate::pq::rewrite(&format!("SELECT {PLAN_COLS} FROM plans WHERE id = ?")))
        .bind(id)
        .fetch_optional(pool)
        .await
    {
        Ok(Some(r)) => r,
        _ => return None,
    };
    map_plan(&row).ok()
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
    match sqlx::query(&crate::pq::rewrite("UPDATE plans SET published = ? WHERE id = ?"))
        .bind(if published { 1i16 } else { 0i16 })
        .bind(id)
        .execute(&mut *tx)
        .await
    {
        Ok(res) => {
            if tx.commit().await.is_err() {
                return false;
            }
            res.affected() > 0
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
    sqlx::query(&crate::pq::rewrite("DELETE FROM plans WHERE id = ?"))
        .bind(id)
        .execute(w(st)?)
        .await?;
    Ok(())
}

/* ------------------------------- 配置项 ------------------------------- */

/// 读配置项（机器人 token 等跨重启保留的状态）；读不到/出错 → None
pub async fn config_get(st: &AppState, key: &str) -> Option<String> {
    let pool = w(st).ok()?;
    sqlx::query(&crate::pq::rewrite("SELECT cfg_value FROM config WHERE cfg_key = ?"))
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()?
        .into_iter()
        .next()
        .and_then(|r| r.try_get::<String, _>(0).ok())
}

/// 写配置项（出错只记日志——配置写失败不应打断主流程）
pub async fn config_set(st: &AppState, key: &str, value: &str) {
    let res = async {
        let pool = w(st)?;
        sqlx::query(
            "INSERT INTO config (cfg_key, cfg_value, updated_at) VALUES (?, ?, ?)
             ON CONFLICT (cfg_key) DO UPDATE SET cfg_value = EXCLUDED.cfg_value, updated_at = EXCLUDED.updated_at",
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
             ON CONFLICT (channel, conversation_id) DO UPDATE SET kind = EXCLUDED.kind, last_text = EXCLUDED.last_text, last_seen = EXCLUDED.last_seen",
        )
        .bind(channel)
        .bind(conversation_id)
        .bind(kind)
        .bind(last_text.chars().take(100).collect::<String>())
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
        sqlx::query(&crate::pq::rewrite("SELECT is_admin FROM bot_sessions WHERE channel = ? AND conversation_id = ?"))
            .bind(channel)
            .bind(conversation_id)
            .fetch_optional(pool)
            .await,
        Ok(Some(r)) if r.try_get::<i16, _>(0).unwrap_or(0) == 1
    )
}

/// 会话列表（管理界面展示）
pub async fn session_list(st: &AppState) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    match sqlx::query(
        "SELECT channel, conversation_id, kind, COALESCE(last_text,''), last_seen, is_admin
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
                    "isAdmin": r.try_get::<i16, _>(5).unwrap_or(0) == 1,
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
        .bind(if is_admin { 1i16 } else { 0i16 })
        .bind(channel)
        .bind(conversation_id)
        .execute(pool)
        .await;
    }
}

/* --------------------------- 抖音号绑定校验（纯函数） --------------------------- */


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

    /* --------------------- 一人一 bot 查重（018，2026-10-12） --------------------- */

    fn wx_row(name: &str, pid: u64, uid: &str) -> (String, u64, String) {
        (name.to_string(), pid, uid.to_string())
    }

    #[test]
    fn wx_push_clash_rejects_same_wechat_on_another_account() {
        let others = vec![wx_row("浩龙", 1001, "wxid_owner")];
        let hit = wx_push_clash_of("玖玉", 1002, "wxid_owner", &others);
        assert!(hit.is_some(), "同一个微信被第二个账号扫到 → 拒绝");
        assert!(hit.unwrap().contains("浩龙"), "文案要指出撞的是谁的账号");
    }

    #[test]
    fn wx_push_clash_rejects_second_bot_for_same_person() {
        let others = vec![wx_row("浩龙", 1001, "wxid_a")];
        let hit = wx_push_clash_of("浩龙副号", 1001, "wxid_b", &others);
        assert!(hit.is_some(), "同一主播挂第二个 bot → 拒绝");
    }

    #[test]
    fn wx_push_clash_ignores_self_update() {
        let others = vec![wx_row("浩龙", 1001, "wxid_owner")];
        // supervisor_loop 会反复 save 同一个账号：自己更新自己绝不能被判成冲突
        assert!(
            wx_push_clash_of("浩龙", 1001, "wxid_owner", &others).is_none(),
            "自己更新自己不算冲突（否则幂等性被破坏）"
        );
    }

    #[test]
    fn wx_push_clash_ignores_sentinel_values() {
        // person_id=0（未绑定 / 管理员观看账号）与空 ilink_user_id 都不参与唯一（015 哨兵语义）
        let others = vec![wx_row("管理员A", 0, ""), wx_row("管理员B", 0, "")];
        assert!(
            wx_push_clash_of("管理员C", 0, "", &others).is_none(),
            "哨兵值不互相冲突，否则第二个人扫不了码"
        );
        // 空 ilink_user_id 的两个已激活账号也不能互挡
        let legacy = vec![wx_row("老账号", 1001, "")];
        assert!(wx_push_clash_of("新账号", 1002, "", &legacy).is_none());
    }

    #[test]
    fn wx_push_clash_allows_distinct_person_and_wechat() {
        let others = vec![wx_row("浩龙", 1001, "wxid_a")];
        assert!(
            wx_push_clash_of("玖玉", 1002, "wxid_b", &others).is_none(),
            "不同人不同微信 → 正常放行"
        );
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

#[cfg(test)]
mod register_tests {
    use super::*;

    /// 注册密码 = 短信验证码：必须是 4~20 位纯数字（用户口径「验证码数字就是密码」）
    #[test]
    fn my_code_is_digits_only() {
        assert_eq!(validate_my_code("246810").unwrap(), "246810");
        assert_eq!(validate_my_code(" 1357 ").unwrap(), "1357");
        assert!(validate_my_code("").is_err(), "空验证码不给过");
        assert!(validate_my_code("123").is_err(), "太短不给过");
        assert!(
            validate_my_code("123456789012345678901").is_err(),
            "太长不给过"
        );
        assert!(
            validate_my_code("12a4").is_err(),
            "字母不给过（手机验证码是数字）"
        );
    }

    /// 手机号：5~20 位数字，允许空格/横线（归一化后）——与管理员手建账号同一套口径
    #[test]
    fn my_phone_normalizes_like_admin_path() {
        assert_eq!(validate_my_phone("199 0000 0001").unwrap(), "19900000001");
        assert_eq!(validate_my_phone("199-0000-0001").unwrap(), "19900000001");
        assert_eq!(validate_my_phone("").unwrap(), "", "空串 = 没填手机号");
        assert!(validate_my_phone("1234").is_err(), "少于 5 位不给过");
        assert!(validate_my_phone("199abc").is_err(), "非数字不给过");
    }

    /* ---------------- 手机号 + 批量开号（016，docs/13）---------------- */

    /// 初始密码 = CSPRNG 随机（2026-10-12 审计修复，废弃手机号后四位）：
    /// 10 位无混淆字符，能通过账号密码校验（4~20 位字母数字）
    #[test]
    fn initial_password_is_random_and_valid() {
        let a = initial_password();
        let b = initial_password();
        assert_eq!(a.len(), 10);
        assert_ne!(a, b, "两次生成不应相同");
        assert!(
            validate_my_account("啸帆", &a).is_ok(),
            "随机密码必须过账号校验"
        );
    }

    /// 绑定申请状态机：只有 pending 能被裁决（防重复审批 / 已裁决改写）
    #[test]
    fn bind_request_only_pending_is_decidable() {
        assert_eq!(bind_request_next_status("pending", true), Some("approved"));
        assert_eq!(bind_request_next_status("pending", false), Some("rejected"));
        assert_eq!(bind_request_next_status("approved", true), None);
        assert_eq!(bind_request_next_status("rejected", false), None);
        assert_eq!(bind_request_next_status("", true), None);
    }

    /// 台账手机号与登录账号手机号必须同一套格式口径（docs/13 §2.2）
    #[test]
    fn person_phone_uses_same_validator_as_account() {
        assert_eq!(validate_my_phone("13354463148").unwrap(), "13354463148");
        assert!(validate_my_phone("133").is_err(), "太短");
        assert!(validate_my_phone("1335446314a").is_err(), "非数字");
        // 空串 = 清空手机号（运营登记错了要能撤）
        assert_eq!(validate_my_phone("").unwrap(), "");
    }

    /// 一次批量要装得下全量在职主播（生产实测 104 人），又不能大到拖垮写池
    #[test]
    fn batch_account_max_fits_full_roster() {
        // 写在 const 块里：这是编译期就能定的性质（clippy::assertions_on_constants）
        const {
            assert!(BATCH_ACCOUNT_MAX >= 104);
            assert!(BATCH_ACCOUNT_MAX <= 2000);
        }
    }

    /// 占用文案要能区分「撞台账」和「撞登录账号」——运营靠它判断去哪改
    #[test]
    fn phone_clash_messages_are_distinct() {
        let p = PhoneClash::Person.message("13354463148");
        let a = PhoneClash::Account.message("13354463148");
        assert!(p.contains("其它在职主播"));
        assert!(a.contains("另一个展示端账号"));
        assert_ne!(p, a);
    }

    /// 没绑主播时不要把手机号当姓名下发（水印/我的页按 bind_name 判）
    #[test]
    fn bind_name_hides_phone_placeholder() {
        let unbound = MyAccountInfo {
            name: "19900000001".into(),
            phone: "19900000001".into(),
            is_admin: false,
            bound: false,
            bound_at: String::new(),
            source: "register".into(),
        };
        assert_eq!(unbound.bind_name(), "");
        let bound = MyAccountInfo {
            name: "啸帆".into(),
            bound: true,
            ..unbound
        };
        assert_eq!(bound.bind_name(), "啸帆");
    }
}
