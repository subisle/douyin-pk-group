//! store::wxpush —— 自 store.rs 按业务域拆分（对外语义不变）。

use super::*; // 共享 w() / now_str() / AppState 等内部工具

/* --------------------- 展示端「我的」页微信推送（绑定 + 追分目标） --------------------- */
/*
   2026-10-09 新增。链路：
     展示端「我的」页生成绑定码 → 用户扫码加微信机器人 → 把绑定码发给机器人
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
}

/// 展示端「我的」页看到的推送状态（不含会话 id 等内部字段）
pub async fn push_state(st: &AppState, name: &str) -> serde_json::Value {
    let Some(pool) = w(st).ok() else {
        return serde_json::json!({ "bound": false, "code": "", "codeExpires": "" });
    };
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT conversation_id, nickname, bind_code, code_expires, bound_at,
                notify_target, daily_report, last_push, last_error
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
        });
    };
    let conv: String = r
        .try_get::<String, _>("conversation_id")
        .unwrap_or_default();
    serde_json::json!({
        "bound": !conv.is_empty(),
        "nickname": r.try_get::<String, _>("nickname").unwrap_or_default(),
        "code": r.try_get::<String, _>("bind_code").unwrap_or_default(),
        "codeExpires": r.try_get::<String, _>("code_expires").unwrap_or_default(),
        "boundAt": r.try_get::<String, _>("bound_at").unwrap_or_default(),
        "notifyTarget": r.try_get::<i16, _>("notify_target").unwrap_or(0) == 1,
        "dailyReport": r.try_get::<i16, _>("daily_report").unwrap_or(0) == 1,
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
    if !expires.is_empty() {
        let ok = chrono::NaiveDateTime::parse_from_str(&expires, "%Y-%m-%d %H:%M:%S")
            .map(|t| t > chrono::Local::now().naive_local())
            .unwrap_or(false);
        if !ok {
            return None;
        }
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

/// 按微信会话查绑定（机器人角色判定 + 普通用户命令取姓名 / person_id / 团别）
pub async fn push_binding_by_conversation(
    st: &AppState,
    conversation_id: &str,
) -> Option<PushBinding> {
    let pool = w(st).ok()?;
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT name, team, person_id, conversation_id, notify_target, daily_report
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
/// conversation_id/notify_target/daily_report，两处查询共用）
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
    }
}

/// 某团已绑定且至少开了一个推送开关的账号（推送引擎用）
pub async fn push_bindings_for_team(st: &AppState, team: &str) -> Vec<PushBinding> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    match sqlx::query(&crate::pq::rewrite(
        "SELECT name, team, person_id, conversation_id, notify_target, daily_report
         FROM push_bindings
         WHERE team = ? AND conversation_id <> '' AND (notify_target = 1 OR daily_report = 1)",
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

/* --------------------- 微信绑定：手机号（2026-10-10） --------------------- */
/*
   旧口径是「页面出 6 位码 → 用户把码发给机器人」。改成扫码加好友后**直接发手机号**绑定：
   手机号在 my_accounts 里本来就是一个手机号只绑一个姓名，拿到手机号就能确定账号，
   用户不用再回页面抄码。「我的」页仍保留绑定码入口作为兜底（没登记手机号时用）。

   2026-10-10 审计 P1-1：只发手机号就能绑定 = 知道手机号即可冒绑 + 枚举登记状态。
   机器人侧已改成二要素（手机号 + 6 位绑定码），本文件提供 push_bind_by_phone_code：
   my_accounts 查手机号 → push_bindings 校验绑定码（一次性 + 30 分钟有效）→ 绑定。
*/

/// 二要素绑定（P1-1）：手机号（my_accounts 里登记过的）+ 6 位一次性绑定码
/// （「我的」页 push_set_code 生成的，30 分钟有效）。两条都命中才绑，码用掉即清。
/// 任一环节不匹配返回 None，调用方统一文案、不区分具体原因。
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
/// 【2026-10-10 审计 P1-1 后线上路径不再用它】单凭手机号即可绑定 = 冒绑/枚举漏洞，
/// 机器人改走 push_bind_by_phone_code（手机号 + 绑定码二要素）。
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
        return Err(anyhow::anyhow!("{msg}"));
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
            if m.contains("uk_wx_push_user_uniq") {
                anyhow::anyhow!("该微信已经绑定过别的账号，一个微信只能绑一个主播")
            } else if m.contains("uk_wx_push_person_uniq") {
                anyhow::anyhow!("该主播已经有一个微信推送账号，要换微信请先解绑旧的")
            } else {
                anyhow::anyhow!("该微信推送账号已存在，请刷新后重试")
            }
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

/// 启动时恢复全部扫码账号（各自跑一条长轮询）
pub async fn wx_push_accounts_all(st: &AppState) -> Vec<WxPushAccount> {
    let Ok(pool) = w(st) else {
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
    let team = normalize_push_team(team);
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
    let _ = sqlx::query(&crate::pq::rewrite(
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
    .await;
}
