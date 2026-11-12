//! 热路径真库回归：直播批量采样/席位、旧分数版本、按日贡献 has_wave，
//! 以及只有艺名的未认领数据暂存 → 认领补账 / 删除清理。
//!
//! 需要独立测试库环境变量（DB_LOCAL_* + DB_WRITER_*）；未配置时显式跳过。
//! 测试数据使用时间戳 ID，结束即清理，不触碰真实赛事。

use pk_server::admin::{
    absorb_pending, absorb_pending_month, commit_import, commit_month_metrics, import_day_delete,
    import_month_delete, DayQuery, ImportRow, MonthMetrics, MonthQuery,
};
use pk_server::live::{BattleScore, BattleSnapshot, LiveRules, RankedSeat, StageKind};
use pk_server::{config, pgrs, state::AppState, store};
use sqlx::Row;

async fn setup() -> Option<AppState> {
    if std::env::var("DB_LOCAL_HOST").is_err() {
        eprintln!("跳过：未配置 DB_LOCAL_*（此测试需要真实测试库）");
        return None;
    }
    let sources = config::load_sources().expect("sources");
    for s in &sources {
        assert_ne!(
            s.database, "douyin",
            "拒绝在非测试库上运行 live_hot_path_e2e"
        );
    }
    let pools = pgrs::build_pools(&sources).await.expect("建池失败");
    let writer = match config::load_writer() {
        Ok(ep) => {
            assert_ne!(ep.database, "douyin", "拒绝在非测试库上运行");
            pk_server::writer::connect(&ep).await.ok()
        }
        Err(_) => None,
    };
    let active = sources
        .iter()
        .find(|s| s.id == "LOCAL")
        .or_else(|| sources.first())?
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

fn score(uid: &str, value: i64, version: i64, rank: i32) -> BattleScore {
    BattleScore {
        user_id: uid.into(),
        nickname: uid.into(),
        score: value,
        rank,
        score_version: version,
        team_id: String::new(),
        bonus_multiple: 100,
        bonus_score: 0,
        bonus_active: false,
        bonus_end_ms: 0,
        current_bonus: 0,
    }
}

#[tokio::test]
#[ignore = "需要真实 PostgreSQL（DB_LOCAL_* + DB_WRITER_*）；加 -- --ignored 运行"]
async fn batch_writes_and_score_versions_are_monotonic() {
    let Some(st) = setup().await else { return };
    if st.writer.is_none() {
        eprintln!("跳过：douyin_writer 未配置");
        return;
    }
    let stamp = chrono::Utc::now().timestamp_millis();
    let event_id = store::live_event_create(
        &st,
        &format!("__e2e_live_{stamp}"),
        "2999-01-01",
        &LiveRules::default(),
        0,
    )
    .await
    .expect("创建赛事");
    let stage_id = store::live_stage_ensure(&st, event_id, StageKind::Final)
        .await
        .expect("创建阶段");
    let group_id = store::live_group_create(
        &st,
        event_id,
        stage_id,
        StageKind::Final,
        1,
        "__e2e_live",
        3,
        0,
    )
    .await
    .expect("创建小组");

    let high = score(&format!("__e2e_uid_{stamp}"), 200, 20, 1);
    let low = score(&high.user_id, 100, 10, 2);
    let snap = BattleSnapshot {
        battle_id: format!("__e2e_battle_{stamp}"),
        channel_id: "__e2e_channel".into(),
        start_time_ms: stamp,
        duration_secs: 300,
        battle_status: 1,
        scores: vec![high.clone(), low.clone()],
        team_scores: Vec::new(),
    };
    let (round_id, is_new) =
        store::live_round_upsert(&st, event_id, group_id, &snap, "__e2e_room", 0, 1)
            .await
            .expect("创建对局");
    assert!(is_new);

    store::live_sample_insert_batch(&st, round_id, &[(&high, 0, 0), (&low, 0, 0)])
        .await
        .expect("批量采样");
    let latest = store::live_latest_scores(&st, round_id).await;
    assert_eq!(latest[0]["score"], 200, "旧版本不能覆盖最新采样");
    assert_eq!(latest[0]["scoreVersion"], 20);

    let high_seat = RankedSeat {
        user_id: high.user_id.clone(),
        nickname: "高版本".into(),
        score: 200,
        rank: 1,
        team_id: String::new(),
        score_version: 20,
    };
    let low_seat = RankedSeat {
        score: 100,
        score_version: 10,
        ..high_seat.clone()
    };
    store::live_seat_upsert_batch(&st, round_id, &[(&high_seat, 0), (&low_seat, 0)], false)
        .await
        .expect("批量席位");
    let seats = store::live_seat_list(&st, round_id)
        .await
        .expect("席位列表");
    assert_eq!(seats[0]["score"], 200, "旧版本不能覆盖正式席位分");
    assert_eq!(seats[0]["scoreVersion"], 20);

    let web_rid = format!("__e2e_rid_{stamp}");
    store::live_room_sample_insert_batch(
        &st,
        &web_rid,
        "__e2e_room",
        &snap.battle_id,
        &[(&high, 0, 0), (&low, 0, 0)],
    )
    .await
    .expect("独立房间批量采样");
    let room_latest = store::live_room_latest_scores(&st, &web_rid, &snap.battle_id).await;
    assert_eq!(room_latest[0]["score"], 200);
    assert_eq!(room_latest[0]["scoreVersion"], 20);

    let pool = st.writer.as_ref().expect("writer");
    sqlx::query(
        "INSERT INTO live_score_sample (round_id, douyin_uid, score, observed_at)
         VALUES ($1, $2, 1, now() - interval '91 days')",
    )
    .bind(round_id)
    .bind(&high.user_id)
    .execute(pool)
    .await
    .expect("插入过期采样");
    let _ = store::live_samples_prune(&st, 90)
        .await
        .expect("清理过期采样");
    let active_old: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM live_score_sample
          WHERE round_id = $1 AND observed_at < now() - interval '90 days'",
    )
    .bind(round_id)
    .fetch_one(pool)
    .await
    .expect("查进行中旧采样");
    assert_eq!(active_old, 1, "进行中对局的旧采样不能清");

    sqlx::query("UPDATE live_round SET status = 'completed' WHERE id = $1")
        .bind(round_id)
        .execute(pool)
        .await
        .expect("结束对局");
    let _ = store::live_samples_prune(&st, 90)
        .await
        .expect("清理已结束对局旧采样");
    let completed_old: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM live_score_sample
          WHERE round_id = $1 AND observed_at < now() - interval '90 days'",
    )
    .bind(round_id)
    .fetch_one(pool)
    .await
    .expect("查已结束旧采样");
    assert_eq!(completed_old, 0, "已结束对局的过期采样应清理");

    for sql in [
        "DELETE FROM live_score_sample WHERE round_id = $1",
        "DELETE FROM live_round_seat WHERE round_id = $1",
        "DELETE FROM live_round_event WHERE round_id = $1",
        "DELETE FROM live_round WHERE id = $1",
        "DELETE FROM live_group_seat WHERE group_id = $1",
        "DELETE FROM live_group WHERE id = $1",
        "DELETE FROM live_stage WHERE id = $1",
        "DELETE FROM live_event WHERE id = $1",
    ] {
        let id = if sql.contains("round_id") || sql.contains("live_round WHERE") {
            round_id
        } else if sql.contains("group_id") || sql.contains("live_group WHERE") {
            group_id
        } else if sql.contains("live_stage") {
            stage_id
        } else {
            event_id
        };
        let _ = sqlx::query(sql).bind(id).execute(pool).await;
    }
    let _ = sqlx::query("DELETE FROM live_room_score_sample WHERE web_rid = $1")
        .bind(&web_rid)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM live_room_monitor WHERE web_rid = $1")
        .bind(&web_rid)
        .execute(pool)
        .await;
}

