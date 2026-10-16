//! 机器人命令注册表：命令名、别名、权限、一行说明的唯一事实来源。
//!
//! 路由（`bot::route_command`）的权限门控与帮助文案（[`help_for`]）都由这张表生成，
//! 加命令 / 改文案只动这一处；单元测试守住「注册表 ↔ 解析 ↔ 帮助文案」不漂移。
//!
//! 权限口径（2026-10-10 用户定稿）：
//! - [`Role::User`] 普通用户 = 微信私聊且已绑定推送的主播，只允许 5 条：
//!   帮助 / 每日报告 / 个人报告 / 暂停接收每日推送 / 开启接收每日推送
//! - [`Role::Admin`] 管理员 = QQ 全部会话 + 微信管理员会话，拥有全部命令

/// 角色：Admin 拥有全部命令；User 只有最小集合
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Admin,
}

impl Role {
    /// 当前角色能否执行「最低要求 required」的命令（Admin ⊇ User）
    pub fn allows(self, required: Role) -> bool {
        self == Role::Admin || required == Role::User
    }
}

/// 命令 ID（路由分发用的 match 键）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmdId {
    Help,
    DailyReport,
    MyReport,
    PushPause,
    PushResume,
    Status,
    Plan,
    TotalRank,
    Rank,
    Query,
    Export,
    DailySquadReport,
    MissingData,
    Import,
}

/// 命令注册项
pub struct CmdSpec {
    pub id: CmdId,
    /// 主命令名（帮助文案里展示的就是它）
    pub name: &'static str,
    /// 别名（英文为主）；匹配时忽略大小写
    pub aliases: &'static [&'static str],
    /// 执行所需的最低权限
    pub role: Role,
    /// 帮助里的一行说明
    pub summary: &'static str,
}

/// 全部命令（顺序 = 帮助文案里的顺序）
pub const COMMANDS: &[CmdSpec] = &[
    CmdSpec {
        id: CmdId::Help,
        name: "帮助",
        aliases: &["help"],
        role: Role::User,
        summary: "再看一遍这份说明",
    },
    CmdSpec {
        id: CmdId::DailyReport,
        name: "每日报告",
        aliases: &["daily"],
        role: Role::User,
        summary: "当天音浪榜 TOP10 + 我的名次",
    },
    CmdSpec {
        id: CmdId::MyReport,
        name: "个人报告",
        aliases: &["me", "my"],
        role: Role::User,
        summary: "我的今日 / 本月数据 + 追分目标进度",
    },
    CmdSpec {
        id: CmdId::PushPause,
        name: "暂停接收每日推送",
        aliases: &["暂停推送", "pause"],
        role: Role::User,
        summary: "停掉每日报告推送（追分提醒不受影响）",
    },
    CmdSpec {
        id: CmdId::PushResume,
        name: "开启接收每日推送",
        aliases: &["开启推送", "resume"],
        role: Role::User,
        summary: "恢复每日报告推送",
    },
    CmdSpec {
        id: CmdId::Status,
        name: "状态",
        aliases: &["status"],
        role: Role::Admin,
        summary: "数据新鲜度与机器人状态",
    },
    CmdSpec {
        id: CmdId::Plan,
        name: "分组",
        aliases: &["group"],
        role: Role::Admin,
        summary: "当前已上架分组方案",
    },
    CmdSpec {
        id: CmdId::TotalRank,
        name: "总榜",
        aliases: &[],
        role: Role::Admin,
        summary: "累计音浪榜",
    },
    CmdSpec {
        id: CmdId::Rank,
        name: "排行",
        aliases: &["rank"],
        role: Role::Admin,
        summary: "近 N 日音浪榜（用法：排行 10，默认 10，最多 100）",
    },
    CmdSpec {
        id: CmdId::Query,
        name: "查询",
        aliases: &["query"],
        role: Role::Admin,
        summary: "查某人数据（用法：查询 主播名）",
    },
    CmdSpec {
        id: CmdId::Export,
        name: "导出",
        aliases: &["export"],
        role: Role::Admin,
        summary: "CSV 文本回复（用法：导出 今日 / 导出 YYYY-MM-DD / 导出 总榜）",
    },
    CmdSpec {
        id: CmdId::DailySquadReport,
        name: "日报",
        aliases: &[],
        role: Role::Admin,
        summary: "文本版日报（用法：日报 [男团|女团] [日期]）",
    },
    CmdSpec {
        id: CmdId::MissingData,
        name: "缺数据",
        aliases: &[],
        role: Role::Admin,
        summary: "当天还没有数据的主播名单（用法：缺数据 [男团|女团] [日期]）",
    },
    CmdSpec {
        id: CmdId::Import,
        name: "导入",
        aliases: &["import"],
        role: Role::Admin,
        summary: "命令行导入数据（用法：导入 [男团|女团] [日期] + 数据行）",
    },
];

/// 8 个无参命令：支持「整句精确匹配」（忽略大小写），其余命令走 token 解析
const ARGLESS: &[CmdId] = &[
    CmdId::Help,
    CmdId::Status,
    CmdId::Plan,
    CmdId::TotalRank,
    CmdId::DailyReport,
    CmdId::MyReport,
    CmdId::PushPause,
    CmdId::PushResume,
];

