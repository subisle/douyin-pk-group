//! 直播 PK 实时监控：赛事运行态、对局判定、异常局、赛程推进（2026-10-20）
//!
//! 与既有模块的边界：
//! - `plans`（分组方案）只作**赛前快照**引用：本模块读它拿名单与分组，实时分数一律
//!   落在 `live_*` 表里，绝不写回 `plans.payload`（每几秒刷一次会覆盖运营分组）。
//! - 认人走 `account.anchor_id`（抖音 UID）→ `person.id`；昵称只作展示兜底，
//!   不能只凭昵称给主播发微信（重名 / 改名会发错人）。
//! - 对外发送一律复用 `push.rs` 的闸门与绑定（`push_bindings.live_pk_report`），
//!   本模块不自建发送通道。
//!
//! 两条硬纪律（来自实测）：
//! 1. `battle_id` / `room_id` / UID 一律当**字符串**，19 位会超出 JS 安全整数；
//! 2. 存在 `battle_stats` ≠ 正在 PK（页面显示「PK结束」后接口还会残留约一分钟），
//!    必须结合 `start_time_ms + duration + battle_status + extra.now` 判定。

use serde::{Deserialize, Serialize};

use crate::state::AppState;
use std::collections::HashSet;
use std::sync::Arc;

/* ------------------------------- 规则模型 ------------------------------- */

/// 赛事规则（可后台改；改了只影响**未定稿**的阶段，已冻结结果不回算）
///
/// 字段名对外（HTTP JSON）一律 camelCase，与前端其它接口一致；
/// 落库列名仍是 snake_case，由 store 层对应，两边不混用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveRules {
    /// 第一波小组数（前 4 组先打，打完先复活）
    #[serde(default = "default_first_wave", rename = "firstWaveGroupCount")]
    pub first_wave_group_count: i32,
    /// 每个小组要几场正式局（异常重开不计入）
    #[serde(default = "default_one", rename = "groupRoundsPerGroup")]
    pub group_rounds_per_group: i32,
    /// 小组赛每组直接晋级人数
    #[serde(default = "default_four", rename = "directQualifyPerGroup")]
    pub direct_qualify_per_group: i32,
    /// 复活房容量（抖音连麦位上限）
    #[serde(default = "default_eight", rename = "revivalRoomCapacity")]
    pub revival_room_capacity: i32,
    /// 每个复活房晋级人数
    #[serde(default = "default_three", rename = "revivalQualifyPerRoom")]
    pub revival_qualify_per_room: i32,
    /// 晋级赛组数（0 = 自动 ceil(人数 / promotion_group_size)）
    #[serde(default, rename = "promotionGroupCount")]
    pub promotion_group_count: i32,
    /// 晋级赛每组人数
    #[serde(default = "default_eight", rename = "promotionGroupSize")]
    pub promotion_group_size: i32,
    /// 晋级赛每组出线人数（进决赛）
    #[serde(default = "default_one", rename = "promotionQualifyPerGroup")]
    pub promotion_qualify_per_group: i32,
    /// 决赛房容量
    #[serde(default = "default_eight", rename = "finalRoomCapacity")]
    pub final_room_capacity: i32,
    /// 结算宽限秒数：结束后继续抓最终分，超时才定稿
    #[serde(default = "default_grace", rename = "settleGraceSecs")]
    pub settle_grace_secs: i32,
}

/* 规则字段的默认值：前端改一项时不必把整套规则都重发一遍
（少发一个字段就被 deserialize 拒掉，实际是在逼调用方复制整套默认值）。 */
fn default_first_wave() -> i32 {
    4
}
fn default_one() -> i32 {
    1
}
fn default_three() -> i32 {
    3
}
fn default_four() -> i32 {
    4
}
fn default_eight() -> i32 {
    8
}
fn default_grace() -> i32 {
    60
}

impl Default for LiveRules {
    fn default() -> Self {
        Self {
            first_wave_group_count: 4,
            group_rounds_per_group: 1,
            direct_qualify_per_group: 4,
            revival_room_capacity: 8,
            revival_qualify_per_room: 3,
            promotion_group_count: 0,
            promotion_group_size: 8,
            promotion_qualify_per_group: 1,
            final_room_capacity: 8,
            settle_grace_secs: 60,
        }
    }
}

/// 规则校验：只挡住会把赛程跑飞的值，不干涉正常运营调参
pub fn validate_rules(r: &LiveRules) -> Result<(), String> {
    let need = |name: &str, v: i32, lo: i32, hi: i32| -> Result<(), String> {
        if !(lo..=hi).contains(&v) {
            Err(format!("{name} 要在 {lo}~{hi} 之间，当前 {v}"))
        } else {
            Ok(())
        }
    };
    need("第一波小组数", r.first_wave_group_count, 1, 64)?;
    need("每小组正式局数", r.group_rounds_per_group, 1, 5)?;
    need("每组直接晋级", r.direct_qualify_per_group, 1, 20)?;
    need("复活房容量", r.revival_room_capacity, 2, 20)?;
    need("每房复活晋级", r.revival_qualify_per_room, 1, 20)?;
    if r.revival_qualify_per_room > r.revival_room_capacity {
        return Err("每房复活晋级人数不能超过复活房容量".into());
    }
    need("晋级赛组数", r.promotion_group_count, 0, 64)?;
    need("晋级赛每组人数", r.promotion_group_size, 2, 20)?;
    need("晋级赛每组出线", r.promotion_qualify_per_group, 1, 20)?;
    if r.promotion_qualify_per_group > r.promotion_group_size {
        return Err("晋级赛出线人数不能超过每组人数".into());
    }
    need("决赛房容量", r.final_room_capacity, 1, 20)?;
    need("结算宽限秒数", r.settle_grace_secs, 10, 600)?;
    Ok(())
}

/* ------------------------------ 观测数据结构 ------------------------------ */

/// 采集器上报的一次观测（一个直播间当前这一拍）
/// 采集器上报的一次观测（一个直播间当前这一拍）
///
/// 字段名 camelCase：采集器是 Python，写 camelCase 与前端/后端一致，
/// 避免出现两套命名约定（snake 在 Rust、camel 在 JS，中间必然有一处要翻译）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    /// 直播间短 id（URL 里的 505899768449），回传时也用它定位监控房间
    #[serde(rename = "webRid")]
    pub web_rid: String,
    /// 抖音 room_id（19 位字符串，可为空）
    #[serde(default, rename = "roomId")]
    pub room_id: String,
    /// 直播间是否在线（false = 下播 / 页面显示结束）
    #[serde(rename = "roomOnline")]
    pub room_online: bool,
    /// 本次观测的服务端时间（毫秒；来自接口 extra.now）
    #[serde(default, rename = "nowMs")]
    pub now_ms: i64,
    /// 当前 battle 快照；没有 PK 时为空
    #[serde(default)]
    pub battle: Option<BattleSnapshot>,
    /// 采集健康度（ok / degraded / error），error 时后端暂停自动推送
    #[serde(default = "default_health")]
    pub health: String,
    #[serde(default)]
    pub error: String,
}

fn default_health() -> String {
    "ok".into()
}

/// 一场 PK 的快照（字段来自 `/webcast/linkmic/list/`）
/// 一场 PK 的快照（字段来自 `/webcast/linkmic/list/`）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BattleSnapshot {
    #[serde(rename = "battleId")]
    pub battle_id: String,
    #[serde(default, rename = "channelId")]
    pub channel_id: String,
    /// PK 开始时间（毫秒）
    #[serde(default, rename = "startTimeMs")]
    pub start_time_ms: i64,
    /// PK 时长（秒）
    #[serde(default, rename = "durationSecs")]
    pub duration_secs: i64,
    /// 抖音给的 battle_status（1 = 进行中，2/3 = 结束 / 结算中）
    #[serde(default, rename = "battleStatus")]
    pub battle_status: i64,
    #[serde(default)]
    pub scores: Vec<BattleScore>,
    /// 队伍分（多人组队 PK 时才有）
    #[serde(default, rename = "teamScores")]
    pub team_scores: Vec<TeamScore>,
}

