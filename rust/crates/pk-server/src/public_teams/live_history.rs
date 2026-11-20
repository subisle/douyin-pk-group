//! 展示端「我的 → 历史比赛成绩」。
//!
//! 身份只从 `X-My-Token` 解析，客户端不能传 personId。查询只读比赛结果冻结表，
//! 不读取音浪、时长、收益或日/月/年榜数据。

use super::*;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct MyMatchQuery {
    pub month: Option<String>,
    pub limit: Option<i64>,
}

fn match_summary(items: &[Value]) -> Value {
    let mut events = std::collections::HashSet::new();
    let mut qualified = 0_i64;
    let mut best_position = 0_i64;
    let mut champions = 0_i64;
    for item in items {
        if let Some(id) = item.get("eventId").and_then(Value::as_i64) {
            events.insert(id);
        }
        if item
            .get("qualified")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            qualified += 1;
        }
        let position = item.get("position").and_then(Value::as_i64).unwrap_or(0);
        if position > 0 && (best_position == 0 || position < best_position) {
            best_position = position;
        }
        if item.get("stageKind").and_then(Value::as_str) == Some("final")
            && item.get("rank").and_then(Value::as_i64) == Some(1)
        {
            champions += 1;
        }
    }
    json!({
        "eventCount": events.len() as i64,
        "appearanceCount": items.len() as i64,
        "qualifiedCount": qualified,
        "bestPosition": if best_position > 0 { Some(best_position) } else { None },
        "championCount": champions,
    })
}

/// GET /api/public/teams/{team}/my/matches?month=YYYY-MM&limit=100
pub async fn my_matches(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<MyMatchQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let settings = PublicSettings::load(&st).await;
    if !settings.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    let (person_id, my_team, person_name) =
        super::my::my_token_required_with_restore(&st, &headers).await?;
    let person_id = i64::try_from(person_id).unwrap_or(0);
    if !super::my_team_allows(&my_team, &team) {
        return Err(ApiError::bad_request("该账号不属于本团"));
    }
    let months = crate::store::live_person_match_months(&st, person_id).await;
    let month = q
        .month
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string);
    let (from_date, to_date) = match month.as_deref() {
        Some(month) => crate::live_api::month_bounds(month)?,
        None => ("0001-01-01".to_string(), "9999-12-31".to_string()),
    };
    let items = crate::store::live_person_matches(
        &st,
        person_id,
        &from_date,
        &to_date,
        q.limit.unwrap_or(100).clamp(1, 100),
    )
    .await;
    let summary = match_summary(&items);
    Ok(Json(json!({
        "published": true,
        "bound": person_id > 0,
        "person": {
            "personId": person_id,
            "personName": person_name,
        },
        "months": months,
        "month": month,
        "items": items,
        "summary": summary,
        "scoreKind": "match",
    })))
}
