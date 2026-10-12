#!/usr/bin/env bash
# 147 PG 每日备份：pg_dump -Fc → gz，保留 14 天。cron:
#   30 3 * * * /opt/pk-server/backup-pg-147.sh >> /opt/pk-server/backup.log 2>&1
set -euo pipefail
cd /opt/pk-server
F="backups/douyin-$(date +%Y%m%d).dump"
docker exec pk-postgres pg_dump -U douyin_admin -d douyin -Fc | gzip > "$F.tmp" && mv "$F.tmp" "$F.gz"
sha256sum "$F.gz" > "$F.gz.sha256"
# 保留 14 天
find backups/ -name 'douyin-*.dump.gz' -mtime +14 -delete
find backups/ -name '*.sha256' -mtime +14 -delete
echo "$(date '+%F %T') 备份完成 $(du -h "$F.gz" | cut -f1)"
