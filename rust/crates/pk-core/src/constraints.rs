//! 内置条件与修复：互斥、固定位、最小间隔、同门回避、均衡微调
//!
//! 修复策略统一为「交换」：只做两人对调，天然保持各组人数不变；
//! 代价函数用「与全组均值的偏离变化」打分，所以每次交换都让均衡变好。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::types::Player;

/// 固定位：把某人钉在指定组（1 起数；`last` = 最后一组）
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedRule {
    pub name: String,
    pub group: PinnedGroup,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PinnedGroup {
    Index(u32),
    Last(String),
}

impl PinnedGroup {
    pub fn last() -> Self {
        Self::Last("last".to_string())
    }

    pub fn is_last(&self) -> bool {
        matches!(self, Self::Last(v) if v == "last")
    }
}

/// 加长休息：第 after_group 组（1 起）之后到下组开始，间隔改成 minutes 分钟。
/// 例：{ afterGroup: 3, minutes: 20 } = 第 3 组打完后休息到 20 分钟再开第 4 组。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BreakRule {
    pub after_group: usize,
    pub minutes: i64,
}

/// 开赛时间：第 1 组 start 开始，之后每组顺延 interval_min 分钟
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRule {
    /// "08:15"
    pub start: String,
    pub interval_min: i64,
    /// 加长休息点（可选，向后兼容老方案）
    #[serde(default)]
    pub breaks: Vec<BreakRule>,
}

/// 第 index 组（0 起）到第 index+1 组之间的间隔分钟数：命中加长休息就用它的值
pub fn gap_after(index: usize, schedule: &ScheduleRule) -> i64 {
    let group_no = index + 1; // 「第几组之后」
    match schedule.breaks.iter().find(|b| b.after_group == group_no) {
        Some(b) => b.minutes.max(0),
        None => schedule.interval_min.max(0),
    }
}

/// 最小间隔：两人所在组号之差必须 ≥ gap（组数不足时约束自动放宽跳过）
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MinGapRule {
    pub a: String,
    pub b: String,
    pub gap: usize,
}

/// 最低组号：该人必须分到指定组号（1 起）或更后面的组
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MinGroupRule {
    pub name: String,
    pub group: u32,
}

/// 强度位次的算法形态
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StrengthMode {
    /// 集中：按各组**总音浪**排位次——第 3 组整体最强（强者集中）
    Total,
    /// 每组一个最强：各组都有一个头牌，头牌之间再按位次排——
    /// 第 3 组的头牌是全场最强、第 4 组次之、第 1 组的头牌最弱
    #[default]
    Top,
}

/// 强度位次：各组「实力」按指定顺序排列（最强 → 最弱的组号，1 起）。
///
/// 语义：这是**次级目标**——强制条件（固定位 / 互斥 / 最小间隔）先满足，
/// 之后再通过对调把各组实力摆成这条顺序。默认「第 3 组最强、第 4 组次强、
/// 第 1 组最弱」，其余按组号降序（3,4,…,g,2,1）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StrengthOrder {
    /// 是否启用
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// 算法形态：集中（按组总音浪）/ 每组一个最强（按头牌）
    #[serde(default)]
    pub mode: StrengthMode,
    /// 最强 → 最弱的组号（1 起）。留空时用 `default_strength_order(组数)`
    #[serde(default)]
    pub order: Vec<usize>,
}

fn default_enabled() -> bool {
    true
}

impl Default for StrengthOrder {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: StrengthMode::Top,
            order: Vec::new(),
        }
    }
}

/// 默认位次：第 3 组最强、第 4 组次强，之后组号越大越弱，第 2 组次弱、第 1 组最弱。
/// 组数不足 3 组时退化为自然序（第 1 组最强）。
pub fn default_strength_order(group_count: usize) -> Vec<usize> {
    if group_count < 3 {
        return (1..=group_count).collect();
    }
    let mut v: Vec<usize> = (3..=group_count).collect();
    v.push(2);
    v.push(1);
    v
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConstraintSet {
    /// 互斥组：同一数组内的人两两不能分到同一组
    pub exclude_together: Vec<Vec<String>>,
    pub pinned: Vec<PinnedRule>,
    pub schedule: ScheduleRule,
    /// 最小间隔（可选，向后兼容旧数据）
    #[serde(default)]
    pub min_gap: Vec<MinGapRule>,
    /// 最低组号（可选，向后兼容旧数据）
    #[serde(default)]
    pub min_group: Vec<MinGroupRule>,
    /// 强度位次（可选）：不传 = 不启用，保持纯均衡
    #[serde(default)]
    pub strength_order: Option<StrengthOrder>,
}

impl Default for ConstraintSet {
    fn default() -> Self {
        Self {
            exclude_together: vec![
                vec!["玖玉".into(), "玖妹".into(), "玖玥".into()],
                vec!["狼影".into(), "浩冬".into()],
            ],
            pinned: vec![
                // 2026-10-07 深夜改：啸泽从「第 1 组」改成「最后一组」
                PinnedRule {
                    name: "啸泽".into(),
                    group: PinnedGroup::last(),
                },
                PinnedRule {
                    name: "啸阳".into(),
                    group: PinnedGroup::Index(1),
                },
                // 2026-10-07 深夜改：啸帆从「最后一组」改成「第 2 组」
                PinnedRule {
                    name: "啸帆".into(),
                    group: PinnedGroup::Index(2),
                },
                // 2026-10-07 补：浩森固定在第 1 组、浩泽固定在最后一组
                PinnedRule {
                    name: "浩森".into(),
                    group: PinnedGroup::Index(1),
                },
                PinnedRule {
                    name: "浩泽".into(),
                    group: PinnedGroup::last(),
                },
            ],
            schedule: ScheduleRule {
                // 2026-10-09 用户拍板：小组赛中午 12:15 开始（第 1 组）
                start: "12:15".into(),
                interval_min: 15,
                // 默认不插加长休息，老方案与测试的时间轴保持不变；
                // 「第 3 组后 20 分钟」由前端显式传（每个方案自己带赛程）
                breaks: Vec::new(),
            },
            min_gap: vec![
                MinGapRule {
                    a: "啸昭".into(),
                    b: "啸寻".into(),
                    gap: 4,
                },
                // 2026-10-07 补：浩泽固定在最后一组，浩森与浩泽至少相隔 3 组
                MinGapRule {
                    a: "浩森".into(),
                    b: "浩泽".into(),
                    gap: 3,
                },
                // 2026-10-07 深夜补：狼佑与玖安至少相隔 3 组
                MinGapRule {
                    a: "狼佑".into(),
                    b: "玖安".into(),
                    gap: 3,
                },
            ],
            min_group: vec![MinGroupRule {
                name: "浩阳".into(),
                group: 6,
            }],
            // 默认不启用：老调用方（机器人 / 旧脚本）行为保持不变，
            // 需要位次时由调用方显式传入（前端默认勾选后每次都会传）
            strength_order: None,
        }
    }
}

pub fn norm_name(name: &str) -> &str {
    name.trim()
}

/// 第 index 组（0 起）的开赛时间，如 08:15 / 08:30
pub fn slot_time(index: usize, schedule: &ScheduleRule) -> String {
    let (h, m) = match schedule.start.split_once(':') {
        Some((h, m)) => (
            h.trim().parse::<i64>().unwrap_or(0),
            m.trim().parse::<i64>().unwrap_or(0),
        ),
        None => (8, 15),
    };
    // 逐段累加：中间可能插了加长休息（如第 3 组后 20 分钟），不能一律乘 interval
    let mut total = h * 60 + m;
    for i in 0..index {
        total += gap_after(i, schedule);
    }
    format!(
        "{:02}:{:02}",
        (total / 60).rem_euclid(24),
        total.rem_euclid(60)
    )
}

/// 固定位解析成实际组下标（0 起）
pub fn resolve_pinned_index(rule: &PinnedRule, group_count: usize) -> usize {
    if group_count == 0 {
        return 0;
    }
    match &rule.group {
        PinnedGroup::Last(_) => group_count - 1,
        PinnedGroup::Index(n) => (*n as usize).saturating_sub(1).min(group_count - 1),
    }
}

/// 按名字在名单里找人（忽略首尾空格）
pub fn find_by_name<'a>(list: &'a [Player], name: &str) -> Option<&'a Player> {
    let target = norm_name(name);
    list.iter().find(|x| norm_name(&x.name) == target)
}

