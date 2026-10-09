import { useCallback, useEffect, useState } from 'react'
import { api } from './api'
import { useAutoRefresh } from './useAutoRefresh'
import { routeHref, useRoute } from './lib/router'

const fmt = (n: number) => (n >= 10000 ? `${(n / 10000).toFixed(1)} 万` : String(n))

export default function DashboardPage() {
  const [data, setData] = useState<any>(null)
  const [days, setDays] = useState(30)
  const [error, setError] = useState('')
  const [team, setTeam] = useState('male')
  const [reportUrl, setReportUrl] = useState('')
  const { go } = useRoute()

  const load = useCallback(async () => {
    setError('')
    try {
      const d = await api(`/api/admin/dashboard?days=${days}&top=10`)
      setData(d)
      // 空库（全新部署 / 刚清空音浪数据）没有基准日，日报 SVG 也没得画
      if (d?.summary?.date) {
        const token = sessionStorage.getItem('pk_token') ?? ''
        setReportUrl(
          `/api/admin/report/daily.svg?date=${d.summary.date}&team=${team}&access_token=${token}`,
        )
      } else {
        setReportUrl('')
      }
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }, [days, team])

  useEffect(() => {
    load()
  }, [load])

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

      <div className="cards">
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

        <section className="card">
          <h2>未播预警（连续 ≥2 天）</h2>
          {data.absent.length === 0 ? (
            <div className="empty">没有人连续未播 👍</div>
          ) : (
            <table>
              <thead>
                <tr>
                  <th>主播</th>
                  <th>连续未播</th>
                </tr>
              </thead>
              <tbody>
                {data.absent.map((r: any) => (
                  <tr key={r.personId}>
                    <td>{r.name}</td>
                    <td className="bad-num">{r.days === -1 ? '窗口内从未开播' : `${r.days} 天`}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>
      </div>

      <section className="card">
        <h2>团别音浪趋势（近 {days} 天）</h2>
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
            {[...data.trend].reverse().map((t: any) => (
              <tr key={t.date}>
                <td>{t.date}</td>
                <td className="num">{fmt(t.male)}</td>
                <td className="num">{fmt(t.female)}</td>
                <td className="num">{fmt(t.total)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>
    </div>
  )
}
