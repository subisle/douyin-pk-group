//! store::my_account —— 自 store.rs 按业务域拆分（对外语义不变）。

use super::*; // 共享 w() / now_str() / AppState 等内部工具

/// 抖音号格式：6~20 位字母 / 数字 / 下划线 / 点（抖音号官方口径的宽松版）
pub fn validate_douyin_no(v: &str) -> std::result::Result<String, String> {
    let v = v.trim();
    let ok = (6..=20).contains(&v.len())
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if !ok {
        return Err(biz("抖音号格式不对：6~20 位字母、数字、下划线或点"));
    }
    Ok(v.to_string())
}

/* ------------------------------ 我的页面账号 ------------------------------ */

/// 业务拒绝（**面向用户的人话**）与库层故障的分流标记（2026-10-16）。
///
/// 背景：`my_account_set` / `bind_inner` / `my_accounts_batch_open` 返回
/// `Result<_, String>`，路由层以前要么 `anyhow::anyhow!(e)`（= safe=false，
/// 「该主播已存在绑定的账号，请联系管理员」这类可执行的业务文案被抹成
/// 400 通用话术「请求参数有误」，真实原因只进日志），要么整包
/// `ApiError::bad_request`（= 库层报错里的表名/约束名/SQL 片段会原样外发，
/// 违反 2026-10-14 审计 P2「不外发底层细节」的口径）。
///
/// 现在的约定：store 层把**业务拒绝**（校验失败 / 状态冲突，文案是人话）
/// 统一包上 `BIZ::` 前缀（[`biz`] / [`is_biz`] / [`biz_msg`]）；库层故障
/// （`format!("…失败：{e}")`，内容是 sqlx/PG 原文）保持无前缀。
/// 路由层 `split_my_account_err` 一处分流：有前缀 → 400 原文原样下发；
/// 无前缀 → 500 走通用文案兜底（原文留日志）。与
/// [`crate::store::WxBindConflict`] 的 typed-error 范式同目标，
/// 这里选前缀是为了不改动 `Result<_, String>` 签名 —— 波及面最小。
pub const BIZ_PREFIX: &str = "BIZ::";

/// 业务拒绝文案 → 带标记的错误串。
pub(crate) fn biz(msg: impl Into<String>) -> String {
    format!("{BIZ_PREFIX}{}", msg.into())
}

/// 是否业务拒绝（带 `BIZ::` 前缀）。
pub fn is_biz(err: &str) -> bool {
    err.starts_with(BIZ_PREFIX)
}

/// PG 唯一约束冲突（SQLSTATE 23505）。
/// 旧代码认的是 MySQL 文案 `Duplicate entry`，PG 原文是
/// `duplicate key value violates unique constraint "xxx"` —— 两种都认，
/// 避免切库后运营看不到「到底是手机号还是姓名撞了」。
fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505"))
}

/// 绑定 UPDATE 的错误分流（2026-10-17 审计 4-N14）：唯一约束冲突（23505，
/// `uk_my_person_uniq`——并发下另一个账号先绑走了同一主播）折成业务文案，
/// 不再把 PG 原文（表名/约束名）拼进返回串；其余错误照旧透传给上层分流 500。
fn bind_update_err(e: sqlx::Error) -> String {
    if is_unique_violation(&e) {
        return biz("该主播刚被其它账号绑定，请刷新后重试");
    }
    format!("绑定失败：{e}")
}

/// 取出去掉标记的业务文案（非业务错误返回原文，调用方无需先判 is_biz）。
pub fn biz_msg(err: &str) -> &str {
    err.strip_prefix(BIZ_PREFIX).unwrap_or(err)
}

/* ------------------- 跨层冻结文案（2026-10-18，BIZ 前缀内的人话） -------------------
 *
 * 这些字面量是**接口契约的一部分**：handler（`public_teams/` 下，T2）靠它们把手写的
 * `Result<_, String>` 映射成 HTTP 错误码（INVALID_QQ / QQ_TAKEN / NEED_PROFILE …）。
 * 导出成常量而不是让 handler 再抄一遍中文，避免两边文案漂移；改文案 = 改接口，
 * 必须同步改 handler 的映射与 `docs/26/05-实现契约-v2.md`。
 */

/// QQ 号格式拒绝（handler → 400 INVALID_QQ）
pub const MSG_INVALID_QQ: &str = "QQ 号格式不对：应为 5~12 位数字且不以 0 开头";
/// QQ 邮箱已被别的账号占用（400 QQ_TAKEN）
pub const MSG_QQ_TAKEN: &str = "该 QQ 邮箱已被其他账号绑定";
/// 手机号已被别的账号占用（400 PHONE_TAKEN）
pub const MSG_PHONE_TAKEN: &str = "该手机号已被其他账号绑定，请联系管理员";
/// 首次补全缺少手机号（400 INVALID_PHONE）
pub const MSG_PHONE_REQUIRED: &str = "请输入手机号";
/// 已不再是「待补全」态（400 PROFILE_ALREADY_DONE）
pub const MSG_PROFILE_ALREADY_DONE: &str = "该账号已完成绑定，如需修改手机号请联系管理员";
/// S1（phone=''）账号试图只绑 QQ（403 NEED_PROFILE）
pub const MSG_NEED_PROFILE: &str = "请先完成手机号绑定";
/// 忘记密码双因子不匹配（400 RESET_MISMATCH）：账号不存在 / 未绑 QQ / QQ 不匹配 /
/// 是管理员账号 / QQ 号格式非法，**全部共用这一句**（防手机号存在性枚举）。
pub const MSG_RESET_MISMATCH: &str =
    "手机号与 QQ 号不匹配，或该账号未绑定 QQ 邮箱，请联系管理员重置";
/// 重置落库时账号消失（兜底，正常路径不该出现）
pub const MSG_RESET_NO_TARGET: &str = "该手机号未绑定账号，请重新申请";
/// 密码长度越界
pub const MSG_PASS_INVALID: &str = "密码需为 4~64 个字符";
/// 空密码 / 全空白
pub const MSG_PASS_BLANK: &str = "密码不能为空";
/// 含 ASCII 控制字符
pub const MSG_PASS_CTRL: &str = "密码不能包含控制字符";

/// QQ 邮箱域名（唯一允许值，小写）。
pub const QQ_MAIL_DOMAIN: &str = "@qq.com";
/// QQ 号长度下限 / 上限（2026-10-18 口径：5~12 位、首位非 0）。
pub const QQ_MIN_LEN: usize = 5;
/// QQ 号长度上限
pub const QQ_MAX_LEN: usize = 12;
/// 密码长度下限（按 **Unicode 字符** 计数，不是字节）
pub const PASS_MIN_LEN: usize = 4;
/// 密码长度上限（按 **Unicode 字符** 计数，不是字节）
pub const PASS_MAX_LEN: usize = 64;

/// 去掉末尾的 `@qq.com`（大小写不敏感）。
/// 用 `char_indices` 切而不是 `strip_suffix`：后者只认精确字节，`@QQ.COM` 这种
/// 脏值（用户从别处粘贴、大小写被改过）也要能剥；同时保证不会切在非 ASCII
/// 字符中间。
fn strip_qq_mail_suffix(s: &str) -> &str {
    let n = QQ_MAIL_DOMAIN.chars().count();
    match s.char_indices().rev().nth(n - 1).map(|(i, _)| i) {
        Some(i) if s[i..].eq_ignore_ascii_case(QQ_MAIL_DOMAIN) => &s[..i],
        _ => s,
    }
}

/// QQ 号归一化 + 校验（2026-10-18 用户拍板）：
/// 去掉**所有**空白（含全角空格）→ 兼容粘贴整段 QQ 邮箱（剥 `@qq.com`，大小写
/// 不敏感）→ 必须是 5~12 位 ASCII 数字且首位非 0。
///
/// 全角数字（`１２３４５６`）**不自动转换**：用户看不出服务端改过什么，宁可拒绝。
/// `Err` 带 `BIZ::` 前缀（[`MSG_INVALID_QQ`]），handler 原样 400 INVALID_QQ。
pub fn normalize_qq(raw: &str) -> std::result::Result<String, String> {
    let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    let digits = strip_qq_mail_suffix(&compact);
    let ok = digits.len() >= QQ_MIN_LEN
        && digits.len() <= QQ_MAX_LEN
        && !digits.starts_with('0')
        && digits.chars().all(|c| c.is_ascii_digit());
    if !ok {
        return Err(biz(MSG_INVALID_QQ));
    }
    Ok(digits.to_string())
}

/// QQ 号 → 完整邮箱（`123456` → `123456@qq.com`）。非法 QQ 号原样返回 `Err`。
pub fn qq_to_email(qq: &str) -> std::result::Result<String, String> {
    Ok(format!("{}{}", normalize_qq(qq)?, QQ_MAIL_DOMAIN))
}

/// 完整邮箱 → QQ 号反解：只认 `<5~12 位数字>@qq.com`（域名大小写不敏感）。
/// 非 QQ 域名（`xx@163.com`、`123@vip.qq.com`）、本地部分不合 QQ 规则 → `None`。
/// **不做猜测式解析** —— 把 163 邮箱的本地部分当 QQ 号会把密码发到错的地址。
pub fn qq_from_email(email: &str) -> Option<String> {
    let (local, domain) = email.trim().split_once('@')?;
    if !domain.eq_ignore_ascii_case("qq.com") {
        return None;
    }
    let ok = local.len() >= QQ_MIN_LEN
        && local.len() <= QQ_MAX_LEN
        && !local.starts_with('0')
        && local.chars().all(|c| c.is_ascii_digit());
    ok.then(|| local.to_string())
}

/// 是否已绑定**合法 QQ 邮箱**。038 存量非 QQ 邮箱一律 false → 视为「未绑 QQ」，
/// 前端提示重新绑定，而不是把 163 地址当 QQ 用。
pub fn is_qq_email(email: &str) -> bool {
    qq_from_email(email).is_some()
}

/// 展示端统一脱敏（2026-10-18）：
/// - QQ 邮箱 → 本地部分保留前 3 位：`123456789@qq.com` → `123***@qq.com`；
/// - 非 QQ 邮箱（038 存量）→ 沿用 `mask_email` 口径：`a***@163.com`；
/// - 空串 → 空串；没有 `@` 的异常值 → `***`（不泄露任何可读内容）。
///
/// **禁止**把完整 QQ 号或完整邮箱写进任何响应与日志。
pub fn mask_qq_email(email: &str) -> String {
    let e = email.trim();
    if e.is_empty() {
        return String::new();
    }
    let Some((local, domain)) = e.split_once('@') else {
        return "***".to_string();
    };
    if is_qq_email(e) {
        let head: String = local.chars().take(3).collect();
        return format!("{head}***{QQ_MAIL_DOMAIN}");
    }
    // 非 QQ 域名：首字符 + ***@域名（与 public_teams::wx::mask_email 同口径）
    let mut out = String::new();
    if let Some(c) = local.chars().next() {
        out.push(c);
    }
    out.push_str("***@");
    out.push_str(domain);
    out
}

/// 展示端密码校验（2026-10-18 用户拍板，**推翻** 2026-10-17 的「字母数字点」）：
/// **4~64 个字符，允许中英文 / 特殊字符 / 任何符号（含空格）**；
/// 拒绝 ASCII 控制字符（`\x00-\x1F`、`\x7F`，防换行/制表符混进库与日志）；
/// **不 trim** —— 首尾空格是密码的一部分，trim 会造成「看着一样、登录不上」。
///
/// 空串 / 全空白仍按「密码不能为空」拒绝（全空格不是密码；长度与字符本身
/// 一律原样，不 trim）。`Err` 带 `BIZ::` 前缀（[`MSG_PASS_*`]）。
pub fn validate_my_password(pass: &str) -> std::result::Result<String, String> {
    if pass.is_empty() || pass.chars().all(|c| c.is_whitespace()) {
        return Err(biz(MSG_PASS_BLANK));
    }
    if pass.chars().any(|c| c.is_ascii_control()) {
        return Err(biz(MSG_PASS_CTRL));
    }
    let n = pass.chars().count();
    if !(PASS_MIN_LEN..=PASS_MAX_LEN).contains(&n) {
        return Err(biz(MSG_PASS_INVALID));
    }
    Ok(pass.to_string())
}

/// 校验姓名 + 密码。`Err`(面向用户的 BIZ 文案)，`Ok`((trim 过的姓名, **原样**密码))。
///
/// 姓名规则不变（非空、≤20 字）；密码规则见 [`validate_my_password`]
/// （2026-10-18 起从「4~20 位字母数字点」放宽到「4~64 任意 Unicode」）。
/// 登录 / 改密 / 绑定等**所有**密码入口都只走这一套，不再各写一份。
pub fn validate_my_account(
    name: &str,
    pass: &str,
) -> std::result::Result<(String, String), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(biz("姓名不能为空"));
    }
    if name.chars().count() > 20 {
        return Err(biz("姓名太长（最多 20 字）"));
    }
    let pass = validate_my_password(pass)?;
    Ok((name.to_string(), pass))
}

/// 展示端账号密码哈希：argon2id。验证码 / 手工密码都只落哈希，不再存明文。
pub(crate) fn hash_my_password(pass: &str) -> String {
    crate::auth::hash_password(pass)
}

pub(crate) fn verify_my_password(pass: &str, stored: &str) -> bool {
    if stored.starts_with("$argon2") {
        return crate::auth::verify_password(pass, stored);
    }
    // 存量明文口令：只支持一次性兼容校验；调用方校验成功后必须立即升级为 argon2id。
    stored == pass
}

/// 登录命中后把老明文密码就地升级为 argon2id。
/// 升级失败不阻断登录；下一次成功登录会重试。
async fn upgrade_my_password_if_plain(pool: &PgPool, name: &str, stored: &str, pass: &str) {
    if stored.starts_with("$argon2") {
        return;
    }
    if let Err(e) = sqlx::query(&crate::pq::rewrite(
        "UPDATE my_accounts SET pass = ? WHERE name = ?",
    ))
    .bind(hash_my_password(pass))
    .bind(name)
    .execute(pool)
    .await
    {
        tracing::warn!(error = %e, name, "展示端老明文密码升级为 argon2id 失败");
    }
}

/// 手机号归一化（选填）：去空格横线后须为 5~20 位纯数字；空串 = 未设置
///
/// pub 的原因：主播档案（person.phone）与登录账号（my_accounts.phone）两套手机号
/// **必须同一套格式口径**，否则会出现"台账里能存、开号时被拒"的割裂（docs/13 §2.2）。
pub fn validate_my_phone(phone: &str) -> std::result::Result<String, String> {
    let p: String = phone
        .trim()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    if p.is_empty() {
        return Ok(String::new());
    }
    if !p.chars().all(|c| c.is_ascii_digit()) || p.len() < 5 || p.len() > 20 {
        return Err(biz("手机号格式不对：应为 5~20 位数字"));
    }
    Ok(p)
}

/// 邮箱格式校验（2026-10-17 绑定邮箱）：基本形状 + 无空白/换行（防 SMTP 头注入）。
/// 返回归一化（去首尾空格）后的邮箱；空串 = 解绑，直接放行（调用方语义：清空绑定）。
pub fn validate_email(email: &str) -> std::result::Result<String, String> {
    let e = email.trim();
    if e.is_empty() {
        return Ok(String::new());
    }
    if e.len() > 128 {
        return Err(biz("邮箱过长（最多 128 字符）"));
    }
    // 防 SMTP 头注入：CR/LF/空格/制表符一律拒
    if e.chars().any(|c| c.is_whitespace()) {
        return Err(biz("邮箱格式不正确"));
    }
    // 基本形状：本地部分 @ 域名，域名至少含一个点
    let Some(at) = e.find('@') else {
        return Err(biz("邮箱格式不正确"));
    };
    if at == 0 || at == e.len() - 1 {
        return Err(biz("邮箱格式不正确"));
    }
    let domain = &e[at + 1..];
    if !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
        return Err(biz("邮箱格式不正确"));
    }
    Ok(e.to_string())
}

/* ─────────────── 可看团别 view_team（迁移 024，2026-10-15） ───────────────
 *
 * 管理员观看账号原先只有 is_admin 一个开关，登录后固定落「任意团」token，
 * 没法按团收敛。用户口径要求分权：
 *   帅帅   总管理员：男女团都能看，且页内可切「全团 / 男团 / 女团」
 *   饭团   女团：只看女团
 *   鹏先生 全团：和帅帅同款（两个团都能看 + 可切）
 *
 * 取值（列里存的是这里 `code()` 的字符串）：
 *   ""（默认/缺省）沿用旧口径：绑了主播按主播所在团，没绑 / is_admin 按任意团
 *   "any"     全团账号：两个团都能看，**且允许页内切换**
 *   "male"    只看男团（钉死，不受改绑影响）
 *   "female"  只看女团（钉死，不受改绑影响）
 *
 * 落进 my token 的团别 = `override(base)`：Any 落 ""（gate_team 见空串放行两团），
 * Male/Female 落死值，Inherit 用 base（旧逻辑算出来的团）。
 */

/// 账号的「可看团别」。数据库里存字符串，代码里一律用这个枚举，
/// 避免 "male " / "MALE" / 拼写错误之类的脏值一路裸奔到鉴权判断里。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum ViewTeam {
    /// 未设置：沿用旧口径（按绑定主播所在团；没绑/is_admin 则是任意团）
    Inherit,
    /// 全团：两个团都能看，并允许在页内切换视角
    Any,
    Male,
    Female,
}

impl ViewTeam {
    /// 存库字符串。空串表示未设置（与迁移的 DEFAULT '' 对齐）。
    pub fn code(self) -> &'static str {
        match self {
            ViewTeam::Inherit => "",
            ViewTeam::Any => "any",
            ViewTeam::Male => "male",
            ViewTeam::Female => "female",
        }
    }

    /// 从库里读出的原始字符串解析。**任何非法值一律回落 Inherit**（不报错）：
    /// 列是 2026-10-15 才加的，存量行全是空串；将来万一有脏数据，宁可退回
    /// 「按绑定主播走」的旧行为，也不能让一个账号因为一列脏值彻底登不进去。
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "any" => ViewTeam::Any,
            "male" => ViewTeam::Male,
            "female" => ViewTeam::Female,
            _ => ViewTeam::Inherit,
        }
    }

    /// 这个账号登录后能不能在页内切团（全团账号才行；钉死某团的不能越界切）
    pub fn switchable(self) -> bool {
        matches!(self, ViewTeam::Any)
    }

    /// 落进 my token 的团别。`base` 是旧逻辑算出来的团（绑了主播 = 主播所在团，
    /// 没绑/is_admin = 空串）。
    /// - `Any` → 空串（gate_team 见空串一律放行，前端因此可以切着看）；
    /// - `Male`/`Female` → 钉死该团，**无视 base**（即使账号后来被改绑到别团的主播）；
    /// - `Inherit` → 归一化后用 base：只认 male/female，其余（unknown / 脏值）收敛成
    ///   空串（= 不限团）。2026-10-17 审计 4-P2-1：token 里的团别会被
    ///   site/minutes/dialect 内联进 SQL（`p.gender = '{team}'`），原先靠
    ///   「gender 写面已全收口」这一隐性前提挡注入，这里把前提钉成显性 ——
    ///   正常账号行为一字不变，未来任何写路径漏过白名单也进不了 SQL。
    pub fn token_team(self, base: &str) -> String {
        match self {
            ViewTeam::Any => String::new(),
            ViewTeam::Male => "male".to_string(),
            ViewTeam::Female => "female".to_string(),
            ViewTeam::Inherit => match base {
                "male" | "female" => base.to_string(),
                _ => String::new(),
            },
        }
    }
}

/// 把任意输入收敛成合法存库值（管理端写入前用）。非法/空一律 ""（= Inherit）。
pub fn normalize_view_team(raw: &str) -> String {
    ViewTeam::parse(raw).code().to_string()
}

