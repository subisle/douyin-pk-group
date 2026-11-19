//! 数据变更广播：管理端写入（含机器人导入）后，通知所有已打开的后台页面自动刷新。
//!
//! 为什么不用轮询：后台有多个页签，每个都轮询会白跑一堆查询；而且导入往往发生在
//! 手机 QQ 上（机器人），用户在电脑前看页面，需要「一导入就跳」。SSE 只走一条长连接，
//! 事件到达即时推送。
//!
//! 进程内广播（tokio broadcast），不跨进程、不持久化——丢事件不影响正确性，
//! 因为前端每次收到任何事件都会重新拉一次全量数据（幂等）。

use std::sync::OnceLock;

use serde::Serialize;
use tokio::sync::broadcast;

/// 一次数据变更。`scope` 用于让前端只刷新关心的页签，避免无谓请求。
#[derive(Clone, Debug, Serialize)]
pub struct Change {
    /// 影响的数据域：import / roster / plans / settings / source / bot / live /
    /// visits / notice / wx_binding
    pub scope: &'static str,
    /// 人类可读的说明（前端可展示成「刚刚有导入」这类提示）
    pub detail: String,
    /// 事件发生时间（RFC3339，本地时区）
    pub at: String,
}

/// 事件域常量（与前端 useAutoRefresh 的 scope 参数对应）
pub const SCOPE_IMPORT: &str = "import";
pub const SCOPE_ROSTER: &str = "roster";
pub const SCOPE_PLANS: &str = "plans";
pub const SCOPE_SETTINGS: &str = "settings";
pub const SCOPE_SOURCE: &str = "source";
pub const SCOPE_BOT: &str = "bot";
/// 直播 PK 实时变更（分数 / 对局状态 / 阶段推进，2026-10-20）
pub const SCOPE_LIVE: &str = "live";
/// 访客日志新增（2026-10-17：后台「访客日志」页原先订阅的 scope 与页面数据无关，
/// 页面永远不自动刷新；给它一个专用域，由 `ipinfo::log_access` 写入成功后广播）
pub const SCOPE_VISITS: &str = "visits";
/// 展示端管理员通知有新内容。事件只做刷新信号，不携带正文或收件人。
pub const SCOPE_NOTICE: &str = "notice";
/// 展示端微信推送绑定发生变化。事件不携带账号名、会话或 UID。
pub const SCOPE_WX_BINDING: &str = "wx_binding";

/// 缓冲区 64：慢客户端（手机挂后台）最多落后 64 条后收到 Lagged，
/// 前端对 Lagged 的处理就是「重新拉一次」，所以丢事件是安全的。
fn bus() -> &'static broadcast::Sender<Change> {
    static BUS: OnceLock<broadcast::Sender<Change>> = OnceLock::new();
    BUS.get_or_init(|| broadcast::channel(64).0)
}

/// 订阅变更（每个 SSE 连接一个 receiver）
pub fn subscribe() -> broadcast::Receiver<Change> {
    bus().subscribe()
}

/// 广播一次变更。没有任何订阅者时静默忽略（正常情况：没开后台页面）。
pub fn emit(scope: &'static str, detail: impl Into<String>) {
    let change = Change {
        scope,
        detail: detail.into(),
        at: chrono::Local::now().to_rfc3339(),
    };
    tracing::debug!(scope, detail = %change.detail, "数据变更广播");
    let _ = bus().send(change);
}

/// 管理员通知可用：只发刷新信号，正文与收件人由登录后的私有接口读取。
pub fn emit_notice_available() {
    emit(SCOPE_NOTICE, "");
}

/// 微信推送绑定变化：公开事件不得携带账号名、会话 id、UID 等敏感信息。
pub fn emit_wx_binding_changed() {
    emit(SCOPE_WX_BINDING, "");
}

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;

use crate::state::AppState;

/// `GET /api/admin/events`：SSE 长连接，有数据变更就推一条 `change` 事件。
///
/// 鉴权走全局中间件（`?access_token=` 已在白名单逻辑里支持，因为 EventSource
/// 不能自定义请求头）。
pub async fn sse(
    State(_st): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = subscribe();
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(c) => {
                let data = serde_json::to_string(&c).unwrap_or_else(|_| "{}".into());
                Some((Ok(Event::default().event("change").data(data)), rx))
            }
            // 客户端落后超过缓冲区：发一条 all 让它重拉全量，连接继续用
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(skipped = n, "SSE 订阅者落后，已跳过事件，通知前端重拉");
                let data = Change {
                    scope: "all",
                    detail: "resync".into(),
                    at: chrono::Local::now().to_rfc3339(),
                };
                let data = serde_json::to_string(&data).unwrap_or_else(|_| "{}".into());
                Some((Ok(Event::default().event("change").data(data)), rx))
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(20))
            .text("keep-alive"),
    )
}

/// 公开 SSE 白名单：只放行页面确实需要的刷新信号。
fn public_scope_allowed(scope: &str) -> Option<&'static str> {
    match scope {
        SCOPE_IMPORT => Some(SCOPE_IMPORT),
        SCOPE_PLANS => Some(SCOPE_PLANS),
        SCOPE_SETTINGS => Some(SCOPE_SETTINGS),
        SCOPE_LIVE => Some(SCOPE_LIVE),
        SCOPE_NOTICE => Some(SCOPE_NOTICE),
        SCOPE_WX_BINDING => Some(SCOPE_WX_BINDING),
        _ => None,
    }
}

