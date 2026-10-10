//! 月度时长导入端到端测试：真实 MySQL（需要 DB_LOCAL_* / DB_WRITER_* 环境变量）。
//! 未配置数据库时自动跳过（CI / 本地纯单测不依赖库）。
//!
//! 覆盖 2026-10-10 用户口径「每日不能导入时长，只有月时长」的完整链路：
//!   1. 月度时长文件（一人一行整月累计）→ monthly_duration
//!   2. 值相同不覆盖 / 值不同才覆盖（与按日导入同一套幂等口径）
//!   3. 月榜 / 年榜 / 个人页的「时长」优先用整月值，而不是日明细求和
//!   4. 按日导入不再写 daily_metric.minutes（时长行被忽略）
//!
//! 测试用独立主播（`__e2e_月时长主播__`）与独立业务月份（1999-01），结束即清理，不污染真实数据。

use pk_server::admin::{self, ImportRow};
use pk_server::public_teams::{my_minutes, MinutesScope};
use pk_server::{config, dialect, mysql, state::AppState};
use sqlx::Row;

const NAME: &str = "__e2e_月时长主播__";
const DOUYIN_NO: &str = "__e2e_month_no__";
const ANCHOR_ID: &str = "__e2e_month_anchor__";
const PERIOD: &str = "1999-01";
const YEAR: &str = "1999";

async fn setup() -> Option<std::sync::Arc<AppState>> {
    if std::env::var("DB_LOCAL_HOST").is_err() {
        eprintln!("跳过：未配置 DB_LOCAL_*（此测试需要真实测试库）");
        return None;
    }
    let sources = config::load_sources().expect("sources");
    let pools = mysql::build_pools(&sources).await.expect("建池失败");
    let writer = match config::load_writer() {
        Ok(ep) => pk_server::writer::connect(&ep).await.ok(),
        Err(_) => None,
    };
    let active = sources
        .iter()
        .find(|s| s.id == "LOCAL")
        .or_else(|| sources.first())
        .expect("at least one source")
        .id
        .clone();
    Some(std::sync::Arc::new(AppState {
        auth: std::sync::Arc::new(pk_server::auth::Auth::from_env()),
        wave_ttl: std::time::Duration::from_secs(600),
        sources,
        pools,
        writer,
        active: std::sync::Mutex::new(active),
        cache: std::sync::Mutex::new(Default::default()),
        bots: std::sync::OnceLock::new(),
        wb: std::sync::OnceLock::new(),
    }))
}

fn need_writer(st: &AppState) -> bool {
    if st.writer.is_none() {
        eprintln!("跳过：douyin_writer 未配置");
        return false;
    }
    true
}

/// 清理测试数据（先前跑挂在中间也不会留垃圾）
async fn cleanup(st: &AppState) {
    let pool = st.writer.as_ref().expect("writer");
    for sql in [
        format!("DELETE FROM monthly_duration WHERE period = '{PERIOD}'"),
        format!(
            "DELETE d FROM daily_metric d JOIN person p ON p.id = d.person_id WHERE p.name = '{NAME}'"
        ),
        format!("DELETE FROM account WHERE anchor_id = '{ANCHOR_ID}'"),
        format!("DELETE FROM person WHERE name = '{NAME}'"),
        format!("DELETE FROM import_batch WHERE filename LIKE '__e2e_month%'"),
    ] {
        let _ = sqlx::query(&sql).execute(pool).await;
    }
}

async fn insert_person(st: &AppState) -> u64 {
    let pool = st.writer.as_ref().expect("writer");
    sqlx::query("INSERT INTO person (name, gender, status) VALUES (?, 'male', 'active')")
        .bind(NAME)
        .execute(pool)
        .await
        .expect("建测试主播")
        .last_insert_id()
}

async fn insert_account(st: &AppState, person_id: u64) {
    let pool = st.writer.as_ref().expect("writer");
    sqlx::query(
        "INSERT INTO account (person_id, anchor_id, douyin_no, anchor_name, is_primary, status) \
         VALUES (?, ?, ?, ?, 1, 'active')",
    )
    .bind(person_id as i64)
    .bind(ANCHOR_ID)
    .bind(DOUYIN_NO)
    .bind(NAME)
    .execute(pool)
    .await
    .expect("建测试账号");
}

/// 塞一天日明细（带时长），用于验证「月报优先于日明细求和」
async fn insert_daily(st: &AppState, person_id: u64, date: &str, minutes: i64) {
    let pool = st.writer.as_ref().expect("writer");
    sqlx::query(
        "INSERT INTO daily_metric (person_id, anchor_id, biz_date, wave, cumulative_wave, \
           wave_reliable, minutes, is_live) VALUES (?, ?, ?, 1000, 1000, 1, ?, 1)",
    )
    .bind(person_id as i64)
    .bind(ANCHOR_ID)
    .bind(date)
    .bind(minutes)
    .execute(pool)
    .await
    .expect("写日明细");
}

