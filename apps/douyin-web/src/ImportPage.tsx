import { useCallback, useEffect, useRef, useState } from 'react'
import { api } from './api'
import { useAutoRefresh } from './useAutoRefresh'

type Row = {
  name: string
  wave?: number | null
  cumulativeWave?: number | null
  minutes?: number | null
  isLive?: boolean | null
  douyinNo?: string | null
}

type CommitReport = {
  batchId: number
  total: number
  okRows: number
  /** 库里已有该人该日、值完全一样 → 没动它（相同不覆盖，不同才覆盖） */
  unchanged?: number
  failedRows: number
  failures: { name: string; reason: string }[]
  missing: string[]
}

/** 该日该团已入库情况（选日期/团别时实时查） */
type DayStatus = {
  checked: boolean
  rows?: number
  lastBatch?: { filename: string; at: string } | null
}

type DeleteDayReport = { ok: boolean; metrics: number; batches: number }

// 数据晚一天：8 号发的 CSV 是 7 号的数据 → 默认业务日期 = 前一天
const yesterday = () => {
  const d = new Date(Date.now() - 24 * 60 * 60 * 1000)
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}

/** 从文件名读日期与团别（运营习惯：2026-10-07-男团.csv）——读出来自动填表单 */
function hintsFromFilename(name: string): { date?: string; team?: string } {
  const out: { date?: string; team?: string } = {}
  const m = name.match(/(\d{4})-(\d{2})-(\d{2})/)
  if (m) {
    const cand = `${m[1]}-${m[2]}-${m[3]}`
    if (!Number.isNaN(new Date(cand).getTime())) out.date = cand
  }
  if (/男团|male/i.test(name)) out.team = 'male'
  else if (/女团|female/i.test(name)) out.team = 'female'
  return out
}

/** 导入页：只认文件（拖拽 / 选文件），解析结果走弹窗预览，确认后才入库。
    粘贴入口已按需求移除——管理端一律在 PC 上用导出文件导入，手抄文本容易错。 */
