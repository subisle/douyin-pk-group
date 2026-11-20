-- 回滚 2026-11-20 中级争霸赛的本地运行态修复。
-- 只撤销 2026-11-20-mid-results.sql 写入的结果，不动原始 live_score_sample。

\set ON_ERROR_STOP on
BEGIN;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM live_event
     WHERE id = 25
       AND event_date = '2026-11-20'
       AND title = '2026-11-20 中级争霸赛 · 实时比分'
  ) THEN
    RAISE EXCEPTION '拒绝回滚：event_id=25 不是预期的 2026-11-20 中级争霸赛';
  END IF;
END $$;

-- 第1组恢复采集时的结果
WITH original(douyin_uid, final_score, final_rank, qualified) AS (
  VALUES
    ('7551155250661671993', 103716::bigint, 1, 1::smallint),
    ('1892679018365079',     22635::bigint, 2, 1::smallint),
    ('3529893223600588',     16728::bigint, 3, 1::smallint),
    ('7614069926093014065',   7504::bigint, 4, 1::smallint),
    ('2693166103017664',      5127::bigint, 5, 0::smallint),
    ('7645162221924877361',   5041::bigint, 6, 0::smallint),
    ('7629720333395084337',   1015::bigint, 7, 0::smallint)
)
UPDATE live_round_seat s
   SET final_score = original.final_score,
       final_rank = original.final_rank,
       qualified = original.qualified
  FROM original
 WHERE s.round_id = 678
   AND s.douyin_uid = original.douyin_uid;

DELETE FROM live_qualifier
 WHERE event_id = 25
   AND stage_id = 32
   AND group_id = 77
   AND source = 'direct';

INSERT INTO live_qualifier (
  event_id, stage_id, group_id, round_id, person_id, douyin_uid, source, rank, score
)
SELECT g.event_id, g.stage_id, g.id, s.round_id, s.person_id, s.douyin_uid,
       'direct', s.final_rank, s.final_score
  FROM live_group g
  JOIN live_round_seat s ON s.round_id = g.official_round_id
 WHERE g.id = 77
   AND s.final_rank BETWEEN 1 AND 4;

-- 第4组恢复采集时的第7/8名
UPDATE live_round_seat
   SET final_score = 336, final_rank = 8, qualified = 0
 WHERE round_id = 819 AND douyin_uid = '7628815085930103867';
UPDATE live_round_seat
   SET final_score = 362, final_rank = 7, qualified = 0
 WHERE round_id = 819 AND douyin_uid = '23526207133124';

-- 第1轮复活2组恢复未冻结
UPDATE live_round_seat SET qualified = 0 WHERE round_id = 1136;
DELETE FROM live_qualifier
 WHERE event_id = 25 AND stage_id = 34 AND group_id = 85 AND source = 'revival';

-- 84/85 原本没有赛前名单，回滚时清掉反填
DELETE FROM live_group_seat WHERE group_id IN (84, 85);

-- 第2轮复活赛：删除补录结果，恢复 pending / 4 个空位
DELETE FROM live_qualifier
 WHERE event_id = 25 AND stage_id = 35 AND group_id IN (86, 87) AND source = 'revival';

DELETE FROM live_round_event
 WHERE round_id IN (
   SELECT id FROM live_round
    WHERE battle_id IN ('manual-20261120-rest-revival-1', 'manual-20261120-rest-revival-2')
 );

DELETE FROM live_round_seat
 WHERE round_id IN (
   SELECT id FROM live_round
    WHERE battle_id IN ('manual-20261120-rest-revival-1', 'manual-20261120-rest-revival-2')
 );

DELETE FROM live_round
 WHERE battle_id IN ('manual-20261120-rest-revival-1', 'manual-20261120-rest-revival-2');

DELETE FROM live_group_seat WHERE group_id IN (86, 87);

UPDATE live_group
   SET status = 'pending',
       qualify_slots = 4,
       official_round_id = 0,
       finalized_at = NULL
 WHERE id IN (86, 87);

UPDATE live_stage
   SET status = 'pending',
       started_at = NULL,
       finished_at = NULL
 WHERE id = 35 AND event_id = 25;

-- 删除本次修复写入的事件记录
DELETE FROM live_round_event
 WHERE round_id = 678
   AND kind = 'manual_correction'
   AND detail LIKE '按文字版最终成绩修正第1组%';
DELETE FROM live_round_event
 WHERE round_id = 819
   AND kind = 'manual_correction'
   AND detail LIKE '按文字版最终成绩修正第4组%';
DELETE FROM live_round_event
 WHERE round_id = 1136
   AND kind = 'manual_correction'
   AND detail LIKE '按文字版赛果冻结第1轮复活2组前4%';

COMMIT;
