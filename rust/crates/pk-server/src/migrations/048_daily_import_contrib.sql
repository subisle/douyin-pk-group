-- 048 · 按日导入批次贡献（2026-11-12）
--
-- 背景：同一业务日经常从机器人/管理端分多次发两份 CSV。旧逻辑直接按
-- (person_id, biz_date) upsert daily_metric，后一份文件会覆盖前一份；
-- 尤其后一份含 0 音浪旧号/别名行时，已入库的正数会被清零。
--
-- 新口径：每次 commit_import 把一个批次对每人每天的「贡献」写进这张表，
-- 再把同一天所有批次贡献按人求和，回写 daily_metric。重发同一批次不会翻倍，
-- 删除批次也会同步删除贡献。

CREATE TABLE IF NOT EXISTS daily_import_contrib (
  batch_id BIGINT NOT NULL,
  person_id BIGINT NOT NULL,
  biz_date DATE NOT NULL,
  wave BIGINT NOT NULL DEFAULT 0,
  minutes INTEGER NOT NULL DEFAULT 0,
  is_live SMALLINT NOT NULL DEFAULT 0,
  has_minutes SMALLINT NOT NULL DEFAULT 0,
  PRIMARY KEY (batch_id, person_id, biz_date)
);

ALTER TABLE import_batch
  ADD COLUMN IF NOT EXISTS source_fingerprint VARCHAR(64) NOT NULL DEFAULT '';

CREATE UNIQUE INDEX IF NOT EXISTS uk_import_batch_fingerprint
  ON import_batch (biz_date, source_fingerprint)
  WHERE source <> 'admin-import-month' AND source_fingerprint <> '';

CREATE INDEX IF NOT EXISTS idx_daily_import_contrib_date
  ON daily_import_contrib (biz_date);