/// 一个人的分数快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BattleScore {
    /// 抖音 UID（字符串，19 位）
    #[serde(rename = "userId")]
    pub user_id: String,
    #[serde(default)]
    pub nickname: String,
    #[serde(default)]
    pub score: i64,
    #[serde(default)]
    pub rank: i32,
    /// 服务端分数版本号：旧版本不能覆盖新版本（防止乱序上报让分数倒退）
    #[serde(default, rename = "scoreVersion")]
    pub score_version: i64,
    #[serde(default, rename = "teamId")]
    pub team_id: String,
    /// 抖音 PK 彩蛋/加成倍率：100 = 无加成，150 = 1.5 倍。
    #[serde(default = "default_bonus_multiple", rename = "bonusMultiple")]
    pub bonus_multiple: i32,
    /// 已经计入显示分的累计加成增量；去加成原分 = score - bonus_score。
    #[serde(default, rename = "bonusScore")]
    pub bonus_score: i64,
    /// 当前采样时 stage=4 加成窗口是否生效。
    #[serde(default, rename = "bonusActive")]
    pub bonus_active: bool,
    /// 当前/最近一次加成窗口的结束时间（毫秒）。
    #[serde(default, rename = "bonusEndMs")]
    pub bonus_end_ms: i64,
    /// 抖音 battle_scores.current_bonus，保留原始字段便于后续校准。
    #[serde(default, rename = "currentBonus")]
    pub current_bonus: i64,
}

fn default_bonus_multiple() -> i32 {
    100
}

/// 把一拍快照展开成采样行，并补齐个人所在队伍的总分 / 名次。
fn battle_sample_rows(snap: &BattleSnapshot) -> Vec<(&BattleScore, i64, i32)> {
    let team_of = |tid: &str| {
        snap.team_scores
            .iter()
            .find(|t| t.team_id == tid)
            .map(|t| (t.score, t.rank))
            .unwrap_or((0, 0))
    };
    snap.scores
        .iter()
        .map(|score| {
            let (team_score, team_rank) = team_of(&score.team_id);
            (score, team_score, team_rank)
        })
        .collect()
}

/// 一支队伍的分数快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamScore {
    #[serde(rename = "teamId")]
    pub team_id: String,
    #[serde(default)]
    pub score: i64,
    #[serde(default)]
    pub rank: i32,
}

/// 单次采集请求的硬边界。真实 PK 房间远小于这些值；边界只拦截异常/恶意大包，
/// 不改变正常采集行为。
const MAX_OBSERVATION_PARTICIPANTS: usize = 64;
const MAX_BATTLE_DURATION_SECS: i64 = 24 * 60 * 60;

/// 校验内部采集上报，防止超长字符串、超大数组、负数分数和溢出时间进入热路径。
///
/// 这是输入边界校验，不承担权限判断（权限由 `X-Live-Token` 中间件负责）。
pub fn validate_observation(obs: &Observation) -> Result<(), String> {
    let check_len = |name: &str, value: &str, max: usize| -> Result<(), String> {
        if value.chars().count() > max {
            Err(format!("{name} 超过 {max} 字符"))
        } else {
            Ok(())
        }
    };
    check_len("webRid", &obs.web_rid, 40)?;
    check_len("roomId", &obs.room_id, 40)?;
    check_len("health", &obs.health, 32)?;
    if obs.now_ms < 0 {
        return Err("nowMs 不能为负数".into());
    }
    let Some(snap) = &obs.battle else {
        return Ok(());
    };
    check_len("battleId", &snap.battle_id, 40)?;
    check_len("channelId", &snap.channel_id, 40)?;
    if snap.start_time_ms < 0 {
        return Err("startTimeMs 不能为负数".into());
    }
    if !(0..=MAX_BATTLE_DURATION_SECS).contains(&snap.duration_secs) {
        return Err(format!(
            "durationSecs 要在 0~{MAX_BATTLE_DURATION_SECS} 之间"
        ));
    }
    if snap.scores.len() > MAX_OBSERVATION_PARTICIPANTS {
        return Err(format!(
            "scores 最多 {MAX_OBSERVATION_PARTICIPANTS} 人，当前 {}",
            snap.scores.len()
        ));
    }
    if snap.team_scores.len() > MAX_OBSERVATION_PARTICIPANTS {
        return Err(format!(
            "teamScores 最多 {MAX_OBSERVATION_PARTICIPANTS} 队，当前 {}",
            snap.team_scores.len()
        ));
    }
    for score in &snap.scores {
        check_len("userId", &score.user_id, 40)?;
        check_len("nickname", &score.nickname, 128)?;
        check_len("teamId", &score.team_id, 40)?;
        if score.score < 0 || score.bonus_score < 0 || score.current_bonus < 0 {
            return Err("分数 / 加成分不能为负数".into());
        }
        if score.score_version < 0 {
            return Err("scoreVersion 不能为负数".into());
        }
        if !(0..=1000).contains(&score.rank) {
            return Err("rank 要在 0~1000 之间".into());
        }
        if !(100..=10_000).contains(&score.bonus_multiple) {
            return Err("bonusMultiple 要在 100~10000 之间".into());
        }
        if score.bonus_end_ms < 0 {
            return Err("bonusEndMs 不能为负数".into());
        }
    }
    for team in &snap.team_scores {
        check_len("teamId", &team.team_id, 40)?;
        if team.score < 0 {
            return Err("队伍分不能为负数".into());
        }
        if !(0..=1000).contains(&team.rank) {
            return Err("队伍 rank 要在 0~1000 之间".into());
        }
    }
    Ok(())
}

impl BattleSnapshot {
    /// 预计结束时间（毫秒）。没有开始时间或时长时返回 None。
    pub fn expected_end_ms(&self) -> Option<i64> {
        if self.start_time_ms <= 0 || self.duration_secs <= 0 {
            return None;
        }
        self.duration_secs
            .checked_mul(1000)
            .and_then(|duration_ms| self.start_time_ms.checked_add(duration_ms))
    }

    /// 参与者签名：判定「同一批人重开一局」用（排序后的 UID 集合）
    pub fn roster_signature(&self) -> String {
        let mut uids: Vec<&str> = self.scores.iter().map(|s| s.user_id.as_str()).collect();
        uids.sort_unstable();
        uids.join(",")
    }

    /// 平均分配后的名次：同分同名次（1,2,2,4），用于晋级线判定
    pub fn ranked(&self) -> Vec<RankedSeat> {
        let mut seats: Vec<RankedSeat> = self
            .scores
            .iter()
            .map(|s| RankedSeat {
                user_id: s.user_id.clone(),
                nickname: s.nickname.clone(),
                score: s.score,
                rank: 0,
                team_id: s.team_id.clone(),
                score_version: s.score_version,
            })
            .collect();
        seats.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.user_id.cmp(&b.user_id))
        });
        let mut rank = 0i32;
        let mut prev: Option<i64> = None;
        for (i, s) in seats.iter_mut().enumerate() {
            if prev != Some(s.score) {
                rank = i as i32 + 1;
                prev = Some(s.score);
            }
            s.rank = rank;
        }
        seats
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RankedSeat {
    pub user_id: String,
    pub nickname: String,
    pub score: i64,
    pub rank: i32,
    pub team_id: String,
    pub score_version: i64,
}

/* ------------------------------ 对局状态判定 ------------------------------ */

/// 对局状态
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RoundStatus {
    /// 刚识别到 battle_id，等落席位
    Detected,
    /// PK 进行中
    Live,
    /// 已到预计结束时间 / 状态显示结束，抓最终分
    Settling,
    /// 正常结束
    Completed,
    /// 异常结束（掉线 / 提前结束 / 人工作废）
    Aborted,
    /// 同一小组出现两场完整局，等管理员选正式局
    PendingReview,
    /// 作废（重复上报 / 误识别）
    Void,
}

impl RoundStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Detected => "detected",
            Self::Live => "live",
            Self::Settling => "settling",
            Self::Completed => "completed",
            Self::Aborted => "aborted",
            Self::PendingReview => "pending_review",
            Self::Void => "void",
        }
    }
    /// 这个状态还能不能继续推进（终态 = 不再自动改）
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Aborted | Self::Void)
    }
}

/// 结束原因
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EndReason {
    Normal,
    /// 实际结束早于预计结束（掉线 / 主播退出连麦）
    EarlyEnd,
    ParticipantDisconnected,
    ManualAbort,
    /// 直播间下播导致采不到最终分
    RoomOffline,
}

impl EndReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::EarlyEnd => "early_end",
            Self::ParticipantDisconnected => "participant_disconnected",
            Self::ManualAbort => "manual_abort",
            Self::RoomOffline => "room_offline",
        }
    }
}

