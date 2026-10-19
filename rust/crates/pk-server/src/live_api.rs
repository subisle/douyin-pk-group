//! 直播 PK 的 HTTP 接口（2026-10-20）
//!
//! - 管理端 `/api/live/*`：走全局 `auth::require_admin`，与后台其它接口同一套鉴权
//! - 采集端 `/api/internal/live/*`：**只认内部令牌**（`LIVE_COLLECTOR_TOKEN`），
//!   且默认只监听 127.0.0.1（见 config::live_bind_local_only），不对外暴露

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::live::{self, LiveRules, Observation, RoundStatus, StageKind};
use crate::state::AppState;
use crate::store;

type ApiResult = Result<Json<Value>, crate::routes::ApiError>;

/* ------------------------------ 采集端鉴权 ------------------------------ */

/// 采集端内部令牌（`LIVE_COLLECTOR_TOKEN`）。
///
/// 不复用管理员口令：采集器是独立常驻进程，长期持凭证，一旦泄露要能单独吊销，
/// 且不该拿到管理端的任何权限（它只能上报观测）。
/// **未配置时一律拒绝**（fail-closed）：宁可采集不起来，也不让上报口裸奔。
pub fn collector_token() -> Option<String> {
    std::env::var("LIVE_COLLECTOR_TOKEN")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// 采集端中间件：只放行带正确 `X-Live-Token` 的请求。
pub async fn require_collector(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let expected = match collector_token() {
        Some(t) => t,
        None => {
            tracing::warn!("LIVE_COLLECTOR_TOKEN 未配置，采集上报接口已关闭");
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "采集上报未启用（未配置 LIVE_COLLECTOR_TOKEN）" })),
            )
                .into_response();
        }
    };
    let got = req
        .headers()
        .get("x-live-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    // 常数时间比较，避免按字节提前返回泄露前缀
    if !constant_time_eq(got.as_bytes(), expected.as_bytes()) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "采集令牌无效" })),
        )
            .into_response();
    }
    next.run(req).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/* ------------------------------ 管理端 ------------------------------ */

#[derive(Deserialize)]
pub struct CreateEventBody {
    pub title: Option<String>,
    #[serde(rename = "eventDate")]
    pub event_date: Option<String>,
    #[serde(rename = "dayPlanId")]
    pub day_plan_id: Option<i64>,
    pub rules: Option<LiveRules>,
}

pub async fn events(State(st): State<Arc<AppState>>) -> ApiResult {
    Ok(Json(
        json!({ "items": store::live_event_list(&st, 50).await }),
    ))
}

pub async fn create_event(
    State(st): State<Arc<AppState>>,
    Json(body): Json<CreateEventBody>,
) -> ApiResult {
    let rules = body.rules.unwrap_or_default();
    live::validate_rules(&rules).map_err(crate::routes::ApiError::bad_request)?;
    let id = store::live_event_create(
        &st,
        &body.title.unwrap_or_else(|| "未命名赛事".into()),
        &body.event_date.unwrap_or_default(),
        &rules,
        body.day_plan_id.unwrap_or(0),
    )
    .await?;
    crate::events::emit(crate::events::SCOPE_LIVE, format!("直播赛事 {id} 已创建"));
    Ok(Json(json!({ "id": id })))
}

/// 绑定分组方案（白天小组赛 / 晚上晋级赛）。
///
/// 只记方案 id：实时分数绝不写回 `plans.payload`（每几秒刷一次会覆盖运营的分组卡）。
#[derive(Deserialize)]
pub struct BindPlansBody {
    #[serde(rename = "dayPlanId")]
    pub day_plan_id: Option<i64>,
    #[serde(rename = "nightPlanId")]
    pub night_plan_id: Option<i64>,
}

