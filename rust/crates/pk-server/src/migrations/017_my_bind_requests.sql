-- 017_my_bind_requests · 展示端「绑定主播」改管理员审核制（2026-10-12 安全审计修复）
--
-- 背景（绑定冒领链）：
--   旧链路 my_register_check_name（免 token）与 my_bind_search（任意 token 模糊搜全台账）
--   都会下发内部 personId，my_bind 凭自报 personId 直接绑、无审核 —— 任何人枚举到别人的
--   personId 就能冒领身份，把别人的「我的」数据、水印、推送全接管。
--   修复后：绑定只收姓名，服务端精确查台账后落本表（pending），管理员审核通过才真正绑定
--   （复用 my_account_bind 的核心绑定逻辑）。
--
-- 只建表，不动任何既有表与数据（远程已应用 001-016，本迁移可重复执行）。
-- 口径与 anchor_claims（抖音号绑定申请，006/008 同款审核流）对齐：
--   status pending/approved/rejected；decided_at NULL = 未裁决；decided_by 目前恒 'admin'。

CREATE TABLE IF NOT EXISTS my_bind_requests (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  name VARCHAR(64) NOT NULL COMMENT '申请账号（my_accounts.name；未绑定时是手机号占位名）',
  person_id BIGINT UNSIGNED NOT NULL DEFAULT 0 COMMENT '申请绑定的主播 id（服务端按姓名精确查台账得来，不是用户自报）',
  person_name VARCHAR(64) NOT NULL DEFAULT '' COMMENT '主播姓名快照（改名不影响审核记录）',
  team VARCHAR(16) NOT NULL DEFAULT '' COMMENT '主播团别快照（male/female）',
  status VARCHAR(16) NOT NULL DEFAULT 'pending' COMMENT 'pending/approved/rejected',
  reason VARCHAR(255) NOT NULL DEFAULT '' COMMENT '驳回原因（approve 时为空）',
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  decided_at TIMESTAMP NULL DEFAULT NULL COMMENT '裁决时间；NULL = 未裁决',
  decided_by VARCHAR(64) NOT NULL DEFAULT '' COMMENT '裁决人（目前恒 admin）',
  PRIMARY KEY (id),
  KEY idx_name_status (name, status),
  KEY idx_status (status)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
