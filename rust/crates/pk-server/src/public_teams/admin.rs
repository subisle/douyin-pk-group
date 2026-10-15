//! public_teams::admin —— 自 public_teams.rs 按业务域拆分（对外语义不变）。

use super::*;

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
        None => (
            crate::store::my_account::initial_password(phone),
            true,
        ),
    };
    crate::store::my_account_set(&st, name, phone, &pass)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
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
            "账号「{}」写入后校验失败（库里读不到），请检查写库配置后重试", name
        ).into());
    }
    my_tokens_revoke(name);
    tracing::info!(account = %name, generated, "管理端设置展示端账号（旧 token 已吊销，已回读校验）");
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
        .map_err(|e| anyhow::anyhow!(e))?;
    // 改绑后吊销旧登录态：账号名会被改写成主播姓名，新旧两个名字都吊销
    // （拿着旧 token 的人必须重新登录才能拿到新身份，审计 2026-10-12）
    my_tokens_revoke(&name);
    my_tokens_revoke(&r.name);
    tracing::info!(account = %name, anchor = %r.name, team = %r.team, "管理员改绑展示端账号（旧 token 已吊销）");
    Ok(Json(json!({
        "ok": true, "name": r.name, "team": r.team, "personId": r.person_id,
    })))
}

/* --------------------- 绑定申请审核（管理端，017 表，2026-10-12 审计） --------------------- */

/// GET /api/admin/my-bind-requests?status=pending：绑定申请列表
/// （status 空 = 全部，pending 排最前；不含任何密码/凭证字段）
pub async fn admin_my_bind_requests(
    State(st): State<ArcState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let status = q.get("status").cloned().unwrap_or_default();
    Ok(Json(
        json!({ "items": crate::store::my_bind_request_list(&st, &status, 200).await }),
    ))
}

