#!/usr/bin/env python3
"""MariaDB dump → 本机 PG douyin_dev 的数据迁移器。

    scripts/mysql2pg.py <dump.sql[.gz]> [--schema-db douyin_dev] [--out 文件]

为什么只搬数据不搬 DDL：目标库 schema 由 pk-server 的 migrations/001_core.sql
建出最终形态（BIGINT IDENTITY / TIMESTAMPTZ / INET / 部分唯一索引）。
从 MariaDB 转一份 DDL 过来只会撞名和丢 trigger，所以这里只按目标列清单 INSERT。

对得上的列才搬：以 --schema-db 的 information_schema 为准，MariaDB dump 里多出的列
（GENERATED ALWAYS AS 的 phone_uniq）自动丢弃，缺的列补 NULL/默认值，名字对不上的
整表跳过并列出，绝不静默错位。

时区：mariadb-dump 在容器 TIME_ZONE='+00:00' 下输出 datetime(3) 的 UTC 字面量，
目标列是 timestamptz，所以时间值一律追加 '+00' 按 UTC 解释（PG 会话按 Asia/Shanghai 显示）。
varchar 时间列（created_at VARCHAR(40) 这类）本来就是北京时间字符串，原样搬。
"""
from __future__ import annotations

import argparse
import gzip
import io
import ipaddress
import re
import subprocess
import sys
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Tuple

PSQL = "/opt/homebrew/opt/postgresql@17/bin/psql"
PG_PORT = "3318"
PG_HOST = "127.0.0.1"
PG_USER = "zmy"

CREATE_TABLE_RE = re.compile(r"^CREATE TABLE `([^`]+)` \((.*?)^\) ENGINE", re.S | re.M)
INSERT_RE = re.compile(r"^INSERT INTO `([^`]+)` VALUES\s*$")
# 顶层元组：配对括号，字符串内的括号由 _in_string 过滤
TUPLE_RE = re.compile(r"\([^()]*(?:\([^()]*\)[^()]*)*\)")
COL_RE = re.compile(r"^\s*`([^`]+)`\s+([a-zA-Z]+)(\([^)]*\))?(\s+unsigned)?(.*)$")


@dataclass
class Target:
    """目标表的一列（来自 PG information_schema）。"""

    name: str
    data_type: str
    is_identity: bool = False


@dataclass
class TablePlan:
    name: str
    cols: List[str]
    types: Dict[str, str]
    identity: bool


# ---------------------------------------------------------------- 读目标 schema


def load_target_schema(db: str) -> Dict[str, TablePlan]:
    sql = """
      SELECT c.table_name, c.column_name, c.data_type,
             CASE WHEN c.is_identity='YES' THEN 1 ELSE 0 END AS is_ident
        FROM information_schema.columns c
        JOIN information_schema.tables t
          ON t.table_schema=c.table_schema AND t.table_name=c.table_name
       WHERE c.table_schema='public' AND t.table_type='BASE TABLE'
       ORDER BY c.table_name, c.ordinal_position
    """
    out = subprocess.run(
        [PSQL, "-p", PG_PORT, "-h", PG_HOST, "-U", PG_USER, "-d", db, "-At", "-F", "\t", "-c", sql],
        check=True, text=True, capture_output=True,
    ).stdout
    plans: Dict[str, TablePlan] = {}
    for line in out.splitlines():
        parts = line.split("\t")
        if len(parts) != 4:
            continue
        tbl, col, dtype, ident = parts
        p = plans.setdefault(tbl, TablePlan(tbl, [], {}, ident == "1"))
        p.cols.append(col)
        p.types[col] = dtype
    return plans


# ---------------------------------------------------------------- MariaDB DDL 解析


def parse_mysql_columns(body: str) -> List[Tuple[str, str]]:
    """返回 (列名, 基类型)；跳过 KEY/PRIMARY/CONSTRAINT 行。

    生成列要留下：dump 的 INSERT 会把 GENERATED 列也写进去（值 NULL），
    列数对不上就按 dump 与 schema 不同步报错。渲染时再按目标列名丢弃。
    """
    cols: List[Tuple[str, str]] = []
    for line in body.split("\n"):
        s = line.strip()
        if not s or s.upper().startswith(("PRIMARY KEY", "UNIQUE KEY", "KEY ", "CONSTRAINT", "INDEX ", "FOREIGN")):
            continue
        m = COL_RE.match(line)
        if not m:
            continue
        cols.append((m.group(1), m.group(2).lower()))
    return cols


