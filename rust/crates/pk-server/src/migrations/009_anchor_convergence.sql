-- 009_anchor_convergence · 主号收敛 + 副号解绑留档（2026-10-10 审计 P2-10 / P2-11）
--
-- 背景：
--   P2-11  生产实测 person 1000001（哈基勺）/ 1000004（狼森）有 account 行但 is_primary 全 0
--          → PRIMARY_ANCHOR_SQL 取不到 → 导入回填 anchor_id=''。
--   P2-10  解绑抖音号直接 DELETE account → wave_snapshot（按 anchor_id 关联，无 person_id）
--          变孤儿，副号历史音浪追溯性消失。
--
-- 本迁移**只建索引，不改任何数据**：
--   1) account(person_id, is_primary)：ensure_primary / 主号查询的高频路径
--   2) wave_snapshot(anchor_id)：JOIN account 的归属查询路径
--      （uk_anchor_date(anchor_id, biz_date) 最左前缀本可覆盖，显式补一个单列索引
--        让解绑前的 COUNT/SUM 快照对账查询也走索引）
--   3) duration_snapshot(anchor_id)：同上（时长快照同结构）
--
-- 数据收敛（1000001/1000004 的 is_primary 置 1）不在 DDL 里做：
--   - 迁移账号（douyin_migrator）只负责结构；数据口径由代码负责
--   - 代码侧 ensure_primary()（admin.rs）已在每次导入回填 anchor_id 前幂等收敛：
--     有 account 无 is_primary=1 → 把最早（id 最小）的号置为主号再取
--   - 存量孤儿快照由 unbind_account 的新逻辑留档：此后解绑会把快照汇总落
--     import_pending 留档（吸收进台账），解绑前产生的孤儿不影响新逻辑
--   → 部署后无需手工改库，跑一次 pk-server migrate + 重启即完成收敛

CREATE INDEX idx_account_person_primary
  ON account (person_id, is_primary);

CREATE INDEX idx_wave_snapshot_anchor
  ON wave_snapshot (anchor_id);

CREATE INDEX idx_duration_snapshot_anchor
  ON duration_snapshot (anchor_id);
