//! 管理员鉴权：一个共享口令换内存 token。
//!
//! - `ADMIN_PASSWORD` 未配置 → 整个 /api 放行（本地开发方便），登录接口会告知前端「无需口令」
//! - 已配置 → 除公开端点外，所有 /api 必须带 `Authorization: Bearer <token>`
//! - token 存在内存里，进程重启即失效；空闲超过 ADMIN_TOKEN_TTL 自动清理
//!
//! 口令比对走常数时间，避免逐字节提前返回泄露前缀。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// 口令哈希：避免明文常驻内存 + 顺带做常数时间比较
fn hash_password(pw: &str) -> [u8; 32] {
    // FNV-1a 64 位跑两轮 + 长度混合，够做「不是明文」+ 常数时间比较
    // （真正的安全性来自网络层：服务只监听 127.0.0.1，公网入口由 Caddy 收口）
    let mut h1: u64 = 0xcbf2_9ce4_8422_2325;
    let mut h2: u64 = 0x9e37_79b9_7f4a_7c15;
    for (i, b) in pw.as_bytes().iter().enumerate() {
        h1 ^= *b as u64;
        h1 = h1.wrapping_mul(0x0000_0100_0000_01b3);
        h2 = h2.rotate_left(5) ^ (*b as u64).wrapping_add(i as u64);
        h2 = h2.wrapping_mul(0xff51_afd7_ed55_8ccd);
    }
    let mut out = [0u8; 32];
    out[..8].copy_from_slice(&h1.to_le_bytes());
    out[8..16].copy_from_slice(&h2.to_le_bytes());
    out[16..24].copy_from_slice(&(h1 ^ h2).rotate_left(17).to_le_bytes());
    out[24..].copy_from_slice(&(pw.len() as u64).wrapping_mul(h1).to_le_bytes());
    out
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 随机 token：优先读 /dev/urandom，失败时退化为时间 + 计数器拼接
fn random_token() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let mut bytes = [0u8; 24];
    // /dev/urandom 是无限流，必须 read_exact 读满 24 字节，不能 fs::read
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut bytes);
    }
    if bytes.iter().all(|b| *b == 0) {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        bytes[..8].copy_from_slice(&t.to_le_bytes());
        bytes[8..12].copy_from_slice(&n.to_le_bytes()[..4]);
        bytes[12..16].copy_from_slice(&std::process::id().to_le_bytes());
        bytes[16..24].copy_from_slice(&t.rotate_left(29).wrapping_add(n).to_le_bytes());
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub struct Auth {
    /// None = 未配置口令，全放行
    password_hash: Option<[u8; 32]>,
    tokens: Mutex<HashMap<String, Instant>>,
    ttl: Duration,
}

impl Default for Auth {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Auth {
    pub fn from_env() -> Self {
        let password_hash = std::env::var("ADMIN_PASSWORD")
            .ok()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .map(|p| hash_password(&p));
        if password_hash.is_none() {
            tracing::warn!(
                "未设置 ADMIN_PASSWORD：/api 将不鉴权（仅适合本地开发，生产务必在 .env 里配上）"
            );
        }
        let ttl = std::env::var("ADMIN_TOKEN_TTL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8 * 3600u64);
        Self {
            password_hash,
            tokens: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(ttl),
        }
    }

    /// 是否开启了鉴权
    pub fn enabled(&self) -> bool {
        self.password_hash.is_some()
    }

    pub fn check_password(&self, input: &str) -> bool {
        match &self.password_hash {
            // 没配口令 → 永远放行
            None => true,
            Some(h) => ct_eq(h, &hash_password(input)),
        }
    }

    pub fn issue(&self) -> String {
        let token = random_token();
        let mut map = self.tokens.lock().expect("token 锁 poisoned");
        let now = Instant::now();
        map.retain(|_, at| now.duration_since(*at) < self.ttl);
        map.insert(token.clone(), now);
        token
    }

    pub fn verify(&self, token: &str) -> bool {
        if !self.enabled() {
            return true;
        }
        let mut map = self.tokens.lock().expect("token 锁 poisoned");
        let now = Instant::now();
        map.retain(|_, at| now.duration_since(*at) < self.ttl);
        map.contains_key(token)
    }

    pub fn revoke(&self, token: &str) {
        self.tokens.lock().expect("token 锁 poisoned").remove(token);
    }

    fn token_of(headers: &axum::http::HeaderMap) -> Option<String> {
        let raw = headers
            .get(header::AUTHORIZATION)?
            .to_str()
            .ok()?
            .trim()
            .to_string();
        // 支持 `Bearer xxx` 与裸 token 两种写法
        let token = match raw.split_once(' ') {
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case("bearer") => rest.trim(),
            _ => raw.as_str(),
        };
        (!token.is_empty()).then(|| token.to_string())
    }
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("enabled", &self.enabled())
            .field("ttl", &self.ttl)
            .finish()
    }
}

/* ------------------------------- axum 中间件 ------------------------------- */

/// 无需登录的路径前缀。
///
/// - `/api/public*` 对外展示页，本来就是给外人看的
/// - `/api/auth/session` 前端启动时靠它判断「这台机器要不要口令」，拦掉的话登录页永远出不来
/// - `/api/auth/login` 换 token 本身
/// - `/api/health` 探活
const OPEN_PREFIXES: &[&str] = &[
    "/api/public",
    "/api/auth/session",
    "/api/auth/login",
    "/api/health",
];

fn is_open(path: &str) -> bool {
    OPEN_PREFIXES.iter().any(|p| path.starts_with(p))
}

pub async fn require_admin(
    State(st): State<std::sync::Arc<crate::state::AppState>>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();
    let auth = &st.auth;

    if !auth.enabled() || is_open(path) {
        return next.run(req).await;
    }

    let ok = Auth::token_of(req.headers()).is_some_and(|t| auth.verify(&t));
    if ok {
        return next.run(req).await;
    }

    // 浏览器导航直接访问后台页面时，返回 401 JSON；前端会跳登录页
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "需要管理员口令" })),
    )
        .into_response()
}

