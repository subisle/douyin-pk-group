//! 数据变更广播：管理端写入（含机器人导入）后，通知所有已打开的后台页面自动刷新。
//!
//! 为什么不用轮询：后台有 5 个页签，每个都轮询会白跑一堆查询；而且导入往往发生在
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
    /// 影响的数据域：import / roster / plans / settings / source / bot
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

/// `GET /api/public/events`：展示端用的轻量 SSE。
///
/// 只公开「数据已导入」这一件事，不携带后台 detail / 操作者 / 业务数字，
/// 前端收到后清掉榜单缓存并重拉当前团队数据，从而让每日最高弹幕随导入自动更新。
pub async fn public_sse(
    State(_st): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = subscribe();
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(c) if c.scope == SCOPE_IMPORT => {
                    let data = serde_json::json!({
                        "scope": SCOPE_IMPORT,
                        "at": c.at,
                    })
                    .to_string();
                    return Some((Ok(Event::default().event("change").data(data)), rx));
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let data = serde_json::json!({
                        "scope": SCOPE_IMPORT,
                        "at": chrono::Local::now().to_rfc3339(),
                        "resync": true,
                    })
                    .to_string();
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
}
