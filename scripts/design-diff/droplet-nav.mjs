#!/usr/bin/env node
/**
 * 底栏「液态水滴 + 边缘色散」设计稿 ↔ 本地页面对照 + 拖拽自检
 * ─────────────────────────────────────────────────────────────────────────────
 * 设计源：Stitch 项目 9879920671738170074 / 屏幕 2f32c73c9eb745c99ad617e812c6189f
 *         （标题「主播数据榜-水滴仅边缘色散质感导航栏」）
 * 本地落盘：.stitch/designs/stitch_droplet_nav/code.html（gitignore，需单独下载）
 *
 * 用途（2026-10-19 第四轮底栏重做）：
 *   1. 静态 1:1 数值对照：在 390×844 视口下分别加载「设计稿 HTML」与「本地页面」，
 *      量 dock / 水滴宿主 / 水体 / 色散环 / 内壁层 / 图标文字 的计算样式与几何，
 *      逐字段 diff（亚像素 0.6px 容差），输出 Markdown 表 + JSON。
 *   2. 拖拽自检：本地页面用真实鼠标手势「长按 → 拖拽 → 松手」，抓拖拽中/贴左壁/
 *      松手后三态截图，并断言：一格宽、内部零彩光、色散只在边缘、不再压暗原选中项、
 *      旧 #navPill/#navLens 不存在、松手磁吸并切页。
 *
 * 运行：node scripts/design-diff/droplet-nav.mjs [--strict]
 *   --strict  有关键差异/自检失败时 exit 1（默认 exit 0，只诊断）
 *   环境变量：DROPLET_URL（默认 http://127.0.0.1:8787/st/m）
 *             DROPLET_ACCT / DROPLET_PASS（默认读 rust/.env 的 ADMIN_PASSWORD）
 *
 * 环境坑：
 *  1. Playwright 只能按绝对路径 import（本机只装了 @playwright/mcp 里那份）；
 *     且必须 chromium.launch({ channel: 'chrome' }) —— 本机没有 playwright 自带 chromium。
 *  2. 设计稿是 Tailwind CDN 版，file:// 打开拿不到外部 CSS；用路由拦截喂到 http://design.local。
 *  3. 设计稿外网头像拉不到，统一拦成 1×1 PNG，避免 goto 超时。
 *  4. 本地页面有登录门（#gateAcct/#gatePass/#gateGo）+ 登录后公告弹层 #itemPopup，
 *     都要处理；填表要清空重填并读回校验（showGate 的 focus() 会和 fill 抢）。
 *  5. 本地页面 HTML 由 Rust include_str! 内嵌 —— 改完 st_page.html 必须 cargo build
 *     并重启 8787 才生效；本脚本不负责编译/重启。
 *  6. 量 idle 态前先把底栏的 CSS 动画暂停到 currentTime=0（呼吸动画会改 border-radius），
 *     否则两边相位不同，圆角字段天然对不上。
 */
import { chromium } from '/opt/homebrew/lib/node_modules/@playwright/mcp/node_modules/playwright/index.mjs';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(__dirname, '..', '..');
const DESIGN_HTML = path.join(REPO, '.stitch', 'designs', 'stitch_droplet_nav', 'code.html');
const SHOT_DIR = path.join(__dirname, 'droplet-nav');
const OUT_JSON = path.join(__dirname, 'last-droplet-nav.json');
const OUT_MD = path.join(__dirname, 'last-droplet-nav.md');
const URL_LOCAL = process.env.DROPLET_URL || 'http://127.0.0.1:8787/st/m';
const STRICT = process.argv.includes('--strict');
const VIEWPORT = { width: 390, height: 844 };

function readEnvPassword() {
  if (process.env.DROPLET_PASS) return process.env.DROPLET_PASS;
  try {
    const env = fs.readFileSync(path.join(REPO, 'rust', '.env'), 'utf8');
    const m = env.match(/^ADMIN_PASSWORD=(.*)$/m);
    if (m && m[1].trim()) return m[1].trim();
  } catch (_) {}
  return process.env.DROPLET_ACCT || 'shuai4444';
}
const ACCT = process.env.DROPLET_ACCT || readEnvPassword();
const PASS = readEnvPassword();

/* 1×1 透明 PNG：顶掉设计稿里拉不到的 googleusercontent 头像 */
const PIXEL = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
  'base64'
);

