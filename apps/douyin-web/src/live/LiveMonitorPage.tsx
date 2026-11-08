import { useEffect, useMemo, useRef, useState } from 'react'
import type { FormEvent } from 'react'
import './live.css'
import { useAutoRefresh } from '../useAutoRefresh'
import { pk } from '../pk/api'
import type { PlanBrief } from '../pk/types'
import { live } from './api'
import type {
  LiveEvent, LiveGroup, LiveGroupSeat, LiveRound, LiveSeat, LiveWorker, MonitorRoom, Overview,
} from './api'

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

function todayLocal() {
  const d = new Date()
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}

function formatMultiplier(multiple: number) {
  const value = Math.max(1, multiple / 100)
  return `${Number.isInteger(value) ? value : value.toFixed(2).replace(/0+$/, '')}x`
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
  const roster: LiveGroupSeat[] = g.seats ?? []
  const freezeSource = g.stageKind === 'promotion'
    ? 'promotion'
    : g.stageKind === 'final'
      ? 'final'
      : g.stageKind.includes('revival') ? 'revival' : 'direct'

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
          <div className="lm-roster">
            <div className="lm-roster-head">赛前名单 <span className="lm-tag">{roster.length} 人</span></div>
            {roster.length === 0 && <p className="hint">还没有赛前名单，先从分组方案生成小组</p>}
            {roster.length > 0 && (
              <div className="lm-roster-list">
                {roster.map((seat, i) => (
                  <span key={`${seat.personId}-${seat.douyinUid}-${i}`} className="lm-roster-person">
                    <span>{seat.personName || seat.nickname || seat.douyinUid || `#${seat.personId}`}</span>
                    {seat.douyinUid && <span className="mono lm-uid">{seat.douyinUid}</span>}
                  </span>
                ))}
              </div>
            )}
          </div>
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
            <button onClick={() => act(() => live.freezeGroup(g.id, freezeSource), g.stageKind === 'final' ? '冻结决赛前三名？' : '冻结这一组的晋级结果？')}>
              {g.stageKind === 'final' ? '冻结决赛名次' : '冻结晋级'}
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
  const [plans, setPlans] = useState<PlanBrief[]>([])
  const [eventId, setEventId] = useState<number>(0)
  const [ov, setOv] = useState<Overview | null>(null)
  const [groupId, setGroupId] = useState<number>(0)
  const [seats, setSeats] = useState<LiveSeat[]>([])
  const [roundId, setRoundId] = useState(0)
  const [err, setErr] = useState('')
  const [busy, setBusy] = useState('')
  const [newTitle, setNewTitle] = useState('')
  const [newDate, setNewDate] = useState(todayLocal())
  const [newDayPlan, setNewDayPlan] = useState(0)
  const [bindDay, setBindDay] = useState(0)
  const [bindNight, setBindNight] = useState(0)
  const [roomUrl, setRoomUrl] = useState('')
  const [roomLabel, setRoomLabel] = useState('')
  const [pushSecs, setPushSecs] = useState(60)
  const [scoreDeltas, setScoreDeltas] = useState<Record<string, number>>({})
  const [rankDeltas, setRankDeltas] = useState<Record<string, number>>({})
  const [lastScoreAt, setLastScoreAt] = useState('')
  const prevScoresRef = useRef<Map<string, number>>(new Map())
  const prevRanksRef = useRef<Map<string, number>>(new Map())
  const scoreRefreshTimerRef = useRef<number | null>(null)

  const load = async () => {
    try {
      const list = await live.events()
      setEvents(list.items)
      if (!eventId && list.items[0]) setEventId(list.items[0].id)
      setErr('')
    } catch (e: any) { setErr(e?.message ?? String(e)) }
    try {
      const list = await pk.listPlans()
      setPlans(list.items ?? [])
    } catch (e: any) {
      // 赛事仍可管理；只是新建/绑定方案下拉为空。
      setPlans([])
      setErr(e?.message ?? String(e))
    }
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
      const nextScoreDeltas: Record<string, number> = {}
      const nextRankDeltas: Record<string, number> = {}
      for (const seat of s.seats) {
        const prevScore = prevScoresRef.current.get(seat.douyinUid)
        if (prevScore != null && seat.score !== prevScore) {
          nextScoreDeltas[seat.douyinUid] = seat.score - prevScore
        }
        const prevRank = prevRanksRef.current.get(seat.douyinUid)
        if (prevRank != null && seat.rank !== prevRank) {
          nextRankDeltas[seat.douyinUid] = prevRank - seat.rank
        }
      }
      prevScoresRef.current = new Map(s.seats.map(seat => [seat.douyinUid, seat.score]))
      prevRanksRef.current = new Map(s.seats.map(seat => [seat.douyinUid, seat.rank]))
      setScoreDeltas(nextScoreDeltas)
      setRankDeltas(nextRankDeltas)
      const samples = new Map(s.scores.map(sample => [sample.douyinUid, sample]))
      setSeats(s.seats.map(seat => {
        const sample = samples.get(seat.douyinUid)
        return {
          ...seat,
          bonusMultiple: sample?.bonusMultiple ?? 100,
          bonusScore: sample?.bonusScore ?? 0,
          bonusActive: sample?.bonusActive ?? false,
          bonusEndMs: sample?.bonusEndMs ?? 0,
          currentBonus: sample?.currentBonus ?? 0,
        }
      }))
      setRoundId(s.roundId)
      setLastScoreAt(s.seats.length > 0 ? new Date().toLocaleTimeString() : '')
    } catch { /* 分数拉取失败不打断监控 */ }
  }
  const loadScoresRef = useRef(loadScores)
  loadScoresRef.current = loadScores

  /** 采集器一拍可能连续广播多条 live；合并成 500ms 一次读取，避免追着事件风暴打接口。 */
  const scheduleScoreRefresh = () => {
    if (scoreRefreshTimerRef.current != null) return
    scoreRefreshTimerRef.current = window.setTimeout(() => {
      scoreRefreshTimerRef.current = null
      void loadScoresRef.current()
    }, 500)
  }

  useEffect(() => { load() }, [])
  useEffect(() => { loadOverview() }, [eventId])
  useEffect(() => {
    prevScoresRef.current = new Map()
    prevRanksRef.current = new Map()
    setScoreDeltas({})
    setRankDeltas({})
    setSeats([])
    setRoundId(0)
    setLastScoreAt('')
    loadScores()
  }, [groupId, eventId])
  useEffect(() => {
    const event = ov?.event
    if (!event || event.id !== eventId) return
    setBindDay(event.dayPlanId || 0)
    setBindNight(event.nightPlanId || 0)
  }, [ov?.event?.id, ov?.event?.dayPlanId, ov?.event?.nightPlanId, eventId])

  // 采集器每上报一拍都会广播 live：分数走 SSE 合并刷新，2 秒轮询只作兜底。
  // 赛程结构变化仍由 5 秒 overview 轮询补齐，避免每拍都做一遍全量赛程查询。
  useAutoRefresh(scheduleScoreRefresh, ['live'])
  useEffect(() => {
    const overviewTimer = window.setInterval(loadOverview, 5000)
    const scoreTimer = window.setInterval(loadScores, 2000)
    return () => {
      window.clearInterval(overviewTimer)
      window.clearInterval(scoreTimer)
    }
  }, [eventId, groupId])
  useEffect(() => () => {
    if (scoreRefreshTimerRef.current != null) {
      window.clearTimeout(scoreRefreshTimerRef.current)
      scoreRefreshTimerRef.current = null
    }
  }, [])

  const cur = events.find(e => e.id === eventId) ?? null

  useEffect(() => {
    if (cur) setPushSecs(cur.pushIntervalSecs)
  }, [cur?.id, cur?.pushIntervalSecs])

  async function savePush(enabled: boolean, intervalSecs = pushSecs) {
    if (!eventId || busy) return
    const interval = Math.max(10, Math.min(3600, Math.round(Number(intervalSecs) || 60)))
    setBusy('push')
    try {
      const r = await live.updatePush(eventId, enabled, interval)
      setEvents(items => items.map(e => e.id === eventId
        ? { ...e, pushEnabled: r.pushEnabled, pushIntervalSecs: r.pushIntervalSecs }
        : e))
      setPushSecs(r.pushIntervalSecs)
    } catch (e: any) { alert(e?.message ?? String(e)) }
    finally { setBusy('') }
  }

  async function createEvent(e: FormEvent) {
    e.preventDefault()
    if (!newTitle.trim()) return alert('请填写赛事名称')
    setBusy('create-event')
    try {
      const r = await live.createEvent({
        title: newTitle.trim(),
        eventDate: newDate,
        dayPlanId: newDayPlan || undefined,
      })
      setNewTitle('')
      setNewDayPlan(0)
      setEventId(r.id)
      await load()
    } catch (e: any) { alert(e?.message ?? String(e)) }
    finally { setBusy('') }
  }

  async function bindPlans() {
    if (!eventId) return
    setBusy('bind-plans')
    try {
      await live.bindPlans(eventId, bindDay || undefined, bindNight || undefined)
      await loadOverview()
    } catch (e: any) { alert(e?.message ?? String(e)) }
    finally { setBusy('') }
  }

  async function addMonitor(e: FormEvent) {
    e.preventDefault()
    if (!eventId || !roomUrl.trim()) return alert('请填写抖音直播间地址或房间号')
    setBusy('add-monitor')
    try {
      await live.addMonitor(eventId, roomUrl.trim(), roomLabel.trim() || undefined)
      setRoomUrl('')
      setRoomLabel('')
      await loadOverview()
    } catch (e: any) { alert(e?.message ?? String(e)) }
    finally { setBusy('') }
  }

  async function step(label: string, fn: () => Promise<unknown>) {
    if (busy) return
    if (!confirm(label)) return
    setBusy(label)
    try { await fn(); await loadOverview() }
    catch (e: any) { alert(e?.message ?? String(e)) }
    finally { setBusy('') }
  }

  const currentGroup = ov?.groups.find(g => g.id === groupId)
  const maxScore = Math.max(1, ...seats.map(s => Number(s.score) || 0))
  const groupWave = seats.reduce((sum, s) => sum + (Number(s.score) || 0), 0)
  const activeBonusCount = seats.filter(s => s.bonusActive).length
  const hasBonusScore = seats.some(s => (s.bonusScore ?? 0) > 0)
  const hasBonusData = seats.some(s => (s.bonusMultiple ?? 100) > 100 || (s.bonusScore ?? 0) > 0)

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
          <div className="lm-push-controls" title="直播分数推送给已开启直播播报的主播">
            <label>
              <input
                type="checkbox"
                checked={cur.pushEnabled}
                disabled={!!busy}
                onChange={e => savePush(e.target.checked, pushSecs)}
              />
              分数推送
            </label>
            <input
              type="number"
              min={10}
              max={3600}
              value={pushSecs}
              disabled={!!busy}
              onChange={e => setPushSecs(Number(e.target.value))}
              onBlur={() => {
                if (pushSecs !== cur.pushIntervalSecs) savePush(cur.pushEnabled, pushSecs)
              }}
            />
            <span>秒</span>
          </div>
        )}
        <button onClick={() => loadOverview()} disabled={!eventId}>刷新</button>
      </div>

      {err && <div className="alert bad">{err}</div>}

      <div className="lm-setup">
        <form className="lm-setup-form" onSubmit={createEvent}>
          <h3>新建赛事</h3>
          <input
            value={newTitle}
            onChange={e => setNewTitle(e.target.value)}
            placeholder="赛事名称，例如：10 月 20 日 PK 争霸赛"
          />
          <input type="date" value={newDate} onChange={e => setNewDate(e.target.value)} />
          <select value={newDayPlan} onChange={e => setNewDayPlan(Number(e.target.value))}>
            <option value={0}>白天分组方案（可稍后绑定）</option>
            {plans.map(p => <option key={p.id} value={p.id}>{p.title}</option>)}
          </select>
          <button type="submit" disabled={busy === 'create-event'}>创建赛事</button>
        </form>

        {eventId > 0 && (
          <div className="lm-setup-form">
            <h3>绑定方案</h3>
            <select value={bindDay} onChange={e => setBindDay(Number(e.target.value))}>
              <option value={0}>白天小组赛方案</option>
              {plans.map(p => <option key={p.id} value={p.id}>{p.title}</option>)}
            </select>
            <select value={bindNight} onChange={e => setBindNight(Number(e.target.value))}>
              <option value={0}>晚上晋级赛方案</option>
              {plans.map(p => <option key={p.id} value={p.id}>{p.title}</option>)}
            </select>
            <button type="button" onClick={bindPlans} disabled={!!busy || (!bindDay && !bindNight)}>
              保存绑定
            </button>
          </div>
        )}
      </div>

      {!eventId && <p className="hint">先创建一个赛事，或从上方选择已有赛事。</p>}

      {ov && (
        <>
          <div className="lm-rooms">
            <div className="lm-section-head">
              <h3>直播间</h3>
              <form className="lm-inline-form" onSubmit={addMonitor}>
                <input
                  value={roomUrl}
                  onChange={e => setRoomUrl(e.target.value)}
                  placeholder="https://live.douyin.com/505899768449"
                />
                <input
                  value={roomLabel}
                  onChange={e => setRoomLabel(e.target.value)}
                  placeholder="备注（可选）"
                />
                <button type="submit" disabled={busy === 'add-monitor'}>添加</button>
              </form>
            </div>
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

          <div className="lm-workers">
            <h3>采集器</h3>
            {ov.workers.length === 0 && <p className="hint">还没有采集器心跳。按下面命令启动本地采集器。</p>}
            {ov.workers.map((w: LiveWorker) => (
              <div key={w.workerId} className="lm-worker">
                <Dot kind={w.online ? 'live' : 'idle'} />
                <span className="mono">{w.workerId}</span>
                <span className="lm-tag">{w.online ? '在线' : `离线 ${w.ageSeconds}s`}</span>
                <span className="lm-tag">{w.rooms} 个直播间</span>
                {w.lastHeartbeat && <span className="hint">最后心跳 {new Date(w.lastHeartbeat).toLocaleTimeString()}</span>}
              </div>
            ))}
            <pre className="lm-command">scripts/live/run-local.sh {eventId} {ov.rooms.map(r => r.webRid).join(' ') || 'ROOM_ID'}</pre>
          </div>

          <div className="lm-board">
            <div className="lm-scores lm-live-scores">
              <h3>
                <span className="lm-live-title"><span className="lm-live-dot" />实时分</span>
                {groupId > 0 && (
                  <select value={groupId} onChange={e => setGroupId(Number(e.target.value))}>
                    {(ov.groups ?? []).map(g => (
                      <option key={g.id} value={g.id}>{g.name}</option>
                    ))}
                  </select>
                )}
              </h3>
              <div className="lm-live-meta">
                <span><i className="lm-live-dot small" /> 实时推送 · 2 秒兜底</span>
                <span>{lastScoreAt ? `更新于 ${lastScoreAt}` : '等待第一笔分数'}</span>
                {roundId > 0 && <span>第 {roundId} 局</span>}
              </div>
              <div className="lm-live-summary">
                <div><span>当前组</span><b>{currentGroup?.name || '—'}</b></div>
                <div><span>组内音浪</span><b>{groupWave.toLocaleString('zh-CN')}</b></div>
                <div><span>在榜人数</span><b>{seats.length}</b></div>
                {hasBonusData && (
                  <div>
                    <span>加成状态</span>
                    <b className={activeBonusCount > 0 ? 'lm-bonus-live-text' : ''}>
                      {activeBonusCount > 0
                        ? `${activeBonusCount} 人加成中`
                        : hasBonusScore ? '加成已结束' : '加成待触发'}
                    </b>
                  </div>
                )}
                {currentGroup && currentGroup.qualifySlots > 0 && (
                  <div><span>晋级名额</span><b>前 {currentGroup.qualifySlots}</b></div>
                )}
              </div>
              {seats.length === 0 && <p className="lm-live-empty">这一组正在等待分数…</p>}
              <div className="lm-live-list">
                {seats.map((s, i) => {
                  const delta = scoreDeltas[s.douyinUid] || 0
                  const rankDelta = rankDeltas[s.douyinUid] || 0
                  const pct = Math.max(2, Math.round(((Number(s.score) || 0) / maxScore) * 100))
                  const qualified = !!currentGroup && currentGroup.qualifySlots > 0 && i < currentGroup.qualifySlots
                  const bonusMultiple = s.bonusMultiple ?? 100
                  const bonusScore = Math.max(0, s.bonusScore ?? 0)
                  const realScore = Math.max(0, (Number(s.score) || 0) - bonusScore)
                  return (
                    <div key={s.douyinUid} className={`lm-live-row${qualified ? ' is-qualified' : ''}`}>
                      <div className={`lm-live-rank rank-${Math.min(s.rank, 4)}`}>#{s.rank}</div>
                      <div className="lm-live-player">
                        <div className="lm-live-name-line">
                          <b className="lm-live-name">{s.nickname || s.douyinUid}</b>
                          {rankDelta !== 0 && (
                            <span className={`lm-rank-move ${rankDelta > 0 ? 'up' : 'down'}`}>
                              {rankDelta > 0 ? '↑' : '↓'}{Math.abs(rankDelta)}
                            </span>
                          )}
                          {bonusMultiple > 100 && (
                            <span
                              className={`lm-bonus-badge${s.bonusActive ? ' is-active' : ''}`}
                              title={s.bonusActive ? '当前加成窗口生效中' : '本场检测到加成任务'}
                            >
                              {s.bonusActive ? '加成中' : '加成'} {formatMultiplier(bonusMultiple)}
                            </span>
                          )}
                        </div>
                        <div className="lm-live-bar"><span style={{ width: `${pct}%` }} /></div>
                      </div>
                      <div
                        className="lm-live-value"
                        title={bonusScore > 0 ? `显示分 ${(Number(s.score) || 0).toLocaleString('zh-CN')} = 去加成 ${realScore.toLocaleString('zh-CN')} + 加成 ${bonusScore.toLocaleString('zh-CN')}` : undefined}
                      >
                        <b key={`${s.douyinUid}-${s.score}`} className={delta ? 'score-bump' : ''}>
                          {(Number(s.score) || 0).toLocaleString('zh-CN')}
                        </b>
                        {delta > 0 && <span className="lm-score-delta">+{delta.toLocaleString('zh-CN')}</span>}
                        {bonusScore > 0 && (
                          <span className="lm-real-score">去加成 {realScore.toLocaleString('zh-CN')}</span>
                        )}
                      </div>
                    </div>
                  )
                })}
              </div>
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
            <button disabled={!!busy || !eventId} onClick={() => step('晋级赛全部定稿后生成决赛？', () => live.buildFinal(eventId))}>
              生成决赛
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
                      <td>{q.source === 'direct' ? '直接晋级'
                        : q.source === 'revival' ? '复活晋级'
                        : q.source === 'final' ? '决赛'
                        : '晋级赛'}</td>
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
