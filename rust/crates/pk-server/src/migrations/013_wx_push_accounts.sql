-- 013_wx_push_accounts · 用户自助扫码的微信推送账号（2026-10-11）
--
-- 背景：按腾讯 openclaw-weixin 官方协议，扫码登录返回的是**账号级 bot 凭证**
-- （bot_token / ilink_bot_id / ilink_user_id / baseurl），一个网关可以同时跑
-- 多个微信账号（多账号支持）。展示端「我的」页改为让用户自己扫码：
-- 扫码确认后把凭证存到本表，服务端按账号各跑一条 getupdates 长轮询；
-- 用户首次给机器人发消息时留下 context_token，之后推送直接发给该会话。
--
-- 与 push_bindings 的关系：
--   push_bindings 仍是「展示端账号 → 推送开关/追分目标」的唯一事实源；
--   本表只存扫码得到的微信侧凭证与会话上下文，按展示端账号名关联。
CREATE TABLE IF NOT EXISTS wx_push_accounts (
  name VARCHAR(64) NOT NULL,
  team VARCHAR(16) NOT NULL DEFAULT '',
  person_id BIGINT UNSIGNED NOT NULL DEFAULT 0,
  ilink_bot_id VARCHAR(128) NOT NULL DEFAULT '',
  ilink_user_id VARCHAR(128) NOT NULL DEFAULT '',
  bot_token VARCHAR(255) NOT NULL DEFAULT '',
  base_url VARCHAR(255) NOT NULL DEFAULT '',
  nickname VARCHAR(64) NOT NULL DEFAULT '',
  conversation_id VARCHAR(191) NOT NULL DEFAULT '',
  updates_buf VARCHAR(255) NOT NULL DEFAULT '',
  bound_at VARCHAR(40) NOT NULL DEFAULT '',
  updated_at VARCHAR(40) NOT NULL DEFAULT '',
  PRIMARY KEY (name),
  KEY idx_wx_push_conv (conversation_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
