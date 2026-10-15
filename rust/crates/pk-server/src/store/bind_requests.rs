//! store::bind_requests —— 自 store.rs 按业务域拆分（对外语义不变）。

use super::*; // 共享 w() / now_str() / AppState 等内部工具

/* --------------------- 绑定申请（my_bind_requests，017，审核制） ---------------------
 *
 * 2026-10-12 安全审计：旧链路「自报 personId 直接绑」可冒领（check-name / bind/search
 * 都泄露内部 personId）。改为：用户只提交姓名 → 服务端精确查台账 → 落 pending 申请 →
 * 管理员审核通过才调 my_account_bind 真正绑定。表结构见 migrations/017_my_bind_requests.sql。
 */

/// 绑定申请状态机（纯函数）：只有 pending 能被裁决。
/// 与 `my_bind_request_decide` 里 SQL 的 `WHERE status = 'pending'` 原子翻转同口径 —
/// 双重保证：先抢裁决权（并发双击只有一刀成功），状态字符串只从这一处来。
pub fn bind_request_next_status(current: &str, approve: bool) -> Option<&'static str> {
    if current != "pending" {
        return None;
    }
    Some(if approve { "approved" } else { "rejected" })
}

/// 表还没跑 017 迁移时的统一文案（表不存在的报错文案里带 "doesn't exist"）
fn bind_requests_table_err(e: impl std::fmt::Display) -> String {
    let m = e.to_string();
    if m.contains("doesn't exist") {
        "缺少数据库迁移 017（先跑一次 pk-server migrate）".to_string()
    } else {
        format!("绑定申请写入失败：{m}")
    }
}

/// 提交绑定申请（pending）。返回 Ok(true) = 已有 pending（幂等，前端重复提交不报错）。
/// 调用方已校验：账号存在且未绑定、主播在台账且未被他人绑定。
pub async fn my_bind_request_submit(
    st: &AppState,
    account: &str,
    person_id: i64,
    person_name: &str,
    team: &str,
) -> std::result::Result<bool, String> {
    let pool = w(st).map_err(|e| e.to_string())?;
    let pending = sqlx::query(&crate::pq::rewrite(
        "SELECT id FROM my_bind_requests WHERE name = ? AND status = 'pending' LIMIT 1",
    ))
    .bind(account)
    .fetch_optional(pool)
    .await
    .map_err(bind_requests_table_err)?
    .is_some();
    if pending {
        return Ok(true);
    }
    sqlx::query(&crate::pq::rewrite(
        "INSERT INTO my_bind_requests (name, person_id, person_name, team, status) VALUES (?, ?, ?, ?, 'pending')",
    ))
    .bind(account)
    .bind(person_id)
    .bind(person_name)
    .bind(team)
    .execute(pool)
    .await
    .map_err(bind_requests_table_err)?;
    Ok(false)
}

/// `my_bind_requests.created_at / decided_at` 是 **TIMESTAMPTZ**（001_core.sql:396），
/// sqlx 的 `String` 解码白名单不含该类型 —— 用 `try_get::<String>` 读必报
/// ColumnDecode，再被 `.unwrap_or_default()` 吞成空串。
/// 2026-10-14 审计修复：latest 路径就是这么漏改的（list 路径已在上一轮改成 chrono）。
/// 抽成公共函数，两条路径共用，杜绝再次分叉。
fn fmt_ts(row: &sqlx::postgres::PgRow, col: &str) -> String {
    row.try_get::<chrono::DateTime<chrono::Utc>, _>(col)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// 该账号最新一条绑定申请（展示端「我的」轮询审核进度用）；没有申请 / 017 未跑 → None
pub async fn my_bind_request_latest(st: &AppState, account: &str) -> Option<serde_json::Value> {
    let pool = w(st).ok()?;
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT status, person_name, reason, created_at FROM my_bind_requests \
         WHERE name = ? ORDER BY id DESC LIMIT 1",
    ))
    .bind(account)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    Some(serde_json::json!({
        "status": row.try_get::<String, _>("status").unwrap_or_default(),
        "personName": row.try_get::<String, _>("person_name").unwrap_or_default(),
        "reason": row.try_get::<String, _>("reason").unwrap_or_default(),
        "createdAt": fmt_ts(&row, "created_at"),
    }))
}

