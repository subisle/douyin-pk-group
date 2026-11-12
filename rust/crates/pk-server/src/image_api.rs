//! 图片生成 API（2026-11-10）
//!
//! 独立于后台会话的图片生成入口。调用方在请求头传管理员口令：
//! `X-API-Key: <ADMIN_PASSWORD>`，也兼容 `Authorization: Bearer <ADMIN_PASSWORD>`。
//! 口令只进请求头，不进 URL；始终校验运行时管理员口令（未配置时按项目规则使用默认口令）。
//!
//! 两个入口：
//! - `/api/images/ranking.png`：月榜音浪 / 时长长条图
//! - `/api/images/lineage.png`：师傅师门图（复用 wxbot 师门日报渲染链路）

use axum::{
    extract::{Query, State},
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use sqlx::Row;
use std::sync::Arc;

use crate::{dialect, lineage_report, pgrs, push, routes::ApiError, state::AppState};

const API_PREFIX: &str = "/api/images";

/// 月榜导出图水印（2026-11-13 用户口径：导出图水印为「鹏先生」）。
/// 只出现在图片接口产出的 PNG 里，不影响后台页面预览。
const RANKING_WATERMARK: &str = "鹏先生";

/// 图片 API Key 与管理员口令共用 Argon2 校验，属于 CPU 密集操作。
/// 限制同时校验数，避免错误 Key 并发请求把 4 核机器打满。
static KEY_VERIFY_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

/// 返回仍待注入 `AppState` 的子路由，合并到主路由后由主入口统一 `with_state`。
/// 鉴权中间件自己捕获同一份状态，因此图片路由不会走后台 session 鉴权。
pub fn router(st: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route(&format!("{API_PREFIX}/health"), get(health))
        .route(&format!("{API_PREFIX}/ranking.png"), get(ranking_png))
        .route(&format!("{API_PREFIX}/lineage.png"), get(lineage_png))
        .layer(axum::middleware::from_fn_with_state(st, require_admin_key))
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ok": true,
        "auth": "admin-password",
        "endpoints": ["/api/images/ranking.png", "/api/images/lineage.png"],
    }))
}

async fn require_admin_key(
    State(st): State<Arc<AppState>>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    let Some(key) = api_key(req.headers()) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "缺少 X-API-Key" })),
        )
            .into_response();
    };
    let permit = match KEY_VERIFY_SLOTS.try_acquire() {
        Ok(permit) => permit,
        Err(_) => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({ "error": "鉴权校验繁忙，请稍后重试" })),
            )
                .into_response();
        }
    };
    let auth = st.auth.clone();
    let verified = tokio::task::spawn_blocking(move || auth.verify_admin_password(&key))
        .await
        .unwrap_or(false);
    drop(permit);
    if !verified {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "API Key 无效" })),
        )
            .into_response();
    }
    next.run(req).await
}

fn api_key(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("x-api-key")
        .or_else(|| headers.get("x-image-key"))
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| bearer_token(headers))
}

fn bearer_token(headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?.trim();
    let token = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
        .unwrap_or(raw)
        .trim();
    (!token.is_empty()).then(|| token.to_string())
}

#[derive(Debug, Deserialize)]
struct RankingQuery {
    period: Option<String>,
    team: Option<String>,
    metric: Option<String>,
}

#[derive(Debug, Clone)]
struct RankRow {
    name: String,
    wave: i64,
    minutes: i64,
}

