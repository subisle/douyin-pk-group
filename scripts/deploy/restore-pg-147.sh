#!/usr/bin/env bash
# 147 PG 恢复：从 /opt/pk-server/backups 的每日 dump 恢复 douyin 库。
#   ./restore-pg-147.sh backups/douyin-20261012.dump.gz
set -euo pipefail
F="${1:?用法: restore-pg-147.sh backups/douyin-YYYYMMDD.dump.gz}"
cd /opt/pk-server
echo "!! 即将用 $F 覆盖 douyin 库，5 秒内 Ctrl+C 取消"
sleep 5
docker exec -i pk-postgres psql -U douyin_admin -d postgres -c "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname='douyin' AND pid<>pg_backend_pid()"
docker exec -i pk-postgres psql -U douyin_admin -d postgres -c "DROP DATABASE IF EXISTS douyin; CREATE DATABASE douyin OWNER douyin_admin"
gunzip -c "$F" | docker exec -i pk-postgres psql -U douyin_admin -d douyin -v ON_ERROR_STOP=1 -q
echo "恢复完成：$F"
