//! 师门日报：师傅收「名下徒弟音浪图 + 与上日的差距」（docs/23 §3）。
//!
//! 由 push.rs 的延迟推送队列触发（`push_bindings.lineage_report` 开关），
//! 图片口径与全团日报一致（wave_reliable=1 / 未播=当日音浪<2 / 直系徒弟）。
//! 渲染产物为分页 SVG（`render_svg_pages`），PNG 转换复用 push.rs::svg_to_png
//! （由调用方处理，本模块只产出 SVG 字符串）。

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::Row;

use crate::pgrs;
use crate::push::fmt_wan;
use crate::report::LineageReportFields;
use crate::state::AppState;

/* ------------------------------ 数据结构 ------------------------------ */

/// 单行：师傅行固定第一（is_master=true），其后徒弟按今日音浪降序、姓名升序。
#[derive(Debug, Clone)]
pub struct LineageRow {
    pub name: String,
    pub today: i64,
    /// 昨日音浪；昨日 map 里没有该人 = None（渲染为 `—`）
    pub yesterday: Option<i64>,
    pub month_total: i64,
    /// 开播 = dialect::is_live_wave(today)（wave<2 = 未播）
    pub live: bool,
    pub is_master: bool,
}

#[derive(Debug, Clone)]
pub struct LineageReport {
    pub master_name: String,
    pub date: String,
    pub rows: Vec<LineageRow>,
    pub total_today: i64,
    /// 昨日有值的行求和；昨日全无数据 = None
    pub total_yesterday: Option<i64>,
    /// 未播徒弟姓名（不含师傅），按 rows 顺序
    pub absent_names: Vec<String>,
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

/// 拉取师门日报数据。date 过白名单，master_id 内联进 SQL（u64 无注入面）。
pub async fn build(
    st: &Arc<AppState>,
    date: &str,
    master_person_id: u64,
) -> anyhow::Result<LineageReport> {
    if !valid_date_str(date) {
        anyhow::bail!("日期不合法");
    }
    if master_person_id == 0 {
        anyhow::bail!("师傅不存在");
    }
    let day = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|_| anyhow::anyhow!("日期不合法"))?;
    let yesterday = day
        .pred_opt()
        .ok_or_else(|| anyhow::anyhow!("日期不合法"))?
        .format("%Y-%m-%d")
        .to_string();
    let month = date.get(..7).unwrap_or(date);

    // a) 师傅本人（查不到 = 师傅不存在）
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

    // b) 名册：直系徒弟（隐藏/离职不进日报，与全团日报同一口径）
    let roster_sql = format!(
        "SELECT id, name FROM person WHERE master_id = {master_person_id} \
         AND deleted_at IS NULL AND hide_in_daily_report = 0 AND status <> 'left'"
    );

    // 今日 / 昨日 / 本月音浪：同一口径 wave_reliable = 1（与榜单/全团日报一致）
    let wave_day_sql = |d: &str| {
        format!(
            "SELECT person_id, (SUM(wave))::BIGINT AS wave FROM daily_metric \
             WHERE wave_reliable = 1 AND TO_CHAR(biz_date, 'YYYY-MM-DD') = '{d}' GROUP BY person_id"
        )
    };
    /* 本月累计（2026-10-17 修复，审计 P0-1）：原先这里是裸 `SUM(daily_metric.wave)`，
    而月榜图 / 个人月卡 / 推送 caption 早已改成「整月音浪 monthly_wave 优先、缺月回落
    日明细」。导了整月音浪后，师傅收到的师门日报图里「本月累计」会比别处少一截
    （只采到已导入日明细的天数）——同一天两个数字，师傅会拿图来对。
    现在复用 [`crate::report::month_wave_sql`]（同一份 COALESCE 口径），
    缺表场景交给 [`crate::dialect::query_month_wave_fallback`] 自动降级。 */
    let month_sql = crate::report::month_wave_sql(&month);
    let month_sql_daily = crate::report::month_wave_sql_daily(&month);

    let mut today_map: HashMap<u64, i64> = HashMap::new();
    for r in pgrs::query(&st.pools, "LOCAL", &wave_day_sql(date)).await? {
        today_map.insert(id_of(&r, "person_id"), signed(&r, "wave"));
    }
    let mut yday_map: HashMap<u64, i64> = HashMap::new();
    for r in pgrs::query(&st.pools, "LOCAL", &wave_day_sql(&yesterday)).await? {
        yday_map.insert(id_of(&r, "person_id"), signed(&r, "wave"));
    }
    let mut month_map: HashMap<u64, i64> = HashMap::new();
    // 缺表（库没跑 026 迁移）时自动回落到纯日明细版，师门日报不被打挂
    for r in
        crate::dialect::query_month_wave_fallback(&st.pools, "LOCAL", &month_sql, &month_sql_daily)
            .await?
    {
        month_map.insert(id_of(&r, "person_id"), signed(&r, "wave"));
    }

    let mut rows: Vec<LineageRow> = Vec::new();
    // 师傅行固定第一
    let master_today = today_map.get(&master_person_id).copied().unwrap_or(0);
    rows.push(LineageRow {
        name: master_name.clone(),
        today: master_today,
        yesterday: yday_map.get(&master_person_id).copied(),
        month_total: month_map.get(&master_person_id).copied().unwrap_or(0),
        live: crate::dialect::is_live_wave(master_today),
        is_master: true,
    });

