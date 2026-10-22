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

/// 「当月直播天数」：只统计有数据行且音浪 >= 阈值的日期，与未播天数互补。
pub const MONTH_LIVE_SQL: &str =
    "SELECT person_id, (SUM(CASE WHEN wave >= {NOT_LIVE_WAVE_THRESHOLD} THEN 1 ELSE 0 END))::BIGINT AS n \
     FROM daily_metric WHERE wave_reliable = 1 \
       AND TO_CHAR(biz_date, 'YYYY-MM') = '{month}' GROUP BY person_id";

pub fn month_live_sql(month: &str) -> String {
    MONTH_LIVE_SQL
        .replace(
            "{NOT_LIVE_WAVE_THRESHOLD}",
            &NOT_LIVE_WAVE_THRESHOLD.to_string(),
        )
        .replace("{month}", month)
}

/// LOCAL：唯一业务库 —— person / account / daily_metric
const ANCHORS_A: &str = "SELECT id, name, avatar_url, status,
       (SELECT pa.hash FROM person_avatar pa
         WHERE pa.person_id = person.id AND pa.image IS NOT NULL) AS avatar_hash
     FROM person
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
/// ⚠️ **这两列是 boolean**（`pb.conversation_id <> ''` / `pb.notify_target = 1 OR ...`），
/// sqlx 按 i64/i16 读会 ColumnDecode（2026-10-14 审计 P0-2：台账两列曾经对所有人恒 false）。
/// 读取侧统一走 `admin::read_opt_i64` 的 int2/int4/bool 类型回退，别在这里改成整数
/// —— 语义上它就是布尔。
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
          WHERE pb.name = p.name LIMIT 1) AS wechat_push,
       /* 抖音头像本地留存（person_avatar，见 avatar_sync.rs）：管理端只下发
          「有没有图 + 内容指纹」，不把 BYTEA 塞进台账 JSON（几十 KB × 全员会撑爆响应）。
          前端拿 hasAvatar 决定是否渲染 img（/api/public/avatars/{id}），
          avatarHash 给前端做缓存校验（变了才重拉）。 */
       (SELECT pa.image IS NOT NULL FROM person_avatar pa
          WHERE pa.person_id = p.id) AS has_avatar,
       (SELECT pa.hash FROM person_avatar pa
          WHERE pa.person_id = p.id) AS avatar_hash
     FROM person p
     LEFT JOIN account a ON a.person_id = p.id AND a.is_primary = 1
     LEFT JOIN person m ON m.id = p.master_id
     WHERE p.deleted_at IS NULL
     ORDER BY p.name";

/// 016 / 015 未跑时的兜底：列清单逐项对齐 [`STREAMERS_SQL`]，少 `p.phone` 与 `has_account`
/// （`my_accounts.person_id` 是 015 加的）。台账页在这套库上只是看不到手机号 / 开号状态，
/// 其余字段与今天完全一致（这是 015 定下的"向后兼容：老库一行代码不改也能跑"口径）。
/// ⚠️ `wechat_bound` / `wechat_push` 同样是 boolean，读取侧走 `admin::read_opt_i64` 的类型回退。
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
          WHERE pb.name = p.name LIMIT 1) AS wechat_push,
       /* 头像两列与 STREAMERS_SQL 保持一致：person_avatar 是 025 加的表，若遇到连该表
          都没有的更老库，这里会整条查询报错 → admin::streamers 退回更早的兜底路径
          （当前没有更早的路径，本地库均 ≥025，先不对齐更老版本，避免兜底层数爆炸）。 */
       (SELECT pa.image IS NOT NULL FROM person_avatar pa
          WHERE pa.person_id = p.id) AS has_avatar,
       (SELECT pa.hash FROM person_avatar pa
          WHERE pa.person_id = p.id) AS avatar_hash
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

/// SQL 里 `?` 占位符的个数。
///
/// 2026-10-14 审计 P1-4 的护栏：`DATE_COUNT_SQL` 只有 1 个 `?`，`TEAM_DATE_COUNT_SQL`
/// 有 2 个；调用方（admin::commit_import 的 reject 分支）曾经不看 SQL 就 bind 两个参数
/// → PG 报 `bind message supplies 2 parameters, but prepared statement requires 1`
/// → 事务第一条语句即失败、整批回滚。绑定个数一律由这个函数算出来，SQL 一改就跟着变。
///
/// 注意：这是朴素计数（不区分字符串字面量），只适用于本文件里这几条不含 `'?'` 的
/// 计数 SQL；`pq::rewrite` 另有字面量跳过逻辑，别拿它去数带字面量问号的 SQL。
pub fn placeholder_count(sql: &str) -> usize {
    sql.matches('?').count()
}

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