# ---------------------------------------------------------------- 值处理

_TIME_RE = re.compile(r"^\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(\.\d+)?$")


def _unescape_mysql_bytes(v: str) -> bytes:
    """解 dump 二进制列的转义文本：'\\x7f' / '\\177' / '\\0' → 原始字节。

    mariadb-dump 把 varbinary 的值写成带 MySQL 转义序列的字符串：
    '\\0' 是 NUL，'\\177' 是八进制 0x7f，'\\n' '\\r' '\\Z' 等是控制字符。
    只有按这套规则解回字节，才能拿到真正的 IP 二进制。
    """
    body = v
    if body.startswith("'") and body.endswith("'"):
        body = body[1:-1]
    out = bytearray()
    i, n = 0, len(body)
    while i < n:
        c = body[i]
        if c == "\\" and i + 1 < n:
            nxt = body[i + 1]
            if nxt == "0":
                out.append(0); i += 2; continue
            if nxt in "1234567":
                j = i + 1
                digits = ""
                while j < n and len(digits) < 3 and body[j] in "01234567":
                    digits += body[j]; j += 1
                out.append(int(digits, 8) & 0xFF); i = j; continue
            mapping = {"n": 10, "r": 13, "t": 9, "b": 8, "Z": 26, "\\": 92}
            if nxt in mapping:
                out.append(mapping[nxt]); i += 2; continue
            out.append(ord(nxt) & 0xFF); i += 2; continue
        out.append(ord(c) & 0xFF); i += 1
    return bytes(out)


def _unquote_mysql_string(v: str) -> str:
    """去掉 dump 字符串的引号并还原转义。

    读文件用的是 latin-1，所以这里拿到的每个字符对应原始一个字节；
    字符串列的真身是 UTF-8，按 UTF-8 解一次拿到正常中文。
    """
    v = v.encode("latin-1", errors="replace").decode("utf-8", errors="replace")
    body = v[1:-1]
    out: List[str] = []
    i = 0
    n = len(body)
    while i < n:
        c = body[i]
        if c == "\\" and i + 1 < n:
            nxt = body[i + 1]
            mapping = {"n": "\n", "r": "\r", "t": "\t", "0": "\0", "Z": "\x1a", "b": "\b"}
            if nxt in mapping:
                out.append(mapping[nxt])
                i += 2
                continue
            out.append(nxt)  # \' \" \\ 等
            i += 2
            continue
        if c == "'" and i + 1 < n and body[i + 1] == "'":
            out.append("'")
            i += 2
            continue
        out.append(c)
        i += 1
    return "".join(out)


def _sanitize_text(s: str) -> str:
    """把非法/不可打印字节从文本列里剔掉。

    生产库里 note 这类 varchar(512) 存的是「失败 5 行: …」的导入说明，
    个别历史行被行政表格的坏字节污染过（含不可打印控制字符和非 UTF-8 序列）。
    PG 的 text 列吃不下 NUL，控制字符在报表里也没意义，所以这里统一清掉，
    保留可见文本。宁可丢几个脏字符，也不要让整批导入失败。
    """
    if "\x00" in s:
        s = s.replace("\x00", "")
    if not any(ord(ch) < 0x20 and ch not in "\n\r\t" for ch in s):
        return s
    return "".join(ch for ch in s if ord(ch) >= 0x20 or ch in "\n\r\t")


def _pg_literal(s: str) -> str:
    return "'" + s.replace("'", "''") + "'"


