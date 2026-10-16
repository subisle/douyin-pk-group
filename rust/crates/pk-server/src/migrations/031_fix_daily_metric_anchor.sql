-- 031: 把 daily_metric.anchor_id 对齐到**当前主号**（存量回扫，一次性的）
--
-- 背景（2026-10-17 审计 P1-4）：分号口径（「我的抖音号」卡 / 推送分号明细段）的主号分支
-- 要求 `account.anchor_id = daily_metric.anchor_id AND account.is_primary = 1`；
-- 而 daily_metric.anchor_id 是**写库那一刻**的主号，换主号后旧行不会跟着变
-- （030 之前 write_daily_row 的 ON CONFLICT 也不写这一列）。
-- 结果：该主播在分号卡上主号=0、副号=0（老号已降副号走 wave_snapshot），历史音浪整段消失。
--
-- 这里把「anchor_id 与当前主号不一致」的历史行刷成当前主号。
-- 只动**当前仍有主号**的人（没有主号的行保持原样，避免抹成空串）。
-- 幂等：刷完就不再命中 WHERE，重跑无副作用。
UPDATE daily_metric d
   SET anchor_id = a.anchor_id
  FROM account a
 WHERE a.person_id = d.person_id
   AND a.is_primary = 1
   AND d.anchor_id IS DISTINCT FROM a.anchor_id;
