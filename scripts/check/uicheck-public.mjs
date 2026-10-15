/**
 * 展示端（st_page.html）全页面验收：真开 Chrome，把网页上能点到的入口全走一遍。
 *
 * 覆盖：
 *   登录门 → 看板（日/月/年口径 + 搜名字 + 日历挑日期）
 *   → 分组（5 个赛事子页逐个点）→ 族谱 → 我的
 *   → 4 个深链（/report /group/mid /lineage /my）刷新不丢页
 * 并全程盯两件事：
 *   · 网络：登录后 /api/* 不得出现 401 / 5xx（展示端登录态只有一个来源：X-My-Token 头）
 *   · 控制台：不得有 JS 报错
 *
 * 用法：
 *   node scripts/check/uicheck-public.mjs [BASE] [管理员口令]
 * 默认 BASE=http://127.0.0.1:8787。用「账号=密码=管理员口令」的快捷口登录，
 * 不写任何库（只读验收）；要验「我的」里的个人数据，需要一个绑过主播的账号登录。
 *
 * 只用 Node 内置能力（Node 22+ 自带 WebSocket），不需要 puppeteer/playwright。
 */
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { mkdirSync, writeFileSync, rmSync } from 'node:fs'
import { setTimeout as sleep } from 'node:timers/promises'

const BASE = (process.argv[2] ?? 'http://127.0.0.1:8787').replace(/\/$/, '')
const ADMIN_PASSWORD = process.argv[3] ?? ''
const OUT = fileURLToPath(new URL('../.uicheck/public/', import.meta.url))
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
const PORT = 9336
const PROFILE = '/tmp/pk-uicheck-public-profile'

if (!ADMIN_PASSWORD) {
  console.error('用法：node scripts/check/uicheck-public.mjs [BASE] <管理员口令>')
  process.exit(2)
}

rmSync(OUT, { recursive: true, force: true })
mkdirSync(OUT, { recursive: true })
rmSync(PROFILE, { recursive: true, force: true })

const chrome = spawn(CHROME, [
  '--headless=new',
  `--remote-debugging-port=${PORT}`,
  `--user-data-dir=${PROFILE}`,
  '--no-first-run',
  '--no-default-browser-check',
  '--disable-gpu',
  '--hide-scrollbars',
  'about:blank',
], { stdio: 'ignore' })

let failed = 0
const ok = (m) => console.log(`  ok   ${m}`)
const bad = (m) => { console.log(`  FAIL ${m}`); failed++ }
const expect = (m, got, want) =>
  String(got) === String(want) ? ok(`${m} (${got})`) : bad(`${m}：期望 ${want}，实际 ${got}`)
const truthy = (m, got) => (got ? ok(m) : bad(m))
/** 单个小节失败不拖垮整轮验收：把异常记成 FAIL 继续往下跑 */
async function section(title, fn) {
  console.log(`\n== ${title} ==`)
  try {
    await fn()
  } catch (e) {
    bad(`小节异常：${e.message}`)
  }
}

/* ------------------------------ 极简 CDP 客户端 ------------------------------ */

async function cdpTarget() {
  for (let i = 0; i < 40; i++) {
    try {
      const list = await fetch(`http://127.0.0.1:${PORT}/json/list`).then((r) => r.json())
      const page = list.find((t) => t.type === 'page')
      if (page?.webSocketDebuggerUrl) return page.webSocketDebuggerUrl
    } catch { /* Chrome 还没起来 */ }
    await sleep(250)
  }
  throw new Error('连不上 Chrome 的调试端口')
}

function connect(url) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url)
    let id = 0
    const pending = new Map()
    const listeners = []
    ws.onopen = () => resolve({
      send: (method, params = {}) => new Promise((res, rej) => {
        const msgId = ++id
        pending.set(msgId, { res, rej })
        ws.send(JSON.stringify({ id: msgId, method, params }))
      }),
      on: (fn) => listeners.push(fn),
      close: () => ws.close(),
    })
    ws.onerror = reject
    ws.onmessage = (ev) => {
      const msg = JSON.parse(ev.data)
      if (msg.id && pending.has(msg.id)) {
        const { res, rej } = pending.get(msg.id)
        pending.delete(msg.id)
        msg.error ? rej(new Error(msg.error.message)) : res(msg.result)
      } else {
        listeners.forEach((fn) => fn(msg))
      }
    }
  })
}

const errors = []
const apiCalls = []   // { url, status }

