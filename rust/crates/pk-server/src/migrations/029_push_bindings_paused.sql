-- 029：解绑 = 停止数据传输（2026-10-17 用户口径）。
--
-- 展示端「解除绑定」（store::push_unbind）除了清 push_bindings 的会话接线
-- （conversation_id / to_user_id / bound_at / bind_code / code_expires），还要把本列 paused 置 1：
-- - wxpush::supervisor_loop 的补拉（accounts_to_start）跳过 paused=1 的账号，不再拉起；
-- - wxpush::poll_body 每轮 wx_push_account_get 时发现 paused=1 即记 info 日志并 return，
--   解绑后最多一个轮询周期内长轮询任务退出，该账号不再收发任何数据；
-- - 重新对接（push_bind_account 激活 / push_bind_by_code 发码绑定 / 重扫路径）把 paused
--   置回 0 = 恢复，supervisor 下一轮（≤30s）自动重新拉起长轮询。
-- wx_push_accounts 表不加列（解绑保留扫码凭证），paused 只挂 push_bindings；
-- 读侧（wx_push_account_get / wx_push_accounts_all）LEFT JOIN 本表取 paused，
-- 没有绑定行的账号按未暂停（0）处理。
ALTER TABLE push_bindings ADD COLUMN IF NOT EXISTS paused SMALLINT NOT NULL DEFAULT 0;

-- 每日报告默认开启（2026-10-17 用户口径）：新对接的用户 daily_report 默认 1。
-- 只改列默认值，不动存量行；push_bind_account / push_set_code 的
-- INSERT ... ON CONFLICT DO UPDATE 都不写这一列，已对接用户自己改过的开关不受影响。
-- 追分提醒 notify_target、师门日报 lineage_report 保持默认关（0），刻意不动。
ALTER TABLE push_bindings ALTER COLUMN daily_report SET DEFAULT 1;
