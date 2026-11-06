//! 师门日报：师傅收「名下徒弟音浪图 + 与上日的差距」（docs/23 §3）。
//!
//! 由 push.rs 的延迟推送队列触发（`push_bindings.lineage_report` 开关），
//! 图片口径与全团日报一致（wave_reliable=1 / 未播=当日音浪<2 / 直系徒弟）。
//! 渲染产物为分页 SVG（`render_svg_pages`），PNG 转换复用 push.rs::svg_to_png
//! （由调用方处理，本模块只产出 SVG 字符串）。

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Datelike;
use sqlx::Row;

use crate::pgrs;
use crate::push::fmt_wan;
use crate::report::LineageReportFields;
use crate::state::AppState;

/* ------------------------------ 数据结构 ------------------------------ */

/// 单行：师傅行固定第一（is_master=true），其后徒弟按今日音浪降序、姓名升序。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineagePeriod {
    Daily,
    Monthly,
    Range,
}

#[derive(Debug, Clone)]
pub struct LineageRow {
    pub name: String,
    pub today: i64,
    /// 当日累计有效时长（分钟）；用于「一天不低于 4 小时」筛选，不直接渲染。
    pub today_minutes: i64,
    /// 昨日音浪；昨日 map 里没有该人 = None（渲染为 `—`）
    pub yesterday: Option<i64>,
    pub month_total: i64,
    /// 本月累计时长（分钟）；月度报表的主字段之一。
    pub month_minutes: i64,
    /// 日榜 = 当日开播；月榜 = 本月音浪 >= 2。
    pub live: bool,
    pub is_master: bool,
}

#[derive(Debug, Clone)]
pub struct LineageReport {
    pub master_name: String,
    /// 日报为 YYYY-MM-DD；月报为 YYYY-MM。
    pub date: String,
    pub period: LineagePeriod,
    pub rows: Vec<LineageRow>,
    pub total_today: i64,
    /// 昨日有值的行求和；昨日全无数据 = None
    pub total_yesterday: Option<i64>,
    /// 未播徒弟姓名（不含师傅），按 rows 顺序
    pub absent_names: Vec<String>,
    /// 日报为单日标准；区间总览为「单日标准 × 天数」后的累计合格线。
    pub daily_qualified_minutes: i64,
    /// 日报为单日红线；区间总览为「单日红线 × 天数」后的累计严重不足线。
    pub daily_severe_minutes: i64,
    /// 区间总览的起止日期与自然日数；其他报表为 None / 0。
    pub range_start: Option<String>,
    pub range_end: Option<String>,
    pub range_days: usize,
}

impl LineageReport {
    /// 较昨日合计：今日合计 − 昨日合计；昨日无数据 = None（显示 `—`）
    pub fn total_diff(&self) -> Option<i64> {
        Some(self.total_today - self.total_yesterday?)
    }

    /// 徒弟数（不含师傅行）
    pub fn disciple_count(&self) -> usize {
        self.rows.iter().filter(|r| !r.is_master).count()
    }

    /// 设置日报按天计算的三级标准：>= qualified 合格，>= severe 未达标，< severe 严重不足。
    pub fn set_daily_minutes_standard(&mut self, qualified_minutes: i64, severe_minutes: i64) {
        self.daily_qualified_minutes = qualified_minutes.max(0);
        self.daily_severe_minutes = severe_minutes.max(0);
    }

    /// 本月累计音浪（含师傅行）。
    pub fn month_wave_total(&self) -> i64 {
        self.rows.iter().map(|r| r.month_total).sum()
    }

    /// 本月累计时长（分钟，含师傅行）。
    pub fn month_minutes_total(&self) -> i64 {
        self.rows.iter().map(|r| r.month_minutes).sum()
    }
}

/* ------------------------------- 常量 -------------------------------- */

/* 2026-10-17 字体栈改版：与 report.rs FONT 同步（去掉 -apple-system，补 Linux CJK 家族；
必须配合 push.rs svg_to_png 的 load_system_fonts()，resvg 0.45 默认空 fontdb）。 */
const FONT: &str = "PingFang SC, Hiragino Sans GB, Heiti SC, STHeiti, Microsoft YaHei, Noto Sans CJK SC, Noto Sans SC, Source Han Sans SC, WenQuanYi Zen Hei, sans-serif";
const COLOR_TEXT: &str = "#101828";
const COLOR_MUTED: &str = "#98A2B3";
/// 较昨日上涨：红（涨=好，运营口径与日报一致）
const COLOR_UP: &str = "#c0392b";
/// 较昨日下跌：绿
const COLOR_DOWN: &str = "#27ae60";
/// 师傅行浅黄底
const MASTER_BG: &str = "#fff8e1";
/// 未播行红底
const ABSENT_BG: &str = "#fdecea";
const WATERMARK: &str = "内部数据 · 请勿外传";

/* ------------------------------ 小 helper ----------------------------- */

/// SVG/XML 文本转义（& < > " ' 五个，& 在前避免双转义）。
/// 名册姓名、师傅名、水印 personal_mark 都来自用户可控字符串，
/// 不转义会提前闭合属性/标签，构成 SVG 注入面。
pub(crate) fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// 日期白名单：长度 10、逐字节数字或 '-'、且是合法日历日期。
/// （crate::bot::valid_date 模块私有不能引，这里保持同一口径：
/// chrono 对 "1-1-1" 这类短串照样返回 Ok，所以长度/字节检查必须在前。）
fn valid_date_str(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 {
        return false;
    }
    for (i, &c) in b.iter().enumerate() {
        let ok = match i {
            4 | 7 => c == b'-',
            _ => c.is_ascii_digit(),
        };
        if !ok {
            return false;
        }
    }
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

/// `2026-10-14` → `10月14日`（M/D 不补零）；解析失败原样返回。
fn cn_md(iso: &str) -> String {
    match chrono::NaiveDate::parse_from_str(iso, "%Y-%m-%d") {
        Ok(d) => d.format("%-m月%-d日").to_string(),
        Err(_) => iso.to_string(),
    }
}

/// `2026-10` → `2026年10月`
fn cn_month(ym: &str) -> String {
    match chrono::NaiveDate::parse_from_str(&format!("{ym}-01"), "%Y-%m-%d") {
        Ok(d) => d.format("%Y年%-m月").to_string(),
        Err(_) => ym.to_string(),
    }
}

/// `2026-11-01` + `2026-11-05` → `11月1日-5日`；跨月时保留两端月份。
fn cn_range(start: &str, end: &str) -> String {
    let Ok(s) = chrono::NaiveDate::parse_from_str(start, "%Y-%m-%d") else {
        return format!("{start}-{end}");
    };
    let Ok(e) = chrono::NaiveDate::parse_from_str(end, "%Y-%m-%d") else {
        return format!("{start}-{end}");
    };
    if s.year() == e.year() && s.month() == e.month() {
        format!("{}月{}日-{}日", s.format("%-m"), s.format("%-d"), e.format("%-d"))
    } else if s.year() == e.year() {
        format!(
            "{}月{}日-{}月{}日",
            s.format("%-m"),
            s.format("%-d"),
            e.format("%-m"),
            e.format("%-d")
        )
    } else {
        format!(
            "{}年{}月{}日-{}年{}月{}日",
            s.format("%Y"),
            s.format("%-m"),
            s.format("%-d"),
            e.format("%Y"),
            e.format("%-m"),
            e.format("%-d")
        )
    }
}

fn valid_month_str(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 7
        && b[4] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || c.is_ascii_digit())
        && chrono::NaiveDate::parse_from_str(&format!("{s}-01"), "%Y-%m-%d").is_ok()
}

