//! 机器人模块（E 阶段）：确定性命令路由 + QQ / 微信渠道适配器
//!
//! 边界（docs/00 §9、docs/06 §4.4）：
//! - QQ 收到即可服务（2026-10-10 用户口径：不需要管理员白名单）；
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

    /// 微信渠道始终初始化：未授权会话先私聊内置口令，
    /// `shuai4444@` 会把当前会话设为管理员。
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
                "note": "未登录；先扫码连接，再私聊发送 shuai4444@ 设置管理员",
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

/// 微信推送绑定（2026-10-10 审计 P1-1 改二要素：手机号 + 「我的」页 6 位绑定码）。
/// 命中返回回复文案，没命中返回 None（继续走管理员判定）。
///
/// 消息格式（给 Lead 写文档用）：
/// - 一条消息：「绑定 13800000000 123456」（绑定/bind/bd + 手机号 + 6 位码）
/// - 分两条：先发「绑定 13800000000」（或只发手机号），30 秒内再发 6 位码即可黏合成一条
/// - 只发手机号（旧口径）→ 不再直接绑定，回复提示需要绑定码（不泄露该号是否登记过）
///
/// 安全口径：绑定码 = 「我的」页 push_set_code 生成的 30 分钟一次性码；错误文案统一，
/// 不区分「手机号没登记」「码不对」「码过期」，避免枚举已登记手机号。
/// 绑定成功回复不带姓名（审计要求：机器人会话可能被截图外传）。
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
                Some(_) => "已解绑，不会再收到推送。\n要重新绑定，在展示端「我的」页生成 6 位绑定码，\n再发「绑定 你的手机号 绑定码」即可。".to_string(),
                None => "这条会话还没有绑定过，不用解绑。".to_string(),
            },
        ));
    }

    // ① 二要素绑定：手机号 + 6 位绑定码。两种写法：
    //    一条消息「绑定 13800000000 123456」，或先发手机号再发码（PENDING_PHONE 黏合）。
    let phone = extract_phone(text);
    let code = extract_bind_code(text);
    let (phone, code) = match (&phone, &code) {
        (Some(p), Some(c)) => (p.clone(), c.clone()),
        (Some(p), None) => {
            // 只有手机号：先黏住等 30 秒内的第二条（6 位码），不查库、不回是否登记
            pending_phone_put(&conversation, p);
            return Some(Outbound::to(
                &conversation,
                "收到手机号。为保护账号安全，还需要一个 6 位绑定码：\n\
                 打开展示端 → 「我的」页 → 「推送设置」→ 点「生成绑定码」，\n\
                 30 分钟内把码发给我（例：绑定 13800000000 123456）。",
            ));
        }
        (None, Some(c)) => {
            // 只有码：看是不是上一条的另一半
            match pending_phone_take(&conversation) {
                Some(p) => (p, c.clone()),
                None => return None, // 不带手机号的孤立码：静默（避免对聊天里的 6 位串回话）
            }
        }
        (None, None) => return None, // 不是绑定消息：继续走管理员判定
    };

    let bind =
        crate::store::push_bind_by_phone_code(st, &phone, &code, &conversation, &conversation)
            .await;
    let (name, team_opt) = match bind {
        Some(pair) => pair,
        None => {
            // 统一文案：不泄露手机号是否登记过、码是否过期（P1-1 审计口径）
            tracing::warn!(
                conversation = %conversation,
                "微信绑定失败（手机号未登记 / 码不对 / 码过期），统一回复"
            );
            return Some(Outbound::to(
                &conversation,
                "绑定没有成功：手机号和绑定码要配对使用。\n\
                 打开展示端 → 「我的」页 → 「推送设置」→ 点「生成绑定码」，\n\
                 再发「绑定 你的手机号 绑定码」（码 30 分钟内有效，用一次就作废）。",
            ));
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
    tracing::info!(conversation = %conversation, "微信推送绑定成功（手机号+绑定码）");
    crate::events::emit(
        crate::events::SCOPE_BOT,
        format!("微信推送已绑定（{}）", mask_phone(&phone)),
    );
    Some(Outbound::to(
        &conversation,
        "✅ 绑定成功\n\n\
         之后每天数据入库约 30 分钟后会给你推送：\n\
         · 追分提醒 —— 追上对手 / 还差多少\n\
         · 每日报告 —— 当天的音浪排行榜\n\n\
         要开哪一项、要追谁，到「我的」页面里设置（两个开关默认关着）。\n\
         回复「解绑」可以断开。",
    ))
}

const WECHAT_ADMIN_CODE: &str = "shuai4444@";

/// 微信内置管理员口令。会话已在 `handle` 开头登记，因此这里可以直接升级当前会话。
async fn try_promote_weixin(st: &Arc<AppState>, inbound: &Inbound) -> Option<Outbound> {
    if inbound.channel != "weixin" || inbound.is_group || inbound.text.trim() != WECHAT_ADMIN_CODE {
        return None;
    }
    crate::store::session_set_admin(st, inbound.channel, &inbound.conversation_id, true).await;
    tracing::info!(conversation = %inbound.conversation_id, "微信会话已通过内置口令设为管理员");
    Some(Outbound::to(&inbound.conversation_id, "已成为管理员"))
}

/// 手机号脱敏（日志 / 事件用）：138****0000
fn mask_phone(phone: &str) -> String {
    let chars: Vec<char> = phone.chars().collect();
    if chars.len() < 7 {
        return "****".into();
    }
    format!(
        "{}****{}",
        chars[..3].iter().collect::<String>(),
        chars[chars.len() - 4..].iter().collect::<String>()
    )
}

/// 从消息里认手机号（P1-1 二要素）：`绑定 13800000000 123456` / `bd 13800000000` /
/// 单独一串数字。只认 7~15 位纯数字，带关键词时后面第二段如果是 6 位码不算（那是码）。
/// 不带关键词时必须只有一段，避免把「查 3 月数据」这类文本误判成绑定。
fn extract_phone(text: &str) -> Option<String> {
    let mut toks = text.split_whitespace();
    let first = toks.next()?;
    let has_kw = matches!(first.to_lowercase().as_str(), "绑定" | "bind" | "bd");
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

/// 从消息里认绑定码（P1-1 二要素）：`绑定 13800000000 ABC234` 里的第三段 / `bind ABC234`
/// 里的第二段 / 单独一串 6 位码。码表排除易混字符（0/O/1/I），手机号不会被当成码。
fn extract_bind_code(text: &str) -> Option<String> {
    let mut toks = text.split_whitespace();
    let first = toks.next()?;
    let has_kw = matches!(first.to_lowercase().as_str(), "绑定" | "bind" | "bd");
    // 带关键词时：跳过手机号段（7~15 位纯数字），码可能是第二或第三段；
    // 不带关键词时：只有一段且像码才认，避免把正常聊天误判成绑定码
    let cand = if has_kw {
        let mut next = toks.next()?;
        if next.chars().all(|c| c.is_ascii_digit()) && next.len() >= 7 && next.len() <= 15 {
            next = toks.next()?;
        }
        next
    } else {
        if toks.next().is_some() {
            return None;
        }
        first
    };
    // 带关键词时后面再有多段不认（「绑定 13800000000 ABC234 备注」这种不算）
    if has_kw && toks.next().is_some() {
        return None;
    }
    let code = cand.trim().to_ascii_uppercase();
    crate::push::looks_like_bind_code(&code).then_some(code)
}

/* ------------------- 二要素绑定：手机号黏合（P1-1） ------------------- */

/// 先发手机号再发码的分两条场景：手机号先黏住 30 秒，等码到了合成一条。
/// key = 会话 id，值 = (手机号, 时间)；到点没等到码就丢（无副作用，纯内存）。
static PENDING_PHONES: std::sync::OnceLock<Mutex<HashMap<String, (String, Instant)>>> =
    std::sync::OnceLock::new();
const PENDING_PHONE_TTL: Duration = Duration::from_secs(30);

fn pending_phones() -> &'static Mutex<HashMap<String, (String, Instant)>> {
    PENDING_PHONES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pending_phone_put(conversation: &str, phone: &str) {
    let mut m = pending_phones().lock().expect("pending_phone 锁 poisoned");
    m.retain(|_, (_, at)| at.elapsed() < PENDING_PHONE_TTL);
    m.insert(
        conversation.to_string(),
        (phone.to_string(), Instant::now()),
    );
}

fn pending_phone_take(conversation: &str) -> Option<String> {
    let mut m = pending_phones().lock().expect("pending_phone 锁 poisoned");
    match m.remove(conversation) {
        Some((p, at)) if at.elapsed() < PENDING_PHONE_TTL => Some(p),
        Some(_) => None,
        None => None,
    }
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
        return Some(Outbound::to(
            &inbound.conversation_id,
            "消息太频繁了，稍后再试（限流 10 条/分钟）",
        ));
    }
    // 展示端「我的」页微信绑定（2026-10-10 审计 P1-1 改二要素：手机号 + 绑定码）。
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
    // 角色判定（2026-10-10 用户口径）：
    // - Admin：QQ 全部会话 / 微信白名单 / 会话表标记过管理员 —— 拥有全部命令
    // - User：微信私聊且 push_bindings 命中的已绑定主播 —— 只允许 5 条命令
    // - 其余：完全静默（不回复任何内容），日志留痕
    let role = if inbound.channel == "qq"
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
        let reply = route_command(st, cfg, &inbound, &text, role).await;
        return Some(Outbound::to(&inbound.conversation_id, reply));
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

        /* 月度时长文件（2026-10-10 用户口径：每日不能导入时长，只有月时长）：
        只有「时长」列、没有音浪列的附件不能按日导 —— 那会「成功 0 行」，看着像成功其实什么都没进。
        文件名带月份（9月份时长.csv）就直接按那个月导；没带月份让运营改名或去管理端选月份。 */
        let month_rows = admin::parse_month_duration_text(&attachment_text);
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
            let reply = match admin::commit_month_duration(
                st,
                &period,
                "auto",
                month_rows,
                "merge",
                &format!("机器人·{filename}"),
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
        let reply = format_report(&filename, &biz_date, "auto", report);
        return Some(Outbound::to(&inbound.conversation_id, reply));
    }

    if text.is_empty() {
        return None;
    }

    let reply = route_command(st, cfg, &inbound, &text, role).await;
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
) -> String {
    // 命令注册表是唯一事实来源：解析不到 → 管理员沿用原提示，普通用户引导看「帮助」
    let Some((id, args)) = commands::parse(text) else {
        return match role {
            Role::User => "没认出这条命令。发「帮助」看你能用的命令。".to_string(),
            Role::Admin => format!("不认识这条命令。发「帮助」看用法。\n你发的是：{text}"),
        };
    };
    // 权限门控：不够权限只说这一句，绝不执行
    if !role.allows(id.required_role()) {
        return format!(
            "「{}」是管理员命令，你没有权限。\n发「帮助」看你能用的命令。",
            id.spec().name
        );
    }

    // 命令行字符串参数（日报 / 缺数据 / 导入沿用 &[&str] 接口）
    let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();

    match id {
        CmdId::Help => commands::help_for(role),
        CmdId::DailyReport => match current_binding(st, inbound).await {
            Some(b) => cmd_daily_report_user(st, &b).await,
            None => NO_BINDING_HINT.to_string(),
        },
        CmdId::MyReport => match current_binding(st, inbound).await {
            Some(b) => cmd_my_report_user(st, &b).await,
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
        CmdId::Export => cmd_export(st, args.first().map(String::as_str).unwrap_or("今日")).await,
        CmdId::DailySquadReport => cmd_daily_report(st, &args_ref).await,
        CmdId::MissingData => cmd_missing(st, &args_ref).await,
        CmdId::Import => cmd_import(st, inbound, &args_ref).await,
    }
}

/* ------------------------- 普通用户命令（已绑定主播） ------------------------- */

/// User 命令取绑定失败时的兜底（理论上 handle 已按绑定放行，这里防解绑竞态）
const NO_BINDING_HINT: &str =
    "没有找到你的推送绑定。\n打开展示端「我的」页 →「推送设置」重新绑定，或发「帮助」。";

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
async fn cmd_daily_report_user(st: &Arc<AppState>, b: &crate::store::PushBinding) -> String {
    let Some(date) = latest_biz_date(st).await else {
        return "还没有可用的业务数据，等运营导入后再来看。".into();
    };
    let Some(team) = binding_team(st, b).await else {
        return "还没有查到你的团别归属，稍后再试；一直这样请联系管理员。".into();
    };
    let items = crate::push::daily_rank(st, &team, &date).await;
    if items.is_empty() {
        return format!("{date} 还没有音浪数据，等运营导入后再来看。");
    }
    crate::push::daily_report_text(&team, &date, &items, b.person_id)
}

/// 普通用户「个人报告」：今日名次/音浪 + 本月累计/名次 + 追分目标进度
async fn cmd_my_report_user(st: &Arc<AppState>, b: &crate::store::PushBinding) -> String {
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
    render_my_report(&b.name, &date, day_pos, month_pos, my_month, &target_lines)
}

/// 「我的报告」排版（纯函数，好测）。
/// gap：≤ 0 已达成；i64::MAX = 追名次但本月还没进榜（没有可比的差距）
fn render_my_report(
    name: &str,
    date: &str,
    day: Option<(usize, i64)>,
    month_pos: Option<usize>,
    my_month: i64,
    targets: &[(String, String, String, i64)],
) -> String {
    let mut out = format!("📈 我的报告（{name}）");
    match day {
        Some((pos, wave)) => out.push_str(&format!(
            "\n今日（{date}）：{} · 团内第 {pos} 名",
            crate::push::fmt_wan(wave)
        )),
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

/// 附件下载大小上限（P2-16，2026-10-10 审计）：8MB。CSV/文本导入远用不到这么大，
/// 超限一律拒绝——rk3318 部署 mem_limit 160m，无上限下载必然 OOM。
const ATTACHMENT_MAX_BYTES: usize = 8 * 1024 * 1024;
/// 附件下载主机白名单（P2-16）：只允许 QQ 开放平台官方 CDN 域名。
/// 附件 URL 由平台下发，但白名单兜底 SSRF——就算平台回显用户可控 URL 也出不了这个圈。
/// 需要接微信文件消息时在这里加域名。
const ATTACHMENT_HOSTS: &[&str] = &[
    "multimedia.nt.qq.com.cn",
    "multimedia.qq.com.cn",
    "groupfile.qq.com",
    "qq.com",
];

/// 下载 URL 主机校验：解析 host，必须与白名单域名相等或以其为后缀（`.qq.com`）。
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
}

/// 下载附件文本（QQ CDN 直链 / 微信文件）。
/// P2-16：主机白名单 + 8MB 大小上限，超限拒绝并告警（防 OOM / SSRF）。
async fn download_text(url: &str) -> anyhow::Result<String> {
    if !attachment_host_allowed(url) {
        tracing::warn!(
            url_head = %url.chars().take(80).collect::<String>(),
            "附件下载被拒：主机不在白名单"
        );
        anyhow::bail!("附件链接的域名不受支持（只接受 QQ 开放平台官方 CDN）");
    }
    let resp = reqwest::Client::builder()
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
    let bytes = resp.bytes().await?.to_vec();
    let filename = url
        .split(['/', '?', '#'])
        .rfind(|s| !s.is_empty())
        .unwrap_or_default();
    let decoded = percent_decode_utf8(filename);
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
    }

    #[test]
    fn my_report_without_data_is_friendly() {
        let none = render_my_report("小美", "2026-10-09", None, None, 0, &[]);
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
    fn bind_code_still_works_as_fallback() {
        assert_eq!(extract_bind_code("ABC234").as_deref(), Some("ABC234"));
        assert_eq!(extract_bind_code("绑定 ABC234").as_deref(), Some("ABC234"));
        // 手机号格式不会被当成绑定码（码表里没有纯数字组合里的小写，且长度固定 6）
        assert_eq!(extract_bind_code("13800138000"), None);
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
