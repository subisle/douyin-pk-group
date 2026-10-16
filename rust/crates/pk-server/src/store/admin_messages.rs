//! store::admin_messages —— 管理员消息下发三张表的数据访问层（docs/23 §5.3）。
//!
//! 表：`admin_messages`（消息主档）/ `admin_outbox`（消息 × 收件人 × 渠道一行，
//! pending → sending → sent/failed/skipped）/ `admin_message_reads`（弹窗已读回执）。
//!
//! 约定（对齐 store/wxpush.rs）：
//! - 写全走 writer 池（`super::w`），SQL 过 `crate::pq::rewrite`（`?` → `$N`）；
//! - 读走 `pgrs`（LOCAL 只读池，入口自带只读守卫 + 重写，只允许 SELECT）；
//! - 状态机：写入时只有 pending / skipped；worker 占行 pending → sending，
//!   发完 finish → sent / failed。skipped 是终态（占行 SQL 不取它）。

use super::*; // 共享 w() / now_str() / AppState / Result / Row / PgResultExt

/* ------------------------------ 行结构 ------------------------------ */

/// 消息主档入参（admin_messages 一行）
pub struct NewMessage {
    pub title: String,
    pub content: String,
    /// none | lineage | daily | month | custom（入口已过白名单；
    /// custom = 管理员上传图，见 task-14）
    pub image_kind: String,
    /// YYYY-MM-DD；None = 库里留 NULL
    pub image_date: Option<String>,
    /// 上传图原始字节（None = 没用上传图；`022` 之前的行也是 None）
    pub image_blob: Option<Vec<u8>>,
    /// 按文件魔数判出来的 MIME；只收 PNG（task-15），有图时恒为 `image/png`，无图空串
    pub image_mime: String,
    /// 上传时的原始文件名（可选）
    pub image_name: String,
    /// 逗号分隔：wxbot,popup
    pub channels: String,
    /// persons | masters | team | all
    pub target_type: String,
    /// 原始请求 JSON（审计可回放）
    pub target_ids: String,
    pub created_by: String,
}

/// outbox 一行：一条消息 × 每收件人 × 每渠道（失败互不影响）
pub struct OutboxRow {
    /// 占行 RETURNING 出来的行 id（写入时不用，置 0）
    pub id: i64,
    pub message_id: i64,
    pub person_id: u64,
    /// wxbot 行 = 主播姓名；popup 行 = my_accounts 登录账号名（已读回执按它对账）
    pub recipient_name: String,
    /// wxbot | popup
    pub channel: String,
    /// 写入时：pending | skipped
    pub status: String,
    pub error: String,
}

fn outbox_from_row(r: &sqlx::postgres::PgRow) -> OutboxRow {
    OutboxRow {
        id: r.try_get::<i64, _>("id").unwrap_or(0),
        message_id: r.try_get::<i64, _>("message_id").unwrap_or(0),
        person_id: r
            .try_get::<i64, _>("person_id")
            .map(|v| v.max(0) as u64)
            .unwrap_or(0),
        recipient_name: r.try_get::<String, _>("recipient_name").unwrap_or_default(),
        channel: r.try_get::<String, _>("channel").unwrap_or_default(),
        status: r.try_get::<String, _>("status").unwrap_or_default(),
        error: r.try_get::<String, _>("error").unwrap_or_default(),
    }
}

/* ------------------------------ 主档写入 ------------------------------ */

