//! store::import_pending —— 导入暂存区（未匹配台账的行）清理。
//!
//! 表结构见 migrations/001_core.sql（import_pending）。前台暂存 UI 已移除，
//! 但导入时「有抖音号 / 采集 ID 却没匹配上台账」的行**仍会持续写入**这张表
//! （admin.rs 的暂存分支 / 解绑留档分支），没人清理就会一直涨。
//!
//! 这里只提供**手动触发**的保留期清理（DELETE /api/admin/import/pending?pruneDays=N），
//! 不挂任何自动任务——删数据必须由人来按。已被认领的行（absorbed_at 非空）是
//! 补账留档（解绑历史也在里面），本函数**永不动它们**。

use super::*; // 共享 w() / AppState 等内部工具

/// 校验 pruneDays 参数：必须 > 0，否则 None。
/// 抽成纯函数（不连库）是为了让「≤0 = 手滑全清，必须拒」这条口径有单元测试盯着；
/// 调用方（admin 的 ?pruneDays= 解析处）也用它归一口径。
/// 用 i32：make_interval(days => …) 的形参就是 int，sqlx 绑 i64 会落成 int8，
/// PG 不做函数实参的 int8→int4 隐式收窄 → `function make_interval(days => bigint) does not exist`。
pub(crate) fn valid_prune_days(v: i32) -> Option<i32> {
    (v > 0).then_some(v)
}

/// 按保留期清理暂存行：只删「还没被认领且早于保留期」的行，返回删除条数。
///
/// - `older_than_days`：保留天数，经 [`valid_prune_days`] 校验（≤0 直接拒绝并告警）；
///   正常口径 ≥ 7（详见 docs/22-数据库运维说明.md 的 runbook）。
/// - 时间比较全部在 PG 侧做（created_at < now() - make_interval(days => $1)），
///   天数走**绑定参数**，SQL 无任何字符串拼接。
/// - 写失败（写池不可用 / 表不存在等）返回 0 并记 error 日志，不 panic。
pub async fn import_pending_prune(st: &AppState, older_than_days: i32) -> u64 {
    let Some(days) = valid_prune_days(older_than_days) else {
        tracing::warn!(older_than_days, "pruneDays 必须 > 0，已拒绝执行清理");
        return 0;
    };
    let Ok(pool) = w(st) else {
        return 0;
    };
    match sqlx::query(&crate::pq::rewrite(
        "DELETE FROM import_pending \
         WHERE absorbed_at IS NULL AND created_at < now() - make_interval(days => ?)",
    ))
    .bind(days)
    .execute(pool)
    .await
    {
        Ok(res) => {
            let n = res.affected();
            if n > 0 {
                tracing::info!(days, deleted = n, "已清理过期导入暂存行");
            }
            n
        }
        Err(e) => {
            tracing::error!(error = %e, days, "清理导入暂存行失败");
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::valid_prune_days;

    /// 保留期下限：≤0 一律拒绝（防手滑把整表清空）；正数原样放行。
    /// 函数本体首行就调这个守卫，单元测试不用连库也能盯住口径。
    #[test]
    fn prune_days_must_be_positive() {
        assert_eq!(valid_prune_days(0), None, "0 = 全清，必须拒");
        assert_eq!(valid_prune_days(-1), None, "负数必须拒");
        assert_eq!(valid_prune_days(-100), None);
        assert_eq!(valid_prune_days(7), Some(7));
        assert_eq!(valid_prune_days(30), Some(30));
        assert_eq!(
            valid_prune_days(3650),
            Some(3650),
            "大天数合法（= 基本不删）"
        );
    }
}
