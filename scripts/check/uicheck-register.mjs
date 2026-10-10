/**
 * 注册自检：登录门「注册」→ WorkBuddy 网关授权链接 → 填姓名/手机号/验证码 → 注册即绑定。
 *
 * 覆盖 2026-10-11 用户口径：手机号 = 账号、短信验证码数字 = 密码；注册时必须填主播姓名，
 * 且姓名必须能在主播台账里查到（姓名框失焦实时校验）；注册即绑定（bound=1，不用再绑一次），
 * 注册同时调 WorkBuddy 用量看板（wb-proxy）的「添加账号 (OAuth)」；
 * 绑定后用户自己不能解绑（本脚本顺带验证「二次绑定被拒」）。
 *
 * 用法：
 *   node scripts/check/uicheck-register.mjs [BASE] [管理员口令]
 * 默认 BASE=http://127.0.0.1:8787。**只对测试/本机环境跑**：会真写一行
 * my_accounts（随机手机号），跑完用管理员接口删掉；没给口令就留一行并提示。
 *
 * 只用 Node 内置能力（Node 22+ 自带 WebSocket），不需要 puppeteer/playwright。
 */
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { setTimeout as sleep } from 'node:timers/promises';

const BASE = (process.argv[2] ?? 'http://127.0.0.1:8787').replace(/\/$/, '');
const ADMIN_PASSWORD = process.argv[3] ?? '';
/* 截图落在 .uicheck/register/（.uicheck/ 已在 .gitignore 里，自检产物不入库） */
const OUT = fileURLToPath(new URL('../.uicheck/register/', import.meta.url));
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const PORT = 9334;
const PROFILE = '/tmp/pk-uicheck-register-profile';

/* 随机测试手机号：199 + 8 位（避开真实号段，也不会跟上次跑的重号） */
const PHONE = '199' + String(Math.floor(Math.random() * 1e8)).padStart(8, '0');
const CODE = String(Math.floor(100000 + Math.random() * 899999)); // 6 位验证码 = 密码
/* 候选主播名（台账里的男团主播）；真正用哪个在流程里定：
   优先挑「台账里有、但还没建过账号」的那个 —— 注册即绑定要求一人一号，
   写死一个名字会在它已被注册时误报失败。 */
const ANCHOR_CANDIDATES = ['啸帆', '狼九', '狼征', '狼明'];
let ANCHOR = ANCHOR_CANDIDATES[0];

/** 用管理员接口挑一个「还没有账号」的候选主播名；没给口令就退回第一个候选 */
async function pickFreeAnchor() {
  if (!ADMIN_PASSWORD) return ANCHOR_CANDIDATES[0];
  try {
    const login = await fetch(`${BASE}/api/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ password: ADMIN_PASSWORD }),
    }).then((r) => r.json());
    if (!login.token) return ANCHOR_CANDIDATES[0];
    const list = await fetch(`${BASE}/api/admin/my-accounts`, {
      headers: { Authorization: `Bearer ${login.token}` },
    }).then((r) => r.json());
    const taken = new Set((list.items || []).map((a) => a.name));
    for (const name of ANCHOR_CANDIDATES) {
      if (taken.has(name)) continue;
      const chk = await fetch(`${BASE}/api/public/my/register/check-name?q=${encodeURIComponent(name)}`)
        .then((r) => r.json());
      if (chk.ok) return name;
    }
    return ANCHOR_CANDIDATES[0];
  } catch {
    return ANCHOR_CANDIDATES[0];
  }
}

rmSync(OUT, { recursive: true, force: true });
mkdirSync(OUT, { recursive: true });
/* 每次都用干净的浏览器 profile：否则上次记住的「团别/登录态」会让选择卡和登录门跳过 */
rmSync(PROFILE, { recursive: true, force: true });

const chrome = spawn(CHROME, [
  '--headless=new',
  `--remote-debugging-port=${PORT}`,
  `--user-data-dir=${PROFILE}`,
  '--no-first-run',
  '--no-default-browser-check',
  '--disable-gpu',
  '--hide-scrollbars',
  'about:blank',
], { stdio: 'ignore' });

let failed = 0;
const ok = (m) => console.log(`  ok   ${m}`);
const bad = (m) => { console.log(`  FAIL ${m}`); failed++; };
const expect = (m, got, want) => (String(got) === String(want) ? ok(`${m} (${got})`) : bad(`${m}：期望 ${want}，实际 ${got}`));

/* ------------------------------ 极简 CDP 客户端 ------------------------------ */

async function cdpTarget() {
  for (let i = 0; i < 40; i++) {
    try {
      const list = await fetch(`http://127.0.0.1:${PORT}/json/list`).then((r) => r.json());
      const page = list.find((t) => t.type === 'page');
      if (page?.webSocketDebuggerUrl) return page.webSocketDebuggerUrl;
    } catch { /* Chrome 还没起来 */ }
    await sleep(250);
  }
  throw new Error('连不上 Chrome 的调试端口');
}