/// 审核列表（管理端）。status 空 = 全部（pending 排最前）。
pub async fn my_bind_request_list(
    st: &AppState,
    status: &str,
    limit: i64,
) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    match sqlx::query(&crate::pq::rewrite(
        "SELECT id, name, person_id, person_name, team, status, reason, created_at, decided_at, decided_by \
         FROM my_bind_requests \
         WHERE (? = '' OR status = ?) \
         ORDER BY (status = 'pending') DESC, id DESC LIMIT ?",
    ))
    .bind(status)
    .bind(status)
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.try_get::<i64, _>("id").unwrap_or(0),
                    "name": r.try_get::<String, _>("name").unwrap_or_default(),
                    "personId": r.try_get::<i64, _>("person_id").unwrap_or(0),
                    "personName": r.try_get::<String, _>("person_name").unwrap_or_default(),
                    "team": r.try_get::<String, _>("team").unwrap_or_default(),
                    "status": r.try_get::<String, _>("status").unwrap_or_default(),
                    "reason": r.try_get::<String, _>("reason").unwrap_or_default(),
                    /* 2026-10-13 审计修复：列是 timestamptz，用 String 读会类型不匹配
                       → .ok() 吞掉后 createdAt/decidedAt 恒为空串。改读 chrono 再格式化。
                       2026-10-14：两条路径统一走 fmt_ts（见该文件顶部）。 */
                    "createdAt": fmt_ts(r, "created_at"),
                    "decidedAt": r
                        .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("decided_at")
                        .ok()
                        .flatten()
                        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_default(),
                    "decidedBy": r.try_get::<String, _>("decided_by").unwrap_or_default(),
                })
            })
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "查绑定申请列表失败");
            Vec::new()
        }
    }
}

/// 审核裁决：pending → approved/rejected 原子翻转（防双击重复绑定，与 anchor_claim_decide 同款）。
/// 返回 Some((申请账号, person_id, 主播姓名快照)) = 抢到裁决权；None = 不存在或已处理。
pub async fn my_bind_request_decide(
    st: &AppState,
    id: i64,
    approve: bool,
    reason: &str,
    decided_by: &str,
) -> Option<(String, i64, String)> {
    let pool = w(st).ok()?;
    let status = bind_request_next_status("pending", approve)?;
    // 2026-10-12 审计追查修复：decided_at 是 TIMESTAMPTZ 列，原来把格式化文本串
    // bind 进去——MySQL 时代 datetime 收文本没事，PG 严格类型直接报
    // "column decided_at is of type timestamp with time zone but expression is of
    // type text"，.ok()? 把错误吞成 None，管理员看到的却是「申请不存在」。
    // 改传 chrono 时间对象（sqlx 按 timestamptz 编码），并在 SQL 报错时留日志。
    let now = chrono::Local::now();
    let n = match sqlx::query(&crate::pq::rewrite(
        "UPDATE my_bind_requests SET status = ?, reason = ?, decided_at = ?, decided_by = ? \
         WHERE id = ? AND status = 'pending'",
    ))
    .bind(status)
    .bind(reason)
    .bind(now)
    .bind(decided_by)
    .bind(id)
    .execute(pool)
    .await
    {
        Ok(r) => r.affected(),
        Err(e) => {
            tracing::error!(id, error = %e, "绑定申请裁决 UPDATE 失败");
            return None;
        }
    };
    if n == 0 {
        return None;
    }
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT name, person_id, person_name FROM my_bind_requests WHERE id = ?",
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    Some((
        row.try_get::<String, _>("name").ok()?,
        /* person_id 在 PG 里是 BIGINT，但历史上灌过 INT4 的库；回退分支原来逐字抄了
        第一句（等于死代码），改成真正的 i32 回退（2026-10-14 审计 P2 同类清理）。 */
        row.try_get::<i64, _>("person_id")
            .or_else(|_| row.try_get::<i32, _>("person_id").map(i64::from))
            .ok()?,
        row.try_get::<String, _>("person_name").ok()?,
    ))
}

/// 裁决回滚：approve 后绑定写入失败时退回 pending，管理员可重试或改判
/// （与 anchor_claim_revert 同款理由：申请不能卡在「已通过」却没绑上）
pub async fn my_bind_request_revert(st: &AppState, id: i64) {
    if let Ok(pool) = w(st) {
        let _ = sqlx::query(&crate::pq::rewrite(
            "UPDATE my_bind_requests SET status = 'pending', decided_at = NULL, decided_by = '' \
             WHERE id = ? AND status = 'approved'",
        ))
        .bind(id)
        .execute(pool)
        .await;
    }
}

