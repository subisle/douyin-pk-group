#!/usr/bin/env bash
# 把本地代码同步到开发机 64，并在需要时跑 Rust / 前端校验。
#
#   scripts/sync64.sh            # 只同步
#   scripts/sync64.sh --check    # 同步后跑 cargo fmt/clippy/test + tsc
#
# 用 rsync --delete 而不是 tar：tar 是覆盖式，本地删掉的文件在 64 上会残留，
# 编译报错会指向一个「本地已经不存在」的文件，很难查。
set -euo pipefail

HOST="${PK64_HOST:-64.188.8.149}"
PASS="${PK64_PASS:-47uUAB24ybLN3s6lMx}"
REMOTE="${PK64_DIR:-/opt/it/douyin-pk-group}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

SSH=(sshpass -p "$PASS" ssh -o StrictHostKeyChecking=no "root@$HOST")

echo "==> 同步 $ROOT -> $HOST:$REMOTE"
# 排除：VCS、构建产物、本机依赖、以及 64 上的运行时配置与数据
rsync -az --delete \
  --exclude='.git/' \
  --exclude='node_modules/' \
  --exclude='target/' \
  --exclude='dist/' \
  --exclude='*.db' \
  --exclude='.workbuddy/' \
  --exclude='*.tsbuildinfo' \
  --exclude='.env' \
  --exclude='deploy/.env' \
  --exclude='deploy/data/' \
  -e "sshpass -p '$PASS' ssh -o StrictHostKeyChecking=no" \
  "$ROOT/" "root@$HOST:$REMOTE/"

if [ "${1:-}" = "--check" ]; then
  remote() { "${SSH[@]}" "export PATH=\$HOME/.cargo/bin:\$PATH; cd '$REMOTE' && $1"; }
  echo "==> cargo fmt --all -- --check"
  remote 'cargo fmt --manifest-path rust/Cargo.toml --all -- --check'
  echo "==> cargo clippy -D warnings"
  remote 'cargo clippy --manifest-path rust/Cargo.toml --workspace --all-targets -- -D warnings'
  echo "==> cargo test --workspace"
  remote 'cargo test --manifest-path rust/Cargo.toml --workspace'
  echo "==> pnpm tsc --noEmit"
  remote 'pnpm -F @pk/web exec tsc --noEmit'
fi
echo "==> 完成"
