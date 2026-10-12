//! 用户自助扫码的微信推送通道（2026-10-11，多账号；同日按 100+ bot 扩容加固）。
//!
//! 每个展示端账号在「我的」页自己扫码登录一个 iLink 账号（官方
//! `Tencent/openclaw-weixin` 协议），服务端为每个账号各跑一条 `getupdates`
//! 长轮询，把用户首次发来的消息上下文（to_user_id + context_token）落库；
//! 之后 `push.rs` 用该账号自己的 bot token 主动推送，不依赖管理员机器人。
//!
//! 生命周期：
//!   - 进程启动 [`bootstrap`] 恢复全部账号，并（main.rs）起一条 [`supervisor_loop`]
//!     每 30 秒扫表补拉：有凭证但没在跑的账号会被重新拉起，任务 panic / 退出也算在内。
//!   - 新账号扫码确认后由公开接口调用 [`ensure_account`] 立即拉起。
//!   - 每个账号一条 `tokio::spawn` 任务，句柄不 `.await`，单个 bot panic 不影响其它 bot
//!     （任务内再包一层 `catch_unwind`，panic 也先 flush cursor 再退出等 supervisor 补拉）。
//!   - 收到全局 shutdown token 后各轮询在等待点立即退出并 flush cursor。
//!
//! 断连风险对应的加固（2026-10-11）：
//!   1. 非 SESSION_EXPIRED 错误走指数退避 + 抖动（[`Backoff`]），成功一次归零；
//!      抖动种子混入账号名哈希，100+ bot 不会同一秒一起重试。
//!   2. HTTP 连接池参数见 `bot/weixin.rs` 的 `shared_http()`（显式池上限 + 空闲超时）。
//!   3. cursor 只在内存推进，按 60 秒 / 每 20 轮节流落库，退出时强制 flush
//!      （[`CursorBuf`]）。
//!   4. supervisor 自动重启死掉的任务（[`supervisor_loop`]）。

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::FutureExt;
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::bot::weixin::{
    user_poll_updates, user_send_text, SESSION_EXPIRED_MAX_RETRY, SESSION_EXPIRED_RETRY,
};
use crate::state::AppState;
use crate::store::WxPushAccount;

/* ------------------------------ 退避（风险 1） ------------------------------ */

/// 退避起点：第一次失败等 ~5 秒（抖动后 2.5–5 秒）
const BACKOFF_BASE: Duration = Duration::from_secs(5);
/// 退避上限：再怎么失败也不超过 60 秒，避免账号长时间静默
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// 2 的幂最大位移（5s << 8 已远超上限，纯防溢出）
const BACKOFF_MAX_SHIFT: u32 = 8;

/// 连续失败计数 → 基础退避时长（指数增长，封顶 [`BACKOFF_MAX`]）。
/// failures = 0（没失败过）按起点算。
fn backoff_base_delay(failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(BACKOFF_MAX_SHIFT);
    BACKOFF_BASE.saturating_mul(1u32 << shift).min(BACKOFF_MAX)
}

/// 在基础退避上加抖动：`base * (0.5 + 0.5 * jitter)`，落在 `[base/2, base]`。
/// 只往下抖不往上抖，保证不会突破 [`BACKOFF_MAX`]。
fn backoff_delay(failures: u32, jitter: f64) -> Duration {
    let base = backoff_base_delay(failures);
    let factor = 0.5 + 0.5 * jitter.clamp(0.0, 1.0);
    let ms = base.as_millis() as f64 * factor;
    Duration::from_millis(ms as u64).min(BACKOFF_MAX)
}

/// splitmix64：把种子打散成 [0,1) 的抖动比例（纯函数，可单测）
fn jitter_ratio(seed: u64) -> f64 {
    let mut x = seed;
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    ((x >> 11) as f64) / ((1u64 << 53) as f64)
}