def bin_to_ip_text(raw: bytes) -> str:
    """varbinary(16) → inet 文本。

    dump 里 IPv4 有两种存法：4 字节二进制（mariadb-dump 引号包裹的原始字节），
    或 16 字节里前 4 字节有效、后面补 0（INET6_ATON 的老数据）。
    都不像 IP 时兜底 hex —— 调用方要能把非 IP 的行剔掉，不能把整个导入搞炸。
    """
    for cand in (raw, raw[:4], raw.rstrip(b"\x00")):
        if len(cand) == 4:
            try:
                return str(ipaddress.IPv4Address(cand))
            except Exception:
                pass
        if len(cand) == 16:
            try:
                return str(ipaddress.IPv6Address(cand))
            except Exception:
                pass
    raise ValueError(
        f"binary 列不像 IP（{len(raw)} 字节）：{raw.hex()}；"
        "目标列是 inet，填占位值会让唯一键把行悄悄吞掉，所以直接中止"
    )


def convert_value(raw: str, mtype: str, ttype: str, col: str) -> str:
    """dump 里的一个字面量 → PG 字面量。返回可直接拼进 SQL 的文本。"""
    v = raw.strip()
    if v == "NULL":
        return "NULL"
    if v.startswith("'"):
        # 二进制列（varbinary 的 IP）在 dump 里也是引号包裹的，但内容是原始字节，
        # 必须先按 latin-1 拿回字节再转 inet/bytea，不能当文本解转义。
        if mtype in ("varbinary", "binary", "blob") and ttype == "inet":
            return _pg_literal(bin_to_ip_text(_unescape_mysql_bytes(v)))
        if mtype in ("varbinary", "binary", "blob") and ttype == "bytea":
            return r"'\\x" + _unescape_mysql_bytes(v).hex() + "'::bytea"
        s = _unquote_mysql_string(v)
        if ttype == "inet":
            # 目标列是 inet 而 MariaDB 存的是点分文本（字符集转存的那种）
            return _pg_literal(s.strip(" \t\r\n"))
        if ttype == "bytea":
            return r"'\x" + s.encode("latin-1", errors="replace").hex() + "'::bytea"
        if ttype in ("smallint", "integer", "bigint", "numeric") and s != "":
            # MySQL 的 0/1 字符串列（tinyint(1) 但存了 '0'/'1'）
            if re.fullmatch(r"-?\d+", s):
                return s
        # 目标列是 timestamptz：mariadb-dump 在容器 TIME_ZONE='+00:00' 会话下吐的是
        # UTC 墙上时间的裸串。直接塞进 timestamptz 会被会话时区（Asia/Shanghai）
        # 当成北京时间解释，绝对时刻差 8 小时。必须先按 timestamp 收下裸串，
        # 再声明它属于 UTC —— 这两步的顺序不能反。
        if ttype == "timestamp with time zone" and _TIME_RE.match(s):
            return f"({_pg_literal(s)}::timestamp AT TIME ZONE 'UTC')"
        # PG 目标是大整型而 MariaDB 用了字符串（时间戳 varchar 等）——保持字符串
        # 先清掉历史坏字节：note 这类说明列被行政表格的脏数据污染过，
        # 含不可打印控制字符，PG 的 text 吃得下但显示和转义都会出问题。
        return _pg_literal(_sanitize_text(s))
    if v.upper() == "NULL":
        return "NULL"
    if v.startswith("0x") and ttype == "bytea":
        return r"'\x" + v[2:] + "'::bytea"
    if mtype in ("varbinary", "binary", "blob") and ttype in ("inet", "bytea"):
        raw = v.encode("latin-1")
        if ttype == "inet":
            return _pg_literal(bin_to_ip_text(raw))
        return r"'\x" + raw.hex() + "'::bytea"
    if ttype in ("timestamp with time zone", "timestamp without time zone", "date") and _TIME_RE.match(v):
        # mariadb-dump 在容器 TIME_ZONE='+00:00' 会话下吐的是 UTC 墙上时间的「裸串」。
        # 直接喂 timestamptz 会被会话时区（Asia/Shanghai）按北京时间解释 → 差 8 小时。
        # 正确做法：先按 timestamp(无时区) 收下这个裸串，再声明它是 UTC。
        #   裸串::timestamp AT TIME ZONE 'UTC'  →  timestamptz（绝对时刻正确）
        # 写反成 timestamptz AT TIME ZONE 'UTC' 会再把绝对时刻当 UTC 标签贴回去，
        # 显示时反而偏移，务必分清这两步。
        if ttype != "timestamp with time zone" or " " not in v:
            return _pg_literal(v)
        return f"({_pg_literal(v)}::timestamp AT TIME ZONE 'UTC')" 
    # 数字、布尔裸值、函数默认值
    return v


