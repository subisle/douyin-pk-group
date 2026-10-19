/**
 * 底栏（#navRow）手势口径自检：
 *   1. **普通短按（tap）不切换路由**（2026-10-19 用户口径）；
 *   2. 长按 → 拖拽 → 松手，才切到落位那一格对应的路由；
 *   3. 拖回原格松手：不报错、路由不变。
 *
 * 用法：
 *   node scripts/check/uicheck-nav-drag.mjs [BASE] [管理员口令]
 * 默认 BASE=http://127.0.0.1:8787。需要先登录（站点 loginRequired=true 时被登录门挡住）。
 *
 * 只用 Node 内置能力（Node 22+ 自带 WebSocket），不需要 puppeteer/playwright。
 */
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { setTimeout as sleep } from 'node:timers/promises';

const BASE = (process.argv[2] ?? 'http://127.0.0.1:8787').replace(/\/$/, '');
const ADMIN_PASSWORD = process.argv[3] ?? '';
const OUT = fileURLToPath(new URL('../.uicheck/nav-drag/', import.meta.url));
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const PORT = 9337;
const PROFILE = '/tmp/pk-uicheck-navdrag-profile';

if (!ADMIN_PASSWORD) {
  console.error('用法：node scripts/check/uicheck-nav-drag.mjs [BASE] <管理员口令>');
  process.exit(2);
}

rmSync(OUT, { recursive: true, force: true });
mkdirSync(OUT, { recursive: true });
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
  await ws.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 2, mobile });
  await ws.send('Emulation.setTouchEmulationEnabled', { enabled: mobile, maxTouchPoints: 1 });
  return errors;
}

async function goto(ws, url) {
  await ws.send('Page.navigate', { url });
  await sleep(1200);
}

async function evaluate(ws, expr) {
  const r = await ws.send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true });
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? 'evaluate 失败');
  return r.result.value;
}

async function waitFor(ws, expr, tries = 40, wait = 250) {
  for (let i = 0; i < tries; i++) {
    const v = await evaluate(ws, expr).catch(() => false);
    if (v) return true;
    await sleep(wait);
  }
  return false;
}

async function shot(ws, name, { beyond = true } = {}) {
  const { data } = await ws.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: beyond });
  writeFileSync(OUT + name, Buffer.from(data, 'base64'));
  return OUT + name;
}

/* 用 CDP 原生输入事件模拟真实手势（dispatchTouchEvent：touchStart/Move/End） */
async function touch(ws, type, x, y) {
  const points = type === 'touchEnd' ? [] : [{ x, y }];
  await ws.send('Input.dispatchTouchEvent', { type, touchPoints: points });
}

/* ------------------------------ 开始 ------------------------------ */

const target = await cdpTarget();
const ws = await connect(target);

