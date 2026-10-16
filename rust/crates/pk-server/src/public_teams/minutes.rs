//! public_teams::minutes —— 自 public_teams.rs 按业务域拆分（对外语义不变）。

use super::*;

/* --------------------- 个人页时长口径（2026-10-10 月度时长导入） --------------------- */

/// 同期对照的三个「上期」窗口（1.0.1）：昨日 / 上月 / 去年同期（去年 YTD 至同日）。
/// 集中成结构便于单测校验 SQL 形状（wave_reliable / 窗口口径）。
pub struct PrevPeriods {
    /// 上一业务日（来自 get_available_dates，非自然日）
    pub day: Option<String>,
    /// 上一个自然月（YYYY-MM）
    pub month: String,
    /// 去年（YYYY），同期 = 去年年初至去年今天（闰日安全：2/29 → None 则取整年）
    pub year: String,
    /// 去年今天（YYYY-MM-DD）；闰日无对应时为 None（按整年算）
    pub year_until: Option<String>,
}

/// 由最新业务日推出三个上期窗口。today 为空（库里没数据）→ 全 None/空，前端显示 —。
pub fn prev_periods_of(today: &str) -> PrevPeriods {
    use chrono::Datelike as _;
    let parsed = chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d").ok();
    let month = parsed
        .map(|d| d.format("%Y-%m").to_string())
        .or_else(|| today.get(..7).map(str::to_string))
        .unwrap_or_default();
    let prev_month = parsed.and_then(|d| d.checked_sub_months(chrono::Months::new(1)));
    let today_last_year = parsed.and_then(|d| d.checked_sub_months(chrono::Months::new(12)));
    PrevPeriods {
        day: None, // 由调用方用 dates[1] 填
        month: prev_month
            .map(|d| d.format("%Y-%m").to_string())
            .unwrap_or(month),
        year: (parsed.map(|d| d.year() - 1).unwrap_or_default()).to_string(),
        year_until: today_last_year.map(|d| d.format("%Y-%m-%d").to_string()),
    }
}

/// 本人三个上期音浪（一条 SQL 条件聚合）。零行 / 无数据 → 对应项 NULL（显示 —，区别于真 0）。
///
/// 口径（2026-10-16 月度音浪导入）：
/// - **pm（上月同期）**：与月卡同口径 —— 上月**有导入的整月音浪**（`monthly_wave`）就用它，
///   没有该月的整月值才回落 `daily_metric` 日明细求和（`wave_reliable = 1`）。子查询优先、
///   没有整月行（NULL）才回落，与 `my_wave_sql` / `period_rollup_sql` 是同一套 COALESCE 口径；
/// - **pd（前一日）**：保持日明细口径 —— 前一日是**单日**窗口，整月值只到「月」这一档，
///   拿它当单日值会把整月音浪算成一天的量；
/// - **py（去年同期）**：保持日明细口径 —— 这是**去年年初至去年今天**的 YTD 部分窗口，
///   整月值无法与部分窗口对齐（会把这个月整月的量算进只过了几天的窗口里，凭空虚高），
///   所以只有整月窗口（pm）走整月优先，部分窗口一律日明细。
///
/// 库没跑 026 迁移时用 [`self_prev_wave_sql_daily`] 重试（见调用点），个人页不因缺表打不开。
pub fn self_prev_wave_sql(p: &PrevPeriods) -> String {
    let day_clause = match &p.day {
        Some(d) => format!(
            "(SUM(CASE WHEN TO_CHAR(biz_date, 'YYYY-MM-DD') = '{d}' THEN wave END))::BIGINT AS pd"
        ),
        None => "(NULL)::BIGINT AS pd".to_string(),
    };
    let until = p
        .year_until
        .as_ref()
        .map(|u| format!(" AND biz_date <= '{u}'"))
        .unwrap_or_default();
    format!(
        "SELECT {day_clause}, \
         (COALESCE((SELECT SUM(mw.wave) FROM monthly_wave mw \
           WHERE mw.person_id = {{pid}} AND mw.period = '{m}'), \
          (SELECT SUM(d2.wave) FROM daily_metric d2 \
           WHERE d2.person_id = {{pid}} AND d2.wave_reliable = 1 \
             AND TO_CHAR(d2.biz_date, 'YYYY-MM') = '{m}')))::BIGINT AS pm, \
         (SUM(CASE WHEN TO_CHAR(biz_date, 'YYYY') = '{y}'{until} THEN wave END))::BIGINT AS py \
         FROM daily_metric WHERE person_id = {{pid}} AND wave_reliable = 1",
        m = p.month,
        y = p.year,
    )
}

/// [`self_prev_wave_sql`] 的纯日明细版：库还没跑 026 迁移（`monthly_wave` 不存在）时用，
/// 三个窗口全部按日明细算（与 026 之前的 shape 逐字一致），由调用点按缺表探测重试。
pub fn self_prev_wave_sql_daily(p: &PrevPeriods) -> String {
    let day_clause = match &p.day {
        Some(d) => format!(
            "(SUM(CASE WHEN TO_CHAR(biz_date, 'YYYY-MM-DD') = '{d}' THEN wave END))::BIGINT AS pd"
        ),
        None => "(NULL)::BIGINT AS pd".to_string(),
    };
    let until = p
        .year_until
        .as_ref()
        .map(|u| format!(" AND biz_date <= '{u}'"))
        .unwrap_or_default();
    format!(
        "SELECT {day_clause}, \
         (SUM(CASE WHEN TO_CHAR(biz_date, 'YYYY-MM') = '{m}' THEN wave END))::BIGINT AS pm, \
         (SUM(CASE WHEN TO_CHAR(biz_date, 'YYYY') = '{y}'{until} THEN wave END))::BIGINT AS py \
         FROM daily_metric WHERE person_id = {{pid}} AND wave_reliable = 1",
        m = p.month,
        y = p.year,
    )
}

/// 个人页时长统计的三种窗口
pub enum MinutesScope<'a> {
    Month(&'a str),
    Year(&'a str),
    Total,
}

impl MinutesScope<'_> {
    /// 日明细的过滤条件（daily_metric 别名 d）
    pub(super) fn daily_where(&self) -> String {
        match self {
            MinutesScope::Month(m) => format!(" AND TO_CHAR(d.biz_date, 'YYYY-MM') = '{m}'"),
            MinutesScope::Year(y) => format!(" AND TO_CHAR(d.biz_date, 'YYYY') = '{y}'"),
            MinutesScope::Total => String::new(),
        }
    }

    /// monthly_duration 的过滤条件
    pub(super) fn period_where(&self) -> String {
        match self {
            MinutesScope::Month(m) => format!(" AND period = '{m}'"),
            MinutesScope::Year(y) => format!(" AND period LIKE '{y}-%'"),
            MinutesScope::Total => String::new(),
        }
    }
}

