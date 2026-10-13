//! store::push_jobs —— 自 store.rs 按业务域拆分（对外语义不变）。

use super::*; // 共享 w() / now_str() / AppState 等内部工具

/* --------------------- 延迟推送队列（2026-10-10） --------------------- */
/*
   运营一次导入往往连着发好几个文件（男女团分开、一人多号分开），入库就推会把
   半成品榜单推给用户。改成：导入只「排队」，到点（默认 30 分钟后）再统一推一次。
   同一 (team, biz_date) 只排一队 —— 重复导入只刷新队列，不叠加。
*/

/// 排队一次推送。run_at = 到点时刻（本地时间 "YYYY-MM-DD HH:MM:SS"）。
/// 同一 (team, biz_date) 已有 pending 任务 → 只返回 id，不新排。
pub async fn push_job_enqueue(st: &AppState, team: &str, biz_date: &str, run_at: &str) -> i64 {
    let Ok(pool) = w(st) else {
        return 0;
    };
    let res = sqlx::query(&crate::pq::rewrite(
        "INSERT INTO push_jobs (team, biz_date, run_at, status, created_at)
         VALUES (?, ?, ?, 'pending', ?)
         ON CONFLICT (team, biz_date) DO UPDATE SET status = 'pending', run_at = EXCLUDED.run_at,
           done_at = '', note = ''
         RETURNING id",
    ))
    .bind(team)
    .bind(biz_date)
    .bind(run_at)
    .bind(now_str())
    .fetch_optional(pool)
    .await;
    match res {
        Ok(Some(r)) => r.try_get::<i64, _>(0).unwrap_or(0),
        Ok(None) => 0,
        Err(e) => {
            tracing::warn!(error = %e, "推送任务排队失败");
            0
        }
    }
}

/// 取一条到点的 pending 任务并立刻标成 running（抢占，多实例也只推一次）
///
/// 修复5：扫描条件同时覆盖「到点的待重试任务」（状态 `failed:N`，N=已重试次数，
/// 0..3）——它们要等退避窗口过完才算到点（done_at 存失败/重试时刻，
/// `done_at + 5*2^(N-1) 分钟 <= now` 才重新可取）。
pub async fn push_job_take(st: &AppState, now: &str) -> Option<(i64, String, String)> {
    let pool = w(st).ok()?;
    /* 2026-10-13 审计修复：列在部分环境是 varchar、部分是 timestamptz（灌库丢类型）。
       原写法 run_at <= ?（varchar 环境隐式转换 OK）但 created_at < ?::timestamptz
       在 varchar 环境直接报 operator 不存在且被 .ok()? 吞掉 → 调度器静默瘫痪。
       改为 NULLIF(col,'')::timestamptz 统一转换 + SQL 端 now()，两种列型都成立。 */
    let row = sqlx::query(
        "SELECT id, team, biz_date FROM push_jobs
         WHERE (status = 'pending' AND NULLIF(run_at,'')::timestamptz <= now())
            OR (status = 'running' AND done_at = '' AND NULLIF(run_at,'')::timestamptz <= now()
                AND created_at <> '' AND NULLIF(created_at,'')::timestamptz < now() - INTERVAL '5 minutes')
            OR (status LIKE 'failed:%' AND done_at <> '' AND NULLIF(done_at,'')::timestamptz <= now()
                - make_interval(mins => 5 * (1 << LEAST(
                     GREATEST(SUBSTRING(status FROM 8)::int - 1, 0), 5))))
         ORDER BY run_at LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    let id = row.try_get::<i64, _>("id").unwrap_or(0);
    let team: String = row.try_get::<String, _>("team").unwrap_or_default();
    let biz_date: String = row.try_get::<String, _>("biz_date").unwrap_or_default();
    if id == 0 {
        return None;
    }
    let taken = sqlx::query(&crate::pq::rewrite(
        "UPDATE push_jobs SET status = 'running'
         WHERE id = ? AND (status = 'pending' OR status LIKE 'failed:%'
                           OR (status = 'running' AND done_at = ''))",
    ))
    .bind(id)
    .execute(pool)
    .await
    .map(|r| r.affected() > 0)
    .unwrap_or(false);
    if !taken {
        return None;
    }
    Some((id, team, biz_date))
}

/// 最近推送任务（后台「机器人」页查看队列用）
pub async fn push_job_list(st: &AppState, limit: i64) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    match sqlx::query(&crate::pq::rewrite(
        "SELECT id, team, biz_date, run_at, status, created_at, done_at, note
         FROM push_jobs ORDER BY id DESC LIMIT ?",
    ))
    .bind(limit)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.try_get::<i64, _>("id").map(|v| v as u64).unwrap_or(0),
                    "team": r.try_get::<String, _>("team").unwrap_or_default(),
                    "bizDate": r.try_get::<String, _>("biz_date").unwrap_or_default(),
                    "runAt": r.try_get::<String, _>("run_at").unwrap_or_default(),
                    "status": r.try_get::<String, _>("status").unwrap_or_default(),
                    "createdAt": r.try_get::<String, _>("created_at").unwrap_or_default(),
                    "doneAt": r.try_get::<String, _>("done_at").unwrap_or_default(),
                    "note": r.try_get::<String, _>("note").unwrap_or_default(),
                })
            })
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "查推送队列失败");
            Vec::new()
        }
    }
}

