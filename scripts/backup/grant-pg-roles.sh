#!/usr/bin/env bash
# 恢复/重建 douyin_dev 后必须跑一次：把 pk_reader / pk_writer 的权限补回去。
# 根因：pg_restore --no-privileges 或 DROP SCHEMA 重建会把 GRANT 全部清掉，
# 服务表现为「relation xxx does not exist」（schema USAGE 丢失）。
# 2026-10-12 已三次踩坑（147 合并 15:18、事故重建 21:28），固化为脚本。
# 用法：scripts/backup/grant-pg-roles.sh   （可用 PGHOST/PGPORT/PGUSER/PGDATABASE 覆盖）
set -euo pipefail
PGHOST="${PGHOST:-127.0.0.1}"; PGPORT="${PGPORT:-3318}"
PGUSER="${PGUSER:-zmy}"; PGDATABASE="${PGDATABASE:-douyin_dev}"
PSQL="${PSQL:-/opt/homebrew/opt/postgresql@17/bin/psql}"

"$PSQL" -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$PGDATABASE" -v ON_ERROR_STOP=1 <<'SQL'
BEGIN;
GRANT USAGE ON SCHEMA public TO pk_reader;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO pk_reader;
GRANT USAGE, CREATE ON SCHEMA public TO pk_writer;   -- pk_writer 兼任 migrator（rust/.env DB_MIGRATOR_USER）
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO pk_writer;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO pk_writer;
-- 新建对象自动授权（docs/16 §2.4-3 待收尾项）
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO pk_reader;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO pk_writer;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT USAGE, SELECT ON SEQUENCES TO pk_writer;
COMMIT;
SQL

"$PSQL" -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$PGDATABASE" -Atc "
select 'reader SELECT person: '||has_table_privilege('pk_reader','public.person','SELECT')||' | '||
       'writer INSERT my_accounts: '||has_table_privilege('pk_writer','public.my_accounts','INSERT')||' | '||
       'writer CREATE schema: '||has_schema_privilege('pk_writer','public','CREATE')"
