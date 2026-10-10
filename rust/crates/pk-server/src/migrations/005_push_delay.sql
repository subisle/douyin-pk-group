-- 005_push_delay · 延迟推送任务 + 追分目标扩类（2026-10-10）
-- 三处改动：
--   1. push_jobs —— 数据入库后不立刻推送，排队到「N 分钟后」再推（默认 30 分钟）。
--                   运营一次导入往往分好几个文件（男团/女团/多抖音号），立刻推会把
--                   半成品榜单推给用户；延迟一档等这轮数据到齐再推。
--                   同一 (team, biz_date) 只排一队（唯一键），重复导入只刷新不重复推。
--   2. my_targets —— kind 增加 'rank'（追名次）：target_value 存目标名次，
--                    target_name 存展示文案（如「第 3 名」）。列结构不变，无需 DDL。
--   3. push_bindings.last_report_date —— 每日报告按业务日去重：
--      延迟队列只保证同一 (团, 日) 排一队，运营隔几小时补一轮数据还会再排一次；
--      记下上次推过的业务日，同一天不再重复推同一份日报。

CREATE TABLE IF NOT EXISTS push_jobs (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  team VARCHAR(16) NOT NULL DEFAULT '',
  biz_date VARCHAR(16) NOT NULL DEFAULT '',
  run_at VARCHAR(40) NOT NULL DEFAULT '',
  status VARCHAR(16) NOT NULL DEFAULT 'pending',
  created_at VARCHAR(40) NOT NULL DEFAULT '',
  done_at VARCHAR(40) NOT NULL DEFAULT '',
  note VARCHAR(255) NOT NULL DEFAULT '',
  PRIMARY KEY (id),
  UNIQUE KEY uk_push_job (team, biz_date)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

ALTER TABLE push_bindings
  ADD COLUMN last_report_date VARCHAR(16) NOT NULL DEFAULT '';
