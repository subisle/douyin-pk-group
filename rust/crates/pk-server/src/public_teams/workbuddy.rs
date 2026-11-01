//! 「我的」页 WorkBuddy 账号绑定（2026-11-01 恢复）。
//!
//! 用户只看到「获取授权链接 → 在 WorkBuddy 登录 → 自动检测成功」。面板密码始终留在
//! pk-server 环境变量里，浏览器只拿到 WorkBuddy 返回的 `authUrl` 和随机 `state`。

use super::*;
use axum::extract::ConnectInfo;
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

fn valid_state(state: &str) -> bool {
    let state = state.trim();
    !state.is_empty() && state.len() <= 128
}

#[derive(Debug, Deserialize)]
pub struct WbStartBody {
    /// cn = 国内版（默认）；intl = 国际版。
    #[serde(default)]
    pub realm: String,
}

#[derive(Debug, Deserialize)]
pub struct WbStateBody {
    #[serde(default)]
    pub state: String,
}

/// POST /api/public/my/workbuddy/oauth/start
///
/// 必须已登录展示端；返回 `{ok,state,authUrl,realm}`。
pub async fn my_workbuddy_oauth_start(
    State(st): State<ArcState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<WbStartBody>,
) -> ApiResult {
    let _ = my_token_required_with_restore(&st, &headers).await?;
    let ip = super::admin::client_ip_for_limit(addr, &headers);
    if !start_allow(&ip) {
        return Err(ApiError::with_status(
            StatusCode::TOO_MANY_REQUESTS,
            "操作太频繁，请稍后再试",
        ));
    }
    let wb = crate::wbgateway::global().ok_or_else(not_configured)?;
    let realm = body.realm.trim();
    let (state, auth_url, realm) = wb.login_start(realm).await.map_err(|e| {
        tracing::warn!(error = %e, "WorkBuddy 授权链接申请失败");
        upstream_error()
    })?;
    let api_base = wb.public_api_base();
    Ok(Json(json!({
        "ok": true,
        "state": state,
        "authUrl": auth_url,
        "realm": realm,
        "apiBase": api_base,
    })))
}

/// POST /api/public/my/workbuddy/oauth/poll
///
/// 必须已登录展示端；原样转发网关状态，`status=ok` 即账号已加入反代池。
pub async fn my_workbuddy_oauth_poll(
    State(st): State<ArcState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<WbStateBody>,
) -> ApiResult {
    let _ = my_token_required_with_restore(&st, &headers).await?;
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
    let wb = crate::wbgateway::global().ok_or_else(not_configured)?;
    let v = wb.login_poll(state).await.map_err(|e| {
        tracing::warn!(error = %e, "WorkBuddy 授权状态查询失败");
        upstream_error()
    })?;
    let status = v.get("status").and_then(Value::as_str).unwrap_or("unknown");
    let message = v.get("message").and_then(Value::as_str).unwrap_or_default();
    let nickname = v
        .pointer("/account/nickname")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    Ok(Json(json!({
        "ok": true,
        "status": status,
        "message": message,
        "nickname": nickname,
        "bound": status == "ok",
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
    let _ = my_token_required_with_restore(&st, &headers).await?;
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
    if let Some(wb) = crate::wbgateway::global() {
        wb.login_cancel(state).await;
    }
    Ok(Json(json!({ "ok": true })))
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
}
