#!/usr/bin/env bash
# Download the HTML code + screenshot for each Stitch screen into .stitch/designs.
#
# Auth: set STITCH_API_KEY   (X-Goog-Api-Key)
#   or  set STITCH_ACCESS_TOKEN (+ optional GOOGLE_CLOUD_PROJECT) for OAuth Bearer.
#
# Usage:
#   STITCH_API_KEY=... ./scripts/stitch-download-designs.sh
#
set -euo pipefail

PROJECT_ID="${STITCH_PROJECT_ID:-10183576689183475}"
MCP_URL="https://stitch.googleapis.com/mcp"
OUT_DIR=".stitch/designs"

SCREEN_IDS=(
  dc8b8f46faf644029f7dff6245caf032
  5d5b2838ce8e47af8ab301ee2c2f91f0
  90047d5bb0054beab6a7ee441b13bea7
  d7def2f60cec4e0dbd9db8ebdbcad4c0
  8a3d67e7ecf04d8ebc59b4423a527f48
)

AUTH_ARGS=()
if [[ -n "${STITCH_API_KEY:-}" ]]; then
  AUTH_ARGS=(-H "X-Goog-Api-Key: ${STITCH_API_KEY}")
elif [[ -n "${STITCH_ACCESS_TOKEN:-}" ]]; then
  AUTH_ARGS=(-H "Authorization: Bearer ${STITCH_ACCESS_TOKEN}")
  if [[ -n "${GOOGLE_CLOUD_PROJECT:-}" ]]; then
    AUTH_ARGS+=(-H "X-Goog-User-Project: ${GOOGLE_CLOUD_PROJECT}")
  fi
else
  echo "❌ 缺少 Stitch 认证凭据。" >&2
  echo "   请设置 STITCH_API_KEY 或 STITCH_ACCESS_TOKEN 后重试。" >&2
  echo "   例如: STITCH_API_KEY=你的key ./scripts/stitch-download-designs.sh" >&2
  exit 2
fi

mkdir -p "$OUT_DIR"
MANIFEST="$OUT_DIR/manifest.json"
: > "$MANIFEST"

i=0
for sid in "${SCREEN_IDS[@]}"; do
  i=$((i+1))
  echo "── [$i/${#SCREEN_IDS[@]}] get_screen $sid"

  resp=$(curl -sS -m 120 -X POST "$MCP_URL" \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    "${AUTH_ARGS[@]}" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":$((100+i)),\"method\":\"tools/call\",\"params\":{\"name\":\"get_screen\",\"arguments\":{\"name\":\"projects/${PROJECT_ID}/screens/${sid}\"}}}")

  if echo "$resp" | jq -e '.result.isError == true' >/dev/null 2>&1; then
    echo "   ⚠️ 服务端错误: $(echo "$resp" | jq -r '[.result.content[]? | select(.type=="text") | .text] | join(" ")' 2>/dev/null | head -c 400)"
    continue
  fi

  # Success payload arrives as `result.structuredContent` OR JSON-in-`content[].text`.
  screen=""
  if echo "$resp" | jq -e '.result.structuredContent != null' >/dev/null 2>&1; then
    screen=$(echo "$resp" | jq -c '.result.structuredContent')
  else
    screen=$(echo "$resp" | jq -r '[.result.content[]? | select(.type=="text") | .text] | join("\n")' 2>/dev/null)
  fi
  if [[ -z "$screen" || "$screen" == "null" ]]; then
    echo "   ⚠️ 无内容返回。原始响应: $(echo "$resp" | head -c 400)"
    continue
  fi

  html_url=$(printf '%s' "$screen" | jq -r '.htmlCode.downloadUrl // empty' 2>/dev/null)
  img_url=$(printf '%s' "$screen"  | jq -r '.screenshot.downloadUrl // empty' 2>/dev/null)
  title=$(printf '%s' "$screen"    | jq -r '.title // empty' 2>/dev/null)

  echo "   title: ${title:-<none>}"

  if [[ -n "$html_url" ]]; then
    curl -L -f -sS --connect-timeout 15 --compressed "$html_url" -o "$OUT_DIR/$sid.html"
    echo "   ✅ HTML -> $OUT_DIR/$sid.html"
  else
    echo "   ⚠️ 未取到 htmlCode.downloadUrl"
  fi

  if [[ -n "$img_url" ]]; then
    curl -L -f -sS --connect-timeout 15 "$img_url" -o "$OUT_DIR/$sid.png"
    echo "   ✅ 截图 -> $OUT_DIR/$sid.png"
  else
    echo "   ⚠️ 未取到 screenshot.downloadUrl"
  fi

  jq -n --arg id "$sid" --arg title "${title:-}" --arg html "$html_url" --arg img "$img_url" \
    '{id:$id,title:$title,htmlUrl:$html,imageUrl:$img}' >> "$MANIFEST"
done

# Normalize the accumulated NDJSON lines into a single JSON array.
python3 - "$MANIFEST" <<'PY'
import json,sys
p=sys.argv[1]
objs=[]
for line in open(p, encoding="utf-8"):
    line=line.strip()
    if not line:
        continue
    try:
        objs.append(json.loads(line))
    except Exception:
        pass
open(p,"w",encoding="utf-8").write(json.dumps(objs, ensure_ascii=False, indent=2))
PY

echo "✅ 完成。清单: $MANIFEST"
