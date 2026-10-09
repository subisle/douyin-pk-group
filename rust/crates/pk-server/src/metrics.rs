//! 指标聚合（口径从 3328 metric/httpapi 移植）：
//! - dashboard：KPI + 团别趋势 + 未播预警 + TopN，一次取最近 N 天在内存里算
//! - monthly / yearly：日指标聚合出月榜 / 年榜（on-the-fly，展示端 08 的月/年 tab 数据源）

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::dialect;
use crate::mysql;
use crate::routes::ApiError;
use crate::state::AppState;

type ApiResult = Result<Json<Value>, ApiError>;
type ArcState = Arc<AppState>;

fn valid_date(s: &str) -> bool {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

fn read_id(row: &sqlx::mysql::MySqlRow, col: &str) -> u64 {
    if let Ok(v) = row.try_get::<u64, _>(col) {
        return v;
    }
    if let Ok(v) = row.try_get::<i64, _>(col) {
        return v as u64;
    }
    0
}

fn read_u64(row: &sqlx::mysql::MySqlRow, col: &str) -> u64 {
    read_id(row, col)
}

/* ------------------------------- dashboard ------------------------------- */

#[derive(Deserialize)]
pub struct DashboardQuery {
    /// 统计基准日（默认最新采集日）
    pub date: Option<String>,
    /// 趋势窗口天数（7–90，默认 30）
    pub days: Option<u32>,
    /// TopN（默认 5）
    pub top: Option<u32>,
}

/// date -> (person_id -> (wave, minutes, live, name, gender))
type PerDay = BTreeMap<String, HashMap<u64, (i64, i64, i8, String, String)>>;

/// GET /api/admin/dashboard?date=&days=&top=
pub async fn dashboard(State(st): State<ArcState>, Query(q): Query<DashboardQuery>) -> ApiResult {
    let info = crate::repos::get_dates_info(&st, Some("LOCAL"), false).await?;
    let dates = &info.dates;
    let baseline = info.baseline.clone();
    // 默认基准日：不取残缺的「今天」（数据每晚 24 点后补），回退到 ≤ 自然昨天的
    // 最新采集日；再回退最新日。与展示端「默认日期 = 当天-1」口径一致。
    let yesterday = (chrono::Local::now().date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let date = match q.date.as_deref() {
        Some(d) if valid_date(d) => d.to_string(),
        _ => dates
            .iter()
            .filter(|d| Some(d.as_str()) != baseline.as_deref() && d.as_str() <= yesterday.as_str())
            .max()
            .cloned()
            .or_else(|| {
                dates
                    .iter()
                    .filter(|d| Some(d.as_str()) != baseline.as_deref())
                    .max()
                    .cloned()
            })
            .or_else(|| dates.first().cloned())
            .unwrap_or_default(),
    };
    if date.is_empty() {
        return Err(anyhow::anyhow!("还没有采集到任何日期").into());
    }
    let days = q.days.unwrap_or(30).clamp(7, 90) as i64;
    let top_n = q.top.unwrap_or(5).clamp(1, 50) as i64;

    let anchor = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d")
        .map_err(|_| anyhow::anyhow!("date 格式应为 YYYY-MM-DD"))?;
    let start = (anchor - chrono::Duration::days(days - 1))
        .format("%Y-%m-%d")
        .to_string();

    // 一次取窗口内全部日指标，内存聚合（200 人 × 90 天 = 万级行，毫秒级）
    let sql = dialect::trend_sql(&start, &date);
    let rows = mysql::query(&st.pools, "LOCAL", &sql).await?;
    // date -> (person_id -> (wave, minutes, live, name, gender))
    let mut per_day: PerDay = Default::default();
    for r in &rows {
        let d: String = r.try_get::<String, _>("d").unwrap_or_default();
        let pid = read_id(r, "person_id");
        let wave = read_u64(r, "w") as i64;
        let minutes = read_u64(r, "m") as i64;
        let live: i8 = r.try_get::<i8, _>("live").unwrap_or(0);
        let name: String = r.try_get::<String, _>("name").unwrap_or_default();
        let gender: String = r.try_get::<String, _>("gender").unwrap_or_default();
        per_day
            .entry(d)
            .or_default()
            .insert(pid, (wave, minutes, live, name, gender));
    }
    // 只保留有效采集日并剔除基线快照日（09-21 基线 1490 万会污染趋势/预警）
    per_day.retain(|d, _| dates.contains(d) && Some(d.as_str()) != baseline.as_deref());

    // KPI：基准日当天
    let today_rows = per_day.get(&date);
    let total_wave: i64 = today_rows
        .map(|m| m.values().map(|(w, _, _, _, _)| w).sum())
        .unwrap_or(0);
    let male_wave: i64 = today_rows
        .map(|m| {
            m.values()
                .filter(|(_, _, _, _, g)| g == "male")
                .map(|(w, _, _, _, _)| w)
                .sum()
        })
        .unwrap_or(0);
    let female_wave: i64 = today_rows
        .map(|m| {
            m.values()
                .filter(|(_, _, _, _, g)| g == "female")
                .map(|(w, _, _, _, _)| w)
                .sum()
        })
        .unwrap_or(0);
    let live_count = today_rows
        .map(|m| m.values().filter(|(_, _, l, _, _)| *l == 1).count())
        .unwrap_or(0);
    // 在册人数 = 全部未删除且未离开的主播（不是「当天有数据的人数」）
    let person_count = mysql::query(&st.pools, "LOCAL", dialect::ACTIVE_PERSON_COUNT_SQL)
        .await?
        .first()
        .map(|r| read_u64(r, "n"))
        .unwrap_or(0) as usize;

    // 趋势（团别聚合，按日期正序）
    let trend: Vec<Value> = per_day
        .iter()
        .map(|(d, persons)| {
            let mut male = 0i64;
            let mut female = 0i64;
            for (w, _, _, _, g) in persons.values() {
                match g.as_str() {
                    "male" => male += w,
                    "female" => female += w,
                    _ => {}
                }
            }
            json!({ "date": d, "male": male, "female": female, "total": male + female })
        })
        .collect();

    // TopN：基准日 wave 降序
    let mut top: Vec<Value> = today_rows
        .map(|m| {
            m.iter()
                .map(|(pid, (wave, minutes, _, name, gender))| {
                    json!({ "personId": pid, "name": name, "team": gender, "wave": wave, "minutes": minutes })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    top.sort_by_key(|v| -v.get("wave").and_then(|w| w.as_i64()).unwrap_or(0));
    top.truncate(top_n as usize);

    // 未播预警：窗口内有数据、截至基准日连续未播天数（≥2 预警；从未开播 = -1）
    let mut last_live: HashMap<u64, &String> = Default::default();
    let mut seen: HashMap<u64, &String> = Default::default();
    for (d, persons) in &per_day {
        for (pid, (_, _, live, name, _)) in persons {
            seen.insert(*pid, name);
            if *live == 1 {
                last_live.insert(*pid, d);
            }
        }
    }
    let mut absent: Vec<Value> = seen
        .iter()
        .map(|(pid, name)| {
            let days = last_live
                .get(pid)
                .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                .map(|last| (anchor - last).num_days());
            json!({
                "personId": pid,
                "name": name,
                "days": days.unwrap_or(-1), // -1 = 窗口内从未开播
            })
        })
        .filter(|v| {
            let d = v.get("days").and_then(|x| x.as_i64()).unwrap_or(0);
            d >= 2 || d == -1
        })
        .collect();
    absent.sort_by_key(|v| -v.get("days").and_then(|d| d.as_i64()).unwrap_or(0));

    Ok(Json(json!({
        "summary": {
            "date": date,
            "totalWave": total_wave,
            "maleWave": male_wave,
            "femaleWave": female_wave,
            "liveCount": live_count,
            "personCount": person_count,
        },
        "trend": trend,
        "absent": absent,
        "top": top,
    })))
}

/* ------------------------------- 月度 / 年度 ------------------------------- */

#[derive(Deserialize)]
pub struct MetricsQuery {
    /// 月：YYYY-MM；年：YYYY
    pub period: Option<String>,
    pub team: Option<String>,
}

/// GET /api/admin/metrics/monthly?period=2026-10&team=male
pub async fn monthly(State(st): State<ArcState>, Query(q): Query<MetricsQuery>) -> ApiResult {
    period_metrics(&st, q, true).await
}

/// GET /api/admin/metrics/yearly?period=2026&team=female
pub async fn yearly(State(st): State<ArcState>, Query(q): Query<MetricsQuery>) -> ApiResult {
    period_metrics(&st, q, false).await
}

async fn period_metrics(st: &AppState, q: MetricsQuery, is_month: bool) -> ApiResult {
    let period = match q.period.as_deref() {
        Some(p) if !p.trim().is_empty() => p.trim().to_string(),
        _ if is_month => chrono::Local::now().format("%Y-%m").to_string(),
        _ => chrono::Local::now().format("%Y").to_string(),
    };
    let ok_chars = period.chars().all(|c| c.is_ascii_digit() || c == '-');
    let len_ok = if is_month {
        period.len() == 7
    } else {
        period.len() == 4
    };
    if !ok_chars || !len_ok {
        return Err(anyhow::anyhow!(if is_month {
            "period 格式应为 YYYY-MM"
        } else {
            "period 格式应为 YYYY"
        })
        .into());
    }
    let team = q.team.as_deref().unwrap_or("");
    if !team.is_empty() && team != "male" && team != "female" {
        return Err(anyhow::anyhow!("team 只能是 male / female").into());
    }

    let sql = dialect::period_metrics_sql(&format!("{period}%"), team);
    let rows = mysql::query(&st.pools, "LOCAL", &sql).await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "personId": read_id(r, "person_id"),
                "name": r.try_get::<String, _>("name").unwrap_or_default(),
                "team": r.try_get::<String, _>("gender").unwrap_or_default(),
                "wave": read_u64(r, "wave"),
                "minutes": read_u64(r, "minutes"),
                "liveDays": read_u64(r, "live_days"),
                "dataDays": read_u64(r, "data_days"),
            })
        })
        .collect();
    // 月度聚合落库（monthly_metric，docs/00 §4 核心实体；查询端当前 on-the-fly，
    // 落库供公开端/历史快照直接读取）
    if is_month {
        if let Ok(pool) = mysql::pool_of(&st.pools, "LOCAL") {
            for it in &items {
                let pid = it.get("personId").and_then(|v| v.as_u64()).unwrap_or(0);
                let wave = it.get("wave").and_then(|v| v.as_i64()).unwrap_or(0);
                let minutes = it.get("minutes").and_then(|v| v.as_i64()).unwrap_or(0);
                let live_days = it.get("liveDays").and_then(|v| v.as_i64()).unwrap_or(0);
                if pid == 0 {
                    continue;
                }
                let _ = sqlx::query(
                    "INSERT INTO monthly_metric (person_id, period, wave, live_days, recomputed_at)
                     VALUES (?,?,?,?,NOW(3))
                     ON DUPLICATE KEY UPDATE wave = VALUES(wave), live_days = VALUES(live_days),
                       minutes = VALUES(minutes), recomputed_at = NOW(3)",
                )
                .bind(pid as i64)
                .bind(&period)
                .bind(wave)
                .bind(live_days as i32)
                .bind(minutes)
                .execute(&pool)
                .await;
            }
        }
    }
    Ok(Json(
        json!({ "period": period, "team": team, "items": items }),
    ))
}
