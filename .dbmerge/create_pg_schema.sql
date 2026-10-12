
-- 抖音 PK 分组核心表（PostgreSQL 版）
CREATE TABLE person (
  id BIGSERIAL PRIMARY KEY,
  name VARCHAR(64) NOT NULL DEFAULT '',
  gender VARCHAR(16) NOT NULL DEFAULT 'unknown' CHECK (gender IN ('male','female','unknown')),
  master_id BIGINT REFERENCES person(id),
  generation INT,
  group_name VARCHAR(64),
  avatar_url VARCHAR(512),
  hide_in_daily_report BOOLEAN NOT NULL DEFAULT false,
  status VARCHAR(16) NOT NULL DEFAULT 'active' CHECK (status IN ('active','left','paused')),
  joined_at DATE,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  deleted_at TIMESTAMP
);
CREATE INDEX idx_person_master ON person(master_id);
CREATE INDEX idx_person_name ON person(name);

CREATE TABLE account (
  id BIGSERIAL PRIMARY KEY,
  person_id BIGINT NOT NULL REFERENCES person(id),
  anchor_id VARCHAR(64) NOT NULL UNIQUE,
  douyin_no VARCHAR(64) NOT NULL DEFAULT '',
  anchor_name VARCHAR(128) NOT NULL DEFAULT '',
  is_primary BOOLEAN NOT NULL DEFAULT false,
  status VARCHAR(16) NOT NULL DEFAULT 'active' CHECK (status IN ('active','disabled')),
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX idx_account_person ON account(person_id);

CREATE TABLE daily_metric (
  id BIGSERIAL PRIMARY KEY,
  person_id BIGINT NOT NULL REFERENCES person(id),
  anchor_id VARCHAR(64) NOT NULL,
  biz_date DATE NOT NULL,
  wave BIGINT NOT NULL DEFAULT 0,
  cumulative_wave BIGINT NOT NULL DEFAULT 0,
  prev_snapshot_date DATE,
  wave_span SMALLINT NOT NULL DEFAULT 1,
  wave_reliable BOOLEAN NOT NULL DEFAULT true,
  minutes INT NOT NULL DEFAULT 0,
  cumulative_minutes INT NOT NULL DEFAULT 0,
  minutes_span SMALLINT NOT NULL DEFAULT 1,
  minutes_reliable BOOLEAN NOT NULL DEFAULT true,
  is_live BOOLEAN NOT NULL DEFAULT false,
  tier CHAR(1),
  source_batch_id BIGINT,
  recomputed_at TIMESTAMP,
  UNIQUE (person_id, biz_date)
);
CREATE INDEX idx_daily_date ON daily_metric(biz_date);

CREATE TABLE import_batch (
  id BIGSERIAL PRIMARY KEY,
  source VARCHAR(32) NOT NULL,
  filename VARCHAR(255) NOT NULL,
  biz_date DATE,
  team VARCHAR(16),
  imported_rows INT NOT NULL DEFAULT 0,
  ok_rows INT NOT NULL DEFAULT 0,
  failed_rows INT NOT NULL DEFAULT 0,
  note VARCHAR(512),
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE wave_snapshot (
  id BIGSERIAL PRIMARY KEY,
  anchor_id VARCHAR(64) NOT NULL,
  biz_date DATE NOT NULL,
  wave_value BIGINT NOT NULL DEFAULT 0,
  rank_in_guild INT,
  source_batch_id BIGINT,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (anchor_id, biz_date)
);

CREATE TABLE monthly_metric (
  id BIGSERIAL PRIMARY KEY,
  person_id BIGINT NOT NULL REFERENCES person(id),
  period CHAR(7) NOT NULL,
  wave BIGINT NOT NULL DEFAULT 0,
  live_days INT NOT NULL DEFAULT 0,
  best_day_wave BIGINT NOT NULL DEFAULT 0,
  best_day_date DATE,
  source_batch_id BIGINT,
  recomputed_at TIMESTAMP,
  UNIQUE (person_id, period)
);

CREATE TABLE my_accounts (
  name VARCHAR(64) PRIMARY KEY,
  phone VARCHAR(24) NOT NULL DEFAULT '',
  pass VARCHAR(255) NOT NULL DEFAULT '',
  is_admin BOOLEAN NOT NULL DEFAULT false,
  created_at VARCHAR(40) NOT NULL,
  bound BOOLEAN NOT NULL DEFAULT true,
  bound_at VARCHAR(40) NOT NULL DEFAULT '',
  source VARCHAR(16) NOT NULL DEFAULT 'admin',
  wb_state VARCHAR(64) NOT NULL DEFAULT '',
  phone_uniq VARCHAR(24) UNIQUE GENERATED ALWAYS AS (NULLIF(phone,'')) STORED
);

CREATE TABLE my_targets (
  id BIGSERIAL PRIMARY KEY,
  name VARCHAR(64) NOT NULL,
  team VARCHAR(16) NOT NULL DEFAULT '',
  kind VARCHAR(16) NOT NULL DEFAULT 'person',
  target_person_id BIGINT NOT NULL DEFAULT 0,
  target_name VARCHAR(64) NOT NULL DEFAULT '',
  target_value BIGINT NOT NULL DEFAULT 0,
  hit BOOLEAN NOT NULL DEFAULT false,
  last_gap BIGINT NOT NULL DEFAULT 0,
  last_notice_date VARCHAR(16) NOT NULL DEFAULT '',
  created_at VARCHAR(40) NOT NULL
);
CREATE INDEX idx_my_targets_name ON my_targets(name);

CREATE TABLE wx_push_accounts (
  name VARCHAR(64) PRIMARY KEY,
  team VARCHAR(16) NOT NULL DEFAULT '',
  person_id BIGINT NOT NULL DEFAULT 0,
  ilink_bot_id VARCHAR(128) NOT NULL DEFAULT '',
  ilink_user_id VARCHAR(128) NOT NULL DEFAULT '',
  bot_token VARCHAR(255) NOT NULL DEFAULT '',
  base_url VARCHAR(255) NOT NULL DEFAULT '',
  nickname VARCHAR(64) NOT NULL DEFAULT '',
  conversation_id VARCHAR(191) NOT NULL DEFAULT '',
  updates_buf VARCHAR(255) NOT NULL DEFAULT '',
  bound_at VARCHAR(40) NOT NULL DEFAULT '',
  updated_at VARCHAR(40) NOT NULL DEFAULT '',
  ilink_user_id_uniq VARCHAR(128) UNIQUE GENERATED ALWAYS AS (NULLIF(ilink_user_id,'')) STORED,
  person_uniq BIGINT UNIQUE GENERATED ALWAYS AS (NULLIF(person_id,0)) STORED
);
CREATE INDEX idx_wx_push_person ON wx_push_accounts(person_id);

CREATE TABLE schema_migrations (
  version VARCHAR(64) PRIMARY KEY,
  applied_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
