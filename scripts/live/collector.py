#!/usr/bin/env python3
"""抖音直播间 PK 分数采集器（2026-10-20）

为什么是独立进程而不是 Rust 里的 HTTP 客户端：
`/webcast/linkmic/list/` 依赖浏览器会话里的 Cookie、`msToken` 与 `a_bogus` 签名，
在后端重新实现这套签名既不稳定也不划算。这里用 Playwright 保持一个真实浏览器页面，
在**页面上下文里**发 fetch（自带 Cookie 与签名参数），拿到结构化数据后上报后端。

使用：
    export LIVE_COLLECTOR_TOKEN=xxx
    python3 scripts/live/collector.py --event 1 --room 505899768449

纪律：
- battle_id / room_id / UID 一律当**字符串**（19 位超 JS 安全整数，转数字会丢精度）
- 存在 battle_stats ≠ 正在 PK（页面显示「PK结束」后接口还会残留约一分钟），
  所以把 start_time_ms / duration / battle_status / now 一并上报，由后端判定
- 采集失败只影响健康度，不影响已落库的对局
"""

import argparse
import json
import os
import sys
import time
import urllib.request
from typing import Any

DEFAULT_API = "http://127.0.0.1:8787"
DEFAULT_INTERVAL = 4.0


class Api:
    def __init__(self, base: str, token: str):
        self.base = base.rstrip("/")
        self.token = token

    def _post(self, path: str, payload: dict) -> dict | None:
        req = urllib.request.Request(
            self.base + path,
            data=json.dumps(payload).encode("utf-8"),
            headers={
                "Content-Type": "application/json",
                "X-Live-Token": self.token,
            },
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=10) as resp:
                return json.loads(resp.read().decode("utf-8"))
        except Exception as e:  # 网络问题不该让采集器退出
            print(f"[warn] 上报失败 {path}: {e}", file=sys.stderr)
            return None

    def ingest(self, event_id: int, obs: dict) -> dict | None:
        return self._post(f"/api/internal/live/events/{event_id}/observations", obs)

    def heartbeat(self, worker_id: str, rooms: int, event_id: int) -> None:
        self._post(
            "/api/internal/live/worker/heartbeat",
            {
                "workerId": worker_id,
                "eventId": event_id,
                "version": "1.0.0",
                "rooms": rooms,
            },
        )


"""采集策略（2026-10-20 实测决定）：

**不自己构造 `linkmic/list` 的 URL**。实测那套请求要带 `aid` / `a_bogus` /
`msToken` / `anchor_id` 等一大串签名参数，手工拼出来的请求一律返回
`{"status_code":10011,"data":{"message":"Request params error"}}`。
页面自己每隔几秒就会发一次这个请求（带完整签名），所以做法是：
监听页面的 response，把最近一次 `linkmic/list` 的 JSON 留下来，
采集循环直接读这份快照。代价是节拍由页面决定（约几秒一次），
好处是完全不用碰签名，也不会因为签名过期突然失效。

只有在页面迟迟不发这个请求时（比如刚开播还没连麦）才回落到主动 fetch，
主动失败也不算错误——下一拍页面自己发了就有了。

⚠️ 主循环的等待必须用 `page.wait_for_timeout`，**不能**用 `time.sleep`：
Playwright 同步 API 的 response 回调只在把控制权交回浏览器的调用期间派发，
time.sleep 期间一个事件都收不到（实测 60 秒里 linkmic 计数恒为 0）。
"""

