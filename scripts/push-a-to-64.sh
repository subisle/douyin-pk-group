#!/usr/bin/env bash
# Class A 业务数据推送：本机（唯一事实源）→ 目标端
#
#   scripts/push-a-to-64.sh                     # 交互确认；默认先备份目标端，推完自动对账
#   CONFIRM=1 scripts/push-a-to-64.sh           # 非交互（CI/自动化）
#   scripts/push-a-to-64.sh --no-backup --no-verify
#
# 目标端由 scripts/db-align/common.sh 的 TARGET_MODE 决定：
#   local      本机另一个 PG 库（本地对练/验证，不碰线上）
#   docker-ssh 64 服务器上的 pk-postgres 容器（线上形态）
#
# 推送内容 = tables.manifest 的 A 类表 + config 的「本机权威键」（docs/17 §5.2/§5.4）
# 推完按 max(id) 复位目标端 identity 序列，并跑一次 A 类对账。

set -Eeuo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=db-align/common.sh
source "$HERE/db-align/common.sh"
# shellcheck source=db-align/engine.sh
source "$HERE/db-align/engine.sh"

DO_BACKUP=1
DO_VERIFY=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-backup) DO_BACKUP=0; shift ;;
    --no-verify) DO_VERIFY=0; shift ;;
    --yes)       CONFIRM=1; shift ;;
    -h|--help)   sed -n '2,16p' "$0"; exit 0 ;;
    *) align_die "未知参数：$1" ;;
  esac
done

align_load_env
align_guard_not_same_db

TABLES="$(manifest_tables A)"
align_log "push 方向：$(_side_name src) → $(_side_name dst)"
align_log "A 类表：$(printf '%s' "$TABLES" | tr '\n' ' ')"

if [[ "$DO_BACKUP" == "1" ]]; then
  align_confirm "将先备份目标端，然后按 A 类表 upsert 推送；继续？"
  dst_dump "$ALIGN_BACKUP_DIR/pre-push-$(date +%Y%m%d-%H%M%S).dump"
else
  align_confirm "（已跳过备份）将按 A 类表 upsert 推送目标端；继续？"
fi

printf '\n== 推送 A 类表 ==\n'
while IFS='|' read -r t conflict _pk _cols mode nullkey; do
  align_sync_table "$t" "$conflict" "$mode" src dst "$nullkey"
done < <(manifest_lines A)

printf '\n== 推送 config（本机权威键）==\n'
# shellcheck disable=SC2086
align_sync_config src dst $CONFIG_KEYS_LOCAL

printf '\n== 目标端序列复位 ==\n'
align_reset_sequences dst

if [[ "$DO_VERIFY" == "1" ]]; then
  printf '\n== 对账（A 类）==\n'
  "$HERE/dbcheck/dbcheck.sh" --class A
fi

printf '\npush 完成。\n'
