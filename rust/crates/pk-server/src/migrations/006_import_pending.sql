-- 006_import_pending · 导入暂存区（未建档主播的行）
-- 用户口径（2026-10-10）：「只要有抖音号或者 id 都导入，但是不能显示在表里和统计里，
-- 只有当主播页面添加了主播才显示。」
--
-- 实现：未匹配到台账、但行里有抖音号 / 采集 ID 的行落在这里 —— 既不在 person/account，
-- 也不在 daily_metric，所以任何榜单、统计、对外展示、缺数据报告都看不见它。
-- 运营在「主播台账」建档或绑定该抖音号后，由 absorb 把它们补账进 daily_metric 才显示。
--
-- 抖音号（douyin_no 列）在这个表里存的是「行里的认人标识」：抖音号优先，其次采集 ID。
-- 匹配用 account.douyin_no 或 account.anchor_id 都算命中（运营文件两种列都出现过）。
CREATE TABLE IF NOT EXISTS import_pending (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  name VARCHAR(128) NOT NULL DEFAULT '' COMMENT '文件里的原始名字',
  douyin_no VARCHAR(64) NOT NULL COMMENT '抖音号或采集 ID（认人用）',
  biz_date DATE NOT NULL COMMENT '业务日期',
  wave BIGINT DEFAULT NULL COMMENT '当日音浪（NULL = 文件里没给，不是 0）',
  cumulative_wave BIGINT DEFAULT NULL COMMENT '累计音浪（NULL = 没给）',
  minutes INT DEFAULT NULL COMMENT '直播分钟（NULL = 没给）',
  is_live TINYINT(1) DEFAULT NULL COMMENT '开播状态（NULL = 没给）',
  source_batch_id BIGINT UNSIGNED DEFAULT NULL COMMENT '来源导入批次',
  absorbed_person_id BIGINT UNSIGNED DEFAULT NULL COMMENT '补账到哪个主播（NULL = 还没认领）',
  absorbed_at DATETIME(3) DEFAULT NULL COMMENT '补账时间（NULL = 还在暂存区）',
  created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  updated_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3),
  PRIMARY KEY (id),
  UNIQUE KEY uk_no_date (douyin_no, biz_date),
  KEY idx_open (absorbed_at, biz_date),
  KEY idx_person (absorbed_person_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
