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

/// 半角或全角数字（微信里粘贴出来的序号常是全角 `１５．`）
fn is_digit(c: char) -> bool {
    c.is_ascii_digit() || ('\u{FF10}'..='\u{FF19}').contains(&c)
}

/// 整个 token 都是数字 → 它是「序号」，不是人名（没有主播叫纯数字）
fn is_index_token(s: &str) -> bool {
    !s.is_empty() && s.chars().all(is_digit)
}

/// 纯数字 token（允许千分位逗号与小数点）→ 名单自带的分数
fn parse_score(s: &str) -> Option<f64> {
    let cleaned: String = s
        .chars()
        .filter(|c| !matches!(c, ',' | '\u{ff0c}') && !is_ws(*c))
        .collect();
    if cleaned.is_empty() || !cleaned.chars().all(|c| is_digit(c) || c == '.') {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

/// 剥掉行首序号：`12. 名字` / `3) 名字` / `5、名字` / `１６．名字` / `16）名字`
///
/// 序号后面没跟「序号符」的（如 `15 浩龙`）这里不处理，交给 parse_roster
/// 的 token 级判断——因为 `名字 师傅`（用在册主播填充生成的格式）也长这样，
/// 只能靠「首个 token 是不是纯数字」来区分。
fn strip_leading_index(s: &str) -> &str {
    let s = s.trim_start_matches(is_ws);
    let end = s
        .char_indices()
        .find(|(_, c)| !is_digit(*c))
        .map_or(s.len(), |(i, _)| i);
    if end == 0 {
        return s;
    }
    let rest = s[end..].trim_start_matches(is_ws);
    match rest.chars().next() {
        Some(c)
            if matches!(
                c,
                '.' | '\u{3001}' | ')' | '\u{ff0e}' | '\u{00b7}' | '\u{ff09}'
            ) =>
        {
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

/// 数字里的千分位逗号先合掉（`71,259` → `71259`）。
/// 逗号本身是分隔符，不先合的话「狼哲 71,259」会被切成 `71` + `259`（分数变成 71）。
/// 名字里不会出现「数字,数字」，所以这个替换不会误伤人名。
fn merge_thousands(s: &str) -> String {
    let cs: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, c) in cs.iter().enumerate() {
        let prev_digit = i > 0 && cs[i - 1].is_ascii_digit();
        let next_digit = cs.get(i + 1).is_some_and(|n| n.is_ascii_digit());
        if matches!(c, ',' | '\u{ff0c}') && prev_digit && next_digit {
            continue;
        }
        out.push(*c);
    }
    out
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
        let stripped = strip_leading_index(raw);
        if stripped.is_empty() {
            continue;
        }
        let merged = merge_thousands(stripped);
        let body: &str = &merged;

        let tokens: Vec<&str> = body
            .split(is_separator)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect();
        // 首个 token 是纯数字 = 序号没跟「序号符」（`15 浩龙` / `16）玖柒`），
        // 跳过它再取人名；后面没人名时才不跳。`名字 师傅` 不受影响——人名不是纯数字。
        let start = usize::from(tokens.len() > 1 && is_index_token(tokens[0]));
        let Some(first) = tokens.get(start).copied() else {
            continue;
        };

        let first_norm = normalize_name(first);
        // 「名字 分数」格式（晋级赛名单自带分数，如 `1. 狼哲 71259`）：
        // 其余 token 里第一段纯数字就当分数，不参赛、也不算队长标注。
        // 人名不可能全是数字，所以这个判定不会误伤 `名字 师傅`。
        let mut score: Option<f64> = None;
        let mut rest: Vec<&str> = Vec::new();
        for t in tokens.iter().skip(start + 1).copied() {
            if normalize_name(t) == first_norm {
                continue;
            }
            if score.is_none() {
                if let Some(v) = parse_score(t) {
                    score = Some(v);
                    continue;
                }
            }
            rest.push(t);
        }

        let (name, alias_applied) = apply_alias(first, options.aliases.as_ref());
        rows.push(ParsedRow {
            raw: raw.to_string(),
            excluded: exclude_set.contains(&normalize_name(&name)),
            name,
            leader: (!rest.is_empty()).then(|| rest.join(" ")),
            alias_applied,
            score,
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

    /// 2026-10-07 用户给的真实名单格式：**前 4 行无序号**（新加入的人写在最上面），
    /// 后面 `1.`..`48.` 带序号但**缺 15、20**（两人退出）。
    /// 结论：序号只是视觉分隔 —— 有没有序号 / 跳号 / 号不对，都不该影响解析。
    const HEAD_THEN_GAPPED_NUMBERS: &str = "浩龙
玖柒
玖玥
狼岳
1. 浩月
2. 狼兴
3. 狼哲
4. 浩艺
5. 狼俊
6. 狼赫
7. 狼澈
8. 啸帆
9. 狼征
10. 浩杰
11. 浩雨
12. 浩玟
13. 啸泽
14. 浩宁
16. 狼佑
17. 狼艺
18. 浩森
19. 阿楠
21. 啸恒
22. 啸安
23. 玖雪
24. 啸辰
25. 狼辉
26. 啸森
27. 狼小宝
28. 啸强
29. 狼霆
30. 浩冬
31. 啸宇
32. 狼仔
33. 浩哲
34. 浩延
35. 狼轩
36. 浩泽
37. 浩运
38. 浩阳
39. 浩辰
40. 浩鸣
41. 浩沐
42. 玖豆
43. 狼九
44. 狼明
45. 玖安
46. 狼腾
47. 玖妹
48. 凤九";

    #[test]
    fn parses_unnumbered_head_then_gapped_numbering() {
        let rows = parse_roster(HEAD_THEN_GAPPED_NUMBERS, &ParseOptions::with_defaults());
        // 4 个无序号 + 1..48 里实有的 46 个 = 50 行，一行都不能丢
        assert_eq!(rows.len(), 50);
        // 无序号的行照样是参赛者（不会因为「没序号」被当成排版噪声丢掉）
        assert_eq!(
            rows[..4]
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            vec!["浩龙", "玖柒", "玖玥", "狼岳"]
        );
        // 序号跳号（缺 15、20）不影响解析顺序 = 文本先后顺序
        assert_eq!(rows[4].name, "浩月");
        assert_eq!(rows[50 - 1].name, "凤九");
        // 这份名单里没有「名字+队长」写法，其余 token 一律不该冒出来
        assert!(rows.iter().all(|r| r.leader.is_none()));
        assert!(rows.iter().all(|r| !r.excluded));
        // 别名（阿楠 → 南方楠）在混排格式里一样生效
        assert_eq!(
            rows.iter()
                .find(|r| r.raw.contains("19. 阿楠"))
                .unwrap()
                .name,
            "南方楠"
        );
    }

    /// 名单里手滑粘进一句说明（跟在最后一行名字后面）时，只会变成该行的「队长」标注，
    /// 不参赛、不影响人数 —— 界面上多一个无意义的队长标签，仅此而已。
    #[test]
    fn trailing_comment_becomes_leader_only() {
        let text = format!("{HEAD_THEN_GAPPED_NUMBERS} 支持解析名单这样解析");
        let rows = parse_roster(&text, &ParseOptions::with_defaults());
        assert_eq!(rows.len(), 50);
        assert_eq!(rows[50 - 1].name, "凤九");
        assert_eq!(rows[50 - 1].leader.as_deref(), Some("支持解析名单这样解析"));
    }

    #[test]
    fn unnumbered_lines_are_plain_players() {
        // 无序号的行 = 普通参赛者：既不是队长标注，也不会被当成排版噪声丢掉
        let rows = parse_roster(
            "浩龙\n玖柒\n玖玥\n狼岳\n1. 浩月",
            &ParseOptions::with_defaults(),
        );
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["浩龙", "玖柒", "玖玥", "狼岳", "浩月"]
        );
        assert!(rows.iter().all(|r| r.leader.is_none() && !r.excluded));
    }

    #[test]
    fn index_token_is_skipped_whatever_the_punctuation() {
        // 序号跟人名之间是空格 / 全角括号 / 全角数字+全角点，都算序号
        let rows = parse_roster(
            "15 浩龙\n16）玖柒\n１７．浩月\n18. 狼兴",
            &ParseOptions::with_defaults(),
        );
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["浩龙", "玖柒", "浩月", "狼兴"]
        );
        assert!(rows.iter().all(|r| r.leader.is_none()));
    }

    #[test]
    fn name_plus_master_survives_the_index_rule() {
        // 「名字 师傅」（用在册主播填充生成的格式）不能被序号规则吃掉
        let rows = parse_roster("浩龙 鹏先生\n狼佑 狼兴", &ParseOptions::with_defaults());
        assert_eq!(rows[0].name, "浩龙");
        assert_eq!(rows[0].leader.as_deref(), Some("鹏先生"));
        assert_eq!(rows[1].name, "狼佑");
        assert_eq!(rows[1].leader.as_deref(), Some("狼兴"));
    }

    #[test]
    fn name_plus_score_parsed_as_score_not_leader() {
        let rows = parse_roster(
            "1. 狼哲 71259\n浩龙 65156\n15. 啸安➕狼兴 12345\n5. 浩鸣   李斌\n玖豆 51,030",
            &ParseOptions::with_defaults(),
        );
        // 有分数：参赛者照旧是第一个名字，分数进 score、不当队长
        assert_eq!(rows[0].name, "狼哲");
        assert_eq!(rows[0].score, Some(71259.0));
        assert_eq!(rows[0].leader, None);
        // 无序号行同样支持
        assert_eq!(rows[1].name, "浩龙");
        assert_eq!(rows[1].score, Some(65156.0));
        // 师傅标注与分数可以共存（分数被摘出去，师傅还留着）
        assert_eq!(rows[2].name, "啸安");
        assert_eq!(rows[2].leader.as_deref(), Some("狼兴"));
        assert_eq!(rows[2].score, Some(12345.0));
        // 老格式（名字 + 师傅，无数字）不受影响
        assert_eq!(rows[3].name, "浩鸣");
        assert_eq!(rows[3].leader.as_deref(), Some("李斌"));
        assert_eq!(rows[3].score, None);
        // 千分位逗号：逗号是分隔符，先合掉再切，否则分数会变成 51
        assert_eq!(rows[4].name, "玖豆");
        assert_eq!(rows[4].score, Some(51030.0));
        assert_eq!(rows[4].leader, None);
    }

    #[test]
    fn default_roster_has_no_scores() {
        let rows = parse_roster(DEFAULT_ROSTER, &ParseOptions::with_defaults());
        assert!(rows.iter().all(|r| r.score.is_none()));
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