/// 个人「时长」SQL：有整月导入值的月份用月报值，其余月份用日明细求和
/// （同一段时长不会既算月报又算日明细）。with_monthly = false 时退回纯日明细口径。
pub(super) fn my_minutes_sql(
    person_id: u64,
    scope: &MinutesScope<'_>,
    with_monthly: bool,
) -> String {
    let daily = scope.daily_where();
    if !with_monthly {
        return format!(
            "SELECT (COALESCE((SELECT SUM(d.minutes) FROM daily_metric d \
             WHERE d.person_id = {person_id}{daily}), 0))::BIGINT AS minutes"
        );
    }
    let period = scope.period_where();
    format!(
        "SELECT (COALESCE((SELECT SUM(minutes) FROM monthly_duration \
          WHERE person_id = {person_id}{period}), 0) \
         + COALESCE((SELECT SUM(d.minutes) FROM daily_metric d \
          WHERE d.person_id = {person_id}{daily} \
            AND TO_CHAR(d.biz_date, 'YYYY-MM') NOT IN \
              (SELECT period FROM monthly_duration WHERE person_id = {person_id}{period})), 0))::BIGINT AS minutes"
    )
}

/// 个人「音浪」SQL（2026-10-16 月度音浪导入）：某月有导入的整月音浪（monthly_wave）就用它，
/// 没有该月的整月值才回落日明细求和 —— 与 [my_minutes_sql] 同一套 COALESCE 口径，
/// 同一段音浪不会既算整月值又算日明细。with_monthly = false（库没跑 026 迁移）时退回纯日明细。
/// 日明细侧只算可信行（wave_reliable = 1，与榜单 / WAVES_A 同口径；基线日是累计值，
/// 混进来会把历史音浪算大）。
///
/// ⚠️ 只给 Month / Year 两个窗口用：**累计（Total）口径不变**（历史累计总音浪 = 日明细求和）。
/// 整月值只是「月内日明细」的替代口径，累计再叠加一遍整月值会把同一个月算两次 ——
/// 所以 Total 在这里直接按日明细算，真被误用时也不会双重计数。
pub(super) fn my_wave_sql(person_id: u64, scope: &MinutesScope<'_>, with_monthly: bool) -> String {
    let daily = scope.daily_where();
    let daily_only = format!(
        "SELECT (COALESCE((SELECT SUM(d.wave) FROM daily_metric d \
         WHERE d.person_id = {person_id} AND d.wave_reliable = 1{daily}), 0))::BIGINT AS wave"
    );
    if !with_monthly || matches!(scope, MinutesScope::Total) {
        return daily_only;
    }
    let period = scope.period_where();
    format!(
        "SELECT (COALESCE((SELECT SUM(wave) FROM monthly_wave \
          WHERE person_id = {person_id}{period}), 0) \
         + COALESCE((SELECT SUM(d.wave) FROM daily_metric d \
          WHERE d.person_id = {person_id} AND d.wave_reliable = 1{daily} \
            AND TO_CHAR(d.biz_date, 'YYYY-MM') NOT IN \
              (SELECT period FROM monthly_wave WHERE person_id = {person_id}{period})), 0))::BIGINT AS wave"
    )
}

/// 个人「音浪」（月 / 年 / 累计）：库还没跑 026 迁移（monthly_wave 不存在）时退回日明细口径，
/// 个人页不因为少一张表打不开（与 [my_minutes] 对 007 的处理同款）。
pub async fn my_wave(
    st: &AppState,
    person_id: u64,
    scope: &MinutesScope<'_>,
) -> anyhow::Result<i64> {
    let source = sid(st);
    let rows = dialect::query_month_wave_fallback(
        &st.pools,
        &source,
        &my_wave_sql(person_id, scope, true),
        &my_wave_sql(person_id, scope, false),
    )
    .await?;
    Ok(rows.first().map(|r| row_i64(r, "wave")).unwrap_or(0))
}

/// 个人「时长」（月 / 年 / 累计）：库还没跑 007 迁移（monthly_duration 不存在）时退回日明细口径，
/// 个人页不因为少一张表打不开。
pub async fn my_minutes(
    st: &AppState,
    person_id: u64,
    scope: &MinutesScope<'_>,
) -> anyhow::Result<i64> {
    let source = sid(st);
    let sql = my_minutes_sql(person_id, scope, true);
    let rows = match pgrs::query(&st.pools, &source, &sql).await {
        Ok(rows) => rows,
        Err(e) if dialect::is_missing_month_table(&e) => {
            let sql = my_minutes_sql(person_id, scope, false);
            pgrs::query(&st.pools, &source, &sql).await?
        }
        Err(e) => return Err(e),
    };
    Ok(rows.first().map(|r| row_i64(r, "minutes")).unwrap_or(0))
}

