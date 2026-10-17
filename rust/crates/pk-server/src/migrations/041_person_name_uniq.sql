-- 041 · 在职主播艺名唯一（2026-10-18 用户拍板：艺名唯一，凭艺名直接认领开号）
--
-- 背景：「凭艺名认领开号」要求一个艺名在在职主播里只对应一个人，否则无法判定
--   认领的是谁；同时防止两个人抢注同一艺名。
--
-- 约束范围：只约束**在职**（deleted_at IS NULL）且**姓名非空**的行：
--   - 已删除/软删的历史行不参与（保留台账历史，改名/合并时不挡路）；
--   - name = '' 的空姓名行不参与（存量脏数据/占位行不该互相冲撞）。
-- 所以是 partial unique index，不是表级 UNIQUE 约束。
--
-- 建索引前本地已核实（2026-10-18）：person 在职 103 行，姓名非空的重名 0 行
-- → 零风险。远端若已有重名在职主播，本迁移会失败，必须先人工合并/改名
-- （本迁移不改数据）。
--
-- 幂等：CREATE INDEX IF NOT EXISTS，可安全重放。

CREATE UNIQUE INDEX IF NOT EXISTS uk_person_name_active ON person (name) WHERE deleted_at IS NULL AND name <> '';
