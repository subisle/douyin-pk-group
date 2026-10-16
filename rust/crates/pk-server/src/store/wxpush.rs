//! store::wxpush —— 自 store.rs 按业务域拆分（对外语义不变）。

use super::*; // 共享 w() / now_str() / AppState 等内部工具

/* --------------------- 展示端「我的」页微信推送（绑定 + 追分目标） --------------------- */
/*
   2026-10-09 新增。链路（2026-10-16 深夜用户口径：页面统一叫「对接码」）：
     展示端「我的」页点「获取二维码」→ 得到二维码 + 4 位对接码 →
     扫码对接（或把 4 位对接码发给机器人）
     → 私聊会话 id 与展示端账号绑定（push_bindings）→ 数据导入完成后按目标推送
   iLink 只能被动回复，所以每次私聊入站都把 to_user_id / context_token 落到 wx_sessions，
   主动推送时从这里取（服务重启不丢会话上下文）。
*/

/// 绑定关系（推送引擎用的完整行）
#[derive(Clone, Debug)]
pub struct PushBinding {
    pub name: String,
    pub team: String,
    pub person_id: u64,
    pub conversation_id: String,
    pub notify_target: bool,
    pub daily_report: bool,
    /// 师门日报开关（docs/23 §4.3）：名下有直系徒弟才推
    pub lineage_report: bool,
    /// 同业务日去重：师门日报只推一次（对齐 last_report_date）
    pub last_lineage_date: String,
}

/// 读 push_bindings 里当前记着的会话 id（空串 = 还没接上任何会话 / 未激活）。
///
/// 为什么单独有这一个读：`push_bindings.conversation_id` 是展示端「已绑定」的
/// 唯一口径（[`push_state`] 的 bound 就是它非空）。扫码激活的判定不能拿
/// `wx_push_accounts.conversation_id` 当代理 —— 那张表的列更宽、先写成功，
/// 一旦 push_bindings 这次写失败（列长不够 / 抖动），判定就会永远为「已激活」，
/// 状态卡死在「未绑定」且不再自愈（2026-10-16 实测踩到）。
pub async fn push_binding_conversation(st: &AppState, name: &str) -> String {
    let Some(pool) = w(st).ok() else {
        return String::new();
    };
    sqlx::query(&crate::pq::rewrite(
        "SELECT conversation_id FROM push_bindings WHERE name = ?",
    ))
    .bind(name.trim())
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .and_then(|r| r.try_get::<String, _>("conversation_id").ok())
    .unwrap_or_default()
}

/// 展示端「我的」页看到的推送状态（不含会话 id 等内部字段）
pub async fn push_state(st: &AppState, name: &str) -> serde_json::Value {
    let Some(pool) = w(st).ok() else {
        return serde_json::json!({ "bound": false, "code": "", "codeExpires": "" });
    };
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT conversation_id, nickname, bind_code, code_expires, bound_at,
                notify_target, daily_report, last_push, last_error,
                lineage_report, last_lineage_date
         FROM push_bindings WHERE name = ?",
    ))
    .bind(name.trim())
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let Some(r) = row else {
        return serde_json::json!({
            "bound": false, "code": "", "codeExpires": "",
            "notifyTarget": false, "dailyReport": false,
            "lineageReport": false, "hasDisciples": false,
        });
    };
    let conv: String = r
        .try_get::<String, _>("conversation_id")
        .unwrap_or_default();
    let mut code: String = r.try_get::<String, _>("bind_code").unwrap_or_default();
    let mut code_expires: String = r.try_get::<String, _>("code_expires").unwrap_or_default();
    if !code.is_empty() && !push_code_is_live(&code_expires) {
        code.clear();
        code_expires.clear();
    }
    serde_json::json!({
        "bound": !conv.is_empty(),
        "nickname": r.try_get::<String, _>("nickname").unwrap_or_default(),
        "code": code,
        "codeExpires": code_expires,
        "boundAt": r.try_get::<String, _>("bound_at").unwrap_or_default(),
        "notifyTarget": r.try_get::<i16, _>("notify_target").unwrap_or(0) == 1,
        "dailyReport": r.try_get::<i16, _>("daily_report").unwrap_or(0) == 1,
        "lineageReport": r.try_get::<i16, _>("lineage_report").unwrap_or(0) == 1,
        "lastPush": r.try_get::<String, _>("last_push").unwrap_or_default(),
        "lastError": r.try_get::<String, _>("last_error").unwrap_or_default(),
    })
}

/// 修复3：push_bindings.team 白名单归一（该列会被 report.rs 内联进 `p.gender='{team}'`）。
/// 返回 None = 完全非法的输入（调用方拒绝）；空串按 all 归一（管理员观看账号口径）。
pub(crate) fn normalize_push_team(team: &str) -> Option<&'static str> {
    match team.trim() {
        "male" => Some("male"),
        "female" => Some("female"),
        "all" => Some("all"),
        "" => Some("all"), // 空团别 = 台账查不到的观看账号，按 all（全站口径）
        _ => None,
    }
}

/// 写入/刷新绑定码（保留已有绑定；重新生成只在未绑定时用于首次绑定）。
/// 已绑定时也允许生成新码（用于换微信号重绑），绑定成功会覆盖旧会话。
pub async fn push_set_code(
    st: &AppState,
    name: &str,
    team: &str,
    person_id: u64,
    code: &str,
) -> Result<()> {
    // 修复3·健壮：team 白名单校验。team 会原样落 push_bindings.team，
    // 而推送引擎/report.rs 拼 SQL 用 `p.gender='{team}'` 内联（见 report.rs build/build_month），
    // URL path 里的任意值直通这里就能污染那段 SQL。入口只放行 male/female/all
    // （空串归一为 all = 观看账号口径），其余值（含引号、SQL 片段）一律报错，
    // 注入面在源头封死。调用方（public_teams/wx.rs）会把该错误转成 400。
    let team = normalize_push_team(team)
        .ok_or_else(|| anyhow::anyhow!("团别参数不合法（只接受 male/female/all）"))?;
    let pool = w(st)?;
    let expires = (chrono::Local::now() + chrono::Duration::minutes(30))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    sqlx::query(&crate::pq::rewrite(
        "INSERT INTO push_bindings (name, team, person_id, bind_code, code_expires, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (name) DO UPDATE SET team = EXCLUDED.team, person_id = EXCLUDED.person_id,
           bind_code = EXCLUDED.bind_code, code_expires = EXCLUDED.code_expires,
           updated_at = EXCLUDED.updated_at",
    ))
    .bind(name.trim())
    .bind(team)
    .bind(person_id as i64)
    .bind(code)
    .bind(expires)
    .bind(now_str())
    .execute(pool)
    .await?;
    Ok(())
}

/// 该绑定码是否已被**别的**账号占着（空码不算冲突）。
async fn push_code_taken(st: &AppState, code: &str, name: &str) -> bool {
    let Ok(pool) = w(st) else {
        return false;
    };
    sqlx::query(&crate::pq::rewrite(
        "SELECT 1 FROM push_bindings WHERE bind_code = ? AND name <> ?",
    ))
    .bind(code)
    .bind(name.trim())
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .is_some()
}

/// 对接码仍在有效期内返回 `true`；空过期时间一律视为已失效。
fn push_code_is_live(code_expires: &str) -> bool {
    !code_expires.trim().is_empty()
        && chrono::NaiveDateTime::parse_from_str(code_expires, "%Y-%m-%d %H:%M:%S")
            .map(|expires| expires > chrono::Local::now().naive_local())
            .unwrap_or(false)
}

/// 当前有效对接码；没有则摇一个新码。同一账号在 30 分钟内幂等，
/// 「获取二维码」并行调用 start/code 时两边拿到的是同一个 4 位码。
pub async fn push_code_current_or_rotate(
    st: &AppState,
    name: &str,
    team: &str,
    person_id: u64,
) -> Result<(String, String)> {
    static CODE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _lock = CODE_LOCK.lock().await;
    let state = push_state(st, name).await;
    let code = state
        .get("code")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let code_expires = state
        .get("codeExpires")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if !code.is_empty() && push_code_is_live(code_expires) {
        return Ok((code.to_string(), code_expires.to_string()));
    }
    let code = push_rotate_code(st, name, team, person_id).await?;
    let state = push_state(st, name).await;
    Ok((
        state
            .get("code")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&code)
            .to_string(),
        state
            .get("codeExpires")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
    ))
}

