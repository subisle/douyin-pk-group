//! 集成测试（需要真实 PostgreSQL：`DB_LOCAL_*` / `DB_WRITER_*`；未配置时**显式打印跳过**）。
//!
//! 跑法（用例默认 `#[ignore]`，避免 CI 无库时「跳过即通过」的空转）：
//! `cd rust && set -a && source .env && set +a && cargo test -p pk-server --test monthly_metric_e2e -- --ignored --nocapture`
//!
//! 2026-10-16 用户口径「时长或者音浪都支持按月进行导入和显示」的端到端验收：
//! 按月导入核心 `admin::commit_month_metrics`（写 monthly_duration / monthly_wave）
//! 与展示端口径 `dialect::query_period`（月榜音浪优先整月导入值、只导整月音浪的人也进榜）
//! 在同一条用例里首尾相接，**真连本地 PG、真跑生产代码、真断言**。
//!
//! 与 store_e2e.rs 同一套纪律：
//! - `setup()` 未配置 `DB_LOCAL_HOST` 时打印显式跳过信息并返回 None（**不静默 pass**）；
//!   保留生产库防呆（库名 `douyin` + 147.90 / 127.0.0.1 / localhost → 拒绝运行）。
//! - 测试数据全部带 `__e2e按月` 前缀 + 独立日期段（2999 年），**开头先清一次**（防上次失败留下的
//!   脏数据让本次断言错乱）+ 结束再清一次。
//! - 原始 SQL 一律 **PG 原生 `$n` 占位符**（不用 `?`、不走 `pq::rewrite`）——2026-10-14 删掉
//!   旧 e2e 的原因就是 MySQL 方言 SQL 一连真库就炸（见 store_e2e.rs 文件头）。
//! - 只断言自己造的数据（自建 `person` 行），不依赖真实台账里的主播。
//!
//! 注：`commit_month_metrics` 内部会调 `spawn_post_import_backup()`（尝试跑
//! `/opt/pk-server/backup-to-remote.sh`）。本机没有该文件 → spawn 失败即 no-op，无需处理。

use pk_server::{admin, config, dialect, pgrs, state::AppState};
use sqlx::Row;

async fn setup() -> Option<AppState> {
    if std::env::var("DB_LOCAL_HOST").is_err() {
        eprintln!("跳过：未配置 DB_LOCAL_*（此测试需要真实测试库）");
        return None;
    }
    let sources = config::load_sources().expect("sources");
    // 防呆（2026-10-12）：e2e 会写库（建/删 __e2e 主播），绝不能对准生产。
    // 生产库的库名是 douyin —— 检测到直接拒绝，让测试显式指一个测试库。
    {
        let prod_hit = |h: &str, d: &str| {
            d == "douyin" && (h.contains("147.90") || h == "127.0.0.1" || h == "localhost")
        };
        for s in &sources {
            if prod_hit(&s.host, &s.database) {
                eprintln!("拒绝运行：数据源 {} 指向生产库（{} / {}）。e2e 测试必须用独立测试库（DB_LOCAL_NAME≠douyin）", s.id, s.host, s.database);
                return None;
            }
        }
        if let Ok(w) = config::load_writer() {
            if prod_hit(&w.host, &w.database) {
                eprintln!("拒绝运行：writer 指向生产库（{} / {}）。e2e 测试必须用独立测试库", w.host, w.database);
                return None;
            }
        }
    }
    let pools = pgrs::build_pools(&sources).await.expect("建池失败");
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
    Some(AppState {
        auth: std::sync::Arc::new(pk_server::auth::Auth::from_env()),
        wave_ttl: std::time::Duration::from_secs(600),
        sources,
        pools,
        writer,
        // P0：测试不起 Redis（None = 全内存模式，与引入前一致）
        redis: None,
        active: std::sync::Mutex::new(active),
        cache: std::sync::Mutex::new(Default::default()),
        bots: std::sync::OnceLock::new(),
    })
}

/// 写池不可用（writer 没配）→ 这些测试没有意义，跳过
fn need_writer(st: &AppState) -> bool {
    if st.writer.is_none() {
        eprintln!("跳过：douyin_writer 未配置");
        return false;
    }
    true
}

/// 测试数据前缀：主播名 / 文件名都带它，清理时一网打尽。
const MONTH_PREFIX: &str = "__e2e按月";
/// 独立业务月份（2999-01）：真实台账不可能有这一期，脏数据一眼可辨。
const PERIOD: &str = "2999-01";
/// 导入批次文件名（清理用前缀定位；同一个文件名两次提交会产生两条 import_batch）。
const FILENAME: &str = "__e2e按月_月报_2999-01.csv";

