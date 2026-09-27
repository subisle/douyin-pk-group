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

/// 开赛时间：第 1 组 start 开始，之后每组顺延 interval_min 分钟
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRule {
    /// "08:15"
    pub start: String,
    pub interval_min: i64,
}

/// 最小间隔：两人所在组号之差必须 ≥ gap（组数不足时约束自动放宽跳过）
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MinGapRule {
    pub a: String,
    pub b: String,
    pub gap: usize,
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
}

impl Default for ConstraintSet {
    fn default() -> Self {
        Self {
            exclude_together: vec![vec!["玖玉".into(), "玖妹".into(), "玖玥".into()]],
            pinned: vec![
                PinnedRule {
                    name: "啸泽".into(),
                    group: PinnedGroup::Index(1),
                },
                PinnedRule {
                    name: "啸帆".into(),
                    group: PinnedGroup::last(),
                },
            ],
            schedule: ScheduleRule {
                start: "08:15".into(),
                interval_min: 15,
            },
            min_gap: vec![MinGapRule {
                a: "啸昭".into(),
                b: "啸寻".into(),
                gap: 4,
            }],
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
    let total = h * 60 + m + index as i64 * schedule.interval_min.max(0);
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

/// 最小间隔修复：对违约的两人，把其中非固定位的一人换到满足间隔的组，
/// 换入者选择对音浪均衡损伤最小、且不制造互斥冲突的对象。
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
        let mut best: Option<(usize, usize, usize, usize, f64)> = None;

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
                        if ki == mi || ki.abs_diff(other) < r.gap {
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
                            if best.is_none_or(|(_, _, _, _, b)| score < b) {
                                best = Some((mi, pi, ki, qi, score));
                            }
                        }
                    }
                }
            }
        }

        let Some((gi, pi, ki, qi, _)) = best else {
            break;
        };
        let tmp = result[gi][pi].clone();
        result[gi][pi] = result[ki][qi].clone();
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

    // 交换后两组内各互斥组的人数是否超标
    let conflicts_after =
        |res: &[Vec<Player>], ai: usize, bi: usize, p: &Player, q: &Player| -> usize {
            set_list
                .iter()
                .map(|s| {
                    let hp = usize::from(s.contains(norm_name(&p.name)));
                    let hq = usize::from(s.contains(norm_name(&q.name)));
                    let ca = count_in(&res[ai], s) as f64 - hp as f64 + hq as f64;
                    let cb = count_in(&res[bi], s) as f64 - hq as f64 + hp as f64;
                    (ca - 1.0).max(0.0) as usize + (cb - 1.0).max(0.0) as usize
                })
                .sum()
        };

    // 交换后两组的同门重复数
    let master_after =
        |res: &[Vec<Player>], ai: usize, bi: usize, p: &Player, q: &Player| -> usize {
            let count = |arr: &[Player], swap_out: &Player, swap_in: &Player| -> usize {
                let mut seen: HashMap<&str, usize> = HashMap::new();
                for x in arr {
                    let l = x.leader_name();
                    if !l.is_empty() {
                        *seen.entry(l).or_default() += 1;
                    }
                }
                for (x, delta) in [(swap_out, -1i64), (swap_in, 1i64)] {
                    let l = x.leader_name();
                    if !l.is_empty() {
                        let slot = seen.entry(l).or_default();
                        *slot = (*slot as i64 + delta).max(0) as usize;
                    }
                }
                seen.values().map(|c| c.saturating_sub(1)).sum::<usize>()
            };
            count(&res[ai], p, q) + count(&res[bi], q, p)
        };

    let mut current = totals.iter().cloned().fold(f64::MIN, f64::max)
        - totals.iter().cloned().fold(f64::MAX, f64::min);

    for _ in 0..options.max_iter {
        let mut best: Option<(usize, usize, usize, usize, f64)> = None;
        for ai in 0..result.len() {
            for bi in (ai + 1)..result.len() {
                for pi in 0..result[ai].len() {
                    let p = result[ai][pi].clone();
                    if options.protected_keys.contains(&p.key) {
                        continue;
                    }
                    for qi in 0..result[bi].len() {
                        let q = result[bi][qi].clone();
                        if options.protected_keys.contains(&q.key) {
                            continue;
                        }
                        if !set_list.is_empty() && conflicts_after(&result, ai, bi, &p, &q) > 0 {
                            continue;
                        }
                        if !options.min_gap_rules.is_empty() {
                            // 模拟交换后间隔违约数不许增加
                            let mut sim = result.clone();
                            sim[ai][pi] = q.clone();
                            sim[bi][qi] = p.clone();
                            if gap_conflicts(&sim, &options.min_gap_rules) > base_gap_conflicts {
                                continue;
                            }
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
                                master_after(&result, ai, bi, &p, &q) as f64 * 1e6
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
                result[ai][pi] = q.clone();
                result[bi][qi] = p.clone();
                totals[ai] += q.wave - p.wave;
                totals[bi] += p.wave - q.wave;
                current = cost;
            }
            _ => break,
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
            None => out.push(Violation {
                kind: "pin".into(),
                group_index: target,
                message: format!("名单里没有找到「{}」（应固定在{}）", rule.name, label),
                names: vec![rule.name.clone()],
            }),
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
        assert_eq!(slot_time(0, &cs.schedule), "08:15");
        assert_eq!(slot_time(1, &cs.schedule), "08:30");
        assert_eq!(slot_time(3, &cs.schedule), "09:00");
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
}
