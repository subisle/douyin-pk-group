//! public_teams::admin —— 自 public_teams.rs 按业务域拆分（对外语义不变）。

use super::*;

/// 展示端账号 store 层 `Result<_, String>` 错误 → ApiError 的统一分流口（2026-10-16）。
///
/// store 层（`store::my_account`）把**业务拒绝**包上 `BIZ::` 前缀（见
/// `store::my_account::BIZ_PREFIX` 的文档）：那是给人看的人话，400 原文原样下发
/// —— 修复「管理端改绑失败只看到『请求参数有误』」的报障（真实原因
/// 「该主播已存在绑定的账号」只进了日志，运营无从下手）。
/// 无前缀 = 库层故障（sqlx/PG 原文，可能带表名/约束名/SQL 片段）→ 500 走
/// 通用文案兜底，原文只留日志（2026-10-14 审计 P2 的口径不变）。
fn account_store_err(e: String) -> ApiError {
    if crate::store::my_account::is_biz(&e) {
        ApiError::bad_request(crate::store::my_account::biz_msg(&e).to_string())
    } else {
        tracing::error!(error = %e, "展示端账号写库失败");
        ApiError::from(anyhow::anyhow!(e))
    }
}

/* --------------------- 我的页面账号（管理端手动添加） --------------------- */

/// GET /api/admin/my-accounts：账号列表
pub async fn admin_my_accounts(State(st): State<ArcState>) -> ApiResult {
    Ok(Json(
        json!({ "items": crate::store::my_account_list(&st).await }),
    ))
}

/// POST /api/admin/my-accounts {name, phone, pass?}：手动设置账号（存在则覆盖更新；手机号选填）。
/// 2026-10-13 产品拍板：`pass` 留空时按运营惯例用**手机号后四位**当初始密码
/// （2026-10-12 曾改成 CSPRNG 随机，因运营转达成本高、主播记不住而回退）；
/// 随响应下发一次（{password}，仅此一次，请管理员当场转达）；显式传 pass 的旧调用照旧兼容。
/// 设置成功后吊销该账号全部 my token（改密/开号后旧登录态必须失效）。
pub async fn admin_my_account_set(
    State(st): State<ArcState>,
    Json(body): Json<Value>,
) -> ApiResult {
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let phone = body
        .get("phone")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (pass, generated) = match body
        .get("pass")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(p) => (p.to_string(), false),
        // 2026-10-13：未指定密码时按运营惯例生成**手机号后四位**（原先是随机 10 位）。
        None => (crate::store::my_account::initial_password(phone), true),
    };
    crate::store::my_account_set(&st, name, phone, &pass)
        .await
        .map_err(account_store_err)?;
    /* 可看团别（迁移 024）：**只在请求里显式带了 viewTeam 时才改**。
    不带 = 保持原值 —— 后台「改密码」「改手机号」这些常规操作（前端表单没这个字段）
    不能顺手把账号的可见范围清成 Inherit。非法值由 normalize 收敛成 ""（沿用旧口径）。 */
    let view_team_written = match body.get("viewTeam").and_then(|v| v.as_str()) {
        Some(raw) => {
            let vt = crate::store::ViewTeam::parse(raw);
            crate::store::my_account_set_view_team(&st, name, vt).await;
            true
        }
        None => false,
    };
    /* 写后回读校验（2026-10-13）：之前出现过「接口返回 ok 但库里没这一行」的静默失败
    （写池指向了非预期实例 / 事务被丢弃），运营看到绿色「已保存」却根本没账号。
    这里落地后立刻回读，读不到就报错，绝不再假成功。 */
    let saved = crate::store::my_account_get(&st, name).await;
    /* 判据是「查得到这一行」而不是 JSON 里的 found 键：
    found 是 admin_my_account_get（GET 单条）自己补的展示字段，
    store::my_account_get 返回的裸值里没有它 —— 上一版读 v["found"] 恒 false，
    导致每次开号/改密明明成功却一律 500「写入后校验失败」（2026-10-13 修复）。 */
    let verified = saved.is_some();
    if !verified {
        return Err(anyhow::anyhow!(
            "账号「{}」写入后校验失败（库里读不到），请检查写库配置后重试",
            name
        )
        .into());
    }
    /* 改了可见范围必须吊销旧 token：旧 token 里挂着改之前算出来的团别，
    不吊销的话范围限制当场就被绕过去了（拿着旧 token 继续看全团）。 */
    my_tokens_revoke(name);
    tracing::info!(account = %name, generated, view_team_written,
        "管理端设置展示端账号（旧 token 已吊销，已回读校验）");
    Ok(Json(json!({
        "ok": true,
        // 随机生成的密码仅此一次下发；管理员显式指定的密码不回显
        "password": if generated { json!(pass) } else { Value::Null },
    })))
}

