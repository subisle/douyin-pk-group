//! HTTP 路由：与旧版 Fastify 服务保持完全一致的路径与响应结构，
//! 这样前端 api.ts 无需改动即可切到 Rust 后端。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::matching::{match_roster, MatchOptions};
use crate::mysql;
use crate::repos;
use crate::sqlite;
use crate::state::AppState;

type ApiResult = Result<Json<serde_json::Value>, ApiError>;

pub struct ApiError(anyhow::Error);

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self(e)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        Self(e.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let msg = self.0.to_string();
        let bad_request = msg.contains("缺少")
            || msg.contains("未知数据源")
            || msg.contains("只读连接拒绝")
            || msg.contains("名单为空");
        let status = if bad_request {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(error = %msg, "请求处理失败");
        }
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

pub async fn health(State(st): State<std::sync::Arc<AppState>>) -> ApiResult {
    let db = mysql::health_check(&st.pools, &st.active_id()).await;
    Ok(Json(json!({ "server": "ok", "db": db })))
}

pub async fn sources(State(st): State<std::sync::Arc<AppState>>) -> ApiResult {
    let active = st.active_id();
    let label = st.source(Some(&active))?.label.clone();
    Ok(Json(json!({
        "active": active,
        "activeLabel": label,
        "items": st.sources.iter().map(|s| s.public()).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
pub struct SetActiveBody {
    pub id: Option<String>,
}

pub async fn set_active(
    State(st): State<std::sync::Arc<AppState>>,
    Json(body): Json<SetActiveBody>,
) -> ApiResult {
    let id = body.id.ok_or_else(|| anyhow::anyhow!("缺少数据源 id"))?;
    st.set_active(&id)?;
    Ok(Json(json!({ "active": st.active_id() })))
}

pub async fn test_source(
    State(st): State<std::sync::Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult {
    let db = mysql::health_check(&st.pools, &id).await;
    let mut latest = None;
    let mut anchor_count = 0;
    if db.ok {
        latest = repos::get_available_dates(&st, Some(&id), true)
            .await?
            .first()
            .cloned();
        anchor_count = repos::load_anchors(&st, Some(&id), true).await?.len();
    }
    Ok(Json(
        json!({ "id": id, "ok": db.ok, "version": db.version, "error": db.error, "latestDate": latest, "anchorCount": anchor_count }),
    ))
}

pub async fn meta(State(st): State<std::sync::Arc<AppState>>) -> ApiResult {
    let active = st.active_id();
    let dates = repos::get_available_dates(&st, None, false).await?;
    let baseline = repos::get_baseline_date(&st, None, false).await?;
    let anchors = repos::load_anchors(&st, None, false).await?;
    Ok(Json(json!({
        "dates": dates,
        "latestDate": dates.first(),
        "baselineDate": baseline,
        "anchorCount": anchors.len(),
        "aliases": sqlite::list_aliases(),
        "sourceId": active,
        "sourceLabel": st.source(None)?.label,
    })))
}

#[derive(Deserialize)]
pub struct ParseBody {
    pub text: Option<String>,
    pub metric: Option<String>,
    pub days: Option<i64>,
    pub aliases: Option<std::collections::HashMap<String, String>>,
    pub exclude: Option<Vec<String>>,
    pub force: Option<bool>,
}

pub async fn roster_parse(
    State(st): State<std::sync::Arc<AppState>>,
    Json(body): Json<ParseBody>,
) -> ApiResult {
    let text = body
        .text
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("缺少名单文本"))?;
    let payload = match_roster(
        &st,
        MatchOptions {
            text: &text,
            metric: body.metric.as_deref().unwrap_or("lastN"),
            days: body.days.unwrap_or(3),
            aliases: body.aliases,
            exclude: body.exclude,
            force: body.force.unwrap_or(false),
        },
    )
    .await?;
    Ok(Json(serde_json::to_value(payload)?))
}

/// 当前名单。前端 zustand 的 `text` 初始值是 DEFAULT_ROSTER，改完就自动存这里。
/// 从没保存过时返回 `saved: false` + `text: null`，前端据此保留 DEFAULT_ROSTER。
/// 这里不能返回空串：空串会被前端理解成「名单被清空了」。
pub async fn get_roster() -> ApiResult {
    match sqlite::load_roster() {
        Some((text, updated_at)) => Ok(Json(
            json!({ "saved": true, "text": text, "updatedAt": updated_at }),
        )),
        None => Ok(Json(
            json!({ "saved": false, "text": null, "updatedAt": null }),
        )),
    }
}

#[derive(Deserialize)]
pub struct RosterBody {
    pub text: Option<String>,
}

/// 覆盖保存名单。后端也拒收空名单：前端只是防手滑，真正的兜底要落在这一层，
/// 否则任何一次空写入都会把用户攒了很久的名单永久盖掉。
pub async fn put_roster(Json(body): Json<RosterBody>) -> ApiResult {
    let text = body
        .text
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("名单为空，不保存"))?;
    let updated_at = sqlite::save_roster(&text);
    Ok(Json(json!({ "ok": true, "updatedAt": updated_at })))
}

#[derive(Deserialize)]
pub struct AnchorQuery {
    pub q: Option<String>,
}

pub async fn anchors(
    State(st): State<std::sync::Arc<AppState>>,
    Query(q): Query<AnchorQuery>,
) -> ApiResult {
    let items = repos::search_anchors(&st, q.q.as_deref().unwrap_or(""), 30, None).await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub struct WavesBody {
    pub metric: Option<String>,
    pub days: Option<i64>,
    pub person_ids: Option<Vec<String>>,
    pub force: Option<bool>,
}

pub async fn waves(
    State(st): State<std::sync::Arc<AppState>>,
    Json(body): Json<WavesBody>,
) -> ApiResult {
    let info = repos::get_waves(
        &st,
        body.metric.as_deref().unwrap_or("lastN"),
        body.days.unwrap_or(3),
        body.person_ids.as_deref(),
        body.force.unwrap_or(false),
        None,
    )
    .await?;
    Ok(Json(serde_json::to_value(info)?))
}

pub async fn cache_refresh(State(st): State<std::sync::Arc<AppState>>) -> ApiResult {
    repos::refresh(&st).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn list_aliases() -> ApiResult {
    Ok(Json(json!({ "items": sqlite::list_aliases() })))
}

#[derive(Deserialize)]
pub struct AliasBody {
    pub alias: Option<String>,
    #[serde(rename = "realName")]
    pub real_name: Option<String>,
}

pub async fn upsert_alias(Json(body): Json<AliasBody>) -> ApiResult {
    let (alias, real_name) = body
        .alias
        .zip(body.real_name)
        .ok_or_else(|| anyhow::anyhow!("缺少 alias 或 realName"))?;
    sqlite::upsert_alias(&alias, &real_name);
    Ok(Json(json!({ "ok": true })))
}

pub async fn remove_alias(Path(alias): Path<String>) -> ApiResult {
    sqlite::delete_alias(&alias);
    Ok(Json(json!({ "ok": true })))
}

pub async fn plans() -> ApiResult {
    Ok(Json(json!({ "items": sqlite::list_plans() })))
}

/// `POST /api/plans/{id}/publish`：上架 / 下架对外展示页
#[derive(Deserialize)]
pub struct PublishBody {
    pub published: Option<bool>,
}

pub async fn set_plan_published(
    Path(id): Path<String>,
    Json(body): Json<PublishBody>,
) -> ApiResult {
    let id: i64 = id.parse().map_err(|_| anyhow::anyhow!("方案不存在"))?;
    let published = body.published.unwrap_or(true);
    if !sqlite::set_published(id, published) {
        return Err(anyhow::anyhow!("方案不存在").into());
    }
    Ok(Json(
        json!({ "ok": true, "id": id, "published": published }),
    ))
}

#[derive(Deserialize)]
pub struct SavePlanBody {
    pub title: Option<String>,
    pub metric: Option<String>,
    pub days: Option<i64>,
    pub payload: Option<serde_json::Value>,
}

pub async fn save_plan(Json(body): Json<SavePlanBody>) -> ApiResult {
    let payload = body
        .payload
        .ok_or_else(|| anyhow::anyhow!("缺少 payload"))?;
    let id = sqlite::save_plan(
        &body.title.unwrap_or_else(|| "未命名方案".into()),
        &body.metric.unwrap_or_else(|| "lastN".into()),
        body.days.unwrap_or(3),
        &serde_json::to_string(&payload)?,
    );
    Ok(Json(json!({ "id": id })))
}

pub async fn get_plan(Path(id): Path<String>) -> ApiResult {
    let id: i64 = id.parse().map_err(|_| anyhow::anyhow!("方案不存在"))?;
    let row = sqlite::get_plan(id).ok_or_else(|| anyhow::anyhow!("方案不存在"))?;
    let payload: serde_json::Value =
        serde_json::from_str(&row.payload).unwrap_or(serde_json::Value::Null);
    Ok(Json(
        json!({ "id": row.id, "title": row.title, "metric": row.metric, "days": row.days, "createdAt": row.created_at, "published": row.published, "payload": payload }),
    ))
}

pub async fn delete_plan(Path(id): Path<String>) -> ApiResult {
    if let Ok(id) = id.parse::<i64>() {
        sqlite::delete_plan(id);
    }
    Ok(Json(json!({ "ok": true })))
}