/// 该主播是否已有绑定的展示端账号（check-name 的 bound 标记 / 绑定申请提交前查重）。
/// 姓名是绑定的实际键（bind_inner 按 name 查重）；015 之后有 person_id 锚点的也一并算。
/// 查询失败一律按「未绑定」返回（宁可让人提交申请由管理员挡，不泄露库状态）。
pub async fn my_account_bound_for_person(st: &AppState, person_id: i64, person_name: &str) -> bool {
    let Ok(pool) = w(st) else {
        return false;
    };
    // 015 锚点口径：person_id > 0 且已绑定（列不存在 = 015 未跑，忽略走姓名口径）
    if person_id > 0 {
        if let Ok(Some(_)) = sqlx::query(&crate::pq::rewrite(
            "SELECT 1 FROM my_accounts WHERE person_id = ? AND person_id > 0 AND bound = 1 LIMIT 1",
        ))
        .bind(person_id)
        .fetch_optional(pool)
        .await
        {
            return true;
        }
    }
    // 姓名口径（历史账号未必回填了 person_id）；bound 列不存在 = 008 未跑，老账号一律视为已绑定
    match sqlx::query(&crate::pq::rewrite(
        "SELECT 1 FROM my_accounts WHERE name = ? AND bound = 1 LIMIT 1",
    ))
    .bind(person_name)
    .fetch_optional(pool)
    .await
    {
        Ok(r) => r.is_some(),
        Err(_) => matches!(
            sqlx::query(&crate::pq::rewrite(
                "SELECT 1 FROM my_accounts WHERE name = ? LIMIT 1"
            ))
            .bind(person_name)
            .fetch_optional(pool)
            .await,
            Ok(Some(_))
        ),
    }
}

/// 注册时按姓名精确查主播（2026-10-11 用户口径：注册要填主播姓名，并且必须在台账里）。
///
/// 口径与「绑定主播」保持一致：
/// - 全台账搜，不限团别（注册时还不知道团别）
/// - `deleted_at IS NULL`（已删除的不算）
/// - `status <> 'left'`（已离团的不算）
///
/// 返回 (person_id, 台账姓名, 团别)；查不到 → Ok(None)。
///
/// 两类会被显式拒绝的情况（审计 2026-10-11 修复）：
/// - **同名多行**：`LIMIT 2` 拿到第二行就报错，绝不静默取 id 最小那行（同名会串数据/串人）；
/// - **团别不是 male/female**（如台账新建师傅/徒弟的默认 `unknown`）：
///   登录按团别查人（`verify` 用 `p.gender = team`），用这种建号会「注册当场成功、
///   token 过期后永远登不上」，且手机号已被占不能重注册 —— 必须在这里挡住并让人去补台账。
pub async fn anchor_lookup_by_name(
    st: &AppState,
    name: &str,
) -> std::result::Result<Option<(i64, String, String)>, String> {
    let key = name.trim();
    if key.is_empty() {
        return Ok(None);
    }
    let rows = crate::pgrs::query_with_binds(
        &st.pools,
        &st.active_id(),
        "SELECT id, name, gender FROM person \
         WHERE deleted_at IS NULL AND status <> 'left' AND name = ? \
         ORDER BY id LIMIT 2",
        |q| q.bind(key.to_string()),
    )
    .await
    .map_err(|e| format!("查主播失败：{e}"))?;
    if rows.len() > 1 {
        return Err(format!(
            "台账里有不止一位叫「{key}」的主播，为避免绑错人，请联系管理员先区分艺名"
        ));
    }
    let Some(r) = rows.into_iter().next() else {
        return Ok(None);
    };
    let pid = r
        .try_get::<i64, _>("id")
        .or_else(|_| r.try_get::<i32, _>("id").map(i64::from))
        .unwrap_or(0);
    if pid <= 0 {
        return Ok(None);
    }
    let team: String = r.try_get("gender").unwrap_or_default();
    if team != "male" && team != "female" {
        return Err(format!(
            "主播「{key}」在台账里还没设置团别（男团 / 女团），请联系管理员设置后再注册"
        ));
    }
    Ok(Some((
        pid,
        r.try_get::<String, _>("name").unwrap_or_default(),
        team,
    )))
}

