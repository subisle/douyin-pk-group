#!/usr/bin/env bash
# 开发出货：Mac 交叉编译 pk-server（Linux ARM64 release）→ rsync 到 rk3328
# 用法：
#   scripts/dev/ship-rk3328.sh            # 编译 + 同步二进制 + web dist
#   scripts/dev/ship-rk3328.sh --no-build # 只同步（上次编译产物）
#   scripts/dev/ship-rk3328.sh --restart  # 同步后重启 3328 上的 pk-dev 服务
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BIN_SRC="$ROOT/rust/target/aarch64-unknown-linux-gnu/release/pk-server"
WEB_SRC="$ROOT/apps/douyin-web/dist"
RK3328_HOST="${RK3328_HOST:?请设置 RK3328_HOST（rk3328 局域网地址）}"
DEST="root@$RK3328_HOST"
REMOTE_DIR="/opt/pk-dev"
SSH="ssh -o ConnectTimeout=10 $DEST"

# 关键：用 rustup 的 cargo（Homebrew cargo 看不到 rustup 装的 target std）
export PATH="$HOME/.cargo/bin:$PATH"

if [[ "${1:-}" != "--no-build" ]]; then
  echo "==> cargo zigbuild (aarch64-linux release)"
  (cd "$ROOT/rust" && cargo zigbuild --release -p pk-server --target aarch64-unknown-linux-gnu)
fi

[[ -f "$BIN_SRC" ]] || { echo "二进制不存在：$BIN_SRC"; exit 1; }

echo "==> rsync 二进制 → $DEST:$REMOTE_DIR/"
$SSH "mkdir -p $REMOTE_DIR/web $REMOTE_DIR/data"
rsync -z --chmod=F755 "$BIN_SRC" "$DEST:$REMOTE_DIR/pk-server.new"
rsync -a --delete "$WEB_SRC/" "$DEST:$REMOTE_DIR/web/"

if [[ "${1:-}" == "--restart" ]]; then
  echo "==> 远端原子替换并重启"
  $SSH "mv $REMOTE_DIR/pk-server.new $REMOTE_DIR/pk-server && systemctl restart pk-dev"
else
  $SSH "mv $REMOTE_DIR/pk-server.new $REMOTE_DIR/pk-server"
fi
echo "==> done"
