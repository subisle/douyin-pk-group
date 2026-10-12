-- 对账校验和模板（scripts/dbcheck/checksum.sql）
--
-- 由 scripts/dbcheck/dbcheck.sh 做占位符替换后执行：
--   {{TABLE}}  表名（来自 scripts/db-align/tables.manifest）
--   {{ORDER}}  md5 聚合的排序键（check_pk）
--   {{COLS}}   参与校验的业务列，已由脚本包成 coalesce(列::text,'<NULL>') 以区分 NULL 与空串
--
-- 关键点：
--   * SET TIME ZONE 'UTC' —— 两端会话时区可能不同（64 容器可能是 UTC），
--     timestamptz 的 ::text 渲染依赖会话时区，不固定会产生假差异。
--   * 只比业务列，不含 id / 时间戳列，避开 touch_updated_at 触发器造成的假差异。
--   * 输出一行：行数|md5，两端 diff 为空即对账通过。

SET TIME ZONE 'UTC';

SELECT count(*)::text || '|' || coalesce(md5(string_agg(h, '' ORDER BY {{ORDER}})), 'EMPTY')
FROM (
  SELECT {{ORDER}}, md5(concat_ws('|', {{COLS}})) AS h
  FROM {{TABLE}}
) s;
