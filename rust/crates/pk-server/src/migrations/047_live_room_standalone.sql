-- 047 · 独立直播间监控（2026-11-09）
--
-- 这一层只保存「直播间当前正在打的 PK」的原始采样，不关联赛事、小组或晋级。
-- 采集器可用 `--room` 单独启动，后续再把同一 battle 接回分组，互不影响。

CREATE TABLE IF NOT EXISTS live_room_monitor (
  web_rid VARCHAR(40) PRIMARY KEY,
  room_id VARCHAR(40) NOT NULL DEFAULT '',
  label VARCHAR(128) NOT NULL DEFAULT '',
  health VARCHAR(16) NOT NULL DEFAULT 'idle',
  last_error VARCHAR(255) NOT NULL DEFAULT '',
  last_battle_id VARCHAR(40) NOT NULL DEFAULT '',
  last_seen_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS live_room_score_sample (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  web_rid VARCHAR(40) NOT NULL,
  room_id VARCHAR(40) NOT NULL DEFAULT '',
  battle_id VARCHAR(40) NOT NULL DEFAULT '',
  douyin_uid VARCHAR(40) NOT NULL DEFAULT '',
  nickname VARCHAR(128) NOT NULL DEFAULT '',
  team_id VARCHAR(40) NOT NULL DEFAULT '',
  score BIGINT NOT NULL DEFAULT 0,
  rank INTEGER NOT NULL DEFAULT 0,
  team_score BIGINT NOT NULL DEFAULT 0,
  team_rank INTEGER NOT NULL DEFAULT 0,
  score_version VARCHAR(40) NOT NULL DEFAULT '',
  bonus_multiple INTEGER NOT NULL DEFAULT 100,
  bonus_score BIGINT NOT NULL DEFAULT 0,
  bonus_active SMALLINT NOT NULL DEFAULT 0,
  bonus_end_ms BIGINT NOT NULL DEFAULT 0,
  current_bonus BIGINT NOT NULL DEFAULT 0,
  observed_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);

CREATE INDEX IF NOT EXISTS idx_live_room_sample_latest
  ON live_room_score_sample (web_rid, battle_id, observed_at DESC);
CREATE INDEX IF NOT EXISTS idx_live_room_monitor_seen
  ON live_room_monitor (last_seen_at DESC);