/// GET /api/admin/my-accounts/{name}：单个账号（右键弹窗回填；没有则 found:false）
pub async fn admin_my_account_get(
    State(st): State<ArcState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> ApiResult {
    let mut v = match crate::store::my_account_get(&st, &name).await {
        Some(v) => v,
        None => return Ok(Json(json!({ "found": false }))),
    };
    v["found"] = json!(true);
    Ok(Json(v))
}

/// DELETE /api/admin/my-accounts/{name}：删除账号（幂等，不存在也返回 ok）。
/// 删号同时吊销该账号全部 my token（内存 token 24h TTL，不能等它自然过期）。
pub async fn admin_my_account_delete(
    State(st): State<ArcState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> ApiResult {
    crate::store::my_account_delete(&st, &name).await;
    my_tokens_revoke(&name);
    Ok(Json(json!({ "ok": true })))
}

/// POST /api/admin/my-accounts/{name}/bind {personId}：管理员改绑主播。
/// 用户口径（2026-10-10）：绑定之后用户自己不能解绑，**只有管理员能改**。
/// 账号名会随主播姓名改写（账号名本来就是主播姓名的绑定关系；登录照样支持手机号）。
pub async fn admin_my_account_bind(
    State(st): State<ArcState>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let person_id = body
        .get("personId")
        .and_then(|v| v.as_i64())
        .or_else(|| {
            body.get("personId")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
        })
        .filter(|v| *v > 0)
        .ok_or_else(|| anyhow::anyhow!("请选择要绑定的主播"))?;
    let r = crate::store::my_account_admin_bind(&st, &name, person_id)
        .await
        .map_err(account_store_err)?;
    // 改绑后吊销旧登录态：账号名会被改写成主播姓名，新旧两个名字都吊销
    // （拿着旧 token 的人必须重新登录才能拿到新身份，审计 2026-10-12）
    my_tokens_revoke(&name);
    my_tokens_revoke(&r.name);
    tracing::info!(account = %name, anchor = %r.name, team = %r.team, "管理员改绑展示端账号（旧 token 已吊销）");
    Ok(Json(json!({
        "ok": true, "name": r.name, "team": r.team, "personId": r.person_id,
    })))
}

/// PUT /api/admin/my-accounts/{name}/view-team {viewTeam}：**只改可看团别，不动密码**。
///
/// 为什么不复用 `POST /api/admin/my-accounts`：那个端点不传 `pass` 时会**自动生成
/// 新密码**（手机号后四位，见 `admin_my_account_set`）——「只想把饭团改成女团」
/// 会顺手把她的密码重置掉，运营当场就转达不出去了。分权限和改口令是两件事，
/// 这里只做前者，且**不碰 pass 列**。
///
/// 改完吊销该账号全部 my token：旧 token 里挂着改之前算出来的团别，不吊销的话
/// 范围限制当场就被绕过去（拿着旧 token 继续看全团）。
pub async fn admin_my_account_set_view_team(
    State(st): State<ArcState>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let raw = body
        .get("viewTeam")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("请选择可看团别"))?;
    let vt = crate::store::ViewTeam::parse(raw);
    /* 账号不存在要明说（UPDATE 影响 0 行时静默返回 ok = 假成功）。
    写后回读校验沿用 admin_my_account_set 的口径：库里读不到就报错。 */
    if crate::store::my_account_get(&st, &name).await.is_none() {
        return Err(anyhow::anyhow!("账号「{name}」不存在").into());
    }
    if !crate::store::my_account_set_view_team(&st, &name, vt).await {
        return Err(anyhow::anyhow!("可看团别写入失败（库里读不到该账号）").into());
    }
    /* 回读复核：写的是不是真的落进去了（非法值应已被 parse 收敛）。 */
    let saved = crate::store::my_account_view_team(&st, &name).await;
    if saved != vt {
        return Err(anyhow::anyhow!(
            "可看团别写入后校验失败（期望 {}，库里是 {}）",
            vt.code(),
            saved.code()
        )
        .into());
    }
    my_tokens_revoke(&name);
    tracing::info!(account = %name, view_team = vt.code(), "管理端改可看团别（旧 token 已吊销）");
    Ok(Json(json!({ "ok": true, "viewTeam": vt.code() })))
}

/* 2026-10-16：绑定申请审核（`/api/admin/my-bind-requests*`，表 my_bind_requests）
随「抖音号审计」整页一起下线 —— 管理端本来就没有入口，展示端也已不再调用绑定申请
接口。三个 handler 已删除，表与迁移保留不删（历史申请记录仍可用 SQL 查）。 */

/* ------------------------------- 防遍历限流 ------------------------------- */

/// 展示端写接口限流器（每 IP 每分钟 5 次；当前唯一用途 = 个人页改密写口挡脚本刷）
///
/// 2026-10-17 用户口径「去除登录限速」：登录链路（verify / my_login）已不再
/// 使用这档限流 —— 名字保留（改档历史见 git），但调用方只剩改密写口。
pub(super) static VERIFY_LIMITER: std::sync::OnceLock<VerifyLimiter> = std::sync::OnceLock::new();
/// 写接口限流：每 IP 每分钟 12 次（原先只给注册类接口，2026-10-14 注册功能移除后
/// 由「忘记密码重置」`/api/public/my/password/reset` 复用；与登录撞库不是一回事，
/// 单独一档更宽松，仍然防脚本刷）
///
/// 名字随用途改：原先叫 `REGISTER_LIMITER`（注册时代命名），现在唯一的调用方就是
/// 忘记密码重置，故改名 `PASSWORD_RESET_LIMITER` —— 档位数值（12 次/分钟）不变。
pub(super) static PASSWORD_RESET_LIMITER: std::sync::OnceLock<VerifyLimiter> =
    std::sync::OnceLock::new();

pub(super) struct VerifyLimiter {
    hits: std::sync::Mutex<HashMap<String, Vec<Instant>>>,
    /// 每分钟允许的次数
    limit: usize,
}

impl VerifyLimiter {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            hits: std::sync::Mutex::new(HashMap::new()),
            limit,
        }
    }

    pub(super) fn allow(&self, ip: &str) -> bool {
        let mut hits = self.hits.lock().expect("verify 限流锁 poisoned");
        let now = Instant::now();
        // P2-15：整表 prune（对照 my_tokens 的 retain 写法），
        // key 不再永久驻留 —— 内存不随历史访客 IP 数增长。
        hits.retain(|_, list| {
            list.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
            !list.is_empty()
        });
        let list = hits.entry(ip.to_string()).or_default();
        list.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
        if list.len() >= self.limit {
            return false;
        }
        list.push(now);
        true
    }
}