/// POST /api/admin/my-bind-requests/{id}/approve：审核通过。
/// 事务式口径（与 anchor_claim_decide 同款）：先原子翻转 pending→approved 占住裁决权，
/// 再执行绑定（复用 my_account_bind 的核心绑定逻辑）；绑定失败则回滚申请为 pending，
/// 不会卡在「已通过却没绑上」。通过后吊销该账号全部 my token（强制重新登录拿新身份）。
pub async fn admin_my_bind_request_approve(
    State(st): State<ArcState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> ApiResult {
    /* person_name 是申请单上的姓名快照，绑定后以主播台账现名（r.name）为准，
    这里只做解构丢弃 —— 事件与响应都发台账现名，避免快照与现名不一致。 */
    let Some((account, person_id, _person_name)) =
        crate::store::my_bind_request_decide(&st, id, true, "", "admin").await
    else {
        return Err(anyhow::anyhow!("这笔申请不存在或已经处理过了").into());
    };
    match crate::store::my_account_bind(&st, &account, person_id).await {
        Ok(r) => {
            // 绑定把账号名从手机号占位改写成主播姓名：新旧名都吊销
            my_tokens_revoke(&account);
            my_tokens_revoke(&r.name);
            tracing::info!(id, account = %account, anchor = %r.name, team = %r.team, "绑定申请审核通过（旧 token 已吊销）");
            crate::events::emit(
                crate::events::SCOPE_ROSTER,
                format!("绑定审核通过：账号「{account}」→ 主播「{}」", r.name),
            );
            Ok(Json(json!({
                "ok": true, "status": "approved",
                "account": account, "name": r.name, "team": r.team,
            })))
        }
        Err(e) => {
            // 绑定失败（主播已绑别人 / 账号状态变了…）：回滚裁决，让管理员能改判或重试
            crate::store::my_bind_request_revert(&st, id).await;
            Err(anyhow::anyhow!(e).into())
        }
    }
}

/// POST /api/admin/my-bind-requests/{id}/reject {reason?}：驳回（reason 会回显给用户）
pub async fn admin_my_bind_request_reject(
    State(st): State<ArcState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult {
    let reason = body
        .get("reason")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default()
        .chars()
        .take(255)
        .collect::<String>();
    let Some((account, _pid, person_name)) =
        crate::store::my_bind_request_decide(&st, id, false, &reason, "admin").await
    else {
        return Err(anyhow::anyhow!("这笔申请不存在或已经处理过了").into());
    };
    tracing::info!(id, account = %account, anchor = %person_name, reason = %reason, "绑定申请已驳回");
    crate::events::emit(
        crate::events::SCOPE_ROSTER,
        format!("绑定驳回：账号「{account}」申请的主播「{person_name}」（{reason}）"),
    );
    Ok(Json(json!({ "ok": true, "status": "rejected" })))
}

/* ------------------------------- 防遍历限流 ------------------------------- */

/// 验证接口限流：每 IP 每分钟 5 次（姓名/密码撞库防护）
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

/// P1-3：限流用的「客户端真实 IP」。
/// 生产经反代（Caddy → 127.0.0.1:8787），直连看到的 ConnectInfo 全是 127.0.0.1，
/// 全站会共用一个限流桶；但 X-Forwarded-For 可以被任何人伪造 —— 所以只信任
/// **来自 127.0.0.1 的连接**带来的 XFF（取最后一跳，即反代追加的直连来源），
/// 非本机连接一律用 ConnectInfo（伪造的 XFF 直接忽略）。
///
/// 2026-10-12 安全审计 H1 修复：第一跳是客户端可任意伪造的值（换一次头换一个
/// 限流桶，实测 8 连发全过）；XFF 链尾才是反代追加的、客户端无法篡改的真实来源。
pub(super) fn client_ip_for_limit(addr: std::net::SocketAddr, headers: &HeaderMap) -> String {
    if addr.ip().is_loopback() {
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

/// 我的 token → (personId, 团别, 账号姓名)（内存，24h 失效；前端 localStorage 保存）。
/// 管理员观看账号 personId=0（不对应台账主播，姓名直接来自 my_accounts）。
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

/// put/take/revoke 之后的 Redis 同步（拿不到 Redis 就静默跳过，退化为纯内存）。
/// 异步工厂：让调用点用 async 块传进来（block_on 里再驱动）
trait FutureFactory: Send + 'static {
    fn future(self) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>;
}

impl<F, Fut> FutureFactory for F
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    fn future(self) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>> {
        Box::pin(self())
    }
}

fn mytok_redis_sync<F: FutureFactory>(f: F) {
    if APP_STATE.get().is_none() {
        return;
    }
    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        rt.block_on(async move {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3), f.future()).await;
        });
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
    if crate::store::cache::sadd(st, &mytok_name_key(name), &members).await.is_some() {
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
    let mut m = my_tokens().lock().expect("my token 锁 poisoned");
    m.retain(|_, (_, _, _, at)| at.elapsed() < Duration::from_secs(24 * 3600));
    m.insert(token.clone(), (person_id, team.clone(), name.clone(), Instant::now()));
    drop(m);
    let t = token.clone();
    let tm = team.clone();
    mytok_redis_sync(move || async move {
        if let Some(st) = APP_STATE.get().cloned() {
            mytok_redis_set(&st, &t, person_id, &tm, &name).await?;
        }
        anyhow::Ok(())
    });
    token
}

pub(super) fn my_token_take(token: &str) -> Option<(u64, String, String)> {
    let mut m = my_tokens().lock().expect("my token 锁 poisoned");
    m.retain(|_, (_, _, _, at)| at.elapsed() < Duration::from_secs(24 * 3600));
    m.get(token)
        .map(|(pid, team, name, _)| (*pid, team.clone(), name.clone()))
}

/// 内存 miss 时从 Redis 回填（2026-10-12 登录态持久化）：
/// who/token 校验路径（async）在 take 失败后调用，命中即回填内存。
/// 只回填不消费（take 语义是只读），让 token 继续在 TTL 内可用。
pub(crate) async fn my_token_restore(st: &crate::state::AppState, token: &str) -> Option<(u64, String, String)> {
    if my_token_take(token).is_some() {
        return my_token_take(token); // 内存里有，直接用
    }
    // 2026-10-13 安全修复：回填前先校验该 token 仍在反向索引 mytokname:<name> 里。
    // 旧 token 若已被吊销（DEL + SREM），SMEMBERS 里不会再有它——即使 Redis 侧
    // mytok:<token> 键因故残留，也不允许复活进内存。不在索引里（含索引键丢失/
    // 整体过期）一律按「已吊销」处理：返回 None 且不回填内存（失败关闭，宁拒不放）。
    let raw = crate::store::cache::get(st, &mytok_key(token)).await?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let owner = v.get("name").and_then(|n| n.as_str()).unwrap_or_default();
    let members = crate::store::cache::smembers(st, &mytok_name_key(owner)).await;
    if !members.iter().any(|t| t == token) {
        return None; // 不在反向索引里 = 已吊销：不回填内存，视为未登录
    }
    let pid = v.get("pid")?.as_u64()?;
    let team = v.get("team")?.as_str()?.to_string();
    let name = v.get("name")?.as_str()?.to_string();
    // 回填内存（Instant=now；TTL 由 Redis 端 24h 控制，内存侧随写随清）
    let mut m = my_tokens().lock().expect("my token 锁 poisoned");
    m.insert(token.to_string(), (pid, team.clone(), name.clone(), Instant::now()));
    Some((pid, team, name))
}

/// 吊销某账号的全部 my token（2026-10-12 安全审计：token 是 24h 内存态、原本无撤销手段）。
/// 调用点：改密 / 找回（重置）密码 / 绑定审核通过 / 管理员改绑 / 删除账号——
/// 这些动作之后旧登录态必须立刻失效，不能等 24h TTL 自然过期。
/// （token 挂的是**账号名**：绑定会把 name 从手机号占位改写成主播姓名，所以绑定类操作
/// 新旧两个名字都要吊销。）
pub(crate) fn my_tokens_revoke(name: &str) {
    let mut m = my_tokens().lock().expect("my token 锁 poisoned");
    let revoked: Vec<String> = {
        let removed: Vec<String> = m
            .iter()
            .filter(|(_, (_, _, n, _))| *n == name)
            .map(|(k, _)| k.clone())
            .collect();
        m.retain(|_, (_, _, n, _)| *n != name);
        removed
    };
    // 2026-10-13 安全修复：内存可见的 token 照旧逐个吊销；
    // 同时必须处理「进程重启后内存为空」的情况——改密前签发的旧 token 还躺在
    // Redis 里，等 my_token_restore 回填就复活了。所以无论内存里有没有，
    // 都从反向索引 mytokname:<name> 取该账号在 Redis 侧的全部 token 逐个 DEL + SREM，
    // 完全不依赖内存枚举。
    // 2026-10-13 安全修复：name 是调用方传进来的 &str，mytok_redis_sync 的异步工厂
    // 要求 'static，先克隆成 owned 再 move 进闭包，避免生命周期逃逸（E0521）。
    let name_owned = name.to_string();
    mytok_redis_sync(move || async move {
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
