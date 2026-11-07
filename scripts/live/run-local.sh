#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if [[ $# -lt 2 ]]; then
  echo "用法: $0 EVENT_ID ROOM_ID [ROOM_ID ...]" >&2
  echo "示例: $0 1 505899768449" >&2
  exit 2
fi

EVENT_ID="$1"
shift

# 只读取采集器需要的变量。不要 source 整个 rust/.env：里面还有带空格/中文的
# DB_LOCAL_LABEL，直接 source 会被 shell 当成命令执行（本地实测 rc=127）。
if [[ -z "${LIVE_COLLECTOR_TOKEN:-}" && -f "$ROOT/rust/.env" ]]; then
  LIVE_COLLECTOR_TOKEN="$(
    sed -n 's/^LIVE_COLLECTOR_TOKEN=//p' "$ROOT/rust/.env" | tail -1
  )"
  export LIVE_COLLECTOR_TOKEN
fi

export LIVE_API="${LIVE_API:-http://127.0.0.1:8787}"

ARGS=(--event "$EVENT_ID")
for ROOM_ID in "$@"; do
  ARGS+=(--room "$ROOM_ID")
done

# 调试参数可从环境传入，例如 LIVE_EXTRA_ARGS="--once --headed"。
if [[ -n "${LIVE_EXTRA_ARGS:-}" ]]; then
  # shellcheck disable=SC2206
  EXTRA_ARGS=(${LIVE_EXTRA_ARGS})
  ARGS+=("${EXTRA_ARGS[@]}")
fi

exec python3 "$ROOT/scripts/live/collector.py" "${ARGS[@]}"
