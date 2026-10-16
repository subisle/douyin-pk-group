import { useCallback, useEffect, useState } from 'react'
import { api } from './api'
import { useAutoRefresh } from './useAutoRefresh'
import { routeHref, useRoute } from './lib/router'

const fmt = (n: number) => (n >= 10000 ? `${(n / 10000).toFixed(1)} 万` : String(n))

/* ---------------- 团别音浪趋势折线图（手绘内联 SVG，零依赖） ----------------
   项目刻意不引图表库（packages 里没有 echarts/recharts），所以这里自己画。
   trend-scale:begin / end 之间是纯数学（数值 → 坐标、刻度取整），不碰 React、
   不碰浏览器 API，可以整段抠出来用 node 做边界自检（见交付说明里的自检脚本）。 */
/* trend-scale:begin */
/** 把步长向上取整到 1/2/5 × 10^k 的「整刻度」——刻度值才不会出现 3721 这种数。 */
function niceStep(v: number): number {
  if (!(v > 0) || !Number.isFinite(v)) return 1
  const mag = Math.pow(10, Math.floor(Math.log10(v)))
  const norm = v / mag
  const mul = norm <= 1 ? 1 : norm <= 2 ? 2 : norm <= 5 ? 5 : 10
  return mul * mag
}

/** 取一批数值的最大值：负值 / NaN / undefined 一律按 0 计。 */
function trendMax(values: number[]): number {
  let m = 0
  for (const v of values) if (Number.isFinite(v) && v > m) m = v
  return m
}

/** Y 轴上界 + 网格刻度。max<=0（空数据 / 全 0）时给一条固定的 0~4 刻度：
    既避免后面出现 0 除，也让全 0 的线贴底时仍有正常网格可看。 */
function yScaleOf(max: number): { top: number; step: number; ticks: number[] } {
  if (!(max > 0) || !Number.isFinite(max)) return { top: 4, step: 1, ticks: [0, 1, 2, 3, 4] }
  // 先按「目标 4 段」取整步长；再把上界抬到步长的整数倍。
  // 取整到 1/2/5 后段数只会变少，实际落在 3~5 条网格线之间（自检里逐例验过）。
  const step = niceStep(max / 4)
  const top = Math.max(step, Math.ceil(max / step) * step)
  const ticks: number[] = []
  for (let i = 0; i * step <= top + step * 1e-6 && i <= 12; i++) ticks.push(i * step)
  return { top, step, ticks }
}

/** X 轴落点：n=0 无点；n=1 单点居中（单点连线会画出畸形图，调用方不连线）；
    n>=2 时首尾贴住左右边距、中间均匀铺开。 */
function xAt(i: number, n: number, left: number, plotW: number): number {
  if (n <= 1) return left + plotW / 2
  return left + (plotW * i) / (n - 1)
}

/** Y 轴落点：音浪值 → 像素（基线 base，向上为正）。top 恒 > 0，这里再兜一层防 0 除。 */
function yAt(v: number, top: number, base: number, plotH: number): number {
  if (!(top > 0)) return base
  const k = Number.isFinite(v) ? v / top : 0
  return base - Math.min(Math.max(k, 0), 1) * plotH
}

/** X 轴日期标签抽稀：最多 maxLabels 个（90 天时逐日标签会糊成一片）。
    末点一律补上 —— 否则最新一天没有标签，等于丢信息。补出来的末点与前一格最多
    只差 1 格，实测最挤的一档是 n=8/14（点间距 52~98px），仍大于「MM-DD」标签宽
    （12px 字号约 42px），不会重叠。 */
function labelIndexes(n: number, maxLabels: number): number[] {
  if (n <= 0) return []
  if (n <= maxLabels) return Array.from({ length: n }, (_, i) => i)
  const every = Math.ceil(n / maxLabels)
  const out: number[] = []
  for (let i = 0; i < n; i += every) out.push(i)
  const last = n - 1
  if (out[out.length - 1] !== last) out.push(last)
  return out
}
/* trend-scale:end */

/* 画布常量：viewBox 逻辑尺寸。CSS 里 max-width 与 CW 同宽 → 桌面端 1:1 渲染，
   刻度字不会被拉大；窄屏等比缩小（不写死 px 宽，页面不会横向滚动）。 */
