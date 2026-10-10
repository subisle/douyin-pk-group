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
const NAME_HEADERS: [&str; 12] = [
    "主播名",
    "主播昵称",
    "昵称",
    "用户昵称",
    "姓名",
    "抖音昵称",
    "达人昵称",
    "作者昵称",
    "成员",
    "主播",
    "name",
    "nickname",
];
const WAVE_HEADERS: [&str; 4] = ["音浪", "wavevalue", "wave", "总音浪"];
const DUR_HEADERS: [&str; 7] = [
    "时长",
    "duration",
    "durationminutes",
    "直播时长",
    "开播有效时长",
    "有效时长",
    "开播时长",
];
const RANK_HEADERS: [&str; 2] = ["排名", "rank"];
const DOUYIN_HEADERS: [&str; 3] = ["抖音号", "抖音id", "douyinno"];

/// 含这些词的列不是音浪列：「分成比-直播音浪收益」这种比例列一旦被当成音浪，
/// 月度时长文件里每个人的当日音浪会变成分成比（35 / 40 / 50），榜单直接失真。
const WAVE_HEADER_EXCLUDE: [&str; 4] = ["分成", "比例", "占比", "系数"];

/// 找音浪列：精确匹配优先；包含匹配时跳过分成比 / 占比这类「带音浪字样的比例列」。
fn find_wave_column(headers: &[String]) -> i32 {
    for c in WAVE_HEADERS {
        for (i, n) in headers.iter().enumerate() {
            if n == c {
                return i as i32;
            }
        }
    }
    for c in WAVE_HEADERS {
        for (i, n) in headers.iter().enumerate() {
            if n.contains(c) && !WAVE_HEADER_EXCLUDE.iter().any(|x| n.contains(x)) {
                return i as i32;
            }
        }
    }
    -1
}

/// 在表头里找第一个命中的列下标；先精确匹配，再包含匹配（应对"主播ID（必填）"）
fn find_column(headers: &[String], candidates: &[&str]) -> i32 {
    for c in candidates {
        for (i, n) in headers.iter().enumerate() {
            if n == c {
                return i as i32;
            }
        }
    }
    for c in candidates {
        for (i, n) in headers.iter().enumerate() {
            if n.contains(c) {
                return i as i32;
            }
        }
    }
    -1
}

/// 按表头猜 CSV 类型：时长 → 音浪 → 主播档案
pub fn detect_kind(headers: &[String]) -> Kind {
    let all = headers.join("");
    for kw in ["时长", "duration", "开播", "有效时长"] {
        if all.contains(kw) {
            return Kind::Duration;
        }
    }
    for kw in ["音浪", "wave", "总音浪"] {
        if all.contains(kw) {
            return Kind::Wave;
        }
    }
    for kw in ["主播", "昵称", "抖音号"] {
        if all.contains(kw) {
            return Kind::Anchors;
        }
    }
    Kind::Unknown
}

/// 解析音浪值，支持 "12.5万" / "125000" / "12,500" / "1.2亿" / "3w"
pub fn parse_wave(raw: &str) -> anyhow::Result<i64> {
    let mut s = raw.trim().replace([',', '，'], "");
    if s.is_empty() || s == "-" {
        return Ok(0);
    }
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
    Ok((v * multiplier) as i64)
}

/// 取 "37小时" 里的 37。primary 优先，其次 fallback。
fn extract_int(s: &str, primary: &str, fallback: &str) -> f64 {
    let idx = s.find(primary).or_else(|| {
        if fallback.is_empty() {
            None
        } else {
            // "分" 可能跟在 "分钟" 后面，只认独立的"分"
            s.find(fallback)
                .filter(|&i| !s[i + fallback.len()..].starts_with("钟"))
        }
    });
    let Some(idx) = idx else { return 0.0 };
    let mut start = idx;
    let bytes = s.as_bytes();
    while start > 0 && (bytes[start - 1].is_ascii_digit() || bytes[start - 1] == b'.') {
        start -= 1;
    }
    s[start..idx].trim().parse().unwrap_or(0.0)
}

