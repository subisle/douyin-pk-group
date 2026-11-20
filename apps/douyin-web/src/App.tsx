import { lazy, Suspense, useEffect, useRef, useState } from 'react'
import DashboardPage from './DashboardPage'
import { hasToken, login, logout, UNAUTHORIZED_EVENT } from './api'
import { subscribeChanges } from './useAutoRefresh'
import { ROUTES, routeHref, useRoute, type RouteKey } from './lib/router'

/* 路由级代码分割（2026-10-17）：各页面懒加载，首屏只拉后台壳 + 数据概况页，
 * 其余页（主播管理 / 导入 / 族谱 / 公告 / 机器人 / 消息通知 / PK）切到才加载。
 * pk.css 也从 App 移到 GroupPage，只有进 PK 页才拉。
 * 之前 10 个页面全打进一个 449KB 的 bundle，首屏一次全下载。 */
const ImportPage = lazy(() => import('./ImportPage'))
const StreamerPage = lazy(() => import('./StreamerPage'))
const VisitPage = lazy(() => import('./VisitPage'))
const LineagePage = lazy(() => import('./LineagePage'))
const AnnouncePage = lazy(() => import('./AnnouncePage'))
const BotPage = lazy(() => import('./BotPage'))
const SendPage = lazy(() => import('./SendPage'))
const LiveMonitorPage = lazy(() => import('./live/LiveMonitorPage'))
const GroupPage = lazy(() => import('./pk/GroupPage'))

/** 移动端导航断点：与 styles.css 的移动外壳保持一致。 */
const MOBILE_NAV_QUERY = '(max-width: 760px)'

/** 侧边栏导航图标（emoji，零依赖；与 router.ts 的 ROUTES 一一对应）。 */
const NAV_ICONS: Record<RouteKey, string> = {
  data: '📊',
  import: '📥',
  streamers: '👤',
  visits: '👁️',
  lineage: '🌳',
  group: '⚔️',
  announce: '📣',
  bots: '🤖',
  send: '📢',
  live: '📡',
}

export default function App() {
  return <AdminApp />
}