    // 徒弟：今日降序、同分姓名升序
    let mut disciples: Vec<LineageRow> = Vec::new();
    for r in pgrs::query(&st.pools, "LOCAL", &roster_sql).await? {
        let pid = id_of(&r, "id");
        let name = r
            .try_get::<Option<String>, _>("name")
            .ok()
            .flatten()
            .unwrap_or_default();
        let today = today_map.get(&pid).copied().unwrap_or(0);
        disciples.push(LineageRow {
            name,
            today,
            yesterday: yday_map.get(&pid).copied(),
            month_total: month_map.get(&pid).copied().unwrap_or(0),
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
        rows,
        total_today,
        total_yesterday,
        absent_names,
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
            render_page(rep, chunk, &label, personal_mark, fields)
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

    // 表头（x/锚点与下方数据行同列一致）
    let th_y = header_h - 30.0; // 表头背景 y
    let ty_y = header_h - 14.0; // 表头文字 y
    s.push_str(&format!(
        "<rect y=\"{th_y:.0}\" width=\"720\" height=\"32\" fill=\"#F2F4F7\"/>"
    ));
    let columns: Vec<(f64, &str, &str)> = [
        (28.0f64, "主播", "start"),
        (240.0, "今日音浪", "middle"),
        (345.0, "昨日音浪", "middle"),
        (445.0, "较昨日", "middle"),
        (565.0, "本月累计", "middle"),
        (692.0, "状态", "end"),
    ]
    .into_iter()
    .filter(|&(_, label, _)| {
        (label == "主播" && fields.anchor)
            || (label == "今日音浪" && fields.today_wave)
            || (label == "昨日音浪" && fields.yesterday_wave)
            || (label == "较昨日" && fields.wave_diff)
            || (label == "本月累计" && fields.month_total)
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
                240.0,
                text_y,
                "middle",
                &fmt_wan(r.today),
                COLOR_TEXT,
            ));
        }
        if fields.yesterday_wave {
            match r.yesterday {
                Some(yv) => s.push_str(&text_cell(
                    345.0,
                    text_y,
                    "middle",
                    &fmt_wan(yv),
                    COLOR_TEXT,
                )),
                None => s.push_str(&text_cell(345.0, text_y, "middle", "—", COLOR_MUTED)),
            }
        }
        if fields.wave_diff {
            match r.yesterday {
                None => s.push_str(&text_cell(445.0, text_y, "middle", "—", COLOR_MUTED)),
                Some(yv) => {
                    let d = r.today - yv;
                    if d > 0 {
                        s.push_str(&text_cell(
                            445.0,
                            text_y,
                            "middle",
                            &format!("▲{}", fmt_wan(d)),
                            COLOR_UP,
                        ));
                    } else if d < 0 {
                        s.push_str(&text_cell(
                            445.0,
                            text_y,
                            "middle",
                            &format!("▼{}", fmt_wan(-d)),
                            COLOR_DOWN,
                        ));
                    } else {
                        s.push_str(&text_cell(445.0, text_y, "middle", "—", COLOR_MUTED));
                    }
                }
            }
        }
        if fields.month_total {
            s.push_str(&text_cell(
                565.0,
                text_y,
                "middle",
                &fmt_wan(r.month_total),
                "#475467",
            ));
        }
        if fields.status {
            if r.live {
                s.push_str(&text_cell(692.0, text_y, "end", "开播", COLOR_TEXT));
            } else {
                s.push_str(&text_cell(692.0, text_y, "end", "未播", COLOR_UP));
            }
        }
    }

    // 底部水印（2026-10-20 统一口径）：可见斜排 + 双层隐形（白 0.20 / 黑 0.012）。
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
    // 水印压在最顶层
    s.push_str(&watermark);
    s.push_str("</svg>");
    s
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
            yesterday,
            month_total,
            live,
            is_master,
        }
    }

    /// 1 师傅 + 3 徒弟：覆盖 yesterday None、▲、▼、未播、名字含 < &
    fn fixture() -> LineageReport {
        LineageReport {
            master_name: "冷师傅".to_string(),
            date: "2026-10-14".to_string(),
            rows: vec![
                row("冷师傅", 50_000, Some(45_000), 900_000, true, true), // ▲
                row("阿三", 120_000, Some(100_000), 1_200_000, true, false), // ▲
                row("小<李&仔", 8_000, Some(9_500), 300_000, true, false), // ▼
                row("小王", 0, None, 50_000, false, false),               // yesterday None + 未播
            ],
            total_today: 178_000,
            total_yesterday: Some(154_500),
            absent_names: vec!["小王".to_string()],
        }
    }

    #[test]
    fn total_diff_cases() {
        let mk = |today: i64, yday: Option<i64>| LineageReport {
            master_name: "m".to_string(),
            date: "2026-10-14".to_string(),
            rows: Vec::new(),
            total_today: today,
            total_yesterday: yday,
            absent_names: Vec::new(),
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
        let hidden = LineageReportFields {
            anchor: false,
            today_wave: false,
            yesterday_wave: false,
            wave_diff: false,
            month_total: false,
            status: false,
        };
        let none = render_svg_pages_with_fields(&rep, 0, "浩月", &hidden);
        for label in ["主播", "今日音浪", "昨日音浪", "较昨日", "本月累计", "状态"]
        {
            assert!(!none[0].contains(label), "关掉的列不该渲染表头：{label}");
        }
        assert!(
            !none[0].contains(">阿三</text>"),
            "主播列关掉后不应再出现姓名"
        );
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
