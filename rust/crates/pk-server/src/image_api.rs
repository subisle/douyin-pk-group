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
    if !st.auth.verify_admin_password(&key) {
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
    let metric_label = if metric == "wave" {
        "累计音浪"
    } else {
        "累计时长"
    };
    let label = format!(
        "{team_label} · {}月{metric_label}",
        period.get(5..7).unwrap_or("")
    );
    let subtitle = if metric == "wave" {
        "按累计音浪从高到低"
    } else {
        "按累计开播有效时长从高到低"
    };
    let svg = ranking_svg(&rows, metric, &label, subtitle, &period);
    let png = push::svg_to_png_at(&svg, 2.0).map_err(ApiError::from)?;
    let filename = format!(
        "ranking-{period}-{}-{metric}.png",
        if team.is_empty() { "all" } else { team }
    );
    image_response(png, &filename)
}

fn ranking_svg(
    rows: &[RankRow],
    metric: &str,
    label: &str,
    subtitle: &str,
    period: &str,
) -> String {
    let mut ordered = rows.to_vec();
    ordered.sort_by(|a, b| {
        let av = if metric == "wave" { a.wave } else { a.minutes };
        let bv = if metric == "wave" { b.wave } else { b.minutes };
        bv.cmp(&av).then_with(|| a.name.cmp(&b.name))
    });
    let columns = if ordered.len() <= 20 { 1 } else { 2 };
    let per_col = ordered.len().div_ceil(columns);
    let width = 1800.0;
    let margin = 66.0;
    let gap = 54.0;
    let col_width = (width - margin * 2.0 - gap * (columns as f64 - 1.0)) / columns as f64;
    let header_y = 205.0;
    let row_start = 260.0;
    let row_height = 50.0;
    let height = row_start + per_col as f64 * row_height + 95.0;
    let max_value = ordered
        .iter()
        .map(|r| if metric == "wave" { r.wave } else { r.minutes })
        .max()
        .unwrap_or(0)
        .max(1);
    let title = format!("{label}排名");
    let font = "Noto Sans CJK SC, PingFang SC, Microsoft YaHei, sans-serif";
    let mut svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">
<rect width="100%" height="100%" fill="#ffffff"/>
<text x="{margin}" y="62" font-family="{font}" font-size="15" font-weight="700" letter-spacing="2" fill="#667085">MONTHLY RANKING · {period}</text>
<text x="{margin}" y="124" font-family="{font}" font-size="58" font-weight="800" fill="#111827">{}</text>
<text x="{margin}" y="160" font-family="{font}" font-size="18" fill="#667085">{subtitle} · {period}</text>
<line x1="{margin}" y1="194" x2="{}" y2="194" stroke="#111827" stroke-width="4"/>
"##,
        lineage_report::esc(&title),
        width - margin
    );
    let mut prev: Option<i64> = None;
    let mut rank = 0usize;
    for (i, row) in ordered.iter().enumerate() {
        let col = if columns == 1 { 0 } else { i / per_col };
        let idx = if columns == 1 { i } else { i % per_col };
        if col >= columns || idx >= per_col {
            continue;
        }
        let value = if metric == "wave" {
            row.wave
        } else {
            row.minutes
        };
        if prev != Some(value) {
            rank = i + 1;
            prev = Some(value);
        }
        let x = margin + col as f64 * (col_width + gap);
        let y = row_start + idx as f64 * row_height;
        if idx == 0 {
            svg.push_str(&format!(
                r##"<text x="{x}" y="{header_y}" font-family="{font}" font-size="14" fill="#8a94a6">名次</text>
<text x="{}" y="{header_y}" font-family="{font}" font-size="14" fill="#8a94a6">主播</text>
<text x="{}" y="{header_y}" font-family="{font}" font-size="14" fill="#8a94a6">占比</text>
<text x="{}" y="{header_y}" text-anchor="end" font-family="{font}" font-size="14" fill="#8a94a6">{}</text>
<line x1="{x}" y1="{header_y}" x2="{}" y2="{header_y}" stroke="#e5e9f0"/>
"##,
                x + 62.0,
                x + 210.0,
                x + col_width - 10.0,
                if metric == "wave" { "累计音浪" } else { "累计时长" },
                x + col_width
            ));
        }
        let name = truncate_chars(&row.name, 16);
        let value_text = if metric == "wave" {
            fmt_wave(value)
        } else {
            fmt_minutes(value)
        };
        let text_color = if value <= 0 { "#aab2bf" } else { "#111827" };
        let name_color = if value <= 0 { "#aab2bf" } else { "#111827" };
        let bar_total = col_width - 330.0;
        let bar_fill = if value <= 0 {
            0.0
        } else {
            (value as f64 / max_value as f64 * bar_total).max(2.0)
        };
        svg.push_str(&format!(
            r##"<line x1="{x}" y1="{}" x2="{}" y2="{}" stroke="#edf0f4"/>
<text x="{x}" y="{}" font-family="{font}" font-size="16" font-weight="600" fill="#7b8798">{rank}</text>
<text x="{}" y="{}" font-family="{font}" font-size="18" font-weight="700" fill="{name_color}">{}</text>
<rect x="{}" y="{}" width="{bar_total}" height="13" rx="6.5" fill="#edf1f5"/>
<rect x="{}" y="{}" width="{bar_fill}" height="13" rx="6.5" fill="#111827"/>
<text x="{}" y="{}" text-anchor="end" font-family="{font}" font-size="20" font-weight="700" fill="{text_color}">{value_text}</text>
"##,
            y + row_height - 1.0,
            x + col_width,
            y + row_height - 1.0,
            y + 31.0,
            x + 62.0,
            y + 31.0,
            lineage_report::esc(&name),
            x + 210.0,
            y + 22.0,
            x + 210.0,
            y + 22.0,
            x + col_width - 10.0,
            y + 31.0,
        ));
    }
    svg.push_str(&format!(r##"<line x1="{margin}" y1="{}" x2="{}" y2="{}" stroke="#111827" stroke-width="2"/><text x="{margin}" y="{}" font-family="{font}" font-size="13" fill="#98a2b3">{label} · {period}</text></svg>"##, height - 64.0, width - margin, height - 64.0, height - 30.0));
    svg
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
            ],
            "wave",
            "全团 · 10月累计音浪",
            "按累计音浪从高到低",
            "2026-10",
        );
        assert!(svg.contains("MONTHLY RANKING · 2026-10"));
        assert!(svg.contains("全团 · 10月累计音浪排名"));
        assert!(
            svg.find(">甲</text>").unwrap_or(usize::MAX)
                < svg.find(">乙</text>").unwrap_or(usize::MAX)
        );
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
