//! WorkBuddy 展示端账号绑定状态。
//!
//! 复用 `config` 表，不新增迁移：`wb_bind_md5:<展示端账号名 md5>` 存绑定账号，
//! `wb_uid_owner_md5:<WorkBuddy uid md5>` 存 uid 归属，`wb_oauth_state_md5:<state md5>`
//! 存一笔待完成授权的归属，`wb_reauth_prompt_md5:<展示端账号名 md5>` 存重新登录
//! 提示时间（Unix 秒）。键里的账号名、uid、state 都先 md5，避免超过
//! `config.cfg_key VARCHAR(64)` 或把用户信息写进配置键。

use crate::state::AppState;
use anyhow::Result;
use chrono::{Local, Utc};
use md5::{Digest, Md5};
use serde_json::{json, Value};
use sqlx::PgPool;
use sqlx::Row;

fn w(st: &AppState) -> Result<&PgPool> {
    st.writer
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("写库不可用：douyin_writer 未配置"))
}

fn digest(raw: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(raw.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn bind_key(account_name: &str) -> String {
    format!("wb_bind_md5:{}", digest(account_name.trim()))
}

fn uid_owner_key(uid: &str) -> String {
    format!("wb_uid_owner_md5:{}", digest(uid.trim()))
}

fn state_key(state: &str) -> String {
    format!("wb_oauth_state_md5:{}", digest(state.trim()))
}

fn reauth_prompt_key(account_name: &str) -> String {
    format!("wb_reauth_prompt_md5:{}", digest(account_name.trim()))
}

fn random_generation(uid: &str) -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| anyhow::anyhow!("WorkBuddy 绑定代次生成失败：{e}"))?;
    Ok(format!(
        "{}:{}",
        uid,
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

/// 重新登录提示的冷却窗口。正好满 7 天允许再次提示，与 SQL 的阈值判断保持一致。
const REAUTH_PROMPT_COOLDOWN_SECS: i64 = 7 * 24 * 60 * 60;

const CLAIM_REAUTH_PROMPT_SQL: &str =
    "INSERT INTO config (cfg_key, cfg_value, updated_at) VALUES (?, ?, ?)
     ON CONFLICT (cfg_key) DO UPDATE
     SET cfg_value = EXCLUDED.cfg_value, updated_at = EXCLUDED.updated_at
     WHERE CASE
         WHEN config.cfg_value ~ '^[0-9]{1,18}$' THEN config.cfg_value::bigint
         ELSE 0
     END <= ?
     RETURNING cfg_key";

fn reauth_prompt_due(last_prompt_at: Option<i64>, now: i64) -> bool {
    last_prompt_at.is_none_or(|last| now.saturating_sub(last) >= REAUTH_PROMPT_COOLDOWN_SECS)
}

fn now_text() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

async fn config_get_raw(st: &AppState, key: &str) -> Option<String> {
    let pool = w(st).ok()?;
    sqlx::query(&crate::pq::rewrite(
        "SELECT cfg_value FROM config WHERE cfg_key = ?",
    ))
    .bind(key)
    .fetch_optional(pool)
    .await
    .ok()?
    .and_then(|row| row.try_get::<String, _>(0).ok())
}

async fn config_set_raw(st: &AppState, key: &str, value: &str) -> Result<()> {
    let pool = w(st)?;
    sqlx::query(&crate::pq::rewrite(
        "INSERT INTO config (cfg_key, cfg_value, updated_at) VALUES (?, ?, ?)
         ON CONFLICT (cfg_key) DO UPDATE SET cfg_value = EXCLUDED.cfg_value, updated_at = EXCLUDED.updated_at",
    ))
    .bind(key)
    .bind(value)
    .bind(now_text())
    .execute(pool)
    .await?;
    Ok(())
}

/// 只读判断当前是否处于重新登录提示窗口，不写标记。被密码/资料/公告等
/// 遮罩挡住的前端会反复调用 status，不能在这里消费 7 天窗口。
async fn reauth_prompt_window_open_at(st: &AppState, account_name: &str, now: i64) -> Result<bool> {
    let account_name = account_name.trim();
    if account_name.is_empty() {
        return Ok(false);
    }
    let pool = w(st)?;
    let raw = sqlx::query(&crate::pq::rewrite(
        "SELECT cfg_value FROM config WHERE cfg_key = ?",
    ))
    .bind(reauth_prompt_key(account_name))
    .fetch_optional(pool)
    .await?
    .and_then(|row| row.try_get::<String, _>(0).ok());
    let last_prompt_at = raw.and_then(|value| value.parse::<i64>().ok());
    Ok(reauth_prompt_due(last_prompt_at, now))
}

/// 状态接口专用：只读，不消费提示窗口。
pub async fn reauth_prompt_window_open(st: &AppState, account_name: &str) -> Result<bool> {
    reauth_prompt_window_open_at(st, account_name, Utc::now().timestamp()).await
}

/// 原子认领一次重新登录提示。只有配置项不存在，或上次提示已满 7 天时，
/// 本条 SQL 才会更新并返回一行；并发请求里最多一个能拿到 `true`。
async fn claim_reauth_prompt_at(st: &AppState, account_name: &str, now: i64) -> Result<bool> {
    let account_name = account_name.trim();
    if account_name.is_empty() {
        return Ok(false);
    }
    let pool = w(st)?;
    claim_reauth_prompt_executor(pool, account_name, now).await
}

async fn claim_reauth_prompt_executor<'e, E>(
    executor: E,
    account_name: &str,
    now: i64,
) -> Result<bool>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let claimed = sqlx::query(&crate::pq::rewrite(CLAIM_REAUTH_PROMPT_SQL))
        .bind(reauth_prompt_key(account_name))
        .bind(now.to_string())
        .bind(now_text())
        .bind(now.saturating_sub(REAUTH_PROMPT_COOLDOWN_SECS))
        .fetch_optional(executor)
        .await?
        .is_some();
    Ok(claimed)
}

/// 服务端持久化、且与 Redis 无关的重新登录提示闸门。
pub async fn claim_reauth_prompt(st: &AppState, account_name: &str) -> Result<bool> {
    claim_reauth_prompt_at(st, account_name, Utc::now().timestamp()).await
}

/// 重登 start 专用：在绑定行锁内确认绑定的 generation 仍是请求开始时读到的那一版，
/// 再原子认领提示窗口。若期间已有另一次授权成功重绑并清了标记，本请求不得把标记写回来。
pub async fn claim_reauth_prompt_for_binding(
    st: &AppState,
    account_name: &str,
    expected_binding: &Value,
) -> Result<bool> {
    let account_name = account_name.trim();
    if account_name.is_empty() {
        return Ok(false);
    }
    let pool = w(st)?;
    let mut tx = pool.begin().await?;
    let current_raw = sqlx::query(&crate::pq::rewrite(
        "SELECT cfg_value FROM config WHERE cfg_key = ? FOR UPDATE",
    ))
    .bind(bind_key(account_name))
    .fetch_optional(&mut *tx)
    .await?
    .and_then(|row| row.try_get::<String, _>(0).ok());
    let current = current_raw.and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
    if current.as_ref() != Some(expected_binding) {
        tx.rollback().await?;
        return Ok(false);
    }
    let claimed =
        claim_reauth_prompt_executor(&mut *tx, account_name, Utc::now().timestamp()).await?;
    tx.commit().await?;
    Ok(claimed)
}

/// 绑定落库与清提示标记必须在同一事务里，避免绑定已成功但接口报错、标记残留。
async fn save_binding_and_clear_reauth_prompt_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_name: &str,
    binding: &Value,
) -> Result<()> {
    sqlx::query(&crate::pq::rewrite(
        "INSERT INTO config (cfg_key, cfg_value, updated_at) VALUES (?, ?, ?)
         ON CONFLICT (cfg_key) DO UPDATE SET cfg_value = EXCLUDED.cfg_value, updated_at = EXCLUDED.updated_at",
    ))
    .bind(bind_key(account_name))
    .bind(binding.to_string())
    .bind(now_text())
    .execute(&mut **tx)
    .await?;
    sqlx::query(&crate::pq::rewrite("DELETE FROM config WHERE cfg_key = ?"))
        .bind(reauth_prompt_key(account_name))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// 授权成功落定绑定后清掉提示标记；错误交回调用方，避免假成功。
pub async fn clear_reauth_prompt(st: &AppState, account_name: &str) -> Result<()> {
    let account_name = account_name.trim();
    if account_name.is_empty() {
        return Ok(());
    }
    let pool = w(st)?;
    sqlx::query(&crate::pq::rewrite("DELETE FROM config WHERE cfg_key = ?"))
        .bind(reauth_prompt_key(account_name))
        .execute(pool)
        .await?;
    Ok(())
}

async fn config_delete_raw(st: &AppState, key: &str) {
    if let Ok(pool) = w(st) {
        let _ = sqlx::query(&crate::pq::rewrite("DELETE FROM config WHERE cfg_key = ?"))
            .bind(key)
            .execute(pool)
            .await;
    }
}

fn parse_json(raw: Option<String>) -> Option<Value> {
    serde_json::from_str(&raw?).ok().filter(Value::is_object)
}

pub fn public_binding(v: &Value) -> Value {
    json!({
        "uid": v.get("uid").and_then(Value::as_str).unwrap_or_default(),
        "nickname": v.get("nickname").and_then(Value::as_str).unwrap_or_default(),
        "realm": v.get("realm").and_then(Value::as_str).unwrap_or_default(),
        "domain": v.get("domain").and_then(Value::as_str).unwrap_or_default(),
        "platform": v.get("platform").and_then(Value::as_str).unwrap_or_default(),
        "accountName": v.get("accountName").and_then(Value::as_str).unwrap_or_default(),
        "boundAt": v.get("boundAt").and_then(Value::as_str).unwrap_or_default(),
    })
}

pub async fn binding_for(st: &AppState, account_name: &str) -> Option<Value> {
    let raw = config_get_raw(st, &bind_key(account_name)).await;
    parse_json(raw)
}

pub async fn all_bindings(st: &AppState) -> std::collections::HashMap<String, Value> {
    let mut out = std::collections::HashMap::new();
    let Ok(pool) = w(st) else {
        return out;
    };
    let Ok(rows) = sqlx::query("SELECT cfg_value FROM config WHERE cfg_key LIKE 'wb_bind_md5:%'")
        .fetch_all(pool)
        .await
    else {
        return out;
    };
    for row in rows {
        let Some(raw) = row.try_get::<String, _>(0).ok() else {
            continue;
        };
        let Some(account) = parse_json(Some(raw)) else {
            continue;
        };
        if let Some(name) = account
            .get("accountName")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            out.insert(name.to_string(), account);
        }
    }
    out
}

