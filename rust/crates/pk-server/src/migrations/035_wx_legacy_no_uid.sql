-- 035: 扫码无主凭证收紧（2026-10-17 审计 4-P2-2，承第二轮 P2-2）。
--
-- 背景：扫码确认时上游偶发不下发 userId，原先「warn 后继续保存」——新扫码会落
-- ilink_user_id 与 conversation_id 双空的「无主凭证」，激活侧 activation_owner_ok
-- 的 None 兼容分支会放行「第一个私聊到 bot 的人」→ 冒认激活、收走主播全部推送。
--
-- 修法：加 legacy_no_uid 列标记「改动前已存在的历史无主行」。激活兼容分支只对
-- legacy_no_uid=1 的行保留；新扫码不再产生可被冒认激活的行（wx.rs 扫码确认侧
-- 对 user_id 缺失直接拒绝保存）。存量双空行在 save 时自动打标。

ALTER TABLE wx_push_accounts
    ADD COLUMN IF NOT EXISTS legacy_no_uid SMALLINT NOT NULL DEFAULT 0;
