//! 运营 CSV 解析（从 3328 Go 版 csvparse 忠实移植，规则必须一致）。
//!
//! 运营手里的 CSV 表头千奇百怪（"主播id"/"抖音号"/"anchor_id"/"uid" 混用），
//! 数值单位也乱（"12.5万" / "2:30:45" / "37小时16分钟" / "150分钟"）。
//! 有表头按表头识别列；没有表头回落到 bot/mod.rs 的裸列序解析。

use serde::{Deserialize, Serialize};

/// CSV 类型，靠表头自动识别。时长 → 音浪 → 主播档案（与 615/3328 顺序一致）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Wave,
    Duration,
    Anchors,
    Unknown,
}

/// 解析出的一行
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CsvRow {
    pub anchor_id: String,
    pub name: String,
    #[serde(rename = "douyinNo")]
    pub douyin_no: String,
    pub wave: Option<f64>,
    pub minutes: Option<f64>,
    #[serde(rename = "rawIndex")]
    pub raw_index: usize,
    pub err: String,
}

/// 去掉空格、下划线、括号、点，统一小写后比较。
/// 顺带丢掉 UTF-8 BOM（\u{feff}）：抖音后台导出的 CSV 首行带 BOM，
/// 不处理的话「主播ID」列永远精确匹配不上，会退到「抖音号」列（踩过）。
fn normalize_header(s: &str) -> String {
    s.chars()
        .filter(|r| {
            !r.is_whitespace()
                && !matches!(
                    r,
                    '_' | '-'
                        | '('
                        | ')'
                        | '（'
                        | '）'
                        | '['
                        | ']'
                        | '【'
                        | '】'
                        | '.'
                        | '\u{feff}'
                )
        })
        .flat_map(char::to_lowercase)
        .collect()
}

const ID_HEADERS: [&str; 7] = [
    "主播id",
    "主播账号",
    "抖音号",
    "抖音id",
    "anchorid",
    "uid",
    "id",
];
/// 真主播 ID 的候选。必须先于抖音号做完「精确 + 包含」匹配，否则
/// `主播ID（主账号）` 会被表头里精确的「抖音号」抢走。
const ANCHOR_ID_HEADERS: [&str; 4] = ["主播id", "主播账号", "anchorid", "uid"];
const GENERIC_ID_HEADERS: [&str; 1] = ["id"];
const NAME_HEADERS: [&str; 14] = [
    "主播名",
    "主播昵称",
    "主播姓名",
    "昵称",
    "用户昵称",
    "姓名",
    "名字",
    "抖音昵称",
    "达人昵称",
    "作者昵称",
    "成员",
    "主播",
    "name",
    "nickname",
];
/// 具体业务列优先于泛化列。真实公会文件同时出现
/// `总音浪 / 会员订阅音浪 / 分成比-直播音浪收益` 时，必须先锁定 `总音浪`。
const WAVE_HEADERS: [&str; 6] = [
    "总音浪",
    "当日音浪",
    "dailywave",
    "音浪",
    "wavevalue",
    "wave",
];
const DUR_HEADERS: [&str; 7] = [
    "开播有效时长",
    "开播时长",
    "直播时长",
    "有效时长",
    "durationminutes",
    "duration",
    "时长",
];
const RANK_HEADERS: [&str; 2] = ["排名", "rank"];
const DOUYIN_HEADERS: [&str; 3] = ["抖音号", "抖音id", "douyinno"];

/// 含这些词的列不是音浪列：「分成比-直播音浪收益」这种比例列一旦被当成音浪，
/// 月度时长文件里每个人的当日音浪会变成分成比（35 / 40 / 50），榜单直接失真。
const WAVE_HEADER_EXCLUDE: [&str; 14] = [
    "分成",
    "比例",
    "占比",
    "系数",
    "会员订阅",
    "星守护",
    "粉丝团",
    "礼物",
    "互动",
    "member",
    "subscri",
    "guard",
    "gift",
    "fansclub",
];

/// 这些是「看播 / 观众停留」类指标，不是主播自己的开播有效时长。
const DUR_HEADER_EXCLUDE: [&str; 9] = [
    "feed", "看播", "观看", "观众", "停留", "watch", "view", "avg", "average",
];

fn find_column_filtered(headers: &[String], candidates: &[&str], exclude: &[&str]) -> i32 {
    // 先按候选词的业务优先级找精确列，再退到包含匹配。不能按表头出现顺序
    // 先命中泛化词，否则 `会员订阅音浪` / `feed看播时长` 会抢走真正指标列。
    for c in candidates {
        for (i, n) in headers.iter().enumerate() {
            if n == c {
                return i as i32;
            }
        }
    }
    for c in candidates {
        for (i, n) in headers.iter().enumerate() {
            if n.contains(c) && !exclude.iter().any(|x| n.contains(x)) {
                return i as i32;
            }
        }
    }
    -1
}

fn find_column_by_candidate_priority(headers: &[String], candidates: &[&str]) -> i32 {
    for c in candidates {
        for (i, n) in headers.iter().enumerate() {
            if n == c {
                return i as i32;
            }
        }
        for (i, n) in headers.iter().enumerate() {
            if n.contains(c) {
                return i as i32;
            }
        }
    }
    -1
}

/// 找音浪列：精确匹配优先；包含匹配时跳过分成比 / 占比这类「带音浪字样的比例列」。
fn find_wave_column(headers: &[String]) -> i32 {
    find_column_filtered(headers, &WAVE_HEADERS, &WAVE_HEADER_EXCLUDE)
}

