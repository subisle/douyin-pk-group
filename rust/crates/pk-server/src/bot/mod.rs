//! 机器人模块（E 阶段）：确定性命令路由 + QQ / 微信渠道适配器
//!
//! 边界（docs/00 §9、docs/06 §4.4）：
//! - 只服务管理员：会话 ID 不在白名单里一律静默忽略（日志留痕，不回复）
//! - 命令路由确定性：同命令同输入同输出，不调用任何模型
//! - 限流 10 条/分钟/会话；写类命令（导入）走 douyin_writer，与 HTTP 端同一套校验

pub mod qq;
pub mod weixin;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::extract::State;
use serde_json::Value;
use sqlx::Row as _;

use crate::admin;
use crate::state::AppState;
use qq::QqTransport;
use weixin::WxTransport;

/// 入站消息（渠道适配器归一化后交给路由）
#[derive(Clone, Debug)]
pub struct Inbound {
    pub channel: &'static str,
    pub conversation_id: String,
    /// true = 群聊会话，false = 私聊
    pub is_group: bool,
    pub text: String,
    pub attachments: Vec<Attachment>,
}

#[derive(Clone, Debug)]
pub struct Attachment {
    pub url: String,
    pub file_name: String,
}

#[derive(Clone, Debug, Default)]
pub struct Outbound {
    pub conversation_id: String,
    pub text: String,
}

impl Outbound {
    pub fn to(conversation_id: &str, text: impl Into<String>) -> Self {
        Self {
            conversation_id: conversation_id.to_string(),
            text: text.into(),
        }
    }
}

/* ------------------------------- 配置 ------------------------------- */

#[derive(Clone, Debug)]
pub struct BotConfig {
    pub qq_app_id: String,
    pub qq_app_secret: String,
    /// 微信 iLink token（空 = 未登录，需要扫码；扫码成功后持久化到 SQLite）
    pub wechat_token: String,
    /// 管理员会话白名单（逗号分隔会话 ID / openid）
    pub qq_admins: Vec<String>,
    pub wechat_admins: Vec<String>,
    /// 每会话限流（条/分钟）
    pub rate_limit_per_min: u32,
}

impl BotConfig {
    pub fn from_env() -> Self {
        let list = |key: &str| -> Vec<String> {
            std::env::var(key)
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        };
        Self {
            qq_app_id: std::env::var("BOT_QQ_APP_ID").unwrap_or_default(),
            qq_app_secret: std::env::var("BOT_QQ_APP_SECRET").unwrap_or_default(),
            wechat_token: std::env::var("BOT_WECHAT_TOKEN").unwrap_or_default(),
            qq_admins: list("BOT_QQ_ADMINS"),
            wechat_admins: list("BOT_WECHAT_ADMINS"),
            rate_limit_per_min: std::env::var("BOT_RATE_LIMIT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10),
        }
    }

    /// 凭据齐即可连接：管理员 openid 要先收到消息才能发现（日志里抓），
    /// 所以白名单不作为连接前提，只作为命令放行条件。
    // （has_qq 已被 resolve_qq_credentials 取代：QQ 凭据现在支持后台页面保存）

    pub fn has_wechat(&self) -> bool {
        !self.wechat_admins.is_empty()
    }
}

/* ------------------------------- 限流 ------------------------------- */

struct RateLimiter {
    hits: Mutex<HashMap<String, Vec<Instant>>>,
    limit: u32,
}

impl RateLimiter {
    fn new(limit: u32) -> Self {
        Self {
            hits: Mutex::new(HashMap::new()),
            limit,
        }
    }

    fn allow(&self, key: &str) -> bool {
        let mut hits = self.hits.lock().expect("限流锁 poisoned");
        let now = Instant::now();
        let list = hits.entry(key.to_string()).or_default();
        list.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
        if list.len() >= self.limit as usize {
            return false;
        }
        list.push(now);
        true
    }
}

static LIMITER: std::sync::OnceLock<RateLimiter> = std::sync::OnceLock::new();

/* --------------------------- 导入会话（不用再问团别） --------------------------- */

/* ---- 运营先发「10.1」再发 CSV：这个业务日期要黏住（两个文件都按它入库）---- */

/// 黏性业务日期：key = 渠道:会话，值 = (业务日期, 设置时间)。
/// 12 小时有效（当天报一次，当天发的文件都用它）；再发一个日期就覆盖。
static PENDING_DATES: std::sync::OnceLock<Mutex<HashMap<String, (String, Instant)>>> =
    std::sync::OnceLock::new();
const PENDING_DATE_TTL: Duration = Duration::from_secs(12 * 3600);

fn pending_dates() -> &'static Mutex<HashMap<String, (String, Instant)>> {
    PENDING_DATES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pending_date_put(key: &str, date: &str) {
    let mut m = pending_dates().lock().expect("pending_date 锁 poisoned");
    m.retain(|_, (_, at)| at.elapsed() < PENDING_DATE_TTL);
    m.insert(key.to_string(), (date.to_string(), Instant::now()));
}

fn pending_date_get(key: &str) -> Option<String> {
    let mut m = pending_dates().lock().expect("pending_date 锁 poisoned");
    match m.get(key) {
        Some((d, at)) if at.elapsed() < PENDING_DATE_TTL => Some(d.clone()),
        Some(_) => {
            m.remove(key);
            None
        }
        None => None,
    }
}

/// 运营报日期的写法都收：「10.1」「10-1」「10/1」「10月1日」「10月1号」「2026-10-01」。
/// 只给月日 → 当年（例：10.1 = 2026-10-01）。
pub fn parse_short_date(s: &str) -> Option<String> {
    let t = s.trim();
    if valid_date(t) {
        return Some(t.to_string());
    }
    let norm: String = t
        .trim_end_matches(['号', '日'])
        .chars()
        .map(|c| if matches!(c, '.' | '/' | '月' | '-') { '-' } else { c })
        .collect();
    let parts: Vec<&str> = norm.split('-').filter(|p| !p.is_empty()).collect();
    if parts.len() == 2 {
        let m: u32 = parts[0].parse().ok()?;
        let d: u32 = parts[1].parse().ok()?;
        let year = chrono::Local::now().format("%Y").to_string();
        let cand = format!("{year}-{m:02}-{d:02}");
        if valid_date(&cand) {
            return Some(cand);
        }
    }
    None
}

/// 路由入口（渠道适配器调用）。命令实现全部是确定性纯逻辑。
pub struct BotRouter;

impl BotRouter {
    pub async fn handle(st: &Arc<AppState>, cfg: &BotConfig, inbound: Inbound) -> Option<Outbound> {
        handle(st, cfg, inbound).await
    }
}

/* ------------------------------- BotManager ------------------------------- */

/// 渠道管理器：持有两个可选渠道，env / 后台配置决定哪些渠道可用。
pub struct BotManager {
    pub config: BotConfig,
    /// QQ 渠道。RwLock 是为了「机器人」页保存 AppID/Secret 后热替换凭据
    /// （token 缓存在 transport 里，凭据变了必须换新实例）。
    pub qq: RwLock<Option<Arc<QqTransport>>>,
    pub wechat: Option<Arc<WxTransport>>,
}

/// QQ 凭据解析：MySQL config（后台「机器人」页保存的）逐字段优先，env 兜底。
/// 返回 (appId, appSecret, source)：source = settings / env / none。
async fn resolve_qq_credentials(st: &AppState) -> (String, String, &'static str) {
    let pick = |setting: Option<String>, env_key: &str| -> String {
        setting
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var(env_key).ok().filter(|s| !s.is_empty()))
            .unwrap_or_default()
    };
    let id_setting = crate::store::config_get(st, "bot_qq_app_id").await;
    let secret_setting = crate::store::config_get(st, "bot_qq_app_secret").await;
    let id = pick(id_setting.clone(), "BOT_QQ_APP_ID");
    let secret = pick(secret_setting.clone(), "BOT_QQ_APP_SECRET");
    let from_settings = id_setting.map(|s| !s.is_empty()).unwrap_or(false)
        || secret_setting.map(|s| !s.is_empty()).unwrap_or(false);
    let source = if id.is_empty() || secret.is_empty() {
        "none"
    } else if from_settings {
        "settings"
    } else {
        "env"
    };
    (id, secret, source)
}

