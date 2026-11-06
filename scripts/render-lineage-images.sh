#!/usr/bin/env bash
# 用 wxbot 推送同一条师门日报渲染链路，给每个有直系徒弟的师傅各生成一份 PNG。
# 默认排除：薇姐、鹏先生。可覆盖：
#   LINEAGE_IMAGE_EXCLUDED_MASTERS="薇姐,鹏先生" scripts/render-lineage-images.sh
# 日报按天标注 6 小时合格、4 小时以下严重不足：
#   scripts/render-lineage-images.sh --date 2026-11-05 \
#     --qualified-today-minutes 360 --severe-today-minutes 240
# 多日总览（每天标准 × 天数，一位师傅一张图）：
#   scripts/render-lineage-images.sh --from 2026-11-01 --to 2026-11-05 \
#     --qualified-today-minutes 360 --severe-today-minutes 240
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT/rust"
export ENV_FILE="${ENV_FILE:-.env}"
export DB_LOCAL_HOST="${DB_LOCAL_HOST:-127.0.0.1}"
export DB_LOCAL_PORT="${DB_LOCAL_PORT:-3318}"
export DB_LOCAL_USER="${DB_LOCAL_USER:-pk_reader}"
export DB_LOCAL_PASSWORD="${DB_LOCAL_PASSWORD:-}"
export DB_LOCAL_NAME="${DB_LOCAL_NAME:-douyin_dev}"
export DB_WRITER_HOST="${DB_WRITER_HOST:-127.0.0.1}"
export DB_WRITER_PORT="${DB_WRITER_PORT:-3318}"
export DB_WRITER_USER="${DB_WRITER_USER:-pk_writer}"
export DB_WRITER_PASSWORD="${DB_WRITER_PASSWORD:-}"
export DB_WRITER_NAME="${DB_WRITER_NAME:-douyin_dev}"
exec cargo run -q -p pk-server --bin render_lineage_images -- "$@"
