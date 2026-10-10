#!/usr/bin/env bash
# 把本机 MySQL（douyin_dev，唯一事实源）整体上传到 RK3318 生产的 MariaDB。
#
#   scripts/upload-db-rk3318.sh          # 交互确认后执行
#   scripts/upload-db-rk3318.sh --yes    # 跳过确认（CI/脚本内调用）
#
# 背景（docs/00 §4 单库决策 2026-10-09）：
# - MySQL douyin_dev 是唯一业务事实源，不再从远程源库同步；
#   上线 = 把这份库整体 dump → scp → 恢复进生产 MariaDB（整体替换）。
# - 生产的 SQLite（分组方案 / 别名 / 上架状态）是运行时状态，不受本次上传影响。
# - schema_migrations 表是 utf8mb4_0900_ai_ci，MariaDB 不认这个 collation，
#   导入前统一改写成 utf8mb4_unicode_ci。
#
# ── 灌库前备份（P1-19）────────────────────────────────────────────────────
# DROP DATABASE 之前先在目标机上把现有库 dump 成 gz 存到 /opt/pk-server/backups/
# （文件名带时间戳），本地保留最近 5 份、更旧的自动清理；备份失败立即中止，
# 绝不在没有备份的情况下灌库。
#
# 回滚方法（灌错 / 灌坏时）：
#   ssh root@<HOST> 'cd /opt/pk-server
#     ls -lt backups/                          # 挑要回退的那份
#     docker exec -i mariadb sh -c \
#       "gunzip -c backups/douyin-<时间戳>.sql.gz | mariadb -uroot -p\$MARIADB_ROOT_PASSWORD"
#     # 注意：dump 是整库替换级别的（含 CREATE/DROP？不含），先手动 DROP 再灌：
#     docker exec mariadb sh -c \
#       "mariadb -uroot -p\$MARIADB_ROOT_PASSWORD -e \"DROP DATABASE IF EXISTS douyin; \
#        CREATE DATABASE douyin CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci\""
#     gunzip -c backups/douyin-<时间戳>.sql.gz | docker exec -i mariadb sh -c \
#       "mariadb -uroot -p\$MARIADB_ROOT_PASSWORD douyin"
#     docker compose restart pk-server'
# ─────────────────────────────────────────────────────────────────────────
set -euo pipefail

HOST="${RK_HOST:-192.168.5.12}"
PASS="${RK_PASS:?请用环境变量提供 RK_PASS}"
DIR="${RK_DIR:-/opt/pk-server}"                 # 生产 compose 目录
CONTAINER="${RK_MARIADB_CONTAINER:-mariadb}"    # 生产 MariaDB 容器名
LOCAL_DB="${LOCAL_DB:-douyin_dev}"              # 本机库名
TARGET_DB="${TARGET_DB:-douyin}"                # 生产库名（deploy/rk3318）
BACKUP_DIR="${RK_BACKUP_DIR:-/opt/pk-server/backups}"  # 目标机备份目录
BACKUP_KEEP="${RK_BACKUP_KEEP:-5}"              # 目标机上保留的备份份数
MY=(mysql -h127.0.0.1 -P3306 -udev -pdev12345)
DUMP=(mysqldump -h127.0.0.1 -P3306 -udev -pdev12345)
SSH=(sshpass -p "$PASS" ssh -o StrictHostKeyChecking=accept-new "root@$HOST")
SCP=(sshpass -p "$PASS" scp -o StrictHostKeyChecking=accept-new)

command -v mysqldump >/dev/null || { echo "缺 mysqldump"; exit 1; }
command -v sshpass >/dev/null || { echo "缺 sshpass"; exit 1; }

echo "==> 检查本机库 $LOCAL_DB"
"${MY[@]}" -e "USE \`$LOCAL_DB\`; SELECT 1;" >/dev/null 2>&1 || {
  echo "    连不上本机 $LOCAL_DB"; exit 1
}

# 1. 导出前快照：各表行数（也是传完后的对账基准）
echo "==> 本机行数快照"
"${MY[@]}" -N -e "
  SELECT table_name, table_rows FROM information_schema.tables
  WHERE table_schema='$LOCAL_DB' ORDER BY table_name;" |
  awk '{printf "    %-22s %s\n", $1, $2}'

