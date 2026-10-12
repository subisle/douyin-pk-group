//! HTTP 路由：与旧版 Fastify 服务保持完全一致的路径与响应结构，
//! 这样前端 api.ts 无需改动即可切到 Rust 后端。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::matching::{match_roster, MatchOptions};
use crate::pgrs;
use crate::repos;
use crate::state::AppState;
use crate::store;

type ApiResult = Result<Json<serde_json::Value>, ApiError>;

pub struct ApiError(anyhow::Error, Option<StatusCode>);

impl ApiError {
    /// 登录态问题（未登录 / 过期 / 团别不符）→ 401，展示端登录门据此弹登录卡
    pub fn auth(msg: impl Into<String>) -> Self {
        Self(anyhow::anyhow!(msg.into()), Some(StatusCode::UNAUTHORIZED))
    }

    /// 明确的 400（功能没开之类）：不靠中文关键词推断，文案可以随便写
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self(anyhow::anyhow!(msg.into()), Some(StatusCode::BAD_REQUEST))
    }

    /// 显式指定状态码的错误（2026-10-12 审计 L3）：给 axum 已经分好类、
    /// 不该再落进中文关键词推断的错误用，比如 multipart 超限的 413。
    /// 注意带显式状态码的文案会原样回传客户端（不走 500 的通用文案兜底），
    /// 所以调用方只能传面向用户的安全文案。
    pub fn with_status(status: StatusCode, msg: impl Into<String>) -> Self {
        Self(anyhow::anyhow!(msg.into()), Some(status))
    }

    /// 地区 / 访问限制：前端不应把 403 当作“需要重新登录”处理
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self(anyhow::anyhow!(msg.into()), Some(StatusCode::FORBIDDEN))
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self(e, None)
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        Self(e.into(), None)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        Self(e.into(), None)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let msg = self.0.to_string();
        if let Some(status) = self.1 {
            return (status, Json(json!({ "error": msg }))).into_response();
        }
        let bad_request = msg.contains("缺少")
            || msg.contains("未知数据源")
            || msg.contains("只读连接拒绝")
            || msg.contains("名单为空")
            || msg.contains("必须")
            || msg.contains("必填")
            || msg.contains("格式")
            || msg.contains("唯一")
            || msg.contains("已存在")
            || msg.contains("不能为空")
            || msg.contains("请输入")
            || msg.contains("太长")
            || msg.contains("不存在")
            || msg.contains("reject 模式");
        let status = if msg.contains("写库不可用") {
            StatusCode::SERVICE_UNAVAILABLE
        } else if msg.contains("太频繁") {
            // 审计 2026-10-11：限流被拒语义是 429，不是 500（也不该记 error 日志）
            StatusCode::TOO_MANY_REQUESTS
        } else if bad_request {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(error = %msg, "请求处理失败");
        }
        // 2026-10-12 安全审计 L5：500 类错误不把底层文本回传客户端——
        // sqlx/PG 的报错常带表名、约束名、SQL 片段，属于实现细节泄露。
        // 服务端日志（上一行）已留全量原文，客户端只给通用提示。
        let client_msg = if status == StatusCode::INTERNAL_SERVER_ERROR {
            "服务器开小差了，请稍后重试".to_string()
        } else {
            msg
        };
        (status, Json(json!({ "error": client_msg }))).into_response()
    }
}

