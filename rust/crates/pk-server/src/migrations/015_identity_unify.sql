-- 015_identity_unify · 身份统一锚点：person_id 成为跨表主键（2026-10-11）
--
-- 背景（对应 docs/12 §2 的问题清单）：
--   P2  my_accounts 是唯一一张完全没有 person_id 的身份表（003_runtime.sql:54-61 只有 5 列），
--       与 person 之间零外键，只靠 PK(name) 与 person.name 字符串相等 —— 主播改名即失联。
--   P1  my_accounts / push_bindings / wx_push_accounts 三张表都以 name 为主键，
--       person.name 改名时三张表完全不知情（admin.rs:432/461 只改 person）。
--   P3  软删 person（admin.rs:515/3518/3638）没有任何级联，bot 凭证残留成孤儿。
--   P4  「一人一号」判定按姓名比对（store.rs:1178），台账重名主播永远注册不了。
--   P5  push_bindings / wx_push_accounts 有 person_id 列但写入口径不一致（store.rs:1603 写、
--       store.rs:2171 不写），person_id=0 让 push.rs:303 的追分排名计算静默跳过。
--
-- 本迁移**只做结构，不改任何既有数据**（沿用 009 的先例：迁移只管结构，数据口径由代码负责）：
--   * 全部是 ADD COLUMN / CREATE INDEX / CREATE TABLE IF NOT EXISTS，不删列、不改主键、不改类型
--   * 新列一律带 DEFAULT，现有 INSERT 不带新列照样成功（向后兼容）
--   * person_id = 0 的哨兵语义与 004/013 既有约定完全一致（0 = 未绑定 / 管理员观看账号）
--   * 存量回填与孤儿扫描见 docs/12 附录 B（运维 SQL，不在迁移里跑）
--
-- 幂等与安全：
--   * MySQL 不支持 ADD COLUMN IF NOT EXISTS / CREATE INDEX IF NOT EXISTS，
--     统一用 information_schema 探测 + 动态 DDL，重跑或人工提前加过都不会报错卡住部署
--     （与 012_my_phone_unique.sql 同款手法）。
--   * 「一人一号」唯一索引只在存量无冲突时才建（012 同款：有重复就跳过，不阻断部署）。

