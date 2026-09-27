"""展示页冒烟截图：/s/:id 只读页渲染检查"""
import asyncio
from playwright.async_api import async_playwright


async def main():
    async with async_playwright() as p:
        browser = await p.chromium.launch(channel="chrome", headless=True)
        page = await browser.new_page(viewport={"width": 1280, "height": 1200}, device_scale_factor=1.5)
        logs = []
        page.on("console", lambda m: logs.append(f"[{m.type}] {m.text}"))
        page.on("pageerror", lambda e: logs.append(f"[pageerror] {e.message}"))

        await page.goto("http://127.0.0.1:8787/s/9", wait_until="networkidle")
        await page.wait_for_timeout(1500)
        await page.screenshot(path="/tmp/share-preview.png", full_page=False)

        text = await page.inner_text("body")
        print("页面包含组数:", "8 组" in text or "组" in text)
        print("包含'加载中':", "加载中" in text)
        print("包含'失效':", "失效" in text)
        errs = [l for l in logs if l.startswith(("[error]", "[pageerror]")) and "404" not in l]
        print("JS 错误:", errs if errs else "无")
        await browser.close()


asyncio.run(main())
