-- 007_monthly_duration · 月度时长导入（2026-10-10 用户口径）
--
-- 背景：抖音公会后台只导得出「整月」开播有效时长（一人一行，如「75小时29分钟41秒」），
-- 每日文件里没有时长。所以时长不再随按日导入落 daily_metric.minutes，
-- 改为按月整批导入到这张表：一人一月一行，值是平台月报原值。
--
-- 展示口径（用户口径 2026-10-10）：月榜 / 个人月卡优先用这里的整月值；
-- 日榜与每日明细完全不动；某月没有整月记录时（历史月份）才回落到 daily_metric 求和。
-- 查询端不读这张表做写入，只有导入 / 删除会写。
--
-- 幂等：IF NOT EXISTS，重复执行安全。

CREATE TABLE IF NOT EXISTS monthly_duration (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  person_id BIGINT UNSIGNED NOT NULL,
  period CHAR(7) NOT NULL COMMENT '业务月份 YYYY-MM',
  minutes INT NOT NULL DEFAULT 0 COMMENT '整月开播有效时长（分钟，平台月报原值）',
  source_batch_id BIGINT UNSIGNED DEFAULT NULL COMMENT '来源导入批次 import_batch.id',
  updated_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3),
  PRIMARY KEY (id),
  UNIQUE KEY uk_person_period (person_id, period),
  KEY idx_period (period)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci
  COMMENT='月度时长导入（平台月报，一人一月一行）';
