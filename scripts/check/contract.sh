#!/usr/bin/env bash
# 契约一致性检查：OpenAPI 里所有 operationId 描述必须标注「状态：已实现 / 计划中 / 已删除」，
# 且机器人命令 schema 的枚举必须与 Rust 侧 COMMANDS 注册表一致。
#
# 为什么需要：契约过去只在 contracts/ 里躺着，没人消费，渐渐和代码漂移 ——
# 出现过「占位接口写成可用功能」和「schema 枚举还停在草稿期」两种情况。
# 这里把它变成 CI 里会红的一步。
#
# 用法：scripts/check/contract.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

CONTRACT="contracts/pk-local/openapi.yaml"
SCHEMA="contracts/pk-local/schemas/robot-command.schema.json"
REGISTRY="rust/crates/pk-server/src/bot/commands.rs"
fail=0

echo "==> 契约文件存在性"
for f in "$CONTRACT" "$SCHEMA" "$REGISTRY"; do
  if [ -f "$f" ]; then
    echo "  ✓ $f"
  else
    echo "  ✗ 缺失：$f"
    fail=1
  fi
done
[ "$fail" = 0 ] || exit 1

echo
echo "==> 1. 每个 operationId 都必须标注状态"
# 抓出所有 description 形如「状态：xxx」的行；没有的就漏标
total_ops=$(grep -c 'operationId:' "$CONTRACT" || true)
labeled=$(grep -c 'description: *"状态：' "$CONTRACT" || true)
echo "  operation 总数: ${total_ops}，已标注状态: ${labeled}"
if [ "${total_ops}" != "${labeled}" ]; then
  # 只提示，不逐个定位（YAML 缩进不定，逐个定位容易误报）
  echo "  ✗ 有 operation 没按「状态：已实现 / 计划中 / 已删除」标注"
  fail=1
else
  echo "  ✓ 全部已标注"
fi

echo
echo "==> 2. 状态取值必须在白名单内"
bad=$(grep -o '状态：[^（(]*' "$CONTRACT" | sed 's/状态：//' | tr -d ' ' | sort -u | grep -vE '^(已实现|计划中|已删除)$' || true)
if [ -n "$bad" ]; then
  echo "  ✗ 出现未经认可的状态值：$bad"
  fail=1
else
  echo "  ✓ 状态取值合法"
fi

echo
echo "==> 3. 机器人命令 schema 枚举 vs Rust 注册表"
# 从 schema 里取 enum 条目；从 Rust 注册表里取 name / aliases 字面量
schema_cmds=$(python3 -I -c "
import json,sys
d=json.load(open('$SCHEMA',encoding='utf-8'))
for c in d['properties']['command']['enum']: print(c)
" | sort -u)

rust_cmds=$(python3 -I -c "
import re,pathlib
src=pathlib.Path('$REGISTRY').read_text(encoding='utf-8')
# 只取 COMMANDS 表之后的 name: / aliases:
body=src.split('pub const COMMANDS',1)[1] if 'pub const COMMANDS' in src else src
names=re.findall(r'name:\s*\"([^\"]+)\"',body)
al=re.findall(r'aliases:\s*&\[([^\]]*)\]',body)
out=set(names)
for a in al:
    out|=set(re.findall(r'\"([^\"]+)\"',a))
for c in sorted(out): print(c)
" | sort -u)

missing=$(comm -23 <(echo "$rust_cmds") <(echo "$schema_cmds"))
extra=$(comm -13 <(echo "$rust_cmds") <(echo "$schema_cmds"))

if [ -n "$missing" ]; then
  echo "  ✗ Rust 有、schema 缺："
  echo "$missing" | sed 's/^/      /'
  fail=1
fi
if [ -n "$extra" ]; then
  echo "  ✗ schema 有、Rust 已无（漂移）："
  echo "$extra" | sed 's/^/      /'
  fail=1
fi
if [ -z "$missing$extra" ]; then
  echo "  ✓ 枚举与注册表一致（$(echo "$rust_cmds" | grep -c . ) 条）"
fi

echo
if [ "$fail" = 0 ]; then
  echo "契约检查通过 ✓"
else
  echo "契约检查失败 ✗"
fi
exit "$fail"