function connect(url) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    let id = 0;
    const pending = new Map();
    const listeners = [];
    ws.onopen = () => resolve({
      send: (method, params = {}) => new Promise((res, rej) => {
        const msgId = ++id;
        pending.set(msgId, { res, rej });
        ws.send(JSON.stringify({ id: msgId, method, params }));
      }),
      on: (fn) => listeners.push(fn),
      close: () => ws.close(),
    });
    ws.onerror = reject;
    ws.onmessage = (ev) => {
      const msg = JSON.parse(ev.data);
      if (msg.id && pending.has(msg.id)) {
        const { res, rej } = pending.get(msg.id);
        pending.delete(msg.id);
        msg.error ? rej(new Error(msg.error.message)) : res(msg.result);
      } else {
        listeners.forEach((fn) => fn(msg));
      }
    };
  });
}

async function session(ws, { width, height, mobile }) {
  const errors = [];
  ws.on((msg) => {
    if (msg.method === 'Runtime.exceptionThrown') {
      errors.push(msg.params.exceptionDetails?.exception?.description ?? '未知异常');
    }
    if (msg.method === 'Runtime.consoleAPICalled' && msg.params.type === 'error') {
      errors.push(msg.params.args.map((a) => a.value ?? a.description ?? '').join(' '));
    }
    /* confirm()/alert()：注册跳过授权、绑定确认都用 confirm，脚本一律点「确定」 */
    if (msg.method === 'Page.javascriptDialogOpening') {
      ws.send('Page.handleJavaScriptDialog', { accept: true }).catch(() => {});
    }
  });
  await ws.send('Page.enable');
  await ws.send('Runtime.enable');
  await ws.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile });
  return errors;
}

async function evaluate(ws, expr) {
  const r = await ws.send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true });
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? '求值失败');
  return r.result.value;
}

async function goto(ws, url) {
  await ws.send('Page.navigate', { url });
  for (let i = 0; i < 60; i++) {
    await sleep(300);
    const ready = await evaluate(ws, `document.readyState === 'complete' && !!document.getElementById('gate')`).catch(() => false);
    if (ready) return;
  }
}

/** 轮询等某个表达式为真 */
async function waitFor(ws, expr, tries = 40, gap = 250) {
  for (let i = 0; i < tries; i++) {
    const hit = await evaluate(ws, expr).catch(() => false);
    if (hit) return true;
    await sleep(gap);
  }
  return false;
}

async function shot(ws, name) {
  const { data } = await ws.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: true });
  writeFileSync(OUT + name, Buffer.from(data, 'base64'));
  return OUT + name;
}

