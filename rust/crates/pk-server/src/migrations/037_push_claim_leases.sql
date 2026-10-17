-- 037: 推送去重「租约」表（2026-10-17 第四轮审计 B 域修复：P2-1 / P2-2 / P2-3）。
--
-- 背景（为什么还需要这张表，第三轮的 claim/restore 不够）：
-- 1) 第三轮用「条件 UPDATE 占位 + 失败还原」解决了同秒并发双推，但**没有解决崩溃**：
--    占位成功后进程崩溃 / 被 kill，标记已经被消耗、消息却没发出去，当天再也不会补推。
-- 2) 多实例 / 手动「立即推送」与调度循环并发时，还原语义只写回「占位前读到的旧值」，
--    没有地方记录「这个占位是谁占的」，无法在进程重启后判断该不该还原。
--
-- 本表把「谁占了哪个去重标记、占位前的旧值」持久化：
--   kind = report → push_bindings.last_report_date
--   kind = lineage → push_bindings.last_lineage_date
--   kind = target → 纯互斥锁（追分提醒目标的去重标记在 my_targets 逐行，不在此表写）
--   kind = run    → run() 单飞锁（name = team:date，不写任何标记）
-- prev_value 保存占位前的值（默认空串 = 之前没推过），用于发送失败 / 过期租约的还原。
-- 发送成功 → 删租约（标记保持新值）；失败 / 被闸拦截 → 写回 prev_value 再删租约。
-- PK (kind, name) 是互斥的根：同一 (kind, name) 同时只可能有一行，INSERT 撞键即「已被占」。
--
-- 幂等：CREATE TABLE IF NOT EXISTS / CREATE INDEX IF NOT EXISTS，可安全重放。
-- 清理：租约超过 15 分钟视为持有方崩溃，由 push::recover_stale_leases 按上述语义收尾。
CREATE TABLE IF NOT EXISTS push_claim_leases (
    kind       VARCHAR(16)  NOT NULL,
    name       VARCHAR(128) NOT NULL,
    prev_value VARCHAR(32)  NOT NULL DEFAULT '',
    claimed_at TIMESTAMPTZ  NOT NULL DEFAULT now(),
    PRIMARY KEY (kind, name)
);

-- 过期租约扫描用（WHERE claimed_at < now() - 15min）；幂等。
CREATE INDEX IF NOT EXISTS idx_push_claim_leases_claimed
    ON push_claim_leases (claimed_at);