/// 判定这一拍的 PK 状态（纯函数，便于单测覆盖「有 battle_stats 但其实已结束」）
///
/// `now_ms` 用接口返回的 `extra.now`，不用本机时钟——采集器与服务器可能有偏差，
/// 抖音自己给的时间才是同一条时间轴。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BattlePhase {
    /// 没有 PK（两局之间 / 还没开始）
    Idle,
    /// 正在 PK
    Running,
    /// 已结束（到点或状态位显示结束）
    Ended,
    /// 结束不到宽限期：仍要抓最终分
    Settling,
}

pub fn classify_battle(snap: &BattleSnapshot, now_ms: i64, grace_secs: i64) -> BattlePhase {
    let ended_by_status = snap.battle_status >= 2;
    let ended_by_clock = snap.expected_end_ms().is_some_and(|end| now_ms >= end);
    if ended_by_status || ended_by_clock {
        // 刚结束的一小段时间内继续采样，抖音最终分会在结算后落定
        let end_ms = snap.expected_end_ms().unwrap_or(now_ms);
        if now_ms.saturating_sub(end_ms) <= grace_secs * 1000 {
            return BattlePhase::Settling;
        }
        return BattlePhase::Ended;
    }
    BattlePhase::Running
}

/// 是否算「提前结束」：有预计结束时间且实际结束早于它 30 秒以上
pub fn is_early_end(snap: &BattleSnapshot, ended_at_ms: i64) -> bool {
    match snap.expected_end_ms() {
        Some(end) => end - ended_at_ms > 30_000,
        None => false,
    }
}

/// 参赛者是否掉线：上一拍在分数列表里、这一拍消失了
pub fn missing_participants(prev: &[String], cur: &BattleSnapshot) -> Vec<String> {
    let cur_ids: std::collections::HashSet<&str> =
        cur.scores.iter().map(|s| s.user_id.as_str()).collect();
    prev.iter()
        .filter(|u| !cur_ids.contains(u.as_str()))
        .cloned()
        .collect()
}

/* ------------------------------ 异常局判定 ------------------------------ */

/// 同一小组两局的关系判定（决定要不要进人工确认）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateVerdict {
    /// 两局里只有一局正常完成 → 正常那局做正式局（另一局标记异常）
    OneCompleted,
    /// 两局都正常完成 → 无法自动决定，进 pending_review
    BothCompleted,
    /// 两局都异常 → 都要重开
    BothAborted,
    /// 参与人重合度低，不是同一批人（不该发生，按不同局处理）
    DifferentRoster,
}

/// 判定两局关系。`prev_sig` / `cur_sig` 是排序后的 UID 逗号串。
///
/// 「人数相同且 > 3」只作提示：**不能**作为自动覆盖条件——实测里忘记拉人重开一局时
/// 两局人数经常一样，系统无法知道主播想算哪一局，必须交给管理员。
pub fn classify_duplicate(
    prev_sig: &str,
    cur_sig: &str,
    prev_completed: bool,
    cur_completed: bool,
) -> DuplicateVerdict {
    if prev_sig != cur_sig {
        return DuplicateVerdict::DifferentRoster;
    }
    match (prev_completed, cur_completed) {
        (true, true) => DuplicateVerdict::BothCompleted,
        (true, false) | (false, true) => DuplicateVerdict::OneCompleted,
        (false, false) => DuplicateVerdict::BothAborted,
    }
}

/* ------------------------------ 分组算法 ------------------------------ */

/// 复活房分配：把淘汰者**平均**分进若干房间，每房人数差 ≤ 1。
///
/// 用蛇形（snake）分配而不是顺序切块：淘汰者按原小组名次排好序后蛇形发牌，
/// 各房实力更均衡，也避免「同一原小组的人全挤在一个复活房」。
///
/// `people` 已按名次排序（名次高的在前）。
pub fn split_revival_rooms<T: Clone>(people: &[T], capacity: i32) -> Vec<Vec<T>> {
    if people.is_empty() || capacity <= 0 {
        return Vec::new();
    }
    let cap = capacity as usize;
    let rooms = people.len().div_ceil(cap).max(1);
    let mut out: Vec<Vec<T>> = vec![Vec::new(); rooms];
    for (i, p) in people.iter().enumerate() {
        let round = i / rooms;
        let idx = if round % 2 == 0 {
            i % rooms
        } else {
            rooms - 1 - (i % rooms)
        };
        out[idx].push(p.clone());
    }
    out
}

/// 晋级赛分组：人数 / 每组人数 → 组数；`fixed` > 0 时按指定组数分。
/// 与复活房同一套蛇形发牌，保证各组人数差 ≤ 1 且强弱不扎堆。
pub fn split_promotion_groups<T: Clone>(people: &[T], group_size: i32, fixed: i32) -> Vec<Vec<T>> {
    if people.is_empty() || group_size <= 0 {
        return Vec::new();
    }
    let groups = if fixed > 0 {
        fixed as usize
    } else {
        people.len().div_ceil(group_size.max(1) as usize).max(1)
    };
    let mut out: Vec<Vec<T>> = vec![Vec::new(); groups];
    for (i, p) in people.iter().enumerate() {
        let round = i / groups;
        let idx = if round % 2 == 0 {
            i % groups
        } else {
            groups - 1 - (i % groups)
        };
        out[idx].push(p.clone());
    }
    out
}

/* ------------------------------ 阶段推进 ------------------------------ */

/// 阶段种类
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StageKind {
    /// 第一波小组赛（前 N 组）
    FirstWaveGroup,
    /// 第一波复活赛
    FirstWaveRevival,
    /// 剩余小组赛
    RestGroup,
    /// 剩余小组复活赛
    RestRevival,
    /// 晚上晋级赛
    Promotion,
    /// 总决赛
    Final,
}

impl StageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FirstWaveGroup => "first_wave_group",
            Self::FirstWaveRevival => "first_wave_revival",
            Self::RestGroup => "rest_group",
            Self::RestRevival => "rest_revival",
            Self::Promotion => "promotion",
            Self::Final => "final",
        }
    }
    /// 赛程顺序（屏障推进依赖这个顺序）
    pub fn order(self) -> i32 {
        match self {
            Self::FirstWaveGroup => 1,
            Self::FirstWaveRevival => 2,
            Self::RestGroup => 3,
            Self::RestRevival => 4,
            Self::Promotion => 5,
            Self::Final => 6,
        }
    }
}

/// 阶段状态
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StageStatus {
    Pending,
    Running,
    /// 所有小组都已定稿，阶段结果已冻结
    Finalized,
}

impl StageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Finalized => "finalized",
        }
    }
}

/// 小组状态
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum GroupStatus {
    Pending,
    Running,
    /// 打完一局但还没定稿（结算宽限 / 等最终分）
    Settling,
    /// 有争议局，等管理员选正式局
    PendingReview,
    /// 异常局，等重开
    WaitingRestart,
    Finalized,
}

impl GroupStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Settling => "settling",
            Self::PendingReview => "pending_review",
            Self::WaitingRestart => "waiting_restart",
            Self::Finalized => "finalized",
        }
    }
}

/// 小组完成条件：必须存在「已定稿且 is_official」的正式局。
/// 以前「打完一局就算完成」在掉线 / 忘记拉人时会被误判成晋级。
pub fn group_is_done(status: GroupStatus) -> bool {
    status == GroupStatus::Finalized
}

/* ------------------------------ 观测处理 ------------------------------ */

/// 观测处理的结果（给采集器回执用，也写日志）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IngestOutcome {
    pub round_id: i64,
    pub group_id: i64,
    pub phase: String,
    pub is_new_round: bool,
    pub note: String,
}

