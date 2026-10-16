//! public_teams::binds —— 自 public_teams.rs 按业务域拆分（对外语义不变）。
//!
//! 2026-10-16「抖音号审计」整页下线（用户口径）：展示端提交入口
//! （`my_claim_get` / `my_claim_post`）、管理端裁决
//! （`admin_anchor_claims` / `admin_anchor_claim_screenshot` / `admin_anchor_claim_decide`）
//! 以及只服务它们的 `person_name_of` 一并删除，对应路由（`/…/my/claim`、
//! `/api/admin/anchor-claims*`）与管理端页面同步移除。
//! 表 `anchor_claims` 与迁移**保留不删**（历史截图与申请记录仍在库里，只停用代码路径）。
//!
//! 本文件现在只剩 `id_of_pub`（minutes.rs 排行榜按 person_id 算名次用）。
//! 保留文件名与 `mod.rs` 的 `pub use binds::*`，避免为一个函数改模块布局。
//! 该函数用 `pub` 而非 `pub(super)`：`pub use binds::*` 只转发 pub 项，
//! 否则 `mod.rs` 的 glob re-export 会报 "doesn't reexport anything" 警告。

use sqlx::Row as _; // try_get 来自 Row trait

/// 取排行榜行的 person_id：PG 里是 BIGINT，历史上也灌过 INT4 的库，
/// 统一按 i64 解、钳到非负，脏值/解不出回 0。
pub fn id_of_pub(r: &sqlx::postgres::PgRow) -> u64 {
    r.try_get::<i64, _>("person_id")
        .map(|v| v.max(0) as u64)
        .unwrap_or(0)
}