async fn ranking_png(
    State(st): State<Arc<AppState>>,
    Query(q): Query<RankingQuery>,
) -> Result<Response, ApiError> {
    let period = q
        .period
        .unwrap_or_else(|| chrono::Local::now().format("%Y-%m").to_string());
    if period.len() != 7 || !dialect::valid_period_shape(&period) {
        return Err(ApiError::bad_request("period 必须是 YYYY-MM"));
    }
    let team = match q.team.as_deref().unwrap_or("all").trim() {
        "" | "all" => "",
        "male" => "male",
        "female" => "female",
        _ => return Err(ApiError::bad_request("team 只能是 male、female 或 all")),
    };
    let metric = match q.metric.as_deref().unwrap_or("wave").trim() {
        "wave" | "音浪" => "wave",
        "minutes" | "duration" | "时长" => "minutes",
        _ => return Err(ApiError::bad_request("metric 只能是 wave 或 minutes")),
    };

    // 与 /api/admin/metrics/monthly 完全同一份月榜 SQL 口径：整月导入优先，
    // 缺月回落到日明细，多抖音号已按人合并。
    let db_rows = dialect::query_period(
        &st.pools,
        "LOCAL",
        dialect::ADMIN_PERIOD_SELECT,
        &format!("{period}%"),
        team,
        "",
        "wave DESC",
    )
    .await?;
    let rows: Vec<RankRow> = db_rows
        .iter()
        .map(|r| {
            Ok(RankRow {
                name: r.try_get::<String, _>("name")?,
                wave: r.try_get::<i64, _>("wave")?,
                minutes: r.try_get::<i64, _>("minutes")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
    if rows.is_empty() {
        return Err(ApiError::bad_request("这个周期没有榜单数据"));
    }

    let team_label = match team {
        "male" => "男团",
        "female" => "女团",
        _ => "全团",
    };
    let svg = ranking_svg(&rows, metric, team_label, &period);
    // resvg 是同步 CPU 密集任务，不能直接占住 Tokio worker；整月长榜
    // 单张可达数十 MB RGBA，放 blocking 池可避免拖慢健康检查与其它请求。
    // 85 行左右的长图按 1.5 倍渲染（2250px 宽），清晰度够用且体积减半。
    let scale = if rows.len() > 45 { 1.5 } else { 2.0 };
    let png = tokio::task::spawn_blocking(move || push::svg_to_png_at(&svg, scale))
        .await
        .map_err(|e| ApiError::from(anyhow::anyhow!("图片渲染任务失败: {e}")))?
        .map_err(ApiError::from)?;
    let filename = format!(
        "ranking-{period}-{}-{metric}.png",
        if team.is_empty() { "all" } else { team }
    );
    image_response(png, &filename)
}

/// 月榜长图（2026-11-13 改版）：单列、奖牌名次、青色圆角条、底部统计条。
/// 版式对齐用户提供的「鹏仔传媒主播时长统计」参考图。
fn ranking_svg(rows: &[RankRow], metric: &str, team_label: &str, period: &str) -> String {
    let mut ordered = rows.to_vec();
    ordered.sort_by(|a, b| {
        let av = if metric == "wave" { a.wave } else { a.minutes };
        let bv = if metric == "wave" { b.wave } else { b.minutes };
        bv.cmp(&av).then_with(|| a.name.cmp(&b.name))
    });

    let is_wave = metric == "wave";
    let (metric_word, bar_header, value_header) = if is_wave {
        ("音浪", "音浪条", "月音浪")
    } else {
        ("时长", "时长条", "月时长")
    };
    let period_label = format!(
        "{}年{}月",
        period.get(0..4).unwrap_or(""),
        period.get(5..7).unwrap_or("").trim_start_matches('0')
    );
    let title = format!("鹏仔传媒主播{metric_word}统计 {period_label}");
    let subtitle = format!(
        "{team_label} · 共 {} 人 · 按累计{}从高到低",
        ordered.len(),
        if is_wave { "音浪" } else { "有效时长" }
    );

    let width = 1500.0;
    let margin = 60.0;
    let header_y = 216.0;
    let row_start = 234.0;
    let row_height = 54.0;
    let footer_y = row_start + ordered.len() as f64 * row_height + 36.0;
    let height = footer_y + 108.0;
    let rank_x = margin + 16.0;
    let name_x = margin + 92.0;
    let bar_x = margin + 340.0;
    let value_x = width - margin;
    let bar_w = value_x - bar_x - 168.0;
    let max_value = ordered
        .iter()
        .map(|r| if is_wave { r.wave } else { r.minutes })
        .max()
        .unwrap_or(0)
        .max(1);
    let font = "PingFang SC, Noto Sans CJK SC, Microsoft YaHei, sans-serif";

    let mut svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">
<defs>
<linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
<stop offset="0" stop-color="#E7F3FF"/>
<stop offset="0.45" stop-color="#F7FBFF"/>
<stop offset="1" stop-color="#FFFFFF"/>
</linearGradient>
<linearGradient id="bar" x1="0" y1="0" x2="1" y2="0">
<stop offset="0" stop-color="#8AD9FA"/>
<stop offset="1" stop-color="#1E9EE6"/>
</linearGradient>
<linearGradient id="medal-1" x1="0" y1="0" x2="0" y2="1">
<stop offset="0" stop-color="#FFE9A8"/>
<stop offset="1" stop-color="#F0A81C"/>
</linearGradient>
<linearGradient id="medal-2" x1="0" y1="0" x2="0" y2="1">
<stop offset="0" stop-color="#F5F9FD"/>
<stop offset="1" stop-color="#B4C2D2"/>
</linearGradient>
<linearGradient id="medal-3" x1="0" y1="0" x2="0" y2="1">
<stop offset="0" stop-color="#FFE2C4"/>
<stop offset="1" stop-color="#CE854A"/>
</linearGradient>
</defs>
<rect width="100%" height="100%" fill="url(#bg)"/>
<text x="{}" y="96" text-anchor="middle" font-family="{font}" font-size="50" font-weight="800" fill="#0F3B63">{}</text>
<text x="{}" y="142" text-anchor="middle" font-family="{font}" font-size="19" fill="#6F8AA3">{subtitle}</text>
<text x="{rank_x}" y="{header_y}" font-family="{font}" font-size="17" font-weight="700" fill="#8FA3B8">序号</text>
<text x="{name_x}" y="{header_y}" font-family="{font}" font-size="17" font-weight="700" fill="#8FA3B8">主播姓名</text>
<text x="{bar_x}" y="{header_y}" font-family="{font}" font-size="17" font-weight="700" fill="#8FA3B8">{bar_header}</text>
<text x="{value_x}" y="{header_y}" text-anchor="end" font-family="{font}" font-size="17" font-weight="700" fill="#8FA3B8">{value_header}</text>
<line x1="{margin}" y1="{}" x2="{}" y2="{}" stroke="#D5E6F5" stroke-width="2"/>
"##,
        width / 2.0,
        lineage_report::esc(&title),
        width / 2.0,
        header_y + 14.0,
        width - margin,
        header_y + 14.0
    );

    // 斑马纹先整段铺底：前三名奖牌彩带会略微越出本行，若随行绘制会被
    // 下一行的底色盖住。
    for i in (1..ordered.len()).step_by(2) {
        let y = row_start + i as f64 * row_height;
        svg.push_str(&format!(
            r##"<rect x="{}" y="{}" width="{}" height="{}" rx="12" fill="#EFF7FF" opacity="0.85"/>
"##,
            margin - 20.0,
            y + 2.0,
            width - (margin - 20.0) * 2.0,
            row_height - 4.0
        ));
    }
    let mut prev: Option<i64> = None;
    let mut rank = 0usize;
    for (i, row) in ordered.iter().enumerate() {
        let value = if is_wave { row.wave } else { row.minutes };
        if prev != Some(value) {
            rank = i + 1;
            prev = Some(value);
        }
        let y = row_start + i as f64 * row_height;
        let center = y + row_height / 2.0;
        if rank <= 3 && value > 0 {
            svg.push_str(&medal_svg(rank_x + 17.0, center, rank, font));
        } else {
            svg.push_str(&format!(
                r##"<text x="{}" y="{}" font-family="{font}" font-size="20" font-weight="700" fill="#9FB2C4">{:02}</text>
"##,
                rank_x,
                center + 7.0,
                rank
            ));
        }
        let name = truncate_chars(&row.name, 10);
        svg.push_str(&format!(
            r##"<text x="{name_x}" y="{}" font-family="{font}" font-size="24" font-weight="700" fill="{}">{}</text>
"##,
            center + 8.0,
            if value > 0 { "#13395C" } else { "#A9B8C6" },
            lineage_report::esc(&name)
        ));

        if value > 0 {
            let fill = (value as f64 / max_value as f64 * bar_w).max(10.0);
            let value_text = if is_wave {
                fmt_wave(value)
            } else {
                fmt_minutes(value)
            };
            svg.push_str(&format!(
                r##"<rect x="{bar_x}" y="{}" width="{bar_w}" height="20" rx="10" fill="#E1F0FB"/>
<rect x="{bar_x}" y="{}" width="{fill}" height="20" rx="10" fill="url(#bar)"/>
<text x="{value_x}" y="{}" text-anchor="end" font-family="{font}" font-size="22" font-weight="800" fill="#12699C">{value_text}</text>
"##,
                center - 10.0,
                center - 10.0,
                center + 8.0
            ));
        } else {
            svg.push_str(&format!(
                r##"<text x="{value_x}" y="{}" text-anchor="end" font-family="{font}" font-size="22" font-weight="800" fill="#E34B4B">未开播</text>
"##,
                center + 8.0
            ));
        }
    }

    let zero_names: Vec<&str> = ordered
        .iter()
        .filter(|r| (if is_wave { r.wave } else { r.minutes }) <= 0)
        .map(|r| r.name.as_str())
        .collect();
    let footer_people = if team_label == "全团" {
        format!("全部主播 {} 人", ordered.len())
    } else {
        format!("{team_label}主播 {} 人", ordered.len())
    };
    svg.push_str(&format!(
        r##"<line x1="{margin}" y1="{footer_y}" x2="{}" y2="{footer_y}" stroke="#D5E6F5" stroke-width="2"/>
<text x="{margin}" y="{}" font-family="{font}" font-size="22" font-weight="800" fill="#1B6F9E">{footer_people}</text>
<text x="{margin}" y="{}" font-family="{font}" font-size="16" fill="#93A3B5">导出月份 {period_label}</text>
"##,
        width - margin,
        footer_y + 44.0,
        footer_y + 76.0
    ));
    if zero_names.is_empty() {
        svg.push_str(&format!(
            r##"<text x="{value_x}" y="{}" text-anchor="end" font-family="{font}" font-size="20" font-weight="800" fill="#1F9D6B">全员开播</text>
"##,
            footer_y + 44.0
        ));
    } else {
        let names = truncate_chars(&zero_names.join("、"), 40);
        svg.push_str(&format!(
            r##"<text x="{value_x}" y="{}" text-anchor="end" font-family="{font}" font-size="20" font-weight="800" fill="#E34B4B">未开播 {} 人</text>
<text x="{value_x}" y="{}" text-anchor="end" font-family="{font}" font-size="16" fill="#E34B4B">{}</text>
"##,
            footer_y + 44.0,
            zero_names.len(),
            footer_y + 76.0,
            lineage_report::esc(&names)
        ));
    }
    // 水印压在最顶层：斜排平铺、低透明度，不影响读数。
    svg.push_str(&watermark_svg(width, height, font, RANKING_WATERMARK));
    svg.push_str("</svg>");
    svg
}

/// 斜排平铺水印。与师门图的「内部数据 · 请勿外传」不同，月榜导出图只标
/// 出品人（鹏先生），既能防无源转发，又不会让榜单看起来像内部密件。
fn watermark_svg(width: f64, height: f64, font: &str, text: &str) -> String {
    let mark = lineage_report::esc(text);
    let step_x = 320.0;
    let step_y = 190.0;
    let cols = (width / step_x).ceil() as i64 + 2;
    let rows = (height / step_y).ceil() as i64 + 1;
    let mut out = String::new();
    for yi in 0..rows {
        for xi in 0..cols {
            let wx = xi as f64 * step_x + if yi % 2 == 1 { step_x / 2.0 } else { 0.0 };
            let wy = yi as f64 * step_y + 40.0;
            out.push_str(&format!(
                r##"<text x="{wx:.0}" y="{wy:.0}" font-family="{font}" font-size="24" font-weight="700" fill="#0F3B63" fill-opacity="0.045" text-anchor="middle" transform="rotate(-24 {wx:.0} {wy:.0})">{mark}</text>
"##
            ));
        }
    }
    out
}

/// 前三名奖牌：彩带 + 圆形徽章 + 名次数字（不用 emoji，避免字体缺字）。
fn medal_svg(cx: f64, cy: f64, rank: usize, font: &str) -> String {
    let dark = match rank {
        1 => "#B97D00",
        2 => "#7E8FA3",
        _ => "#8C5326",
    };
    format!(
        r##"<g>
<path d="M{},{} L{},{} L{},{} Z" fill="{dark}" opacity="0.85"/>
<path d="M{},{} L{},{} L{},{} Z" fill="{dark}" opacity="0.85"/>
<circle cx="{cx}" cy="{cy}" r="18" fill="url(#medal-{rank})" stroke="{dark}" stroke-width="1.5"/>
<text x="{cx}" y="{}" text-anchor="middle" font-family="{font}" font-size="20" font-weight="800" fill="#FFFFFF">{rank}</text>
</g>
"##,
        cx - 15.0,
        cy + 6.0,
        cx - 24.0,
        cy + 33.0,
        cx - 7.0,
        cy + 27.0,
        cx + 15.0,
        cy + 6.0,
        cx + 24.0,
        cy + 33.0,
        cx + 7.0,
        cy + 27.0,
        cy + 7.0
    )
}

fn fmt_wave(value: i64) -> String {
    if value >= 10_000 {
        let v = value as f64 / 10_000.0;
        let s = format!("{v:.1}");
        format!("{}万", s.trim_end_matches('0').trim_end_matches('.'))
    } else {
        value.to_string()
    }
}

fn fmt_minutes(value: i64) -> String {
    if value <= 0 {
        return "未开播".into();
    }
    let h = value / 60;
    let m = value % 60;
    format!("{h}时{m:02}分")
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

fn valid_month(value: &str) -> bool {
    value.len() == 7 && dialect::valid_period_shape(value)
}

fn valid_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[8..].iter().all(u8::is_ascii_digit)
        && chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
}

#[derive(Debug, Deserialize)]
struct LineageQuery {
    master: Option<String>,
    date: Option<String>,
    month: Option<String>,
    from: Option<String>,
    to: Option<String>,
    page: Option<usize>,
    qualified_minutes: Option<i64>,
    severe_minutes: Option<i64>,
}

async fn lineage_png(
    State(st): State<Arc<AppState>>,
    Query(q): Query<LineageQuery>,
) -> Result<Response, ApiError> {
    let master = q
        .master
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| ApiError::bad_request("master 不能为空"))?;
    let month = non_empty(q.month.as_deref());
    let date = non_empty(q.date.as_deref());
    let from = non_empty(q.from.as_deref());
    let to = non_empty(q.to.as_deref());
    if from.is_some() != to.is_some() {
        return Err(ApiError::bad_request("from 和 to 必须同时提供"));
    }
    let mode_count =
        usize::from(month.is_some()) + usize::from(date.is_some()) + usize::from(from.is_some());
    if mode_count > 1 {
        return Err(ApiError::bad_request("month、from/to、date 不能混用"));
    }
    if month.is_some_and(|value| !valid_month(value)) {
        return Err(ApiError::bad_request("month 必须是 YYYY-MM"));
    }
    if date.is_some_and(|value| !valid_date(value)) {
        return Err(ApiError::bad_request("date 必须是 YYYY-MM-DD"));
    }
    if from.is_some_and(|value| !valid_date(value)) || to.is_some_and(|value| !valid_date(value)) {
        return Err(ApiError::bad_request("from / to 必须是 YYYY-MM-DD"));
    }
    if let (Some(from), Some(to)) = (from, to) {
        if from > to {
            return Err(ApiError::bad_request("开始日期不能晚于结束日期"));
        }
    }
    let master_row = pgrs::query_with_binds(
        &st.pools,
        "LOCAL",
        "SELECT id, name FROM person WHERE name = ? AND deleted_at IS NULL AND status <> 'left' LIMIT 1",
        |query| query.bind(master.clone()),
    )
    .await?
    .into_iter()
    .next()
    .ok_or_else(|| ApiError::bad_request("师傅不存在或已离开"))?;
    let master_id = master_row.try_get::<i64, _>("id")?.max(0) as u64;
    let master_name = master_row
        .try_get::<String, _>("name")
        .unwrap_or_else(|_| master.to_string());

    let report = if let Some(month) = month {
        lineage_report::build_month(&st, month, master_id).await?
    } else if let (Some(from), Some(to)) = (from, to) {
        lineage_report::build_range(
            &st,
            from,
            to,
            master_id,
            q.qualified_minutes.unwrap_or(360).max(0),
            q.severe_minutes.unwrap_or(240).max(0),
        )
        .await?
    } else {
        let date = match date {
            Some(date) => date.to_string(),
            None => latest_biz_date(&st).await?,
        };
        lineage_report::build(&st, &date, master_id).await?
    };

    let pages = push::lineage_report_pngs_for(&st, &report, &master_name).await;
    if pages.is_empty() {
        return Err(ApiError::with_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            "师门图片生成失败",
        ));
    }
    let total_pages = pages.len();
    let page = q.page.unwrap_or(0).min(total_pages.saturating_sub(1));
    let png = pages.into_iter().nth(page).unwrap_or_default();
    // Content-Disposition 只能放 ASCII；中文师傅名保留在图片内容与响应元数据之外。
    let filename = format!("lineage-{}-page-{}.png", report.date, page + 1);
    let mut response = image_response(png, &filename)?;
    response.headers_mut().insert(
        "x-lineage-page",
        HeaderValue::from_str(&(page + 1).to_string()).unwrap_or(HeaderValue::from_static("1")),
    );
    response.headers_mut().insert(
        "x-lineage-pages",
        HeaderValue::from_str(&total_pages.to_string()).unwrap_or(HeaderValue::from_static("1")),
    );
    Ok(response)
}

async fn latest_biz_date(st: &Arc<AppState>) -> Result<String, ApiError> {
    let rows = pgrs::query(
        &st.pools,
        "LOCAL",
        "SELECT TO_CHAR(MAX(biz_date), 'YYYY-MM-DD') AS d FROM daily_metric",
    )
    .await?;
    let date = rows
        .first()
        .and_then(|r| r.try_get::<Option<String>, _>("d").ok().flatten())
        .unwrap_or_default();
    if date.is_empty() {
        return Err(ApiError::bad_request("没有可用业务日期"));
    }
    Ok(date)
}

fn image_response(png: Vec<u8>, filename: &str) -> Result<Response, ApiError> {
    let mut response = png.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    let disposition = HeaderValue::from_str(&format!("inline; filename=\"{filename}\""))
        .map_err(|_| ApiError::bad_request("图片文件名不合法"))?;
    response
        .headers_mut()
        .insert(header::CONTENT_DISPOSITION, disposition);
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    #[test]
    fn wave_and_duration_format() {
        assert_eq!(fmt_wave(25_880_170), "2588万");
        assert_eq!(fmt_minutes(11_821), "197时01分");
        assert_eq!(fmt_minutes(0), "未开播");
    }

    #[test]
    fn validates_month_and_date_shapes() {
        assert!(valid_month("2026-10"));
        assert!(!valid_month("2026-13"));
        assert!(!valid_month("2026"));
        assert!(valid_date("2026-11-09"));
        assert!(!valid_date("2026-11-9"));
        assert!(!valid_date("2026-02-30"));
    }

    #[test]
    fn ranking_svg_uses_requested_period_and_metric_order() {
        let svg = ranking_svg(
            &[
                RankRow {
                    name: "甲".into(),
                    wave: 100,
                    minutes: 60,
                },
                RankRow {
                    name: "乙".into(),
                    wave: 50,
                    minutes: 30,
                },
                RankRow {
                    name: "丙".into(),
                    wave: 0,
                    minutes: 0,
                },
            ],
            "wave",
            "全团",
            "2026-10",
        );
        assert!(svg.contains("鹏仔传媒主播音浪统计 2026年10月"));
        assert!(svg.contains("全部主播 3 人"));
        assert!(svg.contains("未开播 1 人"));
        assert!(svg.contains("丙"));
        assert!(svg.contains(">01<") || svg.contains("medal-1"));
        assert!(
            svg.find(">甲</text>").unwrap_or(usize::MAX)
                < svg.find(">乙</text>").unwrap_or(usize::MAX)
        );
        assert!(!svg.contains("MONTHLY RANKING"));
        // 导出图水印：鹏先生（平铺多次）
        assert!(svg.matches(">鹏先生</text>").count() > 1);
    }

    #[test]
    fn watermark_tiles_and_escapes() {
        let svg = watermark_svg(600.0, 300.0, "sans-serif", "鹏先生");
        assert!(svg.matches(">鹏先生</text>").count() >= 2);
        let escaped = watermark_svg(600.0, 300.0, "sans-serif", "A<B");
        assert!(escaped.contains("A&lt;B"));
        assert!(!escaped.contains("A<B"));
    }

    #[test]
    fn ranking_svg_switches_headers_by_metric() {
        let rows = [RankRow {
            name: "甲".into(),
            wave: 10,
            minutes: 90,
        }];
        let duration = ranking_svg(&rows, "minutes", "女团", "2026-06");
        assert!(duration.contains("鹏仔传媒主播时长统计 2026年6月"));
        assert!(duration.contains("时长条"));
        assert!(duration.contains("月时长"));
        assert!(duration.contains("1时30分"));

        let wave = ranking_svg(&rows, "wave", "男团", "2026-06");
        assert!(wave.contains("音浪条"));
        assert!(wave.contains("月音浪"));
        assert!(wave.contains("男团主播 1 人"));
    }

    #[test]
    fn api_key_accepts_x_api_key_and_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("  key-one  "));
        assert_eq!(api_key(&headers).as_deref(), Some("key-one"));

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer key-two"),
        );
        assert_eq!(api_key(&headers).as_deref(), Some("key-two"));

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("bearer key-three"),
        );
        assert_eq!(api_key(&headers).as_deref(), Some("key-three"));
    }

    #[test]
    fn api_key_rejects_missing_or_blank_x_api_key() {
        assert_eq!(api_key(&HeaderMap::new()), None);

        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("   "));
        assert_eq!(api_key(&headers), None);
    }

    #[test]
    fn image_response_sets_png_and_ascii_filename() {
        let Ok(response) =
            image_response(vec![0x89, b'P', b'N', b'G'], "lineage-2026-10-page-1.png")
        else {
            panic!("response should be valid");
        };
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/png"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_DISPOSITION).unwrap(),
            "inline; filename=\"lineage-2026-10-page-1.png\""
        );
    }
}
