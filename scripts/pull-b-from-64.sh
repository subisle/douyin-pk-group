#!/usr/bin/env bash
# Class B 运行时数据回拉：目标端（64 生产）→ 本机开发库
#
#   scripts/pull-b-from-64.sh                   # 交互确认；默认先备份本机源库，拉完自动对账
#   CONFIRM=1 scripts/pull-b-from-64.sh
#   scripts/pull-b-from-64.sh --no-backup --no-verify
#
# 目标端由 TARGET_MODE 决定（local=本地对练；docker-ssh=64 容器）。
# 回拉内容 = tables.manifest 的 B 类表 + config 的「运行时键」（wechat_token 等，docs/17 §5.2）
# 回拉后按 max(id) 复位本机 identity 序列，并跑一次 B 类对账。
#
# 注意：只动 B 类表与运行时 config 键，**不动 A 类业务数据与 schema_migrations**。

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

TABLES="$(manifest_tables B)"
align_log "pull 方向：$(_side_name dst) → $(_side_name src)"
align_log "B 类表：$(printf '%s' "$TABLES" | tr '\n' ' ')"

if [[ "$DO_BACKUP" == "1" ]]; then
  align_confirm "将先备份本机源库，然后把 B 类运行时数据拉回本机；继续？"
  src_dump "$ALIGN_BACKUP_DIR/pre-pull-$(date +%Y%m%d-%H%M%S).dump"
else
  align_confirm "（已跳过备份）将把 B 类运行时数据拉回本机源库；继续？"
fi

printf '\n== 回拉 B 类表 ==\n'
while IFS='|' read -r t conflict _pk _cols mode nullkey; do
  align_sync_table "$t" "$conflict" "$mode" dst src "$nullkey"
done < <(manifest_lines B)

printf '\n== 回拉 config（运行时键）==\n'
# shellcheck disable=SC2086
align_sync_config dst src $CONFIG_KEYS_REMOTE

printf '\n== 本机序列复位 ==\n'
align_reset_sequences src

if [[ "$DO_VERIFY" == "1" ]]; then
  printf '\n== 对账（B 类）==\n'
  "$HERE/dbcheck/dbcheck.sh" --class B
fi

printf '\npull 完成。\n'
