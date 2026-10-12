//! MySQL 只读层：多数据源连接池 + 只读守卫
//!
//! 三层保障（与 TS 版一致）：
//! 1. 所有 SQL 都写死在 dialect.rs，不接受外部传入
//! 2. 只读守卫只放行 SELECT / SHOW，含写操作关键字直接抛错
//! 3. 账号本身只配查询权限（见 README）

use std::collections::HashMap;

use sqlx::pgrs::{MySqlConnectOptions, MySqlPoolOptions};
use sqlx::{MySqlPool, Row};

use crate::config::SourceConfig;

/// 数据源连接池集合。
///
/// 建连失败的数据源记成 `None` + 错误原因，**不让整个服务起不来**：
/// 数据库抽风时，管理员还能登录进后台看情况，对外展示页也能用方案快照兜底
/// （展示页这时会把 `stale` 置 true）。真正查库时再把错误抛出来。
#[derive(Clone)]
pub struct Pools {
    map: HashMap<String, Option<MySqlPool>>,
    errors: HashMap<String, String>,
}

impl Pools {
    /// 是否至少有一个数据源连上了。展示页据此判断「音浪到底是不是实时的」。
    pub fn any_available(&self) -> bool {
        self.map.values().any(|p| p.is_some())
    }

    fn get(&self, source_id: &str) -> anyhow::Result<&MySqlPool> {
        let entry = self
            .map
            .get(source_id)
            .ok_or_else(|| anyhow::anyhow!("未知数据源：{source_id}"))?;
        match entry {
            Some(p) => Ok(p),
            None => Err(anyhow::anyhow!(
                "数据源 {source_id} 不可用：{}",
                self.errors
                    .get(source_id)
                    .map(String::as_str)
                    .unwrap_or("未知原因")
            )),
        }
    }
}

pub async fn build_pools(sources: &[SourceConfig]) -> anyhow::Result<Pools> {
    let mut map = HashMap::new();
    let mut errors = HashMap::new();
    for s in sources {
        let pool = match connect_pool(s).await {
            Ok(p) => {
                tracing::info!(source = %s.id, host = %s.host, database = %s.database, "数据源已连接");
                Some(p)
            }
            Err(e) => {
                // 记下来继续启动：后台和展示页都还要用
                tracing::error!(source = %s.id, host = %s.host, error = %e, "数据源连接失败，服务继续启动");
                errors.insert(s.id.clone(), e.to_string());
                None
            }
        };
        map.insert(s.id.clone(), pool);
    }
    Ok(Pools { map, errors })
}

pub async fn connect_pool(s: &SourceConfig) -> anyhow::Result<MySqlPool> {
    let opts: MySqlConnectOptions = s
        .url()
        .parse()
        .map_err(|e| anyhow::anyhow!("数据源 {} 连接串解析失败: {e}", s.id))?;
    // 该库对 prepared statement 兼容性不好（报 1835 Malformed communication packet），
    // 关掉语句缓存走 text protocol
    let opts = opts.statement_cache_capacity(0);
    MySqlPoolOptions::new()
        .max_connections(s.pool_limit)
        .min_connections(0)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .idle_timeout(std::time::Duration::from_secs(30))
        .connect_with(opts)
        .await
        .map_err(|e| anyhow::anyhow!("数据源 {} 连接失败: {e}", s.id))
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

/// 取某数据源的池句柄（写库/聚合等需要直接 sqlx 调用的场景）
pub fn pool_of(pools: &Pools, source_id: &str) -> anyhow::Result<MySqlPool> {
    pools.get(source_id).cloned()
}

pub async fn query(
    pools: &Pools,
    source_id: &str,
    sql: &str,
) -> anyhow::Result<Vec<sqlx::pgrs::MySqlRow>> {
    readonly_guard(sql)?;
    let pool = pools.get(source_id)?;
    let rows = sqlx::query(sql).fetch_all(pool).await?;
    Ok(rows)
}

/// 带参数的只读查询：SQL 仍需过只读守卫，参数由调用方 bind
pub async fn query_with_binds<F>(
    pools: &Pools,
    source_id: &str,
    sql: &str,
    bind: F,
) -> anyhow::Result<Vec<sqlx::pgrs::MySqlRow>>
where
    F: FnOnce(
        sqlx::query::Query<'_, sqlx::MySql, sqlx::pgrs::MySqlArguments>,
    ) -> sqlx::query::Query<'_, sqlx::MySql, sqlx::pgrs::MySqlArguments>,
{
    readonly_guard(sql)?;
    let pool = pools.get(source_id)?;
    let rows = bind(sqlx::query(sql)).fetch_all(pool).await?;
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
