//! 比赛成绩历史（只读冻结结果）。
//!
//! 这里只读取 `live_group_result` 及其赛事/阶段/小组元数据。比赛分与音浪、时长、
//! 月榜、年榜完全隔离；本模块不访问 `daily_metric`、`monthly_*`、`live_score_sample`。

use super::*;
use serde_json::{json, Value};
use sqlx::Row;

/// 把一场正式局的当前最终席位复制/刷新为历史快照。
///
/// 调用时机是晋级结果冻结之后；迁移 056 也会回填历史已定稿小组。
pub async fn live_group_result_refresh(
    st: &AppState,
    event_id: i64,
    group_id: i64,
    round_id: i64,
    qualifier_source: &str,
) -> anyhow::Result<()> {
    let stage_id = live_group_stage(st, group_id).await?;
    if stage_id <= 0 || round_id <= 0 {
        anyhow::bail!("比赛结果缺少阶段或正式局");
    }
    let seats = live_group_cumulative_seats(st, group_id).await?;
    let pool = w(st)?;
    let qualified_uids: std::collections::HashSet<String> =
        sqlx::query("SELECT douyin_uid FROM live_qualifier WHERE group_id = $1 AND source = $2")
            .bind(group_id)
            .bind(qualifier_source)
            .fetch_all(pool)
            .await?
            .into_iter()
            .filter_map(|r| r.try_get::<String, _>("douyin_uid").ok())
            .collect();
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM live_group_result WHERE group_id = $1")
        .bind(group_id)
        .execute(&mut *tx)
        .await?;
    for (position, seat) in seats.iter().enumerate() {
        let uid = seat
            .get("douyinUid")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if uid.is_empty() {
            continue;
        }
        let qualified = qualified_uids.contains(uid);
        sqlx::query(
            "INSERT INTO live_group_result
               (group_id, event_id, stage_id, round_id, person_id, douyin_uid,
                nickname, person_name_snapshot, match_score, match_rank,
                match_position, qualified, qualifier_source, frozen_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,now())",
        )
        .bind(group_id)
        .bind(event_id)
        .bind(stage_id)
        .bind(round_id)
        .bind(seat.get("personId").and_then(Value::as_i64).unwrap_or(0))
        .bind(uid)
        .bind(seat.get("nickname").and_then(Value::as_str).unwrap_or(""))
        .bind(seat.get("personName").and_then(Value::as_str).unwrap_or(""))
        .bind(seat.get("score").and_then(Value::as_i64).unwrap_or(0))
        .bind(seat.get("rank").and_then(Value::as_i64).unwrap_or(0) as i32)
        .bind(position as i32 + 1)
        .bind(if qualified { 1_i16 } else { 0_i16 })
        .bind(if qualified { qualifier_source } else { "" })
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// 有冻结比赛结果的月份，倒序。
pub async fn live_history_months(st: &AppState) -> Vec<String> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    sqlx::query(
        "SELECT DISTINCT substring(e.event_date from 1 for 7) AS month
           FROM live_event e
          WHERE e.public_token <> ''
            AND e.event_date ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}$'
            AND EXISTS (SELECT 1 FROM live_group_result r WHERE r.event_id = e.id)
          ORDER BY month DESC
          LIMIT 120",
    )
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.iter()
            .filter_map(|r| r.try_get::<String, _>("month").ok())
            .collect()
    })
    .unwrap_or_default()
}