/// person_id 列 BIGINT → u64（负值钳到 0）
fn id_of(r: &sqlx::postgres::PgRow, col: &str) -> u64 {
    r.try_get::<i64, _>(col)
        .map(|v| v.max(0) as u64)
        .unwrap_or(0)
}

/// SUM 结果 BIGINT → i64
fn signed(r: &sqlx::postgres::PgRow, col: &str) -> i64 {
    r.try_get::<i64, _>(col).unwrap_or(0)
}

/* ------------------------------- build ------------------------------- */

/// 读取师傅 + 符合日报口径的直系徒弟。
async fn load_master_and_roster(
    st: &Arc<AppState>,
    master_person_id: u64,
) -> anyhow::Result<(String, Vec<(u64, String)>)> {
    if master_person_id == 0 {
        anyhow::bail!("师傅不存在");
    }
    let master_sql =
        format!("SELECT id, name FROM person WHERE id = {master_person_id} AND deleted_at IS NULL");
    let master_rows = pgrs::query(&st.pools, "LOCAL", &master_sql).await?;
    let Some(mrow) = master_rows.first() else {
        anyhow::bail!("师傅不存在");
    };
    let master_name = mrow
        .try_get::<Option<String>, _>("name")
        .ok()
        .flatten()
        .unwrap_or_default();

    let roster_sql = format!(
        "SELECT id, name FROM person WHERE master_id = {master_person_id} \
         AND deleted_at IS NULL AND hide_in_daily_report = 0 AND status <> 'left'"
    );
    let roster = pgrs::query(&st.pools, "LOCAL", &roster_sql)
        .await?
        .into_iter()
        .map(|r| {
            let pid = id_of(&r, "id");
            let name = r
                .try_get::<Option<String>, _>("name")
                .ok()
                .flatten()
                .unwrap_or_default();
            (pid, name)
        })
        .collect();
    Ok((master_name, roster))
}

/// 本月指标：整月导入值优先，缺表/缺整月值回落日明细。
/// 返回值是 person_id → (音浪, 时长分钟)。
async fn load_month_metrics(
    st: &Arc<AppState>,
    month: &str,
) -> anyhow::Result<HashMap<u64, (i64, i64)>> {
    let select = "p.id AS person_id, \
         (SUM({wave}))::BIGINT AS wave, \
         (SUM({minutes}))::BIGINT AS minutes";
    let rows = crate::dialect::query_period(
        &st.pools,
        "LOCAL",
        select,
        &format!("{month}%"),
        "",
        "",
        "person_id",
    )
    .await?;
    let mut map = HashMap::new();
    for r in rows {
        map.insert(
            id_of(&r, "person_id"),
            (signed(&r, "wave"), signed(&r, "minutes")),
        );
    }
    Ok(map)
}

/// 拉取师门日报数据。date 过白名单，master_id 内联进 SQL（u64 无注入面）。
pub async fn build(
    st: &Arc<AppState>,
    date: &str,
    master_person_id: u64,
) -> anyhow::Result<LineageReport> {
    if !valid_date_str(date) {
        anyhow::bail!("日期不合法");
    }
    let day = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|_| anyhow::anyhow!("日期不合法"))?;
    let yesterday = day
        .pred_opt()
        .ok_or_else(|| anyhow::anyhow!("日期不合法"))?
        .format("%Y-%m-%d")
        .to_string();
    let month = date.get(..7).unwrap_or(date).to_string();
    let (master_name, roster) = load_master_and_roster(st, master_person_id).await?;

    // 今日 / 昨日音浪：主号 daily_metric + 副号 wave_snapshot，按人合并。
    let wave_day_sql = |d: &str| {
        format!(
            "SELECT u.pid AS person_id, (SUM(u.w))::BIGINT AS wave FROM ( \
               SELECT d.person_id AS pid, d.wave AS w FROM daily_metric d \
                WHERE d.wave_reliable = 1 AND TO_CHAR(d.biz_date, 'YYYY-MM-DD') = '{d}' \
               UNION ALL \
               SELECT a.person_id AS pid, ws.wave_value AS w \
                 FROM wave_snapshot ws JOIN account a ON a.anchor_id = ws.anchor_id AND a.is_primary = 0 \
                WHERE TO_CHAR(ws.biz_date, 'YYYY-MM-DD') = '{d}' \
             ) u GROUP BY u.pid"
        )
    };
    let mut today_map: HashMap<u64, i64> = HashMap::new();
    for r in pgrs::query(&st.pools, "LOCAL", &wave_day_sql(date)).await? {
        today_map.insert(id_of(&r, "person_id"), signed(&r, "wave"));
    }
    let mut yday_map: HashMap<u64, i64> = HashMap::new();
    for r in pgrs::query(&st.pools, "LOCAL", &wave_day_sql(&yesterday)).await? {
        yday_map.insert(id_of(&r, "person_id"), signed(&r, "wave"));
    }
    // 今日时长：主号 daily_metric + 副号 duration_snapshot，用于 4 小时达标筛选。
    let minute_day_sql = format!(
        "SELECT u.pid AS person_id, (SUM(u.m))::BIGINT AS minutes FROM ( \
           SELECT d.person_id AS pid, d.minutes AS m FROM daily_metric d \
            WHERE TO_CHAR(d.biz_date, 'YYYY-MM-DD') = '{date}' \
           UNION ALL \
           SELECT a.person_id AS pid, ds.minutes AS m \
             FROM duration_snapshot ds JOIN account a ON a.anchor_id = ds.anchor_id AND a.is_primary = 0 \
            WHERE TO_CHAR(ds.biz_date, 'YYYY-MM-DD') = '{date}' \
         ) u GROUP BY u.pid"
    );
    let mut today_minutes_map: HashMap<u64, i64> = HashMap::new();
    for r in pgrs::query(&st.pools, "LOCAL", &minute_day_sql).await? {
        today_minutes_map.insert(id_of(&r, "person_id"), signed(&r, "minutes"));
    }
    let month_metrics = load_month_metrics(st, &month).await?;

    let mut rows: Vec<LineageRow> = Vec::new();
    let master_today = today_map.get(&master_person_id).copied().unwrap_or(0);
    let (master_month_wave, master_month_minutes) = month_metrics
        .get(&master_person_id)
        .copied()
        .unwrap_or((0, 0));
    rows.push(LineageRow {
        name: master_name.clone(),
        today: master_today,
        today_minutes: today_minutes_map
            .get(&master_person_id)
            .copied()
            .unwrap_or(0),
        yesterday: yday_map.get(&master_person_id).copied(),
        month_total: master_month_wave,
        month_minutes: master_month_minutes,
        live: crate::dialect::is_live_wave(master_today),
        is_master: true,
    });

    let mut disciples: Vec<LineageRow> = Vec::new();
    for (pid, name) in roster {
        let today = today_map.get(&pid).copied().unwrap_or(0);
        let (month_wave, month_minutes) = month_metrics.get(&pid).copied().unwrap_or((0, 0));
        disciples.push(LineageRow {
            name,
            today,
            today_minutes: today_minutes_map.get(&pid).copied().unwrap_or(0),
            yesterday: yday_map.get(&pid).copied(),
            month_total: month_wave,
            month_minutes,
            live: crate::dialect::is_live_wave(today),
            is_master: false,
        });
    }
    disciples.sort_by(|a, b| b.today.cmp(&a.today).then(a.name.cmp(&b.name)));
    rows.extend(disciples);

    let total_today = rows.iter().map(|r| r.today).sum();
    let yvals: Vec<i64> = rows.iter().filter_map(|r| r.yesterday).collect();
    let total_yesterday = if yvals.is_empty() {
        None
    } else {
        Some(yvals.iter().sum())
    };
    let absent_names = rows
        .iter()
        .filter(|r| !r.is_master && !r.live)
        .map(|r| r.name.clone())
        .collect();

    Ok(LineageReport {
        master_name,
        date: date.to_string(),
        period: LineagePeriod::Daily,
        rows,
        total_today,
        total_yesterday,
        absent_names,
        daily_qualified_minutes: 0,
        daily_severe_minutes: 0,
        range_start: None,
        range_end: None,
        range_days: 0,
    })
}

