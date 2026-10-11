import { Component, useCallback, useEffect, useState, type ReactNode } from 'react'
import { api } from './api'
import { useAutoRefresh } from './useAutoRefresh'

/* 抖音号绑定审计：展示端用户提交「抖音号 + 主页截图」→ 这里看截图裁决。
   通过 = 后端写 account 表（抖音号是导入匹配第一优先级），之后音浪自动落到这个人；
   驳回 = 记原因，用户可在展示端「我的」重新提交。 */

type Claim = {
  id: number
  personId: number
  personName: string
  team: string
  douyinNo: string
  status: 'pending' | 'approved' | 'rejected'
  rejectReason: string
  createdAt: string
  decidedAt: string
  screenshotBytes: number
}

const FILTERS: { key: string; label: string }[] = [
  { key: 'pending', label: '待审核' },
  { key: 'approved', label: '已通过' },
  { key: 'rejected', label: '已驳回' },
  { key: '', label: '全部' },
]

const ST_BADGE: Record<string, { text: string; cls: string }> = {
  pending: { text: '待审核', cls: 'badge b-paused' },
  approved: { text: '已通过', cls: 'badge b-active' },
  rejected: { text: '已驳回', cls: 'badge b-left' },
}

/** 未知 status 的兜底（P2-4）：后端将来加新状态 / 脏数据时不能整页白屏 */
const ST_BADGE_FALLBACK: { text: string; cls: string } = {
  text: '未知状态',
  cls: 'badge',
}
const badgeOf = (status: string) => ST_BADGE[status] ?? ST_BADGE_FALLBACK

/** 组件级 ErrorBoundary（P2-4）：渲染抛错只打倒这一页的卡片区域，
 *  显示错误 + 重载按钮，不再白屏。 */
class ClaimErrorBoundary extends Component<{ children: ReactNode }, { error: Error | null }> {
  state = { error: null as Error | null }

  static getDerivedStateFromError(error: Error) {
    return { error }
  }

  componentDidCatch(error: Error) {
    console.error('抖音号审计页渲染出错', error)
  }

  render() {
    if (this.state.error) {
      return (
        <div className="card">
          <div className="alert bad">
            ⚠️ 页面渲染出错：{this.state.error.message}
          </div>
          <button onClick={() => this.setState({ error: null })}>重试</button>
        </div>
      )
    }
    return this.props.children
  }
}

const kb = (n: number) => `${Math.max(1, Math.round(n / 1024))} KB`