# ---------------------------------------------------------------- 字符串切分


BINARY_TYPES = {"varbinary", "binary", "blob", "tinyblob", "mediumblob", "longblob", "bit"}


def split_row(text: str, mtypes: List[str]) -> List[str]:
    """按列类型切分一个元组的字段序列。

    为什么不能先按顶层逗号切：dump 对 binary/blob 列既有「加引号的转义序列」形态，
    也有「裸字节」形态，裸字节里出现 `'` `,` `(` `)` 都会让无差别切分错位
    （真实数据里就有 IPv6 含 0x27）。按列类型走，binary 列按二进制规则吃到
    下一个真正的列边界，字符串/数字列按 MySQL 字面量规则吃。
    """
    out: List[str] = []
    i, n = 0, len(text)
    col = 0
    while i < n:
        while i < n and text[i] in " \t":
            i += 1
        if i >= n:
            break
        is_binary = col < len(mtypes) and mtypes[col] in BINARY_TYPES
        if is_binary:
            val, i = _take_binary(text, i)
        else:
            val, i = _take_literal(text, i)
        out.append(val)
        col += 1
        while i < n and text[i] in " \t":
            i += 1
        if i < n and text[i] == ",":
            i += 1
    return out


def _take_binary(text: str, i: int) -> Tuple[str, int]:
    """吃一个 binary/blob 列值：加引号的转义序列，或裸字节（到顶层逗号为止）。"""
    n = len(text)
    if i < n and text[i] == "'":
        j = i + 1
        while j < n:
            if text[j] == "\\" and j + 1 < n:
                j += 2
                continue
            if text[j] == "'":
                if j + 1 < n and text[j + 1] == "'":
                    j += 2
                    continue
                return text[i : j + 1], j + 1
            j += 1
        return text[i:], n
    # 裸字节：一路吃到顶层逗号（本函数按列调用，逗号即列边界）
    j = i
    while j < n and text[j] != ",":
        j += 1
    return text[i:j].rstrip(), j


def _take_literal(text: str, i: int) -> Tuple[str, int]:
    """吃一个普通列值：MySQL 字符串字面量 / NULL / 数字 / 其它裸值。"""
    n = len(text)
    if i < n and text[i] == "'":
        j = i + 1
        while j < n:
            if text[j] == "\\" and j + 1 < n:
                j += 2
                continue
            if text[j] == "'":
                if j + 1 < n and text[j + 1] == "'":
                    j += 2
                    continue
                return text[i : j + 1], j + 1
            j += 1
        return text[i:], n
    j = i
    while j < n and text[j] != ",":
        j += 1
    return text[i:j].rstrip(), j


def string_spans(s: str) -> List[Tuple[int, int]]:
    spans: List[Tuple[int, int]] = []
    i, n = 0, len(s)
    while i < n:
        if s[i] == "'":
            j = i + 1
            while j < n:
                if s[j] == "\\":
                    j += 2; continue
                if s[j] == "'":
                    if j + 1 < n and s[j + 1] == "'":
                        j += 2; continue
                    break
                j += 1
            spans.append((i, min(j + 1, n)))
            i = j + 1
        else:
            i += 1
    return spans


def in_spans(pos: int, spans: List[Tuple[int, int]]) -> bool:
    return any(a <= pos < b for a, b in spans)


# ---------------------------------------------------------------- 主流程


def open_dump(path: str):
    """按 latin-1 读：每个字节 ↔ 一个码位，dump 里的裸二进制（varbinary 的 IP）
    能原样保住，字符串列的 UTF-8 字节也不会被替换成问号。写回文件时统一转 UTF-8。"""
    if path.endswith(".gz"):
        return io.TextIOWrapper(gzip.open(path, "rb"), encoding="latin-1")
    return open(path, "r", encoding="latin-1")


