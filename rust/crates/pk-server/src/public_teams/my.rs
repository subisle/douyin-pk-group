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

    /* 1. 账号校验：手机号+密码 或 姓名+密码（my_accounts 名单，管理员后台开通）
       2026-10-14：失败要说明白是哪一种（密码错 / 账号没开通），不再统一糊成一句。 */
    let account_name = match crate::store::my_account_check_login(&st, &acct, &pass).await {
        Ok(name) => name,
        Err(why) => {
            crate::ipinfo::log_login(&st, &client_ip, &geo, &team, &acct, false).await;
            return Ok(Json(json!({ "ok": false, "error": why.message() })));
        }
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
    /* 可看团别（迁移 024）：与 my_login 同一套口径，Inherit 时行为一字不变。 */
    let view_team = crate::store::my_account_view_team(&st, &account_name).await;
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
            let person_team = row.try_get::<String, _>("gender").unwrap_or_default();
            let token_team = view_team.token_team(&person_team);
            let token = my_token_put(pid, token_team.clone(), account_name.clone());
            Ok(Json(json!({
                "ok": true, "personId": pid, "name": account_name, "token": token, "bound": true,
                // 登录一次就切对团：前端 applyLoginState 按这个字段设 TEAM，避免登录后
                // 还用默认团拉数据 → 401 → 二次弹登录门
                "team": token_team,
                "switchable": view_team.switchable(),
            })))
        }
        None => {
            // 台账里没有同名主播：管理员观看账号（is_admin=1，如 shuai4444）放行，
            // personId=0——榜单/族谱/分组照常看，「我的」个人数据页为空。
            if !crate::store::my_account_is_admin(&st, &account_name).await {
                return Ok(Json(json!({ "ok": false })));
            }
            let token_team = view_team.token_team(&team);
            let token = my_token_put(0, token_team.clone(), account_name.clone());
            Ok(Json(json!({
                "ok": true, "personId": 0, "name": account_name, "token": token, "team": token_team,
                "admin": true, "bound": true,
                "switchable": view_team.switchable(),
            })))
        }
    }
}

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
    /* 2026-10-14 修正（失败只记一次 + 门控）：本块是这条登录路径**唯一**的失败计数点，
    下面的账号校验分支不再重复计数 —— 原先两处各记一次，auth.rs 的「连续失败 5 次
    锁 10 分钟」实际第 3 次失败就触发，注释与行为不符。
    计数还受 `st.auth.enabled()`（= ADMIN_PASSWORD 已配置）门控：没配口令时
    verify_admin_password 恒 false，快捷分支不可能被爆破；不门控的话，正常用户
    输错密码也会被按「来源」级计数（阈值按 IP 不按账号），NAT 后 5 个人就能把整栋楼
    锁 10 分钟。
    真正打快捷分支的爆破（acct==pass 且口令错）一定满足「已配置」，一次不漏全部计数 ——
    防爆破语义不变；账号密码正确时，下面会 login_record_success 清零。 */
    if st.auth.enabled() {
        let now_locked = st.auth.login_record_failure(&login_key);
        if now_locked {
            tracing::warn!(ip = %client_ip, "展示端登录连续失败达到阈值，锁定 10 分钟");
        }
    }

    /* 1. 账号校验（与 verify 同一套 my_accounts）
       2026-10-14：失败原因（密码错 / 账号没开通）下发前端照实显示。 */
    let account_name = match crate::store::my_account_check_login(&st, &acct, &pass).await {
        Ok(name) => name,
        Err(why) => {
            // 失败计数已在上面的门控块记过（每次请求只记一次），这里不再重复计数
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
            return Ok(Json(json!({ "ok": false, "error": why.message() })));
        }
    };
    // 账号密码正确 → 清零失败计数（与 /api/auth/login 同口径：成功即解锁）
    st.auth.login_record_success(&login_key);
    /* 1.5 账号没绑主播：两个团都放行，token 挂 MY_ANY_KEY（登录后强制绑定） */
    /* 可看团别（迁移 024）：一次性取出来给下面三个分支共用。
       Inherit 时完全沿用旧逻辑（token_team 原样返回 base），分权只在显式配了的
       账号上生效 —— 存量 50 多个账号的行为一个字节都不能变。 */
    let view_team = crate::store::my_account_view_team(&st, &account_name).await;
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
            /* view_team 优先级高于绑定关系：钉死女团的账号即使绑着男团主播也只给女团
               （Inherit 时 token_team 原样返回 team，行为与改动前一致）。 */
            let token_team = view_team.token_team(&team);
            let token = my_token_put(pid, token_team.clone(), name.clone());
            Ok(Json(json!({
                "ok": true, "personId": pid, "name": name, "team": token_team,
                "token": token, "bound": true,
                // 前端据此渲染「全团 / 男团 / 女团」切换器（只有全团账号 true）
                "switchable": view_team.switchable(),
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
            /* 观看账号没有绑定主播，base 是空串（旧口径 = 任意团）：
               - Any     → 空串（两团放行 + 可切）
               - Female  → "female"（饭团：只看女团）
               - Inherit → 空串（旧行为不变） */
            let token_team = view_team.token_team("");
            let token = my_token_put(0, token_team.clone(), account_name.clone());
            Ok(Json(json!({
                "ok": true, "personId": 0, "name": account_name, "team": token_team,
                "token": token, "admin": true, "bound": true,
                "switchable": view_team.switchable(),
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

/// 「忘记密码重置」写接口限流：每 IP 每分钟 12 次。
///
/// 这一档原是注册时代的「注册类写接口」档（一次注册本来就要接好几下，所以比登录撞库
/// 单独放宽到 12）；2026-10-14 注册功能移除后，`POST /api/public/my/password/reset`
/// 是它唯一的调用方 —— 档位数值不动，只把名字改成反映当前用途。
pub(super) fn public_password_reset_allow(ip: &str) -> bool {
    PASSWORD_RESET_LIMITER
        .get_or_init(|| VerifyLimiter::new(12))
        .allow(ip)
}

/// POST /api/public/my/password {newPass}（身份**只**认 X-My-Token 头）
///
/// 个人页（已登录）自助**重置密码**：只能改**当前 token 对应的那个账号**的密码 ——
/// 请求体里不接受任何账号名，杜绝「拿自己的 token 改别人的密码」。
///
/// **2026-10-15 起不再校验旧密码**：用户口径是「点『重置密码』直接输两遍新密码」，
/// 展示端对话框（st_page.html）已删掉旧密码输入框、两次输入是否一致由前端自行拦下，
/// 后端只读 `newPass`。请求里若仍带 `oldPass`（旧版前端 / 第三方脚本）
/// **一律忽略、不报错**，保持向后兼容。
/// 注意：身份门槛没有放松 —— 仍然必须有有效的 `X-My-Token`，也只能改自己这个账号；
/// 管理员观看账号依旧不支持自助改密（400）。
///
/// 成功后：吊销该账号全部旧 token（改密后旧登录态必须立刻失效），**再签发一个
/// 同身份的新 token 返回**（person_id 与团别原样保留），避免用户刚改完密码就被
/// 自己触发的吊销踢回登录门；前端用返回的 token 覆盖本地即可。
pub async fn my_password_set(
    State(st): State<ArcState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    /* 复用登录门那一档写接口限流（public_write_allow：每 IP 每分钟 5 次）：
    改密要跑 argon2，且是免 token 时最值得挡的写口之一。 */
    if !public_write_allow(&client_ip_for_limit(addr, &headers)) {
        return Err(ApiError::with_status(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "尝试太频繁，请稍后再试",
        ));
    }
    let Some(token) = header_my_token(&headers) else {
        return Err(ApiError::auth("登录已过期，请重新登录"));
    };
    /* 内存 miss 时从 Redis 回填（与 my_token_required_with_restore 同口径）：
    服务重启后 24h 内的登录态照常可以改密，不必先重新登录。 */
    let mut ident = my_token_take(&token);
    if ident.is_none() {
        ident = super::admin::my_token_restore(&st, &token).await;
    }
    let Some((person_id, team, account_name)) = ident else {
        return Err(ApiError::auth("登录已过期，请重新登录"));
    };
    /* 2026-10-15 起**不再读 oldPass**（旧前端会带上，忽略即可，不报错）。 */
    let new_pass = body
        .get("newPass")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    crate::store::my_account_reset_password_self(&st, &account_name, new_pass)
        .await
        .map_err(|e| {
            /* 2026-10-16 起 store 层用 BIZ:: 前缀分流（store::my_account::BIZ_PREFIX）：
            业务拒绝（密码格式 / 不支持自助改密 / 当前密码不对）→ 400 原样下发；
            库层故障（「查账号失败」「写入失败」打头，sqlx/PG 原文）→ 500 走通用文案，
            不把表名、SQL 片段回传客户端（审计 2026-10-11 / 2026-10-14 P2 的口径）。 */
            if crate::store::my_account::is_biz(&e) {
                ApiError::bad_request(crate::store::my_account::biz_msg(&e).to_string())
            } else {
                tracing::error!(error = %e, "展示端自助改密写入失败");
                ApiError::from(anyhow::anyhow!(e))
            }
        })?;
    // 改密成功 → 该账号旧 token 全部吊销，再给当前用户换发同身份的新 token
    my_tokens_revoke(&account_name);
    let token = my_token_put(person_id, team, account_name);
    // 不打账号名：未绑定账号的 name 就是手机号本身（同 my_password_reset 的口径）
    tracing::info!("展示端账号已自助重置密码（旧 token 已吊销并换发新 token）");
    Ok(Json(json!({ "ok": true, "token": token })))
}

/// POST /api/public/my/password/reset {phone}
///
/// 2026-10-14 审计修复（P0）：原实现是**任意账号接管** —— 这个端点挂在 /api/public/
/// 下、免管理员鉴权，handler 内也不要任何身份，只要知道（或猜到）一个主播的手机号，
/// 就能把该账号的密码重置成「手机号后四位」，并且接口把新密码明文回显。
/// 手机号在公会内部半公开、且重置结果可自行推算，等于忘记密码流程完全没有身份绑定。
///
/// 现在三件事一起改：
/// 1. 只保留 `public_password_reset_allow` 限流（每 IP 每分钟 12 次）挡脚本刷。
///    （2026-10-14「彻底移除注册功能」后注册开关已不存在，原先这里那道
///    「注册开关」运维总闸随之删掉 —— 忘记密码是未登录场景的唯一入口，
///    不能跟着注册一起消失。）
/// 2. 新密码改用 CSPRNG 随机串（不再是手机号后四位），攻击者无法自行推算。
///    （展示端 st_page.html 早已按「后端下发随机临时密码」写文案与回填，本次是对齐后端。）
/// 3. 日志不再打账号名 —— 未绑定 / 自助注册账号的 name 就是手机号本身，
///    `mask_phone` 的脱敏被 `accounts = ?names` 绕过，只打数量。
///
/// 新密码只在本次响应里下发一次，请当场转达；重置成功后该账号全部 my token 吊销。
pub async fn my_password_reset(
    State(st): State<ArcState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    if !public_password_reset_allow(&client_ip_for_limit(addr, &headers)) {
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
            /* 2026-10-16 起与自助改密同口径：按 store 层 BIZ:: 前缀分流 ——
            业务拒绝（「请输入手机号」「该手机号未绑定账号」）→ 400 原样下发；
            库层故障（sqlx/PG 原文）→ 500 走通用文案，原文细节留日志（审计 2026-10-11）。 */
            if crate::store::my_account::is_biz(&e) {
                ApiError::bad_request(crate::store::my_account::biz_msg(&e).to_string())
            } else {
                tracing::error!(error = %e, "重置展示端账号密码失败");
                ApiError::from(anyhow::anyhow!(e))
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

/* 2026-10-16：`my_bind_search`（GET /api/public/my/bind/search）与 `my_bind`
   （POST /api/public/my/bind）随「抖音号审计」整页下线一并删除 —— 用户口径已改为
   「账号 → 主播的绑定只由管理员在后台做」，展示端在 2026-10-14 就停止调用这两个接口
   （st_page.html 只剩一张「联系管理员」提示卡），留着就是可被脚本调用的死接口。
   `my_bind_requests` 表与迁移保留不删，历史申请记录仍在库里。 */

/* 2026-10-14 注册功能移除：`normalize_team`（注册页团别白名单）随注册一起删除 ——
   它已无任何生产调用点，仅剩 pages.rs 的 register_route_tests 在断言，那个测试模块
   也一并删掉了（见 public_teams/pages.rs）。 */

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
