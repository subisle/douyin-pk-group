//! public_teams::pages —— 自 public_teams.rs 按业务域拆分（对外语义不变）。

use super::*;

/* --------------------------- 公开展示页 --------------------------- */

pub(super) const ST_PAGE: &str = include_str!("st_page.html");

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

pub(super) fn st_page_for(team: &str) -> Response {
    let html = ST_PAGE.replace("{{TEAM}}", team);
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

/* 2026-10-14 注册功能移除：`register_route_tests`（注册页团别白名单用例）随之删除，
   它唯一断言的 `normalize_team` 也已在 my.rs 里一并清理。 */

#[cfg(test)]
mod announcement_tests {
    use super::*;

    /// 图标白名单：合法名原样保留，白名单外 / 空值一律回落成按步序号的默认。
    /// 这一步是防「任意字符串进配置 → 前端拼进 class」的唯一关口，必须守住。
    #[test]
    pub(super) fn icon_for_whitelists_and_falls_back() {
        // 白名单内：原样
        let s = AnnounceStep {
            title: "x".into(),
            icon: "star".into(),
            target: String::new(),
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
                target: String::new(),
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
            target: String::new(),
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
    pub(super) fn default_icons_are_unique_and_whitelisted() {
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
    pub(super) fn to_json_always_emits_a_usable_icon() {
        // 公告位/庆祝字段（2026-10-13）与 icon 测试无关，走 Default::default()
        // （全关 / 全空），保持原断言语义不变。
        let a = Announcement {
            enabled: true,
            steps: vec![
                AnnounceStep {
                    title: "a".into(),
                    icon: String::new(),
                    target: String::new(),
            content: "1".into(),
                },
                AnnounceStep {
                    title: "b".into(),
                    icon: "  ".into(),
                    target: String::new(),
            content: "2".into(),
                },
            ],
            version: 1,
            ..Default::default()
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

    /* ---------------------- 公告系统 v2（2026-10-13） ---------------------- */

    /// 旧数据反序列化兼容：库里存的 cfg_key='announcement' 没有 placements /
    /// celebrate 字段，serde(default) 必须兜底成「全关」，登录页/看板/个人页
    /// 不出公告条、登录后不弹彩带（与 v1 行为一致）。
    #[test]
    pub(super) fn announcement_v2_old_json_defaults_all_off() {
        let ann: Announcement = serde_json::from_str(
            r#"{"enabled":true,"steps":[{"title":"a","content":"b"}],"version":3}"#,
        )
        .expect("旧 JSON 必须能反序列化");
        assert!(ann.enabled);
        assert_eq!(ann.version, 3);
        assert!(!ann.placements.login.enabled);
        assert!(!ann.placements.home.enabled);
        assert!(!ann.placements.my.enabled);
        assert!(ann.placements.login.text.is_empty());
        assert!(!ann.celebrate.enabled);
        assert!(!ann.celebrate.confetti);
        assert_eq!(ann.celebrate.wave, 0);
    }

    /// to_json 全量下发新字段（管理端编辑用），且 placements/celebrate 保存
    /// 取值后 roundtrip 不丢。
    #[test]
    pub(super) fn to_json_emits_placements_and_celebrate() {
        let ann = Announcement {
            enabled: false,
            placements: AnnouncementPlacements {
                login: Placement {
                    enabled: true,
                    text: "第一行\n第二行".into(),
                    icon: " campaign ".into(),
                },
                home: Placement { enabled: false, text: "看板".into(), icon: String::new() },
                my: Placement::default(),
            },
            celebrate: CelebrateConfig {
                enabled: true,
                confetti: true,
                title: String::new(), // 空 = 前端回退 star_title
                name: "狼哲".into(),
                wave: 88888,
            },
            ..Default::default()
        };
        let v = ann.to_json();
        let login = &v["placements"]["login"];
        assert_eq!(login["enabled"], true);
        // text 支持换行，原样下发
        assert_eq!(login["text"], "第一行\n第二行");
        // icon trim 后命中白名单才下发
        assert_eq!(login["icon"], "campaign");
        assert_eq!(v["placements"]["home"]["text"], "看板");
        assert_eq!(v["placements"]["my"]["enabled"], false);
        let c = &v["celebrate"];
        assert_eq!(c["enabled"], true);
        assert_eq!(c["confetti"], true);
        assert_eq!(c["name"], "狼哲");
        assert_eq!(c["wave"], 88888);
        // roundtrip：to_json 再反序列化，字段不丢（save/load 走同一 serde 形态）
        let back: Announcement = serde_json::from_value(v).expect("roundtrip 必须无损");
        assert_eq!(back.placements.login.text, "第一行\n第二行");
        assert_eq!(back.celebrate.name, "狼哲");
        assert_eq!(back.celebrate.wave, 88888);
    }

    /// 公告位 icon 白名单：合法名（含首尾空白）原样/trim 后下发，白名单外一律空串，
    /// 任意字符串不进前端 DOM（与引导弹窗 icon_for 同一道关口）。
    #[test]
    pub(super) fn placement_json_whitelists_icon() {
        let ok = Placement { enabled: true, text: String::new(), icon: " star ".into() };
        assert_eq!(Announcement::placement_json(&ok)["icon"], "star");
        for bad in ["<script>", "foo", "STAR", "star twist", ""] {
            let p = Placement { enabled: true, text: String::new(), icon: bad.into() };
            assert_eq!(
                Announcement::placement_json(&p)["icon"],
                "",
                "{bad:?} 白名单外必须为空"
            );
        }
    }

    /// 登录页公开端点的裁剪形态：celebrate_public_json 只含庆祝五字段，
    /// 不夹带 steps/version 等引导弹窗数据（登录前少出网）。
    #[test]
    pub(super) fn celebrate_public_json_shape() {
        let ann = Announcement {
            celebrate: CelebrateConfig {
                enabled: true,
                confetti: false,
                title: String::new(),
                name: "狼哲".into(),
                wave: 0,
            },
            ..Default::default()
        };
        let v = ann.celebrate_public_json();
        let obj = v.as_object().unwrap();
        let mut keys: Vec<_> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["confetti", "enabled", "name", "title", "wave"]);
        assert_eq!(v["name"], "狼哲");
        assert_eq!(v["wave"], 0);
    }
}

#[cfg(test)]
mod my_accounts_tests {
    use super::*;

    /// 三个窗口都由**业务日**推出：展示端数据是每晚补前一天的，
    /// 用自然日会让「今天」的每个号都显示 0（/my 踩过同样的坑）
    #[test]
    pub(super) fn periods_follow_business_date() {
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
    pub(super) fn periods_of_empty_date_are_empty() {
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
    pub(super) fn accounts_sql_keeps_one_source_per_account() {
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
        // 日期窗口一律 TO_CHAR，不按自然日
        assert!(sql.contains("TO_CHAR(d.biz_date, 'YYYY-MM-DD')"));
        assert!(sql.contains("TO_CHAR(ws.biz_date, 'YYYY-MM-DD')"));
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
    pub(super) fn gate_all_on_passes_numbers_and_sums_them() {
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
    pub(super) fn gate_nulls_disabled_periods() {
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
    pub(super) fn gate_wave_off_also_hides_daily() {
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
    pub(super) fn gate_empty_does_not_panic() {
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
    pub(super) fn gate_missing_wave_fields_counts_as_zero() {
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
    pub(super) fn lineage_window_falls_back() {
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
    pub(super) fn lineage_wave_window_sql_period_and_fallbacks() {
        // daily：精确业务日
        let daily = lineage_wave_window_sql("daily", "2026-10-09");
        assert!(daily.contains("WHERE wave_reliable = 1"), "{daily}");
        assert!(
            daily.contains("TO_CHAR(biz_date, 'YYYY-MM-DD') = '2026-10-09'"),
            "{daily}"
        );

        // monthly：该业务日所在月（业务日口径，不用自然日）
        let monthly = lineage_wave_window_sql("monthly", "2026-10-09");
        assert!(
            monthly.contains("TO_CHAR(biz_date, 'YYYY-MM') = '2026-10'"),
            "{monthly}"
        );
        assert!(!monthly.contains("'2026-10-09'"), "{monthly}");

        // yearly：该业务日所在年
        let yearly = lineage_wave_window_sql("yearly", "2026-10-09");
        assert!(
            yearly.contains("TO_CHAR(biz_date, 'YYYY') = '2026'"),
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
                !sql.contains("TO_CHAR(biz_date, 'YYYY-MM-DD') ="),
                "非法 date 不许进窗口条件：{sql}"
            );
        }
        // monthly/yearly 也一样：非法 date 退回空库口径
        assert!(!lineage_wave_window_sql("monthly", "DROP TABLE x").contains("DROP"));
        assert!(!lineage_wave_window_sql("yearly", "DROP TABLE x").contains("DROP"));
    }
}
