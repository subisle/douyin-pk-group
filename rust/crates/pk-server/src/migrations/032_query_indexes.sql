-- 032: 查询索引补齐 + 重复索引清理（2026-10-17 数据库架构优化）
--
-- 背景：上线前做了一轮读路径排查，重点看「每张榜单/每个页面每次打开都要跑」的查询。
-- 结论：**不新增表、不改列、不动任何读写语义**，只补索引与清重复 ——
-- 这类改动对应用侧完全透明，出问题只需 DROP INDEX 即可回滚。
--
-- 全部用 IF NOT EXISTS / IF EXISTS，可重复执行。

-- ---------------------------------------------------------------------------
-- ① daily_metric：榜单聚合的主表
-- ---------------------------------------------------------------------------
-- 现状：只有 idx_daily_date(biz_date) 与 uk_person_date(person_id, biz_date)。
-- 而日榜 / 月榜 / 年榜的聚合**都带** `wave_reliable = 1` 这个条件
-- （dialect.rs 的 period_rollup_sql、WAVES_A、push 的各条月查询都是），
-- 缺以它为前缀的索引 → 每次聚合都要先捞出该人的全部历史行再过滤。
-- 补 (person_id, biz_date, wave_reliable)：把「可靠行」直接钉在索引里，
-- 按人按月的 SUM 可以直接走索引扫描。
CREATE INDEX IF NOT EXISTS idx_daily_person_date_reliable
    ON daily_metric (person_id, biz_date, wave_reliable);

-- 按业务日捞全团（日榜 / 缺数据 / 导入状态）时带可靠标记，同上理由。
CREATE INDEX IF NOT EXISTS idx_daily_date_reliable
    ON daily_metric (biz_date, wave_reliable);

-- ---------------------------------------------------------------------------
-- ② person：榜单的过滤三件套
-- ---------------------------------------------------------------------------
-- 所有榜单 SQL 的公共尾部都是
--   WHERE p.deleted_at IS NULL AND p.gender = ? AND p.status <> 'left'
--     AND p.hide_in_daily_report = 0
-- 现有 idx_status_gender(status, gender) 不含 deleted_at，软删的行也要扫进来再丢。
-- 补一个覆盖常用过滤组合的复合索引。
CREATE INDEX IF NOT EXISTS idx_person_list_scope
    ON person (gender, status, hide_in_daily_report)
    WHERE deleted_at IS NULL;

-- 师门 / 族谱按师傅查门下（person.master_id），且要排除已删与离团。
CREATE INDEX IF NOT EXISTS idx_person_master_live
    ON person (master_id)
    WHERE deleted_at IS NULL;

-- ---------------------------------------------------------------------------
-- ③ person 按姓名查（改名同步 / 登录反查 / 台账搜索）
-- ---------------------------------------------------------------------------
-- ⚠️ 001_core.sql 里 idx_name 被**定义了两次**（258 / 260 行，同名同列）：
-- `CREATE INDEX IF NOT EXISTS` 只认名字，第二次静默跳过 —— 不影响正确性，
-- 库里实际只有一个。但它是同源残留，读迁移的人会误以为有两个索引。
-- 这里**不去 DROP**（迁移脚本的语句白名单不接受 DROP，见 migrate.rs 的单测），
-- 改为在 001 里删掉重复那一行（见 001 的注释），本迁移只保证索引存在。
CREATE INDEX IF NOT EXISTS idx_name ON person (name);

-- ---------------------------------------------------------------------------
-- ④ 分号口径：account 按人查号
-- ---------------------------------------------------------------------------
-- 「我的抖音号」卡 / 推送分号明细段按 (person_id, is_primary) 取该人的号，
-- 补复合索引避免每次回表排序。
CREATE INDEX IF NOT EXISTS idx_account_person_primary
    ON account (person_id, is_primary);

-- ---------------------------------------------------------------------------
-- ⑤ 方案列表：按赛事类型 + 发布时间倒序
-- ---------------------------------------------------------------------------
-- PK 分组页「方案列表」按 eventType 过滤后按时间倒序取，补复合索引。
-- （030 已把老方案的 event_type 归一成新值，这个索引才有效果）
CREATE INDEX IF NOT EXISTS idx_plans_event_created
    ON plans (event_type, created_at DESC);
