//! public_teams::wx —— 自 public_teams.rs 按业务域拆分（对外语义不变）。

use super::*;
use crate::store::{DupPolicy, NewTarget, TargetAddError};

/* ------------------ 我的页面 · 微信推送（绑定 + 追分目标 + 每日报告） ------------------ */

/// 登录态 → (personId, 团别, 账号姓名)。推送相关接口全部走这一处校验。
pub(super) async fn my_ident(
    st: &ArcState,
    team: &str,
    token: &str,
) -> Result<(u64, String, String), ApiError> {
    let token = token.trim();
    let ident = match my_token_take(token) {
        Some(ident) => Some(ident),
        None => super::admin::my_token_restore(st, token).await,
    };
    let Some((pid, my_team, name)) = ident else {
        return Err(ApiError::auth("登录已过期，请重新登录"));
    };
    if !super::my_team_allows(&my_team, &team) {
        return Err(ApiError::auth("该账号不属于本团"));
    }
    Ok((pid, my_team, name))
}

/// `target_counts` → 契约里统一的 `"counts":{"person":n,"value":n,"rank":n}`
/// （`GET /my/push` 与批量添加的响应共用，避免两处口径漂移）。
fn counts_json(counts: (i64, i64, i64)) -> Value {
    json!({ "person": counts.0, "value": counts.1, "rank": counts.2 })
}

/// 上限 400 文案里的量词（按 kind 分类计数，三种 kind 各一句）。
fn unit_of(kind: &str) -> &'static str {
    match kind {
        "value" => "个目标分数",
        "rank" => "个名次",
        _ => "个人",
    }
}

/// 同 kind 重复时的 400 文案（`DupPolicy::Reject` 用；value 不判重所以不需要）。
fn dup_msg_of(kind: &str) -> &'static str {
    match kind {
        "rank" => "已经在追这个名次了",
        _ => "已经在追这个主播了",
    }
}

/// 写库失败日志里的人话业务名。
fn what_of(kind: &str) -> &'static str {
    match kind {
        "value" => "自定义分",
        "rank" => "名次",
        _ => "追人",
    }
}

/// `valueWan`（单位万）→ 原始音浪。
///
/// FIX3（2026-10-15 qa）：原来只校验 `> 0`，`valueWan=1e300` 会 200 且
/// `target_value` 饱和成 `i64::MAX`。这里补上与 person 分支 `targetWan` 同量级的
/// 1e8「万」上限（= 1e12 音浪，远超真实量级），合法区间行为不变。
fn value_wan_to_raw(body: &Value) -> Result<i64, ApiError> {
    let wan = body
        .get("valueWan")
        .and_then(|v| v.as_f64())
        .filter(|v| *v > 0.0)
        .ok_or_else(|| ApiError::bad_request("请输入本月目标（万），要大于 0"))?;
    // `!(x <= 上限)` 写法同时挡住 inf（JSON 里 1e400 会解析成 inf）
    if !(wan <= 100_000_000.0) {
        return Err(ApiError::bad_request("目标分太大了，检查一下单位（万）"));
    }
    Ok((wan * 10_000.0).round() as i64)
}

/// 追分目标写入的唯一出口：调 [`crate::store::targets_add_serialized`]
/// （FIX1：上限校验 + 判重 + 插入在同一事务里按账号串行化），
/// 并把结构化错误映射成既有口径的 400 / 500 文案。
async fn add_targets(
    st: &ArcState,
    name: &str,
    team_scope: &str,
    kind: &str,
    items: &[NewTarget],
    dup: DupPolicy,
) -> Result<usize, ApiError> {
    crate::store::targets_add_serialized(
        st,
        name,
        team_scope,
        kind,
        items,
        crate::push::MAX_TARGETS,
        dup,
    )
    .await
    .map_err(|e| match e {
        TargetAddError::OverLimit => ApiError::bad_request(format!(
            "最多只能追 {} {}，请先删掉一个",
            crate::push::MAX_TARGETS,
            unit_of(kind)
        )),
        TargetAddError::Duplicate => ApiError::bad_request(dup_msg_of(kind)),
        TargetAddError::Db(err) => {
            // 底层写库失败：原文留日志，客户端给通用提示（不再靠「失败」关键词兜）
            tracing::error!(error = %err, "保存追分目标（{}）失败", what_of(kind));
            ApiError::from(anyhow::anyhow!("保存失败：{err}"))
        }
    })
}

