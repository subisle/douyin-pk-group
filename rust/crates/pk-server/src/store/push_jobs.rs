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
///
/// 2026-10-14 审计 P0-3：抢占时**必须把 N 带进 running**（`failed:N` → `running:N`）。
/// 原实现一律写成 `running`，而重试次数只存在 status 里（push_job_finish 靠解析
/// status 反推）→ N 永久丢失、attempts 恒 0 → 永远写回 `failed:1`，
/// 失败任务每 5 分钟被重新捞出来重跑，**永不进终态**。
pub async fn push_job_take(st: &AppState, now: &str) -> Option<(i64, String, String)> {
    let pool = w(st).ok()?;
    /* 2026-10-13 审计修复：列在部分环境是 varchar、部分是 timestamptz（灌库丢类型）。
    原写法 run_at <= ?（varchar 环境隐式转换 OK）但 created_at < ?::timestamptz
    在 varchar 环境直接报 operator 不存在且被 .ok()? 吞掉 → 调度器静默瘫痪。
    改为 NULLIF(col,'')::timestamptz 统一转换 + SQL 端 now()，两种列型都成立。

    2026-10-17 审计 6-P2-2（承第二轮 P2-4）：running 的 stale 判定改看 **taken_at**
    （进入 running 的时刻，迁移 034）。原先用 created_at（入队时间）——
    它通常比 run_at 早 30 分钟，任务一旦 running 该条件恒真，第二个调度循环
    60 秒后就把它当死任务重抢 → 长任务（150 人图片推送）必被并发重跑、
    主播重复收推送。 */
    let row = match sqlx::query(
        "SELECT id, team, biz_date, status FROM push_jobs
         WHERE (status = 'pending' AND NULLIF(run_at,'')::timestamptz <= now())
            OR (status LIKE 'running%' AND done_at = '' AND NULLIF(run_at,'')::timestamptz <= now()
                AND (NULLIF(taken_at,'') IS NULL
                     OR NULLIF(taken_at,'')::timestamptz < now() - INTERVAL '5 minutes'))
            OR (status LIKE 'failed:%' AND done_at <> '' AND NULLIF(done_at,'')::timestamptz <= now()
                - make_interval(mins => 5 * (1 << LEAST(
                     GREATEST(SUBSTRING(status FROM 8)::int - 1, 0), 5))))
         ORDER BY run_at LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return None,
        Err(e) => {
            /* 2026-10-14 审计 P0-3：这条查询是全系统唯一决定「今天所有延迟推送还会不会跑」
            的查询。原来 `.ok().flatten()?` 把 SQL 错误吞成「队列为空」且**零日志** ——
            调度器瘫痪了运维也看不见。现在显式报错：本轮不取任务，60 秒后下一轮重试。 */
            tracing::error!(error = %e, now, "推送队列扫描失败，本轮不取任务（下轮重试）");
            return None;
        }
    };
    let id = row.try_get::<i64, _>("id").unwrap_or(0);
    let team: String = row.try_get::<String, _>("team").unwrap_or_default();
    let biz_date: String = row.try_get::<String, _>("biz_date").unwrap_or_default();
    let prev_status: String = row.try_get::<String, _>("status").unwrap_or_default();
    if id == 0 {
        return None;
    }
    /* 抢占用 CAS（`AND status = ?`）：把上面读到的状态原样写回去比对，
    多实例并发时只有一个能改成 running。同时把已重试次数带过去（failed:N → running:N）。
    2026-10-17 审计 6-P2-2：CAS 同值 UPDATE（'running'→'running'）在 PG 里
    rows_affected 仍为 1，抢不占不住 —— 现在 UPDATE 顺带刷新 taken_at，
    stale 分支的抢占必然伴随 taken_at 变化，两个实例只有一个能把 5 分钟窗口重置。 */
    let next_status = running_status_for(&prev_status);
    let taken = match sqlx::query(&crate::pq::rewrite(
        "UPDATE push_jobs SET status = ?, taken_at = ? WHERE id = ? AND status = ?",
    ))
    .bind(&next_status)
    .bind(now_str())
    .bind(id)
    .bind(&prev_status)
    .execute(pool)
    .await
    {
        Ok(r) => r.affected() > 0,
        Err(e) => {
            tracing::error!(error = %e, job_id = id, "抢占推送任务失败");
            false
        }
    };
    if !taken {
        return None;
    }
    Some((id, team, biz_date))
}

/// 从 `push_jobs.status` 反推「已重试次数」（纯函数）。
///
/// 口径（重试次数不新增列，只编码在 status 里，见 push_job_finish）：
/// - `pending` / `done` / 终态 `failed` → 0
/// - `failed:N` / `running:N` → N（N = 已重试次数，0..=MAX_JOB_RETRIES）
/// - 空串 / 其它脏值 → 0（保守：当没重试过，最多多跑一次而不是卡死）
pub fn job_attempts_from_status(status: &str) -> u32 {
    match status.split_once(':') {
        Some((head, tail)) if head == "failed" || head == "running" => {
            tail.trim().parse::<u32>().unwrap_or(0)
        }
        _ => 0,
    }
}