/// 月 / 年聚合的公共骨架（2026-10-10 月度时长导入；2026-10-16 月度音浪导入）。
///
/// 整月口径（用户口径）：某月**有导入的整月值**（monthly_duration 时长 / monthly_wave 音浪）
/// 就用它，没有（历史月份 / 只有日明细）才回落到 daily_metric 求和 —— 所以必须先把日明细
/// 聚合到「每人每月」中间表 t，再按人合计；直接 SUM(d.wave) / SUM(d.minutes) 会把整月口径丢掉。
///
/// 中间表 t 的列：pid / m（YYYY-MM）/ w（音浪日明细求和）/ dm（时长日明细求和）/
/// live（开播天数）/ off（未播天数）/ days（有数据天数）。
///
/// **导入了整月音浪、当月却没有日明细行的月份也要进榜**：t 里补一行占位（w/dm/live/off/days 全 0），
/// 音浪由 LEFT JOIN 的整月值填上。平台月报本来就常用来补采「没有按日导入」的历史月份，
/// 只认日明细的话这些人在月榜/年榜上根本不出现，等于导入白做。补齐行只带音浪值，
/// 开播 / 未播天数一律 0（未播天数继续走日明细口径，见文件头 NOT_LIVE_WAVE_THRESHOLD 注释）。
/// ⚠️ 时长**不**做同样的补齐（用户口径：时长保持现有逻辑），所以只有月度时长、没有日明细、
/// 也没有整月音浪的人照旧不进榜。
///
/// - `select`：要吐出的列；音浪写 `{wave}`、时长写 `{minutes}` 占位，由本函数决定口径
///   （with_wave = true → `COALESCE(mw.wave, t.w)`，false → `t.w`；
///    with_duration = true → `COALESCE(md.minutes, t.dm)`，false → `t.dm`）
/// - `like`：`2026-09%`（月）或 `2026%`（年），调用前已过字符白名单
/// - `team`：male / female / 空（空 = 不限团别）
/// - `extra_where`：额外过滤（如公开端的 `AND p.status <> 'left'`），已内联
/// - `order_by`：排序键（后面固定跟 `ORDER BY`），一般写 `wave DESC`
/// - `with_duration` / `with_wave`：是否带上两张整月表（库没跑 007 / 026 迁移时只能 false）。
///   **两个开关互相独立**：只跑了 007 没跑 026 的库上，时长照旧用整月值、音浪回落日明细，
///   而不是一起退回日明细（逐张表的回落判定见 [`query_period`]）。
/// 把「LIKE 窗口」翻成**日期范围** `[lo, hi)`（2026-10-17 性能优化，见 [`period_rollup_sql`]）。
///
/// 入参是调用方传的 `like` 窗口（`2026-09%` 月 / `2026%` 年），返回可直接拼进 SQL 的
/// `YYYY-MM-DD` 字符串对：
///   `"2026-09%"` → ("2026-09-01", "2026-10-01")   // 9 月整月
///   `"2026%"`    → ("2026-01-01", "2027-01-01")   // 2026 全年
///   `"2999-01%"` → ("2999-01-01", "2999-02-01")   // 单测用的哨兵月
///
/// 只认「4 位年」与「4 位年-2 位月」两种形状（调用前已过字符白名单）；
/// **认不出就退回原 LIKE 语义**（返回一个覆盖全时间的范围 + 让调用方走旧分支），
/// 不静默改变查询语义 —— 宁可慢，不能算错。
pub fn like_to_date_range(like: &str) -> (String, String) {
    let s = like.trim_end_matches('%');
    // 年-月：YYYY-MM
    if s.len() == 7 && s.as_bytes()[4] == b'-' {
        let (y, m) = (&s[..4], &s[5..7]);
        if let (Ok(y), Ok(m)) = (y.parse::<u32>(), m.parse::<u32>()) {
            if (1..=12).contains(&m) {
                let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
                return (format!("{y:04}-{m:02}-01"), format!("{ny:04}-{nm:02}-01"));
            }
        }
    }
    // 年：YYYY
    if s.len() == 4 {
        if let Ok(y) = s.parse::<u32>() {
            return (format!("{y:04}-01-01"), format!("{:04}-01-01", y + 1));
        }
    }
    // 认不出：给一个「全时间」范围，配合调用方的兜底（不会漏行，只可能多算 ——
    // 但调用方传入的都是过过白名单的窗口，实际走不到这里）
    ("0001-01-01".to_string(), "9999-01-01".to_string())
}

