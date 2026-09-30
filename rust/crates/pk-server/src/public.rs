//! 对外只读展示端点（无需鉴权）。
//!
//! 只暴露「音浪 + 分组」这类公开展示需要的字段：
//! - 不返回 personId / key / raw 等内部标识
//! - 不返回数据库连接、数据源、别名等任何管理信息
//! - 音浪按方案的 metric/days 实时取（走 WaveCache），库挂了自动降级用方案里的快照值
//!
//! 路由：`GET /api/public/plans/{id}`、`GET /api/public/plans`（已发布列表）

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::repos;
use crate::sqlite;
use crate::state::AppState;

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "展示页不存在或已下架" })),
    )
        .into_response()
}

#[derive(serde::Serialize, Clone)]
pub struct PublicPlayer {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    leader: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    avatar: Option<String>,
    wave: f64,
    /// 全场音浪名次
    rank: usize,
    /// 组内音浪名次
    group_rank: usize,
    /// 仅用于回填实时音浪，绝不出现在响应里
    #[serde(skip)]
    person_id: Option<String>,
    /// 组内原始顺序
    #[serde(skip)]
    order: usize,
}

#[derive(serde::Serialize)]
pub struct PublicGroup {
    name: String,
    /// 开赛时间，如 "08:15"
    slot: String,
    count: usize,
    /// 组内音浪合计
    total: f64,
    players: Vec<PublicPlayer>,
}

/// 按音浪降序打名次，同分并列；打完后按原始顺序还原
fn rank_within(players: &mut [PublicPlayer], assign: fn(&mut PublicPlayer, usize)) {
    let mut order: Vec<usize> = (0..players.len()).collect();
    order.sort_by(|&a, &b| {
        players[b]
            .wave
            .partial_cmp(&players[a].wave)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| players[a].name.cmp(&players[b].name))
    });
    let mut last_wave = f64::NAN;
    let mut last_rank = 0usize;
    for (pos, &idx) in order.iter().enumerate() {
        let wave = players[idx].wave;
        if (wave - last_wave).abs() < f64::EPSILON {
            assign(&mut players[idx], last_rank);
        } else {
            assign(&mut players[idx], pos + 1);
            last_rank = pos + 1;
            last_wave = wave;
        }
    }
    players.sort_by_key(|p| p.order);
}

/// "08:15" + n * interval；起止时间非法就退回第 1 组时间
fn slot_time(index: usize, start: &str, interval_min: i64) -> String {
    let parsed = start
        .trim()
        .split_once(':')
        .and_then(|(h, m)| Some((h.trim().parse::<i64>().ok()?, m.trim().parse::<i64>().ok()?)));
    let Some((h, m)) = parsed else {
        return start.to_string();
    };
    let total = h * 60 + m + index as i64 * interval_min.max(1);
    format!(
        "{:02}:{:02}",
        total.div_euclid(60).rem_euclid(24),
        total.rem_euclid(60)
    )
}

fn as_str(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn as_f64(v: &serde_json::Value, key: &str) -> f64 {
    v.get(key).and_then(|x| x.as_f64()).unwrap_or(0.0)
}

/// 展示页模块显隐，来自方案 payload 的 `display` 字段（管理端可配）。
/// 老方案没有这个字段时全部按 true 处理，行为与旧版一致。
#[derive(serde::Serialize, Clone, Copy)]
struct DisplayConfig {
    wave: bool,
    ranking: bool,
    daily: bool,
    groups: bool,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self { wave: true, ranking: true, daily: true, groups: true }
    }
}

impl DisplayConfig {
    fn from_payload(v: &serde_json::Value) -> Self {
        let flag = |key: &str| v.get("display").and_then(|d| d.get(key)).and_then(|x| x.as_bool());
        Self {
            wave: flag("wave").unwrap_or(true),
            ranking: flag("ranking").unwrap_or(true),
            daily: flag("daily").unwrap_or(true),
            groups: flag("groups").unwrap_or(true),
        }
    }
}