/// 解析时长，统一换算为分钟。
/// 支持：2:30:45 / 1:30 / 37小时16分钟5秒 / 1小时30分 / 150分钟 / 150
pub fn parse_duration(raw: &str) -> anyhow::Result<i64> {
    let s = raw.trim().replace('：', ":");
    if s.is_empty() || s == "-" {
        return Ok(0);
    }
    // 2:30:45 或 1:30
    if s.contains(':') {
        let vals: Vec<i64> = s
            .split(':')
            .map(|p| {
                p.trim()
                    .parse()
                    .map_err(|_| anyhow::anyhow!("时长无法解析: {raw}"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        return match vals.len() {
            3 => Ok(vals[0] * 60 + vals[1] + vals[2] / 60),
            2 => Ok(vals[0] * 60 + vals[1]),
            _ => anyhow::bail!("时长无法解析: {raw}"),
        };
    }
    // 中文：37小时16分钟5秒
    if s.contains("小时") || s.contains("分钟") || s.contains("分") || s.contains("秒") {
        let hours = extract_int(&s, "小时", "时");
        let minutes = extract_int(&s, "分钟", "分");
        let seconds = extract_int(&s, "秒", "");
        return Ok((hours * 60.0 + minutes + seconds / 60.0) as i64);
    }
    // 纯数字：150分钟 或 150
    let s = s
        .trim()
        .trim_end_matches("分钟")
        .trim_end_matches("min")
        .trim();
    let v: f64 = s
        .parse()
        .map_err(|_| anyhow::anyhow!("时长无法解析: {raw}"))?;
    Ok(v as i64)
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

/// 解析整个 CSV。返回类型 + 行。
pub fn parse(text: &str) -> anyhow::Result<(Kind, Vec<CsvRow>)> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let Some(header_line) = lines.next() else {
        anyhow::bail!("CSV 是空的");
    };
    let delimiter = sniff_delimiter(header_line);
    let headers: Vec<String> = split_csv_line(header_line, delimiter)
        .iter()
        .map(|h| normalize_header(h))
        .collect();
    let kind = detect_kind(&headers);

    let id_col = find_column(&headers, &ID_HEADERS);
    let name_col = find_column(&headers, &NAME_HEADERS);
    let wave_col = find_wave_column(&headers);
    let dur_col = find_column(&headers, &DUR_HEADERS);
    let rank_col = find_column(&headers, &RANK_HEADERS);
    let douyin_col = find_column(&headers, &DOUYIN_HEADERS);

    let mut rows = Vec::new();
    for (i, line) in lines.enumerate() {
        let rec = split_csv_line(line, delimiter);
        if is_blank(&rec) {
            continue;
        }
        let get = |col: i32| -> String {
            if col >= 0 {
                rec.get(col as usize).cloned().unwrap_or_default()
            } else {
                String::new()
            }
        };
        let mut row = CsvRow {
            raw_index: i + 2, // +2：跳过表头且行号从 1 开始
            ..Default::default()
        };
        row.anchor_id = get(id_col);
        row.douyin_no = get(douyin_col);
        if row.anchor_id.is_empty() && !row.douyin_no.is_empty() {
            // 没有主播 id 列时用抖音号顶替（与 615/3328 一致）
            row.anchor_id = row.douyin_no.clone();
        }
        row.name = get(name_col);
        if wave_col >= 0 {
            match parse_wave(&get(wave_col)) {
                Ok(v) => row.wave = Some(v as f64),
                Err(e) => row.err = e.to_string(),
            }
        }
        if dur_col >= 0 {
            match parse_duration(&get(dur_col)) {
                Ok(v) => row.minutes = Some(v as f64),
                Err(e) => row.err = e.to_string(),
            }
        }
        if rank_col >= 0 && get(rank_col).parse::<i64>().is_ok() {
            // 排名列暂不消费，保留在原始行里即可
        }
        rows.push(row);
    }
    Ok((kind, rows))
}

/// 月度时长文件解析：只保留「时长」列有值的行，绝不把音浪列当数据。
///
/// 抖音公会后台导出的月度文件（如「9月份时长.csv」）表头是
/// `主播ID,主播昵称,抖音号,…,分成比-直播音浪收益,开播有效时长,主播标签`：
/// 里面有「分成比-直播音浪收益」这种含「音浪」字样的比例列，按音浪解析会把这个
/// 比例（35 / 40 / 50）当成每人的音浪值，所以月度时长走独立入口，只取时长。
pub fn parse_duration_only(text: &str) -> Vec<CsvRow> {
    match parse(text) {
        Ok((_, rows)) => rows
            .into_iter()
            .filter(|r| r.minutes.is_some() && !r.name.is_empty())
            .collect(),
        Err(_) => Vec::new(),
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
        assert_eq!(parse_wave("3w").unwrap(), 30_000);
        assert_eq!(parse_wave("1.2亿").unwrap(), 120_000_000);
        assert_eq!(parse_wave("-").unwrap(), 0);
        assert_eq!(parse_wave("").unwrap(), 0);
        assert!(parse_wave("abc").is_err());
    }

    #[test]
    fn duration_formats() {
        assert_eq!(parse_duration("2:30:45").unwrap(), 150);
        assert_eq!(parse_duration("1:30").unwrap(), 90);
        assert_eq!(parse_duration("37小时16分钟5秒").unwrap(), 37 * 60 + 16);
        assert_eq!(parse_duration("1小时30分").unwrap(), 90);
        assert_eq!(parse_duration("150分钟").unwrap(), 150);
        assert_eq!(parse_duration("150").unwrap(), 150);
        assert_eq!(parse_duration("-").unwrap(), 0);
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

    #[test]
    fn duration_only_keeps_rows_with_minutes() {
        let rows = parse_duration_only(MONTH_CSV);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "浩龙🎹");
        assert_eq!(rows[0].minutes, Some(4529.0));
        // 没有时长列 / 空文件：解析不出行，调用方据此提示「不是月度时长文件」
        assert!(parse_duration_only("主播名,音浪\n浩龙,1000\n").is_empty());
        assert!(parse_duration_only("").is_empty());
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
