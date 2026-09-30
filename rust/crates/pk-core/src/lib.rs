//! 抖音 PK 挑战赛分组核心：名单解析 + 内置约束 + 均衡分组
//!
//! 与旧版 `packages/shared`（TS）一一对应，行为保持一致，便于对照回归。

pub mod constraints;
pub mod group;
pub mod roster;
pub mod types;

pub use constraints::{
    find_by_name, find_violations, gap_conflicts, is_pinned_name, min_group_conflicts, slot_time,
    ConstraintSet, MinGapRule, MinGroupRule, PinnedGroup, PinnedRule, PolishOptions, ScheduleRule,
    Violation,
};
pub use group::{
    auto_group, compute_balance, group_count, is_valid, move_player, random_group,
    resolve_master_conflicts, sequential_group, snake_group,
};
pub use roster::{
    apply_alias, default_aliases, default_exclude, normalize_name, parse_roster, AliasMap,
    ParseOptions, DEFAULT_ROSTER,
};
pub use types::{BalanceStat, GroupOptions, GroupStat, ParsedRow, Player, Strategy, WaveMetric};

/// 内置条件（用户拍板的那一套）：玖玉/玖妹/玖玥 互斥、啸泽第一组、啸帆最后一组、
/// 啸昭与啸寻至少隔 4 组、08:15 起每 15 分钟一组
pub fn default_constraints() -> ConstraintSet {
    ConstraintSet::default()
}

/// 音浪格式化：亿 / 万 / 原值
pub fn format_wave(n: f64) -> String {
    if n >= 1e8 {
        format!("{:.2}亿", n / 1e8)
    } else if n >= 1e4 {
        format!("{:.1}万", n / 1e4)
    } else {
        format!("{}", n.round() as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave_format() {
        assert_eq!(format_wave(1_735_000.0), "173.5万");
        assert_eq!(format_wave(123_456_789.0), "1.23亿");
        assert_eq!(format_wave(9999.0), "9999");
    }
}