/// FNV-1a：账号名 → 稳定哈希（同一账号每轮抖动位置不同，但不同账号互不相同）
fn name_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 抖动种子 = 账号名哈希 ^ 纳秒时间：账号之间天然错开，同一账号逐轮也不同
fn jitter_seed(name: &str) -> u64 {
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    name_hash(name) ^ ns.wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

/// 连续失败退避状态：成功一次立即归零。
#[derive(Debug, Default)]
struct Backoff {
    failures: u32,
}

impl Backoff {
    /// 记录一次失败并返回本次该等多久
    fn record_failure(&mut self, name: &str) -> Duration {
        self.failures = self.failures.saturating_add(1);
        backoff_delay(self.failures, jitter_ratio(jitter_seed(name)))
    }
    fn reset(&mut self) {
        self.failures = 0;
    }
}

/* --------------------------- cursor 落库节流（风险 3） --------------------------- */

/// 距上次落库多久才允许再写（100+ bot 每条消息写一次会打爆写池）
const CURSOR_FLUSH_INTERVAL: Duration = Duration::from_secs(60);
/// 或者累计多少轮没落库就写一次（消息密集时不至于一直拖着）
const CURSOR_FLUSH_ROUNDS: u32 = 20;

/// 长轮询游标：内存推进，按时间 / 轮数节流落库，进程退出前强制 flush。
/// 丢进度的最坏情况是「硬 kill 丢最近 60 秒」，iLink 侧重放由服务端按 cursor 决定，
/// 且重复投递只会让 handle_msg 幂等重跑一次（会话 upsert + 绑定），不会破坏状态。
struct CursorBuf {
    value: String,
    dirty: bool,
    last_flush: Instant,
    rounds_since_flush: u32,
}

impl CursorBuf {
    fn new(initial: String) -> Self {
        Self {
            value: initial,
            dirty: false,
            last_flush: Instant::now(),
            rounds_since_flush: 0,
        }
    }

    fn value(&self) -> &str {
        &self.value
    }

    /// 服务端返回空游标说明没推进，写空串会把进度打回去（对齐 openclaw）
    fn update(&mut self, next: &str) {
        if !next.is_empty() && next != self.value {
            self.value = next.to_string();
            self.dirty = true;
        }
    }

    /// 一轮成功轮询后调用：累计轮数，到点才返回待落库的游标
    fn take_flush(&mut self) -> Option<String> {
        self.rounds_since_flush = self.rounds_since_flush.saturating_add(1);
        if !self.dirty {
            return None;
        }
        if self.last_flush.elapsed() < CURSOR_FLUSH_INTERVAL
            && self.rounds_since_flush < CURSOR_FLUSH_ROUNDS
        {
            return None;
        }
        self.take_final()
    }

    /// 退出前 / 长睡前强制落库：忽略节流，只要脏就写
    fn take_final(&mut self) -> Option<String> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        self.last_flush = Instant::now();
        self.rounds_since_flush = 0;
        Some(self.value.clone())
    }

    /// 是否有尚未落库的进度（观测 / 单测用）
    #[cfg(test)]
    fn needs_write(&self) -> bool {
        self.dirty
    }

    /// 按节流策略落库（每轮调用）
    async fn maybe_flush(&mut self, st: &Arc<AppState>, name: &str) {
        if let Some(v) = self.take_flush() {
            crate::store::wx_push_account_set_updates_buf(st, name, &v).await;
        }
    }

    /// 强制落库（任务退出前调用）
    async fn flush(&mut self, st: &Arc<AppState>, name: &str) {
        if let Some(v) = self.take_final() {
            crate::store::wx_push_account_set_updates_buf(st, name, &v).await;
        }
    }
}

/* ----------------------------- 任务表（风险 4） ----------------------------- */

/// 一条 bot 轮询任务的句柄。任务结束（返回 / panic）后 `alive()` 为 false，
/// supervisor 下一轮扫到就重新拉起。
struct TaskEntry {
    handle: JoinHandle<()>,
}

impl TaskEntry {
    fn new(handle: JoinHandle<()>) -> Self {
        Self { handle }
    }
    fn alive(&self) -> bool {
        !self.handle.is_finished()
    }
}

static TASKS: OnceLock<Mutex<HashMap<String, TaskEntry>>> = OnceLock::new();

fn tasks() -> &'static Mutex<HashMap<String, TaskEntry>> {
    TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// supervisor 扫描间隔（30 秒：死掉的任务最多 30 秒被拉起）
pub const SUPERVISOR_INTERVAL: Duration = Duration::from_secs(30);
/// 启动错峰上限：100+ bot 同时拉起会同时打 getupdates（雷同群啸）。
/// 每条任务按账号名哈希先等 0~5 秒再进入轮询，把首次请求摊开。
const START_STAGGER_MAX: Duration = Duration::from_secs(5);

