//! 公开端团别站点后端（阶段 D，docs/08）：
//! - /st/m、/st/f 两个独立链接，链接即团别
//! - 后端级显隐开关：关闭后不下发数据（不是前端藏）
//! - 「我的」页面验证：姓名+密码（纯数字；账号由管理员在后台手动添加，没有注册）
//! - 公开 API 一律 DTO 白名单，绝不下发密码、抖音号、全量候选名单

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::dialect::{self, is_live_wave};
use crate::mysql;
use crate::routes::ApiError;
use crate::state::AppState;
use axum::response::{IntoResponse, Response};

type ApiResult = Result<Json<Value>, ApiError>;
type ArcState = Arc<AppState>;

/// 公开端一律读「当前生效数据源」，不能写死某个源 id。
/// 早先版本在这里硬编码 `LOCAL`，结果把源切到 A（sqlpub）或直接只配 A 时，
/// 展示页全部接口报「未知数据源 LOCAL」——页面看着是空的，库里其实有数据。
fn sid(st: &AppState) -> String {
    st.active_id()
}

/* ------------------------------- 显隐开关 ------------------------------- */

/// 公开站点配置（2026-10-10 起存 MySQL config 表，cfg_key = public_settings；
/// 每个公开接口都要读它，进程内缓存 30s，save 时立即刷新缓存）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PublicSettings {
    pub male_enabled: bool,
    pub female_enabled: bool,
    pub daily: bool,
    pub monthly: bool,
    pub yearly: bool,
    pub duration: bool,
    pub trend: bool,
    /// 每日报告的音浪数值（2026-10-09）：关掉后榜单不下发任何数字，名次/名次变动照常
    pub wave: bool,
    /// 每日报告页显隐（2026-10-10）：关掉后接口不下发榜单、导航整体消失
    #[serde(default = "default_true")]
    pub data_enabled: bool,
    /// 分组页显隐（2026-10-10）：关掉后导航消失
    #[serde(default = "default_true")]
    pub group_enabled: bool,
    /// 族谱页显隐（2026-10-10）：关掉后接口不下发、导航消失（男团专属页）
    #[serde(default = "default_true")]
    pub lineage_enabled: bool,
    /// 分组子页（按赛事类型）显隐（2026-10-10）：key = 赛事类型，缺省 true。
    /// 关掉的类型子导航消失，该类型子页按「暂未公布」处理。
    #[serde(default)]
    pub group_types: std::collections::HashMap<String, bool>,
    /// 是否开启登录门（2026-10-10）：关 = 全面开放，打开就能看（但没有「我的」页）；
    /// 开 = 全站要登录（现状）
    #[serde(default = "default_true")]
    pub login_required: bool,
    /// 展示端手机号注册开关（2026-10-10 用户口径「注册先不开」）：**默认关**。
    /// 关 = 登录门不显示「注册」页签，注册/授权接口一律拒绝（登录、绑定照常）。
    /// 想开时在管理端「公开站点设置 → 开放手机号注册」打开即可。
    #[serde(default)]
    pub my_register: bool,
}

fn default_true() -> bool {
    true
}

/// 分组页/子页显隐的过滤口径（public::plans 用）：页关 → 全部不发；
/// 子页关 → 该类型不发；没配置过的类型 = 开（老方案兼容）。
pub fn plan_visible_by_settings(s: &PublicSettings, event_type: &str) -> bool {
    s.group_enabled && s.group_type_enabled(event_type)
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    #[test]
    fn unified_display_is_enabled_when_any_legacy_team_is_enabled() {
        let mut s = PublicSettings::default();
        assert!(s.team_enabled("all"));

        s.male_enabled = false;
        assert!(s.team_enabled("all"));

        s.female_enabled = false;
        assert!(!s.team_enabled("all"));
        assert!(!s.team_enabled("unknown"));
    }

    #[test]
    fn plan_visibility_follows_page_and_type_switches() {
        let mut s = PublicSettings::default();
        // 全开：什么类型都可见
        assert!(plan_visible_by_settings(&s, "初级争霸赛小组赛"));
        assert!(plan_visible_by_settings(&s, ""), "未分类的老方案也可见");

        // 分组页整体关掉：任何类型都不可见
        s.group_enabled = false;
        assert!(!plan_visible_by_settings(&s, "初级争霸赛小组赛"));
        s.group_enabled = true;

        // 单独关一个子页：只有它不可见，其它照常
        s.group_types.insert("初级争霸赛小组赛".into(), false);
        assert!(!plan_visible_by_settings(&s, "初级争霸赛小组赛"));
        assert!(plan_visible_by_settings(&s, "月终争霸赛"));
        assert!(plan_visible_by_settings(&s, ""), "没配置过的类型按开处理");

        // 反向写法（只存开的那个）也不影响其它类型
        s.group_types.clear();
        s.group_types.insert("月终争霸赛".into(), true);
        assert!(plan_visible_by_settings(&s, "初级争霸赛小组赛"));
    }
}

impl Default for PublicSettings {
    fn default() -> Self {
        Self {
            male_enabled: true,
            female_enabled: true,
            daily: true,
            monthly: true,
            yearly: true,
            duration: true,
            trend: true,
            wave: true,
            data_enabled: true,
            group_enabled: true,
            lineage_enabled: true,
            group_types: Default::default(),
            login_required: true,
            // 注册默认关（用户口径 2026-10-10「注册先不开」）
            my_register: false,
        }
    }
}

static PS_CACHE: std::sync::OnceLock<
    std::sync::Mutex<Option<(PublicSettings, std::time::Instant)>>,
> = std::sync::OnceLock::new();

fn ps_cache() -> &'static std::sync::Mutex<Option<(PublicSettings, std::time::Instant)>> {
    PS_CACHE.get_or_init(std::sync::Mutex::default)
}

impl PublicSettings {
    pub async fn load(st: &AppState) -> Self {
        // 命中 30s 内的缓存直接用；未命中查库回填
        if let Ok(guard) = ps_cache().lock() {
            if let Some((s, at)) = guard.as_ref() {
                if at.elapsed() < std::time::Duration::from_secs(30) {
                    return s.clone();
                }
            }
        }
        let s: PublicSettings = crate::store::config_get(st, "public_settings")
            .await
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if let Ok(mut guard) = ps_cache().lock() {
            *guard = Some((s.clone(), std::time::Instant::now()));
        }
        s
    }

    pub async fn save(&self, st: &AppState) {
        if let Ok(s) = serde_json::to_string(self) {
            crate::store::config_set(st, "public_settings", &s).await;
        }
        if let Ok(mut guard) = ps_cache().lock() {
            *guard = Some((self.clone(), std::time::Instant::now()));
        }
    }

    fn team_enabled(&self, team: &str) -> bool {
        match team {
            // 2026-10-11 起首页是不分男女的展示端：all 一次读全量数据。
            // male/female 保留兼容旧链接，但同样不再按性别裁剪数据。
            "all" | "male" | "female" => self.male_enabled || self.female_enabled,
            _ => false,
        }
    }

    /// 某个分组子页（赛事类型）是否开放。没配置过的类型 = 开放（老方案兼容）。
    pub fn group_type_enabled(&self, event_type: &str) -> bool {
        self.group_types.get(event_type).copied().unwrap_or(true)
    }

    /// 管理端/公开端都能看到的 JSON 形态（不含敏感项）
    pub fn to_json(&self) -> Value {
        json!({
            "maleEnabled": self.male_enabled,
            "femaleEnabled": self.female_enabled,
            "daily": self.daily,
            "monthly": self.monthly,
            "yearly": self.yearly,
            "duration": self.duration,
            "trend": self.trend,
            "wave": self.wave,
            "dataEnabled": self.data_enabled,
            "groupEnabled": self.group_enabled,
            "lineageEnabled": self.lineage_enabled,
            "groupTypes": self.group_types,
            "loginRequired": self.login_required,
            "myRegister": self.my_register,
        })
    }
}

/* ------------------------------- 管理端 ------------------------------- */

/// GET /api/admin/public/settings
pub async fn get_settings(State(st): State<ArcState>) -> ApiResult {
    Ok(Json(PublicSettings::load(&st).await.to_json()))
}

/// POST /api/admin/public/settings：整包保存
pub async fn update_settings(State(st): State<ArcState>, Json(body): Json<Value>) -> ApiResult {
    let mut s = PublicSettings::load(&st).await;
    let flag =
        |key: &str, cur: bool| -> bool { body.get(key).and_then(|v| v.as_bool()).unwrap_or(cur) };
    s.male_enabled = flag("maleEnabled", s.male_enabled);
    s.female_enabled = flag("femaleEnabled", s.female_enabled);
    s.daily = flag("daily", s.daily);
    s.monthly = flag("monthly", s.monthly);
    s.yearly = flag("yearly", s.yearly);
    s.duration = flag("duration", s.duration);
    s.trend = flag("trend", s.trend);
    s.wave = flag("wave", s.wave);
    s.data_enabled = flag("dataEnabled", s.data_enabled);
    s.group_enabled = flag("groupEnabled", s.group_enabled);
    s.lineage_enabled = flag("lineageEnabled", s.lineage_enabled);
    s.login_required = flag("loginRequired", s.login_required);
    s.my_register = flag("myRegister", s.my_register);
    // 分组子页：传了 groupTypes（对象）就整包替换；没传保持原样。
    // 未知类型直接存下来也没关系 —— group_type_enabled 对没配置的类型按开处理，
    // 存 false 的才关。
    if let Some(gt) = body.get("groupTypes").and_then(|v| v.as_object()) {
        s.group_types = gt
            .iter()
            .filter_map(|(k, v)| v.as_bool().map(|b| (k.clone(), b)))
            .collect();
    }
    s.save(&st).await;
    crate::events::emit(crate::events::SCOPE_SETTINGS, "公开站点设置已更新");
    Ok(Json(json!({ "ok": true, "settings": s.to_json() })))
}

/* ------------------------------- 引导弹窗 ------------------------------- */

/// 引导弹窗（config 表 cfg_key = announcement）：多步骤引导，逐步「下一步」。
/// 展示端打开时弹一次，用户点「完成 / 跳过」后按 version 记住已读，
/// 直到下次保存（version 自增）再弹。
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Announcement {
    pub enabled: bool,
    pub steps: Vec<AnnounceStep>,
    /// 每次保存自增；前端按版本判断是否已读（关闭后不再弹，直到版本变化）
    pub version: u64,
}

/// 引导弹窗的一步：标题 + 正文 + 图标（Material Symbols 名）。
///
/// `icon` 是 2026-10-12 大厂化改造加的可选字段：每步一个图标让引导更像
/// 「新功能介绍」而不是系统告警。缺省（老配置 / 前端不传）按步序号轮换
/// [`DEFAULT_STEP_ICONS`]，所以旧数据不迁移也能有图标。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
pub struct AnnounceStep {
    pub title: String,
    /// Material Symbols 图标名，走前端白名单校验；空 = 按步序号取默认
    #[serde(default)]
    pub icon: String,
    pub content: String,
}

/// 每步默认图标（步序号 % 长度）。取 Material Symbols 里语义中性、
/// 渲染稳定的几个，避免运营不填时露出「campaign」满屏同一个。
pub const DEFAULT_STEP_ICONS: &[&str] = &[
    "auto_awesome",
    "military_tech",
    "qr_code_scanner",
    "notifications_active",
    "insights",
    "tips_and_updates",
];

/// 图标白名单：只放这几个 Material Symbols 名，别让任意字符串进 DOM。
/// 展示端 st_page.html 也有一份同名列白名单做双重校验，两边保持一致。
pub const STEP_ICON_WHITELIST: &[&str] = &[
    "auto_awesome",
    "military_tech",
    "qr_code_scanner",
    "notifications_active",
    "insights",
    "tips_and_updates",
    "campaign",
    "star",
    "bolt",
    "groups",
    "emoji_events",
    "local_fire_department",
    "trending_up",
    "calendar_month",
    "chat_bubble",
    "lock",
    "person",
    "smart_toy",
    "workspace_premium",
];

impl Announcement {
    pub async fn load(st: &AppState) -> Self {
        crate::store::config_get(st, "announcement")
            .await
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub async fn save(&self, st: &AppState) {
        if let Ok(s) = serde_json::to_string(self) {
            crate::store::config_set(st, "announcement", &s).await;
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "steps": self.steps.iter().enumerate().map(|(i, s)| json!({
                "title": s.title,
                "content": s.content,
                // 下发前把空 icon 归一成默认值，前端不用再猜第几步该用什么
                "icon": Self::icon_for(s, i),
            })).collect::<Vec<_>>(),
            "version": self.version,
        })
    }

    /// 这一步实际用的图标：填了合法值就原样用，否则按步序号取默认。
    fn icon_for(s: &AnnounceStep, index: usize) -> String {
        let trimmed = s.icon.trim();
        if !trimmed.is_empty() && STEP_ICON_WHITELIST.contains(&trimmed) {
            return trimmed.to_string();
        }
        DEFAULT_STEP_ICONS
            .get(index % DEFAULT_STEP_ICONS.len())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "campaign".to_string())
    }
}

/// GET /api/admin/announcement
pub async fn get_announcement(State(st): State<ArcState>) -> ApiResult {
    Ok(Json(Announcement::load(&st).await.to_json()))
}

/// POST /api/admin/announcement {enabled?, steps?: [{title, content}]}
pub async fn update_announcement(State(st): State<ArcState>, Json(body): Json<Value>) -> ApiResult {
    let mut a = Announcement::load(&st).await;
    if let Some(v) = body.get("enabled").and_then(|v| v.as_bool()) {
        a.enabled = v;
    }
    if let Some(steps) = body.get("steps").and_then(|v| v.as_array()) {
        a.steps = steps
            .iter()
            .filter_map(|s| {
                let title = s.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let content = s.get("content").and_then(|v| v.as_str()).unwrap_or("");
                // 完全空的步骤直接丢弃
                if title.trim().is_empty() && content.trim().is_empty() {
                    return None;
                }
                let icon = s.get("icon").and_then(|v| v.as_str()).unwrap_or("").trim();
                Some(AnnounceStep {
                    title: title.trim().chars().take(64).collect(),
                    // 白名单外的图标名直接丢掉（回落默认），任意字符串不进配置
                    icon: if STEP_ICON_WHITELIST.contains(&icon) {
                        icon.chars().take(48).collect()
                    } else {
                        String::new()
                    },
                    content: content.trim().chars().take(2000).collect(),
                })
            })
            .collect();
    }
    // 每次保存都让版本前进，前端据此重新弹出（内容没变也弹，保证运营可主动重发）
    a.version += 1;
    a.save(&st).await;
    Ok(Json(json!({ "ok": true, "announcement": a.to_json() })))
}

/* --------------------- 我的页面账号（管理端手动添加） --------------------- */

/// GET /api/admin/my-accounts：账号列表
pub async fn admin_my_accounts(State(st): State<ArcState>) -> ApiResult {
    Ok(Json(
        json!({ "items": crate::store::my_account_list(&st).await }),
    ))
}

/// POST /api/admin/my-accounts {name, phone, pass?}：手动设置账号（存在则覆盖更新；手机号选填）。
/// 2026-10-12 审计修订：`pass` 留空时服务端生成 CSPRNG 随机密码，随响应下发一次
/// （{password}，仅此一次，请管理员当场转达）；显式传 pass 的旧调用照旧兼容。
/// 设置成功后吊销该账号全部 my token（改密/开号后旧登录态必须失效）。
pub async fn admin_my_account_set(
    State(st): State<ArcState>,
    Json(body): Json<Value>,
) -> ApiResult {
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let phone = body
        .get("phone")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (pass, generated) = match body
        .get("pass")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(p) => (p.to_string(), false),
        None => (crate::random::random_password(), true),
    };
    crate::store::my_account_set(&st, name, phone, &pass)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    my_tokens_revoke(name);
    tracing::info!(account = %name, generated, "管理端设置展示端账号（旧 token 已吊销）");
    Ok(Json(json!({
        "ok": true,
        // 随机生成的密码仅此一次下发；管理员显式指定的密码不回显
        "password": if generated { json!(pass) } else { Value::Null },
    })))
}