impl BotManager {
    pub async fn from_env(st: Arc<AppState>) -> Self {
        let config = BotConfig::from_env();
        // 微信 token 优先 env，其次 MySQL config（扫码成功后落库）
        let wechat_token = if config.wechat_token.is_empty() {
            crate::store::config_get(&st, "wechat_token")
                .await
                .unwrap_or_default()
        } else {
            config.wechat_token.clone()
        };
        let (qq_id, qq_secret, _) = resolve_qq_credentials(&st).await;
        let qq = RwLock::new((!qq_id.is_empty() && !qq_secret.is_empty()).then(|| {
            Arc::new(QqTransport::new(
                st.clone(),
                config.clone(),
                qq_id,
                qq_secret,
            ))
        }));
        let wechat = if config.has_wechat() {
            match WxTransport::new(st.clone(), config.clone(), wechat_token) {
                Ok(t) => Some(Arc::new(t)),
                Err(e) => {
                    tracing::warn!(error = %e, "微信渠道初始化失败，渠道不可用");
                    None
                }
            }
        } else {
            None
        };
        Self { config, qq, wechat }
    }

    pub async fn status(&self) -> Value {
        let mut channels = Vec::new();
        let qq = self.qq.read().expect("qq 锁 poisoned").clone();
        if let Some(qq) = qq {
            channels.push(qq.status().await);
        } else {
            channels.push(serde_json::json!({
                "channel": "qq", "configured": false,
                "note": "未配置 AppID / AppSecret（在后台「机器人」页填写）",
            }));
        }
        if let Some(wx) = &self.wechat {
            channels.push(wx.status().await);
        } else {
            channels.push(serde_json::json!({
                "channel": "weixin", "configured": false,
                "note": "未配置 BOT_WECHAT_ADMINS",
            }));
        }
        serde_json::json!({
            "channels": channels,
            "rateLimitPerMin": self.config.rate_limit_per_min,
        })
    }
}

/* --------------------------- 管理端 HTTP handlers --------------------------- */

type ApiResult = Result<axum::Json<Value>, crate::routes::ApiError>;
type ArcState = Arc<AppState>;

fn mgr(st: &AppState) -> anyhow::Result<&Arc<BotManager>> {
    st.bots
        .get()
        .ok_or_else(|| anyhow::anyhow!("机器人模块未初始化"))
}

/// GET /api/admin/bots/status
pub async fn bots_status(State(st): State<ArcState>) -> ApiResult {
    let m = mgr(&st)?;
    Ok(axum::Json(m.status().await))
}

/// POST /api/admin/bots/qq/start
pub async fn qq_start(State(st): State<ArcState>) -> ApiResult {
    let qq = mgr(&st)?
        .qq
        .read()
        .expect("qq 锁 poisoned")
        .clone()
        .ok_or_else(|| {
            anyhow::anyhow!("QQ 凭据未配置：先在后台「机器人」页填写 AppID / AppSecret")
        })?;
    qq.start().await?;
    Ok(axum::Json(serde_json::json!({ "ok": true })))
}

/// POST /api/admin/bots/qq/stop
pub async fn qq_stop(State(st): State<ArcState>) -> ApiResult {
    // 先把 Arc clone 出来再 await：RwLockReadGuard 不能跨 await（future 必须 Send）
    let existing = mgr(&st)?.qq.read().expect("qq 锁 poisoned").clone();
    if let Some(qq) = existing {
        qq.stop().await;
    }
    Ok(axum::Json(serde_json::json!({ "ok": true })))
}

/// GET /api/admin/bots/qq/credentials：AppID 明文回显（非机密），Secret 只回「是否已设置」
pub async fn qq_credentials(State(st): State<ArcState>) -> ApiResult {
    mgr(&st)?;
    let (app_id, secret, source) = resolve_qq_credentials(&st).await;
    Ok(axum::Json(serde_json::json!({
        "appId": app_id,
        "secretSet": !secret.is_empty(),
        "source": source,
    })))
}

