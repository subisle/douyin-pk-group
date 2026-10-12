//! SQL 方言集中地：业务只读查询（person / daily_metric）一套，管理端（LOCAL）一套。
//! 对外统一约定：
//! - 人员表查询返回：id / name / avatar_url / status
//! - 日期查询返回：d（YYYY-MM-DD）
//! - 音浪查询返回：k（人员 id）/ w（区间内音浪合计），序列查询多一个 d（日期）
//!
//! 数据源只剩 LOCAL（唯一 PostgreSQL 业务库），共用同一套只读 SQL。

/// 未播口径（2026-10-09 用户定；2026-10-10 统一三处实现，P1-10）：
/// 「当日导入音浪 < NOT_LIVE_WAVE_THRESHOLD」= 未开播。此前 report.rs（日报）按
/// is_live 标记、public_teams.rs（展示端）按 wave<3、push.rs（推送榜）按 wave>=3，
/// 三套口径同一个人会得出不同的「未播」结论。现在日报 / 展示端 / 推送一律引用这里。
///
/// ⚠️ 口径边界（2026-10-11 用户确认，别当 bug 修）：**当天没有导入行的，就是「没导入」**，
/// 不算未播天。所有未播聚合都只扫 `daily_metric` 里**存在**的行：
/// - 有行且 `wave < 2`  → 未播天 +1
/// - 有行且 `wave >= 2` → 已播
/// - **完全没有行**      → 不参与统计，既不加也不判
///
/// 所以 `absentDays` 的准确语义是「当月**有数据行**且音浪 < 2 的天数」，
/// 不是「当月日历天里没播的天数」。缺采的日子不算进去，这是有意的：
/// 漏采集 ≠ 没开播，把两者混在一起会让"运营忘了导"看起来像"主播没播"。
pub const NOT_LIVE_WAVE_THRESHOLD: i64 = 2;

/// 统一的未播判定：当日音浪 < 2 = 未播（report 日报 / public_teams 榜单 / push 推送共用）
pub fn is_live_wave(wave: i64) -> bool {
    wave >= NOT_LIVE_WAVE_THRESHOLD
}

/// 「当月未播天数」的唯一 SQL 模板：`{month}` 占位为 `YYYY-MM`。
///
/// 日报（report.rs）与展示端榜单（public_teams.rs）以前各抄一份、逐字符相同。
/// 抄两份就等于埋两颗雷：哪天有人只改一处，同一个人的未播天数在两个页面
/// 就不一样了。抽成常量后，口径只有这一处，改哪边都跟着动。
///
/// 语义严格对应上面的口径注释：**只扫存在的行**。所以是
/// `SUM(CASE WHEN wave < 阈值 THEN 1 ELSE 0 END)`，不按日历天补零。
pub const MONTH_ABSENT_SQL: &str =
    "SELECT person_id, (SUM(CASE WHEN wave < {NOT_LIVE_WAVE_THRESHOLD} THEN 1 ELSE 0 END))::BIGINT AS n \
     FROM daily_metric WHERE wave_reliable = 1 \
       AND TO_CHAR(biz_date, 'YYYY-MM') = '{month}' GROUP BY person_id";

/// 生成「当月未播天数」的 SQL。month 必须是 `YYYY-MM`（调用方已过 `valid_date_*`），
/// 这里只做字符串替换，不拼任何外部输入。
pub fn month_absent_sql(month: &str) -> String {
    MONTH_ABSENT_SQL
        .replace(
            "{NOT_LIVE_WAVE_THRESHOLD}",
            &NOT_LIVE_WAVE_THRESHOLD.to_string(),
        )
        .replace("{month}", month)
}

/// LOCAL：唯一业务库 —— person / account / daily_metric
const ANCHORS_A: &str = "SELECT id, name, avatar_url, status FROM person
     WHERE deleted_at IS NULL AND (status IS NULL OR status <> 'left')
     ORDER BY name";
const DATES_A: &str =
    "SELECT TO_CHAR(biz_date, 'YYYY-MM-DD') AS d FROM daily_metric GROUP BY d ORDER BY d DESC";
/// 基线日 = 最早那个「累计快照日」（wave_reliable=0：首日没有前一天可比，只能记累计）。
/// 不能直接用 MIN(biz_date)：2026-10-10 清空音浪数据后重新开始导入时，第一天是正常的
/// 当日音浪，却会被当成基线从趋势图 / 导出里剔掉（自检 export 用例踩过）。
const BASELINE_A: &str =
    "SELECT TO_CHAR(MIN(biz_date), 'YYYY-MM-DD') AS d FROM daily_metric WHERE wave_reliable = 0";
