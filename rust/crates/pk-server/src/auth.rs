//! 管理员鉴权：一个共享口令换内存 token。
//!
//! - 除公开端点外，所有 /api 必须带 `Authorization: Bearer <token>`
//! - **query 只接受一次性 ticket**（`?ticket=`，60 秒有效、用过即焚）：
//!   EventSource（/api/admin/events）/ <img>（日报 SVG）等无法带
//!   Authorization 头的场景，先 `POST /api/auth/ticket`（Bearer 长效 token）
//!   换 ticket，再把 ticket 拼进 URL
//! - 长效 token 一律走 Authorization 头，绝不进 URL（会留在代理日志 /
//!   浏览器历史 / Referer 里）
//! - token 存在内存里，进程重启即失效；空闲超过 ADMIN_TOKEN_TTL 自动清理
//! - 登录失败按来源计数：连续 5 次失败锁 10 分钟（P1-4），成功登录即清零
//!
//! 口令哈希用 argon2id（PHC 字符串常驻内存），校验走 `PasswordVerifier`
//! 的常数时间比较，避免逐字节提前返回泄露前缀。
//!
//! 口令（2026-10-19 用户口径）：**默认 `shuai4444`**，未配置 / 空 `ADMIN_PASSWORD`
//! 时自动生效 —— 管理端永远有一个可用口令，不再出现「没配口令整片 503」。
//! `ADMIN_PASSWORD` 环境变量优先（本地 / 生产都可覆盖）；口令 <16 位或正在用默认值
//! 时启动日志给 WARN，提醒尽快轮换。旧口径（2026-10-11 审计 fail-closed：未配口令
//! 拒绝启动 / 503）按用户要求作废，`ALLOW_NO_ADMIN_PASSWORD` 一并下线。

use std::collections::HashMap;
use std::sync::Mutex;
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

/// P1-4：口令哈希改 argon2id。启动时对管理员口令现算一次（约几十毫秒），
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

/// 管理员口令最小长度（2026-10-17 审计 4-P0-A）：共享口令是整个管理面的唯一凭证，
/// 短口令在「按来源 IP 限流」下仍可能被分布式撞库；本机历史上还出现过口令复用。
/// 2026-10-19 起不再 fail-closed，只用于启动日志 WARN（默认口令本身就短于该阈值）。
const ADMIN_PASSWORD_MIN_LEN: usize = 16;

/// 默认管理员口令（2026-10-19 用户口径「后端的口令默认为 shuai4444」）。
///
/// 未配置 / 空 `ADMIN_PASSWORD` 时用它；配了环境变量则环境变量优先。
/// 与旧版内置兜底口令的区别：这里**明确**是产品默认值（用户定的），
/// 不是偷偷兜底 —— 启动日志会 WARN 提醒生产环境覆盖。
pub const DEFAULT_ADMIN_PASSWORD: &str = "shuai4444";

/// 管理员口令强度闸门（trim 后）：**只做长度下限**。
///
/// 为什么不再内置「已知泄露口令」名单（2026-10-17 审计 4-P0-A 二次整改）：
/// 公开仓库里再写一次口令字面量，等于本轮 P0 清理白做 —— 名单本身就会成为新的泄露源。
/// 历史上泄露的旧管理口令 / 旧开发口令**都短于 16 位**，长度下限已经能把它们挡在门外；
/// 若以后要拦 ≥16 位的泄露值，应先在依赖里引入 sha2，然后只存 SHA-256 十六进制常量做
/// 比对（口令明文绝不落盘）。本函数只决定「是否允许启用管理鉴权」，不改写调用方输入。
fn admin_password_is_acceptable(p: &str) -> bool {
    p.trim().chars().count() >= ADMIN_PASSWORD_MIN_LEN
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
    /// 管理员口令的 argon2id 哈希。`from_env()` 恒为 `Some`（未配置时用默认口令）；
    /// `None` 只出现在测试构造里，表示「完全没有口令」的防御路径。
    password_hash: Option<String>,
    tokens: Mutex<HashMap<String, Instant>>,
    /// 一次性 ticket → 过期时刻（SSE / 日报图等只能把凭证放进 URL 的场景）
    tickets: Mutex<HashMap<String, Instant>>,
    ttl: Duration,
    /* 2026-10-17 用户口径「去除登录限速」：LoginLimiter（连续失败 5 次锁 10 分钟）
    已连同 /api/auth/login 上的挂载一起移除；展示端（verify / my_login）的限速
    同日更早也已摘除。防爆破层次现在只剩：argon2id 校验本身（故意慢哈希）+
    默认/短口令的启动 WARN（2026-10-19 起口令长度不再 fail-closed）。
    若后续要恢复限流，参考 git 历史里的 LoginLimiter。 */
}

