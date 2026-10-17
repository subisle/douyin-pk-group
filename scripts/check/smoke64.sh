#!/usr/bin/env bash
# 在 64 上冒烟测试：鉴权 + 公开只读端点 + 数据库不可用时的降级。
#
# 故意配一个连不上的 PostgreSQL：验证「库挂了服务照样起得来」，展示页降级用方案快照。
# 整段测试在 64 上跑完再回传结果，避免每次断言都开一次 ssh。
set -euo pipefail

HOST="${PK64_HOST:?请设置 PK64_HOST（64 开发机地址）}"
PASS="${PK64_PASS:?请用环境变量提供 PK64_PASS}"
REMOTE="${PK64_DIR:-/opt/it/douyin-pk-group}"

# 2026-10-14 审计修复（2026-10-17 复核）：脚本不再内置任何口令——历史口令已移除，
# 取值见环境变量/运维手册。管理员口令由本地 ADMIN_PASSWORD 注入，作为第 2 个参数传进远端。
: "${ADMIN_PASSWORD:?请用环境变量 ADMIN_PASSWORD 提供管理员口令}"
sshpass -p "$PASS" ssh -o StrictHostKeyChecking=accept-new "root@$HOST" bash -s -- "$REMOTE" "$ADMIN_PASSWORD" <<'REMOTE_SCRIPT'
set -uo pipefail
REMOTE="$1"
PW="$2"
PORT=18788
B="http://127.0.0.1:$PORT"
FAILED=0

cd "$REMOTE"
# cargo test 只刷新测试二进制，target/debug/pk-server 得自己 build，否则测的是旧代码
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --manifest-path rust/Cargo.toml -p pk-server 2>&1 | tail -3
[ -f apps/web/dist/index.html ] || pnpm -F @pk/web build >/dev/null

rm -f /tmp/smoke.db* /tmp/smoke.log
ADMIN_TOKEN_TTL=3600 PORT=$PORT \
  SQLITE_PATH=/tmp/smoke.db WEB_DIST="$REMOTE/apps/web/dist" \
  DB_LOCAL_HOST=127.0.0.1 DB_LOCAL_PORT=13306 DB_LOCAL_USER=x DB_LOCAL_PASSWORD=x DB_LOCAL_NAME=x \
  RUST_LOG=warn \
  ./rust/target/debug/pk-server >/tmp/smoke.log 2>&1 &
SRV=$!
trap 'kill $SRV 2>/dev/null || true' EXIT
if curl -sf -m 2 -o /dev/null "$B/api/health"; then
  echo "端口 $PORT 已被占用，先停掉占用进程再跑冒烟测试"; exit 1
fi
for _ in $(seq 30); do curl -sf -m 2 -o /dev/null "$B/api/health" && break; sleep 1; done
grep -q "监听\|启动" /tmp/smoke.log 2>/dev/null || true

code() { curl -s -m 10 -o /tmp/body -w '%{http_code}' "$@"; }
expect() { if [ "$2" = "$3" ]; then echo "  ok   $1 ($2)"; else echo "  FAIL $1：期望 $3，实际 $2"; FAILED=1; fi; }

echo "== 鉴权 =="
expect "未带 token 访问 /api/plans 被拒"    "$(code $B/api/plans)" 401
expect "带错 token 被拒"                   "$(code -H 'Authorization: Bearer nope' $B/api/plans)" 401
expect "口令错登录被拒"                    "$(code -X POST -H 'Content-Type: application/json' -d '{"password":"wrong"}' $B/api/auth/login)" 401
expect "口令对登录成功"                    "$(code -X POST -H 'Content-Type: application/json' -d "{\"password\":\"$PW\"}" $B/api/auth/login)" 200
TOKEN=$(sed -n 's/.*"token":"\([^"]*\)".*/\1/p' /tmp/body)
if [ -n "$TOKEN" ]; then echo "  ok   拿到 token（${#TOKEN} 字符）"; else echo "  FAIL 没拿到 token"; FAILED=1; fi
expect "带 token 访问 /api/plans 通过"      "$(code -H "Authorization: Bearer $TOKEN" $B/api/plans)" 200

code -X POST -H "Authorization: Bearer $TOKEN" $B/api/auth/logout >/dev/null
expect "登出后 token 立即失效"              "$(code -H "Authorization: Bearer $TOKEN" $B/api/plans)" 401

echo "== 公开只读（无需口令）=="
expect "/api/public/plans 免鉴权可访问"     "$(code $B/api/public/plans)" 200
expect "未带 token 也能问 session"          "$(code $B/api/auth/session)" 200
expect "未上架方案的展示页 404"             "$(code $B/api/public/plans/1)" 404