function AdminApp() {
  const [authed, setAuthed] = useState(hasToken())
  const [password, setPassword] = useState('')
  const [error, setError] = useState('')
  const [toast, setToast] = useState('')
  const [navOpen, setNavOpen] = useState(false)
  const [isMobileNav, setIsMobileNav] = useState(() =>
    typeof window !== 'undefined' && window.matchMedia(MOBILE_NAV_QUERY).matches,
  )
  const menuButtonRef = useRef<HTMLButtonElement>(null)
  const drawerCloseRef = useRef<HTMLButtonElement>(null)
  const activeNavRef = useRef<HTMLAnchorElement>(null)
  /** 当前页面由 URL 决定（/admin/data、/admin/import …），不再是页内 state */
  const { route, go } = useRoute()
  const activeRoute = ROUTES.find((r) => r.key === route)

  useEffect(() => {
    const mq = window.matchMedia(MOBILE_NAV_QUERY)
    const sync = () => {
      setIsMobileNav(mq.matches)
      if (!mq.matches) setNavOpen(false)
    }
    sync()
    mq.addEventListener?.('change', sync)
    return () => mq.removeEventListener?.('change', sync)
  }, [])

  useEffect(() => {
    setNavOpen(false)
  }, [route])

  useEffect(() => {
    if (!navOpen || !isMobileNav) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return
      setNavOpen(false)
      menuButtonRef.current?.focus()
    }
    document.body.classList.add('admin-nav-open')
    drawerCloseRef.current?.focus()
    requestAnimationFrame(() => activeNavRef.current?.scrollIntoView({ block: 'center' }))
    window.addEventListener('keydown', onKey)
    return () => {
      document.body.classList.remove('admin-nav-open')
      window.removeEventListener('keydown', onKey)
    }
  }, [navOpen, isMobileNav])

  function closeNav(returnFocus = false) {
    setNavOpen(false)
    if (returnFocus) menuButtonRef.current?.focus()
  }

  /* 全局变更提示：机器人导入、台账变更、数据源切换这类「不是本页操作触发」的更新，
     弹一条短提示，让用户知道页面已经自己刷新过了（页面数据由各页的
     useAutoRefresh 负责重拉）。 */
  useEffect(() => {
    if (!authed) return
    let timer: number | null = null
    const unsub = subscribeChanges((c) => {
      if (!c.detail) return
      if (!['import', 'roster', 'source', 'bot'].includes(c.scope)) return
      setToast(c.detail)
      if (timer != null) window.clearTimeout(timer)
      timer = window.setTimeout(() => setToast(''), 4000)
    })
    return () => {
      unsub()
      if (timer != null) window.clearTimeout(timer)
    }
  }, [authed])

  /** token 失效（401）时整站切回登录门（P1-11）：
   *  任何一页的请求收到 401，api() 都会广播这个事件，这里把登录态清掉，
   *  不再让用户面对满屏红字还要手动点「退出」。 */
  useEffect(() => {
    const onUnauthorized = () => {
      setAuthed(false)
      setPassword('')
      setError('')
    }
    window.addEventListener(UNAUTHORIZED_EVENT, onUnauthorized)
    return () => window.removeEventListener(UNAUTHORIZED_EVENT, onUnauthorized)
  }, [])

  async function doLogin() {
    setError('')
    try {
      await login(password)
      setPassword('')
      setAuthed(true)
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  if (!authed) {
    return (
      <div className="login-wrap">
        <div className="card login">
          <div className="brand-mark" aria-hidden />
          <h1>抖音数据管理</h1>
          <p className="hint">内部工具 · 需要管理员口令</p>
          <input
            type="password"
            placeholder="管理员口令"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && doLogin()}
          />
          <button className="primary" disabled={!password.trim()} onClick={doLogin}>
            登 录
          </button>
          {error && <div className="alert bad">{error}</div>}
        </div>
      </div>
    )
  }

  return (
    <div className="app-shell">
      <aside
        id="admin-navigation"
        className={navOpen ? 'sidebar is-open' : 'sidebar'}
        aria-hidden={isMobileNav ? !navOpen : undefined}
      >
        <div className="side-brand">
          <span className="brand-mark" aria-hidden />
          <div className="side-brand-text">
            <div className="side-title">抖音数据管理</div>
            <div className="side-sub">鹏仔传媒 · 运营后台</div>
          </div>
          <button
            ref={drawerCloseRef}
            className="mobile-drawer-close"
            type="button"
            aria-label="关闭菜单"
            onClick={() => closeNav(true)}
          >
            ×
          </button>
        </div>
        <nav className="side-nav">
          {ROUTES.map((t) => (
            /* 真链接（不是 button）：能中键/右键新开标签、能复制地址、刷新不丢页；
               普通左键交给 history.pushState，避免整页重载。 */
            <a
              key={t.key}
              ref={route === t.key ? activeNavRef : undefined}
              className={route === t.key ? 'side-link active' : 'side-link'}
              href={routeHref(t.key)}
              tabIndex={isMobileNav && !navOpen ? -1 : undefined}
              aria-current={route === t.key ? 'page' : undefined}
              onClick={(e) => {
                if (e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return
                e.preventDefault()
                closeNav()
                go(t.key)
              }}
            >
              <span className="side-ico" aria-hidden>
                {NAV_ICONS[t.key]}
              </span>
              <span className="side-label">{t.label}</span>
            </a>
          ))}
        </nav>
        <div className="side-foot">
          <button
            className="side-link"
            onClick={() => {
              closeNav()
              logout()
              setAuthed(false)
            }}
          >
            <span className="side-ico" aria-hidden>
              ↪
            </span>
            <span className="side-label">退出登录</span>
          </button>
        </div>
      </aside>
      <button
        className="mobile-nav-backdrop"
        type="button"
        aria-label="关闭菜单"
        tabIndex={navOpen ? 0 : -1}
        onClick={() => closeNav(true)}
      />
      <main className="main">
        <header className="mobile-bar">
          <button
            ref={menuButtonRef}
            className="mobile-menu-btn"
            type="button"
            aria-label={navOpen ? '关闭菜单' : '打开菜单'}
            aria-expanded={navOpen}
            aria-controls="admin-navigation"
            onClick={() => setNavOpen((open) => !open)}
          >
            <span aria-hidden>{navOpen ? '×' : '☰'}</span>
          </button>
          <div className="mobile-bar-copy" aria-live="polite">
            <span>鹏仔传媒 · 运营后台</span>
            <strong>{activeRoute?.label ?? '后台管理'}</strong>
          </div>
        </header>
        <Suspense fallback={<div className="route-loading">加载中…</div>}>
          {route === 'data' && <DashboardPage />}
          {route === 'import' && <ImportPage />}
          {route === 'streamers' && <StreamerPage />}
          {route === 'visits' && <VisitPage />}
          {route === 'lineage' && <LineagePage />}
          {route === 'group' && <GroupPage />}
          {route === 'announce' && <AnnouncePage />}
          {route === 'bots' && <BotPage />}
          {route === 'send' && <SendPage />}
          {route === 'live' && <LiveMonitorPage />}
        </Suspense>
      </main>
      {toast && (
        <div className="toast" role="status">
          <span className="toast-dot" aria-hidden />
          数据已更新 · {toast}
        </div>
      )}
    </div>
  )
}
