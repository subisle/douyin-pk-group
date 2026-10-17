-- 036: 对接码加固 + 035 存量回填（2026-10-17 第四轮审计 P1-2 / P2-10 / P2-11）。
--
-- ① bind_code_fails.code 从 VARCHAR(16) 放宽到 VARCHAR(160)：
--    后续会话维度失败计数（A3 会话锁）要往同一张表写 "conv:<会话 id>" 这类键，
--    会话 id 实测 65 字符（027 已把 conversations 系列放宽到 191），16 位放不下。
--    ALTER COLUMN TYPE 对同一类型重复执行不报错（幂等）；本表只有哨兵行 + 近期
--    错码，重写代价可忽略。若历史数据里已有超长键，这一步会先报错再人工确认，
--    不会静默截断丢数据。
--
-- ② 回填 035 的存量标记：035 只加了 legacy_no_uid 列，存量「双空无主行」要等
--    下一次扫码 save 才打标；那段窗口里激活兼容分支仍会把它们当普通行放行。
--    这里一次性回填：ilink_user_id 与 conversation_id 双空的存量行全标为
--    legacy_no_uid=1。幂等：只动 legacy_no_uid=0 的行，重复执行 affected=0。
--
-- 不可逆点：① 之后再缩回 VARCHAR(16) 会因子串超长失败，属有意保留的单向放宽
-- （会话键只会更长）；② 回填后无法再区分「035 之前的历史无主行」与「035 修复后
-- 误入的行」——但扫码确认侧已对 user_id 缺失直接拒绝保存，正常不会再产生双空行。

ALTER TABLE bind_code_fails
    ALTER COLUMN code TYPE VARCHAR(160);

UPDATE wx_push_accounts
   SET legacy_no_uid = 1
 WHERE ilink_user_id = ''
   AND conversation_id = ''
   AND legacy_no_uid = 0;
