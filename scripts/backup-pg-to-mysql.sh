#!/usr/bin/env bash
# PostgreSQL 唯一业务库 → userxs 远程 MySQL 备份库。
#
# 用法：
#   DB_LOCAL_PASSWORD=... \
#   DB_BACKUP_PASSWORD=... \
#   scripts/backup-pg-to-mysql.sh
#
# 可选环境变量：
#   PG_HOST / PG_PORT / PG_DB / PG_USER
#   DB_BACKUP_HOST / DB_BACKUP_PORT / DB_BACKUP_USER / DB_BACKUP_NAME
#   BACKUP_DIR（默认 ./backups/pg-to-mysql）
#   默认对齐业务表；保留 legacy_* 历史表
#   DRY_RUN=1

set -Eeuo pipefail
export LC_ALL="${LC_ALL:-C.UTF-8}"

BASE_DIR="$(cd "$(dirname "$0")/.." && pwd)"
STAMP="$(date +%Y%m%d-%H%M%S)"
PG_HOST="${PG_HOST:-127.0.0.1}"
PG_PORT="${PG_PORT:-3318}"
PG_DB="${PG_DB:-douyin_dev}"
PG_USER="${PG_USER:-zmy}"
DB_BACKUP_HOST="${DB_BACKUP_HOST:-mysql5.sqlpub.com}"
DB_BACKUP_PORT="${DB_BACKUP_PORT:-3310}"
DB_BACKUP_USER="${DB_BACKUP_USER:-userxs}"
DB_BACKUP_NAME="${DB_BACKUP_NAME:-userxs}"
BACKUP_DIR="${BACKUP_DIR:-$BASE_DIR/backups/pg-to-mysql}"
SQL_DIR="$BACKUP_DIR/$STAMP"

: "${DB_LOCAL_PASSWORD:?请配置 PG 读取账号密码 DB_LOCAL_PASSWORD}"
: "${DB_BACKUP_PASSWORD:?请配置远程备份库密码 DB_BACKUP_PASSWORD}"

command -v pg_dump >/dev/null 2>&1 || PG_DUMP="/opt/homebrew/opt/postgresql@17/bin/pg_dump"
PG_DUMP="${PG_DUMP:-/opt/homebrew/opt/postgresql@17/bin/pg_dump}"
command -v "$PG_DUMP" >/dev/null 2>&1 || { echo "缺 pg_dump" >&2; exit 1; }
python3 - <<'PY' >/dev/null 2>&1 || { echo "缺 Python 依赖：pymysql / psycopg" >&2; exit 1; }
import pymysql, psycopg  # noqa: F401
PY

log() { printf '[backup-pg-to-mysql] %s\n' "$*"; }
fail() { printf '[backup-pg-to-mysql] 错误：%s\n' "$*" >&2; exit 1; }

mkdir -p "$SQL_DIR"
chmod 700 "$BACKUP_DIR" "$SQL_DIR"

log "连接检查：$PG_DB @ $PG_HOST:$PG_PORT"
export PGPASSWORD="$DB_LOCAL_PASSWORD"
psql_bin="${PSQL:-/opt/homebrew/opt/postgresql@17/bin/psql}"
"$psql_bin" -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$PG_DB" -Atqc 'SELECT 1' >/dev/null

if [[ "${DRY_RUN:-0}" != "1" ]]; then
  log "连接检查：$DB_BACKUP_NAME @ $DB_BACKUP_HOST:$DB_BACKUP_PORT"
  DB_BACKUP_HOST="$DB_BACKUP_HOST" DB_BACKUP_PORT="$DB_BACKUP_PORT" \
    DB_BACKUP_USER="$DB_BACKUP_USER" DB_BACKUP_PASSWORD="$DB_BACKUP_PASSWORD" \
    DB_BACKUP_NAME="$DB_BACKUP_NAME" python3 - <<'PY'
import os
import pymysql
pymysql.connect(
    host=os.environ["DB_BACKUP_HOST"], port=int(os.environ["DB_BACKUP_PORT"]),
    user=os.environ["DB_BACKUP_USER"], password=os.environ["DB_BACKUP_PASSWORD"],
    database=os.environ["DB_BACKUP_NAME"], connect_timeout=15,
)
PY
fi

log "导出 PG 逻辑备份"
dump_file="$SQL_DIR/${PG_DB}.dump.gz"
"$PG_DUMP" -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$PG_DB" -Fc -Z 6 \
  -f "$dump_file"
[[ -s "$dump_file" ]] || fail "PG 备份为空"

cat > "$SQL_DIR/backup.py" <<'PY'
import gzip
import json
import os
import sys
from decimal import Decimal
from pathlib import Path

import psycopg
import pymysql

TABLES = [
    "aliases", "person", "account", "import_batch", "daily_metric",
    "duration_snapshot", "wave_snapshot", "import_pending", "monthly_duration",
    "monthly_metric", "plans", "roster_state", "bot_sessions", "config",
    "push_bindings", "push_jobs", "my_targets", "anchor_claims",
    "my_accounts", "my_bind_requests", "access_logs", "ip_allowlist",
    "ip_geo_cache", "identity_orphan_log", "wx_push_accounts", "wx_sessions",
]