def convert(src: str, db: str, out_path: str, truncate: bool = False) -> int:
    targets = load_target_schema(db)
    skipped: List[str] = []
    total_rows = 0
    mysql_cols: Dict[str, List[Tuple[str, str]]] = {}

    out = open(out_path, "w", encoding="utf-8")
    out.write("-- scripts/mysql2pg.py 生成：MariaDB dump → PG douyin_dev 数据\n")
    out.write("BEGIN;\nSET client_min_messages = warning;\n")
    if truncate:
        for t in sorted(targets):
            if t == "schema_migrations":
                continue
            out.write(f"TRUNCATE public.{t} RESTART IDENTITY CASCADE;\n")

    fh = open_dump(src)
    cur_table: Optional[str] = None
    cur_plan: Optional[TablePlan] = None
    buf: List[str] = []
    ddl_buf: List[str] = []
    in_ddl = False
    try:
        for line in fh:
            s = line.rstrip("\n")

            # CREATE TABLE 是多行的：攒到 ") ENGINE" 那一行再解析
            if in_ddl:
                ddl_buf.append(s)
                if s.startswith(") ENGINE") or s.startswith(") DEFAULT"):
                    m = CREATE_TABLE_RE.search("\n".join(ddl_buf) + "\n")
                    if m:
                        mysql_cols[m.group(1)] = parse_mysql_columns(m.group(2))
                    in_ddl = False
                    ddl_buf = []
                continue
            if s.startswith("CREATE TABLE `"):
                ddl_buf = [s]
                in_ddl = True
                continue

            mi = INSERT_RE.match(s)
            if mi:
                tname = mi.group(1)
                if tname not in mysql_cols or tname not in targets:
                    if tname not in skipped:
                        skipped.append(tname)
                    cur_table = None
                    continue
                cur_table = tname
                cur_plan = targets[tname]
                buf = []
                continue

            if cur_table is None:
                continue

            if s.startswith("("):
                # 一个 INSERT 块 = 从 VALUES 那行起的连续元组行；到下一个非 "(" 行
                # （空行 / 下一条语句）才收块。元组内部的折行由 _iter_tuples 按
                # 「能否切出齐整字段数」判断，不数括号——二进制列里的括号会骗计数。
                buf.append(s)
                continue

            # 非数据行：INSERT 块结束，整体渲染
            if buf:
                vals = _render(cur_table, cur_plan, mysql_cols[cur_table], "\n".join(buf))
                total_rows += len(vals)
                if vals:
                    out.write(_insert_head(cur_table, cur_plan, _used_cols(cur_table, cur_plan, mysql_cols)))
                    out.write(",\n".join(vals) + ";\n")
            cur_table = None
            buf = []
    finally:
        if cur_table is not None and buf:
            vals = _render(cur_table, cur_plan, mysql_cols[cur_table], "\n".join(buf))
            total_rows += len(vals)
            if vals:
                out.write(_insert_head(cur_table, cur_plan, _used_cols(cur_table, cur_plan, mysql_cols)))
                out.write(",\n".join(vals) + ";\n")
        fh.close()
        out.write("COMMIT;\n")
        out.close()
    return total_rows


def mtypes_of(table: str, mysql_cols: Dict[str, List[Tuple[str, str]]]) -> List[str]:
    return [t for _, t in mysql_cols.get(table, [])]


def _iter_tuples(body: str, mtypes: List[str]) -> List[str]:
    """把 INSERT 块切成一个个完整元组。

    dump 的结构是一行一个元组；长元组被折行后续行不以 "(" 开头。所以按行走：
    遇到以 "(" 起头的行就开一个新元组，直到该行**按列类型能解析出齐整的字段数**
    才算这个元组写完。二进制列里的 "(" ")" 会骗过括号计数，所以不数括号。
    """
    tuples: List[str] = []
    cur: List[str] = []
    for line in body.split("\n"):
        t = line.strip(" \t\r\n").rstrip(",").rstrip(";").strip(" \t\r\n")
        if not t.startswith("("):
            # 不以 "(" 起头：只可能是上一个折行元组的续行，否则是块尾杂行
            if not cur:
                continue
            cur.append(t)
        else:
            if cur:
                # 上一个元组还没收齐就来了新元组：说明上一段的字段数判定失败。
                # 不猜，直接报错——宁可中止也不静默丢行或错位。
                raise SystemExit(
                    "dump 元组跨行后字段数始终对不上（可能是二进制列含括号/引号），"
                    f"当前片段：{''.join(cur)[:120]!r}"
                )
            cur = [t]
        cand = "".join(cur)
        parts = split_row(cand[cand.index("(") + 1 : cand.rfind(")")], mtypes)
        if len(parts) == len(mtypes):
            tuples.append(cand)
            cur = []
    if cur:
        cand = "".join(cur)
        parts = split_row(cand[cand.index("(") + 1 : cand.rfind(")")], mtypes)
        if len(parts) == len(mtypes):
            tuples.append(cand)
    return tuples