/// GET /api/public/teams/{team}/my/push（token 走 X-My-Token 头）：推送设置全量状态
/// （绑定状态 + 两个开关 + 追分目标 + 是否已配机器人二维码）
pub async fn my_push(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(_q): Query<MyQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let token = header_my_token(&headers).unwrap_or_default();
    let (person_id, _my_team, name) = my_ident(&st, &team, &token).await?;
    let mut state = crate::store::push_state(&st, &name).await;
    let targets = crate::store::targets_list(&st, &name).await;
    let counts = crate::store::target_counts(&st, &name).await;
    let has_qr = crate::store::config_get(&st, "wechat_add_qr")
        .await
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if let Some(obj) = state.as_object_mut() {
        obj.insert("targets".into(), json!(targets));
        obj.insert("maxTargets".into(), json!(crate::push::MAX_TARGETS));
        // 契约 B 节（2026-10-15）：上限按 kind 分类 —— 追人 / 追分数各自独立计数，
        // 前端据此判断哪一类已满（maxTargets 仍复用为**单类**上限，既有字段不变）。
        obj.insert("counts".into(), counts_json(counts));
        obj.insert("hasQr".into(), json!(has_qr));
        obj.insert("personId".into(), json!(person_id));
        // 管理员观看账号（personId=0）没有自己的音浪，追分对比没有意义
        obj.insert("admin".into(), json!(person_id == 0));
        obj.insert("channelConfigured".into(), json!(true));
        // 用户自助扫码（2026-10-11）：凭证已保存 = 可推送；等待首次消息才有会话上下文
        let wxacct = crate::store::wx_push_account_get(&st, &name).await;
        obj.insert("scanBound".into(), json!(wxacct.is_some()));
        /* 「待激活」= **还缺会话语境（context_token）**，不是「wx_push_accounts 里没会话」。
        2026-10-16 修正：原来拿 `wx_push_accounts.conversation_id` 判，而那列在
        push_bindings 写失败时照样是满的 → 页面既不显示「未绑定」（bound=false）
        也不显示「还差最后一步」（needActivate=false），用户完全不知道该做什么。
        现在直接问推送真正要用的那张表（wx_sessions 的 context_token）：
        · 扫了码、会话已记 → 看 wx_sessions 有没有 context_token；
        · 扫了码、还没会话 → 待激活；
        · 没扫码 → false（走「未绑定」分支）。 */
        let need_activate = match wxacct.as_ref() {
            None => false,
            Some(a) if a.conversation_id.is_empty() => true,
            Some(a) => crate::store::wx_session_get(&st, &a.conversation_id)
                .await
                .map(|(_, ctx)| ctx.is_empty())
                .unwrap_or(true),
        };
        obj.insert("needActivate".into(), json!(need_activate));
        obj.insert(
            "scanNickname".into(),
            json!(wxacct
                .as_ref()
                .map(|a| a.nickname.clone())
                .unwrap_or_default()),
        );
        // 手机号绑定（2026-10-10）：登记过手机号就走「发手机号」路径；
        // 只回显脱敏手机号（138****8000），不全量下发，避免页面被别人瞄到就能拿走号。
        let phone = crate::store::my_account_phone(&st, &name).await;
        obj.insert("hasPhone".into(), json!(!phone.is_empty()));
        obj.insert("phoneMasked".into(), json!(mask_phone(&phone)));
        // 师门日报（docs/23 §4.3）：只有「名下有直系徒弟的人」才适用第三开关。
        // 口径：person_id > 0 且存在未删除的直系徒弟（c.master_id = person_id）。
        // 折成一条 COUNT 查询（结果恒 0/1），走 pgrs 只读池；档案不存在 → 0。
        //
        // 2026-10-17 修复（审计 1-P2-4）：原先额外要求「本人 master_id IS NULL」
        // （只认开山师傅），而**推送侧**（lineage_report::build 按结果行数兜底）与
        // 「一键为所有师傅开启」（store::admin_messages，只看有没有徒弟）都只要求
        // **名下有徒弟**。结果：中间层师傅（自己也有师傅、但名下有徒弟）会被一键
        // 开启并收到师门日报，页面上却**不显示这个开关** —— 他自己关不掉。
        // 现在按原注释预留的方向放宽，与推送侧对齐。
        let has_disciples = if person_id > 0 {
            // 2026-10-17 审计 6-P3-9：徒弟过滤与推送侧 build() 同口径
            //（在职 + 未隐藏），否则只有离团/隐藏徒弟的师傅看得到开关、
            // 却永远收不到日报（rows.len()==1 → 跳过），开关成了摆设。
            let sql = "SELECT COUNT(*) AS n FROM person p \
              WHERE p.id = ? AND p.deleted_at IS NULL \
                AND EXISTS (SELECT 1 FROM person c \
                     WHERE c.master_id = p.id AND c.deleted_at IS NULL \
                       AND c.status <> 'left' AND c.hide_in_daily_report = 0)";
            let res =
                pgrs::query_with_binds(&st.pools, &sid(&st), sql, |q| q.bind(person_id as i64))
                    .await;
            match res {
                // COUNT(*) 恒返回一行一列；取不到列（异常列型）按 false 兜底
                Ok(rows) => rows
                    .first()
                    .and_then(|r| r.try_get::<i64, _>("n").ok())
                    .map(|n| n > 0)
                    .unwrap_or(false),
                // 查询失败（库抖动 / 只读守卫拒绝）：false = 前端不显示第三开关，
                // 行为退化到没有该字段的旧版，推送侧仍按真实名单兜底
                Err(e) => {
                    tracing::warn!(person_id, error = %e, "查师门归属失败，hasDisciples 按 false 兜底");
                    false
                }
            }
        } else {
            // 管理员观看账号（person_id == 0）没有主播档案，恒 false
            false
        };
        obj.insert("hasDisciples".into(), json!(has_disciples));
    }
    Ok(Json(state))
}

/// 手机号脱敏：138****8000（中间四位打码）。空串原样返回。
pub(super) fn mask_phone(p: &str) -> String {
    let cs: Vec<char> = p.chars().collect();
    match cs.len() {
        n if n >= 11 => format!(
            "{}****{}",
            cs[..3].iter().collect::<String>(),
            cs[n - 4..].iter().collect::<String>()
        ),
        n if n >= 7 => format!(
            "{}****{}",
            cs[..2].iter().collect::<String>(),
            cs[n - 2..].iter().collect::<String>()
        ),
        n if n >= 3 => format!("{}****{}", cs[0], cs[n - 1]),
        _ => String::new(),
    }
}

/// 邮箱脱敏：a***@b.com（本地部分只留首字符）。空串原样返回。日志用，不落全量邮箱。
///
/// 2026-10-18：展示端邮箱统一改存 QQ 邮箱后，日志/响应侧改用
/// `store::mask_qq_email`（QQ 域保留前 3 位）。本函数保留 —— 它是 038 存量
/// 非 QQ 邮箱（163 等）的原始打码口径，`mask_qq_email` 对非 QQ 域正是复刻这套
/// 规则；留着作为口径出处与后续日志打码的备用实现。
#[allow(dead_code)]
pub(super) fn mask_email(e: &str) -> String {
    if let Some((local, domain)) = e.split_once('@') {
        let mut out = String::new();
        if let Some(c) = local.chars().next() {
            out.push(c);
        }
        out.push_str("***@");
        out.push_str(domain);
        out
    } else {
        // 没有 @ 的异常值：整段打码，避免日志漏出任何可读内容
        if e.is_empty() {
            String::new()
        } else {
            "***".to_string()
        }
    }
}

/* ------------------ 用户自助扫码绑定微信推送（2026-10-11，多账号） ------------------
 *
 * 官方协议：POST get_bot_qrcode 取账号登录二维码；用户扫码确认后
 * get_qrcode_status 返回 bot_token / ilink_bot_id / ilink_user_id / baseurl。
 * 我们把二维码会话与当前「我的」登录账号绑定（qrcode → name），确认后
 * 落 wx_push_accounts 并拉起该账号的 getupdates 长轮询；用户首次给机器人
 * 发消息时留下 context_token，随后推送走该账号自己的 token。 */

#[derive(Clone)]
pub(super) struct PendingWxLogin {
    name: String,
    person_id: u64,
    team: String,
    poll_base: String,
    img_content: String,
    url: String,
    created: Instant,
}

pub(super) type PendingWxLoginMap = HashMap<String, PendingWxLogin>;

pub(super) fn pending_wx_logins() -> &'static Mutex<PendingWxLoginMap> {
    static M: OnceLock<Mutex<PendingWxLoginMap>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn pending_wx_put(
    qrcode: &str,
    name: &str,
    person_id: u64,
    team: &str,
    img_content: &str,
    url: &str,
) {
    let mut m = pending_wx_logins()
        .lock()
        .expect("pending wx login 锁 poisoned");
    m.retain(|_, login| login.created.elapsed() < Duration::from_secs(5 * 60));
    m.retain(|_, login| login.name != name);
    m.insert(
        qrcode.to_string(),
        PendingWxLogin {
            name: name.to_string(),
            person_id,
            team: team.to_string(),
            poll_base: String::new(),
            img_content: img_content.to_string(),
            url: url.to_string(),
            created: Instant::now(),
        },
    );
}

