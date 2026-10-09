/**
 * 管理端（apps/douyin-web）路由自检：真开 Chrome，用 CDP 把 history 路由跑一遍。
 *
 * 为什么不用 tsc / vite build：那两个只保证「能编译」，保证不了「运行时路由对不对」。
 * 这里断言的是路由契约本身：
 *   1. 每个页面一个地址：/admin/data|import|streamers|claims|lineage|group|bots 深链直达
 *   2. 重新导航（= 刷新）不丢页；点导航走 pushState（不整页刷新）；前进后退回到原页
 *   3. 认不出的路径落回 /admin/data；未登录时深链留在地址栏，登录后照旧
 *   4. 导入页没有粘贴入口；上传后弹预览弹窗；确认入库 → 批次列表出现 → 删掉当天数据收尾
 *
 * 用法：
 *   node scripts/uicheck-admin.mjs [BASE] [PASSWORD]
 *     BASE     默认 http://127.0.0.1:8787（pk-server；管理端在 /admin），也可用环境变量 BASE 给
 *     PASSWORD 默认从 /tmp/pk-local.env 的 ADMIN_PASSWORD 读
 *   环境变量 PREFIX 覆盖地址前缀（默认 /admin）。验 vite dev 的无前缀形态：
 *     BASE=http://localhost:5173 PREFIX= node scripts/uicheck-admin.mjs
 *
 * 只用 Node 内置能力（Node 22+ 自带 WebSocket）+ 本机 Chrome，不需要 puppeteer。
 *
 * ⚠️ 第 7 段会往库里写一个 2019-01-01 的测试业务日，并在同一次运行里删掉；
 *    只对本地 / 测试实例跑，别对着生产跑。
 */
import { execSync, spawn } from 'node:child_process'
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { setTimeout as sleep } from 'node:timers/promises'

const BASE = (process.argv[2] ?? process.env.BASE ?? 'http://127.0.0.1:8787').replace(
  /\/+$/,
  '',
)
/** 管理端前缀：生产由 spa.rs 挂在 /admin 下；vite dev 直接挂根 → PREFIX='' */
const PREFIX = process.env.PREFIX ?? '/admin'
const OUT = fileURLToPath(new URL('../.uicheck/', import.meta.url))
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
const PORT = 9334
const CSV = '/tmp/pk-admin-uicheck.csv'
/** 明显不会有人用的业务日期：入库 → 断言 → 当场删除 */
const TEST_DATE = '2019-01-01'

const PAGES = [
  { path: '/data', label: '数据概况', marker: '当日总音浪' },
  { path: '/import', label: '数据导入', marker: '导入音浪数据' },
  { path: '/streamers', label: '主播管理', marker: '主播台账' },
  { path: '/claims', label: '抖音号审计', marker: '抖音号绑定审计' },
  { path: '/lineage', label: '族谱', marker: '师门关系' },
  { path: '/group', label: 'PK 分组', marker: '名单与配置' },
  { path: '/bots', label: '机器人', marker: 'QQ 机器人' },
]

function passwordFromEnv() {
  try {
    const m = readFileSync('/tmp/pk-local.env', 'utf8').match(/^export ADMIN_PASSWORD=(.*)$/m)
    return m ? m[1].trim() : ''
  } catch {
    return ''
  }
}

const PASSWORD = process.argv[3] ?? passwordFromEnv()
if (!PASSWORD) {
  console.error('缺少管理员口令：用法 node scripts/uicheck-admin.mjs [BASE] [PASSWORD]')
  process.exit(2)
}

mkdirSync(OUT, { recursive: true })

/* ------------------------------ 断言计数 ------------------------------ */
let failed = 0
const ok = (m) => console.log(`  ok   ${m}`)
const bad = (m) => {
  console.log(`  FAIL ${m}`)
  failed++
}
const expect = (m, got, want) =>
  String(got) === String(want) ? ok(`${m} (${got})`) : bad(`${m}：期望 ${want}，实际 ${got}`)
const truthy = (m, got) => (got ? ok(m) : bad(m))

