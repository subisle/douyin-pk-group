-- 020 · 修复 001 内索引名撞车导致的静默缺索引（2026-10-13 审计）
-- 001 曾把 idx_person（account/import_pending）与 idx_period（monthly_duration/monthly_metric）
-- 重名，PG 的 IF NOT EXISTS 只认名字 → 后建表的索引从未建出。
-- 幂等补建（新装库 001 已改名，本迁移只服务已灌库的老库）。
CREATE INDEX IF NOT EXISTS idx_import_pending_absorbed ON import_pending (absorbed_person_id);
CREATE INDEX IF NOT EXISTS idx_monthly_duration_period ON monthly_duration (period);
CREATE INDEX IF NOT EXISTS idx_monthly_metric_period ON monthly_metric (period);