/// 账号行 → 管理端 JSON（列表项 / 单条 GET 共用）。
///
/// 列一律**按名字**读、缺失列回落默认值：让三条不同年代的 SELECT（039 全列 /
/// 038 有 email / 001 老列）共用同一份映射，不会因为「少跑一次迁移」就把整个
/// 列表打成空（与 `my_account_info` 的 038 回落策略同口径）。
///
/// 2026-10-18 新增三个字段（契约 §2.7 / §3.8）：
/// - `emailMask`：脱敏 QQ 邮箱（**不下发明文**，运营需要核对时让用户自报）；
/// - `qqBound`：是否已绑合法 QQ 邮箱（038 存量非 QQ 邮箱算「未绑 QQ」）；
/// - `mustChangePass`：是否处于「临时密码登录后待改密」状态（039，缺列 = false）。
fn account_row_json(r: &sqlx::postgres::PgRow) -> serde_json::Value {
    let email: String = r.try_get("email").ok().unwrap_or_default();
    let source: String = r
        .try_get("source")
        .ok()
        .unwrap_or_else(|| "admin".to_string());
    serde_json::json!({
        "name": r.try_get::<String, _>("name").unwrap_or_default(),
        "phone": r.try_get::<String, _>("phone").unwrap_or_default(),
        "isAdmin": r.try_get::<i16, _>("is_admin").unwrap_or(0) == 1,
        "createdAt": r.try_get::<String, _>("created_at").unwrap_or_default(),
        // bound 缺列（008 之前的库）按老口径「一律已绑定」
        "bound": r.try_get::<i16, _>("bound").unwrap_or(1) == 1,
        "boundAt": r.try_get::<String, _>("bound_at").unwrap_or_default(),
        "source": source,
        "viewTeam": r.try_get::<String, _>("view_team").unwrap_or_default(),
        "emailMask": mask_qq_email(&email),
        "qqBound": is_qq_email(&email),
        "mustChangePass": r.try_get::<i16, _>("must_change_pass").unwrap_or(0) == 1,
    })
}

/// 全部账号（管理端列表用），按添加时间倒序。
///
/// 三级列清单回落（越新的迁移没跑就退一档）：
/// 1. 039 全列（含 email/must_change_pass）；
/// 2. 038 列（含 email，不含 039 三列）；
/// 3. 001 老列（不含 008/024/038/039 的任何新列）。
pub async fn my_account_list(st: &AppState) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    let sqls = [
        "SELECT name, phone, is_admin, created_at, bound, bound_at, source, view_team, email, \
         must_change_pass FROM my_accounts ORDER BY created_at DESC, name ASC",
        "SELECT name, phone, is_admin, created_at, bound, bound_at, source, view_team, email \
         FROM my_accounts ORDER BY created_at DESC, name ASC",
        "SELECT name, phone, is_admin, created_at \
         FROM my_accounts ORDER BY created_at DESC, name ASC",
    ];
    for sql in sqls {
        match sqlx::query(&crate::pq::rewrite(sql)).fetch_all(pool).await {
            Ok(rows) => return rows.iter().map(account_row_json).collect(),
            Err(e) => {
                tracing::warn!(error = %e, "查展示端账号失败，回落更老的列清单重试");
            }
        }
    }
    tracing::error!("查展示端账号失败（含老列清单兜底）");
    Vec::new()
}

/// 手动设置账号（右键主播 / 管理端卡片）：存在则覆盖更新手机号+密码，不存在则新建。
/// 一个姓名对应一个密码（1:1，唯一键兜底 + 代码级查重）；手机号选填，填了则一个手机号也只绑一个姓名。
pub async fn my_account_set(
    st: &AppState,
    name: &str,
    phone: &str,
    pass: &str,
) -> std::result::Result<(), String> {
    my_account_set_pool(w(st).map_err(|e| e.to_string())?, name, phone, pass).await
}

async fn my_account_set_pool(
    pool: &PgPool,
    name: &str,
    phone: &str,
    pass: &str,
) -> std::result::Result<(), String> {
    let (name, pass) = validate_my_account(name, pass)?;
    let phone = validate_my_phone(phone)?;
    if !phone.is_empty() {
        let dup_phone: bool = sqlx::query(&crate::pq::rewrite(
            "SELECT 1 FROM my_accounts WHERE phone = ? AND name != ? AND phone != ''",
        ))
        .bind(&phone)
        .bind(&name)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("查重失败：{e}"))?
        .is_some();
        if dup_phone {
            return Err(biz("手机号已存在（已绑定其它姓名）"));
        }
    }
    let pass_hash = hash_my_password(&pass);
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    /* 039（2026-10-18）：**写密码的每条路径都必须清临时密码三列**，否则
    管理员已把密码换掉、账号却还挂着「必须改密」的假锁（契约 §2.7）。
    未跑 039 的库回落老语句（UPDATE 少三列），不让管理端建号功能整体不可用。 */
    const UPSERT_NEW: &str = "INSERT INTO my_accounts (name, phone, pass, is_admin, created_at) \
         VALUES (?, ?, ?, 0, ?) \
         ON CONFLICT (name) DO UPDATE SET phone = EXCLUDED.phone, pass = EXCLUDED.pass, \
           temp_pass_hash = '', temp_pass_exp = NULL, must_change_pass = 0";
    const UPSERT_LEGACY: &str =
        "INSERT INTO my_accounts (name, phone, pass, is_admin, created_at) \
         VALUES (?, ?, ?, 0, ?) \
         ON CONFLICT (name) DO UPDATE SET phone = EXCLUDED.phone, pass = EXCLUDED.pass";
    let mut res = sqlx::query(&crate::pq::rewrite(UPSERT_NEW))
        .bind(&name)
        .bind(&phone)
        .bind(&pass_hash)
        .bind(&now)
        .execute(pool)
        .await;
    if matches!(&res, Err(e) if e.to_string().contains("Unknown column")) {
        tracing::warn!("039 未跑：管理员设密码时无法清临时密码三列（先跑 pk-server migrate）");
        res = sqlx::query(&crate::pq::rewrite(UPSERT_LEGACY))
            .bind(&name)
            .bind(&phone)
            .bind(&pass_hash)
            .bind(&now)
            .execute(pool)
            .await;
    }
    res.map_err(|e| {
        let m = e.to_string();
        /* docs/12 §P6 记过的不一致：唯一索引已经能挡住重复手机号，但错误一直是
        一句裸 SQL 文案，运营看不出是手机号还是姓名撞了。这里把唯一冲突翻成中文
        （PG SQLSTATE 23505 + 旧 MySQL 文案两种都认）。 */
        if is_unique_violation(&e) || m.contains("Duplicate entry") {
            if m.contains("uk_my_phone_uniq") {
                format!("手机号 {phone} 已存在（已绑在另一个账号上）")
            } else if m.contains("pkey") || m.contains("PRIMARY") {
                format!("账号名「{name}」已存在（同名主播？先在台账里改名）")
            } else {
                format!("写入失败：{m}")
            }
        } else if m.contains("Unknown column") {
            "缺少数据库迁移 008（先跑一次 pk-server migrate）".to_string()
        } else {
            format!("写入失败：{m}")
        }
    })?;
    Ok(())
}

/// 设置账号的「可看团别」（迁移 024）。账号不存在返回 false。
///
/// 注意：改这个等于改账号的数据可见范围，**调用方必须同时吊销该账号的全部
/// my token**（`public_teams::admin::my_tokens_revoke`），否则旧 token 里还挂着
/// 改之前算出来的团别，范围限制当场就被绕过去了。
pub async fn my_account_set_view_team(st: &AppState, name: &str, vt: ViewTeam) -> bool {
    let Ok(pool) = w(st) else {
        return false;
    };
    matches!(
        sqlx::query(&crate::pq::rewrite(
            "UPDATE my_accounts SET view_team = ? WHERE name = ?"
        ))
        .bind(vt.code())
        .bind(name.trim())
        .execute(pool)
        .await,
        Ok(r) if r.rows_affected() > 0
    )
}

/// 读账号的「可看团别」。列不存在 / 账号不存在 / 值非法 → Inherit（旧口径）。
pub async fn my_account_view_team(st: &AppState, name: &str) -> ViewTeam {
    let Ok(pool) = w(st) else {
        return ViewTeam::Inherit;
    };
    sqlx::query(&crate::pq::rewrite(
        "SELECT view_team FROM my_accounts WHERE name = ?",
    ))
    .bind(name.trim())
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .and_then(|r| r.try_get::<String, _>("view_team").ok())
    .map(|v| ViewTeam::parse(&v))
    .unwrap_or(ViewTeam::Inherit)
}

/// 某账号登记的手机号（没登记 = 空串）。展示端用来判断能不能走「发手机号」绑定。
pub async fn my_account_phone(st: &AppState, name: &str) -> String {
    let Ok(pool) = w(st) else {
        return String::new();
    };
    sqlx::query(&crate::pq::rewrite(
        "SELECT phone FROM my_accounts WHERE name = ?",
    ))
    .bind(name.trim())
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .and_then(|r| r.try_get::<String, _>("phone").ok())
    .unwrap_or_default()
}

/// 单个账号（右键弹窗回填用）；没有则 None。
/// 额外带 `bound`/`source`（008 迁移）：管理端据此显示「已绑主播 / 待绑定」；
/// 2026-10-18 起与列表同口径补 `emailMask` / `qqBound` / `mustChangePass`
/// （契约 §3.8「单条 GET 同步返回上述字段」），列缺失时同样三级回落。
pub async fn my_account_get(st: &AppState, name: &str) -> Option<serde_json::Value> {
    let pool = w(st).ok()?;
    let key = name.trim();
    let sqls = [
        "SELECT name, phone, is_admin, created_at, bound, bound_at, source, view_team, email, \
         must_change_pass FROM my_accounts WHERE name = ?",
        "SELECT name, phone, is_admin, created_at, bound, bound_at, source, view_team, email \
         FROM my_accounts WHERE name = ?",
        "SELECT name, phone, is_admin, created_at FROM my_accounts WHERE name = ?",
    ];
    for sql in sqls {
        if let Ok(Some(row)) = sqlx::query(&crate::pq::rewrite(sql))
            .bind(key)
            .fetch_optional(pool)
            .await
        {
            return Some(account_row_json(&row));
        }
    }
    None
}

/// 删除账号，返回是否存在
pub async fn my_account_delete(st: &AppState, name: &str) -> bool {
    matches!(w(st), Ok(pool) if sqlx::query(&crate::pq::rewrite("DELETE FROM my_accounts WHERE name = ?"))
        .bind(name.trim())
        .execute(pool)
        .await
        .map(|r| r.affected() > 0)
        .unwrap_or(false))
}

/// 设置/取消展示端管理员标记（管理员观看账号：不对应台账主播也能登录看全站）
pub async fn my_account_set_admin(st: &AppState, name: &str, admin: bool) -> bool {
    matches!(w(st), Ok(pool) if sqlx::query(&crate::pq::rewrite("UPDATE my_accounts SET is_admin = ? WHERE name = ?"))
        .bind(if admin { 1i16 } else { 0i16 })
        .bind(name.trim())
        .execute(pool)
        .await
        .map(|r| r.affected() > 0)
        .unwrap_or(false))
}

/* ------------------- 主播手机号 + 批量开号（016，docs/13） ------------------- */

/// 后台写手机号时被哪类对象占了。两种都拒绝，但文案不同（运营要靠它判断去哪改）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhoneClash {
    /// 别的在职主播档案（person.phone）上已经登记了这个号
    Person,
    /// 别的展示端账号（my_accounts.phone）已经用这个号登录
    Account,
}

impl PhoneClash {
    pub fn message(self, phone: &str) -> String {
        match self {
            Self::Person => format!("手机号 {phone} 已被其它在职主播登记"),
            Self::Account => format!("手机号 {phone} 已绑在另一个展示端账号上"),
        }
    }
}

/// 初始密码 = **手机号后四位**（2026-10-13 产品拍板恢复）。
///
/// 口径沿革：
/// - 旧口径就是手机号后四位（与运营开号习惯一致）；
/// - 2026-10-12 安全审计改成 CSPRNG 随机（理由：手机号在公会内部半公开，
///   后四位等于把初始口令暴露给知道号码的人）；
/// - 2026-10-13 产品决策恢复后四位——随机 10 位密码运营转达成本高、
///   且主播记不住，反复重置；撞库风险改由登录限流承担。
///
/// 手机号不足 4 位（或拿不到手机号）时退回 CSPRNG 随机，保证总有可用密码。
pub fn initial_password(phone: &str) -> String {
    let p: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
    if p.chars().count() >= 4 {
        let v: Vec<char> = p.chars().collect();
        v[v.len() - 4..].iter().collect()
    } else {
        crate::random::random_password()
    }
}

/// 写入主播档案手机号（`person.phone`，016 迁移新增列）。
///
/// 016 没跑时 `person` 表没有该列 → UPDATE 报 Unknown column，这里翻成明确的
/// 「先跑迁移」提示，不让运营看见一条 `Unknown column 'phone'`。
/// 手机号留空 = 清空（运营登记错了要撤掉）。
pub async fn person_set_phone(
    st: &AppState,
    person_id: u64,
    phone: &str,
) -> std::result::Result<(), String> {
    if person_id == 0 {
        return Err(biz("无效的主播 id"));
    }
    let phone = validate_my_phone(phone)?;
    let pool = w(st).map_err(|e| e.to_string())?;
    if !phone.is_empty() {
        let taken_person = sqlx::query(&crate::pq::rewrite(crate::dialect::PERSON_PHONE_TAKEN_SQL))
            .bind(&phone)
            .bind(person_id as i64)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .is_some();
        if taken_person {
            return Err(biz(PhoneClash::Person.message(&phone)));
        }
        let taken_account = sqlx::query(&crate::pq::rewrite(
            "SELECT 1 FROM my_accounts WHERE phone <> '' AND phone = ? LIMIT 1",
        ))
        .bind(&phone)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .is_some();
        if taken_account {
            return Err(biz(PhoneClash::Account.message(&phone)));
        }
    }
    sqlx::query(&crate::pq::rewrite(
        "UPDATE person SET phone = ? WHERE id = ? AND deleted_at IS NULL",
    ))
    .bind(&phone)
    .bind(person_id as i64)
    .execute(pool)
    .await
    .map_err(|e| {
        let m = e.to_string();
        if m.contains("Unknown column") {
            "主播手机号功能未启用：请先跑一次 pk-server migrate（016_streamer_phone）".into()
        } else if is_unique_violation(&e) || m.contains("Duplicate entry") {
            format!("手机号 {phone} 已被其它主播登记")
        } else {
            format!("写入失败：{m}")
        }
    })?;
    Ok(())
}

/// 批量开号单条结果（前端按它渲染「谁成了、谁没成、为什么」）
#[derive(Clone, Debug, serde::Serialize)]
pub struct BatchAccountRow {
    #[serde(rename = "personId")]
    pub person_id: i64,
    /// 登录名 = 台账姓名
    pub name: String,
    /// 实际写进登录账号的手机号（空 = 只能姓名登录）
    pub phone: String,
    /// created = 新开号 / updated = 已存在（补 person_id，密码看 forceReset）
    /// / failed = 见 note
    /// （历史上有过 skipped = 定不了初始密码；2026-10-13 起初始密码恒为手机号后四位，不再跳过）
    pub status: &'static str,
    pub note: String,
    /// 本次生成的初始密码（created 一定下发；updated 只在 forceReset 时下发）
    #[serde(rename = "initialPass", skip_serializing_if = "Option::is_none")]
    pub initial_pass: Option<String>,
}

/// 批量开号的入参：person_id 必填；phone 留空 = 沿用台账上已登记的手机号。
#[derive(Clone, Debug)]
pub struct BatchAccountInput {
    pub person_id: i64,
    pub phone: Option<String>,
}

/// 一次批量开号最多处理多少人。104 个主播一次开完是 104 行，留到 500 给合并/扩容冗余，
/// 同时防误传一份几万行的名单把写池拖成长事务。
pub const BATCH_ACCOUNT_MAX: usize = 500;

