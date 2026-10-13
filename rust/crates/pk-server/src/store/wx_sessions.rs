//! store::wx_sessions —— 自 store.rs 按业务域拆分（对外语义不变）。

use super::*; // 共享 w() / now_str() / AppState 等内部工具

/* --------------------------- 微信私聊会话上下文 --------------------------- */

/// 每次私聊入站刷新（主动推送时取 to_user_id + context_token）
///
/// 2026-10-14 审计修复：原来这里写了 5 列（含 `updated_at`），但 001_core.sql:350-356
/// 的 wx_sessions 只有 4 列（conversation_id / to_user_id / context_token / nickname），
/// 根本没有 updated_at。PG 严格列检查 → 每次私聊入站都报
/// `column "updated_at" of relation "wx_sessions" does not exist`，而错误被 `let _ =`
/// 完全吞掉、零日志。后果：上下文 100% 写不进库 → wx_session_get 恒 None →
/// **主动推送（追分提醒 / 每日报告）整条链路失效**。
/// 去掉这一列（没有任何代码读它，写时刻也不影响取用），让写入在存量库上立刻成立。
pub async fn wx_session_upsert(
    st: &AppState,
    conversation_id: &str,
    to_user_id: &str,
    context_token: &str,
) {
    let Ok(pool) = w(st) else {
        return;
    };
    /* 2026-10-14：写失败必须留痕。iLink 只能被动回复，主动推送全靠这里落下的
       to_user_id + context_token；静默失败等于推送悄悄全挂且无人知晓。 */
    if let Err(e) = sqlx::query(&crate::pq::rewrite(
        "INSERT INTO wx_sessions (conversation_id, to_user_id, context_token)
         VALUES (?, ?, ?)
         ON CONFLICT (conversation_id) DO UPDATE SET to_user_id = EXCLUDED.to_user_id,
           context_token = EXCLUDED.context_token",
    ))
    .bind(conversation_id)
    .bind(to_user_id)
    .bind(context_token)
    .execute(pool)
    .await
    {
        tracing::error!(
            error = %e,
            conversation = %conversation_id,
            "微信私聊上下文写入失败（主动推送将取不到发送上下文）"
        );
    }
}

/// 取某会话的发送上下文（to_user_id, context_token）
pub async fn wx_session_get(st: &AppState, conversation_id: &str) -> Option<(String, String)> {
    let pool = w(st).ok()?;
    let r = sqlx::query(&crate::pq::rewrite(
        "SELECT to_user_id, context_token FROM wx_sessions WHERE conversation_id = ?",
    ))
    .bind(conversation_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    let to_user = r.try_get::<String, _>("to_user_id").unwrap_or_default();
    let token = r.try_get::<String, _>("context_token").unwrap_or_default();
    if to_user.is_empty() || token.is_empty() {
        return None;
    }
    Some((to_user, token))
}

/// 删某会话的发送上下文（换微信号重扫时作废旧会话；不存在时静默）
pub async fn wx_session_delete(st: &AppState, conversation_id: &str) {
    if let Ok(pool) = w(st) {
        let _ = sqlx::query(&crate::pq::rewrite(
            "DELETE FROM wx_sessions WHERE conversation_id = ?",
        ))
        .bind(conversation_id)
        .execute(pool)
        .await;
    }
}
