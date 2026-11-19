-- 055 · 直播监控公开分享与 10 分钟小组 / 5 分钟复活赛（2026-11-19）
--
-- 每次在管理端启动赛事时生成一个不可猜测的 public_token，对外页面只凭 token
-- 读取赛事运行态。token 不包含数据库 id，也不复用管理员凭证；重新启动新赛事
-- 会得到新链接，旧赛事链接仍可查看自己的最终结果。
--
-- 时长只用于公开页倒计时和赛程展示；真实结束仍以抖音 battle 的完整时长为准，
-- 避免网络抖动 / 提前断播被倒计时误判为有效成绩。

ALTER TABLE live_event
  ADD COLUMN IF NOT EXISTS public_token VARCHAR(40) NOT NULL DEFAULT '',
  ADD COLUMN IF NOT EXISTS started_at TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS group_duration_secs INTEGER NOT NULL DEFAULT 600,
  ADD COLUMN IF NOT EXISTS revival_duration_secs INTEGER NOT NULL DEFAULT 300;

-- 2026-11-19 赛制口径：每组前 4 名晋级 / 复活赛前 4 名复活。只改默认值，
-- 已创建赛事保留自己的 rule_version 快照，不追溯改历史配置。
ALTER TABLE live_event ALTER COLUMN direct_qualify_per_group SET DEFAULT 4;
ALTER TABLE live_event ALTER COLUMN revival_qualify_per_room SET DEFAULT 4;

CREATE UNIQUE INDEX IF NOT EXISTS uk_live_event_public_token
  ON live_event (public_token)
  WHERE public_token <> '';

CREATE INDEX IF NOT EXISTS idx_live_event_public_started
  ON live_event (started_at DESC)
  WHERE public_token <> '';
