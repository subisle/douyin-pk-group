//! 本地 SQLite：别名表 + 分组方案。
//! 写操作只落这里，`douyinxs` 永远只读。
//!
//! rusqlite 是同步 API，操作都在 1ms 内，直接在 handler 里同步调用；
//! 连接用 Mutex 包住（Connection 是 Send，包 Mutex 后 State 可跨线程共享）。

use std::sync::{Mutex, MutexGuard, OnceLock};

use anyhow::Result;
use rusqlite::Connection;

static DB: OnceLock<Mutex<Connection>> = OnceLock::new();

pub fn init(path: &str) -> Result<()> {
    if let Some(dir) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS aliases (
           alias TEXT PRIMARY KEY,
           real_name TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS plans (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           title TEXT NOT NULL,
           metric TEXT NOT NULL,
           days INTEGER NOT NULL DEFAULT 3,
           payload TEXT NOT NULL,
           created_at TEXT NOT NULL
         );",
    )?;
    DB.set(Mutex::new(conn))
        .map_err(|_| anyhow::anyhow!("SQLite 重复初始化"))?;
    tracing::info!(path, "SQLite 已就绪");
    Ok(())
}

fn db() -> MutexGuard<'static, Connection> {
    DB.get()
        .expect("SQLite 未初始化")
        .lock()
        .expect("SQLite 连接中毒")
}

pub type AliasMap = std::collections::HashMap<String, String>;

pub fn list_aliases() -> AliasMap {
    let conn = db();
    let mut stmt = conn
        .prepare("SELECT alias, real_name FROM aliases")
        .unwrap();
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .expect("查别名失败");
    rows.filter_map(Result::ok).collect()
}

pub fn upsert_alias(alias: &str, real_name: &str) {
    db().execute(
        "INSERT INTO aliases (alias, real_name) VALUES (?, ?)
             ON CONFLICT(alias) DO UPDATE SET real_name = excluded.real_name",
        rusqlite::params![alias, real_name],
    )
    .expect("写别名失败");
}

pub fn delete_alias(alias: &str) {
    db().execute(
        "DELETE FROM aliases WHERE alias = ?",
        rusqlite::params![alias],
    )
    .expect("删别名失败");
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct PlanRow {
    pub id: i64,
    pub title: String,
    pub metric: String,
    pub days: i64,
    pub payload: String,
    pub created_at: String,
}

pub fn save_plan(title: &str, metric: &str, days: i64, payload: &str) -> i64 {
    let conn = db();
    conn.execute(
        "INSERT INTO plans (title, metric, days, payload, created_at) VALUES (?, ?, ?, ?, ?)",
        rusqlite::params![
            title,
            metric,
            days,
            payload,
            chrono::Utc::now().to_rfc3339()
        ],
    )
    .expect("写方案失败");
    conn.last_insert_rowid()
}

pub fn list_plans() -> Vec<PlanRow> {
    let conn = db();
    let mut stmt = conn
        .prepare("SELECT id, title, metric, days, payload, created_at FROM plans ORDER BY id DESC LIMIT 50")
        .unwrap();
    let rows = stmt.query_map([], map_plan).expect("查方案失败");
    rows.filter_map(Result::ok).collect()
}

pub fn get_plan(id: i64) -> Option<PlanRow> {
    let conn = db();
    let mut stmt = conn
        .prepare("SELECT id, title, metric, days, payload, created_at FROM plans WHERE id = ?")
        .unwrap();
    stmt.query_row([id], map_plan).ok()
}

pub fn delete_plan(id: i64) {
    db().execute("DELETE FROM plans WHERE id = ?", [id])
        .expect("删方案失败");
}

fn map_plan(r: &rusqlite::Row<'_>) -> rusqlite::Result<PlanRow> {
    Ok(PlanRow {
        id: r.get(0)?,
        title: r.get(1)?,
        metric: r.get(2)?,
        days: r.get(3)?,
        payload: r.get(4)?,
        created_at: r.get(5)?,
    })
}