impl CmdId {
    /// 执行所需的最低权限
    pub fn required_role(self) -> Role {
        self.spec().role
    }

    /// 注册表里的规格项（每个 CmdId 都必须登记在 [`COMMANDS`]）
    pub fn spec(self) -> &'static CmdSpec {
        COMMANDS
            .iter()
            .find(|s| s.id == self)
            .expect("CmdId 必须都注册在 COMMANDS 里")
    }
}

/// 候选文本是否命中这个命令（主名或任一别名，忽略大小写）
fn matches_spec(candidate: &str, spec: &CmdSpec) -> bool {
    spec.name.eq_ignore_ascii_case(candidate)
        || spec
            .aliases
            .iter()
            .any(|a| a.eq_ignore_ascii_case(candidate))
}

/// 解析一条命令文本 → (命令 ID, 参数)。
///
/// - 先整句精确匹配 8 个无参命令（帮助 / 状态 / 分组 / 总榜 / 每日报告 / 个人报告 /
///   暂停接收每日推送 / 开启接收每日推送，含英文别名，忽略大小写）
/// - 否则取第一个 token 匹配 name / aliases（忽略大小写），其余 token 作为参数
/// - 都不命中返回 None（路由层负责给未知命令提示）
pub fn parse(text: &str) -> Option<(CmdId, Vec<String>)> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    for spec in COMMANDS.iter().filter(|s| ARGLESS.contains(&s.id)) {
        if matches_spec(text, spec) {
            return Some((spec.id, Vec::new()));
        }
    }
    let mut toks = text.split_whitespace();
    let head = toks.next()?;
    let args: Vec<String> = toks.map(str::to_string).collect();
    COMMANDS
        .iter()
        .find(|s| matches_spec(head, s))
        .map(|s| (s.id, args))
}