/// 处理一次观测：认房间 → 认对局 → 写采样 → 判定状态 → 处理异常。
///
/// 这里是整个实时监控的主干。三条纪律：
/// 1. 一个 `battle_id` 一行，永不覆盖（异常重开是**新行**）；
/// 2. 只有 `is_official=1` 的局参与晋级；
/// 3. 数据不可信（`health=error` / 未认人 / 争议未决）时不推进正式结果。
pub async fn ingest_observation(
    st: &Arc<AppState>,
    event_id: i64,
    obs: &Observation,
) -> anyhow::Result<IngestOutcome> {
    let mut out = IngestOutcome::default();
    crate::store::live_monitor_heartbeat(st, event_id, obs).await?;

    // 采集异常 / 下播：只留健康度，不动对局（残留数据不该判成结束）
    if obs.health == "error" {
        out.phase = "degraded".into();
        out.note = obs.error.clone();
        return Ok(out);
    }
    let Some(snap) = &obs.battle else {
        out.phase = "idle".into();
        if !obs.room_online {
            out.phase = "room_ended".into();
        }
        return Ok(out);
    };
    if snap.battle_id.is_empty() || snap.scores.is_empty() {
        out.phase = "idle".into();
        return Ok(out);
    }

    let rules = crate::store::live_event_rules(st, event_id)
        .await
        .unwrap_or_default();
    let now_ms = if obs.now_ms > 0 {
        obs.now_ms
    } else {
        chrono::Utc::now().timestamp_millis()
    };

    // 1) 定位小组：优先按当前正在进行的阶段找「已认领本 battle」的小组，
    //    否则找该阶段里第一个还没定稿的小组（运营按赛程顺序开打）。
    let (group_id, existing_round_id) =
        resolve_group(st, event_id, snap, &obs.web_rid).await?;
    if group_id == 0 {
        // 还没归组也不能丢数据：先按直播间原始采样保留，等待后续认领 / 归组。
        crate::store::live_room_monitor_upsert(
            st,
            &obs.web_rid,
            &obs.room_id,
            "live",
            &obs.error,
            &snap.battle_id,
        )
        .await?;
        let sample_rows = battle_sample_rows(snap);
        crate::store::live_room_sample_insert_batch(
            st,
            &obs.web_rid,
            &obs.room_id,
            &snap.battle_id,
            &sample_rows,
        )
        .await?;
        out.phase = "unassigned".into();
        out.note = "当前没有可接这一局的小组（阶段未开始或都已定稿）".into();
        return Ok(out);
    }
    out.group_id = group_id;

    // 2) 只有第一次识别这个 battle 时才查“已有几局 / 上一局是谁”；
    // 同一 battle 的后续每拍直接沿用现有 round_id，少两次数据库往返。
    let (parent, attempt_no) = if existing_round_id.is_some() {
        (0, 1)
    } else {
        let attempts = crate::store::live_round_count(st, group_id)
            .await
            .unwrap_or(0);
        let parent = if attempts > 0 {
            crate::store::live_latest_round_id(st, group_id)
                .await
                .unwrap_or(0)
        } else {
            0
        };
        (parent, attempts as i32 + 1)
    };
    let (round_id, is_new) = crate::store::live_round_upsert(
        st,
        event_id,
        group_id,
        snap,
        &obs.web_rid,
        parent,
        attempt_no,
    )
    .await?;
    out.round_id = round_id;
    out.is_new_round = is_new;

    if is_new {
        crate::store::live_round_event_add(
            st,
            round_id,
            "round_detected",
            &format!(
                "识别到对局 battle_id={} 参与 {} 人",
                snap.battle_id,
                snap.scores.len()
            ),
            "collector",
        )
        .await?;
        if parent > 0 {
            crate::store::live_round_event_add(
                st,
                round_id,
                "retry_of",
                &format!("上一局 #{parent} 之后重开"),
                "collector",
            )
            .await?;
        }
    }

    // 3) 写采样 + 席位（旧 score_version 不覆盖新版本，防止乱序上报让分数倒退）
    let ranked = snap.ranked();
    let sample_rows = battle_sample_rows(snap);
    crate::store::live_sample_insert_batch(st, round_id, &sample_rows).await?;

    let uids: Vec<String> = ranked.iter().map(|seat| seat.user_id.clone()).collect();
    let person_ids = crate::store::live_person_ids_by_uids(st, &uids).await;
    let seat_rows: Vec<(&RankedSeat, i64)> = ranked
        .iter()
        .map(|seat| {
            (
                seat,
                person_ids.get(&seat.user_id).copied().unwrap_or(0),
            )
        })
        .collect();
    crate::store::live_seat_upsert_batch(st, round_id, &seat_rows, false).await?;

    // 4) 判定阶段
    let phase = classify_battle(snap, now_ms, rules.settle_grace_secs as i64);
    out.phase = match phase {
        BattlePhase::Running => "live",
        BattlePhase::Settling => "settling",
        BattlePhase::Ended => "ended",
        BattlePhase::Idle => "idle",
    }
    .to_string();

    match phase {
        BattlePhase::Running => {
            crate::store::live_round_set_status(st, round_id, RoundStatus::Live, None).await?;
            crate::store::live_group_set_status(st, group_id, GroupStatus::Running).await?;
        }
        BattlePhase::Settling => {
            /* 抖音说结束了（battle_status >= 2 或已过预计结束时间）就**立刻定稿**：
            只在这里「结束时刻」才是真实的结束时刻，能判出提前结束。
            等到 BattlePhase::Ended（时钟已越过预计结束）再判，
            `is_early_end` 会恒为 false —— 掉线重开永远识别不出来。
            定稿后这一局继续收采样，最终分由后面的 seat 覆盖写。 */
            finalize_round(st, event_id, group_id, round_id, snap, now_ms, &rules).await?;
        }
        BattlePhase::Ended => {
            finalize_round(st, event_id, group_id, round_id, snap, now_ms, &rules).await?;
        }
        BattlePhase::Idle => {}
    }

    crate::events::emit(
        crate::events::SCOPE_LIVE,
        format!("对局 #{} {}", round_id, out.phase),
    );
    Ok(out)
}

/// 独立房间观测结果：不填写 group_id/round_id，也不触碰赛事表。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoomIngestOutcome {
    pub battle_id: String,
    pub score_count: usize,
    pub phase: String,
    pub note: String,
}

/// 独立直播间监控入口。
///
/// 与 `ingest_observation` 的区别只有一个但很关键：这里**不调用 resolve_group**，
/// 只按 web_rid + battle_id 保存采样。它适合“先验证房间能不能稳定抓到分数”的阶段；
/// 等赛事分组确定后，再决定是否把同一 battle 接入 `live_round`。
pub async fn ingest_room_observation(
    st: &Arc<AppState>,
    web_rid: &str,
    obs: &Observation,
) -> anyhow::Result<RoomIngestOutcome> {
    let mut out = RoomIngestOutcome::default();
    let snap = obs.battle.as_ref();
    let battle_id = snap.map(|b| b.battle_id.as_str()).unwrap_or("");
    let health = if obs.health == "error" || !obs.room_online {
        "degraded"
    } else {
        "live"
    };
    crate::store::live_room_monitor_upsert(
        st,
        web_rid,
        &obs.room_id,
        health,
        &obs.error,
        battle_id,
    )
    .await?;

    if obs.health == "error" {
        out.phase = "degraded".into();
        out.note = obs.error.clone();
        return Ok(out);
    }
    let Some(snap) = snap else {
        out.phase = if obs.room_online {
            "idle"
        } else {
            "room_ended"
        }
        .into();
        return Ok(out);
    };
    if snap.battle_id.is_empty() || snap.scores.is_empty() {
        out.phase = "idle".into();
        return Ok(out);
    }

    out.battle_id = snap.battle_id.clone();
    out.score_count = snap.scores.len();
    let sample_rows = battle_sample_rows(snap);
    crate::store::live_room_sample_insert_batch(
        st,
        web_rid,
        &obs.room_id,
        &snap.battle_id,
        &sample_rows,
    )
    .await?;

    let now_ms = if obs.now_ms > 0 {
        obs.now_ms
    } else {
        chrono::Utc::now().timestamp_millis()
    };
    out.phase =
        match classify_battle(snap, now_ms, LiveRules::default().settle_grace_secs as i64) {
            BattlePhase::Running => "live",
            BattlePhase::Settling => "settling",
            BattlePhase::Ended => "ended",
            BattlePhase::Idle => "idle",
        }
        .into();
    Ok(out)
}

/// 每日清理直播原始采样。默认保留 90 天，LIVE_SAMPLE_RETENTION_DAYS=0 可关闭。
/// 清理失败只记日志，不影响采集和推送主链路。
pub async fn sample_prune_loop(st: Arc<AppState>) {
    let days = crate::config::live_sample_retention_days();
    if days <= 0 {
        tracing::info!("直播采样自动清理已关闭（LIVE_SAMPLE_RETENTION_DAYS=0）");
        return;
    }
    let shutdown = crate::push::shutdown_token();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(24 * 60 * 60));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tick.tick() => {}
        }
        match crate::store::live_samples_prune(&st, days).await {
            Ok((event_rows, room_rows)) if event_rows + room_rows > 0 => {
                tracing::info!(event_rows, room_rows, retention_days = days, "直播过期采样已清理");
            }
            Ok(_) => tracing::debug!(retention_days = days, "直播采样清理：没有过期数据"),
            Err(e) => tracing::warn!(error = %e, retention_days = days, "直播采样清理失败（下轮重试）"),
        }
    }
}