/// `POST /api/auth/login`：口令换 token（口令错返回 401）
pub async fn login(
    State(st): State<std::sync::Arc<crate::state::AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let auth = &st.auth;
    let password = body.get("password").and_then(|v| v.as_str()).unwrap_or("");
    if !auth.check_password(password) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "口令不正确" })),
        )
            .into_response();
    }
    let token = auth.issue();
    Json(json!({ "token": token, "enabled": auth.enabled() })).into_response()
}

/// `POST /api/auth/logout`：当前 token 立即失效
pub async fn logout(
    State(st): State<std::sync::Arc<crate::state::AppState>>,
    headers: axum::http::HeaderMap,
) -> Response {
    let auth = &st.auth;
    if let Some(t) = Auth::token_of(&headers) {
        auth.revoke(&t);
    }
    Json(json!({ "ok": true })).into_response()
}

/// `GET /api/auth/session`：前端启动时问一句「要不要口令 / 手里这 token 还能用吗」
pub async fn session(
    State(st): State<std::sync::Arc<crate::state::AppState>>,
    headers: axum::http::HeaderMap,
) -> Response {
    let auth = &st.auth;
    let enabled = auth.enabled();
    let ok = !enabled || Auth::token_of(&headers).is_some_and(|t| auth.verify(&t));
    Json(json!({ "enabled": enabled, "ok": ok })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth_with(password: Option<&str>) -> Auth {
        Auth {
            password_hash: password.map(hash_password),
            tokens: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(3600),
        }
    }

    #[test]
    fn no_password_configured_means_open() {
        let a = auth_with(None);
        assert!(!a.enabled());
        // 没配口令时任何口令都放行，且不需要 token
        assert!(a.check_password(""));
        assert!(a.check_password("随便写"));
        assert!(a.verify("不存在的 token"));
    }

    #[test]
    fn wrong_password_is_rejected() {
        let a = auth_with(Some("s3cret"));
        assert!(a.enabled());
        assert!(a.check_password("s3cret"));
        assert!(!a.check_password("s3cret "));
        assert!(!a.check_password("S3CRET"));
        assert!(!a.check_password(""));
    }

    #[test]
    fn token_roundtrip_and_revoke() {
        let a = auth_with(Some("pw"));
        let t = a.issue();
        assert!(a.verify(&t));
        // 再签一枚不会让旧的失效
        let t2 = a.issue();
        assert!(a.verify(&t) && a.verify(&t2));
        assert_ne!(t, t2);
        // 吊销只影响自己
        a.revoke(&t);
        assert!(!a.verify(&t));
        assert!(a.verify(&t2));
    }

    #[test]
    fn expired_token_is_dropped() {
        let a = Auth {
            password_hash: Some(hash_password("pw")),
            tokens: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(0),
        };
        let t = a.issue();
        assert!(!a.verify(&t), "ttl=0 的 token 应当立刻失效");
    }

    #[test]
    fn tokens_are_unique() {
        let a = auth_with(Some("pw"));
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            assert!(seen.insert(a.issue()), "token 撞车了");
        }
    }

    fn headers_with(v: &str) -> axum::http::HeaderMap {
        let mut h = axum::http::HeaderMap::new();
        h.insert(header::AUTHORIZATION, v.parse().unwrap());
        h
    }

    #[test]
    fn parses_bearer_and_bare_token() {
        assert_eq!(
            Auth::token_of(&headers_with("Bearer abc")).as_deref(),
            Some("abc")
        );
        assert_eq!(
            Auth::token_of(&headers_with("bearer abc")).as_deref(),
            Some("abc")
        );
        assert_eq!(Auth::token_of(&headers_with("abc")).as_deref(), Some("abc"));
        // 没有 / 空的 Authorization 时返回 None
        assert_eq!(Auth::token_of(&axum::http::HeaderMap::new()), None);
        assert_eq!(Auth::token_of(&headers_with("")), None);
        // 只有 "Bearer" 而没有 token 时，等同于把它当裸 token，交给 verify 去判
        assert_eq!(
            Auth::token_of(&headers_with("Bearer")).as_deref(),
            Some("Bearer")
        );
    }

    #[test]
    fn whitelist_covers_public_and_login_only() {
        // 对外展示页 + 健康检查 + 登录：免鉴权
        for p in [
            "/api/public/plans",
            "/api/public/plans/12",
            "/api/auth/session",
            "/api/auth/login",
            "/api/health",
        ] {
            assert!(is_open(p), "{p} 应该免鉴权");
        }
        // 其余管理接口都要口令
        for p in [
            "/api/roster/parse",
            "/api/plans",
            "/api/plans/1/publish",
            "/api/group/arrange",
            "/api/auth/logout",
        ] {
            assert!(!is_open(p), "{p} 不该免鉴权");
        }
    }
}