/// 摇一个新绑定码并落库，撞号自动重摇。
///
/// 为什么不能「生成 → 直接写」：4 位数字只有 10^4 种取值，而 [`push_bind_by_code`]
/// 是 `WHERE bind_code = ?` 单行取账号 —— 两行同码时会把微信绑到**别人**的账号上
/// （收到别人的推送）。双保险：
/// 1. 写前先查一次（本函数），撞了就重摇；
/// 2. 库侧 `028_push_bind_code_unique` 的部分唯一索引兜住并发竞态，写入撞唯一约束
///    （23505）也当撞号处理，继续重摇，而不是把 500 抛给用户。
pub async fn push_rotate_code(
    st: &AppState,
    name: &str,
    team: &str,
    person_id: u64,
) -> Result<String> {
    for _ in 0..32 {
        let code = crate::push::new_bind_code();
        if push_code_taken(st, &code, name).await {
            continue;
        }
        match push_set_code(st, name, team, person_id, &code).await {
            Ok(()) => return Ok(code),
            Err(e) if is_unique_violation(&e) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(anyhow::anyhow!("对接码生成失败：连续撞号，请稍后重试"))
}

/// PG 唯一约束冲突（SQLSTATE 23505）——绑定码撞号重摇的判据
fn is_unique_violation(e: &anyhow::Error) -> bool {
    e.downcast_ref::<sqlx::Error>()
        .and_then(|se| match se {
            sqlx::Error::Database(db) => db.code(),
            _ => None,
        })
        .map(|c| c == "23505")
        .unwrap_or(false)
}

/// 绑定码 → 绑定：把微信私聊会话接到展示端账号上。
/// 返回 Some((账号姓名, 团别))，没命中/已过期返回 None。码一次性，用掉即清。
pub async fn push_bind_by_code(
    st: &AppState,
    code: &str,
    conversation_id: &str,
    to_user_id: &str,
) -> Option<(String, String)> {
    let pool = w(st).ok()?;
    let code = code.trim().to_ascii_uppercase();
    if code.is_empty() {
        return None;
    }
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT name, team, code_expires FROM push_bindings WHERE bind_code = ? AND bind_code <> ''",
    ))
    .bind(&code)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    let expires: String = row.try_get::<String, _>("code_expires").unwrap_or_default();
    if !push_code_is_live(&expires) {
        return None;
    }
    let name: String = row.try_get::<String, _>("name").ok()?;
    let team: String = row.try_get::<String, _>("team").unwrap_or_default();
    // 会话抢占（2026-10-10 审计补）：绑定码路径与手机号路径口径一致——
    // 同一个微信会话只挂一个账号，先把占着这个会话的其它绑定清掉，
    // 否则换号 / 误发码会让两个账号同时往一个微信号推。
    let _ = sqlx::query(&crate::pq::rewrite(
        "UPDATE push_bindings SET conversation_id = '', to_user_id = '', bound_at = '',
           bind_code = '', code_expires = '', updated_at = ?
         WHERE conversation_id = ? AND name <> ?",
    ))
    .bind(now_str())
    .bind(conversation_id)
    .bind(&name)
    .execute(pool)
    .await;
    let res = sqlx::query(&crate::pq::rewrite(
        "UPDATE push_bindings SET conversation_id = ?, to_user_id = ?, nickname = ?,
           bind_code = '', code_expires = '', bound_at = ?, last_error = '', updated_at = ?
         WHERE name = ?",
    ))
    .bind(conversation_id)
    .bind(to_user_id)
    .bind("")
    .bind(now_str())
    .bind(now_str())
    .bind(&name)
    .execute(pool)
    .await;
    if let Err(e) = res {
        tracing::error!(error = %e, "写入微信绑定失败");
        return None;
    }
    Some((name, team))
}

/// 补写绑定行的团别（绑定码路径：台账建档晚于绑定码生成时团别为空，
/// 推送按团捞绑定会漏掉这个人——绑定成功后由机器人回查台账补上）
pub async fn push_set_team(st: &AppState, name: &str, team: &str) -> bool {
    matches!(w(st), Ok(pool) if sqlx::query(&crate::pq::rewrite("UPDATE push_bindings SET team = ?, updated_at = ? WHERE name = ?"))
                .bind(team)
        .bind(now_str())
        .bind(name.trim())
        .execute(pool)
        .await
        .map(|r| r.affected() > 0)
        .unwrap_or(false))
}

/// 解绑（保留目标配置，只断开微信）
pub async fn push_unbind(st: &AppState, name: &str) -> bool {
    matches!(w(st), Ok(pool) if sqlx::query(&crate::pq::rewrite("UPDATE push_bindings SET conversation_id = '', to_user_id = '', bound_at = '',
           bind_code = '', code_expires = '', updated_at = ? WHERE name = ?"))
                .bind(now_str())
        .bind(name.trim())
        .execute(pool)
        .await
        .map(|r| r.affected() > 0)
        .unwrap_or(false))
}

/// 按微信会话解绑（机器人里回复「解绑」）：返回被解绑的账号姓名
pub async fn push_unbind_by_conversation(st: &AppState, conversation_id: &str) -> Option<String> {
    let pool = w(st).ok()?;
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT name FROM push_bindings WHERE conversation_id = ?",
    ))
    .bind(conversation_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    let name: String = row.try_get::<String, _>("name").ok()?;
    let _ = sqlx::query(&crate::pq::rewrite(
        "UPDATE push_bindings SET conversation_id = '', to_user_id = '', bound_at = '',
           bind_code = '', code_expires = '', updated_at = ? WHERE name = ?",
    ))
    .bind(now_str())
    .bind(&name)
    .execute(pool)
    .await;
    // 顺带清掉这个会话的 iLink 上下文（2026-10-10 审计）：context_token 留着无害
    // 但会积累脏数据；删掉后换人重绑时由新入站消息重建。
    let _ = sqlx::query(&crate::pq::rewrite(
        "DELETE FROM wx_sessions WHERE conversation_id = ?",
    ))
    .bind(conversation_id)
    .execute(pool)
    .await;
    Some(name)
}

/// 两个推送开关
pub async fn push_set_flags(
    st: &AppState,
    name: &str,
    notify_target: bool,
    daily_report: bool,
) -> bool {
    matches!(w(st), Ok(pool) if sqlx::query(&crate::pq::rewrite("UPDATE push_bindings SET notify_target = ?, daily_report = ?, updated_at = ? WHERE name = ?"))
                .bind(if notify_target { 1i16 } else { 0i16 })
        .bind(if daily_report { 1i16 } else { 0i16 })
        .bind(now_str())
        .bind(name.trim())
        .execute(pool)
        .await
        .map(|r| r.affected() > 0)
        .unwrap_or(false))
}

/// 只改「每日报告」开关（机器人里的「暂停 / 开启接收每日推送」）。
/// 注意不要用 [`push_set_flags`] 代替：那个会把 notify_target 一起写掉。
pub async fn push_set_daily_report(st: &AppState, name: &str, on: bool) -> bool {
    let Ok(pool) = w(st) else {
        return false;
    };
    let name = name.trim();
    let changed = sqlx::query(&crate::pq::rewrite(
        "UPDATE push_bindings SET daily_report = ?, updated_at = ? WHERE name = ?",
    ))
    .bind(if on { 1i16 } else { 0i16 })
    .bind(now_str())
    .bind(name)
    .execute(pool)
    .await
    .map(|r| r.affected() > 0)
    .unwrap_or(false);
    if changed {
        return true;
    }
    // 值没变时 SQL 执行的 affected rows = 0（同一秒内重复发同一条命令），
    // 不是失败：确认这行还在就按幂等成功回执。
    sqlx::query_scalar::<_, i64>(&crate::pq::rewrite(
        "SELECT COUNT(*) FROM push_bindings WHERE name = ?",
    ))
    .bind(name)
    .fetch_one(pool)
    .await
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// 只改「师门日报」开关（docs/23 §4.3；「我的」页第三开关，仅名下有徒弟的人可见）。
/// 与 [`push_set_daily_report`] 同款幂等语义：值没变但行在 = 成功。
pub async fn push_set_lineage_report(st: &AppState, name: &str, on: bool) -> bool {
    let Ok(pool) = w(st) else {
        return false;
    };
    let name = name.trim();
    let changed = sqlx::query(&crate::pq::rewrite(
        "UPDATE push_bindings SET lineage_report = ?, updated_at = ? WHERE name = ?",
    ))
    .bind(if on { 1i16 } else { 0i16 })
    .bind(now_str())
    .bind(name)
    .execute(pool)
    .await
    .map(|r| r.affected() > 0)
    .unwrap_or(false);
    if changed {
        return true;
    }
    sqlx::query_scalar::<_, i64>(&crate::pq::rewrite(
        "SELECT COUNT(*) FROM push_bindings WHERE name = ?",
    ))
    .bind(name)
    .fetch_one(pool)
    .await
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// 师门日报推送成功后写同业务日去重标记（对齐 last_report_date 机制）
pub async fn push_set_lineage_date(st: &AppState, name: &str, date: &str) -> bool {
    let Ok(pool) = w(st) else {
        return false;
    };
    sqlx::query(&crate::pq::rewrite(
        "UPDATE push_bindings SET last_lineage_date = ?, updated_at = ? WHERE name = ?",
    ))
    .bind(date)
    .bind(now_str())
    .bind(name.trim())
    .execute(pool)
    .await
    .map(|r| r.affected() > 0)
    .unwrap_or(false)
}

/// 按微信会话查绑定（机器人角色判定 + 普通用户命令取姓名 / person_id / 团别）
pub async fn push_binding_by_conversation(
    st: &AppState,
    conversation_id: &str,
) -> Option<PushBinding> {
    let pool = w(st).ok()?;
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT name, team, person_id, conversation_id, notify_target, daily_report,
                lineage_report, last_lineage_date
         FROM push_bindings WHERE conversation_id = ? LIMIT 1",
    ))
    .bind(conversation_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    Some(binding_from_row(&row))
}

/// push_bindings → PushBinding 的统一映射（列固定：name/team/person_id/
/// conversation_id/notify_target/daily_report/lineage_report/last_lineage_date，
/// 两处查询共用）
fn binding_from_row(r: &sqlx::postgres::PgRow) -> PushBinding {
    PushBinding {
        name: r.try_get::<String, _>("name").unwrap_or_default(),
        team: r.try_get::<String, _>("team").unwrap_or_default(),
        person_id: r
            .try_get::<i64, _>("person_id")
            .map(|v| v.max(0) as u64)
            .unwrap_or(0),
        conversation_id: r
            .try_get::<String, _>("conversation_id")
            .unwrap_or_default(),
        notify_target: r.try_get::<i16, _>("notify_target").unwrap_or(0) == 1,
        daily_report: r.try_get::<i16, _>("daily_report").unwrap_or(0) == 1,
        lineage_report: r.try_get::<i16, _>("lineage_report").unwrap_or(0) == 1,
        last_lineage_date: r
            .try_get::<String, _>("last_lineage_date")
            .unwrap_or_default(),
    }
}

/// 某团已绑定且至少开了一个推送开关的账号（推送引擎用）
pub async fn push_bindings_for_team(st: &AppState, team: &str) -> Vec<PushBinding> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    match sqlx::query(&crate::pq::rewrite(
        "SELECT name, team, person_id, conversation_id, notify_target, daily_report,
                lineage_report, last_lineage_date
         FROM push_bindings
         WHERE team = ? AND conversation_id <> '' AND (notify_target = 1 OR daily_report = 1 OR lineage_report = 1)",
    ))
    .bind(team)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows.iter().map(binding_from_row).collect(),
        Err(e) => {
            tracing::error!(error = %e, "查推送绑定失败");
            Vec::new()
        }
    }
}