def parse_linkmic(j: dict) -> dict:
    """把 linkmic/list 的原始 JSON 转成中间结构（字段与后端 Observation 对齐前的一步）"""
    if isinstance(j, str):
        try:
            j = json.loads(j)
        except Exception:
            return {}
    if not isinstance(j, dict):
        return {}
    data = j.get("data") or {}
    bs = data.get("battle_stats")
    users = data.get("user") or []
    out: dict[str, Any] = {
        "roomId": str(data.get("room_id") or ""),
        "online": bool(data.get("has_room", True)),
        "nowMs": int((j.get("extra") or {}).get("now") or 0),
        "userCount": len(users),
        "battle": None,
    }
    if not bs:
        return out
    settings = bs.get("battle_settings") or {}
    infos = bs.get("user_infos") or {}
    bonus_now_ms = int(out.get("nowMs") or 0) or int(time.time() * 1000)
    bonus_map = _bonus_map(settings, bonus_now_ms)
    scores = []
    for s in bs.get("battle_scores") or []:
        uid = str(s.get("user_id_str") or s.get("user_id") or "")
        # linkmic/list 的 battle_scores 里没有昵称，从 user_infos / user[] 兜
        nickname = ""
        info = infos.get(uid) if isinstance(infos, dict) else None
        # 实测结构：user_infos[uid].user.nick_name
        if isinstance(info, dict):
            u = info.get("user")
            if isinstance(u, dict):
                nickname = str(u.get("nick_name") or u.get("nickname") or "")
            if not nickname:
                nickname = str(info.get("nickname") or "")
        if not nickname:
            for u in users:
                uu = u.get("user") or {}
                if str(uu.get("id_str") or uu.get("id") or "") == uid:
                    nickname = str(uu.get("nick_name") or uu.get("nickname") or "")
                    break
        scores.append(
            {
                "userId": uid,
                "nickname": nickname,
                "score": int(s.get("score") or 0),
                "rank": int(s.get("battle_rank") or 0),
                "scoreVersion": int(s.get("score_version") or 0),
                "teamId": str(s.get("multi_pk_team_id") or ""),
                **bonus_map.get(uid, _empty_bonus()),
                "currentBonus": int(s.get("current_bonus") or 0),
            }
        )
    # multi_pk_team 实测是**字符串数组**（如 ["1","2"]），不是对象数组；
    # 队伍分在 battle_scores 里按 multi_pk_team_* 给出，这里按 team_id 聚合。
    teams: dict[str, dict] = {}
    for s in bs.get("battle_scores") or []:
        tid = str(s.get("multi_pk_team_id") or "")
        if not tid:
            continue
        if tid not in teams:
            teams[tid] = {
                "teamId": tid,
                "score": int(s.get("multi_pk_team_score") or 0),
                "rank": int(s.get("multi_pk_team_rank") or 0),
            }
    team_list = [teams[k] for k in sorted(teams)]
    out["battle"] = {
        "battleId": str(bs.get("battle_id_str") or bs.get("battle_id") or ""),
        "channelId": str(bs.get("channel_id_str") or bs.get("channel_id") or ""),
        "startTimeMs": int(settings.get("start_time_ms") or 0),
        "durationSecs": int(settings.get("duration") or 0),
        "battleStatus": int(settings.get("battle_status") or 0),
        "scores": scores,
        "teamScores": team_list,
    }
    return out


def _empty_bonus() -> dict[str, Any]:
    """没有活动加成时的稳定默认值，避免前后端字段缺一块。"""
    return {
        "bonusMultiple": 100,
        "bonusScore": 0,
        "bonusActive": False,
        "bonusEndMs": 0,
    }


