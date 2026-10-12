//! 展示端「我的」页微信推送引擎（2026-10-09，2026-10-10 改延迟触发）。
//!
//! 触发点：**数据入库后延迟 N 分钟**（默认 30，见 [`delay_minutes`]），不是入库即推——
//! 运营一次导入往往连着发好几个文件（男女团分开、一人多号分开），入库就推会把
//! 半成品榜单推给用户。延迟队列在 `push_jobs` 表，同一 (团, 业务日) 只排一队。
//!
//! 推两类，各自独立开关（都在「我的」页设置）：
//! - **每日报告**：当天音浪榜 TOP10 + 我的名次（同一业务日只推一次）
//! - **追分提醒**：把该账号所有追分目标合成一条消息（追上 / 被反超 / 还差多少），
//!   一天只推一次（按业务日去重），但「追上」「被反超」是事件，发生即推。
//!
//! 目标三种（2026-10-10 用户口径：一律比本月累计）：
//! - `person` 追某个人：本月累计音浪超过 TA = 追上
//! - `value`  追数值：本月累计达到某个音浪值
//! - `rank`   追名次：本月累计在团内排到第 N 名以内
//!
//! 发送走微信 iLink 渠道的 [`WxTransport::send_proactive`]（需要最后一次私聊入站留下的
//! context_token）。渠道没配 / 会话失效都只记日志，不影响导入本身。
//! **没开任何开关的人不推**（用户口径 2026-10-10）。
//!
//! 每日报告里的「我的抖音号分号明细」（2026-10-12 新增）：一人多号的主播看总榜
//! 只知道「我今天多少」，看不出是哪个号挣的。报告在「我：第 N 名」后补一段
//! 每个号的当日 / 本月 / 本年音浪（见 [`person_account_waves`] 与
//! [`account_waves_section`]）。只有一个号时不补——那一行就是自己，重复。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use sqlx::Row;

use crate::pgrs;
use crate::state::AppState;

/// 绑定码：6 位，码表剔除易混字符（0/O/1/I）
pub const BIND_CODE_LEN: usize = 6;
/// 每人最多几个追分目标（用户口径 2026-10-09：5 个）
pub const MAX_TARGETS: i64 = 5;
/// 每日报告图片分页：每页行数（40 行 ≈ 微信一屏半，再长看不清）
const PAGE_ROWS: usize = 40;
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// 是不是一个合法绑定码（防止把正常聊天误判成绑定）
pub fn looks_like_bind_code(s: &str) -> bool {
    s.len() == BIND_CODE_LEN && s.bytes().all(|b| CODE_ALPHABET.contains(&b))
}

/// 随机绑定码（2026-10-11 审计整改：改走 CSPRNG）。
/// 之前用 RandomState（SipHash）拼字节 —— 它不是密码学随机源，种子弱、输出可预测，
/// 且按字节取模有偏。现在走 crate::random（getrandom → OS CSPRNG，拒绝采样无偏）。
/// 码表必须保持 CODE_ALPHABET：bot 侧 looks_like_bind_code 用同一码表识别绑定码，
/// 换成纯数字会让生成的码过不了校验、还会和手机号提取撞车。
pub fn new_bind_code() -> String {
    crate::random::random_code(BIND_CODE_LEN, CODE_ALPHABET)
}

/// 音浪数值按「万」表达（用户口径 2026-10-09：推送一律用万）
pub fn fmt_wan(v: i64) -> String {
    let f = v as f64;
    if v.abs() >= 10_000 {
        let w = f / 10_000.0;
        let s = if w.abs() >= 100.0 {
            format!("{}", w.round() as i64)
        } else {
            let r = (w * 10.0).round() / 10.0;
            if (r - r.round()).abs() < f64::EPSILON {
                format!("{}", r.round() as i64)
            } else {
                format!("{r:.1}")
            }
        };
        format!("{s} 万")
    } else {
        format!("{v}")
    }
}

/* ------------------------------ 触发 ------------------------------ */

/// 数据入库后排一次延迟推送（不阻塞导入响应）。
/// 同一 (团, 业务日) 重复导入只刷新队列，不叠加任务。
pub fn schedule_after_import(st: &Arc<AppState>, team: &str, date: &str) {
    let minutes = delay_minutes();
    let run_at = (chrono::Local::now() + chrono::Duration::minutes(minutes))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let st2 = Arc::clone(st);
    let team2 = team.to_string();
    let date2 = date.to_string();
    let handle = match tokio::runtime::Handle::try_current() {
        Ok(h) => h,
        Err(_) => return, // 不在运行时里（单测）直接跳过
    };
    handle.spawn(async move {
        let id = crate::store::push_job_enqueue(&st2, &team2, &date2, &run_at).await;
        if id == 0 {
            tracing::warn!(team = %team2, date = %date2, "推送任务排队失败（写库不可用？）");
            return;
        }
        tracing::info!(job = id, team = %team2, date = %date2, run_at = %run_at, "推送已排队");
    });
}

/// 延迟分钟数（PUSH_DELAY_MINUTES，默认 30）。0 = 入库即推。
pub fn delay_minutes() -> i64 {
    std::env::var("PUSH_DELAY_MINUTES")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| (0..=720).contains(v))
        .unwrap_or(30)
}

/* --------------------- 1.0.1：分批推送（150 人规模） --------------------- */

/// 每批推送人数（WX_PUSH_BATCH_SIZE，默认 20）。iLink 是私聊被动形态，
/// 连环群发容易触发频控；分批 + 批间隔把 150 人的推送摊平到一分多钟。
pub fn batch_size() -> usize {
    std::env::var("WX_PUSH_BATCH_SIZE")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| (1..=200).contains(v))
        .unwrap_or(20)
}