/// `GET /api/public/plans/{id}`：单个方案的公开展示数据
pub async fn plan(State(st): State<std::sync::Arc<AppState>>, Path(id): Path<i64>) -> Response {
    let Some(row) = sqlite::get_published_plan(id) else {
        return not_found();
    };
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&row.payload) else {
        tracing::warn!(id, "方案 payload 不是合法 JSON");
        return not_found();
    };

    let empty = Vec::new();
    let buckets = payload
        .get("buckets")
        .and_then(|b| b.as_array())
        .unwrap_or(&empty);

    // 方案里存的口径优先，其次用落库时的列
    let metric = as_str(&payload, "metric").unwrap_or_else(|| row.metric.clone());
    let days = payload
        .get("days")
        .and_then(|d| d.as_i64())
        .unwrap_or(row.days);
    let display = DisplayConfig::from_payload(&payload);
    let start = payload
        .get("schedule")
        .and_then(|s| s.get("start"))
        .and_then(|v| v.as_str())
        .unwrap_or("08:15")
        .to_string();
    let interval = payload
        .get("schedule")
        .and_then(|s| s.get("intervalMin"))
        .and_then(|v| v.as_i64())
        .unwrap_or(15);

    /* ---- 1. 按快照音浪把结构搭起来，同时收集 personId ---- */
    let mut groups: Vec<PublicGroup> = Vec::with_capacity(buckets.len());
    let mut all_players: Vec<PublicPlayer> = Vec::new();
    let mut person_ids: Vec<String> = Vec::new();
    let mut order = 0usize;

    for (gi, b) in buckets.iter().enumerate() {
        let name = b
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("第 {} 组", gi + 1));
        let rows = b
            .get("players")
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default();

        let mut group_players: Vec<PublicPlayer> = Vec::with_capacity(rows.len());
        for p in &rows {
            // 被排除的人不对外展示
            if p.get("excluded").and_then(|v| v.as_bool()).unwrap_or(false) {
                continue;
            }
            let Some(pname) = p.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let pid = p
                .get("personId")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .filter(|s| !s.is_empty());
            if let Some(id) = &pid {
                person_ids.push(id.clone());
            }
            group_players.push(PublicPlayer {
                name: pname.to_string(),
                leader: as_str(p, "leader"),
                avatar: p.get("avatar").and_then(|v| v.as_str()).map(str::to_string),
                wave: as_f64(p, "wave"),
                rank: 0,
                group_rank: 0,
                person_id: pid,
                order,
            });
            order += 1;
        }

        let total = group_players.iter().map(|p| p.wave).sum();
        rank_within(&mut group_players, |p, r| p.group_rank = r);
        all_players.extend(group_players.iter().cloned());
        groups.push(PublicGroup {
            name,
            slot: slot_time(gi, &start, interval),
            count: group_players.len(),
            total,
            players: group_players,
        });
    }

    /* ---- 2. 实时音浪覆盖快照；查库失败降级为快照并标记 stale ---- */
    // 没人可查（全是未匹配主播）时本来就取不到实时音浪；
    // 这时候库要是还连不上，更不能让页面假装数据是实时的。
    // 管理端关掉「音浪数值」时整个跳过：不查库、也不把音浪带给前端。
    let mut stale = !st.pools.any_available();
    let mut dates: Vec<String> = Vec::new();
    if display.wave && !person_ids.is_empty() {
        match repos::get_waves(&st, &metric, days, Some(&person_ids), false, None).await {
            Ok(info) => {
                dates = info.dates;
                let live: HashMap<&str, f64> =
                    info.waves.iter().map(|(k, v)| (k.as_str(), *v)).collect();
                for g in groups.iter_mut() {
                    for p in g.players.iter_mut() {
                        if let Some(v) = p.person_id.as_deref().and_then(|id| live.get(id)) {
                            p.wave = *v;
                        }
                    }
                }
            }
            Err(e) => {
                stale = true;
                tracing::warn!(id, error = %e, "展示页取音浪失败，降级用方案快照");
            }
        }
    }

    /* ---- 2b. 每日明细：按日期切换分组明细 ----
    管理端关掉「每日音浪」或「音浪数值」时直接省略（每日明细本质也是音浪数据）。
    取不到就省略，前端退回纯列表展示，不留空壳。 */
    let (trend, daily) = if display.daily && display.wave {
        match repos::get_wave_series(&st, false, None).await {
        Ok(series) if !series.dates.is_empty() => {
            let dates = series.dates;
            let day_count = dates.len();
            // 每个日期下标上，把组内成员的当日音浪加起来
            let group_values: Vec<Vec<f64>> = groups
                .iter()
                .map(|g| {
                    (0..day_count)
                        .map(|i| {
                            g.players
                                .iter()
                                .filter_map(|p| p.person_id.as_ref())
                                .filter_map(|id| series.series.get(id))
                                .map(|v| v[i])
                                .sum()
                        })
                        .collect()
                })
                .collect();

            let lines: Vec<_> = groups
                .iter()
                .zip(&group_values)
                .map(|(g, values)| json!({ "name": g.name, "values": values }))
                .collect();

            // 每日明细取整：音浪本来就是计数，小数位对外没有信息量，还会让响应体翻倍
            let daily_groups: Vec<_> = groups
                .iter()
                .zip(&group_values)
                .map(|(g, values)| {
                    let players: Vec<_> = g
                        .players
                        .iter()
                        .map(|p| {
                            let values: Vec<i64> = p
                                .person_id
                                .as_deref()
                                .and_then(|id| series.series.get(id))
                                .map(|v| v.iter().map(|x| x.round() as i64).collect())
                                .unwrap_or_else(|| vec![0; day_count]);
                            json!({ "name": p.name, "leader": p.leader, "values": values })
                        })
                        .collect();
                    json!({
                        "name": g.name,
                        "values": values.iter().map(|v| v.round() as i64).collect::<Vec<i64>>(),
                        "players": players,
                    })
                })
                .collect();

            (
                Some(json!({ "dates": dates.clone(), "series": lines })),
                Some(json!({ "dates": dates, "groups": daily_groups })),
            )
        }
        Ok(_) => (None, None),
        Err(e) => {
            tracing::warn!(id, error = %e, "取每日音浪序列失败，按日明细省略");
            (None, None)
        }
        }
    } else {
        (None, None)
    };

    /* ---- 3. 音浪定稿后统一重算：组内名次、组合计、全场名次 ----
    放在覆盖之后，是因为名次依赖音浪；而且必须无条件执行——
    早期版本把这段塞在「取到实时音浪」的分支里，结果库一挂全场名次全是 0。 */
    all_players.clear();
    for g in groups.iter_mut() {
        rank_within(&mut g.players, |p, r| p.group_rank = r);
        g.total = g.players.iter().map(|p| p.wave).sum();
        g.count = g.players.len();
        all_players.extend(g.players.iter().cloned());
    }
    rank_within(&mut all_players, |p, r| p.rank = r);
    // 用 order 做键回写，避免每组每人都在 flat 里线性找一次（原来是 O(n²)）
    let rank_of: HashMap<usize, usize> = all_players.iter().map(|p| (p.order, p.rank)).collect();
    for g in groups.iter_mut() {
        for p in g.players.iter_mut() {
            p.rank = rank_of.get(&p.order).copied().unwrap_or(0);
            // person_id 只用于回填实时音浪，任何情况下都不外泄
            p.person_id = None;
        }
        // 按音浪名次排，而不是沿用方案里的原始顺序。
        // 方案可能是排序修复前存的，原始顺序未必跟音浪一致，
        // 直接渲染会出现「序号 8 的人音浪比序号 6 还大」。
        g.players.sort_by_key(|p| p.group_rank);
    }

    // 管理端关掉「音浪数值」：数字一律不外泄——音浪清零、全场排行省略、组合计清零。
    // 组内序号保留，它只反映组内顺序，公布页本来就在用。
    let ranking: Vec<_> = if display.wave {
        let mut r = all_players;
        r.sort_by(|a, b| {
            a.rank
                .cmp(&b.rank)
                .then_with(|| a.group_rank.cmp(&b.group_rank))
        });
        r
    } else {
        for g in groups.iter_mut() {
            for p in g.players.iter_mut() {
                p.wave = 0.0;
                p.rank = 0;
            }
            g.total = 0.0;
        }
        Vec::new()
    };

    Json(json!({
        "id": row.id,
        "title": row.title,
        "brand": as_str(&payload, "brand").unwrap_or_default(),
        "metric": metric,
        "days": days,
        "schedule": { "start": start, "intervalMin": interval },
        "display": display,
        "createdAt": row.created_at,
        "dates": dates,
        "stale": stale,
        "counts": {
            "groups": groups.len(),
            "players": if display.wave { ranking.len() } else { groups.iter().map(|g| g.count).sum() },
            "totalWave": if display.wave { ranking.iter().map(|p| p.wave).sum::<f64>() } else { 0.0 },
        },
        "groups": groups,
        "ranking": ranking,
        "trend": trend,
        "daily": daily,
    }))
    .into_response()
}