/// 某账号启动前该等多久（纯函数，按账号名稳定错开 0 ~ START_STAGGER_MAX）
fn start_stagger(name: &str) -> Duration {
    let max_ms = START_STAGGER_MAX.as_millis() as u64;
    Duration::from_millis(name_hash(name) % max_ms.max(1))
}

/// 清掉已经结束的句柄，返回这些账号名（供调用方记日志 / 决定重启）
fn reap_finished(map: &mut HashMap<String, TaskEntry>) -> Vec<String> {
    let dead: Vec<String> = map
        .iter()
        .filter(|(_, e)| !e.alive())
        .map(|(k, _)| k.clone())
        .collect();
    for k in &dead {
        map.remove(k);
    }
    dead
}

/// supervisor 一轮该拉起哪些账号：有凭证、且当前没有活任务在跑。
/// 纯函数（不碰全局表），便于单测。
fn accounts_to_start(accounts: &[WxPushAccount], running: &HashSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    for a in accounts {
        let name = a.name.trim();
        if name.is_empty() || a.bot_token.trim().is_empty() {
            continue; // 没凭证（未扫码 / 已失效）的账号不拉
        }
        if running.contains(name) {
            continue;
        }
        out.push(name.to_string());
    }
    out
}

/// 进程启动时恢复所有已扫码账号
pub async fn bootstrap(st: &Arc<AppState>) {
    let mut n = 0usize;
    for a in crate::store::wx_push_accounts_all(st).await {
        if !a.bot_token.is_empty() {
            ensure_account(st, &a.name).await;
            n += 1;
        }
    }
    if n > 0 {
        tracing::info!(accounts = n, "wxpush：已恢复扫码账号长轮询");
    }
}

/// 为一个展示端账号拉起长轮询（已在跑则跳过；已结束的句柄会被回收后重新拉起）
pub async fn ensure_account(st: &Arc<AppState>, name: &str) {
    let key = name.trim().to_string();
    if key.is_empty() {
        return;
    }
    {
        let mut map = tasks().lock().expect("wxpush tasks 锁 poisoned");
        for dead in reap_finished(&mut map) {
            tracing::warn!(account = %dead, "wxpush 任务已结束（解绑或异常退出），回收句柄");
        }
        if map.contains_key(&key) {
            return;
        }
    }
    let st2 = Arc::clone(st);
    let key2 = key.clone();
    // 每条 bot 一个独立任务，句柄只存不 await：单个 bot panic / 返回不影响其它 bot
    let handle = tokio::spawn(async move { poll_loop(st2, key2).await });
    tasks()
        .lock()
        .expect("wxpush tasks 锁 poisoned")
        .insert(key, TaskEntry::new(handle));
}

/// supervisor：每 [`SUPERVISOR_INTERVAL`] 扫一遍 `wx_push_accounts`，
/// 发现有凭证但没在跑（首次 / 任务 panic 退出 / 被回收）的账号就拉起。
/// 收到全局 shutdown token 后停止补拉（各轮询任务自己也会退出并 flush）。
pub async fn supervisor_loop(st: Arc<AppState>) {
    let token = crate::push::shutdown_token();
    let mut tick = tokio::time::interval(SUPERVISOR_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = token.cancelled() => {
                tracing::info!("wxpush supervisor 收到退出信号，停止补拉");
                return;
            }
            _ = tick.tick() => {}
        }
        let accounts = crate::store::wx_push_accounts_all(&st).await;
        let running: HashSet<String> = {
            let mut map = tasks().lock().expect("wxpush tasks 锁 poisoned");
            for dead in reap_finished(&mut map) {
                tracing::warn!(account = %dead, "wxpush 任务已退出，supervisor 本轮重新拉起");
            }
            map.keys().cloned().collect()
        };
        // 账号行被删（解绑）的任务：下一轮 get 不到凭证会自己退出，这里不额外 abort，
        // 避免在它 flush cursor 的窗口里被打断。
        for name in accounts_to_start(&accounts, &running) {
            tracing::warn!(account = %name, "wxpush 账号未在跑，supervisor 补拉长轮询");
            ensure_account(&st, &name).await;
        }
    }
}

/// 当前在跑（或已结束待回收）的任务条目数（观测用；锁 poisoned 时返回 0）
pub fn task_count() -> usize {
    tasks().lock().map(|m| m.len()).unwrap_or(0)
}

/* ------------------------------- 单账号轮询 ------------------------------- */

