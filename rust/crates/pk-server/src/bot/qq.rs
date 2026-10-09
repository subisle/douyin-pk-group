//! QQ 官方机器人渠道（开放平台），协议与 Go 版 / 615 adapter 一致：
//!   - token：POST https://bots.qq.com/app/getAppAccessToken {appId, clientSecret}
//!   - 鉴权头：Authorization: QQBot {access_token} + X-Union-Appid: {appId}
//!   - 事件走 WebSocket gateway；群 @ = GROUP_AT_MESSAGE_CREATE，私聊 = C2C_MESSAGE_CREATE
//!   - 被动回复必须带 msg_id（几分钟窗口），msg_seq 唯一
//!
//! 注意：官方 token 接口经常 HTTP 200 + 业务错误码，不能只看状态码。

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use crate::state::AppState;

use super::{Attachment, BotConfig, BotRouter, Inbound, Outbound};

const DEFAULT_API_BASE: &str = "https://api.sgroup.qq.com";
/// 沙箱环境是独立域名：只推送「沙箱配置」里配的沙箱群 / 沙箱单聊账号的事件。
/// 机器人未上线时必须在沙箱域名下才能收到测试消息（BOT_QQ_SANDBOX=1 切换）。
const SANDBOX_API_BASE: &str = "https://sandbox.api.sgroup.qq.com";
const DEFAULT_TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";
/// 群聊 + 私聊事件 intent 位
const INTENT_GROUP_AND_C2C: u64 = 1 << 25;

const OP_DISPATCH: u8 = 0;
const OP_HEARTBEAT: u8 = 1;
const OP_IDENTIFY: u8 = 2;
const OP_RECONNECT: u8 = 7;
const OP_INVALID_SESS: u8 = 9;
const OP_HELLO: u8 = 10;

#[derive(Debug, Clone)]
struct SessionCtx {
    group_openid: String,
    user_openid: String,
    msg_id: String,
    at: std::time::Instant,
}

#[derive(Default)]
struct Inner {
    access_token: String,
    token_expires_at: Option<std::time::Instant>,
    seq: i64,
    msg_seq: u32,
    sessions: std::collections::HashMap<String, SessionCtx>,
    phase: String,
    note: String,
    msg_count: u64,
    last_text: String,
}

