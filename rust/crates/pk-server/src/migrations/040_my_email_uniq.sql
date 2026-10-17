-- 040 · 绑定邮箱（QQ 邮箱）全局唯一（2026-10-18 用户拍板：同一 QQ 不能绑多个账号）
--
-- 背景：口径收紧为「绑定 = 填 QQ 号，服务端补 @qq.com」，同一 QQ 邮箱只允许
--   挂在一个展示端账号上（忘记密码靠「手机号 × QQ」双因子定位单账号）。
--   应用层先查重给友好文案，这里用 partial unique index 兜住并发（23505 映射）。
--
-- 建索引前本地已核实（2026-10-18）：my_accounts 68 行，email 全部为空串，
-- 不存在重复值 → 零风险；将来远端若有存量重复，重复那行会让本迁移失败，
-- 必须先人工处理（本迁移不改数据、不清洗）。
--
-- 空串 = 未绑定，不参与唯一（WHERE email <> ''），所以解绑不受该索引约束。
-- 幂等：CREATE INDEX IF NOT EXISTS，可安全重放。

CREATE UNIQUE INDEX IF NOT EXISTS uk_my_email_uniq ON my_accounts (email) WHERE email <> '';
