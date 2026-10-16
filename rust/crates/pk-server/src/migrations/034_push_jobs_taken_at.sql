-- 034: 推送队列 stale 抢占修复（2026-10-17 审计 6-P2-2，承第二轮 P2-4）。
--
-- 背景：stale 判定原先用 created_at（入队时间，通常 = run_at - 30 分钟）当
-- 「进入 running 的时刻」——任务一旦 running，`created_at < now() - 5min` 恒真，
-- 第二个调度循环 60 秒后必把长任务（150 人图片推送、重试场景很常见）当成死任务
-- 重抢 → 多实例 / 调度循环并发时同一主播重复收推送。CAS 的同值 UPDATE
-- （status='running' → 'running'）在 PG 里 rows_affected 仍为 1，抢不占不住。
--
-- 修法：加 taken_at 列（进入 running 的时刻），stale 判定改看 taken_at；
-- 抢占 CAS 顺带刷新 taken_at。status 编码口径（running:N）不变。

ALTER TABLE push_jobs ADD COLUMN IF NOT EXISTS taken_at VARCHAR(40) NOT NULL DEFAULT '';
