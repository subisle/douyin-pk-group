import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import './public-live.css'
import { live } from './api'
import type { LiveGroup, LiveStage, PublicLiveGroup, PublicLivePlayer, PublicLiveSnapshot } from './api'

const STAGE_ORDER = [
  'first_wave_group',
  'first_wave_revival',
  'rest_group',
  'rest_revival',
  'promotion',
  'final',
] as const

const STAGE_META: Record<string, { short: string; title: string; hint: string }> = {
  first_wave_group: { short: '前四组', title: '前四组 · 小组赛', hint: '每组 10 分钟 · 前 4 名晋级' },
  first_wave_revival: { short: '复活赛', title: '前四组 · 复活赛', hint: '淘汰者复活 · 5 分钟' },
  rest_group: { short: '后续小组', title: '后续小组赛', hint: '每组 10 分钟 · 前 4 名晋级' },
  rest_revival: { short: '后续复活', title: '后续小组 · 复活赛', hint: '淘汰者复活 · 5 分钟' },
  promotion: { short: '晋级赛', title: '晋级赛', hint: '小组第一进入决赛' },
  final: { short: '决赛', title: '总决赛', hint: '决出冠亚季军' },
}

const LIVE_STATUSES = new Set(['running', 'settling', 'pending_review', 'waiting_restart'])
const FINISH_HOLD_MS = 8000

function stageIndex(kind: string) {
  const index = STAGE_ORDER.indexOf(kind as (typeof STAGE_ORDER)[number])
  return index < 0 ? 99 : index
}

function stageLabel(kind: string) {
  return STAGE_META[kind]?.title ?? kind
}

function groupStatus(group: PublicLiveGroup) {
  if (group.status === 'finalized') return { text: '结束阶段', tone: 'done' }
  if (group.status === 'running') return { text: 'PK 进行中', tone: 'live' }
  if (group.status === 'settling') return { text: '结束结算中', tone: 'warn' }
  if (group.status === 'pending_review') return { text: '等待确认', tone: 'warn' }
  if (group.status === 'waiting_restart') return { text: '等待重开', tone: 'warn' }
  return { text: '准备阶段', tone: 'idle' }
}

function stageStatusLabel(stage: LiveStage | undefined) {
  if (!stage) return '未生成'
  if (stage.status === 'running') return '进行中'
  if (stage.status === 'finalized') return '已结束'
  return '待开始'
}

function timeText(value: string | null) {
  if (!value) return '—'
  const d = new Date(value)
  if (Number.isNaN(d.getTime())) return '—'
  return d.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit', second: '2-digit' })
}

