-- 022 · 师门日报与管理员消息下发（docs/23）
-- 1) push_bindings 加师门日报开关与同日去重列（与 last_report_date 同机制）
-- 2) 管理员消息下发三张表：admin_messages / admin_outbox / admin_message_reads

-- ---------- 师门日报开关（P1） ----------

ALTER TABLE push_bindings ADD COLUMN IF NOT EXISTS lineage_report SMALLINT NOT NULL DEFAULT 0;
ALTER TABLE push_bindings ADD COLUMN IF NOT EXISTS last_lineage_date VARCHAR(16) NOT NULL DEFAULT '';

-- ---------- 管理员消息下发（P2/P3） ----------

CREATE TABLE IF NOT EXISTS admin_messages (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  title VARCHAR(128) NOT NULL DEFAULT '',
  content TEXT NOT NULL DEFAULT '',
  image_kind VARCHAR(16) NOT NULL DEFAULT 'none',
  image_date DATE,
  channels VARCHAR(32) NOT NULL DEFAULT '',
  target_type VARCHAR(16) NOT NULL DEFAULT 'persons',
  target_ids TEXT NOT NULL DEFAULT '[]',
  created_by VARCHAR(64) NOT NULL DEFAULT 'admin',
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);

CREATE TABLE IF NOT EXISTS admin_outbox (
  id BIGINT GENERATED ALWAYS AS IDENTITY,
  message_id BIGINT NOT NULL,
  person_id BIGINT NOT NULL DEFAULT 0,
  recipient_name VARCHAR(64) NOT NULL DEFAULT '',
  channel VARCHAR(16) NOT NULL DEFAULT 'wxbot',
  status VARCHAR(16) NOT NULL DEFAULT 'pending',
  error VARCHAR(255) NOT NULL DEFAULT '',
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  sent_at TIMESTAMPTZ,
  PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_outbox_msg ON admin_outbox (message_id);
CREATE INDEX IF NOT EXISTS idx_outbox_status ON admin_outbox (status);

CREATE TABLE IF NOT EXISTS admin_message_reads (
  message_id BIGINT NOT NULL,
  name VARCHAR(64) NOT NULL DEFAULT '',
  person_id BIGINT NOT NULL DEFAULT 0,
  read_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (message_id, name)
);
