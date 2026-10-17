//! public_teams::my —— 自 public_teams.rs 按业务域拆分（对外语义不变）。

use super::*;

/* ------------------ 登录 / 绑定响应同形字段与错误码（契约 §3） ------------------
 *
 * 2026-10-18 用户拍板后的展示端身份模型：登录（`/my/login`、`/verify`）、认领
 * （`/my/claim`）、首次补全（`/my/profile/bind`）四个入口的**成功响应必须同形**，
 * 前端只认一套字段；`who` 复用同一组状态字段。状态判定本身全部在 store
 * （`my_account_auth_flags`），handler 只做投影 —— 手机号在这一层就脱敏，
 * 任何展示端响应都不下发明文手机号 / 明文邮箱。
 */

/// 登录成功响应里的身份标志（store `MyAuthFlags` 在 handler 侧的投影）。
pub(super) struct AuthExtra {
    pub(super) need_profile: bool,
    pub(super) need_qq: bool,
    pub(super) must_change_pass: bool,
    /// 是否已绑 QQ 邮箱（管理端列表 / 单条要显示绑定状态）
    pub(super) qq_bound: bool,
    /// 已脱敏手机号（服务端唯一打码点）
    pub(super) phone: String,
    pub(super) email_mask: String,
}

impl AuthExtra {
    fn empty() -> Self {
        Self {
            need_profile: false,
            need_qq: false,
            must_change_pass: false,
            qq_bound: false,
            phone: String::new(),
            email_mask: String::new(),
        }
    }
}

/// 读账号状态；账号行读不到（如管理员快捷口令身份 `admin-gate`）时回全 false / 空串，
/// 保证响应结构不因账号类型而缺字段。
pub(super) async fn auth_extra(st: &ArcState, name: &str) -> AuthExtra {
    match crate::store::my_account_auth_flags(st, name).await {
        Some(f) => AuthExtra {
            need_profile: f.need_profile,
            need_qq: f.need_qq,
            must_change_pass: f.must_change_pass,
            qq_bound: f.qq_bound,
            phone: mask_phone(&f.phone),
            email_mask: f.email_mask,
        },
        None => AuthExtra::empty(),
    }
}

/// 登录成功响应（契约 §3.1，登录 / 认领 / 首次补全共用）。
/// 调用方按需追加 `switchable` / `admin` 等既有字段。
async fn login_ok(st: &ArcState, person_id: u64, name: &str, team: &str, token: &str) -> Value {
    let x = auth_extra(st, name).await;
    json!({
        "ok": true,
        "token": token,
        "personId": person_id,
        "name": name,
        "team": team,
        "bound": true,
        "needBind": false,
        "needProfile": x.need_profile,
        "needQq": x.need_qq,
        "mustChangePass": x.must_change_pass,
        "phone": x.phone,
        "emailMask": x.email_mask,
    })
}

/// 登录失败响应（契约 §3.1：仍走 HTTP 200 + `ok:false`，前端按 `code` 分支）。
fn login_fail(fail: crate::store::my_account::LoginFail) -> Value {
    use crate::store::my_account::LoginFail as F;
    let code = match fail {
        F::BadPassword => "BAD_PASSWORD",
        F::NoAccount => "NO_ACCOUNT",
        F::TempExpired => "TEMP_EXPIRED",
    };
    json!({ "ok": false, "code": code, "error": fail.message() })
}

/// 登录失败响应 + 「认领账号」提示（契约 §3.1）：仅 `NO_ACCOUNT` 且台账里恰好有
/// 一个同名在职主播、该艺名尚未被任何人认领/开号时才带 `canClaim/claimName`。
async fn login_fail_with_claim(
    st: &ArcState,
    fail: crate::store::my_account::LoginFail,
    acct: &str,
) -> Value {
    let mut v = login_fail(fail);
    if matches!(fail, crate::store::my_account::LoginFail::NoAccount)
        && crate::store::my_account_can_claim(st, acct).await
    {
        v["canClaim"] = json!(true);
        v["claimName"] = json!(acct);
    }
    v
}

/// 展示端结构化错误体：HTTP 状态 + `{ok:false, code, error}`。
/// `code` 取值对齐 docs/26-…/02-登录绑定与重置流程.md §3.2/§4.2/§5.6 的错误码表；
/// `error` 是可直接展示的中文文案（store 的业务拒绝原文）。
fn api_err(status: axum::http::StatusCode, code: &str, msg: impl Into<String>) -> ApiError {
    ApiError::with_json(
        status,
        json!({ "ok": false, "code": code, "error": msg.into() }),
    )
}

/// store `Result<_, String>` 分流：`BIZ::` 业务拒绝 → 400/403（带契约错误码）；
/// 无前缀 = 库层故障 → 500 通用文案，原文只进日志（2026-10-14 审计 P2 口径）。
fn store_err(e: String, code_of: fn(&str) -> &'static str) -> ApiError {
    if crate::store::my_account::is_biz(&e) {
        let msg = crate::store::my_account::biz_msg(&e).to_string();
        let code = code_of(&msg);
        let status = if code == "NEED_PROFILE" {
            axum::http::StatusCode::FORBIDDEN
        } else {
            axum::http::StatusCode::BAD_REQUEST
        };
        api_err(status, code, msg)
    } else {
        tracing::error!(error = %e, "展示端账号写库失败");
        ApiError::from(anyhow::anyhow!(e))
    }
}