/// 展示端写接口限流用的「客户端真实 IP」（当前唯一调用方 = 个人页改密写口；
/// 登录链路 2026-10-17「去除登录限速」后已不再走这里）。
///
/// 2026-10-17 修正（与 auth.rs login_source_key / ipinfo::request_ip 同口径）：
/// 生产容器里 ConnectInfo 是 Docker 网关（172.26.0.1，属私网），旧逻辑只信任
/// 回环直连的 XFF，导致限流桶全站共用一个网关 IP。新口径：
/// - 直连方是**回环或私网**才信任转发头；`X-Real-IP`（Caddy 注入的
///   CF-Connecting-IP）优先，其次 XFF 链尾（反代追加、客户端难伪造）；
/// - 公网直连一律用 ConnectInfo，伪造的转发头直接忽略。
pub(super) fn client_ip_for_limit(addr: std::net::SocketAddr, headers: &HeaderMap) -> String {
    if crate::ipinfo::is_internal(&addr.ip()) {
        if let Some(ip) = headers
            .get("x-real-ip")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<std::net::IpAddr>().ok())
        {
            return ip.to_string();
        }
        if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(last) = xff.rsplit(',').next() {
                let ip = last.trim();
                if !ip.is_empty() {
                    return ip.to_string();
                }
            }
        }
    }
    addr.ip().to_string()
}

/// 我的 token → (personId, 团别, 账号姓名, **过期时刻**)（内存；前端 sessionStorage 保存）。
/// 管理员观看账号 personId=0（不对应台账主播，姓名直接来自 my_accounts）。
///
/// 2026-10-17 审计 4-P2-8：第 4 个字段语义从「创建时刻」改为「过期时刻」——
/// put 写 `now+24h`，restore 回填写 `now+Redis 剩余 TTL`，两者都不再无条件续满 24h。
pub(super) type MyTokenMap = HashMap<String, (u64, String, String, Instant)>;

pub(super) static MY_TOKENS: std::sync::OnceLock<std::sync::Mutex<MyTokenMap>> =
    std::sync::OnceLock::new();

pub(super) fn my_tokens() -> &'static std::sync::Mutex<MyTokenMap> {
    MY_TOKENS.get_or_init(std::sync::Mutex::default)
}

