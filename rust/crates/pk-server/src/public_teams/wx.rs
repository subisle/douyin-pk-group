//! public_teams::wx —— 自 public_teams.rs 按业务域拆分（对外语义不变）。

use super::*;

/* ------------------ 我的页面 · 微信推送（绑定 + 追分目标 + 每日报告） ------------------ */

/// 登录态 → (personId, 团别, 账号姓名)。推送相关接口全部走这一处校验。
pub(super) fn my_ident(team: &str, token: &str) -> Result<(u64, String, String), ApiError> {
    let token = token.trim();
    let Some((pid, my_team, name)) = my_token_take(token) else {
        return Err(ApiError::auth("登录已过期，请重新登录"));
    };
    if my_team != team && team != "all" {
        return Err(ApiError::auth("该账号不属于本团"));
    }
    Ok((pid, my_team, name))
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
    let (person_id, _my_team, name) = my_ident(&team, &token)?;
    let mut state = crate::store::push_state(&st, &name).await;
    let targets = crate::store::targets_list(&st, &name).await;
    let has_qr = crate::store::config_get(&st, "wechat_add_qr")
        .await
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if let Some(obj) = state.as_object_mut() {
        obj.insert("targets".into(), json!(targets));
        obj.insert("maxTargets".into(), json!(crate::push::MAX_TARGETS));
        obj.insert("hasQr".into(), json!(has_qr));
        obj.insert("personId".into(), json!(person_id));
        // 管理员观看账号（personId=0）没有自己的音浪，追分对比没有意义
        obj.insert("admin".into(), json!(person_id == 0));
        obj.insert("channelConfigured".into(), json!(true));
        // 用户自助扫码（2026-10-11）：凭证已保存 = 可推送；等待首次消息才有会话上下文
        let wxacct = crate::store::wx_push_account_get(&st, &name).await;
        obj.insert("scanBound".into(), json!(wxacct.is_some()));
        obj.insert(
            "needActivate".into(),
            json!(wxacct
                .as_ref()
                .map(|a| a.conversation_id.is_empty())
                .unwrap_or(false)),
        );
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
        // 口径（父任务指定）：person_id > 0 且本人 master_id IS NULL（开山师傅），
        // 且存在未删除的直系徒弟（c.master_id = person_id AND c.deleted_at IS NULL）。
        // 折成一条 COUNT 查询（结果恒 0/1），走 pgrs 只读池；档案不存在 → 0。
        // ⚠️ 口径备注：lineage_report::build() 与 docs/23 §4.2 的跳过判定只看
        // 「build 出来的行数 ≤ 1」（不要求本人 master_id IS NULL），若产品后续要
        // 放宽到「中间层师傅」（自己也有师傅但名下有徒弟），删掉 p.master_id IS NULL
        // 这半个条件即可，推送侧无需改动（它按 build 结果兜底）。
        let has_disciples = if person_id > 0 {
            let sql = "SELECT COUNT(*) AS n FROM person p \
              WHERE p.id = ? AND p.deleted_at IS NULL AND p.master_id IS NULL \
                AND EXISTS (SELECT 1 FROM person c \
                     WHERE c.master_id = p.id AND c.deleted_at IS NULL)";
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

/* ------------------ 用户自助扫码绑定微信推送（2026-10-11，多账号） ------------------
 *
 * 官方协议：POST get_bot_qrcode 取账号登录二维码；用户扫码确认后
 * get_qrcode_status 返回 bot_token / ilink_bot_id / ilink_user_id / baseurl。
 * 我们把二维码会话与当前「我的」登录账号绑定（qrcode → name），确认后
 * 落 wx_push_accounts 并拉起该账号的 getupdates 长轮询；用户首次给机器人
 * 发消息时留下 context_token，随后推送走该账号自己的 token。 */

pub(super) type PendingWxLogin = (String, u64, String, String, Instant);
pub(super) type PendingWxLoginMap = HashMap<String, PendingWxLogin>;

pub(super) fn pending_wx_logins() -> &'static Mutex<PendingWxLoginMap> {
    static M: OnceLock<Mutex<PendingWxLoginMap>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn pending_wx_put(qrcode: &str, name: &str, person_id: u64, team: &str) {
    let mut m = pending_wx_logins()
        .lock()
        .expect("pending wx login 锁 poisoned");
    m.retain(|_, (_, _, _, _, at)| at.elapsed() < Duration::from_secs(5 * 60));
    m.insert(
        qrcode.to_string(),
        (
            name.to_string(),
            person_id,
            team.to_string(),
            String::new(),
            Instant::now(),
        ),
    );
}

/// 读一次未过期的扫码会话（保留在 map 里，轮询可重复调用）；
/// 返回 (person_id, team, 轮询用的 base_url)。
pub(super) fn pending_wx_take(qrcode: &str, name: &str) -> Option<(u64, String, String)> {
    let mut m = pending_wx_logins()
        .lock()
        .expect("pending wx login 锁 poisoned");
    m.retain(|_, (_, _, _, _, at)| at.elapsed() < Duration::from_secs(5 * 60));
    match m.get(qrcode) {
        Some((n, pid, team, base, _)) if n == name => Some((*pid, team.clone(), base.clone())),
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
    if let Some(v) = m.get_mut(qrcode) {
        v.3 = base_url.trim().to_string();
    }
}

/// POST /api/public/teams/{team}/my/push/wechat/start {token}
/// 取官方协议的用户扫码登录二维码（默认国内 API Base）
pub async fn my_push_wechat_start(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (person_id, _actual_team, name) = my_ident(&team, token)?;
    if person_id == 0 && !crate::store::my_account_is_admin(&st, &name).await {
        // 登录账号缺主播档案（管理员观看账号）：请求态不能做这个操作 → 显式 400
        return Err(ApiError::bad_request("该账号没有主播档案，不能绑定微信推送"));
    }
    // 管理员观看账号（personId=0）也允许扫码绑定；团别留空，绑定后可看「我的」页推送入口。
    let team_scope = crate::store::person_team_of_name(&st, &name)
        .await
        .unwrap_or_default();
    let out = crate::bot::weixin::user_login_qrcode().await?;
    let qrcode = out
        .get("qrcode")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if qrcode.is_empty() {
        // 上游微信接口没吐二维码（上游故障或参数被拒）：给用户引导话术 400，细节留上游日志
        return Err(ApiError::bad_request("微信接口没有返回二维码，请稍后再试"));
    }
    pending_wx_put(&qrcode, &name, person_id, &team_scope);
    Ok(Json(json!({
        "ok": true,
        "qrcode": qrcode,
        "imgContent": out.get("imgContent").cloned().unwrap_or(Value::Null),
        "url": out.get("url").cloned().unwrap_or(Value::Null),
    })))
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
    let (person_id, _actual_team, name) = my_ident(&team, token)?;
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
    if bot_token.is_empty() || bot_id.is_empty() || user_id.is_empty() {
        return Err(ApiError::bad_request("扫码已确认，但微信没有返回完整凭证，请重试"));
    }
    // 重扫不丢已激活绑定（2026-10-13 用户口径：配置还要和之前一样）：
    // 覆盖凭证前先取库里已有账号，下面重申 push_bindings 时带上旧会话。
    let existing = crate::store::wx_push_account_get(&st, &name).await;
    let mut acct = crate::store::WxPushAccount {
        name: name.clone(),
        team: team_scope.clone(),
        person_id,
        ilink_bot_id: bot_id,
        ilink_user_id: user_id,
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
        .map(|e| {
            !e.ilink_user_id.is_empty()
                && e.ilink_user_id != acct.ilink_user_id
        })
        .unwrap_or(false);
    if rescan_new_wechat {
        acct.conversation_id.clear();
        crate::store::wx_push_account_delete(&st, &name).await;
    }
    crate::store::wx_push_account_save(&st, &acct)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "保存微信凭证失败");
            anyhow::anyhow!("保存微信凭证失败：{e}")
        })?;
    if rescan_new_wechat {
        if let Some(e) = &existing {
            let old_conv =
                format!("wxacct:{}:{}", e.ilink_bot_id, e.ilink_user_id);
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
    Ok(Json(json!({
        "status": "confirmed",
        "tokenSaved": true,
        "needActivate": true,
    })))
}

/// POST /api/public/teams/{team}/my/push/code {token}：生成/刷新绑定码（30 分钟有效）
pub async fn my_push_code(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (person_id, _t, name) = my_ident(&team, token)?;
    let code = crate::push::new_bind_code();
    crate::store::push_set_code(&st, &name, &team, person_id, &code)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "生成绑定码失败");
            anyhow::anyhow!("生成绑定码失败：{e}")
        })?;
    let state = crate::store::push_state(&st, &name).await;
    Ok(Json(json!({
        "ok": true,
        "code": state.get("code").cloned().unwrap_or(Value::Null),
        "codeExpires": state.get("codeExpires").cloned().unwrap_or(Value::Null),
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
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (_pid, _t, name) = my_ident(&team, token)?;
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
pub async fn my_push_unbind(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (_pid, _t, name) = my_ident(&team, token)?;
    crate::store::push_unbind(&st, &name).await;
    crate::store::wx_push_account_delete(&st, &name).await;
    Ok(Json(json!({ "ok": true })))
}

/// POST /api/public/teams/{team}/my/push/targets {token, kind, personId?|valueWan?}
/// kind = person（追人）/ value（追数值，前端填万，后端换算成音浪）
pub async fn my_push_target_add(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (person_id, actual_team, name) = my_ident(&team, token)?;
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
    if crate::store::target_count(&st, &name).await >= crate::push::MAX_TARGETS {
        return Err(ApiError::bad_request(format!(
            "最多只能追 {} 个人，请先删掉一个",
            crate::push::MAX_TARGETS
        )));
    }
    let kind = body
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("person");
    match kind {
        "value" => {
            let wan = body
                .get("valueWan")
                .and_then(|v| v.as_f64())
                .filter(|v| *v > 0.0)
                .ok_or_else(|| ApiError::bad_request("请输入本月目标（万），要大于 0"))?;
            let raw = (wan * 10_000.0).round() as i64;
            let t = crate::store::target_add(&st, &name, team_scope, "value", 0, "", raw)
                .await;
            if let Err(e) = t {
                // 底层写库失败：原文留日志，客户端给通用提示（不再靠「失败」关键词兜）
                tracing::error!(error = %e, "保存追分目标（自定义分）失败");
                return Err(anyhow::anyhow!("保存失败：{e}").into());
            }
        }
        // 追名次（2026-10-10）：target_value 存目标名次，名字列存展示文案
        "rank" => {
            let rank = body
                .get("targetRank")
                .and_then(|v| v.as_i64())
                .filter(|v| (1..=999).contains(v))
                .ok_or_else(|| ApiError::bad_request("请输入要追的名次（1~999）"))?;
            let dup = crate::store::targets_for_push(&st, &name)
                .await
                .into_iter()
                .any(|t| t.kind == "rank" && t.target_value == rank);
            if dup {
                return Err(ApiError::bad_request("已经在追这个名次了"));
            }
            let t = crate::store::target_add(
                &st,
                &name,
                team_scope,
                "rank",
                0,
                &format!("第 {rank} 名"),
                rank,
            )
            .await;
            if let Err(e) = t {
                tracing::error!(error = %e, "保存追分目标（名次）失败");
                return Err(anyhow::anyhow!("保存失败：{e}").into());
            }
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
                return Err(ApiError::bad_request("没有找到这个主播（只能追本团在册主播）"));
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
                        return Err(ApiError::bad_request(
                            "目标分太大了，检查一下单位（万）",
                        ));
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
            // 同一个人不重复追
            let dup = crate::store::targets_for_push(&st, &name)
                .await
                .into_iter()
                .any(|t| t.kind == "person" && t.target_person_id == tid);
            if dup {
                return Err(ApiError::bad_request("已经在追这个主播了"));
            }
            let t =
                crate::store::target_add(&st, &name, team_scope, "person", tid, &display, custom_raw)
                    .await;
            if let Err(e) = t {
                tracing::error!(error = %e, "保存追分目标（追人）失败");
                return Err(anyhow::anyhow!("保存失败：{e}").into());
            }
        }
    }
    Ok(Json(
        json!({ "targets": crate::store::targets_list(&st, &name).await }),
    ))
}

/// POST /api/public/teams/{team}/my/push/targets/delete {token, id}
pub async fn my_push_target_del(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (_pid, _t, name) = my_ident(&team, token)?;
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
/// 本团主播搜索（只回 id + 名字）
///
/// 关于 `personId`（2026-10-14 审计复核结论：**保持下发，不改**）：
/// 审计疑此处与 `my_bind_search`（my.rs）口径不一致（后者 2026-10-12 起剥掉 personId）。
/// 复核后判定两者场景不同，不必也不该统一：
/// - `my_bind_search` 是**绑定前**、无团别、跨全台账的模糊搜，personId 等于冒领绑定的弹药；
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
    let (person_id, actual_team, _name) = my_ident(&team, &token)?;
    let team_scope = if team == "all" {
        actual_team.as_str()
    } else {
        team.as_str()
    };
    let key = q.q.unwrap_or_default();
    let key = key.trim();
    if key.is_empty() {
        return Ok(Json(json!({ "items": [] })));
    }
    let items = crate::store::team_anchor_search(&st, &sid(&st), team_scope, key, 20)
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
    if gate_team(&st, s.login_required, &team, &headers).await.is_err() {
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
