#!/usr/bin/env bash
# 数据库对齐同步引擎（push / pull 共用），传输层由 scripts/db-align/common.sh 提供。
#
# 机制（每张表一个事务、一次连接）：
#   源端 COPY (SELECT 列清单) TO STDOUT CSV
#     → 目标端同一 psql 会话内：建临时表 → \copy FROM STDIN → INSERT ... ON CONFLICT
#
# 两类表的 id 处理不同（这是对齐正确性的关键）：
#   upsert 模式（业务数据 A 类 + 状态型 B 类）
#       显式搬 id（OVERRIDING SYSTEM VALUE），保证主键跨环境一致；
#       identity 列不参与 DO UPDATE（GENERATED ALWAYS 不允许 UPDATE），
#       冲突命中时保留目标端既有 id。
#   insert_ignore 模式（仅追加型日志，如 access_logs）
#       **不搬 id**，让目标端 identity 自增；靠业务去重键（dedup_key）判重。
#       否则源端 id 会与目标端既有主键撞车（access_logs_pkey）。
#
# 为什么不用整库替换：会冲掉 64 上的线上注册/绑定/访问日志（docs/17 §5.4 决策 D4）。

# 方向无关的端侧 I/O
_side_q()    { if [[ "$1" == src ]]; then src_q "$2";    else dst_q "$2";    fi; }
_side_psql() { if [[ "$1" == src ]]; then src_psql "${2:-}"; else dst_psql "${2:-}"; fi; }
_side_dump() { if [[ "$1" == src ]]; then src_dump "$2"; else dst_dump "$2"; fi; }
_side_name() {
  if [[ "$1" == src ]]; then
    printf '本机源库 %s' "$SRC_PG_DB"
  elif [[ "$TARGET_MODE" == local ]]; then
    printf '目标库 %s' "$DST_PG_DB"
  else
    printf '64 容器 %s/%s' "$PK64_CONTAINER" "$DST_PG_DB"
  fi
}

# 从逗号列清单里剔除另一份逗号列清单
_cols_without() { # $1=列清单 $2=要剔除的列
  local out="" c
  local -a all=()
  IFS=',' read -r -a all <<< "$1"
  for c in "${all[@]}"; do
    [[ -n "$c" ]] || continue
    case ",$2," in *",$c,"*) continue ;; esac
    out+="${out:+,}$c"
  done
  printf '%s' "$out"
}

# 表在某端的信息
_table_cols() { # $1=side $2=表 → 可插入列（排除生成列）
  _side_q "$1" "SELECT string_agg(column_name, ',' ORDER BY ordinal_position)
                FROM information_schema.columns
                WHERE table_schema='public' AND table_name='$2' AND is_generated='NEVER'"
}
_table_identity_cols() { # $1=side $2=表
  _side_q "$1" "SELECT coalesce(string_agg(column_name, ','), '')
                FROM information_schema.columns
                WHERE table_schema='public' AND table_name='$2' AND is_identity='YES'"
}