/// 2026-10-14 审计 P2：my token 改 **CSPRNG**。原先用 RandomState（SipHash，
/// 每进程一个随机 key）——不是密码学生成器，输出可被观测/预测，而 token 一旦
/// 被猜中就是 24h 的完整登录态。改用 crate::random::random_code（getrandom →
/// OS CSPRNG，auth.rs 的 ticket 也走它），长度与 `my-<16hex>-<16hex>` 格式不变。
fn my_token_random() -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    format!(
        "my-{}-{}",
        crate::random::random_code(16, HEX),
        crate::random::random_code(16, HEX)
    )
}

/// 全局 AppState 钩子（2026-10-12 登录态持久化）：main 启动时注册，
/// 让同步的 token 函数也能把登录态落到 Redis —— **容器重启不再清空登录**。
pub(crate) static APP_STATE: std::sync::OnceLock<std::sync::Arc<crate::state::AppState>> =
    std::sync::OnceLock::new();

pub fn register_app_state(st: std::sync::Arc<crate::state::AppState>) {
    let _ = APP_STATE.set(st);
}

const MYTOK_TTL: u64 = 24 * 3600;
fn mytok_key(token: &str) -> String {
    format!("mytok:{}", token)
}

/// 2026-10-13 安全修复（my token 可吊销性）：反向索引 `mytokname:<name>` → SET of tokens。
/// 之前吊销只删「内存里能枚举到」的 token 的 Redis 键——进程重启后内存为空，
/// Redis 侧旧键删不掉，旧 token 会经 my_token_restore 复活（吊销被绕过）。
/// 有了这个按账号名聚合的 SET，吊销时 SMEMBERS 即可拿到该账号的全部 token，
/// 不再依赖内存枚举。
fn mytok_name_key(name: &str) -> String {
    format!("mytokname:{}", name)
}

/// token 内存 TTL（Duration 形态）。**内存映射的第 4 个字段已从「创建时刻」
/// 改成「过期时刻」**（2026-10-17 审计 4-P2-8）：restore 回填时必须用 Redis 的
/// 剩余 TTL 续期，不能重新起算满 24h，否则接近到期的登录态会被重启无限续命。
const MYTOK_TTL_DUR: Duration = Duration::from_secs(MYTOK_TTL);

/// 进程内「吊销墓碑」：token → 吊销时刻（2026-10-17 审计 4-P2-8）。
///
/// 为什么需要它：`my_tokens_revoke` 对内存是同步删除，但对 Redis 是
/// `mytok_redis_sync` 子线程里的异步删除（可能排队 / 超时 / Redis 降级）。
/// 在这个窗口里，`my_token_restore` 若先从 Redis 读回旧 token 并回填内存，
/// 刚吊销的登录态就被「复活」了。墓碑同步写入、take/restore 先查，
/// 把窗口关死；24h 后自动清理（token 最大寿命），不会无限增长。
static MYTOK_REVOKED: std::sync::OnceLock<std::sync::Mutex<HashMap<String, Instant>>> =
    std::sync::OnceLock::new();

fn mytok_revoked() -> &'static std::sync::Mutex<HashMap<String, Instant>> {
    MYTOK_REVOKED.get_or_init(std::sync::Mutex::default)
}

/// 纯逻辑：清掉超过 token 最大寿命的墓碑（读取路径顺手调用）。
fn revoked_prune(map: &mut HashMap<String, Instant>, now: Instant) {
    map.retain(|_, at| now.duration_since(*at) < MYTOK_TTL_DUR);
}

/// 纯逻辑：登记一批已吊销 token；已存在的不覆盖原吊销时刻。
fn revoked_insert(map: &mut HashMap<String, Instant>, tokens: &[String], now: Instant) {
    revoked_prune(map, now);
    for t in tokens {
        map.entry(t.clone()).or_insert(now);
    }
}

/// 纯逻辑：token 是否在墓碑里（命中即视为已吊销；顺带 prune）。
fn revoked_contains(map: &mut HashMap<String, Instant>, token: &str, now: Instant) -> bool {
    revoked_prune(map, now);
    map.contains_key(token)
}

fn mytok_mark_revoked(tokens: &[String]) {
    if tokens.is_empty() {
        return;
    }
    let mut m = mytok_revoked().lock().expect("my token 墓碑锁 poisoned");
    revoked_insert(&mut m, tokens, Instant::now());
}

fn mytok_is_revoked(token: &str) -> bool {
    let mut m = mytok_revoked().lock().expect("my token 墓碑锁 poisoned");
    revoked_contains(&mut m, token, Instant::now())
}

/// 日志打码：token 只留前 8 位，任何日志都不打印完整登录态。
fn mytok_mask(token: &str) -> String {
    let mut s: String = token.chars().take(8).collect();
    if token.chars().count() > 8 {
        s.push('…');
    }
    s
}