/// GET /api/public/teams/{team}/my（token 走 X-My-Token 头）
/// 个人数据：日/月/年 + 累计 + 团内名次（无密码、无抖音号字段）
pub async fn my(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    // 2026-10-12 起 MyQuery 新增族谱用的 period/date；/my 只用 days 语义，
    // 其余字段忽略（不参与个人页口径）。
    Query(_q): Query<MyQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    let (person_id, my_team, _) = my_token_required(&headers)?;
    if !super::my_team_allows(&my_team, &team) {
        return Err(ApiError::bad_request("该账号不属于本团"));
    }

    // 个人各口径
    pub(super) async fn one(st: &AppState, sql: String) -> anyhow::Result<Value> {
        let source = sid(st);
        let rows = pgrs::query(&st.pools, &source, &sql).await?;
        Ok(rows
            .into_iter()
            .next()
            .map(|r| {
                json!({
                    "wave": row_i64(&r, "wave"),
                    "minutes": row_i64(&r, "minutes"),
                })
            })
            .unwrap_or(json!({ "wave": 0, "minutes": 0 })))
    }
    /* 「今日」必须是库里最新的业务日，不能用自然日今天。
    数据是每晚 24 点后补前一天的，凌晨打开页面时今天的记录还不存在，
    用自然日会让个人页显示全 0，跟榜单对不上。 */
    let dates = crate::repos::get_available_dates(&st, None, false).await?;
    let today = dates.first().cloned().unwrap_or_default();
    let month = today
        .get(..7)
        .map(str::to_string)
        .unwrap_or_else(|| chrono::Local::now().format("%Y-%m").to_string());
    let year = today
        .get(..4)
        .map(str::to_string)
        .unwrap_or_else(|| chrono::Local::now().format("%Y").to_string());

    /* 同期对照（1.0.1）：昨日 = 上一业务日（与日榜名次变动同口径）、上月、去年同期。 */
    let prev = prev_periods_of(&today);
    let mut prev = prev;
    prev.day = dates.get(1).cloned();

    /* 个人各口径音浪（P1-7：与榜单口径一致，只聚合可信行 wave_reliable=1；
    total 的口径选择见 repos::get_waves 注释 —— 含基线日，但基线日行本身
    wave_reliable=0 不可信，被行级过滤后不影响数值，只保留在日期轴上）。 */
    let zero = || json!({ "wave": 0, "minutes": 0 });
    let mut daily = if s.daily && !today.is_empty() {
        Some(
            one(
                &st,
                format!(
                    "SELECT (SUM(wave))::BIGINT AS wave, (SUM(minutes))::BIGINT AS minutes \
             FROM daily_metric WHERE person_id = {person_id} AND wave_reliable = 1 \
             AND TO_CHAR(biz_date, 'YYYY-MM-DD') = '{today}'"
                ),
            )
            .await?,
        )
    } else if s.daily {
        Some(zero())
    } else {
        None
    };
    let mut monthly = if s.monthly { Some(zero()) } else { None };
    let mut yearly = if s.yearly { Some(zero()) } else { None };
    let mut total = one(
        &st,
        format!(
            "SELECT (SUM(wave))::BIGINT AS wave, (SUM(minutes))::BIGINT AS minutes \
         FROM daily_metric WHERE person_id = {person_id} AND wave_reliable = 1"
        ),
    )
    .await?;

    /* 月 / 年口径（2026-10-10 月度时长导入，2026-10-16 月度音浪导入）：
    「某月有导入的整月值就用它，没有才用日明细求和」—— 音浪（monthly_wave）与时长
    （monthly_duration）同一套 COALESCE 口径，两张整月表缺哪张就各自回落哪张。
    所以月 / 年块先建成空的（只走开关判定），数值统一由 my_wave / my_minutes 填，
    不再单独跑一条日明细 SQL（那条查出来的 wave / minutes 都会被覆盖，白跑一次）。
    **累计（total）口径不变**：整月值只是月内日明细的替代，累计再加一遍会把同一个月算两次。 */
    if let Some(v) = monthly.as_mut() {
        v["wave"] = json!(my_wave(&st, person_id, &MinutesScope::Month(&month)).await?);
        v["minutes"] = json!(my_minutes(&st, person_id, &MinutesScope::Month(&month)).await?);
    }
    if let Some(v) = yearly.as_mut() {
        v["wave"] = json!(my_wave(&st, person_id, &MinutesScope::Year(&year)).await?);
        v["minutes"] = json!(my_minutes(&st, person_id, &MinutesScope::Year(&year)).await?);
    }
    total["minutes"] = json!(my_minutes(&st, person_id, &MinutesScope::Total).await?);

    /* 同期对照（1.0.1）：本人三个上期音浪，一条 SQL。上期没数据 → NULL → 前端不显示差值。
    2026-10-16：pm（上月同期）走「整月导入值优先」口径；库没跑 026（monthly_wave 缺）时
    由 dialect::query_month_wave_fallback 换纯日明细版重试 —— 与 my_wave / my_minutes
    同一套缺表探测，个人页不因为少一张表打不开。pd / py 仍是日明细口径（原因见函数注释）。 */
    let mut prev_wave_map: HashMap<&str, Option<i64>> = HashMap::new();
    if s.wave && !today.is_empty() {
        let source = sid(&st);
        let pid = person_id.to_string();
        let rows = dialect::query_month_wave_fallback(
            &st.pools,
            &source,
            &self_prev_wave_sql(&prev).replace("{pid}", &pid),
            &self_prev_wave_sql_daily(&prev).replace("{pid}", &pid),
        )
        .await?;
        if let Some(r) = rows.into_iter().next() {
            let get = |col: &str| -> Option<i64> {
                r.try_get::<i64, _>(col)
                    .ok()
                    .or_else(|| r.try_get::<Option<i64>, _>(col).ok().flatten())
            };
            prev_wave_map.insert("daily", get("pd"));
            prev_wave_map.insert("monthly", get("pm"));
            prev_wave_map.insert("yearly", get("py"));
        }
    }
    let attach_prev = |v: &mut Value, key: &str| {
        if let Some(o) = v.as_object_mut() {
            o.insert(
                "prevWave".into(),
                match prev_wave_map.get(key) {
                    Some(Some(n)) => json!(n),
                    _ => Value::Null,
                },
            );
        }
    };
    if let Some(v) = daily.as_mut() {
        attach_prev(v, "daily");
    }
    if let Some(v) = monthly.as_mut() {
        attach_prev(v, "monthly");
    }
    if let Some(v) = yearly.as_mut() {
        attach_prev(v, "yearly");
    }

    let source = sid(&st);
    /* 团内名次（按累计）。口径与 daily 榜单对齐：离队的不占名次，
    否则粉丝页显示的名次会和榜单差几个人，对不上。
    P1-7：只聚合可信行（wave_reliable=1）。
    2026-10-11 起按团过滤：以前不过滤，男女团同表混排，「团内名次」名不副实。
    过滤手法与 lineage 相同：团别为空串 = 全公会不过滤（保持旧行为）；
    token 里的团别只会是 male/female/all/空串（all 归一成空串——gender 列没有
    all 这个值，不过滤才对），与 lineage 的 query_team 同级信任，沿用 format! 拼 SQL。 */
    let rank_team = if my_team == "all" { "" } else { &my_team };
    let rank_sql = format!(
        "SELECT person_id, tw FROM (SELECT p.id AS person_id, \
          (SUM(d.wave))::BIGINT AS tw \
         FROM daily_metric d JOIN person p ON p.id = d.person_id \
         WHERE p.deleted_at IS NULL \
           AND ('{rank_team}' = '' OR p.gender = '{rank_team}') \
           AND d.wave_reliable = 1 \
         GROUP BY p.id ORDER BY tw DESC) t"
    );
    let ranks = pgrs::query(&st.pools, &source, &rank_sql).await?;
    let my_pos = ranks.iter().position(|r| id_of_pub(r) == person_id);
    let team_rank = my_pos.map(|i| i + 1).unwrap_or(0);
    /* 2026-10-12 用户口径：我的页只显示自己的排名。rankGap / teamSize 不再计算
    也不再下发（响应里恒 null）——rank_sql 本身保留（teamRank 还要用），
    前一名差值和团总人数的查询省掉：每次打开我的页少一条 COUNT 查询，
    也不再让多余数值出网。 */

    /* 姓名 + 师傅姓名一条 SQL 带出（LEFT JOIN，没有师傅 = NULL → 前端「师承【xxx】」
    副标题不显示）。师傅姓名在公开族谱里本来就下发（lineage 节点的 masterName），
    没有额外的团别/隐私顾虑。 */
    let name_sql = format!(
        "SELECT p.name, m.name AS master_name FROM person p \
         LEFT JOIN person m ON m.id = p.master_id AND m.deleted_at IS NULL \
         WHERE p.id = {person_id} AND p.deleted_at IS NULL LIMIT 1"
    );
    let (name, master_name) = pgrs::query(&st.pools, &source, &name_sql)
        .await?
        .into_iter()
        .next()
        .map(|r| {
            (
                r.try_get::<String, _>("name").unwrap_or_default(),
                r.try_get::<Option<String>, _>("master_name").ok().flatten(),
            )
        })
        .unwrap_or_default();

    /* 数值显隐（与每日报告同一套后台开关）：关「音浪数值」→ wave=null；
    关「直播时长」→ minutes=null。名次/人数不是数值，照常下发。 */
    for v in [&mut daily, &mut monthly, &mut yearly]
        .into_iter()
        .flatten()
    {
        if !s.wave {
            v["wave"] = Value::Null;
        }
        if !s.duration {
            v["minutes"] = Value::Null;
        }
    }
    if !s.wave {
        total["wave"] = Value::Null;
    }
    if !s.duration {
        total["minutes"] = Value::Null;
    }
    if !s.wave {
        // 音浪数值都隐藏了，同期对照也不能给（可反推）
        for v in [&mut daily, &mut monthly, &mut yearly]
            .into_iter()
            .flatten()
        {
            if let Some(o) = v.as_object_mut() {
                o.insert("prevWave".into(), Value::Null);
            }
        }
    }

    // 「徒弟 + 师傅」门下累计音浪（2026-10-11）：师傅自己 + 所有徒弟的累计音浪。
    let (disciple_count, disciple_wave, total_wave) =
        lineage_wave_summary(&st, &my_team, person_id)
            .await
            .unwrap_or((0, 0, 0));

    Ok(Json(json!({
        "published": true,
        "personId": person_id,
        "name": name,
        "date": today,
        "daily": daily,
        "monthly": monthly,
        "yearly": yearly,
        "total": total,
        "teamRank": team_rank,
        /* 2026-10-12 用户口径：我的页只显示自己的排名，不再下发 teamSize / rankGap。
        团总人数 + 距上一名差值能反推团队分布（结合排名可推出前面所有人的量级），
        与「可反推的一律 null」的安全口径一致。字段保留位置，前端老版本读 null 不炸。 */
        "teamSize": Value::Null,
        "rankGap": Value::Null,
        // 本人的师傅姓名（展示端「我的」页副标题「师承【xxx】」），没有师傅 = null
        "masterName": master_name,
        // 门下音浪统计：直系徒弟数 / 徒弟音浪总和 / 门下累计（自己 + 徒弟）
        "disciple": {
            "count": disciple_count,
            "wave": if s.wave { json!(disciple_wave) } else { Value::Null },
            "totalWave": if s.wave { json!(total_wave) } else { Value::Null },
        },
        // 管理员观看账号（personId=0）：前端据此隐藏「绑定抖音号」表单
        "admin": person_id == 0,
    })))
}