/// 给一批主播开「展示端登录账号」（`POST /api/admin/my-accounts/batch`，docs/13 §3）。
///
/// 逐条独立判定 —— **一条失败不影响其它条**：104 人一次提交，不能因为第 3 个人手机号
/// 重号就让剩下 101 个都开不成。
///
/// - `person_id` 必须对应在职主播（`deleted_at IS NULL AND status <> 'left'`）且姓名非空
/// - 手机号优先级：**入参 phone > 台账 person.phone > 空**
/// - 初始密码：**手机号后四位**（2026-10-13 产品拍板恢复，与运营开号习惯一致）；
///   `default_pass` 入参保留仅为兼容旧调用，**不再使用**（统一口令本身就是审计要消除的风险）
/// - 已存在账号（按 person_id 优先、name 兜底）→ 更新：补 person_id / 手机号，**默认不动密码**
///   （重跑一遍批量不会把 104 个人的密码全刷掉）；`force_reset = true` 才重置（重置为手机号后四位）
/// - source 一律标 `batch`（与管理端手建的 `admin`、自助注册的 `register` 区分，便于事后审计）
pub async fn my_accounts_batch_open(
    st: &AppState,
    inputs: &[BatchAccountInput],
    default_pass: Option<&str>,
    force_reset: bool,
) -> std::result::Result<Vec<BatchAccountRow>, String> {
    // 审计 2026-10-12：统一兜底口令废弃（知道口令就能登所有没改密的账号），入参不再读取
    let _ = default_pass;
    let pool = w(st).map_err(|e| e.to_string())?;
    if inputs.is_empty() {
        return Err(biz("名单为空"));
    }
    if inputs.len() > BATCH_ACCOUNT_MAX {
        return Err(biz(format!(
            "一次最多开 {} 个账号（本次 {} 个，请分批提交）",
            BATCH_ACCOUNT_MAX,
            inputs.len()
        )));
    }
    /* 016 没跑时 person.phone 不存在 → 退回 LEGACY 列清单（手机号一律当"未登记"）。
    批量开号是 104 人的唯一入口，不能因为少跑一次迁移就整个不可用。 */
    let roster_rows = match crate::pgrs::query(
        &st.pools,
        "LOCAL",
        crate::dialect::PERSON_ROSTER_FOR_ACCOUNTS_SQL,
    )
    .await
    {
        Ok(rows) => rows,
        Err(e)
            if {
                let m = e.to_string();
                m.contains("Unknown column") || m.contains("doesn't exist")
            } =>
        {
            tracing::warn!("person.phone 列不存在（016 未跑），批量开号按无手机号口径继续");
            crate::pgrs::query(
                &st.pools,
                "LOCAL",
                crate::dialect::PERSON_ROSTER_FOR_ACCOUNTS_SQL_LEGACY,
            )
            .await
            .map_err(|e| format!("读取主播台账失败：{e}"))?
        }
        Err(e) => return Err(format!("读取主播台账失败：{e}")),
    };
    let roster: Vec<(i64, String, String)> = roster_rows
        .iter()
        .map(|r| {
            (
                r.try_get::<i64, _>("id").unwrap_or(0),
                r.try_get::<String, _>("name").unwrap_or_default(),
                r.try_get::<String, _>("phone").unwrap_or_default(),
            )
        })
        .collect();

    /* 三次查询把占用情况捞全，避免逐条 SELECT（104 人 × 2 次会明显拖慢接口）。
    016 没跑时 person.phone 不存在 → 台账侧直接当「都没登记」，手机号只按 my_accounts 判重。 */
    let mut person_phone_dup: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Ok(rows) = sqlx::query(
        "SELECT phone FROM person WHERE phone <> '' AND deleted_at IS NULL
          GROUP BY phone HAVING COUNT(*) > 1",
    )
    .fetch_all(pool)
    .await
    {
        for r in &rows {
            if let Ok(p) = r.try_get::<String, _>("phone") {
                person_phone_dup.insert(p);
            }
        }
    }
    // 台账上「谁的号」：手机号 → person_id（用来判断撞的到底是别人还是自己）
    let mut person_phone_owner: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();
    if let Ok(rows) = sqlx::query(
        "SELECT phone, id FROM person WHERE phone <> '' AND deleted_at IS NULL
          GROUP BY phone, id",
    )
    .fetch_all(pool)
    .await
    {
        for r in &rows {
            if let (Ok(p), Ok(id)) = (r.try_get::<String, _>("phone"), r.try_get::<i64, _>("id")) {
                person_phone_owner.insert(p, id);
            }
        }
    }
    // 登录账号侧：手机号 → 账号名（含 person_id，用于「改名后 name 对不上」的判定）
    let mut account_phone: Vec<(String, String, i64)> = Vec::new();
    if let Ok(rows) = sqlx::query(
        "SELECT name, phone, COALESCE(person_id, 0) AS pid FROM my_accounts WHERE phone <> ''",
    )
    .fetch_all(pool)
    .await
    {
        for r in &rows {
            if let (Ok(n), Ok(p)) = (
                r.try_get::<String, _>("name"),
                r.try_get::<String, _>("phone"),
            ) {
                let pid = r.try_get::<i64, _>("pid").unwrap_or(0);
                account_phone.push((n, p, pid));
            }
        }
    }
    // 已开号的主播：person_id → (账号名, 手机号)
    let mut account_by_person: std::collections::HashMap<i64, (String, String)> =
        std::collections::HashMap::new();
    if let Ok(rows) = sqlx::query(
        "SELECT name, phone, person_id FROM my_accounts
          WHERE person_id > 0 ORDER BY person_id, name",
    )
    .fetch_all(pool)
    .await
    {
        for r in &rows {
            if let (Ok(n), Ok(p), Ok(pid)) = (
                r.try_get::<String, _>("name"),
                r.try_get::<String, _>("phone"),
                r.try_get::<i64, _>("person_id"),
            ) {
                account_by_person.entry(pid).or_insert((n, p));
            }
        }
    }
    /* ⚠️ 上面 `account_phone` 只捞了 `phone <> ''` 的行 —— **没登记手机号的账号捞不到**。
    「这个主播/这个姓名有没有账号」必须看全部账号行，否则：
    无手机号的已开号主播（批量开号第一批里没填号的人）再跑一次会走到 INSERT，
    PRIMARY KEY(name) 直接撞车 → 该条报 failed，运营看到"已存在"却开不了号。 */
    let mut account_by_name: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    if let Ok(rows) = sqlx::query(&crate::pq::rewrite("SELECT name, phone FROM my_accounts"))
        .fetch_all(pool)
        .await
    {
        for r in &rows {
            if let (Ok(n), Ok(p)) = (
                r.try_get::<String, _>("name"),
                r.try_get::<String, _>("phone"),
            ) {
                account_by_name.insert(n, p);
            }
        }
    }

    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut out = Vec::with_capacity(inputs.len());
    for input in inputs {
        let pid = input.person_id;
        let Some((_, name, person_phone)) = roster.iter().find(|(id, _, _)| *id == pid) else {
            out.push(BatchAccountRow {
                person_id: pid,
                name: String::new(),
                phone: String::new(),
                status: "failed",
                note: "主播不存在、已删除或已离团（先生效再开号）".into(),
                initial_pass: None,
            });
            continue;
        };
        let name = name.clone();
        if name.is_empty() {
            out.push(BatchAccountRow {
                person_id: pid,
                name,
                phone: String::new(),
                status: "failed",
                note: "台账姓名为空，无法作为登录名".into(),
                initial_pass: None,
            });
            continue;
        }

        /* 手机号：入参优先，其次台账已登记的 */
        let want_phone = match input
            .phone
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(p) => match validate_my_phone(p) {
                Ok(p) => p,
                Err(e) => {
                    out.push(BatchAccountRow {
                        person_id: pid,
                        name,
                        phone: String::new(),
                        status: "failed",
                        note: e,
                        initial_pass: None,
                    });
                    continue;
                }
            },
            None => person_phone.clone(),
        };

        /* 初始密码 = 手机号后四位（2026-10-13 恢复；手机号不足 4 位时退回随机）。
        只在响应里下发一次（initialPass），请管理员当场转达。 */
        let pass = initial_password(&want_phone);
        if let Err(e) = validate_my_account(&name, &pass) {
            // note 直接展示给运营（批量开号结果表），剥掉 BIZ:: 内部标记
            out.push(BatchAccountRow {
                person_id: pid,
                name,
                phone: want_phone,
                status: "failed",
                note: biz_msg(&e).to_string(),
                initial_pass: None,
            });
            continue;
        }

        /* 手机号占用：撞台账（别人的档案）/ 撞登录账号（别人的号）都拒绝 */
        if !want_phone.is_empty() {
            let dup_in_person = person_phone_dup.contains(&want_phone);
            let owned_by_other = person_phone_owner
                .get(&want_phone)
                .is_some_and(|owner| *owner != pid);
            if dup_in_person || owned_by_other {
                out.push(BatchAccountRow {
                    person_id: pid,
                    name,
                    phone: want_phone.clone(),
                    status: "failed",
                    note: PhoneClash::Person.message(&want_phone),
                    initial_pass: None,
                });
                continue;
            }
            let clash_account = account_phone
                .iter()
                .find(|(_, p, apid)| *p == want_phone && *apid != pid)
                .map(|(n, _, _)| n.clone());
            if let Some(owner) = clash_account {
                if account_by_name.get(&name).map(|p| p.as_str()) != Some(want_phone.as_str()) {
                    out.push(BatchAccountRow {
                        person_id: pid,
                        name,
                        phone: want_phone.clone(),
                        status: "failed",
                        note: format!("手机号 {want_phone} 已绑在「{owner}」的账号上"),
                        initial_pass: None,
                    });
                    continue;
                }
            }
        }

        let pass_hash = hash_my_password(&pass);
        /* 已存在账号？person_id 优先（015 锚点口径），name 兜底（未回填 / 015 没跑） */
        let existing = account_by_person.get(&pid).cloned().or_else(|| {
            account_by_name
                .get(&name)
                .map(|p| (name.clone(), p.clone()))
        });
        match existing {
            Some((old_name, old_phone)) => {
                let keep_phone = if want_phone.is_empty() {
                    old_phone
                } else {
                    want_phone.clone()
                };
                // 已存在：默认**不动密码**（重跑批量不该把所有人密码刷掉），
                // 只有 force_reset 才重置；手机号 / person_id 一律补齐。
                // 039：force_reset 写新密码后必须清临时密码三列（契约 §2.7），
                // 否则账号会卡在「已改密但仍要求改密」的假锁。
                let sql = crate::pq::rewrite(if force_reset {
                    "UPDATE my_accounts SET phone = ?, pass = ?, person_id = ?, bound = 1, \
                       temp_pass_hash = '', temp_pass_exp = NULL, must_change_pass = 0 \
                     WHERE name = ?"
                } else {
                    "UPDATE my_accounts SET phone = ?, person_id = ?, bound = 1 WHERE name = ?"
                });
                let mut q = sqlx::query(&sql).bind(&keep_phone);
                if force_reset {
                    q = q.bind(&pass_hash);
                }
                let mut res = q.bind(pid as i64).bind(&old_name).execute(pool).await;
                if force_reset && matches!(&res, Err(e) if e.to_string().contains("Unknown column"))
                {
                    tracing::warn!("039 未跑：批量 force_reset 无法清临时密码三列");
                    res = sqlx::query(&crate::pq::rewrite(
                        "UPDATE my_accounts SET phone = ?, pass = ?, person_id = ?, bound = 1 \
                         WHERE name = ?",
                    ))
                    .bind(&keep_phone)
                    .bind(&pass_hash)
                    .bind(pid as i64)
                    .bind(&old_name)
                    .execute(pool)
                    .await;
                }
                match res {
                    Ok(_) => out.push(BatchAccountRow {
                        person_id: pid,
                        name: old_name,
                        phone: keep_phone,
                        status: "updated",
                        note: if force_reset {
                            "已存在账号：已补 person_id 并重置初始密码".into()
                        } else {
                            "已存在账号：已补 person_id（密码未改动）".into()
                        },
                        initial_pass: force_reset.then_some(pass),
                    }),
                    Err(e) => {
                        let m = e.to_string();
                        out.push(BatchAccountRow {
                            person_id: pid,
                            name: old_name,
                            phone: keep_phone.clone(),
                            status: "failed",
                            note: if is_unique_violation(&e) || m.contains("Duplicate entry") {
                                format!("手机号 {keep_phone} 已被占用")
                            } else {
                                format!("写入失败：{m}")
                            },
                            initial_pass: None,
                        })
                    }
                }
            }
            None => {
                /* 新建：开号即绑定（bound=1、name = 台账姓名、person_id 锚定），
                与单条 my_account_set 同口径 —— 主播拿到号就能登，不用再绑一次。 */
                /* 2026-10-13 审计修复：先查后插存在并发竞态（双跑时预查"不存在"而
                INSERT 撞主键导致整批部分失败）。改 ON CONFLICT (name) 幂等覆盖，
                与单条 my_account_set 同口径；手机号唯一索引冲突仍会被后面的
                match Err 分支捕获并逐行报「手机号已存在」。 */
                let mut res = sqlx::query(&crate::pq::rewrite(
                    "INSERT INTO my_accounts
                       (name, phone, pass, is_admin, created_at, bound, bound_at, source, person_id)
                     VALUES (?, ?, ?, 0, ?, 1, ?, 'batch', ?)
                     ON CONFLICT (name) DO UPDATE SET phone = EXCLUDED.phone,
                       pass = EXCLUDED.pass, person_id = EXCLUDED.person_id,
                       temp_pass_hash = '', temp_pass_exp = NULL, must_change_pass = 0",
                ))
                .bind(&name)
                .bind(&want_phone)
                .bind(&pass_hash)
                .bind(&now)
                .bind(&now)
                .bind(pid as i64)
                .execute(pool)
                .await;
                if matches!(&res, Err(e) if e.to_string().contains("Unknown column")) {
                    tracing::warn!("039 未跑：批量开号 ON CONFLICT 分支无法清临时密码三列");
                    res = sqlx::query(&crate::pq::rewrite(
                        "INSERT INTO my_accounts
                           (name, phone, pass, is_admin, created_at, bound, bound_at, source, person_id)
                         VALUES (?, ?, ?, 0, ?, 1, ?, 'batch', ?)
                         ON CONFLICT (name) DO UPDATE SET phone = EXCLUDED.phone,
                           pass = EXCLUDED.pass, person_id = EXCLUDED.person_id",
                    ))
                    .bind(&name)
                    .bind(&want_phone)
                    .bind(&pass_hash)
                    .bind(&now)
                    .bind(&now)
                    .bind(pid as i64)
                    .execute(pool)
                    .await;
                }
                match res {
                    Ok(_) => out.push(BatchAccountRow {
                        person_id: pid,
                        name,
                        phone: want_phone,
                        status: "created",
                        note: String::new(),
                        initial_pass: Some(pass),
                    }),
                    Err(e) => {
                        let m = e.to_string();
                        let note = if is_unique_violation(&e) || m.contains("Duplicate entry") {
                            if m.contains("pkey") || m.contains("PRIMARY") {
                                format!("账号名「{name}」已存在（同名主播？先改名或走单人改绑）")
                            } else {
                                format!("手机号 {want_phone} 已被占用")
                            }
                        } else if m.contains("Unknown column") {
                            "缺少数据库迁移 008/015（先跑一次 pk-server migrate）".into()
                        } else {
                            format!("写入失败：{m}")
                        };
                        out.push(BatchAccountRow {
                            person_id: pid,
                            name,
                            phone: want_phone,
                            status: "failed",
                            note,
                            initial_pass: None,
                        })
                    }
                }
            }
        }
    }
    Ok(out)
}

/// 是否展示端管理员账号（verify 在台账里找不到同名主播时据此放行）
pub async fn my_account_is_admin(st: &AppState, name: &str) -> bool {
    let Ok(pool) = w(st) else {
        return false;
    };
    matches!(
        sqlx::query(&crate::pq::rewrite("SELECT is_admin FROM my_accounts WHERE name = ?"))
            .bind(name.trim())
            .fetch_optional(pool)
            .await,
        Ok(Some(r)) if r.try_get::<i16, _>(0).unwrap_or(0) == 1
    )
}

/* ------------------------- 内置展示端账号 ------------------------- */

/// 内置展示端账号改从环境变量读（P1-2 修，2026-10-10）：
/// `BUILTIN_MY_ACCOUNTS="name:pass,name2:pass2"`（账号与密码一样时写 `name:` 即可，
/// 密码缺省 = 账号本身）。`is_admin=1`（管理员观看账号：台账里没有同名主播也放行，
/// 全站看得到、个人数据页为空）。
///
/// **2026-10-17 扩展：可选第三段 `viewTeam`** —— `name:pass:viewTeam`，
/// 给管理员账号指定「能看哪个团」：
///   - `any` / 空 = 任意团（总管理员口径，如 浩雨、鹏先生）
///   - `male`  = 只看男团
///   - `female`= 只看女团（如 饭团）
/// 不写第三段 = 保持旧行为（view_team 留空 = 沿用旧口径，按绑定主播所在团）。
/// 这样三位管理员各自的可见范围能一次配到位，不用每次启动后手工改库。
///
/// **未配置环境变量时一个账号都不创建**（默认安全）：以前硬编码 shuai/shuai 随启动重建，
/// 口令公开可猜 → 展示端登录门形同虚设。库里的存量账号不受影响，删除走管理端。
pub fn builtin_my_accounts_from_env() -> Vec<(String, String, String)> {
    let raw = std::env::var("BUILTIN_MY_ACCOUNTS").unwrap_or_default();
    let mut out = Vec::new();
    for pair in raw.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let mut parts = pair.splitn(3, ':');
        let name = parts.next().unwrap_or("").trim();
        let pass = parts.next().unwrap_or("").trim();
        let view_team = parts.next().unwrap_or("").trim();
        if name.is_empty() {
            continue;
        }
        // 密码没写 = 账号密码相同（历史行为），但空密码不行（my_accounts.pass 非空）
        let pass = if pass.is_empty() { name } else { pass };
        // 只看团别做白名单校验，非法/未知值一律按「任意团」处理（管理员不该被关死）
        let view_team = match view_team.to_ascii_lowercase().as_str() {
            "male" => "male",
            "female" => "female",
            // 显式写了 any / 未写第三段 → 'any'（全团，且允许页内切团），
            // 比旧口径的 '' 更明确（见迁移 024 的取值说明）
            _ => "any",
        };
        out.push((name.to_string(), pass.to_string(), view_team.to_string()));
    }
    out
}

/// 启动时补齐内置账号（幂等）：
/// - 同名账号已存在 → 只把 `is_admin` 拉回 1，**密码保持运营自己设置的样子**，不覆盖
/// - 密码被别的姓名占用（`uk_my_pass` 1:1 唯一键）→ 跳过并告警，不能顶掉别人的登录
/// - 表还不存在（没跑过 migrate）→ 告警后跳过，不影响服务启动
///
/// 返回 (新建数, 跳过数)。
pub async fn ensure_builtin_my_accounts(st: &AppState) -> (usize, usize) {
    let accounts = builtin_my_accounts_from_env();
    if accounts.is_empty() {
        tracing::info!(
            "未配置 BUILTIN_MY_ACCOUNTS：不创建内置展示端账号（登录门账号由管理端维护）"
        );
        return (0, 0);
    }
    let Ok(pool) = w(st) else {
        tracing::warn!("内置展示端账号未写入：写库不可用（douyin_writer 未配置）");
        return (0, accounts.len());
    };
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut created = 0usize;
    let mut skipped = 0usize;

    for (name, pass, view_team) in &accounts {
        let name = name.as_str();
        let pass = pass.as_str();
        let exists = match sqlx::query(&crate::pq::rewrite(
            "SELECT name FROM my_accounts WHERE name = ?",
        ))
        .bind(name)
        .fetch_optional(pool)
        .await
        {
            Ok(row) => row.is_some(),
            Err(e) => {
                tracing::warn!(error = %e, name, "内置展示端账号查不到（表还没建？先跑 pk-server migrate）");
                skipped += 1;
                continue;
            }
        };
        if exists {
            /* 已存在：把 is_admin 拉回 1，并按环境变量**同步 view_team**
            （2026-10-17：饭团之类「只看女团」的口径要能靠配置修回来，
            否则运营在界面上改错了、重启服务也修不回来）。
            空串 = 任意团，也会一并写回（覆盖成任意团）。 */
            if let Err(e) = sqlx::query(&crate::pq::rewrite(
                "UPDATE my_accounts SET is_admin = 1, view_team = ? WHERE name = ?",
            ))
            .bind(view_team)
            .bind(name)
            .execute(pool)
            .await
            {
                tracing::warn!(error = %e, name, "内置展示端账号管理员标记更新失败");
                skipped += 1;
            }
            continue;
        }

        // 密码 1:1：已被别的姓名占用就不能插（否则会顶掉那个人的登录）
        match sqlx::query(&crate::pq::rewrite(
            "INSERT INTO my_accounts (name, phone, pass, is_admin, created_at, view_team)
             VALUES (?, '', ?, 1, ?, ?)",
        ))
        .bind(name)
        .bind(hash_my_password(pass))
        .bind(&now)
        .bind(view_team)
        .execute(pool)
        .await
        {
            Ok(_) => {
                created += 1;
                tracing::info!(name, "已写入内置展示端账号（管理员观看账号）");
            }
            Err(e) => {
                tracing::warn!(error = %e, name, "内置展示端账号写入失败");
                skipped += 1;
            }
        }
    }
    (created, skipped)
}

/// 手机号归一化：去空格 / 横线 / 点，与 my_account_set 的写入口径一致
pub fn normalize_phone(v: &str) -> String {
    v.chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != '.')
        .collect()
}

/// 展示端登录失败的原因（2026-10-14 用户口径：失败要说明白是哪一种）。
///
/// 原先登录失败一律返回 `None`，前端只能把「密码错 / 账号不存在 / 没开通」揉成一句
/// 「账号或密码不对，或账号还没开通」。现在把原因传出去，前端照实显示 ——
/// 代价是账号枚举面变大（能拿姓名试探谁开了号），产品已确认接受。
///
/// 2026-10-18 新增 [`LoginFail::TempExpired`]（临时密码口径）：**先验签、再判过期** ——
/// 只有真正持有那个临时密码的人才会看到「已过期」，不向旁人泄露「这个账号有待用
/// 临时密码」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginFail {
    /// 账号（或手机号）查得到，但密码不对
    BadPassword,
    /// 这个账号 / 手机号根本没开通过
    NoAccount,
    /// 临时密码哈希验签通过，但已过 15 分钟有效期
    TempExpired,
}

impl LoginFail {
    /// 给前端直接显示的文案（登录失败只有这几种，不再含糊）
    pub fn message(self) -> &'static str {
        match self {
            LoginFail::BadPassword => "密码错误",
            LoginFail::NoAccount => "账号还没开通",
            LoginFail::TempExpired => "临时密码已过期，请重新获取",
        }
    }

    /// 契约 §3.1 的错误码（handler 直接下发给前端分流 UI）
    pub fn code(self) -> &'static str {
        match self {
            LoginFail::BadPassword => "BAD_PASSWORD",
            LoginFail::NoAccount => "NO_ACCOUNT",
            LoginFail::TempExpired => "TEMP_EXPIRED",
        }
    }
}