/// 登录来源：日志用（2026-10-17 限速移除后仅打日志，不再做限流 key）。
///
/// 2026-10-17 修正「后端登录 IP 不对」：生产链路是 Cloudflare → Caddy →
/// docker-proxy → 容器，容器里 ConnectInfo 看到的是 Docker 网关（如 172.26.0.1），
/// 永远不是 127.0.0.1——旧逻辑只在回环直连时才信 XFF，结果所有来源都记成了网关。
/// 新口径与 ipinfo::request_ip / admin.rs client_ip_for_limit 一致：
/// - 直连方是**回环或私网**（Docker 网关在内）才信任转发头；
/// - `X-Real-IP` 优先（Caddy 显式注入的 CF-Connecting-IP，即真实客户端 IP）；
/// - 否则取 XFF 链尾（最接近服务端的一跳，反代追加、客户端难伪造）；
/// - 公网直连一律用 ConnectInfo（外来伪造头直接忽略）。
fn login_source_key(
    remote: Option<std::net::SocketAddr>,
    headers: &axum::http::HeaderMap,
) -> String {
    let direct = remote.map(|a| a.ip().to_string()).unwrap_or_default();
    let trusted = remote
        .map(|a| crate::ipinfo::is_internal(&a.ip()))
        .unwrap_or(true);
    if trusted {
        if let Some(ip) = headers
            .get("x-real-ip")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<std::net::IpAddr>().ok())
        {
            return ip.to_string();
        }
        if let Some(xff) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            // 取最后一跳（最接近服务端的一跳，反代追加、不可伪造）
            return xff
                .rsplit(',')
                .next()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or(direct);
        }
    }
    direct
}

impl Default for Auth {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Auth {
    pub fn from_env() -> Self {
        /* 2026-10-19 用户口径「后端的口令默认为 shuai4444」：
        - `ADMIN_PASSWORD` 配了（trim 后非空）→ 环境变量优先，口令照常启用；
        - 没配 / 空串 / 纯空白 → 用 [`DEFAULT_ADMIN_PASSWORD`]。
        旧口径（2026-10-11 审计 fail-closed：未配 → 503、短于 16 位 → 503）已作废：
        现在任何时候都有可用口令，不会再把整个管理端锁死。运维可见性靠两条 WARN：
        正在用默认口令、环境变量口令短于 16 位（密码明文绝不进日志）。 */
        let env_password = std::env::var("ADMIN_PASSWORD")
            .ok()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty());
        let password = match env_password {
            Some(p) => {
                if !admin_password_is_acceptable(&p) {
                    tracing::warn!(
                        len = p.chars().count(),
                        min_len = ADMIN_PASSWORD_MIN_LEN,
                        "ADMIN_PASSWORD 短于 16 位：按用户口径照常启用，但建议尽快轮换为强口令"
                    );
                } else {
                    tracing::info!(
                        "管理员口令已从 ADMIN_PASSWORD 加载（argon2id 哈希驻留内存，不存明文）"
                    );
                }
                p
            }
            None => {
                tracing::warn!(
                    "未配置 ADMIN_PASSWORD：已启用默认管理口令（见 auth::DEFAULT_ADMIN_PASSWORD \
                     与 AGENTS.md）；生产环境请在 .env 覆盖成强口令后重启"
                );
                DEFAULT_ADMIN_PASSWORD.to_string()
            }
        };
        let password_hash = Some(hash_password(&password));
        let ttl = std::env::var("ADMIN_TOKEN_TTL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8 * 3600u64);
        Self {
            password_hash,
            tokens: Mutex::new(HashMap::new()),
            tickets: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(ttl),
        }
    }