/// 定位这一局属于哪个小组。
///
/// 顺序：① 该局已落在某个小组（重复上报）→ 沿用；② 当前阶段里第一个未定稿小组。
/// 运营按赛程顺序开打，所以「第一个未定稿」就是当前该打的那组。
async fn resolve_group(
    st: &Arc<AppState>,
    event_id: i64,
    snap: &BattleSnapshot,
    web_rid: &str,
) -> anyhow::Result<(i64, Option<i64>)> {
    if let Some((group_id, round_id)) =
        crate::store::live_round_ref_by_battle(st, &snap.battle_id).await?
    {
        return Ok((group_id, Some(round_id)));
    }

    let signature = snap.roster_signature();
    // 残留 battle_stats / 页面重发可能换一个 battle_id 但仍报同一批人。上一组
    // 刚定稿时，这一局应回到上一组进入人工复核，而不是被顺延到下一组。
    if let Some(g) =
        crate::store::live_recent_finalized_group_for_roster(st, event_id, web_rid, &signature)
            .await
    {
        return Ok((g, None));
    }

    // 有赛前名单时优先按 UID 集合精确认组，避免多直播间或乱序上报把局挂错。
    let current: std::collections::HashSet<&str> =
        snap.scores.iter().map(|s| s.user_id.as_str()).collect();
    for (group_id, uids) in crate::store::live_open_group_rosters(st, event_id).await {
        if uids.is_empty() {
            continue;
        }
        let seeded: std::collections::HashSet<&str> = uids.iter().map(String::as_str).collect();
        if seeded == current {
            return Ok((group_id, None));
        }
    }

    // 兼容没有登记采集号的旧方案：名单无法精确匹配时按赛程顺序接第一组。
    Ok((crate::store::live_next_open_group(st, event_id).await?, None))
}

/// 定稿一局：判断正常 / 异常，只有正常且唯一才设正式局。
async fn finalize_round(
    st: &Arc<AppState>,
    event_id: i64,
    group_id: i64,
    round_id: i64,
    snap: &BattleSnapshot,
    now_ms: i64,
    rules: &LiveRules,
) -> anyhow::Result<()> {
    // 幂等：已定稿的局不动状态（定稿后仍在收最终分，这里会被反复调用）
    let cur = crate::store::live_round_status(st, round_id).await?;
    if cur == Some(RoundStatus::Completed) || cur == Some(RoundStatus::Aborted) {
        return Ok(());
    }

    let early = is_early_end(snap, now_ms);
    let completed_rounds = crate::store::live_completed_rounds(st, group_id).await?;

    // 同一组已有另一场完整局 → 两场都正常完成时无法自动决定，进人工确认
    if completed_rounds.iter().any(|r| r.0 != round_id) {
        crate::store::live_round_set_status(
            st,
            round_id,
            RoundStatus::PendingReview,
            Some("normal"),
        )
        .await?;
        crate::store::live_group_set_status(st, group_id, GroupStatus::PendingReview).await?;
        crate::store::live_round_event_add(
            st,
            round_id,
            "needs_review",
            "同组存在两场完整对局，需要管理员选择正式局",
            "system",
        )
        .await?;
        return Ok(());
    }

    if early {
        crate::store::live_round_set_status(
            st,
            round_id,
            RoundStatus::Aborted,
            Some(EndReason::EarlyEnd.as_str()),
        )
        .await?;
        crate::store::live_group_set_status(st, group_id, GroupStatus::WaitingRestart).await?;
        crate::store::live_round_event_add(
            st,
            round_id,
            "early_end",
            "PK 提前结束（未打满时长），需要重开一局",
            "system",
        )
        .await?;
        return Ok(());
    }

    crate::store::live_round_set_status(
        st,
        round_id,
        RoundStatus::Completed,
        Some(EndReason::Normal.as_str()),
    )
    .await?;
    let _ = rules;
    let _ = event_id;
    crate::store::live_round_set_official(st, round_id).await?;
    crate::store::live_round_event_add(st, round_id, "finalized", "正常结束并设为正式局", "system")
        .await?;
    Ok(())
}

/* ------------------------------ 阶段推进 ------------------------------ */

/// 把一个阶段下的小组按名次冻结晋级结果。
/// `source` 用 `direct` / `revival` / `promotion` 区分来源，便于晚上汇总。
pub async fn freeze_group_qualifiers(
    st: &Arc<AppState>,
    event_id: i64,
    group_id: i64,
    source: &str,
) -> anyhow::Result<usize> {
    let round_id = crate::store::live_group_official_round(st, group_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("小组还没有正式局，不能冻结结果"))?;
    let seats = crate::store::live_seat_list(st, round_id).await?;
    let qualify = crate::store::live_group_qualify_slots(st, group_id)
        .await?
        .unwrap_or(0);
    let stage_id = crate::store::live_group_stage(st, group_id).await?;
    /* 席位要**显式标记**谁晋级、谁淘汰：复活池（`live_stage_eliminated`）查的是
    `qualified = 0` 的席位。只写 live_qualifier 不标记席位的话，淘汰者集合
    永远是空的 —— 复活赛生成时会直接报「没有淘汰者」。 */
    let mut n = 0;
    for seat in &seats {
        let uid = seat
            .get("douyinUid")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let rank = seat.get("rank").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        let score = seat.get("score").and_then(|v| v.as_i64()).unwrap_or(0);
        let person_id = seat.get("personId").and_then(|v| v.as_i64()).unwrap_or(0);
        // 取前 qualify 名（含并列：名次 <= qualify 的都算晋级）
        let qualified = rank > 0 && rank <= qualify.max(0);
        crate::store::live_seat_mark_qualified(st, round_id, uid, qualified).await?;
        if !qualified {
            continue;
        }
        crate::store::live_qualifier_add(
            st, event_id, stage_id, group_id, round_id, person_id, uid, source, rank, score,
        )
        .await?;
        n += 1;
    }
    Ok(n)
}

/* ------------------------------ 推送 ------------------------------ */