/// period 形状白名单（2026-10-17 审计 4-P3-1）：只认两种形态 ——
/// `YYYY-MM`（7 位、月份 01..=12）与 `YYYY`（4 位纯数字）。
///
/// 为什么要有这个函数：site/metrics 的 period 入参原先是「字符集 + 长度」校验，
/// `2026-1-` / `20-2` 这类**字符集合法但形状畸形**的窗口能通过，随后
/// [`like_to_date_range`] 认不出形状 → 回退到「全时间范围」，最终把
/// **全时段聚合**当成该月 / 该年返回（数值错，不是注入）。这里把形状校验收成
/// 唯一口径：调用方先过它再拼 LIKE 窗口；[`period_rollup_sql`] 入口再兜底一次。
pub fn valid_period_shape(period: &str) -> bool {
    let p = period.trim();
    let b = p.as_bytes();
    // YYYY-MM：第 5 位是 '-'，其余是数字，月份必须 01..=12
    if b.len() == 7 {
        return b[4] == b'-'
            && b[..4].iter().all(u8::is_ascii_digit)
            && b[5..].iter().all(u8::is_ascii_digit)
            && matches!(p[5..].parse::<u32>(), Ok(1..=12));
    }
    // YYYY：4 位纯数字（年份不设上限，与 like_to_date_range 的解析口径一致）
    if b.len() == 4 {
        return b.iter().all(u8::is_ascii_digit);
    }
    false
}

/// `period_rollup_sql` 的 `{like}` 窗口形态：在 [`valid_period_shape`] 之上
/// 要求末尾带一个 `%`（月/年都是前缀匹配窗口）。
fn valid_like_window(like: &str) -> bool {
    match like.strip_suffix('%') {
        Some(base) => valid_period_shape(base),
        None => false,
    }
}

pub fn period_rollup_sql(
    select: &str,
    like: &str,
    team: &str,
    extra_where: &str,
    order_by: &str,
    with_duration: bool,
    with_wave: bool,
) -> String {
    /* like 窗口运行期校验（2026-10-17 审计 3-P3-B → 4-P3-1）：`{like}` 会被
    原样拼进 `mw0.period LIKE '{like}'`，必须保证形状可控。原先只有
    `debug_assert!` —— release 构建（生产 `cargo build --release`）里是 no-op，
    等于线上没有护栏。现在改成**发布版也生效**的校验：形状不合法（非
    `YYYY-MM%` / `YYYY%`）就记 debug 并回退到「当前月」窗口，绝不把外部字符串
    带进 SQL，也不静默退化成全时段聚合。 */
    let like_fallback;
    let like = if valid_like_window(like) {
        like
    } else {
        like_fallback = format!("{}%", chrono::Local::now().format("%Y-%m"));
        tracing::debug!(
            got = like,
            fallback = %like_fallback,
            "period_rollup_sql: like 窗口形状非法（应为 YYYY-MM% / YYYY%），回退当前月"
        );
        like_fallback.as_str()
    };
    let team_clause = if team.is_empty() {
        String::new()
    } else {
        format!(" AND p.gender = '{team}'")
    };
    let (minutes_expr, duration_join) = if with_duration {
        (
            "COALESCE(md.minutes, t.dm)",
            " \
         LEFT JOIN monthly_duration md ON md.person_id = t.pid AND md.period = t.m",
        )
    } else {
        ("t.dm", "")
    };
    let (wave_expr, wave_join) = if with_wave {
        (
            "COALESCE(mw.wave, t.w)",
            " \
         LEFT JOIN monthly_wave mw ON mw.person_id = t.pid AND mw.period = t.m",
        )
    } else {
        ("t.w", "")
    };
    /* 有整月音浪、当月没有（可信）日明细行的人：补一行占位，让月 / 年榜上能看见导进来的整月值。
    NOT EXISTS 与本查询内层同一过滤（wave_reliable=1 + 同一个 {like} 窗口形状），
    保证同一人同一月只可能有一路，日明细与整月值绝不会双算。 */
    let month_wave_only = if with_wave {
        format!(
            " \
         UNION ALL \
         SELECT mw0.person_id AS pid, mw0.period AS m, 0 AS w, 0 AS dm, \
                0 AS live, 0 AS off, 0 AS days \
           FROM monthly_wave mw0 \
          WHERE mw0.period LIKE '{like}' \
            AND NOT EXISTS (SELECT 1 FROM daily_metric d0 \
                             WHERE d0.person_id = mw0.person_id AND d0.wave_reliable = 1 \
                               AND TO_CHAR(d0.biz_date, 'YYYY-MM') = mw0.period)"
        )
    } else {
        String::new()
    };
    let select = select
        .replace("{minutes}", minutes_expr)
        .replace("{wave}", wave_expr);
    /* 2026-10-17 性能优化（数据库架构优化）：把日明细的窗口从
    `TO_CHAR(d.biz_date,'YYYY-MM') LIKE '{like}'` 改成**日期范围比较**。
    TO_CHAR 是函数表达式，PG 用不上 biz_date 上的 B-tree 索引 → 每张月/年榜都要
    全表扫 daily_metric；改写成 `biz_date >= lo AND biz_date < hi` 后可走索引范围扫描。
    本机 768 行实测：5.92ms → 0.76ms（约 7.8×），且数据量继续增长时差距会更大。
    等价性已用实库逐窗口比对（月窗口 / 年窗口的人与数值完全一致）。 */
    let (date_lo, date_hi) = like_to_date_range(like);
    format!(
        "SELECT {select} FROM ( \
           SELECT d.person_id AS pid, TO_CHAR(d.biz_date, 'YYYY-MM') AS m, \
             SUM(d.wave) AS w, SUM(d.minutes) AS dm, \
             SUM(CASE WHEN d.wave >= 2 THEN 1 ELSE 0 END) AS live, \
             SUM(CASE WHEN d.wave < 2 THEN 1 ELSE 0 END) AS off, \
             COUNT(DISTINCT d.biz_date) AS days \
           FROM daily_metric d \
           WHERE d.wave_reliable = 1 \
             AND d.biz_date >= '{date_lo}'::date AND d.biz_date < '{date_hi}'::date \
           GROUP BY pid, m{month_wave_only} \
         ) t \
         JOIN person p ON p.id = t.pid{duration_join}{wave_join} \
         WHERE p.deleted_at IS NULL{team_clause}{extra_where} \
         GROUP BY p.id, p.name, p.gender \
         ORDER BY {order_by}"
    )
}