/// 落一条消息主档，返回消息 id。
pub async fn admin_message_insert(st: &AppState, m: &NewMessage) -> Result<i64> {
    let pool = w(st)?;
    let id: i64 = sqlx::query_scalar(&crate::pq::rewrite(
        "INSERT INTO admin_messages
           (title, content, image_kind, image_date, image_blob, image_mime, image_name,
            channels, target_type, target_ids, created_by)
         VALUES (?, ?, ?, ?::DATE, ?, ?, ?, ?, ?, ?, ?)
         RETURNING id",
    ))
    .bind(m.title.trim())
    .bind(&m.content)
    .bind(&m.image_kind)
    .bind(m.image_date.as_deref())
    .bind(m.image_blob.as_deref())
    .bind(&m.image_mime)
    .bind(m.image_name.trim())
    .bind(&m.channels)
    .bind(&m.target_type)
    .bind(&m.target_ids)
    .bind(m.created_by.trim())
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/* ------------------------------ outbox 队列 ------------------------------ */

/// 批量写 outbox 行（UNNEST 数组一次插入，避免几百行逐条往返）。
/// 返回实际写入行数。
pub async fn admin_outbox_insert_batch(
    st: &AppState,
    message_id: i64,
    rows: &[OutboxRow],
) -> Result<u64> {
    if rows.is_empty() {
        return Ok(0);
    }
    let pool = w(st)?;
    let pids: Vec<i64> = rows.iter().map(|r| r.person_id as i64).collect();
    let names: Vec<String> = rows.iter().map(|r| r.recipient_name.clone()).collect();
    let channels: Vec<String> = rows.iter().map(|r| r.channel.clone()).collect();
    let statuses: Vec<String> = rows.iter().map(|r| r.status.clone()).collect();
    let errors: Vec<String> = rows.iter().map(|r| r.error.clone()).collect();
    let res = sqlx::query(&crate::pq::rewrite(
        "INSERT INTO admin_outbox (message_id, person_id, recipient_name, channel, status, error)
         SELECT ?, t.pid, t.rn, t.ch, t.st, t.err
         FROM UNNEST(?::BIGINT[], ?::TEXT[], ?::TEXT[], ?::TEXT[], ?::TEXT[])
         AS t(pid, rn, ch, st, err)",
    ))
    .bind(message_id)
    .bind(&pids)
    .bind(&names)
    .bind(&channels)
    .bind(&statuses)
    .bind(&errors)
    .execute(pool)
    .await?;
    Ok(res.affected())
}

/// 占行取一批 pending（docs/23 §5.4：防重启双发）。
///
/// 外层 `AND status = 'pending'` 是关键：两个 worker 并发跑同一句 UPDATE 时，
/// 后到者在行锁上等待，前者提交后 PG 会拿**新行版本**重核整个 WHERE——
/// 子查询结果集缓存里 id 还在，但新版本 status 已是 'sending'，外层条件把它筛掉，
/// 该行只被先到者认领（RETURNING 也只出先到者这份）。
/// 行卡在 'sending'（进程崩溃）时不自动重发：宁漏不重。
pub async fn admin_outbox_take_pending(st: &AppState, limit: i64) -> Vec<OutboxRow> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    let limit = limit.clamp(1, 200);
    match sqlx::query(&crate::pq::rewrite(
        "UPDATE admin_outbox SET status = 'sending'
         WHERE status = 'pending'
           AND id IN (SELECT id FROM admin_outbox WHERE status = 'pending' ORDER BY id LIMIT ?)
         RETURNING id, message_id, person_id, recipient_name, channel, status, error",
    ))
    .bind(limit)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows.iter().map(outbox_from_row).collect(),
        Err(e) => {
            tracing::error!(error = %e, "outbox 占行失败");
            Vec::new()
        }
    }
}

/// 收尾一行：ok=true → sent（error 字段留降级备注，可空）；ok=false → failed + 原因。
/// 返回是否真的更新了行。
pub async fn admin_outbox_finish(st: &AppState, outbox_id: i64, ok: bool, error: &str) -> bool {
    let Ok(pool) = w(st) else {
        return false;
    };
    let err: String = error.chars().take(255).collect(); // VARCHAR(255)
    sqlx::query(&crate::pq::rewrite(
        "UPDATE admin_outbox
         SET status = ?, error = ?,
             sent_at = CASE WHEN ? THEN CURRENT_TIMESTAMP ELSE sent_at END
         WHERE id = ?",
    ))
    .bind(if ok { "sent" } else { "failed" })
    .bind(&err)
    .bind(ok)
    .bind(outbox_id)
    .execute(pool)
    .await
    .map(|r| r.affected() > 0)
    .unwrap_or(false)
}

/* ------------------------------ 主档读取 ------------------------------ */

