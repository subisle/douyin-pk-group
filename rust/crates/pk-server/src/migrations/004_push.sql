-- 004_push · 「我的」页微信推送（2026-10-09）
-- 三张表：
--   push_bindings —— 展示端账号 ↔ 微信机器人私聊的绑定关系 + 两个推送开关
--   my_targets    —— 每个人的追分目标（追人 / 追数值），每人最多 5 条
--   wx_sessions   —— 微信私聊会话上下文（主动推送必须带 to_user_id + context_token）
-- 说明：iLink 只能「被动回复」形态发消息，所以主动推送依赖最后一次入站消息留下的
-- context_token；会话在每次入站时刷新，token 失效时推送失败会记在 last_error。

CREATE TABLE IF NOT EXISTS push_bindings (
  name VARCHAR(64) NOT NULL,
  team VARCHAR(16) NOT NULL DEFAULT '',
  person_id BIGINT UNSIGNED NOT NULL DEFAULT 0,
  conversation_id VARCHAR(64) NOT NULL DEFAULT '',
  to_user_id VARCHAR(64) NOT NULL DEFAULT '',
  nickname VARCHAR(64) NOT NULL DEFAULT '',
  bind_code VARCHAR(16) NOT NULL DEFAULT '',
  code_expires VARCHAR(40) NOT NULL DEFAULT '',
  bound_at VARCHAR(40) NOT NULL DEFAULT '',
  notify_target TINYINT(1) NOT NULL DEFAULT 0,
  daily_report TINYINT(1) NOT NULL DEFAULT 0,
  last_push VARCHAR(40) NOT NULL DEFAULT '',
  last_error VARCHAR(255) NOT NULL DEFAULT '',
  updated_at VARCHAR(40) NOT NULL DEFAULT '',
  PRIMARY KEY (name),
  KEY idx_push_code (bind_code),
  KEY idx_push_conv (conversation_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS my_targets (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  name VARCHAR(64) NOT NULL,
  team VARCHAR(16) NOT NULL DEFAULT '',
  kind VARCHAR(16) NOT NULL DEFAULT 'person',
  target_person_id BIGINT UNSIGNED NOT NULL DEFAULT 0,
  target_name VARCHAR(64) NOT NULL DEFAULT '',
  target_value BIGINT NOT NULL DEFAULT 0,
  hit TINYINT(1) NOT NULL DEFAULT 0,
  last_gap BIGINT NOT NULL DEFAULT 0,
  last_notice_date VARCHAR(16) NOT NULL DEFAULT '',
  created_at VARCHAR(40) NOT NULL,
  PRIMARY KEY (id),
  KEY idx_targets_name (name)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS wx_sessions (
  conversation_id VARCHAR(64) NOT NULL,
  to_user_id VARCHAR(64) NOT NULL DEFAULT '',
  context_token VARCHAR(255) NOT NULL DEFAULT '',
  nickname VARCHAR(64) NOT NULL DEFAULT '',
  updated_at VARCHAR(40) NOT NULL DEFAULT '',
  PRIMARY KEY (conversation_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
