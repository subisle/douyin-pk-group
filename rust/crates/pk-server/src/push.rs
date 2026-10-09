//! 展示端「我的」页微信推送引擎（2026-10-09）。
//!
//! 触发点：**日报数据导入成功之后**（admin::commit_import 尾部 spawn），
//! 不是定时任务——数据什么时候进来就什么时候算，不依赖调度器。
//!
//! 推两类，各自独立开关（都在「我的」页设置）：
//! - **每日报告**：当天音浪榜 TOP10 + 我的名次（每次导入都会推，同日重复导入会重复推，
//!   这是刻意的：运营补一轮数据就该看到新结果）
//! - **追分提醒**：把该账号所有追分目标合成一条消息（追上 / 被反超 / 还差多少），
//!   一天只推一次（按业务日去重），但「追上」「被反超」是事件，发生即推。
//!
//! 发送走微信 iLink 渠道的 [`WxTransport::send_proactive`]（需要最后一次私聊入站留下的
//! context_token）。渠道没配 / 会话失效都只记日志，不影响导入本身。

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::Row;

use crate::mysql;
use crate::state::AppState;

/// 绑定码：6 位，码表剔除易混字符（0/O/1/I）
pub const BIND_CODE_LEN: usize = 6;
/// 每人最多几个追分目标（用户口径 2026-10-09：5 个）
pub const MAX_TARGETS: i64 = 5;
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// 是不是一个合法绑定码（防止把正常聊天误判成绑定）
pub fn looks_like_bind_code(s: &str) -> bool {
    s.len() == BIND_CODE_LEN && s.bytes().all(|b| CODE_ALPHABET.contains(&b))
}

/// 随机绑定码（RandomState 每次构造都带 OS 级随机种子，省一个 rand 依赖）
pub fn new_bind_code() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::with_capacity(BIND_CODE_LEN);
    while out.len() < BIND_CODE_LEN {
        let n = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        for b in n.to_le_bytes() {
            out.push(CODE_ALPHABET[(b as usize) % CODE_ALPHABET.len()] as char);
            if out.len() == BIND_CODE_LEN {
                break;
            }
        }
    }
    out
}

/// 音浪数值按「万」表达（用户口径 2026-10-09：推送一律用万）
pub fn fmt_wan(v: i64) -> String {
    let f = v as f64;
    if v.abs() >= 10_000 {
        let w = f / 10_000.0;
        let s = if w.abs() >= 100.0 {
            format!("{}", w.round() as i64)
        } else {
            let r = (w * 10.0).round() / 10.0;
            if (r - r.round()).abs() < f64::EPSILON {
                format!("{}", r.round() as i64)
            } else {
                format!("{r:.1}")
            }
        };
        format!("{s} 万")
    } else {
        format!("{v}")
    }
}

/* ------------------------------ 触发 ------------------------------ */

/// 导入完成后异步跑推送（不阻塞导入响应）
pub fn spawn_after_import(st: &Arc<AppState>, team: &str, date: &str) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return; // 不在运行时里（单测）直接跳过
    };
    let st = Arc::clone(st);
    let team = team.to_string();
    let date = date.to_string();
    handle.spawn(async move {
        if let Err(e) = run(&st, &team, &date).await {
            tracing::warn!(error = %e, "推送执行失败");
        }
    });
}

/* ------------------------------ 主流程 ------------------------------ */

