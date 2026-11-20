-- 057 · 赛事终态（2026-11-20）
-- 晋级赛每组只取第 1 名进入决赛，不存在复活池；决赛结束后赛事进入 finished。

ALTER TABLE live_event
  ADD COLUMN IF NOT EXISTS finished_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS idx_live_event_finished
  ON live_event (finished_at DESC)
  WHERE finished_at IS NOT NULL;