/// 同一账号还有未过期的二维码会话时，直接复用它，避免反复向上游要新码。
pub(super) fn pending_wx_cached(name: &str) -> Option<(String, String, String)> {
    let mut m = pending_wx_logins()
        .lock()
        .expect("pending wx login 锁 poisoned");
    m.retain(|_, login| login.created.elapsed() < Duration::from_secs(5 * 60));
    m.iter()
        .find(|(_, login)| login.name == name)
        .map(|(qrcode, login)| (qrcode.clone(), login.img_content.clone(), login.url.clone()))
}

/// 读一次未过期的扫码会话（保留在 map 里，轮询可重复调用）；
/// 返回 (person_id, team, 轮询用的 base_url)。
pub(super) fn pending_wx_take(qrcode: &str, name: &str) -> Option<(u64, String, String)> {
    let mut m = pending_wx_logins()
        .lock()
        .expect("pending wx login 锁 poisoned");
    m.retain(|_, login| login.created.elapsed() < Duration::from_secs(5 * 60));
    match m.get(qrcode) {
        Some(login) if login.name == name => {
            Some((login.person_id, login.team.clone(), login.poll_base.clone()))
        }
        _ => None,
    }
}

/// `scaned_but_redirect`：记下服务端给出的官方主机，下一轮换过去继续轮询。
pub(super) fn pending_wx_set_base(qrcode: &str, base_url: &str) {
    if base_url.trim().is_empty() {
        return;
    }
    let mut m = pending_wx_logins()
        .lock()
        .expect("pending wx login 锁 poisoned");
    if let Some(login) = m.get_mut(qrcode) {
        login.poll_base = base_url.trim().to_string();
    }
}

/// 二维码终态（上游失效 / 已确认保存成功）后必须移除，防止继续轮询旧会话。
pub(super) fn pending_wx_remove(qrcode: &str) {
    pending_wx_logins()
        .lock()
        .expect("pending wx login 锁 poisoned")
        .remove(qrcode);
}

/// POST /api/public/teams/{team}/my/push/wechat/start {token}
/// 取官方协议（openclaw-weixin / iLink）的用户扫码对接二维码（默认国内 API Base）。
///
/// 2026-10-16 深夜用户口径：展示端「微信对接」段只留一个按钮「获取二维码」——
/// 点一下**直接**给二维码与 4 位对接码（对接码走并行的 `POST /my/push/code`，
/// 两个接口各自独立，少一个也不挡住另一个）。所以这里不改响应形状，只保证
/// `imgContent`（data:image/svg+xml;base64,… 的二维码图）拿到就能直接 <img> 渲染。
pub async fn my_push_wechat_start(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (person_id, _actual_team, name) = my_ident(&st, &team, token).await?;
    if person_id == 0 && !crate::store::my_account_is_admin(&st, &name).await {
        // 登录账号缺主播档案（管理员观看账号）：请求态不能做这个操作 → 显式 400
        return Err(ApiError::bad_request(
            "该账号没有主播档案，不能绑定微信推送",
        ));
    }
    // 管理员观看账号（personId=0）也允许扫码绑定；团别留空，绑定后可看「我的」页推送入口。
    let team_scope = crate::store::person_team_of_name(&st, &name)
        .await
        .unwrap_or_default();
    let (qrcode, img_content, url) = if let Some(cached) = pending_wx_cached(&name) {
        cached
    } else {
        let out = crate::bot::weixin::user_login_qrcode().await?;
        let qrcode = out
            .get("qrcode")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if qrcode.is_empty() {
            // 上游微信接口没吐二维码（上游故障或参数被拒）：给用户引导话术 400
            return Err(ApiError::bad_request("微信接口没有返回二维码，请稍后再试"));
        }
        let img_content = out
            .get("imgContent")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if img_content.is_empty() {
            return Err(ApiError::bad_request(
                "微信接口没有返回可用二维码，请稍后再试",
            ));
        }
        let url = out
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        pending_wx_put(&qrcode, &name, person_id, &team_scope, &img_content, &url);
        (qrcode, img_content, url)
    };
    let (code, code_expires) = if person_id == 0 {
        (String::new(), String::new())
    } else {
        crate::store::push_code_current_or_rotate(&st, &name, &team_scope, person_id)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "生成对接码失败");
                anyhow::anyhow!("生成对接码失败：{e}")
            })?
    };
    Ok(Json(json!({
        "ok": true,
        "qrcode": qrcode,
        "imgContent": img_content,
        "url": url,
        "code": code,
        "codeExpires": code_expires,
    })))
}

/// 保存扫码凭证失败的出口（2026-10-16 用户报障：扫码一直提示「服务器开小差了，请稍后重试」）。
///
/// 问题：微信已绑别处 / 该主播已有微信账号这类**业务拒绝**也是 `anyhow` 抛出来的，
/// `From<anyhow::Error>` = `safe=false` → 500 + 通用文案，真实原因（「该微信已经绑定在
/// 「xxx」的账号上；请先解绑旧的再扫」）只进了服务端日志；展示端拿到 500 又当网络抖动，
/// 每 3 秒重试 10 次 → 用户看到的就是「重试中…」空转，且永远不知道该做什么。
///
/// 现在：业务冲突带 [`crate::store::WxBindConflict`] 类型 → 400 原样下发（这是可执行的人话）；
/// 其它错误（PG / 写池 / 网络）保持 anyhow 兜底 = 500 + 通用文案，底层细节只留日志不出去。
fn wx_save_failure(e: anyhow::Error) -> ApiError {
    if let Some(c) = e.downcast_ref::<crate::store::WxBindConflict>() {
        return ApiError::bad_request(c.0.clone());
    }
    ApiError::from(anyhow::anyhow!("保存微信凭证失败：{e}"))
}