    /// 是否开启了鉴权
    pub fn enabled(&self) -> bool {
        self.password_hash.is_some()
    }

    pub fn check_password(&self, input: &str) -> bool {
        match &self.password_hash {
            // 防御式：正常 from_env() 恒有口令（未配置时默认口令兜底），None 仅测试构造
            None => false,
            Some(phc) => verify_password(input, phc),
        }
    }

    /// 展示端登录门（/api/public/my/login）专用：账号、密码两个字段填同一个
    /// 管理员口令时，允许以「管理员观看账号」身份进展示页。口令读自运行时
    /// 配置（ADMIN_PASSWORD 优先，未配置用默认口令），改 .env 后重启即生效。
    /// 正常 from_env() 恒有口令；`None`（仅测试构造）→ 一律 false。
    pub(crate) fn verify_admin_password(&self, input: &str) -> bool {
        self.check_password(input)
    }

    /* ───── 登录失败限流已整条移除（2026-10-17 用户口径「去除登录限速」）─────
    LoginLimiter（连续失败 5 次锁 10 分钟）原先先后挂在展示端登录（verify /
    my_login，2026-10-14 审计 P1 接入）与后台 /api/auth/login（P1-4）上，
    今日全部摘除。防爆破层次现在只剩：argon2id 故意慢哈希 + 默认/短口令 WARN。
    要恢复限流请翻 git 历史找回 LoginLimiter。 */

    pub fn issue(&self) -> String {
        let token = random_token();
        let mut map = self.tokens.lock().expect("token 锁 poisoned");
        let now = Instant::now();
        map.retain(|_, at| now.duration_since(*at) < self.ttl);
        map.insert(token.clone(), now);
        token
    }

    pub fn verify(&self, token: &str) -> bool {
        // 防御式：没有口令（仅测试构造）时任何 token 都不认；正常 from_env 恒有口令
        if !self.enabled() {
            return false;
        }
        let mut map = self.tokens.lock().expect("token 锁 poisoned");
        let now = Instant::now();
        map.retain(|_, at| now.duration_since(*at) < self.ttl);
        map.contains_key(token)
    }

    pub fn revoke(&self, token: &str) {
        self.tokens.lock().expect("token 锁 poisoned").remove(token);
    }

    /// 签发一次性 ticket（调用方必须已验过 Bearer 长效 token，见 issue_ticket handler）
    pub fn issue_ticket(&self) -> String {
        let mut map = self.tickets.lock().expect("ticket 锁 poisoned");
        ticket_issue(&mut map, Instant::now())
    }

