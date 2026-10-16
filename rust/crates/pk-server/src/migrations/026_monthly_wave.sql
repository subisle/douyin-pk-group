-- 026: 月度音浪导入（2026-10-16 用户口径「时长或者音浪都支持按月进行导入和显示」）。
--
-- 平台月报里的「总音浪」是整月口径：一人一月一行，有导入就以它为准
-- （与 monthly_duration 的月度时长同构）。查询端统一走
-- COALESCE(导入月值, daily_metric 日明细求和)：某月没导过整月值才回落日求和。
--
-- 为什么不复用 monthly_metric：那张表是 metrics.rs 用日明细派生的缓存
-- （只写不读，ON CONFLICT 直接覆盖 wave），导入值会被下一次聚合冲掉。
CREATE TABLE IF NOT EXISTS monthly_wave (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  person_id BIGINT NOT NULL,
  period char(7) NOT NULL,
  wave bigint NOT NULL DEFAULT 0,
  source_batch_id BIGINT,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_monthly_wave_period ON monthly_wave (period);
CREATE UNIQUE INDEX IF NOT EXISTS uk_monthly_wave_person_period ON monthly_wave (person_id, period);
DROP TRIGGER IF EXISTS trg_touch_monthly_wave ON monthly_wave;
CREATE TRIGGER trg_touch_monthly_wave BEFORE UPDATE ON monthly_wave FOR EACH ROW EXECUTE FUNCTION touch_updated_at();