/// POST /api/public/teams/{team}/my/push/wechat/poll {token, qrcode, verifyCode?}
/// 轮询扫码状态；confirmed 后保存账号凭证并拉起长轮询
pub async fn my_push_wechat_poll(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (person_id, _actual_team, name) = my_ident(&st, &team, token).await?;
    let qrcode = body
        .get("qrcode")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if qrcode.is_empty() {
        return Err(ApiError::bad_request("缺少 qrcode"));
    }
    let verify = body
        .get("verifyCode")
        .or_else(|| body.get("verify_code"))
        .and_then(|v| v.as_str());
    let Some((_pid, team_scope, poll_base)) = pending_wx_take(&qrcode, &name) else {
        return Err(ApiError::bad_request("二维码已过期，请重新获取"));
    };
    let base = if poll_base.is_empty() {
        "https://ilinkai.weixin.qq.com"
    } else {
        poll_base.as_str()
    };
    let out = crate::bot::weixin::user_login_status_at(base, &qrcode, verify).await?;
    if let Some(host) = out.get("redirectHost").and_then(Value::as_str) {
        if !host.is_empty() {
            let next = if host.starts_with("http") {
                host.to_string()
            } else {
                format!("https://{host}")
            };
            pending_wx_set_base(&qrcode, &next);
        }
    }
    let status = out
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("wait")
        .to_string();
    if status != "confirmed" {
        if matches!(status.as_str(), "expired" | "verify_code_blocked") {
            pending_wx_remove(&qrcode);
        }
        return Ok(Json(json!({ "status": status, "tokenSaved": false })));
    }
    let bot_token = out
        .get("botToken")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let bot_id = out
        .get("botId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let user_id = out
        .get("userId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    /* 2026-10-17 审计 4-P2-2：user_id 缺失**直接拒绝保存**（收窄 2026-10-16 的
    「warn 后继续」口径）。原因：无 user_id 的行落库后 ilink_user_id 与
    conversation_id 双空，激活侧 activation_owner_ok 无法证明发送者身份 ——
    任何能私聊到 bot 的人发第一条消息就能冒认激活，此后该主播的音浪/追分/日报
    全部推给陌生人，还能以本人身份「解绑」。上游偶发不下发 userId 是可重试的
    偶发故障（重扫一次即可），与「绑定白扫」相比宁可严格。 */
    if bot_token.is_empty() || bot_id.is_empty() || user_id.is_empty() {
        if user_id.is_empty() {
            tracing::warn!(account = %name, "扫码已确认但上游未返回 ilink_user_id，拒绝保存（防无主凭证被冒认激活）");
        }
        return Err(ApiError::bad_request(
            "扫码已确认，但微信没有返回完整凭证，请重试",
        ));
    }
    // 重扫不丢已激活绑定（2026-10-13 用户口径：配置还要和之前一样）：
    // 覆盖凭证前先取库里已有账号，下面重申 push_bindings 时带上旧会话。
    let existing = crate::store::wx_push_account_get(&st, &name).await;
    let mut acct = crate::store::WxPushAccount {
        name: name.clone(),
        team: team_scope.clone(),
        person_id,
        ilink_bot_id: bot_id,
        ilink_user_id: user_id.clone(),
        bot_token,
        // baseUrl 可能为空；本次轮询实际是走 pending 会话记录的 redirect base（poll_base）
        // 成功的（poll_base 为空时本就走默认主机），空串回落它，
        // 不然 user_send_text 退默认主机不保证对。
        base_url: {
            let from_out = out
                .get("baseUrl")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if from_out.is_empty() {
                base.to_string()
            } else {
                from_out.to_string()
            }
        },
        nickname: out
            .get("nickname")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        conversation_id: String::new(),
        updates_buf: String::new(),
        bound_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        // 刚扫码的账号未暂停；若此前解绑过，下面 push_bind_account 的 upsert 会把
        // push_bindings.paused 置回 0（重新对接 = 恢复，迁移 029 口径）。
        paused: false,
        /* 2026-10-17 审计 4-P2-2：新扫码凭证不再打 legacy 标（user_id 缺失已在
        上面直接拒绝保存）。重扫历史 legacy 行且这次上游补发了 userId 时，
        标记随之清除 —— activation_owner_ok 从此走严格比对。 */
        legacy_no_uid: match &existing {
            Some(e) => e.legacy_no_uid && user_id.is_empty(),
            None => false,
        },
    };
    // save 的 DO UPDATE 不覆盖 conversation_id；已有账号时把内存结构与库对齐
    if let Some(e) = &existing {
        acct.conversation_id = e.conversation_id.clone();
    }
    // 换微信号重扫（2026-10-13 补）：ilink_user_id / bot_id 变了 = 绑定目标换人了，
    // 旧会话必须作废（account 与 push_bindings 的 conv 一起清空，updates_buf 游标同清），
    // 让用户给新 bot 发第一条消息时按「首次激活」重走一遍；
    // 开关（notify_target/daily_report）与追分目标（my_targets）都不动——配置还要和之前一样。
    // 同一个微信重扫（uid 相同）不走这里，会话原样保留。
    // 注意 save 的 DO UPDATE 不覆盖 conversation_id / updates_buf，
    // 所以换微信时要把旧行删掉重插，清空才真正落库。
    let rescan_new_wechat = existing
        .as_ref()
        .map(|e| !e.ilink_user_id.is_empty() && e.ilink_user_id != acct.ilink_user_id)
        .unwrap_or(false);
    if rescan_new_wechat {
        acct.conversation_id.clear();
        crate::store::wx_push_account_delete(&st, &name).await;
    }
    if let Err(e) = crate::store::wx_push_account_save(&st, &acct).await {
        tracing::error!(error = %e, "保存微信凭证失败");
        return Err(wx_save_failure(e));
    }
    if rescan_new_wechat {
        if let Some(e) = &existing {
            let old_conv = format!("wxacct:{}:{}", e.ilink_bot_id, e.ilink_user_id);
            crate::store::wx_session_delete(&st, &old_conv).await;
        }
    }
    // 先建 push_bindings 行（未通话时 bound=false，但两个推送开关可先设置）。
    // 重扫不丢已激活绑定（2026-10-13 用户口径：配置还要和之前一样）：
    // - 同一个微信重扫：库里已有激活会话就原样重申（conv / to_user_id 不变）；
    // - 换了微信号重扫：会话作废，落空串（等新微信发消息重新激活）；
    // - 首次扫码：传空串建未激活行，与原行为一致。
    // push_bind_account 自带的「同一会话抢占清理」语义不变。
    let (bind_conv, bind_uid) = match existing.as_ref() {
        Some(e) if !e.conversation_id.is_empty() && !rescan_new_wechat => {
            (e.conversation_id.as_str(), e.ilink_user_id.as_str())
        }
        _ => ("", ""),
    };
    crate::store::push_bind_account(&st, &name, &team_scope, bind_conv, bind_uid).await;
    crate::wxpush::ensure_account(&st, &name).await;
    tracing::info!(account = %name, bot = %acct.ilink_bot_id, "用户扫码登录微信推送账号成功，等待首次消息激活");

    /* 对接完成欢迎语（2026-10-16 深夜用户口径：**对接完成发送欢迎语加名字**）。
    这一刻刚拿到该账号的 bot_token，iLink 的私聊是「会话上下文」形态 —— 用户还没给
    机器人发过消息，所以没有 context_token；官方协议允许缺省尝试（`send_raw` 同口径），
    发不出去也不影响对接本身（真正的兜底是 wxpush 的「首次入站激活回执」，同样带名字）。
    出网闸门在 `user_send_text` 里：测试阶段（PUSH_OUT_ENABLED 未开）只记 WARN、不真发，
    返回值仍是 Ok —— 所以 welcome 只是「试过了」，不代表对方一定收到。 */
    let mut welcomed = false;
    if !acct.ilink_user_id.is_empty() {
        let text = crate::bot::wx_dock_welcome(&name);
        match crate::bot::weixin::user_send_text(
            &st,
            &acct.base_url,
            &acct.bot_token,
            &acct.ilink_user_id,
            "",
            &text,
        )
        .await
        {
            Ok(()) => {
                welcomed = true;
                tracing::info!(account = %name, "对接完成欢迎语已发送（含姓名）");
            }
            Err(e) if crate::push::is_blocked(&e) => {
                // 2026-10-17 审计 6-P2-1：闸门拦截不算发送失败 —— 只是不发，
                // 不落「等对方发第一条消息再回执」的误导日志。
                tracing::info!(account = %name, "对接完成欢迎语被出网闸门拦截（未发）");
            }
            Err(e) => tracing::warn!(
                account = %name,
                error = %e,
                "对接完成欢迎语发送失败：等对方给机器人发第一条消息时再回执（wxpush 激活回执）"
            ),
        }
    }
    let response = json!({
        "status": "confirmed",
        "tokenSaved": true,
        "needActivate": true,
        "welcome": welcomed,
    });
    pending_wx_remove(&qrcode);
    Ok(Json(response))
}

/// POST /api/public/teams/{team}/my/push/code {token}：生成/刷新 **4 位数字**对接码
/// （2026-10-16 用户口径：主播把 4 位数字发给机器人即可对接，不再要手机号；30 分钟有效）。
/// 展示端「获取二维码」展开时与二维码并行取一次（页面文案统一叫「对接码」，表列仍是 bind_code）。
pub async fn my_push_code(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    headers: HeaderMap,
    axum::Json(_body): axum::Json<Value>,
) -> ApiResult {
    /* 2026-10-17 审计 4-N15：token 收口到 X-My-Token 头（与 targets_batch 同口径），
    24h 登录态不再过 POST body。前端 postPush 本就两头都带，无破坏。 */
    let token = header_my_token(&headers).unwrap_or_default();
    let (person_id, _t, name) = my_ident(&st, &team, &token).await?;
    if person_id == 0 {
        return Err(ApiError::bad_request("该账号没有主播档案，不能生成对接码"));
    }
    // 4 位数字只有 10^4 种取值：撞号由 store 侧重摇（库里还有部分唯一索引兜底），
    // 这里不再自己 new_bind_code 后直接写。
    let (code, code_expires) =
        crate::store::push_code_current_or_rotate(&st, &name, &team, person_id)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "生成对接码失败");
                anyhow::anyhow!("生成对接码失败：{e}")
            })?;
    Ok(Json(json!({
        "ok": true,
        "code": code,
        "codeExpires": code_expires,
    })))
}

