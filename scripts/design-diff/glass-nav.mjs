// ─────────────────────────────────────────────────────────────────────────────
// 设计稿 ↔ 本地页面 数值对照工具（玻璃拟态主播数据榜 / glass nav）
//
// 用途：主模型看不到截图，所以改用「数值对比」代替视觉对比 —— 在 390×844
//       视口下分别加载「Stitch 设计稿 HTML」和「本地 Rust 服务页面」，抓每个
//       关键元素的 getComputedStyle 属性 + getBoundingClientRect 几何，
//       逐字段 diff，输出 Markdown 对照表 + JSON 落盘。
//
// 运行：node scripts/design-diff/glass-nav.mjs
//       （在仓库根目录下跑；脚本自己按 __dirname 定位仓库根，所以在哪跑都行）
//
// 环境坑（踩过的都记在这里，改脚本前先看）：
//  1. Playwright 只能按绝对路径 import：
//       import { chromium } from '/opt/homebrew/lib/node_modules/@playwright/mcp/node_modules/playwright/index.mjs'
//     项目 node_modules 里没有 playwright，pnpm 装的那份也不带浏览器二进制。
//  2. 必须 chromium.launch({ channel: 'chrome' }) —— 本机只装了 Google Chrome，
//     playwright 自带的 chromium 二进制根本没下载，用默认 channel 会直接报
//     "Executable doesn't exist"。
//  3. 设计稿是 Tailwind CDN 版（<script src="https://cdn.tailwindcss.com">），
//     直接 file:// 打开拿不到外部 CSS，所有类名样式都是空的。必须用路由拦截
//     把它喂给一个 http:// 源（design.local），再等 ~3s 让 CDN 编译完 Tailwind。
//  4. 设计稿里主播头像是 https://lh3.googleusercontent.com/... 外网图，本机拉不到，
//     不拦会等到 goto 超时（而且头像撑不出尺寸，几何会失真）。这里统一拦成
//     1×1 透明 PNG。注意 route 模式写成 '**://lh3.googleusercontent.com/**'，
//     只写 '**lh3.googleusercontent.com/**' 匹配不到。
//  5. 本地页面（st_page.html）有登录门：先填 #gateAcct / #gatePass 再点 #gateGo。
//     登录后可能自动弹 #itemPopup（奖励弹窗），点空白处翻页，翻完自动关；
//     翻页落点坐标 (195,30) 是弹窗外的空白区，点 3~4 次直到它带 hidden class。
//  6. 本地页面 HTML 由 Rust 的 include_str!("st_page.html") 内嵌 —— 改完
//     st_page.html 必须重新 cargo 编译才生效。本脚本不负责编译，也不该去
//     kill / 重启 8787 端口上已在跑的 pk-server 进程，只负责抓。
//  7. 取指标前至少 waitForTimeout(1500)，等入场动画、字体、图片稳定；
//     过早取值会拿到动画中间态（transform/left 尤其明显）。
//  8. 本脚本是诊断工具：任何选择器取不到只记 null，绝不抛错；退出码永远 0，
//     不因为「有不一致」就失败。
//
// 覆盖方式（st_page.html 重构中类名会变，脚本用「多重候选 + 取第一个命中的」）：
//   - 环境变量 GLASSDIFF_OVERRIDE 传 JSON 字符串，key = 逻辑名，value = 选择器
//     数组（或单串）。例：
//       GLASSDIFF_OVERRIDE='{"local.wrap":["main.wrap2","main"]}' node scripts/design-diff/glass-nav.mjs
//   - 命令行 --set <逻辑名>=<选择器> [--set ...] 同上，多次可累加，优先级高于环境变量。
//   - 逻辑名见下面 PROBES 表的 probe key（local.* 那一列）。
// ─────────────────────────────────────────────────────────────────────────────

import { chromium } from '/opt/homebrew/lib/node_modules/@playwright/mcp/node_modules/playwright/index.mjs';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(__dirname, '..', '..');
const DESIGN_HTML = path.join(REPO, '.stitch', 'designs', 'stitch_streamer_glass_nav', 'code.html');
const OUT_JSON = path.join(__dirname, 'last-diff.json');

