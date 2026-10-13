//! 显式迁移 runner：`pk-server migrate`
//!
//! docs/00 §4 决策（2026-10-04，TypeSafe 确认）：
//! - DDL 只由独立的 `douyin_migrator` 账号执行，**不随服务启动自动跑**
//! - 通过 `pk-server migrate` 子命令（或容器内 `docker exec pk-server migrate`）显式触发
//! - 已应用的版本登记在 schema_migrations，重复执行只补未应用的

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Row};

use crate::config::DbEndpoint;

/// 编译进二进制的迁移脚本（按 version 顺序应用）
const MIGRATIONS: &[(&str, &str)] = &[
    ("001_core", include_str!("migrations/001_core.sql")),
    (
        "002_import_audit",
        include_str!("migrations/002_import_audit.sql"),
    ),
    ("003_runtime", include_str!("migrations/003_runtime.sql")),
    ("004_push", include_str!("migrations/004_push.sql")),
    (
        "005_push_delay",
        include_str!("migrations/005_push_delay.sql"),
    ),
    (
        "006_import_pending",
        include_str!("migrations/006_import_pending.sql"),
    ),
    (
        "007_monthly_duration",
        include_str!("migrations/007_monthly_duration.sql"),
    ),
    (
        "008_my_register",
        include_str!("migrations/008_my_register.sql"),
    ),
    (
        "009_anchor_convergence",
        include_str!("migrations/009_anchor_convergence.sql"),
    ),
    (
        "010_my_pass_hash",
        include_str!("migrations/010_my_pass_hash.sql"),
    ),
    ("011_ip_geo", include_str!("migrations/011_ip_geo.sql")),
    (
        "012_my_phone_unique",
        include_str!("migrations/012_my_phone_unique.sql"),
    ),
    (
        "013_wx_push_accounts",
        include_str!("migrations/013_wx_push_accounts.sql"),
    ),
    (
        "014_ip_allowlist",
        include_str!("migrations/014_ip_allowlist.sql"),
    ),
    (
        "015_identity_unify",
        include_str!("migrations/015_identity_unify.sql"),
    ),
    (
        "016_streamer_phone",
        include_str!("migrations/016_streamer_phone.sql"),
    ),
    (
        "017_my_bind_requests",
        include_str!("migrations/017_my_bind_requests.sql"),
    ),
    (
        "018_identity_uniqueness",
        include_str!("migrations/018_identity_uniqueness.sql"),
    ),
    (
        "019_monthly_metric_unique_index",
        include_str!("migrations/019_monthly_metric_unique_index.sql"),
    ),
    // 020 之前只存在于磁盘、没被 include_str! 引用 → `pk-server migrate` 永不执行它，
    // 001 里的索引名撞车因此一个都没补上。这里补登记。
    (
        "020_fix_index_collisions",
        include_str!("migrations/020_fix_index_collisions.sql"),
    ),
    // 021：uk_anchor_date 跨表撞名，wave_snapshot 的唯一索引从未建出（灌库 ON CONFLICT
    // 直接失败 + 音浪可重复计入）。
    (
        "021_fix_wave_snapshot_unique",
        include_str!("migrations/021_fix_wave_snapshot_unique.sql"),
    ),
];

/// 迁移建连（migrator 账号；同样关语句缓存）
async fn connect(ep: &DbEndpoint) -> anyhow::Result<PgPool> {
    let opts: PgConnectOptions = ep
        .url()
        .parse()
        .map_err(|e| anyhow::anyhow!("迁移库连接串解析失败: {e}"))?;
    PgPoolOptions::new()
        .max_connections(ep.pool_limit)
        .min_connections(0)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .idle_timeout(std::time::Duration::from_secs(30))
        .connect_with(opts)
        .await
        .map_err(|e| anyhow::anyhow!("迁移库连接失败（账号 {}）: {e}", ep.user))
}

