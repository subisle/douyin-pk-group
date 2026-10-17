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

/// 校验姓名/密码。Err(错误文案)，Ok((姓名, 密码))。
/// **密码为 4~20 位字母、数字或点**：去首尾空格后必须是 ASCII 字母/数字/点，
/// 不允许空格、横线等其余分隔符。
///
/// 口径沿革（2026-10-13 产品拍板：**回到 4 位下限**）：
/// - 2026-10-10 放宽为 4 位；
/// - 2026-10-12 安全审计 M4 把下限提到 6（理由：4 位易被撞库）；
/// - 2026-10-13 恢复 4 位 —— 运营的实际开号习惯是**手机号后四位**
///   （xlsx 收集入库的那批账号全是 4 位），6 位下限会让后台开号、
///   以及开号后按惯例改密全部被拒，与既有 39 个账号口径不一致。
/// 折中：下限回 4 兼容惯例；撞库风险交给登录限流（5 次/分/IP、
/// 失败 5 次锁 10 分钟）承担，不再用密码长度当第一道防线。
/// - 2026-10-17 允许「点」：主播自定义密码出现 `Aa181206..` 这类带句点的
///   写法（音浪系统账号设置 xlsx 玖豆提交），纯字母数字规则会把人挡在
///   自助改密门外。点不属于常见「分隔符型」弱字符（横线/空格），放宽无
///   撞库增益损失，登录链路本来就不限字符集（argon2 全量比对）。
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

/// 绑定 UPDATE 的错误分流（2026-10-17 审计 4-N14）：唯一约束冲突（23505，
/// `uk_my_person_uniq`——并发下另一个账号先绑走了同一主播）折成业务文案，
/// 不再把 PG 原文（表名/约束名）拼进返回串；其余错误照旧透传给上层分流 500。
fn bind_update_err(e: sqlx::Error) -> String {
    if let sqlx::Error::Database(db) = &e {
        if db.code().as_deref() == Some("23505") {
            return biz("该主播刚被其它账号绑定，请刷新后重试");
        }
    }
    format!("绑定失败：{e}")
}

/// 取出去掉标记的业务文案（非业务错误返回原文，调用方无需先判 is_biz）。
pub fn biz_msg(err: &str) -> &str {
    err.strip_prefix(BIZ_PREFIX).unwrap_or(err)
}

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
    let pass = pass.trim();
    if pass.is_empty() {
        return Err(biz("密码不能为空；已有账号要保留原密码请传当前密码"));
    }
    /* 2026-10-17 口径：允许「点」（`.`）—— 主播自定义密码里出现了 Aa181206..
    这类带句点的写法。仍不允许空格 / 横线等其余分隔符（狼澈那条 H q a 5211314
    带 U+2006 空格，入库前先人工去空格成 Hqa5211314）。 */
    if !pass.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') || pass.len() < 4 || pass.len() > 20 {
        return Err(biz("密码必须是 4~20 位字母、数字或点"));
    }
    Ok((name.to_string(), pass.to_string()))
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