/* --------------------------- 极简 CDP 客户端 --------------------------- */
/**
 * 开一个干净的新标签页再连它。
 *
 * 坑（踩过）：headless Chrome 用固定 user-data-dir 会**恢复上次会话的标签页**，
 * `/json/list` 里于是有好几个 page（chrome://settings、上一轮残留的页面），
 * find(type==='page') 可能连到错的那个 → 断言读的是旧页面，出现「期望 /bots，
 * 实际 /admin/bots」这种看不懂的假失败。所以：先杀掉占着调试端口的僵尸 Chrome、
 * 清掉 profile，再用 /json/new 明确开一个 about:blank 连上去。
 */
async function freshTarget() {
  for (let i = 0; i < 40; i++) {
    try {
      const resp = await fetch(`http://127.0.0.1:${PORT}/json/new?about:blank`, {
        method: 'PUT',
      })
      if (resp.ok) {
        const t = await resp.json()
        if (t.webSocketDebuggerUrl) return t.webSocketDebuggerUrl
      }
    } catch {
      /* Chrome 还没起来 */
    }
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
  const r = await ws.send('Runtime.evaluate', {
    expression: expr,
    returnByValue: true,
    awaitPromise: true,
  })
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? '求值失败')
  return r.result.value
}

async function goto(ws, url) {
  await ws.send('Page.navigate', { url })
  for (let i = 0; i < 60; i++) {
    await sleep(250)
    const ready = await evaluate(
      ws,
      `document.getElementById('root')?.firstElementChild != null`,
    ).catch(() => false)
    if (ready) return
  }
  throw new Error(`页面没挂载：${url}`)
}

async function waitFor(ws, expr, tries = 40) {
  for (let i = 0; i < tries; i++) {
    const hit = await evaluate(ws, `!!(${expr})`).catch(() => false)
    if (hit) return true
    await sleep(250)
  }
  return false
}

const waitText = (ws, text, tries) =>
  waitFor(ws, `document.body.innerText.includes(${JSON.stringify(text)})`, tries)
const pathname = (ws) => evaluate(ws, 'location.pathname')
const activeTab = (ws) =>
  evaluate(ws, `document.querySelector('a.tab.active')?.textContent?.trim() ?? ''`)

/** 点页面上第一个文本包含 text 的可点元素（导航链接 / 按钮） */
async function clickByText(ws, text) {
  const hit = await evaluate(
    ws,
    `(() => {
      const el = [...document.querySelectorAll('a.tab, button, a.file-btn')]
        .find((x) => x.textContent.trim().includes(${JSON.stringify(text)}))
      if (!el) return false
      el.click()
      return true
    })()`,
  )
  if (!hit) throw new Error(`没找到可点元素：${text}`)
}

async function shot(ws, name) {
  const { data } = await ws.send('Page.captureScreenshot', { format: 'png' })
  writeFileSync(OUT + name, Buffer.from(data, 'base64'))
  console.log(`  ->  ${OUT}${name}`)
}

/* ------------------------------- 开始 ------------------------------- */
console.log(`== 管理端路由自检 · BASE=${BASE} ==`)

const loginResp = await fetch(`${BASE}/api/auth/login`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ password: PASSWORD }),
})
if (!loginResp.ok) {
  console.error(`登录失败：HTTP ${loginResp.status}（口令对不对？）`)
  process.exit(2)
}
const { token } = await loginResp.json()
const apiGet = async (path) =>
  (await fetch(BASE + path, { headers: { Authorization: `Bearer ${token}` } })).json()

/* 上一次跑崩了可能把测试日留下：开跑前先清一遍，保证断言从干净状态出发 */
const stale = await apiGet(`/api/admin/import/day-status?bizDate=${TEST_DATE}&team=male`)
if ((stale.rows ?? 0) > 0) {
  const del = await (
    await fetch(`${BASE}/api/admin/import/day?bizDate=${TEST_DATE}&team=male`, {
      method: 'DELETE',
      headers: { Authorization: `Bearer ${token}` },
    })
  ).json()
  console.log(`（清掉上次残留：${TEST_DATE} ${del.metrics} 条音浪 / ${del.batches} 个批次）`)
}

