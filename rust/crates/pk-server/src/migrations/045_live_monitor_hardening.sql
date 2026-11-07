-- 045 · 直播监控联调加固（2026-10-20）
--
-- 1. 采集 worker 归属到具体赛事：管理端能判断「这个赛事的采集器是否在线」，
--    而不是只看一个全局心跳。
-- 2. 小组按 (stage_id, seq) 幂等：重复点击「生成小组」只更新配置，不再复制小组。
-- 3. 赛前名单允许没有抖音 UID 的人先占位：名单来自分组方案，主播没登记采集号时
--    仍要能看到「这组有谁」；有 UID 时仍保持一人一行。

ALTER TABLE live_collector_worker
  ADD COLUMN IF NOT EXISTS event_id BIGINT NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_live_worker_event
  ON live_collector_worker (event_id, last_heartbeat DESC);

CREATE UNIQUE INDEX IF NOT EXISTS uk_live_group_stage_seq
  ON live_group (stage_id, seq);

DROP INDEX IF EXISTS uk_live_group_seat;
CREATE UNIQUE INDEX IF NOT EXISTS uk_live_group_seat_uid
  ON live_group_seat (group_id, douyin_uid)
  WHERE douyin_uid <> '';

-- 没有登记抖音号的主播也要能按 person_id 幂等占位；同名无档案的极端情况
-- 再由昵称兜底，避免重复点击「生成小组」把同一名单复制多份。
CREATE UNIQUE INDEX IF NOT EXISTS uk_live_group_seat_person
  ON live_group_seat (group_id, person_id)
  WHERE person_id > 0;

CREATE UNIQUE INDEX IF NOT EXISTS uk_live_group_seat_nickname
  ON live_group_seat (group_id, nickname)
  WHERE person_id = 0 AND douyin_uid = '' AND nickname <> '';
