#!/usr/bin/env bash
# 目标端 PG 逻辑备份（64 容器形态 / 本地对练库通用），带可读性校验与保留策略。
#
#   scripts/backup/backup-pg-64.sh
#   BACKUP_DIR=/opt/pk-group/backups/pg BACKUP_KEEP=7 scripts/backup/backup-pg-64.sh
#
# 与 scripts/backup/backup-pg.sh（本机形态）的区别：这里走 scripts/db-align/common.sh
# 的传输层，因此对 64 是「ssh + docker exec pg_dump」，本地对练时是直连 PG。
# 备份产物本地保留 BACKUP_KEEP 份（默认 7，见 docs/17 决策 D7：64 磁盘仅余 8.9G）。
# 每个 dump 都用 pg_restore -l 校验可读——「备份出来但恢复不了」等于没有备份。

set -Eeuo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=../db-align/common.sh
source "$HERE/../db-align/common.sh"
# shellcheck source=../db-align/engine.sh
source "$HERE/../db-align/engine.sh"

align_load_env

BACKUP_DIR="${BACKUP_DIR:-$ALIGN_BACKUP_DIR/pg}"
BACKUP_KEEP="${BACKUP_KEEP:-7}"
STAMP="$(date +%Y%m%d-%H%M%S)"
TARGET="$BACKUP_DIR/${DST_PG_DB}-${STAMP}.dump"

mkdir -p "$BACKUP_DIR"
align_log "备份 $(_side_name dst) → $TARGET"

# dst_dump 内部已做「非空 + pg_restore -l 可读」双重校验，失败即退出
dst_dump "$TARGET"

# 保留策略：按时间倒序，删掉超出 BACKUP_KEEP 的
# 注意：macOS 自带 bash 3.2 没有 mapfile，用 while read 兼容
old=()
while IFS= read -r f; do
  [[ -n "$f" ]] && old+=("$f")
done < <(ls -1t "$BACKUP_DIR"/*.dump 2>/dev/null | tail -n +$((BACKUP_KEEP + 1)))
if [[ ${#old[@]} -gt 0 ]]; then
  printf '清理 %s 份过期备份\n' "${#old[@]}"
  rm -f "${old[@]}"
fi

printf '完成：%s\n当前保留 %s 份：\n' "$TARGET" "$(ls -1 "$BACKUP_DIR"/*.dump 2>/dev/null | wc -l | tr -d ' ')"
ls -1t "$BACKUP_DIR"/*.dump 2>/dev/null | head -n "$BACKUP_KEEP" | sed 's/^/  /'
