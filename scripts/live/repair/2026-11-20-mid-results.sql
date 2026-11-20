-- 2026-11-20 中级争霸赛：按运营确认的文字版成绩修复本地直播运行态。
--
-- 适用范围：
--   event_id = 25
--   title    = 2026-11-20 中级争霸赛 · 实时比分
--
-- 本脚本只修运行态结果，不改 live_score_sample（原始采集时间线保留）：
--   1. 第1组最终分与晋级结果（狼九第4，啸辰第6）；
--   2. 第4组玖妹最终分 3336，排名与浩沐互换；
--   3. 第1轮复活2组前4晋级落库；
--   4. 第2轮复活赛两组按 6进3 补录成绩与晋级结果。
--
-- 执行：
--   PGPASSWORD=... psql -h 127.0.0.1 -p 3318 -U pk_writer -d douyin_dev \
--     -v ON_ERROR_STOP=1 -f scripts/live/repair/2026-11-20-mid-results.sql
--
-- 回滚：
--   scripts/live/repair/2026-11-20-mid-results.rollback.sql

\set ON_ERROR_STOP on
BEGIN;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1
      FROM live_event
     WHERE id = 25
       AND event_date = '2026-11-20'
       AND title = '2026-11-20 中级争霸赛 · 实时比分'
  ) THEN
    RAISE EXCEPTION '拒绝执行：event_id=25 不是预期的 2026-11-20 中级争霸赛';
  END IF;

  IF NOT EXISTS (SELECT 1 FROM live_group WHERE id = 77 AND event_id = 25 AND name = '第 1 组')
     OR NOT EXISTS (SELECT 1 FROM live_group WHERE id = 80 AND event_id = 25 AND name = '第 4 组')
     OR NOT EXISTS (SELECT 1 FROM live_group WHERE id = 85 AND event_id = 25 AND name = '复活2组')
     OR NOT EXISTS (SELECT 1 FROM live_group WHERE id = 86 AND event_id = 25 AND name = '复活1组')
     OR NOT EXISTS (SELECT 1 FROM live_group WHERE id = 87 AND event_id = 25 AND name = '复活2组') THEN
    RAISE EXCEPTION '拒绝执行：本地小组 id 与预期不一致';
  END IF;
END $$;

-- ---------------------------------------------------------------------------
-- 1. 第1组：修正最终分、名次、晋级标记
-- ---------------------------------------------------------------------------
WITH repaired(douyin_uid, final_score, final_rank) AS (
  VALUES
    ('7551155250661671993', 113716::bigint, 1),
    ('1892679018365079',    27283::bigint, 2),
    ('3529893223600588',    16728::bigint, 3),
    ('2693166103017664',     9025::bigint, 4),
    ('7645162221924877361',  7841::bigint, 5),
    ('7614069926093014065',  7504::bigint, 6),
    ('7629720333395084337',  1015::bigint, 7)
)
UPDATE live_round_seat s
   SET final_score = repaired.final_score,
       final_rank = repaired.final_rank,
       qualified = CASE WHEN repaired.final_rank <= 4 THEN 1 ELSE 0 END
  FROM repaired
 WHERE s.round_id = 678
   AND s.douyin_uid = repaired.douyin_uid;

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
   AND s.qualified = 1
 ORDER BY s.final_rank;

INSERT INTO live_round_event (round_id, kind, detail, actor, at)
SELECT 678, 'manual_correction',
       '按文字版最终成绩修正第1组分数与晋级：狼明113716、玖玥27283、狼九9025；狼九第4、啸辰第6',
       'admin', now()
 WHERE NOT EXISTS (
   SELECT 1 FROM live_round_event
    WHERE round_id = 678
      AND kind = 'manual_correction'
      AND detail LIKE '按文字版最终成绩修正第1组%'
 );

-- ---------------------------------------------------------------------------
-- 2. 第4组：玖妹 336 -> 3336，并恢复第7/8名
-- ---------------------------------------------------------------------------
UPDATE live_round_seat
   SET final_score = 3336, final_rank = 7, qualified = 0
 WHERE round_id = 819
   AND douyin_uid = '7628815085930103867'; -- 玖妹

UPDATE live_round_seat
   SET final_score = 362, final_rank = 8, qualified = 0
 WHERE round_id = 819
   AND douyin_uid = '23526207133124'; -- 浩沐