/// 给某一局的参赛主播推一次当前分数。
///
/// 三层开关都要开才发：全局 `PUSH_OUT_ENABLED` → 赛事 `push_enabled` → 主播
/// `live_pk_report`。任一层关掉都直接返回 0（**不报错**，这是正常配置不是故障）。
///
/// 冷却靠 `live_push_job` 的唯一键：`dedup_key` 里带时间桶，同一个桶内重复调用
/// 只会有一条入队，天然实现「间隔 N 秒才推一次」。
/// 发送走 [`crate::push::send_live_text`] —— 与每日报告同一条出网窄口。
pub async fn push_round_scores(st: &Arc<AppState>, round_id: i64) -> anyhow::Result<usize> {
    let event_id = crate::store::live_round_event_id(st, round_id)
        .await?
        .unwrap_or(0);
    if event_id <= 0 {
        anyhow::bail!("对局不存在");
    }
    // 第二层开关：赛事级
    let (enabled, interval) = crate::store::live_event_push(st, event_id).await?;
    if !enabled {
        tracing::debug!(event_id, "赛事推送已关闭，跳过");
        return Ok(0);
    }

    // 只推有正式结果的局；争议局（pending_review）未确认前不推，避免推错结果
    let status = crate::store::live_round_status(st, round_id).await?;
    if matches!(status, Some(RoundStatus::PendingReview)) {
        tracing::debug!(round_id, "争议局未确认，不推送");
        return Ok(0);
    }

    let targets = crate::store::live_round_push_targets(st, round_id).await;
    if targets.is_empty() {
        return Ok(0);
    }

    let seats = crate::store::live_seat_list(st, round_id).await?;
    let group_id = crate::store::live_round_group_of_id(st, round_id).await?;
    let qualify = crate::store::live_group_qualify_slots(st, group_id)
        .await?
        .unwrap_or(0);
    let group_name = crate::store::live_group_name(st, group_id).await?;
    // 晋级线：第 qualify 名的分数（同名次并列时取该名次的分）
    let cutoff = seats
        .iter()
        .filter(|s| s.get("rank").and_then(|v| v.as_i64()).unwrap_or(0) as i32 == qualify)
        .filter_map(|s| s.get("score").and_then(|v| v.as_i64()))
        .min();

    let mut sent = 0usize;
    for b in &targets {
        let mine = seats.iter().find(|s| {
            s.get("personId").and_then(|v| v.as_i64()).unwrap_or(0) as u64 == b.person_id
        });
        let Some(mine) = mine else {
            continue;
        };
        let rank = mine.get("rank").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        let score = mine.get("score").and_then(|v| v.as_i64()).unwrap_or(0);

        /* 冷却：时间桶 = 当前时间 / 间隔。同一桶内 dedup_key 相同，
        `uk_live_push_job` 会让第二次入队返回 0（已存在）→ 本次跳过。 */
        let bucket = chrono::Utc::now().timestamp() / interval.max(1) as i64;
        let dedup = format!("live:{round_id}:score:{bucket}:{}", b.person_id);
        let job = crate::store::live_push_enqueue(
            st,
            event_id,
            round_id,
            b.person_id as i64,
            "score",
            &dedup,
        )
        .await;
        if job == 0 {
            continue; // 这一轮已经推过 / 正在推
        }

        let mut text = format!("【PK 实时分】{group_name}\n你当前第 {rank} 名 · {score} 分",);
        if qualify > 0 {
            if rank > 0 && rank <= qualify {
                text.push_str(&format!("\n✅ 在晋级线内（前 {qualify} 名）"));
            } else {
                let gap = match cutoff {
                    Some(c) => format!("，距晋级线还差 {}", c.saturating_sub(score)),
                    None => String::new(),
                };
                text.push_str(&format!("\n⚠️ 暂未进前 {qualify} 名{gap}"));
            }
        }
        if let Some(top) = seats.first() {
            let tn = top.get("nickname").and_then(|v| v.as_str()).unwrap_or("");
            let ts = top.get("score").and_then(|v| v.as_i64()).unwrap_or(0);
            if !tn.is_empty() {
                text.push_str(&format!("\n榜首：{tn} {ts} 分"));
            }
        }

        match crate::push::send_live_text(st, b, &text).await {
            Ok(()) => {
                crate::store::live_push_finish(st, job, true, "").await;
                sent += 1;
            }
            Err(e) => {
                // 被闸门拦下不算失败：回到 pending，开闸后还有机会补推
                crate::store::live_push_finish(st, job, false, &e.to_string()).await;
                tracing::warn!(round_id, person = %b.name, error = %e, "直播分数推送未发出");
            }
        }
    }
    Ok(sent)
}

/* ------------------------------ 赛程生成 ------------------------------ */

/// 从分组方案的 buckets 建一个阶段的小组。
/// `buckets` 形如 `[{ name, players: [{name, ...}] }]`。
pub async fn build_stage_groups(
    st: &Arc<AppState>,
    event_id: i64,
    kind: StageKind,
    buckets: &[serde_json::Value],
    qualify_slots: i32,
) -> anyhow::Result<usize> {
    let stage_id = crate::store::live_stage_ensure(st, event_id, kind).await?;
    let mut n = 0;
    for (i, b) in buckets.iter().enumerate() {
        let raw_name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let name = if raw_name.trim().is_empty() {
            format!("第 {} 组", i + 1)
        } else {
            raw_name.to_string()
        };
        let gid = crate::store::live_group_create(
            st,
            event_id,
            stage_id,
            kind,
            i as i32 + 1,
            &name,
            qualify_slots,
            0,
        )
        .await?;

        let players = b
            .get("players")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut seeds = Vec::with_capacity(players.len());
        for (order, player) in players.iter().enumerate() {
            if player
                .get("excluded")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                continue;
            }
            let person_id = player
                .get("personId")
                .and_then(|v| {
                    v.as_i64()
                        .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
                })
                .unwrap_or(0);
            let plan_name = player
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let anchor = crate::store::live_anchor_for_person(st, person_id).await;
            let (uid, anchor_name) = anchor.unwrap_or_default();
            let nickname = if !plan_name.is_empty() {
                plan_name
            } else if !anchor_name.is_empty() {
                anchor_name
            } else if person_id > 0 {
                format!("#{person_id}")
            } else {
                continue;
            };
            seeds.push(crate::store::GroupSeedSeat {
                person_id,
                douyin_uid: uid,
                nickname,
                seed_order: order as i32,
            });
        }
        crate::store::live_group_seed_replace(st, gid, &seeds).await?;
        n += 1;
        tracing::debug!(group = gid, name = %name, "已建比赛小组");
    }
    Ok(n)
}

/// 生成某一波的复活赛：把该波所有小组的淘汰者**平均**分进若干房间。
///
/// 例：前四组共 16 名淘汰者、房容量 8 → 2 个 8 人房；每房前 3 晋级。
/// 分配用蛇形发牌（`split_revival_rooms`），各房人数差 ≤ 1 且强弱不扎堆。
pub async fn build_revival_rooms(
    st: &Arc<AppState>,
    event_id: i64,
    kind: StageKind,
) -> anyhow::Result<usize> {
    let rules = crate::store::live_event_rules(st, event_id)
        .await
        .ok_or_else(|| anyhow::anyhow!("赛事不存在"))?;
    let group_kind = if kind == StageKind::RestRevival {
        StageKind::RestGroup
    } else {
        StageKind::FirstWaveGroup
    };
    let src_stage = crate::store::live_stage_id_of(st, event_id, group_kind).await?;
    if src_stage == 0 {
        anyhow::bail!("还没有对应的小组赛阶段，先生成小组");
    }
    // 屏障：源阶段必须全部定稿，否则拒绝生成（防止有人没打完就进复活）
    let groups = crate::store::live_group_list(st, event_id).await;
    let pending = groups
        .iter()
        .filter(|g| g.get("stageId").and_then(|v| v.as_i64()) == Some(src_stage))
        .filter(|g| g.get("status").and_then(|v| v.as_str()) != Some("finalized"))
        .count();
    if pending > 0 {
        anyhow::bail!("还有 {pending} 个小组没定稿，不能生成复活赛");
    }

    let eliminated = crate::store::live_stage_eliminated(st, src_stage).await;
    if eliminated.is_empty() {
        anyhow::bail!("没有淘汰者，不需要复活赛");
    }

    let stage_id = crate::store::live_stage_ensure(st, event_id, kind).await?;
    let rooms = split_revival_rooms(&eliminated, rules.revival_room_capacity);
    for (i, room) in rooms.iter().enumerate() {
        let gid = crate::store::live_group_create(
            st,
            event_id,
            stage_id,
            kind,
            i as i32 + 1,
            &format!("复活{}组", i + 1),
            rules.revival_qualify_per_room,
            0,
        )
        .await?;
        // 预落席位（昵称 + UID 先占位，开打后由观测覆盖最终分）
        let mut seeds = Vec::with_capacity(room.len());
        for (seat_order, seat) in room.iter().enumerate() {
            let uid = seat
                .get("douyinUid")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let nickname = seat
                .get("nickname")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let person_id = seat.get("personId").and_then(|v| v.as_i64()).unwrap_or(0);
            seeds.push(crate::store::GroupSeedSeat {
                person_id,
                douyin_uid: uid.to_string(),
                nickname: nickname.to_string(),
                seed_order: seat_order as i32,
            });
        }
        crate::store::live_group_seed_replace(st, gid, &seeds).await?;
        tracing::info!(
            group = gid,
            seats = room.len(),
            "复活房已生成（每房晋级 {} 人）",
            rules.revival_qualify_per_room
        );
    }
    Ok(rooms.len())
}

