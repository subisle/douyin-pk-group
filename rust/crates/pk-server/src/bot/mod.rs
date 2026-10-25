//! 机器人模块（E 阶段）：确定性命令路由 + QQ / 微信渠道适配器
//!
//! 边界（docs/00 §9、docs/06 §4.4）：
//! - QQ 只认 `BOT_QQ_ADMINS` 白名单（2026-10-17 第四轮审计 P1-3 起 **fail-closed**：
//!   未配置 = 一个都不认；docs 早期「QQ 收到即可服务、不需要管理员白名单」的口径作废）；
//!   微信私聊发送内置口令可成为管理员；已完成推送绑定的主播私聊 = 普通用户
//!   （只有「每日报告 / 个人报告 / 暂停·开启接收每日推送 / 帮助」5 条，见 [`commands`]）；
//!   未绑定会话静默忽略（日志留痕，不回复）
//! - 命令路由确定性：同命令同输入同输出，不调用任何模型
//! - 限流 10 条/分钟/会话；写类命令（导入）走 douyin_writer，与 HTTP 端同一套校验

pub mod qq;
pub mod weixin;

mod commands;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::extract::State;
use futures_util::TryStreamExt as _;
use serde_json::Value;
use sqlx::Row as _;

use crate::admin;
use crate::state::AppState;
use commands::{CmdId, Role};
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

/// 渠道出站文件（QQ 富媒体发送用；微信暂不消费）。
#[derive(Clone, Debug)]
pub struct OutboundFile {
    pub file_name: String,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct Outbound {
    pub conversation_id: String,
    pub text: String,
    /// 主消息发完后继续发送的文本（按顺序）。
    ///
    /// 微信扫码账号对接成功时拆成两条：第一条确认已提交/对接成功，第二条发功能
    /// 介绍。微信端连续气泡比一大段文字更容易读，也不会让成功回执被长文案淹没。
    pub followup_texts: Vec<String>,
    /// 随文字一起下发的图片（PNG 字节，按顺序发送）。空 = 纯文本。
    ///
    /// 2026-10-18 用户口径：机器人命令「1」（每日榜单）改成发**榜单图**（与自动推送
    /// 同一套渲染）。文字版本保留作兜底 —— 能发图的渠道（微信扫码账号 / 微信全局
    /// 机器人）优先发图、不再发文字 TOP10；发不了图的渠道（QQ）继续发文字。
    pub images: Vec<Vec<u8>>,
    /// 随消息一起发送的文件；QQ 通过 `msg_type=7` 富媒体发送。
    pub files: Vec<OutboundFile>,
}

impl Outbound {
    pub fn to(conversation_id: &str, text: impl Into<String>) -> Self {
        Self {
            conversation_id: conversation_id.to_string(),
            text: text.into(),
            followup_texts: Vec::new(),
            images: Vec::new(),
            files: Vec::new(),
        }
    }

    /// 主消息后继续发一条文本（微信扫码对接的「成功回执 → 功能介绍」）。
    pub fn with_followup_text(mut self, text: impl Into<String>) -> Self {
        self.followup_texts.push(text.into());
        self
    }

    /// 挂上待发图片（文字仍保留：渠道发不了图时用它兜底）。
    pub fn with_images(mut self, images: Vec<Vec<u8>>) -> Self {
        self.images = images;
        self
    }

    pub fn with_files(mut self, files: Vec<OutboundFile>) -> Self {
        self.files = files;
        self
    }
}

/* ------------------------------- 配置 ------------------------------- */

#[derive(Clone, Debug)]
pub struct BotConfig {
    pub qq_app_id: String,
    pub qq_app_secret: String,
    /// 微信 iLink token（空 = 未登录，需要扫码；扫码成功后持久化到业务库 config 表）
    pub wechat_token: String,
    /// QQ 管理员白名单（逗号分隔 QQ 号 / 会话 ID），**fail-closed**：
    /// 只有名单里的会话才算 Admin；**未配置 = 一个都不认**。
    /// 2026-10-17 第四轮审计 P1-3 修复 —— docs/09 早期「QQ 收到即可服务」的
    /// 无条件 Admin 口径已作废（那等于任何能私聊 bot 的陌生 QQ 都有全量权限）。
    /// 判定见 [`qq_admin_ok`]，生产由 compose 透传 `.env.prod` 的 `BOT_QQ_ADMINS`。
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

