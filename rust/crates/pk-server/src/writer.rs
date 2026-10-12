//! PostgreSQL 写层：douyin_writer 业务写池 + 写守卫
//!
//! 与只读层（mysql.rs）分池分账号：
//! - 写池账号 douyin_writer 只有业务表 SELECT/INSERT/UPDATE/DELETE，**无 DDL**
//! - 写守卫集中拦截 DDL 关键字（双保险：账号权限是根本，守卫是应用层兜底）
//! - 所有写入 SQL 仍集中在 dialect.rs / admin.rs，不接受外部传入的 SQL 字符串
//! - 语句都经 pq() 把 ? 占位符重写成 $N

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;

use crate::config::DbEndpoint;

/// DDL / 管理类关键字：业务写账号一律拒绝（迁移走独立的 migrator 通道）
const DDL_KEYWORDS: [&str; 8] = [
    "DROP", "ALTER", "CREATE", "TRUNCATE", "GRANT", "REVOKE", "RENAME", "LOCK",
];
const FORBIDDEN_PHRASES: [&str; 2] = ["INTO OUTFILE", "LOAD_FILE"];

/// 业务写连接允许的语句前缀
const WRITE_PREFIXES: [&str; 10] = [
    "SELECT", "INSERT", "UPDATE", "DELETE", "ON CONFLICT", "START", "BEGIN", "COMMIT",
    "ROLLBACK", "SET",
];

/// 把单引号字符串字面量里的内容替换成空格，避免值里的关键字（如 'DROP it'）误伤守卫
fn strip_string_literals(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut in_str = false;
    let mut escaped = false;
    for c in sql.chars() {
        if in_str {
            if escaped {
                out.push(' ');
                escaped = false;
            } else if c == '\\' {
                out.push(c);
                escaped = true;
            } else if c == '\'' {
                in_str = false;
                out.push(c);
            } else {
                out.push(' ');
            }
        } else {
            if c == '\'' {
                in_str = true;
            }
            out.push(c);
        }
    }
    out
}

pub fn write_guard(sql: &str) -> anyhow::Result<()> {
    let upper = strip_string_literals(sql).trim().to_ascii_uppercase();
    if let Some(phrase) = FORBIDDEN_PHRASES.iter().find(|k| upper.contains(**k)) {
        anyhow::bail!("写连接拒绝执行：检测到禁止短语 {phrase}");
    }
    let hit = upper
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .find(|token| DDL_KEYWORDS.contains(token));
    if let Some(kw) = hit {
        anyhow::bail!("写连接拒绝执行：检测到 DDL 关键字 {kw}（DDL 只能走显式 migrate）");
    }
    if !WRITE_PREFIXES.iter().any(|p| upper.starts_with(p)) {
        anyhow::bail!(
            "写连接拒绝执行：仅允许业务写语句（INSERT/UPDATE/DELETE/SELECT）"
        );
    }
    Ok(())
}

/// 建写池（PostgreSQL）
pub async fn connect(ep: &DbEndpoint) -> anyhow::Result<PgPool> {
    let opts: PgConnectOptions = ep
        .url()
        .parse()
        .map_err(|e| anyhow::anyhow!("写库连接串解析失败: {e}"))?;
    PgPoolOptions::new()
        .max_connections(ep.pool_limit)
        .min_connections(0)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .idle_timeout(std::time::Duration::from_secs(30))
        .connect_with(opts)
        .await
        .map_err(|e| anyhow::anyhow!("写库连接失败（账号 {}）: {e}", ep.user))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_business_writes_and_reads() {
        for sql in [
            "INSERT INTO daily_metric (person_id) VALUES (1)",
            "UPDATE person SET name = 'x' WHERE id = 1",
            "DELETE FROM aliases WHERE id = 2",
            "INSERT INTO t VALUES (1) ON CONFLICT (id) DO NOTHING",
            "SELECT cumulative_wave FROM daily_metric LIMIT 1",
        ] {
            assert!(write_guard(sql).is_ok(), "{sql} 应放行");
        }
    }

    #[test]
    fn denies_ddl_even_in_disguise() {
        for sql in [
            "DROP TABLE person",
            "alter table person add column x int",
            "CREATE TABLE hack (id int)",
            "TRUNCATE TABLE daily_metric",
            "GRANT ALL ON *.* TO 'x'",
            "REVOKE SELECT ON *.* FROM 'x'",
            "RENAME TABLE a TO b",
            "select * from t; drop table person", // 多语句里的 DDL 一样拦
            "-- 注释\nDROP TABLE person",
            "deleted_at = 1; ALTER TABLE t", // 词边界：分号后的 ALTER 仍要命中
        ] {
            assert!(write_guard(sql).is_err(), "{sql} 应拒绝");
        }
    }

    #[test]
    fn column_names_are_not_keywords() {
        // 列名/值里含关键字子串不误伤（词边界匹配）
        assert!(write_guard("UPDATE person SET created_at = NOW() WHERE id = 1").is_ok());
        assert!(write_guard("INSERT INTO t (note) VALUES ('created something')").is_ok());
        assert!(write_guard("SELECT * FROM t WHERE note = 'DROP it'").is_ok());
    }

    #[test]
    fn denies_unknown_statements() {
        assert!(write_guard("CALL do_something()").is_err());
        assert!(write_guard("USE other_db").is_err());
    }
}
