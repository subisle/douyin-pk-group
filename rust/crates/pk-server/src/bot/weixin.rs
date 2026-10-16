//! 微信 iLink 渠道。端点、请求体、认证头逐字段对齐腾讯官方
//! `Tencent/openclaw-weixin` 插件的 `docs/protocol.md`（1.0.1 全链路修复）：
//!   - 固定路径 {base}/ilink/bot/{route}，base 只允许受信任的 iLink 主机
//!   - 鉴权 POST 带 AuthorizationType: ilink_bot_token、X-WECHAT-UIN（随机 u32
//!     十进制再 base64）、Authorization: Bearer {bot_token}，
//!     外加 iLink-App-Id: bot 与 iLink-App-ClientVersion（0x00MMNNPP 打包）
//!   - 信封恒带 base_info.channel_version + bot_agent（仅观测用）
//!   - 扫码：POST get_bot_qrcode?bot_type=3（body 带 local_token_list），
//!     状态机 wait/scaned/confirmed/expired/need_verifycode/verify_code_blocked/
//!     scaned_but_redirect/binded_redirect，token 在 confirmed 的 bot_token 里，
//!     baseurl 指示后续 API 主机（redirect_host 同理换主机继续轮询）
//!   - 收消息 POST getupdates（游标 get_updates_buf，仅非空才更新），
//!     errcode/ret == -14 表示登录过期 → 暂停一小时后重试（不清凭据），
//!     连续多次失败才回落到等待扫码
//!   - 发消息 POST sendmessage（item type 1 文本；2 图 / 3 语音 / 4 文件 / 5 视频
//!     走 getuploadurl + AES-128-ECB PKCS#7 加密后上传 novac2c CDN）
//!   - 生命周期 msg/notifystart、msg/notifystop（失败仅告警）

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::state::AppState;

use super::{Attachment, BotConfig, BotRouter, Inbound, Outbound};

const DEFAULT_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
/// 对齐 openclaw-weixin 的插件版本号（信封 channel_version）
pub(crate) const CHANNEL_VERSION: &str = "2.4.10";
/// 客户端版本打包（iLink-App-ClientVersion = 0x00MMNNPP 的十进制）
const CLIENT_VERSION_MMNNPP: (u32, u32, u32) = (2, 4, 10);
const SESSION_EXPIRED: i64 = -14;
/// -14 后暂停多久再试（对齐 openclaw：暂停一小时，不清凭据）
pub(crate) const SESSION_EXPIRED_RETRY: Duration = Duration::from_secs(3600);
/// 连续多少次 -14 之前先等一小时重试；超过才认定凭据彻底失效、回到等待扫码
pub(crate) const SESSION_EXPIRED_MAX_RETRY: u32 = 4;

/* ------- 轮询失败节律：逐条对齐官方 @tencent-weixin/openclaw-weixin@2.4.10 -------
源：官方 dist/src/monitor/monitor.js（2026-10-16 从 npm tarball 核对）：
  · RETRY_DELAY_MS = 2_000          单次失败等 2s 再轮
  · MAX_CONSECUTIVE_FAILURES = 3    连续失败阈值
  · BACKOFF_DELAY_MS = 30_000       连续 3 次退避 30s，且计数清零（不是指数增长）
  · DEFAULT_LONG_POLL_TIMEOUT_MS = 35_000，且跟随服务端 longpolling_timeout_ms
注意官方**没有**入站消息限流的概念（它是客户端）；我们的 BOT_RATE_LIMIT 是
4 位绑定码防爆破的自身闸门，不属于协议节律，两者各管各的。 */

/// 单次失败后的重试间隔（官方 `RETRY_DELAY_MS`）
pub(crate) const RETRY_DELAY: Duration = Duration::from_secs(2);
/// 连续失败达到该次数后退避（官方 `MAX_CONSECUTIVE_FAILURES`）
pub(crate) const MAX_CONSECUTIVE_FAILURES: u32 = 3;
/// 连续失败退避时长（官方 `BACKOFF_DELAY_MS`）
pub(crate) const BACKOFF_DELAY: Duration = Duration::from_secs(30);

/// getupdates 失败第 n 次该等多久（官方 monitor.js 节律）。
/// 前 `MAX_CONSECUTIVE_FAILURES - 1` 次 2s，第 3 次起 30s（官方退避后把计数清零）。
pub(crate) fn monitor_failure_delay(failures: u32) -> Duration {
    if failures == 0 {
        return RETRY_DELAY;
    }
    if failures >= MAX_CONSECUTIVE_FAILURES {
        BACKOFF_DELAY
    } else {
        RETRY_DELAY
    }
}
/// 媒体 CDN（下载兜底模板 / 上传 host 提示）
pub(crate) const CDN_BASE_URL: &str = "https://novac2c.cdn.weixin.qq.com/c2c";

