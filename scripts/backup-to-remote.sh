#!/usr/bin/env bash
# 把当前 MySQL/MariaDB 业务库 dump 后导入到远程备份库。
#
# 用法（在部署机上执行）：
#   /opt/pk-server/backup-to-remote.sh
#
# 配置（推荐放 /opt/pk-server/backup.env，权限 0600）：
#   DB_BACKUP_HOST=backup.example.com
#   DB_BACKUP_PORT=3306
#   DB_BACKUP_USER=douyin_backup
#   DB_BACKUP_PASSWORD=...
#   DB_BACKUP_NAME=douyin_backup
#   # 147 默认 pk-mariadb；RK3318 Compose 默认 mariadb
#   DB_BACKUP_CONTAINER=pk-mariadb
#
# 远程账号至少需要目标库的：
#   SELECT, INSERT, UPDATE, DELETE, CREATE, DROP, ALTER, INDEX,
#   LOCK TABLES, REFERENCES

set -Eeuo pipefail

# 稳定解析中文日志；部署机（Debian 12）内置 C.UTF-8。
export LC_ALL="${LC_ALL:-C.UTF-8}"

BASE_DIR="${PK_BACKUP_BASE_DIR:-/opt/pk-server}"
BACKUP_DIR="${BACKUP_DIR:-$BASE_DIR/backups}"
LOCAL_DB="${LOCAL_DB:-douyin}"
REMOTE_PORT="${DB_BACKUP_PORT:-3306}"

for env_file in "$BASE_DIR/backup.env" "$BASE_DIR/pk.env" "$BASE_DIR/.env"; do
  if [[ -r "$env_file" ]]; then
    set -a
    # shellcheck disable=SC1090
    . "$env_file"
    set +a
  fi
done

log() {
  printf '[backup-to-remote] %s\n' "$*"
}

fail() {
  printf '[backup-to-remote] 错误：%s\n' "$*" >&2
  exit 1
}

container_status() {
  docker inspect --format '{{.State.Status}}' "$1" 2>/dev/null || true
}

container_env_value() {
  docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' "$1" \
    | sed -n "s/^$2=//p" | tail -n 1
}

mkdir -p "$BACKUP_DIR"
chmod 700 "$BACKUP_DIR"

LOCK_FILE="$BACKUP_DIR/backup-to-remote.lock"
exec 9>"$LOCK_FILE"
if ! flock -n 9 true; then
  log "已有备份任务在执行，跳过本次触发"
  exit 0
fi

CONTAINER="${DB_BACKUP_CONTAINER:-}"
if [[ -z "$CONTAINER" ]]; then
  for candidate in pk-mariadb mariadb; do
    if [[ "$(container_status "$candidate")" == "running" ]]; then
      CONTAINER="$candidate"
      break
    fi
  done
fi
[[ "$(container_status "$CONTAINER")" == "running" ]] \
  || fail "未找到运行中的本地数据库容器；请设置 DB_BACKUP_CONTAINER"

LOCAL_ROOT_PASSWORD="$(container_env_value "$CONTAINER" MARIADB_ROOT_PASSWORD)"
[[ -n "$LOCAL_ROOT_PASSWORD" ]] \
  || fail "容器 $CONTAINER 没有 MARIADB_ROOT_PASSWORD，无法导出本地库"

: "${DB_BACKUP_HOST:?请配置 DB_BACKUP_HOST}"
: "${DB_BACKUP_USER:?请配置 DB_BACKUP_USER}"
: "${DB_BACKUP_PASSWORD:?请配置 DB_BACKUP_PASSWORD}"

if [[ "${DB_BACKUP_CREATE:-0}" == "1" ]]; then
  docker exec -e MYSQL_PWD="$DB_BACKUP_PASSWORD" "$CONTAINER" mariadb \
    --host="$DB_BACKUP_HOST" \
    --port="$REMOTE_PORT" \
    --user="$DB_BACKUP_USER" \
    --execute="CREATE DATABASE IF NOT EXISTS \`${DB_BACKUP_NAME}\` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci"
fi

dump_file="$(mktemp "$BACKUP_DIR/douyin.XXXXXX.sql")"
chmod 600 "$dump_file"
trap 'rm -f "$dump_file"' EXIT

log "导出本地库 ${LOCAL_DB}（容器 ${CONTAINER}）"
docker exec -e MYSQL_PWD="$LOCAL_ROOT_PASSWORD" "$CONTAINER" mariadb-dump \
  --single-transaction \
  --quick \
  --no-tablespaces \
  --routines \
  --triggers \
  --events \
  --skip-dump-date \
  "$LOCAL_DB" > "$dump_file"
[[ -s "$dump_file" ]] || fail "本地导出结果为空"

dump_hash="$(sha256sum "$dump_file" | awk '{print $1}')"
state_file="$BACKUP_DIR/backup-to-remote.sha"
if [[ -s "$state_file" ]] && [[ "$(cat "$state_file")" == "$dump_hash" ]]; then
  log "数据指纹未变化，跳过远程写入"
  exit 0
fi

log "导入到 ${DB_BACKUP_HOST}:${REMOTE_PORT}/${DB_BACKUP_NAME}"
docker exec -i -e MYSQL_PWD="$DB_BACKUP_PASSWORD" "$CONTAINER" mariadb \
  --host="$DB_BACKUP_HOST" \
  --port="$REMOTE_PORT" \
  --user="$DB_BACKUP_USER" \
  "${DB_BACKUP_NAME}" < "$dump_file"

printf '%s\n' "$dump_hash" > "$state_file"
chmod 600 "$state_file"
log "备份完成：${DB_BACKUP_NAME}（指纹 ${dump_hash}）"
