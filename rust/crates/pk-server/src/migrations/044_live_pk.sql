-- 044 · 直播 PK 实时监控与赛事运行态（2026-10-20）
--
-- 为什么单独一组表：分组方案 `plans` 是**赛前快照**（payload 里存名单与分组卡），
-- 实时分数每 3~5 秒刷新一次，绝不能写进 payload（会覆盖运营的分组、撑爆 TEXT）。
-- 这里只存「赛事运行态」：一场赛事 -> 若干阶段 -> 若干小组 -> 每个 battle_id
-- 一局 -> 每局若干席位 -> 逐次采样 -> 不可变时间线 -> 冻结的晋级结果。
--
-- 与现有表的关系：
--   live_event.day_plan_id / night_plan_id -> plans.id（赛前分组快照，只读引用）
--   live_round_seat.person_id              -> person.id（抖音 UID 认人后的台账主播）
--   live_monitor_room.web_rid              -> 直播间 URL 里的 505899768449 这类短 id
--   推送复用 push_bindings.live_pk_report（本迁移加列），发送仍走 push.rs 统一闸门
--
-- 字符串 id 纪律：抖音的 battle_id / room_id / UID 都是 19 位，**超过 JS 安全整数**，
-- 全链路一律 VARCHAR。任何转成 i64/f64 的写法都会静默丢精度（实测 7698405275833177097
-- 在 JSON.parse 后末位变成 700）。
--
-- 幂等：全部 CREATE TABLE / INDEX IF NOT EXISTS，可安全重放。

