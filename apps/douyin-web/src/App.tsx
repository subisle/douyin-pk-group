import { useEffect, useState } from 'react'
import { hasToken, login, logout } from './api'
import { subscribeChanges } from './useAutoRefresh'
import { ROUTES, routeHref, useRoute } from './lib/router'
import DashboardPage from './DashboardPage'
import ImportPage from './ImportPage'
import StreamerPage from './StreamerPage'
import ClaimPage from './ClaimPage'
import LineagePage from './LineagePage'
import BotPage from './BotPage'
import GroupPage from './pk/GroupPage'
import './pk/pk.css'

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
    <div>
      <header className="topbar">
        <span className="brand">
          <span className="brand-mark" aria-hidden />
          抖音数据管理
          <span className="brand-sub">鹏仔传媒 · 运营后台</span>
        </span>
        <nav>
          {ROUTES.map((t) => (
            /* 真链接（不是 button）：能中键/右键新开标签、能复制地址、刷新不丢页；
               普通左键交给 history.pushState，避免整页重载。 */
            <a
              key={t.key}
              className={route === t.key ? 'tab active' : 'tab'}
              href={routeHref(t.key)}
              aria-current={route === t.key ? 'page' : undefined}
              onClick={(e) => {
                if (e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return
                e.preventDefault()
                go(t.key)
              }}
            >
              {t.label}
            </a>
          ))}
        </nav>
        <button
          className="tab ghost"
          onClick={() => {
            logout()
            setAuthed(false)
          }}
        >
          退出
        </button>
      </header>
      {route === 'data' && <DashboardPage />}
      {route === 'import' && <ImportPage />}
      {route === 'streamers' && <StreamerPage />}
      {route === 'claims' && <ClaimPage />}
      {route === 'lineage' && <LineagePage />}
      {route === 'group' && <GroupPage />}
      {route === 'bots' && <BotPage />}
      {toast && (
        <div className="toast" role="status">
          <span className="toast-dot" aria-hidden />
          数据已更新 · {toast}
        </div>
      )}
    </div>
  )
}