/// POST /api/public/teams/{team}/my/push/settings {token, notifyTarget, dailyReport[, lineageReport]}
///
/// 三个开关的缺省语义**刻意不一致**（保持既有行为，只增量加第三开关）：
/// - `notifyTarget` / `dailyReport`：body 里没带时按 `false` 落库（历史语义——
///   前端每次把两个开关的当前值全量带上，见 st_page.html saveFlags 的注释），
///   继续走 [`crate::store::push_set_flags`] 整行覆盖那两列；
/// - `lineageReport`（docs/23 §4.3）：**缺省 = 不改动现值**。因为旧调用方
///   （只带 token / 只带两个旧开关）不知道这个字段，若跟着 flag() 缺省 false
///   落库，任何旧客户端一保存就会把师门日报静默关掉。字段出现时才走
///   [`crate::store::push_set_lineage_report`] 单独写这一列。
pub async fn my_push_settings(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = header_my_token(&headers).unwrap_or_default();
    let (_pid, _t, name) = my_ident(&st, &team, &token).await?;
    let flag = |k: &str| body.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    crate::store::push_set_flags(&st, &name, flag("notifyTarget"), flag("dailyReport")).await;
    // 师门日报第三开关：body 里带了才改（含显式 false），缺省不动现值。
    // 开关本身允许任意人切换（build() 侧「无徒弟不推空图」兜底），不在入口拦。
    if let Some(v) = body.get("lineageReport").and_then(|v| v.as_bool()) {
        crate::store::push_set_lineage_report(&st, &name, v).await;
    }
    Ok(Json(json!({ "ok": true })))
}

/// POST /api/public/teams/{team}/my/push/unbind {token}
///
/// 2026-10-17 用户口径：页面「解除绑定」只断开推送绑定（push_bindings 的会话接线），
/// **不注销扫码登录状态** —— `wx_push_accounts` 的凭证（bot_token / ilink_user_id）
/// 原样保留。长轮询立即停止（push_unbind 置 paused=1：supervisor 不再补拉，
/// 正在跑的 poll_body 最多一个轮询周期内退出，不再收发任何数据）；之后重新对接
/// 不用再扫码，把新的 4 位对接码发给机器人即可（重新对接 = paused 置回 0 恢复，
/// 本人消息会按「push_bindings 未接上该会话 → 重跑激活」的幂等口径自动回接）。
/// 与机器人里回复「解绑」（push_unbind_by_conversation）的行为对齐 —— 那条路同样
/// 置 paused=1。唯一还删凭证的地方：换了微信号重扫（wx.rs rescan_new_wechat）。
pub async fn my_push_unbind(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    headers: HeaderMap,
    axum::Json(_body): axum::Json<Value>,
) -> ApiResult {
    let token = header_my_token(&headers).unwrap_or_default();
    let (_pid, _t, name) = my_ident(&st, &team, &token).await?;
    crate::store::push_unbind(&st, &name).await;
    Ok(Json(json!({ "ok": true })))
}

