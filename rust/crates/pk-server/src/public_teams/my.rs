//! public_teams::my —— 自 public_teams.rs 按业务域拆分（对外语义不变）。

use super::*;

/* ------------------------------- 我的（验证 + 个人数据） ------------------------------- */

/// POST /api/public/teams/{team}/verify {account, pass}（account = 手机号或姓名；兼容旧 {name, pass}）
/// 没有注册：账号由管理员在后台手动添加（my_accounts 表，密码纯数字，手机号选填）。
/// 验证通过后在本团主播台账里按姓名找人（同名时在团者优先）；只返回「成功 + token」
/// 或统一失败 {ok:false}，绝不下发候选名单。
pub async fn verify(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    /* 地区门禁：公开 API 层的中间件已经统一拦了，这里再过一遍是因为 verify
    要按「登录尝试」记日志——外省被拒的尝试也进 access_logs，后台访客日志能看到
    谁在门外试了什么账号（IP_GATE_DISABLED=1 的本地开发直接放行）。 */
    let client_ip = crate::ipinfo::request_ip(addr, &headers);
    let geo = crate::ipinfo::geo_for_ip(&st, &client_ip)
        .await
        .unwrap_or_default();
    let account_input = body
        .get("account")
        .or_else(|| body.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !crate::ipinfo::gate_disabled() && !geo.allowed {
        crate::ipinfo::log_login(&st, &client_ip, &geo, &team, &account_input, false).await;
        tracing::info!(
            ip = %client_ip, team = %team, province = %geo.province,
            "展示端登录尝试被地区规则拒绝"
        );
        return Err(ApiError::forbidden("当前地区暂不支持访问"));
    }
    let limiter_ok = public_write_allow(&client_ip_for_limit(addr, &headers));
    if !limiter_ok {
        return Err(ApiError::with_status(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "尝试太频繁，请稍后再试",
        ));
    }
    // 账号 = 手机号或姓名：新前端传 account，老前端传 name，两者都行
    let acct = body
        .get("account")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| {
            body.get("name")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
        .unwrap_or_default();
    let pass = body
        .get("pass")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if acct.is_empty() || pass.trim().is_empty() {
        return Err(ApiError::bad_request("请输入账号和密码"));
    }

    /* 1. 账号校验：手机号+密码 或 姓名+密码（my_accounts 名单，管理员手动维护 / 手机号自助注册） */
    let Some(account_name) = crate::store::my_account_check_login(&st, &acct, &pass).await else {
        crate::ipinfo::log_login(&st, &client_ip, &geo, &team, &acct, false).await;
        return Ok(Json(json!({ "ok": false }))); // 统一失败，不区分「账号不存在/密码错」
    };
    crate::ipinfo::log_login(&st, &client_ip, &geo, &team, &account_name, true).await;

    /* 1.5 手机号自助注册还没绑主播的账号：先放进来（personId=0），登录后「我的」页
    会强制让用户绑定主播——不绑定看不到个人数据，也无从套用别人的姓名。 */
    if let Some(info) = crate::store::my_account_info(&st, &account_name).await {
        if !info.bound && !info.is_admin {
            let token = my_token_put(0, team.clone(), account_name.clone());
            return Ok(Json(json!({
                "ok": true,
                "personId": 0,
                "name": account_name,
                "token": token,
                "bound": false,
                "needBind": true,
            })));
        }
    }

    /* 2. 台账里按姓名找本团的人。找不到 = 管理员加了账号但台账还没有这个人
    （或名字打错了），同样返回统一失败。绑定用 bind，姓名是用户输入不能拼 SQL。 */
    let sql = "SELECT p.id, p.name, p.gender FROM person p \
         WHERE p.deleted_at IS NULL AND p.name = ? \
         ORDER BY CASE WHEN p.status = 'left' THEN 1 ELSE 0 END, p.id \
         LIMIT 1"
        .to_string();
    let rows = pgrs::query_with_binds(&st.pools, &sid(&st), &sql, |q| q.bind(account_name.clone()))
        .await?;
    match rows.into_iter().next() {
        Some(row) => {
            let pid = row
                .try_get::<i64, _>("id")
                .map(|v| v.max(0) as u64)
                .unwrap_or(0);
            let token_team = row.try_get::<String, _>("gender").unwrap_or_default();
            let token = my_token_put(pid, token_team.clone(), account_name.clone());
            Ok(Json(json!({
                "ok": true, "personId": pid, "name": account_name, "token": token, "bound": true,
                // 登录一次就切对团：前端 applyLoginState 按这个字段设 TEAM，避免登录后
                // 还用默认团拉数据 → 401 → 二次弹登录门
                "team": token_team
            })))
        }
        None => {
            // 台账里没有同名主播：管理员观看账号（is_admin=1，如 shuai4444）放行，
            // personId=0——榜单/族谱/分组照常看，「我的」个人数据页为空。
            if !crate::store::my_account_is_admin(&st, &account_name).await {
                return Ok(Json(json!({ "ok": false })));
            }
            let token = my_token_put(0, team, account_name.clone());
            Ok(Json(json!({
                "ok": true, "personId": 0, "name": account_name, "token": token,
                "admin": true, "bound": true
            })))
        }
    }
}

/* ----------------- 手机号注册（WorkBuddy 网关 OAuth）+ 绑定主播 -----------------
 *
 * 用户口径（2026-10-10）：
 *   1. 展示端登录门加「注册」——注册同时调用 WorkBuddy 网关的「添加账号 (OAuth)」，
 *      那张授权页本身就是手机号 + 短信验证码登录；
 *   2. 验证码数字就是密码，手机号就是账号（所以这里不需要短信通道）；
 *   3. 注册完提醒绑定主播姓名，绑定之后用户自己不能解绑（只有管理员能改绑）。
 *
 * 分工：面板密码只在服务端，浏览器只拿 state + authUrl（`wbgateway` 模块）。
 */

/* --------------- 免团别登录口（/api/public/my/login）---------------
 *
 * 前端登录门在「团别待定」时（首页直接登录，不知道访客看男团还是女团）调这里，
 * 由账号反推团别。与 verify 的区别只有「团别从哪来」：
 * - verify：路径上带着团别（/api/public/teams/{team}/verify）
 * - my_login：账号在 my_accounts 里绑定了主播 → 用主播所在团；
 *   管理员观看账号（is_admin=1）→ 不落团，token 挂 MY_ANY_KEY，两个团都能用。
 *
 * 曾长期只有前端调用而路由没注册（main.rs 漏了），登录门在首页一直「接口不存在」；
 * 2026-10-11 补齐。 */

pub async fn my_login(
    State(st): State<ArcState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    // 登录门默认关（login_required=false）时这个口没有意义，但也不该报错——照常验，
    // 有账号就给 token；没有公开站点时直接拒绝，避免空跑。
    if !s.team_enabled("male") && !s.team_enabled("female") {
        return Ok(Json(json!({ "published": false })));
    }
    /* 地区门禁同 verify：handler 自己过一遍，被拒的尝试也要进 access_logs。 */
    let client_ip = crate::ipinfo::request_ip(addr, &headers);
    let geo = crate::ipinfo::geo_for_ip(&st, &client_ip)
        .await
        .unwrap_or_default();
    let account_input = body
        .get("account")
        .or_else(|| body.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !crate::ipinfo::gate_disabled() && !geo.allowed {
        // 团别待定，team 记空串（log_login_path 的语义：待定传空）
        crate::ipinfo::log_login_path(
            &st,
            &client_ip,
            &geo,
            "",
            "/api/public/my/login",
            &account_input,
            false,
        )
        .await;
        tracing::info!(
            ip = %client_ip, province = %geo.province,
            "免团别登录尝试被地区规则拒绝"
        );
        return Err(ApiError::forbidden("当前地区暂不支持访问"));
    }
    let limiter_ok = public_write_allow(&client_ip_for_limit(addr, &headers));
    if !limiter_ok {
        return Err(ApiError::with_status(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "尝试太频繁，请稍后再试",
        ));
    }
    /* 2026-10-14 审计 P1：登录接入 LoginLimiter（连续失败锁 10 分钟）。
    原先这里只过上面的 5 次/分滑动窗口，攻击者换 IP 即可对公开端点慢速爆破
    管理员口令（下面的快捷口令分支会跑 argon2，白送算力）。现在与 /api/auth/login
    共用同一套「连续失败 5 次锁 10 分钟」，key 用同一来源口径并加 mylogin: 前缀分桶。 */
    let login_key = format!("mylogin:{}", client_ip_for_limit(addr, &headers));
    if st.auth.login_locked(&login_key) {
        let wait = st.auth.login_locked_seconds(&login_key);
        tracing::warn!(ip = %client_ip, wait_s = wait, "展示端登录失败次数过多，来源被临时锁定");
        // 与 /api/auth/login 同一响应结构（error + lockedFor），前端可做倒计时提示
        return Err(ApiError::with_json(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            json!({
                "error": format!("失败次数过多，请 {wait} 秒后再试"),
                "lockedFor": wait,
            }),
        ));
    }
    let acct = body
        .get("account")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| {
            body.get("name")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
        .unwrap_or_default();
    let pass = body
        .get("pass")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if acct.is_empty() || pass.trim().is_empty() {
        return Err(ApiError::bad_request("请输入账号和密码"));
    }

    /* 0. 管理员快捷口令（账号、密码填同一个管理员口令即可登录展示页）：
    限流之后、my_accounts 账号校验之前判断——这样管理员不用在 my_accounts 里
    专门留一行账号。口令读自运行时配置（ADMIN_PASSWORD），改 .env 重启即生效；
    未配置口令时 verify_admin_password 恒 false（fail-closed）。
    2026-10-12 安全审计 M1：account 字段绝不能落口令本体——access_logs 运营可查
    （/api/admin/visits 支持关键字搜索）且随备份 dump 外流。这里记固定占位符。 */
    let admin_shortcut = acct == pass && st.auth.verify_admin_password(&acct);
    if admin_shortcut {
        st.auth.login_record_success(&login_key);
        crate::ipinfo::log_login_path(
            &st,
            &client_ip,
            &geo,
            "",
            "/api/public/my/login",
            "admin-gate",
            true,
        )
        .await;
        let token = my_token_put(0, String::new(), acct.clone());
        tracing::info!(ip = %client_ip, "展示端管理员口令登录成功（账号=密码=管理员口令）");
        return Ok(Json(json!({
            "ok": true, "personId": 0, "name": acct,
            "token": token, "admin": true, "bound": true
        })));
    }
    /* 2026-10-14（本代理修正）：只有当「快捷分支确实在管管理员口令」时，没对上才记失败 ——
    也就是 ADMIN_PASSWORD 已配置（verify_admin_password 有可能为真）。没配口令时
    这个分支恒 false，正常用户的 acct≠pass 会在这里被误记失败，NAT 后 5 个人就
    能把整栋楼锁 10 分钟（5 次失败阈值是「来源」级，不是「账号」级）。
    反过来，真正打快捷分支的爆破（acct==pass 且口令错）一定满足「已配置」，
    一次不漏全部计数 —— 防爆破语义不变。 */
    if st.auth.enabled() {
        let now_locked = st.auth.login_record_failure(&login_key);
        if now_locked {
            tracing::warn!(ip = %client_ip, "展示端登录连续失败达到阈值，锁定 10 分钟");
        }
    }

    /* 1. 账号校验（与 verify 同一套 my_accounts） */
    let Some(account_name) = crate::store::my_account_check_login(&st, &acct, &pass).await else {
        let _now_locked = st.auth.login_record_failure(&login_key);
        crate::ipinfo::log_login_path(
            &st,
            &client_ip,
            &geo,
            "",
            "/api/public/my/login",
            &acct,
            false,
        )
        .await;
        return Ok(Json(json!({ "ok": false })));
    };
    // 账号密码正确 → 清零失败计数（与 /api/auth/login 同口径：成功即解锁）
    st.auth.login_record_success(&login_key);
    /* 1.5 账号没绑主播：两个团都放行，token 挂 MY_ANY_KEY（登录后强制绑定） */
    if let Some(info) = crate::store::my_account_info(&st, &account_name).await {
        if !info.bound && !info.is_admin {
            crate::ipinfo::log_login_path(
                &st,
                &client_ip,
                &geo,
                "",
                "/api/public/my/login",
                &account_name,
                true,
            )
            .await;
            let token = my_token_put(0, String::new(), account_name.clone());
            return Ok(Json(json!({
                "ok": true, "personId": 0, "name": account_name,
                "token": token, "bound": false, "needBind": true,
            })));
        }
    }
    /* 2. 反查主播所在团：账号名命中的主播可能是男团也可能是女团，都查一遍。 */
    let mut found: Option<(u64, String, String)> = None; // (person_id, name, team)
    {
        let sql = "SELECT p.id, p.name, p.gender FROM person p \
             WHERE p.deleted_at IS NULL AND p.name = ? \
             ORDER BY CASE WHEN p.status = 'left' THEN 1 ELSE 0 END, p.id \
             LIMIT 1"
            .to_string();
        let rows =
            pgrs::query_with_binds(&st.pools, &sid(&st), &sql, |q| q.bind(account_name.clone()))
                .await?;
        if let Some(row) = rows.into_iter().next() {
            let pid = row
                .try_get::<i64, _>("id")
                .map(|v| v.max(0) as u64)
                .unwrap_or(0);
            let name: String = row.try_get::<String, _>("name").unwrap_or_default();
            let team: String = row.try_get::<String, _>("gender").unwrap_or_default();
            found = Some((pid, name, team));
        }
    }
    match found {
        Some((pid, name, team)) => {
            crate::ipinfo::log_login_path(
                &st,
                &client_ip,
                &geo,
                &team,
                "/api/public/my/login",
                &name,
                true,
            )
            .await;
            let token = my_token_put(pid, team.clone(), name.clone());
            Ok(Json(json!({
                "ok": true, "personId": pid, "name": name, "team": team,
                "token": token, "bound": true
            })))
        }
        None => {
            /* 3. 管理员观看账号：两个团都放行（token 挂 MY_ANY_KEY），「我的」数据页为空 */
            if !crate::store::my_account_is_admin(&st, &account_name).await {
                return Ok(Json(json!({ "ok": false })));
            }
            crate::ipinfo::log_login_path(
                &st,
                &client_ip,
                &geo,
                "",
                "/api/public/my/login",
                &account_name,
                true,
            )
            .await;
            let token = my_token_put(0, String::new(), account_name.clone());
            Ok(Json(json!({
                "ok": true, "personId": 0, "name": account_name,
                "token": token, "admin": true, "bound": true
            })))
        }
    }
}

/// 登录验证限流：每 IP 每分钟 5 次（登录门撞库防护）
pub(super) fn public_write_allow(ip: &str) -> bool {
    VERIFY_LIMITER
        .get_or_init(|| VerifyLimiter::new(5))
        .allow(ip)
}

/// 注册类写接口限流：每 IP 每分钟 12 次（一次注册本来就要接好几下，单独一档）
pub(super) fn public_register_allow(ip: &str) -> bool {
    REGISTER_LIMITER
        .get_or_init(|| VerifyLimiter::new(12))
        .allow(ip)
}

/// 授权轮询限流：每 IP 每分钟 120 次（前端 2.5s 轮一次 ≈ 24 次/分，余量给重试）
pub(super) fn public_poll_allow(ip: &str) -> bool {
    REGISTER_POLL_LIMITER
        .get_or_init(|| VerifyLimiter::new(120))
        .allow(ip)
}

/// 注册开关（用户口径 2026-10-10「注册先不开」）：默认关，后台「公开站点设置 → 开放手机号注册」开。
/// 关着时注册相关接口一律 400；登录、绑定、我的页完全不受影响。
pub(super) async fn ensure_register_open(st: &AppState) -> Result<(), ApiError> {
    if PublicSettings::load(st).await.my_register {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "注册功能暂未开放（管理员可在后台「公开站点设置 → 开放手机号注册」里打开）",
        ))
    }
}

/// GET /api/public/my/register/config：注册页能力探测（功能开没开 + 网关配没配）
pub async fn my_register_config(State(st): State<ArcState>) -> ApiResult {
    let open = PublicSettings::load(&st).await.my_register;
    Ok(Json(json!({
        "open": open,
        "oauth": open && st.wb.get().is_some(),
        "platform": "CLI",
    })))
}

/// POST /api/public/my/register/oauth/start {realm}：向网关申请一条「添加账号」授权链接。
/// 返回 {state, authUrl, realm}；面板密码不出服务端。
pub async fn my_register_oauth_start(
    State(st): State<ArcState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    ensure_register_open(&st).await?;
    if !public_register_allow(&client_ip_for_limit(addr, &headers)) {
        return Err(ApiError::with_status(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "尝试太频繁，请稍后再试",
        ));
    }
    let Some(wb) = st.wb.get() else {
        // 面向用户的配置缺口提示：注册流程在没配网关时给 400 引导话术（非服务端故障）
        return Err(ApiError::bad_request(
            "服务端还没有配置 WorkBuddy 网关（WORKBUDDY_PANEL_PASSWORD），暂时不能自动添加账号",
        ));
    };
    let realm = body
        .get("realm")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (state, auth_url, realm) = wb
        .login_start(realm)
        .await
        .map_err(|e| {
            // 网关返回的 e 可能带上游 HTTP 细节：给出面向用户的主文案，细节留日志
            tracing::warn!(error = %e, "申请 WorkBuddy 授权链接失败");
            ApiError::bad_request("申请授权链接失败，请稍后再试或联系管理员")
        })?;
    Ok(Json(
        json!({ "ok": true, "state": state, "authUrl": auth_url, "realm": realm }),
    ))
}

#[derive(Deserialize)]
pub struct OauthPollQuery {
    pub state: Option<String>,
}

/// 姓名校验查询串：`?q=主播姓名`
#[derive(Deserialize)]
pub struct NameQuery {
    pub q: Option<String>,
}

/// GET /api/public/my/register/oauth/poll?state=：轮询授权结果（原样转发网关状态）
pub async fn my_register_oauth_poll(
    State(st): State<ArcState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<OauthPollQuery>,
) -> ApiResult {
    /* 审计 2026-10-11：轮询接口会代打 WorkBuddy 网关面板，加一档宽松限流
    （前端 2.5s 一次，正常远低于 120/分） */
    if !public_poll_allow(&client_ip_for_limit(addr, &headers)) {
        return Err(ApiError::with_status(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "尝试太频繁，请稍后再试",
        ));
    }
    let state = q.state.unwrap_or_default();
    let state = state.trim();
    if state.is_empty() || state.len() > 128 {
        return Err(ApiError::bad_request("缺少授权流水号 state"));
    }
    ensure_register_open(&st).await?;
    let Some(wb) = st.wb.get() else {
        return Err(ApiError::bad_request(
            "服务端还没有配置 WorkBuddy 网关，不能查询授权状态",
        ));
    };
    let v = wb
        .login_poll(state)
        .await
        .map_err(|e| {
            // 网关返回的 e 可能带上游 HTTP 状态/报文细节：细节留日志，客户端给通用话术
            tracing::warn!(error = %e, "查询 WorkBuddy 授权状态失败");
            ApiError::bad_request("查询授权状态失败，请稍后再试或联系管理员")
        })?;
    let status = v
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("unknown")
        .to_string();
    let message = v
        .get("message")
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_string();
    let nickname = v
        .pointer("/account/nickname")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    Ok(Json(
        json!({ "status": status, "message": message, "nickname": nickname }),
    ))
}

/// POST /api/public/my/register/oauth/cancel {state}：用户关掉弹窗时清理面板上的授权状态
pub async fn my_register_oauth_cancel(
    State(st): State<ArcState>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let state = body
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if PublicSettings::load(&st).await.my_register {
        if let Some(wb) = st.wb.get() {
            if !state.is_empty() && state.len() <= 128 {
                wb.login_cancel(&state).await;
            }
        }
    }
    Ok(Json(json!({ "ok": true })))
}

/// GET /api/public/my/register/check-name?q=：注册页实时校验主播姓名（免 token）。
/// 返回 `{ok, exists, bound, name, team}`；查不到 → `{ok:false, exists:false, error}`。
/// 只在「开放手机号注册」开着时可用（关着时不泄露台账姓名）。
///
/// 2026-10-12 安全审计：**不再下发 personId**（免 token 接口泄露内部 id，会被拿去
/// 自报绑定冒领身份）；改回 exists + bound（该主播是否已有绑定账号），前端据此提示
/// 「已有人绑定，请联系管理员」。
pub async fn my_register_check_name(
    State(st): State<ArcState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<NameQuery>,
) -> ApiResult {
    ensure_register_open(&st).await?;
    /* 审计 2026-10-11：姓名校验是免 token 的精确匹配查询，不限流就能被脚本拿来做
    「某个艺名在不在册」的枚举，和注册接口共用同一档限流。 */
    if !public_register_allow(&client_ip_for_limit(addr, &headers)) {
        return Err(ApiError::with_status(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "尝试太频繁，请稍后再试",
        ));
    }
    let key = q.q.clone().unwrap_or_default();
    let key = key.trim();
    if key.is_empty() {
        return Ok(Json(
            json!({ "ok": false, "exists": false, "bound": false, "error": "请输入主播姓名" }),
        ));
    }
    match crate::store::anchor_lookup_by_name(&st, key).await {
        Ok(Some((pid, name, team))) => {
            let bound = crate::store::my_account_bound_for_person(&st, pid, &name).await;
            Ok(Json(json!({
                "ok": true, "exists": true, "bound": bound, "name": name,
                "team": if team == "male" || team == "female" { team } else { String::new() },
            })))
        }
        Ok(None) => Ok(Json(json!({
            "ok": false, "exists": false, "bound": false,
            "error": format!("主播库里没有「{key}」这个姓名，请核对艺名"),
        }))),
        Err(e) => Ok(Json(
            json!({ "ok": false, "exists": false, "bound": false, "error": e }),
        )),
    }
}

/// POST /api/public/my/register {phone, code, name, team?, wbState?}
/// **手机号 = 账号，验证码 = 密码，name = 主播姓名（必须在台账里）**。
/// 2026-10-12 审计修订：注册**不再立即绑定**（自报姓名直接 bound=1 等于无审核冒领）。
/// 注册成功 = 账号落库（手机号占位名、bound=0）+ 自动生成 pending 绑定申请，
/// 管理员审核通过后才真正绑定。响应字段保持向后兼容（ok/token/name/team/personId/
/// bound/needBind 都在），新增 bindStatus:"pending"。
pub async fn my_register(
    State(st): State<ArcState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    ensure_register_open(&st).await?;
    if !public_register_allow(&client_ip_for_limit(addr, &headers)) {
        return Err(ApiError::with_status(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "尝试太频繁，请稍后再试",
        ));
    }
    let phone = body
        .get("phone")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let code = body
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let anchor = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    /* wb_state 落库列是 VARCHAR(64)：超长会写成 500「写入失败」，这里先截断到 64 字符
    （审计 2026-10-11；正常网关 state 是 36 位 UUID，截断只影响伪造/异常入参） */
    let wb_state: String = body
        .get("wbState")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .chars()
        .take(64)
        .collect();

    let (account_name, person_name, _person_id, anchor_team) =
        crate::store::my_account_register(&st, phone, code, anchor, &wb_state)
            .await
            .map_err(|e| {
                /* 审计 2026-10-11：注册失败都是用户可修复的问题（姓名不在台账 / 已注册 /
                格式不对）→ 400；只有底层库故障才留 500。 */
                if e.starts_with("查主播失败")
                    || e.starts_with("查重失败")
                    || e.starts_with("写入失败")
                {
                    // 底层库故障（500 走通用文案）；原文细节留日志
                    tracing::error!(error = %e, "注册账号写入失败");
                    ApiError::from(anyhow::anyhow!(e))
                } else {
                    ApiError::bad_request(e)
                }
            })?;
    /* 团别以台账为准（主播姓名决定男女团），前端传的 team 只作兜底 */
    let team = if anchor_team == "male" || anchor_team == "female" {
        anchor_team
    } else {
        normalize_team(body.get("team").and_then(|v| v.as_str()).unwrap_or(""))
    };
    /* 审核通过前按「未绑定」签发 token（personId=0）：能登录、能看审核进度，
    但拿不到任何个人数据。绑定生效后旧 token 会被吊销，需重新登录拿新身份。 */
    let token = my_token_put(0, team.clone(), account_name.clone());
    tracing::info!(phone = %mask_phone(phone), anchor = %person_name, team = %team, "展示端手机号注册成功（绑定申请待审核）");
    Ok(Json(json!({
        "ok": true,
        "token": token,
        // name 保持回主播姓名（展示用，向后兼容）；账号登录名在审核通过前是手机号占位，
        // 前端要展示登录名请用 who 接口的 phone 字段
        "name": person_name,
        "team": team,
        // 兼容字段：未绑定恒 0（2026-10-12 起内部 personId 不下发）
        "personId": 0,
        "bound": false,
        "needBind": true,
        // 新增：绑定申请已自动生成，等管理员审核（前端轮询 who 的 bindRequest）
        "bindStatus": "pending",
    })))
}

/// POST /api/public/my/password/reset {phone}
///
/// 2026-10-14 审计修复（P0）：原实现是**任意账号接管** —— 这个端点挂在 /api/public/
/// 下、免管理员鉴权，handler 内也不要任何身份，只要知道（或猜到）一个主播的手机号，
/// 就能把该账号的密码重置成「手机号后四位」，并且接口把新密码明文回显。
/// 手机号在公会内部半公开、且重置结果可自行推算，等于忘记密码流程完全没有身份绑定。
///
/// 现在三件事一起改：
/// 1. 受注册开关约束（`my_register`）—— 与注册同口径，运营关掉注册即一并关闭重置；
///    这是唯一的运维级总闸，防止端点在无人配置时被滥用。
/// 2. 新密码改用 CSPRNG 随机串（不再是手机号后四位），攻击者无法自行推算。
///    （展示端 st_page.html 早已按「后端下发随机临时密码」写文案与回填，本次是对齐后端。）
/// 3. 日志不再打账号名 —— 未绑定 / 自助注册账号的 name 就是手机号本身，
///    `mask_phone` 的脱敏被 `accounts = ?names` 绕过，只打数量。
/// 新密码只在本次响应里下发一次，请当场转达；重置成功后该账号全部 my token 吊销。
pub async fn my_password_reset(
    State(st): State<ArcState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    ensure_register_open(&st).await?;
    if !public_register_allow(&client_ip_for_limit(addr, &headers)) {
        return Err(ApiError::with_status(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "尝试太频繁，请稍后再试",
        ));
    }
    let phone = body
        .get("phone")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (password, names) = crate::store::my_account_reset_password_random(&st, phone)
        .await
        .map_err(|e| {
            /* 与注册同口径：业务拒绝 400，库层故障 500（审计 2026-10-11） */
            if e.starts_with("查询账号失败") || e.starts_with("重置失败") {
                // 底层库故障（500 走通用文案）；原文细节留日志
                tracing::error!(error = %e, "重置展示端账号密码失败");
                ApiError::from(anyhow::anyhow!(e))
            } else {
                ApiError::bad_request(e)
            }
        })?;
    // 改密后旧登录态必须立刻失效（审计 2026-10-12：内存 token 原本无撤销手段）
    for n in &names {
        my_tokens_revoke(n);
    }
    tracing::info!(
        phone = %mask_phone(phone),
        accounts = names.len(),
        "展示端账号密码已重置为随机临时密码（旧 token 已吊销）"
    );
    Ok(Json(json!({ "ok": true, "password": password })))
}

#[derive(Deserialize)]
pub struct BindSearchQuery {
    pub q: Option<String>,
}

/// GET /api/public/my/bind/search?q=（token 走 X-My-Token 头）：绑定页搜主播
/// （全台账，注册账号还没有团别）。
/// 2026-10-12 审计：**不再下发 personId**（模糊搜全台账 + 内部 id = 冒领绑定的弹药）；
/// 绑定改审核制后按姓名提交，前端其实不再需要这个 id（可考虑停用本接口改用 check-name）。
pub async fn my_bind_search(
    State(st): State<ArcState>,
    Query(q): Query<BindSearchQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let Some(token) = header_my_token(&headers) else {
        return Err(ApiError::auth("登录已过期，请重新登录"));
    };
    if my_token_take(&token).is_none() {
        return Err(ApiError::auth("登录已过期，请重新登录"));
    }
    let items = crate::store::all_anchor_search(&st, &sid(&st), q.q.as_deref().unwrap_or(""), 20)
        .await
        .into_iter()
        .map(|mut a| {
            if let Some(o) = a.as_object_mut() {
                o.remove("personId");
            }
            a
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items })))
}

