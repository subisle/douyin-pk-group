import { useCallback, useEffect, useState } from 'react'
import { api, ApiError } from './api'
import Pager, { usePager } from './Pager'
import { useAutoRefresh } from './useAutoRefresh'

/* 访客日志（2026-10-10）：展示端只对河南 / 安徽 / 江苏开放，这里是所有访问 / 登录记录。
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

/* IP 查询（2026-10-11）：多数据源并列展示，sources 里失败的行置灰，方便排查哪家源挂了 */
type LookupSource = {
  name: string
  ok: boolean
  province: string
  city: string
  isp: string
}

type Lookup = {
  ip: string
  province: string
  city: string
  isp: string
  source: string
  allowed: boolean
  sources: LookupSource[]
}

type AllowlistItem = {
  id: number
  ip: string
  note: string
  createdAt: string
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

const hitSources = (d: Lookup) => {
  const ok = d.sources.filter((s) => s.ok).map((s) => s.name).join('、')
  return d.source || ok || '—'
}

export default function VisitPage() {
  const [items, setItems] = useState<Visit[]>([])
  const [stats, setStats] = useState<Stats | null>(null)
  const [onlyBlocked, setOnlyBlocked] = useState(false)
  const [draft, setDraft] = useState('')
  const [q, setQ] = useState('')
  const [error, setError] = useState('')
  const [ipDraft, setIpDraft] = useState('')
  const [looking, setLooking] = useState(false)
  const [lookup, setLookup] = useState<Lookup | null>(null)
  const [lookupError, setLookupError] = useState('')
  const [allowlist, setAllowlist] = useState<AllowlistItem[]>([])
  const [wlActive, setWlActive] = useState(false)
  const [wlDraft, setWlDraft] = useState('')
  const [wlNote, setWlNote] = useState('')
  const [wlBusy, setWlBusy] = useState(false)
  const [wlError, setWlError] = useState('')

  /* 客户端分页（2026-10-16 表格规范）：接口一次给 300 条，页面切片渲染。
     第二参传筛选条件组合串 —— 「只看拦截」或搜索词一变就回第 1 页。 */
  const pager = usePager(items.length, `${onlyBlocked ? 'blocked' : 'all'}|${q}`)

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

  const lookupIp = useCallback((raw: string) => {
    const ip = raw.trim()
    if (!ip) return
    setLooking(true)
    setLookupError('')
    api<Lookup>(`/api/admin/ip-lookup?ip=${encodeURIComponent(ip)}`)
      .then((d) => {
        setLookup(d)
        setLookupError('')
      })
      .catch((e) => {
        setLookup(null)
        // 400 等 HTTP 错误带后端文案（如「IP 格式不对」）；网络层错误统一兜底
        setLookupError(e instanceof ApiError ? e.message : '查询失败，稍后再试')
      })
      .finally(() => setLooking(false))
  }, [])

  const loadAllowlist = useCallback(() => {
    api<{ items: AllowlistItem[]; active: boolean }>('/api/admin/ip-allowlist')
      .then((d) => {
        setAllowlist(d.items ?? [])
        setWlActive(!!d.active)
        setWlError('')
      })
      .catch((e) => setWlError(e.message ?? String(e)))
  }, [])

  useEffect(loadAllowlist, [loadAllowlist])

  const addAllowlist = useCallback(() => {
    const ip = wlDraft.trim()
    if (!ip) return
    setWlBusy(true)
    setWlError('')
    api('/api/admin/ip-allowlist', {
      method: 'POST',
      body: JSON.stringify({ ip, note: wlNote.trim() }),
    })
      .then(() => {
        setWlDraft('')
        setWlNote('')
        loadAllowlist()
      })
      .catch((e) => setWlError(e.message ?? String(e)))
      .finally(() => setWlBusy(false))
  }, [wlDraft, wlNote, loadAllowlist])

  const removeAllowlist = useCallback(
    (id: number) => {
      setWlBusy(true)
      setWlError('')
      api(`/api/admin/ip-allowlist/${id}`, { method: 'DELETE' })
        .then(() => loadAllowlist())
        .catch((e) => setWlError(e.message ?? String(e)))
        .finally(() => setWlBusy(false))
    },
    [loadAllowlist],
  )

  return (
    <div className="page">
      <h1>访客日志</h1>
      <p className="hint">
        展示端只对安徽 / 河南 / 江苏开放（后端按 IP 归属地判定，多数据源投票），
        白名单是地区之外的额外放行 IP；其余 IP 一律提示「您的 ip 不在授权范围内」。
        这里只记 IP 访问日志（登录了带账号，没登录只记 IP），不再记录路径。
      </p>

      {stats && (
        <div className="cards cards-kpi">
          <div className="card kpi">
            <span className="hint">今日访问</span>
            <b className="num">{stats.today}</b>
            <span className="hint">累计 {stats.all} 条</span>
          </div>
          <div className="card kpi k-violet">
            <span className="hint">今日独立 IP</span>
            <b className="num">{stats.todayUniq}</b>
            <span className="hint">按 IP 去重</span>
          </div>
          <div className="card kpi k-good">
            <span className="hint">今日放行</span>
            <b className="num">{stats.todayAllowed}</b>
            <span className="hint">白名单 / 地区</span>
          </div>
          <div className="card kpi k-warn">
            <span className="hint">今日拦截</span>
            <b className="num">{stats.todayBlocked}</b>
            <span className="hint">不在授权范围</span>
          </div>
          <div className="card kpi">
            <span className="hint">今日登录</span>
            <b className="num">{stats.todayLogins}</b>
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

      <section className="card" style={{ marginBottom: 12 }}>
        <h2>访问白名单</h2>
        <p className="hint" style={{ marginTop: 0 }}>
          安徽 / 河南 / 江苏始终可访问；白名单是地区之外的<b>额外放行</b>（如省外的运营 /
          管理员固定 IP）。支持精确 IP 或 CIDR（如 1.2.3.4 / 10.0.0.0/24）。
        </p>
        <div className="row" style={{ margin: 0 }}>
          <form
            style={{ display: 'flex', gap: 8 }}
            onSubmit={(e) => {
              e.preventDefault()
              addAllowlist()
            }}
          >
            <input
              placeholder="IP，如 121.8.215.106 或 10.0.0.0/24"
              value={wlDraft}
              onChange={(e) => setWlDraft(e.target.value)}
              style={{ width: 240 }}
            />
            <input
              placeholder="备注（可选）"
              value={wlNote}
              onChange={(e) => setWlNote(e.target.value)}
              style={{ width: 160 }}
            />
            <button className="primary" type="submit" disabled={wlBusy || !wlDraft.trim()}>
              {wlBusy ? '保存中…' : '添加'}
            </button>
          </form>
          {wlActive && <span className="badge b-active">白名单 {allowlist.length} 条</span>}
          {!wlActive && <span className="hint">白名单为空 · 仅安徽 / 河南 / 江苏可访问</span>}
        </div>

        {wlError && <div className="alert bad">{wlError}</div>}

        {allowlist.length > 0 && (
          <table style={{ marginTop: 8 }}>
            <thead>
              <tr>
                <th>IP</th>
                <th>备注</th>
                <th>添加时间</th>
                <th className="col-actions"></th>
              </tr>
            </thead>
            <tbody>
              {allowlist.map((a) => (
                <tr key={a.id}>
                  <td title={a.ip}>
                    <span className="cell-clip">{a.ip}</span>
                  </td>
                  <td title={a.note || undefined}>
                    <span className="cell-clip">{a.note || '—'}</span>
                  </td>
                  <td>{a.createdAt}</td>
                  <td className="row-actions">
                    <button className="ghost" disabled={wlBusy} onClick={() => removeAllowlist(a.id)}>
                      删除
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>

      <section className="card" style={{ marginBottom: 12 }}>
        <h2>IP 查询</h2>
        <div className="row" style={{ margin: 0 }}>
          <form
            style={{ display: 'flex', gap: 8 }}
            onSubmit={(e) => {
              e.preventDefault()
              lookupIp(ipDraft)
            }}
          >
            <input
              placeholder="输入 IP，如 121.8.215.106"
              value={ipDraft}
              onChange={(e) => setIpDraft(e.target.value)}
              style={{ width: 220 }}
            />
            <button className="ghost" type="submit" disabled={looking}>
              {looking ? '查询中…' : '查询'}
            </button>
          </form>
        </div>

        {lookupError && <div className="alert bad">{lookupError}</div>}

        {lookup && (
          <div>
            <div className="row" style={{ margin: '10px 0 0' }}>
              <span>
                <b>{[lookup.province, lookup.city].filter(Boolean).join(' · ') || '未知'}</b>
                {' · '}
                {lookup.isp || '未知运营商'}
                {' · 命中源：'}
                {hitSources(lookup)}
              </span>
              {lookup.allowed ? (
                <span className="badge b-active">放行</span>
              ) : (
                <span className="badge" style={{ color: 'var(--bad)', background: 'var(--bad-bg)' }}>
                  拦截
                </span>
              )}
            </div>
            <table>
              <thead>
                <tr>
                  <th>源名</th>
                  <th>归属</th>
                  <th>城市</th>
                  <th>运营商</th>
                  <th>是否成功</th>
                </tr>
              </thead>
              <tbody>
                {lookup.sources.map((s) => (
                  <tr key={s.name} style={{ opacity: s.ok ? undefined : 0.55 }}>
                    <td>{s.name}</td>
                    <td>{s.province || '—'}</td>
                    <td>{s.city || '—'}</td>
                    <td>{s.isp || '—'}</td>
                    <td>
                      {s.ok ? <span className="badge b-active">成功</span> : <span className="badge b-left">失败</span>}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {error && <div className="alert bad">{error}</div>}

      <div className="card">
        {items.length === 0 ? (
          <p className="hint">
            {onlyBlocked || q ? '没有符合条件的记录。' : '还没有访问记录。'}
          </p>
        ) : (
          <>
            <div className="table-scroll">
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
                  </tr>
                </thead>
                <tbody>
                  {pager.slice(items).map((v) => (
                    <tr key={v.id}>
                      <td>{v.createdAt}</td>
                      <td>{KIND_LABEL[v.kind] ?? v.kind}</td>
                      <td
                        title={v.ip ? `${v.ip} · 点击查询该 IP` : undefined}
                        style={{ cursor: v.ip ? 'pointer' : undefined }}
                        onClick={() => {
                          if (!v.ip) return
                          setIpDraft(v.ip)
                          lookupIp(v.ip)
                        }}
                      >
                        <span className="cell-clip">{v.ip || '—'}</span>
                      </td>
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
                      <td title={v.account || undefined}>
                        <span className="cell-clip">{v.account || '—'}</span>
                      </td>
                      <td>{TEAM_LABEL[v.team] ?? (v.team || '—')}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            {/* 分页条放滚动容器外：不跟着表头一起被滚走 */}
            <Pager {...pager} />
          </>
        )}
      </div>
    </div>
  )
}