# 2. dump（单事务一致性快照；collation 改写适配 MariaDB）
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
sql="$tmp/douyin_dev.sql.gz"
echo "==> 导出 $LOCAL_DB"
"${DUMP[@]}" --single-transaction --quick --no-tablespaces \
  --set-gtid-purged=OFF "$LOCAL_DB" |
  sed 's/utf8mb4_0900_ai_ci/utf8mb4_unicode_ci/g' |
  gzip > "$sql"
ls -lh "$sql" | awk '{print "    dump:", $5, $9}'

# 3. 确认：生产库将被整体替换
if [ "${1:-}" != "--yes" ]; then
  printf "⚠️  将整体替换 %s:%s 的数据库「%s」（替换前会先在目标机备份到 %s）。继续？[y/N] " \
    "$HOST" "$CONTAINER" "$TARGET_DB" "$BACKUP_DIR"
  read -r ans
  [ "$ans" = "y" ] || [ "$ans" = "Y" ] || { echo "已取消"; exit 1; }
fi

# 4. 上传 + 目标机备份现有库 + 恢复 + 重启 + 对账
echo "==> 上传到 $HOST"
"${SCP[@]}" "$sql" "root@$HOST:/tmp/douyin_dev.sql.gz"

echo "==> 在 $HOST 上备份现有库（失败即中止，不灌库）"
"${SSH[@]}" "
  set -e
  mkdir -p '$BACKUP_DIR'
  chmod 700 '$BACKUP_DIR'
  if docker exec $CONTAINER mariadb -uroot -p\$MARIADB_ROOT_PASSWORD -N -e \
       \"SELECT SCHEMA_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME='$TARGET_DB';\" |
       grep -q .; then
    bak='$BACKUP_DIR/$TARGET_DB-'\$(date +%Y%m%d-%H%M%S)'.sql.gz'
    echo \"    备份 \$TARGET_DB → \$bak\"
    docker exec $CONTAINER sh -c \
      \"mariadb-dump -uroot -p\\\$MARIADB_ROOT_PASSWORD --single-transaction --quick \\
         --routines --triggers '$TARGET_DB'\" | gzip > \"\$bak\"
    # 备份完整性自检：解压能走通且非空才算备份成功，否则立即中止
    [ -s \"\$bak\" ] && gunzip -t \"\$bak\"
    ls -lh \"\$bak\" | awk '{print \"    备份大小:\", \$5, \$9}'
    # 只保留最近 $BACKUP_KEEP 份，更旧的清掉
    ls -1t '$BACKUP_DIR'/$TARGET_DB-*.sql.gz 2>/dev/null | tail -n +$((BACKUP_KEEP + 1)) | xargs -r rm -f
  else
    echo \"    目标库 $TARGET_DB 不存在（全新部署），跳过备份\"
  fi
"

echo "==> 恢复进 $CONTAINER（库 $TARGET_DB 整体替换）"
"${SSH[@]}" "
  set -e
  docker exec -i $CONTAINER sh -c '
    mariadb -uroot -p\$MARIADB_ROOT_PASSWORD -e \"
      DROP DATABASE IF EXISTS $TARGET_DB;
      CREATE DATABASE $TARGET_DB CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;\"
    gunzip -c /tmp/douyin_dev.sql.gz | mariadb -uroot -p\$MARIADB_ROOT_PASSWORD $TARGET_DB
    rm -f /tmp/douyin_dev.sql.gz'
  cd '$DIR' && docker compose restart pk-server >/dev/null
  sleep 3
  for i in \$(seq 1 15); do
    curl -fsS -o /dev/null http://127.0.0.1:8787/api/health && break
    sleep 2
  done
"

echo "==> 生产行数对账（InnoDB 行数为估算值，看量级即可）"
"${SSH[@]}" "docker exec $CONTAINER mariadb -uroot -p\$MARIADB_ROOT_PASSWORD $TARGET_DB -N -e 'SELECT table_name, table_rows FROM information_schema.tables WHERE table_schema=DATABASE() ORDER BY table_name;'" |
  awk '{printf "    %-22s %s\n", $1, $2}'

echo "==> 完成：数据已上传，pk-server 已重启（灌库前的备份在 $HOST:$BACKUP_DIR）"
