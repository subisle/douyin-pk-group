-- 025 · 主播抖音头像本地留存（person_avatar）
--
-- 背景（2026-10-16 用户口径）：
--   1. 展示端主界面「主播列表」要显示主播的**抖音真实头像**；
--   2. **已经注销的账号不用取头像**；
--   3. **头像要留在本地** —— 抖音那个页面接口是非官方契约，以后可能改/封，
--      留一份本地副本 + 原图 URL，接口哪天取不到了页面照样有图；
--   4. **每 7 天刷一遍** —— 刷新判定靠下面的 synced_at，不靠外部 cron。
--
-- 为什么不写进 person.avatar_url、也不落文件系统：
--   - person 是榜单/台账每查必读的表，百来行 3KB 的图塞进去没必要；同 023 对
--     admin_messages 图片的处理口径 —— 图片单独存，列表/详情只投影小字段；
--   - 文件系统的副本不随库走（换机/恢复/迁移就丢），本项目的事实源是 PG。
--
-- 列语义：
--   image       头像 JPEG 原图（抖音 CDN 的 200x200 档，约 3KB）。NULL = 没有可用头像
--               （账号已注销 / 抖音号查不到 / 没登记抖音号）。
--   mime        恒为 image/jpeg（入口按文件魔数校验，非 JPEG 一律不收）
--   source_url  抖音 CDN 原图 URL，留它是为了接口失效后还能人工回溯/重抓
--   hash        image 内容的前 16 位 md5(hex)；展示端拼成 `?v=<hash>` 破缓存，
--               图片一变 URL 就变，因此可以放心长缓存
--   note        探测结论：'' 正常 / 'deregistered' 已注销 / 'not_found' 号查不到
--               / 'no_douyin_no' 没登记抖音号 / 'bad_response' 返回的不是图片
--   synced_at   上次探测时间（"查不到 / 已注销"也算探测过）：7 天刷新靠它判定，
--               否则每次启动都会去戳一遍抖音
--
-- 不加 FK 约束：与 account / daily_metric 一样是「真外键语义」，由服务层维护
-- （docs/12 的外键口径），避免历史脏数据把整表写入挡住。
--
-- 幂等：CREATE TABLE IF NOT EXISTS / CREATE INDEX IF NOT EXISTS，可安全重放。

CREATE TABLE IF NOT EXISTS person_avatar (
  person_id BIGINT PRIMARY KEY,
  image BYTEA,
  mime VARCHAR(32) NOT NULL DEFAULT 'image/jpeg',
  source_url VARCHAR(512) NOT NULL DEFAULT '',
  hash VARCHAR(16) NOT NULL DEFAULT '',
  note VARCHAR(32) NOT NULL DEFAULT '',
  synced_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_person_avatar_synced ON person_avatar (synced_at);