/// POST /api/public/my/bind {token, name}：提交主播绑定申请（**2026-10-12 起改管理员审核**）。
///
/// 旧口径「自报 personId 直接绑」被废（安全审计：check-name / bind/search 都泄露过
/// personId，任何人拿别人的 id 就能冒领身份）。现在只收姓名，服务端精确查台账
/// （排除已软删/已离团）后落 my_bind_requests(pending)，管理员审核通过才真正绑定。
/// 兼容期：老前端传的 personId 一律忽略，只按 name 处理；name 缺失 → 400。
///
/// 响应：{ok:true, status:"pending", already?, personName, team}；
/// 已有 pending → already:true（幂等）；账号已绑定 / 主播已被他人绑定 → 409。
/// 返回 Response 而不是 ApiResult：409 需要带 code 字段（ApiError 错误体只有 {error}）。
pub async fn my_bind(State(st): State<ArcState>, axum::Json(body): axum::Json<Value>) -> Response {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let Some((_, _, token_name)) = my_token_take(token.trim()) else {
        return ApiError::auth("登录已过期，请重新登录").into_response();
    };
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default();
    if name.is_empty() {
        return ApiError::bad_request("请选择要绑定的主播").into_response();
    }
    let Some(info) = crate::store::my_account_info(&st, &token_name).await else {
        return ApiError::auth("账号不存在，请重新登录").into_response();
    };
    if info.bound || info.is_admin {
        return (
            axum::http::StatusCode::CONFLICT,
            Json(json!({
                "error": "该账号已绑定主播，不能自行更改（需要改绑请联系管理员）",
                "code": "already_bound",
            })),
        )
            .into_response();
    }
    // 服务端按姓名精确查台账；内部 personId 不出服务端（不随响应下发）
    let (person_id, person_name, team) = match crate::store::anchor_lookup_by_name(&st, name).await
    {
        Ok(Some(v)) => v,
        Ok(None) => {
            return ApiError::bad_request(format!("主播库里没有「{name}」这个姓名，请核对艺名"))
                .into_response()
        }
        Err(e) => return ApiError::bad_request(e).into_response(),
    };
    if crate::store::my_account_bound_for_person(&st, person_id, &person_name).await {
        return (
            axum::http::StatusCode::CONFLICT,
            Json(json!({
                "error": "该主播已存在绑定的账号，如有疑问请联系管理员",
                "code": "person_bound",
            })),
        )
            .into_response();
    }
    match crate::store::my_bind_request_submit(&st, &token_name, person_id, &person_name, &team)
        .await
    {
        Ok(already) => {
            if !already {
                tracing::info!(account = %token_name, anchor = %person_name, "展示端绑定申请已提交（待审核）");
                crate::events::emit(
                    crate::events::SCOPE_ROSTER,
                    format!(
                        "绑定申请：账号「{token_name}」申请绑定主播「{person_name}」（待审核）"
                    ),
                );
            }
            Json(json!({
                "ok": true, "status": "pending", "already": already,
                "personName": person_name, "team": team,
            }))
            .into_response()
        }
        Err(e) => ApiError::bad_request(e).into_response(),
    }
}