const LOCAL_URL = process.env.GLASSDIFF_URL || 'http://127.0.0.1:8787/';
const GATE_ACCT = process.env.GLASSDIFF_ACCT || '13800000000';
const GATE_PASS = process.env.GLASSDIFF_PASS || '13800000000';

const VIEWPORT = { width: 390, height: 844 };

// 1×1 透明 PNG，用来顶掉设计稿里拉不到的 googleusercontent 头像
const PIXEL = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
  'base64'
);

// ── 覆盖表 ───────────────────────────────────────────────────────────────────
const override = {};
if (process.env.GLASSDIFF_OVERRIDE) {
  try { Object.assign(override, JSON.parse(process.env.GLASSDIFF_OVERRIDE)); }
  catch (e) { console.warn('[warn] GLASSDIFF_OVERRIDE 不是合法 JSON，忽略：', e.message); }
}
for (let i = 0; i < process.argv.length; i++) {
  if (process.argv[i] === '--set' && process.argv[i + 1]) {
    const [k, ...rest] = process.argv[i + 1].split('=');
    if (k && rest.length) override[k.trim()] = [rest.join('=').trim()];
  }
}

// ── 探针表 ───────────────────────────────────────────────────────────────────
// 每项：区块 / 展示名 / 设计稿选择器（数组，取第一个命中的）/ 本地选择器 / 要取的 CSS 属性
// 设计侧取值逻辑直接沿用 /tmp/ip-glass/inspect.mjs（已验证能取到全部指标）。
const PROBES = [
  // ── 页面 / 主容器 ──
  { sec: '页面', name: 'body', design: 'body', local: ['body'], props: ['background-color', 'background-image', 'font-family', 'color', 'font-size'] },
  { sec: '页面', name: 'main.wrap', design: 'main', local: ['main.wrap', 'main', '.wrap'], props: ['max-width', 'background-color', 'background-image', 'box-shadow', 'padding-bottom', 'border-left-width'] },

  // ── 头部 ──
  { sec: '头部', name: 'masthead', design: 'header', local: ['.masthead', 'header.masthead', 'header'], props: ['position', 'background-color', 'backdrop-filter', 'border-bottom', 'padding-top', 'padding-bottom', 'z-index'] },
  { sec: '头部', name: 'mh-title .t (h1)', design: 'h1', local: ['.mh-title .t', '.mh-title h1', '.mh-title'], props: ['font-size', 'font-weight', 'letter-spacing', 'color', 'text-shadow'] },
  { sec: '头部', name: 'mh-sub', design: 'header .text-\\[11px\\]', local: ['.mh-sub', '#mhSub'], props: ['font-size', 'color', 'margin-top', 'font-family', 'text-align'] },
  { sec: '头部', name: 'team-switch', design: 'header .inline-flex', local: ['.team-switch', '#teamSwitch'], props: ['background-color', 'padding', 'border-radius', 'font-size', 'border', 'box-shadow', 'backdrop-filter'] },
  { sec: '头部', name: 'team-switch button', design: 'header .inline-flex button', local: ['.team-switch button', '.team-switch button.on'], props: ['background-color', 'color', 'padding', 'border-radius', 'font-size', 'font-weight', 'box-shadow', 'border'] },
  { sec: '头部', name: 'mh-ann (公告胶囊)', design: '.mt-2\\.5', local: ['.mh-ann', '#mhAnn'], props: ['border-radius', 'padding', 'background-image', 'background-color', 'box-shadow', 'height'] },
  { sec: '头部', name: 'mh-ann 标题', design: '.bg-clip-text', local: ['.mh-ann .mha-tx', '.mh-ann'], props: ['font-size', 'background-image', 'font-weight', 'letter-spacing', 'color'] },
  { sec: '头部', name: 'mh-ann 角标', design: '.text-amber-700.font-mono', local: ['.mh-ann .mha-idx'], props: ['font-size', 'color', 'padding', 'border-radius', 'background-color', 'border'] },
  { sec: '头部', name: 'ping 圆点', design: 'header .animate-ping', local: ['.mh-dot', '.masthead .dot'], props: ['width', 'height', 'background-color', 'box-shadow'] },

  // ── 工具栏 ──
  { sec: '工具栏', name: 'period bar (st-seg)', design: '#period-segmented-bar', local: ['#seg', '.st-seg'], props: ['background-color', 'padding', 'border-radius', 'border', 'box-shadow', 'backdrop-filter', 'height'] },
  { sec: '工具栏', name: 'seg 容器', design: '#period-segmented-bar', local: ['.seg.st-seg', '#seg', '.st-seg'], props: ['background-color', 'padding', 'border-radius', 'border', 'box-shadow', 'backdrop-filter', 'height'] },
  { sec: '工具栏', name: 'seg active pill', design: '#period-active-pill', local: ['#segPill'], props: ['background-color', 'border', 'box-shadow', 'border-radius', 'width', 'height', 'transform'] },
  { sec: '工具栏', name: 'seg button.on', design: '.period-tab-btn', local: ['.seg button.on', '#seg button.on', '.st-seg button.on'], props: ['font-size', 'font-weight', 'color', 'padding', 'height'] },
  { sec: '工具栏', name: 'dates wrap', design: '#period-date-container', local: ['.dates', '#dates'], props: ['gap', 'display'] },
  { sec: '工具栏', name: 'date button.on', design: '#current-period-tag', local: ['.dates button.on', '#dates button.on'], props: ['background-color', 'color', 'font-size', 'padding', 'border-radius', 'box-shadow', 'border', 'height'] },
  { sec: '工具栏', name: 'date button (次)', design: '#period-sub-1', local: ['.dates button:not(.on)', '#dates button:not(.on)'], props: ['background-color', 'color', 'font-size', 'padding', 'border-radius', 'height', 'border'] },
  { sec: '工具栏', name: 'icon-btn', design: '.w-8.h-8', local: ['.icon-btn', '#btnCal', '#btnSearch'], props: ['width', 'height', 'border-radius', 'background-color', 'border', 'box-shadow'] },

  // ── 领奖台 ──
  { sec: '领奖台', name: 'st-podium (grid)', design: '[aria-label="排名前三领奖台"] .grid', local: ['.st-podium'], props: ['grid-template-columns', 'gap', 'padding-top', 'padding-bottom', 'display'] },
  { sec: '领奖台', name: 'pod-1 容器', design: '[aria-label="排名前三领奖台"] .grid > div:nth-child(2)', local: ['.st-pod-1', '.st-pod.st-pod-1'], props: ['transform', 'margin-top', 'padding-bottom'] },
  { sec: '领奖台', name: 'pod-1 头像环', design: '[aria-label="排名前三领奖台"] .grid > div:nth-child(2) .w-20', local: ['.st-pod-1 .st-pod-av', '.st-pod-1 .st-pod-ring'], props: ['width', 'height', 'padding', 'background-image', 'border-radius', 'box-shadow'] },
  { sec: '领奖台', name: 'pod-2 头像环', design: '[aria-label="排名前三领奖台"] .grid > div:nth-child(1) .w-16', local: ['.st-pod-2 .st-pod-av', '.st-pod-2 .st-pod-ring'], props: ['width', 'height', 'padding', 'background-image', 'border-radius'] },
  { sec: '领奖台', name: 'pod-1 名字', design: '[aria-label="排名前三领奖台"] .grid > div:nth-child(2) span.font-extrabold', local: ['.st-pod-1 .st-pod-nm'], props: ['font-size', 'font-weight', 'color'] },
  { sec: '领奖台', name: 'pod-1 音浪', design: '.text-xl.font-black', local: ['.st-pod-1 .st-pod-wv', '.st-pod-wv'], props: ['font-size', 'font-weight', 'background-image', 'color'] },
  { sec: '领奖台', name: 'pod-2 音浪', design: '.text-base.font-black', local: ['.st-pod-2 .st-pod-wv'], props: ['font-size', 'font-weight'] },
  { sec: '领奖台', name: 'pod-1 标签', design: '#podium-unit-1', local: ['.st-pod-1 .st-pod-lbl', '.st-pod-lbl'], props: ['font-size', 'color', 'font-weight'] },
  { sec: '领奖台', name: 'pod-1 总计', design: '.text-\\[10px\\].text-amber-700\\/80', local: ['.st-pod-1 .st-pod-total', '.st-pod-total'], props: ['font-size', 'color'] },

  // ── 榜单 ──
  { sec: '榜单', name: 'st-list', design: '[aria-label="主播排行榜名单"]', local: ['.st-list'], props: ['display'] },
  { sec: '榜单', name: 'st-list-head', design: '[aria-label="主播排行榜名单"] .flex.items-center.justify-between', local: ['.st-list-head'], props: ['font-size', 'color', 'padding', 'border-bottom', 'display', 'justify-content'] },
  { sec: '榜单', name: 'st-list-body (卡片)', design: '.glass-card', local: ['.st-list-body'], props: ['background-color', 'border-radius', 'border', 'box-shadow', 'backdrop-filter'] },
  { sec: '榜单', name: 'st-row', design: '.glass-card .flex.items-center.justify-between', local: ['.st-row', '.st-list-body .st-row'], props: ['padding', 'height', 'display', 'align-items'] },
  { sec: '榜单', name: 'st-row-rk', design: '.glass-card .font-black.text-base', local: ['.st-row-rk'], props: ['font-size', 'color', 'font-family', 'width', 'text-align'] },
  { sec: '榜单', name: 'st-row-av', design: '.glass-card .w-11', local: ['.st-row-av'], props: ['width', 'height', 'border-radius', 'box-shadow'] },
  { sec: '榜单', name: 'st-row-nm', design: '.glass-card .font-bold.text-\\[15px\\]', local: ['.st-row-nm'], props: ['font-size', 'font-weight', 'color'] },
  { sec: '榜单', name: 'st-row-wv', design: '.glass-card .text-right .font-black', local: ['.st-row-wv'], props: ['font-size', 'font-family', 'font-weight'] },
  { sec: '榜单', name: 'st-row-lbl', design: '.glass-card .text-\\[10px\\]', local: ['.st-row-lbl'], props: ['font-size', 'color'] },

  // ── 底栏 nav ──
  { sec: '底栏', name: 'navRow 容器', design: '#liquid-tab-bar', local: ['#navRow', '.tabbar', 'nav.tabbar'], props: ['border-radius', 'padding', 'background-color', 'border', 'box-shadow', 'backdrop-filter', 'height'] },
  { sec: '底栏', name: 'nav 外层定位', design: '#liquid-tab-bar', local: ['#navRow', 'nav.tabbar'], props: ['position', 'bottom', 'left', 'width', 'max-width', 'z-index'] },
  { sec: '底栏', name: 'nav button', design: '.nav-tab-item', local: ['#navRow > button', '.tabbar > button'], props: ['flex', 'height', 'font-size', 'color', 'background', 'padding-top', 'padding-bottom'] },
  { sec: '底栏', name: 'nav button.on', design: '.nav-tab-item .tab-label', local: ['#navRow button.on .tab-label', '.tabbar button.on .tab-label'], props: ['font-size', 'font-weight', 'color'] },
  { sec: '底栏', name: 'nav 未选中文字', design: '.nav-tab-item[data-index="1"] .tab-label', local: ['#navRow button:not(.on) .tab-label', '.tabbar button:not(.on) .tab-label'], props: ['font-size', 'font-weight', 'color'] },
  { sec: '底栏', name: 'nav 图标 svg', design: '.nav-tab-item svg', local: ['.tabbar button .ic svg', '#navRow button svg'], props: ['width', 'height'] },
  { sec: '底栏', name: 'nav active pill', design: '#active-indicator', local: ['#navPill'], props: ['border-radius', 'background-image', 'box-shadow', 'border', 'backdrop-filter', 'width'] },
];

