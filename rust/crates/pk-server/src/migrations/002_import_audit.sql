-- 002_import_audit · 补齐 docs/00 §4 核心实体
-- 幂等：全部 IF NOT EXISTS。目标库 MariaDB 11.4 / MySQL 8+ 均可执行。
-- 执行方式：仅 pk-server migrate 显式触发（douyin_migrator 账号），启动不自动跑。
--
-- 注：import_batch 的审计列（biz_date/team/ok_rows/failed_rows/note）已并入
-- 001_core 建表语句（新库一次到位）；已应用旧 001 的存量库由本迁移历史覆盖，
-- 无需再执行 ALTER（MySQL 不支持 ADD COLUMN IF NOT EXISTS）。

-- 月度聚合指标（口径见 docs/00 §4：后端定义统计窗口，不让前端各自推导）
CREATE TABLE IF NOT EXISTS monthly_metric (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  person_id BIGINT UNSIGNED NOT NULL,
  period CHAR(7) NOT NULL COMMENT 'YYYY-MM',
  wave BIGINT NOT NULL DEFAULT 0 COMMENT '当月音浪合计（增量日）',
  live_days INT NOT NULL DEFAULT 0,
  best_day_wave BIGINT NOT NULL DEFAULT 0,
  best_day_date DATE DEFAULT NULL,
  source_batch_id BIGINT UNSIGNED DEFAULT NULL,
  recomputed_at DATETIME(3) DEFAULT NULL,
  PRIMARY KEY (id),
  UNIQUE KEY uk_person_period (person_id, period),
  KEY idx_period (period)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- 账号级累计音浪快照（副号数据补充；与源库 douyinxs.wave_snapshot 对齐）
CREATE TABLE IF NOT EXISTS wave_snapshot (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  anchor_id VARCHAR(64) NOT NULL,
  biz_date DATE NOT NULL,
  wave_value BIGINT NOT NULL DEFAULT 0 COMMENT '平台累计总音浪',
  rank_in_guild INT DEFAULT NULL,
  source_batch_id BIGINT UNSIGNED DEFAULT NULL,
  created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  PRIMARY KEY (id),
  UNIQUE KEY uk_anchor_date (anchor_id, biz_date),
  KEY idx_snap_date (biz_date)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- 账号级直播时长观测
CREATE TABLE IF NOT EXISTS duration_snapshot (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  anchor_id VARCHAR(64) NOT NULL,
  biz_date DATE NOT NULL,
  minutes INT NOT NULL DEFAULT 0 COMMENT '当日直播分钟数（差分）',
  cumulative_minutes INT NOT NULL DEFAULT 0,
  minutes_span SMALLINT NOT NULL DEFAULT 1,
  minutes_reliable TINYINT(1) NOT NULL DEFAULT 1,
  source_batch_id BIGINT UNSIGNED DEFAULT NULL,
  created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  PRIMARY KEY (id),
  UNIQUE KEY uk_anchor_date (anchor_id, biz_date),
  KEY idx_dur_date (biz_date)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