# 同步单表：align_sync_table <表> <冲突列> <upsert|insert_ignore> <from> <to> [nullkey]
align_sync_table() {
  local t="$1" conflict="$2" mode="$3" from="$4" to="$5" nullkey="${6:-}"
  local cols identity_cols insert_cols csv rows_before rows_after rows_loaded out

  cols="$(_table_cols "$to" "$t")"
  [[ -n "$cols" ]] || align_die "表 $t 在目标端不存在或无列（先在目标端跑 pk-server migrate）"
  identity_cols="$(_table_identity_cols "$to" "$t")"

  # insert_ignore（追加型日志）：不搬 identity 列，由目标端生成 id
  if [[ "$mode" == "insert_ignore" ]]; then
    insert_cols="$(_cols_without "$cols" "$identity_cols")"
  else
    insert_cols="$cols"
  fi

  csv="$(mktemp "${TMPDIR:-/tmp}/align-$t-XXXXXX")"
  _side_q "$from" "COPY (SELECT $insert_cols FROM $t) TO STDOUT WITH (FORMAT csv)" > "$csv"

  # psql 的 \copy FROM STDIN 以单独一行 \. 结束；数据里若真出现该行会截断，先拦下来
  if grep -qxF '\.' "$csv"; then
    rm -f "$csv"
    align_die "表 $t 的导出数据里出现单独的 \\. 行，会破坏 \\copy 传输；请先人工排查"
  fi

  # 只有真的写入 identity 列时才需要 OVERRIDING SYSTEM VALUE
  local override=""
  if [[ -n "$identity_cols" ]]; then
    local first_id="${identity_cols%%,*}"
    case ",$insert_cols," in *",$first_id,"*) override="OVERRIDING SYSTEM VALUE" ;; esac
  fi

  local insert_sql
  if [[ "$mode" == "upsert" ]]; then
    # identity 列不允许 UPDATE，SET 列表要排除（冲突命中时保留目标端既有 id）
    local set_cols set_clause
    set_cols="$(_cols_without "$(_cols_without "$insert_cols" "$conflict")" "$identity_cols")"
    local -a allcols=() setparts=()
    IFS=',' read -r -a allcols <<< "$set_cols"
    local c
    for c in "${allcols[@]}"; do
      [[ -n "$c" ]] || continue
      setparts+=("$c=EXCLUDED.$c")
    done
    set_clause="$(IFS=,; printf '%s' "${setparts[*]}")"
    insert_sql="INSERT INTO $t ($insert_cols) $override SELECT $insert_cols FROM _align_tmp
                ON CONFLICT ($conflict) DO UPDATE SET $set_clause;"
  else
    # 追加型日志：conflict 列（dedup_key）可能为 NULL，而 NULL 不参与 ON CONFLICT 去重，
    # 只靠它会在每次 pull 时重复插入（access_logs 有 237 行历史 NULL）。故：
    #   conflict 非空 → 按 conflict 判重
    #   conflict 为 NULL → 退化为 nullkey 自然键判重
    if [[ -n "$nullkey" && "$conflict" != *,* ]]; then
      local nk_cond="" nk
      local -a nks=()
      IFS=',' read -r -a nks <<< "$nullkey"
      for nk in "${nks[@]}"; do
        [[ -n "$nk" ]] || continue
        nk_cond+="${nk_cond:+ AND }x.$nk IS NOT DISTINCT FROM s.$nk"
      done
      insert_sql="INSERT INTO $t ($insert_cols)
                  SELECT s.* FROM _align_tmp s
                  WHERE NOT EXISTS (
                    SELECT 1 FROM $t x
                    WHERE (s.$conflict IS NOT NULL AND x.$conflict IS NOT DISTINCT FROM s.$conflict)
                       OR (s.$conflict IS NULL AND x.$conflict IS NULL AND $nk_cond)
                  )
                  ON CONFLICT ($conflict) DO NOTHING;"
    else
      insert_sql="INSERT INTO $t ($insert_cols) $override SELECT $insert_cols FROM _align_tmp
                  ON CONFLICT ($conflict) DO NOTHING;"
    fi
  fi

  rows_before="$(_side_q "$to" "SELECT count(*) FROM $t")"

  # 一个会话内完成：建临时表 → \copy 数据 → upsert → 报告行数
  out="$(
    {
      printf 'BEGIN;\n'
      # 用 CTAS 只复制列与类型、不带任何约束：日志表模式会刻意不搬 identity 列，
      # 若用 LIKE t 会连 id 的 NOT NULL 一起复制过来，导致 \copy 报空值违约。
      printf 'CREATE TEMP TABLE _align_tmp AS SELECT %s FROM %s WITH NO DATA;\n' "$insert_cols" "$t"
      printf '\\copy _align_tmp (%s) FROM STDIN WITH (FORMAT csv)\n' "$insert_cols"
      cat "$csv"
      printf '\\.\n'
      printf '%s\n' "$insert_sql"
      printf "SELECT 'ALIGN_ROWS=' || count(*) FROM _align_tmp;\n"
      printf 'COMMIT;\n'
    } | _side_psql "$to" "-At"
  )"
  rm -f "$csv"

  rows_loaded="$(printf '%s' "$out" | sed -n 's/^ALIGN_ROWS=//p' | head -1)"
  rows_after="$(_side_q "$to" "SELECT count(*) FROM $t")"
  printf '  %-20s 搬运 %s 行 · 目标 %s → %s 行\n' "$t" "${rows_loaded:-?}" "$rows_before" "$rows_after"
}

