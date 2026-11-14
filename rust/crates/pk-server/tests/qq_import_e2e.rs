//! QQ CSV 导入真库 E2E（需要 `DB_LOCAL_*` + `DB_WRITER_*`，用例默认 ignored）。
//!
//! 跑法：
//! `cd rust && set -a && source .env && set +a && cargo test -p pk-server --test qq_import_e2e -- --ignored --nocapture`
//!
//! 覆盖当前 QQ 导入语义：
//! 1. 同一 person 的两个主播 ID 在同一次提交里音浪 / 时长求和；
//! 2. 同一 ID 后一次导入覆盖前值，缺项保留，显式 0 覆盖；
//! 3. 按日删除默认同时清掉音浪和时长。
//!
//! 纪律：
//! - 只允许本地测试库，库名 `douyin` / 已知生产主机直接 panic，绝不静默跳过；
//! - 所有测试数据使用 `__e2e_qq` 前缀和独立日期 `2999-11-13`；
//! - 开头、结尾均清理；不创建备份，不打印连接串或密钥。

use pk_server::admin::{commit_import_anchor_only, import_day_delete, DayQuery, ImportRow};
use pk_server::{config, pgrs, state::AppState};
use sqlx::{PgPool, Row};

const PREFIX: &str = "__e2e_qq";
const BIZ_DATE: &str = "2999-11-13";

async fn setup() -> Option<AppState> {
    if std::env::var("DB_LOCAL_HOST").is_err() {
        eprintln!("跳过：未配置 DB_LOCAL_*（此测试需要真实测试库）");
        return None;
    }

    let sources = config::load_sources().expect("sources");
    for source in &sources {
        assert_safe_target("reader", &source.host, &source.database);
    }
    let writer_endpoint = match config::load_writer() {
        Ok(endpoint) => {
            assert_safe_target("writer", &endpoint.host, &endpoint.database);
            Some(endpoint)
        }
        Err(_) => None,
    };

    let pools = pgrs::build_pools(&sources).await.expect("建池失败");
    let writer = match writer_endpoint {
        Some(endpoint) => pk_server::writer::connect(&endpoint).await.ok(),
        None => None,
    };
    let active = sources
        .iter()
        .find(|source| source.id == "LOCAL")
        .or_else(|| sources.first())
        .expect("至少一个数据源")
        .id
        .clone();

    Some(AppState {
        auth: std::sync::Arc::new(pk_server::auth::Auth::from_env()),
        wave_ttl: std::time::Duration::from_secs(60),
        sources,
        pools,
        writer,
        redis: None,
        active: std::sync::Mutex::new(active),
        cache: std::sync::Mutex::new(Default::default()),
        bots: std::sync::OnceLock::new(),
    })
}

fn assert_safe_target(role: &str, host: &str, database: &str) {
    let host_lower = host.trim().to_ascii_lowercase();
    let database_lower = database.trim().to_ascii_lowercase();
    let production_host = host_lower.starts_with("147.90.") || host_lower == "147.90";
    assert!(
        database_lower != "douyin" && !production_host,
        "拒绝运行 QQ E2E：{role} 指向生产库（host={host}, database={database}）"
    );
}

fn need_writer(st: &AppState) -> bool {
    if st.writer.is_none() {
        eprintln!("跳过：douyin_writer 未配置或连接失败");
        return false;
    }
    true
}

