//! 集成测试（需要真实 PostgreSQL：DB_LOCAL_* / DB_WRITER_* 环境变量；未配置时跳过）。
//!
//! 2026-10-14 收口：本文件只保留 **暂存补账幂等性** 一条用例
//! （`absorb_pending_is_idempotent_across_repeated_calls`）。
//!
//! 为什么其余 e2e 被删除：
//! - 原 store_e2e.rs 其余用例与 bot_e2e.rs、monthly_duration_e2e.rs 都是 MySQL 时代写的：
//!   SQL 用 `?` 占位符、`DELETE d FROM ... JOIN` 方言，且直接 `sqlx::query(...)` 不走
//!   生产代码的 `pq::rewrite`（`?`→`$n`）。生产库已迁 PostgreSQL 后，它们一旦真正连库运行
//!   就报 `42601 syntax error at or near ","`（实测 store_e2e 17 失败 11、bot_e2e 15 失败 9、
//!   monthly_duration_e2e 1 失败 1）。
//! - 而 `setup()` 在未配置 `DB_LOCAL_HOST` 时直接返回 None、用例静默 return，
//!   退出码还是 0 —— CI 一直全绿，实际一条断言都没跑过（vacuously passing）。
//! - 按用户决定整体移除而不是逐条移植；有单测/纯函数覆盖的行为不再保留 e2e 重复份。
//!
//! ⚠️ 重新引入 e2e 的前提：本地/CI 起真实 postgres 服务 + 用 **PG 原生 `$n` 占位符**写 SQL
//! （或统一走 `pq::rewrite`），并且跳过必须显式（不要静默 return 让 CI 空转）。
//!
//! 测试用键全部带 `__e2e合并` / `MERGE_PREFIX` 前缀 + 独立日期段（2999 年），
//! 结束即清理，不污染真实数据；setup() 内置生产库防呆（库名 douyin + 生产网段直接拒绝）。

use pk_server::{config, pgrs, state::AppState};
use sqlx::Row;

