//! 登录后 WorkBuddy 账号绑定 + 管理端核对（2026-11-13）。
//!
//! 用户先用自己的展示端账号登录，再在强制弹窗里直接走国内版；服务端申请授权链接、
//! 展示链接并自动检测授权结果。成功后把 WorkBuddy uid / nickname / realm 落到服务端，
//! 一个展示端账号只保留一次绑定；展示端不再在「我的」页放入口，弹窗不可跳过，
//! 管理端账号详情可核对。登录时还会查面板账号到期状态，临期/过期时允许同账号重新授权。

use super::*;
use axum::extract::{ConnectInfo, Query};
use axum::http::StatusCode;
use std::net::SocketAddr;
use std::sync::OnceLock;

static WB_START_LIMITER: OnceLock<VerifyLimiter> = OnceLock::new();
static WB_POLL_LIMITER: OnceLock<VerifyLimiter> = OnceLock::new();

fn start_allow(ip: &str) -> bool {
    WB_START_LIMITER
        .get_or_init(|| VerifyLimiter::new(5))
        .allow(ip)
}

fn poll_allow(ip: &str) -> bool {
    WB_POLL_LIMITER
        .get_or_init(|| VerifyLimiter::new(120))
        .allow(ip)
}

fn not_configured() -> ApiError {
    ApiError::with_json(
        StatusCode::SERVICE_UNAVAILABLE,
        json!({
            "ok": false,
            "code": "WORKBUDDY_NOT_CONFIGURED",
            "error": "WorkBuddy 绑定暂未开放，请联系管理员"
        }),
    )
}

fn upstream_error() -> ApiError {
    ApiError::with_json(
        StatusCode::BAD_GATEWAY,
        json!({
            "ok": false,
            "code": "WORKBUDDY_UPSTREAM",
            "error": "WorkBuddy 网关暂时不可用，请稍后再试"
        }),
    )
}

fn store_error() -> ApiError {
    ApiError::with_json(
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({
            "ok": false,
            "code": "WORKBUDDY_STORE",
            "error": "绑定状态保存失败，请稍后重试"
        }),
    )
}

fn valid_state(state: &str) -> bool {
    let state = state.trim();
    !state.is_empty() && state.len() <= 128
}

fn realm_label(realm: &str) -> &'static str {
    if realm == "intl" {
        "国际版 (www.workbuddy.ai)"
    } else {
        "国内版 (copilot.tencent.com)"
    }
}

fn realm_host(realm: &str) -> &'static str {
    if realm == "intl" {
        "www.workbuddy.ai"
    } else {
        "copilot.tencent.com"
    }
}

fn auth_url_allowed(url: &str) -> bool {
    let rest = match url.strip_prefix("https://") {
        Some(rest) => rest,
        None => return false,
    };
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    [
        "copilot.tencent.com",
        "codebuddy.cn",
        "workbuddy.cn",
        "www.workbuddy.ai",
        "workbuddy.ai",
        "codebuddy.ai",
    ]
    .iter()
    .any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")))
}

async fn wb_identity(st: &ArcState, headers: &HeaderMap) -> Result<(u64, String), ApiError> {
    let (person_id, _, token_name) = my_token_required_with_restore(st, headers).await?;
    if token_name.trim() == "admin-gate" {
        return Err(ApiError::bad_request("请先登录自己的展示端账号"));
    }
    let info = crate::store::my_account_info(st, &token_name)
        .await
        .ok_or_else(|| ApiError::auth("登录已过期，请重新登录"))?;
    /* 绑定 WorkBuddy 不再要求账号先填手机号：管理员观看账号（如 shuai4444）
    也要能绑定；只保留“已开通展示端账号”这一层归属校验。 */
    if !info.bound {
        return Err(ApiError::bad_request("请先完成展示端账号信息绑定"));
    }
    Ok((person_id, info.name.trim().to_string()))
}

fn public_account(v: &Value) -> Value {
    crate::store::public_binding(v)
}

fn sanitize_account(v: &Value) -> Value {
    json!({
        "uid": v.get("uid").and_then(Value::as_str).unwrap_or_default(),
        "nickname": v.get("nickname").and_then(Value::as_str).unwrap_or_default(),
        "realm": v.get("realm").and_then(Value::as_str).unwrap_or_default(),
        "domain": v.get("domain").and_then(Value::as_str).unwrap_or_default(),
        "platform": v.get("platform").and_then(Value::as_str).unwrap_or_default(),
    })
}

