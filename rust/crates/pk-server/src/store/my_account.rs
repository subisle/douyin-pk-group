//! store::my_account —— 自 store.rs 按业务域拆分（对外语义不变）。

use super::*; // 共享 w() / now_str() / AppState 等内部工具

/// 抖音号格式：6~20 位字母 / 数字 / 下划线 / 点（抖音号官方口径的宽松版）
pub fn validate_douyin_no(v: &str) -> std::result::Result<String, String> {
    let v = v.trim();
    let ok = (6..=20).contains(&v.len())
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if !ok {
        return Err("抖音号格式不对：6~20 位字母、数字、下划线或点".into());
    }
    Ok(v.to_string())
}

/* ------------------------------ 我的页面账号 ------------------------------ */

/// 校验姓名/密码。Err(错误文案)，Ok((姓名, 密码))。
/// **密码为 6~20 位字母或数字（2026-10-12 安全审计 M4：下限 4→6）**：
/// 去首尾空格后必须全是 ASCII 字母/数字，不允许空格、横线等分隔符。
/// 历史口径备注：2026-10-10 曾放宽为 4 位纯数字/字母；4~5 位存量密码仍可
/// 登录（校验只在设置/修改时执行），但新设密码一律 ≥6 位。
pub fn validate_my_account(
    name: &str,
    pass: &str,
) -> std::result::Result<(String, String), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("姓名不能为空".into());
    }
    if name.chars().count() > 20 {
        return Err("姓名太长（最多 20 字）".into());
    }
    let pass = pass.trim();
    if pass.is_empty() {
        return Err("密码不能为空；已有账号要保留原密码请传当前密码".into());
    }
    if !pass.chars().all(|c| c.is_ascii_alphanumeric()) || pass.len() < 6 || pass.len() > 20 {
        return Err("密码必须是 6~20 位字母或数字".into());
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
        return Err("手机号格式不对：应为 5~20 位数字".into());
    }
    Ok(p)
}

