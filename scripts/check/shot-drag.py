"""分组看板拖拽冒烟：指针事件拖拽（鼠标）落位、组内重排、Esc 取消、幽灵卡片"""
import asyncio

from playwright.async_api import async_playwright

BASE = "http://127.0.0.1:8787"
OUT = "/tmp"
PW = "dev123"


async def main():
    async with async_playwright() as p:
        browser = await p.chromium.launch(channel="chrome", headless=True)
        page = await browser.new_page(viewport={"width": 1440, "height": 1000}, device_scale_factor=1.5)
        logs = []
        page.on("console", lambda m: logs.append(f"[{m.type}] {m.text}"))
        page.on("pageerror", lambda e: logs.append(f"[pageerror] {e.message}"))

        await page.goto(BASE + "/", wait_until="networkidle")
        await page.fill('input[type="password"]', PW)
        await page.click("button.primary")
        await page.wait_for_selector(".topbar", timeout=8000)
        await page.click('.topbar nav .tab:has-text("PK 分组")')
        await page.wait_for_selector(".pk .hint", timeout=8000)
        await page.click('button:has-text("用在册主播填充")')
        await page.wait_for_timeout(1200)
        await page.click('button:has-text("解析并分组")')
        await page.wait_for_selector(".pk-board .pk-group", timeout=25000)
        await page.wait_for_timeout(1000)

        g1 = page.locator(".pk-group").nth(0)
        g2 = page.locator(".pk-group").nth(1)
        names1 = await g1.locator(".pk-name").all_inner_texts()
        names2 = await g2.locator(".pk-name").all_inner_texts()
        print("拖前 第1组:", [n.strip()[:6] for n in names1[:3]], "第2组:", [n.strip()[:6] for n in names2[:3]])

        async def drag(src, dst, to_bottom=False, cancel=False, shot=None):
            sb = await src.bounding_box()
            db = await dst.bounding_box()
            await page.mouse.move(sb["x"] + sb["width"] / 2, sb["y"] + sb["height"] / 2)
            await page.mouse.down()
            await page.mouse.move(sb["x"] + sb["width"] / 2 + 12, sb["y"] + sb["height"] / 2 + 12, steps=4)
            ty = db["y"] + (db["height"] * 0.8 if to_bottom else db["height"] * 0.2)
            await page.mouse.move(db["x"] + db["width"] / 2, ty, steps=12)
            await page.wait_for_timeout(150)
            if shot:
                await page.screenshot(path=f"{OUT}/{shot}")
                print("  拖拽中 幽灵卡片:", await page.locator(".pk-ghost").count() > 0,
                      "| 插入线:", await page.locator(".pk-card.drop-before, .pk-card.drop-after").count() > 0)
            if cancel:
                await page.keyboard.press("Escape")
                await page.mouse.up()
            else:
                await page.mouse.up()
            await page.wait_for_timeout(700)

        # 1. 跨组拖：第 1 组第 1 人 → 第 2 组第 2 人下方
        await drag(g1.locator(".pk-card").nth(0), g2.locator(".pk-card").nth(1), to_bottom=True, shot="pk-drag-1.png")
        n1 = await g1.locator(".pk-name").all_inner_texts()
        n2 = await g2.locator(".pk-name").all_inner_texts()
        moved = names1[0].strip()[:6]
        print("跨组后 第1组首行:", n1[0].strip()[:6], "| 第2组含被拖人:", moved in [x.strip()[:6] for x in n2])
        print("  第2组:", [x.strip()[:6] for x in n2])

        # 2. 组内重排：把第 1 组第 1 人拖到第 3 人下面
        before = [x.strip()[:6] for x in await g1.locator(".pk-name").all_inner_texts()]
        await drag(g1.locator(".pk-card").nth(0), g1.locator(".pk-card").nth(2), to_bottom=True)
        after = [x.strip()[:6] for x in await g1.locator(".pk-name").all_inner_texts()]
        print("组内重排:", before[:4], "→", after[:4])

        # 3. Esc 取消：位置应完全不变
        before2 = after[:]
        await drag(g1.locator(".pk-card").nth(0), g2.locator(".pk-card").nth(0), cancel=True)
        after2 = [x.strip()[:6] for x in await g1.locator(".pk-name").all_inner_texts()]
        print("Esc 取消后未变:", before2[:4] == after2[:4])

        # 4. 触屏抓手存在
        print("触屏抓手数量:", await page.locator(".pk-grip").count())
        await page.screenshot(path=f"{OUT}/pk-drag-2.png")

        errs = [l for l in logs if l.startswith(("[error]", "[pageerror]"))]
        print("JS 错误:", errs if errs else "无")
        await browser.close()


asyncio.run(main())