/** 管理员接口删掉测试账号（按手机号找当前 name，绑过主播 name 会变） */
async function cleanup() {
  if (!ADMIN_PASSWORD) {
    console.log(`\n注意：没给管理员口令，测试账号没删。请到管理端删掉手机号 ${PHONE} 的账号。`);
    return;
  }
  try {
    const login = await fetch(`${BASE}/api/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ password: ADMIN_PASSWORD }),
    }).then((r) => r.json());
    const token = login.token;
    if (!token) throw new Error('管理员登录失败');
    const list = await fetch(`${BASE}/api/admin/my-accounts`, {
      headers: { Authorization: `Bearer ${token}` },
    }).then((r) => r.json());
    const hit = (list.items || []).find((a) => a.phone === PHONE);
    if (!hit) { console.log('清理：没找到测试账号（可能注册就没成功）'); return; }
    await fetch(`${BASE}/api/admin/my-accounts/${encodeURIComponent(hit.name)}`, {
      method: 'DELETE',
      headers: { Authorization: `Bearer ${token}` },
    });
    console.log(`清理：已删除测试账号 ${hit.name}（手机号 ${PHONE}）`);
  } catch (e) {
    console.log(`清理失败（请手工删手机号 ${PHONE}）：${e.message}`);
  }
}

/* ---------------------------------- 开始 ---------------------------------- */

const target = await cdpTarget();
const ws = await connect(target);

try {
  console.log(`== 注册自检 · ${BASE} · 测试手机号 ${PHONE} ==`);
  const errors = await session(ws, { width: 390, height: 844, mobile: true });

  /* 1. 进站：团别选择卡（开放站点首访）或直接登录门（loginRequired=true）→ 登录门 */
  await goto(ws, `${BASE}/`);
  const picked = await waitFor(ws, `!!document.getElementById('pickM')`, 10, 250);
  if (picked) {
    await evaluate(ws, `document.getElementById('pickM').click()`);
    ok('团别选择卡出现并点了「男团」');
  } else {
    /* loginRequired=true 的站点不发选择卡，boot() 直接弹登录门 —— 不是缺陷 */
    const direct = await waitFor(ws, `!document.getElementById('gate').classList.contains('hidden')`, 10, 250);
    direct ? ok('没有团别选择卡（loginRequired=true），直接进登录门')
        : bad('既没有团别选择卡，登录门也没弹出来');
  }
  const gateUp = await waitFor(ws, `!document.getElementById('gate').classList.contains('hidden')`);
  gateUp ? ok('登录门弹出') : bad('登录门没弹出来');

  /* 用户口径（2026-10-10）「注册先不开」：默认关闭 —— 关着时断言「页签藏起来 + 接口被拒」，
     打开开关（后台「公开站点设置 → 开放手机号注册」）后再跑下面的完整流程。 */
  const cfg = await fetch(`${BASE}/api/public/my/register/config`).then((r) => r.json());
  if (!cfg.open) {
    console.log('注册开关：关闭（默认）。完整流程需要先在后台打开「开放手机号注册」。');
    const tabHidden = await evaluate(ws, `!!document.getElementById('tabReg')?.hidden`);
    tabHidden ? ok('登录门不显示「注册」页签') : bad('注册关着，但「注册」页签还露着');

    const bad1 = await fetch(`${BASE}/api/public/my/register`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ phone: PHONE, code: CODE, team: 'male' }),
    }).then(async (r) => ({ status: r.status, body: await r.json().catch(() => ({})) }));
    bad1.status === 400 && String(bad1.body.error || '').includes('暂未开放')
      ? ok('关着时注册接口被拒：' + bad1.body.error)
      : bad('关着时注册接口没被拒：' + JSON.stringify(bad1));

    const bad2 = await fetch(`${BASE}/api/public/my/register/oauth/start`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ realm: 'cn' }),
    }).then(async (r) => ({ status: r.status, body: await r.json().catch(() => ({})) }));
    bad2.status === 400
      ? ok('关着时「添加账号 OAuth」接口被拒（不会误连网关）')
      : bad('关着时授权接口没被拒：' + JSON.stringify(bad2));

    const registered = await evaluate(ws, `(localStorage.getItem('myToken_male') || '') !== ''`);
    !registered ? ok('关着时没有任何注册态写入') : bad('关着时却写入了登录态');
    console.log('  ->  ' + (await shot(ws, '01-gate-register-closed.png')));
  } else {

    ANCHOR = await pickFreeAnchor();
    console.log(`   用于注册的主播名：${ANCHOR}`);

    /* 2. 登录门有两个页签；注册页签里要出现 WorkBuddy 授权步骤 */
    expect('登录页签存在', await evaluate(ws, `!!document.getElementById('tabLogin')`), 'true');
    expect('注册页签存在', await evaluate(ws, `!!document.getElementById('tabReg')`), 'true');
    await evaluate(ws, `document.getElementById('tabReg').click()`);
    const regVisible = await waitFor(ws, `!document.getElementById('paneReg').classList.contains('hidden')`);
    regVisible ? ok('切到注册页签') : bad('注册页签没显示出来');
    const oauthBox = await waitFor(ws, `!document.getElementById('regOauth').classList.contains('hidden')`);
    oauthBox ? ok('网关授权步骤已显示（服务端配了面板密码）') : bad('网关授权步骤没显示（检查 WORKBUDDY_PANEL_PASSWORD）');
    console.log('  ->  ' + (await shot(ws, '01-register-tab.png')));

    /* 3. 点「获取授权链接」：拿到 copilot.tencent.com 授权链接（= 手机号登录页） */
    await evaluate(ws, `document.getElementById('regAuth').click()`);
    const gotLink = await waitFor(ws, `(document.querySelector('#regLinkBox a')?.href ?? '').indexOf('copilot.tencent.com') > 0`, 50, 300);
    const authUrl = await evaluate(ws, `document.querySelector('#regLinkBox a')?.href ?? ''`);
    gotLink ? ok('拿到授权链接 ' + authUrl.slice(0, 58) + '…') : bad('没拿到授权链接：' + authUrl);
    await evaluate(ws, `document.getElementById('regAuth').click()`); // 重新生成：验证可重来
    await waitFor(ws, `(document.querySelector('#regLinkBox a')?.href ?? '').indexOf('copilot.tencent.com') > 0`, 40, 300);
    const authUrl2 = await evaluate(ws, `document.querySelector('#regLinkBox a')?.href ?? ''`);
    authUrl2 && authUrl2 !== authUrl ? ok('「重新生成链接」换了一条新链接') : bad('重新生成没换链接');
    console.log('  ->  ' + (await shot(ws, '02-oauth-link.png')));

    /* 4. 跳过授权（不进账号池）+ 填姓名/手机号/验证码注册。
       2026-10-11 新口径：姓名必须在台账里（失焦实时校验），注册即绑定。 */
    await evaluate(ws, `document.getElementById('regSkip').click()`);
    const skipped = await waitFor(ws, `(document.getElementById('regState')?.textContent ?? '').includes('已跳过')`);
    skipped ? ok('跳过授权有确认提示') : bad('跳过授权没有反馈');

    await evaluate(ws, `(() => {
      const n = document.getElementById('regName'), p = document.getElementById('regPhone'), c = document.getElementById('regCode');
      n.value = ${JSON.stringify(ANCHOR)}; p.value = ${JSON.stringify(PHONE)}; c.value = ${JSON.stringify(CODE)};
      n.dispatchEvent(new Event('input', { bubbles: true }));
      p.dispatchEvent(new Event('input', { bubbles: true }));
      c.dispatchEvent(new Event('input', { bubbles: true }));
    })()`);

    /* 姓名实时校验：失焦 → check-name → 应提示「✓ 台账里有这位主播」 */
    await evaluate(ws, `document.getElementById('regName').dispatchEvent(new Event('blur'))`);
    const nameOk = await waitFor(ws, `(document.getElementById('regNameTip')?.textContent ?? '').includes('台账里有这位主播')`, 40, 300);
    nameOk ? ok('姓名实时校验通过（' + ANCHOR + '）')
        : bad('姓名实时校验没通过：' + await evaluate(ws, `document.getElementById('regNameTip')?.textContent ?? ''`));

    await evaluate(ws, `document.getElementById('regGo').click()`);
    const registered = await waitFor(ws, `document.getElementById('gate').classList.contains('hidden')`, 60, 300);
    registered ? ok('注册成功，登录门关闭') : bad('注册没成功：' + await evaluate(ws, `document.getElementById('regErr')?.textContent ?? ''`));
    expect('本地已存 token', await evaluate(ws, `(sessionStorage.getItem('myToken_male') || '').startsWith('my-')`), 'true');

    /* 5. 注册即绑定：不该出现「未绑定」提醒条，也没有绑定卡 */
    const barHidden = await evaluate(ws, `document.getElementById('bindBar').hidden`);
    barHidden ? ok('没有出现「未绑定主播」提醒条（注册即绑定）') : bad('注册后还出现未绑定提醒条');
    await evaluate(ws, `document.getElementById('navMy').click()`);
    const myNm = await waitFor(ws, `(document.querySelector('.my-head .nm')?.textContent ?? '').trim() === ${JSON.stringify(ANCHOR)}`, 60, 300);
    myNm ? ok('「我的」页直接显示主播数据（' + ANCHOR + '）') : bad('「我的」页没直接显示主播数据');
    expect('水印按主播姓名', await evaluate(ws, `localStorage.getItem('myName_male')`), ANCHOR);
    console.log('  ->  ' + (await shot(ws, '03-my-bound.png')));

    /* 6. 绑定是一次性的：已绑定账号再调 bind 必须被拒（用户口径：绑定后不支持解绑） */
    const again = await fetch(`${BASE}/api/public/my/bind`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ token: await evaluate(ws, `sessionStorage.getItem('myToken_male')`), personId: 1 }),
    }).then((r) => r.json());
    String(again.error || '').includes('已绑定')
      ? ok('二次绑定被拒：' + again.error)
      : bad('二次绑定没被拒：' + JSON.stringify(again));

    /* 8. 手机号 + 验证码回登录（注册出来的账号，验证码就是密码）。
       登录限流是每 IP 每分钟 5 次，自检本身可能已经用掉几格 → 撞上限就等一分钟重试一次。 */
    let relogin = {};
    for (let i = 0; i < 2; i++) {
      relogin = await fetch(`${BASE}/api/public/teams/male/verify`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ account: PHONE, pass: CODE }),
      }).then((r) => r.json());
      if (String(relogin.error || '').includes('太频繁') && i === 0) {
        console.log('  ..   登录限流命中（每分钟 5 次），等 62 秒后重试一次');
        await sleep(62000);
        continue;
      }
      break;
    }
    relogin.ok === true && relogin.name === ANCHOR
      ? ok('手机号 + 验证码可以登录（' + relogin.name + '）')
      : bad('手机号登录失败：' + JSON.stringify(relogin));

    /* 9. 清掉自检过程中在网关面板上开的授权状态（不留垃圾 state） */
    for (const u of [authUrl, authUrl2]) {
      const state = new URL(u).searchParams.get('state');
      if (!state) continue;
      await fetch(`${BASE}/api/public/my/register/oauth/cancel`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ state }),
      }).catch(() => {});
    }
    ok('已在面板上取消自检用的授权 state');

    errors.length ? bad('本次流程有 console 报错: ' + errors.slice(0, 3).join(' | ')) : ok('全流程无 console 报错');

  }
} catch (e) {
  bad(`脚本异常：${e.message}`);
} finally {
  ws.close();
  chrome.kill();
}

await cleanup();

console.log();
console.log(failed === 0 ? `全部通过，截图在 ${OUT}` : `${failed} 项失败，截图在 ${OUT}`);
process.exit(failed === 0 ? 0 : 1);