// 内容驱动差异白名单：这些字段的不一致来自两边数据不同（mock 3 行 vs 真实 99 人），
// 不是样式问题。单列为 expected，不计入「不一致」。
const CONTENT_DRIVEN = new Set([
  'body.rect.h', 'body.rect.y',
  'main.wrap.rect.h', 'main.wrap.rect.y',
  'st-list.rect.h', 'st-list-body (卡片).rect.h',
  'dates wrap.rect.w', 'date button.on.rect.w', 'date button.on.rect.x',
  'date button (次).rect.x', 'date button (次).rect.w',
  'icon-btn.rect.x', 'icon-btn.rect.y',
  'mh-ann 标题.rect.x', 'mh-ann 标题.rect.w',   // 图标是 emoji vs Material Symbols，字形宽度天然不同
  'st-list.rect.x', 'st-list.rect.w',            // 全宽 section：依赖行数与视口内容高度
]);

// 探针名必须唯一 —— 重名会让 design/local 结果互相覆盖（踩过：'active pill'
// 同时在工具栏和底栏出现，工具栏的值被底栏顶掉）
{
  const seen = new Set();
  const dup = [];
  for (const p of PROBES) { if (seen.has(p.name)) dup.push(p.name); seen.add(p.name); }
  if (dup.length) {
    console.error('[错误] PROBES 里探针名重复：', dup.join(', '), '—— 请改名后再跑');
    process.exit(0); // 诊断工具仍然 0，但明确报错
  }
}