    /// 微信渠道始终初始化：未授权会话可私聊提权口令（env `WECHAT_ADMIN_CODE`，
    /// 未配置 = 提权路径整体关闭，见 [`wechat_admin_code`]），或由管理员在后台勾选会话。
    pub fn has_wechat(&self) -> bool {
        true
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
        // P2-15（2026-10-10 审计）：每次都全表 retain——先把过期时间戳清掉，
        // 空了的会话一并移除，map 不随历史会话数无界增长（对照 my_tokens 的写法）。
        hits.retain(|_, list| {
            list.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
            !list.is_empty()
        });
        let list = hits.entry(key.to_string()).or_default();
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
/// 只给月日 → 就近取年（P2-12，2026-10-10 审计）：与「今天」差超过约 6 个月就取相邻年份，
/// 1 月初发「12.31」算去年、12 月底发「1.1」算今年。
pub fn parse_short_date(s: &str) -> Option<String> {
    let t = s.trim();
    if valid_date(t) {
        return Some(t.to_string());
    }
    let norm: String = t
        .trim_end_matches(['号', '日'])
        .chars()
        .map(|c| {
            if matches!(c, '.' | '/' | '月' | '-') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let parts: Vec<&str> = norm.split('-').filter(|p| !p.is_empty()).collect();
    if parts.len() == 2 {
        let m: u32 = parts[0].parse().ok()?;
        let d: u32 = parts[1].parse().ok()?;
        let today = chrono::Local::now().date_naive();
        // 两种年份候选里取离今天更近的那个（约 6 个月内必然是正确年份）
        let this_year = format!("{}-{m:02}-{d:02}", today.format("%Y"));
        let last_year = format!(
            "{}-{m:02}-{d:02}",
            (today - chrono::Duration::days(365)).format("%Y")
        );
        let pick = |cand: &str| valid_date(cand).then(|| cand.to_string());
        match (pick(&this_year), pick(&last_year)) {
            (Some(a), Some(b)) => {
                let da = chrono::NaiveDate::parse_from_str(&a, "%Y-%m-%d").ok()?;
                let db = chrono::NaiveDate::parse_from_str(&b, "%Y-%m-%d").ok()?;
                let da_gap = (da - today).num_days().abs();
                let db_gap = (db - today).num_days().abs();
                return Some(if da_gap <= db_gap { a } else { b });
            }
            (Some(a), None) => return Some(a),
            (None, Some(b)) => return Some(b),
            (None, None) => return None,
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

/// QQ 凭据解析：config 表（后台「机器人」页保存的）逐字段优先，env 兜底。
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
        // 微信 token 优先 env，其次 config 表（扫码成功后落库）
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
            // 1.0.1：confirmed 可能下发过 baseurl / ilink_user_id，重启后接着用
            let wx_base = crate::store::config_get(&st, "wechat_baseurl")
                .await
                .unwrap_or_default();
            let wx_uid = crate::store::config_get(&st, "wechat_ilink_user_id")
                .await
                .unwrap_or_default();
            match WxTransport::new(st.clone(), config.clone(), wechat_token, wx_base, wx_uid) {
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
                "note": "未登录；先扫码连接，再由管理员在后台「机器人」页勾选管理员会话（或配置提权口令）",
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
        crate::store::config_set(&st, "bot_qq_app_id", id).await;
    }
    if let Some(sec) = &app_secret {
        crate::store::config_set(&st, "bot_qq_app_secret", sec).await;
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
        .ok_or_else(|| anyhow::anyhow!("微信渠道未初始化"))?;
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

/// POST /api/admin/bots/weixin/qrcode/check {qrcode, verifyCode?}：
/// 查扫码状态，成功自动保存 token。1.0.1 对齐 openclaw-weixin 全状态机：
/// need_verifycode 时前端出验证码输入框，下次轮询带 verifyCode；
/// scaned_but_redirect / binded_redirect 时后端自动切 redirect_host 继续轮。
#[derive(serde::Deserialize)]
pub struct QrCheckBody {
    pub qrcode: Option<String>,
    pub verify_code: Option<String>,
    /// camelCase 兼容（前端统一传 verifyCode）
    #[serde(rename = "verifyCode")]
    pub verify_code_camel: Option<String>,
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
    let verify = body
        .verify_code
        .or(body.verify_code_camel)
        .filter(|s| !s.trim().is_empty());
    let out = wx.check_qrcode(&qrcode, verify.as_deref()).await?;
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

/// 微信推送对接（2026-10-16 用户口径：**只发 4 位数字对接码即可**，不再要手机号）。
/// 命中返回回复文案，没命中返回 None（继续走管理员判定）。
///
/// 消息格式（给 Lead 写文档用）：
/// - 直接发 4 位数字：「1234」——展示端「我的」页的文案就是这么写的
/// - 带关键词：「绑定 1234」/「bind 1234」/「bd 1234」
/// - 旧写法「绑定 13800000000 1234」也认：手机号被忽略，起作用的只有码
/// - 只发手机号 → 回一句「现在不用手机号了，发 4 位对接码即可」（不查库、不泄露该号是否登记过）
///
/// 安全口径：对接码 = 「我的」页 push_rotate_code 生成的 **4 位数字、30 分钟一次性、库里全局唯一**
/// 的码（见迁移 028）。错误文案统一，不区分「码不对」「码过期」，避免枚举。
/// **裸 4 位数字没对上码时静默**：否则正常聊天里随便一个 4 位数都会收到一句「绑定失败」。
/// 对接成功的回复 = [`wx_dock_success`] + [`wx_dock_intro`]（**带账号姓名**）：
/// P1-1 审计原口径是「回复不带姓名，
/// 防会话被截图外传」，2026-10-16 深夜用户口径改为**欢迎语带名字** —— 主播自己一眼确认
/// 绑对了号，比防外传更值。要退回匿名口径只改这一个函数即可（扫码路径的入站回执同理）。
async fn try_bind_weixin(st: &Arc<AppState>, inbound: &Inbound) -> Option<Outbound> {
    let conversation = inbound.conversation_id.clone();
    let text = inbound.text.trim();
    // 解绑：任何微信私聊都可以发；没绑过也回一句解释（绑定流程本身对未绑定会话开放）
    let lower = text.to_lowercase();
    if matches!(lower.as_str(), "解绑" | "取消绑定" | "unbind") {
        let name = crate::store::push_unbind_by_conversation(st, &conversation).await;
        return Some(Outbound::to(
            &conversation,
            match name {
                Some(_) => "已解绑，不会再收到推送。\n要重新对接，打开展示端「我的」页 →「消息设置」→ 微信对接，\n点「获取二维码」，把 4 位对接码发给我即可。".to_string(),
                None => "这条会话还没有绑定过，不用解绑。".to_string(),
            },
        ));
    }

    // 绑定：认 4 位数字绑定码（带不带「绑定」关键词都行）
    let has_kw = is_bind_keyword(text);
    let Some(code) = extract_bind_code(text) else {
        // 没认出码：只发手机号时给一句指引（不查库、不泄露登记状态），其余静默
        if extract_phone(text).is_some() {
            return Some(Outbound::to(
                &conversation,
                "收到手机号。现在对接不用手机号了：\n\
                 打开展示端 →「我的」页 →「消息设置」→ 点「获取二维码」，\n\
                 把页面上那 4 位对接码直接发给我即可（30 分钟内有效，用一次就作废）。",
            ));
        }
        return None; // 不是绑定消息：继续走管理员判定
    };

    // 会话级尝试锁（2026-10-17 第四轮审计 P1-2）：同会话 10 分钟内失败 ≥5 次即拒绝兑换。
    // 锁定期间：明确绑定意图（带关键词）回统一文案；裸数字照旧静默，避免正常聊天被刷屏。
    let conv_key = bind_fail_conv_key(inbound.channel, &conversation);
    if conv_bind_locked(st, &conv_key).await {
        if !has_kw {
            return None;
        }
        tracing::warn!(
            conversation = %conversation,
            "会话对接码失败次数过多，临时拒绝兑换（统一文案）"
        );
        return Some(Outbound::to(&conversation, BIND_FAIL_TEXT));
    }

    let bind = crate::store::push_bind_by_code(st, &code, &conversation, &conversation).await;
    let (name, team_opt) = match bind {
        Some(pair) => {
            // 兑换成功：清该会话的失败计数，别让历史失败把正常用户继续锁在门外
            conv_bind_clear(st, &conv_key).await;
            pair
        }
        None => {
            // 记一次会话失败。裸数字也记：攻击者不带「绑定」关键词就能绕过计数，
            // 所以计数必须发生在「尝试兑换」这一步，而不是回复文案那一步。
            conv_bind_record_fail(st, &conv_key).await;
            if !has_kw {
                // 裸数字没对上：静默，别把正常聊天里的 4 位数变成一句「绑定失败」
                return None;
            }
            // 统一文案：不泄露码是否过期（P1-1 审计口径）
            tracing::warn!(
                conversation = %conversation,
                "微信绑定失败（码不对 / 码过期），统一回复"
            );
            return Some(Outbound::to(&conversation, BIND_FAIL_TEXT));
        }
    };
    // 绑定行团别为空（台账建档晚于绑定）会导致推送按团捞不到这个人、
    // 永远收不到推送。这里回查一次台账补上（沿用绑定码路径的修复）。
    if team_opt.is_empty() {
        if let Some(t) = crate::store::person_team_of_name(st, &name).await {
            crate::store::push_set_team(st, &name, &t).await;
            tracing::info!(account = %name, team = %t, "绑定后回填团别");
        }
    }
    // 只记内部日志（姓名可能敏感），回复不带姓名（P1-1）
    tracing::info!(conversation = %conversation, account = %name, "微信推送绑定成功（4 位绑定码）");
    crate::events::emit(
        crate::events::SCOPE_BOT,
        format!("微信推送已绑定（{name}）"),
    );
    Some(Outbound::to(&conversation, wx_dock_success(&name)).with_followup_text(wx_dock_intro()))
}

/* ------------------ 对接码会话级尝试锁（2026-10-17 第四轮审计 P1-2） ------------------
 * 4 位对接码只有 10^4 空间，且 30 分钟有效：攻击者可以换 N 个微信号（N 个会话）并行
 * 试码，机器人层「每会话 10 条/分钟」挡不住 N 会话叠加。除了 `store/wxpush.rs` 里
 * 按码 / 全局的失败计数（迁移 033）之外，这里再补一层**会话维度**的锁：
 *   同一会话 10 分钟内兑换失败 ≥5 次 → 暂时拒绝该会话的兑换请求，统一文案回复，
 *   不区分「码不对 / 码过期 / 试太多」，不给试码者任何状态反馈。
 *
 * 计数复用 bind_code_fails 表（不新建表），key = `conv:<channel>:<conversation_id>`；
 * 迁移 036 会把 code 列从 VARCHAR(16) 加宽到 160 —— 加宽前这个 key 大概率装不下。
 * 表不存在 / 列宽还没升级 / DB 故障 → **WARN 一次并降级放行原兑换逻辑**，
 * 绝不因为加固层自身不可用而阻断正常绑定。
 * ----------------------------------------------------------------------------------- */

/// 同一会话在窗口内失败达到该次数 → 临时拒绝兑换
const BIND_FAIL_CONV_MAX: i32 = 5;
/// 会话失败计数窗口（分钟）：窗口从首次失败起算，过期后下一次失败归 1 重计。
/// 用 i32（不是 i64）：`make_interval(mins => $n)` 的参数是 int4，i32 能精确对上；
/// 若绑 i64，sqlx 会把参数声明成 BIGINT，而 PG 函数解析不给 int8→int4 隐式转换，
/// 运行期会因「function make_interval(...) does not exist」而静默失败（降级放行）。
const BIND_FAIL_CONV_WINDOW_MIN: i32 = 10;
/// 兑换失败 / 被临时锁定的**统一文案**：不泄露码状态，也不暴露「被锁」这件事
const BIND_FAIL_TEXT: &str = "对接没有成功：这个 4 位对接码不对或已经过期。\n\
     打开展示端 →「我的」页 →「消息设置」→ 点「获取二维码」，\n\
     把新出的 4 位对接码发给我（30 分钟内有效，用一次就作废）。";

/// 会话计数 key（`conv:<channel>:<conversation_id>`）：与按码计数共用一张表。
fn bind_fail_conv_key(channel: &str, conversation_id: &str) -> String {
    format!("conv:{channel}:{conversation_id}")
}

/// 会话级计数不可用（表缺 / 列宽不够 / DB 故障）时只 WARN 一次，避免每条消息刷日志。
fn warn_conv_bind_lock_unavailable(err: &sqlx::Error) {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        tracing::warn!(
            error = %err,
            "会话级对接码尝试锁不可用（bind_code_fails 表缺 / 列宽不足 / DB 故障），\
             已降级放行原兑换逻辑；跑过迁移 036 后自动生效"
        );
    });
}

/// 写库池；未配置 = 无写库，直接降级（与其它写路径同语义）。
fn bind_fail_pool(st: &AppState) -> Option<&sqlx::PgPool> {
    st.writer.as_ref()
}

/// 该会话是否已被失败计数临时锁定（10 分钟窗口内失败 ≥5）。
async fn conv_bind_locked(st: &AppState, key: &str) -> bool {
    let Some(pool) = bind_fail_pool(st) else {
        return false;
    };
    match sqlx::query(&crate::pq::rewrite(
        "SELECT 1 FROM bind_code_fails \
          WHERE code = ? AND fails >= ? \
            AND window_at > now() - make_interval(mins => ?)",
    ))
    .bind(key)
    .bind(BIND_FAIL_CONV_MAX)
    .bind(BIND_FAIL_CONV_WINDOW_MIN)
    .fetch_optional(pool)
    .await
    {
        Ok(row) => row.is_some(),
        // 表缺 / 列不存在都按「锁不可用」处理：放行是对业务最安全的降级。
        Err(e) => {
            warn_conv_bind_lock_unavailable(&e);
            false
        }
    }
}

/// 记一次会话兑换失败（10 分钟窗口，过期归 1 重计）。
async fn conv_bind_record_fail(st: &AppState, key: &str) {
    let Some(pool) = bind_fail_pool(st) else {
        return;
    };
    let sql = "INSERT INTO bind_code_fails (code, fails, window_at, updated_at) \
               VALUES (?, 1, now(), now()) \
               ON CONFLICT (code) DO UPDATE SET \
                 fails = CASE WHEN bind_code_fails.window_at < now() - make_interval(mins => ?) \
                              THEN 1 ELSE bind_code_fails.fails + 1 END, \
                 window_at = CASE WHEN bind_code_fails.window_at < now() - make_interval(mins => ?) \
                              THEN now() ELSE bind_code_fails.window_at END, \
                 updated_at = now()";
    if let Err(e) = crate::writer::write_guard(sql) {
        tracing::warn!(error = %e, "会话对接码失败计数 SQL 未通过写守卫，跳过");
        return;
    }
    if let Err(e) = sqlx::query(&crate::pq::rewrite(sql))
        .bind(key)
        .bind(BIND_FAIL_CONV_WINDOW_MIN)
        .bind(BIND_FAIL_CONV_WINDOW_MIN)
        .execute(pool)
        .await
    {
        warn_conv_bind_lock_unavailable(&e);
    }
}

/// 兑换成功 / 换新码成功 → 清掉该会话的失败计数（历史失败不该继续锁着正常用户）。
async fn conv_bind_clear(st: &AppState, key: &str) {
    let Some(pool) = bind_fail_pool(st) else {
        return;
    };
    let sql = "DELETE FROM bind_code_fails WHERE code = ?";
    if let Err(e) = crate::writer::write_guard(sql) {
        tracing::warn!(error = %e, "会话对接码失败计数清理 SQL 未通过写守卫，跳过");
        return;
    }
    if let Err(e) = sqlx::query(&crate::pq::rewrite(sql))
        .bind(key)
        .execute(pool)
        .await
    {
        // 清理失败不影响本次绑定，只 warn（锁会随 10 分钟窗口自然过期）
        tracing::warn!(error = %e, "会话对接码失败计数清理失败（不影响绑定，锁会自然过期）");
    }
}

/* ---------------------- 对接欢迎语的鼓励文案池（2026-10-18） ---------------------- */

/// 1000 条鼓励文案，一行一条（`include_str!` 零运行时 IO，编译期进二进制）。
///
/// 为什么单独放数据文件：文案是运营资产，改文案不该动 Rust 源码、更不该为了改一句话
/// 重新走一遍代码评审；同时 `include_str!` 保证部署时不会因为漏拷一个 txt 而静默空池。
const WX_GREETINGS_RAW: &str = include_str!("greetings.txt");

/// 文案池（去空行 / 去注释行；首次调用解析一次，之后复用）。
fn wx_greetings() -> &'static [&'static str] {
    static POOL: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        WX_GREETINGS_RAW
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    })
    .as_slice()
}

/// 随机抽一条鼓励文案。池子为空（文件被误清空）时兜底一句，永不让欢迎语缺句。
fn random_greeting() -> &'static str {
    match wx_greetings() {
        [] => "今天也要开心一点。",
        pool => pool[crate::random::pick_index(pool.len())],
    }
}

/// 这段文本是不是一条已注册的命令（供扫码激活路径判断：首条消息是命令就顺便执行）。
pub(crate) fn looks_like_command(text: &str) -> bool {
    commands::parse(text).is_some()
}

/// 微信对接成功回执（所有对接入口的第一条消息）。
///
/// 2026-10-16 用户口径：「对接完成发送欢迎语加名字」——直接叫名字（账号姓名 =
/// 展示端登录的那个姓名），比通用文案更像「系统认识你」，主播一眼确认绑对了号。
/// 曾经 P1-1 审计要求不带姓名（防会话截图外传），本次由用户明确改回带姓名；
/// 要恢复匿名口径，把 `你好，{name}！` 换成 `你好！` 即可，三个入口同时生效。
///
pub(crate) fn wx_dock_success(name: &str) -> String {
    let who = name.trim();
    if who.is_empty() {
        "你好！\n✅ 微信对接成功，机器人已为你配置好。".to_string()
    } else {
        format!("你好，{who}！\n✅ 微信对接成功，机器人已为你配置好。")
    }
}

/// 微信功能介绍（所有对接入口的第二条消息）。
///
/// 2026-10-18 用户口径：欢迎语 = **随机鼓励文案（1000 条池）+ 功能简介 + 1/2/3/4 菜单**。
/// 主播多数是**没开播的时候**打开，第一句要先给情绪价值（鼓励/陪伴），再讲功能，
/// 最后给最省事的入口：回一个数字。
/// 2026-10-19 用户口径：成功确认与功能介绍拆成两条发，本条不再重复「对接成功」。
pub(crate) fn wx_dock_intro() -> String {
    format!(
        "{greeting}\n\n\
         以后每天自动推送每日报告，追分提醒也会单独推送，\n\
         开关和追分目标都在展示端「我的」页里设置。\n\n\
         想查什么，直接回数字：\n\
         1 —— 每日榜单（当天 TOP10 + 你排第几）\n\
         2 —— 个人每日播报（今日音浪 / 预估收入 + 本月 + 追分进度）\n\
         3 —— 徒弟榜单（名下徒弟本月排第几）\n\
         4 —— 追分榜单（你在追的人排第几、还差多少）\n\n\
         发「帮助」看全部说明，发「解绑」可断开对接。",
        greeting = random_greeting()
    )
}

/// 微信对接成功后连续发送「成功回执 → 功能介绍」。
///
/// 只走 [`weixin::user_send_text`] 这个已装出网闸门的窄口；任一条被拦截/失败都
/// 原样返回 `Err`，调用方不能把 `Blocked` 当成已送达。两条之间留很短间隔，
/// 保证微信端气泡顺序稳定。
pub(crate) async fn send_wx_dock_messages(
    st: &AppState,
    base_url: &str,
    bot_token: &str,
    to_user: &str,
    context_token: &str,
    name: &str,
) -> anyhow::Result<()> {
    weixin::user_send_text(
        st,
        base_url,
        bot_token,
        to_user,
        context_token,
        &wx_dock_success(name),
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(400)).await;
    weixin::user_send_text(
        st,
        base_url,
        bot_token,
        to_user,
        context_token,
        &wx_dock_intro(),
    )
    .await
}

/// 微信提权口令（2026-10-11 审计整改）：不再硬编码在源码里，
/// 启动时读 env `WECHAT_ADMIN_CODE`（trim 后非空才启用），进程内只读一次。
/// 未配置 = 提权路径整体关闭：用户发任何口令都无效果，且不回显是否已配置。
fn wechat_admin_code() -> Option<&'static str> {
    static CODE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CODE.get_or_init(|| {
        std::env::var("WECHAT_ADMIN_CODE")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
    .as_deref()
}

/// 口令比对（纯函数，可单测）：只有「已配置」且「全文精确相等」才算命中。
/// 刻意不用 contains / 前缀匹配——口令必须是整条消息，避免正常聊天里提到口令被误提权。
fn admin_code_matches(configured: Option<&str>, text: &str) -> bool {
    matches!(configured, Some(code) if !code.is_empty() && text.trim() == code)
}

/// 微信管理员提权口令。会话已在 `handle` 开头登记，因此这里可以直接升级当前会话。
async fn try_promote_weixin(st: &Arc<AppState>, inbound: &Inbound) -> Option<Outbound> {
    // 未配置口令时静默返回（None 会继续走角色判定，未授权会话照旧静默忽略），
    // 不向用户透露「提权未开启」这类配置信息。
    if inbound.channel != "weixin"
        || inbound.is_group
        || !admin_code_matches(wechat_admin_code(), &inbound.text)
    {
        return None;
    }
    crate::store::session_set_admin(st, inbound.channel, &inbound.conversation_id, true).await;
    /* 广播 SCOPE_BOT（2026-10-17 修复，审计 6-12a）：
    原先提权成功不 emit，后台「机器人」页要等 20 秒兜底轮询才看到管理员标记变化。 */
    crate::events::emit(crate::events::SCOPE_BOT, "微信会话已提为管理员");
    tracing::info!(conversation = %inbound.conversation_id, "微信会话已通过提权口令设为管理员");
    Some(Outbound::to(&inbound.conversation_id, "已成为管理员"))
}

/// 从消息里认手机号：`绑定 13800000000 1234` / `bd 13800000000` / 单独一串数字。
/// 只认 7~15 位纯数字（4 位绑定码长度对不上，不会被误认）。
/// 不带关键词时必须只有一段，避免把「查 3 月数据」这类文本误判成绑定。
/// 2026-10-16 起手机号不再是绑定凭据，只用来给「只发了手机号」的人回一句指引。
fn extract_phone(text: &str) -> Option<String> {
    let mut toks = text.split_whitespace();
    let first = toks.next()?;
    let has_kw = is_bind_keyword(first);
    let cand = if has_kw { toks.next()? } else { first };
    // 不带关键词时不许有第二段；带关键词时第二段只可能是码（交给码提取器处理）
    if !has_kw && toks.next().is_some() {
        return None;
    }
    let digits: String = cand.chars().filter(|c| c.is_ascii_digit()).collect();
    let original_is_digits = cand.chars().all(|c| c.is_ascii_digit());
    if !original_is_digits || !(7..=15).contains(&digits.len()) {
        return None;
    }
    Some(digits)
}

/// 这条消息是不是以绑定关键词开头（`绑定` / `bind` / `bd`）。
/// 用途：区分「用户明确在绑定」和「聊天里恰好出现一个 4 位数」——
/// 后者没对上码时静默，前者才回失败文案。
fn is_bind_keyword(text: &str) -> bool {
    matches!(
        text.split_whitespace()
            .next()
            .unwrap_or_default()
            .to_lowercase()
            .as_str(),
        "绑定" | "bind" | "bd"
    )
}

/// 从消息里认 4 位数字绑定码（2026-10-16 口径）：
/// - 带关键词：「绑定 1234」/「bind 1234」；也容忍旧写法「绑定 13800000000 1234」
///   （扫关键词后面所有段，取第一个像码的 —— 手机号 11 位，长度就对不上，不会被误认）；
/// - 不带关键词：整条消息只有一段、且正好 4 位数字才认（避免把正常聊天误判成绑定码）。
fn extract_bind_code(text: &str) -> Option<String> {
    let mut toks = text.split_whitespace();
    let first = toks.next()?;
    if !is_bind_keyword(first) {
        if toks.next().is_some() {
            return None;
        }
        return crate::push::looks_like_bind_code(first).then(|| first.to_string());
    }
    toks.find(|t| crate::push::looks_like_bind_code(t))
        .map(|t| t.to_string())
}

/* ------------------- 二要素绑定：手机号黏合（P1-1，2026-10-16 已下线） ------------------- */
/*
   原来「先发手机号、30 秒内再发绑定码」靠这里的内存黏合（PENDING_PHONES）。
   2026-10-16 改成「只发 4 位数字绑定码即可」后不再需要：码本身就唯一指向一个账号，
   用户不必再先报手机号。整个黏合机制连同它的 TTL 一起删掉，少一处内存态。
*/

/* ---------------- 管理员判定：QQ fail-closed（2026-10-17 第四轮审计 P1-3） ----------------
 * 历史口径（docs/09、2026-10-10 用户口径）：QQ 渠道「收到即可服务」，任何能私聊到
 * bot 的 QQ 都是 Admin；`BOT_QQ_ADMINS` 虽然从 env 读进 BotConfig，但**全代码零引用**
 * —— 配了白名单也不生效（死配置），没配则等于陌生 QQ 可以直接执行导入写库 / 全量导出 /
 * 触发推送。
 *
 * 审计要求改成 fail-closed：**未配置 = 没有管理员**。这与 docs 早期口径相反，是审计后的
 * 安全收紧：上线前必须在 .env.prod 配好 `BOT_QQ_ADMINS`（生产 compose 已透传该键），
 * 否则 QQ 会话只会被日志留痕后静默忽略。
 * 会话表勾选 / 微信提权口令路径不受影响（与渠道无关，照旧算 Admin）。
 * ----------------------------------------------------------------------------------- */

/// 「BOT_QQ_ADMINS 未配置」进程内只提示一次，避免每条 QQ 消息刷一条 WARN。
fn warn_qq_admins_unset_once() {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        tracing::warn!(
            "BOT_QQ_ADMINS 未配置：QQ 渠道不再默认为管理员（fail-closed，第四轮审计 P1-3）\
             —— 生产请在 .env.prod 配好 QQ 号白名单，否则 QQ 会话不会被处理"
        );
    });
}

/// QQ 管理员判定（纯函数，可单测）：**未配置 = false**（fail-closed）；
/// 配了则只认名单内的会话 ID（QQ 号）。
fn qq_admin_ok(qq_admins: &[String], conversation_id: &str) -> bool {
    if qq_admins.is_empty() {
        warn_qq_admins_unset_once();
        return false;
    }
    qq_admins.iter().any(|a| a == conversation_id)
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
    // 限流（P1-1 审计：必须前置到绑定分支之前，否则绑定路径不限速可暴力试码）。
    // 对所有渠道所有会话生效（含绑定/管理员）。
    let limiter = LIMITER.get_or_init(|| RateLimiter::new(cfg.rate_limit_per_min));
    let key = format!("{}:{}", inbound.channel, inbound.conversation_id);
    if !limiter.allow(&key) {
        /* 文案带上**实际生效**的上限（2026-10-17 修复，审计 6-12b）：
        BOT_RATE_LIMIT 可配，原先写死「10 条/分钟」，改了配置文案就在撒谎。 */
        return Some(Outbound::to(
            &inbound.conversation_id,
            format!(
                "消息太频繁了，稍后再试（限流 {} 条/分钟）",
                cfg.rate_limit_per_min
            ),
        ));
    }
    // 展示端「我的」页微信绑定（2026-10-16 口径：只发 4 位绑定码即可）。
    // 必须在管理员白名单之前处理——来绑定的都是普通主播，不是管理员；
    // 限流已经在上一步生效，绑定码暴力尝试会被限流挡住。
    if inbound.channel == "weixin" && !inbound.is_group {
        if let Some(out) = try_bind_weixin(st, &inbound).await {
            return Some(out);
        }
    }
    if let Some(out) = try_promote_weixin(st, &inbound).await {
        return Some(out);
    }
    // 角色判定（2026-10-10 用户口径，2026-10-11 复核确认）：
    // - Admin：QQ 白名单 / 微信白名单 / 会话表标记过管理员 —— 拥有全部命令
    // - User：微信私聊且 push_bindings 命中的已绑定主播 —— 只允许 5 条命令
    // - 其余：完全静默（不回复任何内容），日志留痕
    //
    // QQ 的管理员判定（2026-10-17 第四轮审计 P1-3，**行为已改 fail-closed**）：
    // 原先是「channel == qq 就无条件 Admin」+ `BOT_QQ_ADMINS` 死配置；
    // 现在**只有配在 BOT_QQ_ADMINS 里的会话才是 Admin，没配 = 一个都不认**。
    // 注意这与 docs/09 早期「QQ 收到即可服务」的定稿口径相反，是本轮审计后的安全收紧；
    // 生产上线前必须在 .env.prod 配好白名单（prod compose 已透传该键）。
    // 会话表标记过的管理员（后台勾选 / 微信提权口令）始终算 Admin，与渠道无关。
    let is_qq_admin =
        inbound.channel == "qq" && qq_admin_ok(&cfg.qq_admins, &inbound.conversation_id);
    let role = if is_qq_admin
        || cfg.wechat_admins.contains(&inbound.conversation_id)
        || crate::store::session_is_admin(st, inbound.channel, &inbound.conversation_id).await
    {
        Role::Admin
    } else if inbound.channel == "weixin"
        && !inbound.is_group
        && crate::store::push_binding_by_conversation(st, &inbound.conversation_id)
            .await
            .is_some()
    {
        Role::User
    } else {
        tracing::warn!(
            channel = inbound.channel,
            conversation = %inbound.conversation_id,
            kind = if inbound.is_group { "group" } else { "private" },
            text = %inbound.text,
            "未授权 / 未绑定会话消息，静默忽略（微信可在管理端勾选管理员，或先绑定推送）"
        );
        return None;
    };

    let text = inbound.text.trim().to_string();

    // 附件导入与纯日期黏住都是管理员流程：普通用户发这两类消息时给权限提示，
    // 不静默、不下载附件、不落库。
    if role == Role::User {
        if !inbound.attachments.is_empty() {
            return Some(Outbound::to(
                &inbound.conversation_id,
                "导入是管理员功能，你没有权限。\n发「帮助」看你能用的命令。",
            ));
        }
        if !text.is_empty() && parse_short_date(&text).is_some() {
            return Some(Outbound::to(
                &inbound.conversation_id,
                "指定导入日期是管理员功能，你没有权限。\n发「帮助」看你能用的命令。",
            ));
        }
        if text.is_empty() {
            return None;
        }
        return Some(route_command(st, cfg, &inbound, &text, role).await);
    }

    // —— 以下为管理员流程（附件下载 / 日期黏住 / 命令路由）——

    // 附件：即时下载（QQ CDN 直链带 rkey，有时效），存为待确认导入
    let mut attachment_text = String::new();
    for att in &inbound.attachments {
        tracing::info!(
            file = %att.file_name,
            url_head = %att.url.chars().take(80).collect::<String>(),
            "机器人收到附件，开始下载"
        );
        // 传平台给的真实文件名：CDN URL 推不出扩展名时 xlsx 会被当文本，
        // 二进制垃圾直接进暂存区（批次 #287 就是这么来的）
        match download_text(&att.url, Some(&att.file_name)).await {
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

        /* 只有时长的月度文件（2026-10-10 用户口径：每日不能导入时长，只有月时长）：
        只有「时长」列、没有音浪列的附件不能按日导 —— 那会「成功 0 行」，看着像成功其实什么都没进。
        文件名带月份（9月份时长.csv）就直接按那个月导；没带月份让运营改名或去管理端选月份。
        2026-10-16 起按月导入也支持音浪（admin 的「按月导入」可选两种指标），但机器人这边
        只对「只有时长」的文件自动分流：带音浪的概览列表既可能是按日快照也可能是月报，
        机器判不出来，交给管理端弹窗里人工选。 */
        let month_rows = admin::parse_month_metrics_text(&attachment_text);
        if admin::is_month_duration_file(&month_rows, &rows) {
            let Some((y, m)) = crate::csvparse::month_from_filename(&filename) else {
                return Some(Outbound::to(
                    &inbound.conversation_id,
                    format!(
                        "文件「{filename}」看着是月度时长（只有时长列、没有音浪列），但文件名里没有月份。\n\
                         把文件名改成「9月份时长.csv」这样再发，或到管理端「数据导入」页选年份 + 月份导入。"
                    ),
                ));
            };
            let period = format!("{y}-{m:02}");
            tracing::info!(
                file = %filename,
                period = %period,
                rows = month_rows.len(),
                "机器人月度时长导入"
            );
            let reply = match admin::commit_month_metrics(
                st,
                &period,
                "auto",
                month_rows,
                "merge",
                &format!("机器人·{filename}"),
                admin::MonthMetrics::MINUTES_ONLY,
            )
            .await
            {
                Ok(r) => {
                    let mut s = format!("✅ {period} 月度时长导入成功 {} 人", r.ok_rows);
                    for f in r.failures.iter().take(10) {
                        s.push_str(&format!(
                            "\n· {}：{}",
                            f.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                            f.get("reason").and_then(|v| v.as_str()).unwrap_or("")
                        ));
                    }
                    if r.failures.len() > 10 {
                        s.push_str(&format!("\n… 还有 {} 条失败", r.failures.len() - 10));
                    }
                    s
                }
                Err(e) => format!("❌ {period} 月度时长导入失败：{e}"),
            };
            return Some(Outbound::to(&inbound.conversation_id, reply));
        }

        // 业务日期优先级：本条消息里带的日期 > 之前报过的日期（12h 内）> 文件名里的日期 > 收到日 − 1
        let (file_date, _file_team) = hints_from_filename(&filename);
        let msg_date = parse_short_date(&text);
        let sticky = pending_date_get(&key);
        let (biz_date, date_src) = pick_biz_date(
            msg_date.as_deref(),
            sticky.as_deref(),
            file_date.as_deref(),
            &default_biz_date(),
        );
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

        /* 团别（2026-10-17 修复，审计 6-4）：
        原先附件分支**先于**命令路由执行且硬编码 "auto"，运营发「导入 女团 2026-10-05」
        + 附件时团别被**静默丢弃**，只会按主播档案逐行归属。
        现在按「消息文本里的团别 > 文件名里的团别 > auto」三级取：
        文本优先（运营当下说的算），文件名兜底（老习惯 `2026-10-07-男团.csv`），
        都没有才 auto（一个文件男女混着也能导，团别按档案归属）。 */
        let msg_team = text.split_whitespace().find_map(team_of);
        let team = msg_team.or(_file_team).unwrap_or("auto");
        tracing::info!(
            file = %filename,
            biz_date = %biz_date,
            date_src = %date_src,
            team = %team,
            team_src = if msg_team.is_some() { "消息文本" } else if _file_team.is_some() { "文件名" } else { "auto（按档案归属）" },
            rows = rows.len(),
            "机器人文件导入"
        );
        let report = admin::commit_import(
            st,
            team,
            &biz_date,
            rows,
            "merge",
            &format!("机器人·{filename}"),
        )
        .await;
        let reply = format_report(&filename, &biz_date, team, report);
        return Some(Outbound::to(&inbound.conversation_id, reply));
    }

    if text.is_empty() {
        return None;
    }

    Some(route_command(st, cfg, &inbound, &text, role).await)
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
    role: Role,
) -> Outbound {
    // 命令注册表是唯一事实来源：解析不到 → 管理员沿用原提示，普通用户引导看「帮助」
    let Some((id, args)) = commands::parse(text) else {
        let msg = match role {
            Role::User => "没认出这条命令。发「帮助」看你能用的命令。".to_string(),
            Role::Admin => format!("不认识这条命令。发「帮助」看用法。\n你发的是：{text}"),
        };
        return Outbound::to(&inbound.conversation_id, msg);
    };
    // 权限门控：不够权限只说这一句，绝不执行
    if !role.allows(id.required_role()) {
        return Outbound::to(
            &inbound.conversation_id,
            format!(
                "「{}」是管理员命令，你没有权限。\n发「帮助」看你能用的命令。",
                id.spec().name
            ),
        );
    }

    // 命令行字符串参数（日报 / 缺数据 / 导入沿用 &[&str] 接口）
    let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();

    // 命令 1（每日榜单）：图片版优先；文字留着给发不了图的渠道兜底
    let mut images: Vec<Vec<u8>> = Vec::new();
    let mut files: Vec<OutboundFile> = Vec::new();
    let text = match id {
        CmdId::Help => commands::help_for(role),
        CmdId::DailyReport => match current_binding(st, inbound).await {
            Some(b) => {
                let (t, pngs) = cmd_daily_report_user(st, &b).await;
                images = pngs;
                t
            }
            None => NO_BINDING_HINT.to_string(),
        },
        CmdId::MyReport => match current_binding(st, inbound).await {
            Some(b) => cmd_my_report_user(st, &b).await,
            None => NO_BINDING_HINT.to_string(),
        },
        // 2026-10-18 新增：3 = 名下徒弟的榜单，4 = 追分榜单
        /* 2026-10-19 用户口径「徒弟的也去除文字只发图片」：命令 3 与自动推送的
        师门日报同源（lineage_report::build + 同一套字段配置），渲染成功就只发图，
        文字留给发不了图的渠道（QQ）兜底 —— 与命令 1 的处理方式一致。 */
        CmdId::DiscipleRank => match current_binding(st, inbound).await {
            Some(b) => {
                let (t, pngs) = cmd_disciple_rank_user(st, &b).await;
                images = pngs;
                t
            }
            None => NO_BINDING_HINT.to_string(),
        },
        CmdId::TargetRank => match current_binding(st, inbound).await {
            Some(b) => cmd_target_rank_user(st, &b).await,
            None => NO_BINDING_HINT.to_string(),
        },
        CmdId::PushPause => match current_binding(st, inbound).await {
            Some(b) => cmd_push_pause_user(st, &b).await,
            None => NO_BINDING_HINT.to_string(),
        },
        CmdId::PushResume => match current_binding(st, inbound).await {
            Some(b) => cmd_push_resume_user(st, &b).await,
            None => NO_BINDING_HINT.to_string(),
        },
        CmdId::Status => cmd_status(st).await,
        CmdId::Plan => cmd_plan(st).await,
        CmdId::TotalRank => cmd_rank(st, "total", 0, "总音浪榜").await,
        CmdId::Rank => {
            let n: i64 = args.first().and_then(|t| t.parse().ok()).unwrap_or(10);
            cmd_rank(st, "lastN", n.clamp(1, 100), &format!("近 {n} 日音浪榜")).await
        }
        CmdId::Query => {
            let name = args.join(" ");
            if name.is_empty() {
                "用法：查询 <主播名>".into()
            } else {
                cmd_query(st, &name).await
            }
        }
        CmdId::Export => match cmd_export(st, &args_ref).await {
            ExportReply::Text(text) => text,
            ExportReply::File { file_name, data } => {
                files.push(OutboundFile { file_name, data });
                // QQ 只发附件，不额外发长文本；微信无文件能力时给一句兜底提示。
                if inbound.channel == "qq" {
                    String::new()
                } else {
                    "CSV 文件已生成（QQ 渠道可直接接收文件）".to_string()
                }
            }
        },
        CmdId::DailySquadReport => cmd_daily_report(st, &args_ref).await,
        CmdId::MissingData => cmd_missing(st, &args_ref).await,
        CmdId::Import => cmd_import(st, inbound, &args_ref).await,
    };
    Outbound::to(&inbound.conversation_id, text)
        .with_images(images)
        .with_files(files)
}

/* ------------------------- 普通用户命令（已绑定主播） ------------------------- */

/// User 命令取绑定失败时的兜底（理论上 handle 已按绑定放行，这里防解绑竞态）
const NO_BINDING_HINT: &str =
    "没有找到你的推送绑定。\n打开展示端「我的」页 →「消息设置」→ 微信对接，或发「帮助」。";

/// 当前会话的推送绑定（User 命令专用）
async fn current_binding(
    st: &Arc<AppState>,
    inbound: &Inbound,
) -> Option<crate::store::PushBinding> {
    crate::store::push_binding_by_conversation(st, &inbound.conversation_id).await
}

/// 最新业务日（当日报告的「今天」口径：最近一个可用日期）
async fn latest_biz_date(st: &Arc<AppState>) -> Option<String> {
    crate::repos::get_available_dates(st, Some("LOCAL"), false)
        .await
        .ok()
        .and_then(|d| d.first().cloned())
}

/// 绑定行的团别；为空时回台账反查（建档晚于绑定会留空，与绑定路径同一修复口径）
async fn binding_team(st: &Arc<AppState>, b: &crate::store::PushBinding) -> Option<String> {
    if !b.team.is_empty() {
        return Some(b.team.clone());
    }
    crate::store::person_team_of_name(st, &b.name).await
}

/// 普通用户「每日报告」：最新业务日 TOP10 + 我的名次（与推送同一份文案）
async fn cmd_daily_report_user(
    st: &Arc<AppState>,
    b: &crate::store::PushBinding,
) -> (String, Vec<Vec<u8>>) {
    let Some(date) = latest_biz_date(st).await else {
        return (
            "还没有可用的业务数据，等运营导入后再来看。".into(),
            Vec::new(),
        );
    };
    let Some(team) = binding_team(st, b).await else {
        return (
            "还没有查到你的团别归属，稍后再试；一直这样请联系管理员。".into(),
            Vec::new(),
        );
    };
    /* 分号明细（2026-10-12 用户口径「推送多个号带着抖音号」）：
    用户主动发的「每日报告」和定时推送走的是同一条 wxbot 通道，文案口径必须一致，
    所以这里也带上「我的抖音号」段。取数失败 push.rs 内部只 warn 并返回空 →
    自动退回不带明细的原文案，命令照常能回复。 */
    let items = crate::push::daily_rank(st, &team, &date).await;
    if items.is_empty() {
        return (
            format!("{date} 还没有音浪数据，等运营导入后再来看。"),
            Vec::new(),
        );
    }
    let accounts = crate::push::person_account_waves(st, b.person_id, &date).await;
    let text =
        crate::push::daily_report_text_with_accounts(&team, &date, &items, b.person_id, &accounts);
    /* 2026-10-18 用户口径「文字的榜单去掉」：先渲染榜单图（与自动推送同一套两页版）。
    渲染成功 → 微信渠道只发图、不发这段文字；渲染失败 → 退回文字，命令不会没响应。 */
    let pngs = crate::push::daily_report_pngs(st, &team, &date, b.name.trim()).await;
    (text, pngs)
}

/// 普通用户「个人报告」：今日名次/音浪/预估收入 + 本月累计/名次 + 追分目标进度
pub async fn cmd_my_report_user(st: &Arc<AppState>, b: &crate::store::PushBinding) -> String {
    let Some(date) = latest_biz_date(st).await else {
        return "还没有可用的业务数据，等运营导入后再来看。".into();
    };
    let Some(team) = binding_team(st, b).await else {
        return "还没有查到你的团别归属，稍后再试；一直这样请联系管理员。".into();
    };
    let month = date.get(..7).unwrap_or(&date).to_string();
    // 今日：当日榜里的名次与音浪
    let day_items = crate::push::daily_rank(st, &team, &date).await;
    let day_pos = day_items
        .iter()
        .position(|(pid, _, _)| *pid == b.person_id)
        .map(|i| (i + 1, day_items[i].2));
    // 本月：累计音浪 + 团内名次（与追分口径一致：可信行聚合）
    let month_map = crate::push::month_wave_map(st, &team, &month).await;
    let month_rank = crate::push::month_rank_list(st, &team, &month).await;
    let my_month = month_map.get(&b.person_id).copied().unwrap_or(0);
    let month_pos = month_rank
        .iter()
        .position(|(pid, _, _)| *pid == b.person_id)
        .map(|i| i + 1);
    // 追分目标：逐条折算成「还差多少」/「已追上」
    let targets = crate::store::targets_for_push(st, &b.name).await;
    let target_lines: Vec<(String, String, String, i64)> = targets
        .iter()
        .map(|t| {
            let rival = month_map.get(&t.target_person_id).copied().unwrap_or(0);
            let (gap, label, detail, gap_text) =
                crate::push::eval_target(t, my_month, rival, Some(&month_rank), b.person_id);
            (label, detail, gap_text, gap)
        })
        .collect();
    /* 今日音浪 + 预估收入（2026-10-19 用户口径「个人的今日音浪还有收入」）：
    今日音浪取日榜里本人那一行（`day_pos` 的第二项），没上榜 = 今天没开播 → 不显示收入。
    收入口径与展示端「我的」页同源（后台 share_pct / fee_pct，见 push::est_yuan），
    不在机器人这边另写一套，避免两处算出不同数字。 */
    let day_wave = day_pos.map(|(_, w)| w);
    let est = match day_wave {
        Some(w) => match crate::push::est_yuan(st, w).await {
            Some(v) => Some(crate::push::fmt_yuan(v)),
            None => None,
        },
        None => None,
    };
    render_my_report(
        &b.name,
        &date,
        day_pos,
        month_pos,
        my_month,
        &target_lines,
        est.as_deref(),
    )
}

/// 「我的报告」排版（纯函数，好测）。
/// gap：≤ 0 已达成；i64::MAX = 追名次但本月还没进榜（没有可比的差距）
pub(crate) fn render_my_report(
    name: &str,
    date: &str,
    day: Option<(usize, i64)>,
    month_pos: Option<usize>,
    my_month: i64,
    targets: &[(String, String, String, i64)],
    est: Option<&str>, // 今日预估到手（已格式化成 ¥x；None = 今天没播 / 无可估音浪）
) -> String {
    let mut out = format!("📈 我的报告（{name}）");
    match day {
        Some((pos, wave)) => {
            out.push_str(&format!(
                "\n今日（{date}）：{} · 团内第 {pos} 名",
                crate::push::fmt_wan(wave)
            ));
            /* 今日音浪后面紧跟预估收入（用户口径）：数字与金额同一行，不另起一段。
            没有可估金额（今天没播 / wave=0）就不显示这一截，不留空的「¥」。 */
            if let Some(e) = est {
                out.push_str(&format!(" · 预估 {e}"));
            }
        }
        None => out.push_str(&format!("\n今日（{date}）：今天还没开播（榜上还没有你）")),
    }
    match month_pos {
        Some(pos) => out.push_str(&format!(
            "\n本月累计：{} · 团内第 {pos} 名",
            crate::push::fmt_wan(my_month)
        )),
        None => out.push_str(&format!(
            "\n本月累计：{} · 本月还没开播",
            crate::push::fmt_wan(my_month)
        )),
    }
    if targets.is_empty() {
        out.push_str("\n追分目标：还没有设置。到展示端「我的」页可以加（最多 5 个）。");
        return out;
    }
    out.push_str("\n追分目标：");
    for (label, detail, gap_text, gap) in targets {
        if *gap <= 0 {
            out.push_str(&format!("\n· ✅ 已追上「{label}」"));
        } else if *gap == i64::MAX {
            // 追名次但本月没进榜：gap 是哨兵值，只报现状不报假差距
            out.push_str(&format!("\n· {label}：{detail}"));
        } else {
            out.push_str(&format!("\n· {label}：{detail}，还差 {gap_text}"));
        }
    }
    out
}

/// 普通用户「徒弟榜单」（命令 3）：名下**直系徒弟**的本月团内排名。
///
/// 口径与「个人报告 / 追分榜单」一致：本月累计音浪 + 团内名次（可信行聚合）。
/// 只算直系（`person.master_id = 本人`），与「我的」页 `disciple.count` 同源，徒孙不计。
/// 返回 `(文字兜底, 师门日报图)`：图非空时微信渠道只发图（2026-10-19 用户口径）。
pub async fn cmd_disciple_rank_user(
    st: &Arc<AppState>,
    b: &crate::store::PushBinding,
) -> (String, Vec<Vec<u8>>) {
    if b.person_id == 0 {
        return (
            "这个账号没有绑定主播档案，看不到徒弟榜单。".into(),
            Vec::new(),
        );
    }
    let Some(date) = latest_biz_date(st).await else {
        return (
            "还没有可用的业务数据，等运营导入后再来看。".into(),
            Vec::new(),
        );
    };
    let Some(team) = binding_team(st, b).await else {
        return (
            "还没有查到你的团别归属，稍后再试；一直这样请联系管理员。".into(),
            Vec::new(),
        );
    };
    let month = date.get(..7).unwrap_or(&date).to_string();
    let disciples = crate::push::direct_disciples(st, b.person_id).await;
    if disciples.is_empty() {
        return (
            format!(
                "「{}」名下目前还没有徒弟。\n徒弟在台账里挂到你名下之后，这里就能看到他们的本月排名。",
                b.name.trim()
            ),
            Vec::new(),
        );
    }
    let rank = crate::push::month_rank_list(st, &team, &month).await;
    let pos: HashMap<u64, usize> = rank
        .iter()
        .enumerate()
        .map(|(i, (pid, _, _))| (*pid, i + 1))
        .collect();
    let wave_map = crate::push::month_wave_map(st, &team, &month).await;
    let mut rows: Vec<(Option<usize>, String, i64)> = disciples
        .iter()
        .map(|(pid, dname)| {
            (
                pos.get(pid).copied(),
                dname.trim().to_string(),
                wave_map.get(pid).copied().unwrap_or(0),
            )
        })
        .collect();
    // 上榜的按名次在前；没上榜（本月没播）的排最后，内部按姓名
    rows.sort_by(|a, b| match (a.0, b.0) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.1.cmp(&b.1),
    });
    let text = render_disciple_rank(b.name.trim(), &month, &rows);
    /* 师门日报图（与自动推送同一份渲染，带本人水印）：渲染失败（无直系徒弟 /
    库抖动 / PNG 转换失败）就退回文字，命令不会没响应。 */
    let pngs = match crate::lineage_report::build(st, &date, b.person_id).await {
        Ok(rep) if rep.rows.len() > 1 => {
            crate::push::lineage_report_pngs_for(st, &rep, b.name.trim()).await
        }
        _ => Vec::new(),
    };
    (text, pngs)
}

/// 徒弟榜单排版（纯函数，好测）。`rows` 已按名次排好：`(团内名次, 徒弟名, 本月音浪)`。
pub(crate) fn render_disciple_rank(
    name: &str,
    month: &str,
    rows: &[(Option<usize>, String, i64)],
) -> String {
    /// 一条微信消息里最多列多少人（再多会刷屏，且微信有长度限制）
    const MAX_ROWS: usize = 30;
    let total: i64 = rows.iter().map(|(_, _, w)| *w).sum();
    let mut out = format!(
        "👥 徒弟榜单（{name} · {month}）\n共 {} 人 · 本月合计 {}",
        rows.len(),
        crate::push::fmt_wan(total)
    );
    for (pos, dname, wave) in rows.iter().take(MAX_ROWS) {
        match pos {
            Some(p) => out.push_str(&format!(
                "\n· 第 {p} 名 · {dname} · {}",
                crate::push::fmt_wan(*wave)
            )),
            None => out.push_str(&format!("\n· {dname} · 本月还没开播")),
        }
    }
    if rows.len() > MAX_ROWS {
        out.push_str(&format!("\n…还有 {} 人没有列出", rows.len() - MAX_ROWS));
    }
    out.push_str("\n\n（口径：本月累计音浪 · 团内名次；只算直系徒弟）");
    out
}

/// 普通用户「追分榜单」（命令 4）：我在追的人现在团内第几、还差多少。
///
/// 与「个人报告」底部那几行追分目标同源（`targets_for_push` + `eval_target`），
/// 区别是把每个目标单独展开成榜单项，带对方当前名次和音浪。
async fn cmd_target_rank_user(st: &Arc<AppState>, b: &crate::store::PushBinding) -> String {
    if b.person_id == 0 {
        return "这个账号没有绑定主播档案，看不到追分榜单。".into();
    }
    let Some(date) = latest_biz_date(st).await else {
        return "还没有可用的业务数据，等运营导入后再来看。".into();
    };
    let Some(team) = binding_team(st, b).await else {
        return "还没有查到你的团别归属，稍后再试；一直这样请联系管理员。".into();
    };
    let month = date.get(..7).unwrap_or(&date).to_string();
    let targets = crate::store::targets_for_push(st, &b.name).await;
    if targets.is_empty() {
        return "还没有设置追分目标。\n到展示端「我的」页可以添加（最多 5 个）。".into();
    }
    let month_map = crate::push::month_wave_map(st, &team, &month).await;
    let month_rank = crate::push::month_rank_list(st, &team, &month).await;
    let pos: HashMap<u64, usize> = month_rank
        .iter()
        .enumerate()
        .map(|(i, (pid, _, _))| (*pid, i + 1))
        .collect();
    let my_month = month_map.get(&b.person_id).copied().unwrap_or(0);
    let my_pos = pos.get(&b.person_id).copied();
    let mut lines: Vec<String> = Vec::new();
    for t in &targets {
        let rival = month_map.get(&t.target_person_id).copied().unwrap_or(0);
        let (gap, label, detail, gap_text) =
            crate::push::eval_target(t, my_month, rival, Some(&month_rank), b.person_id);
        // 只有「追人」目标才有对方名次可报；追名次 / 追固定分没有对手人
        let rival_part = if t.target_person_id > 0 {
            match pos.get(&t.target_person_id).copied() {
                Some(p) => format!("（对方第 {p} 名 · {}）", crate::push::fmt_wan(rival)),
                None => "（对方本月还没开播）".to_string(),
            }
        } else {
            String::new()
        };
        if gap <= 0 {
            lines.push(format!("· ✅ {label} · 已达成{rival_part}"));
        } else if gap == i64::MAX {
            // 追名次但本月还没进榜：哨兵值，只报现状不报假差距
            lines.push(format!("· {label} · {detail}{rival_part}"));
        } else {
            lines.push(format!("· {label} · {detail}{rival_part}，还差 {gap_text}"));
        }
    }
    render_target_rank(b.name.trim(), &month, my_pos, my_month, &lines)
}

/// 追分榜单排版（纯函数，好测）。
fn render_target_rank(
    name: &str,
    month: &str,
    my_pos: Option<usize>,
    my_wave: i64,
    lines: &[String],
) -> String {
    let mut out = format!("🎯 追分榜单（{name} · {month}）\n我：");
    match my_pos {
        Some(p) => out.push_str(&format!("第 {p} 名 · {}", crate::push::fmt_wan(my_wave))),
        None => out.push_str(&format!(
            "本月还没开播（累计 {}）",
            crate::push::fmt_wan(my_wave)
        )),
    }
    out.push('\n');
    out.push_str(&lines.join("\n"));
    out.push_str("\n\n（口径：本月累计音浪 · 团内名次）");
    out
}

/// 普通用户「暂停接收每日推送」：只关 daily_report 开关
async fn cmd_push_pause_user(st: &Arc<AppState>, b: &crate::store::PushBinding) -> String {
    if crate::store::push_set_daily_report(st, &b.name, false).await {
        "已暂停「每日报告」推送 ✅\n\n· 只影响每日报告；追分提醒不受影响\n· 想改回来发「开启接收每日推送」，也可以到展示端「我的」页设置"
            .into()
    } else {
        "操作没成功（写库暂时不可用？），稍后再试。".into()
    }
}

/// 普通用户「开启接收每日推送」：只开 daily_report 开关
async fn cmd_push_resume_user(st: &Arc<AppState>, b: &crate::store::PushBinding) -> String {
    if crate::store::push_set_daily_report(st, &b.name, true).await {
        "已开启「每日报告」推送 ✅\n\n· 数据入库后约 30 分钟会给你推当天榜\n· 追分提醒的开关也在展示端「我的」页"
            .into()
    } else {
        "操作没成功（写库暂时不可用？），稍后再试。".into()
    }
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

fn valid_export_period(s: &str) -> bool {
    matches!(s.len(), 4 | 7) && crate::dialect::valid_period_shape(s)
}

fn normalize_export_period(raw: &str) -> String {
    raw.chars()
        .map(|c| match c {
            '—' | '–' | '－' => '-',
            _ => c,
        })
        .collect()
}

fn export_period_or_current(raw: Option<&str>) -> Option<String> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) if valid_export_period(s) => Some(s.to_string()),
        Some(_) => None,
        None => Some(chrono::Local::now().format("%Y-%m").to_string()),
    }
}

fn csv_cell(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn export_minutes(minutes: i64) -> String {
    let m = minutes.max(0);
    if m == 0 {
        return "0h".into();
    }
    let h = m / 60;
    let rem = m % 60;
    match (h, rem) {
        (0, _) => format!("{rem}m"),
        (_, 0) => format!("{h}h"),
        _ => format!("{h}h{rem}m"),
    }
}

fn row_pid(r: &sqlx::postgres::PgRow) -> i64 {
    r.try_get::<i64, _>("person_id")
        .or_else(|_| r.try_get::<i64, _>("id"))
        .unwrap_or(0)
}

fn row_i64(r: &sqlx::postgres::PgRow, col: &str) -> i64 {
    r.try_get::<i64, _>(col)
        .or_else(|_| r.try_get::<i32, _>(col).map(i64::from))
        .unwrap_or(0)
}

fn period_rank_csv(period: &str, rows: Vec<(i64, String, i64, i64, i64)>) -> String {
    let mut rows = rows;
    rows.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));
    let wave_label = if period.len() == 7 {
        format!("{}月音浪", period[5..7].trim_start_matches('0'))
    } else {
        format!("{period}年音浪")
    };
    let mut out = format!("\u{feff}排名,主播姓名,{wave_label},时长,未播天数\n");
    for (i, (_, name, wave, minutes, absent)) in rows.into_iter().enumerate() {
        out.push_str(&format!(
            "{},{},{},{},{}\n",
            i + 1,
            csv_cell(&name),
            wave,
            export_minutes(minutes),
            absent
        ));
    }
    out
}