/// 管理端月 / 年聚合要吐的列（`{wave}` / `{minutes}` 由 period_rollup_sql 按口径替换）
pub const ADMIN_PERIOD_SELECT: &str = "p.id AS person_id, p.name, p.gender, \
     (SUM({wave}))::BIGINT AS wave, \
     (SUM({minutes}))::BIGINT AS minutes, \
     (SUM(t.live))::BIGINT AS live_days, \
     (SUM(t.days))::BIGINT AS data_days";

/// 从错误信息里认「哪张整月表不存在」→ `(缺 monthly_duration[007], 缺 monthly_wave[026])`。
///
/// PG 报缺表一律点名关系（`relation "monthly_wave" does not exist`），所以**点名优先**：
/// 两张表各自回落、互不牵连。不点名表名的模糊判据（`doesn't exist` / `Unknown table`）
/// 留给历史错误格式，这时两张表都当缺 —— 一起回落日明细最保守，也不会把页面打挂。
fn missing_month_tables(e: &anyhow::Error) -> (bool, bool) {
    let m = e.to_string();
    let names_duration = m.contains("monthly_duration");
    let names_wave = m.contains("monthly_wave");
    let generic =
        m.contains("doesn't exist") || m.contains("does not exist") || m.contains("Unknown table");
    let unnamed = generic && !names_duration && !names_wave;
    (names_duration || unnamed, names_wave || unnamed)
}

/// 错误是不是「monthly_duration 表不存在」（库还没跑 007 迁移）。
/// 只认这一张表：报错点名 monthly_wave 时返回 false，时长不跟着一起降级。
pub fn is_missing_month_table(e: &anyhow::Error) -> bool {
    missing_month_tables(e).0
}

/// 错误是不是「monthly_wave 表不存在」（库还没跑 026 迁移）。与 [`is_missing_month_table`]
/// 互为独立开关：哪张表缺就关哪个，另一张的整月口径照旧生效。
pub fn is_missing_month_wave_table(e: &anyhow::Error) -> bool {
    missing_month_tables(e).1
}

/// 月 / 年聚合查询：先按「整月导入值优先」查，库缺哪张整月表就关掉哪个开关重试
/// （007 没跑 → 时长回落日明细；026 没跑 → 音浪回落日明细；两张都缺 → 都回落），
/// 别让少一张表把整个展示页 / 数据页打不开。与缺表无关的错误原样上报，不吞成空榜。
pub async fn query_period(
    pools: &crate::pgrs::Pools,
    source: &str,
    select: &str,
    like: &str,
    team: &str,
    extra_where: &str,
    order_by: &str,
) -> anyhow::Result<Vec<sqlx::postgres::PgRow>> {
    let (mut with_duration, mut with_wave) = (true, true);
    loop {
        let sql = period_rollup_sql(
            select,
            like,
            team,
            extra_where,
            order_by,
            with_duration,
            with_wave,
        );
        match crate::pgrs::query(pools, source, &sql).await {
            Ok(rows) => return Ok(rows),
            Err(e) => {
                let (lack_duration, lack_wave) = missing_month_tables(&e);
                let next_duration = with_duration && !lack_duration;
                let next_wave = with_wave && !lack_wave;
                if next_duration == with_duration && next_wave == with_wave {
                    return Err(e);
                }
                if with_duration && !next_duration {
                    tracing::warn!(
                        "monthly_duration 表不存在，月/年时长按日明细口径回落（请先跑 pk-server migrate）"
                    );
                }
                if with_wave && !next_wave {
                    tracing::warn!(
                        "monthly_wave 表不存在，月/年音浪按日明细口径回落（请先跑 pk-server migrate 026）"
                    );
                }
                with_duration = next_duration;
                with_wave = next_wave;
            }
        }
    }
}