/// e2e 公共清理：`__e2e按月` 前缀的主播名下一切 + 该前缀的导入批次。
///
/// 前缀判定用 `strpos(x, $1) = 1` 而不是 `LIKE '__e2e按月%'`：PG 的 LIKE 里 `_` 是
/// 单字符通配符，前缀自带下划线时 `LIKE` 会连 `XXe2e按月…` 一起匹配 —— 虽然 2999 年段
/// 不会误伤真实数据，但清理语句必须是字面前缀语义，不能靠「反正没人叫这个名」兜着。
async fn cleanup_month_e2e(st: &AppState) {
    let pool = st.writer.as_ref().expect("writer");
    for sql in [
        // 先删依赖 person 的两张整月表（用 USING 关联，PG 方言）
        "DELETE FROM monthly_duration md USING person p \
          WHERE p.id = md.person_id AND strpos(p.name, $1) = 1",
        "DELETE FROM monthly_wave mw USING person p \
          WHERE p.id = mw.person_id AND strpos(p.name, $1) = 1",
        "DELETE FROM import_batch WHERE strpos(filename, $1) = 1",
        // 账号行（本用例不建，兜住上次失败留下的）
        "DELETE FROM account WHERE strpos(douyin_no, $1) = 1 OR strpos(anchor_id, $1) = 1",
        "DELETE FROM person WHERE strpos(name, $1) = 1",
    ] {
        let _ = sqlx::query(sql).bind(MONTH_PREFIX).execute(pool).await;
    }
}