// ── 浏览器内取值 helper（序列化成字符串注入） ────────────────────────────────
const PICK_SRC = `
  (probes) => {
    const pick = (el, props) => {
      if (!el) return null;
      const cs = getComputedStyle(el);
      const r = el.getBoundingClientRect();
      const o = { rect: { x: +r.x.toFixed(1), y: +r.y.toFixed(1), w: +r.width.toFixed(1), h: +r.height.toFixed(1) } };
      (props || []).forEach(p => {
        let v = '';
        try { v = cs.getPropertyValue(p); } catch (_) {}
        if (v !== undefined && v !== '') o[p] = v;
      });
      return o;
    };
    const first = (sels) => {
      const list = Array.isArray(sels) ? sels : [sels];
      for (const s of list) {
        if (!s) continue;
        let el = null;
        try { el = document.querySelector(s); } catch (_) { el = null; }
        if (el) return el;
      }
      return null;
    };
    const out = {};
    for (const p of probes) {
      try { out[p.name] = pick(first(p.sel), p.props); }
      catch (_) { out[p.name] = null; }
    }
    return out;
  }
`;

function probesFor(side) {
  return PROBES.map((p) => {
    const base = side === 'design' ? p.design : p.local;
    const ov = override[`${side}.${p.name}`] || override[p.name];
    let sel = ov ? (Array.isArray(ov) ? ov : [ov]) : (Array.isArray(base) ? base : [base]);
    return { name: p.name, sel, props: p.props };
  });
}