/// `GET /api/health`：探活。P2-21 增强 —— 除了只读库连通性，还探写池与调度器：
/// - `db`：当前数据源（只读池）连通性（沿用原口径，连不上不 500）
/// - `writer`：douyin_writer 写池连通性；未配置/连不上时 ok=false 并带原因
/// - `scheduler.lastRun`：延迟推送调度器最近一轮扫描时间；一轮都没跑过标 null
///
/// P0（docs/17 §7.3）再加 `redis`：`"disabled"`（未配置 `REDIS_URL`）/ `"up"`（PING 通，
/// 500ms 超时）/ `"down"`（配了但连不上）。**down 只代表缓存层降级，不影响整体 health
/// 语义，本接口始终 200**——停 Redis 时服务行为与引入前完全一致。
pub async fn health(State(st): State<std::sync::Arc<AppState>>) -> ApiResult {
    let db = pgrs::health_check(&st.pools, &st.active_id()).await;
    let writer = match &st.writer {
        Some(pool) => {
            let ok = sqlx::query_scalar::<_, i32>("SELECT 1")
                .fetch_one(pool)
                .await
                .is_ok();
            json!({ "ok": ok, "configured": true })
        }
        None => json!({ "ok": false, "configured": false, "error": "写库未配置或启动时连接失败" }),
    };
    let scheduler = json!({
        "running": true,
        "lastRun": crate::push::scheduler_last_run(),
    });
    // P0：Redis 三态（disabled / up / down），down 不影响整体 health，不返回非 200
    let redis = crate::store::cache::status(&st).await;
    Ok(Json(json!({
        "server": "ok",
        "db": json!({ "ok": db.ok }),
        "writer": writer,
        "scheduler": scheduler,
        "redis": redis,
    })))
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
    crate::events::emit(crate::events::SCOPE_SOURCE, format!("数据源已切换到 {id}"));
    Ok(Json(json!({ "active": st.active_id() })))
}

