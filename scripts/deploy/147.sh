#!/usr/bin/env bash
# 一条命令把 main 分支发布到 147（147.90.235.187，现役生产 https://pk.subisle.com）。
#
#   PK147_PASS='<root 口令>' scripts/deploy/147.sh
#   PK147_PASS='<root 口令>' PK147_PROXY=127.0.0.1:7890 scripts/deploy/147.sh
#   PK147_PASS='<root 口令>' scripts/deploy/147.sh --no-migrate     # 跳过 DDL
#   PK147_PASS='<root 口令>' scripts/deploy/147.sh --skip-ci        # 用已发布的包，不触发构建
#
# 147 的形态（2026-10-09 起，见 docs/04 §「部署形态」）：
#   - pk-server = systemd 服务 /etc/systemd/system/pk-server.service，进程就是
#     /opt/pk-server/pk-server，EnvironmentFile=/opt/pk-server/pk.env，监听 127.0.0.1:8787
#   - 前端静态目录 /opt/pk-server/web（pk.env 的 WEB_DIST 指这里）= apps/douyin-web 的 dist
#   - 数据库 = 本机容器 pk-mariadb（库 douyin）；公网入口 Caddy（pk.subisle.com）
#   - 历史参考：scripts/deploy/upload-db-vps147.sh 负责把本机 douyin_dev 整体灌进这个库
#
# 构建全在 GitHub：CI 绿灯 → amd64-bundle 工作流导出「可 docker load 的镜像包」→
# 部署机 docker load → 从镜像里取出二进制与前端替换 → 跑迁移 → 重启 → 只读自检。
# 为什么不让 147 自己拉 ghcr：ghcr 上的包是私有的，机器上没有 read:packages 凭据。
#
# 可覆盖：PK147_HOST / PK147_PASS / PK147_DIR / PK147_BASE / PK147_PROXY / PK147_ADMIN
set -euo pipefail

ARCH="${PK147_ARCH:-amd64}"
HOST="${PK147_HOST:-147.90.235.187}"
# 口令不写死：仓库是公开的，硬编码等于把 root 口令发到 GitHub。
PASS="${PK147_PASS:?请用环境变量提供 PK147_PASS，例如 PK147_PASS='xxx' scripts/deploy/147.sh}"
DIR="${PK147_DIR:-/opt/pk-server}"
BASE="${PK147_BASE:-https://pk.subisle.com}"
IMAGE="${PK147_IMAGE:-pk-server:$ARCH}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

MIGRATE=1
SKIP_CI=0
for arg in "$@"; do
  case "$arg" in
    --no-migrate) MIGRATE=0 ;;
    --skip-ci)    SKIP_CI=1 ;;
    *) echo "未知参数：${arg}（可用：--no-migrate / --skip-ci）" >&2; exit 2 ;;
  esac
done

export PATH="/opt/homebrew/bin:$PATH"
# bash 在非 UTF-8 locale 下会把紧跟 $VAR 的中文字节当成变量名（$short（ → 未定义变量），
# 所以这里显式固定 locale，脚本里的中文提示才不会把变量展开弄坏。
export LANG="${LANG:-en_US.UTF-8}"
export LC_ALL="${LC_ALL:-en_US.UTF-8}"
export HTTPS_PROXY="${HTTPS_PROXY:-http://127.0.0.1:7890}"
export HTTP_PROXY="$HTTPS_PROXY"

SSH_OPTS=(-o StrictHostKeyChecking=accept-new -o ConnectTimeout=20)
if [ -n "${PK147_PROXY:-}" ]; then
  SSH_OPTS+=(-o "ProxyCommand=nc -X connect -x $PK147_PROXY %h %p")
fi
ssh_do() { sshpass -p "$PASS" ssh "${SSH_OPTS[@]}" "root@$HOST" "$@"; }
scp_do() { sshpass -p "$PASS" scp "${SSH_OPTS[@]}" "$@"; }

cd "$ROOT"
sha="$(git rev-parse HEAD)"
short="$(git rev-parse --short HEAD)"
branch="$(git rev-parse --abbrev-ref HEAD)"
echo "==> 发布提交 ${short}（${ARCH} → ${HOST}，${DIR}）"

if [ -n "$(git status --porcelain)" ]; then
  echo "    本地有未提交改动：GitHub 构建的是远端提交，先 commit + push 再发布" >&2
  exit 1
fi

