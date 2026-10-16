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
//!   1. 非 SESSION_EXPIRED 错误走官方节律重试（bot::weixin 的 monitor_failure_delay：
//!      单次 2s、连续 3 次退避 30s），成功一次归零；
//!      抖动种子混入账号名哈希，100+ bot 不会同一秒一起重试。
//!   2. HTTP 连接池参数见 `bot/weixin.rs` 的 `shared_http()`（显式池上限 + 空闲超时）。
//!   3. cursor 只在内存推进，按 60 秒 / 每 20 轮节流落库，退出时强制 flush
//!      （[`CursorBuf`]）。
//!   4. supervisor 自动重启死掉的任务（[`supervisor_loop`]）。

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::bot::weixin::{
    user_poll_updates, user_send_text, SESSION_EXPIRED_MAX_RETRY, SESSION_EXPIRED_RETRY,
};
use crate::state::AppState;
use crate::store::WxPushAccount;

/* ------------------- 轮询失败节律（对齐官方 2.4.10） ------------------- */
/*
   原来这里是自造的指数退避（5s 起步翻倍、60s 封顶 + 按账号名抖动）。
   2026-10-16 用户口径「限制根据官方来」：整体换成官方
   `@tencent-weixin/openclaw-weixin@2.4.10` `dist/src/monitor/monitor.js` 的节律 ——
     · 单次失败等 2s（RETRY_DELAY_MS）
     · 连续 3 次（MAX_CONSECUTIVE_FAILURES）退避 30s（BACKOFF_DELAY_MS），计数清零
     · 成功一轮计数归零
   常量与换算函数的唯一事实来源在 bot::weixin（`RETRY_DELAY` / `MAX_CONSECUTIVE_FAILURES`
   / `BACKOFF_DELAY` / `monitor_failure_delay`），这里直接引用，避免两处漂移。
   官方没有按账号抖动（它一个客户端只跑自己一个账号；我们 100+ 账号会同时失败，
   抖动仍有价值 —— 但官方常量是硬口径，不再加料，靠 START_STAGGER_MAX 的启动错峰兜底）。
*/

use crate::bot::weixin::{monitor_failure_delay, MAX_CONSECUTIVE_FAILURES};

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

/// FNV-1a：账号名 → 稳定哈希（启动错峰用：不同账号摊开在 0~5s，不同秒打 getupdates）
fn name_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
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

