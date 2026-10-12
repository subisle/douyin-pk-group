//! WorkBuddy 网关（wb-proxy 用量看板）客户端。
//!
//! 用途：展示端「注册」时，顺手把注册人本人的 WorkBuddy 账号加进网关账号池 ——
//! 面板 `?tab=gateway` 上那个「+ 添加账号 (OAuth)」按钮走的就是这套接口，
//! 用户在授权页用**手机号 + 短信验证码**登录，登录完成后账号自动入池。
//!
//! 实测契约（2026-10-10，wb-proxy/1.6.17）：
//!   POST /panel/login {password}                  → {ok, token}          （免 token）
//!   POST /accounts/login/start {platform, realm}  → {state, authUrl}     （要 X-Panel-Token）
//!   GET  /accounts/login/poll?state=<state>       → {status, message, account?}
//!   POST /accounts/login/cancel {state}           → {cancelled}
//!   status: pending（等授权）| ok（已入池）| error | expired | unknown
//!
//! 配置（.env）：
//!   WORKBUDDY_PANEL_URL       面板地址，默认 <http://64.188.8.149:8788>
//!                             （2026-10-13 安全修复：默认值仅为向后兼容，
//!                              生产必须配成 HTTPS —— 面板密码要过这条链路）
//!   WORKBUDDY_PANEL_PASSWORD  面板密码；**没配 = 注册页不显示 OAuth 步骤**，其余注册照常
//!
//! 安全：面板密码只留在服务端（环境变量 + 内存里的 token），`/panel/*` 一律不转发给浏览器；
//! 展示端只拿到 state 与 authUrl（authUrl 本来就是给用户浏览器打开的授权页）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

/// 默认面板地址（用户 2026-10-11 给的口径）。
///
/// 2026-10-13 安全修复：面板密码经这条链路明文 POST（/panel/login），硬编码公网
/// IP + http 意味着密码全程明文过公网。保留默认值只为向后兼容（没配环境变量的
/// 旧部署不炸），**生产必须通过 WORKBUDDY_PANEL_URL 配成 HTTPS 地址**；
/// 该地址也可按 config.rs::env 的口径用环境变量覆盖（空串视为未配置）。
pub const DEFAULT_PANEL_URL: &str = "http://64.188.8.149:8788";