// ── 设计稿加载 ───────────────────────────────────────────────────────────────
async function loadDesign(browser) {
  const page = await browser.newPage({ viewport: VIEWPORT, deviceScaleFactor: 2 });
  let html = '';
  try { html = fs.readFileSync(DESIGN_HTML, 'utf8'); }
  catch (e) { return { page, error: `读不到设计稿 ${DESIGN_HTML}：${e.message}` }; }
  await page.route('**://lh3.googleusercontent.com/**', (r) => r.fulfill({ contentType: 'image/png', body: PIXEL }));
  await page.route('http://design.local/**', (r) => r.fulfill({ contentType: 'text/html', body: html }));
  try {
    await page.goto('http://design.local/', { waitUntil: 'domcontentloaded', timeout: 30000 });
    await page.waitForTimeout(3000); // 等 Tailwind CDN 编译
  } catch (e) { return { page, error: `设计稿 goto 失败：${e.message}` }; }
  return { page, error: null };
}

// ── 本地页面加载（登录 + 关弹窗） ────────────────────────────────────────────
async function loadLocal(browser) {
  const page = await browser.newPage({ viewport: VIEWPORT, deviceScaleFactor: 2 });
  const notes = [];
  try {
    await page.goto(LOCAL_URL, { waitUntil: 'domcontentloaded', timeout: 30000 });
  } catch (e) { return { page, error: `本地页面 goto 失败（8787 在跑吗？）：${e.message}`, notes }; }

  // 登录门：boot() 先 await siteIsOpen()（401）才 showGate()，#gateAcct 要等一会儿才出现；
  // 最多轮询 10s（1s × 10），出现就填。已登录（siteIsOpen 直接放行）时等不到，跳过。
  try {
    let hasGate = null;
    for (let i = 0; i < 10; i++) {
      hasGate = await page.$('#gateAcct');
      if (hasGate) break;
      await page.waitForTimeout(1000);
    }
    if (hasGate) {
      /* fill 前先等输入框值稳定：showGate 之后 30ms 有一次 focus()，赶在它前面 fill
         会撞上重渲染，账号被填两遍（1380000000013800000000 → 「请输入账号和密码」）。
         这里显式清空再填，填完读回校验，不对就重试一次；再不对就 goto 重来一次。 */
      const fillOnce = async () => {
        await page.evaluate(() => {
          const a = document.getElementById('gateAcct'), p = document.getElementById('gatePass');
          if (a) { a.value = ''; a.dispatchEvent(new Event('input', { bubbles: true })); }
          if (p) { p.value = ''; p.dispatchEvent(new Event('input', { bubbles: true })); }
        });
        await page.fill('#gateAcct', GATE_ACCT);
        await page.fill('#gatePass', GATE_PASS);
        return page.evaluate(() => ({
          acct: document.getElementById('gateAcct')?.value || '',
          pass: document.getElementById('gatePass')?.value || '',
        }));
      };
      let vals = await fillOnce();
      if (vals.acct !== GATE_ACCT || vals.pass !== GATE_PASS) {
        await page.waitForTimeout(600);   // 等 focus()/重渲染那一下过去
        vals = await fillOnce();
      }
      if (vals.acct !== GATE_ACCT || vals.pass !== GATE_PASS) {
        throw new Error(`输入框校验失败 acct="${vals.acct}" pass="${vals.pass}"`);
      }
      await page.click('#gateGo');
      notes.push('已提交登录门');
    } else {
      notes.push('未发现 #gateAcct（可能已登录或页面结构变了）');
    }
  } catch (e) { notes.push(`登录门异常（继续尝试）：${e.message}`); }

  /* 登录后 bootRest → loadData → renderList 是异步链：等榜单行出现（或 12s 兜底），
     否则拿指标时 #view 还是空、全部 rect=0（2026-10-18 踩过：登录提交后只等 2s，
     body 只有 219px 高、领奖台/列表整块「本地没有」）。 */
  try {
    for (let i = 0; i < 12; i++) {
      const n = await page.evaluate(() => document.querySelectorAll('.st-row, .lb-row').length);
      if (n > 0) { notes.push(`榜单行已渲染（${n} 行）`); break; }
      await page.waitForTimeout(1000);
    }
  } catch (_) {}

  await page.waitForTimeout(2000);

  // 公告/奖励弹窗：点空白翻页直到带 hidden
  try {
    for (let i = 0; i < 6; i++) {
      const st = await page.evaluate(() => {
        const b = document.getElementById('itemPopup');
        if (!b) return 'absent';
        return b.classList.contains('hidden') ? 'hidden' : 'open';
      });
      if (st !== 'open') { notes.push(`itemPopup: ${st}`); break; }
      await page.mouse.click(195, 30);
      await page.waitForTimeout(500);
      notes.push(`itemPopup: 第 ${i + 1} 次点击空白`);
    }
  } catch (e) { notes.push(`弹窗处理异常（继续）：${e.message}`); }

  // 确保落在看板（data）视图
  try {
    const v = await page.evaluate(() => {
      const b = document.querySelector('#navRow button.on, .tabbar button.on');
      return b ? (b.dataset.v || b.id || '?') : null;
    });
    if (v && v !== 'data') {
      await page.click('#navRow button[data-v="data"], .tabbar button[data-v="data"]');
      notes.push(`已从 ${v} 切回看板`);
    } else { notes.push(`当前视图: ${v || '未知'}`); }
  } catch (e) { notes.push(`视图确认异常（继续）：${e.message}`); }

  await page.waitForTimeout(1500); // 等动画/字体稳定
  return { page, error: null, notes };
}