/// supervisor 一轮该拉起哪些账号：有凭证、没在跑、且**未被解绑暂停**。
/// 纯函数（不碰全局表），便于单测。
/// paused（2026-10-17 用户口径「解绑 = 停止数据传输」）在 push_bindings 表上，
/// 由 store::wx_push_account_get / wx_push_accounts_all 联表带出：
/// 解绑后账号凭证（wx_push_accounts）保留，但不再补拉长轮询；
/// 重新对接时 store::push_bind_account 把 paused 置回 0，这里自动恢复补拉。
fn accounts_to_start(accounts: &[WxPushAccount], running: &HashSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    for a in accounts {
        let name = a.name.trim();
        if name.is_empty() || a.bot_token.trim().is_empty() {
            continue; // 没凭证（未扫码 / 已失效）的账号不拉
        }
        if a.paused {
            continue; // 解绑暂停（push_bindings.paused=1）的账号不拉，等重新对接恢复
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

/// 单账号轮询的**最小间隔**（用户口径 2026-10-16：正常限制一下一分钟，防服务器占用过高）。
/// 空轮询本来就睡 longpolling_timeout_ms（一圈 ≈ 35s 挂住 + 30s 睡 ≈ 1 次/分钟），
/// 这条下限管的是两头热循环：有消息时循环不睡会连打（实测单次请求仅 ~0.2s），
/// 失败重试 2s 也偏密。任何一轮结束后没到 60s 就在下一轮顶补睡。
/// 100 账号 × 1 次/分钟 ≈ 1.7 请求/秒，服务器毫无压力；首条消息走长轮询即时到达，不受影响。
const POLL_MIN_INTERVAL: Duration = Duration::from_secs(60);

/// 距上一轮开始过了 `elapsed`，下一轮之前还该等多久（纯函数，可单测）
fn poll_floor_wait(elapsed: Duration) -> Duration {
    POLL_MIN_INTERVAL.saturating_sub(elapsed)
}

async fn poll_body(st: &Arc<AppState>, name: &str, cursor: &mut CursorBuf) {
    // 失败节律对齐官方 @tencent-weixin/openclaw-weixin@2.4.10 monitor.js：
    // 单次失败 2s、连续 3 次退避 30s 并清零（原来是自造的 5s→60s 指数退避）。
    // -14（登录过期）仍走下面专门的 1 小时暂停分支，不在这条计数里。
    // 在官方节律之上再叠一条 [`POLL_MIN_INTERVAL`] 下限（用户口径：一分钟一次封顶），
    // 官方没有入站限流概念，这是本服务自己的资源保护。
    let mut consecutive_failures = 0u32;
    let mut expired_streak = 0u32;
    // 首轮不等：last_round 预回拨一个完整间隔
    let mut last_round = Instant::now() - POLL_MIN_INTERVAL;
    loop {
        // 一分钟下限（用户口径）：不管上一轮是空转、有消息还是失败，没到 60s 就补睡。
        // 睡前先落游标（session_stagger 场景下退出也不丢进度）。
        let floor = poll_floor_wait(last_round.elapsed());
        if !floor.is_zero() {
            cursor.flush(st, name).await;
            if !sleep_or_shutdown(floor).await {
                cursor.flush(st, name).await;
                return;
            }
        }
        last_round = Instant::now();
        let Some(acct) = crate::store::wx_push_account_get(st, name).await else {
            tracing::info!(account = %name, "扫码账号已解绑，停止轮询");
            return; // 已解绑 / 被删除
        };
        if acct.paused {
            // 2026-10-17 用户口径「解绑 = 停止数据传输」：paused=1（push_unbind 置位）
            // 时立即退出任务，本账号不再收发任何数据。supervisor 因 paused 过滤不会
            // 再补拉；重新对接（push_bind_account 置回 0）后由 supervisor 自动恢复。
            tracing::info!(account = %name, "账号已解绑暂停，停止轮询");
            return;
        }
        if acct.bot_token.is_empty() {
            tracing::info!(account = %name, "扫码账号无凭证，停止轮询");
            return;
        }
        let cursor_snapshot = cursor.value().to_string();
        match user_poll_updates(&acct.base_url, &acct.bot_token, &cursor_snapshot).await {
            Ok(out) => {
                // 一轮成功：失败与过期计数都归零
                consecutive_failures = 0;
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
                    // 官方节律（monitor.js）：失败 +1；前 2 次 2s，连续第 3 次起 30s
                    //（官方退避后把计数清零，这里同步清零，避免从此固定 30s）
                    consecutive_failures += 1;
                    let delay = monitor_failure_delay(consecutive_failures);
                    if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                        consecutive_failures = 0;
                    }
                    tracing::warn!(account = %name, error = %msg, delay_ms = delay.as_millis() as u64, "扫码账号 getupdates 失败，按官方节律重试");
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
    /* 2026-10-17 审计 4-P2-2：无主行的兼容放行收窄到 legacy_no_uid=1 的存量 ——
    新扫码在确认侧已拒绝 user_id 缺失（wx.rs），不会再产生「双空」行；
    改动前落库的历史无主行仍放行（否则老账号永远激活不了），但记 warn。 */
    if !acct.legacy_no_uid {
        return Some(false);
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

    /* 「是否首次激活」的口径 = **push_bindings 里记的是不是这个会话**。
    不能用 `acct.conversation_id`（wx_push_accounts）判：那张表的列更宽、先写成功，
    一旦 push_bindings 那次写失败（列长不够 / 写库抖动），旧写法会永远判成「已激活」，
    激活分支再也不跑 → 展示端卡在「未绑定」且不自愈（2026-10-16 用户报障根因）。
    现在只要 push_bindings 还没接上这个会话，本人消息就重跑一次激活（幂等 upsert）。 */
    let already_bound =
        crate::store::push_binding_conversation(st, &acct.name).await == conversation;
    // 会话绑定只在本人分支里做：非本人消息不能覆盖 conversation_id 抢占绑定
    // （旧逻辑无条件调用，等于「最后说话的人」都能改写绑定目标）。
    crate::store::wx_push_account_bind_conversation(st, &acct.name, &conversation, &from_user)
        .await;
    // 扫码后的第一条本人消息 = 推送激活：把展示端账号接到该会话，并回执一次
    if !already_bound {
        crate::store::push_bind_account(st, &acct.name, &acct.team, &conversation, &from_user)
            .await;
        // 2026-10-13 用户口径：激活回执先问候「你好，艺名」；艺名空白则兜底只发「你好」
        let greeting = match acct.name.trim() {
            "" => "你好".to_string(),
            n => format!("你好，{n}"),
        };
        let text = format!(
            // 2026-10-15 文案改版：两行式要点列表更像「人发的简报」；明确两类推送
            // 会自动来（旧版「会给你推送」偏被动），开关入口与解绑指令原样保留。
            "{greeting}\n\n✅ 微信推送已绑定\n\n以后数据更新会自动推给你：\n· 追分提醒\n· 每日报告\n\n开关与追分目标在展示页「我的」里设置。回复「解绑」断开推送。"
        );
        if let Err(e) = user_send_text(
            st,
            &acct.base_url,
            &acct.bot_token,
            &from_user,
            &context_token,
            &text,
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
        st,
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
    // 官方节律常量只在测试断言里直接比对（生产路径经 monitor_failure_delay 间接使用）
    use crate::bot::weixin::{BACKOFF_DELAY, RETRY_DELAY};

    fn acct(name: &str, token: &str) -> WxPushAccount {
        WxPushAccount {
            name: name.to_string(),
            bot_token: token.to_string(),
            ..Default::default()
        }
    }

    /* ------------------- 风险 1：失败节律（官方 2.4.10 口径） ------------------- */

    #[test]
    fn failure_delay_follows_official_monitor_rhythm() {
        // 官方 monitor.js：单次失败 2s，连续第 3 次退避 30s
        assert_eq!(RETRY_DELAY, Duration::from_secs(2));
        assert_eq!(MAX_CONSECUTIVE_FAILURES, 3);
        assert_eq!(BACKOFF_DELAY, Duration::from_secs(30));
        assert_eq!(monitor_failure_delay(0), RETRY_DELAY);
        assert_eq!(monitor_failure_delay(1), RETRY_DELAY, "第 1 次失败 → 2s");
        assert_eq!(monitor_failure_delay(2), RETRY_DELAY, "第 2 次失败 → 2s");
        assert_eq!(
            monitor_failure_delay(3),
            BACKOFF_DELAY,
            "连续第 3 次 → 30s（官方退避档）"
        );
        assert_eq!(
            monitor_failure_delay(50),
            BACKOFF_DELAY,
            "再多也是 30s（官方计数清零后的同一档）"
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

    /// 2026-10-17 用户口径「解绑 = 停止数据传输」：paused=1 的账号即使有凭证、
    /// 没在跑，supervisor 也不补拉；其余账号照常。
    #[test]
    fn supervisor_skips_paused_accounts() {
        let mut paused = acct("已解绑", "tok-p");
        paused.paused = true;
        let accounts = vec![paused, acct("正常", "tok-1")];
        let start = accounts_to_start(&accounts, &HashSet::new());
        assert_eq!(start, vec!["正常".to_string()], "paused=1 不补拉，其余照常");
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

    #[test]
    fn poll_floor_caps_one_round_per_minute() {
        // 用户口径（2026-10-16）：单账号一分钟最多一轮。
        assert_eq!(POLL_MIN_INTERVAL, Duration::from_secs(60));
        // 刚跑完一轮 → 补睡满 60s；过了 35s（长轮询挂住）→ 再睡 25s；超时 → 不睡
        assert_eq!(
            poll_floor_wait(Duration::from_secs(0)),
            Duration::from_secs(60)
        );
        assert_eq!(
            poll_floor_wait(Duration::from_secs(35)),
            Duration::from_secs(25)
        );
        assert_eq!(poll_floor_wait(Duration::from_secs(60)), Duration::ZERO);
        assert_eq!(
            poll_floor_wait(Duration::from_secs(3600)),
            Duration::ZERO,
            "超时不回拨（saturating）"
        );
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
        empty.legacy_no_uid = true; // 迁移 035：历史存量无主行才保留兼容放行
        assert_eq!(
            activation_owner_ok(&empty, "whoever", &conv_of("whoever")),
            None
        );
        // 2026-10-17 审计 4-P2-2：新口径下**非 legacy** 的无主行（双空）一律拒绝激活
        // —— 新扫码已改为 user_id 缺失直接拒绝保存，这类行不应再出现；
        // 一旦出现（比如手工插库），绝不能让「第一个说话的人」冒认激活。
        let mut ownerless = acct("新账号", "tok");
        ownerless.ilink_bot_id = bot.into();
        assert_eq!(
            activation_owner_ok(&ownerless, "whoever", &conv_of("whoever")),
            Some(false),
            "无主新行不能被任意首条消息激活"
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