/// 拉取整月师门报表：每行展示本月累计音浪与本月累计时长。
pub async fn build_month(
    st: &Arc<AppState>,
    month: &str,
    master_person_id: u64,
) -> anyhow::Result<LineageReport> {
    if !valid_month_str(month) {
        anyhow::bail!("月份不合法，应为 YYYY-MM");
    }
    let (master_name, roster) = load_master_and_roster(st, master_person_id).await?;
    let metrics = load_month_metrics(st, month).await?;

    let (master_wave, master_minutes) = metrics.get(&master_person_id).copied().unwrap_or((0, 0));
    let mut rows = vec![LineageRow {
        name: master_name.clone(),
        today: 0,
        today_minutes: 0,
        yesterday: None,
        month_total: master_wave,
        month_minutes: master_minutes,
        live: master_wave >= 2,
        is_master: true,
    }];
    let mut disciples: Vec<LineageRow> = roster
        .into_iter()
        .map(|(pid, name)| {
            let (wave, minutes) = metrics.get(&pid).copied().unwrap_or((0, 0));
            LineageRow {
                name,
                today: 0,
                today_minutes: 0,
                yesterday: None,
                month_total: wave,
                month_minutes: minutes,
                live: wave >= 2,
                is_master: false,
            }
        })
        .collect();
    disciples.sort_by(|a, b| {
        b.month_total
            .cmp(&a.month_total)
            .then(b.month_minutes.cmp(&a.month_minutes))
            .then(a.name.cmp(&b.name))
    });
    rows.extend(disciples);

    let absent_names = rows
        .iter()
        .filter(|r| !r.is_master && !r.live)
        .map(|r| r.name.clone())
        .collect();
    let total_today = rows.iter().map(|r| r.month_total).sum();
    Ok(LineageReport {
        master_name,
        date: month.to_string(),
        period: LineagePeriod::Monthly,
        rows,
        total_today,
        total_yesterday: None,
        absent_names,
        daily_qualified_minutes: 0,
        daily_severe_minutes: 0,
        range_start: None,
        range_end: None,
        range_days: 0,
    })
}

/// 拉取一段日期的师门总览：累计音浪/时长 + 按天数放大的达标线。
///
/// 例：11 月 1–5 日共 5 天，按每天 6 小时合格、每天 4 小时最低线，
/// 区间合格线 = 30 小时，严重不足线 = 20 小时。
pub async fn build_range(
    st: &Arc<AppState>,
    start: &str,
    end: &str,
    master_person_id: u64,
    per_day_qualified_minutes: i64,
    per_day_severe_minutes: i64,
) -> anyhow::Result<LineageReport> {
    if !valid_date_str(start) || !valid_date_str(end) {
        anyhow::bail!("日期不合法");
    }
    let start_day = chrono::NaiveDate::parse_from_str(start, "%Y-%m-%d")
        .map_err(|_| anyhow::anyhow!("日期不合法"))?;
    let end_day = chrono::NaiveDate::parse_from_str(end, "%Y-%m-%d")
        .map_err(|_| anyhow::anyhow!("日期不合法"))?;
    anyhow::ensure!(start_day <= end_day, "开始日期不能晚于结束日期");
    let range_days = (end_day - start_day).num_days() + 1;

    let (master_name, roster) = load_master_and_roster(st, master_person_id).await?;
    // 区间累计必须按日明细求和，不能用整月导入值，否则会把区间外数据也算进来。
    // 主号 daily_metric + 副号 wave_snapshot / duration_snapshot，按人合并。
    let range_sql = format!(
        "SELECT u.pid AS person_id, \
           (SUM(u.w))::BIGINT AS wave, (SUM(u.m))::BIGINT AS minutes \
         FROM ( \
           SELECT d.person_id AS pid, d.wave AS w, d.minutes AS m \
             FROM daily_metric d \
            WHERE d.wave_reliable = 1 \
              AND d.biz_date >= '{start}'::date AND d.biz_date <= '{end}'::date \
           UNION ALL \
           SELECT a.person_id AS pid, ws.wave_value AS w, 0 AS m \
             FROM wave_snapshot ws \
             JOIN account a ON a.anchor_id = ws.anchor_id AND a.is_primary = 0 \
            WHERE ws.biz_date >= '{start}'::date AND ws.biz_date <= '{end}'::date \
           UNION ALL \
           SELECT a.person_id AS pid, 0 AS w, ds.minutes AS m \
             FROM duration_snapshot ds \
             JOIN account a ON a.anchor_id = ds.anchor_id AND a.is_primary = 0 \
            WHERE ds.biz_date >= '{start}'::date AND ds.biz_date <= '{end}'::date \
         ) u GROUP BY u.pid"
    );
    let mut metrics: HashMap<u64, (i64, i64)> = HashMap::new();
    for r in pgrs::query(&st.pools, "LOCAL", &range_sql).await? {
        metrics.insert(id_of(&r, "person_id"), (signed(&r, "wave"), signed(&r, "minutes")));
    }

    let qualified_minutes = per_day_qualified_minutes
        .max(0)
        .saturating_mul(range_days);
    let severe_minutes = per_day_severe_minutes
        .max(0)
        .saturating_mul(range_days);

    let (master_wave, master_minutes) = metrics.get(&master_person_id).copied().unwrap_or((0, 0));
    let mut rows = vec![LineageRow {
        name: master_name.clone(),
        today: master_wave,
        today_minutes: master_minutes,
        yesterday: None,
        month_total: master_wave,
        month_minutes: master_minutes,
        live: master_wave >= 2,
        is_master: true,
    }];
    let mut disciples: Vec<LineageRow> = roster
        .into_iter()
        .map(|(pid, name)| {
            let (wave, minutes) = metrics.get(&pid).copied().unwrap_or((0, 0));
            LineageRow {
                name,
                today: wave,
                today_minutes: minutes,
                yesterday: None,
                month_total: wave,
                month_minutes: minutes,
                live: wave >= 2,
                is_master: false,
            }
        })
        .collect();
    disciples.sort_by(|a, b| {
        b.month_total
            .cmp(&a.month_total)
            .then(b.month_minutes.cmp(&a.month_minutes))
            .then(a.name.cmp(&b.name))
    });
    rows.extend(disciples);

    let absent_names = rows
        .iter()
        .filter(|r| !r.is_master && !r.live)
        .map(|r| r.name.clone())
        .collect();
    let total_today = rows.iter().map(|r| r.month_total).sum();
    Ok(LineageReport {
        master_name,
        date: format!("{start}~{end}"),
        period: LineagePeriod::Range,
        rows,
        total_today,
        total_yesterday: None,
        absent_names,
        daily_qualified_minutes: qualified_minutes,
        daily_severe_minutes: severe_minutes,
        range_start: Some(start.to_string()),
        range_end: Some(end.to_string()),
        range_days: range_days.max(0) as usize,
    })
}

/* --------------------------- SVG 分页渲染 ----------------------------- */

/// 分页渲染：per_page <= 0 或 >= 行数 → 单页；多页时每页独立 svg 文档，
/// 第 2 页起标题带「（第 n 页/共 m 页）」。
pub fn render_svg_pages(rep: &LineageReport, per_page: usize, personal_mark: &str) -> Vec<String> {
    render_svg_pages_with_fields(
        rep,
        per_page,
        personal_mark,
        &LineageReportFields::default(),
    )
}