/// GET /api/admin/my-accounts/{name}：单个账号（右键弹窗回填；没有则 found:false）
pub async fn admin_my_account_get(
    State(st): State<ArcState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> ApiResult {
    let mut v = match crate::store::my_account_get(&st, &name).await {
        Some(v) => v,
        None => return Ok(Json(json!({ "found": false }))),
    };
    v["found"] = json!(true);
    Ok(Json(v))
}

/// DELETE /api/admin/my-accounts/{name}：删除账号（幂等，不存在也返回 ok）。
/// 删号同时吊销该账号全部 my token（内存 token 24h TTL，不能等它自然过期）。
pub async fn admin_my_account_delete(
    State(st): State<ArcState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> ApiResult {
    crate::store::my_account_delete(&st, &name).await;
    my_tokens_revoke(&name);
    Ok(Json(json!({ "ok": true })))
}

/// POST /api/admin/my-accounts/{name}/bind {personId}：管理员改绑主播。
/// 用户口径（2026-10-10）：绑定之后用户自己不能解绑，**只有管理员能改**。
/// 账号名会随主播姓名改写（账号名本来就是主播姓名的绑定关系；登录照样支持手机号）。
pub async fn admin_my_account_bind(
    State(st): State<ArcState>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let person_id = body
        .get("personId")
        .and_then(|v| v.as_i64())
        .or_else(|| {
            body.get("personId")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
        })
        .filter(|v| *v > 0)
        .ok_or_else(|| anyhow::anyhow!("请选择要绑定的主播"))?;
    let r = crate::store::my_account_admin_bind(&st, &name, person_id)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    // 改绑后吊销旧登录态：账号名会被改写成主播姓名，新旧两个名字都吊销
    // （拿着旧 token 的人必须重新登录才能拿到新身份，审计 2026-10-12）
    my_tokens_revoke(&name);
    my_tokens_revoke(&r.name);
    tracing::info!(account = %name, anchor = %r.name, team = %r.team, "管理员改绑展示端账号（旧 token 已吊销）");
    Ok(Json(json!({
        "ok": true, "name": r.name, "team": r.team, "personId": r.person_id,
    })))
}

/* --------------------- 绑定申请审核（管理端，017 表，2026-10-12 审计） --------------------- */

/// GET /api/admin/my-bind-requests?status=pending：绑定申请列表
/// （status 空 = 全部，pending 排最前；不含任何密码/凭证字段）
pub async fn admin_my_bind_requests(
    State(st): State<ArcState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let status = q.get("status").cloned().unwrap_or_default();
    Ok(Json(
        json!({ "items": crate::store::my_bind_request_list(&st, &status, 200).await }),
    ))
}

/// POST /api/admin/my-bind-requests/{id}/approve：审核通过。
/// 事务式口径（与 anchor_claim_decide 同款）：先原子翻转 pending→approved 占住裁决权，
/// 再执行绑定（复用 my_account_bind 的核心绑定逻辑）；绑定失败则回滚申请为 pending，
/// 不会卡在「已通过却没绑上」。通过后吊销该账号全部 my token（强制重新登录拿新身份）。
pub async fn admin_my_bind_request_approve(
    State(st): State<ArcState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> ApiResult {
    /* person_name 是申请单上的姓名快照，绑定后以主播台账现名（r.name）为准，
    这里只做解构丢弃 —— 事件与响应都发台账现名，避免快照与现名不一致。 */
    let Some((account, person_id, _person_name)) =
        crate::store::my_bind_request_decide(&st, id, true, "", "admin").await
    else {
        return Err(anyhow::anyhow!("这笔申请不存在或已经处理过了").into());
    };
    match crate::store::my_account_bind(&st, &account, person_id).await {
        Ok(r) => {
            // 绑定把账号名从手机号占位改写成主播姓名：新旧名都吊销
            my_tokens_revoke(&account);
            my_tokens_revoke(&r.name);
            tracing::info!(id, account = %account, anchor = %r.name, team = %r.team, "绑定申请审核通过（旧 token 已吊销）");
            crate::events::emit(
                crate::events::SCOPE_ROSTER,
                format!("绑定审核通过：账号「{account}」→ 主播「{}」", r.name),
            );
            Ok(Json(json!({
                "ok": true, "status": "approved",
                "account": account, "name": r.name, "team": r.team,
            })))
        }
        Err(e) => {
            // 绑定失败（主播已绑别人 / 账号状态变了…）：回滚裁决，让管理员能改判或重试
            crate::store::my_bind_request_revert(&st, id).await;
            Err(anyhow::anyhow!(e).into())
        }
    }
}

/// POST /api/admin/my-bind-requests/{id}/reject {reason?}：驳回（reason 会回显给用户）
pub async fn admin_my_bind_request_reject(
    State(st): State<ArcState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult {
    let reason = body
        .get("reason")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default()
        .chars()
        .take(255)
        .collect::<String>();
    let Some((account, _pid, person_name)) =
        crate::store::my_bind_request_decide(&st, id, false, &reason, "admin").await
    else {
        return Err(anyhow::anyhow!("这笔申请不存在或已经处理过了").into());
    };
    tracing::info!(id, account = %account, anchor = %person_name, reason = %reason, "绑定申请已驳回");
    crate::events::emit(
        crate::events::SCOPE_ROSTER,
        format!("绑定驳回：账号「{account}」申请的主播「{person_name}」（{reason}）"),
    );
    Ok(Json(json!({ "ok": true, "status": "rejected" })))
}

/* ------------------------------- 防遍历限流 ------------------------------- */

/// 验证接口限流：每 IP 每分钟 5 次（姓名/密码撞库防护）
static VERIFY_LIMITER: std::sync::OnceLock<VerifyLimiter> = std::sync::OnceLock::new();
/// 注册类接口限流：每 IP 每分钟 12 次（一次注册本来就要「申请授权 → 重新生成 → 提交」好几下，
/// 跟登录撞库不是一回事，单独一档更宽松；仍然防脚本刷）
static REGISTER_LIMITER: std::sync::OnceLock<VerifyLimiter> = std::sync::OnceLock::new();
/// 授权轮询限流：每 IP 每分钟 120 次（前端 2.5s 一次，正常 24 次/分；
/// 单独一档防止脚本拿轮询接口猛打网关面板 —— 审计 2026-10-11）
static REGISTER_POLL_LIMITER: std::sync::OnceLock<VerifyLimiter> = std::sync::OnceLock::new();

struct VerifyLimiter {
    hits: std::sync::Mutex<HashMap<String, Vec<Instant>>>,
    /// 每分钟允许的次数
    limit: usize,
}

impl VerifyLimiter {
    fn new(limit: usize) -> Self {
        Self {
            hits: std::sync::Mutex::new(HashMap::new()),
            limit,
        }
    }

    fn allow(&self, ip: &str) -> bool {
        let mut hits = self.hits.lock().expect("verify 限流锁 poisoned");
        let now = Instant::now();
        // P2-15：整表 prune（对照 my_tokens 的 retain 写法），
        // key 不再永久驻留 —— 内存不随历史访客 IP 数增长。
        hits.retain(|_, list| {
            list.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
            !list.is_empty()
        });
        let list = hits.entry(ip.to_string()).or_default();
        list.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
        if list.len() >= self.limit {
            return false;
        }
        list.push(now);
        true
    }
}

/// P1-3：限流用的「客户端真实 IP」。
/// 生产经反代（Caddy → 127.0.0.1:8787），直连看到的 ConnectInfo 全是 127.0.0.1，
/// 全站会共用一个限流桶；但 X-Forwarded-For 可以被任何人伪造 —— 所以只信任
/// **来自 127.0.0.1 的连接**带来的 XFF（取第一个 IP，即反代记录的真实客户端），
/// 非本机连接一律用 ConnectInfo（伪造的 XFF 直接忽略）。
fn client_ip_for_limit(addr: std::net::SocketAddr, headers: &HeaderMap) -> String {
    if addr.ip().is_loopback() {
        if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(first) = xff.split(',').next() {
                let ip = first.trim();
                if !ip.is_empty() {
                    return ip.to_string();
                }
            }
        }
    }
    addr.ip().to_string()
}

/// 我的 token → (personId, 团别, 账号姓名)（内存，24h 失效；前端 localStorage 保存）。
/// 管理员观看账号 personId=0（不对应台账主播，姓名直接来自 my_accounts）。
type MyTokenMap = HashMap<String, (u64, String, String, Instant)>;

static MY_TOKENS: std::sync::OnceLock<std::sync::Mutex<MyTokenMap>> = std::sync::OnceLock::new();

fn my_tokens() -> &'static std::sync::Mutex<MyTokenMap> {
    MY_TOKENS.get_or_init(std::sync::Mutex::default)
}

/// 进程内随机数来源：RandomState 每次构造都带 OS 级随机种子，省一个 rand 依赖
fn rand_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

fn my_token_put(person_id: u64, team: String, name: String) -> String {
    // token 不再编入 pid/时间戳（老格式可被相邻值猜中），纯随机串，映射关系只存在内存里
    let token = format!("my-{:016x}-{:016x}", rand_u64(), rand_u64());
    let mut m = my_tokens().lock().expect("my token 锁 poisoned");
    m.retain(|_, (_, _, _, at)| at.elapsed() < Duration::from_secs(24 * 3600));
    m.insert(token.clone(), (person_id, team, name, Instant::now()));
    token
}

fn my_token_take(token: &str) -> Option<(u64, String, String)> {
    let mut m = my_tokens().lock().expect("my token 锁 poisoned");
    m.retain(|_, (_, _, _, at)| at.elapsed() < Duration::from_secs(24 * 3600));
    m.get(token)
        .map(|(pid, team, name, _)| (*pid, team.clone(), name.clone()))
}

/// 吊销某账号的全部 my token（2026-10-12 安全审计：token 是 24h 内存态、原本无撤销手段）。
/// 调用点：改密 / 找回（重置）密码 / 绑定审核通过 / 管理员改绑 / 删除账号——
/// 这些动作之后旧登录态必须立刻失效，不能等 24h TTL 自然过期。
/// （token 挂的是**账号名**：绑定会把 name 从手机号占位改写成主播姓名，所以绑定类操作
/// 新旧两个名字都要吊销。）
pub(crate) fn my_tokens_revoke(name: &str) {
    let mut m = my_tokens().lock().expect("my token 锁 poisoned");
    m.retain(|_, (_, _, n, _)| n != name);
}

/// 只读一眼 token 对应的账号（不消费）：公开 API 中间件用它给访问日志带账号。
/// 未登录的访客返回 None，日志就只记 IP。
pub(crate) fn my_token_peek(token: &str) -> Option<(u64, String, String)> {
    my_token_take(token)
}

/// 从 X-My-Token 头取登录 token（2026-10-12 起**唯一**入口）。
///
/// 不再接受 `?token=` / `?access_token=` URL query（审计安全修复）：
/// URL 会进网关/服务器访问日志、浏览器历史与 Referer 头，token 走 query 等于把
/// 24h 有效的登录态写进各种日志。POST JSON body 里的 `token` 字段保留（不进 URL/日志）。
fn header_my_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-my-token")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// 展示端登录门（2026-10-10）：/st/{team} 的数据接口一律要求「我的」登录态，
/// 未登录只给一张登录卡。token 只走 X-My-Token 头（2026-10-12 起，不再认 URL query）。
/// 校验通过返回 person_id；token 与路径团别不符也拒绝。
///
/// 2026-10-10 加开关：后台关掉「开启登录」（login_required=false）时全站免登录开放，
/// 返回 person_id=0（一个不存在的档案 id，调用方只拿它判「管理员/游客」，不下发个人数据）。
/// 带 token 的照常解析，所以关登录时之前登过的人仍能直接用「我的」。
fn gate_team(login_required: bool, team: &str, headers: &HeaderMap) -> Result<u64, ApiError> {
    let token = header_my_token(headers);
    // 有 token 就先验（开关关着时也认，访客拿旧 token 仍能看「我的」）
    if let Some(t) = token.as_deref().filter(|s| !s.is_empty()) {
        if let Some((pid, my_team, _)) = my_token_take(t) {
            // 团别为空 = 免团别登录口（/api/public/my/login）发的「任意团」token：
            // 管理员观看账号 / 没绑主播的账号，两个团都放行
            if my_team == team || my_team.is_empty() || team == "all" {
                return Ok(pid);
            }
            // token 团别不符：开着登录门时报错；关了就按游客放行（别把路人拦在门外）
            return if login_required {
                Err(ApiError::auth("该账号不属于本团"))
            } else {
                Ok(0)
            };
        }
    }
    // 没带 token / token 失效：关登录 → 游客放行；开登录 → 拦下
    if !login_required {
        return Ok(0);
    }
    Err(ApiError::auth("请先登录"))
}

/// 从 token 里取账号所属团别；游客 / 任意团账号返回空串（统一入口按全量展示）。
fn token_team(headers: &HeaderMap) -> String {
    header_my_token(headers)
        .and_then(|t| my_token_take(&t))
        .map(|(_, team, _)| team)
        .filter(|team| team == "male" || team == "female")
        .unwrap_or_default()
}

/// 统一入口 `/api/public/teams/all` 的数据口径：
/// - 管理员观看 / 未绑定账号 / 开放游客：看全量；
/// - 绑定男团主播：只看男团；
/// - 绑定女团主播：只看女团。
fn data_team<'a>(path_team: &'a str, token_team: &'a str) -> &'a str {
    if path_team == "all" {
        token_team
    } else {
        path_team
    }
}

/* ------------------------------- 站点数据 ------------------------------- */

/// GET /api/public/teams/{team}：站点配置 + 最新日期
/// 团链接被关闭时返回 published=false（前端展示「暂未公布」占位，不是 404）
/// 展示端登录门（2026-10-10）：要「我的」登录态（token 走 X-My-Token 头）
pub async fn site(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    let query_team = data_team(&team, &token_team(&headers)).to_string();
    gate_team(s.login_required, &team, &headers)?;
    let dates = crate::repos::get_available_dates(&st, None, false).await?;
    // 公告弹窗：开启时随站点配置一起下发；关闭时不下发（前端不弹）
    let ann = Announcement::load(&st).await;
    Ok(Json(json!({
        "published": true,
        "team": query_team,
        "latestDate": dates.first(),
        "dates": dates,
        "announcement": if ann.enabled { ann.to_json() } else { Value::Null },
        "settings": {
            "daily": s.daily,
            "monthly": s.monthly,
            "yearly": s.yearly,
            "duration": s.duration,
            "trend": s.trend,
            "wave": s.wave,
            // 页面级显隐（2026-10-10）：每日报告 / 分组 / 族谱 / 分组子页 / 登录开关
            "dataEnabled": s.data_enabled,
            "groupEnabled": s.group_enabled,
            "lineageEnabled": s.lineage_enabled,
            "groupTypes": s.group_types,
            "loginRequired": s.login_required,
        },
    })))
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub date: Option<String>,
    pub period: Option<String>,
}

/// GET /api/public/teams/{team}/daily?date=
/// DTO 白名单：名次 / 名字 / 音浪 / 分钟 / 开播——绝无抖音号
pub async fn daily(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    let query_team = data_team(&team, &token_team(&headers)).to_string();
    gate_team(s.login_required, &team, &headers)?;
    if !s.daily {
        return Ok(Json(json!({ "published": true, "dailyEnabled": false })));
    }
    // 每日报告页整体显隐（2026-10-10）：关掉后榜单数据不下发
    if !s.data_enabled {
        return Ok(Json(json!({ "published": true, "dailyEnabled": false })));
    }
    let dates = crate::repos::get_available_dates(&st, None, false).await?;
    let date = match q.date.as_deref() {
        Some(d) if valid_date_pub(d) && dates.iter().any(|x| x == d) => d.to_string(),
        _ => dates.first().cloned().unwrap_or_default(),
    };
    if date.is_empty() {
        return Err(anyhow::anyhow!("暂未公布数据").into());
    }
    let source = sid(&st);
    // 前一业务日（用于名次变动 ↑↓）
    let idx = dates.iter().position(|d| d == &date);
    let prev_date = idx.and_then(|i| dates.get(i + 1)).cloned();

    /* 全团在册主播都要出现在榜上：当天没有采集记录的人走 LEFT JOIN 得到 0 音浪 /
    未开播，前端标记为「未开播」。只在 daily_metric 里捞人的话，某天只录了 1 个人时
    榜单就真只剩 1 行，跟库里的在册人数对不上。
    hide_in_daily_report = 1 的主播（管理端逐人设置）完全不进榜、不占名次。 */
    let sql = format!(
        "SELECT p.id AS person_id, p.name, \
         CAST(COALESCE(SUM(d.wave), 0) AS SIGNED) AS wave, \
         CAST(COALESCE(SUM(d.minutes), 0) AS SIGNED) AS minutes, \
         CASE WHEN COALESCE(SUM(d.wave), 0) >= 2 THEN 1 ELSE 0 END AS live, \
         COUNT(d.id) AS nrow \
         FROM person p \
         LEFT JOIN daily_metric d ON d.person_id = p.id \
           AND DATE_FORMAT(d.biz_date, '%Y-%m-%d') = '{date}' \
           AND d.wave_reliable = 1 \
         WHERE p.deleted_at IS NULL \
         AND ('{query_team}' = '' OR p.gender = '{query_team}') \
         AND p.hide_in_daily_report = 0 \
         GROUP BY p.id, p.name \
         ORDER BY nrow DESC, wave DESC, p.name ASC"
    );
    let rows = mysql::query(&st.pools, &source, &sql).await?;

    // 当月未播天数 + 历史累计总音浪 + 前一日名次（内存 join）
    // 未播口径（2026-10-09 用户定；2026-10-10 P1-10 统一引用 dialect::NOT_LIVE_WAVE_THRESHOLD）：
    // 当天导入音浪 < 2 = 未开播；
    // totalWave = 历史累计总音浪（2026-10-11 用户口径，展示页列「累计总音浪」）。
    // P1-7：只聚合可信行（wave_reliable=1），与 WAVES_A / 月年榜口径一致。
    let month = &date[..7];
    // 与日报共用 dialect::month_absent_sql（唯一口径：有数据行且音浪 < 2 才算未播）
    let absent_sql = crate::dialect::month_absent_sql(month);
    let total_sql = "SELECT person_id, CAST(SUM(wave) AS SIGNED) AS total \
         FROM daily_metric WHERE wave_reliable = 1 GROUP BY person_id"
        .to_string();
    let mut absent_map: HashMap<u64, i64> = Default::default();
    for r in mysql::query(&st.pools, &source, &absent_sql).await? {
        absent_map.insert(row_pid(&r, "person_id"), row_i64(&r, "n"));
    }
    let mut total_map: HashMap<u64, i64> = Default::default();
    for r in mysql::query(&st.pools, &source, &total_sql).await? {
        total_map.insert(row_pid(&r, "person_id"), row_i64(&r, "total"));
    }
    let mut prev_rank: HashMap<u64, usize> = Default::default();
    if let Some(pd) = &prev_date {
        let psql = format!(
            "SELECT p.id AS person_id \
             FROM daily_metric d JOIN person p ON p.id = d.person_id \
             WHERE p.deleted_at IS NULL \
             AND ('{query_team}' = '' OR p.gender = '{query_team}') \
             AND p.hide_in_daily_report = 0 AND d.wave_reliable = 1 \
             AND DATE_FORMAT(d.biz_date, '%Y-%m-%d') = '{pd}' \
             GROUP BY p.id, p.name ORDER BY SUM(d.wave) DESC"
        );
        for (i, r) in mysql::query(&st.pools, &source, &psql)
            .await?
            .iter()
            .enumerate()
        {
            prev_rank.insert(row_pid(r, "person_id"), i + 1);
        }
    }

    // 名次只在「当天有数据」的人之间排：没采集记录的人不给名次，前端显示「—」，
    // 否则明明没数据的人会因为有 80 个同僚也排在 40 名，读者一眼看不出他没播。
    let mut last_wave: Option<i64> = None;
    let mut last_rank = 0usize;
    let mut ranked = 0usize;
    let mut items: Vec<Value> = Vec::with_capacity(rows.len());
    for r in rows.iter() {
        let pid = row_pid(r, "person_id");
        let has_data = row_i64(r, "nrow") > 0;
        let cur = row_i64(r, "wave");
        let rank = if has_data {
            ranked += 1;
            if Some(cur) == last_wave {
                last_rank
            } else {
                last_rank = ranked;
                last_rank
            }
        } else {
            0
        };
        if has_data {
            last_wave = Some(cur);
        }
        let delta = match prev_rank.get(&pid).copied() {
            // 前一天压根没他（没数据）或今天没数据：都不算升降
            Some(pr) if has_data => Some(if pr > rank {
                json!({ "dir": "up", "n": pr - rank })
            } else if pr < rank {
                json!({ "dir": "down", "n": rank - pr })
            } else {
                json!({ "dir": "flat" })
            }),
            _ => None,
        };
        let mut item = json!({
            // P2-19：公开榜单不再下发 personId（对齐 public.rs 的白名单策略 ——
            // 展示端团数据视图只按 name/isLive/rank 渲染，不依赖这个内部 id；
            // 需要 id 的只有 push 搜索/绑定接口，那些接口单独下发）
            "name": r.try_get::<String, _>("name").unwrap_or_default(),
            "wave": cur,
            "isLive": row_live(r),
            "hasData": has_data,
            "absentDays": absent_map.get(&pid).copied().unwrap_or(0),
            "totalWave": total_map.get(&pid).copied().unwrap_or(0),
            "rank": rank,
        });
        if s.duration {
            item["minutes"] = json!(row_i64(r, "minutes"));
        }
        if s.trend {
            item["delta"] = delta.unwrap_or(Value::Null);
        }
        if !s.wave {
            // 音浪数值显隐（后台开关）：关掉后不下发任何数字
            item["wave"] = Value::Null;
            item["totalWave"] = Value::Null;
        }
        items.push(item);
    }
    Ok(Json(json!({
        "published": true,
        "date": date,
        "teamSize": rows.len(),
        // P2-13：liveCount 语义对齐为「开播人数」（当日音浪 ≥ 未播阈值），
        // 原来统计的是「有数据人数」，字段名与口径不符
        "liveCount": items
            .iter()
            .filter(|i| {
                i["wave"].as_i64().map(is_live_wave).unwrap_or(false)
            })
            .count(),
        "items": items,
    })))
}
/// GET /api/public/teams/{team}/lineage：师门树（公开端画布用）
///
/// DTO 白名单：id / 名字 / 师傅 id+名 / 状态 / 代数 / 直系徒弟数——绝无抖音号。
/// 师傅不在本团（含 master 为空）的当作树根。离团（status='left'）的人也在树里
/// （家族成员不因离团消失），前端负责淡显。
///
/// 访问口径（2026-10-12 安全审计修复。产品口径：族谱页给**有师门归属**的用户看，
/// 管理员看全部）：
/// - **永远要求有效 my token**——即使 login_required=false 也不放行游客：族谱是全团
///   师承关系数据，游客可见等于把内部师门结构对公网开放。token 缺失/失效 → 401；
/// - 管理员观看账号（my_accounts.is_admin=1）→ 行为不变：可看任意团，
///   team=all 时返回两团全量；
/// - 普通绑定用户：本人**有师傅**（master_id 非空/非 0）或**有徒弟**（直系徒弟数 > 0）
///   才放行，数据范围维持全团（与现状一致）；否则 403 + code=lineage_forbidden
///   （前端据此显示「族谱仅对师门成员开放」，区别于地区门禁的 403）；
/// - 未绑定账号（person_id=0 且非管理员）→ 403 同上；team=all 仅管理员可用。
///
/// 返回类型用 Response 而不是 ApiResult：403 需要带自定义 code 字段，ApiError 的
/// 错误体只有 {error}。
pub async fn lineage(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<MyQuery>,
    headers: HeaderMap,
) -> Response {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Json(json!({ "published": false })).into_response();
    }
    if !s.lineage_enabled {
        return Json(json!({ "published": true, "lineageEnabled": false })).into_response();
    }
    /* 音浪口径（2026-10-12 用户口径「支持日期查看音浪」）：
    daily=最新业务日当天；monthly/yearly=最新业务日所在月/年。
    date 参数可指定基准日（非法直接回落，不报错——族谱页不因脏参数打不开）。 */
    let dates: Vec<String> = crate::repos::get_available_dates(&st, None, false)
        .await
        .unwrap_or_default();
    let (period, base_date) = lineage_window(
        q.period.as_deref(),
        q.date.as_deref(),
        dates.first().map(String::as_str),
    );
    // 族谱门禁：永远要有效 token（不放行游客），身份判定见上方口径注释
    let Some(token) = header_my_token(&headers) else {
        return ApiError::auth("请先登录").into_response();
    };
    let Some((pid, my_team, account_name)) = my_token_take(&token) else {
        return ApiError::auth("登录已过期，请重新登录").into_response();
    };
    let is_admin = crate::store::my_account_is_admin(&st, &account_name).await;
    let query_team = if is_admin {
        // 管理员：行为不变（任意团；all = 两团全量，即 SQL 里团别条件恒真）
        if team == "all" {
            String::new()
        } else {
            team.clone()
        }
    } else {
        // 普通账号：token 团别与路径团别不符照旧 401（与登录门同口径）
        if !my_team.is_empty() && my_team != team && team != "all" {
            return ApiError::auth("该账号不属于本团").into_response();
        }
        let (has_master, disciples) = match person_lineage_membership(&st, pid).await {
            Ok(v) => v,
            Err(e) => return ApiError::from(e).into_response(),
        };
        if !lineage_view_allowed(false, pid, has_master, disciples, &team) {
            return lineage_forbidden();
        }
        team.clone()
    };
    /* 族谱只显示有师门归属的人（2026-10-09 规则）：没有师傅且没有徒弟的孤立节点
    不下发（画布和导出图的「没有师傅」名单自动联动）。
    与榜单显隐（hide_in_daily_report）是两个独立配置：榜单隐藏的人只要挂在
    师门下，族谱照常显示。 */
    let sql = format!(
        "SELECT p.id, p.name, p.master_id, m.name AS master_name, p.status, p.generation, \
         (SELECT COUNT(*) FROM person k WHERE k.master_id = p.id AND k.deleted_at IS NULL) AS disciples \
         FROM person p LEFT JOIN person m ON m.id = p.master_id \
         WHERE p.deleted_at IS NULL \
         AND ('{query_team}' = '' OR p.gender = '{query_team}') \
         AND (p.master_id IS NOT NULL \
              OR EXISTS(SELECT 1 FROM person k WHERE k.master_id = p.id AND k.deleted_at IS NULL))"
    );
    let rows = match mysql::query(&st.pools, &sid(&st), &sql).await {
        Ok(r) => r,
        Err(e) => return ApiError::from(e).into_response(),
    };

    // 个人音浪（2026-10-12 改按 period/date 窗口过滤；与榜单口径一致 wave_reliable=1）
    let wave_sql = lineage_wave_window_sql(&period, &base_date);
    let mut wave_map: HashMap<u64, i64> = Default::default();
    match mysql::query(&st.pools, &sid(&st), &wave_sql).await {
        Ok(rs) => {
            for r in &rs {
                wave_map.insert(row_pid(r, "person_id"), row_i64(r, "total"));
            }
        }
        Err(e) => return ApiError::from(e).into_response(),
    }

    let mut nodes: Vec<Value> = Vec::with_capacity(rows.len());
    let mut ids: std::collections::HashSet<u64> = Default::default();
    let mut children: HashMap<u64, Vec<u64>> = Default::default();
    for r in &rows {
        let id = row_pid(r, "id");
        ids.insert(id);
        if let Some(master) = read_opt_u64_field(r, "master_id").filter(|m| ids.contains(m)) {
            children.entry(master).or_default().push(id);
        }
    }
    // 门下累计总音浪（自己 + 所有子孙），记忆化递归（人数几百，不会爆栈）。
    fn subtree_wave(
        id: u64,
        children: &HashMap<u64, Vec<u64>>,
        wave: &HashMap<u64, i64>,
        memo: &mut HashMap<u64, i64>,
    ) -> i64 {
        if let Some(v) = memo.get(&id) {
            return *v;
        }
        let mut total = wave.get(&id).copied().unwrap_or(0);
        if let Some(kids) = children.get(&id) {
            for k in kids {
                total += subtree_wave(*k, children, wave, memo);
            }
        }
        memo.insert(id, total);
        total
    }
    let mut subtree_memo: HashMap<u64, i64> = Default::default();
    // 真实 id/masterId 先单独收集，最后统一重映射为响应内序号（见 lineage_remap_ids）
    let mut raw_ids: Vec<(u64, Option<u64>)> = Vec::with_capacity(rows.len());
    for r in &rows {
        let id = row_pid(r, "id");
        let master_id = read_opt_u64_field(r, "master_id").filter(|m| ids.contains(m));
        let wave = wave_map.get(&id).copied().unwrap_or(0);
        let total_wave = subtree_wave(id, &children, &wave_map, &mut subtree_memo);
        raw_ids.push((id, master_id));
        nodes.push(json!({
            "name": r.try_get::<String, _>("name").unwrap_or_default(),
            "masterName": r.try_get::<Option<String>, _>("master_name").ok().flatten(),
            "status": r.try_get::<String, _>("status").unwrap_or_default(),
            "generation": r.try_get::<Option<i64>, _>("generation").ok().flatten(),
            "disciples": row_i64(r, "disciples"),
            // 音浪数值显隐（后台开关）：关掉后不下发任何数字
            "wave": if s.wave { json!(wave) } else { Value::Null },
            "totalWave": if s.wave { json!(total_wave) } else { Value::Null },
        }));
    }
    for (node, (id, master)) in nodes.iter_mut().zip(lineage_remap_ids(&raw_ids)) {
        node["id"] = json!(id);
        node["masterId"] = match master {
            Some(m) => json!(m),
            None => Value::Null,
        };
    }
    Json(json!({
        "published": true,
        "team": team,
        "nodes": nodes,
        // 2026-10-12：音浪口径与基准日（前端切日/月/年时回显 + 请求下一次窗口用）
        "period": period,
        "date": base_date,
        // 前端据此区分管理员（全组树）与普通主播（只看自己徒弟）
        "viewerIsAdmin": is_admin,
    }))
    .into_response()
}

/// 族谱音浪窗口（纯函数）：period 非法回落 daily；date 非法回落最新业务日。
/// 返回 (生效口径, 基准日)。
fn lineage_window(
    period: Option<&str>,
    date: Option<&str>,
    latest: Option<&str>,
) -> (String, String) {
    let period = match period.map(str::trim).filter(|s| !s.is_empty()) {
        Some("monthly") => "monthly",
        Some("yearly") => "yearly",
        _ => "daily",
    };
    // 日期白名单：YYYY-MM-DD 且真实存在（防注入 + 防脏数据，照 push::is_biz_date）
    let base = date
        .map(str::trim)
        .filter(|d| d.len() == 10 && chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok())
        .or(latest)
        .unwrap_or_default()
        .to_string();
    (period.to_string(), base)
}

/// 族谱音浪 SQL（纯函数，2026-10-12）：按 period 窗口条件聚合。
/// monthly/yearly 由基准日推出窗口（DATE_FORMAT 前缀匹配），daily 精确到日。
/// base_date 已由 lineage_window 白名单校验后才拼进 SQL。
fn lineage_wave_window_sql(period: &str, base_date: &str) -> String {
    /* 双保险：调用方 lineage_window 已校验，但纯函数直接被测试/复用时也挡一道。
    日期形状非法 → 退回无窗口条件（空库口径）；绝不把外部字符串带进 SQL。 */
    let base_date = if base_date.len() == 10
        && chrono::NaiveDate::parse_from_str(base_date, "%Y-%m-%d").is_ok()
    {
        base_date
    } else {
        // 空串（库没数据）或形状非法 → 都按空库口径，绝不把外部字符串带进 SQL
        ""
    };
    let where_clause = if base_date.is_empty() {
        "WHERE wave_reliable = 1".to_string() // 没有任何业务日：不给窗口条件（全空库）
    } else {
        match period {
            "monthly" => format!(
                "WHERE wave_reliable = 1 AND DATE_FORMAT(biz_date, '%Y-%m') = '{}'",
                base_date.get(..7).unwrap_or(base_date)
            ),
            "yearly" => format!(
                "WHERE wave_reliable = 1 AND DATE_FORMAT(biz_date, '%Y') = '{}'",
                base_date.get(..4).unwrap_or(base_date)
            ),
            _ => format!(
                "WHERE wave_reliable = 1 AND DATE_FORMAT(biz_date, '%Y-%m-%d') = '{base_date}'"
            ),
        }
    };
    format!(
        "SELECT person_id, CAST(SUM(wave) AS SIGNED) AS total \
         FROM daily_metric {where_clause} GROUP BY person_id"
    )
}

/// 族谱访问判定（纯函数，口径与原因见 lineage 注释）：
/// 管理员恒放行；普通用户 team=all 拒绝（all 仅管理员）、未绑定（person_id=0）拒绝、
/// 无师门归属（没师傅也没徒弟）拒绝。
pub fn lineage_view_allowed(
    is_admin: bool,
    person_id: u64,
    has_master: bool,
    disciples: i64,
    team: &str,
) -> bool {
    if is_admin {
        return true;
    }
    if team == "all" || person_id == 0 {
        return false;
    }
    has_master || disciples > 0
}

/// 403 + code=lineage_forbidden：前端据此显示「族谱仅对师门成员开放」
/// （必须和地区门禁的 403 区分开，后者是 IP 白名单拦截、文案不同）。
fn lineage_forbidden() -> Response {
    (
        axum::http::StatusCode::FORBIDDEN,
        Json(json!({
            "error": "族谱仅对师门成员开放（有师傅或徒弟的账号可见）",
            "code": "lineage_forbidden",
        })),
    )
        .into_response()
}

/// 本人师门归属：(有师傅, 直系徒弟数)。person_id=0（未绑定）或档案不存在 → (false, 0)。
async fn person_lineage_membership(st: &AppState, person_id: u64) -> anyhow::Result<(bool, i64)> {
    if person_id == 0 {
        return Ok((false, 0));
    }
    let sql = "SELECT p.master_id, \
         (SELECT COUNT(*) FROM person k WHERE k.master_id = p.id AND k.deleted_at IS NULL) AS disciples \
         FROM person p WHERE p.id = ? AND p.deleted_at IS NULL LIMIT 1";
    let rows =
        mysql::query_with_binds(&st.pools, &sid(st), sql, |q| q.bind(person_id as i64)).await?;
    Ok(match rows.first() {
        Some(r) => (
            read_opt_u64_field(r, "master_id")
                .map(|m| m > 0)
                .unwrap_or(false),
            row_i64(r, "disciples"),
        ),
        None => (false, 0),
    })
}

/// 族谱节点 id 重映射（2026-10-12 安全审计，对齐 daily 接口 P2-19：公开端点不下发内部主键）：
/// 真实 person.id → 本次响应内的序号 1..N，masterId 同步映射；
/// 未出现在节点集里的 masterId → None（前端把这种人当树根，与原口径一致）。
/// 前端只在一棵响应树内用这些 id 建图，序号映射够用。
pub fn lineage_remap_ids(nodes: &[(u64, Option<u64>)]) -> Vec<(u64, Option<u64>)> {
    let mut seq: HashMap<u64, u64> = HashMap::with_capacity(nodes.len());
    for (i, (id, _)) in nodes.iter().enumerate() {
        seq.insert(*id, (i + 1) as u64);
    }
    nodes
        .iter()
        .map(|(id, master)| (seq[id], master.and_then(|m| seq.get(&m).copied())))
        .collect()
}

/// 「徒弟 + 师傅」门下累计音浪统计（2026-10-11）：
/// 师傅自己 + 所有子孙的可信音浪（wave_reliable=1）总和。
/// 返回 (直系徒弟数, 徒弟音浪总和[不含自己], 门下累计音浪[含自己])。
pub async fn lineage_wave_summary(
    st: &AppState,
    team: &str,
    person_id: u64,
) -> anyhow::Result<(i64, i64, i64)> {
    if person_id == 0 {
        return Ok((0, 0, 0));
    }
    let source = sid(st);
    // 本团 person 树（只认有师门归属关系的那部分，与族谱同口径）
    let tree_sql = format!(
        "SELECT id, master_id FROM person \
         WHERE deleted_at IS NULL AND ('{team}' = '' OR gender = '{team}')"
    );
    let rows = mysql::query(&st.pools, &source, &tree_sql).await?;
    let mut ids: std::collections::HashSet<u64> = Default::default();
    let mut children: HashMap<u64, Vec<u64>> = Default::default();
    for r in &rows {
        let id = row_pid(r, "id");
        ids.insert(id);
    }
    for r in &rows {
        let id = row_pid(r, "id");
        if let Some(master) = read_opt_u64_field(r, "master_id").filter(|m| ids.contains(m)) {
            children.entry(master).or_default().push(id);
        }
    }
    // 每人累计总音浪（与榜单 totalWave 同口径）
    let wave_sql = "SELECT person_id, CAST(SUM(wave) AS SIGNED) AS total \
         FROM daily_metric WHERE wave_reliable = 1 GROUP BY person_id"
        .to_string();
    let mut wave_map: HashMap<u64, i64> = Default::default();
    for r in mysql::query(&st.pools, &source, &wave_sql).await? {
        wave_map.insert(row_pid(&r, "person_id"), row_i64(&r, "total"));
    }
    // 递归门下累计（记忆化）
    fn subtree(
        id: u64,
        children: &HashMap<u64, Vec<u64>>,
        wave: &HashMap<u64, i64>,
        memo: &mut HashMap<u64, i64>,
    ) -> i64 {
        if let Some(v) = memo.get(&id) {
            return *v;
        }
        let mut total = wave.get(&id).copied().unwrap_or(0);
        if let Some(kids) = children.get(&id) {
            for k in kids {
                total += subtree(*k, children, wave, memo);
            }
        }
        memo.insert(id, total);
        total
    }
    let mut memo: HashMap<u64, i64> = Default::default();
    let total_wave = subtree(person_id, &children, &wave_map, &mut memo);
    let self_wave = wave_map.get(&person_id).copied().unwrap_or(0);
    let disciple_count = children
        .get(&person_id)
        .map(|v| v.len() as i64)
        .unwrap_or(0);
    Ok((disciple_count, total_wave - self_wave, total_wave))
}

fn read_opt_u64_field(r: &sqlx::mysql::MySqlRow, col: &str) -> Option<u64> {
    use sqlx::Row;
    if let Ok(v) = r.try_get::<u64, _>(col) {
        return Some(v);
    }
    if let Ok(v) = r.try_get::<Option<i64>, _>(col) {
        return v.map(|x| x.max(0) as u64);
    }
    if let Ok(v) = r.try_get::<i64, _>(col) {
        return Some(v.max(0) as u64);
    }
    None
}

/// GET /api/public/teams/{team}/monthly?period=YYYY-MM（或 /yearly?period=YYYY）
pub async fn monthly(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    let query_team = data_team(&team, &token_team(&headers)).to_string();
    gate_team(s.login_required, &team, &headers)?;
    // 显隐判定用**路径上的团**（all 恒为开放），数据过滤才用 query_team。
    // 以前把 query_team 传进去判 team_enabled，匿名访客 token 为空 → data_team 返回 ""
    // → team_enabled("") == false，月/年榜对所有人 published:false（前端 .slice 崩）。
    period_public(&st, &team, &query_team, &q, true).await
}

pub async fn yearly(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    let query_team = data_team(&team, &token_team(&headers)).to_string();
    gate_team(s.login_required, &team, &headers)?;
    period_public(&st, &team, &query_team, &q, false).await
}

/// `gate` = URL 路径上的团（只用来判显隐：all 恒开放）；`team` = 实际数据过滤用的团
/// （匿名 all → 空串 = 不过滤，绑了男/女团的主播 → male/female）。
async fn period_public(
    st: &Arc<AppState>,
    gate: &str,
    team: &str,
    q: &ListQuery,
    is_month: bool,
) -> ApiResult {
    let s = PublicSettings::load(st).await;
    if !s.team_enabled(gate) {
        return Ok(Json(json!({ "published": false })));
    }
    let enabled = if is_month { s.monthly } else { s.yearly };
    if !enabled {
        return Ok(Json(json!({ "published": true, "periodEnabled": false })));
    }
    let period = match q.period.as_deref() {
        Some(p)
            if !p.trim().is_empty()
                && p.chars().all(|c| c.is_ascii_digit() || c == '-')
                && p.len() == if is_month { 7 } else { 4 } =>
        {
            p.trim().to_string()
        }
        _ if is_month => chrono::Local::now().format("%Y-%m").to_string(),
        _ => chrono::Local::now().format("%Y").to_string(),
    };
    // 与 daily 口径对齐：离队的人不进榜（P2-19：也不再下发 personId，
    // 前端月/年榜只按 name/rank/wave 渲染）。
    // P1-7：period_rollup_sql 内层已统一按 wave_reliable=1 过滤。
    // 时长口径 2026-10-10：月 / 年里的「时长」优先用按月导入的整月值（monthly_duration，
    // 平台月报），没有整月记录的月份才用日明细求和 —— 见 dialect::period_rollup_sql。
    let rows = dialect::query_period(
        &st.pools,
        &sid(st),
        "p.id AS person_id, p.name, \
         CAST(SUM(t.w) AS SIGNED) AS wave, \
         CAST(SUM({minutes}) AS SIGNED) AS minutes, \
         CAST(SUM(t.live) AS SIGNED) AS live_days, \
         CAST(SUM(t.off) AS SIGNED) AS off_days",
        &format!("{period}%"),
        team,
        " AND p.status <> 'left' AND p.hide_in_daily_report = 0",
        "wave DESC, p.name ASC",
    )
    .await?;
    // 名次：同音浪并列，下一档跳号（与 daily / 分组页的名次算法一致）
    let mut last_wave: Option<i64> = None;
    let mut last_rank = 0usize;
    let mut items: Vec<Value> = Vec::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        let wave = row_i64(r, "wave");
        let rank = if Some(wave) == last_wave {
            last_rank
        } else {
            last_rank = i + 1;
            last_rank
        };
        last_wave = Some(wave);
        let mut item = json!({
            // P2-19：公开月/年榜不再下发 personId（对齐 public.rs 白名单策略）
            "name": r.try_get::<String, _>("name").unwrap_or_default(),
            "wave": wave,
            "rank": rank,
        });
        if s.duration {
            item["minutes"] = json!(row_i64(r, "minutes"));
            item["liveDays"] = json!(row_i64(r, "live_days"));
        }
        item["absentDays"] = json!(row_i64(r, "off_days"));
        if !s.wave {
            item["wave"] = Value::Null;
        }
        items.push(item);
    }
    Ok(Json(
        json!({ "published": true, "period": period, "items": items }),
    ))
}

