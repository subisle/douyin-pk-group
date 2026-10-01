//! 数据仓库：主播表（带缓存）、音浪（带缓存 + 口径计算）

use std::collections::HashMap;

use sqlx::Row;

use crate::config::wave_cache_ttl_secs;
use crate::dialect::{dialect_of, inline_dates};
use crate::mysql;
use crate::state::{AnchorBrief, AppState, Cached, DateInfo, WaveInfo, WaveMap, WaveSeries};

/// 主键在不同库/不同版本里可能是 BIGINT UNSIGNED / BIGINT / INT / VARCHAR，
/// 逐个类型尝试，都不行就返回 None
fn read_id(row: &sqlx::mysql::MySqlRow, idx: usize) -> Option<String> {
    if let Ok(v) = row.try_get::<u64, _>(idx) {
        return Some(v.to_string());
    }
    if let Ok(v) = row.try_get::<i64, _>(idx) {
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
    let rows = mysql::query(&st.pools, &id, dia.dates_sql).await?;
    let mut dates: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<Option<String>, _>(0).ok().flatten())
        .collect();
    dates.sort_unstable_by(|a, b| b.cmp(a)); // 倒序
    dates.dedup();

    let baseline = match dia.baseline_sql {
        Some(sql) => mysql::query(&st.pools, &id, sql)
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

pub async fn get_baseline_date(
    st: &AppState,
    source: Option<&str>,
    force: bool,
) -> anyhow::Result<Option<String>> {
    Ok(get_dates_info(st, source, force).await?.baseline)
}

/// 音浪口径：
/// - total：全部日期求和 = 基线日累计快照 + 之后每日增量 ≈ 真实累计总音浪
/// - lastN：最近 N 个「增量日」求和（排除基线日，避免把累计快照算进近几日）
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
    // lastN 只在增量日里取近 N 天；total 用全部日期（含基线日累计基数）
    let delta_dates: Vec<String> = match &info.baseline {
        Some(b) => info.dates.iter().filter(|d| *d != b).cloned().collect(),
        None => info.dates.clone(),
    };
    let use_dates: Vec<String> = if metric == "lastN" {
        delta_dates.into_iter().take(days.max(1) as usize).collect()
    } else {
        info.dates.clone()
    };

    let cache_key = format!("{id}:{metric}:{days}");
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
                let rows = mysql::query(&st.pools, &id, &sql).await?;
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
/// 基线日会被剔除：它记的是累计快照而不是当日增量，混进曲线会变成一根假尖峰。
pub async fn get_wave_series(
    st: &AppState,
    force: bool,
    source: Option<&str>,
) -> anyhow::Result<WaveSeries> {
    let id = st.source(source)?.id.clone();
    let info = get_dates_info(st, Some(&id), force).await?;

    // 只取增量日（去掉基线日），再截取最近的一段
    let mut dates: Vec<String> = match &info.baseline {
        Some(b) => info.dates.iter().filter(|d| *d != b).cloned().collect(),
        None => info.dates.clone(),
    };
    // 点太多挤在一起反而看不清趋势，够看趋势就行
    const MAX_POINTS: usize = 14;
    if dates.len() > MAX_POINTS {
        dates = dates.split_off(dates.len() - MAX_POINTS);
    }
    // 折线图要正序：老 → 新
    dates.reverse();

    let cache_key = format!("{id}:series:v2-live");
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
    let mut live: HashMap<String, Vec<i8>> = HashMap::new();
    if !dates.is_empty() {
        let sql = inline_dates(dialect_of(&id).series_sql, &dates)?;
        let rows = mysql::query(&st.pools, &id, &sql).await?;
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
            // MAX(is_live) 聚出来的可能是 i8/int；解析不了按未开播处理
            let l: i8 = r.try_get::<Option<i8>, _>(3).ok().flatten().unwrap_or(0);
            series.entry(pid.clone()).or_insert_with(|| vec![0.0; dates.len()])[i] = v;
            live.entry(pid).or_insert_with(|| vec![0; dates.len()])[i] = l;
        }
    }

    let out = WaveSeries { dates, series, live };
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
    let rows = mysql::query(&st.pools, &id, dialect_of(&id).anchors_sql).await?;
    let mut brief: Vec<AnchorBrief> = Vec::with_capacity(rows.len());
    for r in &rows {
        // id 在 A 库是 BIGINT、B 库是 INT，统一按 i64 读再转字符串；万一变 VARCHAR 也兜得住
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
        let avatar: Option<String> = r.try_get::<Option<String>, _>(2).ok().flatten();
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
