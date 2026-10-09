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
    throw new ApiError(401, '需要管理员口令')
  }
  const text = await resp.text()
  const data = text ? JSON.parse(text) : {}
  if (!resp.ok) {
    throw new ApiError(resp.status, data?.error ?? `请求失败（HTTP ${resp.status}）`)
  }
  return data as T
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
