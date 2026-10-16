//! PostgreSQL 只读层：业务库连接池 + 只读守卫
//!
//! 三层保障：
//! 1. 所有 SQL 都写死在 dialect.rs，不接受外部传入
//! 2. 只读守卫只放行 SELECT，含写操作关键字直接抛错
//! 3. 账号本身只配查询权限（见 README）
//!
//! 2026-10 数据库收敛：PostgreSQL 是唯一业务库；远程 MySQL 只承担备份职责。

use std::collections::HashMap;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Row};

use crate::config::SourceConfig;

/// 数据源连接池集合。
///
/// 建连失败的数据源记成 `None` + 错误原因，**不让整个服务起不来**：
/// 数据库抽风时，管理员还能登录进后台看情况，对外展示页也能用方案快照兜底
/// （展示页这时会把 `stale` 置 true）。真正查库时再把错误抛出来。
#[derive(Clone)]
pub struct Pools {
    map: HashMap<String, Option<PgPool>>,
    errors: HashMap<String, String>,
}

impl Pools {
    /// 是否至少有一个数据源连上了。展示页据此判断「音浪到底是不是实时的」。
    pub fn any_available(&self) -> bool {
        self.map.values().any(|p| p.is_some())
    }

    fn get(&self, source_id: &str) -> anyhow::Result<&PgPool> {
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

/// 把会话时区钉成 `Asia/Shanghai`（2026-10-16 修「导入批次时间早 8 小时」）。
///
/// 问题：sqlx 建出来的 PG 会话时区是 **UTC**（实测 `SHOW timezone` = UTC），
/// 而库里 `import_batch.created_at` / `person_avatar.synced_at` / `admin_messages.created_at`
/// 这些都是 `timestamptz`：所有 `TO_CHAR(created_at, 'YYYY-MM-DD HH24:MI')` 按 UTC 渲染。
/// 结果是导入批次列表的「时间」列比本地时间**早 8 小时**（本地 14:44 显示 06:44），
/// 凌晨导入的批次连**日期**都会少一天 —— 运营看到的就是「业务日期不对」。
///
/// 为什么用 `after_connect` 而不是连接串的 `options(-c timezone=…)`：
/// 后者在 sqlx 0.8 + 本机 PG 上实测**不生效**（`SHOW timezone` 仍是 UTC），
/// `after_connect` 里 SET 一次对**池里每条连接**都成立（实测 3/3）。
/// 读池与写池共用这一个函数，保证同一份数据在哪条连接上渲染都一致。
pub fn with_local_timezone(pool: PgPoolOptions) -> PgPoolOptions {
    pool.after_connect(|conn, _meta| {
        Box::pin(async move {
            sqlx::query("SET TIME ZONE 'Asia/Shanghai'")
                .execute(conn)
                .await?;
            Ok(())
        })
    })
}

/// 会话时区（与 docs/16「会话时区 Asia/Shanghai」同一口径）
pub const SESSION_TIMEZONE: &str = "Asia/Shanghai";

pub async fn connect_pool(s: &SourceConfig) -> anyhow::Result<PgPool> {
    let opts: PgConnectOptions = s
        .url()
        .parse()
        .map_err(|e| anyhow::anyhow!("数据源 {} 连接串解析失败: {e}", s.id))?;
    with_local_timezone(
        PgPoolOptions::new()
            .max_connections(s.pool_limit)
            .min_connections(0)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .idle_timeout(std::time::Duration::from_secs(30)),
    )
    .connect_with(opts)
    .await
    .map_err(|e| anyhow::anyhow!("数据源 {} 连接失败: {e}", s.id))
}

/// 写操作关键字——只读连接一律拒绝（按词边界匹配，`deleted_at` 这种列名不算）
const FORBIDDEN: [&str; 10] = [
    "INSERT", "UPDATE", "DELETE", "DROP", "CREATE", "ALTER", "TRUNCATE", "REPLACE", "GRANT",
    "REVOKE",
];
const FORBIDDEN_PHRASES: [&str; 2] = ["COPY ", "LOAD_FILE"];

fn readonly_guard(sql: &str) -> anyhow::Result<()> {
    let upper = sql.trim().to_ascii_uppercase();
    if let Some(phrase) = FORBIDDEN_PHRASES.iter().find(|k| upper.contains(**k)) {
        anyhow::bail!("只读连接拒绝执行：检测到写操作关键字 {}", phrase.trim());
    }
    let hit = upper
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .find(|token| FORBIDDEN.contains(token));
    if let Some(kw) = hit {
        anyhow::bail!("只读连接拒绝执行：检测到写操作关键字 {kw}");
    }
    if !upper.starts_with("SELECT") {
        anyhow::bail!("只读连接拒绝执行：仅允许 SELECT 语句");
    }
    Ok(())
}

/// 取某数据源的池句柄（写库/聚合等需要直接 sqlx 调用的场景）
pub fn pool_of(pools: &Pools, source_id: &str) -> anyhow::Result<PgPool> {
    pools.get(source_id).cloned()
}

pub async fn query(
    pools: &Pools,
    source_id: &str,
    sql: &str,
) -> anyhow::Result<Vec<sqlx::postgres::PgRow>> {
    readonly_guard(sql)?;
    // 与 query_with_binds 同一约定：入口统一做 `?` → `$N` 重写，调用方不用关心方言。
    // 传 `$N` 原生写法的 SQL 重写后原样通过（没有 `?` 就不动）。
    let sql = crate::pq::rewrite(sql);
    let pool = pools.get(source_id)?;
    let rows = sqlx::query(&sql).fetch_all(pool).await?;
    Ok(rows)
}

/// 带参数的只读查询：SQL 仍需过只读守卫，参数由调用方 bind
///
/// 2026-10-12：执行前先做 `crate::pq::rewrite`，把 MySQL 风格的 `?` 占位符
/// 机械重写成 PG 的 `$1..$N`——之前漏了这一步，`WHERE p.name = ? ORDER BY …`
/// 直接送进 PG 会在 `ORDER` 处报语法错误（`?` 在 PG 里是裸的未知 token，
/// 恰好卡在 ORDER BY 前面）。前提：调用方 SQL 里要么全是 `?` 要么全是 `$N`，
/// 不混用（见 pq.rs 头注释）。
pub async fn query_with_binds<F>(
    pools: &Pools,
    source_id: &str,
    sql: &str,
    bind: F,
) -> anyhow::Result<Vec<sqlx::postgres::PgRow>>
where
    F: FnOnce(
        sqlx::query::Query<'_, sqlx::Postgres, sqlx::postgres::PgArguments>,
    ) -> sqlx::query::Query<'_, sqlx::Postgres, sqlx::postgres::PgArguments>,
{
    readonly_guard(sql)?;
    let sql = crate::pq::rewrite(sql);
    let pool = pools.get(source_id)?;
    let rows = bind(sqlx::query(&sql)).fetch_all(pool).await?;
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
    match query_scalar_str(pools, source_id, "SELECT version() AS v").await {
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

/// rows_affected 统一适配：PostgreSQL 返回 i64。
/// 全库统一走 `affected()`（u64），调用点不用关心方言。
pub trait PgResultExt {
    fn affected(&self) -> u64;
}

impl PgResultExt for sqlx::postgres::PgQueryResult {
    fn affected(&self) -> u64 {
        self.rows_affected()
    }
}
