#!/usr/bin/env bash
# 数据库对账：逐表比对「行数 + 业务列 md5 校验和」，两端不一致即 exit 1。
#
#   scripts/dbcheck/dbcheck.sh                 # 默认 A 类（push 后应一致）
#   scripts/dbcheck/dbcheck.sh --class B       # pull 后应一致
#   scripts/dbcheck/dbcheck.sh --class C       # config 按键对账（只比本机权威键）
#   scripts/dbcheck/dbcheck.sh --class all
#   scripts/dbcheck/dbcheck.sh --list          # 只列清单，不连库
#
# 两端由 scripts/db-align/common.sh 解析（本地参数 scripts/db-align/local.env，线上走环境变量）。
# 校验和口径见 scripts/dbcheck/checksum.sql：SET TIME ZONE 'UTC' 固定时区渲染，
# 只比业务列（不含 id / 时间戳），避开 touch_updated_at 触发器造成的假差异。

set -Eeuo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=../db-align/common.sh
source "$HERE/../db-align/common.sh"
# shellcheck source=../db-align/engine.sh
source "$HERE/../db-align/engine.sh"

CLASS="A"
LIST_ONLY=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --class) CLASS="${2:?--class 需要参数 A|B|C|all}"; shift 2 ;;
    --list)  LIST_ONLY=1; shift ;;
    -h|--help) sed -n '2,16p' "$0"; exit 0 ;;
    *) align_die "未知参数：$1" ;;
  esac
done

if [[ "$LIST_ONLY" == "1" ]]; then
  printf '%-22s %-4s %s\n' "表" "类" "冲突键(唯一键)"
  while IFS='|' read -r t conflict _pk _cols _mode _nullkey; do
    printf '%-22s %-4s %s\n' "$t" "$CLASS" "$conflict"
  done < <(manifest_lines "$CLASS")
  exit 0
fi

align_load_env
align_guard_not_same_db

TEMPLATE="$HERE/checksum.sql"
[[ -f "$TEMPLATE" ]] || align_die "缺少模板：$TEMPLATE"

# 把 check_cols 包成 coalesce(列::text,'<NULL>')，区分 NULL 与空串
build_cols() { # $1=逗号分隔列名
  local out="" c
  IFS=',' read -r -a _cols <<< "$1"
  for c in "${_cols[@]}"; do
    out+="${out:+,}coalesce(${c}::text,'<NULL>')"
  done
  printf '%s' "$out"
}

checksum() { # $1=side $2=表 $3=order $4=cols
  local sql
  sql="$(sed -e "s|{{TABLE}}|$2|g" -e "s|{{ORDER}}|$3|g" -e "s|{{COLS}}|$4|g" "$TEMPLATE")"
  _side_q "$1" "$sql"
}

# config（C 类）：只对账「本机权威键」——运行时键（wechat_*）两端本就该不同
config_checksum() { # $1=side
  local keylist
  keylist="$(printf "'%s'," $CONFIG_KEYS_LOCAL)"
  keylist="${keylist%,}"
  _side_q "$1" "SET TIME ZONE 'UTC';
    SELECT count(*)::text || '|' || coalesce(md5(string_agg(h, '' ORDER BY cfg_key)), 'EMPTY')
    FROM (SELECT cfg_key, md5(concat_ws('|', cfg_key, coalesce(cfg_value,''))) AS h
          FROM config WHERE cfg_key IN ($keylist)) s;"
}

printf '对账（%s 类）· 源 = %s · 目标 = %s\n' "$CLASS" "$(_side_name src)" "$(_side_name dst)"
printf '%-22s %10s %34s | %10s %34s | %s\n' "表" "源行数" "源 md5" "目标行数" "目标 md5" "结论"
printf -- '-------------------------------------------------------------------------------------------------------------\n'

diffs=0
total=0
if [[ "$CLASS" == "C" ]]; then
  s="$(config_checksum src)"; d="$(config_checksum dst)"
  printf '%-22s %10s %34s | %10s %34s | %s\n' "config(本机权威键)" \
    "${s%%|*}" "${s##*|}" "${d%%|*}" "${d##*|}" \
    "$([[ "$s" == "$d" ]] && echo OK || echo DIFF)"
  [[ "$s" == "$d" ]] || diffs=$((diffs+1))
  total=1
else
  while IFS='|' read -r t conflict pk cols mode _nullkey; do
    c="$(build_cols "$cols")"
    s="$(checksum src "$t" "$pk" "$c")"
    d="$(checksum dst "$t" "$pk" "$c")"
    total=$((total+1))
    if [[ "$s" == "$d" ]]; then verdict="OK"; else verdict="DIFF"; diffs=$((diffs+1)); fi
    printf '%-22s %10s %34s | %10s %34s | %s\n' "$t" \
      "${s%%|*}" "${s##*|}" "${d%%|*}" "${d##*|}" "$verdict"
  done < <(manifest_lines "$CLASS")
fi

printf -- '-------------------------------------------------------------------------------------------------------------\n'
if [[ "$diffs" == "0" ]]; then
  printf '对账通过：%s 张表全部一致\n' "$total"
else
  printf '对账不一致：%s/%s 张表有差异\n' "$diffs" "$total"
  exit 1
fi