const CW = 760
const CH = 250
/* 右内边距 26 而不是 16：X 轴最后一个标签以 x=CW-PAD.r 居中，半宽约 20px
   （「MM-DD」12px 字号），PAD.r 太小时标签会被 viewBox 右边缘裁掉（实测 16 时
   最后一天显示成「09-3C」）。左内边距 58 留给 Y 轴刻度（「15.0 万」）。 */
const PAD = { t: 14, r: 26, b: 26, l: 58 }
const C_MALE = '#007AFF' // 男团蓝
const C_FEMALE = '#db2777' // = --pink
const C_TOTAL = '#98a2b3' // = --ink3，合计走虚线弱化
const C_GRID = '#10182814' // ≈ --line（#101828 8%）；SVG 属性里 var() 不生效，故写字面色值

const SERIES = [
  { key: 'male' as const, name: '男团', color: C_MALE, dash: undefined },
  { key: 'female' as const, name: '女团', color: C_FEMALE, dash: undefined },
  { key: 'total' as const, name: '合计', color: C_TOTAL, dash: '5 4' },
]

/** 团别音浪趋势折线图：三条线（男团 / 女团 / 合计）+ 网格刻度 + 原生 tooltip。 */
function TrendChart({ trend }: { trend: any[] }) {
  const num = (v: any) => (Number.isFinite(Number(v)) && Number(v) > 0 ? Number(v) : 0)
  /* 后端 dashboard 按日期正序给 trend，但原表格是 `[...trend].reverse()`（最新在上）。
     图表 X 轴必须左旧右新，所以这里显式按 date 再排一次，不依赖上游顺序
     （YYYY-MM-DD 字典序 = 时间序）。 */
  const pts = (trend ?? [])
    .map((t: any) => ({
      date: String(t?.date ?? ''),
      male: num(t?.male),
      female: num(t?.female),
      total: num(t?.total),
    }))
    .filter((p) => p.date)
    .sort((a, b) => a.date.localeCompare(b.date))

  if (pts.length === 0) return <div className="empty">暂无数据</div>

  const plotW = CW - PAD.l - PAD.r
  const plotH = CH - PAD.t - PAD.b
  const base = PAD.t + plotH
  const scale = yScaleOf(trendMax(pts.flatMap((p) => [p.male, p.female, p.total])))
  const x = (i: number) => xAt(i, pts.length, PAD.l, plotW)
  const y = (v: number) => yAt(v, scale.top, base, plotH)
  // 只有 ≥2 天才画线；单点场景不生成 polyline（points="" 是畸形节点，由描点表达）
  const line = (k: 'male' | 'female' | 'total') => pts.map((p, i) => `${x(i)},${y(p[k])}`).join(' ')
  const labels = labelIndexes(pts.length, 7)
  // 悬停热区：每天一条竖带，鼠标落在该天任意高度都能取到数值（原生 tooltip 命中面积太小）
  const bandW = plotW / Math.max(1, pts.length)
  const tip = (p: (typeof pts)[number]) =>
    `${p.date} 男团 ${fmt(p.male)} / 女团 ${fmt(p.female)} / 合计 ${fmt(p.total)}`

  return (
    <div className="trend-chart">
      <svg viewBox={`0 0 ${CW} ${CH}`} role="img" aria-label={`近 ${pts.length} 天团别音浪趋势折线图`}>
        {/* 网格线 + Y 轴刻度（音浪，沿用 fmt() 的「万」口径） */}
        {scale.ticks.map((v) => (
          <g key={v}>
            <line x1={PAD.l} y1={y(v)} x2={CW - PAD.r} y2={y(v)} stroke={C_GRID} strokeWidth={1} />
            <text x={PAD.l - 8} y={y(v)} textAnchor="end" dominantBaseline="middle" fontSize={12} fill={C_TOTAL}>
              {fmt(v)}
            </text>
          </g>
        ))}
        {/* X 轴基线 + 抽稀后的日期标签（MM-DD） */}
        <line x1={PAD.l} y1={base} x2={CW - PAD.r} y2={base} stroke={C_GRID} strokeWidth={1} />
        {labels.map((i) => (
          <text key={pts[i].date} x={x(i)} y={base + 17} textAnchor="middle" fontSize={12} fill={C_TOTAL}>
            {pts[i].date.slice(5)}
          </text>
        ))}
        {/* 三条折线：合计虚线弱化，男/女实线。只有 1 天时不画线（见上面的 line 注释） */}
        {pts.length >= 2 &&
          SERIES.map((s) => (
            <polyline
              key={s.key}
              points={line(s.key)}
              fill="none"
              stroke={s.color}
              strokeWidth={s.key === 'total' ? 1.5 : 2}
              strokeDasharray={s.dash}
              strokeLinejoin="round"
              strokeLinecap="round"
            />
          ))}
        {/* 每个数据点一个圆点，<title> 给完整三元组（悬停任一曲线都能看全三项） */}
        {pts.map((p, i) => (
          <g key={p.date}>
            {SERIES.map((s) => (
              <circle key={s.key} cx={x(i)} cy={y(p[s.key])} r={3} fill={s.color}>
                <title>{tip(p)}</title>
              </circle>
            ))}
          </g>
        ))}
        {/* 每天一条透明竖带，扩大悬停命中面积 */}
        {pts.map((p, i) => {
          const left = Math.max(PAD.l, x(i) - bandW / 2)
          const right = Math.min(CW - PAD.r, x(i) + bandW / 2)
          return (
            <rect key={`hit-${p.date}`} x={left} y={PAD.t} width={Math.max(1, right - left)} height={plotH} fill="transparent">
              <title>{tip(p)}</title>
            </rect>
          )
        })}
      </svg>
      <div className="trend-legend">
        {SERIES.map((s) => (
          <span key={s.key} className={s.dash ? 'lg-dashed' : undefined} style={{ color: s.color }}>
            <i />
            {s.name}
          </span>
        ))}
        <span className="hint">纵轴：音浪（不足 1 万显示原值）</span>
      </div>
      {pts.length === 1 && <div className="hint trend-note">窗口内只有 1 天数据，仅描点、不连线。</div>}
    </div>
  )
}