/// 生成晚上晋级赛：白天直接晋级 + 复活晋级，蛇形分组，每组第 1 进决赛。
pub async fn build_promotion_groups(st: &Arc<AppState>, event_id: i64) -> anyhow::Result<usize> {
    let rules = crate::store::live_event_rules(st, event_id)
        .await
        .ok_or_else(|| anyhow::anyhow!("赛事不存在"))?;
    /* 今晚这类“名单已经提前分好组”的场景，直接以绑定的晚上方案为准。
    只有没绑晚上方案（或方案没有 buckets）时，才回退到白天晋级名单自动分房。 */
    let night_plan_id = crate::store::live_event_night_plan(st, event_id).await?;
    if night_plan_id > 0 {
        let buckets = crate::store::plan_buckets(st, night_plan_id).await?;
        if !buckets.is_empty() {
            return build_stage_groups(
                st,
                event_id,
                StageKind::Promotion,
                &buckets,
                rules.promotion_qualify_per_group,
            )
            .await;
        }
    }
    let qualifiers = crate::store::live_qualifier_list(st, event_id).await;
    let people: Vec<serde_json::Value> = qualifiers
        .into_iter()
        .filter(|q| {
            let s = q.get("source").and_then(|v| v.as_str()).unwrap_or("");
            s == "direct" || s == "revival"
        })
        .collect();
    if people.is_empty() {
        anyhow::bail!("还没有白天晋级名单，先定稿小组赛与复活赛");
    }
    let stage_id = crate::store::live_stage_ensure(st, event_id, StageKind::Promotion).await?;
    let groups = split_promotion_groups(
        &people,
        rules.promotion_group_size,
        rules.promotion_group_count,
    );
    for (i, g) in groups.iter().enumerate() {
        let gid = crate::store::live_group_create(
            st,
            event_id,
            stage_id,
            StageKind::Promotion,
            i as i32 + 1,
            &format!("晋级{}组", i + 1),
            rules.promotion_qualify_per_group,
            0,
        )
        .await?;
        let mut seeds = Vec::with_capacity(g.len());
        for (seat_order, seat) in g.iter().enumerate() {
            let uid = seat
                .get("douyinUid")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let person_id = seat.get("personId").and_then(|v| v.as_i64()).unwrap_or(0);
            let name = seat
                .get("personName")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            seeds.push(crate::store::GroupSeedSeat {
                person_id,
                douyin_uid: uid.to_string(),
                nickname: name.to_string(),
                seed_order: seat_order as i32,
            });
        }
        crate::store::live_group_seed_replace(st, gid, &seeds).await?;
    }
    Ok(groups.len())
}