pub fn render_svg_pages_with_fields(
    rep: &LineageReport,
    per_page: usize,
    personal_mark: &str,
    fields: &LineageReportFields,
) -> Vec<String> {
    if rep.rows.is_empty() {
        return Vec::new();
    }
    let page_chunks: Vec<&[LineageRow]> = if per_page == 0 || rep.rows.len() <= per_page {
        vec![rep.rows.as_slice()]
    } else {
        rep.rows.chunks(per_page).collect()
    };
    let page_count = page_chunks.len();
    page_chunks
        .into_iter()
        .enumerate()
        .map(|(pi, chunk)| {
            // 首页标题保持干净，第 2 页起才带页码
            let label = if page_count > 1 && pi >= 1 {
                format!("（第 {} 页/共 {} 页）", pi + 1, page_count)
            } else {
                String::new()
            };
            match rep.period {
                LineagePeriod::Daily => render_page(rep, chunk, &label, personal_mark, fields),
                LineagePeriod::Monthly => {
                    render_month_page(rep, chunk, &label, personal_mark, fields)
                }
                LineagePeriod::Range => {
                    render_range_page(rep, chunk, &label, personal_mark, fields)
                }
            }
        })
        .collect()
}

#[allow(clippy::too_many_lines)]
fn render_page(
    rep: &LineageReport,
    rows: &[LineageRow],
    page_label: &str,
    personal_mark: &str,
    fields: &LineageReportFields,
) -> String {
    let has_duration_standard = rep.daily_qualified_minutes > 0 || rep.daily_severe_minutes > 0;
    let header_h = if has_duration_standard { 132.0 } else { 112.0 };
    let list_h = rows.len() as f64 * 48.0;
    let footer_y = header_h + list_h + 26.0;
    let total_h = footer_y + 22.0;

    let mut s = String::with_capacity(32 * 1024);
    s.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1440\" height=\"{}\" viewBox=\"0 0 720 {total_h:.0}\" role=\"img\">",
        total_h as i64 * 2
    ));
    s.push_str(&format!(
        "<rect width=\"720\" height=\"{total_h:.0}\" fill=\"#FFFFFF\"/>"
    ));

    // 标题：`{master_name} 师门 · {M月D日}音浪`
    let date_cn = cn_md(&rep.date);
    s.push_str(&format!(
        "<text x=\"360\" y=\"32\" font-size=\"25\" fill=\"{COLOR_TEXT}\" font-weight=\"700\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">{} 师门 · {date_cn}音浪{}</text>",
        esc(&rep.master_name),
        esc(page_label),
    ));

    // 副标题：门下 N 人 · 今日合计 X [· 较昨日 ▲x/▼x/—]
    let mut sub = format!("门下 {} 人", rep.disciple_count());
    if fields.today_wave {
        sub.push_str(&format!(" · 今日合计 {}", fmt_wan(rep.total_today)));
    }
    if fields.month_duration {
        sub.push_str(&format!(
            " · 本月时长 {}",
            crate::push::fmt_minutes(rep.month_minutes_total())
        ));
    }
    if rep.daily_qualified_minutes > 0 || rep.daily_severe_minutes > 0 {
        let qualified = rep
            .rows
            .iter()
            .filter(|r| !r.is_master && r.today_minutes >= rep.daily_qualified_minutes)
            .count();
        let severe = rep
            .rows
            .iter()
            .filter(|r| !r.is_master && r.today_minutes < rep.daily_severe_minutes)
            .count();
        sub.push_str(&format!(" · 合格 {qualified} 人 · 严重不足 {severe} 人"));
    }
    if fields.wave_diff {
        match rep.total_diff() {
            Some(d) if d > 0 => sub.push_str(&format!(
                " · 较昨日 <tspan fill=\"{COLOR_UP}\" font-weight=\"700\">▲{}</tspan>",
                fmt_wan(d)
            )),
            Some(d) if d < 0 => sub.push_str(&format!(
                " · 较昨日 <tspan fill=\"{COLOR_DOWN}\" font-weight=\"700\">▼{}</tspan>",
                fmt_wan(-d)
            )),
            Some(_) => sub.push_str(&format!(
                " · 较昨日 <tspan fill=\"{COLOR_MUTED}\" font-weight=\"700\">—</tspan>"
            )),
            None => {}
        }
    }
    s.push_str(&format!(
        "<text x=\"360\" y=\"66\" font-size=\"14\" fill=\"#475467\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">{sub}</text>"
    ));

    if has_duration_standard {
        let note = format!(
            "时长标准（按天）：{}小时合格 · {}小时以下严重不足",
            rep.daily_qualified_minutes as f64 / 60.0,
            rep.daily_severe_minutes as f64 / 60.0
        );
        s.push_str(&format!(
            "<text x=\"360\" y=\"88\" font-size=\"10.5\" fill=\"#667085\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">{}</text>",
            esc(&note)
        ));
    }

    // 表头（x/锚点与下方数据行同列一致）
    let th_y = header_h - 30.0; // 表头背景 y
    let ty_y = header_h - 14.0; // 表头文字 y
    s.push_str(&format!(
        "<rect y=\"{th_y:.0}\" width=\"720\" height=\"32\" fill=\"#F2F4F7\"/>"
    ));
    let columns: Vec<(f64, &str, &str)> = [
        (28.0f64, "主播", "start"),
        (145.0, "今日音浪", "middle"),
        (235.0, "今日时长", "middle"),
        (325.0, "昨日音浪", "middle"),
        (415.0, "较昨日", "middle"),
        (505.0, "本月累计", "middle"),
        (600.0, "本月时长", "middle"),
        (696.0, "状态", "end"),
    ]
    .into_iter()
    .filter(|&(_, label, _)| {
        (label == "主播" && fields.anchor)
            || (label == "今日音浪" && fields.today_wave)
            || (label == "今日时长" && fields.today_duration)
            || (label == "昨日音浪" && fields.yesterday_wave)
            || (label == "较昨日" && fields.wave_diff)
            || (label == "本月累计" && fields.month_total)
            || (label == "本月时长" && fields.month_duration)
            || (label == "状态" && fields.status)
    })
    .collect();
    for (x, label, anchor) in columns {
        s.push_str(&format!(
            "<text x=\"{x}\" y=\"{ty_y:.0}\" font-size=\"12\" fill=\"#667085\" font-weight=\"700\" text-anchor=\"{anchor}\" dominant-baseline=\"central\" font-family=\"{FONT}\">{label}</text>"
        ));
    }

    // 数据行
    for (i, r) in rows.iter().enumerate() {
        let y = header_h + i as f64 * 48.0;
        let text_y = y + 24.0;
        let severe = has_duration_standard && r.today_minutes < rep.daily_severe_minutes;
        let unmet =
            has_duration_standard && !severe && r.today_minutes < rep.daily_qualified_minutes;
        let row_bg = if r.is_master {
            MASTER_BG
        } else if severe || (!has_duration_standard && !r.live) {
            ABSENT_BG
        } else if unmet {
            "#FFF7ED"
        } else {
            "#FFFFFF"
        };
        s.push_str(&format!(
            "<rect y=\"{y:.0}\" width=\"720\" height=\"48\" fill=\"{row_bg}\" stroke=\"#EAECF0\" stroke-width=\"1\"/>"
        ));
        // 主播名（师傅行名字后缀「（师）」）
        if fields.anchor {
            s.push_str(&format!(
                "<text x=\"28\" y=\"{text_y:.0}\" font-size=\"14\" fill=\"{COLOR_TEXT}\" font-weight=\"700\" text-anchor=\"start\" dominant-baseline=\"central\" font-family=\"{FONT}\">{}{}</text>",
                esc(&r.name),
                if r.is_master { "（师）" } else { "" },
            ));
        }
        if fields.today_wave {
            s.push_str(&text_cell(
                145.0,
                text_y,
                "middle",
                &fmt_wan(r.today),
                COLOR_TEXT,
            ));
        }
        if fields.today_duration {
            s.push_str(&text_cell(
                235.0,
                text_y,
                "middle",
                &crate::push::fmt_minutes(r.today_minutes),
                if severe {
                    COLOR_UP
                } else if unmet {
                    "#C2410C"
                } else {
                    "#168482"
                },
            ));
        }
        if fields.yesterday_wave {
            match r.yesterday {
                Some(yv) => s.push_str(&text_cell(
                    325.0,
                    text_y,
                    "middle",
                    &fmt_wan(yv),
                    COLOR_TEXT,
                )),
                None => s.push_str(&text_cell(325.0, text_y, "middle", "—", COLOR_MUTED)),
            }
        }
        if fields.wave_diff {
            match r.yesterday {
                None => s.push_str(&text_cell(415.0, text_y, "middle", "—", COLOR_MUTED)),
                Some(yv) => {
                    let d = r.today - yv;
                    if d > 0 {
                        s.push_str(&text_cell(
                            415.0,
                            text_y,
                            "middle",
                            &format!("▲{}", fmt_wan(d)),
                            COLOR_UP,
                        ));
                    } else if d < 0 {
                        s.push_str(&text_cell(
                            415.0,
                            text_y,
                            "middle",
                            &format!("▼{}", fmt_wan(-d)),
                            COLOR_DOWN,
                        ));
                    } else {
                        s.push_str(&text_cell(415.0, text_y, "middle", "—", COLOR_MUTED));
                    }
                }
            }
        }
        if fields.month_total {
            s.push_str(&text_cell(
                505.0,
                text_y,
                "middle",
                &fmt_wan(r.month_total),
                "#475467",
            ));
        }
        if fields.month_duration {
            s.push_str(&text_cell(
                600.0,
                text_y,
                "middle",
                &crate::push::fmt_minutes(r.month_minutes),
                "#168482",
            ));
        }
        if fields.status {
            if has_duration_standard && !r.is_master {
                if severe {
                    s.push_str(&text_cell(696.0, text_y, "end", "严重不足", COLOR_UP));
                } else if unmet {
                    s.push_str(&text_cell(696.0, text_y, "end", "未达标", "#C2410C"));
                } else {
                    s.push_str(&text_cell(696.0, text_y, "end", "合格", "#168482"));
                }
            } else if r.live {
                s.push_str(&text_cell(696.0, text_y, "end", "开播", COLOR_TEXT));
            } else {
                s.push_str(&text_cell(696.0, text_y, "end", "未播", COLOR_UP));
            }
        }
    }

    // 水印压在最顶层
    s.push_str(&watermark_layer(total_h, personal_mark));
    s.push_str("</svg>");
    s
}