/// 批间隔秒数（WX_PUSH_BATCH_INTERVAL_SECS，默认 3）。
pub fn batch_interval_secs() -> u64 {
    std::env::var("WX_PUSH_BATCH_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v <= 300)
        .unwrap_or(3)
}

/* --------------------- P1-20 / P2-21：调度器生命周期与健康状态 --------------------- */

/// 调度循环的退出信号：main.rs 在 graceful shutdown 时 cancel()，循环尽快退出。
/// 不额外引 token crate，用 watch 通道实现同等语义（可 clone、可等待）。
#[derive(Clone)]
pub struct ShutdownToken {
    inner: std::sync::Arc<tokio::sync::watch::Sender<bool>>,
}

impl ShutdownToken {
    fn new() -> Self {
        let (tx, _rx) = tokio::sync::watch::channel(false);
        Self {
            inner: std::sync::Arc::new(tx),
        }
    }
    /// 发出退出信号
    pub fn cancel(&self) {
        let _ = self.inner.send(true);
    }
    /// 等待取消信号（已取消则立即返回）
    pub async fn cancelled(&self) {
        let mut rx = self.inner.subscribe();
        if *rx.borrow_and_update() {
            return;
        }
        let _ = rx.changed().await;
    }
}

/// P1-20：全局调度循环 shutdown token（调度循环与 main.rs 的退出逻辑共用一份）
static SCHEDULER_SHUTDOWN: std::sync::OnceLock<ShutdownToken> = std::sync::OnceLock::new();

pub fn shutdown_token() -> ShutdownToken {
    SCHEDULER_SHUTDOWN.get_or_init(ShutdownToken::new).clone()
}

/// P2-21：调度器最近一轮扫描完成时间（RFC3339）。一轮都没跑过（刚启动）
/// 返回 None，/api/health 的 scheduler.lastRun 据此标 null。
static SCHEDULER_LAST_RUN: std::sync::OnceLock<std::sync::Mutex<Option<String>>> =
    std::sync::OnceLock::new();

pub fn scheduler_last_run() -> Option<String> {
    SCHEDULER_LAST_RUN
        .get()
        .and_then(|m| m.lock().ok())
        .and_then(|g| g.clone())
}

fn note_scheduler_run() {
    let now = chrono::Local::now().to_rfc3339();
    if let Some(m) = SCHEDULER_LAST_RUN.get() {
        if let Ok(mut g) = m.lock() {
            *g = Some(now);
        }
    }
}

/// 后台调度器：每 60 秒扫一次到点的推送任务。
/// 启动一次（main.rs spawn）；写库不可用时静默空转，不影响服务。
/// P1-20：收到 shutdown token 的取消信号后尽快退出（在途的单个推送推完再停，
/// 未开始的任务留给下次启动处理，不会丢：push_jobs 表里状态未变）。
pub async fn scheduler_loop(st: Arc<AppState>) {
    let token = shutdown_token();
    // 占位，health 才有地方记录最近一轮时间
    let _ = SCHEDULER_LAST_RUN.set(std::sync::Mutex::new(None));
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    loop {
        tokio::select! {
            _ = token.cancelled() => {
                tracing::info!("调度循环收到退出信号，停止");
                return;
            }
            _ = tick.tick() => {}
        }
        note_scheduler_run();
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        while let Some((id, team, date)) = crate::store::push_job_take(&st, &now).await {
            tokio::select! {
                _ = token.cancelled() => {
                    tracing::info!(job = id, team = %team, "调度循环退出，任务留给下次启动处理");
                    return;
                }
                res = run(&st, &team, &date) => {
                    match res {
                        Ok(()) => {
                            crate::store::push_job_finish(&st, id, true, "").await;
                            tracing::info!(job = id, team = %team, date = %date, "延迟推送完成");
                        }
                        Err(e) => {
                            tracing::warn!(job = id, error = %e, "延迟推送失败");
                            crate::store::push_job_finish(&st, id, false, &e.to_string()).await;
                        }
                    }
                }
            }
        }
    }
}

/// 给一条绑定发消息：优先走用户自己扫码得到的微信账号（多账号），
/// 没有扫码凭证时回落到管理员机器人（旧手机号 / 绑定码路径）。
///
/// 扫码账号的主动推送必须带用户首次入站留下的 context_token（官方协议：
/// 私聊是会话上下文形态）。没等到用户消息时明确报错，推送机会留到下次。
async fn send_binding(
    st: &Arc<AppState>,
    b: &crate::store::PushBinding,
    wx_global: Option<&Arc<crate::bot::weixin::WxTransport>>,
    text: &str,
) -> anyhow::Result<()> {
    /* 扫码账号：发文本；没有扫码凭证的旧绑定：回落管理员机器人文本通道 */
    if let Some(a) = crate::store::wx_push_account_get(st, &b.name).await {
        anyhow::ensure!(!a.bot_token.is_empty(), "扫码账号凭证为空，请重新扫码");
        let (to_user, context_token) = push_ctx(st, b, &a).await?;
        return crate::bot::weixin::user_send_text(
            &a.base_url,
            &a.bot_token,
            &to_user,
            &context_token,
            text,
        )
        .await;
    }
    wx_proactive(wx_global, &b.conversation_id, text).await
}

/// 发图片（1..N 张）+ 首张前带说明文字。逐张独立 sendmessage（openclaw 口径）。
/// 走扫码账号通道（getuploadurl → AES-ECB → novac2c CDN → image_item）。
async fn send_binding_images(
    st: &Arc<AppState>,
    b: &crate::store::PushBinding,
    caption: &str,
    pngs: &[Vec<u8>],
) -> anyhow::Result<()> {
    anyhow::ensure!(!pngs.is_empty(), "没有可发送的图片");
    let a = crate::store::wx_push_account_get(st, &b.name)
        .await
        .ok_or_else(|| anyhow::anyhow!("图片推送需要扫码账号（请在「我的」页重新扫码）"))?;
    anyhow::ensure!(!a.bot_token.is_empty(), "扫码账号凭证为空，请重新扫码");
    let (to_user, context_token) = push_ctx(st, b, &a).await?;
    for (i, png) in pngs.iter().enumerate() {
        // 说明文字只挂在第一张前；第二张起带页码提示
        let cap = if i == 0 {
            caption.to_string()
        } else {
            format!("📄 榜单第 {} 页（接上条）", i + 1)
        };
        crate::store::wxmedia::user_send_image(
            &a.base_url,
            &a.bot_token,
            &to_user,
            &context_token,
            &cap,
            png,
        )
        .await?;
    }
    Ok(())
}

/// 两条路径共用的会话上下文解析：conv 取账号/绑定里非空者，
/// to_user 取会话表/账号 ilink_user_id 里非空者；缺 context_token 报可读错误。
async fn push_ctx(
    st: &Arc<AppState>,
    b: &crate::store::PushBinding,
    a: &crate::store::WxPushAccount,
) -> anyhow::Result<(String, String)> {
    let conv = if a.conversation_id.is_empty() {
        b.conversation_id.clone()
    } else {
        a.conversation_id.clone()
    };
    let (to_user, context_token) = crate::store::wx_session_get(st, &conv)
        .await
        .unwrap_or_default();
    let to_user = if to_user.is_empty() {
        a.ilink_user_id.clone()
    } else {
        to_user
    };
    anyhow::ensure!(
        !context_token.is_empty(),
        "等待你在微信里给机器人发送一条消息以激活推送"
    );
    Ok((to_user, context_token))
}

/// 管理员机器人通道发文本（旧绑定兜底）
async fn wx_proactive(
    wx_global: Option<&Arc<crate::bot::weixin::WxTransport>>,
    conv: &str,
    text: &str,
) -> anyhow::Result<()> {
    let Some(wx) = wx_global else {
        anyhow::bail!("没有可用的微信推送通道（请先在「我的」页扫码绑定）");
    };
    wx.send_proactive(conv, text).await
}

/* ------------------------------ 主流程 ------------------------------ */

/// 按推送时刻的问候语（2026-10-12 用户口径：晚上好，浩月）。
/// 时段：5-11 早上好 / 11-13 中午好 / 13-18 下午好 / 18-23 晚上好 / 23-5 夜深了。
/// 时区用服务器本地时间（147 = EDT，部署机随 TZ env；与导入运维同时区）。
fn greeting(name: &str) -> String {
    use chrono::Timelike;
    let h = chrono::Local::now().hour();
    let word = match h {
        5..=10 => "早上好",
        11..=12 => "中午好",
        13..=17 => "下午好",
        18..=22 => "晚上好",
        _ => "夜深了",
    };
    format!("{word}，{name}")
}

/// 图片推送的 caption：问候语 + 我的名次行（图里已有完整榜单，文字只给关键信息）。
fn greeting_with_rank(
    name: &str,
    items: &[(u64, String, i64)],
    me: u64,
    my_month: i64,
    star: Option<&crate::report::StarBanner>,
) -> String {
    let mut out = greeting(name);
    // 🚩 流动小红旗：称号（后台可配）+ 名字
    if let Some(st) = star {
        let mut line = format!("🚩 {} {}", st.title, st.name);
        if st.wave > 0 {
            line.push_str(&format!(" {}", fmt_wan(st.wave)));
        }
        out.push_str(&format!("\n{line}"));
    }
    // 我在月榜的位置 + 本月累计
    let rank_in_month = items
        .iter()
        .position(|(pid, _, _)| *pid == me)
        .map(|i| i + 1);
    match rank_in_month {
        Some(r) => out.push_str(&format!("\n本月第 {} 名 · 累计 {}", r, fmt_wan(my_month))),
        None => out.push_str("\n本月还没开播"),
    }
    out.push_str("\n（本月累计榜见图，含未播天数）");
    out
}

/// 日报 SVG → PNG 字节（resvg 纯 Rust 渲染，服务器零系统依赖）。
/// 宽 720 逻辑像素 ×2 输出（1440），微信长图清晰度够用。
fn svg_to_png(svg: &str) -> anyhow::Result<Vec<u8>> {
    use anyhow::Context as _;
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_str(svg, &opt)?;
    let size = tree.size();
    let scale = 2.0f32;
    let (w, h) = ((size.width() * scale).round() as u32, (size.height() * scale).round() as u32);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).context("PNG 画布创建失败")?;
    pixmap.fill(resvg::tiny_skia::Color::from_rgba8(255, 255, 255, 255));
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().context("PNG 编码失败")
}