pub struct QqTransport {
    st: Arc<AppState>,
    cfg: BotConfig,
    app_id: String,
    app_secret: String,
    /// 接口 / gateway 基址（正式 or 沙箱）
    api_base: String,
    http: reqwest::Client,
    inner: Arc<Mutex<Inner>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl QqTransport {
    pub fn new(st: Arc<AppState>, cfg: BotConfig, app_id: String, app_secret: String) -> Self {
        let sandbox = std::env::var("BOT_QQ_SANDBOX")
            .map(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes" | "sandbox"))
            .unwrap_or(false);
        let api_base = if sandbox {
            SANDBOX_API_BASE
        } else {
            DEFAULT_API_BASE
        }
        .to_string();
        Self {
            st,
            cfg,
            app_id,
            app_secret,
            api_base,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("reqwest client"),
            inner: Arc::new(Mutex::new(Inner {
                phase: "idle".into(),
                ..Default::default()
            })),
            task: Mutex::new(None),
        }
    }

    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    pub fn configured(&self) -> bool {
        !self.app_id.is_empty()
    }

    pub async fn status(&self) -> Value {
        let inner = self.inner.lock().await;
        json!({
            "channel": "qq",
            "configured": self.configured(),
            "env": if self.api_base.contains("sandbox") { "sandbox" } else { "prod" },
            "apiBase": self.api_base,
            "phase": inner.phase,
            "note": inner.note,
            "connected": inner.phase == "ready",
            "messageCount": inner.msg_count,
            "lastMessage": inner.last_text,
        })
    }

    /* ------------------------------ token ------------------------------ */

    async fn ensure_token(&self) -> anyhow::Result<String> {
        {
            let inner = self.inner.lock().await;
            if let Some(exp) = inner.token_expires_at {
                if std::time::Instant::now() + Duration::from_secs(300) < exp
                    && !inner.access_token.is_empty()
                {
                    return Ok(inner.access_token.clone());
                }
            }
        }
        let resp = self
            .http
            .post(DEFAULT_TOKEN_URL)
            .json(&json!({
                "appId": self.app_id,
                "clientSecret": self.app_secret,
            }))
            .send()
            .await?;
        let body: Value = resp.json().await?;
        let token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if token.is_empty() {
            let msg = body
                .get("message")
                .or_else(|| body.get("msg"))
                .or_else(|| body.get("error"))
                .and_then(|v| v.as_str())
                .unwrap_or("未知错误");
            let code = body.get("code").cloned().unwrap_or(Value::Null);
            anyhow::bail!("获取 QQ Bot token 失败：{msg}（code {code}）");
        }
        let expires_in = body
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .unwrap_or(7200);
        let mut inner = self.inner.lock().await;
        inner.access_token = token.clone();
        inner.token_expires_at = Some(std::time::Instant::now() + Duration::from_secs(expires_in));
        Ok(token)
    }

    async fn auth_headers(&self) -> anyhow::Result<Vec<(String, String)>> {
        let token = self.ensure_token().await?;
        Ok(vec![
            ("Authorization".into(), format!("QQBot {token}")),
            ("X-Union-Appid".into(), self.app_id.clone()),
        ])
    }

    async fn gateway(&self) -> anyhow::Result<String> {
        let headers = self.auth_headers().await?;
        let mut req = self.http.get(format!("{}/gateway", self.api_base));
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let url = body
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if url.is_empty() {
            // 关键：把 QQ 的真实响应打出来。否则只看到「缺少 url」无法定位是
            // token 失效 / 被限流（code 11244 等）/ 代理返回了 HTML 错误页。
            anyhow::bail!(
                "QQ Bot gateway 响应缺少 url（HTTP {status}）：{}",
                truncate(&text, 400)
            );
        }
        Ok(url)
    }

    /* ------------------------------ 启停 ------------------------------ */

    /// 启动连接循环（断线退避重连）。self 是 Arc：循环体克隆自身持有。
    pub async fn start(self: &Arc<Self>) -> anyhow::Result<()> {
        let mut task = self.task.lock().await;
        if task.is_some() {
            return Ok(());
        }
        anyhow::ensure!(
            self.configured(),
            "请先配置 BOT_QQ_APP_ID / BOT_QQ_APP_SECRET"
        );
        {
            let mut inner = self.inner.lock().await;
            inner.phase = "connecting".into();
            inner.note.clear();
        }
        let me = Arc::clone(self);
        *task = Some(tokio::spawn(async move { me.connect_loop().await }));
        Ok(())
    }

    pub async fn stop(&self) {
        if let Some(h) = self.task.lock().await.take() {
            h.abort();
        }
        let mut inner = self.inner.lock().await;
        inner.phase = "stopped".into();
        inner.note.clear();
    }

    async fn set_phase(&self, phase: &str, note: impl Into<String>) {
        let mut inner = self.inner.lock().await;
        inner.phase = phase.to_string();
        inner.note = note.into();
    }

    async fn set_note(&self, note: impl Into<String>) {
        self.inner.lock().await.note = note.into();
    }

    async fn connect_loop(self: Arc<Self>) {
        let mut backoff = Duration::from_secs(3);
        loop {
            match self.run_once().await {
                Ok(()) => return, // 主动退出
                Err(e) => {
                    // 之前是 ready 说明连接本来是健康的，多半是网络抖动 / QQ 要求重连，
                    // 这种情况立刻快速重连；反之（一直没 ready）说明是配置或限流问题，
                    // 让退避继续涨，别几十秒一次刷屏日志。
                    let was_ready = self.inner.lock().await.phase == "ready";
                    self.set_phase("error", e.to_string()).await;
                    tracing::warn!(error = %e, was_ready, "QQ 连接断开，稍后重连");
                    if was_ready {
                        backoff = Duration::from_secs(3);
                    }
                }
            }
            tokio::time::sleep(backoff).await;
            if backoff < Duration::from_secs(180) {
                backoff *= 2;
            }
        }
    }

    /// 一轮连接：gateway → HELLO → identify → 心跳 + 收事件
    async fn run_once(self: &Arc<Self>) -> anyhow::Result<()> {
        let url = self.gateway().await?;
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| anyhow::anyhow!("连接 QQ gateway 失败: {e}"))?;
        let (mut write, mut read) = ws.split();

        // HELLO
        let hello = tokio::time::timeout(Duration::from_secs(20), read.next())
            .await
            .map_err(|_| anyhow::anyhow!("等待 QQ HELLO 超时"))?
            .ok_or_else(|| anyhow::anyhow!("QQ 连接在 HELLO 前断开"))??;
        let hello: Value = parse_ws(&hello)?;
        anyhow::ensure!(
            hello.get("op").and_then(|v| v.as_u64()) == Some(OP_HELLO as u64),
            "QQ gateway 首帧不是 HELLO"
        );
        let heartbeat_ms = hello
            .pointer("/d/heartbeat_interval")
            .and_then(|v| v.as_u64())
            .unwrap_or(30_000);
        let heartbeat = Duration::from_millis(heartbeat_ms);

        // identify
        let token = self.ensure_token().await?;
        let identify = json!({
            "op": OP_IDENTIFY,
            "d": {
                "token": format!("QQBot {token}"),
                "intents": INTENT_GROUP_AND_C2C,
                "shard": [0, 1],
            }
        });
        write.send(Message::Text(identify.to_string())).await?;

        // 注意：identify 已发出但 QQ 还没回 READY，此时不算真正在线（鉴权可能被拒）。
        // 真正的 ready 由 handle_dispatch 收到 READY 事件后置位，避免「假在线」。
        self.set_phase("connecting", "已连 gateway，等待 READY")
            .await;
        tracing::info!(
            api_base = %self.api_base,
            "QQ 机器人已连接 gateway，等待 READY（未收到消息时先核对环境是否为沙箱）"
        );

        // 心跳 + 收事件在同一任务里 select：官方要求按 HELLO 下发的间隔回 op=1，
        // 不回心跳 gateway 会在约 60s 后强制 reset 连接。
        let mut hb = tokio::time::interval(heartbeat);
        hb.tick().await; // interval 首个 tick 立即触发，吃掉
        let result = loop {
            tokio::select! {
                _ = hb.tick() => {
                    let seq = self.inner.lock().await.seq;
                    if let Err(e) = write
                        .send(Message::Text(json!({ "op": OP_HEARTBEAT, "d": seq }).to_string()))
                        .await
                    {
                        break Err(anyhow::anyhow!("发送心跳失败: {e}"));
                    }
                }
                frame = read.next() => {
                    match frame {
                        None => break Err(anyhow::anyhow!("QQ 连接已关闭")),
                        Some(Err(e)) => break Err(anyhow::anyhow!("QQ 连接断开: {e}")),
                        Some(Ok(m)) => {
                            if let Err(e) = self.handle_frame(m).await {
                                break Err(e);
                            }
                        }
                    }
                }
            }
        };
        let _ = write.close().await;
        result
    }

    /// 处理单帧：dispatch 分发，reconnect/invalid 会话转为错误触发重连
    async fn handle_frame(self: &Arc<Self>, msg: Message) -> anyhow::Result<()> {
        let frame: Value = parse_ws(&msg)?;
        if frame.is_null() {
            return Ok(()); // ping/pong 帧
        }
        let op = frame.get("op").and_then(|v| v.as_u64()).unwrap_or(255) as u8;
        if let Some(s) = frame.get("s").and_then(|v| v.as_i64()) {
            if s > 0 {
                self.inner.lock().await.seq = s;
            }
        }
        match op {
            OP_DISPATCH => {
                let t = frame
                    .get("t")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // 全量留痕：任何 dispatch 都打印事件名 + 原始帧预览（排查「平台到底推没推」）
                tracing::info!(
                    event = %t,
                    preview = %truncate(&frame.to_string(), 400),
                    "QQ 收到 dispatch 事件"
                );
                self.handle_dispatch(&t, frame.get("d").cloned().unwrap_or(Value::Null))
                    .await;
            }
            OP_RECONNECT => anyhow::bail!("QQ 要求重连"),
            OP_INVALID_SESS => anyhow::bail!("QQ 会话失效，需要重新 identify"),
            _ => {}
        }
        Ok(())
    }

    async fn handle_dispatch(self: &Arc<Self>, event_type: &str, d: Value) {
        // READY：identify 被 QQ 接受，此刻才算真正在线
        if event_type == "READY" {
            let sid = d.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
            let bot_id = d.pointer("/user/id").and_then(|v| v.as_str()).unwrap_or("");
            let bot_name = d
                .pointer("/user/username")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            self.set_phase("ready", format!("已登录 {bot_name}")).await;
            tracing::info!(
                session_id = %sid,
                bot_id = %bot_id,
                bot_name = %bot_name,
                "QQ 机器人 READY，鉴权通过"
            );
            return;
        }

        let msg_id = d
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let content = d
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let group_openid = d
            .get("group_openid")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let user_openid = d
            .pointer("/author/user_openid")
            .or_else(|| d.pointer("/author/id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let is_group = event_type == "GROUP_AT_MESSAGE_CREATE"
            || event_type == "GROUP_MESSAGE_CREATE"
            || !group_openid.is_empty();
        let is_c2c = event_type == "C2C_MESSAGE_CREATE" || !user_openid.is_empty();
        tracing::info!(
            event = %event_type,
            is_group,
            is_c2c,
            group = %group_openid,
            user = %user_openid,
            msg_id = %msg_id,
            text_len = content.chars().count(),
            atts = d.get("attachments").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
            "QQ 消息事件解析"
        );
        // 本部署只服务私聊（BOT_QQ_PRIVATE_ONLY=0 可恢复群聊）：群聊事件一律忽略，
        // 这样群里被误触发也不会响应，同时不依赖群场景权限配置。
        if is_group && private_only() {
            tracing::info!(
                event = %event_type,
                group = %group_openid,
                "群聊事件：本部署只做私聊，已忽略"
            );
            return;
        }
        if !is_group && !is_c2c {
            tracing::warn!(event = %event_type, "QQ 事件类型不识别（既不是群@也不是私聊），丢弃");
            return;
        }
        let conversation = if is_group {
            group_openid.clone()
        } else if !user_openid.is_empty() {
            user_openid.clone()
        } else {
            return;
        };

        // 富媒体消息 content 常是 "file://…" 或空，不当正文
        let mut text = content.trim().trim_start_matches('/').trim().to_string();
        if let Some(i) = text.find("file://") {
            text = text[..i].trim().to_string();
        }
        let mut attachments = Vec::new();
        if let Some(atts) = d.get("attachments").and_then(|v| v.as_array()) {
            for a in atts {
                let url = a.get("url").and_then(|v| v.as_str()).unwrap_or_default();
                if url.is_empty() {
                    continue;
                }
                attachments.push(Attachment {
                    url: normalize_att_url(url),
                    file_name: a
                        .get("filename")
                        .and_then(|v| v.as_str())
                        .unwrap_or("file")
                        .to_string(),
                });
            }
        }
        if text.is_empty() && attachments.is_empty() {
            tracing::warn!(event = %event_type, "QQ 消息既无文本也无附件，丢弃");
            return;
        }

        {
            let mut inner = self.inner.lock().await;
            inner.sessions.insert(
                conversation.clone(),
                SessionCtx {
                    group_openid: group_openid.clone(),
                    user_openid: user_openid.clone(),
                    msg_id: msg_id.clone(),
                    at: std::time::Instant::now(),
                },
            );
            inner.msg_count += 1;
            inner.last_text = text.clone();
        }

        let inbound = Inbound {
            channel: "qq",
            conversation_id: conversation.clone(),
            is_group,
            text,
            attachments,
        };
        let Some(out) = BotRouter::handle(&self.st, &self.cfg, inbound).await else {
            return;
        };
        if let Err(e) = self.send(&out).await {
            self.set_note(format!("发送失败: {e}")).await;
            tracing::warn!(error = %e, "QQ 消息发送失败");
        }
    }

    async fn send(&self, out: &Outbound) -> anyhow::Result<()> {
        let (group, user, msg_id, in_window) = {
            let inner = self.inner.lock().await;
            match inner.sessions.get(&out.conversation_id) {
                Some(sc) => {
                    // 被动回复窗口：单聊 60 分钟有效、群聊 5 分钟。窗口内必须带 msg_id，
                    // 否则降级成「主动消息」——每月只有 4 条额度，超了直接静默失败。
                    let win = reply_window_secs(!sc.group_openid.is_empty());
                    (
                        sc.group_openid.clone(),
                        sc.user_openid.clone(),
                        sc.msg_id.clone(),
                        sc.at.elapsed() < Duration::from_secs(win),
                    )
                }
                None => anyhow::bail!("找不到会话 {} 的回复上下文", out.conversation_id),
            }
        };
        if !msg_id.is_empty() && !in_window {
            tracing::warn!("被动回复窗口已过，本条将按主动消息发送（每月额度 4 条）");
        }
        let msg_id = if in_window { msg_id } else { String::new() };
        let seq = {
            let mut inner = self.inner.lock().await;
            inner.msg_seq += 1;
            inner.msg_seq
        };
        let headers = self.auth_headers().await?;
        let path = if !group.is_empty() {
            format!("/v2/groups/{group}/messages")
        } else if !user.is_empty() {
            format!("/v2/users/{user}/messages")
        } else {
            anyhow::bail!("缺少回复目标 openid");
        };
        let body = json!({
            "content": out.text,
            "msg_type": 0,
            "msg_seq": seq,
            "msg_id": msg_id,
        });
        let mut req = self.http.post(format!("{}{path}", self.api_base));
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let resp = req.json(&body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("QQ 消息发送失败 HTTP {status}: {}", truncate(&text, 200));
        }
        Ok(())
    }
}

/// 本部署只服务私聊（BOT_QQ_PRIVATE_ONLY=0 可恢复群聊）
fn private_only() -> bool {
    is_private_only_env(std::env::var("BOT_QQ_PRIVATE_ONLY").ok().as_deref())
}

/// 纯函数版：未设置 = 只做私聊；显式 0/false/no/off 才恢复群聊
fn is_private_only_env(v: Option<&str>) -> bool {
    v.map(|v| {
        !matches!(
            v.trim(),
            "0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF"
        )
    })
    .unwrap_or(true)
}

/// 被动回复窗口：单聊 60 分钟有效、群聊 5 分钟
fn reply_window_secs(is_group: bool) -> u64 {
    if is_group {
        4 * 60
    } else {
        55 * 60
    }
}

/// 附件 URL 可能是协议相对的 "//…"，补 https:
fn normalize_att_url(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        url.to_string()
    }
}

fn parse_ws(msg: &Message) -> anyhow::Result<Value> {
    match msg {
        Message::Text(t) => Ok(serde_json::from_str(t)?),
        Message::Binary(b) => Ok(serde_json::from_slice(b)?),
        Message::Ping(_) | Message::Pong(_) => Ok(Value::Null),
        other => anyhow::bail!("QQ WS 非文本帧: {other:?}"),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_private_only() {
        assert!(is_private_only_env(None), "未设置时应只做私聊");
        assert!(is_private_only_env(Some("1")));
        assert!(is_private_only_env(Some("true")));
        assert!(!is_private_only_env(Some("0")), "显式 0 才恢复群聊");
        assert!(!is_private_only_env(Some("false")));
        assert!(!is_private_only_env(Some("off")));
    }

    #[test]
    fn private_reply_window_is_an_hour() {
        // 私聊被动回复窗口 60 分钟，必须远大于旧值 4 分钟，
        // 否则慢命令（同步主播等）回复会降级成主动消息，每月只 4 条
        assert_eq!(reply_window_secs(false), 55 * 60);
        assert_eq!(reply_window_secs(true), 4 * 60);
    }

    #[test]
    fn attachment_url_normalized() {
        assert_eq!(
            normalize_att_url("//cdn.qq.com/a.txt"),
            "https://cdn.qq.com/a.txt"
        );
        assert_eq!(normalize_att_url("https://x/y"), "https://x/y");
    }
}
