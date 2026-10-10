import { useCallback, useEffect, useState } from 'react'
import { api } from './api'
import { useAutoRefresh } from './useAutoRefresh'

/* 访客日志（2026-10-10）：展示端只对河南 / 安徽开放，这里是所有访问 / 登录记录。
   登录了带账号；没登录只记 IP；外省被拦的访问同样入库，用于排查「谁被挡住了」。 */

type Visit = {
  id: number
  kind: string
  ip: string
  province: string
  city: string
  isp: string
  source: string
  team: string
  path: string
  account: string
  allowed: boolean
  loginOk: boolean
  createdAt: string
}

type Stats = {
  all: number
  today: number
  todayUniq: number
  todayAllowed: number
  todayBlocked: number
  todayLogins: number
}

const KIND_LABEL: Record<string, string> = {
  api: '访问',
  login: '登录',
  visit: '访问',
}

const TEAM_LABEL: Record<string, string> = {
  male: '男团',
  female: '女团',
}

const place = (v: Visit) => {
  const p = [v.province, v.city].filter(Boolean).join(' · ')
  return p || '未知'
}

export default function VisitPage() {
  const [items, setItems] = useState<Visit[]>([])
  const [stats, setStats] = useState<Stats | null>(null)
  const [onlyBlocked, setOnlyBlocked] = useState(false)
  const [draft, setDraft] = useState('')
  const [q, setQ] = useState('')
  const [error, setError] = useState('')

  const load = useCallback(() => {
    const params = new URLSearchParams({ limit: '300' })
    if (onlyBlocked) params.set('blocked', '1')
    if (q) params.set('q', q)
    api(`/api/admin/visits?${params}`)
      .then((d) => {
        setItems(d.items ?? [])
        setStats(d.stats ?? null)
        setError('')
      })
      .catch((e) => setError(e.message ?? String(e)))
  }, [onlyBlocked, q])

  useEffect(load, [load])
  useAutoRefresh(load, ['import', 'roster'])

  return (
    <div className="page">
      <h1>访客日志</h1>
      <p className="hint">
        展示端只对河南 / 安徽开放（后端按 IP 归属地判定，多数据源投票）。
        这里记录所有访问与登录：登录了带账号，没登录只记 IP；外省被拦的访问也会入库。
      </p>

      {stats && (
        <div className="cards">
          <div className="card kpi">
            <span className="hint">今日访问</span>
            <b>{stats.today}</b>
            <span className="hint">累计 {stats.all} 条</span>
          </div>
          <div className="card kpi k-violet">
            <span className="hint">今日独立 IP</span>
            <b>{stats.todayUniq}</b>
            <span className="hint">按 IP 去重</span>
          </div>
          <div className="card kpi k-good">
            <span className="hint">今日放行</span>
            <b>{stats.todayAllowed}</b>
            <span className="hint">河南 / 安徽</span>
          </div>
          <div className="card kpi k-warn">
            <span className="hint">今日拦截</span>
            <b>{stats.todayBlocked}</b>
            <span className="hint">外省 / 查不到归属</span>
          </div>
          <div className="card kpi">
            <span className="hint">今日登录</span>
            <b>{stats.todayLogins}</b>
            <span className="hint">含失败尝试</span>
          </div>
        </div>
      )}

      <div className="row" style={{ marginBottom: 12 }}>
        <button
          className={onlyBlocked ? 'primary' : 'ghost'}
          onClick={() => setOnlyBlocked((v) => !v)}
        >
          {onlyBlocked ? '只看拦截中（点掉看全部）' : '只看拦截'}
        </button>
        <form
          style={{ display: 'flex', gap: 8, marginLeft: 12 }}
          onSubmit={(e) => {
            e.preventDefault()
            setQ(draft.trim())
          }}
        >
          <input
            placeholder="搜 IP / 账号"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            style={{ width: 200 }}
          />
          <button className="ghost" type="submit">
            查询
          </button>
          {(q || draft) && (
            <button
              className="ghost"
              type="button"
              onClick={() => {
                setDraft('')
                setQ('')
              }}
            >
              清空
            </button>
          )}
        </form>
      </div>

      {error && <div className="alert bad">{error}</div>}

      <div className="card">
        {items.length === 0 ? (
          <p className="hint">
            {onlyBlocked || q ? '没有符合条件的记录。' : '还没有访问记录。'}
          </p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>时间</th>
                <th>类型</th>
                <th>IP</th>
                <th>归属地</th>
                <th>运营商</th>
                <th>结果</th>
                <th>账号</th>
                <th>团别</th>
                <th>路径</th>
              </tr>
            </thead>
            <tbody>
              {items.map((v) => (
                <tr key={v.id}>
                  <td>{v.createdAt}</td>
                  <td>{KIND_LABEL[v.kind] ?? v.kind}</td>
                  <td>{v.ip || '—'}</td>
                  <td title={v.source ? `来源：${v.source}` : undefined}>{place(v)}</td>
                  <td>{v.isp || '—'}</td>
                  <td>
                    {v.allowed ? (
                      <span className="badge b-active">放行</span>
                    ) : (
                      <span className="badge b-left">拦截</span>
                    )}
                    {v.kind === 'login' && (
                      <span className={v.loginOk ? 'badge b-active' : 'badge b-paused'}>
                        {v.loginOk ? '成功' : '失败'}
                      </span>
                    )}
                  </td>
                  <td>{v.account || '—'}</td>
                  <td>{TEAM_LABEL[v.team] ?? (v.team || '—')}</td>
                  <td title={v.path}>{v.path || '—'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  )
}