/// 绑定页搜索：全台账按名字模糊搜（注册出来的账号还没有团别，不能只搜本团）
pub async fn all_anchor_search(
    st: &AppState,
    source: &str,
    q: &str,
    limit: i64,
) -> Vec<serde_json::Value> {
    let key = q.trim();
    if key.is_empty() {
        return Vec::new();
    }
    let sql = "SELECT id, name, gender FROM person \
               WHERE deleted_at IS NULL AND status <> 'left' AND name LIKE ? \
               ORDER BY CASE WHEN name = ? THEN 0 ELSE 1 END, name LIMIT ?";
    let like = format!("%{key}%");
    match crate::pgrs::query_with_binds(&st.pools, source, sql, |qb| {
        qb.bind(like.clone()).bind(key.to_string()).bind(limit)
    })
    .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "personId": r.try_get::<i64, _>("id").unwrap_or(0),
                    "name": r.try_get::<String, _>("name").unwrap_or_default(),
                    "team": r.try_get::<String, _>("gender").unwrap_or_default(),
                })
            })
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "全台账主播搜索失败");
            Vec::new()
        }
    }
}

/* --------------------- 抖音号绑定申请（展示端提交 → 管理端审计） --------------------- */

/// 提交绑定申请。Err(错误文案)；Ok(申请 id)。
/// 规则：一人同时只能有一笔待审；同抖音号已通过过就不再收。
pub async fn anchor_claim_insert(
    st: &AppState,
    person_id: i64,
    person_name: &str,
    team: &str,
    douyin_no: &str,
    screenshot: &[u8],
    mime: &str,
) -> std::result::Result<i64, String> {
    let no = validate_douyin_no(douyin_no)?;
    if screenshot.is_empty() {
        return Err("缺少截图：请上传抖音主页截图".into());
    }
    if screenshot.len() > 1_500_000 {
        return Err(
            "截图超过大小限制（最多 1.5MB），请换小一点的图，格式支持 JPEG/PNG/WebP".into(),
        );
    }
    let mime = match mime {
        "image/jpeg" | "image/png" | "image/webp" => mime,
        _ => "image/jpeg",
    };
    let pool = w(st).map_err(|e| e.to_string())?;
    if sqlx::query(&crate::pq::rewrite(
        "SELECT 1 FROM anchor_claims WHERE person_id = ? AND status = 'pending'",
    ))
    .bind(person_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("查重失败：{e}"))?
    .is_some()
    {
        return Err("已存在待审核的提交，请等管理员审核".into());
    }
    if sqlx::query(&crate::pq::rewrite(
        "SELECT 1 FROM anchor_claims WHERE person_id = ? AND status = 'approved' AND douyin_no = ?",
    ))
    .bind(person_id)
    .bind(&no)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("查重失败：{e}"))?
    .is_some()
    {
        return Err("该抖音号已存在绑定记录，不用重复提交".into());
    }
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let row = sqlx::query(&crate::pq::rewrite(
        "INSERT INTO anchor_claims
           (person_id, person_name, team, douyin_no, screenshot, mime, status, created_at)
         VALUES (?, ?, ?, ?, ?, ?, 'pending', ?) RETURNING id",
    ))
    .bind(person_id)
    .bind(person_name)
    .bind(team)
    .bind(&no)
    .bind(screenshot)
    .bind(mime)
    .bind(&now)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("写入失败：{e}"))?;
    row.try_get::<i64, _>(0)
        .map_err(|e| format!("读取返回 id 失败：{e}"))
}

/// 某人最近一笔申请（展示端「我的」显示状态用）
pub async fn anchor_claim_mine(st: &AppState, person_id: i64) -> Option<serde_json::Value> {
    let pool = w(st).ok()?;
    let row = match sqlx::query(&crate::pq::rewrite(
        "SELECT id, douyin_no, status, reject_reason, created_at, decided_at, person_name, team
         FROM anchor_claims WHERE person_id = ? ORDER BY id DESC LIMIT 1",
    ))
    .bind(person_id)
    .fetch_optional(pool)
    .await
    {
        Ok(Some(r)) => r,
        _ => return None,
    };
    Some(serde_json::json!({
        "id": row.try_get::<i64, _>("id").ok()?,
        "douyinNo": row.try_get::<String, _>("douyin_no").unwrap_or_default(),
        "status": row.try_get::<String, _>("status").unwrap_or_default(),
        "rejectReason": row.try_get::<String, _>("reject_reason").unwrap_or_default(),
        "createdAt": row.try_get::<String, _>("created_at").unwrap_or_default(),
        "decidedAt": row.try_get::<String, _>("decided_at").unwrap_or_default(),
        "personName": row.try_get::<String, _>("person_name").unwrap_or_default(),
        "team": row.try_get::<String, _>("team").unwrap_or_default(),
    }))
}