/// 月报：与日报同一视觉体系，但只展示本月累计音浪、本月累计时长与状态。
#[allow(clippy::too_many_lines)]
fn render_month_page(
    rep: &LineageReport,
    rows: &[LineageRow],
    page_label: &str,
    personal_mark: &str,
    fields: &LineageReportFields,
) -> String {
    let header_h = 112.0f64;
    let list_h = rows.len() as f64 * 48.0;
    let footer_y = header_h + list_h + 26.0;
    let total_h = footer_y + 22.0;

    let mut s = String::with_capacity(32 * 1024);
    s.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1440\" height=\"{}\" viewBox=\"0 0 720 {total_h:.0}\" role=\"img\">",
        total_h as i64 * 2
    ));
    s.push_str(&format!(
        "<rect width=\"720\" height=\"{total_h:.0}\" fill=\"#FFFFFF\"/>"
    ));

    s.push_str(&format!(
        "<text x=\"360\" y=\"32\" font-size=\"25\" fill=\"{COLOR_TEXT}\" font-weight=\"700\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">{} 师门 · {}月度战绩{}</text>",
        esc(&rep.master_name),
        esc(&cn_month(&rep.date)),
        esc(page_label),
    ));

    let mut sub = format!("门下 {} 人", rep.disciple_count());
    if fields.month_total {
        sub.push_str(&format!(" · 本月音浪 {}", fmt_wan(rep.month_wave_total())));
    }
    if fields.month_duration {
        sub.push_str(&format!(
            " · 本月时长 {}",
            crate::push::fmt_minutes(rep.month_minutes_total())
        ));
    }
    s.push_str(&format!(
        "<text x=\"360\" y=\"66\" font-size=\"14\" fill=\"#475467\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">{sub}</text>"
    ));

    let th_y = header_h - 30.0;
    let ty_y = header_h - 14.0;
    s.push_str(&format!(
        "<rect y=\"{th_y:.0}\" width=\"720\" height=\"32\" fill=\"#F2F4F7\"/>"
    ));
    let columns: Vec<(f64, &str, &str)> = [
        (28.0f64, "主播", "start"),
        (350.0, "本月音浪", "middle"),
        (530.0, "本月时长", "middle"),
        (696.0, "状态", "end"),
    ]
    .into_iter()
    .filter(|&(_, label, _)| {
        (label == "主播" && fields.anchor)
            || (label == "本月音浪" && fields.month_total)
            || (label == "本月时长" && fields.month_duration)
            || (label == "状态" && fields.status)
    })
    .collect();
    for (x, label, anchor) in columns {
        s.push_str(&format!(
            "<text x=\"{x}\" y=\"{ty_y:.0}\" font-size=\"12\" fill=\"#667085\" font-weight=\"700\" text-anchor=\"{anchor}\" dominant-baseline=\"central\" font-family=\"{FONT}\">{label}</text>"
        ));
    }

    for (i, r) in rows.iter().enumerate() {
        let y = header_h + i as f64 * 48.0;
        let text_y = y + 24.0;
        let row_bg = if r.is_master {
            MASTER_BG
        } else if !r.live {
            ABSENT_BG
        } else {
            "#FFFFFF"
        };
        s.push_str(&format!(
            "<rect y=\"{y:.0}\" width=\"720\" height=\"48\" fill=\"{row_bg}\" stroke=\"#EAECF0\" stroke-width=\"1\"/>"
        ));
        if fields.anchor {
            s.push_str(&format!(
                "<text x=\"28\" y=\"{text_y:.0}\" font-size=\"14\" fill=\"{COLOR_TEXT}\" font-weight=\"700\" text-anchor=\"start\" dominant-baseline=\"central\" font-family=\"{FONT}\">{}{}</text>",
                esc(&r.name),
                if r.is_master { "（师）" } else { "" },
            ));
        }
        if fields.month_total {
            s.push_str(&text_cell(
                350.0,
                text_y,
                "middle",
                &fmt_wan(r.month_total),
                COLOR_TEXT,
            ));
        }
        if fields.month_duration {
            s.push_str(&text_cell(
                530.0,
                text_y,
                "middle",
                &crate::push::fmt_minutes(r.month_minutes),
                "#168482",
            ));
        }
        if fields.status {
            if r.live {
                s.push_str(&text_cell(696.0, text_y, "end", "开播", COLOR_TEXT));
            } else {
                s.push_str(&text_cell(696.0, text_y, "end", "未播", COLOR_UP));
            }
        }
    }

    s.push_str(&watermark_layer(total_h, personal_mark));
    s.push_str("</svg>");
    s
}