/// 读环境变量口径（与 config.rs 的 env()/env_or() 一致：未配置或空串都算没配）。
/// 2026-10-13 安全修复：面板地址从硬编码改为可被环境覆盖，配合生产切 HTTPS。
fn panel_url_or_default() -> String {
    std::env::var("WORKBUDDY_PANEL_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_PANEL_URL.to_string())
}

const UA: &str = "pk-server/1.0 (+workbuddy-register)";

pub struct WbGateway {
    base: String,
    password: String,
    /// 面板登录 token（X-Panel-Token）。进程内缓存，401 时清掉重登。
    token: Mutex<Option<String>>,
    http: reqwest::Client,
}

impl WbGateway {
    /// 读环境变量建客户端；没配密码 → None（= 注册不带 OAuth 步骤，功能降级而不是报错）
    pub fn from_env() -> Option<Arc<Self>> {
        let password = std::env::var("WORKBUDDY_PANEL_PASSWORD")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())?;
        // 2026-10-13 安全修复：改走 panel_url_or_default()（读 WORKBUDDY_PANEL_URL，
        // 与 config.rs::env 同口径），不再依赖这里的内联读取；默认值仅向后兼容，
        // 生产应配 HTTPS 地址覆盖。
        let base = panel_url_or_default();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(8))
            .user_agent(UA)
            .build()
            .ok()?;
        Some(Arc::new(Self {
            base: base.trim_end_matches('/').to_string(),
            password,
            token: Mutex::new(None),
            http,
        }))
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// 发起「添加账号 OAuth」：返回 (state, authUrl, realm)
    pub async fn login_start(&self, realm: &str) -> Result<(String, String, String), String> {
        let realm = normalize_realm(realm);
        let v = self
            .authed_post(
                "/accounts/login/start",
                json!({ "platform": "CLI", "realm": realm }),
            )
            .await?;
        let state = str_field(&v, "state").ok_or_else(|| "网关没有返回 state".to_string())?;
        let auth_url = str_field(&v, "authUrl")
            .or_else(|| str_field(&v, "auth_url"))
            .ok_or_else(|| "网关没有返回授权链接".to_string())?;
        Ok((state, auth_url, realm.to_string()))
    }

    /// 轮询授权结果（原样转发 status/message，前端只认 ok/pending/…）
    pub async fn login_poll(&self, state: &str) -> Result<Value, String> {
        let path = format!("/accounts/login/poll?state={}", urlencode(state.trim()));
        self.authed_get(&path).await
    }

    /// 取消一笔还没完成的授权（关弹窗/换链接时清理，避免面板里堆状态）
    pub async fn login_cancel(&self, state: &str) {
        let _ = self
            .authed_post("/accounts/login/cancel", json!({ "state": state.trim() }))
            .await;
    }

    /* ------------------------------ 内部 ------------------------------ */

    /// 面板登录换 token（缓存住）
    async fn panel_token(&self) -> Result<String, String> {
        if let Some(t) = self
            .token
            .lock()
            /* 审计 2026-10-11：某次 panic 毒化锁后，expect 会让之后每个请求连锁 panic，
            这里取回中毒锁里的值继续用（token 本身就是可重建的缓存） */
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .filter(|t| !t.is_empty())
        {
            return Ok(t);
        }
        let (status, v) = self
            .send(
                reqwest::Method::POST,
                "/panel/login",
                None,
                Some(json!({ "password": self.password })),
            )
            .await?;
        if !status.is_success() {
            return Err(format!(
                "网关面板登录失败（HTTP {status}）：{}",
                err_message(&v)
            ));
        }
        let token =
            str_field(&v, "token").ok_or_else(|| "网关面板登录没有返回 token".to_string())?;
        *self.token.lock().unwrap_or_else(|e| e.into_inner()) = Some(token.clone());
        Ok(token)
    }

    /// 带 token 的 POST；401（token 过期/没配）时清缓存重登一次
    async fn authed_post(&self, path: &str, body: Value) -> Result<Value, String> {
        let token = self.panel_token().await?;
        let (status, v) = self
            .send(
                reqwest::Method::POST,
                path,
                Some(&token),
                Some(body.clone()),
            )
            .await?;
        if status.as_u16() == 401 {
            self.forget_token();
            let token = self.panel_token().await?;
            let (status, v) = self
                .send(reqwest::Method::POST, path, Some(&token), Some(body))
                .await?;
            return finish(status, v);
        }
        finish(status, v)
    }

    async fn authed_get(&self, path: &str) -> Result<Value, String> {
        let token = self.panel_token().await?;
        let (status, v) = self
            .send(reqwest::Method::GET, path, Some(&token), None)
            .await?;
        if status.as_u16() == 401 {
            self.forget_token();
            let token = self.panel_token().await?;
            let (status, v) = self
                .send(reqwest::Method::GET, path, Some(&token), None)
                .await?;
            return finish(status, v);
        }
        finish(status, v)
    }

    fn forget_token(&self) {
        *self.token.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// 一次 HTTP 往返：返回 (状态码, JSON)。网络层错误直接 Err（文案给用户看）
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Result<(reqwest::StatusCode, Value), String> {
        let url = format!("{}{}", self.base, path);
        let mut req = self.http.request(method, &url);
        if let Some(t) = token {
            req = req.header("X-Panel-Token", t);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("连不上 WorkBuddy 网关（{}）：{e}", self.base))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }));
        Ok((status, v))
    }
}

fn finish(status: reqwest::StatusCode, v: Value) -> Result<Value, String> {
    if status.is_success() {
        Ok(v)
    } else {
        Err(format!("网关返回 HTTP {status}：{}", err_message(&v)))
    }
}

/// 面板错误体：{"error": {"message": "..."}}（也兼容 {"message": ...} / 纯文本）
fn err_message(v: &Value) -> String {
    v.pointer("/error/message")
        .and_then(|m| m.as_str())
        .or_else(|| v.get("message").and_then(|m| m.as_str()))
        .or_else(|| v.get("error").and_then(|m| m.as_str()))
        .or_else(|| v.get("raw").and_then(|m| m.as_str()))
        .unwrap_or("未知错误")
        .chars()
        .take(200)
        .collect()
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|s| s.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// 版本白名单：cn（国内版 copilot.tencent.com）/ intl（国际版 www.workbuddy.ai）
pub fn normalize_realm(v: &str) -> &'static str {
    match v.trim().to_ascii_lowercase().as_str() {
        "intl" | "international" | "国际" | "国际版" => "intl",
        _ => "cn",
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realm_whitelist() {
        assert_eq!(normalize_realm("cn"), "cn");
        assert_eq!(normalize_realm("CN"), "cn");
        assert_eq!(normalize_realm(""), "cn");
        assert_eq!(normalize_realm("intl"), "intl");
        assert_eq!(normalize_realm("国际版"), "intl");
        assert_eq!(normalize_realm("随便填"), "cn");
    }

    #[test]
    fn error_message_shapes() {
        let v = json!({"error": {"message": "panel password required"}});
        assert_eq!(err_message(&v), "panel password required");
        let v = json!({"message": "boom"});
        assert_eq!(err_message(&v), "boom");
        let v = json!({"raw": "<html>500</html>"});
        assert_eq!(err_message(&v), "<html>500</html>");
        let v = json!({});
        assert_eq!(err_message(&v), "未知错误");
    }

    #[test]
    fn urlencode_state() {
        assert_eq!(urlencode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(urlencode("a b/c"), "a%20b%2Fc");
    }
}
