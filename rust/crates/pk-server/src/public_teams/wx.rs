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
        return Err(anyhow::anyhow!("该账号没有主播档案，不能绑定微信推送").into());
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
        return Err(anyhow::anyhow!("微信接口没有返回二维码").into());
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
        return Err(anyhow::anyhow!("缺少 qrcode").into());
    }
    let verify = body
        .get("verifyCode")
        .or_else(|| body.get("verify_code"))
        .and_then(|v| v.as_str());
    let Some((_pid, team_scope, poll_base)) = pending_wx_take(&qrcode, &name) else {
        return Err(anyhow::anyhow!("二维码已过期，请重新获取").into());
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
        return Err(anyhow::anyhow!("扫码已确认，但微信没有返回完整凭证").into());
    }
    let acct = crate::store::WxPushAccount {
        name: name.clone(),
        team: team_scope.clone(),
        person_id,
        ilink_bot_id: bot_id,
        ilink_user_id: user_id,
        bot_token,
        base_url: out
            .get("baseUrl")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        nickname: out
            .get("nickname")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        conversation_id: String::new(),
        updates_buf: String::new(),
        bound_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    };
    crate::store::wx_push_account_save(&st, &acct)
        .await
        .map_err(|e| anyhow::anyhow!("保存微信凭证失败：{e}"))?;
    // 先建 push_bindings 行（未通话时 bound=false，但两个推送开关可先设置）
    crate::store::push_bind_account(&st, &name, &team_scope, "", "").await;
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
        .map_err(|e| anyhow::anyhow!("生成绑定码失败：{e}"))?;
    let state = crate::store::push_state(&st, &name).await;
    Ok(Json(json!({
        "ok": true,
        "code": state.get("code").cloned().unwrap_or(Value::Null),
        "codeExpires": state.get("codeExpires").cloned().unwrap_or(Value::Null),
    })))
}

/// POST /api/public/teams/{team}/my/push/settings {token, notifyTarget, dailyReport}
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
        return Err(anyhow::anyhow!("管理员观看账号没有音浪数据，不能设置追分目标").into());
    }
    if crate::store::target_count(&st, &name).await >= crate::push::MAX_TARGETS {
        return Err(anyhow::anyhow!(format!(
            "最多只能追 {} 个人，请先删掉一个",
            crate::push::MAX_TARGETS
        ))
        .into());
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
                .ok_or_else(|| anyhow::anyhow!("请输入本月目标（万），要大于 0"))?;
            let raw = (wan * 10_000.0).round() as i64;
            crate::store::target_add(&st, &name, team_scope, "value", 0, "", raw)
                .await
                .map_err(|e| anyhow::anyhow!("保存失败：{e}"))?;
        }
        // 追名次（2026-10-10）：target_value 存目标名次，名字列存展示文案
        "rank" => {
            let rank = body
                .get("targetRank")
                .and_then(|v| v.as_i64())
                .filter(|v| (1..=999).contains(v))
                .ok_or_else(|| anyhow::anyhow!("请输入要追的名次（1~999）"))?;
            let dup = crate::store::targets_for_push(&st, &name)
                .await
                .into_iter()
                .any(|t| t.kind == "rank" && t.target_value == rank);
            if dup {
                return Err(anyhow::anyhow!("已经在追这个名次了").into());
            }
            crate::store::target_add(
                &st,
                &name,
                team_scope,
                "rank",
                0,
                &format!("第 {rank} 名"),
                rank,
            )
            .await
            .map_err(|e| anyhow::anyhow!("保存失败：{e}"))?;
        }
        _ => {
            let tid = body
                .get("personId")
                .and_then(|v| v.as_u64())
                .filter(|v| *v > 0)
                .ok_or_else(|| anyhow::anyhow!("请选择要追的主播"))?;
            if tid == person_id {
                return Err(anyhow::anyhow!("不能追自己").into());
            }
            // 只允许追本团在册主播（搜索接口也只下发本团名单，这里是服务端兜底）
            let anchor = crate::store::team_anchor_search(&st, &sid(&st), team_scope, "", 1000)
                .await
                .into_iter()
                .find(|a| a.get("personId").and_then(|v| v.as_u64()) == Some(tid));
            let Some(anchor) = anchor else {
                return Err(anyhow::anyhow!("没有找到这个主播（只能追本团在册主播）").into());
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
                        return Err(anyhow::anyhow!("目标分要大于 0（万）").into());
                    }
                    if w > 100_000_000.0 {
                        return Err(anyhow::anyhow!("目标分太大了，检查一下单位（万）").into());
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
                return Err(anyhow::anyhow!("已经在追这个主播了").into());
            }
            crate::store::target_add(&st, &name, team_scope, "person", tid, &display, custom_raw)
                .await
                .map_err(|e| anyhow::anyhow!("保存失败：{e}"))?;
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
        .ok_or_else(|| anyhow::anyhow!("缺少目标 id"))?;
    crate::store::target_delete(&st, &name, id as i64).await;
    Ok(Json(
        json!({ "targets": crate::store::targets_list(&st, &name).await }),
    ))
}

/// GET /api/public/teams/{team}/my/push/search?q=（token 走 X-My-Token 头）：
/// 本团主播搜索（只回 id + 名字）
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
    if gate_team(s.login_required, &team, &headers).is_err() {
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