pub async fn run(st: &Arc<AppState>, team: &str, date: &str) -> anyhow::Result<()> {
    let bindings = crate::store::push_bindings_for_team(st, team).await;
    if bindings.is_empty() {
        return Ok(());
    }
    // 用户自助扫码的账号优先；没有扫码凭证时回落到管理员机器人（兼容旧绑定）
    let wx_global = st
        .bots
        .get()
        .and_then(|m| m.wechat.as_ref())
        .map(std::sync::Arc::clone);

    // 本月累计（追分口径：本月音浪，不是历史累计）
    let month = date.get(..7).unwrap_or(date).to_string();
    let month_map = month_wave_map(st, team, &month).await;
    // 追名次目标要全团本月排名，只在这类目标存在时才算
    let month_rank = if bindings.iter().any(|b| b.notify_target && b.person_id > 0) {
        Some(month_rank_list(st, team, &month).await)
    } else {
        None
    };

    // 每日报告全团共用一份排名，算一次
    let rank_items = if bindings.iter().any(|b| b.daily_report) {
        daily_rank(st, team, date).await
    } else {
        Vec::new()
    };

    for (idx, b) in bindings.iter().enumerate() {
        let mut sent_any = false;

        // ① 每日报告（同一业务日只推一次：补数据重排队列时不再重复推）
        if b.daily_report && !rank_items.is_empty() {
            let last = crate::store::push_last_report_date(st, &b.name).await;
            if last == date {
                tracing::debug!(account = %b.name, date = %date, "每日报告同日已推过，跳过");
            } else {
                // 图片版（2026-10-12）：日报按「每页 40 行」分页渲染（榜单太长微信看不清），
                // 每人一份带艺名水印的 PNG（防转发扩散），逐张发送；
                // caption = 问候语 + 我的名次行。渲染/取数失败退回纯文本，不中断整轮。
                let accounts = person_account_waves(st, b.person_id, date).await;
                let text = daily_report_text_with_accounts(
                    team,
                    date,
                    &rank_items,
                    b.person_id,
                    &accounts,
                );
                /* 月榜口径（2026-10-12 用户要求）：图里发本月累计音浪 + 当月未播天数，
                   日榜不发；头部 🚩 流动小红旗条，称号/人名后台可配：
                   - config star_title：称号（如「初级争霸赛冠军」），缺省「每日之星」
                   - config star_override：手动指定「称号|人名|音浪」（ Trophy 人工颁奖），
                   没配 override 就自动取当日音浪第一。
                   - config star_icon：小红旗条旁的 Material Symbols 图标名，空 = 不带图标。 */
                let star = {
                    let ov = crate::store::config_get(st, "star_override").await;
                    let auto = rank_items
                        .first()
                        .map(|(_, name, w)| (name.clone(), *w));
                    let (title, def) = parse_star_override(ov.as_deref(), auto);
                    let icon = crate::store::config_get(st, "star_icon")
                        .await
                        .unwrap_or_default();
                    let icon = icon.trim().to_string();
                    def.map(|(name, wave)| crate::report::StarBanner {
                        title,
                        name,
                        wave,
                        icon,
                    })
                };
                let svgs = match crate::report::build_month(st, date, team, star.clone()).await {
                    Ok(rep) => crate::report::render_month_svg_pages(&rep, PAGE_ROWS, &b.name),
                    Err(e) => {
                        tracing::warn!(error = %e, "月榜 SVG 渲染失败，退回文本推送");
                        Vec::new()
                    }
                };
                if svgs.is_empty() {
                    tracing::info!(account = %b.name, text = %text.replace('\n', " | "), "推送·每日报告(文本)");
                    match send_binding(st, b, wx_global.as_ref(), &text).await {
                        Ok(()) => {
                            sent_any = true;
                            crate::store::push_set_last_report_date(st, &b.name, date).await;
                            tracing::info!(account = %b.name, "每日报告已推送");
                        }
                        Err(e) => {
                            tracing::warn!(account = %b.name, error = %e, "每日报告推送失败");
                            crate::store::push_touch(st, &b.name, &e.to_string()).await;
                        }
                    }
                } else {
                    let caption = greeting_with_rank(
                        &b.name,
                        &rank_items,
                        b.person_id,
                        month_map.get(&b.person_id).copied().unwrap_or(0),
                        star.as_ref(),
                    );
                    let pngs: Vec<Vec<u8>> = svgs
                        .iter()
                        .filter_map(|svg| match svg_to_png(svg) {
                            Ok(p) => Some(p),
                            Err(e) => {
                                tracing::warn!(account = %b.name, error = %e, "SVG 转 PNG 失败");
                                None
                            }
                        })
                        .collect();
                    if pngs.is_empty() {
                        tracing::warn!(account = %b.name, "全部页面转 PNG 失败，退回文本推送");
                        match send_binding(st, b, wx_global.as_ref(), &text).await {
                            Ok(()) => {
                                sent_any = true;
                                crate::store::push_set_last_report_date(st, &b.name, date).await;
                            }
                            Err(e2) => {
                                tracing::warn!(account = %b.name, error = %e2, "每日报告推送失败");
                                crate::store::push_touch(st, &b.name, &e2.to_string()).await;
                            }
                        }
                    } else {
                        tracing::info!(account = %b.name, pages = pngs.len(), bytes = pngs.iter().map(|p| p.len()).sum::<usize>(), "推送·每日报告(图片)");
                        match send_binding_images(st, b, &caption, &pngs).await {
                            Ok(()) => {
                                sent_any = true;
                                crate::store::push_set_last_report_date(st, &b.name, date).await;
                                tracing::info!(account = %b.name, pages = pngs.len(), "每日报告(图片)已推送");
                            }
                            Err(e) => {
                                tracing::warn!(account = %b.name, error = %e, "每日报告图片推送失败，退回文本");
                                match send_binding(st, b, wx_global.as_ref(), &text).await {
                                    Ok(()) => {
                                        sent_any = true;
                                        crate::store::push_set_last_report_date(st, &b.name, date).await;
                                    }
                                    Err(e2) => {
                                        tracing::warn!(account = %b.name, error = %e2, "每日报告文本兜底也失败");
                                        crate::store::push_touch(st, &b.name, &e2.to_string()).await;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // ② 追分提醒（多个目标合成一条）
        if b.notify_target && b.person_id > 0 {
            let targets = crate::store::targets_for_push(st, &b.name).await;
            if !targets.is_empty() {
                let my = month_map.get(&b.person_id).copied().unwrap_or(0);
                let mut lines: Vec<String> = Vec::new();
                let mut event = false;
                let mut due = false;
                for t in &targets {
                    let rival = month_map.get(&t.target_person_id).copied().unwrap_or(0);
                    let (gap, label, detail, gap_text) =
                        eval_target(t, my, rival, month_rank.as_deref(), b.person_id);
                    if gap <= 0 {
                        if !t.hit {
                            lines.push(format!("🚀 已追上「{label}」！\n   {detail}",));
                            event = true;
                        }
                        crate::store::target_set_state(st, t.id, true, 0, date).await;
                    } else {
                        if t.hit {
                            lines.push(format!(
                                "⚠️ 「{label}」又差回来了\n   {detail}，还差 {gap_text}",
                            ));
                            event = true;
                        } else if t.last_notice_date != date {
                            lines.push(format!("· 距「{label}」还差 {gap_text}（{detail}）"));
                        }
                        if t.last_notice_date != date {
                            due = true;
                        }
                        // 追名次没进榜时 gap 是哨兵值，落库记 0（前端只显示「未达成」）
                        let store_gap = if gap == i64::MAX { 0 } else { gap };
                        crate::store::target_set_state(st, t.id, false, store_gap, date).await;
                    }
                }
                if !lines.is_empty() && (event || due) {
                    let text = format!(
                        "📈 追分进度（{} 数据）\n{}\n\n你本月累计 {}",
                        date.get(5..).unwrap_or(date),
                        lines.join("\n"),
                        fmt_wan(my)
                    );
                    tracing::info!(account = %b.name, text = %text.replace('\n', " | "), "推送·追分提醒");
                    match send_binding(st, b, wx_global.as_ref(), &text).await {
                        Ok(()) => {
                            sent_any = true;
                            tracing::info!(account = %b.name, targets = targets.len(), "追分提醒已推送");
                        }
                        Err(e) => {
                            tracing::warn!(account = %b.name, error = %e, "追分提醒推送失败");
                            crate::store::push_touch(st, &b.name, &e.to_string()).await;
                        }
                    }
                }
            }
        }

        if sent_any {
            crate::store::push_touch(st, &b.name, "").await;
        }
        /* 1.0.1 分批节拍：150 人规模下连环群发容易触发 iLink 频控。
        每发完 batch_size() 个人停 batch_interval_secs() 秒（env 可调）；
        批内单人失败照常记日志继续，不影响整批节奏。最后一人不停。 */
        let batch = batch_size().max(1);
        if (idx + 1) % batch == 0 && idx + 1 < bindings.len() {
            let interval = batch_interval_secs();
            if interval > 0 {
                tracing::debug!(
                    sent = idx + 1,
                    total = bindings.len(),
                    interval,
                    "推送分批：本批发完，暂停"
                );
                tokio::time::sleep(Duration::from_secs(interval)).await;
            }
        }
    }
    Ok(())
}

/* ------------------------------ 数据 ------------------------------ */

/// 本月累计音浪：person_id → wave（只算在册且对外可见的人，与榜单口径一致）
/// P1-7：只聚合可信行（wave_reliable=1）。
pub(crate) async fn month_wave_map(st: &AppState, team: &str, month: &str) -> HashMap<u64, i64> {
    let sql = "SELECT p.id AS person_id, (COALESCE(SUM(d.wave), 0))::BIGINT AS wave \
          FROM person p \
          LEFT JOIN daily_metric d ON d.person_id = p.id \
            AND TO_CHAR(d.biz_date, 'YYYY-MM') = ? \
            AND d.wave_reliable = 1 \
          WHERE p.deleted_at IS NULL AND p.gender = ? \
          GROUP BY p.id";
    let mut map = HashMap::new();
    match pgrs::query_with_binds(&st.pools, &st.active_id(), sql, |q| {
        q.bind(month.to_string()).bind(team.to_string())
    })
    .await
    {
        Ok(rows) => {
            for r in &rows {
                let pid = r.try_get::<i64, _>("person_id").unwrap_or(0);
                let w = r.try_get::<i64, _>("wave").unwrap_or(0);
                if pid > 0 {
                    map.insert(pid as u64, w);
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "读本月累计失败，追分按 0 处理"),
    }
    map
}

/// 本月团内排名（(person_id, 名字, 音浪)，按音浪降序）。追名次目标用。
/// 口径与月榜一致：在册 + 未离团 + 不隐藏；本月完全没有数据的不进榜。
/// P1-7：只聚合可信行（wave_reliable=1）。P1-6：日期参数化（不再内联）。
pub(crate) async fn month_rank_list(
    st: &AppState,
    team: &str,
    month: &str,
) -> Vec<(u64, String, i64)> {
    let sql = "SELECT p.id AS person_id, p.name, (COALESCE(SUM(d.wave), 0))::BIGINT AS wave \
         FROM person p \
         JOIN daily_metric d ON d.person_id = p.id \
           AND TO_CHAR(d.biz_date, 'YYYY-MM') = ? \
           AND d.wave_reliable = 1 \
         WHERE p.deleted_at IS NULL AND p.gender = ? AND p.status <> 'left' \
           AND p.hide_in_daily_report = 0 \
         GROUP BY p.id, p.name \
         HAVING wave > 0 \
         ORDER BY wave DESC, p.name ASC";
    match pgrs::query_with_binds(&st.pools, &st.active_id(), sql, |q| {
        q.bind(month.to_string()).bind(team.to_string())
    })
    .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                (
                    r.try_get::<i64, _>("person_id")
                        .map(|v| v.max(0) as u64)
                        .unwrap_or(0),
                    r.try_get::<String, _>("name").unwrap_or_default(),
                    r.try_get::<i64, _>("wave")
                        .or_else(|_| r.try_get::<i64, _>("wave").map(|v| v as i64))
                        .unwrap_or(0),
                )
            })
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "读本月排行失败，追名次目标按未达成处理");
            Vec::new()
        }
    }
}

/// 当日榜（(person_id, 名字, 音浪)），口径与展示端每日榜一致：
/// 在册 + 未离团 + 不隐藏（hide_in_daily_report=0），当日音浪低于
/// [`crate::dialect::NOT_LIVE_WAVE_THRESHOLD`]（2）视为未开播（不计入榜）。
/// P1-7：只聚合可信行（wave_reliable=1）。P1-6：日期参数化（不再内联 SQL）。
pub(crate) async fn daily_rank(st: &AppState, team: &str, date: &str) -> Vec<(u64, String, i64)> {
    let sql = "SELECT p.id AS person_id, p.name, (COALESCE(SUM(d.wave), 0))::BIGINT AS wave \
         FROM person p \
         JOIN daily_metric d ON d.person_id = p.id \
           AND TO_CHAR(d.biz_date, 'YYYY-MM-DD') = ? \
           AND d.wave_reliable = 1 \
         WHERE p.deleted_at IS NULL AND p.gender = ? AND p.status <> 'left' \
           AND p.hide_in_daily_report = 0 \
         GROUP BY p.id, p.name \
         HAVING wave >= ? \
         ORDER BY wave DESC, p.name ASC";
    match pgrs::query_with_binds(&st.pools, &st.active_id(), sql, |q| {
        q.bind(date.to_string())
            .bind(team.to_string())
            .bind(crate::dialect::NOT_LIVE_WAVE_THRESHOLD)
    })
    .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                (
                    r.try_get::<i64, _>("person_id")
                        .map(|v| v.max(0) as u64)
                        .unwrap_or(0),
                    r.try_get::<String, _>("name").unwrap_or_default(),
                    r.try_get::<i64, _>("wave")
                        .or_else(|_| r.try_get::<i64, _>("wave").map(|v| v as i64))
                        .unwrap_or(0),
                )
            })
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "读当日榜失败，跳过日报推送");
            Vec::new()
        }
    }
}

