#!/usr/bin/env bash
# 把本机 MySQL（douyin_dev，唯一事实源）整体上传到 147 流量机生产的 MariaDB。
#
#   scripts/upload-db-vps147.sh          # 交互确认后执行
#   scripts/upload-db-vps147.sh --yes    # 跳过确认
#
# 背景（docs/00 §4 单库决策 2026-10-09/10）：
# - MySQL douyin_dev 是唯一业务事实源（含运行时七表）；
#   上线 = 把这份库整体 dump → scp → 恢复进生产 MariaDB（整体替换）。
# - 生产 pk-server = 147 的 systemd 服务（/opt/pk-server），MariaDB = docker 容器 pk-mariadb。
# - schema_migrations 表的 utf8mb4_0900_ai_ci 导入前统一 sed 成 utf8mb4_unicode_ci（MariaDB 不认）。
set -euo pipefail

HOST="${RK_HOST:-147.90.235.187}"
PASS="${RK_PASS:-shuai4444}"
CONTAINER="${RK_MARIADB_CONTAINER:-pk-mariadb}"
LOCAL_DB="${LOCAL_DB:-douyin_dev}"
TARGET_DB="${TARGET_DB:-douyin}"
MY=(mysql -h127.0.0.1 -P3306 -udev -pdev12345)
DUMP=(mysqldump -h127.0.0.1 -P3306 -udev -pdev12345)
SSH=(sshpass -p "$PASS" ssh -o StrictHostKeyChecking=no "root@$HOST")
SCP=(sshpass -p "$PASS" scp -o StrictHostKeyChecking=no)

command -v mysqldump >/dev/null || { echo "缺 mysqldump"; exit 1; }
command -v sshpass >/dev/null || { echo "缺 sshpass"; exit 1; }

echo "==> 检查本机库 $LOCAL_DB"
"${MY[@]}" -e "USE \`$LOCAL_DB\`; SELECT 1;" >/dev/null 2>&1 || { echo "    连不上本机 $LOCAL_DB"; exit 1; }

echo "==> 本机行数快照"
"${MY[@]}" -N -e "
  SELECT table_name, table_rows FROM information_schema.tables
  WHERE table_schema='$LOCAL_DB' ORDER BY table_name;" |
  awk '{printf "    %-22s %s\n", $1, $2}'

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
sql="$tmp/douyin_dev.sql.gz"
echo "==> 导出 $LOCAL_DB"
"${DUMP[@]}" --single-transaction --quick --no-tablespaces \
  --set-gtid-purged=OFF "$LOCAL_DB" |
  sed 's/utf8mb4_0900_ai_ci/utf8mb4_unicode_ci/g' |
  gzip > "$sql"
ls -lh "$sql" | awk '{print "    dump:", $5, $9}'

if [ "${1:-}" != "--yes" ]; then
  printf "⚠️  将整体替换 %s:%s 的数据库「%s」，生产现有数据会被覆盖。继续？[y/N] " \
    "$HOST" "$CONTAINER" "$TARGET_DB"
  read -r ans
  [ "$ans" = "y" ] || [ "$ans" = "Y" ] || { echo "已取消"; exit 1; }
fi

echo "==> 上传到 $HOST"
"${SCP[@]}" "$sql" "root@$HOST:/tmp/douyin_dev.sql.gz"

echo "==> 恢复进 $CONTAINER（库 $TARGET_DB 整体替换）"
"${SSH[@]}" "
  set -e
  gunzip -c /tmp/douyin_dev.sql.gz | docker exec -i $CONTAINER mariadb -uroot -p\$MARIADB_ROOT_PASSWORD $TARGET_DB
  rm -f /tmp/douyin_dev.sql.gz
  systemctl restart pk-server
  sleep 3
  for i in \$(seq 1 15); do
    curl -fsS -o /dev/null http://127.0.0.1:8787/api/health && break
    sleep 2
  done
"

echo "==> 生产行数对账（InnoDB 行数为估算值，看量级即可）"
"${SSH[@]}" "docker exec $CONTAINER mariadb -uroot -p\$MARIADB_ROOT_PASSWORD $TARGET_DB -N -e 'SELECT table_name, table_rows FROM information_schema.tables WHERE table_schema=DATABASE() ORDER BY table_name;'" |
  awk '{printf "    %-22s %s\n", $1, $2}'

echo "==> 完成：数据已上传，pk-server 已重启（https://pk.subisle.com）"