/* ── 选择器表：同一个逻辑元素在设计稿 / 本地页面的选择器 ─────────────────────── */
const SELECTORS = {
  dock:      { design: '#capsule-bottom-dock',  local: '#navRow' },
  droplet:   { design: '#liquid-water-droplet', local: '#navDrop' },
  body:      { design: '#droplet-body-el',      local: '#navDropBody' },
  ring:      { design: '#droplet-chromatic-el', local: '#navDropRing' },
  wallL:     { design: '.dock-wall-dispersion-left',  local: '#navWallL' },
  wallR:     { design: '.dock-wall-dispersion-right', local: '#navWallR' },
  tab:       { design: '.dock-tab-btn',         local: '#navRow button[data-v]' },
  focusLabel:{ design: '.dock-tab-btn.is-droplet-focused .tab-label', local: '#navRow button.is-droplet-focused .tab-label' },
  focusIcon: { design: '.dock-tab-btn.is-droplet-focused .tab-icon',  local: '#navRow button.is-droplet-focused .ic' },
  idleLabel: { design: '.dock-tab-btn .tab-label.font-medium',        local: '#navRow button:not(.on) .tab-label' },
};

/* 每个逻辑元素要取的 CSS 属性（顺序就是 Markdown 表的行序） */
const PROPS = {
  dock: ['width', 'height', 'padding', 'border-top-width', 'border-radius', 'background-color', 'box-shadow', 'backdrop-filter'],
  droplet: ['top', 'bottom', 'left', 'width', 'height', 'border-radius', 'transform', 'transition'],
  body: ['background-color', 'background-image', 'border-top-width', 'border-top-left-radius', 'box-shadow', 'backdrop-filter'],
  ring: ['background-image', 'opacity', 'padding', 'inset', 'box-shadow', 'filter', 'mask-composite', '-webkit-mask-composite', 'background-position'],
  wallL: ['width', 'opacity', 'background-image', 'filter'],
  wallR: ['width', 'opacity', 'background-image', 'filter'],
  tab: ['width', 'height', 'padding-top', 'padding-bottom', 'flex-grow', 'flex-basis', 'font-size'],
  focusLabel: ['font-size', 'font-weight', 'color', 'letter-spacing'],
  focusIcon: ['color', 'transform', 'filter'],
  idleLabel: ['font-size', 'font-weight', 'color'],
};

/* 两边布局骨架必须一致的几何量（相对 dock 的偏移，避免页面高度差异影响） */
const RECT_PROPS = ['droplet', 'body', 'ring', 'tab', 'focusLabel', 'focusIcon'];

async function extract(page, side) {
  const spec = {};
  for (const [key, sel] of Object.entries(SELECTORS)) spec[key] = { sel: sel[side], props: PROPS[key] };
  const raw = await page.evaluate((specIn) => {
    const pick = (sel, props) => {
      const el = document.querySelector(sel);
      if (!el) return null;
      const cs = getComputedStyle(el);
      const o = {};
      for (const p of props) {
        let v = '';
        try { v = cs.getPropertyValue(p); } catch (_) {}
        if (v !== '') o[p] = v;
      }
      const r = el.getBoundingClientRect();
      o.rect = { x: +r.x.toFixed(1), y: +r.y.toFixed(1), w: +r.width.toFixed(1), h: +r.height.toFixed(1) };
      return o;
    };
    const out = {};
    for (const [key, info] of Object.entries(specIn)) out[key] = pick(info.sel, info.props);
    return out;
  }, spec);
  const dockRect = raw.dock?.rect || { x: 0, y: 0 };
  for (const key of RECT_PROPS) {
    const r = raw[key]?.rect;
    if (!r) continue;
    raw[key].rel = { dx: +(r.x - dockRect.x).toFixed(1), dy: +(r.y - dockRect.y).toFixed(1), w: r.w, h: r.h };
    delete raw[key].rect;
  }
  return raw;
}

/* idle 态把底栏动画冻结在 currentTime=0：呼吸动画会改 border-radius/transform */
async function freezeNavAnimations(page, side) {
  const root = side === 'design' ? '#capsule-bottom-dock' : '#navRow';
  await page.evaluate((sel) => {
    const el = document.querySelector(sel);
    if (!el || typeof el.getAnimations !== 'function') return;
    el.getAnimations({ subtree: true }).forEach((a) => {
      try { a.pause(); a.currentTime = 0; } catch (_) {}
    });
  }, root);
}

