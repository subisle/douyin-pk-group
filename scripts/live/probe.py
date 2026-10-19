#!/usr/bin/env python3
"""一次性探测：打开直播间，打印 linkmic/list 的原始结构与分数（调试用）"""

import argparse
import json
import sys
import time

sys.path.insert(0, __file__.rsplit("/", 1)[0])
from collector import parse_linkmic  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--room", default="505899768449")
    ap.add_argument("--headed", action="store_true")
    ap.add_argument("--channel", default="chrome")
    args = ap.parse_args()

    from playwright.sync_api import sync_playwright

    launch = {"headless": not args.headed}
    if args.channel.strip():
        launch["channel"] = args.channel.strip()
    with sync_playwright() as p:
        browser = p.chromium.launch(**launch)
        ctx = browser.new_context()
        page = ctx.new_page()
        latest = {}

        def on_response(resp):
            if "/webcast/linkmic/list/" not in (resp.url or ""):
                return
            try:
                latest["data"] = parse_linkmic(resp.json())
                latest["at"] = time.time()
            except Exception as e:
                latest["error"] = str(e)

        page.on("response", on_response)
        page.goto(
            f"https://live.douyin.com/{args.room}",
            wait_until="domcontentloaded",
            timeout=60000,
        )
        page.wait_for_timeout(12000)
        print(json.dumps(latest, ensure_ascii=False, indent=2)[:6000])
        browser.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
