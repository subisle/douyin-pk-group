/**
 * 独立验证：展示页「公告多条 items」端到端（HTTP 契约 + 真浏览器 + 迁移回退 + 现场恢复）。
 *
 * 这是**验证脚本**，不是实现：只读契约断言 + 真开 headless Chrome 看 DOM；
 * 唯一写库动作是 3 次 POST /api/admin/announcement（配置 items），
 * 结尾把 items 写回空数组 []（= 迁移合成态），不留测试数据。
 *
 * 断言分四段：
 *   A. 契约：基线 GET 有 items 且恰好 1 条（迁移合成）
 *   B. 配 2 条（brand/plain，顺序 A、B）→ admin GET / teams/all / announcements 同序同口径
 *   C. 负向：enabled=false 不下发、非法 icon/tone/wave 归一、全空条目丢弃
 *   D. 迁移：POST items: [] → 三处公开口径都恰好回落 1 条（不是 2 条）= 现场恢复
 *   浏览器断言（在 B 与 D 之间各跑一次）：#annItems 子元素条数 / 顺序 / tone 样式 / 无重复条 / console 无 error
 *
 * 用法：
 *   node scripts/check/uicheck-announce-items.mjs [BASE]
 *   环境变量 BASE / ADMIN_PASSWORD / MY_ACCOUNT / MY_PASS / CDP_PORT(默认 9341) 可覆盖
 *
 * 前置：pk-server 已在 BASE 上跑（127.0.0.1:8787），存在一个已绑定主播的账号能过 /api/public/my/login。
 * 只依赖 Node 内置能力（Node 22+ 自带 WebSocket/fetch）+ 本机 Chrome。
 */
import { execSync, spawn } from 'node:child_process'
import { mkdirSync, rmSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { setTimeout as sleep } from 'node:timers/promises'

const BASE = (process.env.BASE ?? process.argv[2] ?? 'http://127.0.0.1:8787').replace(/\/+$/, '')
const PORT = Number(process.env.CDP_PORT ?? 9341) // 故意不占 uicheck-admin 的 9334
const OUT = fileURLToPath(new URL('../.uicheck/', import.meta.url))
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
const ADMIN_PASSWORD = process.env.ADMIN_PASSWORD ?? 'shuai4444@'
const MY_ACCOUNT = process.env.MY_ACCOUNT ?? 'shuai4444@'
const MY_PASS = process.env.MY_PASS ?? 'shuai4444@'

mkdirSync(OUT, { recursive: true })

/* ------------------------------ 断言计数 ------------------------------ */
let failed = 0
let checks = 0
const ok = (m) => {
  checks++
  console.log(`  ok   ${m}`)
}
const bad = (m) => {
  checks++
  failed++
  console.log(`  FAIL ${m}`)
}
const expect = (m, got, want) =>
  String(got) === String(want) ? ok(`${m} (${got})`) : bad(`${m}：期望 ${want}，实际 ${got}`)
const truthy = (m, got) => (got ? ok(m) : bad(m))
const same = (m, a, b) => {
  const x = JSON.stringify(a)
  const y = JSON.stringify(b)
  x === y ? ok(`${m} (${x.slice(0, 140)}${x.length > 140 ? '…' : ''})`) : bad(`${m}：\n       期望 ${y}\n       实际 ${x}`)
}

/* ------------------------------- HTTP ------------------------------- */
async function adminLogin() {
  const r = await fetch(`${BASE}/api/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ password: ADMIN_PASSWORD }),
  })
  if (!r.ok) throw new Error(`管理员登录失败 HTTP ${r.status}`)
  return (await r.json()).token
}

async function myLogin() {
  const r = await fetch(`${BASE}/api/public/my/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ account: MY_ACCOUNT, pass: MY_PASS }),
  })
  if (!r.ok) throw new Error(`主播端登录失败 HTTP ${r.status}`)
  const d = await r.json()
  if (!d.token) throw new Error(`主播端登录没回 token：${JSON.stringify(d).slice(0, 200)}`)
  return d.token
}

const adminToken = await adminLogin()
const myToken = await myLogin()
const adminGet = async () =>
  (await fetch(`${BASE}/api/admin/announcement`, { headers: { Authorization: `Bearer ${adminToken}` } })).json()