    /// 核销一次性 ticket：存在且未过期 → 立即作废并返回 true
    pub fn redeem_ticket(&self, ticket: &str) -> bool {
        let mut map = self.tickets.lock().expect("ticket 锁 poisoned");
        ticket_redeem(&mut map, ticket, Instant::now())
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

/* --------------------------- 一次性 ticket --------------------------- */

/// ticket 有效期（秒）。SSE / 日报 SVG 等只能把凭证放进 URL 的场景专用：
/// 先拿长效 Bearer token 换 ticket，再把 ticket 拼进 query。一次性 + 短 TTL，
/// 即使 URL 被记入代理日志 / 浏览器历史，泄露窗口也被压到最小。
pub const TICKET_TTL_SECS: u64 = 60;
/// ticket 字符表：字母数字，免 URL 转义
const TICKET_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
const TICKET_LEN: usize = 32;

/// 签发一枚 ticket 并登记过期时刻；顺手惰性清理过期项（不需要后台清扫任务）。
/// 纯函数（显式传 map 与 now），方便单测。
fn ticket_issue(map: &mut HashMap<String, Instant>, now: Instant) -> String {
    map.retain(|_, exp| *exp > now);
    let t = crate::random::random_code(TICKET_LEN, TICKET_ALPHABET);
    map.insert(t.clone(), now + Duration::from_secs(TICKET_TTL_SECS));
    t
}

/// 核销 ticket：存在且未过期 → 立即删除并返回 true（一次性）；否则 false。
/// 同样顺手惰性清理过期项，map 不会无限膨胀。
fn ticket_redeem(map: &mut HashMap<String, Instant>, ticket: &str, now: Instant) -> bool {
    map.retain(|_, exp| *exp > now);
    map.remove(ticket).is_some()
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
    /* 「微信对接」独立短路径（2026-10-17 修复，审计 5-P0）：
    main.rs 把 `/wechat` 与 `/my/{key}` 并列注册成**公开页**路由（深链/刷新要走它），
    但这里原先漏了它 —— 未登录访问被 require_admin 拦成
    401 + `{"error":"需要管理员口令"}`，主播点运营发来的链接只看到一行 JSON、页面空白。
    规范路径 `/my/wechat` 因为在 `/my/` 前缀内一直是好的，只有这条短链挂了。
    用**精确路径**写法（open_prefix_hit 认「路径正好等于前缀」），
    不用 starts_with，避免 `/wechat-xxx` 这类路径被顺带放过。 */
    "/wechat",
    "/api/public/",
    "/api/auth/session",
    "/api/auth/login",
    "/api/health",
];

/// P2-20：带边界的前缀匹配。命中条件（任一）：
/// - `path` 正好等于 `prefix`（去掉尾斜杠后比较）：`/my` 命中 `/my/`
/// - `path` 以 `prefix` 开头且 `prefix` 以 `/` 结尾：`/st/f` 命中 `/st/`
/// - `path` 以 `{prefix}/` 开头：不带斜杠的前缀写法的子路径
///
/// `/api/public-x` 不会命中 `/api/public/`，`/10` 不会命中 `/1`，`/myfile` 不会命中 `/my`。
fn open_prefix_hit(prefix: &str, path: &str) -> bool {
    if !path.starts_with(prefix) {
        // 白名单写成 `/my/`，但前端 router 用的是不带尾斜杠的 `/my`：
        // 去掉尾斜杠后等值也算命中。边界仍然成立：`/my-page`、`/reports` 不命中。
        return prefix.strip_suffix('/').is_some_and(|base| path == base);
    }
    // 前缀以 / 结尾时天然带边界（下一个字符不可能让两段拼成别的段）；
    // 不带 / 的前缀（/1、/api/health 等）要么路径到此正好结束，要么紧跟 /。
    prefix.ends_with('/')
        || path.len() == prefix.len()
        || path.as_bytes().get(prefix.len()) == Some(&b'/')
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

    if is_open(path) {
        return next.run(req).await;
    }

    // 防御式：正常 from_env 恒有口令（未配置时默认口令兜底），这里只在
    // 「无口令实例」（仅测试构造）时兜 503 —— 绝不出现「无口令却放行一切」。
    if !auth.enabled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "服务端未配置管理员口令，管理接口已关闭" })),
        )
            .into_response();
    }

    let mut ok = Auth::token_of(req.headers()).is_some_and(|t| auth.verify(&t));
    // EventSource / <img src> 无法带 Authorization 头：query **只接受一次性
    // ticket**（?ticket=...，60 秒、用过即焚），不再接受长效 token ——
    // 长效 token 进 URL 会留在代理日志与浏览器历史里，泄露面太大。
    if !ok {
        if let Some(q) = req.uri().query() {
            for pair in q.split('&') {
                if let Some(v) = pair.strip_prefix("ticket=") {
                    ok = auth.redeem_ticket(v);
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
/// 2026-10-17 用户口径「去除登录限速」：原先这里挂 LoginLimiter（连续失败 5 次
/// 锁 10 分钟），本 handler 已整条移除 —— 口令错就 401，不计数、不锁定。
/// 地区门禁与登录日志不在本层（管理端无地区门禁）。source 仅用于日志。
pub async fn login(
    State(st): State<std::sync::Arc<crate::state::AppState>>,
    axum::extract::ConnectInfo(remote): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let auth = &st.auth;
    // 防御式：正常 from_env 恒有口令，这里只在「无口令实例」（仅测试构造）时兜 503
    if !auth.enabled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "服务端未配置管理员口令，管理接口已关闭" })),
        )
            .into_response();
    }
    let source = login_source_key(Some(remote), &headers);

    let password = body.get("password").and_then(|v| v.as_str()).unwrap_or("");
    if !auth.check_password(password) {
        // 2026-10-17 用户口径「去除登录限速」：失败不计数、不锁定，仅记日志
        tracing::warn!(source = %source, "后台登录口令不正确");
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

/// `POST /api/auth/ticket`：长效 Bearer token 换一次性 ticket（60 秒、用过即焚）。
///
/// 给 EventSource（/api/admin/events）与 <img>（日报 SVG）这类无法带
/// Authorization 头的场景用：前端先 POST 这里拿 ticket，再拼 `?ticket=`。
/// 这里**显式再验一次 Bearer 头**而不是只依赖 require_admin —— query ticket
/// 也能过中间件，不拦的话一张 ticket 就能续杯新 ticket，一次性语义就空了。
pub async fn issue_ticket(
    State(st): State<std::sync::Arc<crate::state::AppState>>,
    headers: axum::http::HeaderMap,
) -> Response {
    let auth = &st.auth;
    if !auth.enabled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "服务端未配置管理员口令，管理接口已关闭" })),
        )
            .into_response();
    }
    let ok = Auth::token_of(&headers).is_some_and(|t| auth.verify(&t));
    if !ok {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "需要管理员口令" })),
        )
            .into_response();
    }
    let ticket = auth.issue_ticket();
    Json(json!({ "ticket": ticket, "expiresInSec": TICKET_TTL_SECS })).into_response()
}

