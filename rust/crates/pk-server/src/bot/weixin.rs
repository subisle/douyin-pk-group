//! 微信 iLink 渠道。端点、请求体、认证头全部照搬 Go 版 / 615 ilink-adapter，
//! 私有协议字段名一个都不能改：
//!   - 固定路径 {base}/ilink/bot/{route}，base 只允许 ilinkai.weixin.qq.com / edge.weixin.qq.com
//!   - POST 必须带 AuthorizationType: ilink_bot_token、X-WECHAT-UIN（随机）、
//!     Authorization: Bearer {token}，请求体自动包一层 base_info.channel_version
//!   - 长轮询 getupdates，返回 msgs[] 与游标 get_updates_buf；超时跟随服务端下发
//!   - errcode == -14 表示登录过期，必须重新扫码

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::state::AppState;

use super::{BotConfig, BotRouter, Inbound, Outbound};

const DEFAULT_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
const CHANNEL_VERSION: &str = "1.0.2";
const SESSION_EXPIRED: i64 = -14;

const TRUSTED_HOSTS: [&str; 2] = ["ilinkai.weixin.qq.com", "edge.weixin.qq.com"];

#[derive(Clone, Debug, Default)]
struct SessionCtx {
    to_user_id: String,
    group_id: String,
    context_token: String,
}

#[derive(Default)]
struct Inner {
    phase: String, // disconnected / awaiting_scan / running / session_expired / stopped / error
    note: String,
    sessions: std::collections::HashMap<String, SessionCtx>,
    cursor: String,
    msg_count: u64,
    sent: u64,
    last_text: String,
    /// 扫码登录态
    qr_code: String,
    nickname: String,
}

pub struct WxTransport {
    st: Arc<AppState>,
    cfg: BotConfig,
    base_url: String,
    token: Mutex<String>,
    http: reqwest::Client,
    inner: Arc<Mutex<Inner>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl WxTransport {
    pub fn new(st: Arc<AppState>, cfg: BotConfig, token: String) -> anyhow::Result<Self> {
        // 防止配置被改成任意地址导致 token 外泄
        let host = url_host(DEFAULT_BASE_URL);
        anyhow::ensure!(
            TRUSTED_HOSTS.contains(&host.as_str()),
            "微信接口地址是非受信任的 iLink 主机: {host}"
        );
        let has_token = !token.is_empty();
        Ok(Self {
            st,
            cfg,
            base_url: DEFAULT_BASE_URL.into(),
            token: Mutex::new(token),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(70))
                .build()?,
            inner: Arc::new(Mutex::new(Inner {
                phase: if has_token {
                    "disconnected".into()
                } else {
                    "awaiting_scan".into()
                },
                ..Default::default()
            })),
            task: Mutex::new(None),
        })
    }

    pub fn configured(&self) -> bool {
        self.cfg.has_wechat()
    }

    pub async fn status(&self) -> Value {
        let inner = self.inner.lock().await;
        json!({
            "channel": "weixin",
            "configured": self.configured(),
            "phase": inner.phase,
            "note": inner.note,
            "connected": inner.phase == "running",
            "messageCount": inner.msg_count,
            "sentCount": inner.sent,
            "lastMessage": inner.last_text,
            "hasToken": !self.token.lock().await.is_empty(),
            "nickname": inner.nickname,
        })
    }

    /* ------------------------------ HTTP 层 ------------------------------ */

    fn route_url(&self, route: &str, query: &[(&str, &str)]) -> String {
        let mut url = format!("{}/ilink/bot/{route}", self.base_url);
        if !query.is_empty() {
            let qs: Vec<String> = query
                .iter()
                .map(|(k, v)| format!("{k}={}", urlencode(v)))
                .collect();
            url.push('?');
            url.push_str(&qs.join("&"));
        }
        url
    }