INSERT INTO live_round_event (round_id, kind, detail, actor, at)
SELECT 819, 'manual_correction',
       '按文字版最终成绩修正第4组：玖妹3336（第7），浩沐362（第8）',
       'admin', now()
 WHERE NOT EXISTS (
   SELECT 1 FROM live_round_event
    WHERE round_id = 819
      AND kind = 'manual_correction'
      AND detail LIKE '按文字版最终成绩修正第4组%'
 );

-- ---------------------------------------------------------------------------
-- 3. 第1轮复活2组：分数已正确，补冻结前4名
-- ---------------------------------------------------------------------------
UPDATE live_round_seat
   SET qualified = CASE WHEN final_rank BETWEEN 1 AND 4 THEN 1 ELSE 0 END
 WHERE round_id = 1136;

DELETE FROM live_qualifier
 WHERE event_id = 25
   AND stage_id = 34
   AND group_id = 85
   AND source = 'revival';

INSERT INTO live_qualifier (
  event_id, stage_id, group_id, round_id, person_id, douyin_uid, source, rank, score
)
SELECT g.event_id, g.stage_id, g.id, s.round_id, s.person_id, s.douyin_uid,
       'revival', s.final_rank, s.final_score
  FROM live_group g
  JOIN live_round_seat s ON s.round_id = g.official_round_id
 WHERE g.id = 85
   AND s.final_rank BETWEEN 1 AND 4;

INSERT INTO live_round_event (round_id, kind, detail, actor, at)
SELECT 1136, 'manual_correction',
       '按文字版赛果冻结第1轮复活2组前4：狼俊、浩阳、啸辰、浩森',
       'admin', now()
 WHERE NOT EXISTS (
   SELECT 1 FROM live_round_event
    WHERE round_id = 1136
      AND kind = 'manual_correction'
      AND detail LIKE '按文字版赛果冻结第1轮复活2组前4%'
 );

-- ---------------------------------------------------------------------------
-- 3.1 复活组回填分组/连麦名单
--
-- 复活组的实际名单来自直播连麦顺序，最终结果落在 live_round_seat；
-- 管理端“赛前名单”读的是 live_group_seat。历史采集没有回填这里，
-- 会导致复活组已经出分但页面仍显示“等待分组/连麦”。
-- ---------------------------------------------------------------------------
DELETE FROM live_group_seat WHERE group_id IN (84, 85);

INSERT INTO live_group_seat (group_id, person_id, douyin_uid, nickname, seed_order)
SELECT g.id, s.person_id, s.douyin_uid, s.nickname,
       (ROW_NUMBER() OVER (
          PARTITION BY g.id
          ORDER BY s.final_rank, s.final_score DESC, s.id
       ) - 1)::integer
  FROM live_group g
  JOIN live_round r ON r.group_id = g.id AND r.is_official = 1
  JOIN live_round_seat s ON s.round_id = r.id
 WHERE g.id IN (84, 85);

-- ---------------------------------------------------------------------------
-- 4. 第2轮复活赛：改 6进3，补名单、正式局、最终分和晋级结果
-- ---------------------------------------------------------------------------
UPDATE live_group
   SET qualify_slots = 3,
       status = 'finalized',
       finalized_at = COALESCE(finalized_at, now())
 WHERE id IN (86, 87);

DELETE FROM live_group_seat WHERE group_id IN (86, 87);

INSERT INTO live_group_seat (group_id, person_id, douyin_uid, nickname, seed_order)
SELECT v.group_id, v.person_id, v.douyin_uid,
       COALESCE(
         (SELECT rs.nickname
            FROM live_round_seat rs
           WHERE rs.person_id = v.person_id
           ORDER BY rs.id DESC
           LIMIT 1),
         p.name,
         ''
       ),
       v.seed_order
  FROM (VALUES
    -- 第2轮复活1组（6进3）
    (86::bigint, 121::bigint, '4469946317288075'::varchar, 0),
    (86::bigint, 142::bigint, '7629653493028701233'::varchar, 1),
    (86::bigint,  28::bigint, '1883920098401323'::varchar, 2),
    (86::bigint,   5::bigint, '7477876614760825916'::varchar, 3),
    (86::bigint, 145::bigint, '3562012945417192'::varchar, 4),
    (86::bigint, 1000074::bigint, '4337986464397978'::varchar, 5),
    -- 第2轮复活2组（6进3）
    (87::bigint,  22::bigint, '1001031913192032'::varchar, 0),
    (87::bigint,  86::bigint, '1554036048015532'::varchar, 1),
    (87::bigint, 140::bigint, '7629948514345485361'::varchar, 2),
    (87::bigint, 1024::bigint, '7663779763521504315'::varchar, 3),
    (87::bigint, 1029::bigint, '7673819516355118129'::varchar, 4),
    (87::bigint, 188::bigint, '7639946422502278193'::varchar, 5)
  ) AS v(group_id, person_id, douyin_uid, seed_order)
  LEFT JOIN person p ON p.id = v.person_id;