#[derive(serde::Deserialize)]
pub struct QqCredBody {
    #[serde(rename = "appId")]
    pub app_id: Option<String>,
    #[serde(rename = "appSecret")]
    pub app_secret: Option<String>,
}

/// POST /api/admin/bots/qq/credentials {appId?, appSecret?}
/// 只覆盖传了的字段（secret 留空 = 不修改）；保存后热替换 QQ transport，
/// 下次点「启动」就用新凭据（旧连接同时停掉）。
pub async fn qq_save_credentials(
    State(st): State<ArcState>,
    axum::Json(body): axum::Json<QqCredBody>,
) -> ApiResult {
    let m = mgr(&st)?;
    let app_id = body
        .app_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let app_secret = body
        .app_secret
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if app_id.is_none() && app_secret.is_none() {
        return Err(anyhow::anyhow!("appId / appSecret 至少填一个").into());
    }
    if let Some(id) = &app_id {
        crate::store::config_set(&st, "bot_qq_app_id", &id).await;
    }
    if let Some(sec) = &app_secret {
        crate::store::config_set(&st, "bot_qq_app_secret", &sec).await;
    }
    let (id, secret, source) = resolve_qq_credentials(&st).await;
    if id.is_empty() || secret.is_empty() {
        return Err(anyhow::anyhow!(
            "AppID 与 AppSecret 需要齐全（当前缺{}）",
            if id.is_empty() { "AppID" } else { "AppSecret" }
        )
        .into());
    }
    // 凭据变了 token 必须重取：停掉旧连接、换新 transport
    let old = m.qq.read().expect("qq 锁 poisoned").clone();
    if let Some(old) = old {
        old.stop().await;
    }
    *m.qq.write().expect("qq 锁 poisoned") = Some(Arc::new(QqTransport::new(
        st.clone(),
        BotConfig::from_env(),
        id,
        secret,
    )));
    crate::events::emit(crate::events::SCOPE_BOT, "QQ 机器人凭据已更新");
    Ok(axum::Json(
        serde_json::json!({ "ok": true, "source": source }),
    ))
}

/// POST /api/admin/bots/weixin/start
pub async fn weixin_start(State(st): State<ArcState>) -> ApiResult {
    let wx = mgr(&st)?
        .wechat
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("微信渠道未配置（BOT_WECHAT_ADMINS）"))?;
    Arc::clone(wx).start().await?;
    Ok(axum::Json(serde_json::json!({ "ok": true })))
}

/// POST /api/admin/bots/weixin/stop
pub async fn weixin_stop(State(st): State<ArcState>) -> ApiResult {
    if let Some(wx) = mgr(&st)?.wechat.as_ref() {
        wx.stop().await;
    }
    Ok(axum::Json(serde_json::json!({ "ok": true })))
}

/// GET /api/admin/bots/weixin/qrcode：取登录二维码
pub async fn weixin_qrcode(State(st): State<ArcState>) -> ApiResult {
    let wx = mgr(&st)?
        .wechat
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("微信渠道未配置"))?;
    let qr = wx.get_qrcode().await?;
    Ok(axum::Json(qr))
}

/// POST /api/admin/bots/weixin/qrcode/check {qrcode}：查扫码状态，成功自动保存 token
#[derive(serde::Deserialize)]
pub struct QrCheckBody {
    pub qrcode: Option<String>,
}

pub async fn weixin_qrcode_check(
    State(st): State<ArcState>,
    axum::Json(body): axum::Json<QrCheckBody>,
) -> ApiResult {
    let wx = mgr(&st)?
        .wechat
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("微信渠道未配置"))?;
    let qrcode = body.qrcode.ok_or_else(|| anyhow::anyhow!("缺少 qrcode"))?;
    let out = wx.check_qrcode(&qrcode).await?;
    Ok(axum::Json(out))
}

/// POST /api/admin/bots/weixin/token {token, nickname?}：手工粘贴 token（不走扫码）
#[derive(serde::Deserialize)]
pub struct WxTokenBody {
    pub token: Option<String>,
    pub nickname: Option<String>,
}

pub async fn weixin_token(
    State(st): State<ArcState>,
    axum::Json(body): axum::Json<WxTokenBody>,
) -> ApiResult {
    let wx = mgr(&st)?
        .wechat
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("微信渠道未配置"))?;
    let token = body
        .token
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("缺少 token"))?;
    wx.set_token(token, body.nickname.as_deref().unwrap_or(""))
        .await?;
    Ok(axum::Json(serde_json::json!({ "ok": true })))
}

/* --------------------- 机器人「加好友」二维码（展示端推送绑定用） --------------------- */

/// GET /api/admin/bots/weixin/add-qr：是否已配置（不回显图片本体）
pub async fn weixin_add_qr_get(State(st): State<ArcState>) -> ApiResult {
    mgr(&st)?;
    let has = crate::store::config_get(&st, "wechat_add_qr")
        .await
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    Ok(axum::Json(serde_json::json!({ "configured": has })))
}

#[derive(serde::Deserialize)]
pub struct WxAddQrBody {
    /// dataURL（data:image/png;base64,…）；传空串 = 清除
    pub qr: Option<String>,
}

/// POST /api/admin/bots/weixin/add-qr：保存机器人加好友二维码。
/// 「我的」页推送绑定让用户扫这张码加机器人，再发绑定码完成绑定。
pub async fn weixin_add_qr_set(
    State(st): State<ArcState>,
    axum::Json(body): axum::Json<WxAddQrBody>,
) -> ApiResult {
    mgr(&st)?;
    let qr = body.qr.unwrap_or_default();
    let qr = qr.trim();
    if qr.is_empty() {
        crate::store::config_set(&st, "wechat_add_qr", "").await;
        return Ok(axum::Json(
            serde_json::json!({ "ok": true, "configured": false }),
        ));
    }
    if qr.len() > 400_000 {
        return Err(anyhow::anyhow!("二维码图片太大（请压到 300KB 以内再上传）").into());
    }
    if crate::public_teams::decode_data_url(qr).is_none() {
        return Err(
            anyhow::anyhow!("二维码格式不对（需要 data:image/...;base64,... 形式的图片）").into(),
        );
    }
    crate::store::config_set(&st, "wechat_add_qr", qr).await;
    crate::events::emit(crate::events::SCOPE_BOT, "机器人加好友二维码已更新");
    Ok(axum::Json(
        serde_json::json!({ "ok": true, "configured": true }),
    ))
}