/// 面板账号的到期判断：token 过期、token/积分包 14 天内到期、或账号已从池中消失。
fn expiry_status_at(v: &Value, now: i64) -> Value {
    let expires_at = v
        .get("expiresAt")
        .and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)))
        .unwrap_or(0);
    let token_expired = v
        .get("tokenExpired")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || v.get("needRelogin")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let account_days_left = (expires_at > 0).then(|| (expires_at - now) as f64 / 86_400.0);

    // 积分包口径统一为「未过期且仍有余额」：面板的 `earliest_expiring` 已经是这类
    // 包里最早到期的一个，优先信它。只有这个字段缺失或不可用时才回退遍历 packages；
    // 回退时同样跳过余额为 0、已过期和永久包。真实面板里用尽的旧包仍返回
    // days_left=0.5，曾因此把 28 天有效期误报成 0.5 天。
    fn usable_credit_days(package: &Value) -> Option<f64> {
        if package.get("no_expiry").and_then(Value::as_bool) == Some(true)
            || package.get("is_expired").and_then(Value::as_bool) == Some(true)
        {
            return None;
        }
        if package
            .get("remain")
            .and_then(Value::as_f64)
            .is_some_and(|remain| remain <= 0.0)
        {
            return None;
        }
        package
            .get("days_left")
            .and_then(Value::as_f64)
            .map(|days| days.max(0.0))
    }

    let mut credit_days = v
        .pointer("/credits/earliest_expiring")
        .and_then(usable_credit_days);
    if credit_days.is_none() {
        if let Some(packages) = v.pointer("/credits/packages").and_then(Value::as_array) {
            for package in packages {
                if let Some(days) = usable_credit_days(package) {
                    credit_days = Some(credit_days.map_or(days, |old| old.min(days)));
                }
            }
        }
    }
    let round1 = |days: f64| (days * 10.0).round() / 10.0;
    // 积分包到期不等于账号登录凭证失效；只把 token / 账号 expiresAt 视为 expired。
    let expired = token_expired || (expires_at > 0 && expires_at <= now);
    let needs_login = expired
        || account_days_left.is_some_and(|days| days <= 14.0)
        || credit_days.is_some_and(|days| days <= 14.0);
    let reason = if expired {
        "expired"
    } else if needs_login {
        "expiring"
    } else {
        ""
    };
    let expiry_source = if token_expired {
        "token"
    } else {
        match (account_days_left, credit_days) {
            (Some(account), Some(credit)) if credit < account => "credits",
            (Some(_), _) => "account",
            (None, Some(_)) => "credits",
            (None, None) => "",
        }
    };
    json!({
        "checked": true,
        "found": true,
        "expiresAt": expires_at,
        "expiresIn": v.get("expiresIn").and_then(Value::as_str).unwrap_or_default(),
        "daysLeft": account_days_left.map(round1),
        "earliestExpiringDaysLeft": credit_days.map(round1),
        "expirySource": expiry_source,
        "tokenExpired": token_expired,
        "needRelogin": v.get("needRelogin").and_then(Value::as_bool).unwrap_or(false),
        "expired": expired,
        "needsLogin": needs_login,
        "reason": reason,
        "shouldPrompt": false,
    })
}

fn expiry_status(v: &Value) -> Value {
    expiry_status_at(v, chrono::Utc::now().timestamp())
}

/// `needsLogin` / `reason` 始终描述真实账号状态；`shouldPrompt` 只控制前端本次是否弹窗。
fn with_should_prompt(mut status: Value, should_prompt: bool) -> Value {
    if let Some(object) = status.as_object_mut() {
        object.insert("shouldPrompt".to_string(), Value::Bool(should_prompt));
    }
    status
}

#[derive(Debug, Default, Deserialize)]
pub struct WbStatusQuery {
    /// 授权返回只确认本地绑定结果，跳过面板到期检查，避免额外一次上游等待。
    #[serde(default)]
    pub quick: bool,
}

#[derive(Debug, Deserialize)]
pub struct WbStartBody {
    /// cn = 国内版（默认）；intl = 国际版。
    #[serde(default)]
    pub realm: String,
    /// 已绑定账号临期 / 过期后的重新授权。
    #[serde(default)]
    pub reauth: bool,
}

#[derive(Debug, Deserialize)]
pub struct WbStateBody {
    #[serde(default)]
    pub state: String,
}