/// POST /api/public/teams/{team}/my/push/targets {token, kind, personId?|valueWan?}
/// kind = person（追人）/ value（追数值，前端填万，后端换算成音浪）
pub async fn my_push_target_add(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = header_my_token(&headers).unwrap_or_default();
    let (person_id, actual_team, name) = my_ident(&st, &team, &token).await?;
    let team_scope = if team == "all" {
        actual_team.as_str()
    } else {
        team.as_str()
    };
    if person_id == 0 {
        return Err(ApiError::bad_request(
            "管理员观看账号没有音浪数据，不能设置追分目标",
        ));
    }
    let kind = body
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("person");
    /* 上限按 kind 分类计数（契约 B 节，用户口径「分数是分数，追人是追人」）：
    person / value / rank 各自独立比单类上限（都复用 push::MAX_TARGETS），互不挤占。
    rank 前端已无新增入口，这里仍按单类上限兜底（不改它旧有的 ≤5 有效行为）。

    FIX1（2026-10-15 qa 并发穿透）：这个上限校验 + 判重 + 插入**全部下沉到**
    `store::targets_add_serialized`，在同一事务里按账号串行化。原实现在这里先
    `target_count`、再查重、再逐条 INSERT（check-then-insert，且 my_targets 无唯一
    约束），6 路并发单条 kind=value 实测能插 6 行（应 ≤5）、3 路并发同 id 插 2 行。
    所以这里不再自己计数/查重，只把「要写什么」交给那次事务。 */
    match kind {
        "value" => {
            let raw = value_wan_to_raw(&body)?;
            let items = [crate::store::NewTarget {
                person_id: 0,
                name: String::new(),
                value: raw,
            }];
            // value 历史上允许同一个数值目标并存，所以不加判重（DupPolicy::Ignore）
            add_targets(&st, &name, team_scope, "value", &items, DupPolicy::Ignore).await?;
        }
        // 追名次（2026-10-10）：target_value 存目标名次，名字列存展示文案
        "rank" => {
            let rank = body
                .get("targetRank")
                .and_then(|v| v.as_i64())
                .filter(|v| (1..=999).contains(v))
                .ok_or_else(|| ApiError::bad_request("请输入要追的名次（1~999）"))?;
            let items = [crate::store::NewTarget {
                person_id: 0,
                name: format!("第 {rank} 名"),
                value: rank,
            }];
            add_targets(&st, &name, team_scope, "rank", &items, DupPolicy::Reject).await?;
        }
        _ => {
            let tid = body
                .get("personId")
                .and_then(|v| v.as_u64())
                .filter(|v| *v > 0)
                .ok_or_else(|| ApiError::bad_request("请选择要追的主播"))?;
            if tid == person_id {
                return Err(ApiError::bad_request("不能追自己"));
            }
            // 只允许追本团在册主播（搜索接口也只下发本团名单，这里是服务端兜底）
            let anchor = crate::store::team_anchor_search(&st, &sid(&st), team_scope, "", 1000)
                .await
                .into_iter()
                .find(|a| a.get("personId").and_then(|v| v.as_u64()) == Some(tid));
            let Some(anchor) = anchor else {
                return Err(ApiError::bad_request(
                    "没有找到这个主播（只能追本团在册主播）",
                ));
            };
            let tname = anchor
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            /* 自定义目标分（2026-10-12 用户口径「自定义分数，追人的分数」）：
            选填（单位万）。填了存 target_value = 万*10000，推送按这个数对比；
            不填保持旧行为——比他本人本月累计（target_value=0 是「没有自定义分」的哨兵）。
            展示文案写进 target_name（推送 label / 页面列表都读它）。 */
            let custom_raw = match body.get("targetWan").and_then(|v| v.as_f64()) {
                Some(w) => {
                    // ApiResult 的错误是 ApiError（From<anyhow::Error>）；ensure! 的宏展开
                    // 在返回 ApiError 的函数里类型不匹配，改成手写 if + anyhow!(...).into()
                    if w <= 0.0 {
                        return Err(ApiError::bad_request("目标分要大于 0（万）"));
                    }
                    if w > 100_000_000.0 {
                        return Err(ApiError::bad_request("目标分太大了，检查一下单位（万）"));
                    }
                    (w * 10_000.0).round() as i64
                }
                None => 0,
            };
            let display = if custom_raw > 0 {
                format!("{tname} · {}", crate::push::fmt_wan(custom_raw))
            } else {
                tname.clone()
            };
            let items = [crate::store::NewTarget {
                person_id: tid,
                name: display,
                value: custom_raw,
            }];
            // 同一个人不重复追：DupPolicy::Reject 在事务里判重 → 400「已经在追这个主播了」
            add_targets(&st, &name, team_scope, "person", &items, DupPolicy::Reject).await?;
        }
    }
    Ok(Json(
        json!({ "targets": crate::store::targets_list(&st, &name).await }),
    ))
}

/// 单次批量提交的主播数上限。
///
/// FIX2（2026-10-15 qa）：body 上限 4MB（约 55 万个 id），而 push 系路由不吃
/// public_write_allow 限流；旧实现用 `Vec::contains` 去重是 O(n²)，实测 4 万唯一 id
/// 要 4.71s（元素 ×4 → 耗时 ×16），外推 4MB 单请求就是千秒级 CPU。
const MAX_BATCH_IDS: usize = 100;

/// `personIds` 原始数组 → 校验 + 去重后的 id 列表（纯函数，便于单测）。
///
/// 空数组、`0`、负数、非整数一律 `400`（契约 B 节「personId 非法」）；
/// 重复出现的同一个 id 只保留第一次出现的位置 —— 这就是「同一个 personId 重复出现
/// 计入 skipped」的去重来源（handler 用 `请求条数 - added` 算出 skipped）。
///
/// FIX2：① 长度上限**在最前面**判，超限立刻 400（不能遍历完 4 万条再拒，那正是要修的
/// CPU 放大）；② 去重换 `HashSet`（O(n)）而不是 `Vec::contains`（O(n²)），
/// `HashSet::insert` 返回 false 即重复，`want` 里仍是首次出现的顺序。
fn parse_person_ids(raw: &[Value]) -> Result<Vec<u64>, ApiError> {
    if raw.is_empty() {
        return Err(ApiError::bad_request("请先选择要追的主播"));
    }
    if raw.len() > MAX_BATCH_IDS {
        return Err(ApiError::bad_request(format!(
            "一次最多提交 {MAX_BATCH_IDS} 个主播"
        )));
    }
    let mut seen: std::collections::HashSet<u64> =
        std::collections::HashSet::with_capacity(raw.len());
    let mut want: Vec<u64> = Vec::with_capacity(raw.len());
    for v in raw {
        let id = v
            .as_u64()
            .filter(|id| *id > 0)
            .ok_or_else(|| ApiError::bad_request("personId 不合法（需要正整数）"))?;
        if seen.insert(id) {
            want.push(id);
        }
    }
    Ok(want)
}