/// 团别白名单：male / female（兼容 m / f），其它一律空串（老链接/异常入参）
pub(super) fn normalize_team(v: &str) -> String {
    match v.trim().to_ascii_lowercase().as_str() {
        "male" | "m" => "male".to_string(),
        "female" | "f" => "female".to_string(),
        _ => String::new(),
    }
}

/// 「我的」系列 GET 接口的查询参数（2026-10-12 起 token 只走 X-My-Token 头，不再收
/// `?token=` —— URL 会进访问日志/浏览器历史/Referer，等价于把登录态写进日志）。
#[derive(Deserialize)]
pub struct MyQuery {
    /// 趋势图取几个业务日（默认 14，上限 30）
    pub days: Option<i64>,
    /// 族谱音浪口径：daily（默认）/ monthly / yearly（2026-10-12）
    pub period: Option<String>,
    /// 族谱基准日 YYYY-MM-DD（monthly/yearly 以它所在的月/年为窗口；非法回落最新业务日）
    pub date: Option<String>,
}

/// 取请求头里的 my token 并解析身份；缺失/失效 → 401（各「我的」GET 接口统一入口）。
pub(super) fn my_token_required(headers: &HeaderMap) -> Result<(u64, String, String), ApiError> {
    let Some(token) = header_my_token(headers) else {
        return Err(ApiError::auth("缺少验证 token，请先验证姓名和密码"));
    };
    my_token_take(&token).ok_or_else(|| ApiError::auth("验证已过期，请重新验证"))
}

/// 异步版（2026-10-12 登录态持久化）：内存 miss 时从 Redis 回填再取。
/// 服务重启清空内存后，24h 内的登录态经此恢复——用户不再被强制二次登录。
pub(super) async fn my_token_required_with_restore(
    st: &crate::state::AppState,
    headers: &HeaderMap,
) -> Result<(u64, String, String), ApiError> {
    let Some(token) = header_my_token(headers) else {
        return Err(ApiError::auth("缺少验证 token，请先验证姓名和密码"));
    };
    if let Some(v) = my_token_take(&token) {
        return Ok(v);
    }
    if let Some(v) = super::admin::my_token_restore(st, &token).await {
        return Ok(v);
    }
    Err(ApiError::auth("验证已过期，请重新登录"))
}