/// 找开播时长列：`开播有效时长 / 直播时长` 优先，明确排除 feed 看播时长。
fn find_duration_column(headers: &[String]) -> i32 {
    find_column_filtered(headers, &DUR_HEADERS, &DUR_HEADER_EXCLUDE)
}

/// 在表头里找第一个命中的列下标；先精确匹配，再包含匹配（应对"主播ID（必填）"）
fn find_column(headers: &[String], candidates: &[&str]) -> i32 {
    find_column_filtered(headers, candidates, &[])
}

fn find_id_column(headers: &[String]) -> i32 {
    let anchor = find_column_by_candidate_priority(headers, &ANCHOR_ID_HEADERS);
    if anchor >= 0 {
        return anchor;
    }
    let douyin = find_column(headers, &DOUYIN_HEADERS);
    if douyin >= 0 {
        return douyin;
    }
    find_column_by_candidate_priority(headers, &GENERIC_ID_HEADERS)
}

/// 按表头猜 CSV 类型：时长 → 音浪 → 主播档案
pub fn detect_kind(headers: &[String]) -> Kind {
    let normalized: Vec<String> = headers.iter().map(|h| normalize_header(h)).collect();
    if find_duration_column(&normalized) >= 0 {
        return Kind::Duration;
    }
    if find_wave_column(&normalized) >= 0 {
        return Kind::Wave;
    }
    let all = normalized.join("");
    for kw in ["主播", "昵称", "抖音号"] {
        if all.contains(kw) {
            return Kind::Anchors;
        }
    }
    Kind::Unknown
}

/// 解析音浪值，支持 "12.5万" / "125000" / "12,500" / "1.2亿" / "3w"
pub fn parse_wave(raw: &str) -> anyhow::Result<i64> {
    if metric_is_missing(raw) {
        return Ok(0);
    }
    let cleaned = raw.trim().replace([',', '，'], "").replace(' ', "");
    let mut s = if let Some(stripped) = cleaned.strip_suffix("音浪") {
        stripped.to_string()
    } else if let Some(stripped) = cleaned.strip_prefix("音浪") {
        stripped.to_string()
    } else {
        cleaned
    };
    let mut multiplier: f64 = 1.0;
    if let Some(stripped) = s.strip_suffix('万') {
        multiplier = 10_000.0;
        s = stripped.to_string();
    } else if s.ends_with('w') || s.ends_with('W') {
        multiplier = 10_000.0;
        s = s[..s.len() - 1].to_string();
    } else if let Some(stripped) = s.strip_suffix('亿') {
        multiplier = 100_000_000.0;
        s = stripped.to_string();
    }
    let v: f64 = s
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("音浪值无法解析: {raw}"))?;
    non_negative_i64(v * multiplier, "音浪", raw)
}

fn non_negative_i64(value: f64, label: &str, raw: &str) -> anyhow::Result<i64> {
    if !value.is_finite() || value < 0.0 || value >= i64::MAX as f64 {
        anyhow::bail!("{label}必须是有限非负整数范围，当前: {raw}");
    }
    Ok(value as i64)
}

/// 空单元格 / 单个占位横线表示“文件没有提供这个指标”，
/// 必须与明确写出的数字 `0` 区分，否则时长-only 导入会把旧音浪清零。
fn metric_is_missing(raw: &str) -> bool {
    let s = raw.trim();
    s.is_empty() || matches!(s, "-" | "—" | "–")
}

fn append_row_error(row: &mut CsvRow, err: String) {
    if row.err.is_empty() {
        row.err = err;
    } else {
        row.err.push_str("；");
        row.err.push_str(&err);
    }
}

