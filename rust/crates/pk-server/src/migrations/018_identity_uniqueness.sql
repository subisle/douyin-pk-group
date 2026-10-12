-- 018_identity_uniqueness · 一人一号一 bot 的唯一约束（2026-10-12 审计修复）
--
-- 背景（docs/13 末尾「v2 · 一人一号一 bot 的唯一约束」，承接 015 的 P4 / P5 与 016 的手机号口径）：
--   「一个主播 = 一个展示端账号 = 一个手机号 = 一个微信推送 bot」这条唯一性，
--   此前**只在代码里按姓名字符串比对**（store.rs 的台账重名判定），数据库侧三处都缺约束：
--     ① my_accounts.person_id      —— 015 加了列与非唯一索引，唯一索引被注释留在「S3 之后再启用」
--     ② wx_push_accounts.person_id —— 015 只加了非唯一索引 idx_wx_push_person，一人能挂多个 bot
--     ③ wx_push_accounts.ilink_user_id —— 完全无索引，同一个微信能被两个账号各扫一次
--   后果：同一个主播建两个账号 / 改名重扫就能挂两个 bot；反过来一个微信被两个人扫到也不报错，
--   推送会静默发给先激活的那个账号（push_bindings 按 conversation_id 走，谁激活算谁的）。
--
-- 做法：三个**软唯一**索引（生成列 + UNIQUE），与 012（my_accounts.phone）/ 015 ⑤（person_uniq）/
--   016（person.phone）**完全同款手法**：
--     * NULLIF(x, '')        → 空串变 NULL，UNIQUE 允许多个 NULL（没登记的不互相冲突）
--     * NULLIF(person_id, 0) → 0 变 NULL（person_id = 0 = 未绑定 / 管理员观看账号，015 定的哨兵语义）
--   为什么必须是软唯一（不能上普通唯一索引）：104 个在职主播绝大多数 person_id = 0（还没开号），
--   普通 UNIQUE(person_id) 会让**第二个人直接扫不了码** —— 这正是 015 注释里写下的理由。
--
-- 幂等与安全：information_schema 探测 + SET @s / PREPARE / EXECUTE 动态 DDL（同 012 / 015 / 016）。
--   唯一索引只在**存量无冲突**时才建；有冲突就跳过（退化成 `SELECT 1`），
--   不让迁移失败卡住部署 —— 冲突清单由运维 SQL 查出人工处理后补建（docs/13 §v2.3）。
--
-- 语句白名单：migrate.rs 的单测要求每条语句以 CREATE / ALTER / SET / PREPARE / EXECUTE /
--   DEALLOCATE 开头，动态 DDL 全部包在 SET @s = ... 里（不含裸 UPDATE / INSERT / DELETE）。
--
-- 明确不加的东西：
--   * push_bindings.to_user_id 不做唯一 —— push_bind_account（store.rs）的 upsert 撞上
--     to_user_id 唯一键时，ON DUPLICATE KEY UPDATE 更新的是**冲突的那一行**（另一个账号），
--     等于把别人的微信绑定顶掉，比现在的重复绑定更糟。收益（一个微信只绑一个账号）已经由
--     本迁移 ① 的 uk_wx_push_user_uniq 覆盖，理由记在 docs/13 §v2.4。