/// 公开事件只输出 `scope` / `at`（可选 `resync`），绝不序列化后台 `detail`。
fn public_change_payload(scope: &str, at: &str, resync: bool) -> Option<String> {
    let scope = public_scope_allowed(scope)?;
    let mut payload = serde_json::json!({ "scope": scope, "at": at });
    if resync {
        payload["resync"] = serde_json::Value::Bool(true);
    }
    Some(payload.to_string())
}

/// `GET /api/public/events`：展示端轻量 SSE。
///
/// 白名单转发导入、方案变更、站点设置、管理员通知、微信绑定变化、直播公开页刷新；
/// 其他后台 scope 全部丢弃。所有公开事件都不携带后台 detail / 操作者 / 账号名 / 会话 / 业务数字。
pub async fn public_sse(
    State(_st): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = subscribe();
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(c) => {
                    if let Some(data) = public_change_payload(c.scope, &c.at, false) {
                        return Some((Ok(Event::default().event("change").data(data)), rx));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let at = chrono::Local::now().to_rfc3339();
                    let data = public_change_payload(SCOPE_IMPORT, &at, true)
                        .expect("import 必须在公开 SSE 白名单内");
                    return Some((Ok(Event::default().event("change").data(data)), rx));
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(20))
            .text("keep-alive"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全局总线会被并行跑的其它用例 emit 干扰，所以按唯一 detail 认领自己那条。
    async fn wait_for(rx: &mut broadcast::Receiver<Change>, detail: &str) -> Change {
        loop {
            let c = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("等事件超时")
                .expect("总线已关闭");
            if c.detail == detail {
                return c;
            }
        }
    }

    async fn wait_for_scope(rx: &mut broadcast::Receiver<Change>, scope: &str) -> Change {
        loop {
            let c = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("等事件超时")
                .expect("总线已关闭");
            if c.scope == scope {
                return c;
            }
        }
    }

    #[tokio::test]
    async fn emit_reaches_subscriber() {
        let mut rx = subscribe();
        emit(SCOPE_IMPORT, "导入用例-甲");
        let got = wait_for(&mut rx, "导入用例-甲").await;
        assert_eq!(got.scope, SCOPE_IMPORT);
        assert!(!got.at.is_empty(), "应带时间戳");
    }

    #[tokio::test]
    async fn emit_without_subscribers_does_not_panic() {
        // 没有任何 SSE 连接时导入照样要能跑通
        emit(SCOPE_ROSTER, "无订阅者用例");
    }

    #[tokio::test]
    async fn multiple_subscribers_all_receive() {
        let mut a = subscribe();
        let mut b = subscribe();
        emit(SCOPE_PLANS, "多订阅用例");
        assert_eq!(wait_for(&mut a, "多订阅用例").await.scope, SCOPE_PLANS);
        assert_eq!(wait_for(&mut b, "多订阅用例").await.scope, SCOPE_PLANS);
    }

    #[test]
    fn public_sse_whitelist_excludes_private_scopes() {
        for scope in [SCOPE_ROSTER, SCOPE_SOURCE, SCOPE_BOT, SCOPE_VISITS] {
            assert_eq!(
                public_scope_allowed(scope),
                None,
                "私域 scope 泄露：{scope}"
            );
            assert_eq!(
                public_change_payload(scope, "2026-11-15T01:00:00+08:00", false),
                None
            );
        }
        for scope in [
            SCOPE_IMPORT,
            SCOPE_PLANS,
            SCOPE_SETTINGS,
            SCOPE_LIVE,
            SCOPE_NOTICE,
            SCOPE_WX_BINDING,
        ] {
            assert_eq!(public_scope_allowed(scope), Some(scope));
        }
    }

    #[test]
    fn public_payload_never_contains_detail() {
        for scope in [
            SCOPE_IMPORT,
            SCOPE_PLANS,
            SCOPE_SETTINGS,
            SCOPE_LIVE,
            SCOPE_NOTICE,
            SCOPE_WX_BINDING,
        ] {
            let raw = public_change_payload(scope, "2026-11-15T01:00:00+08:00", false)
                .expect("白名单事件应生成 payload");
            let v: serde_json::Value = serde_json::from_str(&raw).expect("payload 必须是 JSON");
            assert_eq!(v["scope"], scope);
            assert_eq!(v.as_object().map(|o| o.len()), Some(2), "{v}");
            assert!(v.get("detail").is_none(), "{v}");
            assert!(
                v.get("nickname").is_none() && v.get("conversationId").is_none(),
                "{v}"
            );
        }
    }

    #[tokio::test]
    async fn public_signal_helpers_have_empty_detail() {
        let mut notice = subscribe();
        let mut wx = subscribe();
        emit_notice_available();
        emit_wx_binding_changed();
        let n = wait_for_scope(&mut notice, SCOPE_NOTICE).await;
        assert!(n.detail.is_empty(), "notice 不能携带敏感 detail");
        let w = wait_for_scope(&mut wx, SCOPE_WX_BINDING).await;
        assert!(w.detail.is_empty(), "wx_binding 不能携带敏感 detail");
    }
}
