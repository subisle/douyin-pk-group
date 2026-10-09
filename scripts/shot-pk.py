"""PK 分组工作台冒烟：统一后台 / → PK 分组 页，走完 填名单→分组→调整→保存→上架"""
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

        tabs = await page.locator(".topbar nav .tab").all_inner_texts()
        print("后台页签:", tabs)
        await page.click('.topbar nav .tab:has-text("PK 分组")')
        await page.wait_for_selector(".pk-board, .pk .hint", timeout=8000)
        await page.wait_for_timeout(600)
        await page.screenshot(path=f"{OUT}/pk-1-empty.png")

        # 用在册主播填充名单 → 解析并分组
        await page.click('button:has-text("用在册主播填充")')
        await page.wait_for_timeout(1500)
        roster = await page.locator(".pk-roster").input_value()
        print("名单行数:", len([l for l in roster.split("\n") if l.strip()]))
        await page.screenshot(path=f"{OUT}/pk-2-roster.png")

        await page.click('button:has-text("解析并分组")')
        await page.wait_for_selector(".pk-board .pk-group", timeout=25000)
        await page.wait_for_timeout(1200)
        groups = await page.locator(".pk-group").count()
        cards = await page.locator(".pk-card").count()
        spread = await page.locator(".pk-spread").inner_text()
        print("分组数:", groups, "| 卡片数:", cards)
        print("均衡:", spread.replace("\n", " "))
        await page.screenshot(path=f"{OUT}/pk-3-board.png")
        if groups:
            head = await page.locator(".pk-group").first.locator(".pk-group-head").inner_text()
            print("第一组:", head.replace("\n", " "))

        # 用「移到其它组」下拉做一次跨组移动（下拉比模拟 HTML5 拖拽稳）
        before = await page.locator(".pk-group").nth(0).locator(".pk-card").count()
        sel = page.locator(".pk-card").first.locator("select.pk-move")
        opts = await sel.locator("option").all()
        await sel.select_option(value=await opts[1].get_attribute("value"))
        await page.wait_for_timeout(900)
        after = await page.locator(".pk-group").nth(0).locator(".pk-card").count()
        print("跨组移动: 第 1 组", before, "→", after)

        # 锁定 + 均衡微调
        await page.locator(".pk-card").first.hover()
        await page.locator(".pk-card").first.locator("button.pk-ico").first.click()
        await page.wait_for_timeout(200)
        await page.click('button:has-text("均衡微调")')
        await page.wait_for_timeout(1500)
        print("微调后均衡:", (await page.locator(".pk-spread").inner_text()).replace("\n", " "))
        await page.screenshot(path=f"{OUT}/pk-4-polish.png")

        # 撤销 / 重做
        await page.click('button:has-text("撤销")')
        await page.wait_for_timeout(700)
        print("撤销后有违规告警:", await page.locator(".pk-alert").count() > 0)

        # 保存方案 + 上架
        await page.fill('input[placeholder="方案标题"]', "冒烟测试方案")
        await page.click('button:has-text("保存方案")')
        await page.wait_for_timeout(1500)
        rows = await page.locator("table tbody tr").count()
        print("方案行数:", rows)
        await page.locator('table tbody tr').first.locator('button:has-text("上架")').click()
        await page.wait_for_timeout(1200)
        status = await page.locator("table tbody tr").first.inner_text()
        print("方案状态:", status.replace("\n", " | "))
        await page.screenshot(path=f"{OUT}/pk-5-plans.png")

        # 导出长图（只跑逻辑，不校验像素）
        await page.click('button:has-text("导出长图")')
        await page.wait_for_timeout(2500)
        print("导出后提示:", await page.locator(".alert.good, .alert.bad").last.inner_text())

        errs = [l for l in logs if l.startswith(("[error]", "[pageerror]"))]
        print("JS 错误:", errs if errs else "无")
        await browser.close()


asyncio.run(main())
