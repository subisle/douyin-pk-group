/**
 * 注册自检：登录门「注册」→ WorkBuddy 网关授权链接 → 填姓名/手机号/验证码 → 注册
 * → 管理员审核绑定 → 手机号回登录看到个人数据。
 *
 * 覆盖口径：手机号 = 账号、短信验证码数字 = 密码；注册时必须填主播姓名，且姓名必须能在
 * 主播台账里查到（姓名框失焦实时校验）；注册同时调 WorkBuddy 用量看板（wb-proxy）的
 * 「添加账号 (OAuth)」。
 * **2026-10-12 审计起改为审核制**：注册不再立即绑定（bound=0 + 自动生成 pending 绑定申请），
 * 管理员审核通过才真正绑上；通过后旧 token 立即吊销 → 必须重新登录。
 * 本脚本按这个现行口径断言整条链路；绑定后用户自己不能解绑（顺带验证「二次绑定被拒」）。
 *
 * 用法：
 *   node scripts/check/uicheck-register.mjs [BASE] [管理员口令]
 * 默认 BASE=http://127.0.0.1:8787。**只对测试/本机环境跑**：会真写一行
 * my_accounts + 一条绑定申请（随机手机号），跑完用管理员接口删掉账号；没给口令就留一行并提示。
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

/** 用管理员接口挑一个「还没开过号」的男团主播（台账 hasAccount=false）。
    不能只看 my_accounts 的名字：审核制下注册账号名先是手机号占位，绑完才改成主播名。 */