/// 公开赛事历史列表。范围使用 event_date 字符串上界，不扫实时采样表。
pub async fn live_history_events(
    st: &AppState,
    from_date: &str,
    to_date: &str,
    limit: i64,
) -> Vec<Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    sqlx::query(
        "SELECT e.id, e.public_token, e.title, e.event_date, e.status,
                e.started_at, e.finished_at, max(r.frozen_at) AS last_result_at,
                count(DISTINCT r.group_id)::BIGINT AS group_count,
                count(r.douyin_uid)::BIGINT AS result_count,
                string_agg(DISTINCT st.kind, ',') AS stage_kinds
           FROM live_event e
           JOIN live_group_result r ON r.event_id = e.id
           JOIN live_stage st ON st.id = r.stage_id
          WHERE e.public_token <> ''
            AND e.event_date >= $1
            AND e.event_date < $2
          GROUP BY e.id, e.public_token, e.title, e.event_date, e.status, e.started_at, e.finished_at
          ORDER BY e.event_date DESC, e.started_at DESC NULLS LAST, e.id DESC
          LIMIT $3",
    )
    .bind(from_date)
    .bind(to_date)
    .bind(limit.clamp(1, 100))
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.iter()
            .map(|r| {
                let stage_kinds = r
                    .try_get::<String, _>("stage_kinds")
                    .unwrap_or_default();
                json!({
                    "key": r.try_get::<String, _>("public_token").unwrap_or_default(),
                    "title": r.try_get::<String, _>("title").unwrap_or_default(),
                    "eventDate": r.try_get::<String, _>("event_date").unwrap_or_default(),
                    "month": r.try_get::<String, _>("event_date").unwrap_or_default().chars().take(7).collect::<String>(),
                    "status": r.try_get::<String, _>("status").unwrap_or_default(),
                    "startedAt": r.try_get::<chrono::DateTime<chrono::Utc>, _>("started_at").map(|t| t.to_rfc3339()).ok(),
                    "finishedAt": r.try_get::<chrono::DateTime<chrono::Utc>, _>("finished_at").map(|t| t.to_rfc3339()).ok(),
                    "lastResultAt": r.try_get::<chrono::DateTime<chrono::Utc>, _>("last_result_at").map(|t| t.to_rfc3339()).ok(),
                    "groupCount": r.try_get::<i64, _>("group_count").unwrap_or(0),
                    "resultCount": r.try_get::<i64, _>("result_count").unwrap_or(0),
                    "stageKinds": stage_kinds.split(',').filter(|v| !v.is_empty()).collect::<Vec<_>>(),
                })
            })
            .collect()
    })
    .unwrap_or_else(|e| {
        tracing::error!(error = %e, "查公开比赛历史赛事失败");
        Vec::new()
    })
}

