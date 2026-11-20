//! 对外只读展示端点（展示端登录开启时按公开登录门鉴权）。
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
use crate::state::AppState;
use crate::store;

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

fn parse_breaks(v: &serde_json::Value) -> Vec<(usize, i64)> {
    v.get("breaks")
        .and_then(|b| b.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|b| {
                    let after = b.get("afterGroup").and_then(|x| x.as_u64())? as usize;
                    let min = b.get("minutes").and_then(|x| x.as_i64()).unwrap_or(15);
                    Some((after, min))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// "08:15" + 逐段间隔；中间可能插了加长休息（第 3 组后 20 分钟）。
/// 起止时间非法就退回第 1 组时间。
fn slot_time(index: usize, start: &str, interval_min: i64, breaks: &[(usize, i64)]) -> String {
    let parsed = start
        .trim()
        .split_once(':')
        .and_then(|(h, m)| Some((h.trim().parse::<i64>().ok()?, m.trim().parse::<i64>().ok()?)));
    let Some((h, m)) = parsed else {
        return start.to_string();
    };
    let mut total = h * 60 + m;
    for i in 0..index {
        let step = breaks
            .iter()
            .find(|(after, _)| *after == i + 1)
            .map(|(_, min)| *min)
            .unwrap_or(interval_min);
        total += step.max(0);
    }
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

/// 分组开放前的赛事公告状态。时间是固定 +08:00，避免生产容器时区不同导致倒计时错一天。
struct GroupNoticeView {
    json: serde_json::Value,
    revealed: bool,
}

struct GroupFreezeView {
    json: serde_json::Value,
    frozen: bool,
}

fn group_freeze_view_at(
    s: &crate::public_teams::PublicSettings,
    now: chrono::DateTime<chrono::FixedOffset>,
) -> Option<GroupFreezeView> {
    let naive =
        chrono::NaiveDateTime::parse_from_str(s.group_freeze_at.trim(), "%Y-%m-%dT%H:%M").ok()?;
    let at = naive.and_local_timezone(now.offset().clone()).single()?;
    let frozen = now >= at;
    let seconds_until = (at - now).num_seconds().max(0);
    Some(GroupFreezeView {
        frozen,
        json: json!({
            "enabled": true,
            "freezeAt": format!("{}+08:00", at.format("%Y-%m-%dT%H:%M:%S")),
            "label": at.format("%m月%d日 %H:%M").to_string(),
            "frozen": frozen,
            "secondsUntil": seconds_until,
            "status": if frozen { "frozen" } else { "countdown" },
        }),
    })
}

fn group_freeze_view(s: &crate::public_teams::PublicSettings) -> Option<GroupFreezeView> {
    let offset = chrono::FixedOffset::east_opt(8 * 3600)?;
    group_freeze_view_at(s, chrono::Utc::now().with_timezone(&offset))
}

fn group_notice_view_for_date_at(
    s: &crate::public_teams::PublicSettings,
    event_date: &str,
    now: chrono::DateTime<chrono::FixedOffset>,
) -> Option<GroupNoticeView> {
    if !s.group_notice_enabled {
        return None;
    }
    let event_date = chrono::NaiveDate::parse_from_str(event_date.trim(), "%Y-%m-%d").ok()?;
    let start_time = chrono::NaiveTime::parse_from_str(s.group_notice_start_time.trim(), "%H:%M")
        .unwrap_or_else(|_| chrono::NaiveTime::from_hms_opt(8, 15, 0).unwrap());
    let event_start = event_date.and_time(start_time);
    let lead = s.group_notice_reveal_lead_minutes.clamp(0, 1440);
    let reveal_at = event_start - chrono::Duration::minutes(lead);
    let event_end = event_date
        .succ_opt()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .unwrap_or(event_start);
    let local_now = now.naive_local();
    let revealed = local_now >= reveal_at && local_now < event_end;
    let ended = local_now >= event_end;
    let days_until = (event_date - now.date_naive()).num_days().max(0);
    let signup_date =
        chrono::NaiveDate::parse_from_str(s.group_notice_signup_date.trim(), "%Y-%m-%d").ok();
    let title = if s.group_notice_title.trim().is_empty() {
        "分组赛事公告".to_string()
    } else {
        s.group_notice_title.trim().to_string()
    };
    let offset = now.offset().to_string();
    Some(GroupNoticeView {
        revealed,
        json: json!({
            "enabled": true,
            "title": title,
            "eventDate": event_date.format("%Y-%m-%d").to_string(),
            "signupDate": signup_date.map(|d| d.format("%Y-%m-%d").to_string()),
            "signupStarted": signup_date.map(|d| now.date_naive() >= d).unwrap_or(false),
            "startTime": start_time.format("%H:%M").to_string(),
            "revealLeadMinutes": lead,
            "revealAt": format!("{}+08:00", reveal_at.format("%Y-%m-%dT%H:%M:%S")),
            "eventStartAt": format!("{}+08:00", event_start.format("%Y-%m-%dT%H:%M:%S")),
            "daysUntil": days_until,
            "revealed": revealed,
            "ended": ended,
            "status": if revealed { "revealed" } else if ended { "ended" } else { "countdown" },
            "timezone": offset,
        }),
    })
}

fn group_notice_view_at(
    s: &crate::public_teams::PublicSettings,
    now: chrono::DateTime<chrono::FixedOffset>,
) -> Option<GroupNoticeView> {
    group_notice_view_for_date_at(s, &s.group_notice_event_date, now)
}

fn group_notice_view(s: &crate::public_teams::PublicSettings) -> Option<GroupNoticeView> {
    let offset = chrono::FixedOffset::east_opt(8 * 3600)?;
    group_notice_view_at(s, chrono::Utc::now().with_timezone(&offset))
}

/// 公开详情页的展示模式：倒计时和分组互斥。
///
/// - 公告开启：显示倒计时，只有在开赛前 N 分钟到赛事当天结束之间才放榜；
/// - 公告关闭：直接放榜，不再要求“今天必须有数据”。
///
/// 方案自带 `eventDate` 时以方案日期为准；老方案没有日期时回落到全局公告日期。
fn plan_group_visibility_at(
    s: &crate::public_teams::PublicSettings,
    event_date: &str,
    now: chrono::DateTime<chrono::FixedOffset>,
) -> (bool, Option<GroupNoticeView>) {
    if !s.group_notice_enabled {
        return (true, None);
    }
    let notice = if event_date.trim().is_empty() {
        group_notice_view_at(s, now)
    } else {
        group_notice_view_for_date_at(s, event_date, now)
    };
    // 配置损坏（开了公告却没有有效日期）时退回分组，避免整页只剩空公告。
    let revealed = notice.as_ref().map(|n| n.revealed).unwrap_or(true);
    (revealed, notice)
}

fn plan_group_visibility(
    s: &crate::public_teams::PublicSettings,
    event_date: &str,
) -> (bool, Option<GroupNoticeView>) {
    let offset = chrono::FixedOffset::east_opt(8 * 3600).expect("valid +08:00 offset");
    plan_group_visibility_at(s, event_date, chrono::Utc::now().with_timezone(&offset))
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            wave: true,
            ranking: true,
            daily: true,
            groups: true,
        }
    }
}

impl DisplayConfig {
    fn from_payload(v: &serde_json::Value) -> Self {
        let flag = |key: &str| {
            v.get("display")
                .and_then(|d| d.get(key))
                .and_then(|x| x.as_bool())
        };
        Self {
            wave: flag("wave").unwrap_or(true),
            ranking: flag("ranking").unwrap_or(true),
            daily: flag("daily").unwrap_or(true),
            groups: flag("groups").unwrap_or(true),
        }
    }
}

/// 晋级赛方案里的 `wave` 是比赛分快照，不是日/月音浪。
/// 展示接口必须原样使用，不能被实时音浪覆盖或重新排序。
fn uses_snapshot_match_scores(event_type: &str, payload: &serde_json::Value) -> bool {
    matches!(event_type, "初级争霸赛晋级赛" | "中级争霸赛晋级赛")
        || matches!(
            as_str(payload, "scoreSource").as_deref(),
            Some("roster-score" | "match" | "snapshot")
        )
        || as_str(payload, "scoreKind").as_deref() == Some("match")
}

/// `GET /api/public/plans/{id}`：单个方案的公开展示数据
pub async fn plan(
    State(st): State<std::sync::Arc<AppState>>,
    Path(id): Path<i64>,
    headers: axum::http::HeaderMap,
) -> Response {
    let team = match crate::public_teams::public_identity_team(&st, &headers).await {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };
    // 争霸赛/晋级赛分组当前只属于男团；女团账号不返回方案详情。
    if team == "female" {
        return not_found();
    }
    let Some(row) = store::get_published_plan(&st, id).await else {
        return not_found();
    };
    /* 路由导航隐藏分组 / 某个赛事类型时，详情接口也必须同口径拒绝，
    否则知道方案 id 就能绕过列表开关直接拿到名单和音浪。 */
    let settings = crate::public_teams::PublicSettings::load(&st).await;
    let freeze_active = group_freeze_view(&settings)
        .map(|f| f.frozen)
        .unwrap_or(false);
    if !freeze_active
        && (settings.group_enabled == false
            || !crate::public_teams::plan_visible_by_settings(&settings, &row.event_type))
    {
        return not_found();
    }
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&row.payload) else {
        tracing::warn!(id, "方案 payload 不是合法 JSON");
        return not_found();
    };

    let snapshot_match_scores = uses_snapshot_match_scores(&row.event_type, &payload);
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
    let event_date = payload
        .get("eventDate")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let rules = payload
        .get("publicRules")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let mut display = DisplayConfig::from_payload(&payload);
    if snapshot_match_scores {
        // 比赛分项目没有“每日音浪明细”语义。
        display.daily = false;
    }
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
    let breaks = payload
        .get("schedule")
        .map(parse_breaks)
        .unwrap_or_default();

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
            slot: slot_time(gi, &start, interval, &breaks),
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
    if display.wave && !snapshot_match_scores && !person_ids.is_empty() {
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
    let (mut trend, mut daily) = if !snapshot_match_scores
        && display.daily
        && display.wave
        && display.groups
    {
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
                            // 当天是否开播（daily_metric.is_live），与 values 下标一一对应
                            let live: Vec<i16> = p
                                .person_id
                                .as_deref()
                                .and_then(|id| series.live.get(id))
                                .cloned()
                                .unwrap_or_else(|| vec![0; day_count]);
                            json!({ "name": p.name, "leader": p.leader, "values": values, "live": live })
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
    let mut ranking: Vec<_> = if display.wave {
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

    // 比赛日仅作为页面元信息保留；是否放榜由上面的“倒计时 / 分组”互斥模式决定。
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let match_day = dates.first().map(|d| d == &today).unwrap_or(false);
    let (mut group_reveal, mut notice_view) = plan_group_visibility(&settings, event_date);
    let freeze_view = group_freeze_view(&settings);
    // 到点后锁死名单展示：即使倒计时配置仍开着，也直接显示当前分组。
    if freeze_view.as_ref().map(|f| f.frozen).unwrap_or(false) {
        group_reveal = true;
        notice_view = None;
    }
    /* 展示开关必须落在接口层：关闭总排行时连每人的全场 rank 一起抹掉；
    关闭公开名单时连 groups / ranking / 每日明细一起不下发，不能只靠前端隐藏。 */
    if !display.ranking {
        for g in groups.iter_mut() {
            for p in g.players.iter_mut() {
                p.rank = 0;
            }
        }
        ranking.clear();
    }
    if !display.groups {
        groups.clear();
        ranking.clear();
    }
    if !group_reveal {
        groups.clear();
        ranking.clear();
        trend = None;
        daily = None;
    }
    let count_groups = groups.len();
    let count_players = groups.iter().map(|g| g.players.len()).sum::<usize>();

    Json(json!({
        "id": row.id,
        "title": row.title,
        "brand": as_str(&payload, "brand").unwrap_or_default(),
        "eventType": row.event_type,
        "eventDate": event_date,
        "rules": rules,
        "metric": metric,
        "days": days,
        "scoreKind": if snapshot_match_scores { "match" } else { "wave" },
        "schedule": { "start": start, "intervalMin": interval },
        "display": display,
        "matchDay": match_day,
        "groupReveal": group_reveal,
        "groupNotice": notice_view.map(|n| n.json),
        "groupFreeze": freeze_view.map(|f| f.json),
        "createdAt": row.created_at,
        "dates": dates,
        "stale": stale,
        "counts": {
            "groups": count_groups,
            "players": count_players,
            "totalWave": if display.wave && display.groups && display.ranking {
                ranking.iter().map(|p| p.wave).sum::<f64>()
            } else {
                0.0
            },
        },
        "groups": groups,
        "ranking": ranking,
        "trend": trend,
        "daily": daily,
    }))
    .into_response()
}

/// `GET /api/public/plans`：已发布方案的轻量列表
/// 2026-10-10 起受公开站点设置约束：「分组」页关掉 → 全部不下发；
/// 某赛事类型子页关掉 → 该类型方案按未上架处理。
pub async fn plans(
    State(st): State<std::sync::Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Response {
    let team = match crate::public_teams::public_identity_team(&st, &headers).await {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };
    let s = crate::public_teams::PublicSettings::load(&st).await;
    let freeze_active = group_freeze_view(&s).map(|f| f.frozen).unwrap_or(false);
    if team == "female" || (!freeze_active && s.group_enabled == false) {
        return Json(json!({ "items": [], "groupNotice": null })).into_response();
    }
    let items: Vec<_> = store::list_plans(&st)
        .await
        .into_iter()
        .filter(|p| p.published)
        .filter(|p| {
            freeze_active || crate::public_teams::plan_visible_by_settings(&s, &p.event_type)
        })
        .map(|p| {
            json!({
                "id": p.id,
                "title": p.title,
                "metric": p.metric,
                "days": p.days,
                "eventType": p.event_type,
                "createdAt": p.created_at,
            })
        })
        .collect();
    let group_notice = group_notice_view(&s).map(|n| n.json);
    let group_freeze = group_freeze_view(&s).map(|f| f.json);
    Json(json!({ "items": items, "groupNotice": group_notice, "groupFreeze": group_freeze }))
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice_cfg() -> crate::public_teams::PublicSettings {
        crate::public_teams::PublicSettings {
            group_notice_enabled: true,
            group_notice_title: "中旬争霸赛".into(),
            group_notice_event_date: "2026-11-20".into(),
            group_notice_signup_date: "2026-11-18".into(),
            group_notice_start_time: "08:15".into(),
            group_notice_reveal_lead_minutes: 120,
            ..Default::default()
        }
    }

    #[test]
    fn group_notice_counts_down_and_opens_two_hours_before_start() {
        let cfg = notice_cfg();
        let offset = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
        let before = chrono::NaiveDate::from_ymd_opt(2026, 11, 17)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_local_timezone(offset)
            .unwrap();
        let view = group_notice_view_at(&cfg, before).unwrap();
        assert!(!view.revealed);
        assert_eq!(view.json["daysUntil"], 3);
        assert_eq!(view.json["status"], "countdown");

        let reveal = chrono::NaiveDate::from_ymd_opt(2026, 11, 20)
            .unwrap()
            .and_hms_opt(6, 15, 0)
            .unwrap()
            .and_local_timezone(offset)
            .unwrap();
        assert!(group_notice_view_at(&cfg, reveal).unwrap().revealed);

        let ended = chrono::NaiveDate::from_ymd_opt(2026, 11, 21)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(offset)
            .unwrap();
        let ended = group_notice_view_at(&cfg, ended).unwrap();
        assert!(!ended.revealed);
        assert_eq!(ended.json["status"], "ended");
    }

    #[test]
    fn plan_without_event_date_uses_global_countdown_in_notice_mode() {
        let cfg = notice_cfg();
        let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 19)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_local_timezone(chrono::FixedOffset::east_opt(8 * 3600).unwrap())
            .unwrap();

        let (revealed, notice) = plan_group_visibility_at(&cfg, "", now);

        assert!(!revealed);
        assert_eq!(notice.unwrap().json["eventDate"], "2026-11-20");
    }

    #[test]
    fn notice_disabled_reveals_groups_without_waiting_for_match_day() {
        let mut cfg = notice_cfg();
        cfg.group_notice_enabled = false;
        let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 19)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_local_timezone(chrono::FixedOffset::east_opt(8 * 3600).unwrap())
            .unwrap();

        let (revealed, notice) = plan_group_visibility_at(&cfg, "", now);

        assert!(revealed);
        assert!(notice.is_none());
    }

    #[test]
    fn group_freeze_counts_down_then_locks_at_configured_time() {
        let mut cfg = notice_cfg();
        cfg.group_freeze_at = "2026-11-20T16:00".into();
        let offset = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
        let before = chrono::NaiveDate::from_ymd_opt(2026, 11, 20)
            .unwrap()
            .and_hms_opt(15, 59, 0)
            .unwrap()
            .and_local_timezone(offset)
            .unwrap();
        let view = group_freeze_view_at(&cfg, before).unwrap();
        assert!(!view.frozen);
        assert_eq!(view.json["secondsUntil"], 60);
        assert_eq!(view.json["status"], "countdown");

        let at = chrono::NaiveDate::from_ymd_opt(2026, 11, 20)
            .unwrap()
            .and_hms_opt(16, 0, 0)
            .unwrap()
            .and_local_timezone(offset)
            .unwrap();
        let view = group_freeze_view_at(&cfg, at).unwrap();
        assert!(view.frozen);
        assert_eq!(view.json["status"], "frozen");
    }

    #[test]
    fn plan_event_date_controls_reveal_instead_of_global_notice_date() {
        let mut cfg = notice_cfg();
        cfg.group_notice_event_date = "2026-11-25".into();
        let offset = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
        let before = chrono::NaiveDate::from_ymd_opt(2026, 11, 20)
            .unwrap()
            .and_hms_opt(6, 14, 59)
            .unwrap()
            .and_local_timezone(offset)
            .unwrap();

        let (revealed, notice) = plan_group_visibility_at(&cfg, "2026-11-20", before);

        assert!(!revealed);
        assert_eq!(notice.unwrap().json["eventDate"], "2026-11-20");

        let reveal_at = chrono::NaiveDate::from_ymd_opt(2026, 11, 20)
            .unwrap()
            .and_hms_opt(6, 15, 0)
            .unwrap()
            .and_local_timezone(offset)
            .unwrap();
        let (revealed, _) = plan_group_visibility_at(&cfg, "2026-11-20", reveal_at);
        assert!(revealed);
    }

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
        let s = |i: usize| slot_time(i, "08:15", 15, &[]);
        assert_eq!(s(0), "08:15");
        assert_eq!(s(1), "08:30");
        assert_eq!(s(3), "09:00");
    }

    #[test]
    fn slot_time_handles_bad_input_and_midnight() {
        // 起止时间不合法就原样返回，别把页面搞崩
        assert_eq!(slot_time(2, "abc", 15, &[]), "abc");
        // 跨过午夜要回绕，不能出现 25:00
        assert_eq!(slot_time(1, "23:55", 15, &[]), "00:10");
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
    fn promotion_plans_are_snapshot_match_scores() {
        let empty = serde_json::json!({});
        assert!(uses_snapshot_match_scores("中级争霸赛晋级赛", &empty));
        assert!(uses_snapshot_match_scores(
            "月终争霸赛",
            &serde_json::json!({ "scoreSource": "snapshot" })
        ));
        assert!(uses_snapshot_match_scores(
            "月终争霸赛",
            &serde_json::json!({ "scoreKind": "match" })
        ));
        assert!(!uses_snapshot_match_scores("中级争霸赛小组赛", &empty));
    }

    #[test]
    fn as_str_drops_empty_strings() {
        let v = serde_json::json!({ "a": "", "b": "有值" });
        assert_eq!(as_str(&v, "a"), None);
        assert_eq!(as_str(&v, "b").as_deref(), Some("有值"));
        assert_eq!(as_str(&v, "缺失"), None);
    }
}
