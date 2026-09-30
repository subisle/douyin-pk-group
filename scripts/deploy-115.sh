#!/usr/bin/env bash
# 直连部署到腾讯云 115.159.72.31，公网端口 6666。
# 用法：scripts/deploy-115.sh
# 可覆盖：DEPLOY_HOST / DEPLOY_PASS / DEPLOY_DIR / DEPLOY_PORT / DEPLOY_ADMIN
set -euo pipefail

HOST="${DEPLOY_HOST:-115.159.72.31}"
# 密码不写死：仓库是公开的，写在这里等于把 root 口令发到 GitHub。
# 用法：DEPLOY_PASS='<root 密码>' scripts/deploy-115.sh
PASS="${DEPLOY_PASS:?请用环境变量提供 DEPLOY_PASS，例如 DEPLOY_PASS='xxx' scripts/deploy-115.sh}"
DIR="${DEPLOY_DIR:-/opt/pk-server}"
PORT="${DEPLOY_PORT:-6666}"
ADMIN="${DEPLOY_ADMIN:-}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [ -z "$ADMIN" ]; then
  ADMIN="$(openssl rand -hex 12)"
fi

SSH_OPTS=(-o StrictHostKeyChecking=no -o ConnectTimeout=15)
run_remote() {
  sshpass -p "$PASS" ssh "${SSH_OPTS[@]}" "root@$HOST" "$1"
}

echo "==> 检查远端环境"
run_remote 'set -e; command -v docker >/dev/null; docker compose version >/dev/null; mkdir -p '"$DIR"'/data'

echo "==> 同步源码到 $DIR/src"
rsync -az --delete \
  --exclude .git \
  --exclude node_modules \
  --exclude 'apps/web/dist' \
  --exclude 'apps/server/data' \
  --exclude 'rust/target' \
  --exclude 'rust/data' \
  "$ROOT/" "root@$HOST:$DIR/src/"

echo "==> 生成远端配置"
run_remote "set -e
  cd '$DIR/src/deploy'
  if [ -f .env ]; then
    cp .env .env.bak
  elif [ -f ../../../apps/server/.env ]; then
    cp ../../../apps/server/.env .env
  else
    cp .env.example .env
  fi
  sed -i '/^PORT=/d; /^SQLITE_PATH=/d; /^PK_PORT=/d' .env
  cat >> .env <<'ENV'
PORT=8787
SQLITE_PATH=/app/data/app.db
PK_PORT=$PORT
ENV
  if ! grep -q '^ADMIN_PASSWORD=' .env; then
    echo \"ADMIN_PASSWORD=$ADMIN\" >> .env
  fi
"

echo "==> 远端构建并启动（公网端口 $PORT）"
run_remote "set -e
  cd '$DIR/src'
  docker compose -f deploy/docker-compose.yml up -d --build
  for i in \$(seq 1 60); do
    if curl -fsS \"http://127.0.0.1:8787/api/health\" >/dev/null; then
      echo '服务已就绪'
      exit 0
    fi
    sleep 2
  done
  docker compose -f deploy/docker-compose.yml ps
  docker compose -f deploy/docker-compose.yml logs --tail=100 pk-server
  exit 1
"

echo "==> 完成"
echo "服务地址：http://$HOST:$PORT/"
echo "管理后台：http://$HOST:$PORT/"
if [ -z "${DEPLOY_ADMIN:-}" ]; then
  echo "本次生成的 ADMIN_PASSWORD：$ADMIN"
fi
