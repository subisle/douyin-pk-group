/**
 * 「我的」页自检（2026-10-19 用户口径）：
 *   1. 个人页大头像用抖音真头像（<img data-av>，同源 /api/public/avatars/...）；
 *   2. 「我的抖音号」每一行也有头像（.st-my-acct .row .ico img[data-av]）；
 *   3. 头像 404 时回落首字/图标，不留破图（把 src 改坏后重挂 onerror 验证）；
 *   4. 收益口径按后台配置显示：分成 tag 是配置的百分比，卡上写着估算公式。
 *
 * 用法：node scripts/check/uicheck-my-avatar.mjs [BASE] [账号] [密码]
 * 默认 BASE=http://127.0.0.1:8787、账号=鹏先生、密码=peng2026（AGENTS.md 本地口径）。
 * 只读自检：不写库，不对外发送。
 */
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { setTimeout as sleep } from 'node:timers/promises';

const BASE = (process.argv[2] ?? 'http://127.0.0.1:8787').replace(/\/$/, '');
const ACCT = process.argv[3] ?? '鹏先生';
const PASS = process.argv[4] ?? 'peng2026';
const OUT = fileURLToPath(new URL('../.uicheck/my-avatar/', import.meta.url));
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const PORT = 9341;
const PROFILE = '/tmp/pk-uicheck-myavatar-profile';

rmSync(OUT, { recursive: true, force: true });
mkdirSync(OUT, { recursive: true });
rmSync(PROFILE, { recursive: true, force: true });

const chrome = spawn(CHROME, [
  '--headless=new', `--remote-debugging-port=${PORT}`, `--user-data-dir=${PROFILE}`,
  '--no-first-run', '--no-default-browser-check', '--disable-gpu', '--hide-scrollbars', 'about:blank',
], { stdio: 'ignore' });

let failed = 0;
const ok = (m) => console.log(`  ok   ${m}`);
const bad = (m) => { console.log(`  FAIL ${m}`); failed++; };
const expect = (m, got, want) =>
  (String(got) === String(want) ? ok(`${m} (${got})`) : bad(`${m}：期望 ${want}，实际 ${got}`));

async function cdpTarget() {
  for (let i = 0; i < 40; i++) {
    try {
      const list = await fetch(`http://127.0.0.1:${PORT}/json/list`).then((r) => r.json());
      const page = list.find((t) => t.type === 'page');
      if (page?.webSocketDebuggerUrl) return page.webSocketDebuggerUrl;
    } catch { /* 等 Chrome */ }
    await sleep(250);
  }
  throw new Error('连不上 Chrome 调试端口');
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
      } else listeners.forEach((fn) => fn(msg));
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

