//! 两个库的 SQL 方言差异集中在这里。对外统一约定：
//! - 人员表查询返回：id / name / avatar_url / status
//! - 日期查询返回：d（YYYY-MM-DD）
//! - 音浪查询返回：k（人员 id）/ w（区间内音浪合计），序列查询多一个 d（日期）

/// 未播口径（2026-10-09 用户定；2026-10-10 统一三处实现，P1-10）：
/// 「当日导入音浪 < NOT_LIVE_WAVE_THRESHOLD」= 未开播。此前 report.rs（日报）按
/// is_live 标记、public_teams.rs（展示端）按 wave<3、push.rs（推送榜）按 wave>=3，
/// 三套口径同一个人会得出不同的「未播」结论。现在日报 / 展示端 / 推送一律引用这里。
pub const NOT_LIVE_WAVE_THRESHOLD: i64 = 2;

/// 统一的未播判定：当日音浪 < 2 = 未播（report 日报 / public_teams 榜单 / push 推送共用）
pub fn is_live_wave(wave: i64) -> bool {
    wave >= NOT_LIVE_WAVE_THRESHOLD
}

/// A：douyinxs —— person / account / daily_metric
const ANCHORS_A: &str = "SELECT id, name, avatar_url, status FROM person
     WHERE deleted_at IS NULL AND (status IS NULL OR status <> 'left')
     ORDER BY name";
const DATES_A: &str =
    "SELECT DATE_FORMAT(biz_date, '%Y-%m-%d') AS d FROM daily_metric GROUP BY d ORDER BY d DESC";
/// 基线日 = 最早那个「累计快照日」（wave_reliable=0：首日没有前一天可比，只能记累计）。
/// 不能直接用 MIN(biz_date)：2026-10-10 清空音浪数据后重新开始导入时，第一天是正常的
/// 当日音浪，却会被当成基线从趋势图 / 导出里剔掉（自检 export 用例踩过）。
const BASELINE_A: &str =
    "SELECT DATE_FORMAT(MIN(biz_date), '%Y-%m-%d') AS d FROM daily_metric WHERE wave_reliable = 0";
const WAVES_A: &str = "SELECT k, CAST(SUM(w) AS DOUBLE) AS w FROM (
     SELECT person_id AS k, SUM(wave) AS w FROM daily_metric
      WHERE wave_reliable = 1 AND DATE_FORMAT(biz_date, '%Y-%m-%d') IN (?PLACEHOLDERS?)
      GROUP BY person_id
     UNION ALL
     SELECT a.person_id AS k, SUM(ws.wave_value) AS w
      FROM wave_snapshot ws JOIN account a ON a.anchor_id = ws.anchor_id AND a.is_primary = 0
      WHERE DATE_FORMAT(ws.biz_date, '%Y-%m-%d') IN (?PLACEHOLDERS?)
      GROUP BY a.person_id
   ) t GROUP BY k";

/// A 库每日序列：展示页每日排名用。返回 k（人员 id）/ d（日期）/ w（当日音浪）/ live（当日是否开播）。
/// 一人多号：daily_metric 每人每天一行（主号口径），副号音浪从 wave_snapshot（is_primary=0）并进来。
/// wave_snapshot.wave_value 是当日值语义（已验证：主号快照与 daily_metric.wave 962 对中 958 相等）。
const SERIES_A: &str = "SELECT k, d, CAST(SUM(w) AS DOUBLE) AS w, \
     CASE WHEN SUM(w) >= 2 THEN 1 ELSE 0 END AS live FROM (
     SELECT person_id AS k, DATE_FORMAT(biz_date, '%Y-%m-%d') AS d, SUM(wave) AS w, MAX(is_live) AS live
      FROM daily_metric
      WHERE wave_reliable = 1 AND DATE_FORMAT(biz_date, '%Y-%m-%d') IN (?PLACEHOLDERS?)
      GROUP BY person_id, d
     UNION ALL
     SELECT a.person_id AS k, DATE_FORMAT(ws.biz_date, '%Y-%m-%d') AS d, SUM(ws.wave_value) AS w,
            CASE WHEN SUM(ws.wave_value) > 0 THEN 1 ELSE 0 END AS live
      FROM wave_snapshot ws JOIN account a ON a.anchor_id = ws.anchor_id AND a.is_primary = 0
      WHERE DATE_FORMAT(ws.biz_date, '%Y-%m-%d') IN (?PLACEHOLDERS?)
      GROUP BY a.person_id, d
   ) t GROUP BY k, d";

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

