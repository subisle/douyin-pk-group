#!/usr/bin/env bash
# rk3318 一键部署：GitHub Actions 构建镜像包 → 下载 → scp → docker load → 重启容器。
# 数据库（SQLite /app/data/app.db）和 .env 都在宿主机 /opt/pk-server/data、
# /opt/pk-server/.env，重建容器不影响主播账号密码。
set -euo pipefail
cd "$(dirname "$0")/.."

REPO="${REPO:-subisle/douyin-pk-group}"
HOST="${HOST:-rk3318}"
# 内网地址不进公开仓库：HTTP 自检的目标由环境变量提供
HTTP_HOST="${RK3318_HTTP_HOST:?请设置 RK3318_HTTP_HOST（rk3318 局域网地址）}"
DIR="/opt/pk-server"
TAR="/tmp/pk-server-arm64.tar.gz"

echo "==> 1/5 触发 GitHub Actions 构建镜像包"
gh workflow run arm64-bundle.yml -R "$REPO"
sleep 5
run_id=$(gh run list --workflow=arm64-bundle.yml -R "$REPO" --limit 1 --json databaseId --jq '.[0].databaseId')
echo "    构建 run: https://github.com/$REPO/actions/runs/$run_id"
gh run watch "$run_id" -R "$REPO" --exit-status > /dev/null
echo "    构建完成 ✓"

echo "==> 2/5 下载镜像包"
gh release download arm64-bundle -R "$REPO" -p 'pk-server-arm64.tar.gz' -O "$TAR" --clobber
echo "    $(ls -lh "$TAR" | awk '{print $5}') ✓"

echo "==> 3/5 上传到 rk3318"
scp -q "$TAR" "$HOST:$DIR/"
echo "    ✓"

echo "==> 4/5 docker load + 重启容器（数据卷 / .env 不动，账号密码保留）"
ssh "$HOST" "cd $DIR && gunzip -f pk-server-arm64.tar.gz && docker load -q -i pk-server-arm64.tar && docker compose up -d --force-recreate && rm -f pk-server-arm64.tar"

echo "==> 5/5 健康检查"
sleep 8
ssh "$HOST" "docker ps --filter name=pk-server --format '{{.Status}}' | grep -q healthy && echo '容器 healthy ✓' || { echo '容器异常！'; docker logs pk-server --tail 20; exit 1; }"
curl -s -o /dev/null -w "    首页 HTTP %{http_code}\n" -m 8 http://$HTTP_HOST:8787/
curl -s -o /dev/null -w "    后台 HTTP %{http_code}\n" -m 8 http://$HTTP_HOST:8787/admin

echo "==> 部署完成：http://$HTTP_HOST:8787/"