/* --------------------- 2026-10-12：我的抖音号分号明细 --------------------- */

/// 一个抖音号在「当日 / 本月 / 本年」三个口径下的音浪。
///
/// 为什么要按号拆：一人多号的主播在总榜里只看到一行合计，不知道是哪个号在挣。
/// 口径与 [`crate::dialect::WAVES_A`] 同源（主号 daily_metric、副号 wave_snapshot），
/// 只是分组粒度从人下沉到号。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AccountWave {
    /// 抖音号（展示用，可能为空）
    pub(crate) douyin_no: String,
    /// 抖音昵称（可能为空）
    pub(crate) anchor_name: String,
    /// 采集 ID。`douyin_no` / `anchor_name` 都空时的最后兜底显示名（有些号导入时只带 id）
    pub(crate) anchor_id: String,
    /// 是否主号（主号在文案里标「（主号）」，且排序时优先）
    pub(crate) primary: bool,
    pub(crate) day: i64,
    pub(crate) month: i64,
    pub(crate) year: i64,
}

impl AccountWave {
    /// 推送里的账号标识：**抖音号优先**，没登记就退回采集 ID，并显式加 `ID` 前缀
    /// 说明「这不是抖音号」。
    ///
    /// 为什么不再用昵称兜底（2026-10-12 用户口径「推送多个号带着抖音号」）：
    /// `anchor_name` 是抖音昵称，**常常等于主播艺名**（生产里 `狼佑`/`狼沐`/`狼征`
    /// 这些就是），一人多号时几行全同名 —— 收件人根本分不清哪行是哪个号，
    /// 等于没带抖音号。采集 ID 至少一号一个、且在管理后台台账里能对上。
    /// 库里 126 个号有 24 个没登记抖音号，这条路径是真实存在的。
    pub(crate) fn display_name(&self) -> String {
        let no = self.douyin_no.trim();
        if !no.is_empty() {
            return no.to_string();
        }
        let id = self.anchor_id.trim();
        if !id.is_empty() {
            return format!("ID {id}");
        }
        // 理论上不可达（account.anchor_id 是唯一键、非空）；连 ID 都没有时只剩昵称
        self.anchor_name.trim().to_string()
    }
}