if [ "$SKIP_CI" = 0 ]; then
  # 1. 等 CI 全绿：没绿就构建出坏镜像，部署完再回滚更慢
  echo "==> 等待 CI（${short}）"
  for i in $(seq 1 60); do
    status="$(gh run list --commit "$sha" --workflow CI --limit 1 --json status,conclusion \
      --jq '.[0] | "\(.status)/\(.conclusion // "-")"' 2>/dev/null || echo "none/none")"
    case "$status" in
      completed/success) echo "    CI 通过"; break ;;
      completed/*)      echo "    CI 失败：$status" >&2; gh run list --commit "$sha" --workflow CI --limit 1; exit 1 ;;
      none/none)        echo "    远端还没有这个提交的 CI：先 git push" >&2; exit 1 ;;
      *)                sleep 10 ;;
    esac
    [ "$i" = 60 ] && { echo "    CI 超时" >&2; exit 1; }
  done

  # 2. 让 CI 构建 amd64 镜像包并更新 rolling release
  echo "==> 触发 $ARCH 镜像包"
  gh workflow run "$ARCH-bundle.yml" --ref "$branch"
  rid=""
  for i in $(seq 1 90); do
    read -r rid rstatus rconclusion rsha < <(gh run list --workflow "$ARCH-bundle.yml" --limit 1 \
      --json databaseId,status,conclusion,headSha \
      --jq '.[0] | "\(.databaseId) \(.status) \(.conclusion // "-") \(.headSha)"')
    if [ "$rsha" = "$sha" ] && [ "$rstatus" = "completed" ]; then
      if [ "$rconclusion" != "success" ]; then
        echo "    镜像包构建失败：run $rid" >&2; gh run view "$rid" --log-failed; exit 1
      fi
      break
    fi
    sleep 10
  done
  [ -n "$rid" ] || { echo "    镜像包超时" >&2; exit 1; }
  echo "    镜像包就绪（run ${rid}）"
fi

# 3. 下载离线镜像包
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "==> 下载 $ARCH-bundle"
gh release download "$ARCH-bundle" -D "$tmp" --clobber --pattern "pk-server-$ARCH.tar.gz"
ls -lh "$tmp" | tail -n +2

echo "==> 上传到 $HOST"
scp_do "$tmp/pk-server-$ARCH.tar.gz" "root@$HOST:/tmp/"

# 4. 导入镜像、从镜像里取二进制 + 前端，替换 systemd 部署的文件（旧版本留备份）
echo "==> 导入镜像并替换 $DIR"
sshpass -p "$PASS" ssh "${SSH_OPTS[@]}" "root@$HOST" "bash -s" <<REMOTE
set -euo pipefail
ts=\$(date +%Y%m%d-%H%M%S)

docker load -i /tmp/pk-server-$ARCH.tar.gz
rm -f /tmp/pk-server-$ARCH.tar.gz

# 147 跑的是 systemd + 静态目录，不是容器：把镜像当「产物包」用，只取两个文件树。
docker rm -f pk-export >/dev/null 2>&1 || true
docker create --name pk-export "$IMAGE" >/dev/null
rm -rf /tmp/pk-new
mkdir -p /tmp/pk-new/web
docker cp pk-export:/usr/local/bin/pk-server /tmp/pk-new/pk-server
docker cp pk-export:/app/web/. /tmp/pk-new/web/
docker rm pk-export >/dev/null
chmod 0755 /tmp/pk-new/pk-server

# 自检产物：前端没打进去（index.html 不引 fingerprint 资源）就直接中止，不替换
grep -q 'assets/' /tmp/pk-new/web/index.html || { echo "    镜像里没有前端产物，中止" >&2; exit 1; }
/tmp/pk-new/pk-server --version >/dev/null 2>&1 || true

cd "$DIR"
cp -a pk-server "pk-server.bak-\$ts"
if [ -d web ]; then mv web "web.bak-\$ts"; fi
install -m 0755 /tmp/pk-new/pk-server pk-server
mv /tmp/pk-new/web web
rm -rf /tmp/pk-new
echo "    已替换（旧版本备份：pk-server.bak-\${ts} / web.bak-\${ts}）"
REMOTE

# 5. 迁移（DDL 只由 douyin_migrator 跑，schema_migrations 幂等）
if [ "$MIGRATE" = 1 ]; then
  echo "==> 数据库迁移（$DIR/pk.env 的 DB_MIGRATOR_*）"
  ssh_do "set -euo pipefail
    cd '$DIR'
    set -a; . ./pk.env; set +a
    ./pk-server migrate"
fi

# 6. 重启 systemd 服务并等健康
echo "==> 重启 pk-server"
ssh_do 'set -e
  systemctl restart pk-server
  for i in $(seq 1 30); do
    curl -fsS -o /dev/null http://127.0.0.1:8787/api/health && { echo "    服务已就绪"; exit 0; }
    sleep 2
  done
  systemctl status pk-server --no-pager | tail -20
  journalctl -u pk-server -n 60 --no-pager
  exit 1'

echo "==> 远端健康：$(ssh_do 'curl -fsS http://127.0.0.1:8787/api/health')"

# 导入成功后 pk-server 会异步调用 /opt/pk-server/backup-to-remote.sh。
echo "==> 安装远程备份脚本"
scp_do scripts/backup-to-remote.sh "root@$HOST:/tmp/backup-to-remote.sh"
ssh_do "set -euo pipefail
  mkdir -p '$DIR/backups'
  install -m 0755 /tmp/backup-to-remote.sh '$DIR/backup-to-remote.sh'
  rm -f /tmp/backup-to-remote.sh
  if [ -f '$DIR/backup.env' ]; then chmod 0600 '$DIR/backup.env'; fi"

# 7. 自检：管理端路由（只读模式，不写库）+ 公网前端资源可达
ADMIN="${PK147_ADMIN:-shuai4444@}"
echo "==> 管理端路由自检（uicheck-admin，只读模式）"
UICHECK_READONLY=1 PREFIX=/admin node scripts/check/uicheck-admin.mjs "$BASE" "$ADMIN"

asset="$(curl -fsS "$BASE/" | grep -o '/assets/[^"]*\.js' | head -1)"
curl -fsS -o /dev/null -w "==> 公网前端资源 $asset → HTTP %{http_code}\n" "$BASE$asset"

echo "==> 完成：$BASE/admin 管理后台    $BASE/ 展示端"
