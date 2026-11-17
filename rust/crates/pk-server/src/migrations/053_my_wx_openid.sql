-- 053_my_wx_openid（2026-11-15）：微信小程序登录
-- my_accounts 增加微信 openid；一个 openid 只能绑定一个账号（空串/未设置不参与唯一约束）。
ALTER TABLE my_accounts ADD COLUMN IF NOT EXISTS wx_openid TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS uk_my_wx_openid
  ON my_accounts (wx_openid)
  WHERE wx_openid IS NOT NULL AND wx_openid <> '';