/// GET /api/public/teams/{team}/my/series?days=（token 走 X-My-Token 头）
/// 本人近 N 个业务日的音浪与时长（按日期正序）。只返回这一个人的序列，
/// 不下发密码，也不下发团里任何别人的数据。
pub async fn my_series(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<MyQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    let (person_id, my_team, _) = my_token_required(&headers)?;
    if !super::my_team_allows(&my_team, &team) {
        return Err(ApiError::bad_request("该账号不属于本团"));
    }
    let days = q.days.unwrap_or(14).clamp(1, 30);
    let dates = crate::repos::get_available_dates(&st, None, false).await?;
    let mut take: Vec<String> = dates.into_iter().take(days as usize).collect();
    take.reverse(); // 折线图要正序：老 → 新
    let empty_dates: Vec<String> = Vec::new();
    if take.is_empty() {
        return Ok(Json(
            json!({ "published": true, "dates": empty_dates, "waves": [], "minutes": [] }),
        ));
    }

    /* 2026-10-14 审计 P1（按 Lead 口径：消除 SQL 拼接，改日期窗口过滤 + 绑定参数）。
    Lead 已核实：原实现**不是**轴错位 bug——SQL 取最近 N 个有数据的日期，轴取最近
    N 个全局业务日，个人数据日 ⊆ 全局业务日，两者天然一致。真正缺陷是 format! 拼
    {person_id} 和 LIMIT {days}。现改为绑定参数窗口：终点 = 轴最新业务日，起点 =
    终点往前第 days-1 个**轴内**业务日（聚合日期与轴逐日对齐的保证不变，不再需要
    LIMIT）。person_id 与两个日期全部走 ? 占位符（pgrs::rewrite 统一转 $N）。
    bind 先例：push.rs month_wave_map 在同一 active 池上 `TO_CHAR(biz_date,…) = ?`
    绑定正常；「该库 bind 报 1835」是 MySQL 时代的口径——2026-10 收敛后业务库只有
    PostgreSQL（pgrs Pools 只装 PgPool），本地库 `biz_date = ?::date` 亦为例证。 */
    let window_start_idx = take.len().saturating_sub(days as usize);
    // 窗口边界独立成 String（不借 take）：闭包按值 move 它们，之后 take 照常 move 进响应
    let window_end = take.last().expect("take 非空已在上方提前返回").clone();
    let window_start = take[window_start_idx].clone();
    let sql = "SELECT TO_CHAR(biz_date, 'YYYY-MM-DD') AS d, \
         (SUM(wave))::BIGINT AS wave, (SUM(minutes))::BIGINT AS minutes \
         FROM daily_metric WHERE person_id = ? AND wave_reliable = 1 \
         AND biz_date >= ?::date AND biz_date <= ?::date \
         GROUP BY d ORDER BY d ASC";
    let rows = {
        // 闭包要求 'static：直接把 String **move** 进闭包（E0597 —— 借用活不过语句）。
        // 先把要 move 的值在闭包外准备好，闭包体里再解引用，避免与 take 的 move 冲突。
        let ws = window_start.clone();
        let we = window_end.clone();
        pgrs::query_with_binds(&st.pools, &sid(&st), sql, move |q| {
            q.bind(person_id as i64).bind(ws).bind(we)
        })
        .await?
    };
    let mut wave_map: HashMap<String, i64> = HashMap::new();
    let mut min_map: HashMap<String, i64> = HashMap::new();
    for r in &rows {
        let d = r.try_get::<String, _>("d").unwrap_or_default();
        wave_map.insert(d.clone(), row_i64(r, "wave"));
        min_map.insert(d, row_i64(r, "minutes"));
    }
    let waves: Vec<i64> = take
        .iter()
        .map(|d| wave_map.get(d).copied().unwrap_or(0))
        .collect();
    let minutes: Vec<i64> = take
        .iter()
        .map(|d| min_map.get(d).copied().unwrap_or(0))
        .collect();
    Ok(Json(json!({
        "published": true,
        "dates": take,
        // 后台关掉「音浪数值」：序列不下发数字，前端不画图
        "waves": if s.wave { json!(waves) } else { Value::Null },
        "minutes": if s.duration { json!(minutes) } else { Value::Null },
        "durationEnabled": s.duration,
    })))
}

