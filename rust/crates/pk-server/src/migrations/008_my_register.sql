-- 008_my_register · 展示端手机号注册 + 绑定主播（2026-10-10 用户口径）
--
-- 需求原文：「添加注册功能，注册功能同时调用 [WorkBuddy 网关] 里的添加账号 oauth
-- 实现手机号登录，然后验证码数字就是密码，手机号就是账号。然后提醒用户进行绑定姓名，
-- 绑定之后不支持解绑。」（后续澄清：绑的是「主播」，用户自己不能解绑，管理员可以改）
--
-- 三条改动：
--   bound    1 = 已绑定主播。老数据默认 1 —— 管理员手建的账号本来就是给某个主播建的；
--            自助注册出来的账号是 0（只有手机号，还没对应主播）。
--   bound_at 绑定时间（空串 = 从未绑定）
--   source   admin（后台手建）/ register（手机号自助注册）
--   wb_state WorkBuddy 网关注册授权流水号（state，审计用；没走 OAuth 就是空串）
--
-- 注册瞬间 name 列先填「手机号」占位——PRIMARY KEY 不能为空，而注册时还不知道主播是谁。
-- 绑定成功后 name 改写成主播姓名（登录仍然支持手机号：verify 先按手机号找人）。
-- 绑定是不可逆的（用户侧）：绑定接口只会接受 bound=0 的账号；改绑只开管理员接口。
--
-- 幂等：ALTER 重复执行会报「列已存在」，靠 schema_migrations 保证只跑一次。

ALTER TABLE my_accounts
  ADD COLUMN bound TINYINT(1) NOT NULL DEFAULT 1,
  ADD COLUMN bound_at VARCHAR(40) NOT NULL DEFAULT '',
  ADD COLUMN source VARCHAR(16) NOT NULL DEFAULT 'admin',
  ADD COLUMN wb_state VARCHAR(64) NOT NULL DEFAULT '';
