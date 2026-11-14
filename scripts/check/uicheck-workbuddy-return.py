#!/usr/bin/env python3
"""WorkBuddy 授权返回快速提示 + QQ 绑定号码隐藏回归（无后端、本机 Chrome）。"""
from pathlib import Path
import os

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2]
CHROME = os.environ.get("CHROME_PATH", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
HTML = (ROOT / "rust/crates/pk-server/src/public_teams/st_page.html").read_text()
HTML = HTML.replace("{{TEAM}}", "all").replace("{{WEB_APP_VERSION}}", "test")
PENDING = """JSON.stringify({
  state:'return-state', realm:'cn', authUrl:'https://copilot.tencent.com/login?state=return-state',
  startedAt:Date.now(), account:'测试账号'
})"""


def new_page(browser):
    page = browser.new_page()
    errors = []
    page.on("pageerror", lambda error: errors.append(str(error)))

    def route(request):
        if request.request.url == "http://app.test/":
            return request.fulfill(status=200, content_type="text/html", body=HTML)
        if request.request.url.endswith("/manifest.webmanifest"):
            return request.fulfill(status=200, content_type="application/json", body="{}")
        return request.fulfill(status=200, content_type="application/json",
                               body='{"loginRequired":true,"items":[]}')

    page.route("**/*", route)
    page.goto("http://app.test/", wait_until="domcontentloaded")
    page.wait_for_function("typeof maybePromptTencentBind === 'function'")
    return page, errors


with sync_playwright() as playwright:
    browser = playwright.chromium.launch(headless=True, executable_path=CHROME)

    page, errors = new_page(browser)
    result = page.evaluate(f"""async () => {{
      sessionStorage.setItem(wbPendingKey, {PENDING});
      window.promptCount = 0;
      maybePromptTencentBind = () => {{ window.promptCount++; }};
      queueMyRankFocus = () => {{}};
      renderTeamSwitch = () => {{}};
      applyWatermark = () => {{}};
      const started = performance.now();
      applyLoginState({{name:'测试账号', bound:true, team:'male'}});
      await new Promise(resolve => setTimeout(resolve, 120));
      return {{promptCount:window.promptCount, elapsed:performance.now() - started}};
    }}""")
    assert not errors, errors
    assert result["promptCount"] == 1 and result["elapsed"] < 500, result
    print(f"ok 授权返回调度: {result['elapsed']:.1f}ms（原固定等待 900ms）")
    page.close()

    page, errors = new_page(browser)
    result = page.evaluate(f"""async () => {{
      sessionStorage.setItem(wbPendingKey, {PENDING});
      site = {{settings:{{workbuddyEnabled:true}}}};
      meState = {{name:'测试账号', bound:true}};
      myToken = () => 'token';
      document.getElementById('gate').classList.add('hidden');
      document.getElementById('gate').innerHTML = '';
      window.quickValue = null;
      window.opened = null;
      fetchWbStatus = async (quick) => {{
        window.quickValue = quick;
        return {{ok:true, enabled:true, bound:true,
          account:{{uid:'u1',nickname:'测试账号'}}, accountStatus:null}};
      }};
      openWbBindModal = (opts) => {{ window.opened = opts; }};
      maybePromptTencentBind(meState);
      await new Promise(resolve => setTimeout(resolve, 30));
      return {{quick:window.quickValue, opened:window.opened}};
    }}""")
    assert not errors, errors
    assert result["quick"] is True and result["opened"] is not None, result
    print("ok 返回状态查询: quick=true，跳过面板到期检查")
    page.close()

    page, errors = new_page(browser)
    result = page.evaluate("""async () => {
      const dlg = {mask:{}};
      wbBindModal = dlg;
      wbBindState = 'return-state';
      wbBindStartedAt = Date.now();
      window.pollCount = 0;
      window.successCount = 0;
      postJson = async () => {
        window.pollCount++;
        return {status:'ok', bound:true, account:{uid:'u1',nickname:'测试账号'}};
      };
      wbRenderSuccess = () => { window.successCount++; };
      const started = performance.now();
      wbStartPolling(dlg);
      await new Promise(resolve => setTimeout(resolve, 120));
      return {pollCount:window.pollCount, successCount:window.successCount,
        elapsed:performance.now() - started};
    }""")
    assert not errors, errors
    assert result["pollCount"] == 1 and result["successCount"] == 1, result
    assert result["elapsed"] < 500, result
    print(f"ok 返回首次轮询: {result['elapsed']:.1f}ms（原固定等待 1200ms）")
    page.close()

    page, errors = new_page(browser)
    result = page.evaluate("""() => {
      meState = {emailMask:'123***@qq.com', email:'123***@qq.com'};
      openQqDlg({source:'my'});
      const mask = document.getElementById('qqMask');
      const text = mask.textContent;
      const values = Array.from(mask.querySelectorAll('input')).map(el => el.value);
      const hidden = !text.includes('123***') && !values.some(v => v.includes('123'));
      const rowSource = renderMy.toString();
      return {hidden, text, rowUsesMask:rowSource.includes('esc(meState.emailMask)')};
    }""")
    assert not errors, errors
    assert result["hidden"] and not result["rowUsesMask"], result
    print("ok QQ 绑定号码: 我的页面与改绑弹窗均不展示原号码")
    page.close()
    browser.close()