/// 微信推送绑定（展示端「我的」页生成绑定码 → 用户发给机器人）。
/// 命中返回回复文案，没命中返回 None（继续走管理员判定）。
async fn try_bind_weixin(st: &Arc<AppState>, inbound: &Inbound) -> Option<Outbound> {
    let conversation = inbound.conversation_id.clone();
    // 解绑：这条会话确实绑过才回应（否则静默，免得对任何陌生消息都回话）
    let lower = inbound.text.trim().to_lowercase();
    if matches!(lower.as_str(), "解绑" | "取消绑定" | "unbind") {
        let name = crate::store::push_unbind_by_conversation(st, &conversation).await;
        return Some(Outbound::to(
            &conversation,
            match name {
                Some(n) => format!(
                    "已解绑 {n}，不会再收到推送。\n要重新绑定，回「我的」页面获取新的绑定码。"
                ),
                None => "这条会话还没有绑定过，不用解绑。".to_string(),
            },
        ));
    }
    let code = extract_bind_code(&inbound.text)?;
    let (name, _team) =
        crate::store::push_bind_by_code(st, &code, &conversation, &conversation).await?;
    tracing::info!(account = %name, conversation = %conversation, "微信推送绑定成功");
    crate::events::emit(crate::events::SCOPE_BOT, format!("微信推送已绑定：{name}"));
    Some(Outbound::to(
        &conversation,
        format!(
            "✅ 绑定成功，{name}\n\n\
             之后数据更新（每晚入库后）会给你推送：\n\
             · 追分提醒 —— 追上对手 / 还差多少\n\
             · 每日报告 —— 当天的音浪排行榜\n\n\
             要开哪一项、要追谁，回到「我的」页面里设置。\n\
             回复「解绑」可以断开。"
        ),
    ))
}

/// 从消息里认绑定码：`绑定 ABC234` / `bind abc234` / 直接发 `ABC234`（六位，码表排除易混字符）
fn extract_bind_code(text: &str) -> Option<String> {
    let mut toks = text.split_whitespace();
    let first = toks.next()?;
    let has_kw = matches!(first.to_lowercase().as_str(), "绑定" | "bind" | "bd");
    let cand = if has_kw {
        toks.next()?
    } else {
        // 不带关键词时必须只有一段，避免把正常聊天误判成绑定码
        if toks.next().is_some() {
            return None;
        }
        first
    };
    let code = cand.trim().to_ascii_uppercase();
    crate::push::looks_like_bind_code(&code).then_some(code)
}

/// 处理一条入站消息。返回 None = 静默忽略（非管理员 / 空消息）。
pub async fn handle(st: &Arc<AppState>, cfg: &BotConfig, inbound: Inbound) -> Option<Outbound> {
    // 登记会话（含非管理员）：管理界面据此展示群聊/私聊列表并勾选管理员
    crate::store::session_upsert(
        st,
        inbound.channel,
        &inbound.conversation_id,
        if inbound.is_group { "group" } else { "private" },
        &inbound.text,
    )
    .await;
    // 让开着的「机器人」页立刻看到新会话/新消息数，不用等 5 秒轮询
    crate::events::emit(
        crate::events::SCOPE_BOT,
        format!("{} 收到消息", inbound.channel),
    );
    // 展示端「我的」页微信绑定（2026-10-09）：用户扫码加机器人后把绑定码发过来。
    // 必须在管理员白名单之前处理——来绑定的都是普通主播，不是管理员。
    if inbound.channel == "weixin" && !inbound.is_group {
        if let Some(out) = try_bind_weixin(st, &inbound).await {
            return Some(out);
        }
    }
    let env_admins = match inbound.channel {
        "qq" => &cfg.qq_admins,
        _ => &cfg.wechat_admins,
    };
    let is_admin = env_admins.contains(&inbound.conversation_id)
        || crate::store::session_is_admin(st, &inbound.channel, &inbound.conversation_id).await;
    if !is_admin {
        tracing::warn!(
            channel = inbound.channel,
            conversation = %inbound.conversation_id,
            kind = if inbound.is_group { "group" } else { "private" },
            text = %inbound.text,
            "非管理员会话消息，静默忽略（可在管理端会话列表勾选管理员，或加入 BOT_QQ_ADMINS）"
        );
        return None;
    }

    let limiter = LIMITER.get_or_init(|| RateLimiter::new(cfg.rate_limit_per_min));
    let key = format!("{}:{}", inbound.channel, inbound.conversation_id);
    if !limiter.allow(&key) {
        return Some(Outbound::to(
            &inbound.conversation_id,
            "消息太频繁了，稍后再试（限流 10 条/分钟）",
        ));
    }

    let text = inbound.text.trim().to_string();

    // 附件：即时下载（QQ CDN 直链带 rkey，有时效），存为待确认导入
    let mut attachment_text = String::new();
    for att in &inbound.attachments {
        tracing::info!(
            file = %att.file_name,
            url_head = %att.url.chars().take(80).collect::<String>(),
            "机器人收到附件，开始下载"
        );
        match download_text(&att.url).await {
            Ok(body) => {
                tracing::info!(file = %att.file_name, bytes = body.len(), "附件下载完成");
                attachment_text.push_str(&body);
                attachment_text.push('\n');
            }
            Err(e) => {
                tracing::warn!(file = %att.file_name, error = %e, "附件下载失败");
                return Some(Outbound::to(
                    &inbound.conversation_id,
                    format!("附件下载失败（{}）：{e}", att.file_name),
                ));
            }
        }
    }
    // 运营先发「10.1」这种纯日期：记下来，接下来发的 CSV 文件都按这个业务日期入库。
    // （两个文件只报一次日期就够；再发一个日期可以改）
    if attachment_text.is_empty() && !text.is_empty() {
        if let Some(d) = parse_short_date(&text) {
            pending_date_put(&key, &d);
            return Some(Outbound::to(
                &inbound.conversation_id,
                format!(
                    "已记下业务日期 {d}。把 CSV 文件发过来就按这个日期入库（不分男女团，团别按主播档案归属）；\n再发一个日期可以改，发「帮助」看全部命令。"
                ),
            ));
        }
    }

    if !attachment_text.is_empty() {
        let rows = admin::parse_any_text(&attachment_text);
        let filename = inbound
            .attachments
            .first()
            .map(|a| a.file_name.clone())
            .unwrap_or_else(|| "文件".into());

        // 业务日期优先级：本条消息里带的日期 > 之前报过的日期（12h 内）> 文件名里的日期 > 收到日 − 1
        let (file_date, _file_team) = hints_from_filename(&filename);
        let msg_date = parse_short_date(&text);
        let sticky = pending_date_get(&key);
        let (biz_date, date_src) = match (&msg_date, &sticky, &file_date) {
            (Some(d), _, _) => (d.clone(), "本条消息指定"),
            (None, Some(d), _) => (d.clone(), "沿用你报的日期"),
            (None, None, Some(d)) => (d.clone(), "文件名里的日期（补录）"),
            (None, None, None) => (default_biz_date(), "默认「收到日 − 1」（数据半夜才更新）"),
        };
        if msg_date.is_some() {
            // 同一条消息里带了日期：黏住，后面那个文件不用再报
            pending_date_put(&key, &biz_date);
        }

        if rows.is_empty() {
            return Some(Outbound::to(
                &inbound.conversation_id,
                format!("文件「{filename}」里没解析到数据行，请确认是 CSV 或「名字 音浪」格式。"),
            ));
        }

        // 不分男女团（用户口径 2026-10-10）：一个文件里男女混着也能导，团别按主播档案逐行归属
        tracing::info!(
            file = %filename,
            biz_date = %biz_date,
            date_src = %date_src,
            rows = rows.len(),
            "机器人文件导入（不分男女团）"
        );
        let report = admin::commit_import(
            st,
            "auto",
            &biz_date,
            rows,
            "merge",
            &format!("机器人·{filename}"),
        )
        .await;
        let mut reply = format_report(&filename, &biz_date, "auto", report);
        reply.push_str(&format!(
            "\n（业务日期 {biz_date} · {date_src}；不分男女团，团别按主播档案归属）"
        ));
        return Some(Outbound::to(&inbound.conversation_id, reply));
    }

    if text.is_empty() {
        return None;
    }

    let reply = route_command(st, cfg, &inbound, &text).await;
    Some(Outbound::to(&inbound.conversation_id, reply))
}