INSERT INTO live_round (
  event_id, group_id, battle_id, channel_id, room_web_rid, round_kind,
  attempt_no, parent_round_id, roster_signature, start_at, expected_end_at,
  actual_end_at, end_reason, status, is_official
)
VALUES
  (
    25, 86, 'manual-20261120-rest-revival-1', '', '', 'normal',
    1, 0,
    '4469946317288075,7629653493028701233,1883920098401323,7477876614760825916,3562012945417192,4337986464397978',
    NULL, NULL, NULL, 'manual', 'completed', 1
  ),
  (
    25, 87, 'manual-20261120-rest-revival-2', '', '', 'normal',
    1, 0,
    '1001031913192032,1554036048015532,7629948514345485361,7663779763521504315,7673819516355118129,7639946422502278193',
    NULL, NULL, NULL, 'manual', 'completed', 1
  )
ON CONFLICT (battle_id, group_id) DO UPDATE
   SET status = 'completed',
       end_reason = 'manual',
       is_official = 1,
       updated_at = now();

UPDATE live_group g
   SET official_round_id = r.id,
       status = 'finalized',
       qualify_slots = 3,
       finalized_at = COALESCE(g.finalized_at, now())
  FROM live_round r
 WHERE r.event_id = 25
   AND r.group_id = g.id
   AND r.battle_id IN ('manual-20261120-rest-revival-1', 'manual-20261120-rest-revival-2');

WITH inputs(group_id, person_id, douyin_uid, final_score, final_rank) AS (
  VALUES
    -- 第2轮复活1组（6进3）
    (86::bigint, 121::bigint, '4469946317288075'::varchar, 21203::bigint, 1),
    (86::bigint, 142::bigint, '7629653493028701233'::varchar, 10162::bigint, 2),
    (86::bigint,  28::bigint, '1883920098401323'::varchar,  6332::bigint, 3),
    (86::bigint,   5::bigint, '7477876614760825916'::varchar,  6141::bigint, 4),
    (86::bigint, 145::bigint, '3562012945417192'::varchar,  2798::bigint, 5),
    (86::bigint, 1000074::bigint, '4337986464397978'::varchar,   235::bigint, 6),
    -- 第2轮复活2组（6进3）
    (87::bigint,  22::bigint, '1001031913192032'::varchar,  5427::bigint, 1),
    (87::bigint,  86::bigint, '1554036048015532'::varchar,  4984::bigint, 2),
    (87::bigint, 140::bigint, '7629948514345485361'::varchar,  4895::bigint, 3),
    (87::bigint, 1024::bigint, '7663779763521504315'::varchar,  3351::bigint, 4),
    (87::bigint, 1029::bigint, '7673819516355118129'::varchar,  1067::bigint, 5),
    (87::bigint, 188::bigint, '7639946422502278193'::varchar,     6::bigint, 6)
)
INSERT INTO live_round_seat (
  round_id, person_id, douyin_uid, nickname, team_id, final_score, final_rank,
  qualified, score_version
)
SELECT r.id, i.person_id, i.douyin_uid,
       COALESCE(
         (SELECT rs.nickname
            FROM live_round_seat rs
           WHERE rs.person_id = i.person_id
           ORDER BY rs.id DESC
           LIMIT 1),
         p.name,
         ''
       ),
       '', i.final_score, i.final_rank,
       CASE WHEN i.final_rank <= 3 THEN 1 ELSE 0 END,
       -1
  FROM inputs i
  JOIN live_group g ON g.id = i.group_id
  JOIN live_round r ON r.group_id = i.group_id
                   AND r.battle_id = CASE i.group_id
                                      WHEN 86 THEN 'manual-20261120-rest-revival-1'
                                      ELSE 'manual-20261120-rest-revival-2'
                                    END
  LEFT JOIN person p ON p.id = i.person_id
