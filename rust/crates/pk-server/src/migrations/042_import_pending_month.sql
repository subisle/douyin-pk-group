-- 042 · 月度导入未匹配行暂存区（2026-10-18 用户口径）
--
-- 用户原话：「之前有数据但是没有绑定，后续如果绑定，数据要同步过来」。
-- 按日导入早就有这条链路（import_pending → absorb_pending → daily_metric），
-- 但**月度**导入的未匹配行一直是直接丢弃的：月报先导、事后才建档 / 绑号，
-- 那个月的整月时长 / 音浪就再也回不来了，只能重导文件。
--
-- 为什么不复用 import_pending：那张表按「某一天」（biz_date）补账，
-- 整月值摊到某一天会失真。月度值有独立的业务日期口径（period YYYY-MM），
-- 且只承载 wave / minutes 两种月度指标，所以单开一张同构表。
--
-- 形状对齐 import_pending（001_core），只是 biz_date → period char(7)，
-- 并去掉日口径专有的 cumulative_wave / is_live（月度值直接给总量，没有快照差分）。
--
-- 幂等：唯一键 (douyin_no, period) —— 重导同月同号按最新值覆盖，不翻倍。
-- 「已补账」的行靠 absorbed_at IS NULL 排除，重复 absorb 不会二次叠加。
CREATE TABLE IF NOT EXISTS import_pending_month (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  name VARCHAR(128) NOT NULL DEFAULT '',
  douyin_no VARCHAR(64) NOT NULL,
  period char(7) NOT NULL,
  wave bigint,
  minutes int,
  source_batch_id BIGINT,
  absorbed_person_id BIGINT,
  absorbed_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_import_pending_month_no_period
  ON import_pending_month (douyin_no, period);
CREATE INDEX IF NOT EXISTS idx_import_pending_month_open
  ON import_pending_month (absorbed_at, period);
CREATE INDEX IF NOT EXISTS idx_import_pending_month_absorbed
  ON import_pending_month (absorbed_person_id);

DROP TRIGGER IF EXISTS trg_touch_import_pending_month ON import_pending_month;
CREATE TRIGGER trg_touch_import_pending_month BEFORE UPDATE ON import_pending_month FOR EACH ROW EXECUTE FUNCTION touch_updated_at();
