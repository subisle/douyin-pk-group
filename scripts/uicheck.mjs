/**
 * 页面渲染自检：用 Chrome DevTools Protocol 真正把页面渲染一遍。
 *
 * 为什么不用 tsc / vite build：那两个只保证「能编译」，保证不了「运行时不报错」。
 * 这个脚本会抓 console 报错、页面异常，并对关键 DOM 做断言，顺带按手机 / PC
 * 两种宽度截图。
 *
 * 用法：
 *   node scripts/uicheck.mjs http://64.188.8.149:18791 demo-pass-2026
 *
 * 只用 Node 内置能力（Node 22+ 自带 WebSocket），不需要 puppeteer/playwright。
 */
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { setTimeout as sleep } from 'node:timers/promises';

const BASE = process.argv[2] ?? 'http://127.0.0.1:8787';
const PASSWORD = process.argv[3] ?? '';
// 必须用 fileURLToPath：目录名含中文，URL.pathname 会留下 %E6%8A%96 之类的编码
const OUT = fileURLToPath(new URL('../.uicheck/', import.meta.url));
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const PORT = 9333;

rmSync(OUT, { recursive: true, force: true });
mkdirSync(OUT, { recursive: true });

const chrome = spawn(CHROME, [
  '--headless=new',
  `--remote-debugging-port=${PORT}`,
  '--user-data-dir=/tmp/pk-uicheck-profile',
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
      const r = await fetch(`http://127.0.0.1:${PORT}/json/list`);
      const list = await r.json();
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
    ws.onopen = () =>
      resolve({
        send: (method, params = {}) =>
          new Promise((res, rej) => {
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

/* --------------------------------- 页面会话 --------------------------------- */

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
  await ws.send('Emulation.setDeviceMetricsOverride', {
    width, height, deviceScaleFactor: 1, mobile,
  });
  return errors;
}

async function goto(ws, url) {
  await ws.send('Page.navigate', { url });
  // 展示页首次请求要真查一次库拿实时音浪，冷启动可能好几秒，
  // 所以轮询等「不再是加载态」而不是死等固定时间。
  for (let i = 0; i < 40; i++) {
    await sleep(500);
    const ready = await evaluate(
      ws,
      `!document.body.innerText.includes('加载中') && document.getElementById('root').firstElementChild != null`,
    ).catch(() => false);
    if (ready) return;
  }
}

async function evaluate(ws, expr) {
  const r = await ws.send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true });
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? '求值失败');
  return r.result.value;
}

/* GroupBoard 的分组列带 data-group-col，排序卡片带 data-player-card。
 * 靠这两个属性定位，避免 Tailwind 类名一改断言就失效。 */
const GROUP_COLS_JS = `[...document.querySelectorAll('[data-group-col]')]`;

async function shot(ws, name) {
  const { data } = await ws.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: true });
  writeFileSync(OUT + name, Buffer.from(data, 'base64'));
  return OUT + name;
}

/* ---------------------------------- 开始 ---------------------------------- */

const url = await cdpTarget();
const ws = await connect(url);

