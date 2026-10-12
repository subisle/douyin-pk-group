/**
 * 展示端登录门自检：登录成功后**必须停在站内**，不能又回到登录卡。
 *
 * 背景（2026-10-13 修复）：后端展示端登录态只认 `X-My-Token` 请求头
 * （public_teams/admin.rs::header_my_token），而 st_page.html 曾经把 token 拼在 URL
 * （?token= / ?access_token=）→ 登录成功后第一个数据请求 401 → sessionExpired 清 token
 * → 又弹登录门。用户看到的就是「提示登录成功，然后回到 https://pk.subisle.com/ 登录页」。
 *
 * 这个脚本用真浏览器把这条链路走一遍，并断言：
 *   1. 首页弹登录门（loginRequired=true 时）；
 *   2. 用管理员口令（账号=密码）登录成功，toast 出现「登录成功」；
 *   3. 登录门关掉后 **8 秒内不再弹回来**（旧代码在这里必挂：1~2 秒内就回门）；
 *   4. 登录态没被清（sessionStorage token 还在）+ 页面进入正常态（不是「连不上服务」）；
 *   5. HTTP 层口径：带 X-My-Token 头能过 gate，`?access_token=` / `?token=` 一律 401
 *      （防止以后有人再把 token 写回 URL）。
 *
 * 用法：
 *   node scripts/check/uicheck-login.mjs [BASE] [管理员口令]
 * 默认 BASE=http://127.0.0.1:8787。**登录会写一行 access_logs（账号记 admin-gate 占位符）**，
 * 生产环境跑只读无副作用；不含任何写库动作。
 *
 * 只用 Node 内置能力（Node 22+ 自带 WebSocket），不需要 puppeteer/playwright。
 */
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { setTimeout as sleep } from 'node:timers/promises';

const BASE = (process.argv[2] ?? 'http://127.0.0.1:8787').replace(/\/$/, '');
const ADMIN_PASSWORD = process.argv[3] ?? '';
/* 截图落在 .uicheck/login/（.uicheck/ 已在 .gitignore 里，自检产物不入库） */
const OUT = fileURLToPath(new URL('../.uicheck/login/', import.meta.url));
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const PORT = 9335;
const PROFILE = '/tmp/pk-uicheck-login-profile';

if (!ADMIN_PASSWORD) {
  console.error('用法：node scripts/check/uicheck-login.mjs [BASE] <管理员口令>');
  process.exit(2);
}

rmSync(OUT, { recursive: true, force: true });
mkdirSync(OUT, { recursive: true });
/* 干净 profile：否则上次的 sessionStorage 登录态会让登录门直接跳过 */
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
const expect = (m, got, want) =>
  (String(got) === String(want) ? ok(`${m} (${got})`) : bad(`${m}：期望 ${want}，实际 ${got}`));

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

const gateHidden = `document.getElementById('gate').classList.contains('hidden')`;

/* ---------------------------------- 开始 ---------------------------------- */

const target = await cdpTarget();
const ws = await connect(target);