/// 按月导入 + 展示端口径端到端（一条用例内部顺序断言）：
///   1. 自建测试主播
///   2. 首次导入（时长 + 音浪）→ 两张整月表落值
///   3. 同参数重导 → 幂等（不翻倍、值不变）
///   4. 只勾音浪再导 → 音浪被覆盖、时长纹丝不动
///   5. 月榜（query_period）→ 音浪取整月导入值 7777（我们没造任何 daily_metric 行，
///      同时验证「只导整月音浪、当月没有日明细的人也进榜」）
///   6. 清理并断言三张表清空
///
/// ⚠️ **默认 `#[ignore]`，必须显式跑**（2026-10-16 独立复核 P1）：
/// CI（`.github/workflows/ci.yml` 的 `cargo test --workspace`）没有配 `DB_LOCAL_*`，
/// 不 ignore 的话本用例会「打印跳过 → 判 ok」——那正是 ci.yml 注释里点名删掉旧 e2e 的
/// 空转问题。ignore 后 CI 明确报 ignored，不会假装覆盖。真跑命令：
/// ```bash
/// cd rust && set -a && source .env && set +a && \
///   cargo test -p pk-server --test monthly_metric_e2e -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "需要真实 PostgreSQL（DB_LOCAL_*）；跑法见文件头，加 -- --ignored"]
async fn month_metrics_import_then_display_e2e() {
    let Some(st) = setup().await else { return };
    if !need_writer(&st) {
        return;
    }
    // commit_month_metrics 要 &Arc<AppState>；其余调用点按 Deref 拿 &AppState / &Pools
    let st = std::sync::Arc::new(st);
    let pool = st.writer.as_ref().expect("writer");
    let name = format!("{MONTH_PREFIX}导入");

    // 开头清一次：上次失败留下的 (person, 2999-01) 行会让「首次导入应写 1 行」变成 unchanged
    cleanup_month_e2e(&st).await;

    /* ── 1. 自建测试主播（不碰真实台账） ── */
    let pid: i64 = sqlx::query(
        "INSERT INTO person (name, gender, status) VALUES ($1, 'male', 'active') RETURNING id",
    )
    .bind(&name)
    .fetch_one(pool)
    .await
    .expect("建测试主播")
    .try_get(0)
    .expect("person.id");

    // 真跑（非跳过）时的自证行：`--nocapture` 下必须能看到这一行，
    // 否则说明 setup() 走了跳过 / 拒绝分支，本次「绿色」是空转。
    let local = st
        .sources
        .iter()
        .find(|s| s.id == "LOCAL")
        .map(|s| format!("{}:{}/{}", s.host, s.port, s.database))
        .unwrap_or_else(|| "LOCAL 未配置".into());
    eprintln!("[monthly_metric_e2e] 真跑断言（非跳过）：person_id={pid} period={PERIOD} db={local}");

    // 一行数据：name 命中主播（无需账号行）；时长 + 音浪都给值
    let row = admin::ImportRow {
        name: name.clone(),
        wave: Some(5000.0),
        minutes: Some(120.0),
        ..Default::default()
    };

    /* ── 2. 首次按月导入：时长 + 音浪都写 ── */
    let r1 = admin::commit_month_metrics(
        &st,
        PERIOD,
        "auto",
        vec![row.clone()],
        "merge",
        FILENAME,
        admin::MonthMetrics::BOTH,
    )
    .await
    .expect("首次按月导入");
    assert_eq!(r1.ok_rows, 1, "首次导入应写 1 行（期望 ok_rows=1，实际 {}）", r1.ok_rows);
    assert_eq!(r1.unchanged, 0, "首次导入不该有未覆盖行（期望 unchanged=0，实际 {}）", r1.unchanged);
    assert_eq!(
        r1.minutes_total, 120,
        "报告时长合计应为 120 分钟（期望 120，实际 {}）",
        r1.minutes_total
    );
    assert_eq!(
        r1.wave_total, 5000,
        "报告音浪合计应为 5000（期望 5000，实际 {}）",
        r1.wave_total
    );
    assert!(r1.failures.is_empty(), "不该有失败行：{:?}", r1.failures);
    assert!(r1.skipped.is_empty(), "不该有跳过行：{:?}", r1.skipped);

    // 直查校验：minutes 列是 INT4，**必须**按 i32 解（`try_get::<i64>` 会
    // mismatched types 解码失败 —— 生产端踩过的正是这个坑，见步骤 3 的注释）。
    let q_minutes = |pid: i64| {
        let pool = pool;
        async move {
            sqlx::query("SELECT minutes FROM monthly_duration WHERE person_id = $1 AND period = $2")
                .bind(pid)
                .bind(PERIOD)
                .fetch_optional(pool)
                .await
                .expect("查 monthly_duration")
                .map(|r| r.try_get::<i32, _>(0).expect("minutes"))
        }
    };
    let q_wave = |pid: i64| {
        let pool = pool;
        async move {
            sqlx::query("SELECT wave FROM monthly_wave WHERE person_id = $1 AND period = $2")
                .bind(pid)
                .bind(PERIOD)
                .fetch_optional(pool)
                .await
                .expect("查 monthly_wave")
                .map(|r| r.try_get::<i64, _>(0).expect("wave"))
        }
    };

    assert_eq!(
        q_minutes(pid).await,
        Some(120),
        "monthly_duration 应有 (person={pid}, {PERIOD}, 120)，实际 {:?}",
        q_minutes(pid).await
    );
    assert_eq!(
        q_wave(pid).await,
        Some(5000),
        "monthly_wave 应有 (person={pid}, {PERIOD}, 5000)，实际 {:?}",
        q_wave(pid).await
    );

    /* ── 3. 幂等：同参数重导 → 一行不写、数值不翻倍 ──
       本断言是回归闸门：首次运行时真的抓到过生产 bug —— commit_month_metrics 读
       monthly_duration.minutes（INT4）用的是 `try_get::<i64>`，sqlx 的 INT8 与 INT4
       不兼容 → 解码失败被 `unwrap_or(0)` 吞成 0 → 每次重导都判成「值变了」照写一遍，
       表现为 ok_rows=1 / unchanged=0（库里数值倒是对的，只有幂等报告与 updated_at 失真）。
       该处现已修为 `SELECT (minutes)::BIGINT`；这条断言守住它不许回退。 */
    let r2 = admin::commit_month_metrics(
        &st,
        PERIOD,
        "auto",
        vec![row.clone()],
        "merge",
        FILENAME,
        admin::MonthMetrics::BOTH,
    )
    .await
    .expect("二次按月导入");
    assert_eq!(
        r2.ok_rows, 0,
        "同参数重导必须一行不写（期望 ok_rows=0，实际 {}）",
        r2.ok_rows
    );
    assert_eq!(
        r2.unchanged, 1,
        "同参数重导应报 1 人未覆盖（期望 unchanged=1，实际 {}）",
        r2.unchanged
    );
    assert_eq!(
        q_minutes(pid).await,
        Some(120),
        "重导后时长不许翻倍（期望仍 120，实际 {:?}）",
        q_minutes(pid).await
    );
    assert_eq!(
        q_wave(pid).await,
        Some(5000),
        "重导后音浪不许翻倍（期望仍 5000，实际 {:?}）",
        q_wave(pid).await
    );

    /* ── 4. 指标可分开：只勾音浪 → 音浪覆盖、时长不动 ── */
    let r3 = admin::commit_month_metrics(
        &st,
        PERIOD,
        "auto",
        vec![admin::ImportRow {
            name: name.clone(),
            wave: Some(7777.0),
            minutes: Some(999.0),
            ..Default::default()
        }],
        "merge",
        FILENAME,
        admin::MonthMetrics {
            minutes: false,
            wave: true,
        },
    )
    .await
    .expect("只导音浪");
    assert_eq!(r3.ok_rows, 1, "只导音浪时音浪变了应写 1 行（实际 {}）", r3.ok_rows);
    assert_eq!(
        r3.minutes_total, 0,
        "没勾时长时报告时长合计必须为 0（期望 0，实际 {}）",
        r3.minutes_total
    );
    assert_eq!(r3.wave_total, 7777, "只导音浪时音浪合计应为 7777（实际 {}）", r3.wave_total);
    assert_eq!(
        q_wave(pid).await,
        Some(7777),
        "monthly_wave 应被覆盖为 7777（实际 {:?}）",
        q_wave(pid).await
    );
    assert_eq!(
        q_minutes(pid).await,
        Some(120),
        "只导音浪绝不许碰 monthly_duration：应仍是 120（实际 {:?}）",
        q_minutes(pid).await
    );

    /* ── 5. 展示端口径：月榜音浪取整月导入值 ──
       本用例**没有造任何 daily_metric 行**，该主播能出现在月榜上完全靠
       period_rollup_sql 的 monthly_wave 补行分支 —— 正好一并验证
       「只导整月音浪、当月没有日明细的人也进榜」。 */
    let rows = dialect::query_period(
        &st.pools,
        "LOCAL",
        dialect::ADMIN_PERIOD_SELECT,
        &format!("{PERIOD}%"),
        "",  // 不限团别
        "",  // 无额外过滤
        "wave DESC",
    )
    .await
    .expect("月榜查询（query_period）");
    let hit = rows
        .iter()
        .find(|r| r.try_get::<i64, _>("person_id").unwrap_or(-1) == pid)
        .unwrap_or_else(|| {
            panic!(
                "月榜里必须有 {name}（只导整月音浪、没有日明细的人也要进榜）；{PERIOD} 共 {} 行",
                rows.len()
            )
        });
    assert_eq!(
        hit.try_get::<i64, _>("wave").expect("wave"),
        7777,
        "月榜音浪必须取整月导入值 7777（不许回落日明细求和；实际 {}）",
        hit.try_get::<i64, _>("wave").unwrap_or(-1)
    );
    assert_eq!(
        hit.try_get::<i64, _>("minutes").expect("minutes"),
        120,
        "月榜时长必须取整月导入值 120（实际 {}）",
        hit.try_get::<i64, _>("minutes").unwrap_or(-1)
    );
    assert_eq!(
        hit.try_get::<i64, _>("live_days").expect("live_days"),
        0,
        "补行出来的整月音浪行没有日明细，开播天数应为 0（实际 {}）",
        hit.try_get::<i64, _>("live_days").unwrap_or(-1)
    );

    /* ── 6. 清理 + 断言清干净 ── */
    cleanup_month_e2e(&st).await;
    let still_minutes: i64 = sqlx::query("SELECT COUNT(*) FROM monthly_duration WHERE person_id = $1")
        .bind(pid)
        .fetch_one(pool)
        .await
        .expect("查残留 monthly_duration")
        .try_get(0)
        .expect("count");
    let still_wave: i64 = sqlx::query("SELECT COUNT(*) FROM monthly_wave WHERE person_id = $1")
        .bind(pid)
        .fetch_one(pool)
        .await
        .expect("查残留 monthly_wave")
        .try_get(0)
        .expect("count");
    let still_batch: i64 = sqlx::query("SELECT COUNT(*) FROM import_batch WHERE strpos(filename, $1) = 1")
        .bind(MONTH_PREFIX)
        .fetch_one(pool)
        .await
        .expect("查残留 import_batch")
        .try_get(0)
        .expect("count");
    let still_person: i64 = sqlx::query("SELECT COUNT(*) FROM person WHERE id = $1")
        .bind(pid)
        .fetch_one(pool)
        .await
        .expect("查残留 person")
        .try_get(0)
        .expect("count");
    assert_eq!(still_minutes, 0, "monthly_duration 应清空（person={pid}，实际剩 {still_minutes} 行）");
    assert_eq!(still_wave, 0, "monthly_wave 应清空（person={pid}，实际剩 {still_wave} 行）");
    assert_eq!(
        still_batch, 0,
        "import_batch 应清空（filename 前缀 {MONTH_PREFIX}，实际剩 {still_batch} 行）"
    );
    assert_eq!(still_person, 0, "person 应清空（id={pid}，实际剩 {still_person} 行）");
}