/* ── 数值 diff ───────────────────────────────────────────────────────────────── */
const NUMERIC = /^-?\d+(\.\d+)?(px|rem|em|%|deg|s|ms)?$/;
const EPS = 0.6;
function num(v) {
  if (v == null) return null;
  const s = String(v).trim();
  if (!NUMERIC.test(s)) return null;
  return parseFloat(s);
}
function near(a, b) { return a != null && b != null && Math.abs(a - b) <= EPS; }
function normStr(s) { return String(s || '').replace(/\s+/g, ' ').trim(); }

/* Tailwind 的 ring/shadow reset 会在 computed box-shadow 前面拼全零段
   （rgba(0,0,0,0) 0 0 0 0 = 渲染上等于没有）；比较前剥掉，两边才是同口径。 */
function stripZeroShadows(v) {
  if (v == null) return v;
  const parts = String(v).split(/,(?![^(]*\))/);
  const kept = parts.map((s) => s.trim()).filter((s) => {
    const m = s.match(/^rgba?\([^)]*\)\s+([\d.]+)px\s+([\d.]+)px\s+([\d.]+)px\s+([\d.]+)px$/i);
    if (!m) return true;
    const x = m[1], y = m[2], blur = m[3], spread = m[4];
    return !(x === '0' && y === '0' && blur === '0' && spread === '0');
  });
  return kept.join(', ');
}

function compare(design, local) {
  const rows = [];
  for (const key of Object.keys(SELECTORS)) {
    const d = design[key], l = local[key];
    if (!d || !l) {
      rows.push({ key, prop: '<元素>', design: d ? '存在' : '缺失', local: l ? '存在' : '缺失', status: d && l ? 'ok' : 'missing' });
      continue;
    }
    const props = new Set([...Object.keys(d), ...Object.keys(l)]);
    props.delete('rect');
    props.delete('rel');
    for (const p of props) {
      const dv = d[p], lv = l[p];
      if (dv === undefined || lv === undefined) {
        rows.push({ key, prop: p, design: dv ?? '—', local: lv ?? '—', status: 'missing' });
        continue;
      }
      const dvn = p === 'box-shadow' ? stripZeroShadows(dv) : dv;
      const lvn = p === 'box-shadow' ? stripZeroShadows(lv) : lv;
      const dn = num(dvn), ln = num(lvn);
      let status;
      if (dn != null && ln != null) status = near(dn, ln) ? 'ok' : 'diff';
      else status = normStr(dvn) === normStr(lvn) ? 'ok' : 'diff';
      rows.push({ key, prop: p, design: dvn, local: lvn, status });
    }
    if (d.rel && l.rel) {
      for (const p of ['dx', 'dy', 'w', 'h']) {
        const dv = d.rel[p], lv = l.rel[p];
        rows.push({ key, prop: 'rel.' + p, design: dv, local: lv, status: near(dv, lv) ? 'ok' : 'diff' });
      }
    }
  }
  return rows;
}

/* ── 加载 ────────────────────────────────────────────────────────────────────── */
async function loadDesign(browser) {
  if (!fs.existsSync(DESIGN_HTML)) throw new Error('缺少设计稿 ' + DESIGN_HTML + '（先按 Stitch 屏幕 ID 下载）');
  const html = fs.readFileSync(DESIGN_HTML, 'utf8');
  const page = await browser.newPage({ viewport: VIEWPORT, deviceScaleFactor: 2 });
  await page.route('**://lh3.googleusercontent.com/**', (r) => r.fulfill({ contentType: 'image/png', body: PIXEL }));
  await page.route('http://design.local/**', (r) => r.fulfill({ contentType: 'text/html', body: html }));
  await page.goto('http://design.local/', { waitUntil: 'domcontentloaded', timeout: 30000 });
  await page.waitForTimeout(3200);
  return page;
}

