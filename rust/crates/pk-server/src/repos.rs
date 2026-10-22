//! 数据仓库：主播表（带缓存）、音浪（带缓存 + 口径计算）

use std::collections::HashMap;

use sqlx::Row;

use crate::config::wave_cache_ttl_secs;
use crate::dialect::{dialect_of, inline_dates};
use crate::pgrs;
use crate::state::{AnchorBrief, AppState, Cached, DateInfo, WaveInfo, WaveMap, WaveSeries};

/// 主键在不同库/不同版本里可能是 BIGINT / INT / VARCHAR，
/// 逐个类型尝试，都不行就返回 None
fn read_id(row: &sqlx::postgres::PgRow, idx: usize) -> Option<String> {
    if let Ok(v) = row.try_get::<i64, _>(idx) {
        return Some(v.to_string());
    }
    /* 第二分支原来逐字抄了第一句（死代码）：列的候选类型是 INT4 而不是再来一遍 BIGINT。
    2026-10-14 审计 P2 同类清理 —— 少了它，INT4 主键的库只能靠下面的 String 兜底。 */
    if let Ok(v) = row.try_get::<i32, _>(idx) {
        return Some(v.to_string());
    }
    row.try_get::<String, _>(idx).ok()
}

/// 读取日期信息（全部采集日期 + 基线日标记）
pub async fn get_dates_info(
    st: &AppState,
    source: Option<&str>,
    force: bool,
) -> anyhow::Result<DateInfo> {
    let id = st.source(source)?.id.clone();
    let ttl = st.wave_ttl;
    if !force {
        let cache = st.cache.lock().expect("cache 锁 poisoned");
        if let Some(hit) = cache.dates.get(&id).and_then(|c| c.fresh(ttl)) {
            return Ok(hit.clone());
        }
    }
    let dia = dialect_of(&id);
    let rows = pgrs::query(&st.pools, &id, dia.dates_sql).await?;
    let mut dates: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<Option<String>, _>(0).ok().flatten())
        .collect();
    dates.sort_unstable_by(|a, b| b.cmp(a)); // 倒序
    dates.dedup();

    let baseline = match dia.baseline_sql {
        Some(sql) => pgrs::query(&st.pools, &id, sql)
            .await?
            .first()
            .and_then(|r| r.try_get::<Option<String>, _>(0).ok().flatten()),
        None => None,
    };

    let info = DateInfo { dates, baseline };
    st.cache
        .lock()
        .expect("cache 锁 poisoned")
        .dates
        .insert(id, Cached::new(info.clone()));
    Ok(info)
}

pub async fn get_available_dates(
    st: &AppState,
    source: Option<&str>,
    force: bool,
) -> anyhow::Result<Vec<String>> {
    Ok(get_dates_info(st, source, force).await?.dates)
}

/// 已导入过整月时长或整月音浪的月份（YYYY-MM，倒序）。
///
/// 2026-10-22：展示端原先只从 daily_metric 的日数据推导可选月份，导致
/// 只有整月表数据、没有日数据的 7/8/9 月无法进入月榜选择器。这里把两张
/// 整月表里的 period 一并取回；任一表缺失只忽略该表，不影响站点启动。
pub async fn get_imported_month_periods(st: &AppState, source: Option<&str>) -> Vec<String> {
    let Ok(src) = st.source(source) else { return Vec::new() };
    let id = src.id.clone();
    let mut out: Vec<String> = Vec::new();
    for table in ["monthly_duration", "monthly_wave"] {
        let sql = format!("SELECT DISTINCT period FROM {table} WHERE period <> ''");
        match pgrs::query(&st.pools, &id, &sql).await {
            Ok(rows) => out.extend(
                rows.iter()
                    .filter_map(|r| r.try_get::<String, _>(0).ok())
                    .filter(|p| p.len() == 7 && p.as_bytes().get(4) == Some(&b'-')),
            ),
            Err(e) => tracing::debug!(source = %id, table, error = %e, "读取整月数据月份失败，按空处理"),
        }
    }
    out.sort_unstable_by(|a, b| b.cmp(a));
    out.dedup();
    out
}

pub async fn get_baseline_date(
    st: &AppState,
    source: Option<&str>,
    force: bool,
) -> anyhow::Result<Option<String>> {
    Ok(get_dates_info(st, source, force).await?.baseline)
}

