#!/usr/bin/env bash
# 在指定本地时间启动赛事 WebSocket 采集器。
# 用法: scripts/live/schedule-local-ws.sh HH:MM EVENT_ID ROOM_ID [ROOM_ID ...]
# 可选: LIVE_SCHEDULE_DATE=YYYY-MM-DD 指定日期；LIVE_SCHEDULE_DRY_RUN=1 只校验时间。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if [[ $# -lt 3 ]]; then
  echo "用法: $0 HH:MM EVENT_ID ROOM_ID [ROOM_ID ...]" >&2
  exit 2
fi

START_TIME="$1"
EVENT_ID="$2"
shift 2
ROOM_IDS=("$@")
SCHEDULE_DATE="${LIVE_SCHEDULE_DATE:-$(date +%Y-%m-%d)}"
LOCK_FILE="/tmp/pk-live-schedule-${EVENT_ID}.pid"

if [[ -f "$LOCK_FILE" ]]; then
  OLD_PID="$(cat "$LOCK_FILE" 2>/dev/null || true)"
  if [[ "$OLD_PID" =~ ^[0-9]+$ ]] && kill -0 "$OLD_PID" 2>/dev/null; then
    echo "赛事 ${EVENT_ID} 已有自动监控进程 PID=${OLD_PID}" >&2
    exit 0
  fi
fi
printf '%s\n' "$$" > "$LOCK_FILE"
trap '[[ "$(cat "$LOCK_FILE" 2>/dev/null || true)" == "$$" ]] && rm -f "$LOCK_FILE"' EXIT INT TERM

python3 - "$SCHEDULE_DATE" "$START_TIME" "${LIVE_SCHEDULE_DRY_RUN:-0}" <<'PY'
import datetime as dt
import os
import sys
import time

date_s, time_s, dry_s = sys.argv[1:4]
try:
    target = dt.datetime.strptime(f"{date_s} {time_s}", "%Y-%m-%d %H:%M")
except ValueError as exc:
    raise SystemExit(f"时间必须是 YYYY-MM-DD HH:MM：{exc}")
now = dt.datetime.now()
print(f"[schedule] target={target.isoformat(timespec='minutes')} now={now.isoformat(timespec='seconds')}", flush=True)
remaining = (target - now).total_seconds()
if dry_s == "1":
    print(f"[schedule] dry-run wait_seconds={max(0, int(remaining))}", flush=True)
    raise SystemExit(0)
while True:
    remaining = (target - dt.datetime.now()).total_seconds()
    if remaining <= 0:
        break
    time.sleep(min(30.0, remaining))
print("[schedule] 到达目标时间，准备启动采集器", flush=True)
PY

if [[ "${LIVE_SCHEDULE_DRY_RUN:-0}" == "1" ]]; then
  exit 0
fi

# 到点后等目标服务可用；容器/服务重启时不要直接丢掉本次监控。
HEALTH_BASE="${LIVE_API:-http://127.0.0.1:8787}"
for _ in $(seq 1 120); do
  if curl -fsS --max-time 3 "${HEALTH_BASE%/}/api/health" >/dev/null 2>&1; then
    break
  fi
  sleep 2
done

exec "$ROOT/scripts/live/run-local-ws.sh" "$EVENT_ID" "${ROOM_IDS[@]}"
