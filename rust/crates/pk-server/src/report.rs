//! 日报渲染（3328 render/testdata/apple.svg 模板口径）：
//! 纯服务端 SVG，无需浏览器。720 宽，行高 48；前三名金银铜徽章、
//! 当日音浪条形图、未播行红色高亮、底部未播者按师傅分组、"内部数据 · 请勿外传"水印。

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::Row;

use crate::pgrs;
use crate::state::AppState;

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PushReportFields {
    pub daily: DailyReportFields,
    pub lineage: LineageReportFields,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DailyReportFields {
    pub daily_star: bool,
    pub rank: bool,
    pub name: bool,
    pub absent_days: bool,
    pub wave: bool,
    pub total_wave: bool,
    pub status: bool,
}

impl Default for DailyReportFields {
    fn default() -> Self {
        Self {
            daily_star: true,
            rank: true,
            name: true,
            absent_days: true,
            wave: true,
            total_wave: true,
            status: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LineageReportFields {
    pub anchor: bool,
    pub today_wave: bool,
    pub yesterday_wave: bool,
    pub wave_diff: bool,
    pub month_total: bool,
    pub status: bool,
}

impl Default for LineageReportFields {
    fn default() -> Self {
        Self {
            anchor: true,
            today_wave: true,
            yesterday_wave: true,
            wave_diff: true,
            month_total: true,
            status: true,
        }
    }
}

impl PushReportFields {
    pub fn from_json(value: &str) -> Self {
        serde_json::from_str(value).unwrap_or_default()
    }

    pub fn to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

/// 日报数据（含当日指标、当月未播天数、累计音浪）
pub struct DailyReport {
    pub date: String,
    pub team: String,
    pub rows: Vec<ReportRow>,
    pub master_groups: Vec<(String, Vec<String>)>, // 未播者按师傅分组
    pub no_master: Vec<String>,
    /// 每日之星（2026-10-12）：（称号, 名字, 音浪）。推送图头部 🚩 标出。
    /// 称号后台可配（config 表 star_title，如「初级争霸赛冠军」），默认「每日之星」；
    /// None = 不显示星条（网页版日报保持 None，版式不动）。
    pub daily_star: Option<StarBanner>,
}

#[derive(Clone, Debug)]
pub struct StarBanner {
    /// 称号（后台可配，如「初级争霸赛冠军」/「每日之星」）
    pub title: String,
    /// 主播名（如「狼哲」）
    pub name: String,
    /// 音浪值（可 0 = 只展示称号+名字）
    pub wave: i64,
    /// 图标：空 = 默认 🚩 emoji；以 "data:image" 开头 = 内嵌 SVG `<image>`
    pub icon: String,
}

#[derive(Clone, Debug)]
pub struct ReportRow {
    pub name: String,
    pub wave: i64,
    pub minutes: i64,
    pub is_live: bool,
    pub absent_days: i64, // 当月未播天数（有数据行中）
    pub total_wave: i64,
}

/// SVG/XML 文本转义。修复4：补 &quot; / &#39; —— 水印 personal_mark、师傅分组等
/// 都来自用户可控字符串（账号名、台账姓名），原版只转义 & < >，名字里带引号
/// 会提前闭合属性/标签，构成 SVG 注入面。& → &amp; 的既有行为保持不变，
/// 已有调用点不会双转义（转义只做一次，顺序 & 在前）。
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn fmt_wan(v: i64) -> String {
    if v >= 10_000 {
        let w = v as f64 / 10_000.0;
        if w >= 100.0 {
            format!("{} 万", w.round() as i64)
        } else {
            format!("{:.1} 万", (w * 10.0).round() / 10.0)
        }
    } else {
        format!("{v}")
    }
}

/// SVG 文本宽度的保守估算（resvg 没有文本度量接口）：CJK / emoji 按 1.0×字号、
/// 其余（ASCII 数字字母空格标点）按 0.55×字号。只用于「日期 + 每日之星」单行的
/// 居中起点计算，几个像素的误差肉眼不可见，也不会影响其它列的固定 x 坐标。
fn approx_text_width(s: &str, size: f64) -> f64 {
    s.chars()
        .map(|c| {
            if (c as u32) < 0x2E80 {
                size * 0.55
            } else {
                size
            }
        })
        .sum()
}

/// 拉取日报数据。date/team 已由调用方校验。
pub async fn build(st: &Arc<AppState>, date: &str, team: &str) -> anyhow::Result<DailyReport> {
    use crate::dialect::is_live_wave;
    // 当日指标（P1-7：与榜单一致，只聚合可信行 wave_reliable=1）
    let today_sql = format!(
        "SELECT person_id, (SUM(wave))::BIGINT AS wave, \
         (SUM(minutes))::BIGINT AS m, MAX(is_live) AS live \
         FROM daily_metric WHERE wave_reliable = 1 \
           AND TO_CHAR(biz_date, 'YYYY-MM-DD') = '{date}' GROUP BY person_id"
    );
    // 当月未播天数：与展示端榜单共用 dialect::MONTH_ABSENT_SQL（只有一份口径，
    // 见那里的注释：只算有数据行，缺采的日子不算未播）。
    // 2026-10-14 审计修复（P1）：原来写 `&date[..7]`。chrono 的 parse_from_str 对
    // **非补零输入照样返回 Ok**（"1-1-1" 长 5、"26-1-1" 长 6），所以上游的
    // valid_date_str 挡不住短串 —— 越界直接 panic（实测：byte index 7 is out of bounds）。
    // HTTP 侧（daily_svg 的 query date）与机器人侧（"日报 男团 26-1-1"）都可达。
    // 改成安全切片，与同文件 build_month 的 `get(..7)` 口径一致。
    let month = date.get(..7).unwrap_or(date);
    let month_sql = crate::dialect::month_absent_sql(month);
    // 累计音浪（≤ date 的全部增量求和；P1-7 total 口径与 WAVES_A 一致：只算可靠行。
    // 基线日 wave_reliable=0 记的是累计基数，其 wave 值本来就是 0/不可信，
    // 排除它不会少算当日口径的累计 —— 「累计总音浪」列展示的是当日口径累计）。
    let total_sql = format!(
        "SELECT person_id, (SUM(wave))::BIGINT AS total \
         FROM daily_metric WHERE wave_reliable = 1 \
           AND TO_CHAR(biz_date, 'YYYY-MM-DD') <= '{date}' GROUP BY person_id"
    );
    // 名册（含师傅；P1-10：隐藏/离职的主播不进日报 —— 对外 SVG 不该再出现他们）
    let roster_sql = format!(
        "SELECT p.id, p.name, m.name AS master_name FROM person p \
         LEFT JOIN person m ON m.id = p.master_id \
         WHERE p.deleted_at IS NULL AND p.gender = '{team}' \
           AND p.hide_in_daily_report = 0 AND p.status <> 'left'"
    );

    let mut wave_map: HashMap<u64, i64> = Default::default();
    let mut minutes_map: HashMap<u64, i64> = Default::default();
    let mut live_map: HashMap<u64, bool> = Default::default();
    for r in pgrs::query(&st.pools, "LOCAL", &today_sql).await? {
        let pid = id_of(&r, "person_id");
        wave_map.insert(pid, signed(&r, "wave"));
        minutes_map.insert(pid, signed(&r, "m"));
        live_map.insert(pid, r.try_get::<i16, _>("live").unwrap_or(0) == 1);
    }
    let mut absent_map: HashMap<u64, i64> = Default::default();
    for r in pgrs::query(&st.pools, "LOCAL", &month_sql).await? {
        absent_map.insert(id_of(&r, "person_id"), signed(&r, "n"));
    }
    let mut total_map: HashMap<u64, i64> = Default::default();
    for r in pgrs::query(&st.pools, "LOCAL", &total_sql).await? {
        total_map.insert(id_of(&r, "person_id"), signed(&r, "total"));
    }

    let mut rows: Vec<ReportRow> = Vec::new();
    let mut master_groups: HashMap<String, Vec<String>> = Default::default();
    let mut no_master: Vec<String> = Vec::new();
    for r in pgrs::query(&st.pools, "LOCAL", &roster_sql).await? {
        let pid = id_of(&r, "id");
        let name: String = r.try_get::<String, _>("name").unwrap_or_default();
        let master: String = r
            .try_get::<Option<String>, _>("master_name")
            .ok()
            .flatten()
            .unwrap_or_default();
        let wave = wave_map.get(&pid).copied().unwrap_or(0);
        let total = total_map.get(&pid).copied().unwrap_or(0);
        let absent = absent_map.get(&pid).copied().unwrap_or(0);
        // P1-10：未播 = 当日音浪 < 2（与展示端 / 推送榜共用 dialect::is_live_wave），
        // is_live 标记只作参考，不再单独作为判定依据
        let is_live = is_live_wave(wave);
        rows.push(ReportRow {
            name: name.clone(),
            wave,
            minutes: minutes_map.get(&pid).copied().unwrap_or(0),
            is_live,
            absent_days: absent,
            total_wave: total,
        });
        // 未播者按师傅分组（3328 口径：底部汇总行）
        if !is_live {
            if master.is_empty() {
                no_master.push(name);
            } else {
                master_groups.entry(master).or_default().push(name);
            }
        }
    }
    rows.sort_by(|a, b| b.wave.cmp(&a.wave).then(a.name.cmp(&b.name)));
    let mut master_groups: Vec<(String, Vec<String>)> = master_groups.into_iter().collect();
    master_groups.sort();
    let daily_star = rows.iter().find(|r| r.is_live).map(|r| StarBanner {
        title: "每日之星".to_string(),
        name: r.name.clone(),
        wave: r.wave,
        icon: String::new(),
    });
    Ok(DailyReport {
        date: date.to_string(),
        team: team.to_string(),
        rows,
        master_groups,
        no_master,
        daily_star,
    })
}

fn id_of(r: &sqlx::postgres::PgRow, col: &str) -> u64 {
    if let Ok(v) = r.try_get::<i64, _>(col) {
        return v.max(0) as u64;
    }
    if let Ok(v) = r.try_get::<i64, _>(col) {
        return v as u64;
    }
    if let Ok(v) = r.try_get::<String, _>(col) {
        return v.parse().unwrap_or(0);
    }
    0
}

fn signed(r: &sqlx::postgres::PgRow, col: &str) -> i64 {
    if let Ok(v) = r.try_get::<i64, _>(col) {
        return v;
    }
    if let Ok(v) = r.try_get::<i64, _>(col) {
        return v;
    }
    if let Ok(v) = r.try_get::<String, _>(col) {
        return v.parse().unwrap_or(0);
    }
    0
}

/* 2026-10-17 字体栈改版（子代理诊断）：去掉 CSS 专有的 -apple-system（fontdb 当普通名查
永远 miss，白进 warn log），补全 Linux 常见 CJK 家族提高服务器命中率；本机 macOS 命中
AssetsV2 的 PingFang SC（/tmp/resvg-font-test B9 实测与旧栈同像素）。前提是 svg_to_png
必须先 load_system_fonts()（resvg 0.45 默认空 fontdb，见 push.rs 同日注释）。 */
const FONT: &str = "PingFang SC, Hiragino Sans GB, Heiti SC, STHeiti, Microsoft YaHei, Noto Sans CJK SC, Noto Sans SC, Source Han Sans SC, WenQuanYi Zen Hei, sans-serif";
const MONO: &str = "SF Mono, Menlo, Consolas, monospace";
const WATERMARK: &str = "内部数据 · 请勿外传";

/// 日报月榜的「本月累计音浪」SQL（2026-10-16 月度音浪导入）：该月**有导入的整月音浪**
/// （monthly_wave）优先，没有才回落 daily_metric 日明细求和 —— 与展示端
/// [`crate::dialect::period_rollup_sql`]、推送月榜同一口径，免得日报与展示端对不上。
/// `(person_id, period)` 唯一，所以 `MAX(mw.wave)` 就是那一个整月值；没有整月行时它是 NULL
/// → COALESCE 落到日明细（只算可信行 wave_reliable=1）。返回 person_id / wave，
/// 没有数据的人不进结果集（调用方按缺省 0 处理，语义不变）。
///
/// `pub(crate)`（2026-10-17 修复，审计 P0-1）：师门日报 [`crate::lineage_report`] 原先
/// 自己写了一份裸 `SUM(wave)`，导了整月音浪后会与月榜 / 个人卡差一截，这里统一复用，
/// 免得再多一份口径。缺表降级走 [`crate::dialect::query_month_wave_fallback`]。
pub(crate) fn month_wave_sql(month: &str) -> String {
    format!(
        "SELECT p.id AS person_id, (COALESCE(MAX(mw.wave), SUM(d.wave), 0))::BIGINT AS wave \
         FROM person p \
         LEFT JOIN daily_metric d ON d.person_id = p.id \
           AND TO_CHAR(d.biz_date, 'YYYY-MM') = '{month}' \
           AND d.wave_reliable = 1 \
         LEFT JOIN monthly_wave mw ON mw.person_id = p.id AND mw.period = '{month}' \
         WHERE p.deleted_at IS NULL \
         GROUP BY p.id"
    )
}

/// 库没跑 026 迁移（monthly_wave 不存在）时的纯日明细版：与旧口径逐字一致，
/// 由 [`crate::dialect::query_month_wave_fallback`] 在缺表时自动换用，日报不被打挂。
pub(crate) fn month_wave_sql_daily(month: &str) -> String {
    format!(
        "SELECT person_id, (SUM(wave))::BIGINT AS wave \
         FROM daily_metric WHERE wave_reliable = 1 \
           AND TO_CHAR(biz_date, 'YYYY-MM') = '{month}' GROUP BY person_id"
    )
}

/// 渲染日报 SVG（apple 风模板，720 宽，行高 48）
/// 月榜构建（2026-10-12 微信推送口径）：rows = 本月累计音浪降序（含未开播者），
/// absent_days = 当月未播天数，total_wave = 本月累计（同值复用），daily_star = 🚩 当日之星。
/// 与日报 build 同一份名册口径（在册/未离团/不隐藏）。
pub async fn build_month(
    st: &std::sync::Arc<AppState>,
    date: &str,
    team: &str,
    daily_star: Option<StarBanner>,
) -> anyhow::Result<DailyReport> {
    use crate::dialect::is_live_wave;
    let month = date.get(..7).unwrap_or(date).to_string();
    // 本月累计（可信行）：整月导入值优先、缺月回落日明细；缺表自动降级（见上面两个函数）
    let month_sql = month_wave_sql(&month);
    let month_sql_daily = month_wave_sql_daily(&month);
    // 名册 + 师傅
    let roster_sql = format!(
        "SELECT p.id, p.name, m.name AS master_name FROM person p          LEFT JOIN person m ON m.id = p.master_id          WHERE p.deleted_at IS NULL AND p.gender = '{team}'            AND p.hide_in_daily_report = 0 AND p.status <> 'left'"
    );
    // 当月未播天数（与日报同口径：只有数据行里 <2 的算）
    let month_absent_sql = crate::dialect::month_absent_sql(&month);

    let mut wave_map: HashMap<u64, i64> = Default::default();
    for r in
        crate::dialect::query_month_wave_fallback(&st.pools, "LOCAL", &month_sql, &month_sql_daily)
            .await?
    {
        wave_map.insert(id_of(&r, "person_id"), signed(&r, "wave"));
    }
    let mut absent_map: HashMap<u64, i64> = Default::default();
    for r in pgrs::query(&st.pools, "LOCAL", &month_absent_sql).await? {
        absent_map.insert(id_of(&r, "person_id"), signed(&r, "n"));
    }

    let mut rows: Vec<ReportRow> = Vec::new();
    let mut master_groups: HashMap<String, Vec<String>> = Default::default();
    let mut no_master: Vec<String> = Vec::new();
    for r in pgrs::query(&st.pools, "LOCAL", &roster_sql).await? {
        let pid = id_of(&r, "id");
        let name: String = r.try_get::<String, _>("name").unwrap_or_default();
        let master: String = r
            .try_get::<Option<String>, _>("master_name")
            .ok()
            .flatten()
            .unwrap_or_default();
        let wave = wave_map.get(&pid).copied().unwrap_or(0);
        let absent = absent_map.get(&pid).copied().unwrap_or(0);
        let is_live = is_live_wave(wave);
        rows.push(ReportRow {
            name: name.clone(),
            wave,
            minutes: 0,
            is_live,
            absent_days: absent,
            total_wave: wave, // 月榜的「总音浪」列 = 本月累计（同一数值不重复算口径）
        });
        if !is_live {
            if master.is_empty() {
                no_master.push(name);
            } else {
                master_groups.entry(master).or_default().push(name);
            }
        }
    }
    rows.sort_by(|a, b| b.wave.cmp(&a.wave).then(a.name.cmp(&b.name)));
    let mut master_groups: Vec<(String, Vec<String>)> = master_groups.into_iter().collect();
    master_groups.sort();
    Ok(DailyReport {
        date: date.to_string(),
        team: team.to_string(),
        rows,
        master_groups,
        no_master,
        daily_star,
    })
}

/// 分页渲染（2026-10-12 用户口径）：榜单太长微信里看不清，拆两张发。
/// 第 1 页 = 已开播榜（TOP 在前，含标题/表头/统计脚注）；第 2 页 = 未开播名单
/// （红底行 + 师傅分组警示条）。每页独立成图，各自带全量斜排水印。
/// 页内行数上限 `per_page`（0 = 不拆）。返回空 Vec 只在 rows 为空时发生。
/// `personal_mark` 非空时，斜排水印追加收件人艺名（如「浩月的专属数据」），
/// 防转发扩散——管理员后台 /report 接口传空，推送引擎按每个用户传艺名。
pub fn render_svg_pages(rep: &DailyReport, per_page: usize, personal_mark: &str) -> Vec<String> {
    render_svg_pages_with_fields(rep, per_page, personal_mark, &DailyReportFields::default())
}

pub fn render_svg_pages_with_fields(
    rep: &DailyReport,
    per_page: usize,
    personal_mark: &str,
    fields: &DailyReportFields,
) -> Vec<String> {
    if rep.rows.is_empty() {
        return Vec::new();
    }
    if per_page == 0 || rep.rows.len() <= per_page {
        // 单页/首页：名次从 1 开始，row_offset = 0（修复2）
        return vec![render_svg_labeled(
            rep,
            "",
            rep.rows.len(),
            personal_mark,
            fields,
            0,
        )];
    }
    /* 排序口径与 render_svg 一致：音浪降序（rows 已按此排好，不重复排）。
    第 1 页取已开播者（保持名次），第 2 页放未开播者。开播者超过一页时
    第 1 页装满 per_page 行，剩余开播者继续占用第 2 页开头（名次连续）。 */
    let mut pages_rows: Vec<Vec<ReportRow>> = Vec::new();
    let mut cur: Vec<ReportRow> = Vec::new();
    for r in &rep.rows {
        cur.push(ReportRow {
            name: r.name.clone(),
            wave: r.wave,
            minutes: r.minutes,
            is_live: r.is_live,
            absent_days: r.absent_days,
            total_wave: r.total_wave,
        });
        if cur.len() == per_page {
            pages_rows.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        pages_rows.push(cur);
    }

    let total = rep.rows.len();
    let page_count = pages_rows.len();
    pages_rows
        .into_iter()
        .enumerate()
        .map(|(pi, rows)| {
            let page_label = if page_count > 1 {
                format!("（第 {} / {} 页）", pi + 1, page_count)
            } else {
                String::new()
            };
            /* 未开播统计口径：全量数据里的未播者（不是本页的）。
            警示条只在含未播者的页出现（即最后一页），避免重复。 */
            let is_last = pi + 1 == page_count;
            let page_rep = DailyReport {
                date: rep.date.clone(),
                team: rep.team.clone(),
                master_groups: if is_last {
                    rep.master_groups.clone()
                } else {
                    Vec::new()
                },
                no_master: if is_last {
                    rep.no_master.clone()
                } else {
                    Vec::new()
                },
                // 🚩 冠军条每页都带（2026-10-12 用户口径：每页都要显示）
                daily_star: rep.daily_star.clone(),
                rows,
            };
            render_svg_labeled_inner_with_all(
                &page_rep,
                &page_label,
                total,
                personal_mark,
                false,
                fields,
                &rep.rows,
                pi * per_page,
            )
        })
        .collect()
}

/// render_svg 的分页变体：标题带页码、footer 人数显示全团口径。
/// `total` = 全团人数（跨页统计用）；页内行数 = 本页行数。
/// `row_offset` = 本页第一行在全团榜单里的名次偏移（修复2：前面页行数之和，
/// 名次跨页连续；原先 `total - n` 只对「两页且当前是末页」成立，>2 页时第 2 页起编号错）。
fn render_svg_labeled(
    rep: &DailyReport,
    page_label: &str,
    total: usize,
    personal_mark: &str,
    fields: &DailyReportFields,
    row_offset: usize,
) -> String {
    render_svg_labeled_inner(
        rep,
        page_label,
        total,
        personal_mark,
        false,
        fields,
        row_offset,
    )
}

/// 月榜分页渲染（2026-10-12 微信推送口径）：本月累计音浪 + 当月未播天数，
/// 标题「本月音浪榜」，表头列名切换。分页逻辑与日报一致。
pub fn render_month_svg_pages(
    rep: &DailyReport,
    per_page: usize,
    personal_mark: &str,
) -> Vec<String> {
    render_month_svg_pages_with_fields(rep, per_page, personal_mark, &DailyReportFields::default())
}

/// 月榜「定页数」分页渲染（2026-10-17 用户口径「每日报告分成两页即可」）：
/// 与 `render_month_svg_pages_with_fields` 同一套页内重打包/页码/名次连续逻辑，
/// 但切法由调用方给定（如固定 2 页对半分），不再按 per_page 均匀切。
/// `pages_rows` 为空 = 没有行，返回空 Vec（调用方退文本）。
pub fn render_month_svg_pages_exact(
    rep: &DailyReport,
    pages_rows: Vec<Vec<ReportRow>>,
    personal_mark: &str,
    fields: &DailyReportFields,
) -> Vec<String> {
    if pages_rows.is_empty() {
        return Vec::new();
    }
    let total = rep.rows.len();
    let page_count = pages_rows.len();
    let mut offset = 0usize;
    pages_rows
        .into_iter()
        .enumerate()
        .map(|(pi, rows)| {
            let page_label = if page_count > 1 {
                format!("（第 {} / {} 页）", pi + 1, page_count)
            } else {
                String::new()
            };
            let is_last = pi + 1 == page_count;
            let page_rep = DailyReport {
                date: rep.date.clone(),
                team: rep.team.clone(),
                master_groups: if is_last {
                    rep.master_groups.clone()
                } else {
                    Vec::new()
                },
                no_master: if is_last {
                    rep.no_master.clone()
                } else {
                    Vec::new()
                },
                // 🚩 冠军条每页都带（2026-10-12 用户口径：每页都要显示）
                daily_star: rep.daily_star.clone(),
                rows,
            };
            let svg = render_svg_labeled_inner_with_all(
                &page_rep,
                &page_label,
                total,
                personal_mark,
                true,
                fields,
                &rep.rows,
                offset,
            );
            offset += page_rep.rows.len();
            svg
        })
        .collect()
}

pub fn render_month_svg_pages_with_fields(
    rep: &DailyReport,
    per_page: usize,
    personal_mark: &str,
    fields: &DailyReportFields,
) -> Vec<String> {
    if rep.rows.is_empty() {
        return Vec::new();
    }
    if per_page == 0 || rep.rows.len() <= per_page {
        return vec![render_svg_labeled_inner(
            rep,
            "",
            rep.rows.len(),
            personal_mark,
            true,
            fields,
            0,
        )];
    }
    let mut pages_rows: Vec<Vec<ReportRow>> = Vec::new();
    let mut cur: Vec<ReportRow> = Vec::new();
    for r in &rep.rows {
        cur.push(ReportRow {
            name: r.name.clone(),
            wave: r.wave,
            minutes: r.minutes,
            is_live: r.is_live,
            absent_days: r.absent_days,
            total_wave: r.total_wave,
        });
        if cur.len() == per_page {
            pages_rows.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        pages_rows.push(cur);
    }
    let total = rep.rows.len();
    let page_count = pages_rows.len();
    pages_rows
        .into_iter()
        .enumerate()
        .map(|(pi, rows)| {
            let page_label = if page_count > 1 {
                format!("（第 {} / {} 页）", pi + 1, page_count)
            } else {
                String::new()
            };
            let is_last = pi + 1 == page_count;
            let page_rep = DailyReport {
                date: rep.date.clone(),
                team: rep.team.clone(),
                master_groups: if is_last {
                    rep.master_groups.clone()
                } else {
                    Vec::new()
                },
                no_master: if is_last {
                    rep.no_master.clone()
                } else {
                    Vec::new()
                },
                // 🚩 冠军条每页都带（2026-10-12 用户口径：每页都要显示）
                daily_star: rep.daily_star.clone(),
                rows,
            };
            // 修复2：第 pi 页的名次从「前面页行数之和」起编（pi*per_page），跨页连续
            render_svg_labeled_inner_with_all(
                &page_rep,
                &page_label,
                total,
                personal_mark,
                true,
                fields,
                &rep.rows,
                pi * per_page,
            )
        })
        .collect()
}

/// 单页渲染的便捷入口：全量行 = 本页行（测试 / 非分页路径使用）。
fn render_svg_labeled_inner(
    rep: &DailyReport,
    page_label: &str,
    total: usize,
    personal_mark: &str,
    is_month: bool,
    fields: &DailyReportFields,
    row_offset: usize,
) -> String {
    render_svg_labeled_inner_with_all(
        rep,
        page_label,
        total,
        personal_mark,
        is_month,
        fields,
        &rep.rows,
        row_offset,
    )
}

#[allow(clippy::too_many_lines)]
fn render_svg_labeled_inner_with_all(
    rep: &DailyReport,
    page_label: &str,
    total: usize,
    personal_mark: &str,
    is_month: bool,
    fields: &DailyReportFields,
    all_rows: &[ReportRow],
    // 修复2：本页第一行的名次偏移 = 前面页行数之和（0 = 单页/首页）。
    // 原先函数内用 `total - n` 推导，只对「两页且当前是末页」成立。
    row_offset: usize,
) -> String {
    let n = rep.rows.len();
    // 头部高度固定 126px（2026-10-17 用户口径「每日之星放到日报头部」：
    // 星信息并入日期行，不再单独占一条 40px 横幅，图片整体更紧凑）。
    let header_h = 126.0f64;
    let list_h = n as f64 * 48.0;
    let footer_y = header_h + list_h + 20.0;
    let total_h = footer_y + 50.0;
    // 分页时也必须用全量榜单的最大值，否则第二页低音浪会被放大成和第一页第一名一样长。
    let max_wave = all_rows.iter().map(|r| r.wave).max().unwrap_or(1).max(1);
    let th_y = header_h - 40.0; // 表头背景 y
    let ty_y = header_h - 22.0; // 表头文字 y
    let team_label = if rep.team == "male" {
        "男团"
    } else {
        "女团"
    };

    let mut s = String::with_capacity(64 * 1024);
    s.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1440\" height=\"{}\" viewBox=\"0 0 720 {total_h:.0}\" role=\"img\">",
        total_h as i64 * 2
    ));
    s.push_str(&format!(
        "<rect width=\"720\" height=\"{total_h:.0}\" fill=\"#FFFFFF\"/>"
    ));

    // 水印层（2026-10-12 用户口径）：推送图只压收件人艺名（如「浩月」），
    // 斜排密铺防转发；管理后台网页版（无 personal_mark）保持通用水印。
    // ⚠️ 渲染顺序：水印压在最顶层（条形图/数值之上）——先攒着，</svg> 前再拼接。
    // 修复4：水印文字（personal_mark 来自收件人账号名）必须过 esc——两层隐形水印
    // 与 footer 呼应处共用同一份转义文本；seed 仍用原始 personal_mark 计算，
    // 保证转义前后的水印位置不变（不因修复4改变随机种子）。
    let mark_escaped = esc(personal_mark.trim());
    let marks: Vec<String> = if personal_mark.trim().is_empty() {
        vec![WATERMARK.to_string()]
    } else {
        vec![mark_escaped.clone()]
    };
    // 随机分布（2026-10-12 用户口径）：网格基础上每点加 ±18px 抖动。
    // 种子 = 日期|团别|收件人 的 FNV hash → 同一张图稳定可复现（检测方按同种子
    // 重建网格做相关），不同人/不同天的水印位置互不相同，防固定网格批量抹除。
    let seed = {
        let mut h: u64 = 1469598103934665603;
        for b in format!("{}|{}|{}", rep.date, rep.team, personal_mark).bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(1099511628211);
        }
        h
    };
    let jitter = |i: u64| -> f64 {
        let mut z = i.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(seed);
        z ^= z >> 30;
        z = z.wrapping_mul(0xBF58476D1CE4E5B9);
        z ^= z >> 27;
        ((z % 37) as f64) - 18.0
    };
    let mut watermark_layer = String::new();
    let mut wi: u64 = 0;
    for (si, x) in [-30.0f64, 90.0, 210.0, 330.0, 450.0, 570.0, 690.0]
        .into_iter()
        .enumerate()
    {
        for (yi, y) in (0..((total_h / 52.0).ceil() as i64 + 1)).enumerate() {
            let y = y as f64 * 52.0 - 30.0;
            let mark = &marks[(si + yi) % marks.len()];
            wi += 1;
            let wx = x + jitter(wi);
            let wy = y + jitter(wi.wrapping_add(0x5DEECE66D));
            // 双层隐形水印：
            //  a) 白色文字压白底/浅底（fill #FFFFFF, opacity 0.5）→ 肉眼不可见，
            //     在白/浅色背景上形成极细微的亮度差；
            //  b) 同坐标再叠一层纯黑 opacity 0.004 → 在蓝条/深色区域形成可检测暗差。
            // 两层都是实心文字字形，像素层面稳定存在，程序按模板位置做相关即可提取。
            watermark_layer.push_str(&format!(
                "<text x=\"{wx:.0}\" y=\"{wy:.0}\" font-family=\"{FONT}\" font-size=\"22\" font-weight=\"900\" fill=\"#FFFFFF\" fill-opacity=\"0.20\" text-anchor=\"middle\" transform=\"rotate(-18 {wx:.0} {wy:.0})\">{mark}</text>"
            ));
            watermark_layer.push_str(&format!(
                "<text x=\"{wx:.0}\" y=\"{wy:.0}\" font-family=\"{FONT}\" font-size=\"22\" font-weight=\"900\" fill=\"#000000\" fill-opacity=\"0.012\" text-anchor=\"middle\" transform=\"rotate(-18 {wx:.0} {wy:.0})\">{mark}</text>"
            ));
        }
    }

    // 标题：月榜图明确写「月榜」，避免和当日日报混读。
    let report_title = if is_month {
        "主播月榜日报"
    } else {
        "主播数据日报"
    };
    s.push_str(&format!(
        "<text x=\"360\" y=\"21\" font-size=\"25\" fill=\"#101828\" font-weight=\"700\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">{report_title} · {team_label}{page_label}</text>"
    ));
    let date_cn = format_cn_date(&rep.date);

    /* 2026-10-17 用户口径：**每日之星并入日报头部**。
    原先是「日期一行 + 单独一条 40px 琥珀横幅」，头部会撑到 168px；现在日期与
    每日之星同一行居中，星信息压成一颗紧凑胶囊 —— 头部恒定 126px（无星时日期
    单独居中），图和表头都上移，第一眼仍然是「谁得了每日之星」。
    icon 支持 data:image 内嵌图；空 / 非 data:image 回退 🚩 emoji。 */
    let star = if fields.daily_star {
        rep.daily_star.as_ref()
    } else {
        None
    };
    match star {
        None => {
            s.push_str(&format!(
                "<text x=\"360\" y=\"52\" font-size=\"13\" fill=\"#475467\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">{date_cn}</text>"
            ));
        }
        Some(star) => {
            const SUB_SIZE: f64 = 13.0;
            let use_image_icon = !star.icon.is_empty() && star.icon.starts_with("data:image");
            let mut main = star.name.clone();
            if star.wave > 0 {
                main.push_str(&format!(" · {}", fmt_wan(star.wave)));
            }
            // 胶囊内边距 / 图标 / 间隙（估算宽度只影响居中，肉眼级误差可接受）
            const PAD: f64 = 10.0;
            const GAP: f64 = 6.0;
            const ICON_W: f64 = 16.0;
            let title_w = approx_text_width(&star.title, SUB_SIZE);
            let main_w = approx_text_width(&main, SUB_SIZE);
            // +8px 右侧余量：估算宽度对中文标点/空格偏保守，留一点呼吸位避免文字顶到描边
            let pill_w = PAD + ICON_W + GAP + title_w + 10.0 + main_w + PAD + 8.0;
            let pill_h = 24.0f64;
            let pill_y = 52.0 - pill_h / 2.0;
            let date_w = approx_text_width(&date_cn, SUB_SIZE);
            let sep_gap = 10.0;
            let mut x = 360.0 - (date_w + sep_gap + pill_w) / 2.0;
            s.push_str(&format!(
                "<text x=\"{x:.1}\" y=\"52\" font-size=\"{SUB_SIZE}\" fill=\"#475467\" text-anchor=\"start\" dominant-baseline=\"central\" font-family=\"{FONT}\">{date_cn}</text>"
            ));
            x += date_w + sep_gap;
            s.push_str(&format!(
                "<rect x=\"{x:.1}\" y=\"{pill_y:.1}\" width=\"{pill_w:.1}\" height=\"{pill_h:.1}\" fill=\"#FFFAEB\" rx=\"{:.1}\" stroke=\"#FEDF89\"/>",
                pill_h / 2.0
            ));
            let icon_x = x + PAD;
            if use_image_icon {
                s.push_str(&format!(
                    "<image x=\"{icon_x:.1}\" y=\"{:.1}\" width=\"{ICON_W}\" height=\"{ICON_W}\" href=\"{}\" preserveAspectRatio=\"xMidYMid meet\"/>",
                    52.0 - ICON_W / 2.0,
                    esc(&star.icon)
                ));
            } else {
                s.push_str(&format!(
                    "<text x=\"{icon_x:.1}\" y=\"52\" font-size=\"16\" text-anchor=\"start\" dominant-baseline=\"central\" font-family=\"{FONT}\">🚩</text>"
                ));
            }
            let title_x = icon_x + ICON_W + GAP;
            s.push_str(&format!(
                "<text x=\"{title_x:.1}\" y=\"52\" font-size=\"{SUB_SIZE}\" fill=\"#B54708\" font-weight=\"700\" text-anchor=\"start\" dominant-baseline=\"central\" font-family=\"{FONT}\">{}</text>",
                esc(&star.title)
            ));
            let main_x = title_x + title_w + 10.0;
            s.push_str(&format!(
                "<text x=\"{main_x:.1}\" y=\"52\" font-size=\"{SUB_SIZE}\" fill=\"#101828\" font-weight=\"700\" text-anchor=\"start\" dominant-baseline=\"central\" font-family=\"{FONT}\">{}</text>",
                esc(&main)
            ));
        }
    }

    // 表头（月榜：当日音浪列 → 本月累计音浪；未播天数/总音浪保留）
    // (x, 标签, text-anchor)：x/锚点与下方数据行同列一致，保证表头与列值对齐
    s.push_str(&format!(
        "<rect y=\"{th_y:.0}\" width=\"720\" height=\"34\" fill=\"#F2F4F7\"/>"
    ));
    let wave_col_label = if is_month {
        "本月累计音浪"
    } else {
        "当日音浪"
    };
    /* 最后一列：日榜是历史累计；月榜不再重复左边同一个「本月累计」，
    改为「距上一名」，用全量榜单跨页计算差距。 */
    let total_col_label = if is_month {
        "距上一名"
    } else {
        "累计总音浪"
    };
    let columns: Vec<(f64, &str, &str)> = [
        (32.3, "排名", "middle"),
        (72.6, "主播姓名", "start"),
        (253.8, "当月未播天数", "middle"),
        (433.8, wave_col_label, "middle"),
        (708.0, total_col_label, "end"),
    ]
    .into_iter()
    .filter(|&(_, label, _)| {
        (label == "排名" && fields.rank)
            || (label == "主播姓名" && fields.name)
            || (label == "当月未播天数" && fields.absent_days)
            || (label == "本月累计音浪" && fields.wave)
            || (label == "当日音浪" && fields.wave)
            || (label == "距上一名" && fields.total_wave)
            || (label == "累计总音浪" && fields.total_wave)
    })
    .collect();
    for (x, label, anchor) in columns {
        s.push_str(&format!(
            "<text x=\"{x}\" y=\"{ty_y:.0}\" font-size=\"12\" fill=\"#667085\" font-weight=\"700\" text-anchor=\"{anchor}\" dominant-baseline=\"central\" font-family=\"{FONT}\">{label}</text>"
        ));
    }

    // 数据行（名次按全团连续编号：修复2 —— row_offset 参数由分页方传入（前面页行数之和），
    // >2 页时编号依然连续；原先函数内 `total - n` 只在「两页且当前是末页」时碰巧正确）
    let bar_track_w = 242.4f64; // 312.6 + 242.4 = 555，中心 433.8，与表头严格对齐
    let wave_value_x = 433.8f64;
    for (i, r) in rep.rows.iter().enumerate() {
        let y = header_h + i as f64 * 48.0;
        let text_y = y + 25.0;
        let rank = row_offset + i + 1;
        let off_air = !r.is_live;
        let is_me = !personal_mark.trim().is_empty() && r.name == personal_mark.trim();
        let row_bg = if off_air {
            "#FFF7F7"
        } else if is_me {
            "#F0F7FF"
        } else {
            "#FFFFFF"
        };
        let row_stroke = if off_air {
            "#FEE4E2"
        } else if is_me {
            "#B2DDFF"
        } else {
            "#EAECF0"
        };
        s.push_str(&format!(
            "<rect y=\"{y:.0}\" width=\"720\" height=\"48\" fill=\"{row_bg}\" stroke=\"{row_stroke}\" stroke-width=\"1\"/>"
        ));
        if off_air || is_me {
            let accent = if off_air { "#F04438" } else { "#007AFF" };
            s.push_str(&format!(
                "<rect y=\"{:.0}\" width=\"4\" height=\"32\" fill=\"{accent}\" rx=\"2\"/>",
                y + 8.0
            ));
        }
        // 排名徽章（前三名金/银/铜）
        let (badge_bg, badge_stroke, badge_fg) = match rank {
            1 => ("#FFFAEB", "#FEDF89", "#B54708"),
            2 => ("#F9FAFB", "#D0D5DD", "#475467"),
            3 => ("#FFF6ED", "#FED7AA", "#C4320A"),
            _ => ("#FFF1F2", "#FFE4E6", "#B42318"),
        };
        let badge_fg = if off_air || rank <= 3 {
            badge_fg
        } else {
            "#475467"
        };
        let badge_bg = if off_air { "#FFF1F2" } else { badge_bg };
        if fields.rank {
            s.push_str(&format!(
                "<rect x=\"12.3\" y=\"{:.0}\" width=\"40\" height=\"24\" fill=\"{badge_bg}\" rx=\"12\" stroke=\"{badge_stroke}\"/><text x=\"32.3\" y=\"{text_y:.0}\" font-size=\"12\" fill=\"{badge_fg}\" font-weight=\"700\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{MONO}\">{:02}</text>",
                y + 12.0,
                rank
            ));
        }
        let name_fg = if off_air {
            "#B42318"
        } else if is_me {
            "#005FCC"
        } else {
            "#101828"
        };
        if fields.name {
            let name_text = if is_me {
                format!("{}（我）", r.name)
            } else {
                r.name.clone()
            };
            s.push_str(&format!(
                "<text x=\"72.6\" y=\"{text_y:.0}\" font-size=\"14\" fill=\"{name_fg}\" font-weight=\"700\" text-anchor=\"start\" dominant-baseline=\"central\" font-family=\"{FONT}\">{}</text>",
                esc(&name_text)
            ));
        }
        let absent_fg = if off_air { "#B42318" } else { "#027A48" };
        if fields.absent_days {
            s.push_str(&format!(
                "<text x=\"253.8\" y=\"{text_y:.0}\" font-size=\"13\" fill=\"{absent_fg}\" font-weight=\"700\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">{}</text>",
                r.absent_days
            ));
        }
        if fields.wave {
            if off_air {
                s.push_str(&format!(
                    "<rect x=\"312.6\" y=\"{:.0}\" width=\"{bar_track_w}\" height=\"22\" fill=\"#FEE4E2\" rx=\"11\"/><text x=\"433.8\" y=\"{text_y:.0}\" font-size=\"13\" fill=\"#D92D20\" font-weight=\"700\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">未开播</text>",
                    y + 14.0
                ));
            } else {
                let w = (bar_track_w * r.wave as f64 / max_wave as f64).round();
                s.push_str(&format!(
                    "<rect x=\"312.6\" y=\"{:.0}\" width=\"{bar_track_w}\" height=\"22\" fill=\"#EAF3FF\" rx=\"11\"/>",
                    y + 14.0
                ));
                if w > 1.0 {
                    s.push_str(&format!(
                        "<rect x=\"312.6\" y=\"{:.0}\" width=\"{w:.0}\" height=\"22\" fill=\"#007AFF\" rx=\"11\"/>",
                        y + 14.0
                    ));
                }
                /* 2026-10-20：数字放回粗条中心，与「本月累计音浪」表头同 x。
                白色半透明胶囊保证浅色底 / 蓝色填充上都清晰，不再把值放到条外。 */
                s.push_str(&format!(
                    "<rect x=\"{:.1}\" y=\"{:.0}\" width=\"56\" height=\"16\" fill=\"#FFFFFF\" fill-opacity=\"0.86\" rx=\"8\"/><text x=\"{wave_value_x}\" y=\"{text_y:.0}\" font-size=\"13\" fill=\"#344054\" font-weight=\"700\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{MONO}\">{}</text>",
                    wave_value_x - 28.0,
                    text_y - 8.0,
                    esc(&fmt_wan(r.wave))
                ));
            }
        } else if fields.status && off_air {
            s.push_str(&format!(
                "<text x=\"433.8\" y=\"{text_y:.0}\" font-size=\"13\" fill=\"#D92D20\" font-weight=\"700\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">未开播</text>"
            ));
        }
        if fields.total_wave {
            let right_text = if is_month {
                let idx = row_offset + i;
                let gap = if idx == 0 {
                    all_rows
                        .get(1)
                        .map(|next| (r.wave - next.wave).max(0))
                        .unwrap_or(0)
                } else {
                    all_rows
                        .get(idx - 1)
                        .map(|prev| (prev.wave - r.wave).max(0))
                        .unwrap_or(0)
                };
                if all_rows.len() <= 1 {
                    "—".to_string()
                } else if gap == 0 {
                    "持平".to_string()
                } else if idx == 0 {
                    format!("领先 {}", fmt_wan(gap))
                } else {
                    format!("差 {}", fmt_wan(gap))
                }
            } else {
                fmt_wan(r.total_wave)
            };
            s.push_str(&format!(
                "<text x=\"708\" y=\"{text_y:.0}\" font-size=\"13\" fill=\"#475467\" text-anchor=\"end\" dominant-baseline=\"central\" font-family=\"{MONO}\">{}</text>",
                esc(&right_text)
            ));
        }
    }

    // footer：数据说明 + 未播统计（全量口径，分页时不能只数本页）
    let off_count = all_rows.iter().filter(|r| !r.is_live).count();
    s.push_str(&format!(
        "<text x=\"8\" y=\"{footer_y:.0}\" font-size=\"12\" fill=\"#667085\" font-weight=\"600\" text-anchor=\"start\" dominant-baseline=\"central\" font-family=\"{FONT}\">数据日期 {} · {} {total} 人</text>",
        date_cn,
        team_label,
    ));
    s.push_str(&format!(
        "<text x=\"712\" y=\"{footer_y:.0}\" font-size=\"12\" fill=\"#667085\" font-weight=\"600\" text-anchor=\"end\" dominant-baseline=\"central\" font-family=\"{FONT}\">未开播 {} 人</text>",
        off_count
    ));
    s.push_str(&format!(
        "<text x=\"360\" y=\"{footer_y:.0}\" font-size=\"12\" fill=\"#98A2B3\" font-weight=\"700\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">{}</text>",
        // 修复4：personal_mark 同样要过 esc（与水印层共用 mark_escaped）
        if personal_mark.trim().is_empty() { WATERMARK.to_string() } else { mark_escaped }
    ));

    // 水印最顶层：盖在榜单/条形图/数值/冠军条之上，全图可见
    s.push_str(&watermark_layer);
    s.push_str("</svg>");
    s
}

pub fn render_svg(rep: &DailyReport) -> String {
    // 单页渲染：名次从 1 开始，row_offset = 0（修复2 新增参数）；
    // 网页版固定全字段显示（推送图的字段显隐只作用于微信推送）。
    render_svg_labeled(
        rep,
        "",
        rep.rows.len(),
        "",
        &DailyReportFields::default(),
        0,
    )
}

fn format_cn_date(iso: &str) -> String {
    match chrono::NaiveDate::parse_from_str(iso, "%Y-%m-%d") {
        Ok(d) => d.format("%Y年%-m月%-d日").to_string(),
        Err(_) => iso.to_string(),
    }
}

/// 机器人「日报」命令的文本版
///
/// 时长口径（2026-10-16 用户口径「机器人推送的也改」）：与展示端同一条 `x时x分` 规则
/// （[`crate::push::fmt_minutes`]，不带秒、不用「分钟」长词）。**时长为 0 时不显示**——
/// 0 表示没有时长数据（老文件没有「时长 / 开播有效时长」列，或当天没开播），
/// 拼出来只会是噪音「X / 0分」；
/// 这与展示端领奖台 / 榜单行对 `minutes === 0` 的隐藏语义一致。
pub fn render_text(rep: &DailyReport) -> String {
    let team_label = if rep.team == "male" {
        "男团"
    } else {
        "女团"
    };
    let mut out = format!("{} {} 日报（{}）\n", team_label, rep.date, rep.rows.len());
    for (i, r) in rep.rows.iter().take(15).enumerate() {
        let status = if r.is_live {
            match r.minutes {
                0 => fmt_wan(r.wave),
                m => format!("{} / 时长{}", fmt_wan(r.wave), crate::push::fmt_minutes(m)),
            }
        } else {
            format!("未开播（当月第 {} 天）", r.absent_days)
        };
        out.push_str(&format!(
            "{}. {} {} · 累计 {}\n",
            i + 1,
            r.name,
            status,
            fmt_wan(r.total_wave)
        ));
    }
    let off: Vec<&str> = rep
        .rows
        .iter()
        .filter(|r| !r.is_live)
        .map(|r| r.name.as_str())
        .collect();
    if !off.is_empty() {
        out.push_str(&format!("\n⚠️ 未开播 {} 人：{}", off.len(), off.join("、")));
    }
    out
}

/* ------------------------------- HTTP handler ------------------------------- */

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct ReportQuery {
    pub date: Option<String>,
    pub team: Option<String>,
}

/// GET /api/admin/report/daily.svg?date=&team=male
/// 需要管理员鉴权；<img> 引用可用 ?access_token= 传 token（auth 中间件支持）
pub async fn daily_svg(
    State(st): State<Arc<AppState>>,
    Query(q): Query<ReportQuery>,
) -> Result<Response, crate::routes::ApiError> {
    let dates = crate::repos::get_available_dates(&st, Some("LOCAL"), false).await?;
    let date = match q.date.as_deref() {
        Some(d) if valid_date_str(d) => d.to_string(),
        _ => dates.first().cloned().unwrap_or_default(),
    };
    if date.is_empty() {
        // 2026-10-14 审计 P2：库里一条业务日都没有 → 这是运营可理解的状态，不是服务端故障。
        // 原先走 anyhow 兜底落 500，运营只看到「服务器开小差了」。显式 400 + 明确文案。
        return Err(crate::routes::ApiError::bad_request(
            "还没有采集到任何日期（先在「导入」页上传一份音浪文件）",
        ));
    }
    let team = match q.team.as_deref() {
        Some("female") => "female",
        _ => "male",
    };
    let rep = build(&st, &date, team).await?;
    let svg = render_svg(&rep);
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "image/svg+xml; charset=utf-8",
        )],
        svg,
    )
        .into_response())
}