/// `GET /api/auth/session`：前端启动时问一句「要不要口令 / 手里这 token 还能用吗」
pub async fn session(
    State(st): State<std::sync::Arc<crate::state::AppState>>,
    headers: axum::http::HeaderMap,
) -> Response {
    let auth = &st.auth;
    let enabled = auth.enabled();
    // 防御式：正常 from_env 恒 enabled=true（默认口令兜底）；恒发 ticket 不受影响。
    // 保留「enabled=false 且 ok=false」的语义，避免前端把无口令实例误认成免登录。
    let ok = enabled && Auth::token_of(&headers).is_some_and(|t| auth.verify(&t));
    Json(json!({ "enabled": enabled, "ok": ok })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 环境变量测试串行锁：`Auth::from_env()` 读的是进程级 env，
    /// 多个 #[test] 并行会互相踩（本模块目前只有本用例改 env，仍统一加锁防回归）。
    static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn auth_with(password: Option<&str>) -> Auth {
        Auth {
            password_hash: password.map(hash_password),
            tokens: Mutex::new(HashMap::new()),
            tickets: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(3600),
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

    /// 无口令实例（仅测试构造；from_env 已不会产生 None）：
    /// 口令校验与 token 校验一律拒绝，任何输入都不放行。
    #[test]
    fn disabled_auth_rejects_everything() {
        let a = auth_with(None);
        assert!(!a.enabled());
        assert!(!a.check_password(""));
        assert!(!a.check_password("any-password"));
        // 没配口令时即便手里有「token」也不认
        assert!(!a.verify("anything"));
        // ticket 也同样发得出但要有 token 才发 —— 这里只验证核销不认凭空值
        assert!(!a.redeem_ticket("no-such-ticket"));
    }

    /// 2026-10-19 用户口径「后端的口令默认为 shuai4444」：
    /// 未配置 / 空串 / 纯空白 → 启用内置默认口令；环境变量配了 → 环境变量优先
    /// （短口令只 WARN，不再 fail-closed）。注意：本测试**不使用任何真实口令明文**
    /// （DEFAULT_ADMIN_PASSWORD 是用户点名写入代码的公开默认值，断言时用常量引用），
    /// 强口令样例一律为明显的假值。
    #[test]
    fn admin_password_defaults_to_builtin_when_unset() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::env::var("ADMIN_PASSWORD").ok();

        // 未配置：默认口令兜底，管理鉴权启用
        std::env::remove_var("ADMIN_PASSWORD");
        let auth = Auth::from_env();
        assert!(auth.enabled(), "未配置口令应启用默认口令，管理鉴权不关闭");
        assert!(
            auth.check_password(DEFAULT_ADMIN_PASSWORD),
            "未配置时应当用 DEFAULT_ADMIN_PASSWORD 校验"
        );
        assert!(!auth.check_password("wrong-password"));

        // 纯空白：等同未配置，仍走默认口令
        std::env::set_var("ADMIN_PASSWORD", "   ");
        let auth = Auth::from_env();
        assert!(auth.enabled(), "纯空白环境变量等同未配置，仍应启用");
        assert!(
            auth.check_password(DEFAULT_ADMIN_PASSWORD),
            "纯空白时应当走默认口令"
        );
        assert!(!auth.check_password("   "), "空白本身不该当口令用");

        // 显式短口令：环境变量优先，照常启用（不再 fail-closed，只 WARN）
        let short = "short-pw";
        std::env::set_var("ADMIN_PASSWORD", short);
        let auth = Auth::from_env();
        assert!(auth.enabled(), "显式配置的短口令也应启用管理鉴权");
        assert!(auth.check_password(short), "环境变量口令优先于默认口令");
        assert!(
            !auth.check_password(DEFAULT_ADMIN_PASSWORD),
            "配了环境变量后默认口令不应继续生效"
        );

        // ≥16 位强口令：环境变量优先，正常校验
        let strong = "S3cure-Admin-Phrase-2026-10-19";
        std::env::set_var("ADMIN_PASSWORD", strong);
        let auth = Auth::from_env();
        assert!(auth.enabled(), "强口令应启用管理鉴权");
        assert!(auth.check_password(strong));
        assert!(!auth.check_password(DEFAULT_ADMIN_PASSWORD));

        // 还原环境，避免影响其它用例
        match old {
            Some(v) => std::env::set_var("ADMIN_PASSWORD", v),
            None => std::env::remove_var("ADMIN_PASSWORD"),
        }
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
            tickets: Mutex::new(HashMap::new()),
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

    /// ticket 纯逻辑：一次性 + TTL + 惰性清理（显式传 map/now，不依赖锁与系统时钟）
    #[test]
    fn ticket_one_time_and_ttl() {
        let mut map = HashMap::new();
        let now = Instant::now();

        let t = ticket_issue(&mut map, now);
        assert_eq!(t.len(), 32, "ticket 固定 32 位");
        assert!(
            t.bytes().all(|b| b.is_ascii_alphanumeric()),
            "ticket 只用字母数字，免 URL 转义"
        );
        // 未过期：核销成功
        assert!(ticket_redeem(&mut map, &t, now));
        // 一次性：第二次核销必须失败
        assert!(!ticket_redeem(&mut map, &t, now));
        // 不存在的 ticket 失败
        assert!(!ticket_redeem(&mut map, "no-such-ticket", now));

        // 过期：TTL 之后核销失败，且被惰性清理出 map
        let t2 = ticket_issue(&mut map, now);
        let later = now + Duration::from_secs(TICKET_TTL_SECS + 1);
        assert!(!ticket_redeem(&mut map, &t2, later), "过期 ticket 不能用");
        assert!(map.is_empty(), "过期项应被惰性清理");
    }

    /// ticket 经 Auth 的完整往返：签发 → 核销成功 → 再核销失败
    #[test]
    fn ticket_via_auth_roundtrip() {
        let a = auth_with(Some("pw"));
        let t = a.issue_ticket();
        assert!(a.redeem_ticket(&t));
        assert!(!a.redeem_ticket(&t), "一次性：第二次必须失败");
    }

    /// 两枚 ticket 互不干扰：核销一张不影响另一张
    #[test]
    fn tickets_are_independent() {
        let a = auth_with(Some("pw"));
        let (t1, t2) = (a.issue_ticket(), a.issue_ticket());
        assert_ne!(t1, t2);
        assert!(a.redeem_ticket(&t1));
        assert!(a.redeem_ticket(&t2));
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
            // 不带尾斜杠的展示端子路径入口：白名单写的是 `/my/`，
            // 但浏览器和前端 router 用的是 `/my`（历史上这里漏了，
            // `/my` 被鉴权中间件拦成 401，展示端「我的」整页打不开）
            "/my",
            "/group",
            "/report",
            "/lineage",
        ] {
            assert!(is_open(p), "{p} 应该免鉴权");
        }
    }
    /* LoginLimiter 的两条单元测试（连续失败锁定 / 到期自动解锁）已随限速
    一起移除（2026-10-17「去除登录限速」口径）；要恢复请翻 git 历史。 */

    /// 2026-10-17 修正「后端登录 IP 不对」：来源提取的信任边界从「仅回环直连」
    /// 扩为「回环或私网直连」（生产容器里 ConnectInfo 是 Docker 网关 172.26.0.1），
    /// 且 `X-Real-IP`（Caddy 注入的 CF-Connecting-IP）优先于 XFF 链尾。
    /// 公网直连仍然一律用 ConnectInfo，外来伪造头直接忽略。
    #[test]
    fn login_source_trusts_headers_from_private_direct() {
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-forwarded-for", "203.0.113.7, 10.0.0.1".parse().unwrap());
        let loopback: std::net::SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let external: std::net::SocketAddr = "203.0.113.99:5000".parse().unwrap();
        // 64 生产：docker-proxy 直连方 = Docker 网关（172.26.0.1，属私网）
        let gateway: std::net::SocketAddr = "172.26.0.1:5000".parse().unwrap();

        // XFF 链尾（回环直连）
        assert_eq!(login_source_key(Some(loopback), &h), "10.0.0.1");
        // XFF 链尾（私网网关直连 —— 修复前这里会错误回落成 172.26.0.1）
        assert_eq!(login_source_key(Some(gateway), &h), "10.0.0.1");
        // X-Real-IP 优先于 XFF（Caddy 注入的 CF 真实客户端 IP）
        let mut hr = axum::http::HeaderMap::new();
        hr.insert("x-real-ip", "203.0.113.77".parse().unwrap());
        hr.insert("x-forwarded-for", "1.2.3.4".parse().unwrap());
        assert_eq!(login_source_key(Some(loopback), &hr), "203.0.113.77");
        assert_eq!(login_source_key(Some(gateway), &hr), "203.0.113.77");
        // X-Real-IP 非法值 → 回落 XFF
        let mut hbad = axum::http::HeaderMap::new();
        hbad.insert("x-real-ip", "not-an-ip".parse().unwrap());
        hbad.insert("x-forwarded-for", "203.0.113.9".parse().unwrap());
        assert_eq!(login_source_key(Some(loopback), &hbad), "203.0.113.9");
        // 公网直连 → 不信任任何转发头，用直连 IP（旧测试用 192.168.1.5 属私网，
        // 现在会信任头，改用真公网地址验证这条边界）
        let mut hforge = axum::http::HeaderMap::new();
        hforge.insert("x-real-ip", "1.2.3.4".parse().unwrap());
        assert_eq!(login_source_key(Some(external), &hforge), "203.0.113.99");
        // 没有 XFF → 用直连 IP
        assert_eq!(
            login_source_key(Some(loopback), &axum::http::HeaderMap::new()),
            "127.0.0.1"
        );
        // 没有直连地址（测试环境）→ 视为可信，走头；无头时空 key
        assert_eq!(login_source_key(None, &h), "10.0.0.1");
        assert_eq!(login_source_key(None, &axum::http::HeaderMap::new()), "");
    }
}
