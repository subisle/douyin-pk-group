//! store::live —— 直播 PK 赛事运行态的读写（2026-10-20）
//!
//! 写路径全部走业务写池（`w()`），与项目其它写路径同一口径；
//! SQL 一律过 `crate::pq::rewrite` 把 `?` 转成 `$N`。
//! 实时分数不进 `plans.payload`（那是运营的分组快照），只落在 `live_*` 表。

use super::*; // 共享 w() / now_str() / AppState
use crate::live::{
    BattleSnapshot, GroupStatus, LiveRules, Observation, RankedSeat, RoundStatus, StageKind,
    StageStatus,
};
use serde_json::Value;
use sqlx::Row;

/* ------------------------------ 赛事 ------------------------------ */

/// 创建赛事（带规则快照）。返回 id。
pub async fn live_event_create(
    st: &AppState,
    title: &str,
    event_date: &str,
    rules: &LiveRules,
    day_plan_id: i64,
) -> anyhow::Result<i64> {
    let row = sqlx::query(&crate::pq::rewrite(
        "INSERT INTO live_event (title, event_date, status, day_plan_id, rule_version,
            first_wave_group_count, group_rounds_per_group, direct_qualify_per_group,
            revival_room_capacity, revival_qualify_per_room, promotion_group_count,
            promotion_group_size, promotion_qualify_per_group, final_room_capacity,
            settle_grace_secs, push_enabled, push_interval_secs)
         VALUES (?, ?, 'draft', ?, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, 60)
         RETURNING id",
    ))
    .bind(title)
    .bind(event_date)
    .bind(day_plan_id)
    .bind(rules.first_wave_group_count)
    .bind(rules.group_rounds_per_group)
    .bind(rules.direct_qualify_per_group)
    .bind(rules.revival_room_capacity)
    .bind(rules.revival_qualify_per_room)
    .bind(rules.promotion_group_count)
    .bind(rules.promotion_group_size)
    .bind(rules.promotion_qualify_per_group)
    .bind(rules.final_room_capacity)
    .bind(rules.settle_grace_secs)
    .fetch_one(w(st)?)
    .await?;
    Ok(row.try_get::<i64, _>("id").unwrap_or(0))
}

/// 赛事列表（管理端下拉用）
pub async fn live_event_list(st: &AppState, limit: i64) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    sqlx::query(
        "SELECT id, title, event_date, status, day_plan_id, night_plan_id, rule_version,
                first_wave_group_count, group_rounds_per_group, direct_qualify_per_group,
                revival_room_capacity, revival_qualify_per_room, promotion_group_count,
                promotion_group_size, promotion_qualify_per_group, final_room_capacity,
                settle_grace_secs, push_enabled, push_interval_secs, updated_at
         FROM live_event ORDER BY id DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .map(|rows| rows.iter().map(live_event_json).collect())
    .unwrap_or_else(|e| {
        tracing::error!(error = %e, "查直播赛事失败");
        Vec::new()
    })
}

fn live_event_json(r: &sqlx::postgres::PgRow) -> serde_json::Value {
    let g = |c: &str| r.try_get::<i32, _>(c).unwrap_or(0);
    let s = |c: &str| r.try_get::<String, _>(c).unwrap_or_default();
    serde_json::json!({
        "id": r.try_get::<i64, _>("id").unwrap_or(0),
        "title": s("title"),
        "eventDate": s("event_date"),
        "status": s("status"),
        "dayPlanId": r.try_get::<i64, _>("day_plan_id").unwrap_or(0),
        "nightPlanId": r.try_get::<i64, _>("night_plan_id").unwrap_or(0),
        "ruleVersion": g("rule_version"),
        "rules": {
            "firstWaveGroupCount": g("first_wave_group_count"),
            "groupRoundsPerGroup": g("group_rounds_per_group"),
            "directQualifyPerGroup": g("direct_qualify_per_group"),
            "revivalRoomCapacity": g("revival_room_capacity"),
            "revivalQualifyPerRoom": g("revival_qualify_per_room"),
            "promotionGroupCount": g("promotion_group_count"),
            "promotionGroupSize": g("promotion_group_size"),
            "promotionQualifyPerGroup": g("promotion_qualify_per_group"),
            "finalRoomCapacity": g("final_room_capacity"),
            "settleGraceSecs": g("settle_grace_secs"),
        },
        "pushEnabled": r.try_get::<i16, _>("push_enabled").unwrap_or(0) == 1,
        "pushIntervalSecs": g("push_interval_secs"),
        "updatedAt": r.try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at")
            .map(|t| t.to_rfc3339())
            .unwrap_or_default(),
    })
}