/// 审计列表（管理端）。status 空 = 全部；不含截图本体。
///
/// ⚠️ `LENGTH(screenshot)` 对 BYTEA 返回 **integer（INT4）**：读取侧必须按 i32 解，
/// 按 i64 读会 ColumnDecode 被 `.unwrap_or(0)` 吞成 0（2026-10-14 审计 P0-2：
/// 审核列表的 screenshotBytes 曾经恒为 0）。
pub async fn anchor_claim_list(st: &AppState, status: &str, limit: i64) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    match sqlx::query(&crate::pq::rewrite(
        "SELECT id, person_id, person_name, team, douyin_no, status,
                reject_reason, created_at, decided_at, LENGTH(screenshot)
         FROM anchor_claims
         WHERE (? = '' OR status = ?)
         ORDER BY (status = 'pending') DESC, id DESC LIMIT ?",
    ))
    .bind(status)
    .bind(status)
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.try_get::<i64, _>(0).unwrap_or(0),
                    "personId": r.try_get::<i64, _>(1).unwrap_or(0),
                    "personName": r.try_get::<String, _>(2).unwrap_or_default(),
                    "team": r.try_get::<String, _>(3).unwrap_or_default(),
                    "douyinNo": r.try_get::<String, _>(4).unwrap_or_default(),
                    "status": r.try_get::<String, _>(5).unwrap_or_default(),
                    "rejectReason": r.try_get::<String, _>(6).unwrap_or_default(),
                    "createdAt": r.try_get::<String, _>(7).unwrap_or_default(),
                    "decidedAt": r.try_get::<String, _>(8).unwrap_or_default(),
                    "screenshotBytes": r.try_get::<i32, _>(9).map(i64::from).unwrap_or(0),
                })
            })
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "查绑定申请列表失败");
            Vec::new()
        }
    }
}

/// 截图本体（管理端预览）
pub async fn anchor_claim_image(st: &AppState, id: i64) -> Option<(Vec<u8>, String)> {
    let pool = w(st).ok()?;
    let row = match sqlx::query(&crate::pq::rewrite(
        "SELECT screenshot, mime FROM anchor_claims WHERE id = ?",
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    {
        Ok(Some(r)) => r,
        _ => return None,
    };
    Some((
        row.try_get::<Vec<u8>, _>("screenshot").ok()?,
        row.try_get::<String, _>("mime").unwrap_or_default(),
    ))
}

/// 审计裁决：approve=true 通过 / false 驳回。返回是否存在 pending 的这笔。
pub async fn anchor_claim_decide(
    st: &AppState,
    id: i64,
    approve: bool,
    reject_reason: &str,
    decided_by: &str,
) -> Option<(i64, String, String)> {
    let pool = w(st).ok()?;
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let status = if approve { "approved" } else { "rejected" };
    let n = sqlx::query(&crate::pq::rewrite(
        "UPDATE anchor_claims SET status = ?, reject_reason = ?, decided_at = ?, decided_by = ?
         WHERE id = ? AND status = 'pending'",
    ))
    .bind(status)
    .bind(reject_reason)
    .bind(&now)
    .bind(decided_by)
    .bind(id)
    .execute(pool)
    .await
    .ok()?
    .affected();
    if n == 0 {
        return None;
    }
    let row = match sqlx::query(&crate::pq::rewrite(
        "SELECT person_id, douyin_no, person_name FROM anchor_claims WHERE id = ?",
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    {
        Ok(Some(r)) => r,
        _ => return None,
    };
    Some((
        row.try_get::<i64, _>("person_id").ok()?,
        row.try_get::<String, _>("douyin_no").ok()?,
        row.try_get::<String, _>("person_name").ok()?,
    ))
}

/// 裁决回滚：通过但账号写入失败时，把申请退回 pending，管理员可重试或改判
pub async fn anchor_claim_revert(st: &AppState, id: i64) {
    if let Ok(pool) = w(st) {
        let _ = sqlx::query(&crate::pq::rewrite(
            "UPDATE anchor_claims SET status = 'pending', decided_at = '', decided_by = ''
             WHERE id = ? AND status = 'approved'",
        ))
        .bind(id)
        .execute(pool)
        .await;
    }
}