/// 约束是否命中了某人
pub fn is_pinned_name<'a>(name: &str, cs: &'a ConstraintSet) -> Option<&'a PinnedRule> {
    let n = norm_name(name);
    cs.pinned.iter().find(|r| norm_name(&r.name) == n)
}

/// 某人命中的互斥组
pub fn excluded_sets_of<'a>(name: &str, cs: &'a ConstraintSet) -> Vec<&'a [String]> {
    let n = norm_name(name);
    cs.exclude_together
        .iter()
        .filter(|set| set.iter().any(|x| norm_name(x) == n))
        .map(Vec::as_slice)
        .collect()
}

fn norm_set(sets: &[Vec<String>]) -> Vec<HashSet<String>> {
    sets.iter()
        .map(|s| {
            s.iter()
                .map(|x| norm_name(x).to_string())
                .collect::<HashSet<_>>()
        })
        .filter(|s| s.len() > 1)
        .collect()
}

pub fn sum_wave(group: &[Player]) -> f64 {
    group.iter().map(|p| p.wave).sum()
}

fn count_in(group: &[Player], set: &HashSet<String>) -> usize {
    group
        .iter()
        .filter(|p| set.contains(norm_name(&p.name)))
        .count()
}

/// 互斥修复：把同一互斥组里多出来的人换到没有该组其他人的组，
/// 交换时优先不破坏音浪均衡，且绝不移动固定位的人。
pub fn resolve_exclusions(
    groups: &[Vec<Player>],
    sets: &[Vec<String>],
    protected_keys: &HashSet<String>,
    max_iter: usize,
) -> Vec<Vec<Player>> {
    let set_list = norm_set(sets);
    if set_list.is_empty() {
        return groups.to_vec();
    }

    let mut result: Vec<Vec<Player>> = groups.to_vec();
    let total: f64 = result.iter().map(|g| sum_wave(g)).sum();
    let mean = if result.is_empty() {
        0.0
    } else {
        total / result.len() as f64
    };

    let conflict_count = |result: &[Vec<Player>]| -> usize {
        result
            .iter()
            .map(|g| {
                set_list
                    .iter()
                    .map(|s| count_in(g, s).saturating_sub(1))
                    .sum::<usize>()
            })
            .sum()
    };

    for _ in 0..max_iter {
        if conflict_count(&result) == 0 {
            break;
        }
        let mut best: Option<(usize, usize, usize, usize, f64)> = None;

        for ai in 0..result.len() {
            let sum_a = sum_wave(&result[ai]);
            for s in &set_list {
                if count_in(&result[ai], s) < 2 {
                    continue;
                }
                for pi in 0..result[ai].len() {
                    let p = result[ai][pi].clone();
                    if !s.contains(norm_name(&p.name)) || protected_keys.contains(&p.key) {
                        continue;
                    }
                    for bi in 0..result.len() {
                        if bi == ai || count_in(&result[bi], s) > 0 {
                            continue;
                        }
                        let sum_b = sum_wave(&result[bi]);
                        for qi in 0..result[bi].len() {
                            let q = result[bi][qi].clone();
                            if s.contains(norm_name(&q.name)) || protected_keys.contains(&q.key) {
                                continue;
                            }

                            // 交换是否会引入其它互斥组的冲突
                            let mut extra = 0.0;
                            for ss in &set_list {
                                if ss == s {
                                    continue;
                                }
                                let before = count_in(&result[ai], ss).saturating_sub(1) as f64
                                    + count_in(&result[bi], ss).saturating_sub(1) as f64;
                                let ca = count_in(&result[ai], ss)
                                    + usize::from(ss.contains(norm_name(&q.name)));
                                let cb = count_in(&result[bi], ss)
                                    + usize::from(ss.contains(norm_name(&p.name)));
                                extra += (ca as f64 - 1.0).max(0.0) + (cb as f64 - 1.0).max(0.0)
                                    - before;
                            }

                            let before = (sum_a - mean).abs() + (sum_b - mean).abs();
                            let after = (sum_a - p.wave + q.wave - mean).abs()
                                + (sum_b - q.wave + p.wave - mean).abs();
                            let score = extra * 1e9 + (after - before);
                            if best.is_none_or(|(_, _, _, _, b)| score < b) {
                                best = Some((ai, pi, bi, qi, score));
                            }
                        }
                    }
                }
            }
        }

        let Some((ai, pi, bi, qi, _)) = best else {
            break;
        };
        let tmp = result[ai][pi].clone();
        result[ai][pi] = result[bi][qi].clone();
        result[bi][qi] = tmp;
    }
    result
}

/// 把固定位的人放回指定组（按音浪降序插入，保持组内排序）
pub fn place_pinned(
    groups: &[Vec<Player>],
    players: &[Player],
    pinned: &[(String, usize)],
) -> Vec<Vec<Player>> {
    let mut result: Vec<Vec<Player>> = groups.to_vec();
    for (key, gi) in pinned {
        let Some(p) = players.iter().find(|x| x.key == *key) else {
            continue;
        };
        while result.len() <= *gi {
            result.push(Vec::new());
        }
        let arr = &mut result[*gi];
        if arr.iter().any(|x| x.key == *key) {
            continue;
        }
        match arr.iter().position(|x| x.wave < p.wave) {
            Some(at) => arr.insert(at, p.clone()),
            None => arr.push(p.clone()),
        }
    }
    result
}

fn group_index_of_name(groups: &[Vec<Player>], name: &str) -> Option<usize> {
    let n = norm_name(name);
    groups
        .iter()
        .position(|g| g.iter().any(|p| norm_name(&p.name) == n))
}

/// 统计最小间隔违约数（找不到人不计）
pub fn gap_conflicts(groups: &[Vec<Player>], rules: &[MinGapRule]) -> usize {
    rules
        .iter()
        .filter(|r| {
            match (
                group_index_of_name(groups, &r.a),
                group_index_of_name(groups, &r.b),
            ) {
                (Some(ia), Some(ib)) => ia.abs_diff(ib) < r.gap,
                _ => false,
            }
        })
        .count()
}

pub fn min_group_conflicts(groups: &[Vec<Player>], rules: &[MinGroupRule]) -> usize {
    rules
        .iter()
        .filter(|r| {
            group_index_of_name(groups, &r.name).is_some_and(|at| at + 1 < r.group as usize)
        })
        .count()
}