const adminPost = async (body) =>
  (
    await fetch(`${BASE}/api/admin/announcement`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${adminToken}` },
      body: JSON.stringify(body),
    })
  ).json()
const pubTeamsAll = async () =>
  (await fetch(`${BASE}/api/public/teams/all`, { headers: { 'X-My-Token': myToken } })).json()
const pubAnnouncements = async () => (await fetch(`${BASE}/api/public/announcements`)).json()

/* --------------------------- 极简 CDP 客户端 --------------------------- */
async function freshTarget() {
  for (let i = 0; i < 40; i++) {
    try {
      const resp = await fetch(`http://127.0.0.1:${PORT}/json/new?about:blank`, { method: 'PUT' })
      if (resp.ok) {
        const t = await resp.json()
        if (t.webSocketDebuggerUrl) return t.webSocketDebuggerUrl
      }
    } catch {
      /* Chrome 还没起来 */
    }
    await sleep(250)
  }
  throw new Error(`连不上 Chrome 调试端口 ${PORT}`)
}

function connect(url) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url)
    let id = 0
    const pending = new Map()
    const listeners = []
    ws.onopen = () =>
      resolve({
        send: (method, params = {}) =>
          new Promise((res, rej) => {
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

async function evaluate(ws, expr) {
  const r = await ws.send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true })
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? '求值失败')
  return r.result.value
}

async function waitFor(ws, expr, tries = 60) {
  for (let i = 0; i < tries; i++) {
    const hit = await evaluate(ws, `!!(${expr})`).catch(() => false)
    if (hit) return true
    await sleep(250)
  }
  return false
}

async function shot(ws, name) {
  const { data } = await ws.send('Page.captureScreenshot', { format: 'png' })
  writeFileSync(OUT + name, Buffer.from(data, 'base64'))
  console.log(`  ->   ${OUT}${name}`)
}

/* ------------------------------ 测试数据 ------------------------------ */
const A = {
  enabled: true,
  icon: 'emoji_events',
  title: '验证用·品牌条 A',
  name: '甲主播',
  wave: 123456,
  text: 'brand 紫金渐变正文',
  tone: 'brand',
}
const B = {
  enabled: true,
  icon: 'bolt',
  title: '验证用·描边条 B',
  name: '乙主播',
  wave: 0,
  text: 'plain 白底描边正文',
  tone: 'plain',
}
const NEG = [
  { enabled: false, icon: 'emoji_events', title: '关掉的条', name: '丙', wave: 5, text: '', tone: 'brand' },
  { enabled: true, icon: '绝对不是合法图标', title: '非法图标条', name: '丁', wave: -9, text: '', tone: 'weird' },
  { enabled: true, icon: '', title: '', name: '', text: '', tone: 'plain' }, // 全空 → 写入即丢
  { enabled: true, icon: 'campaign', title: '合法第三条', name: '戊', wave: 0, text: '纯文本条', tone: 'plain' },
]

/* =============================== 开始 =============================== */
console.log(`== 公告多条端到端验证 · BASE=${BASE} · CDP:${PORT} ==`)

/* ---------- A. 基线契约 ---------- */
console.log('\n== A. 基线（迁移合成态） ==')
const before = await adminGet()
console.log(`  [证据] GET /api/admin/announcement → items=${JSON.stringify(before.items)} version=${before.version}`)
truthy('admin GET 响应含 items 字段且是数组', Array.isArray(before.items))
expect('基线 items 恰好 1 条（items 为空 → 内存合成）', before.items.length, 1)
const baselineItems = JSON.parse(JSON.stringify(before.items))
const baselineVersion = before.version

const pubAllBefore = await pubTeamsAll()
same('基线 public/teams/all items 与 admin 同口径', pubAllBefore.items, before.items)
const pubAnnBefore = await pubAnnouncements()
same('基线 public/announcements items 与 admin 同口径', pubAnnBefore.items, before.items)

/* ---------- 浏览器准备 ---------- */
const PROFILE = `/tmp/pk-ann-uicheck-profile-${process.pid}`
try {
  for (const pid of execSync(`lsof -ti tcp:${PORT}`, { stdio: ['ignore', 'pipe', 'ignore'] })
    .toString()
    .split(/\s+/)
    .filter(Boolean)) {
    if (Number(pid) === process.pid) continue
    try {
      process.kill(Number(pid), 'SIGKILL')
    } catch {
      /* 已退出 */
    }
  }
} catch {
  /* 端口没人占 */
}
rmSync(PROFILE, { recursive: true, force: true })

const chrome = spawn(
  CHROME,
  [
    '--headless=new',
    `--remote-debugging-port=${PORT}`,
    `--user-data-dir=${PROFILE}`,
    '--no-first-run',
    '--no-default-browser-check',
    '--disable-gpu',
    '--hide-scrollbars',
    'about:blank',
  ],
  { stdio: 'ignore' },
)

let ws = null
let consoleErrors = []
let logErrors = []
try {
  ws = await connect(await freshTarget())
  await ws.send('Page.enable')
  await ws.send('Runtime.enable')
  await ws.send('Log.enable')
  await ws.send('Network.enable')
  await ws.send('Emulation.setDeviceMetricsOverride', {
    width: 1440,
    height: 900,
    deviceScaleFactor: 1,
    mobile: false,
  })
  ws.on((msg) => {
    if (msg.method === 'Runtime.exceptionThrown') {
      consoleErrors.push('exception: ' + (msg.params.exceptionDetails?.exception?.description ?? '未知'))
    }
    if (msg.method === 'Runtime.consoleAPICalled' && msg.params.type === 'error') {
      consoleErrors.push('console.error: ' + msg.params.args.map((a) => a.value ?? a.description ?? '').join(' '))
    }
    if (msg.method === 'Log.entryAdded' && msg.params.entry?.level === 'error') {
      logErrors.push(`${msg.params.entry.source}: ${msg.params.entry.text} ${msg.params.entry.url ?? ''}`)
    }
  })

  /* 登录态注入：展示页从 sessionStorage 读 myToken_any（源码 MY_ANY_KEY）。
     同时把 pk_announce_v 垫成当前 version，避免引导弹窗遮住页面影响取证。 */
  const injectLogin = (annVersion) =>
    ws.send('Page.addScriptToEvaluateOnNewDocument', {
      source:
        `try { sessionStorage.setItem('myToken_any', ${JSON.stringify(myToken)});` +
        ` localStorage.removeItem('myToken_any');` +
        ` localStorage.setItem('pk_announce_v', ${JSON.stringify(String(annVersion))}); } catch (e) {}`,
    })

  /**
   * 打开展示页并把公告条渲染等到位。
   * @param {number} want 期望 #annItems 子元素数（等不到也继续，交由断言报 FAIL）
   */
  async function openBoard(want, annVersion) {
    consoleErrors = []
    logErrors = []
    await injectLogin(annVersion)
    await ws.send('Page.navigate', { url: `${BASE}/` })
    for (let i = 0; i < 80; i++) {
      await sleep(250)
      const n = await evaluate(ws, `document.querySelectorAll('#annItems .ann-item').length`).catch(() => -1)
      const home = await evaluate(ws, `!!document.getElementById('home-ann')`).catch(() => false)
      if (home && n >= 0 && (n === want || i > 40)) break
    }
    // 引导弹窗万一还是弹了：点掉，避免遮罩影响截图（不影响 #annItems 断言）
    await evaluate(ws, `(function(){ var c = window.announceCtx; if (typeof c === 'function') { try { c() } catch(e){} } })()`).catch(() => {})
    await sleep(200)
  }

  /** 采集 #annItems 的结构化快照 */
  const snapshot = () =>
    evaluate(
      ws,
      `(() => {
        const rows = [...document.querySelectorAll('#annItems .ann-item')]
        const bodyText = document.body.innerText
        return JSON.stringify({
          count: rows.length,
          allAnnItems: document.querySelectorAll('.ann-item').length,
          rows: rows.map((r) => {
            const cs = getComputedStyle(r)
            return {
              cls: r.className,
              text: r.innerText.replace(/\\s+/g, ' ').trim(),
              icon: (r.querySelector('.ai-ico') || {}).textContent || '',
              backgroundImage: cs.backgroundImage,
              boxShadow: cs.boxShadow,
              borderTop: cs.borderTopWidth + ' ' + cs.borderTopStyle + ' ' + cs.borderTopColor,
              title: (r.querySelector('.ai-title') || {}).textContent || '',
              name: (r.querySelector('.ai-name') || {}).textContent || '',
            }
          }),
          champTitleCount: (bodyText.match(/初级争霸赛冠军/g) || []).length,
          homeAnn: (document.getElementById('home-ann') || {}).innerText || '',
          annItemsDisplay: getComputedStyle(document.getElementById('annItems')).display,
        })
      })()`,
    ).then(JSON.parse)

  const human = (rs) => rs.map((r) => `${r.cls.includes('ai-brand') ? 'brand' : 'plain'}｜${r.title}｜${r.name}｜${r.text}`).join('  ||  ')

  /* ---------- 浏览器 1：基线 1 条 ---------- */
  console.log('\n== 浏览器 1：基线（1 条合成公告） ==')
  await openBoard(1, baselineVersion)
  const s1 = await snapshot()
  console.log(`  [证据] #annItems 子元素数=${s1.count} 全页 .ann-item 数=${s1.allAnnItems} 内容：${human(s1.rows)}`)
  console.log(`  [证据] 全页「初级争霸赛冠军」出现次数=${s1.champTitleCount}；#home-ann="${s1.homeAnn.replace(/\n/g, ' ')}"`)
  expect('页面顶部 #annItems 恰好 1 条', s1.count, 1)
  expect('全页 .ann-item 也只有 1 条（无第二套条）', s1.allAnnItems, 1)
  expect('「初级争霸赛冠军」全页只出现 1 次（无重复冠军条）', s1.champTitleCount, 1)
  truthy('该条文案含标题', s1.rows[0]?.text.includes('初级争霸赛冠军'))
  truthy('该条文案含人名', s1.rows[0]?.text.includes('狼哲'))
  truthy('该条文案含音浪（狼哲 · 28万5917）', s1.rows[0]?.text.includes('28万5917'))
  await shot(ws, 'announce-items-one.png')
  same('浏览器 1 阶段 console 无 error', consoleErrors, [])
  if (logErrors.length) console.log(`  [备注] Log error（非 console.error）：${JSON.stringify(logErrors)}`)

  /* ---------- B. 契约：配 2 条 ---------- */
  console.log('\n== B. 配 2 条（A=brand、B=plain，顺序 A、B） ==')
  const post2 = await adminPost({ items: [A, B] })
  expect('POST 响应 announcement.items 条数', post2.announcement?.items?.length, 2)
  const after2 = await adminGet()
  console.log(`  [证据] GET /api/admin/announcement → items=${JSON.stringify(after2.items)}`)
  expect('admin GET items 条数', after2.items.length, 2)
  same('顺序 = A、B', after2.items.map((i) => i.title), [A.title, B.title])
  truthy('A 的 icon 在白名单内原样保留', after2.items[0].icon === 'emoji_events')
  truthy('A 的 tone = brand 且 wave 原样', after2.items[0].tone === 'brand' && after2.items[0].wave === 123456)
  truthy('A 的 text 原样保留', after2.items[0].text === A.text)
  truthy('B 的 tone = plain 且 wave=0', after2.items[1].tone === 'plain' && after2.items[1].wave === 0)
  truthy('B 的 icon 在白名单内原样保留', after2.items[1].icon === 'bolt')

  const all2 = await pubTeamsAll()
  console.log(`  [证据] GET /api/public/teams/all → items=${JSON.stringify(all2.items)}`)
  expect('public/teams/all items 条数', all2.items.length, 2)
  same('public items 与 admin 同序同内容', all2.items, after2.items)
  same('announcement.items 与顶层 items 同口径', all2.announcement?.items, all2.items)
  const pa2 = await pubAnnouncements()
  console.log(`  [证据] GET /api/public/announcements → items=${JSON.stringify(pa2.items)}`)
  same('public/announcements.items 与 site.items 同口径', pa2.items, all2.items)

  /* ---------- 浏览器 2：2 条 ---------- */
  console.log('\n== 浏览器 2：2 条公告（顺序 + tone 样式） ==')
  await openBoard(2, after2.version)
  const s2 = await snapshot()
  console.log(`  [证据] #annItems 子元素数=${s2.count} 内容：${human(s2.rows)}`)
  console.log(`  [证据] tone 样式：${JSON.stringify(s2.rows.map((r) => ({ cls: r.cls, backgroundImage: r.backgroundImage, boxShadow: r.boxShadow, borderTop: r.borderTop, icon: r.icon })))}`)
  console.log(`  [证据] 全页「初级争霸赛冠军」出现次数=${s2.champTitleCount}（配置 2 条后应消失）`)
  expect('页面出现 2 条公告', s2.count, 2)
  expect('全页 .ann-item 数量一致（无第二套）', s2.allAnnItems, 2)
  same('顺序 = A、B', s2.rows.map((r) => r.title), [A.title, B.title])
  truthy('第 1 条 class = ai-brand', s2.rows[0]?.cls.includes('ai-brand'))
  truthy('第 2 条 class = ai-plain', s2.rows[1]?.cls.includes('ai-plain'))
  truthy(
    'brand 条是紫金深色渐变（#4C1D95 系）+ 发光阴影',
    /rgb\(76, 29, 149\)/.test(s2.rows[0]?.backgroundImage ?? '') && s2.rows[0]?.boxShadow !== 'none',
  )
  truthy('plain 条是浅底渐变 + 描边', /rgb\(245, 243, 255\)/.test(s2.rows[1]?.backgroundImage ?? '') && s2.rows[1]?.borderTop.startsWith('1px solid'))
  truthy('两条背景样式确实不同', s2.rows[0]?.backgroundImage !== s2.rows[1]?.backgroundImage)
  truthy('第 1 条文案含标题/人名/音浪（甲主播 · 12万3456）', (s2.rows[0]?.text ?? '').includes('甲主播 · 12万3456'))
  truthy('第 2 条 wave=0 不显示音浪数字', !/万|\d{4,}/.test(s2.rows[1]?.name ?? ''))
  expect('配了 2 条后「初级争霸赛冠军」不再出现', s2.champTitleCount, 0)
  await shot(ws, 'announce-items-two.png')
  same('浏览器 2 阶段 console 无 error', consoleErrors, [])
  if (logErrors.length) console.log(`  [备注] Log error（非 console.error）：${JSON.stringify(logErrors)}`)

  /* ---------- C. 负向 ---------- */
  console.log('\n== C. 负向：enabled=false / 非法 icon / 非法 tone / 负 wave / 全空条 ==')
  const post3 = await adminPost({ items: NEG })
  expect('POST 响应：4 条里全空条被丢弃 → 3 条', post3.announcement?.items?.length, 3)
  const afterNeg = await adminGet()
  console.log(`  [证据] GET /api/admin/announcement → items=${JSON.stringify(afterNeg.items)}`)
  expect('admin 侧保留 3 条（含 enabled=false 供编辑）', afterNeg.items.length, 3)
  expect('enabled=false 条目仍在 admin 全量下发里', afterNeg.items[0].enabled, false)
  expect('非法 icon 归一为空串', JSON.stringify(afterNeg.items[1].icon), '""')
  expect('非法 tone 归一为 plain', afterNeg.items[1].tone, 'plain')
  expect('负数 wave 归 0', afterNeg.items[1].wave, 0)
  truthy('全空条目确实没进配置（无空标题条目）', !afterNeg.items.some((i) => !i.title && !i.name && !i.text))

  const storedEnabled = afterNeg.items.filter((i) => i.enabled).length
  expect('admin 侧 3 条里 enabled=true 的有 2 条（另 1 条关掉但仍保存）', storedEnabled, 2)
  const allNeg = await pubTeamsAll()
  console.log(`  [证据] GET /api/public/teams/all → items=${JSON.stringify(allNeg.items)}`)
  expect('公开下发只含 enabled=true 的 2 条', allNeg.items.length, 2)
  same('公开下发 = admin 里 enabled=true 的子集（顺序不变）', allNeg.items, afterNeg.items.filter((i) => i.enabled))
  truthy('公开下发不含任何 enabled=false 条目', !allNeg.items.some((i) => i.enabled === false))
  truthy('公开下发无「关掉的条」', !allNeg.items.some((i) => i.title === '关掉的条'))
  const paNeg = await pubAnnouncements()
  same('public/announcements.items 与 site.items 同口径（负向态）', paNeg.items, allNeg.items)

  /* ---------- D. 迁移回退 + 现场恢复 ---------- */
  console.log('\n== D. items=[] 迁移口径（同时恢复现场） ==')
  const post4 = await adminPost({ items: [] })
  expect('POST items:[] 响应即回落 1 条', post4.announcement?.items?.length, 1)
  const afterEmpty = await adminGet()
  console.log(`  [证据] GET /api/admin/announcement → items=${JSON.stringify(afterEmpty.items)} version=${afterEmpty.version}`)
  expect('items=[] 后 admin GET 恰好 1 条（来自 celebrate/champ_banner 合成）', afterEmpty.items.length, 1)
  truthy('合成条不是空条（有标题或人名）', !!(afterEmpty.items[0]?.title || afterEmpty.items[0]?.name))
  truthy('合成条来自 celebrate 口径（title=初级争霸赛冠军 / name=狼哲）',
    afterEmpty.items[0]?.title === '初级争霸赛冠军' && afterEmpty.items[0]?.name === '狼哲')
  const allEmpty = await pubTeamsAll()
  const paEmpty = await pubAnnouncements()
  console.log(`  [证据] GET /api/public/teams/all → items=${JSON.stringify(allEmpty.items)}`)
  console.log(`  [证据] GET /api/public/announcements → items=${JSON.stringify(paEmpty.items)}`)
  expect('public/teams/all 恰好 1 条', allEmpty.items.length, 1)
  expect('public/announcements 恰好 1 条', paEmpty.items.length, 1)
  truthy('三处公开口径都是 1 条、不是 2 条', [afterEmpty.items.length, allEmpty.items.length, paEmpty.items.length].every((n) => n === 1))
  same('public 与 admin 同口径（迁移态）', allEmpty.items, afterEmpty.items)

  /* ---------- 浏览器 3：恢复后回到 1 条 ---------- */
  console.log('\n== 浏览器 3：恢复现场后回到 1 条 ==')
  await openBoard(1, afterEmpty.version)
  const s3 = await snapshot()
  console.log(`  [证据] #annItems 子元素数=${s3.count} 内容：${human(s3.rows)}`)
  expect('恢复后页面顶部恰好 1 条', s3.count, 1)
  expect('恢复后「初级争霸赛冠军」全页只出现 1 次', s3.champTitleCount, 1)
  await shot(ws, 'announce-items-restored.png')
  same('浏览器 3 阶段 console 无 error', consoleErrors, [])
  if (logErrors.length) console.log(`  [备注] Log error（非 console.error）：${JSON.stringify(logErrors)}`)

  /* ---------- E. 现场比对 ---------- */
  console.log('\n== E. 现场恢复比对 ==')
  const finalAnn = await adminGet()
  const strip = (a) => {
    const { version, ...rest } = a
    return rest
  }
  same('恢复后（忽略 version）与基线完全一致', strip(finalAnn), strip(before))
  console.log(`  [证据] version 基线=${baselineVersion} → 恢复后=${finalAnn.version}（3 次 POST 各 +1，属既有保存语义）`)
  console.log(`  [证据] 恢复后 items=${JSON.stringify(finalAnn.items)}（= items 存空数组后的内存合成态）`)
} finally {
  try {
    ws?.close()
  } catch {
    /* ignore */
  }
  try {
    chrome.kill('SIGKILL')
  } catch {
    /* ignore */
  }
  try {
    for (const pid of execSync(`lsof -ti tcp:${PORT}`, { stdio: ['ignore', 'pipe', 'ignore'] })
      .toString()
      .split(/\s+/)
      .filter(Boolean)) {
      // 本进程自己也连着 CDP 端口，lsof 会把自己列出来 —— 绝不能 kill 自己
      if (Number(pid) === process.pid) continue
      try {
        process.kill(Number(pid), 'SIGKILL')
      } catch {
        /* ignore */
      }
    }
  } catch {
    /* 没有残留 */
  }
  rmSync(PROFILE, { recursive: true, force: true })
}

console.log(`\n== 结果：${checks - failed}/${checks} 通过${failed ? `，${failed} 条 FAIL` : ''} ==`)
process.exit(failed ? 1 : 0)