-- ① 一个微信只能属于一个账号（软唯一：空串不参与）
--    防的是「同一个人换账号名重扫」和「两个人的微信被登记到两行」两种错位。
--    ⚠️ 冲突检测**故意不加** person_deleted_at IS NULL：生成列只看 ilink_user_id，
--       已标记离职的行照样占唯一索引，漏判会让 ALTER 直接失败、卡住部署（016 同款教训）。
SET @dup_wx_user_groups = (
  SELECT COUNT(*) FROM (
    SELECT ilink_user_id FROM wx_push_accounts
     WHERE ilink_user_id <> '' GROUP BY ilink_user_id HAVING COUNT(*) > 1
  ) t
);
SET @has_wx_user_uniq = (
  SELECT COUNT(*) FROM information_schema.columns
  WHERE table_schema = DATABASE() AND table_name = 'wx_push_accounts'
    AND column_name = 'ilink_user_id_uniq'
);
SET @s = IF(
  @dup_wx_user_groups = 0 AND @has_wx_user_uniq = 0,
  'ALTER TABLE wx_push_accounts
     ADD COLUMN ilink_user_id_uniq VARCHAR(128) GENERATED ALWAYS AS (NULLIF(ilink_user_id, '''')) STORED,
     ADD UNIQUE KEY uk_wx_push_user_uniq (ilink_user_id_uniq)',
  'SELECT 1'
);
PREPARE stmt_018 FROM @s;
EXECUTE stmt_018;
DEALLOCATE PREPARE stmt_018;

-- ② 一个主播只能有一个 bot（软唯一：person_id = 0 不参与）
--    wx_push_accounts 的主键是 name（账号名），015 只给它加了非唯一索引 idx_wx_push_person；
--    没有这条约束时「一个主播两个账号各扫一次」就能挂两个 bot，推送会双发。
SET @dup_wx_person_groups = (
  SELECT COUNT(*) FROM (
    SELECT person_id FROM wx_push_accounts
     WHERE person_id > 0 GROUP BY person_id HAVING COUNT(*) > 1
  ) t
);
SET @has_wx_person_uniq = (
  SELECT COUNT(*) FROM information_schema.columns
  WHERE table_schema = DATABASE() AND table_name = 'wx_push_accounts'
    AND column_name = 'person_uniq'
);
SET @s = IF(
  @dup_wx_person_groups = 0 AND @has_wx_person_uniq = 0,
  'ALTER TABLE wx_push_accounts
     ADD COLUMN person_uniq BIGINT UNSIGNED GENERATED ALWAYS AS (NULLIF(person_id, 0)) STORED,
     ADD UNIQUE KEY uk_wx_push_person_uniq (person_uniq)',
  'SELECT 1'
);
PREPARE stmt_018 FROM @s;
EXECUTE stmt_018;
DEALLOCATE PREPARE stmt_018;

-- ③ 按 ilink_user_id 反查（唯一索引建在生成列上，MySQL 不会拿它去加速 ilink_user_id = ? 的
--    等值查询；store.rs 的扫码查重走的就是等值查询，补一个普通索引。与 016 给 person.phone
--    补 idx_person_phone 同款理由）
SET @s = IF(
  (SELECT COUNT(*) FROM information_schema.statistics
    WHERE table_schema = DATABASE() AND table_name = 'wx_push_accounts'
      AND index_name = 'idx_wx_push_ilink_user') = 0,
  'CREATE INDEX idx_wx_push_ilink_user ON wx_push_accounts (ilink_user_id)',
  'SELECT 1'
);
PREPARE stmt_018 FROM @s;
EXECUTE stmt_018;
DEALLOCATE PREPARE stmt_018;

-- ④ 一个主播只能有一个展示端账号（软唯一：person_id = 0 不参与）
--    015 ⑤ 已经按同样的 IF 口径建过（列名 / 索引名完全一致），生产上跑过 015 就会命中
--    @has_my_person_uniq > 0 → 跳过；015 因存量冲突跳过的，这里同样算一次冲突、同样跳过。
--    两条迁移用同一套判定，谁先跑结果都一样，不用管执行顺序。
-- ④ 的前置：person_id 列是 015 才加的（015 之前建的库没有这一列）。
--    不先探测就直接 SELECT person_id 会让整条迁移报 Unknown column 而中断 ——
--    与「不让迁移失败卡住部署」的原则冲突（本机 douyin_dev 就是没跑 015 的库，实测踩到）。
--    列不存在 → @dup 置 -1，下面的 IF 走 SELECT 1 跳过，等 015 跑完再补建。
--    注：注释只能写 `--` 行注释 —— migrate.rs 的 split_statements 只剥 `--` 行，
--    块注释（/* */）会被当成语句、过不了 embedded_migrations_split_cleanly 白名单。
SET @has_my_person_col = (
  SELECT COUNT(*) FROM information_schema.columns
  WHERE table_schema = DATABASE() AND table_name = 'my_accounts'
    AND column_name = 'person_id'
);
--    ⚠️ 不能写成 `SET @dup = IF(@has_col = 0, -1, (SELECT ... FROM my_accounts ...))`：
--    MySQL 在**解析阶段**就会编译 IF 的两个分支，列不存在直接 Unknown column，
--    运行时短路根本来不及（实测踩到）。所以连「数重复」本身也要包进动态 SQL，
--    只有 EXECUTE 时才解析，列不存在就走 SELECT -1 那条。
SET @s = IF(
  @has_my_person_col = 0,
  'SELECT -1 INTO @dup_my_person_groups',
  'SELECT COUNT(*) INTO @dup_my_person_groups FROM (
     SELECT person_id FROM my_accounts
      WHERE person_id > 0 GROUP BY person_id HAVING COUNT(*) > 1
   ) t'
);
PREPARE stmt_018 FROM @s;
EXECUTE stmt_018;
DEALLOCATE PREPARE stmt_018;
SET @has_my_person_uniq = (
  SELECT COUNT(*) FROM information_schema.columns
  WHERE table_schema = DATABASE() AND table_name = 'my_accounts'
    AND column_name = 'person_uniq'
);
SET @s = IF(
  @dup_my_person_groups = 0 AND @has_my_person_uniq = 0,
  'ALTER TABLE my_accounts
     ADD COLUMN person_uniq BIGINT UNSIGNED GENERATED ALWAYS AS (NULLIF(person_id, 0)) STORED,
     ADD UNIQUE KEY uk_my_person_uniq (person_uniq)',
  'SELECT 1'
);
PREPARE stmt_018 FROM @s;
EXECUTE stmt_018;
DEALLOCATE PREPARE stmt_018;

-- ⑤ 按 person_id 查「这个主播有没有 wx bot」时让优化器有得选（与 015 的 idx_wx_push_person
--    并存：非唯一索引服务于范围扫描 / 孤儿扫描，唯一索引负责拦重复，两者不冲突）
SET @s = IF(
  (SELECT COUNT(*) FROM information_schema.statistics
    WHERE table_schema = DATABASE() AND table_name = 'wx_push_accounts'
      AND index_name = 'idx_wx_push_person') = 0,
  'CREATE INDEX idx_wx_push_person ON wx_push_accounts (person_id)',
  'SELECT 1'
);
PREPARE stmt_018 FROM @s;
EXECUTE stmt_018;
DEALLOCATE PREPARE stmt_018;
