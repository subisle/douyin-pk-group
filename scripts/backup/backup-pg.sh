#!/usr/bin/env bash
# PostgreSQL douyin_dev 逻辑备份（pg_dump 自定义格式 + gzip 压缩）
#
#   scripts/backup/backup-pg.sh              # 备份到 BACKUP_DIR（默认 ./backups/pg）
#   BACKUP_DIR=/opt/pk-server/backups scripts/backup/backup-pg.sh
#
# 环境变量（与 deploy/.env 一致）：
#   PG_HOST / PG_PORT / PG_DB / PG_USER / PGPASSWORD
#
# 保留策略：BACKUP_KEEP（默认 7 份），更旧的自动删除。MySQL 原库不动，
# 作为切换期的对照源保留；本脚本只备份 PG（新唯一事实源）。
set -Eeuo pipefail
export LC_ALL="${LC_ALL:-C.UTF-8}"

PG_HOST="${PG_HOST:-127.0.0.1}"
PG_PORT="${PG_PORT:-3318}"
PG_DB="${PG_DB:-douyin_dev}"
PG_USER="${PG_USER:-douyin_dev}"
BACKUP_DIR="${BACKUP_DIR:-$(cd "$(dirname "$0")/../.." && pwd)/backups/pg}"
BACKUP_KEEP="${BACKUP_KEEP:-7}"
STAMP="$(date +%Y%m%d-%H%M%S)"
TARGET="$BACKUP_DIR/${PG_DB}-${STAMP}.dump"

PG_DUMP="${PG_DUMP:-pg_dump}"
command -v "$PG_DUMP" >/dev/null || PG_DUMP="/opt/homebrew/opt/postgresql@17/bin/pg_dump"
command -v "$PG_DUMP" >/dev/null || { echo "缺 pg_dump（brew install postgresql@17）"; exit 1; }

mkdir -p "$BACKUP_DIR"
echo "==> pg_dump $PG_DB@$PG_HOST:$PG_PORT -> $TARGET"
"$PG_DUMP" -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$PG_DB" \
  -Fc -Z 6 -f "$TARGET"
echo "    $(du -h "$TARGET" | cut -f1) $(wc -c < "$TARGET") bytes"

# 可恢复性验证：列出归档内容（失败即报错）
"$PG_DUMP" --version >/dev/null
/opt/homebrew/opt/postgresql@17/bin/pg_restore -l "$TARGET" >/dev/null 2>&1 \
  || pg_restore -l "$TARGET" >/dev/null

# 清理旧备份
COUNT=$(ls -1t "$BACKUP_DIR/${PG_DB}-"*.dump 2>/dev/null | wc -l | tr -d ' ')
if [ "$COUNT" -gt "$BACKUP_KEEP" ]; then
  ls -1t "$BACKUP_DIR/${PG_DB}-"*.dump | tail -n +"$((BACKUP_KEEP + 1))" | while read -r old; do
    rm -f "$old"; echo "    清理旧备份: $old"
  done
fi
echo "==> 完成（保留最近 $BACKUP_KEEP 份）"