/// 区间总览：一位师傅一张图，累计音浪/时长 + 按天数放大的 6h / 4h 标准。
#[allow(clippy::too_many_lines)]
fn render_range_page(
    rep: &LineageReport,
    rows: &[LineageRow],
    page_label: &str,
    personal_mark: &str,
    fields: &LineageReportFields,
) -> String {
    let has_standard = rep.daily_qualified_minutes > 0 || rep.daily_severe_minutes > 0;
    let header_h = if has_standard { 132.0f64 } else { 112.0f64 };
    let list_h = rows.len() as f64 * 48.0;
    let footer_y = header_h + list_h + 26.0;
    let total_h = footer_y + 22.0;
    let range_label = match (&rep.range_start, &rep.range_end) {
        (Some(start), Some(end)) => cn_range(start, end),
        _ => rep.date.clone(),
    };

    let mut s = String::with_capacity(32 * 1024);
    s.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1440\" height=\"{}\" viewBox=\"0 0 720 {total_h:.0}\" role=\"img\">",
        total_h as i64 * 2
    ));
    s.push_str(&format!(
        "<rect width=\"720\" height=\"{total_h:.0}\" fill=\"#FFFFFF\"/>"
    ));
    s.push_str(&format!(
        "<text x=\"360\" y=\"32\" font-size=\"25\" fill=\"{COLOR_TEXT}\" font-weight=\"700\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">{} 师门 · {}总览{}</text>",
        esc(&rep.master_name),
        esc(&range_label),
        esc(page_label),
    ));

    let qualified = rep
        .rows
        .iter()
        .filter(|r| {
            !r.is_master
                && has_standard
                && r.month_minutes >= rep.daily_qualified_minutes
        })
        .count();
    let severe = rep
        .rows
        .iter()
        .filter(|r| {
            !r.is_master && has_standard && r.month_minutes < rep.daily_severe_minutes
        })
        .count();
    let mut sub = format!("门下 {} 人", rep.disciple_count());
    if fields.month_total {
        sub.push_str(&format!(" · 累计音浪 {}", fmt_wan(rep.month_wave_total())));
    }
    if fields.month_duration {
        sub.push_str(&format!(
            " · 累计时长 {}",
            crate::push::fmt_minutes(rep.month_minutes_total())
        ));
    }
    if fields.status && has_standard {
        sub.push_str(&format!(" · 合格 {qualified} 人 · 严重不足 {severe} 人"));
    }
    s.push_str(&format!(
        "<text x=\"360\" y=\"66\" font-size=\"14\" fill=\"#475467\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">{sub}</text>"
    ));

    if has_standard {
        // 每日标准乘以自然日数，得到整段日期的累计合格线与严重不足线。
        let days = rep.range_days.max(1);
        let qualified_hours = rep.daily_qualified_minutes as f64 / 60.0;
        let severe_hours = rep.daily_severe_minutes as f64 / 60.0;
        let note = format!(
            "时长标准（每天）：6小时合格 · 4小时以下严重不足；本区间 {days} 天：{qualified_hours:.0}小时合格 · {severe_hours:.0}小时以下严重不足"
        );
        s.push_str(&format!(
            "<text x=\"360\" y=\"96\" font-size=\"12\" fill=\"#667085\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">{note}</text>"
        ));
    }

    let th_y = header_h - 30.0;
    let ty_y = header_h - 14.0;
    s.push_str(&format!(
        "<rect y=\"{th_y:.0}\" width=\"720\" height=\"32\" fill=\"#F2F4F7\"/>"
    ));
    let columns: Vec<(f64, &str, &str)> = [
        (28.0f64, "主播", "start"),
        (350.0, "累计音浪", "middle"),
        (530.0, "累计时长", "middle"),
        (696.0, "状态", "end"),
    ]
    .into_iter()
    .filter(|&(_, label, _)| {
        (label == "主播" && fields.anchor)
            || (label == "累计音浪" && fields.month_total)
            || (label == "累计时长" && fields.month_duration)
            || (label == "状态" && fields.status)
    })
    .collect();
    for (x, label, anchor) in columns {
        s.push_str(&format!(
            "<text x=\"{x}\" y=\"{ty_y:.0}\" font-size=\"12\" fill=\"#667085\" font-weight=\"700\" text-anchor=\"{anchor}\" dominant-baseline=\"central\" font-family=\"{FONT}\">{label}</text>"
        ));
    }

    for (i, r) in rows.iter().enumerate() {
        let y = header_h + i as f64 * 48.0;
        let text_y = y + 24.0;
        let severe = !r.is_master && has_standard && r.month_minutes < rep.daily_severe_minutes;
        let unmet = !r.is_master
            && has_standard
            && !severe
            && r.month_minutes < rep.daily_qualified_minutes;
        let row_bg = if r.is_master {
            MASTER_BG
        } else if severe || (!has_standard && !r.live) {
            ABSENT_BG
        } else if unmet {
            "#FFF7ED"
        } else {
            "#FFFFFF"
        };
        s.push_str(&format!(
            "<rect y=\"{y:.0}\" width=\"720\" height=\"48\" fill=\"{row_bg}\" stroke=\"#EAECF0\" stroke-width=\"1\"/>"
        ));
        if fields.anchor {
            s.push_str(&format!(
                "<text x=\"28\" y=\"{text_y:.0}\" font-size=\"14\" fill=\"{COLOR_TEXT}\" font-weight=\"700\" text-anchor=\"start\" dominant-baseline=\"central\" font-family=\"{FONT}\">{}{}</text>",
                esc(&r.name),
                if r.is_master { "（师）" } else { "" },
            ));
        }
        if fields.month_total {
            s.push_str(&text_cell(
                350.0,
                text_y,
                "middle",
                &fmt_wan(r.month_total),
                COLOR_TEXT,
            ));
        }
        if fields.month_duration {
            s.push_str(&text_cell(
                530.0,
                text_y,
                "middle",
                &crate::push::fmt_minutes(r.month_minutes),
                if severe {
                    COLOR_UP
                } else if unmet {
                    "#C2410C"
                } else {
                    "#168482"
                },
            ));
        }
        if fields.status {
            if r.is_master {
                if r.live {
                    s.push_str(&text_cell(696.0, text_y, "end", "开播", COLOR_TEXT));
                } else {
                    s.push_str(&text_cell(696.0, text_y, "end", "未播", COLOR_UP));
                }
            } else if has_standard {
                if severe {
                    s.push_str(&text_cell(696.0, text_y, "end", "严重不足", COLOR_UP));
                } else if unmet {
                    s.push_str(&text_cell(696.0, text_y, "end", "未达标", "#C2410C"));
                } else {
                    s.push_str(&text_cell(696.0, text_y, "end", "合格", "#168482"));
                }
            } else if r.live {
                s.push_str(&text_cell(696.0, text_y, "end", "开播", COLOR_TEXT));
            } else {
                s.push_str(&text_cell(696.0, text_y, "end", "未播", COLOR_UP));
            }
        }
    }

    s.push_str(&watermark_layer(total_h, personal_mark));
    s.push_str("</svg>");
    s
}

/// 可见斜排 + 双层隐形水印。
fn watermark_layer(total_h: f64, personal_mark: &str) -> String {
    let mark_src = if personal_mark.trim().is_empty() {
        WATERMARK.to_string()
    } else {
        format!("{} {}", WATERMARK, personal_mark.trim())
    };
    let mark = esc(&mark_src);
    let mut watermark = String::new();
    let step_x = 140.0f64;
    let step_y = 76.0f64;
    let wm_rows = (total_h / step_y).ceil() as i64 + 1;
    for yi in 0..wm_rows {
        for xi in 0..6u32 {
            let wx = xi as f64 * step_x - 30.0 + if yi % 2 == 1 { step_x / 2.0 } else { 0.0 };
            let wy = yi as f64 * step_y + 24.0;
            watermark.push_str(&format!(
                "<text x=\"{wx:.0}\" y=\"{wy:.0}\" font-size=\"16\" font-weight=\"700\" fill=\"#101828\" fill-opacity=\"0.07\" text-anchor=\"middle\" font-family=\"{FONT}\" transform=\"rotate(-24 {wx:.0} {wy:.0})\">{mark}</text>"
            ));
            watermark.push_str(&format!(
                "<text x=\"{wx:.0}\" y=\"{wy:.0}\" font-size=\"22\" font-weight=\"900\" fill=\"#FFFFFF\" fill-opacity=\"0.20\" text-anchor=\"middle\" font-family=\"{FONT}\" transform=\"rotate(-24 {wx:.0} {wy:.0})\">{mark}</text>"
            ));
            watermark.push_str(&format!(
                "<text x=\"{wx:.0}\" y=\"{wy:.0}\" font-size=\"22\" font-weight=\"900\" fill=\"#000000\" fill-opacity=\"0.012\" text-anchor=\"middle\" font-family=\"{FONT}\" transform=\"rotate(-24 {wx:.0} {wy:.0})\">{mark}</text>"
            ));
        }
    }
    watermark
}

