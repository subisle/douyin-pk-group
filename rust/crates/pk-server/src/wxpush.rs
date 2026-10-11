//! 用户自助扫码的微信推送通道（2026-10-11，多账号）。
//!
//! 每个展示端账号在「我的」页自己扫码登录一个 iLink 账号（官方
//! `Tencent/openclaw-weixin` 协议），服务端为每个账号各跑一条 `getupdates`
//! 长轮询，把用户首次发来的消息上下文（to_user_id + context_token）落库；
//! 之后 `push.rs` 用该账号自己的 bot token 主动推送，不依赖管理员机器人。
//!
//! 生命周期：进程启动恢复全部账号；新账号扫码确认后由公开接口调用
//! [`ensure_account`] 拉起轮询任务。任务随进程退出，不单独停止（解绑时账号行被删，
//! 下一轮读不到凭证自动结束）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::Value;
use tokio::task::JoinHandle;

use crate::bot::weixin::{user_poll_updates, user_send_text};
use crate::state::AppState;
use crate::store::WxPushAccount;

static TASKS: OnceLock<Mutex<HashMap<String, JoinHandle<()>>>> = OnceLock::new();

fn tasks() -> &'static Mutex<HashMap<String, JoinHandle<()>>> {
    TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 进程启动时恢复所有已扫码账号
pub async fn bootstrap(st: &Arc<AppState>) {
    for a in crate::store::wx_push_accounts_all(st).await {
        if !a.bot_token.is_empty() {
            ensure_account(st, &a.name).await;
        }
    }
}

/// 为一个展示端账号拉起长轮询（已在跑则跳过）
pub async fn ensure_account(st: &Arc<AppState>, name: &str) {
    let key = name.trim().to_string();
    {
        let mut map = tasks().lock().expect("wxpush tasks 锁 poisoned");
        map.retain(|_, h| !h.is_finished());
        if map.contains_key(&key) {
            return;
        }
    }
    let st2 = Arc::clone(st);
    let key2 = key.clone();
    let handle = tokio::spawn(async move { poll_loop(st2, key2).await });
    tasks()
        .lock()
        .expect("wxpush tasks 锁 poisoned")
        .insert(key, handle);
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

async fn poll_loop(st: Arc<AppState>, name: String) {
    let mut expired_streak = 0u32;
    loop {
        let Some(acct) = crate::store::wx_push_account_get(&st, &name).await else {
            return; // 已解绑 / 被删除
        };
        if acct.bot_token.is_empty() {
            return;
        }
        match user_poll_updates(&acct.base_url, &acct.bot_token, &acct.updates_buf).await {
            Ok(out) => {
                expired_streak = 0;
                let cursor = str_of(&out, "get_updates_buf");
                if !cursor.is_empty() {
                    crate::store::wx_push_account_set_updates_buf(&st, &name, &cursor).await;
                }
                let timeout_ms = out
                    .get("longpolling_timeout_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(30_000)
                    .min(35_000);
                let msgs = out
                    .get("msgs")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if msgs.is_empty() {
                    tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
                } else {
                    for m in &msgs {
                        handle_msg(&st, &acct, m).await;
                    }
                }
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("SESSION_EXPIRED") {
                    expired_streak += 1;
                    if expired_streak >= 4 {
                        tracing::warn!(account = %name, "扫码账号登录连续过期，停止轮询；需要重新扫码");
                        return;
                    }
                    tracing::warn!(account = %name, "扫码账号登录过期，一小时后重试");
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                } else {
                    tracing::warn!(account = %name, error = %msg, "扫码账号 getupdates 失败，5 秒后重试");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }
}

async fn handle_msg(st: &Arc<AppState>, acct: &WxPushAccount, m: &Value) {
    let from_user = str_of(m, "from_user_id");
    if from_user.is_empty() {
        return;
    }
    let context_token = str_of(m, "context_token");
    let conversation = format!("wxacct:{}:{}", acct.ilink_bot_id, from_user);
    crate::store::wx_session_upsert(st, &conversation, &from_user, &context_token).await;
    let first_message = acct.conversation_id.is_empty();
    crate::store::wx_push_account_bind_conversation(st, &acct.name, &conversation, &from_user)
        .await;
    // 扫码后的第一条消息 = 推送激活：把展示端账号接到该会话，并回执一次
    if first_message {
        crate::store::push_bind_account(st, &acct.name, &acct.team, &conversation, &from_user)
            .await;
        let text = "✅ 微信推送已绑定\n\n以后数据更新后会给你推送：\n· 追分提醒\n· 每日报告\n\n要开哪一项、要追谁，到展示页「我的」里设置。回复「解绑」可以断开。";
        if let Err(e) = user_send_text(
            &acct.base_url,
            &acct.bot_token,
            &from_user,
            &context_token,
            text,
        )
        .await
        {
            tracing::warn!(account = %acct.name, error = %e, "扫码激活回执发送失败");
        } else {
            tracing::info!(account = %acct.name, "扫码推送已激活（首次入站留下会话上下文）");
        }
    }
}