try {
  console.log(`== 登录门自检 · ${BASE} ==`);
  const errors = await session(ws, { width: 390, height: 844, mobile: true });

  /* 0. HTTP 层口径：登录拿 token，头能过、URL 查询串不能过 */
  const login = await fetch(`${BASE}/api/public/my/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ account: ADMIN_PASSWORD, pass: ADMIN_PASSWORD }),
  }).then(async (r) => ({ status: r.status, body: await r.json().catch(() => ({})) }));
  expect('HTTP 登录返回 200', login.status, 200);
  login.body.ok === true ? ok('HTTP 登录 ok=true') : bad(`HTTP 登录失败：${JSON.stringify(login.body)}`);
  const token = login.body.token || '';
  token ? ok(`拿到 token（${token.length} 字符）`) : bad('没拿到 token');

  if (token) {
    const site = await fetch(`${BASE}/api/public/teams/all`, { headers: { 'X-My-Token': token } });
    expect('X-My-Token 头可过登录门（/all）', site.status, 200);
    const who = await fetch(`${BASE}/api/public/teams/all/my/who`, { headers: { 'X-My-Token': token } });
    expect('X-My-Token 头可过 /my/who', who.status, 200);
    const byQuery = await fetch(`${BASE}/api/public/teams/all?access_token=${encodeURIComponent(token)}`);
    expect('?access_token= 查询串必须被拒（旧写法就是它导致回登录门）', byQuery.status, 401);
    const whoQuery = await fetch(`${BASE}/api/public/teams/all/my/who?token=${encodeURIComponent(token)}`);
    expect('?token= 查询串必须被拒', whoQuery.status, 401);
  }

  /* 1. 首页：登录门应当弹出来 */
  await goto(ws, `${BASE}/`);
  const gateUp = await waitFor(ws, `!${gateHidden}`, 40, 250);
  gateUp ? ok('首页弹出登录门') : bad('登录门没弹出来（页面可能不是登录门形态）');
  if (gateUp) console.log('  ->  ' + (await shot(ws, '01-gate.png')));

  /* 2. 走真实 UI 登录 */
  await evaluate(ws, `(() => {
    document.getElementById('gateAcct').value = ${JSON.stringify(ADMIN_PASSWORD)};
    document.getElementById('gatePass').value = ${JSON.stringify(ADMIN_PASSWORD)};
    document.getElementById('gateGo').click();
    return true;
  })()`);
  const toast = await waitFor(ws, `!!document.querySelector('#toast.show') &&
    /登录成功/.test(document.getElementById('toastTx').textContent || '')`, 40, 200);
  toast ? ok('出现「登录成功」提示') : bad('没看到「登录成功」toast');
  console.log('  ->  ' + (await shot(ws, '02-login-success.png')));

  /* 3. 登录门必须关掉 */
  const closed = await waitFor(ws, gateHidden, 40, 250);
  closed ? ok('登录门关闭') : bad('登录门没关（登录被拒）');

  /* 4. 关键回归：8 秒内不许再弹回来（旧代码 1~2 秒就回门） */
  await sleep(8000);
  const stillHidden = await evaluate(ws, gateHidden).catch(() => false);
  stillHidden ? ok('8 秒后登录门仍关闭（不再回登录页）')
              : bad('登录门又弹回来了 —— 登录态没站住（本脚本要抓的就是这个）');

  const tokenKept = await evaluate(ws, `(sessionStorage.getItem('myToken_any') || '').length > 0`).catch(() => false);
  tokenKept ? ok('登录态仍在（sessionStorage token 未被 sessionExpired 清掉）')
            : bad('登录态被清掉了（sessionExpired 触发）');

  const body = await evaluate(ws, `document.body.innerText`).catch(() => '');
  !/连不上服务/.test(body) ? ok('页面进入正常态（不是「连不上服务」）')
                           : bad('页面显示「连不上服务」');
  /数据榜|音浪|排行|我的/.test(body) ? ok('主内容已渲染')
                                     : bad('主内容没渲染出来：' + String(body).slice(0, 80));
  console.log('  ->  ' + (await shot(ws, '03-after-login.png')));

  /* 5. 刷新一次：登录态要能续上（不再弹门） */
  await goto(ws, `${BASE}/`);
  await sleep(1500);
  const afterReload = await evaluate(ws, gateHidden).catch(() => false);
  afterReload ? ok('刷新后仍是登录态（不重复弹门）')
              : bad('刷新后又弹登录门（token 没被 boot 认出来）');

  const real = errors.filter((e) => !/favicon|404 \(Not Found\)/i.test(e));
  real.length === 0 ? ok('无 JS 运行时错误') : bad('JS 报错：' + real.slice(0, 3).join(' | '));
} finally {
  ws.close();
  chrome.kill();
}

console.log(failed === 0 ? '\n登录门自检全部通过 ✓' : `\n登录门自检失败 ${failed} 项 ✗`);
process.exit(failed === 0 ? 0 : 1);
