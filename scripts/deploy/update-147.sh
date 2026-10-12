#!/usr/bin/env bash
# 147 一键更新 pk-server（GHCR 镜像）。在 147 本机执行：
#   /opt/pk-server/update.sh            # 拉 latest
#   /opt/pk-server/update.sh v1.0.3     # 指定版本
# 流程：记录当前 tag → pull → compose up → migrate → 健康检查 → 失败自动回滚。
set -euo pipefail
cd /opt/pk-server

TAG="${1:-latest}"
COMPOSE="docker compose -f docker-compose.prod.yml --env-file .env.prod"
HEALTH="http://127.0.0.1:8787/api/health"
LOG=/opt/pk-server/update.log

log() { echo "$(date '+%F %T') $*" | tee -a "$LOG"; }

# 当前运行版本（回滚点）
OLD_TAG="$(docker inspect pk-server --format '{{index .Config.Labels "org.opencontainers.image.version"}}' 2>/dev/null || echo latest)"
log "=== 更新开始：$OLD_TAG → $TAG ==="

# 1. 拉新镜像（GHCR 登录态由 docker login 预先配好）
export PK_TAG="$TAG"
log "pull $TAG..."
$COMPOSE pull pk-server || { log "pull 失败，终止（服务未动）"; exit 1; }

# 2. 滚动替换
$COMPOSE up -d pk-server
sleep 5

# 3. 数据库迁移（容器内执行；PG 已就绪）
log "跑迁移..."
$COMPOSE exec -T pk-server /usr/local/bin/pk-server migrate >> "$LOG" 2>&1 \
  || log "迁移命令返回非零（可能是没有新迁移），继续健康检查"

# 4. 健康检查：60s 内 3 次通过才算成功
ok=0
for i in $(seq 1 12); do
  if curl -sf -m 5 "$HEALTH" | grep -q '"server":"ok"'; then
    ok=$((ok+1)); [ "$ok" -ge 3 ] && break
  else
    ok=0
  fi
  sleep 5
done

if [ "$ok" -ge 3 ]; then
  log "=== 更新成功：现运行 $TAG（旧版 $OLD_TAG 已由 Docker 保留，可回滚）==="
else
  log "!!! 健康检查失败，回滚到 $OLD_TAG"
  export PK_TAG="$OLD_TAG"
  $COMPOSE up -d pk-server
  sleep 8
  curl -sf -m 5 "$HEALTH" >/dev/null && log "回滚完成，服务恢复" || log "!!! 回滚后仍不健康，人工介入"
  exit 1
fi
