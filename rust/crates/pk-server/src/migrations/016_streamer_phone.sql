-- 016_streamer_phone · 主播手机号落地 + 批量开号锚点（2026-10-11）
--
-- 背景（对应 docs/13 §1，承接 docs/12 的 P7 与 P1）：
--   ① `person` 表**根本没有存手机号的地方** —— 运营手里的主播手机号只能躺在 Excel / 微信里，
--      `SHOW COLUMNS FROM person LIKE '%phone%'` 返回空；而展示端账号（my_accounts）的
--      手机号登录 / 忘记密码重置 / 微信二要素绑定三件事全部依赖手机号
--      → 104 个在职主播里只有 1 个有登录账号（鹏先生 / 13354463148）。
--   ② 没有批量开号入口：运营只能 POST /api/admin/my-accounts 一条条建，104 人逐个点不现实。
--
-- 本迁移**只做结构**（沿用 009 / 015 的先例：迁移只管结构，数据口径由代码与运维 SQL 负责）：
--   * person.phone           档案上的主播手机号（运营登记；空串 = 未登记）
--   * person.phone_uniq      生成列 NULLIF(phone,'') + 唯一索引（与 012 的 my_accounts.phone_uniq
--                            **完全同款手法**：空串 -> NULL，UNIQUE 允许多个 NULL，
--                            所以 104 个没登记手机号的人不会互相冲突，只有真重复才报错）
--   * idx_person_phone       按手机号反查主播（运营拿一个号码问"这是谁"；批量导入手机号时去重扫描）
--
-- 明确不改的东西（避免误读）：
--   * 不动 my_accounts（phone / phone_uniq / person_id 分别由 010 / 012 / 015 负责）
--   * 不改主键、不改列类型、不删列、不删行 → 104 个主播一行都不会少
--   * 不建外键（全库刻意无外键，见 001_core.sql:4-5）
--   * 不建 `person_contacts` 多联系方式表 —— 一人一号是当前事实，多号需求出现时再按
--     docs/13 §2.4 的升级路径加表，现在加只会让每条查询都多一个 LEFT JOIN。
--
-- 幂等与安全：information_schema 探测 + PREPARE/EXECUTE 动态 DDL（同 012 / 015）。
--   唯一索引在存量已存在重复手机号时**跳过创建**而不是让迁移失败卡住部署（012 同款）：
--   重复号码需人工在后台改掉后再补建（docs/13 §7.1 给了补建 SQL）。
--
-- 语句白名单：migrate.rs 的单测要求每条语句以 CREATE / ALTER / SET / PREPARE /
--   EXECUTE / DEALLOCATE 开头，动态 DDL 全部包在 SET @s = ... 里（不含裸 UPDATE / INSERT）。

-- ① person.phone：主播档案手机号（运营登记口径，与 my_accounts.phone 是两份数据、
--    语义不同：这里记"这个人怎么联系"，my_accounts 记"这个账号用什么号登录"）
SET @s = IF(
  (SELECT COUNT(*) FROM information_schema.columns
    WHERE table_schema = DATABASE() AND table_name = 'person' AND column_name = 'phone') = 0,
  'ALTER TABLE person
     ADD COLUMN phone VARCHAR(24) NOT NULL DEFAULT ''''
     COMMENT ''主播手机号（运营登记；空串 = 未登记）。批量开号的默认来源，也是微信绑定的联系凭据''',
  'SELECT 1'
);
PREPARE stmt_016 FROM @s;
EXECUTE stmt_016;
DEALLOCATE PREPARE stmt_016;

-- ② 手机号唯一（软唯一：空串不参与）+ 生成列。
--    为什么 person 侧也要唯一：批量开号时一个手机号只能对应一个登录账号
--    （my_accounts 侧 012 已经唯一），若台账里同号两人，开号就会随机落到其中一人头上。
--    ⚠️ 冲突检测**故意不加** `deleted_at IS NULL`：生成列只看 phone，
--       已软删的行照样占唯一索引，漏判会让 ALTER 直接失败、卡住部署。
SET @dup_phone_groups = (
  SELECT COUNT(*) FROM (
    SELECT phone FROM person WHERE phone <> '' GROUP BY phone HAVING COUNT(*) > 1
  ) t
);
SET @has_phone_uniq = (
  SELECT COUNT(*) FROM information_schema.columns
  WHERE table_schema = DATABASE() AND table_name = 'person' AND column_name = 'phone_uniq'
);
SET @s = IF(
  @dup_phone_groups = 0 AND @has_phone_uniq = 0,
  'ALTER TABLE person
     ADD COLUMN phone_uniq VARCHAR(24) GENERATED ALWAYS AS (NULLIF(phone, '''')) STORED,
     ADD UNIQUE KEY uk_person_phone_uniq (phone_uniq)',
  'SELECT 1'
);
PREPARE stmt_016 FROM @s;
EXECUTE stmt_016;
DEALLOCATE PREPARE stmt_016;

-- ③ 按手机号反查主播（唯一索引建在生成列上，MySQL 不会拿它去加速 phone = ? 的等值/前缀查询，
--    所以显式补一个 phone 上的普通索引；104 行的表现在用不上，几百人规模后批量导入去重会走它）
SET @s = IF(
  (SELECT COUNT(*) FROM information_schema.statistics
    WHERE table_schema = DATABASE() AND table_name = 'person' AND index_name = 'idx_person_phone') = 0,
  'CREATE INDEX idx_person_phone ON person (phone)',
  'SELECT 1'
);
PREPARE stmt_016 FROM @s;
EXECUTE stmt_016;
DEALLOCATE PREPARE stmt_016;