/// 音浪口径（P1-7/P1-8，2026-10-10 修订）：
/// - 行级过滤：所有聚合 SQL（WAVES_A/SERIES_A 等）统一 `wave_reliable = 1`，
///   一天里个别不可信行被剔除，**不再整天消失**。
/// - total：包含基线日。基线日（wave_reliable=0 的最早一天）记的是「累计快照」，
///   是后续差分的累计基数；SUM 里那天的 wave 本身是 0（不可信行被行级过滤掉），
///   但日期要保留在 dates 里，否则「累计总音浪」会少算基线日的基数 ——
///   与 WAVES_A「可靠行求和」口径保持一致。
/// - lastN / month：只取「增量日」（排除基线日），避免把累计快照翻倍算进近几日。
pub async fn get_waves(
    st: &AppState,
    metric: &str,
    days: i64,
    person_ids: Option<&[String]>,
    force: bool,
    source: Option<&str>,
) -> anyhow::Result<WaveInfo> {
    let id = st.source(source)?.id.clone();
    let info = get_dates_info(st, Some(&id), force).await?;
    // lastN 只在增量日里取近 N 天；total 用全部日期（含基线日，见函数头口径说明）
    let delta_dates: Vec<String> = match &info.baseline {
        Some(b) => info.dates.iter().filter(|d| *d != b).cloned().collect(),
        None => info.dates.clone(),
    };
    let use_dates: Vec<String> = if metric == "lastN" || metric == "last1" {
        // last1 = 最近 1 个有效业务日（等价于 lastN 天数填 1，单独给个名字方便前端下拉）
        let n: usize = if metric == "last1" {
            1
        } else {
            days.max(1) as usize
        };
        delta_dates.into_iter().take(n).collect()
    } else if metric == "month" {
        // 本月累计：只取「最新数据所在自然月」的增量日。
        // 基线日已排除（它记的是累计快照，算进来会把整月数据翻一倍）。
        match delta_dates.first() {
            Some(d) => {
                let prefix = d[..d.len().min(7)].to_string(); // YYYY-MM
                delta_dates
                    .into_iter()
                    .filter(|x| x.starts_with(&prefix))
                    .collect()
            }
            None => Vec::new(),
        }
    } else {
        info.dates.clone()
    };

    let cache_key = format!("{id}:v3-merge:{metric}:{days}");
    let cached: Option<WaveMap> = {
        let cache = st.cache.lock().expect("cache 锁 poisoned");
        if force {
            None
        } else {
            cache
                .waves
                .get(&cache_key)
                .and_then(|c| c.fresh(st.wave_ttl))
                .cloned()
        }
    };

    let waves = match cached {
        Some(w) => w,
        None => {
            let mut w: WaveMap = HashMap::new();
            if !use_dates.is_empty() {
                let sql = inline_dates(dialect_of(&id).waves_sql, &use_dates)?;
                let rows = pgrs::query(&st.pools, &id, &sql).await?;
                for r in rows {
                    let v: f64 = r.try_get::<Option<f64>, _>(1).ok().flatten().unwrap_or(0.0);
                    if let Some(k) = read_id(&r, 0) {
                        w.insert(k, v);
                    }
                }
            }
            st.cache
                .lock()
                .expect("cache 锁 poisoned")
                .waves
                .insert(cache_key, Cached::new(w.clone()));
            w
        }
    };

    Ok(WaveInfo {
        metric: metric.to_string(),
        days,
        dates: use_dates,
        waves: filter_ids(waves, person_ids),
    })
}

/// 每日音浪序列（展示页曲线图数据源）。
///
/// 与 `get_waves` 的区别：那边把区间压成一个总数，这边按 (人, 日期) 展开成
/// 与 `dates` 等长、下标一一对应的数组，前端画折线时不用再自己对齐日期。
///
/// 基线日（P1-8 修订）：**不再按日期整列剔除**。日期轴保留基线日，
/// 值由 series_sql 的行级 `wave_reliable = 1` 过滤决定 —— 基线日全天不可信时
/// 自然聚合出 0，个别行不可信时只丢那一行，整天数据不再消失。
/// （基线日那天记的是累计快照而不是当日增量，SQL 的 reliable 过滤会把它归零，
/// 曲线上不会出现假尖峰。）
pub async fn get_wave_series(
    st: &AppState,
    force: bool,
    source: Option<&str>,
) -> anyhow::Result<WaveSeries> {
    let id = st.source(source)?.id.clone();
    let info = get_dates_info(st, Some(&id), force).await?;

    // 日期轴含基线日（P1-8：行级 reliable 过滤代替整天剔除），再截取最近的一段
    let mut dates: Vec<String> = info.dates.clone();
    // 点太多挤在一起反而看不清趋势，够看趋势就行。
    // dates 此时是倒序（新 → 旧），截断必须保留头部；split_off 取尾部会把最新一天丢掉
    const MAX_POINTS: usize = 14;
    if dates.len() > MAX_POINTS {
        dates.truncate(MAX_POINTS);
    }
    // 折线图要正序：老 → 新
    dates.reverse();

    let cache_key = format!("{id}:series:v3-merge");
    if !force {
        let cache = st.cache.lock().expect("cache 锁 poisoned");
        if let Some(hit) = cache
            .series
            .get(&cache_key)
            .and_then(|c| c.fresh(st.wave_ttl))
        {
            return Ok(hit.clone());
        }
    }

    let mut series: HashMap<String, Vec<f64>> = HashMap::new();
    let mut live: HashMap<String, Vec<i16>> = HashMap::new();
    if !dates.is_empty() {
        let sql = inline_dates(dialect_of(&id).series_sql, &dates)?;
        let rows = pgrs::query(&st.pools, &id, &sql).await?;
        let index: HashMap<&str, usize> = dates
            .iter()
            .enumerate()
            .map(|(i, d)| (d.as_str(), i))
            .collect();
        for r in &rows {
            let Some(pid) = read_id(r, 0) else { continue };
            let Some(d) = r.try_get::<Option<String>, _>(1).ok().flatten() else {
                continue;
            };
            let Some(&i) = index.get(d.as_str()) else {
                continue;
            };
            let v: f64 = r.try_get::<Option<f64>, _>(2).ok().flatten().unwrap_or(0.0);
            /* SERIES_A 的 live 列是 `MAX(is_live)`（smallint）与 `CASE ... THEN 1 ELSE 0 END`
            （integer）的 UNION ALL —— PG 会统一成 **integer**（实库 pg_typeof 已确认），
            按 i16 读每行都 ColumnDecode 再被吞成 0（折线图开播标记恒 0）。
            2026-10-14 审计 P0-2：按 i32 读，再显式收窄成 0/1 存进 Vec<i16>。 */
            let l: i16 = r.try_get::<Option<i32>, _>(3).ok().flatten().unwrap_or(0) as i16;
            series
                .entry(pid.clone())
                .or_insert_with(|| vec![0.0; dates.len()])[i] = v;
            live.entry(pid).or_insert_with(|| vec![0; dates.len()])[i] = l;
        }
    }

    let out = WaveSeries {
        dates,
        series,
        live,
    };
    st.cache
        .lock()
        .expect("cache 锁 poisoned")
        .series
        .insert(cache_key, Cached::new(out.clone()));
    Ok(out)
}