/// 全部账号（管理端列表用），按添加时间倒序。
/// `bound`/`source` 来自 008 迁移；表还没跑到 008 时退回不带这两列的查询（一律视为已绑定）。
pub async fn my_account_list(st: &AppState) -> Vec<serde_json::Value> {
    let Ok(pool) = w(st) else {
        return Vec::new();
    };
    let rows = match sqlx::query(
        "SELECT name, phone, is_admin, created_at, bound, bound_at, source \
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
            return Err("手机号已存在（已绑定其它姓名）".into());
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
        这里与注册路径（store.rs 的 my_account_register）翻成同一套中文文案。 */
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
    let row = sqlx::query(&crate::pq::rewrite(
        "SELECT name, phone, is_admin, created_at, bound, bound_at, source \
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

/// 初始密码 = CSPRNG 随机密码（2026-10-12 安全审计修复）。
///
/// 旧口径「手机号后四位」被废：手机号对同事/粉丝半公开，后四位等于把初始口令
/// 写死在公开信息里，拿到手机号就能登录别人的展示端账号。
/// 现在统一走 `crate::random::random_password()`（10 位无混淆字符），
/// 只在开号/重置的 API 响应里下发一次，请管理员当场转达。
pub fn initial_password() -> String {
    crate::random::random_password()
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
        return Err("无效的主播 id".into());
    }
    let phone = validate_my_phone(phone)?;
    let pool = w(st).map_err(|e| e.to_string())?;
    if !phone.is_empty() {
        let taken_person =
            sqlx::query(&crate::pq::rewrite(crate::dialect::PERSON_PHONE_TAKEN_SQL))
            .bind(&phone)
            .bind(person_id as i64)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .is_some();
        if taken_person {
            return Err(PhoneClash::Person.message(&phone));
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
            return Err(PhoneClash::Account.message(&phone));
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
    /// （历史上有过 skipped = 定不了初始密码；2026-10-12 起初始密码恒为 CSPRNG 随机，不再跳过）
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
/// - 初始密码：**CSPRNG 随机密码**（2026-10-12 审计修复，废弃「手机号后四位」）；
///   `default_pass` 入参保留仅为兼容旧调用，**不再使用**（统一口令本身就是审计要消除的风险）
/// - 已存在账号（按 person_id 优先、name 兜底）→ 更新：补 person_id / 手机号，**默认不动密码**
///   （重跑一遍批量不会把 104 个人的密码全刷掉）；`force_reset = true` 才重置（也重置为随机密码）
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
        return Err("名单为空".into());
    }
    if inputs.len() > BATCH_ACCOUNT_MAX {
        return Err(format!(
            "一次最多开 {} 个账号（本次 {} 个，请分批提交）",
            BATCH_ACCOUNT_MAX,
            inputs.len()
        ));
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

        /* 初始密码：CSPRNG 随机（审计 2026-10-12，废弃手机号后四位 / 统一口令）。
        每个账号独立随机，只在响应里下发一次（initialPass），请管理员当场转达。 */
        let pass = initial_password();
        if let Err(e) = validate_my_account(&name, &pass) {
            out.push(BatchAccountRow {
                person_id: pid,
                name,
                phone: want_phone,
                status: "failed",
                note: e,
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
                与 my_account_register 同口径 —— 主播拿到号就能登，不用再绑一次。 */
                let res = sqlx::query(&crate::pq::rewrite(
                    "INSERT INTO my_accounts
                       (name, phone, pass, is_admin, created_at, bound, bound_at, source, person_id)
                     VALUES (?, ?, ?, 0, ?, 1, ?, 'batch', ?)",
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
/// **未配置环境变量时一个账号都不创建**（默认安全）：以前硬编码 shuai/shuai 随启动重建，
/// 口令公开可猜 → 展示端登录门形同虚设。库里的存量账号不受影响，删除走管理端。
pub fn builtin_my_accounts_from_env() -> Vec<(String, String)> {
    let raw = std::env::var("BUILTIN_MY_ACCOUNTS").unwrap_or_default();
    let mut out = Vec::new();
    for pair in raw.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (name, pass) = match pair.split_once(':') {
            Some((n, p)) => (n.trim(), p.trim()),
            None => (pair, ""),
        };
        if name.is_empty() {
            continue;
        }
        // 密码没写 = 账号密码相同（历史行为），但空密码不行（my_accounts.pass 非空）
        let pass = if pass.is_empty() { name } else { pass };
        out.push((name.to_string(), pass.to_string()));
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

    for (name, pass) in &accounts {
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
            if let Err(e) = sqlx::query(&crate::pq::rewrite(
                "UPDATE my_accounts SET is_admin = 1 WHERE name = ?",
            ))
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
            "INSERT INTO my_accounts (name, phone, pass, is_admin, created_at)
             VALUES (?, '', ?, 1, ?)",
        ))
        .bind(name)
        .bind(hash_my_password(pass))
        .bind(&now)
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

/// 展示端登录（2026-10-10 起支持手机号+密码，姓名+密码继续兼容）：
/// 输入「账号」先按手机号查（纯数字 5~20 位才当手机号），没命中再按姓名查。
/// 返回命中的账号姓名（拿去台账里找人），全都没对上 → None。
pub async fn my_account_check_login(st: &AppState, account: &str, pass: &str) -> Option<String> {
    let pool = w(st).ok()?;
    let pass = pass.trim();
    if pass.is_empty() {
        return None;
    }
    let acct = account.trim();
    if acct.is_empty() {
        return None;
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
            let stored: String = row.try_get("pass").ok()?;
            let name: String = row.try_get("name").ok()?;
            if verify_my_password(pass, &stored) {
                upgrade_my_password_if_plain(pool, &name, &stored, pass).await;
                return Some(name);
            }
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
        let stored: String = row.try_get("pass").ok()?;
        let name: String = row.try_get("name").ok()?;
        if verify_my_password(pass, &stored) {
            upgrade_my_password_if_plain(pool, &name, &stored, pass).await;
            return Some(name);
        }
    }
    None
}

/* ------------------- 手机号自助注册 + 绑定主播（2026-10-10 用户口径） ------------------- */

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
    /// admin（后台手建）/ register（手机号自助注册）
    pub source: String,
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

/// 短信验证码 = 密码：4~20 位纯数字（用户口径「验证码数字就是密码」）
pub fn validate_my_code(code: &str) -> std::result::Result<String, String> {
    let c = code.trim();
    if c.is_empty() {
        return Err("请输入短信验证码".into());
    }
    if !c.chars().all(|ch| ch.is_ascii_digit()) || c.len() < 4 || c.len() > 20 {
        return Err("验证码格式不对：应为 4~20 位数字（验证码就是密码）".into());
    }
    Ok(c.to_string())
}

/// 忘记密码：把已登记手机号的账号密码重置为 **CSPRNG 随机密码**
/// （2026-10-12 审计修复：废弃「手机号后四位」——手机号半公开，后四位等于公开口令）。
///
/// 返回 `(新密码, 受影响账号名列表)`：新密码只在 API 响应里下发这一次（请当场转达），
/// 账号名列表给调用方吊销该账号全部 my token（改密后旧登录态必须失效）。
pub async fn my_account_reset_password_random(
    st: &AppState,
    phone: &str,
) -> std::result::Result<(String, Vec<String>), String> {
    let phone = validate_my_phone(phone)?;
    if phone.is_empty() {
        return Err("请输入注册手机号".into());
    }
    let pool = w(st).map_err(|e| e.to_string())?;
    /* 只认「自助注册的普通账号」（is_admin=0）：管理员观看账号不参与重置，
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
    if names.is_empty() {
        return Err("该手机号还没有注册或未登记".into());
    }
    let new_pass = initial_password();
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

/// 查账号信息（登录名或手机号都认）。表还没跑 008 迁移时退回老查询：
/// 老行都是管理员手建的 → 一律视为「已绑定」，登录/我的页照常，只有新注册用不了。
pub async fn my_account_info(st: &AppState, key: &str) -> Option<MyAccountInfo> {
    let pool = w(st).ok()?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    let sql = "SELECT name, phone, is_admin, bound, bound_at, source FROM my_accounts \
               WHERE name = ? OR (phone <> '' AND phone = ?) LIMIT 1";
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
        })
}

/// 展示端自助注册（2026-10-12 审计修订）：**手机号 = 账号，短信验证码 = 密码，注册时直接填主播姓名**。
///
/// 规则：
/// - 手机号 / 验证码都是纯数字；手机号 5~20 位、验证码 4~20 位
/// - **主播姓名必须能在台账（person 表）里精确查到**，查不到直接拒绝（避免随便填个名字就进站）
/// - 手机号已存在（管理员手建或已注册）→ 拒绝，引导去登录
/// - 该姓名已经有账号了 → 拒绝（一人一号）
/// - **注册不再立即绑定**（2026-10-12 安全审计：自助绑定无审核会被冒领）：账号先以手机号
///   占位名落库（`bound=0`，回到 008 迁移的原始设计），同时自动生成一条
///   `my_bind_requests(pending)` 申请，管理员审核通过后才真正绑定。
///
/// 返回 `(账号登录名, 主播姓名, personId, 团别)`。审核通过前登录名 = 手机号占位，
/// 绑定通过后由 `my_account_bind` 改写为台账姓名（登录照样支持手机号）。
pub async fn my_account_register(
    st: &AppState,
    phone: &str,
    code: &str,
    anchor_name: &str,
    wb_state: &str,
) -> std::result::Result<(String, String, i64, String), String> {
    let pool = w(st).map_err(|e| e.to_string())?;
    let phone = validate_my_phone(phone)?;
    if phone.is_empty() {
        return Err("请输入手机号".into());
    }
    let code = validate_my_code(code)?;

    /* 主播姓名：必须在台账里（全台账搜，注册时还不知道团别） */
    let anchor = anchor_name.trim();
    if anchor.is_empty() {
        return Err("请输入主播姓名（艺名）".into());
    }
    if anchor.chars().count() > 20 {
        return Err("姓名太长（最多 20 字）".into());
    }
    let Some((person_id, person_name, team)) = anchor_lookup_by_name(st, anchor).await? else {
        return Err(format!(
            "主播库里没有「{anchor}」这个姓名，请核对艺名；确认无误请联系管理员先加进台账"
        ));
    };
    /* 一次查询覆盖三种占用（少两次往返）：同名主播已占号 / 手机号已占 / 历史「手机号占位名」。
    真正的唯一性由主键(name) + uk_my_phone_uniq(phone) 兜底，这里只为给出准确文案。 */
    let clashes = sqlx::query(&crate::pq::rewrite(
        "SELECT name, phone FROM my_accounts \
         WHERE name IN (?, ?) OR (phone <> '' AND phone = ?) LIMIT 5",
    ))
    .bind(&person_name)
    .bind(&phone)
    .bind(&phone)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("查重失败：{e}"))?;
    for row in &clashes {
        let row_name: String = row.try_get("name").unwrap_or_default();
        let row_phone: String = row.try_get("phone").unwrap_or_default();
        if !row_phone.is_empty() && row_phone == phone {
            return Err("该手机号已存在（已注册），请直接登录；忘记密码请联系管理员".into());
        }
        if row_name == person_name {
            return Err("该主播已经有账号了，请直接登录；忘记密码可点「忘记密码？」重置".into());
        }
        if row_name == phone {
            return Err("该手机号已存在（已注册），请直接登录".into());
        }
    }
    let pass_hash = hash_my_password(&code);
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    /* 审计 2026-10-12：注册**不再立即绑定**（自报姓名直接 bound=1 等于无审核冒领）。
    回到 008 的原始设计：name 先填手机号占位、bound=0，绑定走 my_bind_requests 审核流。 */
    sqlx::query(&crate::pq::rewrite(
        "INSERT INTO my_accounts (name, phone, pass, is_admin, created_at, bound, bound_at, source, wb_state)
         VALUES (?, ?, ?, 0, ?, 0, '', 'register', ?)",
    ))
    .bind(&phone)
    .bind(&phone)
    .bind(&pass_hash)
    .bind(&now)
    .bind(wb_state.trim())
    .execute(pool)
    .await
    .map_err(|e| {
        let m = e.to_string();
        if m.contains("Duplicate entry") {
            /* 并发穿过上面的预检：按冲突的键给文案（PRIMARY = name，其余 = 手机号唯一键） */
            if m.contains("PRIMARY") {
                "该手机号已存在（已注册），请直接登录；忘记密码可点「忘记密码？」重置".to_string()
            } else {
                "该手机号已存在（已注册），请直接登录；忘记密码请联系管理员".to_string()
            }
        } else if m.contains("Unknown column") {
            "缺少数据库迁移 008（先跑一次 pk-server migrate）".to_string()
        } else {
            format!("写入失败：{m}")
        }
    })?;
    /* 自动生成 pending 绑定申请（注册即申请，审核通过才生效）。
    017 未跑时申请写入失败不阻断注册 —— 账号已建好，用户登录后仍可在「我的」页手动提交。 */
    if let Err(e) = my_bind_request_submit(st, &phone, person_id, &person_name, &team).await {
        tracing::warn!(account = %phone, anchor = %person_name, error = %e,
            "注册后自动生成绑定申请失败（账号已建好，可登录后手动提交）");
    }
    Ok((phone, person_name, person_id, team))
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
        return Err("请先登录".into());
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
        return Err("该账号已绑定主播，不能自行更改（需要改绑请联系管理员）".into());
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
        return Err("该主播不存在或已删除".to_string());
    }
    if status == "left" {
        return Err("该主播已离团，不能绑定".into());
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
        return Err("该主播已存在绑定的账号，请联系管理员".into());
    }

    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let affected = sqlx::query(&crate::pq::rewrite(
        "UPDATE my_accounts SET name = ?, bound = 1, bound_at = ? WHERE name = ? AND bound = 0",
    ))
    .bind(&person_name)
    .bind(&now)
    .bind(login_name)
    .execute(pool)
    .await
    .map_err(|e| format!("绑定失败：{e}"))?
    .affected();
    if affected == 0 && !admin {
        // 并发下被别人先绑走了：用户侧不允许覆盖
        return Err("该账号已绑定主播，不能自行更改（需要改绑请联系管理员）".into());
    }
    if affected == 0 {
        sqlx::query(&crate::pq::rewrite(
            "UPDATE my_accounts SET name = ?, bound = 1, bound_at = ? WHERE name = ?",
        ))
        .bind(&person_name)
        .bind(&now)
        .bind(login_name)
        .execute(pool)
        .await
        .map_err(|e| format!("绑定失败：{e}"))?;
    }
    Ok(MyBindResult {
        name: person_name,
        team,
        person_id,
    })
}
