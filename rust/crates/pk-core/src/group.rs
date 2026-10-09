//! 分组策略：蛇形均衡 / 梯队式 / 随机，以及带内置条件的自动分组

use std::collections::HashSet;

use crate::constraints::{
    default_strength_order, gap_conflicts, norm_name, order_by_strength, place_pinned,
    place_top_seeds, polish_balance, resolve_exclusions, resolve_min_gaps, resolve_pinned_index,
    sum_wave, ConstraintSet, PolishOptions, SeedOptions, StrengthMode, StrengthOptions,
};
use crate::types::{BalanceStat, GroupOptions, GroupStat, Player, Strategy};

pub fn group_count(total: usize, group_size: usize) -> usize {
    total.div_ceil(group_size.max(1)).max(1)
}

/// 音浪降序；音浪相同时按名字（TS 版用 zh-Hans 排序，Rust 用码位序近似）
fn by_wave_desc(a: &Player, b: &Player) -> std::cmp::Ordering {
    b.wave
        .partial_cmp(&a.wave)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| a.name.cmp(&b.name))
}

fn sorted_desc(players: &[Player]) -> Vec<Player> {
    let mut v = players.to_vec();
    v.sort_by(by_wave_desc);
    v
}

fn cut_into(sorted: &[Player], groups: usize) -> Vec<Vec<Player>> {
    let mut out: Vec<Vec<Player>> = Vec::with_capacity(groups);
    let base = sorted.len() / groups;
    let extra = sorted.len() % groups;
    let mut cursor = 0;
    for i in 0..groups {
        let size = base + usize::from(i < extra);
        out.push(sorted[cursor..cursor + size].to_vec());
        cursor += size;
    }
    out
}

/// 蛇形（S 型折返）均衡分组：按音浪降序，奇数轮反向分配，
/// 使各组总音浪自然收敛。第 1 名与第 14 名同组、第 2 名与第 13 名同组……
pub fn snake_group(players: &[Player], group_size: usize) -> Vec<Vec<Player>> {
    let sorted = sorted_desc(players);
    let g = group_count(sorted.len(), group_size);
    snake_deal(&sorted, g)
}

/// 蛇形分配到**指定组数**（auto_group 用：组数必须按总人数算，
/// 不能按抽出固定位后的剩余人数算——后者恰好整除时会铺不满 g 组，
/// 空组最后只接得住固定位挤出来的溢出人员，出现 8×11+3 式失衡）。
fn snake_deal(sorted: &[Player], groups: usize) -> Vec<Vec<Player>> {
    let g = groups.max(1);
    let mut out: Vec<Vec<Player>> = vec![Vec::new(); g];
    for (i, p) in sorted.iter().enumerate() {
        let round = i / g;
        let idx = i % g;
        let target = if round % 2 == 0 { idx } else { g - 1 - idx };
        out[target].push(p.clone());
    }
    out
}

/// 梯队式：按音浪降序依次切组，第 1 组全是最高的一档，最后一组最低。
/// 不做均衡——各组的强弱本身就是阶梯。
pub fn sequential_group(players: &[Player], group_size: usize) -> Vec<Vec<Player>> {
    let sorted = sorted_desc(players);
    sorted
        .chunks(group_size.max(1))
        .map(<[Player]>::to_vec)
        .collect()
}

/// 随机洗牌后再切组；给 seed 可复现
pub fn random_group(players: &[Player], group_size: usize, seed: Option<u64>) -> Vec<Vec<Player>> {
    use rand::seq::SliceRandom;
    let mut arr = players.to_vec();
    match seed {
        Some(s) => {
            use rand::SeedableRng;
            let mut rng = rand::rngs::StdRng::seed_from_u64(s);
            arr.shuffle(&mut rng);
        }
        None => arr.shuffle(&mut rand::thread_rng()),
    }
    arr.sort_by(by_wave_desc);
    let g = group_count(arr.len(), group_size);
    cut_into(&arr, g)
}

