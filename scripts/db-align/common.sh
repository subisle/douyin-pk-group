#!/usr/bin/env bash
# 数据库对齐公共库（被 dbcheck / push / pull / bootstrap 等脚本 source）
#
# 设计依据：docs/17-数据库对齐与64部署方案.md §5（数据对齐）与 §9（交付物）
#
# ── 两端角色 ────────────────────────────────────────────────────────────
#   SRC（源）= 本机 PG douyin_dev（唯一事实源）
#   DST（目标）= 由 TARGET_MODE 决定：
#       local      本机另一个 PG 库（本地对练/验证用，不碰线上）
#       docker-ssh 远端 64 服务器上的 pk-postgres 容器（线上形态）
#
# ── 本地与线上配置分离 ──────────────────────────────────────────────────
#   本地参数只放 scripts/db-align/local.env（已 gitignore，**不入库**）
#   线上参数走环境变量（PK64_HOST / PK64_CONTAINER / DST_PG_*），
#   部署相关文件里不出现 127.0.0.1 / 本机路径这类本地值。
# ───────────────────────────────────────────────────────────────────────

set -Eeuo pipefail

ALIGN_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ALIGN_MANIFEST="${ALIGN_MANIFEST:-$ALIGN_ROOT/scripts/db-align/tables.manifest}"
ALIGN_BACKUP_DIR="${ALIGN_BACKUP_DIR:-$ALIGN_ROOT/backups/db-align}"

# config 按键权威拆分（docs/17 §5.2）
CONFIG_KEYS_LOCAL="announcement public_settings commission_settings bot_qq_app_id bot_qq_app_secret sqlite_carried wechat_add_qr"
CONFIG_KEYS_REMOTE="wechat_token wechat_baseurl wechat_ilink_user_id"

