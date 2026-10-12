//! 内置主播名册装载（docs 需求：新部署自带 105 人主播数据，不依赖远程源库）。
//!
//! 行为：启动时若 person 表为空（全新部署），把 `seeds/roster.json`（远程同步后
//! 导出的快照，2026-10-06）写入 person / account。**只在空表时装载**——
//! 已有数据（含远程同步/导入的）一律不动；更新走「同步主播」或导入流程。

use std::sync::Arc;

use serde::Deserialize;
use sqlx::Row;

use crate::state::AppState;

const ROSTER_SEED: &str = include_str!("seeds/roster.json");

#[derive(Deserialize)]
struct Seed {
    #[allow(dead_code)]
    generated: String,
    #[allow(dead_code)]
    source: String,
    persons: Vec<SeedPerson>,
    accounts: Vec<SeedAccount>,
}

#[derive(Deserialize)]
struct SeedPerson {
    id: i64,
    name: String,
    gender: String,
    #[serde(rename = "masterId")]
    master_id: Option<i64>,
    #[serde(rename = "generation")]
    generation: Option<i64>,
    #[serde(rename = "groupName")]
    group_name: Option<String>,
    #[serde(rename = "avatarUrl")]
    avatar_url: Option<String>,
    #[serde(rename = "hideInDailyReport")]
    hide_in_daily_report: Option<bool>,
    status: String,
    #[serde(rename = "joinedAt")]
    joined_at: Option<String>,
    #[serde(rename = "deletedAt")]
    deleted_at: Option<String>,
}

#[derive(Deserialize)]
struct SeedAccount {
    id: i64,
    #[serde(rename = "personId")]
    person_id: i64,
    #[serde(rename = "anchorId")]
    anchor_id: String,
    #[serde(rename = "douyinNo")]
    douyin_no: Option<String>,
    #[serde(rename = "anchorName")]
    anchor_name: Option<String>,
    #[serde(rename = "isPrimary")]
    is_primary: Option<bool>,
    status: String,
    #[serde(rename = "createdAt")]
    created_at: Option<String>,
}

fn parse() -> &'static Seed {
    use std::sync::OnceLock;
    static SEED: OnceLock<Seed> = OnceLock::new();
    SEED.get_or_init(|| serde_json::from_str(ROSTER_SEED).expect("内置名册 JSON 损坏"))
}

/// 表行数（出错按 None 处理，调用方跳过装载）
async fn person_count(pool: &sqlx::PgPool) -> Option<i64> {
    sqlx::query(&crate::pq::rewrite("SELECT COUNT(*) AS n FROM person"))
        .fetch_one(pool)
        .await
        .ok()
        .and_then(|r| {
            r.try_get::<i64, _>(0)
                .or_else(|_| r.try_get::<i64, _>(0).map(|v| v as i64))
                .ok()
        })
}

/// 启动时调用：person 表为空 → 装载内置名册。失败只告警不阻断启动。
pub async fn seed_if_empty(st: &Arc<AppState>) {
    let Some(pool) = st.writer.as_ref() else {
        tracing::info!("写库未配置，跳过内置名册装载");
        return;
    };
    match person_count(pool).await {
        Some(0) => {}
        Some(n) => {
            tracing::info!(n, "person 表非空，跳过内置名册装载");
            return;
        }
        None => {
            tracing::warn!("person 表不存在（先执行 migrate），跳过内置名册装载");
            return;
        }
    }

    let seed = parse();
    let mut tx = match pool.begin().await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "内置名册装载失败（开事务）");
            return;
        }
    };
    let mut persons = 0u64;
    for p in &seed.persons {
        let res = sqlx::query(&crate::pq::rewrite(
            "INSERT INTO person (id, name, gender, master_id, generation, group_name, \
             avatar_url, hide_in_daily_report, status, joined_at, deleted_at) \
             VALUES (?,?,?,?,?,?,?,?,?,?,?)",
        ))
        .bind(p.id)
        .bind(&p.name)
        .bind(&p.gender)
        .bind(p.master_id)
        .bind(p.generation.map(|v| v as i32))
        .bind(&p.group_name)
        .bind(&p.avatar_url)
        .bind(p.hide_in_daily_report.unwrap_or(false) as i16)
        .bind(&p.status)
        .bind(&p.joined_at)
        .bind(&p.deleted_at)
        .execute(&mut *tx)
        .await;
        if res.is_ok() {
            persons += 1;
        }
    }
    let mut accounts = 0u64;
    for a in &seed.accounts {
        let res = sqlx::query(&crate::pq::rewrite(
            "INSERT INTO account (id, person_id, anchor_id, douyin_no, anchor_name, \
             is_primary, status, created_at) VALUES (?,?,?,?,?,?,?,?)",
        ))
        .bind(a.id)
        .bind(a.person_id)
        .bind(&a.anchor_id)
        .bind(a.douyin_no.clone().unwrap_or_default())
        .bind(a.anchor_name.clone().unwrap_or_default())
        .bind(a.is_primary.unwrap_or(false) as i16)
        .bind(&a.status)
        .bind(&a.created_at)
        .execute(&mut *tx)
        .await;
        if res.is_ok() {
            accounts += 1;
        }
    }
    if let Err(e) = tx.commit().await {
        tracing::error!(error = %e, "内置名册装载提交失败");
        return;
    }
    tracing::info!(persons, accounts, "内置主播名册已装载（空表启动）");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_parses_and_is_complete() {
        let seed = parse();
        assert_eq!(seed.persons.len(), 105, "内置名册应有 105 人");
        assert_eq!(seed.accounts.len(), 118, "内置账号应有 118 个");
        // 主号唯一性抽查：每个 person 至多一个 is_primary
        let mut primary_per_person: std::collections::HashMap<i64, u32> = Default::default();
        for a in &seed.accounts {
            if a.is_primary.unwrap_or(false) {
                *primary_per_person.entry(a.person_id).or_default() += 1;
            }
        }
        assert!(
            primary_per_person.values().all(|&n| n <= 1),
            "存在多个主号的 person"
        );
        // 名字无空
        assert!(seed.persons.iter().all(|p| !p.name.trim().is_empty()));
    }
}
