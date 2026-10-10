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
# 口令不写死：仓库是公开的，硬编码等于把 root 口令发到 GitHub。
PASS="${RK_PASS:?请用环境变量提供 RK_PASS，例如 RK_PASS='xxx' scripts/deploy-host.sh}"
DIR="${RK_DIR:-/opt/pk-server}"
IMAGE="${RK_IMAGE:-pk-server:$ARCH}"
# 管理口令不写死，部署完从远端 .env 读回来（uicheck 冒烟要用）
PORT="${RK_PORT:-8888}"
BASE="http://$HOST:$PORT"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

export PATH="/opt/homebrew/bin:$PATH"
export HTTPS_PROXY="${HTTPS_PROXY:-http://127.0.0.1:7890}"
export HTTP_PROXY="$HTTPS_PROXY"

cd "$ROOT"

sha="$(git rev-parse HEAD)"
short="$(git rev-parse --short HEAD)"
echo "==> 发布提交 ${short}（${ARCH} → ${HOST}）"

# 1. 等 CI 全绿：没绿就构建出坏镜像，部署完再回滚更慢
echo "==> 等待 CI（${short}）"
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
    echo "    镜像包构建失败：run ${rid}"; gh run view "$rid" --log-failed; exit 1
  fi
  sleep 10
done
echo "    镜像包就绪（run ${rid}）"

# 3. 下载离线镜像包
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "==> 下载 $ARCH-bundle"
gh release download "$ARCH-bundle" -D "$tmp" --clobber
ls -lh "$tmp"

# 4. 传到机器、导入、重启
echo "==> 部署到 $HOST"
sshpass -p "$PASS" scp -o StrictHostKeyChecking=accept-new "$tmp/pk-server-$ARCH.tar.gz" "root@$HOST:/tmp/"
sshpass -p "$PASS" ssh -o StrictHostKeyChecking=accept-new "root@$HOST" "
  set -e
  docker load -i /tmp/pk-server-$ARCH.tar.gz
  rm -f /tmp/pk-server-$ARCH.tar.gz
  # \u515c\u5e95\uff1a\u65e9\u4e8e bundle \u5de5\u4f5c\u6d41\u52a0 tags \u7684\u955c\u50cf\u5305\u8f7d\u5165\u540e\u662f <none>:<none>\uff0c
  # compose \u5f15\u7528 pk-server:$ARCH \u4f1a\u56e0\u6b64\u53bb Docker Hub \u62c9\u540c\u540d\u955c\u50cf\u800c\u5931\u8d25\u3002
  if ! docker image inspect pk-server:$ARCH >/dev/null 2>&1; then
    dangling=\$(docker images -q --filter dangling=true | head -1)
    [ -n "\$dangling" ] && docker tag "\$dangling" pk-server:$ARCH
  fi
  cd '$DIR'
  docker compose up -d --force-recreate
  for i in \$(seq 1 30); do
    if curl -fsS -o /dev/null '$BASE/api/health'; then echo '    服务已就绪'; break; fi
    sleep 2
  done
  docker compose ps
"

# 5. 真跑一遍渲染自检：路由深链 / 导航 / 弹窗（只读模式，不写库）
echo "==> 渲染自检（uicheck-admin，只读模式）"
# 管理口令从远端 .env 读回（不落仓库）；也可 RK_ADMIN 直接指定
ADMIN="${RK_ADMIN:-$(sshpass -p "$PASS" ssh -o StrictHostKeyChecking=accept-new "root@$HOST" \
  "grep '^ADMIN_PASSWORD=' '$DIR/.env' | cut -d= -f2-" 2>/dev/null)}"
# 部署自检默认只读（UICHECK_READONLY=1）：只跑路由与渲染断言，不写 2019-01-01 测试日。
# 全量自检（含入库/删除回环）请在测试环境手动跑：UICHECK_READONLY= node scripts/uicheck-admin.mjs。
UICHECK_READONLY="${UICHECK_READONLY:-1}" \
PREFIX=/admin node scripts/uicheck-admin.mjs "$BASE" "$ADMIN"

echo "==> 完成：$BASE/admin/data  管理后台    $BASE/  展示端"