/// 已绑定微信的账号数 + 开了推送开关的人数（后台概览用）
pub async fn push_stats(st: &AppState) -> serde_json::Value {
    let Ok(pool) = w(st) else {
        return serde_json::json!({ "bound": 0, "enabled": 0 });
    };
    let bound: i64 = sqlx::query(&crate::pq::rewrite(
        "SELECT COUNT(*) AS n FROM push_bindings WHERE conversation_id <> ''",
    ))
    .fetch_one(pool)
    .await
    .ok()
    .and_then(|r| r.try_get::<i64, _>("n").ok())
    .unwrap_or(0);
    let enabled: i64 = sqlx::query(&crate::pq::rewrite(
        "SELECT COUNT(*) AS n FROM push_bindings \
         WHERE conversation_id <> '' AND (notify_target = 1 OR daily_report = 1)",
    ))
    .fetch_one(pool)
    .await
    .ok()
    .and_then(|r| r.try_get::<i64, _>("n").ok())
    .unwrap_or(0);
    serde_json::json!({ "bound": bound, "enabled": enabled })
}

/// 任务结束（成功 / 失败都记一笔，失败原因写 note 供后台查看）
///
/// 修复5·重试：push_jobs 表没有 retries 列（不新增迁移），把已重试次数编码在
/// status 里（`failed:N`，N=已重试次数；成功落 done，额度用完落终态 failed）。
/// 失败且 N < 3 时标成 `failed:N+1`，done_at 记失败时刻——push_job_take 会按
/// `done_at + 5*2^(N-1) 分钟` 的退避窗口把它重新捞出来跑（5/10/20 分钟三档）。
/// note 始终保留最后一次的失败原因；后台「机器人」页能看到 N 的变化轨迹。
pub async fn push_job_finish(st: &AppState, id: i64, ok: bool, note: &str) {
    if let Ok(pool) = w(st) {
        // 修复5：读当前已重试次数（pending=0，failed:N=N；终态 done/failed 也按 0 读，
        // 但对终态来说 ok=true 时覆盖成 done 无害，ok=false 时只有任务不存在才会走到）
        let attempts: i64 = sqlx::query(&crate::pq::rewrite(
            "SELECT CASE
                   WHEN status = 'pending' THEN 0
                   WHEN status LIKE 'failed:%' THEN COALESCE(SUBSTRING(status FROM 8)::int, 0)
                   ELSE 0 END
             FROM push_jobs WHERE id = ?",
        ))
        .bind(id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get::<i64, _>(0).ok())
        .unwrap_or(0);
        // 修复5：先算好目标状态再回写。
        // ok → 终态 done（清掉失败计数）；
        // 失败且 attempts < 3 → `failed:N+1`（N=当前已重试次数），take 按退避窗口回捞；
        // 失败且额度用完 → 终态 failed，不再自动重试。
        let next_status = if ok {
            "done".to_string()
        } else if attempts < MAX_JOB_RETRIES {
            format!("failed:{}", attempts + 1)
        } else {
            "failed".to_string()
        };
        let _ = sqlx::query(&crate::pq::rewrite(
            "UPDATE push_jobs SET status = ?, done_at = ?, note = ? WHERE id = ?",
        ))
        .bind(next_status)
        .bind(now_str())
        .bind(note.chars().take(200).collect::<String>())
        .bind(id)
        .execute(pool)
        .await;
    }
}

/// 修复5：单条推送任务的最大自动重试次数（不含首跑）。
pub const MAX_JOB_RETRIES: i64 = 3;

/* --------------------------- 追分目标 --------------------------- */

/// 某账号的目标列表（展示端用）
pub async fn targets_list(st: &AppState, name: &str) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    match sqlx::query(&crate::pq::rewrite(
        "SELECT id, kind, target_person_id, target_name, target_value, hit, last_gap, created_at
         FROM my_targets WHERE name = ? ORDER BY id",
    ))
    .bind(name.trim())
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.try_get::<i64, _>("id").map(|v| v as u64).unwrap_or(0),
                    "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                    "targetPersonId": r.try_get::<i64, _>("target_person_id").map(|v| v.max(0) as u64).unwrap_or(0),
                    "targetName": r.try_get::<String, _>("target_name").unwrap_or_default(),
                    "targetValue": r.try_get::<i64, _>("target_value").unwrap_or(0),
                    "hit": r.try_get::<i16, _>("hit").unwrap_or(0) == 1,
                    "lastGap": r.try_get::<i64, _>("last_gap").unwrap_or(0),
                    "createdAt": r.try_get::<String, _>("created_at").unwrap_or_default(),
                })
            })
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "查追分目标失败");
            Vec::new()
        }
    }
}