/* 导入用例用真主播名（否则匹配不上，导入全失败） */
const streamers = await apiGet('/api/admin/streamers?team=male')
const names = (streamers.items ?? []).slice(0, 3).map((s) => s.name)
if (names.length < 3) {
  console.error('男团主播不足 3 人，无法构造导入用例')
  process.exit(2)
}
const csv = ['主播名,音浪', ...names.map((n, i) => `${n},${(i + 1) * 111000}`)].join('\n') + '\n'
writeFileSync(CSV, csv)

/* 干净的浏览器：杀掉占着调试端口的僵尸实例 + 清 profile（否则会恢复上一轮标签页，
   /json/list 里混进旧页面，断言可能读到别的页面） */
const PROFILE = `/tmp/pk-admin-uicheck-profile-${process.pid}`
try {
  // 只杀占着这个调试端口的进程（别用 pkill -f 模式，它会把执行它的 shell 也匹配上）
  for (const pid of execSync(`lsof -ti tcp:${PORT}`, { stdio: ['ignore', 'pipe', 'ignore'] })
    .toString()
    .split(/\s+/)
    .filter(Boolean)) {
    try {
      process.kill(Number(pid), 'SIGKILL')
    } catch {
      /* 已经退出了 */
    }
  }
} catch {
  /* 没有僵尸进程（lsof 无输出会非零退出）也无所谓 */
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

let ws
try {
  ws = await connect(await freshTarget())
  await ws.send('Page.enable')
  await ws.send('Runtime.enable')
  await ws.send('DOM.enable')
  await ws.send('Emulation.setDeviceMetricsOverride', {
    width: 1440,
    height: 900,
    deviceScaleFactor: 1,
    mobile: false,
  })
  /* confirm 弹窗：删除当天数据要用，直接接受 */
  ws.on((msg) => {
    if (msg.method === 'Page.javascriptDialogOpening') {
      ws.send('Page.handleJavaScriptDialog', { accept: true }).catch(() => {})
    }
  })

  let errors = []
  ws.on((msg) => {
    if (msg.method === 'Runtime.exceptionThrown') {
      errors.push(msg.params.exceptionDetails?.exception?.description ?? '未知异常')
    }
    if (msg.method === 'Runtime.consoleAPICalled' && msg.params.type === 'error') {
      errors.push(msg.params.args.map((a) => a.value ?? a.description ?? '').join(' '))
    }
  })

  /* ---------- 1. 未登录：深链留在地址栏 ---------- */
  console.log('== 1. 未登录深链 ==')
  errors = []
  await goto(ws, `${BASE}${PREFIX}/import`)
  await waitFor(ws, `document.querySelector('.login')`)
  truthy('登录门可见', await evaluate(ws, `!!document.querySelector('.login')`))
  expect('未登录时地址栏保留深链', await pathname(ws), `${PREFIX}/import`)

  /* 注入登录态（sessionStorage 里放 token，等价于已登录刷新） */
  await ws.send('Page.addScriptToEvaluateOnNewDocument', {
    source: `try { sessionStorage.setItem('pk_token', ${JSON.stringify(token)}) } catch (e) {}`,
  })

  /* ---------- 2. 七个页面：深链直达（等价刷新） ---------- */
  console.log('== 2. 深链直达 / 刷新不丢页 ==')
  for (const p of PAGES) {
    errors = []
    await goto(ws, `${BASE}${PREFIX}${p.path}`)
    const ready = await waitText(ws, p.marker, 60)
    truthy(`/${p.path} 渲染出「${p.marker}」`, ready)
    expect(`${PREFIX}${p.path} 地址不变`, await pathname(ws), `${PREFIX}${p.path}`)
    expect(`/${p.path} 导航高亮`, await activeTab(ws), p.label)
    if (errors.length) bad(`/${p.path} console 报错：${errors.join(' | ')}`)
  }

  await goto(ws, `${BASE}${PREFIX}/data`)
  await waitText(ws, '当日总音浪')
  await shot(ws, 'admin-data.png')

  /* ---------- 3. 点导航 = pushState（不整页刷新） ---------- */
  console.log('== 3. 导航是真链接 + pushState ==')
  errors = []
  const hrefs = await evaluate(
    ws,
    `JSON.stringify([...document.querySelectorAll('a.tab')].map((a) => a.getAttribute('href')))`,
  )
  expect(
    '导航 href 与路由表一致',
    hrefs,
    JSON.stringify(PAGES.map((p) => `${PREFIX}${p.path}`)),
  )
  await evaluate(ws, 'window.__pkNavMarker = 1')
  await clickByText(ws, '数据导入')
  truthy('切到导入页', await waitText(ws, '导入音浪数据'))
  expect(`地址变成 ${PREFIX}/import`, await pathname(ws), `${PREFIX}/import`)
  expect('没整页刷新（marker 还在）', await evaluate(ws, 'window.__pkNavMarker'), 1)

  await clickByText(ws, '机器人')
  truthy('切到机器人页', await waitText(ws, 'QQ 机器人'))
  expect(`地址变成 ${PREFIX}/bots`, await pathname(ws), `${PREFIX}/bots`)

  /* ---------- 4. 前进后退 ---------- */
  console.log('== 4. 前进后退 ==')
  await evaluate(ws, 'history.back()')
  truthy(`后退回到 ${PREFIX}/import`, await waitFor(ws, `location.pathname === '${PREFIX}/import'`))
  await waitText(ws, '导入音浪数据')
  await evaluate(ws, 'history.forward()')
  truthy(`前进回到 ${PREFIX}/bots`, await waitFor(ws, `location.pathname === '${PREFIX}/bots'`))
  await waitText(ws, 'QQ 机器人')

  /* ---------- 5. 认不出的路径落回默认页 ---------- */
  console.log('== 5. 未知路径兜底 ==')
  await goto(ws, `${BASE}${PREFIX}/nope`)
  truthy(`未知路径归位 ${PREFIX}/data`, await waitFor(ws, `location.pathname === '${PREFIX}/data'`))
  await waitText(ws, '当日总音浪')

  /* ---------- 6. 导入页：没有粘贴入口 + 上传弹预览 ---------- */
  console.log('== 6. 导入页无粘贴 + 预览弹窗 ==')
  errors = []
  await goto(ws, `${BASE}${PREFIX}/import`)
  await waitText(ws, '导入音浪数据')
  const importText = await evaluate(ws, 'document.body.innerText')
  truthy('页面上没有「粘贴」字样', !importText.includes('粘贴'))
  expect(
    '没有多行文本框',
    await evaluate(ws, `document.querySelectorAll('.page textarea').length`),
    0,
  )

  const input = await ws.send('Runtime.evaluate', {
    expression: `document.querySelector('input[type=file]')`,
  })
  if (!input.result?.objectId) throw new Error('找不到文件输入框')
  await ws.send('DOM.setFileInputFiles', { files: [CSV], objectId: input.result.objectId })

  truthy('弹出预览弹窗', await waitFor(ws, `document.querySelector('.import-pop')`))
  expect(
    '预览行数',
    await evaluate(ws, `document.querySelectorAll('.import-pop tbody tr').length`),
    3,
  )
  const popText = await evaluate(ws, `document.querySelector('.import-pop').innerText`)
  truthy(`预览里带文件名（pk-admin-uicheck.csv）`, popText.includes('pk-admin-uicheck.csv'))
  truthy(`预览第一行是 ${names[0]}`, popText.includes(names[0]))
  /* 弹窗必须整块落在视口里（表格自己滚，不能把确认按钮顶出屏幕） */
  const box = JSON.parse(
    await evaluate(
      ws,
      `(() => { const r = document.querySelector('.import-pop').getBoundingClientRect()
        return JSON.stringify({ right: Math.round(r.right), bottom: Math.round(r.bottom), vw: innerWidth, vh: innerHeight }) })()`,
    ),
  )
  truthy(
    `弹窗在视口内（${box.right}≤${box.vw} · ${box.bottom}≤${box.vh}）`,
    box.right <= box.vw && box.bottom <= box.vh,
  )
  await shot(ws, 'admin-import-preview.png')

  /* Esc 取消 = 关弹窗，不入库 */
  await ws.send('Input.dispatchKeyEvent', {
    type: 'keyDown',
    key: 'Escape',
    code: 'Escape',
    windowsVirtualKeyCode: 27,
    nativeVirtualKeyCode: 27,
  })
  truthy('Esc 关掉预览', await waitFor(ws, `!document.querySelector('.import-pop')`))

  /* ---------- 7. 真入库 → 批次列表 → 删掉当天数据 ---------- */
  console.log(`== 7. 入库 / 清理（业务日期 ${TEST_DATE}）==`)
  const before = await apiGet(`/api/admin/import/day-status?bizDate=${TEST_DATE}&team=male`)
  expect('测试日期入库前没有数据', before.rows ?? 0, 0)

  const input2 = await ws.send('Runtime.evaluate', {
    expression: `document.querySelector('input[type=file]')`,
  })
  await ws.send('DOM.setFileInputFiles', { files: [CSV], objectId: input2.result.objectId })
  await waitFor(ws, `document.querySelector('.import-pop')`)

  /* 改业务日期：React 受控 input 要走原生 setter + input 事件 */
  await evaluate(
    ws,
    `(() => {
      const el = document.querySelector('.import-pop input[type=date]')
      const set = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value').set
      set.call(el, ${JSON.stringify(TEST_DATE)})
      el.dispatchEvent(new Event('input', { bubbles: true }))
      return el.value
    })()`,
  )
  truthy('当日状态查回「还没有数据」', await waitText(ws, '还没有数据', 60))

  await clickByText(ws, '确认导入')
  truthy('导入完成提示出现', await waitText(ws, '导入完成', 60))
  const doneText = await evaluate(ws, `document.querySelector('.alert.good').innerText`)
  truthy(`报告成功 3 行（${doneText.split('\n')[0]}）`, doneText.includes('成功 3 行'))
  truthy('预览弹窗已关闭', !(await evaluate(ws, `!!document.querySelector('.import-pop')`)))

  const after = await apiGet(`/api/admin/import/day-status?bizDate=${TEST_DATE}&team=male`)
  expect('库里当天有 3 行', after.rows, 3)
  truthy(
    '批次列表出现该业务日期',
    await waitText(ws, TEST_DATE, 30),
  )

  /* 收尾：删掉测试日（按钮依赖当日状态，入库后必须能点） */
  const btnOk = await waitFor(ws, `!document.querySelector('button.danger').disabled`)
  if (!btnOk) {
    console.log(
      `    按钮：${await evaluate(ws, `document.querySelector('button.danger')?.outerHTML ?? 'null'`)}`,
    )
    console.log(
      `    提示：${await evaluate(ws, `[...document.querySelectorAll('.alert')].map((a) => a.innerText.replace(/\\n/g, ' ').slice(0, 80)).join(' || ')`)}`,
    )
  }
  truthy('删除按钮可用', btnOk)
  await clickByText(ws, '删除当前日期')
  truthy('删除结果提示出现', await waitText(ws, '已删除', 60))
  const delText = await evaluate(ws, `document.body.innerText.match(/已删除[^\\n]*/)?.[0] ?? ''`)
  truthy(`删除报告：${delText}`, /已删除\s*3\s*条音浪数据/.test(delText))
  const cleaned = await apiGet(`/api/admin/import/day-status?bizDate=${TEST_DATE}&team=male`)
  expect('清理后当天 0 行', cleaned.rows ?? 0, 0)

  /* ---------- 8. 全程没有 console 报错 ---------- */
  console.log('== 8. console 清洁度 ==')
  errors.length ? bad(`有 console 报错：${errors.join(' | ')}`) : ok('全程无 console 报错')
} finally {
  ws?.close()
  chrome.kill('SIGKILL')
  rmSync(PROFILE, { recursive: true, force: true })
}

console.log(failed === 0 ? '\n全部通过 ✅' : `\n${failed} 项失败 ❌`)
process.exit(failed === 0 ? 0 : 1)
