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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use sqlx::Row;

use crate::mysql;
use crate::state::AppState;

/// 绑定码：6 位，码表剔除易混字符（0/O/1/I）
pub const BIND_CODE_LEN: usize = 6;
/// 每人最多几个追分目标（用户口径 2026-10-09：5 个）
pub const MAX_TARGETS: i64 = 5;
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// 是不是一个合法绑定码（防止把正常聊天误判成绑定）
pub fn looks_like_bind_code(s: &str) -> bool {
    s.len() == BIND_CODE_LEN && s.bytes().all(|b| CODE_ALPHABET.contains(&b))
}

/// 随机绑定码（RandomState 每次构造都带 OS 级随机种子，省一个 rand 依赖）
pub fn new_bind_code() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::with_capacity(BIND_CODE_LEN);
    while out.len() < BIND_CODE_LEN {
        let n = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        for b in n.to_le_bytes() {
            out.push(CODE_ALPHABET[(b as usize) % CODE_ALPHABET.len()] as char);
            if out.len() == BIND_CODE_LEN {
                break;
            }
        }
    }
    out
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
    if let Some(a) = crate::store::wx_push_account_get(st, &b.name).await {
        anyhow::ensure!(!a.bot_token.is_empty(), "扫码账号凭证为空，请重新扫码");
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
        return crate::bot::weixin::user_send_text(
            &a.base_url,
            &a.bot_token,
            &to_user,
            &context_token,
            text,
        )
        .await;
    }
    let Some(wx) = wx_global else {
        anyhow::bail!("没有可用的微信推送通道（请先在「我的」页扫码绑定）");
    };
    wx.send_proactive(&b.conversation_id, text).await
}

/* ------------------------------ 主流程 ------------------------------ */

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
                let text = daily_report_text(team, date, &rank_items, b.person_id);
                tracing::info!(account = %b.name, text = %text.replace('\n', " | "), "推送·每日报告");
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
    let sql = "SELECT p.id AS person_id, CAST(COALESCE(SUM(d.wave), 0) AS SIGNED) AS wave \
          FROM person p \
          LEFT JOIN daily_metric d ON d.person_id = p.id \
            AND DATE_FORMAT(d.biz_date, '%Y-%m') = ? \
            AND d.wave_reliable = 1 \
          WHERE p.deleted_at IS NULL AND p.gender = ? \
          GROUP BY p.id";
    let mut map = HashMap::new();
    match mysql::query_with_binds(&st.pools, &st.active_id(), sql, |q| {
        q.bind(month.to_string()).bind(team.to_string())
    })
    .await
    {
        Ok(rows) => {
            for r in &rows {
                let pid = r
                    .try_get::<u64, _>("person_id")
                    .or_else(|_| r.try_get::<i64, _>("person_id").map(|v| v as u64))
                    .unwrap_or(0);
                let w = r
                    .try_get::<i64, _>("wave")
                    .or_else(|_| r.try_get::<u64, _>("wave").map(|v| v as i64))
                    .unwrap_or(0);
                if pid > 0 {
                    map.insert(pid, w);
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
    let sql = "SELECT p.id AS person_id, p.name, CAST(COALESCE(SUM(d.wave), 0) AS SIGNED) AS wave \
         FROM person p \
         JOIN daily_metric d ON d.person_id = p.id \
           AND DATE_FORMAT(d.biz_date, '%Y-%m') = ? \
           AND d.wave_reliable = 1 \
         WHERE p.deleted_at IS NULL AND p.gender = ? AND p.status <> 'left' \
           AND p.hide_in_daily_report = 0 \
         GROUP BY p.id, p.name \
         HAVING wave > 0 \
         ORDER BY wave DESC, p.name ASC";
    match mysql::query_with_binds(&st.pools, &st.active_id(), sql, |q| {
        q.bind(month.to_string()).bind(team.to_string())
    })
    .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                (
                    r.try_get::<u64, _>("person_id")
                        .or_else(|_| r.try_get::<i64, _>("person_id").map(|v| v as u64))
                        .unwrap_or(0),
                    r.try_get::<String, _>("name").unwrap_or_default(),
                    r.try_get::<i64, _>("wave")
                        .or_else(|_| r.try_get::<u64, _>("wave").map(|v| v as i64))
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
    let sql = "SELECT p.id AS person_id, p.name, CAST(COALESCE(SUM(d.wave), 0) AS SIGNED) AS wave \
         FROM person p \
         JOIN daily_metric d ON d.person_id = p.id \
           AND DATE_FORMAT(d.biz_date, '%Y-%m-%d') = ? \
           AND d.wave_reliable = 1 \
         WHERE p.deleted_at IS NULL AND p.gender = ? AND p.status <> 'left' \
           AND p.hide_in_daily_report = 0 \
         GROUP BY p.id, p.name \
         HAVING wave >= ? \
         ORDER BY wave DESC, p.name ASC";
    match mysql::query_with_binds(&st.pools, &st.active_id(), sql, |q| {
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
                    r.try_get::<u64, _>("person_id")
                        .or_else(|_| r.try_get::<i64, _>("person_id").map(|v| v as u64))
                        .unwrap_or(0),
                    r.try_get::<String, _>("name").unwrap_or_default(),
                    r.try_get::<i64, _>("wave")
                        .or_else(|_| r.try_get::<u64, _>("wave").map(|v| v as i64))
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
        _ => (
            rival - my,
            t.target_name.clone(),
            format!("你 {} / TA {}", fmt_wan(my), fmt_wan(rival)),
            fmt_wan((rival - my).max(0)),
        ),
    }
}

/// 每日报告文案：TOP10 + 我的名次
pub(crate) fn daily_report_text(
    team: &str,
    date: &str,
    items: &[(u64, String, i64)],
    me: u64,
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

    #[test]
    fn daily_report_text_marks_my_rank() {
        let items = vec![
            (1u64, "阿甲".to_string(), 1_200_000),
            (2u64, "阿乙".to_string(), 900_000),
            (3u64, "阿丙".to_string(), 100_000),
        ];
        let t = daily_report_text("male", "2026-10-08", &items, 3);
        assert!(t.contains("男团 10-08 音浪榜"), "{t}");
        assert!(t.contains("1. 阿甲 120 万"), "{t}");
        assert!(t.contains("我：第 3 名"), "{t}");

        let t2 = daily_report_text("female", "2026-10-08", &items, 9);
        assert!(t2.contains("今天还没开播"), "{t2}");
    }
}
