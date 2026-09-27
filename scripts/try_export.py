"""验收：真实浏览器里跑一遍导出，把 PNG 落盘并检查尺寸。"""
import asyncio
import os
from playwright.async_api import async_playwright


async def main():
    async with async_playwright() as p:
        browser = await p.chromium.launch(channel="chrome", headless=True)
        page = await browser.new_page(viewport={"width": 1440, "height": 1000})
        logs = []
        page.on("console", lambda m: logs.append(f"[{m.type}] {m.text}"))
        page.on("pageerror", lambda e: logs.append(f"[pageerror] {e.message}"))

        await page.goto("http://127.0.0.1:5173/", wait_until="networkidle")
        await page.get_by_role("button", name="解析并分组").click()
        await page.wait_for_timeout(8000)

        try:
            async with page.expect_download(timeout=25000) as dl:
                await page.get_by_role("button", name="导出图片").click()
            d = await dl.value
            path = "/tmp/export-verify.png"
            await d.save_as(path)
            size = os.path.getsize(path)
            print(f"✅ 导出成功：{d.suggested_filename}  {size/1024:.0f} KB -> {path}")
        except Exception as e:
            print("❌ 导出失败：", type(e).__name__, str(e)[:160])

        await page.wait_for_timeout(1500)
        toasts = await page.locator("div.fixed.bottom-6").all_inner_texts()
        print("toast：", toasts)

        errs = [l for l in logs if l.startswith("[error]") and "404" not in l]
        print("控制台异常：", errs or "(无)")
        await browser.close()


asyncio.run(main())