/// 单个赛事的历史阶段与已冻结小组。
pub async fn live_history_groups(st: &AppState, event_id: i64) -> Vec<Value> {
    let groups = live_group_list(st, event_id).await;
    if groups.is_empty() {
        return Vec::new();
    }
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    let rows = sqlx::query(
        "SELECT r.group_id, r.person_id, r.nickname, r.person_name_snapshot,
                r.match_score, r.match_rank, r.match_position, r.qualified
           FROM live_group_result r
          WHERE r.event_id = $1
          ORDER BY r.group_id, r.match_position, r.douyin_uid",
    )
    .bind(event_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let mut players_by_group: std::collections::HashMap<i64, Vec<Value>> =
        std::collections::HashMap::new();
    for r in rows {
        let gid = r.try_get::<i64, _>("group_id").unwrap_or(0);
        let person_name = r
            .try_get::<String, _>("person_name_snapshot")
            .unwrap_or_default();
        let nickname = r.try_get::<String, _>("nickname").unwrap_or_default();
        players_by_group.entry(gid).or_default().push(json!({
            "personId": r.try_get::<i64, _>("person_id").unwrap_or(0),
            "personName": person_name,
            "nickname": nickname,
            "matchScore": r.try_get::<i64, _>("match_score").unwrap_or(0),
            "rank": r.try_get::<i32, _>("match_rank").unwrap_or(0),
            "position": r.try_get::<i32, _>("match_position").unwrap_or(0),
            "qualified": r.try_get::<i16, _>("qualified").unwrap_or(0) == 1,
        }));
    }
    let mut out = Vec::new();
    for mut group in groups {
        let gid = group.get("id").and_then(Value::as_i64).unwrap_or(0);
        let Some(players) = players_by_group.remove(&gid) else {
            continue;
        };
        if let Some(obj) = group.as_object_mut() {
            obj.insert("players".into(), Value::Array(players));
            obj.insert("status".into(), json!("finalized"));
        }
        out.push(group);
    }
    out
}

/// 某个用户自己的历史比赛成绩。身份由调用方从登录 token 解析，绝不接受客户端 personId。
pub async fn live_person_matches(
    st: &AppState,
    person_id: i64,
    from_date: &str,
    to_date: &str,
    limit: i64,
) -> Vec<Value> {
    if person_id <= 0 {
        return Vec::new();
    }
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    sqlx::query(
        "WITH results AS (
           SELECT r.round_id, r.event_id, r.stage_id, r.group_id, r.person_id,
                  r.person_name_snapshot, r.nickname, r.match_score, r.match_rank,
                  r.match_position, r.qualified, r.qualifier_source, r.frozen_at, r.douyin_uid,
                  e.title AS event_title, e.event_date,
                  st.kind AS stage_kind, st.seq AS stage_seq,
                  g.name AS group_name, g.seq AS group_seq, g.qualify_slots,
                  count(*) OVER (PARTITION BY r.group_id)::BIGINT AS participant_count
             FROM live_group_result r
             JOIN live_event e ON e.id = r.event_id
             JOIN live_stage st ON st.id = r.stage_id
             JOIN live_group g ON g.id = r.group_id
            WHERE e.event_date >= $2
              AND e.event_date < $3
         ),
         rows AS (
           SELECT r.*,
                  row_number() OVER (
                    PARTITION BY r.group_id
                    ORDER BY r.match_position, r.match_score DESC, r.douyin_uid
                  ) AS account_rank
             FROM results r
            WHERE r.person_id = $1
         )
         SELECT * FROM rows
          WHERE account_rank = 1
          ORDER BY event_date DESC, stage_seq DESC, group_seq DESC, round_id DESC
          LIMIT $4",
    )
    .bind(person_id)
    .bind(from_date)
    .bind(to_date)
    .bind(limit.clamp(1, 100))
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.iter()
            .map(|r| {
                let stage_kind = r.try_get::<String, _>("stage_kind").unwrap_or_default();
                let rank = r.try_get::<i32, _>("match_rank").unwrap_or(0);
                let position = r.try_get::<i32, _>("match_position").unwrap_or(0);
                let qualified = r.try_get::<i16, _>("qualified").unwrap_or(0) == 1;
                json!({
                    "roundId": r.try_get::<i64, _>("round_id").unwrap_or(0),
                    "eventId": r.try_get::<i64, _>("event_id").unwrap_or(0),
                    "eventTitle": r.try_get::<String, _>("event_title").unwrap_or_default(),
                    "eventDate": r.try_get::<String, _>("event_date").unwrap_or_default(),
                    "stageKind": stage_kind,
                    "stageLabel": crate::live::stage_label(&stage_kind),
                    "groupName": r.try_get::<String, _>("group_name").unwrap_or_default(),
                    "groupSeq": r.try_get::<i32, _>("group_seq").unwrap_or(0),
                    "personName": r.try_get::<String, _>("person_name_snapshot").unwrap_or_default(),
                    "nickname": r.try_get::<String, _>("nickname").unwrap_or_default(),
                    "matchScore": r.try_get::<i64, _>("match_score").unwrap_or(0),
                    "rank": rank,
                    "position": position,
                    "participantCount": r.try_get::<i64, _>("participant_count").unwrap_or(0),
                    "qualified": qualified,
                    "qualifierSource": r.try_get::<String, _>("qualifier_source").unwrap_or_default(),
                    "resultLabel": crate::live::match_result_label(&stage_kind, rank, qualified),
                    "frozenAt": r.try_get::<chrono::DateTime<chrono::Utc>, _>("frozen_at").map(|t| t.to_rfc3339()).ok(),
                })
            })
            .collect()
    })
    .unwrap_or_else(|e| {
        tracing::error!(person_id, error = %e, "查个人比赛历史失败");
        Vec::new()
    })
}

pub async fn live_person_match_months(st: &AppState, person_id: i64) -> Vec<String> {
    if person_id <= 0 {
        return Vec::new();
    }
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    sqlx::query(
        "SELECT DISTINCT substring(e.event_date from 1 for 7) AS month
           FROM live_group_result r JOIN live_event e ON e.id = r.event_id
          WHERE r.person_id = $1
          ORDER BY month DESC",
    )
    .bind(person_id)
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.iter()
            .filter_map(|r| r.try_get::<String, _>("month").ok())
            .collect()
    })
    .unwrap_or_default()
}

/// 公开快照中只保留已冻结比赛，并把 `score` 明确改名为 `matchScore`。
pub async fn live_history_snapshot(st: &AppState, event: Value) -> Value {
    let event_id = event.get("id").and_then(Value::as_i64).unwrap_or(0);
    let stages = live_stage_list(st, event_id).await;
    let groups = live_history_groups(st, event_id).await;
    json!({
        "event": event,
        "stages": stages,
        "groups": groups,
        "serverNow": chrono::Utc::now().to_rfc3339(),
        "scoreKind": "match",
    })
}
