-- 051 · 未认领导入数据全部暂存（2026-11-12）
--
-- 用户口径：没匹配到台账的数据也要存下来，等待后续建号 / 认领再补账。
-- 旧链路只暂存带抖音号 / 采集 ID 的行；只有艺名的行直接失败丢弃。
-- 这里给两张暂存表加统一的 claim_key：
--   - 有抖音号 / 采集 ID：claim_key = 该标识（原行为）
--   - 只有艺名：claim_key = 截断后的艺名（等待同名主播建号 / 认领）
--
-- 唯一键从 (douyin_no, biz_date/period) 改为 (claim_key, biz_date/period)，
-- 因此重导同一个无号昵称仍幂等，不会按日期堆出重复行。

ALTER TABLE import_pending
  ADD COLUMN IF NOT EXISTS claim_key VARCHAR(128) NOT NULL DEFAULT '';

UPDATE import_pending
   SET claim_key = CASE WHEN douyin_no <> '' THEN douyin_no ELSE name END
 WHERE claim_key = '';

DROP INDEX IF EXISTS uk_no_date;
CREATE UNIQUE INDEX IF NOT EXISTS uk_import_pending_claim_date
  ON import_pending (claim_key, biz_date)
  WHERE claim_key <> '';
CREATE INDEX IF NOT EXISTS idx_import_pending_claim_open
  ON import_pending (claim_key, biz_date)
  WHERE absorbed_at IS NULL AND claim_key <> '';

ALTER TABLE import_pending_month
  ADD COLUMN IF NOT EXISTS claim_key VARCHAR(128) NOT NULL DEFAULT '';

UPDATE import_pending_month
   SET claim_key = CASE WHEN douyin_no <> '' THEN douyin_no ELSE name END
 WHERE claim_key = '';

DROP INDEX IF EXISTS uk_import_pending_month_no_period;
CREATE UNIQUE INDEX IF NOT EXISTS uk_import_pending_month_claim_period
  ON import_pending_month (claim_key, period)
  WHERE claim_key <> '';
CREATE INDEX IF NOT EXISTS idx_import_pending_month_claim_open
  ON import_pending_month (claim_key, period)
  WHERE absorbed_at IS NULL AND claim_key <> '';