/// 记录一次推送结果（成功清错误，失败留原因）
pub async fn push_touch(st: &AppState, name: &str, error: &str) {
    if let Ok(pool) = w(st) {
        let _ = sqlx::query(&crate::pq::rewrite(
            "UPDATE push_bindings SET last_push = ?, last_error = ? WHERE name = ?",
        ))
        .bind(now_str())
        .bind(error.chars().take(200).collect::<String>())
        .bind(name.trim())
        .execute(pool)
        .await;
    }
}

/// 每日报告去重：上次推过的业务日。同一天不再重复推同一份日报
/// （延迟队列只保证同一 (团, 日) 排一队，运营隔几小时补数据会再排一次）。
pub async fn push_last_report_date(st: &AppState, name: &str) -> String {
    let Ok(pool) = w(st) else {
        return String::new();
    };
    sqlx::query(&crate::pq::rewrite(
        "SELECT last_report_date FROM push_bindings WHERE name = ?",
    ))
    .bind(name.trim())
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .and_then(|r| r.try_get::<String, _>("last_report_date").ok())
    .unwrap_or_default()
}

pub async fn push_set_last_report_date(st: &AppState, name: &str, date: &str) {
    if let Ok(pool) = w(st) {
        let _ = sqlx::query(&crate::pq::rewrite(
            "UPDATE push_bindings SET last_report_date = ? WHERE name = ?",
        ))
        .bind(date)
        .bind(name.trim())
        .execute(pool)
        .await;
    }
}

/* --------------------- 微信绑定：绑定码（2026-10-16 口径） --------------------- */
/*
   2026-10-16 用户口径：**主播在微信里只发 4 位对接码即可对接**（不再要求手机号）。
   页面（展示端「我的」页）点「获取二维码」直接给二维码 + 对接码；
   对接动作由 [`push_bind_by_code`] 完成 —— 码一次性、30 分钟有效、用掉即清。
   表列名仍叫 `bind_code`（历史命名，不动库），对外文案一律「对接码」。

   历史沿革（都保留在代码里，别再走回去）：
   - 最早：页面出 6 位字母数字码 → 发码绑定；
   - 2026-10-10：改「扫码加好友 → 直接发手机号」绑定，码降为兜底；
   - 2026-10-10 审计 P1-1：只发手机号 = 知道手机号即可冒绑 + 枚举登记状态，
     改成二要素（手机号 + 6 位码），即 [`push_bind_by_phone_code`]（**现已不在机器人路径上**，
     保留给历史调用方与 e2e）；
   - 2026-10-16：二维码回归「加好友」用途（不再扫码即绑定），绑定回归单要素
     —— 但凭据是 4 位随机码而不是手机号，所以 P1-1 的「知道手机号就能冒绑」不成立。
*/

/// 二要素绑定（P1-1 历史口径，2026-10-16 起机器人不再走这条路）：
/// 手机号（my_accounts 里登记过的）+ 一次性绑定码（「我的」页生成的，30 分钟有效）。
/// 两条都命中才绑，码用掉即清。任一环节不匹配返回 None，调用方统一文案、不区分具体原因。
#[allow(dead_code)]
pub async fn push_bind_by_phone_code(
    st: &AppState,
    phone: &str,
    code: &str,
    conversation_id: &str,
    to_user_id: &str,
) -> Option<(String, String)> {
    let pool = w(st).ok()?;
    let phone = normalize_phone(phone);
    if phone.is_empty() || !phone.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // ① 手机号 → 账号姓名（没登记 = None，与码校验失败同一返回值，不泄露差异）
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT name FROM my_accounts WHERE phone = ?",
    ))
    .bind(&phone)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    let name: String = row.try_get::<String, _>("name").ok()?;
    if name.trim().is_empty() {
        return None;
    }
    // ② 绑定码校验：复用 push_bind_by_code 的查码 + 过期判断 + 会话抢占 + 码清空。
    //    它按码找行，这里还要确认那行就是手机号对应的账号，防止「甲的码绑乙的号」。
    let (code_name, team) = push_bind_by_code(st, code, conversation_id, to_user_id).await?;
    if code_name != name {
        // 码是对的但属于别的账号：把刚接过去的会话撤掉（该账号回到未绑定态，
        // 与「码用掉即清」一致），对外仍统一回复失败，不泄露两账号关系。
        let _ = sqlx::query(&crate::pq::rewrite(
            "UPDATE push_bindings SET conversation_id = '', to_user_id = '', bound_at = '',
               updated_at = ? WHERE name = ?",
        ))
        .bind(now_str())
        .bind(&code_name)
        .execute(pool)
        .await;
        return None;
    }
    Some((name, team))
}

/// 按手机号绑定微信会话（单要素）。返回 Some((账号姓名, 团别))。
/// 【2026-10-10 审计 P1-1 后线上路径不再用它】单凭手机号即可绑定 = 冒绑/枚举漏洞；
/// 2026-10-16 起机器人统一走 push_bind_by_code（4 位绑定码单要素）。
/// 保留原因：集成测试 store_e2e.rs 仍在直接调用它覆盖绑定表读写。
#[allow(dead_code)]
pub async fn push_bind_by_phone(
    st: &AppState,
    phone: &str,
    conversation_id: &str,
    to_user_id: &str,
) -> Option<(String, String)> {
    let pool = w(st).ok()?;
    let phone = normalize_phone(phone);
    if phone.is_empty() || !phone.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT name FROM my_accounts WHERE phone = ?",
    ))
    .bind(&phone)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    let name: String = row.try_get::<String, _>("name").ok()?;
    if name.trim().is_empty() {
        return None;
    }
    // my_accounts 只有姓名：团别要回台账反查（台账里没这个人 = 管理员观看账号，团别留空）
    let team = person_team_of_name(st, &name).await.unwrap_or_default();
    push_bind_account(st, &name, &team, conversation_id, to_user_id).await;
    Some((name, team))
}