async function session(ws) {
  ws.on((msg) => {
    if (msg.method === 'Runtime.exceptionThrown') {
      errors.push(msg.params.exceptionDetails?.exception?.description ?? '未知异常')
    }
    if (msg.method === 'Runtime.consoleAPICalled' && msg.params.type === 'error') {
      errors.push(msg.params.args.map((a) => a.value ?? a.description ?? '').join(' '))
    }
    if (msg.method === 'Page.javascriptDialogOpening') {
      ws.send('Page.handleJavaScriptDialog', { accept: true }).catch(() => {})
    }
    if (msg.method === 'Network.responseReceived') {
      const r = msg.params.response
      if (String(r.url).includes('/api/')) apiCalls.push({ url: r.url, status: r.status })
    }
  })
  await ws.send('Page.enable')
  await ws.send('Runtime.enable')
  await ws.send('Network.enable')
  await ws.send('Emulation.setDeviceMetricsOverride', {
    width: 390, height: 844, deviceScaleFactor: 1, mobile: true,
  })
}

async function evaluate(ws, expr) {
  const r = await ws.send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true })
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? '求值失败')
  return r.result.value
}

async function goto(ws, url) {
  await ws.send('Page.navigate', { url })
  for (let i = 0; i < 60; i++) {
    await sleep(300)
    const ready = await evaluate(ws, `document.readyState === 'complete' && !!document.getElementById('gate')`).catch(() => false)
    if (ready) return
  }
  throw new Error(`页面没挂载：${url}`)
}

async function waitFor(ws, expr, tries = 40, gap = 250) {
  for (let i = 0; i < tries; i++) {
    const hit = await evaluate(ws, expr).catch(() => false)
    if (hit) return true
    await sleep(gap)
  }
  return false
}

async function shot(ws, name) {
  const { data } = await ws.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: true })
  writeFileSync(OUT + name, Buffer.from(data, 'base64'))
  return OUT + name
}

const gateHidden = `document.getElementById('gate').classList.contains('hidden')`
const viewHtml = `document.getElementById('view').innerHTML`
const viewText = `document.getElementById('view').innerText`
const sub = `document.getElementById('mhSub').textContent.trim()`
/** 视图里有没有"坏掉"的信号 */
const BAD_TEXT = ['连不上服务', '取不到个人数据', '加载失败', 'HTTP 5', 'undefined']

async function assertViewHealthy(ws, label) {
  const txt = await evaluate(ws, viewText).catch(() => '')
  const hit = BAD_TEXT.find((b) => txt.includes(b))
  truthy(`${label}：页面无错误文案`, !hit)
  truthy(`${label}：内容已渲染（${txt.trim().length} 字）`, txt.trim().length > 30)
}

/** 点某个导航按钮并等视图切换 */
async function navTo(ws, v) {
  await evaluate(ws, `document.querySelector('[data-v="${v}"]').click()`)
  await sleep(1200)
}

/* ---------------------------------- 开始 ---------------------------------- */

console.log(`== 展示端全页面验收 · ${BASE} ==`)