pub async fn save_oauth_state(
    st: &AppState,
    state: &str,
    account_name: &str,
    person_id: u64,
    realm: &str,
) -> Result<()> {
    let payload = json!({
        "accountName": account_name.trim(),
        "personId": person_id,
        "realm": realm,
        "createdAt": now_text(),
    });
    config_set_raw(st, &state_key(state), &payload.to_string()).await
}

pub async fn oauth_state_owner(st: &AppState, state: &str) -> Option<String> {
    let value = parse_json(config_get_raw(st, &state_key(state)).await)?;
    value
        .get("accountName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(String::from)
}

pub async fn clear_oauth_state(st: &AppState, state: &str) {
    config_delete_raw(st, &state_key(state)).await;
}

/// 成功授权后落定绑定关系；返回 None 表示这个 WorkBuddy uid 已被别的展示端账号占用。
pub async fn commit_binding(
    st: &AppState,
    account_name: &str,
    person_id: u64,
    account: &Value,
) -> Result<Option<Value>> {
    let account_name = account_name.trim();
    let uid = account
        .get("uid")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|uid| !uid.is_empty())
        .ok_or_else(|| anyhow::anyhow!("WorkBuddy 账号缺少 uid"))?
        .to_string();
    let generation = random_generation(&uid)?;
    let binding = json!({
        "uid": uid,
        "nickname": account.get("nickname").and_then(Value::as_str).unwrap_or_default(),
        "realm": account.get("realm").and_then(Value::as_str).unwrap_or_default(),
        "domain": account.get("domain").and_then(Value::as_str).unwrap_or_default(),
        "platform": account.get("platform").and_then(Value::as_str).unwrap_or_default(),
        "accountName": account_name,
        "boundAt": now_text(),
        "generation": generation,
    });

    let pool = w(st)?;
    let mut tx = pool.begin().await?;
    // 同一展示端账号的首次绑定必须串行：否则两个不同 UID 会同时看到“未绑定”，
    // 各自占用 uid owner 后相互覆盖绑定，留下一个永久孤儿 UID。
    // 使用哈希后的绑定键作为 advisory lock key，避免把账号名写进锁标识；
    // 极端哈希冲突只会让两个账号短暂串行，不影响正确性。
    sqlx::query(&crate::pq::rewrite(
        "SELECT pg_advisory_xact_lock(hashtextextended(?, 0))",
    ))
    .bind(bind_key(account_name))
    .execute(&mut *tx)
    .await?;

    let existing_raw = sqlx::query(&crate::pq::rewrite(
        "SELECT cfg_value FROM config WHERE cfg_key = ? FOR UPDATE",
    ))
    .bind(bind_key(account_name))
    .fetch_optional(&mut *tx)
    .await?
    .and_then(|row| row.try_get::<String, _>(0).ok());
    let existing = parse_json(existing_raw);

    if let Some(existing) = existing {
        let same_uid = existing.get("uid").and_then(Value::as_str) == Some(uid.as_str());
        return if same_uid {
            save_binding_and_clear_reauth_prompt_in_tx(&mut tx, account_name, &binding).await?;
            tx.commit().await?;
            Ok(Some(binding))
        } else {
            tx.rollback().await?;
            Err(anyhow::anyhow!("该展示端账号已有不同的 WorkBuddy 绑定"))
        };
    }

    let owner = json!({ "accountName": account_name, "personId": person_id });
    let owner_inserted = sqlx::query(&crate::pq::rewrite(
        "INSERT INTO config (cfg_key, cfg_value, updated_at) VALUES (?, ?, ?)
         ON CONFLICT (cfg_key) DO NOTHING",
    ))
    .bind(uid_owner_key(&uid))
    .bind(owner.to_string())
    .bind(now_text())
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if !owner_inserted {
        let existing_owner = sqlx::query(&crate::pq::rewrite(
            "SELECT cfg_value FROM config WHERE cfg_key = ? FOR UPDATE",
        ))
        .bind(uid_owner_key(&uid))
        .fetch_optional(&mut *tx)
        .await?
        .and_then(|row| row.try_get::<String, _>(0).ok())
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
        let owner_name = existing_owner
            .as_ref()
            .and_then(|v| v.get("accountName"))
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if owner_name != account_name {
            tx.rollback().await?;
            return Ok(None);
        }
    }
    save_binding_and_clear_reauth_prompt_in_tx(&mut tx, account_name, &binding).await?;
    tx.commit().await?;
    Ok(Some(binding))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_keys_are_bounded_and_hashed() {
        let long_name = "展示端账号".repeat(20);
        let long_uid = "0123456789abcdef".repeat(8);
        let long_state = "5fe24490-d5be-4763-a4e9-7290330bc13a".repeat(4);
        assert!(bind_key(&long_name).len() <= 64);
        assert!(uid_owner_key(&long_uid).len() <= 64);
        assert!(state_key(&long_state).len() <= 64);
        assert!(reauth_prompt_key(&long_name).len() <= 64);
        assert!(!bind_key(&long_name).contains(&long_name));
        assert!(!reauth_prompt_key(&long_name).contains(&long_name));
        assert_eq!(bind_key("浩雨"), bind_key(" 浩雨 "));
        assert_eq!(reauth_prompt_key("浩雨"), reauth_prompt_key(" 浩雨 "));
        assert_ne!(bind_key("浩雨"), bind_key("饭团"));
        assert_ne!(reauth_prompt_key("浩雨"), reauth_prompt_key("饭团"));
    }

    #[test]
    fn reauth_prompt_cooldown_has_exact_seven_day_boundary() {
        let now = 1_800_000_000i64;
        assert!(reauth_prompt_due(None, now));
        assert!(!reauth_prompt_due(Some(now), now));
        assert!(!reauth_prompt_due(
            Some(now - REAUTH_PROMPT_COOLDOWN_SECS + 1),
            now
        ));
        assert!(reauth_prompt_due(
            Some(now - REAUTH_PROMPT_COOLDOWN_SECS),
            now
        ));
        assert!(!reauth_prompt_due(Some(now + 10), now));
    }

    #[test]
    fn malformed_config_json_is_not_a_binding() {
        assert!(parse_json(None).is_none());
        assert!(parse_json(Some("not-json".to_string())).is_none());
        assert!(parse_json(Some("[1,2]".to_string())).is_none());
        assert!(parse_json(Some("{\"uid\":\"u1\"}".to_string())).is_some());
    }

    #[test]
    fn public_binding_drops_internal_fields() {
        let view = public_binding(&json!({
            "uid": "u-1",
            "nickname": "小鹏",
            "realm": "cn",
            "domain": "copilot.tencent.com",
            "platform": "CLI",
            "accountName": "鹏先生",
            "boundAt": "2026-11-13 10:00:00",
            "generation": "internal-generation",
            "secret": "不应外发",
        }));
        assert_eq!(view["uid"], "u-1");
        assert_eq!(view["accountName"], "鹏先生");
        assert!(view.get("secret").is_none());
        assert!(view.get("generation").is_none());
    }

    /* ------------------------ 真库并发回归（默认跳过） ------------------------ */

    /// 只在显式设置 PK_IT_DB=1 且提供 DB_WRITER_* 时运行；不读写任何业务表。
    async fn it_state() -> Option<std::sync::Arc<AppState>> {
        if std::env::var("PK_IT_DB").as_deref() != Ok("1") {
            return None;
        }
        let host = std::env::var("DB_WRITER_HOST").ok()?;
        let port: u16 = std::env::var("DB_WRITER_PORT").ok()?.parse().ok()?;
        let user = std::env::var("DB_WRITER_USER").ok()?;
        let pass = std::env::var("DB_WRITER_PASSWORD").unwrap_or_default();
        let db = std::env::var("DB_WRITER_NAME").ok()?;
        let opt = sqlx::postgres::PgConnectOptions::new()
            .host(&host)
            .port(port)
            .username(&user)
            .password(&pass)
            .database(&db);
        let pool = match sqlx::PgPool::connect_with(opt).await {
            Ok(pool) => pool,
            Err(e) => {
                eprintln!("[skip] 连写库失败：{e}");
                return None;
            }
        };
        let pools = crate::pgrs::build_pools(&[]).await.ok()?;
        Some(std::sync::Arc::new(AppState {
            auth: std::sync::Arc::new(crate::auth::Auth::from_env()),
            sources: Vec::new(),
            pools,
            writer: Some(pool),
            redis: None,
            active: std::sync::Mutex::new("LOCAL".to_string()),
            cache: std::sync::Mutex::new(Default::default()),
            wave_ttl: std::time::Duration::from_secs(600),
            bots: std::sync::OnceLock::new(),
        }))
    }

    async fn cleanup_reauth_prompt(st: &AppState, account_name: &str) {
        if let Some(pool) = st.writer.as_ref() {
            let _ = sqlx::query(&crate::pq::rewrite("DELETE FROM config WHERE cfg_key = ?"))
                .bind(reauth_prompt_key(account_name))
                .execute(pool)
                .await;
        }
    }

    async fn cleanup_workbuddy_binding(st: &AppState, account_name: &str, uid: &str) {
        if let Some(pool) = st.writer.as_ref() {
            for key in [
                bind_key(account_name),
                uid_owner_key(uid),
                reauth_prompt_key(account_name),
            ] {
                let _ = sqlx::query(&crate::pq::rewrite("DELETE FROM config WHERE cfg_key = ?"))
                    .bind(key)
                    .execute(pool)
                    .await;
            }
        }
    }

    #[tokio::test]
    async fn rebind_clear_and_stale_start_are_transaction_safe_real_db() {
        let Some(st) = it_state().await else {
            eprintln!("[skip] 未设置 PK_IT_DB=1 / DB_WRITER_*，跳过重绑/旧 start 竞态回归");
            return;
        };
        let account_name = format!("__wb_rebind_it_{}", std::process::id());
        let uid = format!("__wb_uid_it_{}", std::process::id());
        cleanup_workbuddy_binding(&st, &account_name, &uid).await;
        let payload = json!({
            "uid": uid,
            "nickname": "重绑测试",
            "realm": "cn",
            "domain": "copilot.tencent.com",
            "platform": "CLI",
        });

        let first = commit_binding(&st, &account_name, 1, &payload)
            .await
            .expect("首次绑定失败")
            .expect("首次绑定不应被占用");
        let old_binding = binding_for(&st, &account_name)
            .await
            .expect("首次绑定应可读");
        assert_eq!(first["generation"], old_binding["generation"]);

        let now = Utc::now().timestamp();
        assert!(claim_reauth_prompt_at(&st, &account_name, now)
            .await
            .expect("认领提示失败"));
        assert!(!reauth_prompt_window_open_at(&st, &account_name, now + 1)
            .await
            .expect("认领后读取提示窗口失败"));

        let second = commit_binding(&st, &account_name, 1, &payload)
            .await
            .expect("同 UID 重绑失败")
            .expect("同 UID 重绑不应被占用");
        assert_ne!(
            first["generation"], second["generation"],
            "每次成功重绑都必须推进绑定代次"
        );
        assert!(
            reauth_prompt_window_open_at(&st, &account_name, now + 1)
                .await
                .expect("重绑后读取提示窗口失败"),
            "成功重绑必须清掉提示标记"
        );

        assert!(
            !claim_reauth_prompt_for_binding(&st, &account_name, &old_binding)
                .await
                .expect("旧 start 认领查询失败"),
            "旧 start 读到旧绑定代次后，不得在重绑成功后再写回提示标记"
        );
        assert!(
            reauth_prompt_window_open_at(&st, &account_name, now + 1)
                .await
                .expect("旧 start 后读取提示窗口失败"),
            "被拒绝的旧 start 不得留下冷却标记"
        );
        cleanup_workbuddy_binding(&st, &account_name, &uid).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_first_binding_different_uids_is_account_serialized_real_db() {
        let Some(st) = it_state().await else {
            eprintln!("[skip] 未设置 PK_IT_DB=1 / DB_WRITER_*，跳过首次绑定双 UID 并发回归");
            return;
        };
        let account_name = format!("__wb_first_binding_race_it_{}", std::process::id());

        for round in 0..12 {
            let uid_a = format!("__wb_first_binding_race_a_{}_{}", std::process::id(), round);
            let uid_b = format!("__wb_first_binding_race_b_{}_{}", std::process::id(), round);
            cleanup_workbuddy_binding(&st, &account_name, &uid_a).await;
            cleanup_workbuddy_binding(&st, &account_name, &uid_b).await;

            let payload_a = json!({
                "uid": uid_a,
                "nickname": "双 UID 并发 A",
                "realm": "cn",
                "domain": "copilot.tencent.com",
                "platform": "CLI",
            });
            let payload_b = json!({
                "uid": uid_b,
                "nickname": "双 UID 并发 B",
                "realm": "cn",
                "domain": "copilot.tencent.com",
                "platform": "CLI",
            });
            let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));

            let st_a = std::sync::Arc::clone(&st);
            let account_a = account_name.clone();
            let barrier_a = std::sync::Arc::clone(&barrier);
            let task_a = tokio::spawn(async move {
                barrier_a.wait().await;
                commit_binding(&st_a, &account_a, 1, &payload_a).await
            });

            let st_b = std::sync::Arc::clone(&st);
            let account_b = account_name.clone();
            let barrier_b = std::sync::Arc::clone(&barrier);
            let task_b = tokio::spawn(async move {
                barrier_b.wait().await;
                commit_binding(&st_b, &account_b, 1, &payload_b).await
            });

            let result_a = task_a.await.expect("UID A 绑定任务 panic");
            let result_b = task_b.await.expect("UID B 绑定任务 panic");
            let ok_a = result_a
                .as_ref()
                .ok()
                .and_then(|binding| binding.as_ref())
                .is_some();
            let ok_b = result_b
                .as_ref()
                .ok()
                .and_then(|binding| binding.as_ref())
                .is_some();
            assert!(
                ok_a ^ ok_b,
                "同一账号并发首次绑定两个 UID 只能成功一个：A={ok_a}, B={ok_b}"
            );
            assert!(
                result_a.is_err() ^ result_b.is_err(),
                "另一个并发绑定必须明确报“已有不同绑定”，不能静默成功"
            );

            let current = binding_for(&st, &account_name)
                .await
                .expect("并发首次绑定后应能读到胜出的绑定");
            let winner_uid = current["uid"].as_str().expect("胜出的绑定必须有 uid");
            assert!(winner_uid == uid_a || winner_uid == uid_b);

            let owner_a = parse_json(config_get_raw(&st, &uid_owner_key(&uid_a)).await);
            let owner_b = parse_json(config_get_raw(&st, &uid_owner_key(&uid_b)).await);
            let owner_a_name = owner_a
                .as_ref()
                .and_then(|owner| owner.get("accountName"))
                .and_then(Value::as_str);
            let owner_b_name = owner_b
                .as_ref()
                .and_then(|owner| owner.get("accountName"))
                .and_then(Value::as_str);
            if winner_uid == uid_a {
                assert_eq!(owner_a_name, Some(account_name.as_str()));
                assert!(owner_b.is_none(), "失败的 UID B 不应留下 owner 孤儿");
            } else {
                assert_eq!(owner_b_name, Some(account_name.as_str()));
                assert!(owner_a.is_none(), "失败的 UID A 不应留下 owner 孤儿");
            }

            cleanup_workbuddy_binding(&st, &account_name, &uid_a).await;
            cleanup_workbuddy_binding(&st, &account_name, &uid_b).await;
            assert!(
                binding_for(&st, &account_name).await.is_none(),
                "临时账号绑定必须清理干净"
            );
            assert!(
                config_get_raw(&st, &uid_owner_key(&uid_a)).await.is_none(),
                "临时 UID A owner 必须清理干净"
            );
            assert!(
                config_get_raw(&st, &uid_owner_key(&uid_b)).await.is_none(),
                "临时 UID B owner 必须清理干净"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_rebind_and_start_leave_no_reauth_marker_real_db() {
        let Some(st) = it_state().await else {
            eprintln!("[skip] 未设置 PK_IT_DB=1 / DB_WRITER_*，跳过 start/rebind 并发回归");
            return;
        };
        let account_name = format!("__wb_start_rebind_it_{}", std::process::id());
        let uid = format!("__wb_uid_race_it_{}", std::process::id());
        cleanup_workbuddy_binding(&st, &account_name, &uid).await;
        let payload = json!({
            "uid": uid,
            "nickname": "并发重绑测试",
            "realm": "cn",
            "domain": "copilot.tencent.com",
            "platform": "CLI",
        });
        commit_binding(&st, &account_name, 1, &payload)
            .await
            .expect("准备初始绑定时失败")
            .expect("准备初始绑定不应被占用");

        for _ in 0..12 {
            let expected_binding = binding_for(&st, &account_name)
                .await
                .expect("并发回归读取旧绑定失败");
            let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));

            let claim_st = std::sync::Arc::clone(&st);
            let claim_account = account_name.clone();
            let claim_expected = expected_binding.clone();
            let claim_barrier = std::sync::Arc::clone(&barrier);
            let claim = tokio::spawn(async move {
                claim_barrier.wait().await;
                claim_reauth_prompt_for_binding(&claim_st, &claim_account, &claim_expected).await
            });

            let rebind_st = std::sync::Arc::clone(&st);
            let rebind_account = account_name.clone();
            let rebind_barrier = std::sync::Arc::clone(&barrier);
            let rebind_payload = payload.clone();
            let rebind = tokio::spawn(async move {
                rebind_barrier.wait().await;
                commit_binding(&rebind_st, &rebind_account, 1, &rebind_payload).await
            });

            claim
                .await
                .expect("start 认领任务 panic")
                .expect("start 认领查询失败");
            rebind
                .await
                .expect("重绑任务 panic")
                .expect("并发重绑失败")
                .expect("并发重绑不应被占用");

            let current_binding = binding_for(&st, &account_name)
                .await
                .expect("并发回归读取新绑定失败");
            assert_ne!(
                current_binding["generation"], expected_binding["generation"],
                "并发重绑必须产生新 generation"
            );
            assert!(
                reauth_prompt_window_open_at(&st, &account_name, Utc::now().timestamp())
                    .await
                    .expect("并发回归读取提示窗口失败"),
                "无论 start/rebind 谁先拿到锁，成功后都不能留下提示标记"
            );
        }
        cleanup_workbuddy_binding(&st, &account_name, &uid).await;
    }

    #[tokio::test]
    async fn blocked_status_reads_do_not_consume_reauth_prompt_window_real_db() {
        let Some(st) = it_state().await else {
            eprintln!("[skip] 未设置 PK_IT_DB=1 / DB_WRITER_*，跳过 blocked status 回归");
            return;
        };
        let account_name = format!("__wb_blocked_status_it_{}", std::process::id());
        cleanup_reauth_prompt(&st, &account_name).await;
        let now = Utc::now().timestamp();

        /* 模拟前端被 mustPwdMask / qqMask / gate 连续挡住并延迟重试 status。 */
        for _ in 0..12 {
            assert!(
                reauth_prompt_window_open_at(&st, &account_name, now)
                    .await
                    .expect("读取提示窗口失败"),
                "只读 status 不得消费提示窗口"
            );
        }
        assert!(
            claim_reauth_prompt_at(&st, &account_name, now)
                .await
                .expect("首次真正开始重登时认领失败"),
            "多次 blocked status 后，首次真正 start 仍应成功认领"
        );
        assert!(!reauth_prompt_window_open_at(&st, &account_name, now + 1)
            .await
            .expect("认领后读取提示窗口失败"));
        cleanup_reauth_prompt(&st, &account_name).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn reauth_prompt_claim_is_atomic_real_db() {
        let Some(st) = it_state().await else {
            eprintln!("[skip] 未设置 PK_IT_DB=1 / DB_WRITER_*，跳过重新登录提示真库并发回归");
            return;
        };
        let account_name = format!("__wb_reauth_prompt_it_{}", std::process::id());
        cleanup_reauth_prompt(&st, &account_name).await;
        let now = Utc::now().timestamp();

        let mut handles = Vec::new();
        for _ in 0..16 {
            let st = std::sync::Arc::clone(&st);
            let account_name = account_name.clone();
            handles.push(tokio::spawn(async move {
                claim_reauth_prompt_at(&st, &account_name, now).await
            }));
        }
        let mut claimed = 0;
        for handle in handles {
            match handle.await.expect("并发认领任务不应 panic") {
                Ok(true) => claimed += 1,
                Ok(false) => {}
                Err(e) => panic!("并发认领失败：{e}"),
            }
        }
        assert_eq!(claimed, 1, "16 个并发请求只能有一个拿到弹窗资格");
        assert!(!reauth_prompt_window_open_at(&st, &account_name, now + 1)
            .await
            .expect("冷却期内读取提示窗口失败"));

        assert!(!claim_reauth_prompt_at(&st, &account_name, now + 1)
            .await
            .expect("冷却期内查询失败"));
        assert!(
            !claim_reauth_prompt_at(&st, &account_name, now + REAUTH_PROMPT_COOLDOWN_SECS - 1)
                .await
                .expect("7 天差 1 秒查询失败")
        );
        assert!(
            claim_reauth_prompt_at(&st, &account_name, now + REAUTH_PROMPT_COOLDOWN_SECS)
                .await
                .expect("满 7 天应允许再次提示")
        );
        assert!(!reauth_prompt_window_open_at(
            &st,
            &account_name,
            now + REAUTH_PROMPT_COOLDOWN_SECS
        )
        .await
        .expect("再次认领后读取提示窗口失败"));

        clear_reauth_prompt(&st, &account_name)
            .await
            .expect("清理提示标记失败");
        assert!(
            claim_reauth_prompt_at(&st, &account_name, now + REAUTH_PROMPT_COOLDOWN_SECS)
                .await
                .expect("清理后查询失败"),
            "重新绑定清理标记后，下次满足条件应立即允许提示"
        );
        cleanup_reauth_prompt(&st, &account_name).await;
    }
}