pub async fn test_source(
    State(st): State<std::sync::Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult {
    let db = pgrs::health_check(&st.pools, &id).await;
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
        json!({ "id": id, "ok": db.ok, "error": db.error, "latestDate": latest, "anchorCount": anchor_count }),
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
        "aliases": store::list_aliases(&st).await,
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

#[derive(Deserialize)]
pub struct RosterQuery {
    /// 赛事类型（初级/中级争霸赛小组赛、月终争霸赛、两级争霸赛晋级赛）。缺省 = 未分类，兼容旧前端。
    #[serde(rename = "type")]
    pub event_type: Option<String>,
}

/// 当前名单（按赛事类型各存一份，2026-10-06）。
/// 从没保存过该类型时返回 `saved: false` + `text: null`，前端据此清空输入框。
/// 这里不能返回空串：空串会被前端理解成「名单被清空了」。
pub async fn get_roster(
    State(st): State<std::sync::Arc<AppState>>,
    Query(q): Query<RosterQuery>,
) -> ApiResult {
    let event_type = store::normalize_event_type(q.event_type.as_deref().unwrap_or(""));
    match store::load_roster(&st, &event_type).await {
        Some((text, updated_at)) => Ok(Json(
            json!({ "saved": true, "text": text, "updatedAt": updated_at, "type": event_type }),
        )),
        None => Ok(Json(
            json!({ "saved": false, "text": null, "updatedAt": null, "type": event_type }),
        )),
    }
}

#[derive(Deserialize)]
pub struct RosterBody {
    pub text: Option<String>,
    #[serde(rename = "type")]
    pub event_type: Option<String>,
}

/// 覆盖保存名单（按赛事类型）。后端也拒收空名单：前端只是防手滑，真正的兜底要落在这一层，
/// 否则任何一次空写入都会把用户攒了很久的名单永久盖掉。
pub async fn put_roster(
    State(st): State<std::sync::Arc<AppState>>,
    Json(body): Json<RosterBody>,
) -> ApiResult {
    let text = body
        .text
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("名单为空，不保存"))?;
    let event_type = store::normalize_event_type(body.event_type.as_deref().unwrap_or(""));
    let updated_at = store::save_roster(&st, &event_type, &text).await;
    Ok(Json(
        json!({ "ok": true, "updatedAt": updated_at, "type": event_type }),
    ))
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

pub async fn list_aliases(State(st): State<std::sync::Arc<AppState>>) -> ApiResult {
    Ok(Json(json!({ "items": store::list_aliases(&st).await })))
}

#[derive(Deserialize)]
pub struct AliasBody {
    pub alias: Option<String>,
    #[serde(rename = "realName")]
    pub real_name: Option<String>,
}

pub async fn upsert_alias(
    State(st): State<std::sync::Arc<AppState>>,
    Json(body): Json<AliasBody>,
) -> ApiResult {
    let (alias, real_name) = body
        .alias
        .zip(body.real_name)
        .ok_or_else(|| anyhow::anyhow!("缺少 alias 或 realName"))?;
    store::upsert_alias(&st, &alias, &real_name).await?;
    crate::events::emit(
        crate::events::SCOPE_ROSTER,
        format!("新增别名 {alias} → {real_name}"),
    );
    Ok(Json(json!({ "ok": true })))
}

pub async fn remove_alias(
    State(st): State<std::sync::Arc<AppState>>,
    Path(alias): Path<String>,
) -> ApiResult {
    store::delete_alias(&st, &alias).await?;
    crate::events::emit(crate::events::SCOPE_ROSTER, format!("删除别名 {alias}"));
    Ok(Json(json!({ "ok": true })))
}

pub async fn plans(State(st): State<std::sync::Arc<AppState>>) -> ApiResult {
    Ok(Json(json!({ "items": store::list_plans(&st).await })))
}

/// `POST /api/plans/{id}/publish`：上架 / 下架对外展示页
#[derive(Deserialize)]
pub struct PublishBody {
    pub published: Option<bool>,
}

pub async fn set_plan_published(
    State(st): State<std::sync::Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<PublishBody>,
) -> ApiResult {
    let id: i64 = id.parse().map_err(|_| anyhow::anyhow!("方案不存在"))?;
    let published = body.published.unwrap_or(true);
    if !store::set_published(&st, id, published).await {
        return Err(anyhow::anyhow!("方案不存在").into());
    }
    crate::events::emit(
        crate::events::SCOPE_PLANS,
        if published {
            format!("方案 {id} 已上架")
        } else {
            format!("方案 {id} 已下架")
        },
    );
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
    /// 赛事类型（初级/中级争霸赛小组赛、月终争霸赛、两级争霸赛晋级赛），白名单外的值落成未分类
    #[serde(rename = "eventType")]
    pub event_type: Option<String>,
}

pub async fn save_plan(
    State(st): State<std::sync::Arc<AppState>>,
    Json(body): Json<SavePlanBody>,
) -> ApiResult {
    let payload = body
        .payload
        .ok_or_else(|| anyhow::anyhow!("缺少 payload"))?;
    let id = store::save_plan(
        &st,
        &body.title.unwrap_or_else(|| "未命名方案".into()),
        &body.metric.unwrap_or_else(|| "lastN".into()),
        body.days.unwrap_or(3),
        &serde_json::to_string(&payload)?,
        body.event_type.as_deref().unwrap_or(""),
    )
    .await?;
    crate::events::emit(crate::events::SCOPE_PLANS, format!("方案 {id} 已保存"));
    Ok(Json(json!({ "id": id })))
}

pub async fn get_plan(
    State(st): State<std::sync::Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult {
    let id: i64 = id.parse().map_err(|_| anyhow::anyhow!("方案不存在"))?;
    let row = store::get_plan(&st, id)
        .await
        .ok_or_else(|| anyhow::anyhow!("方案不存在"))?;
    let payload: serde_json::Value =
        serde_json::from_str(&row.payload).unwrap_or(serde_json::Value::Null);
    Ok(Json(
        json!({ "id": row.id, "title": row.title, "metric": row.metric, "days": row.days, "createdAt": row.created_at, "published": row.published, "eventType": row.event_type, "payload": payload }),
    ))
}

pub async fn delete_plan(
    State(st): State<std::sync::Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult {
    if let Ok(id) = id.parse::<i64>() {
        store::delete_plan(&st, id).await?;
        crate::events::emit(crate::events::SCOPE_PLANS, format!("方案 {id} 已删除"));
    }
    Ok(Json(json!({ "ok": true })))
}