/// 回填内存时使用的过期时刻（纯函数，便于单测）：现在 + Redis 剩余 TTL，
/// 并以 24h 为上限兜底（Redis 侧理论不会超，防脏数据放大寿命）。
fn restore_expiry(now: Instant, ttl_secs: u64) -> Instant {
    now + Duration::from_secs(ttl_secs.min(MYTOK_TTL))
}

/// 异步工厂：让调用点用 async 块传进来（block_on 里再驱动）
trait FutureFactory: Send + 'static {
    fn future(
        self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>;
}

impl<F, Fut> FutureFactory for F
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    fn future(
        self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>> {
        Box::pin(self())
    }
}

/// put/take/revoke 之后的 Redis 同步（拿不到 Redis 就静默跳过，退化为纯内存）。
///
/// 2026-10-17 审计 4-P2-8：超时/失败不再完全静默 —— 以前 `let _ = timeout(...)`
/// 把「Redis 没删掉旧 token」这类安全相关降级吞掉了；现在打 WARN（`target` 里
/// 只放打码后的 token / 账号，绝不打印完整凭证）。
fn mytok_redis_sync<F: FutureFactory>(op: &'static str, target: String, f: F) {
    if APP_STATE.get().is_none() {
        return;
    }
    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            tracing::warn!(
                op = %op,
                target = %target,
                "my token Redis 同步线程起不来：登录态只在本进程内存生效"
            );
            return;
        };
        let res = rt.block_on(async move {
            tokio::time::timeout(std::time::Duration::from_secs(3), f.future()).await
        });
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(
                op = %op,
                target = %target,
                error = %e,
                "my token Redis 同步失败：本进程照常生效，但重启后登录态可能丢失/旧 token 可能复活（已由墓碑兜底）"
            ),
            Err(_) => tracing::warn!(
                op = %op,
                target = %target,
                "my token Redis 同步超时（3s），本次写入未确认"
            ),
        }
    });
}

async fn mytok_redis_set(
    st: &crate::state::AppState,
    token: &str,
    pid: u64,
    team: &str,
    name: &str,
) -> anyhow::Result<()> {
    let v = serde_json::json!({ "pid": pid, "team": team, "name": name });
    crate::store::cache::set(st, &mytok_key(token), &v.to_string(), MYTOK_TTL).await;
    // 2026-10-13 安全修复：同步登记反向索引（SADD + 与 token 键同 TTL 的 EXPIRE），
    // 让「按账号名吊销」能在不依赖内存枚举的情况下覆盖 Redis 侧全部 token。
    let members: Vec<&str> = vec![token];
    if crate::store::cache::sadd(st, &mytok_name_key(name), &members)
        .await
        .is_some()
    {
        crate::store::cache::expire(st, &mytok_name_key(name), MYTOK_TTL).await;
    }
    Ok(())
}

// 2026-10-13 安全修复：原 mytok_redis_del 已并入 mytok_redis_revoke（吊销要连
// 反向索引一起清，单删 token 键不足以让旧登录失效），函数体保留在 revoke 里。

/// 2026-10-13 安全修复：吊销路径专用——同时删 token 键本体 + 从反向索引 SET 里摘除。
async fn mytok_redis_revoke(
    st: &crate::state::AppState,
    token: &str,
    name: &str,
) -> anyhow::Result<()> {
    crate::store::cache::del(st, &mytok_key(token)).await;
    // SREM 失败只是降级（SET 有 24h EXPIRE 兜底），不冒泡
    let _ = crate::store::cache::srem(st, &mytok_name_key(name), &[token]).await;
    Ok(())
}

pub(super) fn my_token_put(person_id: u64, team: String, name: String) -> String {
    // token 不再编入 pid/时间戳（老格式可被相邻值猜中），纯随机串，映射关系只存在内存里
    // 2026-10-14 审计 P2：随机源从 RandomState 换成 CSPRNG（random::random_code）
    let token = my_token_random();
    let now = Instant::now();
    let mut m = my_tokens().lock().expect("my token 锁 poisoned");
    // 第 4 个字段是**过期时刻**（2026-10-17 审计 4-P2-8，见 MYTOK_TTL_DUR 注释）
    m.retain(|_, (_, _, _, exp)| *exp > now);
    m.insert(
        token.clone(),
        (person_id, team.clone(), name.clone(), now + MYTOK_TTL_DUR),
    );
    drop(m);
    let t = token.clone();
    let tm = team.clone();
    let log_target = mytok_mask(&t);
    mytok_redis_sync("put", log_target, move || async move {
        if let Some(st) = APP_STATE.get().cloned() {
            mytok_redis_set(&st, &t, person_id, &tm, &name).await?;
        }
        anyhow::Ok(())
    });
    token
}