function clockText(seconds: number) {
  const safe = Math.max(0, Math.ceil(seconds))
  const m = Math.floor(safe / 60)
  const s = safe % 60
  return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`
}

/** 只描述直播间上报的真实状态；本页计时永远不能推进比赛。 */
function groupPhase(group: PublicLiveGroup, nowMs: number) {
  if (group.status === 'finalized') {
    return { text: '已结束', detail: '结果已按直播间 PK 结束信号确认', urgent: false }
  }
  if (group.status === 'settling') {
    return { text: '等待结束', detail: '直播间 PK 已结束，等待最终分数', urgent: true }
  }
  if (group.status === 'pending_review') {
    return { text: '待确认', detail: '等待核对直播对局', urgent: true }
  }
  if (group.status === 'waiting_restart') {
    return { text: '待重开', detail: '直播间异常结束，等待下一场', urgent: true }
  }
  if (group.status === 'running') {
    const start = group.round?.startAt ? Date.parse(group.round.startAt) : NaN
    const elapsed = Number.isNaN(start) ? '' : ` · 实际已进行 ${clockText(Math.max(0, (nowMs - start) / 1000))}`
    return { text: 'PK 进行中', detail: `跟随直播间实际对局${elapsed}`, urgent: false }
  }
  return { text: '准备中', detail: '整组人齐后开始 PK', urgent: false }
}

function playerName(player: PublicLivePlayer) {
  // 对外展示只使用台账姓名；直播昵称会随活动文案和改名变化，不能作为身份字段。
  return player.personName || player.douyinUid || '未登记'
}

/** 分数按 600ms 平滑滚动到新值，避免每次轮询直接跳成新数字。 */
function AnimatedScore({ value }: { value: number }) {
  const target = Math.max(0, Number(value) || 0)
  const [shown, setShown] = useState(target)
  const [updating, setUpdating] = useState(false)
  const shownRef = useRef(target)

  useEffect(() => {
    const from = shownRef.current
    if (from === target) {
      setShown(target)
      return
    }
    setUpdating(true)
    const started = performance.now()
    let raf = 0
    const tick = (now: number) => {
      const progress = Math.min(1, (now - started) / 600)
      const eased = 1 - Math.pow(1 - progress, 3)
      const next = Math.round(from + (target - from) * eased)
      shownRef.current = next
      setShown(next)
      if (progress < 1) {
        raf = requestAnimationFrame(tick)
      } else {
        shownRef.current = target
        setShown(target)
        setUpdating(false)
      }
    }
    raf = requestAnimationFrame(tick)
    return () => cancelAnimationFrame(raf)
  }, [target])

  return (
    <strong className={`plm-score${updating ? ' is-updating' : ''}`}>
      {shown.toLocaleString('zh-CN')}
    </strong>
  )
}

function GroupCard({ group, nowMs, qualify }: { group: PublicLiveGroup; nowMs: number; qualify: number }) {
  const status = groupStatus(group)
  const phase = groupPhase(group, nowMs)
  const maxScore = Math.max(1, ...group.players.map((p) => Number(p.score) || 0))
  const revival = group.stageKind.includes('revival')
  const live = LIVE_STATUSES.has(group.status)
  const scored = group.players.filter((p) => p.rank > 0).length

  return (
    <article className={`plm-group-card${revival ? ' is-revival' : ''}${live ? ' is-live' : ''}`}>
      <header className="plm-group-head">
        <div>
          <div className="plm-group-kicker">
            {revival ? '复活赛' : '小组赛'} · 第 {group.seq} 组
          </div>
          <h3>{group.name || `第 ${group.seq} 组`}</h3>
        </div>
        <span className={`plm-status plm-${status.tone}`}><i />{status.text}</span>
      </header>

      <div className="plm-group-meta">
        <span>规则时长 {Math.round(group.durationSecs / 60)} 分钟 · 实际以直播间为准</span>
        <span>
          {revival
            ? `前 ${qualify} 名复活 · 后 ${Math.max(group.players.length - qualify, 0)} 名淘汰`
            : `前 ${qualify} 名晋级 · 后 ${Math.max(group.players.length - qualify, 0)} 名进复活池`}
        </span>
        <span>{scored}/{group.players.length} 人已出分</span>
        <strong className={phase.urgent ? 'is-urgent' : ''}>{phase.text}</strong>
      </div>

      <div className="plm-player-list">
        {group.players.length === 0 && <div className="plm-empty">分组等待生成…</div>}
        {group.players.map((player, index) => {
          const position = player.position || index + 1
          const scored = player.rank > 0
          const qualified = scored && position <= qualify
          const zone = !scored
            ? ''
            : qualified
              ? (revival ? '复活区' : '晋级区')
              : (revival ? '淘汰区' : '复活区')
          const zoneClass = zone === '晋级区'
            ? 'is-promoted'
            : zone === '复活区'
              ? 'is-revival-zone'
              : zone === '淘汰区'
                ? 'is-eliminated-zone'
                : ''
          const pct = Math.max(2, Math.round(((Number(player.score) || 0) / maxScore) * 100))
          return (
            <div key={`${player.douyinUid || player.personId || 'p'}-${index}`} className={`plm-player${qualified ? ' is-qualified' : ''}${zoneClass ? ` ${zoneClass}` : ''}`}>
              <span className="plm-rank">{scored ? `#${position}` : '—'}</span>
              <div className="plm-player-main">
                <div className="plm-player-name">
                  <b>{playerName(player)}</b>
                  {zone && <span className={`plm-zone ${zoneClass}`}>{zone}</span>}
                </div>
                <div className="plm-score-track"><i style={{ width: `${pct}%` }} /></div>
              </div>
              <AnimatedScore value={Number(player.score) || 0} />
            </div>
          )
        })}
      </div>

      <footer className="plm-group-foot">
        <span>
          {group.status === 'pending'
            ? '准备阶段：整组人齐后开始 PK'
            : group.round?.startAt
              ? `开赛 ${timeText(group.round.startAt)}`
              : '等待采集到 PK 对局'}
        </span>
        {group.round?.observedAt && <span>更新 {timeText(group.round.observedAt)}</span>}
      </footer>
    </article>
  )
}

function sortGroups<T extends LiveGroup>(groups: T[]): T[] {
  return [...groups].sort((a, b) => stageIndex(a.stageKind) - stageIndex(b.stageKind) || a.seq - b.seq)
}