def _bonus_map(settings: dict, now_ms: int) -> dict[str, dict[str, Any]]:
    """从 battle_settings.lynx_data 提取抖音 PK 彩蛋/加成任务。

    实测字段（2026-11-08）：
      battle_easter_egg_info.easter_egg_info_map[uid].config.addition_multiple = "150"
      update_info.reward_info.addition_score_map[uid] = "298"
      config.period_list[[stage=4].start_time_sec/duration_sec → 生效区间

    `addition_multiple=150` 表示 1.5 倍；`addition_score_map` 是已经计入
    显示分的累计增量，所以去加成原分 = 显示分 - bonusScore。
    """
    raw = settings.get("lynx_data")
    if isinstance(raw, str):
        try:
            raw = json.loads(raw)
        except Exception:
            return {}
    if not isinstance(raw, dict):
        return {}
    egg = raw.get("battle_easter_egg_info") or {}
    infos = egg.get("easter_egg_info_map") if isinstance(egg, dict) else None
    if not isinstance(infos, dict):
        return {}

    now_ms = int(now_ms or 0)
    out: dict[str, dict[str, Any]] = {}
    for uid, info in infos.items():
        if not isinstance(info, dict):
            continue
        cfg = info.get("config") or {}
        update = info.get("update_info") or {}
        reward = update.get("reward_info") or {}
        score_map = reward.get("addition_score_map") or {}
        bonus_score = 0
        if isinstance(score_map, dict):
            try:
                bonus_score = max(0, int(score_map.get(str(uid)) or 0))
            except (TypeError, ValueError):
                bonus_score = 0
        try:
            multiple = int(cfg.get("addition_multiple") or 100)
        except (TypeError, ValueError):
            multiple = 100
        if multiple <= 0:
            multiple = 100

        active = False
        end_ms = 0
        periods = cfg.get("period_list") or []
        if isinstance(periods, list):
            for period in periods:
                if not isinstance(period, dict):
                    continue
                try:
                    stage = int(period.get("stage") or 0)
                    start_sec = int(period.get("start_time_sec") or 0)
                    duration_sec = int(period.get("duration_sec") or 0)
                except (TypeError, ValueError):
                    continue
                # stage=4 是实际加成窗口；stage=1~3 是预告/动画。
                if stage != 4 or start_sec <= 0 or duration_sec <= 0:
                    continue
                start_ms = start_sec * 1000
                candidate_end_ms = (start_sec + duration_sec) * 1000
                end_ms = max(end_ms, candidate_end_ms)
                if now_ms > 0 and start_ms <= now_ms < candidate_end_ms:
                    active = True

        out[str(uid)] = {
            "bonusMultiple": multiple,
            "bonusScore": bonus_score,
            "bonusActive": active,
            "bonusEndMs": end_ms,
        }
    return out


def to_observation(web_rid: str, raw: dict, now_ms: int) -> dict:
    """把页面返回的原始结构转成后端 Observation（字段名与 Rust 侧一致）"""
    raw_now_ms = int(raw.get("nowMs") or 0)
    obs: dict[str, Any] = {
        "webRid": web_rid,
        "roomId": str(raw.get("roomId", "")),
        "roomOnline": bool(raw.get("online", True)),
        # 优先使用抖音接口的 extra.now；它和 battle 时间在同一条时间轴。
        # 只有接口没给时才回落到本机时钟。
        "nowMs": raw_now_ms if raw_now_ms > 0 else now_ms,
        "health": "ok",
        "error": "",
    }
    b = raw.get("battle")
    if isinstance(b, dict) and b.get("battleId"):
        # 后端 Observation 用 camelCase（与前端其它接口一致），这里直接拼好
        obs["battle"] = {
            "battleId": str(b["battleId"]),
            "channelId": str(b.get("channelId", "")),
            "startTimeMs": int(b.get("startTimeMs", 0)),
            "durationSecs": int(b.get("durationSecs", 0)),
            "battleStatus": int(b.get("battleStatus", 0)),
            "scores": [
                {
                    "userId": str(s.get("userId", "")),
                    "nickname": str(s.get("nickname", "")),
                    "score": int(s.get("score", 0)),
                    "rank": int(s.get("rank", 0)),
                    "scoreVersion": int(s.get("scoreVersion", 0)),
                    "teamId": str(s.get("teamId", "")),
                    "bonusMultiple": int(s.get("bonusMultiple", 100)),
                    "bonusScore": int(s.get("bonusScore", 0)),
                    "bonusActive": bool(s.get("bonusActive", False)),
                    "bonusEndMs": int(s.get("bonusEndMs", 0)),
                    "currentBonus": int(s.get("currentBonus", 0)),
                }
                for s in b.get("scores", [])
            ],
            "teamScores": [
                {
                    "teamId": str(t.get("teamId", "")),
                    "score": int(t.get("score", 0)),
                    "rank": int(t.get("rank", 0)),
                }
                for t in b.get("teamScores", [])
            ],
        }
    return obs


