//! public_teams::settings —— 自 public_teams.rs 按业务域拆分（对外语义不变）。

use super::*;

/* ------------------------------- 显隐开关 ------------------------------- */

/// 公开站点配置（2026-10-10 起存 config 表，cfg_key = public_settings；
/// 每个公开接口都要读它，进程内缓存 30s，save 时立即刷新缓存）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PublicSettings {
    #[serde(default = "default_true")]
    pub male_enabled: bool,
    #[serde(default = "default_true")]
    pub female_enabled: bool,
    #[serde(default = "default_true")]
    pub daily: bool,
    #[serde(default = "default_true")]
    pub monthly: bool,
    #[serde(default = "default_true")]
    pub yearly: bool,
    #[serde(default = "default_true")]
    pub duration: bool,
    #[serde(default = "default_true")]
    pub trend: bool,
    /// 每日报告的音浪数值（2026-10-09）：关掉后所有公开口径（日/月/年/个人）的
    /// wave / totalWave 一律置空，名次/名次变动照常。
    #[serde(default = "default_true")]
    pub wave: bool,
    /// 未播天数显隐（2026-10-19）：日/月/年榜的 absentDays 同受此开关控制。
    /// 老配置没有该键时按 true（保持历史行为）。
    #[serde(default = "default_true")]
    pub absent_days: bool,
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
    /// 预估收益口径（2026-10-19）：展示端「我的」页与「我的抖音号」把音浪折成人民币时用的
    /// **主播分成比例**（百分数，35 = 35%）。抖音的分成不是固定值（个人主播约 30%，公会
    /// 主播按公会等级 30%~55%，2026 年起又改成任务制弹性分成），所以这里可配，
    /// 不再在前端硬编码 35%。
    #[serde(default = "default_share_pct")]
    pub share_pct: i64,
    /// 预估收益口径（2026-10-19）：提现/平台手续费比例（百分数，0 = 不扣）。抖音侧实际扣费
    /// 不固定（提现单笔手续费、平台技术服务费、个税都可能是不同口径），这里只给一个可配的
    /// 估算比例，前端按「分成后 ×（1 - 手续费）」估到手。
    #[serde(default)]
    pub fee_pct: f64,
    /// 是否开启登录门（2026-10-10）：关 = 全面开放，打开就能看（但没有「我的」页）；
    /// 开 = 全站要登录（现状）
    #[serde(default = "default_true")]
    pub login_required: bool,
}

pub(super) fn default_true() -> bool {
    true
}

/// 分成比例缺省 35%（历史硬编码值；老配置没有该键时保持原行为）。
pub(crate) fn default_share_pct() -> i64 {
    35
}

/// 分成 / 手续费比例的合法区间（百分数，含端点）。越界一律按缺省值回落，
/// 绝不让一个负数或 300% 的脏配置把「预估到手」算成负数。
pub(crate) fn clamp_pct(v: i64, fallback: i64) -> i64 {
    if (0..=100).contains(&v) {
        v
    } else {
        fallback
    }
}

/// 税费 / 手续费比例（2026-10-20 支持小数）：夹回 0..=100，缺省回落。
pub(crate) fn clamp_fee_pct(v: f64, fallback: f64) -> f64 {
    if v.is_finite() && (0.0..=100.0).contains(&v) {
        (v * 1000.0).round() / 1000.0
    } else {
        fallback
    }
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

    /// 老配置没有 absent_days 键时按 true 迁移，不能因为新增字段把整份设置解析失败。
    #[test]
    pub(super) fn old_settings_default_absent_days_on() {
        let mut v = serde_json::to_value(PublicSettings::default()).unwrap();
        let obj = v.as_object_mut().unwrap();
        obj.remove("absent_days");
        obj.remove("duration");
        obj.remove("wave");
        let parsed: PublicSettings = serde_json::from_value(v).unwrap();
        assert!(
            parsed.absent_days && parsed.duration && parsed.wave,
            "老配置缺字段只按字段级默认 true，不能把整份设置回落成别的值"
        );
    }

    /// 管理端/展示端共用的 JSON 必须把三个字段开关都下发；关掉时必须明确为 false。
    #[test]
    pub(super) fn public_settings_json_has_metric_field_switches() {
        let mut s = PublicSettings::default();
        s.duration = false;
        s.wave = false;
        s.absent_days = false;
        let v = s.to_json();
        assert_eq!(v["duration"], false);
        assert_eq!(v["wave"], false);
        assert_eq!(v["absentDays"], false);
    }

    /// `wave=false` 的兜底清洗：每日之星/冠军横幅/公告条里的 wave 都要置空，
    /// 但不误伤非 wave 字段。
    #[test]
    pub(super) fn hide_wave_numbers_is_recursive_and_narrow() {
        let mut v = serde_json::json!({
            "star": {"name": "A", "wave": 123},
            "items": [{"title": "冠军", "wave": 456, "text": "keep"}],
            "waveEnabled": false
        });
        hide_wave_numbers(&mut v);
        assert!(v["star"]["wave"].is_null());
        assert!(v["items"][0]["wave"].is_null());
        assert_eq!(v["items"][0]["text"], "keep");
        assert_eq!(v["waveEnabled"], false);
    }

    /// 预估收益口径（2026-10-19）：老配置没有 sharePct/feePct 时按 35% / 0% 迁移，
    /// 保持「前端硬编码 35%」时代的旧行为，不因为新增字段把整份设置解析失败。
    #[test]
    pub(super) fn old_settings_default_share_and_fee() {
        let mut v = serde_json::to_value(PublicSettings::default()).unwrap();
        let obj = v.as_object_mut().unwrap();
        obj.remove("share_pct");
        obj.remove("fee_pct");
        let parsed: PublicSettings = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.share_pct, 35, "缺省仍是历史硬编码的 35%");
        assert_eq!(parsed.fee_pct, 0.0);
        assert_eq!(clamp_pct(parsed.share_pct, default_share_pct()), 35);
    }

    /// 脏配置（负数 / 超 100%）一律夹回缺省：不能让「预估到手」算成负数或放大数倍。
    #[test]
    pub(super) fn earnings_pct_clamps_dirty_values() {
        assert_eq!(clamp_pct(-5, 35), 35);
        assert_eq!(clamp_pct(300, 35), 35);
        assert_eq!(clamp_pct(101, 35), 35);
        assert_eq!(clamp_pct(0, 35), 0, "0% 是合法值（分成全归平台）");
        assert_eq!(clamp_pct(100, 35), 100);
        // 下发 JSON 里也必须是夹过的值，前端拿到的永远是合法百分数
        let mut s = PublicSettings::default();
        s.share_pct = 999;
        s.fee_pct = -3.0;
        let v = s.to_json();
        assert_eq!(v["sharePct"], 35);
        assert_eq!(v["feePct"], 0.0);
    }

    #[test]
    pub(super) fn period_enabled_follows_each_tab_switch() {
        let mut s = PublicSettings::default();
        assert!(s.period_enabled("daily"));
        assert!(s.period_enabled("monthly"));
        assert!(s.period_enabled("yearly"));
        s.daily = false;
        assert!(!s.period_enabled("daily"));
        s.monthly = false;
        assert!(!s.period_enabled("monthly"));
        s.yearly = false;
        assert!(!s.period_enabled("yearly"));
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
            absent_days: true,
            data_enabled: true,
            group_enabled: true,
            lineage_enabled: true,
            group_types: Default::default(),
            share_pct: default_share_pct(),
            fee_pct: 0.0,
            login_required: true,
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

/// 把任意公开 DTO 里的 `wave` 数值递归置空。用于站点头部的每日之星、冠军横幅、
/// 公告条/庆祝等「非榜单」数据：`PublicSettings.wave=false` 时它们同样不能成为
/// 绕过榜单闸门的数值出口。只动键名恰好为 `wave` 的值，不动 `wave: false` 这类
/// 开关本身（调用方只把数据对象传进来）。
pub(super) fn hide_wave_numbers(v: &mut Value) {
    match v {
        Value::Object(map) => {
            for (k, child) in map.iter_mut() {
                if k == "wave" {
                    *child = Value::Null;
                } else {
                    hide_wave_numbers(child);
                }
            }
        }
        Value::Array(rows) => {
            for child in rows {
                hide_wave_numbers(child);
            }
        }
        _ => {}
    }
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

    /// 某个公开周期页签是否开放；非法/空 period 按 daily 处理（与 lineage_window 同口径）。
    pub(super) fn period_enabled(&self, period: &str) -> bool {
        match period {
            "monthly" => self.monthly,
            "yearly" => self.yearly,
            _ => self.daily,
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
            "absentDays": self.absent_days,
            "dataEnabled": self.data_enabled,
            "groupEnabled": self.group_enabled,
            "lineageEnabled": self.lineage_enabled,
            "groupTypes": self.group_types,
            "sharePct": clamp_pct(self.share_pct, default_share_pct()),
            "feePct": self.fee_pct.clamp(0.0, 100.0),
            "loginRequired": self.login_required,
        })
    }
}

