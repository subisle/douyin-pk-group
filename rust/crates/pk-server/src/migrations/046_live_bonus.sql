-- 046 · 直播 PK 加成监控（2026-11-08）
--
-- 抖音 linkmic/list 的 battle_easter_egg_info 会给出：
--   addition_multiple = 150      → 1.5 倍加成
--   addition_score_map[uid]       → 已计入显示分的累计加成增量
--   period_list.stage=4.start_time_sec/duration_sec → 加成生效窗口
--
-- 采样表保留原始显示分和加成快照；管理端展示“显示分”和
-- “去加成原分 = score - bonus_score”，不改写最终结算分。

ALTER TABLE live_score_sample
  ADD COLUMN IF NOT EXISTS bonus_multiple INTEGER NOT NULL DEFAULT 100;
ALTER TABLE live_score_sample
  ADD COLUMN IF NOT EXISTS bonus_score BIGINT NOT NULL DEFAULT 0;
ALTER TABLE live_score_sample
  ADD COLUMN IF NOT EXISTS bonus_active SMALLINT NOT NULL DEFAULT 0;
ALTER TABLE live_score_sample
  ADD COLUMN IF NOT EXISTS bonus_end_ms BIGINT NOT NULL DEFAULT 0;
ALTER TABLE live_score_sample
  ADD COLUMN IF NOT EXISTS current_bonus BIGINT NOT NULL DEFAULT 0;