pub(super) fn my_token_take(token: &str) -> Option<(u64, String, String)> {
    /* 2026-10-17 审计 4-P2-8：先查吊销墓碑。墓碑是同步写入的，而 Redis DEL 在
    mytok_redis_sync 子线程里异步发生——没有这一步，吊销后紧接着的 restore 可能先从
    Redis 读回旧值再回填内存，把刚吊销的登录态「复活」。命中墓碑时顺手清内存残留。 */
    if mytok_is_revoked(token) {
        let mut m = my_tokens().lock().expect("my token 锁 poisoned");
        m.remove(token);
        return None;
    }
    let now = Instant::now();
    let mut m = my_tokens().lock().expect("my token 锁 poisoned");
    m.retain(|_, (_, _, _, exp)| *exp > now);
    m.get(token)
        .map(|(pid, team, name, _)| (*pid, team.clone(), name.clone()))
}

/// 内存 miss 时从 Redis 回填（2026-10-12 登录态持久化）：
/// who/token 校验路径（async）在 take 失败后调用，命中即回填内存。
/// 只回填不消费（take 语义是只读），让 token 继续在 TTL 内可用。
pub(crate) async fn my_token_restore(
    st: &crate::state::AppState,
    token: &str,
) -> Option<(u64, String, String)> {
    /* 2026-10-17 审计 4-P2-8：墓碑优先。吊销时墓碑是**同步**写入的，Redis DEL 在
    子线程里异步执行；若这里先读 Redis，可能把刚吊销的 token 回填进内存（复活）。
    命中墓碑直接拒绝，并清掉可能被并发回填的内存条目。 */
    if mytok_is_revoked(token) {
        let mut m = my_tokens().lock().expect("my token 锁 poisoned");
        m.remove(token);
        return None;
    }
    if let Some(v) = my_token_take(token) {
        return Some(v); // 内存里有，直接用
    }
    // 2026-10-13 安全修复：回填前先校验该 token 仍在反向索引 mytokname:<name> 里。
    // 旧 token 若已被吊销（DEL + SREM），SMEMBERS 里不会再有它——即使 Redis 侧
    // mytok:<token> 键因故残留，也不允许复活进内存。不在索引里（含索引键丢失/
    // 整体过期）一律按「已吊销」处理：返回 None 且不回填内存（失败关闭，宁拒不放）。
    let raw = crate::store::cache::get(st, &mytok_key(token)).await?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let owner = v.get("name").and_then(|n| n.as_str()).unwrap_or_default();
    /* 2026-10-17 审计 4-P2-8：回填需要的三元组从 Redis JSON 里取（原实现用未定义的
    pid/team/name 编译不过——2026-10-17 修复时补回）；缺失按默认值处理，name 仍要能
    对上反向索引，否则上面已经失败关闭。 */
    let pid = v.get("pid").and_then(|x| x.as_u64()).unwrap_or(0);
    let team = v
        .get("team")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    let name = owner.to_string();
    let members = crate::store::cache::smembers(st, &mytok_name_key(owner)).await;
    if !members.iter().any(|t| t == token) {
        return None; // 不在反向索引里 = 已吊销：不回填内存，视为未登录
    }
    /* 2026-10-17 审计 4-P2-8：内存 TTL 用 Redis 的**剩余** TTL，不再重置满 24h。
    原实现回填时 `Instant::now()` 重新起算，导致「签发后 23h 重启」会把只剩 1h 的
    登录态续成 24h（跨重启最多 ~48h 会话）。拿不到 TTL（未配置 Redis / 命令失败 /
    键没有过期时间）→ 无法确认寿命，保守按无效处理：宁可让用户重新登录，
    也不放大一个寿命未知的登录态。 */
    let Some(ttl_secs) = crate::store::cache::ttl(st, &mytok_key(token)).await else {
        tracing::warn!(
            token = %mytok_mask(token),
            "my token 回填失败：拿不到 Redis 剩余 TTL，按失效处理（失败关闭）"
        );
        return None;
    };
    if ttl_secs == 0 {
        return None;
    }
    let now = Instant::now();
    let expires_at = restore_expiry(now, ttl_secs);
    let mut m = my_tokens().lock().expect("my token 锁 poisoned");
    m.retain(|_, (_, _, _, exp)| *exp > now);
    m.insert(
        token.to_string(),
        (pid, team.clone(), name.clone(), expires_at),
    );
    Some((pid, team, name))
}