/// GET /api/public/my/workbuddy/status
///
/// 登录后 / 刷新时由服务端判断是否已经绑定，避免浏览器本地缓存冒充绑定状态。
/// 本接口只读：`shouldPrompt` 表示 7 天窗口是否开放，真正消费发生在 oauth/start。
pub async fn my_workbuddy_status(
    State(st): State<ArcState>,
    headers: HeaderMap,
    Query(query): Query<WbStatusQuery>,
) -> ApiResult {
    let (_, account_name) = wb_identity(&st, &headers).await?;
    let binding = crate::store::binding_for(&st, &account_name).await;
    let account = binding.as_ref().map(public_account);
    let mut account_status = Value::Null;
    if let Some(binding) = binding.as_ref().filter(|_| !query.quick) {
        let uid = binding
            .get("uid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let realm = binding
            .get("realm")
            .and_then(Value::as_str)
            .unwrap_or("cn")
            .to_string();
        if let Some(wb) = crate::wbgateway::global() {
            match wb.account_view(&uid, &realm).await {
                Ok(Some(view)) => account_status = expiry_status(&view),
                Ok(None) => {
                    /* 本地有绑定、面板池里找不到：视为需要重新登录。 */
                    account_status = json!({
                        "checked": true,
                        "found": false,
                        "expired": true,
                        "needsLogin": true,
                        "reason": "missing",
                        "shouldPrompt": false,
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, uid = %uid, "WorkBuddy 账号到期状态查询失败");
                    account_status = json!({ "checked": false, "shouldPrompt": false });
                }
            }
        }
    }
    let needs_login = account_status
        .get("needsLogin")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let should_prompt = if needs_login {
        crate::store::reauth_prompt_window_open(&st, &account_name)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    error = %e,
                    account = %account_name,
                    "WorkBuddy 重新登录提示窗口读取失败，本次不弹窗"
                );
                false
            })
    } else {
        false
    };
    account_status = with_should_prompt(account_status, should_prompt);
    Ok(Json(json!({
        "ok": true,
        "enabled": crate::wbgateway::global().is_some(),
        "bound": account.is_some(),
        "account": account,
        "accountStatus": account_status,
    })))
}

/// POST /api/public/my/workbuddy/oauth/start
///
/// 必须已登录展示端；返回网关下发的 `{ok,state,authUrl,realm}`。
/// 已绑定账号以 reauth=true 成功拿到授权链接后，才原子消费 7 天提示窗口。
pub async fn my_workbuddy_oauth_start(
    State(st): State<ArcState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<WbStartBody>,
) -> ApiResult {
    let (person_id, account_name) = wb_identity(&st, &headers).await?;
    let ip = super::admin::client_ip_for_limit(addr, &headers);
    if !start_allow(&ip) {
        return Err(ApiError::with_status(
            StatusCode::TOO_MANY_REQUESTS,
            "操作太频繁，请稍后再试",
        ));
    }
    let realm = crate::wbgateway::normalize_realm(body.realm.trim());
    let mut prompt_binding = None;
    if let Some(account) = crate::store::binding_for(&st, &account_name).await {
        if !body.reauth {
            return Err(ApiError::with_json(
                StatusCode::CONFLICT,
                json!({
                    "ok": false,
                    "code": "WORKBUDDY_ALREADY_BOUND",
                    "error": "该展示端账号已绑定 WorkBuddy",
                    "account": public_account(&account),
                }),
            ));
        }
        prompt_binding = Some(account);
        tracing::info!(account = %account_name, realm, "WorkBuddy 账号临期/过期，允许重新授权");
    }
    let wb = crate::wbgateway::global().ok_or_else(not_configured)?;
    let (state, auth_url, realm) = wb.login_start(realm).await.map_err(|e| {
        tracing::warn!(error = %e, "WorkBuddy 授权链接申请失败");
        upstream_error()
    })?;
    if !auth_url_allowed(&auth_url) {
        tracing::warn!(auth_url = %auth_url, "WorkBuddy 返回了不在白名单内的授权链接");
        wb.login_cancel(&state).await;
        return Err(ApiError::with_json(
            StatusCode::BAD_GATEWAY,
            json!({
                "ok": false,
                "code": "WORKBUDDY_INVALID_AUTH_URL",
                "error": "WorkBuddy 返回的授权链接格式不正确"
            }),
        ));
    }
    crate::store::save_oauth_state(&st, &state, &account_name, person_id, &realm)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "WorkBuddy 授权流水落库失败");
            store_error()
        })?;
    if let Some(expected_binding) = prompt_binding {
        /* 只有成功拿到且校验通过重新登录链接，才消费 7 天提示窗口。
        绑定行锁会挡住已并发完成的重绑；多标签并发时也只有第一个 claim=true。
        无论 claim=false 还是写库失败，都继续返回授权链接，不阻断主动重登。 */
        match crate::store::claim_reauth_prompt_for_binding(&st, &account_name, &expected_binding)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                tracing::info!(account = %account_name, "WorkBuddy 重登提示窗口已消费或绑定已更新，继续授权");
            }
            Err(e) => {
                tracing::warn!(error = %e, account = %account_name, "WorkBuddy 重登提示标记写入失败，继续授权");
            }
        }
    }
    let api_base = wb.public_api_base();
    Ok(Json(json!({
        "ok": true,
        "state": state,
        "authUrl": auth_url,
        "realm": realm,
        "realmLabel": realm_label(&realm),
        "realmHost": realm_host(&realm),
        "apiBase": api_base,
    })))
}

