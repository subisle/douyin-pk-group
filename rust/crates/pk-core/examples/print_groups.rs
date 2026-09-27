//! 打印真实名单的分组结果，用于人工核对：cargo run -p pk-core --example print_groups
use pk_core::*;

fn main() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/players.json");
    let raw = std::fs::read_to_string(path).expect("读 fixture");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("解析 fixture");
    let players: Vec<Player> = serde_json::from_value(v["players"].clone()).expect("players");

    let cs = ConstraintSet::default();
    let groups = auto_group(&players, &GroupOptions::new(8, Strategy::Snake));
    for (i, g) in groups.iter().enumerate() {
        let total: f64 = g.iter().map(|p| p.wave).sum();
        println!(
            "第{}组 {} {:>7.1}万 {}人  {}",
            i + 1,
            slot_time(i, &cs.schedule),
            total / 10000.0,
            g.len(),
            g.iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    let b = compute_balance(&groups);
    println!(
        "\n极差 {:.1}%  max {:.0}  min {:.0}  组人数 {:?}",
        b.spread,
        b.max,
        b.min,
        groups.iter().map(|g| g.len()).collect::<Vec<_>>()
    );
    println!("违规: {:?}", find_violations(&groups, &cs));
}
