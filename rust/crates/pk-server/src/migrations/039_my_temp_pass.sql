-- 039 · 忘记密码「临时密码」三列（2026-10-18 用户拍板，契约 docs/26/05 §1）
--
-- 背景：忘记密码改为「手机号 + QQ 号双因子匹配 → 临时密码发 QQ 邮箱」；
--   临时密码 15 分钟内可重复使用（不消费），登录后提示并强制改密。
--   重置会把 pass 覆盖成不可知的 CSPRNG 哈希，旧密码立即失效。
--
-- 本轮**不新增 qq 列**：QQ 号复用 038 的 my_accounts.email，存 <qq>@qq.com。
--
-- 新增列（只加列，不动既有列/数据）：
--   temp_pass_hash   VARCHAR(255) NOT NULL DEFAULT ''
--       '' = 当前没有待使用的临时密码；非空 = argon2id PHC 字符串。
--       绝不允许存明文；任何日志/响应都不出现该值的原文。
--   temp_pass_exp    TIMESTAMPTZ（可空）
--       NULL = 无临时密码；非空 = 该临时密码的绝对失效时刻（以 PostgreSQL
--       now() 为准，避免应用与库时钟漂移）。
--   must_change_pass SMALLINT NOT NULL DEFAULT 0
--       1 = 账号处于「必须改密」状态（临时密码登录后置位）；0 = 正常。
--       沿用 is_admin/bound 的 SMALLINT 口径，不引入 BOOLEAN 造成同表风格分裂。
--
-- 不建索引：登录/定位走 name 主键与 uk_my_phone_uniq，临时字段只作为行内附加
-- 列读取；当前 68 行量级，加索引只有写放大。
--
-- 幂等：ADD COLUMN IF NOT EXISTS，可安全重放（runner 不包事务，必须幂等）。

ALTER TABLE my_accounts ADD COLUMN IF NOT EXISTS temp_pass_hash VARCHAR(255) NOT NULL DEFAULT '';
ALTER TABLE my_accounts ADD COLUMN IF NOT EXISTS temp_pass_exp TIMESTAMPTZ;
ALTER TABLE my_accounts ADD COLUMN IF NOT EXISTS must_change_pass SMALLINT NOT NULL DEFAULT 0;