/// 台账里查这个姓名属于哪个团（person.gender）；查不到返回 None
pub async fn person_team_of_name(st: &AppState, name: &str) -> Option<String> {
    let rows = crate::pgrs::query_with_binds(
        &st.pools,
        "LOCAL",
        "SELECT gender FROM person WHERE deleted_at IS NULL AND name = ? LIMIT 1",
        |q| q.bind(name.trim().to_string()),
    )
    .await
    .ok()?;
    let g = rows
        .first()
        .and_then(|r| r.try_get::<String, _>("gender").ok())
        .unwrap_or_default();
    match g.as_str() {
        "male" | "female" => Some(g),
        _ => None,
    }
}

/* --------------------- 用户自助扫码的微信推送账号（2026-10-11） --------------------- */

/// 用户扫码登录得到的微信账号凭证（wx_push_accounts 一行）
#[derive(Clone, Debug, Default)]
pub struct WxPushAccount {
    pub name: String,
    pub team: String,
    pub person_id: u64,
    pub ilink_bot_id: String,
    pub ilink_user_id: String,
    pub bot_token: String,
    pub base_url: String,
    pub nickname: String,
    pub conversation_id: String,
    pub updates_buf: String,
    pub bound_at: String,
}

fn wx_push_account_from_row(row: &sqlx::postgres::PgRow) -> WxPushAccount {
    WxPushAccount {
        name: row.try_get::<String, _>("name").unwrap_or_default(),
        team: row.try_get::<String, _>("team").unwrap_or_default(),
        person_id: row
            .try_get::<i64, _>("person_id")
            .map(|v| v.max(0) as u64)
            .unwrap_or(0),
        ilink_bot_id: row.try_get::<String, _>("ilink_bot_id").unwrap_or_default(),
        ilink_user_id: row
            .try_get::<String, _>("ilink_user_id")
            .unwrap_or_default(),
        bot_token: row.try_get::<String, _>("bot_token").unwrap_or_default(),
        base_url: row.try_get::<String, _>("base_url").unwrap_or_default(),
        nickname: row.try_get::<String, _>("nickname").unwrap_or_default(),
        conversation_id: row
            .try_get::<String, _>("conversation_id")
            .unwrap_or_default(),
        updates_buf: row.try_get::<String, _>("updates_buf").unwrap_or_default(),
        bound_at: row.try_get::<String, _>("bound_at").unwrap_or_default(),
    }
}

/// 保存 / 覆盖某个展示端账号的微信扫码凭证
/// 扫码凭证的行内冲突判定（纯函数，2026-10-12 审计整改）。
///
/// 018 给 `wx_push_accounts` 加了两个软唯一索引（`uk_wx_push_user_uniq` / `uk_wx_push_person_uniq`），
/// 但唯一索引抛出来的是 `Duplicate entry`，运营看不懂、也没法据此决定留哪一行。
/// 所以落库前先查一次，命中就返回人话文案。
///
/// 两条「不算冲突」的边界，缺一个就会把正常路径打死：
/// - **自己更新自己**：`wxpush.rs` 的 `supervisor_loop` / `ensure_account` 会反复 save
///   同一个账号（重连、token 刷新），排除自身 `name` 才能保持幂等；
/// - **空值不参与**：`ilink_user_id` 空串、`person_id = 0` 是「还没激活 / 管理员观看账号」
///   的哨兵语义（015 定的），生成列把它们变 NULL 不进唯一索引，查重口径必须一致。
///
/// 入参 `existing` 是「除自己以外的其它凭证行」(name, person_id, ilink_user_id)。
pub fn wx_push_clash_of(
    self_name: &str,
    person_id: u64,
    ilink_user_id: &str,
    existing: &[(String, u64, String)],
) -> Option<String> {
    let me = self_name.trim();
    let user = ilink_user_id.trim();
    for (name, pid, uid) in existing {
        if name.trim() == me {
            continue; // 自己更新自己：不算冲突
        }
        if !user.is_empty() && uid.trim() == user {
            return Some(format!(
                "该微信已经绑定在「{name}」的账号上；一个微信只能绑一个主播，请先解绑旧的再扫"
            ));
        }
        if person_id > 0 && *pid == person_id && *pid > 0 {
            return Some(format!(
                "该主播已经有一个微信推送账号（「{name}」）；要换微信请先解绑旧的"
            ));
        }
    }
    None
}

/// 扫码绑定的**业务冲突**（微信已绑别的账号 / 该主播已有微信账号 / 唯一索引兜底）。
///
/// 为什么单独建一个类型（2026-10-16）：这类拒绝是写给用户看的**人话**
/// （「请先解绑旧的再扫」），但它从 store 层经 `anyhow` 抛到路由层后，
/// `From<anyhow::Error>` 一律算 `safe=false` → 500 + 通用文案「服务器开小差了，请稍后重试」，
/// 用户既看不到原因、展示端还会把它当网络抖动每 3 秒重试 10 次（2026-10-16 实测）。
/// 带上本类型后，路由层能 `downcast_ref` 出来按 400 原样下发 ——
/// 与 routes.rs「不靠中文关键词猜状态码」的口径一致，也不泄露 PG 细节。
#[derive(Debug)]
pub struct WxBindConflict(pub String);

impl std::fmt::Display for WxBindConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for WxBindConflict {}

/// 冲突文案 → 带类型的业务冲突错误（路由层 downcast 出来下发 400）。
/// 单独抽一层是为了能单测「文案进、typed error 出」这段映射（不需要 DB）。
fn bind_conflict(msg: String) -> anyhow::Error {
    WxBindConflict(msg).into()
}

/// 落扫码凭证前把「别人的行」捞出来查重（排除自身，见 [`wx_push_clash_of`]）。
/// 018 没跑时 `ilink_user_id` 列存在（013 就有）、`person_id` 也在（015 才加），
/// 所以 015 未跑的库这里会拿到空列表 → 查重自然退化，不影响写入。
async fn wx_push_others(st: &AppState) -> Vec<(String, u64, String)> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    let rows = sqlx::query(
        "SELECT name, COALESCE(person_id, 0) AS pid, COALESCE(ilink_user_id, '') AS uid \
         FROM wx_push_accounts",
    )
    .fetch_all(pool)
    .await;
    match rows {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                (
                    r.try_get::<String, _>("name").unwrap_or_default(),
                    r.try_get::<i64, _>("pid")
                        .map(|v| v.max(0) as u64)
                        .unwrap_or(0),
                    r.try_get::<String, _>("uid").unwrap_or_default(),
                )
            })
            .collect(),
        Err(e) => {
            // 查重失败不阻断写入：唯一索引还在，最坏情况是拿到 Duplicate entry 文案
            tracing::warn!(error = %e, "扫码凭证查重失败，退回唯一索引兜底");
            Vec::new()
        }
    }
}