const WAVES_A: &str = "SELECT k, (SUM(w))::DOUBLE PRECISION AS w FROM (
     SELECT person_id AS k, SUM(wave) AS w FROM daily_metric
      WHERE wave_reliable = 1 AND TO_CHAR(biz_date, 'YYYY-MM-DD') IN (?PLACEHOLDERS?)
      GROUP BY person_id
     UNION ALL
     SELECT a.person_id AS k, SUM(ws.wave_value) AS w
      FROM wave_snapshot ws JOIN account a ON a.anchor_id = ws.anchor_id AND a.is_primary = 0
      WHERE TO_CHAR(ws.biz_date, 'YYYY-MM-DD') IN (?PLACEHOLDERS?)
      GROUP BY a.person_id
   ) t GROUP BY k";

/// A 库每日序列：展示页每日排名用。返回 k（人员 id）/ d（日期）/ w（当日音浪）/ live（当日是否开播）。
/// 一人多号：daily_metric 每人每天一行（主号口径），副号音浪从 wave_snapshot（is_primary=0）并进来。
/// wave_snapshot.wave_value 是当日值语义（已验证：主号快照与 daily_metric.wave 962 对中 958 相等）。
const SERIES_A: &str = "SELECT k, d, (SUM(w))::DOUBLE PRECISION AS w, \
     CASE WHEN SUM(w) >= 2 THEN 1 ELSE 0 END AS live FROM (
     SELECT person_id AS k, TO_CHAR(biz_date, 'YYYY-MM-DD') AS d, SUM(wave) AS w, MAX(is_live) AS live
      FROM daily_metric
      WHERE wave_reliable = 1 AND TO_CHAR(biz_date, 'YYYY-MM-DD') IN (?PLACEHOLDERS?)
      GROUP BY person_id, d
     UNION ALL
     SELECT a.person_id AS k, TO_CHAR(ws.biz_date, 'YYYY-MM-DD') AS d, SUM(ws.wave_value) AS w,
            CASE WHEN SUM(ws.wave_value) > 0 THEN 1 ELSE 0 END AS live
      FROM wave_snapshot ws JOIN account a ON a.anchor_id = ws.anchor_id AND a.is_primary = 0
      WHERE TO_CHAR(ws.biz_date, 'YYYY-MM-DD') IN (?PLACEHOLDERS?)
      GROUP BY a.person_id, d
   ) t GROUP BY k, d";

pub struct Dialect {
    pub anchors_sql: &'static str,
    pub dates_sql: &'static str,
    /// 基线日 SQL：首日记的是累计音浪。没有这个问题的库返回 None
    pub baseline_sql: Option<&'static str>,
    pub waves_sql: &'static str,
    /// 每日音浪序列：返回 k（人员 id）/ d（日期）/ w（当日音浪）
    pub series_sql: &'static str,
}

/// 数据源 → 方言。LOCAL 与 A 的 schema 一致（person / daily_metric），共用同一套 SQL；
/// 参数 `_id` 保留只为日志与将来分叉，当前两个源返回同一份常量。
pub fn dialect_of(_id: &str) -> Dialect {
    Dialect {
        anchors_sql: ANCHORS_A,
        dates_sql: DATES_A,
        baseline_sql: Some(BASELINE_A),
        waves_sql: WAVES_A,
        series_sql: SERIES_A,
    }
}

/// 该库对带参数的查询会报 1835（Malformed communication packet），
/// 所以日期直接内联进 SQL。日期来自库里的 TO_CHAR 结果，
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

/* ------------------------- 管理端 SQL（LOCAL 专用） ------------------------- */
// 管理端读写在同一台本地 PostgreSQL（读走 pk_reader 池、写走 douyin_writer 池），
// 日期按 PostgreSQL 口径格式化成字符串透传（值经 bind 绑定，字符串拼进来的
// 值经 bind 绑定，字符串拼进来的只有日期白名单校验后的字面量）。