/// 分号明细的 SQL（纯函数，好测；日期与人员 id 都是白名单校验后拼进去的）。
///
/// 一条 SQL 出三个口径：子查询把主号（daily_metric，只算 wave_reliable=1）与
/// 副号（wave_snapshot，JOIN account.is_primary=0）UNION ALL 成「号 → 日 → 音浪」，
/// 外层按 account 分组用 `SUM(CASE WHEN ...)` 同时算出日 / 月 / 年。
///
/// **一个号不能两边都计**：主号分支要求 `account.is_primary = 1`，副号分支要求
/// `is_primary = 0`，与 `WAVES_A` 的 is_primary 分流完全一致。
/// 未收敛（该人没有 is_primary=1 的号）时主号分支取不到行，主号显示 0——
/// `admin.rs::ensure_primary` 每次导入都会收敛，正常不会命中，宁可少算不重复算。
///
/// `date` 必须形如 `YYYY-MM-DD`（业务日，来自库里的 biz_date）；不合法返回 None。
/// 日期不 bind 而拼进 SQL：该库对 prepared statement 报 1835（见 dialect.rs 注释），
/// 所以先过白名单再拼。person_id 是 u64，天然安全。
pub(crate) fn account_waves_sql(person_id: u64, date: &str) -> Option<String> {
    if person_id == 0 || !is_biz_date(date) {
        return None;
    }
    let day = date;
    let month = date.get(..7)?;
    let year = date.get(..4)?;
    Some(format!(
        "SELECT a.id AS aid, a.anchor_id AS anchor_id, a.douyin_no AS douyin_no, \
       a.anchor_name AS anchor_name, a.is_primary AS is_pri, \
       (COALESCE(SUM(CASE WHEN t.d = '{day}' THEN t.w ELSE 0 END), 0))::BIGINT AS wd, \
       (COALESCE(SUM(CASE WHEN t.m = '{month}' THEN t.w ELSE 0 END), 0))::BIGINT AS wm, \
       (COALESCE(SUM(CASE WHEN t.y = '{year}' THEN t.w ELSE 0 END), 0))::BIGINT AS wy \
  FROM account a \
  LEFT JOIN ( \
       SELECT d.anchor_id AS ak, TO_CHAR(d.biz_date, 'YYYY-MM-DD') AS d, \
              TO_CHAR(d.biz_date, 'YYYY-MM') AS m, TO_CHAR(d.biz_date, 'YYYY') AS y, \
              d.wave AS w \
         FROM daily_metric d \
         JOIN account a2 ON a2.anchor_id = d.anchor_id AND a2.person_id = d.person_id \
          AND a2.is_primary = 1 \
        WHERE d.person_id = {person_id} AND d.wave_reliable = 1 \
       UNION ALL \
       SELECT ws.anchor_id AS ak, TO_CHAR(ws.biz_date, 'YYYY-MM-DD') AS d, \
              TO_CHAR(ws.biz_date, 'YYYY-MM') AS m, TO_CHAR(ws.biz_date, 'YYYY') AS y, \
              ws.wave_value AS w \
         FROM wave_snapshot ws \
         JOIN account a3 ON a3.anchor_id = ws.anchor_id AND a3.person_id = {person_id} \
          AND a3.is_primary = 0 \
       ) t ON t.ak = a.anchor_id \
 WHERE a.person_id = {person_id} \
 GROUP BY a.id, a.anchor_id, a.douyin_no, a.anchor_name, a.is_primary \
 ORDER BY a.is_primary DESC, a.id ASC"
    ))
}

/// 解析 star_override（2026-10-12 小红旗条）：`称号|人名|音浪` / `称号|人名` / `称号`。
/// 返回 (称号, Some((人名, 音浪)))：
/// - None / 空串（trim 后）→ 默认称号「每日之星」+ 自动名单（当日音浪第一）
/// - 只有称号 → 换称号，人名/音浪仍走自动名单
/// - 称号 trim 后为空 → 回落「每日之星」
fn parse_star_override(
    ov: Option<&str>,
    auto: Option<(String, i64)>,
) -> (String, Option<(String, i64)>) {
    let (mut title, def) = match ov.map(str::trim).filter(|s| !s.is_empty()) {
        Some(v) => match v.splitn(3, '|').collect::<Vec<_>>()[..] {
            [t, n, w] => (
                t.trim().to_string(),
                Some((n.trim().to_string(), w.trim().parse().unwrap_or(0))),
            ),
            [t, n] => (t.trim().to_string(), Some((n.trim().to_string(), 0))),
            [t] => (t.trim().to_string(), auto),
            _ => ("每日之星".to_string(), auto),
        },
        None => ("每日之星".to_string(), auto),
    };
    if title.is_empty() {
        title = "每日之星".to_string();
    }
    (title, def)
}

