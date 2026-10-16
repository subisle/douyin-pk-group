//! 分组算法 HTTP 端点：自动分组 / 体检当前分组。
//!
//! 算法实现在 pk-core（Rust）。前端只负责拖拽与展示，
//! 一致性由这里保证——浏览器里不再跑第二份算法。

use axum::extract::State;
use axum::Json;
use pk_core::{
    auto_group, compute_balance, find_violations, slot_time, BalanceStat, ConstraintSet,
    GroupOptions, Player, Strategy, Violation,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::routes::ApiError;
use crate::state::AppState;
use crate::store;

/// 前端可配的约束子集（互斥/固定位/间隔写死，只放开赛时间）
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ConstraintInput {
    #[serde(default)]
    pub schedule: Option<ScheduleInput>,
    /// 完整覆盖（可选）：前端一般不传，用后端内置条件
    #[serde(default)]
    pub full: Option<ConstraintSet>,
    /// 强度位次（可选）：独立于 full，前端只开关位次时不用重发整套内置条件
    #[serde(default, rename = "strengthOrder")]
    pub strength_order: Option<pk_core::StrengthOrder>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ScheduleInput {
    pub start: String,
    #[serde(rename = "intervalMin")]
    pub interval_min: i64,
    /// 加长休息点（可选）：如第 3 组后隔 20 分钟
    #[serde(default)]
    pub breaks: Option<Vec<pk_core::BreakRule>>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ArrangeRequest {
    pub players: Vec<Player>,
    #[serde(rename = "groupSize", default = "default_group_size")]
    pub group_size: usize,
    #[serde(default)]
    pub strategy: Strategy,
    #[serde(default)]
    pub constraints: Option<ConstraintInput>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ArrangeResponse {
    pub groups: Vec<Vec<Player>>,
    pub balance: BalanceStat,
    /// 每组开赛时间（index 与 groups 对齐）
    pub slots: Vec<String>,
    pub violations: Vec<Violation>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct InspectRequest {
    pub groups: Vec<Vec<Player>>,
    #[serde(default)]
    pub constraints: Option<ConstraintInput>,
}

#[derive(Serialize, Clone, Debug)]
pub struct InspectResponse {
    pub balance: BalanceStat,
    pub violations: Vec<Violation>,
    pub slots: Vec<String>,
}

fn default_group_size() -> usize {
    8
}

/// 合并前端可配项与后端内置条件
pub fn resolve_constraints(input: Option<&ConstraintInput>) -> ConstraintSet {
    let mut base = input.and_then(|c| c.full.clone()).unwrap_or_default();
    if let Some(so) = input.and_then(|c| c.strength_order.as_ref()) {
        base.strength_order = Some(so.clone());
    }
    match input.and_then(|c| c.schedule.as_ref()) {
        Some(s) => ConstraintSet {
            schedule: pk_core::ScheduleRule {
                start: s.start.clone(),
                interval_min: s.interval_min,
                breaks: s.breaks.clone().unwrap_or_default(),
            },
            ..base
        },
        None => base,
    }
}

type GroupResult<T> = Result<Json<T>, ApiError>;

pub async fn arrange(
    State(_st): State<Arc<AppState>>,
    Json(req): Json<ArrangeRequest>,
) -> GroupResult<ArrangeResponse> {
    let constraints = resolve_constraints(req.constraints.as_ref());
    let groups = auto_group(
        &req.players,
        &GroupOptions {
            group_size: req.group_size,
            strategy: req.strategy,
            constraints: Some(constraints.clone()),
        },
    );
    let slots = groups
        .iter()
        .enumerate()
        .map(|(i, _)| slot_time(i, &constraints.schedule))
        .collect();
    Ok(Json(ArrangeResponse {
        balance: compute_balance(&groups),
        violations: find_violations(&groups, &constraints),
        groups,
        slots,
    }))
}

/// 拖拽后体检：重算均衡度、列出约束冲突
pub async fn inspect(
    State(_st): State<Arc<AppState>>,
    Json(req): Json<InspectRequest>,
) -> GroupResult<InspectResponse> {
    let constraints = resolve_constraints(req.constraints.as_ref());
    let slots = req
        .groups
        .iter()
        .enumerate()
        .map(|(i, _)| slot_time(i, &constraints.schedule))
        .collect();
    Ok(Json(InspectResponse {
        balance: compute_balance(&req.groups),
        violations: find_violations(&req.groups, &constraints),
        slots,
    }))
}

/// 把内置条件交给前端展示（互斥/固定位/间隔说明文案）
pub async fn constraints_meta() -> GroupResult<serde_json::Value> {
    let cs = ConstraintSet::default();
    let pinned: Vec<serde_json::Value> = cs
        .pinned
        .iter()
        .map(|p| {
            let group = match &p.group {
                pk_core::PinnedGroup::Index(n) => serde_json::Value::from(*n),
                pk_core::PinnedGroup::Last(_) => serde_json::Value::from("last"),
            };
            serde_json::json!({ "name": p.name, "group": group })
        })
        .collect();
    Ok(Json(serde_json::json!({
        "excludeTogether": cs.exclude_together,
        "pinned": pinned,
        "schedule": { "start": cs.schedule.start, "intervalMin": cs.schedule.interval_min },
        "minGap": cs.min_gap,
        "minGroup": cs.min_group,
    })))
}

/// 在现有分组上做「均衡微调」：只接受让极差变小的交换，不动锁定的人
#[derive(Serialize, Clone, Debug)]
pub struct PolishResponse {
    pub groups: Vec<Vec<Player>>,
    pub balance: BalanceStat,
    pub violations: Vec<Violation>,
    pub slots: Vec<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct PolishRequest {
    pub groups: Vec<Vec<Player>>,
    /// 锁住不动的人（key 集合）
    #[serde(default)]
    pub protected: Vec<String>,
    #[serde(default)]
    pub constraints: Option<ConstraintInput>,
}

pub async fn polish(
    State(_st): State<Arc<AppState>>,
    Json(req): Json<PolishRequest>,
) -> GroupResult<PolishResponse> {
    let constraints = resolve_constraints(req.constraints.as_ref());
    let protected: std::collections::HashSet<String> = req.protected.into_iter().collect();
    let groups = pk_core::constraints::polish_balance(
        &req.groups,
        &pk_core::PolishOptions {
            protected_keys: protected,
            exclude_sets: constraints.exclude_together.clone(),
            avoid_master: false,
            min_gap_rules: constraints.min_gap.clone(),
            max_iter: 400,
        },
    );
    let slots = groups
        .iter()
        .enumerate()
        .map(|(i, _)| slot_time(i, &constraints.schedule))
        .collect();
    Ok(Json(PolishResponse {
        balance: compute_balance(&groups),
        violations: find_violations(&groups, &constraints),
        groups,
        slots,
    }))
}

/* ------------------------------ PK 展板 ------------------------------ */

/// 展板配置的 key（config 表单键，沿用「不建新表」的口径——展板配置本质
/// 也是跨重启保留的偏好项，和机器人 token 一个待遇）。
pub const BOARD_CONFIG_KEY: &str = "pk_board_config";

/// PK 展板的分组配置：前端「展板」页把整份配置原样存后端，
/// 换设备/重开页面再取回来。字段语义与 /api/group/arrange 的请求一一对应，
/// 所以单独落一份白名单校验，避免坏配置把 arrange 调用打挂。
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct BoardConfig {
    #[serde(rename = "eventType")]
    pub event_type: String,
    pub metric: String,
    pub days: i64,
    pub strategy: String,
    #[serde(rename = "strengthMode")]
    pub strength_mode: String,
    #[serde(rename = "groupSize")]
    pub group_size: usize,
    #[serde(rename = "startTime")]
    pub start_time: String,
    #[serde(rename = "intervalMin")]
    pub interval_min: i64,
    #[serde(rename = "longBreak")]
    pub long_break: i64,
}

/// 展板配置白名单校验。单独拆成纯函数（Value 进、BoardConfig 出），
/// 方便单测直接喂 JSON；handler 也走它，保证「存进去的一定合法」。
pub fn validate_board_config(v: &mut serde_json::Value) -> Result<BoardConfig, String> {
    let config: BoardConfig = serde_json::from_value(v.take())
        .map_err(|e| format!("展板配置字段缺失或类型不对：{e}"))?;

    // 赛事类型：先归一（兼容老叫法），归一后仍不在白名单才算非法
    let ev = store::normalize_event_type(&config.event_type);
    if !store::EVENT_TYPES.contains(&ev.as_str()) {
        return Err("不认识的赛事类型".into());
    }
    let mut config = config;
    config.event_type = ev;

    if !matches!(config.metric.as_str(), "lastN" | "last1" | "month" | "total") {
        return Err("取数口径只能是 lastN / last1 / month / total".into());
    }
    // last1 时天数前端自己管（本来就不看），这里只卡 lastN 的范围
    if config.metric != "last1" && !(1..=30).contains(&config.days) {
        return Err("统计天数要在 1~30 之间".into());
    }
    if !matches!(
        config.strategy.as_str(),
        "snake" | "snake-no-master" | "sequential" | "sequential-last-n" | "random"
    ) {
        return Err("不认识的分组策略".into());
    }
    if !matches!(config.strength_mode.as_str(), "top" | "total" | "off") {
        return Err("不认识的强度模式".into());
    }
    if !(2..=20).contains(&config.group_size) {
        return Err("每组人数要在 2~20 之间".into());
    }
    // 手工校验 HH:MM，避免引正则依赖；HH<24 MM<60 才算合法时刻
    let t = config.start_time.as_bytes();
    let ok_time = t.len() == 5
        && t[0].is_ascii_digit()
        && t[1].is_ascii_digit()
        && t[2] == b':'
        && t[3].is_ascii_digit()
        && t[4].is_ascii_digit()
        && t[0..2] < b"24"[..] // 逐位比较 == 按十进制比大小
        && t[3..5] < b"60"[..];
    if !ok_time {
        return Err("开赛时间要是 HH:MM 格式（如 12:15）".into());
    }
    if !(1..=120).contains(&config.interval_min) {
        return Err("每组间隔要在 1~120 分钟之间".into());
    }
    if !(0..=120).contains(&config.long_break) {
        return Err("加长休息要在 0~120 分钟之间（0 = 不插）".into());
    }
    Ok(config)
}

/// 读展板配置：没存过按 `{saved:false}` 处理，坏 JSON 也不 500——
/// 配置读不出来顶多让前端用默认值，不该把整个展板页打挂。
pub async fn board_config_get(
    State(st): State<Arc<AppState>>,
) -> GroupResult<serde_json::Value> {
    let config = match store::config_get(&st, BOARD_CONFIG_KEY).await {
        Some(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(v) => Some(v),
            // 理论上不会发生（PUT 侧只存合法 JSON），防御一下
            Err(e) => {
                tracing::error!(error = %e, "展板配置 JSON 解析失败，按没存过处理");
                None
            }
        },
        None => None,
    };
    Ok(Json(serde_json::json!({
        "saved": config.is_some(),
        "config": config,
    })))
}

/// 存展板配置：请求体就是配置本体（不包一层），校验通过才落库
pub async fn board_config_put(
    State(st): State<Arc<AppState>>,
    Json(mut body): Json<serde_json::Value>,
) -> GroupResult<serde_json::Value> {
    let config = validate_board_config(&mut body).map_err(ApiError::bad_request)?;
    store::config_set(&st, BOARD_CONFIG_KEY, &serde_json::to_string(&config)?).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合法配置原样通过（eventType 老叫法顺带被归一到新白名单值）
    #[test]
    fn board_config_accepts_valid() {
        let mut v: serde_json::Value = serde_json::from_str(
            r#"{
                "eventType": "初级",
                "metric": "lastN", "days": 3,
                "strategy": "snake", "strengthMode": "top",
                "groupSize": 8, "startTime": "12:15",
                "intervalMin": 15, "longBreak": 20
            }"#,
        )
        .unwrap();
        let cfg = validate_board_config(&mut v).unwrap();
        assert_eq!(cfg.event_type, "初级争霸赛小组赛");
        assert_eq!(cfg.group_size, 8);
    }

    /// 每组人数越界（这里 21）必须被拒，且带中文说明
    #[test]
    fn board_config_rejects_group_size_out_of_range() {
        let mut v: serde_json::Value = serde_json::from_str(
            r#"{
                "eventType": "初级争霸赛小组赛",
                "metric": "lastN", "days": 3,
                "strategy": "snake", "strengthMode": "top",
                "groupSize": 21, "startTime": "12:15",
                "intervalMin": 15, "longBreak": 0
            }"#,
        )
        .unwrap();
        let err = validate_board_config(&mut v).unwrap_err();
        assert!(err.contains("2~20"), "实际报错：{err}");
    }

    /// 白名单外的赛事类型必须被拒（归一救不回来的那种）
    #[test]
    fn board_config_rejects_unknown_event_type() {
        let mut v: serde_json::Value = serde_json::from_str(
            r#"{
                "eventType": "周五娱乐赛",
                "metric": "lastN", "days": 3,
                "strategy": "snake", "strengthMode": "off",
                "groupSize": 8, "startTime": "12:15",
                "intervalMin": 15, "longBreak": 0
            }"#,
        )
        .unwrap();
        let err = validate_board_config(&mut v).unwrap_err();
        assert_eq!(err, "不认识的赛事类型");
    }
}
