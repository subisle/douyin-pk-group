//! 日报渲染（3328 render/testdata/apple.svg 模板口径）：
//! 纯服务端 SVG，无需浏览器。720 宽，行高 48；前三名金银铜徽章、
//! 当日音浪条形图、未播行红色高亮、底部未播者按师傅分组、"内部数据 · 请勿外传"水印。

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::Row;

use crate::mysql;
use crate::state::AppState;

/// 日报数据（含当日指标、当月未播天数、累计音浪）
pub struct DailyReport {
    pub date: String,
    pub team: String,
    pub rows: Vec<ReportRow>,
    pub master_groups: Vec<(String, Vec<String>)>, // 未播者按师傅分组
    pub no_master: Vec<String>,
}

pub struct ReportRow {
    pub name: String,
    pub wave: i64,
    pub minutes: i64,
    pub is_live: bool,
    pub absent_days: i64, // 当月未播天数（有数据行中）
    pub total_wave: i64,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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

/// 拉取日报数据。date/team 已由调用方校验。
pub async fn build(st: &Arc<AppState>, date: &str, team: &str) -> anyhow::Result<DailyReport> {
    // 当日指标
    let today_sql = format!(
        "SELECT person_id, CAST(SUM(wave) AS SIGNED) AS wave, \
         CAST(SUM(minutes) AS SIGNED) AS m, MAX(is_live) AS live \
         FROM daily_metric WHERE DATE_FORMAT(biz_date, '%Y-%m-%d') = '{date}' GROUP BY person_id"
    );
    // 当月未播天数（有数据行中 is_live=0 的天数）
    let month = &date[..7];
    let month_sql = format!(
        "SELECT person_id, CAST(SUM(CASE WHEN is_live = 0 THEN 1 ELSE 0 END) AS SIGNED) AS n \
         FROM daily_metric WHERE DATE_FORMAT(biz_date, '%Y-%m') = '{month}' GROUP BY person_id"
    );
    // 累计音浪（≤ date 的全部增量求和）
    let total_sql = format!(
        "SELECT person_id, CAST(SUM(wave) AS SIGNED) AS total \
         FROM daily_metric WHERE DATE_FORMAT(biz_date, '%Y-%m-%d') <= '{date}' GROUP BY person_id"
    );
    // 名册（含师傅）
    let roster_sql = format!(
        "SELECT p.id, p.name, m.name AS master_name FROM person p \
         LEFT JOIN person m ON m.id = p.master_id \
         WHERE p.deleted_at IS NULL AND p.gender = '{team}'"
    );

    let mut wave_map: HashMap<u64, i64> = Default::default();
    let mut minutes_map: HashMap<u64, i64> = Default::default();
    let mut live_map: HashMap<u64, bool> = Default::default();
    for r in mysql::query(&st.pools, "LOCAL", &today_sql).await? {
        let pid = id_of(&r, "person_id");
        wave_map.insert(pid, signed(&r, "wave"));
        minutes_map.insert(pid, signed(&r, "m"));
        live_map.insert(pid, r.try_get::<i8, _>("live").unwrap_or(0) == 1);
    }
    let mut absent_map: HashMap<u64, i64> = Default::default();
    for r in mysql::query(&st.pools, "LOCAL", &month_sql).await? {
        absent_map.insert(id_of(&r, "person_id"), signed(&r, "n"));
    }
    let mut total_map: HashMap<u64, i64> = Default::default();
    for r in mysql::query(&st.pools, "LOCAL", &total_sql).await? {
        total_map.insert(id_of(&r, "person_id"), signed(&r, "total"));
    }

    let mut rows: Vec<ReportRow> = Vec::new();
    let mut master_groups: HashMap<String, Vec<String>> = Default::default();
    let mut no_master: Vec<String> = Vec::new();
    for r in mysql::query(&st.pools, "LOCAL", &roster_sql).await? {
        let pid = id_of(&r, "id");
        let name: String = r.try_get::<String, _>("name").unwrap_or_default();
        let master: String = r
            .try_get::<Option<String>, _>("master_name")
            .ok()
            .flatten()
            .unwrap_or_default();
        let is_live = live_map.get(&pid).copied().unwrap_or(false);
        let wave = wave_map.get(&pid).copied().unwrap_or(0);
        let total = total_map.get(&pid).copied().unwrap_or(0);
        let absent = absent_map.get(&pid).copied().unwrap_or(0);
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
    Ok(DailyReport {
        date: date.to_string(),
        team: team.to_string(),
        rows,
        master_groups,
        no_master,
    })
}

fn id_of(r: &sqlx::mysql::MySqlRow, col: &str) -> u64 {
    if let Ok(v) = r.try_get::<u64, _>(col) {
        return v;
    }
    if let Ok(v) = r.try_get::<i64, _>(col) {
        return v as u64;
    }
    if let Ok(v) = r.try_get::<String, _>(col) {
        return v.parse().unwrap_or(0);
    }
    0
}

fn signed(r: &sqlx::mysql::MySqlRow, col: &str) -> i64 {
    if let Ok(v) = r.try_get::<i64, _>(col) {
        return v;
    }
    if let Ok(v) = r.try_get::<u64, _>(col) {
        return v as i64;
    }
    if let Ok(v) = r.try_get::<String, _>(col) {
        return v.parse().unwrap_or(0);
    }
    0
}

const FONT: &str = "-apple-system, 'PingFang SC', 'Noto Sans SC', 'Microsoft YaHei', sans-serif";
const MONO: &str = "SF Mono, Menlo, Consolas, monospace";
const WATERMARK: &str = "内部数据 · 请勿外传";

/// 渲染日报 SVG（apple 风模板，720 宽，行高 48）
pub fn render_svg(rep: &DailyReport) -> String {
    let n = rep.rows.len();
    let list_h = n as f64 * 48.0;
    let footer_y = 126.0 + list_h + 42.0;
    let total_h = footer_y + 50.0;
    let max_wave = rep.rows.iter().map(|r| r.wave).max().unwrap_or(1).max(1);
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

    // 水印层
    for x in [-40.0f64, 140.0, 320.0, 500.0, 680.0] {
        for y in 0..((total_h / 76.0).ceil() as i64 + 1) {
            let y = y as f64 * 76.0 - 40.0;
            s.push_str(&format!(
                "<text x=\"{x}\" y=\"{y:.0}\" font-family=\"{FONT}\" font-size=\"18\" font-weight=\"900\" fill=\"#334155\" fill-opacity=\"0.055\" text-anchor=\"middle\" transform=\"rotate(-18 {x} {y:.0})\">{WATERMARK}</text>"
            ));
        }
    }

    // 标题
    s.push_str(&format!(
        "<text x=\"360\" y=\"22\" font-size=\"25\" fill=\"#101828\" font-weight=\"700\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">主播数据日报 · {team_label}</text>"
    ));
    let date_cn = format_cn_date(&rep.date);
    s.push_str(&format!(
        "<text x=\"360\" y=\"50\" font-size=\"13\" fill=\"#475467\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">{date_cn}</text>"
    ));

    // 表头
    s.push_str("<rect y=\"86\" width=\"720\" height=\"34\" fill=\"#F2F4F7\"/>");
    for (x, label) in [
        (32, "排名"),
        (134, "主播姓名"),
        (254, "当月未播天数"),
        (434, "当日音浪"),
        (641, "累计总音浪"),
    ] {
        s.push_str(&format!(
            "<text x=\"{x}\" y=\"104\" font-size=\"12\" fill=\"#667085\" font-weight=\"700\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">{label}</text>"
        ));
    }

    // 数据行
    let bar_full = 242.5f64;
    for (i, r) in rep.rows.iter().enumerate() {
        let y = 126.0 + i as f64 * 48.0;
        let text_y = y + 25.0;
        let rank = i + 1;
        let off_air = !r.is_live;
        let row_bg = if off_air { "#FFF7F7" } else { "#FFFFFF" };
        let row_stroke = if off_air { "#FEE4E2" } else { "#EAECF0" };
        s.push_str(&format!(
            "<rect y=\"{y:.0}\" width=\"720\" height=\"48\" fill=\"{row_bg}\" stroke=\"{row_stroke}\" stroke-width=\"1\"/>"
        ));
        if off_air {
            s.push_str(&format!(
                "<rect y=\"{:.0}\" width=\"4\" height=\"32\" fill=\"#F04438\" rx=\"2\"/>",
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
        s.push_str(&format!(
            "<rect x=\"12.3\" y=\"{:.0}\" width=\"40\" height=\"24\" fill=\"{badge_bg}\" rx=\"12\" stroke=\"{badge_stroke}\"/><text x=\"32.3\" y=\"{text_y:.0}\" font-size=\"12\" fill=\"{badge_fg}\" font-weight=\"700\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{MONO}\">{:02}</text>",
            y + 12.0,
            rank
        ));
        let name_fg = if off_air { "#B42318" } else { "#101828" };
        s.push_str(&format!(
            "<text x=\"72.6\" y=\"{text_y:.0}\" font-size=\"14\" fill=\"{name_fg}\" font-weight=\"700\" text-anchor=\"start\" dominant-baseline=\"central\" font-family=\"{FONT}\">{}</text>",
            esc(&r.name)
        ));
        // 未播天数
        let absent_fg = if off_air { "#B42318" } else { "#027A48" };
        s.push_str(&format!(
            "<text x=\"253.8\" y=\"{text_y:.0}\" font-size=\"13\" fill=\"{absent_fg}\" font-weight=\"700\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">{}</text>",
            r.absent_days
        ));
        // 音浪条形 or 未开播
        if off_air {
            s.push_str(&format!(
                "<rect x=\"312.6\" y=\"{:.0}\" width=\"{bar_full}\" height=\"22\" fill=\"#FEE4E2\" rx=\"11\"/><text x=\"433.8\" y=\"{text_y:.0}\" font-size=\"13\" fill=\"#D92D20\" font-weight=\"700\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">未开播</text>",
                y + 14.0
            ));
        } else {
            let w = bar_full * r.wave as f64 / max_wave as f64;
            s.push_str(&format!(
                "<rect x=\"312.6\" y=\"{:.0}\" width=\"{bar_full}\" height=\"22\" fill=\"#EAF3FF\" rx=\"11\"/>",
                y + 14.0
            ));
            if w > 1.0 {
                s.push_str(&format!(
                    "<rect x=\"312.6\" y=\"{:.0}\" width=\"{w:.1}\" height=\"22\" fill=\"#007AFF\" rx=\"11\"/>",
                    y + 14.0
                ));
            }
            s.push_str(&format!(
                "<text x=\"433.8\" y=\"{text_y:.0}\" font-size=\"13\" fill=\"#FFFFFF\" font-weight=\"700\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{MONO}\">{}</text>",
                esc(&fmt_wan(r.wave))
            ));
        }
        // 累计
        s.push_str(&format!(
            "<text x=\"708\" y=\"{text_y:.0}\" font-size=\"14\" fill=\"#475467\" text-anchor=\"end\" dominant-baseline=\"central\" font-family=\"{MONO}\">{}</text>",
            esc(&fmt_wan(r.total_wave))
        ));
    }

    // footer：数据说明 + 未播统计
    let off_count = rep.rows.iter().filter(|r| !r.is_live).count();
    s.push_str(&format!(
        "<text x=\"8\" y=\"{footer_y:.0}\" font-size=\"12\" fill=\"#667085\" font-weight=\"600\" text-anchor=\"start\" dominant-baseline=\"central\" font-family=\"{FONT}\">数据日期 {} · {} {} 人</text>",
        date_cn,
        team_label,
        rep.rows.len()
    ));
    s.push_str(&format!(
        "<text x=\"712\" y=\"{footer_y:.0}\" font-size=\"12\" fill=\"#667085\" font-weight=\"600\" text-anchor=\"end\" dominant-baseline=\"central\" font-family=\"{FONT}\">未开播 {} 人</text>",
        off_count
    ));
    s.push_str(&format!(
        "<text x=\"360\" y=\"{footer_y:.0}\" font-size=\"12\" fill=\"#98A2B3\" font-weight=\"700\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{FONT}\">{WATERMARK}</text>"
    ));

    // 底部警示条：未播者按师傅分组
    let summary_y = footer_y + 34.0;
    if !rep.master_groups.is_empty() || !rep.no_master.is_empty() {
        let mut parts: Vec<String> = rep
            .master_groups
            .iter()
            .map(|(m, names)| format!("{}: {}", m, names.join("、")))
            .collect();
        if !rep.no_master.is_empty() {
            parts.push(format!("无师傅: {}", rep.no_master.join("、")));
        }
        s.push_str(&format!(
            "<rect y=\"{:.0}\" width=\"720\" height=\"42\" fill=\"#FEF2F2\" rx=\"8\" stroke=\"#FECACA\"/><text x=\"12\" y=\"{summary_y:.0}\" font-size=\"13\" fill=\"#B42318\" text-anchor=\"start\" dominant-baseline=\"central\" font-family=\"{FONT}\">{}</text>",
            footer_y + 8.0,
            esc(&parts.join("  /  "))
        ));
    }

    s.push_str("</svg>");
    s
}

fn format_cn_date(iso: &str) -> String {
    match chrono::NaiveDate::parse_from_str(iso, "%Y-%m-%d") {
        Ok(d) => d.format("%Y年%-m月%-d日").to_string(),
        Err(_) => iso.to_string(),
    }
}

/// 机器人「日报」命令的文本版
pub fn render_text(rep: &DailyReport) -> String {
    let team_label = if rep.team == "male" {
        "男团"
    } else {
        "女团"
    };
    let mut out = format!("{} {} 日报（{}）\n", team_label, rep.date, rep.rows.len());
    for (i, r) in rep.rows.iter().take(15).enumerate() {
        let status = if r.is_live {
            format!("{} / {} 分钟", fmt_wan(r.wave), r.minutes)
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
        return Err(anyhow::anyhow!("还没有采集到任何日期").into());
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

fn valid_date_str(s: &str) -> bool {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}