/// POST /api/public/teams/{team}/my/push/targets/batch
/// （契约 B 节 2026-10-15）：追人目标**批量多选**添加。token 走 `X-My-Token` 头，
/// body `{"personIds":[1,2,3]}`。
///
/// 响应：
/// - `200 {"ok":true,"added":n,"skipped":n,"counts":{...},"targets":[...]}`
///   `targets` = 操作后的最新完整列表（前端直接重渲染）；`skipped` = 请求里被跳过的条数
///   （请求内重复 + 已经在追的），用于「选了 5 个、只加上 3 个」时给用户对账。
/// - `400`：空数组 / 超过追人上限 / 含非本团在册主播 / personId 非法（0 或不存在）/
///   追自己 / 一次提交超过 100 个
/// - `401`：登录已过期
///
/// 口径与单条 `my_push_target_add` 完全一致：personId 一律用 `team_anchor_search`
/// 兜底校验「本团在册主播」（防止越权追别团的人）、不能追自己；上限按 kind 分类计数
/// （这里只加 person，所以比的是现有追人条数 + 本次新增条数 ≤ push::MAX_TARGETS）。
///
/// 先全校验、后统一落库：身份/参数/名额校验都在写库**之前**返回 400，不会插一半；
/// 落库走 [`crate::store::targets_add_serialized`] 的**单个事务**（FIX1，
/// `pg_advisory_xact_lock` 按账号串行化），所以并发请求要么整批成功、要么整批回滚，
/// 也不再可能把单类上限撑爆或插出重复行。重试安全——已存在的会被计成 skipped。
pub async fn my_push_targets_batch(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = header_my_token(&headers).unwrap_or_default();
    let (person_id, actual_team, name) = my_ident(&st, &team, &token).await?;
    let team_scope = if team == "all" {
        actual_team.as_str()
    } else {
        team.as_str()
    };
    if person_id == 0 {
        return Err(ApiError::bad_request(
            "管理员观看账号没有音浪数据，不能设置追分目标",
        ));
    }
    let raw_ids = body
        .get("personIds")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ApiError::bad_request("缺少 personIds"))?;
    let want = parse_person_ids(raw_ids)?;
    if want.contains(&person_id) {
        return Err(ApiError::bad_request("不能追自己"));
    }
    // 本团在册主播兜底校验（与单条添加同一处、同一 limit=1000 口径），一次取全团名单。
    let anchors: std::collections::HashMap<u64, String> =
        crate::store::team_anchor_search(&st, &sid(&st), team_scope, "", 1000)
            .await
            .into_iter()
            .filter_map(|a| {
                let id = a.get("personId").and_then(|v| v.as_u64())?;
                let n = a
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                Some((id, n))
            })
            .collect();
    if want.iter().any(|id| !anchors.contains_key(id)) {
        return Err(ApiError::bad_request(
            "没有找到这个主播（只能追本团在册主播）",
        ));
    }
    // 已经在追的 person 撞车 / 上限 / 插入：全部交给那次事务（FIX1）。
    // 这里只把「想加谁」按勾选顺序准备好；`skipped` 仍按契约 = 请求原始条数 − added。
    let items: Vec<NewTarget> = want
        .iter()
        .map(|id| NewTarget {
            person_id: *id,
            name: anchors.get(id).cloned().unwrap_or_default(),
            value: 0,
        })
        .collect();
    let added = add_targets(&st, &name, team_scope, "person", &items, DupPolicy::Skip).await?;
    Ok(Json(json!({
        "ok": true,
        "added": added,
        "skipped": raw_ids.len().saturating_sub(added),
        "counts": counts_json(crate::store::target_counts(&st, &name).await),
        "targets": crate::store::targets_list(&st, &name).await,
    })))
}

