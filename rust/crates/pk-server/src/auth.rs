//! 管理员鉴权：一个共享口令换内存 token。
//!
//! - `ADMIN_PASSWORD` 未配置 → **拒绝启动**（P1-5），除非显式设 `ALLOW_NO_ADMIN_PASSWORD=1`（本地开发）
//! - 已配置 → 除公开端点外，所有 /api 必须带 `Authorization: Bearer <token>`
//! - token 存在内存里，进程重启即失效；空闲超过 ADMIN_TOKEN_TTL 自动清理
//! - 登录失败按来源计数：连续 5 次失败锁 10 分钟（P1-4），成功登录即清零
//!
//! 口令哈希用 argon2id（PHC 字符串常驻内存），校验走 `PasswordVerifier`
//! 的常数时间比较，避免逐字节提前返回泄露前缀。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// P1-4：口令哈希改 argon2id。启动时对 ADMIN_PASSWORD 现算一次（约几十毫秒），
/// 之后每次登录 verify。存 PHC 字符串，不再有自造 FNV 变体。
pub(crate) fn hash_password(pw: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .expect("argon2 哈希失败")
        .to_string()
}

/// 校验：输入与 PHC 串比对。PHC 串解析失败按「不匹配」处理（不 panic）。
pub(crate) fn verify_password(input: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(input.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
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
    /// None = 未配置口令，全放行（仅 ALLOW_NO_ADMIN_PASSWORD=1 时可能存在）
    password_hash: Option<String>,
    tokens: Mutex<HashMap<String, Instant>>,
    ttl: Duration,
    /// P1-4：登录失败限流（同来源连续失败 N 次锁 M 分钟）
    limiter: LoginLimiter,
}

/// P1-4：登录失败限流 —— 同一来源连续失败 N 次锁定 M 分钟，成功登录清零。
/// 参考 public_teams.rs 的 VerifyLimiter（每窗口固定次数），这里是「连续失败
/// 计数 + 到期解锁」，对在线撞库更直接；内存 HashMap，key 会随锁过期淘汰。
pub struct LoginLimiter {
    hits: Mutex<HashMap<String, Entry>>,
    max_failures: u32,
    lockout: Duration,
}

struct Entry {
    /// 连续失败次数
    failures: u32,
    /// 锁定截止时刻（仅在 failures 达到阈值时设置）
    locked_until: Option<Instant>,
    /// 最近一次活动，用于淘汰不活跃的 key
    last: Instant,
}

impl Default for LoginLimiter {
    fn default() -> Self {
        Self::new(5, Duration::from_secs(10 * 60))
    }
}

impl LoginLimiter {
    pub fn new(max_failures: u32, lockout: Duration) -> Self {
        Self {
            hits: Mutex::new(HashMap::new()),
            max_failures,
            lockout,
        }
    }

    /// 来源是否被锁。锁定到期时顺手解锁并清零。
    pub fn locked(&self, key: &str) -> bool {
        let mut hits = self.hits.lock().expect("login 限流锁 poisoned");
        let now = Instant::now();
        if let Some(e) = hits.get_mut(key) {
            if let Some(until) = e.locked_until {
                if now < until {
                    return true;
                }
                // 锁定期满：解锁并清零
                e.locked_until = None;
                e.failures = 0;
            }
            e.last = now;
        }
        false
    }

    /// 记一次失败；达到阈值则进入锁定并返回 true（用于打日志）。
    pub fn record_failure(&self, key: &str) -> bool {
        let mut hits = self.hits.lock().expect("login 限流锁 poisoned");
        let now = Instant::now();
        // 顺手淘汰：只留活跃 key（锁定中的也算活跃），防 map 无限膨胀
        hits.retain(|_, e| {
            now.duration_since(e.last) < Duration::from_secs(3600)
                || e.locked_until.is_some_and(|u| now < u)
        });
        let e = hits.entry(key.to_string()).or_insert(Entry {
            failures: 0,
            locked_until: None,
            last: now,
        });
        e.last = now;
        e.failures = e.failures.saturating_add(1);
        if e.failures >= self.max_failures {
            e.locked_until = Some(now + self.lockout);
            true
        } else {
            false
        }
    }

    /// 成功登录：清零该来源的失败计数。
    pub fn record_success(&self, key: &str) {
        let mut hits = self.hits.lock().expect("login 限流锁 poisoned");
        if let Some(e) = hits.get_mut(key) {
            e.failures = 0;
            e.locked_until = None;
            e.last = Instant::now();
        }
    }

    /// 剩余锁定秒数（用于响应体提示）；未锁返回 0。
    pub fn locked_seconds(&self, key: &str) -> u64 {
        let mut hits = self.hits.lock().expect("login 限流锁 poisoned");
        let now = Instant::now();
        if let Some(e) = hits.get_mut(key) {
            if let Some(until) = e.locked_until {
                if now < until {
                    return (until - now).as_secs() + 1;
                }
                e.locked_until = None;
                e.failures = 0;
            }
        }
        0
    }
}

/// 登录来源：优先 X-Forwarded-For（仅当直连方是 127.0.0.1，即经本机反代），
/// 否则用直连 IP。避免经 Caddy 后全站共用一个来源，同时不信任外来的 XFF。
fn login_source_key(
    remote: Option<std::net::SocketAddr>,
    headers: &axum::http::HeaderMap,
) -> String {
    let direct = remote.map(|a| a.ip().to_string()).unwrap_or_default();
    if direct == "127.0.0.1" || direct == "::1" {
        if let Some(xff) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            // 取第一跳（最初客户端）
            return xff
                .split(',')
                .next()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or(direct);
        }
    }
    direct
}

