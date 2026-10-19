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

/// 艺名抽取：从抖音昵称里认出台账艺名（2026-10-18 用户口径）。
///
/// ## 为什么需要
///
/// 运营 CSV 的「主播昵称」是抖音上的展示名，跟台账艺名不是一回事：
///
/// | 昵称 | 艺名 |
/// |---|---|
/// | `狼霆🎹` | 狼霆 |
/// | `浩艺🎙️` | 浩艺（抖音号是 `haoyi6725`，库里根本没有这个号） |
/// | `十一宝《才艺🎹薇姐》小号` | 薇姐 |
/// | `三宝🎹才艺（每天晚上八点开播）` | 三宝 |
/// | `玖玉🎹🎤(尊师狼轩)` | 玖玉（括号里的师傅名不算） |
/// | `用户5567837253729` / `账号已注销` | 认不出 → None |
///
/// 同时有的人换了号 / 开了小号，库里 `account` 只有旧号，新号的行按号码匹配会掉进
/// 暂存区。艺名兜底让这些行也能归到正确的人身上（用户口径：从左往右第一个整词命中算数）。
///
/// ## 「不能搞错」的四条硬闸门
///
/// 1. **候选只能来自台账**（`roster` 参数）。昵称里出现但台账没有的（`八宝` / `婷宝` /
///    `狼月` / `狼烁`）一律不认 —— 不做「像艺名就认」的启发式。
/// 2. **整词命中，禁止子串**。`狼凯🎹（南方楠）` 里不会拆出「狼凯」（台账没有），
///    `狼明浩雨` 里不会拆出「明浩」。
/// 3. **括号内容整体丢弃**。`玖玉🎹🎤(尊师狼轩)` 里的「狼轩」是师傅，不是本人；
///    `浩坤🎹🎤《鹏先生》` 里的「鹏先生」同理 —— 这类行按「从左往右」取到的是括号前的
///    「浩坤」，正是本人。
/// 4. **不做模糊匹配**（无编辑距离 / 无拼音）。`晓明` 与 `晓鸣`、`狼宸`(已离团) 与
///    `狼辰`(在职) 只差一个字，模糊必错。代价是 `狼月` / `狼烁` 这类真·外团号认不出，
///    但它们本来就该留在暂存区由运营处理。
///
/// ## 返回值
///
/// `Some(艺名)` = 命中；`None` = 认不出，**调用方必须当作「未匹配」处理，不得猜**。
pub fn extract_stage_name(nickname: &str, roster: &[String]) -> Option<String> {
    let washed = wash_nickname(nickname);
    let tight: String = washed.chars().filter(|c| !c.is_whitespace()).collect();
    // 先按「带空格的原始洗后串」找（空格天然分隔「十一宝 小号」这类），
    // 再退到去空格的紧凑串（`啸  𝄞  燃` 洗完是 `啸   燃`，去空格才连成 `啸燃`）。
    for text in [washed.as_str(), tight.as_str()] {
        // strict：命中两侧都不能紧贴汉字；relax：只要求左侧不紧贴。
        // relax 是兜底（`薇姐唢呐下午5点30分直播` → 薇姐），必须放在 strict 之后，
        // 否则 `浩坤🎹🎤《鹏先生》` 这类行会被右侧规则提前放行，跑到错误的分支。
        if let Some(hit) = scan_stage_name(text, roster, false) {
            return Some(hit);
        }
        if let Some(hit) = scan_stage_name(text, roster, true) {
            return Some(hit);
        }
    }
    None
}

/// 装饰字符：emoji / 乐器符号 / 变体选择符 / 零宽连接符。
/// 洗掉它们是为了让「玖雪 🎹」「啸  𝄞  燃 🎹 🎤」里的艺名能连成整词。
fn is_decor(c: char) -> bool {
    matches!(c,
        '\u{1F000}'..='\u{1FAFF}'   // emoji 主区
        | '\u{2600}'..='\u{27BF}'   // 杂项符号 + 装饰符号（🎹 等）
        | '\u{2B00}'..='\u{2BFF}'
        | '\u{2190}'..='\u{21FF}'   // 箭头
        | '\u{2300}'..='\u{23FF}'   // 杂项技术符号
        | '\u{1D100}'..='\u{1D1FF}' // 音乐符号（𝄞）
        | '\u{FE0F}'                // 变体选择符-16
        | '\u{200D}'                // 零宽连接符
        | '\u{FEFF}'
    )
}

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
}