    async fn do_json(
        &self,
        method: reqwest::Method,
        route: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
    ) -> anyhow::Result<Value> {
        let mut req = self.http.request(method, self.route_url(route, query));
        if let Some(payload) = &body {
            req = req
                .header("Content-Type", "application/json")
                .header("AuthorizationType", "ilink_bot_token")
                .header("X-WECHAT-UIN", random_uin())
                .header(
                    "Authorization",
                    format!("Bearer {}", self.token.lock().await),
                );
            req = req.json(payload);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let raw = resp.bytes().await?;
        anyhow::ensure!(
            status.is_success(),
            "微信接口 HTTP {status}: {}",
            String::from_utf8_lossy(&raw)
        );
        if raw.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&raw).map_err(|e| anyhow::anyhow!("微信接口返回了无效 JSON: {e}"))
    }

    async fn post_json(&self, route: &str, mut body: Value) -> anyhow::Result<Value> {
        // 请求体自动包一层 base_info.channel_version
        let payload = body.as_object_mut().expect("body 必须是对象");
        let base_info = payload
            .remove("base_info")
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        let mut merged = json!({ "channel_version": CHANNEL_VERSION });
        if let Some(m) = merged.as_object_mut() {
            for (k, v) in base_info {
                m.insert(k, v);
            }
        }
        payload.insert("base_info".into(), merged);
        self.do_json(reqwest::Method::POST, route, &[], Some(body))
            .await
    }

    /// errcode/ret 非 0 → 错误；-14 = 登录过期
    fn check_code(payload: &Value) -> anyhow::Result<()> {
        let code = payload
            .get("errcode")
            .and_then(|v| v.as_i64())
            .or_else(|| payload.get("ret").and_then(|v| v.as_i64()))
            .unwrap_or(0);
        if code == 0 {
            return Ok(());
        }
        let msg = payload
            .get("errmsg")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if code == SESSION_EXPIRED {
            anyhow::bail!("SESSION_EXPIRED: 微信登录已过期，请重新扫码连接：{msg}");
        }
        anyhow::bail!("微信接口错误 ({code}): {msg}");
    }

    /* ------------------------------ 扫码登录 ------------------------------ */

    /// 取登录二维码（bot_type=3）
    pub async fn get_qrcode(&self) -> anyhow::Result<Value> {
        let out = self
            .do_json(
                reqwest::Method::GET,
                "get_bot_qrcode",
                &[("bot_type", "3")],
                None,
            )
            .await?;
        Self::check_code(&out)?;
        let qrcode = str_of(&out, &["qrcode"]);
        {
            let mut inner = self.inner.lock().await;
            inner.qr_code = qrcode.clone();
            inner.phase = "awaiting_scan".into();
        }
        Ok(json!({
            "qrcode": qrcode,
            "url": first_of(&out, &["qrcode_url", "qr_code_url", "url"]),
        }))
    }

    /// 查询扫码状态；扫完带回 token。成功后持久化到 SQLite 并写回 transport。
    pub async fn check_qrcode(&self, qrcode: &str) -> anyhow::Result<Value> {
        anyhow::ensure!(!qrcode.trim().is_empty(), "微信二维码标识不能为空");
        let out = self
            .do_json(
                reqwest::Method::GET,
                "get_qrcode_status",
                &[("qrcode", qrcode)],
                None,
            )
            .await?;
        Self::check_code(&out)?;
        let status = str_of(&out, &["status"]).to_lowercase();
        let token = first_of(&out, &["token", "bot_token", "access_token"]);
        let nickname = first_of(&out, &["nickname", "nick_name", "name"]);
        let token_saved = !token.is_empty();
        if token_saved {
            self.set_token(token, &nickname).await?;
        }
        Ok(json!({
            "status": if status.is_empty() { "waiting" } else { &status },
            "tokenSaved": token_saved,
            "nickname": nickname,
        }))
    }

    /// token 落 MySQL config（重启不丢），并写回内存
    pub async fn set_token(&self, token: String, nickname: &str) -> anyhow::Result<()> {
        anyhow::ensure!(!token.is_empty(), "token 为空");
        *self.token.lock().await = token.clone();
        crate::store::config_set(&self.st, "wechat_token", &token).await;
        {
            let mut inner = self.inner.lock().await;
            inner.nickname = nickname.to_string();
            inner.phase = "running".into();
            inner.note.clear();
        }
        tracing::info!(nickname, "微信 iLink 登录凭证已更新");
        Ok(())
    }