/// POST /api/public/my/workbuddy/oauth/poll
///
/// 必须已登录展示端且 state 属于当前账号；`status=ok` 时持久化绑定并返回账号。
pub async fn my_workbuddy_oauth_poll(
    State(st): State<ArcState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<WbStateBody>,
) -> ApiResult {
    let (person_id, account_name) = wb_identity(&st, &headers).await?;
    let ip = super::admin::client_ip_for_limit(addr, &headers);
    if !poll_allow(&ip) {
        return Err(ApiError::with_status(
            StatusCode::TOO_MANY_REQUESTS,
            "查询太频繁，请稍后再试",
        ));
    }
    let state = body.state.trim();
    if !valid_state(state) {
        return Err(ApiError::bad_request("缺少授权流水号 state"));
    }
    let owner = crate::store::oauth_state_owner(&st, state).await;
    if owner.as_deref() != Some(account_name.as_str()) {
        return Err(ApiError::with_json(
            StatusCode::BAD_REQUEST,
            json!({
                "ok": false,
                "code": "WORKBUDDY_STATE_MISMATCH",
                "error": "授权链接已失效，请重新获取"
            }),
        ));
    }
    let wb = crate::wbgateway::global().ok_or_else(not_configured)?;
    let v = wb.login_poll(state).await.map_err(|e| {
        tracing::warn!(error = %e, "WorkBuddy 授权状态查询失败");
        upstream_error()
    })?;
    let status = v.get("status").and_then(Value::as_str).unwrap_or("unknown");
    let message = v.get("message").and_then(Value::as_str).unwrap_or_default();
    if status != "ok" {
        return Ok(Json(json!({
            "ok": true,
            "status": status,
            "message": message,
            "bound": false,
            "account": null,
        })));
    }
    let upstream = v.get("account").cloned().ok_or_else(|| {
        ApiError::with_json(
            StatusCode::BAD_GATEWAY,
            json!({
                "ok": false,
                "code": "WORKBUDDY_ACCOUNT_MISSING",
                "error": "WorkBuddy 没有返回账号信息，请联系管理员"
            }),
        )
    })?;
    let account = sanitize_account(&upstream);
    let committed = crate::store::commit_binding(&st, &account_name, person_id, &account)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "WorkBuddy 绑定落库失败");
            store_error()
        })?;
    if committed.is_none() {
        return Err(ApiError::with_json(
            StatusCode::CONFLICT,
            json!({
                "ok": false,
                "code": "WORKBUDDY_UID_TAKEN",
                "error": "这个 WorkBuddy 账号已绑定给其他人，请换一个账号登录"
            }),
        ));
    }
    crate::store::clear_oauth_state(&st, state).await;
    Ok(Json(json!({
        "ok": true,
        "status": "ok",
        "message": "绑定成功",
        "bound": true,
        "account": committed.map(|v| public_account(&v)),
    })))
}

