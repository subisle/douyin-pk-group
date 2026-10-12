//! store::legacy —— 自 store.rs 按业务域拆分（对外语义不变）。

use super::*; // 共享 w() / now_str() / AppState 等内部工具

/* -------------------- 旧 SQLite 数据一次性搬家（migrate 触发） -------------------- */

/// 把旧 SQLite（SQLITE_PATH）里的运行时数据幂等搬进 MySQL。
/// 只搬一次：config 表 sqlite_carried=1 后直接跳过（SQLite 文件保持原样不删，作留底）。
/// 表缺列/文件不存在都按「没得搬」处理，绝不阻塞 migrate。
pub async fn carry_legacy_sqlite(pool: &PgPool, sqlite_path: &str) -> Result<()> {
    use sqlx::Row;

    // 1. 已搬过 → 跳过
    let carried = sqlx::query("SELECT cfg_value FROM config WHERE cfg_key = 'sqlite_carried'")
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    if let Some(r) = carried {
        if r.try_get::<String, _>(0).map(|v| v == "1").unwrap_or(false) {
            tracing::info!("旧 SQLite 数据已搬家过，跳过（config.sqlite_carried=1）");
            return Ok(());
        }
    }
    if !std::path::Path::new(sqlite_path).exists() {
        tracing::info!(path = sqlite_path, "没有旧 SQLite 文件，跳过搬家");
        return Ok(());
    }
    let conn = match rusqlite::Connection::open_with_flags(
        sqlite_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = sqlite_path, error = %e, "旧 SQLite 打不开，跳过搬家");
            return Ok(());
        }
    };
    let has_col = |table: &str, col: &str| -> bool {
        let mut stmt = match conn.prepare(&format!("PRAGMA table_info({table})")) {
            Ok(s) => s,
            Err(_) => return false,
        };
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default();
        cols.iter().any(|c| c == col)
    };
    let has_table = |table: &str| -> bool {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |_| Ok(()),
        )
        .is_ok()
    };
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // 2. aliases
    if has_table("aliases") {
        let mut stmt = conn.prepare("SELECT alias, real_name FROM aliases")?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(Result::ok)
            .collect();
        for (a, rn) in &rows {
            sqlx::query(
                "INSERT INTO aliases (alias, real_name) VALUES (?, ?)
                 ON CONFLICT (alias) DO UPDATE SET real_name = EXCLUDED.real_name",
            )
            .bind(a)
            .bind(rn)
            .execute(pool)
            .await?;
        }
        tracing::info!(n = rows.len(), "搬家：aliases");
    }

    // 3. roster_state
    if has_table("roster_state") {
        let mut stmt = conn.prepare("SELECT event_type, text, updated_at FROM roster_state")?;
        let rows: Vec<(String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .filter_map(Result::ok)
            .collect();
        for (et, body, at) in &rows {
            sqlx::query(
                "INSERT INTO roster_state (event_type, body, updated_at) VALUES (?, ?, ?)
                 ON CONFLICT (event_type) DO UPDATE SET body = EXCLUDED.body, updated_at = EXCLUDED.updated_at",
            )
            .bind(et)
            .bind(body)
            .bind(at)
            .execute(pool)
            .await?;
        }
        tracing::info!(n = rows.len(), "搬家：roster_state");
    }

    // 4. config（跳过搬家标记自身）
    if has_table("config") {
        let mut stmt = conn.prepare("SELECT key, value FROM config")?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(Result::ok)
            .collect();
        for (k, v) in &rows {
            if k == "sqlite_carried" {
                continue;
            }
            sqlx::query(
                "INSERT INTO config (cfg_key, cfg_value, updated_at) VALUES (?, ?, ?)
                 ON CONFLICT (cfg_key) DO UPDATE SET cfg_value = EXCLUDED.cfg_value, updated_at = EXCLUDED.updated_at",
            )
            .bind(k)
            .bind(v)
            .bind(&now)
            .execute(pool)
            .await?;
        }
        tracing::info!(n = rows.len(), "搬家：config");
    }

    // 5. bot_sessions
    if has_table("bot_sessions") {
        let mut stmt = conn.prepare(
            "SELECT channel, conversation_id, kind, COALESCE(last_text,''), last_seen, is_admin FROM bot_sessions",
        )?;
        let rows: Vec<(String, String, String, String, String, i64)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .filter_map(Result::ok)
            .collect();
        for (c, cid, kind, lt, ls, adm) in &rows {
            sqlx::query(
                "INSERT INTO bot_sessions (channel, conversation_id, kind, last_text, last_seen, is_admin)
                 VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT (channel, conversation_id) DO UPDATE SET kind = EXCLUDED.kind, last_text = EXCLUDED.last_text,
                   last_seen = EXCLUDED.last_seen, is_admin = EXCLUDED.is_admin",
            )
            .bind(c)
            .bind(cid)
            .bind(kind)
            .bind(lt)
            .bind(ls)
            .bind(*adm as i16)
            .execute(pool)
            .await?;
        }
        tracing::info!(n = rows.len(), "搬家：bot_sessions");
    }

    // 6. plans（published 列在老版本的 plans 表里可能没有）
    if has_table("plans") {
        let pub_col = has_col("plans", "published");
        let et_col = has_col("plans", "event_type");
        let sql = if pub_col && et_col {
            "SELECT id, title, metric, days, payload, created_at, published, event_type FROM plans"
                .to_string()
        } else if et_col {
            "SELECT id, title, metric, days, payload, created_at, 0, event_type FROM plans"
                .to_string()
        } else {
            "SELECT id, title, metric, days, payload, created_at, 0, '' FROM plans".to_string()
        };
        let mut stmt = conn.prepare(&sql)?;
        type PlanRowTuple = (i64, String, String, i64, String, String, i64, String);
        let rows: Vec<PlanRowTuple> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            })?
            .filter_map(Result::ok)
            .collect();
        for (id, title, metric, days, payload, created, published, et) in &rows {
            sqlx::query(
                "INSERT INTO plans (id, title, metric, days, payload, created_at, published, event_type)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (id) DO UPDATE SET title = EXCLUDED.title, metric = EXCLUDED.metric,
                   days = EXCLUDED.days, payload = EXCLUDED.payload, created_at = EXCLUDED.created_at,
                   published = EXCLUDED.published, event_type = EXCLUDED.event_type",
            )
            .bind(id)
            .bind(title)
            .bind(metric)
            .bind(days)
            .bind(payload)
            .bind(created)
            .bind(*published as i16)
            .bind(et)
            .execute(pool)
            .await?;
        }
        tracing::info!(n = rows.len(), "搬家：plans");
    }

    // 7. my_accounts（is_admin 列可能没有）
    if has_table("my_accounts") {
        let admin_col = has_col("my_accounts", "is_admin");
        let sql = if admin_col {
            "SELECT name, phone, pass, is_admin, created_at FROM my_accounts".to_string()
        } else {
            "SELECT name, phone, pass, 0, created_at FROM my_accounts".to_string()
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows: Vec<(String, String, String, i64, String)> = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .filter_map(Result::ok)
            .collect();
        for (name, phone, pass, is_admin, created) in &rows {
            sqlx::query(
                "INSERT INTO my_accounts (name, phone, pass, is_admin, created_at)
                 VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT (name) DO UPDATE SET phone = EXCLUDED.phone, pass = EXCLUDED.pass,
                   is_admin = EXCLUDED.is_admin, created_at = EXCLUDED.created_at",
            )
            .bind(name)
            .bind(phone)
            .bind(hash_my_password(pass))
            .bind(*is_admin as i16)
            .bind(created)
            .execute(pool)
            .await?;
        }
        tracing::info!(n = rows.len(), "搬家：my_accounts");
    }

    // 8. anchor_claims（截图 BLOB 原样搬）
    if has_table("anchor_claims") {
        let mut stmt = conn.prepare(
            "SELECT id, person_id, person_name, team, douyin_no, screenshot, mime, status,
                    COALESCE(reject_reason,''), created_at, COALESCE(decided_at,''), COALESCE(decided_by,'')
             FROM anchor_claims",
        )?;
        type AnchorClaimRowTuple = (
            i64,
            i64,
            String,
            String,
            String,
            Vec<u8>,
            String,
            String,
            String,
            String,
            String,
            String,
        );
        let rows: Vec<AnchorClaimRowTuple> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                ))
            })?
            .filter_map(Result::ok)
            .collect();
        for row in &rows {
            sqlx::query(
                "INSERT INTO anchor_claims
                   (id, person_id, person_name, team, douyin_no, screenshot, mime, status,
                    reject_reason, created_at, decided_at, decided_by)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (id) DO UPDATE SET person_id = EXCLUDED.person_id, person_name = EXCLUDED.person_name,
                   team = EXCLUDED.team, douyin_no = EXCLUDED.douyin_no, screenshot = EXCLUDED.screenshot,
                   mime = EXCLUDED.mime, status = EXCLUDED.status, reject_reason = EXCLUDED.reject_reason,
                   created_at = EXCLUDED.created_at, decided_at = EXCLUDED.decided_at, decided_by = EXCLUDED.decided_by",
            )
            .bind(row.0)
            .bind(row.1)
            .bind(&row.2)
            .bind(&row.3)
            .bind(&row.4)
            .bind(&row.5)
            .bind(&row.6)
            .bind(&row.7)
            .bind(&row.8)
            .bind(&row.9)
            .bind(&row.10)
            .bind(&row.11)
            .execute(pool)
            .await?;
        }
        tracing::info!(n = rows.len(), "搬家：anchor_claims");
    }

    // 9. 落搬家标记
    sqlx::query(
        "INSERT INTO config (cfg_key, cfg_value, updated_at) VALUES ('sqlite_carried', '1', ?)
         ON CONFLICT (cfg_key) DO UPDATE SET cfg_value = '1', updated_at = EXCLUDED.updated_at",
    )
    .bind(&now)
    .execute(pool)
    .await?;
    tracing::info!("旧 SQLite 运行时数据已全部并入 MySQL（sqlite_carried=1）");
    Ok(())
}