pub async fn wx_push_account_save(st: &AppState, a: &WxPushAccount) -> Result<()> {
    let pool = w(st)?;
    /* 一人一 bot（018）：先查重再写，把 Duplicate entry 换成运营看得懂的文案。
    查重失败（表结构缺列 / 连不上）不阻断 —— 唯一索引仍在，最坏退回英文报错。 */
    let others = wx_push_others(st).await;
    if let Some(msg) = wx_push_clash_of(&a.name, a.person_id, &a.ilink_user_id, &others) {
        // 业务冲突：带上类型，路由层按 400 原样下发（见 WxBindConflict）
        return Err(bind_conflict(msg));
    }
    sqlx::query(&crate::pq::rewrite(
        "INSERT INTO wx_push_accounts
           (name, team, person_id, ilink_bot_id, ilink_user_id, bot_token, base_url,
            nickname, conversation_id, updates_buf, bound_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (name) DO UPDATE SET
           team = EXCLUDED.team, person_id = EXCLUDED.person_id,
           ilink_bot_id = EXCLUDED.ilink_bot_id, ilink_user_id = EXCLUDED.ilink_user_id,
           bot_token = EXCLUDED.bot_token, base_url = EXCLUDED.base_url,
           nickname = EXCLUDED.nickname, updated_at = EXCLUDED.updated_at",
    ))
    .bind(a.name.trim())
    .bind(&a.team)
    .bind(a.person_id as i64)
    .bind(&a.ilink_bot_id)
    .bind(&a.ilink_user_id)
    .bind(&a.bot_token)
    .bind(&a.base_url)
    .bind(&a.nickname)
    .bind(&a.conversation_id)
    .bind(&a.updates_buf)
    .bind(&a.bound_at)
    .bind(now_str())
    .execute(pool)
    .await
    .map_err(|e| {
        /* 并发穿过上面的预检：唯一索引兜底。按撞到的键给文案
        （018 的两个软唯一索引：微信侧 uk_wx_push_user_uniq / 主播侧 uk_wx_push_person_uniq）。 */
        let m = e.to_string();
        if m.contains("Duplicate entry") {
            // 唯一索引兜底文案同上：全是用户可执行的人话，一律按业务冲突下发 400
            let msg = if m.contains("uk_wx_push_user_uniq") {
                "该微信已经绑定过别的账号，一个微信只能绑一个主播"
            } else if m.contains("uk_wx_push_person_uniq") {
                "该主播已经有一个微信推送账号，要换微信请先解绑旧的"
            } else {
                "该微信推送账号已存在，请刷新后重试"
            };
            bind_conflict(msg.to_string())
        } else {
            anyhow::anyhow!("写入微信推送凭证失败：{m}")
        }
    })?;
    Ok(())
}

/// 更新长轮询游标（仅非空时更新，避免把进度打回去）
pub async fn wx_push_account_set_updates_buf(st: &AppState, name: &str, updates_buf: &str) -> bool {
    if updates_buf.is_empty() {
        return false;
    }
    matches!(w(st), Ok(pool) if sqlx::query(&crate::pq::rewrite("UPDATE wx_push_accounts SET updates_buf = ?, updated_at = ? WHERE name = ?"))
            .bind(updates_buf)
    .bind(now_str())
    .bind(name.trim())
    .execute(pool)
    .await
    .map(|r| r.affected() > 0)
    .unwrap_or(false))
}

/// 首次收到该用户消息时补会话（conversation_id / bound_at）
pub async fn wx_push_account_bind_conversation(
    st: &AppState,
    name: &str,
    conversation_id: &str,
    ilink_user_id: &str,
) -> bool {
    matches!(w(st), Ok(pool) if sqlx::query(&crate::pq::rewrite("UPDATE wx_push_accounts
           SET conversation_id = ?, ilink_user_id = COALESCE(NULLIF(ilink_user_id, ''), ?),
               bound_at = COALESCE(NULLIF(bound_at, ''), ?), updated_at = ?
         WHERE name = ?"))
            .bind(conversation_id)
    .bind(ilink_user_id)
    .bind(now_str())
    .bind(now_str())
    .bind(name.trim())
    .execute(pool)
    .await
    .map(|r| r.affected() > 0)
    .unwrap_or(false))
}

/// 按展示端账号名取扫码凭证
pub async fn wx_push_account_get(st: &AppState, name: &str) -> Option<WxPushAccount> {
    let pool = w(st).ok()?;
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT name, team, person_id, ilink_bot_id, ilink_user_id, bot_token, base_url,
                nickname, conversation_id, updates_buf, bound_at
         FROM wx_push_accounts WHERE name = ? LIMIT 1",
    ))
    .bind(name.trim())
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    Some(wx_push_account_from_row(&row))
}

/// to_user_id（ilink_user_id）→ 扫码账号展示端名。出网白名单窄口用；
/// 查不到 = None（非扫码通道或收件人不是已扫码账号）。
pub async fn wx_push_account_name_of(st: &AppState, to_user_id: &str) -> Option<String> {
    let pool = w(st).ok()?;
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT name FROM wx_push_accounts WHERE ilink_user_id = ? LIMIT 1",
    ))
    .bind(to_user_id.trim())
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    row.try_get::<String, _>("name").ok()
}

/// 启动时恢复全部扫码账号（各自跑一条长轮询）
pub async fn wx_push_accounts_all(st: &AppState) -> Vec<WxPushAccount> {    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    sqlx::query(
        "SELECT name, team, person_id, ilink_bot_id, ilink_user_id, bot_token, base_url,
                nickname, conversation_id, updates_buf, bound_at
         FROM wx_push_accounts ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .map(|rows| rows.iter().map(wx_push_account_from_row).collect())
    .unwrap_or_default()
}

/// 删除扫码凭证（解绑推送时一并清）
pub async fn wx_push_account_delete(st: &AppState, name: &str) -> bool {
    matches!(w(st), Ok(pool) if sqlx::query(&crate::pq::rewrite("DELETE FROM wx_push_accounts WHERE name = ?"))
        .bind(name.trim())
        .execute(pool)
        .await
        .map(|r| r.affected() > 0)
        .unwrap_or(false))
}

/* --------------- 追分目标按 kind 分类计数（2026-10-15 契约 B 节） --------------- */

/// 追分目标按 kind 分类计数，返回 `(person, value, rank)`。
///
/// 用户口径「分数是分数，追人是追人」：追人（person）与追分数（value）各自独立
/// 计上限（都沿用 [`crate::push::MAX_TARGETS`]），所以上限校验不能再数总数。
/// 一条 `GROUP BY kind` 查询拿全三类，顺带给 `GET /my/push` 的 `counts` 回显用。
///
/// 与既有 `push_jobs::target_count` 同口径的失败语义：没库/查询失败一律回 `(0,0,0)`
/// （调用方随后真正写库时才会暴露 DB 故障，这里不额外改行为）。
pub async fn target_counts(st: &AppState, name: &str) -> (i64, i64, i64) {
    let Ok(pool) = w(st) else {
        return (0, 0, 0);
    };
    match sqlx::query(&crate::pq::rewrite(
        "SELECT kind, COUNT(*) AS n FROM my_targets WHERE name = ? GROUP BY kind",
    ))
    .bind(name.trim())
    .fetch_all(pool)
    .await
    {
        Ok(rows) => {
            let mut out = (0i64, 0i64, 0i64);
            for r in rows.iter() {
                let n = r.try_get::<i64, _>("n").unwrap_or(0);
                match r.try_get::<String, _>("kind").unwrap_or_default().as_str() {
                    "person" => out.0 = n,
                    "value" => out.1 = n,
                    "rank" => out.2 = n,
                    _ => {}
                }
            }
            out
        }
        Err(e) => {
            tracing::error!(error = %e, "统计追分目标分类条数失败");
            (0, 0, 0)
        }
    }
}

/* ---------------- 追分目标写入：事务内串行化（FIX1 · 2026-10-15 qa 并发穿透） ---------------- */

/// 一次事务里要插入的一条目标（调用方已按语义准备好）。
#[derive(Clone, Debug)]
pub struct NewTarget {
    /// 追人（kind=person）用：判重 + 落 target_person_id；value/rank 恒 0
    pub person_id: u64,
    /// 展示文案（person = 主播名；value/rank 一般传空串）
    pub name: String,
    /// person = 自定义目标分（0 = 没填）；value = 原始音浪；rank = 名次
    pub value: i64,
}

/// 同 kind 重复目标的处理策略（三条既有口径各不一样，不能一刀切）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DupPolicy {
    /// 不查重：value 历史上就允许同一个数值目标并存
    Ignore,
    /// 查到重复整单拒绝：单条 person/rank（保持既有 400 文案）
    Reject,
    /// 重复的跳过不插：批量 person（计进 skipped）
    Skip,
}

/// 目标写入的结构化失败原因。文案由 handler（wx.rs）出，store 只给原因，
/// 这样 store 层不掺 UI 文案，也不会把 sqlx/PG 细节漏给客户端。
#[derive(Debug)]
pub enum TargetAddError {
    /// 单类上限已满（person / value / rank 各自 ≤ push::MAX_TARGETS）
    OverLimit,
    /// 同 kind 的重复目标（仅 `DupPolicy::Reject` 会产生）
    Duplicate,
    /// 写库失败（原文只进日志，不下发客户端）
    Db(anyhow::Error),
}

/// 底层错误（sqlx::Error / anyhow::Error）统一收成 `TargetAddError::Db`：
/// `map_err` 里直接写闭包，省得为两种错误各写一次 From。
fn db_err(e: impl std::fmt::Display) -> TargetAddError {
    TargetAddError::Db(anyhow::anyhow!("{e}"))
}