/// 清理本次 E2E 的独立日期和 `__e2e_qq` 前缀数据。
///
/// 使用 PG 原生 `$n` 参数，不拼用户输入；`strpos` 而不是 `LIKE`，避免 `_` 被当成通配符。
async fn cleanup_qq_e2e(pool: &PgPool) {
    for sql in [
        "DELETE FROM daily_metric WHERE biz_date = $1::date",
        "DELETE FROM daily_import_contrib WHERE biz_date = $1::date",
        "DELETE FROM wave_snapshot WHERE biz_date = $1::date",
        "DELETE FROM duration_snapshot WHERE biz_date = $1::date",
        "DELETE FROM push_jobs WHERE biz_date = $1::date",
    ] {
        sqlx::query(sql)
            .bind(BIZ_DATE)
            .execute(pool)
            .await
            .unwrap_or_else(|error| panic!("清理 E2E 日期数据失败: {sql}: {error}"));
    }

    for sql in [
        "DELETE FROM import_pending WHERE strpos(name, $1) = 1 OR strpos(douyin_no, $1) = 1",
        "DELETE FROM import_pending_month WHERE strpos(name, $1) = 1 OR strpos(douyin_no, $1) = 1",
        "DELETE FROM import_batch WHERE strpos(filename, $1) = 1",
        "DELETE FROM account WHERE strpos(anchor_id, $1) = 1 OR strpos(douyin_no, $1) = 1",
        "DELETE FROM person WHERE strpos(name, $1) = 1",
    ] {
        sqlx::query(sql)
            .bind(PREFIX)
            .execute(pool)
            .await
            .unwrap_or_else(|error| panic!("清理 E2E 前缀数据失败: {sql}: {error}"));
    }
}