ON CONFLICT (round_id, douyin_uid) DO UPDATE
   SET person_id = EXCLUDED.person_id,
       nickname = EXCLUDED.nickname,
       final_score = EXCLUDED.final_score,
       final_rank = EXCLUDED.final_rank,
       qualified = EXCLUDED.qualified;

DELETE FROM live_qualifier
 WHERE event_id = 25
   AND stage_id = 35
   AND group_id IN (86, 87)
   AND source = 'revival';

INSERT INTO live_qualifier (
  event_id, stage_id, group_id, round_id, person_id, douyin_uid, source, rank, score
)
SELECT g.event_id, g.stage_id, g.id, s.round_id, s.person_id, s.douyin_uid,
       'revival', s.final_rank, s.final_score
  FROM live_group g
  JOIN live_round_seat s ON s.round_id = g.official_round_id
 WHERE g.id IN (86, 87)
   AND s.final_rank BETWEEN 1 AND 3;

UPDATE live_stage
   SET status = 'finalized',
       finished_at = COALESCE(finished_at, now())
 WHERE id = 35
   AND event_id = 25;

INSERT INTO live_round_event (round_id, kind, detail, actor, at)
SELECT r.id, 'manual_import',
       '按文字版完整赛果补录第2轮复活赛：6进3，最终成绩已冻结',
       'admin', now()
  FROM live_round r
 WHERE r.battle_id IN ('manual-20261120-rest-revival-1', 'manual-20261120-rest-revival-2')
   AND NOT EXISTS (
     SELECT 1 FROM live_round_event e
      WHERE e.round_id = r.id
        AND e.kind = 'manual_import'
   );

-- ---------------------------------------------------------------------------
-- 5. 断言：修复后必须与文字版汇总完全一致
-- ---------------------------------------------------------------------------
DO $$
DECLARE
  v_group1 bigint;
  v_group4 bigint;
  v_rest_revival bigint;
  v_total bigint;
  v_q42 integer;
BEGIN
  SELECT SUM(final_score) INTO v_group1
    FROM live_round_seat WHERE round_id = 678;
  SELECT SUM(final_score) INTO v_group4
    FROM live_round_seat WHERE round_id = 819;
  SELECT COALESCE(SUM(s.final_score), 0) INTO v_rest_revival
    FROM live_round r
    JOIN live_round_seat s ON s.round_id = r.id
   WHERE r.group_id IN (86, 87);

  SELECT COALESCE(SUM(s.final_score), 0) INTO v_total
    FROM live_round r
    JOIN live_group g ON g.id = r.group_id
    JOIN live_round_seat s ON s.round_id = r.id
   WHERE g.event_id = 25
     AND r.is_official = 1;

  SELECT COUNT(*) INTO v_q42
    FROM live_qualifier
   WHERE event_id = 25
     AND source IN ('direct', 'revival');

  IF (SELECT COUNT(*) FROM live_group_seat WHERE group_id = 84) <> 7
     OR (SELECT COUNT(*) FROM live_group_seat WHERE group_id = 85) <> 8 THEN
    RAISE EXCEPTION '第一轮复活赛名单回填数量错误';
  END IF;

  IF v_group1 <> 183112 THEN
    RAISE EXCEPTION '第1组合计错误：%，应为 183112', v_group1;
  END IF;
  IF v_group4 <> 154276 THEN
    RAISE EXCEPTION '第4组合计错误：%，应为 154276', v_group4;
  END IF;
  IF v_rest_revival <> 66601 THEN
    RAISE EXCEPTION '第2轮复活赛合计错误：%，应为 66601', v_rest_revival;
  END IF;
  IF v_total <> 1266987 THEN
    RAISE EXCEPTION '赛事总音浪错误：%，应为 1266987', v_total;
  END IF;
  IF v_q42 <> 42 THEN
    RAISE EXCEPTION '晋级人数错误：%，应为 42', v_q42;
  END IF;
END $$;

COMMIT;
