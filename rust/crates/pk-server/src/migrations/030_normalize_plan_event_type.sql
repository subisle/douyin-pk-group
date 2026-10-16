-- 030: 存量方案的 event_type 回写（2026-10-09 改名后落库的方案一直没人管）
--
-- 背景（2026-10-17 审计 4-9）：2026-10-09 赛事类型改名（「初级」→「初级争霸赛小组赛」、
-- 「月底争霸赛」→「月终争霸赛」等）。改名只作用于**新写入**的行：
-- `store::normalize_event_type` 在保存时归一，但**从不回扫存量**。
-- 结果：改名前列库的方案其 event_type 仍是老值（或直接为空串），而
--   · 展示端 `st_page.html` 用 (p.eventType||'') === page.type **精确相等**过滤 → 老值永不命中；
--   · 后台 GroupPage.loadPlans 同样按 eventType 精确过滤 → 后台也看不到、也上不了架。
-- 表现：改名前的历史方案（含当时 published=1 的）在两端**凭空消失**，运营以为方案丢了。
--
-- 这里按 store::LEGACY_EVENT_RENAMES 的同款映射把老值 UPDATE 成新值。
-- 幂等：老值改完就不再命中任何一条 WHEN，重跑无副作用。
UPDATE plans SET event_type = CASE event_type
    WHEN '初级'       THEN '初级争霸赛小组赛'
    WHEN '中级争霸赛' THEN '中级争霸赛小组赛'
    WHEN '月底争霸赛' THEN '月终争霸赛'
    WHEN '初级晋级赛' THEN '初级争霸赛晋级赛'
    WHEN '中级晋级赛' THEN '中级争霸赛晋级赛'
    WHEN '晋级赛'     THEN '初级争霸赛晋级赛'
    ELSE event_type
END
WHERE event_type IN ('初级','中级争霸赛','月底争霸赛','初级晋级赛','中级晋级赛','晋级赛');
