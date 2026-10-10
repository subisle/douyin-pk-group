-- 010_my_pass_hash · 展示端账号密码哈希化（2026-10-10）
-- argon2id PHC 字符串约 97 字节；原 VARCHAR(32) 只能存明文。
-- uk_my_pass 依赖明文等值，哈希后失去意义，移除以避免误当作“密码防重”。
--
-- 幂等与兼容：
--   * MODIFY COLUMN 对相同列定义重复执行是安全的。
--   * MariaDB 支持 DROP INDEX IF EXISTS，但 MySQL 8 不支持；为了同时兼容
--     MariaDB 11.4 / MySQL 8+，这里用 information_schema 探测后动态 DROP。
--   * 迁移 runner 仍通过 schema_migrations 保证正常只执行一次；以下动态 DDL
--     是对“索引已被人工提前移除”的兜底。
ALTER TABLE my_accounts
  MODIFY COLUMN pass VARCHAR(255) NOT NULL DEFAULT '';

SET @has_uk_my_pass = (
  SELECT COUNT(*)
  FROM information_schema.statistics
  WHERE table_schema = DATABASE()
    AND table_name = 'my_accounts'
    AND index_name = 'uk_my_pass'
);
SET @drop_uk_my_pass = IF(
  @has_uk_my_pass > 0,
  'ALTER TABLE my_accounts DROP INDEX uk_my_pass',
  'SELECT 1'
);
PREPARE drop_uk_my_pass_stmt FROM @drop_uk_my_pass;
EXECUTE drop_uk_my_pass_stmt;
DEALLOCATE PREPARE drop_uk_my_pass_stmt;
