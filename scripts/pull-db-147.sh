#!/usr/bin/env bash
# 把 147 生产机（147.90.***.*** = https://pk.subisle.com）的 MariaDB `douyin` 库
# 拉到本机，恢复进本机 PG douyin_dev（pg-migration 分支的迁移目标库）。
#
#   PK147_PASS='<root 口令>' scripts/pull-db-147.sh
#   PK147_PASS='<root 口令>' scripts/pull-db-147.sh --dump-only   # 只拉 dump，不灌库
#   PK147_PASS='<root 口令>' scripts/pull-db-147.sh --no-backup  # 跳过灌库前的本地备份
#
# 链路：mariadb-dump（容器内，root 口令从 147 的 /opt/pk-server/.env 读，不落盘）
#       → scp 到 .dbmerge/pk147-douyin-<ts>.sql.gz
#       → scripts/mysql2pg.py 转成 PG 方言（按目标库 information_schema 对齐列）
#       → 灌进本机 PG douyin_dev（3318，先备份再灌）
#       → 行数对账 + setval 复位 identity 序列
#
# 时区：147 的 MariaDB 跑在 UTC，dump 出的 datetime(3) 是 UTC 字面量；
#       目标列 timestamptz，转换时统一按 UTC 解释（PG 会话 Asia/Shanghai 显示）。
#
# 为什么不做 DDL 转换：目标 schema 由 pk-server migrations/001_core.sql 建出最终形态
#（BIGINT IDENTITY / TIMESTAMPTZ / INET / 部分唯一索引），再灌一份 MariaDB 转来的 DDL
# 只会撞名、丢触发器。这里只搬数据。
set -euo pipefail

HOST="${PK147_HOST:?请设置 PK147_HOST（147 生产机地址）}"
PASS="${PK147_PASS:?请用环境变量提供 PK147_PASS（147 root 口令），例如 PK147_PASS='xxx' scripts/pull-db-147.sh}"
PROXY="${PK147_PROXY:-127.0.0.1:7890}"        # 空则直连
REMOTE_DIR="${PK147_REMOTE_DIR:-/opt/pk-server}"
SRC_DB="${SRC_DB:-douyin}"
CONTAINER="${PK_MARIADB_CONTAINER:-pk-mariadb}"

PG_HOST="${PG_HOST:-127.0.0.1}"
PG_PORT="${PG_PORT:-3318}"
PG_DB="${PG_DB:-douyin_dev}"
PG_USER="${PG_USER:-zmy}"
PSQL=/opt/homebrew/opt/postgresql@17/bin/psql
PGDUMP=/opt/homebrew/opt/postgresql@17/bin/pg_dump

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TS="$(date +%Y%m%d-%H%M%S)"
OUT_DIR="$ROOT/.dbmerge"
mkdir -p "$OUT_DIR"

DUMP_ONLY=0
BACKUP=1
for a in "$@"; do
  case "$a" in
    --dump-only)  DUMP_ONLY=1 ;;
    --no-backup)  BACKUP=0 ;;
    -h|--help)    sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "未知参数：$a（--dump-only / --no-backup）" >&2; exit 2 ;;
  esac
done

SSH=(sshpass -p "$PASS" ssh -o StrictHostKeyChecking=accept-new -o ConnectTimeout=20 "root@$HOST")
# 传输通道：SFTP（2026-10-12 用户要求，长传输更稳——断线可用 put -a 续传，默认重试 3 次）
# -b 会强制 BatchMode=yes（禁密码），必须显式关掉并限密码认证
SFTP=(sshpass -p "$PASS" sftp -o StrictHostKeyChecking=accept-new -o ConnectTimeout=20 \
      -o BatchMode=no -o PreferredAuthentications=password -o PubkeyAuthentication=no)
if [ -n "$PROXY" ]; then
  SSH+=(-o "ProxyCommand=nc -X connect -x $PROXY %h %p")
  SFTP+=(-o "ProxyCommand=nc -X connect -x $PROXY %h %p")
fi

# sftp_get <远程路径> <本地路径>：SFTP 下载（get），失败自动重试（断点续传 -a）
sftp_get() {
  local remote="$1" local_path="$2" attempt
  for attempt in 1 2 3; do
    # get -a：本地已存在部分文件时从断点续传（OpenSSH 8.0+）
    if printf 'get -a "%s" "%s"\n' "$remote" "$local_path" | "${SFTP[@]}" -b - "root@$HOST"; then
      return 0
    fi
    echo "    sftp 传输失败（第 $attempt 次），重试..."
    sleep 3
  done
  echo "    sftp 三次重试均失败" >&2
  return 1
}

REMOTE_ROOT_PW='$(grep "^MARIADB_ROOT_PASSWORD=" "'$REMOTE_DIR'/.env" | cut -d= -f2-)'

echo "==> 1/5 在 $HOST 上导出 MariaDB $SRC_DB"
"${SSH[@]}" "
  set -euo pipefail
  root_pw=$REMOTE_ROOT_PW
  docker exec $CONTAINER sh -c \"mariadb-dump -uroot -p\\\"\$root_pw\\\" \
    --single-transaction --quick --skip-add-locks --skip-lock-tables \
    --default-character-set=utf8mb4 --no-tablespaces '$SRC_DB'\" \
    | sed 's/utf8mb4_0900_ai_ci/utf8mb4_unicode_ci/g' > /tmp/pk147-$SRC_DB-$TS.sql
  gzip -f /tmp/pk147-$SRC_DB-$TS.sql
  gunzip -t /tmp/pk147-$SRC_DB-$TS.sql
  ls -lh /tmp/pk147-$SRC_DB-$TS.sql.gz | awk '{print \"    dump:\", \$5, \$9}'
