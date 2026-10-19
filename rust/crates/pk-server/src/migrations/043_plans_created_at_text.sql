-- 043 · plans.created_at 列型对齐（2026-10-18 本机实读发现）
--
-- 背景：001_core.sql 里 plans.created_at 是 VARCHAR(40)（存北京时间字面量，
--   与 my_accounts / push_jobs 的 varchar 时间列同一口径），但本机库是从更早的
--   线上库恢复的，该列实际落成了 TIMESTAMPTZ。`CREATE TABLE IF NOT EXISTS`
--   不会改已存在的表，于是代码与库两边对不上：
--     · store::map_plan 按 String 解码 → 每行 ColumnDecode 失败，
--       GET /api/plans 恒返回空列表（日志刷「方案行解析失败，已跳过该行」，
--       11 个方案像凭空消失）；
--     · save_plan 把 RFC3339 文本写进 timestamptz 列 → PG 报
--       `column "created_at" is of type timestamp with time zone but expression
--        is of type text`，保存方案恒 500，分组页没法新建方案。
--
-- 只改列型、不改语义：存量值按会话时区（Asia/Shanghai）渲染成与代码一致的
-- `YYYY-MM-DDTHH:MM:SS+08:00` 字面量（前端只切前 16 位显示，格式不变）。
--
-- 幂等：列已经是 varchar 时整段跳过；可安全重放。

DO $$
BEGIN
  IF EXISTS (
    SELECT 1 FROM information_schema.columns
    WHERE table_schema = 'public'
      AND table_name = 'plans'
      AND column_name = 'created_at'
      AND data_type = 'timestamp with time zone'
  ) THEN
    ALTER TABLE plans ALTER COLUMN created_at TYPE VARCHAR(40)
      USING to_char(created_at AT TIME ZONE 'Asia/Shanghai', 'YYYY-MM-DD"T"HH24:MI:SS') || '+08:00';
    ALTER TABLE plans ALTER COLUMN created_at SET NOT NULL;
  END IF;
END $$;