/* --------------------- 我的抖音号（分账号音浪 2026-10-12） --------------------- */

/// 分账号音浪的四个窗口（2026-10-12 建）：今天的业务日 / 本月 / 今年。
/// 四窗口都由**库里最新业务日**推出，不能用自然日——数据是每晚补前一天的，
/// 凌晨打开页面时「今天」的行还不存在，用自然日会让每个号都显示 0（与 /my 同因）。
pub struct AccountPeriods {
    /// 最新业务日（YYYY-MM-DD）；库里没数据时为空串
    pub day: String,
    /// 业务日所在月（YYYY-MM）
    pub month: String,
    /// 业务日所在年（YYYY）
    pub year: String,
}

impl AccountPeriods {
    /// 由最新业务日推出三个窗口。空串输入 → 三个窗口都空（SQL 侧退化成「不命中任何行」）。
    pub fn of(today: &str) -> Self {
        Self {
            day: today.to_string(),
            month: today.get(..7).map(str::to_string).unwrap_or_default(),
            year: today.get(..4).map(str::to_string).unwrap_or_default(),
        }
    }
}

/// 分账号音浪 SQL（**一条查全部号**，不每人每号循环查库）。
///
/// 口径与 dialect::WAVES_A 完全对齐（2026-10-12），不许自己发明新口径：
/// - 主号（is_primary=1）走 daily_metric，且只算 wave_reliable=1 的行；
/// - 副号（is_primary=0）走 wave_snapshot.wave_value（当日值语义，已验证与 daily_metric 对得上）。
///
/// 两边按「号」UNION ALL 后再按 account.id 汇总，所以一个号的历史不会被算两遍：
/// 主号即便在 wave_snapshot 里也有历史行，也只取 daily_metric 那一支（与 WAVES_A 的
/// is_primary 互斥条件同源，改一处即可，别在两处各写一套）。
///
/// 窗口一律 `TO_CHAR(biz_date, ...)`（该库对 bind 参数报 1835，日期必须内联；
/// 值来自库里的 TO_CHAR 结果且经 [valid_date_pub] 形状校验，不含外部输入）。
/// 没有数据的号 SUM 出来是 NULL，调用方用 COALESCE 归零（有这个号但没播 = 0，
/// 只有后台开关关掉才下发 null，两者语义不同，前端靠它区分）。
pub fn my_accounts_sql(person_id: u64, p: &AccountPeriods) -> String {
    /* 月 / 年口径（2026-10-17 修复，审计 P0-2）：
    /my 的月卡 / 年卡走 `my_wave_sql`（整月音浪 monthly_wave 优先、缺月回落日明细），
    而这里原先**完全没有 monthly_wave**，主号那一支就是裸 `SUM(daily_metric.wave)`。
    导了整月音浪后，同一台手机上「音浪对比」卡本月 50 万、紧挨着的「我的抖音号」卡
    合计行本月 48 万，两卡自相矛盾（主播会截图来问）。

    难点：整月值是**人**粒度，分号卡是**号**粒度，没法把整月值拆到每个号上。
    取法（与 /my 同口径、且不双算）：
      · 主号那一支的月 / 年 = 整月值优先（有人粒度整月值就用它，没有才用主号日明细，
        且日明细只算「该月没导过整月值」的月份，避免同一个月算两次）；
      · 副号那一支仍走 wave_snapshot（副号本来就没有整月值）；
      · 日 / 累计不变（累计口径全局都是纯日明细，见 my_wave_sql 的 Total 分支）。
    这样两个卡的月 / 年数字对齐，合计 = 整月值(或主号日明细) + 副号。 */
    format!(
        "SELECT a.id AS aid, a.anchor_id, a.douyin_no, a.anchor_name, a.is_primary, \
           (COALESCE(SUM(CASE WHEN t.d = '{day}' THEN t.w END), 0))::BIGINT AS wd, \
           (COALESCE(SUM(CASE WHEN t.d LIKE '{month}-%' THEN t.w END), 0))::BIGINT AS wm, \
           (COALESCE(SUM(CASE WHEN t.d LIKE '{year}-%' THEN t.w END), 0))::BIGINT AS wy, \
           (COALESCE(SUM(t.w), 0))::BIGINT AS wt, \
           MAX(t.d) AS last_d, \
           COUNT(DISTINCT t.d) AS days, \
           (COALESCE(MIN(a.is_primary), 0))::SMALLINT AS is_prim, \
           COALESCE((SELECT SUM(mw.wave) FROM monthly_wave mw \
                      WHERE mw.person_id = {person_id} AND mw.period = '{month}'), 0)::BIGINT AS mw_m, \
           COALESCE((SELECT SUM(mw.wave) FROM monthly_wave mw \
                      WHERE mw.person_id = {person_id} AND mw.period LIKE '{year}-%'), 0)::BIGINT AS mw_y \
         FROM account a LEFT JOIN ( \
           SELECT d.anchor_id AS anchor_id, TO_CHAR(d.biz_date, 'YYYY-MM-DD') AS d, \
                  d.wave AS w \
           FROM daily_metric d JOIN account pa \
             ON pa.person_id = d.person_id AND pa.anchor_id = d.anchor_id \
              AND pa.is_primary = 1 \
           WHERE d.person_id = {person_id} AND d.wave_reliable = 1 \
             AND TO_CHAR(d.biz_date, 'YYYY-MM') NOT IN \
                 (SELECT mw0.period FROM monthly_wave mw0 WHERE mw0.person_id = {person_id}) \
           UNION ALL \
           SELECT ws.anchor_id AS anchor_id, TO_CHAR(ws.biz_date, 'YYYY-MM-DD') AS d, \
                  ws.wave_value AS w \
           FROM wave_snapshot ws JOIN account sa \
             ON sa.anchor_id = ws.anchor_id AND sa.is_primary = 0 \
           WHERE sa.person_id = {person_id} \
         ) t ON t.anchor_id = a.anchor_id \
         WHERE a.person_id = {person_id} \
         GROUP BY a.id, a.anchor_id, a.douyin_no, a.anchor_name, a.is_primary \
         ORDER BY a.is_primary DESC, a.id",
        day = p.day,
        month = p.month,
        year = p.year,
    )
}

