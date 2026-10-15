const BASE = ''

let token: string | null = sessionStorage.getItem('pk_token')

export function hasToken() {
  return !!token
}

export function setToken(t: string | null) {
  token = t
  if (t) sessionStorage.setItem('pk_token', t)
  else sessionStorage.removeItem('pk_token')
}

/** token 失效广播事件（迁移自 apps/web 冻结版 api.ts + AuthGate 的机制）：
 *  token 过期 / 后端重启导致 401 时，api() 清掉本地 token 并广播，
 *  App.tsx 监听后把整站切回登录门，避免每页都满屏红字、要手动点「退出」。 */
export const UNAUTHORIZED_EVENT = 'pk:unauthorized'

export class ApiError extends Error {
  status: number
  constructor(status: number, message: string) {
    super(message)
    this.status = status
  }
}

export async function api<T = any>(path: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers)
  if (token) headers.set('Authorization', `Bearer ${token}`)
  if (init?.body && !(init.body instanceof FormData) && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json')
  }
  const resp = await fetch(BASE + path, { ...init, headers })
  if (resp.status === 401) {
    setToken(null)
    // 广播给 App：清登录态（login 接口自己报 401 时不广播，避免登录页重复抖动）
    if (!path.startsWith('/api/auth/login')) {
      window.dispatchEvent(new CustomEvent(UNAUTHORIZED_EVENT))
    }
    throw new ApiError(401, '需要管理员口令')
  }
  const text = await resp.text()
  const data = text ? JSON.parse(text) : {}
  if (!resp.ok) {
    throw new ApiError(resp.status, data?.error ?? `请求失败（HTTP ${resp.status}）`)
  }
  return data as T
}

/**
 * 换一次性 ticket（60 秒有效、用过即焚）。
 *
 * 审计后长效 token **只能走 Authorization 头**，query 里只认 `?ticket=`
 * （见后端 auth.rs）。EventSource 这类放不进请求头的场景必须先换 ticket；
 * `<img>` / `<a>` 能改成 fetch 的优先走 fetch + Authorization（见 DashboardPage）。
 */
export async function issueTicket(): Promise<string> {
  const d = await api<{ ticket: string }>('/api/auth/ticket', { method: 'POST' })
  return d.ticket
}

export async function login(password: string) {
  const data = await api<{ token: string; enabled: boolean }>('/api/auth/login', {
    method: 'POST',
    body: JSON.stringify({ password }),
  })
  setToken(data.token)
  return data
}

export function logout() {
  api('/api/auth/logout', { method: 'POST' }).catch(() => {})
  setToken(null)
}

/* ---------------- 管理员消息下发（docs/23 §5.5，/api/admin/send/*） ----------------
 * 鉴权与现有 admin api 相同：api() 自动带 Authorization: Bearer token。
 * 圈人只传「意图」（docs/23 §5.2），展开与校验都在服务端做，前端不给全量 id 兜底。 */

/** 收件人候选（GET /api/admin/send/targets?q=） */
export type SendTarget = {
  personId: number
  name: string
  team: string
  masterId: number | null
  masterName: string | null
  hasWx: boolean
  hasPopup: boolean
  discipleCount: number
}

/** 圈人意图：persons=手工多选 / masters=按师傅（includeMaster 默认 true，师傅本人也收）/ team=按团批量 / all=全部 */
export type SendTargetSpec =
  | { type: 'persons'; ids: number[] }
  | { type: 'masters'; ids: number[]; includeMaster?: boolean }
  | { type: 'team'; team: 'male' | 'female' }
  | { type: 'all' }

export type SendChannel = 'wxbot' | 'popup'

/** POST /api/admin/send 请求体 */
export type SendCreateReq = {
  title: string
  content: string
  imageKind: 'none' | 'lineage'
  imageDate?: string
  channels: SendChannel[]
  target: SendTargetSpec
  /** task-14 上传图：裸 base64 或 dataURL 都行，后端按**文件头魔数**校验（当前只收 PNG）。
   * **未选图时不要带这个字段** —— 缺字段 = 没带图；带空串会被后端判 400「图片内容为空」。 */
  imageBase64?: string
  /** 可选：声明 mime（后端以魔数为准，只作一致性校验） */
  imageMime?: string
  /** 可选：原始文件名（只读回显 / 审计用） */
  imageName?: string
}

export type SendCreateResp = { ok: boolean; messageId: number; recipients: number }

/** 发送计数（历史列表与详情共用） */
export type SendCounts = { sent: number; failed: number; skipped: number; pending: number }

/** 消息投影（admin_messages，docs/23 §5.3；channels 后端为逗号分隔串，容忍数组返回） */
export type SendMessage = {
  id: number
  title: string
  content: string
  imageKind: string
  imageDate?: string | null
  createdAt: string
  channels: string | string[]
  targetType: string
  counts: SendCounts
  /** task-14 上传图只读字段：imageKind = "custom" 时才有值（渲染图 / 纯文本消息为空或 0） */
  imageMime?: string
  imageName?: string
  imageBytes?: number
  hasImage?: boolean
}

/** outbox 行：一条消息 × 每收件人 × 每渠道 = 一行，失败互不影响 */
export type SendOutboxRow = {
  id: number
  recipientName: string
  channel: string
  status: 'pending' | 'sent' | 'failed' | 'skipped'
  error: string
  sentAt: string | null
  readAt: string | null
}

export type SendMessageDetail = { message: SendMessage; outbox: SendOutboxRow[] }

/** 收件人候选（q 为空拉全量，供批量计数与展开徒弟用） */
export async function fetchSendTargets(q: string): Promise<{ items: SendTarget[] }> {
  const key = q.trim()
  return api(`/api/admin/send/targets${key ? `?q=${encodeURIComponent(key)}` : ''}`)
}

/** 创建消息 + outbox 并启动 worker（立即返回，页面跳到发送详情） */
export async function createSend(body: SendCreateReq): Promise<SendCreateResp> {
  return api('/api/admin/send', { method: 'POST', body: JSON.stringify(body) })
}

/** 发送历史（最近 limit 条，含四态计数） */
export async function fetchSendMessages(limit = 50): Promise<{ items: SendMessage[] }> {
  return api(`/api/admin/send/messages?limit=${limit}`)
}

/** 发送详情：每收件人每渠道状态（页面 3s 轮询直到无 pending） */
export async function fetchSendMessageDetail(id: number): Promise<SendMessageDetail> {
  return api(`/api/admin/send/messages/${id}`)
}

/** 一键为所有师傅（有徒弟的主播）开启师门日报，返回影响行数 */
export async function postEnableLineage(): Promise<{ ok: boolean; enabled: number }> {
  return api('/api/admin/send/enable-lineage', { method: 'POST' })
}
