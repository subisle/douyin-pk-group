-- 023 · 管理员消息下发支持自定义上传图片（task-14；格式口径 task-15 收窄为只收 PNG）
--
-- 背景：022 的 admin_messages 只有 image_kind（服务端渲染图：lineage/daily/month），
-- 没有放「管理员自己上传的图」的地方，所以「消息下发」页发不了图。
-- 这里加三列（不用新表：一图对一消息，放主档一起去重/清理都简单）：
--   image_blob  原始图片字节（入口校验：解码后 ≤5MB、单边 ≤4096px、总像素 ≤1600 万；
--               2026-10-15 用户口径「只留 PNG 这一种格式即可」，所以只收 PNG）
--   image_mime  按**文件魔数**判出来的 MIME；只收 PNG 故恒为 'image/png'，空串 = 没有上传图
--   image_name  上传时的原始文件名（可选，≤128 字，仅供后台展示/审计）
--
-- 注意：这三列**有意不放进 list/detail 的 JSON 投影**（只有 imageMime / imageBytes /
-- hasImage 这类小字段），避免 5MB 的 BYTEA 被 base64 塞进列表响应；取原图只走
-- store::admin_message_image() 这一条按 id 的单列 SELECT。
--
-- 幂等：全部 IF NOT EXISTS。本文件若在「DDL 已生效但 schema_migrations 未登记」
-- 的场景下被重放（例如先在 psql 里手工验证过），也不会报错。

ALTER TABLE admin_messages ADD COLUMN IF NOT EXISTS image_blob BYTEA;
ALTER TABLE admin_messages ADD COLUMN IF NOT EXISTS image_mime VARCHAR(32) NOT NULL DEFAULT '';
ALTER TABLE admin_messages ADD COLUMN IF NOT EXISTS image_name VARCHAR(128) NOT NULL DEFAULT '';