/// P1-4：登录限流器单例（每进程一份，测试里也用 Auth 自带的那份）
static LOGIN_LIMITER: OnceLock<LoginLimiter> = OnceLock::new();
pub fn login_limiter() -> &'static LoginLimiter {
    LOGIN_LIMITER.get_or_init(LoginLimiter::default)
}

impl Default for Auth {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Auth {
    /// P1-5：口令是否已配置。`main.rs` 在口令缺失且未显式
    /// `ALLOW_NO_ADMIN_PASSWORD=1` 时会拒绝启动。
    pub fn password_configured() -> bool {
        std::env::var("ADMIN_PASSWORD")
            .ok()
            .map(|p| !p.trim().is_empty())
            .unwrap_or(false)
    }

    pub fn from_env() -> Self {
        let password_hash = std::env::var("ADMIN_PASSWORD")
            .ok()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .map(|p| {
                let phc = hash_password(&p);
                tracing::info!("ADMIN_PASSWORD 已加载（argon2id 哈希驻留内存，不存明文）");
                phc
            });
        if password_hash.is_none() {
            // 走到这里的调用方（测试 / ALLOW_NO_ADMIN_PASSWORD=1 显式放行）只打提示；
            // main.rs 的启动守卫负责真正的「拒绝启动」。
            tracing::warn!(
                "未设置 ADMIN_PASSWORD：/api 将不鉴权（仅本地开发；生产必须在 .env 里配上，或显式设 ALLOW_NO_ADMIN_PASSWORD=1）"
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
            limiter: LoginLimiter::default(),
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
            Some(phc) => verify_password(input, phc),
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
/// - `/api/public/` 对外展示页，本来就是给外人看的
/// - `/api/auth/session` 前端启动时靠它判断「这台机器要不要口令」，拦掉的话登录页永远出不来
/// - `/api/auth/login` 换 token 本身
/// - `/api/health` 探活
///
/// P2-20：这里全部是**带边界的前缀段** —— 匹配时要求命中项正好是路径前缀且
/// 下一个字符是 `/` 或路径到此结束，避免 `/api/public-x` 这类「看起来像白名单」
/// 的新路径被静默放过。`/` 根路径单独精确放行（见 is_open）。
const OPEN_PREFIXES: &[&str] = &[
    "/st/",
    // 精确路径（open_prefix_hit 里「路径正好等于前缀」时放行）：
    "/1",
    "/0", // 对外展示页：/1 男团 /0 女团（旧链接别名，路由是精确匹配）
    // 男女共用一个网址（2026-10-10）：根路径与子路径视图都进展示页。
    // 注意 /my 是展示页「我的」前缀；管理端没有以这些开头的路径，不冲突。
    "/report/",
    "/lineage/",
    "/my/",
    "/group/",
    "/api/public/",
    "/api/auth/session",
    "/api/auth/login",
    "/api/health",
];

/// P2-20：带边界的前缀匹配 —— `path` 以 `prefix` 开头，且 prefix 正好是完整
/// 路径（`/api/health`）或其后紧跟 `/`（`/st/f` 命中 `/st/`）。
/// `/api/public-x` 不会命中 `/api/public/`，`/10` 也不会命中 `/1`。
fn open_prefix_hit(prefix: &str, path: &str) -> bool {
    if !path.starts_with(prefix) {
        return false;
    }
    // 前缀以 / 结尾时天然带边界（下一个字符不可能让两段拼成别的段）；
    // 不带 / 的前缀（/api/health 等）要求路径到此正好结束。
    prefix.ends_with('/') || path.len() == prefix.len()
}

fn is_open(path: &str) -> bool {
    // 根路径就是对外展示页（男女共用一个网址），精确放行；
    // 不能用 starts_with("/") 前缀放行，否则整个 /api 都裸奔了。
    if path == "/" {
        return true;
    }
    OPEN_PREFIXES.iter().any(|p| open_prefix_hit(p, path))
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

    let mut ok = Auth::token_of(req.headers()).is_some_and(|t| auth.verify(&t));
    // <img src> 无法带 Authorization 头：允许 ?access_token= 传 token（日报图等场景）
    if !ok {
        if let Ok(q) = req.uri().query().map(|q| q.to_string()).ok_or(()) {
            for pair in q.split('&') {
                if let Some(v) = pair.strip_prefix("access_token=") {
                    ok = auth.verify(v);
                    break;
                }
            }
        }
    }
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
///
/// P1-4：登录失败限流 —— 同一来源连续 5 次失败锁 10 分钟，成功登录清零。
pub async fn login(
    State(st): State<std::sync::Arc<crate::state::AppState>>,
    axum::extract::ConnectInfo(remote): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let auth = &st.auth;
    let source = login_source_key(Some(remote), &headers);
    let limiter = &auth.limiter;

    // 已被锁定：直接拒绝，不验口令（验证本身要跑 argon2，白送给攻击者算力）
    if limiter.locked(&source) {
        let wait = limiter.locked_seconds(&source);
        tracing::warn!(source = %source, wait_s = wait, "登录失败次数过多，来源被临时锁定");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error": format!("失败次数过多，请 {wait} 秒后再试"),
                "lockedFor": wait,
            })),
        )
            .into_response();
    }

    let password = body.get("password").and_then(|v| v.as_str()).unwrap_or("");
    if !auth.check_password(password) {
        let now_locked = limiter.record_failure(&source);
        if now_locked {
            tracing::warn!(source = %source, "连续登录失败达到阈值，锁定 10 分钟");
        }
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "口令不正确" })),
        )
            .into_response();
    }
    limiter.record_success(&source);
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
            limiter: LoginLimiter::default(),
        }
    }

    #[test]
    fn argon2_hash_roundtrip() {
        let phc = hash_password("s3cret 口令");
        assert!(phc.starts_with("$argon2id$"), "应当是 argon2id 的 PHC 串");
        assert!(!phc.contains("s3cret"), "PHC 串里不能出现明文");
        assert!(verify_password("s3cret 口令", &phc));
        assert!(!verify_password("s3cret", &phc));
        assert!(!verify_password("s3cret 口令 ", &phc));
        assert!(!verify_password("S3CRET 口令", &phc));
        assert!(!verify_password("", &phc));
        // 盐随机：同口令两次哈希结果不同
        assert_ne!(hash_password("s3cret 口令"), phc);
        // 坏 PHC 串按「不匹配」处理，不 panic
        assert!(!verify_password("x", "$argon2id$garbage"));
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
            limiter: LoginLimiter::default(),
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

    /// P2-20：前缀必须带段边界 —— 形似白名单的新路径不能被静默放过。
    #[test]
    fn open_prefixes_require_segment_boundary() {
        // 形似白名单但不是：必须仍走鉴权
        for p in [
            "/api/public-x",
            "/api/publicx",
            "/api/publication",
            "/api/auth/login-x",
            "/api/healthcheck",
            "/api/health-check",
            "/staging",
            // 注意：/st/{team} 本来就是公开路由，/st/page 属于白名单（team=page）；
            // 这里验证的是「形似但不在白名单体系里」的路径。
            "/1st",
            "/10",
            "/01",
            "/1x",
            "/0x",
            "/my-page",
            "/reports",
            "/groups",
            "/lineage2",
        ] {
            assert!(!is_open(p), "{p} 不该被白名单放过");
        }
        // 真正的白名单路径照常放行
        for p in [
            "/st/",
            "/st/f",
            "/st/m/whatever",
            "/1",
            "/0",
            "/report/",
            "/lineage/",
            "/my/",
            "/my/push",
            "/group/",
            "/group/abc",
            "/api/public/",
            "/api/public/teams/f",
            "/api/health",
        ] {
            assert!(is_open(p), "{p} 应该免鉴权");
        }
    }

    /// P1-4：登录失败限流 —— 连续失败 N 次锁 M 分钟；成功清零；不同来源互不影响。
    #[test]
    fn login_limiter_locks_after_consecutive_failures() {
        let lim = LoginLimiter::new(3, Duration::from_secs(60));
        let a = "10.0.0.1";
        let b = "10.0.0.2";

        assert!(!lim.locked(a));
        assert!(!lim.record_failure(a));
        assert!(!lim.record_failure(a));
        assert!(!lim.locked(a), "没到阈值不应锁");
        assert!(lim.record_failure(a), "第 3 次连续失败应触发锁定");
        assert!(lim.locked(a), "达到阈值后来源被锁");
        assert!(lim.locked_seconds(a) > 0 && lim.locked_seconds(a) <= 60);
        // 别的来源不受牵连
        assert!(!lim.locked(b));
        // 成功登录清零
        lim.record_success(a);
        assert!(!lim.locked(a), "成功登录后应解锁");
        assert_eq!(lim.locked_seconds(a), 0);
        // 清零后重新计数，不会立刻再锁
        assert!(!lim.record_failure(a));
        assert!(!lim.locked(a));
    }

    /// P1-4：锁定到期自动解锁（把锁定时刻拨到过去）。
    #[test]
    fn login_limiter_unlocks_after_lockout_expires() {
        let lim = LoginLimiter::new(1, Duration::from_secs(60));
        let a = "10.0.0.9";
        assert!(lim.record_failure(a));
        assert!(lim.locked(a));
        // 手动把锁定时刻拨到过去，模拟时间流逝
        {
            let mut hits = lim.hits.lock().unwrap();
            let e = hits.get_mut(a).unwrap();
            e.locked_until = Some(Instant::now() - Duration::from_secs(1));
        }
        assert!(!lim.locked(a), "锁定期满应自动解锁并清零");
        // 解锁后重新计数：max=1 时下一次失败会再次触发锁定（这正是预期语义）
        assert!(lim.record_failure(a));
        assert!(lim.locked(a));
    }

    /// P1-4：XFF 只在直连方是回环地址（经本机反代）时才被信任。
    #[test]
    fn login_source_trusts_xff_only_from_loopback() {
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-forwarded-for", "203.0.113.7, 10.0.0.1".parse().unwrap());
        let loopback: std::net::SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let external: std::net::SocketAddr = "192.168.1.5:5000".parse().unwrap();

        assert_eq!(login_source_key(Some(loopback), &h), "203.0.113.7");
        // 直连方不是回环 → 不信任 XFF，用直连 IP
        assert_eq!(login_source_key(Some(external), &h), "192.168.1.5");
        // 没有 XFF → 用直连 IP
        assert_eq!(
            login_source_key(Some(loopback), &axum::http::HeaderMap::new()),
            "127.0.0.1"
        );
        // 没有直连地址（测试环境）→ 空 key，照样可用
        assert_eq!(login_source_key(None, &h), "");
    }
}
