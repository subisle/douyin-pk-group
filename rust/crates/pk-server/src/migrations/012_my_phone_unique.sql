-- 012_my_phone_unique · 同一手机号只能有一个账号（2026-10-11 审计修复）
--
-- 背景：my_accounts 只有 PRIMARY KEY(name)，phone 列没有索引也没有唯一约束。
-- 注册接口的「手机号查重」是 SELECT + INSERT 两步，两个并发请求（同手机号、不同姓名）
-- 可以同时穿过检查，落成两行同 phone —— 之后手机号登录 / 忘记密码会命中不确定的一行。
--
-- 做法：加一个生成列 phone_uniq = NULLIF(phone, '')（空串 -> NULL，管理员未登记手机号的行
-- 在唯一索引里允许多个 NULL），再对它建唯一索引；不改任何既有行的 phone 语义。
-- 兼容：MariaDB 11.x / MySQL 8+ 都支持 STORED 生成列 + 唯一索引。
--
-- 幂等与安全：
--   * 列/索引已存在（重跑、或人工加过）-> 跳过；
--   * 历史数据里已存在重复的非空 phone -> **跳过建索引**而不是让迁移失败、卡住部署；
--     用 `SHOW INDEX FROM my_accounts WHERE Key_name='uk_my_phone_uniq'` 可确认是否生效，
--     重复号码需要人工在后台改绑后再补建。
SET @dup_phone_groups = (
  SELECT COUNT(*) FROM (
    SELECT phone FROM my_accounts WHERE phone <> '' GROUP BY phone HAVING COUNT(*) > 1
  ) t
);
SET @has_phone_uniq = (
  SELECT COUNT(*) FROM information_schema.columns
  WHERE table_schema = DATABASE() AND table_name = 'my_accounts' AND column_name = 'phone_uniq'
);
SET @ddl = IF(
  @dup_phone_groups = 0 AND @has_phone_uniq = 0,
  'ALTER TABLE my_accounts ADD COLUMN phone_uniq VARCHAR(24) GENERATED ALWAYS AS (NULLIF(phone, '''')) STORED, ADD UNIQUE KEY uk_my_phone_uniq (phone_uniq)',
  'SELECT 1'
);
PREPARE add_phone_uniq_stmt FROM @ddl;
EXECUTE add_phone_uniq_stmt;
DEALLOCATE PREPARE add_phone_uniq_stmt;