#[tokio::test]
#[ignore = "需要真实 PostgreSQL（DB_LOCAL_* + DB_WRITER_*）；加 -- --ignored 运行"]
async fn qq_import_semantics_are_persisted_and_day_delete_clears_both_metrics() {
    let Some(st) = setup().await else { return };
    if !need_writer(&st) {
        return;
    }
    let st = std::sync::Arc::new(st);
    let pool = st.writer.as_ref().expect("writer");

    cleanup_qq_e2e(pool).await;

    let stamp = chrono::Utc::now().timestamp_millis();
    let person_name = format!("{PREFIX}_person_{stamp}");
    let primary_anchor = format!("{PREFIX}_anchor_primary_{stamp}");
    let secondary_anchor = format!("{PREFIX}_anchor_secondary_{stamp}");

    let person_id: i64 = sqlx::query(
        "INSERT INTO person (name, gender, status)
         VALUES ($1, 'male', 'active')
         RETURNING id",
    )
    .bind(&person_name)
    .fetch_one(pool)
    .await
    .expect("建测试主播")
    .try_get(0)
    .expect("person.id");

    for (anchor, is_primary) in [(&primary_anchor, 1_i16), (&secondary_anchor, 0_i16)] {
        sqlx::query(
            "INSERT INTO account (person_id, anchor_id, douyin_no, anchor_name, is_primary)
             VALUES ($1, $2, '', $3, $4)",
        )
        .bind(person_id)
        .bind(anchor)
        .bind(&person_name)
        .bind(is_primary)
        .execute(pool)
        .await
        .expect("建测试账号");
    }

    eprintln!(
        "[qq_import_e2e] 真跑断言（非跳过）：person_id={person_id} date={BIZ_DATE} prefix={PREFIX}"
    );

    // 1) 两个主播 ID 属于同一 person：QQ 严格 ID 模式按人求和。
    let first = commit_import_anchor_only(
        &st,
        "auto",
        BIZ_DATE,
        vec![
            ImportRow {
                name: person_name.clone(),
                anchor_id: Some(primary_anchor.clone()),
                wave: Some(100.0),
                minutes: Some(60.0),
                ..Default::default()
            },
            ImportRow {
                name: person_name.clone(),
                anchor_id: Some(secondary_anchor.clone()),
                wave: Some(25.0),
                minutes: Some(30.0),
                ..Default::default()
            },
        ],
        "merge",
        &format!("{PREFIX}_sum.csv"),
    )
    .await
    .expect("多主播 ID 汇总导入");
    assert_eq!(first.total, 1, "同 person 两行应在提交层合并成一人");
    assert_eq!(first.ok_rows, 1);
    assert_eq!(first.minutes_total, 90);

    let after_sum = read_metrics(pool, person_id).await;
    assert_eq!(after_sum, (125, 90), "两个主播 ID 的音浪和时长都必须求和");

    // 2) 同一个主播 ID 后值覆盖；缺时长时保留旧值。
    let second = commit_import_anchor_only(
        &st,
        "auto",
        BIZ_DATE,
        vec![ImportRow {
            name: person_name.clone(),
            anchor_id: Some(primary_anchor.clone()),
            wave: Some(200.0),
            minutes: None,
            ..Default::default()
        }],
        "merge",
        &format!("{PREFIX}_wave_override.csv"),
    )
    .await
    .expect("同 ID 覆盖导入");
    assert_eq!(second.ok_rows, 1);
    assert_eq!(second.minutes_total, 0, "缺时长列不计入本次时长合计");

    let after_override = read_metrics(pool, person_id).await;
    assert_eq!(
        after_override,
        (200, 90),
        "后值覆盖音浪，缺失的时长保留旧值"
    );

    // 显式 0 必须覆盖，不能走「缺项保留」分支。
    let zeroed = commit_import_anchor_only(
        &st,
        "auto",
        BIZ_DATE,
        vec![ImportRow {
            name: person_name.clone(),
            anchor_id: Some(primary_anchor.clone()),
            wave: Some(0.0),
            minutes: Some(0.0),
            ..Default::default()
        }],
        "merge",
        &format!("{PREFIX}_explicit_zero.csv"),
    )
    .await
    .expect("显式 0 覆盖导入");
    assert_eq!(zeroed.ok_rows, 1);
    let after_zero = read_metrics(pool, person_id).await;
    assert_eq!(after_zero, (0, 0), "显式 0 必须同时覆盖音浪和时长");

    // 3) 按日删除：先恢复两个正数，再走真实删除接口，确认整行两项都消失。
    let restored = commit_import_anchor_only(
        &st,
        "auto",
        BIZ_DATE,
        vec![ImportRow {
            name: person_name.clone(),
            anchor_id: Some(primary_anchor.clone()),
            wave: Some(333.0),
            minutes: Some(44.0),
            ..Default::default()
        }],
        "merge",
        &format!("{PREFIX}_before_delete.csv"),
    )
    .await
    .expect("删除前恢复数据");
    assert_eq!(restored.ok_rows, 1);
    assert_eq!(read_metrics(pool, person_id).await, (333, 44));

    let deleted = import_day_delete(
        axum::extract::State(st.clone()),
        axum::extract::Query(DayQuery {
            biz_date: BIZ_DATE.into(),
            team: Some("auto".into()),
            metrics: None,
        }),
    )
    .await
    .unwrap_or_else(|_| panic!("按日删除接口失败"));
    let body = deleted.0;
    assert_eq!(body["deletedRows"], 1, "应删除 1 行 daily_metric: {body}");
    assert_eq!(body["affectedRows"], 1, "应影响 1 行 daily_metric: {body}");

    let remaining: Option<(i64, i32)> = sqlx::query_as(
        "SELECT wave, minutes FROM daily_metric
          WHERE person_id = $1 AND biz_date = $2::date",
    )
    .bind(person_id)
    .bind(BIZ_DATE)
    .fetch_optional(pool)
    .await
    .expect("删除后核查 daily_metric");
    assert_eq!(remaining, None, "按日删除必须同时清掉音浪和时长");

    // 结尾清理，并确认本次造的人 / 账号 / 日数据都已消失。
    cleanup_qq_e2e(pool).await;
    let leftovers: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT
           (SELECT COUNT(*) FROM person WHERE strpos(name, $1) = 1),
           (SELECT COUNT(*) FROM account
             WHERE strpos(anchor_id, $1) = 1 OR strpos(douyin_no, $1) = 1),
           (SELECT COUNT(*) FROM daily_metric WHERE biz_date = $2::date),
           (SELECT COUNT(*) FROM import_batch WHERE strpos(filename, $1) = 1)",
    )
    .bind(PREFIX)
    .bind(BIZ_DATE)
    .fetch_one(pool)
    .await
    .expect("核查清理结果");
    assert_eq!(leftovers, (0, 0, 0, 0), "E2E 结尾清理不完整");
}

async fn read_metrics(pool: &PgPool, person_id: i64) -> (i64, i64) {
    let row = sqlx::query(
        "SELECT wave, minutes FROM daily_metric
          WHERE person_id = $1 AND biz_date = $2::date",
    )
    .bind(person_id)
    .bind(BIZ_DATE)
    .fetch_one(pool)
    .await
    .expect("读取 daily_metric");
    (
        row.try_get::<i64, _>("wave").expect("wave"),
        row.try_get::<i32, _>("minutes").expect("minutes") as i64,
    )
}