/// 读赛事规则（阶段推进 / 生成分组用）
pub async fn live_event_rules(st: &AppState, event_id: i64) -> Option<LiveRules> {
    let pool = w(st).ok()?;
    let r = sqlx::query(
        "SELECT first_wave_group_count, group_rounds_per_group, direct_qualify_per_group,
                revival_room_capacity, revival_qualify_per_room, promotion_group_count,
                promotion_group_size, promotion_qualify_per_group, final_room_capacity,
                settle_grace_secs
         FROM live_event WHERE id = $1",
    )
    .bind(event_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    let g = |c: &str| r.try_get::<i32, _>(c).unwrap_or(0);
    Some(LiveRules {
        first_wave_group_count: g("first_wave_group_count"),
        group_rounds_per_group: g("group_rounds_per_group"),
        direct_qualify_per_group: g("direct_qualify_per_group"),
        revival_room_capacity: g("revival_room_capacity"),
        revival_qualify_per_room: g("revival_qualify_per_room"),
        promotion_group_count: g("promotion_group_count"),
        promotion_group_size: g("promotion_group_size"),
        promotion_qualify_per_group: g("promotion_qualify_per_group"),
        final_room_capacity: g("final_room_capacity"),
        settle_grace_secs: g("settle_grace_secs"),
    })
}

/// 改规则字段：写库 + 记一条审计。只影响未定稿阶段（调用方负责拦）。
pub async fn live_rules_update(
    st: &AppState,
    event_id: i64,
    rules: &LiveRules,
    actor: &str,
    reason: &str,
) -> anyhow::Result<()> {
    let pool = w(st)?;
    let mut tx = pool.begin().await?;
    let old = sqlx::query(
        "SELECT first_wave_group_count, direct_qualify_per_group, revival_room_capacity,
                revival_qualify_per_room, promotion_group_count, promotion_group_size,
                promotion_qualify_per_group, final_room_capacity, settle_grace_secs, rule_version
         FROM live_event WHERE id = $1 FOR UPDATE",
    )
    .bind(event_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| anyhow::anyhow!("赛事不存在"))?;

    let pairs: Vec<(&str, i32, i32)> = vec![
        (
            "first_wave_group_count",
            old.try_get::<i32, _>("first_wave_group_count").unwrap_or(0),
            rules.first_wave_group_count,
        ),
        (
            "direct_qualify_per_group",
            old.try_get::<i32, _>("direct_qualify_per_group")
                .unwrap_or(0),
            rules.direct_qualify_per_group,
        ),
        (
            "revival_room_capacity",
            old.try_get::<i32, _>("revival_room_capacity").unwrap_or(0),
            rules.revival_room_capacity,
        ),
        (
            "revival_qualify_per_room",
            old.try_get::<i32, _>("revival_qualify_per_room")
                .unwrap_or(0),
            rules.revival_qualify_per_room,
        ),
        (
            "promotion_group_count",
            old.try_get::<i32, _>("promotion_group_count").unwrap_or(0),
            rules.promotion_group_count,
        ),
        (
            "promotion_group_size",
            old.try_get::<i32, _>("promotion_group_size").unwrap_or(0),
            rules.promotion_group_size,
        ),
        (
            "promotion_qualify_per_group",
            old.try_get::<i32, _>("promotion_qualify_per_group")
                .unwrap_or(0),
            rules.promotion_qualify_per_group,
        ),
        (
            "final_room_capacity",
            old.try_get::<i32, _>("final_room_capacity").unwrap_or(0),
            rules.final_room_capacity,
        ),
        (
            "settle_grace_secs",
            old.try_get::<i32, _>("settle_grace_secs").unwrap_or(0),
            rules.settle_grace_secs,
        ),
    ];

    for (field, old_v, new_v) in pairs {
        if old_v == new_v {
            continue;
        }
        // 字段名来自上面的固定白名单（不是外部输入），拼 SQL 无注入面
        sqlx::query(&format!(
            "UPDATE live_event SET {field} = $1, rule_version = rule_version + 1, updated_at = now() WHERE id = $2"
        ))
        .bind(new_v)
        .bind(event_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(&crate::pq::rewrite(
            "INSERT INTO live_event_rule (event_id, version, field, old_value, new_value, actor, reason)
             SELECT ?, rule_version, ?, ?, ?, ?, ? FROM live_event WHERE id = ?",
        ))
        .bind(event_id)
        .bind(field)
        .bind(old_v.to_string())
        .bind(new_v.to_string())
        .bind(actor)
        .bind(reason)
        .bind(event_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/* ------------------------------ 阶段 / 小组 ------------------------------ */

/// 建阶段（幂等：同 event + kind 只建一次）
pub async fn live_stage_ensure(
    st: &AppState,
    event_id: i64,
    kind: StageKind,
) -> anyhow::Result<i64> {
    let row = sqlx::query(&crate::pq::rewrite(
        "INSERT INTO live_stage (event_id, kind, seq, status)
         VALUES (?, ?, ?, 'pending')
         ON CONFLICT (event_id, kind) DO UPDATE SET seq = EXCLUDED.seq
         RETURNING id",
    ))
    .bind(event_id)
    .bind(kind.as_str())
    .bind(kind.order())
    .fetch_one(w(st)?)
    .await?;
    Ok(row.try_get::<i64, _>("id").unwrap_or(0))
}

pub async fn live_stage_set_status(
    st: &AppState,
    stage_id: i64,
    status: StageStatus,
) -> anyhow::Result<()> {
    let sql = if status == StageStatus::Running {
        "UPDATE live_stage SET status = $1, started_at = COALESCE(started_at, now()) WHERE id = $2"
    } else if status == StageStatus::Finalized {
        "UPDATE live_stage SET status = $1, finished_at = COALESCE(finished_at, now()) WHERE id = $2"
    } else {
        "UPDATE live_stage SET status = $1 WHERE id = $2"
    };
    sqlx::query(sql)
        .bind(status.as_str())
        .bind(stage_id)
        .execute(w(st)?)
        .await?;
    Ok(())
}

/// 建小组。返回 id。
pub async fn live_group_create(
    st: &AppState,
    event_id: i64,
    stage_id: i64,
    kind: StageKind,
    seq: i32,
    name: &str,
    qualify_slots: i32,
    source_group_id: i64,
) -> anyhow::Result<i64> {
    let row = sqlx::query(&crate::pq::rewrite(
        "INSERT INTO live_group (event_id, stage_id, kind, seq, name, status, qualify_slots, source_group_id)
         VALUES (?, ?, ?, ?, ?, 'pending', ?, ?) RETURNING id",
    ))
    .bind(event_id)
    .bind(stage_id)
    .bind(kind.as_str())
    .bind(seq)
    .bind(name)
    .bind(qualify_slots)
    .bind(source_group_id)
    .fetch_one(w(st)?)
    .await?;
    Ok(row.try_get::<i64, _>("id").unwrap_or(0))
}

/// 阶段下的小组（含官方局摘要），管理端监控页数据源
pub async fn live_group_list(st: &AppState, event_id: i64) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    sqlx::query(
        "SELECT g.id, g.stage_id, g.kind, g.seq, g.name, g.status, g.qualify_slots,
                g.source_group_id, g.official_round_id, g.finalized_at,
                s.kind AS stage_kind, s.status AS stage_status
         FROM live_group g JOIN live_stage s ON s.id = g.stage_id
         WHERE g.event_id = $1 ORDER BY s.seq, g.seq",
    )
    .bind(event_id)
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.try_get::<i64, _>("id").unwrap_or(0),
                    "stageId": r.try_get::<i64, _>("stage_id").unwrap_or(0),
                    "stageKind": r.try_get::<String, _>("stage_kind").unwrap_or_default(),
                    "stageStatus": r.try_get::<String, _>("stage_status").unwrap_or_default(),
                    "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                    "seq": r.try_get::<i32, _>("seq").unwrap_or(0),
                    "name": r.try_get::<String, _>("name").unwrap_or_default(),
                    "status": r.try_get::<String, _>("status").unwrap_or_default(),
                    "qualifySlots": r.try_get::<i32, _>("qualify_slots").unwrap_or(0),
                    "sourceGroupId": r.try_get::<i64, _>("source_group_id").unwrap_or(0),
                    "officialRoundId": r.try_get::<i64, _>("official_round_id").unwrap_or(0),
                    "finalizedAt": r.try_get::<chrono::DateTime<chrono::Utc>, _>("finalized_at")
                        .map(|t| t.to_rfc3339()).ok(),
                })
            })
            .collect()
    })
    .unwrap_or_else(|e| {
        tracing::error!(error = %e, "查直播小组失败");
        Vec::new()
    })
}

/// 改小组状态（定稿时写 finalized_at 与官方局）
pub async fn live_group_set_status(
    st: &AppState,
    group_id: i64,
    status: GroupStatus,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE live_group SET status = $1,
                finalized_at = CASE WHEN $1 = 'finalized' THEN now() ELSE finalized_at END
         WHERE id = $2",
    )
    .bind(status.as_str())
    .bind(group_id)
    .execute(w(st)?)
    .await?;
    Ok(())
}

