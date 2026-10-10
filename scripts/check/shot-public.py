"""公开展示端（/1 男团 /0 女团）渲染冒烟：三页签 + 字段显隐 + 验证流程截图"""
import asyncio
import sys

from playwright.async_api import async_playwright

BASE = "http://127.0.0.1:8787"
OUT = "/tmp"


async def main():
    async with async_playwright() as p:
        browser = await p.chromium.launch(channel="chrome", headless=True)
        page = await browser.new_page(
            viewport={"width": 390, "height": 844}, device_scale_factor=2
        )
        logs = []
        page.on("console", lambda m: logs.append(f"[{m.type}] {m.text}"))
        page.on("pageerror", lambda e: logs.append(f"[pageerror] {e.message}"))

        await page.goto(BASE + "/1", wait_until="networkidle")
        await page.wait_for_selector("#view .row, #view .empty", timeout=8000)
        await page.wait_for_timeout(400)
        await page.screenshot(path=f"{OUT}/st-1-data.png", full_page=False)

        rows = await page.locator("#view .row").count()
        head = await page.locator(".list-head").inner_text() if await page.locator(".list-head").count() else ""
        sub = await page.locator("#mhSub").inner_text()
        print("数据页行数:", rows)
        print("表头:", head.replace("\n", " | "))
        print("masthead:", sub)

        # 字段面板
        await page.click("#btnFields")
        await page.wait_for_timeout(400)
        await page.screenshot(path=f"{OUT}/st-2-fields.png")
        labels = await page.locator("#fieldList .lb").all_inner_texts()
        print("可选字段:", " / ".join(x.replace("\n", "(") + ")" for x in labels))

        # 打开「累计音浪」「当月未播」，看行变化
        for k in ("total", "absent"):
            await page.click(f'#fieldList .sw[data-f="{k}"]')
            await page.wait_for_timeout(150)
        await page.click("#fieldDone")
        await page.wait_for_timeout(300)
        await page.screenshot(path=f"{OUT}/st-3-morefields.png")
        meta = await page.locator("#view .row").first.locator(".row-sub").inner_text()
        print("首行附加字段:", meta.replace("\n", " | "))

        # 隐藏未开播的人
        await page.click("#btnFields")
        await page.wait_for_timeout(350)
        await page.click('#fieldList .sw[data-f="nodata"]')
        await page.click("#fieldDone")
        await page.wait_for_timeout(300)
        print("隐藏未开播后行数:", await page.locator("#view .row").count())

        # 月口径
        await page.click('#seg button[data-p="monthly"]')
        await page.wait_for_timeout(900)
        await page.screenshot(path=f"{OUT}/st-4-monthly.png")
        print("月榜行数:", await page.locator("#view .row").count())
        print("周期:", await page.locator("#pNow").inner_text())
        await page.click("#pPrev")
        await page.wait_for_timeout(900)
        print("上一月:", await page.locator("#pNow").inner_text(), "行数:", await page.locator("#view .row").count())
        await page.click('#seg button[data-p="daily"]')
        await page.wait_for_timeout(800)

        # 「我的」已下线：确认页面上既没有 tab 也没有验证入口
        navs = await page.locator("[data-v]").all_inner_texts()
        print("导航项:", [n.strip() for n in navs])
        print("存在我的入口:", any("我的" in n for n in navs), "| 存在 #dy 输入框:", await page.locator("#dy").count() > 0)

        # 分组 tab
        has_group = await page.locator("#tabGroup").is_visible()
        print("分组 tab 可见:", has_group)
        if has_group:
            await page.click("#tabGroup")
            await page.wait_for_timeout(1200)
            await page.screenshot(path=f"{OUT}/st-7-group.png")

        # 女团
        await page.goto(BASE + "/0", wait_until="networkidle")
        await page.wait_for_selector("#view .row, #view .empty", timeout=8000)
        await page.wait_for_timeout(500)
        await page.screenshot(path=f"{OUT}/st-8-female.png")
        print("女团行数:", await page.locator("#view .row").count(), "标题:", await page.locator("#mhTitle").inner_text())

        errs = [l for l in logs if l.startswith(("[error]", "[pageerror]"))]
        print("JS 错误:", errs if errs else "无")
        await browser.close()


asyncio.run(main())
