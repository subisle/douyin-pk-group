//! Redis 缓存 / 锁封装（P0，docs/17 §7.3 / §7.5）。
//!
//! 定位：Redis **只放可丢的派生数据**（缓存、锁、去重标记），事实永远在 PG。
//! 所以这里的每个 API 都是「尽力而为」：
//!
//! - `REDIS_URL` 未配置 → 一律返回 `None` / `false`，调用方静默回落内存缓存 + 直查 PG；
//! - 连接失败 / 命令超时（500ms）→ 同样返回 `None` / `false`，只打一条**带指数退避**的
//!   `warn`（1s → 2s → … 上限 300s），恢复时补一条 `info`；
//! - **不允许任何路径把 Redis 错误冒泡成 5xx**：停 Redis 时服务行为与引入前完全一致。
//!
//! **本文件是全项目唯一允许 `use redis` 的地方**：业务模块只用这里的封装。
//! P0 只备好 API，业务调用点（音浪缓存 P1 / ip_geo 与推送锁 P2）保持现状不动。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use deadpool_redis::{Config as RedisConfig, PoolConfig, Runtime};
use redis::RedisError;

use crate::config;
use crate::state::AppState;

/// 连接池句柄别名：业务模块不必认识 deadpool。
pub type RedisPool = deadpool_redis::Pool;

/// 单条 Redis 命令的硬超时（含从池里取连接的时间）。
const OP_TIMEOUT: Duration = Duration::from_millis(500);

/// 降级 warn 的退避上限：最坏情况下每 5 分钟一条，不会每请求刷屏。
const MAX_WARN_INTERVAL: Duration = Duration::from_secs(300);

/// 缓存层当前是否处于降级（最近一次操作失败）。由 30s PING 探针与各次操作共同刷新。
static DEGRADED: AtomicBool = AtomicBool::new(false);

/// 上次降级 warn 的时间 + 下一次允许 warn 的间隔（指数退避）。
static LAST_WARN: Mutex<Option<(Instant, Duration)>> = Mutex::new(None);

/// 把「取连接 + 执行命令」整体套进 500ms 超时，失败只记录不冒泡。
///
/// 返回 `None` 有三种来源，调用方语义一致（降级）：未配置、失败、超时。
macro_rules! redis_op {
    ($st:expr, $name:expr, |$conn:ident| $body:expr) => {{
        match $st.redis.as_ref() {
            None => None,
            Some(pool) => {
                let fut = async {
                    let mut $conn = pool.get().await.map_err(|e| e.to_string())?;
                    $body.await.map_err(|e: RedisError| e.to_string())
                };
                match tokio::time::timeout(OP_TIMEOUT, fut).await {
                    Ok(Ok(v)) => {
                        note_ok();
                        Some(v)
                    }
                    Ok(Err(e)) => {
                        note_failure($name, &e);
                        None
                    }
                    Err(_) => {
                        note_failure($name, "超时 500ms");
                        None
                    }
                }
            }
        }
    }};
}

/// 建池（不实际连接：deadpool 是懒连接，首次取连接才握手）。
///
/// - 未配置 `REDIS_URL` → `None`（全内存模式，不打印告警）；
/// - 建池失败 → `None` + 一条 `warn`（服务照常启动，与写池失败的处理风格一致）。
pub fn init_pool() -> Option<RedisPool> {
    let url = config::redis_url()?;
    let limit = config::redis_pool_limit();

    let mut cfg = RedisConfig::from_url(url);
    cfg.pool = Some(PoolConfig {
        max_size: limit,
        timeouts: deadpool_redis::Timeouts {
            wait: Some(OP_TIMEOUT),
            create: Some(OP_TIMEOUT),
            recycle: Some(OP_TIMEOUT),
        },
        ..Default::default()
    });

    match cfg.create_pool(Some(Runtime::Tokio1)) {
        // 注意：不要把 url 打进日志（含口令）
        Ok(pool) => {
            tracing::info!(pool_limit = limit, "Redis 连接池已创建（懒连接）");
            Some(pool)
        }
        Err(e) => {
            tracing::warn!(error = %e, "Redis 连接池创建失败，缓存层停用（服务照常）");
            None
        }
    }
}