/* ------------------------------ 对局 ------------------------------ */

/// 按 (group_id, battle_id) 定位或创建对局；返回 (round_id, is_new)。
///
/// 一个 battle_id 永远是独立一行：忘记拉人重开一局 → 新 battle_id → 新行，
/// 绝不覆盖上一局（覆盖会毁掉掉线证据，也没法事后选正式局）。
pub async fn live_round_upsert(
    st: &AppState,
    event_id: i64,
    group_id: i64,
    snap: &BattleSnapshot,
    web_rid: &str,
    parent_round_id: i64,
    attempt_no: i32,
) -> anyhow::Result<(i64, bool)> {
    let row = sqlx::query(&crate::pq::rewrite(
        "INSERT INTO live_round (event_id, group_id, battle_id, channel_id, room_web_rid,
                round_kind, attempt_no, parent_round_id, roster_signature, status)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'detected')
         ON CONFLICT (battle_id, group_id) DO UPDATE SET channel_id = EXCLUDED.channel_id
         RETURNING id, (xmax = 0) AS inserted",
    ))
    .bind(event_id)
    .bind(group_id)
    .bind(&snap.battle_id)
    .bind(&snap.channel_id)
    .bind(web_rid)
    .bind(if parent_round_id > 0 {
        "retry"
    } else {
        "normal"
    })
    .bind(attempt_no)
    .bind(parent_round_id)
    .bind(snap.roster_signature())
    .fetch_one(w(st)?)
    .await?;
    Ok((
        row.try_get::<i64, _>("id").unwrap_or(0),
        row.try_get::<bool, _>("inserted").unwrap_or(false),
    ))
}

/// 写席位（幂等：同一局同一 UID 只留一行，重复上报按最新分覆盖）
pub async fn live_seat_upsert(
    st: &AppState,
    round_id: i64,
    seat: &RankedSeat,
    person_id: i64,
    qualified: bool,
) -> anyhow::Result<()> {
    sqlx::query(&crate::pq::rewrite(
        "INSERT INTO live_round_seat (round_id, person_id, douyin_uid, nickname, team_id,
                final_score, final_rank, qualified)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (round_id, douyin_uid) DO UPDATE SET
           final_score = EXCLUDED.final_score,
           final_rank = EXCLUDED.final_rank,
           nickname = EXCLUDED.nickname,
           qualified = EXCLUDED.qualified,
           person_id = CASE WHEN live_round_seat.person_id = 0 THEN EXCLUDED.person_id
                            ELSE live_round_seat.person_id END",
    ))
    .bind(round_id)
    .bind(person_id)
    .bind(&seat.user_id)
    .bind(&seat.nickname)
    .bind(&seat.team_id)
    .bind(seat.score)
    .bind(seat.rank)
    .bind(if qualified { 1i16 } else { 0i16 })
    .execute(w(st)?)
    .await?;
    Ok(())
}

/// 分数采样（只追加；清理任务按 90 天保留）
pub async fn live_sample_insert(
    st: &AppState,
    round_id: i64,
    s: &crate::live::BattleScore,
    team_score: i64,
    team_rank: i32,
) -> anyhow::Result<()> {
    sqlx::query(&crate::pq::rewrite(
        "INSERT INTO live_score_sample (round_id, douyin_uid, score, rank, team_score, team_rank, score_version)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    ))
    .bind(round_id)
    .bind(&s.user_id)
    .bind(s.score)
    .bind(s.rank)
    .bind(team_score)
    .bind(team_rank)
    .bind(s.score_version.to_string())
    .execute(w(st)?)
    .await?;
    Ok(())
}

/// 时间线（只追加，永不修改）
pub async fn live_round_event_add(
    st: &AppState,
    round_id: i64,
    kind: &str,
    detail: &str,
    actor: &str,
) -> anyhow::Result<()> {
    sqlx::query(&crate::pq::rewrite(
        "INSERT INTO live_round_event (round_id, kind, detail, actor) VALUES (?, ?, ?, ?)",
    ))
    .bind(round_id)
    .bind(kind)
    .bind(detail)
    .bind(actor)
    .execute(w(st)?)
    .await?;
    Ok(())
}

/// 结对局状态 / 结束原因
pub async fn live_round_set_status(
    st: &AppState,
    round_id: i64,
    status: RoundStatus,
    reason: Option<&str>,
) -> anyhow::Result<()> {
    let reason = reason.unwrap_or("");
    sqlx::query(
        "UPDATE live_round SET status = $1,
                end_reason = CASE WHEN $2 = '' THEN end_reason ELSE $2 END,
                actual_end_at = CASE WHEN $1 IN ('completed','aborted') THEN now() ELSE actual_end_at END,
                updated_at = now()
         WHERE id = $3",
    )
    .bind(status.as_str())
    .bind(reason)
    .bind(round_id)
    .execute(w(st)?)
    .await?;
    Ok(())
}

/// 设正式局。`uk_live_round_official` 保证一个小组只有一场——并发下第二个调用会失败，
/// 调用方要把这个错误当成「已有正式局」而不是崩溃。
pub async fn live_round_set_official(st: &AppState, round_id: i64) -> anyhow::Result<()> {
    let pool = w(st)?;
    let mut tx = pool.begin().await?;
    let group_id: i64 = sqlx::query("SELECT group_id FROM live_round WHERE id = $1")
        .bind(round_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| anyhow::anyhow!("对局不存在"))?
        .try_get("group_id")?;
    // 先清掉同组其它正式局标记（换正式局场景），再给自己打标
    sqlx::query("UPDATE live_round SET is_official = 0 WHERE group_id = $1 AND id <> $2")
        .bind(group_id)
        .bind(round_id)
        .execute(&mut *tx)
        .await?;
    /* 争议局（pending_review）被管理员选为正式局后要转成 completed：
    pending_review 的语义是「这一局是否有效还没定」，既然已确认有效，
    它就是一个正常结束的局 —— 留在 pending_review 会让后续汇总
    （按 completed 取结果）把它漏掉。 */
    sqlx::query(
        "UPDATE live_round SET is_official = 1, updated_at = now(),
                status = CASE WHEN status = 'pending_review' THEN 'completed' ELSE status END,
                end_reason = CASE WHEN end_reason = '' THEN 'normal' ELSE end_reason END
         WHERE id = $1",
    )
    .bind(round_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE live_group SET official_round_id = $1, status = 'finalized',
                finalized_at = now() WHERE id = $2",
    )
    .bind(round_id)
    .bind(group_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// 对局详情（含席位），争议局对比用
pub async fn live_round_detail(st: &AppState, round_id: i64) -> Option<serde_json::Value> {
    let pool = w(st).ok()?;
    let r = sqlx::query(
        "SELECT id, event_id, group_id, battle_id, channel_id, room_web_rid, round_kind,
                attempt_no, parent_round_id, roster_signature, start_at, expected_end_at,
                actual_end_at, end_reason, status, is_official
         FROM live_round WHERE id = $1",
    )
    .bind(round_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    let seats = live_seat_list(st, round_id).await.unwrap_or_default();
    let events = live_round_events(st, round_id).await.unwrap_or_default();
    let ts = |c: &str| {
        r.try_get::<chrono::DateTime<chrono::Utc>, _>(c)
            .map(|t| t.to_rfc3339())
            .ok()
    };
    Some(serde_json::json!({
        "id": r.try_get::<i64, _>("id").unwrap_or(0),
        "eventId": r.try_get::<i64, _>("event_id").unwrap_or(0),
        "groupId": r.try_get::<i64, _>("group_id").unwrap_or(0),
        "battleId": r.try_get::<String, _>("battle_id").unwrap_or_default(),
        "channelId": r.try_get::<String, _>("channel_id").unwrap_or_default(),
        "roomWebRid": r.try_get::<String, _>("room_web_rid").unwrap_or_default(),
        "roundKind": r.try_get::<String, _>("round_kind").unwrap_or_default(),
        "attemptNo": r.try_get::<i32, _>("attempt_no").unwrap_or(1),
        "parentRoundId": r.try_get::<i64, _>("parent_round_id").unwrap_or(0),
        "rosterSignature": r.try_get::<String, _>("roster_signature").unwrap_or_default(),
        "startAt": ts("start_at"),
        "expectedEndAt": ts("expected_end_at"),
        "actualEndAt": ts("actual_end_at"),
        "endReason": r.try_get::<String, _>("end_reason").unwrap_or_default(),
        "status": r.try_get::<String, _>("status").unwrap_or_default(),
        "isOfficial": r.try_get::<i16, _>("is_official").unwrap_or(0) == 1,
        "seats": seats,
        "events": events,
    }))
}

pub async fn live_seat_list(
    st: &AppState,
    round_id: i64,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let rows = sqlx::query(
        "SELECT person_id, douyin_uid, nickname, team_id, final_score, final_rank, qualified
         FROM live_round_seat WHERE round_id = $1 ORDER BY final_rank, douyin_uid",
    )
    .bind(round_id)
    .fetch_all(w(st)?)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "personId": r.try_get::<i64, _>("person_id").unwrap_or(0),
                "douyinUid": r.try_get::<String, _>("douyin_uid").unwrap_or_default(),
                "nickname": r.try_get::<String, _>("nickname").unwrap_or_default(),
                "teamId": r.try_get::<String, _>("team_id").unwrap_or_default(),
                "score": r.try_get::<i64, _>("final_score").unwrap_or(0),
                "rank": r.try_get::<i32, _>("final_rank").unwrap_or(0),
                "qualified": r.try_get::<i16, _>("qualified").unwrap_or(0) == 1,
            })
        })
        .collect())
}