/// 登录成功的结果（契约 docs/26/05 §2.4）。
///
/// 从 2026-10-18 起来落 handler 需要的全部身份信息，避免 handler 再查一遍：
/// - `person_id`：`my_accounts.person_id`（0 = 管理员观看账号 / 未绑主播）；
/// - `team`：台账主播 `gender`（未绑 → 空串），handler 用它 + view_team 算 token 团别；
/// - `temp_used`：本次是否凭**临时密码**登录（handler 据此回 `mustChangePass:true`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginOutcome {
    pub name: String,
    pub person_id: i64,
    pub team: String,
    /// true = 这次用的是邮箱里那串临时密码
    pub temp_used: bool,
}

/// 临时密码校验结果（契约 §2.4）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TempPassResult {
    /// 验签通过且在有效期内（已置 must_change_pass=1；**不清 temp**，15 分钟内可重复用）
    Valid,
    /// 验签通过但已过期（已惰性清空 temp 两列）
    Expired,
    /// 没有临时密码 / 密码不对（不区分，避免泄露「有待用临时密码」）
    Mismatch,
}

/// 登录用账号行：一次查出密码、person_id、团别与 039 的临时密码列。
///
/// `where_sql` 是**代码里写死的**片段（`m.phone = ?` / `m.name = ?`），不接收外部输入。
/// 039 未跑时整句失败 → 回落老列查询（临时密码当不存在，登录照常可用），
/// 与 038 在 `my_account_info` 里的回落策略同口径 —— 不让没迁移的库整体登不进去。
async fn login_row(pool: &PgPool, where_sql: &str, key: &str) -> Option<sqlx::postgres::PgRow> {
    let sql = format!(
        "SELECT m.name, m.pass, m.person_id, COALESCE(p.gender, '') AS team, \
                m.temp_pass_hash, m.must_change_pass, \
                (m.temp_pass_exp IS NOT NULL AND m.temp_pass_exp > now()) AS temp_valid \
         FROM my_accounts m LEFT JOIN person p ON p.id = m.person_id AND p.deleted_at IS NULL \
         WHERE {where_sql} LIMIT 1"
    );
    match sqlx::query(&crate::pq::rewrite(&sql))
        .bind(key)
        .fetch_optional(pool)
        .await
    {
        Ok(row) => row,
        Err(e) => {
            tracing::warn!(error = %e, "含 039 列的登录查询失败，回落老列（临时密码不可用）");
            let sql = format!(
                "SELECT m.name, m.pass, m.person_id, COALESCE(p.gender, '') AS team \
                 FROM my_accounts m LEFT JOIN person p ON p.id = m.person_id AND p.deleted_at IS NULL \
                 WHERE {where_sql} LIMIT 1"
            );
            sqlx::query(&crate::pq::rewrite(&sql))
                .bind(key)
                .fetch_optional(pool)
                .await
                .ok()
                .flatten()
        }
    }
}

/// 置「必须改密」标记（临时密码登录命中）。039 未跑时静默跳过（无列可写）。
async fn set_must_change_pass(pool: &PgPool, name: &str) -> std::result::Result<(), String> {
    match sqlx::query(&crate::pq::rewrite(
        "UPDATE my_accounts SET must_change_pass = 1 WHERE name = ?",
    ))
    .bind(name)
    .execute(pool)
    .await
    {
        Ok(_) => Ok(()),
        Err(e) if e.to_string().contains("Unknown column") => {
            tracing::warn!("039 未跑：无法置 must_change_pass（先跑 pk-server migrate）");
            Ok(())
        }
        Err(e) => Err(format!("更新改密标记失败：{e}")),
    }
}

/// 只清临时密码两列（保留 must_change_pass）—— 过期惰性清理用。
async fn clear_temp_hash_only(pool: &PgPool, name: &str) -> std::result::Result<(), String> {
    match sqlx::query(&crate::pq::rewrite(
        "UPDATE my_accounts SET temp_pass_hash = '', temp_pass_exp = NULL WHERE name = ?",
    ))
    .bind(name)
    .execute(pool)
    .await
    {
        Ok(_) => Ok(()),
        Err(e) if e.to_string().contains("Unknown column") => Ok(()),
        Err(e) => Err(format!("清空临时密码失败：{e}")),
    }
}

/// 清临时密码三列（含 must_change_pass）—— 改密成功 / 管理员改密 / 解绑 QQ 用。
async fn clear_temp_pool(pool: &PgPool, name: &str) -> std::result::Result<(), String> {
    match sqlx::query(&crate::pq::rewrite(
        "UPDATE my_accounts SET temp_pass_hash = '', temp_pass_exp = NULL, must_change_pass = 0 \
         WHERE name = ?",
    ))
    .bind(name)
    .execute(pool)
    .await
    {
        Ok(_) => Ok(()),
        Err(e) if e.to_string().contains("Unknown column") => {
            tracing::warn!("039 未跑：无法清临时密码三列（先跑 pk-server migrate）");
            Ok(())
        }
        Err(e) => Err(format!("清空临时密码失败：{e}")),
    }
}

/// 已取到账号行后的登录判定（手机号路径与姓名路径共用）。
///
/// 契约 §2.4 的顺序：**先常密码，失败再试临时密码**；
/// - 常密码命中 → 顺带清临时态（用户已持有当前密码，不该再被要求改密）；
/// - 常密码失败、临时密码验签通过且未过期 → 置 must_change_pass=1，
///   **不清 temp**（15 分钟内可重复用，避免「手滑刷新就锁死」）；
/// - 验签通过但过期 → 惰性清 temp 两列 + `TempExpired`；
/// - 都不匹配 → `BadPassword`（不暴露「有临时密码待用」）。
async fn login_from_row(
    pool: &PgPool,
    row: &sqlx::postgres::PgRow,
    pass: &str,
) -> std::result::Result<LoginOutcome, LoginFail> {
    let name: String = row.try_get("name").map_err(|_| LoginFail::NoAccount)?;
    let stored: String = row.try_get("pass").unwrap_or_default();
    let person_id: i64 = row.try_get("person_id").unwrap_or(0);
    let team: String = row.try_get("team").unwrap_or_default();
    // 039 列缺失（回落老查询）时一律按「没有临时密码」处理
    let temp_hash: String = row.try_get("temp_pass_hash").unwrap_or_default();
    let temp_valid: bool = row.try_get("temp_valid").unwrap_or(false);
    let must_change: i16 = row.try_get("must_change_pass").unwrap_or(0);

    if !stored.is_empty() && verify_my_password(pass, &stored) {
        upgrade_my_password_if_plain(pool, &name, &stored, pass).await;
        /* 常密码命中：顺手清临时态（设计 02 T15）。重置会把 pass 覆盖成不可知
        哈希，所以这里基本只会命中「管理员改过密但没清临时态」的残留；
        清掉不会把用户挡在门外，反而避免挂着「必须改密」的假锁。 */
        if !temp_hash.is_empty() || must_change == 1 {
            if let Err(e) = clear_temp_pool(pool, &name).await {
                tracing::warn!(error = %e, name, "常密码登录后清临时态失败（不阻断登录）");
            }
        }
        return Ok(LoginOutcome {
            name,
            person_id,
            team,
            temp_used: false,
        });
    }

    if !temp_hash.is_empty() && verify_my_password(pass, &temp_hash) {
        if temp_valid {
            /* 置「必须改密」标记失败不阻断登录：用户拿的是有效临时密码，
            一次 DB 抖动不该把人挡在门外；只留 WARN 供排查。 */
            if let Err(e) = set_must_change_pass(pool, &name).await {
                tracing::warn!(error = %e, name, "临时密码登录后置 must_change_pass 失败");
            }
            return Ok(LoginOutcome {
                name,
                person_id,
                team,
                temp_used: true,
            });
        }
        let _ = clear_temp_hash_only(pool, &name).await;
        return Err(LoginFail::TempExpired);
    }

    Err(LoginFail::BadPassword)
}

/// 展示端登录（2026-10-10 起支持手机号+密码，姓名+密码继续兼容）：
/// 输入「账号」先按手机号查（纯数字 5~20 位才当手机号），没命中再按姓名查；
/// 密码命中后返回 [`LoginOutcome`]（含 person_id / team / 是否临时密码登录）。
///
/// **密码不 trim**（2026-10-18 口径）：首尾空格是密码的一部分。
pub async fn my_account_check_login(
    st: &AppState,
    account: &str,
    pass: &str,
) -> std::result::Result<LoginOutcome, LoginFail> {
    let pool = w(st).map_err(|_| LoginFail::NoAccount)?;
    if pass.is_empty() {
        return Err(LoginFail::NoAccount);
    }
    let acct = account.trim();
    if acct.is_empty() {
        return Err(LoginFail::NoAccount);
    }
    // 1) 手机号路径：纯数字 5~20 位
    let phone = normalize_phone(acct);
    let as_phone = phone.chars().all(|c| c.is_ascii_digit()) && (5..=20).contains(&phone.len());
    if as_phone {
        if let Some(row) = login_row(pool, "m.phone = ?", &phone).await {
            /* 手机号查得到账号、密码不对 —— 明确是密码错，不再走姓名分支
            （防止同名艺名串号）。 */
            return login_from_row(pool, &row, pass).await;
        }
    }
    // 2) 姓名（艺名）路径
    match login_row(pool, "m.name = ?", acct).await {
        Some(row) => login_from_row(pool, &row, pass).await,
        None => Err(LoginFail::NoAccount),
    }
}

/// 临时密码校验（契约 §2.4）：合法且未过期 → 置 must_change_pass=1 并返回 `Valid`
/// （**不清 temp**，15 分钟内可重复用）；已过期 → 惰性清空 temp 两列并返回 `Expired`；
/// 不匹配/没有临时密码 → `Mismatch`。`Err` 只用于库层故障。
pub async fn my_account_temp_pass_try(
    st: &AppState,
    name: &str,
    input: &str,
) -> std::result::Result<TempPassResult, String> {
    let pool = w(st).map_err(|e| e.to_string())?;
    temp_pass_try_pool(pool, name, input).await
}

/// [`my_account_temp_pass_try`] 的池版本（登录链路直接复用，少一次 AppState 解包）。
async fn temp_pass_try_pool(
    pool: &PgPool,
    name: &str,
    input: &str,
) -> std::result::Result<TempPassResult, String> {
    let key = name.trim();
    if key.is_empty() || input.is_empty() {
        return Ok(TempPassResult::Mismatch);
    }
    let row = match sqlx::query(&crate::pq::rewrite(
        "SELECT temp_pass_hash, (temp_pass_exp IS NOT NULL AND temp_pass_exp > now()) AS temp_valid \
         FROM my_accounts WHERE name = ? LIMIT 1",
    ))
    .bind(key)
    .fetch_optional(pool)
    .await
    {
        Ok(row) => row,
        // 039 未跑：没有临时密码这回事，按 Mismatch 处理（不把登录链路拖挂）
        Err(e) if e.to_string().contains("Unknown column") => {
            tracing::warn!("039 未跑：临时密码校验不可用（先跑 pk-server migrate）");
            return Ok(TempPassResult::Mismatch);
        }
        Err(e) => return Err(format!("查临时密码失败：{e}")),
    };
    let Some(row) = row else {
        return Ok(TempPassResult::Mismatch);
    };
    let hash: String = row.try_get("temp_pass_hash").unwrap_or_default();
    if hash.is_empty() || !verify_my_password(input, &hash) {
        return Ok(TempPassResult::Mismatch);
    }
    let valid: bool = row.try_get("temp_valid").unwrap_or(false);
    if valid {
        set_must_change_pass(pool, key).await?;
        Ok(TempPassResult::Valid)
    } else {
        clear_temp_hash_only(pool, key).await?;
        Ok(TempPassResult::Expired)
    }
}

/// 清空临时密码与「必须改密」标记（改密成功 / 管理员改密 / 解绑 QQ 时调用）。
pub async fn my_account_clear_temp(st: &AppState, name: &str) -> std::result::Result<(), String> {
    let pool = w(st).map_err(|e| e.to_string())?;
    clear_temp_pool(pool, name.trim()).await
}

/* ------------- 展示端账号：绑定态 / 密码管理（2026-10-10 用户口径） ------------- */

/// 展示端账号的绑定态。`name` 既是登录名也是主播姓名：
/// 注册出来的账号还没绑主播，`name` 先是手机号占位（PRIMARY KEY 不能为空），绑定时改写。
#[derive(Clone, Debug, serde::Serialize)]
pub struct MyAccountInfo {
    pub name: String,
    pub phone: String,
    pub is_admin: bool,
    /// 1 = 已绑定主播（管理员手建的老账号默认 1）
    pub bound: bool,
    pub bound_at: String,
    /// 账号来源（008 迁移新增，只做事后审计用，不参与鉴权）。取值沿革：
    /// - `admin`：管理端手建 / 单条 `my_account_set`（008 之前的老账号也归这一档）；
    /// - `batch`：批量开号（`my_accounts_batch_open`）；
    /// - `register`：手机号自助注册 —— **2026-10-14 注册功能已移除**，新账号不会再写入
    ///   这个值，存量行保留原值以便追溯（不要按它做分支判断）。
    pub source: String,
    /// 可看团别（迁移 024）。库里列可能不存在（没跑到迁移）或值非法，
    /// 一律按 [`ViewTeam::Inherit`] 处理——退回「按绑定主播走」的旧行为。
    pub view_team: ViewTeam,
    /// 绑定邮箱（迁移 038，2026-10-17）。空串 = 未绑定。忘记密码重置时
    /// 有邮箱就把随机密码发过去，没有就提示找管理员。
    pub email: String,
}

impl Default for ViewTeam {
    fn default() -> Self {
        ViewTeam::Inherit
    }
}

impl MyAccountInfo {
    /// 没绑主播时别把手机号当姓名下发（水印/我的页都按这个判）
    pub fn bind_name(&self) -> String {
        if self.bound {
            self.name.clone()
        } else {
            String::new()
        }
    }
}

/* 2026-10-14 注册功能移除：`validate_my_code`（旧「短信验证码就是密码」的 6~20 位数字
格式校验）随注册一起删除 —— 它已无生产调用点，唯一的断言在 store/mod.rs 的单测里，
那个用例也一并删掉了。 */

/* ---------- 忘记密码（2026-10-18 用户拍板：手机号 + QQ 双因子 → 临时密码） ---------- */

/// 忘记密码**第一步**：按「手机号 + QQ 号」定位要重置的普通账号与其绑定邮箱。
///
/// 契约 docs/26/05 §2.5：先用手机号定位 `is_admin = 0` 的账号；库里 `email` 必须
/// 非空且等于 `qq_to_email(qq)`。**任何不匹配都返回同一句** [`MSG_RESET_MISMATCH`]
/// （账号不存在 / 未绑 QQ / QQ 不匹配 / 是管理员账号 / QQ 号格式非法）——
/// 手机号存在性不外泄；只有同时知道正确 QQ 号才会拿到 `Ok`，等价于双因子已匹配。
///
/// 返回 `(账号名, 库里存的邮箱)`：**发信必须发这个返回值里的邮箱**，不能发请求里
/// 原样传入的字符串（防「输入 A、发到 B」与 SMTP 头注入）。
///
/// 残留风险如实记录：本入口（`/api/public/my/password/reset`）没有持有证明 ——
/// 知道手机号 + QQ 号即可触发重置（知识因子，非短信/OAuth）。刷重置由重置限流
/// （12/分/IP + 同号 10 分钟窗口 / 1 小时 3 次）承担；登录链路自 2026-10-17
/// 「去除登录限速」后不再限速。真正收口仍需短信验证码或 OAuth 归属校验。
pub async fn my_account_reset_target(
    st: &AppState,
    phone: &str,
    qq: &str,
) -> std::result::Result<(String, String), String> {
    // 手机号格式是本地可判定的错误 → 单独给 INVALID_PHONE（不涉及账号存在性）
    let phone = validate_my_phone(phone)?;
    if phone.is_empty() {
        return Err(biz(MSG_PHONE_REQUIRED));
    }
    // QQ 号格式非法 → 也归入统一不匹配文案（不让「格式对不对」成为探测信号）
    let Ok(email_expected) = qq_to_email(qq) else {
        return Err(biz(MSG_RESET_MISMATCH));
    };
    let pool = w(st).map_err(|e| e.to_string())?;
    /* 只认**普通账号**（is_admin=0）：管理员观看账号不参与重置，否则知道手机号 +
    QQ 的人就能接管后台视角账号（审计 2026-10-11 修复口径保留）。
    手机号唯一索引（uk_my_phone_uniq，非空 phone 唯一）保证最多命中一行。 */
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT name, email FROM my_accounts WHERE phone = ? AND phone <> '' AND is_admin = 0 LIMIT 1",
    ))
    .bind(&phone)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("查询账号失败：{e}"))?;
    let Some(row) = row else {
        return Err(biz(MSG_RESET_MISMATCH));
    };
    let name: String = row.try_get("name").unwrap_or_default();
    let email: String = row.try_get("email").unwrap_or_default();
    /* 绑定邮箱必须与输入 QQ 号拼出的邮箱一致（域名大小写不敏感）。038 存量非 QQ
    邮箱（xx@163.com）在这一步就是不匹配 —— 不清洗、不猜 QQ，提示用户重新绑。 */
    if email.trim().is_empty() || !email.trim().eq_ignore_ascii_case(&email_expected) {
        return Err(biz(MSG_RESET_MISMATCH));
    }
    Ok((name, email))
}

/// 忘记密码**第二步**：发信成功后落库（契约 §2.5）。
///
/// 语义（2026-10-18 用户拍板 = 真正的「重置」，不是旁路凭证）：
/// - `pass` 覆盖为**不可知 CSPRNG 哈希**（明文生成即丢）→ 旧密码立即失效，
///   「知道旧密码的人还能登录」这条静默接管路径被彻底切断；
/// - `temp_pass_hash = argon2id(临时密码)`、`temp_pass_exp = now() + 15 分钟`；
/// - `must_change_pass = 1`（登录后提示改密）；
/// - 返回账号名（handler 拿它 `my_tokens_revoke` 吊销全部旧 token）。
///
/// 调用顺序不能反：**先发信成功、再调本函数** —— 否则用户会拿到一封「已重置」
/// 却登不进；本函数失败则用户可重试申请（限流仍生效）。
pub async fn my_account_reset_apply(
    st: &AppState,
    phone: &str,
    temp_pass: &str,
) -> std::result::Result<String, String> {
    let phone = validate_my_phone(phone)?;
    if phone.is_empty() {
        return Err(biz(MSG_PHONE_REQUIRED));
    }
    /* 旧密码作废：生成一个谁也不知道的随机串，只把哈希写库。
    明文 `unknown_pass` 在本函数内立即丢弃 —— 之后没有任何人能再用旧密码登录。 */
    let unknown_pass = crate::random::random_password();
    let unknown_hash = hash_my_password(&unknown_pass);
    drop(unknown_pass);
    let temp_hash = hash_my_password(temp_pass);
    let pool = w(st).map_err(|e| e.to_string())?;
    let row = sqlx::query(&crate::pq::rewrite(
        "UPDATE my_accounts \
            SET pass = ?, temp_pass_hash = ?, \
                temp_pass_exp = now() + interval '15 minutes', must_change_pass = 1 \
          WHERE phone = ? AND phone <> '' AND is_admin = 0 \
          RETURNING name",
    ))
    .bind(&unknown_hash)
    .bind(&temp_hash)
    .bind(&phone)
    .fetch_optional(pool)
    .await
    .map_err(|e| {
        if e.to_string().contains("Unknown column") {
            biz("临时密码功能未启用：请先跑一次 pk-server migrate（039_my_temp_pass）")
        } else {
            format!("重置失败：{e}")
        }
    })?;
    let Some(row) = row else {
        return Err(biz(MSG_RESET_NO_TARGET));
    };
    Ok(row.try_get::<String, _>("name").unwrap_or_default())
}

