//! 两个库的 SQL 方言差异集中在这里。对外统一约定：
//! - 人员表查询返回：id / name / avatar_url / status
//! - 日期查询返回：d（YYYY-MM-DD）
//! - 音浪查询返回：k（人员 id）/ w（区间内音浪合计）

/// A：douyinxs —— person / account / daily_metric
const ANCHORS_A: &str = "SELECT id, name, avatar_url, status FROM person
     WHERE deleted_at IS NULL AND (status IS NULL OR status <> 'left')
     ORDER BY name";
const DATES_A: &str =
    "SELECT DATE_FORMAT(biz_date, '%Y-%m-%d') AS d FROM daily_metric GROUP BY d ORDER BY d DESC";
const BASELINE_A: &str = "SELECT DATE_FORMAT(MIN(biz_date), '%Y-%m-%d') AS d FROM daily_metric";
const WAVES_A: &str = "SELECT person_id AS k, CAST(SUM(wave) AS DOUBLE) AS w FROM daily_metric
     WHERE wave_reliable = 1 AND DATE_FORMAT(biz_date, '%Y-%m-%d') IN (?PLACEHOLDERS?)
     GROUP BY person_id";

/// B：userxs —— persons / accounts / wave_stats（anchor_id 是关联键）
const ANCHORS_B: &str =
    "SELECT id, name, NULL AS avatar_url, 'active' AS status FROM persons ORDER BY name";
const DATES_B: &str =
    "SELECT DATE_FORMAT(date, '%Y-%m-%d') AS d FROM wave_stats GROUP BY d ORDER BY d DESC";
const WAVES_B: &str =
    "SELECT a.person_id AS k, CAST(SUM(ws.wave_value) AS DOUBLE) AS w FROM wave_stats ws
     JOIN accounts a ON a.anchor_id = ws.anchor_id
     WHERE DATE_FORMAT(ws.date, '%Y-%m-%d') IN (?PLACEHOLDERS?)
     GROUP BY a.person_id";

pub struct Dialect {
    pub anchors_sql: &'static str,
    pub dates_sql: &'static str,
    /// 基线日 SQL：首日记的是累计音浪。没有这个问题的库返回 None
    pub baseline_sql: Option<&'static str>,
    pub waves_sql: &'static str,
}

pub fn dialect_of(id: &str) -> Dialect {
    if id == "B" {
        Dialect {
            anchors_sql: ANCHORS_B,
            dates_sql: DATES_B,
            baseline_sql: None,
            waves_sql: WAVES_B,
        }
    } else {
        Dialect {
            anchors_sql: ANCHORS_A,
            dates_sql: DATES_A,
            baseline_sql: Some(BASELINE_A),
            waves_sql: WAVES_A,
        }
    }
}

/// 该库对带参数的查询会报 1835（Malformed communication packet），
/// 所以日期直接内联进 SQL。日期来自库里的 DATE_FORMAT 结果，
/// 这里再做一次字符白名单（只允许 YYYY-MM-DD）兜底。
pub fn inline_dates(sql: &str, dates: &[String]) -> anyhow::Result<String> {
    let mut quoted = Vec::with_capacity(dates.len());
    for d in dates {
        anyhow::ensure!(
            !d.is_empty() && d.len() <= 10 && d.chars().all(|c| c.is_ascii_digit() || c == '-'),
            "非法日期：{d}"
        );
        quoted.push(format!("'{d}'"));
    }
    Ok(sql.replace("?PLACEHOLDERS?", &quoted.join(",")))
}