async fn setup() -> Option<AppState> {
    if std::env::var("DB_LOCAL_HOST").is_err() {
        eprintln!("跳过：未配置 DB_LOCAL_*（此测试需要真实测试库）");
        return None;
    }
    let sources = config::load_sources().expect("sources");
    // 防呆（2026-10-12）：e2e 会写库（建/删 __e2e 主播），绝不能对准生产。
    // 生产 MariaDB 的库名是 douyin —— 检测到直接拒绝，让测试显式指一个测试库。
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
                eprintln!(
                    "拒绝运行：writer 指向生产库（{} / {}）。e2e 测试必须用独立测试库",
                    w.host, w.database
                );
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

const MERGE_PREFIX: &str = "__e2e合并";

/// e2e 公共清理：测试主播（__e2e合并 前缀）名下的一切 + 独立测试日期段。
/// 方言注意：多表删除用 PG 的 `DELETE FROM ... USING ... WHERE`，
/// 不是 MySQL 的 `DELETE d FROM ... JOIN`（2026-10-14 转换）。
async fn cleanup_admin_e2e(st: &AppState) {
    let pool = st.writer.as_ref().expect("writer");
    for sql in [
        "DELETE FROM daily_metric WHERE biz_date BETWEEN '2999-01-01' AND '2999-12-31'".to_string(),
        "DELETE FROM wave_snapshot WHERE biz_date BETWEEN '2999-01-01' AND '2999-12-31'".to_string(),
        format!(
            "DELETE FROM daily_metric d USING person p WHERE p.id = d.person_id AND p.name LIKE '{MERGE_PREFIX}%'"
        ),
        format!("DELETE FROM account WHERE anchor_id LIKE '{MERGE_PREFIX}%' OR douyin_no LIKE '{MERGE_PREFIX}%'"),
        format!("DELETE FROM import_pending WHERE name LIKE '{MERGE_PREFIX}%' OR douyin_no LIKE '{MERGE_PREFIX}%'"),
        "DELETE FROM import_batch WHERE filename LIKE '__e2e_%'".to_string(),
        format!("DELETE FROM person WHERE name LIKE '{MERGE_PREFIX}%'"),
    ] {
        let _ = sqlx::query(&sql).execute(pool).await;
    }
}

/* ══════════ 暂存补账幂等性（2026-10-14） ══════════
后台「导入暂存区」的 UI 入口已移除：运营不再能手工挨行「建档 / 丢弃」。
这条链路现在完全依赖 absorb 自动补账，因此它的**幂等性**成了唯一防线 ——
一旦重复补账，音浪会静默翻倍且没有任何前端提示能让运营察觉。

触发点共 5 处（create_streamer / update_streamer / bind_account ×3 / 绑定审核通过），
建档后往往会紧跟一次绑号（或二次编辑），同一个 person_id 会被 absorb 多次。
本测试把「连续两次 absorb」钉死：第二次必须补 0 行、daily_metric 数值不变。

本用例用 **PG 原生 $n** 占位符，是在真实 PostgreSQL 上真跑真断言的 e2e
（原文件其余 MySQL 方言用例已于 2026-10-14 删除，见文件头注释）；
新增用例请照此写法。 */

#[tokio::test]
async fn absorb_pending_is_idempotent_across_repeated_calls() {
    let Some(st) = setup().await else { return };
    if !need_writer(&st) {
        return;
    }
    let pool = st.writer.as_ref().expect("writer");
    let name = format!("{MERGE_PREFIX}暂存补账");
    let no = format!("{MERGE_PREFIX}_pending_no");
    let nomatch = format!("{MERGE_PREFIX}_nomatch");
    let biz = "2999-02-01";

    cleanup_admin_e2e(&st).await;

    let pid = sqlx::query(
        "INSERT INTO person (name, gender, status) VALUES ($1, 'male', 'active') RETURNING id",
    )
    .bind(&name)
    .fetch_one(pool)
    .await
    .expect("建测试主播")
    .try_get::<i64, _>(0)
    .expect("id");

    let _ = sqlx::query("DELETE FROM import_pending WHERE douyin_no = ANY($1)")
        .bind(vec![no.clone(), nomatch.clone()])
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM account WHERE douyin_no = $1")
        .bind(&no)
        .execute(pool)
        .await;

    // 该主播持有一个抖音号 —— absorb 的命中依据（PENDING_FOR_PERSON_SQL）
    sqlx::query(
        "INSERT INTO account (person_id, anchor_id, douyin_no, is_primary) VALUES ($1,$2,$3,1)",
    )
    .bind(pid)
    .bind(&no)
    .bind(&no)
    .execute(pool)
    .await
    .expect("挂账号");

    // 两行暂存：一行能对上号（应命中），一行无关（不应命中）
    for (no2, w) in [(no.clone(), 500i64), (nomatch.clone(), 700i64)] {
        sqlx::query(
            "INSERT INTO import_pending (name, douyin_no, biz_date, wave, minutes)
             VALUES ($1,$2,$3::date,$4,$5)
             ON CONFLICT (douyin_no, biz_date) DO UPDATE SET wave = EXCLUDED.wave",
        )
        .bind(&name)
        .bind(&no2)
        .bind(biz)
        .bind(w)
        .bind(30i32)
        .execute(pool)
        .await
        .expect("插暂存行");
    }

    let got = pk_server::admin::absorb_pending(&st, pid as u64)
        .await
        .expect("首次补账");
    assert_eq!(got, 1, "首轮只补能对上号的那 1 行（实际 {got}）");

    let wave_after_first: i64 = sqlx::query(
        "SELECT (SUM(wave))::BIGINT FROM daily_metric WHERE person_id = $1 AND biz_date = $2::date",
    )
    .bind(pid)
    .bind(biz)
    .fetch_one(pool)
    .await
    .expect("查当日")
    .try_get::<i64, _>(0)
    .unwrap_or(0);
    assert!(
        wave_after_first > 0,
        "首轮补账必须真的落进 daily_metric（实际 {wave_after_first}）"
    );

    let got2 = pk_server::admin::absorb_pending(&st, pid as u64)
        .await
        .expect("二次补账");
    assert_eq!(
        got2, 0,
        "重复 absorb 绝不能再补行（否则音浪翻倍，实际补了 {got2} 行）"
    );

    let wave_after_second: i64 = sqlx::query(
        "SELECT (SUM(wave))::BIGINT FROM daily_metric WHERE person_id = $1 AND biz_date = $2::date",
    )
    .bind(pid)
    .bind(biz)
    .fetch_one(pool)
    .await
    .expect("查当日")
    .try_get::<i64, _>(0)
    .unwrap_or(0);
    assert_eq!(
        wave_after_second, wave_after_first,
        "二次 absorb 后当日音浪必须不变（首次 {wave_after_first}，二次 {wave_after_second}）"
    );

    let still: i64 = sqlx::query(
        "SELECT COUNT(*) FROM import_pending WHERE douyin_no = $1 AND absorbed_at IS NULL",
    )
    .bind(&nomatch)
    .fetch_one(pool)
    .await
    .expect("查未命中行")
    .try_get::<i64, _>(0)
    .unwrap_or(0);
    assert_eq!(
        still, 1,
        "对不上号的暂存行不该被 absorb 吞掉（实际剩 {still}）"
    );

    cleanup_admin_e2e(&st).await;
}