/// 生成帮助文案：分「普通用户命令 / 管理员命令」两段。
/// - [`Role::User`] 只列 5 条普通用户命令
/// - [`Role::Admin`] = 全部命令 + 运营的日期黏住 / CSV 附件导入说明
///
/// 两版都带绑定 / 解绑说明。
pub fn help_for(role: Role) -> String {
    let mut out = String::from(match role {
        Role::User => "可用命令（普通用户）：",
        Role::Admin => "可用命令（管理员）：",
    });
    out.push_str("\n【普通用户命令】");
    for spec in COMMANDS.iter().filter(|s| s.role == Role::User) {
        out.push_str(&format!("\n· {} —— {}", spec.name, spec.summary));
    }
    if role == Role::Admin {
        out.push_str("\n【管理员命令】");
        for spec in COMMANDS.iter().filter(|s| s.role == Role::Admin) {
            out.push_str(&format!("\n· {} —— {}", spec.name, spec.summary));
        }
    }
    out.push_str(
        "\n\n对接 / 解绑：\n\
         直接发 4 位对接码 —— 展示端「我的」页点「获取二维码」后把数字发过来（30 分钟内有效）\n\
         解绑 —— 断开微信与账号的对接，随时可以重新对接",
    );
    if role == Role::Admin {
        // 运营口径：日期黏住 / CSV 附件导入 / 默认日期（原帮助文案的等价信息）
        out.push_str(
            "\n\n发「10.1」= 指定业务日期（之后发的 CSV 文件都按它入库，不用带团别）\n\
             发 CSV 文件 = 直接入库（不分男女团，团别按主播档案归属；同一天重复导，值相同不覆盖）\n\
             默认日期为前一天（数据晚一天）；同日重复导入，后发的覆盖先发的。",
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_IDS: &[CmdId] = &[
        CmdId::Help,
        CmdId::DailyReport,
        CmdId::MyReport,
        CmdId::PushPause,
        CmdId::PushResume,
        CmdId::Status,
        CmdId::Plan,
        CmdId::TotalRank,
        CmdId::Rank,
        CmdId::Query,
        CmdId::Export,
        CmdId::DailySquadReport,
        CmdId::MissingData,
        CmdId::Import,
    ];

    /// 帮助里的「· 命令名 —— …」行 → 命令名
    fn listed_names(role: Role) -> Vec<String> {
        help_for(role)
            .lines()
            .filter_map(|l| l.strip_prefix("· "))
            .map(|l| l.split_whitespace().next().unwrap_or_default().to_string())
            .collect()
    }

    /// 每个 CmdId 恰好登记一次，name / alias 全局无重复
    #[test]
    fn registry_covers_every_id_exactly_once() {
        assert_eq!(
            COMMANDS.len(),
            ALL_IDS.len(),
            "COMMANDS 与 CmdId 数量不一致"
        );
        for id in ALL_IDS {
            let n = COMMANDS.iter().filter(|s| s.id == *id).count();
            assert_eq!(n, 1, "{id:?} 应恰好登记一次（实际 {n} 次）");
        }
        let mut seen: Vec<String> = Vec::new();
        for s in COMMANDS {
            for key in std::iter::once(s.name).chain(s.aliases.iter().copied()) {
                let lower = key.to_lowercase();
                assert!(!seen.contains(&lower), "命令名/别名重复：{key}");
                seen.push(lower);
            }
        }
    }

    /// 主名与全部别名都能解析回同一个命令
    #[test]
    fn parse_covers_names_and_aliases() {
        for spec in COMMANDS {
            assert_eq!(
                parse(spec.name).map(|p| p.0),
                Some(spec.id),
                "主名解析失败：{}",
                spec.name
            );
            for alias in spec.aliases {
                assert_eq!(
                    parse(alias).map(|p| p.0),
                    Some(spec.id),
                    "别名解析失败：{alias}"
                );
            }
        }
    }

    #[test]
    fn parse_ignores_case_and_extra_spaces() {
        for s in ["HELP", "Help", "  help  "] {
            assert_eq!(parse(s).map(|p| p.0), Some(CmdId::Help), "{s}");
        }
        assert_eq!(parse("STATUS").map(|p| p.0), Some(CmdId::Status));
        assert_eq!(parse("Daily").map(|p| p.0), Some(CmdId::DailyReport));
    }

    #[test]
    fn parse_splits_args_after_first_token() {
        assert_eq!(parse("排行 5"), Some((CmdId::Rank, vec!["5".to_string()])));
        assert_eq!(parse("rank 20"), Some((CmdId::Rank, vec!["20".into()])));
        // 查询名带空格：剩余 token 原样给出
        assert_eq!(
            parse("查询 张三 高"),
            Some((CmdId::Query, vec!["张三".into(), "高".into()]))
        );
        assert_eq!(
            parse("导出 总榜"),
            Some((CmdId::Export, vec!["总榜".into()]))
        );
        // 无参命令整句匹配：带尾巴就不再是整句，走 token 解析，参数忽略即可
        assert_eq!(parse("帮助 我"), Some((CmdId::Help, vec!["我".into()])));
        assert_eq!(parse("暂停接收每日推送"), Some((CmdId::PushPause, vec![])));
        assert_eq!(parse("total").map(|p| p.0), None, "总榜没有英文别名");
    }

    #[test]
    fn parse_unknown_returns_none() {
        for s in [
            "",
            "   ",
            "blahblah",
            "今天天气不错",
            "10.1",
            "绑定 13800000000",
        ] {
            assert_eq!(parse(s), None, "不该命中命令：{s}");
        }
    }

    /// 普通用户帮助只列普通用户命令（不含「状态 / 排行 / 导入」等管理员命令名）
    #[test]
    fn help_user_lists_only_user_commands() {
        let text = help_for(Role::User);
        assert!(text.contains("可用命令"), "{text}");
        assert!(text.contains("【普通用户命令】"), "{text}");
        for admin_only in [
            "【管理员命令】",
            "状态",
            "排行",
            "导入",
            "总榜",
            "查询",
            "导出",
            "分组",
            "缺数据",
        ] {
            assert!(
                !text.contains(admin_only),
                "用户帮助不该出现 {admin_only}：{text}"
            );
        }
        let names = listed_names(Role::User);
        assert_eq!(names.len(), 5, "普通用户恰好 5 条命令：{names:?}");
        for name in &names {
            let (id, _) = parse(name).unwrap_or_else(|| panic!("帮助里的 {name} 解析不到"));
            assert_eq!(
                id.required_role(),
                Role::User,
                "{name} 不该出现在用户帮助里"
            );
        }
        // 绑定 / 解绑说明
        assert!(text.contains("对接码"), "{text}");
        assert!(text.contains("解绑"), "{text}");
    }

    /// 管理员帮助 = 全部命令（两段齐全），且保留运营的日期 / CSV 导入说明
    #[test]
    fn help_admin_lists_all_commands() {
        let text = help_for(Role::Admin);
        assert!(text.contains("可用命令"), "{text}");
        assert!(
            text.contains("【普通用户命令】") && text.contains("【管理员命令】"),
            "{text}"
        );
        let names = listed_names(Role::Admin);
        assert_eq!(
            names.len(),
            COMMANDS.len(),
            "管理员帮助要列出全部命令：{names:?}"
        );
        for spec in COMMANDS {
            assert!(
                names.contains(&spec.name.to_string()),
                "缺命令：{}",
                spec.name
            );
        }
        for key in [
            "数据新鲜度",
            "总榜",
            "导出",
            "缺数据",
            "发「10.1」",
            "发 CSV 文件",
            "默认日期为前一天",
        ] {
            assert!(text.contains(key), "管理员帮助缺关键信息 {key}：{text}");
        }
        assert!(text.contains("对接码"), "{text}");
    }

    /// 防漂移：帮助文案里出现的每个命令名都能被 parse 命中
    #[test]
    fn help_names_are_parseable() {
        for role in [Role::User, Role::Admin] {
            for name in listed_names(role) {
                assert!(
                    parse(&name).is_some(),
                    "{role:?} 帮助里的 {name} 解析不到（注册表漂移）"
                );
            }
        }
    }
}
