//! 展示端「我的」页微信推送引擎（2026-10-09，2026-10-10 改延迟触发）。
//!
//! 触发点：**数据入库后延迟 N 分钟**（默认 30，见 [`delay_minutes`]），不是入库即推——
//! 运营一次导入往往连着发好几个文件（男女团分开、一人多号分开），入库就推会把
//! 半成品榜单推给用户。延迟队列在 `push_jobs` 表，同一 (团, 业务日) 只排一队。
//!
//! 推三类，各自独立开关（都在「我的」页设置）：
//! - **每日报告**：当天音浪榜 TOP10 + 我的名次（同一业务日只推一次）
//! - **追分提醒**：把该账号所有追分目标合成一条消息（追上 / 被反超 / 还差多少），
//!   一天只推一次（按业务日去重），但「追上」「被反超」是事件，发生即推。
//! - **师门日报**（docs/23 §4，2026-10-14）：名下有直系徒弟的师傅收「徒弟音浪对比图」
//!   （同一业务日只推一次，按 `last_lineage_date` 去重；无徒弟不推空图）。
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

/// 绑定码：**4 位数字**（2026-10-16 用户口径：主播在微信里把 4 位数字发给机器人即可绑定，
/// 不再要求手机号，见 [`crate::bot`] 的 `try_bind_weixin`）。
///
/// 只有 10^4 种取值，所以**必须**保证「一个活码只挂一个账号」：库里靠
/// `028_push_bind_code_unique` 的部分唯一索引（`bind_code <> ''`）兜底，
/// 写入侧 [`crate::store::push_rotate_code`] 撞号自动重摇。
pub const BIND_CODE_LEN: usize = 4;
/// 每人最多几个追分目标（用户口径 2026-10-09：5 个）
pub const MAX_TARGETS: i64 = 5;
/// 每日报告图片分页：每页行数（40 行 ≈ 微信一屏半，再长看不清）
const PAGE_ROWS: usize = 40;

/// 是不是一个合法绑定码（防止把正常聊天误判成绑定）。
/// 口径：正好 4 位 ASCII 数字。手机号（7~15 位）长度就对不上，不会被误认。
pub fn looks_like_bind_code(s: &str) -> bool {
    s.len() == BIND_CODE_LEN && s.bytes().all(|b| b.is_ascii_digit())
}