/// 主号那一行的月 / 年音浪：有整月音浪（monthly_wave）就用它，没有才用主号日明细
/// （2026-10-17 修复，审计 P0-2）。与 [`my_wave_sql`] 同口径，两处不会双算：
/// 上面的日明细侧已经排掉「该月导过整月值」的行，所以这里可以直接取整，
/// 整月值缺失时标量为 0 → COALESCE 落到日明细。
pub(super) fn account_month_wave(
    r: &sqlx::postgres::PgRow,
    monthly: i64,
    yearly: i64,
) -> (i64, i64) {
    let is_primary = r
        .try_get::<i16, _>("is_prim")
        .or_else(|_| r.try_get::<i16, _>("is_primary"))
        .unwrap_or(0)
        == 1;
    if !is_primary {
        return (monthly, yearly);
    }
    let mw_m = r.try_get::<i64, _>("mw_m").unwrap_or(0);
    let mw_y = r.try_get::<i64, _>("mw_y").unwrap_or(0);
    (
        if mw_m > 0 { mw_m } else { monthly },
        if mw_y > 0 { mw_y } else { yearly },
    )
}

/// 一行查询结果 → 一个抖音号的 DTO（数值部分先全给，开关过滤在 [my_accounts_gate] 里统一做）。
pub(super) fn my_account_row(r: &sqlx::postgres::PgRow) -> Value {
    let anchor_id = r.try_get::<String, _>("anchor_id").unwrap_or_default();
    let douyin_no = r.try_get::<String, _>("douyin_no").unwrap_or_default();
    let anchor_name = r.try_get::<String, _>("anchor_name").unwrap_or_default();
    let primary = r.try_get::<i16, _>("is_primary").unwrap_or(0) == 1;
    // 副号没有时长，也不判未播：这里只有音浪口径
    let last_date = r
        .try_get::<Option<String>, _>("last_d")
        .ok()
        .flatten()
        .filter(|s: &String| valid_date_pub(s));
    // 主号的月 / 年走整月音浪优先（与 /my 月卡对齐，见 account_month_wave）
    let (wm, wy) = account_month_wave(r, row_i64(r, "wm"), row_i64(r, "wy"));
    json!({
        "accountId": row_pid(r, "aid"),
        "anchorId": anchor_id,
        "douyinNo": douyin_no,
        "anchorName": anchor_name,
        "primary": primary,
        "daily": { "wave": row_i64(r, "wd") },
        "monthly": { "wave": wm },
        "yearly": { "wave": wy },
        "total": { "wave": row_i64(r, "wt") },
        /* 当日是否开播：与全站同口径（dialect::NOT_LIVE_WAVE_THRESHOLD，音浪 >= 2）。
        注意这是「号」级的开播，副号有音浪就算播了。 */
        "liveToday": dialect::is_live_wave(row_i64(r, "wd")),
        "lastDate": last_date,
        "days": row_i64(r, "days"),
    })
}

/// 后台显隐开关过滤（纯函数，便于单测）：
/// - 关「每日音浪」或关「音浪数值」→ daily 置 null（两者任一关都不给数字，否则可反推）；
/// - 关「月报」→ monthly null；关「年报」→ yearly null；关「音浪数值」→ total null；
/// - 开播标记 / 最后日期 / 天数不是音浪数值，照常下发（与 /my 里名次照常下发同理）。
///
/// 顺带回一份全号加总（与 /my 的合计口径一致：主号 daily_metric + 副号 wave_snapshot，
/// 因此 summary 与 /my 的 total 可以对得上，前端卡顶直接显示它）。
pub fn my_accounts_gate(items: &mut [Value], s: &PublicSettings) -> Value {
    let mut sum_d: i64 = 0;
    let mut sum_m: i64 = 0;
    let mut sum_y: i64 = 0;
    let mut sum_t: i64 = 0;
    for it in items.iter_mut() {
        let Some(o) = it.as_object_mut() else {
            continue;
        };
        let pick = |o: &serde_json::Map<String, Value>, k: &str| -> i64 {
            o.get(k)
                .and_then(|v| v.get("wave"))
                .and_then(Value::as_i64)
                .unwrap_or(0)
        };
        sum_d += pick(o, "daily");
        sum_m += pick(o, "monthly");
        sum_y += pick(o, "yearly");
        sum_t += pick(o, "total");
        if !(s.daily && s.wave) {
            o.insert("daily".into(), Value::Null);
        }
        if !s.monthly {
            o.insert("monthly".into(), Value::Null);
        }
        if !s.yearly {
            o.insert("yearly".into(), Value::Null);
        }
        if !s.wave {
            o.insert("total".into(), Value::Null);
        }
    }
    let one = |on: bool, v: i64| -> Value {
        if on {
            json!({ "wave": v })
        } else {
            Value::Null
        }
    };
    json!({
        "count": items.len(),
        "daily": one(s.daily && s.wave, sum_d),
        "monthly": one(s.monthly, sum_m),
        "yearly": one(s.yearly, sum_y),
        "total": one(s.wave, sum_t),
    })
}

