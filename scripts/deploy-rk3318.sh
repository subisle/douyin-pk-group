#!/usr/bin/env bash
# 一条命令把 main 分支发布到 rk3318：等 CI → 触发 arm64 镜像包 → 下载 → 导入 → 重启 → 自检。
#
#   scripts/deploy-rk3318.sh              # 默认 main 最新提交
#   scripts/deploy-rk3318.sh a21df5b      # 指定提交（默认取当前 HEAD）
#
# 为什么走「CI 导出镜像包」而不是本机构建：
# ghcr 是私有的，rk3318 没有 read:packages 凭据；本机也没有 arm64 工具链。
# 所以由 GitHub Actions 交叉编译并导出成能 docker load 的 tar，再离线导入。
set -euo pipefail

HOST="${RK_HOST:-192.168.5.12}"
PASS="${RK_PASS:-shuai}"
DIR="${RK_DIR:-/opt/pk-server}"
IMAGE="${RK_IMAGE:-pk-server:arm64}"
ADMIN="${RK_ADMIN:-pk-rk3318-2026}"
BASE="http://$HOST:8787"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

export PATH="/opt/homebrew/bin:$PATH"
export HTTPS_PROXY="${HTTPS_PROXY:-http://127.0.0.1:7890}"
export HTTP_PROXY="$HTTPS_PROXY"

cd "$ROOT"

sha="$(git rev-parse --short HEAD)"
echo "==> 发布提交 $sha"

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

# 2. 让 CI 交叉编译 arm64 并更新 rolling release
echo "==> 触发 arm64 镜像包"
gh workflow run arm64-bundle.yml --ref "$(git rev-parse --abbrev-ref HEAD)"
for i in $(seq 1 60); do
  run="$(gh run list --workflow arm64-bundle.yml --limit 1 --json databaseId,status,conclusion \
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
echo "==> 下载 arm64-bundle"
gh release download arm64-bundle -D "$tmp" --clobber
ls -lh "$tmp"

# 4. 传到机器、导入、重启
echo "==> 部署到 $HOST"
sshpass -p "$PASS" scp -o StrictHostKeyChecking=no "$tmp/pk-server-arm64.tar.gz" "root@$HOST:/tmp/"
sshpass -p "$PASS" ssh -o StrictHostKeyChecking=no "root@$HOST" "
  set -e
  docker load -i /tmp/pk-server-arm64.tar.gz
  rm -f /tmp/pk-server-arm64.tar.gz
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