/// `GET key`。未配置 / 失败 / 未命中 → `None`。
pub async fn get(st: &AppState, key: &str) -> Option<String> {
    redis_op!(st, "GET", |c| redis::cmd("GET")
        .arg(key)
        .query_async::<Option<String>>(&mut c))
    .flatten()
}

/// `SET key value EX ttl`。返回是否写入成功（失败即降级，返回 `false`）。
pub async fn set(st: &AppState, key: &str, value: &str, ttl_secs: u64) -> bool {
    redis_op!(st, "SET", |c| redis::cmd("SET")
        .arg(key)
        .arg(value)
        .arg("EX")
        .arg(ttl_secs)
        .query_async::<redis::Value>(&mut c))
    .is_some()
}

/// `DEL key`。命令成功即 `true`（键不存在也算成功）；失败降级返回 `false`。
pub async fn del(st: &AppState, key: &str) -> bool {
    redis_op!(st, "DEL", |c| redis::cmd("DEL")
        .arg(key)
        .query_async::<i64>(&mut c))
    .is_some()
}

/// 抢锁：`SET key 1 NX EX ttl_secs`。抢到 = `true`；已被占 / 降级 = `false`。
///
/// P2 的推送去重锁 `pk:lock:push:{job_id}` 用它；拿不到锁就跳过本轮，
/// Redis 不可用时调用方沿用现有 DB 抢占语义。
pub async fn lock(st: &AppState, key: &str, ttl_secs: u64) -> bool {
    redis_op!(st, "SET NX", |c| redis::cmd("SET")
        .arg(key)
        .arg("1")
        .arg("NX")
        .arg("EX")
        .arg(ttl_secs)
        .query_async::<Option<String>>(&mut c))
    .flatten()
    .is_some()
}

/// 放锁：`DEL key`（P0/P2 均为「自己持有才删」的简单语义，非 CAS）。
pub async fn unlock(st: &AppState, key: &str) -> bool {
    redis_op!(st, "UNLOCK", |c| redis::cmd("DEL")
        .arg(key)
        .query_async::<i64>(&mut c))
    .is_some()
}

/// `/api/health` 用的三态：未配置 = `"disabled"`；PING 通 = `"up"`；配了但连不上 = `"down"`。
///
/// `down` 只表示缓存层降级，不影响整体 health 语义（HTTP 仍 200）。
pub async fn status(st: &AppState) -> &'static str {
    if st.redis.is_none() {
        return "disabled";
    }
    match redis_op!(st, "PING", |c| redis::cmd("PING")
        .query_async::<String>(&mut c))
    {
        Some(_) => "up",
        None => "down",
    }
}

/// 缓存层是否处于降级状态（最近一次操作失败且尚未恢复）。
pub fn degraded() -> bool {
    DEGRADED.load(Ordering::Relaxed)
}

/* ------------------------------ 内部：超时 + 降级 ------------------------------ */

/// 成功：从降级中恢复时补一条 info，并把退避窗口复位。
fn note_ok() {
    if DEGRADED.swap(false, Ordering::Relaxed) {
        *LAST_WARN.lock().unwrap_or_else(|e| e.into_inner()) = None;
        tracing::info!("Redis 已恢复，缓存层重新启用");
    }
}

/// 失败：置降级标志，并按指数退避决定是否打这条 warn。
fn note_failure(op: &str, err: &str) {
    DEGRADED.store(true, Ordering::Relaxed);
    let now = Instant::now();
    {
        // 只在这里持锁：日志放到锁外，避免持锁做 IO
        let mut guard = LAST_WARN.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((last, interval)) = *guard {
            if now.duration_since(last) < interval {
                return; // 退避窗口内：静默
            }
            *guard = Some((now, (interval * 2).min(MAX_WARN_INTERVAL)));
        } else {
            *guard = Some((now, Duration::from_secs(1)));
        }
    }
    tracing::warn!(
        op = op,
        error = err,
        "Redis 操作失败，已降级为内存缓存 / 直查 PG（不影响请求）"
    );
}