/// 已登录用户改密的**公共实现**（`my_account_self_set_password` 与新
/// [`my_account_reset_password_self`] 共用，两条路径的账号门禁与错误语义完全一致）。
///
/// 身份由调用方（`public_teams::my::my_password_set`）从 `X-My-Token` 解析后传入
/// `name` —— 本函数只改这一个账号，**不接受请求体里的账号名**，否则等于拿自己的
/// token 改别人的密码。该门禁与「忘记密码重置」（未登录、按手机号）是两条独立路径。
///
/// `old_pass` 决定要不要校验旧密码：
/// - `Some(p)` = 校验（2026-10-14 起的自助改密口径）；
/// - `None` = 不校验（2026-10-15 起的「重置密码」口径，见 [`my_account_reset_password_self`]）。
///
/// 成功返回 `Ok(())`；失败返回面向用户的文案（handler 原样 400 下发），逐条对应
/// 冻结的接口契约：
/// 1. 账号不存在 / 管理员观看账号 / 没登记过密码
///    → 「该账号不支持自助改密（未登记密码或属管理员账号）」
/// 2. 当前密码不对（**仅 `Some` 时**校验，复用 verify_my_password，兼容存量明文口令）
///    → 「当前密码不正确」
/// 3. 新密码格式（走 [`validate_my_password`]：4~64 任意 Unicode，控制字符除外）
///    → 对应 [`MSG_PASS_INVALID`] / [`MSG_PASS_BLANK`] / [`MSG_PASS_CTRL`]
/// 4. 新旧密码相同（**仅 `Some` 时**判断）→ 「新密码不能和当前密码相同」
async fn self_set_password_inner(
    st: &AppState,
    name: &str,
    old_pass: Option<&str>,
    new_pass: &str,
) -> std::result::Result<(), String> {
    let pool = w(st).map_err(|e| e.to_string())?;
    let key = name.trim();
    if key.is_empty() {
        return Err(biz("登录已过期，请重新登录"));
    }
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT pass, is_admin FROM my_accounts WHERE name = ? LIMIT 1",
    ))
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("查账号失败：{e}"))?
    .ok_or_else(|| biz("该账号不支持自助改密（未登记密码或属管理员账号）"))?;
    let stored: String = row.try_get("pass").unwrap_or_default();
    let is_admin = row.try_get::<i16, _>("is_admin").unwrap_or(0) == 1;
    /* 管理员观看账号不参与自助改密（同「忘记密码重置」的口径：后台视角账号不能被
    任意登录者接管）；密码为空的账号视为「未登记密码」，同样拒绝。文案不区分两者，
    避免泄露该账号是不是管理员。 */
    if is_admin || stored.trim().is_empty() {
        return Err(biz("该账号不支持自助改密（未登记密码或属管理员账号）"));
    }
    // 当前密码：只有调用方给了旧密码才校验（与登录同口径，兼容存量明文口令）。
    // 2026-10-18：**不 trim** —— 密码按原样字节比对，首尾空格属于密码本身。
    if let Some(old_pass) = old_pass {
        if !verify_my_password(old_pass, &stored) {
            return Err(biz("当前密码不正确"));
        }
    }
    /* 新密码复用统一口令规则（4~64 任意 Unicode），不在这里另写一套校验。
    错误文案原样透传（MSG_PASS_* 已带 BIZ:: 前缀），handler 400 下发。 */
    let new_pass = validate_my_password(new_pass)?;
    /* 「新旧不能相同」只能在拿到旧密码时判：重置路径（`None`）没有旧密码可比，
    且冻结契约对重置只规定了「格式 / 管理员账号」两类 400，不额外加限制。 */
    if let Some(old_pass) = old_pass {
        if new_pass == old_pass {
            return Err(biz("新密码不能和当前密码相同"));
        }
    }
    /* 039（契约 §2.7）：改密成功必须清临时密码三列 —— 否则用户已经设了新密码，
    下次登录仍被 `mustChangePass` 提示，甚至临时密码还能再登一次。 */
    let mut res = sqlx::query(&crate::pq::rewrite(
        "UPDATE my_accounts SET pass = ?, temp_pass_hash = '', temp_pass_exp = NULL, \
           must_change_pass = 0 WHERE name = ?",
    ))
    .bind(hash_my_password(&new_pass))
    .bind(key)
    .execute(pool)
    .await;
    if matches!(&res, Err(e) if e.to_string().contains("Unknown column")) {
        tracing::warn!("039 未跑：改密后无法清临时密码三列（先跑 pk-server migrate）");
        res = sqlx::query(&crate::pq::rewrite(
            "UPDATE my_accounts SET pass = ? WHERE name = ?",
        ))
        .bind(hash_my_password(&new_pass))
        .bind(key)
        .execute(pool)
        .await;
    }
    res.map_err(|e| format!("写入失败：{e}"))?;
    Ok(())
}

/// 已登录用户**自助改密**（2026-10-14 用户口径：个人页支持设置自定义密码）。
///
/// 保留原签名与语义（`public_teams::admin` 等既有调用方不受影响）：**要求旧密码正确**。
/// 实现改走 [`self_set_password_inner`]（与「重置密码」共用账号门禁 / 新密码规则 /
/// 写库与错误文案），因此行为与 2026-10-14 逐字一致：
/// 1. 账号不存在 / 管理员观看账号 / 没登记过密码
///    → 「该账号不支持自助改密（未登记密码或属管理员账号）」
/// 2. 当前密码不对 → 「当前密码不正确」
/// 3. 新密码格式（走 [`validate_my_password`]：4~64 任意 Unicode）
/// 4. 新旧密码相同 → 「新密码不能和当前密码相同」
pub async fn my_account_self_set_password(
    st: &AppState,
    name: &str,
    old_pass: &str,
    new_pass: &str,
) -> std::result::Result<(), String> {
    self_set_password_inner(st, name, Some(old_pass), new_pass).await
}

/// 已登录用户**重置密码**（2026-10-15 用户口径：点「重置密码」直接输两遍新密码）。
///
/// 与 [`my_account_self_set_password`] 的**唯一区别：不再校验旧密码**。
/// 沿革：2026-10-14 个人页自助改密要求先填当前密码；2026-10-15 产品拍板改成
/// 「重置密码」——展示端对话框只留「新密码 / 确认新密码」两个框，两次是否一致由前端
/// 拦下，后端在同一身份下直接改密即可。
///
/// 这次放松**不降低身份门槛**：身份仍由调用方从 `X-My-Token` 解析后传入 `name`，
/// 本函数不接受请求体里的账号名（否则等于拿自己的 token 改别人的密码）；管理员观看
/// 账号继续拒绝自助改密，密码为空的账号同样拒绝（同 [`self_set_password_inner`] 口径）。
///
/// 成功返回 `Ok(())`；调用方负责吊销该账号全部旧 token 并换发新 token。
pub async fn my_account_reset_password_self(
    st: &AppState,
    name: &str,
    new_pass: &str,
) -> std::result::Result<(), String> {
    self_set_password_inner(st, name, None, new_pass).await
}

/// 查账号信息（登录名或手机号都认）。表还没跑 008 迁移时退回老查询：
/// 老行都是管理员手建的 → 一律视为「已绑定」，登录/我的页照常，只有新注册用不了。
pub async fn my_account_info(st: &AppState, key: &str) -> Option<MyAccountInfo> {
    let pool = w(st).ok()?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    /* view_team（迁移 024）单列查询：列不存在时**整体失败**，这时走下面的老查询
    路径（view_team 回落 Inherit），保证没跑迁移的库照常能登录。
    email（迁移 038）同 view_team 一并读：列不存在时整个 SELECT 失败 → 回落老查询。 */
    let sql = "SELECT name, phone, is_admin, bound, bound_at, source, view_team, email \
               FROM my_accounts WHERE name = ? OR (phone <> '' AND phone = ?) LIMIT 1";
    if let Ok(Some(r)) = sqlx::query(&crate::pq::rewrite(sql))
        .bind(key)
        .bind(key)
        .fetch_optional(pool)
        .await
    {
        return Some(MyAccountInfo {
            name: r.try_get::<String, _>("name").unwrap_or_default(),
            phone: r.try_get::<String, _>("phone").unwrap_or_default(),
            is_admin: r.try_get::<i16, _>("is_admin").unwrap_or(0) == 1,
            bound: r.try_get::<i16, _>("bound").unwrap_or(1) == 1,
            bound_at: r.try_get::<String, _>("bound_at").unwrap_or_default(),
            source: r.try_get::<String, _>("source").unwrap_or_default(),
            view_team: r
                .try_get::<String, _>("view_team")
                .map(|v| ViewTeam::parse(&v))
                .unwrap_or_default(),
            email: r.try_get::<String, _>("email").unwrap_or_default(),
        });
    }
    let sql = "SELECT name, phone, is_admin FROM my_accounts \
               WHERE name = ? OR (phone <> '' AND phone = ?) LIMIT 1";
    sqlx::query(&crate::pq::rewrite(sql))
        .bind(key)
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .map(|r| MyAccountInfo {
            name: r.try_get::<String, _>("name").unwrap_or_default(),
            phone: r.try_get::<String, _>("phone").unwrap_or_default(),
            is_admin: r.try_get::<i16, _>("is_admin").unwrap_or(0) == 1,
            bound: true,
            bound_at: String::new(),
            source: "admin".to_string(),
            view_team: ViewTeam::Inherit,
            email: String::new(),
        })
}

/// 读账号的绑定邮箱（按登录名）。没登记 / 列不存在（迁移 038 没跑）→ 空串。
pub async fn my_account_email(st: &AppState, name: &str) -> String {
    let Ok(pool) = w(st) else {
        return String::new();
    };
    let key = name.trim();
    if key.is_empty() {
        return String::new();
    }
    sqlx::query(&crate::pq::rewrite(
        "SELECT email FROM my_accounts WHERE name = ? LIMIT 1",
    ))
    .bind(key)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .and_then(|r| r.try_get::<String, _>("email").ok())
    .unwrap_or_default()
}

/// 设置 / 清空账号的绑定邮箱（2026-10-17）。`email` 已由调用方 `validate_email`
/// 归一化（空串 = 解绑）。只按 `name` 改这一个账号，不接受请求体里的账号名。
pub async fn my_account_set_email(
    st: &AppState,
    name: &str,
    email: &str,
) -> std::result::Result<(), String> {
    let pool = w(st).map_err(|e| e.to_string())?;
    let key = name.trim();
    if key.is_empty() {
        return Err(biz("登录已过期，请重新登录"));
    }
    sqlx::query(&crate::pq::rewrite(
        "UPDATE my_accounts SET email = ? WHERE name = ?",
    ))
    .bind(email)
    .bind(key)
    .execute(pool)
    .await
    .map_err(|e| {
        if e.to_string().contains("Unknown column") {
            biz("邮箱功能未启用：请先跑一次 pk-server migrate（038_my_account_email）")
        } else {
            format!("写入邮箱失败：{e}")
        }
    })?;
    Ok(())
}

/* ------------- 登录态身份标志（who / login / verify 共用，契约 §2.2） ------------- */

/// 展示端账号的登录态身份标志。
///
/// 2026-10-18（契约 docs/26/05 §2.2）：把「要不要强制补全 / 要不要催绑 QQ /
/// 要不要提示改密」收敛成一个结构，`who`、`login`、`verify` 三处共用，
/// 避免各算一套口径漂移。
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct MyAuthFlags {
    /// `my_accounts.bound = 1`（账号已绑主播；管理员手建老账号默认 1）
    pub bound: bool,
    /// 管理员观看账号（不参与补全 / 催绑 / 重置）
    pub is_admin: bool,
    /// 原始手机号（**handler 必须用 `wx::mask_phone` 打码后再下发**，不要原样外发）
    pub phone: String,
    /// 原始邮箱（**仅内部用**；对外一律用 `email_mask`，禁止明文下发）
    pub email: String,
    /// 是否已绑合法 QQ 邮箱（038 存量非 QQ 邮箱算「未绑」）
    pub qq_bound: bool,
    /// 脱敏邮箱：QQ → `123***@qq.com`；非 QQ → `a***@163.com`；未绑 → `""`
    pub email_mask: String,
    /// 需要强制补全（S1：绑了主播、非管理员、还没手机号）
    pub need_profile: bool,
    /// 需要提示绑 QQ（S2：已绑手机号、非管理员、不是 S1、还没 QQ）
    pub need_qq: bool,
    /// 临时密码登录后待改密（039；未迁移库恒 false）
    pub must_change_pass: bool,
}

impl MyAuthFlags {
    /// 从账号行算标志。`must_change_pass` 单独传入：039 未跑时调用方给 false。
    fn from_row(r: &sqlx::postgres::PgRow, must_change_pass: bool) -> Self {
        let phone: String = r.try_get("phone").unwrap_or_default();
        let email: String = r.try_get("email").unwrap_or_default();
        let is_admin = r.try_get::<i16, _>("is_admin").unwrap_or(0) == 1;
        let bound = r.try_get::<i16, _>("bound").unwrap_or(1) == 1;
        let qq_bound = is_qq_email(&email);
        // 契约 §2.2：needProfile 优先于 needQq；管理员观看账号两者都不参与
        let need_profile = bound && !is_admin && phone.is_empty();
        let need_qq = bound && !is_admin && !need_profile && !qq_bound;
        Self {
            bound,
            is_admin,
            email_mask: mask_qq_email(&email),
            email,
            phone,
            qq_bound,
            need_profile,
            need_qq,
            must_change_pass,
        }
    }
}

/// 读登录态身份标志（`who` / `login` / `verify` 共用）。账号不存在 → None。
///
/// 逐级列回落（039 → 038 → 001 老列）：未迁移的库只失去「是否待改密 / 是否绑 QQ」
/// 这两项新信息，**不会整体登不进去**（与 `my_account_info` 的 038 回落同口径）。
pub async fn my_account_auth_flags(st: &AppState, name: &str) -> Option<MyAuthFlags> {
    let pool = w(st).ok()?;
    let key = name.trim();
    if key.is_empty() {
        return None;
    }
    let sqls = [
        "SELECT phone, is_admin, bound, email, must_change_pass \
         FROM my_accounts WHERE name = ? LIMIT 1",
        "SELECT phone, is_admin, bound, email FROM my_accounts WHERE name = ? LIMIT 1",
        "SELECT phone, is_admin, bound FROM my_accounts WHERE name = ? LIMIT 1",
    ];
    for sql in sqls {
        let got = sqlx::query(&crate::pq::rewrite(sql))
            .bind(key)
            .fetch_optional(pool)
            .await;
        match got {
            Ok(Some(r)) => {
                let must = r.try_get::<i16, _>("must_change_pass").unwrap_or(0) == 1;
                return Some(MyAuthFlags::from_row(&r, must));
            }
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!(error = %e, name = key, "读身份标志失败，回落更老的列清单");
            }
        }
    }
    None
}

/* ------------------------- 凭艺名认领开号（契约 §2.3） ------------------------- */

/// 认领成功后的主播身份（handler 直接拿去发 token / 回登录同形包）。
#[derive(Clone, Debug, serde::Serialize)]
pub struct ClaimedAccount {
    /// 台账主播 id
    #[serde(rename = "personId")]
    pub person_id: i64,
    /// 艺名（= 登录名）
    pub name: String,
    /// 台账 `person.gender`（male / female）
    pub team: String,
    /// 认领时还没有手机号，恒为空串（前端据此进强制补全）
    pub phone: String,
}

/// 认领判定（契约 §2.3）：登录门「账号不存在」时供前端显示「认领账号」按钮。
///
/// 可认领必须同时满足：
/// 1. 台账里有**唯一**同名在职主播（`deleted_at IS NULL`、`status != 'left'`、`name <> ''`）；
/// 2. 该主播 `gender ∈ {male, female}`（团别是后续一切数据的前提）；
/// 3. `my_accounts` 里没有同名行（艺名唯一），且 `person_id` 没被别的账号占用。
///
/// 任何查询失败都按「不可认领」处理（登录门宁可少显示按钮，也不能给假希望）。
pub async fn my_account_can_claim(st: &AppState, name: &str) -> bool {
    let Ok(pool) = w(st) else {
        return false;
    };
    let key = name.trim();
    if key.is_empty() {
        return false;
    }
    let rows = match sqlx::query(&crate::pq::rewrite(
        "SELECT id, gender FROM person \
          WHERE name = ? AND name <> '' AND deleted_at IS NULL AND status <> 'left'",
    ))
    .bind(key)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, name = key, "认领判定查台账失败");
            return false;
        }
    };
    if rows.len() != 1 {
        return false;
    }
    let person_id: i64 = rows[0].try_get("id").unwrap_or(0);
    let gender: String = rows[0].try_get("gender").unwrap_or_default();
    if person_id <= 0 || !matches!(gender.as_str(), "male" | "female") {
        return false;
    }
    match sqlx::query(&crate::pq::rewrite(
        "SELECT 1 FROM my_accounts WHERE name = ? LIMIT 1",
    ))
    .bind(key)
    .fetch_optional(pool)
    .await
    {
        Ok(Some(_)) => return false,
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(error = %e, name = key, "认领判定查同名账号失败");
            return false;
        }
    }
    match sqlx::query(&crate::pq::rewrite(
        "SELECT 1 FROM my_accounts WHERE person_id = ? LIMIT 1",
    ))
    .bind(person_id)
    .fetch_optional(pool)
    .await
    {
        Ok(Some(_)) => false,
        Ok(None) => true,
        Err(e) => {
            tracing::warn!(error = %e, person_id, "认领判定查 person_id 占用失败");
            false
        }
    }
}

/// 凭艺名认领开号：单事务 INSERT `my_accounts`（契约 §2.3）。
///
/// 写入：`name` = 台账艺名、`phone = ''`、`pass = argon2id(pass)`、`is_admin = 0`、
/// `bound = 1`、`bound_at = now`、`source = 'claim'`、`person_id` = 台账 id。
/// 成功返回 [`ClaimedAccount`]（team 取 `person.gender`），handler 直接发 token 并回
/// 登录同形包（`needProfile:true`、`needQq:true`）。
///
/// 失败一律 `BIZ::` 中文文案（400）：艺名不存在 / 台账重名 / 已离团 / 已有人认领 /
/// 性别不合法 / 密码不合法；并发撞主键或 `uk_my_person_uniq`（23505）统一折成
/// 「账号已存在，请直接登录」（用户下一步就该去登录，而不是再点一次认领）。
pub async fn my_account_claim(
    st: &AppState,
    name: &str,
    pass: &str,
) -> std::result::Result<ClaimedAccount, String> {
    let key = name.trim();
    if key.is_empty() {
        return Err(biz("请填写艺名"));
    }
    let pass = validate_my_password(pass)?;
    let pool = w(st).map_err(|e| e.to_string())?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("认领事务开启失败：{e}"))?;

    // 1) 台账里找同名在职主播（含已离团行，才能给出「已离团」这条明确文案）
    let rows = sqlx::query(&crate::pq::rewrite(
        "SELECT id, gender, status FROM person \
          WHERE name = ? AND name <> '' AND deleted_at IS NULL",
    ))
    .bind(key)
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| format!("查台账失败：{e}"))?;
    if rows.is_empty() {
        return Err(biz("台账里没有这个艺名，请联系管理员开号"));
    }
    if rows.len() > 1 {
        return Err(biz("该艺名在台账里有多个主播，请联系管理员"));
    }
    let person_id: i64 = rows[0].try_get("id").unwrap_or(0);
    let gender: String = rows[0].try_get("gender").unwrap_or_default();
    let status: String = rows[0].try_get("status").unwrap_or_default();
    if person_id <= 0 {
        return Err(biz("台账里没有这个艺名，请联系管理员开号"));
    }
    if status == "left" {
        return Err(biz("该主播已离团，不能认领"));
    }
    /* 性别必须是 male/female：团别决定展示端能看哪半边数据，unknown 的账号进去
    也是空页面，不如直说「联系管理员」把台账补对。 */
    if !matches!(gender.as_str(), "male" | "female") {
        return Err(biz("该主播性别信息异常，请联系管理员"));
    }

    // 2) 占用检查（事务内；并发最终由 my_accounts 主键与 uk_my_person_uniq 兜底）
    let name_taken = sqlx::query(&crate::pq::rewrite(
        "SELECT 1 FROM my_accounts WHERE name = ? LIMIT 1",
    ))
    .bind(key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| format!("查账号失败：{e}"))?
    .is_some();
    if name_taken {
        return Err(biz("该艺名已被认领，请直接登录"));
    }
    let person_taken = sqlx::query(&crate::pq::rewrite(
        "SELECT 1 FROM my_accounts WHERE person_id = ? LIMIT 1",
    ))
    .bind(person_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| format!("查账号失败：{e}"))?
    .is_some();
    if person_taken {
        return Err(biz("该主播已有登录账号，请联系管理员"));
    }

    // 3) 落库（开号即绑定：bound=1，source 标 claim 便于事后审计）
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let res = sqlx::query(&crate::pq::rewrite(
        "INSERT INTO my_accounts \
           (name, phone, pass, is_admin, created_at, bound, bound_at, source, person_id) \
         VALUES (?, '', ?, 0, ?, 1, ?, 'claim', ?)",
    ))
    .bind(key)
    .bind(hash_my_password(&pass))
    .bind(&now)
    .bind(&now)
    .bind(person_id)
    .execute(&mut *tx)
    .await;
    match res {
        Ok(_) => {}
        Err(e) => {
            if is_unique_violation(&e) {
                /* 并发：两个请求同时认领同一艺名 → 主键（name）或
                uk_my_person_uniq（person_id）先到先得，输家去登录即可。 */
                return Err(biz("账号已存在，请直接登录"));
            }
            return Err(format!("认领失败：{e}"));
        }
    }
    tx.commit()
        .await
        .map_err(|e| format!("认领提交失败：{e}"))?;
    Ok(ClaimedAccount {
        person_id,
        name: key.to_string(),
        team: gender,
        phone: String::new(),
    })
}