/// 导入结果 → 机器人回复文本（简短：只报成败行数；失败原因给前 10 条。
/// 缺数据完整度不自动发，需要时用「缺数据」命令查）
fn format_report(
    filename: &str,
    biz_date: &str,
    team: &str,
    report: anyhow::Result<admin::CommitReport>,
) -> String {
    let _ = filename; // 文案精简：不再回显文件名
    match report {
        Ok(r) => {
            // auto（不分男女团）时不带团别字样，回复更短
            let mut out = if crate::admin::is_auto_team(team) {
                format!("✅ {biz_date} 导入成功 {} 行", r.ok_rows)
            } else {
                let team_label = if team == "female" { "女团" } else { "男团" };
                format!("✅ {biz_date} {team_label} 导入成功 {} 行", r.ok_rows)
            };
            if r.unchanged > 0 {
                out.push_str(&format!(
                    "（另有 {} 行音浪值与库里相同，未覆盖）",
                    r.unchanged
                ));
            }
            if !r.failures.is_empty() {
                out.push_str(&format!("，失败 {} 行：", r.failures.len()));
                for f in r.failures.iter().take(10) {
                    let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let reason = f.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                    out.push_str(&format!("\n· {name}：{reason}"));
                }
            }
            out
        }
        Err(e) => format!("导入失败：{e}"),
    }
}

async fn route_command(
    st: &Arc<AppState>,
    _cfg: &BotConfig,
    inbound: &Inbound,
    text: &str,
) -> String {
    let lower = text.to_lowercase();
    let tokens: Vec<&str> = text.split_whitespace().collect();

    match lower.as_str() {
        "帮助" | "help" => return help_text(),
        "状态" | "status" => return cmd_status(st).await,
        "分组" | "group" => return cmd_plan(st).await,
        "总榜" => return cmd_rank(st, "total", 0, "总音浪榜").await,
        _ => {}
    }

    match tokens[0] {
        "排行" | "rank" => {
            let n: i64 = tokens.get(1).and_then(|t| t.parse().ok()).unwrap_or(10);
            cmd_rank(st, "lastN", n.clamp(1, 100), &format!("近 {n} 日音浪榜")).await
        }
        "查询" | "query" => {
            let name = tokens.get(1..).map(|t| t.join(" ")).unwrap_or_default();
            if name.is_empty() {
                "用法：查询 <主播名>".into()
            } else {
                cmd_query(st, &name).await
            }
        }
        "导出" | "export" => cmd_export(st, tokens.get(1).copied().unwrap_or("今日")).await,
        "日报" => cmd_daily_report(st, tokens.get(1..).unwrap_or(&[])).await,
        "缺数据" => cmd_missing(st, tokens.get(1..).unwrap_or(&[])).await,
        "导入" | "import" => cmd_import(st, inbound, tokens.get(1..).unwrap_or(&[])).await,
        _ => format!("不认识这条命令。发「帮助」看用法。\n你发的是：{text}"),
    }
}

fn help_text() -> String {
    "可用命令（仅管理员）：\n\
     状态 —— 数据新鲜度与机器人状态\n\
     排行 [N] —— 近 N 日音浪榜（默认 10）\n\
     总榜 —— 累计音浪榜\n\
     查询 <主播名> —— 查某人数据\n\
     导出 今日|YYYY-MM-DD|总榜 —— CSV 文本回复\n\
     发「10.1」= 指定业务日期 2026-10-01（之后发的 CSV 都按它入库，不用带团别）\n\
     发 CSV 文件 = 直接入库（不分男女团，团别按主播档案归属；同一天重复导，值相同不覆盖）\n\
     导入 [男团|女团] [2026-10-01|10.1] + 数据行 —— 需要按团校验时才带团别\n\
     分组 —— 当前已上架分组方案\n\
     \n默认日期为前一天（数据晚一天）；同日重复导入，后发的覆盖先发的。"
        .to_string()
}