pub async fn live_round_events(
    st: &AppState,
    round_id: i64,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let rows = sqlx::query(
        "SELECT kind, detail, actor, at FROM live_round_event WHERE round_id = $1 ORDER BY at, id",
    )
    .bind(round_id)
    .fetch_all(w(st)?)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                "detail": r.try_get::<String, _>("detail").unwrap_or_default(),
                "actor": r.try_get::<String, _>("actor").unwrap_or_default(),
                "at": r.try_get::<chrono::DateTime<chrono::Utc>, _>("at")
                    .map(|t| t.to_rfc3339()).unwrap_or_default(),
            })
        })
        .collect())
}

/// 某局的最后一拍采样（实时榜用）
pub async fn live_latest_scores(st: &AppState, round_id: i64) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    sqlx::query(
        "SELECT DISTINCT ON (douyin_uid) douyin_uid, score, rank, team_score, team_rank, observed_at
         FROM live_score_sample WHERE round_id = $1 ORDER BY douyin_uid, observed_at DESC",
    )
    .bind(round_id)
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.iter()
            .map(|r| {
                serde_json::json!({
                    "douyinUid": r.try_get::<String, _>("douyin_uid").unwrap_or_default(),
                    "score": r.try_get::<i64, _>("score").unwrap_or(0),
                    "rank": r.try_get::<i32, _>("rank").unwrap_or(0),
                    "teamScore": r.try_get::<i64, _>("team_score").unwrap_or(0),
                    "teamRank": r.try_get::<i32, _>("team_rank").unwrap_or(0),
                    "observedAt": r.try_get::<chrono::DateTime<chrono::Utc>, _>("observed_at")
                        .map(|t| t.to_rfc3339()).unwrap_or_default(),
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

/* ------------------------------ 监控房间 ------------------------------ */

pub async fn live_monitor_upsert(
    st: &AppState,
    event_id: i64,
    web_rid: &str,
    url: &str,
    label: &str,
) -> anyhow::Result<i64> {
    let row = sqlx::query(&crate::pq::rewrite(
        "INSERT INTO live_monitor_room (event_id, web_rid, url, label, enabled)
         VALUES (?, ?, ?, ?, 1)
         ON CONFLICT (event_id, web_rid) DO UPDATE SET url = EXCLUDED.url,
           label = CASE WHEN EXCLUDED.label = '' THEN live_monitor_room.label ELSE EXCLUDED.label END
         RETURNING id",
    ))
    .bind(event_id)
    .bind(web_rid)
    .bind(url)
    .bind(label)
    .fetch_one(w(st)?)
    .await?;
    Ok(row.try_get::<i64, _>("id").unwrap_or(0))
}

/// 采集心跳：更新健康度与最后可见时间
pub async fn live_monitor_heartbeat(
    st: &AppState,
    event_id: i64,
    obs: &Observation,
) -> anyhow::Result<()> {
    let health = if obs.health == "ok" && obs.room_online {
        "live"
    } else if obs.health == "error" {
        "degraded"
    } else {
        "idle"
    };
    sqlx::query(&crate::pq::rewrite(
        "UPDATE live_monitor_room SET health = ?, last_seen_at = now(), last_error = ?
         WHERE event_id = ? AND web_rid = ?",
    ))
    .bind(health)
    .bind(obs.error.chars().take(200).collect::<String>())
    .bind(event_id)
    .bind(&obs.web_rid)
    .execute(w(st)?)
    .await?;
    Ok(())
}

pub async fn live_monitor_list(st: &AppState, event_id: i64) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    sqlx::query(
        "SELECT id, web_rid, room_id, url, label, enabled, health, last_seen_at, last_error
         FROM live_monitor_room WHERE event_id = $1 ORDER BY id",
    )
    .bind(event_id)
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.try_get::<i64, _>("id").unwrap_or(0),
                    "webRid": r.try_get::<String, _>("web_rid").unwrap_or_default(),
                    "roomId": r.try_get::<String, _>("room_id").unwrap_or_default(),
                    "url": r.try_get::<String, _>("url").unwrap_or_default(),
                    "label": r.try_get::<String, _>("label").unwrap_or_default(),
                    "enabled": r.try_get::<i16, _>("enabled").unwrap_or(0) == 1,
                    "health": r.try_get::<String, _>("health").unwrap_or_default(),
                    "lastSeenAt": r.try_get::<chrono::DateTime<chrono::Utc>, _>("last_seen_at")
                        .map(|t| t.to_rfc3339()).ok(),
                    "lastError": r.try_get::<String, _>("last_error").unwrap_or_default(),
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

/* ------------------------------ 晋级结果 ------------------------------ */

/// 冻结一条晋级结果（幂等：唯一键 (event, source, stage, group, uid)）
#[allow(clippy::too_many_arguments)]
pub async fn live_qualifier_add(
    st: &AppState,
    event_id: i64,
    stage_id: i64,
    group_id: i64,
    round_id: i64,
    person_id: i64,
    douyin_uid: &str,
    source: &str,
    rank: i32,
    score: i64,
) -> anyhow::Result<()> {
    sqlx::query(&crate::pq::rewrite(
        "INSERT INTO live_qualifier (event_id, stage_id, group_id, round_id, person_id,
                douyin_uid, source, rank, score)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (event_id, source, stage_id, group_id, douyin_uid) DO UPDATE SET
           rank = EXCLUDED.rank, score = EXCLUDED.score, round_id = EXCLUDED.round_id,
           person_id = CASE WHEN live_qualifier.person_id = 0 THEN EXCLUDED.person_id
                            ELSE live_qualifier.person_id END",
    ))
    .bind(event_id)
    .bind(stage_id)
    .bind(group_id)
    .bind(round_id)
    .bind(person_id)
    .bind(douyin_uid)
    .bind(source)
    .bind(rank)
    .bind(score)
    .execute(w(st)?)
    .await?;
    Ok(())
}

pub async fn live_qualifier_list(st: &AppState, event_id: i64) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    sqlx::query(
        "SELECT q.stage_id, q.group_id, q.round_id, q.person_id, q.douyin_uid, q.source,
                q.rank, q.score, COALESCE(p.name, '') AS person_name, s.kind AS stage_kind
         FROM live_qualifier q
         LEFT JOIN person p ON p.id = q.person_id
         LEFT JOIN live_stage s ON s.id = q.stage_id
         WHERE q.event_id = $1
         ORDER BY s.seq, q.group_id, q.rank, q.score DESC",
    )
    .bind(event_id)
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.iter()
            .map(|r| {
                serde_json::json!({
                    "stageId": r.try_get::<i64, _>("stage_id").unwrap_or(0),
                    "stageKind": r.try_get::<String, _>("stage_kind").unwrap_or_default(),
                    "groupId": r.try_get::<i64, _>("group_id").unwrap_or(0),
                    "roundId": r.try_get::<i64, _>("round_id").unwrap_or(0),
                    "personId": r.try_get::<i64, _>("person_id").unwrap_or(0),
                    "personName": r.try_get::<String, _>("person_name").unwrap_or_default(),
                    "douyinUid": r.try_get::<String, _>("douyin_uid").unwrap_or_default(),
                    "source": r.try_get::<String, _>("source").unwrap_or_default(),
                    "rank": r.try_get::<i32, _>("rank").unwrap_or(0),
                    "score": r.try_get::<i64, _>("score").unwrap_or(0),
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

/* ------------------------------ 认人 ------------------------------ */

/// 抖音 UID → person_id（走 account.anchor_id，昵称只作兜底）
pub async fn live_person_by_uid(st: &AppState, uid: &str) -> Option<i64> {
    if uid.is_empty() {
        return None;
    }
    let pool = w(st).ok()?;
    sqlx::query(&crate::pq::rewrite(
        "SELECT person_id FROM account WHERE anchor_id = ? LIMIT 1",
    ))
    .bind(uid)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .and_then(|r| r.try_get::<i64, _>("person_id").ok())
    .filter(|v| *v > 0)
}

/* ------------------------------ 对局定位辅助 ------------------------------ */

/// 这个 battle_id 已经落在哪个小组（0 = 还没有）
pub async fn live_round_group_of(st: &AppState, battle_id: &str) -> anyhow::Result<Option<i64>> {
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT group_id FROM live_round WHERE battle_id = ? ORDER BY id LIMIT 1",
    ))
    .bind(battle_id)
    .fetch_optional(w(st)?)
    .await?;
    Ok(row.and_then(|r| r.try_get::<i64, _>("group_id").ok()))
}

/// 当前阶段里第一个未定稿小组（运营按赛程顺序开打，所以这就是该打的那组）
pub async fn live_next_open_group(st: &AppState, event_id: i64) -> anyhow::Result<i64> {
    let row = sqlx::query(
        "SELECT g.id FROM live_group g JOIN live_stage s ON s.id = g.stage_id
         WHERE g.event_id = $1 AND s.status <> 'finalized' AND g.status NOT IN ('finalized')
         ORDER BY s.seq, g.seq LIMIT 1",
    )
    .bind(event_id)
    .fetch_optional(w(st)?)
    .await?;
    Ok(row
        .and_then(|r| r.try_get::<i64, _>("id").ok())
        .unwrap_or(0))
}

/// 该组已有几局
pub async fn live_round_count(st: &AppState, group_id: i64) -> anyhow::Result<i64> {
    let row = sqlx::query("SELECT COUNT(*)::BIGINT AS n FROM live_round WHERE group_id = $1")
        .bind(group_id)
        .fetch_one(w(st)?)
        .await?;
    Ok(row.try_get::<i64, _>("n").unwrap_or(0))
}

/// 该组最近一局 id
pub async fn live_latest_round_id(st: &AppState, group_id: i64) -> anyhow::Result<i64> {
    let row = sqlx::query("SELECT id FROM live_round WHERE group_id = $1 ORDER BY id DESC LIMIT 1")
        .bind(group_id)
        .fetch_optional(w(st)?)
        .await?;
    Ok(row
        .and_then(|r| r.try_get::<i64, _>("id").ok())
        .unwrap_or(0))
}

/// 该组已正常完成的局（判定「同组两场完整局」用）
pub async fn live_completed_rounds(
    st: &AppState,
    group_id: i64,
) -> anyhow::Result<Vec<(i64, i64)>> {
    let rows = sqlx::query(
        "SELECT id, COALESCE(EXTRACT(EPOCH FROM actual_end_at) * 1000, 0)::BIGINT AS ended
         FROM live_round WHERE group_id = $1 AND status = 'completed' ORDER BY id",
    )
    .bind(group_id)
    .fetch_all(w(st)?)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            (
                r.try_get::<i64, _>("id").unwrap_or(0),
                r.try_get::<i64, _>("ended").unwrap_or(0),
            )
        })
        .collect())
}

/// 标记席位是否晋级（复活池靠 `qualified = 0` 取淘汰者）
pub async fn live_seat_mark_qualified(
    st: &AppState,
    round_id: i64,
    uid: &str,
    qualified: bool,
) -> anyhow::Result<()> {
    sqlx::query(&crate::pq::rewrite(
        "UPDATE live_round_seat SET qualified = ? WHERE round_id = ? AND douyin_uid = ?",
    ))
    .bind(if qualified { 1i16 } else { 0i16 })
    .bind(round_id)
    .bind(uid)
    .execute(w(st)?)
    .await?;
    Ok(())
}

/// 读对局当前状态（finalize 的幂等判断用）
pub async fn live_round_status(
    st: &AppState,
    round_id: i64,
) -> anyhow::Result<Option<RoundStatus>> {
    let row = sqlx::query("SELECT status FROM live_round WHERE id = $1")
        .bind(round_id)
        .fetch_optional(w(st)?)
        .await?;
    let Some(r) = row else {
        return Ok(None);
    };
    let v = r.try_get::<String, _>("status").unwrap_or_default();
    Ok(Some(match v.as_str() {
        "detected" => RoundStatus::Detected,
        "live" => RoundStatus::Live,
        "settling" => RoundStatus::Settling,
        "completed" => RoundStatus::Completed,
        "aborted" => RoundStatus::Aborted,
        "pending_review" => RoundStatus::PendingReview,
        "void" => RoundStatus::Void,
        _ => RoundStatus::Detected,
    }))
}

pub async fn live_group_official_round(
    st: &AppState,
    group_id: i64,
) -> anyhow::Result<Option<i64>> {
    let row = sqlx::query("SELECT official_round_id FROM live_group WHERE id = $1")
        .bind(group_id)
        .fetch_optional(w(st)?)
        .await?;
    Ok(row
        .and_then(|r| r.try_get::<i64, _>("official_round_id").ok())
        .filter(|v| *v > 0))
}

pub async fn live_group_qualify_slots(st: &AppState, group_id: i64) -> anyhow::Result<Option<i32>> {
    let row = sqlx::query("SELECT qualify_slots FROM live_group WHERE id = $1")
        .bind(group_id)
        .fetch_optional(w(st)?)
        .await?;
    Ok(row.and_then(|r| r.try_get::<i32, _>("qualify_slots").ok()))
}

pub async fn live_group_stage(st: &AppState, group_id: i64) -> anyhow::Result<i64> {
    let row = sqlx::query("SELECT stage_id FROM live_group WHERE id = $1")
        .bind(group_id)
        .fetch_optional(w(st)?)
        .await?;
    Ok(row
        .and_then(|r| r.try_get::<i64, _>("stage_id").ok())
        .unwrap_or(0))
}

/* ------------------------------ 赛事 / 阶段辅助 ------------------------------ */

pub async fn live_stage_list(st: &AppState, event_id: i64) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    sqlx::query(
        "SELECT id, kind, seq, status, started_at, finished_at
         FROM live_stage WHERE event_id = $1 ORDER BY seq",
    )
    .bind(event_id)
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.try_get::<i64, _>("id").unwrap_or(0),
                    "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                    "seq": r.try_get::<i32, _>("seq").unwrap_or(0),
                    "status": r.try_get::<String, _>("status").unwrap_or_default(),
                    "startedAt": r.try_get::<chrono::DateTime<chrono::Utc>, _>("started_at")
                        .map(|t| t.to_rfc3339()).ok(),
                    "finishedAt": r.try_get::<chrono::DateTime<chrono::Utc>, _>("finished_at")
                        .map(|t| t.to_rfc3339()).ok(),
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

pub async fn live_stage_id_of(
    st: &AppState,
    event_id: i64,
    kind: StageKind,
) -> anyhow::Result<i64> {
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT id FROM live_stage WHERE event_id = ? AND kind = ?",
    ))
    .bind(event_id)
    .bind(kind.as_str())
    .fetch_optional(w(st)?)
    .await?;
    Ok(row
        .and_then(|r| r.try_get::<i64, _>("id").ok())
        .unwrap_or(0))
}