export default function ImportPage() {
  const [rows, setRows] = useState<Row[]>([])
  const [filename, setFilename] = useState('')
  const [team, setTeam] = useState('male')
  const [bizDate, setBizDate] = useState(yesterday())
  const [mode, setMode] = useState('merge')
  const [busy, setBusy] = useState(false)
  const [parsing, setParsing] = useState(false)
  /** 业务日期是从文件名推出来的（不是默认的前一天），弹窗里要标出来 */
  const [dateFromFile, setDateFromFile] = useState(false)
  const [error, setError] = useState('')
  const [report, setReport] = useState<CommitReport | null>(null)
  const [batches, setBatches] = useState<any[]>([])
  /** 拖拽悬停高亮 */
  const [dragOver, setDragOver] = useState(false)
  /** 该日已有数据状态：rows == null 表示还没查/没选 */
  const [dayStatus, setDayStatus] = useState<DayStatus>({ checked: false })
  /** 入库后再查一次当日状态：否则「删除当前日期 / 团别数据」按钮还停在入库前的禁用态 */
  const [statusNonce, setStatusNonce] = useState(0)
  const fileInputRef = useRef<HTMLInputElement>(null)

  /** 预览弹窗：解析出内容就打开；取消/入库后 rows 清空即关闭 */
  const previewOpen = rows.length > 0

  const loadBatches = useCallback(() => {
    api('/api/admin/import/batches')
      .then((d) => setBatches(d.items ?? []))
      .catch((e) => setError(String(e.message ?? e)))
  }, [])

  useEffect(loadBatches, [loadBatches])

  // 机器人私聊发 CSV 导入后，这里的批次列表要立刻出现新记录
  useAutoRefresh(loadBatches, ['import'])

  /** 选了日期/团别就实时查该日已入库行数 → 提示「覆盖 / 新增」口径。
      文件也是可能几秒内连续换的，300ms 抖一下省请求。 */
  useEffect(() => {
    if (!bizDate || !team) {
      setDayStatus({ checked: false })
      return
    }
    setDayStatus((s) => ({ ...s, checked: false }))
    const t = setTimeout(() => {
      api<DayStatus>(
        `/api/admin/import/day-status?bizDate=${bizDate}&team=${team}`,
      )
        .then((d) => setDayStatus(d))
        .catch(() => setDayStatus({ checked: false }))
    }, 300)
    return () => clearTimeout(t)
  }, [bizDate, team, statusNonce])

  /** 预览弹窗开着时按 Esc = 取消（还没入库，关掉不丢数据） */
  useEffect(() => {
    if (!previewOpen) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') closePreview()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [previewOpen])

  async function uploadFile(file: File) {
    reset()
    // 文件名带日期/团别就直接带上（2026-10-07-男团.csv 这类），不用再手选
    const hints = hintsFromFilename(file.name)
    setDateFromFile(!!hints.date)
    if (hints.date) setBizDate(hints.date)
    if (hints.team) setTeam(hints.team)
    const fd = new FormData()
    fd.append('file', file)
    fd.append('filename', file.name)
    setParsing(true)
    try {
      const d = await api<{ filename: string; rows: Row[] }>('/api/admin/import/upload', {
        method: 'POST',
        body: fd,
      })
      if (!d.rows?.length) {
        setError('没解析出任何主播行：确认文件是 CSV / TSV / 文本，且每行至少一个名字。')
        return
      }
      setRows(d.rows)
      setFilename(d.filename)
    } catch (e: any) {
      setError(e.message ?? String(e))
    } finally {
      setParsing(false)
    }
  }

  async function commit() {
    setBusy(true)
    reset()
    try {
      const d = await api<CommitReport>('/api/admin/import/commit', {
        method: 'POST',
        body: JSON.stringify({ rows, team, bizDate, mode, filename }),
      })
      setReport(d)
      closePreview()
      loadBatches()
      setStatusNonce((n) => n + 1)
    } catch (e: any) {
      setError(e.message ?? String(e))
    } finally {
      setBusy(false)
    }
  }

  async function deleteDay() {
    const label = `${bizDate} ${team === 'female' ? '女团' : '男团'}`
    if (!confirm(`确认删除 ${label} 的音浪数据？对应导入批次也会删除，操作不可恢复。`)) return
    setBusy(true)
    reset()
    try {
      const d = await api<DeleteDayReport>(
        `/api/admin/import/day?bizDate=${bizDate}&team=${team}`,
        { method: 'DELETE' },
      )
      setStatusNonce((n) => n + 1)
      loadBatches()
      setError(`已删除 ${d.metrics} 条音浪数据、${d.batches} 个导入批次。`)
    } catch (e: any) {
      setError(e.message ?? String(e))
    } finally {
      setBusy(false)
    }
  }

  function reset() {
    setError('')
    setReport(null)
  }

  /** 关预览弹窗：只丢解析结果，业务日期/团别/模式留着，方便换个文件接着导 */
  function closePreview() {
    setRows([])
    setFilename('')
  }

  /* 投放区：拖进来（dragenter/over 高亮，drop 解析）或点一下选文件。
     全页只有这一个入口 —— 没有粘贴框，也没有第二个上传按钮。 */
  function onDrop(e: React.DragEvent) {
    e.preventDefault()
    setDragOver(false)
    const f = e.dataTransfer.files?.[0]
    if (f) void uploadFile(f)
  }
  function onDragEnter(e: React.DragEvent) {
    e.preventDefault()
    if (e.dataTransfer.types.includes('Files')) setDragOver(true)
  }
  function onDragOver(e: React.DragEvent) {
    e.preventDefault()
    if (e.dataTransfer.types.includes('Files')) setDragOver(true)
  }
  function onDragLeave(e: React.DragEvent) {
    if (!e.currentTarget.contains(e.relatedTarget as Node)) setDragOver(false)
  }
  function pickFile() {
    if (!parsing) fileInputRef.current?.click()
  }

  const canCommit = rows.length > 0 && !!team && !!bizDate && !busy
  const canDelete = !!bizDate && !!team && !busy && dayStatus.checked && (dayStatus.rows ?? 0) > 0

  const errorBox = error ? <div className="alert bad">{error}</div> : null

  const dayHint = (() => {
    if (!dayStatus.checked || dayStatus.rows == null) return null
    const n = dayStatus.rows
    const last = dayStatus.lastBatch
    const lastTxt = last ? `（上次导入：${last.at} ${last.filename}）` : ''
    if (n === 0)
      return (
        <span className="alert good" style={{ flex: 1 }}>
          📭 {bizDate} {team === 'female' ? '女团' : '男团'}还没有数据，这次导入是<b>新增</b>
        </span>
      )
    return (
      <span className="alert warn" style={{ flex: 1 }}>
        ⚠️ {bizDate} {team === 'female' ? '女团' : '男团'}已有 <b>{n}</b> 行数据{lastTxt}
        {mode === 'merge' ? (
          <>
            ，这次导入会<b>覆盖</b>同名主播的行（以新数据为准）
          </>
        ) : (
          <>，当前模式「当日已有则拒绝」会<b>导入失败</b></>
        )}
      </span>
    )
  })()

  return (
    <div className="page">
      <section className="card">
        <h2>导入音浪数据</h2>

        {/* 全页唯一入口：拖到这里，或点这里选文件 */}
        <div
          className={'dropzone' + (dragOver ? ' is-over' : '') + (parsing ? ' is-busy' : '')}
          role="button"
          tabIndex={0}
          aria-label="拖入或点击选择音浪数据文件（CSV / TSV / 文本）"
          aria-busy={parsing}
          onClick={pickFile}
          onKeyDown={(e) => {
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault()
              pickFile()
            }
          }}
          onDragEnter={onDragEnter}
          onDragOver={onDragOver}
          onDragLeave={onDragLeave}
          onDrop={onDrop}
        >
          <svg
            className="dz-icon"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.6"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <path d="M12 16V4" />
            <path d="m7 9 5-5 5 5" />
            <path d="M4 15v3a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-3" />
          </svg>
          <b className="dz-title">
            {parsing
              ? '正在解析文件…'
              : dragOver
                ? '松手就开始解析'
                : '把 CSV / TSV 文件拖到这里'}
          </b>
          <span className="dz-sub">
            {parsing
              ? '解析完弹出预览，确认后才入库'
              : '或点击这里选择文件 · 一次一个文件 · .csv / .tsv / .txt'}
          </span>
          <input
            ref={fileInputRef}
            type="file"
            accept=".csv,.tsv,.txt"
            hidden
            disabled={parsing}
            onChange={(e) => {
              const f = e.target.files?.[0]
              if (f) void uploadFile(f)
              e.target.value = ''
            }}
          />
        </div>

        <p className="hint">
          列序不严格：名字必填，数字按「当日音浪 → 累计音浪 → 分钟」依次识别，开播 / 未开播自动识别。
          <b>默认归属前一天（数据晚一天）；文件名带日期和团别（如 2026-10-07-男团.csv）会自动填好。</b>
          解析结果先在弹窗里逐行核对，确认才入库。
        </p>

        {errorBox}
        <div className="row">
          <button className="danger" disabled={!canDelete} onClick={deleteDay}>
            删除当前日期 / 团别数据
          </button>
          <span className="hint">按上方业务日期和团别删除已入库的音浪数据；没有解析中的文件也可以操作。</span>
        </div>
        {report && (
          <div className="alert good">
            导入完成：成功 {report.okRows} 行，失败 {report.failedRows} 行（批次 #{report.batchId}）
            {!!report.unchanged && (
              <div>
                ♻️ 另有 {report.unchanged} 行音浪值与库里完全相同，未覆盖（改过的值才会覆盖）
              </div>
            )}
            {report.failures.map((f, i) => (
              <div key={i}>
                · {f.name}：{f.reason}
              </div>
            ))}
            {report.missing.length === 0 ? (
              <div>✅ 该团当天数据已齐</div>
            ) : (
              <div>
                ⚠️ 还有 {report.missing.length} 人当天没有数据：{report.missing.join('、')}
              </div>
            )}
          </div>
        )}
      </section>

      <section className="card">
        <h2>导入批次</h2>
        <p className="hint">数据一律以本地库为准（远程库只是开发期同步参考，已不再使用）。</p>
        {batches.length === 0 ? (
          <p className="hint">还没有导入记录</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>#</th>
                <th>业务日期</th>
                <th>团别</th>
                <th>解析</th>
                <th>成功</th>
                <th>失败</th>
                <th>备注</th>
                <th>时间</th>
              </tr>
            </thead>
            <tbody>
              {batches.map((b) => (
                <tr key={b.id}>
                  <td>{b.id}</td>
                  <td>{b.bizDate ?? '—'}</td>
                  <td>{b.team ?? '—'}</td>
                  <td>{b.importedRows}</td>
                  <td className="num">{b.okRows}</td>
                  <td className={b.failedRows > 0 ? 'bad-num' : 'num'}>{b.failedRows}</td>
                  <td className="hint">{b.note ?? '—'}</td>
                  <td className="hint">{b.createdAt}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>

      {previewOpen && (
        <div
          className="mask"
          onClick={(e) => {
            // 点遮罩空白处 = 取消预览（还没入库，没有副作用）
            if (e.target === e.currentTarget && !busy) closePreview()
          }}
        >
          <div className="import-pop" role="dialog" aria-modal="true" aria-label="导入预览">
            <div className="pop-head">
              <h2>导入预览</h2>
              <span className="hint">
                {filename} · 解析到 {rows.length} 行，核对无误再入库
              </span>
            </div>

            <div className="row">
              <label>
                团别{' '}
                <select value={team} onChange={(e) => setTeam(e.target.value)}>
                  <option value="male">男团</option>
                  <option value="female">女团</option>
                </select>
              </label>
              <label>
                业务日期{' '}
                <input
                  type="date"
                  value={bizDate}
                  onChange={(e) => {
                    setBizDate(e.target.value)
                    setDateFromFile(false)
                  }}
                />
                {dateFromFile && <span className="hint">（取自文件名）</span>}
              </label>
              <label>
                模式{' '}
                <select value={mode} onChange={(e) => setMode(e.target.value)}>
                  <option value="merge">覆盖（幂等）</option>
                  <option value="reject">当日已有则拒绝</option>
                </select>
              </label>
            </div>

            {dayHint && <div className="row">{dayHint}</div>}

            <div className="pop-table">
              <table>
                <thead>
                  <tr>
                    <th>#</th>
                    <th>名字</th>
                    <th>当日音浪</th>
                    <th>累计音浪</th>
                    <th>分钟</th>
                    <th>开播</th>
                    <th>抖音号</th>
                  </tr>
                </thead>
                <tbody>
                  {rows.map((r, i) => (
                    <tr key={i}>
                      <td className="hint">{i + 1}</td>
                      <td>{r.name}</td>
                      <td className="num">{r.wave ?? '—'}</td>
                      <td className="num">{r.cumulativeWave ?? '—'}</td>
                      <td className="num">{r.minutes ?? '—'}</td>
                      <td>{r.isLive == null ? '—' : r.isLive ? '开播' : '未开播'}</td>
                      <td>{r.douyinNo ?? '—'}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>

            {errorBox}

            <div className="pop-foot">
              <span className="hint">
                入库后不可撤销；同一天导错可先「删除当前日期 / 团别数据」再重导。
              </span>
              <button disabled={busy} onClick={closePreview}>
                取消
              </button>
              <button className="primary" disabled={!canCommit} onClick={commit}>
                {busy ? '导入中…' : `确认导入 ${rows.length} 行`}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
