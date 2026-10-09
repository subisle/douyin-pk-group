//! 真实名单 + 真实音浪的回归基线（fixture 由 TS 版 dump，2026-09-27）
//! 新内置约束加入后，蛇形均衡基线从 TS 的 13.3% 放宽到 14.2%；
//! 2026-10-07 补「浩森固定第 1 组 / 浩泽固定最后一组 / 两人间隔 ≥ 3 组」时放宽到 19.4%；
//! 同日深夜啸泽改钉末组、啸帆改钉第 2 组后回到 14.4%。
//! 固定位落点直接决定均衡可调空间，改内置条件就要跟着改这个基线，仍远低于梯队式。

use pk_core::{
    auto_group, compute_balance, find_violations, slot_time, ConstraintSet, GroupOptions, Player,
    Strategy,
};

fn load_players() -> Vec<Player> {
    let raw = include_str!("fixtures/players.json");
    let v: serde_json::Value = serde_json::from_str(raw).expect("fixture 是合法 JSON");
    serde_json::from_value(v["players"].clone()).expect("players 能反序列化为 Vec<Player>")
}

#[test]
fn real_roster_matches_ts_baseline() {
    let players = load_players();
    assert_eq!(players.len(), 54);
    let active: Vec<&Player> = players.iter().filter(|p| !p.excluded).collect();
    assert_eq!(active.len(), 54, "fixture 里应已剔除「目」");

    let cs = ConstraintSet::default();
    let groups = auto_group(&players, &GroupOptions::new(8, Strategy::Snake));

    // 1) 组数与 TS 基线一致
    assert_eq!(groups.len(), 7, "54 人每组上限 8 → 7 组");

    // 2) 所有人都恰好出现一次
    let flat: Vec<&Player> = groups.iter().flatten().collect();
    assert_eq!(flat.len(), 54);
    let mut keys: Vec<&str> = flat.iter().map(|p| p.key.as_str()).collect();
    keys.sort_unstable();
    let unique = {
        let mut k = keys.clone();
        k.dedup();
        k.len()
    };
    assert_eq!(unique, 54, "不能有人重复或丢失");

    // 3) 内置条件全部满足
    let violations = find_violations(&groups, &cs);
    assert!(violations.is_empty(), "约束冲突: {violations:?}");

    // 4) 固定位
    let at_opt = |n: &str| groups.iter().position(|g| g.iter().any(|p| p.name == n));
    let at = |n: &str| at_opt(n).unwrap_or_else(|| panic!("名单里没有 {n}"));
    assert_eq!(at("啸泽"), 6, "啸泽固定最后一组"); // 2026-10-07 深夜改：原为第 1 组
    assert_eq!(at("啸阳"), 0, "啸阳固定第 1 组");
    assert_eq!(at("啸帆"), 1, "啸帆固定第 2 组"); // 2026-10-07 深夜改：原为最后一组
    assert!(at("浩阳") >= 5, "浩阳必须第 6 组或更后");
    // 2026-10-07 新增：浩森固定第 1 组、浩泽固定最后一组。
    // fixture 里没有浩泽（不在名单里的人约束自动跳过），所以只对存在的人断言
    assert_eq!(at("浩森"), 0, "浩森固定第 1 组");
    if let Some(haoze) = at_opt("浩泽") {
        assert_eq!(haoze, 6, "浩泽固定最后一组");
        assert!(
            at("浩森").abs_diff(haoze) >= 3,
            "浩森与浩泽需间隔 ≥ 3 组（当前 {} 组）",
            at("浩森").abs_diff(haoze)
        );
    }

    // 5) 最小间隔
    assert!(
        at("啸昭").abs_diff(at("啸寻")) >= 4,
        "啸昭={} 啸寻={} 需间隔 ≥ 4 组",
        at("啸昭") + 1,
        at("啸寻") + 1
    );

    // 6) 开赛时间（2026-10-09 起默认 12:15 开赛）
    assert_eq!(slot_time(0, &cs.schedule), "12:15");
    assert_eq!(slot_time(6, &cs.schedule), "13:45");

    // 7) 均衡度与当前约束基线对齐（允许极差在 0.6 个百分点内浮动）
    let bal = compute_balance(&groups);
    assert!(bal.max > 0.0 && bal.min > 0.0);
    assert!(
        (bal.spread - 14.4).abs() < 0.6,
        "极差 {}% 与新约束基线 14.4% 偏离过大（max {} / min {}）",
        bal.spread,
        bal.max,
        bal.min
    );
    println!(
        "7 组 / 极差 {:.1}%（max {:.0} / min {:.0}）/ 啸昭第 {} 组 / 啸寻第 {} 组",
        bal.spread,
        bal.max,
        bal.min,
        at("啸昭") + 1,
        at("啸寻") + 1
    );
}

#[test]
fn every_group_has_a_slot_and_roughly_even_size() {
    let players = load_players();
    let groups = auto_group(&players, &GroupOptions::new(8, Strategy::Snake));
    let cs = ConstraintSet::default();
    for (i, g) in groups.iter().enumerate() {
        assert!(!g.is_empty(), "第 {} 组为空", i + 1);
        assert!(g.len() <= 8, "第 {} 组超编", i + 1);
        assert!(!slot_time(i, &cs.schedule).is_empty());
    }
    let max = groups.iter().map(Vec::len).max().unwrap();
    let min = groups.iter().map(Vec::len).min().unwrap();
    assert!(max - min <= 1, "蛇形分组各组人数差应 ≤ 1，实际 {min}~{max}");
}
