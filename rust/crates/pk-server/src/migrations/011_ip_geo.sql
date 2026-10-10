-- 011_ip_geo · 访客 IP 归属地门禁 + 访问日志（2026-10-10）
-- 展示端只对河南 / 安徽访客开放；归属信息用 ihmily/ip-info-api 收集的免费接口
-- 并行查询、投票合并，缓存 7 天（查询失败默认拒绝）。
-- 访问日志记录所有访客（含外省被拒的）：登录了带账号，没登录只记 IP。

CREATE TABLE IF NOT EXISTS ip_geo_cache (
  ip VARBINARY(16) NOT NULL,
  province VARCHAR(32) NOT NULL DEFAULT '',
  city VARCHAR(64) NOT NULL DEFAULT '',
  isp VARCHAR(64) NOT NULL DEFAULT '',
  source VARCHAR(191) NOT NULL DEFAULT '',
  allowed TINYINT(1) NOT NULL DEFAULT 0,
  looked_up_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (ip)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS access_logs (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  kind VARCHAR(16) NOT NULL DEFAULT 'api',
  ip VARBINARY(16) NOT NULL,
  province VARCHAR(32) NOT NULL DEFAULT '',
  city VARCHAR(64) NOT NULL DEFAULT '',
  isp VARCHAR(64) NOT NULL DEFAULT '',
  geo_source VARCHAR(191) NOT NULL DEFAULT '',
  team VARCHAR(16) NOT NULL DEFAULT '',
  path VARCHAR(160) NOT NULL DEFAULT '',
  account_name VARCHAR(64) NOT NULL DEFAULT '',
  allowed TINYINT(1) NOT NULL DEFAULT 0,
  login_ok TINYINT(1) NOT NULL DEFAULT 0,
  -- 普通访问去重键（ip|path|5 分钟桶，登录行 NULL）：靠唯一键把并发重复请求挡掉
  dedup_key VARCHAR(191) NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id),
  UNIQUE KEY uniq_access_dedup (dedup_key),
  KEY idx_access_time (created_at),
  KEY idx_access_kind_time (kind, created_at),
  KEY idx_access_ip_time (ip, created_at),
  KEY idx_access_account_time (account_name, created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