/// 纯计算（无 IO，便于单测）：从「候选键 + 已有键集合 + 单类现有条数」算出
/// **本次真正要插入的下标**（保序）。
///
/// - `Reject`：候选里有已在的 → `Err(Duplicate)`；`Skip`：跳过，继续看下一个；
///   `Ignore`：不查重（候选之间也不去重，保持 value 的历史行为）。
/// - 上限按「现有条数 + 本次要插的条数」一起算，超了 → `Err(OverLimit)`。
///   注意只重复提交已满名单（新增 0 条）不会报错，语义与旧实现一致。
fn plan_inserts(
    keys: &[i64],
    existing: &std::collections::HashSet<i64>,
    used: i64,
    max_per_kind: i64,
    dup: DupPolicy,
) -> Result<Vec<usize>, TargetAddError> {
    let mut pending: Vec<usize> = Vec::with_capacity(keys.len());
    let mut seen: std::collections::HashSet<i64> = existing.clone();
    // 单条入口（Reject）的历史口径是「先判上限、再判重复」；批量（Skip）不产生
    // Duplicate，所以这里只对 Reject 保留同样的优先级，两种 400 同时成立时文案不变。
    if dup == DupPolicy::Reject && used >= max_per_kind {
        return Err(TargetAddError::OverLimit);
    }
    for (i, key) in keys.iter().enumerate() {
        if dup != DupPolicy::Ignore && seen.contains(key) {
            match dup {
                DupPolicy::Reject => return Err(TargetAddError::Duplicate),
                DupPolicy::Skip => continue,
                DupPolicy::Ignore => unreachable!("Ignore 已在上面短路"),
            }
        }
        seen.insert(*key);
        pending.push(i);
    }
    if used + pending.len() as i64 > max_per_kind {
        return Err(TargetAddError::OverLimit);
    }
    Ok(pending)
}

/// 追分目标写入（上限校验 + 去重 + 逐条 INSERT）——**两条入口共用**：
/// 批量 `POST /my/push/targets/batch`（`DupPolicy::Skip`）与单条 `POST /my/push/targets`
/// （person/rank 用 `Reject`、value 用 `Ignore`）。
///
/// FIX1（qa 实测：空态 4 路并发 batch 各插 5 个 → DB 10 行；3 路并发同 id → 2 行重复；
/// 6 路并发单条 value → 6 行）：旧实现是「先 COUNT/先查重 → 再逐条 INSERT」的
/// check-then-insert，`my_targets` 又没有 (name,kind,target_person_id) 唯一约束，
/// 并发下必然穿越。现在整段在**同一事务**里按账号串行化：
/// `pg_advisory_xact_lock(hashtext(账号名))` → 重读分类计数与已存在键 → 校验 → 插入 → 提交。
/// 锁在提交/回滚时自动释放，不需要新迁移，也不依赖唯一索引。
///
/// 返回本次真正插入的条数（`added`）。批量的 `skipped` 由调用方按
/// 「请求原始条数 − added」计算（契约不变）。
pub async fn targets_add_serialized(
    st: &AppState,
    name: &str,
    team: &str,
    kind: &str,
    items: &[NewTarget],
    max_per_kind: i64,
    dup: DupPolicy,
) -> Result<usize, TargetAddError> {
    let pool = w(st).map_err(db_err)?;
    targets_add_serialized_pool(pool, name, team, kind, items, max_per_kind, dup).await
}

