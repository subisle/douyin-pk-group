//! WorkBuddy 展示端账号绑定状态。
//!
//! 复用 `config` 表，不新增迁移：`wb_bind_md5:<展示端账号名 md5>` 存绑定账号，
//! `wb_uid_owner_md5:<WorkBuddy uid md5>` 存 uid 归属，`wb_oauth_state_md5:<state md5>`
//! 存一笔待完成授权的归属。键里的账号名、uid、state 都先 md5，避免超过
//! `config.cfg_key VARCHAR(64)` 或把用户信息写进配置键。

use crate::state::AppState;
use anyhow::Result;
use chrono::Local;
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

async fn config_set_if_absent_raw(st: &AppState, key: &str, value: &str) -> Result<bool> {
    let pool = w(st)?;
    let inserted = sqlx::query(&crate::pq::rewrite(
        "INSERT INTO config (cfg_key, cfg_value, updated_at) VALUES (?, ?, ?)
         ON CONFLICT (cfg_key) DO NOTHING",
    ))
    .bind(key)
    .bind(value)
    .bind(now_text())
    .execute(pool)
    .await?
    .rows_affected();
    Ok(inserted == 1)
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
    let binding = json!({
        "uid": uid,
        "nickname": account.get("nickname").and_then(Value::as_str).unwrap_or_default(),
        "realm": account.get("realm").and_then(Value::as_str).unwrap_or_default(),
        "domain": account.get("domain").and_then(Value::as_str).unwrap_or_default(),
        "platform": account.get("platform").and_then(Value::as_str).unwrap_or_default(),
        "accountName": account_name,
        "boundAt": now_text(),
    });
    if let Some(existing) = binding_for(st, account_name).await {
        let same_uid = existing.get("uid").and_then(Value::as_str) == Some(uid.as_str());
        return if same_uid {
            Ok(Some(existing))
        } else {
            Err(anyhow::anyhow!("该展示端账号已有不同的 WorkBuddy 绑定"))
        };
    }
    let owner = json!({ "accountName": account_name, "personId": person_id });
    if !config_set_if_absent_raw(st, &uid_owner_key(&uid), &owner.to_string()).await? {
        let existing = parse_json(config_get_raw(st, &uid_owner_key(&uid)).await);
        let owner_name = existing
            .as_ref()
            .and_then(|v| v.get("accountName"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if owner_name != account_name {
            return Ok(None);
        }
    }
    config_set_raw(st, &bind_key(account_name), &binding.to_string()).await?;
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
        assert!(!bind_key(&long_name).contains(&long_name));
        assert_eq!(bind_key("浩雨"), bind_key(" 浩雨 "));
        assert_ne!(bind_key("浩雨"), bind_key("饭团"));
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
            "secret": "不应外发",
        }));
        assert_eq!(view["uid"], "u-1");
        assert_eq!(view["accountName"], "鹏先生");
        assert!(view.get("secret").is_none());
    }
}
