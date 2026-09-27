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

/// 前端可配的约束子集（互斥/固定位/间隔写死，只放开赛时间）
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ConstraintInput {
    #[serde(default)]
    pub schedule: Option<ScheduleInput>,
    /// 完整覆盖（可选）：前端一般不传，用后端内置条件
    #[serde(default)]
    pub full: Option<ConstraintSet>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ScheduleInput {
    pub start: String,
    #[serde(rename = "intervalMin")]
    pub interval_min: i64,
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
    let base = input.and_then(|c| c.full.clone()).unwrap_or_default();
    match input.and_then(|c| c.schedule.as_ref()) {
        Some(s) => ConstraintSet {
            schedule: pk_core::ScheduleRule {
                start: s.start.clone(),
                interval_min: s.interval_min,
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