try {
  /* ============ 1. 对外展示页：手机 ============ */
  console.log('== 对外展示页 /s/1 · 手机 390x844 ==');
  let errors = await session(ws, { width: 390, height: 844, mobile: true });
  await goto(ws, `${BASE}/s/1`);

  expect('页面标题', await evaluate(ws, 'document.title'), 'PK 挑战赛分组');
  await sleep(800);
  const mobile = JSON.parse(await evaluate(ws, `(() => {
    const g = document.querySelector('[data-group-card]')?.parentElement;
    return JSON.stringify({
      root: !!document.getElementById('root').firstElementChild,
      cards: document.querySelectorAll('[data-group-card]').length,
      players: document.querySelectorAll('[data-player-card]').length,
      dates: document.querySelectorAll('[data-date-chip]').length,
      slots: document.querySelectorAll('[data-group-slot]').length,
      pager: document.querySelector('[data-pager]')?.innerText.replace(/\s+/g, ' ') ?? '',
      pagerCount: document.querySelectorAll('[data-pager]').length,
      trend: !!document.querySelector('svg path'),
      hasSchedule: /开赛/.test(document.body.innerText),
      hasWave: /音浪/.test(document.body.innerText),
      hasTrendTitle: /每日音浪曲线/.test(document.body.innerText),
      colWidth: g ? Math.round(g.getBoundingClientRect().width) : 0,
      viewport: window.innerWidth,
      hScroll: document.documentElement.scrollWidth > window.innerWidth + 1,
      bodyText: document.body.innerText.slice(0, 80),
    });
  })()`));
  mobile.root ? ok('React 已挂载') : bad('React 没挂载');
  expect('手机端每页分组数', mobile.cards, 1);
  mobile.players > 0 ? ok(`组内渲染 ${mobile.players} 人`) : bad('组内没渲染人员');
  mobile.slots > 0 ? ok(`赛程时段 ${mobile.slots} 段`) : bad('赛程时长没渲染');
  mobile.hasSchedule ? ok('展示了赛程时长') : bad('缺少赛程时长');
  mobile.pagerCount > 0 ? ok(`分页控件：${mobile.pager}`) : bad('没有分页控件');
  // 公布页刻意不上音浪：这三样都不该出现在页面上
  mobile.hasWave ? bad('公布页出现了「音浪」，与新契约冲突') : ok('全页无「音浪」字样');
  mobile.dates === 0 ? ok('日期条已下线（只为切音浪快照而存在）') : bad(`还剩 ${mobile.dates} 个日期 chip`);
  mobile.hasTrendTitle ? bad('每日音浪曲线还在页面上') : ok('每日音浪曲线已下线');
  expect('手机端无横向滚动', mobile.hScroll, false);
  mobile.colWidth > 300 && mobile.colWidth < 400
    ? ok(`单列宽度 ${mobile.colWidth}px（视口 ${mobile.viewport}px）`)
    : bad(`手机端列宽异常：${mobile.colWidth}px`);
  console.log(`  ..  首屏文字：${mobile.bodyText.replace(/\n/g, ' / ')}`);
  console.log('  ->  ' + (await shot(ws, 'share-mobile.png')));
  errors.length ? bad('手机端 console 报错: ' + errors.join(' | ')) : ok('手机端无 console 报错');

  /* ============ 2. 对外展示页：PC 1440x900 ============ */
  console.log('== 对外展示页 /s/1 · PC 1440x900 ==');
  errors = await session(ws, { width: 1440, height: 900, mobile: false });
  await goto(ws, `${BASE}/s/1`);
  await sleep(800);
  const desk = JSON.parse(await evaluate(ws, `(() => {
    const cards = [...document.querySelectorAll('[data-group-card]')];
    const tops = [...new Set(cards.map(c => Math.round(c.getBoundingClientRect().top)))];
    return JSON.stringify({
      cards: cards.length,
      rows: tops.length,
      names: cards.map(c => c.dataset.groupCard),
      pager: document.querySelector('[data-pager]')?.innerText.replace(/\s+/g, ' ') ?? '',
      hScroll: document.documentElement.scrollWidth > window.innerWidth + 1,
    });
  })()`));
  expect('PC 端每页分组数', desk.cards, 3);
  desk.rows === 1 ? ok('PC 端三组并排一行') : bad(`PC 端排了 ${desk.rows} 行`);
  desk.pager ? ok(`分页控件：${desk.pager}`) : bad('没有分页控件');
  expect('PC 端无横向滚动', desk.hScroll, false);
  console.log('  ->  ' + (await shot(ws, 'share-pc.png')));
  errors.length ? bad('PC 端 console 报错: ' + errors.join(' | ')) : ok('PC 端无 console 报错');

  /* ============ 3. 翻页 + 赛程条联动 ============ */
  console.log('== 翻页与赛程条联动 ==');
  const paged = JSON.parse(await evaluate(ws, `(() => {
    const before = [...document.querySelectorAll('[data-group-card]')].map(c => c.dataset.groupCard);
    const next = [...document.querySelectorAll('[data-pager] button')].find(b => b.getAttribute('aria-label') === '下一页');
    next?.click();
    return JSON.stringify({ before, clicked: !!next });
  })()`));
  await sleep(400);
  const after = JSON.parse(await evaluate(ws, `JSON.stringify({
    names: [...document.querySelectorAll('[data-group-card]')].map(c => c.dataset.groupCard),
    pager: document.querySelector('[data-pager]')?.innerText.replace(/\s+/g, ' ') ?? '',
  })`));
  paged.clicked ? ok('点「下一页」') : bad('没有下一页按钮');
  JSON.stringify(after.names) !== JSON.stringify(paged.before) && after.names.length > 0
    ? ok(`翻页生效：${paged.before.join('/')} → ${after.names.join('/')}（${after.pager}）`)
    : bad(`翻页后分组没变：${after.names.join('/')}`);
  console.log('  ->  ' + (await shot(ws, 'share-page2.png')));

  const dated = JSON.parse(await evaluate(ws, `(() => {
    const slots = [...document.querySelectorAll('[data-group-slot]')];
    const last = slots[slots.length - 1];
    last?.click();
    return JSON.stringify({
      clicked: !!last,
      label: last?.dataset.groupSlot,
      active: document.querySelectorAll('[data-group-slot][aria-current=true]').length,
    });
  })()`));
  await sleep(500);
  const dayState = JSON.parse(await evaluate(ws, `JSON.stringify({
    head: document.querySelector('header')?.innerText.replace(/\s+/g, ' ').slice(0, 90) ?? '',
    visible: [...document.querySelectorAll('[data-group-card]')].map(c => c.dataset.groupCard),
    onPage: [...document.querySelectorAll('[data-group-slot][aria-current=true]')].map(b => b.dataset.groupSlot),
    pager: document.querySelector('[data-pager]')?.innerText.replace(/\s+/g, ' ') ?? '',
  })`));
  dated.clicked ? ok(`点赛程条「${dated.label}」跳到该组所在页`) : bad('赛程条没渲染');
  dated.active > 0 ? ok(`当前页 ${dated.active} 个时段高亮（${dayState.pager}）`) : bad('赛程条没有当前页高亮');
  // 点时段是跳到「包含该组的那一页」，不是把该组顶到第一位，所以断言该组在当前页可见
  dayState.visible.includes(dated.label)
    ? ok(`跳页正确：${dayState.visible.join('/')}（${dated.label} 在其中）`)
    : bad(`跳页后是 ${dayState.visible.join('/')}，不含 ${dated.label}`);
  dayState.onPage.includes(dated.label)
    ? ok(`高亮跟到 ${dayState.onPage.join('/')}`)
    : bad(`高亮没跟上：${dayState.onPage.join('/')}`);
  console.log('  ->  ' + (await shot(ws, 'share-dated.png')));
  errors.length ? bad('翻页/点时段 console 报错: ' + errors.join(' | ')) : ok('翻页/点时段无 console 报错');

  /* ============ 4. 管理员登录页 ============ */
  console.log('== 管理员登录页 ==');
  errors = await session(ws, { width: 1280, height: 900, mobile: false });
  // 上一轮登录的 token 还在 localStorage 里，不清掉就进不了登录页
  await goto(ws, `${BASE}/`);
  await evaluate(ws, 'localStorage.clear()');
  await goto(ws, `${BASE}/`);
  const login = JSON.parse(await evaluate(ws, `JSON.stringify({
    hasPassword: !!document.querySelector('input[type=password]'),
    text: document.body.innerText.slice(0, 80).replace(/\\n/g, ' / '),
  })`));
  login.hasPassword ? ok('渲染出口令输入框') : bad('没渲染出口令输入框');
  console.log('  ->  ' + (await shot(ws, 'login.png')));

  /* ============ 5. 登录 → 分组看板 ============ */
  if (login.hasPassword && PASSWORD) {
    console.log('== 登录并进入看板 ==');
    await evaluate(ws, `(() => {
      const i = document.querySelector('input[type=password]');
      const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value').set;
      setter.call(i, ${JSON.stringify(PASSWORD)});
      i.dispatchEvent(new Event('input', { bubbles: true }));
      return true;
    })()`);
    await evaluate(ws, `[...document.querySelectorAll('button')].find(b => b.textContent.includes('进入后台'))?.click()`);
    await sleep(3000);
    const board = JSON.parse(await evaluate(ws, `JSON.stringify({
      loggedIn: !document.querySelector('input[type=password]'),
      text: document.body.innerText.slice(0, 120).replace(/\\n/g, ' / '),
    })`));
    board.loggedIn ? ok('口令登录成功，进入看板') : bad('登录后仍停在登录页');
    console.log(`  ..  看板首屏：${board.text}`);
    errors.length ? bad('看板 console 报错: ' + errors.join(' | ')) : ok('看板无 console 报错');

    /* ---- 载入已保存方案：让分组看板 / 卡片菜单用真实 54 人数据渲染 ---- */
    console.log('== 载入方案，渲染分组看板 ==');
    await evaluate(ws, `[...document.querySelectorAll('button')].find(b => b.textContent.includes('历史方案'))?.click()`);
    await sleep(1200);
    const loaded = await evaluate(ws, `(() => {
      const btn = [...document.querySelectorAll('button')].find(b => b.textContent.trim() === '载入');
      if (!btn) return 'no-load-button';
      btn.click();
      return 'clicked';
    })()`);
    loaded === 'clicked' ? ok('点「载入」加载方案') : bad('历史方案里没有「载入」按钮');
    // 载入后会查库 + 跑一次 inspect，轮询等分组列真的出现
    for (let i = 0; i < 40; i++) {
      await sleep(500);
      const n = await evaluate(ws, `${GROUP_COLS_JS}.length`).catch(() => 0);
      if (n > 1) break;
    }
    const board2 = JSON.parse(await evaluate(ws, `(() => {
      const cols = ${GROUP_COLS_JS};
      const cards = [...document.querySelectorAll('[data-player-card]')];
      return JSON.stringify({
        columns: cols.length,
        players: cards.length,
        firstCol: cols[0]?.innerText.replace(/\\n/g, ' ').slice(0, 70) ?? '',
        tools: !!document.body.innerText.includes('均衡微调'),
        search: !!document.querySelector('input[placeholder*="找主播"]'),
        sort: !!document.body.innerText.includes('组内排序'),
        dense: [...document.querySelectorAll('button')].some(b => b.title.includes('紧凑模式')),
      });
    })()`));
    expect('看板分组列数', board2.columns, 7);
    expect('卡片总数', board2.players, 54);
    /* 回归用例：曾经 arrange 不排组内，序号和音浪会反向（第 8 名音浪比第 6 名大） */
    const order = JSON.parse(await evaluate(ws, `(() => {
      const cols = ${GROUP_COLS_JS};
      return JSON.stringify(cols.map(c => [...c.querySelectorAll('[data-player-card]')].map(card => {
        const t = card.innerText.split('\\n');
        return { idx: Number(t[0]), wave: t[2] || '' };
      })));
    })()`));
    let misordered = 0;
    for (const col of order) {
      for (let i = 1; i < col.length; i++) {
        const a = parseFloat(col[i - 1].wave.replace(/[^0-9.]/g, '')) * (col[i - 1].wave.includes('万') ? 10000 : 1);
        const b = parseFloat(col[i].wave.replace(/[^0-9.]/g, '')) * (col[i].wave.includes('万') ? 10000 : 1);
        if (col[i - 1].idx + 1 !== col[i].idx || b > a) misordered++;
      }
    }
    expect('组内序号连续且音浪降序', misordered, 0);
    board2.tools ? ok('工具条已渲染（均衡微调）') : bad('工具条没渲染');
    board2.search ? ok('全局搜索框已渲染') : bad('没渲染全局搜索框');
    board2.sort ? ok('「组内排序」按钮已渲染') : bad('没渲染「组内排序」');
    board2.dense ? ok('「紧凑」切换已渲染') : bad('没渲染「紧凑」切换');
    console.log(`  ..  第一组：${board2.firstCol}`);
    console.log('  ->  ' + (await shot(ws, 'board-groups.png')));
    errors.length ? bad('分组看板 console 报错: ' + errors.join(' | ')) : ok('分组看板无 console 报错');

    /* ---- 手机上看分组看板：确认拖拽区不横向溢出 ---- */
    await session(ws, { width: 390, height: 844, mobile: true });
    await sleep(600);
    const boardMobile = JSON.parse(await evaluate(ws, `JSON.stringify({
      hScroll: document.documentElement.scrollWidth > window.innerWidth + 1,
      cols: ${GROUP_COLS_JS}.length,
    })`));
    expect('看板手机端无横向滚动', boardMobile.hScroll, false);
    expect('看板手机端分组列数', boardMobile.cols, 7);
    console.log('  ->  ' + (await shot(ws, 'board-mobile.png')));
  }

  /* ============ 6. 分享链接状态码 ============ */
  console.log('== 分享链接 HTTP 状态 ==');
  for (const [label, path, want] of [
    ['已上架方案', '/s/1', 200],
    ['不存在的方案', '/s/99999', 200],
    ['不存在的接口', '/api/does-not-exist', 404],
  ]) {
    const r = await fetch(BASE + path, { redirect: 'manual' });
    expect(label, r.status, want);
  }
} catch (e) {
  bad(`脚本异常：${e.message}`);
} finally {
  ws.close();
  chrome.kill();
}

console.log();
console.log(failed === 0 ? `全部通过，截图在 ${OUT}` : `${failed} 项失败，截图在 ${OUT}`);
process.exit(failed === 0 ? 0 : 1);