/// [`targets_add_serialized`] 的事务本体。单独暴露是为了能在集成测试里拿一个
/// `PgPool` 直接跑真并发回归（不用起 HTTP 服务，也就不会碰到线上那套机器人/推送循环）。
pub async fn targets_add_serialized_pool(
    pool: &PgPool,
    name: &str,
    team: &str,
    kind: &str,
    items: &[NewTarget],
    max_per_kind: i64,
    dup: DupPolicy,
) -> Result<usize, TargetAddError> {
    let name = name.trim();
    if items.is_empty() {
        return Ok(0);
    }
    let mut tx = pool.begin().await.map_err(db_err)?;
    // 1) 按账号取事务级咨询锁：两条入口共用同一把，互相之间也串行。
    //    （hashtext 冲突只意味着两个账号偶尔多串一会儿，不影响正确性。）
    sqlx::query(&crate::pq::rewrite(
        "SELECT pg_advisory_xact_lock(hashtext(?))",
    ))
    .bind(name)
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;
    // 2) 事务内重读：已有键（判重用）+ 单类现有条数（上限用）。
    //    person 比 target_person_id，value/rank 比 target_value。
    let key_sql = if kind == "person" {
        "SELECT target_person_id AS v FROM my_targets WHERE name = ? AND kind = ?"
    } else {
        "SELECT target_value AS v FROM my_targets WHERE name = ? AND kind = ?"
    };
    let existing: std::collections::HashSet<i64> = if dup == DupPolicy::Ignore {
        std::collections::HashSet::new()
    } else {
        sqlx::query(&crate::pq::rewrite(key_sql))
            .bind(name)
            .bind(kind)
            .fetch_all(&mut *tx)
            .await
            .map_err(db_err)?
            .iter()
            .map(|r| r.try_get::<i64, _>("v").unwrap_or(0))
            .collect()
    };
    let used: i64 = sqlx::query(&crate::pq::rewrite(
        "SELECT COUNT(*) AS n FROM my_targets WHERE name = ? AND kind = ?",
    ))
    .bind(name)
    .bind(kind)
    .fetch_one(&mut *tx)
    .await
    .map_err(db_err)?
    .try_get::<i64, _>("n")
    .unwrap_or(0);
    // 3) 纯计算要插哪几条；OverLimit / Duplicate 在这里返回（tx 随之 drop → 自动回滚）。
    let keys: Vec<i64> = items
        .iter()
        .map(|it| {
            if kind == "person" {
                it.person_id as i64
            } else {
                it.value
            }
        })
        .collect();
    let pending = plan_inserts(&keys, &existing, used, max_per_kind, dup)?;
    if pending.is_empty() {
        return Ok(0);
    }
    // 4) 串行区内逐条插入 + 提交。
    for &i in &pending {
        let it = &items[i];
        sqlx::query(&crate::pq::rewrite(
            "INSERT INTO my_targets (name, team, kind, target_person_id, target_name, target_value, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(name)
        .bind(team)
        .bind(kind)
        .bind(it.person_id as i64)
        .bind(&it.name)
        .bind(it.value)
        .bind(now_str())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
    }
    tx.commit().await.map_err(db_err)?;
    Ok(pending.len())
}

/// 把已存在的展示端账号接到微信会话上（手机号 / 绑定码两条路共用）
pub async fn push_bind_account(
    st: &AppState,
    name: &str,
    team: &str,
    conversation_id: &str,
    to_user_id: &str,
) {
    // 修复3·健壮：与 push_set_code 同一口径的 team 白名单（team 会落 push_bindings.team，
    // report.rs 用 `p.gender='{team}'` 内联拼 SQL）。合法值只有 male/female/all，
    // 空串（台账查不到的观看账号）按 all 归一，其余一律拒绝写库。
    // 2026-10-16：非法值原先会被 .bind(None) 落成 NULL → NOT NULL 约束报错，而下面的
    // 写入是 `let _ =`（静默）→ 「绑定看似成功、库里没行」。这里显式拒绝 + 记 warn。
    let Some(team) = normalize_push_team(team) else {
        tracing::warn!(account = %name, "push_bindings 未写入：团别非法（只认 male/female/all/空）");
        return;
    };
    let Ok(pool) = w(st) else {
        return;
    };
    // 同一个微信会话只能绑一个账号：先把占着这个会话的旧绑定清掉，
    // 否则换号后旧账号还在收推送。
    let _ = sqlx::query(&crate::pq::rewrite(
        "UPDATE push_bindings SET conversation_id = '', to_user_id = '', bound_at = '',
           bind_code = '', code_expires = '', updated_at = ?
         WHERE conversation_id = ? AND name <> ?",
    ))
    .bind(now_str())
    .bind(conversation_id)
    .bind(name.trim())
    .execute(pool)
    .await;
    // 静默吞错改记 warn（2026-10-16 报障：这里写失败时展示端永远显示未绑定，
    // 而日志里一条线索都没有——根因是 conversation_id 列长不够，见迁移 027）。
    if let Err(e) = sqlx::query(&crate::pq::rewrite(
        "INSERT INTO push_bindings (name, team, conversation_id, to_user_id, bound_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (name) DO UPDATE SET team = CASE
           WHEN EXCLUDED.team <> '' THEN EXCLUDED.team
           ELSE push_bindings.team
         END,
           conversation_id = EXCLUDED.conversation_id, to_user_id = EXCLUDED.to_user_id,
           bound_at = EXCLUDED.bound_at, bind_code = '', code_expires = '',
           last_error = '', updated_at = EXCLUDED.updated_at",
    ))
    .bind(name.trim())
    .bind(team)
    .bind(conversation_id)
    .bind(to_user_id)
    .bind(now_str())
    .bind(now_str())
    .execute(pool)
    .await
    {
        tracing::warn!(
            account = %name, error = %e, conv_len = conversation_id.chars().count(),
            "push_bindings 写入失败（展示端会一直显示未绑定）"
        );
    }
}

/* ------------- 追分目标写入单测：plan_inserts 纯逻辑 + 真库并发回归（FIX1） ------------- */

#[cfg(test)]
mod target_write_tests {
    use super::*;
    use std::collections::HashSet;

    fn set(v: &[i64]) -> HashSet<i64> {
        v.iter().copied().collect()
    }

    fn plan_ok(keys: &[i64], existing: &[i64], used: i64, max: i64, dup: DupPolicy) -> Vec<usize> {
        match plan_inserts(keys, &set(existing), used, max, dup) {
            Ok(v) => v,
            Err(e) => panic!("期望通过，实际 {e:?}"),
        }
    }

    fn plan_err(
        keys: &[i64],
        existing: &[i64],
        used: i64,
        max: i64,
        dup: DupPolicy,
    ) -> TargetAddError {
        match plan_inserts(keys, &set(existing), used, max, dup) {
            Ok(v) => panic!("期望报错，实际通过 {v:?}"),
            Err(e) => e,
        }
    }

    /// Skip：已在追的跳过，返回的是**下标**（保序）。
    #[test]
    fn plan_skips_existing_targets() {
        // keys=[1,2,3]，已在追 1 → 只插 2、3（下标 1、2）
        assert_eq!(plan_ok(&[1, 2, 3], &[1], 1, 5, DupPolicy::Skip), vec![1, 2]);
    }

    /// 上限按「现有条数 + 本次要插的条数」一起算。
    #[test]
    fn plan_enforces_max_on_existing_plus_new() {
        // 已有 4 个 + 新 1 个 = 5，刚好到顶 → 通过
        assert_eq!(plan_ok(&[5], &[1, 2, 3, 4], 4, 5, DupPolicy::Skip), vec![0]);
        // 已有 4 个 + 新 2 个 = 6 > 5 → OverLimit
        assert!(matches!(
            plan_err(&[5, 6], &[1, 2, 3, 4], 4, 5, DupPolicy::Skip),
            TargetAddError::OverLimit
        ));
    }

    /// 已满时重复提交全是在追的名单：新增 0 条，不能报错（契约语义）。
    #[test]
    fn plan_resubmitting_full_list_is_not_overflow() {
        assert_eq!(
            plan_ok(&[1, 2, 3, 4, 5], &[1, 2, 3, 4, 5], 5, 5, DupPolicy::Skip),
            Vec::<usize>::new()
        );
    }

    /// 请求内重复：Skip 只保首位，`skipped` 仍由调用方按「原始条数 − added」算。
    #[test]
    fn skip_count_matches_contract_formula() {
        // raw=[7,7,1]、已在追 1 → added=1、skipped=3-1=2
        let keys = [7, 7, 1];
        let added = plan_ok(&keys, &[1], 1, 5, DupPolicy::Skip).len();
        assert_eq!(added, 1);
        assert_eq!(keys.len() - added, 2, "重复项 + 已存在项都要算进 skipped");
    }

    /// Reject（单条 person/rank）：重复 → Duplicate（handler 映射成既有 400 文案）。
    #[test]
    fn reject_policy_duplicate() {
        assert!(matches!(
            plan_err(&[9], &[9], 1, 5, DupPolicy::Reject),
            TargetAddError::Duplicate
        ));
        // 请求内两个相同键也一样拒
        assert!(matches!(
            plan_err(&[9, 9], &[], 0, 5, DupPolicy::Reject),
            TargetAddError::Duplicate
        ));
    }

    /// 单条入口的历史优先级：已满 + 重复同时成立时报「上限」，不是「重复」。
    #[test]
    fn reject_policy_over_limit_takes_precedence() {
        assert!(matches!(
            plan_err(&[9], &[9], 5, 5, DupPolicy::Reject),
            TargetAddError::OverLimit
        ));
    }

    /// Ignore（单条 value）：不查重，同一个数值可以并存（保持历史行为），只受上限约束。
    #[test]
    fn ignore_policy_allows_same_value_twice() {
        assert_eq!(
            plan_ok(&[100, 100], &[100], 1, 5, DupPolicy::Ignore),
            vec![0, 1]
        );
        assert!(matches!(
            plan_err(&[1, 2, 3, 4, 5, 6], &[], 0, 5, DupPolicy::Ignore),
            TargetAddError::OverLimit
        ));
    }

    /* --------------------------- 真库并发回归（默认跳过） --------------------------- */

    /// 拿写池；没设 `PK_IT_DB=1` / 缺 `DB_WRITER_*` 就返回 None（默认 cargo test 不碰库）。
    /// 跑法：
    /// `cd rust && set -a && . ./.env && set +a && PK_IT_DB=1 cargo test -p pk-server --lib \
    ///   target_write_tests::concurrency_regression_real_db -- --nocapture`
    async fn it_pool() -> Option<sqlx::PgPool> {
        if std::env::var("PK_IT_DB").as_deref() != Ok("1") {
            return None;
        }
        let host = std::env::var("DB_WRITER_HOST").ok()?;
        let port: u16 = std::env::var("DB_WRITER_PORT").ok()?.parse().ok()?;
        let user = std::env::var("DB_WRITER_USER").ok()?;
        let pass = std::env::var("DB_WRITER_PASSWORD").unwrap_or_default();
        let db = std::env::var("DB_WRITER_NAME").ok()?;
        let opt = sqlx::postgres::PgConnectOptions::new()
            .host(&host)
            .port(port)
            .username(&user)
            .password(&pass)
            .database(&db);
        match sqlx::PgPool::connect_with(opt).await {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("[skip] 连写库失败：{e}");
                None
            }
        }
    }

    async fn count_kind(pool: &sqlx::PgPool, name: &str, kind: &str) -> i64 {
        sqlx::query(&crate::pq::rewrite(
            "SELECT COUNT(*) AS n FROM my_targets WHERE name = ? AND kind = ?",
        ))
        .bind(name)
        .bind(kind)
        .fetch_one(pool)
        .await
        .expect("count 查询失败")
        .try_get::<i64, _>("n")
        .unwrap_or(0)
    }

    /// 有重复 target_person_id 的分组数（0 = 无重复行）
    async fn dup_person_groups(pool: &sqlx::PgPool, name: &str) -> i64 {
        sqlx::query(&crate::pq::rewrite(
            "SELECT COUNT(*) AS n FROM (SELECT target_person_id FROM my_targets
             WHERE name = ? AND kind = 'person'
             GROUP BY target_person_id HAVING COUNT(*) > 1) d",
        ))
        .bind(name)
        .fetch_one(pool)
        .await
        .expect("dup 查询失败")
        .try_get::<i64, _>("n")
        .unwrap_or(0)
    }

    async fn cleanup(pool: &sqlx::PgPool, name: &str) {
        let _ = sqlx::query(&crate::pq::rewrite("DELETE FROM my_targets WHERE name = ?"))
            .bind(name)
            .execute(pool)
            .await;
    }

    fn items(ids: &[u64]) -> Vec<NewTarget> {
        ids.iter()
            .map(|id| NewTarget {
                person_id: *id,
                name: format!("t{id}"),
                value: 0,
            })
            .collect()
    }

    /// FIX1 回归：真并发打 `targets_add_serialized_pool`（handler 用的就是它），
    /// 断言单类上限 ≤5、无重复行、added 之和 ≤5。临时数据用 `__e2e_fix_t6_<pid>` 前缀，
    /// 每段断言前先删干净（断言放在 cleanup 之后，失败也不留脏数据）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrency_regression_real_db() {
        let Some(pool) = it_pool().await else {
            eprintln!("[skip] 未设置 PK_IT_DB=1 / DB_WRITER_*，跳过真库并发回归");
            return;
        };
        let name = format!("__e2e_fix_t6_{}", std::process::id());

        // ---- A) 空态 6 路并发 batch（各 5 个不同 id）→ 只能落下 5 行 ----
        cleanup(&pool, &name).await;
        let mut hs = Vec::new();
        for k in 0..6u64 {
            let pool = pool.clone();
            let name = name.clone();
            hs.push(tokio::spawn(async move {
                let ids: Vec<u64> = (0..5).map(|i| k * 5 + i + 1).collect();
                targets_add_serialized_pool(
                    &pool,
                    &name,
                    "male",
                    "person",
                    &items(&ids),
                    5,
                    DupPolicy::Skip,
                )
                .await
            }));
        }
        let (mut oks, mut over, mut added_sum) = (0usize, 0usize, 0usize);
        for h in hs {
            match h.await.expect("join") {
                Ok(n) => {
                    oks += 1;
                    added_sum += n;
                }
                Err(TargetAddError::OverLimit) => over += 1,
                Err(e) => panic!("不应出现的错误：{e:?}"),
            }
        }
        let a_rows = count_kind(&pool, &name, "person").await;
        let a_dups = dup_person_groups(&pool, &name).await;
        cleanup(&pool, &name).await;
        let a_left = count_kind(&pool, &name, "person").await;
        println!(
            "[A] 6 路并发 batch(各 5 id)：200×{oks} / 400(OverLimit)×{over}；added 之和={added_sum}；\
             DB person 行数={a_rows}；重复 id 组={a_dups}；清理后残留={a_left}"
        );
        assert_eq!(a_left, 0, "临时数据必须删净");
        assert!(
            a_rows <= 5,
            "person 行数必须 ≤5（旧实现实测 10），实际 {a_rows}"
        );
        assert!(added_sum <= 5, "added 之和必须 ≤5，实际 {added_sum}");
        assert_eq!(a_dups, 0, "不允许重复 target_person_id");

        // ---- B) 3 路并发同一个 id ----
        // B1 批量（Skip）：只落 1 行、added 之和 = 1
        cleanup(&pool, &name).await;
        let mut hs = Vec::new();
        for _ in 0..3 {
            let pool = pool.clone();
            let name = name.clone();
            hs.push(tokio::spawn(async move {
                targets_add_serialized_pool(
                    &pool,
                    &name,
                    "male",
                    "person",
                    &items(&[4242]),
                    5,
                    DupPolicy::Skip,
                )
                .await
            }));
        }
        let mut b1_added = 0usize;
        for h in hs {
            b1_added += h.await.expect("join").expect("Skip 策略不该报错");
        }
        let b1_rows = count_kind(&pool, &name, "person").await;
        cleanup(&pool, &name).await;
        println!("[B1] 3 路并发同 id（batch/Skip）：added 之和={b1_added}；DB 行数={b1_rows}");
        assert_eq!(b1_rows, 1, "3 路并发同一 id 必须恰好 1 行，实际 {b1_rows}");
        assert_eq!(b1_added, 1, "added 之和必须 = 1，实际 {b1_added}");

        // B2 单条（Reject）：恰好 1 个成功、2 个 Duplicate
        cleanup(&pool, &name).await;
        let mut hs = Vec::new();
        for _ in 0..3 {
            let pool = pool.clone();
            let name = name.clone();
            hs.push(tokio::spawn(async move {
                targets_add_serialized_pool(
                    &pool,
                    &name,
                    "male",
                    "person",
                    &items(&[4243]),
                    5,
                    DupPolicy::Reject,
                )
                .await
            }));
        }
        let (mut b2_ok, mut b2_dup) = (0usize, 0usize);
        for h in hs {
            match h.await.expect("join") {
                Ok(_) => b2_ok += 1,
                Err(TargetAddError::Duplicate) => b2_dup += 1,
                Err(e) => panic!("不应出现的错误：{e:?}"),
            }
        }
        let b2_rows = count_kind(&pool, &name, "person").await;
        cleanup(&pool, &name).await;
        println!(
            "[B2] 3 路并发同 id（single/Reject）：成功×{b2_ok} / Duplicate×{b2_dup}；DB 行数={b2_rows}"
        );
        assert_eq!(b2_rows, 1, "3 路并发同一 id 必须恰好 1 行，实际 {b2_rows}");
        assert_eq!((b2_ok, b2_dup), (1, 2), "应恰好 1 成功 + 2 Duplicate");

        // ---- C) 6 路并发单条 kind=value（各不同值）→ ≤5 行 ----
        cleanup(&pool, &name).await;
        let mut hs = Vec::new();
        for i in 1..=6i64 {
            let pool = pool.clone();
            let name = name.clone();
            hs.push(tokio::spawn(async move {
                let one = [NewTarget {
                    person_id: 0,
                    name: String::new(),
                    value: i * 10_000,
                }];
                targets_add_serialized_pool(
                    &pool,
                    &name,
                    "male",
                    "value",
                    &one,
                    5,
                    DupPolicy::Ignore,
                )
                .await
            }));
        }
        let (mut c_ok, mut c_over) = (0usize, 0usize);
        for h in hs {
            match h.await.expect("join") {
                Ok(_) => c_ok += 1,
                Err(TargetAddError::OverLimit) => c_over += 1,
                Err(e) => panic!("不应出现的错误：{e:?}"),
            }
        }
        let c_rows = count_kind(&pool, &name, "value").await;
        cleanup(&pool, &name).await;
        println!(
            "[C] 6 路并发单条 value：成功×{c_ok} / 400(OverLimit)×{c_over}；DB value 行数={c_rows}"
        );
        assert!(
            c_rows <= 5,
            "value 行数必须 ≤5（旧实现实测 6），实际 {c_rows}"
        );
        assert_eq!(c_ok, c_rows as usize, "成功数必须等于落库行数");

        // ---- D) 分类互不挤占：person 满 5 后 value 仍能加满 5 ----
        cleanup(&pool, &name).await;
        let p_ids: Vec<u64> = (0..5).map(|i| 9000 + i).collect();
        targets_add_serialized_pool(
            &pool,
            &name,
            "male",
            "person",
            &items(&p_ids),
            5,
            DupPolicy::Skip,
        )
        .await
        .expect("person 5 个应成功");
        let mut v_ok = 0usize;
        for i in 1..=5i64 {
            let one = [NewTarget {
                person_id: 0,
                name: String::new(),
                value: i * 1_000,
            }];
            if targets_add_serialized_pool(
                &pool,
                &name,
                "male",
                "value",
                &one,
                5,
                DupPolicy::Ignore,
            )
            .await
            .is_ok()
            {
                v_ok += 1;
            }
        }
        let d_person = count_kind(&pool, &name, "person").await;
        let d_value = count_kind(&pool, &name, "value").await;
        cleanup(&pool, &name).await;
        let d_left =
            count_kind(&pool, &name, "person").await + count_kind(&pool, &name, "value").await;
        println!(
            "[D] person 满 5 后加 value：value 成功×{v_ok}；person={d_person} value={d_value}；清理后残留={d_left}"
        );
        assert_eq!((d_person, d_value), (5, 5), "person / value 各自独立算上限");
        assert_eq!(d_left, 0, "临时数据必须删净");

        println!("[done] 真库并发回归全部通过，临时账号 {name} 已清空");
    }
}