/* ------------------------------- 管理端 ------------------------------- */

/// GET /api/admin/public/settings
pub async fn get_settings(State(st): State<ArcState>) -> ApiResult {
    let mut v = PublicSettings::load(&st).await.to_json();
    // 🚩 流动小红旗条（2026-10-12）：称号 / 手动指定，存 config 表
    v["starTitle"] = json!(crate::store::config_get(&st, "star_title")
        .await
        .unwrap_or_default());
    v["starOverride"] = json!(crate::store::config_get(&st, "star_override")
        .await
        .unwrap_or_default());
    /* 🏆 争霸赛冠军横幅（2026-10-13）：整条文案可编（「称号|人名|音浪」），空 = 不显示 */
    v["champBanner"] = json!(crate::store::config_get(&st, "champ_banner")
        .await
        .unwrap_or_default());
    v["starIcon"] = json!(crate::store::config_get(&st, "star_icon")
        .await
        .unwrap_or_default());
    /* 推送图字段显隐（2026-10-17 用户口径）：日报 / 师门日报推送图的每一列是否显示，
    存 config `push_report_fields`（JSON；缺省/损坏 = 全部显示）。推送引擎 push.rs
    读同一份配置，前端只负责改。 */
    let fields = crate::report::PushReportFields::from_json(
        &crate::store::config_get(&st, "push_report_fields")
            .await
            .unwrap_or_default(),
    );
    v["pushReportFields"] = serde_json::to_value(&fields).unwrap_or_default();
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
    // 兼容两种请求写法：新管理端用 absentDays，旧脚本/手工请求可能用 snake_case。
    s.absent_days = flag("absentDays", flag("absent_days", s.absent_days));
    // 预估收益口径（2026-10-19）：传了才改，且强制夹到 0..=100（脏值回落缺省，
    // 不让负数 / 300% 这类脏配置把「预估到手」算成负数）。
    let pct = |key: &str, cur: i64, fallback: i64| -> i64 {
        clamp_pct(
            body.get(key).and_then(|v| v.as_i64()).unwrap_or(cur),
            fallback,
        )
    };
    s.share_pct = pct("sharePct", s.share_pct, default_share_pct());
    // 2026-10-20：税费 / 手续费允许两位小数（实测 0.624%），不能再用整数截断。
    s.fee_pct = body
        .get("feePct")
        .and_then(|v| v.as_f64())
        .filter(|v| *v >= 0.0 && *v <= 100.0)
        .map(|v| (v * 1000.0).round() / 1000.0)
        .unwrap_or(s.fee_pct);
    s.data_enabled = flag("dataEnabled", s.data_enabled);
    s.group_enabled = flag("groupEnabled", s.group_enabled);
    s.lineage_enabled = flag("lineageEnabled", s.lineage_enabled);
    s.login_required = flag("loginRequired", s.login_required);
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
    /* 推送图字段显隐（2026-10-17）：传了 `pushReportFields` 对象就整包覆盖，
    缺字段由 #[serde(default)] 回落「显示」（与旧数据兼容，不会因为少传一个
    键把运营没动过的列关掉）；传非对象 / 不传 = 保持原配置。 */
    if let Some(obj) = body.get("pushReportFields").filter(|v| v.is_object()) {
        let fields = crate::report::PushReportFields::from_json(&obj.to_string());
        if let Ok(json) = fields.to_json() {
            crate::store::config_set(&st, "push_report_fields", &json).await;
        }
    }
    s.save(&st).await;
    /* 图片字段 / 每日之星等设置变化后，旧榜单图缓存必须作废。 */
    crate::push::clear_daily_image_cache();
    crate::events::emit(crate::events::SCOPE_SETTINGS, "公开站点设置已更新");
    Ok(Json(json!({ "ok": true, "settings": s.to_json() })))
}