/// 最小间隔修复：对违约的两人，把其中非固定位的一人换到满足间隔的组，
/// 换入者选择对音浪均衡损伤最小、且不制造互斥冲突的对象。
/// 若单次交换无法直接满足（例如两人都被挤在中间组、够不着任何「距离 ≥ gap」的组），
/// 退而求其次做「拉远」换位：把一方换到离对方最远的组，逐轮迭代直到满足或到头。
pub fn resolve_min_gaps(
    groups: &[Vec<Player>],
    rules: &[MinGapRule],
    protected_keys: &HashSet<String>,
    exclude_sets: &[Vec<String>],
    max_iter: usize,
) -> Vec<Vec<Player>> {
    let mut result: Vec<Vec<Player>> = groups.to_vec();
    let set_list = norm_set(exclude_sets);
    if rules.is_empty() || result.len() < 2 {
        return result;
    }

    // 交换 p(在 gi) 与 q(在 ki) 后，两组成员中互斥组是否超 1 人
    let exclusion_ok =
        |res: &[Vec<Player>], gi: usize, ki: usize, p: &Player, q: &Player| -> bool {
            set_list.iter().all(|s| {
                let cnt = |g: &[Player], out: &Player, inn: &Player| {
                    count_in(g, s) as f64 - f64::from(s.contains(norm_name(&out.name)))
                        + f64::from(s.contains(norm_name(&inn.name)))
                };
                cnt(&res[gi], p, q) <= 1.0 && cnt(&res[ki], q, p) <= 1.0
            })
        };

    let total: f64 = result.iter().map(|g| sum_wave(g)).sum();
    let mean = total / result.len() as f64;

    for _ in 0..max_iter {
        if gap_conflicts(&result, rules) == 0 {
            break;
        }
        // 一步到位的交换（换过去就满足间隔）
        let mut best: Option<(usize, usize, usize, usize, f64)> = None;
        // 一步到不了时的「拉远」交换（只接受把两人距离拉得更大的，逐轮逼近）
        let mut relief: Option<(usize, usize, usize, usize, f64)> = None;
        let mut relief_dist = 0usize;

        for r in rules {
            let (Some(ia), Some(ib)) = (
                group_index_of_name(&result, &r.a),
                group_index_of_name(&result, &r.b),
            ) else {
                continue;
            };
            if ia.abs_diff(ib) >= r.gap {
                continue;
            }
            let cur_dist = ia.abs_diff(ib);

            // 两个方向都可尝试：动 a 或动 b（固定位优先不动）
            let mut moves = Vec::new();
            let pinned_a = result[ia]
                .iter()
                .any(|p| protected_keys.contains(&p.key) && norm_name(&p.name) == norm_name(&r.a));
            let pinned_b = result[ib]
                .iter()
                .any(|p| protected_keys.contains(&p.key) && norm_name(&p.name) == norm_name(&r.b));
            if !pinned_a {
                moves.push((ia, ib));
            }
            if !pinned_b {
                moves.push((ib, ia));
            }

            for (mi, other) in moves {
                for pi in 0..result[mi].len() {
                    let p = result[mi][pi].clone();
                    let name_p = norm_name(&p.name);
                    if name_p != norm_name(&r.a) && name_p != norm_name(&r.b) {
                        continue;
                    }
                    if protected_keys.contains(&p.key) {
                        continue;
                    }
                    for ki in 0..result.len() {
                        if ki == mi {
                            continue;
                        }
                        let dist = ki.abs_diff(other);
                        let fixes = dist >= r.gap;
                        let relieves = dist > cur_dist;
                        if !fixes && !relieves {
                            continue;
                        }
                        let (sum_from, sum_to) = (sum_wave(&result[mi]), sum_wave(&result[ki]));
                        for qi in 0..result[ki].len() {
                            let q = result[ki][qi].clone();
                            if protected_keys.contains(&q.key)
                                || !exclusion_ok(&result, mi, ki, &p, &q)
                            {
                                continue;
                            }
                            let before = (sum_from - mean).abs() + (sum_to - mean).abs();
                            let after = (sum_from - p.wave + q.wave - mean).abs()
                                + (sum_to - q.wave + p.wave - mean).abs();
                            let score = after - before;
                            if fixes {
                                if best.is_none_or(|(_, _, _, _, b)| score < b) {
                                    best = Some((mi, pi, ki, qi, score));
                                }
                            } else if dist > relief_dist
                                || (dist == relief_dist
                                    && relief.is_none_or(|(_, _, _, _, s)| score < s))
                            {
                                relief_dist = dist;
                                relief = Some((mi, pi, ki, qi, score));
                            }
                        }
                    }
                }
            }
        }

        // 优先一步到位；到不了就先拉远（规则内距离严格变大，必然收敛）
        let next = best.or(relief);
        let Some((mi, pi, ki, qi, _)) = next else {
            break;
        };
        let tmp = result[mi][pi].clone();
        result[mi][pi] = result[ki][qi].clone();
        result[ki][qi] = tmp;
    }
    result
}

#[derive(Clone, Debug, Default)]
pub struct PolishOptions {
    /// 不能移动的人（固定位）
    pub protected_keys: HashSet<String>,
    /// 互斥组：交换后不能让同一组里出现两个
    pub exclude_sets: Vec<Vec<String>>,
    /// 是否同时避免同门（师傅）扎堆
    pub avoid_master: bool,
    /// 最小间隔规则：交换后违约数不许增加
    pub min_gap_rules: Vec<MinGapRule>,
    pub max_iter: usize,
}

