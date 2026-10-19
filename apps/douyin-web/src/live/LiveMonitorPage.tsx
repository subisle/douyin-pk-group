import { useEffect, useMemo, useState } from 'react'
import './live.css'
import { useAutoRefresh } from '../useAutoRefresh'
import { live } from './api'
import type { LiveEvent, LiveGroup, LiveRound, LiveSeat, MonitorRoom, Overview } from './api'

/* 直播分数监控（2026-10-20）
   分组只决定「谁和谁打」，实时分数来自抖音连麦接口，二者不混存。 */

const STAGE_LABEL: Record<string, string> = {
  first_wave_group: '第一波小组赛',
  first_wave_revival: '第一波复活赛',
  rest_group: '剩余小组赛',
  rest_revival: '剩余复活赛',
  promotion: '晋级赛',
  final: '决赛',
}

const GROUP_STATUS: Record<string, { label: string; cls: string }> = {
  pending: { label: '未开始', cls: 'idle' },
  running: { label: '进行中', cls: 'live' },
  settling: { label: '结算中', cls: 'warn' },
  pending_review: { label: '待确认', cls: 'warn' },
  waiting_restart: { label: '待重开', cls: 'warn' },
  finalized: { label: '已定稿', cls: 'ok' },
}

/** 状态点：颜色即语义，运营扫一眼就知道哪一卡住了 */
function Dot({ kind }: { kind: string }) {
  return <span className={`dot dot-${kind}`} aria-hidden />
}