/// 是否已有定稿阶段（改规则的拦截条件）
pub async fn live_has_finalized_stage(st: &AppState, event_id: i64) -> anyhow::Result<bool> {
    let row = sqlx::query(
        "SELECT COUNT(*)::BIGINT AS n FROM live_stage WHERE event_id = $1 AND status = 'finalized'",
    )
    .bind(event_id)
    .fetch_one(w(st)?)
    .await?;
    Ok(row.try_get::<i64, _>("n").unwrap_or(0) > 0)
}

pub async fn live_event_day_plan(st: &AppState, event_id: i64) -> anyhow::Result<i64> {
    let row = sqlx::query("SELECT day_plan_id FROM live_event WHERE id = $1")
        .bind(event_id)
        .fetch_optional(w(st)?)
        .await?;
    Ok(row
        .and_then(|r| r.try_get::<i64, _>("day_plan_id").ok())
        .unwrap_or(0))
}

pub async fn live_event_of_group(st: &AppState, group_id: i64) -> anyhow::Result<i64> {
    let row = sqlx::query("SELECT event_id FROM live_group WHERE id = $1")
        .bind(group_id)
        .fetch_optional(w(st)?)
        .await?;
    Ok(row
        .and_then(|r| r.try_get::<i64, _>("event_id").ok())
        .unwrap_or(0))
}

