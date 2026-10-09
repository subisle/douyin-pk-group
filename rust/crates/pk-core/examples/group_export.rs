//! 临时导出：读 players.json，跑「内置条件 + 强度位次(top)」分组，输出 JSON。
//! 用法: cargo run -p pk-core --example group_export -- /path/to/players.json
use pk_core::*;
use serde_json::json;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: group_export <players.json>");
    let raw = std::fs::read_to_string(&path).expect("读 players.json");
    let players: Vec<Player> = serde_json::from_str(&raw).expect("解析 players");

    // 内置条件 + 当前端默认：第 3 组后加长休息 20 分钟、强度位次「每组一个最强」
    let mut cs = ConstraintSet::default();
    cs.schedule.breaks = vec![BreakRule {
        after_group: 3,
        minutes: 20,
    }];
    cs.strength_order = Some(StrengthOrder {
        enabled: true,
        mode: StrengthMode::Top,
        order: Vec::new(), // 留空 = 默认位次 [3,4,…,g,2,1]
    });

    let groups = auto_group(
        &players,
        &GroupOptions {
            group_size: 8,
            strategy: Strategy::Snake,
            constraints: Some(cs.clone()),
        },
    );
    let balance = compute_balance(&groups);
    let out = json!({
        "groupCount": groups.len(),
        "total": players.len(),
        "groups": groups
            .iter()
            .enumerate()
            .map(|(i, g)| {
                json!({
                    "no": i + 1,
                    "time": slot_time(i, &cs.schedule),
                    "count": g.len(),
                    "totalWave": g.iter().map(|p| p.wave).sum::<f64>(),
                    "members": g
                        .iter()
                        .map(|p| json!({"name": p.name, "wave": p.wave}))
                        .collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>(),
        "spread": balance.spread,
        "violations": find_violations(&groups, &cs)
            .iter()
            .map(|v| format!("{v:?}"))
            .collect::<Vec<_>>(),
        "gapConflicts": gap_conflicts(&groups, &cs.min_gap),
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
