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
//! fail-closed（2026-10-11 审计整改）：未配置 ADMIN_PASSWORD 时**没有兜底
//! 口令**，管理接口全部 503；main.rs 启动守卫默认直接拒绝启动（本地开发
//! 需显式 ALLOW_NO_ADMIN_PASSWORD=1，但此时管理接口依然全关）。

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
const ADMIN_PASSWORD_MIN_LEN: usize = 16;

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
    /// 管理员口令的 argon2id 哈希（None = 未配置 ADMIN_PASSWORD，fail-closed）
    password_hash: Option<String>,
    tokens: Mutex<HashMap<String, Instant>>,
    /// 一次性 ticket → 过期时刻（SSE / 日报图等只能把凭证放进 URL 的场景）
    tickets: Mutex<HashMap<String, Instant>>,
    ttl: Duration,
    /// P1-4：登录失败限流（同来源连续失败 N 次锁 M 分钟）。
    /// 2026-10-14 审计（P1）：展示端 /api/public/my/login 经 login_locked /
    /// login_record_* 委托方法复用这一个限流器（此前只有 /api/auth/login 在用，
    /// 公开端可绕开锁定爆破管理员口令），字段保持私有。
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
///
/// 2026-10-12 安全审计 H1 修复：改取 **最后一跳**。XFF 规范里每个代理把「来源」
/// 追加到链尾——自建 Caddy 是追加者，真实客户端在链上；取第一跳等于取攻击者
/// 随手伪造的值（`curl -H "X-Forwarded-For: 1.2.3.4"` 每次换桶，限流完全失效，
/// 审计实测复现）。直连反代场景下最后一跳 = 反代追加的直连 IP，客户端无法伪造。
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
    /// 是否配置了 ADMIN_PASSWORD（trim 后非空）。main.rs 启动守卫用它：
    /// 没配置默认拒绝启动（本地开发可显式 ALLOW_NO_ADMIN_PASSWORD=1 放行启动，
    /// 但管理接口仍然 fail-closed 全关，见 require_admin）。
    pub fn password_configured() -> bool {
        std::env::var("ADMIN_PASSWORD")
            .ok()
            .is_some_and(|p| !p.trim().is_empty())
    }

    pub fn from_env() -> Self {
        /* 审计 2026-10-11（fail-closed）：口令只认环境变量 ADMIN_PASSWORD，
        删掉内置兜底口令（硬编码在公开仓库里等于明文公开）。未配置时
        password_hash=None，管理接口全部 503，绝不再放行任何请求。 */
        let password_hash = match std::env::var("ADMIN_PASSWORD")
            .ok()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
        {
            Some(p) => {
                /* 2026-10-17 审计 4-P0-A：口令过短不得当作「已配置」放行（历史泄露值都
                短于 16 位，一律被长度下限挡住）。不满足时与「未配置」同语义
                （password_hash=None），require_admin 会 503 —— 启动日志给出可执行提示，
                但绝不降级成无鉴权。 */
                if admin_password_is_acceptable(&p) {
                    tracing::info!(
                        "管理员口令已从 ADMIN_PASSWORD 加载（argon2id 哈希驻留内存，不存明文）"
                    );
                    Some(hash_password(&p))
                } else {
                    tracing::error!(
                        len = p.chars().count(),
                        min_len = ADMIN_PASSWORD_MIN_LEN,
                        "ADMIN_PASSWORD 过短（<16 位）：管理接口已 fail-closed 全关（503），\
                         请立即轮换为 ≥16 位强随机口令后重启"
                    );
                    None
                }
            }
            None => {
                tracing::error!(
                    "ADMIN_PASSWORD 未配置，管理接口全部关闭（fail-closed）——请在 .env / 环境里配置后重启"
                );
                None
            }
        };
        let ttl = std::env::var("ADMIN_TOKEN_TTL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8 * 3600u64);
        Self {
            password_hash,
            tokens: Mutex::new(HashMap::new()),
            tickets: Mutex::new(HashMap::new()),
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
            // fail-closed：没配口令 → 任何输入都不通过
            None => false,
            Some(phc) => verify_password(input, phc),
        }
    }

    /// 展示端登录门（/api/public/my/login）专用：账号、密码两个字段填同一个
    /// 管理员口令时，允许以「管理员观看账号」身份进展示页。口令读自运行时
    /// 配置（ADMIN_PASSWORD 启动时哈希驻留内存），改 .env 后重启即生效。
    /// 未配置口令 → 一律 false（fail-closed）。
    pub(crate) fn verify_admin_password(&self, input: &str) -> bool {
        self.check_password(input)
    }

    /* ───── 登录失败限流（2026-10-14 审计 P1 修复）─────
    /api/public/my/login 的管理员快捷口令分支原先只过每 IP 每分钟 5 次的滑动窗口
    （public_write_allow），**没有**接这里连续失败锁 10 分钟的 LoginLimiter ——
    而那个 limiter 只挂在 /api/auth/login 上。结果是公网可以对这个公开端点上的
    管理员口令匀速慢速爆破（5 次/分/IP，换 IP 就重置），argon2 校验还白送攻击者算力。
    暴露三个委托方法给展示端登录用，key 仍由调用方按来源 IP 生成，跟 /api/auth/login
    共用同一个 limiter 实例与同一套阈值（5 次失败锁 10 分钟）。 */

    /// 该来源是否已被锁定（锁定期间拒绝，不跑 argon2）
    pub fn login_locked(&self, key: &str) -> bool {
        self.limiter.locked(key)
    }

    /// 剩余锁定秒数（响应体提示用）
    pub fn login_locked_seconds(&self, key: &str) -> u64 {
        self.limiter.locked_seconds(key)
    }

    /// 记一次失败；返回是否刚好达到阈值触发锁定
    pub fn login_record_failure(&self, key: &str) -> bool {
        self.limiter.record_failure(key)
    }

    /// 登录成功：清零该来源失败计数
    pub fn login_record_success(&self, key: &str) {
        self.limiter.record_success(key)
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
        // fail-closed：没配口令时任何 token 都不认
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

    // fail-closed（2026-10-11 审计整改）：未配置 ADMIN_PASSWORD 时管理接口
    // 一律 503 —— 宁可全关，也不能像旧逻辑那样「没配口令就放行一切」。
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
/// P1-4：登录失败限流 —— 同一来源连续 5 次失败锁 10 分钟，成功登录清零。
pub async fn login(
    State(st): State<std::sync::Arc<crate::state::AppState>>,
    axum::extract::ConnectInfo(remote): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let auth = &st.auth;
    // fail-closed：没配口令时登录接口同样关闭（不验口令、不计限流）
    if !auth.enabled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "服务端未配置管理员口令，管理接口已关闭" })),
        )
            .into_response();
    }
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
    // fail-closed：未配置口令时 enabled=false 且 ok=false（旧逻辑 !enabled → ok=true
    // 会让前端以为「这台机器不要口令」，与 require_admin 的全关语义自相矛盾）
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

    /// fail-closed：未配置口令（None）时口令校验与 token 校验一律拒绝，
    /// 任何输入都不放行（2026-10-11 审计整改，删掉内置兜底口令后的核心语义）。
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

    /// 2026-10-17 审计 4-P0-A：口令过短（含历史泄露值，它们都 <16 位）不得启用管理鉴权
    /// （fail-closed）；长度足够的强口令正常启用；未配置保持原 fail-closed 行为。
    /// 注意：本测试**不使用任何真实口令明文**，样例一律为明显的假值。
    #[test]
    fn admin_password_gate_fails_closed_when_too_short() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::env::var("ADMIN_PASSWORD").ok();

        // 过短 / 15 位差 1 / 纯空白：一律不启用管理鉴权
        for weak in [
            "short",
            "weak-pass-123",     // 明显假样例：13 位，过短
            "abc123456789012",   // 15 位，差 1 位
            "abc1234567890123 ", // 16 位但 trim 后 15 位（首尾空白不算强度）
            "   ",
        ] {
            std::env::set_var("ADMIN_PASSWORD", weak);
            let auth = Auth::from_env();
            assert!(!auth.enabled(), "过短口令 {weak:?} 不应启用管理鉴权");
            assert!(!auth.check_password(weak), "fail-closed 时任何输入都不通过");
            assert!(!auth.verify("anything"));
        }

        // 未配置：保持原行为（管理接口全关）
        std::env::remove_var("ADMIN_PASSWORD");
        assert!(!Auth::from_env().enabled());

        // ≥16 位强口令：正常启用，且能通过校验
        let strong = "S3cure-Admin-Phrase-2026-10-17";
        std::env::set_var("ADMIN_PASSWORD", strong);
        let auth = Auth::from_env();
        assert!(auth.enabled(), "16 位以上口令应启用管理鉴权");
        assert!(auth.check_password(strong));
        assert!(!auth.check_password("wrong-password"));

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
    /// 2026-10-12 安全审计 H1：改取**最后一跳**——第一跳是客户端可伪造的值，
    /// 每次换头即换限流桶（实测 8 连发全过）；链尾才是反代追加的真实来源。
    #[test]
    fn login_source_trusts_xff_only_from_loopback() {
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-forwarded-for", "203.0.113.7, 10.0.0.1".parse().unwrap());
        let loopback: std::net::SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let external: std::net::SocketAddr = "192.168.1.5:5000".parse().unwrap();

        // 最后一跳 = 10.0.0.1（反代追加的直连来源，客户端无法篡改链尾之后的部分）
        assert_eq!(login_source_key(Some(loopback), &h), "10.0.0.1");
        // 单跳 XFF（无中间代理）：链尾即自身
        let mut h1 = axum::http::HeaderMap::new();
        h1.insert("x-forwarded-for", "203.0.113.9".parse().unwrap());
        assert_eq!(login_source_key(Some(loopback), &h1), "203.0.113.9");
        // 伪造者把假值放第一跳也无效——仍然取链尾
        let mut hf = axum::http::HeaderMap::new();
        hf.insert(
            "x-forwarded-for",
            "1.2.3.4, 5.6.7.8, 203.0.113.10".parse().unwrap(),
        );
        assert_eq!(login_source_key(Some(loopback), &hf), "203.0.113.10");
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
