-- 056 · 比赛成绩冻结与历史查询（2026-11-20）
--
-- 现场分数采样 live_score_sample 只保留 90 天，live_round_seat 在正式局确定后
-- 仍可能继续接收最终采样。历史成绩需要一份独立、可长期读取的完整结果快照。
--
-- 该表只属于比赛成绩域，不写 daily_metric / monthly_metric，也不参与音浪、
-- 时长、收益或日/月/年榜计算。

CREATE TABLE IF NOT EXISTS live_group_result (
  group_id BIGINT NOT NULL,
  event_id BIGINT NOT NULL,
  stage_id BIGINT NOT NULL,
  round_id BIGINT NOT NULL,
  person_id BIGINT NOT NULL DEFAULT 0,
  douyin_uid VARCHAR(40) NOT NULL DEFAULT '',
  nickname VARCHAR(128) NOT NULL DEFAULT '',
  person_name_snapshot VARCHAR(64) NOT NULL DEFAULT '',
  match_score BIGINT NOT NULL DEFAULT 0,
  match_rank INTEGER NOT NULL DEFAULT 0,
  match_position INTEGER NOT NULL DEFAULT 0,
  qualified SMALLINT NOT NULL DEFAULT 0,
  qualifier_source VARCHAR(24) NOT NULL DEFAULT '',
  frozen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (group_id, douyin_uid)
);

CREATE INDEX IF NOT EXISTS idx_live_group_result_event
  ON live_group_result (event_id, stage_id, group_id);

CREATE INDEX IF NOT EXISTS idx_live_group_result_person
  ON live_group_result (person_id, event_id, group_id);

-- 迁移时把当前已经定稿的正式局复制成不可变结果快照。
-- 旧数据没有单独的“成绩冻结时刻”，先以小组 finalized_at 作为冻结时间；
-- 后续新的定稿流程会在 freeze_group_qualifiers 后写入同一张表。
INSERT INTO live_group_result (
  group_id, event_id, stage_id, round_id, person_id, douyin_uid,
  nickname, person_name_snapshot, match_score, match_rank,
  match_position, qualified, qualifier_source, frozen_at
)
SELECT
  g.id,
  g.event_id,
  g.stage_id,
  r.id,
  s.person_id,
  s.douyin_uid,
  s.nickname,
  COALESCE(p.name, ''),
  s.final_score,
  s.final_rank,
  ROW_NUMBER() OVER (
    PARTITION BY r.id
    ORDER BY
      CASE WHEN s.final_rank > 0 THEN 0 ELSE 1 END,
      s.final_rank,
      s.final_score DESC,
      s.douyin_uid
  )::INTEGER,
  s.qualified,
  COALESCE(q.source, ''),
  COALESCE(g.finalized_at, r.actual_end_at, r.updated_at, now())
FROM live_group g
JOIN live_round r
  ON r.id = g.official_round_id
 AND r.is_official = 1
 AND r.status = 'completed'
JOIN live_round_seat s ON s.round_id = r.id
LEFT JOIN person p ON p.id = s.person_id
LEFT JOIN LATERAL (
  SELECT source
    FROM live_qualifier
   WHERE round_id = r.id
     AND douyin_uid = s.douyin_uid
   ORDER BY id DESC
   LIMIT 1
) q ON true
WHERE g.status = 'finalized'
  AND g.official_round_id > 0
ON CONFLICT (group_id, douyin_uid) DO NOTHING;
