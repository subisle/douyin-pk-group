//! store::wx_sessions —— 自 store.rs 按业务域拆分（对外语义不变）。

use super::*; // 共享 w() / now_str() / AppState 等内部工具

/* --------------------------- 微信私聊会话上下文 --------------------------- */

/// 每次私聊入站刷新（主动推送时取 to_user_id + context_token）
pub async fn wx_session_upsert(
    st: &AppState,
    conversation_id: &str,
    to_user_id: &str,
    context_token: &str,
) {
    if let Ok(pool) = w(st) {
        let _ = sqlx::query(&crate::pq::rewrite(
            "INSERT INTO wx_sessions (conversation_id, to_user_id, context_token, updated_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT (conversation_id) DO UPDATE SET to_user_id = EXCLUDED.to_user_id,
               context_token = EXCLUDED.context_token, updated_at = EXCLUDED.updated_at",
        ))
        .bind(conversation_id)
        .bind(to_user_id)
        .bind(context_token)
        .bind(now_str())
        .execute(pool)
        .await;
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
