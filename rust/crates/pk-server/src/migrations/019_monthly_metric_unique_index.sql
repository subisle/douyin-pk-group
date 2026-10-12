-- 019: 补建 monthly_metric 唯一索引（001 里原名 uk_person_period 与
-- monthly_duration 撞名，IF NOT EXISTS 只认名字 → 本索引从未真正建出，
-- monthly_metric 的 ON CONFLICT 落库一直报 no unique constraint）。
-- 2026-10-12 数据库审计 P0-2b：001 已改名，本迁移对存量库幂等补建。
CREATE UNIQUE INDEX IF NOT EXISTS uk_monthly_metric_person_period
  ON monthly_metric (person_id, period);