/// 吊销某账号的全部 my token（2026-10-12 安全审计：token 是 24h 内存态、原本无撤销手段）。
/// 调用点：改密 / 找回（重置）密码 / 绑定审核通过 / 管理员改绑 / 删除账号——
/// 这些动作之后旧登录态必须立刻失效，不能等 24h TTL 自然过期。
/// （token 挂的是**账号名**：绑定会把 name 从手机号占位改写成主播姓名，所以绑定类操作
/// 新旧两个名字都要吊销。）
pub(crate) fn my_tokens_revoke(name: &str) {
    let revoked: Vec<String> = {
        let mut m = my_tokens().lock().expect("my token 锁 poisoned");
        let removed: Vec<String> = m
            .iter()
            .filter(|(_, (_, _, n, _))| *n == name)
            .map(|(k, _)| k.clone())
            .collect();
        m.retain(|_, (_, _, n, _)| *n != name);
        removed
    };
    /* 2026-10-17 审计 4-P2-8：内存可见的 token **同步**写进吊销墓碑。
    Redis 侧的 DEL 要等子线程（排队/超时/Redis 降级都可能），这期间 take/restore
    只要命中墓碑就直接拒绝，不会出现「刚吊销又被回填复活」的窗口。 */
    mytok_mark_revoked(&revoked);
    // 2026-10-13 安全修复：内存可见的 token 照旧逐个吊销；
    // 同时必须处理「进程重启后内存为空」的情况——改密前签发的旧 token 还躺在
    // Redis 里，等 my_token_restore 回填就复活了。所以无论内存里有没有，
    // 都从反向索引 mytokname:<name> 取该账号在 Redis 侧的全部 token 逐个 DEL + SREM，
    // 完全不依赖内存枚举。
    // 2026-10-13 安全修复：name 是调用方传进来的 &str，mytok_redis_sync 的异步工厂
    // 要求 'static，先克隆成 owned 再 move 进闭包，避免生命周期逃逸（E0521）。
    let name_owned = name.to_string();
    let log_target = format!("name={}", mytok_mask(name));
    mytok_redis_sync("revoke", log_target, move || async move {
        if let Some(st) = APP_STATE.get().cloned() {
            let name = name_owned;
            // 1) Redis 反向索引里的全部 token（含本进程没见过的）
            let mut all: Vec<String> =
                crate::store::cache::smembers(&st, &mytok_name_key(&name)).await;
            // 2) 内存枚举到的也合并进去（极端情况：反向索引写失败但 token 键写成功了）
            for t in revoked {
                if !all.contains(&t) {
                    all.push(t);
                }
            }
            /* 注意：这里**不**把 Redis 侧才发现的旧 token 补进墓碑 —— 吊销是异步线程，
            同一账号紧接着重登/改密后换发的新 token 可能已经被写进反向索引、再被本线程
            SMEMBERS 读到；补墓碑会误杀刚签发的新 token（用户改完密码立刻 401）。
            内存可见的 token 已在 `my_tokens_revoke` 里同步落墓碑；Redis-only 旧 token
            靠下面的 DEL + SREM 从反向索引摘除，restore 的索引校验兜底。 */
            for token in all {
                mytok_redis_revoke(&st, &token, &name).await?;
            }
        }
        anyhow::Ok(())
    });
}

/// 只读一眼 token 对应的账号（不消费）：公开 API 中间件用它给访问日志带账号。
/// 未登录的访客返回 None，日志就只记 IP。
pub(crate) fn my_token_peek(token: &str) -> Option<(u64, String, String)> {
    my_token_take(token)
}