async function pickFreeAnchor() {
  if (!ADMIN_PASSWORD) return ANCHOR_CANDIDATES[0];
  try {
    const login = await fetch(`${BASE}/api/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ password: ADMIN_PASSWORD }),
    }).then((r) => r.json());
    if (!login.token) return ANCHOR_CANDIDATES[0];
    const list = await fetch(`${BASE}/api/admin/streamers?team=male`, {
      headers: { Authorization: `Bearer ${login.token}` },
    }).then((r) => r.json());
    const free = (list.items || []).filter((s) => !s.hasAccount && s.status !== 'left');
    const hit = free.find((s) => ANCHOR_CANDIDATES.includes(s.name)) || free[0];
    if (hit) return hit.name;
    console.log('   台账里已经没有「没开过号」的男团主播，退回首個候选名（可能因此失败）');
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
  const picked = await waitFor(ws, `!!document.getElementById('pickM')`, 4, 250);
  if (picked) bad('展示端还出现男女团选择卡（应直接登录）');
  else {
    const direct = await waitFor(ws, `!document.getElementById('gate').classList.contains('hidden')`, 10, 250);
    direct ? ok('展示端没有团别选择卡，直接进登录门')
        : bad('登录门没弹出来');
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

    const registered = await evaluate(ws, `(localStorage.getItem('myToken_any') || '') !== ''`);
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
    oauthBox ? ok('获取验证码步骤已显示（服务端配了面板密码）') : bad('获取验证码步骤没显示（检查 WORKBUDDY_PANEL_PASSWORD）');
    const noRealm = await evaluate(ws, `!document.getElementById('regRealm')`);
    const noSkip = await evaluate(ws, `!document.getElementById('regSkip')`);
    noRealm ? ok('不显示国内外选择，默认国内版') : bad('仍显示国内外选择');
    noSkip ? ok('已去除跳过授权按钮') : bad('仍显示跳过授权按钮');
    console.log('  ->  ' + (await shot(ws, '01-register-tab.png')));

    /* 3. 进入注册页会自动获取国内版授权链接；这里验证链接并重新获取一次 */
    const gotLink = await waitFor(ws, `(document.querySelector('#regLinkBox a')?.href ?? '').indexOf('copilot.tencent.com') > 0`, 50, 300);
    const authUrl = await evaluate(ws, `document.querySelector('#regLinkBox a')?.href ?? ''`);
    gotLink ? ok('拿到授权链接 ' + authUrl.slice(0, 58) + '…') : bad('没拿到授权链接：' + authUrl);
    await evaluate(ws, `document.getElementById('regAuth').click()`); // 重新获取：验证可重来
    await waitFor(ws, `(document.querySelector('#regLinkBox a')?.href ?? '').indexOf('copilot.tencent.com') > 0`, 40, 300);
    const authUrl2 = await evaluate(ws, `document.querySelector('#regLinkBox a')?.href ?? ''`);
    authUrl2 && authUrl2 !== authUrl ? ok('「重新获取验证码」换了一条新链接') : bad('重新获取没换链接');
    console.log('  ->  ' + (await shot(ws, '02-oauth-link.png')));

    /* 4. 授权是必经步骤：不提供跳过。这里模拟网关已完成入池，再继续注册。 */
    await evaluate(ws, `(() => {
      regOauthDone = true;
      const step = document.getElementById('regStep1');
      if (step) step.classList.add('done');
    })()`);
    ok('模拟 WorkBuddy 授权完成（默认必须加入账号池）');

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
    expect('本地已存 token', await evaluate(ws, `(sessionStorage.getItem('myToken_any') || '').startsWith('my-')`), 'true');

    /* 5. 审核制（2026-10-12 审计修复）：注册 ≠ 立刻绑定 —— 账号落库 bound=0、
       自动生成一条 pending 绑定申请；前端应出现「未绑定主播」提醒条 + 绑定卡。 */
    const needBind = await evaluate(ws, `!document.getElementById('bindBar').hidden`);
    needBind ? ok('注册后出现「未绑定主播」提醒条（审核制：不再注册即绑定）') : bad('没出现未绑定提醒条');
    await evaluate(ws, `document.getElementById('navMy').click()`);
    const bindCard = await waitFor(ws, `document.body.innerText.includes('绑定主播姓名')`, 40, 300);
    bindCard ? ok('「我的」页给出绑定卡') : bad('「我的」页没有绑定卡');
    console.log('  ->  ' + (await shot(ws, '03-my-needbind.png')));

    /* 6. 管理员审核通过（后台「绑定申请」）→ 绑定生效、旧 token 立即吊销 */
    const adminTok = await fetch(`${BASE}/api/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ password: ADMIN_PASSWORD }),
    }).then((r) => r.json()).then((d) => d.token || '');
    const pending = await fetch(`${BASE}/api/admin/my-bind-requests?status=pending`, {
      headers: { Authorization: `Bearer ${adminTok}` },
    }).then((r) => r.json());
    const req = (pending.items || []).find((x) => x.personName === ANCHOR) || (pending.items || [])[0];
    req ? ok(`管理端能看到待审申请（id=${req.id} · ${req.personName ?? ''}）`) : bad('管理端看不到待审申请');
    if (req) {
      const ap = await fetch(`${BASE}/api/admin/my-bind-requests/${req.id}/approve`, {
        method: 'POST',
        headers: { Authorization: `Bearer ${adminTok}` },
      }).then((r) => r.json());
      ap.ok === true
        ? ok(`审核通过 → 绑定到「${ap.name}」，旧 token 已吊销`)
        : bad('审核通过失败：' + JSON.stringify(ap));
    }

    /* 7. 旧 token 已被吊销：刷新应回登录门；用「手机号 + 验证码」重新登录后能看到自己的数据。
       登录限流每 IP 每分钟 5 次 → 撞上限等一分钟重试一次。 */
    await goto(ws, `${BASE}/`);
    await sleep(1500);
    const backToGate = await waitFor(ws, `!document.getElementById('gate').classList.contains('hidden')`, 40, 300);
    backToGate ? ok('绑定生效后旧登录态失效 → 回登录门') : bad('旧 token 没失效（还停在站内）');
    if (backToGate) {
      for (let i = 0; i < 2; i++) {
        await evaluate(ws, `(() => {
          document.getElementById('gateAcct').value = ${JSON.stringify(PHONE)};
          document.getElementById('gatePass').value = ${JSON.stringify(CODE)};
          document.getElementById('gateGo').click();
          return true;
        })()`);
        const logged = await waitFor(ws, `document.getElementById('gate').classList.contains('hidden')`, 30, 300);
        if (logged) { ok('手机号 + 验证码重新登录成功'); break; }
        const err = await evaluate(ws, `document.getElementById('gateErr')?.textContent ?? ''`);
        if (err.includes('频繁') && i === 0) {
          console.log('  ..   登录限流命中（每分钟 5 次），等 62 秒后重试一次');
          await sleep(62000);
          continue;
        }
        bad('手机号 + 验证码登录失败：' + (err || '(无提示)'));
        break;
      }
    }
    /* 绑到主播后团别已定（male）→ 名字记在 myName_male；团别待定时才落 myName_any */
    const nameStored = await evaluate(ws, `localStorage.getItem('myName_male') || localStorage.getItem('myName_any')`);
    expect('水印按主播姓名', nameStored, ANCHOR);
    await evaluate(ws, `document.getElementById('navMy').click()`);
    const myNm = await waitFor(ws, `(document.querySelector('.st-my-head .nm')?.textContent ?? '').trim() === ${JSON.stringify(ANCHOR)}`, 60, 300);
    myNm ? ok('「我的」页显示主播数据（' + ANCHOR + '）') : bad('「我的」页没显示主播数据');
    console.log('  ->  ' + (await shot(ws, '04-my-bound.png')));

    /* 8. 绑定是一次性的：已绑定账号再调 bind 必须被拒（用户口径：绑定后不支持解绑） */
    const again = await fetch(`${BASE}/api/public/my/bind`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ token: await evaluate(ws, `sessionStorage.getItem('myToken_any')`), name: ANCHOR }),
    }).then((r) => r.json());
    String(again.error || '').includes('已绑定')
      ? ok('二次绑定被拒：' + again.error)
      : bad('二次绑定没被拒：' + JSON.stringify(again));

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
