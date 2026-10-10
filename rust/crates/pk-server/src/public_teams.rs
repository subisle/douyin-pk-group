//! 公开端团别站点后端（阶段 D，docs/08）：
//! - /st/m、/st/f 两个独立链接，链接即团别
//! - 后端级显隐开关：关闭后不下发数据（不是前端藏）
//! - 「我的」页面验证：姓名+密码（纯数字；账号由管理员在后台手动添加，没有注册）
//! - 公开 API 一律 DTO 白名单，绝不下发密码、抖音号、全量候选名单

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::dialect::{self, is_live_wave, NOT_LIVE_WAVE_THRESHOLD};
use crate::mysql;
use crate::routes::ApiError;
use crate::state::AppState;
use axum::response::{IntoResponse, Response};

type ApiResult = Result<Json<Value>, ApiError>;
type ArcState = Arc<AppState>;

/// 公开端一律读「当前生效数据源」，不能写死某个源 id。
/// 早先版本在这里硬编码 `LOCAL`，结果把源切到 A（sqlpub）或直接只配 A 时，
/// 展示页全部接口报「未知数据源 LOCAL」——页面看着是空的，库里其实有数据。
fn sid(st: &AppState) -> String {
    st.active_id()
}

/* ------------------------------- 显隐开关 ------------------------------- */

/// 公开站点配置（2026-10-10 起存 MySQL config 表，cfg_key = public_settings；
/// 每个公开接口都要读它，进程内缓存 30s，save 时立即刷新缓存）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PublicSettings {
    pub male_enabled: bool,
    pub female_enabled: bool,
    pub daily: bool,
    pub monthly: bool,
    pub yearly: bool,
    pub duration: bool,
    pub trend: bool,
    /// 每日报告的音浪数值（2026-10-09）：关掉后榜单不下发任何数字，名次/名次变动照常
    pub wave: bool,
    /// 每日报告页显隐（2026-10-10）：关掉后接口不下发榜单、导航整体消失
    #[serde(default = "default_true")]
    pub data_enabled: bool,
    /// 分组页显隐（2026-10-10）：关掉后导航消失
    #[serde(default = "default_true")]
    pub group_enabled: bool,
    /// 族谱页显隐（2026-10-10）：关掉后接口不下发、导航消失（男团专属页）
    #[serde(default = "default_true")]
    pub lineage_enabled: bool,
    /// 分组子页（按赛事类型）显隐（2026-10-10）：key = 赛事类型，缺省 true。
    /// 关掉的类型子导航消失，该类型子页按「暂未公布」处理。
    #[serde(default)]
    pub group_types: std::collections::HashMap<String, bool>,
    /// 是否开启登录门（2026-10-10）：关 = 全面开放，打开就能看（但没有「我的」页）；
    /// 开 = 全站要登录（现状）
    #[serde(default = "default_true")]
    pub login_required: bool,
    /// 展示端手机号注册开关（2026-10-10 用户口径「注册先不开」）：**默认关**。
    /// 关 = 登录门不显示「注册」页签，注册/授权接口一律拒绝（登录、绑定照常）。
    /// 想开时在管理端「公开站点设置 → 开放手机号注册」打开即可。
    #[serde(default)]
    pub my_register: bool,
}

fn default_true() -> bool {
    true
}

/// 分组页/子页显隐的过滤口径（public::plans 用）：页关 → 全部不发；
/// 子页关 → 该类型不发；没配置过的类型 = 开（老方案兼容）。
pub fn plan_visible_by_settings(s: &PublicSettings, event_type: &str) -> bool {
    s.group_enabled && s.group_type_enabled(event_type)
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    #[test]
    fn plan_visibility_follows_page_and_type_switches() {
        let mut s = PublicSettings::default();
        // 全开：什么类型都可见
        assert!(plan_visible_by_settings(&s, "初级争霸赛小组赛"));
        assert!(plan_visible_by_settings(&s, ""), "未分类的老方案也可见");

        // 分组页整体关掉：任何类型都不可见
        s.group_enabled = false;
        assert!(!plan_visible_by_settings(&s, "初级争霸赛小组赛"));
        s.group_enabled = true;

        // 单独关一个子页：只有它不可见，其它照常
        s.group_types.insert("初级争霸赛小组赛".into(), false);
        assert!(!plan_visible_by_settings(&s, "初级争霸赛小组赛"));
        assert!(plan_visible_by_settings(&s, "月终争霸赛"));
        assert!(plan_visible_by_settings(&s, ""), "没配置过的类型按开处理");

        // 反向写法（只存开的那个）也不影响其它类型
        s.group_types.clear();
        s.group_types.insert("月终争霸赛".into(), true);
        assert!(plan_visible_by_settings(&s, "初级争霸赛小组赛"));
    }
}

impl Default for PublicSettings {
    fn default() -> Self {
        Self {
            male_enabled: true,
            female_enabled: true,
            daily: true,
            monthly: true,
            yearly: true,
            duration: true,
            trend: true,
            wave: true,
            data_enabled: true,
            group_enabled: true,
            lineage_enabled: true,
            group_types: Default::default(),
            login_required: true,
            // 注册默认关（用户口径 2026-10-10「注册先不开」）
            my_register: false,
        }
    }
}

static PS_CACHE: std::sync::OnceLock<
    std::sync::Mutex<Option<(PublicSettings, std::time::Instant)>>,
> = std::sync::OnceLock::new();

fn ps_cache() -> &'static std::sync::Mutex<Option<(PublicSettings, std::time::Instant)>> {
    PS_CACHE.get_or_init(std::sync::Mutex::default)
}

impl PublicSettings {
    pub async fn load(st: &AppState) -> Self {
        // 命中 30s 内的缓存直接用；未命中查库回填
        if let Ok(guard) = ps_cache().lock() {
            if let Some((s, at)) = guard.as_ref() {
                if at.elapsed() < std::time::Duration::from_secs(30) {
                    return s.clone();
                }
            }
        }
        let s: PublicSettings = crate::store::config_get(st, "public_settings")
            .await
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if let Ok(mut guard) = ps_cache().lock() {
            *guard = Some((s.clone(), std::time::Instant::now()));
        }
        s
    }

    pub async fn save(&self, st: &AppState) {
        if let Ok(s) = serde_json::to_string(self) {
            crate::store::config_set(st, "public_settings", &s).await;
        }
        if let Ok(mut guard) = ps_cache().lock() {
            *guard = Some((self.clone(), std::time::Instant::now()));
        }
    }

    fn team_enabled(&self, team: &str) -> bool {
        match team {
            "male" => self.male_enabled,
            "female" => self.female_enabled,
            _ => false,
        }
    }

    /// 某个分组子页（赛事类型）是否开放。没配置过的类型 = 开放（老方案兼容）。
    pub fn group_type_enabled(&self, event_type: &str) -> bool {
        self.group_types.get(event_type).copied().unwrap_or(true)
    }

    /// 管理端/公开端都能看到的 JSON 形态（不含敏感项）
    pub fn to_json(&self) -> Value {
        json!({
            "maleEnabled": self.male_enabled,
            "femaleEnabled": self.female_enabled,
            "daily": self.daily,
            "monthly": self.monthly,
            "yearly": self.yearly,
            "duration": self.duration,
            "trend": self.trend,
            "wave": self.wave,
            "dataEnabled": self.data_enabled,
            "groupEnabled": self.group_enabled,
            "lineageEnabled": self.lineage_enabled,
            "groupTypes": self.group_types,
            "loginRequired": self.login_required,
            "myRegister": self.my_register,
        })
    }
}

/* ------------------------------- 管理端 ------------------------------- */

/// GET /api/admin/public/settings
pub async fn get_settings(State(st): State<ArcState>) -> ApiResult {
    Ok(Json(PublicSettings::load(&st).await.to_json()))
}

/// POST /api/admin/public/settings：整包保存
pub async fn update_settings(State(st): State<ArcState>, Json(body): Json<Value>) -> ApiResult {
    let mut s = PublicSettings::load(&st).await;
    let flag =
        |key: &str, cur: bool| -> bool { body.get(key).and_then(|v| v.as_bool()).unwrap_or(cur) };
    s.male_enabled = flag("maleEnabled", s.male_enabled);
    s.female_enabled = flag("femaleEnabled", s.female_enabled);
    s.daily = flag("daily", s.daily);
    s.monthly = flag("monthly", s.monthly);
    s.yearly = flag("yearly", s.yearly);
    s.duration = flag("duration", s.duration);
    s.trend = flag("trend", s.trend);
    s.wave = flag("wave", s.wave);
    s.data_enabled = flag("dataEnabled", s.data_enabled);
    s.group_enabled = flag("groupEnabled", s.group_enabled);
    s.lineage_enabled = flag("lineageEnabled", s.lineage_enabled);
    s.login_required = flag("loginRequired", s.login_required);
    s.my_register = flag("myRegister", s.my_register);
    // 分组子页：传了 groupTypes（对象）就整包替换；没传保持原样。
    // 未知类型直接存下来也没关系 —— group_type_enabled 对没配置的类型按开处理，
    // 存 false 的才关。
    if let Some(gt) = body.get("groupTypes").and_then(|v| v.as_object()) {
        s.group_types = gt
            .iter()
            .filter_map(|(k, v)| v.as_bool().map(|b| (k.clone(), b)))
            .collect();
    }
    s.save(&st).await;
    crate::events::emit(crate::events::SCOPE_SETTINGS, "公开站点设置已更新");
    Ok(Json(json!({ "ok": true, "settings": s.to_json() })))
}

/* --------------------- 我的页面账号（管理端手动添加） --------------------- */

/// GET /api/admin/my-accounts：账号列表
pub async fn admin_my_accounts(State(st): State<ArcState>) -> ApiResult {
    Ok(Json(
        json!({ "items": crate::store::my_account_list(&st).await }),
    ))
}

/// POST /api/admin/my-accounts {name, phone, pass}：手动设置账号（存在则覆盖更新；密码纯数字，手机号选填）
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
    let pass = body
        .get("pass")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    crate::store::my_account_set(&st, name, phone, pass)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(Json(json!({ "ok": true })))
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