const SERIES_B: &str = "SELECT a.person_id AS k, DATE_FORMAT(ws.date, '%Y-%m-%d') AS d, CAST(SUM(ws.wave_value) AS DOUBLE) AS w, 0 AS live FROM wave_stats ws
     JOIN accounts a ON a.anchor_id = ws.anchor_id
     WHERE DATE_FORMAT(ws.date, '%Y-%m-%d') IN (?PLACEHOLDERS?)
     GROUP BY a.person_id, d";

pub struct Dialect {
    pub anchors_sql: &'static str,
    pub dates_sql: &'static str,
    /// 基线日 SQL：首日记的是累计音浪。没有这个问题的库返回 None
    pub baseline_sql: Option<&'static str>,
    pub waves_sql: &'static str,
    /// 每日音浪序列：返回 k（人员 id）/ d（日期）/ w（当日音浪）
    pub series_sql: &'static str,
}

pub fn dialect_of(id: &str) -> Dialect {
    if id == "LOCAL" {
        Dialect {
            anchors_sql: ANCHORS_A,
            dates_sql: DATES_A,
            baseline_sql: Some(BASELINE_A),
            waves_sql: WAVES_A,
            series_sql: SERIES_A,
        }
    } else if id == "B" {
        Dialect {
            anchors_sql: ANCHORS_B,
            dates_sql: DATES_B,
            baseline_sql: None,
            waves_sql: WAVES_B,
            series_sql: SERIES_B,
        }
    } else {
        Dialect {
            anchors_sql: ANCHORS_A,
            dates_sql: DATES_A,
            baseline_sql: Some(BASELINE_A),
            waves_sql: WAVES_A,
            series_sql: SERIES_A,
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

/* ------------------------- 管理端 SQL（B 阶段，LOCAL 专用） ------------------------- */
// 管理端读写在同一台本地 MariaDB（读走 pk_reader 池、写走 douyin_writer 池），
// 与 A/B 采集源无关。日期一律 DATE_FORMAT 成字符串透传（同上 1835 兼容原因，
// 值经 bind 绑定，字符串拼进来的只有日期白名单校验后的字面量）。

/// 主播台账（含主号账号信息）；aliases 在 SQLite，代码层合并。
/// wechat_bound / wechat_push 来自 push_bindings（按姓名 1:1）：运营在台账里
/// 一眼看出谁绑了微信机器人、谁开了推送，不用去「机器人」页翻会话列表。
pub const STREAMERS_SQL: &str = "SELECT p.id, p.name, p.gender, p.master_id, p.generation,
       p.group_name, p.avatar_url, p.status, p.hide_in_daily_report,
       DATE_FORMAT(p.joined_at, '%Y-%m-%d') AS joined,
       a.anchor_id, a.douyin_no, a.anchor_name, m.name AS master_name,
       (SELECT COUNT(*) FROM person c WHERE c.master_id = p.id AND c.deleted_at IS NULL) AS disciple_count,
       (SELECT GROUP_CONCAT(a2.douyin_no ORDER BY a2.is_primary DESC, a2.id SEPARATOR ' / ')
          FROM account a2 WHERE a2.person_id = p.id AND a2.douyin_no <> '') AS all_douyin_nos,
       (SELECT pb.conversation_id <> '' FROM push_bindings pb WHERE pb.name = p.name
          LIMIT 1) AS wechat_bound,
       (SELECT pb.notify_target = 1 OR pb.daily_report = 1 FROM push_bindings pb
          WHERE pb.name = p.name LIMIT 1) AS wechat_push
     FROM person p
     LEFT JOIN account a ON a.person_id = p.id AND a.is_primary = 1
     LEFT JOIN person m ON m.id = p.master_id
     WHERE p.deleted_at IS NULL
     ORDER BY p.name";

/// 按主键找主播（详情页）
pub const STREAMER_BY_ID_SQL: &str = "SELECT id, name, gender, master_id, generation,
       group_name, avatar_url, status, DATE_FORMAT(joined_at, '%Y-%m-%d') AS joined
     FROM person WHERE deleted_at IS NULL AND id = ? LIMIT 1";

/// 按名字精确找主播（含团别，导入匹配用）
pub const STREAMER_BY_NAME_SQL: &str =
    "SELECT id, name, gender FROM person WHERE deleted_at IS NULL AND name = ? LIMIT 1";

/// 抖音号占用检查（跨 person 唯一）
pub const DOUYIN_NO_TAKEN_SQL: &str =
    "SELECT p.id, p.name FROM account a JOIN person p ON p.id = a.person_id
     WHERE a.douyin_no = ? AND p.deleted_at IS NULL LIMIT 1";

/// 上一个业务日的累计快照（导入差分用）
pub const PREV_CUMULATIVE_SQL: &str = "SELECT cumulative_wave,
       DATE_FORMAT(biz_date, '%Y-%m-%d') AS d
     FROM daily_metric
     WHERE person_id = ? AND biz_date < ?
     ORDER BY biz_date DESC
     LIMIT 1";

/// 主号 anchor_id（daily_metric.anchor_id NOT NULL，导入时回填）
pub const PRIMARY_ANCHOR_SQL: &str =
    "SELECT anchor_id FROM account WHERE person_id = ? AND is_primary = 1 LIMIT 1";

/// 同团同日已有数据行数（reject 模式 / 冲突提示）
pub const TEAM_DATE_COUNT_SQL: &str = "SELECT COUNT(*) FROM daily_metric d
     JOIN person p ON p.id = d.person_id
     WHERE DATE_FORMAT(d.biz_date, '%Y-%m-%d') = ? AND p.gender = ?";

/// 不分男女团：该业务日已入库多少行（导入不分团别时用）
pub const DATE_COUNT_SQL: &str =
    "SELECT COUNT(*) FROM daily_metric WHERE DATE_FORMAT(biz_date, '%Y-%m-%d') = ?";

/// 不分男女团：该业务日还没有数据的在职主播（跨男/女团）。
/// 已离开（status='left'，如狼宸 / 狼森）不再要求当天有数据 —— 否则每天都被报「缺数据」。
/// 榜单隐藏（hide_in_daily_report=1，P2-14）的也不报 —— 完整度只看会出现在日报里的人。
pub const MISSING_ALL_SQL: &str = "SELECT p.name FROM person p
     LEFT JOIN daily_metric d ON d.person_id = p.id
       AND DATE_FORMAT(d.biz_date, '%Y-%m-%d') = ?
     WHERE p.deleted_at IS NULL AND p.status <> 'left' AND p.hide_in_daily_report = 0
       AND d.id IS NULL
     ORDER BY p.name";

/// 导入批次列表（最近 50 条）
pub const BATCH_LIST_SQL: &str = "SELECT id, source, filename,
       DATE_FORMAT(biz_date, '%Y-%m-%d') AS bd, team, imported_rows, ok_rows, failed_rows, note,
       DATE_FORMAT(created_at, '%Y-%m-%d %H:%i:%s') AS ts
     FROM import_batch ORDER BY id DESC LIMIT 50";

/// 窗口内逐日指标（dashboard 趋势/未播/TopN 数据源）；日期已过白名单校验后内联
pub fn trend_sql(start: &str, end: &str) -> String {
    format!(
        "SELECT p.id AS person_id, p.name, p.gender, \
         DATE_FORMAT(d.biz_date, '%Y-%m-%d') AS d, \
         CAST(SUM(d.wave) AS SIGNED) AS w, CAST(SUM(d.minutes) AS SIGNED) AS m, \
         CASE WHEN SUM(d.wave) >= 2 THEN 1 ELSE 0 END AS live \
         FROM daily_metric d JOIN person p ON p.id = d.person_id \
         WHERE p.deleted_at IS NULL AND d.wave_reliable = 1 \
           AND d.biz_date BETWEEN '{start}' AND '{end}' \
         GROUP BY p.id, p.name, p.gender, d ORDER BY d"
    )
}

/// 月 / 年聚合的公共骨架（2026-10-10 月度时长导入）。
///
/// 时长口径（用户口径）：某月**有导入的整月时长**（monthly_duration，平台月报）就用它，
/// 没有（历史月份 / 只有日明细）才回落到 daily_metric 求和 —— 所以必须先把日明细聚合到
/// 「每人每月」中间表 t，再按人合计；直接 SUM(d.minutes) 会把月报口径丢掉。
///
/// 中间表 t 的列：pid / m（YYYY-MM）/ w（音浪合计）/ dm（日明细时长求和）/
/// live（开播天数）/ days（有数据天数）。
///
/// - `select`：要吐出的列；时长那一列写 `{minutes}` 占位，由本函数决定口径
///   （with_monthly = true → `COALESCE(md.minutes, t.dm)`，false → `t.dm`）
/// - `like`：`2026-09%`（月）或 `2026%`（年），调用前已过字符白名单
/// - `team`：male / female / 空（空 = 不限团别）
/// - `extra_where`：额外过滤（如公开端的 `AND p.status <> 'left'`），已内联
/// - `order_by`：排序键（后面固定跟 `ORDER BY`），一般写 `wave DESC`
/// - `with_monthly`：是否带上 monthly_duration（库没跑 007 迁移时只能 false）
pub fn period_rollup_sql(
    select: &str,
    like: &str,
    team: &str,
    extra_where: &str,
    order_by: &str,
    with_monthly: bool,
) -> String {
    let team_clause = if team.is_empty() {
        String::new()
    } else {
        format!(" AND p.gender = '{team}'")
    };
    let (minutes_expr, monthly_join) = if with_monthly {
        (
            "COALESCE(md.minutes, t.dm)",
            " \
         LEFT JOIN monthly_duration md ON md.person_id = t.pid AND md.period = t.m",
        )
    } else {
        ("t.dm", "")
    };
    let select = select.replace("{minutes}", minutes_expr);
    format!(
        "SELECT {select} FROM ( \
           SELECT d.person_id AS pid, DATE_FORMAT(d.biz_date, '%Y-%m') AS m, \
             SUM(d.wave) AS w, SUM(d.minutes) AS dm, \
             SUM(CASE WHEN d.wave >= 2 THEN 1 ELSE 0 END) AS live, \
             COUNT(DISTINCT d.biz_date) AS days \
           FROM daily_metric d \
           WHERE d.wave_reliable = 1 \
             AND DATE_FORMAT(d.biz_date, '%Y-%m') LIKE '{like}' \
           GROUP BY pid, m \
         ) t \
         JOIN person p ON p.id = t.pid{monthly_join} \
         WHERE p.deleted_at IS NULL{team_clause}{extra_where} \
         GROUP BY p.id, p.name, p.gender \
         ORDER BY {order_by}"
    )
}

/// 管理端月 / 年聚合要吐的列（`{minutes}` 由 period_rollup_sql 按口径替换）
pub const ADMIN_PERIOD_SELECT: &str = "p.id AS person_id, p.name, p.gender, \
     CAST(SUM(t.w) AS SIGNED) AS wave, \
     CAST(SUM({minutes}) AS SIGNED) AS minutes, \
     CAST(SUM(t.live) AS SIGNED) AS live_days, \
     CAST(SUM(t.days) AS SIGNED) AS data_days";

/// 错误是不是「monthly_duration 表不存在」（库还没跑 007 迁移）
pub fn is_missing_month_table(e: &anyhow::Error) -> bool {
    let m = e.to_string();
    m.contains("monthly_duration")
        || m.contains("doesn't exist")
        || m.contains("does not exist")
        || m.contains("Unknown table")
}

/// 月 / 年聚合查询：先按「导入的整月时长优先」查，库还没建 monthly_duration（007 迁移没跑）时
/// 退回纯日明细口径 —— 别让少一张表把整个展示页/数据页打不开。
pub async fn query_period(
    pools: &crate::mysql::Pools,
    source: &str,
    select: &str,
    like: &str,
    team: &str,
    extra_where: &str,
    order_by: &str,
) -> anyhow::Result<Vec<sqlx::mysql::MySqlRow>> {
    let sql = period_rollup_sql(select, like, team, extra_where, order_by, true);
    match crate::mysql::query(pools, source, &sql).await {
        Ok(rows) => Ok(rows),
        Err(e) if is_missing_month_table(&e) => {
            tracing::warn!(
                "monthly_duration 表不存在，月/年时长按日明细口径回落（请先跑 pk-server migrate）"
            );
            let sql = period_rollup_sql(select, like, team, extra_where, order_by, false);
            crate::mysql::query(pools, source, &sql).await
        }
        Err(e) => Err(e),
    }
}

/// 某主播的全部账号（详情页）
pub const ACCOUNTS_BY_PERSON_SQL: &str = "SELECT id, anchor_id, douyin_no, anchor_name,
       is_primary, status, DATE_FORMAT(created_at, '%Y-%m-%d') AS created
     FROM account WHERE person_id = ? ORDER BY is_primary DESC, id";

/// 同名主播分组（重复检测）
pub const DUP_NAMES_SQL: &str = "SELECT name, COUNT(*) AS n,
       GROUP_CONCAT(id ORDER BY id) AS ids
     FROM person WHERE deleted_at IS NULL GROUP BY name HAVING n > 1";

/// 同抖音号（重复检测）
pub const DUP_DOUYIN_SQL: &str = "SELECT a.douyin_no, COUNT(*) AS n,
       GROUP_CONCAT(p.id ORDER BY p.id) AS ids, GROUP_CONCAT(p.name ORDER BY p.id) AS names
     FROM account a JOIN person p ON p.id = a.person_id
     WHERE p.deleted_at IS NULL AND a.douyin_no <> ''
     GROUP BY a.douyin_no HAVING n > 1";

/// 团内当天缺数据的主播（导入完整度报告）；已离开 / 榜单隐藏的不报（与 MISSING_ALL_SQL 同口径）
pub const MISSING_SQL: &str = "SELECT p.name FROM person p
     LEFT JOIN daily_metric d ON d.person_id = p.id
       AND DATE_FORMAT(d.biz_date, '%Y-%m-%d') = ?
     WHERE p.deleted_at IS NULL AND p.status <> 'left' AND p.hide_in_daily_report = 0 \
       AND p.gender = ? AND d.id IS NULL
     ORDER BY p.name";

/// 沿师傅链向上走（防环校验用）
pub const MASTER_CHAIN_SQL: &str =
    "SELECT id, master_id FROM person WHERE deleted_at IS NULL AND id = ?";

/// 设置/清除师傅（master_id 自引用 person.id；传 NULL 即清除）
pub const SET_MASTER_SQL: &str =
    "UPDATE person SET master_id = ? WHERE id = ? AND deleted_at IS NULL";

/// 抖音号 → person 映射（导入按抖音号匹配；一人多账号多号归一）
pub const ACCOUNT_DOUYIN_MAP_SQL: &str =
    "SELECT person_id, douyin_no FROM account WHERE douyin_no <> ''";

/// 采集 ID（anchor_id）→ person 映射。运营文件里的「主播id / uid / anchor_id」列
/// 走这条路认人（用户口径 2026-10-10：只要有抖音号或者 id 都能认）。
pub const ACCOUNT_ANCHOR_MAP_SQL: &str =
    "SELECT person_id, anchor_id FROM account WHERE anchor_id <> ''";

/// 暂存区列表（未认领的导入行）。不进任何榜单/统计，只给管理端处理用。
pub const PENDING_LIST_SQL: &str = "SELECT id, name, douyin_no,
       DATE_FORMAT(biz_date, '%Y-%m-%d') AS bd, wave, cumulative_wave, minutes, is_live,
       source_batch_id, DATE_FORMAT(created_at, '%Y-%m-%d %H:%i') AS ts
     FROM import_pending WHERE absorbed_at IS NULL
     ORDER BY biz_date DESC, id DESC LIMIT 300";

/// 某个主播名下（按抖音号或采集 ID 命中）还没认领的暂存行 —— 建档 / 绑号后补账用
pub const PENDING_FOR_PERSON_SQL: &str = "SELECT id, name, douyin_no,
       DATE_FORMAT(biz_date, '%Y-%m-%d') AS bd, wave, cumulative_wave, minutes, is_live,
       source_batch_id
     FROM import_pending
     WHERE absorbed_at IS NULL
       AND (douyin_no IN (SELECT a.douyin_no FROM account a
                            WHERE a.person_id = ? AND a.douyin_no <> '')
         OR douyin_no IN (SELECT a.anchor_id FROM account a
                            WHERE a.person_id = ? AND a.anchor_id <> ''))
     ORDER BY biz_date, id";

/// 在册主播总数（未删除且未离开）
pub const ACTIVE_PERSON_COUNT_SQL: &str =
    "SELECT COUNT(*) AS n FROM person WHERE deleted_at IS NULL AND status <> 'left'";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_uses_douyin_schema() {
        let dialect = dialect_of("LOCAL");
        assert!(dialect.anchors_sql.contains("FROM person"));
        assert!(dialect.waves_sql.contains("FROM daily_metric"));
        assert!(dialect.baseline_sql.is_some());
    }

    #[test]
    fn admin_sql_targets_local_schema() {
        assert!(STREAMERS_SQL.contains("person p"));
        assert!(STREAMERS_SQL.contains("gender"));
        assert!(PREV_CUMULATIVE_SQL.contains("cumulative_wave"));
        assert!(BATCH_LIST_SQL.contains("ok_rows"), "002 迁移后的审计字段");
    }
}
