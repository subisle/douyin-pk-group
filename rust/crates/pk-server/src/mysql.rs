//! MySQL 只读层：多数据源连接池 + 只读守卫
//!
//! 三层保障（与 TS 版一致）：
//! 1. 所有 SQL 都写死在 dialect.rs，不接受外部传入
//! 2. 只读守卫只放行 SELECT / SHOW，含写操作关键字直接抛错
//! 3. 账号本身只配查询权限（见 README）

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
use sqlx::{MySqlPool, Row};

use crate::config::SourceConfig;

pub type Pools = Arc<HashMap<String, MySqlPool>>;

pub async fn build_pools(sources: &[SourceConfig]) -> anyhow::Result<Pools> {
    let mut map = HashMap::new();
    for s in sources {
        let opts: MySqlConnectOptions = s
            .url()
            .parse()
            .map_err(|e| anyhow::anyhow!("数据源 {} 连接串解析失败: {e}", s.id))?;
        // 该库对 prepared statement 兼容性不好（报 1835 Malformed communication packet），
        // 关掉语句缓存走 text protocol
        let opts = opts.statement_cache_capacity(0);
        let pool = MySqlPoolOptions::new()
            .max_connections(s.pool_limit)
            .min_connections(0)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .idle_timeout(std::time::Duration::from_secs(30))
            .connect_with(opts)
            .await
            .map_err(|e| anyhow::anyhow!("数据源 {} 连接失败: {e}", s.id))?;
        map.insert(s.id.clone(), pool);
        tracing::info!(source = %s.id, host = %s.host, database = %s.database, "数据源已连接");
    }
    Ok(Arc::new(map))
}

/// 写操作关键字——只读连接一律拒绝（按词边界匹配，`deleted_at` 这种列名不算）
const FORBIDDEN: [&str; 10] = [
    "INSERT", "UPDATE", "DELETE", "DROP", "CREATE", "ALTER", "TRUNCATE", "REPLACE", "GRANT",
    "REVOKE",
];
const FORBIDDEN_PHRASES: [&str; 2] = ["INTO OUTFILE", "LOAD_FILE"];

fn readonly_guard(sql: &str) -> anyhow::Result<()> {
    let upper = sql.trim().to_ascii_uppercase();
    if let Some(phrase) = FORBIDDEN_PHRASES.iter().find(|k| upper.contains(**k)) {
        anyhow::bail!("只读连接拒绝执行：检测到写操作关键字 {phrase}");
    }
    let hit = upper
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .find(|token| FORBIDDEN.contains(token));
    if let Some(kw) = hit {
        anyhow::bail!("只读连接拒绝执行：检测到写操作关键字 {kw}");
    }
    if !(upper.starts_with("SELECT") || upper.starts_with("SHOW")) {
        anyhow::bail!("只读连接拒绝执行：仅允许 SELECT / SHOW 语句");
    }
    Ok(())
}

pub async fn query(
    pools: &Pools,
    source_id: &str,
    sql: &str,
) -> anyhow::Result<Vec<sqlx::mysql::MySqlRow>> {
    readonly_guard(sql)?;
    let pool = pools
        .get(source_id)
        .ok_or_else(|| anyhow::anyhow!("未知数据源：{source_id}"))?;
    let rows = sqlx::query(sql).fetch_all(pool).await?;
    Ok(rows)
}

pub async fn query_scalar_str(
    pools: &Pools,
    source_id: &str,
    sql: &str,
) -> anyhow::Result<Option<String>> {
    let rows = query(pools, source_id, sql).await?;
    Ok(rows
        .first()
        .and_then(|r| r.try_get::<Option<String>, _>(0).ok().flatten()))
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct DbHealth {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub async fn health_check(pools: &Pools, source_id: &str) -> DbHealth {
    match query_scalar_str(pools, source_id, "SELECT VERSION() AS v").await {
        Ok(v) => DbHealth {
            ok: true,
            version: v,
            error: None,
        },
        Err(e) => DbHealth {
            ok: false,
            version: None,
            error: Some(e.to_string()),
        },
    }
}