/// 表格单元格 text 元素（font-size 13）
fn text_cell(x: f64, y: f64, anchor: &str, text: &str, fill: &str) -> String {
    format!(
        "<text x=\"{x:.0}\" y=\"{y:.0}\" font-size=\"13\" fill=\"{fill}\" font-weight=\"700\" text-anchor=\"{anchor}\" dominant-baseline=\"central\" font-family=\"{FONT}\">{text}</text>"
    )
}

/* ------------------------------ 文本摘要 ------------------------------ */

/// 机器人文本版师门日报（多行逐徒弟明细；2026-10-17 起口径「有文字就不发图片」，
/// 师门日报只发这段文字、不再发图）。
pub fn render_text(rep: &LineageReport) -> String {
    let mut out = format!(
        "👨‍🏫 {} 师门 · {}：门下 {} 人，今日合计 {}",
        rep.master_name,
        cn_md(&rep.date),
        rep.disciple_count(),
        fmt_wan(rep.total_today)
    );
    if let Some(d) = rep.total_diff() {
        let sym = if d > 0 {
            "▲"
        } else if d < 0 {
            "▼"
        } else {
            "—"
        };
        let body = if d == 0 {
            String::new()
        } else {
            fmt_wan(d.abs())
        };
        out.push_str(&format!("（{sym}{body}）"));
    }
    // 逐徒弟明细：师傅行固定第一（后缀「（师）」），徒弟按今日音浪降序；
    // 未播的今日显示「未播」，有昨日数据的附「较昨日▲/▼差」，持平不显示。
    for r in &rep.rows {
        let tag = if r.is_master { "（师）" } else { "" };
        let today = if r.live {
            fmt_wan(r.today)
        } else {
            "未播".to_string()
        };
        out.push_str(&format!("\n{}{} {}", r.name, tag, today));
        if let Some(yv) = r.yesterday {
            let d = r.today - yv;
            if d > 0 {
                out.push_str(&format!(" 较昨日▲{}", fmt_wan(d)));
            } else if d < 0 {
                out.push_str(&format!(" 较昨日▼{}", fmt_wan(-d)));
            }
            // d == 0（持平）不显示较昨日
        }
    }
    if !rep.absent_names.is_empty() {
        out.push_str(&format!(
            "\n未播 {} 人：{}",
            rep.absent_names.len(),
            rep.absent_names.join("、")
        ));
    }
    out
}