try {
  console.log(`== 底栏手势自检 · ${BASE} ==`);
  const errors = await session(ws, { width: 390, height: 844, mobile: true });

  /* 登录：走 UI 登录门（管理员口令 = 账号 = 密码） */
  await goto(ws, `${BASE}/st/m`);
  const gateUp = await waitFor(ws, `!document.getElementById('gate').classList.contains('hidden')`, 40, 250);
  if (!gateUp) {
    bad('登录门没弹出来（站点可能是开放态，本用例需要 loginRequired=true）');
  }
  await evaluate(ws, `(() => {
    document.getElementById('gateAcct').value = ${JSON.stringify(ADMIN_PASSWORD)};
    document.getElementById('gatePass').value = ${JSON.stringify(ADMIN_PASSWORD)};
    document.getElementById('gateGo').click();
    return true;
  })()`);
  const logged = await waitFor(ws, `document.getElementById('gate').classList.contains('hidden')`, 40, 250);
  logged ? ok('登录成功，登录门关闭') : bad('登录失败（登录门没关）');
  await sleep(1200);

  /* 登录后的「公告弹层」#itemPopup 是 z-index 89 全屏遮罩，会盖住底栏（z-31）并吃掉
     pointer 事件（测试里它会导致 pointercancel 中断拖拽）。它是一次性的产品行为，
     这里按用户视角关掉它再测手势：点遮罩空白处翻到最后一页即自动关闭。 */
  const dismissPopup = async () => {
    for (let i = 0; i < 12; i++) {
      const open = await evaluate(ws,
        `!document.getElementById('itemPopup')?.classList.contains('hidden')`).catch(() => false);
      if (!open) return true;
      await evaluate(ws, `(() => { const b = document.getElementById('itemPopup'); if (b) b.click(); return true; })()`);
      await sleep(300);
    }
    return false;
  };
  const popupGone = await dismissPopup();
  popupGone ? ok('公告弹层已关闭（不遮挡底栏）')
            : bad('公告弹层关不掉，底栏手势测试会被它遮住');
  await sleep(600);

  /* 读底栏可见格的中心坐标 + 当前路由 */
  const nav = await evaluate(ws, `(() => {
    const tabs = [...document.querySelectorAll('#navRow button[data-v]')].filter(b => b.offsetParent !== null);
    return tabs.map((b, i) => {
      const r = b.getBoundingClientRect();
      return { i, v: b.dataset.v, on: b.classList.contains('on'), cx: r.left + r.width / 2, cy: r.top + r.height / 2 };
    });
  })()`);
  console.log('  底栏可见格:', nav.map(t => `${t.i}:${t.v}${t.on ? '(选中)' : ''}`).join(' '));
  if (nav.length < 2) { bad('底栏可见格不足 2 个，没法测拖拽'); throw new Error('nav too small'); }

  const route = () => evaluate(ws, `location.pathname + location.search`);
  const activeV = () => evaluate(ws, `(() => {
    const b = [...document.querySelectorAll('#navRow button[data-v]')].find(x => x.classList.contains('on'));
    return b ? b.dataset.v : '';
  })()`);

  const idxOf = (v) => nav.findIndex(t => t.v === v);
  const curOn = nav.findIndex(t => t.on);
  const otherIdx = curOn === 0 ? 1 : 0;
  const from = nav[curOn], to = nav[otherIdx];

  /* ---------- 1. 普通短按：不切路由 ---------- */
  const r0 = await route(), v0 = await activeV();
  await touch(ws, 'touchStart', to.cx, to.cy);
  await sleep(60);                       // <150ms，且不移动
  await touch(ws, 'touchEnd', to.cx, to.cy);
  await sleep(900);
  const r1 = await route(), v1 = await activeV();
  r1 === r0 && v1 === v0
    ? ok(`短按「${to.v}」不切路由（路由 ${r1} / 选中 ${v1} 均未变）`)
    : bad(`短按竟然切了路由：${r0}(${v0}) → ${r1}(${v1})`);
  console.log('  ->  ' + (await shot(ws, '01-after-tap.png')));
  /* 短按后液态水滴必须留在原选中格（视觉上明确「没切换」） */
  const dropStill = await evaluate(ws, `(() => {
    const tabs = [...document.querySelectorAll('#navRow button[data-v]')].filter(b => b.offsetParent !== null);
    const on = tabs.findIndex(b => b.classList.contains('on'));
    const drop = document.getElementById('navDrop');
    if (!drop) return false;
    const dr = document.getElementById('navRow').getBoundingClientRect();
    const m = new DOMMatrixReadOnly(getComputedStyle(drop).transform);
    const tr = tabs[Math.max(0, on)].getBoundingClientRect();
    return Math.abs(m.m41 - (tr.left - dr.left - 6)) < 3;
  })()`);
  dropStill ? ok('短按后液态水滴仍停在原选中格') : bad('短按后水滴位置漂了');

  /* ---------- 2. 长按 + 拖拽 + 松手：切到目标格 ---------- */
  await touch(ws, 'touchStart', from.cx, from.cy);
  await sleep(260);                       // >150ms → 进入拖拽模式
  /* 分几步移动，模拟真实拖动（也触发速度感应） */
  const steps = 8;
  for (let s = 1; s <= steps; s++) {
    const x = from.cx + (to.cx - from.cx) * (s / steps);
    await touch(ws, 'touchMove', x, from.cy);
    await sleep(24);
  }
  /* 拖拽中视觉断言（2026-10-19 第四轮设计稿口径）：
     一格宽清澈水滴 + 色散只在最外沿 + 焦点图标琥珀；旧琥珀大光晕/压暗态必须消失。 */
  const dragVis = await evaluate(ws, `(() => {
    const dock = document.getElementById('navRow');
    const drop = document.getElementById('navDrop');
    const body = document.getElementById('navDropBody');
    const ring = document.getElementById('navDropRing');
    const tabs = [...document.querySelectorAll('#navRow button[data-v]')].filter(b => b.offsetParent !== null);
    const rr = drop.getBoundingClientRect();
    const csBody = getComputedStyle(body), csRing = getComputedStyle(ring);
    const onTab = tabs.find(b => b.classList.contains('on'));
    return {
      dragging: dock.classList.contains('is-droplet-dragging'),
      dropW: rr.width, tabW: tabs[0].getBoundingClientRect().width,
      bodyBg: csBody.backgroundColor, bodyBgImg: csBody.backgroundImage,
      ringConic: /conic-gradient/.test(csRing.backgroundImage),
      ringMask: csRing.maskComposite || csRing.webkitMaskComposite,
      focused: tabs.filter(b => b.classList.contains('is-droplet-focused')).length,
      dimOp: onTab && onTab.querySelector('.ic') ? getComputedStyle(onTab.querySelector('.ic')).opacity : null,
      oldNodes: !!(document.getElementById('navPill') || document.getElementById('navLens')),
      oldClasses: tabs.some(b => b.classList.contains('is-covered') || b.classList.contains('is-dim')),
    };
  })()`);
  /* 拖拽中截图必须用 viewport-only：captureBeyondViewport 会触发一次布局/resize，
     页面 resize 处理器若把水滴拉回选中格，会让这次拖拽的落点算错（踩过）。 */
  console.log('  ->  ' + (await shot(ws, '02-mid-drag.png', { beyond: false })));
  dragVis.dragging ? ok('拖拽中进入水滴跟手态（is-droplet-dragging）') : bad('拖拽中没进入 is-droplet-dragging');
  Math.abs(dragVis.dropW - dragVis.tabW) <= 1.5
    ? ok(`拖拽水滴保持一格宽（${dragVis.dropW}px ≈ ${dragVis.tabW}px）`)
    : bad(`拖拽水滴宽 ${dragVis.dropW}px ≠ 格宽 ${dragVis.tabW}px`);
  dragVis.bodyBgImg === 'none' && /rgba\(255, 255, 255, 0\.72\)/.test(dragVis.bodyBg)
    ? ok('水滴内部零彩光（无渐变，白 .72）')
    : bad(`水滴内部有彩光：${dragVis.bodyBg} / ${dragVis.bodyBgImg}`);
  dragVis.ringConic && /exclude/.test(dragVis.ringMask)
    ? ok('色散只在最外沿（conic 环 + mask exclude）')
    : bad('色散环不对：mask=' + dragVis.ringMask);
  !dragVis.oldNodes ? ok('旧 #navPill/#navLens 已移除') : bad('旧琥珀胶囊/光晕节点还在');
  !dragVis.oldClasses ? ok('旧 .is-covered/.is-dim 压暗态已移除') : bad('拖拽仍是旧压暗/覆盖态');
  dragVis.dimOp === '1' ? ok('拖拽不再压暗原选中项（opacity=1）') : bad('原选中项仍被压暗 opacity=' + dragVis.dimOp);
  dragVis.focused === 1 ? ok('拖拽焦点图标恰一格琥珀') : bad('focused 格数=' + dragVis.focused);

  await touch(ws, 'touchEnd', to.cx, to.cy);
  await sleep(1400);                      // 等磁吸(340ms) + 切页
  const r2 = await route(), v2 = await activeV();
  v2 === to.v
    ? ok(`长按拖拽松手后切到目标格「${to.v}」（路由 ${r2}）`)
    : bad(`长按拖拽松手后没有切到「${to.v}」：选中仍是 ${v2}，路由 ${r2}`);
  console.log('  ->  ' + (await shot(ws, '02-after-drag.png')));

  /* ---------- 3. 拖回原格松手：回到原路由 ---------- */
  const nowOn = idxOf(v2);
  const backTo = nav[curOn];
  await touch(ws, 'touchStart', nav[nowOn].cx, nav[nowOn].cy);
  await sleep(260);
  for (let s = 1; s <= steps; s++) {
    const x = nav[nowOn].cx + (backTo.cx - nav[nowOn].cx) * (s / steps);
    await touch(ws, 'touchMove', x, nav[nowOn].cy);
    await sleep(24);
  }
  await touch(ws, 'touchEnd', backTo.cx, backTo.cy);
  await sleep(1400);
  const r3 = await route(), v3 = await activeV();
  v3 === backTo.v
    ? ok(`拖回原格松手 → 回到「${backTo.v}」（路由 ${r3}）`)
    : bad(`拖回原格后没回到「${backTo.v}」：选中 ${v3}，路由 ${r3}`);
  console.log('  ->  ' + (await shot(ws, '03-after-drag-back.png')));

  /* ---------- 4. 无 JS 运行时错误 ---------- */
  errors.length === 0 ? ok('无 JS 运行时错误') : bad('JS 错误：' + errors.slice(0, 3).join(' | '));

  console.log(failed === 0 ? '\n底栏手势自检全部通过 ✓' : `\n底栏手势自检失败 ${failed} 项 ✗`);
  process.exitCode = failed === 0 ? 0 : 1;
} finally {
  ws.close();
  chrome.kill('SIGKILL');
}