/// 某阶段下所有小组中，未晋级的人（= 淘汰者），按名次排序
pub async fn live_stage_eliminated(st: &AppState, stage_id: i64) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    sqlx::query(
        "SELECT s.person_id, s.douyin_uid, s.nickname, s.final_score, s.final_rank, g.id AS group_id
         FROM live_round_seat s
         JOIN live_round r ON r.id = s.round_id
         JOIN live_group g ON g.id = r.group_id
         WHERE g.stage_id = $1 AND r.is_official = 1 AND s.qualified = 0
         ORDER BY s.final_rank, s.final_score DESC, s.douyin_uid",
    )
    .bind(stage_id)
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.iter()
            .map(|r| {
                serde_json::json!({
                    "personId": r.try_get::<i64, _>("person_id").unwrap_or(0),
                    "douyinUid": r.try_get::<String, _>("douyin_uid").unwrap_or_default(),
                    "nickname": r.try_get::<String, _>("nickname").unwrap_or_default(),
                    "score": r.try_get::<i64, _>("final_score").unwrap_or(0),
                    "rank": r.try_get::<i32, _>("final_rank").unwrap_or(0),
                    "groupId": r.try_get::<i64, _>("group_id").unwrap_or(0),
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

/// 对局所属赛事
pub async fn live_round_event_id(st: &AppState, round_id: i64) -> anyhow::Result<Option<i64>> {
    let row = sqlx::query("SELECT event_id FROM live_round WHERE id = $1")
        .bind(round_id)
        .fetch_optional(w(st)?)
        .await?;
    Ok(row.and_then(|r| r.try_get::<i64, _>("event_id").ok()))
}

/// 对局所属小组
pub async fn live_round_group_of_id(st: &AppState, round_id: i64) -> anyhow::Result<i64> {
    let row = sqlx::query("SELECT group_id FROM live_round WHERE id = $1")
        .bind(round_id)
        .fetch_optional(w(st)?)
        .await?;
    Ok(row
        .and_then(|r| r.try_get::<i64, _>("group_id").ok())
        .unwrap_or(0))
}

/// 小组名字（推送文案用）
pub async fn live_group_name(st: &AppState, group_id: i64) -> anyhow::Result<String> {
    let row = sqlx::query("SELECT name FROM live_group WHERE id = $1")
        .bind(group_id)
        .fetch_optional(w(st)?)
        .await?;
    Ok(row
        .and_then(|r| r.try_get::<String, _>("name").ok())
        .unwrap_or_default())
}

/// 读赛事推送开关与间隔（改设置时保留未传的那一项）
pub async fn live_event_push(st: &AppState, event_id: i64) -> anyhow::Result<(bool, i32)> {
    let row = sqlx::query("SELECT push_enabled, push_interval_secs FROM live_event WHERE id = $1")
        .bind(event_id)
        .fetch_optional(w(st)?)
        .await?
        .ok_or_else(|| anyhow::anyhow!("赛事不存在"))?;
    Ok((
        row.try_get::<i16, _>("push_enabled").unwrap_or(0) == 1,
        row.try_get::<i32, _>("push_interval_secs").unwrap_or(60),
    ))
}

/// 赛事的推送开关与间隔（第二层；第一层是 PUSH_OUT_ENABLED，
/// 第三层是 `push_bindings.live_pk_report`）
pub async fn live_event_set_push(
    st: &AppState,
    event_id: i64,
    enabled: bool,
    interval_secs: i32,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        (10..=3600).contains(&interval_secs),
        "推送间隔要在 10~3600 秒之间"
    );
    sqlx::query(&crate::pq::rewrite(
        "UPDATE live_event SET push_enabled = ?, push_interval_secs = ?, updated_at = now()
         WHERE id = ?",
    ))
    .bind(if enabled { 1i16 } else { 0i16 })
    .bind(interval_secs)
    .bind(event_id)
    .execute(w(st)?)
    .await?;
    Ok(())
}

/// 某场对局里「参赛且开了直播推送」的展示端账号（去重到 person）。
///
/// 同一个人可能挂多个抖音号（`account` 一人多号），按 `person_id` 去重，
/// 避免同一场 PK 给同一个人连发好几条。
pub async fn live_round_push_targets(st: &AppState, round_id: i64) -> Vec<PushBinding> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    sqlx::query(
        "SELECT DISTINCT ON (pb.person_id)
                pb.name, pb.team, pb.person_id, pb.conversation_id,
                pb.notify_target, pb.daily_report, pb.lineage_report, pb.last_lineage_date
         FROM live_round_seat s
         JOIN push_bindings pb
           ON pb.person_id = s.person_id
          AND pb.person_id > 0
          AND pb.conversation_id <> ''
          AND pb.live_pk_report = 1
          AND COALESCE(pb.paused, 0) = 0
         WHERE s.round_id = $1
         ORDER BY pb.person_id, pb.name",
    )
    .bind(round_id)
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.iter()
            .map(|r| PushBinding {
                name: r.try_get::<String, _>("name").unwrap_or_default(),
                team: r.try_get::<String, _>("team").unwrap_or_default(),
                person_id: r
                    .try_get::<i64, _>("person_id")
                    .map(|v| v.max(0) as u64)
                    .unwrap_or(0),
                conversation_id: r
                    .try_get::<String, _>("conversation_id")
                    .unwrap_or_default(),
                notify_target: r.try_get::<i16, _>("notify_target").unwrap_or(0) == 1,
                daily_report: r.try_get::<i16, _>("daily_report").unwrap_or(0) == 1,
                lineage_report: r.try_get::<i16, _>("lineage_report").unwrap_or(0) == 1,
                last_lineage_date: r
                    .try_get::<String, _>("last_lineage_date")
                    .unwrap_or_default(),
            })
            .collect()
    })
    .unwrap_or_else(|e| {
        tracing::error!(error = %e, round_id, "查直播推送目标失败");
        Vec::new()
    })
}