/* ------------------------------- 引导弹窗 ------------------------------- */

/// 引导弹窗（config 表 cfg_key = announcement）：多步骤引导，逐步「下一步」。
/// 展示端打开时弹一次，用户点「完成 / 跳过」后按 version 记住已读，
/// 直到下次保存（version 自增）再弹。
/// 2026-10-17 起展示端前端 st_page.html 的 GUIDE_ENABLED=false 总闸关闭引导，
/// 此配置当前不生效（结构保留供恢复：前端把 GUIDE_ENABLED 改回 true 即可）。
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
    /// 看板弹幕配置（2026-10-19）：位置与内容开关（serde default → 旧配置无此字段也安全）
    #[serde(default)]
    pub danmaku: DanmakuConfig,
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
    /// 弹窗标记（2026-10-15）：true = 该条公告除横幅外在展示端以弹窗形式弹出一次。
    /// 与横幅互不排斥（同一条两个位置都展示）；旧数据缺字段 serde(default) 回落
    /// false，纯横幅行为不变。布尔值无白名单可言，写入端只读 bool。
    #[serde(default)]
    pub popup: bool,
    /// 横幅对齐（2026-10-16 用户口径）：`"left"`（默认，原版式）/ `"center"` / `"right"`，
    /// 空 / 非法 / 大小写不符一律归一为 `"left"`（写入与下发共用 [`normalize_align`]）。
    /// 只影响展示端**横幅**（图标 + 文案整组靠中/靠右），公告弹窗不动。
    #[serde(default)]
    pub align: String,
    /// 三甲席位序号（2026-10-16 用户口径「直接选择主播 x 月成绩」）：0 = 冠军 / 1 = 亚军 /
    /// 2 = 季军；缺省 / 越界 = 未指定（弹窗按数组顺序兜底 0/1/2，横幅不受影响）。
    /// 管理端选席位后保存；展示端弹窗的徽章图按它走（同序号时以数组顺序兜底）。
    /// 默认 -1（不能用 serde(default) 的 i8 零值：0 是「冠军」，旧数据会被误标）。
    #[serde(default = "announce_seat_default")]
    pub seat: i8,
    /// 成绩月份（2026-10-16）：`YYYY-MM`，空 = 不指定。管理端「选主播 + 选月份」时
    /// 带出该月成绩自动填 wave；本字段只是记录口径，展示端仍只认 wave 数字。
    #[serde(default)]
    pub period: String,
    /// 抖音头像同源地址（2026-10-16 用户口径「头像使用抖音头像」）：选主播时由
    /// seat-options 带出（`/api/public/avatars/{pid}?v={hash}`）存进来，弹窗选手卡
    /// 直接渲染真头像（401/无图时前端回落首字色块）。白名单校验：只收本站
    /// `/api/public/avatars/` 开头的路径，任意 URL / 外域一律丢弃（零注入面）。
    #[serde(default)]
    pub avatar: String,
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

/// 看板弹幕（2026-10-19）：展示页「前三名分数」与「名次/主播」之间的横向飘字胶囊。
/// 只允许两种内容：每日冠军（榜单 rank1）与争霸赛冠军（公告条目）。
/// 2026-10-19 第三轮口径：弹幕位置改为**结构性锚定**（嵌在领奖台与榜单表头之间），
/// `top` 从「屏幕绝对位置」改为**微调偏移量**（-80..=80 px，正=下移，0=自然位）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DanmakuConfig {
    /// 总开关（false = 展示端完全不渲染弹幕条）
    #[serde(default)]
    pub enabled: bool,
    /// 位置微调（px，-80..=80）。0 = 前三名与「名次/主播」之间的自然位
    #[serde(default = "danmaku_top_default")]
    pub top: i64,
    /// 显示「每日冠军」（榜单第一名，👑 金胶囊）
    #[serde(default)]
    pub daily_champion: bool,
    /// 显示「争霸赛冠军」（公告条目第一条，🏆 金胶囊）
    #[serde(default)]
    pub contest_champion: bool,
}

pub(super) fn danmaku_top_default() -> i64 {
    0
}

impl DanmakuConfig {
    /// top 白名单：-80..=80 之外 clamp 回边界（防任意整数；超出会盖住分数或表头）
    pub fn normalized_top(&self) -> i64 {
        self.top.clamp(DANMAKU_TOP_MIN, DANMAKU_TOP_MAX)
    }
}

/// 弹幕位置微调边界（px）：负=上移贴分数，正=下移贴表头；再大就压内容了
pub(super) const DANMAKU_TOP_MIN: i64 = -80;
pub(super) const DANMAKU_TOP_MAX: i64 = 80;

impl Default for DanmakuConfig {
    /// enabled 默认 false（升级后不自动开，运营在后台显式打开）；
    /// daily/contest 默认 true（总开关一开就有内容，不必再逐项勾选）。
    fn default() -> Self {
        Self {
            enabled: false,
            top: danmaku_top_default(),
            daily_champion: true,
            contest_champion: true,
        }
    }
}