async function loadLocal(browser) {
  const page = await browser.newPage({ viewport: VIEWPORT, deviceScaleFactor: 2, isMobile: true, hasTouch: true });
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.goto(URL_LOCAL, { waitUntil: 'domcontentloaded', timeout: 30000 });
  let gate = null;
  for (let i = 0; i < 10; i++) {
    gate = await page.$('#gateAcct');
    if (gate) break;
    await page.waitForTimeout(1000);
  }
  if (gate) {
    const fillOnce = async () => {
      await page.evaluate(() => {
        for (const id of ['gateAcct', 'gatePass']) {
          const el = document.getElementById(id);
          if (el) { el.value = ''; el.dispatchEvent(new Event('input', { bubbles: true })); }
        }
      });
      await page.fill('#gateAcct', ACCT);
      await page.fill('#gatePass', PASS);
      return page.evaluate(() => ({
        acct: document.getElementById('gateAcct')?.value || '',
        pass: document.getElementById('gatePass')?.value || '',
      }));
    };
    let vals = await fillOnce();
    if (vals.acct !== ACCT || vals.pass !== PASS) { await page.waitForTimeout(600); vals = await fillOnce(); }
    if (vals.acct !== ACCT || vals.pass !== PASS) throw new Error('登录框填不进去 ' + JSON.stringify(vals));
    await page.click('#gateGo');
    await page.waitForFunction(() => document.getElementById('gate').classList.contains('hidden'), null, { timeout: 15000 });
  }
  for (let i = 0; i < 15; i++) {
    const n = await page.evaluate(() => document.querySelectorAll('.st-row').length).catch(() => 0);
    if (n > 0) break;
    await page.waitForTimeout(1000);
  }
  for (let i = 0; i < 8; i++) {
    const open = await page.evaluate(() => {
      const b = document.getElementById('itemPopup');
      return b && !b.classList.contains('hidden');
    }).catch(() => false);
    if (!open) break;
    await page.mouse.click(195, 30);
    await page.waitForTimeout(400);
  }
  await page.waitForTimeout(1200);
  return { page, errors };
}

/* ── 截图（裁底栏一条） ───────────────────────────────────────────────────────── */
async function navShot(page, file) {
  const dock = await page.evaluate(() => {
    const el = document.querySelector('#navRow, #capsule-bottom-dock');
    if (!el) return null;
    const r = el.getBoundingClientRect();
    return { x: r.x, y: r.y, w: r.width, h: r.height };
  });
  if (!dock) return;
  await page.screenshot({
    path: file,
    clip: {
      x: Math.max(0, Math.round(dock.x - 12)),
      y: Math.max(0, Math.round(dock.y - 14)),
      width: Math.min(VIEWPORT.width, Math.round(dock.w + 24)),
      height: Math.round(dock.h + 28),
    },
  });
}

/* ── 主流程 ──────────────────────────────────────────────────────────────────── */
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const result = { strict: STRICT, when: new Date().toISOString(), design: DESIGN_HTML, local: URL_LOCAL, rows: [], checks: [], errors: [] };
let failed = 0;
const check = (name, ok, detail) => {
  result.checks.push({ name, ok: !!ok, detail });
  if (!ok) failed++;
  console.log('  ' + (ok ? 'ok  ' : 'FAIL') + ' ' + name + (detail ? '  — ' + detail : ''));
};
const mkdir = () => { fs.rmSync(SHOT_DIR, { recursive: true, force: true }); fs.mkdirSync(SHOT_DIR, { recursive: true }); };

