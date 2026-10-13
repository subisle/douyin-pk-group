-- 021 · 补建 wave_snapshot 唯一索引（2026-10-14 审计）
--
-- 001_core.sql 里有两个同名唯一索引 uk_anchor_date：
--   :123  CREATE UNIQUE INDEX IF NOT EXISTS uk_anchor_date ON duration_snapshot (anchor_id, biz_date)
--   :329  CREATE UNIQUE INDEX IF NOT EXISTS uk_anchor_date ON wave_snapshot (anchor_id, biz_date)
-- PG 的索引名在 schema 内全局唯一，且 IF NOT EXISTS 只按名字判断 —— 所以 :329 执行时
-- 看到同名索引（其实挂在 duration_snapshot 上）就直接跳过，**wave_snapshot 至今没有
-- (anchor_id, biz_date) 唯一约束**。
--
-- 后果（两条都是实打实的）：
--  1. 任何 ON CONFLICT (anchor_id, biz_date) 的写入都报
--     "there is no unique or exclusion constraint matching the ON CONFLICT specification"
--     （.dbmerge/merge.py 灌库走 ON_ERROR_STOP=1，整轮灌库在此终止）。
--  2. 没有唯一约束 = 同一人同一天的音浪快照可重复插入，而 dialect.rs / minutes.rs /
--     push.rs 的聚合口径都 JOIN 这张表 → 音浪重复计入。
--
-- 本迁移用不撞车的新名字补建。旧的错名索引保留（它服务 duration_snapshot，是有效的），
-- 不要 DROP。
CREATE UNIQUE INDEX IF NOT EXISTS uk_wave_snapshot_anchor_date
  ON wave_snapshot (anchor_id, biz_date);
