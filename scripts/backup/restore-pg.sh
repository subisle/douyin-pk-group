#!/usr/bin/env bash
# 从 pg_dump 备份恢复 douyin_dev（覆盖前先备份当前库）
#
#   scripts/backup/restore-pg.sh backups/pg/douyin_dev-XXXX.dump
#
# 流程：dump 当前库到 /tmp（兜底）→ drop/create → pg_restore → 行数核对
set -Eeuo pipefail
DUMP="${1:?用法: restore-pg.sh <backup.dump>}"
PG_HOST="${PG_HOST:-127.0.0.1}"
PG_PORT="${PG_PORT:-3318}"
PG_DB="${PG_DB:-douyin_dev}"
PG_USER="${PG_USER:-douyin_dev}"
PG_DUMP="${PG_DUMP:-/opt/homebrew/opt/postgresql@17/bin/pg_dump}"
PG_RESTORE="${PG_RESTORE:-/opt/homebrew/opt/postgresql@17/bin/pg_restore}"
PSQL="${PSQL:-/opt/homebrew/opt/postgresql@17/bin/psql}"

[ -f "$DUMP" ] || { echo "备份文件不存在: $DUMP"; exit 1; }

echo "==> 兜底备份当前库"
$PG_DUMP -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$PG_DB" -Fc -Z 6 \
  -f "/tmp/${PG_DB}-pre-restore-$(date +%H%M%S).dump"

echo "==> 重建 $PG_DB"
$PSQL -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres \
  -c "DROP DATABASE IF EXISTS $PG_DB;" -c "CREATE DATABASE $PG_DB OWNER douyin_dev;" >/dev/null

echo "==> pg_restore $DUMP"
$PG_RESTORE -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$PG_DB" \
  --no-owner --role=douyin_dev "$DUMP"
echo "==> 完成。行数抽查："
$PSQL -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$PG_DB" -At \
  -c "SELECT 'person='||count(*) FROM person;"