/// 主档行 → JSON 投影。
///
/// ⚠️ **有意不下发 image_blob**（一张图最多 5MB，base64 进 JSON 会撑爆列表接口）：
/// 只回 `imageMime` / `imageName` / `imageBytes` / `hasImage` 四个小字段，
/// 原图由 `admin_message_image()`（和展示端 `…/image.png`）按需单取。
/// 注意调用方的 SELECT 必须带上 `image_mime` / `image_name` / `image_bytes` 三列。
fn message_from_row(r: &sqlx::postgres::PgRow) -> serde_json::Value {
    let image_bytes = r.try_get::<i64, _>("image_bytes").unwrap_or(0);
    serde_json::json!({
        "id": r.try_get::<i64, _>("id").unwrap_or(0),
        "title": r.try_get::<String, _>("title").unwrap_or_default(),
        "content": r.try_get::<String, _>("content").unwrap_or_default(),
        "imageKind": r.try_get::<String, _>("image_kind").unwrap_or_default(),
        "imageDate": r.try_get::<Option<String>, _>("image_date_s").ok().flatten(),
        "imageMime": r.try_get::<String, _>("image_mime").unwrap_or_default(),
        "imageName": r.try_get::<String, _>("image_name").unwrap_or_default(),
        "imageBytes": image_bytes,
        "hasImage": image_bytes > 0,
        "channels": r.try_get::<String, _>("channels").unwrap_or_default(),
        "targetType": r.try_get::<String, _>("target_type").unwrap_or_default(),
        "targetIds": r.try_get::<String, _>("target_ids").unwrap_or_default(),
        "createdBy": r.try_get::<String, _>("created_by").unwrap_or_default(),
        "createdAt": r.try_get::<String, _>("created_s").unwrap_or_default(),
    })
}

/// 主档 SELECT 的公共列表（`message_from_row` 依赖：三列一个都不能少）
const MESSAGE_COLS: &str = "id, title, content, image_kind,
                TO_CHAR(image_date, 'YYYY-MM-DD') AS image_date_s,
                image_mime, image_name,
                COALESCE(OCTET_LENGTH(image_blob), 0)::BIGINT AS image_bytes,
                channels, target_type, target_ids, created_by,
                TO_CHAR(created_at, 'YYYY-MM-DD HH24:MI:SS') AS created_s";

/// 单条主档（详情页用）。查不到 = None。
pub async fn admin_message_get(st: &AppState, id: i64) -> Result<Option<serde_json::Value>> {
    let sql = format!("SELECT {MESSAGE_COLS} FROM admin_messages WHERE id = ?");
    let rows = crate::pgrs::query_with_binds(&st.pools, "LOCAL", &sql, |q| q.bind(id)).await?;
    Ok(rows.first().map(message_from_row))
}

/// 一条消息的上传图原图：`(mime, bytes)`。没有上传图 / 消息不存在 = None。
///
/// 只按 id 取 `image_mime + image_blob` 两列，不碰其它字段：投递（wxbot 转 PNG）
/// 与展示端图片端点都用这一条，避免把 5MB BYTEA 拖进任何列表查询。
pub async fn admin_message_image(st: &AppState, id: i64) -> Result<Option<(String, Vec<u8>)>> {
    let rows = crate::pgrs::query_with_binds(
        &st.pools,
        "LOCAL",
        "SELECT image_mime, image_blob FROM admin_messages
         WHERE id = ? AND image_blob IS NOT NULL",
        |q| q.bind(id),
    )
    .await?;
    let Some(r) = rows.first() else {
        return Ok(None);
    };
    let mime = r.try_get::<String, _>("image_mime").unwrap_or_default();
    let blob = r
        .try_get::<Option<Vec<u8>>, _>("image_blob")
        .ok()
        .flatten()
        .unwrap_or_default();
    if blob.is_empty() {
        return Ok(None);
    }
    Ok(Some((mime, blob)))
}

