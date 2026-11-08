#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if [[ $# -lt 1 ]]; then
  echo "用法: $0 ROOM_ID [ROOM_ID ...]" >&2
  echo "示例: $0 90968758361" >&2
  exit 2
fi

if [[ -z "${LIVE_COLLECTOR_TOKEN:-}" && -f "$ROOT/rust/.env" ]]; then
  LIVE_COLLECTOR_TOKEN="$(
    sed -n 's/^LIVE_COLLECTOR_TOKEN=//p' "$ROOT/rust/.env" | tail -1
  )"
  export LIVE_COLLECTOR_TOKEN
fi

export LIVE_API="${LIVE_API:-http://127.0.0.1:8787}"

ARGS=(--event 0)
for ROOM_ID in "$@"; do
  ARGS+=(--room "$ROOM_ID")
done

if [[ -n "${LIVE_EXTRA_ARGS:-}" ]]; then
  # shellcheck disable=SC2206
  EXTRA_ARGS=(${LIVE_EXTRA_ARGS})
  ARGS+=("${EXTRA_ARGS[@]}")
fi

exec python3 "$ROOT/scripts/live/collector.py" "${ARGS[@]}"