/* ---------------- 绑定 QQ / 首次补全（契约 §2.6，2026-10-18 用户口径） ---------------- */

/// 绑定 / 改绑 / 解绑 QQ 邮箱（契约 §2.6）。
///
/// - `qq` 空串 = **解绑**：`email = ''`，并清掉未使用的临时密码两列
///   （`must_change_pass` 保留：解绑不该顺手解除「必须改密」）；
/// - 非空：`normalize_qq` → `<qq>@qq.com`，应用层查重给友好文案 + 040 唯一索引
///   兜并发（23505 → [`MSG_QQ_TAKEN`]）；
/// - 038 存量的非 QQ 邮箱允许被覆盖（用户重新绑 QQ 即覆盖旧值）；
/// - S1（`phone = ''`）账号不允许只绑 QQ → [`MSG_NEED_PROFILE`]（handler → 403），
///   防绕过 `profile_bind` 的完整校验单独把 QQ 绑上。
///
/// 返回**脱敏后**的邮箱（解绑 → 空串）。成功响应只给脱敏值，不下发明文。
pub async fn my_account_set_qq(
    st: &AppState,
    name: &str,
    qq: &str,
) -> std::result::Result<String, String> {
    let pool = w(st).map_err(|e| e.to_string())?;
    let key = name.trim();
    if key.is_empty() {
        return Err(biz("登录已过期，请重新登录"));
    }
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT phone FROM my_accounts WHERE name = ? LIMIT 1",
    ))
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("查账号失败：{e}"))?;
    let Some(row) = row else {
        return Err(biz("账号不存在，请重新登录"));
    };
    let phone: String = row.try_get("phone").unwrap_or_default();
    if phone.is_empty() {
        return Err(biz(MSG_NEED_PROFILE));
    }

    let qq = qq.trim();
    if qq.is_empty() {
        // 解绑：清邮箱 + 未使用临时密码（039 未跑时回落只清邮箱）
        let mut res = sqlx::query(&crate::pq::rewrite(
            "UPDATE my_accounts SET email = '', temp_pass_hash = '', temp_pass_exp = NULL \
             WHERE name = ?",
        ))
        .bind(key)
        .execute(pool)
        .await;
        if matches!(&res, Err(e) if e.to_string().contains("Unknown column")) {
            tracing::warn!("039 未跑：解绑 QQ 时无法清临时密码两列");
            res = sqlx::query(&crate::pq::rewrite(
                "UPDATE my_accounts SET email = '' WHERE name = ?",
            ))
            .bind(key)
            .execute(pool)
            .await;
        }
        res.map_err(|e| {
            if e.to_string().contains("Unknown column") {
                biz("邮箱功能未启用：请先跑一次 pk-server migrate（038_my_account_email）")
            } else {
                format!("解绑邮箱失败：{e}")
            }
        })?;
        return Ok(String::new());
    }

    let email = qq_to_email(qq)?;
    // 应用层查重（文案友好）；并发由 040 的 uk_my_email_uniq 兜底
    let taken = sqlx::query(&crate::pq::rewrite(
        "SELECT 1 FROM my_accounts WHERE email = ? AND email <> '' AND name <> ? LIMIT 1",
    ))
    .bind(&email)
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("查重失败：{e}"))?
    .is_some();
    if taken {
        return Err(biz(MSG_QQ_TAKEN));
    }
    sqlx::query(&crate::pq::rewrite(
        "UPDATE my_accounts SET email = ? WHERE name = ?",
    ))
    .bind(&email)
    .bind(key)
    .execute(pool)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            biz(MSG_QQ_TAKEN)
        } else if e.to_string().contains("Unknown column") {
            biz("邮箱功能未启用：请先跑一次 pk-server migrate（038_my_account_email）")
        } else {
            format!("写入邮箱失败：{e}")
        }
    })?;
    Ok(mask_qq_email(&email))
}

/// 首次补全（S1 → S2/S3，契约 §2.6 / 设计 02 §3.2）。
///
/// 身份只认 token 传入的 `name`（**不接受请求体里的账号名**）；`phone` 必填
/// （5~20 位数字、全局唯一）；`qq` 选填（空 = 暂不绑，登录后 `needQq` 继续提醒，
/// 没 QQ 的用户可找管理员）；`new_pass` 选填（`Some` 时改密并清三列临时态）。
///
/// 单事务 `UPDATE ... WHERE name = ? AND phone = '' AND is_admin = 0`：
/// rowcount ≠ 1 → [`MSG_PROFILE_ALREADY_DONE`]（已补全过 / 账号不存在 / 管理员账号，
/// 统一文案，不让「是不是管理员」成为探测信号）。并发唯一冲突（23505）按索引名翻文案。
pub async fn my_account_profile_bind(
    st: &AppState,
    name: &str,
    phone: &str,
    qq: &str,
    new_pass: Option<&str>,
) -> std::result::Result<(), String> {
    let key = name.trim();
    if key.is_empty() {
        return Err(biz("登录已过期，请重新登录"));
    }
    let phone = validate_my_phone(phone)?;
    if phone.is_empty() {
        return Err(biz(MSG_PHONE_REQUIRED));
    }
    let qq = qq.trim();
    let email = if qq.is_empty() {
        String::new()
    } else {
        qq_to_email(qq)?
    };
    let pass_hash = match new_pass {
        Some(p) => Some(hash_my_password(&validate_my_password(p)?)),
        None => None,
    };
    let pool = w(st).map_err(|e| e.to_string())?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("补全事务开启失败：{e}"))?;

    // 手机号唯一（排除自己）：先查一遍给友好文案，最终仍靠 uk_my_phone_uniq
    let phone_taken = sqlx::query(&crate::pq::rewrite(
        "SELECT 1 FROM my_accounts WHERE phone = ? AND phone <> '' AND name <> ? LIMIT 1",
    ))
    .bind(&phone)
    .bind(key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| format!("查重失败：{e}"))?
    .is_some();
    if phone_taken {
        return Err(biz(MSG_PHONE_TAKEN));
    }
    if !email.is_empty() {
        // QQ 唯一（排除自己）：同一 QQ 不能绑多个账号（040 索引兜底并发）
        let qq_taken = sqlx::query(&crate::pq::rewrite(
            "SELECT 1 FROM my_accounts WHERE email = ? AND email <> '' AND name <> ? LIMIT 1",
        ))
        .bind(&email)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| format!("查重失败：{e}"))?
        .is_some();
        if qq_taken {
            return Err(biz(MSG_QQ_TAKEN));
        }
    }

    let sql = if pass_hash.is_some() {
        "UPDATE my_accounts SET phone = ?, email = ?, pass = ?, \
           temp_pass_hash = '', temp_pass_exp = NULL, must_change_pass = 0 \
         WHERE name = ? AND phone = '' AND is_admin = 0"
    } else {
        "UPDATE my_accounts SET phone = ?, email = ? \
         WHERE name = ? AND phone = '' AND is_admin = 0"
    };
    // rewrite 返回 String：必须先落成一个活到 `q` 之后的局部量，不能借临时值
    let sql_owned = crate::pq::rewrite(sql);
    let mut q = sqlx::query(&sql_owned).bind(&phone).bind(&email);
    if let Some(h) = &pass_hash {
        q = q.bind(h);
    }
    let mut res = q.bind(key).execute(&mut *tx).await;
    if pass_hash.is_some() && matches!(&res, Err(e) if e.to_string().contains("Unknown column")) {
        tracing::warn!("039 未跑：首次补全改密时无法清临时密码三列");
        let hash = pass_hash.clone().unwrap_or_default();
        res = sqlx::query(&crate::pq::rewrite(
            "UPDATE my_accounts SET phone = ?, email = ?, pass = ? \
             WHERE name = ? AND phone = '' AND is_admin = 0",
        ))
        .bind(&phone)
        .bind(&email)
        .bind(&hash)
        .bind(key)
        .execute(&mut *tx)
        .await;
    }
    let rowcount = match res {
        Ok(r) => r.rows_affected(),
        Err(e) => {
            if is_unique_violation(&e) {
                let m = e.to_string();
                return Err(biz(if m.contains("uk_my_email_uniq") {
                    MSG_QQ_TAKEN
                } else {
                    MSG_PHONE_TAKEN
                }));
            }
            if e.to_string().contains("Unknown column") {
                return Err(biz(
                    "邮箱功能未启用：请先跑一次 pk-server migrate（038_my_account_email）",
                ));
            }
            return Err(format!("补全失败：{e}"));
        }
    };
    if rowcount != 1 {
        return Err(biz(MSG_PROFILE_ALREADY_DONE));
    }
    tx.commit()
        .await
        .map_err(|e| format!("补全提交失败：{e}"))?;
    Ok(())
}

/// 绑定结果：绑定后的主播身份（前端拿它重新发 token / 换团）
#[derive(Clone, Debug, serde::Serialize)]
pub struct MyBindResult {
    pub name: String,
    pub team: String,
    pub person_id: i64,
}

/// 用户自助绑定主播（**只允许一次**）：账号必须还是 `bound=0`。
/// 绑定后 `name` 从手机号占位改写成主播姓名，展示端水印/我的页都按它走。
pub async fn my_account_bind(
    st: &AppState,
    login_name: &str,
    person_id: i64,
) -> std::result::Result<MyBindResult, String> {
    bind_inner(st, login_name.trim(), person_id, false).await
}

/// 管理员改绑（用户自己改不了）：已绑定的账号也能改到另一位主播。
pub async fn my_account_admin_bind(
    st: &AppState,
    login_name: &str,
    person_id: i64,
) -> std::result::Result<MyBindResult, String> {
    bind_inner(st, login_name.trim(), person_id, true).await
}

async fn bind_inner(
    st: &AppState,
    login_name: &str,
    person_id: i64,
    admin: bool,
) -> std::result::Result<MyBindResult, String> {
    let pool = w(st).map_err(|e| e.to_string())?;
    if login_name.is_empty() {
        return Err(biz("请先登录"));
    }
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT name, bound FROM my_accounts WHERE name = ? LIMIT 1",
    ))
    .bind(login_name)
    .fetch_optional(pool)
    .await
    .map_err(|e| {
        if e.to_string().contains("Unknown column") {
            "缺少数据库迁移 008（先跑一次 pk-server migrate）".to_string()
        } else {
            format!("查账号失败：{e}")
        }
    })?
    .ok_or_else(|| "账号不存在，请重新登录".to_string())?;
    let bound = row.try_get::<i16, _>("bound").unwrap_or(1) == 1;
    if bound && !admin {
        return Err(biz(
            "该账号已绑定主播，不能自行更改（需要改绑请联系管理员）",
        ));
    }

    let p = crate::pgrs::query_with_binds(
        &st.pools,
        &st.active_id(),
        "SELECT id, name, gender, status FROM person WHERE id = ? AND deleted_at IS NULL LIMIT 1",
        |q| q.bind(person_id),
    )
    .await
    .map_err(|e| format!("查主播失败：{e}"))?
    .into_iter()
    .next()
    .ok_or_else(|| "该主播不存在或已删除".to_string())?;
    let person_name = p.try_get::<String, _>("name").unwrap_or_default();
    let team = p.try_get::<String, _>("gender").unwrap_or_default();
    let status = p.try_get::<String, _>("status").unwrap_or_default();
    if person_name.is_empty() {
        return Err(biz("该主播不存在或已删除"));
    }
    if status == "left" {
        return Err(biz("该主播已离团，不能绑定"));
    }

    let taken = sqlx::query(&crate::pq::rewrite(
        "SELECT name FROM my_accounts WHERE name = ? AND name <> ? LIMIT 1",
    ))
    .bind(&person_name)
    .bind(login_name)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("查重失败：{e}"))?
    .is_some();
    if taken {
        return Err(biz("该主播已存在绑定的账号，请联系管理员"));
    }

    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    // 2026-10-12 补漏：绑定必须回写 person_id 锚点（015 迁移的设计——账号改名后
    // 靠 person_id 才不失联；uk_my_person_uniq 顺带保证一个主播只被一个账号绑）。
    // 原来两条 UPDATE 都只写 name/bound/bound_at，自助绑定的账号 pid 恒 0。
    let affected = sqlx::query(&crate::pq::rewrite(
        "UPDATE my_accounts SET name = ?, bound = 1, bound_at = ?, person_id = ? \
         WHERE name = ? AND bound = 0",
    ))
    .bind(&person_name)
    .bind(&now)
    .bind(person_id)
    .bind(login_name)
    .execute(pool)
    .await
    .map_err(|e| bind_update_err(e))?
    .affected();
    if affected == 0 && !admin {
        // 并发下被别人先绑走了：用户侧不允许覆盖
        return Err(biz(
            "该账号已绑定主播，不能自行更改（需要改绑请联系管理员）",
        ));
    }
    if affected == 0 {
        sqlx::query(&crate::pq::rewrite(
            "UPDATE my_accounts SET name = ?, bound = 1, bound_at = ?, person_id = ? WHERE name = ?",
        ))
        .bind(&person_name)
        .bind(&now)
        .bind(person_id)
        .bind(login_name)
        .execute(pool)
        .await
        .map_err(bind_update_err)?;
    }
    Ok(MyBindResult {
        name: person_name,
        team,
        person_id,
    })
}

/* ------------------ 单元测试：登录 / 密码重置 / 自助改密的纯函数边界 ------------------ */
///
/// 2026-10-14 第四轮审计把这三条链路改造完，但**当时没有任何测试守住边界**：
/// 注册移除、忘记密码重置（当时改 CSPRNG 随机串，2026-10-17 用户口径又改回
/// 手机号后四位，与开号初始密码统一）、个人页改走已登录自助改密，
/// 全靠人工复核。这里把不碰数据库、可重复跑的那部分契约钉死。
#[cfg(test)]
mod tests {
    use super::*;

    /* --------------------- BIZ:: 业务错误标记（2026-10-16） --------------------- */

    /// 契约：业务拒绝带 `BIZ::` 前缀，库层故障不带 —— 路由层靠它分流 400/500。
    /// 改绑报障（2026-10-16）的回归锚点：别让「该主播已存在绑定的账号」再被
    /// 抹成 400 通用话术「请求参数有误」。
    #[test]
    fn biz_marker_roundtrip() {
        let e = biz("该主播已存在绑定的账号，请联系管理员");
        assert!(is_biz(&e), "业务拒绝必须带 {BIZ_PREFIX} 前缀");
        assert_eq!(biz_msg(&e), "该主播已存在绑定的账号，请联系管理员");
        // 库层故障（模拟 format!("查账号失败：{e}") 的产物）不带前缀
        let db = format!("查账号失败：duplicate key …");
        assert!(!is_biz(&db));
        assert_eq!(biz_msg(&db), db, "非业务错误 biz_msg 应返回原文");
    }

    /// 校验器（人话文案）一律走 biz 标记；这里钉死三个入口，防止有人改回去。
    #[test]
    fn validators_mark_biz() {
        assert!(is_biz(&validate_my_account("", "abc123").unwrap_err()));
        assert!(is_biz(&validate_my_account("狼哲", "短").unwrap_err()));
        assert!(is_biz(&validate_my_phone("abc").unwrap_err()));
        assert!(is_biz(&validate_douyin_no("中文抖音号").unwrap_err()));
    }

    /* --------------------- 可看团别 view_team（迁移 024） --------------------- */

    /// 解析：**任何非法值都回落 Inherit**，绝不因为一列脏值让账号登不进去。
    #[test]
    fn view_team_parse_falls_back_on_garbage() {
        for raw in [
            "", "  ", "MALE", "Female", " male ", "any", "zzz", "0", "男团",
        ] {
            let v = ViewTeam::parse(raw);
            assert!(
                matches!(
                    v,
                    ViewTeam::Inherit | ViewTeam::Any | ViewTeam::Male | ViewTeam::Female
                ),
                "{raw:?} 应是合法枚举"
            );
        }
        assert_eq!(ViewTeam::parse(""), ViewTeam::Inherit, "空串 = 未设置");
        assert_eq!(ViewTeam::parse("zzz"), ViewTeam::Inherit, "脏值 → Inherit");
        assert_eq!(ViewTeam::parse("男团"), ViewTeam::Inherit, "中文 → Inherit");
        // 大小写与空格容忍（运营在后台手填 / 脚本写入时不至于静默失效）
        assert_eq!(ViewTeam::parse("MALE"), ViewTeam::Male);
        assert_eq!(ViewTeam::parse(" Female "), ViewTeam::Female);
    }

    /// code() 与 parse() 必须互为逆运算（存库 → 读库不漂移）。
    #[test]
    fn view_team_code_roundtrips() {
        for v in [
            ViewTeam::Inherit,
            ViewTeam::Any,
            ViewTeam::Male,
            ViewTeam::Female,
        ] {
            assert_eq!(ViewTeam::parse(v.code()), v, "{v:?} 存库后读回来应一致");
        }
        // 未设置必须落成空串，与迁移的 DEFAULT '' 对齐（非空串会被当成真配置）
        assert_eq!(ViewTeam::Inherit.code(), "");
    }

    /// token_team：Any 落空串（gate_team 放行两团）；钉死团别**无视绑定关系**。
    #[test]
    fn view_team_token_team_override() {
        // 全团账号：不管绑了谁，都落空串 = 两团放行 + 前端可切
        assert_eq!(ViewTeam::Any.token_team("male"), "");
        assert_eq!(ViewTeam::Any.token_team("female"), "");
        assert_eq!(ViewTeam::Any.token_team(""), "");
        // 钉死女团：即使账号绑着男团主播（base=male），也只给女团
        assert_eq!(ViewTeam::Female.token_team("male"), "female");
        assert_eq!(ViewTeam::Male.token_team("female"), "male");
        // Inherit：原样沿用旧逻辑算出来的团（这条路径的语义必须与改动前完全一致）
        assert_eq!(ViewTeam::Inherit.token_team("male"), "male");
        assert_eq!(ViewTeam::Inherit.token_team("female"), "female");
        assert_eq!(ViewTeam::Inherit.token_team(""), "");
        // 2026-10-17 审计 4-P2-1：Inherit 对非 male/female 一律归一成空串 ——
        // token 里的团别会被内联进 SQL，脏值（unknown / 未来写路径漏网的）进不了 SQL
        assert_eq!(ViewTeam::Inherit.token_team("unknown"), "");
        assert_eq!(ViewTeam::Inherit.token_team("male'--"), "");
        assert_eq!(ViewTeam::Inherit.token_team("male OR 1=1"), "");
    }