-- ①-1 my_accounts 补 person_id（P2：唯一没有该列的身份表）
SET @s = IF(
  (SELECT COUNT(*) FROM information_schema.columns
    WHERE table_schema = DATABASE() AND table_name = 'my_accounts' AND column_name = 'person_id') = 0,
  'ALTER TABLE my_accounts
     ADD COLUMN person_id BIGINT UNSIGNED NOT NULL DEFAULT 0
     COMMENT ''对应 person.id；0 = 未绑定主播 / 管理员观看账号（与 push_bindings / wx_push_accounts 同语义）''',
  'SELECT 1'
);
PREPARE stmt_015 FROM @s;
EXECUTE stmt_015;
DEALLOCATE PREPARE stmt_015;

-- ①-2 按人查账号（P7 批量开号后的去重扫描路径 / 「某主播有没有账号」）
SET @s = IF(
  (SELECT COUNT(*) FROM information_schema.statistics
    WHERE table_schema = DATABASE() AND table_name = 'my_accounts' AND index_name = 'idx_my_person') = 0,
  'CREATE INDEX idx_my_person ON my_accounts (person_id)',
  'SELECT 1'
);
PREPARE stmt_015 FROM @s;
EXECUTE stmt_015;
DEALLOCATE PREPARE stmt_015;

-- ①-3 「已绑定主播的账号」列表 + 回填验收扫描（bound=0 是手机号占位行，不算绑定）
SET @s = IF(
  (SELECT COUNT(*) FROM information_schema.statistics
    WHERE table_schema = DATABASE() AND table_name = 'my_accounts' AND index_name = 'idx_my_person_bound') = 0,
  'CREATE INDEX idx_my_person_bound ON my_accounts (person_id, bound)',
  'SELECT 1'
);
PREPARE stmt_015 FROM @s;
EXECUTE stmt_015;
DEALLOCATE PREPARE stmt_015;

-- ②-1 push_bindings：person_id 已有列（004_push.sql:12），缺索引；
--     且 push_bind_account（store.rs:2171）的 upsert 不带 person_id → 默认 0，
--     导致 push.rs:303 的「有追分目标才算全团排名」判定静默跳过。索引让回填与排查可查。
SET @s = IF(
  (SELECT COUNT(*) FROM information_schema.statistics
    WHERE table_schema = DATABASE() AND table_name = 'push_bindings' AND index_name = 'idx_push_person') = 0,
  'CREATE INDEX idx_push_person ON push_bindings (person_id)',
  'SELECT 1'
);
PREPARE stmt_015 FROM @s;
EXECUTE stmt_015;
DEALLOCATE PREPARE stmt_015;

-- ②-2 wx_push_accounts：person_id 已有列（013:15），缺索引。
--     bot_token 是长期凭证，删主播时必须能按人定位到这些行（P3 安全项）。
SET @s = IF(
  (SELECT COUNT(*) FROM information_schema.statistics
    WHERE table_schema = DATABASE() AND table_name = 'wx_push_accounts' AND index_name = 'idx_wx_push_person') = 0,
  'CREATE INDEX idx_wx_push_person ON wx_push_accounts (person_id)',
  'SELECT 1'
);
PREPARE stmt_015 FROM @s;
EXECUTE stmt_015;
DEALLOCATE PREPARE stmt_015;

-- ③ 级联标记 person_deleted_at：主播软删时把 person.deleted_at 快照过来。
--    为什么不用外键 ON DELETE CASCADE：
--      1) 本库全库刻意不建外键（001_core.sql:4-5「全量迁移时不用管插入顺序」）；
--      2) 这里删的是**软删**（UPDATE person SET deleted_at = NOW(3)），外键级联根本不会触发；
--      3) 物理删凭证会导致误删后无法追溯 —— 标记列保留数据，读路径加 `person_deleted_at IS NULL` 过滤。
--    回填动作在应用层软删事务里做（admin.rs:515 / 3518 / 3638），漏网的由附录 B-3 扫描补齐。

SET @s = IF(
  (SELECT COUNT(*) FROM information_schema.columns
    WHERE table_schema = DATABASE() AND table_name = 'my_accounts' AND column_name = 'person_deleted_at') = 0,
  'ALTER TABLE my_accounts
     ADD COLUMN person_deleted_at DATETIME(3) DEFAULT NULL
     COMMENT ''主播被软删的时间快照（级联标记；NULL = 在职）''',
  'SELECT 1'
);
PREPARE stmt_015 FROM @s;
EXECUTE stmt_015;
DEALLOCATE PREPARE stmt_015;

SET @s = IF(
  (SELECT COUNT(*) FROM information_schema.columns
    WHERE table_schema = DATABASE() AND table_name = 'push_bindings' AND column_name = 'person_deleted_at') = 0,
  'ALTER TABLE push_bindings
     ADD COLUMN person_deleted_at DATETIME(3) DEFAULT NULL
     COMMENT ''主播被软删的时间快照（级联标记；NULL = 在职）''',
  'SELECT 1'
);
PREPARE stmt_015 FROM @s;
EXECUTE stmt_015;
DEALLOCATE PREPARE stmt_015;

SET @s = IF(
  (SELECT COUNT(*) FROM information_schema.columns
    WHERE table_schema = DATABASE() AND table_name = 'wx_push_accounts' AND column_name = 'person_deleted_at') = 0,
  'ALTER TABLE wx_push_accounts
     ADD COLUMN person_deleted_at DATETIME(3) DEFAULT NULL
     COMMENT ''主播被软删的时间快照（级联标记；NULL = 在职）''',
  'SELECT 1'
);
PREPARE stmt_015 FROM @s;
EXECUTE stmt_015;
DEALLOCATE PREPARE stmt_015;

-- ③-4/5/6 孤儿扫描与后台「离职主播残留」列表走索引（表虽小，保持与其它软删表一致的查询形态）
SET @s = IF(
  (SELECT COUNT(*) FROM information_schema.statistics
    WHERE table_schema = DATABASE() AND table_name = 'my_accounts' AND index_name = 'idx_my_person_deleted') = 0,
  'CREATE INDEX idx_my_person_deleted ON my_accounts (person_deleted_at)',
  'SELECT 1'
);
PREPARE stmt_015 FROM @s;
EXECUTE stmt_015;
DEALLOCATE PREPARE stmt_015;