PG = dict(
    host=os.environ.get("PG_HOST", "127.0.0.1"),
    port=int(os.environ.get("PG_PORT", "3318")),
    user=os.environ.get("PG_USER", "zmy"),
    password=os.environ["DB_LOCAL_PASSWORD"],
    dbname=os.environ.get("PG_DB", "douyin_dev"),
)
MY = dict(
    host=os.environ.get("DB_BACKUP_HOST", "mysql5.sqlpub.com"),
    port=int(os.environ.get("DB_BACKUP_PORT", "3310")),
    user=os.environ.get("DB_BACKUP_USER", "userxs"),
    password=os.environ["DB_BACKUP_PASSWORD"],
    database=os.environ.get("DB_BACKUP_NAME", "userxs"), charset="utf8mb4",
    autocommit=False, connect_timeout=15,
)


def mysql_value(value):
    if value is None:
        return None
    if value is None or isinstance(value, (str, int, float, bytes)):
        return value
    if isinstance(value, Decimal):
        return int(value) if value == value.to_integral_value() else float(value)
    if hasattr(value, "isoformat"):
        return value.isoformat() if not hasattr(value, "hour") else value.isoformat(sep=" ")
    if isinstance(value, (list, dict)):
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    return str(value)


def log(message):
    print(f"[backup-pg-to-mysql] {message}")


def main(sql_path: Path):
    old_env = os.environ.copy()
    os.environ["PGPASSWORD"] = os.environ["DB_LOCAL_PASSWORD"]
    try:
        with psycopg.connect(**PG) as pg, pymysql.connect(**MY) as my:
            with pg.cursor() as p, my.cursor() as m:
                p.execute(
                    "SELECT table_name FROM information_schema.tables "
                    "WHERE table_schema = 'public' AND table_type = 'BASE TABLE'"
                )
                pg_tables = {row[0] for row in p.fetchall()}
                missing = [t for t in TABLES if t not in pg_tables]
                if missing:
                    raise RuntimeError(f"PG 缺少业务表: {missing}")

                m.execute("SELECT table_name FROM information_schema.tables WHERE table_schema = DATABASE()")
                my_tables = {row[0] for row in m.fetchall()}
                if True:
                    for table in reversed(TABLES):
                        if table in my_tables:
                            m.execute(f"SET FOREIGN_KEY_CHECKS=0; DROP TABLE IF EXISTS `{table}`;")
                    nullable_columns = {
                        "import_batch": {"source", "filename", "biz_date", "team", "imported_rows", "ok_rows", "failed_rows"},
                    }
                    m.execute("SET FOREIGN_KEY_CHECKS=0;")
                    with open(sql_path, encoding="utf-8") as f:
                        sql = f.read()
                    for statement in sql.split(";\n"):
                        stmt = statement.strip()
                        if stmt:
                            m.execute(stmt)
                    for table, columns in nullable_columns.items():
                        for column in columns:
                            m.execute(
                                "ALTER TABLE `{}` MODIFY `{}` {} NULL".format(
                                    table,
                                    column,
                                    "int" if column in {"imported_rows", "ok_rows", "failed_rows"} else "varchar(64)",
                                )
                            )
                    my.commit()

                report = {}
                for table in TABLES:
                    p.execute(f'SELECT * FROM "{table}"')
                    columns = [d.name for d in p.description]
                    rows = p.fetchall()
                    placeholders = ",".join(["%s"] * len(columns))
                    escaped_columns = [c.replace("`", "``") for c in columns]
                    quoted = ",".join(f"`{c}`" for c in escaped_columns)
                    sql = (
                        f"INSERT INTO `{table}` ({quoted}) VALUES ({placeholders})"
                    )
                    for row in rows:
                        row_values = tuple(mysql_value(v) for v in row)
                        try:
                            m.execute(sql, row_values)
                        except Exception:
                            log(f"导入失败：{table} id={row_values[0] if row_values else '?'}")
                            raise
                    if table in {"person", "account", "daily_metric", "import_batch"}:
                        m.execute(f"SELECT MAX(`id`) FROM `{table}`")
                        max_id = m.fetchone()[0] or 0
                        m.execute(
                            "ALTER TABLE `{}` AUTO_INCREMENT = %s".format(table),
                            (max_id + 1,),
                        )
                    report[table] = len(rows)
                m.execute(
                    "DELETE FROM schema_migrations"
                )
                m.executemany(
                    "INSERT INTO schema_migrations (version) VALUES (%s)",
                    [str(i) for i in range(1, 19)],
                )
                my.commit()
                print(json.dumps(report, ensure_ascii=False, sort_keys=True))
    finally:
        os.environ.clear()
        os.environ.update(old_env)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: backup.py <aligned-schema.sql>")
    main(Path(sys.argv[1]))
PY

if [[ "${DRY_RUN:-0}" == "1" ]]; then
  log "dry-run 完成：$SQL_DIR"
  exit 0
fi

log "转换并写入远程 MySQL"
python3 "$SQL_DIR/backup.py" "$BASE_DIR/scripts/userxs-aligned-schema.sql"

log "写入校验指纹"
sha256sum "$dump_file" > "$SQL_DIR/backup.sha256"
sha256sum "$SQL_DIR/backup.py" "$BASE_DIR/scripts/userxs-aligned-schema.sql" >> "$SQL_DIR/backup.sha256"
log "备份完成：$SQL_DIR"

find "$BACKUP_DIR" -mindepth 1 -maxdepth 1 -type d -name '20*' | sort -r | tail -n +"$(( ${BACKUP_KEEP:-7} + 1 ))" | while read -r old; do
  rm -rf "$old"
done