"

echo "==> 2/5 下载 dump（SFTP，支持断点续传）"
sftp_get "/tmp/pk147-$SRC_DB-$TS.sql.gz" "$OUT_DIR/pk147-$SRC_DB-$TS.sql.gz"
"${SSH[@]}" "rm -f /tmp/pk147-$SRC_DB-$TS.sql.gz"
DUMP="$OUT_DIR/pk147-$SRC_DB-$TS.sql.gz"
ls -lh "$DUMP" | awk '{print "    本地:", $5, $9}'

if [ "$DUMP_ONLY" = 1 ]; then
  echo "==> 完成（--dump-only）：$DUMP"
  exit 0
fi

echo "==> 3/5 建 schema（pk-server migrations/001_core.sql，与生产同源）"
# 防呆（2026-10-12 PG 切换后加）：该步骤会 DROP 整个 public schema。
# 本机 douyin_dev 已是唯一事实源，不允许无确认重建——除非显式 CONFIRM_DROP=1。
if [ "${CONFIRM_DROP:-0}" != "1" ]; then
  echo "!! 即将 DROP SCHEMA public CASCADE（目标 $PG_DB@$PG_HOST:$PG_PORT）"
  echo "!! 本机库含唯一事实源数据时请改用 scripts/backup/restore-pg.sh 或先备份。"
  echo "!! 确认要清库重建请加环境变量：CONFIRM_DROP=1 $0 ..."
  exit 1
fi
# 转换器按目标库的 information_schema 对齐列，所以 schema 必须先建好；
# 顺序反了的话，DROP SCHEMA 之后转换器读不到列清单会直接失败。
if [ "$BACKUP" = 1 ]; then
  BAK="$OUT_DIR/douyin_dev-before-import-$TS.dump"
  if "$PGDUMP" -p "$PG_PORT" -h 127.0.0.1 -U "$PG_USER" -d "$PG_DB" -Fc -f "$BAK" 2>/dev/null; then
    echo "    已备份现有库 → $BAK"
  else
    echo "    （没有既有数据可备份）"
  fi
fi
"$PSQL" -p "$PG_PORT" -h 127.0.0.1 -U "$PG_USER" -d "$PG_DB" -v ON_ERROR_STOP=1 -q \
  -c "DROP SCHEMA public CASCADE; CREATE SCHEMA public;"
"$PSQL" -p "$PG_PORT" -h 127.0.0.1 -U "$PG_USER" -d "$PG_DB" -v ON_ERROR_STOP=1 -q \
  -f "$ROOT/rust/crates/pk-server/src/migrations/001_core.sql" 2>&1 | grep -v "NOTICE\|already exists\|does not exist, skipping" || true

echo "==> 4/5 转换 MariaDB → PostgreSQL（按目标库列清单对齐）"
PG_SQL="$OUT_DIR/pg-$SRC_DB-$TS.sql"
PG_HOST="$PG_HOST" PG_PORT="$PG_PORT" PG_USER="$PG_USER" \
  python3 "$ROOT/scripts/mysql2pg.py" "$DUMP" --schema-db "$PG_DB" --out "$PG_SQL"

echo "==> 灌库"
"$PSQL" -p "$PG_PORT" -h 127.0.0.1 -U "$PG_USER" -d "$PG_DB" -v ON_ERROR_STOP=1 -q \
  -f "$PG_SQL"

echo "==> 复位 identity 序列（保住 MariaDB 旧 id 后必须做）"
"$PSQL" -p "$PG_PORT" -h 127.0.0.1 -U "$PG_USER" -d "$PG_DB" -q -v ON_ERROR_STOP=1 <<SQL
DO \$\$
DECLARE t record; r record;
BEGIN
  FOR t IN SELECT table_name FROM information_schema.columns
             WHERE table_schema='public' AND is_identity='YES'
             GROUP BY table_name
  LOOP
    EXECUTE format('SELECT setval(pg_get_serial_sequence(%L, %L), COALESCE((SELECT MAX(id) FROM %I), 0) + 1, false)',
                   'public.'||t.table_name, 'id', t.table_name);
  END LOOP;
END \$\$;
SQL

echo "==> 5/5 行数对账（147 MariaDB vs 本机 PG）"
"${SSH[@]}" "docker exec $CONTAINER mariadb -uroot -p\"\$(grep '^MARIADB_ROOT_PASSWORD=' '$REMOTE_DIR/.env' | cut -d= -f2-)\" '$SRC_DB' -N -e 'SELECT table_name, table_rows FROM information_schema.tables WHERE table_schema=DATABASE() ORDER BY table_name'" \
  | awk '{printf "  147   %-24s %s\n", $1, $2}'
"$PSQL" -p "$PG_PORT" -h 127.0.0.1 -U "$PG_USER" -d "$PG_DB" -At \
  -c "SELECT relname||' '||n_live_tup FROM pg_stat_user_tables ORDER BY relname" \
  | awk '{printf "  本地  %-24s %s\n", $1, $2}'

echo "==> 完成：147 生产 douyin 已灌入本机 PG $PG_DB"
echo "    dump: $DUMP"
echo "    SQL : $PG_SQL"
