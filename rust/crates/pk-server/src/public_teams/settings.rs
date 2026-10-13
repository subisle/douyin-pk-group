//! public_teams::settings —— 自 public_teams.rs 按业务域拆分（对外语义不变）。

use super::*;

/* ------------------------------- 显隐开关 ------------------------------- */

/// 公开站点配置（2026-10-10 起存 config 表，cfg_key = public_settings；
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

pub(super) fn default_true() -> bool {
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
    pub(super) fn unified_display_is_enabled_when_any_legacy_team_is_enabled() {
        let mut s = PublicSettings::default();
        assert!(s.team_enabled("all"));

        s.male_enabled = false;
        assert!(s.team_enabled("all"));

        s.female_enabled = false;
        assert!(!s.team_enabled("all"));
        assert!(!s.team_enabled("unknown"));
    }

    #[test]
    pub(super) fn plan_visibility_follows_page_and_type_switches() {
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

pub(super) static PS_CACHE: std::sync::OnceLock<
    std::sync::Mutex<Option<(PublicSettings, std::time::Instant)>>,
> = std::sync::OnceLock::new();

pub(super) fn ps_cache() -> &'static std::sync::Mutex<Option<(PublicSettings, std::time::Instant)>>
{
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

    pub(super) fn team_enabled(&self, team: &str) -> bool {
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
    let mut v = PublicSettings::load(&st).await.to_json();
    // 🚩 流动小红旗条（2026-10-12）：称号 / 手动指定，存 config 表
    v["starTitle"] = json!(crate::store::config_get(&st, "star_title").await.unwrap_or_default());
    v["starOverride"] =
        json!(crate::store::config_get(&st, "star_override").await.unwrap_or_default());
    /* 🏆 争霸赛冠军横幅（2026-10-13）：整条文案可编（「称号|人名|音浪」），空 = 不显示 */
    v["champBanner"] =
        json!(crate::store::config_get(&st, "champ_banner").await.unwrap_or_default());
    v["starIcon"] = json!(crate::store::config_get(&st, "star_icon").await.unwrap_or_default());
    Ok(Json(v))
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
    // 🚩 小红旗条：称号 / 手动指定（「称号|人名|音浪」或「称号|人名」或「称号」）/ 图标，空串清除
    for (key, body_key) in [
        ("star_title", "starTitle"),
        ("star_override", "starOverride"),
        ("star_icon", "starIcon"),
        ("champ_banner", "champBanner"),
    ] {
        if let Some(v) = body.get(body_key) {
            let val = v.as_str().unwrap_or_default().trim().to_string();
            crate::store::config_set(&st, key, &val).await;
        }
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
    /// 三处公告位（2026-10-13）：登录页 / 展示页（看板）/ 个人页，各自独立开关与文案。
    /// text 支持换行；每处独立展示，互不影响。
    #[serde(default)]
    pub placements: AnnouncementPlacements,
    /// 庆祝配置（2026-10-13）：每日之星/争霸赛冠军登录后彩带庆祝 + 展示
    #[serde(default)]
    pub celebrate: CelebrateConfig,
}

/// 三处公告位（2026-10-13）：登录页（login）/ 展示页看板顶部（home）/ 个人页顶部（my）。
/// 整体挂在 Announcement 下随 cfg_key='announcement' 一起存取，旧数据缺字段时
/// serde(default) 回落全关（老行为：登录页/看板/个人页都没有公告条）。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
pub struct AnnouncementPlacements {
    /// 登录页公告（登录门卡片上方一条）
    #[serde(default)]
    pub login: Placement,
    /// 展示页（看板顶部）公告
    #[serde(default)]
    pub home: Placement,
    /// 个人页公告（「我的」页顶部）
    #[serde(default)]
    pub my: Placement,
}

/// 单个公告位：独立开关 + 文案 + 可选图标。
/// enabled=false 时对应接口不下发该条（前端也无从渲染），不只是前端隐藏。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
pub struct Placement {
    /// 该公告位是否启用
    #[serde(default)]
    pub enabled: bool,
    /// 公告文案；支持换行，可为空（空串 = 前端不渲染内容行）
    #[serde(default)]
    pub text: String,
    /// 可选图标（Material Symbols 名，如 star_tech）；空 = 不带图标。
    /// 入口在 update_announcement 走 STEP_ICON_WHITELIST 白名单，任意字符串不进配置。
    #[serde(default)]
    pub icon: String,
}

/// 庆祝配置（2026-10-13）：每日之星/争霸赛冠军登录后彩带庆祝 + 展示。
/// 语义与推送小红旗条共用一套兜底链：title 空 → 推送配置 star_title；name 空 →
/// star_override 里的人名。enabled=false = 登录后不弹彩带、横幅不闪。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
pub struct CelebrateConfig {
    /// 庆祝总开关（关 = 登录后不弹彩带、横幅不闪）
    #[serde(default)]
    pub enabled: bool,
    /// 彩带动画开关
    #[serde(default)]
    pub confetti: bool,
    /// 庆祝标题（如「初级争霸赛冠军」），空 = 用推送配置 star_title
    #[serde(default)]
    pub title: String,
    /// 庆祝人（艺名），空 = 用 star_override 的人名
    #[serde(default)]
    pub name: String,
    /// 音浪数（0 = 不显示数值）
    #[serde(default)]
    pub wave: i64,
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
    /// CSS 选择器（2026-10-13 spotlight 引导）：指向页面上的具体元素，
    /// 前端挖孔高亮 + 卡片贴位；空 = 旧的居中卡片模式（完全向后兼容）。
    #[serde(default)]
    pub target: String,
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

    pub(super) fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "steps": self.steps.iter().enumerate().map(|(i, s)| json!({
                "title": s.title,
                "content": s.content,
                // 下发前把空 icon 归一成默认值，前端不用再猜第几步该用什么
                "icon": Self::icon_for(s, i),
                // spotlight 引导的目标元素选择器（空 = 居中卡片）
                "target": s.target,
            })).collect::<Vec<_>>(),
            "version": self.version,
            // 公告位（2026-10-13）：全量下发，管理端编辑用；展示端各取所需
            "placements": {
                "login": Self::placement_json(&self.placements.login),
                "home": Self::placement_json(&self.placements.home),
                "my": Self::placement_json(&self.placements.my),
            },
            // 庆祝配置（2026-10-13）：全量下发（总开关 / 彩带 / 标题 / 人名 / 音浪数）
            "celebrate": {
                "enabled": self.celebrate.enabled,
                "confetti": self.celebrate.confetti,
                "title": self.celebrate.title,
                "name": self.celebrate.name,
                "wave": self.celebrate.wave,
            },
        })
    }

    /// 单个公告位 → 下发 JSON（2026-10-13）。text 原样下发（支持换行），
    /// icon 归一化：空白去掉、白名单外的丢弃（空 = 前端不带图标）。
    pub(super) fn placement_json(p: &Placement) -> Value {
        // 归一化先算好再进 json!（宏的值位置放不了语句块）
        let icon = p.icon.trim();
        let icon = if STEP_ICON_WHITELIST.contains(&icon) { icon } else { "" };
        json!({
            "enabled": p.enabled,
            "text": p.text,
            "icon": icon,
        })
    }

    /// 登录页公开端点（GET /api/public/announcements）用的庆祝裁剪 JSON：
    /// 只给登录前需要的那几个字段，与 to_json 全量形态分开，方便各端各取所需。
    pub fn celebrate_public_json(&self) -> Value {
        json!({
            "enabled": self.celebrate.enabled,
            "confetti": self.celebrate.confetti,
            "title": self.celebrate.title,
            "name": self.celebrate.name,
            "wave": self.celebrate.wave,
        })
    }

    /// 这一步实际用的图标：填了合法值就原样用，否则按步序号取默认。
    pub(super) fn icon_for(s: &AnnounceStep, index: usize) -> String {
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

/// POST /api/admin/announcement
/// {enabled?, steps?: [{title, content, icon?, target?}],
///  placements?: {login|home|my: {enabled?, text?, icon?}},   // 2026-10-13 公告位
///  celebrate?: {enabled?, confetti?, title?, name?, wave?}}  // 2026-10-13 庆祝
/// 部分更新语义：没传的字段/公告位保持原样（与 enabled/steps 同一套口径）。
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
                // spotlight 目标选择器：只收合法字符，长度兜底，不让任意长串进配置
                let target = s.get("target").and_then(|v| v.as_str()).unwrap_or("").trim();
                let target_ok = !target.is_empty()
                    && target.len() <= 120
                    && target.chars().all(|c| c.is_ascii_alphanumeric()
                        || " _-#.[\"]>:()~*=^|,$".contains(c));
                Some(AnnounceStep {
                    title: title.trim().chars().take(64).collect(),
                    // 白名单外的图标名直接丢掉（回落默认），任意字符串不进配置
                    icon: if STEP_ICON_WHITELIST.contains(&icon) {
                        icon.chars().take(48).collect()
                    } else {
                        String::new()
                    },
                    content: content.trim().chars().take(2000).collect(),
                    target: if target_ok { target.to_string() } else { String::new() },
                })
            })
            .collect();
    }
    // 每次保存都让版本前进，前端据此重新弹出（内容没变也弹，保证运营可主动重发）
    a.version += 1;

    /* ---------------- 公告位（2026-10-13）：login / home / my 三处独立 ----------------
    部分更新：没传的公告位整体保持原样；传了就逐字段合并（enabled/text/icon，
    缺哪个字段保哪个字段），与 enabled/steps 的部分更新语义一致。
    校验沿用引导弹窗同一道关口：icon 走 STEP_ICON_WHITELIST 白名单，白名单外
    直接丢（存空串 = 不带图标），任意字符串不进配置；text 支持换行，只做长度兜底。 */
    if let Some(placements) = body.get("placements").and_then(|v| v.as_object()) {
        let parse_placement = |cur: &Placement, v: &Value| -> Placement {
            Placement {
                enabled: v
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(cur.enabled),
                // text 允许换行；trim 只在字符层面兜底，不去掉中间换行
                text: v
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .chars()
                    .take(500) // 长度兜底：公告条不是长文，防止配置被撑爆
                    .collect(),
                // 与 AnnounceStep.icon 同一白名单：合法才收，任意字符串不进配置
                icon: {
                    let icon = v.get("icon").and_then(|v| v.as_str()).unwrap_or("").trim();
                    if STEP_ICON_WHITELIST.contains(&icon) {
                        icon.chars().take(48).collect()
                    } else {
                        String::new()
                    }
                },
            }
        };
        if let Some(v) = placements.get("login") {
            a.placements.login = parse_placement(&a.placements.login, v);
        }
        if let Some(v) = placements.get("home") {
            a.placements.home = parse_placement(&a.placements.home, v);
        }
        if let Some(v) = placements.get("my") {
            a.placements.my = parse_placement(&a.placements.my, v);
        }
    }

    /* ---------------- 庆祝配置（2026-10-13）：部分更新，逐字段合并 ----------------
    enabled/confetti 是开关，title/name 截断兜底，wave 负数归 0（音浪数不为负）。 */
    if let Some(c) = body.get("celebrate").and_then(|v| v.as_object()) {
        if let Some(v) = c.get("enabled").and_then(|v| v.as_bool()) {
            a.celebrate.enabled = v;
        }
        if let Some(v) = c.get("confetti").and_then(|v| v.as_bool()) {
            a.celebrate.confetti = v;
        }
        if let Some(v) = c.get("title").and_then(|v| v.as_str()) {
            a.celebrate.title = v.trim().chars().take(64).collect();
        }
        if let Some(v) = c.get("name").and_then(|v| v.as_str()) {
            a.celebrate.name = v.trim().chars().take(64).collect();
        }
        if let Some(v) = c.get("wave").and_then(|v| v.as_i64()) {
            a.celebrate.wave = v.max(0);
        }
    }

    a.save(&st).await;
    Ok(Json(json!({ "ok": true, "announcement": a.to_json() })))
}