/* 0. HTTP 层：登录 + 各视图用到的接口都通（这是最快定位"哪个接口坏了"的方式） */
let settings = {}
await section('0. 接口清单（X-My-Token 头）', async () => {
  const login = await fetch(`${BASE}/api/public/my/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ account: ADMIN_PASSWORD, pass: ADMIN_PASSWORD }),
  }).then(async (r) => ({ status: r.status, body: await r.json().catch(() => ({})) }))
  expect('登录接口', login.status, 200)
  const token = login.body.token
  if (!token) { bad('没拿到 token，后续接口清单跳过'); return }
  const H = { 'X-My-Token': token }
  const site = await fetch(`${BASE}/api/public/teams/all`, { headers: H }).then((r) => r.json())
  settings = site.settings || {}
  // 2026-10-14：注册功能已下线，settings 不再下发 myRegister，日志里去掉该项
  console.log(`  --   后台开关：分组=${settings.groupEnabled !== false} 族谱=${settings.lineageEnabled !== false} 登录门=${settings.loginRequired !== false}`)
  const date = (site.dates || [])[0] || ''
  const endpoints = [
    ['站点配置 /all', '/api/public/teams/all'],
    ['方案列表 /plans', '/api/public/plans'],
    ['日榜 /daily', `/api/public/teams/all/daily${date ? '?date=' + date : ''}`],
    ['月榜 /monthly', '/api/public/teams/all/monthly'],
    ['年榜 /yearly', '/api/public/teams/all/yearly'],
    ['我的 /my/who', '/api/public/teams/all/my/who'],
    ['我的 /my', '/api/public/teams/all/my'],
    ['趋势 /my/series', '/api/public/teams/all/my/series?days=14'],
    ['推送 /my/push', '/api/public/teams/all/my/push'],
    ['绑定状态 /my/claim', '/api/public/teams/all/my/claim'],
    ['抖音号 /my/accounts', '/api/public/teams/all/my/accounts'],
  ]
  for (const [label, path] of endpoints) {
    const r = await fetch(BASE + path, { headers: H })
    expect(`${label} → ${r.status}`, r.ok, true)
  }
  /* 族谱接口对「非师门成员」是 403 + code=lineage_forbidden（设计如此，不是坏了）；
     其它任何 4xx/5xx 都算接口坏。 */
  const lin = await fetch(`${BASE}/api/public/teams/all/lineage?period=daily`, { headers: H })
  if (lin.status === 200) ok('族谱 /lineage → 200')
  else {
    const body = await lin.json().catch(() => ({}))
    truthy(`族谱 /lineage → 403 + code=lineage_forbidden（非师门成员，设计如此）`, lin.status === 403 && body.code === 'lineage_forbidden')
  }
})

const target = await cdpTarget()
const ws = await connect(target)

try {
  await session(ws)

  /* 1. 登录门 */
  await section('1. 登录门', async () => {
    await goto(ws, `${BASE}/`)
    truthy('登录门弹出', await waitFor(ws, `!${gateHidden}`))
    await evaluate(ws, `(() => {
      document.getElementById('gateAcct').value = ${JSON.stringify(ADMIN_PASSWORD)};
      document.getElementById('gatePass').value = ${JSON.stringify(ADMIN_PASSWORD)};
      document.getElementById('gateGo').click();
      return true;
    })()`)
    truthy('登录成功（登录门关闭）', await waitFor(ws, gateHidden, 40, 250))
    await sleep(1500)
    truthy('登录后登录门没再弹回来', await evaluate(ws, gateHidden))
    /* 进站前那一次 /all 401 是「还没登录」的正常表现（siteIsOpen 探门）；
       清掉它，第 7 段只盯登录之后的 401。 */
    const preLogin = apiCalls.filter((c) => c.status === 401 && c.url.includes('/api/public/teams/'))
    truthy(`未登录时探门只 401 一次（${preLogin.length}）`, preLogin.length <= 1)
    apiCalls.length = 0
    await shot(ws, '00-after-login.png')
  })

  /* 2. 看板：榜单 + 日/月/年口径 + 搜名字 + 日历 */
  await section('2. 看板（每日报告）', async () => {
    await navTo(ws, 'data')
    truthy('看板有榜单内容', await waitFor(ws, `document.querySelectorAll('#view .st-row, #view .st-pod').length > 0 || ${viewText}.includes('暂未公布')`))
    await assertViewHealthy(ws, '看板')
    await shot(ws, '01-data-daily.png')

    for (const [period, label] of [['monthly', '月榜'], ['yearly', '年榜'], ['daily', '日榜']]) {
      const before = apiCalls.length
      await evaluate(ws, `document.querySelector('#seg button[data-p="${period}"]').click()`)
      const hit = await (async () => {
        for (let i = 0; i < 40; i++) {
          if (apiCalls.slice(before).some((c) => c.url.includes(`/${period === 'daily' ? 'daily' : period}`) && c.status === 200)) return true
          await sleep(200)
        }
        return false
      })()
      truthy(`${label}切换后接口 200`, hit)
      truthy(`${label}切换后仍有榜单内容`, await waitFor(ws, `document.querySelectorAll('#view .st-row, #view .st-pod').length > 0 || ${viewText}.includes('暂未公布')`))
    }
    await shot(ws, '02-data-monthly.png')

    await evaluate(ws, `document.querySelector('#seg button[data-p="daily"]').click()`)
    await sleep(1000)
    const rowsBefore = await evaluate(ws, `document.querySelectorAll('#view .st-row').length`)
    await evaluate(ws, `document.getElementById('btnSearch').click()`)
    truthy('搜索框能打开', await evaluate(ws, `!document.getElementById('searchBox').classList.contains('hidden')`))
    await evaluate(ws, `(() => {
      const q = document.getElementById('q'); q.value = '狼'; q.dispatchEvent(new Event('input', { bubbles: true })); return true;
    })()`)
    await sleep(600)
    const rowsAfter = await evaluate(ws, `document.querySelectorAll('#view .st-row').length`)
    truthy(`搜名字能过滤（${rowsBefore} → ${rowsAfter}）`, rowsAfter < rowsBefore && rowsAfter > 0)
    await evaluate(ws, `document.getElementById('qClear').click()`)
    await sleep(500)
    truthy('清空后恢复全量', (await evaluate(ws, `document.querySelectorAll('#view .st-row').length`)) === rowsBefore)

    await evaluate(ws, `document.getElementById('btnCal').click()`)
    const calOpen = await waitFor(ws, `!!document.getElementById('calPop') && !document.getElementById('calPop').classList.contains('hidden')`)
    truthy('日历弹层能打开', calOpen)
    if (calOpen) {
      const days = await evaluate(ws, `document.querySelectorAll('#calGrid .cal-d.has:not([disabled])').length`)
      truthy(`日历有可选日期（${days} 天）`, days > 0)
      if (days > 0) {
        /* 选一天：日历应关掉且内容按那天重取（抽一个非当前日，避免"点了没反应"假通过） */
        const target = await evaluate(ws, `(() => {
          const b = [...document.querySelectorAll('#calGrid .cal-d.has:not([disabled])')].find((x) => !x.classList.contains('on'))
          if (!b) return ''
          const d = b.dataset.d; b.click(); return d
        })()`)
        truthy(`能点日历换日期（选 ${target || '无其他日期'}）`, !!target)
        truthy('选完日历自动关闭', await waitFor(ws, `document.getElementById('calPop').classList.contains('hidden')`, 12))
        await sleep(1200)
      }
      await shot(ws, '03-calendar.png')
      if (!(await evaluate(ws, `document.getElementById('calPop').classList.contains('hidden')`))) {
        await evaluate(ws, `document.getElementById('calClose').click()`)
        truthy('日历能关掉', await waitFor(ws, `document.getElementById('calPop').classList.contains('hidden')`, 12))
      }
    }
  })

  /* 3. 分组：5 个赛事子页逐个点（后台开关关掉时，正确行为是改道看板 → 断言这个） */
  await section('3. 分组（5 个赛事子页）', async () => {
    if (settings.groupEnabled === false) {
      console.log('  --   分组被后台「公开站点设置」关掉：断言「点分组会改道看板」')
      await navTo(ws, 'group')
      expect('分组关掉后改道看板', await evaluate(ws, `document.querySelector('[data-v].on')?.dataset.v ?? ''`), 'data')
      return
    }
    await navTo(ws, 'group')
    truthy('子导航显示出来', await evaluate(ws, `!document.getElementById('subnav').classList.contains('hidden')`))
    const groups = JSON.parse(await evaluate(ws, `JSON.stringify([...document.querySelectorAll('#subnav button:not([disabled])')].map((b) => ({ g: b.dataset.g, t: b.textContent.trim() })))`))
    truthy(`分组子页数量（${groups.length}）`, groups.length > 0)
    for (const { g, t } of groups) {
      /* 子导航按钮文案 ≠ 页面标题（按钮「月终争霸赛」/ 标题「月终争霸赛」，
         按钮「初级小组赛」/ 标题「初级争霸赛小组赛」）→ 期望值从页面自己的 GROUP_PAGES 取 */
      const want = await evaluate(ws, `GROUP_PAGES.find((p) => p.key === ${JSON.stringify(g)})?.label ?? ''`)
      await evaluate(ws, `document.querySelector('#subnav button[data-g="${g}"]').click()`)
      await sleep(1000)
      const label = await evaluate(ws, sub)
      expect(`点「${t}」→ 标题`, label, want)
      /* 关键回归：点了第 N 个子页不许落回第一个子页 */
      const firstKey = groups[0].g
      if (g !== firstKey) {
        truthy(`点「${t}」没被改道回第一个子页`, label !== groups[0].label && label === want)
      }
      await assertViewHealthy(ws, t)
    }
    await shot(ws, '04-group.png')
  })

  /* 4. 族谱（族谱是男团专属页；后台关掉时同样应改道）
     非师门成员（如管理员观看账号）会拿到 403 + code=lineage_forbidden：
     正确表现是**页面骨架保留、内容区给提示**，绝不能整页换成地区门禁文案。 */
  await section('4. 族谱', async () => {
    if (settings.lineageEnabled === false) {
      console.log('  --   族谱被后台「公开站点设置」关掉：断言「点族谱会改道看板」')
      await navTo(ws, 'lineage')
      expect('族谱关掉后改道看板', await evaluate(ws, `document.querySelector('[data-v].on')?.dataset.v ?? ''`), 'data')
      return
    }
    await navTo(ws, 'lineage')
    await sleep(1500)
    truthy('族谱页骨架还在（导航/标题没被整页替换）', await evaluate(ws, `!!document.getElementById('mhSub') && !!document.getElementById('navData')`))
    const s = await evaluate(ws, sub)
    const denied = s === 'LINEAGE' && (await evaluate(ws, viewText)).includes('族谱仅对师门成员开放')
    if (denied) {
      console.log('  --   当前账号非师门成员 → 断言就地提示（这正是 403 的正确落法）')
      truthy('非师门成员：内容区显示族谱提示', true)
      truthy('非师门成员：没有整页换成地区门禁文案', !(await evaluate(ws, 'document.body.innerText')).includes('IP 不在授权范围'))
    } else {
      truthy(`族谱标题是 LINEAGE（${s}）`, s.startsWith('LINEAGE'))
      const nodes = await evaluate(ws, `document.querySelectorAll('#view *').length`)
      truthy(`族谱画布有元素（${nodes} 个）`, nodes > 3)
    }
    await assertViewHealthy(ws, '族谱')
    await shot(ws, '05-lineage.png')
  })

  /* 5. 我的 */
  await section('5. 我的', async () => {
    await navTo(ws, 'my')
    await sleep(1500)
    expect('我的页标题', await evaluate(ws, sub), 'MY')
    await assertViewHealthy(ws, '我的')
    truthy('我的页有卡片', (await evaluate(ws, `document.querySelectorAll('#view .card, #view .section').length`)) > 0)
    await shot(ws, '06-my.png')
  })

  /* 6. 深链（等价刷新）：关掉的页要落到还开着的第一页，这也是正确行为 */
  await section('6. 深链直达 / 刷新不丢页', async () => {
    const wantOf = {
      '/report': 'data',
      '/group/mid': settings.groupEnabled === false ? 'data' : 'group',
      '/lineage': settings.lineageEnabled === false ? 'data' : 'lineage',
      '/my': settings.loginRequired === false ? 'data' : 'my',
    }
    for (const [path, want] of Object.entries(wantOf)) {
      await goto(ws, `${BASE}${path}`)
      await sleep(1800)
      const active = await evaluate(ws, `document.querySelector('[data-v].on')?.dataset.v ?? ''`)
      expect(`深链 ${path} 落在 ${want} 页`, active, want)
      await assertViewHealthy(ws, `深链 ${path}`)
      expect(`深链 ${path} 地址保持`, await evaluate(ws, 'location.pathname'), path)
    }
  })

  /* 7. 网络 + 控制台 */
  await section('7. 网络与控制台清洁度', async () => {
    const authFails = apiCalls.filter((c) => c.status === 401)
    const serverErrs = apiCalls.filter((c) => c.status >= 500)
    const clientErrs = apiCalls.filter((c) => c.status >= 400 && c.status < 500 && c.status !== 401)
    expect('登录后没有任何 /api 401（登录态站不住会在这里现形）', JSON.stringify(authFails.map((c) => c.url)), '[]')
    expect('没有 /api 5xx', JSON.stringify(serverErrs.map((c) => `${c.status} ${c.url}`)), '[]')
    if (clientErrs.length) {
      console.log(`  --   ${clientErrs.length} 条 4xx（非致命，逐条看）：`)
      for (const c of clientErrs.slice(0, 8)) console.log(`       ${c.status} ${c.url.replace(BASE, '')}`)
    }
    const real = errors.filter((e) => !/favicon|404 \(Not Found\)/i.test(e))
    expect('全程无 JS 报错', JSON.stringify(real.slice(0, 3)), '[]')
    console.log(`  --   /api 请求共 ${apiCalls.length} 条`)
  })
} finally {
  ws.close()
  chrome.kill()
}

console.log(failed === 0 ? '\n展示端全页面验收全部通过 ✓' : `\n展示端全页面验收失败 ${failed} 项 ✗`)
process.exit(failed === 0 ? 0 : 1)