try {
  mkdir();
  const designPage = await loadDesign(browser);
  const { page: localPage, errors } = await loadLocal(browser);

  /* 1) 静态 idle 对照 */
  await freezeNavAnimations(designPage, 'design');
  await freezeNavAnimations(localPage, 'local');
  await designPage.waitForTimeout(200);
  await localPage.waitForTimeout(200);
  const d = await extract(designPage, 'design');
  const l = await extract(localPage, 'local');
  result.designMetrics = d;
  result.localMetrics = l;
  result.rows = compare(d, l);
  await navShot(designPage, path.join(SHOT_DIR, 'design-idle.png'));
  await navShot(localPage, path.join(SHOT_DIR, 'local-idle.png'));

  const mismatch = result.rows.filter((r) => r.status === 'diff');
  const missing = result.rows.filter((r) => r.status === 'missing');
  check('静态对照无缺失元素', missing.length === 0, missing.map((r) => r.key).join(',') || '全部命中');
  check('静态对照关键字段一致（0 不一致）', mismatch.length === 0,
    mismatch.length ? mismatch.slice(0, 12).map((r) => r.key + '.' + r.prop + ' 设计=' + r.design + ' 本地=' + r.local).join(' | ') : '0 不一致');

  /* 2) 本地拖拽自检 */
  const tabs = await localPage.evaluate(() => [...document.querySelectorAll('#navRow button[data-v]')]
    .filter((b) => b.offsetParent !== null)
    .map((b) => {
      const r = b.getBoundingClientRect();
      return { v: b.dataset.v, on: b.classList.contains('on'), cx: r.left + r.width / 2, cy: r.top + r.height / 2, w: r.width };
    }));
  check('本地底栏可见格 ≥2', tabs.length >= 2, tabs.map((t) => t.v + (t.on ? '*' : '')).join(' '));
  const cur = tabs.findIndex((t) => t.on);
  const from = tabs[cur], to = tabs[cur === 0 ? 1 : 0];
  await localPage.mouse.move(from.cx, from.cy);
  await localPage.mouse.down();
  await localPage.waitForTimeout(320);
  const steps = 10;
  for (let s = 1; s <= steps; s++) {
    await localPage.mouse.move(from.cx + (to.cx - from.cx) * (s / steps), from.cy);
    await localPage.waitForTimeout(24);
  }
  await localPage.waitForTimeout(40);
  const drag = await localPage.evaluate(() => {
    const dock = document.getElementById('navRow');
    const drop = document.getElementById('navDrop');
    const body = document.getElementById('navDropBody');
    const ring = document.getElementById('navDropRing');
    const tabs = [...document.querySelectorAll('#navRow button[data-v]')].filter((b) => b.offsetParent !== null);
    const r = drop.getBoundingClientRect();
    const csBody = getComputedStyle(body), csRing = getComputedStyle(ring);
    const onTab = tabs.find((b) => b.classList.contains('on'));
    return {
      dockDragging: dock.classList.contains('is-droplet-dragging'),
      dropW: r.width,
      dropH: r.height,
      tabW: tabs[0].getBoundingClientRect().width,
      bodyBg: csBody.backgroundColor,
      bodyBgImg: csBody.backgroundImage,
      ringBg: csRing.backgroundImage,
      ringMask: csRing.maskComposite || csRing.webkitMaskComposite,
      focused: tabs.filter((b) => b.classList.contains('is-droplet-focused')).map((b) => b.dataset.v),
      edge: tabs.filter((b) => b.classList.contains('is-droplet-edge')).map((b) => b.dataset.v),
      activeOpacity: onTab && onTab.querySelector('.ic') ? getComputedStyle(onTab.querySelector('.ic')).opacity : null,
      oldNodes: !!(document.getElementById('navPill') || document.getElementById('navLens')),
      oldClasses: tabs.some((b) => b.classList.contains('is-covered') || b.classList.contains('is-dim')),
    };
  });
  await navShot(localPage, path.join(SHOT_DIR, 'local-drag.png'));
  check('拖拽中 dock 进入 is-droplet-dragging', drag.dockDragging);
  check('水滴宽度 = 一格（不再 1.5 格大光晕）', Math.abs(drag.dropW - drag.tabW) <= 1.2, '水滴 ' + drag.dropW.toFixed(1) + ' / 格 ' + drag.tabW.toFixed(1));
  check('水滴内部零彩光（水体无渐变、白 .72）', drag.bodyBgImg === 'none' && /rgba\(255, 255, 255, 0\.72\)/.test(drag.bodyBg), drag.bodyBg + ' / ' + drag.bodyBgImg);
  check('色散只在外沿（conic-gradient + mask-composite:exclude）', /conic-gradient/.test(drag.ringBg) && /exclude/.test(drag.ringMask), (drag.ringMask || '') + ' / ' + String(drag.ringBg).slice(0, 60) + '…');
  check('旧 #navPill/#navLens 已不存在', !drag.oldNodes);
  check('旧 .is-covered/.is-dim 已不存在', !drag.oldClasses);
  check('拖拽中恰有一格 focused（跟手焦点）', drag.focused.length === 1, 'focused=' + drag.focused.join(',') + ' edge=' + drag.edge.join(','));
  check('原选中项不再压暗（opacity=1）', drag.activeOpacity === '1', 'opacity=' + drag.activeOpacity);

  await localPage.mouse.move(tabs[0].cx - 160, from.cy);
  await localPage.waitForTimeout(80);
  const wall = await localPage.evaluate(() => getComputedStyle(document.getElementById('navWallL')).opacity);
  await navShot(localPage, path.join(SHOT_DIR, 'local-drag-wall.png'));
  check('拖到最左端触发内壁碰撞色散', parseFloat(wall) > 0.5, 'wallL opacity=' + wall);

  await localPage.mouse.move(to.cx, from.cy);
  await localPage.waitForTimeout(30);
  await localPage.mouse.up();
  await localPage.waitForTimeout(1500);
  const after = await localPage.evaluate(() => {
    const tabs = [...document.querySelectorAll('#navRow button[data-v]')].filter((b) => b.offsetParent !== null);
    const on = tabs.find((b) => b.classList.contains('on'));
    const drop = document.getElementById('navDrop');
    const dr = document.getElementById('navRow').getBoundingClientRect();
    const tr = on ? on.getBoundingClientRect() : null;
    const m = new DOMMatrixReadOnly(getComputedStyle(drop).transform);
    return {
      active: on ? on.dataset.v : '',
      view,
      dropX: m.m41,
      expectX: tr ? tr.left - dr.left - 6 : null,
      dropCls: drop.className,
      focused: tabs.filter((b) => b.classList.contains('is-droplet-focused')).map((b) => b.dataset.v),
    };
  });
  await navShot(localPage, path.join(SHOT_DIR, 'local-after.png'));
  check('松手磁吸并切到目标页', after.active === to.v && after.view === to.v, 'active=' + after.active + ' view=' + after.view + ' 期望=' + to.v);
  check('松手后水滴落回目标格', after.expectX != null && Math.abs(after.dropX - after.expectX) <= 1.5, 'dropX=' + after.dropX + ' 期望=' + after.expectX);
  check('松手后水滴恢复 idle 呼吸态', /is-idle/.test(after.dropCls) && after.focused.length === 1, after.dropCls);
  check('无 JS 运行时错误', errors.length === 0, errors.slice(0, 3).join(' | '));

  /* 3) 报告落盘 */
  const total = result.rows.length;
  const okCount = result.rows.filter((r) => r.status === 'ok').length;
  result.summary = { fields: total, ok: okCount, mismatch: mismatch.length, missing: missing.length, checksFailed: failed };

  const md = [];
  md.push('# 底栏「液态水滴」设计稿 ↔ 本地对照报告');
  md.push('');
  md.push('- 设计稿：' + path.relative(REPO, DESIGN_HTML) + '（Stitch 9879920671738170074 / 2f32c73c…）');
  md.push('- 本地：' + URL_LOCAL);
  md.push('- 字段：' + total + '（一致 ' + okCount + '，不一致 ' + mismatch.length + '，缺失 ' + missing.length + '）；自检 ' + result.checks.length + ' 项（失败 ' + failed + '）');
  md.push('');
  md.push('## 自检');
  md.push('');
  for (const c of result.checks) md.push('- ' + (c.ok ? '✅' : '❌') + ' ' + c.name + (c.detail ? ' — ' + c.detail : ''));
  md.push('');
  md.push('## 数值对照（不一致/缺失）');
  md.push('');
  if (!mismatch.length && !missing.length) md.push('（0 不一致、0 缺失）');
  else {
    md.push('| 元素 | 属性 | 设计稿 | 本地 | 状态 |');
    md.push('|---|---|---|---|---|');
    for (const r of [...mismatch, ...missing]) {
      md.push('| ' + r.key + ' | ' + r.prop + ' | ' + String(r.design).slice(0, 90) + ' | ' + String(r.local).slice(0, 90) + ' | ' + r.status + ' |');
    }
  }
  md.push('');
  fs.writeFileSync(OUT_MD, md.join('\n') + '\n');
  fs.writeFileSync(OUT_JSON, JSON.stringify(result, null, 1));

  console.log('\n字段 ' + total + '：一致 ' + okCount + ' · 不一致 ' + mismatch.length + ' · 缺失 ' + missing.length + '；自检失败 ' + failed);
  console.log('截图目录：' + path.relative(REPO, SHOT_DIR));
  console.log('报告：' + path.relative(REPO, OUT_MD) + ' / ' + path.relative(REPO, OUT_JSON));

  if (STRICT && (failed > 0 || mismatch.length > 0 || missing.length > 0)) process.exitCode = 1;
} catch (e) {
  console.error('运行失败：', (e && e.stack) || e);
  result.errors.push(String((e && e.stack) || e));
  fs.writeFileSync(OUT_JSON, JSON.stringify(result, null, 1));
  process.exitCode = STRICT ? 1 : 0;
} finally {
  await browser.close();
}