/// 同门回避：局部交换，把同队长的人拆到不同组，交换时尽量少破坏音浪均衡
pub fn resolve_master_conflicts(groups: &[Vec<Player>], max_iter: usize) -> Vec<Vec<Player>> {
    let leader_conflicts = |gs: &[Vec<Player>]| -> usize {
        gs.iter()
            .map(|g| {
                let mut seen: std::collections::HashMap<&str, usize> =
                    std::collections::HashMap::new();
                for p in g {
                    let l = p.leader_name();
                    if !l.is_empty() {
                        *seen.entry(l).or_default() += 1;
                    }
                }
                seen.values().map(|c| c.saturating_sub(1)).sum::<usize>()
            })
            .sum()
    };

    let mut result: Vec<Vec<Player>> = groups.to_vec();
    for _ in 0..max_iter {
        if leader_conflicts(&result) == 0 {
            break;
        }
        let mut best: Option<(usize, usize, usize, usize, f64)> = None;
        for ai in 0..result.len() {
            let leaders_a: HashSet<&str> = result[ai]
                .iter()
                .map(|p| p.leader_name())
                .filter(|l| !l.is_empty())
                .collect();
            for pi in 0..result[ai].len() {
                let p = result[ai][pi].clone();
                let lp = p.leader_name();
                if lp.is_empty() {
                    continue;
                }
                for bi in 0..result.len() {
                    if bi == ai {
                        continue;
                    }
                    let leaders_b: HashSet<&str> = result[bi]
                        .iter()
                        .map(|p| p.leader_name())
                        .filter(|l| !l.is_empty())
                        .collect();
                    if leaders_b.contains(lp) {
                        continue;
                    }
                    for qi in 0..result[bi].len() {
                        let q = result[bi][qi].clone();
                        let lq = q.leader_name();
                        if !lq.is_empty() && leaders_a.contains(lq) && lq != lp {
                            continue;
                        }
                        let (sum_a, sum_b) = (sum_wave(&result[ai]), sum_wave(&result[bi]));
                        let before = (sum_a - sum_b).abs();
                        let after = (sum_a - p.wave + q.wave - (sum_b - q.wave + p.wave)).abs();
                        let cost = -(before - after);
                        if best.is_none_or(|(_, _, _, _, c)| cost < c) {
                            best = Some((ai, pi, bi, qi, cost));
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
        if leader_conflicts(&result) == 0 {
            break;
        }
    }
    result
}

/// 固定位插回后可能出现超编组（末组多 1 人），这里把多余的人挪到最缺人的组。
/// 选人标准：非固定位 + 不破坏互斥 + 让组间极差变化最小。
fn fix_overflow(
    groups: &[Vec<Player>],
    group_size: usize,
    protected: &HashSet<String>,
    exclude_sets: &[Vec<String>],
) -> Vec<Vec<Player>> {
    let set_list: Vec<HashSet<String>> = exclude_sets
        .iter()
        .map(|s| -> HashSet<String> { s.iter().map(|x| norm_name(x).to_string()).collect() })
        .filter(|s| s.len() > 1)
        .collect();

    let mut result: Vec<Vec<Player>> = groups.to_vec();
    if result.len() < 2 {
        return result;
    }
    let total: f64 = result.iter().map(|g| sum_wave(g)).sum();
    let mean = total / result.len() as f64;

    for _ in 0..result.len() * 2 {
        let Some(over) = result.iter().position(|g| g.len() > group_size) else {
            break;
        };
        // 目标组：人数最少、且不与超编组里的人撞互斥
        let Some(target) = (0..result.len())
            .filter(|&j| j != over && result[j].len() < group_size)
            .min_by_key(|&j| result[j].len())
        else {
            break;
        };

        let (sum_over, sum_target) = (sum_wave(&result[over]), sum_wave(&result[target]));
        let dev =
            |p: &Player| (sum_over - p.wave - mean).abs() + (sum_target + p.wave - mean).abs();
        let best = result[over]
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                !protected.contains(&p.key)
                    && !set_list.iter().any(|s: &HashSet<String>| {
                        s.contains(norm_name(&p.name))
                            && result[target]
                                .iter()
                                .any(|x| s.contains(norm_name(&x.name)))
                    })
            })
            .min_by(|(_, a), (_, b)| {
                dev(a)
                    .partial_cmp(&dev(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i);
        let Some(idx) = best else { break };
        let p = result[over].remove(idx);
        result[target].push(p);
    }
    result
}

/// 自动分组 + 内置条件。
/// **内置条件永远优先**（2026-10-07 用户拍板：无论选哪种策略、哪种强度位次，
/// 固定位 / 互斥 / 最小间隔 / 至少第 N 组起 都必须先满足）。
/// 顺序：固定位的人先抽出来 → 其余人按策略分 → 固定位插回指定组
/// → 互斥修复 → 最小间隔修复 → 均衡微调（梯队式跳过）→ 强度位次（梯队式跳过）。
pub fn auto_group(players: &[Player], options: &GroupOptions) -> Vec<Vec<Player>> {
    let active: Vec<Player> = players.iter().filter(|p| !p.excluded).cloned().collect();
    if active.is_empty() {
        return Vec::new();
    }

    let cs = options.constraints.clone().unwrap_or_default();
    let g = group_count(active.len(), options.group_size);

    /* 梯队式（Sequential / SequentialLastN）：底子仍是「按音浪降序切块」，
    但内置条件照样优先——固定位照样抽出来插回去、互斥/最小间隔照样修。
    只跳过两件会把阶梯抹平的事：均衡微调（polish_balance）与强度位次
    （梯队本身就是位次：第 1 组最强，与「第 3 组最强」的位次表天然冲突，
    前端对梯队策略也把位次下拉禁掉了）。 */
    let ladder = matches!(
        options.strategy,
        Strategy::Sequential | Strategy::SequentialLastN
    );

    /* 1. 固定位 */
    let mut pinned: Vec<(String, usize)> = Vec::new();
    for rule in &cs.pinned {
        if let Some(hit) = crate::constraints::find_by_name(&active, &rule.name) {
            pinned.push((hit.key.clone(), resolve_pinned_index(rule, g)));
        }
    }
    for rule in &cs.min_group {
        if let Some(hit) = crate::constraints::find_by_name(&active, &rule.name) {
            let target = (rule.group as usize).saturating_sub(1).min(g - 1);
            pinned.push((hit.key.clone(), target));
        }
    }
    let pinned_keys: HashSet<String> = pinned.iter().map(|(k, _)| k.clone()).collect();
    let rest: Vec<Player> = active
        .iter()
        .filter(|p| !pinned_keys.contains(&p.key))
        .cloned()
        .collect();

    /* 2. 其余人按策略分组：组数一律用 g（按总人数算出的组数），
    不能按 group_count(rest) 算——抽出固定位后剩余人数恰好整除时
    （如 88 人 / 每组 8 人 = 11 组），按 rest 算会铺不满 g 组，
    空组最后只接得住固定位插回挤出的溢出人员，出现 8×11+3 式失衡。
    各策略内部语义不变，只是把「切几份」统一成 g。 */
    let mut groups = match options.strategy {
        Strategy::Random => {
            use rand::seq::SliceRandom;
            let mut arr = rest.clone();
            arr.shuffle(&mut rand::thread_rng());
            arr.sort_by(by_wave_desc);
            cut_into(&arr, g)
        }
        Strategy::Sequential | Strategy::SequentialLastN => cut_into(&sorted_desc(&rest), g),
        Strategy::Snake | Strategy::SnakeNoMaster => snake_deal(&sorted_desc(&rest), g),
    };
    if options.strategy == Strategy::SnakeNoMaster {
        groups = resolve_master_conflicts(&groups, 400);
    }
    while groups.len() < g {
        groups.push(Vec::new());
    }

    /* 3. 固定位插回（可能撑爆末组，顺手修一下容量） */
    groups = place_pinned(&groups, &active, &pinned);
    groups = fix_overflow(
        &groups,
        options.group_size,
        &pinned_keys,
        &cs.exclude_together,
    );

    /* 4. 互斥修复 */
    groups = resolve_exclusions(&groups, &cs.exclude_together, &pinned_keys, 300);

    /* 4.5 最小间隔修复（啸昭/啸寻这类「至少相隔 N 组」） */
    let min_gap_rules = cs.min_gap.clone();
    if !min_gap_rules.is_empty() {
        groups = resolve_min_gaps(
            &groups,
            &min_gap_rules,
            &pinned_keys,
            &cs.exclude_together,
            200,
        );
    }

    /* 5. 均衡微调：只接受让极差变小的交换，绝不移动固定位、绝不制造新的互斥/间隔冲突。
    梯队式跳过——微调会把「第 1 组最强」的阶梯抹平。 */
    if !ladder {
        groups = polish_balance(
            &groups,
            &PolishOptions {
                protected_keys: pinned_keys.clone(),
                exclude_sets: cs.exclude_together.clone(),
                avoid_master: options.strategy == Strategy::SnakeNoMaster,
                min_gap_rules: min_gap_rules.clone(),
                max_iter: 400,
            },
        );
    }

    /* 5.5 强度位次：强制条件与均衡都处理完，最后再把各组实力摆成指定顺序
    （默认第 3 组最强、第 4 组次强、第 1 组最弱）。
    只做跨组对调，绝不移动固定位、绝不新增互斥/间隔冲突。
    梯队式跳过：阶梯本身就是位次，套位次表等于把策略废掉。 */
    if let Some(so) = cs.strength_order.as_ref().filter(|s| s.enabled && !ladder) {
        let base_order = if so.order.is_empty() {
            default_strength_order(groups.len())
        } else {
            so.order.clone()
        };
        match so.mode {
            /* 每组一个最强：先把前 N 名（N = 组数）按位次表一人一组铺开当头牌，
            再把头牌锁住做均衡——这样每组都有一个能打的，且第 3 组的头牌最强。 */
            StrengthMode::Top => {
                groups = place_top_seeds(
                    &groups,
                    &SeedOptions {
                        order: base_order.clone(),
                        protected_keys: pinned_keys.clone(),
                        exclude_sets: cs.exclude_together.clone(),
                        min_gap_rules: min_gap_rules.clone(),
                    },
                );
                // 头牌不能被后来的微调挪走，否则位次白排
                let mut protect = pinned_keys.clone();
                for g in &groups {
                    if let Some(top) = g.iter().max_by(|a, b| {
                        a.wave
                            .partial_cmp(&b.wave)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    }) {
                        protect.insert(top.key.clone());
                    }
                }
                groups = polish_balance(
                    &groups,
                    &PolishOptions {
                        protected_keys: protect,
                        exclude_sets: cs.exclude_together.clone(),
                        avoid_master: options.strategy == Strategy::SnakeNoMaster,
                        min_gap_rules: min_gap_rules.clone(),
                        max_iter: 400,
                    },
                );
                groups = order_by_strength(
                    &groups,
                    &StrengthOptions {
                        order: base_order,
                        mode: StrengthMode::Top,
                        protected_keys: pinned_keys.clone(),
                        exclude_sets: cs.exclude_together.clone(),
                        min_gap_rules: min_gap_rules.clone(),
                        avoid_master: options.strategy == Strategy::SnakeNoMaster,
                        max_iter: 200,
                    },
                );
            }
            /* 集中：约束与均衡都处理完之后，再按组总音浪把实力摆成指定顺序 */
            StrengthMode::Total => {
                groups = order_by_strength(
                    &groups,
                    &StrengthOptions {
                        order: base_order,
                        mode: StrengthMode::Total,
                        protected_keys: pinned_keys.clone(),
                        exclude_sets: cs.exclude_together.clone(),
                        min_gap_rules: min_gap_rules.clone(),
                        avoid_master: options.strategy == Strategy::SnakeNoMaster,
                        max_iter: 400,
                    },
                );
            }
        }
    }

    groups.retain(|g| !g.is_empty());

    /* 6. 组内按音浪降序。
    蛇形/均衡只保证「谁在哪组」，不管组内顺序，直接返回的话看板上会出现
    「序号 6 的音浪比序号 8 还大」这种刺眼场面（分享页的组内名次就是这么算的）。
    放在最后做：前面的固定位、互斥、最小间隔、微调都只关心成员归属，不关心顺序。 */
    for g in groups.iter_mut() {
        g.sort_by(|a, b| {
            b.wave
                .partial_cmp(&a.wave)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        });
    }
    groups
}

pub fn compute_balance(groups: &[Vec<Player>]) -> BalanceStat {
    let stats: Vec<GroupStat> = groups
        .iter()
        .enumerate()
        .map(|(index, g)| {
            let total = sum_wave(g);
            GroupStat {
                index,
                count: g.len(),
                total,
                avg: if g.is_empty() {
                    0
                } else {
                    (total / g.len() as f64).round() as i64
                },
            }
        })
        .collect();
    let totals: Vec<f64> = stats.iter().map(|s| s.total).collect();
    let max = totals.iter().cloned().fold(f64::MIN, f64::max);
    let min = totals.iter().cloned().fold(f64::MAX, f64::min);
    let (max, min) = if totals.is_empty() {
        (0.0, 0.0)
    } else {
        (max, min)
    };
    let mean = if totals.is_empty() {
        0.0
    } else {
        totals.iter().sum::<f64>() / totals.len() as f64
    };
    let variance = if totals.is_empty() {
        0.0
    } else {
        totals.iter().map(|t| (t - mean).powi(2)).sum::<f64>() / totals.len() as f64
    };
    BalanceStat {
        groups: stats,
        max,
        min,
        mean: mean.round() as i64,
        spread: if mean != 0.0 {
            ((max - min) / mean * 1000.0).round() / 10.0
        } else {
            0.0
        },
        std_dev: variance.sqrt().round() as i64,
    }
}

/// 拖拽微调：把 player 移动到 target_group 的指定位置
pub fn move_player(
    groups: &[Vec<Player>],
    from: (usize, usize),
    to: (usize, usize),
) -> Vec<Vec<Player>> {
    let mut next: Vec<Vec<Player>> = groups.to_vec();
    if from.0 >= next.len() || to.0 >= next.len() || from.1 >= next[from.0].len() {
        return groups.to_vec();
    }
    let p = next[from.0].remove(from.1);
    let insert_at = to.1.min(next[to.0].len());
    next[to.0].insert(insert_at, p);
    next
}

/// 约束是否全部满足（分组后自检的快捷方式）
pub fn is_valid(groups: &[Vec<Player>], cs: &ConstraintSet) -> bool {
    crate::constraints::find_violations(groups, cs).is_empty()
        && gap_conflicts(groups, &cs.min_gap) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constraints::find_violations;

    fn players(names: &[(&str, f64)]) -> Vec<Player> {
        names.iter().map(|(n, w)| Player::new(*n, *n, *w)).collect()
    }

    #[test]
    fn group_count_rounds_up() {
        assert_eq!(group_count(54, 8), 7);
        assert_eq!(group_count(56, 8), 7);
        assert_eq!(group_count(57, 8), 8);
        assert_eq!(group_count(0, 8), 1);
    }

    #[test]
    fn snake_spreads_evenly() {
        let list: Vec<Player> = (0..16)
            .map(|i| Player::new(format!("p{i}"), format!("p{i}"), 1000.0 - i as f64 * 10.0))
            .collect();
        let groups = snake_group(&list, 4);
        assert_eq!(groups.len(), 4);
        let bal = compute_balance(&groups);
        // 蛇形把 16 人按 4 组切，每组都是一档一档交错，总和应完全相等
        assert_eq!(bal.spread, 0.0);
    }

    #[test]
    fn sequential_keeps_tiers() {
        let list: Vec<Player> = (0..12)
            .map(|i| Player::new(format!("p{i}"), format!("p{i}"), 1200.0 - i as f64 * 100.0))
            .collect();
        let groups = sequential_group(&list, 3);
        let total_first: f64 = groups[0].iter().map(|p| p.wave).sum();
        let total_last: f64 = groups[3].iter().map(|p| p.wave).sum();
        assert!(total_first > total_last);
    }

    #[test]
    fn sequential_fills_by_group_size() {
        let list: Vec<Player> = (0..9)
            .map(|i| Player::new(format!("p{i}"), format!("p{i}"), 1000.0 - i as f64))
            .collect();
        let groups = sequential_group(&list, 8);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 8);
        assert_eq!(groups[1].len(), 1);
        assert_eq!(groups[0][0].name, "p0");
        assert_eq!(groups[0][7].name, "p7");
        assert_eq!(groups[1][0].name, "p8");
    }

    /// 内置条件优先（2026-10-07 用户拍板）：梯队式也必须满足固定位 / 互斥 / 最小间隔 /
    /// 至少第 N 组起——它只是不做均衡微调、不套强度位次（那两件事会把阶梯抹平）。
    /// 10 组布局：40 人每组 4 人，约束相关的人给低音浪，填充的人组成清晰阶梯。
    fn builtin_fixture() -> Vec<Player> {
        let mut items: Vec<(String, f64)> = vec![
            ("啸泽".into(), 300.0),
            ("啸阳".into(), 290.0),
            ("啸帆".into(), 280.0),
            ("浩森".into(), 270.0),
            ("浩泽".into(), 260.0),
            ("浩阳".into(), 250.0),
            ("啸昭".into(), 240.0),
            ("啸寻".into(), 230.0),
            ("狼佑".into(), 220.0),
            ("玖安".into(), 210.0),
            ("玖玉".into(), 200.0),
            ("玖妹".into(), 190.0),
            ("玖玥".into(), 180.0),
            ("狼影".into(), 170.0),
            ("浩冬".into(), 160.0),
        ];
        for i in 0..25 {
            items.push((format!("填充{i:02}"), 1000.0 - i as f64 * 10.0));
        }
        items
            .iter()
            .map(|(n, w)| Player::new(n.clone(), n.clone(), *w))
            .collect()
    }

    fn group_at(groups: &[Vec<Player>], name: &str) -> usize {
        groups
            .iter()
            .position(|g| g.iter().any(|p| p.name == name))
            .unwrap_or_else(|| panic!("找不到 {name}"))
    }

    #[test]
    fn sequential_respects_builtin_constraints() {
        let list = builtin_fixture();
        let cs = ConstraintSet::default();
        for strategy in [Strategy::Sequential, Strategy::SequentialLastN] {
            let groups = auto_group(&list, &GroupOptions::new(4, strategy));
            assert_eq!(groups.len(), 10, "{strategy:?} 组数不对");
            let bad = find_violations(&groups, &cs);
            assert!(bad.is_empty(), "{strategy:?} 违反内置条件: {bad:?}");
            assert_eq!(
                gap_conflicts(&groups, &cs.min_gap),
                0,
                "{strategy:?} 间隔没满足"
            );
            // 固定位
            assert_eq!(
                group_at(&groups, "啸泽"),
                groups.len() - 1,
                "啸泽应最后一组"
            );
            assert_eq!(group_at(&groups, "啸帆"), 1, "啸帆应第 2 组");
            assert_eq!(group_at(&groups, "浩森"), 0, "浩森应第 1 组");
            assert_eq!(
                group_at(&groups, "浩泽"),
                groups.len() - 1,
                "浩泽应最后一组"
            );
            assert!(group_at(&groups, "浩阳") >= 5, "浩阳应从第 6 组起");
            // 互斥：狼影/浩冬 不同组；玖玉/玖妹/玖玥 不能全挤一组
            assert_ne!(group_at(&groups, "狼影"), group_at(&groups, "浩冬"));
            let jiu: HashSet<usize> = ["玖玉", "玖妹", "玖玥"]
                .iter()
                .map(|n| group_at(&groups, n))
                .collect();
            assert!(jiu.len() >= 2, "玖玉/玖妹/玖玥 挤在同一组了");
            // 阶梯还在：第 1 组明显强于最后一组（微调/位次都没跑）
            let first: f64 = groups[0].iter().map(|p| p.wave).sum();
            let last: f64 = groups[groups.len() - 1].iter().map(|p| p.wave).sum();
            assert!(first > last, "阶梯被抹平了: {first} vs {last}");
        }
    }

    /// 用户拍板的红线：**无论策略 / 强度位次怎么选，内置条件都必须满足**。
    #[test]
    fn every_strategy_and_strength_keeps_builtin_constraints() {
        let list = builtin_fixture();
        let cs = ConstraintSet::default();
        for strategy in [
            Strategy::Snake,
            Strategy::SnakeNoMaster,
            Strategy::Sequential,
            Strategy::SequentialLastN,
            Strategy::Random,
        ] {
            for mode in [None, Some(StrengthMode::Top), Some(StrengthMode::Total)] {
                let mut c = cs.clone();
                c.strength_order = mode.map(|m| crate::constraints::StrengthOrder {
                    enabled: true,
                    mode: m,
                    order: Vec::new(),
                });
                let groups = auto_group(
                    &list,
                    &GroupOptions {
                        group_size: 4,
                        strategy,
                        constraints: Some(c),
                    },
                );
                let bad = find_violations(&groups, &cs);
                assert!(
                    bad.is_empty(),
                    "{strategy:?} / {mode:?} 违反内置条件: {bad:?}"
                );
                assert_eq!(
                    gap_conflicts(&groups, &cs.min_gap),
                    0,
                    "{strategy:?} / {mode:?} 最小间隔没满足"
                );
            }
        }
    }

    #[test]
    fn sequential_last_n_matches_sequential_but_pins_metric_semantics() {
        // 名单顺序故意与音浪顺序相反：验证只认 wave，不认名单先后
        let list = players(&[("丁", 100.0), ("丙", 300.0), ("乙", 200.0), ("甲", 400.0)]);
        fn names_of(gs: &[Vec<Player>]) -> Vec<&str> {
            gs.iter()
                .flat_map(|g| g.iter())
                .map(|p| p.name.as_str())
                .collect()
        }

        let by_last_n = auto_group(&list, &GroupOptions::new(2, Strategy::SequentialLastN));
        assert_eq!(names_of(&by_last_n), vec!["甲", "丙", "乙", "丁"]);

        // 算法与 Sequential 完全相同，区别只在调用方必须填 lastN 口径的 wave
        let by_total = auto_group(&list, &GroupOptions::new(2, Strategy::Sequential));
        assert_eq!(names_of(&by_total), names_of(&by_last_n));
    }

    #[test]
    fn sequential_last_n_serde_name_is_kebab() {
        let s: Strategy = serde_json::from_str("\"sequential-last-n\"").unwrap();
        assert_eq!(s, Strategy::SequentialLastN);
        assert_eq!(
            serde_json::to_value(s).unwrap(),
            serde_json::json!("sequential-last-n")
        );
    }

    #[test]
    fn auto_group_respects_pinned() {
        // 啸泽(第1组) 啸帆(最后一组)；20 人每组 4 人 → 5 组
        let mut items: Vec<(&str, f64)> = vec![
            ("啸泽", 500.0),
            ("啸帆", 100.0),
            ("啸昭", 400.0),
            ("啸寻", 300.0),
        ];
        for i in 0..16 {
            items.push((
                Box::leak(format!("填充{i}").into_boxed_str()),
                500.0 - i as f64,
            ));
        }
        let list = players(&items);
        let groups = auto_group(&list, &GroupOptions::new(4, Strategy::Snake));
        assert_eq!(groups.len(), 5);
        let at = |n: &str| {
            groups
                .iter()
                .position(|g| g.iter().any(|p| p.name == n))
                .unwrap()
        };
        // 2026-10-07 深夜：啸泽改固定最后一组、啸帆改固定第 2 组
        assert_eq!(at("啸泽"), groups.len() - 1);
        assert_eq!(at("啸帆"), 1);
    }

    /// 回归：抽出固定位后剩余人数恰好整除（88/8=11 组）时，
    /// 必须仍按总人数的组数（12）蛇形铺满，不能让末组只接住溢出的固定位。
    /// 91 人（含 3 个固定位相关人）每组 8 人 → 12 组，人数应为 8×4+7×8 量级的均衡态，
    /// 绝不允许出现「11 组满员 + 末组 3 人」。
    #[test]
    fn auto_group_balances_when_rest_exactly_divisible() {
        // 91 人：啸泽/啸帆/浩阳 参与固定位与最低组号，其余 88 人恰好 11×8
        let mut items: Vec<(&str, f64)> = vec![("啸泽", 294.0), ("啸帆", 6852.0), ("浩阳", 1786.0)];
        for i in 0..88 {
            items.push((
                Box::leak(format!("填充{i:02}").into_boxed_str()),
                40000.0 - i as f64 * 100.0,
            ));
        }
        let list = players(&items);
        let groups = auto_group(&list, &GroupOptions::new(8, Strategy::Snake));
        assert_eq!(groups.len(), 12);
        let sizes: Vec<usize> = groups.iter().map(|g| g.len()).collect();
        assert!(
            sizes.iter().max().unwrap() - sizes.iter().min().unwrap() <= 1,
            "组人数失衡: {sizes:?}"
        );
        let at = |n: &str| {
            groups
                .iter()
                .position(|g| g.iter().any(|p| p.name == n))
                .unwrap()
        };
        assert_eq!(at("啸泽"), 11, "啸泽固定最后一组");
        assert_eq!(at("啸帆"), 1, "啸帆固定第 2 组");
        assert!(at("浩阳") >= 5);
    }

    #[test]
    fn auto_group_respects_min_gap_when_enough_groups() {
        // 组数 >= gap+1 时才可能满足间隔：32 人每组 4 人 → 8 组
        let mut items: Vec<(&str, f64)> = vec![
            ("啸泽", 900.0),
            ("啸帆", 100.0),
            ("啸昭", 800.0),
            ("啸寻", 700.0),
        ];
        for i in 0..28 {
            items.push((
                Box::leak(format!("填充{i}").into_boxed_str()),
                900.0 - i as f64,
            ));
        }
        let list = players(&items);
        let groups = auto_group(&list, &GroupOptions::new(4, Strategy::Snake));
        assert_eq!(groups.len(), 8);
        let at = |n: &str| {
            groups
                .iter()
                .position(|g| g.iter().any(|p| p.name == n))
                .unwrap()
        };
        assert!(
            at("啸昭").abs_diff(at("啸寻")) >= 4,
            "啸昭={} 啸寻={}",
            at("啸昭"),
            at("啸寻")
        );
    }

    #[test]
    fn auto_group_excludes_excluded_players() {
        let mut list = players(&[("甲", 300.0), ("乙", 200.0), ("丙", 100.0)]);
        list[0].excluded = true;
        let groups = auto_group(&list, &GroupOptions::new(8, Strategy::Snake));
        let all: Vec<&Player> = groups.iter().flatten().collect();
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|p| !p.excluded));
    }

    #[test]
    fn move_player_relocates() {
        let list = players(&[("a", 3.0), ("b", 2.0), ("c", 1.0)]);
        let groups = vec![
            vec![list[0].clone(), list[1].clone()],
            vec![list[2].clone()],
        ];
        let moved = move_player(&groups, (0, 0), (1, 0));
        assert_eq!(moved[1][0].name, "a");
        assert_eq!(moved[0].len(), 1);
    }
}

#[cfg(test)]
mod intra_group_order_tests {
    use super::*;

    fn p(name: &str, wave: f64) -> Player {
        Player {
            key: name.to_string(),
            raw: name.to_string(),
            name: name.to_string(),
            leader: None,
            person_id: None,
            matched: true,
            wave,
            avatar: None,
            excluded: false,
        }
    }

    fn names(g: &[Player]) -> Vec<&str> {
        g.iter().map(|x| x.name.as_str()).collect()
    }

    /// 组内必须按音浪降序：看板上直接用数组下标当序号显示，顺序乱了序号就乱了
    #[test]
    fn groups_are_sorted_by_wave_desc() {
        let players: Vec<Player> = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"]
            .iter()
            .enumerate()
            .map(|(i, n)| p(n, (12 - i) as f64 * 1000.0))
            .collect();
        for strategy in [
            Strategy::Snake,
            Strategy::SnakeNoMaster,
            Strategy::Sequential,
            Strategy::SequentialLastN,
            Strategy::Random,
        ] {
            let groups = auto_group(&players, &GroupOptions::new(4, strategy));
            assert!(!groups.is_empty());
            for g in &groups {
                let waves: Vec<f64> = g.iter().map(|x| x.wave).collect();
                let mut sorted = waves.clone();
                sorted.sort_by(|a, b| b.partial_cmp(a).unwrap());
                assert_eq!(
                    waves,
                    sorted,
                    "{strategy:?} 的组 {:?} 没按音浪降序",
                    names(g)
                );
            }
        }
    }

    /// 同音浪时按姓名定序，保证结果稳定可复现（不然每次刷新顺序都可能跳）
    #[test]
    fn equal_waves_fall_back_to_name_order() {
        let players: Vec<Player> = ["狼耀", "浩冬", "玖玥", "啸安", "浩杰", "浩宁"]
            .iter()
            .map(|n| p(n, 5000.0))
            .collect();
        let groups = auto_group(&players, &GroupOptions::new(3, Strategy::Snake));
        for g in &groups {
            let list = names(g);
            let mut want = list.clone();
            want.sort();
            assert_eq!(list, want, "同音浪时应按姓名排：{:?}", list);
        }
    }

    /// 排序不能动分组结果：总人数、组数、每组音浪合计都要和排序前一致
    #[test]
    fn sorting_does_not_change_membership() {
        let players: Vec<Player> = (0..20)
            .map(|i| p(&format!("p{i:02}"), ((i * 37) % 11) as f64 * 1000.0 + 500.0))
            .collect();
        let groups = auto_group(&players, &GroupOptions::new(5, Strategy::Snake));
        let total: f64 = groups.iter().flatten().map(|x| x.wave).sum();
        let expect_total: f64 = players.iter().map(|x| x.wave).sum();
        assert_eq!(groups.iter().flatten().count(), 20);
        assert!(
            (total - expect_total).abs() < f64::EPSILON,
            "排序改变了音浪合计"
        );
    }
}