/// 发送历史：主档 + 每条 (sent, failed, skipped, pending) 计数。
/// pending 计数含 'sending'（正在发，页面当作未完成）。
pub async fn admin_message_list(st: &AppState, limit: i64) -> Result<Vec<serde_json::Value>> {
    let limit = limit.clamp(1, 200);
    let sql = format!(
        "SELECT {MESSAGE_COLS},
                (SELECT COUNT(*) FROM admin_outbox o WHERE o.message_id = m.id AND o.status = 'sent') AS sent_n,
                (SELECT COUNT(*) FROM admin_outbox o WHERE o.message_id = m.id AND o.status = 'failed') AS failed_n,
                (SELECT COUNT(*) FROM admin_outbox o WHERE o.message_id = m.id AND o.status = 'skipped') AS skipped_n,
                (SELECT COUNT(*) FROM admin_outbox o WHERE o.message_id = m.id
                   AND o.status IN ('pending', 'sending')) AS pending_n
         FROM admin_messages m
         ORDER BY m.id DESC
         LIMIT ?"
    );
    let rows = crate::pgrs::query_with_binds(&st.pools, "LOCAL", &sql, |q| q.bind(limit)).await?;
    Ok(rows
        .iter()
        .map(|r| {
            let mut v = message_from_row(r);
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "counts".into(),
                    serde_json::json!({
                        "sent": r.try_get::<i64, _>("sent_n").unwrap_or(0),
                        "failed": r.try_get::<i64, _>("failed_n").unwrap_or(0),
                        "skipped": r.try_get::<i64, _>("skipped_n").unwrap_or(0),
                        "pending": r.try_get::<i64, _>("pending_n").unwrap_or(0),
                    }),
                );
            }
            v
        })
        .collect())
}

/// 一条消息的全部 outbox 行（每收件人 × 每渠道），弹窗行 LEFT JOIN 已读回执。
/// 已读回执只对 popup 行有意义（wxbot 没有已读概念），JOIN 条件里限定渠道。
pub async fn admin_outbox_for_message(
    st: &AppState,
    message_id: i64,
) -> Result<Vec<serde_json::Value>> {
    let rows = crate::pgrs::query_with_binds(
        &st.pools,
        "LOCAL",
        "SELECT o.id, o.person_id, o.recipient_name, o.channel, o.status, o.error,
                TO_CHAR(o.sent_at, 'YYYY-MM-DD HH24:MI:SS') AS sent_s,
                TO_CHAR(r.read_at, 'YYYY-MM-DD HH24:MI:SS') AS read_s
         FROM admin_outbox o
         LEFT JOIN admin_message_reads r
           ON r.message_id = o.message_id AND r.name = o.recipient_name AND o.channel = 'popup'
         WHERE o.message_id = ?
         ORDER BY o.id",
        |q| q.bind(message_id),
    )
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.try_get::<i64, _>("id").unwrap_or(0),
                "recipientName": r.try_get::<String, _>("recipient_name").unwrap_or_default(),
                "channel": r.try_get::<String, _>("channel").unwrap_or_default(),
                "status": r.try_get::<String, _>("status").unwrap_or_default(),
                "error": r.try_get::<String, _>("error").unwrap_or_default(),
                "sentAt": r.try_get::<Option<String>, _>("sent_s").ok().flatten(),
                "readAt": r.try_get::<Option<String>, _>("read_s").ok().flatten(),
            })
        })
        .collect())
}

/* ------------------------------ 弹窗已读 ------------------------------ */

/// 写已读回执（幂等：同一 (消息, 账号) 重复点不报错、不覆盖首次时间）。
pub async fn admin_message_read_mark(
    st: &AppState,
    message_id: i64,
    name: &str,
    person_id: u64,
) -> Result<()> {
    let pool = w(st)?;
    sqlx::query(&crate::pq::rewrite(
        "INSERT INTO admin_message_reads (message_id, name, person_id)
         VALUES (?, ?, ?)
         ON CONFLICT (message_id, name) DO NOTHING",
    ))
    .bind(message_id)
    .bind(name.trim())
    .bind(person_id as i64)
    .execute(pool)
    .await?;
    Ok(())
}