// ── diff ─────────────────────────────────────────────────────────────────────
const NUMERIC = /^-?\d+(\.\d+)?(px|rem|em|%|deg|)$/;

function numOf(v) {
  if (v == null) return null;
  const s = String(v).trim();
  if (!NUMERIC.test(s)) return null;
  const n = parseFloat(s);
  return Number.isFinite(n) ? n : null;
}

// 数值型字段允许 0.6px 误差（亚像素/DPR 舍入），其余按字符串严格比
// Tailwind 的 ring/shadow reset 会在 computed box-shadow 前面拼全零段
// （如「rgba(0, 0, 0, 0) 0px 0px 0px 0px」「rgb(255, 255, 255) 0px 0px 0px 0px」）——
// 偏移/模糊/扩散全 0 = 渲染上等于没有。比较前把这类段剥掉，两边才是同口径。
function stripZeroShadows(v) {
  if (v == null) return v;
  const parts = String(v).split(/,(?![^(]*\))/); // 按逗号切，括号里的逗号不算
  const kept = parts.map((s) => s.trim()).filter((s) => {
    const m = s.match(/^rgba?\([^)]*\)\s+([\d.]+)px\s+([\d.]+)px\s+([\d.]+)px\s+([\d.]+)px$/i);
    if (!m) return true;                       // 认不出的格式原样保留
    const [, x, y, blur, spread] = m;
    return !(x === '0' && y === '0' && blur === '0' && spread === '0');  // 全零 = 无效段，剥
  });
  return kept.join(', ');
}