/// DELETE /api/admin/my-accounts/{name}：删除账号（幂等，不存在也返回 ok）
pub async fn admin_my_account_delete(
    State(st): State<ArcState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> ApiResult {
    crate::store::my_account_delete(&st, &name).await;
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
    tracing::info!(account = %name, anchor = %r.name, team = %r.team, "管理员改绑展示端账号");
    Ok(Json(json!({
        "ok": true, "name": r.name, "team": r.team, "personId": r.person_id,
    })))
}

/* ------------------------------- 防遍历限流 ------------------------------- */

/// 验证接口限流：每 IP 每分钟 5 次（姓名/密码撞库防护）
static VERIFY_LIMITER: std::sync::OnceLock<VerifyLimiter> = std::sync::OnceLock::new();
/// 注册类接口限流：每 IP 每分钟 12 次（一次注册本来就要「申请授权 → 重新生成 → 提交」好几下，
/// 跟登录撞库不是一回事，单独一档更宽松；仍然防脚本刷）
static REGISTER_LIMITER: std::sync::OnceLock<VerifyLimiter> = std::sync::OnceLock::new();

struct VerifyLimiter {
    hits: std::sync::Mutex<HashMap<String, Vec<Instant>>>,
    /// 每分钟允许的次数
    limit: usize,
}

impl VerifyLimiter {
    fn new(limit: usize) -> Self {
        Self {
            hits: std::sync::Mutex::new(HashMap::new()),
            limit,
        }
    }

    fn allow(&self, ip: &str) -> bool {
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
/// **来自 127.0.0.1 的连接**带来的 XFF（取第一个 IP，即反代记录的真实客户端），
/// 非本机连接一律用 ConnectInfo（伪造的 XFF 直接忽略）。
fn client_ip_for_limit(addr: std::net::SocketAddr, headers: &HeaderMap) -> String {
    if addr.ip().is_loopback() {
        if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(first) = xff.split(',').next() {
                let ip = first.trim();
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
type MyTokenMap = HashMap<String, (u64, String, String, Instant)>;

static MY_TOKENS: std::sync::OnceLock<std::sync::Mutex<MyTokenMap>> = std::sync::OnceLock::new();

fn my_tokens() -> &'static std::sync::Mutex<MyTokenMap> {
    MY_TOKENS.get_or_init(std::sync::Mutex::default)
}

/// 进程内随机数来源：RandomState 每次构造都带 OS 级随机种子，省一个 rand 依赖
fn rand_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

fn my_token_put(person_id: u64, team: String, name: String) -> String {
    // token 不再编入 pid/时间戳（老格式可被相邻值猜中），纯随机串，映射关系只存在内存里
    let token = format!("my-{:016x}-{:016x}", rand_u64(), rand_u64());
    let mut m = my_tokens().lock().expect("my token 锁 poisoned");
    m.retain(|_, (_, _, _, at)| at.elapsed() < Duration::from_secs(24 * 3600));
    m.insert(token.clone(), (person_id, team, name, Instant::now()));
    token
}

fn my_token_take(token: &str) -> Option<(u64, String, String)> {
    let mut m = my_tokens().lock().expect("my token 锁 poisoned");
    m.retain(|_, (_, _, _, at)| at.elapsed() < Duration::from_secs(24 * 3600));
    m.get(token)
        .map(|(pid, team, name, _)| (*pid, team.clone(), name.clone()))
}

/// 只读一眼 token 对应的账号（不消费）：公开 API 中间件用它给访问日志带账号。
/// 未登录的访客返回 None，日志就只记 IP。
pub(crate) fn my_token_peek(token: &str) -> Option<(u64, String, String)> {
    my_token_take(token)
}

/// 展示端登录门（2026-10-10）：/st/{team} 的数据接口一律要求「我的」登录态，
/// 未登录只给一张登录卡。token 走 ?access_token=（fetch/img 通用）或 X-My-Token 头。
/// 校验通过返回 person_id；token 与路径团别不符也拒绝。
///
/// 2026-10-10 加开关：后台关掉「开启登录」（login_required=false）时全站免登录开放，
/// 返回 person_id=0（一个不存在的档案 id，调用方只拿它判「管理员/游客」，不下发个人数据）。
/// 带 token 的照常解析，所以关登录时之前登过的人仍能直接用「我的」。
fn gate_team(
    login_required: bool,
    team: &str,
    q_token: Option<&str>,
    headers: &HeaderMap,
) -> Result<u64, ApiError> {
    let token = q_token
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| {
            headers
                .get("x-my-token")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
    // 有 token 就先验（开关关着时也认，访客拿旧 token 仍能看「我的」）
    if let Some(t) = token.as_deref().filter(|s| !s.is_empty()) {
        if let Some((pid, my_team, _)) = my_token_take(t) {
            if my_team == team {
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

/// 各数据接口共用的查询参数：登录门 token 可以跟在任意子接口的 URL 上
#[derive(Deserialize)]
pub struct GateQuery {
    #[serde(rename = "access_token")]
    pub access_token: Option<String>,
}

/* ------------------------------- 站点数据 ------------------------------- */

/// GET /api/public/teams/{team}：站点配置 + 最新日期
/// 团链接被关闭时返回 published=false（前端展示「暂未公布」占位，不是 404）
/// 展示端登录门（2026-10-10）：要「我的」登录态
pub async fn site(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(g): Query<GateQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    gate_team(s.login_required, &team, g.access_token.as_deref(), &headers)?;
    let dates = crate::repos::get_available_dates(&st, None, false).await?;
    Ok(Json(json!({
        "published": true,
        "team": team,
        "latestDate": dates.first(),
        "dates": dates,
        "settings": {
            "daily": s.daily,
            "monthly": s.monthly,
            "yearly": s.yearly,
            "duration": s.duration,
            "trend": s.trend,
            "wave": s.wave,
            // 页面级显隐（2026-10-10）：每日报告 / 分组 / 族谱 / 分组子页 / 登录开关
            "dataEnabled": s.data_enabled,
            "groupEnabled": s.group_enabled,
            "lineageEnabled": s.lineage_enabled,
            "groupTypes": s.group_types,
            "loginRequired": s.login_required,
        },
    })))
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub date: Option<String>,
    pub period: Option<String>,
    #[serde(rename = "access_token")]
    pub access_token: Option<String>,
}

/// GET /api/public/teams/{team}/daily?date=
/// DTO 白名单：名次 / 名字 / 音浪 / 分钟 / 开播——绝无抖音号
pub async fn daily(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    gate_team(s.login_required, &team, q.access_token.as_deref(), &headers)?;
    if !s.daily {
        return Ok(Json(json!({ "published": true, "dailyEnabled": false })));
    }
    // 每日报告页整体显隐（2026-10-10）：关掉后榜单数据不下发
    if !s.data_enabled {
        return Ok(Json(json!({ "published": true, "dailyEnabled": false })));
    }
    let dates = crate::repos::get_available_dates(&st, None, false).await?;
    let date = match q.date.as_deref() {
        Some(d) if valid_date_pub(d) && dates.iter().any(|x| x == d) => d.to_string(),
        _ => dates.first().cloned().unwrap_or_default(),
    };
    if date.is_empty() {
        return Err(anyhow::anyhow!("暂未公布数据").into());
    }
    let source = sid(&st);
    // 前一业务日（用于名次变动 ↑↓）
    let idx = dates.iter().position(|d| d == &date);
    let prev_date = idx.and_then(|i| dates.get(i + 1)).cloned();

    /* 全团在册主播都要出现在榜上：当天没有采集记录的人走 LEFT JOIN 得到 0 音浪 /
    未开播，前端标记为「未开播」。只在 daily_metric 里捞人的话，某天只录了 1 个人时
    榜单就真只剩 1 行，跟库里的在册人数对不上。
    hide_in_daily_report = 1 的主播（管理端逐人设置）完全不进榜、不占名次。 */
    let sql = format!(
        "SELECT p.id AS person_id, p.name, \
         CAST(COALESCE(SUM(d.wave), 0) AS SIGNED) AS wave, \
         CAST(COALESCE(SUM(d.minutes), 0) AS SIGNED) AS minutes, \
         CASE WHEN COALESCE(SUM(d.wave), 0) >= 2 THEN 1 ELSE 0 END AS live, \
         COUNT(d.id) AS nrow \
         FROM person p \
         LEFT JOIN daily_metric d ON d.person_id = p.id \
           AND DATE_FORMAT(d.biz_date, '%Y-%m-%d') = '{date}' \
           AND d.wave_reliable = 1 \
         WHERE p.deleted_at IS NULL AND p.gender = '{team}' AND p.status <> 'left' \
         AND p.hide_in_daily_report = 0 \
         GROUP BY p.id, p.name \
         ORDER BY nrow DESC, wave DESC, p.name ASC"
    );
    let rows = mysql::query(&st.pools, &source, &sql).await?;

    // 当月未播天数 + 当月累计音浪 + 前一日名次（内存 join）
    // 未播口径（2026-10-09 用户定；2026-10-10 P1-10 统一引用 dialect::NOT_LIVE_WAVE_THRESHOLD）：
    // 当天导入音浪 < 2 = 未开播；
    // totalWave 从「历史累计」改为「当月累计」（榜单副行显示的就是本月总音浪）。
    // P1-7：三处聚合都只算可信行（wave_reliable=1），与 WAVES_A / 月年榜口径一致。
    let month = &date[..7];
    let absent_sql = format!(
        "SELECT person_id, CAST(SUM(CASE WHEN wave < {NOT_LIVE_WAVE_THRESHOLD} THEN 1 ELSE 0 END) AS SIGNED) AS n \
         FROM daily_metric WHERE wave_reliable = 1 \
           AND DATE_FORMAT(biz_date, '%Y-%m') = '{month}' GROUP BY person_id"
    );
    let total_sql = format!(
        "SELECT person_id, CAST(SUM(wave) AS SIGNED) AS total \
         FROM daily_metric WHERE wave_reliable = 1 \
           AND DATE_FORMAT(biz_date, '%Y-%m') = '{month}' GROUP BY person_id"
    );
    let mut absent_map: HashMap<u64, i64> = Default::default();
    for r in mysql::query(&st.pools, &source, &absent_sql).await? {
        absent_map.insert(row_pid(&r, "person_id"), row_i64(&r, "n"));
    }
    let mut total_map: HashMap<u64, i64> = Default::default();
    for r in mysql::query(&st.pools, &source, &total_sql).await? {
        total_map.insert(row_pid(&r, "person_id"), row_i64(&r, "total"));
    }
    let mut prev_rank: HashMap<u64, usize> = Default::default();
    if let Some(pd) = &prev_date {
        let psql = format!(
            "SELECT p.id AS person_id \
             FROM daily_metric d JOIN person p ON p.id = d.person_id \
             WHERE p.deleted_at IS NULL AND p.gender = '{team}' \
             AND p.hide_in_daily_report = 0 AND d.wave_reliable = 1 \
             AND DATE_FORMAT(d.biz_date, '%Y-%m-%d') = '{pd}' \
             GROUP BY p.id, p.name ORDER BY SUM(d.wave) DESC"
        );
        for (i, r) in mysql::query(&st.pools, &source, &psql)
            .await?
            .iter()
            .enumerate()
        {
            prev_rank.insert(row_pid(r, "person_id"), i + 1);
        }
    }

    // 名次只在「当天有数据」的人之间排：没采集记录的人不给名次，前端显示「—」，
    // 否则明明没数据的人会因为有 80 个同僚也排在 40 名，读者一眼看不出他没播。
    let mut last_wave: Option<i64> = None;
    let mut last_rank = 0usize;
    let mut ranked = 0usize;
    let mut items: Vec<Value> = Vec::with_capacity(rows.len());
    for r in rows.iter() {
        let pid = row_pid(r, "person_id");
        let has_data = row_i64(r, "nrow") > 0;
        let cur = row_i64(r, "wave");
        let rank = if has_data {
            ranked += 1;
            if Some(cur) == last_wave {
                last_rank
            } else {
                last_rank = ranked;
                last_rank
            }
        } else {
            0
        };
        if has_data {
            last_wave = Some(cur);
        }
        let delta = match prev_rank.get(&pid).copied() {
            // 前一天压根没他（没数据）或今天没数据：都不算升降
            Some(pr) if has_data => Some(if pr > rank {
                json!({ "dir": "up", "n": pr - rank })
            } else if pr < rank {
                json!({ "dir": "down", "n": rank - pr })
            } else {
                json!({ "dir": "flat" })
            }),
            _ => None,
        };
        let mut item = json!({
            // P2-19：公开榜单不再下发 personId（对齐 public.rs 的白名单策略 ——
            // 展示端团数据视图只按 name/isLive/rank 渲染，不依赖这个内部 id；
            // 需要 id 的只有 push 搜索/绑定接口，那些接口单独下发）
            "name": r.try_get::<String, _>("name").unwrap_or_default(),
            "wave": cur,
            "isLive": row_live(r),
            "hasData": has_data,
            "absentDays": absent_map.get(&pid).copied().unwrap_or(0),
            "totalWave": total_map.get(&pid).copied().unwrap_or(0),
            "rank": rank,
        });
        if s.duration {
            item["minutes"] = json!(row_i64(r, "minutes"));
        }
        if s.trend {
            item["delta"] = delta.unwrap_or(Value::Null);
        }
        if !s.wave {
            // 音浪数值显隐（后台开关）：关掉后不下发任何数字
            item["wave"] = Value::Null;
            item["totalWave"] = Value::Null;
        }
        items.push(item);
    }
    Ok(Json(json!({
        "published": true,
        "date": date,
        "teamSize": rows.len(),
        // P2-13：liveCount 语义对齐为「开播人数」（当日音浪 ≥ 未播阈值），
        // 原来统计的是「有数据人数」，字段名与口径不符
        "liveCount": items
            .iter()
            .filter(|i| {
                i["wave"].as_i64().map(is_live_wave).unwrap_or(false)
            })
            .count(),
        "items": items,
    })))
}
/// GET /api/public/teams/{team}/lineage：师门树（公开端画布用）
///
/// DTO 白名单：id / 名字 / 师傅 id+名 / 状态 / 代数 / 直系徒弟数——绝无抖音号。
/// 师傅不在本团（含 master 为空）的当作树根。离团（status='left'）的人也在树里
/// （家族成员不因离团消失），前端负责淡显。
pub async fn lineage(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(g): Query<GateQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    if !s.lineage_enabled {
        return Ok(Json(json!({ "published": true, "lineageEnabled": false })));
    }
    gate_team(s.login_required, &team, g.access_token.as_deref(), &headers)?;
    /* 族谱只显示有师门归属的人（2026-10-09 规则）：没有师傅且没有徒弟的孤立节点
    不下发（画布和导出图的「没有师傅」名单自动联动）。
    与榜单显隐（hide_in_daily_report）是两个独立配置：榜单隐藏的人只要挂在
    师门下，族谱照常显示。 */
    let sql = format!(
        "SELECT p.id, p.name, p.master_id, m.name AS master_name, p.status, p.generation, \
         (SELECT COUNT(*) FROM person k WHERE k.master_id = p.id AND k.deleted_at IS NULL) AS disciples \
         FROM person p LEFT JOIN person m ON m.id = p.master_id \
         WHERE p.deleted_at IS NULL AND p.gender = '{team}' \
         AND (p.master_id IS NOT NULL \
              OR EXISTS(SELECT 1 FROM person k WHERE k.master_id = p.id AND k.deleted_at IS NULL))"
    );
    let rows = mysql::query(&st.pools, &sid(&st), &sql).await?;

    let mut nodes: Vec<Value> = Vec::with_capacity(rows.len());
    let mut ids: std::collections::HashSet<u64> = Default::default();
    for r in &rows {
        let id = row_pid(r, "id");
        ids.insert(id);
    }
    for r in &rows {
        let master_id = read_opt_u64_field(r, "master_id").filter(|m| ids.contains(m));
        nodes.push(json!({
            "id": row_pid(r, "id"),
            "name": r.try_get::<String, _>("name").unwrap_or_default(),
            "masterId": master_id,
            "masterName": r.try_get::<Option<String>, _>("master_name").ok().flatten(),
            "status": r.try_get::<String, _>("status").unwrap_or_default(),
            "generation": r.try_get::<Option<i64>, _>("generation").ok().flatten(),
            "disciples": row_i64(r, "disciples"),
        }));
    }
    Ok(Json(json!({
        "published": true,
        "team": team,
        "nodes": nodes,
    })))
}

fn read_opt_u64_field(r: &sqlx::mysql::MySqlRow, col: &str) -> Option<u64> {
    use sqlx::Row;
    if let Ok(v) = r.try_get::<u64, _>(col) {
        return Some(v);
    }
    if let Ok(v) = r.try_get::<Option<i64>, _>(col) {
        return v.map(|x| x.max(0) as u64);
    }
    if let Ok(v) = r.try_get::<i64, _>(col) {
        return Some(v.max(0) as u64);
    }
    None
}

/// GET /api/public/teams/{team}/monthly?period=YYYY-MM（或 /yearly?period=YYYY）
pub async fn monthly(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    gate_team(s.login_required, &team, q.access_token.as_deref(), &headers)?;
    period_public(&st, &team, &q, true).await
}

pub async fn yearly(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    gate_team(s.login_required, &team, q.access_token.as_deref(), &headers)?;
    period_public(&st, &team, &q, false).await
}

async fn period_public(st: &Arc<AppState>, team: &str, q: &ListQuery, is_month: bool) -> ApiResult {
    let s = PublicSettings::load(st).await;
    if !s.team_enabled(team) {
        return Ok(Json(json!({ "published": false })));
    }
    let enabled = if is_month { s.monthly } else { s.yearly };
    if !enabled {
        return Ok(Json(json!({ "published": true, "periodEnabled": false })));
    }
    let period = match q.period.as_deref() {
        Some(p)
            if !p.trim().is_empty()
                && p.chars().all(|c| c.is_ascii_digit() || c == '-')
                && p.len() == if is_month { 7 } else { 4 } =>
        {
            p.trim().to_string()
        }
        _ if is_month => chrono::Local::now().format("%Y-%m").to_string(),
        _ => chrono::Local::now().format("%Y").to_string(),
    };
    // 与 daily 口径对齐：离队的人不进榜（P2-19：也不再下发 personId，
    // 前端月/年榜只按 name/rank/wave 渲染）。
    // P1-7：period_rollup_sql 内层已统一按 wave_reliable=1 过滤。
    // 时长口径 2026-10-10：月 / 年里的「时长」优先用按月导入的整月值（monthly_duration，
    // 平台月报），没有整月记录的月份才用日明细求和 —— 见 dialect::period_rollup_sql。
    let rows = dialect::query_period(
        &st.pools,
        &sid(st),
        "p.id AS person_id, p.name, \
         CAST(SUM(t.w) AS SIGNED) AS wave, \
         CAST(SUM({minutes}) AS SIGNED) AS minutes, \
         CAST(SUM(t.live) AS SIGNED) AS live_days",
        &format!("{period}%"),
        team,
        " AND p.status <> 'left' AND p.hide_in_daily_report = 0",
        "wave DESC, p.name ASC",
    )
    .await?;
    // 名次：同音浪并列，下一档跳号（与 daily / 分组页的名次算法一致）
    let mut last_wave: Option<i64> = None;
    let mut last_rank = 0usize;
    let mut items: Vec<Value> = Vec::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        let wave = row_i64(r, "wave");
        let rank = if Some(wave) == last_wave {
            last_rank
        } else {
            last_rank = i + 1;
            last_rank
        };
        last_wave = Some(wave);
        let mut item = json!({
            // P2-19：公开月/年榜不再下发 personId（对齐 public.rs 白名单策略）
            "name": r.try_get::<String, _>("name").unwrap_or_default(),
            "wave": wave,
            "rank": rank,
        });
        if s.duration {
            item["minutes"] = json!(row_i64(r, "minutes"));
            item["liveDays"] = json!(row_i64(r, "live_days"));
        }
        if !s.wave {
            item["wave"] = Value::Null;
        }
        items.push(item);
    }
    Ok(Json(
        json!({ "published": true, "period": period, "items": items }),
    ))
}

fn row_pid(r: &sqlx::mysql::MySqlRow, col: &str) -> u64 {
    if let Ok(v) = r.try_get::<u64, _>(col) {
        return v;
    }
    if let Ok(v) = r.try_get::<i64, _>(col) {
        return v as u64;
    }
    0
}

fn valid_date_pub(s: &str) -> bool {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

fn row_live(r: &sqlx::mysql::MySqlRow) -> bool {
    if let Ok(v) = r.try_get::<i8, _>("live") {
        return v == 1;
    }
    if let Ok(v) = r.try_get::<i64, _>("live") {
        return v == 1;
    }
    r.try_get::<String, _>("live")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .map(|v| v == 1)
        .unwrap_or(false)
}

fn row_i64(r: &sqlx::mysql::MySqlRow, col: &str) -> i64 {
    if let Ok(v) = r.try_get::<i64, _>(col) {
        return v;
    }
    if let Ok(v) = r.try_get::<u64, _>(col) {
        return v as i64;
    }
    if let Ok(v) = r.try_get::<String, _>(col) {
        return v.parse().unwrap_or(0);
    }
    0
}

/* ------------------------------- 我的（验证 + 个人数据） ------------------------------- */

/// POST /api/public/teams/{team}/verify {account, pass}（account = 手机号或姓名；兼容旧 {name, pass}）
/// 没有注册：账号由管理员在后台手动添加（SQLite my_accounts，密码纯数字，手机号选填）。
/// 验证通过后在本团主播台账里按姓名找人（同名时在团者优先）；只返回「成功 + token」
/// 或统一失败 {ok:false}，绝不下发候选名单。
pub async fn verify(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    /* 地区门禁：公开 API 层的中间件已经统一拦了，这里再过一遍是因为 verify
    要按「登录尝试」记日志——外省被拒的尝试也进 access_logs，后台访客日志能看到
    谁在门外试了什么账号（IP_GATE_DISABLED=1 的本地开发直接放行）。 */
    let client_ip = crate::ipinfo::request_ip(addr, &headers);
    let geo = crate::ipinfo::geo_for_ip(&st, &client_ip)
        .await
        .unwrap_or_default();
    let account_input = body
        .get("account")
        .or_else(|| body.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !crate::ipinfo::gate_disabled() && !geo.allowed {
        crate::ipinfo::log_login(&st, &client_ip, &geo, &team, &account_input, false).await;
        return Err(ApiError::forbidden("当前地区暂不支持访问"));
    }
    let limiter_ok = public_write_allow(&client_ip_for_limit(addr, &headers));
    if !limiter_ok {
        return Err(anyhow::anyhow!("尝试太频繁，请稍后再试").into());
    }
    // 账号 = 手机号或姓名：新前端传 account，老前端传 name，两者都行
    let acct = body
        .get("account")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| {
            body.get("name")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
        .unwrap_or_default();
    let pass = body
        .get("pass")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if acct.is_empty() || pass.trim().is_empty() {
        return Err(anyhow::anyhow!("请输入账号和密码").into());
    }

    /* 1. 账号校验：手机号+密码 或 姓名+密码（my_accounts 名单，管理员手动维护 / 手机号自助注册） */
    let Some(account_name) = crate::store::my_account_check_login(&st, &acct, &pass).await else {
        crate::ipinfo::log_login(&st, &client_ip, &geo, &team, &acct, false).await;
        return Ok(Json(json!({ "ok": false }))); // 统一失败，不区分「账号不存在/密码错」
    };
    crate::ipinfo::log_login(&st, &client_ip, &geo, &team, &account_name, true).await;

    /* 1.5 手机号自助注册还没绑主播的账号：先放进来（personId=0），登录后「我的」页
    会强制让用户绑定主播——不绑定看不到个人数据，也无从套用别人的姓名。 */
    if let Some(info) = crate::store::my_account_info(&st, &account_name).await {
        if !info.bound && !info.is_admin {
            let token = my_token_put(0, team.clone(), account_name.clone());
            return Ok(Json(json!({
                "ok": true,
                "personId": 0,
                "name": account_name,
                "token": token,
                "bound": false,
                "needBind": true,
            })));
        }
    }

    /* 2. 台账里按姓名找本团的人。找不到 = 管理员加了账号但台账还没有这个人
    （或名字打错了），同样返回统一失败。绑定用 bind，姓名是用户输入不能拼 SQL。 */
    let sql = format!(
        "SELECT p.id, p.name FROM person p \
         WHERE p.deleted_at IS NULL AND p.gender = '{team}' AND p.name = ? \
         ORDER BY CASE WHEN p.status = 'left' THEN 1 ELSE 0 END, p.id \
         LIMIT 1"
    );
    let rows =
        mysql::query_with_binds(&st.pools, &sid(&st), &sql, |q| q.bind(account_name.clone()))
            .await?;
    match rows.into_iter().next() {
        Some(row) => {
            let pid = row
                .try_get::<i64, _>("id")
                .map(|v| v as u64)
                .or_else(|_| row.try_get::<u64, _>("id"))
                .unwrap_or(0);
            let token = my_token_put(pid, team, account_name.clone());
            Ok(Json(json!({
                "ok": true, "personId": pid, "name": account_name, "token": token, "bound": true
            })))
        }
        None => {
            // 台账里没有同名主播：管理员观看账号（is_admin=1，如 shuai4444）放行，
            // personId=0——榜单/族谱/分组照常看，「我的」个人数据页为空。
            if !crate::store::my_account_is_admin(&st, &account_name).await {
                return Ok(Json(json!({ "ok": false })));
            }
            let token = my_token_put(0, team, account_name.clone());
            Ok(Json(json!({
                "ok": true, "personId": 0, "name": account_name, "token": token,
                "admin": true, "bound": true
            })))
        }
    }
}

/* ----------------- 手机号注册（WorkBuddy 网关 OAuth）+ 绑定主播 -----------------
 *
 * 用户口径（2026-10-10）：
 *   1. 展示端登录门加「注册」——注册同时调用 WorkBuddy 网关的「添加账号 (OAuth)」，
 *      那张授权页本身就是手机号 + 短信验证码登录；
 *   2. 验证码数字就是密码，手机号就是账号（所以这里不需要短信通道）；
 *   3. 注册完提醒绑定主播姓名，绑定之后用户自己不能解绑（只有管理员能改绑）。
 *
 * 分工：面板密码只在服务端，浏览器只拿 state + authUrl（`wbgateway` 模块）。
 */

/// 登录验证限流：每 IP 每分钟 5 次（登录门撞库防护）
fn public_write_allow(ip: &str) -> bool {
    VERIFY_LIMITER
        .get_or_init(|| VerifyLimiter::new(5))
        .allow(ip)
}

/// 注册类写接口限流：每 IP 每分钟 12 次（一次注册本来就要接好几下，单独一档）
fn public_register_allow(ip: &str) -> bool {
    REGISTER_LIMITER
        .get_or_init(|| VerifyLimiter::new(12))
        .allow(ip)
}

/// 注册开关（用户口径 2026-10-10「注册先不开」）：默认关，后台「公开站点设置 → 开放手机号注册」开。
/// 关着时注册相关接口一律 400；登录、绑定、我的页完全不受影响。
async fn ensure_register_open(st: &AppState) -> Result<(), ApiError> {
    if PublicSettings::load(st).await.my_register {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "注册功能暂未开放（管理员可在后台「公开站点设置 → 开放手机号注册」里打开）",
        ))
    }
}

/// GET /api/public/my/register/config：注册页能力探测（功能开没开 + 网关配没配）
pub async fn my_register_config(State(st): State<ArcState>) -> ApiResult {
    let open = PublicSettings::load(&st).await.my_register;
    Ok(Json(json!({
        "open": open,
        "oauth": open && st.wb.get().is_some(),
        "platform": "CLI",
    })))
}

/// POST /api/public/my/register/oauth/start {realm}：向网关申请一条「添加账号」授权链接。
/// 返回 {state, authUrl, realm}；面板密码不出服务端。
pub async fn my_register_oauth_start(
    State(st): State<ArcState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    ensure_register_open(&st).await?;
    if !public_register_allow(&client_ip_for_limit(addr, &headers)) {
        return Err(anyhow::anyhow!("尝试太频繁，请稍后再试").into());
    }
    let Some(wb) = st.wb.get() else {
        return Err(anyhow::anyhow!(
            "服务端还没有配置 WorkBuddy 网关（WORKBUDDY_PANEL_PASSWORD），暂时不能自动添加账号"
        )
        .into());
    };
    let realm = body
        .get("realm")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (state, auth_url, realm) = wb
        .login_start(realm)
        .await
        .map_err(|e| anyhow::anyhow!("申请授权链接失败：{e}"))?;
    Ok(Json(
        json!({ "ok": true, "state": state, "authUrl": auth_url, "realm": realm }),
    ))
}

#[derive(Deserialize)]
pub struct OauthPollQuery {
    pub state: Option<String>,
}

/// GET /api/public/my/register/oauth/poll?state=：轮询授权结果（原样转发网关状态）
pub async fn my_register_oauth_poll(
    State(st): State<ArcState>,
    Query(q): Query<OauthPollQuery>,
) -> ApiResult {
    let state = q.state.unwrap_or_default();
    let state = state.trim();
    if state.is_empty() || state.len() > 128 {
        return Err(anyhow::anyhow!("缺少授权流水号 state").into());
    }
    ensure_register_open(&st).await?;
    let Some(wb) = st.wb.get() else {
        return Err(anyhow::anyhow!("服务端还没有配置 WorkBuddy 网关，不能查询授权状态").into());
    };
    let v = wb.login_poll(state).await.map_err(|e| anyhow::anyhow!(e))?;
    let status = v
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("unknown")
        .to_string();
    let message = v
        .get("message")
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_string();
    let nickname = v
        .pointer("/account/nickname")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    Ok(Json(
        json!({ "status": status, "message": message, "nickname": nickname }),
    ))
}

/// POST /api/public/my/register/oauth/cancel {state}：用户关掉弹窗时清理面板上的授权状态
pub async fn my_register_oauth_cancel(
    State(st): State<ArcState>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let state = body
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if PublicSettings::load(&st).await.my_register {
        if let Some(wb) = st.wb.get() {
            if !state.is_empty() && state.len() <= 128 {
                wb.login_cancel(&state).await;
            }
        }
    }
    Ok(Json(json!({ "ok": true })))
}

/// POST /api/public/my/register {phone, code, team, wbState?}
/// **手机号 = 账号，验证码 = 密码**；注册成功直接发登录 token（personId=0，bound=false），
/// 前端进站后强制提示绑定主播。
pub async fn my_register(
    State(st): State<ArcState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    ensure_register_open(&st).await?;
    if !public_register_allow(&client_ip_for_limit(addr, &headers)) {
        return Err(anyhow::anyhow!("尝试太频繁，请稍后再试").into());
    }
    let phone = body
        .get("phone")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let code = body
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let wb_state = body
        .get("wbState")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    // 团别：注册还没有主播，token 先挂在当前这个团（绑定成功后按主播所在团换团）
    let team = normalize_team(body.get("team").and_then(|v| v.as_str()).unwrap_or(""));

    let name = crate::store::my_account_register(&st, phone, code, wb_state)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    let token = my_token_put(0, team.clone(), name.clone());
    tracing::info!(phone = %name, team = %team, "展示端手机号注册成功");
    Ok(Json(json!({
        "ok": true,
        "token": token,
        "name": name,
        "team": team,
        "personId": 0,
        "bound": false,
        "needBind": true,
    })))
}

#[derive(Deserialize)]
pub struct BindSearchQuery {
    pub token: Option<String>,
    pub q: Option<String>,
}

/// GET /api/public/my/bind/search?token=&q=：绑定页搜主播（全台账，注册账号还没有团别）
pub async fn my_bind_search(
    State(st): State<ArcState>,
    Query(q): Query<BindSearchQuery>,
) -> ApiResult {
    let token = q.token.unwrap_or_default();
    if my_token_take(token.trim()).is_none() {
        return Err(ApiError::auth("登录已过期，请重新登录"));
    }
    let items =
        crate::store::all_anchor_search(&st, &sid(&st), q.q.as_deref().unwrap_or(""), 20).await;
    Ok(Json(json!({ "items": items })))
}

/// POST /api/public/my/bind {token, personId}：绑定主播（**只有一次**）。
/// 绑定成功后换发新 token（带主播 personId 与所在团），前端据此刷新身份/换团。
pub async fn my_bind(State(st): State<ArcState>, axum::Json(body): axum::Json<Value>) -> ApiResult {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
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
    let Some((_, _, token_name)) = my_token_take(token.trim()) else {
        return Err(ApiError::auth("登录已过期，请重新登录"));
    };
    let r = crate::store::my_account_bind(&st, &token_name, person_id)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    let new_token = my_token_put(r.person_id.max(0) as u64, r.team.clone(), r.name.clone());
    tracing::info!(account = %token_name, anchor = %r.name, team = %r.team, "展示端账号绑定主播");
    crate::events::emit(
        crate::events::SCOPE_ROSTER,
        format!("展示端账号「{token_name}」绑定主播「{}」", r.name),
    );
    Ok(Json(json!({
        "ok": true,
        "token": new_token,
        "name": r.name,
        "team": r.team,
        "personId": r.person_id,
        "bound": true,
    })))
}

/// 团别白名单：male / female（兼容 m / f），其它一律空串（老链接/异常入参）
fn normalize_team(v: &str) -> String {
    match v.trim().to_ascii_lowercase().as_str() {
        "male" | "m" => "male".to_string(),
        "female" | "f" => "female".to_string(),
        _ => String::new(),
    }
}

#[derive(Deserialize)]
pub struct MyQuery {
    pub token: Option<String>,
    /// 趋势图取几个业务日（默认 14，上限 30）
    pub days: Option<i64>,
}

/* --------------------- 个人页时长口径（2026-10-10 月度时长导入） --------------------- */

/// 个人时长统计的三种窗口
pub enum MinutesScope<'a> {
    Month(&'a str),
    Year(&'a str),
    Total,
}

impl MinutesScope<'_> {
    /// 日明细的过滤条件（daily_metric 别名 d）
    fn daily_where(&self) -> String {
        match self {
            MinutesScope::Month(m) => format!(" AND DATE_FORMAT(d.biz_date, '%Y-%m') = '{m}'"),
            MinutesScope::Year(y) => format!(" AND DATE_FORMAT(d.biz_date, '%Y') = '{y}'"),
            MinutesScope::Total => String::new(),
        }
    }

    /// monthly_duration 的过滤条件
    fn period_where(&self) -> String {
        match self {
            MinutesScope::Month(m) => format!(" AND period = '{m}'"),
            MinutesScope::Year(y) => format!(" AND period LIKE '{y}-%'"),
            MinutesScope::Total => String::new(),
        }
    }
}

/// 个人「时长」SQL：有整月导入值的月份用月报值，其余月份用日明细求和
/// （同一段时长不会既算月报又算日明细）。with_monthly = false 时退回纯日明细口径。
fn my_minutes_sql(person_id: u64, scope: &MinutesScope<'_>, with_monthly: bool) -> String {
    let daily = scope.daily_where();
    if !with_monthly {
        return format!(
            "SELECT CAST(COALESCE((SELECT SUM(d.minutes) FROM daily_metric d \
             WHERE d.person_id = {person_id}{daily}), 0) AS SIGNED) AS minutes"
        );
    }
    let period = scope.period_where();
    format!(
        "SELECT CAST(COALESCE((SELECT SUM(minutes) FROM monthly_duration \
          WHERE person_id = {person_id}{period}), 0) \
         + COALESCE((SELECT SUM(d.minutes) FROM daily_metric d \
          WHERE d.person_id = {person_id}{daily} \
            AND DATE_FORMAT(d.biz_date, '%Y-%m') NOT IN \
              (SELECT period FROM monthly_duration WHERE person_id = {person_id}{period})), 0) \
         AS SIGNED) AS minutes"
    )
}

/// 个人「时长」（月 / 年 / 累计）：库还没跑 007 迁移（monthly_duration 不存在）时退回日明细口径，
/// 个人页不因为少一张表打不开。
pub async fn my_minutes(
    st: &AppState,
    person_id: u64,
    scope: &MinutesScope<'_>,
) -> anyhow::Result<i64> {
    let source = sid(st);
    let sql = my_minutes_sql(person_id, scope, true);
    let rows = match mysql::query(&st.pools, &source, &sql).await {
        Ok(rows) => rows,
        Err(e) if dialect::is_missing_month_table(&e) => {
            let sql = my_minutes_sql(person_id, scope, false);
            mysql::query(&st.pools, &source, &sql).await?
        }
        Err(e) => return Err(e),
    };
    Ok(rows.first().map(|r| row_i64(r, "minutes")).unwrap_or(0))
}

/// GET /api/public/teams/{team}/my?token=
/// 个人数据：日/月/年 + 累计 + 团内名次（无密码、无抖音号字段）
pub async fn my(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<MyQuery>,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    let token = q.token.unwrap_or_default();
    if token.is_empty() {
        return Err(anyhow::anyhow!("缺少验证 token，请先验证姓名和密码").into());
    }
    let Some((person_id, my_team, _)) = my_token_take(&token) else {
        return Err(anyhow::anyhow!("验证已过期，请重新验证").into());
    };
    if my_team != team {
        return Err(anyhow::anyhow!("该账号不属于本团").into());
    }

    // 个人各口径
    async fn one(st: &AppState, sql: String) -> anyhow::Result<Value> {
        let source = sid(st);
        let rows = mysql::query(&st.pools, &source, &sql).await?;
        Ok(rows
            .into_iter()
            .next()
            .map(|r| {
                json!({
                    "wave": row_i64(&r, "wave"),
                    "minutes": row_i64(&r, "minutes"),
                })
            })
            .unwrap_or(json!({ "wave": 0, "minutes": 0 })))
    }
    /* 「今日」必须是库里最新的业务日，不能用自然日今天。
    数据是每晚 24 点后补前一天的，凌晨打开页面时今天的记录还不存在，
    用自然日会让个人页显示全 0，跟榜单对不上。 */
    let dates = crate::repos::get_available_dates(&st, None, false).await?;
    let today = dates.first().cloned().unwrap_or_default();
    let month = today
        .get(..7)
        .map(str::to_string)
        .unwrap_or_else(|| chrono::Local::now().format("%Y-%m").to_string());
    let year = today
        .get(..4)
        .map(str::to_string)
        .unwrap_or_else(|| chrono::Local::now().format("%Y").to_string());

    /* 个人各口径音浪（P1-7：与榜单口径一致，只聚合可信行 wave_reliable=1；
    total 的口径选择见 repos::get_waves 注释 —— 含基线日，但基线日行本身
    wave_reliable=0 不可信，被行级过滤后不影响数值，只保留在日期轴上）。 */
    let zero = || json!({ "wave": 0, "minutes": 0 });
    let daily = if s.daily && !today.is_empty() {
        Some(
            one(
                &st,
                format!(
            "SELECT CAST(SUM(wave) AS SIGNED) AS wave, CAST(SUM(minutes) AS SIGNED) AS minutes \
             FROM daily_metric WHERE person_id = {person_id} AND wave_reliable = 1 \
             AND DATE_FORMAT(biz_date, '%Y-%m-%d') = '{today}'"
        ),
            )
            .await?,
        )
    } else if s.daily {
        Some(zero())
    } else {
        None
    };
    let mut monthly = if s.monthly {
        Some(
            one(
                &st,
                format!(
            "SELECT CAST(SUM(wave) AS SIGNED) AS wave, CAST(SUM(minutes) AS SIGNED) AS minutes \
             FROM daily_metric WHERE person_id = {person_id} AND wave_reliable = 1 \
             AND DATE_FORMAT(biz_date, '%Y-%m') = '{month}'"
        ),
            )
            .await?,
        )
    } else {
        None
    };
    let mut yearly = if s.yearly {
        Some(
            one(
                &st,
                format!(
            "SELECT CAST(SUM(wave) AS SIGNED) AS wave, CAST(SUM(minutes) AS SIGNED) AS minutes \
             FROM daily_metric WHERE person_id = {person_id} AND wave_reliable = 1 \
             AND DATE_FORMAT(biz_date, '%Y') = '{year}'"
        ),
            )
            .await?,
        )
    } else {
        None
    };
    let mut total = one(
        &st,
        format!(
            "SELECT CAST(SUM(wave) AS SIGNED) AS wave, CAST(SUM(minutes) AS SIGNED) AS minutes \
         FROM daily_metric WHERE person_id = {person_id} AND wave_reliable = 1"
        ),
    )
    .await?;

    /* 时长口径（2026-10-10 月度时长导入）：日明细里已经没有时长（每日不能导入时长），
    时长只有按月的整月值。所以月 / 年 / 累计的「时长」按「某月有整月导入值就用它、
    没有才用日明细求和」算，音浪仍走 daily_metric（音浪不按月导入）。 */
    if let Some(v) = monthly.as_mut() {
        v["minutes"] = json!(my_minutes(&st, person_id, &MinutesScope::Month(&month)).await?);
    }
    if let Some(v) = yearly.as_mut() {
        v["minutes"] = json!(my_minutes(&st, person_id, &MinutesScope::Year(&year)).await?);
    }
    total["minutes"] = json!(my_minutes(&st, person_id, &MinutesScope::Total).await?);

    let source = sid(&st);
    // 团内名次（按累计）。口径与 daily 榜单对齐：离队的不占名次，
    // 否则粉丝页显示的名次会和榜单差几个人，对不上。
    // P1-7：只聚合可信行（wave_reliable=1）。
    let rank_sql = format!(
        "SELECT person_id FROM (SELECT p.id AS person_id, SUM(d.wave) AS tw \
         FROM daily_metric d JOIN person p ON p.id = d.person_id \
         WHERE p.deleted_at IS NULL AND p.gender = '{team}' AND p.status <> 'left' \
           AND d.wave_reliable = 1 \
         GROUP BY p.id ORDER BY tw DESC) t"
    );
    let size_sql = format!(
        "SELECT COUNT(*) AS n FROM person \
         WHERE deleted_at IS NULL AND gender = '{team}' AND status <> 'left'"
    );
    let ranks = mysql::query(&st.pools, &source, &rank_sql).await?;
    let team_rank = ranks
        .iter()
        .position(|r| id_of_pub(r) == person_id)
        .map(|i| i + 1)
        .unwrap_or(0);
    let team_size = mysql::query(&st.pools, &source, &size_sql)
        .await?
        .into_iter()
        .next()
        .map(|r| row_i64(&r, "n"))
        .unwrap_or(0);

    let name_sql =
        format!("SELECT name FROM person WHERE id = {person_id} AND deleted_at IS NULL LIMIT 1");
    let name = mysql::query(&st.pools, &source, &name_sql)
        .await?
        .into_iter()
        .next()
        .and_then(|r| r.try_get::<String, _>("name").ok())
        .unwrap_or_default();

    /* 数值显隐（与每日报告同一套后台开关）：关「音浪数值」→ wave=null；
    关「直播时长」→ minutes=null。名次/人数不是数值，照常下发。 */
    let mut daily = daily;
    for v in [&mut daily, &mut monthly, &mut yearly]
        .into_iter()
        .flatten()
    {
        if !s.wave {
            v["wave"] = Value::Null;
        }
        if !s.duration {
            v["minutes"] = Value::Null;
        }
    }
    if !s.wave {
        total["wave"] = Value::Null;
    }
    if !s.duration {
        total["minutes"] = Value::Null;
    }

    Ok(Json(json!({
        "published": true,
        "personId": person_id,
        "name": name,
        "date": today,
        "daily": daily,
        "monthly": monthly,
        "yearly": yearly,
        "total": total,
        "teamRank": team_rank,
        "teamSize": team_size,
        // 管理员观看账号（personId=0）：前端据此隐藏「绑定抖音号」表单
        "admin": person_id == 0,
    })))
}

/// GET /api/public/teams/{team}/my/series?token=&days=
/// 本人近 N 个业务日的音浪与时长（按日期正序）。只返回这一个人的序列，
/// 不下发密码，也不下发团里任何别人的数据。
pub async fn my_series(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<MyQuery>,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    let token = q.token.unwrap_or_default();
    if token.is_empty() {
        return Err(anyhow::anyhow!("缺少验证 token，请先验证姓名和密码").into());
    }
    let Some((person_id, my_team, _)) = my_token_take(&token) else {
        return Err(anyhow::anyhow!("验证已过期，请重新验证").into());
    };
    if my_team != team {
        return Err(anyhow::anyhow!("该账号不属于本团").into());
    }
    let days = q.days.unwrap_or(14).clamp(1, 30);
    let dates = crate::repos::get_available_dates(&st, None, false).await?;
    let mut take: Vec<String> = dates.into_iter().take(days as usize).collect();
    take.reverse(); // 折线图要正序：老 → 新
    let empty_dates: Vec<String> = Vec::new();
    if take.is_empty() {
        return Ok(Json(
            json!({ "published": true, "dates": empty_dates, "waves": [], "minutes": [] }),
        ));
    }

    // P1-7：序列只聚合可信行（wave_reliable=1），与展示页曲线/榜单口径一致
    let sql = format!(
        "SELECT DATE_FORMAT(biz_date, '%Y-%m-%d') AS d, \
         CAST(SUM(wave) AS SIGNED) AS wave, CAST(SUM(minutes) AS SIGNED) AS minutes \
         FROM daily_metric WHERE person_id = {person_id} AND wave_reliable = 1 \
         GROUP BY d ORDER BY d DESC LIMIT {days}"
    );
    let rows = mysql::query(&st.pools, &sid(&st), &sql).await?;
    let mut wave_map: HashMap<String, i64> = HashMap::new();
    let mut min_map: HashMap<String, i64> = HashMap::new();
    for r in &rows {
        let d = r.try_get::<String, _>("d").unwrap_or_default();
        wave_map.insert(d.clone(), row_i64(r, "wave"));
        min_map.insert(d, row_i64(r, "minutes"));
    }
    let waves: Vec<i64> = take
        .iter()
        .map(|d| wave_map.get(d).copied().unwrap_or(0))
        .collect();
    let minutes: Vec<i64> = take
        .iter()
        .map(|d| min_map.get(d).copied().unwrap_or(0))
        .collect();
    Ok(Json(json!({
        "published": true,
        "dates": take,
        // 后台关掉「音浪数值」：序列不下发数字，前端不画图
        "waves": if s.wave { json!(waves) } else { Value::Null },
        "minutes": if s.duration { json!(minutes) } else { Value::Null },
        "durationEnabled": s.duration,
    })))
}

/// GET /api/public/teams/{team}/my/who?token= ：轻量身份（展示端登录门启动时用），
/// 只回姓名 + personId，不拉任何业务数据
pub async fn my_who(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<MyQuery>,
) -> ApiResult {
    let token = q.token.unwrap_or_default();
    let Some((person_id, my_team, token_name)) = my_token_take(token.trim()) else {
        return Err(ApiError::auth("登录已过期，请重新登录"));
    };
    if my_team != team {
        return Err(ApiError::auth("该账号不属于本团"));
    }
    // personId=0 有两种：管理员观看账号（is_admin=1，姓名直接来自登录账号），
    // 以及「手机号注册了、还没绑主播」的账号（bound=0，前端要提示去绑定）
    if person_id == 0 {
        if let Some(info) = crate::store::my_account_info(&st, &token_name).await {
            if !info.bound && !info.is_admin {
                return Ok(Json(json!({
                    "ok": true, "personId": 0, "name": info.phone, "team": team,
                    "phone": info.phone, "admin": false, "bound": false, "needBind": true,
                })));
            }
            if info.is_admin {
                return Ok(Json(json!({
                    "ok": true, "personId": 0, "name": token_name, "team": team,
                    "admin": true, "bound": true,
                })));
            }
        }
        return Ok(Json(json!({
            "ok": true, "personId": 0, "name": token_name, "team": team,
            "admin": true, "bound": true,
        })));
    }
    let sql = "SELECT name FROM person WHERE id = ? AND deleted_at IS NULL".to_string();
    let rows =
        mysql::query_with_binds(&st.pools, &sid(&st), &sql, |q| q.bind(person_id as i64)).await?;
    let name = rows
        .first()
        .and_then(|r| r.try_get::<String, _>("name").ok())
        .unwrap_or_default();
    if name.is_empty() {
        return Err(ApiError::auth("账号不存在或已离团，请联系管理员"));
    }
    Ok(Json(
        json!({ "ok": true, "personId": person_id, "name": name, "team": team, "bound": true }),
    ))
}

/* --------------------- 抖音号绑定申请（展示端提交 → 管理端审计） --------------------- */

/// GET /api/public/teams/{team}/my/claim?token= ：我的绑定状态
/// （最近一笔申请 + 已绑定的抖音号列表，让用户知道现在音浪记在哪个号上）
pub async fn my_claim_get(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<MyQuery>,
) -> ApiResult {
    let token = q.token.unwrap_or_default();
    let Some((person_id, my_team, _)) = my_token_take(token.trim()) else {
        return Err(ApiError::auth("登录已过期，请重新登录"));
    };
    if my_team != team {
        return Err(ApiError::auth("该账号不属于本团"));
    }
    let bound = mysql::query_with_binds(
        &st.pools,
        &sid(&st),
        "SELECT douyin_no, is_primary FROM account \
         WHERE person_id = ? AND status = 'active' ORDER BY is_primary DESC, id",
        |q| q.bind(person_id as i64),
    )
    .await?
    .iter()
    .filter_map(|r| {
        let no = r
            .try_get::<String, _>("douyin_no")
            .ok()
            .filter(|s| !s.is_empty())?;
        let primary = r.try_get::<i8, _>("is_primary").unwrap_or(0) == 1;
        Some(json!({ "douyinNo": no, "primary": primary }))
    })
    .collect::<Vec<_>>();
    Ok(Json(json!({
        "claim": crate::store::anchor_claim_mine(&st, person_id as i64).await,
        "bound": bound,
    })))
}

/// POST /api/public/teams/{team}/my/claim {token, douyinNo, screenshot}
/// screenshot = dataURL（data:image/jpeg;base64,…），服务端解码后存 SQLite 待审计。
pub async fn my_claim_post(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let Some((person_id, my_team, _)) = my_token_take(token.trim()) else {
        return Err(ApiError::auth("登录已过期，请重新登录"));
    };
    if my_team != team {
        return Err(ApiError::auth("该账号不属于本团"));
    }
    if person_id == 0 {
        return Err(anyhow::anyhow!("管理员账号缺少对应的主播档案，不能绑定抖音号").into());
    }
    let douyin_no = body
        .get("douyinNo")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let data_url = body
        .get("screenshot")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (mime, b64) = data_url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(";base64,"))
        .ok_or_else(|| anyhow::anyhow!("截图数据格式不对，请重新选择图片"))?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|_| anyhow::anyhow!("截图数据格式非法（解码失败），请重新选择图片"))?;

    // 提交前先查重：抖音号已被占用就直接拒，省得用户白等审计
    let no = crate::store::validate_douyin_no(douyin_no).map_err(|e| anyhow::anyhow!(e))?;
    let dup = mysql::query_with_binds(
        &st.pools,
        &sid(&st),
        "SELECT person_id FROM account WHERE douyin_no = ? LIMIT 1",
        |q| q.bind(no.clone()),
    )
    .await?;
    if let Some(r) = dup.first() {
        let owner = r
            .try_get::<i64, _>("person_id")
            .map(|v| v as u64)
            .unwrap_or(0);
        if owner == person_id {
            return Err(anyhow::anyhow!("这个抖音号已经绑定在你名下了").into());
        }
        return Err(anyhow::anyhow!("该抖音号已绑定其他主播，如有疑问请联系管理员").into());
    }

    // 报错文案统一走 anyhow::anyhow!(中文) → 400（routes.rs 关键词）
    let name = person_name_of(&st, person_id).await?;
    crate::store::anchor_claim_insert(&st, person_id as i64, &name, &team, &no, &bytes, mime)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    crate::events::emit(
        crate::events::SCOPE_IMPORT,
        format!("绑定申请：{} 提交抖音号 {}（待审计）", name, no),
    );
    Ok(Json(json!({ "ok": true })))
}

/// person 表里找名字（claim 提交与 whoami 共用）
async fn person_name_of(st: &AppState, person_id: u64) -> anyhow::Result<String> {
    let sql = "SELECT name FROM person WHERE id = ? AND deleted_at IS NULL";
    let rows =
        mysql::query_with_binds(&st.pools, &sid(st), sql, |q| q.bind(person_id as i64)).await?;
    Ok(rows
        .first()
        .and_then(|r| r.try_get::<String, _>("name").ok())
        .unwrap_or_default())
}

/* --------------------- 抖音号绑定审计（管理端） --------------------- */

/// GET /api/admin/anchor-claims?status=pending：审计列表（不含截图本体）
pub async fn admin_anchor_claims(
    State(st): State<ArcState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let status = q.get("status").cloned().unwrap_or_default();
    Ok(Json(
        json!({ "items": crate::store::anchor_claim_list(&st, &status, 200).await }),
    ))
}

/// GET /api/admin/anchor-claims/{id}/screenshot：截图本体
/// （<img> 无法带 Authorization 头，走 ?access_token=，与 auth.rs 的放宽规则一致）
pub async fn admin_anchor_claim_screenshot(
    State(st): State<ArcState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Response {
    match crate::store::anchor_claim_image(&st, id).await {
        Some((bytes, mime)) => ([(axum::http::header::CONTENT_TYPE, mime)], bytes).into_response(),
        None => (axum::http::StatusCode::NOT_FOUND, "没有这张截图").into_response(),
    }
}

/// POST /api/admin/anchor-claims/{id}/decide {approve, reason?}
/// 通过 → 写 MySQL account（抖音号精确命中是导入匹配第一优先级，此后音浪自动落到这个人）；
/// 驳回 → 记原因，用户可在展示端重新提交。
pub async fn admin_anchor_claim_decide(
    State(st): State<ArcState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let approve = body
        .get("approve")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let reason = body
        .get("reason")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if !approve && reason.is_empty() {
        return Err(anyhow::anyhow!("驳回要填写原因，请输入驳回原因").into());
    }

    // 1. 先占住裁决权（pending → approved/rejected 原子翻转，防双击重复绑定）
    let Some((person_id, no, person_name)) =
        crate::store::anchor_claim_decide(&st, id, approve, &reason, "admin").await
    else {
        return Err(anyhow::anyhow!("这笔申请不存在或已经处理过了").into());
    };

    if !approve {
        crate::events::emit(
            crate::events::SCOPE_IMPORT,
            format!("绑定驳回：{} 的抖音号 {no}（{reason}）", person_name),
        );
        return Ok(Json(json!({ "ok": true, "status": "rejected" })));
    }

    // 2. 通过：写 account。抖音号占用检查 + 主号判定（此人还没有账号 → 主号）
    let writer = st
        .writer
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("写库不可用：douyin_writer 未配置"))?;
    let dup = sqlx::query("SELECT person_id FROM account WHERE douyin_no = ? LIMIT 1")
        .bind(&no)
        .fetch_optional(writer)
        .await?;
    if let Some(r) = dup {
        let owner = r
            .try_get::<i64, _>("person_id")
            .map(|v| v as u64)
            .unwrap_or(0);
        // 回滚裁决，让管理员能改判或处理冲突
        crate::store::anchor_claim_revert(&st, id).await;
        return Err(match owner == person_id as u64 {
            true => anyhow::anyhow!("该抖音号已存在绑定记录（就在该主播名下）"),
            false => anyhow::anyhow!("该抖音号已存在绑定记录（绑在其他主播名下）"),
        }
        .into());
    }
    let cnt = sqlx::query("SELECT COUNT(*) AS n FROM account WHERE person_id = ?")
        .bind(person_id)
        .fetch_one(writer)
        .await?
        .try_get::<i64, _>("n")
        .unwrap_or(0);
    let ins = sqlx::query(
        "INSERT INTO account (person_id, anchor_id, douyin_no, anchor_name, is_primary) \
         VALUES (?,?,?,?,?)",
    )
    .bind(person_id)
    .bind(&no)
    .bind(&no)
    .bind(&person_name)
    .bind(if cnt == 0 { 1i8 } else { 0i8 })
    .execute(writer)
    .await;
    if let Err(e) = ins {
        // account 写失败必须回滚裁决，否则申请卡在「已通过」却没有账号
        crate::store::anchor_claim_revert(&st, id).await;
        let msg = e.to_string();
        return Err(if msg.contains("Duplicate entry") {
            anyhow::anyhow!("抖音号或采集 ID 已存在（唯一键冲突），请检查是否重复绑定").into()
        } else {
            anyhow::anyhow!("写入账号失败：{msg}").into()
        });
    }

    // 审核通过 = 这个号正式绑到该主播名下：暂存区里这个号的导入行立刻补账，
    // 不用等下一次导入（用户口径：建档/绑号后才显示，绑定的这一刻就算「添加上主播」）
    let absorbed = match crate::admin::absorb_pending(&st, person_id as u64).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(person_id, error = %e, "绑定通过后暂存补账失败（不影响绑定）");
            0
        }
    };

    crate::events::emit(
        crate::events::SCOPE_ROSTER,
        format!(
            "绑定通过：{} ← 抖音号 {no}，音浪将从下次导入起落到 TA 名下{}",
            person_name,
            if absorbed > 0 {
                format!("（已补账 {absorbed} 行暂存数据）")
            } else {
                String::new()
            }
        ),
    );
    Ok(Json(json!({
        "ok": true,
        "status": "approved",
        "personId": person_id,
        "douyinNo": no,
        "primary": cnt == 0,
        "absorbed": absorbed,
    })))
}

fn id_of_pub(r: &sqlx::mysql::MySqlRow) -> u64 {
    if let Ok(v) = r.try_get::<u64, _>("person_id") {
        return v;
    }
    if let Ok(v) = r.try_get::<i64, _>("person_id") {
        return v as u64;
    }
    0
}

/* ------------------ 我的页面 · 微信推送（绑定 + 追分目标 + 每日报告） ------------------ */

/// 登录态 → (personId, 团别, 账号姓名)。推送相关接口全部走这一处校验。
fn my_ident(team: &str, token: &str) -> Result<(u64, String, String), ApiError> {
    let token = token.trim();
    let Some((pid, my_team, name)) = my_token_take(token) else {
        return Err(ApiError::auth("登录已过期，请重新登录"));
    };
    if my_team != team {
        return Err(ApiError::auth("该账号不属于本团"));
    }
    Ok((pid, my_team, name))
}

/// GET /api/public/teams/{team}/my/push?token=：推送设置全量状态
/// （绑定状态 + 两个开关 + 追分目标 + 是否已配机器人二维码）
pub async fn my_push(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<MyQuery>,
) -> ApiResult {
    let (person_id, _my_team, name) = my_ident(&team, q.token.as_deref().unwrap_or(""))?;
    let mut state = crate::store::push_state(&st, &name).await;
    let targets = crate::store::targets_list(&st, &name).await;
    let has_qr = crate::store::config_get(&st, "wechat_add_qr")
        .await
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if let Some(obj) = state.as_object_mut() {
        obj.insert("targets".into(), json!(targets));
        obj.insert("maxTargets".into(), json!(crate::push::MAX_TARGETS));
        obj.insert("hasQr".into(), json!(has_qr));
        obj.insert("personId".into(), json!(person_id));
        // 管理员观看账号（personId=0）没有自己的音浪，追分对比没有意义
        obj.insert("admin".into(), json!(person_id == 0));
        obj.insert("channelConfigured".into(), json!(channel_ready(&st)));
        // 手机号绑定（2026-10-10）：登记过手机号就走「发手机号」路径；
        // 只回显脱敏手机号（138****8000），不全量下发，避免页面被别人瞄到就能拿走号。
        let phone = crate::store::my_account_phone(&st, &name).await;
        obj.insert("hasPhone".into(), json!(!phone.is_empty()));
        obj.insert("phoneMasked".into(), json!(mask_phone(&phone)));
    }
    Ok(Json(state))
}

/// 手机号脱敏：138****8000（中间四位打码）。空串原样返回。
fn mask_phone(p: &str) -> String {
    let cs: Vec<char> = p.chars().collect();
    match cs.len() {
        n if n >= 11 => format!(
            "{}****{}",
            cs[..3].iter().collect::<String>(),
            cs[n - 4..].iter().collect::<String>()
        ),
        n if n >= 7 => format!(
            "{}****{}",
            cs[..2].iter().collect::<String>(),
            cs[n - 2..].iter().collect::<String>()
        ),
        n if n >= 3 => format!("{}****{}", cs[0], cs[n - 1]),
        _ => String::new(),
    }
}

/// 微信渠道是否可用（进程里已有微信实例）。没配时前端提示「暂未开放」。
fn channel_ready(st: &AppState) -> bool {
    st.bots.get().map(|m| m.wechat.is_some()).unwrap_or(false)
}

/// POST /api/public/teams/{team}/my/push/code {token}：生成/刷新绑定码（30 分钟有效）
pub async fn my_push_code(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (person_id, _t, name) = my_ident(&team, token)?;
    let code = crate::push::new_bind_code();
    crate::store::push_set_code(&st, &name, &team, person_id, &code)
        .await
        .map_err(|e| anyhow::anyhow!("生成绑定码失败：{e}"))?;
    let state = crate::store::push_state(&st, &name).await;
    Ok(Json(json!({
        "ok": true,
        "code": state.get("code").cloned().unwrap_or(Value::Null),
        "codeExpires": state.get("codeExpires").cloned().unwrap_or(Value::Null),
    })))
}

/// POST /api/public/teams/{team}/my/push/settings {token, notifyTarget, dailyReport}
pub async fn my_push_settings(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (_pid, _t, name) = my_ident(&team, token)?;
    let flag = |k: &str| body.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    crate::store::push_set_flags(&st, &name, flag("notifyTarget"), flag("dailyReport")).await;
    Ok(Json(json!({ "ok": true })))
}

/// POST /api/public/teams/{team}/my/push/unbind {token}
pub async fn my_push_unbind(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (_pid, _t, name) = my_ident(&team, token)?;
    crate::store::push_unbind(&st, &name).await;
    Ok(Json(json!({ "ok": true })))
}

/// POST /api/public/teams/{team}/my/push/targets {token, kind, personId?|valueWan?}
/// kind = person（追人）/ value（追数值，前端填万，后端换算成音浪）
pub async fn my_push_target_add(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (person_id, _t, name) = my_ident(&team, token)?;
    if person_id == 0 {
        return Err(anyhow::anyhow!("管理员观看账号没有音浪数据，不能设置追分目标").into());
    }
    if crate::store::target_count(&st, &name).await >= crate::push::MAX_TARGETS {
        return Err(anyhow::anyhow!(format!(
            "最多只能追 {} 个人，请先删掉一个",
            crate::push::MAX_TARGETS
        ))
        .into());
    }
    let kind = body
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("person");
    match kind {
        "value" => {
            let wan = body
                .get("valueWan")
                .and_then(|v| v.as_f64())
                .filter(|v| *v > 0.0)
                .ok_or_else(|| anyhow::anyhow!("请输入本月目标（万），要大于 0"))?;
            let raw = (wan * 10_000.0).round() as i64;
            crate::store::target_add(&st, &name, &team, "value", 0, "", raw)
                .await
                .map_err(|e| anyhow::anyhow!("保存失败：{e}"))?;
        }
        // 追名次（2026-10-10）：target_value 存目标名次，名字列存展示文案
        "rank" => {
            let rank = body
                .get("targetRank")
                .and_then(|v| v.as_i64())
                .filter(|v| (1..=999).contains(v))
                .ok_or_else(|| anyhow::anyhow!("请输入要追的名次（1~999）"))?;
            let dup = crate::store::targets_for_push(&st, &name)
                .await
                .into_iter()
                .any(|t| t.kind == "rank" && t.target_value == rank);
            if dup {
                return Err(anyhow::anyhow!("已经在追这个名次了").into());
            }
            crate::store::target_add(&st, &name, &team, "rank", 0, &format!("第 {rank} 名"), rank)
                .await
                .map_err(|e| anyhow::anyhow!("保存失败：{e}"))?;
        }
        _ => {
            let tid = body
                .get("personId")
                .and_then(|v| v.as_u64())
                .filter(|v| *v > 0)
                .ok_or_else(|| anyhow::anyhow!("请选择要追的主播"))?;
            if tid == person_id {
                return Err(anyhow::anyhow!("不能追自己").into());
            }
            // 只允许追本团在册主播（搜索接口也只下发本团名单，这里是服务端兜底）
            let anchor = crate::store::team_anchor_search(&st, &sid(&st), &team, "", 1000)
                .await
                .into_iter()
                .find(|a| a.get("personId").and_then(|v| v.as_u64()) == Some(tid));
            let Some(anchor) = anchor else {
                return Err(anyhow::anyhow!("没有找到这个主播（只能追本团在册主播）").into());
            };
            let tname = anchor
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            // 同一个人不重复追
            let dup = crate::store::targets_for_push(&st, &name)
                .await
                .into_iter()
                .any(|t| t.kind == "person" && t.target_person_id == tid);
            if dup {
                return Err(anyhow::anyhow!("已经在追这个主播了").into());
            }
            crate::store::target_add(&st, &name, &team, "person", tid, &tname, 0)
                .await
                .map_err(|e| anyhow::anyhow!("保存失败：{e}"))?;
        }
    }
    Ok(Json(
        json!({ "targets": crate::store::targets_list(&st, &name).await }),
    ))
}

/// POST /api/public/teams/{team}/my/push/targets/delete {token, id}
pub async fn my_push_target_del(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (_pid, _t, name) = my_ident(&team, token)?;
    let id = body
        .get("id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("缺少目标 id"))?;
    crate::store::target_delete(&st, &name, id).await;
    Ok(Json(
        json!({ "targets": crate::store::targets_list(&st, &name).await }),
    ))
}

/// GET /api/public/teams/{team}/my/push/search?token=&q=：本团主播搜索（只回 id + 名字）
pub async fn my_push_search(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<PushSearchQuery>,
) -> ApiResult {
    let (person_id, _t, _name) = my_ident(&team, q.token.as_deref().unwrap_or(""))?;
    let key = q.q.unwrap_or_default();
    let key = key.trim();
    if key.is_empty() {
        return Ok(Json(json!({ "items": [] })));
    }
    let items = crate::store::team_anchor_search(&st, &sid(&st), &team, key, 20)
        .await
        .into_iter()
        .filter(|a| a.get("personId").and_then(|v| v.as_u64()) != Some(person_id))
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub struct PushSearchQuery {
    pub token: Option<String>,
    pub q: Option<String>,
}

/// GET /api/public/teams/{team}/my/push/qr?access_token=
/// 机器人「加好友」二维码图片（管理员在后台「机器人」页上传）。
/// 走独立图片端点是为了不让 base64 撑大设置接口的 JSON。
pub async fn my_push_qr(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(g): Query<GateQuery>,
    headers: HeaderMap,
) -> Response {
    let s = PublicSettings::load(&st).await;
    if gate_team(s.login_required, &team, g.access_token.as_deref(), &headers).is_err() {
        return (axum::http::StatusCode::UNAUTHORIZED, "请先登录").into_response();
    }
    let Some(data_url) = crate::store::config_get(&st, "wechat_add_qr").await else {
        return (axum::http::StatusCode::NOT_FOUND, "没有配置机器人二维码").into_response();
    };
    match decode_data_url(&data_url) {
        // P2-17：MIME 白名单 —— 上传入口（bot/mod.rs）原样透传 dataURL 的 MIME，
        // 曾经能存进 data:text/html 造成同源 XSS。这里只放行图片类型，其余一律 404。
        Some((mime, bytes))
            if matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp") =>
        {
            ([(axum::http::header::CONTENT_TYPE, mime)], bytes).into_response()
        }
        _ => (axum::http::StatusCode::NOT_FOUND, "机器人二维码格式不对").into_response(),
    }
}

/// dataURL → (mime, bytes)。P2-17：这里保留原签名（bot/mod.rs 上传校验在用它），
/// 图片 MIME 白名单只在下发端（my_push_qr）强制执行。
pub fn decode_data_url(data_url: &str) -> Option<(String, Vec<u8>)> {
    let (mime, b64) = data_url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(";base64,"))?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()?;
    Some((mime.to_string(), bytes))
}

/* --------------------------- 公开展示页（/st/m、/st/f） --------------------------- */

const ST_PAGE: &str = include_str!("st_page.html");

/// GET / 根路径：展示页（男女共用一个网址，进页由前端弹团别选择卡）
pub async fn st_root() -> Response {
    st_page_for("male") // 初始默认值，前端 boot 会按选择/记忆覆盖
}

/// GET /1 男团展示页（好记的单字符路由）
pub async fn st_m() -> Response {
    st_page_for("male")
}

/// GET /0 女团展示页
pub async fn st_0() -> Response {
    st_page_for("female")
}

/// GET /st/{team}：别名路由（m / f）
pub async fn st_page(axum::extract::Path(team): axum::extract::Path<String>) -> Response {
    let team = match team.as_str() {
        "f" | "0" => "female",
        _ => "male",
    };
    st_page_for(team)
}

/// GET /st/{team}/{*rest}：展示端子路径路由（/report、/group/junior、/lineage…）。
/// 单页应用：子路径一律渲染同一个 HTML，视图切换由前端 history API 完成，
/// 刷新/分享子路径链接都不会 404。
pub async fn st_page_sub(
    axum::extract::Path((team, _rest)): axum::extract::Path<(String, String)>,
) -> Response {
    let team = match team.as_str() {
        "f" | "0" => "female",
        _ => "male",
    };
    st_page_for(team)
}

fn st_page_for(team: &str) -> Response {
    let html = ST_PAGE.replace("{{TEAM}}", team);
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

#[cfg(test)]
mod register_route_tests {
    use super::*;

    /// 注册页的团别白名单：male/female（兼容 m/f），其它一律空串（老链接/异常入参）
    #[test]
    fn team_whitelist_for_register() {
        assert_eq!(normalize_team("male"), "male");
        assert_eq!(normalize_team("M"), "male");
        assert_eq!(normalize_team("female"), "female");
        assert_eq!(normalize_team(" f "), "female");
        assert_eq!(normalize_team(""), "");
        assert_eq!(normalize_team("男团"), "");
    }
}