/// 抢占时写回的 running 状态：把 `failed:N` 的 N 原样带过去，别丢重试计数。
fn running_status_for(status: &str) -> String {
    match job_attempts_from_status(status) {
        0 => "running".to_string(),
        n => format!("running:{n}"),
    }
}

/// finish 时算下一个状态（纯函数）：
/// - 成功 → 终态 `done`
/// - 失败且已重试次数 < 上限 → `failed:N+1`（退避后由 push_job_take 回捞）
/// - 失败且额度用完 → 终态 `failed`（不带冒号，扫描 SQL 的 `LIKE 'failed:%'` 不再命中）
fn finish_status(status: &str, ok: bool) -> String {
    if ok {
        return "done".to_string();
    }
    let attempts = job_attempts_from_status(status);
    if attempts < MAX_JOB_RETRIES {
        format!("failed:{}", attempts + 1)
    } else {
        "failed".to_string()
    }
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
    /* 2026-10-17 修复（审计 1-P2-5）：原先只数两列（notify_target / daily_report），
    而推送引擎取人（wxpush.rs 的 push_bindings_for_team）是**三列**
    （还 OR 了 lineage_report=1）。师门日报已开启的人被漏掉 → 后台统计系统性偏低，
    运营看到的「开了推送的人数」比实际少。这里补上第三列，与引擎同口径。 */
    let enabled: i64 = sqlx::query(&crate::pq::rewrite(
        "SELECT COUNT(*) AS n FROM push_bindings \
         WHERE conversation_id <> '' \
           AND (notify_target = 1 OR daily_report = 1 OR lineage_report = 1)",
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
///
/// 2026-10-14 审计 P0-3：次数现在还可能出现在 `running:N` 里（take 抢占时带过来的）。
/// 原来只认 `failed:%`，且用 SQL 的 CASE 读成 i64（CASE 实际是 **integer**，
/// 类型不匹配被 `.ok().flatten().unwrap_or(0)` 吞成 0）→ attempts 恒 0。
/// 改成：读回 status 字符串，用 [`job_attempts_from_status`] 在 Rust 侧解析。
pub async fn push_job_finish(st: &AppState, id: i64, ok: bool, note: &str) {
    if let Ok(pool) = w(st) {
        // 读当前状态（pending=0，failed:N=N，running:N=N；终态 done/failed 也按 0 读，
        // 但对终态来说 ok=true 时覆盖成 done 无害，ok=false 时只有任务不存在才会走到）
        let status: Option<String> = match sqlx::query(&crate::pq::rewrite(
            "SELECT status FROM push_jobs WHERE id = ?",
        ))
        .bind(id)
        .fetch_optional(pool)
        .await
        {
            Ok(Some(r)) => r.try_get::<String, _>(0).ok(),
            Ok(None) => None,
            Err(e) => {
                // 读状态失败不能静默按 0 算：那会让「已重试 3 次」的任务被判成首跑，
                // 回到无限重试。显式记 error（下面仍按 0 次保守处理）。
                tracing::error!(error = %e, job_id = id, "读推送任务状态失败，按 0 次重试处理");
                None
            }
        };
        let next_status = finish_status(status.as_deref().unwrap_or_default(), ok);
        if let Err(e) = sqlx::query(&crate::pq::rewrite(
            "UPDATE push_jobs SET status = ?, done_at = ?, note = ? WHERE id = ?",
        ))
        .bind(next_status)
        .bind(now_str())
        .bind(note.chars().take(200).collect::<String>())
        .bind(id)
        .execute(pool)
        .await
        {
            // 写回失败 = 任务卡在 running:N，必须看得见（原来是 `let _ =` 静默丢弃）
            tracing::error!(error = %e, job_id = id, ok, "回写推送任务终态失败");
        }
    }
}

/// 修复5：单条推送任务的最大自动重试次数（不含首跑）。
pub const MAX_JOB_RETRIES: u32 = 3;

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

/* 2026-10-17（审计 1-P2-6）：删掉 `target_count` / `target_add` 两个**死函数**。
它们是 2026-10-15 之前的旧口径：
  · `target_count` 数的是**总条数**，而现在上限是**分类独立**计数
    （追人 / 分数 / 名次各自 ≤5，见 `wxpush::target_counts`）；
  · `target_add` 无事务、无判重、无上限校验。
两者都没有生产调用方（唯一写入口是 `wx::my_push_target_add` →
`wxpush::targets_add_serialized`，后者在事务内判重 + 按 kind 判单类上限 + 并发串行化）。
留着谁图省事直接调就会绕过分类上限与并发保护，复现 FIX1 的老问题。 */

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

/// 新增目标请走 [`crate::store::wxpush::targets_add_serialized`]：
/// 事务内判重 + 按 kind 判单类上限 + 并发串行化（旧的无校验 `target_add` 已删，
/// 见上面「审计 1-P2-6」的说明）。

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

/* ------------------------------ 单元测试（纯函数） ------------------------------ */

#[cfg(test)]
mod tests {
    use super::*;

    /// 状态里编码的「已重试次数」必须能反解出来：pending/done/终态 failed 都是 0，
    /// failed:N 与 running:N 都读出 N（P0-2：SQL 的 CASE 是 int4，原来按 i64 读恒 0）。
    #[test]
    fn attempts_parsed_from_status() {
        assert_eq!(job_attempts_from_status("pending"), 0);
        assert_eq!(job_attempts_from_status("done"), 0);
        assert_eq!(
            job_attempts_from_status("failed"),
            0,
            "终态 failed 不带冒号"
        );
        assert_eq!(job_attempts_from_status("failed:1"), 1);
        assert_eq!(job_attempts_from_status("failed:3"), 3);
        assert_eq!(job_attempts_from_status("running"), 0, "首跑抢占不带后缀");
        assert_eq!(job_attempts_from_status("running:1"), 1);
        assert_eq!(job_attempts_from_status("running:2"), 2);
        // 脏值一律当 0（保守：最多多跑一次，不会卡死）
        assert_eq!(job_attempts_from_status(""), 0);
        assert_eq!(job_attempts_from_status("failed:"), 0);
        assert_eq!(job_attempts_from_status("failed:x"), 0);
        assert_eq!(job_attempts_from_status("running:-1"), 0);
    }

    /// 抢占时 N 必须原样带过去：`failed:N` → `running:N`。
    /// 这条丢了就是 P0-3 的无限重试（次数只存在 status 里，丢了就永远算出 0）。
    #[test]
    fn take_preserves_retry_count_in_running_status() {
        assert_eq!(running_status_for("pending"), "running");
        assert_eq!(running_status_for("failed:1"), "running:1");
        assert_eq!(running_status_for("failed:2"), "running:2");
        assert_eq!(running_status_for("failed:3"), "running:3");
        assert_eq!(running_status_for("running:2"), "running:2", "重复抢占幂等");
    }

    /// 连续失败到上限必须落**终态** `failed`，且第 4 次跑完后不再产生可回捞的 `failed:N`。
    /// 生命周期：pending → running → failed:1 → running:1 → failed:2 → running:2
    ///          → failed:3 → running:3 → failed（终态）
    #[test]
    fn failures_reach_terminal_state_after_retry_budget() {
        let mut status = "pending".to_string();
        let mut runs = 0;
        loop {
            // 与扫描 SQL 同口径：只回捞 pending / failed:N（`LIKE 'failed:%'`）
            let takeable = status == "pending" || status.starts_with("failed:");
            if !takeable {
                break;
            }
            status = running_status_for(&status);
            runs += 1;
            assert!(runs <= 10, "重试次数没有收敛，疑似无限重试：{status}");
            status = finish_status(&status, false);
        }
        assert_eq!(status, "failed", "额度用完应落终态 failed");
        assert!(
            !status.starts_with("failed:"),
            "终态不带冒号 → 扫描 SQL 的 LIKE 'failed:%' 不再命中"
        );
        assert_eq!(runs, 4, "首跑 + 3 次重试 = 4 次，第 5 次不再被 take");
        assert_eq!(job_attempts_from_status(&status), 0);
    }

    /// 成功一律落终态 done，且把之前累计的失败次数清掉。
    #[test]
    fn success_clears_retry_count() {
        assert_eq!(finish_status("pending", true), "done");
        assert_eq!(finish_status("running:3", true), "done");
        assert_eq!(finish_status("failed:2", true), "done");
    }

    /// 失败时按「已重试次数」递进，绝不跳号 / 回退。
    #[test]
    fn failure_increments_attempts() {
        assert_eq!(finish_status("pending", false), "failed:1");
        assert_eq!(finish_status("running", false), "failed:1");
        assert_eq!(finish_status("running:1", false), "failed:2");
        assert_eq!(finish_status("running:2", false), "failed:3");
        assert_eq!(
            finish_status("running:3", false),
            "failed",
            "额度用完 → 终态"
        );
    }

    /// 上限就是 3 次重试：改常量必须同步改这里（退避窗口按 N 算，语义别漂）。
    #[test]
    fn retry_budget_is_three() {
        assert_eq!(MAX_JOB_RETRIES, 3);
    }
}