/// 看板弹幕（2026-10-19）的部分更新内核：`danmaku` 对象 → 逐字段合并进当前配置
/// （没传的字段保持原样）；top 越界 clamp 回 -80..=80（与 normalized_top 同一区间，
/// 语义是「相对弹幕自然位的微调偏移」）。抽成纯函数与 parse_items 同理由：
/// update_announcement 要 AppState，合并语义（clamp / 部分更新）单测直接调它，不起 server。
pub(super) fn merge_danmaku(cur: &mut DanmakuConfig, d: &serde_json::Map<String, Value>) {
    if let Some(v) = d.get("enabled").and_then(|v| v.as_bool()) {
        cur.enabled = v;
    }
    if let Some(v) = d.get("top").and_then(|v| v.as_i64()) {
        cur.top = v.clamp(DANMAKU_TOP_MIN, DANMAKU_TOP_MAX);
    }
    if let Some(v) = d.get("dailyChampion").and_then(|v| v.as_bool()) {
        cur.daily_champion = v;
    }
    if let Some(v) = d.get("contestChampion").and_then(|v| v.as_bool()) {
        cur.contest_champion = v;
    }
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
/// [`AnnounceItem::seat`] 的缺省值：-1 = 未指定（写函数是因为 serde(default) 对
/// i8 取类型零值 0，而 0 有「冠军」语义）。
fn announce_seat_default() -> i8 {
    -1
}

pub(super) fn normalize_tone(raw: &str) -> String {
    if raw.trim() == "brand" {
        "brand".to_string()
    } else {
        "plain".to_string()
    }
}

/// align 归一（2026-10-16）：只有 `"center"` / `"right"` 原样保留，其余（空 / 非法 /
/// 大小写不符 / `"left"`）一律 `"left"` —— 旧数据（没有该字段）天然是原版式左对齐。
/// 与 tone 同口径：写入与下发共用这一道关口，管理端/存量脏数据都挡得住。
pub(super) fn normalize_align(raw: &str) -> String {
    match raw.trim() {
        "center" => "center".to_string(),
        "right" => "right".to_string(),
        _ => "left".to_string(),
    }
}

/// `star_override`（格式「称号|人名|音浪」，1~3 段）里的第 2 段人名；没有则空串。
/// 迁移合成时 celebrate.name 为空就回落到这里（与 site() 的 star 解析同口径）。
pub(super) fn star_override_name(raw: &str) -> String {
    raw.trim()
        .split('|')
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
///
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
            // 迁移合成条目固定不带弹窗（2026-10-15）：弹窗只给运营显式配置的条目
            popup: false,
            // 迁移合成条目固定左对齐（2026-10-16）：与原版式一字不差
            align: "left".to_string(),
            // 合成条目不属于三甲配置：席位未指定（弹窗按数组顺序兜底）、无月份、无头像
            seat: -1,
            period: String::new(),
            avatar: String::new(),
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
        // 迁移合成条目固定不带弹窗（2026-10-15），与 celebrate 分支同口径
        popup: false,
        // 迁移合成条目固定左对齐（2026-10-16），与 celebrate 分支同口径
        align: "left".to_string(),
        // 合成条目不属于三甲配置：席位未指定（弹窗按数组顺序兜底）、无月份、无头像
        seat: -1,
        period: String::new(),
        avatar: String::new(),
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
                // 弹窗标记（2026-10-15）：布尔直读，缺省 false = 纯横幅（旧前端不传也一样）
                popup: v.get("popup").and_then(|x| x.as_bool()).unwrap_or(false),
                // 横幅对齐（2026-10-16）：白名单归一，缺省/非法 = left（原版式）
                align: normalize_align(v.get("align").and_then(|x| x.as_str()).unwrap_or("")),
                // 三甲席位（2026-10-16）：0/1/2 之外一律 -1（未指定），防任意整数进配置
                seat: {
                    let s = v.get("seat").and_then(|x| x.as_i64()).unwrap_or(-1);
                    if (0..=2).contains(&s) {
                        s as i8
                    } else {
                        -1
                    }
                },
                // 成绩月份（2026-10-16）：只收 YYYY-MM 形状（月必须两位，01-12 之外不拦——
                // 导入口径容错），其余丢弃（空 = 不指定）
                period: {
                    let p = v
                        .get("period")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .trim();
                    let digits_ok = |s: &str| s.chars().all(|c| c.is_ascii_digit());
                    let ok = p.len() == 7
                        && p.as_bytes()[4] == b'-'
                        && digits_ok(&p[..4])
                        && digits_ok(&p[5..]);
                    if ok {
                        p.to_string()
                    } else {
                        String::new()
                    }
                },
                // 抖音头像（2026-10-16）：只收本站同源头像路径 `/api/public/avatars/...`
                // （选人端点带出的形态），任意字符串 / 外域 URL / 相对跳转一律丢弃
                avatar: {
                    let a = v
                        .get("avatar")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .trim();
                    let ok = a.starts_with("/api/public/avatars/")
                        && !a.contains("..")
                        && a.chars()
                            .all(|c| c.is_ascii_alphanumeric() || "/?=&._-".contains(c));
                    if ok && a.len() <= 120 {
                        a.to_string()
                    } else {
                        String::new()
                    }
                },
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
        let mut a = Self::load(st).await;
        if a.items.is_empty() {
            let star_title = crate::store::config_get(st, "star_title")
                .await
                .unwrap_or_default();
            let star_override = crate::store::config_get(st, "star_override")
                .await
                .unwrap_or_default();
            let champ_banner = crate::store::config_get(st, "champ_banner")
                .await
                .unwrap_or_default();
            a = with_synth_items(a, &star_title, &star_override, &champ_banner);
        }
        // 公告条头像兜底（2026-10-17 用户报障「公告页面还是没有头像」）：
        // 存量条目的 `avatar` 是保存时冻结的字符串，早期保存的是空串。下发前按 name
        // 回查 person_avatar 补上（内存补、不写库；管理端 GET 也看到补好的，保存即固化）。
        // name 对不上 / 无本地留存图 → 保持空，前端照旧回落首字色块。
        fill_empty_avatars(st, &mut a.items).await;
        a
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
            // 看板弹幕（2026-10-19）：top 走 normalized_top（-80..=80 微调偏移）再直出，
            // 管理端全量与公开裁剪形态（site.rs 的 announcement_json 也走本函数）同一键
            "danmaku": {
                "enabled": self.danmaku.enabled,
                "top": self.danmaku.normalized_top(),
                "dailyChampion": self.danmaku.daily_champion,
                "contestChampion": self.danmaku.contest_champion,
            },
        })
    }

    /// 单条公告条 → 下发 JSON（公开/管理端同口径）：icon trim 后不在白名单 → ""、
    /// tone 归一为 brand|plain、wave 负数 → 0，text 原样下发（支持换行）。
    /// popup（2026-10-15）原样下发：管理端全量（all_items_json，含 enabled=false）
    /// 与公开过滤（public_items_json）都走本函数，改这一处即全覆盖
    /// （GET /api/public/announcements 与 GET /api/public/teams/{team} 都经它）。
    pub(super) fn item_json(it: &AnnounceItem) -> Value {
        let icon = it.icon.trim();
        let icon = if STEP_ICON_WHITELIST.contains(&icon) {
            icon
        } else {
            ""
        };
        json!({
            "enabled": it.enabled,
            "icon": icon,
            "title": it.title,
            "name": it.name,
            "wave": it.wave.max(0),
            "text": it.text,
            "tone": normalize_tone(&it.tone),
            // 横幅对齐（2026-10-16）：left（默认）/ center / right，非法值下发时也归一
            "align": normalize_align(&it.align),
            // 弹窗标记：true = 展示端除横幅外再弹窗一次；与横幅不互斥
            "popup": it.popup,
            // 三甲席位（2026-10-16）：0=冠军 / 1=亚军 / 2=季军 / -1=未指定；
            // 成绩月份 YYYY-MM（空 = 不指定）。管理端编辑回显用，公开端多两个字段无害。
            "seat": it.seat,
            "period": it.period,
            // 抖音头像同源地址（2026-10-16）：弹窗选手卡渲染真头像；空 = 首字色块
            "avatar": it.avatar,
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
        let icon = if STEP_ICON_WHITELIST.contains(&icon) {
            icon
        } else {
            ""
        };
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
///  items?: [{enabled?, icon?, title?, name?, wave?, text?, tone?, popup?}]   // 2026-10-14 公告条
///  danmaku?: {enabled?, top?, dailyChampion?, contestChampion?}}             // 2026-10-19 看板弹幕
///  // popup 为 2026-10-15 弹窗标记（bool，缺省 false，纯横幅不变）
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
                let target = s
                    .get("target")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let target_ok = !target.is_empty()
                    && target.len() <= 120
                    && target
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || " _-#.[\"]>:()~*=^|,$".contains(c));
                Some(AnnounceStep {
                    title: title.trim().chars().take(64).collect(),
                    // 白名单外的图标名直接丢掉（回落默认），任意字符串不进配置
                    icon: if STEP_ICON_WHITELIST.contains(&icon) {
                        icon.chars().take(48).collect()
                    } else {
                        String::new()
                    },
                    content: content.trim().chars().take(2000).collect(),
                    target: if target_ok {
                        target.to_string()
                    } else {
                        String::new()
                    },
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

    /* ---------------- 看板弹幕（2026-10-19）：部分更新，逐字段合并 ----------------
    没传 danmaku 整块保持原样；传了就逐字段合并（enabled/top/dailyChampion/
    contestChampion，缺哪个字段保哪个字段），与 celebrate 的部分更新语义一致。
    top 越界在 merge_danmaku 里 clamp 回 -80..=80（防任意整数进配置）。
    合并语义抽在纯函数 merge_danmaku，单测直接调它（见 announcement_items_tests）。 */
    if let Some(d) = body.get("danmaku").and_then(|v| v.as_object()) {
        merge_danmaku(&mut a.danmaku, d);
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

/* ---------------- 三甲席位选人（2026-10-16 用户口径「直接选择主播 x 月成绩」） ---------------- */

/// person_id → 头像 hash 表（seat_options 用）：只读 `person_avatar`（image 非空行），
/// 与 avatars.rs 的 404 语义同口径 —— 表缺失 / 查询失败一律空 map（头像缺失 =
/// 前端回落首字色块，绝不把榜单打挂）。
async fn avatar_hash_map(st: &AppState) -> std::collections::HashMap<u64, String> {
    let sql = "SELECT person_id, hash FROM person_avatar WHERE image IS NOT NULL";
    match crate::pgrs::query(&st.pools, "LOCAL", sql).await {
        Ok(rows) => rows
            .iter()
            .filter_map(|r| {
                let pid = r.try_get::<i64, _>("person_id").ok()?.max(0) as u64;
                let hash = r.try_get::<Option<String>, _>("hash").ok().flatten()?;
                Some((pid, hash))
            })
            .collect(),
        Err(_) => std::collections::HashMap::new(),
    }
}

/// 公告条头像兜底（2026-10-17 用户报障「公告页面还是没有头像」）。
///
/// 背景：公告条条目的 `avatar` 是**配置时冻结**的字符串。早期保存的条目（avatar 功能
/// 2026-10-16 才接入、或保存时选人没带出头像）落库即空串，展示端只能回落首字色块。
/// 这里在下发前按 `name` 回查 `person_avatar`（本地留存图，迁移 025）补上：
/// - 内存填充、**不写库**；管理端 GET 也能看到补好的值，管理员保存即固化；
/// - name 对不上 / 该人无本地留存图 → 保持空串，前端照旧回落首字色块（不破版）。
async fn fill_empty_avatars(st: &AppState, items: &mut [AnnounceItem]) {
    let any_missing = items
        .iter()
        .any(|i| i.avatar.trim().is_empty() && !i.name.trim().is_empty());
    if !any_missing {
        return;
    }
    // name → 同源头像地址。只取「有本地留存图」的真人；同名取其一（公告条按姓名认人）。
    let sql = "SELECT p.name AS name, p.id AS person_id, pa.hash AS hash \
               FROM person p JOIN person_avatar pa ON pa.person_id = p.id \
               WHERE pa.image IS NOT NULL AND p.deleted_at IS NULL";
    let Ok(rows) = crate::pgrs::query(&st.pools, "LOCAL", sql).await else {
        return;
    };
    let mut by_name: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for r in &rows {
        let (Ok(name), Ok(pid), Ok(hash)) = (
            r.try_get::<String, _>("name"),
            r.try_get::<i64, _>("person_id"),
            r.try_get::<Option<String>, _>("hash"),
        ) else {
            continue;
        };
        let Some(hash) = hash else { continue };
        if hash.trim().is_empty() {
            continue;
        }
        by_name
            .entry(name)
            .or_insert_with(|| super::site::avatar_url(pid.max(0) as u64, hash.trim()));
    }
    for it in items.iter_mut() {
        if it.avatar.trim().is_empty() {
            if let Some(url) = by_name.get(it.name.trim()) {
                it.avatar = url.clone();
            }
        }
    }
}

/// GET /api/admin/announcement/seat-options?period=YYYY-MM
///
/// 给公告页「席位配置」的选择器用：返回该月的主播成绩榜（personId/name/team/wave），
/// 运营直接选主播 + 选月份，成绩自动带出填进 wave，不用手抄数字。
///
/// 口径与展示端月榜完全一致：走 [`crate::dialect::query_period`]（monthly_wave
/// 导入的整月值优先，没有才 daily_metric 日明细求和，wave_reliable=1 过滤），
/// `ORDER BY wave DESC` —— 选中即所见，弹窗里显示的数字就是这个榜的数字。
#[derive(Deserialize)]
pub struct SeatOptionsQuery {
    /// 成绩月份 YYYY-MM；缺省 = 当月（本机时区，与 metrics::monthly 同口径）
    pub period: Option<String>,
}

pub async fn seat_options(
    State(st): State<ArcState>,
    Query(q): Query<SeatOptionsQuery>,
) -> ApiResult {
    let period = match q.period.as_deref() {
        Some(p) if !p.trim().is_empty() => p.trim().to_string(),
        _ => chrono::Local::now().format("%Y-%m").to_string(),
    };
    // 只收 YYYY-MM 形状（7 位、第 5 位是 '-'、其余是数字），注入面归零
    let ok = period.len() == 7
        && period.as_bytes()[4] == b'-'
        && period
            .chars()
            .enumerate()
            .all(|(i, c)| i == 4 || c.is_ascii_digit());
    if !ok {
        return Err(anyhow::anyhow!("period 格式应为 YYYY-MM").into());
    }
    /* 月榜同一份查询（ADMIN_PERIOD_SELECT：personId/name/gender/wave/minutes/...），
    这里只需要选人字段，全量列也无妨（同人一个月一行，行数 ≤ 主播数）。

    2026-10-17 修复（审计 2-P2-12）：`period` 是 **LIKE 通配窗口**，月榜
    （site.rs 的 /monthly）传的是 `format!("{period}%")`；这里原先传裸 period。
    YYYY-MM 恰好精确相等所以现在看不出差别，但一旦窗口放宽到年（`YYYY`）就会
    `LIKE '2026'` 命中 0 行 —— 与月榜口径分叉。补上 `%` 与月榜对齐。 */
    let rows = crate::dialect::query_period(
        &st.pools,
        "LOCAL",
        crate::dialect::ADMIN_PERIOD_SELECT,
        &format!("{period}%"),
        "",
        "",
        "wave DESC",
    )
    .await?;
    // 头像地址表（2026-10-16 用户口径「头像使用抖音头像」）：person_avatar 的内容
    // hash → 同源 `/api/public/avatars/{pid}?v={hash}`（site.rs::avatar_url 同一形态）。
    // 表不存在（迁移未跑）→ 空 map，avatar 一律 null，前端回落首字色块。
    let avatars: std::collections::HashMap<u64, String> = avatar_hash_map(&st)
        .await
        .into_iter()
        .map(|(pid, hash)| (pid, super::site::avatar_url(pid, &hash)))
        .collect();
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            let pid = r
                .try_get::<i64, _>("person_id")
                .map(|v| v.max(0) as u64)
                .unwrap_or(0);
            json!({
                "personId": pid,
                "name": r.try_get::<Option<String>, _>("name").ok().flatten().unwrap_or_default(),
                "team": r.try_get::<String, _>("gender").unwrap_or_default(),
                "wave": r
                    .try_get::<i64, _>("wave")
                    .map(|v| v.max(0) as u64)
                    .unwrap_or(0),
                "avatar": avatars.get(&pid),
            })
        })
        .collect();
    Ok(Json(json!({ "period": period, "items": items })))
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
            // 弹窗默认关（2026-10-15）：存量测试语义不变，popup 单独有用例覆盖
            popup: false,
            // 对齐默认居左（2026-10-16）：存量用例语义不变，align 单独有用例覆盖
            align: "left".to_string(),
            // 席位未指定 / 无月份 / 无头像（2026-10-16）：存量用例语义不变，有专测
            seat: -1,
            period: String::new(),
            avatar: String::new(),
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
        assert_eq!(
            eff.items[0].title, "每日之星",
            "celebrate.title 空 → 回落 star_title"
        );
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
        let eff = with_synth_items(
            Announcement::default(),
            "",
            "",
            "初级争霸赛冠军|狼哲|123456",
        );
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

    /// ⑤b align 归一（2026-10-16）：只有 center / right 原样，空 / 非法 / 大小写不符
    /// 一律 left —— 老库没有该字段 = 原版式左对齐，行为一字不变。
    #[test]
    fn align_is_normalized_to_left_center_or_right() {
        let items = parse_items(&[
            json!({"title": "a", "align": "center"}),
            json!({"title": "b", "align": " right "}),
            json!({"title": "c", "align": ""}),
            json!({"title": "d"}),
            json!({"title": "e", "align": "CENTER"}),
            json!({"title": "f", "align": "middle"}),
        ]);
        assert_eq!(
            items.iter().map(|i| i.align.as_str()).collect::<Vec<_>>(),
            vec!["center", "right", "left", "left", "left", "left"]
        );
        assert_eq!(Announcement::item_json(&items[0])["align"], "center");
        assert_eq!(Announcement::item_json(&items[1])["align"], "right");
        // 存量脏 align 在下发时也归一
        let dirty = AnnounceItem {
            title: "x".into(),
            align: "靠右".into(),
            ..Default::default()
        };
        assert_eq!(Announcement::item_json(&dirty)["align"], "left");
        assert_eq!(
            Announcement::item_json(&AnnounceItem::default())["align"],
            "left",
            "旧数据（没有该字段）= 原版式左对齐"
        );
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

    /// ⑦ popup 字段（2026-10-15）：旧 JSON 没有 popup 键 → serde(default) 回落 false，
    /// 下发（管理端全量与公开过滤同一 item_json）也输出 "popup":false，
    /// 存量纯横幅行为不变。
    #[test]
    fn old_json_without_popup_defaults_false_and_downlinks_false() {
        // 反序列化：缺 popup 键回落 false
        let ann: Announcement = serde_json::from_str(
            r#"{"items":[{"enabled":true,"title":"冠军","name":"狼哲","tone":"brand"}]}"#,
        )
        .expect("无 popup 字段的旧 JSON 必须能反序列化");
        assert_eq!(ann.items.len(), 1);
        assert!(!ann.items[0].popup, "旧数据缺 popup 字段必须回落 false");

        // 下发：to_json（管理端全量形态）与公开过滤形态都输出 "popup":false
        let a = Announcement {
            items: vec![item("冠军", "狼哲", "brand", true)],
            ..Default::default()
        };
        assert_eq!(a.to_json()["items"][0]["popup"], false);
        assert_eq!(a.public_items_json()[0]["popup"], false);
    }

    /// ⑧ popup 保存 roundtrip（2026-10-15）：保存入口 parse_items 读 bool，
    /// "popup":true 的条目 roundtrip 后保持 true，且公开下发（site /
    /// GET /api/public/announcements 共用的 public_items_json）可见。
    /// 语义：popup 与横幅互不排斥，同一条两个位置都展示。
    #[test]
    fn popup_true_survives_save_roundtrip_and_is_publicly_visible() {
        // 保存路径：update_announcement 的 items 整包替换走 parse_items
        let parsed = parse_items(&[json!({
            "enabled": true,
            "title": "夺冠快讯",
            "name": "狼哲",
            "tone": "brand",
            "popup": true
        })]);
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].popup, "保存带 popup:true 必须解析成 true");

        // serde roundtrip（save/load 同一形态）后保持 true
        let a = Announcement {
            items: parsed,
            ..Default::default()
        };
        let back: Announcement =
            serde_json::from_value(a.to_json()).expect("popup:true roundtrip 必须无损");
        assert!(back.items[0].popup, "roundtrip 后 popup 不得丢失");

        // 公开下发可见（enabled=true 的条目走同一 item_json）
        let pub_items = a.public_items_json();
        assert_eq!(pub_items.as_array().unwrap().len(), 1);
        assert_eq!(pub_items[0]["popup"], true, "公开下发必须带 popup:true");

        // 反向对照：不传 popup 的保存条目仍是 false（前端旧版不传不受影响）
        let plain = parse_items(&[json!({"enabled": true, "title": "纯横幅"})]);
        assert!(!plain[0].popup, "缺省 popup 必须 false");
        assert_eq!(
            Announcement {
                items: plain,
                ..Default::default()
            }
            .public_items_json()[0]["popup"],
            false
        );
    }

    /// 三甲席位 seat / period（2026-10-16 用户口径「直接选择主播 x 月成绩」）：
    /// ① 0/1/2 原样收、越界/负数/非数字归 -1；② period 只收 YYYY-MM 形状；
    /// ③ item_json 下发带 seat/period（管理端回显）；④ roundtrip 无损。
    #[test]
    fn seat_and_period_roundtrip() {
        // 合法席位 + 合法月份：原样收
        let parsed = parse_items(&[json!({
            "enabled": true, "title": "初级争霸赛冠军", "name": "狼哲",
            "wave": 285917, "popup": true, "seat": 0, "period": "2026-09"
        })]);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].seat, 0);
        assert_eq!(parsed[0].period, "2026-09");

        // 越界席位（3 / -5 / 字符串"0"）归 -1（未指定）；坏月份丢弃（「甲」的合法月份保留）
        let bad = parse_items(&[
            json!({"title": "甲", "seat": 3, "period": "2026-09"}),
            json!({"title": "乙", "seat": -5, "period": "2026-9"}),
            json!({"title": "丙", "seat": "0", "period": "202609"}),
        ]);
        assert!(bad.iter().all(|it| it.seat == -1));
        assert_eq!(bad[0].period, "2026-09"); // 合法月份不受坏席位牵连
        assert_eq!(bad[1].period, "");
        assert_eq!(bad[2].period, "");

        // 头像（2026-10-16）：只收本站同源头像路径；外域 / 任意串 / 路径穿越丢弃
        let av = parse_items(&[
            json!({"title": "正", "avatar": "/api/public/avatars/31?v=abc123"}),
            json!({"title": "外域", "avatar": "https://evil.example/x.png"}),
            json!({"title": "穿越", "avatar": "/api/public/avatars/../secret"}),
            json!({"title": "乱串", "avatar": "javascript:alert(1)"}),
        ]);
        assert_eq!(av[0].avatar, "/api/public/avatars/31?v=abc123");
        assert!(av[1].avatar.is_empty() && av[2].avatar.is_empty() && av[3].avatar.is_empty());

        // 下发带 seat / period / avatar；roundtrip 无损
        let a = Announcement {
            items: parsed,
            ..Default::default()
        };
        let js = a.to_json();
        assert_eq!(js["items"][0]["seat"], 0);
        assert_eq!(js["items"][0]["period"], "2026-09");
        let back: Announcement = serde_json::from_value(js).expect("seat roundtrip 必须无损");
        assert_eq!(back.items[0].seat, 0);
        assert_eq!(back.items[0].period, "2026-09");

        // avatar roundtrip（合法路径无损下发）
        let av_a = Announcement {
            items: av,
            ..Default::default()
        };
        let av_js = av_a.to_json();
        assert_eq!(
            av_js["items"][0]["avatar"],
            "/api/public/avatars/31?v=abc123"
        );
        assert_eq!(av_js["items"][1]["avatar"], "");

        // 旧数据没有这两个字段：serde(default) 回落 -1 / 空串，不炸
        let legacy: Announcement =
            serde_json::from_str(r#"{"items":[{"enabled":true,"title":"旧数据"}]}"#)
                .expect("旧数据必须能反序列化");
        assert_eq!(legacy.items[0].seat, -1);
        assert!(legacy.items[0].period.is_empty());
    }

    /* ---------------- 看板弹幕 danmaku 单测（2026-10-19） ---------------- */

    /// ① 默认值：enabled=false（升级后不自动开，运营在后台显式打开）/
    /// top=0（自然位：前三名分数与「名次/主播」之间）/ 两个内容开关 true。
    #[test]
    fn danmaku_defaults() {
        let d = DanmakuConfig::default();
        assert!(!d.enabled, "默认关闭：升级不得自动开弹幕");
        assert_eq!(d.top, 0, "默认偏移 0 = 自然位");
        assert!(d.daily_champion);
        assert!(d.contest_champion);
    }

    /// ② top 白名单：normalized_top 与保存端 merge_danmaku 同一 -80..=80 clamp
    /// （2026-10-19 第三轮：top 语义 = 相对「前三名与名次栏之间」自然位的微调偏移）。
    /// 保存路径等价于 POST danmaku {enabled:true, top:900} → GET 回 top==80。
    #[test]
    fn danmaku_top_is_clamped_on_both_sides() {
        // 下发端：normalized_top clamp
        let mut d = DanmakuConfig::default();
        d.top = 900;
        assert_eq!(d.normalized_top(), 80);
        d.top = -900;
        assert_eq!(d.normalized_top(), -80);
        d.top = -5;
        assert_eq!(d.normalized_top(), -5);
        d.top = 24;
        assert_eq!(d.normalized_top(), 24, "白名单内原样");

        // 保存端：merge_danmaku（update_announcement 的合并内核）同口径
        let mut cur = DanmakuConfig::default();
        merge_danmaku(
            &mut cur,
            &serde_json::from_str(r#"{"enabled": true, "top": 900}"#).unwrap(),
        );
        assert!(cur.enabled);
        assert_eq!(cur.top, 80, "保存端 top 越界 clamp 回 80");

        // 区间内原样收、越界 clamp
        let mut cur = DanmakuConfig::default();
        merge_danmaku(&mut cur, &serde_json::from_str(r#"{"top": -1}"#).unwrap());
        assert_eq!(cur.top, -1);
        merge_danmaku(&mut cur, &serde_json::from_str(r#"{"top": 390}"#).unwrap());
        assert_eq!(cur.top, 80);
    }

    /// ③ 部分更新语义：没传的字段保持原样；非 i64 的 top（浮点/字符串）不碰原值；
    /// 完全不传 danmaku（旧前端保存）整块原样。
    #[test]
    fn danmaku_partial_update_keeps_untouched_fields() {
        let base = DanmakuConfig {
            enabled: true,
            top: 30,
            daily_champion: false,
            contest_champion: true,
        };

        // 只传 contestChampion：其余三字段不动
        let mut cur = base.clone();
        merge_danmaku(
            &mut cur,
            &serde_json::from_str(r#"{"contestChampion": false}"#).unwrap(),
        );
        assert_eq!(
            (
                cur.enabled,
                cur.top,
                cur.daily_champion,
                cur.contest_champion
            ),
            (true, 30, false, false),
            "没传的字段必须保持原样"
        );

        // 非法 top 类型（浮点 / 字符串）= 没传，原值保留
        let mut cur = base.clone();
        merge_danmaku(&mut cur, &serde_json::from_str(r#"{"top": 39.5}"#).unwrap());
        assert_eq!(cur.top, 30);
        merge_danmaku(&mut cur, &serde_json::from_str(r#"{"top": "90"}"#).unwrap());
        assert_eq!(cur.top, 30);

        // update_announcement 不传 danmaku 键 → 整块原样（合并内核根本不被调用，
        // 这里钉的是「空对象合并 = 无操作」这一语义）
        let mut cur = base.clone();
        merge_danmaku(&mut cur, &serde_json::Map::new());
        assert_eq!(cur, base);
    }

    /// ④ 旧配置兼容：{"items":[]} 反序列化不报错，danmaku 整块回落默认值；
    /// serde（save/load 同一形态）roundtrip 无损。
    #[test]
    fn old_announcement_json_without_danmaku_defaults_and_roundtrips() {
        let ann: Announcement = serde_json::from_str(r#"{"items":[]}"#)
            .expect("旧配置（无 danmaku 字段）必须能反序列化");
        assert_eq!(ann.danmaku, DanmakuConfig::default());
        assert!(!ann.danmaku.enabled);
        assert_eq!(ann.danmaku.top, 0);
        assert!(ann.danmaku.daily_champion && ann.danmaku.contest_champion);

        // save/load 走 serde derive（snake_case）→ roundtrip 无损
        let a = Announcement {
            danmaku: DanmakuConfig {
                enabled: true,
                top: -40,
                daily_champion: true,
                contest_champion: false,
            },
            ..Default::default()
        };
        let back: Announcement = serde_json::from_str(&serde_json::to_string(&a).unwrap())
            .expect("danmaku 存取 roundtrip 必须无损");
        assert_eq!(back.danmaku, a.danmaku);
    }

    /// ⑤ 下发形态：to_json（管理端全量）与公开端共用的这个键输出 camelCase
    /// （enabled/top/dailyChampion/contestChampion），top 走 normalized_top。
    #[test]
    fn danmaku_downlink_json_shape() {
        let mut a = Announcement::default();
        a.danmaku.top = 9999; // 越界脏数据：下发时 clamp
        let d = &a.to_json()["danmaku"];
        assert_eq!(d["enabled"], false);
        assert_eq!(d["top"], 80, "下发端 top 也要过白名单");
        assert_eq!(d["dailyChampion"], true);
        assert_eq!(d["contestChampion"], true);
    }
}