#[tokio::test]
#[ignore = "需要真实 PostgreSQL（DB_LOCAL_* + DB_WRITER_*）；加 -- --ignored 运行"]
async fn time_only_import_preserves_existing_wave_contribution() {
    let Some(st) = setup().await else { return };
    let st = std::sync::Arc::new(st);
    if st.writer.is_none() {
        eprintln!("跳过：douyin_writer 未配置");
        return;
    }
    let stamp = chrono::Utc::now().timestamp_millis();
    let name = format!("__e2e_contrib_{stamp}");
    let uid = format!("__e2e_anchor_{stamp}");
    let douyin_no = format!("__e2e_dy_{stamp}");
    let date = "2999-01-02";
    let pool = st.writer.as_ref().expect("writer");
    let person_id: i64 =
        sqlx::query("INSERT INTO person (name, gender) VALUES ($1, 'male') RETURNING id")
            .bind(&name)
            .fetch_one(pool)
            .await
            .expect("建测试主播")
            .try_get("id")
            .expect("person id");
    sqlx::query(
        "INSERT INTO account (person_id, anchor_id, douyin_no, anchor_name, is_primary)
         VALUES ($1, $2, $3, $4, 1)",
    )
    .bind(person_id)
    .bind(&uid)
    .bind(&douyin_no)
    .bind(&name)
    .execute(pool)
    .await
    .expect("建测试账号");

    let wave_report = commit_import(
        &st,
        "auto",
        date,
        vec![ImportRow {
            name: name.clone(),
            douyin_no: Some(douyin_no.clone()),
            anchor_id: Some(uid.clone()),
            wave: Some(1_000.0),
            ..Default::default()
        }],
        "merge",
        "__e2e_wave_only.csv",
    )
    .await
    .expect("音浪批次导入");

    let minutes_report = commit_import(
        &st,
        "auto",
        date,
        vec![ImportRow {
            name: name.clone(),
            douyin_no: Some(douyin_no.clone()),
            anchor_id: Some(uid.clone()),
            minutes: Some(120.0),
            ..Default::default()
        }],
        "merge",
        "__e2e_minutes_only.csv",
    )
    .await
    .expect("时长批次导入");

    let pool = st.writer.as_ref().expect("writer");
    let row = sqlx::query(
        "SELECT wave, (minutes)::BIGINT AS minutes FROM daily_metric
          WHERE person_id = $1 AND biz_date = $2::date",
    )
    .bind(person_id)
    .bind(date)
    .fetch_one(pool)
    .await
    .expect("读取导入结果");
    assert_eq!(row.try_get::<i64, _>("wave").unwrap(), 1_000);
    assert_eq!(
        row.try_get::<i64, _>("minutes").unwrap(),
        120,
        "时长-only 批次必须保留已有音浪"
    );
    assert!(minutes_report.ok_rows >= 1);

    for batch_id in [wave_report.batch_id, minutes_report.batch_id] {
        let _ = sqlx::query("DELETE FROM daily_import_contrib WHERE batch_id = $1")
            .bind(batch_id)
            .execute(pool)
            .await;
        let _ = sqlx::query("DELETE FROM import_batch WHERE id = $1")
            .bind(batch_id)
            .execute(pool)
            .await;
    }
    let _ = sqlx::query("DELETE FROM daily_metric WHERE person_id = $1 AND biz_date = $2::date")
        .bind(person_id)
        .bind(date)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM account WHERE person_id = $1")
        .bind(person_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM person WHERE id = $1")
        .bind(person_id)
        .execute(pool)
        .await;
}

#[tokio::test]
#[ignore = "需要真实 PostgreSQL（DB_LOCAL_* + DB_WRITER_*）；加 -- --ignored 运行"]
async fn monthly_import_bulk_upsert_preserves_unchanged_contract() {
    let Some(st) = setup().await else { return };
    let st = std::sync::Arc::new(st);
    if st.writer.is_none() {
        eprintln!("跳过：douyin_writer 未配置");
        return;
    }
    let stamp = chrono::Utc::now().timestamp_millis();
    let name = format!("__e2e_month_{stamp}");
    let uid = format!("__e2e_month_anchor_{stamp}");
    let douyin_no = format!("__e2e_month_dy_{stamp}");
    let period = "2999-01";
    let pool = st.writer.as_ref().expect("writer");
    let person_id: i64 =
        sqlx::query("INSERT INTO person (name, gender) VALUES ($1, 'female') RETURNING id")
            .bind(&name)
            .fetch_one(pool)
            .await
            .expect("建测试主播")
            .try_get("id")
            .expect("person id");
    sqlx::query(
        "INSERT INTO account (person_id, anchor_id, douyin_no, anchor_name, is_primary)
         VALUES ($1, $2, $3, $4, 1)",
    )
    .bind(person_id)
    .bind(&uid)
    .bind(&douyin_no)
    .bind(&name)
    .execute(pool)
    .await
    .expect("建测试账号");

    let row = ImportRow {
        name: name.clone(),
        douyin_no: Some(douyin_no.clone()),
        anchor_id: Some(uid.clone()),
        wave: Some(1_000.0),
        minutes: Some(60.0),
        ..Default::default()
    };
    let first = commit_month_metrics(
        &st,
        period,
        "auto",
        vec![row.clone()],
        "merge",
        "__e2e_month_1.csv",
        MonthMetrics::BOTH,
    )
    .await
    .expect("月度首次导入");
    assert_eq!(first.ok_rows, 1);
    assert_eq!(first.unchanged, 0);

    let same = commit_month_metrics(
        &st,
        period,
        "auto",
        vec![row.clone()],
        "merge",
        "__e2e_month_2.csv",
        MonthMetrics::BOTH,
    )
    .await
    .expect("月度重复导入");
    assert_eq!(same.ok_rows, 0);
    assert_eq!(same.unchanged, 1);

    let changed = commit_month_metrics(
        &st,
        period,
        "auto",
        vec![ImportRow {
            wave: Some(2_000.0),
            ..row
        }],
        "merge",
        "__e2e_month_3.csv",
        MonthMetrics::BOTH,
    )
    .await
    .expect("月度更新导入");
    assert_eq!(changed.ok_rows, 1);
    assert_eq!(changed.unchanged, 0);

    let wave: i64 =
        sqlx::query_scalar("SELECT wave FROM monthly_wave WHERE person_id = $1 AND period = $2")
            .bind(person_id)
            .bind(period)
            .fetch_one(pool)
            .await
            .expect("月音浪");
    let minutes: i32 = sqlx::query_scalar(
        "SELECT minutes FROM monthly_duration WHERE person_id = $1 AND period = $2",
    )
    .bind(person_id)
    .bind(period)
    .fetch_one(pool)
    .await
    .expect("月时长");
    assert_eq!((wave, minutes), (2_000, 60));

    for report in [first, same, changed] {
        let _ = sqlx::query("DELETE FROM import_batch WHERE id = $1")
            .bind(report.batch_id)
            .execute(pool)
            .await;
    }
    let _ = sqlx::query("DELETE FROM monthly_wave WHERE person_id = $1 AND period = $2")
        .bind(person_id)
        .bind(period)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM monthly_duration WHERE person_id = $1 AND period = $2")
        .bind(person_id)
        .bind(period)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM account WHERE person_id = $1")
        .bind(person_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM person WHERE id = $1")
        .bind(person_id)
        .execute(pool)
        .await;
}

#[tokio::test]
#[ignore = "需要真实 PostgreSQL（DB_LOCAL_* + DB_WRITER_*）；加 -- --ignored 运行"]
async fn name_only_import_is_stored_and_absorbed_after_claim() {
    let Some(st) = setup().await else { return };
    let st = std::sync::Arc::new(st);
    if st.writer.is_none() {
        eprintln!("跳过：douyin_writer 未配置");
        return;
    }
    let stamp = chrono::Utc::now().timestamp_millis();
    let daily_name = format!("__e2e_claim_daily_{stamp}");
    let month_name = format!("__e2e_claim_month_{stamp}");
    let daily_date = "2999-03-01";
    let period = "2999-04";
    let pool = st.writer.as_ref().expect("writer");

    let daily_report = commit_import(
        &st,
        "auto",
        daily_date,
        vec![ImportRow {
            name: daily_name.clone(),
            wave: Some(777.0),
            minutes: Some(55.0),
            ..Default::default()
        }],
        "merge",
        "__e2e_name_only_daily.csv",
    )
    .await
    .expect("仅艺名按日导入");
    assert_eq!(daily_report.pending.len(), 1, "未认得的人也必须暂存");
    assert!(daily_report.failures.is_empty());

    let pending_daily = sqlx::query(
        "SELECT name, douyin_no, claim_key FROM import_pending
          WHERE claim_key = $1 AND biz_date = $2::date AND absorbed_at IS NULL",
    )
    .bind(&daily_name)
    .bind(daily_date)
    .fetch_one(pool)
    .await
    .expect("按日暂存行");
    assert_eq!(pending_daily.try_get::<String, _>("douyin_no").unwrap(), "");
    assert_eq!(
        pending_daily.try_get::<String, _>("name").unwrap(),
        daily_name
    );

    let month_report = commit_month_metrics(
        &st,
        period,
        "auto",
        vec![ImportRow {
            name: month_name.clone(),
            wave: Some(888.0),
            minutes: Some(66.0),
            ..Default::default()
        }],
        "merge",
        "__e2e_name_only_month.csv",
        MonthMetrics::BOTH,
    )
    .await
    .expect("仅艺名按月导入");
    assert!(
        month_report
            .failures
            .iter()
            .any(|v| v["reason"].as_str().is_some_and(|s| s.contains("已暂存"))),
        "月度未认领行也必须提示已暂存"
    );
    let pending_month = sqlx::query(
        "SELECT name, douyin_no, claim_key FROM import_pending_month
          WHERE claim_key = $1 AND btrim(period) = $2 AND absorbed_at IS NULL",
    )
    .bind(&month_name)
    .bind(period)
    .fetch_one(pool)
    .await
    .expect("月度暂存行");
    assert_eq!(pending_month.try_get::<String, _>("douyin_no").unwrap(), "");

    let daily_pid: i64 =
        sqlx::query("INSERT INTO person (name, gender) VALUES ($1, 'male') RETURNING id")
            .bind(&daily_name)
            .fetch_one(pool)
            .await
            .expect("按日认领建档")
            .try_get("id")
            .unwrap();
    let month_pid: i64 =
        sqlx::query("INSERT INTO person (name, gender) VALUES ($1, 'female') RETURNING id")
            .bind(&month_name)
            .fetch_one(pool)
            .await
            .expect("月度认领建档")
            .try_get("id")
            .unwrap();

    assert_eq!(absorb_pending(&st, daily_pid as u64).await.unwrap(), 1);
    assert_eq!(
        absorb_pending_month(&st, month_pid as u64).await.unwrap(),
        1
    );

    let daily = sqlx::query(
        "SELECT wave, (minutes)::BIGINT AS minutes FROM daily_metric
          WHERE person_id = $1 AND biz_date = $2::date",
    )
    .bind(daily_pid)
    .bind(daily_date)
    .fetch_one(pool)
    .await
    .expect("按日补账结果");
    assert_eq!(daily.try_get::<i64, _>("wave").unwrap(), 777);
    assert_eq!(daily.try_get::<i64, _>("minutes").unwrap(), 55);

    let wave: i64 =
        sqlx::query_scalar("SELECT wave FROM monthly_wave WHERE person_id = $1 AND period = $2")
            .bind(month_pid)
            .bind(period)
            .fetch_one(pool)
            .await
            .expect("月度音浪补账");
    let minutes: i32 = sqlx::query_scalar(
        "SELECT minutes FROM monthly_duration WHERE person_id = $1 AND period = $2",
    )
    .bind(month_pid)
    .bind(period)
    .fetch_one(pool)
    .await
    .expect("月时长补账");
    assert_eq!((wave, minutes), (888, 66));

    for batch_id in [daily_report.batch_id, month_report.batch_id] {
        let _ = sqlx::query("DELETE FROM daily_import_contrib WHERE batch_id = $1")
            .bind(batch_id)
            .execute(pool)
            .await;
        let _ = sqlx::query("DELETE FROM import_batch WHERE id = $1")
            .bind(batch_id)
            .execute(pool)
            .await;
    }
    let _ = sqlx::query("DELETE FROM import_pending WHERE claim_key = $1")
        .bind(&daily_name)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM import_pending_month WHERE claim_key = $1")
        .bind(&month_name)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM daily_metric WHERE person_id = $1 AND biz_date = $2::date")
        .bind(daily_pid)
        .bind(daily_date)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM monthly_wave WHERE person_id = $1 AND period = $2")
        .bind(month_pid)
        .bind(period)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM monthly_duration WHERE person_id = $1 AND period = $2")
        .bind(month_pid)
        .bind(period)
        .execute(pool)
        .await;
    for pid in [daily_pid, month_pid] {
        let _ = sqlx::query("DELETE FROM person WHERE id = $1")
            .bind(pid)
            .execute(pool)
            .await;
    }
}

#[tokio::test]
#[ignore = "需要真实 PostgreSQL（DB_LOCAL_* + DB_WRITER_*）；加 -- --ignored 运行"]
async fn deleting_day_or_month_clears_its_unclaimed_rows() {
    let Some(st) = setup().await else { return };
    let st = std::sync::Arc::new(st);
    if st.writer.is_none() {
        eprintln!("跳过：douyin_writer 未配置");
        return;
    }
    let stamp = chrono::Utc::now().timestamp_millis();
    let daily_name = format!("__e2e_delete_daily_{stamp}");
    let month_name = format!("__e2e_delete_month_{stamp}");
    let daily_date = "2999-05-01";
    let period = "2999-06";
    let pool = st.writer.as_ref().expect("writer");

    let daily_batch = commit_import(
        &st,
        "auto",
        daily_date,
        vec![ImportRow {
            name: daily_name.clone(),
            wave: Some(100.0),
            ..Default::default()
        }],
        "merge",
        "__e2e_delete_daily.csv",
    )
    .await
    .expect("按日暂存导入")
    .batch_id;
    let month_batch = commit_month_metrics(
        &st,
        period,
        "auto",
        vec![ImportRow {
            name: month_name.clone(),
            wave: Some(200.0),
            ..Default::default()
        }],
        "merge",
        "__e2e_delete_month.csv",
        MonthMetrics {
            minutes: false,
            wave: true,
        },
    )
    .await
    .expect("月度暂存导入")
    .batch_id;

    let _ = import_day_delete(
        axum::extract::State(st.clone()),
        axum::extract::Query(DayQuery {
            biz_date: daily_date.into(),
            team: "auto".into(),
        }),
    )
    .await
    .unwrap_or_else(|_| panic!("删除按日数据失败"));
    let daily_left: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM import_pending WHERE claim_key = $1 AND absorbed_at IS NULL",
    )
    .bind(&daily_name)
    .fetch_one(pool)
    .await
    .expect("核查按日暂存");
    assert_eq!(daily_left, 0, "删日必须同步清理未认领暂存");

    let _ = import_month_delete(
        axum::extract::State(st.clone()),
        axum::extract::Query(MonthQuery {
            period: period.into(),
            team: "auto".into(),
        }),
    )
    .await
    .unwrap_or_else(|_| panic!("删除月度数据失败"));
    let month_left: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM import_pending_month WHERE claim_key = $1 AND absorbed_at IS NULL",
    )
    .bind(&month_name)
    .fetch_one(pool)
    .await
    .expect("核查月度暂存");
    assert_eq!(month_left, 0, "删月必须同步清理未认领暂存");

    for batch_id in [daily_batch, month_batch] {
        let _ = sqlx::query("DELETE FROM import_batch WHERE id = $1")
            .bind(batch_id)
            .execute(pool)
            .await;
    }
    let _ = sqlx::query("DELETE FROM import_pending WHERE claim_key = $1")
        .bind(&daily_name)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM import_pending_month WHERE claim_key = $1")
        .bind(&month_name)
        .execute(pool)
        .await;
}
