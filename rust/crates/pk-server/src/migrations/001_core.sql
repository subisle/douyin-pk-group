-- 001_core · 核心表
-- 与 sqlpub 源库（douyinxs）的表结构逐字段对齐（2026-10-02 从 SHOW CREATE TABLE 抄齐），
-- 差异只有：不建外键（全量迁移时不用管插入顺序）、不设 AUTO_INCREMENT 起点。
-- pk-server 的 SQL（dialect.rs A 源）直接跑在这套表上，S3 切 LOCAL 靠它。

CREATE TABLE IF NOT EXISTS person (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  name VARCHAR(64) NOT NULL DEFAULT '',
  gender ENUM('male','female','unknown') NOT NULL DEFAULT 'unknown',
  master_id BIGINT UNSIGNED DEFAULT NULL COMMENT '所属师傅（自引用 person.id）',
  generation INT DEFAULT NULL COMMENT '第几代徒弟',
  group_name VARCHAR(64) DEFAULT NULL COMMENT '所属团队/分组',
  avatar_url VARCHAR(512) DEFAULT NULL COMMENT '头像，每日之星图要用',
  hide_in_daily_report TINYINT(1) NOT NULL DEFAULT 0,
  status ENUM('active','left','paused') NOT NULL DEFAULT 'active',
  joined_at DATE DEFAULT NULL,
  created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  updated_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3),
  deleted_at DATETIME(3) DEFAULT NULL,
  PRIMARY KEY (id),
  KEY idx_master (master_id),
  KEY idx_status_gender (status, gender),
  KEY idx_name (name)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS account (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  person_id BIGINT UNSIGNED NOT NULL,
  anchor_id VARCHAR(64) NOT NULL COMMENT '抖音采集 ID（唯一键）',
  douyin_no VARCHAR(64) NOT NULL DEFAULT '' COMMENT '抖音号（展示用）',
  anchor_name VARCHAR(128) NOT NULL DEFAULT '',
  is_primary TINYINT(1) NOT NULL DEFAULT 0,
  status ENUM('active','disabled') NOT NULL DEFAULT 'active',
  created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  PRIMARY KEY (id),
  UNIQUE KEY uk_anchor (anchor_id),
  KEY idx_person (person_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS daily_metric (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  person_id BIGINT UNSIGNED NOT NULL,
  anchor_id VARCHAR(64) NOT NULL,
  biz_date DATE NOT NULL,
  wave BIGINT NOT NULL DEFAULT 0 COMMENT '日音浪 = 当前累计 - 上次累计',
  cumulative_wave BIGINT NOT NULL DEFAULT 0 COMMENT '当天累计总音浪（快照原值）',
  prev_snapshot_date DATE DEFAULT NULL COMMENT '上一次快照日期，用于判定差分跨度',
  wave_span SMALLINT NOT NULL DEFAULT 1 COMMENT '差分跨越天数；>1 表示中间漏采',
  wave_reliable TINYINT(1) NOT NULL DEFAULT 1 COMMENT '0=漏采或回退导致日音浪不可信',
  minutes INT NOT NULL DEFAULT 0 COMMENT '当日直播分钟数（差分）',
  cumulative_minutes INT NOT NULL DEFAULT 0,
  minutes_span SMALLINT NOT NULL DEFAULT 1,
  minutes_reliable TINYINT(1) NOT NULL DEFAULT 1,
  is_live TINYINT(1) NOT NULL DEFAULT 0,
  tier CHAR(1) DEFAULT NULL COMMENT '当日等级快照，导出直接读',
  source_batch_id BIGINT UNSIGNED DEFAULT NULL,
  recomputed_at DATETIME(3) DEFAULT NULL,
  PRIMARY KEY (id),
  UNIQUE KEY uk_person_date (person_id, biz_date),
  KEY idx_daily_date (biz_date)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS import_batch (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  source VARCHAR(32) NOT NULL,
  filename VARCHAR(255) NOT NULL,
  biz_date DATE DEFAULT NULL COMMENT '业务日期',
  team VARCHAR(16) DEFAULT NULL COMMENT 'male / female / mixed',
  imported_rows INT UNSIGNED NOT NULL DEFAULT 0,
  ok_rows INT UNSIGNED NOT NULL DEFAULT 0 COMMENT '成功写入行数',
  failed_rows INT UNSIGNED NOT NULL DEFAULT 0 COMMENT '失败/未匹配行数',
  note VARCHAR(512) DEFAULT NULL COMMENT '失败摘要 / 人工备注',
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id),
  KEY idx_import_batch_source_time (source, created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
