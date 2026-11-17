-- 054_push_mini_subscribe（2026-11-17）：微信小程序订阅消息 + 一键微信对接
--
-- push_bindings 同时承载 iLink 扫码账号与小程序 openid：
-- - mini_openid 非空 = 该展示端账号可通过微信小程序订阅消息接收推送；
-- - mini_* 三个开关独立于网页端机器人开关，避免把「只在机器人里开过」误当成
--   小程序订阅已授权（订阅消息按模板单独授权，不能共用开关）。
-- 同一 openid 只能对应一个展示端账号，和 my_accounts.uk_my_wx_openid 对齐。

ALTER TABLE push_bindings ADD COLUMN IF NOT EXISTS mini_openid VARCHAR(128) NOT NULL DEFAULT '';
ALTER TABLE push_bindings ADD COLUMN IF NOT EXISTS mini_subscribed_at VARCHAR(40) NOT NULL DEFAULT '';
ALTER TABLE push_bindings ADD COLUMN IF NOT EXISTS mini_daily_report SMALLINT NOT NULL DEFAULT 0;
ALTER TABLE push_bindings ADD COLUMN IF NOT EXISTS mini_notify_target SMALLINT NOT NULL DEFAULT 0;
ALTER TABLE push_bindings ADD COLUMN IF NOT EXISTS mini_lineage_report SMALLINT NOT NULL DEFAULT 0;

CREATE UNIQUE INDEX IF NOT EXISTS uk_push_mini_openid
  ON push_bindings (mini_openid)
  WHERE mini_openid <> '';
