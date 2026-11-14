-- 导入锁定状态（2026-11-15）
-- 0 = 尚未导入，1 = 已导入但允许后续覆盖，2 = 已锁定不可覆盖。
-- 历史数据保守回填为 1，避免升级后突然把旧数据全部冻住；新的 locked 导入再显式升为 2。
ALTER TABLE daily_metric
  ADD COLUMN IF NOT EXISTS wave_state SMALLINT NOT NULL DEFAULT 0,
  ADD COLUMN IF NOT EXISTS minutes_state SMALLINT NOT NULL DEFAULT 0;

ALTER TABLE monthly_duration
  ADD COLUMN IF NOT EXISTS state SMALLINT NOT NULL DEFAULT 0;

ALTER TABLE monthly_wave
  ADD COLUMN IF NOT EXISTS state SMALLINT NOT NULL DEFAULT 0;

ALTER TABLE import_pending
  ADD COLUMN IF NOT EXISTS wave_state SMALLINT NOT NULL DEFAULT 0,
  ADD COLUMN IF NOT EXISTS minutes_state SMALLINT NOT NULL DEFAULT 0;

ALTER TABLE import_pending_month
  ADD COLUMN IF NOT EXISTS wave_state SMALLINT NOT NULL DEFAULT 0,
  ADD COLUMN IF NOT EXISTS minutes_state SMALLINT NOT NULL DEFAULT 0;

UPDATE daily_metric
   SET wave_state = 1,
       minutes_state = 1
 WHERE wave_state = 0
   AND minutes_state = 0;

UPDATE monthly_duration SET state = 1 WHERE state = 0;
UPDATE monthly_wave SET state = 1 WHERE state = 0;
