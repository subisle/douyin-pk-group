-- 038 · 展示端账号绑定邮箱（2026-10-17 用户口径）
--
-- 背景：忘记密码重置的口径本轮改为「随机密码发到绑定邮箱」——
--   绑了邮箱的账号：重置时生成 CSPRNG 随机密码，SMTP 发到该邮箱；
--   没绑邮箱的账号：重置直接提示「未绑定邮箱，请联系管理员重置密码」。
-- 邮箱由用户在展示端「我的」页自助绑定/解绑（POST /api/public/my/email）。
--
-- 新增列：my_accounts.email VARCHAR(128) NOT NULL DEFAULT ''
--   ''（默认） = 未绑定邮箱
--
-- 幂等：ADD COLUMN IF NOT EXISTS，可安全重放。

ALTER TABLE my_accounts ADD COLUMN IF NOT EXISTS email VARCHAR(128) NOT NULL DEFAULT '';
