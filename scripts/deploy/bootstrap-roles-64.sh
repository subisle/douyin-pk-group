#!/usr/bin/env bash
# 目标端角色/权限引导（幂等）：对齐 docs/17 §5.3 第 4 步 + 本机实查权限模型
#
#   scripts/deploy/bootstrap-roles-64.sh
#   PK_READER_PASSWORD='...' PK_WRITER_PASSWORD='...' scripts/deploy/bootstrap-roles-64.sh
#
# 目标端由 TARGET_MODE 决定（local=本地对练库；docker-ssh=64 容器）。
# 口令经 stdin 传入 psql，不出现在 argv / ssh 命令行里；不提供则不改口令。
# 本地参数放 scripts/db-align/local.env（不入库）；线上走环境变量。

set -Eeuo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=../db-align/common.sh
source "$HERE/../db-align/common.sh"
# shellcheck source=../db-align/engine.sh
source "$HERE/../db-align/engine.sh"

align_load_env

SQL_FILE="$HERE/bootstrap-roles-64.sql"
[[ -f "$SQL_FILE" ]] || align_die "缺少 $SQL_FILE"

APP_DB="$DST_PG_DB"
APP_USER="$DST_PG_USER"

align_log "引导目标端角色/权限：$(_side_name dst)（库=${APP_DB} 属主=${APP_USER}）"

# 占位符替换：单引号做 SQL 转义，避免口令里的引号破坏语句
sql_escape() { printf '%s' "${1//\'/\'\'}"; }
READER_PASS="$(sql_escape "${PK_READER_PASSWORD:-}")"
WRITER_PASS="$(sql_escape "${PK_WRITER_PASSWORD:-}")"

sed -e "s|:'reader_pass'|'$READER_PASS'|g" \
    -e "s|:'writer_pass'|'$WRITER_PASS'|g" \
    -e "s|:\"app_db\"|\"$APP_DB\"|g" \
    -e "s|:\"app_user\"|\"$APP_USER\"|g" \
    "$SQL_FILE" | dst_psql ""

align_log "角色/权限引导完成。"
