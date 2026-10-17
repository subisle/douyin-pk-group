#!/usr/bin/env bash
# 在 64 上跑 cargo fmt，再把结果拉回本地。
#
# 本机没装 Rust 工具链，格式化只能在 64 上做。顺序很关键：
#   同步到 64 → 64 上 fmt → 拉回本地
# 如果先在 64 上 fmt 再同步，会把本地刚改的东西用 64 的旧版本覆盖掉。
set -euo pipefail

HOST="${PK64_HOST:?请设置 PK64_HOST（64 开发机地址）}"
PASS="${PK64_PASS:?请用环境变量提供 PK64_PASS}"
REMOTE="${PK64_DIR:-/opt/it/douyin-pk-group}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

"$ROOT/scripts/sync64.sh" >/dev/null
sshpass -p "$PASS" ssh -o StrictHostKeyChecking=accept-new "root@$HOST" \
  "export PATH=\$HOME/.cargo/bin:\$PATH; cargo fmt --manifest-path '$REMOTE/rust/Cargo.toml' --all && echo '64 上 fmt 完成'"
rsync -az --delete --exclude='target/' \
  -e "sshpass -p '$PASS' ssh -o StrictHostKeyChecking=accept-new" \
  "root@$HOST:$REMOTE/rust/" "$ROOT/rust/"
echo "已拉回本地"