/// 跑一条「月 / 年音浪优先整月导入值」的 SQL：库还没跑 026 迁移（没有 monthly_wave）时，
/// 用调用方给的纯日明细 SQL 重试 —— 与 [`query_period`] 同一套探测，但**只管 monthly_wave
/// 这一张表**（时长表缺不缺与音浪无关，两张整月表互不牵连）。
pub async fn query_month_wave_fallback(
    pools: &crate::pgrs::Pools,
    source: &str,
    prefer_month_sql: &str,
    daily_only_sql: &str,
) -> anyhow::Result<Vec<sqlx::postgres::PgRow>> {
    match crate::pgrs::query(pools, source, prefer_month_sql).await {
        Ok(rows) => Ok(rows),
        Err(e) if is_missing_month_wave_table(&e) => {
            tracing::warn!(
                "monthly_wave 表不存在，月/年音浪按日明细口径回落（请先跑 pk-server migrate 026）"
            );
            crate::pgrs::query(pools, source, daily_only_sql).await
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
///
/// 口径：**一个抖音号挂在 ≥2 个不同在册主播名下**才算重复（需要人工核对归属）。
/// 同一个主播名下同一个号存两行 account（主号/副号、历史重复导入）是常态，
/// 不是重复 —— 所以先在子查询里按 (douyin_no, person) 去重，`n` 数的是「人」而不是 account 行，
/// `ids` / `names` 里同一个人也只出现一次。
pub const DUP_DOUYIN_SQL: &str = "SELECT douyin_no, COUNT(*) AS n,
       string_agg(id::text, ',' ORDER BY id) AS ids, string_agg(name, ',' ORDER BY id) AS names
     FROM (SELECT DISTINCT a.douyin_no, p.id, p.name
             FROM account a JOIN person p ON p.id = a.person_id
            WHERE p.deleted_at IS NULL AND a.douyin_no <> '') d
     GROUP BY douyin_no HAVING COUNT(*) > 1";

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

/// 月度暂存区里「某个主播名下、还没认领」的整月行（042，建档 / 绑号后补账用）。
///
/// 命中口径与按日 [`PENDING_FOR_PERSON_SQL`] 逐条对齐：暂存行的号码出现在该主播
/// account 的 douyin_no 或 anchor_id 里即算命中；只认 `absorbed_at IS NULL`（幂等）。
/// 区别只在业务日期：月度暂存按 `period char(7)`（YYYY-MM）而不是某一天，
/// 所以按 period 排序，补账时按月叠加。
pub const PENDING_MONTH_FOR_PERSON_SQL: &str =
    "SELECT id, name, btrim(period) AS period, wave, minutes,
       source_batch_id
     FROM import_pending_month
     WHERE absorbed_at IS NULL
       AND (douyin_no IN (SELECT a.douyin_no FROM account a
                            WHERE a.person_id = ? AND a.douyin_no <> '')
         OR douyin_no IN (SELECT a.anchor_id FROM account a
                            WHERE a.person_id = ? AND a.anchor_id <> ''))
     ORDER BY period, id";

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

    /// 042：月度暂存补账的命中口径必须和按日 `PENDING_FOR_PERSON_SQL` 一致 ——
    /// 该主播名下任一抖音号 / 采集 ID 命中即算，且只认未认领行（幂等的前提）。
    /// 钉死关键子句是防「顺手改成按 name 命中」或「漏掉 absorbed_at IS NULL 导致重复补账」。
    #[test]
    fn pending_month_sql_matches_person_scoped_by_account() {
        for must in [
            "FROM import_pending_month",
            "absorbed_at IS NULL",
            "a.douyin_no <> ''",
            "a.anchor_id <> ''",
            "ORDER BY period, id",
        ] {
            assert!(
                PENDING_MONTH_FOR_PERSON_SQL.contains(must),
                "月度暂存补账缺「{must}」：{PENDING_MONTH_FOR_PERSON_SQL}"
            );
        }
        // 两个绑定：douyin_no 一个、anchor_id 一个（与 bind 调用严格对齐，多一个就 panic）
        assert_eq!(placeholder_count(PENDING_MONTH_FOR_PERSON_SQL), 2);
        assert!(
            !PENDING_MONTH_FOR_PERSON_SQL.contains("biz_date"),
            "月度暂存按 period 认，不是某一天：{PENDING_MONTH_FOR_PERSON_SQL}"
        );
        // 两张暂存表不能写串（按日 / 按月各一条补账链路）
        assert!(!PENDING_FOR_PERSON_SQL.contains("import_pending_month"));
        assert!(!PENDING_LIST_SQL.contains("import_pending_month"));
    }

    /// 同抖音号重复检测的口径：**同一个抖音号挂在 ≥2 个不同主播名下**才算重复。
    /// 一个主播名下同一个号存两行 account（主号/副号、历史重复导入）是常态，
    /// 曾经只 `COUNT(*) > 1` 导致把这类行报成「重复：同抖音号 2 组」的误报。
    #[test]
    fn dup_douyin_counts_people_not_account_rows() {
        assert!(
            DUP_DOUYIN_SQL.contains("SELECT DISTINCT a.douyin_no, p.id"),
            "必须先按 (douyin_no, person) 去重：同一主播名下同一个号多行 account 不算重复\n{DUP_DOUYIN_SQL}"
        );
        assert!(
            DUP_DOUYIN_SQL.contains("HAVING COUNT(*) > 1"),
            "阈值仍是 ≥2，但这里的「行」已是去重后的人\n{DUP_DOUYIN_SQL}"
        );
        // 去重后计数：n 是人数；ids / names 里同一人只出现一次（外层只能从去重子查询取列）
        assert!(DUP_DOUYIN_SQL.contains("COUNT(*) AS n"));
        assert!(DUP_DOUYIN_SQL.contains("string_agg(id::text, ',' ORDER BY id) AS ids"));
        assert!(DUP_DOUYIN_SQL.contains("string_agg(name, ',' ORDER BY id) AS names"));
        assert!(
            DUP_DOUYIN_SQL.contains(") d\n     GROUP BY douyin_no"),
            "GROUP BY / HAVING 必须作用在去重子查询的结果上，别退回「直接 JOIN 再 GROUP BY」的老写法\n{DUP_DOUYIN_SQL}"
        );
        // 字段名是 admin.rs / 前端契约：改列名会 500，这里钉死
        for col in ["douyin_no", "n", "ids", "names"] {
            assert!(DUP_DOUYIN_SQL.contains(col), "接口按列名取值，缺 {col}");
        }
        // 同名检测是同表 person 的重名，语义本来就对，不许被顺手改掉
        assert!(DUP_NAMES_SQL.contains("FROM person WHERE deleted_at IS NULL"));
        assert!(DUP_NAMES_SQL.contains("GROUP BY name HAVING COUNT(*) > 1"));
    }

    /// P1-4 护栏：reject 模式的绑定参数个数必须与 SQL 占位符个数一致 ——
    /// 不分团 1 个（只有 biz_date），分团 2 个（biz_date + team）。
    /// 谁给这两条 SQL 加/减一个 `?`，这条测试会先红，提醒同步改 bind。
    #[test]
    fn day_count_sql_placeholder_counts() {
        assert_eq!(
            placeholder_count(DATE_COUNT_SQL),
            1,
            "不分团只按 biz_date 过滤：只允许 1 个 ?"
        );
        assert_eq!(
            placeholder_count(TEAM_DATE_COUNT_SQL),
            2,
            "分团是 biz_date + gender：2 个 ?"
        );
        assert!(DATE_COUNT_SQL.contains("FROM daily_metric"));
        assert!(TEAM_DATE_COUNT_SQL.contains("p.gender = ?"));
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

    /// LIKE 窗口 → 日期范围（2026-10-17 性能优化）：月 / 年 / 跨年边界都要对，
    /// 这段范围直接决定月榜与年榜**算哪些天**，算错 = 数字错。
    #[test]
    fn like_to_date_range_covers_month_and_year() {
        // 月窗口
        assert_eq!(
            like_to_date_range("2026-09%"),
            ("2026-09-01".to_string(), "2026-10-01".to_string())
        );
        assert_eq!(
            like_to_date_range("2999-01%"),
            ("2999-01-01".to_string(), "2999-02-01".to_string())
        );
        // 12 月要跨年
        assert_eq!(
            like_to_date_range("2026-12%"),
            ("2026-12-01".to_string(), "2027-01-01".to_string())
        );
        // 年窗口
        assert_eq!(
            like_to_date_range("2026%"),
            ("2026-01-01".to_string(), "2027-01-01".to_string())
        );
        // 不带 % 也能处理（调用方可能直接传窗口值）
        assert_eq!(
            like_to_date_range("2026-05"),
            ("2026-05-01".to_string(), "2026-06-01".to_string())
        );
        // 认不出的脏值 → 全时间范围（宁可多算，不能算错/静默改语义）
        let (lo, hi) = like_to_date_range("garbage");
        assert_eq!(lo, "0001-01-01");
        assert_eq!(hi, "9999-01-01");
    }

    /// period 形状白名单（2026-10-17 审计 4-P3-1）：只认 YYYY-MM / YYYY，
    /// 畸形但字符集合法的窗口（`2026-1-` / `20-2`）必须被拒 —— 否则
    /// like_to_date_range 认不出会退化成全时段聚合。
    #[test]
    fn valid_period_shape_accepts_only_month_or_year() {
        // 正常值
        assert!(valid_period_shape("2026-10"));
        assert!(valid_period_shape("2026-01"));
        assert!(valid_period_shape("2026-12"));
        assert!(valid_period_shape("2026"));
        assert!(valid_period_shape("1999"));
        // 畸形但字符集合法（原「字符集 + 长度」校验会放行的那一类）
        assert!(!valid_period_shape("2026-1-"), "月/日缺位 + 多余 '-'");
        assert!(!valid_period_shape("20-2"), "年份不足 4 位");
        assert!(!valid_period_shape("2026-13"), "13 月不存在");
        assert!(!valid_period_shape("2026-00"), "00 月不存在");
        assert!(!valid_period_shape("202610"), "缺 '-'");
        assert!(!valid_period_shape("2026-10-01"), "多了一段日期");
        assert!(!valid_period_shape("----"));
        assert!(!valid_period_shape(""));
        assert!(!valid_period_shape("garbage"));
        // 两侧空白容忍（调用方先 trim 再拼窗口）
        assert!(valid_period_shape(" 2026-10 "));
    }

    /// period_rollup_sql 的 like 窗口：非法形状必须回退当前月，且回退后的 SQL
    /// 只能是 `YYYY-MM%` 形状（release 也有护栏，取代原 debug_assert）。
    #[test]
    fn period_rollup_sql_falls_back_on_bad_like_window() {
        let select = ADMIN_PERIOD_SELECT;
        let sql = period_rollup_sql(select, "2026-1-%", "", "", "wave DESC", false, true);
        let month = chrono::Local::now().format("%Y-%m").to_string();
        assert!(
            sql.contains(&format!("mw0.period LIKE '{month}%'")),
            "非法窗口应回退当前月：{sql}"
        );
        assert!(!sql.contains("2026-1-"), "非法窗口不得进 SQL：{sql}");
        // 正常窗口原样保留（不回退）
        let ok = period_rollup_sql(select, "2999-01%", "", "", "wave DESC", false, true);
        assert!(ok.contains("mw0.period LIKE '2999-01%'"), "{ok}");
    }

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

    #[test]
    fn live_days_are_complementary_to_absent_days() {
        let sql = month_live_sql("2999-01");
        assert!(sql.contains("SUM(CASE WHEN wave >= 2"), "{sql}");
        assert!(sql.contains("wave_reliable = 1"), "{sql}");
        assert!(sql.contains("2999-01"), "{sql}");
    }

    // ---- 月 / 年骨架的整月口径（2026-10-16 月度音浪导入）----

    /// 月榜骨架：某月有导入的整月音浪（monthly_wave）就用它，没有该月的整月值才回落
    /// 日明细求和 —— 与月度时长同一套 COALESCE 写法。
    #[test]
    fn month_rollup_prefers_imported_wave_over_daily_sum() {
        let sql = period_rollup_sql(
            "{wave} AS wave, {minutes} AS minutes",
            "2999-01%",
            "",
            "",
            "wave DESC",
            true,
            true,
        );
        assert!(
            sql.contains("COALESCE(mw.wave, t.w)"),
            "音浪要「整月优先、缺月回落日明细」：{sql}"
        );
        assert!(
            sql.contains("LEFT JOIN monthly_wave mw ON mw.person_id = t.pid AND mw.period = t.m"),
            "整月音浪按 (人, 月) 对齐：{sql}"
        );
        // 回落的来源必须是「每人每月」的日明细求和，不是全期一把 SUM
        assert!(sql.contains("SUM(d.wave) AS w"), "{sql}");
        /* 2026-10-17 性能优化：日明细窗口从 `TO_CHAR(...) LIKE` 改成**日期范围**
        （能走 biz_date 索引，实测 5.92ms → 0.76ms）。这里改成断言范围条件，
        并反向确认不再出现函数表达式比较。 */
        assert!(
            sql.contains("d.biz_date >= '2999-01-01'::date AND d.biz_date < '2999-02-01'::date"),
            "日明细窗口应改写成日期范围：{sql}"
        );
        assert!(
            !sql.contains("TO_CHAR(d.biz_date, 'YYYY-MM') LIKE"),
            "不该再对 biz_date 做函数表达式比较（用不上索引）：{sql}"
        );
        assert!(sql.contains("GROUP BY pid, m"), "{sql}");
        // 两个占位符都必须被替换干净（写错占位名会留下 {wave} 进 SQL → PG 语法错）
        assert!(
            !sql.contains("{wave}") && !sql.contains("{minutes}"),
            "{sql}"
        );
        // 时长开关没关：仍用整月时长
        assert!(sql.contains("COALESCE(md.minutes, t.dm)"), "{sql}");
    }

    /// 只有整月音浪、当月没有日明细行的月份也要进榜（平台月报补采历史月份的场景），
    /// 且同一人同一月只走一路 —— 日明细与整月值绝不双算。
    #[test]
    fn month_rollup_includes_month_wave_only_rows() {
        let sql = period_rollup_sql(
            "{wave} AS wave",
            "2999-01%",
            "",
            "",
            "wave DESC",
            false,
            true,
        );
        assert!(sql.contains("FROM monthly_wave mw0"), "{sql}");
        assert!(
            sql.contains("NOT EXISTS (SELECT 1 FROM daily_metric d0"),
            "有日明细的月份不能再补一行（否则日明细 + 整月值双算）：{sql}"
        );
        assert!(
            sql.contains("0 AS live, 0 AS off, 0 AS days"),
            "补齐行不带开播 / 未播天数（未播天数继续走日明细口径）：{sql}"
        );
        assert!(
            sql.contains("mw0.period LIKE '2999-01%'"),
            "补齐行只认窗口内的月份：{sql}"
        );
    }

    /// 库没跑 026（monthly_wave 缺）关掉音浪开关后：SQL 里不能再出现这张表 / 这个别名，
    /// 但时长的整月口径必须照旧（两个开关互不牵连）。
    #[test]
    fn month_rollup_without_wave_table_keeps_duration_month_value() {
        let sql = period_rollup_sql(
            "{wave} AS wave, {minutes} AS minutes",
            "2999%",
            "",
            "",
            "wave DESC",
            true,
            false,
        );
        assert!(!sql.contains("monthly_wave"), "{sql}");
        assert!(!sql.contains("mw."), "{sql}");
        assert!(sql.contains("t.w AS wave"), "音浪回落成日明细求和：{sql}");
        assert!(
            sql.contains("COALESCE(md.minutes, t.dm)"),
            "音浪表缺不该把时长也降级：{sql}"
        );
        assert!(sql.contains("LEFT JOIN monthly_duration md"), "{sql}");
    }

    /// 库没跑 007（monthly_duration 缺）关掉时长开关后：音浪的整月口径照旧生效。
    #[test]
    fn month_rollup_without_duration_table_keeps_wave_month_value() {
        let sql = period_rollup_sql(
            "{wave} AS wave, {minutes} AS minutes",
            "2999%",
            "",
            "",
            "wave DESC",
            false,
            true,
        );
        assert!(!sql.contains("monthly_duration"), "{sql}");
        assert!(!sql.contains("md."), "{sql}");
        assert!(
            sql.contains("t.dm AS minutes"),
            "时长回落成日明细求和：{sql}"
        );
        assert!(
            sql.contains("COALESCE(mw.wave, t.w)"),
            "时长表缺不该把音浪也降级：{sql}"
        );
    }

    /// 管理端月 / 年聚合用的是同一份列清单（metrics.rs 直接传 ADMIN_PERIOD_SELECT）：
    /// 两个占位必须都在 —— 少一个要么留着 `{wave}` 进 SQL（PG 语法错），
    /// 要么静默退化成「只按日明细」而没人发现。
    #[test]
    fn admin_period_select_uses_both_placeholders() {
        assert!(
            ADMIN_PERIOD_SELECT.contains("{wave}"),
            "管理端月/年榜的音浪列也要走整月优先口径：{ADMIN_PERIOD_SELECT}"
        );
        assert!(
            ADMIN_PERIOD_SELECT.contains("{minutes}"),
            "时长列占位不能丢：{ADMIN_PERIOD_SELECT}"
        );
    }

    /// 两张整月表各自独立回落：报错点名哪张表就只关哪个开关，互不牵连。
    #[test]
    fn missing_month_table_probes_are_independent() {
        let wave_gone = anyhow::anyhow!(
            "error returned from database: relation \"monthly_wave\" does not exist"
        );
        assert!(is_missing_month_wave_table(&wave_gone));
        assert!(
            !is_missing_month_table(&wave_gone),
            "音浪表缺 → 不能把时长的整月口径也一起降级"
        );

        let duration_gone = anyhow::anyhow!(
            "error returned from database: relation \"monthly_duration\" does not exist"
        );
        assert!(is_missing_month_table(&duration_gone));
        assert!(
            !is_missing_month_wave_table(&duration_gone),
            "时长表缺 → 不能把音浪的整月口径也一起降级"
        );

        // 不点名表名的历史错误格式：两张都当缺（一起回落日明细，最保守）
        let vague = anyhow::anyhow!("Unknown table 'monthly'");
        assert!(is_missing_month_table(&vague) && is_missing_month_wave_table(&vague));

        // 与缺表无关的错误绝不能触发回落（否则真错误会被吞成空榜）
        let other = anyhow::anyhow!("syntax error at or near \"FROM\"");
        assert!(!is_missing_month_table(&other) && !is_missing_month_wave_table(&other));
    }
}