    /// 只有全团账号能页内切团；钉死某团 / 未设置的都不可切。
    #[test]
    fn view_team_switchable_only_for_any() {
        assert!(ViewTeam::Any.switchable());
        assert!(!ViewTeam::Male.switchable());
        assert!(!ViewTeam::Female.switchable());
        assert!(!ViewTeam::Inherit.switchable());
    }

    /// 写入前收敛：脏值一律变空串（= Inherit），不把垃圾带进库。
    #[test]
    fn normalize_view_team_sanitizes() {
        assert_eq!(normalize_view_team("any"), "any");
        assert_eq!(normalize_view_team("female"), "female");
        assert_eq!(normalize_view_team("MALE"), "male");
        assert_eq!(normalize_view_team("zzz"), "");
        assert_eq!(normalize_view_team(""), "");
    }

    /* ---------------- 密码规则：4~64 个字符，任意 Unicode（2026-10-18 用户拍板） ----------------
     *
     * 口径沿革：2026-10-10 放宽 4 位 → 10-12 审计提到 6 → 10-13 拍回 4 →
     * 10-17 允许「点」 → **2026-10-18 直接放开到「4~64 个字符、允许中英文/特殊字符/
     * 任何符号（含空格）」**（用户原话：「密码支持中英文、特殊字符、任何符号」）。
     * 仍然拒绝 ASCII 控制字符（防换行/制表符混进日志与 SQL），**并且不 trim**。 */

    /// 放行：中英文、数字、符号、空格、emoji 都算「字符」——只数个数，不看字符集。
    #[test]
    fn password_rule_accepts_any_unicode() {
        for p in [
            "9282", // 手机号后四位（运营开号习惯）
            "abcd",
            "133-1234-5678", // 横线（旧口径拒绝，2026-10-18 起放行）
            "ab_123",        // 下划线
            "12 34",         // 中间空格
            "abcd!",
            "ab@c#",
            "密码abcd",        // 中文
            "Aa181206..",      // 点
            "ab\u{1f600}cd",   // emoji（按 1 个字符计）
            "p@ss w0rd! 中文", // 混排
            "  a1b  ",         // 首尾空格也是密码的一部分（不 trim）
        ] {
            assert!(
                validate_my_password(p).is_ok(),
                "{p:?}（{} 个字符）应放行",
                p.chars().count()
            );
        }
    }

    /// 长度边界：4 个字符是最小合法值；64 个字符是上限（**按字符数，不是字节数**）。
    #[test]
    fn password_rule_length_boundaries_are_char_based() {
        assert!(validate_my_password("1234").is_ok(), "4 个字符 → 放行");
        assert!(
            validate_my_password(&"密".repeat(64)).is_ok(),
            "64 个中文字符 = 192 字节，按字符计应放行（按字节算会误杀）"
        );
        assert_eq!("密".repeat(64).chars().count(), 64);
        assert!(PASS_MIN_LEN == 4 && PASS_MAX_LEN == 64);
    }

    /// 下限 4：1~3 个字符一律拒绝。
    #[test]
    fn password_rule_rejects_too_short() {
        for p in [
            "1", "12", "123", "a", "ab", "abc", "a1", "1a2", "密", "密码",
        ] {
            assert!(
                validate_my_account("凤夏", p).is_err(),
                "{p:?}（{} 个字符）少于 4 → 拒绝",
                p.chars().count()
            );
        }
        let err = validate_my_password("123").unwrap_err();
        assert_eq!(biz_msg(&err), MSG_PASS_INVALID);
    }

    /// 上限 64：65 个字符起一律拒绝（中文同样按字符数算）。
    #[test]
    fn password_rule_rejects_too_long() {
        assert!(
            validate_my_password(&"1".repeat(65)).is_err(),
            "65 位数字 → 拒绝"
        );
        assert!(
            validate_my_password(&"a".repeat(65)).is_err(),
            "65 位字母 → 拒绝"
        );
        assert!(
            validate_my_password(&"z9".repeat(33)).is_err(),
            "66 位混合 → 拒绝"
        );
        assert!(
            validate_my_password(&"密".repeat(65)).is_err(),
            "65 个中文字符 → 拒绝"
        );
        let err = validate_my_password(&"a".repeat(65)).unwrap_err();
        assert_eq!(biz_msg(&err), MSG_PASS_INVALID);
    }

    /// 空串 / 全空白：按「密码不能为空」拒绝（全空格不是密码；含其它字符时空格照常保留）。
    #[test]
    fn password_rule_rejects_blank() {
        for p in ["", "   ", "\t\n ", "        "] {
            let err = validate_my_password(p).expect_err(&format!("{p:?} 是空白密码，必须拒绝"));
            assert!(is_biz(&err), "空密码拒绝必须是 BIZ 文案：{err}");
            assert_eq!(biz_msg(&err), MSG_PASS_BLANK, "实际文案：{err}");
        }
        // validate_my_account 同样拦（空密码不能开通/改密）
        assert!(validate_my_account("凤夏", "").is_err());
    }

    /// ASCII 控制字符（\x00-\x1F、\x7F）一律拒绝：它们会破坏日志、终端与 SQL 文本。
    #[test]
    fn password_rule_rejects_ascii_control_chars() {
        for p in [
            "ab\ncd",     // 换行
            "ab\tcd",     // 制表符
            "ab\rcd",     // 回车
            "ab\u{0}cd",  // NUL
            "ab\u{7f}cd", // DEL
            "abcd\u{1}",  // SOH
        ] {
            let err = validate_my_password(p).expect_err(&format!("{p:?} 含控制字符，必须拒绝"));
            assert_eq!(biz_msg(&err), MSG_PASS_CTRL, "实际文案：{err}");
        }
        // 非 ASCII 的「控制类」字符不在拒绝范围（用户口径是任意符号），例如零宽连接符
        assert!(validate_my_password("ab\u{200d}cd").is_ok());
    }

    /// **不 trim**：首尾空格原样保留（否则会出现「看着一样、登录不上」）。
    #[test]
    fn password_rule_keeps_password_untrimmed() {
        let (name, pass) = validate_my_account("  凤夏 ", "  9282  ").unwrap();
        assert_eq!(name, "凤夏", "姓名照旧 trim");
        assert_eq!(pass, "  9282  ", "密码不能 trim，首尾空格属于密码本身");
        // 中间空格本来就算字符（2026-10-18 起放行）
        assert_eq!(validate_my_password(" 92 82 ").unwrap(), " 92 82 ");
    }

    /// 姓名规则不变：空、纯空格、超 20 字拒绝；中文名不被误伤。
    #[test]
    fn password_rule_also_validates_name() {
        assert!(validate_my_account("", "9282").is_err(), "姓名为空 → 拒绝");
        assert!(
            validate_my_account("   ", "9282").is_err(),
            "姓名 trim 后为空 → 拒绝"
        );
        assert!(
            validate_my_account(&"凤".repeat(21), "9282").is_err(),
            "姓名超 20 字 → 拒绝"
        );
        assert_eq!(
            validate_my_account("狼哲", "9282").unwrap(),
            ("狼哲".to_string(), "9282".to_string()),
            "中文名原样保留"
        );
    }

    /* --------------- QQ 号 / QQ 邮箱 / 脱敏（2026-10-18 契约 §2.1） --------------- */

    /// QQ 号归一化：去空白、兼容粘贴整段邮箱、大小写不敏感剥后缀。
    #[test]
    fn normalize_qq_strips_whitespace_and_suffix() {
        assert_eq!(normalize_qq("123456").unwrap(), "123456");
        assert_eq!(normalize_qq("  123456  ").unwrap(), "123456");
        assert_eq!(normalize_qq("123 456").unwrap(), "123456", "内部空格也去");
        assert_eq!(
            normalize_qq("12345\u{3000}6").unwrap(),
            "123456",
            "全角空格也去"
        );
        assert_eq!(normalize_qq("123456@qq.com").unwrap(), "123456");
        assert_eq!(
            normalize_qq("123456@QQ.COM").unwrap(),
            "123456",
            "域名大小写不敏感"
        );
        assert_eq!(normalize_qq(" 123456@qq.com ").unwrap(), "123456");
        // 边界：5 位与 12 位合法
        assert_eq!(normalize_qq("12345").unwrap(), "12345");
        assert_eq!(normalize_qq("123456789012").unwrap(), "123456789012");
    }

    /// QQ 号非法形态：长度越界、首位 0、非 ASCII 数字、非 QQ 域名、空串。
    /// 注意全角数字**不自动转换**（用户看不出服务端改过什么，宁可拒绝）。
    #[test]
    fn normalize_qq_rejects_bad_shapes() {
        for raw in [
            "",
            "   ",
            "1234",          // 4 位（少一位）
            "1234567890123", // 13 位（多一位）
            "012345",        // 首位 0
            "0",
            "12a456",
            "12-456",         // 横线不是 QQ 号的一部分
            "123456@163.com", // 只收 QQ 域名
            "123456@vip.qq.com",
            "１２３４５６", // 全角数字
            "abcde",
            "@qq.com",
        ] {
            let err = normalize_qq(raw).expect_err(&format!("{raw:?} 不是合法 QQ 号，必须拒绝"));
            assert!(is_biz(&err), "必须是 BIZ 文案：{err}");
            assert_eq!(biz_msg(&err), MSG_INVALID_QQ, "实际文案：{err}");
        }
    }

    /// QQ ↔ 邮箱互转，以及「非 QQ 域名绝不猜」。
    #[test]
    fn qq_email_roundtrip_and_domain_guard() {
        assert_eq!(qq_to_email("123456").unwrap(), "123456@qq.com");
        assert!(qq_to_email("1234").is_err(), "非法 QQ 号不能拼出邮箱");
        assert_eq!(qq_from_email("123456@qq.com").as_deref(), Some("123456"));
        assert_eq!(qq_from_email("123456@QQ.com").as_deref(), Some("123456"));
        assert!(is_qq_email("123456@qq.com"));
        // 非 QQ 域名 / 本地部分不像 QQ 号 → None（不把 163 地址的本地部分当 QQ）
        for raw in [
            "",
            "xx@163.com",
            "123456@vip.qq.com",
            "12345a@qq.com",
            "012345@qq.com",
            "1234@qq.com",
            "1234567890123@qq.com",
            "123456@qq.com.cn",
            "没有at",
        ] {
            assert_eq!(qq_from_email(raw), None, "{raw:?} 不该被识别成 QQ 邮箱");
            assert!(!is_qq_email(raw), "{raw:?} qqBound 应为 false");
        }
    }

    /// 脱敏：QQ 本地部分保留前 3 位；非 QQ 走「首字符***@域名」；空串 → 空串。
    /// **绝不允许**完整 QQ 号出现在下发值里。
    #[test]
    fn mask_qq_email_masks_local_part() {
        assert_eq!(mask_qq_email("123456789@qq.com"), "123***@qq.com");
        assert_eq!(mask_qq_email("12345@qq.com"), "123***@qq.com");
        assert_eq!(mask_qq_email(" 123456@qq.com "), "123***@qq.com");
        assert_eq!(mask_qq_email("a@163.com"), "a***@163.com");
        assert_eq!(mask_qq_email("longname@corp.cn"), "l***@corp.cn");
        assert_eq!(mask_qq_email(""), "", "未绑定 → 空串");
        assert_eq!(mask_qq_email("   "), "");
        assert_eq!(mask_qq_email("没有at"), "***", "异常值整段打码");
        for email in ["123456789@qq.com", "12345@qq.com"] {
            let masked = mask_qq_email(email);
            let local = email.split_once('@').unwrap().0;
            assert!(
                !masked.contains(local),
                "脱敏结果 {masked} 不应包含完整本地部分 {local}"
            );
        }
    }

    /* --------------------------- 初始密码：手机号后四位 --------------------------- */

    /// 分界线是「数字个数 >= 4」，够 4 位取最后四个数字。
    #[test]
    fn initial_password_takes_last_four_digits() {
        assert_eq!(
            initial_password("13800138000"),
            "8000",
            "11 位手机号取后四位"
        );
        assert_eq!(
            initial_password("13800138000"),
            "8000",
            "末位是 0 也要补齐四位"
        );
        assert_eq!(
            initial_password("0000"),
            "0000",
            "全 0 是合法结果，不能当假值丢掉"
        );
        assert_eq!(initial_password("1234"), "1234", "刚好 4 位 → 原样返回");
        // 带分隔符 / 空格的手机号：只认数字
        assert_eq!(initial_password("138-0013-8000"), "8000");
        assert_eq!(initial_password("138 0013 8000"), "8000");
        // 超过 11 位（带国际区号）仍取最后四个数字
        assert_eq!(initial_password("8613800138000"), "8000");
    }

    /// 同一手机号两次结果必须一致 —— 运营要靠电话转述给主播，不可随机。
    #[test]
    fn initial_password_is_stable_for_same_phone() {
        let a = initial_password("13800138000");
        let b = initial_password("13800138000");
        assert_eq!(a, b, "同一手机号必须可复现，否则运营没法口头转述");
    }

    /// 手机号不足 4 位（空 / 无数字）→ 退回 CSPRNG 随机的 10 位字母数字。
    /// 这是「没登记手机号也能开号」的兜底，断了就开不出号。
    #[test]
    fn initial_password_falls_back_to_random_when_under_four() {
        for phone in ["", "1", "12", "123", "abc", "13-8", "+86"] {
            let p = initial_password(phone);
            assert_eq!(
                p.len(),
                10,
                "{phone:?} 不足 4 位数字 → 应走随机 10 位，实际 {p:?}"
            );
            assert!(
                p.chars().all(|c| c.is_ascii_alphanumeric()),
                "{phone:?} 兜底密码 {p:?} 必须是纯字母数字"
            );
            assert!(
                validate_my_account("凤夏", &p).is_ok(),
                "{phone:?} 兜底密码 {p:?} 必须过账号校验"
            );
        }
    }

    /// 兜底走的确实是随机数：两次结果不同，不可预测。
    #[test]
    fn initial_password_fallback_is_random_each_time() {
        let a = initial_password("1");
        let b = initial_password("1");
        assert_ne!(a, b, "同一短手机号两次兜底不应相同（应为 CSPRNG 随机）");
    }

    /// 两条链路的衔接保证：写库用的初始密码必须能被 `validate_my_account` 接受，
    /// 否则主播第一次自助改密就被自己挡在门外。
    #[test]
    fn initial_password_always_passes_account_validation() {
        for phone in [
            "13800138000",
            "13800138000",
            "0000",
            "1234",
            "8613800138000",
            "",
            "123",
            "abc",
        ] {
            let p = initial_password(phone);
            assert!(
                validate_my_account("凤夏", &p).is_ok(),
                "initial_password({phone:?}) = {p:?} 应能通过账号密码校验"
            );
        }
    }

    /* ----------------- 忘记密码：邮箱绑定 + 随机密码（2026-10-17 用户口径） ----------------- */

    /// 忘记密码重置 = 随机密码发绑定邮箱；没绑邮箱 → 业务拒绝「找管理员」。
    /// 这里钉住两条契约文案（业务拒绝用 BIZ:: 前缀，handler 据此 400 原样下发），
    /// 以及「重置用的是 CSPRNG 随机串、不是可推导的后四位」这一口径。
    #[test]
    fn reset_password_mismatch_message_and_temp_pass() {
        /* 2026-10-18 口径：手机号 + QQ 双因子，所有不匹配分支共用同一句文案
        （防手机号存在性枚举）。这句是 handler 映射 400 RESET_MISMATCH 的锚点。 */
        assert_eq!(
            biz_msg(&biz(MSG_RESET_MISMATCH)),
            "手机号与 QQ 号不匹配，或该账号未绑定 QQ 邮箱，请联系管理员重置"
        );
        assert_eq!(
            biz_msg(&biz(MSG_RESET_MISMATCH)),
            MSG_RESET_MISMATCH,
            "这句是跨层冻结文案，改了必须同步 handler 的映射"
        );
        // 临时密码 = 10 位 CSPRNG 串（不是可推导的手机号后四位），且必须过密码规则
        let p = crate::random::random_password();
        assert_eq!(p.chars().count(), 10, "临时密码应为 10 位随机串");
        assert_ne!(
            p,
            initial_password("13800138000"),
            "临时密码 ≠ 手机号后四位"
        );
        assert!(
            validate_my_password(&p).is_ok(),
            "临时密码必须过统一密码规则"
        );
        // 非法 QQ 号的拒绝文案与用户填错 QQ 的提示一致
        assert_eq!(biz_msg(&qq_to_email("abc").unwrap_err()), MSG_INVALID_QQ);
    }

    /// 随机密码两次不同 —— 连点两次重置不能拿到同一个串（即便都发同一邮箱）。
    #[test]
    fn reset_password_random_differs_between_calls() {
        let a = crate::random::random_password();
        let b = crate::random::random_password();
        assert_ne!(a, b, "两次重置不应下发同一个随机密码");
    }

    /// 邮箱格式校验：合法放行、非法拒绝、空串 = 解绑放行。
    #[test]
    fn validate_email_accepts_and_rejects() {
        assert_eq!(validate_email("  a@b.com  ").unwrap(), "a@b.com");
        assert_eq!(validate_email("").unwrap(), "");
        for bad in [
            "a",
            "a@",
            "@b.com",
            "a@b",
            "a@b.",
            "a@.b.com",
            "a b@c.com",
            "a@b.c om",
            "a@b.com\r\nX-Header: y",
        ] {
            assert!(
                is_biz(&validate_email(bad).unwrap_err()),
                "非法邮箱应被业务拒绝：{bad:?}"
            );
        }
    }

    /* ------------- 登录失败原因：密码错 ≠ 账号没开通（2026-10-14 用户口径） ------------- */

    /// 登录失败文案只有两种，且必须能区分「密码错」和「没开通」——
    /// 前端直接显示后端给的这句，所以这两个字面量是契约，改了要同步改前端兜底。
    #[test]
    fn login_fail_message_says_which_one() {
        assert_eq!(LoginFail::BadPassword.message(), "密码错误");
        assert_eq!(LoginFail::NoAccount.message(), "账号还没开通");
        assert_eq!(
            LoginFail::TempExpired.message(),
            "临时密码已过期，请重新获取"
        );
        assert_ne!(
            LoginFail::BadPassword.message(),
            LoginFail::NoAccount.message(),
            "两种失败不能塌成同一句，否则又回到「账号或密码不对，或账号还没开通」"
        );
        assert_ne!(
            LoginFail::TempExpired.message(),
            LoginFail::BadPassword.message(),
            "临时密码过期的提示不能塌成「密码错误」，否则用户不知道该重新申请"
        );
        // 契约 §3.1 的 code：handler 直接下发，前端按 code 分流 UI
        assert_eq!(LoginFail::BadPassword.code(), "BAD_PASSWORD");
        assert_eq!(LoginFail::NoAccount.code(), "NO_ACCOUNT");
        assert_eq!(LoginFail::TempExpired.code(), "TEMP_EXPIRED");
    }

    /// 两种失败都必须**不含**对方的字样：文案里不能出现「或」这种和稀泥的措辞，
    /// 也不能把「没开通」塞进密码错的提示里。
    #[test]
    fn login_fail_messages_do_not_conflate() {
        for m in [
            LoginFail::BadPassword.message(),
            LoginFail::NoAccount.message(),
            LoginFail::TempExpired.message(),
        ] {
            assert!(!m.contains('或'), "失败文案不应含糊：{m}");
            assert!(!m.is_empty());
        }
    }