fn row_pid(r: &sqlx::mysql::MySqlRow, col: &str) -> u64 {
    if let Ok(v) = r.try_get::<u64, _>(col) {
        return v;
    }
    if let Ok(v) = r.try_get::<i64, _>(col) {
        return v as u64;
    }
    0
}

fn valid_date_pub(s: &str) -> bool {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

fn row_live(r: &sqlx::mysql::MySqlRow) -> bool {
    if let Ok(v) = r.try_get::<i8, _>("live") {
        return v == 1;
    }
    if let Ok(v) = r.try_get::<i64, _>("live") {
        return v == 1;
    }
    r.try_get::<String, _>("live")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .map(|v| v == 1)
        .unwrap_or(false)
}

fn row_i64(r: &sqlx::mysql::MySqlRow, col: &str) -> i64 {
    if let Ok(v) = r.try_get::<i64, _>(col) {
        return v;
    }
    if let Ok(v) = r.try_get::<u64, _>(col) {
        return v as i64;
    }
    if let Ok(v) = r.try_get::<String, _>(col) {
        return v.parse().unwrap_or(0);
    }
    0
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
    let limiter_ok = public_write_allow(&client_ip_for_limit(addr, &headers));
    if !limiter_ok {
        return Err(anyhow::anyhow!("尝试太频繁，请稍后再试").into());
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
        return Err(anyhow::anyhow!("请输入账号和密码").into());
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
    let rows =
        mysql::query_with_binds(&st.pools, &sid(&st), &sql, |q| q.bind(account_name.clone()))
            .await?;
    match rows.into_iter().next() {
        Some(row) => {
            let pid = row
                .try_get::<i64, _>("id")
                .map(|v| v as u64)
                .or_else(|_| row.try_get::<u64, _>("id"))
                .unwrap_or(0);
            let token_team = row.try_get::<String, _>("gender").unwrap_or_default();
            let token = my_token_put(pid, token_team.clone(), account_name.clone());
            Ok(Json(json!({
                "ok": true, "personId": pid, "name": account_name, "token": token, "bound": true
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
        return Err(anyhow::anyhow!("尝试太频繁，请稍后再试").into());
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
        return Err(anyhow::anyhow!("请输入账号和密码").into());
    }

    /* 1. 账号校验（与 verify 同一套 my_accounts） */
    let Some(account_name) = crate::store::my_account_check_login(&st, &acct, &pass).await else {
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
            mysql::query_with_binds(&st.pools, &sid(&st), &sql, |q| q.bind(account_name.clone()))
                .await?;
        if let Some(row) = rows.into_iter().next() {
            let pid = row
                .try_get::<i64, _>("id")
                .map(|v| v as u64)
                .or_else(|_| row.try_get::<u64, _>("id"))
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
fn public_write_allow(ip: &str) -> bool {
    VERIFY_LIMITER
        .get_or_init(|| VerifyLimiter::new(5))
        .allow(ip)
}

/// 注册类写接口限流：每 IP 每分钟 12 次（一次注册本来就要接好几下，单独一档）
fn public_register_allow(ip: &str) -> bool {
    REGISTER_LIMITER
        .get_or_init(|| VerifyLimiter::new(12))
        .allow(ip)
}

/// 授权轮询限流：每 IP 每分钟 120 次（前端 2.5s 轮一次 ≈ 24 次/分，余量给重试）
fn public_poll_allow(ip: &str) -> bool {
    REGISTER_POLL_LIMITER
        .get_or_init(|| VerifyLimiter::new(120))
        .allow(ip)
}

/// 注册开关（用户口径 2026-10-10「注册先不开」）：默认关，后台「公开站点设置 → 开放手机号注册」开。
/// 关着时注册相关接口一律 400；登录、绑定、我的页完全不受影响。
async fn ensure_register_open(st: &AppState) -> Result<(), ApiError> {
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
        return Err(anyhow::anyhow!("尝试太频繁，请稍后再试").into());
    }
    let Some(wb) = st.wb.get() else {
        return Err(anyhow::anyhow!(
            "服务端还没有配置 WorkBuddy 网关（WORKBUDDY_PANEL_PASSWORD），暂时不能自动添加账号"
        )
        .into());
    };
    let realm = body
        .get("realm")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (state, auth_url, realm) = wb
        .login_start(realm)
        .await
        .map_err(|e| anyhow::anyhow!("申请授权链接失败：{e}"))?;
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
        return Err(anyhow::anyhow!("尝试太频繁，请稍后再试").into());
    }
    let state = q.state.unwrap_or_default();
    let state = state.trim();
    if state.is_empty() || state.len() > 128 {
        return Err(anyhow::anyhow!("缺少授权流水号 state").into());
    }
    ensure_register_open(&st).await?;
    let Some(wb) = st.wb.get() else {
        return Err(anyhow::anyhow!("服务端还没有配置 WorkBuddy 网关，不能查询授权状态").into());
    };
    let v = wb.login_poll(state).await.map_err(|e| anyhow::anyhow!(e))?;
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
        return Err(anyhow::anyhow!("尝试太频繁，请稍后再试").into());
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
        return Err(anyhow::anyhow!("尝试太频繁，请稍后再试").into());
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
/// 忘记密码：把自助注册账号密码重置为 **CSPRNG 随机密码**（2026-10-12 审计修复，
/// 废弃「手机号后四位」——手机号半公开，后四位等于公开口令）。
/// 新密码只在本次响应里下发一次，请当场转达；重置成功后该账号全部 my token 吊销。
pub async fn my_password_reset(
    State(st): State<ArcState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    if !public_register_allow(&client_ip_for_limit(addr, &headers)) {
        return Err(anyhow::anyhow!("尝试太频繁，请稍后再试").into());
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
                ApiError::from(anyhow::anyhow!(e))
            } else {
                ApiError::bad_request(e)
            }
        })?;
    // 改密后旧登录态必须立刻失效（审计 2026-10-12：内存 token 原本无撤销手段）
    for n in &names {
        my_tokens_revoke(n);
    }
    tracing::info!(phone = %mask_phone(phone), accounts = ?names, "展示端账号密码已重置为随机密码（旧 token 已吊销）");
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
fn normalize_team(v: &str) -> String {
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
fn my_token_required(headers: &HeaderMap) -> Result<(u64, String, String), ApiError> {
    let Some(token) = header_my_token(headers) else {
        return Err(ApiError::auth("缺少验证 token，请先验证姓名和密码"));
    };
    my_token_take(&token).ok_or_else(|| ApiError::auth("验证已过期，请重新验证"))
}

/* --------------------- 个人页时长口径（2026-10-10 月度时长导入） --------------------- */

/// 同期对照的三个「上期」窗口（1.0.1）：昨日 / 上月 / 去年同期（去年 YTD 至同日）。
/// 集中成结构便于单测校验 SQL 形状（wave_reliable / 窗口口径）。
pub struct PrevPeriods {
    /// 上一业务日（来自 get_available_dates，非自然日）
    pub day: Option<String>,
    /// 上一个自然月（YYYY-MM）
    pub month: String,
    /// 去年（YYYY），同期 = 去年年初至去年今天（闰日安全：2/29 → None 则取整年）
    pub year: String,
    /// 去年今天（YYYY-MM-DD）；闰日无对应时为 None（按整年算）
    pub year_until: Option<String>,
}

/// 由最新业务日推出三个上期窗口。today 为空（库里没数据）→ 全 None/空，前端显示 —。
pub fn prev_periods_of(today: &str) -> PrevPeriods {
    use chrono::Datelike as _;
    let parsed = chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d").ok();
    let month = parsed
        .map(|d| d.format("%Y-%m").to_string())
        .or_else(|| today.get(..7).map(str::to_string))
        .unwrap_or_default();
    let prev_month = parsed.and_then(|d| d.checked_sub_months(chrono::Months::new(1)));
    let today_last_year = parsed.and_then(|d| d.checked_sub_months(chrono::Months::new(12)));
    PrevPeriods {
        day: None, // 由调用方用 dates[1] 填
        month: prev_month
            .map(|d| d.format("%Y-%m").to_string())
            .unwrap_or(month),
        year: (parsed.map(|d| d.year() - 1).unwrap_or_default()).to_string(),
        year_until: today_last_year.map(|d| d.format("%Y-%m-%d").to_string()),
    }
}

/// 本人三个上期音浪（一条 SQL 条件聚合）。零行 / 无数据 → 对应项 NULL（显示 —，区别于真 0）。
pub fn self_prev_wave_sql(p: &PrevPeriods) -> String {
    let day_clause = match &p.day {
        Some(d) => format!(
            "CAST(SUM(CASE WHEN DATE_FORMAT(biz_date, '%Y-%m-%d') = '{d}' THEN wave END) AS SIGNED) AS pd"
        ),
        None => "CAST(NULL AS SIGNED) AS pd".to_string(),
    };
    let until = p
        .year_until
        .as_ref()
        .map(|u| format!(" AND biz_date <= '{u}'"))
        .unwrap_or_default();
    format!(
        "SELECT {day_clause}, \
         CAST(SUM(CASE WHEN DATE_FORMAT(biz_date, '%Y-%m') = '{m}' THEN wave END) AS SIGNED) AS pm, \
         CAST(SUM(CASE WHEN DATE_FORMAT(biz_date, '%Y') = '{y}'{until} THEN wave END) AS SIGNED) AS py \
         FROM daily_metric WHERE person_id = {{pid}} AND wave_reliable = 1",
        m = p.month,
        y = p.year,
    )
}

/// 个人页时长统计的三种窗口
pub enum MinutesScope<'a> {
    Month(&'a str),
    Year(&'a str),
    Total,
}

impl MinutesScope<'_> {
    /// 日明细的过滤条件（daily_metric 别名 d）
    fn daily_where(&self) -> String {
        match self {
            MinutesScope::Month(m) => format!(" AND DATE_FORMAT(d.biz_date, '%Y-%m') = '{m}'"),
            MinutesScope::Year(y) => format!(" AND DATE_FORMAT(d.biz_date, '%Y') = '{y}'"),
            MinutesScope::Total => String::new(),
        }
    }

    /// monthly_duration 的过滤条件
    fn period_where(&self) -> String {
        match self {
            MinutesScope::Month(m) => format!(" AND period = '{m}'"),
            MinutesScope::Year(y) => format!(" AND period LIKE '{y}-%'"),
            MinutesScope::Total => String::new(),
        }
    }
}

/// 个人「时长」SQL：有整月导入值的月份用月报值，其余月份用日明细求和
/// （同一段时长不会既算月报又算日明细）。with_monthly = false 时退回纯日明细口径。
fn my_minutes_sql(person_id: u64, scope: &MinutesScope<'_>, with_monthly: bool) -> String {
    let daily = scope.daily_where();
    if !with_monthly {
        return format!(
            "SELECT CAST(COALESCE((SELECT SUM(d.minutes) FROM daily_metric d \
             WHERE d.person_id = {person_id}{daily}), 0) AS SIGNED) AS minutes"
        );
    }
    let period = scope.period_where();
    format!(
        "SELECT CAST(COALESCE((SELECT SUM(minutes) FROM monthly_duration \
          WHERE person_id = {person_id}{period}), 0) \
         + COALESCE((SELECT SUM(d.minutes) FROM daily_metric d \
          WHERE d.person_id = {person_id}{daily} \
            AND DATE_FORMAT(d.biz_date, '%Y-%m') NOT IN \
              (SELECT period FROM monthly_duration WHERE person_id = {person_id}{period})), 0) \
         AS SIGNED) AS minutes"
    )
}

/// 个人「时长」（月 / 年 / 累计）：库还没跑 007 迁移（monthly_duration 不存在）时退回日明细口径，
/// 个人页不因为少一张表打不开。
pub async fn my_minutes(
    st: &AppState,
    person_id: u64,
    scope: &MinutesScope<'_>,
) -> anyhow::Result<i64> {
    let source = sid(st);
    let sql = my_minutes_sql(person_id, scope, true);
    let rows = match mysql::query(&st.pools, &source, &sql).await {
        Ok(rows) => rows,
        Err(e) if dialect::is_missing_month_table(&e) => {
            let sql = my_minutes_sql(person_id, scope, false);
            mysql::query(&st.pools, &source, &sql).await?
        }
        Err(e) => return Err(e),
    };
    Ok(rows.first().map(|r| row_i64(r, "minutes")).unwrap_or(0))
}

/// GET /api/public/teams/{team}/my（token 走 X-My-Token 头）
/// 个人数据：日/月/年 + 累计 + 团内名次（无密码、无抖音号字段）
pub async fn my(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    // 2026-10-12 起 MyQuery 新增族谱用的 period/date；/my 只用 days 语义，
    // 其余字段忽略（不参与个人页口径）。
    Query(_q): Query<MyQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    let (person_id, my_team, _) = my_token_required(&headers)?;
    if my_team != team && team != "all" {
        return Err(anyhow::anyhow!("该账号不属于本团").into());
    }

    // 个人各口径
    async fn one(st: &AppState, sql: String) -> anyhow::Result<Value> {
        let source = sid(st);
        let rows = mysql::query(&st.pools, &source, &sql).await?;
        Ok(rows
            .into_iter()
            .next()
            .map(|r| {
                json!({
                    "wave": row_i64(&r, "wave"),
                    "minutes": row_i64(&r, "minutes"),
                })
            })
            .unwrap_or(json!({ "wave": 0, "minutes": 0 })))
    }
    /* 「今日」必须是库里最新的业务日，不能用自然日今天。
    数据是每晚 24 点后补前一天的，凌晨打开页面时今天的记录还不存在，
    用自然日会让个人页显示全 0，跟榜单对不上。 */
    let dates = crate::repos::get_available_dates(&st, None, false).await?;
    let today = dates.first().cloned().unwrap_or_default();
    let month = today
        .get(..7)
        .map(str::to_string)
        .unwrap_or_else(|| chrono::Local::now().format("%Y-%m").to_string());
    let year = today
        .get(..4)
        .map(str::to_string)
        .unwrap_or_else(|| chrono::Local::now().format("%Y").to_string());

    /* 同期对照（1.0.1）：昨日 = 上一业务日（与日榜名次变动同口径）、上月、去年同期。 */
    let prev = prev_periods_of(&today);
    let mut prev = prev;
    prev.day = dates.get(1).cloned();

    /* 个人各口径音浪（P1-7：与榜单口径一致，只聚合可信行 wave_reliable=1；
    total 的口径选择见 repos::get_waves 注释 —— 含基线日，但基线日行本身
    wave_reliable=0 不可信，被行级过滤后不影响数值，只保留在日期轴上）。 */
    let zero = || json!({ "wave": 0, "minutes": 0 });
    let mut daily = if s.daily && !today.is_empty() {
        Some(
            one(
                &st,
                format!(
            "SELECT CAST(SUM(wave) AS SIGNED) AS wave, CAST(SUM(minutes) AS SIGNED) AS minutes \
             FROM daily_metric WHERE person_id = {person_id} AND wave_reliable = 1 \
             AND DATE_FORMAT(biz_date, '%Y-%m-%d') = '{today}'"
        ),
            )
            .await?,
        )
    } else if s.daily {
        Some(zero())
    } else {
        None
    };
    let mut monthly = if s.monthly {
        Some(
            one(
                &st,
                format!(
            "SELECT CAST(SUM(wave) AS SIGNED) AS wave, CAST(SUM(minutes) AS SIGNED) AS minutes \
             FROM daily_metric WHERE person_id = {person_id} AND wave_reliable = 1 \
             AND DATE_FORMAT(biz_date, '%Y-%m') = '{month}'"
        ),
            )
            .await?,
        )
    } else {
        None
    };
    let mut yearly = if s.yearly {
        Some(
            one(
                &st,
                format!(
            "SELECT CAST(SUM(wave) AS SIGNED) AS wave, CAST(SUM(minutes) AS SIGNED) AS minutes \
             FROM daily_metric WHERE person_id = {person_id} AND wave_reliable = 1 \
             AND DATE_FORMAT(biz_date, '%Y') = '{year}'"
        ),
            )
            .await?,
        )
    } else {
        None
    };
    let mut total = one(
        &st,
        format!(
            "SELECT CAST(SUM(wave) AS SIGNED) AS wave, CAST(SUM(minutes) AS SIGNED) AS minutes \
         FROM daily_metric WHERE person_id = {person_id} AND wave_reliable = 1"
        ),
    )
    .await?;

    /* 时长口径（2026-10-10 月度时长导入）：日明细里已经没有时长（每日不能导入时长），
    时长只有按月的整月值。所以月 / 年 / 累计的「时长」按「某月有整月导入值就用它、
    没有才用日明细求和」算，音浪仍走 daily_metric（音浪不按月导入）。 */
    if let Some(v) = monthly.as_mut() {
        v["minutes"] = json!(my_minutes(&st, person_id, &MinutesScope::Month(&month)).await?);
    }
    if let Some(v) = yearly.as_mut() {
        v["minutes"] = json!(my_minutes(&st, person_id, &MinutesScope::Year(&year)).await?);
    }
    total["minutes"] = json!(my_minutes(&st, person_id, &MinutesScope::Total).await?);

    /* 同期对照（1.0.1）：本人三个上期音浪，一条 SQL。上期没数据 → NULL → 前端不显示差值。 */
    let mut prev_wave_map: HashMap<&str, Option<i64>> = HashMap::new();
    if s.wave && !today.is_empty() {
        let sql = self_prev_wave_sql(&prev).replace("{pid}", &person_id.to_string());
        let source = sid(&st);
        if let Some(r) = mysql::query(&st.pools, &source, &sql)
            .await?
            .into_iter()
            .next()
        {
            let get = |col: &str| -> Option<i64> {
                r.try_get::<i64, _>(col)
                    .ok()
                    .or_else(|| r.try_get::<Option<i64>, _>(col).ok().flatten())
            };
            prev_wave_map.insert("daily", get("pd"));
            prev_wave_map.insert("monthly", get("pm"));
            prev_wave_map.insert("yearly", get("py"));
        }
    }
    let attach_prev = |v: &mut Value, key: &str| {
        if let Some(o) = v.as_object_mut() {
            o.insert(
                "prevWave".into(),
                match prev_wave_map.get(key) {
                    Some(Some(n)) => json!(n),
                    _ => Value::Null,
                },
            );
        }
    };
    if let Some(v) = daily.as_mut() {
        attach_prev(v, "daily");
    }
    if let Some(v) = monthly.as_mut() {
        attach_prev(v, "monthly");
    }
    if let Some(v) = yearly.as_mut() {
        attach_prev(v, "yearly");
    }

    let source = sid(&st);
    /* 团内名次（按累计）。口径与 daily 榜单对齐：离队的不占名次，
    否则粉丝页显示的名次会和榜单差几个人，对不上。
    P1-7：只聚合可信行（wave_reliable=1）。
    2026-10-11 起按团过滤：以前不过滤，男女团同表混排，「团内名次」名不副实。
    过滤手法与 lineage 相同：团别为空串 = 全公会不过滤（保持旧行为）；
    token 里的团别只会是 male/female/all/空串（all 归一成空串——gender 列没有
    all 这个值，不过滤才对），与 lineage 的 query_team 同级信任，沿用 format! 拼 SQL。 */
    let rank_team = if my_team == "all" { "" } else { &my_team };
    let rank_sql = format!(
        "SELECT person_id, tw FROM (SELECT p.id AS person_id, \
          CAST(SUM(d.wave) AS SIGNED) AS tw \
         FROM daily_metric d JOIN person p ON p.id = d.person_id \
         WHERE p.deleted_at IS NULL \
           AND ('{rank_team}' = '' OR p.gender = '{rank_team}') \
           AND d.wave_reliable = 1 \
         GROUP BY p.id ORDER BY tw DESC) t"
    );
    let ranks = mysql::query(&st.pools, &source, &rank_sql).await?;
    let my_pos = ranks.iter().position(|r| id_of_pub(r) == person_id);
    let team_rank = my_pos.map(|i| i + 1).unwrap_or(0);
    /* 2026-10-12 用户口径：我的页只显示自己的排名。rankGap / teamSize 不再计算
    也不再下发（响应里恒 null）——rank_sql 本身保留（teamRank 还要用），
    前一名差值和团总人数的查询省掉：每次打开我的页少一条 COUNT 查询，
    也不再让多余数值出网。 */

    /* 姓名 + 师傅姓名一条 SQL 带出（LEFT JOIN，没有师傅 = NULL → 前端「师承【xxx】」
    副标题不显示）。师傅姓名在公开族谱里本来就下发（lineage 节点的 masterName），
    没有额外的团别/隐私顾虑。 */
    let name_sql = format!(
        "SELECT p.name, m.name AS master_name FROM person p \
         LEFT JOIN person m ON m.id = p.master_id AND m.deleted_at IS NULL \
         WHERE p.id = {person_id} AND p.deleted_at IS NULL LIMIT 1"
    );
    let (name, master_name) = mysql::query(&st.pools, &source, &name_sql)
        .await?
        .into_iter()
        .next()
        .map(|r| {
            (
                r.try_get::<String, _>("name").unwrap_or_default(),
                r.try_get::<Option<String>, _>("master_name").ok().flatten(),
            )
        })
        .unwrap_or_default();

    /* 数值显隐（与每日报告同一套后台开关）：关「音浪数值」→ wave=null；
    关「直播时长」→ minutes=null。名次/人数不是数值，照常下发。 */
    for v in [&mut daily, &mut monthly, &mut yearly]
        .into_iter()
        .flatten()
    {
        if !s.wave {
            v["wave"] = Value::Null;
        }
        if !s.duration {
            v["minutes"] = Value::Null;
        }
    }
    if !s.wave {
        total["wave"] = Value::Null;
    }
    if !s.duration {
        total["minutes"] = Value::Null;
    }
    if !s.wave {
        // 音浪数值都隐藏了，同期对照也不能给（可反推）
        for v in [&mut daily, &mut monthly, &mut yearly]
            .into_iter()
            .flatten()
        {
            if let Some(o) = v.as_object_mut() {
                o.insert("prevWave".into(), Value::Null);
            }
        }
    }

    // 「徒弟 + 师傅」门下累计音浪（2026-10-11）：师傅自己 + 所有徒弟的累计音浪。
    let (disciple_count, disciple_wave, total_wave) =
        lineage_wave_summary(&st, &my_team, person_id)
            .await
            .unwrap_or((0, 0, 0));

    Ok(Json(json!({
        "published": true,
        "personId": person_id,
        "name": name,
        "date": today,
        "daily": daily,
        "monthly": monthly,
        "yearly": yearly,
        "total": total,
        "teamRank": team_rank,
        /* 2026-10-12 用户口径：我的页只显示自己的排名，不再下发 teamSize / rankGap。
        团总人数 + 距上一名差值能反推团队分布（结合排名可推出前面所有人的量级），
        与「可反推的一律 null」的安全口径一致。字段保留位置，前端老版本读 null 不炸。 */
        "teamSize": Value::Null,
        "rankGap": Value::Null,
        // 本人的师傅姓名（展示端「我的」页副标题「师承【xxx】」），没有师傅 = null
        "masterName": master_name,
        // 门下音浪统计：直系徒弟数 / 徒弟音浪总和 / 门下累计（自己 + 徒弟）
        "disciple": {
            "count": disciple_count,
            "wave": if s.wave { json!(disciple_wave) } else { Value::Null },
            "totalWave": if s.wave { json!(total_wave) } else { Value::Null },
        },
        // 管理员观看账号（personId=0）：前端据此隐藏「绑定抖音号」表单
        "admin": person_id == 0,
    })))
}

/// GET /api/public/teams/{team}/my/series?days=（token 走 X-My-Token 头）
/// 本人近 N 个业务日的音浪与时长（按日期正序）。只返回这一个人的序列，
/// 不下发密码，也不下发团里任何别人的数据。
pub async fn my_series(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<MyQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    let (person_id, my_team, _) = my_token_required(&headers)?;
    if my_team != team && team != "all" {
        return Err(anyhow::anyhow!("该账号不属于本团").into());
    }
    let days = q.days.unwrap_or(14).clamp(1, 30);
    let dates = crate::repos::get_available_dates(&st, None, false).await?;
    let mut take: Vec<String> = dates.into_iter().take(days as usize).collect();
    take.reverse(); // 折线图要正序：老 → 新
    let empty_dates: Vec<String> = Vec::new();
    if take.is_empty() {
        return Ok(Json(
            json!({ "published": true, "dates": empty_dates, "waves": [], "minutes": [] }),
        ));
    }

    // P1-7：序列只聚合可信行（wave_reliable=1），与展示页曲线/榜单口径一致
    let sql = format!(
        "SELECT DATE_FORMAT(biz_date, '%Y-%m-%d') AS d, \
         CAST(SUM(wave) AS SIGNED) AS wave, CAST(SUM(minutes) AS SIGNED) AS minutes \
         FROM daily_metric WHERE person_id = {person_id} AND wave_reliable = 1 \
         GROUP BY d ORDER BY d DESC LIMIT {days}"
    );
    let rows = mysql::query(&st.pools, &sid(&st), &sql).await?;
    let mut wave_map: HashMap<String, i64> = HashMap::new();
    let mut min_map: HashMap<String, i64> = HashMap::new();
    for r in &rows {
        let d = r.try_get::<String, _>("d").unwrap_or_default();
        wave_map.insert(d.clone(), row_i64(r, "wave"));
        min_map.insert(d, row_i64(r, "minutes"));
    }
    let waves: Vec<i64> = take
        .iter()
        .map(|d| wave_map.get(d).copied().unwrap_or(0))
        .collect();
    let minutes: Vec<i64> = take
        .iter()
        .map(|d| min_map.get(d).copied().unwrap_or(0))
        .collect();
    Ok(Json(json!({
        "published": true,
        "dates": take,
        // 后台关掉「音浪数值」：序列不下发数字，前端不画图
        "waves": if s.wave { json!(waves) } else { Value::Null },
        "minutes": if s.duration { json!(minutes) } else { Value::Null },
        "durationEnabled": s.duration,
    })))
}

/* --------------------- 我的抖音号（分账号音浪 2026-10-12） --------------------- */

/// 分账号音浪的四个窗口（2026-10-12 建）：今天的业务日 / 本月 / 今年。
/// 四窗口都由**库里最新业务日**推出，不能用自然日——数据是每晚补前一天的，
/// 凌晨打开页面时「今天」的行还不存在，用自然日会让每个号都显示 0（与 /my 同因）。
pub struct AccountPeriods {
    /// 最新业务日（YYYY-MM-DD）；库里没数据时为空串
    pub day: String,
    /// 业务日所在月（YYYY-MM）
    pub month: String,
    /// 业务日所在年（YYYY）
    pub year: String,
}

impl AccountPeriods {
    /// 由最新业务日推出三个窗口。空串输入 → 三个窗口都空（SQL 侧退化成「不命中任何行」）。
    pub fn of(today: &str) -> Self {
        Self {
            day: today.to_string(),
            month: today.get(..7).map(str::to_string).unwrap_or_default(),
            year: today.get(..4).map(str::to_string).unwrap_or_default(),
        }
    }
}

/// 分账号音浪 SQL（**一条查全部号**，不每人每号循环查库）。
///
/// 口径与 dialect::WAVES_A 完全对齐（2026-10-12），不许自己发明新口径：
/// - 主号（is_primary=1）走 daily_metric，且只算 wave_reliable=1 的行；
/// - 副号（is_primary=0）走 wave_snapshot.wave_value（当日值语义，已验证与 daily_metric 对得上）。
///
/// 两边按「号」UNION ALL 后再按 account.id 汇总，所以一个号的历史不会被算两遍：
/// 主号即便在 wave_snapshot 里也有历史行，也只取 daily_metric 那一支（与 WAVES_A 的
/// is_primary 互斥条件同源，改一处即可，别在两处各写一套）。
///
/// 窗口一律 `DATE_FORMAT(biz_date, ...)`（该库对 bind 参数报 1835，日期必须内联；
/// 值来自库里的 DATE_FORMAT 结果且经 [valid_date_pub] 形状校验，不含外部输入）。
/// 没有数据的号 SUM 出来是 NULL，调用方用 COALESCE 归零（有这个号但没播 = 0，
/// 只有后台开关关掉才下发 null，两者语义不同，前端靠它区分）。
pub fn my_accounts_sql(person_id: u64, p: &AccountPeriods) -> String {
    format!(
        "SELECT a.id AS aid, a.anchor_id, a.douyin_no, a.anchor_name, a.is_primary, \
           CAST(COALESCE(SUM(CASE WHEN t.d = '{day}' THEN t.w END), 0) AS SIGNED) AS wd, \
           CAST(COALESCE(SUM(CASE WHEN t.d LIKE '{month}-%' THEN t.w END), 0) AS SIGNED) AS wm, \
           CAST(COALESCE(SUM(CASE WHEN t.d LIKE '{year}-%' THEN t.w END), 0) AS SIGNED) AS wy, \
           CAST(COALESCE(SUM(t.w), 0) AS SIGNED) AS wt, \
           MAX(t.d) AS last_d, \
           COUNT(DISTINCT t.d) AS days \
         FROM account a LEFT JOIN ( \
           SELECT d.anchor_id AS anchor_id, DATE_FORMAT(d.biz_date, '%Y-%m-%d') AS d, \
                  d.wave AS w \
           FROM daily_metric d JOIN account pa \
             ON pa.person_id = d.person_id AND pa.anchor_id = d.anchor_id \
              AND pa.is_primary = 1 \
           WHERE d.person_id = {person_id} AND d.wave_reliable = 1 \
           UNION ALL \
           SELECT ws.anchor_id AS anchor_id, DATE_FORMAT(ws.biz_date, '%Y-%m-%d') AS d, \
                  ws.wave_value AS w \
           FROM wave_snapshot ws JOIN account sa \
             ON sa.anchor_id = ws.anchor_id AND sa.is_primary = 0 \
           WHERE sa.person_id = {person_id} \
         ) t ON t.anchor_id = a.anchor_id \
         WHERE a.person_id = {person_id} \
         GROUP BY a.id, a.anchor_id, a.douyin_no, a.anchor_name, a.is_primary \
         ORDER BY a.is_primary DESC, a.id",
        day = p.day,
        month = p.month,
        year = p.year,
    )
}

/// 一行查询结果 → 一个抖音号的 DTO（数值部分先全给，开关过滤在 [my_accounts_gate] 里统一做）。
fn my_account_row(r: &sqlx::mysql::MySqlRow) -> Value {
    let anchor_id = r.try_get::<String, _>("anchor_id").unwrap_or_default();
    let douyin_no = r.try_get::<String, _>("douyin_no").unwrap_or_default();
    let anchor_name = r.try_get::<String, _>("anchor_name").unwrap_or_default();
    let primary = r.try_get::<i8, _>("is_primary").unwrap_or(0) == 1;
    // 副号没有时长，也不判未播：这里只有音浪口径
    let last_date = r
        .try_get::<Option<String>, _>("last_d")
        .ok()
        .flatten()
        .filter(|s: &String| valid_date_pub(s));
    json!({
        "accountId": row_pid(r, "aid"),
        "anchorId": anchor_id,
        "douyinNo": douyin_no,
        "anchorName": anchor_name,
        "primary": primary,
        "daily": { "wave": row_i64(r, "wd") },
        "monthly": { "wave": row_i64(r, "wm") },
        "yearly": { "wave": row_i64(r, "wy") },
        "total": { "wave": row_i64(r, "wt") },
        /* 当日是否开播：与全站同口径（dialect::NOT_LIVE_WAVE_THRESHOLD，音浪 >= 2）。
        注意这是「号」级的开播，副号有音浪就算播了。 */
        "liveToday": dialect::is_live_wave(row_i64(r, "wd")),
        "lastDate": last_date,
        "days": row_i64(r, "days"),
    })
}

/// 后台显隐开关过滤（纯函数，便于单测）：
/// - 关「每日音浪」或关「音浪数值」→ daily 置 null（两者任一关都不给数字，否则可反推）；
/// - 关「月报」→ monthly null；关「年报」→ yearly null；关「音浪数值」→ total null；
/// - 开播标记 / 最后日期 / 天数不是音浪数值，照常下发（与 /my 里名次照常下发同理）。
///
/// 顺带回一份全号加总（与 /my 的合计口径一致：主号 daily_metric + 副号 wave_snapshot，
/// 因此 summary 与 /my 的 total 可以对得上，前端卡顶直接显示它）。
pub fn my_accounts_gate(items: &mut [Value], s: &PublicSettings) -> Value {
    let mut sum_d: i64 = 0;
    let mut sum_m: i64 = 0;
    let mut sum_y: i64 = 0;
    let mut sum_t: i64 = 0;
    for it in items.iter_mut() {
        let Some(o) = it.as_object_mut() else {
            continue;
        };
        let pick = |o: &serde_json::Map<String, Value>, k: &str| -> i64 {
            o.get(k)
                .and_then(|v| v.get("wave"))
                .and_then(Value::as_i64)
                .unwrap_or(0)
        };
        sum_d += pick(o, "daily");
        sum_m += pick(o, "monthly");
        sum_y += pick(o, "yearly");
        sum_t += pick(o, "total");
        if !(s.daily && s.wave) {
            o.insert("daily".into(), Value::Null);
        }
        if !s.monthly {
            o.insert("monthly".into(), Value::Null);
        }
        if !s.yearly {
            o.insert("yearly".into(), Value::Null);
        }
        if !s.wave {
            o.insert("total".into(), Value::Null);
        }
    }
    let one = |on: bool, v: i64| -> Value {
        if on {
            json!({ "wave": v })
        } else {
            Value::Null
        }
    };
    json!({
        "count": items.len(),
        "daily": one(s.daily && s.wave, sum_d),
        "monthly": one(s.monthly, sum_m),
        "yearly": one(s.yearly, sum_y),
        "total": one(s.wave, sum_t),
    })
}

/// GET /api/public/teams/{team}/my/accounts（token 走 X-My-Token 头）
/// 我的**每个抖音号**的日 / 月 / 年 / 累计音浪（一人多号时 /my 只有合计数，看不出哪个号在跑）。
/// 只下发本人数据；数值一律受后台「展示开关」控制，关掉的口径下发 null。
pub async fn my_accounts(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(_q): Query<MyQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    let (person_id, my_team, _) = my_token_required(&headers)?;
    if my_team != team && team != "all" {
        return Err(ApiError::auth("该账号不属于本团"));
    }
    // 管理员观看账号（personId=0）没有主播档案，也就没有抖音号：给空列表而不是报错，
    // 前端「我的」页顶部照常显示（与 /my 的 admin 分支同处理方式）
    if person_id == 0 {
        let mut empty: Vec<Value> = Vec::new();
        let summary = my_accounts_gate(&mut empty, &s);
        return Ok(Json(json!({
            "published": true,
            "date": "",
            "accounts": empty,
            "summary": summary,
        })));
    }

    /* 「今日」必须是库里最新的业务日，不能用自然日今天（数据与 /my 同因：每晚补前一天）。 */
    let dates = crate::repos::get_available_dates(&st, None, false).await?;
    let today = dates.first().cloned().unwrap_or_default();
    // 日期来自库里的 DATE_FORMAT 结果，这里再校一次形状，防止异常值内联进 SQL
    if !today.is_empty() && !valid_date_pub(&today) {
        return Err(ApiError::bad_request("业务日格式异常"));
    }
    let periods = AccountPeriods::of(&today);

    let sql = my_accounts_sql(person_id, &periods);
    let rows = mysql::query(&st.pools, &sid(&st), &sql).await?;
    let mut accounts: Vec<Value> = rows.iter().map(my_account_row).collect();
    let summary = my_accounts_gate(&mut accounts, &s);
    Ok(Json(json!({
        "published": true,
        "date": today,
        "accounts": accounts,
        "summary": summary,
    })))
}

/// GET /api/public/teams/{team}/my/who（token 走 X-My-Token 头）：轻量身份
/// （展示端登录门启动时用），只回姓名 + personId，不拉任何业务数据。
/// 2026-10-12 起附带 `bindRequest`（该账号最新一条绑定申请：{status, personName,
/// reason, createdAt} | null）——绑定改管理员审核制后，前端轮询这里展示审核进度。
pub async fn my_who(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(_q): Query<MyQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let (person_id, my_team, token_name) = my_token_required(&headers)?;
    if my_team != team && team != "all" {
        return Err(ApiError::auth("该账号不属于本团"));
    }
    let bind_req = crate::store::my_bind_request_latest(&st, &token_name).await;
    // personId=0 有两种：管理员观看账号（is_admin=1，姓名直接来自登录账号），
    // 以及「手机号注册了、还没绑主播」的账号（bound=0，前端要提示去绑定）
    if person_id == 0 {
        if let Some(info) = crate::store::my_account_info(&st, &token_name).await {
            if !info.bound && !info.is_admin {
                return Ok(Json(json!({
                    "ok": true, "personId": 0, "name": info.phone, "team": my_team,
                    "phone": info.phone, "admin": false, "bound": false, "needBind": true,
                    "bindRequest": bind_req,
                })));
            }
            if info.is_admin {
                return Ok(Json(json!({
                    "ok": true, "personId": 0, "name": token_name, "team": my_team,
                    "admin": true, "bound": true, "bindRequest": bind_req,
                })));
            }
        }
        return Ok(Json(json!({
            "ok": true, "personId": 0, "name": token_name, "team": my_team,
            "admin": true, "bound": true, "bindRequest": bind_req,
        })));
    }
    let sql = "SELECT name FROM person WHERE id = ? AND deleted_at IS NULL".to_string();
    let rows =
        mysql::query_with_binds(&st.pools, &sid(&st), &sql, |q| q.bind(person_id as i64)).await?;
    let name = rows
        .first()
        .and_then(|r| r.try_get::<String, _>("name").ok())
        .unwrap_or_default();
    if name.is_empty() {
        return Err(ApiError::auth("账号不存在或已离团，请联系管理员"));
    }
    // 水印口径（2026-10-11 用户要求）：登录后全屏水印显示自己的手机号（打码），
    // who 里带上 phone；账号没填手机号时前端回退艺名
    let phone = crate::store::my_account_phone(&st, &token_name).await;
    Ok(Json(json!({
        "ok": true, "personId": person_id, "name": name, "team": my_team,
        "phone": phone, "bound": true, "bindRequest": bind_req,
    })))
}

/* --------------------- 抖音号绑定申请（展示端提交 → 管理端审计） --------------------- */

/// GET /api/public/teams/{team}/my/claim（token 走 X-My-Token 头）：我的绑定状态
/// （最近一笔申请 + 已绑定的抖音号列表，让用户知道现在音浪记在哪个号上）
pub async fn my_claim_get(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(_q): Query<MyQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let (person_id, my_team, _) = my_token_required(&headers)?;
    if my_team != team && team != "all" {
        return Err(ApiError::auth("该账号不属于本团"));
    }
    let bound = mysql::query_with_binds(
        &st.pools,
        &sid(&st),
        "SELECT douyin_no, is_primary FROM account \
         WHERE person_id = ? AND status = 'active' ORDER BY is_primary DESC, id",
        |q| q.bind(person_id as i64),
    )
    .await?
    .iter()
    .filter_map(|r| {
        let no = r
            .try_get::<String, _>("douyin_no")
            .ok()
            .filter(|s| !s.is_empty())?;
        let primary = r.try_get::<i8, _>("is_primary").unwrap_or(0) == 1;
        Some(json!({ "douyinNo": no, "primary": primary }))
    })
    .collect::<Vec<_>>();
    Ok(Json(json!({
        "claim": crate::store::anchor_claim_mine(&st, person_id as i64).await,
        "bound": bound,
    })))
}

/// POST /api/public/teams/{team}/my/claim {token, douyinNo, screenshot}
/// screenshot = dataURL（data:image/jpeg;base64,…），服务端解码后入库待审计（anchor_claims）。
pub async fn my_claim_post(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let Some((person_id, my_team, _)) = my_token_take(token.trim()) else {
        return Err(ApiError::auth("登录已过期，请重新登录"));
    };
    if my_team != team && team != "all" {
        return Err(ApiError::auth("该账号不属于本团"));
    }
    if person_id == 0 {
        return Err(anyhow::anyhow!("管理员账号缺少对应的主播档案，不能绑定抖音号").into());
    }
    let douyin_no = body
        .get("douyinNo")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let data_url = body
        .get("screenshot")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (mime, b64) = data_url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(";base64,"))
        .ok_or_else(|| anyhow::anyhow!("截图数据格式不对，请重新选择图片"))?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|_| anyhow::anyhow!("截图数据格式非法（解码失败），请重新选择图片"))?;

    // 提交前先查重：抖音号已被占用就直接拒，省得用户白等审计
    let no = crate::store::validate_douyin_no(douyin_no).map_err(|e| anyhow::anyhow!(e))?;
    let dup = mysql::query_with_binds(
        &st.pools,
        &sid(&st),
        "SELECT person_id FROM account WHERE douyin_no = ? LIMIT 1",
        |q| q.bind(no.clone()),
    )
    .await?;
    if let Some(r) = dup.first() {
        let owner = r
            .try_get::<i64, _>("person_id")
            .map(|v| v as u64)
            .unwrap_or(0);
        if owner == person_id {
            return Err(anyhow::anyhow!("这个抖音号已经绑定在你名下了").into());
        }
        return Err(anyhow::anyhow!("该抖音号已绑定其他主播，如有疑问请联系管理员").into());
    }

    // 报错文案统一走 anyhow::anyhow!(中文) → 400（routes.rs 关键词）
    let name = person_name_of(&st, person_id).await?;
    crate::store::anchor_claim_insert(&st, person_id as i64, &name, &team, &no, &bytes, mime)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    crate::events::emit(
        crate::events::SCOPE_IMPORT,
        format!("绑定申请：{} 提交抖音号 {}（待审计）", name, no),
    );
    Ok(Json(json!({ "ok": true })))
}

/// person 表里找名字（claim 提交与 whoami 共用）
async fn person_name_of(st: &AppState, person_id: u64) -> anyhow::Result<String> {
    let sql = "SELECT name FROM person WHERE id = ? AND deleted_at IS NULL";
    let rows =
        mysql::query_with_binds(&st.pools, &sid(st), sql, |q| q.bind(person_id as i64)).await?;
    Ok(rows
        .first()
        .and_then(|r| r.try_get::<String, _>("name").ok())
        .unwrap_or_default())
}

/* --------------------- 抖音号绑定审计（管理端） --------------------- */

/// GET /api/admin/anchor-claims?status=pending：审计列表（不含截图本体）
pub async fn admin_anchor_claims(
    State(st): State<ArcState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let status = q.get("status").cloned().unwrap_or_default();
    Ok(Json(
        json!({ "items": crate::store::anchor_claim_list(&st, &status, 200).await }),
    ))
}

/// GET /api/admin/anchor-claims/{id}/screenshot：截图本体
/// （<img> 无法带 Authorization 头，走 ?access_token=，与 auth.rs 的放宽规则一致）
pub async fn admin_anchor_claim_screenshot(
    State(st): State<ArcState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Response {
    match crate::store::anchor_claim_image(&st, id).await {
        Some((bytes, mime)) => ([(axum::http::header::CONTENT_TYPE, mime)], bytes).into_response(),
        None => (axum::http::StatusCode::NOT_FOUND, "没有这张截图").into_response(),
    }
}

/// POST /api/admin/anchor-claims/{id}/decide {approve, reason?}
/// 通过 → 写 MySQL account（抖音号精确命中是导入匹配第一优先级，此后音浪自动落到这个人）；
/// 驳回 → 记原因，用户可在展示端重新提交。
pub async fn admin_anchor_claim_decide(
    State(st): State<ArcState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let approve = body
        .get("approve")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let reason = body
        .get("reason")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if !approve && reason.is_empty() {
        return Err(anyhow::anyhow!("驳回要填写原因，请输入驳回原因").into());
    }

    // 1. 先占住裁决权（pending → approved/rejected 原子翻转，防双击重复绑定）
    let Some((person_id, no, person_name)) =
        crate::store::anchor_claim_decide(&st, id, approve, &reason, "admin").await
    else {
        return Err(anyhow::anyhow!("这笔申请不存在或已经处理过了").into());
    };

    if !approve {
        crate::events::emit(
            crate::events::SCOPE_IMPORT,
            format!("绑定驳回：{} 的抖音号 {no}（{reason}）", person_name),
        );
        return Ok(Json(json!({ "ok": true, "status": "rejected" })));
    }

    // 2. 通过：写 account。抖音号占用检查 + 主号判定（此人还没有账号 → 主号）
    let writer = st
        .writer
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("写库不可用：douyin_writer 未配置"))?;
    let dup = sqlx::query("SELECT person_id FROM account WHERE douyin_no = ? LIMIT 1")
        .bind(&no)
        .fetch_optional(writer)
        .await?;
    if let Some(r) = dup {
        let owner = r
            .try_get::<i64, _>("person_id")
            .map(|v| v as u64)
            .unwrap_or(0);
        // 回滚裁决，让管理员能改判或处理冲突
        crate::store::anchor_claim_revert(&st, id).await;
        return Err(match owner == person_id as u64 {
            true => anyhow::anyhow!("该抖音号已存在绑定记录（就在该主播名下）"),
            false => anyhow::anyhow!("该抖音号已存在绑定记录（绑在其他主播名下）"),
        }
        .into());
    }
    let cnt = sqlx::query("SELECT COUNT(*) AS n FROM account WHERE person_id = ?")
        .bind(person_id)
        .fetch_one(writer)
        .await?
        .try_get::<i64, _>("n")
        .unwrap_or(0);
    let ins = sqlx::query(
        "INSERT INTO account (person_id, anchor_id, douyin_no, anchor_name, is_primary) \
         VALUES (?,?,?,?,?)",
    )
    .bind(person_id)
    .bind(&no)
    .bind(&no)
    .bind(&person_name)
    .bind(if cnt == 0 { 1i8 } else { 0i8 })
    .execute(writer)
    .await;
    if let Err(e) = ins {
        // account 写失败必须回滚裁决，否则申请卡在「已通过」却没有账号
        crate::store::anchor_claim_revert(&st, id).await;
        let msg = e.to_string();
        return Err(if msg.contains("Duplicate entry") {
            anyhow::anyhow!("抖音号或采集 ID 已存在（唯一键冲突），请检查是否重复绑定").into()
        } else {
            anyhow::anyhow!("写入账号失败：{msg}").into()
        });
    }

    // 审核通过 = 这个号正式绑到该主播名下：暂存区里这个号的导入行立刻补账，
    // 不用等下一次导入（用户口径：建档/绑号后才显示，绑定的这一刻就算「添加上主播」）
    let absorbed = match crate::admin::absorb_pending(&st, person_id as u64).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(person_id, error = %e, "绑定通过后暂存补账失败（不影响绑定）");
            0
        }
    };

    crate::events::emit(
        crate::events::SCOPE_ROSTER,
        format!(
            "绑定通过：{} ← 抖音号 {no}，音浪将从下次导入起落到 TA 名下{}",
            person_name,
            if absorbed > 0 {
                format!("（已补账 {absorbed} 行暂存数据）")
            } else {
                String::new()
            }
        ),
    );
    Ok(Json(json!({
        "ok": true,
        "status": "approved",
        "personId": person_id,
        "douyinNo": no,
        "primary": cnt == 0,
        "absorbed": absorbed,
    })))
}

fn id_of_pub(r: &sqlx::mysql::MySqlRow) -> u64 {
    if let Ok(v) = r.try_get::<u64, _>("person_id") {
        return v;
    }
    if let Ok(v) = r.try_get::<i64, _>("person_id") {
        return v as u64;
    }
    0
}

/* ------------------ 我的页面 · 微信推送（绑定 + 追分目标 + 每日报告） ------------------ */

/// 登录态 → (personId, 团别, 账号姓名)。推送相关接口全部走这一处校验。
fn my_ident(team: &str, token: &str) -> Result<(u64, String, String), ApiError> {
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
fn mask_phone(p: &str) -> String {
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

type PendingWxLogin = (String, u64, String, String, Instant);
type PendingWxLoginMap = HashMap<String, PendingWxLogin>;

fn pending_wx_logins() -> &'static Mutex<PendingWxLoginMap> {
    static M: OnceLock<Mutex<PendingWxLoginMap>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pending_wx_put(qrcode: &str, name: &str, person_id: u64, team: &str) {
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
fn pending_wx_take(qrcode: &str, name: &str) -> Option<(u64, String, String)> {
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
fn pending_wx_set_base(qrcode: &str, base_url: &str) {
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
    crate::store::target_delete(&st, &name, id).await;
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

/* --------------------------- 公开展示页 --------------------------- */

const ST_PAGE: &str = include_str!("st_page.html");

/// GET / 根路径：展示页（2026-10-11 起不分男女，统一读 /api/public/teams/all）
pub async fn st_root() -> Response {
    st_page_for("all")
}

/// GET /1 旧男团链接：兼容重定向到统一首页
pub async fn st_m() -> Response {
    st_page_for("all")
}

/// GET /0 旧女团链接：兼容重定向到统一首页
pub async fn st_0() -> Response {
    st_page_for("all")
}

/// GET /st/{team}：旧团别链接统一渲染首页
pub async fn st_page(axum::extract::Path(_team): axum::extract::Path<String>) -> Response {
    st_page_for("all")
}

/// GET /st/{team}/{*rest}：展示端子路径路由（/report、/group/junior、/lineage…）。
/// 单页应用：子路径一律渲染同一个 HTML，视图切换由前端 history API 完成，
/// 刷新/分享子路径链接都不会 404。
pub async fn st_page_sub(
    axum::extract::Path((_team, _rest)): axum::extract::Path<(String, String)>,
) -> Response {
    st_page_for("all")
}

fn st_page_for(team: &str) -> Response {
    let html = ST_PAGE.replace("{{TEAM}}", team);
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

#[cfg(test)]
mod register_route_tests {
    use super::*;

    /// 注册页的团别白名单：male/female（兼容 m/f），其它一律空串（老链接/异常入参）
    #[test]
    fn team_whitelist_for_register() {
        assert_eq!(normalize_team("male"), "male");
        assert_eq!(normalize_team("M"), "male");
        assert_eq!(normalize_team("female"), "female");
        assert_eq!(normalize_team(" f "), "female");
        assert_eq!(normalize_team(""), "");
        assert_eq!(normalize_team("男团"), "");
    }
}

#[cfg(test)]
mod announcement_tests {
    use super::*;

    /// 图标白名单：合法名原样保留，白名单外 / 空值一律回落成按步序号的默认。
    /// 这一步是防「任意字符串进配置 → 前端拼进 class」的唯一关口，必须守住。
    #[test]
    fn icon_for_whitelists_and_falls_back() {
        // 白名单内：原样
        let s = AnnounceStep {
            title: "x".into(),
            icon: "star".into(),
            content: "y".into(),
        };
        assert_eq!(Announcement::icon_for(&s, 3), "star");

        // 白名单外（含 HTML / 脚本注入尝试）：回落默认。
        // 注意 "star " 不在里面：icon_for 会先 trim，"star "trim 后落在白名单内，
        // 所以它算合法输入（见下一条），这里只收真正非法的值。
        for bad in [
            "<script>",
            "foo",
            "STAR",
            "star\"",
            "../etc/passwd",
            "star;",
            "star\nonmore",
            "star twist",
        ] {
            let s = AnnounceStep {
                title: "x".into(),
                icon: bad.into(),
                content: "y".into(),
            };
            let got = Announcement::icon_for(&s, 0);
            assert_eq!(got, DEFAULT_STEP_ICONS[0], "{bad:?} 该回落默认");
            assert!(
                STEP_ICON_WHITELIST.contains(&got.as_str()),
                "{got} 必须在白名单内"
            );
        }

        // 首尾空白是合法输入的容忍（trim 后命中白名单），不是攻击面
        let padded = AnnounceStep {
            title: "x".into(),
            icon: " star ".into(),
            content: "y".into(),
        };
        assert_eq!(Announcement::icon_for(&padded, 0), "star");

        // 空 icon 按步序号轮换，不会满屏同一个
        let empty = AnnounceStep::default();
        assert_eq!(Announcement::icon_for(&empty, 0), DEFAULT_STEP_ICONS[0]);
        assert_eq!(
            Announcement::icon_for(&empty, DEFAULT_STEP_ICONS.len()),
            DEFAULT_STEP_ICONS[0],
            "超出长度要回绕，不能越界"
        );
        assert_ne!(
            Announcement::icon_for(&empty, 0),
            Announcement::icon_for(&empty, 1)
        );
    }

    /// 默认图标表自己不能重复、不能越界（前端按步序号取默认，重复会让
    /// 「第 3 步」和「第 1 步」长得一样）
    #[test]
    fn default_icons_are_unique_and_whitelisted() {
        let mut seen = std::collections::HashSet::new();
        for i in DEFAULT_STEP_ICONS {
            assert!(seen.insert(i), "默认图标重复：{i}");
            assert!(
                STEP_ICON_WHITELIST.contains(i),
                "默认图标必须在白名单内：{i}"
            );
        }
        assert!(
            DEFAULT_STEP_ICONS.len() >= 4,
            "默认图标要够 4 步以上的引导轮换"
        );
    }

    /// to_json 下发的 icon 是归一化后的值，不是空串（前端不猜）
    #[test]
    fn to_json_always_emits_a_usable_icon() {
        let a = Announcement {
            enabled: true,
            steps: vec![
                AnnounceStep {
                    title: "a".into(),
                    icon: String::new(),
                    content: "1".into(),
                },
                AnnounceStep {
                    title: "b".into(),
                    icon: "  ".into(),
                    content: "2".into(),
                },
            ],
            version: 1,
        };
        let v = a.to_json();
        let icons: Vec<&str> = v["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["icon"].as_str().unwrap())
            .collect();
        assert_eq!(icons.len(), 2);
        for i in &icons {
            assert!(!i.is_empty(), "下发的 icon 不能为空");
            assert!(STEP_ICON_WHITELIST.contains(i), "下发的 {i} 不在白名单");
        }
        assert_ne!(icons[0], icons[1], "相邻步骤默认图标应不同");
    }
}

#[cfg(test)]
mod my_accounts_tests {
    use super::*;

    /// 三个窗口都由**业务日**推出：展示端数据是每晚补前一天的，
    /// 用自然日会让「今天」的每个号都显示 0（/my 踩过同样的坑）
    #[test]
    fn periods_follow_business_date() {
        let p = AccountPeriods::of("2026-10-11");
        assert_eq!(p.day, "2026-10-11");
        assert_eq!(p.month, "2026-10");
        assert_eq!(p.year, "2026");

        // 跨年：1 月的业务日不能推出上一年的窗口
        let p = AccountPeriods::of("2027-01-01");
        assert_eq!(p.month, "2027-01");
        assert_eq!(p.year, "2027");
    }

    /// 库里没数据（today 空串）时三个窗口都空：SQL 侧「不命中任何行」，
    /// 每个号给 0，接口不能炸
    #[test]
    fn periods_of_empty_date_are_empty() {
        let p = AccountPeriods::of("");
        assert_eq!(p.day, "");
        assert_eq!(p.month, "");
        assert_eq!(p.year, "");
        let sql = my_accounts_sql(86, &p);
        assert!(sql.contains("t.d = ''"), "{sql}");
        assert!(sql.contains("t.d LIKE '-%'"), "{sql}");
    }

    /// SQL 形状的硬约束（口径回归）：
    /// - 主号只走 daily_metric 且 wave_reliable=1，副号只走 wave_snapshot；
    /// - 两侧都用 account 的 is_primary 过滤，一个号的历史不会被算两遍；
    /// - 只查本人的号（person_id 内联），排序主号在前。
    #[test]
    fn accounts_sql_keeps_one_source_per_account() {
        let sql = my_accounts_sql(86, &AccountPeriods::of("2026-10-11"));
        assert!(
            sql.contains("pa.is_primary = 1"),
            "主号走 daily_metric：{sql}"
        );
        assert!(sql.contains("d.wave_reliable = 1"), "主号只算可信行：{sql}");
        assert!(
            sql.contains("sa.is_primary = 0"),
            "副号走 wave_snapshot：{sql}"
        );
        // 主号分支按「(person_id, anchor_id)」回 account，副号分支按 anchor_id 回 account，
        // 两边互斥 → 既是主号又在 wave_snapshot 有历史行的号不会重复计
        assert!(sql.contains("ON pa.person_id = d.person_id AND pa.anchor_id = d.anchor_id"));
        assert!(sql.contains("ON sa.anchor_id = ws.anchor_id AND sa.is_primary = 0"));
        // 日期窗口一律 DATE_FORMAT，不按自然日
        assert!(sql.contains("DATE_FORMAT(d.biz_date, '%Y-%m-%d')"));
        assert!(sql.contains("DATE_FORMAT(ws.biz_date, '%Y-%m-%d')"));
        assert!(sql.contains("t.d LIKE '2026-10-%'"));
        assert!(sql.contains("t.d LIKE '2026-%'"));
        // 只查本人的号
        assert_eq!(sql.matches("person_id = 86").count(), 3, "{sql}");
        // 主号在前，其余按 id 升序（与 admin 台账 ORDER BY is_primary DESC, id 一致）
        assert!(sql.ends_with("ORDER BY a.is_primary DESC, a.id"), "{sql}");
        // 没数据的号要有 0 而不是 NULL（有号没播 = 0），所以外层 COALESCE
        assert!(sql.contains("COALESCE(SUM(t.w), 0)"), "{sql}");
        assert!(sql.contains("COUNT(DISTINCT t.d) AS days"), "{sql}");
        assert!(sql.contains("MAX(t.d) AS last_d"), "{sql}");
    }

    /// 开关全开：数值照常下发，summary 是各号加总
    #[test]
    fn gate_all_on_passes_numbers_and_sums_them() {
        let s = PublicSettings::default();
        let mut items = vec![
            json!({
                "accountId": 1, "daily": {"wave": 100}, "monthly": {"wave": 1000},
                "yearly": {"wave": 5000}, "total": {"wave": 9000}, "liveToday": true
            }),
            json!({
                "accountId": 2, "daily": {"wave": 1}, "monthly": {"wave": 20},
                "yearly": {"wave": 300}, "total": {"wave": 400}, "liveToday": false
            }),
        ];
        let sum = my_accounts_gate(&mut items, &s);
        assert_eq!(sum["count"], 2);
        assert_eq!(sum["daily"]["wave"], 101);
        assert_eq!(sum["monthly"]["wave"], 1020);
        assert_eq!(sum["yearly"]["wave"], 5300);
        assert_eq!(sum["total"]["wave"], 9400);
        assert_eq!(items[0]["daily"]["wave"], 100);
        assert_eq!(items[1]["total"]["wave"], 400);
    }

    /// 关掉的口径一律下发 null：给数字就能靠差值反推出被关掉的口径（/my 的 prevWave 同理）
    #[test]
    fn gate_nulls_disabled_periods() {
        let s = PublicSettings {
            daily: false,
            monthly: false,
            yearly: false,
            wave: false,
            ..Default::default()
        };
        let mut items = vec![json!({
            "daily": {"wave": 100}, "monthly": {"wave": 1000},
            "yearly": {"wave": 5000}, "total": {"wave": 9000},
            "liveToday": true, "lastDate": "2026-10-11", "days": 9
        })];
        let sum = my_accounts_gate(&mut items, &s);
        assert!(items[0]["daily"].is_null());
        assert!(items[0]["monthly"].is_null());
        assert!(items[0]["yearly"].is_null());
        assert!(items[0]["total"].is_null());
        assert!(sum["daily"].is_null());
        assert!(sum["monthly"].is_null());
        assert!(sum["yearly"].is_null());
        assert!(sum["total"].is_null());
        // 开播标记 / 最后日期 / 天数不是音浪数值，照常下发
        assert_eq!(items[0]["liveToday"], true);
        assert_eq!(items[0]["lastDate"], "2026-10-11");
        assert_eq!(items[0]["days"], 9);
    }

    /// 关「音浪数值」时必须连 daily 一起 null：daily 就是音浪，
    /// 只关 wave 却留 daily 等于没关
    #[test]
    fn gate_wave_off_also_hides_daily() {
        let s = PublicSettings {
            wave: false,
            ..Default::default()
        };
        let mut items = vec![json!({
            "daily": {"wave": 100}, "monthly": {"wave": 1000},
            "yearly": {"wave": 5000}, "total": {"wave": 9000}
        })];
        let sum = my_accounts_gate(&mut items, &s);
        assert!(items[0]["daily"].is_null());
        assert!(items[0]["total"].is_null());
        assert!(sum["daily"].is_null());
        assert!(sum["total"].is_null());
        // 月 / 年开关没关，照常下发
        assert_eq!(items[0]["monthly"]["wave"], 1000);
        assert_eq!(items[0]["yearly"]["wave"], 5000);
    }

    /// 空结果（这个主播还没绑号 / 管理员观看账号）不炸：count=0，合计 0
    #[test]
    fn gate_empty_does_not_panic() {
        let s = PublicSettings::default();
        let mut items: Vec<Value> = Vec::new();
        let sum = my_accounts_gate(&mut items, &s);
        assert_eq!(sum["count"], 0);
        assert_eq!(sum["daily"]["wave"], 0);
        assert_eq!(sum["total"]["wave"], 0);
        assert!(items.is_empty());
    }

    /// 缺字段的行（副号刚绑上、啥数据都没有）按 0 参与合计，不能 panic
    #[test]
    fn gate_missing_wave_fields_counts_as_zero() {
        let s = PublicSettings::default();
        let mut items = vec![json!({ "accountId": 3, "daily": null, "monthly": {} })];
        let sum = my_accounts_gate(&mut items, &s);
        assert_eq!(sum["count"], 1);
        assert_eq!(sum["daily"]["wave"], 0);
        assert_eq!(sum["monthly"]["wave"], 0);
        assert_eq!(sum["yearly"]["wave"], 0);
        assert_eq!(sum["total"]["wave"], 0);
    }
}

#[cfg(test)]
mod lineage_window_tests {
    use super::*;

    /// 族谱窗口解析（2026-10-12）：period 非法回落 daily；date 形状非法回落最新业务日。
    #[test]
    fn lineage_window_falls_back() {
        // 默认 / 非法 period 都回落 daily，date 不传用 latest
        assert_eq!(
            lineage_window(None, None, Some("2026-10-09")),
            ("daily".to_string(), "2026-10-09".to_string())
        );
        assert_eq!(
            lineage_window(Some("week"), Some("not-a-date"), Some("2026-10-09")),
            ("daily".to_string(), "2026-10-09".to_string())
        );

        // date 形状非法回落 latest，period 不因此被改写（monthly 仍生效）
        assert_eq!(
            lineage_window(Some("monthly"), Some("2026-02-30"), Some("2026-10-09")),
            ("monthly".to_string(), "2026-10-09".to_string())
        );

        // 显式合法 date 覆盖 latest
        assert_eq!(
            lineage_window(Some("monthly"), Some("2026-09-15"), Some("2026-10-09")),
            ("monthly".to_string(), "2026-09-15".to_string())
        );
        assert_eq!(
            lineage_window(Some("yearly"), Some("2026-01-01"), None),
            ("yearly".to_string(), "2026-01-01".to_string())
        );

        // 无业务日（库里没数据）时空字符串，SQL 侧退化成全空库
        assert_eq!(
            lineage_window(None, None, None),
            ("daily".to_string(), String::new())
        );
    }

    /// 族谱音浪窗口 SQL（2026-10-12）：daily/monthly/yearly 条件 + 防注入双保险。
    #[test]
    fn lineage_wave_window_sql_period_and_fallbacks() {
        // daily：精确业务日
        let daily = lineage_wave_window_sql("daily", "2026-10-09");
        assert!(daily.contains("WHERE wave_reliable = 1"), "{daily}");
        assert!(
            daily.contains("DATE_FORMAT(biz_date, '%Y-%m-%d') = '2026-10-09'"),
            "{daily}"
        );

        // monthly：该业务日所在月（业务日口径，不用自然日）
        let monthly = lineage_wave_window_sql("monthly", "2026-10-09");
        assert!(
            monthly.contains("DATE_FORMAT(biz_date, '%Y-%m') = '2026-10'"),
            "{monthly}"
        );
        assert!(!monthly.contains("'2026-10-09'"), "{monthly}");

        // yearly：该业务日所在年
        let yearly = lineage_wave_window_sql("yearly", "2026-10-09");
        assert!(
            yearly.contains("DATE_FORMAT(biz_date, '%Y') = '2026'"),
            "{yearly}"
        );

        // 非法 period 回落 daily
        assert_eq!(lineage_wave_window_sql("weekly", "2026-10-09"), daily);
        assert_eq!(lineage_wave_window_sql("", "2026-10-09"), daily);

        // date 非法不能进 SQL：没有业务日时空库查询，不能带用户输入
        for bad in [
            "1=1",
            "2026-13-40",
            "2026-01-01 OR 1=1",
            "2026-01-01; DROP TABLE x",
        ] {
            let sql = lineage_wave_window_sql("daily", bad);
            assert!(
                sql.contains("FROM daily_metric WHERE wave_reliable = 1 GROUP BY person_id"),
                "{bad} -> {sql}"
            );
            assert!(!sql.contains(bad), "{bad} 不许进 SQL: {sql}");
            assert!(
                !sql.contains("DATE_FORMAT(biz_date, '%Y-%m-%d') ="),
                "非法 date 不许进窗口条件：{sql}"
            );
        }
        // monthly/yearly 也一样：非法 date 退回空库口径
        assert!(!lineage_wave_window_sql("monthly", "DROP TABLE x").contains("DROP"));
        assert!(!lineage_wave_window_sql("yearly", "DROP TABLE x").contains("DROP"));
    }
}