/* --------------- 扫码绑定的业务冲突（2026-10-16）：必须带类型，路由层才能给 400 人话 --------------- */

#[cfg(test)]
mod wx_bind_conflict_tests {
    use super::{bind_conflict, wx_push_clash_of, WxBindConflict};

    fn others() -> Vec<(String, u64, String)> {
        vec![
            ("shuai4444".to_string(), 0, "o9cq808v597x".to_string()),
            ("狼征".to_string(), 1, "o9cq800cQBnO".to_string()),
            // 同一主播（person_id=1）换过账号名 / 换过微信后留下的旧行
            ("狼征-旧号".to_string(), 1, "o9cqOldWechat".to_string()),
        ]
    }

    /// 同一台微信（uid 相同）绑到**别的**账号名下：必须拒，且是带类型的业务冲突 ——
    /// 不带类型就会走 anyhow 兜底 = 500「服务器开小差了，请稍后重试」，用户看不到
    /// 「请先解绑旧的再扫」（2026-10-16 用户报障的根因就是这个）。
    #[test]
    fn other_account_same_wechat_is_typed_conflict() {
        let msg = wx_push_clash_of("帅帅", 0, "o9cq808v597x", &others()).expect("应判为冲突");
        assert!(msg.contains("shuai4444"), "文案要点名占用的账号：{msg}");
        let e = bind_conflict(msg);
        let c = e
            .downcast_ref::<WxBindConflict>()
            .expect("必须是 WxBindConflict，否则路由层降级成 500 通用文案");
        assert!(c.0.contains("请先解绑旧的再扫"), "{}", c.0);
    }

    /// 自己更新自己（同名重扫）不算冲突：supervisor 重连 / token 刷新会反复 save。
    #[test]
    fn self_update_is_not_conflict() {
        assert!(wx_push_clash_of("shuai4444", 0, "o9cq808v597x", &others()).is_none());
    }

    /// 同一主播已有微信账号（person_id 命中）同样拒，文案指向主播而不是微信。
    #[test]
    fn same_person_second_wechat_is_conflict() {
        let msg = wx_push_clash_of("狼征", 1, "o9cqFresh", &others()).expect("应判为冲突");
        assert!(msg.contains("该主播已经有"), "{msg}");
    }

    /// 空 uid / person_id=0 是「未激活 / 管理员观看账号」哨兵，不参与判重。
    #[test]
    fn sentinel_empties_do_not_clash() {
        assert!(wx_push_clash_of("帅帅", 0, "", &others()).is_none());
        assert!(wx_push_clash_of("帅帅", 0, "o9cqFresh", &[]).is_none());
    }
}