/// 目标条数（上限校验用）
pub async fn target_count(st: &AppState, name: &str) -> i64 {
    let Ok(pool) = w(st) else {
        return 0;
    };
    sqlx::query(&crate::pq::rewrite(
        "SELECT COUNT(*) AS n FROM my_targets WHERE name = ?",
    ))
    .bind(name.trim())
    .fetch_one(pool)
    .await
    .ok()
    .and_then(|r| r.try_get::<i64, _>("n").ok())
    .unwrap_or(0)
}

/// 目标行（推送引擎用的最小集合）
#[derive(Clone, Debug)]
pub struct PushTarget {
    pub id: i64,
    pub kind: String,
    pub target_person_id: u64,
    pub target_name: String,
    pub target_value: i64,
    pub hit: bool,
    pub last_notice_date: String,
}

pub async fn targets_for_push(st: &AppState, name: &str) -> Vec<PushTarget> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    match sqlx::query(&crate::pq::rewrite(
        "SELECT id, kind, target_person_id, target_name, target_value, hit, last_notice_date
         FROM my_targets WHERE name = ? ORDER BY id",
    ))
    .bind(name.trim())
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| PushTarget {
                id: r.try_get::<i64, _>("id").unwrap_or(0),
                kind: r.try_get::<String, _>("kind").unwrap_or_default(),
                target_person_id: r
                    .try_get::<i64, _>("target_person_id")
                    .map(|v| v.max(0) as u64)
                    .unwrap_or(0),
                target_name: r.try_get::<String, _>("target_name").unwrap_or_default(),
                target_value: r.try_get::<i64, _>("target_value").unwrap_or(0),
                hit: r.try_get::<i16, _>("hit").unwrap_or(0) == 1,
                last_notice_date: r
                    .try_get::<String, _>("last_notice_date")
                    .unwrap_or_default(),
            })
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "查追分目标失败");
            Vec::new()
        }
    }
}

/// 新增目标（kind = person 追人 / value 追数值）
pub async fn target_add(
    st: &AppState,
    name: &str,
    team: &str,
    kind: &str,
    target_person_id: u64,
    target_name: &str,
    target_value: i64,
) -> Result<u64> {
    let pool = w(st)?;
    let r = sqlx::query(&crate::pq::rewrite(
        "INSERT INTO my_targets (name, team, kind, target_person_id, target_name, target_value, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING id",
    ))
    .bind(name.trim())
    .bind(team)
    .bind(kind)
    .bind(target_person_id as i64)
    .bind(target_name)
    .bind(target_value)
    .bind(now_str())
    .fetch_one(pool)
    .await?;
    Ok(r.try_get::<i64, _>(0)? as u64)
}

/// 删除目标（只能删自己的）
pub async fn target_delete(st: &AppState, name: &str, id: i64) -> bool {
    matches!(w(st), Ok(pool) if sqlx::query(&crate::pq::rewrite("DELETE FROM my_targets WHERE id = ? AND name = ?"))
        .bind(id)
        .bind(name.trim())
        .execute(pool)
        .await
        .map(|r| r.affected() > 0)
        .unwrap_or(false))
}

/// 推送后回写目标状态（是否已追上 / 上次差距 / 上次提醒的业务日）
pub async fn target_set_state(st: &AppState, id: i64, hit: bool, gap: i64, notice_date: &str) {
    if let Ok(pool) = w(st) {
        let _ = sqlx::query(&crate::pq::rewrite(
            "UPDATE my_targets SET hit = ?, last_gap = ?, last_notice_date = ? WHERE id = ?",
        ))
        .bind(if hit { 1i16 } else { 0i16 })
        .bind(gap)
        .bind(notice_date)
        .bind(id)
        .execute(pool)
        .await;
    }
}

/// 本团主播搜索（展示端追分选择用）：只回 id + 名字，不下发抖音号等敏感字段
pub async fn team_anchor_search(
    st: &AppState,
    source: &str,
    team: &str,
    q: &str,
    limit: i64,
) -> Vec<serde_json::Value> {
    let sql = "SELECT id, name FROM person \
               WHERE deleted_at IS NULL AND (? = '' OR gender = ?) AND status <> 'left' AND name LIKE ? \
               ORDER BY name LIMIT ?";
    let like = format!("%{}%", q.trim());
    match crate::pgrs::query_with_binds(&st.pools, source, sql, |qb| {
        qb.bind(team.to_string())
            .bind(team.to_string())
            .bind(like.clone())
            .bind(limit)
    })
    .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "personId": r.try_get::<i64, _>("id").unwrap_or(0),
                    "name": r.try_get::<String, _>("name").unwrap_or_default(),
                })
            })
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "本团主播搜索失败");
            Vec::new()
        }
    }
}