/// 均衡微调：反复做「最重组的轻物 ↔ 最轻组的重物」交换压极差。
/// 只接受能让极差变小的交换，且不移动固定位、不新增互斥/间隔冲突。
pub fn polish_balance(groups: &[Vec<Player>], options: &PolishOptions) -> Vec<Vec<Player>> {
    let set_list = norm_set(&options.exclude_sets);
    let mut result: Vec<Vec<Player>> = groups.to_vec();
    if result.len() < 2 {
        return result;
    }
    let base_gap_conflicts = gap_conflicts(&result, &options.min_gap_rules);
    let mut totals: Vec<f64> = result.iter().map(|g| sum_wave(g)).collect();

    /* ---- 全局玩家索引：循环内全部差量/整数下标，零字符串分配、零整组克隆 ---- */

    // gid[组][组内下标] → 全局下标（随交换同步维护）；pos_arr[全局下标] → 当前组号
    let mut gid: Vec<Vec<usize>> = Vec::with_capacity(result.len());
    let mut pos_arr: Vec<usize> = Vec::new();
    // in_set[全局下标][si]：是否属于第 si 个互斥组；leader_of[全局下标]：同门编号（-1 无）
    let mut in_set: Vec<Vec<bool>> = Vec::new();
    let mut leader_of: Vec<i64> = Vec::new();
    let mut is_protected: Vec<bool> = Vec::new();
    let mut idx_of: HashMap<String, usize> = HashMap::new();
    let mut leader_ids: HashMap<String, usize> = HashMap::new();
    for (gi, g) in result.iter().enumerate() {
        let mut row = Vec::with_capacity(g.len());
        for p in g {
            let n = norm_name(&p.name);
            let gidx = pos_arr.len();
            idx_of.insert(n.to_string(), gidx);
            row.push(gidx);
            pos_arr.push(gi);
            in_set.push(set_list.iter().map(|s| s.contains(n)).collect());
            let l = p.leader_name();
            let lid = if l.is_empty() {
                -1
            } else {
                let next = leader_ids.len();
                *leader_ids.entry(l.to_string()).or_insert(next) as i64
            };
            leader_of.push(lid);
            is_protected.push(options.protected_keys.contains(&p.key));
        }
        gid.push(row);
    }
    // min_gap 规则预解析成全局下标对（找不到人的规则永不违约，直接丢弃）
    let gap_rules: Vec<(usize, usize, usize)> = options
        .min_gap_rules
        .iter()
        .filter_map(|r| {
            Some((
                *idx_of.get(norm_name(&r.a))?,
                *idx_of.get(norm_name(&r.b))?,
                r.gap,
            ))
        })
        .collect();

    // 组 × 互斥组 计数表（交换时差量维护，试算 O(|sets|)）
    let mut set_cnt: Vec<Vec<usize>> = vec![vec![0; set_list.len()]; result.len()];
    for (gi, row) in gid.iter().enumerate() {
        for &gidx in row {
            for (si, hit) in in_set[gidx].iter().enumerate() {
                if *hit {
                    set_cnt[gi][si] += 1;
                }
            }
        }
    }

    // leader 计数（组 × 同门编号）+ 全局同门重复数 Σ max(0, cnt-1)
    let mut leader_cnt: Vec<Vec<usize>> = vec![vec![0; leader_ids.len()]; result.len()];
    let mut master_total = 0usize;
    if options.avoid_master {
        for (gi, row) in gid.iter().enumerate() {
            for &gidx in row {
                let lid = leader_of[gidx];
                if lid >= 0 {
                    leader_cnt[gi][lid as usize] += 1;
                }
            }
            master_total += leader_cnt[gi]
                .iter()
                .map(|c| c.saturating_sub(1))
                .sum::<usize>();
        }
    }
    let g2 = |c: i64| -> i64 { (c - 1).max(0) };
    // 交换 p(ai→bi)、q(bi→ai) 后的全局同门重复数（差量 O(1)，lp/lq 为同门编号）
    let master_after_total =
        |cnt: &[Vec<usize>], base: usize, ai: usize, bi: usize, lp: i64, lq: i64| -> i64 {
            let mut delta: i64 = 0;
            // (组, 出, 入)；-1 视作计数 0（无同门，出入都无增减）
            for (gi, lo, li) in [(ai, lp, lq), (bi, lq, lp)] {
                if lo == li {
                    continue; // 同桶出+入，计数净不变
                }
                let co = if lo < 0 {
                    0
                } else {
                    cnt[gi][lo as usize] as i64
                };
                let ci = if li < 0 {
                    0
                } else {
                    cnt[gi][li as usize] as i64
                };
                delta += g2(co - 1) - g2(co); // 出
                delta += g2(ci + 1) - g2(ci); // 入
            }
            base as i64 + delta
        };

    let mut current = totals.iter().cloned().fold(f64::MIN, f64::max)
        - totals.iter().cloned().fold(f64::MAX, f64::min);

    for _ in 0..options.max_iter {
        let mut best: Option<(usize, usize, usize, usize, f64)> = None;
        for ai in 0..result.len() {
            for bi in (ai + 1)..result.len() {
                for pi in 0..result[ai].len() {
                    let p = &result[ai][pi];
                    let ip = gid[ai][pi];
                    if is_protected[ip] {
                        continue;
                    }
                    let lp = leader_of[ip];
                    for qi in 0..result[bi].len() {
                        let q = &result[bi][qi];
                        let iq = gid[bi][qi];
                        if is_protected[iq] {
                            continue;
                        }
                        let lq = leader_of[iq];

                        // 互斥试算：查两人隶属位 + 组计数表，O(|sets|)
                        if !set_list.is_empty() {
                            let hp_flags = &in_set[ip];
                            let hq_flags = &in_set[iq];
                            let mut viol = 0usize;
                            for si in 0..set_list.len() {
                                let hp = usize::from(hp_flags[si]);
                                let hq = usize::from(hq_flags[si]);
                                let ca = set_cnt[ai][si] - hp + hq;
                                let cb = set_cnt[bi][si] - hq + hp;
                                viol += ca.saturating_sub(1) + cb.saturating_sub(1);
                            }
                            if viol > 0 {
                                continue;
                            }
                        }

                        // 间隔违约试算：pos_arr 原地改两条目再还原，零分配
                        let gap_ok = if gap_rules.is_empty() {
                            true
                        } else {
                            pos_arr[ip] = bi;
                            pos_arr[iq] = ai;
                            let ok = base_gap_conflicts
                                >= gap_rules
                                    .iter()
                                    .filter(|(a, b, gap)| pos_arr[*a].abs_diff(pos_arr[*b]) < *gap)
                                    .count();
                            pos_arr[ip] = ai;
                            pos_arr[iq] = bi;
                            ok
                        };
                        if !gap_ok {
                            continue;
                        }

                        // 极差（O(g)，避免每候选重算全量）
                        let a = totals[ai] - p.wave + q.wave;
                        let b = totals[bi] - q.wave + p.wave;
                        let mut mx = f64::MIN;
                        let mut mn = f64::MAX;
                        for (i, t) in totals.iter().enumerate() {
                            let v = if i == ai {
                                a
                            } else if i == bi {
                                b
                            } else {
                                *t
                            };
                            mx = mx.max(v);
                            mn = mn.min(v);
                        }
                        let spread = mx - mn;
                        let cost = spread
                            + if options.avoid_master {
                                master_after_total(&leader_cnt, master_total, ai, bi, lp, lq) as f64
                                    * 1e6
                            } else {
                                0.0
                            };
                        if best.is_none_or(|(_, _, _, _, c)| cost < c) {
                            best = Some((ai, bi, pi, qi, cost));
                        }
                    }
                }
            }
        }
        match best {
            Some((ai, bi, pi, qi, cost)) if cost < current - 1.0 => {
                let p = result[ai][pi].clone();
                let q = result[bi][qi].clone();
                // 同步索引结构（slot↔全局下标、组号、同门计数、互斥计数）
                let ip = gid[ai][pi];
                let iq = gid[bi][qi];
                pos_arr[ip] = bi;
                pos_arr[iq] = ai;
                gid[ai][pi] = iq;
                gid[bi][qi] = ip;
                let lp = leader_of[ip];
                let lq = leader_of[iq];
                if options.avoid_master {
                    // 与 master_after_total 的差量保持一致
                    let mut delta: i64 = 0;
                    for (gi, lo, li) in [(ai, lp, lq), (bi, lq, lp)] {
                        if lo == li {
                            continue;
                        }
                        if lo >= 0 {
                            let co = leader_cnt[gi][lo as usize];
                            delta += g2(co as i64 - 1) - g2(co as i64);
                            leader_cnt[gi][lo as usize] = co - 1;
                        }
                        if li >= 0 {
                            let ci = leader_cnt[gi][li as usize];
                            delta += g2(ci as i64 + 1) - g2(ci as i64);
                            leader_cnt[gi][li as usize] = ci + 1;
                        }
                    }
                    master_total = (master_total as i64 + delta) as usize;
                }
                for si in 0..set_list.len() {
                    let hp = usize::from(in_set[ip][si]);
                    let hq = usize::from(in_set[iq][si]);
                    set_cnt[ai][si] = set_cnt[ai][si] - hp + hq;
                    set_cnt[bi][si] = set_cnt[bi][si] - hq + hp;
                }
                result[ai][pi] = q.clone();
                result[bi][qi] = p.clone();
                // a 组：-p +q；b 组：-q +p（p/q 是交换前的局部克隆）
                totals[ai] += q.wave - p.wave;
                totals[bi] += p.wave - q.wave;
                current = cost;
            }
            _ => break,
        }
    }
    result
}

/* ------------------------------------------------------------------ *
 * 强度位次：强制条件（固定位 / 互斥 / 最小间隔）与均衡微调都处理完之后，
 * 再把各组实力摆成指定顺序（默认 3 最强 / 4 次强 / 1 最弱）。
 * 手段与前面一致——只做跨组两人对调，人数天然不变；
 * 绝不移动固定位、绝不新增互斥 / 间隔 / 同门冲突。
 * ------------------------------------------------------------------ */

#[derive(Clone, Debug, Default)]
pub struct StrengthOptions {
    /// 最强 → 最弱的组号（1 起）
    pub order: Vec<usize>,
    /// 排位次看什么：组总音浪 / 组内头牌单人
    pub mode: StrengthMode,
    /// 不能移动的人（固定位）
    pub protected_keys: HashSet<String>,
    pub exclude_sets: Vec<Vec<String>>,
    pub min_gap_rules: Vec<MinGapRule>,
    /// 是否同时避免同门扎堆（蛇形 + 同门回避策略用）
    pub avoid_master: bool,
    pub max_iter: usize,
}