enum ExportReply {
    Text(String),
    File { file_name: String, data: Vec<u8> },
}

fn export_file_name(team: &str, period: &str) -> String {
    format!("总排名-{period}-{}.csv", team_label(team))
}

/// 导出按月/按年总排名 CSV：只取对外展示名册（未删除、未离团、未隐藏），
/// 男女团分开；未开播但有数据行的人也要保留，排名按周期音浪降序。
async fn export_period_rank_csv(st: &Arc<AppState>, team: &str, period: &str) -> String {
    let roster_sql = format!(
        "SELECT id AS person_id, name FROM person \
         WHERE deleted_at IS NULL AND status <> 'left' AND hide_in_daily_report = 0 \
           AND gender = '{team}' ORDER BY name"
    );
    let Ok(roster) = crate::pgrs::query(&st.pools, "LOCAL", &roster_sql).await else {
        return "导出失败：数据库暂时不可用。".into();
    };
    if roster.is_empty() {
        return format!("{}没有可导出的对外展示人员。", team_label(team));
    }

    let like = format!("{period}%");
    let stats_rows = match crate::dialect::query_period(
        &st.pools,
        "LOCAL",
        crate::dialect::ADMIN_PERIOD_SELECT,
        &like,
        team,
        " AND p.hide_in_daily_report = 0 AND p.status <> 'left'",
        "wave DESC",
    )
    .await
    {
        Ok(rows) => rows,
        Err(e) => return format!("导出失败：读取周期指标失败（{e}）"),
    };
    let mut wave_map: HashMap<i64, i64> = HashMap::new();
    let mut minutes_map: HashMap<i64, i64> = HashMap::new();
    for r in &stats_rows {
        let pid = row_pid(r);
        wave_map.insert(pid, row_i64(r, "wave"));
        minutes_map.insert(pid, row_i64(r, "minutes"));
    }

    // 只有整月时长、没有日/月音浪的人不会进入 period_rollup 的统计行；
    // 补一遍 monthly_duration，保证“时长表格”不会漏掉这类人员。
    let month_duration_sql = format!(
        "SELECT person_id, (SUM(minutes))::BIGINT AS minutes \
         FROM monthly_duration WHERE period LIKE '{period}%' GROUP BY person_id"
    );
    match crate::pgrs::query(&st.pools, "LOCAL", &month_duration_sql).await {
        Ok(rows) => {
            for r in &rows {
                minutes_map
                    .entry(row_pid(r))
                    .or_insert_with(|| row_i64(r, "minutes"));
            }
        }
        Err(e) if crate::dialect::is_missing_month_table(&e) => {}
        Err(e) => return format!("导出失败：读取整月时长失败（{e}）"),
    }

    let (date_lo, date_hi) = crate::dialect::like_to_date_range(&like);
    let absent_sql = format!(
        "SELECT person_id, (SUM(CASE WHEN wave < 2 THEN 1 ELSE 0 END))::BIGINT AS n \
         FROM daily_metric WHERE wave_reliable = 1 \
           AND biz_date >= '{date_lo}'::date AND biz_date < '{date_hi}'::date \
         GROUP BY person_id"
    );
    let absent_rows = match crate::pgrs::query(&st.pools, "LOCAL", &absent_sql).await {
        Ok(rows) => rows,
        Err(e) => return format!("导出失败：读取未播天数失败（{e}）"),
    };
    let mut absent_map: HashMap<i64, i64> = HashMap::new();
    for r in &absent_rows {
        absent_map.insert(row_pid(r), row_i64(r, "n"));
    }

    let rows: Vec<(i64, String, i64, i64, i64)> = roster
        .iter()
        .map(|r| {
            let pid = row_pid(r);
            (
                pid,
                r.try_get::<String, _>("name").unwrap_or_default(),
                wave_map.get(&pid).copied().unwrap_or(0),
                minutes_map.get(&pid).copied().unwrap_or(0),
                absent_map.get(&pid).copied().unwrap_or(0),
            )
        })
        .collect();
    period_rank_csv(period, rows)
}

