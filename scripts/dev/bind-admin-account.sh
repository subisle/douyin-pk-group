#!/usr/bin/env bash
# 本地开发辅助：把一个管理员观看账号改绑到主播「浩月」，
# 让本地开发阶段可以用管理员账号完整验收「追分设置」链路（管理员观看账号 personId=0
# 不能设追分目标，绑到真实主播档案后 personId > 0 才能走通）。
#
# 用法：
#   ADMIN_PASSWORD=<管理口令> bash scripts/dev/bind-admin-account.sh [BASE]
# 默认 BASE=http://127.0.0.1:8787（本地 pk-server）。
#
# 可选环境变量：
#   ACCOUNT=帅帅   指定要改绑的观看账号（不指定 = 自动取第一个 is_admin=1 的账号）
#   PERSON=浩月    要绑到哪个主播（默认浩月）
#
# 前提：
#   · pk-server 已启动且配置了 ADMIN_PASSWORD（管理 API fail-closed，没配口令接口全关）；
#   · 台账（person 表）里有未删除、非离团的「浩月」。
#
# 脚本只调既有管理接口，不做任何直连库的写操作：
#   1. POST /api/auth/login                          → 管理员 token
#   2. GET  /api/admin/my-accounts                   → 挑一个管理员观看账号
#   3. GET  /api/admin/streamers?q=浩月               → 浩月的 personId
#   4. POST /api/admin/my-accounts/{account}/bind     → 改绑（成功后账号名会改成主播姓名，
#      旧 token 一并被吊销 —— 这是改绑接口的既有安全语义，见 public_teams/admin.rs）
set -euo pipefail

BASE="${2:-${1:-http://127.0.0.1:8787}}"
BASE="${BASE%/}"
: "${ADMIN_PASSWORD:?请用 ADMIN_PASSWORD=<管理口令> 运行本脚本}"

# 2026-10-16：两个历史遗留管理员账号已删除（现在用 帅帅/饭团/鹏先生
# 三个分权账号），ACCOUNT 不再写死 —— 用 ACCOUNT=xxx 指定要改绑的观看账号。
# 没指定时自动取第一个 is_admin=1 的账号，脚本在任何库上都能跑。
ACCOUNT="${ACCOUNT:-}"
PERSON="${PERSON:-浩月}"

echo "==> BASE = ${BASE}"

# 1. 管理员登录
TOKEN=$(curl -sS -X POST "${BASE}/api/auth/login" \
  -H 'Content-Type: application/json' \
  -d "{\"password\":\"${ADMIN_PASSWORD}\"}" | sed -n 's/.*"token"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
[ -n "${TOKEN}" ] || { echo "登录失败：检查 ADMIN_PASSWORD 与服务地址" >&2; exit 1; }
echo "==> 管理员登录成功"

# 2. 没指定账号时自动挑一个管理员观看账号（is_admin=1）
if [ -z "${ACCOUNT}" ]; then
  ACCOUNT=$(curl -sS "${BASE}/api/admin/my-accounts" \
    -H "Authorization: Bearer ${TOKEN}" | python3 -c '
import json, sys
d = json.load(sys.stdin)
admins = [it["name"] for it in (d.get("items") or []) if it.get("isAdmin")]
if not admins:
    print("ERR:库里没有 is_admin=1 的观看账号，用 ACCOUNT=xxx 指定", file=sys.stderr); sys.exit(1)
print(admins[0])
')
  [ -n "${ACCOUNT}" ] || { echo "没找到可改绑的管理员观看账号" >&2; exit 1; }
fi
echo "==> 改绑账号 = ${ACCOUNT}"

# 2. 查浩月的 personId（台账接口按名字过滤）
PID=$(curl -sS "${BASE}/api/admin/streamers?q=$(python3 - <<'EOF'
import urllib.parse; print(urllib.parse.quote('浩月'))
EOF
)" -H "Authorization: Bearer ${TOKEN}" | python3 -c '
import json, sys
d = json.load(sys.stdin)
items = d.get("items") or []
exact = [it for it in items if it.get("name") == "浩月"]
if not exact:
    print("ERR:台账里没有精确匹配「浩月」的主播", file=sys.stderr); sys.exit(1)
print(exact[0]["personId"])
')
[ -n "${PID}" ] || { echo "没拿到「浩月」的 personId" >&2; exit 1; }
echo "==> 浩月 personId = ${PID}"

# 3. 改绑展示端账号（成功后账号名变成「浩月」）
RESULT=$(curl -sS -X POST "${BASE}/api/admin/my-accounts/${ACCOUNT}/bind" \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer ${TOKEN}" \
  -d "{\"personId\":${PID}}")
echo "==> ${RESULT}"
echo "${RESULT}" | grep -q '"ok"[[:space:]]*:[[:space:]]*true' \
  || { echo "改绑失败，见上方响应" >&2; exit 1; }
echo "==> 完成：${ACCOUNT} 已绑定到「浩月」（账号名已改写，用浩月身份重新登录展示端即可测试追分）"