/// 该人（my token 的 name + person_id）的未读弹窗消息：发到他的 popup 行已 sent、
/// 且没有已读回执。最新在前，一次最多 50 条（弹窗只展示未读，够用）。
pub async fn admin_unread_for(
    st: &AppState,
    name: &str,
    person_id: u64,
) -> Result<Vec<serde_json::Value>> {
    let name = name.trim().to_string();
    let rows = crate::pgrs::query_with_binds(
        &st.pools,
        "LOCAL",
        "SELECT m.id, m.title, m.content, m.image_kind,
                TO_CHAR(m.image_date, 'YYYY-MM-DD') AS image_date_s,
                m.image_mime,
                COALESCE(OCTET_LENGTH(m.image_blob), 0)::BIGINT AS image_bytes,
                TO_CHAR(m.created_at, 'YYYY-MM-DD HH24:MI:SS') AS created_s
         FROM admin_outbox o
         JOIN admin_messages m ON m.id = o.message_id
         LEFT JOIN admin_message_reads r
           ON r.message_id = o.message_id AND r.name = o.recipient_name
         WHERE o.channel = 'popup' AND o.status = 'sent'
           AND o.recipient_name = ? AND o.person_id = ?
           AND r.read_at IS NULL
         ORDER BY m.id DESC
         LIMIT 50",
        |q| q.bind(name).bind(person_id as i64),
    )
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let image_bytes = r.try_get::<i64, _>("image_bytes").unwrap_or(0);
            serde_json::json!({
                "id": r.try_get::<i64, _>("id").unwrap_or(0),
                "title": r.try_get::<String, _>("title").unwrap_or_default(),
                "content": r.try_get::<String, _>("content").unwrap_or_default(),
                "imageKind": r.try_get::<String, _>("image_kind").unwrap_or_default(),
                // 弹窗据此决定要不要取 `…/image.png`（原图按需单取，不进这个响应）
                "imageMime": r.try_get::<String, _>("image_mime").unwrap_or_default(),
                "hasImage": image_bytes > 0,
                "date": r
                    .try_get::<Option<String>, _>("image_date_s")
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
                "createdAt": r.try_get::<String, _>("created_s").unwrap_or_default(),
            })
        })
        .collect())
}

/// 访问校验：这个人（name + person_id）确实是该消息 popup 渠道的已发送收件人。
/// 图片端点 / 已读标记共用，防止拿自己的登录态标别人的消息已读 / 取别人的图。
pub async fn admin_popup_recipient_exists(
    st: &AppState,
    message_id: i64,
    name: &str,
    person_id: u64,
) -> Result<bool> {
    let name = name.trim().to_string();
    let rows = crate::pgrs::query_with_binds(
        &st.pools,
        "LOCAL",
        "SELECT 1 AS ok FROM admin_outbox
         WHERE message_id = ? AND channel = 'popup' AND status = 'sent'
           AND recipient_name = ? AND person_id = ?
         LIMIT 1",
        |q| q.bind(message_id).bind(name).bind(person_id as i64),
    )
    .await?;
    Ok(!rows.is_empty())
}

/* ------------------------------ 一键开启师门日报 ------------------------------ */

/// 管理端「为所有师傅开启师门日报」：名下有**会进日报的**徒弟（直系、未删除、
/// 在职、未隐藏 —— 与推送侧 lineage_report::build 的名册过滤同口径，审计 6-P3-9）
/// 且还没开的绑定批量置 1，返回影响行数。幂等：已开的不重算。
pub async fn admin_lineage_enable_all(st: &AppState) -> Result<u64> {
    let pool = w(st)?;
    let res = sqlx::query(&crate::pq::rewrite(
        "UPDATE push_bindings
         SET lineage_report = 1, updated_at = ?
         WHERE lineage_report = 0 AND person_id > 0
           AND EXISTS (SELECT 1 FROM person c
                       WHERE c.master_id = push_bindings.person_id
                         AND c.deleted_at IS NULL
                         AND c.status <> 'left'
                         AND c.hide_in_daily_report = 0)",
    ))
    .bind(now_str())
    .execute(pool)
    .await?;
    Ok(res.affected())
}
