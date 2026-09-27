"""看板预览截图：解析分组后整页截屏，检查新版彩色组卡效果。"""
import asyncio
from playwright.async_api import async_playwright


async def main():
    async with async_playwright() as p:
        browser = await p.chromium.launch(channel="chrome", headless=True)
        page = await browser.new_page(viewport={"width": 1440, "height": 1400}, device_scale_factor=1.5)
        logs = []
        page.on("console", lambda m: logs.append(f"[{m.type}] {m.text}"))
        page.on("pageerror", lambda e: logs.append(f"[pageerror] {e.message}"))

        await page.goto("http://127.0.0.1:5173/", wait_until="networkidle")
        await page.get_by_role("button", name="解析并分组").click()
        await page.wait_for_timeout(8000)

        await page.screenshot(path="/tmp/board-preview.png", full_page=False)
        print("截图: /tmp/board-preview.png")
        errs = [l for l in logs if l.startswith("[error]") and "404" not in l]
        print("控制台异常:", errs or "(无)")
        await browser.close()


asyncio.run(main())