/// 等一段时间；收到退出信号返回 false（调用方应立即收尾退出）
async fn sleep_or_shutdown(d: Duration) -> bool {
    let token = crate::push::shutdown_token();
    tokio::select! {
        _ = tokio::time::sleep(d) => true,
        _ = token.cancelled() => false,
    }
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// 任务入口：包一层 catch_unwind —— panic 也不能丢 cursor，退出后由 supervisor 拉起。
/// 句柄从不 await，因此 panic 不会传播到其它 bot / 调用方。
async fn poll_loop(st: Arc<AppState>, name: String) {
    // 错峰：进程重启时 100+ 个 bot 不要同一秒一起打 getupdates
    let stagger = start_stagger(&name);
    if !stagger.is_zero() && !sleep_or_shutdown(stagger).await {
        return;
    }
    let initial = crate::store::wx_push_account_get(&st, &name)
        .await
        .map(|a| a.updates_buf)
        .unwrap_or_default();
    let mut cursor = CursorBuf::new(initial);
    let outcome = AssertUnwindSafe(poll_body(&st, &name, &mut cursor))
        .catch_unwind()
        .await;
    // 无论正常返回还是 panic，退出前强制落库
    cursor.flush(&st, &name).await;
    if let Err(panic) = outcome {
        let msg = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or("（非字符串 panic）");
        tracing::error!(account = %name, error = %msg, "wxpush 轮询任务 panic，cursor 已落库，等待 supervisor 重新拉起");
    }
}

async fn poll_body(st: &Arc<AppState>, name: &str, cursor: &mut CursorBuf) {
    let mut backoff = Backoff::default();
    let mut expired_streak = 0u32;
    loop {
        let Some(acct) = crate::store::wx_push_account_get(st, name).await else {
            tracing::info!(account = %name, "扫码账号已解绑，停止轮询");
            return; // 已解绑 / 被删除
        };
        if acct.bot_token.is_empty() {
            tracing::info!(account = %name, "扫码账号无凭证，停止轮询");
            return;
        }
        let cursor_snapshot = cursor.value().to_string();
        match user_poll_updates(&acct.base_url, &acct.bot_token, &cursor_snapshot).await {
            Ok(out) => {
                // 一轮成功：退避与过期计数都归零
                backoff.reset();
                expired_streak = 0;
                cursor.update(&str_of(&out, "get_updates_buf"));
                cursor.maybe_flush(st, name).await;
                let timeout_ms = out
                    .get("longpolling_timeout_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(30_000)
                    .min(35_000);
                let msgs = out
                    .get("msgs")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if msgs.is_empty() {
                    if !sleep_or_shutdown(Duration::from_millis(timeout_ms)).await {
                        return;
                    }
                } else {
                    for m in &msgs {
                        handle_msg(st, &acct, m).await;
                    }
                }
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("SESSION_EXPIRED") {
                    expired_streak += 1;
                    if expired_streak >= SESSION_EXPIRED_MAX_RETRY {
                        tracing::warn!(account = %name, "扫码账号登录连续过期，停止轮询；需要重新扫码");
                        return;
                    }
                    tracing::warn!(account = %name, streak = expired_streak, "扫码账号登录过期，一小时后重试");
                    // 睡前先把进度落库（后面要睡一小时）
                    cursor.flush(st, name).await;
                    if !sleep_or_shutdown(SESSION_EXPIRED_RETRY).await {
                        return;
                    }
                } else {
                    let delay = backoff.record_failure(name);
                    tracing::warn!(account = %name, error = %msg, delay = ?delay, "扫码账号 getupdates 失败，退避后重试");
                    if !sleep_or_shutdown(delay).await {
                        cursor.flush(st, name).await;
                        return;
                    }
                }
            }
        }
    }
}

/// 群消息判定（纯函数，2026-10-11 审计整改）：iLink 群消息带非空 `group_id`，
/// 或群会话 id 以 `@chatroom` 结尾。群消息一律不处理——群里任何人都能给 bot 发消息，
/// 一旦参与绑定 / 激活判定就等于把「第一个说话的人」换成「群里第一个说话的人」。
fn is_group_message(group_id: &str, from_user: &str) -> bool {
    !group_id.trim().is_empty() || from_user.ends_with("@chatroom")
}

/// 本人判定（纯函数）：这条消息是否来自「扫码者本人 / 已绑定会话的那个人」。
/// - `Some(true/false)`：可以确定是不是本人；
/// - `None`：历史数据两个字段都空（老账号从未激活过），无法判定——
///   调用方按兼容口径放行激活，但记 warn 日志。
///
/// 为什么严格卡本人（2026-10-11 审计整改）：扫码 confirmed 后「第一个发消息的人」
/// 会被绑定激活——任何人（群友、陌生人）抢先发一句话就能抢占推送绑定。
/// 扫码确认时已把扫码者本人的 userId 存进 `ilink_user_id`，激活必须只认他。
fn activation_owner_ok(acct: &WxPushAccount, from_user: &str, conversation: &str) -> Option<bool> {
    if !acct.ilink_user_id.is_empty() {
        // 新数据：扫码时已存本人 userId，严格比对
        return Some(acct.ilink_user_id == from_user);
    }
    if !acct.conversation_id.is_empty() {
        // 历史已激活账号（无 ilink_user_id）：只认已绑定会话的发送者
        return Some(acct.conversation_id == conversation);
    }
    // 老数据从未激活过：无据可查，交给调用方按兼容口径处理
    None
}

/// 从 iLink 消息里取第一段文本（`item_list` 里 type=1 的文本项）。
/// 图片 / 语音 / 文件等非文本消息返回空串（扫码账号是普通用户，不处理附件导入）。
fn message_text(m: &Value) -> String {
    m.get("item_list")
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find_map(|it| {
                if it.get("type").and_then(Value::as_i64) == Some(1) {
                    it.pointer("/text_item/text")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                } else {
                    None
                }
            })
        })
        .unwrap_or_default()
}

