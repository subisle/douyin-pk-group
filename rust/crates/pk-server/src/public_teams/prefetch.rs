//! 登录前加密预取（2026-10-22）。
//!
//! 登录前只下发 AES-256-GCM 密文；256 位随机密钥仅保存在服务端内存，
//! 用户登录成功并拿到展示端 token 后，才按 token 团别释放对应密钥。
//! 前端用 Web Crypto 解密并在本地渲染。密钥不落库、不写日志，10 分钟过期。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::json;

use super::*;

const PREFETCH_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Clone)]
struct StoredKey {
    team: String,
    key: [u8; 32],
    expires: Instant,
}

fn key_store() -> &'static Mutex<HashMap<String, StoredKey>> {
    static STORE: OnceLock<Mutex<HashMap<String, StoredKey>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn prune_store(map: &mut HashMap<String, StoredKey>, now: Instant) {
    map.retain(|_, v| v.expires > now);
}

fn valid_sid(sid: &str) -> bool {
    (16..=96).contains(&sid.len())
        && sid
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn normalize_prefetch_team(raw: &str) -> &'static str {
    match raw.trim() {
        "male" => "male",
        "female" => "female",
        _ => "all",
    }
}

#[derive(Deserialize)]
pub struct PrefetchQuery {
    pub sid: String,
    pub team: Option<String>,
}

/// `GET /api/public/prefetch?sid=&team=`：登录前提前获取加密的首屏 site + daily。
/// 明文只在服务端进程内短暂存在；下发内容无法被未登录用户解密。
pub async fn encrypted_prefetch(
    State(st): State<ArcState>,
    Query(q): Query<PrefetchQuery>,
) -> ApiResult {
    let sid = q.sid.trim();
    if !valid_sid(sid) {
        return Err(ApiError::bad_request("预取会话标识不合法"));
    }
    let hint = normalize_prefetch_team(q.team.as_deref().unwrap_or(""));
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(hint) {
        return Ok(Json(json!({
            "ok": true,
            "disabled": true,
            "team": hint,
            "site": Value::Null,
            "daily": Value::Null,
        })));
    }
    let query_team = if hint == "all" { "" } else { hint };
    let (site, daily) = tokio::try_join!(
        site_payload(&st, &s, query_team),
        daily_payload(&st, &s, query_team, None),
    )?;
    let plain = serde_json::to_vec(&json!({
        "team": hint,
        "site": site.0,
        "daily": daily.0,
    }))
    .map_err(|e| anyhow::anyhow!("预取序列化失败：{e}"))?;

    let mut key = [0u8; 32];
    let mut iv = [0u8; 12];
    getrandom::getrandom(&mut key).map_err(|e| anyhow::anyhow!("预取密钥生成失败：{e}"))?;
    getrandom::getrandom(&mut iv).map_err(|e| anyhow::anyhow!("预取 IV 生成失败：{e}"))?;
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|_| anyhow::anyhow!("预取密钥初始化失败"))?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&iv), plain.as_ref())
        .map_err(|_| anyhow::anyhow!("预取加密失败"))?;

    let now = Instant::now();
    let expires = now + PREFETCH_TTL;
    {
        let mut map = key_store()
            .lock()
            .map_err(|_| anyhow::anyhow!("预取密钥存储不可用"))?;
        prune_store(&mut map, now);
        if map.len() >= 2048 {
            if let Some(oldest) = map
                .iter()
                .min_by_key(|(_, v)| v.expires)
                .map(|(sid, _)| sid.clone())
            {
                map.remove(&oldest);
            }
        }
        map.insert(
            sid.to_string(),
            StoredKey {
                team: hint.to_string(),
                key,
                expires,
            },
        );
    }

    Ok(Json(json!({
        "ok": true,
        "sid": sid,
        "team": hint,
        "iv": STANDARD.encode(iv),
        "data": STANDARD.encode(ciphertext),
        "expiresIn": PREFETCH_TTL.as_secs(),
    })))
}

#[derive(Deserialize)]
pub struct PrefetchKeyQuery {
    pub sid: String,
}

/// `GET /api/public/prefetch/key?sid=`：登录后按 token 团别释放一次性解密密钥。
pub async fn prefetch_key(
    State(st): State<ArcState>,
    Query(q): Query<PrefetchKeyQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let sid = q.sid.trim();
    if !valid_sid(sid) {
        return Err(ApiError::bad_request("预取会话标识不合法"));
    }
    let identity_team = public_identity_team(&st, &headers).await?;
    let now = Instant::now();
    let entry = {
        let mut map = key_store()
            .lock()
            .map_err(|_| anyhow::anyhow!("预取密钥存储不可用"))?;
        prune_store(&mut map, now);
        let Some(entry) = map.get(sid).cloned() else {
            return Err(ApiError::bad_request("预取数据已过期，请直接加载"));
        };
        // 空团 token = 全团账号，可取任意团；固定团 token 只能取同团。
        if !identity_team.is_empty() && entry.team != identity_team {
            return Err(ApiError::forbidden("预取数据不属于当前团"));
        }
        // 全团密文只有全团 token 能解；固定团即使有 token 也不能拿全团密钥。
        if entry.team == "all" && !identity_team.is_empty() {
            return Err(ApiError::forbidden("预取数据不属于当前团"));
        }
        map.remove(sid);
        entry
    };
    Ok(Json(json!({
        "ok": true,
        "sid": sid,
        "team": entry.team,
        "key": STANDARD.encode(entry.key),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefetch_sid_is_strict() {
        assert!(valid_sid("0123456789abcdef"));
        assert!(valid_sid("A_b-1234567890xyz"));
        assert!(!valid_sid("short"));
        assert!(!valid_sid("0123456789abcde/"));
        assert!(!valid_sid(&"a".repeat(97)));
    }

    #[test]
    fn prefetch_team_only_allows_known_teams() {
        assert_eq!(normalize_prefetch_team("male"), "male");
        assert_eq!(normalize_prefetch_team("female"), "female");
        assert_eq!(normalize_prefetch_team(""), "all");
        assert_eq!(normalize_prefetch_team("bogus"), "all");
    }
}