/* -------------------------------- 单测 -------------------------------- */

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        name: &str,
        today: i64,
        yesterday: Option<i64>,
        month_total: i64,
        live: bool,
        is_master: bool,
    ) -> LineageRow {
        LineageRow {
            name: name.to_string(),
            today,
            today_minutes: 0,
            yesterday,
            month_total,
            month_minutes: month_total / 4,
            live,
            is_master,
        }
    }

    /// 1 师傅 + 3 徒弟：覆盖 yesterday None、▲、▼、未播、名字含 < &
    fn fixture() -> LineageReport {
        LineageReport {
            master_name: "冷师傅".to_string(),
            date: "2026-10-14".to_string(),
            period: LineagePeriod::Daily,
            rows: vec![
                row("冷师傅", 50_000, Some(45_000), 900_000, true, true), // ▲
                row("阿三", 120_000, Some(100_000), 1_200_000, true, false), // ▲
                row("小<李&仔", 8_000, Some(9_500), 300_000, true, false), // ▼
                row("小王", 0, None, 50_000, false, false),               // yesterday None + 未播
            ],
            total_today: 178_000,
            total_yesterday: Some(154_500),
            absent_names: vec!["小王".to_string()],
            daily_qualified_minutes: 0,
            daily_severe_minutes: 0,
            range_start: None,
            range_end: None,
            range_days: 0,
        }
    }

    #[test]
    fn daily_minutes_standard_keeps_all_disciples_for_rendering() {
        let mut rep = fixture();
        rep.rows[0].today_minutes = 300;
        rep.rows[1].today_minutes = 240;
        rep.rows[2].today_minutes = 239;
        rep.rows[3].today_minutes = 0;
        rep.set_daily_minutes_standard(360, 240);
        assert_eq!(rep.rows.len(), 4);
        let names: Vec<&str> = rep.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["冷师傅", "阿三", "小<李&仔", "小王"]);
        assert_eq!(rep.disciple_count(), 3);
        assert_eq!(rep.daily_qualified_minutes, 360);
        assert_eq!(rep.daily_severe_minutes, 240);
    }

    #[test]
    fn total_diff_cases() {
        let mk = |today: i64, yday: Option<i64>| LineageReport {
            master_name: "m".to_string(),
            date: "2026-10-14".to_string(),
            period: LineagePeriod::Daily,
            rows: Vec::new(),
            total_today: today,
            total_yesterday: yday,
            absent_names: Vec::new(),
            daily_qualified_minutes: 0,
            daily_severe_minutes: 0,
            range_start: None,
            range_end: None,
            range_days: 0,
        };
        assert_eq!(mk(100, None).total_diff(), None);
        assert_eq!(mk(150, Some(100)).total_diff(), Some(50));
        assert_eq!(mk(80, Some(120)).total_diff(), Some(-40));
        assert_eq!(mk(100, Some(100)).total_diff(), Some(0));
    }

    #[test]
    fn svg_single_page_counts() {
        let rep = fixture();
        let pages = render_svg_pages(&rep, 0, "浩月");
        assert_eq!(pages.len(), 1);
        let svg = &pages[0];
        // 独立 svg 文档
        assert!(svg.starts_with("<svg"));
        assert!(svg.ends_with("</svg>"));
        // 师傅行后缀
        assert!(svg.contains("（师）"));
        // ▲：师傅行 + 阿三行 + 副标题合计 = 3；▼：小李行 = 1；—：小王昨日/较昨日 = 2
        assert_eq!(svg.matches('▲').count(), 3);
        assert_eq!(svg.matches('▼').count(), 1);
        assert_eq!(svg.matches('—').count(), 2);
        // 未播行红底
        assert!(svg.contains("#fdecea"));
        // 师傅行浅黄底
        assert!(svg.contains("#fff8e1"));
        // 水印
        assert!(svg.contains("内部数据"));
        // 副标题带合计与较昨日
        assert!(svg.contains("今日合计"));
        assert!(svg.contains("较昨日"));
        // 未播状态列
        assert!(svg.contains("未播"));
    }

    /// 2026-10-17 用户口径「字段后端支持显示和不显示」：师门日报图的列显隐来自
    /// config `push_report_fields.lineage`，关掉的列必须连同表头一起消失。
    #[test]
    fn lineage_report_fields_hide_columns() {
        let rep = fixture();
        let all = render_svg_pages_with_fields(&rep, 0, "浩月", &LineageReportFields::default());
        assert!(all[0].contains("今日音浪"));
        assert!(all[0].contains("本月时长"));
        let hidden = LineageReportFields {
            anchor: false,
            today_wave: false,
            today_duration: false,
            yesterday_wave: false,
            wave_diff: false,
            month_total: false,
            month_duration: false,
            status: false,
        };
        let none = render_svg_pages_with_fields(&rep, 0, "浩月", &hidden);
        for label in [
            "主播",
            "今日音浪",
            "今日时长",
            "昨日音浪",
            "较昨日",
            "本月累计",
            "本月时长",
            "状态",
        ] {
            assert!(!none[0].contains(label), "关掉的列不该渲染表头：{label}");
        }
        assert!(
            !none[0].contains(">阿三</text>"),
            "主播列关掉后不应再出现姓名"
        );
    }

    #[test]
    fn month_report_renders_wave_and_duration() {
        let mut rows = vec![
            row("浩龙", 0, None, 1_710_000, true, true),
            row("玖甜", 0, None, 589, true, false),
            row("玖瑶", 0, None, 0, false, false),
        ];
        rows[0].month_minutes = 2_250;
        rows[1].month_minutes = 9;
        let rep = LineageReport {
            master_name: "浩龙".to_string(),
            date: "2026-10".to_string(),
            period: LineagePeriod::Monthly,
            rows,
            total_today: 1_710_589,
            total_yesterday: None,
            absent_names: vec!["玖瑶".to_string()],
            daily_qualified_minutes: 0,
            daily_severe_minutes: 0,
            range_start: None,
            range_end: None,
            range_days: 0,
        };
        let svg =
            &render_svg_pages_with_fields(&rep, 0, "浩龙", &LineageReportFields::default())[0];
        assert!(svg.contains("2026年10月月度战绩"));
        assert!(svg.contains("本月音浪"));
        assert!(svg.contains("本月时长"));
        assert!(svg.contains("171 万"));
        assert!(svg.contains("9分"));
        assert!(svg.contains("玖瑶"));
        assert!(svg.contains("未播"));
    }

    #[test]
    fn range_report_scales_daily_standard_by_days() {
        let mut rows = vec![
            row("浩龙", 0, None, 1_710_000, true, true),
            row("合格徒弟", 0, None, 600_000, true, false),
            row("未达标徒弟", 0, None, 500_000, true, false),
            row("严重不足徒弟", 0, None, 400_000, true, false),
        ];
        rows[0].month_minutes = 3_000;
        rows[1].month_minutes = 1_800; // 30h
        rows[2].month_minutes = 1_400; // 23h20m
        rows[3].month_minutes = 1_000; // 16h40m
        let rep = LineageReport {
            master_name: "浩龙".to_string(),
            date: "2026-11-01~2026-11-05".to_string(),
            period: LineagePeriod::Range,
            rows,
            total_today: 3_210_000,
            total_yesterday: None,
            absent_names: Vec::new(),
            // 每天 6h/4h × 5 天 = 30h/20h。
            daily_qualified_minutes: 1_800,
            daily_severe_minutes: 1_200,
            range_start: Some("2026-11-01".to_string()),
            range_end: Some("2026-11-05".to_string()),
            range_days: 5,
        };
        let svg =
            &render_svg_pages_with_fields(&rep, 0, "浩龙", &LineageReportFields::default())[0];
        assert!(svg.contains("11月1日-5日总览"));
        assert!(svg.contains("本区间 5 天：30小时合格 · 20小时以下严重不足"));
        assert!(svg.contains("合格 1 人"));
        assert!(svg.contains("严重不足 1 人"));
        assert!(svg.contains(">合格</text>"));
        assert!(svg.contains(">未达标</text>"));
        assert!(svg.contains(">严重不足</text>"));
    }

    #[test]
    fn svg_escapes_name() {
        let rep = fixture();
        let svg = &render_svg_pages(&rep, 0, "")[0];
        // < 与 & 都被转义
        assert!(svg.contains("小&lt;李&amp;仔"));
        // 不存在裸 `<` 直接跟原名
        assert!(!svg.contains("<小<李&仔"));
    }

    #[test]
    fn svg_pagination() {
        let rep = fixture();
        // ceil(4/2) = 2 页
        let pages = render_svg_pages(&rep, 2, "浩月");
        assert_eq!(pages.len(), 2);
        // 首页标题不带页码
        assert!(!pages[0].contains("第 1 页"));
        // 第 2 页标题带页码
        assert!(pages[1].contains("第 2 页"));
        assert!(pages[1].contains("共 2 页"));
        // 每页都是独立 svg 文档，且行被拆开（第 2 页只有 2 行 = 2 个行背景之外再无（师））
        assert!(pages[1].starts_with("<svg"));
        assert!(!pages[1].contains("（师）"));
        // per_page >= 行数 → 单页
        assert_eq!(render_svg_pages(&rep, 4, "").len(), 1);
        assert_eq!(render_svg_pages(&rep, 100, "").len(), 1);
    }

    #[test]
    fn text_summary() {
        let rep = fixture();
        let t = render_text(&rep);
        assert!(t.contains("冷师傅"));
        assert!(t.contains("今日合计"));
        assert!(t.contains("万"));
        // 合计较昨日 ▲
        assert!(t.contains('▲'));
        // 师傅行后缀「（师）」
        assert!(t.contains("（师）"));
        // 逐徒弟明细里有 ▼（小李）与「未播」（小王）
        assert!(t.contains('▼'));
        assert!(t.contains("未播"));
        // 底部未播名单（冒号分隔）
        assert!(t.contains("未播 1 人：小王"));
    }

    #[test]
    fn text_rows_list_every_disciple() {
        let rep = fixture();
        let t = render_text(&rep);
        // 师傅 + 三个徒弟都在明细里
        assert!(t.contains("阿三"));
        assert!(t.contains("小<李&仔"));
        assert!(t.contains("小王"));
        // 逐徒弟较昨日：阿三 ▲、小李 ▼（名字 + 今日 + 较昨日）
        assert!(t.contains("阿三 12 万 较昨日▲2 万"));
        assert!(t.contains("小<李&仔 8000 较昨日▼1500"));
    }

    #[test]
    fn text_no_yesterday_omits_diff() {
        let mut rep = fixture();
        // 所有行昨日无数据 → 合计 diff None，逐徒弟也不显示「较昨日/▲/▼」
        for r in &mut rep.rows {
            r.yesterday = None;
        }
        rep.total_yesterday = None;
        let t = render_text(&rep);
        assert!(!t.contains("较昨日"));
        assert!(!t.contains('▲'));
        assert!(!t.contains('▼'));
    }

    #[test]
    fn svg_has_visible_and_hidden_watermark_on_every_page() {
        let rep = fixture();
        let pages = render_svg_pages_with_fields(&rep, 2, "浩雨", &LineageReportFields::default());
        assert_eq!(pages.len(), 2, "fixture 应分两页");
        for (i, svg) in pages.iter().enumerate() {
            assert!(
                svg.contains("内部数据 · 请勿外传 浩雨"),
                "第 {} 页缺可见水印",
                i + 1
            );
            assert!(
                svg.contains("fill-opacity=\"0.20\"") && svg.contains("fill-opacity=\"0.012\""),
                "第 {} 页缺双层隐形水印",
                i + 1
            );
        }
    }

    #[test]
    fn valid_date_str_cases() {
        assert!(valid_date_str("2026-10-14"));
        assert!(!valid_date_str("2026-10-4"));
        assert!(!valid_date_str("2026/10/14"));
        // 位数/字符都对但不是合法日历日期
        assert!(!valid_date_str("2026-13-40"));
        assert!(!valid_date_str(""));
        assert!(!valid_date_str("26-10-14"));
    }
}