SET @s = IF(
  (SELECT COUNT(*) FROM information_schema.statistics
    WHERE table_schema = DATABASE() AND table_name = 'push_bindings' AND index_name = 'idx_push_person_deleted') = 0,
  'CREATE INDEX idx_push_person_deleted ON push_bindings (person_deleted_at)',
  'SELECT 1'
);
PREPARE stmt_015 FROM @s;
EXECUTE stmt_015;
DEALLOCATE PREPARE stmt_015;

SET @s = IF(
  (SELECT COUNT(*) FROM information_schema.statistics
    WHERE table_schema = DATABASE() AND table_name = 'wx_push_accounts' AND index_name = 'idx_wx_push_person_deleted') = 0,
  'CREATE INDEX idx_wx_push_person_deleted ON wx_push_accounts (person_deleted_at)',
  'SELECT 1'
);
PREPARE stmt_015 FROM @s;
EXECUTE stmt_015;
DEALLOCATE PREPARE stmt_015;

-- ④ identity_orphan_log：孤儿审计留痕（P3）
--    只记录、不自动删 —— 误删 bot 凭证 / 登录账号是不可逆的，交给运营在后台确认后处理。
--    uk_orphan (tbl, row_key, kind) 保证重复扫描不刷屏（ON DUPLICATE KEY 只刷新 detected_at）。
CREATE TABLE IF NOT EXISTS identity_orphan_log (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  tbl VARCHAR(32) NOT NULL COMMENT 'my_accounts / push_bindings / wx_push_accounts',
  row_key VARCHAR(64) NOT NULL COMMENT '该表的现状主键 name（person_id 尚未成为 PK，先记 name）',
  person_id BIGINT UNSIGNED NOT NULL DEFAULT 0,
  kind VARCHAR(24) NOT NULL DEFAULT 'person_deleted'
    COMMENT 'person_deleted=主播已离职 / person_missing=person 行不存在 / dup_person=一人多号',
  detected_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  handled TINYINT(1) NOT NULL DEFAULT 0 COMMENT '1 = 运营已处理',
  note VARCHAR(255) NOT NULL DEFAULT '' COMMENT '处理备注（谁、什么时候、怎么处理的）',
  PRIMARY KEY (id),
  UNIQUE KEY uk_orphan (tbl, row_key, kind),
  KEY idx_orphan_open (handled, detected_at),
  KEY idx_orphan_person (person_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- ⑤ 一人一号硬约束（P4）：与 012 的 phone_uniq **完全同款手法** ——
--    生成列 person_uniq = NULLIF(person_id, 0)：person_id=0（未绑定 / 管理员观看账号）→ NULL，
--    UNIQUE 允许多个 NULL，所以 104 个未开号的人不会互相冲突，只有「同一主播两个登录账号」才报错。
--    存量有冲突时**跳过建索引**而不是让迁移失败卡住部署（012 同款），冲突需人工改绑后再补建。
--
--    ⚠️ 建议 S3 之后再启用（等 push_bind_account 等写入路径都带上 person_id），
--       否则「先建行后写 person_id」的 upsert 顺序可能撞锁。放在 015 里但默认受 IF 保护。
SET @dup_person_groups = (
  SELECT COUNT(*) FROM (
    SELECT person_id FROM my_accounts WHERE person_id > 0 GROUP BY person_id HAVING COUNT(*) > 1
  ) t
);
SET @has_person_uniq = (
  SELECT COUNT(*) FROM information_schema.columns
  WHERE table_schema = DATABASE() AND table_name = 'my_accounts' AND column_name = 'person_uniq'
);
SET @s = IF(
  @dup_person_groups = 0 AND @has_person_uniq = 0,
  'ALTER TABLE my_accounts
     ADD COLUMN person_uniq BIGINT UNSIGNED GENERATED ALWAYS AS (NULLIF(person_id, 0)) STORED,
     ADD UNIQUE KEY uk_my_person_uniq (person_uniq)',
  'SELECT 1'
);
PREPARE stmt_015 FROM @s;
EXECUTE stmt_015;
DEALLOCATE PREPARE stmt_015;