/// 全部账号（管理端列表用），按添加时间倒序。
/// `bound`/`source` 来自 008 迁移；表还没跑到 008 时退回不带这两列的查询（一律视为已绑定）。
pub async fn my_account_list(st: &AppState) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    let rows = match sqlx::query(
        "SELECT name, phone, is_admin, created_at, bound, bound_at, source, view_team \
         FROM my_accounts ORDER BY created_at DESC, name ASC",
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.try_get::<String, _>(0).unwrap_or_default(),
                    "phone": r.try_get::<String, _>(1).unwrap_or_default(),
                    "isAdmin": r.try_get::<i16, _>(2).unwrap_or(0) == 1,
                    "createdAt": r.try_get::<String, _>(3).unwrap_or_default(),
                    "bound": r.try_get::<i16, _>(4).unwrap_or(1) == 1,
                    "boundAt": r.try_get::<String, _>(5).unwrap_or_default(),
                    "source": r.try_get::<String, _>(6).unwrap_or_default(),
                    "viewTeam": r.try_get::<String, _>(7).unwrap_or_default(),
                })
            })
            .collect::<Vec<_>>(),
        Err(_) => sqlx::query(
            "SELECT name, phone, is_admin, created_at \
             FROM my_accounts ORDER BY created_at DESC, name ASC",
        )
        .fetch_all(pool)
        .await
        .map(|rows| {
            rows.iter()
                .map(|r| {
                    serde_json::json!({
                        "name": r.try_get::<String, _>(0).unwrap_or_default(),
                        "phone": r.try_get::<String, _>(1).unwrap_or_default(),
                        "isAdmin": r.try_get::<i16, _>(2).unwrap_or(0) == 1,
                        "createdAt": r.try_get::<String, _>(3).unwrap_or_default(),
                        "bound": true,
                        "boundAt": "",
                        "source": "admin",
                        "viewTeam": "",
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "查展示端账号失败");
            Vec::new()
        }),
    };
    rows
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
    sqlx::query(&crate::pq::rewrite(
        "INSERT INTO my_accounts (name, phone, pass, is_admin, created_at) VALUES (?, ?, ?, 0, ?)
         ON CONFLICT (name) DO UPDATE SET phone = EXCLUDED.phone, pass = EXCLUDED.pass",
    ))
    .bind(&name)
    .bind(&phone)
    .bind(&pass_hash)
    .bind(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string())
    .execute(pool)
    .await
    .map_err(|e| {
        let m = e.to_string();
        /* docs/12 §P6 记过的不一致：唯一索引已经能挡住重复手机号，但错误一直是
        一句 `写入失败：Duplicate entry ...`，运营看不出是手机号还是姓名撞了。
        这里把唯一索引冲突翻成中文文案（原先与注册路径共用一套，注册已移除）。 */
        if m.contains("Duplicate entry") {
            if m.contains("PRIMARY") {
                format!("账号名「{name}」已存在（同名主播？先在台账里改名）")
            } else {
                format!("手机号 {phone} 已存在（已绑在另一个账号上）")
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
/// 额外带 `bound`/`source`（008 迁移）：管理端据此显示「已绑主播 / 待绑定」。
pub async fn my_account_get(st: &AppState, name: &str) -> Option<serde_json::Value> {
    let pool = w(st).ok()?;
    let key = name.trim();
    /* 带 view_team 的新查询（迁移 024）。列不存在时整句失败 → 走下面老查询，
    viewTeam 回落 ""（前端按「沿用旧口径」显示）。 */
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT name, phone, is_admin, created_at, bound, bound_at, source, view_team \
         FROM my_accounts WHERE name = ?",
    ))
    .bind(key)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    if let Some(row) = row {
        let name = row.try_get::<String, _>("name").ok()?;
        return Some(serde_json::json!({
            "name": name,
            "phone": row.try_get::<String, _>("phone").unwrap_or_default(),
            "isAdmin": row.try_get::<i16, _>("is_admin").unwrap_or(0) == 1,
            "createdAt": row.try_get::<String, _>("created_at").unwrap_or_default(),
            "bound": row.try_get::<i16, _>("bound").unwrap_or(1) == 1,
            "boundAt": row.try_get::<String, _>("bound_at").unwrap_or_default(),
            "source": row.try_get::<String, _>("source").unwrap_or_default(),
            "viewTeam": row.try_get::<String, _>("view_team").unwrap_or_default(),
        }));
    }
    // 兜底：表还没跑 008 迁移（没有 bound/source 列）时退回老查询
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT name, phone, is_admin, created_at FROM my_accounts WHERE name = ?",
    ))
    .bind(key)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    let name = row.try_get::<String, _>("name").ok()?;
    Some(serde_json::json!({
        "name": name,
        "phone": row.try_get::<String, _>("phone").unwrap_or_default(),
        "isAdmin": row.try_get::<i16, _>("is_admin").unwrap_or(0) == 1,
        "createdAt": row.try_get::<String, _>("created_at").unwrap_or_default(),
        "bound": true,
        "boundAt": "",
        "source": "admin",
        "viewTeam": "",
    }))
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
        } else if m.contains("Duplicate entry") {
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
                let sql = if force_reset {
                    "UPDATE my_accounts SET phone = ?, pass = ?, person_id = ?, bound = 1
                     WHERE name = ?"
                } else {
                    "UPDATE my_accounts SET phone = ?, person_id = ?, bound = 1 WHERE name = ?"
                };
                let sql = crate::pq::rewrite(sql);
                let mut q = sqlx::query(&sql).bind(&keep_phone);
                if force_reset {
                    q = q.bind(&pass_hash);
                }
                q = q.bind(pid as i64).bind(&old_name);
                match q.execute(pool).await {
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
                            note: if m.contains("Duplicate entry") {
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
                let res = sqlx::query(&crate::pq::rewrite(
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
                        let note = if m.contains("Duplicate entry") {
                            if m.contains("PRIMARY") {
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginFail {
    /// 账号（或手机号）查得到，但密码不对
    BadPassword,
    /// 这个账号 / 手机号根本没开通过
    NoAccount,
}

impl LoginFail {
    /// 给前端直接显示的文案（登录失败只有这两种，不再含糊）
    pub fn message(self) -> &'static str {
        match self {
            LoginFail::BadPassword => "密码错误",
            LoginFail::NoAccount => "账号还没开通",
        }
    }
}

/// 展示端登录（2026-10-10 起支持手机号+密码，姓名+密码继续兼容）：
/// 输入「账号」先按手机号查（纯数字 5~20 位才当手机号），没命中再按姓名查。
/// 成功返回命中的账号姓名（拿去台账里找人）；失败返回 [`LoginFail`] 说明是哪一种。
pub async fn my_account_check_login(
    st: &AppState,
    account: &str,
    pass: &str,
) -> std::result::Result<String, LoginFail> {
    let pool = w(st).map_err(|_| LoginFail::NoAccount)?;
    let pass = pass.trim();
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
        if let Some(row) = sqlx::query(&crate::pq::rewrite(
            "SELECT name, pass FROM my_accounts WHERE phone = ? LIMIT 1",
        ))
        .bind(&phone)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        {
            let stored: String = row.try_get("pass").map_err(|_| LoginFail::NoAccount)?;
            let name: String = row.try_get("name").map_err(|_| LoginFail::NoAccount)?;
            if verify_my_password(pass, &stored) {
                upgrade_my_password_if_plain(pool, &name, &stored, pass).await;
                return Ok(name);
            }
            /* 手机号查得到账号、密码不对 —— 明确是密码错，不是「没开通」。 */
            return Err(LoginFail::BadPassword);
        }
    }
    if let Some(row) = sqlx::query(&crate::pq::rewrite(
        "SELECT name, pass FROM my_accounts WHERE name = ? LIMIT 1",
    ))
    .bind(acct)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    {
        let stored: String = row.try_get("pass").map_err(|_| LoginFail::NoAccount)?;
        let name: String = row.try_get("name").map_err(|_| LoginFail::NoAccount)?;
        if verify_my_password(pass, &stored) {
            upgrade_my_password_if_plain(pool, &name, &stored, pass).await;
            return Ok(name);
        }
        /* 姓名查得到账号、密码不对。 */
        return Err(LoginFail::BadPassword);
    }
    Err(LoginFail::NoAccount)
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

/// 忘记密码：把已登记手机号的账号密码重置为 **CSPRNG 随机临时密码**
///
/// ⚠️ 2026-10-14 口径与残留风险（产品已确认保留此流程，故如实记录）：
/// 本入口（`/api/public/my/password/reset`）**不带身份证明** —— 只凭手机号即可触发，
/// 且**新密码会在响应里明文回给调用方**。因此「知道某人手机号」等价于「可以接管该账号」，
/// 这是无短信通道下的固有缺口，不是随机化密码能解决的（随机化只挡住了「密码可从手机号
/// 推算」，挡不住「接口把密码告诉你」）。
/// 已做的补偿：仅限登录门调用（个人页改走已登录自助改密 —— 2026-10-15 起为
/// `my_account_reset_password_self`，此前为 `my_account_self_set_password`）、
/// 受 `public_password_reset_allow` 限流、失败/成功均不打明文手机号。
/// 若要真正收口，需要服务端短信验证码或 OAuth 归属校验（见 docs/24 §P0-2）。
///
/// 返回 `(新密码, 受影响账号名列表)`：新密码只在 API 响应里下发这一次（请当场转达），
/// 账号名列表给调用方吊销该账号全部 my token（改密后旧登录态必须失效）。
pub async fn my_account_reset_password_random(
    st: &AppState,
    phone: &str,
) -> std::result::Result<(String, Vec<String>), String> {
    let phone = validate_my_phone(phone)?;
    if phone.is_empty() {
        return Err(biz("请输入手机号"));
    }
    let pool = w(st).map_err(|e| e.to_string())?;
    /* 只认**普通账号**（is_admin=0）：管理员观看账号不参与重置，
    否则知道手机号的人就能接管后台视角账号（审计 2026-10-11 修复）。
    文案不区分「没登记」和「管理员账号」，避免泄露台账。 */
    let names: Vec<String> = sqlx::query(&crate::pq::rewrite(
        "SELECT name FROM my_accounts WHERE phone = ? AND phone <> '' AND is_admin = 0",
    ))
    .bind(&phone)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("查询账号失败：{e}"))?
    .iter()
    .filter_map(|r| r.try_get::<String, _>("name").ok())
    .collect();
    /* 手机号查不到已登记的普通账号 = 这个号没绑定任何账号 → 直说「未绑定」
    （2026-10-14 用户口径：没绑定就显示未绑定，不再绕成「未登记或未开通」）。
    管理员观看账号同样不在这里命中（is_admin=0 过滤），与「未绑定」共用一句文案，
    避免泄露某个号是不是后台账号。 */
    if names.is_empty() {
        return Err(biz("该手机号未绑定账号"));
    }
    /* 重置后的密码 = CSPRNG 随机串（2026-10-14 审计 P0）：不再用手机号后四位。
    手机号后四位是可推导值，配上一个不带身份证明的重置入口 = 任意账号接管。
    展示端只把本次响应里下发的新密码回填一次，攻击者拿不到。 */
    let new_pass = crate::random::random_password();
    sqlx::query(&crate::pq::rewrite(
        "UPDATE my_accounts SET pass = ? WHERE phone = ? AND phone <> '' AND is_admin = 0",
    ))
    .bind(hash_my_password(&new_pass))
    .bind(&phone)
    .execute(pool)
    .await
    .map_err(|e| format!("重置失败：{e}"))?;
    Ok((new_pass, names))
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
/// 3. 新密码格式（复用 validate_my_account：4~20 位字母或数字）
///    → 「密码必须是 4~20 位字母、数字或点」
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
    // 当前密码：只有调用方给了旧密码才校验（与登录同口径，兼容存量明文口令）
    if let Some(old_pass) = old_pass {
        if !verify_my_password(old_pass.trim(), &stored) {
            return Err(biz("当前密码不正确"));
        }
    }
    /* 新密码复用统一口令规则（4~20 位字母或数字），不在这里另写一套校验。
    空密码 / 非法字符 / 长度越界统一给契约里的那句文案（姓名类错误原样保留）。
    2026-10-16：validate_my_account 的错误现在带 BIZ:: 前缀，contains 判断
    改用剥掉前缀后的文案（biz_msg），分流逻辑不变。 */
    let (_, new_pass) = validate_my_account(key, new_pass).map_err(|e| {
        if biz_msg(&e).contains("姓名") {
            e
        } else {
            biz("密码必须是 4~20 位字母、数字或点")
        }
    })?;
    /* 「新旧不能相同」只能在拿到旧密码时判：重置路径（`None`）没有旧密码可比，
    且冻结契约对重置只规定了「格式 / 管理员账号」两类 400，不额外加限制。 */
    if let Some(old_pass) = old_pass {
        if new_pass == old_pass.trim() {
            return Err(biz("新密码不能和当前密码相同"));
        }
    }
    sqlx::query(&crate::pq::rewrite(
        "UPDATE my_accounts SET pass = ? WHERE name = ?",
    ))
    .bind(hash_my_password(&new_pass))
    .bind(key)
    .execute(pool)
    .await
    .map_err(|e| format!("写入失败：{e}"))?;
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
/// 3. 新密码格式（4~20 位字母、数字或点）→ 「密码必须是 4~20 位字母、数字或点」
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
    路径（view_team 回落 Inherit），保证没跑迁移的库照常能登录。 */
    let sql = "SELECT name, phone, is_admin, bound, bound_at, source, view_team \
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
        })
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
/// 注册移除、忘记密码改 CSPRNG 随机临时密码、个人页改走已登录自助改密，
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

    /* ------------------------- 密码规则：4~20 位字母或数字 ------------------------- */

    /// 4~20 位字母数字放行（下限 4 是 2026-10-13 从 6 拍回来的）。
    #[test]
    fn password_rule_accepts_four_to_twenty_alphanumeric() {
        for p in [
            "9282",
            "abcd",
            "1234",
            "13312345678",
            "a1b2c3d4e5f6g7h8i9j0",
        ] {
            assert!(
                validate_my_account("凤夏", p).is_ok(),
                "{p}（{} 位字母数字）应放行",
                p.len()
            );
        }
        assert_eq!("a1b2c3d4e5f6g7h8i9j0".len(), 20, "20 位是上限边界值");
    }

    /// 上限 20：超过 20 位一律拒绝。
    #[test]
    fn password_rule_rejects_too_long() {
        assert!(
            validate_my_account("凤夏", &"1".repeat(21)).is_err(),
            "21 位数字 → 拒绝"
        );
        assert!(
            validate_my_account("凤夏", &"a".repeat(21)).is_err(),
            "21 位字母 → 拒绝"
        );
        assert!(
            validate_my_account("凤夏", &"z9".repeat(11)).is_err(),
            "22 位混合 → 拒绝"
        );
    }

    /// 下限 4：1~3 位一律拒绝。
    #[test]
    fn password_rule_rejects_too_short() {
        for p in ["1", "12", "123", "a", "ab", "abc", "a1", "1a2"] {
            assert!(
                validate_my_account("凤夏", p).is_err(),
                "{p}（{} 位）少于 4 位 → 拒绝",
                p.len()
            );
        }
    }

    /// 空 / 纯空白：trim 后为空走「不能为空」那条文案，不落到长度分支。
    #[test]
    fn password_rule_rejects_blank() {
        assert!(validate_my_account("凤夏", "").is_err(), "空密码 → 拒绝");
        assert!(
            validate_my_account("凤夏", "   ").is_err(),
            "纯空格 trim 后为空 → 拒绝"
        );
        assert!(
            validate_my_account("凤夏", "\t\n ").is_err(),
            "制表符 / 换行 trim 后为空 → 拒绝"
        );
        // 自助改密表单靠这条文案定位问题（留空 = 要保留原密码就传当前密码）
        let err = validate_my_account("凤夏", "").unwrap_err();
        assert!(err.contains("不能为空"), "实际文案：{err}");
    }

    /// 符号一律拒绝（点除外，2026-10-17 放开）。注意长度都落在 4~20 之间，
    /// 否则命中的是长度分支，测不到字符分支。
    #[test]
    fn password_rule_rejects_symbols() {
        for p in [
            "133-1234-5678", // 带横线（手机号常见写法）
            "ab_123",        // 带下划线
            "12 34",         // 内含空格（trim 清不掉中间的）
            "abcd!",         // 标点
            "ab@c#",         // 邮箱常用符号
            "密码abcd",      // 中文
            "ab\u{1f600}cd", // emoji（按非字母数字拒绝）
            "a/b\\c",        // 路径分隔符
        ] {
            assert!(
                validate_my_account("凤夏", p).is_err(),
                "{p} 含字母数字点之外的字符 → 拒绝"
            );
        }
        // 点放行（2026-10-17：Aa181206.. 这类自定义密码要能用）
        for p in ["Aa181206..", "a.c.d", "8888.", ".9999"] {
            assert!(
                validate_my_account("凤夏", p).is_ok(),
                "{p} 含点 → 应放行"
            );
        }
    }

    /// 姓名也一并校验：空、纯空格、超 20 字拒绝；中文名不被误伤。
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

    /// 返回值必须是 trim 过的：前后空格若连同哈希写进库，会造成「看着一样、
    /// 登录不上」这种最难查的问题。
    #[test]
    fn password_rule_returns_trimmed_values() {
        let (name, pass) = validate_my_account("  凤夏 ", "  9282  ").unwrap();
        assert_eq!(name, "凤夏", "姓名要 trim");
        assert_eq!(pass, "9282", "密码要 trim");
        // 中间空格不属于 trim 范围，且本来就过不了字符校验
        assert!(validate_my_account("凤夏", " 92 82 ").is_err());
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

    /* ----------------- 忘记密码：必须是随机临时密码（不是手机号后四位） ----------------- */

    /// 忘记密码下发的是 `random_password()` 的 10 位随机串，**不是**手机号后四位。
    /// 2026-10-12 审计把这条从「可推算」改成「不可推算」，这里钉住不许回退 ——
    /// 一旦有人为了「方便运营」改回后四位，这条就红。
    #[test]
    fn reset_password_is_random_not_phone_last4() {
        const PHONE: &str = "13800138000";
        // 开号初始密码仍走后四位（产品口径，与重置是两条不同的路）
        assert_eq!(initial_password(PHONE), "8000");

        let tmp = crate::random::random_password();
        assert_eq!(tmp.len(), 10, "重置临时密码应为 10 位随机串");
        assert!(
            tmp.chars().all(|c| c.is_ascii_alphanumeric()),
            "重置临时密码 {tmp:?} 必须是纯字母数字"
        );
        assert!(
            validate_my_account("凤夏", &tmp).is_ok(),
            "随机临时密码必须能用于登录（落在 4~20 位字母数字规则内）"
        );
    }

    /// 重置临时密码两次不同 —— 连点两次不能拿到同一个串。
    #[test]
    fn reset_password_differs_between_calls() {
        let a = crate::random::random_password();
        let b = crate::random::random_password();
        assert_ne!(a, b, "两次重置不应下发同一个临时密码");
    }

    /* ------------- 登录失败原因：密码错 ≠ 账号没开通（2026-10-14 用户口径） ------------- */

    /// 登录失败文案只有两种，且必须能区分「密码错」和「没开通」——
    /// 前端直接显示后端给的这句，所以这两个字面量是契约，改了要同步改前端兜底。
    #[test]
    fn login_fail_message_says_which_one() {
        assert_eq!(LoginFail::BadPassword.message(), "密码错误");
        assert_eq!(LoginFail::NoAccount.message(), "账号还没开通");
        assert_ne!(
            LoginFail::BadPassword.message(),
            LoginFail::NoAccount.message(),
            "两种失败不能塌成同一句，否则又回到「账号或密码不对，或账号还没开通」"
        );
    }

    /// 两种失败都必须**不含**对方的字样：文案里不能出现「或」这种和稀泥的措辞，
    /// 也不能把「没开通」塞进密码错的提示里。
    #[test]
    fn login_fail_messages_do_not_conflate() {
        for m in [
            LoginFail::BadPassword.message(),
            LoginFail::NoAccount.message(),
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
}
