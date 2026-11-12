-- 050 · 按日贡献区分“没有音浪”和“显式音浪 0”（2026-11-12）
--
-- 048 只用 wave=0 表示空值，导致“先导音浪、再导只有时长的文件”时，
-- 重算 SUM(wave) 会把已有音浪清零。has_wave=1 才参与音浪重算；
-- 显式导入 0 仍会写成 has_wave=1。
--
-- 历史行只能可靠判断非零音浪；非零值回填为 has_wave=1，
-- 历史 0 保持 0（按未提供音浪处理），避免迁移时把旧缺失值误当显式零。

ALTER TABLE daily_import_contrib
  ADD COLUMN IF NOT EXISTS has_wave SMALLINT NOT NULL DEFAULT 0;

UPDATE daily_import_contrib
   SET has_wave = 1
 WHERE has_wave = 0 AND wave <> 0;