export default function DashboardPage() {
  const [data, setData] = useState<any>(null)
  const [days, setDays] = useState(30)
  const [error, setError] = useState('')
  const [team, setTeam] = useState('male')
  const [reportUrl, setReportUrl] = useState('')
  const { go } = useRoute()

  /* 日报 SVG 只认 Authorization 头（审计后 `?access_token=` 已废），
     而 <a href> 带不了请求头 → 先 fetch 成 blob 再挂 object URL
     （与「消息下发」页的图片预览同一套写法）。 */
  const loadReport = useCallback(async (date: string, t: string) => {
    try {
      const resp = await fetch(`/api/admin/report/daily.svg?date=${date}&team=${t}`, {
        headers: { Authorization: `Bearer ${sessionStorage.getItem('pk_token') ?? ''}` },
      })
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`)
      setReportUrl(URL.createObjectURL(await resp.blob()))
    } catch {
      setReportUrl('')   // 日报图拿不到不挡主数据：KPI 照常显示
    }
  }, [])

  const load = useCallback(async () => {
    setError('')
    try {
      const d = await api(`/api/admin/dashboard?days=${days}&top=10`)
      setData(d)
      // 空库（全新部署 / 刚清空音浪数据）没有基准日，日报 SVG 也没得画
      if (d?.summary?.date) await loadReport(d.summary.date, team)
      else setReportUrl('')
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }, [days, team, loadReport])

  useEffect(() => {
    load()
  }, [load])

  useEffect(() => {
    // 日报图是 blob URL：换图 / 卸载时用完即还
    return () => {
      if (reportUrl) URL.revokeObjectURL(reportUrl)
    }
  }, [reportUrl])

  // 机器人/网页导入、台账变更、数据源切换后自动重拉
  useAutoRefresh(load, ['import', 'roster', 'source'])

  if (error) return <div className="page"><div className="alert bad">{error}</div></div>
  if (!data)
    return (
      <div className="page">
        <div className="empty">加载中…</div>
      </div>
    )

  /* 空库：还没有任何采集日期（全新部署 / 音浪数据被清空）。
     不显示假 KPI、不报红，直接引导去导入第一批数据。 */
  if (data.empty) {
    return (
      <div className="page">
        <section className="card">
          <h2>数据概况</h2>
          <div className="empty">
            还没有采集到任何日期 · 台账里已有 {data.summary?.personCount ?? 0} 人在册，
            <a
              className="linkish"
              href={routeHref('import')}
              onClick={(e) => {
                if (e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return
                e.preventDefault()
                go('import')
              }}
            >
              去「数据导入」
            </a>
            导入第一批音浪数据
          </div>
        </section>
      </div>
    )
  }

  const s = data.summary

  return (
    <div className="page">
      <div className="row">
        <label>
          窗口{' '}
          <select value={days} onChange={(e) => setDays(Number(e.target.value))}>
            <option value={7}>近 7 天</option>
            <option value={14}>近 14 天</option>
            <option value={30}>近 30 天</option>
            <option value={90}>近 90 天</option>
          </select>
        </label>
        <label>
          日报团别{' '}
          <select value={team} onChange={(e) => setTeam(e.target.value)}>
            <option value="male">男团</option>
            <option value="female">女团</option>
          </select>
        </label>
        <a className="file-btn" href={reportUrl} target="_blank" rel="noreferrer">
          打开日报 SVG
        </a>
      </div>

      <div className="cards cards-kpi">
        <section className="card kpi">
          <span className="hint">统计日期</span>
          <b>{s.date}</b>
        </section>
        <section className="card kpi k-violet">
          <span className="hint">当日总音浪</span>
          <b className="num">{fmt(s.totalWave)}</b>
        </section>
        <section className="card kpi k-good">
          <span className="hint">男团 / 女团</span>
          <b className="num">
            {fmt(s.maleWave)} / {fmt(s.femaleWave)}
          </b>
        </section>
        <section className="card kpi k-warn">
          <span className="hint">开播 / 在册</span>
          <b className="num">
            {s.liveCount} / {s.personCount}
          </b>
        </section>
      </div>

      <div className="cards">
        <section className="card">
          <h2>音浪 Top {data.top.length}</h2>
          {data.top.length === 0 ? (
            <div className="empty">暂无数据</div>
          ) : (
            <table>
              <thead>
                <tr>
                  <th>主播</th>
                  <th>团别</th>
                  <th>当日音浪</th>
                  <th>分钟</th>
                </tr>
              </thead>
              <tbody>
                {data.top.map((r: any) => (
                  <tr key={r.personId}>
                    <td>{r.name}</td>
                    <td>{r.team === 'male' ? '男' : '女'}</td>
                    <td className="num">{fmt(r.wave)}</td>
                    <td className="num">{r.minutes}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>

        <section className="card card-absent">
          <h2>未播预警（连续 ≥2 天）</h2>
          {(() => {
            /* 「窗口内从未开播」（days=-1）不算连续未播预警，不再逐条红字刷屏，
               收成卡片底部一行灰字名单；表格只留真正的连续未播（≥2 天）。 */
            const warned = data.absent.filter((r: any) => r.days >= 2)
            const neverLive = data.absent.filter((r: any) => r.days === -1)
            return (
              <>
                {warned.length === 0 ? (
                  <div className="empty">没有人连续未播 👍</div>
                ) : (
                  <div className="absent-scroll">
                    <table>
                      <thead>
                        <tr>
                          <th>主播</th>
                          <th>连续未播</th>
                        </tr>
                      </thead>
                      <tbody>
                        {warned.map((r: any) => (
                          <tr key={r.personId}>
                            <td>{r.name}</td>
                            <td className="bad-num">{r.days} 天</td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                )}
                {neverLive.length > 0 && (
                  <div className="absent-never hint">
                    窗口内从未开播（{neverLive.length}）：
                    {neverLive.map((r: any) => r.name).join('、')}
                  </div>
                )}
              </>
            )
          })()}
        </section>
      </div>

      <section className="card">
        <h2>团别音浪趋势（近 {days} 天）</h2>
        {/* 曲线图在上；原来的逐日明细不删信息，收进 <details> 里（仍是反序：最新在上） */}
        <TrendChart trend={data.trend} />
        <details className="trend-detail">
          <summary>明细表（{data.trend?.length ?? 0} 天）</summary>
          <table>
            <thead>
              <tr>
                <th>日期</th>
                <th>男团</th>
                <th>女团</th>
                <th>合计</th>
              </tr>
            </thead>
            <tbody>
              {[...(data.trend ?? [])].reverse().map((t: any) => (
                <tr key={t.date}>
                  <td>{t.date}</td>
                  <td className="num">{fmt(t.male)}</td>
                  <td className="num">{fmt(t.female)}</td>
                  <td className="num">{fmt(t.total)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </details>
      </section>
    </div>
  )
}