pub async fn run(st: &Arc<AppState>, team: &str, date: &str) -> anyhow::Result<()> {
    let bindings = crate::store::push_bindings_for_team(st, team).await;
    if bindings.is_empty() {
        return Ok(());
    }
    let Some(mgr) = st.bots.get() else {
        return Ok(());
    };
    let Some(wx) = mgr.wechat.clone() else {
        tracing::debug!("微信渠道未配置，跳过推送");
        return Ok(());
    };

    // 本月累计（追分口径：本月音浪，不是历史累计）
    let month = date.get(..7).unwrap_or(date).to_string();
    let month_map = month_wave_map(st, team, &month).await;

    // 每日报告全团共用一份排名，算一次
    let rank_items = if bindings.iter().any(|b| b.daily_report) {
        daily_rank(st, team, date).await
    } else {
        Vec::new()
    };

    for b in &bindings {
        let mut sent_any = false;

        // ① 每日报告
        if b.daily_report && !rank_items.is_empty() {
            let text = report_text(team, date, &rank_items, b.person_id);
            tracing::info!(account = %b.name, text = %text.replace('\n', " | "), "推送·每日报告");
            match wx.send_proactive(&b.conversation_id, &text).await {
                Ok(()) => {
                    sent_any = true;
                    tracing::info!(account = %b.name, "每日报告已推送");
                }
                Err(e) => {
                    tracing::warn!(account = %b.name, error = %e, "每日报告推送失败");
                    crate::store::push_touch(st, &b.name, &e.to_string()).await;
                }
            }
        }

        // ② 追分提醒（多个目标合成一条）
        if b.notify_target && b.person_id > 0 {
            let targets = crate::store::targets_for_push(st, &b.name).await;
            if !targets.is_empty() {
                let my = month_map.get(&b.person_id).copied().unwrap_or(0);
                let mut lines: Vec<String> = Vec::new();
                let mut event = false;
                let mut due = false;
                for t in &targets {
                    let (rival, label) = if t.kind == "value" {
                        (
                            t.target_value,
                            format!("本月目标 {}", fmt_wan(t.target_value)),
                        )
                    } else {
                        (
                            month_map.get(&t.target_person_id).copied().unwrap_or(0),
                            t.target_name.clone(),
                        )
                    };
                    let gap = rival - my;
                    if gap <= 0 {
                        if !t.hit {
                            lines.push(format!(
                                "🚀 已追上「{label}」！\n   你 {} / TA {}，反超 {}",
                                fmt_wan(my),
                                fmt_wan(rival),
                                fmt_wan(-gap)
                            ));
                            event = true;
                        }
                        crate::store::target_set_state(st, t.id, true, 0, date).await;
                    } else {
                        if t.hit {
                            lines.push(format!(
                                "⚠️ 「{label}」又超过你了\n   你 {} / TA {}，还差 {}",
                                fmt_wan(my),
                                fmt_wan(rival),
                                fmt_wan(gap)
                            ));
                            event = true;
                        } else if t.last_notice_date != date {
                            lines.push(format!(
                                "· 距「{label}」还差 {}（你 {} / TA {}）",
                                fmt_wan(gap),
                                fmt_wan(my),
                                fmt_wan(rival)
                            ));
                        }
                        if t.last_notice_date != date {
                            due = true;
                        }
                        crate::store::target_set_state(st, t.id, false, gap, date).await;
                    }
                }
                if !lines.is_empty() && (event || due) {
                    let text = format!(
                        "📈 追分进度（{} 数据）\n{}\n\n你本月累计 {}",
                        date.get(5..).unwrap_or(date),
                        lines.join("\n"),
                        fmt_wan(my)
                    );
                    tracing::info!(account = %b.name, text = %text.replace('\n', " | "), "推送·追分提醒");
                    match wx.send_proactive(&b.conversation_id, &text).await {
                        Ok(()) => {
                            sent_any = true;
                            tracing::info!(account = %b.name, targets = targets.len(), "追分提醒已推送");
                        }
                        Err(e) => {
                            tracing::warn!(account = %b.name, error = %e, "追分提醒推送失败");
                            crate::store::push_touch(st, &b.name, &e.to_string()).await;
                        }
                    }
                }
            }
        }

        if sent_any {
            crate::store::push_touch(st, &b.name, "").await;
        }
    }
    Ok(())
}

/* ------------------------------ 数据 ------------------------------ */