    /* ------------------------------ 启停 ------------------------------ */

    pub async fn start(self: &Arc<Self>) -> anyhow::Result<()> {
        let mut task = self.task.lock().await;
        if task.is_some() {
            return Ok(());
        }
        anyhow::ensure!(self.configured(), "微信渠道未初始化");
        if self.token.lock().await.is_empty() {
            self.inner.lock().await.phase = "awaiting_scan".into();
            anyhow::bail!("微信未登录：请先通过 /api/admin/bots/weixin/qrcode 扫码连接");
        }
        self.inner.lock().await.phase = "running".into();
        let me = Arc::clone(self);
        *task = Some(tokio::spawn(async move { me.poll_loop().await }));
        Ok(())
    }

    pub async fn stop(&self) {
        if let Some(h) = self.task.lock().await.take() {
            h.abort();
        }
        let mut inner = self.inner.lock().await;
        inner.phase = "stopped".into();
    }

    /// 长轮询收消息循环。游标持久化在内存（重启从头收一次，命令都是幂等/只读，可接受）。
    async fn poll_loop(self: Arc<Self>) {
        loop {
            let cursor = self.inner.lock().await.cursor.clone();
            match self
                .post_json("getupdates", json!({ "get_updates_buf": cursor }))
                .await
            {
                Ok(out) => {
                    if let Err(e) = Self::check_code(&out) {
                        let msg = e.to_string();
                        if msg.contains("SESSION_EXPIRED") {
                            let mut inner = self.inner.lock().await;
                            inner.phase = "session_expired".into();
                            inner.note = "登录过期，请重新扫码".into();
                            *self.token.lock().await = String::new();
                            tracing::warn!("微信登录过期，轮询停止");
                            return;
                        }
                        self.set_note(format!("getupdates 错误: {msg}")).await;
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        continue;
                    }
                    let new_cursor = str_of(&out, &["get_updates_buf"]);
                    let timeout_ms = out
                        .get("longpolling_timeout_ms")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(30_000);
                    {
                        let mut inner = self.inner.lock().await;
                        inner.cursor = new_cursor;
                    }
                    if let Some(msgs) = out.get("msgs").and_then(|v| v.as_array()) {
                        for m in msgs {
                            self.handle_msg(m).await;
                        }
                    }
                    // 服务端没消息时立刻再轮；有下发超时就按服务端节奏空转等待
                    if msgs_empty(&out) {
                        tokio::time::sleep(Duration::from_millis(timeout_ms.min(35_000))).await;
                    }
                }
                Err(e) => {
                    self.set_note(format!("轮询失败: {e}")).await;
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }

    async fn handle_msg(self: &Arc<Self>, m: &Value) {
        let from_user = str_of(m, &["from_user_id"]);
        let group_id = str_of(m, &["group_id"]);
        let context_token = str_of(m, &["context_token"]);
        let conversation = if !group_id.is_empty() {
            group_id.clone()
        } else {
            from_user.clone()
        };
        if conversation.is_empty() {
            return;
        }
        // 私聊会话上下文落库（2026-10-09）：主动推送（追分提醒/每日报告）必须带
        // to_user_id + context_token，而 iLink 只有被动回复形态——只能在每次入站时
        // 把上下文存下来，推送时再取（服务重启不丢）。
        if group_id.is_empty() {
            crate::store::wx_session_upsert(&self.st, &conversation, &from_user, &context_token)
                .await;
        }
        // 第一条 type=1 文本
        let mut text = String::new();
        if let Some(items) = m.get("item_list").and_then(|v| v.as_array()) {
            for it in items {
                if it.get("type").and_then(|v| v.as_i64()) == Some(1) {
                    if let Some(t) = it.pointer("/text_item/text").and_then(|v| v.as_str()) {
                        text = t.to_string();
                        break;
                    }
                }
            }
        }
        {
            let mut inner = self.inner.lock().await;
            inner.sessions.insert(
                conversation.clone(),
                SessionCtx {
                    to_user_id: from_user.clone(),
                    group_id: group_id.clone(),
                    context_token: context_token.clone(),
                },
            );
            inner.msg_count += 1;
            inner.last_text = text.clone();
        }
        let inbound = Inbound {
            channel: "weixin",
            conversation_id: conversation.clone(),
            is_group: !group_id.is_empty(),
            text,
            attachments: Vec::new(), // iLink 文件消息 v1 不处理，只认文本命令
        };
        if let Some(out) = BotRouter::handle(&self.st, &self.cfg, inbound).await {
            if let Err(e) = self.send(&out).await {
                self.set_note(format!("发送失败: {e}")).await;
                tracing::warn!(error = %e, "微信消息发送失败");
            }
        }
    }

    async fn send(&self, out: &Outbound) -> anyhow::Result<()> {
        let (to_user, group, context_token) = {
            let inner = self.inner.lock().await;
            match inner.sessions.get(&out.conversation_id) {
                Some(sc) => (
                    sc.to_user_id.clone(),
                    sc.group_id.clone(),
                    sc.context_token.clone(),
                ),
                None => anyhow::bail!("找不到会话 {} 的回复上下文", out.conversation_id),
            }
        };
        self.send_raw(&to_user, &group, &context_token, &out.text)
            .await
    }

    /// 主动推送（追分提醒 / 每日报告，2026-10-09）：会话上下文从 DB 取，
    /// 内存里没有也能发（服务重启过 / 用户上次发言在另一个进程周期）。
    /// 会话 id = 私聊的 from_user_id，与 to_user_id 相同。
    pub async fn send_proactive(&self, conversation_id: &str, text: &str) -> anyhow::Result<()> {
        anyhow::ensure!(self.configured(), "微信渠道未初始化");
        let (to_user, token) = match crate::store::wx_session_get(&self.st, conversation_id).await {
            Some(v) => v,
            None => {
                // 兜底：进程内还留着会话（本次运行内用户刚发过消息）
                let inner = self.inner.lock().await;
                let sc = inner.sessions.get(conversation_id).ok_or_else(|| {
                    anyhow::anyhow!("没有可用的微信会话上下文（让对方先给机器人发一条消息）")
                })?;
                (sc.to_user_id.clone(), sc.context_token.clone())
            }
        };
        self.send_raw(&to_user, "", &token, text).await
    }

    async fn send_raw(
        &self,
        to_user: &str,
        group: &str,
        context_token: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !context_token.is_empty(),
            "缺少 context_token（iLink 回复必须带）"
        );
        let body = json!({
            "msg": {
                "from_user_id": "",
                "to_user_id": to_user,
                "client_id": client_id(),
                "message_type": 2,
                "message_state": 2,
                "context_token": context_token,
                "item_list": [{ "type": 1, "text_item": { "text": text } }],
                "group_id": group,
            }
        });
        let resp = self.post_json("sendmessage", body).await?;
        Self::check_code(&resp)?;
        self.inner.lock().await.sent += 1;
        Ok(())
    }

    async fn set_note(&self, note: impl Into<String>) {
        self.inner.lock().await.note = note.into();
    }
}

fn msgs_empty(out: &Value) -> bool {
    out.get("msgs")
        .and_then(|v| v.as_array())
        .map(|a| a.is_empty())
        .unwrap_or(true)
}

fn str_of(v: &Value, keys: &[&str]) -> String {
    for k in keys {
        if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
            let s = s.trim();
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    String::new()
}

fn first_of(v: &Value, keys: &[&str]) -> String {
    str_of(v, keys)
}

fn url_host(u: &str) -> String {
    u.split("://")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .unwrap_or("")
        .to_lowercase()
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
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

/// X-WECHAT-UIN：4 字节随机数十进制再 base64（照 Go 版）
fn random_uin() -> String {
    let n: u32 = rand_u32();
    let s = n.to_string();
    b64(s.as_bytes())
}

fn rand_u32() -> u32 {
    let mut buf = [0u8; 4];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| {
            use std::io::Read;
            f.read_exact(&mut buf)?;
            Ok(())
        })
        .is_err()
    {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u32)
            .unwrap_or(0);
        buf = t.to_be_bytes();
    }
    u32::from_be_bytes(buf)
}

fn b64(data: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn client_id() -> String {
    let n = rand_u32();
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("c-{ms}-{}", n)
}
