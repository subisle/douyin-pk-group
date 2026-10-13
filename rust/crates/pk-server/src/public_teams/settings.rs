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
    /// 展示页公告条（2026-10-14）：可配多条，数组顺序 = 展示端从上到下。
    /// 是冠军横幅的**唯一来源**，消掉 celebrate / champ_banner 两套配置撞车。
    /// 空数组时由 [`Announcement::load_effective`] 按 celebrate → champ_banner
    /// 内存合成一条（只在内存里做，绝不写库）；现有 celebrate 字段保留不动
    ///（彩带 / 登录庆祝逻辑仍用它）。
    #[serde(default)]
    pub items: Vec<AnnounceItem>,
}

/// 一条展示页公告条（2026-10-14，cfg_key='announcement' 的 `items` 元素）。
/// 全部字段 `#[serde(default)]`：旧配置 / 前端少传一律回落默认，反序列化不报错。
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AnnounceItem {
    /// 是否启用；false 的条目保存在配置里（后台可编辑），但公开下发时被过滤掉
    pub enabled: bool,
    /// Material Symbols 图标名，写入与下发都过 [`STEP_ICON_WHITELIST`]；白名单外 → ""
    pub icon: String,
    /// 标题（如「初级争霸赛冠军」）
    pub title: String,
    /// 人名（如「狼哲」）
    pub name: String,
    /// 音浪数（0 = 展示端不显示数值）
    pub wave: i64,
    /// 正文，支持换行
    pub text: String,
    /// "brand"（紫金渐变）| "plain"（白底描边）；空/非法归一为 "plain"
    pub tone: String,
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

/* ------------------- 公告条 items（2026-10-14）：归一化 / 迁移合成 ------------------- */

/// tone 归一：只有 `"brand"` 走紫金渐变，其余（空 / 非法 / `"plain"`）一律 `"plain"`
///（白底描边）。写入与下发共用这一道关口。
pub(super) fn normalize_tone(raw: &str) -> String {
    if raw.trim() == "brand" {
        "brand".to_string()
    } else {
        "plain".to_string()
    }
}

/// `star_override`（格式「称号|人名|音浪」，1~3 段）里的第 2 段人名；没有则空串。
/// 迁移合成时 celebrate.name 为空就回落到这里（与 site() 的 star 解析同口径）。
pub(super) fn star_override_name(raw: &str) -> String {
    raw.trim()
        .splitn(3, '|')
        .nth(1)
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

/// 解析「称号|人名|音浪」三段式配置（champ_banner 与 star_override 同格式）：
/// 空串 → None；缺段按空串 / 0 兜底；音浪非数字或负数 → 0。
pub(super) fn parse_pipe3(raw: &str) -> Option<(String, String, i64)> {
    let v = raw.trim();
    if v.is_empty() {
        return None;
    }
    let parts: Vec<&str> = v.splitn(3, '|').collect();
    let title = parts.first().map(|s| s.trim()).unwrap_or("").to_string();
    let name = parts.get(1).map(|s| s.trim()).unwrap_or("").to_string();
    let wave = parts
        .get(2)
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0)
        .max(0);
    Some((title, name, wave))
}

/// 迁移合成（纯函数）：`items` 为空时按现有配置合成**恰好一条**，保住页面不空窗
/// 也不出现重复。优先级：
/// 1. `celebrate.enabled && (title 非空 || name 非空)` → brand + emoji_events，
///    title 空回落 config `star_title`，name 空回落 `star_override` 的第 2 段人名；
/// 2. 否则 config `champ_banner` 非空（「称号|人名|音浪」）→ plain + military_tech；
/// 3. 都没有 → None（items 保持空数组）。
/// 合成的条目 `enabled=true`，否则公开端拿不到、迁移等于没做。
pub(super) fn synth_item(
    celebrate: &CelebrateConfig,
    star_title: &str,
    star_override: &str,
    champ_banner: &str,
) -> Option<AnnounceItem> {
    let c_title = celebrate.title.trim();
    let c_name = celebrate.name.trim();
    if celebrate.enabled && (!c_title.is_empty() || !c_name.is_empty()) {
        let title = if c_title.is_empty() {
            star_title.trim().to_string()
        } else {
            c_title.to_string()
        };
        let name = if c_name.is_empty() {
            star_override_name(star_override)
        } else {
            c_name.to_string()
        };
        return Some(AnnounceItem {
            enabled: true,
            icon: "emoji_events".to_string(),
            title,
            name,
            wave: celebrate.wave.max(0),
            text: String::new(),
            tone: "brand".to_string(),
        });
    }
    let (title, name, wave) = parse_pipe3(champ_banner)?;
    // 「||」这类解析后全空的配置等于没内容，别下发一条空公告条
    if title.is_empty() && name.is_empty() {
        return None;
    }
    Some(AnnounceItem {
        enabled: true,
        icon: "military_tech".to_string(),
        title,
        name,
        wave,
        text: String::new(),
        tone: "plain".to_string(),
    })
}

/// `load_effective` 的纯函数内核：items 为空 → 合成一条；非空**原样返回**，
/// 绝不用迁移覆盖运营已经配好的内容。单测直接调它，不碰库。
pub(super) fn with_synth_items(
    mut a: Announcement,
    star_title: &str,
    star_override: &str,
    champ_banner: &str,
) -> Announcement {
    if a.items.is_empty() {
        let it = synth_item(&a.celebrate, star_title, star_override, champ_banner);
        if let Some(it) = it {
            a.items.push(it);
        }
    }
    a
}

/// POST body.items → `Vec<AnnounceItem>`（与 steps 同一道校验关口）：
/// 全空条目（title/name/text 都空）直接丢弃、icon 走 [`STEP_ICON_WHITELIST`]、
/// tone 归一为 brand|plain、wave 负数按 0；各字段做长度兜底。
pub(super) fn parse_items(arr: &[Value]) -> Vec<AnnounceItem> {
    arr.iter()
        .filter_map(|v| {
            let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("").trim();
            let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").trim();
            // text 允许换行；trim 只在字符层面兜底，不去掉中间换行
            let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("").trim();
            // 全空条目直接丢弃：写入即不留噪声（steps 同口径）
            if title.is_empty() && name.is_empty() && text.is_empty() {
                return None;
            }
            let icon = v.get("icon").and_then(|x| x.as_str()).unwrap_or("").trim();
            Some(AnnounceItem {
                enabled: v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(false),
                // 白名单外的图标名直接丢掉（存空串），任意字符串不进配置/DOM
                icon: if STEP_ICON_WHITELIST.contains(&icon) {
                    icon.chars().take(48).collect()
                } else {
                    String::new()
                },
                title: title.chars().take(64).collect(),
                name: name.chars().take(64).collect(),
                wave: v.get("wave").and_then(|x| x.as_i64()).unwrap_or(0).max(0),
                text: text.chars().take(500).collect(),
                tone: normalize_tone(v.get("tone").and_then(|x| x.as_str()).unwrap_or("")),
            })
        })
        .collect()
}

impl Announcement {
    pub async fn load(st: &AppState) -> Self {
        crate::store::config_get(st, "announcement")
            .await
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// load + 内存合成（迁移，2026-10-14）：`items` 为空时按 celebrate → champ_banner
    /// 合成恰好一条，**绝不写库**（[`with_synth_items`] 只改内存副本）。
    /// 三处下发点（site / public_announcements / get_announcement）统一走它，
    /// 管理员 GET 因此也能看到当前线上合成内容，保存即固化。
    pub async fn load_effective(st: &AppState) -> Self {
        let a = Self::load(st).await;
        if !a.items.is_empty() {
            return a;
        }
        let star_title = crate::store::config_get(st, "star_title")
            .await
            .unwrap_or_default();
        let star_override = crate::store::config_get(st, "star_override")
            .await
            .unwrap_or_default();
        let champ_banner = crate::store::config_get(st, "champ_banner")
            .await
            .unwrap_or_default();
        with_synth_items(a, &star_title, &star_override, &champ_banner)
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
            // 展示页公告条（2026-10-14）：管理端**全量**下发（含 enabled=false，供后台编辑）；
            // 公开前端的裁剪形态走 public_items_json()
            "items": self.all_items_json(),
        })
    }

    /// 单条公告条 → 下发 JSON（公开/管理端同口径）：icon trim 后不在白名单 → ""、
    /// tone 归一为 brand|plain、wave 负数 → 0，text 原样下发（支持换行）。
    pub(super) fn item_json(it: &AnnounceItem) -> Value {
        let icon = it.icon.trim();
        let icon = if STEP_ICON_WHITELIST.contains(&icon) { icon } else { "" };
        json!({
            "enabled": it.enabled,
            "icon": icon,
            "title": it.title,
            "name": it.name,
            "wave": it.wave.max(0),
            "text": it.text,
            "tone": normalize_tone(&it.tone),
        })
    }

    /// 公开下发形态：只发 `enabled=true` 的条目（enabled=false 的不下发），
    /// 顺序 = 配置数组顺序 = 展示端从上到下。
    pub(super) fn public_items_json(&self) -> Value {
        Value::Array(
            self.items
                .iter()
                .filter(|i| i.enabled)
                .map(Self::item_json)
                .collect(),
        )
    }

    /// 管理端全量形态：含 enabled=false，管理员能编辑到每一条。
    pub(super) fn all_items_json(&self) -> Value {
        Value::Array(self.items.iter().map(Self::item_json).collect())
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
/// 走 load_effective：items 为空时合成的那一条也要让管理员看到（当前线上内容），
/// 保存即固化进配置。
pub async fn get_announcement(State(st): State<ArcState>) -> ApiResult {
    Ok(Json(Announcement::load_effective(&st).await.to_json()))
}

/// POST /api/admin/announcement
/// {enabled?, steps?: [{title, content, icon?, target?}],
///  placements?: {login|home|my: {enabled?, text?, icon?}},   // 2026-10-13 公告位
///  celebrate?: {enabled?, confetti?, title?, name?, wave?},  // 2026-10-13 庆祝
///  items?: [{enabled?, icon?, title?, name?, wave?, text?, tone?}]}  // 2026-10-14 公告条
/// 部分更新语义：没传的字段/公告位保持原样（与 enabled/steps 同一套口径）；
/// 传了 items 就**整包替换**（顺序即展示顺序）。
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

    /* ---------------- 展示页公告条（2026-10-14）：整包替换 ----------------
    传了 items（数组，含空数组）就整体替换，数组顺序 = 展示顺序；没传保持原样
    （与 steps/placements 的部分更新语义一致）。校验同 steps 一道关口：
    全空条目丢弃、icon 走白名单、tone 归一为 brand|plain、wave 负数按 0。
    注意这里用 load（不是 load_effective）：迁移合成只在内存里发生，绝不因为
    一次无关的保存就把合成条目写进库里。 */
    if let Some(arr) = body.get("items").and_then(|v| v.as_array()) {
        a.items = parse_items(arr);
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
    // 响应与 admin GET 保持同一口径（load_effective）：没传 items 时也把内存合成的那条
    // 回给前端，避免「保存后界面突然空了」。合成只影响响应，绝不写库。
    let saved = Announcement::load_effective(&st).await;
    Ok(Json(json!({ "ok": true, "announcement": saved.to_json() })))
}

/* ==================== 公告条 items 单测（2026-10-14） ==================== */

#[cfg(test)]
mod announcement_items_tests {
    use super::*;
    use serde_json::json;

    fn item(title: &str, name: &str, tone: &str, enabled: bool) -> AnnounceItem {
        AnnounceItem {
            enabled,
            icon: String::new(),
            title: title.into(),
            name: name.into(),
            wave: 0,
            text: String::new(),
            tone: tone.into(),
        }
    }

    /// ① 空 items + celebrate 有值 → 合成**恰好一条** brand，且公开下发就一条（不空窗不重复）。
    #[test]
    fn empty_items_with_celebrate_synth_exactly_one_brand() {
        let a = Announcement {
            celebrate: CelebrateConfig {
                enabled: true,
                confetti: true,
                title: "初级争霸赛冠军".into(),
                name: "狼哲".into(),
                wave: 123456,
            },
            ..Default::default()
        };
        let eff = with_synth_items(a, "", "", "");
        assert_eq!(eff.items.len(), 1, "空 items 必须合成恰好一条");
        let it = &eff.items[0];
        assert!(it.enabled, "合成条目必须 enabled=true，否则公开端拿不到");
        assert_eq!(it.tone, "brand");
        assert_eq!(it.icon, "emoji_events");
        assert_eq!(it.title, "初级争霸赛冠军");
        assert_eq!(it.name, "狼哲");
        assert_eq!(it.wave, 123456);
        assert_eq!(it.text, "");
        assert_eq!(
            eff.public_items_json().as_array().unwrap().len(),
            1,
            "公开下发也只有一条"
        );
    }

    /// ①-fallback title/name 缺一边 → 回落 star_title / star_override 第 2 段；
    /// celebrate.enabled=false 时不算“有值”，不合成 brand。
    #[test]
    fn celebrate_synth_falls_back_to_star_config() {
        let a = Announcement {
            celebrate: CelebrateConfig {
                enabled: true,
                confetti: false,
                title: String::new(),
                name: "狼哲".into(),
                wave: 5,
            },
            ..Default::default()
        };
        let eff = with_synth_items(a, "每日之星", "称号|阿哲|999", "");
        assert_eq!(eff.items.len(), 1);
        assert_eq!(eff.items[0].title, "每日之星", "celebrate.title 空 → 回落 star_title");
        assert_eq!(eff.items[0].name, "狼哲");

        let a = Announcement {
            celebrate: CelebrateConfig {
                enabled: true,
                confetti: false,
                title: "初级争霸赛冠军".into(),
                name: String::new(),
                wave: 5,
            },
            ..Default::default()
        };
        let eff = with_synth_items(a, "每日之星", "称号|阿哲|999", "");
        assert_eq!(
            eff.items[0].name, "阿哲",
            "celebrate.name 空 → 回落 star_override 第 2 段人名"
        );

        let a = Announcement {
            celebrate: CelebrateConfig {
                enabled: false,
                confetti: false,
                title: "初级争霸赛冠军".into(),
                name: "狼哲".into(),
                wave: 5,
            },
            ..Default::default()
        };
        let eff = with_synth_items(a, "每日之星", "称号|阿哲|999", "");
        assert!(eff.items.is_empty(), "celebrate.enabled=false 不算有值");
    }

    /// ② 空 items + 只有 champ_banner → 合成恰好一条 plain；缺段 / 脏音浪 / 空串按兜底。
    #[test]
    fn empty_items_with_only_champ_banner_synth_exactly_one_plain() {
        let eff = with_synth_items(Announcement::default(), "", "", "初级争霸赛冠军|狼哲|123456");
        assert_eq!(eff.items.len(), 1);
        let it = &eff.items[0];
        assert!(it.enabled);
        assert_eq!(it.tone, "plain");
        assert_eq!(it.icon, "military_tech");
        assert_eq!(it.title, "初级争霸赛冠军");
        assert_eq!(it.name, "狼哲");
        assert_eq!(it.wave, 123456);

        // 两段式（无音浪）/ 非法音浪 → wave 0
        assert_eq!(
            with_synth_items(Announcement::default(), "", "", "冠军|狼哲").items[0].wave,
            0
        );
        assert_eq!(
            with_synth_items(Announcement::default(), "", "", "冠军|狼哲|abc").items[0].wave,
            0
        );
        // 都没配 → 保持空数组（不硬造一条空公告）
        assert!(with_synth_items(Announcement::default(), "", "", "   ")
            .items
            .is_empty());
        // 「||」解析后全空 = 没内容，也不下发空条
        assert!(with_synth_items(Announcement::default(), "", "", "||")
            .items
            .is_empty());
    }

    /// ③ items 非空时迁移不得覆盖/追加（运营配了几条就是几条）。
    #[test]
    fn non_empty_items_are_never_touched_by_migration() {
        let a = Announcement {
            items: vec![
                item("运营配的第一条", "", "plain", true),
                item("第二条", "乙", "brand", false),
            ],
            celebrate: CelebrateConfig {
                enabled: true,
                confetti: true,
                title: "初级争霸赛冠军".into(),
                name: "狼哲".into(),
                wave: 1,
            },
            ..Default::default()
        };
        let eff = with_synth_items(a, "每日之星", "称号|阿哲|999", "冠军|狼哲|123456");
        assert_eq!(eff.items.len(), 2, "items 非空时迁移不得插条目");
        assert_eq!(eff.items[0].title, "运营配的第一条");
        assert_eq!(eff.items[1].title, "第二条");
    }

    /// ④ icon 白名单：含空白合法名 trim 后保留，其余（含 XSS 串）一律清空；
    /// 存量脏数据在下发时也再挡一道。
    #[test]
    fn icon_outside_whitelist_is_cleared() {
        let items = parse_items(&[
            json!({"enabled": true, "icon": " military_tech ", "title": "ok"}),
            json!({"enabled": true, "icon": "<script>alert(1)</script>", "title": "x"}),
            json!({"enabled": true, "icon": "", "title": "空图标"}),
        ]);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].icon, "military_tech", "白名单名 trim 后保留");
        assert_eq!(items[1].icon, "");
        assert_eq!(items[2].icon, "");

        let dirty = AnnounceItem {
            icon: "<img onerror=1>".into(),
            title: "x".into(),
            tone: "brand".into(),
            ..Default::default()
        };
        assert_eq!(Announcement::item_json(&dirty)["icon"], "");
    }

    /// ⑤ tone 归一：只有 "brand" 原样，空 / 非法 / 大小写不符一律 "plain"。
    #[test]
    fn tone_is_normalized_to_brand_or_plain() {
        let items = parse_items(&[
            json!({"title": "a", "tone": "brand"}),
            json!({"title": "b", "tone": " BRAND "}),
            json!({"title": "c", "tone": ""}),
            json!({"title": "d", "tone": "gold"}),
            json!({"title": "e", "tone": "plain"}),
        ]);
        assert_eq!(
            items.iter().map(|i| i.tone.as_str()).collect::<Vec<_>>(),
            vec!["brand", "plain", "plain", "plain", "plain"]
        );
        assert_eq!(Announcement::item_json(&items[0])["tone"], "brand");
        assert_eq!(Announcement::item_json(&items[4])["tone"], "plain");
        // 存量脏 tone 在下发时也归一
        let dirty = AnnounceItem {
            title: "x".into(),
            tone: "紫金".into(),
            ..Default::default()
        };
        assert_eq!(Announcement::item_json(&dirty)["tone"], "plain");
    }

    /// ⑥ enabled=false 保存在配置里（管理端可编辑）但不进公开下发；顺序 = 配置顺序。
    #[test]
    fn disabled_items_stay_for_admin_but_are_not_downlinked_publicly() {
        let a = Announcement {
            items: vec![
                item("上线的", "甲", "brand", true),
                item("下线的", "乙", "plain", false),
                item("上线的第二条", "", "plain", true),
            ],
            ..Default::default()
        };
        let pub_items = a.public_items_json();
        let arr = pub_items.as_array().unwrap();
        assert_eq!(arr.len(), 2, "enabled=false 不下发");
        assert!(arr.iter().all(|i| i["enabled"] == true));
        assert_eq!(arr[0]["title"], "上线的");
        assert_eq!(arr[1]["title"], "上线的第二条", "顺序 = 配置顺序");

        let all = a.all_items_json();
        assert_eq!(all.as_array().unwrap().len(), 3, "管理端全量（含 false）");
        assert_eq!(
            a.to_json()["items"].as_array().unwrap().len(),
            3,
            "admin GET 的 items 是全量"
        );
    }

    /// 写入校验：全空条目丢弃、正文保留换行、wave 负数按 0。
    #[test]
    fn write_validation_drops_empty_items_and_clamps() {
        let items = parse_items(&[
            json!({"enabled": true, "title": "", "name": "", "text": ""}),
            json!({"enabled": true, "title": "  ", "name": " ", "text": "\n"}),
            json!({"enabled": true, "title": "留下的", "name": "", "text": ""}),
            json!({"enabled": true, "title": "", "name": "", "text": "纯正文条"}),
        ]);
        assert_eq!(items.len(), 2, "全空条目必须丢弃");
        assert_eq!(items[0].title, "留下的");
        assert_eq!(items[1].text, "纯正文条");

        assert_eq!(
            parse_items(&[json!({"title": "t", "text": "第一行\n第二行"})])[0].text,
            "第一行\n第二行",
            "正文换行原样保留"
        );
        assert_eq!(parse_items(&[json!({"title": "t", "wave": -5})])[0].wave, 0);
        assert_eq!(
            parse_items(&[json!({"title": "t", "wave": 88})])[0].wave,
            88
        );
    }

    /// 旧配置兼容：没有 items 键 → 空数组；item 少字段 → serde(default) 兜底。
    #[test]
    fn old_announcement_json_without_items_still_parses() {
        let ann: Announcement =
            serde_json::from_str(r#"{"enabled":true,"steps":[],"version":3}"#).unwrap();
        assert!(ann.items.is_empty());

        let ann: Announcement = serde_json::from_str(r#"{"items":[{"title":"x"}]}"#).unwrap();
        assert_eq!(ann.items.len(), 1);
        assert!(!ann.items[0].enabled);
        assert_eq!(ann.items[0].tone, "");
        assert_eq!(ann.items[0].wave, 0);

        // to_json → 反序列化 roundtrip 无损（save/load 走同一 serde 形态）
        let back: Announcement = serde_json::from_value(
            Announcement {
                items: vec![item("标题", "人名", "brand", true)],
                ..Default::default()
            }
            .to_json(),
        )
        .expect("items roundtrip 必须无损");
        assert_eq!(back.items.len(), 1);
        assert_eq!(back.items[0].title, "标题");
        assert_eq!(back.items[0].tone, "brand");
    }
}