/// 业务日白名单：`YYYY-MM-DD` 且是真实日期（挡住拼进 SQL 的注入与脏数据）
fn is_biz_date(s: &str) -> bool {
    s.len() == 10 && chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

/// 某人每个抖音号在「当日 / 本月 / 本年」的音浪（分号明细数据源）。
///
/// 一条 SQL 出结果（150 人规模下每份日报多一条查询，可接受：既有代码本来
/// 也是每人多次查库）。取数失败只 warn 并返回空——调用方据此退回不带明细的文案，
/// 不让一次库抖动掐掉整轮推送。
pub(crate) async fn person_account_waves(
    st: &AppState,
    person_id: u64,
    date: &str,
) -> Vec<AccountWave> {
    let Some(sql) = account_waves_sql(person_id, date) else {
        if person_id > 0 {
            tracing::warn!(person_id, date = %date, "分号明细：业务日不合法，跳过");
        }
        return Vec::new();
    };
    match pgrs::query(&st.pools, &st.active_id(), &sql).await {
        Ok(rows) => rows
            .iter()
            .map(|r| AccountWave {
                douyin_no: r.try_get::<String, _>("douyin_no").unwrap_or_default(),
                anchor_name: r.try_get::<String, _>("anchor_name").unwrap_or_default(),
                anchor_id: r.try_get::<String, _>("anchor_id").unwrap_or_default(),
                primary: row_i64(r, "is_pri") > 0,
                day: row_i64(r, "wd"),
                month: row_i64(r, "wm"),
                year: row_i64(r, "wy"),
            })
            .collect(),
        Err(e) => {
            tracing::warn!(person_id, date = %date, error = %e, "读分号音浪失败，日报不带分号明细");
            Vec::new()
        }
    }
}

/// 读整数字段：库里可能是 SIGNED / UNSIGNED / Decimal 字符串，统一兜成 i64
fn row_i64(r: &sqlx::postgres::PgRow, col: &str) -> i64 {
    if let Ok(v) = r.try_get::<i64, _>(col) {
        return v;
    }
    if let Ok(v) = r.try_get::<i64, _>(col) {
        return v as i64;
    }
    r.try_get::<String, _>(col)
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
}

/// 分号明细段落（纯函数）。少于 2 个号返回空串——只有主号时这段就是「我」那行的
/// 重复，用户口径 2026-10-12 明确不补。
///
/// 排序：**当日音浪降序**（→ 本月 → 本年 → 主号 → 抖音号）。日报语境下主播最关心
/// 「今天哪个号在挣」，所以先按当日排；当日全为 0（今天还没开播）时自然退回看本月。
/// 折叠上限 5（2026-10-12 从 3 提到 5）：用户口径是「多个号都要带着抖音号」，
/// 折掉就看不见号了；生产实测一人最多 3 个号，5 是留余量、同时兜住异常多号的极端。
/// 超过 5 个只显示前 5 个，其余折叠成「…等 N 个号」。
pub(crate) fn account_waves_section(accounts: &[AccountWave]) -> String {
    if accounts.len() < 2 {
        return String::new();
    }
    let mut list: Vec<&AccountWave> = accounts.iter().collect();
    list.sort_by(|x, y| {
        y.day
            .cmp(&x.day)
            .then(y.month.cmp(&x.month))
            .then(y.year.cmp(&x.year))
            .then(y.primary.cmp(&x.primary))
            .then(x.douyin_no.cmp(&y.douyin_no))
    });
    const SHOW_MAX: usize = 5;
    let mut out = format!("\n\n我的抖音号（{} 个）", accounts.len());
    for a in list.iter().take(SHOW_MAX) {
        out.push_str(&format!(
            "\n· {}{} 日 {} / 月 {} / 年 {}",
            a.display_name(),
            if a.primary { "（主号）" } else { "" },
            fmt_wan(a.day),
            fmt_wan(a.month),
            fmt_wan(a.year),
        ));
    }
    if accounts.len() > SHOW_MAX {
        out.push_str(&format!("\n…等 {} 个号", accounts.len() - SHOW_MAX));
    }
    out
}

/// 把三种目标统一折算成「还差多少」+ 文案。纯函数，不碰库，好测。
///
/// - `my`：我本月累计音浪；`rival`：追人目标时对方的本月累计（其它类型忽略）
/// - 返回 `(gap, label, detail, gap_text)`：
///   - `gap <= 0` = 已达成；`gap == i64::MAX` = 追名次但本月还没进榜（永不达成）
///   - `label` 目标名（「本月第 3 名」/「本月目标 50 万」/ 主播名）
///   - `detail` 我的当前值（「你第 5 名」/「你 30 万」/「你 30 万 / TA 40 万」）
///   - `gap_text` 差距的人话（「2 名」/「20 万」/「本月还没开播」）
pub(crate) fn eval_target(
    t: &crate::store::PushTarget,
    my: i64,
    rival: i64,
    month_rank: Option<&[(u64, String, i64)]>,
    person_id: u64,
) -> (i64, String, String, String) {
    match t.kind.as_str() {
        "value" => (
            t.target_value - my,
            format!("本月目标 {}", fmt_wan(t.target_value)),
            format!("你 {}", fmt_wan(my)),
            fmt_wan((t.target_value - my).max(0)),
        ),
        "rank" => {
            let want = t.target_value.max(1);
            let mine = month_rank
                .and_then(|v| v.iter().position(|(pid, _, _)| *pid == person_id))
                .map(|i| i as i64 + 1)
                .unwrap_or(0);
            // 没进榜（本月没播）时用 MAX 作哨兵：永不达成，且不写一个假的差距数
            let gap = if mine == 0 { i64::MAX } else { mine - want };
            (
                gap,
                format!("本月第 {want} 名"),
                if mine == 0 {
                    "你还没进本月榜".to_string()
                } else {
                    format!("你第 {mine} 名")
                },
                if mine == 0 {
                    "本月还没开播".to_string()
                } else {
                    format!("{} 名", (mine - want).max(0))
                },
            )
        }
        _ => {
            /* 自定义目标分（2026-10-12 用户口径「自定义分数，追人的分数」）：
            person 目标 target_value > 0 = 用户设了固定分，rival 用它（对手本月涨跌不影响）；
            target_value = 0 = 旧行为，按对手本月累计。展示文案 target_name 已带「· N 万」。 */
            let rival_now = if t.target_value > 0 {
                t.target_value
            } else {
                rival
            };
            (
                rival_now - my,
                t.target_name.clone(),
                if t.target_value > 0 {
                    format!("你 {}", fmt_wan(my))
                } else {
                    format!("你 {} / TA {}", fmt_wan(my), fmt_wan(rival))
                },
                fmt_wan((rival_now - my).max(0)),
            )
        }
    }
}

/// 每日报告文案：TOP10 + 我的名次 + 我的抖音号分号明细（2026-10-12）。
///
/// 明细挂在「我：第 N 名 · X」之后：先给人一个总览名次，再看每个号各挣多少。
/// `accounts` 为空 / 只有 1 个号时 [`account_waves_section`] 返回空串，文案与旧版一致。
pub(crate) fn daily_report_text_with_accounts(
    team: &str,
    date: &str,
    items: &[(u64, String, i64)],
    me: u64,
    accounts: &[AccountWave],
) -> String {
    let label = if team == "female" { "女团" } else { "男团" };
    let mut out = format!(
        "📊 {} {} 音浪榜（{} 人开播）",
        label,
        date.get(5..).unwrap_or(date),
        items.len()
    );
    for (i, (_, name, wave)) in items.iter().take(10).enumerate() {
        out.push_str(&format!("\n{}. {} {}", i + 1, name, fmt_wan(*wave)));
    }
    if let Some(pos) = items.iter().position(|(pid, _, _)| *pid == me) {
        out.push_str(&format!(
            "\n\n我：第 {} 名 · {}",
            pos + 1,
            fmt_wan(items[pos].2)
        ));
    } else if me > 0 {
        out.push_str("\n\n我：今天还没开播");
    }
    out.push_str(&account_waves_section(accounts));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tgt(kind: &str, name: &str, value: i64, pid: u64) -> crate::store::PushTarget {
        crate::store::PushTarget {
            id: 1,
            kind: kind.to_string(),
            target_person_id: pid,
            target_name: name.to_string(),
            target_value: value,
            hit: false,
            last_notice_date: String::new(),
        }
    }

    /// 小红旗条 override 解析：None → 默认称号 + 自动名单（当日音浪第一）
    #[test]
    fn star_override_none_uses_auto() {
        let (title, def) = parse_star_override(None, Some(("狼哲".into(), 42)));
        assert_eq!(title, "每日之星");
        assert_eq!(def, Some(("狼哲".to_string(), 42)));
    }

    /// 「称号|人名|音浪」三段：全手动指定
    #[test]
    fn star_override_full() {
        let (title, def) = parse_star_override(Some("初级争霸赛冠军|狼哲|285917"), Some(("甲".into(), 1)));
        assert_eq!(title, "初级争霸赛冠军");
        assert_eq!(def, Some(("狼哲".to_string(), 285917)));
    }

    /// 「称号|人名」两段：音浪 = 0
    #[test]
    fn star_override_two_parts() {
        let (title, def) = parse_star_override(Some("冠军|狼哲"), Some(("甲".into(), 1)));
        assert_eq!(title, "冠军");
        assert_eq!(def, Some(("狼哲".to_string(), 0)));
    }

    /// 只有称号：换称号，人名/音浪仍走自动名单
    #[test]
    fn star_override_title_only_passthrough_auto() {
        let (title, def) = parse_star_override(Some("冠军"), Some(("狼哲".into(), 99)));
        assert_eq!(title, "冠军");
        assert_eq!(def, Some(("狼哲".to_string(), 99)));
    }

    /// 空串 / 纯空白 / 空称号 → 回落默认称号；音浪段不是数字 → 0
    #[test]
    fn star_override_empty_falls_back() {
        let (title, def) = parse_star_override(Some(""), Some(("狼哲".into(), 7)));
        assert_eq!(title, "每日之星");
        assert_eq!(def, Some(("狼哲".to_string(), 7)));

        let (title, _) = parse_star_override(Some("   "), None);
        assert_eq!(title, "每日之星");

        // 「|狼哲|285917」：称号段 trim 后为空 → 回落「每日之星」
        let (title, def) = parse_star_override(Some("|狼哲|285917"), None);
        assert_eq!(title, "每日之星");
        assert_eq!(def, Some(("狼哲".to_string(), 285917)));

        // 音浪段解析失败 → 0
        let (_, def) = parse_star_override(Some("冠军|狼哲|abc"), None);
        assert_eq!(def, Some(("狼哲".to_string(), 0)));
    }

    /// 追人：差距 = 对方 - 我，文案要带上双方数值
    #[test]
    fn eval_target_person() {
        let t = tgt("person", "浩冬", 0, 777);
        let (gap, label, detail, gap_text) = eval_target(&t, 300_000, 500_000, None, 1);
        assert_eq!(gap, 200_000, "还差 20 万");
        assert_eq!(label, "浩冬");
        assert_eq!(detail, "你 30 万 / TA 50 万");
        assert_eq!(gap_text, "20 万");

        // 追上了 → gap <= 0
        let (gap, _, _, _) = eval_target(&t, 600_000, 500_000, None, 1);
        assert!(gap <= 0, "超过对方就算追上");
    }

    /// 追数值：本月累计达到目标值
    #[test]
    fn eval_target_value() {
        let t = tgt("value", "", 500_000, 0);
        let (gap, label, detail, gap_text) = eval_target(&t, 300_000, 0, None, 1);
        assert_eq!(gap, 200_000);
        assert_eq!(label, "本月目标 50 万");
        assert_eq!(detail, "你 30 万");
        assert_eq!(gap_text, "20 万");

        let (gap, _, _, _) = eval_target(&t, 500_000, 0, None, 1);
        assert_eq!(gap, 0, "刚好达到 = 达成");
    }

    /// 追名次（2026-10-10）：gap 比的是名次，不是音浪
    #[test]
    fn eval_target_rank() {
        // 本月榜：A 第一、我(1034) 第五
        let rank = vec![
            (1u64, "A".into(), 900_000),
            (2, "B".into(), 800_000),
            (3, "C".into(), 700_000),
            (4, "D".into(), 600_000),
            (1034, "我".into(), 500_000),
        ];
        let t = tgt("rank", "第 3 名", 3, 0);
        let (gap, label, detail, gap_text) = eval_target(&t, 500_000, 0, Some(&rank), 1034);
        assert_eq!(gap, 2, "第 5 名追第 3 名 → 还差 2 名");
        assert_eq!(label, "本月第 3 名");
        assert_eq!(detail, "你第 5 名");
        assert_eq!(gap_text, "2 名");

        // 已经在目标名次内 → 达成
        let (gap, _, _, _) = eval_target(&t, 900_000, 0, Some(&rank), 1);
        assert!(gap <= 0, "第一名追第三名 = 已达成");
    }

    /// 榜单为空（库挂了 / 本月无数据）：同样按没进榜处理，不能 panic
    #[test]
    fn eval_target_person_custom_score_works_without_rival_value() {
        // 月榜缺对手数值（空 map / 库抖动）时，自定义分仍能算差距，不能退成 0
        let t = tgt("person", "张三 · 50 万", 500_000, 1001);
        let (gap, _, _, gap_text) = eval_target(&t, 300_000, 0, None, 9);
        assert_eq!(gap, 200_000, "按 50 万 - 30 万，不用对手缺失的 0");
        assert_eq!(gap_text, "20 万");
    }

    /// 追名次但本月没播（榜单里查不到自己）：不能报「还差 0 名」这种假数据
    #[test]
    fn eval_target_rank_when_not_on_board() {
        let rank = vec![(1u64, "A".into(), 900_000)];
        let t = tgt("rank", "第 3 名", 3, 0);
        let (gap, _, detail, gap_text) = eval_target(&t, 0, 0, Some(&rank), 1034);
        assert_eq!(gap, i64::MAX, "没进榜用哨兵值，永不达成");
        assert_eq!(detail, "你还没进本月榜");
        assert_eq!(gap_text, "本月还没开播");
    }

    /// person 目标带自定义分（2026-10-12）：target_value>0 → 按自定义分对比，
    /// 与对手本月涨跌无关；target_value=0 保持旧行为（比对手本月累计）。
    #[test]
    fn eval_target_person_with_custom_score() {
        // 张三 · 50 万：对手实际才 10 万，也要按 50 万算差距
        let t = tgt("person", "张三 · 50 万", 500_000, 1001);
        let (gap, label, detail, gap_text) = eval_target(&t, 200_000, 100_000, None, 9);
        assert_eq!(
            gap, 300_000,
            "500000 - 200000 = 300000（不是按对手 10 万算）"
        );
        assert_eq!(label, "张三 · 50 万");
        assert_eq!(detail, "你 20 万", "自定义分时不显示对手数值");
        assert_eq!(gap_text, "30 万");

        // 已超过自定义分 → gap<=0 = 达成
        let (gap, _, _, _) = eval_target(&t, 600_000, 100_000, None, 9);
        assert!(gap <= 0);

        // target_value=0 → 旧行为：按对手本月累计
        let legacy = tgt("person", "李四", 0, 1002);
        let (gap, _, detail, _) = eval_target(&legacy, 200_000, 300_000, None, 9);
        assert_eq!(gap, 100_000, "旧口径 = 对手 30 万 - 我 20 万");
        assert_eq!(detail, "你 20 万 / TA 30 万");
    }

    /// 榜单为空（库挂了 / 本月无数据）：同样按没进榜处理，不能 panic
    #[test]
    fn eval_target_rank_with_empty_board() {
        let t = tgt("rank", "第 3 名", 3, 0);
        let (gap, _, _, _) = eval_target(&t, 0, 0, Some(&[]), 1034);
        assert_eq!(gap, i64::MAX);
        assert_eq!(
            eval_target(&t, 0, 0, None, 1034).0,
            i64::MAX,
            "None 榜单同哨兵"
        );
    }

    #[test]
    fn wan_formatting() {
        assert_eq!(fmt_wan(0), "0");
        assert_eq!(fmt_wan(9_999), "9999");
        assert_eq!(fmt_wan(10_000), "1 万");
        assert_eq!(fmt_wan(113_700), "11.4 万");
        assert_eq!(fmt_wan(1_000_000), "100 万");
        assert_eq!(fmt_wan(1234), "1234");
    }

    #[test]
    fn bind_code_shape() {
        for _ in 0..20 {
            let c = new_bind_code();
            assert_eq!(c.len(), BIND_CODE_LEN);
            assert!(looks_like_bind_code(&c), "生成的码要能通过校验: {c}");
            assert!(!c.contains('0') && !c.contains('O') && !c.contains('1') && !c.contains('I'));
        }
        assert!(!looks_like_bind_code("ABC"));
        assert!(!looks_like_bind_code("ABCDE1"), "1 不在码表里");
        assert!(!looks_like_bind_code(""));
    }

    fn aw(douyin_no: &str, primary: bool, day: i64, month: i64, year: i64) -> AccountWave {
        AccountWave {
            douyin_no: douyin_no.to_string(),
            anchor_name: String::new(),
            anchor_id: String::new(),
            primary,
            day,
            month,
            year,
        }
    }

    /// 0 个号 / 1 个号都不加分号明细：一行榜单里已经写着我今天多少，重复没意义
    #[test]
    fn account_waves_section_skips_when_under_two() {
        assert_eq!(account_waves_section(&[]), "");
        assert_eq!(
            account_waves_section(&[aw("cyl765", true, 12_000, 120_000, 880_000)]),
            ""
        );
        // 没有分号数据时就是原本那一条文案（2026-10-12 起唯一的实现只有这个函数，
        // 所以这里**钉死基线字符串**，改动文案时会立刻红）
        let items = vec![(1u64, "阿甲".to_string(), 100_000)];
        assert_eq!(
            daily_report_text_with_accounts("male", "2026-10-08", &items, 1, &[]),
            "📊 男团 10-08 音浪榜（1 人开播）\n1. 阿甲 10 万\n\n我：第 1 名 · 10 万"
        );
    }

    /// 2 个号：加段落，当日音浪高的在前，主号标「（主号）」
    #[test]
    fn account_waves_section_two_accounts() {
        let accs = vec![
            aw("cyl765", true, 12_000, 120_000, 880_000),
            aw("abc123", false, 3_000, 45_000, 300_000),
        ];
        let sec = account_waves_section(&accs);
        assert_eq!(
            sec,
            "\n\n我的抖音号（2 个）\n· cyl765（主号） 日 1.2 万 / 月 12 万 / 年 88 万\n· abc123 日 3000 / 月 4.5 万 / 年 30 万",
            "{sec}"
        );
        // 副号当日更高时排到前面（日报语境：先看今天哪个号在挣）
        let flipped = vec![
            aw("cyl765", true, 1_000, 900_000, 9_000_000),
            aw("abc123", false, 30_000, 40_000, 50_000),
        ];
        let sec2 = account_waves_section(&flipped);
        assert!(
            sec2.find("abc123").unwrap() < sec2.find("cyl765").unwrap(),
            "当日音浪降序：{sec2}"
        );
        assert!(
            sec2.contains("· cyl765（主号）"),
            "主号标记不因排序丢失：{sec2}"
        );
    }

    /// 超过 5 个号才折叠（2026-10-12 上限从 3 提到 5：用户要求多个号都带着抖音号，
    /// 折掉就看不见号了；生产实测一人最多 3 个号，5 是留余量）
    #[test]
    fn account_waves_section_folds_over_five() {
        let seven = vec![
            aw("a1", true, 1, 10, 100),
            aw("a2", false, 9, 90, 900),
            aw("a3", false, 8, 80, 800),
            aw("a4", false, 7, 70, 700),
            aw("a5", false, 6, 60, 600),
            aw("a6", false, 5, 50, 500),
            aw("a7", false, 4, 40, 400),
        ];
        let sec = account_waves_section(&seven);
        assert!(sec.starts_with("\n\n我的抖音号（7 个）"), "{sec}");
        assert_eq!(sec.matches("\n· ").count(), 5, "列 5 行：{sec}");
        // 按**当日音浪降序**取前 5：a2(9) a3(8) a4(7) a5(6) a6(5)；
        // 当日最低的 a1(1) 和 a7(4) 被折掉（日报语境：先看今天哪个号在挣）
        for k in ["a2", "a3", "a4", "a5", "a6"] {
            assert!(sec.contains(k), "{k} 必须带着抖音号出现：{sec}");
        }
        assert!(
            !sec.contains("a1") && !sec.contains("a7"),
            "末尾折叠：{sec}"
        );
        assert!(sec.ends_with("…等 2 个号"), "{sec}");

        // 正好 5 个不折叠
        let five: Vec<AccountWave> = (1..=5)
            .map(|i| aw(&format!("d{i}"), i == 1, i, i, i))
            .collect();
        let sec5 = account_waves_section(&five);
        assert_eq!(sec5.matches("\n· ").count(), 5, "5 个号全列：{sec5}");
        assert!(!sec5.contains("…等"), "5 个号不该折叠：{sec5}");
    }

    /// 推送里的账号标识：**抖音号优先，没登记就用采集 ID**。
    ///
    /// 刻意**不用昵称兜底**（2026-10-12 用户口径「推送多个号带着抖音号」）：
    /// 昵称常常等于主播艺名（生产里 `狼佑`/`狼沐` 就是），一人多号时几行全同名，
    /// 收件人分不清哪个号是哪个 —— 等于没带抖音号。
    #[test]
    fn account_wave_display_name_prefers_douyin_no_then_id() {
        // 有抖音号 → 用抖音号
        let with_no = AccountWave {
            douyin_no: "39863161947".to_string(),
            anchor_name: "狼佑".to_string(),
            anchor_id: "dy_haolong01".to_string(),
            primary: true,
            day: 0,
            month: 0,
            year: 0,
        };
        assert_eq!(with_no.display_name(), "39863161947");

        // 没抖音号 → 退回采集 ID，且带 `ID` 前缀（不能让人把 ID 当成抖音号）
        let id_only = AccountWave {
            douyin_no: String::new(),
            anchor_name: "小柠檬".to_string(),
            anchor_id: "777".to_string(),
            primary: false,
            day: 0,
            month: 0,
            year: 0,
        };
        assert_eq!(id_only.display_name(), "ID 777");
        assert_ne!(
            id_only.display_name(),
            "小柠檬",
            "昵称绝不能当抖音号用（一人多号时会串成同名）"
        );

        // 抖音号两侧有空白（导入脏数据）→ trim 掉，别出现「·  3986… 」这种
        let padded = AccountWave {
            douyin_no: "  39863161947  ".to_string(),
            ..Default::default()
        };
        assert_eq!(padded.display_name(), "39863161947");

        // 连 ID 都没有（理论上不可达）→ 只剩昵称；全空不 panic
        assert_eq!(
            AccountWave {
                anchor_name: "小柠檬".to_string(),
                ..Default::default()
            }
            .display_name(),
            "小柠檬"
        );
        assert_eq!(AccountWave::default().display_name(), "");
    }

    /// 生产真实形态：一人 3 个号、其中 1 个没登记抖音号 —— 三行必须各自可区分
    /// （数据取自 douyin_dev person 86「狼佑」：anchor_id dy_haolong01 / dy_jiuyu01 /
    /// 2961423756440795，后两个的 anchor_name 与本人同名）
    #[test]
    fn account_waves_section_keeps_accounts_distinguishable_without_douyin_no() {
        let accs = vec![
            AccountWave {
                douyin_no: "39863161947".to_string(),
                anchor_name: "狼佑".to_string(),
                anchor_id: "dy_haolong01".to_string(),
                primary: true,
                day: 0,
                month: 943,
                year: 943,
            },
            AccountWave {
                douyin_no: String::new(),
                anchor_name: "狼佑".to_string(),
                anchor_id: "dy_jiuyu01".to_string(),
                primary: false,
                day: 0,
                month: 0,
                year: 0,
            },
            AccountWave {
                douyin_no: String::new(),
                anchor_name: "狼佑".to_string(),
                anchor_id: "2961423756440795".to_string(),
                primary: false,
                day: 0,
                month: 0,
                year: 0,
            },
        ];
        let sec = account_waves_section(&accs);
        assert!(sec.contains("· 39863161947（主号）"), "{sec}");
        assert!(sec.contains("· ID 2961423756440795"), "{sec}");
        assert!(sec.contains("· ID dy_jiuyu01"), "{sec}");
        // 三行标识必须互不相同，否则收件人分不清哪个号是哪个
        // （“· ” 与 “ 日 ” 之间的那一段就是账号标识）
        let names: Vec<&str> = sec
            .lines()
            .filter(|l| l.starts_with("· "))
            .map(|l| {
                l.trim_start_matches("· ")
                    .split(" 日 ")
                    .next()
                    .unwrap_or("")
            })
            .collect();
        let uniq: std::collections::HashSet<&str> = names.iter().copied().collect();
        assert_eq!(uniq.len(), names.len(), "每行标识必须唯一：{sec}");
        assert!(
            !sec.contains("· 狼佑"),
            "昵称不能出现在行首当标识（会串成同名）：{sec}"
        );
    }

    /// 分号明细 SQL：跨年边界（2026-01-01）的月 / 年片段必须正确取到前 7 / 前 4 位
    #[test]
    fn account_waves_sql_boundary_dates() {
        let sql = account_waves_sql(1034, "2026-01-01").expect("合法业务日");
        assert!(sql.contains("t.d = '2026-01-01'"), "{sql}");
        assert!(sql.contains("t.m = '2026-01'"), "{sql}");
        assert!(sql.contains("t.y = '2026'"), "{sql}");
        assert!(sql.contains("1034"), "{sql}");
        assert!(
            sql.trim_start().starts_with("SELECT"),
            "要过 PostgreSQL 只读守卫"
        );

        // 12-31：月/年不能串到下一年
        let sql2 = account_waves_sql(7, "2026-12-31").expect("合法业务日");
        assert!(sql2.contains("t.m = '2026-12'"), "{sql2}");
        assert!(sql2.contains("t.y = '2026'"), "{sql2}");
        assert!(!sql2.contains("2027"), "{sql2}");
    }

    /// 非法输入不生成 SQL（挡住拼进 SQL 的注入 / 脏数据）
    #[test]
    fn account_waves_sql_rejects_bad_input() {
        assert_eq!(account_waves_sql(0, "2026-10-12"), None, "person_id=0");
        assert_eq!(account_waves_sql(1, "2026-10-12' OR 1=1--"), None);
        assert_eq!(account_waves_sql(1, "2026-13-01"), None, "月份不存在");
        assert_eq!(account_waves_sql(1, "2026-2-30"), None, "日期不存在");
        assert_eq!(account_waves_sql(1, "20261012"), None, "必须是 YYYY-MM-DD");
        assert_eq!(account_waves_sql(1, ""), None);
        // 闰年 2 月 29 合法
        assert!(account_waves_sql(1, "2024-02-29").is_some());
    }

    /// 分号明细文案里的数字一律走 fmt_wan（用户口径 2026-10-09：推送用万）
    #[test]
    fn account_waves_section_uses_wan_units() {
        let accs = vec![
            aw("big1", true, 113_700, 1_000_000, 12_345_678),
            aw("small", false, 9_999, 0, 12_345),
        ];
        let sec = account_waves_section(&accs);
        assert!(sec.contains("日 11.4 万 / 月 100 万 / 年 1235 万"), "{sec}");
        assert!(sec.contains("日 9999 / 月 0 / 年 1.2 万"), "{sec}");
        assert_eq!(fmt_wan(113_700), "11.4 万");
        assert_eq!(fmt_wan(9_999), "9999");
    }

    /// 每日报告带分号明细：明细挂在「我：第 N 名」之后
    #[test]
    fn daily_report_text_appends_account_section() {
        let items = vec![
            (1u64, "阿甲".to_string(), 1_200_000),
            (2u64, "阿乙".to_string(), 900_000),
            (9u64, "我".to_string(), 100_000),
        ];
        let accs = vec![
            aw("cyl765", true, 80_000, 500_000, 3_000_000),
            aw("abc123", false, 20_000, 100_000, 900_000),
        ];
        let t = daily_report_text_with_accounts("female", "2026-10-08", &items, 9, &accs);
        assert!(t.contains("3. 我 10 万"), "{t}");
        assert!(t.contains("我：第 3 名 · 10 万"), "{t}");
        let my_pos = t.find("我：第 3 名").unwrap();
        let sec_pos = t.find("我的抖音号（2 个）").unwrap();
        assert!(my_pos < sec_pos, "明细必须在我的名次之后：{t}");
        assert!(
            t.contains("· cyl765（主号） 日 8 万 / 月 50 万 / 年 300 万"),
            "{t}"
        );
        assert!(t.contains("· abc123 日 2 万 / 月 10 万 / 年 90 万"), "{t}");

        // 没开播的人也照常给明细（可能只是当天没播，本月数据是有的）
        let t2 = daily_report_text_with_accounts("male", "2026-10-08", &items, 404, &accs);
        assert!(t2.contains("我：今天还没开播"), "{t2}");
        assert!(t2.contains("我的抖音号（2 个）"), "{t2}");
        let a = t2.find("我：今天还没开播").unwrap();
        let b = t2.find("我的抖音号（2 个）").unwrap();
        assert!(a < b, "明细在最后：{t2}");
    }

    #[test]
    fn daily_report_text_marks_my_rank() {
        let items = vec![
            (1u64, "阿甲".to_string(), 1_200_000),
            (2u64, "阿乙".to_string(), 900_000),
            (3u64, "阿丙".to_string(), 100_000),
        ];
        let t = daily_report_text_with_accounts("male", "2026-10-08", &items, 3, &[]);
        assert!(t.contains("男团 10-08 音浪榜"), "{t}");
        assert!(t.contains("1. 阿甲 120 万"), "{t}");
        assert!(t.contains("我：第 3 名"), "{t}");

        let t2 = daily_report_text_with_accounts("female", "2026-10-08", &items, 9, &[]);
        assert!(t2.contains("今天还没开播"), "{t2}");
    }
}