export default function PublicLiveMonitorPage({ token }: { token?: string }) {
  const [snapshot, setSnapshot] = useState<PublicLiveSnapshot | null>(null)
  const [error, setError] = useState('')
  const [selectedStage, setSelectedStage] = useState('')
  const [nowMs, setNowMs] = useState(Date.now())
  const [offsetMs, setOffsetMs] = useState(0)
  const requestSeq = useRef(0)
  const lastAutoStage = useRef('')

  const load = useCallback(async () => {
    const seq = ++requestSeq.current
    try {
      const next = token ? await live.publicEvent(token) : await live.publicCurrent()
      if (seq !== requestSeq.current) return
      setSnapshot(next)
      const server = Date.parse(next.serverNow)
      setOffsetMs(Number.isNaN(server) ? 0 : server - Date.now())
      setError('')
    } catch (e: any) {
      if (seq !== requestSeq.current) return
      setError(e?.message ?? String(e))
    }
  }, [token])

  useEffect(() => {
    let stopped = false
    let refreshTimer: number | null = null
    let source: EventSource | null = null

    const scheduleRefresh = () => {
      if (refreshTimer != null || stopped) return
      refreshTimer = window.setTimeout(() => {
        refreshTimer = null
        if (!stopped && document.visibilityState !== 'hidden' && navigator.onLine !== false) {
          void load()
        }
      }, 350)
    }
    const connect = () => {
      source?.close()
      source = null
      if (stopped || document.visibilityState === 'hidden' || navigator.onLine === false) return
      source = new EventSource('/api/public/events')
      source.addEventListener('change', (ev) => {
        try {
          const data = JSON.parse((ev as MessageEvent).data) as { scope?: string }
          if (!data.scope || data.scope === 'all' || data.scope === 'live') scheduleRefresh()
        } catch {
          scheduleRefresh()
        }
      })
    }
    const onVisibility = () => {
      if (document.visibilityState === 'hidden') {
        source?.close()
        source = null
        if (refreshTimer != null) window.clearTimeout(refreshTimer)
        refreshTimer = null
        return
      }
      void load()
      connect()
    }
    const onOnline = () => {
      void load()
      connect()
    }
    const onOffline = () => {
      source?.close()
      source = null
    }

    void load()
    connect()
    const fallback = window.setInterval(() => {
      if (document.visibilityState !== 'hidden' && navigator.onLine !== false) void load()
    }, 3000)
    const ticker = window.setInterval(() => setNowMs(Date.now()), 1000)
    document.addEventListener('visibilitychange', onVisibility)
    window.addEventListener('online', onOnline)
    window.addEventListener('offline', onOffline)
    return () => {
      stopped = true
      requestSeq.current += 1
      source?.close()
      window.clearInterval(fallback)
      window.clearInterval(ticker)
      if (refreshTimer != null) window.clearTimeout(refreshTimer)
      document.removeEventListener('visibilitychange', onVisibility)
      window.removeEventListener('online', onOnline)
      window.removeEventListener('offline', onOffline)
    }
  }, [load])

  useEffect(() => {
    if (snapshot?.event.title) document.title = `${snapshot.event.title} · 实时比分`
    return () => { document.title = '抖音数据管理' }
  }, [snapshot?.event.title])

  const groups = useMemo(() => sortGroups(snapshot?.groups ?? []), [snapshot?.groups])
  const stages = useMemo(
    () => [...(snapshot?.stages ?? [])].sort((a, b) => a.seq - b.seq),
    [snapshot?.stages],
  )
  const serverNow = nowMs + offsetMs
  const currentGroup = useMemo(() => {
    const active = groups.find((g) => LIVE_STATUSES.has(g.status) && g.status !== 'pending_review')
      ?? groups.find((g) => LIVE_STATUSES.has(g.status))
    if (active) return active

    // 刚结束的一组先保留 8 秒结果，避免结算分数一出现就立刻切走。
    const recentlyFinished = groups
      .filter((g) => g.status === 'finalized' && g.finalizedAt)
      .sort((a, b) => Date.parse(b.finalizedAt || '') - Date.parse(a.finalizedAt || ''))
      .find((g) => serverNow - Date.parse(g.finalizedAt || '') < FINISH_HOLD_MS)
    if (recentlyFinished) return recentlyFinished

    return groups.find((g) => g.stageStatus !== 'finalized')
      ?? groups.filter((g) => g.status === 'finalized').at(-1)
      ?? null
  }, [groups, serverNow])
  const currentStage = currentGroup?.stageKind ?? ''
  const stageGroups = useMemo(
    () => groups.filter((g) => g.stageKind === selectedStage),
    [groups, selectedStage],
  )
  const selectedMeta = STAGE_META[selectedStage]
  const selectedStageInfo = stages.find((s) => s.kind === selectedStage)
  const event = snapshot?.event
  const qualifyDefault = event?.rules.directQualifyPerGroup || 4
  const activePhase = currentGroup ? groupPhase(currentGroup, serverNow) : null

  useEffect(() => {
    if (!currentStage) return
    if (!selectedStage || currentStage !== lastAutoStage.current) {
      setSelectedStage(currentStage)
      lastAutoStage.current = currentStage
    }
  }, [currentStage, selectedStage])

  const visibleStages = STAGE_ORDER.filter((kind) => {
    if (groups.some((g) => g.stageKind === kind) || stages.some((s) => s.kind === kind)) return true
    return kind === 'first_wave_group'
  })

  if (!snapshot && !error) {
    return <div className="plm-shell plm-centered"><div className="plm-loader" /><p>正在连接实时比分…</p></div>
  }
  if (!snapshot && error) {
    return (
      <div className="plm-shell plm-centered">
        <div className="plm-error-card">
          <span>{token ? '直播链接不可用' : '直播尚未开始'}</span>
          <h1>{error}</h1>
          <p>{token ? '请向运营确认最新公开链接。' : '当前没有进行中的赛事，开赛后页面会自动刷新。'}</p>
        </div>
      </div>
    )
  }

  return (
    <div className="plm-shell">
      <div className="plm-aurora plm-aurora-a" />
      <div className="plm-aurora plm-aurora-b" />
      <header className="plm-header">
        <div>
          <div className="plm-eyebrow"><i /> LIVE · 实时比分</div>
          <h1>{event?.title}</h1>
          <p>{event?.eventDate || '比赛日'} · 小组赛每组 10 分钟 · 复活赛 5 分钟</p>
        </div>
        <div className="plm-header-side">
          <span className="plm-fresh">自动更新</span>
          <span className="plm-link-chip">对外公开</span>
        </div>
      </header>

      <section className="plm-hero">
        <div className="plm-hero-main">
          <span className="plm-kicker">当前赛程</span>
          <h2>{stageLabel(currentStage || 'first_wave_group')}</h2>
          <p>{currentGroup ? `${currentGroup.name} · ${groupStatus(currentGroup).text}` : '等待下一阶段自动开始'}</p>
        </div>
        <div className="plm-hero-clock">
          <span className={`plm-phase-state${activePhase?.urgent ? ' is-urgent' : ''}`}>{activePhase?.text ?? '等待中'}</span>
          <small>{activePhase?.detail ?? '以直播间实际 PK 状态为准'}</small>
        </div>
        <div className="plm-summary">
          <div><b>{groups.filter((g) => g.status === 'finalized').length}</b><span>已结束</span></div>
          <div><b>{groups.filter((g) => LIVE_STATUSES.has(g.status)).length}</b><span>进行中</span></div>
          <div><b>{groups.reduce((n, g) => n + g.players.length, 0)}</b><span>参赛人次</span></div>
        </div>
      </section>

      <nav className="plm-stage-tabs" aria-label="比赛阶段">
        {visibleStages.map((kind) => {
          const stage = stages.find((s) => s.kind === kind)
          const count = groups.filter((g) => g.stageKind === kind).length
          return (
            <button key={kind} className={selectedStage === kind ? 'is-active' : ''} onClick={() => setSelectedStage(kind)}>
              <span>{STAGE_META[kind]?.short ?? kind}</span>
              <small>{count > 0 ? `${count} 组 · ${stageStatusLabel(stage)}` : stageStatusLabel(stage)}</small>
            </button>
          )
        })}
      </nav>

      <main className="plm-content">
        <div className="plm-section-head">
          <div>
            <span className="plm-kicker">分组与实时分数</span>
            <h2>{selectedMeta?.title ?? stageLabel(selectedStage)}</h2>
          </div>
          <div className="plm-section-hint">
            <span>{selectedMeta?.hint}</span>
            <span>{stageStatusLabel(selectedStageInfo)}</span>
          </div>
        </div>

        {stageGroups.length === 0 ? (
          <div className="plm-waiting">
            <span className="plm-waiting-dot" />
            <h3>{selectedStage === 'first_wave_revival' ? '前四组全部结束后自动生成复活分组' : '等待运营启动这一阶段'}</h3>
            <p>页面会自动刷新，无需手动重开链接。</p>
          </div>
        ) : (
          <div className="plm-grid">
            {stageGroups.map((group) => (
              <GroupCard
                key={group.id}
                group={group}
                nowMs={serverNow}
                qualify={group.qualifySlots || qualifyDefault}
              />
            ))}
          </div>
        )}
      </main>

      <footer className="plm-footer">
        <span>比分来自抖音 PK 实时采样</span>
        <span>小组前 4 名晋级，其余进复活池；复活赛前 4 名复活</span>
      </footer>
      {error && <div className="plm-toast">{error}</div>}
    </div>
  )
}
