-- 003_runtime · 运行时状态表并入 MySQL（2026-10-10 单库化）
-- 历史上这批表放在本地 SQLite（别名/方案/名单草稿/配置/机器人会话/展示端账号/绑定审计），
-- 单库决策后统一并入业务库——部署时随 upload-db 整体带上生产。
-- 与旧 SQLite 结构的差异（都是方言/保留字原因，语义不变）：
--   config.key      → cfg_key（key 是 MySQL 保留字）
--   roster_state.text → body（避开 TEXT 类型名）
--   plans.payload   → MEDIUMTEXT（分组结果 JSON 可能超过 64KB）
--   anchor_claims.screenshot → MEDIUMBLOB（截图上限 1.5MB，普通 BLOB 只有 64KB）
-- 旧 SQLite 文件的数据由 pk-server migrate 一次性搬进来（store::carry_legacy_sqlite）。

CREATE TABLE IF NOT EXISTS aliases (
  alias VARCHAR(64) NOT NULL,
  real_name VARCHAR(64) NOT NULL DEFAULT '',
  PRIMARY KEY (alias)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS plans (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  title VARCHAR(128) NOT NULL,
  metric VARCHAR(16) NOT NULL,
  days INT NOT NULL DEFAULT 3,
  payload MEDIUMTEXT NOT NULL,
  created_at VARCHAR(40) NOT NULL,
  published TINYINT(1) NOT NULL DEFAULT 0,
  event_type VARCHAR(32) NOT NULL DEFAULT '',
  PRIMARY KEY (id),
  KEY idx_plans_published (published)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS roster_state (
  event_type VARCHAR(32) NOT NULL,
  body MEDIUMTEXT NOT NULL,
  updated_at VARCHAR(40) NOT NULL,
  PRIMARY KEY (event_type)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS config (
  cfg_key VARCHAR(64) NOT NULL,
  cfg_value MEDIUMTEXT NOT NULL,
  updated_at VARCHAR(40) NOT NULL,
  PRIMARY KEY (cfg_key)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS bot_sessions (
  channel VARCHAR(16) NOT NULL,
  conversation_id VARCHAR(64) NOT NULL,
  kind VARCHAR(16) NOT NULL,
  last_text VARCHAR(255) DEFAULT NULL,
  last_seen VARCHAR(40) NOT NULL,
  is_admin TINYINT(1) NOT NULL DEFAULT 0,
  PRIMARY KEY (channel, conversation_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS my_accounts (
  name VARCHAR(64) NOT NULL,
  phone VARCHAR(24) NOT NULL DEFAULT '',
  pass VARCHAR(32) NOT NULL,
  is_admin TINYINT(1) NOT NULL DEFAULT 0,
  created_at VARCHAR(40) NOT NULL,
  PRIMARY KEY (name),
  UNIQUE KEY uk_my_pass (pass)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS anchor_claims (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  person_id BIGINT UNSIGNED NOT NULL,
  person_name VARCHAR(64) NOT NULL,
  team VARCHAR(16) NOT NULL DEFAULT '',
  douyin_no VARCHAR(32) NOT NULL,
  screenshot MEDIUMBLOB NOT NULL,
  mime VARCHAR(32) NOT NULL DEFAULT 'image/jpeg',
  status VARCHAR(16) NOT NULL DEFAULT 'pending',
  reject_reason VARCHAR(255) NOT NULL DEFAULT '',
  created_at VARCHAR(40) NOT NULL,
  decided_at VARCHAR(40) NOT NULL DEFAULT '',
  decided_by VARCHAR(32) NOT NULL DEFAULT '',
  PRIMARY KEY (id),
  KEY idx_claims_status (status, created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