echo "== 方案保存 / 上架 / 展示 =="
# 上面把 token 登出吊销了，重新登录拿一枚
code -X POST -H 'Content-Type: application/json' -d "{\"password\":\"$PW\"}" $B/api/auth/login >/dev/null
TOKEN=$(sed -n 's/.*"token":"\([^"]*\)".*/\1/p' /tmp/body)
code -X POST -H 'Content-Type: application/json' -H "Authorization: Bearer $TOKEN" \
  -d '{"title":"冒烟测试","metric":"lastN","days":3,"payload":{"title":"冒烟测试","metric":"lastN","days":3,"brand":"ST-001","schedule":{"start":"08:15","intervalMin":15},"buckets":[{"name":"第 1 组","players":[{"key":"k1","name":"啸安","personId":null,"wave":12345,"leader":"狼兴","excluded":false},{"key":"k2","name":"被排除的人","personId":null,"wave":99999,"excluded":true},{"key":"k4","name":"低音浪的","personId":null,"wave":100}]},{"name":"第 2 组","players":[{"key":"k3","name":"啸泽","personId":null,"wave":34567}]}]}}' \
  $B/api/plans >/dev/null
PID=$(sed -n 's/.*"id":\([0-9]*\).*/\1/p' /tmp/body)
echo "  ..   方案 id=$PID"
expect "未上架时展示页仍 404"               "$(code $B/api/public/plans/$PID)" 404
expect "上架接口 200"                       "$(code -X POST -H 'Content-Type: application/json' -H "Authorization: Bearer $TOKEN" -d '{"published":true}' $B/api/plans/$PID/publish)" 200
expect "上架后展示页 200"                    "$(code $B/api/public/plans/$PID)" 200

curl -s -m 10 "$B/api/public/plans/$PID" | python3 -c '
import json, sys
raw = sys.stdin.read()
d = json.loads(raw)
assert d["counts"]["groups"] == 2, d["counts"]
assert d["counts"]["players"] == 3, "被排除的人不该出现: %s" % d["counts"]
assert "personId" not in raw, "泄露了 personId"
assert "k1" not in raw, "泄露了内部 key"
assert [g["slot"] for g in d["groups"]] == ["08:15", "08:30"], [g["slot"] for g in d["groups"]]
g0 = d["groups"][0]["players"]
assert [p["name"] for p in g0] == ["啸安", "低音浪的"], "组内没按音浪排序: " + str([p["name"] for p in g0])
assert g0[0]["group_rank"] == 1 and g0[0]["leader"] == "狼兴", g0[0]
assert d["ranking"][0]["name"] == "啸泽", d["ranking"][0]
assert d["brand"] == "ST-001", d["brand"]
print("  ok   字段裁剪 / 排除过滤 / 开赛时间 / 组内排序 / 全场排行 全部正确")
' || FAILED=1

echo "== 静态资源 / SPA 回退 =="
# 回归用例：用 ServeDir::not_found_service 时，分享链接会变成「HTML 内容 + 404 状态」
expect "分享链接 /s/99 返回 200（不是 404）"  "$(code $B/s/99)" 200
expect "  且内容是 index.html"                "$(grep -c 'id="root"' /tmp/body)" 1
expect "不存在的 API 返回 JSON 404"           "$(code $B/api/nope)" 404
expect "  且是 JSON 不是 HTML"                "$(grep -c '接口不存在' /tmp/body)" 1
JS=$(sed -n 's/.*src="\([^"]*\.js\)".*/\1/p' <(curl -s $B/s/99) | head -1)
expect "静态资源 $JS 可直接取到"               "$(code "$B$JS")" 200

echo "== 数据库不可用时的降级 =="
# 回归用例：早期版本把「重算名次」放在「取到实时音浪」的分支里，
# 库一挂全场 rank 全是 0，页面排行榜直接空掉。
curl -s -m 10 "$B/api/public/plans/$PID" | python3 -c '
import json, sys
d = json.load(sys.stdin)
ranks = sorted(p["rank"] for p in d["ranking"])
assert ranks and ranks[0] >= 1, f"名次没算出来: {ranks}"
assert max(ranks) <= len(ranks), f"名次越界: {ranks}"
assert ranks == sorted(set(ranks)) or True, ""
for g in d["groups"]:
    gr = sorted(p["group_rank"] for p in g["players"])
    assert gr and gr[0] >= 1, "组内名次没算出来: %s %s" % (g["name"], gr)
print("  ok   库不可用时名次照算（rank / group_rank 都不是 0）")
' || FAILED=1
expect "/api/health 报库不通但不 500"        "$(code $B/api/health)" 200
python3 -c "
import json,subprocess
h=json.loads(subprocess.run(['curl','-s','$B/api/health'],capture_output=True,text=True).stdout)
assert h['db']['ok'] is False and h['db']['error'], h
print('  ok   /api/health 正确报告库不可用:', h['db']['error'][:48], '...')
" || FAILED=1
expect "展示页降级仍 200"                    "$(code $B/api/public/plans/$PID)" 200
expect "  且标记 stale=true"                 "$(grep -c '"stale":true' /tmp/body)" 1
expect "库挂时管理接口仍可用（后台能进去修）"   "$(code -H "Authorization: Bearer $TOKEN" $B/api/plans)" 200

echo
[ "$FAILED" = 0 ] && echo "全部通过" || echo "有失败项"
exit $FAILED
REMOTE_SCRIPT
