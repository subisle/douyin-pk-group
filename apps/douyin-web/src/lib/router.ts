/* 管理端 history 路由：每个导航项都是一个真实地址，刷新 / 前进后退 / 分享链接
 * 都落在同一页。
 *
 * - 生产：pk-server 的 spa.rs 把 `/admin` 与 `/admin/*` 剥掉前缀后按 dist 解析
 *   （`/admin/group` 这种深层链接也回退到 index.html），所以前端可以放心用
 *   history 路由。
 * - 开发：vite dev 直接挂在根（`/group`），base 自动识别为空串。
 *
 * 不引第三方路由库：页面就 7 个常量路径，手写一份够用也好读。
 */
import { useCallback, useEffect, useState } from 'react'

/** 管理端部署前缀（生产由 spa.rs 分流到后台 dist） */
export const ADMIN_BASE = '/admin'

export type RouteKey =
  | 'data'
  | 'import'
  | 'streamers'
  | 'claims'
  | 'visits'
  | 'lineage'
  | 'group'
  | 'bots'

export const DEFAULT_ROUTE: RouteKey = 'data'

/** 导航顺序 = 页面顺序，path 是 base 之后的部分 */
export const ROUTES: { key: RouteKey; path: string; label: string }[] = [
  { key: 'data', path: '/data', label: '数据概况' },
  { key: 'import', path: '/import', label: '数据导入' },
  { key: 'streamers', path: '/streamers', label: '主播管理' },
  { key: 'claims', path: '/claims', label: '抖音号审计' },
  { key: 'visits', path: '/visits', label: '访客日志' },
  { key: 'lineage', path: '/lineage', label: '族谱' },
  { key: 'group', path: '/group', label: 'PK 分组' },
  { key: 'bots', path: '/bots', label: '机器人' },
]

const PATHS = new Map<RouteKey, string>(ROUTES.map((r) => [r.key, r.path]))
const KEYS = new Map<string, RouteKey>(ROUTES.map((r) => [r.path, r.key]))

/** 当前站点的 base：pathname 落在 /admin 下才是 /admin，开发态为空串 */
export function detectBase(pathname: string = window.location.pathname): string {
  return pathname === ADMIN_BASE || pathname.startsWith(`${ADMIN_BASE}/`) ? ADMIN_BASE : ''
}

/** pathname → 路由 key；认不出返回 null（由调用方兜底到默认页） */
export function readRoute(pathname: string = window.location.pathname): RouteKey | null {
  const base = detectBase(pathname)
  let rest = base ? pathname.slice(base.length) : pathname
  if (!rest || rest === '/') return DEFAULT_ROUTE
  rest = rest.replace(/\/+$/, '') || '/'
  return KEYS.get(rest) ?? null
}

/** 路由 → 可点、可复制、可新开标签的完整地址（自动带上部署 base） */
export function routeHref(key: RouteKey, base: string = detectBase()): string {
  return `${base}${PATHS.get(key) ?? PATHS.get(DEFAULT_ROUTE)!}`
}

/** 认不出的路径（旧书签 / 手输错的地址）replace 到默认页，别让地址栏留着垃圾路径 */
function resolveRoute(): RouteKey {
  const route = readRoute()
  if (route) return route
  window.history.replaceState(null, '', routeHref(DEFAULT_ROUTE))
  return DEFAULT_ROUTE
}

/** 当前路由 + 跳转。跳转走 pushState，所以浏览器前进后退能原样回到上一页。 */

/* ---------------- 切页守卫（P1-12） ----------------
 * 页面（PK 分组看板）可以注册一个守卫：切走前有未保存的变更就返回提示文案，
 * 由 useRoute 的 go() 在真正跳转前 confirm。没有确认就卸载，拖半小时的看板会归零。
 * 只拦「本应用内的导航点击」；浏览器刷新/关标签用 beforeunload（页面自己挂）。 */
type LeaveGuard = () => string | null;
const leaveGuards = new Set<LeaveGuard>();

/** 注册切页守卫，返回注销函数。守卫返回提示文案 = 有未保存变更需要确认。 */
export function registerLeaveGuard(guard: LeaveGuard): () => void {
  leaveGuards.add(guard);
  return () => leaveGuards.delete(guard);
}

function confirmLeave(): boolean {
  for (const guard of leaveGuards) {
    const msg = guard();
    if (msg && !window.confirm(`${msg}\n\n确定要离开吗？未保存的内容会丢失。`)) return false;
  }
  return true;
}

export function useRoute(): { route: RouteKey; go: (key: RouteKey) => void } {
  const [route, setRoute] = useState<RouteKey>(() => resolveRoute())

  useEffect(() => {
    const onPop = () => setRoute(resolveRoute())
    window.addEventListener('popstate', onPop)
    return () => window.removeEventListener('popstate', onPop)
  }, [])

  const go = useCallback((key: RouteKey) => {
    if (!confirmLeave()) return;
    const href = routeHref(key)
    const now = `${window.location.pathname}${window.location.search}`
    if (now !== href) window.history.pushState(null, '', href)
    setRoute(key)
    /* pushState 自己不触发 popstate：页面里可能不止一个 useRoute（例如数据页空状态里的
       「去导入」按钮），补发一发，让所有实例都跟着地址走，避免「地址变了页面没换」。 */
    window.dispatchEvent(new PopStateEvent('popstate'))
    window.scrollTo(0, 0)
  }, [])

  return { route, go }
}