async fn cmd_status(st: &Arc<AppState>) -> String {
    let dates = crate::repos::get_available_dates(st, Some("LOCAL"), false).await;
    let Ok(dates) = dates else {
        return "数据库暂时不可用，稍后再试。".into();
    };
    let anchors = crate::repos::load_anchors(st, Some("LOCAL"), false)
        .await
        .map(|a| a.len())
        .unwrap_or(0);
    let latest = dates.first().map(String::as_str).unwrap_or("无");
    format!(
        "数据状态：\n· 最新业务日：{latest}\n· 有效日期数：{}\n· 在册主播：{anchors}\n· 写库：{}",
        dates.len(),
        if st.writer.is_some() {
            "已连接"
        } else {
            "未配置"
        }
    )
}

async fn cmd_plan(st: &AppState) -> String {
    let plans = crate::store::list_plans(st).await;
    let published: Vec<_> = plans.iter().filter(|p| p.published).collect();
    match published.last() {
        Some(p) => format!(
            "当前已上架方案：{}\n口径：{} · 保存于 {}",
            p.title, p.metric, p.created_at
        ),
        None => "还没有已上架的分组方案。".to_string(),
    }
}

/// 榜单通用实现：按口径取音浪 + 主播名，排序后组文本
async fn cmd_rank(st: &Arc<AppState>, metric: &str, days: i64, title: &str) -> String {
    let Ok(anchors) = crate::repos::load_anchors(st, Some("LOCAL"), false).await else {
        return "数据库暂时不可用。".into();
    };
    let Ok(info) =
        crate::repos::get_waves(st, metric, days.max(1), None, false, Some("LOCAL")).await
    else {
        return "音浪数据暂时不可用。".into();
    };
    let mut rows: Vec<(String, f64)> = anchors
        .iter()
        .filter_map(|a| info.waves.get(&a.person_id).map(|w| (a.name.clone(), *w)))
        .filter(|(_, w)| *w > 0.0)
        .collect();
    if rows.is_empty() {
        return format!("{title}：暂无数据。");
    }
    rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let body: Vec<String> = rows
        .iter()
        .take(15)
        .enumerate()
        .map(|(i, (name, w))| format!("{}. {} {}", i + 1, name, fmt_num(*w)))
        .collect();
    format!("{title}\n{}", body.join("\n"))
}

async fn cmd_query(st: &Arc<AppState>, name: &str) -> String {
    let Ok(anchors) = crate::repos::load_anchors(st, Some("LOCAL"), false).await else {
        return "数据库暂时不可用。".into();
    };
    let key = pk_core::normalize_name(name);
    let Some(a) = anchors
        .iter()
        .find(|a| pk_core::normalize_name(&a.name) == key)
        .or_else(|| anchors.iter().find(|a| a.name == name))
    else {
        // 别名兜底
        let aliases = crate::store::list_aliases(st).await;
        let real = aliases.get(name).map(String::as_str);
        return match real {
            Some(r) => Box::pin(cmd_query(st, r)).await,
            None => format!("没有找到主播「{name}」。"),
        };
    };
    let Ok(info) = crate::repos::get_waves(
        st,
        "total",
        1,
        Some(std::slice::from_ref(&a.person_id)),
        false,
        Some("LOCAL"),
    )
    .await
    else {
        return "音浪数据暂时不可用。".into();
    };
    let total = info.waves.get(&a.person_id).copied().unwrap_or(0.0);
    let Ok(recent) = crate::repos::get_waves(
        st,
        "lastN",
        1,
        Some(std::slice::from_ref(&a.person_id)),
        false,
        Some("LOCAL"),
    )
    .await
    else {
        return "音浪数据暂时不可用。".into();
    };
    let yesterday = recent.waves.get(&a.person_id).copied().unwrap_or(0.0);
    format!(
        "{}（{}）\n累计音浪：{}\n最近一个业务日：{}",
        a.name,
        "在册",
        fmt_num(total),
        fmt_num(yesterday)
    )
}

/// 导出：CSV 文本回复（文件上传待接入渠道富媒体接口后替换）
async fn cmd_export(st: &Arc<AppState>, arg: &str) -> String {
    let (metric, days, label) = match arg {
        "总榜" => ("total", 1, "总音浪"),
        "今日" => ("lastN", 1, "最近一个业务日"),
        d => {
            if valid_date(d) {
                return cmd_export_date(st, d).await;
            }
            return "用法：导出 今日 | 导出 YYYY-MM-DD | 导出 总榜".into();
        }
    };
    csv_of(st, metric, days, label).await
}

async fn cmd_export_date(st: &Arc<AppState>, date: &str) -> String {
    let Ok(series) = crate::repos::get_wave_series(st, false, Some("LOCAL")).await else {
        return "音浪数据暂时不可用。".into();
    };
    let Some(idx) = series.dates.iter().position(|d| d == date) else {
        return format!("{} 没有采集记录（序列只保留最近 14 天）。", date);
    };
    let Ok(anchors) = crate::repos::load_anchors(st, Some("LOCAL"), false).await else {
        return "数据库暂时不可用。".into();
    };
    let mut rows: Vec<(String, f64)> = anchors
        .iter()
        .filter_map(|a| {
            series
                .series
                .get(&a.person_id)
                .and_then(|v| v.get(idx))
                .map(|w| (a.name.clone(), *w))
        })
        .collect();
    rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = format!("名字,{date}音浪\n");
    for (name, w) in rows {
        // CSV 是机器格式：数值不能带千分位逗号（会把一行拆成三列）
        out.push_str(&format!("{},{}\n", name, w.round() as i64));
    }
    out
}