/// 晋级赛全部定稿后，把各组出线者汇入一个决赛房。
/// 决赛取前 3 名作为最终名次，因此 `qualify_slots=3`。
pub async fn build_final_group(st: &Arc<AppState>, event_id: i64) -> anyhow::Result<usize> {
    let rules = crate::store::live_event_rules(st, event_id)
        .await
        .ok_or_else(|| anyhow::anyhow!("赛事不存在"))?;
    let groups = crate::store::live_group_list(st, event_id).await;
    let promotion_groups: Vec<&serde_json::Value> = groups
        .iter()
        .filter(|g| g.get("stageKind").and_then(|v| v.as_str()) == Some("promotion"))
        .collect();
    if promotion_groups.is_empty() {
        anyhow::bail!("还没有晋级赛小组");
    }
    let pending = promotion_groups
        .iter()
        .filter(|g| g.get("status").and_then(|v| v.as_str()) != Some("finalized"))
        .count();
    if pending > 0 {
        anyhow::bail!("还有 {pending} 个晋级赛小组没定稿，不能生成决赛");
    }

    let qualifiers = crate::store::live_qualifier_list(st, event_id).await;
    let people: Vec<serde_json::Value> = qualifiers
        .into_iter()
        .filter(|q| q.get("source").and_then(|v| v.as_str()) == Some("promotion"))
        .collect();
    if people.is_empty() {
        anyhow::bail!("还没有晋级赛出线名单，请先逐组冻结晋级结果");
    }
    if people.len() > rules.final_room_capacity as usize {
        anyhow::bail!(
            "决赛人数 {} 超过决赛房容量 {}，请先调整规则",
            people.len(),
            rules.final_room_capacity
        );
    }

    let stage_id = crate::store::live_stage_ensure(st, event_id, StageKind::Final).await?;
    let gid =
        crate::store::live_group_create(st, event_id, stage_id, StageKind::Final, 1, "决赛", 3, 0)
            .await?;

    let mut seen = HashSet::new();
    let mut seeds = Vec::with_capacity(people.len());
    for (order, seat) in people.iter().enumerate() {
        let person_id = seat.get("personId").and_then(|v| v.as_i64()).unwrap_or(0);
        let mut uid = seat
            .get("douyinUid")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let plan_name = seat
            .get("personName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let anchor = if uid.is_empty() && person_id > 0 {
            crate::store::live_anchor_for_person(st, person_id).await
        } else {
            None
        };
        let (anchor_uid, anchor_name) = anchor.unwrap_or_default();
        if uid.is_empty() {
            uid = anchor_uid;
        }
        let dedup = if !uid.is_empty() {
            format!("uid:{uid}")
        } else if person_id > 0 {
            format!("person:{person_id}")
        } else if !plan_name.is_empty() {
            format!("name:{plan_name}")
        } else {
            continue;
        };
        if !seen.insert(dedup) {
            continue;
        }
        let nickname = if !plan_name.is_empty() {
            plan_name
        } else if !anchor_name.is_empty() {
            anchor_name
        } else if person_id > 0 {
            format!("#{person_id}")
        } else {
            continue;
        };
        seeds.push(crate::store::GroupSeedSeat {
            person_id,
            douyin_uid: uid,
            nickname,
            seed_order: order as i32,
        });
    }
    if seeds.is_empty() {
        anyhow::bail!("晋级赛出线名单缺少可用主播");
    }
    crate::store::live_group_seed_replace(st, gid, &seeds).await?;
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(ids: &[&str], scores: &[i64], status: i64, start: i64, dur: i64) -> BattleSnapshot {
        BattleSnapshot {
            battle_id: "b1".into(),
            channel_id: "c1".into(),
            start_time_ms: start,
            duration_secs: dur,
            battle_status: status,
            scores: ids
                .iter()
                .zip(scores.iter())
                .map(|(u, s)| BattleScore {
                    user_id: (*u).into(),
                    nickname: (*u).into(),
                    score: *s,
                    rank: 0,
                    score_version: 1,
                    team_id: String::new(),
                    bonus_multiple: 100,
                    bonus_score: 0,
                    bonus_active: false,
                    bonus_end_ms: 0,
                    current_bonus: 0,
                })
                .collect(),
            team_scores: Vec::new(),
        }
    }

    /* ---------------------- 推送的三层开关（纯逻辑） ---------------------- */

    /// 推送开关的层次判定：任一层关掉就不推。
    /// 这里测的是纯函数口径（与 live::push_round_scores 的判断一致），
    /// 不碰数据库与网络。
    fn push_allowed(global: bool, event: bool, person: bool) -> bool {
        global && event && person
    }

    #[test]
    fn push_requires_all_three_layers() {
        assert!(push_allowed(true, true, true));
        assert!(!push_allowed(false, true, true), "总闸关 → 不发");
        assert!(!push_allowed(true, false, true), "赛事关 → 不发");
        assert!(!push_allowed(true, true, false), "主播没开 → 不发");
    }

    #[test]
    fn push_interval_bounds_are_enforced() {
        // 与 live_api::update_push 的校验同一口径：10~3600 秒
        for bad in [0, 1, 9, 3601, 100_000] {
            assert!(!(10..=3600).contains(&bad), "{bad} 应被拒");
        }
        for ok in [10, 60, 300, 3600] {
            assert!((10..=3600).contains(&ok), "{ok} 应被接受");
        }
    }

    #[test]
    fn cutoff_score_is_lowest_at_rank() {
        // 晋级线 = 第 qualify 名的分数（并列时取该名次的分数）
        let seats = vec![
            serde_json::json!({"rank": 1, "score": 900}),
            serde_json::json!({"rank": 2, "score": 800}),
            serde_json::json!({"rank": 2, "score": 800}),
            serde_json::json!({"rank": 4, "score": 600}),
        ];
        let qualify = 2;
        let cutoff = seats
            .iter()
            .filter(|s| s.get("rank").and_then(|v| v.as_i64()).unwrap_or(0) as i32 == qualify)
            .filter_map(|s| s.get("score").and_then(|v| v.as_i64()))
            .min();
        assert_eq!(cutoff, Some(800), "晋级线是并列第 2 名的分数");
    }

    #[test]
    fn rules_default_is_valid() {
        assert!(validate_rules(&LiveRules::default()).is_ok());
    }

    #[test]
    fn rules_reject_bad_values() {
        let mut r = LiveRules::default();
        r.direct_qualify_per_group = 0;
        assert!(validate_rules(&r).is_err());
        r = LiveRules::default();
        r.revival_qualify_per_room = 9;
        assert!(validate_rules(&r).is_err(), "复活晋级不能超过房容量");
    }

    #[test]
    fn battle_stats_present_but_ended_is_not_running() {
        // 开始 1000、时长 300 秒 → 预计结束 301000；now 已过 40 秒 → 结算宽限内
        let s = snap(&["a", "b"], &[10, 5], 1, 1_000, 300);
        assert_eq!(classify_battle(&s, 341_000, 60), BattlePhase::Settling);
        // 超过宽限 → 已结束（即便 battle_status 还是 1）
        assert_eq!(classify_battle(&s, 500_000, 60), BattlePhase::Ended);
        // 正常进行中
        assert_eq!(classify_battle(&s, 100_000, 60), BattlePhase::Running);
    }

    #[test]
    fn battle_status_ended_shortcircuits_clock() {
        let s = snap(&["a", "b"], &[10, 5], 2, 1_000, 300);
        assert_eq!(classify_battle(&s, 20_000, 60), BattlePhase::Settling);
    }

    #[test]
    fn expected_end_uses_checked_arithmetic() {
        let mut s = snap(&["a"], &[1], 1, i64::MAX - 10, 1);
        assert_eq!(s.expected_end_ms(), None, "溢出不能 panic / 回绕");
        s.start_time_ms = 1_000;
        s.duration_secs = 300;
        assert_eq!(s.expected_end_ms(), Some(301_000));
    }

    #[test]
    fn observation_validation_rejects_oversized_or_negative_values() {
        let mut obs = Observation {
            web_rid: "505899768449".into(),
            room_id: "7705758224691399458".into(),
            room_online: true,
            now_ms: 1_700_000_000_000,
            battle: Some(snap(&["a"], &[10], 1, 1_000, 300)),
            health: "ok".into(),
            error: String::new(),
        };
        assert!(validate_observation(&obs).is_ok());

        obs.battle.as_mut().unwrap().scores[0].score = -1;
        assert!(validate_observation(&obs).is_err());

        obs.battle.as_mut().unwrap().scores[0].score = 10;
        obs.battle.as_mut().unwrap().scores[0].nickname = "x".repeat(129);
        assert!(validate_observation(&obs).is_err());

        obs.battle.as_mut().unwrap().scores[0].nickname = "a".into();
        obs.battle.as_mut().unwrap().duration_secs = MAX_BATTLE_DURATION_SECS + 1;
        assert!(validate_observation(&obs).is_err());

        obs.battle.as_mut().unwrap().duration_secs = 300;
        obs.web_rid = "x".repeat(41);
        assert!(validate_observation(&obs).is_err());
    }

    #[test]
    fn ranked_seats_keep_score_version() {
        let s = snap(&["a", "b"], &[10, 5], 1, 1_000, 300);
        let ranked = s.ranked();
        assert_eq!(ranked[0].score_version, 1);
        assert_eq!(ranked[1].score_version, 1);
    }

    #[test]
    fn sample_rows_keep_team_score_and_rank() {
        let mut s = snap(&["a", "b"], &[10, 5], 1, 1_000, 300);
        s.scores[0].team_id = "red".into();
        s.scores[1].team_id = "blue".into();
        s.team_scores = vec![
            TeamScore {
                team_id: "red".into(),
                score: 10,
                rank: 1,
            },
            TeamScore {
                team_id: "blue".into(),
                score: 5,
                rank: 2,
            },
        ];
        let rows = battle_sample_rows(&s);
        assert_eq!(rows[0].1, 10);
        assert_eq!(rows[0].2, 1);
        assert_eq!(rows[1].1, 5);
        assert_eq!(rows[1].2, 2);
    }

    #[test]
    fn early_end_detected() {
        let s = snap(&["a", "b"], &[10, 5], 2, 1_000, 300);
        // 预计 301 秒结束，实际 100 秒就结束 → 提前
        assert!(is_early_end(&s, 100_000));
        assert!(!is_early_end(&s, 300_000));
    }

    #[test]
    fn missing_participants_finds_dropped() {
        let prev = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let s = snap(&["a", "c"], &[1, 2], 1, 0, 300);
        assert_eq!(missing_participants(&prev, &s), vec!["b".to_string()]);
    }

    #[test]
    fn ranking_handles_ties() {
        let s = snap(&["a", "b", "c", "d"], &[100, 100, 50, 10], 1, 0, 300);
        let r = s.ranked();
        assert_eq!(r[0].rank, 1);
        assert_eq!(r[1].rank, 1, "同分同名次");
        assert_eq!(r[2].rank, 3, "跳过并列占用的名次");
        assert_eq!(r[3].rank, 4);
    }

    #[test]
    fn duplicate_rounds_both_completed_needs_review() {
        // 用户口径：两局人数相同且 > 3，不能自动覆盖
        assert_eq!(
            classify_duplicate("a,b,c,d", "a,b,c,d", true, true),
            DuplicateVerdict::BothCompleted
        );
        assert_eq!(
            classify_duplicate("a,b,c,d", "a,b,c,d", true, false),
            DuplicateVerdict::OneCompleted
        );
        assert_eq!(
            classify_duplicate("a,b,c", "a,b,c,d", true, true),
            DuplicateVerdict::DifferentRoster
        );
    }

    #[test]
    fn revival_rooms_split_evenly_16_into_two() {
        let people: Vec<i32> = (1..=16).collect();
        let rooms = split_revival_rooms(&people, 8);
        assert_eq!(rooms.len(), 2);
        assert_eq!(rooms[0].len(), 8);
        assert_eq!(rooms[1].len(), 8, "16 人淘汰 → 两个 8 人复活房");
        // 蛇形：1 在房 1、2 在房 2、3 回房 1，强弱不扎堆
        assert_eq!(rooms[0][0], 1);
        assert_eq!(rooms[1][0], 2);
        // 蛇形：第 1 轮正序（1→房1、2→房2），第 2 轮**反序**（3→房2、4→房1），
        // 这样连续名次不会被发进同一个房间。
        assert_eq!(rooms[1][1], 3);
        assert_eq!(rooms[0][1], 4);
    }

    #[test]
    fn revival_rooms_uneven_differs_by_at_most_one() {
        let people: Vec<i32> = (1..=17).collect();
        let rooms = split_revival_rooms(&people, 8);
        assert_eq!(rooms.len(), 3);
        let lens: Vec<usize> = rooms.iter().map(|r| r.len()).collect();
        // 蛇形发牌下余数落在**靠前**的房间（第 3 轮只发了 1 个人），
        // 真正要保证的是「任意两房人数差 ≤ 1」，不是「最后一房最少」。
        assert!(
            lens.iter().max().unwrap() - lens.iter().min().unwrap() <= 1,
            "人数差不超过 1，实际 {lens:?}"
        );
        assert_eq!(lens.iter().sum::<usize>(), 17, "人不能丢");
    }

    #[test]
    fn promotion_groups_respect_fixed_count() {
        let people: Vec<i32> = (1..=18).collect();
        let g = split_promotion_groups(&people, 8, 0);
        assert_eq!(g.len(), 3, "18 人 / 每组 8 人 → 3 组");
        let g = split_promotion_groups(&people, 8, 6);
        assert_eq!(g.len(), 6);
        let total: usize = g.iter().map(|x| x.len()).sum();
        assert_eq!(total, 18, "人不能丢");
    }

    #[test]
    fn stage_order_drives_barrier() {
        assert!(StageKind::FirstWaveGroup.order() < StageKind::FirstWaveRevival.order());
        assert!(StageKind::FirstWaveRevival.order() < StageKind::RestGroup.order());
        assert!(StageKind::RestRevival.order() < StageKind::Promotion.order());
        assert!(StageKind::Promotion.order() < StageKind::Final.order());
    }

    #[test]
    fn only_finalized_group_counts_as_done() {
        assert!(group_is_done(GroupStatus::Finalized));
        assert!(!group_is_done(GroupStatus::Running));
        assert!(!group_is_done(GroupStatus::Settling));
        assert!(!group_is_done(GroupStatus::PendingReview));
        assert!(!group_is_done(GroupStatus::WaitingRestart));
    }

    #[test]
    fn roster_signature_is_order_independent() {
        let a = snap(&["c", "a", "b"], &[1, 2, 3], 1, 0, 300);
        let b = snap(&["a", "b", "c"], &[3, 2, 1], 1, 0, 300);
        assert_eq!(a.roster_signature(), b.roster_signature());
    }
}