export default function ClaimPage() {
  const [items, setItems] = useState<Claim[]>([])
  const [filter, setFilter] = useState('pending')
  const [msg, setMsg] = useState('')
  const [error, setError] = useState('')
  const [shot, setShot] = useState<{ claim: Claim; url: string } | null>(null)
  const [busyId, setBusyId] = useState<number | null>(null)

  const load = useCallback(() => {
    api(`/api/admin/anchor-claims${filter ? `?status=${filter}` : ''}`)
      .then((d) => {
        setItems(d.items ?? [])
        setError('')
      })
      .catch((e) => setError(e.message ?? String(e)))
  }, [filter])

  useEffect(load, [load])
  useAutoRefresh(load, ['import', 'roster'])

  useEffect(() => {
    // 弹层里的 blob URL 用完即还
    return () => {
      if (shot) URL.revokeObjectURL(shot.url)
    }
  }, [shot])

  async function openShot(c: Claim) {
    try {
      const resp = await fetch(`/api/admin/anchor-claims/${c.id}/screenshot`, {
        headers: { Authorization: `Bearer ${sessionStorage.getItem('pk_token') ?? ''}` },
      })
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`)
      const url = URL.createObjectURL(await resp.blob())
      setShot({ claim: c, url })
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  async function decide(c: Claim, approve: boolean) {
    let reason = ''
    if (!approve) {
      const raw = window.prompt('驳回原因（会展示给主播，留空取消）', '截图看不清，请重传')
      if (raw == null) return
      reason = raw
      if (!reason.trim()) {
        setError('驳回要填写原因')
        return
      }
    }
    setBusyId(c.id)
    setError('')
    try {
      await api(`/api/admin/anchor-claims/${c.id}/decide`, {
        method: 'POST',
        body: JSON.stringify({ approve, reason }),
      })
      setMsg(
        approve
          ? `已通过：${c.personName} ← ${c.douyinNo}，音浪从下次导入起落到 TA 名下`
          : `已驳回：${c.personName} 的 ${c.douyinNo}`,
      )
      load()
    } catch (e: any) {
      setError(e.message ?? String(e))
    } finally {
      setBusyId(null)
    }
  }

  return (
    <div className="page">
      <h1>抖音号绑定审计</h1>
      <p className="hint">
        用户在展示端「我的」提交抖音号 + 主页截图；通过后写入账号表，
        之后导入的音浪按抖音号精确匹配落到该主播名下。
      </p>

      <div className="row" style={{ marginBottom: 12 }}>
        {FILTERS.map((f) => (
          <button
            key={f.key}
            className={filter === f.key ? 'primary' : 'ghost'}
            style={{ marginRight: 8 }}
            onClick={() => setFilter(f.key)}
          >
            {f.label}
          </button>
        ))}
      </div>

      {error && <div className="alert bad">{error}</div>}
      {msg && <div className="alert">{msg}</div>}

      <ClaimErrorBoundary>
        <div className="card">
        {items.length === 0 ? (
          <p className="hint">没有{FILTERS.find((f) => f.key === filter)?.label ?? ''}的申请。</p>
        ) : (
          <div className="table-scroll">
            <table>
              <thead>
                <tr>
                  <th>提交时间</th>
                <th>主播</th>
                <th>团别</th>
                <th>抖音号</th>
                <th>状态</th>
                <th>截图</th>
                <th>操作</th>
              </tr>
            </thead>
            <tbody>
              {items.map((c) => {
                const badge = badgeOf(c.status)
                return (
                  <tr key={c.id}>
                    <td className="num" style={{ whiteSpace: 'nowrap' }}>
                      {c.createdAt}
                      {c.status === 'pending' && (
                        <div className="hint">{kb(c.screenshotBytes || 0)}</div>
                      )}
                    </td>
                    <td>
                      <b>{c.personName}</b>
                      {c.status === 'rejected' && c.rejectReason && (
                        <div className="hint">驳回原因：{c.rejectReason}</div>
                      )}
                      {c.status !== 'pending' && c.decidedAt && (
                        <div className="hint">处理于 {c.decidedAt}</div>
                      )}
                    </td>
                    <td>
                      <span className={`badge ${c.team === 'male' ? 'b-male' : 'b-female'}`}>
                        {c.team === 'male' ? '男团' : '女团'}
                      </span>
                    </td>
                    <td className="num">{c.douyinNo}</td>
                    <td>
                      <span className={badge.cls}>{badge.text}</span>
                    </td>
                    <td>
                      <button className="ghost" onClick={() => openShot(c)}>
                        看截图
                      </button>
                    </td>
                    <td>
                      {c.status === 'pending' ? (
                        <>
                          <button
                            className="primary"
                            disabled={busyId === c.id}
                            onClick={() => decide(c, true)}
                          >
                            通过
                          </button>{' '}
                          <button
                            className="danger"
                            disabled={busyId === c.id}
                            onClick={() => decide(c, false)}
                          >
                            驳回
                          </button>
                        </>
                      ) : (
                        <span className="hint">已处理</span>
                      )}
                    </td>
                  </tr>
                )
              })}
              </tbody>
            </table>
          </div>
        )}
        </div>
      </ClaimErrorBoundary>

      {shot && (
        <div
          style={{
            position: 'fixed',
            inset: 0,
            background: 'rgba(15,23,42,.55)',
            display: 'grid',
            placeItems: 'center',
            zIndex: 50,
          }}
          onClick={() => setShot(null)}
        >
          <div
            className="card"
            style={{ maxWidth: 420, padding: 16, textAlign: 'center' }}
            onClick={(e) => e.stopPropagation()}
          >
            <h2 style={{ marginTop: 0 }}>
              {shot.claim.personName} · {shot.claim.douyinNo}
            </h2>
            <img
              src={shot.url}
              alt="抖音主页截图"
              style={{ maxWidth: '100%', maxHeight: '65vh', borderRadius: 10 }}
            />
            <div style={{ marginTop: 12 }}>
              {shot.claim.status === 'pending' && (
                <>
                  <button
                    className="primary"
                    disabled={busyId === shot.claim.id}
                    onClick={() => decide(shot.claim, true)}
                  >
                    通过
                  </button>{' '}
                  <button
                    className="danger"
                    disabled={busyId === shot.claim.id}
                    onClick={() => decide(shot.claim, false)}
                  >
                    驳回
                  </button>
                </>
              )}{' '}
              <button className="ghost" onClick={() => setShot(null)}>
                关闭
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