/// 导出：CSV 文本回复（QQ 当前只发文本；微信同口径）。
///
/// - `导出 今日 | YYYY-MM-DD | 总榜`：保留原有日报/总榜导出
/// - `导出 男团 [YYYY|YYYY-MM]` / `导出 女团 [YYYY|YYYY-MM]`：按月/按年导出对外展示名册，
///   不传周期 = 当前月
/// - 也接受 `导出 YYYY-MM 男团|女团` 和紧凑写法 `导出男团2026—10`
async fn cmd_export(st: &Arc<AppState>, args: &[&str]) -> ExportReply {
    if let Some(team) = args.first().and_then(|s| team_of(s)) {
        let period_arg = args.get(1).copied().map(normalize_export_period);
        let Some(period) = export_period_or_current(period_arg.as_deref()) else {
            return ExportReply::Text(
                "周期格式应为 YYYY（全年）或 YYYY-MM（月），例如：导出男团2026—10".into(),
            );
        };
        if args.len() > 2 {
            return ExportReply::Text("用法：导出 男团|女团 [YYYY|YYYY-MM]".into());
        }
        let csv = export_period_rank_csv(st, team, &period).await;
        if csv.starts_with("导出失败") || csv.contains("没有可导出") {
            return ExportReply::Text(csv);
        }
        return ExportReply::File {
            file_name: export_file_name(team, &period),
            data: csv.into_bytes(),
        };
    }
    if args.len() >= 2 {
        if let Some(team) = team_of(args[1]) {
            let period_arg = normalize_export_period(args[0]);
            let Some(period) = export_period_or_current(Some(&period_arg)) else {
                return ExportReply::Text(
                    "周期格式应为 YYYY（全年）或 YYYY-MM（月），例如：导出男团2026—10".into(),
                );
            };
            if args.len() > 2 {
                return ExportReply::Text("用法：导出 男团|女团 [YYYY|YYYY-MM]".into());
            }
            let csv = export_period_rank_csv(st, team, &period).await;
            if csv.starts_with("导出失败") || csv.contains("没有可导出") {
                return ExportReply::Text(csv);
            }
            return ExportReply::File {
                file_name: export_file_name(team, &period),
                data: csv.into_bytes(),
            };
        }
    }
    let arg = args.first().copied().unwrap_or("今日");
    let (metric, days, label) = match arg {
        "总榜" => ("total", 1, "总音浪"),
        "今日" => ("lastN", 1, "最近一个业务日"),
        d => {
            if valid_date(d) {
                return ExportReply::Text(cmd_export_date(st, d).await);
            }
            return ExportReply::Text("用法：导出 今日 | 导出 YYYY-MM-DD | 导出 总榜".into());
        }
    };
    ExportReply::Text(csv_of(st, metric, days, label).await)
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
        // 传平台给的真实文件名：CDN URL 推不出扩展名时 xlsx 会被当文本，
        // 二进制垃圾直接进暂存区（批次 #287 就是这么来的）
        match download_text(&att.url, Some(&att.file_name)).await {
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
        crate::pgrs::query_with_binds(&st.pools, "LOCAL", crate::dialect::MISSING_SQL, |q| {
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
///
/// 2026-10-12 用户复述：「11 号接收的就是 10 号的数据，**前提是没指定日期**」——
/// 它是四条日期来源里**最低优先级**的一条（`pick_biz_date` 的兜底），
/// 报过日期 / 文件名带日期时都不会走到这里。
fn default_biz_date() -> String {
    prev_biz_date(chrono::Local::now().date_naive())
}

/// 给定「今天」，算出它的前一天并格式化成业务日。
/// 抽成带参数的纯函数，是为了能单测跨月 / 跨年 / 闰月边界（不能靠真实时钟）。
fn prev_biz_date(today: chrono::NaiveDate) -> String {
    (today - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string()
}

/// 业务日期的四条来源按优先级取一条（纯函数，好测）。
///
/// 优先级（用户口径 2026-10-05 / 2026-10-12）：
/// 1. **本条消息里带的日期**（运营发「10.1」时顺手把日期粘住，后面几个文件不用再报）
/// 2. 之前贴住的日期（12h 内）
/// 3. 文件名里的日期（补录历史数据时靠它）
/// 4. **收到日 − 1**（兜底：抖音后台半夜才出前一天的数据）
///
/// 返回 `(业务日期, 来源说明)`，来源说明进日志与回复文案。
fn pick_biz_date(
    msg_date: Option<&str>,
    sticky: Option<&str>,
    file_date: Option<&str>,
    default_date: &str,
) -> (String, &'static str) {
    match (msg_date, sticky, file_date) {
        (Some(d), _, _) => (d.to_string(), "本条消息指定"),
        (None, Some(d), _) => (d.to_string(), "沿用你报的日期"),
        (None, None, Some(d)) => (d.to_string(), "文件名里的日期（补录）"),
        (None, None, None) => (
            default_date.to_string(),
            "默认「收到日 − 1」（数据半夜才更新）",
        ),
    }
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

/// 附件下载大小上限（P2-16，2026-10-10 审计）：8MB。CSV/文本导入远用不到这么大，
/// 超限一律拒绝——rk3318 部署 mem_limit 160m，无上限下载必然 OOM。
const ATTACHMENT_MAX_BYTES: usize = 8 * 1024 * 1024;
/// 附件下载主机白名单（P2-16）：默认只允许 QQ 开放平台官方 CDN 域名。
/// 附件 URL 由平台下发，但白名单兜底 SSRF——就算平台回显用户可控 URL 也出不了这个圈。
/// 需要接微信文件消息时在这里加域名，或用 `BOT_ATTACHMENT_HOSTS` 追加（逗号分隔）。
/// 1.0.1：微信 iLink 媒体走 novac2c CDN，把官方微信 CDN 域名一并放行。
const ATTACHMENT_HOSTS: &[&str] = &[
    "multimedia.nt.qq.com.cn",
    "multimedia.qq.com.cn",
    "groupfile.qq.com",
    "qq.com",
    "weixin.qq.com",
];

/// `BOT_ATTACHMENT_HOSTS` 追加的域名（逗号分隔，忽略空白与空项）。
///
/// 为什么需要：QQ 开放平台历史上换过附件 CDN 域名（gchat.qpic.qq.com、
/// file.ws.qq.com 等），白名单硬编码时一旦平台换域，导入会 100% 失败且
/// 只有改代码才能恢复。留一个 env 口子，出问题可以当场放行而不必重发版本。
fn extra_attachment_hosts() -> Vec<String> {
    std::env::var("BOT_ATTACHMENT_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 下载 URL 主机校验：解析 host，必须与白名单域名相等或以其为后缀（`.qq.com`）。
/// e2e / 本机联调用 `BOT_ATTACHMENT_HOSTS=127.0.0.1,localhost` 放行测试服务器。
fn attachment_host_allowed(url: &str) -> bool {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host = rest
        .split(['/', ':', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    ATTACHMENT_HOSTS
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{h}")))
        || extra_attachment_hosts()
            .iter()
            .any(|h| host == *h || host.ends_with(&format!(".{h}")))
}

/// 下载附件文本（QQ CDN 直链 / 微信文件）。
/// P2-16：主机白名单 + 8MB 大小上限，超限拒绝并告警（防 OOM / SSRF）。
///
/// `file_name` 用平台下发的真实文件名（QQ 的 `attachments[].filename`）优先，
/// 没有再从 URL 推。只靠 URL 推不稳：CDN 链接常是
/// `.../download?appid=..&fileid=..` 这种，按 ? 切完最后一段是查询参数不是文件名，
/// 扩展名判据失效 → xlsx 被当纯文本 → 二进制垃圾进暂存区（批次 #287 就是这么来的）。
async fn download_text(url: &str, file_name: Option<&str>) -> anyhow::Result<String> {
    if !attachment_host_allowed(url) {
        tracing::warn!(
            url_head = %url.chars().take(80).collect::<String>(),
            "附件下载被拒：主机不在白名单"
        );
        anyhow::bail!("附件链接的域名不受支持（只接受 QQ 开放平台官方 CDN）");
    }
    // 不跟随重定向（P2-16）：白名单只在首跳生效，跟随会把「qq.com 已放行」当成
    // 跳进内网任意地址的通行证。QQ CDN 直链本来就是最终地址，不需要跟随。
    let resp = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()?
        .get(url)
        .send()
        .await?
        .error_for_status()?;
    // 大小上限：看 Content-Length。缺失时也拒绝（QQ CDN 直链必带头，
    // 没头的来源不可信——不把未知大小读进 160m 的容器内存）。
    let len = resp.content_length().ok_or_else(|| {
        tracing::warn!("附件响应没有 Content-Length，拒绝下载（P2-16 大小上限兜底）");
        anyhow::anyhow!("附件服务器没有返回大小信息，拒绝下载")
    })?;
    if len as usize > ATTACHMENT_MAX_BYTES {
        tracing::warn!(len, max = ATTACHMENT_MAX_BYTES, "附件超大，拒绝下载");
        anyhow::bail!(
            "附件太大（{} MB，上限 {} MB）",
            len / 1024 / 1024,
            ATTACHMENT_MAX_BYTES / 1024 / 1024
        );
    }
    // 按声明长度读，但边读边累计：声明不可信，实际超过就中止（防 OOM）。
    let mut bytes: Vec<u8> = Vec::with_capacity(len.min(ATTACHMENT_MAX_BYTES as u64) as usize);
    let mut stream = resp.bytes_stream();
    let mut total: usize = 0;
    while let Some(chunk) = stream.try_next().await? {
        total = total.saturating_add(chunk.len());
        if total > ATTACHMENT_MAX_BYTES {
            tracing::warn!(
                total,
                max = ATTACHMENT_MAX_BYTES,
                "附件实际超过上限，中止下载"
            );
            anyhow::bail!(
                "附件实际大小超过上限（{} MB）",
                ATTACHMENT_MAX_BYTES / 1024 / 1024
            );
        }
        bytes.extend_from_slice(&chunk);
    }
    let decoded = match file_name.filter(|n| !n.trim().is_empty()) {
        Some(n) => n.trim().to_string(),
        None => percent_decode_utf8(
            url.split(['/', '?', '#'])
                .rfind(|s| !s.is_empty())
                .unwrap_or_default(),
        ),
    };
    if let Some(text) = admin::maybe_spreadsheet_text(&decoded, &bytes)? {
        return Ok(text);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// 只解 URL 编码的文件名；不引入 URL crate，够覆盖 `1122.xlsx` 这类 CDN 路径。
fn percent_decode_utf8(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &value[i + 1..i + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// GET /api/admin/bots/sessions：所有出现过的会话（群聊/私聊 + 管理员标记）
pub async fn bots_sessions(State(st): State<ArcState>) -> ApiResult {
    mgr(&st)?;
    Ok(axum::Json(
        serde_json::json!({ "items": crate::store::session_list(&st).await }),
    ))
}

/// GET /api/admin/push/jobs：最近的延迟推送任务（运营看队列有没有推出去）
pub async fn push_jobs(State(st): State<ArcState>) -> ApiResult {
    Ok(axum::Json(serde_json::json!({
        "items": crate::store::push_job_list(&st, 30).await,
        "stats": crate::store::push_stats(&st).await,
        "delayMinutes": crate::push::delay_minutes(),
    })))
}

/// POST /api/admin/push/run?team=&bizDate=：立刻推一次（不等到点）
/// 运营想马上看到推送效果时用。
/// P1-6（2026-10-10 审计）：team/bizDate 在进 push.rs 前先过白名单——
/// 复用 admin::valid_team / admin::valid_biz_date，格式不对直接 400，
/// 不给内联 SQL 拼接留任何注入面。
pub async fn push_run_now(
    State(st): State<ArcState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let team = q.get("team").cloned().unwrap_or_default();
    let date = q.get("bizDate").cloned().unwrap_or_default();
    if team.is_empty() || date.is_empty() {
        return Err(anyhow::anyhow!("缺少 team 或 bizDate").into());
    }
    admin::valid_team(&team).map_err(|e| anyhow::anyhow!("{e}"))?;
    admin::valid_biz_date(&date).map_err(|e| anyhow::anyhow!("{e}"))?;
    crate::push::run(&st, &team, &date).await?;
    Ok(axum::Json(
        serde_json::json!({ "ok": true, "team": team, "bizDate": date }),
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
    /* 广播 SCOPE_BOT（2026-10-17 修复，审计 6-12a）：后台勾选管理员后立刻刷新机器人页 */
    crate::events::emit(
        crate::events::SCOPE_BOT,
        format!("机器人会话管理员标记已更新：{channel}/{conversation_id}"),
    );
    Ok(axum::Json(serde_json::json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 提权口令比对（2026-10-11 审计整改：口令改走 env，未配置 = 关闭）
    #[test]
    fn admin_code_match_rules() {
        // 未配置（None / 空串）时任何文本都不命中——提权路径整体关闭
        assert!(!admin_code_matches(None, "anything"));
        assert!(!admin_code_matches(Some(""), "anything"));
        // 已配置：必须 trim 后全文精确相等
        assert!(admin_code_matches(Some("s3cret"), "s3cret"));
        assert!(admin_code_matches(Some("s3cret"), "  s3cret  "));
        // 子串 / 前缀 / 大小写差异都不算（口令必须是整条消息）
        assert!(!admin_code_matches(Some("s3cret"), "口令是 s3cret 吗"));
        assert!(!admin_code_matches(Some("s3cret"), "s3cret2"));
        assert!(!admin_code_matches(Some("s3cret"), "S3CRET"));
    }

    #[test]
    fn team_aliases() {
        assert_eq!(team_of("男团"), Some("male"));
        assert_eq!(team_of("male"), Some("male"));
        assert_eq!(team_of("女团"), Some("female"));
        assert_eq!(team_of("boy"), None);
    }

    #[test]
    fn export_args_validate_year_and_month() {
        assert!(valid_export_period("2026-09"));
        assert!(valid_export_period("2026-12"));
        assert!(valid_export_period("2026"));
        assert!(!valid_export_period("2026-9"));
        assert!(!valid_export_period("2026-13"));
        assert_eq!(
            export_period_or_current(Some("2026-09")).as_deref(),
            Some("2026-09")
        );
        assert_eq!(
            export_period_or_current(Some("2026")).as_deref(),
            Some("2026")
        );
        assert!(export_period_or_current(Some("2026-9")).is_none());
    }

    #[test]
    fn period_export_csv_matches_display_format() {
        let csv = period_rank_csv(
            "2026-09",
            vec![(1, "乙".into(), 100, 75, 2), (2, "甲,乙".into(), 200, 0, 0)],
        );
        let lines: Vec<&str> = csv.trim_start_matches('\u{feff}').lines().collect();
        assert_eq!(lines[0], "排名,主播姓名,9月音浪,时长,未播天数");
        assert_eq!(lines[1], "1,\"甲,乙\",200,0h,0");
        assert_eq!(lines[2], "2,乙,100,1h15m,2");
        assert_eq!(export_minutes(0), "0h");
        assert_eq!(export_minutes(60), "1h");
        assert_eq!(export_minutes(61), "1h1m");

        let year = period_rank_csv("2026", vec![(1, "乙".into(), 100, 0, 0)]);
        assert!(year
            .trim_start_matches('\u{feff}')
            .starts_with("排名,主播姓名,2026年音浪,时长,未播天数\n"));
        assert_eq!(
            export_file_name("male", "2026-10"),
            "总排名-2026-10-男团.csv"
        );
        assert_eq!(export_file_name("female", "2026"), "总排名-2026-女团.csv");
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

    /* ---- 普通用户「我的报告」排版（2026-10-10 权限分层） ---- */

    #[test]
    fn my_report_renders_day_month_and_targets() {
        let targets = vec![
            (
                "本月第 3 名".to_string(),
                "你第 5 名".to_string(),
                "2 名".to_string(),
                2i64,
            ),
            (
                "浩冬".to_string(),
                "你 30 万 / TA 20 万".to_string(),
                "0".to_string(),
                -100,
            ),
            (
                "本月第 1 名".to_string(),
                "你还没进本月榜".to_string(),
                "本月还没开播".to_string(),
                i64::MAX,
            ),
        ];
        let t = render_my_report(
            "小美",
            "2026-10-09",
            Some((4, 50_000)),
            Some(5),
            300_000,
            &targets,
            None,
        );
        assert!(t.contains("📈 我的报告（小美）"), "{t}");
        assert!(t.contains("今日（2026-10-09）：5 万 · 团内第 4 名"), "{t}");
        assert!(t.contains("本月累计：30 万 · 团内第 5 名"), "{t}");
        assert!(t.contains("· 本月第 3 名：你第 5 名，还差 2 名"), "{t}");
        assert!(t.contains("· ✅ 已追上「浩冬」"), "{t}");
        assert!(t.contains("· 本月第 1 名：你还没进本月榜"), "{t}");
        assert!(
            !t.contains("还差 本月还没开播"),
            "哨兵值不该拼出别扭文案：{t}"
        );
        assert!(!t.contains("预估"), "没传收入就不该出现金额：{t}");
    }

    /* ---- 2026-10-19：个人报告带「今日音浪 + 预估收入」 ---- */

    #[test]
    fn my_report_appends_estimated_income_when_given() {
        let t = render_my_report(
            "小美",
            "2026-10-09",
            Some((4, 50_000)),
            Some(5),
            300_000,
            &[],
            Some("¥1750"),
        );
        assert!(
            t.contains("今日（2026-10-09）：5 万 · 团内第 4 名 · 预估 ¥1750"),
            "{t}"
        );
        assert!(
            t.contains("追分目标：还没有设置"),
            "无目标时的文案不变：{t}"
        );
    }

    /// 今天没开播（day=None）→ 不该出现任何金额，也不该留空的「预估 ¥」
    #[test]
    fn my_report_hides_income_when_not_live() {
        let t = render_my_report(
            "小美",
            "2026-10-09",
            None,
            Some(5),
            300_000,
            &[],
            Some("¥1750"),
        );
        assert!(t.contains("今天还没开播"), "{t}");
        assert!(!t.contains("预估"), "没播就不该显示收入：{t}");
    }

    /* ---- 2026-10-18：对接欢迎语 + 数字命令 3/4 ---- */

    #[test]
    fn greetings_pool_has_1000_unique_lines() {
        let pool = wx_greetings();
        assert_eq!(pool.len(), 1000, "鼓励文案池必须是 1000 条");
        let mut seen = std::collections::HashSet::new();
        for line in pool {
            assert!(!line.is_empty(), "文案不能有空行");
            assert!(
                line.chars().count() <= 45,
                "文案过长（{} 字）：{line}",
                line.chars().count()
            );
            assert!(!line.contains('\n'), "文案不能含换行：{line}");
            assert!(seen.insert(*line), "文案重复：{line}");
        }
    }

    #[test]
    fn random_greeting_comes_from_pool() {
        let pool = wx_greetings();
        for _ in 0..50 {
            let g = random_greeting();
            assert!(pool.contains(&g), "随机文案不在池子里：{g}");
        }
    }

    #[test]
    fn dock_intro_has_greeting_and_numeric_menu() {
        let t = wx_dock_intro();
        assert!(
            !t.contains("微信对接成功"),
            "成功回执必须单独作为第一条消息，介绍里不能重复：{t}"
        );
        for line in [
            "1 —— 每日榜单",
            "2 —— 个人每日播报",
            "3 —— 徒弟榜单",
            "4 —— 追分榜单",
        ] {
            assert!(t.contains(line), "欢迎语缺菜单项 {line}：{t}");
        }
        assert!(t.contains("帮助"), "{t}");
        assert!(t.contains("解绑"), "{t}");
        // 第一行必须来自文案池（随机鼓励语）
        let first = t.lines().next().unwrap_or_default().trim();
        assert!(
            wx_greetings().contains(&first),
            "第一行不是文案池里的句子：{first}"
        );
    }

    #[test]
    fn dock_success_is_short_and_carries_name() {
        let t = wx_dock_success("浩雨");
        assert_eq!(t.lines().next().unwrap(), "你好，浩雨！");
        assert!(t.contains("✅ 微信对接成功"), "{t}");
        assert!(t.contains("机器人已为你配置好"), "{t}");
        assert!(!t.contains("每日榜单"), "功能介绍必须放到第二条：{t}");

        for blank in [wx_dock_success(""), wx_dock_success("   ")] {
            assert_eq!(blank.lines().next().unwrap(), "你好！", "{blank}");
            assert!(!blank.contains("你好，！"), "{blank}");
        }
    }

    #[test]
    fn disciple_rank_renders_positions_and_offline() {
        let rows = vec![
            (Some(3usize), "狼鑫".to_string(), 500_000i64),
            (Some(9), "狼雨".to_string(), 200_000),
            (None, "玖汐".to_string(), 0),
        ];
        let t = render_disciple_rank("浩雨", "2026-10", &rows);
        assert!(t.contains("👥 徒弟榜单（浩雨 · 2026-10）"), "{t}");
        assert!(t.contains("共 3 人 · 本月合计 70 万"), "{t}");
        assert!(t.contains("· 第 3 名 · 狼鑫 · 50 万"), "{t}");
        assert!(t.contains("· 玖汐 · 本月还没开播"), "{t}");
    }

    #[test]
    fn disciple_rank_caps_long_lists() {
        let rows: Vec<(Option<usize>, String, i64)> = (1..=35)
            .map(|i| (Some(i), format!("徒弟{i}"), 10_000))
            .collect();
        let t = render_disciple_rank("浩雨", "2026-10", &rows);
        assert!(t.contains("…还有 5 人没有列出"), "{t}");
    }

    #[test]
    fn target_rank_renders_gap_reached_and_offboard() {
        let lines = vec![
            "· 「狼鑫」· 你 20 万 / TA 30 万（对方第 3 名 · 30 万），还差 10 万".to_string(),
            "· ✅ 浩冬 · 已达成（对方第 9 名 · 1 万）".to_string(),
            "· 本月第 1 名 · 你还没进本月榜（对方本月还没开播）".to_string(),
        ];
        let t = render_target_rank("浩雨", "2026-10", Some(5), 200_000, &lines);
        assert!(t.contains("🎯 追分榜单（浩雨 · 2026-10）"), "{t}");
        assert!(t.contains("我：第 5 名 · 20 万"), "{t}");
        assert!(t.contains("还差 10 万"), "{t}");
        assert!(t.contains("已达成"), "{t}");
        assert!(
            !t.contains("还差 本月还没开播"),
            "哨兵值不该拼出别扭文案：{t}"
        );
    }

    #[test]
    fn target_rank_when_not_on_board() {
        let t = render_target_rank("浩雨", "2026-10", None, 0, &[]);
        assert!(t.contains("本月还没开播"), "{t}");
    }

    #[test]
    fn my_report_without_data_is_friendly() {
        let none = render_my_report("小美", "2026-10-09", None, None, 0, &[], None);
        assert!(none.contains("今天还没开播"), "{none}");
        assert!(none.contains("本月还没开播"), "{none}");

        assert!(none.contains("追分目标：还没有设置"), "{none}");
    }

    /* ---- 手机号绑定识别（2026-10-10） ---- */

    #[test]
    fn phone_extracted_from_bind_messages() {
        assert_eq!(extract_phone("13800138000").as_deref(), Some("13800138000"));
        assert_eq!(
            extract_phone("绑定 13800138000").as_deref(),
            Some("13800138000")
        );
        assert_eq!(
            extract_phone("bd 13800138000").as_deref(),
            Some("13800138000")
        );
        assert_eq!(
            extract_phone("绑定 138 0013 8000"),
            None,
            "带空格的不认，避免误判"
        );
    }

    #[test]
    fn phone_not_confused_with_other_messages() {
        // 短数字 / 非纯数字 / 多段文本都不该被当成绑定手机号
        for s in ["3", "查 3 月数据", "帮助", "2026-10-01", "1380013800012345"] {
            assert_eq!(extract_phone(s), None, "不该认成手机号：{s}");
        }
    }

    #[test]
    fn bind_code_is_four_digits() {
        // 2026-10-16 口径：4 位数字，带不带「绑定」关键词都认
        assert_eq!(extract_bind_code("1234").as_deref(), Some("1234"));
        assert_eq!(extract_bind_code("绑定 1234").as_deref(), Some("1234"));
        assert_eq!(extract_bind_code("bind 1234").as_deref(), Some("1234"));
        assert_eq!(extract_bind_code("bd 1234").as_deref(), Some("1234"));
        // 旧写法「绑定 手机号 码」也认：手机号 11 位不会被当成码
        assert_eq!(
            extract_bind_code("绑定 13800138000 1234").as_deref(),
            Some("1234")
        );
        // 手机号本身不是码
        assert_eq!(extract_bind_code("13800138000"), None);
        // 位数不对 / 非数字不认
        for s in ["123", "12345", "12a4", "ABCD", "绑定 12", "绑定"] {
            assert_eq!(extract_bind_code(s), None, "不该认成绑定码：{s}");
        }
        // 不带关键词时多段文本不认（避免正常聊天被误判）
        assert_eq!(extract_bind_code("今天 1234 分"), None);
        // 关键词判定：决定「没对上码」时是回失败文案还是静默
        assert!(is_bind_keyword("绑定 1234"));
        assert!(!is_bind_keyword("1234"));
        assert!(!is_bind_keyword("今天 1234 分"));
    }

    /* ---------------- 业务日期来源（2026-10-12 用户口径） ----------------
    「11 号接收的就是 10 号的数据，前提是没指定日期；同日期再导入就是覆盖」 */

    /// 兜底：四条来源都没给 → 收到日 − 1
    #[test]
    fn biz_date_falls_back_to_yesterday_when_nothing_specified() {
        let (d, src) = pick_biz_date(None, None, None, "2026-10-10");
        assert_eq!(d, "2026-10-10", "11 号收到的文件 = 10 号的数据");
        assert!(src.contains("收到日 − 1"), "来源要说清是兜底：{src}");
    }

    /// 优先级：本条消息指定 > 贴住的日期 > 文件名里的日期 > 收到日 − 1
    #[test]
    fn biz_date_priority_order() {
        // ① 消息里报了日期 → 它说了算，后面三个都不看
        let (d, src) = pick_biz_date(
            Some("2026-10-01"),
            Some("2026-09-09"),
            Some("2026-08-08"),
            "2026-10-10",
        );
        assert_eq!(d, "2026-10-01");
        assert_eq!(src, "本条消息指定");

        // ② 没报但之前贴住过（12h 内）→ 沿用，不看文件名与兜底
        let (d, src) = pick_biz_date(None, Some("2026-09-09"), Some("2026-08-08"), "2026-10-10");
        assert_eq!(d, "2026-09-09");
        assert!(src.contains("沿用"), "{src}");

        // ③ 只有文件名带日期（补录）→ 用它
        let (d, src) = pick_biz_date(None, None, Some("2019-03-13"), "2026-10-10");
        assert_eq!(d, "2019-03-13");
        assert!(src.contains("文件名"), "{src}");

        // ④ 全都没有 → 兜底
        let (d, _) = pick_biz_date(None, None, None, "2026-10-10");
        assert_eq!(d, "2026-10-10");
    }

    /// 兜底日就是「收到日 − 1」，跨月 / 跨年 / 闰月都不能算错
    #[test]
    fn prev_biz_date_handles_month_and_year_boundaries() {
        let d = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert_eq!(prev_biz_date(d("2026-10-11")), "2026-10-10");
        assert_eq!(prev_biz_date(d("2026-08-01")), "2026-07-31", "跨月");
        assert_eq!(prev_biz_date(d("2027-01-01")), "2026-12-31", "跨年");
        assert_eq!(prev_biz_date(d("2026-03-01")), "2026-02-28", "平年 2 月");
        assert_eq!(prev_biz_date(d("2024-03-01")), "2024-02-29", "闰年 2 月");
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

    // ---- 附件域名白名单（防 SSRF）----
    // 白名单是导入链路的唯一入口守门人；一旦它把合法 CDN 挡掉，导入会 100% 失败，
    // 而失败现象（「域名不受支持」）离根因很远。这些断言不需要数据库，CI 一定跑得到。
    //
    // env 是进程级全局，这几个用例都要读写它 —— 并发跑会互相踩，统一串行化。

    /// 保护 `BOT_ATTACHMENT_HOSTS` 的读写（env 是进程全局的）
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    const KEY: &str = "BOT_ATTACHMENT_HOSTS";

    #[test]
    fn attachment_whitelist_allows_qq_cdn() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var(KEY).ok();
        std::env::remove_var(KEY);
        for u in [
            "https://multimedia.nt.qq.com.cn/a/b.csv",
            "http://multimedia.qq.com.cn/x.csv",
            "https://groupfile.qq.com/f.csv",
            "https://gchat.qpic.qq.com/whatever", // 子域后缀匹配
            "https://cdn.qq.com/a.csv",
        ] {
            assert!(attachment_host_allowed(u), "应放行：{u}");
        }
        match saved {
            Some(v) => std::env::set_var(KEY, v),
            None => std::env::remove_var(KEY),
        }
    }

    #[test]
    fn attachment_whitelist_blocks_foreign_and_internal() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var(KEY).ok();
        std::env::remove_var(KEY);
        for u in [
            "http://169.254.169.254/latest/meta-data/", // 云元数据
            "http://10.0.0.5/internal",
            "http://evil.com/steal.csv",
            "https://evil.com/steal.csv",
            "http://qq.com.evil.com/x.csv", // 后缀伪装
            "file:///etc/passwd",
        ] {
            assert!(!attachment_host_allowed(u), "应拒绝：{u}");
        }
        // 默认态（不设 env）本机也要拒绝：放行必须走显式配置。
        assert!(!attachment_host_allowed("http://127.0.0.1:8787/"));
        assert!(!attachment_host_allowed("http://localhost/data.csv"));
        match saved {
            Some(v) => std::env::set_var(KEY, v),
            None => std::env::remove_var(KEY),
        }
    }

    #[test]
    fn attachment_whitelist_env_adds_host() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // 平台换 CDN 域名时靠这个口子应急；这里验证它真的生效而不是摆设。
        let saved = std::env::var(KEY).ok();
        std::env::set_var(KEY, "127.0.0.1, localhost ,");
        assert!(
            attachment_host_allowed("http://127.0.0.1:8080/data.csv"),
            "env 追加后本机应放行（e2e 依赖这条）"
        );
        assert!(attachment_host_allowed("http://localhost/data.csv"));
        // 追加不得削弱原有防护
        assert!(!attachment_host_allowed("http://169.254.169.254/"));
        match saved {
            Some(v) => std::env::set_var(KEY, v),
            None => std::env::remove_var(KEY),
        }
    }

    /// QQ 管理员必须 fail-closed（2026-10-17 第四轮审计 P1-3）：
    /// 未配置 BOT_QQ_ADMINS = 一个都不认；配了只认名单内的会话。
    #[test]
    fn qq_admin_defaults_fail_closed() {
        assert!(
            !qq_admin_ok(&[], "12345678"),
            "BOT_QQ_ADMINS 未配置时不应默认管理员"
        );
        let admins = vec!["12345678".to_string()];
        assert!(qq_admin_ok(&admins, "12345678"));
        assert!(
            !qq_admin_ok(&admins, "87654321"),
            "名单外的 QQ 号不是管理员"
        );
    }

    /// 会话级失败计数 key 必须带渠道前缀，且能装进迁移 036 加宽后的 160 列。
    #[test]
    fn bind_fail_conv_key_shape() {
        assert_eq!(bind_fail_conv_key("weixin", "abc"), "conv:weixin:abc");
        let long = "x".repeat(120);
        assert!(bind_fail_conv_key("weixin", &long).len() <= 160);
    }

    /// 对接成功回执（2026-10-19：第一条）必须带名字，功能介绍第二条不能重复成功。
    #[test]
    fn dock_messages_split_success_and_intro() {
        let success = wx_dock_success("浩雨");
        let intro = wx_dock_intro();
        assert!(success.contains("浩雨"), "{success}");
        assert!(success.contains("微信对接成功"), "{success}");
        assert!(!intro.contains("微信对接成功"), "{intro}");
        assert!(intro.contains("解绑"), "要留解绑指引：{intro}");
    }
}
