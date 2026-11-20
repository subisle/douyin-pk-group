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

if [[ -z "${LIVE_COLLECTOR_TOKEN:-}" && -f "$ROOT/rust/.env" ]]; then
  LIVE_COLLECTOR_TOKEN="$(
    sed -n 's/^LIVE_COLLECTOR_TOKEN=//p' "$ROOT/rust/.env" | tail -1
  )"
  export LIVE_COLLECTOR_TOKEN
fi

export LIVE_API="${LIVE_API:-http://127.0.0.1:8787}"

# 可选赛程窗口：格式均为 `YYYY-MM-DD HH:MM` 或 ISO `YYYY-MM-DDTHH:MM`。
# 未到开始时间直接退出（不在后台挂着等待）；超过结束时间也不启动。
WINDOW_STATE=$(python3 - "${LIVE_START_AT:-}" "${LIVE_END_AT:-}" <<'PY'
import datetime as dt
import sys

def parse(value):
    value = (value or "").strip()
    if not value:
        return None
    for fmt in ("%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M"):
        try:
            return dt.datetime.strptime(value, fmt)
        except ValueError:
            pass
    raise SystemExit(f"直播赛程时间格式错误：{value}")

start = parse(sys.argv[1])
end = parse(sys.argv[2])
now = dt.datetime.now()
if start and now < start:
    print("before")
elif end and now >= end:
    print("after")
else:
    print("active")
PY
)
case "$WINDOW_STATE" in
  before)
    echo "[window] 未到直播比赛时间，跳过采集启动"
    exit 0
    ;;
  after)
    echo "[window] 已超过直播比赛结束时间，跳过采集启动"
    exit 0
    ;;
esac

ARGS=(--event "$EVENT_ID")
for ROOM_ID in "$@"; do
  ARGS+=(--room "$ROOM_ID")
done

if [[ -n "${LIVE_EXTRA_ARGS:-}" ]]; then
  # shellcheck disable=SC2206
  EXTRA_ARGS=(${LIVE_EXTRA_ARGS})
  ARGS+=("${EXTRA_ARGS[@]}")
fi

if [[ -n "${LIVE_MAX_MINUTES:-}" ]]; then
  ARGS+=(--max-minutes "$LIVE_MAX_MINUTES")
fi

exec python3 "$ROOT/scripts/live/collector_ws.py" "${ARGS[@]}"