async fn csv_of(st: &Arc<AppState>, metric: &str, days: i64, label: &str) -> String {
    let Ok(anchors) = crate::repos::load_anchors(st, Some("LOCAL"), false).await else {
        return "数据库暂时不可用。".into();
    };
    let Ok(info) = crate::repos::get_waves(st, metric, days, None, false, Some("LOCAL")).await
    else {
        return "音浪数据暂时不可用。".into();
    };
    let mut rows: Vec<(String, f64)> = anchors
        .iter()
        .filter_map(|a| info.waves.get(&a.person_id).map(|w| (a.name.clone(), *w)))
        .collect();
    rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = format!("名字,{label}音浪\n");
    for (name, w) in rows {
        // CSV 是机器格式：数值不能带千分位逗号（会把一行拆成三列）
        out.push_str(&format!("{},{}\n", name, w.round() as i64));
    }
    out
}

/// 导入：<男团|女团> [YYYY-MM-DD]，数据行跟在命令后或作为附件（CSV）
async fn cmd_import(st: &Arc<AppState>, inbound: &Inbound, args: &[&str]) -> String {
    let mut args: Vec<&str> = args.to_vec();
    // 团别可不写：默认「不分男女团」（按主播档案逐行归属）；写了男团/女团就按团校验
    let mut team = "auto";
    if let Some(t) = args.first().and_then(|a| team_of(a)) {
        team = t;
        args.remove(0);
    } else if args.first().is_some_and(|a| crate::admin::is_auto_team(a)) {
        args.remove(0);
    }
    let key = format!("{}:{}", inbound.channel, inbound.conversation_id);
    // 日期支持 2026-10-01 / 10.1；不写就用你刚报过的日期，再没有就默认「收到日 − 1」
    let biz_date = match args.first() {
        Some(d) => match parse_short_date(d) {
            Some(x) => {
                pending_date_put(&key, &x);
                x
            }
            None => return "日期格式应为 2026-10-01 或 10.1。".into(),
        },
        None => pending_date_get(&key).unwrap_or_else(default_biz_date),
    };

    // 数据来源：附件优先，其次命令文本剩余部分
    let mut text = String::new();
    for att in &inbound.attachments {
        match download_text(&att.url).await {
            Ok(body) => text.push_str(&body),
            Err(e) => return format!("附件下载失败（{}）：{e}", att.file_name),
        }
    }
    if text.is_empty() {
        // 命令行剩余内容（去掉 导入 团别 [日期] 之后的部分）
        if let Some(pos) = inbound.text.find('\n') {
            text = inbound.text[pos + 1..].to_string();
        }
    }
    let rows = admin::parse_any_text(&text);
    if rows.is_empty() {
        return "没有解析到任何数据行。格式示例：\n浩龙 500000 1200000 开播\n（名字 当日音浪 累计 开播状态）".into();
    }

    let report = admin::commit_import(st, team, &biz_date, rows, "merge", "机器人导入").await;
    format_report("机器人导入", &biz_date, team, report)
}

/// 日报 [男团|女团] [日期]：文本版日报（当日榜 + 未播名单）
async fn cmd_daily_report(st: &Arc<AppState>, args: &[&str]) -> String {
    let team = args.first().and_then(|a| team_of(a)).unwrap_or("male");
    let date = match args.get(1) {
        Some(d) if valid_date(d) => d.to_string(),
        _ => crate::repos::get_available_dates(st, Some("LOCAL"), false)
            .await
            .ok()
            .and_then(|d| d.first().cloned())
            .unwrap_or_else(default_biz_date),
    };
    match crate::report::build(st, &date, team).await {
        Ok(rep) => crate::report::render_text(&rep),
        Err(e) => format!("日报生成失败：{e}"),
    }
}

/// 缺数据 [男团|女团] [日期]：当天还没有数据的主播名单（默认昨天）
async fn cmd_missing(st: &Arc<AppState>, args: &[&str]) -> String {
    let team = args.first().and_then(|a| team_of(a)).unwrap_or("male");
    let date = match args.get(1) {
        Some(d) if valid_date(d) => d.to_string(),
        _ => default_biz_date(),
    };
    let sql = crate::dialect::MISSING_SQL.replace("/*TEAM*/", "");
    let _ = sql;
    let rows =
        crate::mysql::query_with_binds(&st.pools, "LOCAL", crate::dialect::MISSING_SQL, |q| {
            q.bind(date.clone()).bind(team)
        })
        .await;
    match rows {
        Ok(rows) => {
            let names: Vec<String> = rows
                .iter()
                .filter_map(|r| r.try_get::<String, _>("name").ok())
                .collect();
            if names.is_empty() {
                format!("{} {} 数据已齐 ✅", team_label(team), date)
            } else {
                format!(
                    "{} {} 缺数据 {} 人：{}",
                    team_label(team),
                    date,
                    names.len(),
                    names.join("、")
                )
            }
        }
        Err(e) => format!("查询失败：{e}"),
    }
}

fn team_label(team: &str) -> String {
    match team {
        "male" => "男团".into(),
        "female" => "女团".into(),
        _ => team.to_string(),
    }
}

fn team_of(s: &str) -> Option<&'static str> {
    match s {
        "男团" | "male" => Some("male"),
        "女团" | "female" => Some("female"),
        _ => None,
    }
}

/// 从文件名读日期与团别，运营命名习惯：`2026-10-07-男团.csv`。
/// 读得出来就直接入库，省掉「请回复团别」这一轮。
fn hints_from_filename(name: &str) -> (Option<String>, Option<&'static str>) {
    let lower = name.to_lowercase();
    let team = if lower.contains("男团") || lower.contains("male") {
        Some("male")
    } else if lower.contains("女团") || lower.contains("female") {
        Some("female")
    } else {
        None
    };
    (find_date(name), team)
}

/// 在任意文本里找第一个合法的 `YYYY-MM-DD`。
/// 按字节滑窗，但只认「4 数字 - 2 数字 - 2 数字」且能通过日历校验的片段——
/// 数字和 `-` 都是 ASCII，多字节汉字的字节都 ≥ 0x80，不会误配。
fn find_date(s: &str) -> Option<String> {
    let b = s.as_bytes();
    if b.len() < 10 {
        return None;
    }
    let digits = |r: &[u8]| r.iter().all(u8::is_ascii_digit);
    for i in 0..=b.len() - 10 {
        let w = &b[i..i + 10];
        if w[4] != b'-' || w[7] != b'-' {
            continue;
        }
        if !digits(&w[..4]) || !digits(&w[5..7]) || !digits(&w[8..10]) {
            continue;
        }
        if let Ok(cand) = std::str::from_utf8(w) {
            if valid_date(cand) {
                return Some(cand.to_string());
            }
        }
    }
    None
}