/// GET /api/public/teams/{team}/my/accounts（token 走 X-My-Token 头）
/// 我的**每个抖音号**的日 / 月 / 年 / 累计音浪（一人多号时 /my 只有合计数，看不出哪个号在跑）。
/// 只下发本人数据；数值一律受后台「展示开关」控制，关掉的口径下发 null。
pub async fn my_accounts(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(_q): Query<MyQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    let (person_id, my_team, _) = my_token_required(&headers)?;
    if !super::my_team_allows(&my_team, &team) {
        return Err(ApiError::auth("该账号不属于本团"));
    }
    // 管理员观看账号（personId=0）没有主播档案，也就没有抖音号：给空列表而不是报错，
    // 前端「我的」页顶部照常显示（与 /my 的 admin 分支同处理方式）
    if person_id == 0 {
        let mut empty: Vec<Value> = Vec::new();
        let summary = my_accounts_gate(&mut empty, &s);
        return Ok(Json(json!({
            "published": true,
            "date": "",
            "accounts": empty,
            "summary": summary,
        })));
    }

    /* 「今日」必须是库里最新的业务日，不能用自然日今天（数据与 /my 同因：每晚补前一天）。 */
    let dates = crate::repos::get_available_dates(&st, None, false).await?;
    let today = dates.first().cloned().unwrap_or_default();
    // 日期来自库里的 TO_CHAR 结果，这里再校一次形状，防止异常值内联进 SQL
    if !today.is_empty() && !valid_date_pub(&today) {
        return Err(ApiError::bad_request("业务日格式异常"));
    }
    let periods = AccountPeriods::of(&today);

    let sql = my_accounts_sql(person_id, &periods);
    let rows = pgrs::query(&st.pools, &sid(&st), &sql).await?;
    let mut accounts: Vec<Value> = rows.iter().map(my_account_row).collect();
    let summary = my_accounts_gate(&mut accounts, &s);
    Ok(Json(json!({
        "published": true,
        "date": today,
        "accounts": accounts,
        "summary": summary,
    })))
}

/// GET /api/public/teams/{team}/my/who（token 走 X-My-Token 头）：轻量身份
/// （展示端登录门启动时用），只回姓名 + personId，不拉任何业务数据。
/// 2026-10-16：原先附带的 `bindRequest`（绑定申请审核进度）随「抖音号审计」整页下线
/// 一并去掉 —— 展示端已不再提交绑定申请，前端也不再读这个字段。
pub async fn my_who(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(_q): Query<MyQuery>,
    headers: HeaderMap,
) -> ApiResult {
    // 登录态持久化（2026-10-12）：内存 miss 时从 Redis 回填——服务重启后
    // 24h 内的登录不再被强制二次登录。
    let (person_id, my_team, token_name) = my_token_required_with_restore(&st, &headers).await?;
    if !super::my_team_allows(&my_team, &team) {
        return Err(ApiError::auth("该账号不属于本团"));
    }
    /* 可看团别（迁移 024）：who 是前端每次启动/恢复登录态都要问的身份接口，
    `switchable` 在这里补发一次 —— 用户刷新页面时不会重新走登录接口，
    切换器只能靠这里重建（否则刷新一次切换器就没了）。 */
    let view_team = crate::store::my_account_view_team(&st, &token_name).await;
    let switchable = view_team.switchable();
    // personId=0 有两种：管理员观看账号（is_admin=1，姓名直接来自登录账号），
    // 以及「手机号注册了、还没绑主播」的账号（bound=0，前端要提示去绑定）
    if person_id == 0 {
        if let Some(info) = crate::store::my_account_info(&st, &token_name).await {
            if !info.bound && !info.is_admin {
                return Ok(Json(json!({
                    "ok": true, "personId": 0, "name": info.phone, "team": my_team,
                    "phone": info.phone, "admin": false, "bound": false, "needBind": true,
                    "switchable": switchable,
                })));
            }
            if info.is_admin {
                return Ok(Json(json!({
                    "ok": true, "personId": 0, "name": token_name, "team": my_team,
                    "admin": true, "bound": true, "switchable": switchable,
                })));
            }
        }
        return Ok(Json(json!({
            "ok": true, "personId": 0, "name": token_name, "team": my_team,
            "admin": true, "bound": true, "switchable": switchable,
        })));
    }
    let sql = "SELECT name FROM person WHERE id = ? AND deleted_at IS NULL".to_string();
    let rows =
        pgrs::query_with_binds(&st.pools, &sid(&st), &sql, |q| q.bind(person_id as i64)).await?;
    let name = rows
        .first()
        .and_then(|r| r.try_get::<String, _>("name").ok())
        .unwrap_or_default();
    if name.is_empty() {
        return Err(ApiError::auth("账号不存在或已离团，请联系管理员"));
    }
    // 水印口径（2026-10-11 用户要求）：登录后全屏水印显示自己的手机号（打码），
    // who 里带上 phone；账号没填手机号时前端回退艺名
    // 2026-10-14 审计 P1：原先把**明文手机号**下发，打码全靠前端 maskPhone()——
    // 前端漏调（或接口被直接 curl）明文号码就出网了。改成服务端在这里就脱敏
    // （mask_phone 与 my_push 用的同一份），键名保持 phone 不变，前端两处
    // maskPhone() 调用随本日改动删除，避免二次打码把已打码的值再打一遍。
    let phone = crate::store::my_account_phone(&st, &token_name).await;
    let phone_masked = super::wx::mask_phone(&phone);
    Ok(Json(json!({
        "ok": true, "personId": person_id, "name": name, "team": my_team,
        "phone": phone_masked, "bound": true, "switchable": switchable,
    })))
}

/* --------------------------------- 单测 --------------------------------- */

#[cfg(test)]
mod tests {
    use super::*;