    /* ------------- 内置管理员账号三段式解析（2026-10-17） ------------- */

    /// `BUILTIN_MY_ACCOUNTS` 支持 `name:pass:viewTeam`：
    /// - 两段（旧写法）= any；一段（只有名字）= 密码同账号名 + any
    /// - viewTeam 只认 male / female，其它（含 any、空、脏值）一律 any（管理员不该被关死）
    /// 这是三位管理员（浩雨=any / 饭团=female / 鹏先生=any）能一次配到位的基础。
    #[test]
    fn builtin_accounts_parses_view_team() {
        std::env::set_var(
            "BUILTIN_MY_ACCOUNTS",
            "浩雨:haoyu2026:any,饭团:fantuan2026:female,鹏先生:peng2026:any",
        );
        let got = builtin_my_accounts_from_env();
        assert_eq!(got.len(), 3, "三位管理员：{got:?}");
        assert_eq!(got[0], ("浩雨".into(), "haoyu2026".into(), "any".into()));
        assert_eq!(
            got[1],
            ("饭团".into(), "fantuan2026".into(), "female".into())
        );
        assert_eq!(got[2], ("鹏先生".into(), "peng2026".into(), "any".into()));

        // 旧两段式：viewTeam 缺省 = any
        std::env::set_var("BUILTIN_MY_ACCOUNTS", "old:pwd");
        assert_eq!(
            builtin_my_accounts_from_env(),
            vec![("old".into(), "pwd".into(), "any".into())]
        );
        // 只有名字：密码 = 账号名，viewTeam = any
        std::env::set_var("BUILTIN_MY_ACCOUNTS", "solo");
        assert_eq!(
            builtin_my_accounts_from_env(),
            vec![("solo".into(), "solo".into(), "any".into())]
        );
        // 大小写容错 + 脏值兜底
        std::env::set_var("BUILTIN_MY_ACCOUNTS", "a:p:Male,b:p:FEMALE,c:p:weird,d:p:");
        let g = builtin_my_accounts_from_env();
        assert_eq!(g[0].2, "male");
        assert_eq!(g[1].2, "female");
        assert_eq!(g[2].2, "any", "脏值按 any，不能把管理员关死");
        assert_eq!(g[3].2, "any");
        // 未配置 / 空串 = 不建任何账号（默认安全）
        std::env::remove_var("BUILTIN_MY_ACCOUNTS");
        assert!(builtin_my_accounts_from_env().is_empty());
        std::env::set_var("BUILTIN_MY_ACCOUNTS", " , ,");
        assert!(builtin_my_accounts_from_env().is_empty(), "空项应被跳过");
        std::env::remove_var("BUILTIN_MY_ACCOUNTS");
    }

    /* --------------- 真库集成回归（默认 #[ignore]；PK_IT_DB=1 时才跑） ---------------
     *
     * 覆盖契约 docs/26/05 §2 的写路径：认领 → 登录（艺名 / 手机号 / 临时密码）→
     * 首次补全 → 绑·解绑 QQ → 忘记密码双因子 → 改密清三列 → 管理端新字段 → 过期惰性清理。
     *
     * 跑法（本机 douyin_dev，只动下面四个常量对应的测试数据）：
     *   cd rust && PK_IT_DB=1 cargo test -p pk-server --lib \
     *     store::my_account::tests::my_account_flow_real_db -- --ignored --nocapture
     *
     * 测试数据用固定名字 + 手机号/QQ，跑前跑后各清一次；panic 造成的残留会被下一次
     * 开头那次清理扫掉（不会碰真实账号：名字带 __t1_verify_flow_ 前缀）。
     */
    const IT_NAME_MALE: &str = "__t1_verify_flow_男";
    const IT_NAME_FEMALE: &str = "__t1_verify_flow_女";
    const IT_PHONE_MALE: &str = "19900000001";
    const IT_PHONE_FEMALE: &str = "19900000002";
    const IT_QQ_MALE: &str = "10000001";
    const IT_QQ_FEMALE: &str = "10000002";

    /// 按 DB_WRITER_* 环境变量建一个最小 AppState（只有 writer 池会被用到）。
    /// 与 `store::wxpush` 的真库回归测试同一套开关（PK_IT_DB=1）。
    async fn it_state() -> Option<crate::state::AppState> {
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
        let pool = match sqlx::PgPool::connect_with(opt).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[skip] 连写库失败：{e}");
                return None;
            }
        };
        let pools = crate::pgrs::build_pools(&[]).await.ok()?;
        Some(crate::state::AppState {
            auth: std::sync::Arc::new(crate::auth::Auth::from_env()),
            sources: Vec::new(),
            pools,
            writer: Some(pool),
            redis: None,
            active: std::sync::Mutex::new("LOCAL".to_string()),
            cache: std::sync::Mutex::new(Default::default()),
            wave_ttl: std::time::Duration::from_secs(600),
            bots: std::sync::OnceLock::new(),
        })
    }

    async fn it_cleanup(st: &crate::state::AppState) {
        let Some(pool) = st.writer.as_ref() else {
            return;
        };
        for name in [IT_NAME_MALE, IT_NAME_FEMALE] {
            let _ = sqlx::query(&crate::pq::rewrite(
                "DELETE FROM my_accounts WHERE name = ?",
            ))
            .bind(name)
            .execute(pool)
            .await;
            let _ = sqlx::query(&crate::pq::rewrite("DELETE FROM person WHERE name = ?"))
                .bind(name)
                .execute(pool)
                .await;
        }
        // 上次跑一半 panic 可能留下「占用着手机号/QQ」的行：按值再清一遍
        for p in [IT_PHONE_MALE, IT_PHONE_FEMALE] {
            let _ = sqlx::query(&crate::pq::rewrite(
                "DELETE FROM my_accounts WHERE phone = ?",
            ))
            .bind(p)
            .execute(pool)
            .await;
        }
        for q in [IT_QQ_MALE, IT_QQ_FEMALE] {
            let _ = sqlx::query(&crate::pq::rewrite(
                "DELETE FROM my_accounts WHERE email = ?",
            ))
            .bind(format!("{q}@qq.com"))
            .execute(pool)
            .await;
        }
    }

    async fn it_add_person(st: &crate::state::AppState, name: &str, gender: &str) -> i64 {
        let pool = st.writer.as_ref().expect("writer 池");
        sqlx::query(&crate::pq::rewrite(
            "INSERT INTO person (name, gender, status) VALUES (?, ?, 'active') RETURNING id",
        ))
        .bind(name)
        .bind(gender)
        .fetch_one(pool)
        .await
        .expect("插入测试主播失败（person 表结构变了？）")
        .try_get("id")
        .unwrap_or(0)
    }

    #[ignore = "真库集成：需 PK_IT_DB=1 与 DB_WRITER_* 环境变量"]
    #[tokio::test]
    async fn my_account_flow_real_db() {
        let Some(st) = it_state().await else {
            eprintln!("[skip] 未设置 PK_IT_DB=1 / DB_WRITER_*，跳过真库集成回归");
            return;
        };
        it_cleanup(&st).await;
        let body: std::result::Result<(), String> = async {
            // ---- 1) 认领开号 ----
            let p1 = it_add_person(&st, IT_NAME_MALE, "male").await;
            assert!(
                my_account_can_claim(&st, IT_NAME_MALE).await,
                "唯一在职同名主播应可认领"
            );
            // 041：同名在职主播不能再插第二个（否则认领无法判定是谁）
            let dup_person = sqlx::query(&crate::pq::rewrite(
                "INSERT INTO person (name, gender, status) VALUES (?, 'female', 'active')",
            ))
            .bind(IT_NAME_MALE)
            .execute(st.writer.as_ref().unwrap())
            .await;
            assert!(
                dup_person.is_err(),
                "041 uk_person_name_active 必须挡住同名在职主播"
            );

            let claimed = my_account_claim(&st, IT_NAME_MALE, "初识密码@2026")
                .await
                .expect("认领应成功");
            assert_eq!(claimed.person_id, p1);
            assert_eq!(claimed.name, IT_NAME_MALE);
            assert_eq!(claimed.team, "male");
            assert_eq!(claimed.phone, "", "认领后还没手机号 → 前端进强制补全");
            assert!(
                !my_account_can_claim(&st, IT_NAME_MALE).await,
                "已认领 → 不可再认领"
            );
            assert!(
                is_biz(
                    &my_account_claim(&st, IT_NAME_MALE, "初识密码@2026")
                        .await
                        .unwrap_err()
                ),
                "重复认领必须是业务拒绝（handler 400 原样下发）"
            );

            // ---- 2) 登录：艺名 + 含中文/符号/空格的密码 ----
            let out = my_account_check_login(&st, IT_NAME_MALE, "初识密码@2026")
                .await
                .expect("艺名登录");
            assert_eq!(out.name, IT_NAME_MALE);
            assert_eq!(out.person_id, p1);
            assert_eq!(out.team, "male");
            assert!(!out.temp_used, "常密码登录不是临时密码登录");
            assert_eq!(
                my_account_check_login(&st, IT_NAME_MALE, "错密码")
                    .await
                    .unwrap_err(),
                LoginFail::BadPassword
            );
            // 密码不 trim：前后多一个空格就登不进
            assert_eq!(
                my_account_check_login(&st, IT_NAME_MALE, " 初识密码@2026 ")
                    .await
                    .unwrap_err(),
                LoginFail::BadPassword,
                "密码按原样比对（不 trim）"
            );

            // ---- 3) S1 标志 + 拒绝单独绑 QQ ----
            let f = my_account_auth_flags(&st, IT_NAME_MALE)
                .await
                .expect("flags");
            assert!(f.need_profile, "没手机号 → needProfile");
            assert!(!f.need_qq, "S1 优先，不催 QQ");
            assert!(!f.qq_bound);
            assert_eq!(f.email_mask, "");
            assert_eq!(f.phone, "", "S1 还没有手机号（handler 打码前是空串）");
            assert_eq!(
                biz_msg(
                    &my_account_set_qq(&st, IT_NAME_MALE, IT_QQ_MALE)
                        .await
                        .unwrap_err()
                ),
                MSG_NEED_PROFILE,
                "S1 只绑 QQ 必须被拦（handler → 403 NEED_PROFILE）"
            );

            // ---- 4) 首次补全：手机号 + QQ + 新密码 ----
            my_account_profile_bind(
                &st,
                IT_NAME_MALE,
                IT_PHONE_MALE,
                IT_QQ_MALE,
                Some("新密码!a1"),
            )
            .await
            .expect("首次补全应成功");
            assert_eq!(
                biz_msg(
                    &my_account_profile_bind(&st, IT_NAME_MALE, "19900000009", "", None)
                        .await
                        .unwrap_err()
                ),
                MSG_PROFILE_ALREADY_DONE,
                "第二次补全 → PROFILE_ALREADY_DONE"
            );
            let f = my_account_auth_flags(&st, IT_NAME_MALE)
                .await
                .expect("flags");
            assert!(!f.need_profile && !f.need_qq);
            assert!(f.qq_bound);
            assert_eq!(f.email_mask, "100***@qq.com", "脱敏：本地部分留前 3 位");
            assert!(!f.must_change_pass);
            assert_eq!(
                my_account_check_login(&st, IT_NAME_MALE, "初识密码@2026")
                    .await
                    .unwrap_err(),
                LoginFail::BadPassword,
                "补全时给了新密码 → 旧密码失效"
            );
            assert!(my_account_check_login(&st, IT_NAME_MALE, "新密码!a1")
                .await
                .is_ok());
            assert!(
                my_account_check_login(&st, IT_PHONE_MALE, "新密码!a1")
                    .await
                    .is_ok(),
                "手机号路径同样能登"
            );

            // ---- 5) 手机号 / QQ 唯一 ----
            let _p2 = it_add_person(&st, IT_NAME_FEMALE, "female").await;
            assert!(my_account_can_claim(&st, IT_NAME_FEMALE).await);
            my_account_claim(&st, IT_NAME_FEMALE, "另一个密码1")
                .await
                .expect("第二人认领");
            assert_eq!(
                biz_msg(
                    &my_account_profile_bind(
                        &st,
                        IT_NAME_FEMALE,
                        IT_PHONE_FEMALE,
                        IT_QQ_MALE,
                        None,
                    )
                    .await
                    .unwrap_err()
                ),
                MSG_QQ_TAKEN,
                "同一 QQ 不能绑多个账号（040 唯一索引 + 应用层查重）"
            );
            assert_eq!(
                biz_msg(
                    &my_account_profile_bind(&st, IT_NAME_FEMALE, IT_PHONE_MALE, "", None)
                        .await
                        .unwrap_err()
                ),
                MSG_PHONE_TAKEN,
                "手机号唯一"
            );
            // 暂不绑 QQ（qq 空）→ S2：needQq=true
            my_account_profile_bind(&st, IT_NAME_FEMALE, IT_PHONE_FEMALE, "", None)
                .await
                .expect("第二人补全（暂不绑 QQ）");
            let f2 = my_account_auth_flags(&st, IT_NAME_FEMALE)
                .await
                .expect("flags");
            assert!(f2.need_qq && !f2.qq_bound, "没 QQ 的用户由 needQq 引导");
            assert_eq!(
                biz_msg(
                    &my_account_set_qq(&st, IT_NAME_FEMALE, IT_QQ_MALE)
                        .await
                        .unwrap_err()
                ),
                MSG_QQ_TAKEN
            );
            assert_eq!(
                my_account_set_qq(&st, IT_NAME_FEMALE, IT_QQ_FEMALE)
                    .await
                    .expect("绑定自己的 QQ"),
                "100***@qq.com"
            );
            assert!(
                my_account_auth_flags(&st, IT_NAME_FEMALE)
                    .await
                    .unwrap()
                    .qq_bound
            );
            // 解绑 → 空串 + needQq 回归
            assert_eq!(
                my_account_set_qq(&st, IT_NAME_FEMALE, "")
                    .await
                    .expect("解绑 QQ"),
                ""
            );
            assert!(
                my_account_auth_flags(&st, IT_NAME_FEMALE)
                    .await
                    .unwrap()
                    .need_qq
            );

            // ---- 6) 忘记密码双因子（不匹配一律同一文案）----
            let (t_name, t_email) = my_account_reset_target(&st, IT_PHONE_MALE, IT_QQ_MALE)
                .await
                .expect("手机号 + QQ 匹配");
            assert_eq!(t_name, IT_NAME_MALE);
            assert_eq!(t_email, "10000001@qq.com");
            for (phone, qq, why) in [
                (IT_PHONE_MALE, IT_QQ_FEMALE, "QQ 不匹配"),
                (IT_PHONE_FEMALE, IT_QQ_FEMALE, "该账号未绑 QQ"),
                (IT_PHONE_MALE, "abc", "QQ 格式非法"),
                ("19900000000", IT_QQ_MALE, "手机号不存在"),
            ] {
                assert_eq!(
                    biz_msg(&my_account_reset_target(&st, phone, qq).await.unwrap_err()),
                    MSG_RESET_MISMATCH,
                    "{why} → 统一 RESET_MISMATCH 文案（防枚举）"
                );
            }

            // ---- 7) 下发临时密码：旧密码作废，临时密码 15 分钟内可重复用 ----
            assert_eq!(
                my_account_reset_apply(&st, IT_PHONE_MALE, "TempPass123")
                    .await
                    .expect("下发临时密码"),
                IT_NAME_MALE
            );
            assert_eq!(
                my_account_check_login(&st, IT_NAME_MALE, "新密码!a1")
                    .await
                    .unwrap_err(),
                LoginFail::BadPassword,
                "重置把 pass 覆盖成不可知哈希 → 旧密码立即失效"
            );
            let out = my_account_check_login(&st, IT_NAME_MALE, "TempPass123")
                .await
                .expect("临时密码登录");
            assert!(out.temp_used);
            assert!(
                my_account_auth_flags(&st, IT_NAME_MALE)
                    .await
                    .unwrap()
                    .must_change_pass,
                "临时密码登录后 mustChangePass=true"
            );
            let again = my_account_check_login(&st, IT_NAME_MALE, "TempPass123")
                .await
                .expect("临时密码不消费，15 分钟内可重复用");
            assert!(
                again.temp_used,
                "契约 §2.4：Valid 不清 temp（避免手滑锁死）"
            );
            assert_eq!(
                my_account_temp_pass_try(&st, IT_NAME_MALE, "TempPass123")
                    .await
                    .unwrap(),
                TempPassResult::Valid
            );
            assert_eq!(
                my_account_temp_pass_try(&st, IT_NAME_MALE, "WrongTemp1")
                    .await
                    .unwrap(),
                TempPassResult::Mismatch
            );

            // ---- 8) 改密：清三列，临时密码立即失效 ----
            my_account_reset_password_self(&st, IT_NAME_MALE, "最终密码 2026")
                .await
                .expect("改密");
            assert!(
                !my_account_auth_flags(&st, IT_NAME_MALE)
                    .await
                    .unwrap()
                    .must_change_pass
            );
            assert_eq!(
                my_account_check_login(&st, IT_NAME_MALE, "TempPass123")
                    .await
                    .unwrap_err(),
                LoginFail::BadPassword,
                "改密后临时密码失效"
            );
            assert!(my_account_check_login(&st, IT_NAME_MALE, "最终密码 2026")
                .await
                .is_ok());

            // ---- 9) 管理端列表 / 单条新字段 ----
            let list = my_account_list(&st).await;
            let item = list
                .iter()
                .find(|v| v["name"].as_str() == Some(IT_NAME_MALE))
                .expect("列表应含测试账号");
            assert_eq!(item["qqBound"], true);
            assert_eq!(item["emailMask"], "100***@qq.com");
            assert_eq!(item["mustChangePass"], false);
            let got = my_account_get(&st, IT_NAME_MALE).await.expect("单条");
            assert_eq!(got["qqBound"], true);
            assert_eq!(got["emailMask"], "100***@qq.com");
            assert_eq!(got["mustChangePass"], false);

            // ---- 10) clear_temp 显式清三列 ----
            my_account_reset_apply(&st, IT_PHONE_MALE, "TempPass456")
                .await
                .expect("再次下发临时密码");
            my_account_clear_temp(&st, IT_NAME_MALE)
                .await
                .expect("清临时密码");
            assert_eq!(
                my_account_check_login(&st, IT_NAME_MALE, "TempPass456")
                    .await
                    .unwrap_err(),
                LoginFail::BadPassword
            );
            assert!(
                !my_account_auth_flags(&st, IT_NAME_MALE)
                    .await
                    .unwrap()
                    .must_change_pass
            );

            // ---- 11) 过期：先验签再判过期 → TempExpired，并惰性清 temp ----
            my_account_reset_apply(&st, IT_PHONE_MALE, "TempPass789")
                .await
                .expect("下发临时密码");
            sqlx::query(&crate::pq::rewrite(
                "UPDATE my_accounts SET temp_pass_exp = now() - interval '1 minute' WHERE name = ?",
            ))
            .bind(IT_NAME_MALE)
            .execute(st.writer.as_ref().unwrap())
            .await
            .expect("把 expire 改到过去");
            assert_eq!(
                my_account_check_login(&st, IT_NAME_MALE, "TempPass789")
                    .await
                    .unwrap_err(),
                LoginFail::TempExpired,
                "过期 → TEMP_EXPIRED（handler 回「临时密码已过期，请重新获取」）"
            );
            assert_eq!(
                my_account_check_login(&st, IT_NAME_MALE, "TempPass789")
                    .await
                    .unwrap_err(),
                LoginFail::BadPassword,
                "惰性清理后同一串不再认"
            );
            Ok(())
        }
        .await;
        it_cleanup(&st).await;
        if let Err(e) = body {
            panic!("真库集成回归失败：{e}");
        }
    }
}