/// 把迁移脚本拆成单条语句：去掉 `--` 注释行后按 `;` 分割。
/// PG 版注意：`$$...$$`（plpgsql 函数体）内部的分号不是语句边界，先保护再切。
fn split_statements(sql: &str) -> Vec<String> {
    let no_comments: String = sql
        .lines()
        .filter(|l| !l.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    // 把 $$...$$ 段落替换成不含分号的占位符后按 ; 切，切完再还原
    let mut dollar_bodies: Vec<String> = Vec::new();
    let mut masked = String::with_capacity(no_comments.len());
    let mut rest = no_comments.as_str();
    while let Some(pos) = rest.find("$$") {
        // 第一个 $$ 之前的内容原样保留（去掉 $$ 本身）
        masked.push_str(&rest[..pos]);
        rest = &rest[pos + 2..];
        if let Some(end) = rest.find("$$") {
            // 整个函数体（含两侧 $$）作为占位符替身，内部无 ';'
            dollar_bodies.push(format!("$$ {} $$", &rest[..end]));
            masked.push_str(&format!("\x00DOLLAR{}\x00", dollar_bodies.len() - 1));
            rest = &rest[end + 2..];
        } else {
            // 未闭合（不该发生）：原样返回剩余
            masked.push_str("$$");
            masked.push_str(rest);
            rest = "";
            break;
        }
    }
    masked.push_str(rest);
    let mut chunks: Vec<String> = Vec::new();
    for stmt in masked.split(';') {
        let t = stmt.trim();
        if t.is_empty() {
            continue;
        }
        // 还原被保护的 $$...$$ 段落
        let mut restored = t.to_string();
        for (i, body) in dollar_bodies.iter().enumerate() {
            restored = restored.replace(&format!("\x00DOLLAR{i}\x00"), body);
        }
        chunks.push(restored);
    }
    chunks
}

/// 执行迁移，返回本次应用的版本列表。幂等，可重复执行。
pub async fn run(ep: &DbEndpoint) -> anyhow::Result<Vec<String>> {
    let pool = connect(ep).await?;
    run_with(&pool).await
}

pub async fn run_with(pool: &PgPool) -> anyhow::Result<Vec<String>> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version VARCHAR(64) NOT NULL,
            applied_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (version)
        )",
    )
    .execute(pool)
    .await?;

    let applied_before: Vec<String> =
        sqlx::query(&crate::pq::rewrite("SELECT version FROM schema_migrations"))
        .fetch_all(pool)
        .await?
        .iter()
        .filter_map(|r| r.try_get::<String, _>(0).ok())
        .collect();

    let mut applied_now = Vec::new();
    for (version, sql) in MIGRATIONS {
        if applied_before.iter().any(|v| v == version) {
            tracing::info!(version, "迁移已应用，跳过");
            continue;
        }
        for stmt in split_statements(sql) {
            // 走 raw_sql：迁移脚本没有任何绑定参数，raw_sql 支持 DDL / 多语句。
            if let Err(e) = sqlx::raw_sql(&stmt).execute(pool).await {
                anyhow::bail!("迁移 {version} 执行失败: {e}\n语句: {stmt}");
            }
        }
        sqlx::query(&crate::pq::rewrite(
            "INSERT INTO schema_migrations (version) VALUES (?)",
        ))
        .bind(version)
        .execute(pool)
        .await?;
        tracing::info!(version, "迁移已应用");
        applied_now.push(version.to_string());
    }
    Ok(applied_now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_statements_and_strips_comments() {
        let sql = "-- 注释 ; 里有分号也不影响\nCREATE TABLE a (id INT);\n\nALTER TABLE a\n  ADD COLUMN x INT;\n";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].starts_with("CREATE TABLE a"));
        assert!(stmts[1].starts_with("ALTER TABLE a"));
    }

    #[test]
    fn embedded_migrations_split_cleanly() {
        for (version, sql) in MIGRATIONS {
            let stmts = split_statements(sql);
            assert!(!stmts.is_empty(), "{version} 至少要有一条语句");
            for s in &stmts {
                let upper = s.to_ascii_uppercase();
                assert!(
                    upper.starts_with("CREATE")
                        || upper.starts_with("ALTER")
                        || upper.starts_with("INSERT")
                        || upper.starts_with("UPDATE")
                        || upper.starts_with("DROP TRIGGER")
                        || upper.starts_with("SELECT"),
                    "{version} 里出现意外语句: {s}"
                );
            }
        }
    }

    #[test]
    fn versions_are_ordered() {
        let mut names: Vec<&str> = MIGRATIONS.iter().map(|(v, _)| *v).collect();
        let sorted = names.clone();
        names.sort();
        assert_eq!(names, sorted, "迁移必须按文件名顺序排列");
    }

    /// 磁盘上的每个 `migrations/0xx_*.sql` 都必须被登记进 MIGRATIONS
    /// （2026-10-14 审计：020 文件存在却漏登记 → 索引撞车修复从未执行）。
    /// 上一条「编号连续」测试防不住这种漏登记——它只检查已登记项之间的关系。
    #[test]
    fn every_migration_file_is_registered() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/migrations");
        let mut on_disk: Vec<String> = std::fs::read_dir(dir)
            .expect("读取 migrations 目录失败")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".sql"))
            .map(|n| n.trim_end_matches(".sql").to_string())
            .collect();
        on_disk.sort();

        let mut registered: Vec<&str> = MIGRATIONS.iter().map(|(v, _)| *v).collect();
        registered.sort();

        let missing: Vec<&String> = on_disk
            .iter()
            .filter(|n| !registered.contains(&n.as_str()))
            .collect();
        let orphan: Vec<&&str> = registered
            .iter()
            .filter(|v| !on_disk.iter().any(|n| n == *v))
            .collect();
        assert!(
            missing.is_empty(),
            "这些迁移文件在磁盘上但没被 include_str! 登记（永远不会被应用）: {missing:?}"
        );
        assert!(
            orphan.is_empty(),
            "MIGRATIONS 登记了但磁盘上没有对应文件（编译期就该报错）: {orphan:?}"
        );
    }

    /// 编号连续且无重复（xxx_name 前缀，001 起），防止登记时漏号/撞号。
    #[test]
    fn version_numbers_are_unique_and_contiguous() {
        let mut nums: Vec<u32> = MIGRATIONS
            .iter()
            .map(|(v, _)| {
                let (num, rest) = v.split_once('_').unwrap_or_else(|| {
                    panic!("迁移版本 {v} 不符合 xxx_name 命名");
                });
                assert!(!rest.is_empty(), "迁移版本 {v} 缺少下划线后的名字");
                num.parse()
                    .unwrap_or_else(|_| panic!("迁移版本 {v} 的编号 {num} 不是数字"))
            })
            .collect();
        nums.sort_unstable();
        nums.dedup();
        assert_eq!(
            nums,
            (1..=nums.len() as u32).collect::<Vec<_>>(),
            "迁移编号必须从 001 起连续且无重复"
        );
    }
}