async fn handle_msg(st: &Arc<AppState>, acct: &WxPushAccount, m: &Value) {
    let from_user = str_of(m, "from_user_id");
    if from_user.is_empty() {
        return;
    }
    // 群消息一律忽略：不写会话、不绑定、不路由（2026-10-11 审计整改，见 is_group_message）
    let group_id = str_of(m, "group_id");
    if is_group_message(&group_id, &from_user) {
        return;
    }
    let context_token = str_of(m, "context_token");
    let conversation = format!("wxacct:{}:{}", acct.ilink_bot_id, from_user);
    // 私聊上下文落库：主动推送（追分提醒 / 每日报告）要带 to_user_id + context_token。
    // 非本人也照常 upsert（保持原有行为），但下面的激活 / 路由只认本人。
    crate::store::wx_session_upsert(st, &conversation, &from_user, &context_token).await;

    // —— 本人判定：非本人消息绝不触发绑定 / 激活 / 命令路由 ——
    match activation_owner_ok(acct, &from_user, &conversation) {
        Some(false) => {
            tracing::warn!(
                account = %acct.name,
                "非扫码者本人消息，忽略激活（仅登记会话上下文）"
            );
            return;
        }
        None => {
            // 兼容老数据：ilink_user_id 与 conversation_id 都空，无法证明发送者身份。
            // 放行激活（否则老账号永远激活不了），但留 warn 便于事后审计。
            tracing::warn!(
                account = %acct.name,
                "历史账号缺少 ilink_user_id，无法校验本人，按兼容口径允许激活"
            );
        }
        Some(true) => {}
    }

    let first_message = acct.conversation_id.is_empty();
    // 会话绑定只在本人分支里做：非本人消息不能覆盖 conversation_id 抢占绑定
    // （旧逻辑无条件调用，等于「最后说话的人」都能改写绑定目标）。
    crate::store::wx_push_account_bind_conversation(st, &acct.name, &conversation, &from_user)
        .await;
    // 扫码后的第一条本人消息 = 推送激活：把展示端账号接到该会话，并回执一次
    if first_message {
        crate::store::push_bind_account(st, &acct.name, &acct.team, &conversation, &from_user)
            .await;
        let text = "✅ 微信推送已绑定\n\n以后数据更新后会给你推送：\n· 追分提醒\n· 每日报告\n\n要开哪一项、要追谁，到展示页「我的」里设置。回复「解绑」可以断开。";
        if let Err(e) = user_send_text(
            &acct.base_url,
            &acct.bot_token,
            &from_user,
            &context_token,
            text,
        )
        .await
        {
            tracing::warn!(account = %acct.name, error = %e, "扫码激活回执发送失败");
        } else {
            tracing::info!(account = %acct.name, "扫码推送已激活（首次入站留下会话上下文）");
        }
        return; // 激活消息本身不进命令路由（它只是「打个招呼」）
    }

    // —— 已激活账号的本人后续消息：接入命令路由（2026-10-11 审计整改）——
    // 之前这里什么都不做，「每日报告」「解绑」等命令静默无响应，
    // 与激活回执里「回复『解绑』可以断开」的承诺矛盾。
    // 复用 BotRouter：角色判定（push_bindings 命中 → User）、限流（10 条/分钟/会话）、
    // 「解绑」命令（按 conversation_id 操作 push_bindings）全部走同一份实现。
    let text = message_text(m);
    if text.trim().is_empty() {
        return; // 非文本消息（图 / 语音 / 文件）：扫码账号是普通用户，不处理附件
    }
    let inbound = crate::bot::Inbound {
        channel: "weixin",
        conversation_id: conversation.clone(),
        is_group: false,
        text,
        attachments: Vec::new(),
    };
    let cfg = crate::bot::BotConfig::from_env();
    let Some(out) = crate::bot::BotRouter::handle(st, &cfg, inbound).await else {
        return; // None = 静默语义（未绑定 / 空消息），不发任何回复
    };
    if out.text.trim().is_empty() {
        return;
    }
    if let Err(e) = user_send_text(
        &acct.base_url,
        &acct.bot_token,
        &from_user,
        &context_token,
        &out.text,
    )
    .await
    {
        tracing::warn!(account = %acct.name, error = %e, "扫码账号命令回复发送失败");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct(name: &str, token: &str) -> WxPushAccount {
        WxPushAccount {
            name: name.to_string(),
            bot_token: token.to_string(),
            ..Default::default()
        }
    }

    /* ------------------------------ 风险 1：退避 ------------------------------ */

    #[test]
    fn backoff_grows_exponentially_and_caps_at_max() {
        // 失败次数 → 基础时长：5s → 10s → 20s → 40s → 60s（封顶）
        assert_eq!(backoff_base_delay(0), Duration::from_secs(5));
        assert_eq!(backoff_base_delay(1), Duration::from_secs(5));
        assert_eq!(backoff_base_delay(2), Duration::from_secs(10));
        assert_eq!(backoff_base_delay(3), Duration::from_secs(20));
        assert_eq!(backoff_base_delay(4), Duration::from_secs(40));
        assert_eq!(backoff_base_delay(5), BACKOFF_MAX);
        assert_eq!(backoff_base_delay(50), BACKOFF_MAX, "再多次也不超过 60 秒");
        // 溢出保护：5s << 8 仍在 u64 内且被 min 到上限
        assert_eq!(backoff_base_delay(u32::MAX), BACKOFF_MAX);
    }

    #[test]
    fn backoff_jitter_stays_within_half_base_and_max() {
        // 抖动只往下加：落在 [base/2, base]，永不超上限
        for failures in 0..8u32 {
            let base = backoff_base_delay(failures);
            let lo = backoff_delay(failures, 0.0);
            let hi = backoff_delay(failures, 1.0);
            assert_eq!(lo, Duration::from_millis(base.as_millis() as u64 / 2));
            assert!(hi <= base && lo <= hi, "{lo:?} <= {hi:?} <= {base:?}");
            assert!(hi <= BACKOFF_MAX);
            // 非法 jitter 被 clamp，不会算出负数 / 超大值
            assert_eq!(backoff_delay(failures, -5.0), lo);
            assert_eq!(backoff_delay(failures, 9.0), hi);
        }
    }

    #[test]
    fn backoff_resets_on_success() {
        let mut b = Backoff::default();
        let d1 = b.record_failure("a");
        let d2 = b.record_failure("a");
        assert!(d2 >= Duration::from_millis(backoff_base_delay(2).as_millis() as u64 / 2));
        assert_eq!(b.failures, 2);
        b.reset();
        assert_eq!(b.failures, 0);
        let d3 = b.record_failure("a");
        let half = Duration::from_millis(BACKOFF_BASE.as_millis() as u64 / 2);
        assert!(d3 >= half && d3 <= BACKOFF_BASE, "归零后回到起点: {d3:?}");
        let _ = d1;
    }

    #[test]
    fn jitter_ratio_is_unit_range_and_seed_stable() {
        for seed in [0u64, 1, 42, u64::MAX] {
            let r = jitter_ratio(seed);
            assert!((0.0..1.0).contains(&r), "seed {seed} → {r}");
            assert_eq!(r, jitter_ratio(seed), "同一种子必须稳定");
        }
        // 不同种子要真的散开（100+ bot 才不会同秒重试）
        let mut vals: Vec<f64> = (0..64).map(jitter_ratio).collect();
        vals.sort_by(f64::total_cmp);
        vals.dedup();
        assert!(vals.len() > 60, "抖动值几乎不重复");
        // 账号名参与种子：不同账号的第一轮等待不同
        assert_ne!(name_hash("鹏先生"), name_hash("浩冬"));
        assert_ne!(
            jitter_ratio(name_hash("鹏先生")),
            jitter_ratio(name_hash("浩冬"))
        );
    }

    /* --------------------------- 风险 3：cursor 落库 --------------------------- */

    #[test]
    fn cursor_flush_throttled_by_interval_and_rounds() {
        let mut c = CursorBuf::new(String::new());
        assert!(c.take_flush().is_none(), "没推进过不写库");
        c.update("cur-1");
        assert!(c.needs_write());
        // 刚推进：不到 60 秒、不到 20 轮 → 不写（轮数照常累计）
        assert!(c.take_flush().is_none());
        assert_eq!(c.rounds_since_flush, 2);

        // 累计够轮数 → 写
        let mut c2 = CursorBuf::new(String::new());
        c2.update("cur-2");
        let mut flushed = None;
        for _ in 0..CURSOR_FLUSH_ROUNDS {
            flushed = c2.take_flush();
        }
        assert_eq!(flushed.as_deref(), Some("cur-2"), "满 20 轮触发落库");
        assert!(!c2.needs_write(), "落库后脏标记清零");
        assert_eq!(c2.rounds_since_flush, 0);

        // 或：距上次落库超过 60 秒 → 写
        let mut c3 = CursorBuf::new(String::new());
        c3.update("cur-3");
        c3.last_flush = Instant::now() - CURSOR_FLUSH_INTERVAL - Duration::from_secs(1);
        assert_eq!(c3.take_flush().as_deref(), Some("cur-3"));
    }

    #[test]
    fn cursor_does_not_advance_on_empty_or_same_value() {
        let mut c = CursorBuf::new("cur-0".to_string());
        c.update("");
        assert!(!c.needs_write(), "空游标写库会把进度打回去");
        assert_eq!(c.value(), "cur-0");
        c.update("cur-0");
        assert!(!c.needs_write(), "值没变不必写");
        c.update("cur-1");
        assert!(c.needs_write());
        assert_eq!(c.value(), "cur-1");
    }

    #[test]
    fn cursor_forced_flush_on_exit_ignores_throttle() {
        let mut c = CursorBuf::new(String::new());
        c.update("cur-x");
        // 没到时间 / 轮数，但退出前必须写
        assert_eq!(c.take_final().as_deref(), Some("cur-x"));
        assert!(!c.needs_write());
        assert!(c.take_final().is_none(), "第二次 flush 不重复写");
    }

    /* --------------------------- 风险 4：supervisor --------------------------- */

    #[test]
    fn supervisor_picks_up_missing_accounts_only() {
        let accounts = vec![
            acct("鹏先生", "tok-1"), // 没在跑 → 补拉
            acct("浩冬", "tok-2"),   // 在跑 → 跳过
            acct("未扫码", ""),      // 没凭证 → 跳过
            acct("  ", "tok-3"),     // 空名 → 跳过
        ];
        let running: HashSet<String> = ["浩冬".to_string()].into_iter().collect();
        let start = accounts_to_start(&accounts, &running);
        assert_eq!(start, vec!["鹏先生".to_string()], "只补拉有凭证且没在跑的");
    }

    #[test]
    fn supervisor_picks_up_all_when_nothing_running() {
        let accounts = vec![acct("a", "t"), acct("b", "t")];
        let start = accounts_to_start(&accounts, &HashSet::new());
        assert_eq!(start.len(), 2);
        assert!(start.contains(&"a".to_string()) && start.contains(&"b".to_string()));
    }

    #[test]
    fn start_stagger_spreads_accounts_within_budget() {
        let mut seen: Vec<u128> = Vec::new();
        for i in 0..64u32 {
            let d = start_stagger(&format!("acct-{i}"));
            assert!(d < START_STAGGER_MAX, "{d:?} 必须小于 5 秒");
            assert_eq!(d, start_stagger(&format!("acct-{i}")), "同一账号必须稳定");
            seen.push(d.as_millis());
        }
        seen.sort_unstable();
        seen.dedup();
        assert!(seen.len() > 50, "不同账号的错峰延迟要真的散开");
    }

    /* --------------------- 审计整改（2026-10-11）：群消息 / 本人判定 --------------------- */

    #[test]
    fn group_messages_are_detected() {
        // 带 group_id → 群消息
        assert!(is_group_message("12345@chatroom", "user-a"));
        assert!(is_group_message("  g  ", "user-a"), "group_id 只看是否非空");
        // from_user 本身就是群会话 id（兜底：服务端没给 group_id 的情况）
        assert!(is_group_message("", "12345@chatroom"));
        // 普通私聊
        assert!(!is_group_message("", "wxid_abc123"));
        assert!(
            !is_group_message("  ", "wxid_abc123"),
            "空白 group_id 视为私聊"
        );
    }

    #[test]
    fn activation_owner_requires_scanner_themselves() {
        let bot = "bot-1";
        let conv_of = |u: &str| format!("wxacct:{bot}:{u}");
        // 新数据：ilink_user_id 已存扫码者本人 → 严格比对
        let mut a = acct("鹏先生", "tok");
        a.ilink_bot_id = bot.into();
        a.ilink_user_id = "owner-wxid".into();
        assert_eq!(
            activation_owner_ok(&a, "owner-wxid", &conv_of("owner-wxid")),
            Some(true)
        );
        assert_eq!(
            activation_owner_ok(&a, "stranger", &conv_of("stranger")),
            Some(false),
            "陌生人抢先发消息不能激活"
        );
        // 历史已激活账号（ilink_user_id 空、conversation_id 非空）：只认已绑定会话的发送者
        let mut legacy = acct("浩冬", "tok");
        legacy.ilink_bot_id = bot.into();
        legacy.conversation_id = conv_of("owner-wxid");
        assert_eq!(
            activation_owner_ok(&legacy, "owner-wxid", &conv_of("owner-wxid")),
            Some(true)
        );
        assert_eq!(
            activation_owner_ok(&legacy, "stranger", &conv_of("stranger")),
            Some(false)
        );
        // 老数据从未激活（两个字段都空）：无法判定 → None（调用方放行但记 warn）
        let mut empty = acct("老账号", "tok");
        empty.ilink_bot_id = bot.into();
        assert_eq!(
            activation_owner_ok(&empty, "whoever", &conv_of("whoever")),
            None
        );
    }

    #[test]
    fn message_text_extracts_first_text_item_only() {
        // 正常文本消息
        let m = serde_json::json!({
            "item_list": [
                { "type": 2, "image_item": {} },
                { "type": 1, "text_item": { "text": "每日报告" } }
            ]
        });
        assert_eq!(message_text(&m), "每日报告");
        // 纯图片 / 语音 / 文件 → 空串（不路由）
        for items in [
            serde_json::json!([{ "type": 2, "image_item": {} }]),
            serde_json::json!([{ "type": 3, "voice_item": {} }]),
            serde_json::json!([]),
        ] {
            let m = serde_json::json!({ "item_list": items });
            assert_eq!(message_text(&m), "", "非文本消息不该有文本");
        }
        // 缺 item_list 字段也不 panic
        assert_eq!(message_text(&serde_json::json!({})), "");
    }

    #[tokio::test]
    async fn reap_finished_drops_dead_handles_only() {
        let mut map: HashMap<String, TaskEntry> = HashMap::new();
        // 已结束的任务（等它跑完，句柄自然 is_finished）
        let done = tokio::spawn(async {});
        while !done.is_finished() {
            tokio::task::yield_now().await;
        }
        map.insert("dead".to_string(), TaskEntry::new(done));
        // 还活着的任务（等一个永不就绪的 future，测试结束随 runtime 丢弃）
        let pending = tokio::spawn(async {
            std::future::pending::<()>().await;
        });
        map.insert("alive".to_string(), TaskEntry::new(pending));

        let dead = reap_finished(&mut map);
        assert_eq!(dead, vec!["dead".to_string()], "只回收已结束的");
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("alive"));
        // 再 reap 一次：活任务不会被误回收
        assert!(reap_finished(&mut map).is_empty());
    }
}