/// 随机绑定码（2026-10-11 审计整改：改走 CSPRNG）。
/// 之前用 RandomState（SipHash）拼字节 —— 它不是密码学随机源，种子弱、输出可预测，
/// 且按字节取模有偏。现在走 crate::random（getrandom → OS CSPRNG，拒绝采样无偏）。
pub fn new_bind_code() -> String {
    crate::random::random_digits(BIND_CODE_LEN)
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

/// 分钟 → 「x时x分」（用户口径 2026-10-16，与展示端 `st_page.html` 的 `minFmt` 同一条规则）：
/// `108` → `1时48分`、整点 `120` → `2时`、不足 1 小时 `45` → `45分`、`0` → `0分`；
/// 不带秒、不用「小时 / 分钟」长词。推送 / 机器人文本与展示端必须长一个样。
///
/// 注意 i64 整除向零截断：负数（不该出现，纯防御）走 `m + "分"` 分支，与 JS 版一致。
pub fn fmt_minutes(v: i64) -> String {
    let h = v / 60;
    let m = v % 60;
    if h > 0 {
        if m == 0 {
            format!("{h}时")
        } else {
            format!("{h}时{m}分")
        }
    } else {
        format!("{m}分")
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

/* -------- 1.0.2：一次推送的总时长硬约束（PUSH_MAX_TOTAL_SECS，默认 180 秒） -------- */

/// 一次推送的**总时长上限**（PUSH_MAX_TOTAL_SECS，默认 180 = 3 分钟）。
/// 用户口径（2026-10-15）：「立即发送就是后端进行，反正在 3 分钟内发送完即可，
/// 确保不会瞬间资源占用过多」。显式设 0 = 关掉这条约束（退回纯 env 节拍）。
pub fn max_total_secs() -> u64 {
    std::env::var("PUSH_MAX_TOTAL_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v <= 3600)
        .unwrap_or(180)
}

/// 自动压缩间隔时的**保底**：绝不为赶时间把间隔压成 0（那就是用户说的
/// 「瞬间资源占用过多」）。宁可超时尽力发，也不瞬间打满。
pub const MIN_BATCH_INTERVAL_SECS: u64 = 1;

/// 一次推送实际采用的节拍（纯函数算出来的结果，便于单测 + 日志核对）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushPace {
    /// 待发人数（至少 1，便于日志/计算有意义）
    pub total: usize,
    /// 批大小
    pub batch: usize,
    /// 最终采用的批间隔（秒）
    pub interval_secs: u64,
    /// 预计总时长 = (批数 - 1) × 间隔（最后一批发完不再等），秒
    pub est_secs: u64,
    /// 是否因为总时长上限而**压缩**了间隔
    pub capped: bool,
    /// 连保底间隔都超预算（人数极多）：已尽力而为，调用方要记警告
    pub over_budget: bool,
}

/// 按「待发人数 + 批大小 + 期望间隔 + 总时长上限」反推**实际采用**的批间隔。
///
/// 批数 `n = ceil(total / batch)`，批间隔个数 `gaps = n - 1`，总时长 ≈ `gaps × interval`。
/// - 按默认档（20 人/批、3 秒）算出来不超上限 → 原样采用（尊重 env 配置）；
/// - 超上限 → `interval = max(上限 / gaps, MIN_BATCH_INTERVAL_SECS)` 自动缩短，
///   `capped = true`；
/// - 连保底间隔都超预算 → 取保底 + `over_budget = true`，按最小间隔尽力发。
/// - `max_total_secs == 0`（显式关约束）或只有 1 批 → 完全不动间隔。
pub fn plan_pace(total: usize, batch: usize, want_interval: u64, max_total_secs: u64) -> PushPace {
    let batch = batch.max(1);
    let total = total.max(1);
    let batches = total.div_ceil(batch);
    let gaps = batches.saturating_sub(1) as u64;
    let mut interval = want_interval;
    let (mut capped, mut over_budget) = (false, false);
    if max_total_secs > 0 && gaps > 0 && gaps.saturating_mul(want_interval) > max_total_secs {
        // 能塞进预算的间隔（向下取整），但不低于保底
        interval = (max_total_secs / gaps)
            .max(MIN_BATCH_INTERVAL_SECS)
            .min(want_interval);
        capped = interval < want_interval;
        over_budget = gaps.saturating_mul(interval) > max_total_secs;
    }
    PushPace {
        total,
        batch,
        interval_secs: interval,
        est_secs: gaps.saturating_mul(interval),
        capped,
        over_budget,
    }
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

/* ─────────── 出网闸门（2026-10-16 测试阶段纪律） ───────────
 *
 * 用户口径：**不向任何 webot 发消息，除非是管理员三位（帅帅 / 饭团 / 鹏先生）
 * 登录后触发**。本地现在是测试阶段，库里躺着真实绑定与待发任务，调度器每分钟
 * 扫一次 —— 不设闸就是真发给主播。
 *
 * 实现：所有对外发送最终都汇聚到 `send_binding` / `send_binding_images` 这两个
 * 函数（文本 / 图片两条通道），闸门只在这两处收口，不存在漏网路径。
 *
 * 开关 `PUSH_OUT_ENABLED`：
 *   - 未配置 / `0`（默认）  = **一律不发**，只记一条 WARN 日志（含收件人与正文摘要，
 *     便于本地验证「本来要发什么」），对调用方返回 Ok —— 不打断调度器的任务流转；
 *   - `1`                    = 恢复真实发送（生产/64 上的既有行为，一字不改）。
 */

/// 是否允许真实出网发送。默认关（测试阶段一律不发）。
pub fn push_out_enabled() -> bool {
    matches!(
        std::env::var("PUSH_OUT_ENABLED").ok().as_deref(),
        Some("1" | "true" | "TRUE")
    )
}

/// 闸门：不允许出网时记日志并返回 true（= 已拦截）。
fn out_blocked(kind: &str, to: &str, preview: &str) -> bool {
    if push_out_enabled() {
        return false;
    }
    let preview: String = preview.chars().take(120).collect();
    tracing::warn!(
        kind, to, preview = %preview,
        "【测试阶段】已拦截对外推送（未发）；要真实发送请设 PUSH_OUT_ENABLED=1"
    );
    true
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
    /* 测试阶段出网闸门：未开 PUSH_OUT_ENABLED 一律不发（见函数上方的口径说明） */
    if out_blocked("text", &b.name, text) {
        return Ok(());
    }
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
    /* 测试阶段出网闸门：图片通道同样收口（与文本同一开关，见 send_binding 上方说明） */
    if out_blocked("image", &b.name, caption) {
        return Ok(());
    }
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
/// pub(crate)：lineage_report.rs 与 admin_send.rs 的图片渲染共用同一份调用。
pub(crate) fn svg_to_png(svg: &str) -> anyhow::Result<Vec<u8>> {
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
    let month_rank_opt = if bindings.iter().any(|b| b.notify_target && b.person_id > 0) {
        Some(month_rank_list(st, team, &month).await)
    } else {
        None
    };
    // 2026-10-12 审计 M6：图片推送 caption 的「本月第 N 名」必须用**月榜**排名。
    // 原来把当日榜 rank_items 传给 greeting_with_rank 算名次——文字与月榜图对不上。
    // 只要有 daily_report 订阅者就要有月榜名次（month_rank_opt 可能没算过，补一次）。
    let month_rank_for_caption = if bindings.iter().any(|b| b.daily_report) {
        Some(match month_rank_opt {
            Some(list) => list,
            None => month_rank_list(st, team, &month).await,
        })
    } else {
        month_rank_opt
    };

    // 每日报告全团共用一份排名，算一次
    let rank_items = if bindings.iter().any(|b| b.daily_report) {
        daily_rank(st, team, date).await
    } else {
        Vec::new()
    };

    /* 修复1·性能：🚩 小红旗条与 build_month（4 条聚合 SQL + 全量名册）对全团是
    同一份结果（DailyReport 只依赖 team+date，daily_star 也在 rep 里），原先在
    订阅者循环里每人各算一次 —— N 个订阅者 = N 次全量重算。现在提到循环外算一次；
    循环内只做按人的 render_month_svg_pages（水印 personal_mark 因收件人而异）
    与 resvg PNG 转换（PNG 必须按人渲染，保留）。构建失败只记一次日志，
    month_rep 保持 None → 循环内 svgs 为空，每个订阅者照旧退回文本推送，
    行为与原先逐人失败一致。 */
    let star = if bindings.iter().any(|b| b.daily_report) {
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
    } else {
        None
    };
    let month_rep: Option<crate::report::DailyReport> = if bindings.iter().any(|b| b.daily_report) {
        match crate::report::build_month(st, date, team, star.clone()).await {
            Ok(rep) => Some(rep),
            Err(e) => {
                tracing::warn!(error = %e, "月榜数据构建失败，全部订阅者退回文本推送");
                None
            }
        }
    } else {
        None
    };

    /* 1.0.2：发送前算好**本次实际采用的节拍**（分批 + 批间隔），保证 3 分钟总时长上限。
    这里只算一次：既避免循环里反复读 env，也让「待发人数/批大小/间隔/预计耗时」能打一条
    INFO 日志给运营核对（用户口径：立即发送也要在 3 分钟内发完、且不瞬间打满）。 */
    let pace = plan_pace(
        bindings.len(),
        batch_size(),
        batch_interval_secs(),
        max_total_secs(),
    );
    tracing::info!(
        total = pace.total,
        batch = pace.batch,
        interval_secs = pace.interval_secs,
        est_secs = pace.est_secs,
        capped = pace.capped,
        over_budget = pace.over_budget,
        "推送分批节拍：待发 {} 人 / 每批 {} 人 / 批间隔 {} 秒 / 预计 {} 秒",
        pace.total,
        pace.batch,
        pace.interval_secs,
        pace.est_secs
    );
    if pace.capped {
        tracing::warn!(
            total = pace.total,
            want_interval_secs = batch_interval_secs(),
            got_interval_secs = pace.interval_secs,
            est_secs = pace.est_secs,
            max_total_secs = max_total_secs(),
            over_budget = pace.over_budget,
            "待发人数较多：为守住总时长上限自动压缩批间隔（{} → {} 秒，保底 {} 秒）{}",
            batch_interval_secs(),
            pace.interval_secs,
            MIN_BATCH_INTERVAL_SECS,
            if pace.over_budget {
                "；连保底间隔也发不完，按最小间隔尽力发送（不会压成 0）"
            } else {
                ""
            }
        );
    }
    let run_started = std::time::Instant::now();

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
                /* 🚩 小红旗条与月榜数据已在循环外算好（修复1·性能）：rep 对全团相同，
                这里只按收件人 render_month_svg_pages（personal_mark 水印因人而异）
                并转 PNG。 */
                let svgs = match &month_rep {
                    Some(rep) => crate::report::render_month_svg_pages(rep, PAGE_ROWS, &b.name),
                    None => Vec::new(), // 构建失败（循环外已 warn）→ 退回文本推送
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
                        month_rank_for_caption.as_deref().unwrap_or(&[]),
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
                        eval_target(t, my, rival, month_rank_for_caption.as_deref(), b.person_id);
                    if gap <= 0 {
                        if !t.hit {
                            // 2026-10-15：文案抽成纯函数 line_target_reached（见函数注释）
                            lines.push(line_target_reached(&label, &detail));
                            event = true;
                        }
                        crate::store::target_set_state(st, t.id, true, 0, date).await;
                    } else {
                        if t.hit {
                            // 2026-10-15：文案抽成纯函数 line_target_regained
                            lines.push(line_target_regained(&label, &detail, &gap_text));
                            event = true;
                        } else if t.last_notice_date != date {
                            // 2026-10-15：文案抽成纯函数 line_target_pending
                            lines.push(line_target_pending(&label, &detail, &gap_text));
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
                    // 2026-10-15：整条文案抽成纯函数 target_push_text（头部/完成度/尾部
                    // 口径见函数注释）；触发条件 event||due 与落库逻辑原样不动。
                    let text = target_push_text(
                        date,
                        &lines,
                        my,
                        value_completion_pct(&targets, my),
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

        // ③ 师门日报（docs/23 §4，2026-10-14）：名下有直系徒弟的师傅收徒弟音浪对比图。
        /* 与 daily_report 完全同构的三层保护：
        - 开关：b.lineage_report（「我的」页第三开关 / 管理端一键开启）；
        - 同业务日去重：b.last_lineage_date == date → skip（对齐 last_report_date，
          补数重排队列时不重复发）；
        - 有徒弟才算师傅：build() 报「师傅不存在」或名下只有师傅本人一行（无徒弟）
          → 记 debug 跳过，不推空图；person_id == 0（管理员观看账号）直接跳过。
        图片路径：按人 build（师傅不同、水印不同，循环内不可外提；同师傅在同一团
        只有一行绑定，天然不重复算）→ render_svg_pages(rep, PAGE_ROWS, 收件人名)
        → 逐页 svg_to_png → send_binding_images(caption = render_text)。
        渲染/PNG 失败降级 wx_proactive 纯文本（同 daily_report 图片失败退文本）；
        发送成功后才写 last_lineage_date（幂等，失败下次再推）。 */
        if b.lineage_report {
            if b.person_id == 0 {
                tracing::debug!(account = %b.name, "师门日报：管理员观看账号（person_id=0），跳过");
            } else if b.last_lineage_date == date {
                tracing::debug!(account = %b.name, date = %date, "师门日报同日已推过，跳过");
            } else {
                match crate::lineage_report::build(st, date, b.person_id).await {
                    Ok(rep) if rep.rows.len() > 1 => {
                        let caption = crate::lineage_report::render_text(&rep);
                        let svgs =
                            crate::lineage_report::render_svg_pages(&rep, PAGE_ROWS, &b.name);
                        let pngs: Vec<Vec<u8>> = svgs
                            .iter()
                            .filter_map(|svg| match svg_to_png(svg) {
                                Ok(p) => Some(p),
                                Err(e) => {
                                    tracing::warn!(account = %b.name, error = %e, "师门日报 SVG 转 PNG 失败");
                                    None
                                }
                            })
                            .collect();
                        if pngs.is_empty() {
                            // 全部页面转 PNG 失败 → 退回文本摘要（同 daily_report 图片失败退文本）
                            tracing::warn!(account = %b.name, "师门日报全部页面转 PNG 失败，退回文本推送");
                            match send_binding(st, b, wx_global.as_ref(), &caption).await {
                                Ok(()) => {
                                    sent_any = true;
                                    crate::store::push_set_lineage_date(st, &b.name, date).await;
                                    tracing::info!(account = %b.name, "师门日报(文本)已推送");
                                }
                                Err(e2) => {
                                    tracing::warn!(account = %b.name, error = %e2, "师门日报文本推送失败");
                                    crate::store::push_touch(st, &b.name, &e2.to_string()).await;
                                }
                            }
                        } else {
                            tracing::info!(account = %b.name, pages = pngs.len(), bytes = pngs.iter().map(|p| p.len()).sum::<usize>(), "推送·师门日报(图片)");
                            match send_binding_images(st, b, &caption, &pngs).await {
                                Ok(()) => {
                                    sent_any = true;
                                    crate::store::push_set_lineage_date(st, &b.name, date).await;
                                    tracing::info!(account = %b.name, pages = pngs.len(), "师门日报(图片)已推送");
                                }
                                Err(e) => {
                                    tracing::warn!(account = %b.name, error = %e, "师门日报图片推送失败，退回文本");
                                    match send_binding(st, b, wx_global.as_ref(), &caption).await {
                                        Ok(()) => {
                                            sent_any = true;
                                            crate::store::push_set_lineage_date(st, &b.name, date)
                                                .await;
                                        }
                                        Err(e2) => {
                                            tracing::warn!(account = %b.name, error = %e2, "师门日报文本兜底也失败");
                                            crate::store::push_touch(st, &b.name, &e2.to_string())
                                                .await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(rep) => tracing::debug!(
                        account = %b.name,
                        rows = rep.rows.len(),
                        "师门日报：名下无直系徒弟（或徒弟全隐藏/离职），skipped"
                    ),
                    Err(e) => {
                        // 「师傅不存在」（档案被删 / person_id 失联）或库抖动：只记日志，
                        // 不写 last_lineage_date，不影响其他订阅者。
                        tracing::warn!(account = %b.name, error = %e, "师门日报数据构建失败，跳过");
                    }
                }
            }
        }

        if sent_any {
            crate::store::push_touch(st, &b.name, "").await;
        }
        /* 分批节拍（1.0.1 起；1.0.2 起用循环外算好的 `pace`）：150 人规模下连环群发容易
        触发 iLink 频控。每发完 pace.batch 个人停 pace.interval_secs 秒；间隔已按
        PUSH_MAX_TOTAL_SECS（默认 180 秒）反推过，人数涨了也不会破 3 分钟。
        批内单人失败照常记日志继续，不影响整批节奏。最后一人不停。 */
        if (idx + 1) % pace.batch == 0 && idx + 1 < bindings.len() {
            if pace.interval_secs > 0 {
                tracing::debug!(
                    sent = idx + 1,
                    total = bindings.len(),
                    interval = pace.interval_secs,
                    "推送分批：本批发完，暂停"
                );
                tokio::time::sleep(Duration::from_secs(pace.interval_secs)).await;
            }
        }
    }
    // 实际耗时（含全部批间隔）：与开头的「预计」对照，方便核对是否真在 3 分钟内。
    tracing::info!(
        total = pace.total,
        est_secs = pace.est_secs,
        actual_secs = run_started.elapsed().as_secs(),
        "推送分批发送完成"
    );
    Ok(())
}

/* ------------------------------ 数据 ------------------------------ */

/* 月音浪口径（2026-10-16 月度音浪导入）：某月**有导入的整月值**（monthly_wave）就用它，
没有该月的整月值才回落 daily_metric 日明细求和 —— 与展示端
[`crate::dialect::period_rollup_sql`]、个人卡 `my_wave_sql` 同一口径，免得推送 / 日报
与展示端的月榜对不上。累计（历史累计）与当日口径不变。
`(person_id, period)` 是唯一键，所以 ON 里带上 period 后 `MAX(mw.wave)` 就是那一个整月值；
该人该月没有整月行时它是 NULL → COALESCE 落到日明细求和。行数 / 返回列与旧版逐项一致
（仍从 person 出发、LEFT JOIN 日明细、没有人还是 0），调用方不用改。
绑定顺序由这两条 SQL 固定：整月版 month → month → team；日明细版 month → team。
⚠️ `HAVING` / `WHERE` 里必须写**聚合表达式本体**：PG 不认 select 别名，
`HAVING wave > 0` 会被解析成 daily_metric.wave（42803 不在 GROUP BY 里）→ 整条查询报错。 */
const MONTH_WAVE_MAP_SQL: &str = "SELECT p.id AS person_id, \
      (COALESCE(MAX(mw.wave), SUM(d.wave), 0))::BIGINT AS wave \
     FROM person p \
     LEFT JOIN daily_metric d ON d.person_id = p.id \
       AND TO_CHAR(d.biz_date, 'YYYY-MM') = ? \
       AND d.wave_reliable = 1 \
     LEFT JOIN monthly_wave mw ON mw.person_id = p.id AND mw.period = ? \
     WHERE p.deleted_at IS NULL AND p.gender = ? \
     GROUP BY p.id";
/// 库没跑 026 迁移（monthly_wave 不存在）时的纯日明细版：与 026 之前的推送口径逐字一致。
const MONTH_WAVE_MAP_SQL_DAILY: &str = "SELECT p.id AS person_id, \
      (COALESCE(SUM(d.wave), 0))::BIGINT AS wave \
     FROM person p \
     LEFT JOIN daily_metric d ON d.person_id = p.id \
       AND TO_CHAR(d.biz_date, 'YYYY-MM') = ? \
       AND d.wave_reliable = 1 \
     WHERE p.deleted_at IS NULL AND p.gender = ? \
     GROUP BY p.id";

/// 跑一条「月音浪优先整月导入值」的查询（带绑定参数）：库没跑 026 迁移时用纯日明细版重试 ——
/// 与 [`crate::dialect::query_month_wave_fallback`] 同一套探测（只管 monthly_wave 这一张表），
/// 只是这里两条 SQL 都带 `?`、走 [`pgrs::query_with_binds`]。缺表只降级不报错，推送不被打挂。
async fn query_month_wave_binds(
    st: &AppState,
    prefer_sql: &str,
    daily_only_sql: &str,
    month: &str,
    team: &str,
) -> anyhow::Result<Vec<sqlx::postgres::PgRow>> {
    match pgrs::query_with_binds(&st.pools, &st.active_id(), prefer_sql, |q| {
        q.bind(month.to_string())
            .bind(month.to_string())
            .bind(team.to_string())
    })
    .await
    {
        Ok(rows) => Ok(rows),
        Err(e) if crate::dialect::is_missing_month_wave_table(&e) => {
            tracing::warn!(
                "monthly_wave 表不存在，推送月音浪按日明细口径回落（请先跑 pk-server migrate 026）"
            );
            pgrs::query_with_binds(&st.pools, &st.active_id(), daily_only_sql, |q| {
                q.bind(month.to_string()).bind(team.to_string())
            })
            .await
        }
        Err(e) => Err(e),
    }
}

/// 本月累计音浪：person_id → wave（只算在册且对外可见的人，与榜单口径一致）
/// P1-7：只聚合可信行（wave_reliable=1）。2026-10-16：月口径与展示端统一成
/// 「整月导入值优先、没有才日明细求和」（见 [`MONTH_WAVE_MAP_SQL`]）；
/// 没有数据的人照旧是 0，map 语义不变。
pub(crate) async fn month_wave_map(st: &AppState, team: &str, month: &str) -> HashMap<u64, i64> {
    let rows = match query_month_wave_binds(
        st,
        MONTH_WAVE_MAP_SQL,
        MONTH_WAVE_MAP_SQL_DAILY,
        month,
        team,
    )
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "读本月累计失败，追分按 0 处理");
            return HashMap::new();
        }
    };
    let mut map = HashMap::new();
    for r in &rows {
        let pid = r.try_get::<i64, _>("person_id").unwrap_or(0);
        let w = r.try_get::<i64, _>("wave").unwrap_or(0);
        if pid > 0 {
            map.insert(pid as u64, w);
        }
    }
    map
}

/// 本月团内排名（(person_id, 名字, 音浪)，按音浪降序）。追名次目标用。
/// 口径与月榜一致：在册 + 未离团 + 不隐藏；本月完全没有数据的不进榜。
/// P1-7：只聚合可信行（wave_reliable=1）。P1-6：日期参数化（不再内联）。
/// 2026-10-16：月音浪与展示端统一成「整月导入值优先、没有才日明细求和」。
/// ⚠️ `HAVING` 必须写聚合表达式本体（PG 不认 select 别名）：原先的 `HAVING wave > 0`
/// 被解析成 daily_metric.wave → 42803（`must appear in the GROUP BY clause`），
/// 这条查询在 PG 上一直整条报错、月榜恒为空（2026-10-16 本地库实测复现）。
const MONTH_RANK_SQL: &str = "SELECT p.id AS person_id, p.name, \
      (COALESCE(MAX(mw.wave), SUM(d.wave), 0))::BIGINT AS wave \
     FROM person p \
     LEFT JOIN daily_metric d ON d.person_id = p.id \
       AND TO_CHAR(d.biz_date, 'YYYY-MM') = ? \
       AND d.wave_reliable = 1 \
     LEFT JOIN monthly_wave mw ON mw.person_id = p.id AND mw.period = ? \
     WHERE p.deleted_at IS NULL AND p.gender = ? AND p.status <> 'left' \
       AND p.hide_in_daily_report = 0 \
     GROUP BY p.id, p.name \
     HAVING (COALESCE(MAX(mw.wave), SUM(d.wave), 0)) > 0 \
     ORDER BY wave DESC, p.name ASC";
/// 库没跑 026 迁移时的纯日明细版（除 HAVING 修正外，与 026 之前的形状一致）。
const MONTH_RANK_SQL_DAILY: &str = "SELECT p.id AS person_id, p.name, \
      (COALESCE(SUM(d.wave), 0))::BIGINT AS wave \
     FROM person p \
     LEFT JOIN daily_metric d ON d.person_id = p.id \
       AND TO_CHAR(d.biz_date, 'YYYY-MM') = ? \
       AND d.wave_reliable = 1 \
     WHERE p.deleted_at IS NULL AND p.gender = ? AND p.status <> 'left' \
       AND p.hide_in_daily_report = 0 \
     GROUP BY p.id, p.name \
     HAVING (COALESCE(SUM(d.wave), 0)) > 0 \
     ORDER BY wave DESC, p.name ASC";

pub(crate) async fn month_rank_list(
    st: &AppState,
    team: &str,
    month: &str,
) -> Vec<(u64, String, i64)> {
    match query_month_wave_binds(st, MONTH_RANK_SQL, MONTH_RANK_SQL_DAILY, month, team).await {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                (
                    r.try_get::<i64, _>("person_id")
                        .map(|v| v.max(0) as u64)
                        .unwrap_or(0),
                    r.try_get::<String, _>("name").unwrap_or_default(),
                    r.try_get::<i64, _>("wave")
                        .or_else(|_| r.try_get::<i64, _>("wave"))
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
/// ⚠️ 2026-10-16 修正 `HAVING wave >= ?`（select 别名在 PG 的 HAVING 里不可见，
/// 会被解析成 daily_metric.wave → 42803 整条报错、当日榜恒为空，本地库实测复现）；
/// 日口径本身不变，仍是「当日音浪 >= 阈值」，只是改写成聚合表达式本体。
/// 绑定顺序：date → team → 阈值。
const DAILY_RANK_SQL: &str = "SELECT p.id AS person_id, p.name, \
      (COALESCE(SUM(d.wave), 0))::BIGINT AS wave \
     FROM person p \
     JOIN daily_metric d ON d.person_id = p.id \
       AND TO_CHAR(d.biz_date, 'YYYY-MM-DD') = ? \
       AND d.wave_reliable = 1 \
     WHERE p.deleted_at IS NULL AND p.gender = ? AND p.status <> 'left' \
       AND p.hide_in_daily_report = 0 \
     GROUP BY p.id, p.name \
     HAVING (COALESCE(SUM(d.wave), 0)) >= ? \
     ORDER BY wave DESC, p.name ASC";

pub(crate) async fn daily_rank(st: &AppState, team: &str, date: &str) -> Vec<(u64, String, i64)> {
    let sql = DAILY_RANK_SQL;
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
                        .or_else(|_| r.try_get::<i64, _>("wave"))
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
        return v;
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

/* --------------------- 追分提醒文案（2026-10-15 改版：纯函数拼装） --------------------- */

/// 追分提醒「已追上」行（纯函数）。2026-10-15 文案改版：
/// - 缩进用全角空格替代两个半角空格——微信里半角空格易被吞/错位，等宽显示更稳；
/// - 去掉句尾感叹号：简报语气，不喊口号。
fn line_target_reached(label: &str, detail: &str) -> String {
    format!("🎉 已追上「{label}」\n　{detail}")
}

/// 追分提醒「被反超」行（纯函数）：曾追上又差回来，是事件，语气最重（⚠️）。
/// 旧文案「又差回来了」偏口语，改成「被反超」更直接点出原因。
fn line_target_regained(label: &str, detail: &str, gap_text: &str) -> String {
    format!("⚠️ 「{label}」被反超\n　{detail}，还差 {gap_text}")
}

/// 追分提醒「进行中」行（纯函数）：日常进度播报。改成两行式（目标一行、
/// 现状+差距一行），比旧的「距 X 还差 N（详情）」单行更易扫读。
fn line_target_pending(label: &str, detail: &str, gap_text: &str) -> String {
    format!("· {label}\n　{detail}，还差 {gap_text}")
}

/// 数值目标（kind=="value" 且 target_value>0）的完成度，0-100 四舍五入取整，
/// 超额封顶 100（不出「180%」这种）。多个数值目标取第一个（生产上一个账号
/// 基本只设一个数值目标）；没有符合条件的目标返回 None，文案就不出完成度行。
fn value_completion_pct(targets: &[crate::store::PushTarget], my: i64) -> Option<u32> {
    let t = targets
        .iter()
        .find(|t| t.kind == "value" && t.target_value > 0)?;
    // i128 中间量防 my*100 溢出；+target_value/2 实现四舍五入（target_value>0）
    let num = my as i128 * 100 + t.target_value as i128 / 2;
    let pct = (num / t.target_value as i128).clamp(0, 100) as u32;
    Some(pct)
}

/// 追分提醒整条文案（纯函数，2026-10-15 改版）：
/// 头部「📈 追分进度（MM-DD）」（去掉旧版「数据」二字，更短）+ 各目标行 +
/// 【完成度行（仅有数值目标时）】+ 空行 + 尾部「你本月累计 X」+「调整目标」指引。
/// 完成度行放尾部前：它是本月小结性质，与逐目标进度行分开，放空行前更顺。
fn target_push_text(date: &str, lines: &[String], my: i64, completion_pct: Option<u32>) -> String {
    let mut out = format!("📈 追分进度（{}）", date.get(5..).unwrap_or(date));
    out.push('\n');
    out.push_str(&lines.join("\n"));
    if let Some(p) = completion_pct {
        out.push_str(&format!("\n本月目标完成度 {p}%"));
    }
    out.push_str(&format!(
        "\n\n你本月累计 {}\n调整目标：展示页 → 我的 → 追分设置",
        fmt_wan(my)
    ));
    out
}

/// 每日报告文案：TOP10 + 我的名次 + 我的抖音号分号明细（2026-10-12）。
///
/// 明细挂在「我：第 N 名 · X」之后：先给人一个总览名次，再看每个号各挣多少。
/// 2026-10-15 起名次行后按名次补一句：≤3 名给「🏆 上榜 TOP3」肯定行，
/// >3 名且上一名可算时给「距上一名」差距行（我未开播则两者都不出）。
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
        // 2026-10-15 新增：名次行后的正向反馈 / 追赶提示
        if pos < 3 {
            // 上榜 TOP3：先给一句肯定（简报也要有情绪价值）
            out.push_str("\n🏆 上榜 TOP3，继续保持！");
        } else {
            // 3 名开外：给一个可执行的追赶目标（上一名的音浪差）。
            // 榜单按音浪降序，above >= my，这里 max(0) 只兜异常排序的脏数据。
            let above = items[pos - 1].2;
            out.push_str(&format!("\n距上一名 {}", fmt_wan((above - items[pos].2).max(0))));
        }
    } else if me > 0 {
        out.push_str("\n\n我：今天还没开播");
    }
    out.push_str(&account_waves_section(accounts));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 时长格式与展示端 `minFmt` 必须是同一条规则（2026-10-16 用户口径「x时x分去掉秒」，
    /// 机器人推送 / 日报文本与展示端长一个样）。
    #[test]
    fn fmt_minutes_matches_display_format() {
        assert_eq!(fmt_minutes(0), "0分");
        assert_eq!(fmt_minutes(45), "45分");
        assert_eq!(fmt_minutes(59), "59分");
        assert_eq!(fmt_minutes(60), "1时");
        assert_eq!(fmt_minutes(108), "1时48分");
        assert_eq!(fmt_minutes(120), "2时");
        assert_eq!(fmt_minutes(4529), "75时29分");
        assert_eq!(fmt_minutes(1439), "23时59分");
        assert_eq!(fmt_minutes(1440), "24时");
        // 防御：负数不该出现，但行为要与 JS 版一致（都不进「时」分支）
        assert_eq!(fmt_minutes(-30), "-30分");
        assert_eq!(fmt_minutes(-90), "-30分");
    }

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
        for _ in 0..40 {
            let c = new_bind_code();
            assert_eq!(c.len(), BIND_CODE_LEN);
            assert!(c.bytes().all(|b| b.is_ascii_digit()), "绑定码必须是纯数字: {c}");
            assert!(looks_like_bind_code(&c), "生成的码要能通过校验: {c}");
        }
        assert!(!looks_like_bind_code("123"), "位数不够");
        assert!(!looks_like_bind_code("12345"), "位数超了");
        assert!(!looks_like_bind_code("12a4"), "非数字不认");
        assert!(!looks_like_bind_code(""), "空串不认");
        // 手机号是 11 位，永远不会被当成 4 位绑定码
        assert!(!looks_like_bind_code("13800138000"));
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
        // 所以这里**钉死基线字符串**，改动文案时会立刻红）。
        // 2026-10-15：第 1 名 ≤3 → 名次行后新增「🏆 上榜 TOP3」行，基线随之更新。
        let items = vec![(1u64, "阿甲".to_string(), 100_000)];
        assert_eq!(
            daily_report_text_with_accounts("male", "2026-10-08", &items, 1, &[]),
            "📊 男团 10-08 音浪榜（1 人开播）\n1. 阿甲 10 万\n\n我：第 1 名 · 10 万\n🏆 上榜 TOP3，继续保持！"
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

    /* ---------- 追分提醒文案（2026-10-15 改版，纯函数） ---------- */

    /// 三种行各归其位：已追上（🎉）/ 被反超（⚠️）/ 进行中（·），
    /// 整条文案的头、尾、完成度行、调整入口一个不缺（钉死基线字符串）
    #[test]
    fn target_push_text_mixes_three_line_kinds() {
        let lines = vec![
            line_target_reached("本月目标 50 万", "你 52 万"),
            line_target_regained("浩冬", "你 30 万 / TA 40 万", "10 万"),
            line_target_pending("本月第 3 名", "你第 5 名", "2 名"),
        ];
        let t = target_push_text("2026-10-15", &lines, 520_000, Some(104));
        assert_eq!(
            t,
            "📈 追分进度（10-15）\n\
             🎉 已追上「本月目标 50 万」\n　你 52 万\n\
             ⚠️ 「浩冬」被反超\n　你 30 万 / TA 40 万，还差 10 万\n\
             · 本月第 3 名\n　你第 5 名，还差 2 名\n\
             本月目标完成度 104%\n\
             \n\
             你本月累计 52 万\n\
             调整目标：展示页 → 我的 → 追分设置",
            "{t}"
        );
        // 头部只带 MM-DD，不再带「数据」二字（2026-10-15 口径）
        assert!(!t.contains("数据）"), "{t}");
        // 缩进必须是全角空格（微信等宽显示更稳），不能退回两个半角空格
        assert!(!t.contains("\n  "), "不允许两个半角空格缩进：{t}");
    }

    /// 没有数值目标 → 不出完成度行（person / rank 目标都不算完成度）
    #[test]
    fn target_push_text_omits_completion_without_value_target() {
        let lines = vec![line_target_pending("浩冬", "你 30 万 / TA 40 万", "10 万")];
        let t = target_push_text("2026-10-15", &lines, 300_000, None);
        assert!(!t.contains("完成度"), "{t}");
        assert!(t.contains("你本月累计 30 万"), "{t}");
        assert!(t.ends_with("调整目标：展示页 → 我的 → 追分设置"), "{t}");
    }

    /// 完成度百分比边界：0 / 四舍五入 / 恰好 100 / 超额封顶 100
    #[test]
    fn value_completion_pct_boundaries() {
        let tv = |v: i64| vec![crate::store::PushTarget {
            id: 1,
            kind: "value".to_string(),
            target_person_id: 0,
            target_name: String::new(),
            target_value: v,
            hit: false,
            last_notice_date: String::new(),
        }];
        // 0 → 0%
        assert_eq!(value_completion_pct(&tv(500_000), 0), Some(0));
        // 22.74% 四舍五入 → 23%
        assert_eq!(value_completion_pct(&tv(500_000), 113_700), Some(23));
        // 恰好 100%
        assert_eq!(value_completion_pct(&tv(500_000), 500_000), Some(100));
        // 超额封顶 100%（不出 180%）
        assert_eq!(value_completion_pct(&tv(500_000), 900_000), Some(100));
        // target_value <= 0（脏数据）不出完成度
        assert_eq!(value_completion_pct(&tv(0), 123), None);
        assert_eq!(value_completion_pct(&tv(-5), 123), None);
        // kind 不是 value 不算（rank / person 目标没有「完成度」概念）
        let rank = vec![crate::store::PushTarget {
            kind: "rank".to_string(),
            target_value: 3,
            ..tv(1).remove(0)
        }];
        assert_eq!(value_completion_pct(&rank, 500_000), None);
        // 空目标列表同样 None
        assert_eq!(value_completion_pct(&[], 500_000), None);
        // 多个数值目标取第一个（约定见函数注释）
        let two = [tv(400_000), tv(800_000)].concat();
        assert_eq!(value_completion_pct(&two, 200_000), Some(50));
    }

    /* ---------- 每日报告：TOP3 行 / 距上一名行（2026-10-15 新增） ---------- */

    /// 第 1-3 名补「🏆 上榜 TOP3」；3 名开外补「距上一名」；未开播两者都不出
    #[test]
    fn daily_report_text_top3_line_and_gap_to_above() {
        let items = vec![
            (1u64, "阿甲".to_string(), 1_200_000),
            (2u64, "阿乙".to_string(), 900_000),
            (3u64, "阿丙".to_string(), 700_000),
            (4u64, "阿丁".to_string(), 500_000),
            (9u64, "我".to_string(), 113_700),
        ];
        // 第 2 名：有 TOP3 行，没有距上一名行
        let t2 = daily_report_text_with_accounts("male", "2026-10-15", &items, 2, &[]);
        assert!(t2.contains("🏆 上榜 TOP3，继续保持！"), "{t2}");
        assert!(!t2.contains("距上一名"), "{t2}");
        // 第 3 名：同样只给 TOP3 行（≤3 的口径含第 3）
        let t3 = daily_report_text_with_accounts("male", "2026-10-15", &items, 3, &[]);
        assert!(t3.contains("我：第 3 名 · 70 万"), "{t3}");
        assert!(t3.contains("🏆 上榜 TOP3"), "{t3}");

        // 第 5 名：距上一名 = 50 万 - 11.37 万 ≈ 38.6 万（fmt_wan 四舍五入到 38.6 万）
        let t5 = daily_report_text_with_accounts("male", "2026-10-15", &items, 9, &[]);
        assert!(t5.contains("我：第 5 名 · 11.4 万"), "{t5}");
        assert!(t5.contains("\n距上一名 38.6 万"), "{t5}");
        assert!(!t5.contains("TOP3"), "{t5}");

        // 距上一名行必须紧贴「我：第 N 名」之后、分号明细（若有）之前
        let accs = vec![
            aw("cyl765", true, 80_000, 500_000, 3_000_000),
            aw("abc123", false, 20_000, 100_000, 900_000),
        ];
        let t5b = daily_report_text_with_accounts("male", "2026-10-15", &items, 9, &accs);
        let gap_pos = t5b.find("距上一名").unwrap();
        let my_pos = t5b.find("我：第 5 名").unwrap();
        let sec_pos = t5b.find("我的抖音号").unwrap();
        assert!(my_pos < gap_pos && gap_pos < sec_pos, "{t5b}");

        // 榜首（pos==0）不补距上一名，只给 TOP3（第 1 名必然 ≤3，不会走到差距分支）
        let t1 = daily_report_text_with_accounts("male", "2026-10-15", &items, 1, &[]);
        assert!(t1.contains("🏆 上榜 TOP3"), "{t1}");
        assert!(!t1.contains("距上一名"), "{t1}");

        // 未开播：两个新增行都不出
        let t404 = daily_report_text_with_accounts("male", "2026-10-15", &items, 404, &[]);
        assert!(t404.contains("我：今天还没开播"), "{t404}");
        assert!(!t404.contains("TOP3") && !t404.contains("距上一名"), "{t404}");
    }

    /// 与上一名打平（差距 0）：按口径「差距可算就出」，出「距上一名 0」——
    /// 打平本身是有用信息（再涨 1 就能反超），不像脏数据需要隐藏
    #[test]
    fn daily_report_text_tied_with_above_shows_zero_gap() {
        let items = vec![
            (1u64, "阿甲".to_string(), 500_000),
            (2u64, "阿乙".to_string(), 500_000),
            (3u64, "阿丙".to_string(), 400_000),
            (4u64, "我".to_string(), 400_000),
        ];
        let t = daily_report_text_with_accounts("male", "2026-10-15", &items, 4, &[]);
        assert!(t.contains("我：第 4 名 · 40 万"), "{t}");
        assert!(t.contains("\n距上一名 0"), "打平要出 0 差距行：{t}");
    }

    /* ---------- 推送节拍反推（PUSH_MAX_TOTAL_SECS=180 硬约束） ---------- */

    /// 默认档（20 人/批、3 秒/批）下的实际规模：都必须在 180 秒内，且原样采用配置
    #[test]
    fn pace_keeps_default_interval_within_budget() {
        for (total, want_batches, want_est) in [(3usize, 1u64, 0u64), (150, 8, 21), (1000, 50, 147)]
        {
            let p = plan_pace(total, 20, 3, 180);
            println!(
                "[pace] total={} batch={} interval={} est={}s capped={} over={}",
                p.total, p.batch, p.interval_secs, p.est_secs, p.capped, p.over_budget
            );
            assert_eq!(p.interval_secs, 3, "预算内不动默认间隔（{total} 人）");
            assert_eq!(p.est_secs, want_est, "{total} 人预计耗时");
            assert_eq!(
                (p.total as u64).div_ceil(p.batch as u64),
                want_batches,
                "{total} 人的批数"
            );
            assert!(p.est_secs <= 180, "{total} 人必须 ≤180 秒");
            assert!(!p.capped && !p.over_budget);
        }
    }

    /// 1200 人仍在预算内（边界），1300 人开始自动压缩间隔
    #[test]
    fn pace_shrinks_interval_past_budget() {
        let edge = plan_pace(1200, 20, 3, 180);
        println!(
            "[pace] total={} batch={} interval={} est={}s capped={} over={}",
            edge.total, edge.batch, edge.interval_secs, edge.est_secs, edge.capped, edge.over_budget
        );
        assert_eq!(edge.est_secs, 177, "1200 人（59 个间隔）仍守得住");
        assert!(!edge.capped);

        let over = plan_pace(1300, 20, 3, 180);
        println!(
            "[pace] total={} batch={} interval={} est={}s capped={} over={}",
            over.total, over.batch, over.interval_secs, over.est_secs, over.capped, over.over_budget
        );
        assert!(over.capped, "1300 人必须自动压缩间隔");
        assert_eq!(over.interval_secs, 2, "180/64 → 2 秒");
        assert!(over.est_secs <= 180, "压缩后要落回预算内");
    }

    /// 极端规模：连保底间隔都发不完 → 取最小间隔、over_budget=true、绝不压成 0
    #[test]
    fn pace_extreme_falls_back_to_min_interval_never_zero() {
        let p = plan_pace(20000, 20, 3, 180);
        println!(
            "[pace] total={} batch={} interval={} est={}s capped={} over={}",
            p.total, p.batch, p.interval_secs, p.est_secs, p.capped, p.over_budget
        );
        assert!(p.capped, "必须压缩");
        assert_eq!(p.interval_secs, MIN_BATCH_INTERVAL_SECS, "取保底间隔");
        assert!(p.interval_secs > 0, "绝不压成 0（瞬间打满）");
        assert!(p.over_budget, "已尽力而为，调用方要记警告");

        // 小批 + 大间隔的另一种极端：1000 人每批 5 个、间隔 10 秒 → 199 个间隔
        let q = plan_pace(1000, 5, 10, 180);
        println!(
            "[pace] total={} batch={} interval={} est={}s capped={} over={}",
            q.total, q.batch, q.interval_secs, q.est_secs, q.capped, q.over_budget
        );
        assert_eq!(q.interval_secs, MIN_BATCH_INTERVAL_SECS);
        assert!(q.over_budget && q.interval_secs > 0);
    }

    /// 只有一批（人数 ≤ 批大小）：没有批间隔，间隔与上限都不影响
    #[test]
    fn pace_single_batch_has_no_gap() {
        let p = plan_pace(5, 20, 3, 180);
        assert_eq!((p.interval_secs, p.est_secs, p.capped), (3, 0, false));
        // 单人
        let one = plan_pace(1, 20, 3, 180);
        assert_eq!((one.interval_secs, one.est_secs), (3, 0));
    }

    /// 显式关掉约束（PUSH_MAX_TOTAL_SECS=0）→ 完全尊重 env 间隔
    #[test]
    fn pace_zero_budget_disables_the_cap() {
        let p = plan_pace(20000, 20, 3, 0);
        assert_eq!(p.interval_secs, 3);
        assert!(!p.capped && !p.over_budget);
        // 显式配成 0 秒（运维主动关掉节拍）也原样尊重
        let z = plan_pace(1000, 20, 0, 180);
        assert_eq!((z.interval_secs, z.est_secs), (0, 0));
    }

    /// 环境变量默认值：PUSH_MAX_TOTAL_SECS 缺省 180（别把 delay 默认值也改了）
    #[test]
    fn max_total_secs_defaults_to_180() {
        if std::env::var("PUSH_MAX_TOTAL_SECS").is_ok() {
            return; // 本机显式设过就不测默认值，免得 CI/本地互相打脸
        }
        assert_eq!(max_total_secs(), 180);
        assert_eq!(delay_minutes(), 30, "本卡不许动 PUSH_DELAY_MINUTES 默认值");
    }

    /* ------- 月音浪口径（2026-10-16 月度音浪导入：整月优先、缺月回落日明细） ------- */

    /// 月口径必须「某月有导入的整月音浪（monthly_wave）就用它，没有才日明细求和」，
    /// 与展示端 period_rollup_sql / my_wave_sql 同一套 COALESCE —— 否则推送 / 日报
    /// 的月榜和展示端对不上。日明细侧仍只算可信行，窗口仍是参数化月份。
    #[test]
    fn month_wave_sql_prefers_monthly_import_over_daily_sum() {
        assert!(
            MONTH_WAVE_MAP_SQL.contains("COALESCE(MAX(mw.wave), SUM(d.wave), 0)"),
            "月累计要「整月优先、缺月回落日明细」：{MONTH_WAVE_MAP_SQL}"
        );
        assert!(
            MONTH_WAVE_MAP_SQL
                .contains("LEFT JOIN monthly_wave mw ON mw.person_id = p.id AND mw.period = ?"),
            "整月音浪按 (人, 月) 对齐：{MONTH_WAVE_MAP_SQL}"
        );
        // 形状不变：仍从 person 出发、仍按团别过滤、没有数据的人仍是 0
        assert!(
            MONTH_WAVE_MAP_SQL.contains("FROM person p"),
            "{MONTH_WAVE_MAP_SQL}"
        );
        assert!(
            MONTH_WAVE_MAP_SQL.contains("WHERE p.deleted_at IS NULL AND p.gender = ?"),
            "{MONTH_WAVE_MAP_SQL}"
        );
        assert!(
            MONTH_WAVE_MAP_SQL.contains("GROUP BY p.id"),
            "{MONTH_WAVE_MAP_SQL}"
        );
        assert!(
            MONTH_WAVE_MAP_SQL.contains("AND d.wave_reliable = 1"),
            "日明细侧只算可信行：{MONTH_WAVE_MAP_SQL}"
        );
        assert!(
            MONTH_WAVE_MAP_SQL.contains("TO_CHAR(d.biz_date, 'YYYY-MM') = ?"),
            "回落窗口要带年份且参数化（P1-6 不许内联）：{MONTH_WAVE_MAP_SQL}"
        );
        // 名次榜同一口径，且「没数据的不进榜」用聚合表达式判（不许写别名，见下一条用例）
        assert!(
            MONTH_RANK_SQL.contains("COALESCE(MAX(mw.wave), SUM(d.wave), 0)"),
            "{MONTH_RANK_SQL}"
        );
        assert!(
            MONTH_RANK_SQL
                .contains("LEFT JOIN monthly_wave mw ON mw.person_id = p.id AND mw.period = ?"),
            "{MONTH_RANK_SQL}"
        );
        assert!(
            MONTH_RANK_SQL.contains("AND p.status <> 'left'")
                && MONTH_RANK_SQL.contains("AND p.hide_in_daily_report = 0"),
            "在册 / 可见过滤不能丢：{MONTH_RANK_SQL}"
        );
        assert!(
            MONTH_RANK_SQL.contains("HAVING (COALESCE(MAX(mw.wave), SUM(d.wave), 0)) > 0"),
            "{MONTH_RANK_SQL}"
        );
    }

    /// 库没跑 026（monthly_wave 不存在）→ 退回纯日明细版，SQL 里不能再出现这张表 / 这个别名，
    /// 推送不能因为少一张表打挂（query_month_wave_binds 按错误点名降级重试）。
    #[test]
    fn month_wave_sql_daily_fallback_has_no_month_table() {
        for sql in [MONTH_WAVE_MAP_SQL_DAILY, MONTH_RANK_SQL_DAILY] {
            assert!(!sql.contains("monthly_wave"), "日明细版不能带整月表：{sql}");
            assert!(!sql.contains("mw."), "日明细版不能带 mw 别名：{sql}");
            assert!(
                sql.contains("(COALESCE(SUM(d.wave), 0))::BIGINT AS wave"),
                "日明细版仍是原口径（缺省 0）：{sql}"
            );
            assert!(sql.contains("AND d.wave_reliable = 1"), "{sql}");
        }
    }

    /// P1-4 同款护栏：两条月音浪 SQL 的 `?` 个数必须与 query_month_wave_binds 的绑定个数
    /// 一致（整月版 month→month→team 三个，日明细版 month→team 两个）——谁给 SQL 加/减一个
    /// `?` 而忘了改 bind，PG 会报「bind message supplies N parameters」整条失败。
    #[test]
    fn month_wave_sql_placeholder_counts() {
        use crate::dialect::placeholder_count;
        assert_eq!(placeholder_count(MONTH_WAVE_MAP_SQL), 3);
        assert_eq!(placeholder_count(MONTH_WAVE_MAP_SQL_DAILY), 2);
        assert_eq!(placeholder_count(MONTH_RANK_SQL), 3);
        assert_eq!(placeholder_count(MONTH_RANK_SQL_DAILY), 2);
        assert_eq!(placeholder_count(DAILY_RANK_SQL), 3, "date→team→阈值");
    }

    /// PG 的 HAVING / WHERE 里**看不到 select 别名**：`HAVING wave > 0` 会被解析成
    /// daily_metric.wave（不在 GROUP BY 里）→ 42803，整条查询恒报错、月榜 / 当日榜恒为空。
    /// 2026-10-16 本地库实测复现（month_rank_list 与 daily_rank 两条都中招）。
    /// 这里钉死「一律写聚合表达式本体」，谁改回别名这条测试先红。
    #[test]
    fn rank_having_uses_aggregate_not_alias() {
        for sql in [MONTH_RANK_SQL, MONTH_RANK_SQL_DAILY, DAILY_RANK_SQL] {
            assert!(
                !sql.contains("HAVING wave"),
                "HAVING 里不许用 select 别名（PG 解析成 daily_metric.wave → 42803）：{sql}"
            );
            assert!(
                sql.contains("HAVING (COALESCE("),
                "HAVING 必须写聚合表达式本体：{sql}"
            );
        }
        assert!(
            DAILY_RANK_SQL.contains("HAVING (COALESCE(SUM(d.wave), 0)) >= ?"),
            "日口径不变：当日音浪 >= 阈值（阈值仍走绑定）：{DAILY_RANK_SQL}"
        );
    }

    /* ---------------- 出网闸门（2026-10-16 测试阶段纪律） ----------------
     * 这三个用例都要改 PUSH_OUT_ENABLED 环境变量，而 `cargo test` 的多线程
     * 用例共享进程环境 —— 不串行会互相污染（实测：并跑时 set_var 把另一个
     * 用例的「未配置」变成「已开启」，断言随机失败）。统一走这把锁。
     */
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 在锁内跑：进来先清变量，出去一定恢复成「未设置」（不污染别的用例）。
    fn with_push_out_env<F: FnOnce()>(f: F) {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("PUSH_OUT_ENABLED");
        f();
        std::env::remove_var("PUSH_OUT_ENABLED");
    }

    /// 默认（未设 PUSH_OUT_ENABLED）必须**关闭**出网 —— 这是测试阶段的安全默认值，
    /// 谁要是把它改成默认开，本地跑测试就会真发给主播。
    #[test]
    fn push_out_defaults_to_disabled() {
        with_push_out_env(|| {
            assert!(!push_out_enabled(), "未配置时必须关闭出网");
        });
    }

    /// 只有显式 "1"/"true" 才开；其它写法（含 "0"、空串、脏值）一律当关。
    #[test]
    fn push_out_only_enabled_by_explicit_on() {
        with_push_out_env(|| {
            for on in ["1", "true", "TRUE"] {
                std::env::set_var("PUSH_OUT_ENABLED", on);
                assert!(push_out_enabled(), "{on} 应开启");
            }
            for off in ["0", "", "false", "yes", "on"] {
                std::env::set_var("PUSH_OUT_ENABLED", off);
                assert!(!push_out_enabled(), "{off:?} 应保持关闭");
            }
        });
    }

    /// 关闭时 out_blocked 返回 true（= 拦截），开启时放行。
    #[test]
    fn out_blocked_gate() {
        with_push_out_env(|| {
            assert!(out_blocked("text", "浩冬", "测试消息"), "关闭时必须拦截");
            std::env::set_var("PUSH_OUT_ENABLED", "1");
            assert!(!out_blocked("text", "浩冬", "测试消息"), "开启时必须放行");
        });
    }
}