/// 一组的「实力值」：集中模式看合计，每组一个最强模式看组内最高单人
pub fn group_strength(group: &[Player], mode: StrengthMode) -> f64 {
    match mode {
        StrengthMode::Total => sum_wave(group),
        StrengthMode::Top => group
            .iter()
            .map(|p| p.wave)
            .fold(0.0, |a: f64, b: f64| a.max(b)),
    }
}

/* ------------------------------------------------------------------ *
 * 种子平铺：每组各有一个头牌，且头牌之间按位次排——
 * 第 3 组的头牌是全场最强、第 4 组次之…第 1 组的头牌最弱。
 * 手段仍是「两人对调」：把强者换到他该去的组，换出来的那个人补到强者原来的组。
 * ------------------------------------------------------------------ */

#[derive(Clone, Debug, Default)]
pub struct SeedOptions {
    /// 最强 → 最弱的组号（1 起）
    pub order: Vec<usize>,
    /// 不能移动的人（固定位）
    pub protected_keys: HashSet<String>,
    pub exclude_sets: Vec<Vec<String>>,
    pub min_gap_rules: Vec<MinGapRule>,
}

fn total_conflicts(groups: &[Vec<Player>], sets: &[Vec<String>], rules: &[MinGapRule]) -> usize {
    let list = norm_set(sets);
    let mut n = 0usize;
    for g in groups {
        for s in &list {
            n += count_in(g, s).saturating_sub(1);
        }
    }
    n + gap_conflicts(groups, rules)
}

pub fn place_top_seeds(groups: &[Vec<Player>], options: &SeedOptions) -> Vec<Vec<Player>> {
    let mut result: Vec<Vec<Player>> = groups.to_vec();
    let gn = result.len();
    if gn < 2 {
        return result;
    }
    let mut order: Vec<usize> = Vec::new();
    for g in &options.order {
        let Some(gi) = g.checked_sub(1) else { continue };
        if gi < gn && !order.contains(&gi) {
            order.push(gi);
        }
    }
    if order.len() < 2 {
        return result;
    }

    // 头牌 = 全场音浪最高的前 N 个人（N = 组数），按位次表落到对应组
    let mut all: Vec<(f64, String)> = result
        .iter()
        .flat_map(|g| g.iter().map(|p| (p.wave, p.key.clone())))
        .collect();
    all.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let seeds: Vec<String> = all
        .into_iter()
        .take(gn.min(order.len()))
        .map(|(_, k)| k)
        .collect();
    let key_in = |result: &[Vec<Player>], key: &str| -> Option<(usize, usize)> {
        for (gi, g) in result.iter().enumerate() {
            if let Some(pi) = g.iter().position(|p| p.key == key) {
                return Some((gi, pi));
            }
        }
        None
    };
    let base_conf = total_conflicts(&result, &options.exclude_sets, &options.min_gap_rules);

    for (rank, seed_key) in seeds.iter().enumerate() {
        let target = order[rank];
        let Some((at_g, at_p)) = key_in(&result, seed_key) else {
            continue;
        };
        if at_g == target {
            continue;
        }
        if options.protected_keys.contains(seed_key) {
            continue; // 固定位的人不能被当作头牌挪走
        }
        // 目标组里挑一个换出来：优先「另一位头牌正好要来我这组」（互换乘袭），
        // 其次挑换完之后各组总音浪最平均的那个
        let mut best: Option<(bool, f64, usize)> = None; // (是否互换乘袭, 极差, 候选下标)
        let cand_count = result[target].len();
        for qi in 0..cand_count {
            let q_key = result[target][qi].key.clone();
            if options.protected_keys.contains(&q_key) {
                continue;
            }
            // 对方也是头牌时，只有「他的目标组正是我当前这组」才算互换乘袭；
            // 否则这一换会把别人的头牌拽走，越换越乱
            let q_rank = seeds.iter().position(|s| s == &q_key);
            let mutual = matches!(q_rank, Some(r) if order.get(r) == Some(&at_g));
            if q_rank.is_some() && !mutual {
                continue;
            }
            // 试换
            let tmp = result[at_g][at_p].clone();
            result[at_g][at_p] = result[target][qi].clone();
            result[target][qi] = tmp;
            let ok = total_conflicts(&result, &options.exclude_sets, &options.min_gap_rules)
                <= base_conf;
            if ok {
                let totals: Vec<f64> = result.iter().map(|g| sum_wave(g)).collect();
                let mx = totals.iter().cloned().fold(f64::MIN, f64::max);
                let mn = totals.iter().cloned().fold(f64::MAX, f64::min);
                let spread = mx - mn;
                if best.is_none_or(|(bm, bs, _)| (mutual && !bm) || (mutual == bm && spread < bs)) {
                    best = Some((mutual, spread, qi));
                }
            }
            // 换回来
            let tmp = result[at_g][at_p].clone();
            result[at_g][at_p] = result[target][qi].clone();
            result[target][qi] = tmp;
        }
        let Some((_, _, qi)) = best else { continue };
        let tmp = result[at_g][at_p].clone();
        result[at_g][at_p] = result[target][qi].clone();
        result[target][qi] = tmp;
    }

    result
}

/// 位次惩罚：相邻位次里「弱的反而比强的还高」的缺口求和，0 = 完全达成。
/// ai/ta、bi/tb 是「试算中」被改写的两组；不试算时传 (usize::MAX, 0.0)。
fn rank_penalty(totals: &[f64], order: &[usize], ai: usize, ta: f64, bi: usize, tb: f64) -> f64 {
    let mut pen = 0.0;
    for w in order.windows(2) {
        let strong = if w[0] == ai {
            ta
        } else if w[0] == bi {
            tb
        } else {
            totals[w[0]]
        };
        let weak = if w[1] == ai {
            ta
        } else if w[1] == bi {
            tb
        } else {
            totals[w[1]]
        };
        // 音浪是整数，留 1 的余量才算「严格更强」，否则并列会反复横跳
        let d = weak - strong + 1.0;
        if d > 0.0 {
            pen += d;
        }
    }
    pen
}

/// 极差：位次达成的前提下，尽量别把组间差距拉得太夸张
fn spread_with(totals: &[f64], ai: usize, ta: f64, bi: usize, tb: f64) -> f64 {
    let mut mx = f64::NEG_INFINITY;
    let mut mn = f64::INFINITY;
    for (i, t) in totals.iter().enumerate() {
        let v = if i == ai {
            ta
        } else if i == bi {
            tb
        } else {
            *t
        };
        if v > mx {
            mx = v;
        }
        if v < mn {
            mn = v;
        }
    }
    mx - mn
}

/// 交换 p(ai→bi)、q(bi→ai) 后的全局同门重复数（差量 O(1)，lp/lq 为同门编号）
fn master_after(cnt: &[Vec<usize>], base: usize, ai: usize, bi: usize, lp: i64, lq: i64) -> i64 {
    let g2 = |c: i64| -> i64 { (c - 1).max(0) };
    let mut delta: i64 = 0;
    for (gi, lo, li) in [(ai, lp, lq), (bi, lq, lp)] {
        if lo == li {
            continue; // 同桶出 + 入，计数净不变
        }
        let co = if lo < 0 {
            0
        } else {
            cnt[gi][lo as usize] as i64
        };
        let ci = if li < 0 {
            0
        } else {
            cnt[gi][li as usize] as i64
        };
        delta += g2(co - 1) - g2(co); // 出
        delta += g2(ci + 1) - g2(ci); // 入
    }
    base as i64 + delta
}