/// 主播台账（含主号账号信息）；aliases 在业务库里，代码层合并。
/// wechat_bound / wechat_push 来自 push_bindings（按姓名 1:1）：运营在台账里
/// 一眼看出谁绑了微信机器人、谁开了推送，不用去「机器人」页翻会话列表。
/// ⚠️ 016 迁移后新增 `p.phone` 与 `a.has_account`。库**没跑 016** 时这两列不存在 →
/// 查询报 Unknown column，`admin::streamers` 会退回 [`STREAMERS_SQL_LEGACY`]（同 admin.rs:728
/// 的导入匹配路径，`load_local_streamers` 也要用同一套兜底）。改这里务必同步改 LEGACY 版本。
pub const STREAMERS_SQL: &str = "SELECT p.id, p.name, p.gender, p.master_id, p.generation,
       p.group_name, p.avatar_url, p.status, p.hide_in_daily_report, p.phone,
       TO_CHAR(p.joined_at, 'YYYY-MM-DD') AS joined,
       (SELECT COUNT(*) FROM my_accounts ma WHERE ma.person_id = p.id) AS has_account,
       a.anchor_id, a.douyin_no, a.anchor_name, m.name AS master_name,
       (SELECT COUNT(*) FROM person c WHERE c.master_id = p.id AND c.deleted_at IS NULL) AS disciple_count,
       (SELECT string_agg(a2.douyin_no, ' / ' ORDER BY a2.is_primary DESC, a2.id)
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

/// 016 / 015 未跑时的兜底：列清单逐项对齐 [`STREAMERS_SQL`]，少 `p.phone` 与 `has_account`
/// （`my_accounts.person_id` 是 015 加的）。台账页在这套库上只是看不到手机号 / 开号状态，
/// 其余字段与今天完全一致（这是 015 定下的"向后兼容：老库一行代码不改也能跑"口径）。
pub const STREAMERS_SQL_LEGACY: &str = "SELECT p.id, p.name, p.gender, p.master_id, p.generation,
       p.group_name, p.avatar_url, p.status, p.hide_in_daily_report,
       TO_CHAR(p.joined_at, 'YYYY-MM-DD') AS joined,
       a.anchor_id, a.douyin_no, a.anchor_name, m.name AS master_name,
       (SELECT COUNT(*) FROM person c WHERE c.master_id = p.id AND c.deleted_at IS NULL) AS disciple_count,
       (SELECT string_agg(a2.douyin_no, ' / ' ORDER BY a2.is_primary DESC, a2.id)
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
/// 016 起带 `phone`；库没跑 016 时用 [`STREAMER_BY_ID_SQL_LEGACY`]（admin.rs 两处详情/设师傅共用）
pub const STREAMER_BY_ID_SQL: &str = "SELECT id, name, gender, master_id, generation,
       group_name, avatar_url, status, phone,
       TO_CHAR(joined_at, 'YYYY-MM-DD') AS joined
     FROM person WHERE deleted_at IS NULL AND id = ? LIMIT 1";

/// 016 未跑（person 表还没有 phone 列）时的兜底：列清单与 STREAMER_BY_ID_SQL 逐项对齐，
/// 只少 phone —— 调用方 read_opt_str(row,"phone") 取不到就当"未登记"，行为与空串一致。
pub const STREAMER_BY_ID_SQL_LEGACY: &str = "SELECT id, name, gender, master_id, generation,
       group_name, avatar_url, status,
       TO_CHAR(joined_at, 'YYYY-MM-DD') AS joined
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
       TO_CHAR(biz_date, 'YYYY-MM-DD') AS d
     FROM daily_metric
     WHERE person_id = ? AND biz_date < ?::date
     ORDER BY biz_date DESC
     LIMIT 1";

/// 主号 anchor_id（daily_metric.anchor_id NOT NULL，导入时回填）
pub const PRIMARY_ANCHOR_SQL: &str =
    "SELECT anchor_id FROM account WHERE person_id = ? AND is_primary = 1 LIMIT 1";

/// 同团同日已有数据行数（reject 模式 / 冲突提示）
pub const TEAM_DATE_COUNT_SQL: &str = "SELECT COUNT(*) FROM daily_metric d
     JOIN person p ON p.id = d.person_id
     WHERE TO_CHAR(d.biz_date, 'YYYY-MM-DD') = ? AND p.gender = ?";

/// 不分男女团：该业务日已入库多少行（导入不分团别时用）
pub const DATE_COUNT_SQL: &str =
    "SELECT COUNT(*) FROM daily_metric WHERE TO_CHAR(biz_date, 'YYYY-MM-DD') = ?";

/// 不分男女团：该业务日还没有数据的在职主播（跨男/女团）。
/// 已离开（status='left'，如狼宸 / 狼森）不再要求当天有数据 —— 否则每天都被报「缺数据」。
/// 榜单隐藏（hide_in_daily_report=1，P2-14）的也不报 —— 完整度只看会出现在日报里的人。
pub const MISSING_ALL_SQL: &str = "SELECT p.name FROM person p
     LEFT JOIN daily_metric d ON d.person_id = p.id
       AND TO_CHAR(d.biz_date, 'YYYY-MM-DD') = ?
     WHERE p.deleted_at IS NULL AND p.status <> 'left' AND p.hide_in_daily_report = 0
       AND d.id IS NULL
     ORDER BY p.name";

/// 导入批次列表（最近 50 条）
pub const BATCH_LIST_SQL: &str = "SELECT id, source, filename,
       TO_CHAR(biz_date, 'YYYY-MM-DD') AS bd, team, imported_rows, ok_rows, failed_rows, note,
       TO_CHAR(created_at, 'YYYY-MM-DD HH24:MI:SS') AS ts
     FROM import_batch ORDER BY id DESC LIMIT 50";

/// 窗口内逐日指标（dashboard 趋势/未播/TopN 数据源）；日期已过白名单校验后内联
pub fn trend_sql(start: &str, end: &str) -> String {
    format!(
        "SELECT p.id AS person_id, p.name, p.gender, \
         TO_CHAR(d.biz_date, 'YYYY-MM-DD') AS d, \
         (SUM(d.wave))::BIGINT AS w, (SUM(d.minutes))::BIGINT AS m, \
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
           SELECT d.person_id AS pid, TO_CHAR(d.biz_date, 'YYYY-MM') AS m, \
             SUM(d.wave) AS w, SUM(d.minutes) AS dm, \
             SUM(CASE WHEN d.wave >= 2 THEN 1 ELSE 0 END) AS live, \
             SUM(CASE WHEN d.wave < 2 THEN 1 ELSE 0 END) AS off, \
             COUNT(DISTINCT d.biz_date) AS days \
           FROM daily_metric d \
           WHERE d.wave_reliable = 1 \
             AND TO_CHAR(d.biz_date, 'YYYY-MM') LIKE '{like}' \
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
     (SUM(t.w))::BIGINT AS wave, \
     (SUM({minutes}))::BIGINT AS minutes, \
     (SUM(t.live))::BIGINT AS live_days, \
     (SUM(t.days))::BIGINT AS data_days";

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
    pools: &crate::pgrs::Pools,
    source: &str,
    select: &str,
    like: &str,
    team: &str,
    extra_where: &str,
    order_by: &str,
) -> anyhow::Result<Vec<sqlx::postgres::PgRow>> {
    let sql = period_rollup_sql(select, like, team, extra_where, order_by, true);
    match crate::pgrs::query(pools, source, &sql).await {
        Ok(rows) => Ok(rows),
        Err(e) if is_missing_month_table(&e) => {
            tracing::warn!(
                "monthly_duration 表不存在，月/年时长按日明细口径回落（请先跑 pk-server migrate）"
            );
            let sql = period_rollup_sql(select, like, team, extra_where, order_by, false);
            crate::pgrs::query(pools, source, &sql).await
        }
        Err(e) => Err(e),
    }
}

/// 某主播的全部账号（详情页）
pub const ACCOUNTS_BY_PERSON_SQL: &str = "SELECT id, anchor_id, douyin_no, anchor_name,
       is_primary, status, TO_CHAR(created_at, 'YYYY-MM-DD') AS created
     FROM account WHERE person_id = ? ORDER BY is_primary DESC, id";

/// 同名主播分组（重复检测）
pub const DUP_NAMES_SQL: &str = "SELECT name, COUNT(*) AS n,
       string_agg(id::text, ',' ORDER BY id) AS ids
     FROM person WHERE deleted_at IS NULL GROUP BY name HAVING COUNT(*) > 1";

/// 同抖音号（重复检测）
pub const DUP_DOUYIN_SQL: &str = "SELECT a.douyin_no, COUNT(*) AS n,
       string_agg(p.id::text, ',' ORDER BY p.id) AS ids, string_agg(p.name, ',' ORDER BY p.id) AS names
     FROM account a JOIN person p ON p.id = a.person_id
     WHERE p.deleted_at IS NULL AND a.douyin_no <> ''
     GROUP BY a.douyin_no HAVING COUNT(*) > 1";

/// 团内当天缺数据的主播（导入完整度报告）；已离开 / 榜单隐藏的不报（与 MISSING_ALL_SQL 同口径）
pub const MISSING_SQL: &str = "SELECT p.name FROM person p
     LEFT JOIN daily_metric d ON d.person_id = p.id
       AND TO_CHAR(d.biz_date, 'YYYY-MM-DD') = ?
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
       TO_CHAR(biz_date, 'YYYY-MM-DD') AS bd, wave, cumulative_wave, minutes, is_live,
       source_batch_id, TO_CHAR(created_at, 'YYYY-MM-DD HH24:MI') AS ts
     FROM import_pending WHERE absorbed_at IS NULL
     ORDER BY biz_date DESC, id DESC LIMIT 300";

/// 某个主播名下（按抖音号或采集 ID 命中）还没认领的暂存行 —— 建档 / 绑号后补账用
pub const PENDING_FOR_PERSON_SQL: &str = "SELECT id, name, douyin_no,
       TO_CHAR(biz_date, 'YYYY-MM-DD') AS bd, wave, cumulative_wave, minutes, is_live,
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

/* --------------------- 主播手机号 + 批量开号（016，docs/13） --------------------- */

/// 手机号被别的在职主播占了？返回抢占者的 id / name（排自己）。
/// 与 `uk_person_phone_uniq`（016 生成的软唯一索引）互补：索引兜底，这条只为给出"被谁占了"的中文文案。
/// `deleted_at IS NULL` 与台账页主查询同口径（已离职的人不再占位，手机号可以给新人）。
pub const PERSON_PHONE_TAKEN_SQL: &str = "SELECT id, name FROM person
     WHERE phone <> '' AND phone = ? AND deleted_at IS NULL AND id <> ? LIMIT 1";

/// 批量开号前的在职主播快照：只要 person_id + 姓名 + 手机号 + 团别。
/// 手机号为空也能开（登录名 = 姓名，密码必须显式给），只是开了之后不能走"手机号登录 / 忘记密码"。
/// `status <> 'left'` 与导入完整度报告同口径：已离团的不给开号（开了也登不进来，见 store::bind_inner）。
pub const PERSON_ROSTER_FOR_ACCOUNTS_SQL: &str = "SELECT id, name, gender, phone FROM person
      WHERE deleted_at IS NULL AND status <> 'left' AND name <> ''
      ORDER BY name";

/// 016 没跑（`person.phone` 不存在）时的兜底：少 phone 一列，其余逐项对齐。
/// 批量开号在这套库上仍可用（手机号一律当"未登记"，靠 `defaultPass` 开号），
/// 只是台账上的手机号没法自动带出来 —— 迁移只铺路、不强依赖。
pub const PERSON_ROSTER_FOR_ACCOUNTS_SQL_LEGACY: &str = "SELECT id, name, gender FROM person
      WHERE deleted_at IS NULL AND status <> 'left' AND name <> ''
      ORDER BY name";

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

    /// 016：台账 SQL 与它的兜底版必须**逐列同构** —— 只差 `p.phone` 与 `has_account`。
    /// 有人在 016 版里加了一列（比如新加个 wechat_openid）却忘了同步 LEGACY，
    /// 生产库没跑迁移时台账页就会因为这个字段 500，而不是"少显示一列"。
    #[test]
    fn streamers_legacy_sql_is_a_subset_of_016() {
        for must_have in [
            "p.id",
            "p.name",
            "p.gender",
            "p.master_id",
            "p.generation",
            "p.group_name",
            "p.avatar_url",
            "p.status",
            "p.hide_in_daily_report",
            "a.anchor_id",
            "a.douyin_no",
            "a.anchor_name",
            "master_name",
            "disciple_count",
            "all_douyin_nos",
            "wechat_bound",
            "wechat_push",
            "FROM person p",
            "WHERE p.deleted_at IS NULL",
            "ORDER BY p.name",
        ] {
            assert!(STREAMERS_SQL.contains(must_have), "016 版缺 {must_have}");
            assert!(
                STREAMERS_SQL_LEGACY.contains(must_have),
                "LEGACY 版缺 {must_have}（016 未跑时的兜底必须同构）"
            );
        }
        assert!(STREAMERS_SQL.contains("p.phone"), "016 版要带手机号");
        assert!(STREAMERS_SQL.contains("has_account"), "016 版要带开号状态");
        assert!(
            !STREAMERS_SQL_LEGACY.contains("p.phone"),
            "LEGACY 版不能带 phone（person 表没这列）"
        );
    }

    /// PERSON_ROSTER_FOR_ACCOUNTS_SQL 必须过只读守卫（它走 pk_reader 只读池）
    #[test]
    fn batch_open_sql_is_readonly_and_scoped() {
        assert!(PERSON_ROSTER_FOR_ACCOUNTS_SQL.contains("deleted_at IS NULL"));
        assert!(
            PERSON_ROSTER_FOR_ACCOUNTS_SQL.contains("status <> 'left'"),
            "已离团的不给开号（登不进来，见 store::bind_inner）"
        );
        assert!(PERSON_PHONE_TAKEN_SQL.contains("deleted_at IS NULL"));
        // LEGACY 版只少 phone，其余与 016 版逐字一致（改一处必须改两处）
        for must_have in [
            "SELECT id, name, gender",
            "FROM person",
            "deleted_at IS NULL",
            "status <> 'left'",
            "name <> ''",
            "ORDER BY name",
        ] {
            assert!(
                PERSON_ROSTER_FOR_ACCOUNTS_SQL.contains(must_have),
                "016 版缺 {must_have}"
            );
            assert!(
                PERSON_ROSTER_FOR_ACCOUNTS_SQL_LEGACY.contains(must_have),
                "LEGACY 版缺 {must_have}"
            );
        }
        assert!(PERSON_ROSTER_FOR_ACCOUNTS_SQL.contains("phone"));
        assert!(!PERSON_ROSTER_FOR_ACCOUNTS_SQL_LEGACY.contains("phone"));
    }

    // ---- 未播口径（2026-10-09 用户定：当日音浪 < 2 = 未播）----
    // 这三处曾经各用各的判据，导致同一个人被判出不同结论。测试守住常量和边界，
    // 让任何人想改口径都必须先改这里。

    #[test]
    fn not_live_threshold_is_two() {
        assert_eq!(NOT_LIVE_WAVE_THRESHOLD, 2, "业务口径：当日音浪 < 2 = 未播");
    }

    #[test]
    fn is_live_wave_boundary() {
        // 严格小于 2 才算未播：1 未播，2 播了
        assert!(!is_live_wave(0), "0 未播");
        assert!(!is_live_wave(1), "1 未播（阈值边界）");
        assert!(is_live_wave(2), "2 已播（阈值边界）");
        assert!(is_live_wave(3), "3 已播");
        assert!(is_live_wave(1_000_000), "大额音浪已播");
    }

    #[test]
    fn is_live_wave_handles_negative() {
        // 表里 wave 是有符号整型；脏数据（负数）不该被当成「已播」
        assert!(!is_live_wave(-1), "负数视为未播");
        assert!(!is_live_wave(i64::MIN), "极小值视为未播");
    }

    #[test]
    fn is_live_wave_matches_threshold_constant() {
        // 防止有人只改常量不改函数（或反过来），两边必须始终自洽
        assert!(!is_live_wave(NOT_LIVE_WAVE_THRESHOLD - 1));
        assert!(is_live_wave(NOT_LIVE_WAVE_THRESHOLD));
    }

    /// 「当天没导入行 = 没导入，不算未播天」是定稿口径，用测试钉住，
    /// 免得后来人看见「缺采的日子没算进去」当成 bug 好心改掉。
    ///
    /// 聚合只扫 `daily_metric` 存在的行：有行且 wave<2 → 未播 +1；无行 → 不参与。
    /// 这里直接断言 SQL 的形状：必须是「按人按月 SUM(CASE WHEN wave < 阈值)」，
    /// 且不能出现把「无行」折算成天数的写法（比如按日历天补零）。
    #[test]
    fn absent_days_only_counts_existing_rows() {
        let month_sql = month_absent_sql("2999-01");
        assert!(
            month_sql.contains("SUM(CASE WHEN wave < 2"),
            "月未播天数必须按「有数据行且音浪低于阈值」累加：{month_sql}"
        );
        assert!(
            month_sql.contains("wave_reliable = 1"),
            "月未播天数只算可信行：{month_sql}"
        );
        assert!(
            month_sql.contains("TO_CHAR(biz_date"),
            "月份过滤要带年份，否则跨年会串月：{month_sql}"
        );
        // 不允许按日历天补零——那会把「没导入」算成「未播」
        for bad in ["COALESCE", "GENERATE_SERIES", "calendar"] {
            assert!(
                !month_sql
                    .to_ascii_lowercase()
                    .contains(&bad.to_ascii_lowercase()),
                "月未播天数不得按日历天补零（缺采 ≠ 未播）：{month_sql}"
            );
        }
    }
}