/// 主播侧开关：开 / 关直播分数推送（展示端账号名）
pub async fn live_set_person_push(st: &AppState, name: &str, on: bool) -> anyhow::Result<bool> {
    let res = sqlx::query(&crate::pq::rewrite(
        "UPDATE push_bindings SET live_pk_report = ?, updated_at = ? WHERE name = ?",
    ))
    .bind(if on { 1i16 } else { 0i16 })
    .bind(now_str())
    .bind(name.trim())
    .execute(w(st)?)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// 采集进程心跳
pub async fn live_worker_heartbeat(
    st: &AppState,
    worker_id: &str,
    version: &str,
    rooms: i32,
) -> anyhow::Result<()> {
    sqlx::query(&crate::pq::rewrite(
        "INSERT INTO live_collector_worker (worker_id, version, rooms, last_heartbeat)
         VALUES (?, ?, ?, now())
         ON CONFLICT (worker_id) DO UPDATE SET version = EXCLUDED.version,
           rooms = EXCLUDED.rooms, last_heartbeat = now()",
    ))
    .bind(worker_id)
    .bind(version)
    .bind(rooms)
    .execute(w(st)?)
    .await?;
    Ok(())
}

/// 分组方案里的分组卡（buckets）：[{name, players:[{name,...}]}]
pub async fn plan_buckets(st: &AppState, plan_id: i64) -> anyhow::Result<Vec<serde_json::Value>> {
    let row = match crate::store::get_plan(st, plan_id).await {
        Some(p) => p,
        None => anyhow::bail!("分组方案不存在"),
    };
    let payload: serde_json::Value = serde_json::from_str(&row.payload).unwrap_or(Value::Null);
    let buckets = payload
        .get("buckets")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(buckets)
}

/// 给还没开打的小组落参赛名单（复活 / 晋级分组生成时用）。
/// 与 `live_round_seat` 分开：那张表是**某一局**的实际结果，这张是**赛前谁要上场**，
/// 对局还没产生时也要能在监控页显示名单。
pub async fn live_group_seed_seat(
    st: &AppState,
    group_id: i64,
    person_id: i64,
    uid: &str,
    nickname: &str,
    order: i32,
) -> anyhow::Result<()> {
    if uid.is_empty() {
        return Ok(());
    }
    sqlx::query(&crate::pq::rewrite(
        "INSERT INTO live_group_seat (group_id, person_id, douyin_uid, nickname, seed_order)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (group_id, douyin_uid) DO UPDATE SET nickname = EXCLUDED.nickname,
           seed_order = EXCLUDED.seed_order,
           person_id = CASE WHEN live_group_seat.person_id = 0 THEN EXCLUDED.person_id
                            ELSE live_group_seat.person_id END",
    ))
    .bind(group_id)
    .bind(person_id)
    .bind(uid)
    .bind(nickname)
    .bind(order)
    .execute(w(st)?)
    .await?;
    Ok(())
}

/// 小组的赛前名单
pub async fn live_group_seats(st: &AppState, group_id: i64) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    sqlx::query(
        "SELECT person_id, douyin_uid, nickname, seed_order, COALESCE(p.name, '') AS person_name
         FROM live_group_seat s LEFT JOIN person p ON p.id = s.person_id
         WHERE group_id = $1 ORDER BY seed_order, id",
    )
    .bind(group_id)
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.iter()
            .map(|r| {
                serde_json::json!({
                    "personId": r.try_get::<i64, _>("person_id").unwrap_or(0),
                    "douyinUid": r.try_get::<String, _>("douyin_uid").unwrap_or_default(),
                    "nickname": r.try_get::<String, _>("nickname").unwrap_or_default(),
                    "personName": r.try_get::<String, _>("person_name").unwrap_or_default(),
                    "seedOrder": r.try_get::<i32, _>("seed_order").unwrap_or(0),
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

/// 小组下所有对局（争议局并排对比用）
pub async fn live_round_list(st: &AppState, group_id: i64) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    sqlx::query(
        "SELECT id, battle_id, round_kind, attempt_no, parent_round_id, status, is_official,
                end_reason, roster_signature, start_at, actual_end_at
         FROM live_round WHERE group_id = $1 ORDER BY id",
    )
    .bind(group_id)
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.try_get::<i64, _>("id").unwrap_or(0),
                    "battleId": r.try_get::<String, _>("battle_id").unwrap_or_default(),
                    "roundKind": r.try_get::<String, _>("round_kind").unwrap_or_default(),
                    "attemptNo": r.try_get::<i32, _>("attempt_no").unwrap_or(1),
                    "parentRoundId": r.try_get::<i64, _>("parent_round_id").unwrap_or(0),
                    "status": r.try_get::<String, _>("status").unwrap_or_default(),
                    "isOfficial": r.try_get::<i16, _>("is_official").unwrap_or(0) == 1,
                    "endReason": r.try_get::<String, _>("end_reason").unwrap_or_default(),
                    "rosterSignature": r.try_get::<String, _>("roster_signature").unwrap_or_default(),
                    "startAt": r.try_get::<chrono::DateTime<chrono::Utc>, _>("start_at")
                        .map(|t| t.to_rfc3339()).ok(),
                    "actualEndAt": r.try_get::<chrono::DateTime<chrono::Utc>, _>("actual_end_at")
                        .map(|t| t.to_rfc3339()).ok(),
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

/* ------------------------------ 推送作业 ------------------------------ */

/// 排一条推送（去重键撞了就不排，返回 0）
pub async fn live_push_enqueue(
    st: &AppState,
    event_id: i64,
    round_id: i64,
    person_id: i64,
    kind: &str,
    dedup_key: &str,
) -> i64 {
    let Ok(pool) = w(st) else {
        return 0;
    };
    sqlx::query(&crate::pq::rewrite(
        "INSERT INTO live_push_job (event_id, round_id, person_id, kind, dedup_key)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (dedup_key) DO NOTHING RETURNING id",
    ))
    .bind(event_id)
    .bind(round_id)
    .bind(person_id)
    .bind(kind)
    .bind(dedup_key)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .and_then(|r| r.try_get::<i64, _>("id").ok())
    .unwrap_or(0)
}

/// 取一批待发（pending 且已到点）
pub async fn live_push_take(st: &AppState, limit: i64) -> Vec<(i64, i64, i64, String)> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    sqlx::query(
        "UPDATE live_push_job SET status = 'running', attempts = attempts + 1
         WHERE id IN (
           SELECT id FROM live_push_job
           WHERE status = 'pending' AND run_at <= now() ORDER BY run_at LIMIT $1
           FOR UPDATE SKIP LOCKED
         ) RETURNING id, event_id, person_id, kind",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.iter()
            .map(|r| {
                (
                    r.try_get::<i64, _>("id").unwrap_or(0),
                    r.try_get::<i64, _>("event_id").unwrap_or(0),
                    r.try_get::<i64, _>("person_id").unwrap_or(0),
                    r.try_get::<String, _>("kind").unwrap_or_default(),
                )
            })
            .collect()
    })
    .unwrap_or_default()
}

/// 结束一条作业。`ok=false` 时回到 pending（保留补发机会），不是丢弃。
pub async fn live_push_finish(st: &AppState, id: i64, ok: bool, error: &str) {
    if let Ok(pool) = w(st) {
        let _ = sqlx::query(
            "UPDATE live_push_job SET status = $1, sent_at = CASE WHEN $1 = 'sent' THEN now() ELSE sent_at END,
                    last_error = $2 WHERE id = $3",
        )
        .bind(if ok { "sent" } else { "pending" })
        .bind(error.chars().take(200).collect::<String>())
        .bind(id)
        .execute(pool)
        .await;
    }
}

/// 绑定分组方案（白天小组赛 / 晚上晋级赛）。
///
/// `plans` 只是**赛前快照**：这里只记 id，实时分数一律不写回 `plans.payload`
/// （每几秒刷一次会把运营的分组卡覆盖掉）。
pub async fn live_event_bind_plans(
    st: &AppState,
    event_id: i64,
    day_plan_id: Option<i64>,
    night_plan_id: Option<i64>,
) -> anyhow::Result<()> {
    let mut builder =
        sqlx::QueryBuilder::<sqlx::Postgres>::new("UPDATE live_event SET updated_at = now()");
    if let Some(v) = day_plan_id {
        builder.push(", day_plan_id = ").push_bind(v);
    }
    if let Some(v) = night_plan_id {
        builder.push(", night_plan_id = ").push_bind(v);
    }
    builder.push(" WHERE id = ").push_bind(event_id);
    builder.build().execute(w(st)?).await?;
    Ok(())
}
