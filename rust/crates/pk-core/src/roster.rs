//! 接龙名单解析：行首第一个名字 = 参赛主播，其余 = 队长/师傅（只标注，不参赛）
//!
//! 例：`啸安➕狼兴` → 参赛 啸安、师傅 狼兴；`啸阳（狼泽）` → 参赛 啸阳、师傅 狼泽

use std::collections::HashMap;

use crate::types::ParsedRow;

pub type AliasMap = HashMap<String, String>;

#[derive(Clone, Debug, Default)]
pub struct ParseOptions {
    pub aliases: Option<AliasMap>,
    pub exclude: Vec<String>,
}

impl ParseOptions {
    pub fn with_defaults() -> Self {
        Self {
            aliases: Some(default_aliases()),
            exclude: default_exclude(),
        }
    }
}

fn is_ws(c: char) -> bool {
    c.is_whitespace() || c == '\u{3000}'
}

/// 分隔符：空白 / 全角加号 / emoji 加号 / 括号 / 各类横线 / 逗号顿号斜杠
fn is_separator(c: char) -> bool {
    is_ws(c)
        || matches!(
            c,
            '\u{FF0B}' // ＋
                | '+'
                | '\u{2795}' // ➕
                | '\u{FF08}' // （
                | '('
                | '\u{FF09}' // ）
                | ')'
                | '\u{2014}' // —
                | '\u{2013}' // –
                | '-'
                | '_'
                | '~'
                | ','
                | '\u{ff0c}' // ，
                | '\u{3001}' // 、
                | '/'
        )
}

/// 剥掉行首序号：`12. 名字` / `3) 名字` / `5、名字`
fn strip_leading_index(s: &str) -> &str {
    let s = s.trim_start_matches(is_ws);
    let digits = s.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return s;
    }
    let rest = s[digits..].trim_start_matches(is_ws);
    match rest.chars().next() {
        Some(c) if matches!(c, '.' | '\u{3001}' | ')' | '\u{ff0e}' | '\u{00b7}') => {
            rest[c.len_utf8()..].trim_start_matches(is_ws)
        }
        _ => s,
    }
}

/// 全角括号转半角 + 去空白，供别名/排除表比对
pub fn normalize_name(input: &str) -> String {
    input
        .chars()
        .filter(|c| !is_ws(*c))
        .map(|c| match c {
            '\u{FF08}' => '(',
            '\u{FF09}' => ')',
            other => other,
        })
        .collect()
}

pub fn apply_alias(name: &str, aliases: Option<&AliasMap>) -> (String, Option<String>) {
    let Some(aliases) = aliases else {
        return (name.to_string(), None);
    };
    if let Some(hit) = aliases
        .get(&normalize_name(name))
        .or_else(|| aliases.get(name))
    {
        return (hit.clone(), Some(hit.clone()));
    }
    (name.to_string(), None)
}

pub fn parse_roster(text: &str, options: &ParseOptions) -> Vec<ParsedRow> {
    let exclude_set: Vec<String> = options.exclude.iter().map(|x| normalize_name(x)).collect();
    let mut rows = Vec::new();

    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let raw = line.trim();
        if raw.is_empty() {
            continue;
        }
        let body = strip_leading_index(raw);
        if body.is_empty() {
            continue;
        }

        let tokens: Vec<&str> = body
            .split(is_separator)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect();
        let Some(first) = tokens.first() else {
            continue;
        };

        let first_norm = normalize_name(first);
        let rest: Vec<&str> = tokens
            .iter()
            .skip(1)
            .copied()
            .filter(|t| normalize_name(t) != first_norm)
            .collect();

        let (name, alias_applied) = apply_alias(first, options.aliases.as_ref());
        rows.push(ParsedRow {
            raw: raw.to_string(),
            excluded: exclude_set.contains(&normalize_name(&name)),
            name,
            leader: (!rest.is_empty()).then(|| rest.join(" ")),
            alias_applied,
        });
    }

    rows
}

pub fn default_aliases() -> AliasMap {
    [("阿楠".to_string(), "南方楠".to_string())]
        .into_iter()
        .collect()
}