pub async fn bind_plans(
    State(st): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<BindPlansBody>,
) -> ApiResult {
    if body.day_plan_id.is_none() && body.night_plan_id.is_none() {
        return Err(crate::routes::ApiError::bad_request(
            "至少要给一个方案 id（dayPlanId 或 nightPlanId）",
        ));
    }
    if let Some(pid) = body.day_plan_id {
        if pid > 0 && crate::store::get_plan(&st, pid).await.is_none() {
            return Err(crate::routes::ApiError::bad_request("白天分组方案不存在"));
        }
    }
    if let Some(pid) = body.night_plan_id {
        if pid > 0 && crate::store::get_plan(&st, pid).await.is_none() {
            return Err(crate::routes::ApiError::bad_request("晚上晋级赛方案不存在"));
        }
    }
    store::live_event_bind_plans(&st, id, body.day_plan_id, body.night_plan_id).await?;
    crate::events::emit(
        crate::events::SCOPE_LIVE,
        format!("赛事 {id} 已绑定分组方案"),
    );
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct RulesBody {
    pub rules: LiveRules,
    pub reason: Option<String>,
}

/// 改规则。只影响未定稿阶段：已 finalized 的阶段不允许改（避免追溯改结果）。
pub async fn update_rules(
    State(st): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<RulesBody>,
) -> ApiResult {
    live::validate_rules(&body.rules).map_err(crate::routes::ApiError::bad_request)?;
    if store::live_has_finalized_stage(&st, id)
        .await
        .unwrap_or(false)
    {
        return Err(crate::routes::ApiError::bad_request(
            "已有定稿阶段，不能改规则（改了不会重算历史结果）",
        ));
    }
    store::live_rules_update(
        &st,
        id,
        &body.rules,
        "admin",
        &body.reason.unwrap_or_default(),
    )
    .await?;
    crate::events::emit(crate::events::SCOPE_LIVE, format!("赛事 {id} 规则已更新"));
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct MonitorBody {
    pub url: String,
    pub label: Option<String>,
}

/// 加监控直播间。URL 形如 https://live.douyin.com/505899768449
pub async fn add_monitor(
    State(st): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<MonitorBody>,
) -> ApiResult {
    let web_rid = extract_web_rid(&body.url)
        .ok_or_else(|| crate::routes::ApiError::bad_request("直播间地址里没有找到直播间 id"))?;
    let rid = store::live_monitor_upsert(
        &st,
        id,
        &web_rid,
        &body.url,
        &body.label.unwrap_or_default(),
    )
    .await?;
    crate::events::emit(
        crate::events::SCOPE_LIVE,
        format!("赛事 {id} 已添加监控直播间 {web_rid}"),
    );
    Ok(Json(json!({ "id": rid, "webRid": web_rid })))
}

/// 从直播间 URL 里取 web_rid（505899768449 这类短 id）
pub fn extract_web_rid(url: &str) -> Option<String> {
    let path = url.split('?').next().unwrap_or(url);
    let last = path.trim_end_matches('/').rsplit('/').next()?;
    let rid = last.trim();
    (!rid.is_empty() && rid.chars().all(|c| c.is_ascii_alphanumeric())).then(|| rid.to_string())
}

/// 监控总览：赛事 + 阶段/小组 + 直播间健康 + 晋级名单
pub async fn overview(State(st): State<Arc<AppState>>, Path(id): Path<i64>) -> ApiResult {
    let event = store::live_event_list(&st, 200)
        .await
        .into_iter()
        .find(|e| e.get("id").and_then(|v| v.as_i64()) == Some(id));
    let groups = store::live_group_list(&st, id).await;
    let rooms = store::live_monitor_list(&st, id).await;
    let qualifiers = store::live_qualifier_list(&st, id).await;
    let stages = store::live_stage_list(&st, id).await;
    Ok(Json(json!({
        "event": event,
        "stages": stages,
        "groups": groups,
        "rooms": rooms,
        "qualifiers": qualifiers,
    })))
}

#[derive(Deserialize)]
pub struct GroupQuery {
    #[serde(rename = "groupId")]
    pub group_id: Option<i64>,
}

/// 实时榜：某局/某组的最新分数与排名
pub async fn live_scores(
    State(st): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(q): Query<GroupQuery>,
) -> ApiResult {
    let group_id = q.group_id.unwrap_or(0);
    let round_id = if group_id > 0 {
        store::live_group_official_round(&st, group_id)
            .await?
            .or_else(|| None)
            .unwrap_or_else(|| {
                // 还没定稿就取该组最近一局（实时监控要看正在打的这局）
                0
            })
    } else {
        0
    };
    let round_id = if round_id == 0 && group_id > 0 {
        futures_latest_round(&st, group_id).await
    } else {
        round_id
    };
    let scores = if round_id > 0 {
        store::live_latest_scores(&st, round_id).await
    } else {
        Vec::new()
    };
    let seats = if round_id > 0 {
        store::live_seat_list(&st, round_id)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    Ok(Json(json!({
        "eventId": id,
        "groupId": group_id,
        "roundId": round_id,
        "scores": scores,
        "seats": seats,
    })))
}

async fn futures_latest_round(st: &Arc<AppState>, group_id: i64) -> i64 {
    store::live_latest_round_id(st, group_id).await.unwrap_or(0)
}

/* ------------------------------ 争议局与人工干预 ------------------------------ */

#[derive(Deserialize)]
pub struct OfficialBody {
    #[serde(rename = "roundId")]
    pub round_id: i64,
}

/// 选正式局：同一小组两场完整对局时用。会清掉同组其它局的正式标记并定稿该组。
pub async fn choose_official(
    State(st): State<Arc<AppState>>,
    Path(group_id): Path<i64>,
    Json(body): Json<OfficialBody>,
) -> ApiResult {
    store::live_round_set_official(&st, body.round_id).await?;
    store::live_round_event_add(
        &st,
        body.round_id,
        "official_chosen",
        "管理员选择本局为正式局",
        "admin",
    )
    .await?;
    let event_id = store::live_event_of_group(&st, group_id).await.unwrap_or(0);
    if event_id > 0 {
        crate::events::emit(
            crate::events::SCOPE_LIVE,
            format!("小组 {group_id} 已选定正式局"),
        );
    }
    Ok(Json(json!({ "ok": true, "roundId": body.round_id })))
}

/// 作废一局（误识别 / 运营判定不算）
pub async fn void_round(State(st): State<Arc<AppState>>, Path(round_id): Path<i64>) -> ApiResult {
    store::live_round_set_status(&st, round_id, RoundStatus::Void, Some("manual_abort")).await?;
    store::live_round_event_add(&st, round_id, "voided", "管理员作废本局", "admin").await?;
    Ok(Json(json!({ "ok": true })))
}

/// 强制定稿（正常局没被自动判定时用）
pub async fn finalize_group(
    State(st): State<Arc<AppState>>,
    Path(group_id): Path<i64>,
) -> ApiResult {
    let round_id = store::live_group_official_round(&st, group_id)
        .await?
        .or(store::live_latest_round_id(&st, group_id).await.ok())
        .filter(|v| *v > 0)
        .ok_or_else(|| crate::routes::ApiError::bad_request("这个小组还没有对局"))?;
    store::live_round_set_official(&st, round_id).await?;
    store::live_round_event_add(&st, round_id, "force_finalized", "管理员强制定稿", "admin")
        .await?;
    Ok(Json(json!({ "ok": true, "roundId": round_id })))
}

/// 某组的所有对局（争议局并排对比）
pub async fn group_rounds(State(st): State<Arc<AppState>>, Path(group_id): Path<i64>) -> ApiResult {
    Ok(Json(json!({
        "items": store::live_round_list(&st, group_id).await,
    })))
}

/* ------------------------------ 赛程推进 ------------------------------ */

/// 从分组方案生成白天小组（第一波 + 剩余）。
/// 方案 payload 里的 buckets 就是分组；按规则切成第一波与剩余两批。
pub async fn build_day_groups(State(st): State<Arc<AppState>>, Path(id): Path<i64>) -> ApiResult {
    let rules = store::live_event_rules(&st, id)
        .await
        .ok_or_else(|| crate::routes::ApiError::bad_request("赛事不存在"))?;
    let plan_id = store::live_event_day_plan(&st, id).await.unwrap_or(0);
    if plan_id <= 0 {
        return Err(crate::routes::ApiError::bad_request(
            "赛事还没绑定白天分组方案",
        ));
    }
    let buckets = store::plan_buckets(&st, plan_id).await?;
    let first_wave = live::build_stage_groups(
        &st,
        id,
        StageKind::FirstWaveGroup,
        &buckets[..buckets.len().min(rules.first_wave_group_count as usize)],
        rules.direct_qualify_per_group,
    )
    .await?;
    let rest = if buckets.len() > rules.first_wave_group_count as usize {
        live::build_stage_groups(
            &st,
            id,
            StageKind::RestGroup,
            &buckets[rules.first_wave_group_count as usize..],
            rules.direct_qualify_per_group,
        )
        .await?
    } else {
        0
    };
    crate::events::emit(
        crate::events::SCOPE_LIVE,
        format!("赛事 {id} 白天小组已生成"),
    );
    Ok(Json(json!({ "firstWave": first_wave, "rest": rest })))
}

/// 冻结一个小组的晋级结果（按正式局的名次取前 N 名）。
///
/// 冻结后这些人的 `qualified` 落库，剩下的就是**淘汰者**——复活赛的参赛池
/// 由 `live_stage_eliminated` 查询得出，所以这一步必须在生成复活赛之前做。
#[derive(Deserialize)]
pub struct FreezeBody {
    /// direct / revival / promotion（决定晚上汇总时按哪一类取人）
    #[serde(default = "default_source")]
    pub source: String,
}

fn default_source() -> String {
    "direct".into()
}

pub async fn freeze_group(
    State(st): State<Arc<AppState>>,
    Path(group_id): Path<i64>,
    Json(body): Json<FreezeBody>,
) -> ApiResult {
    let source = if body.source.is_empty() {
        "direct".to_string()
    } else {
        body.source.clone()
    };
    if !matches!(source.as_str(), "direct" | "revival" | "promotion") {
        return Err(crate::routes::ApiError::bad_request(
            "source 只能是 direct / revival / promotion",
        ));
    }
    let event_id = store::live_event_of_group(&st, group_id).await.unwrap_or(0);
    let n = live::freeze_group_qualifiers(&st, event_id, group_id, &source).await?;
    crate::events::emit(
        crate::events::SCOPE_LIVE,
        format!("小组 {group_id} 已冻结 {n} 个晋级名额"),
    );
    Ok(Json(json!({ "ok": true, "qualified": n })))
}

/// 把某个阶段下已定稿小组的晋级结果**批量**冻结（小组赛打完一键收口）。
#[derive(Deserialize)]
pub struct FreezeStageBody {
    #[serde(default = "default_source")]
    pub source: String,
}

pub async fn freeze_stage(
    State(st): State<Arc<AppState>>,
    Query(q): Query<std::collections::HashMap<String, String>>,
    Json(body): Json<FreezeStageBody>,
) -> ApiResult {
    let wave = q.get("wave").cloned().unwrap_or_else(|| "first".into());
    let kind = if wave == "rest" {
        StageKind::RestGroup
    } else {
        StageKind::FirstWaveGroup
    };
    let source = if body.source.is_empty() {
        "direct".to_string()
    } else {
        body.source.clone()
    };
    let event_id: i64 = q.get("eventId").and_then(|v| v.parse().ok()).unwrap_or(0);
    if event_id <= 0 {
        return Err(crate::routes::ApiError::bad_request("缺少 eventId"));
    }
    let stage_id = store::live_stage_id_of(&st, event_id, kind).await?;
    let groups = store::live_group_list(&st, event_id).await;
    let mut total = 0usize;
    let mut done = 0usize;
    for g in groups.iter() {
        if g.get("stageId").and_then(|v| v.as_i64()) != Some(stage_id) {
            continue;
        }
        if g.get("status").and_then(|v| v.as_str()) != Some("finalized") {
            continue;
        }
        let gid = g.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        total += live::freeze_group_qualifiers(&st, event_id, gid, &source).await?;
        done += 1;
    }
    crate::events::emit(
        crate::events::SCOPE_LIVE,
        format!(
            "{} 阶段已冻结 {done} 个小组、共 {total} 人晋级",
            kind.as_str()
        ),
    );
    Ok(Json(
        json!({ "ok": true, "groups": done, "qualified": total }),
    ))
}

/* ------------------------------ 推送开关 ------------------------------ */

/// 赛事推送开关 + 间隔（第二层开关）。
///
/// 三层开关的层次，任一层关掉都不发：
/// 1. `PUSH_OUT_ENABLED`（全局总闸，`push.rs`）
/// 2. `live_event.push_enabled` + `push_interval_secs`（本接口，赛事级）
/// 3. `push_bindings.live_pk_report`（主播自己开没开）
#[derive(Deserialize)]
pub struct PushBody {
    #[serde(default, rename = "pushEnabled")]
    pub push_enabled: Option<bool>,
    #[serde(default, rename = "pushIntervalSecs")]
    pub push_interval_secs: Option<i32>,
}

pub async fn update_push(
    State(st): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<PushBody>,
) -> ApiResult {
    let (enabled, interval) = store::live_event_push(&st, id).await.unwrap_or((true, 60));
    let enabled = body.push_enabled.unwrap_or(enabled);
    let interval = body.push_interval_secs.unwrap_or(interval);
    /* 校验放**这里**而不是 store 层的 `anyhow::ensure!`：
    后者转成 ApiError 时走 From<anyhow::Error>（safe=false），
    500 兜底会把「间隔要在 10~3600 秒」吞成「服务器开小差了」——
    运营输错一个数字就看到一句毫不相关的报错。 */
    if !(10..=3600).contains(&interval) {
        return Err(crate::routes::ApiError::bad_request(
            "推送间隔要在 10~3600 秒之间",
        ));
    }
    store::live_event_set_push(&st, id, enabled, interval).await?;
    crate::events::emit(
        crate::events::SCOPE_LIVE,
        format!(
            "赛事 {id} 推送已{}（间隔 {interval} 秒）",
            if enabled { "开启" } else { "关闭" }
        ),
    );
    Ok(Json(
        json!({ "ok": true, "pushEnabled": enabled, "pushIntervalSecs": interval }),
    ))
}

/// 主播侧直播推送开关（第三层）
#[derive(Deserialize)]
pub struct PersonPushBody {
    pub name: String,
    #[serde(default)]
    pub on: Option<bool>,
}

pub async fn set_person_push(
    State(st): State<Arc<AppState>>,
    Json(body): Json<PersonPushBody>,
) -> ApiResult {
    let on = body.on.unwrap_or(true);
    let ok = store::live_set_person_push(&st, &body.name, on).await?;
    if !ok {
        return Err(crate::routes::ApiError::bad_request("展示端账号不存在"));
    }
    Ok(Json(
        json!({ "ok": true, "name": body.name, "livePkReport": on }),
    ))
}

/// 手动给某一局的所有参赛主播推一次当前分数（运营发现没推上时的补救口）
pub async fn push_round_now(
    State(st): State<Arc<AppState>>,
    Path(round_id): Path<i64>,
) -> ApiResult {
    let n = live::push_round_scores(&st, round_id).await?;
    Ok(Json(json!({ "ok": true, "sent": n })))
}

/// 生成某一波的复活赛分组：把该波淘汰者**平均**分进若干房间，每房前 N 晋级。
#[derive(Deserialize)]
pub struct RevivalQuery {
    pub wave: Option<String>, // first | rest
}

pub async fn build_revival(
    State(st): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(q): Query<RevivalQuery>,
) -> ApiResult {
    let wave = q.wave.unwrap_or_else(|| "first".into());
    let kind = if wave == "rest" {
        StageKind::RestRevival
    } else {
        StageKind::FirstWaveRevival
    };
    let rooms = live::build_revival_rooms(&st, id, kind).await?;
    crate::events::emit(
        crate::events::SCOPE_LIVE,
        format!("赛事 {id} 复活赛已生成 {} 个房间", rooms),
    );
    Ok(Json(json!({ "rooms": rooms })))
}

/// 生成晚上晋级赛分组：白天直接晋级 + 复活晋级，蛇形分房，每组第 1 进决赛。
pub async fn build_promotion(State(st): State<Arc<AppState>>, Path(id): Path<i64>) -> ApiResult {
    let groups = live::build_promotion_groups(&st, id).await?;
    crate::events::emit(
        crate::events::SCOPE_LIVE,
        format!("赛事 {id} 晋级赛已生成 {} 组", groups),
    );
    Ok(Json(json!({ "groups": groups })))
}

/* ------------------------------ 采集端（内部） ------------------------------ */

/// 采集端上报观测。鉴权在 main.rs 挂的中间件里做（只认 LIVE_COLLECTOR_TOKEN）。
pub async fn ingest(
    State(st): State<Arc<AppState>>,
    Path(event_id): Path<i64>,
    Json(obs): Json<Observation>,
) -> ApiResult {
    let out = live::ingest_observation(&st, event_id, &obs).await?;
    Ok(Json(json!({
        "ok": true,
        "roundId": out.round_id,
        "groupId": out.group_id,
        "phase": out.phase,
        "isNewRound": out.is_new_round,
        "note": out.note,
    })))
}

/// 采集进程心跳（排查「采集器还活着吗」用）
#[derive(Deserialize)]
pub struct HeartbeatBody {
    #[serde(rename = "workerId")]
    pub worker_id: String,
    pub version: Option<String>,
    pub rooms: Option<i32>,
}

pub async fn heartbeat(
    State(st): State<Arc<AppState>>,
    Json(body): Json<HeartbeatBody>,
) -> ApiResult {
    store::live_worker_heartbeat(
        &st,
        &body.worker_id,
        &body.version.unwrap_or_default(),
        body.rooms.unwrap_or(0),
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_web_rid_from_douyin_url() {
        assert_eq!(
            extract_web_rid("https://live.douyin.com/505899768449?from_search=true"),
            Some("505899768449".into())
        );
        assert_eq!(
            extract_web_rid("https://live.douyin.com/505899768449/"),
            Some("505899768449".into())
        );
        assert_eq!(extract_web_rid("https://live.douyin.com/"), None);
        assert_eq!(extract_web_rid(""), None);
        // 非法字符（含斜杠以外的符号）不认，避免把整段路径当房间 id
        assert_eq!(extract_web_rid("https://live.douyin.com/abc-def!"), None);
    }
}