def _unbalanced(block: str) -> bool:
    """块内**引号外**的左括号是否比右括号多（元组还没写完）。

    只数引号外的：varbinary 列是裸字节，里面出现 '(' 或 ')' 很常见，
    连引号内的括号一起数会把配平判错，把后续元组全吞进一个块里。
    """
    depth = 0
    in_str = False
    i, n = 0, len(block)
    while i < n:
        c = block[i]
        if in_str:
            if c == "\\" and i + 1 < n:
                i += 2
                continue
            if c == "'":
                if i + 1 < n and block[i + 1] == "'":
                    i += 2
                    continue
                in_str = False
            i += 1
            continue
        if c == "'":
            in_str = True
        elif c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
        i += 1
    return depth > 0


def _used_cols(table: str, plan: TablePlan, mysql_cols: Dict[str, List[Tuple[str, str]]]) -> str:
    """目标列 ∩ dump 列，保持目标表列的原始顺序。"""
    have = {n for n, _ in mysql_cols.get(table, [])}
    used = [c for c in plan.cols if c in have]
    return ", ".join(f'"{c}"' for c in used)


def _insert_head(table: str, plan: TablePlan, collist: str) -> str:
    # id 是 GENERATED ALWAYS AS IDENTITY，迁移要带着 MariaDB 的旧 id 灌进去，
    # 必须 OVERRIDING SYSTEM VALUE，否则整批报 cannot insert a non-DEFAULT value。
    if plan.identity:
        return f"INSERT INTO public.{table} ({collist}) OVERRIDING SYSTEM VALUE VALUES\n"
    return f"INSERT INTO public.{table} ({collist}) VALUES\n"


def _render(table: str, plan: TablePlan, mcols: List[Tuple[str, str]], block: str) -> List[str]:
    """把一个 INSERT 块的元组渲染成 PG 值列表。"""
    mtype = dict(mcols)
    mnames = [n for n, _ in mcols]
    mtype_list = [t for _, t in mcols]
    # dump 的 INSERT 块：一行一个元组，长元组折行后续行不以 "(" 开头。
    # 按括号深度拼回完整元组再解析——不能拿正则扫整个块，
    # 二进制列里的 "(" ")" 会让配对括号的正则匹配错位置。
    body = block.strip(" \t\r\n").rstrip(",").rstrip(";")
    values: List[str] = []
    for tup in _iter_tuples(body, mtype_list):
        raw_parts = split_row(tup[1:-1], mtype_list)
        if len(raw_parts) != len(mnames):
            # 字符串/裸二进制里形如 "(...)" 的碎片：候选匹配范围可能跨过了字符串边界。
            # 真正的元组列数必然等于 CREATE TABLE 的列数，所以对不上就是匹配错了，
            # 收紧到本行重新取，而不是跳过——跳过会静默丢行。
            continue
        pg_vals: List[str] = []
        for mname, raw in zip(mnames, raw_parts):
            if mname not in plan.types:
                continue  # dump 多出的列（如生成列 phone_uniq）直接丢
            pg_vals.append(convert_value(raw, mtype[mname], plan.types[mname], mname))
        # pg_vals 的顺序 = mnames 过滤后的顺序 = _used_cols 的顺序
        values.append("(" + ", ".join(pg_vals) + ")")
    return values


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("dump")
    ap.add_argument("--schema-db", default="douyin_dev")
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    out = args.out or (args.dump + ".pg.sql")
    rows = convert(args.dump, args.schema_db, out)
    print(f"转换完成：{rows} 行 → {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