/// 绑定类写口的 BIZ 文案 → 错误码（契约 §3.3 / §3.4）。文案常量由 store 冻结导出，
/// 精确比较避免关键词误判；`validate_my_phone` 的格式文案只在关键词层兜底
/// （它沿用旧文案、未单独导出常量）；未命中的新文案回落 `BAD_REQUEST`（`error` 仍可读）。
fn bind_code(msg: &str) -> &'static str {
    use crate::store as s;
    if msg == s::MSG_PROFILE_ALREADY_DONE {
        "PROFILE_ALREADY_DONE"
    } else if msg == s::MSG_NEED_PROFILE {
        "NEED_PROFILE"
    } else if msg == s::MSG_PHONE_REQUIRED || msg.contains("手机号格式") {
        "INVALID_PHONE"
    } else if msg == s::MSG_PHONE_TAKEN {
        "PHONE_TAKEN"
    } else if msg == s::MSG_INVALID_QQ {
        "INVALID_QQ"
    } else if msg == s::MSG_QQ_TAKEN {
        "QQ_TAKEN"
    } else if msg == s::MSG_PASS_INVALID || msg == s::MSG_PASS_BLANK || msg == s::MSG_PASS_CTRL {
        "INVALID_PASS"
    } else {
        "BAD_REQUEST"
    }
}

/// 认领写口的 BIZ 文案 → 错误码（契约 §3.2；前端主要读 `error`，`code` 只做分支）。
///
/// 认领失败文案是 store 新增的一批（尚未像绑定类那样冻结导出常量），这里按**语义
/// 关键词**映射，文案小改不会让前端分支失效；完全认不出时回落 `CLAIM_FAILED`。
fn claim_code(msg: &str) -> &'static str {
    use crate::store as s;
    if msg == s::MSG_PASS_INVALID || msg == s::MSG_PASS_BLANK || msg == s::MSG_PASS_CTRL {
        "INVALID_PASS"
    } else if msg.contains("已存在") {
        // 并发撞主键 → 「账号已存在，请直接登录」（契约 §3.2）
        "ACCOUNT_EXISTS"
    } else if msg.contains("已被认领") {
        "ALREADY_CLAIMED"
    } else if msg.contains("已有登录账号") {
        "PERSON_TAKEN"
    } else if msg.contains("离团") {
        "NAME_LEFT"
    } else if msg.contains("多个") {
        "NAME_DUPLICATED"
    } else if msg.contains("性别") {
        "BAD_GENDER"
    } else if msg.contains("台账里没有") {
        "NAME_NOT_FOUND"
    } else {
        "CLAIM_FAILED"
    }
}

