-- 014_ip_allowlist · 展示端访问 IP 白名单（2026-10-11）
--
-- 背景：展示端访问授权从「地区门禁（河南 / 安徽）」切换为「IP 白名单授权」。
-- 白名单表非空 = 进入白名单授权模式：只有表内 IP（以及内网 / IP_GATE_ALLOWLIST
-- 环境变量里的 IP）放行，其它访客一律 403「您的 ip 不在授权范围内」。
-- 表为空时维持原有地区门禁，不影响存量环境。
--
-- ip 存原始字符串（精确 IP 或 CIDR，如 1.2.3.4 / 10.0.0.0/24），
-- 匹配走应用层 ip_matches_entry（精确 + CIDR 前缀）。
CREATE TABLE IF NOT EXISTS ip_allowlist (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  ip VARCHAR(45) NOT NULL,
  note VARCHAR(128) NOT NULL DEFAULT '',
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id),
  UNIQUE KEY uk_ip_allowlist_ip (ip)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
