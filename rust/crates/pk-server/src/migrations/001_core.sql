-- 001_core · 核心表（PostgreSQL 版）（由 MySQL douyin_dev 实际 schema 转换，2026-10）
-- 类型映射:BIGINT UNSIGNED->BIGINT / TINYINT(1)->SMALLINT / ENUM->VARCHAR+CHECK
-- MEDIUMTEXT->TEXT / MEDIUMBLOB->BYTEA / DATETIME(3)->TIMESTAMPTZ / VARBINARY(16)->INET
-- 生成列唯一键 -> 部分唯一索引(PG 地道写法)
-- updated_at 自动更新 -> 触发器 touch_updated_at(文件末尾统一建)

CREATE TABLE IF NOT EXISTS access_logs (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  kind VARCHAR(16) NOT NULL DEFAULT 'api',
  ip INET NOT NULL,
  province VARCHAR(32) NOT NULL DEFAULT '',
  city VARCHAR(64) NOT NULL DEFAULT '',
  isp VARCHAR(64) NOT NULL DEFAULT '',
  geo_source VARCHAR(191) NOT NULL DEFAULT '',
  team VARCHAR(16) NOT NULL DEFAULT '',
  path VARCHAR(160) NOT NULL DEFAULT '',
  account_name VARCHAR(64) NOT NULL DEFAULT '',
  allowed SMALLINT NOT NULL DEFAULT 0,
  login_ok SMALLINT NOT NULL DEFAULT 0,
  dedup_key VARCHAR(191),
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_access_account_time ON access_logs (account_name, created_at);
CREATE INDEX IF NOT EXISTS idx_access_ip_time ON access_logs (ip, created_at);
CREATE INDEX IF NOT EXISTS idx_access_kind_time ON access_logs (kind, created_at);
CREATE INDEX IF NOT EXISTS idx_access_time ON access_logs (created_at);
CREATE UNIQUE INDEX IF NOT EXISTS uniq_access_dedup ON access_logs (dedup_key);

CREATE TABLE IF NOT EXISTS account (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  person_id BIGINT NOT NULL,
  anchor_id VARCHAR(64) NOT NULL,
  douyin_no VARCHAR(64) NOT NULL DEFAULT '',
  anchor_name VARCHAR(128) NOT NULL DEFAULT '',
  is_primary SMALLINT NOT NULL DEFAULT 0,
  status VARCHAR(32) NOT NULL DEFAULT 'active',
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_account_person_primary ON account (person_id, is_primary);
CREATE INDEX IF NOT EXISTS idx_douyin_no ON account (douyin_no);
CREATE INDEX IF NOT EXISTS idx_person ON account (person_id);
CREATE UNIQUE INDEX IF NOT EXISTS uk_anchor ON account (anchor_id);

CREATE TABLE IF NOT EXISTS aliases (
  alias VARCHAR(64) NOT NULL,
  real_name VARCHAR(64) NOT NULL DEFAULT '',
  PRIMARY KEY (alias)
);

CREATE TABLE IF NOT EXISTS anchor_claims (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  person_id BIGINT NOT NULL,
  person_name VARCHAR(64) NOT NULL,
  team VARCHAR(16) NOT NULL DEFAULT '',
  douyin_no VARCHAR(32) NOT NULL,
  screenshot BYTEA NOT NULL,
  mime VARCHAR(32) NOT NULL DEFAULT 'image/jpeg',
  status VARCHAR(16) NOT NULL DEFAULT 'pending',
  reject_reason VARCHAR(255) NOT NULL DEFAULT '',
  created_at VARCHAR(40) NOT NULL,
  decided_at VARCHAR(40) NOT NULL DEFAULT '',
  decided_by VARCHAR(32) NOT NULL DEFAULT '',
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_claims_status ON anchor_claims (status, created_at);

CREATE TABLE IF NOT EXISTS bot_sessions (
  channel VARCHAR(16) NOT NULL,
  conversation_id VARCHAR(64) NOT NULL,
  kind VARCHAR(16) NOT NULL,
  last_text VARCHAR(255),
  last_seen VARCHAR(40) NOT NULL,
  is_admin SMALLINT NOT NULL DEFAULT 0,
  PRIMARY KEY (channel, conversation_id)
);

CREATE TABLE IF NOT EXISTS config (
  cfg_key VARCHAR(64) NOT NULL,
  cfg_value TEXT NOT NULL,
  updated_at VARCHAR(40) NOT NULL,
  PRIMARY KEY (cfg_key)
);

CREATE TABLE IF NOT EXISTS daily_metric (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  person_id BIGINT NOT NULL,
  anchor_id VARCHAR(64) NOT NULL,
  biz_date date NOT NULL,
  wave bigint NOT NULL DEFAULT 0,
  cumulative_wave bigint NOT NULL DEFAULT 0,
  prev_snapshot_date date,
  wave_span smallint NOT NULL DEFAULT 1,
  wave_reliable SMALLINT NOT NULL DEFAULT 1,
  minutes int NOT NULL DEFAULT 0,
  cumulative_minutes int NOT NULL DEFAULT 0,
  minutes_span smallint NOT NULL DEFAULT 1,
  minutes_reliable SMALLINT NOT NULL DEFAULT 1,
  is_live SMALLINT NOT NULL DEFAULT 0,
  tier char(1),
  source_batch_id BIGINT,
  recomputed_at TIMESTAMPTZ,
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_daily_date ON daily_metric (biz_date);
CREATE UNIQUE INDEX IF NOT EXISTS uk_person_date ON daily_metric (person_id, biz_date);

CREATE TABLE IF NOT EXISTS duration_snapshot (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  anchor_id VARCHAR(64) NOT NULL,
  biz_date date NOT NULL,
  minutes int NOT NULL DEFAULT 0,
  cumulative_minutes int NOT NULL DEFAULT 0,
  minutes_span smallint NOT NULL DEFAULT 1,
  minutes_reliable SMALLINT NOT NULL DEFAULT 1,
  source_batch_id BIGINT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_dur_date ON duration_snapshot (biz_date);
CREATE INDEX IF NOT EXISTS idx_duration_snapshot_anchor ON duration_snapshot (anchor_id);
CREATE UNIQUE INDEX IF NOT EXISTS uk_anchor_date ON duration_snapshot (anchor_id, biz_date);

CREATE TABLE IF NOT EXISTS import_batch (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  source VARCHAR(32) NOT NULL,
  filename VARCHAR(255) NOT NULL,
  biz_date date,
  team VARCHAR(16),
  imported_rows INTEGER NOT NULL DEFAULT 0,
  ok_rows INTEGER NOT NULL DEFAULT 0,
  failed_rows INTEGER NOT NULL DEFAULT 0,
  note VARCHAR(512),
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_import_batch_source_time ON import_batch (source, created_at);

CREATE TABLE IF NOT EXISTS import_pending (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  name VARCHAR(128) NOT NULL DEFAULT '',
  douyin_no VARCHAR(64) NOT NULL,
  biz_date date NOT NULL,
  wave bigint,
  cumulative_wave bigint,
  minutes int,
  is_live SMALLINT,
  source_batch_id BIGINT,
  absorbed_person_id BIGINT,
  absorbed_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_open ON import_pending (absorbed_at, biz_date);
CREATE INDEX IF NOT EXISTS idx_person ON import_pending (absorbed_person_id);
CREATE UNIQUE INDEX IF NOT EXISTS uk_no_date ON import_pending (douyin_no, biz_date);

CREATE TABLE IF NOT EXISTS ip_allowlist (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  ip VARCHAR(45) NOT NULL,
  note VARCHAR(128) NOT NULL DEFAULT '',
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_ip_allowlist_ip ON ip_allowlist (ip);

CREATE TABLE IF NOT EXISTS ip_geo_cache (
  ip INET NOT NULL,
  province VARCHAR(32) NOT NULL DEFAULT '',
  city VARCHAR(64) NOT NULL DEFAULT '',
  isp VARCHAR(64) NOT NULL DEFAULT '',
  source VARCHAR(191) NOT NULL DEFAULT '',
  allowed SMALLINT NOT NULL DEFAULT 0,
  looked_up_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (ip)
);

CREATE TABLE IF NOT EXISTS monthly_duration (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  person_id BIGINT NOT NULL,
  period char(7) NOT NULL,
  minutes int NOT NULL DEFAULT 0,
  source_batch_id BIGINT,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_period ON monthly_duration (period);
CREATE UNIQUE INDEX IF NOT EXISTS uk_person_period ON monthly_duration (person_id, period);

CREATE TABLE IF NOT EXISTS monthly_metric (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  person_id BIGINT NOT NULL,
  period char(7) NOT NULL,
  wave bigint NOT NULL DEFAULT 0,
  live_days int NOT NULL DEFAULT 0,
  best_day_wave bigint NOT NULL DEFAULT 0,
  best_day_date date,
  source_batch_id BIGINT,
  recomputed_at TIMESTAMPTZ,
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_period ON monthly_metric (period);
CREATE UNIQUE INDEX IF NOT EXISTS uk_person_period ON monthly_metric (person_id, period);

CREATE TABLE IF NOT EXISTS my_accounts (
  name VARCHAR(64) NOT NULL,
  phone VARCHAR(24) NOT NULL DEFAULT '',
  pass VARCHAR(255) NOT NULL DEFAULT '',
  is_admin SMALLINT NOT NULL DEFAULT 0,
  created_at VARCHAR(40) NOT NULL,
  bound SMALLINT NOT NULL DEFAULT 1,
  bound_at VARCHAR(40) NOT NULL DEFAULT '',
  source VARCHAR(16) NOT NULL DEFAULT 'admin',
  wb_state VARCHAR(64) NOT NULL DEFAULT '',
  person_id BIGINT NOT NULL DEFAULT 0,
  PRIMARY KEY (name)
);

CREATE TABLE IF NOT EXISTS my_targets (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  name VARCHAR(64) NOT NULL,
  team VARCHAR(16) NOT NULL DEFAULT '',
  kind VARCHAR(16) NOT NULL DEFAULT 'person',
  target_person_id BIGINT NOT NULL DEFAULT 0,
  target_name VARCHAR(64) NOT NULL DEFAULT '',
  target_value bigint NOT NULL DEFAULT 0,
  hit SMALLINT NOT NULL DEFAULT 0,
  last_gap bigint NOT NULL DEFAULT 0,
  last_notice_date VARCHAR(16) NOT NULL DEFAULT '',
  created_at VARCHAR(40) NOT NULL,
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_targets_name ON my_targets (name);

CREATE TABLE IF NOT EXISTS person (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  name VARCHAR(64) NOT NULL DEFAULT '',
  gender VARCHAR(32) NOT NULL DEFAULT 'unknown',
  master_id BIGINT,
  generation INTEGER,
  group_name VARCHAR(64),
  avatar_url VARCHAR(512),
  hide_in_daily_report SMALLINT NOT NULL DEFAULT 0,
  phone VARCHAR(24) NOT NULL DEFAULT '',
  status VARCHAR(32) NOT NULL DEFAULT 'active',
  joined_at DATE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  deleted_at TIMESTAMPTZ,
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_master ON person (master_id);
CREATE INDEX IF NOT EXISTS idx_name ON person (name);
CREATE INDEX IF NOT EXISTS idx_status_gender ON person (status, gender);
CREATE INDEX IF NOT EXISTS idx_name ON person (name);
CREATE INDEX IF NOT EXISTS idx_person_phone ON person (phone);

CREATE TABLE IF NOT EXISTS plans (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  title VARCHAR(128) NOT NULL,
  metric VARCHAR(16) NOT NULL,
  days int NOT NULL DEFAULT 3,
  payload TEXT NOT NULL,
  created_at VARCHAR(40) NOT NULL,
  published SMALLINT NOT NULL DEFAULT 0,
  event_type VARCHAR(32) NOT NULL DEFAULT '',
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_plans_published ON plans (published);

CREATE TABLE IF NOT EXISTS push_bindings (
  name VARCHAR(64) NOT NULL,
  team VARCHAR(16) NOT NULL DEFAULT '',
  person_id BIGINT NOT NULL DEFAULT 0,
  conversation_id VARCHAR(64) NOT NULL DEFAULT '',
  to_user_id VARCHAR(64) NOT NULL DEFAULT '',
  nickname VARCHAR(64) NOT NULL DEFAULT '',
  bind_code VARCHAR(16) NOT NULL DEFAULT '',
  code_expires VARCHAR(40) NOT NULL DEFAULT '',
  bound_at VARCHAR(40) NOT NULL DEFAULT '',
  notify_target SMALLINT NOT NULL DEFAULT 0,
  daily_report SMALLINT NOT NULL DEFAULT 0,
  last_push VARCHAR(40) NOT NULL DEFAULT '',
  last_error VARCHAR(255) NOT NULL DEFAULT '',
  updated_at VARCHAR(40) NOT NULL DEFAULT '',
  last_report_date VARCHAR(16) NOT NULL DEFAULT '',
  PRIMARY KEY (name)
);
CREATE INDEX IF NOT EXISTS idx_push_code ON push_bindings (bind_code);
CREATE INDEX IF NOT EXISTS idx_push_conv ON push_bindings (conversation_id);

CREATE TABLE IF NOT EXISTS push_jobs (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  team VARCHAR(16) NOT NULL DEFAULT '',
  biz_date VARCHAR(16) NOT NULL DEFAULT '',
  run_at VARCHAR(40) NOT NULL DEFAULT '',
  status VARCHAR(16) NOT NULL DEFAULT 'pending',
  created_at VARCHAR(40) NOT NULL DEFAULT '',
  done_at VARCHAR(40) NOT NULL DEFAULT '',
  note VARCHAR(255) NOT NULL DEFAULT '',
  PRIMARY KEY (id)
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_push_job ON push_jobs (team, biz_date);

CREATE TABLE IF NOT EXISTS roster_state (
  event_type VARCHAR(32) NOT NULL,
  body TEXT NOT NULL,
  updated_at VARCHAR(40) NOT NULL,
  PRIMARY KEY (event_type)
);

CREATE TABLE IF NOT EXISTS wave_snapshot (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  anchor_id VARCHAR(64) NOT NULL,
  biz_date date NOT NULL,
  wave_value bigint NOT NULL DEFAULT 0,
  rank_in_guild int,
  source_batch_id BIGINT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_snap_date ON wave_snapshot (biz_date);
CREATE INDEX IF NOT EXISTS idx_wave_snapshot_anchor ON wave_snapshot (anchor_id);
CREATE UNIQUE INDEX IF NOT EXISTS uk_anchor_date ON wave_snapshot (anchor_id, biz_date);

CREATE TABLE IF NOT EXISTS wx_push_accounts (
  name VARCHAR(64) NOT NULL,
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
  PRIMARY KEY (name)
);
CREATE INDEX IF NOT EXISTS idx_wx_push_conv ON wx_push_accounts (conversation_id);
CREATE INDEX IF NOT EXISTS idx_wx_push_ilink_user ON wx_push_accounts (ilink_user_id);
CREATE INDEX IF NOT EXISTS idx_wx_push_person ON wx_push_accounts (person_id);

CREATE TABLE IF NOT EXISTS wx_sessions (
  conversation_id VARCHAR(64) NOT NULL,
  to_user_id VARCHAR(64) NOT NULL DEFAULT '',
  context_token VARCHAR(255) NOT NULL DEFAULT '',
  nickname VARCHAR(64) NOT NULL DEFAULT '',
  PRIMARY KEY (conversation_id)
);



-- 软唯一键：空串/0 不参与唯一约束（等价 MySQL 生成列 NULLIF 方案）
CREATE UNIQUE INDEX IF NOT EXISTS uk_my_phone_uniq ON my_accounts (phone) WHERE phone <> '';
CREATE UNIQUE INDEX IF NOT EXISTS uk_person_phone_uniq ON person (phone) WHERE phone <> '';
CREATE UNIQUE INDEX IF NOT EXISTS uk_wx_push_user_uniq ON wx_push_accounts (ilink_user_id) WHERE ilink_user_id <> '';
CREATE UNIQUE INDEX IF NOT EXISTS uk_wx_push_person_uniq ON wx_push_accounts (person_id) WHERE person_id > 0;
CREATE UNIQUE INDEX IF NOT EXISTS uk_my_person_uniq ON my_accounts (person_id) WHERE person_id > 0;
CREATE INDEX IF NOT EXISTS idx_my_person ON my_accounts (person_id);
CREATE INDEX IF NOT EXISTS idx_my_person_bound ON my_accounts (person_id, bound);
CREATE INDEX IF NOT EXISTS idx_push_person ON push_bindings (person_id);
CREATE INDEX IF NOT EXISTS idx_wx_push_person ON wx_push_accounts (person_id);

-- identity_orphan_log（015：身份异常审计）
CREATE TABLE IF NOT EXISTS identity_orphan_log (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  tbl VARCHAR(32) NOT NULL,
  row_key VARCHAR(64) NOT NULL,
  person_id BIGINT NOT NULL DEFAULT 0,
  kind VARCHAR(24) NOT NULL DEFAULT 'person_deleted',
  detected_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  handled SMALLINT NOT NULL DEFAULT 0,
  note VARCHAR(255) NOT NULL DEFAULT '',
  PRIMARY KEY (id)
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_orphan ON identity_orphan_log (tbl, row_key, kind);
CREATE INDEX IF NOT EXISTS idx_orphan_open ON identity_orphan_log (handled, detected_at);
CREATE INDEX IF NOT EXISTS idx_orphan_person ON identity_orphan_log (person_id);

-- my_bind_requests（017：绑定审核流）
CREATE TABLE IF NOT EXISTS my_bind_requests (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  name VARCHAR(64) NOT NULL,
  person_id BIGINT NOT NULL DEFAULT 0,
  person_name VARCHAR(64) NOT NULL DEFAULT '',
  team VARCHAR(16) NOT NULL DEFAULT '',
  status VARCHAR(16) NOT NULL DEFAULT 'pending',
  reason VARCHAR(255) NOT NULL DEFAULT '',
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  decided_at TIMESTAMPTZ,
  decided_by VARCHAR(64) NOT NULL DEFAULT '',
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_name_status ON my_bind_requests (name, status);
CREATE INDEX IF NOT EXISTS idx_status ON my_bind_requests (status);

-- updated_at 自动更新触发器
CREATE OR REPLACE FUNCTION touch_updated_at() RETURNS trigger AS $$
BEGIN
  NEW.updated_at := now();
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_touch_import_pending ON import_pending;
CREATE TRIGGER trg_touch_import_pending BEFORE UPDATE ON import_pending FOR EACH ROW EXECUTE FUNCTION touch_updated_at();
DROP TRIGGER IF EXISTS trg_touch_monthly_duration ON monthly_duration;
CREATE TRIGGER trg_touch_monthly_duration BEFORE UPDATE ON monthly_duration FOR EACH ROW EXECUTE FUNCTION touch_updated_at();
DROP TRIGGER IF EXISTS trg_touch_person ON person;
CREATE TRIGGER trg_touch_person BEFORE UPDATE ON person FOR EACH ROW EXECUTE FUNCTION touch_updated_at();