async fn scalar_i64(st: &AppState, sql: &str) -> i64 {
    mysql::query(&st.pools, "LOCAL", sql)
        .await
        .expect("查询失败")
        .first()
        .map(|r| r.try_get::<i64, _>(0).unwrap_or(0))
        .unwrap_or(-1)
}

/// 行里的 person_id：BIGINT UNSIGNED 用 u64 读，退回 i64
fn row_uid(r: &sqlx::mysql::MySqlRow, col: &str) -> u64 {
    r.try_get::<u64, _>(col)
        .ok()
        .or_else(|| r.try_get::<i64, _>(col).ok().map(|v| v as u64))
        .unwrap_or(0)
}

fn month_row(minutes: f64) -> ImportRow {
    ImportRow {
        name: NAME.to_string(),
        douyin_no: Some(DOUYIN_NO.to_string()),
        minutes: Some(minutes),
        ..Default::default()
    }
}

#[tokio::test]
async fn monthly_duration_import_drives_month_and_year_minutes() {
    let Some(st) = setup().await else { return };
    if !need_writer(&st) {
        return;
    }
    cleanup(&st).await;
    let person_id = insert_person(&st).await;
    insert_account(&st, person_id).await;
    // 该月有两天日明细（时长 60 + 30 = 90 分钟）：月报口径必须压过它
    insert_daily(&st, person_id, "1999-01-05", 60).await;
    insert_daily(&st, person_id, "1999-01-06", 30).await;

    // 1) 月度时长导入：整月 4529 分钟（75小时29分钟41秒）
    let report = admin::commit_month_duration(
        &st,
        PERIOD,
        "auto",
        vec![month_row(4529.0)],
        "merge",
        "__e2e_month.csv",
    )
    .await
    .expect("月度时长导入");
    assert_eq!(report.ok_rows, 1, "命中主播要写进 monthly_duration");
    assert_eq!(report.minutes_total, 4529);
    assert!(
        report.failures.is_empty(),
        "不该有失败：{:?}",
        report.failures
    );
    assert_eq!(
        scalar_i64(
            &st,
            &format!(
                "SELECT COUNT(*) FROM monthly_duration WHERE person_id = {person_id} AND period = '{PERIOD}'"
            )
        )
        .await
        .min(1),
        1
    );

    // 2) 幂等：同一个值重导 → 一行不动（相同不覆盖）
    let again = admin::commit_month_duration(
        &st,
        PERIOD,
        "auto",
        vec![month_row(4529.0)],
        "merge",
        "__e2e_month.csv",
    )
    .await
    .expect("重导");
    assert_eq!(again.ok_rows, 0);
    assert_eq!(again.unchanged, 1, "值相同不覆盖");

    // 3) 值变了才覆盖
    let changed = admin::commit_month_duration(
        &st,
        PERIOD,
        "auto",
        vec![month_row(4800.0)],
        "merge",
        "__e2e_month.csv",
    )
    .await
    .expect("改值重导");
    assert_eq!(changed.ok_rows, 1);

    // 4) 月榜口径：整月值 4800（不是日明细 90）
    let rows = dialect::query_period(
        &st.pools,
        "LOCAL",
        dialect::ADMIN_PERIOD_SELECT,
        &format!("{PERIOD}%"),
        "",
        "",
        "wave DESC",
    )
    .await
    .expect("月榜查询");
    let mine = rows
        .iter()
        .find(|r| row_uid(r, "person_id") == person_id)
        .expect("测试主播应出现在月榜里（他有日明细音浪）");
    assert_eq!(
        mine.try_get::<i64, _>("minutes").unwrap_or(0),
        4800,
        "月榜时长要用导入的整月值，不是日明细求和（90）"
    );

    // 5) 个人页月 / 年 / 累计口径同样是整月值
    assert_eq!(
        my_minutes(&st, person_id, &MinutesScope::Month(PERIOD))
            .await
            .expect("个人月时长"),
        4800
    );
    assert_eq!(
        my_minutes(&st, person_id, &MinutesScope::Year(YEAR))
            .await
            .expect("个人年时长"),
        4800
    );
    assert_eq!(
        my_minutes(&st, person_id, &MinutesScope::Total)
            .await
            .expect("个人累计时长"),
        4800
    );

    // 6) 按日导入不再写时长（用户口径 2026-10-10）：只有时长的行被忽略、不落 daily_metric
    let daily = admin::commit_import(
        &st,
        "auto",
        "1999-01-07",
        vec![month_row(120.0)],
        "merge",
        "__e2e_daily.csv",
    )
    .await
    .expect("按日导入");
    assert_eq!(daily.dropped_minutes, 1, "按日导入的时长要被忽略");
    assert_eq!(daily.ok_rows, 0, "只剩时长的行不该写库");
    assert_eq!(
        scalar_i64(
            &st,
            &format!(
                "SELECT COUNT(*) FROM daily_metric WHERE person_id = {person_id} AND biz_date = '1999-01-07'"
            )
        )
        .await,
        0,
        "按日导入不能写 daily_metric.minutes"
    );

    cleanup(&st).await;
}