def build_ingest_payload(
    web_rid: str,
    raw: dict,
    now_ms: int,
    last_response_at: float,
    wall_now: float,
    interval: float,
) -> tuple[dict, bool]:
    """根据快照新鲜度生成上报内容；过期快照绝不能再更新实时分。"""
    stale = wall_now - last_response_at > max(30.0, interval * 6)
    if stale or not raw:
        return (
            {
                "webRid": web_rid,
                "roomOnline": True,
                "nowMs": now_ms,
                "health": "error" if stale else "ok",
                "error": "页面长时间没发 linkmic 请求" if stale else "",
            },
            False,
        )
    obs = to_observation(web_rid, raw, now_ms)
    if obs.get("nowMs", 0) <= 0:
        obs["nowMs"] = now_ms
    return obs, True


def main() -> int:
    ap = argparse.ArgumentParser(description="抖音直播间 PK 分数采集器")
    ap.add_argument("--event", type=int, required=True, help="后端赛事 id")
    ap.add_argument("--room", action="append", default=[], help="直播间 web_rid，可重复")
    ap.add_argument("--api", default=os.environ.get("LIVE_API", DEFAULT_API))
    ap.add_argument("--token", default=os.environ.get("LIVE_COLLECTOR_TOKEN", ""))
    ap.add_argument("--interval", type=float, default=DEFAULT_INTERVAL)
    ap.add_argument("--once", action="store_true", help="只采一拍就退出（调试用）")
    ap.add_argument(
        "--wait",
        type=float,
        default=60.0,
        help="--once 时最多等多少秒拿到页面数据（默认 60）",
    )
    ap.add_argument("--headed", action="store_true", help="显示浏览器窗口（调试用）")
    ap.add_argument(
        "--channel",
        default=os.environ.get("LIVE_BROWSER_CHANNEL", "chrome"),
        help="Playwright 浏览器通道（默认 chrome = 系统 Chrome；空串 = 自带 chromium）",
    )
    ap.add_argument(
        "--executable",
        default=os.environ.get("LIVE_BROWSER_EXECUTABLE", ""),
        help="Chrome/Chromium 可执行文件路径（部署机未安装系统 Chrome 时使用）",
    )
    ap.add_argument(
        "--no-sandbox",
        action="store_true",
        default=os.environ.get("LIVE_BROWSER_NO_SANDBOX") == "1",
        help="VPS / 容器环境禁用 Chromium sandbox（等价 --no-sandbox）",
    )
    args = ap.parse_args()

    if not args.token:
        print("缺少 LIVE_COLLECTOR_TOKEN", file=sys.stderr)
        return 2
    if not args.room:
        print("至少给一个 --room", file=sys.stderr)
        return 2

    api = Api(args.api, args.token)
    worker_id = f"local-{os.getpid()}"

    from playwright.sync_api import sync_playwright  # 延迟导入：--help 不需要浏览器

    launch = {"headless": not args.headed}
    # 本地默认用系统 Chrome；部署机可直接指定 Playwright 下载的 Chrome 路径。
    if args.executable.strip():
        launch["executable_path"] = args.executable.strip()
    elif args.channel and args.channel.strip():
        launch["channel"] = args.channel.strip()
    if args.no_sandbox:
        launch["chromium_sandbox"] = False
    with sync_playwright() as p:
        browser = p.chromium.launch(**launch)
        ctx = browser.new_context()
        pages = {}

        # 每个房间留一份「最近一次 linkmic/list 的响应」，由页面自己的请求刷新。
        # 监听必须在 goto 之前挂上：首次加载页面时就会发 linkmic/list，
        # 先 goto 再 on(response) 会永远错过第一拍。
        latest: dict[str, dict] = {}
        last_at: dict[str, float] = {}
        last_reload: dict[str, float] = {}

        def make_handler(rid: str):
            def on_response(resp):
                url = resp.url or ""
                if "/webcast/linkmic/list/" not in url:
                    return
                try:
                    j = resp.json()
                except Exception:
                    return
                latest[rid] = parse_linkmic(j)
                last_at[rid] = time.time()

            return on_response

        for rid in args.room:
            page = ctx.new_page()
            latest[rid] = {}
            # 从页面打开时开始计时。若这里留 0，首拍会被误判成“已过期”。
            last_at[rid] = time.time()
            last_reload[rid] = 0.0
            page.on("response", make_handler(rid))
            page.goto(
                f"https://live.douyin.com/{rid}",
                wait_until="domcontentloaded",
                timeout=60000,
            )
            pages[rid] = page
            print(f"[info] 已打开直播间 {rid}")

        first_page = next(iter(pages.values()))
        started = time.time()
        try:
            while True:
                now_ms = int(time.time() * 1000)
                for rid, page in pages.items():
                    wall_now = time.time()
                    stale_after = max(30.0, args.interval * 6)
                    # 页面在前台一段时间后会停止轮询 linkmic/list；长时间没新响应时
                    # 重新加载一次直播间，重新触发页面自己的签名请求（不能手拼 URL）。
                    if (
                        wall_now - last_at.get(rid, 0.0) > stale_after
                        and wall_now - last_reload.get(rid, 0.0) > 15.0
                    ):
                        last_reload[rid] = wall_now
                        latest[rid] = {}
                        try:
                            page.reload(wait_until="domcontentloaded", timeout=60000)
                            last_at[rid] = time.time()
                            print(f"[info] {rid} linkmic 超时，已重新加载直播间")
                        except Exception as e:
                            print(f"[warn] {rid} 重新加载直播间失败: {e}", file=sys.stderr)
                    raw = latest.get(rid) or {}
                    obs, has_fresh_snapshot = build_ingest_payload(
                        rid,
                        raw,
                        now_ms,
                        last_at.get(rid, 0.0),
                        wall_now,
                        args.interval,
                    )
                    if not has_fresh_snapshot:
                        api.ingest(args.event, obs)
                        print(f"[poll] {rid} 等待页面数据…")
                        continue
                    res = api.ingest(args.event, obs)
                    bid = (obs.get("battle") or {}).get("battleId", "")
                    print(
                        f"[poll] {rid} battle={bid or '-'} "
                        f"phase={(res or {}).get('phase', '?')} "
                        f"users={raw.get('userCount', 0)}"
                    )
                api.heartbeat(worker_id, len(pages), args.event)
                # --once 要等到真的拿到过数据才退出，否则「刚开播还没连麦」时
                # 一拍就返回，调试时永远看不到页面数据。
                if args.once and (
                    any(latest.get(rid) for rid in pages)
                    or (time.time() - started) > args.wait
                ):
                    break
                # 关键：必须走 **页面** 的等待，不能用 time.sleep。
                # Playwright 的同步 API 里，response 回调只在 wait_for_timeout
                # 这类「把控制权交回浏览器」的调用期间派发；用 time.sleep 会
                # 让整轮循环一个事件都收不到（实测 tick 12 次 linkmic 仍为 0）。
                # 用列表里第一个页面驱动等待即可：同一 context 共享一条
                # Playwright 连接，回调对全部页面生效。
                first_page.wait_for_timeout(int(args.interval * 1000))
        except KeyboardInterrupt:
            print("[info] 已停止")
        finally:
            browser.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