const TRUSTED_HOSTS: [&str; 3] = [
    "ilinkai.weixin.qq.com",
    "edge.weixin.qq.com",
    // confirmed 返回的 baseurl / redirect_host 可能给出其它官方主机，运行时校验后缀
    "weixin.qq.com",
];

/* ---------------- 用户自助扫码（多账号）与无状态收发（2026-10-11） ----------------
 *
 * 官方协议里二维码是「账号级 bot 登录」：每个用户自己扫码确认后，服务端拿到一组
 * bot_token / ilink_bot_id / ilink_user_id / baseurl，即多账号支持里的一个账号。
 * 推送服务按账号各跑一条 getupdates 长轮询；用户首次给机器人发消息时留下
 * context_token，之后才具备主动推送的会话上下文。以下函数不依赖全局 BotManager，
 * 供展示端「我的」页扫码与推送引擎直接调用。 */

/// 官方 `qrcode_img_content` 是需要编码进二维码的内容（通常是 URL 字符串），
/// 不是图片地址。这里统一渲染成 SVG data URL，前后端 <img> 可直接显示，零前端依赖。
pub fn qr_svg_data_url(content: &str) -> Option<String> {
    let code = qrcode::QrCode::new(content.as_bytes()).ok()?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(180, 180)
        .quiet_zone(true)
        .build();
    use base64::Engine as _;
    Some(format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(svg.as_bytes())
    ))
}

/* 连接池（2026-10-11 多 bot 扩容）：100+ 个扫码账号共用这一个 `reqwest::Client`
 * 跑 getupdates 长轮询。默认值是「每 host 空闲连接上限 usize::MAX / 90 秒回收」，
 * 看着够用，但池子是全局共享的——高频账号会把空闲连接占满并让低频账号的连接
 * 在 90 秒后被回收，长轮询正好卡在这个窗口上被断开。这里显式给一个明确的池：
 *   - pool_max_idle_per_host: 每个 host 最多缓存的空闲连接数（>= 并发 bot 数）
 *   - pool_idle_timeout: 空闲连接保留时长，长轮询一轮最长 35 秒，给足余量
 * 并发上限用环境变量 WX_POOL_MAX_IDLE_PER_HOST 调（默认 128）。 */
fn pool_max_idle_per_host() -> usize {
    std::env::var("WX_POOL_MAX_IDLE_PER_HOST")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| (1..=4096).contains(v))
        .unwrap_or(128)
}

/// 空闲连接保留时长（长轮询一轮 <= 35 秒，90 秒默认值对抖动后的复用偏紧）
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

fn shared_http() -> &'static reqwest::Client {
    static HTTP: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    HTTP.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(75))
            .pool_max_idle_per_host(pool_max_idle_per_host())
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .build()
            .expect("构造微信 HTTP 客户端失败")
    })
}

fn normalize_base(base_url: &str) -> String {
    let b = base_url.trim().trim_end_matches('/');
    if b.is_empty() {
        DEFAULT_BASE_URL.to_string()
    } else if b.starts_with("http://") || b.starts_with("https://") {
        b.to_string()
    } else {
        format!("https://{b}")
    }
}