/// 本月累计音浪：person_id → wave（只算在册且对外可见的人，与榜单口径一致）
async fn month_wave_map(st: &AppState, team: &str, month: &str) -> HashMap<u64, i64> {
    let sql = format!(
        "SELECT p.id AS person_id, CAST(COALESCE(SUM(d.wave), 0) AS SIGNED) AS wave \
         FROM person p \
         LEFT JOIN daily_metric d ON d.person_id = p.id \
           AND DATE_FORMAT(d.biz_date, '%Y-%m') = '{month}' \
         WHERE p.deleted_at IS NULL AND p.gender = '{team}' \
         GROUP BY p.id"
    );
    let mut map = HashMap::new();
    match mysql::query(&st.pools, &st.active_id(), &sql).await {
        Ok(rows) => {
            for r in &rows {
                let pid = r
                    .try_get::<u64, _>("person_id")
                    .or_else(|_| r.try_get::<i64, _>("person_id").map(|v| v as u64))
                    .unwrap_or(0);
                let w = r
                    .try_get::<i64, _>("wave")
                    .or_else(|_| r.try_get::<u64, _>("wave").map(|v| v as i64))
                    .unwrap_or(0);
                if pid > 0 {
                    map.insert(pid, w);
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "读本月累计失败，追分按 0 处理"),
    }
    map
}

/// 当日榜（(person_id, 名字, 音浪)），口径与展示端每日榜一致：
/// 在册 + 未离团 + 不隐藏（hide_in_daily_report=0），当天音浪 < 3 视为未开播（不计入榜）
async fn daily_rank(st: &AppState, team: &str, date: &str) -> Vec<(u64, String, i64)> {
    let sql = format!(
        "SELECT p.id AS person_id, p.name, CAST(COALESCE(SUM(d.wave), 0) AS SIGNED) AS wave \
         FROM person p \
         JOIN daily_metric d ON d.person_id = p.id \
           AND DATE_FORMAT(d.biz_date, '%Y-%m-%d') = '{date}' \
         WHERE p.deleted_at IS NULL AND p.gender = '{team}' AND p.status <> 'left' \
           AND p.hide_in_daily_report = 0 \
         GROUP BY p.id, p.name \
         HAVING wave >= 3 \
         ORDER BY wave DESC, p.name ASC"
    );
    match mysql::query(&st.pools, &st.active_id(), &sql).await {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                (
                    r.try_get::<u64, _>("person_id")
                        .or_else(|_| r.try_get::<i64, _>("person_id").map(|v| v as u64))
                        .unwrap_or(0),
                    r.try_get::<String, _>("name").unwrap_or_default(),
                    r.try_get::<i64, _>("wave")
                        .or_else(|_| r.try_get::<u64, _>("wave").map(|v| v as i64))
                        .unwrap_or(0),
                )
            })
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "读当日榜失败，跳过日报推送");
            Vec::new()
        }
    }
}

/// 每日报告文案：TOP10 + 我的名次
fn report_text(team: &str, date: &str, items: &[(u64, String, i64)], me: u64) -> String {
    let label = if team == "female" { "女团" } else { "男团" };
    let mut out = format!(
        "📊 {} {} 音浪榜（{} 人开播）",
        label,
        date.get(5..).unwrap_or(date),
        items.len()
    );
    for (i, (_, name, wave)) in items.iter().take(10).enumerate() {
        out.push_str(&format!("\n{}. {} {}", i + 1, name, fmt_wan(*wave)));
    }
    if let Some(pos) = items.iter().position(|(pid, _, _)| *pid == me) {
        out.push_str(&format!(
            "\n\n我：第 {} 名 · {}",
            pos + 1,
            fmt_wan(items[pos].2)
        ));
    } else if me > 0 {
        out.push_str("\n\n我：今天还没开播");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wan_formatting() {
        assert_eq!(fmt_wan(0), "0");
        assert_eq!(fmt_wan(9_999), "9999");
        assert_eq!(fmt_wan(10_000), "1 万");
        assert_eq!(fmt_wan(113_700), "11.4 万");
        assert_eq!(fmt_wan(1_000_000), "100 万");
        assert_eq!(fmt_wan(1234), "1234");
    }

    #[test]
    fn bind_code_shape() {
        for _ in 0..20 {
            let c = new_bind_code();
            assert_eq!(c.len(), BIND_CODE_LEN);
            assert!(looks_like_bind_code(&c), "生成的码要能通过校验: {c}");
            assert!(!c.contains('0') && !c.contains('O') && !c.contains('1') && !c.contains('I'));
        }
        assert!(!looks_like_bind_code("ABC"));
        assert!(!looks_like_bind_code("ABCDE1"), "1 不在码表里");
        assert!(!looks_like_bind_code(""));
    }

    #[test]
    fn report_text_marks_my_rank() {
        let items = vec![
            (1u64, "阿甲".to_string(), 1_200_000),
            (2u64, "阿乙".to_string(), 900_000),
            (3u64, "阿丙".to_string(), 100_000),
        ];
        let t = report_text("male", "2026-10-08", &items, 3);
        assert!(t.contains("男团 10-08 音浪榜"), "{t}");
        assert!(t.contains("1. 阿甲 120 万"), "{t}");
        assert!(t.contains("我：第 3 名"), "{t}");

        let t2 = report_text("female", "2026-10-08", &items, 9);
        assert!(t2.contains("今天还没开播"), "{t2}");
    }
}