/// POST /api/public/teams/{team}/my/push/targets/delete {token, id}
pub async fn my_push_target_del(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = header_my_token(&headers).unwrap_or_default();
    let (_pid, _t, name) = my_ident(&st, &team, &token).await?;
    let id = body
        .get("id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| ApiError::bad_request("缺少目标 id"))?;
    crate::store::target_delete(&st, &name, id as i64).await;
    Ok(Json(
        json!({ "targets": crate::store::targets_list(&st, &name).await }),
    ))
}

/// GET /api/public/teams/{team}/my/push/search?q=（token 走 X-My-Token 头）：
/// 本团主播搜索（回 id + 名字 + 本月累计音浪 `wave`，口径同推送月榜）。
///
/// q 语义（2026-10-17）：空串不再早退返回空数组，而是作为「默认候选」——
/// team_anchor_search 对空 q 就是全团按本月音浪降序（同口径见 store::push_jobs）。
/// limit 分档：q 空 → 50，q 非空 → 30。
///
/// 关于 `personId`（2026-10-14 审计复核结论：**保持下发，不改**）：
/// 审计曾疑此处与已下线的「绑定抖音号」搜索（原 `my_bind_search`，2026-10-16 随
/// 抖音号审计整页下线删除）口径不一致。复核后判定两者场景不同，不必也不该统一：
/// - 已删的那个是**绑定前**、无团别、跨全台账的模糊搜，personId 等于冒领绑定的弹药；
/// - 这里是**已登录且已限定本团**的搜索，且 `my_push_target_add` 服务端会再用
///   team_anchor_search 兜底校验 tid 必须是本团在册主播（wx.rs 的 `_ =>` 分支），
///   所以拿到 id 也不带来额外能力；
/// - 展示端 st_page.html 的追分目标选人依赖这个 id 提交，剥掉会直接打断该功能。
pub async fn my_push_search(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<PushSearchQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let token = header_my_token(&headers).unwrap_or_default();
    let (person_id, actual_team, _name) = my_ident(&st, &team, &token).await?;
    let team_scope = if team == "all" {
        actual_team.as_str()
    } else {
        team.as_str()
    };
    let key = q.q.unwrap_or_default();
    let key = key.trim();
    // 空关键词 = 默认候选（全团按本月音浪降序），不再早退；只有非空搜索才收紧 limit。
    let limit = if key.is_empty() { 50 } else { 30 };
    let items = crate::store::team_anchor_search(&st, &sid(&st), team_scope, key, limit)
        .await
        .into_iter()
        .filter(|a| a.get("personId").and_then(|v| v.as_u64()) != Some(person_id))
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub struct PushSearchQuery {
    pub q: Option<String>,
}

/// GET /api/public/teams/{team}/my/push/qr（token 走 X-My-Token 头）
/// 机器人「加好友」二维码图片（管理员在后台「机器人」页上传）。
/// 走独立图片端点是为了不让 base64 撑大设置接口的 JSON。
/// 2026-10-12 起不再认 ?access_token=（URL 进日志）；<img> 直链场景请改用
/// fetch + X-My-Token 头拿 blob 转 objectURL（当前 st_page.html 已不走这个端点）。
pub async fn my_push_qr(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    headers: HeaderMap,
) -> Response {
    let s = PublicSettings::load(&st).await;
    if gate_team(&st, s.login_required, &team, &headers)
        .await
        .is_err()
    {
        return (axum::http::StatusCode::UNAUTHORIZED, "请先登录").into_response();
    }
    let Some(data_url) = crate::store::config_get(&st, "wechat_add_qr").await else {
        return (axum::http::StatusCode::NOT_FOUND, "没有配置机器人二维码").into_response();
    };
    match decode_data_url(&data_url) {
        // P2-17：MIME 白名单 —— 上传入口（bot/mod.rs）原样透传 dataURL 的 MIME，
        // 曾经能存进 data:text/html 造成同源 XSS。这里只放行图片类型，其余一律 404。
        Some((mime, bytes))
            if matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp") =>
        {
            ([(axum::http::header::CONTENT_TYPE, mime)], bytes).into_response()
        }
        _ => (axum::http::StatusCode::NOT_FOUND, "机器人二维码格式不对").into_response(),
    }
}

/// dataURL → (mime, bytes)。P2-17：这里保留原签名（bot/mod.rs 上传校验在用它），
/// 图片 MIME 白名单只在下发端（my_push_qr）强制执行。
pub fn decode_data_url(data_url: &str) -> Option<(String, Vec<u8>)> {
    let (mime, b64) = data_url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(";base64,"))?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()?;
    Some((mime.to_string(), bytes))
}

/* --------------- 批量追人（契约 B 节）的纯逻辑单测：无 IO，覆盖去重/上限口径 --------------- */

#[cfg(test)]
mod batch_target_tests {
    use super::{parse_person_ids, value_wan_to_raw, MAX_BATCH_IDS};

    // ApiError 没实现 Debug（routes.rs，不在本任务写范围），所以不能 unwrap/unwrap_err，
    // 统一走这个小助手断言成功。
    fn parse_ok(raw: &[serde_json::Value]) -> Vec<u64> {
        match parse_person_ids(raw) {
            Ok(v) => v,
            Err(_) => panic!("期望解析成功，实际返回了 400"),
        }
    }

    fn wan_ok(v: serde_json::Value) -> i64 {
        match value_wan_to_raw(&v) {
            Ok(raw) => raw,
            Err(_) => panic!("期望通过换算，实际 400：{v}"),
        }
    }

    #[test]
    fn parse_rejects_empty_and_illegal() {
        assert!(parse_person_ids(&[]).is_err(), "空数组必须 400");
        assert!(
            parse_person_ids(&[serde_json::json!(0)]).is_err(),
            "0 必须 400"
        );
        assert!(
            parse_person_ids(&[serde_json::json!(-1)]).is_err(),
            "负数必须 400"
        );
        assert!(
            parse_person_ids(&[serde_json::json!("3")]).is_err(),
            "字符串必须 400"
        );
        assert!(
            parse_person_ids(&[serde_json::json!(1.5)]).is_err(),
            "小数必须 400"
        );
        assert!(
            parse_person_ids(&[serde_json::json!(null)]).is_err(),
            "null 必须 400"
        );
    }

    #[test]
    fn parse_dedupes_keeping_first_position() {
        let raw = [
            serde_json::json!(3),
            serde_json::json!(1),
            serde_json::json!(3),
            serde_json::json!(2),
        ];
        assert_eq!(parse_ok(&raw), vec![3, 1, 2]);
    }

    /// FIX2：入口长度上限（恰好 100 通过、101 立刻 400）。
    #[test]
    fn parse_rejects_over_length_cap() {
        let ok: Vec<serde_json::Value> = (1..=MAX_BATCH_IDS as u64)
            .map(|i| serde_json::json!(i))
            .collect();
        assert_eq!(parse_ok(&ok).len(), MAX_BATCH_IDS);
        let over: Vec<serde_json::Value> = (1..=MAX_BATCH_IDS as u64 + 1)
            .map(|i| serde_json::json!(i))
            .collect();
        assert!(
            parse_person_ids(&over).is_err(),
            "超过 {MAX_BATCH_IDS} 个必须 400"
        );
    }

    /// FIX2：5000 个唯一 id 必须在长度上限那一步**立刻**拒绝。
    /// 旧实现 `Vec::contains` 去重是 O(n²)（qa 实测 4 万 id = 4.71s，元素 ×4 → 耗时 ×16），
    /// 上限前置后这里应该是微秒级；留 50ms 只防回归，不是性能目标。
    #[test]
    fn parse_rejects_5000_ids_under_50ms() {
        let raw: Vec<serde_json::Value> = (1..=5000u64).map(|i| serde_json::json!(i)).collect();
        let t0 = std::time::Instant::now();
        assert!(parse_person_ids(&raw).is_err(), "5000 个必须 400");
        let dt = t0.elapsed();
        println!("[FIX2] parse_person_ids 拒绝 5000 个唯一 id 耗时 {dt:?}");
        assert!(
            dt < std::time::Duration::from_millis(50),
            "超长必须在上限处立刻拒绝，实测 {dt:?}"
        );
    }

    /// FIX3：value 分支补 1e8「万」上限，堵住 `target_value` 饱和成 i64::MAX。
    #[test]
    fn value_wan_cap_rejects_saturation() {
        // 合法区间行为不变：12.5 万 → 125000 音浪
        assert_eq!(wan_ok(serde_json::json!({"valueWan": 12.5})), 125_000);
        // 边界 1e8 万 → 1e12 音浪，仍允许
        assert_eq!(
            wan_ok(serde_json::json!({"valueWan": 1e8})),
            1_000_000_000_000
        );
        // qa 实测缺陷：1e300 原来 200 且落库 i64::MAX
        assert!(
            value_wan_to_raw(&serde_json::json!({"valueWan": 1e300})).is_err(),
            "1e300 必须 400"
        );
        assert!(
            value_wan_to_raw(&serde_json::json!({"valueWan": 1.1e8})).is_err(),
            "超过 1e8 万必须 400"
        );
        assert!(
            value_wan_to_raw(&serde_json::json!({"valueWan": 0})).is_err(),
            "0 必须 400"
        );
        assert!(
            value_wan_to_raw(&serde_json::json!({})).is_err(),
            "缺字段必须 400"
        );
    }

    #[test]
    fn counts_json_shape_is_frozen() {
        // 既有字段不能少、不能改名：counts 固定三键
        let v = super::counts_json((2, 3, 1));
        assert_eq!(v["person"], serde_json::json!(2));
        assert_eq!(v["value"], serde_json::json!(3));
        assert_eq!(v["rank"], serde_json::json!(1));
        assert_eq!(v.as_object().map(|o| o.len()), Some(3));
    }
}

/* --------------- 扫码保存失败的出口（2026-10-16）：业务冲突 400 / 基础设施 500 --------------- */

#[cfg(test)]
mod wx_save_failure_tests {
    use super::wx_save_failure;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    /// 业务冲突（微信已绑别处 / 该主播已有微信账号）→ 400 + 原样人话。
    /// 回归（2026-10-16 用户报障）：以前这类拒绝走 anyhow 兜底 = 500
    /// 「服务器开小差了，请稍后重试」，用户看不到「请先解绑旧的再扫」，
    /// 展示端还会把它当网络抖动每 3 秒重试 10 次。
    #[test]
    fn bind_conflict_maps_to_400() {
        let e: anyhow::Error = crate::store::WxBindConflict(
            "该微信已经绑定在另一个账号上；一个微信只能绑一个主播，请先解绑旧的再扫"
                .into(),
        )
        .into();
        assert_eq!(
            wx_save_failure(e).into_response().status(),
            StatusCode::BAD_REQUEST
        );
    }

    /// 非业务错误（PG / 写池 / 网络）仍是 500：文案由 routes 层统一换成通用话术，
    /// 底层报错（表名 / 约束名）绝不外发。
    #[test]
    fn infra_error_stays_500() {
        let e = anyhow::anyhow!(
            "error returned from database: relation \"wx_push_accounts\" does not exist"
        );
        assert_eq!(
            wx_save_failure(e).into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