/// 重置密码成功响应（契约 §3.5）。被同号窗口闸抑制时用同形包、`emailMask` 空串。
fn reset_ok_json(email_mask: &str) -> Value {
    json!({
        "ok": true, "sent": true, "channel": "qq-mail",
        "expiresInMinutes": 15, "emailMask": email_mask,
    })
}

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
    /* 2026-10-17 用户口径「去除登录限速」：原先这里有两道闸 ——
    ① public_write_allow 每 IP 每分钟 5 次滑窗（VERIFY_LIMITER）；
    ② LoginLimiter 连续失败 5 次锁 10 分钟（key = myverify:<IP>）。
    两道已全部移除：登录（含失败）不限速、不锁定，输错直接走账号校验返回
    「密码错误 / 账号还没开通」。地区门禁与 access_logs 登录日志保留不动。 */
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
    /* 密码**不 trim**（2026-10-18 用户口径：支持中英文/特殊字符/任何符号，
    首尾空格也是密码的一部分）；只挡空串。账号标识本身 trim。 */
    if acct.is_empty() || pass.is_empty() {
        // 契约 §3.1 的本地可判定错误：400 + code（docs 26-02 §2.2 的同名错误码）
        return Err(api_err(
            axum::http::StatusCode::BAD_REQUEST,
            "INVALID_INPUT",
            "请输入账号和密码",
        ));
    }

    /* 1. 账号校验：手机号+密码 或 姓名+密码（my_accounts 名单，管理员后台开通）
    2026-10-14：失败要说明白是哪一种（密码错 / 账号没开通），不再统一糊成一句。 */
    /* 账号校验返回 `LoginOutcome`（契约 §2.4）：临时密码命中时同样成功，`temp_used`
    只影响日志，`mustChangePass` 由下面的身份标志统一下发。 */
    let account_name = match crate::store::my_account_check_login(&st, &acct, &pass).await {
        Ok(outcome) => outcome.name,
        Err(why) => {
            crate::ipinfo::log_login(&st, &client_ip, &geo, &team, &acct, false).await;
            // 2026-10-17 用户口径「去除登录限速」：失败不计数、不锁定
            // 2026-10-18：失败回 `{ok:false, code, error}`；NO_ACCOUNT 且可认领时带 canClaim
            return Ok(Json(login_fail_with_claim(&st, why, &acct).await));
        }
    };
    crate::ipinfo::log_login(&st, &client_ip, &geo, &team, &account_name, true).await;

    /* 1.5 手机号自助注册还没绑主播的账号：先放进来（personId=0），登录后「我的」页
    会强制让用户绑定主播——不绑定看不到个人数据，也无从套用别人的姓名。 */
    if let Some(info) = crate::store::my_account_info(&st, &account_name).await {
        if !info.bound && !info.is_admin {
            let token = my_token_put(0, team.clone(), account_name.clone());
            /* B1 未绑主播账号（旁支，旧账号形态）：保留 needBind 语义，
            状态字段照常下发（契约 §2.4 的「未绑主播为账号名/占位」口径）。 */
            let x = auth_extra(&st, &account_name).await;
            return Ok(Json(json!({
                "ok": true,
                "personId": 0,
                "name": account_name,
                "token": token,
                "bound": false,
                "needBind": true,
                "needProfile": x.need_profile,
                "needQq": x.need_qq,
                "mustChangePass": x.must_change_pass,
                "phone": x.phone,
                "emailMask": x.email_mask,
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
            /* 与 /my/login 同形（契约 §3.1）：verify 额外保留 switchable。
            登录一次就切对团：前端 applyLoginState 按 team 设 TEAM，避免登录后
            还用默认团拉数据 → 401 → 二次弹登录门。 */
            let mut v = login_ok(&st, pid, &account_name, &token_team, &token).await;
            v["switchable"] = json!(view_team.switchable());
            Ok(Json(v))
        }
        None => {
            // 台账里没有同名主播：管理员观看账号（my_accounts.is_admin=1，如内置管理员账号）放行，
            // personId=0——榜单/族谱/分组照常看，「我的」个人数据页为空。
            if !crate::store::my_account_is_admin(&st, &account_name).await {
                return Ok(Json(json!({
                    "ok": false,
                    "code": "NO_ACCOUNT",
                    "error": "账号还没绑定主播，请联系管理员",
                })));
            }
            let token_team = view_team.token_team(&team);
            let token = my_token_put(0, token_team.clone(), account_name.clone());
            let mut v = login_ok(&st, 0, &account_name, &token_team, &token).await;
            v["admin"] = json!(true);
            v["switchable"] = json!(view_team.switchable());
            Ok(Json(v))
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
    /* 2026-10-17 用户口径「去除登录限速」：原先这里有两道闸 ——
    ① public_write_allow 每 IP 每分钟 5 次滑窗（VERIFY_LIMITER）；
    ② LoginLimiter 连续失败 5 次锁 10 分钟（key = mylogin:<IP>，2026-10-14 审计 P1
      接入，2026-10-17 审计 4-P1-1 又给 verify 加了同款）。
    两道已全部移除：登录（含失败与管理员快捷口令爆破尝试）不限速、不锁定。
    地区门禁与 access_logs 登录日志保留不动 —— 失败尝试仍全部进访客日志。 */
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
    /* 密码**不 trim**（2026-10-18 用户口径：支持中英文/特殊字符/任何符号，
    首尾空格也是密码的一部分）；只挡空串。账号标识本身 trim。 */
    if acct.is_empty() || pass.is_empty() {
        // 契约 §3.1 的本地可判定错误：400 + code（docs 26-02 §2.2 的同名错误码）
        return Err(api_err(
            axum::http::StatusCode::BAD_REQUEST,
            "INVALID_INPUT",
            "请输入账号和密码",
        ));
    }

    /* 0. 管理员快捷口令（账号、密码填同一个管理员口令即可登录展示页）：
    限流之后、my_accounts 账号校验之前判断——这样管理员不用在 my_accounts 里
    专门留一行账号。口令读自运行时配置（ADMIN_PASSWORD），改 .env 重启即生效；
    未配置口令时 verify_admin_password 恒 false（fail-closed）。
    2026-10-12 安全审计 M1：account 字段绝不能落口令本体——access_logs 运营可查
    （/api/admin/visits 支持关键字搜索）且随备份 dump 外流。这里记固定占位符。 */
    let admin_shortcut = acct == pass && st.auth.verify_admin_password(&acct);
    if admin_shortcut {
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
        /* 2026-10-17 审计 4-P2-B：token 身份名固定占位 `admin-gate`，**绝不能再用
        ADMIN_PASSWORD 本体** —— 身份名会进 Redis 键名/键值（mytokname:*）、经
        /my/who 回显、被前端写进 localStorage 与水印（原实现正是这样把口令扩散出去的）。
        响应里的 "name" 只给展示用的中性文案「管理员」，前端水印/localStorage 不再落敏感值。 */
        let token = my_token_put(0, String::new(), "admin-gate".to_string());
        tracing::info!(ip = %client_ip, "展示端管理员口令登录成功（账号=密码=管理员口令）");
        /* 与账号登录同形（契约 §3.1）：管理员不参与 needProfile/needQq 引导，
        身份标志由 auth_extra 按 `admin-gate` 查不到账号行 → 全 false/空串。 */
        let mut v = login_ok(&st, 0, "admin-gate", "", &token).await;
        v["name"] = json!("管理员");
        v["admin"] = json!(true);
        return Ok(Json(v));
    }
    /* 2026-10-17 用户口径「去除登录限速」：原先这里对「已配置口令 + 非快捷登录」
    记一次连续失败（5 次锁 10 分钟），现已整条移除 —— 不再计数、不再锁定，
    每次失败尝试照常进 access_logs（上面账号校验分支里的 log_login_path）。 */

    /* 1. 账号校验（与 verify 同一套 my_accounts）
    2026-10-14：失败原因（密码错 / 账号没开通）下发前端照实显示。 */
    let account_name = match crate::store::my_account_check_login(&st, &acct, &pass).await {
        Ok(outcome) => outcome.name,
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
            // 2026-10-18：失败回 {ok:false, code, error}；NO_ACCOUNT 且可认领时带 canClaim
            return Ok(Json(login_fail_with_claim(&st, why, &acct).await));
        }
    };
    // 账号密码正确（2026-10-17 用户口径：登录限速已去除，无失败计数可清）
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
            /* B1 未绑主播账号：保留 needBind 语义，状态字段照常下发（契约 §2.4）。 */
            let x = auth_extra(&st, &account_name).await;
            return Ok(Json(json!({
                "ok": true,
                "personId": 0,
                "name": account_name,
                "token": token,
                "bound": false,
                "needBind": true,
                "needProfile": x.need_profile,
                "needQq": x.need_qq,
                "mustChangePass": x.must_change_pass,
                "phone": x.phone,
                "emailMask": x.email_mask,
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
            /* 与 verify 同形（契约 §3.1）；前端据此渲染「全团 / 男团 / 女团」切换器。 */
            let mut v = login_ok(&st, pid, &name, &token_team, &token).await;
            v["switchable"] = json!(view_team.switchable());
            Ok(Json(v))
        }
        None => {
            /* 3. 管理员观看账号：两个团都放行（token 挂 MY_ANY_KEY），「我的」数据页为空 */
            if !crate::store::my_account_is_admin(&st, &account_name).await {
                return Ok(Json(json!({
                    "ok": false,
                    "code": "NO_ACCOUNT",
                    "error": "账号还没绑定主播，请联系管理员",
                })));
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
            let mut v = login_ok(&st, 0, &account_name, &token_team, &token).await;
            v["admin"] = json!(true);
            v["switchable"] = json!(view_team.switchable());
            Ok(Json(v))
        }
    }
}

/// POST /api/public/my/claim {name, pass}（契约 §3.2）。
///
/// 2026-10-18 用户拍板：**凭艺名直接认领开号** —— 台账里有、但 `my_accounts` 还没
/// 开号（或同一主播没被占用）的在职主播，可以用艺名 + 自设密码认领，认领即建号
/// （`store::my_account_claim` 单事务 INSERT：bound=1、phone=''、source='claim'）。
/// 之后与登录同形返回（`needProfile:true` → 前端进首次补全强制引导）。
///
/// 失败一律 400 + BIZ 文案：艺名不存在 / 艺名重复 / 已离团 / 已被认领 / 主播已有账号 /
/// 性别不合法 / 密码不合法 / 并发撞主键（「账号已存在，请直接登录」）。
///
/// 限流：**独立**一档 5 次/分/IP（`CLAIM_LIMITER`）。认领无身份校验，限流防批量抢注；
/// 它与登录限速无关（登录链路 2026-10-17 起仍不限速）。
pub async fn my_claim(
    State(st): State<ArcState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let client_ip = client_ip_for_limit(addr, &headers);
    if !claim_allow(&client_ip) {
        return Err(api_err(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
            "尝试太频繁，请稍后再试",
        ));
    }
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    /* 密码**不 trim**（任意符号/空格都是密码的一部分），只挡空串。 */
    let pass = body.get("pass").and_then(Value::as_str).unwrap_or_default();
    if name.is_empty() || pass.is_empty() {
        return Err(api_err(
            axum::http::StatusCode::BAD_REQUEST,
            "INVALID_INPUT",
            "请输入艺名和密码",
        ));
    }
    let claimed = crate::store::my_account_claim(&st, name, pass)
        .await
        .map_err(|e| store_err(e, claim_code))?;
    let person_id = claimed.person_id.max(0) as u64;
    let token = my_token_put(person_id, claimed.team.clone(), claimed.name.clone());
    let view_team = crate::store::my_account_view_team(&st, &claimed.name).await;
    let mut v = login_ok(&st, person_id, &claimed.name, &claimed.team, &token).await;
    /* needProfile 恒 true（新账号 phone=''，登录后必须先走「手机号（必填）+ QQ（选填）+
    新密码」的首次补全）；needQq 与 `my_account_auth_flags` 一致 —— S1 阶段
    needProfile=true 时 needQq=false（§2.2），补全后再按真实绑定态下发。
    前端以 needProfile 优先，不受影响。 */
    v["needProfile"] = json!(true);
    // 与登录同形：带上切换器字段（新账号一般是 Inherit → false）
    v["switchable"] = json!(view_team.switchable());
    tracing::info!(
        ip = %client_ip,
        artist = %name,
        person_id,
        team = %claimed.team,
        "展示端账号凭艺名认领成功（单事务建号，待首次补全）"
    );
    Ok(Json(v))
}

/// POST /api/public/my/profile/bind {phone, qq?, newPass?}（契约 §3.3）。
///
/// S1 首次补全：身份**只**认 `X-My-Token`（请求体里的账号名一律忽略）。只允许
/// `phone=''` 的账号调用一次 —— store 的 `WHERE name=? AND phone=''` 守卫保证，
/// 重复调用回 400 `PROFILE_ALREADY_DONE`。`qq` / `newPass` 选填（空 = 暂不绑 / 沿用
/// 当前密码）。成功后吊销该账号全部旧 token 并**换发同身份新 token**：否则用户刚补全
/// 就被自己触发的吊销踢回登录门（与改密同一纪律）。
pub async fn my_profile_bind(
    State(st): State<ArcState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    /* 写口限流：每 IP 每分钟 5 次（复用 `public_write_allow`，契约 §3.3）。 */
    if !public_write_allow(&client_ip_for_limit(addr, &headers)) {
        return Err(api_err(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
            "尝试太频繁，请稍后再试",
        ));
    }
    let (person_id, team, account_name) = my_token_required_with_restore(&st, &headers).await?;
    let phone = body
        .get("phone")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let qq = body.get("qq").and_then(Value::as_str).unwrap_or_default();
    /* 密码**不 trim**；空串 = 不改密码（"newPass": "" 视同未传）。 */
    let new_pass = body
        .get("newPass")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    crate::store::my_account_profile_bind(&st, &account_name, phone, qq, new_pass.as_deref())
        .await
        .map_err(|e| store_err(e, bind_code))?;
    my_tokens_revoke(&account_name);
    let token = my_token_put(person_id, team.clone(), account_name.clone());
    let view_team = crate::store::my_account_view_team(&st, &account_name).await;
    let mut v = login_ok(&st, person_id, &account_name, &team, &token).await;
    v["switchable"] = json!(view_team.switchable());
    tracing::info!(
        account = %account_name,
        person_id,
        phone = %mask_phone(phone),
        "展示端账号完成首次补全（手机号 + QQ + 密码），旧 token 已吊销并换发"
    );
    Ok(Json(v))
}

/// 展示端写接口限流：每 IP 每分钟 5 次。
///
/// 2026-10-17 用户口径「去除登录限速」：登录链路（verify / my_login）原先挂的
/// 这道闸已摘除 —— 本函数只给**已登录**写口挡脚本刷，不再用于登录限速。
/// 2026-10-18 起调用方：个人页改密（`my_password_set`）、首次补全
/// （`my_profile_bind`）、绑定 QQ 邮箱（`my_email_set`）—— 三者共用同一档 5 次/分
/// （契约 §3.3 / §3.4 都是「5 次/分/IP」）。
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

/// 「凭艺名认领开号」写口限流：每 IP 每分钟 5 次（独立一档，不与已登录写口共用桶）。
///
/// 为什么单独限流：认领是**无身份校验**的写接口（2026-10-18 用户拍板「凭艺名直接
/// 认领开号」），任何人只要知道一个还没被占用的艺名就能建号 —— 限流是防批量抢注
/// 的兜底（档位与其它写口一致：5 次/分/IP；正常用户一次就够，不会误伤）。
pub(super) static CLAIM_LIMITER: OnceLock<VerifyLimiter> = OnceLock::new();

/// true = 本次允许认领；false = 429 RATE_LIMITED。
fn claim_allow(ip: &str) -> bool {
    CLAIM_LIMITER
        .get_or_init(|| VerifyLimiter::new(5))
        .allow(ip)
}

/* -------- 「忘记密码重置」按手机号限流（2026-10-17 审计 4-P2-A） --------
 *
 * 端点免登录、只认手机号，第四轮把「明文回显」拿掉后仍有一条可用性攻击面：
 * 任何人只要知道主播手机号，就能把账号密码重置掉并吊销其全部 token
 * （my_password_reset 的既有行为），主播本人正在用的登录态直接被踢。
 * （2026-10-17 起重置 = 随机密码发绑定邮箱，主播收邮件后可自行恢复，
 * 但「反复触发把人踢下线 / 刷邮箱」的骚扰面仍在，这层闸继续保留。）
 * 反复触发即可把「忘记密码」变成定向骚扰武器，且 12 次/分/IP 的限流换 IP 就绕过。
 *
 * 这里在内存里按手机号再做一层闸（与 IP 限流并存）：
 * - 同一手机号 10 分钟内只**真正重置一次**；
 * - 1 小时内最多真正重置 3 次；
 * - 被抑制的请求**不改密码、不吊销 token、不登记新窗口**，但对外仍回
 *   `{ok:true, names:<现有账号数>}`，与正常请求同形（不暴露「被限流」这一中间态）。
 *
 * 进程内状态（与 my token 一样重启清零）；读取时顺手清理 1 小时前的记录，
 * map 不会随历史手机号无限增长。 */
static PASSWORD_RESET_GUARD: OnceLock<Mutex<HashMap<String, Vec<Instant>>>> = OnceLock::new();

/// 同一手机号两次「真正重置」的最小间隔
const RESET_GUARD_MIN_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// 1 小时窗口内允许的真正重置次数上限
const RESET_GUARD_MAX_PER_HOUR: usize = 3;
/// 限流窗口（与上面「1 小时最多 3 次」同一口径）
const RESET_GUARD_WINDOW: Duration = Duration::from_secs(60 * 60);

fn reset_guard() -> &'static Mutex<HashMap<String, Vec<Instant>>> {
    PASSWORD_RESET_GUARD.get_or_init(|| Mutex::new(HashMap::new()))
}

/// true = 本次允许真正重置；false = 抑制（10 分钟内重复，或 1 小时已满 3 次）。
/// 调用即顺带清理：超窗记录丢弃、空 key 删除。
fn reset_guard_allow(phone: &str) -> bool {
    let mut guard = reset_guard().lock().expect("重置限流锁 poisoned");
    let now = Instant::now();
    guard.retain(|_, hist| {
        hist.retain(|t| now.duration_since(*t) < RESET_GUARD_WINDOW);
        !hist.is_empty()
    });
    match guard.get(phone) {
        // 10 分钟内已真正重置过：抑制（防止「知道了手机号就能反复踢人下线」）
        Some(hist)
            if hist
                .last()
                .is_some_and(|t| now.duration_since(*t) < RESET_GUARD_MIN_INTERVAL) =>
        {
            false
        }
        // 1 小时窗口已满：抑制
        Some(hist) if hist.len() >= RESET_GUARD_MAX_PER_HOUR => false,
        _ => true,
    }
}

/// 真正重置成功后登记一次（被抑制的请求不登记，避免刷请求把封锁期越拖越长）。
fn reset_guard_record(phone: &str) {
    let mut guard = reset_guard().lock().expect("重置限流锁 poisoned");
    guard
        .entry(phone.to_string())
        .or_default()
        .push(Instant::now());
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
    /* 改密写接口限流（每 IP 每分钟 5 次）：改密要跑 argon2，挡脚本刷写口。
    2026-10-17 用户口径「去除登录限速」只去了登录链路（verify / my_login）的
    两道闸；这里是**已登录**的改密写口，不属于登录限速，保留。 */
    if !public_write_allow(&client_ip_for_limit(addr, &headers)) {
        // 与 profile/bind、email 同形：429 + code（前端 apiErrText 按 RATE_LIMITED 出文案）
        return Err(api_err(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
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
    /* 清临时密码与强制改密标记（契约 §3.6）由 store 的改密实现负责（`temp_pass_*`
    与 `must_change_pass` 同一条 UPDATE 清掉，见 `self_set_password_inner`）——
    handler 不再重复写一遍，避免一次改密跑两条 UPDATE。
    `mustChangePass` 是展示端「临时密码登录后请改密」弹层的唯一触发源，所以下面
    响应里恒回 false。 */
    let token = my_token_put(person_id, team, account_name);
    // 不打账号名：未绑定账号的 name 就是手机号本身（同 my_password_reset 的口径）
    tracing::info!("展示端账号已自助重置密码（旧 token 已吊销并换发新 token）");
    Ok(Json(json!({
        "ok": true, "token": token,
        // 契约 §3.6：改密成功即清强制改密标记，前端据此关掉提示层
        "mustChangePass": false,
    })))
}

/// POST /api/public/my/password/reset {phone, qq}
///
/// 忘记密码（未登录）：**手机号 + QQ 号双因子**核对通过后，给账号绑定邮箱发一封
/// 「15 分钟内有效的临时密码」。临时密码在有效期内可重复使用；落库时常密码会被
/// 覆盖成不可知哈希（旧密码立即失效，契约 §2.5），所以用户必须走「临时密码登录 →
/// 立即改密」这条路。
///
/// 口径沿革（这一条被产品反复改过，故完整记录）：
/// - 2026-10-14 审计 P0：原是「重置成手机号后四位 + 明文回显」，任意账号接管；
///   改 CSPRNG 随机串，但仍明文回显一次；
/// - 2026-10-17 审计 4-P1-1：不再明文回显；
/// - **2026-10-18 用户拍板（本轮）**：改成「手机号 + QQ 号」双因子 + 邮件临时密码
///   （契约 §3.5）。没有 QQ 的主播找管理员重置 —— 重置路径必须有 QQ。
///
/// 严格顺序（契约 §3.5；顺序不可调换，避免半成品状态与账号枚举）：
///   1. 每 IP 每分钟 12 次限流（`public_password_reset_allow`，档位沿用不动）；
///   2. 手机号格式（`validate_my_phone`）→ 400 INVALID_PHONE；
///   3. 同手机号 10 分钟 1 次 / 1 小时 3 次的二级闸（`PASSWORD_RESET_GUARD`）：
///      被抑制回**同形成功包**（`emailMask:""`），不发信、不改库、不吊销 token；
///   4. 双因子定位账号（`my_account_reset_target`，任一不匹配统一 400 RESET_MISMATCH，
///      不泄露手机号是否存在 / 是否绑了 QQ）；
///   5. 生成临时密码（CSPRNG 10 位）；
///   6. SMTP 发信 —— **失败 503 且不落库、不改密码、不吊销 token**；
///   7. 发信成功后才落库（`my_account_reset_apply`：写临时密码哈希 + 15 分钟有效期 +
///      强制改密标记，并把常密码覆盖成不可知哈希）；
///   8. 吊销该账号全部 my token。
pub async fn my_password_reset(
    State(st): State<ArcState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    /* 1. IP 限流（12 次/分，档位沿用）。 */
    if !public_password_reset_allow(&client_ip_for_limit(addr, &headers)) {
        return Err(api_err(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
            "尝试太频繁，请稍后再试",
        ));
    }
    let phone_raw = body
        .get("phone")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let qq = body.get("qq").and_then(|v| v.as_str()).unwrap_or_default();
    /* 2. 手机号格式：本地可判定，先于任何账号查询（防枚举 + 省一次 DB 查询）。 */
    let phone = crate::store::my_account::validate_my_phone(phone_raw).map_err(|e| {
        api_err(
            axum::http::StatusCode::BAD_REQUEST,
            "INVALID_PHONE",
            crate::store::my_account::biz_msg(&e).to_string(),
        )
    })?;
    if phone.is_empty() {
        return Err(api_err(
            axum::http::StatusCode::BAD_REQUEST,
            "INVALID_PHONE",
            crate::store::MSG_PHONE_REQUIRED,
        ));
    }
    /* 3. 同手机号窗口闸（2026-10-17 审计 4-P2-A）：抑制时回同形成功包，不做任何状态变化。 */
    if !reset_guard_allow(&phone) {
        tracing::info!(
            phone = %mask_phone(&phone),
            "重置密码请求过于频繁，已抑制（本次不发信、不改库、不吊销 token）"
        );
        return Ok(Json(reset_ok_json("")));
    }
    /* 4. 双因子：手机号 + QQ 必须命中同一普通账号，且该账号绑定的正是这个 QQ 邮箱。
    所有不匹配分支（未绑手机号 / 未绑 QQ / QQ 不匹配 / 管理员账号 / 手机号查不到）
    统一一句文案（防枚举），错误码 RESET_MISMATCH。 */
    let (name, email) = crate::store::my_account_reset_target(&st, &phone, qq)
        .await
        .map_err(|e| {
            if crate::store::my_account::is_biz(&e) {
                api_err(
                    axum::http::StatusCode::BAD_REQUEST,
                    "RESET_MISMATCH",
                    crate::store::my_account::biz_msg(&e).to_string(),
                )
            } else {
                tracing::error!(error = %e, "重置展示端账号密码定位失败");
                ApiError::from(anyhow::anyhow!(e))
            }
        })?;
    /* 5. 生成临时密码（10 位、去易混淆字符、OS CSPRNG）。 */
    let temp_pass = crate::random::random_password();
    /* 6. 发信。发不出去 = 什么都不改（避免把用户锁死在「已重置但没拿到密码」的中间态）。 */
    crate::mailer::send_temp_password_mail(&email, &temp_pass)
        .await
        .map_err(|e| {
            tracing::error!(
                phone = %mask_phone(&phone),
                error = %e,
                "重置密码临时密码邮件发送失败"
            );
            api_err(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "SMTP_UNAVAILABLE",
                "邮件服务暂时不可用，请稍后重试或联系管理员（本次没有改密码）",
            )
        })?;
    /* 7. 邮件已发 → 才落库（契约 §3.5：发信成功后才 apply，顺序不可颠倒）。 */
    let applied_name = crate::store::my_account_reset_apply(&st, &phone, &temp_pass)
        .await
        .map_err(|e| {
            if crate::store::my_account::is_biz(&e) {
                tracing::warn!(error = %e, "重置密码落库时目标账号已消失");
                api_err(
                    axum::http::StatusCode::BAD_REQUEST,
                    "RESET_MISMATCH",
                    crate::store::my_account::biz_msg(&e).to_string(),
                )
            } else {
                tracing::error!(error = %e, "重置展示端账号密码落库失败");
                ApiError::from(anyhow::anyhow!(e))
            }
        })?;
    /* 只有真正重置成功才登记窗口；被抑制的请求不登记，避免刷请求把封锁期越拖越长。 */
    reset_guard_record(&phone);
    /* 8. 改密后旧登录态必须立刻失效（临时密码登录要重新登）。 */
    my_tokens_revoke(&applied_name);
    tracing::info!(
        phone = %mask_phone(&phone),
        account = %name,
        "展示端账号已下发临时密码（15 分钟内有效，旧 token 已吊销）"
    );
    /* 只回脱敏邮箱，不回临时密码（密码在用户邮箱里）。 */
    Ok(Json(reset_ok_json(&crate::store::mask_qq_email(&email))))
}

/// POST /api/public/my/email {qq}（兼容旧 {email}，契约 §3.4）
///
/// 个人页（已登录）自助绑定 / 改绑 / 解绑 **QQ 邮箱**（2026-10-18 口径）：
/// 忘记密码重置走「手机号 + QQ」双因子，所以这里存的是 `qq@qq.com`。
/// 入参 `{qq}`；`{qq:""}` = 解绑。旧前端传 `{email}` 时只接受 `@qq.com` 域
/// （存量 163 等非 QQ 邮箱一律 400 INVALID_QQ —— 那些邮箱已不参与重置双因子）。
/// 身份只认 `X-My-Token` 头，请求体里不接受账号名 —— 只能改自己的邮箱。
///
/// 返回 `{ok, email, emailMask}`：`email` 是脱敏值（保留旧键兼容旧前端），
/// `emailMask` 是契约新键，两者同值，任何情况下都不回明文邮箱。
pub async fn my_email_set(
    State(st): State<ArcState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    /* 契约 §3.4：该写口新增 5 次/分/IP 限流（与改密同档，防脚本刷绑定）。 */
    if !public_write_allow(&client_ip_for_limit(addr, &headers)) {
        return Err(api_err(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
            "尝试太频繁，请稍后再试",
        ));
    }
    let Some(token) = header_my_token(&headers) else {
        return Err(ApiError::auth("登录已过期，请重新登录"));
    };
    /* 内存 miss 时从 Redis 回填（与 my_password_set 同口径）：
    服务重启后 24h 内的登录态照常可以改邮箱，不必先重新登录。 */
    let mut ident = my_token_take(&token);
    if ident.is_none() {
        ident = super::admin::my_token_restore(&st, &token).await;
    }
    let Some((_person_id, _team, account_name)) = ident else {
        return Err(ApiError::auth("登录已过期，请重新登录"));
    };
    /* 入参归一：首选 `qq`（原样交给 store 的 `normalize_qq` 去空格/校验）；
    兼容旧 `email` —— 只认 `@qq.com` 域，其它域与空串语义分别是 400 / 解绑。 */
    let qq: String = match body.get("qq").and_then(Value::as_str) {
        Some(q) => q.to_string(),
        None => match body.get("email").and_then(Value::as_str) {
            Some(mail) if !mail.trim().is_empty() => {
                let mail = mail.trim();
                match crate::store::qq_from_email(mail) {
                    Some(q) => q,
                    // 域是 qq.com 但本地部分不是合法 QQ 号 → 按「QQ 号格式」报
                    // （与 store 同一句文案）；非 QQ 域 → 明确只支持 QQ 邮箱。
                    None => {
                        let qq_domain = mail
                            .rsplit_once('@')
                            .is_some_and(|(_, d)| d.eq_ignore_ascii_case("qq.com"));
                        let msg = if qq_domain {
                            crate::store::MSG_INVALID_QQ
                        } else {
                            "只支持 QQ 邮箱，请填写 QQ 号"
                        };
                        return Err(api_err(
                            axum::http::StatusCode::BAD_REQUEST,
                            "INVALID_QQ",
                            msg,
                        ));
                    }
                }
            }
            // 旧 `{email:""}` / 两个字段都没给 = 解绑
            _ => String::new(),
        },
    };
    let email_mask = crate::store::my_account_set_qq(&st, &account_name, &qq)
        .await
        .map_err(|e| store_err(e, bind_code))?;
    tracing::info!(
        account = %account_name,
        email = %email_mask,
        "展示端账号已更新绑定 QQ 邮箱"
    );
    Ok(Json(json!({
        "ok": true,
        // `email` 保留旧键（脱敏值，非明文），`emailMask` 为契约新键（契约 §3.4）
        "email": email_mask.clone(),
        "emailMask": email_mask,
    })))
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

/* ------------------------------- 契约单测 -------------------------------
 *
 * 只测 handler 的**纯逻辑**（错误码映射 / 响应形状），不连库：
 * store 的 QQ/密码/临时密码/双因子/脱敏由 `store::my_account` 自己的单测覆盖。
 */
#[cfg(test)]
mod contract_tests {
    use super::*;
    use crate::store::my_account::LoginFail;

    /// 契约 §3.1：登录失败仍是 HTTP 200 + `{ok:false, code, error}`，
    /// 三个 code 与 store 的 `LoginFail` 变体一一对应（前端按 code 分支）。
    #[test]
    fn login_fail_codes_match_contract() {
        let cases = [
            (LoginFail::BadPassword, "BAD_PASSWORD"),
            (LoginFail::NoAccount, "NO_ACCOUNT"),
            (LoginFail::TempExpired, "TEMP_EXPIRED"),
        ];
        for (fail, code) in cases {
            let v = login_fail(fail);
            assert_eq!(v["ok"], false, "{code}");
            assert_eq!(v["code"], code);
            assert!(
                v.get("error")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty()),
                "{code} 必须带可展示文案"
            );
            // 失败响应绝不能带 token（否则等于失败也发了登录态）
            assert!(v.get("token").is_none(), "{code} 不得下发 token");
        }
    }

    /// 契约 §3.3 / §3.4：绑定类 BIZ 文案 → 错误码。直接引用 store 冻结常量，
    /// 两边文案漂移时这条用例先红。
    #[test]
    fn bind_code_maps_store_text_to_contract_codes() {
        use crate::store as s;
        assert_eq!(bind_code(s::MSG_PHONE_REQUIRED), "INVALID_PHONE");
        assert_eq!(
            bind_code("手机号格式不对：应为 5~20 位数字"),
            "INVALID_PHONE"
        );
        assert_eq!(bind_code(s::MSG_PHONE_TAKEN), "PHONE_TAKEN");
        assert_eq!(bind_code(s::MSG_INVALID_QQ), "INVALID_QQ");
        assert_eq!(bind_code(s::MSG_QQ_TAKEN), "QQ_TAKEN");
        assert_eq!(
            bind_code(s::MSG_PROFILE_ALREADY_DONE),
            "PROFILE_ALREADY_DONE"
        );
        assert_eq!(bind_code(s::MSG_NEED_PROFILE), "NEED_PROFILE");
        assert_eq!(bind_code(s::MSG_PASS_INVALID), "INVALID_PASS");
        assert_eq!(bind_code(s::MSG_PASS_BLANK), "INVALID_PASS");
        assert_eq!(bind_code(s::MSG_PASS_CTRL), "INVALID_PASS");
        // 库里读不到账号之类的新文案：不瞎猜，回落通用码（error 原样仍可读）
        assert_eq!(bind_code("账号不存在，请重新登录"), "BAD_REQUEST");
    }

    /// 契约 §3.2：认领失败按语义映射（批量开号/并发撞主键等场景都能归位）。
    #[test]
    fn claim_code_maps_claim_failures() {
        use crate::store as s;
        assert_eq!(claim_code(s::MSG_PASS_INVALID), "INVALID_PASS");
        assert_eq!(claim_code("账号已存在，请直接登录"), "ACCOUNT_EXISTS");
        assert_eq!(claim_code("该艺名已被认领，请直接登录"), "ALREADY_CLAIMED");
        assert_eq!(
            claim_code("该主播已有登录账号，请联系管理员"),
            "PERSON_TAKEN"
        );
        assert_eq!(claim_code("该主播已离团，不能认领"), "NAME_LEFT");
        assert_eq!(
            claim_code("该艺名在台账里有多个主播，请联系管理员"),
            "NAME_DUPLICATED"
        );
        assert_eq!(claim_code("该主播性别信息异常，请联系管理员"), "BAD_GENDER");
        assert_eq!(
            claim_code("台账里没有这个艺名，请联系管理员开号"),
            "NAME_NOT_FOUND"
        );
        assert_eq!(claim_code("完全没见过的文案"), "CLAIM_FAILED");
    }

    /// 认领写口限流：独立一档 5 次/分/IP（防无身份校验的批量抢注），
    /// 超限 → 429 RATE_LIMITED（handler 里由 `claim_allow` 判）。
    #[test]
    fn claim_limiter_allows_five_per_minute_per_ip() {
        // 用文档保留网段当假 IP，避免与其他用例/真实访客撞桶
        let ip = "198.51.100.77";
        for i in 0..5 {
            assert!(claim_allow(ip), "第 {i} 次应放行（5 次/分/IP）");
        }
        assert!(!claim_allow(ip), "第 6 次必须被限流");
        // 独立桶：别的 IP 不受影响
        assert!(claim_allow("198.51.100.78"));
    }

    /// 契约 §3.5：成功响应形状（`expiresInMinutes: 15` 固定；抑制时 emailMask 空串）。
    #[test]
    fn reset_ok_shape_matches_contract() {
        let v = reset_ok_json("123***@qq.com");
        assert_eq!(v["ok"], true);
        assert_eq!(v["sent"], true);
        assert_eq!(v["expiresInMinutes"], 15);
        assert_eq!(v["emailMask"], "123***@qq.com");
        // 被同号闸抑制：同形包但 emailMask 空串、且绝不回临时密码
        let suppressed = reset_ok_json("");
        assert_eq!(suppressed["ok"], true);
        assert_eq!(suppressed["emailMask"], "");
        assert!(suppressed.get("tempPass").is_none());
    }
}