    /// 月卡音浪（2026-10-16 用户口径）：某月有导入的整月音浪就用它，没有才回落日明细求和 ——
    /// 与「时长」同一套 COALESCE 写法，同一段音浪不会既算整月值又算日明细。
    #[test]
    fn my_wave_prefers_monthly_import_over_daily_sum() {
        let sql = my_wave_sql(1027, &MinutesScope::Month("2999-01"), true);
        assert!(sql.contains("FROM monthly_wave"), "{sql}");
        assert!(sql.contains("AND period = '2999-01'"), "{sql}");
        assert!(
            sql.contains("NOT IN"),
            "有整月值的月份不能再算一遍日明细：{sql}"
        );
        assert!(
            sql.contains("TO_CHAR(d.biz_date, 'YYYY-MM') = '2999-01'"),
            "回落窗口要带年份，否则跨年串月：{sql}"
        );
        assert!(
            sql.contains("AND d.wave_reliable = 1"),
            "日明细侧只算可信行（基线日是累计值，混进来会算大）：{sql}"
        );
        assert!(
            sql.matches("person_id = 1027").count() >= 3,
            "整月值 / 日明细 / NOT IN 三处都只查本人：{sql}"
        );
    }

    /// 年卡音浪同理：整月值按 `YYYY-%` 前缀取，日明细按年窗口取，缺月才回落。
    #[test]
    fn my_wave_year_window_uses_period_prefix() {
        let sql = my_wave_sql(1027, &MinutesScope::Year("2999"), true);
        assert!(sql.contains("AND period LIKE '2999-%'"), "{sql}");
        assert!(
            sql.contains("TO_CHAR(d.biz_date, 'YYYY') = '2999'"),
            "{sql}"
        );
        assert!(sql.contains("FROM monthly_wave"), "{sql}");
    }

    /// 库没跑 026（monthly_wave 不存在）→ 退回纯日明细口径，SQL 里不能再出现这张表。
    #[test]
    fn my_wave_without_month_table_is_daily_only() {
        let sql = my_wave_sql(1027, &MinutesScope::Month("2999-01"), false);
        assert!(!sql.contains("monthly_wave"), "{sql}");
        assert!(sql.contains("FROM daily_metric"), "{sql}");
        assert!(sql.contains("AND d.wave_reliable = 1"), "{sql}");
        assert!(
            sql.contains("AND TO_CHAR(d.biz_date, 'YYYY-MM') = '2999-01'"),
            "{sql}"
        );
    }

    /// 累计口径不改：历史累计总音浪仍是日明细求和（整月值只是月内替代，累计叠加 = 双算）。
    #[test]
    fn my_wave_total_stays_daily_sum() {
        let sql = my_wave_sql(1027, &MinutesScope::Total, true);
        assert!(!sql.contains("monthly_wave"), "累计不许走整月口径：{sql}");
        assert!(!sql.contains("NOT IN"), "{sql}");
        assert!(sql.contains("FROM daily_metric"), "{sql}");
        assert!(sql.contains("AND d.wave_reliable = 1"), "{sql}");
        // 与当月那版对照：同一个函数在 Total 下退化成无窗口的日明细求和
        assert_eq!(sql, my_wave_sql(1027, &MinutesScope::Total, false));
    }

    /// 上月同期（pm）走整月优先口径（2026-10-16 月度音浪导入）：上月有导入的整月音浪
    /// （monthly_wave）就用它，没有才回落日明细求和 —— 与月卡 / 展示端月榜同一口径。
    #[test]
    fn self_prev_wave_month_prefers_monthly_import() {
        let mut prev = prev_periods_of("2999-09-15");
        prev.day = Some("2999-09-14".to_string());
        assert_eq!(prev.month, "2999-08", "上月同期 = 上一个自然月");
        let sql = self_prev_wave_sql(&prev);
        assert!(
            sql.contains("COALESCE((SELECT SUM(mw.wave) FROM monthly_wave mw"),
            "pm 要整月优先、没有才回落：{sql}"
        );
        assert!(sql.contains("AND mw.period = '2999-08'"), "{sql}");
        assert!(
            sql.contains("(SELECT SUM(d2.wave) FROM daily_metric d2"),
            "回落来源是日明细求和：{sql}"
        );
        assert!(
            sql.contains("AND d2.wave_reliable = 1"),
            "回落侧只算可信行（基线日是累计值）：{sql}"
        );
        assert!(
            sql.contains("TO_CHAR(d2.biz_date, 'YYYY-MM') = '2999-08'"),
            "回落窗口要带年份，否则跨年串月：{sql}"
        );
        assert!(
            sql.contains("{pid}"),
            "person_id 占位符留给调用方替换：{sql}"
        );
    }

    /// pd（前一日）与 py（去年同期）**保持日明细口径**：前一日是单日窗口、去年同期是
    /// 「去年年初至去年今天」的 YTD 部分窗口，整月值无法与部分窗口对齐（会把整月音浪
    /// 算成一天 / 塞进只过了几天的窗口里，凭空虚高）。所以 monthly_wave 只能出现在 pm 一处。
    #[test]
    fn self_prev_wave_day_and_year_stay_daily_only() {
        let prev = PrevPeriods {
            day: Some("2999-09-14".to_string()),
            month: "2999-08".to_string(),
            year: "2998".to_string(),
            year_until: Some("2998-09-15".to_string()),
        };
        let sql = self_prev_wave_sql(&prev);
        // pd：仍是单日条件聚合
        assert!(
            sql.contains("TO_CHAR(biz_date, 'YYYY-MM-DD') = '2999-09-14' THEN wave"),
            "{sql}"
        );
        // py：仍是 YTD 部分窗口（带 until 上限）
        assert!(sql.contains("AND biz_date <= '2998-09-15'"), "{sql}");
        assert!(sql.contains("TO_CHAR(biz_date, 'YYYY') = '2998'"), "{sql}");
        // 整月表只出现在 pm 那一段（在 py 之前）
        assert_eq!(sql.matches("monthly_wave").count(), 1, "{sql}");
        assert!(
            sql.find("monthly_wave").unwrap() < sql.find("AS py").unwrap(),
            "整月值只允许 pm 用：{sql}"
        );

        // 库没跑 026（monthly_wave 缺）→ 纯日明细版：三个窗口全走 daily_metric
        let daily = self_prev_wave_sql_daily(&prev);
        assert!(!daily.contains("monthly_wave"), "{daily}");
        assert!(
            daily.contains("AS pd") && daily.contains("AS pm") && daily.contains("AS py"),
            "三个列名不能变（调用方按 pd/pm/py 读）：{daily}"
        );
        assert!(
            daily.contains("TO_CHAR(biz_date, 'YYYY-MM') = '2999-08' THEN wave"),
            "pm 回落成日明细口径：{daily}"
        );
    }
}