async function waitFor(ws, expr, tries = 60, gap = 250) {
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

const target = await cdpTarget();
const ws = await connect(target);

try {
  console.log(`== 「我的」页头像 + 收益口径自检 · ${BASE} · ${ACCT} ==`);
  const errors = await session(ws, { width: 390, height: 844, mobile: true });

  /* 登录拿 token，直接种进 sessionStorage（登录门 UI 不是本脚本的验证对象） */
  const login = await fetch(`${BASE}/api/public/my/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ account: ACCT, pass: PASS }),
  }).then((r) => r.json().catch(() => ({})));
  if (login.ok !== true) throw new Error(`登录失败：${JSON.stringify(login)}`);
  const token = login.token;
  ok(`登录成功 personId=${login.personId}`);

  /* 后台收益配置（本脚本的期望值来源） */
  const st = await fetch(`${BASE}/api/public/teams/all/my`, { headers: { 'X-My-Token': token } }).then((r) => r.json());
  const acct = await fetch(`${BASE}/api/public/teams/all/my/accounts`, { headers: { 'X-My-Token': token } }).then((r) => r.json());
  const sharePct = st?.earnings?.sharePct;
  const feePct = st?.earnings?.feePct;
  ok(`后端收益口径 sharePct=${sharePct} feePct=${feePct}`);
  acct?.earnings?.sharePct === sharePct
    ? ok('/my 与 /my/accounts 收益口径同源')
    : bad(`两个接口收益口径不一致：${JSON.stringify(st?.earnings)} vs ${JSON.stringify(acct?.earnings)}`);

  await ws.send('Page.navigate', { url: `${BASE}/` });
  await sleep(600);
  await evaluate(ws, `sessionStorage.setItem('myToken_any', ${JSON.stringify(token)}); true`);
  await ws.send('Page.navigate', { url: `${BASE}/my` });

  const ready = await waitFor(ws, `!!document.querySelector('.st-my-head')`, 80, 250);
  ready ? ok('「我的」页渲染完成') : bad('「我的」页没渲染出来');
  console.log('  ->  ' + (await shot(ws, '01-my.png')));

  /* 1. 个人页大头像 */
  const headAv = await evaluate(ws, `(() => {
    const box = document.querySelector('.st-my-av');
    if (!box) return { exists: false };
    const img = box.querySelector('img[data-av]');
    return {
      exists: true,
      hasImg: box.classList.contains('has-img'),
      src: img ? img.getAttribute('src') : null,
      sameOrigin: img ? img.getAttribute('src').startsWith('/api/public/avatars/') : false,
      natural: img ? img.naturalWidth : 0,
    };
  })()`);
  headAv.exists ? ok('个人页头像容器存在') : bad('没有 .st-my-av');
  headAv.hasImg ? ok('个人页头像走真头像分支（has-img）') : bad('个人页头像没走真头像分支（回落到首字）');
  headAv.sameOrigin ? ok(`头像走同源头像 ${headAv.src}`) : bad(`头像不是同源头像：${headAv.src}`);
  headAv.natural > 0 ? ok(`头像字节加载成功（naturalWidth=${headAv.natural}）`) : bad('头像没加载出像素（可能是 404）');

  /* 2. 抖音号列表每行头像 */
  const rows = await evaluate(ws, `(() => {
    const rs = Array.from(document.querySelectorAll('.st-my-acct .row'));
    return rs.map((r) => {
      const ico = r.querySelector('.ico');
      const img = ico ? ico.querySelector('img[data-av]') : null;
      return {
        title: (r.querySelector('.t') || {}).textContent || '',
        hasImg: !!(ico && ico.classList.contains('has-img')),
        src: img ? img.getAttribute('src') : null,
        natural: img ? img.naturalWidth : 0,
        dataN: ico ? ico.dataset.n : null,
      };
    });
  })()`);
  rows.length > 0 ? ok(`「我的抖音号」渲染了 ${rows.length} 行`) : bad('「我的抖音号」没有行（或无绑定号）');
  const allImg = rows.length > 0 && rows.every((r) => r.hasImg && r.src && r.src.startsWith('/api/public/avatars/'));
  allImg ? ok('抖音号列表每行都是同源真头像') : bad(`有行没有头像：${JSON.stringify(rows)}`);
  const allLoaded = rows.every((r) => r.natural > 0);
  allLoaded ? ok('抖音号头像全部加载出像素') : bad(`有头像没加载出来：${JSON.stringify(rows)}`);
  const allDataN = rows.every((r) => r.dataN);
  allDataN ? ok('每行都带 data-n（裂图可回落）') : bad('有行缺 data-n，裂图会留空白');

  /* 3. 收益口径展示 */
  const pctTag = await evaluate(ws, `(() => {
    const el = document.querySelector('.st-my-big .st-cmp-rmb .pct-tag');
    return el ? el.textContent.trim() : null;
  })()`);
  pctTag && pctTag.indexOf(String(sharePct) + '%分成') >= 0
    ? ok(`分成 tag 按后台配置显示：${pctTag}`)
    : bad(`分成 tag 与配置不符：期望含 ${sharePct}%分成，实际 ${pctTag}`);
  const feeShown = feePct > 0 ? (pctTag && pctTag.indexOf(String(feePct) + '%') >= 0) : (pctTag && pctTag.indexOf('费') < 0);
  feeShown ? ok(`手续费显示与配置一致（feePct=${feePct}）`) : bad(`手续费显示与配置不符：${pctTag}`);
  const note = await evaluate(ws, `(() => {
    const el = document.querySelector('.st-my-acct .fee-note');
    return el ? el.textContent.trim() : null;
  })()`);
  note && note.indexOf(String(sharePct) + '% 分成') >= 0
    ? ok('「我的抖音号」卡写了收益口径说明')
    : bad(`收益口径说明没写或不含配置值：${note}`);
  note && note.indexOf('35%') >= 0 && sharePct !== 35 ? bad('口径说明里还残留硬编码 35%') : ok('无硬编码残留');

  /* 4. 裂图兜底：把 src 改坏，触发 onerror，看是否回落且不留破图 */
  await evaluate(ws, `(() => {
    document.querySelectorAll('#view img[data-av]').forEach((img) => { img.src = '/api/public/avatars/999999?v=deadbeef'; });
    return true;
  })()`);
  await sleep(1200);
  const fallback = await evaluate(ws, `(() => {
    const box = document.querySelector('.st-my-av');
    const ico = document.querySelector('.st-my-acct .ico');
    return {
      headHasImg: box ? box.classList.contains('has-img') : null,
      headText: box ? box.textContent.trim() : null,
      icoHasImg: ico ? ico.classList.contains('has-img') : null,
      icoIcon: ico ? !!ico.querySelector('.material-symbols-outlined') : null,
      brokenImgs: document.querySelectorAll('#view img[data-av]').length,
    };
  })()`);
  fallback.headHasImg === false && fallback.headText && fallback.headText.length > 0
    ? ok(`大头像裂图回落首字「${fallback.headText}」`)
    : bad(`大头像裂图没回落：${JSON.stringify(fallback)}`);
  fallback.icoHasImg === false && fallback.icoIcon === true
    ? ok('抖音号行裂图回落到 material 图标')
    : bad(`抖音号行裂图没回落图标：${JSON.stringify(fallback)}`);
  fallback.brokenImgs === 0 ? ok('页面上没有残留的破图 <img>') : bad(`还有 ${fallback.brokenImgs} 个破图`);
  console.log('  ->  ' + (await shot(ws, '02-fallback.png')));

  const real = errors.filter((e) => !/404|Failed to load resource|net::ERR/i.test(e));
  real.length ? bad(`页面有 JS 报错：${real.slice(0, 3).join(' | ')}`) : ok('无 JS 报错');
} catch (e) {
  bad(String(e?.message ?? e));
} finally {
  ws.close();
  chrome.kill();
}

console.log(failed ? `\n== ${failed} 项失败 ==` : '\n== 全部通过 ==');
process.exit(failed ? 1 : 0);