CREATE TABLE IF NOT EXISTS live_event (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  title VARCHAR(128) NOT NULL DEFAULT '',
  event_date VARCHAR(16) NOT NULL DEFAULT '',
  status VARCHAR(24) NOT NULL DEFAULT 'draft',
  day_plan_id BIGINT NOT NULL DEFAULT 0,
  night_plan_id BIGINT NOT NULL DEFAULT 0,
  rule_version INTEGER NOT NULL DEFAULT 1,
  -- 规则快照（可后台改；改了只影响未开始的阶段，已定稿结果不动）
  first_wave_group_count INTEGER NOT NULL DEFAULT 4,
  group_rounds_per_group INTEGER NOT NULL DEFAULT 1,
  direct_qualify_per_group INTEGER NOT NULL DEFAULT 4,
  revival_room_capacity INTEGER NOT NULL DEFAULT 8,
  revival_qualify_per_room INTEGER NOT NULL DEFAULT 3,
  promotion_group_count INTEGER NOT NULL DEFAULT 0,
  promotion_group_size INTEGER NOT NULL DEFAULT 8,
  promotion_qualify_per_group INTEGER NOT NULL DEFAULT 1,
  final_room_capacity INTEGER NOT NULL DEFAULT 8,
  settle_grace_secs INTEGER NOT NULL DEFAULT 60,
  -- 推送（第二层；第一层仍是 PUSH_OUT_ENABLED）
  push_enabled SMALLINT NOT NULL DEFAULT 1,
  push_interval_secs INTEGER NOT NULL DEFAULT 60,
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_live_event_date ON live_event (event_date, id DESC);

-- 规则修订审计：谁在什么时候把「每组晋级 4 人」改成了 3 人（只记录，不回算历史）
CREATE TABLE IF NOT EXISTS live_event_rule (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  event_id BIGINT NOT NULL,
  version INTEGER NOT NULL,
  field VARCHAR(48) NOT NULL,
  old_value VARCHAR(64) NOT NULL DEFAULT '',
  new_value VARCHAR(64) NOT NULL DEFAULT '',
  actor VARCHAR(64) NOT NULL DEFAULT '',
  reason VARCHAR(255) NOT NULL DEFAULT '',
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_live_event_rule_event ON live_event_rule (event_id, version DESC);

-- 阶段：第一波小组 / 第一波复活 / 剩余小组 / 剩余复活 / 晋级赛 / 决赛
CREATE TABLE IF NOT EXISTS live_stage (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  event_id BIGINT NOT NULL,
  kind VARCHAR(24) NOT NULL,
  seq INTEGER NOT NULL DEFAULT 0,
  status VARCHAR(24) NOT NULL DEFAULT 'pending',
  started_at TIMESTAMPTZ,
  finished_at TIMESTAMPTZ,
  PRIMARY KEY (id)
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_live_stage ON live_stage (event_id, kind);

-- 实际比赛小组（小组赛来自 plan 分组；复活/晋级/决赛由本系统生成）
CREATE TABLE IF NOT EXISTS live_group (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  event_id BIGINT NOT NULL,
  stage_id BIGINT NOT NULL,
  kind VARCHAR(24) NOT NULL,
  seq INTEGER NOT NULL DEFAULT 0,
  name VARCHAR(64) NOT NULL DEFAULT '',
  status VARCHAR(24) NOT NULL DEFAULT 'pending',
  qualify_slots INTEGER NOT NULL DEFAULT 0,
  source_group_id BIGINT NOT NULL DEFAULT 0,
  official_round_id BIGINT NOT NULL DEFAULT 0,
  finalized_at TIMESTAMPTZ,
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_live_group_stage ON live_group (stage_id, seq);

-- 每一局：一个 battle_id 一行（异常重开是**新行**，永不覆盖）
CREATE TABLE IF NOT EXISTS live_round (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  event_id BIGINT NOT NULL,
  group_id BIGINT NOT NULL,
  battle_id VARCHAR(40) NOT NULL DEFAULT '',
  channel_id VARCHAR(40) NOT NULL DEFAULT '',
  room_web_rid VARCHAR(40) NOT NULL DEFAULT '',
  round_kind VARCHAR(16) NOT NULL DEFAULT 'normal',
  attempt_no INTEGER NOT NULL DEFAULT 1,
  parent_round_id BIGINT NOT NULL DEFAULT 0,
  roster_signature VARCHAR(255) NOT NULL DEFAULT '',
  start_at TIMESTAMPTZ,
  expected_end_at TIMESTAMPTZ,
  actual_end_at TIMESTAMPTZ,
  end_reason VARCHAR(32) NOT NULL DEFAULT '',
  status VARCHAR(24) NOT NULL DEFAULT 'detected',
  is_official SMALLINT NOT NULL DEFAULT 0,
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_live_round_battle ON live_round (battle_id, group_id);
-- 一个小组最多一场正式局（DB 级兜底，防止并发上报把两局都设成 official）
CREATE UNIQUE INDEX IF NOT EXISTS uk_live_round_official ON live_round (group_id)
  WHERE is_official = 1;
CREATE INDEX IF NOT EXISTS idx_live_round_group ON live_round (group_id, attempt_no);

-- 席位：这一局里谁在打、打了多少分
CREATE TABLE IF NOT EXISTS live_round_seat (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  round_id BIGINT NOT NULL,
  person_id BIGINT NOT NULL DEFAULT 0,
  douyin_uid VARCHAR(40) NOT NULL DEFAULT '',
  nickname VARCHAR(128) NOT NULL DEFAULT '',
  team_id VARCHAR(40) NOT NULL DEFAULT '',
  final_score bigint NOT NULL DEFAULT 0,
  final_rank INTEGER NOT NULL DEFAULT 0,
  qualified SMALLINT NOT NULL DEFAULT 0,
  PRIMARY KEY (id)
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_live_seat ON live_round_seat (round_id, douyin_uid);
CREATE INDEX IF NOT EXISTS idx_live_seat_person ON live_round_seat (person_id);

-- 小组参赛名单（赛前生成，开打前的「谁要上场」）：对局还没产生时也要能显示名单
CREATE TABLE IF NOT EXISTS live_group_seat (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  group_id BIGINT NOT NULL,
  person_id BIGINT NOT NULL DEFAULT 0,
  douyin_uid VARCHAR(40) NOT NULL DEFAULT '',
  nickname VARCHAR(128) NOT NULL DEFAULT '',
  seed_order INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (id)
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_live_group_seat ON live_group_seat (group_id, douyin_uid);

-- 分数采样（90 天后清理；对局结果与时间线永久保留）
CREATE TABLE IF NOT EXISTS live_score_sample (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  round_id BIGINT NOT NULL,
  douyin_uid VARCHAR(40) NOT NULL DEFAULT '',
  score bigint NOT NULL DEFAULT 0,
  rank INTEGER NOT NULL DEFAULT 0,
  team_score bigint NOT NULL DEFAULT 0,
  team_rank INTEGER NOT NULL DEFAULT 0,
  score_version VARCHAR(40) NOT NULL DEFAULT '',
  observed_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_live_sample_round ON live_score_sample (round_id, observed_at DESC);

-- 时间线：开局 / 掉线 / 提前结束 / 重开 / 定稿，只追加不修改
CREATE TABLE IF NOT EXISTS live_round_event (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  round_id BIGINT NOT NULL,
  kind VARCHAR(32) NOT NULL DEFAULT '',
  detail VARCHAR(255) NOT NULL DEFAULT '',
  actor VARCHAR(64) NOT NULL DEFAULT '',
  at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_live_round_event ON live_round_event (round_id, at);

-- 冻结的晋级结果（直接晋级 / 复活晋级 / 晋级赛出线 / 进入决赛）
CREATE TABLE IF NOT EXISTS live_qualifier (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  event_id BIGINT NOT NULL,
  stage_id BIGINT NOT NULL,
  group_id BIGINT NOT NULL DEFAULT 0,
  round_id BIGINT NOT NULL DEFAULT 0,
  person_id BIGINT NOT NULL DEFAULT 0,
  douyin_uid VARCHAR(40) NOT NULL DEFAULT '',
  source VARCHAR(24) NOT NULL DEFAULT '',
  rank INTEGER NOT NULL DEFAULT 0,
  score bigint NOT NULL DEFAULT 0,
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_live_qualifier
  ON live_qualifier (event_id, source, stage_id, group_id, douyin_uid);

-- 被监控的直播间：一个小组一场 PK，主办方直播间只有一个（505899768449 这类）
CREATE TABLE IF NOT EXISTS live_monitor_room (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  event_id BIGINT NOT NULL,
  web_rid VARCHAR(40) NOT NULL DEFAULT '',
  room_id VARCHAR(40) NOT NULL DEFAULT '',
  url VARCHAR(512) NOT NULL DEFAULT '',
  label VARCHAR(128) NOT NULL DEFAULT '',
  enabled SMALLINT NOT NULL DEFAULT 1,
  health VARCHAR(24) NOT NULL DEFAULT 'offline',
  last_seen_at TIMESTAMPTZ,
  last_error VARCHAR(255) NOT NULL DEFAULT '',
  PRIMARY KEY (id)
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_live_monitor_room ON live_monitor_room (event_id, web_rid);

-- 采集进程心跳（多房间 / 重启排查用）
CREATE TABLE IF NOT EXISTS live_collector_worker (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  worker_id VARCHAR(64) NOT NULL,
  version VARCHAR(32) NOT NULL DEFAULT '',
  rooms INTEGER NOT NULL DEFAULT 0,
  last_heartbeat TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (worker_id)
);

-- 推送作业：周期/事件推送的去重、冷却、重试
CREATE TABLE IF NOT EXISTS live_push_job (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  event_id BIGINT NOT NULL,
  round_id BIGINT NOT NULL DEFAULT 0,
  person_id BIGINT NOT NULL DEFAULT 0,
  kind VARCHAR(32) NOT NULL DEFAULT '',
  dedup_key VARCHAR(128) NOT NULL DEFAULT '',
  status VARCHAR(16) NOT NULL DEFAULT 'pending',
  run_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  sent_at TIMESTAMPTZ,
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error VARCHAR(255) NOT NULL DEFAULT '',
  PRIMARY KEY (id)
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_live_push_job ON live_push_job (dedup_key);
CREATE INDEX IF NOT EXISTS idx_live_push_job_run ON live_push_job (status, run_at);

-- 主播侧的直播推送开关（第三层；前两层是 PUSH_OUT_ENABLED 与 live_event.push_enabled）
ALTER TABLE push_bindings ADD COLUMN IF NOT EXISTS live_pk_report SMALLINT NOT NULL DEFAULT 0;

DROP TRIGGER IF EXISTS trg_touch_live_event ON live_event;
CREATE TRIGGER trg_touch_live_event BEFORE UPDATE ON live_event
  FOR EACH ROW EXECUTE FUNCTION touch_updated_at();
DROP TRIGGER IF EXISTS trg_touch_live_round ON live_round;
CREATE TRIGGER trg_touch_live_round BEFORE UPDATE ON live_round
  FOR EACH ROW EXECUTE FUNCTION touch_updated_at();