pub fn order_by_strength(groups: &[Vec<Player>], options: &StrengthOptions) -> Vec<Vec<Player>> {
    let mut result: Vec<Vec<Player>> = groups.to_vec();
    if result.len() < 2 {
        return result;
    }
    // 位次 → 0 起；越界或重复的组号直接丢掉
    let mut order: Vec<usize> = Vec::new();
    for g in &options.order {
        let Some(gi) = g.checked_sub(1) else { continue };
        if gi < result.len() && !order.contains(&gi) {
            order.push(gi);
        }
    }
    if order.len() < 2 {
        return result;
    }

    let set_list = norm_set(&options.exclude_sets);
    let gn = result.len();

    /* ---- 全局玩家索引：循环内全整数运算 ---- */
    let mut gid: Vec<Vec<usize>> = Vec::with_capacity(gn);
    let mut pos: Vec<usize> = Vec::new();
    let mut wv: Vec<f64> = Vec::new();
    let mut in_set: Vec<Vec<bool>> = Vec::new();
    let mut is_protected: Vec<bool> = Vec::new();
    let mut leader_of: Vec<i64> = Vec::new();
    let mut idx_of: HashMap<String, usize> = HashMap::new();
    let mut leader_ids: HashMap<String, usize> = HashMap::new();
    for (gi, g) in result.iter().enumerate() {
        let mut row = Vec::with_capacity(g.len());
        for p in g {
            let n = norm_name(&p.name);
            let x = pos.len();
            idx_of.insert(n.to_string(), x);
            row.push(x);
            pos.push(gi);
            wv.push(p.wave);
            in_set.push(set_list.iter().map(|s| s.contains(n)).collect());
            let l = p.leader_name();
            let lid = if l.is_empty() {
                -1
            } else {
                let next = leader_ids.len();
                *leader_ids.entry(l.to_string()).or_insert(next) as i64
            };
            leader_of.push(lid);
            is_protected.push(options.protected_keys.contains(&p.key));
        }
        gid.push(row);
    }

    let mut set_cnt: Vec<Vec<usize>> = vec![vec![0; set_list.len()]; gn];
    for (gi, row) in gid.iter().enumerate() {
        for &x in row {
            for (si, hit) in in_set[x].iter().enumerate() {
                if *hit {
                    set_cnt[gi][si] += 1;
                }
            }
        }
    }
    let mut leader_cnt: Vec<Vec<usize>> = vec![vec![0; leader_ids.len()]; gn];
    let mut master_total = 0usize;
    if options.avoid_master {
        for (gi, row) in gid.iter().enumerate() {
            for &x in row {
                let lid = leader_of[x];
                if lid >= 0 {
                    leader_cnt[gi][lid as usize] += 1;
                }
            }
            master_total += leader_cnt[gi]
                .iter()
                .map(|c| c.saturating_sub(1))
                .sum::<usize>();
        }
    }
    let gap_rules: Vec<(usize, usize, usize)> = options
        .min_gap_rules
        .iter()
        .filter_map(|r| {
            Some((
                *idx_of.get(norm_name(&r.a))?,
                *idx_of.get(norm_name(&r.b))?,
                r.gap,
            ))
        })
        .collect();
    let base_gap = gap_conflicts(&result, &options.min_gap_rules);

    let mut totals: Vec<f64> = result
        .iter()
        .map(|g| group_strength(g, options.mode))
        .collect();
    // Top 模式下位次看「组内最高单人」，交换要能 O(1) 算出新的头牌：
    // 记下每组的最高值与次高值，被换走的那个人正好是唯一最高时才降到次高
    let mut top1: Vec<f64> = vec![0.0; gn];
    let mut cnt1: Vec<usize> = vec![0; gn];
    let mut top2: Vec<f64> = vec![0.0; gn];
    let refresh_peaks = |result: &[Vec<Player>],
                         gi: usize,
                         top1: &mut Vec<f64>,
                         cnt1: &mut Vec<usize>,
                         top2: &mut Vec<f64>| {
        let mut a = 0.0f64;
        let mut ca = 0usize;
        let mut b = 0.0f64;
        for p in &result[gi] {
            if p.wave > a {
                b = a;
                a = p.wave;
                ca = 1;
            } else if (p.wave - a).abs() < f64::EPSILON {
                ca += 1;
            } else if p.wave > b {
                b = p.wave;
            }
        }
        top1[gi] = a;
        cnt1[gi] = ca;
        top2[gi] = b;
    };
    if options.mode == StrengthMode::Top {
        for gi in 0..gn {
            refresh_peaks(&result, gi, &mut top1, &mut cnt1, &mut top2);
        }
    }
    let sentinel = (usize::MAX, 0.0);

    for _ in 0..options.max_iter.max(1) {
        let cur_pen = rank_penalty(
            &totals, &order, sentinel.0, sentinel.1, sentinel.0, sentinel.1,
        );
        let cur_spr = spread_with(&totals, sentinel.0, sentinel.1, sentinel.0, sentinel.1);
        if cur_pen <= 0.0 {
            break; // 位次已达成
        }
        let mut best: Option<(f64, f64, usize, usize, usize, usize)> = None;
        let better = |pen: f64, spr: f64, bp: f64, bs: f64| {
            pen < bp - 1e-9 || (pen <= bp + 1e-9 && spr < bs - 1e-9)
        };

        for ai in 0..gn {
            for bi in (ai + 1)..gn {
                let ta0 = totals[ai];
                let tb0 = totals[bi];
                for (pi, &p) in gid[ai].iter().enumerate() {
                    if is_protected[p] {
                        continue;
                    }
                    for (qi, &q) in gid[bi].iter().enumerate() {
                        if is_protected[q] {
                            continue;
                        }
                        // 互斥：交换后两组里同一个互斥组的人数都不能 > 1
                        let mut ok = true;
                        for si in 0..set_list.len() {
                            let hp = usize::from(in_set[p][si]);
                            let hq = usize::from(in_set[q][si]);
                            if hp == 0 && hq == 0 {
                                continue;
                            }
                            if set_cnt[ai][si] - hp + hq > 1 || set_cnt[bi][si] - hq + hp > 1 {
                                ok = false;
                                break;
                            }
                        }
                        if !ok {
                            continue;
                        }
                        // 最小间隔：违约数不许增加
                        if !gap_rules.is_empty() {
                            let mut v = 0usize;
                            for &(x, y, gap) in &gap_rules {
                                let px = if x == p {
                                    bi
                                } else if x == q {
                                    ai
                                } else {
                                    pos[x]
                                };
                                let py = if y == p {
                                    bi
                                } else if y == q {
                                    ai
                                } else {
                                    pos[y]
                                };
                                if px.abs_diff(py) < gap {
                                    v += 1;
                                }
                            }
                            if v > base_gap {
                                continue;
                            }
                        }
                        // 同门：扎堆数不许增加
                        if options.avoid_master
                            && master_after(
                                &leader_cnt,
                                master_total,
                                ai,
                                bi,
                                leader_of[p],
                                leader_of[q],
                            ) > master_total as i64
                        {
                            continue;
                        }

                        let d = wv[q] - wv[p];
                        let (ta, tb) = if options.mode == StrengthMode::Top {
                            // 新的组内最高：p 被换走后可能降到次高，再和进来的 q 比
                            let na = if cnt1[ai] == 1 && (wv[p] - top1[ai]).abs() < f64::EPSILON {
                                top2[ai]
                            } else {
                                top1[ai]
                            }
                            .max(wv[q]);
                            let nb = if cnt1[bi] == 1 && (wv[q] - top1[bi]).abs() < f64::EPSILON {
                                top2[bi]
                            } else {
                                top1[bi]
                            }
                            .max(wv[p]);
                            (na, nb)
                        } else {
                            (ta0 + d, tb0 - d)
                        };
                        let pen = rank_penalty(&totals, &order, ai, ta, bi, tb);
                        let spr = spread_with(&totals, ai, ta, bi, tb);
                        // 字典序：先把位次惩罚压到 0，再尽量别把极差拉大
                        if !better(pen, spr, cur_pen, cur_spr) {
                            continue;
                        }
                        if best.is_none_or(|(bp, bs, _, _, _, _)| better(pen, spr, bp, bs)) {
                            best = Some((pen, spr, ai, pi, bi, qi));
                        }
                    }
                }
            }
        }

        let Some((_, _, ai, pi, bi, qi)) = best else {
            break;
        };
        let p = gid[ai][pi];
        let q = gid[bi][qi];

        let tmp = result[ai][pi].clone();
        result[ai][pi] = result[bi][qi].clone();
        result[bi][qi] = tmp;
        gid[ai][pi] = q;
        gid[bi][qi] = p;
        pos[p] = bi;
        pos[q] = ai;
        for si in 0..set_list.len() {
            let hp = usize::from(in_set[p][si]);
            let hq = usize::from(in_set[q][si]);
            if hp != 0 || hq != 0 {
                set_cnt[ai][si] = set_cnt[ai][si] - hp + hq;
                set_cnt[bi][si] = set_cnt[bi][si] - hq + hp;
            }
        }
        if options.avoid_master {
            for (gi, lo, li) in [
                (ai, leader_of[p], leader_of[q]),
                (bi, leader_of[q], leader_of[p]),
            ] {
                if lo >= 0 {
                    leader_cnt[gi][lo as usize] = leader_cnt[gi][lo as usize].saturating_sub(1);
                }
                if li >= 0 {
                    leader_cnt[gi][li as usize] += 1;
                }
            }
            master_total = leader_cnt
                .iter()
                .map(|r| r.iter().map(|c| c.saturating_sub(1)).sum::<usize>())
                .sum();
        }
        totals[ai] += wv[q] - wv[p];
        totals[bi] += wv[p] - wv[q];
        if options.mode == StrengthMode::Top {
            totals[ai] = group_strength(&result[ai], options.mode);
            totals[bi] = group_strength(&result[bi], options.mode);
            refresh_peaks(&result, ai, &mut top1, &mut cnt1, &mut top2);
            refresh_peaks(&result, bi, &mut top1, &mut cnt1, &mut top2);
        }
    }

    result
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Violation {
    /// exclude | pin | gap
    pub kind: String,
    pub group_index: usize,
    pub message: String,
    /// 涉及的人名，用于卡片高亮
    pub names: Vec<String>,
}

/// 检查当前分组有没有违反内置条件（手动拖拽后用来告警）
pub fn find_violations(groups: &[Vec<Player>], cs: &ConstraintSet) -> Vec<Violation> {
    let mut out = Vec::new();

    for set in &cs.exclude_together {
        let s: HashSet<&str> = set.iter().map(|x| norm_name(x)).collect();
        for (i, g) in groups.iter().enumerate() {
            let hit: Vec<&Player> = g
                .iter()
                .filter(|p| s.contains(norm_name(&p.name)))
                .collect();
            if hit.len() > 1 {
                let names: Vec<String> = hit.iter().map(|p| p.name.clone()).collect();
                out.push(Violation {
                    kind: "exclude".into(),
                    group_index: i,
                    message: format!("第 {} 组：{} 不能同组", i + 1, names.join("、")),
                    names,
                });
            }
        }
    }

    for rule in &cs.pinned {
        let target = resolve_pinned_index(rule, groups.len());
        let label = match &rule.group {
            PinnedGroup::Last(_) => "最后一组".to_string(),
            PinnedGroup::Index(n) => format!("第 {n} 组"),
        };
        match group_index_of_name(groups, &rule.name) {
            // 名单里没有此人 → 约束自动跳过（内置条件只对名单里存在的人生效）
            None => continue,
            Some(at) if at != target => out.push(Violation {
                kind: "pin".into(),
                group_index: at,
                message: format!("{} 应固定在{}，当前在第 {} 组", rule.name, label, at + 1),
                names: vec![rule.name.clone()],
            }),
            _ => {}
        }
    }

    for rule in &cs.min_gap {
        let (Some(ia), Some(ib)) = (
            group_index_of_name(groups, &rule.a),
            group_index_of_name(groups, &rule.b),
        ) else {
            continue;
        };
        if ia.abs_diff(ib) >= rule.gap {
            continue;
        }
        out.push(Violation {
            kind: "gap".into(),
            group_index: ia.min(ib),
            message: format!(
                "{}（第 {} 组）和 {}（第 {} 组）需至少间隔 {} 组",
                rule.a,
                ia + 1,
                rule.b,
                ib + 1,
                rule.gap
            ),
            names: vec![rule.a.clone(), rule.b.clone()],
        });
    }

    for rule in &cs.min_group {
        let Some(at) = group_index_of_name(groups, &rule.name) else {
            // 名单里没有此人 → 约束自动跳过
            continue;
        };
        if at + 1 < rule.group as usize {
            out.push(Violation {
                kind: "pin".into(),
                group_index: at,
                message: format!(
                    "{} 必须在第 {} 组或更后，当前在第 {} 组",
                    rule.name,
                    rule.group,
                    at + 1
                ),
                names: vec![rule.name.clone()],
            });
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn players(items: &[(&str, f64)]) -> Vec<Player> {
        items.iter().map(|(n, w)| Player::new(*n, *n, *w)).collect()
    }

    #[test]
    fn slot_times() {
        let cs = ConstraintSet::default();
        assert_eq!(slot_time(0, &cs.schedule), "12:15");
        assert_eq!(slot_time(1, &cs.schedule), "12:30");
        assert_eq!(slot_time(3, &cs.schedule), "13:00");
    }

    #[test]
    fn pinned_index_resolves_last() {
        let rule = PinnedRule {
            name: "啸帆".into(),
            group: PinnedGroup::last(),
        };
        assert_eq!(resolve_pinned_index(&rule, 7), 6);
        let first = PinnedRule {
            name: "啸泽".into(),
            group: PinnedGroup::Index(1),
        };
        assert_eq!(resolve_pinned_index(&first, 7), 0);
    }

    #[test]
    fn missing_person_skips_constraints() {
        // 名单里没有固定位/间隔条件里的人 → 约束自动跳过，不产生冲突告警
        // （2026-10-07：任意名单不应被内置条件报红）
        let groups = vec![players(&[("南方楠", 100.0), ("玖玉", 90.0)])];
        let v = find_violations(&groups, &ConstraintSet::default());
        assert!(v.is_empty(), "内置条件的人不在名单里不应报冲突：{v:?}");
    }

    #[test]
    fn excludes_are_repaired_without_moving_pinned() {
        // a1 与 a2 互斥，被排进同一组；b 是固定位
        let list = players(&[
            ("玖玉", 100.0),
            ("玖妹", 90.0),
            ("玖玥", 80.0),
            ("啸泽", 70.0),
            ("狼兴", 60.0),
        ]);
        let groups = vec![
            vec![list[0].clone(), list[1].clone()],
            vec![list[2].clone(), list[3].clone(), list[4].clone()],
        ];
        let mut protected = HashSet::new();
        protected.insert("啸泽".to_string());
        let fixed = resolve_exclusions(
            &groups,
            &[vec!["玖玉".into(), "玖妹".into(), "玖玥".into()]],
            &protected,
            100,
        );
        assert_eq!(gap_conflicts(&fixed, &[]), 0);
        for g in &fixed {
            let mut seen = HashSet::new();
            for p in g {
                assert!(seen.insert(p.name.clone()), "玖玉/玖妹仍同组");
            }
        }
        // 固定位没被动
        assert_eq!(group_index_of_name(&fixed, "啸泽"), Some(1));
    }

    #[test]
    fn min_gap_is_repaired_by_swap() {
        // 啸昭与啸寻同在第 1 组（组号差 0 < 4），第 5 组有人可换
        let a = Player::new("啸昭", "啸昭", 90.0);
        let b = Player::new("啸寻", "啸寻", 80.0);
        let keep = Player::new("填充", "填充", 10.0);
        let q = Player::new("对手", "对手", 20.0);
        let groups = vec![
            vec![a.clone(), b.clone(), keep.clone()],
            vec![],
            vec![],
            vec![],
            vec![q.clone()],
        ];
        let fixed = resolve_min_gaps(
            &groups,
            &[MinGapRule {
                a: "啸昭".into(),
                b: "啸寻".into(),
                gap: 4,
            }],
            &HashSet::new(),
            &[],
            50,
        );
        assert_eq!(
            gap_conflicts(
                &fixed,
                &[MinGapRule {
                    a: "啸昭".into(),
                    b: "啸寻".into(),
                    gap: 4
                }]
            ),
            0
        );
        let at = |n: &str| group_index_of_name(&fixed, n).unwrap();
        assert_eq!(at("啸昭"), 4);
        assert_eq!(at("啸寻"), 0);
    }

    #[test]
    fn min_gap_fixes_middle_stuck_case_via_relief() {
        // 6 组、间隔 4：甲乙都被挤在中间组（下标 2/3）时，任何单次交换都无法
        // 直接满足「距离 ≥ 4」，必须靠「先拉远再满足」的兜底策略
        let mut groups: Vec<Vec<Player>> = vec![Vec::new(); 6];
        for (i, g) in groups.iter_mut().enumerate() {
            for j in 0..4 {
                let n = format!("普通{}{}", i, j);
                g.push(Player::new(&n, &n, 100.0));
            }
        }
        groups[2].push(Player::new("甲", "甲", 90.0));
        groups[3].push(Player::new("乙", "乙", 80.0));
        let rules = vec![MinGapRule {
            a: "甲".into(),
            b: "乙".into(),
            gap: 4,
        }];
        let fixed = resolve_min_gaps(&groups, &rules, &HashSet::new(), &[], 200);
        assert_eq!(gap_conflicts(&fixed, &rules), 0, "拉远兜底后必须清零违约");
    }

    #[test]
    fn default_order_is_three_strongest_one_weakest() {
        assert_eq!(default_strength_order(6), vec![3, 4, 5, 6, 2, 1]);
        assert_eq!(default_strength_order(3), vec![3, 2, 1]);
        assert_eq!(default_strength_order(2), vec![1, 2]);
        assert_eq!(default_strength_order(13).first(), Some(&3));
        assert_eq!(default_strength_order(13).last(), Some(&1));
    }

    #[test]
    fn strength_order_lands_on_target_ranks() {
        // 6 组 × 4 人，初始各组音浪故意乱序；目标 3>4>5>6>2>1
        let mut groups: Vec<Vec<Player>> = Vec::new();
        let waves = [10.0, 60.0, 30.0, 40.0, 50.0, 20.0]; // 组 i 的基准
        for (i, w) in waves.iter().enumerate() {
            let mut g = Vec::new();
            for j in 0..4 {
                let n = format!("人{}{}", i, j);
                g.push(Player::new(&n, &n, w * 100.0 + j as f64));
            }
            groups.push(g);
        }
        let out = order_by_strength(
            &groups,
            &StrengthOptions {
                order: default_strength_order(6),
                mode: StrengthMode::Total,
                max_iter: 400,
                ..Default::default()
            },
        );
        let totals: Vec<f64> = out.iter().map(|g| sum_wave(g)).collect();
        let want = [2, 3, 4, 5, 1, 0]; // 位次 3,4,5,6,2,1 → 0 起下标
        for w in want.windows(2) {
            assert!(
                totals[w[0]] > totals[w[1]],
                "位次未达成：{:?}（目标 3>4>5>6>2>1）",
                totals
            );
        }
        // 人数不变
        for (i, g) in out.iter().enumerate() {
            assert_eq!(g.len(), groups[i].len());
        }
    }

    #[test]
    fn top_seeds_land_one_per_group_in_rank_order() {
        // 6 组，每组 3 个弱者；前 6 名强者一开始全挤在前两组里
        let mut groups: Vec<Vec<Player>> = Vec::new();
        for i in 0..6 {
            let mut g = Vec::new();
            for j in 0..3 {
                let n = format!("弱{}{}", i, j);
                g.push(Player::new(&n, &n, 10.0 + j as f64));
            }
            groups.push(g);
        }
        for k in 0..6 {
            let strong = Player::new(
                format!("强{}", k),
                format!("强{}", k),
                1000.0 - k as f64 * 100.0,
            );
            groups[k / 3].push(strong); // 强 0..2 挤第 1 组，强 3..5 挤第 2 组
        }
        let out = place_top_seeds(
            &groups,
            &SeedOptions {
                order: default_strength_order(6),
                ..Default::default()
            },
        );
        // 每组恰好一个强者，且强者之间按位次 3>4>5>6>2>1 排
        let peak: Vec<f64> = out
            .iter()
            .map(|g| group_strength(g, StrengthMode::Top))
            .collect();
        let want = [2, 3, 4, 5, 1, 0];
        for w in want.windows(2) {
            assert!(peak[w[0]] > peak[w[1]], "头牌位次未达成：{peak:?}");
        }
        for (i, g) in out.iter().enumerate() {
            let top_count = g.iter().filter(|p| p.wave >= 500.0).count();
            assert_eq!(
                top_count,
                1,
                "第 {} 组应有且只有一个强者：{:?}",
                i + 1,
                g.iter().map(|p| p.name.clone()).collect::<Vec<_>>()
            );
        }
        for (i, g) in out.iter().enumerate() {
            assert_eq!(g.len(), groups[i].len(), "人数不能变");
        }
    }

    #[test]
    fn long_break_stretches_slot_between_groups() {
        let mut cs = ConstraintSet::default();
        assert_eq!(slot_time(0, &cs.schedule), "12:15");
        assert_eq!(slot_time(2, &cs.schedule), "12:45");
        // 第 3 组后加长到 20 分钟：前两组仍是 15 分钟节奏，第 4 组起整体 +5
        cs.schedule.breaks = vec![BreakRule {
            after_group: 3,
            minutes: 20,
        }];
        assert_eq!(slot_time(2, &cs.schedule), "12:45"); // 第 3 组不受影响
        assert_eq!(slot_time(3, &cs.schedule), "13:05"); // 12:45 + 20
        assert_eq!(slot_time(4, &cs.schedule), "13:20"); // 之后回到 15
        assert_eq!(gap_after(2, &cs.schedule), 20);
        assert_eq!(gap_after(1, &cs.schedule), 15);
    }

    #[test]
    fn strength_order_never_moves_pinned() {
        // 啸泽固定在第 1 组：位次只能靠别人腾挪，绝不能把他换走
        let base: [f64; 4] = [10.0, 300.0, 100.0, 200.0];
        let mut groups: Vec<Vec<Player>> = Vec::new();
        for (i, w) in base.iter().enumerate() {
            let mut g = Vec::new();
            for j in 0..3 {
                let n = format!("普通{}{}", i, j);
                g.push(Player::new(&n, &n, *w));
            }
            groups.push(g);
        }
        let pinned = Player::new("啸泽", "啸泽", 10.0);
        groups[0].push(pinned.clone());
        let out = order_by_strength(
            &groups,
            &StrengthOptions {
                order: vec![3, 4, 2, 1],
                mode: StrengthMode::Total,
                protected_keys: HashSet::from([pinned.key.clone()]),
                max_iter: 400,
                ..Default::default()
            },
        );
        let at = out
            .iter()
            .position(|g| g.iter().any(|p| p.key == pinned.key));
        assert_eq!(at, Some(0), "固定位的人不该被强度位次挪走");
        let totals: Vec<f64> = out.iter().map(|g| sum_wave(g)).collect();
        // 目标：第 3 组 > 第 4 组 > 第 2 组 > 第 1 组
        assert!(
            totals[2] > totals[3] && totals[3] > totals[1] && totals[1] > totals[0],
            "位次未达成：{totals:?}"
        );
    }
}