/// 无状态 JSON 调用：二维码请求 / 状态轮询 / 长轮询 / 发消息共用。
/// POST 一律带 AuthorizationType（官方协议：二维码 POST 也带）与 X-WECHAT-UIN。
pub(crate) async fn wx_json(
    method: reqwest::Method,
    base_url: &str,
    route: &str,
    query: &[(&str, &str)],
    token: Option<&str>,
    body: Option<Value>,
) -> anyhow::Result<Value> {
    let base = normalize_base(base_url);
    anyhow::ensure!(is_trusted_host(&base), "微信接口地址不可信: {base}");
    let mut req =
        shared_http().request(method.clone(), WxTransport::route_url(&base, route, query));
    req = req
        .header("iLink-App-Id", "bot")
        .header("iLink-App-ClientVersion", client_version_decimal());
    if let Some(t) = token.map(str::trim).filter(|s| !s.is_empty()) {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    if let Some(payload) = &body {
        req = req
            .header("Content-Type", "application/json")
            .header("AuthorizationType", "ilink_bot_token")
            .header("X-WECHAT-UIN", random_uin())
            .json(payload);
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

/// 用户扫码第一步：取账号登录二维码（固定 API Base，不带本地 token）
pub async fn user_login_qrcode() -> anyhow::Result<Value> {
    let out = wx_json(
        reqwest::Method::POST,
        DEFAULT_BASE_URL,
        "get_bot_qrcode",
        &[("bot_type", "3")],
        None,
        Some(json!({ "local_token_list": [] })),
    )
    .await?;
    WxTransport::check_code(&out)?;
    let content = str_of(&out, &["qrcode_img_content"]);
    Ok(json!({
        "qrcode": str_of(&out, &["qrcode"]),
        "imgContent": qr_svg_data_url(&content).unwrap_or_default(),
        "content": content,
        "url": first_of(&out, &["qrcode_url", "qr_code_url", "url"]),
    }))
}

/// 用户扫码第二步：轮询二维码状态（原样返回 token / 账号 id 等字段）
pub async fn user_login_status(qrcode: &str, verify_code: Option<&str>) -> anyhow::Result<Value> {
    user_login_status_at(DEFAULT_BASE_URL, qrcode, verify_code).await
}

/// 同 [`user_login_status`]，但允许按 `scaned_but_redirect` 返回的官方主机继续轮询。
pub async fn user_login_status_at(
    base_url: &str,
    qrcode: &str,
    verify_code: Option<&str>,
) -> anyhow::Result<Value> {
    anyhow::ensure!(!qrcode.trim().is_empty(), "微信二维码标识不能为空");
    let mut query: Vec<(&str, &str)> = vec![("qrcode", qrcode)];
    if let Some(v) = verify_code.map(str::trim).filter(|s| !s.is_empty()) {
        query.push(("verify_code", v));
    }
    let out = wx_json(
        reqwest::Method::GET,
        base_url,
        "get_qrcode_status",
        &query,
        None,
        None,
    )
    .await?;
    WxTransport::check_code(&out)?;
    let status = str_of(&out, &["status"]).to_lowercase();
    Ok(json!({
        "status": if status.is_empty() { "wait" } else { &status },
        "botToken": first_of(&out, &["bot_token", "token", "access_token"]),
        "botId": str_of(&out, &["ilink_bot_id"]),
        "userId": str_of(&out, &["ilink_user_id"]),
        "baseUrl": str_of(&out, &["baseurl"]),
        "redirectHost": str_of(&out, &["redirect_host"]),
        "nickname": first_of(&out, &["nickname", "nick_name", "name"]),
    }))
}

/// 单账号长轮询 getupdates（供 wxpush 模块使用）
pub async fn user_poll_updates(base_url: &str, token: &str, cursor: &str) -> anyhow::Result<Value> {
    let out = wx_json(
        reqwest::Method::POST,
        base_url,
        "getupdates",
        &[],
        Some(token),
        Some(json!({
            "get_updates_buf": cursor,
            "base_info": { "channel_version": CHANNEL_VERSION, "bot_agent": "OpenClaw" }
        })),
    )
    .await?;
    WxTransport::check_code(&out)?;
    Ok(out)
}

/// 单账号主动发文本（扫码用户的推送走这里，不经过全局机器人 token）
///
/// 2026-10-16：**这里是所有微信出网的必经窄口**（push.rs 的推送、admin_send.rs 的
/// 管理员下发最终都调到这里），所以测试阶段的出网闸门装在这一层最稳 ——
/// 上层任何新增发送路径都不可能绕过它。开关 `PUSH_OUT_ENABLED`（语义见
/// `push::push_out_enabled`）：未开时只记 WARN 日志、不发消息、返回 Ok。
pub async fn user_send_text(
    st: &AppState,
    base_url: &str,
    token: &str,
    to_user_id: &str,
    context_token: &str,
    text: &str,
) -> anyhow::Result<()> {
    if !crate::push::push_out_allowed_for_account(st, to_user_id).await {
        let preview: String = text.chars().take(120).collect();
        tracing::warn!(
            to_user_id, preview = %preview,
            "【测试阶段】已拦截微信文本发送（未发）；要真实发送请设 PUSH_OUT_ENABLED=1"
        );
        /* 2026-10-17 审计 6-P2-1：拦截返回 Err(Blocked) 而不是 Ok ——
        「被闸拦下 ≠ 发送成功」。原先返回 Ok 会让 admin_send.rs 的手动下发把
        outbox 落成 sent 并推进 last_lineage_date（师门日报当天被永久吞掉）。
        push.rs 的自动推送已用 is_blocked() 识别，不会把 Blocked 当失败重试。 */
        return Err(crate::push::Blocked.into());
    }
    let out = wx_json(
        reqwest::Method::POST,
        base_url,
        "sendmessage",
        &[],
        Some(token),
        Some(json!({
            "msg": {
                "from_user_id": "",
                "to_user_id": to_user_id,
                "client_id": client_id(),
                "message_type": 2,
                "message_state": 2,
                "context_token": context_token,
                "item_list": [{ "type": 1, "text_item": { "text": text } }]
            },
            "base_info": { "channel_version": CHANNEL_VERSION, "bot_agent": "OpenClaw" }
        })),
    )
    .await?;
    WxTransport::check_code(&out)
}

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
    /// confirmed 后服务端可能改发baseurl / redirect_host，记下来换主机轮询
    redirect_host: String,
}

pub struct WxTransport {
    st: Arc<AppState>,
    cfg: BotConfig,
    base_url: Mutex<String>,
    /// confirmed 时拿到的 ilink_user_id（getconfig / sendtyping 用；本期只落库）
    ilink_user_id: Mutex<String>,
    token: Mutex<String>,
    http: reqwest::Client,
    inner: Arc<Mutex<Inner>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl WxTransport {
    /// `base_url`/`ilink_user_id` 由调用方（BotManager::from_env，异步上下文）从
    /// config 表带进来 —— confirmed 时可能换过 API 主机，重启后要接着用。
    pub fn new(
        st: Arc<AppState>,
        cfg: BotConfig,
        token: String,
        base_url: String,
        ilink_user_id: String,
    ) -> anyhow::Result<Self> {
        // 防止配置被改成任意地址导致 token 外泄
        anyhow::ensure!(
            is_trusted_host(DEFAULT_BASE_URL),
            "微信接口地址是非受信任的 iLink 主机: {DEFAULT_BASE_URL}"
        );
        let has_token = !token.is_empty();
        let base_url = if base_url.is_empty() || !is_trusted_host(&base_url) {
            DEFAULT_BASE_URL.into()
        } else {
            base_url
        };
        Ok(Self {
            st,
            cfg,
            base_url: Mutex::new(base_url),
            ilink_user_id: Mutex::new(ilink_user_id),
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

    async fn base(&self) -> String {
        let b = self.base_url.lock().await;
        if b.is_empty() {
            DEFAULT_BASE_URL.into()
        } else {
            b.clone()
        }
    }

    fn route_url(base: &str, route: &str, query: &[(&str, &str)]) -> String {
        let mut url = format!("{base}/ilink/bot/{route}");
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

    /// 发一个 iLink 请求。`auth` 决定带不带 Bearer（扫码创建/轮询不带）。
    /// body 为 Some 时是 JSON POST（自动带 Content-Type + X-WECHAT-UIN +
    /// iLink-App-Id/ClientVersion）；None 时是 GET（只带 App-Id/ClientVersion）。
    async fn do_json(
        &self,
        method: reqwest::Method,
        route: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
        auth: bool,
    ) -> anyhow::Result<Value> {
        let mut req = self
            .http
            .request(method, Self::route_url(&self.base().await, route, query));
        if auth {
            req = req.header(
                "Authorization",
                format!("Bearer {}", self.token.lock().await),
            );
        }
        req = req
            .header("iLink-App-Id", "bot")
            .header("iLink-App-ClientVersion", client_version_decimal());
        if let Some(payload) = &body {
            req = req
                .header("Content-Type", "application/json")
                .header("AuthorizationType", "ilink_bot_token")
                .header("X-WECHAT-UIN", random_uin());
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

    /// 带信封的鉴权 POST（getupdates / sendmessage / notifystart 等）
    async fn post_json(&self, route: &str, mut body: Value) -> anyhow::Result<Value> {
        let payload = body.as_object_mut().expect("body 必须是对象");
        let base_info = payload
            .remove("base_info")
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        let mut merged = json!({
            "channel_version": CHANNEL_VERSION,
            "bot_agent": "OpenClaw",
        });
        if let Some(m) = merged.as_object_mut() {
            for (k, v) in base_info {
                m.insert(k, v);
            }
        }
        payload.insert("base_info".into(), merged);
        self.do_json(reqwest::Method::POST, route, &[], Some(body), true)
            .await
    }

    /// errcode/ret 非 0 → 错误；-14 = 登录过期。
    /// 对齐 openclaw：缺 ret/errcode 视为成功（不同路由返回的字段不一样）。
    pub(crate) fn check_code(payload: &Value) -> anyhow::Result<()> {
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

    /// 取登录二维码（bot_type=3）。POST + local_token_list（带最近 token 便于复登，
    /// 对齐 openclaw 最多 10 个）；图片内容在 qrcode_img_content（协议里没有 qrcode_url）。
    pub async fn get_qrcode(&self) -> anyhow::Result<Value> {
        let local_tokens = {
            let t = self.token.lock().await;
            if t.is_empty() {
                vec![]
            } else {
                vec![t.clone()]
            }
        };
        let out = self
            .do_json(
                reqwest::Method::POST,
                "get_bot_qrcode",
                &[("bot_type", "3")],
                Some(json!({ "local_token_list": local_tokens })),
                false,
            )
            .await?;
        Self::check_code(&out)?;
        let qrcode = str_of(&out, &["qrcode"]);
        {
            let mut inner = self.inner.lock().await;
            inner.qr_code = qrcode.clone();
            inner.phase = "awaiting_scan".into();
            inner.redirect_host.clear();
        }
        let content = str_of(&out, &["qrcode_img_content"]);
        Ok(json!({
            "qrcode": qrcode,
            "imgContent": qr_svg_data_url(&content).unwrap_or_default(),
            "content": content,
            "url": first_of(&out, &["qrcode_url", "qr_code_url", "url"]),
        }))
    }

    /// 查询扫码状态。全状态机（对齐 openclaw）：
    /// wait / scaned / confirmed / expired / need_verifycode / verify_code_blocked /
    /// scaned_but_redirect / binded_redirect。
    /// scaned_but_redirect / binded_redirect 时按 redirect_host 换主机继续轮；
    /// confirmed 时持久化 bot_token + baseurl + ilink_user_id。
    /// need_verifycode 时前端拿 verify 提示输入验证码，下次轮询带 verify_code。
    pub async fn check_qrcode(
        &self,
        qrcode: &str,
        verify_code: Option<&str>,
    ) -> anyhow::Result<Value> {
        anyhow::ensure!(!qrcode.trim().is_empty(), "微信二维码标识不能为空");
        let mut query: Vec<(&str, &str)> = vec![("qrcode", qrcode)];
        if let Some(v) = verify_code.map(str::trim).filter(|s| !s.is_empty()) {
            query.push(("verify_code", v));
        }
        let out = self
            .do_json(
                reqwest::Method::GET,
                "get_qrcode_status",
                &query,
                None,
                false,
            )
            .await?;
        Self::check_code(&out)?;
        let status = str_of(&out, &["status"]).to_lowercase();
        let mut next_host = str_of(&out, &["redirect_host"]);
        let baseurl = str_of(&out, &["baseurl"]);
        let token = first_of(&out, &["bot_token", "token", "access_token"]);
        let ilink_user_id = first_of(&out, &["ilink_user_id", "ilink_bot_id"]);
        let nickname = first_of(&out, &["nickname", "nick_name", "name"]);
        let token_saved = !token.is_empty();
        if token_saved {
            self.set_token_full(&token, &nickname, &baseurl, &ilink_user_id)
                .await?;
        }
        if !next_host.is_empty() && is_trusted_host(&next_host) {
            self.base_url_lock_swap(&mut next_host).await;
        } else {
            next_host.clear();
        }
        Ok(json!({
            "status": if status.is_empty() { "wait" } else { &status },
            "tokenSaved": token_saved,
            "nickname": nickname,
            "redirectHost": next_host,
        }))
    }

    /// redirect_host 可信时把后续请求（含扫码轮询）切过去
    async fn base_url_lock_swap(&self, host: &mut String) {
        let mut b = self.base_url.lock().await;
        let url = if host.starts_with("http") {
            host.clone()
        } else {
            format!("https://{host}")
        };
        *b = url;
        crate::store::config_set(&self.st, "wechat_baseurl", &b.clone()).await;
        let mut inner = self.inner.lock().await;
        inner.redirect_host = std::mem::take(host);
    }

    /// token 落 config 表（重启不丢），并写回内存。扫码 confirmed 时还会带
    /// baseurl（后续 API 主机）与 ilink_user_id（输入状态等功能的身份），一并落库。
    pub async fn set_token(&self, token: String, nickname: &str) -> anyhow::Result<()> {
        self.set_token_full(&token, nickname, "", "").await
    }

    async fn set_token_full(
        &self,
        token: &str,
        nickname: &str,
        baseurl: &str,
        ilink_user_id: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!token.is_empty(), "token 为空");
        *self.token.lock().await = token.to_string();
        crate::store::config_set(&self.st, "wechat_token", token).await;
        if !baseurl.is_empty() && is_trusted_host(baseurl) {
            *self.base_url.lock().await = baseurl.to_string();
            crate::store::config_set(&self.st, "wechat_baseurl", baseurl).await;
        }
        if !ilink_user_id.is_empty() {
            *self.ilink_user_id.lock().await = ilink_user_id.to_string();
            crate::store::config_set(&self.st, "wechat_ilink_user_id", ilink_user_id).await;
        }
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
        // 生命周期上报（失败仅告警，对齐 openclaw）
        if let Err(e) = self.post_json("msg/notifystart", json!({})).await {
            tracing::warn!(error = %e, "微信 notifystart 失败（不影响收发）");
        }
        let me = Arc::clone(self);
        *task = Some(tokio::spawn(async move { me.poll_loop().await }));
        Ok(())
    }

    pub async fn stop(&self) {
        if let Some(h) = self.task.lock().await.take() {
            h.abort();
        }
        if let Err(e) = self.post_json("msg/notifystop", json!({})).await {
            tracing::warn!(error = %e, "微信 notifystop 失败（仅告警）");
        }
        let mut inner = self.inner.lock().await;
        inner.phase = "stopped".into();
    }

    /// 长轮询收消息循环。游标仅非空才更新（对齐 openclaw：空游标会把进度打回去）。
    /// -14 → 暂停一小时重试（不清凭据），连续 SESSION_EXPIRED_MAX_RETRY 次仍过期
    /// 才回落等待扫码。
    /// 其它失败节律**对齐官方 `@tencent-weixin/openclaw-weixin@2.4.10`
    /// `dist/src/monitor/monitor.js`**：单次失败等 [`RETRY_DELAY_MS`]（2s），
    /// 连续 [`MAX_CONSECUTIVE_FAILURES`]（3）次退避 [`BACKOFF_DELAY_MS`]（30s）
    /// 并把计数清零（不是指数增长）。成功一轮计数归零。
    async fn poll_loop(self: Arc<Self>) {
        let mut expired_streak: u32 = 0;
        let mut consecutive_failures: u32 = 0;
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
                            expired_streak += 1;
                            if expired_streak >= SESSION_EXPIRED_MAX_RETRY {
                                self.set_note("登录已连续过期，请重新扫码".to_string())
                                    .await;
                                let mut inner = self.inner.lock().await;
                                inner.phase = "awaiting_scan".into();
                                return;
                            }
                            self.set_note(format!(
                                "登录过期（第 {expired_streak} 次），一小时后自动重试"
                            ))
                            .await;
                            let mut inner = self.inner.lock().await;
                            inner.phase = "session_expired".into();
                            drop(inner);
                            tokio::time::sleep(SESSION_EXPIRED_RETRY).await;
                            self.inner.lock().await.phase = "running".into();
                            continue;
                        }
                        // 官方口径：API 错误与网络错误同一节律（monitor.js 的 catch 分支）
                        consecutive_failures += 1;
                        self.set_note(format!("getupdates 错误: {msg}")).await;
                        let delay = monitor_failure_delay(consecutive_failures);
                        tracing::warn!(
                            failures = consecutive_failures,
                            delay_ms = delay.as_millis() as u64,
                            "微信全局通道 getupdates 失败，按官方节律重试"
                        );
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    expired_streak = 0;
                    consecutive_failures = 0;
                    // 服务端返回空游标说明没有推进，存空串会把进度打回去
                    let new_cursor = str_of(&out, &["get_updates_buf"]);
                    let timeout_ms = out
                        .get("longpolling_timeout_ms")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(30_000);
                    {
                        let mut inner = self.inner.lock().await;
                        if !new_cursor.is_empty() {
                            inner.cursor = new_cursor;
                        }
                    }
                    if let Some(msgs) = out.get("msgs").and_then(|v| v.as_array()) {
                        for m in msgs {
                            self.handle_msg(m).await;
                        }
                    }
                    // 服务端没消息时按下发超时空转等待；**有消息也不能裸奔连打**
                    //（用户口径 2026-10-16：批间至少隔 2s，借用官方 RETRY_DELAY 档）。
                    // 全局通道承载所有绑定码与管理员命令，压到 1 分钟会让用户等码，不能照搬
                    // 扫码账号的 60s 下限；单条流 + 人的手速，2s 批间隔足够防占用。
                    if msgs_empty(&out) {
                        tokio::time::sleep(Duration::from_millis(timeout_ms.min(35_000))).await;
                    } else {
                        tokio::time::sleep(RETRY_DELAY).await;
                    }
                }
                Err(e) => {
                    // 超时/网络抖动按官方节律处理（monitor.js：失败 +1，连续 3 次退避 30s）
                    consecutive_failures += 1;
                    let delay = monitor_failure_delay(consecutive_failures);
                    self.set_note(format!("轮询失败: {e}")).await;
                    tracing::warn!(
                        failures = consecutive_failures,
                        delay_ms = delay.as_millis() as u64,
                        "微信全局通道轮询失败，按官方节律重试"
                    );
                    tokio::time::sleep(delay).await;
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
        // item_list：type 1 文本（照旧）；type 2 图 / 3 语音 / 4 文件 / 5 视频（1.0.1）
        // 解出 CDN 地址与文件名附到 inbound.attachments（媒体只有文件类才能进导入，
        // 图片/语音/视频管理员发来时走同一条下载链路，普通用户会被权限提示拦住）。
        let mut text = String::new();
        let mut attachments: Vec<Attachment> = Vec::new();
        if let Some(items) = m.get("item_list").and_then(|v| v.as_array()) {
            for it in items {
                match it.get("type").and_then(|v| v.as_i64()) {
                    Some(1) => {
                        if text.is_empty() {
                            if let Some(t) = it.pointer("/text_item/text").and_then(|v| v.as_str())
                            {
                                text = t.to_string();
                            }
                        }
                    }
                    Some(t @ 2..=5) => {
                        if let Some(att) = parse_media_item(it, t) {
                            attachments.push(att);
                        }
                    }
                    _ => {}
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
            attachments,
        };
        if let Some(out) = BotRouter::handle(&self.st, &self.cfg, inbound).await {
            if let Err(e) = self.send(&out).await {
                self.set_note(format!("发送失败: {e}")).await;
                tracing::warn!(error = %e, "微信消息发送失败");
            }
        }
    }

    async fn send(&self, out: &Outbound) -> anyhow::Result<()> {
        /* 2026-10-17 本地灰度：这是管理员全局机器人通道（不是用户扫码账号），
        会话对不上 PUSH_OUT_ALLOW 白名单的账号名 —— 白名单非空时一律拦截
        （灰度期只放白名单里的扫码账号，宁可少发）。 */
        let preview: String = out.text.chars().take(120).collect();
        if !crate::push::push_out_enabled() {
            tracing::warn!(
                conversation = %out.conversation_id, preview = %preview,
                "【测试阶段】已拦截微信回复发送（未发）；要真实发送请设 PUSH_OUT_ENABLED=1"
            );
            return Ok(());
        }
        if crate::push::push_out_allow_nonempty() {
            tracing::warn!(
                conversation = %out.conversation_id, preview = %preview,
                "【本地灰度】白名单只对微信扫码账号生效，全局机器人回复已拦截（未发）"
            );
            return Ok(());
        }
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
        /* 2026-10-16：测试阶段出网闸门（与 user_send_text 同一开关）。
        这条是管理员全局机器人通道（旧手机号/绑定码绑定的人走这里）。
        2026-10-17 本地灰度：白名单非空时全局机器人通道一律拦截（对不上账号名）。 */
        let preview: String = text.chars().take(120).collect();
        if !crate::push::push_out_enabled() {
            tracing::warn!(
                conversation_id, preview = %preview,
                "【测试阶段】已拦截微信主动推送（未发）；要真实发送请设 PUSH_OUT_ENABLED=1"
            );
            return Ok(());
        }
        if crate::push::push_out_allow_nonempty() {
            tracing::warn!(
                conversation_id, preview = %preview,
                "【本地灰度】白名单只对微信扫码账号生效，全局机器人主动推送已拦截（未发）"
            );
            return Ok(());
        }
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
        // 对齐 openclaw：缺 context_token 仍尝试发送（服务端允许），但记警告
        if context_token.is_empty() {
            tracing::warn!("sendmessage 缺少 context_token，仍按协议尝试发送");
        }
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
        // sendmessage：缺 ret 不报错（对齐 openclaw —— 部分成功响应不带 ret）
        if resp.get("ret").is_some() || resp.get("errcode").is_some() {
            Self::check_code(&resp)?;
        }
        self.inner.lock().await.sent += 1;
        Ok(())
    }

    async fn set_note(&self, note: impl Into<String> + Send) {
        self.inner.lock().await.note = note.into();
    }
}

/* ------------------------------ 媒体（入站解析） ------------------------------ */

/// 入站媒体 item → 附件（复用 Attachment：url + file_name，与 QQ 附件同一条下载链路）。
/// CDN 解密下载（AES-128-ECB PKCS#7）由统一的 download_text 链路处理与否在 1.0.1 里
/// 只到「识别 + 给出直链」这一步：文本命令不受影响，管理员发 CSV 文件能进导入。
fn parse_media_item(it: &Value, ty: i64) -> Option<Attachment> {
    let node = match ty {
        2 => "image_item",
        3 => "voice_item",
        4 => "file_item",
        5 => "video_item",
        _ => return None,
    };
    let name = it
        .pointer(&format!("/{node}/file_name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let full_url = it
        .pointer("/media/full_url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let enc = it
        .pointer("/media/encrypt_query_param")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    // 都没有 → 结构不合预期，丢弃但不报错
    if full_url.is_empty() && enc.is_empty() {
        return None;
    }
    Some(Attachment {
        url: if full_url.is_empty() {
            format!(
                "{CDN_BASE_URL}/download?encrypted_query_param={}",
                urlencode(enc)
            )
        } else {
            full_url.to_string()
        },
        file_name: if name.is_empty() {
            "media".into()
        } else {
            name
        },
    })
}

/* ------------------------------ 小工具 ------------------------------ */

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

/// 主机是否可信：精确命中受信任列表，或是 *.weixin.qq.com 官方子域
///（confirmed 的 baseurl / redirect_host 会给出官方机器列表里的其它主机）。
fn is_trusted_host(url_or_host: &str) -> bool {
    let host = url_host(url_or_host);
    if host.is_empty() {
        return false;
    }
    TRUSTED_HOSTS.iter().any(|h| host == *h) || host.ends_with(".weixin.qq.com")
}

fn url_host(u: &str) -> String {
    u.split("://")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .unwrap_or(u)
        .split('/')
        .next()
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

/// X-WECHAT-UIN：4 字节随机数十进制再 base64（对齐 openclaw / Go 版）
fn random_uin() -> String {
    let n: u32 = rand_u32();
    let s = n.to_string();
    b64(s.as_bytes())
}

/// iLink-App-ClientVersion：0x00MMNNPP 打包后十进制（对齐 openclaw）
fn client_version_decimal() -> String {
    let (mm, nn, pp) = CLIENT_VERSION_MMNNPP;
    ((mm << 16) | (nn << 8) | pp).to_string()
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
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// 供 store::wxmedia 等外部模块复用的响应码检查（包装 impl 内同逻辑）
pub(crate) fn check_code(payload: &Value) -> anyhow::Result<()> {
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
    anyhow::bail!("微信接口错误 {code}: {msg}")
}

pub(crate) fn client_id() -> String {
    let n = rand_u32();
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("c-{ms}-{}", n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_version_matches_protocol_packing() {
        // 0x0002040A = 2.4.10 → 132106
        assert_eq!(client_version_decimal(), "132106");
    }

    #[test]
    fn trusted_hosts_allow_official_subdomains_only() {
        assert!(is_trusted_host("https://ilinkai.weixin.qq.com"));
        assert!(is_trusted_host("ilinkai.weixin.qq.com"));
        assert!(is_trusted_host("https://foo.weixin.qq.com/x"));
        assert!(!is_trusted_host("https://evil.example.com"));
        assert!(!is_trusted_host("weixin.qq.com.evil.com"));
        assert!(!is_trusted_host(""));
    }

    #[test]
    fn route_url_keeps_base_and_query() {
        assert_eq!(
            WxTransport::route_url("https://ilinkai.weixin.qq.com", "getupdates", &[]),
            "https://ilinkai.weixin.qq.com/ilink/bot/getupdates"
        );
        assert_eq!(
            WxTransport::route_url(
                "https://ilinkai.weixin.qq.com",
                "get_qrcode_status",
                &[("qrcode", "a b/c")]
            ),
            "https://ilinkai.weixin.qq.com/ilink/bot/get_qrcode_status?qrcode=a%20b%2Fc"
        );
    }

    #[test]
    fn check_code_tolerates_missing_ret() {
        // sendmessage 成功响应可能不带 ret —— 不报错
        assert!(WxTransport::check_code(&json!({})).is_ok());
        assert!(WxTransport::check_code(&json!({"ret": 0})).is_ok());
        assert!(WxTransport::check_code(&json!({"errcode": -14})).is_err());
        assert!(WxTransport::check_code(&json!({"ret": 5, "errmsg": "x"})).is_err());
    }

    #[test]
    fn parse_media_item_builds_download_url() {
        let it = json!({
            "type": 2,
            "image_item": { "file_name": "a.png" },
            "media": { "encrypt_query_param": "ENC/ABC", "aes_key": "k" }
        });
        let att = parse_media_item(&it, 2).unwrap();
        assert_eq!(att.file_name, "a.png");
        assert_eq!(
            att.url,
            format!("{CDN_BASE_URL}/download?encrypted_query_param=ENC%2FABC")
        );

        let it2 = json!({
            "type": 4,
            "file_item": { "file_name": "x.csv" },
            "media": { "full_url": "https://novac2c.cdn.weixin.qq.com/c2c/full" }
        });
        let att2 = parse_media_item(&it2, 4).unwrap();
        assert_eq!(att2.file_name, "x.csv");
        assert_eq!(att2.url, "https://novac2c.cdn.weixin.qq.com/c2c/full");

        // 结构不合预期 → None，不崩
        assert!(parse_media_item(&json!({"type": 2}), 2).is_none());
        // 没有文件名 → 兜底名
        let it3 = json!({
            "type": 5,
            "video_item": {},
            "media": { "full_url": "https://x.weixin.qq.com/v" }
        });
        assert_eq!(parse_media_item(&it3, 5).unwrap().file_name, "media");
    }
}
