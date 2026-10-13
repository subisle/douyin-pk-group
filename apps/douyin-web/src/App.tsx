import { useEffect, useState } from 'react'
import { hasToken, login, logout, UNAUTHORIZED_EVENT } from './api'
import { subscribeChanges } from './useAutoRefresh'
import { ROUTES, routeHref, useRoute, type RouteKey } from './lib/router'
import DashboardPage from './DashboardPage'
import ImportPage from './ImportPage'
import StreamerPage from './StreamerPage'
import ClaimPage from './ClaimPage'
import VisitPage from './VisitPage'
import LineagePage from './LineagePage'
import AnnouncePage from './AnnouncePage'
import BotPage from './BotPage'
import GroupPage from './pk/GroupPage'
import './pk/pk.css'

/** 侧边栏导航图标（emoji，零依赖；与 router.ts 的 RouteKey 一一对应） */
const NAV_ICONS: Record<RouteKey, string> = {
  data: '📊',
  import: '📥',
  streamers: '👤',
  claims: '🔗',
  visits: '👁️',
  lineage: '🌳',
  group: '⚔️',
  announce: '📣',
  bots: '🤖',
}

export default function App() {
  const [authed, setAuthed] = useState(hasToken())
  const [password, setPassword] = useState('')
  const [error, setError] = useState('')
  const [toast, setToast] = useState('')
  /** 当前页面由 URL 决定（/admin/data、/admin/import …），不再是页内 state */
  const { route, go } = useRoute()

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
      <aside className="sidebar">
        <div className="side-brand">
          <span className="brand-mark" aria-hidden />
          <div className="side-brand-text">
            <div className="side-title">抖音数据管理</div>
            <div className="side-sub">鹏仔传媒 · 运营后台</div>
          </div>
        </div>
        <nav className="side-nav">
          {ROUTES.map((t) => (
            /* 真链接（不是 button）：能中键/右键新开标签、能复制地址、刷新不丢页；
               普通左键交给 history.pushState，避免整页重载。 */
            <a
              key={t.key}
              className={route === t.key ? 'side-link active' : 'side-link'}
              href={routeHref(t.key)}
              aria-current={route === t.key ? 'page' : undefined}
              onClick={(e) => {
                if (e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return
                e.preventDefault()
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
      <main className="main">
        {route === 'data' && <DashboardPage />}
        {route === 'import' && <ImportPage />}
        {route === 'streamers' && <StreamerPage />}
        {route === 'claims' && <ClaimPage />}
        {route === 'visits' && <VisitPage />}
        {route === 'lineage' && <LineagePage />}
        {route === 'group' && <GroupPage />}
        {route === 'announce' && <AnnouncePage />}
        {route === 'bots' && <BotPage />}
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
