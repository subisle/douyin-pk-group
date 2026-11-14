#!/usr/bin/env python3
"""主屏幕首次启动与软件更新的无后端浏览器回归（本机 Chrome + Playwright）。"""
from pathlib import Path
import os
import re

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2]
CHROME = os.environ.get("CHROME_PATH", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
PAGES_RS = (ROOT / "rust/crates/pk-server/src/public_teams/pages.rs").read_text()
CURRENT = re.search(r'const WEB_APP_VERSION: &str = "([^"]+)"', PAGES_RS).group(1)
NEWER = CURRENT + ".1"
HTML = (ROOT / "rust/crates/pk-server/src/public_teams/st_page.html").read_text()
HTML = HTML.replace("{{TEAM}}", "all").replace("{{WEB_APP_VERSION}}", CURRENT)


def exercise(page, latest: str):
    errors = []
    page.on("pageerror", lambda error: errors.append(str(error)))

    def route(request):
        url = request.request.url
        if url == "http://app.test/":
            return request.fulfill(status=200, content_type="text/html", body=HTML)
        if "/api/public/app-version" in url:
            body = f'{{"webVersion":"{latest}","android":{{"versionCode":4,"versionName":"1.3"}}}}'
            return request.fulfill(status=200, content_type="application/json", body=body)
        if "/api/public/announcements" in url:
            return request.fulfill(status=200, content_type="application/json",
                                   body='{"loginRequired":true,"items":[]}')
        if url.endswith("/manifest.webmanifest"):
            return request.fulfill(status=200, content_type="application/json", body="{}")
        return request.fulfill(status=200, content_type="application/json", body="{}")

    page.route("**/*", route)
    # 模拟从主屏幕独立启动，而不是普通浏览器标签页。
    page.add_init_script("Object.defineProperty(navigator, 'standalone', {get: () => true});")
    page.goto("http://app.test/", wait_until="domcontentloaded")
    page.wait_for_function("typeof checkSoftwareUpdate === 'function'")
    result = page.evaluate("""async () => {
      await checkSoftwareUpdate(true);
      const guide = document.getElementById('pwaGuide');
      const sheet = document.createElement('div');
      sheet.className = 'pwa-sheet';
      guide.appendChild(sheet);
      const style = getComputedStyle(sheet);
      const sheetMaxHeight = style.maxHeight, sheetOverflow = style.overflowY;
      guide.classList.remove('hidden');
      guide.innerHTML = '<div>stale guide</div>';
      pwaGuideOpen = false; // BFCache / 首次恢复时可能丢失这个内存标记。
      lockScroll();
      markPwaGuideInstalled();
      return {
        guideHidden: guide.classList.contains('hidden'),
        guideHtml: guide.innerHTML,
        bodyOverflow: document.body.style.overflow,
        sheetMaxHeight, sheetOverflow,
        updateVisible: !document.getElementById('appUpdate').classList.contains('hidden'),
        updateText: document.getElementById('appUpdateText').textContent,
        current: WEB_APP_VERSION,
        newer: compareAppVersions(WEB_APP_VERSION + '.1', WEB_APP_VERSION) > 0,
        same: compareAppVersions(WEB_APP_VERSION, WEB_APP_VERSION) === 0,
      };
    }""")
    assert not errors, errors
    assert result["guideHidden"] and result["guideHtml"] == "", result
    assert result["bodyOverflow"] != "hidden", result
    assert result["sheetOverflow"] == "auto" and "px" in result["sheetMaxHeight"], result
    assert result["current"] == CURRENT and result["newer"] and result["same"], result
    if latest > CURRENT:
        assert result["updateVisible"] and latest in result["updateText"], result
    else:
        assert not result["updateVisible"], result


with sync_playwright() as playwright:
    browser = playwright.chromium.launch(headless=True, executable_path=CHROME)
    for name, ua, viewport in [
        ("iOS 主屏幕", "Mozilla/5.0 (iPhone; CPU iPhone OS 19_0 like Mac OS X) AppleWebKit/605.1.15 Version/19.0 Safari/604.1", {"width": 320, "height": 480}),
        ("Android 主屏幕", "Mozilla/5.0 (Linux; Android 15) AppleWebKit/537.36 Chrome/140.0.0.0 Mobile Safari/537.36", {"width": 360, "height": 640}),
    ]:
        page = browser.new_page(user_agent=ua, viewport=viewport)
        exercise(page, NEWER)
        page.close()
        print(f"ok {name}: 首次启动清理、弹层滚动、新版本提醒")

    page = browser.new_page()
    exercise(page, CURRENT)
    page.close()
    print("ok 当前版本: 不误报更新")
    browser.close()
