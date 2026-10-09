//! 调试：复现 auto_group 各阶段，打印每组人数，找出 11×8+3 的成因
use pk_core::constraints::{
    place_pinned, polish_balance, resolve_exclusions, resolve_min_gaps, resolve_pinned_index,
};
use pk_core::*;
use std::collections::HashSet;

fn sizes(groups: &[Vec<Player>]) -> Vec<usize> {
    groups.iter().map(|g| g.len()).collect()
}

fn main() {
    let path = std::env::args().nth(1).expect("path");
    let raw = std::fs::read_to_string(&path).unwrap();
    let players: Vec<Player> = serde_json::from_str(&raw).unwrap();

    let mut cs = ConstraintSet::default();
    cs.schedule.breaks = vec![BreakRule {
        after_group: 3,
        minutes: 20,
    }];
    cs.strength_order = Some(StrengthOrder {
        enabled: true,
        mode: StrengthMode::Top,
        order: Vec::new(),
    });

    let active: Vec<Player> = players.iter().filter(|p| !p.excluded).cloned().collect();
    let g = group_count(active.len(), 8);
    println!("总人数 {} 组数 {}", active.len(), g);

    // 1. 固定位
    let mut pinned: Vec<(String, usize)> = Vec::new();
    for rule in &cs.pinned {
        if let Some(hit) = find_by_name(&active, &rule.name) {
            pinned.push((hit.key.clone(), resolve_pinned_index(rule, g)));
        }
    }
    for rule in &cs.min_group {
        if let Some(hit) = find_by_name(&active, &rule.name) {
            pinned.push((
                hit.key.clone(),
                (rule.group as usize).saturating_sub(1).min(g - 1),
            ));
        }
    }
    let pinned_keys: HashSet<String> = pinned.iter().map(|(k, _)| k.clone()).collect();
    let rest: Vec<Player> = active
        .iter()
        .filter(|p| !pinned_keys.contains(&p.key))
        .cloned()
        .collect();
    println!(
        "pinned: {:?}",
        pinned
            .iter()
            .map(|(k, i)| (k.split('#').next().unwrap(), i))
            .collect::<Vec<_>>()
    );

    // 2. snake
    let mut groups = snake_group(&rest, 8);
    while groups.len() < g {
        groups.push(Vec::new());
    }
    println!(
        "snake 后: {:?} sum={}",
        sizes(&groups),
        sizes(&groups).iter().sum::<usize>()
    );

    // 3. place_pinned
    let groups = place_pinned(&groups, &active, &pinned);
    println!(
        "pinned 插回后: {:?} sum={}",
        sizes(&groups),
        sizes(&groups).iter().sum::<usize>()
    );

    // 4. 互斥修复
    let groups = resolve_exclusions(&groups, &cs.exclude_together, &pinned_keys, 300);
    println!(
        "互斥修复后: {:?} sum={}",
        sizes(&groups),
        sizes(&groups).iter().sum::<usize>()
    );

    // 5. 最小间隔
    let groups = resolve_min_gaps(
        &groups,
        &cs.min_gap,
        &pinned_keys,
        &cs.exclude_together,
        200,
    );
    println!(
        "间隔修复后: {:?} sum={}",
        sizes(&groups),
        sizes(&groups).iter().sum::<usize>()
    );

    // 6. 均衡微调
    let groups = polish_balance(
        &groups,
        &PolishOptions {
            protected_keys: pinned_keys.clone(),
            exclude_sets: cs.exclude_together.clone(),
            avoid_master: false,
            min_gap_rules: cs.min_gap.clone(),
            max_iter: 400,
        },
    );
    println!(
        "均衡微调后: {:?} sum={}",
        sizes(&groups),
        sizes(&groups).iter().sum::<usize>()
    );

    // 7. 强度位次 top：头牌铺位
    let so = cs.strength_order.as_ref().unwrap();
    println!(
        "强度位次: enabled={} mode={:?} order={:?}",
        so.enabled, so.mode, so.order
    );
    let base_order = if so.order.is_empty() {
        default_strength_order(groups.len())
    } else {
        so.order.clone()
    };
    let groups = place_top_seeds(
        &groups,
        &SeedOptions {
            order: base_order.clone(),
            protected_keys: pinned_keys.clone(),
            exclude_sets: cs.exclude_together.clone(),
            min_gap_rules: cs.min_gap.clone(),
        },
    );
    println!(
        "头牌铺位后: {:?} sum={}",
        sizes(&groups),
        sizes(&groups).iter().sum::<usize>()
    );

    // 8. 头牌保护 + 再均衡
    let mut protect = pinned_keys.clone();
    for g in &groups {
        if let Some(top) = g.iter().max_by(|a, b| a.wave.partial_cmp(&b.wave).unwrap()) {
            protect.insert(top.key.clone());
        }
    }
    let groups = polish_balance(
        &groups,
        &PolishOptions {
            protected_keys: protect,
            exclude_sets: cs.exclude_together.clone(),
            avoid_master: false,
            min_gap_rules: cs.min_gap.clone(),
            max_iter: 400,
        },
    );
    println!(
        "头牌保护均衡后: {:?} sum={}",
        sizes(&groups),
        sizes(&groups).iter().sum::<usize>()
    );

    // 9. 位次对调
    let groups = order_by_strength(
        &groups,
        &StrengthOptions {
            order: base_order,
            mode: StrengthMode::Top,
            protected_keys: pinned_keys.clone(),
            exclude_sets: cs.exclude_together.clone(),
            min_gap_rules: cs.min_gap.clone(),
            avoid_master: false,
            max_iter: 200,
        },
    );
    println!(
        "位次对调后: {:?} sum={}",
        sizes(&groups),
        sizes(&groups).iter().sum::<usize>()
    );
}