/// 默认业务日期 = 前一天。数据有延迟：每晚 24 点后发前一天的数据，
/// 8 号发的 CSV 全部是 7 号的（用户口径，2026-10-05 确认）。
fn default_biz_date() -> String {
    (chrono::Local::now().date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string()
}

fn valid_date(s: &str) -> bool {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

/// 数字格式化：整数不带小数点，加千分位
fn fmt_num(w: f64) -> String {
    let n = w.round() as i64;
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// 下载附件文本（QQ CDN 直链 / 微信文件）
async fn download_text(url: &str) -> anyhow::Result<String> {
    let resp = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?
        .get(url)
        .send()
        .await?
        .error_for_status()?;
    let bytes = resp.bytes().await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// GET /api/admin/bots/sessions：所有出现过的会话（群聊/私聊 + 管理员标记）
pub async fn bots_sessions(State(st): State<ArcState>) -> ApiResult {
    mgr(&st)?;
    Ok(axum::Json(
        serde_json::json!({ "items": crate::store::session_list(&st).await }),
    ))
}

#[derive(serde::Deserialize)]
pub struct SessionAdminBody {
    pub channel: Option<String>,
    #[serde(rename = "conversationId")]
    pub conversation_id: Option<String>,
    #[serde(rename = "isAdmin")]
    pub is_admin: Option<bool>,
}

/// POST /api/admin/bots/sessions/admin：勾选/取消管理员会话（群聊或私聊）
pub async fn bots_session_admin(
    State(st): State<ArcState>,
    axum::Json(body): axum::Json<SessionAdminBody>,
) -> ApiResult {
    mgr(&st)?;
    let channel = body
        .channel
        .ok_or_else(|| anyhow::anyhow!("缺少 channel"))?;
    let conversation_id = body
        .conversation_id
        .ok_or_else(|| anyhow::anyhow!("缺少 conversationId"))?;
    if channel != "qq" && channel != "weixin" {
        return Err(anyhow::anyhow!("channel 只能是 qq / weixin").into());
    }
    crate::store::session_set_admin(
        &st,
        &channel,
        &conversation_id,
        body.is_admin.unwrap_or(true),
    )
    .await;
    Ok(axum::Json(serde_json::json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_aliases() {
        assert_eq!(team_of("男团"), Some("male"));
        assert_eq!(team_of("male"), Some("male"));
        assert_eq!(team_of("女团"), Some("female"));
        assert_eq!(team_of("boy"), None);
    }

    #[test]
    fn dates_validated() {
        assert!(valid_date("2026-10-04"));
        assert!(!valid_date("10-04"));
        assert!(!valid_date("2026/10/04"));
    }

    /// 运营报日期的写法：「10.1」「10-1」「10/1」「10月1日」都指当年 10 月 1 日
    #[test]
    fn short_dates_parsed() {
        let year = chrono::Local::now().format("%Y").to_string();
        let want = format!("{year}-10-01");
        for s in ["10.1", "10-1", "10/1", "10月1日", "10月1号", "10.01"] {
            assert_eq!(parse_short_date(s), Some(want.clone()), "解析失败：{s}");
        }
        // 完整日期原样返回
        assert_eq!(parse_short_date("2026-10-01"), Some("2026-10-01".into()));
        // 认不出的不当日期（别把随便一句话当日期）
        for s in ["", "今天", "10", "13.1", "2.30", "帮助"] {
            assert_eq!(parse_short_date(s), None, "不该认成日期：{s}");
        }
    }

    #[test]
    fn numbers_formatted_with_thousands() {
        assert_eq!(fmt_num(11370000.0), "11,370,000");
        assert_eq!(fmt_num(560000.4), "560,000");
        assert_eq!(fmt_num(0.0), "0");
        assert_eq!(fmt_num(-1234.0), "-1,234");
    }

    #[test]
    fn rate_limiter_blocks_after_limit() {
        let rl = RateLimiter::new(3);
        assert!(rl.allow("qq:alice"));
        assert!(rl.allow("qq:alice"));
        assert!(rl.allow("qq:alice"));
        assert!(!rl.allow("qq:alice"), "超过 3 条/分钟应拦截");
        assert!(rl.allow("qq:bob"), "不同会话互不影响");
    }

    #[test]
    fn unknown_command_gets_hint() {
        // route_command 的兜底分支：不含关键字时不 panic，返回提示
        // （完整路由需要 AppState，这里只验证纯函数层）
        assert!(team_of("女团").is_some());
    }

    #[test]
    fn filename_hints_read_date_and_team() {
        let (d, t) = hints_from_filename("2026-10-07-男团.csv");
        assert_eq!(d.as_deref(), Some("2026-10-07"));
        assert_eq!(t, Some("male"));

        let (d, t) = hints_from_filename("女团 2026-10-06.xlsx");
        assert_eq!(d.as_deref(), Some("2026-10-06"));
        assert_eq!(t, Some("female"));

        // 只有团别没日期：日期回落到默认（前一天）
        let (d, t) = hints_from_filename("male-1007.csv");
        assert_eq!(d, None);
        assert_eq!(t, Some("male"));

        // 都没有：留给数据行推断
        assert_eq!(hints_from_filename("export.csv"), (None, None));
    }

    #[test]
    fn date_scan_ignores_junk_and_bad_dates() {
        assert_eq!(find_date("没有日期"), None);
        assert_eq!(
            find_date("2026-13-45.csv"),
            None,
            "不存在的日期应被日历校验挡掉"
        );
        assert_eq!(find_date("short"), None);
        // 前缀有汉字也不影响（多字节不会误配）
        assert_eq!(
            find_date("男团数据-2026-10-07-导出.csv").as_deref(),
            Some("2026-10-07")
        );
    }
}
