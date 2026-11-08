import { useEffect, useState } from 'react'
import type { FormEvent } from 'react'
import './live.css'
import './room.css'
import { live } from './api'
import type { StandaloneRoomScores } from './api'

/** 独立直播间监控：只按 web_rid 读取实时采样，不经过赛事或分组。 */
function normalizeRoom(value: string) {
  const fromUrl = value.match(/live\.douyin\.com\/(\d+)/i)?.[1]
  const digits = (fromUrl ?? value).replace(/\s/g, '')
  return /^\d{5,40}$/.test(digits) ? digits : ''
}

function formatMultiplier(multiple: number) {
  const value = Math.max(1, multiple / 100)
  return `${Number.isInteger(value) ? value : value.toFixed(2).replace(/0+$/, '')}x`
}

function formatTime(value: string | null) {
  if (!value) return '—'
  const d = new Date(value)
  return Number.isNaN(d.getTime()) ? value : d.toLocaleTimeString('zh-CN')
}

export default function RoomMonitorPage() {
  const initial = normalizeRoom(new URLSearchParams(window.location.search).get('room') ?? '')
  const [input, setInput] = useState(initial)
  const [room, setRoom] = useState(initial)
  const [data, setData] = useState<StandaloneRoomScores | null>(null)
  const [error, setError] = useState('')
  const [loading, setLoading] = useState(false)

  async function load(webRid: string) {
    if (!webRid) return
    setLoading(true)
    try {
      const next = await live.roomScores(webRid)
      setData(next)
      setError('')
    } catch (e: any) {
      setError(e?.message ?? String(e))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    if (!room) return
    void load(room)
    const timer = window.setInterval(() => { void load(room) }, 2000)
    return () => window.clearInterval(timer)
  }, [room])

  function submit(e: FormEvent) {
    e.preventDefault()
    const next = normalizeRoom(input)
    if (!next) {
      setError('请输入直播间链接或纯数字房间号（例如 90968758361）')
      return
    }
    setInput(next)
    setRoom(next)
    const q = new URLSearchParams(window.location.search)
    q.set('room', next)
    window.history.replaceState(null, '', `${window.location.pathname}?${q.toString()}`)
  }

  const roomState = data?.room
  const scores = data?.scores ?? []
  const maxScore = Math.max(1, ...scores.map(s => Number(s.score) || 0))
  const activeBonusCount = scores.filter(s => s.bonusActive).length
  const hasBonusScore = scores.some(s => (s.bonusScore ?? 0) > 0)

  return (
    <div className="page">
      <h2>独立直播监控</h2>
      <div className="lm-room-intro">
        <form className="lm-room-search" onSubmit={submit}>
          <input
            value={input}
            onChange={e => setInput(e.target.value)}
            placeholder="https://live.douyin.com/90968758361 或 90968758361"
            aria-label="直播间链接或房间号"
          />
          <button className="primary" type="submit" disabled={loading}>开始监控</button>
        </form>
        <p className="hint">
          当前只抓取直播间实时采样，<b>不关联赛事、小组或晋级</b>；采集器用
          <code>scripts/live/run-room.sh {room || '房间号'}</code> 启动。
        </p>
      </div>

      {error && <div className="alert bad">{error}</div>}
      {!room && <div className="lm-live-empty">输入房间号后开始监控，数据会每 2 秒刷新。</div>}
      {room && data && (
        <div className="lm-room-board">
          <div className="lm-scores lm-live-scores">
            <h3>
              <span className="lm-live-title"><span className="lm-live-dot" />实时分</span>
              <span className="lm-tag">{data.webRid}</span>
            </h3>
            <div className="lm-live-meta">
              <span><i className="lm-live-dot small" /> 独立房间 · 2 秒刷新</span>
              <span>最后采样 {formatTime(roomState?.lastSeenAt ?? null)}</span>
              <span>状态 {roomState?.health || 'idle'}</span>
              {roomState?.battleId && <span>对局 {roomState.battleId.slice(-10)}</span>}
            </div>
            <div className="lm-live-summary">
              <div><span>直播间</span><b>{data.webRid}</b></div>
              <div><span>在榜人数</span><b>{scores.length}</b></div>
              <div><span>组内显示分</span><b>{scores.reduce((n, s) => n + (Number(s.score) || 0), 0).toLocaleString('zh-CN')}</b></div>
              <div><span>加成状态</span><b className={activeBonusCount > 0 ? 'lm-bonus-live-text' : ''}>
                {activeBonusCount > 0 ? `${activeBonusCount} 人加成中` : hasBonusScore ? '加成已结束' : '加成待触发'}
              </b></div>
            </div>
            {scores.length === 0 && <p className="lm-live-empty">还没有收到这一局的分数…</p>}
            <div className="lm-live-list">
              {scores.map(s => {
                const bonusMultiple = s.bonusMultiple ?? 100
                const bonusScore = Math.max(0, s.bonusScore ?? 0)
                const realScore = Math.max(0, (Number(s.score) || 0) - bonusScore)
                const pct = Math.max(2, Math.round(((Number(s.score) || 0) / maxScore) * 100))
                return (
                  <div key={s.douyinUid} className="lm-live-row">
                    <div className={`lm-live-rank rank-${Math.min(Math.max(s.rank, 1), 4)}`}>#{s.rank}</div>
                    <div className="lm-live-player">
                      <div className="lm-live-name-line">
                        <b className="lm-live-name">{s.nickname || s.douyinUid}</b>
                        {bonusMultiple > 100 && (
                          <span className={`lm-bonus-badge${s.bonusActive ? ' is-active' : ''}`}>
                            {s.bonusActive ? '加成中' : '加成'} {formatMultiplier(bonusMultiple)}
                          </span>
                        )}
                      </div>
                      <div className="lm-live-bar"><span style={{ width: `${pct}%` }} /></div>
                    </div>
                    <div
                      className="lm-live-value"
                      title={bonusScore > 0
                        ? `显示分 ${(Number(s.score) || 0).toLocaleString('zh-CN')} = 去加成 ${realScore.toLocaleString('zh-CN')} + 加成 ${bonusScore.toLocaleString('zh-CN')}`
                        : undefined}
                    >
                      <b>{(Number(s.score) || 0).toLocaleString('zh-CN')}</b>
                      {bonusScore > 0 && <span className="lm-real-score">去加成 {realScore.toLocaleString('zh-CN')}</span>}
                    </div>
                  </div>
                )
              })}
            </div>
          </div>

          <aside className="lm-room-info">
            <h3>监控口径</h3>
            <p>这里只记录直播间和 PK 对局，不判断谁属于哪一组，也不推进晋级。</p>
            <dl>
              <div><dt>房间号</dt><dd>{data.webRid}</dd></div>
              <div><dt>对局 ID</dt><dd className="mono">{roomState?.battleId || '—'}</dd></div>
              <div><dt>最后采样</dt><dd>{formatTime(roomState?.lastSeenAt ?? null)}</dd></div>
              <div><dt>显示分</dt><dd>直播画面上的带加成分数</dd></div>
              <div><dt>真实分</dt><dd>显示分 − 累计加成增量</dd></div>
            </dl>
            {roomState?.lastError && <div className="alert bad">{roomState.lastError}</div>}
            <p className="hint">加成倍数来自 <code>battle_easter_egg_info</code>；原分不会覆盖直播显示分。</p>
          </aside>
        </div>
      )}
    </div>
  )
}