align_log()  { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
align_die()  { printf '错误：%s\n' "$*" >&2; exit 1; }
align_warn() { printf '警告：%s\n' "$*" >&2; }

# 载入本地参数文件（存在才载入；**已存在的环境变量优先，不覆盖**）
# 逐行解析而不是 source：① 显式传入的环境变量必须能覆盖本地文件（否则命令行改不动配置）；
# ② 与 Rust 端 config::load_dotenv 的「已存在优先」语义保持一致。
align_load_env() {
  local f="${ALIGN_ENV_FILE:-$ALIGN_ROOT/scripts/db-align/local.env}"
  [[ -f "$f" ]] || return 0
  local line key val
  while IFS= read -r line || [[ -n "$line" ]]; do
    line="${line#"${line%%[![:space:]]*}"}"   # 去前导空白
    [[ -z "$line" || "$line" == \#* ]] && continue
    line="${line#export }"
    key="${line%%=*}"
    val="${line#*=}"
    key="${key%"${key##*[![:space:]]}"}"       # 去键尾空白
    [[ "$key" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || continue
    val="${val#"${val%%[![:space:]]*}"}"
    val="${val%"${val##*[![:space:]]}"}"
    # 去成对引号
    if [[ "$val" == \"*\" || "$val" == \'*\' ]]; then val="${val:1:${#val}-2}"; fi
    if printenv "$key" >/dev/null 2>&1; then continue; fi   # 环境变量优先
    export "$key=$val"
  done < "$f"
  align_log "已载入本地参数：$f"
}

_align_find_bin() { # $1=名字 $2=环境变量名 $3=回退绝对路径
  local name="$1" var="$2" fallback="$3"
  if [[ -n "${!var:-}" ]]; then printf '%s' "${!var}"; return; fi
  if command -v "$name" >/dev/null 2>&1; then printf '%s' "$(command -v "$name")"; return; fi
  if [[ -x "$fallback" ]]; then printf '%s' "$fallback"; return; fi
  align_die "找不到 ${name}（可用环境变量 $var 指定）"
}

PSQL_BIN="$(_align_find_bin psql PSQL_BIN /opt/homebrew/opt/postgresql@17/bin/psql)"
PG_DUMP_BIN="$(_align_find_bin pg_dump PG_DUMP_BIN /opt/homebrew/opt/postgresql@17/bin/pg_dump)"
PG_RESTORE_BIN="$(_align_find_bin pg_restore PG_RESTORE_BIN /opt/homebrew/opt/postgresql@17/bin/pg_restore)"

# ── 连接参数默认值 ──────────────────────────────────────────────────────
SRC_PG_HOST="${SRC_PG_HOST:-127.0.0.1}"
SRC_PG_PORT="${SRC_PG_PORT:-3318}"
SRC_PG_DB="${SRC_PG_DB:-douyin_dev}"
SRC_PG_USER="${SRC_PG_USER:-zmy}"
TARGET_MODE="${TARGET_MODE:-local}"
DST_PG_USER="${DST_PG_USER:-zmy}"
PK64_HOST="${PK64_HOST:-}"
PK64_CONTAINER="${PK64_CONTAINER:-pk-postgres}"
DST_PG_DB="${DST_PG_DB:-douyin_dev}"
DST_PG_HOST="${DST_PG_HOST:-127.0.0.1}"
DST_PG_PORT="${DST_PG_PORT:-3318}"

# ── 两端执行器（统一读 stdin 传 SQL，避免多层引号地狱）──────────────────
src_psql() { # $1=额外 flag（如 -At），SQL 从 stdin
  local flags="${1:-}"
  PGPASSWORD="${SRC_PG_PASSWORD:-}" "$PSQL_BIN" \
    -h "$SRC_PG_HOST" -p "$SRC_PG_PORT" -U "$SRC_PG_USER" -d "$SRC_PG_DB" \
    -v ON_ERROR_STOP=1 -q $flags
}

dst_psql() {
  local flags="${1:-}"
  case "$TARGET_MODE" in
    local)
      PGPASSWORD="${DST_PG_PASSWORD:-}" "$PSQL_BIN" \
        -h "$DST_PG_HOST" -p "$DST_PG_PORT" -U "$DST_PG_USER" -d "$DST_PG_DB" \
        -v ON_ERROR_STOP=1 -q $flags
      ;;
    docker-ssh)
      [[ -n "$PK64_HOST" ]] || align_die "TARGET_MODE=docker-ssh 需要 PK64_HOST"
      ssh -o BatchMode=yes -o ConnectTimeout=10 "$PK64_HOST" \
        "docker exec -i $PK64_CONTAINER psql -U $DST_PG_USER -d $DST_PG_DB -v ON_ERROR_STOP=1 -q $flags"
      ;;
    *) align_die "未知 TARGET_MODE=${TARGET_MODE}（应为 local 或 docker-ssh）" ;;
  esac
}

src_q() { printf '%s\n' "$1" | src_psql "-At"; }   # 查询（元组输出）
dst_q() { printf '%s\n' "$1" | dst_psql "-At"; }

# ── 备份（任何写入目标端之前必须调用）──────────────────────────────────
dst_dump() { # $1=输出文件
  local out="$1"
  mkdir -p "$(dirname "$out")"
  case "$TARGET_MODE" in
    local)
      PGPASSWORD="${DST_PG_PASSWORD:-}" "$PG_DUMP_BIN" \
        -h "$DST_PG_HOST" -p "$DST_PG_PORT" -U "$DST_PG_USER" -d "$DST_PG_DB" \
        -Fc -Z 6 -f "$out"
      ;;
    docker-ssh)
      ssh -o BatchMode=yes -o ConnectTimeout=10 "$PK64_HOST" \
        "docker exec $PK64_CONTAINER pg_dump -U $DST_PG_USER -d $DST_PG_DB -Fc -Z 6" > "$out"
      ;;
  esac
  [[ -s "$out" ]] || align_die "目标端备份失败（文件为空）：$out"
  "$PG_RESTORE_BIN" -l "$out" >/dev/null || align_die "目标端备份不可读（pg_restore -l 失败）：$out"
  align_log "目标端已备份：$out"
}

src_dump() { # $1=输出文件
  local out="$1"
  mkdir -p "$(dirname "$out")"
  PGPASSWORD="${SRC_PG_PASSWORD:-}" "$PG_DUMP_BIN" \
    -h "$SRC_PG_HOST" -p "$SRC_PG_PORT" -U "$SRC_PG_USER" -d "$SRC_PG_DB" \
    -Fc -Z 6 -f "$out"
  [[ -s "$out" ]] || align_die "源端备份失败（文件为空）：$out"
  "$PG_RESTORE_BIN" -l "$out" >/dev/null || align_die "源端备份不可读：$out"
  align_log "源端已备份：$out"
}

# ── 清单解析 ────────────────────────────────────────────────────────────
manifest_lines() { # $1=class（A|B|all）
  local cls="${1:-all}"
  [[ -f "$ALIGN_MANIFEST" ]] || align_die "清单不存在：$ALIGN_MANIFEST"
  awk -F'|' -v c="$cls" '
    /^[[:space:]]*#/ { next }
    NF < 6 { next }
    (c == "all" || $2 == c) { print $1 "|" $3 "|" $4 "|" $5 "|" $6 "|" $7 }
  ' "$ALIGN_MANIFEST"
}

manifest_tables() { manifest_lines "${1:-all}" | cut -d'|' -f1; }

# 目标端表的可插入列（排除生成列；identity 列保留，用 OVERRIDING SYSTEM VALUE 显式写入）
dst_columns() { # $1=表
  dst_q "SELECT string_agg(column_name, ',' ORDER BY ordinal_position)
         FROM information_schema.columns
         WHERE table_schema='public' AND table_name='$1' AND is_generated='NEVER'"
}

# ── 安全检查 ────────────────────────────────────────────────────────────
align_guard_not_same_db() {
  if [[ "$TARGET_MODE" == "local" && "$DST_PG_DB" == "$SRC_PG_DB" ]]; then
    align_die "源库与目标库同名（${SRC_PG_DB}）：本地模式下必须指向不同库，避免自己对自己搬运"
  fi
}

align_confirm() { # $1=提示；环境变量 CONFIRM=1 或 --yes 跳过交互
  local msg="$1"
  if [[ "${CONFIRM:-0}" == "1" || "${ALIGN_ASSUME_YES:-0}" == "1" ]]; then return 0; fi
  if [[ ! -t 0 ]]; then align_die "${msg}（非交互环境请显式设 CONFIRM=1）"; fi
  read -r -p "$msg [y/N] " ans
  [[ "$ans" == "y" || "$ans" == "Y" ]] || align_die "已取消"
}

align_require_table() { # $1=表：确认目标端存在
  local n
  n="$(dst_q "SELECT count(*) FROM information_schema.tables WHERE table_schema='public' AND table_name='$1'")"
  [[ "$n" == "1" ]] || align_die "目标端缺少表 $1（先在目标端跑 pk-server migrate）"
}
