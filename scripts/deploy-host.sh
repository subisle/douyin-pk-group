#!/usr/bin/env bash
# 把 main 分支发布到一台自建部署机：等 CI → 触发对应架构的镜像包 → 下载 → 导入 → 重启 → 自检。
#
#   scripts/deploy-host.sh                      # 用下面默认值
#   scripts/deploy-host.sh 115.159.72.31 amd64  # 显式指定主机与架构
#
# 架构决定三件事：跑哪个 bundle 工作流、下哪个 release、docker load 什么镜像。
#
# 为什么走「CI 导出镜像包」而不是让部署机直接拉 ghcr：
# ghcr 是私有的，部署机没有 read:packages 凭据；本机也没有对应架构工具链。
# 所以由 GitHub Actions 构建并导出成能 docker load 的 tar，再离线导入。
set -euo pipefail

ARCH="${2:-amd64}"
HOST="${RK_HOST:-115.159.72.31}"
PASS="${RK_PASS:-Shuai4444@}"
DIR="${RK_DIR:-/opt/pk-server}"
IMAGE="${RK_IMAGE:-pk-server:$ARCH}"
ADMIN="${RK_ADMIN:-pk-tx-2026}"
BASE="http://$HOST:8787"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

export PATH="/opt/homebrew/bin:$PATH"
export HTTPS_PROXY="${HTTPS_PROXY:-http://127.0.0.1:7890}"
export HTTP_PROXY="$HTTPS_PROXY"

cd "$ROOT"

sha="$(git rev-parse --short HEAD)"
echo "==> 发布提交 $sha（$ARCH → $HOST）"

# 1. 等 CI 全绿：没绿就构建出坏镜像，部署完再回滚更慢
echo "==> 等待 CI（$sha）"
for i in $(seq 1 60); do
  status="$(gh run list --commit "$sha" --workflow CI --limit 1 --json status,conclusion \
    --jq '.[0] | "\(.status)/\(.conclusion // "-")"' 2>/dev/null || echo "none/none")"
  case "$status" in
    completed/success) echo "    CI 通过"; break ;;
    completed/*)      echo "    CI 失败：$status"; gh run list --commit "$sha" --workflow CI --limit 1; exit 1 ;;
    *)                sleep 10 ;;
  esac
  [ "$i" = 60 ] && { echo "    CI 超时"; exit 1; }
done

# 2. 让 CI 构建该架构的镜像包并更新 rolling release
echo "==> 触发 $ARCH 镜像包"
gh workflow run "$ARCH-bundle.yml" --ref "$(git rev-parse --abbrev-ref HEAD)"
for i in $(seq 1 60); do
  run="$(gh run list --workflow "$ARCH-bundle.yml" --limit 1 --json databaseId,status,conclusion \
    --jq '.[0] | "\(.databaseId) \(.status) \(.conclusion // "-")"')"
  set -- $run
  rid="$1"
  if [ "$2" = "completed" ]; then
    [ "$3" = "success" ] && break
    echo "    镜像包构建失败：run $rid"; gh run view "$rid" --log-failed; exit 1
  fi
  sleep 10
done
echo "    镜像包就绪（run $rid）"

# 3. 下载离线镜像包
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "==> 下载 $ARCH-bundle"
gh release download "$ARCH-bundle" -D "$tmp" --clobber
ls -lh "$tmp"

# 4. 传到机器、导入、重启
echo "==> 部署到 $HOST"
sshpass -p "$PASS" scp -o StrictHostKeyChecking=no "$tmp/pk-server-$ARCH.tar.gz" "root@$HOST:/tmp/"
sshpass -p "$PASS" ssh -o StrictHostKeyChecking=no "root@$HOST" "
  set -e
  docker load -i /tmp/pk-server-$ARCH.tar.gz
  rm -f /tmp/pk-server-$ARCH.tar.gz
  cd '$DIR'
  docker compose up -d --force-recreate
  for i in \$(seq 1 30); do
    if curl -fsS -o /dev/null '$BASE/api/health'; then echo '    服务已就绪'; break; fi
    sleep 2
  done
  docker compose ps
"

# 5. 真跑一遍渲染自检：手机 / PC / 翻页 / 切日期
echo "==> 渲染自检"
node scripts/uicheck.mjs "$BASE" "$ADMIN"

echo "==> 完成：$BASE/  后台    $BASE/s/1  公布页"