fn parse_clock_duration(s: &str, raw: &str) -> anyhow::Result<f64> {
    let parts: Vec<&str> = s.split(':').collect();
    if !matches!(parts.len(), 2 | 3) {
        anyhow::bail!("时长无法解析: {raw}");
    }
    let vals: Vec<i64> = parts
        .iter()
        .map(|p| {
            p.parse::<i64>()
                .map_err(|_| anyhow::anyhow!("时长无法解析: {raw}"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    if vals.iter().any(|v| *v < 0) {
        anyhow::bail!("时长不能为负数: {raw}");
    }
    // 分、秒必须落在 0..60；小时位可以超过 24（例如 37:16:05）。
    if vals[1..].iter().any(|v| *v >= 60) {
        anyhow::bail!("时长分/秒必须小于 60: {raw}");
    }
    Ok(match vals.as_slice() {
        [h, m] => h
            .checked_mul(60)
            .and_then(|v| v.checked_add(*m))
            .ok_or_else(|| anyhow::anyhow!("时长超出范围: {raw}"))? as f64,
        [h, m, sec] => {
            let minutes = h
                .checked_mul(60)
                .and_then(|v| v.checked_add(*m))
                .ok_or_else(|| anyhow::anyhow!("时长超出范围: {raw}"))?;
            minutes as f64 + *sec as f64 / 60.0
        }
        _ => unreachable!(),
    })
}

fn duration_unit_at(chars: &[char], i: usize) -> Option<(usize, u8, f64)> {
    // 长单位放前面，避免 `分钟` 被 `分` 抢先；rank 只能严格递减。
    for (unit, rank, mult) in [
        ("小时", 3, 60.0),
        ("hours", 3, 60.0),
        ("hour", 3, 60.0),
        ("minutes", 2, 1.0),
        ("分钟", 2, 1.0),
        ("minute", 2, 1.0),
        ("mins", 2, 1.0),
        ("min", 2, 1.0),
        ("秒钟", 1, 1.0 / 60.0),
        ("seconds", 1, 1.0 / 60.0),
        ("second", 1, 1.0 / 60.0),
        ("secs", 1, 1.0 / 60.0),
        ("sec", 1, 1.0 / 60.0),
        ("时", 3, 60.0),
        ("分", 2, 1.0),
        ("秒", 1, 1.0 / 60.0),
        ("h", 3, 60.0),
        ("m", 2, 1.0),
        ("s", 1, 1.0 / 60.0),
    ] {
        let uc: Vec<char> = unit.chars().collect();
        if chars.get(i..i + uc.len()) == Some(uc.as_slice()) {
            return Some((uc.len(), rank, mult));
        }
    }
    None
}

fn parse_unit_duration(s: &str, raw: &str) -> anyhow::Result<f64> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut total = 0.0;
    let mut last_rank = u8::MAX;
    while i < chars.len() {
        let start = i;
        let mut saw_digit = false;
        let mut saw_dot = false;
        while i < chars.len() {
            if chars[i].is_ascii_digit() {
                saw_digit = true;
                i += 1;
            } else if chars[i] == '.' && !saw_dot {
                saw_dot = true;
                i += 1;
            } else {
                break;
            }
        }
        if !saw_digit {
            anyhow::bail!("时长无法解析: {raw}");
        }
        let number: String = chars[start..i].iter().collect();
        let value: f64 = number
            .parse()
            .map_err(|_| anyhow::anyhow!("时长无法解析: {raw}"))?;
        let Some((unit_len, rank, multiplier)) = duration_unit_at(&chars, i) else {
            anyhow::bail!("时长缺少有效单位或含非法字符: {raw}");
        };
        if rank >= last_rank {
            anyhow::bail!("时长单位顺序不正确: {raw}");
        }
        total += value * multiplier;
        last_rank = rank;
        i += unit_len;
    }
    if !total.is_finite() || total < 0.0 {
        anyhow::bail!("时长必须是有限非负数: {raw}");
    }
    Ok(total)
}

/// 解析时长，统一换算为分钟。
/// 支持：2:30:45 / 1:30 / 37小时16分钟5秒 / 1小时30分 / 150分钟 / 150
pub fn parse_duration(raw: &str) -> anyhow::Result<i64> {
    let trimmed = raw.trim();
    if metric_is_missing(trimmed) {
        return Ok(0);
    }
    // 空格在中文时长里只作分隔（`1 小时 30 分钟`），不参与数值解析。
    let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    let s = compact.replace('：', ":").to_ascii_lowercase();
    if s.starts_with('-') || s.contains("-") {
        anyhow::bail!("时长不能为负数: {raw}");
    }
    // 2:30:45 或 1:30
    if s.contains(':') {
        return non_negative_i64(parse_clock_duration(&s, raw)?, "时长", raw);
    }
    // 中文 / 英文单位：37小时16分钟5秒 / 1.5h / 150分钟。
    // 纯数字（没有单位）沿用旧口径，按分钟。
    if s.chars().all(|c| c.is_ascii_digit() || c == '.') {
        let v: f64 = s
            .parse()
            .map_err(|_| anyhow::anyhow!("时长无法解析: {raw}"))?;
        return non_negative_i64(v, "时长", raw);
    }
    non_negative_i64(parse_unit_duration(&s, raw)?, "时长", raw)
}

/// 按引号感知的方式切一行 CSV（支持 "a,b" 带逗号字段）
fn split_csv_line(line: &str, delimiter: char) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == delimiter {
            fields.push(cur.trim().to_string());
            cur = String::new();
        } else {
            cur.push(c);
        }
    }
    fields.push(cur.trim().to_string());
    fields
}

fn sniff_delimiter(line: &str) -> char {
    let count = |d: char| line.matches(d).count();
    if count('\t') >= count(',') && line.contains('\t') {
        '\t'
    } else {
        ','
    }
}

fn is_blank(rec: &[String]) -> bool {
    rec.iter().all(|f| f.trim().is_empty())
}

fn normalized_fields(rec: &[String]) -> Vec<String> {
    let mut out: Vec<String> = rec.iter().map(|f| normalize_header(f)).collect();
    while out.last().is_some_and(|s| s.is_empty()) {
        out.pop();
    }
    out
}

/// 判断某一行是否还是 CSV 表头。不能只看第一份表头：QQ / 粘贴入口一旦把
/// 多个附件正文拼在一起，第二份文件的列数、列序甚至分隔符都可能不同。
fn looks_like_header(rec: &[String]) -> bool {
    let fields = normalized_fields(rec);
    if fields.len() < 2 {
        return false;
    }
    let known = fields
        .iter()
        .filter(|n| {
            let n = n.as_str();
            ID_HEADERS
                .iter()
                .chain(NAME_HEADERS.iter())
                .chain(DOUYIN_HEADERS.iter())
                .chain(WAVE_HEADERS.iter())
                .chain(DUR_HEADERS.iter())
                .chain(RANK_HEADERS.iter())
                .any(|h| n == *h || (h.chars().count() >= 3 && n.contains(*h)))
        })
        .count();
    known >= 2
}

fn is_repeated_header(rec: &[String], headers: &[String]) -> bool {
    normalized_fields(rec) == normalized_fields(headers)
}

fn merge_kind(a: Kind, b: Kind) -> Kind {
    match (a, b) {
        (Kind::Duration, _) | (_, Kind::Duration) => Kind::Duration,
        (Kind::Wave, _) | (_, Kind::Wave) => Kind::Wave,
        (Kind::Anchors, _) | (_, Kind::Anchors) => Kind::Anchors,
        _ => Kind::Unknown,
    }
}

fn usable_ident(raw: &str) -> bool {
    !matches!(raw.trim(), "" | "0" | "-" | "—" | "–" | "账号已注销")
}

/// 一个 CSV block 的列映射。多附件被拼接时，每遇到新表头就重新解析 schema，
/// 后续数据行不会再被第一份文件的列序解释。
struct CsvSchema {
    delimiter: char,
    headers: Vec<String>,
    kind: Kind,
    id_col: i32,
    name_col: i32,
    wave_col: i32,
    dur_col: i32,
    douyin_col: i32,
}

impl CsvSchema {
    fn from_header(header_line: &str) -> Self {
        let delimiter = sniff_delimiter(header_line);
        let headers = split_csv_line(header_line, delimiter)
            .iter()
            .map(|h| normalize_header(h))
            .collect::<Vec<_>>();
        Self {
            delimiter,
            kind: detect_kind(&headers),
            id_col: find_id_column(&headers),
            name_col: find_column(&headers, &NAME_HEADERS),
            wave_col: find_wave_column(&headers),
            dur_col: find_duration_column(&headers),
            douyin_col: find_column(&headers, &DOUYIN_HEADERS),
            headers,
        }
    }

    fn parse_row(&self, rec: &[String], raw_index: usize) -> CsvRow {
        let get = |col: i32| -> String {
            if col >= 0 {
                rec.get(col as usize).cloned().unwrap_or_default()
            } else {
                String::new()
            }
        };
        let mut row = CsvRow {
            raw_index,
            ..Default::default()
        };
        let anchor_id = get(self.id_col);
        let douyin_no = get(self.douyin_col);
        row.anchor_id = usable_ident(&anchor_id)
            .then_some(anchor_id)
            .unwrap_or_default();
        row.douyin_no = usable_ident(&douyin_no)
            .then_some(douyin_no)
            .unwrap_or_default();
        if row.anchor_id.is_empty() && !row.douyin_no.is_empty() {
            // 主播 ID 才是稳定标识；列缺失、为空或写 0/横线时才回退抖音号。
            row.anchor_id = row.douyin_no.clone();
        }
        row.name = get(self.name_col);
        if self.wave_col >= 0 {
            let raw = get(self.wave_col);
            if !metric_is_missing(&raw) {
                match parse_wave(&raw) {
                    Ok(v) => row.wave = Some(v as f64),
                    Err(e) => append_row_error(&mut row, e.to_string()),
                }
            }
        }
        if self.dur_col >= 0 {
            let raw = get(self.dur_col);
            if !metric_is_missing(&raw) {
                match parse_duration(&raw) {
                    Ok(v) => row.minutes = Some(v as f64),
                    Err(e) => append_row_error(&mut row, e.to_string()),
                }
            }
        }
        row
    }
}

/// 解析整个 CSV。返回类型 + 行（含表头里时长列是否存在，见 [`ParseOut`]）。
///
/// 同一段文本里若出现多个表头（QQ 多附件被上游拼接），每个 block 都用自己的
/// 表头/分隔符解析，并合并结果；重复表头只切 schema，绝不当成主播数据。
pub fn parse(text: &str) -> anyhow::Result<(Kind, ParseOut)> {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .collect();
    let Some(header_line) = lines.first() else {
        anyhow::bail!("CSV 是空的");
    };
    let mut schema = CsvSchema::from_header(header_line);
    let mut kind = schema.kind;
    let mut has_dur_col = schema.dur_col >= 0;
    let mut has_wave_col = schema.wave_col >= 0;
    let mut rows = Vec::new();

    for (i, line) in lines.iter().enumerate().skip(1) {
        // 先按当前行自己的分隔符试探表头；命中后切换该 block 的完整列映射。
        let probe_delimiter = sniff_delimiter(line);
        let probe = split_csv_line(line, probe_delimiter);
        if !is_blank(&probe)
            && (is_repeated_header(&probe, &schema.headers) || looks_like_header(&probe))
        {
            schema = CsvSchema::from_header(line);
            kind = merge_kind(kind, schema.kind);
            has_dur_col |= schema.dur_col >= 0;
            has_wave_col |= schema.wave_col >= 0;
            continue;
        }

        let rec = split_csv_line(line, schema.delimiter);
        if is_blank(&rec) {
            continue;
        }
        rows.push(schema.parse_row(&rec, i + 1));
    }
    Ok((
        kind,
        ParseOut {
            rows,
            has_dur_col,
            has_wave_col,
        },
    ))
}

/// [`parse`] 的结果：解析出的行 + 表头里有没有时长列 / 音浪列。
///
/// 为什么要把这两个「有没有这一列」带出来：月度入口靠它区分两种
/// 「这一行没有时长」——文件压根没有时长列（不是月度指标文件，应整体判空，
/// 让前端提示换文件）vs 有时长列但这一行写坏了（必须报出来，否则那个人
/// 整月时长在月榜上是 0 且无提示）。音浪列同理（2026-10-16 起月度也能导音浪）。
pub struct ParseOut {
    pub rows: Vec<CsvRow>,
    pub has_dur_col: bool,
    pub has_wave_col: bool,
}

// 让 `parse(...)` 的结果能像以前那样直接当行的集合用（len / 下标 / 迭代），
// 需要「有没有时长列」这个新增信息时再走 `.has_dur_col`。
impl std::ops::Deref for ParseOut {
    type Target = Vec<CsvRow>;
    fn deref(&self) -> &Self::Target {
        &self.rows
    }
}

/// 月度指标文件解析结果：行 + 表头里有没有时长列 / 音浪列。
///
/// 上层靠 `has_dur_col` / `has_wave_col` 决定：文件里根本没有这类列
/// （不是月度指标文件 → 前端提示换文件）vs 有列但这一行没值 / 写坏了
/// （进 skipped 报告，否则整人静默消失、月榜上显示为 0）。
pub struct MonthParseOut {
    pub rows: Vec<CsvRow>,
    pub has_dur_col: bool,
    pub has_wave_col: bool,
}

impl MonthParseOut {
    fn empty() -> Self {
        Self {
            rows: Vec::new(),
            has_dur_col: false,
            has_wave_col: false,
        }
    }
}

/// 月度指标文件解析：**时长列或音浪列任一存在**就保留行，两种指标一起带出来。
///
/// 用户口径 2026-10-16：「时长或者音浪都支持按月进行导入和显示」。抖音公会后台
/// 导出的文件有两种长相：
/// - 纯月时长（`9月份时长.csv`）：`…,分成比-直播音浪收益,开播有效时长,主播标签`；
/// - 概览列表（`主播概览列表_*.csv` / `10.14….csv`）：
///   `…,分成比-直播音浪收益,总音浪,会员订阅音浪,开播有效时长,feed看播时长,…`
///   两种指标同在一张表里，按月导入时哪个都不该丢。
///
/// 「分成比-直播音浪收益」这种含「音浪」字样的比例列由 [`find_wave_column`] 的
/// 排除表挡掉（否则 35 / 40 / 50 会被人人当成年纪音浪值），这里不重复判断。
pub fn parse_month_metrics(text: &str) -> MonthParseOut {
    let Ok((_, out)) = parse(text) else {
        return MonthParseOut::empty();
    };
    if !out.has_dur_col && !out.has_wave_col {
        // 两列都没有 → 不是月度指标文件，整体判空（前端据此提示换文件 / 切回按日）
        return MonthParseOut::empty();
    }
    let rows = out
        .rows
        .into_iter()
        // 有名字/认人信息就行：时长或音浪解析失败的行保留，err 里带着原因，
        // 由上层计进 skipped 报告。
        .filter(|r| !r.name.is_empty() || !r.anchor_id.is_empty() || !r.douyin_no.is_empty())
        .collect();
    MonthParseOut {
        rows,
        has_dur_col: out.has_dur_col,
        has_wave_col: out.has_wave_col,
    }
}

/// 从 `i` 开始读连续数字，返回（结束下标, 值, 位数）
fn read_digits(cs: &[char], i: usize) -> (usize, i64, usize) {
    let mut end = i;
    let mut v: i64 = 0;
    while end < cs.len() && cs[end].is_ascii_digit() && end - i < 9 {
        v = v * 10 + (cs[end] as u8 - b'0') as i64;
        end += 1;
    }
    (end, v, end - i)
}

/// 从文件名读业务月份，返回 (年, 月)。
///
/// 运营命名习惯：「9月份时长.csv」（只写月份 → 年份补当前年）、
/// 「2026年9月份时长.csv」、「2026-09-时长.csv」都认。
/// 读不出来返回 None，由调用方用当前月兜底（前端还会让运营在弹窗里改）。
pub fn month_from_filename(name: &str) -> Option<(i32, u32)> {
    let cs: Vec<char> = name.chars().collect();
    let now_year = chrono::Local::now()
        .format("%Y")
        .to_string()
        .parse::<i32>()
        .unwrap_or(2026);
    // 模式 1：年份 + 分隔符 + 月份（2026年9月 / 2026-09 / 2026_09 / 2026.09 / 2026/9）
    let mut i = 0;
    while i < cs.len() {
        if !cs[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let (end, y, n) = read_digits(&cs, i);
        if n == 4 && (1970..=2999).contains(&y) {
            if let Some(&sep) = cs.get(end) {
                if matches!(sep, '年' | '-' | '_' | '/' | '.') {
                    let (_, m, mn) = read_digits(&cs, end + 1);
                    if mn >= 1 && (1..=12).contains(&m) {
                        return Some((y as i32, m as u32));
                    }
                }
            }
        }
        i = end.max(i + 1);
    }
    // 模式 2：只写月份（「9月份时长.csv」）→ 年份补当前年
    let mut i = 0;
    while i < cs.len() {
        if !cs[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let (end, m, n) = read_digits(&cs, i);
        if n <= 2 && (1..=12).contains(&m) && cs.get(end) == Some(&'月') {
            return Some((now_year, m as u32));
        }
        i = end.max(i + 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave_values_with_units() {
        assert_eq!(parse_wave("12.5万").unwrap(), 125_000);
        assert_eq!(parse_wave("125000").unwrap(), 125_000);
        assert_eq!(parse_wave("12,500").unwrap(), 12_500);
        assert_eq!(parse_wave("12 500").unwrap(), 12_500);
        assert_eq!(parse_wave("3w").unwrap(), 30_000);
        assert_eq!(parse_wave("1.2亿").unwrap(), 120_000_000);
        assert_eq!(parse_wave("-").unwrap(), 0);
        assert_eq!(parse_wave("").unwrap(), 0);
        assert!(parse_wave("abc").is_err());
        assert!(parse_wave("音浪").is_err());
        assert!(parse_wave("12音浪34").is_err());
    }

    #[test]
    fn duration_formats() {
        assert_eq!(parse_duration("2:30:45").unwrap(), 150);
        assert_eq!(parse_duration("1:30").unwrap(), 90);
        assert_eq!(parse_duration("37小时16分钟5秒").unwrap(), 37 * 60 + 16);
        assert_eq!(parse_duration("1小时30分").unwrap(), 90);
        assert_eq!(parse_duration("150分钟").unwrap(), 150);
        assert_eq!(parse_duration("2 小时 30 分钟 45 秒").unwrap(), 150);
        assert_eq!(parse_duration("1.5小时").unwrap(), 90);
        assert_eq!(parse_duration("1h30m").unwrap(), 90);
        assert_eq!(parse_duration("150min").unwrap(), 150);
        assert_eq!(parse_duration("150").unwrap(), 150);
        assert_eq!(parse_duration("-").unwrap(), 0);
        assert_eq!(parse_duration("—").unwrap(), 0);
    }

    #[test]
    fn detects_kind_by_headers() {
        let h: Vec<String> = ["主播名", "音浪", "直播时长"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(detect_kind(&h), Kind::Duration);
        let h: Vec<String> = ["主播名", "音浪"].iter().map(|s| s.to_string()).collect();
        assert_eq!(detect_kind(&h), Kind::Wave);
        let h: Vec<String> = ["主播名", "抖音号"].iter().map(|s| s.to_string()).collect();
        assert_eq!(detect_kind(&h), Kind::Anchors);
    }

    #[test]
    fn parses_headers_and_columns() {
        let csv = "主播昵称,音浪,直播时长\n浩龙,12.5万,2:30:45\n玖玉,3w,150分钟\n";
        let (kind, rows) = parse(csv).unwrap();
        assert_eq!(kind, Kind::Duration);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "浩龙");
        assert_eq!(rows[0].wave, Some(125_000.0));
        assert_eq!(rows[0].minutes, Some(150.0));
        assert_eq!(rows[1].wave, Some(30_000.0));
        assert_eq!(rows[1].minutes, Some(150.0));
    }

    #[test]
    fn quoted_fields_with_comma() {
        // 名字带逗号要加引号；数字带千分位也要加引号
        let csv = "主播名,音浪\n\"浩龙,团队\",\"12,500\"\n";
        let (_, rows) = parse(csv).unwrap();
        assert_eq!(rows[0].name, "浩龙,团队");
        assert_eq!(rows[0].wave, Some(12500.0));
    }

    #[test]
    fn blank_metrics_are_none_but_explicit_zero_is_kept() {
        let csv = "主播昵称,音浪,直播时长\n\
                   浩龙,,\n\
                   玖玉,0,0\n\
                   狼耀,-,—\n";
        let (_, rows) = parse(csv).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].wave, None, "空音浪不能当成显式 0");
        assert_eq!(rows[0].minutes, None, "空时长不能当成显式 0");
        assert_eq!(rows[1].wave, Some(0.0));
        assert_eq!(rows[1].minutes, Some(0.0));
        assert_eq!(rows[2].wave, None, "占位横线也按缺值处理");
        assert_eq!(rows[2].minutes, None);
    }

    #[test]
    fn negative_or_non_finite_metrics_are_rejected() {
        assert!(parse_wave("-1").is_err());
        assert!(parse_wave("NaN").is_err());
        assert!(parse_wave("inf").is_err());
        assert!(parse_duration("-1").is_err());
        assert!(parse_duration("1:-30").is_err());
        assert!(parse_duration("NaN").is_err());
        assert!(parse_duration("inf").is_err());
        for raw in [
            "abc",
            "abc小时",
            "1小时oops",
            "1小时30",
            "1:99",
            "9999999999999999999:00",
        ] {
            assert!(parse_duration(raw).is_err(), "非空非法时长不能变 0：{raw}");
        }
    }

    #[test]
    fn exact_metric_columns_beat_decoy_columns_regardless_of_order() {
        let csv =
            "主播昵称,会员订阅音浪,分成比-直播音浪收益,总音浪,feed看播时长,直播时长,开播有效时长\n\
                   浩龙,88888,35,5508,26小时7分钟4秒,120分钟,1小时48分钟15秒\n";
        let (kind, out) = parse(csv).unwrap();
        assert_eq!(kind, Kind::Duration);
        assert_eq!(out.rows[0].wave, Some(5508.0), "音浪必须取总音浪");
        assert_eq!(
            out.rows[0].minutes,
            Some(108.0),
            "时长必须取开播有效时长，不能取 feed看播时长"
        );

        let only_derivatives = "主播昵称,星守护音浪,会员订阅音浪\n浩龙,999,888\n";
        let (_, out) = parse(only_derivatives).unwrap();
        assert_eq!(
            out.rows[0].wave, None,
            "没有总/当日音浪时，不能把星守护或会员订阅音浪顶上"
        );
    }

    #[test]
    fn anchor_id_beats_douyin_no_and_placeholder_id_falls_back() {
        let csv = "抖音号,主播账号,主播昵称,主播ID,总音浪\n\
                   dy-1,acc-1,浩龙,id-1,100\n\
                   dy-2,acc-2,玖玉,0,200\n\
                   dy-3,acc-3,狼耀,—,300\n";
        let (_, out) = parse(csv).unwrap();
        assert_eq!(out.rows[0].anchor_id, "id-1");
        assert_eq!(out.rows[0].douyin_no, "dy-1");
        assert_eq!(out.rows[1].anchor_id, "dy-2");
        assert_eq!(out.rows[2].anchor_id, "dy-3");
    }

    #[test]
    fn invalid_metrics_keep_errors_and_do_not_become_zero() {
        let csv = "主播昵称,总音浪,开播有效时长\n浩龙,abc,oops\n玖玉,0,0\n";
        let (_, out) = parse(csv).unwrap();
        assert_eq!(out.rows.len(), 2);
        assert_eq!(out.rows[0].wave, None);
        assert_eq!(out.rows[0].minutes, None);
        assert!(out.rows[0].err.contains("音浪"), "{}", out.rows[0].err);
        assert!(out.rows[0].err.contains("时长"), "{}", out.rows[0].err);
        assert_eq!(out.rows[1].wave, Some(0.0), "显式 0 必须保留");
        assert_eq!(out.rows[1].minutes, Some(0.0), "显式 0 必须保留");
        assert!(out.rows[1].err.is_empty());
    }

    /// 抖音公会后台的月度时长文件（9月份时长.csv 的真实表头）。
    /// 里面「分成比-直播音浪收益」含「音浪」二字：以前包含匹配会把 35 当成人人一万的当日音浪。
    const MONTH_CSV: &str =
        "主播ID,主播昵称,抖音号,粉丝数,签约类型,分成比-直播音浪收益,开播有效时长,主播标签\n\
         2436948398193464,浩龙🎹,cyl765,19526,金牌主播,35,75小时29分钟41秒,\n\
         7616689691311686705,玖玉🎹🎤,37181712726,4702,普通主播,35,120小时45分钟1秒,\n";

    #[test]
    fn ratio_column_is_not_wave() {
        let (kind, rows) = parse(MONTH_CSV).unwrap();
        assert_eq!(kind, Kind::Duration);
        assert_eq!(rows[0].wave, None, "分成比不是音浪，不能读成 35");
        assert_eq!(rows[1].wave, None);
        assert_eq!(
            rows[0].minutes,
            Some((75 * 60 + 29) as f64),
            "75小时29分钟41秒"
        );
        assert_eq!(rows[1].minutes, Some((120 * 60 + 45) as f64));
        assert_eq!(rows[0].anchor_id, "2436948398193464");
        assert_eq!(rows[0].douyin_no, "cyl765");
    }

    /// 概览列表（2026-10-16 用户给的导入样例：`10.14….csv` / `10.11 22222.csv`）。
    /// 与月时长文件的区别是它同时带「总音浪」：按月导入时两种指标都要留住。
    const OVERVIEW_CSV: &str =
        "主播ID,主播昵称,抖音号,粉丝数,签约类型,分成比-直播音浪收益,总音浪,会员订阅音浪,开播有效时长,feed看播时长,主播标签\n\
         2855887229949241,双狼呈音,75995728462,58,普通主播,35,5508,0,1小时48分钟15秒,7小时26分钟3秒,\n\
         2416047830865369,狼霆🎹,56874096680,363,普通主播,35,3193,0,1小时2分钟31秒,4小时1分钟26秒,\n";

    #[test]
    fn month_metrics_keep_both_duration_and_wave() {
        // 纯月时长文件：只有时长列
        let m = parse_month_metrics(MONTH_CSV);
        assert_eq!(m.rows.len(), 2);
        assert_eq!(m.rows[0].name, "浩龙🎹");
        assert_eq!(m.rows[0].minutes, Some(4529.0));
        assert_eq!(m.rows[0].wave, None, "分成比不是音浪");
        assert!(m.has_dur_col && !m.has_wave_col);
        // 概览列表：时长 + 音浪同在一张表
        let m = parse_month_metrics(OVERVIEW_CSV);
        assert_eq!(m.rows.len(), 2);
        assert!(m.has_dur_col && m.has_wave_col);
        assert_eq!(m.rows[0].wave, Some(5508.0), "音浪要取「总音浪」列");
        assert_eq!(
            m.rows[0].minutes,
            Some(108.0),
            "1小时48分钟15秒 → 108 分钟（秒丢掉）"
        );
        assert_eq!(m.rows[1].wave, Some(3193.0));
        assert_eq!(m.rows[1].minutes, Some(62.0));
        // 只有音浪列的月度文件也认（月度音浪导入）
        let m = parse_month_metrics("主播昵称,音浪\n浩龙,12000\n");
        assert_eq!(m.rows.len(), 1);
        assert!(!m.has_dur_col && m.has_wave_col);
        assert_eq!(m.rows[0].wave, Some(12000.0));
        // 两类列都没有 / 空文件：解析不出行，调用方据此提示「不是月度指标文件」
        assert!(parse_month_metrics("主播名\n浩龙\n").rows.is_empty());
        assert!(parse_month_metrics("").rows.is_empty());
    }

    /// 抖音后台导出的 CSV 带 UTF-8 BOM：不能让「主播ID」列因此认不出来
    /// （认不出来就会退到「抖音号」列，主播 ID 语义就错了）。
    #[test]
    fn bom_header_still_finds_anchor_id_column() {
        let csv = "\u{feff}主播ID,主播昵称,抖音号,开播有效时长\n\
                   2436948398193464,浩龙🎹,cyl765,75小时29分钟41秒\n";
        let (_, rows) = parse(csv).unwrap();
        assert_eq!(rows[0].anchor_id, "2436948398193464");
        assert_eq!(rows[0].douyin_no, "cyl765");
        assert_eq!(rows[0].name, "浩龙🎹");
    }

    /// 表头归一化只影响比较，不能影响数据行或列识别。
    #[test]
    fn header_normalization_ignores_formatting_and_bom() {
        assert_eq!(
            normalize_header("\u{feff} 主播_ID（直播）.总音浪 "),
            "主播id直播总音浪"
        );
    }

    /// QQ 同时转发两份同格式 CSV：正文被拼成一段，两份文件都带 BOM，
    /// 第二份表头还带空格/下划线/括号/点，归一化后应跳过而不是成为数据行。
    #[test]
    fn concatenated_csv_skips_normalized_repeated_header_and_keeps_data() {
        let first = "\u{feff}主播ID,主播昵称,抖音号,总音浪,开播有效时长\r\n\
                     7616689691311686705,玖玉🎹,37181712726,7717,3小时47分钟17秒\r\n";
        let second = "\u{feff}主播 ID,主播_昵称,抖音.号,总 音浪,开播(有效)时长\r\n\
                      2436948398193464,浩龙🎹,cyl765,5508,1小时48分钟15秒\r\n";
        let (kind, out) = parse(&format!("{first}{second}")).unwrap();
        assert_eq!(kind, Kind::Duration);
        assert!(out.has_wave_col && out.has_dur_col);
        assert_eq!(out.rows.len(), 2, "重复表头必须跳过：{:?}", out.rows);
        assert_eq!(out.rows[0].name, "玖玉🎹");
        assert_eq!(out.rows[0].anchor_id, "7616689691311686705");
        assert_eq!(out.rows[0].douyin_no, "37181712726");
        assert_eq!(out.rows[0].wave, Some(7717.0));
        assert_eq!(out.rows[0].minutes, Some(227.0));
        assert_eq!(out.rows[1].name, "浩龙🎹");
        assert_eq!(out.rows[1].anchor_id, "2436948398193464");
        assert_eq!(out.rows[1].douyin_no, "cyl765");
        assert_eq!(out.rows[1].wave, Some(5508.0));
        assert_eq!(out.rows[1].minutes, Some(108.0));
    }

    /// 更极端：第二份 CSV 的列顺序和表头写法都不同。每个 block 必须切换自己的
    /// schema；否则第二份表头会变成假主播，后续数据也会按第一份列序错读。
    #[test]
    fn concatenated_csv_with_different_schema_uses_each_header() {
        let text = "\u{feff}主播ID,主播昵称,抖音号,总音浪,开播有效时长\r\n\
                    1001,玖玉🎹,dy-1,7717,3小时47分钟17秒\r\n\
                    \u{feff}主播_昵称,总 音浪,主播ID（主账号）,抖音号,直播时长,\r\n\
                    浩龙🎹,5508,2002,dy-2,1小时48分钟15秒,\r\n";
        let (_, out) = parse(text).unwrap();
        assert_eq!(out.rows.len(), 2, "表头不能成为假行：{:?}", out.rows);
        assert_eq!(out.rows[0].name, "玖玉🎹");
        assert_eq!(out.rows[0].anchor_id, "1001");
        assert_eq!(out.rows[0].wave, Some(7717.0));
        assert_eq!(out.rows[1].name, "浩龙🎹");
        assert_eq!(out.rows[1].anchor_id, "2002");
        assert_eq!(out.rows[1].douyin_no, "dy-2");
        assert_eq!(out.rows[1].wave, Some(5508.0));
        assert_eq!(out.rows[1].minutes, Some(108.0));
    }

    #[test]
    fn month_from_filename_patterns() {
        let now_year: i32 = chrono::Local::now()
            .format("%Y")
            .to_string()
            .parse()
            .unwrap();
        assert_eq!(month_from_filename("9月份时长.csv"), Some((now_year, 9)));
        assert_eq!(month_from_filename("12月时长.csv"), Some((now_year, 12)));
        assert_eq!(month_from_filename("2026年9月份时长.csv"), Some((2026, 9)));
        assert_eq!(month_from_filename("2026-09-时长.csv"), Some((2026, 9)));
        assert_eq!(month_from_filename("时长_2026_10.csv"), Some((2026, 10)));
        // 读不出月份：交给前端 / 当前月兜底
        assert_eq!(month_from_filename("时长.csv"), None);
        // 带完整日期的文件名取到年月（只有内容真是时长表才会走月度导入）
        assert_eq!(month_from_filename("2026-10-07-男团.csv"), Some((2026, 10)));
        assert_eq!(month_from_filename("13月份时长.csv"), None);
    }
}
