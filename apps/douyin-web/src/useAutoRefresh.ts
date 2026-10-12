import { useEffect, useRef } from 'react'
import { issueTicket } from './api'

/** 后端 events.rs 广播出来的变更 */
export type Change = { scope: string; detail: string; at: string }

type Listener = (c: Change) => void

const listeners = new Set<Listener>()
let source: EventSource | null = null
let sourceToken = ''
let connecting = ''
let reconnectTimer: number | null = null

function parse(data: string): Change {
  try {
    const d = JSON.parse(data) as Partial<Change>
    return { scope: d.scope ?? 'all', detail: d.detail ?? '', at: d.at ?? '' }
  } catch {
    return { scope: 'all', detail: '', at: '' }
  }
}

function dispatch(c: Change) {
  for (const fn of listeners) {
    try {
      fn(c)
    } catch {
      /* 单个订阅者出错不要打断其他人 */
    }
  }
}

/** 建立（或复用）唯一的 SSE 连接。多处 hook 共用一条，避免每页开一条长连接。
 *
 * EventSource 不能自定义请求头，而审计后 query 里**只认一次性 ticket**
 * （`?ticket=`，60 秒、用过即废，见后端 auth.rs）→ 每次建连（含每次重连）都现换一张；
 * 长效 token 只留给 fetch 的 Authorization 头，绝不进 URL。 */
async function ensureConnection(token: string) {
  if ((source && sourceToken === token) || connecting === token) return
  closeConnection()
  connecting = token
  sourceToken = token
  let ticket = ''
  try {
    ticket = await issueTicket()
  } catch {
    connecting = ''
    scheduleReconnect()   // 换不到 ticket（token 失效 / 网络抖动）：交给统一退避重连
    return
  }
  connecting = ''
  // 换 ticket 是异步的：期间可能已退出登录 / 最后一位订阅者已退订
  if (sourceToken !== token || listeners.size === 0) return
  const es = new EventSource(`/api/admin/events?ticket=${encodeURIComponent(ticket)}`)
  es.addEventListener('change', (ev) => dispatch(parse((ev as MessageEvent).data)))
  // 长连接断了（服务重启 / 网络切换）→ 旧 ticket 已废，退避后重新换票再连，
  // 并在连接恢复时补发一条 all，让页面把重连期间漏掉的变更补齐。
  es.addEventListener('error', () => {
    es.close()
    if (source === es) source = null
    sourceToken = ''
    scheduleReconnect()
  })
  source = es
}

/** 退避重连（3 秒一次，同一时刻只排一个） */
function scheduleReconnect() {
  if (reconnectTimer != null) return
  reconnectTimer = window.setTimeout(() => {
    reconnectTimer = null
    const t = sessionStorage.getItem('pk_token')
    if (t && listeners.size > 0) {
      void ensureConnection(t)
      dispatch({ scope: 'all', detail: '连接已恢复', at: '' })
    }
  }, 3000)
}

function closeConnection() {
  source?.close()
  source = null
  sourceToken = ''
  connecting = ''
}

/* 离开页面（含进 bfcache）就断开 SSE，回来再连。
 *
 * 为什么必须断：浏览器对同一个源（HTTP/1.1）只有 6 条并发连接，每条 SSE 长连接占一条。
 * 后台要在 7 个页面之间来回切，旧文档若被 bfcache 留着、连接没关，连接数很快被占满 ——
 * 表现是「切几个页面后再导入，点确认毫无反应」：请求排在队里发不出去，页面也不报错。
 * 生产走 HTTPS/HTTP2 能多路复用，但本地 dev（http/1.1）和没上 h2 的代理都会踩。 */
if (typeof window !== 'undefined') {
  window.addEventListener('pagehide', () => closeConnection())
  window.addEventListener('pageshow', (e) => {
    // bfcache 恢复：上面已把连接关掉，按当前登录态补连
    if (!e.persisted) return
    const token = sessionStorage.getItem('pk_token')
    if (token && listeners.size > 0) void ensureConnection(token)
  })
}

/**
 * 订阅数据变更。返回退订函数。
 * 供 App 层做全局提示用；页面级自动刷新请用 useAutoRefresh。
 */
export function subscribeChanges(fn: Listener): () => void {
  listeners.add(fn)
  const token = sessionStorage.getItem('pk_token')
  if (token) void ensureConnection(token)
  return () => {
    listeners.delete(fn)
    if (listeners.size === 0) {
      if (reconnectTimer != null) {
        clearTimeout(reconnectTimer)
        reconnectTimer = null
      }
      closeConnection()
    }
  }
}

/**
 * 页面级自动刷新：后端有对应 scope 的变更时重跑 reload。
 *
 *   const load = useCallback(...)
 *   useAutoRefresh(load, ['import', 'roster'])
 *
 * scopes 省略 = 任何变更都刷新。scope 取值：import / roster / plans /
 * settings / source / bot / all（见后端 events.rs）。
 */
export function useAutoRefresh(reload: () => void, scopes?: string[]) {
  // 用 ref 存回调：调用方常传内联写法，放进依赖会让连接反复重建
  const cbRef = useRef(reload)
  cbRef.current = reload

  // scopes 一般是字面量数组，每次渲染都是新对象；拼成字符串再当依赖
  const key = scopes?.join(',') ?? ''

  useEffect(() => {
    const wanted = key ? key.split(',') : []
    return subscribeChanges((c) => {
      if (wanted.length === 0 || c.scope === 'all' || wanted.includes(c.scope)) {
        cbRef.current()
      }
    })
  }, [key])
}