# config 按键同步：align_sync_config <from> <to> <键...>
align_sync_config() {
  local from="$1" to="$2"; shift 2
  local -a keys=("$@")
  [[ ${#keys[@]} -gt 0 ]] || return 0

  local -a quoted=()
  local k
  for k in "${keys[@]}"; do quoted+=("'$k'"); done
  local keylist
  keylist="$(IFS=,; printf '%s' "${quoted[*]}")"

  local csv out
  csv="$(mktemp "${TMPDIR:-/tmp}/align-config-XXXXXX")"
  _side_q "$from" "COPY (SELECT cfg_key, cfg_value FROM config WHERE cfg_key IN ($keylist)) TO STDOUT WITH (FORMAT csv)" > "$csv"

  if [[ ! -s "$csv" ]]; then
    rm -f "$csv"
    printf '  %-20s 源端无这些键，跳过（%s）\n' "config" "${keys[*]}"
    return 0
  fi
  if grep -qxF '\.' "$csv"; then
    rm -f "$csv"
    align_die "config 导出数据里出现单独的 \\. 行，会破坏 \\copy 传输"
  fi

  out="$(
    {
      printf 'BEGIN;\n'
      printf 'CREATE TEMP TABLE _align_cfg (cfg_key text, cfg_value text);\n'
      printf '\\copy _align_cfg (cfg_key, cfg_value) FROM STDIN WITH (FORMAT csv)\n'
      cat "$csv"
      printf '\\.\n'
      printf 'INSERT INTO config (cfg_key, cfg_value, updated_at)\n'
      printf "SELECT cfg_key, cfg_value, to_char(now() AT TIME ZONE 'Asia/Shanghai','YYYY-MM-DD HH24:MI:SS')\n"
      printf 'FROM _align_cfg\n'
      printf 'ON CONFLICT (cfg_key) DO UPDATE SET cfg_value=EXCLUDED.cfg_value, updated_at=EXCLUDED.updated_at;\n'
      printf "SELECT 'ALIGN_ROWS=' || count(*) FROM _align_cfg;\n"
      printf 'COMMIT;\n'
    } | _side_psql "$to" "-At"
  )"
  rm -f "$csv"
  printf '  %-20s 同步 %s 个键（%s）\n' "config" \
    "$(printf '%s' "$out" | sed -n 's/^ALIGN_ROWS=//p' | head -1)" "${keys[*]}"
}

# 序列复位：upsert 显式写入 id 不会推进 identity 序列，必须按 max(id) 复位，
# 否则目标端自身写入（admin API 建档）会与显式 id 撞车（docs/17 §5.4）
align_reset_sequences() { # $1=side
  local side="$1"
  _side_psql "$side" "-At" <<'SQL' >/dev/null
DO $$
DECLARE r record; maxv bigint;
BEGIN
  FOR r IN
    SELECT c.table_name, c.column_name,
           pg_get_serial_sequence(format('%I.%I','public',c.table_name), c.column_name) AS seq
    FROM information_schema.columns c
    WHERE c.table_schema='public' AND c.is_identity='YES'
  LOOP
    CONTINUE WHEN r.seq IS NULL;
    EXECUTE format('SELECT COALESCE(max(%I),0) FROM public.%I', r.column_name, r.table_name) INTO maxv;
    IF maxv > 0 THEN
      PERFORM setval(r.seq::regclass, maxv, true);
    END IF;
  END LOOP;
END $$;
SQL
  printf '  %-20s 已按 max(id) 复位\n' "序列"
}