function GroupCard({
  g, eventId, onChanged,
}: { g: LiveGroup; eventId: number; onChanged: () => void }) {
  const [rounds, setRounds] = useState<LiveRound[] | null>(null)
  const [open, setOpen] = useState(false)
  const [busy, setBusy] = useState('')

  async function loadRounds() {
    try {
      const r = await live.groupRounds(g.id)
      setRounds(r.items)
    } catch { setRounds([]) }
  }
  useEffect(() => { if (open) loadRounds() }, [open, g.id])

  const st = GROUP_STATUS[g.status] ?? { label: g.status, cls: 'idle' }
  const needReview = rounds?.filter(r => r.status === 'pending_review') ?? []

  async function act(fn: () => Promise<unknown>, label: string) {
    if (busy) return
    if (!confirm(label)) return
    setBusy(label)
    try { await fn(); await loadRounds(); onChanged() }
    catch (e: any) { alert(e?.message ?? String(e)) }
    finally { setBusy('') }
  }

  return (
    <div className={`lm-group lm-${st.cls}`}>
      <div className="lm-group-head">
        <Dot kind={st.cls} />
        <b>{g.name}</b>
        <span className="lm-tag">{st.label}</span>
        {g.qualifySlots > 0 && <span className="lm-tag">前 {g.qualifySlots} 名晋级</span>}
        <button className="linkish" onClick={() => setOpen(o => !o)}>
          {open ? '收起' : `${g.officialRoundId ? '对局' : '详情'} ▾`}
        </button>
      </div>

      {open && (
        <div className="lm-rounds">
          {rounds === null && <p className="hint">加载中…</p>}
          {rounds?.length === 0 && <p className="hint">这一组还没有对局</p>}
          {rounds?.map(r => (
            <div key={r.id} className={`lm-round ${r.isOfficial ? 'is-official' : ''}`}>
              <div className="lm-round-line">
                <span className="mono">#{r.attemptNo}</span>
                <span className="mono" title={r.battleId}>{r.battleId.slice(0, 14)}…</span>
                <span className="lm-tag">{r.status}</span>
                {r.endReason && r.endReason !== 'normal' && (
                  <span className="lm-tag lm-bad">{r.endReason}</span>
                )}
                {r.isOfficial && <span className="lm-tag lm-ok">正式局</span>}
              </div>
              <div className="lm-round-actions">
                {!r.isOfficial && (
                  <button onClick={() => act(() => live.chooseOfficial(g.id, r.id), '把这一局设为正式局？')}>
                    设为正式局
                  </button>
                )}
                <button onClick={() => act(() => live.voidRound(r.id), '作废这一局？')}>作废</button>
              </div>
            </div>
          ))}
          {needReview.length > 1 && (
            <p className="lm-warn">这一组有 {needReview.length} 场待确认，需要选择正式局</p>
          )}
          <div className="lm-round-actions">
            <button onClick={() => act(() => live.finalizeGroup(g.id), '强制把这一组定稿？')}>
              强制定稿
            </button>
            <button onClick={() => act(() => live.freezeGroup(g.id, g.stageKind.includes('revival') ? 'revival' : 'direct'), '冻结这一组的晋级结果？')}>
              冻结晋级
            </button>
            {g.officialRoundId > 0 && (
              <button onClick={() => act(() => live.pushRoundScores(g.officialRoundId), '给这一局的主播推一次当前分数？')}>
                推一次分数
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  )
}

export default function LiveMonitorPage() {
  const [events, setEvents] = useState<LiveEvent[]>([])
  const [eventId, setEventId] = useState<number>(0)
  const [ov, setOv] = useState<Overview | null>(null)
  const [groupId, setGroupId] = useState<number>(0)
  const [seats, setSeats] = useState<LiveSeat[]>([])
  const [roundId, setRoundId] = useState(0)
  const [err, setErr] = useState('')
  const [busy, setBusy] = useState('')

  const load = async () => {
    try {
      const list = await live.events()
      setEvents(list.items)
      if (!eventId && list.items[0]) setEventId(list.items[0].id)
      setErr('')
    } catch (e: any) { setErr(e?.message ?? String(e)) }
  }

  const loadOverview = async () => {
    if (!eventId) return
    try {
      const o = await live.overview(eventId)
      setOv(o)
      const running = o.groups.find(g => g.status === 'running') ?? o.groups[0]
      if (running && (!groupId || !o.groups.some(g => g.id === groupId))) setGroupId(running.id)
      setErr('')
    } catch (e: any) { setErr(e?.message ?? String(e)) }
  }

  const loadScores = async () => {
    if (!eventId || !groupId) return
    try {
      const s = await live.scores(eventId, groupId)
      setSeats(s.seats)
      setRoundId(s.roundId)
    } catch { /* 分数拉取失败不打断监控 */ }
  }

  useEffect(() => { load() }, [])
  useEffect(() => { loadOverview() }, [eventId])
  useEffect(() => { loadScores() }, [groupId, eventId])

  // 直播变更（分数/对局/阶段）→ 立刻重拉；同时每 5 秒兜底轮询一次
  useAutoRefresh(loadOverview, ['live'])
  useEffect(() => {
    const t = window.setInterval(() => { loadOverview(); loadScores() }, 5000)
    return () => window.clearInterval(t)
  }, [eventId, groupId])

  const cur = events.find(e => e.id === eventId) ?? null

  async function step(label: string, fn: () => Promise<unknown>) {
    if (busy) return
    if (!confirm(label)) return
    setBusy(label)
    try { await fn(); await loadOverview() }
    catch (e: any) { alert(e?.message ?? String(e)) }
    finally { setBusy('') }
  }

  const groupsByStage = useMemo(() => {
    const m = new Map<string, LiveGroup[]>()
    for (const g of ov?.groups ?? []) {
      const k = g.stageKind
      m.set(k, [...(m.get(k) ?? []), g])
    }
    return [...m.entries()]
  }, [ov])

  return (
    <div className="page">
      <h2>直播分数监控</h2>

      <div className="lm-toolbar">
        <select value={eventId} onChange={e => setEventId(Number(e.target.value))}>
          <option value={0}>选择赛事…</option>
          {events.map(e => <option key={e.id} value={e.id}>{e.title}</option>)}
        </select>
        {cur && (
          <span className="lm-tag">
            推送 {cur.pushEnabled ? `开 · 每 ${cur.pushIntervalSecs} 秒` : '关'}
          </span>
        )}
        <button onClick={() => loadOverview()} disabled={!eventId}>刷新</button>
      </div>

      {err && <div className="alert bad">{err}</div>}

      {!eventId && <p className="hint">先选择一个赛事，或到下面「新建赛事」建一场。</p>}

      {ov && (
        <>
          <div className="lm-rooms">
            <h3>直播间</h3>
            {ov.rooms.length === 0 && <p className="hint">还没有监控直播间</p>}
            {ov.rooms.map((r: MonitorRoom) => (
              <div key={r.id} className={`lm-room lm-${r.health}`}>
                <Dot kind={r.health === 'live' ? 'live' : r.health === 'degraded' ? 'warn' : 'idle'} />
                <b>{r.label || r.webRid}</b>
                <span className="mono">{r.webRid}</span>
                <span className="lm-tag">{r.health}</span>
                {r.lastError && <span className="lm-tag lm-bad">{r.lastError}</span>}
              </div>
            ))}
          </div>

          <div className="lm-board">
            <div className="lm-scores">
              <h3>
                实时分
                {groupId > 0 && (
                  <select value={groupId} onChange={e => setGroupId(Number(e.target.value))}>
                    {(ov.groups ?? []).map(g => (
                      <option key={g.id} value={g.id}>{g.name}</option>
                    ))}
                  </select>
                )}
              </h3>
              {seats.length === 0 && <p className="hint">这一局还没有分数</p>}
              <table className="lm-table">
                <tbody>
                  {seats.map(s => (
                    <tr key={s.douyinUid} className={s.qualified ? 'is-qualified' : ''}>
                      <td className="lm-rank">#{s.rank}</td>
                      <td>{s.nickname || s.douyinUid}</td>
                      <td className="mono lm-score">{s.score}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>

            <div className="lm-stages">
              <h3>赛程</h3>
              {groupsByStage.map(([kind, gs]) => (
                <div key={kind} className="lm-stage">
                  <div className="lm-stage-head">
                    {STAGE_LABEL[kind] ?? kind}
                    <span className="lm-tag">{gs.filter(g => g.status === 'finalized').length}/{gs.length} 定稿</span>
                  </div>
                  {gs.map(g => (
                    <GroupCard key={g.id} g={g} eventId={eventId} onChanged={loadOverview} />
                  ))}
                </div>
              ))}
            </div>
          </div>

          <div className="lm-actions">
            <h3>赛程推进</h3>
            <button disabled={!!busy || !eventId} onClick={() => step('生成白天小组？', () => live.buildDayGroups(eventId))}>
              生成白天小组
            </button>
            <button disabled={!!busy || !eventId} onClick={() => step('冻结第一波晋级结果？', () => live.freezeStage(eventId, 'first', 'direct'))}>
              冻结第一波晋级
            </button>
            <button disabled={!!busy || !eventId} onClick={() => step('生成第一波复活赛？', () => live.buildRevival(eventId, 'first'))}>
              生成第一波复活赛
            </button>
            <button disabled={!!busy || !eventId} onClick={() => step('生成晚上晋级赛？', () => live.buildPromotion(eventId))}>
              生成晋级赛
            </button>
          </div>

          {ov.qualifiers.length > 0 && (
            <div className="lm-qualifiers">
              <h3>晋级名单（{ov.qualifiers.length} 人）</h3>
              <table className="lm-table">
                <thead><tr><th>来源</th><th>主播</th><th>名次</th><th>分数</th></tr></thead>
                <tbody>
                  {ov.qualifiers.map((q, i) => (
                    <tr key={i}>
                      <td>{q.source === 'direct' ? '直接晋级' : q.source === 'revival' ? '复活晋级' : '晋级赛'}</td>
                      <td>{q.personName || q.douyinUid}</td>
                      <td>#{q.rank}</td>
                      <td className="mono">{q.score}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </>
      )}
    </div>
  )
}
