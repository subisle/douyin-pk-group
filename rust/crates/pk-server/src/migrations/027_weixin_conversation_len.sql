-- 027: 微信 iLink 会话 key 放不下 —— conversation_id 64 → 191。
--
-- 2026-10-16 用户报障「二维码绑定有问题」的根因：
-- 扫码账号的会话 key 形如 `wxacct:{bot_id}:{user_id}`，实测
--   wxacct:9558d8586108@im.bot:o9cq808v597xaUoY0c-VHGRBHoDk@im.wechat = 65 字符
-- 而 wx_sessions / push_bindings / bot_sessions 的 conversation_id 都是 VARCHAR(64)：
--   · wx_sessions 写上下文直接报 `value too long for type character varying(64)`
--     → 主动推送取不到 context_token；
--   · push_bindings 的写入是 `let _ =`（静默）→ 「已绑定」状态永远落不下去，
--     展示端「我的」页一直显示未绑定 / 让你再扫一次（只有 wx_push_accounts 那列是
--     VARCHAR(191)，所以凭证看着是存上了，状态却对不上）。
--
-- 与 wx_push_accounts.conversation_id 对齐取 191（同一套 key、同一份口径）。
-- ALTER TYPE 是幂等的（写宽只会 no-op），可安全重放。
ALTER TABLE wx_sessions    ALTER COLUMN conversation_id TYPE varchar(191);
ALTER TABLE push_bindings  ALTER COLUMN conversation_id TYPE varchar(191);
ALTER TABLE bot_sessions   ALTER COLUMN conversation_id TYPE varchar(191);
