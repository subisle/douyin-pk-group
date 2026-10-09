import { useEffect, useRef } from 'react'

/** 后端 events.rs 广播出来的变更 */
export type Change = { scope: string; detail: string; at: string }

type Listener = (c: Change) => void

const listeners = new Set<Listener>()
let source: EventSource | null = null
let sourceToken = ''
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

/** 建立（或复用）唯一的 SSE 连接。多处 hook 共用一条，避免每页开一条长连接。 */
function ensureConnection(token: string) {
  if (source && sourceToken === token) return
  closeConnection()
  sourceToken = token
  // EventSource 不能自定义请求头，token 走查询参数
  //（后端 require_admin 已支持 ?access_token=，日报图也是这么用的）
  const es = new EventSource(`/api/admin/events?access_token=${encodeURIComponent(token)}`)
  es.addEventListener('change', (ev) => dispatch(parse((ev as MessageEvent).data)))
  // 服务重启会让旧 token 失效 → 401 → 浏览器会无限重试。这里退避重连，
  // 并在连接恢复时补发一条 all，让页面把重连期间漏掉的变更补齐。
  es.addEventListener('error', () => {
    es.close()
    if (source === es) source = null
    if (reconnectTimer != null) return
    reconnectTimer = window.setTimeout(() => {
      reconnectTimer = null
      const t = sessionStorage.getItem('pk_token')
      if (t && listeners.size > 0) {
        ensureConnection(t)
        dispatch({ scope: 'all', detail: '连接已恢复', at: '' })
      }
    }, 3000)
  })
  source = es
}

function closeConnection() {
  source?.close()
  source = null
  sourceToken = ''
}

/**
 * 订阅数据变更。返回退订函数。
 * 供 App 层做全局提示用；页面级自动刷新请用 useAutoRefresh。
 */
export function subscribeChanges(fn: Listener): () => void {
  listeners.add(fn)
  const token = sessionStorage.getItem('pk_token')
  if (token) ensureConnection(token)
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