/// 2026-10-14 审计修复（P1）：原来只靠 `parse_from_str("%Y-%m-%d")`，而 chrono 对
/// **非补零输入照样返回 Ok** —— "1-1-1"（长 5）、"26-1-1"（长 6）都能通过，
/// 随后 `&date[..7]` 就越界 panic。这里补上长度约束，把宽松解析彻底封死。
/// 合法业务日必须是定长 10 位的 `YYYY-MM-DD`。
fn valid_date_str(s: &str) -> bool {
    s.len() == 10 && chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-14 审计回归：chrono 的 `parse_from_str("%Y-%m-%d")` 对**非补零**输入
    /// 也返回 Ok（"1-1-1"、"26-1-1" 都能过）。只靠解析做校验会让短串流到 `date[..7]`
    /// 处越界 panic。这里把「能通过 valid_date_str 的输入一定安全取前 7 位」钉死。
    #[test]
    fn valid_date_str_rejects_short_and_unpadded_dates() {
        // 合法：定长补零
        assert!(valid_date_str("2026-10-14"));
        // 不合法：非补零（chrono 会解析成功，但长度不足 → 旧实现越界 panic）
        assert!(!valid_date_str("1-1-1"), "\"1-1-1\" 只有 5 位，必须拒绝");
        assert!(!valid_date_str("26-1-1"), "\"26-1-1\" 只有 6 位，必须拒绝");
        assert!(!valid_date_str("2026-1-1"), "月/日未补零必须拒绝");
        assert!(!valid_date_str(""));
        assert!(!valid_date_str("2026-13-45"));

        // 更进一步：任何被判为合法的串，安全切片都不能 panic
        for s in ["2026-10-14", "2026-01-01", "9999-12-31"] {
            assert!(valid_date_str(s));
            let month = s.get(..7).unwrap_or(s);
            assert_eq!(month.len(), 7);
        }
    }

    /// 日报月榜的月口径（2026-10-16 月度音浪导入）：该月有导入的整月音浪
    /// （monthly_wave）就用它，没有才回落日明细求和 —— 与展示端 period_rollup_sql /
    /// 推送 month_wave_map 同一套 COALESCE，否则日报的「本月累计音浪」会与展示端对不上。
    /// 缺表（026 未跑）时由 query_month_wave_fallback 换纯日明细版。
    #[test]
    fn month_wave_sql_prefers_monthly_import_over_daily_sum() {
        let sql = month_wave_sql("2999-01");
        assert!(
            sql.contains("COALESCE(MAX(mw.wave), SUM(d.wave), 0)"),
            "整月优先、缺月回落日明细：{sql}"
        );
        assert!(
            sql.contains(
                "LEFT JOIN monthly_wave mw ON mw.person_id = p.id AND mw.period = '2999-01'"
            ),
            "整月音浪按 (人, 月) 对齐：{sql}"
        );
        assert!(
            sql.contains("AND TO_CHAR(d.biz_date, 'YYYY-MM') = '2999-01'"),
            "回落窗口要带年份，否则跨年串月：{sql}"
        );
        assert!(sql.contains("AND d.wave_reliable = 1"), "只算可信行：{sql}");
        // 列名不变（wave_map 按 person_id / wave 读，缺省 0）
        assert!(
            sql.contains("AS person_id") && sql.contains("AS wave"),
            "{sql}"
        );

        let daily = month_wave_sql_daily("2999-01");
        assert!(
            !daily.contains("monthly_wave") && !daily.contains("mw."),
            "缺表回落版不能带整月表 / 别名：{daily}"
        );
        assert!(daily.contains("(SUM(wave))::BIGINT AS wave"), "{daily}");
        assert!(daily.contains("WHERE wave_reliable = 1"), "{daily}");
        assert!(
            daily.contains("TO_CHAR(biz_date, 'YYYY-MM') = '2999-01'"),
            "{daily}"
        );
    }

    /// 取 SVG 中包含 `label` 的完整 `<text>` 元素（含起始标签），便于断言表头锚点。
    fn text_element_of(svg: &str, label: &str) -> String {
        let idx = svg
            .find(label)
            .unwrap_or_else(|| panic!("SVG 中找不到标签: {label}"));
        let start = svg[..idx]
            .rfind("<text")
            .unwrap_or_else(|| panic!("标签 {label} 前找不到 <text>"));
        let end = idx + label.len() + svg[idx + label.len()..].find("</text>").unwrap_or(0);
        svg[start..end].to_string()
    }

    /// 开发预览（不参与断言）：跑 `cargo test -p pk-server --lib preview -- --nocapture`
    /// 会把微信推送月榜两页渲染到 /tmp/preview-new-{1,2}.svg（真实分页逻辑 + 40 行/页）。
    #[test]
    fn preview_push_svg_pages() {
        let rows: Vec<ReportRow> = [
            ("浩龙", 547_308, 1),
            ("狼澈", 301_154, 1),
            ("狼哲", 285_917, 0),
            ("凤九", 277_929, 0),
            ("南方楠", 234_259, 1),
            ("浩冬", 221_000, 1),
            ("狼兴", 169_000, 0),
            ("玖玥", 148_000, 1),
            ("狼俊", 141_000, 0),
            ("浩鸣", 137_000, 2),
            ("狼小宝", 136_000, 0),
            ("啸恒", 122_000, 0),
            ("浩艺", 116_000, 1),
            ("玖柒", 113_000, 0),
            ("狼辰", 108_000, 1),
            ("浩辰", 100_000, 1),
            ("浩雨", 99_000, 1),
            ("浩沐", 97_000, 0),
            ("啸帆", 93_000, 1),
            ("浩阳", 82_000, 0),
            ("浩玟", 78_000, 1),
            ("啸辰", 78_000, 0),
            ("狼赫", 75_000, 1),
            ("狼泽", 73_000, 0),
            ("狼岳", 69_000, 0),
            ("玖雪", 67_000, 0),
            ("玖豆", 61_000, 0),
            ("啸森", 58_000, 0),
            ("浩楠", 56_000, 1),
            ("狼仔", 55_000, 1),
            ("浩艺2", 53_000, 0),
            ("啸宇", 52_000, 0),
            ("玖妹", 50_000, 0),
            ("狼辉", 50_000, 1),
            ("啸安", 49_000, 1),
            ("浩然", 49_000, 0),
            ("浩森2", 49_000, 1),
            ("狼腾", 45_000, 0),
            ("狼九", 45_000, 0),
            ("狼明", 41_000, 2),
            ("狼轩", 38_000, 1),
            ("啸弘", 37_000, 0),
            ("浩运", 35_000, 1),
            ("狼征", 35_000, 2),
            ("凤瑶", 30_000, 0),
            ("狼艺", 29_000, 1),
            ("啸阳", 28_000, 1),
            ("浩泽", 28_000, 1),
            ("浩杰", 27_000, 1),
            ("浩宁", 25_000, 1),
            ("狼影", 24_000, 1),
            ("狼雨", 21_000, 0),
            ("啸昭", 19_000, 0),
            ("啸寻", 16_000, 0),
            ("狼耀", 16_000, 0),
            ("狼瑞", 14_000, 0),
            ("啸钰", 13_000, 0),
            ("浩启", 11_000, 2),
            ("啸升", 10_000, 0),
            ("浩月", 9_945, 1),
            ("啸泽2", 9_942, 1),
            ("浩延", 9_403, 2),
            ("狼旭", 8_441, 1),
            ("啸强", 7_850, 1),
            ("啸燃", 7_051, 1),
            ("凤夏", 6_196, 0),
            ("狼帅", 5_974, 1),
            ("玖久", 2_977, 0),
            ("狼彬", 1_922, 0),
            ("玖安", 1_169, 0),
            ("玖忆", 917, 0),
            ("狼途", 895, 1),
            ("狼鑫", 763, 0),
            ("啸墨", 405, 2),
            ("浩鑫", 296, 1),
            ("啸鸣", 225, 0),
            ("狼卓", 6, 1),
            ("狼霞", 0, 2),
            ("啸沐", 0, 2),
            ("玖玉", 0, 2),
            ("狼裕", 0, 2),
            ("狼航", 0, 2),
            ("玖依", 0, 2),
            ("浩坤", 0, 2),
        ]
        .into_iter()
        .map(|(name, wave, absent)| ReportRow {
            name: name.into(),
            wave,
            minutes: 0,
            is_live: wave >= 2,
            absent_days: absent,
            total_wave: wave,
        })
        .collect();
        let rep = DailyReport {
            date: "2026-10-11".into(),
            team: "male".into(),
            rows,
            master_groups: vec![("薇姐".into(), vec!["晓爱".into()])],
            no_master: vec![],
            daily_star: Some(StarBanner {
                title: "初级争霸赛冠军".into(),
                name: "狼哲".into(),
                wave: 285_917,
                icon: String::new(),
            }),
        };
        // 收件人：env PREVIEW_NAMES（逗号分隔，缺省浩月）。每个人渲染带自己隐形水印的
        // 第 1 页 → /tmp/preview-<名字>-p1.svg（2026-10-12 用户要求：随机四张不同姓名）。
        let names: Vec<String> = std::env::var("PREVIEW_NAMES")
            .unwrap_or_else(|_| "浩月".to_string())
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        for name in &names {
            let svgs = render_month_svg_pages(&rep, 40, name);
            let path = format!("/tmp/preview-{}-p1.svg", name);
            if std::fs::write(&path, svgs.first().map(String::as_str).unwrap_or("")).is_ok() {
                println!("written {} (watermark={})", path, name);
            }
        }
        // 全页输出（原行为，收件人固定第一个名字）
        let pages = render_month_svg_pages(
            &rep,
            40,
            names.first().map(String::as_str).unwrap_or("浩月"),
        );
        for (i, svg) in pages.iter().enumerate() {
            let path = format!("/tmp/preview-new-{}.svg", i + 1);
            if std::fs::write(&path, svg).is_ok() {
                println!("written {}", path);
            }
        }
    }

    /// 每人专属水印（2026-10-12）：同一榜单、不同收件人 → 水印文字必须是
    /// 各自艺名（双层：白 0.20 + 黑 0.012），且互相不同。
    #[test]
    fn per_recipient_hidden_watermark() {
        let mk_rows = || {
            vec![ReportRow {
                name: "浩龙".into(),
                wave: 547_308,
                minutes: 0,
                is_live: true,
                absent_days: 1,
                total_wave: 547_308,
            }]
        };
        let build = || DailyReport {
            date: "2026-10-11".into(),
            team: "male".into(),
            rows: mk_rows(),
            master_groups: vec![],
            no_master: vec![],
            daily_star: None,
        };
        let a = render_svg_labeled_inner(
            &build(),
            "",
            1,
            "浩月",
            true,
            &DailyReportFields::default(),
            0,
        );
        let b = render_svg_labeled_inner(
            &build(),
            "",
            1,
            "狼征",
            true,
            &DailyReportFields::default(),
            0,
        );
        // 双层水印都在：白 0.20 + 黑 0.012（2026-10-19 降低白色强度，避免糊住数值）
        assert!(a.contains("fill-opacity=\"0.20\"") && a.contains("fill-opacity=\"0.012\""));
        // 文字是各自艺名，且互不相同
        assert!(a.contains(&format!(">{}</text>", "浩月")));
        assert!(b.contains(&format!(">{}</text>", "狼征")));
        assert_ne!(
            a.matches(">浩月</text>").count(),
            0,
            "浩月版水印文字应为浩月"
        );
        assert!(b.contains(">狼征</text>"));
        assert!(!b.contains(">浩月</text>"), "狼征的图里不能出现浩月水印");
        // 管理后台版（无 personal_mark）保持通用水印，不带个人名
        let admin =
            render_svg_labeled_inner(&build(), "", 1, "", true, &DailyReportFields::default(), 0);
        assert!(admin.contains(WATERMARK));
        assert!(!admin.contains(">浩月</text>"));
    }

    #[test]
    fn month_svg_headers_align_with_values() {
        let rep = DailyReport {
            date: "2026-10-11".into(),
            team: "male".into(),
            rows: vec![ReportRow {
                name: "浩龙".into(),
                wave: 547_308,
                minutes: 0,
                is_live: true,
                absent_days: 1,
                total_wave: 547_308,
            }],
            master_groups: vec![],
            no_master: vec![],
            daily_star: Some(StarBanner {
                title: "初级争霸赛冠军".into(),
                name: "狼哲".into(),
                wave: 285_917,
                icon: String::new(),
            }),
        };
        let svg =
            render_svg_labeled_inner(&rep, "", 1, "浩月", true, &DailyReportFields::default(), 0);
        // 表头锚点与列值锚点一一对应：主播姓名 start @72.6，末列 end @708。
        // 2026-10-19：月榜末列改为「距上一名」，不再重复左边同一个本月累计。
        let h_name = text_element_of(&svg, "主播姓名");
        assert!(
            h_name.contains("x=\"72.6\""),
            "姓名表头 x 应与列值同为 72.6: {h_name}"
        );
        assert!(
            h_name.contains("text-anchor=\"start\""),
            "姓名表头要 start 对齐: {h_name}"
        );
        assert!(
            !svg.contains(">累计总音浪</text>"),
            "月榜不该再用日榜的「累计总音浪」表头（语义是本月累计）"
        );
        let h_total = text_element_of(&svg, "距上一名");
        assert!(
            h_total.contains("x=\"708\""),
            "距上一名表头 x 应与列值同为 708: {h_total}"
        );
        assert!(
            h_total.contains("text-anchor=\"end\""),
            "总音浪列头要 end 对齐"
        );
        // 其余列：排名/未播天数/本月累计音浪 均与列值同 x 同锚点
        let h_rank = text_element_of(&svg, "排名");
        assert!(h_rank.contains("x=\"32.3\"") && h_rank.contains("text-anchor=\"middle\""));
        let h_absent = text_element_of(&svg, "当月未播天数");
        assert!(h_absent.contains("x=\"253.8\"") && h_absent.contains("text-anchor=\"middle\""));
        let h_wave = text_element_of(&svg, "本月累计音浪");
        assert!(h_wave.contains("x=\"433.8\"") && h_wave.contains("text-anchor=\"middle\""));
        // 每日之星条：称号 + 名字（icon 空 = 默认 🚩 emoji）
        assert!(svg.contains("初级争霸赛冠军"));
        assert!(svg.contains("狼哲"));
        assert!(svg.contains("🚩"), "icon 为空时应回退默认 🚩 emoji");
        assert!(!svg.contains("<image "), "icon 为空时不应渲染 <image>");
    }

    #[test]
    fn star_banner_renders_data_image_icon() {
        let rep = DailyReport {
            date: "2026-10-11".into(),
            team: "male".into(),
            rows: vec![ReportRow {
                name: "浩龙".into(),
                wave: 100,
                minutes: 0,
                is_live: true,
                absent_days: 0,
                total_wave: 100,
            }],
            master_groups: vec![],
            no_master: vec![],
            daily_star: Some(StarBanner {
                title: "每日之星".into(),
                name: "狼哲".into(),
                wave: 0,
                icon: "data:image/png;base64,iVBOR&x<y".into(),
            }),
        };
        let svg = render_svg_labeled_inner(&rep, "", 1, "", true, &DailyReportFields::default(), 0);
        // data:image 图标走 <image>（2026-10-17 起缩成 16×16 的头部胶囊图标），
        // href 经 esc() 转义 & < >
        assert!(svg.contains("<image "), "data:image 图标应渲染为 <image>");
        assert!(
            svg.contains(
                "width=\"16\" height=\"16\" href=\"data:image/png;base64,iVBOR&amp;x&lt;y\""
            ),
            "头部胶囊图标应为 16×16 且 href 做转义"
        );
        assert!(
            !svg.contains(">🚩</text>"),
            "有 data:image 图标时不应再画 🚩 emoji"
        );
        // 2026-10-17 用户口径：每日之星并入头部日期行（不再单独占 40px 横幅）
        let title = text_element_of(&svg, "每日之星");
        assert!(
            title.contains("y=\"52\""),
            "每日之星要并入日报头部日期行（同一行 y=52）: {title}"
        );
        assert!(
            !svg.contains("width=\"696\" height=\"40\""),
            "不应再有单独的小红旗横幅"
        );
    }

    /// 2026-10-17 用户口径「字段后端支持显示和不显示」：推送图字段来自 config
    /// `push_report_fields`，关掉的列必须真的从 SVG 里消失（表头 + 数值一起）。
    #[test]
    fn daily_report_fields_hide_columns() {
        let rep = DailyReport {
            date: "2026-10-11".into(),
            team: "male".into(),
            rows: vec![ReportRow {
                name: "浩龙".into(),
                wave: 100,
                minutes: 0,
                is_live: true,
                absent_days: 3,
                total_wave: 500,
            }],
            master_groups: vec![],
            no_master: vec![],
            daily_star: None,
        };
        let all = DailyReportFields::default();
        let svg_all = render_svg_labeled_inner(&rep, "", 1, "", false, &all, 0);
        assert!(svg_all.contains("排名") && svg_all.contains("主播姓名"));

        let hidden = DailyReportFields {
            daily_star: false,
            rank: false,
            name: false,
            absent_days: false,
            wave: false,
            total_wave: false,
            status: false,
        };
        let svg_none = render_svg_labeled_inner(&rep, "", 1, "", false, &hidden, 0);
        for label in ["排名", "主播姓名", "当月未播天数", "当日音浪", "累计总音浪"]
        {
            assert!(!svg_none.contains(label), "关掉的列不该渲染表头：{label}");
        }
        assert!(
            !svg_none.contains(">浩龙</text>"),
            "姓名列关掉后不应再出现姓名"
        );
    }

    /// 2026-10-20：音浪粗条与「本月累计音浪」表头中心对齐，数值放回条内。
    #[test]
    fn wave_value_text_is_centered_inside_track() {
        let rep = DailyReport {
            date: "2026-10-16".into(),
            team: "male".into(),
            rows: vec![
                ReportRow {
                    name: "第一名".into(),
                    wave: 800_000,
                    minutes: 0,
                    is_live: true,
                    absent_days: 0,
                    total_wave: 800_000,
                },
                ReportRow {
                    name: "末位".into(),
                    wave: 250,
                    minutes: 0,
                    is_live: true,
                    absent_days: 0,
                    total_wave: 250,
                },
            ],
            master_groups: vec![],
            no_master: vec![],
            daily_star: None,
        };
        let svg =
            render_svg_labeled_inner(&rep, "", 2, "", false, &DailyReportFields::default(), 0);
        assert!(
            svg.matches("fill=\"#344054\" font-weight=\"700\"").count() >= 2,
            "音浪值应使用深色文字: {svg}"
        );
        assert!(
            svg.contains("width=\"242.4\"") && svg.contains("x=\"312.6\""),
            "音浪粗条应从 312.6 起、宽 242.4，中心与表头 433.8 对齐: {svg}"
        );
        assert!(
            svg.contains("x=\"433.8\"") && svg.contains("text-anchor=\"middle\""),
            "音浪值应放回粗条中心: {svg}"
        );
        assert!(
            svg.contains("fill-opacity=\"0.86\""),
            "条内数值应有半透明胶囊保证可读性: {svg}"
        );
        assert!(
            !svg.contains("paint-order=\"stroke\""),
            "不应再依赖白字描边"
        );
    }

    #[test]
    fn monthly_report_gap_column_and_marks_viewer() {
        let rep = DailyReport {
            date: "2026-10-10".into(),
            team: "male".into(),
            rows: vec![
                ReportRow {
                    name: "浩龙".into(),
                    wave: 90_000,
                    minutes: 0,
                    is_live: true,
                    absent_days: 1,
                    total_wave: 90_000,
                },
                ReportRow {
                    name: "浩月".into(),
                    wave: 10_000,
                    minutes: 0,
                    is_live: true,
                    absent_days: 0,
                    total_wave: 10_000,
                },
            ],
            master_groups: vec![],
            no_master: vec![],
            daily_star: None,
        };
        let svg =
            render_svg_labeled_inner(&rep, "", 2, "浩月", true, &DailyReportFields::default(), 0);
        assert!(
            svg.contains("领先 8.0 万"),
            "第一名应显示领先第二名的差值: {svg}"
        );
        assert!(
            svg.contains("差 8.0 万"),
            "第二名应显示距上一名的差值: {svg}"
        );
        assert!(
            svg.contains("浩月（我）"),
            "收件人本人行应标记「我」: {svg}"
        );
        assert!(
            svg.contains("fill=\"#F0F7FF\""),
            "收件人本人行应有浅蓝高亮: {svg}"
        );
    }

    #[test]
    fn pagination_bars_use_global_max_wave() {
        let rep = DailyReport {
            date: "2026-10-10".into(),
            team: "male".into(),
            rows: vec![
                ReportRow {
                    name: "第一名".into(),
                    wave: 100_000,
                    minutes: 0,
                    is_live: true,
                    absent_days: 0,
                    total_wave: 100_000,
                },
                ReportRow {
                    name: "第二名".into(),
                    wave: 10_000,
                    minutes: 0,
                    is_live: true,
                    absent_days: 0,
                    total_wave: 10_000,
                },
                ReportRow {
                    name: "第三名".into(),
                    wave: 1_000,
                    minutes: 0,
                    is_live: true,
                    absent_days: 0,
                    total_wave: 1_000,
                },
            ],
            master_groups: vec![],
            no_master: vec![],
            daily_star: None,
        };
        let pages = render_month_svg_pages(&rep, 2, "");
        assert_eq!(pages.len(), 2);
        assert!(
            pages[1].contains("width=\"2\""),
            "第二页条形图必须按全量最大音浪缩放，而不是页内最大值"
        );
    }

    #[test]
    fn pagination_footer_uses_full_off_air_count() {
        let rep = DailyReport {
            date: "2026-10-10".into(),
            team: "male".into(),
            rows: vec![
                ReportRow {
                    name: "开播".into(),
                    wave: 90_000,
                    minutes: 0,
                    is_live: true,
                    absent_days: 0,
                    total_wave: 90_000,
                },
                ReportRow {
                    name: "未播".into(),
                    wave: 0,
                    minutes: 0,
                    is_live: false,
                    absent_days: 2,
                    total_wave: 0,
                },
                ReportRow {
                    name: "未播2".into(),
                    wave: 0,
                    minutes: 0,
                    is_live: false,
                    absent_days: 2,
                    total_wave: 0,
                },
            ],
            master_groups: vec![],
            no_master: vec![],
            daily_star: None,
        };
        let pages = render_month_svg_pages(&rep, 1, "");
        assert_eq!(pages.len(), 3);
        for (i, page) in pages.iter().enumerate() {
            assert!(
                page.contains("未开播 2 人"),
                "第 {} 页 footer 应使用全量未播人数: {page}",
                i + 1
            );
        }
    }

    /// 机器人「日报」文本的时长口径（2026-10-16 用户口径「机器人推送的也改」）：
    /// 与展示端同一条 `x时x分` 规则、不带「分钟」长词；**时长 0 不显示**
    /// （0 = 没有时长数据：老文件没有时长列或当天没开播，拼出来是「X / 0分」这种噪音）。
    #[test]
    fn bot_daily_report_text_formats_duration_like_display() {
        let mk = |name: &str, wave: i64, minutes: i64| ReportRow {
            name: name.into(),
            wave,
            minutes,
            is_live: true,
            absent_days: 0,
            total_wave: wave,
        };
        let rep = DailyReport {
            date: "2026-10-16".into(),
            team: "male".into(),
            rows: vec![
                mk("浩龙", 63_531, 117),         // 1时57分（10.13.csv 的真实值）
                mk("玖玉", 23_403, 0),           // 没有时长数据 → 只显示音浪
                mk("狼霆", 3_193, 75 * 60 + 29), // 75时29分
            ],
            master_groups: vec![],
            no_master: vec![],
            daily_star: None,
        };
        let t = render_text(&rep);
        assert!(t.contains("1. 浩龙 6.4 万 / 时长1时57分"), "{t}");
        assert!(t.contains("2. 玖玉 2.3 万"), "{t}");
        assert!(
            !t.lines()
                .any(|l| l.contains("2. 玖玉") && l.contains("0分")),
            "时长 0 不显示：{t}"
        );
        assert!(t.contains("3. 狼霆 3193 / 时长75时29分"), "{t}");
        // 全文不许再出现旧口径的「分钟」长词（未播行、统计行都不带）
        assert!(!t.contains("分钟"), "机器人日报文本不应再用「分钟」：{t}");
    }
}