function compare(a, b, prop) {
  if (a == null || b == null) return null;
  if (prop === 'box-shadow') {
    a = stripZeroShadows(a); b = stripZeroShadows(b);
  }
  const na = numOf(a), nb = numOf(b);
  if (na != null && nb != null) {
    const unitA = String(a).replace(/^-?\d+(\.\d+)?/, '');
    const unitB = String(b).replace(/^-?\d+(\.\d+)?/, '');
    if (unitA === unitB) return Math.abs(na - nb) <= 0.6 ? 'same' : 'diff';
  }
  return String(a) === String(b) ? 'same' : 'diff';
}

function fmt(v) {
  if (v == null) return '—';
  const s = String(v);
  return s.length > 58 ? s.slice(0, 55) + '…' : s;
}

function escCell(s) { return String(s).replace(/\|/g, '\\|').replace(/\n/g, ' '); }

// ── main ─────────────────────────────────────────────────────────────────────
let browser = null;
const result = { design: {}, local: {}, rows: [], missing: [], meta: {} };

try {
  browser = await chromium.launch({ channel: 'chrome' });

  const d = await loadDesign(browser);
  const l = await loadLocal(browser);
  result.meta.designError = d.error;
  result.meta.localError = l.error;
  result.meta.notes = l.notes || [];
  result.meta.viewport = VIEWPORT;
  result.meta.localUrl = LOCAL_URL;

  if (!d.error) {
    try { result.design = await d.page.evaluate(eval(PICK_SRC), probesFor('design')); }
    catch (e) { result.meta.designEvalError = e.message; }
  }
  if (!l.error) {
    try { result.local = await l.page.evaluate(eval(PICK_SRC), probesFor('local')); }
    catch (e) { result.meta.localEvalError = e.message; }
  }

  // 逐字段 diff
  for (const p of PROBES) {
    const dv = result.design[p.name] || null;
    const lv = result.local[p.name] || null;
    if (!dv && !lv) { result.missing.push({ sec: p.sec, name: p.name, side: 'both' }); continue; }
    if (!dv) { result.missing.push({ sec: p.sec, name: p.name, side: 'design' }); continue; }
    if (!lv) { result.missing.push({ sec: p.sec, name: p.name, side: 'local' }); continue; }

    // rect 用数值容差比；命中内容驱动白名单的 diff 降级为 expected
    for (const k of ['x', 'y', 'w', 'h']) {
      const a = dv.rect[k], b = lv.rect[k];
      if (a == null || b == null) continue;
      let st = Math.abs(a - b) <= 1.5 ? 'same' : 'diff';
      const cd = st === 'diff' && CONTENT_DRIVEN.has(`${p.name}.rect.${k}`);
      if (cd) st = 'expected';
      result.rows.push({ sec: p.sec, name: p.name, field: `rect.${k}`, design: a, local: b, status: st, ...(cd ? { contentDriven: true } : {}) });
    }
    const fields = new Set([...(p.props || []), ...Object.keys(dv).filter((k) => k !== 'rect'), ...Object.keys(lv).filter((k) => k !== 'rect')]);
    for (const f of fields) {
      if (f === 'rect') continue;
      const a = dv[f], b = lv[f];
      if (a == null && b == null) continue;
      if (a == null || b == null) { result.missing.push({ sec: p.sec, name: p.name, side: a == null ? 'design' : 'local', field: f }); continue; }
      let st = compare(a, b, f);
      const cd = st === 'diff' && CONTENT_DRIVEN.has(`${p.name}.${f}`);
      if (cd) st = 'expected';
      result.rows.push({ sec: p.sec, name: p.name, field: f, design: a, local: b, status: st, ...(cd ? { contentDriven: true } : {}) });
    }
  }

  // ── 输出 ──
  const diffs = result.rows.filter((r) => r.status === 'diff');
  const expected = result.rows.filter((r) => r.status === 'expected');
  console.log(`\n视口 ${VIEWPORT.width}×${VIEWPORT.height} · 本地 ${LOCAL_URL}`);
  if (d.error) console.log(`[设计稿错误] ${d.error}`);
  if (l.error) console.log(`[本地错误] ${l.error}`);
  for (const n of result.meta.notes) console.log(`[note] ${n}`);

  console.log('\n## 对照表（两边都取到的字段）');
  console.log('区块 | 字段 | 设计稿值 | 本地值 | ✓/✗');
  console.log('--- | --- | --- | --- | ---');
  for (const r of result.rows) {
    if (r.status === 'expected') continue; // 内容驱动差异单独成节，不进主表
    console.log(`${r.sec} | ${escCell(r.name)}.${escCell(r.field)} | ${escCell(fmt(r.design))} | ${escCell(fmt(r.local))} | ${r.status === 'same' ? '✓' : '✗'}`);
  }

  console.log(`\n## 不一致项（${diffs.length}）`);
  if (!diffs.length) console.log('（无）');
  else {
    console.log('区块 | 字段 | 设计稿值 | 本地值');
    console.log('--- | --- | --- | ---');
    for (const r of diffs) {
      console.log(`${r.sec} | ${escCell(r.name)}.${escCell(r.field)} | ${escCell(fmt(r.design))} | ${escCell(fmt(r.local))}`);
    }
  }

  console.log(`\n## 内容驱动差异（数据不同导致，非样式问题）`);
  if (!expected.length) console.log('（无）');
  else {
    console.log('区块 | 字段 | 设计稿值 | 本地值 | 状态');
    console.log('--- | --- | --- | --- | ---');
    for (const r of expected) {
      console.log(`${r.sec} | ${escCell(r.name)}.${escCell(r.field)} | ${escCell(fmt(r.design))} | ${escCell(fmt(r.local))} | ≈ (内容)`);
    }
  }

  console.log(`\n## 缺失（${result.missing.length}）`);
  if (!result.missing.length) console.log('（无）');
  else {
    console.log('区块 | 元素 | 缺的一侧 | 字段');
    console.log('--- | --- | --- | ---');
    for (const m of result.missing) {
      console.log(`${m.sec} | ${escCell(m.name)} | ${m.side === 'both' ? '两边都没有' : m.side === 'design' ? '设计稿没有' : '本地没有'} | ${escCell(m.field || '')}`);
    }
  }

  fs.writeFileSync(OUT_JSON, JSON.stringify({
    meta: result.meta,
    design: result.design,
    local: result.local,
    rows: result.rows, // expected 行保留（status='expected'，带 contentDriven:true），结构兼容
    missing: result.missing,
    diffCount: diffs.length,
    expectedCount: expected.length,
  }, null, 2));
  console.log(`\n完整 JSON → ${path.relative(REPO, OUT_JSON)}`);
  console.log(`统计：比对 ${result.rows.length} 字段，不一致 ${diffs.length}，内容驱动（预期不同） ${expected.length}，缺失 ${result.missing.length}`);
} catch (e) {
  console.error('\n[脚本异常]', e && e.stack ? e.stack : e);
  console.log('\n已取到的部分：');
  console.log('  设计稿:', Object.keys(result.design || {}).length, '个探针');
  console.log('  本地  :', Object.keys(result.local || {}).length, '个探针');
  try {
    fs.writeFileSync(OUT_JSON, JSON.stringify({ meta: result.meta, design: result.design, local: result.local, rows: result.rows, missing: result.missing, error: String(e && e.message) }, null, 2));
    console.log('  （部分结果已写入', OUT_JSON, '）');
  } catch (_) {}
} finally {
  if (browser) { try { await browser.close(); } catch (_) {} }
}

process.exit(0); // 诊断工具，永远 0