pub fn default_exclude() -> Vec<String> {
    vec!["目".to_string()]
}

/// 内置的 55 人名单（接龙格式）
pub const DEFAULT_ROSTER: &str = "1. 目
2. 浩杰
3. 浩宁
4. 浩延
5. 浩鸣   李斌
6. 浩艺
7. 浩阳
8. 浩运
9. 浩龙
10. 玖玥
11. 狼兴
12. 凤九
13. 狼辉
14. 浩哲
15. 啸安➕狼兴
16. 狼耀
17. 浩玟
18. 狼征
19. 狼小宝
20. 啸恒
21. 狼澈
22. 啸阳（狼泽）
23. 啸辰
24. 狼途
25. 啸泽
26. 啸森——狼俊
27. 狼岳
28. 狼明+浩龙
29. 玖柒
30. 狼九
31. 狼哲
32. 浩冬
33. 狼影
34. 狼佑
35. 浩辰
36. 狼艺
37. 狼雨
38. 阿楠
39. 浩雨
40. 啸帆
41. 狼俊
42. 玖雪
43. 啸强（狼俊）
44. 狼赫+浩雨
45. 浩森
46.  啸宇 + 狼俊
47. 浩启
48. 狼泽
49. 啸弘 （狼泽）
50. 啸寻
51. 啸昭
52. 狼仔
53. 狼轩
54. 狼辰—浩杰
55. 浩月";

#[cfg(test)]
mod tests {
    use super::*;

    fn row_of(text: &str, name: &str) -> ParsedRow {
        parse_roster(text, &ParseOptions::with_defaults())
            .into_iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("没解析到 {name}"))
    }

    #[test]
    fn parses_default_roster() {
        let rows = parse_roster(DEFAULT_ROSTER, &ParseOptions::with_defaults());
        assert_eq!(rows.len(), 55);
        assert_eq!(rows.iter().filter(|r| r.excluded).count(), 1);
    }

    #[test]
    fn leader_suffix_is_not_a_player() {
        let row = row_of("15. 啸安➕狼兴", "啸安");
        assert_eq!(row.leader.as_deref(), Some("狼兴"));
        let row = row_of("22. 啸阳（狼泽）", "啸阳");
        assert_eq!(row.leader.as_deref(), Some("狼泽"));
        let row = row_of("26. 啸森——狼俊", "啸森");
        assert_eq!(row.leader.as_deref(), Some("狼俊"));
        let row = row_of("46.  啸宇 + 狼俊", "啸宇");
        assert_eq!(row.leader.as_deref(), Some("狼俊"));
    }

    #[test]
    fn leader_only_dropped_when_same_as_player() {
        // 狼明参赛，浩龙是师傅（名单里另有其人）
        assert_eq!(
            row_of("28. 狼明+浩龙", "狼明").leader.as_deref(),
            Some("浩龙")
        );
        // 第一个 token 重复出现时才算去掉
        let rows = parse_roster("啸安➕啸安", &ParseOptions::with_defaults());
        assert_eq!(rows[0].leader, None);
    }

    #[test]
    fn alias_and_exclude() {
        let row = row_of("38. 阿楠", "南方楠");
        assert_eq!(row.alias_applied.as_deref(), Some("南方楠"));
        assert!(!row.excluded);
        assert!(row_of("1. 目", "目").excluded);
    }

    #[test]
    fn strips_leading_index() {
        assert_eq!(strip_leading_index("10. 浩龙"), "浩龙");
        assert_eq!(strip_leading_index("3) 浩龙"), "浩龙");
        assert_eq!(strip_leading_index("5、浩龙"), "浩龙");
        assert_eq!(strip_leading_index("浩龙"), "浩龙");
        // 与 TS 版正则行为一致：数字 + 序号符一律剥掉
        assert_eq!(strip_leading_index("1.5 浩龙"), "5 浩龙");
    }

    #[test]
    fn normalizes_names() {
        assert_eq!(normalize_name("（阿楠）"), "(阿楠)");
        assert_eq!(normalize_name("  啸  安 "), "啸安");
    }
}
