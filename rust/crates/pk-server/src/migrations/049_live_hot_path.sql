-- 049 · 直播热路径加固（2026-11-12）
--
-- 1. 席位与采样保留分数版本：旧响应晚到时不覆盖更高版本的分数。
-- 2. 最新分查询按 (uid, version, observed_at, id) 覆盖，避免每拍 DISTINCT ON 后排序。
-- 3. observed_at 单列索引供保留期清理使用。
--
-- 兼容性：全部 ADD COLUMN IF NOT EXISTS / CREATE INDEX IF NOT EXISTS；
-- 历史字符串版本能安全转 BIGINT 的回填，异常值保持 -1，不影响现有读取。

ALTER TABLE live_round_seat
  ADD COLUMN IF NOT EXISTS score_version BIGINT NOT NULL DEFAULT -1;

ALTER TABLE live_score_sample
  ADD COLUMN IF NOT EXISTS score_version_num BIGINT NOT NULL DEFAULT -1;
ALTER TABLE live_room_score_sample
  ADD COLUMN IF NOT EXISTS score_version_num BIGINT NOT NULL DEFAULT -1;

UPDATE live_score_sample
   SET score_version_num = CASE
       WHEN score_version ~ '^[0-9]{1,19}$'
        AND score_version::NUMERIC <= 9223372036854775807
       THEN score_version::BIGINT
       ELSE -1
   END
 WHERE score_version_num = -1 AND score_version <> '';

UPDATE live_room_score_sample
   SET score_version_num = CASE
       WHEN score_version ~ '^[0-9]{1,19}$'
        AND score_version::NUMERIC <= 9223372036854775807
       THEN score_version::BIGINT
       ELSE -1
   END
 WHERE score_version_num = -1 AND score_version <> '';

CREATE INDEX IF NOT EXISTS idx_live_sample_latest_version
  ON live_score_sample (round_id, douyin_uid, score_version_num DESC, observed_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_live_room_sample_latest_version
  ON live_room_score_sample (web_rid, battle_id, douyin_uid, score_version_num DESC, observed_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_live_sample_observed_at
  ON live_score_sample (observed_at);
CREATE INDEX IF NOT EXISTS idx_live_room_sample_observed_at
  ON live_room_score_sample (observed_at);

-- 新索引完整覆盖旧查询；删掉旧索引，减少每拍写入的索引维护成本。
DROP INDEX IF EXISTS idx_live_sample_round;
DROP INDEX IF EXISTS idx_live_room_sample_latest;