fn is_bracket_open(c: char) -> bool {
    matches!(c, '（' | '(' | '【' | '[' | '《' | '〈' | '「')
}

fn is_bracket_close(c: char) -> bool {
    matches!(c, '）' | ')' | '】' | ']' | '》' | '〉' | '」')
}

/// 洗昵称：括号（含内容）→ 空格，装饰字符 → 空格，其余原样保留。
///
/// 括号**连内容一起丢弃**而不是只删括号本身：`（尊师狼轩）` 里剩一个「狼轩」
/// 会被当成艺名命中，把这一行认成别人。生产里 `玖玉🎹🎤(尊师狼轩)`、`狼凯🎹（南方楠）`、
/// `浩坤🎹🎤《鹏先生》` 都是这个坑。
fn wash_nickname(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut depth = 0usize;
    for c in s.chars() {
        if is_bracket_open(c) {
            depth += 1;
            out.push(' ');
        } else if is_bracket_close(c) {
            depth = depth.saturating_sub(1);
            out.push(' ');
        } else if depth > 0 {
            // 括号内：整段丢弃
        } else if is_decor(c) || c.is_whitespace() {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

/// 在洗过的串里找艺名：返回**最靠左**的命中（用户口径「从左往右」）；
/// 位置相同时取最长的（`南方楠` 优先于 `方楠` 这类子集，虽然台账里不重叠也要求稳）。
///
/// `relax` = 右边界放宽（只要求左边界不紧贴汉字）。仅用于 strict 找不到时的兜底，
/// 例如 `薇姐唢呐下午5点30分直播` —— 「薇姐」右侧紧贴汉字，但它是唯一候选。
fn scan_stage_name(text: &str, roster: &[String], relax: bool) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut best: Option<(usize, usize, &str)> = None;
    for name in roster {
        let key: Vec<char> = name.chars().collect();
        if key.len() < 2 {
            continue; // 单字艺名不做匹配，误伤太大（「八宝」会被认成「宝」）
        }
        if let Some(at) = find_word_at(&chars, &key) {
            if !boundary_ok(&chars, at, key.len(), relax) {
                continue;
            }
            let cand = (at, key.len(), name.as_str());
            if best.is_none_or(|b| cand.0 < b.0 || (cand.0 == b.0 && cand.1 > b.1)) {
                best = Some(cand);
            }
        }
    }
    best.map(|(_, _, n)| n.to_string())
}

/// 在 chars 里找 key 第一次出现的下标（按 char 位置，不用字节偏移 —— 全中文更省事）
fn find_word_at(chars: &[char], key: &[char]) -> Option<usize> {
    if key.is_empty() || key.len() > chars.len() {
        return None;
    }
    (0..=chars.len() - key.len()).find(|&i| &chars[i..i + key.len()] == key)
}

/// 边界检查：命中两侧不能紧贴汉字 —— 否则 `晓明` 会被 `小晓明` 命中、
/// `狼明浩雨` 里会拆出「明浩」这类根本不存在的艺名。
fn boundary_ok(chars: &[char], at: usize, len: usize, relax: bool) -> bool {
    let left_ok = at == 0 || !is_cjk(chars[at - 1]);
    if !left_ok {
        return false;
    }
    if relax {
        return true;
    }
    let right = at + len;
    right >= chars.len() || !is_cjk(chars[right])
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

    /// 艺名抽取的台账候选：取自生产 person 表的真实艺名（节选，含 2 字与 3 字）。
    fn roster() -> Vec<String> {
        [
            "狼征",
            "狼明",
            "狼腾",
            "浩艺",
            "浩坤",
            "浩辰",
            "浩泽",
            "浩森",
            "玖玉",
            "玖玥",
            "玖安",
            "玖久",
            "啸燃",
            "啸泽",
            "啸安",
            "晓雅",
            "晓妍",
            "晓静",
            "晓楠",
            "晓倩",
            "晓暖",
            "晓涵",
            "三宝",
            "狼佑",
            "狼霆",
            "狼途",
            "狼澈",
            "狼小宝",
            "鹏先生",
            "南方楠",
            "薇姐",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    /// 生产昵称 → 期望艺名（None = 认不出，必须留在暂存区）。
    /// 全部来自 2026-10-17 / 2026-09 的运营 CSV 真实行。
    fn stage(s: &str) -> Option<String> {
        extract_stage_name(s, &roster())
    }

    #[test]
    fn stage_name_plain_and_decorated() {
        assert_eq!(stage("狼霆🎹").as_deref(), Some("狼霆"));
        assert_eq!(stage("浩艺🎙️").as_deref(), Some("浩艺"));
        assert_eq!(stage("玖玥🎹").as_deref(), Some("玖玥"));
        assert_eq!(stage("晓静").as_deref(), Some("晓静"));
        // 洗掉空格 / 音乐符号后连成整词
        assert_eq!(stage("啸  𝄞  燃 🎹 🎤").as_deref(), Some("啸燃"));
        assert_eq!(stage("玖雪 🎹").as_deref(), None, "玖雪 不在台账里，不能认");
    }

    #[test]
    fn stage_name_bracket_content_is_never_the_person() {
        // 用户口径：玖玉🎹🎤(尊师狼轩) 是玖玉，不是狼轩 —— 括号内容必须整段丢弃
        assert_eq!(stage("玖玉🎹🎤(尊师狼轩)").as_deref(), Some("玖玉"));
        assert_eq!(
            stage("狼凯🎹（南方楠）").as_deref(),
            None,
            "南方楠 在括号里，不算"
        );
        // 从左往右：括号前的浩坤是本人，括号里的鹏先生不是
        assert_eq!(stage("浩坤🎹🎤《鹏先生》").as_deref(), Some("浩坤"));
        assert_eq!(
            stage("三宝🎹才艺（每天晚上八点开播）").as_deref(),
            Some("三宝")
        );
        assert_eq!(stage("晓楠🎹").as_deref(), Some("晓楠"));
    }

    #[test]
    fn stage_name_leftmost_wins_and_loose_side_falls_back() {
        // 宽松兜底：艺名在串首，右侧紧贴汉字也认（洗完是「薇姐唢呐下午5点30分直播」）
        assert_eq!(
            stage("薇姐唢呐🔥下午5点30分直播🔥").as_deref(),
            Some("薇姐")
        );
        assert_eq!(stage("晓涵才艺【尊师🎹微姐】").as_deref(), Some("晓涵"));
        assert_eq!(stage("晓暖【键盘🎹薇姐】").as_deref(), Some("晓暖"));
        /* 括号里带艺名、括号外没有 → 不认（保守闸门③）。
        生产里 `十一宝《才艺🎹薇姐》小号` / `十七宝《小号》才艺🎹薇姐` /
        `晓玖才艺薇姐` 都长这样，看着像薇姐的小号，但也可能只是薇姐的徒弟号。
        「宁可留在暂存区等运营点一下」优先于自动认人 —— 要认就往 aliases 里加一条
        `十一宝 → 薇姐`，那是人工确认过的事实，不是猜的。 */
        assert_eq!(stage("十一宝《才艺🎹薇姐》小号"), None);
        assert_eq!(stage("晓玖才艺薇姐"), None);
    }

    #[test]
    fn stage_name_rejects_substring_and_unknown() {
        // 左边界紧贴汉字 → 拒绝：`小晓明` 不能认成「晓明」（何况台账里也没有晓明）
        assert_eq!(stage("小晓明"), None);
        /* 昵称里连续出现两个艺名时按「从左往右」取左边（用户口径 2026-10-18）。
        生产 CSV 里没有这种昵称（艺名后面跟的多是「才艺 / 唢呐 / 🎹」），
        真出现了也以左边为准，与 `玖玉🎹🎤(尊师狼轩)` 取「玖玉」是同一条规则。 */
        assert_eq!(stage("狼明浩雨").as_deref(), Some("狼明"));
        // 台账外的名字（外团 / 观众 / 注销号）一个都不认
        for nick in [
            "账号已注销",
            "用户5567837253729",
            "八宝",
            "狼月🎹🎙️",
            "狼烁🎹🎤",
            "玖汐🎹🎤",
            "洋洋洋",
            "店小二🍗",
            "茫然",
            "已换号",
            "：",
            "ST-001",
        ] {
            assert_eq!(stage(nick), None, "{nick} 不该被认成任何艺名");
        }
    }

    #[test]
    fn stage_name_empty_roster_never_matches() {
        assert_eq!(extract_stage_name("狼霆🎹", &[]), None);
    }
}