/// `GET /api/public/plans`：已发布方案的轻量列表
pub async fn plans(_state: State<std::sync::Arc<AppState>>) -> Response {
    let items: Vec<_> = sqlite::list_plans()
        .into_iter()
        .filter(|p| p.published)
        .map(|p| {
            json!({
                "id": p.id,
                "title": p.title,
                "metric": p.metric,
                "days": p.days,
                "createdAt": p.created_at,
            })
        })
        .collect();
    Json(json!({ "items": items })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(name: &str, wave: f64, order: usize) -> PublicPlayer {
        PublicPlayer {
            name: name.to_string(),
            leader: None,
            avatar: None,
            wave,
            rank: 0,
            group_rank: 0,
            person_id: None,
            order,
        }
    }

    fn assign(p: &mut PublicPlayer, r: usize) {
        p.rank = r;
    }

    #[test]
    fn slot_time_advances_by_interval() {
        let s = |i: usize| slot_time(i, "08:15", 15);
        assert_eq!(s(0), "08:15");
        assert_eq!(s(1), "08:30");
        assert_eq!(s(3), "09:00");
    }

    #[test]
    fn slot_time_handles_bad_input_and_midnight() {
        // 起止时间不合法就原样返回，别把页面搞崩
        assert_eq!(slot_time(2, "abc", 15), "abc");
        // 跨过午夜要回绕，不能出现 25:00
        assert_eq!(slot_time(1, "23:55", 15), "00:10");
    }

    #[test]
    fn ranking_is_descending_and_order_is_restored() {
        let mut ps = vec![
            player("甲", 100.0, 0),
            player("乙", 300.0, 1),
            player("丙", 200.0, 2),
        ];
        rank_within(&mut ps, assign);
        // 排完名次要按原始顺序还原，不能把展示顺序打乱
        assert_eq!(
            ps.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["甲", "乙", "丙"]
        );
        assert_eq!(ps[0].rank, 3);
        assert_eq!(ps[1].rank, 1);
        assert_eq!(ps[2].rank, 2);
    }

    #[test]
    fn equal_waves_share_a_rank() {
        let mut ps = vec![
            player("甲", 200.0, 0),
            player("乙", 200.0, 1),
            player("丙", 100.0, 2),
        ];
        rank_within(&mut ps, assign);
        assert_eq!(ps[0].rank, 1, "同分并列第 1");
        assert_eq!(ps[1].rank, 1, "同分并列第 1");
        assert_eq!(ps[2].rank, 3, "下一个名次跳号");
    }

    #[test]
    fn missing_wave_reads_as_zero() {
        let v = serde_json::json!({ "wave": "不是数字" });
        assert_eq!(as_f64(&v, "wave"), 0.0);
        assert_eq!(as_f64(&serde_json::json!({}), "wave"), 0.0);
    }

    #[test]
    fn group_players_come_back_sorted_by_rank() {
        // 模拟「方案里顺序是乱的」（排序修复前存的老方案）
        let mut g = vec![
            player("低音浪的", 100.0, 0),
            player("最高的", 900.0, 1),
            player("中音浪的", 500.0, 2),
        ];
        rank_within(&mut g, |p, r| p.group_rank = r);
        let mut display = g.clone();
        // public.rs 收尾时做的排序
        display.sort_by_key(|p| p.group_rank);
        let seq: Vec<usize> = display.iter().map(|p| p.group_rank).collect();
        assert_eq!(seq, [1, 2, 3], "展示顺序必须让名次连成 1,2,3…");
        let waves: Vec<f64> = display.iter().map(|p| p.wave).collect();
        let mut desc = waves.clone();
        desc.sort_by(|a, b| b.partial_cmp(a).unwrap());
        assert_eq!(waves, desc, "展示出来必须是音浪降序");
    }

    #[test]
    fn as_str_drops_empty_strings() {
        let v = serde_json::json!({ "a": "", "b": "有值" });
        assert_eq!(as_str(&v, "a"), None);
        assert_eq!(as_str(&v, "b").as_deref(), Some("有值"));
        assert_eq!(as_str(&v, "缺失"), None);
    }
}