/// POST /api/public/my/workbuddy/oauth/cancel
///
/// 用户换链接时清理上一笔未完成授权。取消失败不影响前端重试。
pub async fn my_workbuddy_oauth_cancel(
    State(st): State<ArcState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<WbStateBody>,
) -> ApiResult {
    let (_, account_name) = wb_identity(&st, &headers).await?;
    let ip = super::admin::client_ip_for_limit(addr, &headers);
    if !poll_allow(&ip) {
        return Err(ApiError::with_status(
            StatusCode::TOO_MANY_REQUESTS,
            "操作太频繁，请稍后再试",
        ));
    }
    let state = body.state.trim();
    if !valid_state(state) {
        return Err(ApiError::bad_request("缺少授权流水号 state"));
    }
    let owner = crate::store::oauth_state_owner(&st, state).await;
    if owner.as_deref() != Some(account_name.as_str()) {
        return Ok(Json(json!({ "ok": true })));
    }
    if let Some(wb) = crate::wbgateway::global() {
        wb.login_cancel(state).await;
    }
    crate::store::clear_oauth_state(&st, state).await;
    Ok(Json(json!({ "ok": true })))
}

/// GET /api/admin/my/workbuddy-bindings
///
/// 管理端只读绑定列表，用于核对每个展示端账号对应的 WorkBuddy 账号。
pub async fn admin_workbuddy_bindings(State(st): State<ArcState>) -> ApiResult {
    let mut items: Vec<Value> = crate::store::all_bindings(&st)
        .await
        .into_iter()
        .map(|(name, account)| {
            json!({
                "name": name,
                "bound": true,
                "account": public_account(&account),
            })
        })
        .collect();
    items.sort_by(|a, b| {
        a.get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .cmp(b.get("name").and_then(Value::as_str).unwrap_or_default())
    });
    Ok(Json(json!({ "ok": true, "items": items })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_shape_validation_is_bounded() {
        assert!(valid_state("a".repeat(64).as_str()));
        assert!(valid_state("a".repeat(128).as_str()));
        assert!(!valid_state(""));
        assert!(!valid_state("   "));
        assert!(!valid_state("a".repeat(129).as_str()));
    }

    #[test]
    fn realm_copy_matches_user_flow() {
        assert_eq!(realm_label("cn"), "国内版 (copilot.tencent.com)");
        assert_eq!(realm_label("intl"), "国际版 (www.workbuddy.ai)");
        assert_eq!(realm_host("cn"), "copilot.tencent.com");
        assert_eq!(realm_host("intl"), "www.workbuddy.ai");
    }

    #[test]
    fn auth_url_whitelist_blocks_arbitrary_urls() {
        assert!(auth_url_allowed(
            "https://www.codebuddy.cn/login?platform=CLI&state=abc"
        ));
        assert!(auth_url_allowed("https://www.workbuddy.ai/login?state=abc"));
        assert!(auth_url_allowed("https://copilot.tencent.com/login"));
        assert!(!auth_url_allowed("http://www.codebuddy.cn/login?state=abc"));
        assert!(!auth_url_allowed("https://evil.example.com/login"));
        assert!(!auth_url_allowed(
            "https://www.codebuddy.cn.evil.example.com/login"
        ));
    }

    #[test]
    fn expiry_status_marks_soon_or_expired_accounts() {
        let now = 1_800_000_000i64;
        let far = expiry_status_at(&json!({ "expiresAt": now + 20 * 86_400 }), now);
        assert_eq!(far["needsLogin"], false);
        assert_eq!(far["reason"], "");
        assert_eq!(far["expirySource"], "account");

        let soon = expiry_status_at(&json!({ "expiresAt": now + 14 * 86_400 }), now);
        assert_eq!(soon["needsLogin"], true);
        assert_eq!(soon["reason"], "expiring");
        assert_eq!(soon["expirySource"], "account");

        let expired = expiry_status_at(&json!({ "expiresAt": now - 1 }), now);
        assert_eq!(expired["needsLogin"], true);
        assert_eq!(expired["reason"], "expired");
        assert_eq!(expired["expirySource"], "account");
        assert_eq!(expired["shouldPrompt"], false);

        let credit_soon = expiry_status_at(
            &json!({
                "expiresAt": now + 30 * 86_400,
                "credits": { "packages": [{ "remain": 10.0, "days_left": 7.0 }] }
            }),
            now,
        );
        assert_eq!(credit_soon["needsLogin"], true);
        assert_eq!(credit_soon["earliestExpiringDaysLeft"], 7.0);
        assert_eq!(credit_soon["expirySource"], "credits");
    }

    /// 真实面板回归：账号 28 天到期、有效积分包最早 16.9 天；
    /// 已用尽的旧积分包仍返回 days_left=0.5，不能被当成账号到期依据。
    #[test]
    fn exhausted_credit_packages_do_not_override_account_expiry() {
        let now = 1_800_000_000i64;
        let status = expiry_status_at(
            &json!({
                "expiresAt": now + 28 * 86_400,
                "expiresIn": "28 days",
                "tokenExpired": false,
                "needRelogin": false,
                "credits": {
                    "remain": 954.79,
                    "earliest_expiring": {
                        "remain": 53.57,
                        "days_left": 16.9,
                        "no_expiry": false,
                        "is_expired": false
                    },
                    "packages": [
                        {
                            "remain": 0.0,
                            "days_left": 0.5,
                            "no_expiry": false,
                            "is_expired": false,
                            "status": 3
                        }
                    ]
                }
            }),
            now,
        );
        assert_eq!(status["daysLeft"], 28.0);
        assert_eq!(status["earliestExpiringDaysLeft"], 16.9);
        assert_eq!(status["needsLogin"], false);
        assert_eq!(status["reason"], "");
        assert_eq!(status["expirySource"], "credits");
    }

    #[test]
    fn zero_balance_packages_are_ignored_when_earliest_field_is_missing() {
        let now = 1_800_000_000i64;
        let status = expiry_status_at(
            &json!({
                "expiresAt": now + 28 * 86_400,
                "credits": {
                    "packages": [
                        { "remain": 0.0, "days_left": 0.5, "is_expired": false },
                        { "remain": 10.0, "days_left": 7.0, "is_expired": false }
                    ]
                }
            }),
            now,
        );
        assert_eq!(status["earliestExpiringDaysLeft"], 7.0);
        assert_eq!(status["needsLogin"], true);
        assert_eq!(status["reason"], "expiring");
    }

    #[test]
    fn expired_credit_packages_are_ignored_even_with_balance() {
        let now = 1_800_000_000i64;
        let expired_only = expiry_status_at(
            &json!({
                "expiresAt": now + 28 * 86_400,
                "credits": {
                    "earliest_expiring": {
                        "remain": 10.0,
                        "days_left": 0.5,
                        "no_expiry": false,
                        "is_expired": true
                    },
                    "packages": [{
                        "remain": 10.0,
                        "days_left": 0.5,
                        "no_expiry": false,
                        "is_expired": true
                    }]
                }
            }),
            now,
        );
        assert!(expired_only["earliestExpiringDaysLeft"].is_null());
        assert_eq!(expired_only["needsLogin"], false);
        assert_eq!(expired_only["expirySource"], "account");

        let mixed = expiry_status_at(
            &json!({
                "expiresAt": now + 28 * 86_400,
                "credits": {
                    "earliest_expiring": {
                        "remain": 10.0,
                        "days_left": 0.5,
                        "no_expiry": false,
                        "is_expired": true
                    },
                    "packages": [
                        {
                            "remain": 10.0,
                            "days_left": 0.5,
                            "no_expiry": false,
                            "is_expired": true
                        },
                        {
                            "remain": 8.0,
                            "days_left": 7.0,
                            "no_expiry": false,
                            "is_expired": false
                        }
                    ]
                }
            }),
            now,
        );
        assert_eq!(mixed["earliestExpiringDaysLeft"], 7.0);
        assert_eq!(mixed["needsLogin"], true);
        assert_eq!(mixed["expirySource"], "credits");
    }

    #[test]
    fn should_prompt_does_not_change_account_health() {
        let status = expiry_status_at(
            &json!({ "expiresAt": 1_800_000_000i64 - 1 }),
            1_800_000_000i64,
        );
        let suppressed = with_should_prompt(status, false);
        assert_eq!(suppressed["needsLogin"], true);
        assert_eq!(suppressed["reason"], "expired");
        assert_eq!(suppressed["shouldPrompt"], false);

        let prompted = with_should_prompt(suppressed.clone(), true);
        assert_eq!(prompted["needsLogin"], true);
        assert_eq!(prompted["reason"], "expired");
        assert_eq!(prompted["shouldPrompt"], true);
    }

    #[test]
    fn poll_account_is_a_public_subset() {
        let account = sanitize_account(&json!({
            "uid": "u-1",
            "nickname": "小鹏",
            "realm": "cn",
            "domain": "copilot.tencent.com",
            "platform": "CLI",
            "accessToken": "secret",
            "credentialMask": "mask",
        }));
        assert_eq!(account["uid"], "u-1");
        assert_eq!(account["nickname"], "小鹏");
        assert_eq!(account["realm"], "cn");
        assert!(account.get("accessToken").is_none());
        assert!(account.get("credentialMask").is_none());
    }
}