fn filter_ids(data: WaveMap, person_ids: Option<&[String]>) -> WaveMap {
    match person_ids {
        Some(ids) if !ids.is_empty() => {
            let set: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
            data.into_iter()
                .filter(|(k, _)| set.contains(k.as_str()))
                .collect()
        }
        _ => data,
    }
}

pub async fn load_anchors(
    st: &AppState,
    source: Option<&str>,
    force: bool,
) -> anyhow::Result<Vec<AnchorBrief>> {
    let id = st.source(source)?.id.clone();
    if !force {
        let cache = st.cache.lock().expect("cache 锁 poisoned");
        if let Some(hit) = cache.anchors.get(&id).and_then(|c| c.fresh(st.wave_ttl)) {
            return Ok(hit.clone());
        }
    }
    let rows = pgrs::query(&st.pools, &id, dialect_of(&id).anchors_sql).await?;
    let mut brief: Vec<AnchorBrief> = Vec::with_capacity(rows.len());
    for r in &rows {
        // id 在不同库可能是 BIGINT / INT，统一按 i64 读再转字符串；万一变 VARCHAR 也兜得住
        let Some(person_id) = read_id(r, 0) else {
            continue;
        };
        let name: String = match r.try_get::<String, _>(1) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(person_id, "主播行 name 读取失败: {e}");
                continue;
            }
        };
        // 头像：优先用 person_avatar 本地留存（迁移 025），没有本地图时回落到旧 person.avatar_url。
        // person.avatar_url 自 2026-10-16 起不再由 avatar_sync 写入（基本恒空），仅作历史兜底。
        let avatar: Option<String> = {
            let legacy = r.try_get::<Option<String>, _>(2).ok().flatten();
            let hash = r.try_get::<Option<String>, _>(4).ok().flatten();
            match hash {
                Some(h) if !h.trim().is_empty() => {
                    Some(format!("/api/public/avatars/{person_id}?v={}", h.trim()))
                }
                _ => legacy.filter(|u| !u.trim().is_empty()),
            }
        };
        brief.push(AnchorBrief {
            person_id,
            name,
            avatar,
        });
    }
    if brief.is_empty() && !rows.is_empty() {
        tracing::warn!(
            "主播查询返回 {} 行但全部解析失败（source={id}）",
            rows.len()
        );
    }
    st.cache
        .lock()
        .expect("cache 锁 poisoned")
        .anchors
        .insert(id, Cached::new(brief.clone()));
    Ok(brief)
}

pub async fn search_anchors(
    st: &AppState,
    q: &str,
    limit: usize,
    source: Option<&str>,
) -> anyhow::Result<Vec<AnchorBrief>> {
    let all = load_anchors(st, source, false).await?;
    let key = q.trim();
    if key.is_empty() {
        return Ok(all.into_iter().take(limit).collect());
    }
    let mut exact: Vec<AnchorBrief> = all.iter().filter(|a| a.name == key).cloned().collect();
    exact.extend(
        all.iter()
            .filter(|a| a.name != key && a.name.contains(key))
            .cloned(),
    );
    Ok(exact.into_iter().take(limit).collect())
}

/// 刷新缓存用：强制重取日期 + 主播
pub async fn refresh(st: &AppState) -> anyhow::Result<()> {
    st.clear_cache();
    get_dates_info(st, None, true).await?;
    load_anchors(st, None, true).await?;
    Ok(())
}

pub fn ttl_from_env() -> std::time::Duration {
    std::time::Duration::from_secs(wave_cache_ttl_secs())
}