/// 从 X-My-Token 头取登录 token（2026-10-12 起**唯一**入口）。
///
/// 不再接受 `?token=` / `?access_token=` URL query（审计安全修复）：
/// URL 会进网关/服务器访问日志、浏览器历史与 Referer 头，token 走 query 等于把
/// 24h 有效的登录态写进各种日志。POST JSON body 里的 `token` 字段保留（不进 URL/日志）。
pub(super) fn header_my_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-my-token")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// 展示端登录门（2026-10-10）：/st/{team} 的数据接口一律要求「我的」登录态，
/// 未登录只给一张登录卡。token 只走 X-My-Token 头（2026-10-12 起，不再认 URL query）。
/// 校验通过返回 person_id；token 与路径团别不符也拒绝。
///
/// 2026-10-10 加开关：后台关掉「开启登录」（login_required=false）时全站免登录开放，
/// 返回 person_id=0（一个不存在的档案 id，调用方只拿它判「管理员/游客」，不下发个人数据）。
/// 带 token 的照常解析，所以关登录时之前登过的人仍能直接用「我的」。
pub(super) async fn gate_team(
    st: &crate::state::AppState,
    login_required: bool,
    team: &str,
    headers: &HeaderMap,
) -> Result<u64, ApiError> {
    let token = header_my_token(headers);
    // 有 token 就先验（开关关着时也认，访客拿旧 token 仍能看「我的」）
    if let Some(t) = token.as_deref().filter(|s| !s.is_empty()) {
        // 登录态持久化（2026-10-12）：内存 miss 先从 Redis 回填（服务重启后
        // 24h 内的登录态自动恢复），再走原判定。
        if my_token_take(t).is_none() {
            let _ = my_token_restore(st, t).await;
        }
        if let Some((pid, my_team, _)) = my_token_take(t) {
            // 团别为空 = 免团别登录口（/api/public/my/login）发的「任意团」token：
            // 管理员观看账号 / 没绑主播的账号，两个团都放行
            if my_team == team || my_team.is_empty() || team == "all" {
                return Ok(pid);
            }
            // token 团别不符：开着登录门时报错；关了就按游客放行（别把路人拦在门外）
            return if login_required {
                Err(ApiError::auth("该账号不属于本团"))
            } else {
                Ok(0)
            };
        }
    }
    // 没带 token / token 失效：关登录 → 游客放行；开登录 → 拦下
    if !login_required {
        return Ok(0);
    }
    Err(ApiError::auth("请先登录"))
}

/// 从 token 里取账号所属团别；游客 / 任意团账号返回空串（统一入口按全量展示）。
pub(super) fn token_team(headers: &HeaderMap) -> String {
    header_my_token(headers)
        .and_then(|t| my_token_take(&t))
        .map(|(_, team, _)| team)
        .filter(|team| team == "male" || team == "female")
        .unwrap_or_default()
}

/// 统一入口 `/api/public/teams/all` 的数据口径：
/// - 管理员观看 / 未绑定账号 / 开放游客：看全量；
/// - 绑定男团主播：只看男团；
/// - 绑定女团主播：只看女团。
pub(super) fn data_team<'a>(path_team: &'a str, token_team: &'a str) -> &'a str {
    if path_team == "all" {
        token_team
    } else {
        path_team
    }
}

#[cfg(test)]
mod token_lifecycle_tests {
    //! 2026-10-17 审计 4-P2-8：my token 吊销墓碑与回填 TTL 的**纯逻辑**单测。
    //! 不依赖 Redis / 全局静态（把 map 显式传进来），并行跑也不会互相干扰。

    use super::*;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    /// 墓碑：登记后拒绝；不同 token 不误伤；超 24h 自动清理（不无限增长）。
    #[test]
    fn revoked_tombstone_rejects_and_prunes() {
        let now = Instant::now();
        let mut map: HashMap<String, Instant> = HashMap::new();
        let token = "my-00000000-00000000".to_string();

        // 未吊销：放行
        assert!(!revoked_contains(&mut map, &token, now));

        // 吊销后：同 token 命中（take / restore 都会据此拒绝）
        revoked_insert(&mut map, std::slice::from_ref(&token), now);
        assert!(revoked_contains(&mut map, &token, now));

        // 别的 token 不受牵连
        assert!(!revoked_contains(&mut map, "my-ffffffff-ffffffff", now));

        // 24h 后墓碑失效并被 prune 掉（token 最大寿命已过，无需再挡）
        let later = now + MYTOK_TTL_DUR + Duration::from_secs(1);
        assert!(!revoked_contains(&mut map, &token, later));
        assert!(map.is_empty(), "过期墓碑应被清理，避免无限增长");
    }

    /// 回填 TTL：内存过期时刻 = now + Redis 剩余 TTL，而不是重置满 24h；
    /// 异常大的 TTL 以 24h 为上限兜底。
    #[test]
    fn restore_expiry_uses_remaining_ttl_not_full_lifetime() {
        let now = Instant::now();
        // 只剩 60 秒：回填后也只剩 60 秒（不续命）
        assert_eq!(restore_expiry(now, 60), now + Duration::from_secs(60));
        // 脏数据（超过 24h）：封顶到 24h
        assert_eq!(
            restore_expiry(now, MYTOK_TTL + 10 * 24 * 3600),
            now + MYTOK_TTL_DUR
        );
    }

    /// 日志打码：只暴露前 8 位，完整 token 不进日志。
    #[test]
    fn token_log_mask_keeps_only_prefix() {
        let token = "my-0123456789abcdef-0123456789abcdef";
        let masked = mytok_mask(token);
        assert!(masked.starts_with("my-01234"));
        assert_eq!(masked.chars().count(), 9, "前 8 位 + 省略号");
        assert!(!masked.contains("abcdef"), "后半段不得出现在日志里");
    }
}
