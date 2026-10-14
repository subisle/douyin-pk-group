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
  anchorId?: string | null
}

type CommitReport = {
  batchId: number
  total: number
  okRows: number
  /** 库里已有该人该日、值完全一样 → 没动它（相同不覆盖，不同才覆盖） */
  unchanged?: number
  failedRows: number
  failures: { name: string; reason: string }[]
  /** 未建档但有抖音号/ID：已暂存，不进榜单和统计，台账建档/绑号后自动补账 */
  pending?: { name: string; douyinNo: string }[]
  /** 已建档但当天一行数值都没有：跳过（不算失败，缺数据报告会跟踪） */
  skipped?: string[]
  missing: string[]
  /** 月度时长：本次写入的分钟合计 */
  minutesTotal?: number
  period?: string
  /** 按日导入时文件里的「时长」列被忽略的行数（时长只按月导入） */
  droppedMinutes?: number
}

/** 上传响应：按日走 rows；月度时长文件额外给一份只认时长列的 monthRows + 模式建议 */
type UploadResp = {
  filename: string
  total: number
  rows: Row[]
  monthRows?: Row[]
  suggest?: {
    kind: 'daily' | 'month-duration'
    year?: number | null
    month?: number | null
    fromFilename?: boolean
    reason?: string
  }
}

/** 月度时长预览（服务端匹配 + 与日明细对账） */
type MonthPreviewRow = {
  raw: Row
  personId?: number | null
  name: string
  matched: boolean
  teamConflict?: string | null
  matchedByDouyin?: boolean
}

type MonthPreview = {
  period: string
  total: number
  matched: number
  unmatched: number
  minutesTotal: number
  existingRows: number
  existingMinutes: number
  dailyMinutes: number
  diffs: { personId: number; name: string; imported: number; daily: number }[]
  rows: MonthPreviewRow[]
}

/** 该月已导入的整月时长（选年月时实时查） */
type MonthStatus = {
  period: string
  rows: number
  minutes: number
  lastBatch?: { filename: string; at: string } | null
}

type DeleteDayReport = { ok: boolean; metrics: number; batches: number }
type DeleteMonthReport = { ok: boolean; rows: number; batches: number }

/** 导入页解析草稿（P1-12）：切页前写 sessionStorage，回页恢复。 */
const IMPORT_DRAFT_KEY = '/admin:import-draft'
type ImportDraft = {
  kind: 'daily' | 'month'
  team: string
  bizDate: string
  mode: string
  year: number
  monthNum: number
  dateFromFile: boolean
  filename: string
  rows: Row[]
  monthRows: Row[]
  suggest: UploadResp['suggest'] | null
  report: CommitReport | null
}

/** 分钟 → 「12小时34分」（运营看的是小时，不是分钟） */
const hourText = (v: number) => {
  const h = Math.floor(v / 60)
  const m = Math.round(v % 60)
  return h > 0 ? `${h}小时${m ? `${m}分` : ''}` : `${m}分`
}

/** 团别 → 中文（P2-1）：批次列表 / 删除确认等处统一走这里，不再直接渲染英文值。
 *  male→男团、female→女团、mixed 或 auto→自动·不分团；未知值原样返回兜底。 */
function teamLabel(team: string | null | undefined): string {
  if (team === 'male') return '男团'
  if (team === 'female') return '女团'
  if (team === 'mixed' || team === 'auto' || !team) return '自动·不分团'
  return team
}

/** 该日该团已入库情况（选日期/团别时实时查） */
type DayStatus = {
  checked: boolean
  rows?: number
  lastBatch?: { filename: string; at: string } | null
}

// 数据晚一天：8 号发的 CSV 是 7 号的数据 → 默认业务日期 = 前一天
const yesterday = () => {
  const d = new Date(Date.now() - 24 * 60 * 60 * 1000)
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}

/** 从文件名读业务日期（运营习惯：2026-10-07-男团.csv）
 *  只读日期：导入不分男女团（团别按主播档案逐行归属），文件名里的团别不再当筛选条件。 */
function dateFromFilename(name: string): string | undefined {
  const m = name.match(/(\d{4})-(\d{2})-(\d{2})/)
  if (!m) return undefined
  const cand = `${m[1]}-${m[2]}-${m[3]}`
  return Number.isNaN(new Date(cand).getTime()) ? undefined : cand
}

/** 导入页：只认文件（拖拽 / 选文件），解析结果走弹窗预览，确认后才入库。
    粘贴入口已按需求移除——管理端一律在 PC 上用导出文件导入，手抄文本容易错。 */
export default function ImportPage() {
  /* ---------------- 解析草稿持久化（P1-12 最小可靠版） ----------------
   * 切页会卸载本组件，解析好的预览（文件/日期/团别/模式/弹窗状态）全丢，
   * 运营容易重复上传甚至重复入库。这里把草稿写进 sessionStorage
   * （key 带 /admin 前缀，避免和其它 app 的 session 冲突），回到本页自动恢复。
   * 恢复走 useState 的惰性初始化（一次性读出，各字段从草稿取初值），
   * 不用「挂载后再回填」的 effect —— StrictMode 双挂载下回填会被初值写覆盖。
   * 只存「能安全恢复」的字段：解析结果 + 表单选择 + 入库报告；错误/状态查询
   * 不存（回页后由各自的 effect 现查，永远新鲜）。 */
  const importDraft = (() => {
    try {
      const raw = sessionStorage.getItem(IMPORT_DRAFT_KEY)
      return raw ? (JSON.parse(raw) as Partial<ImportDraft>) : null
    } catch (e) {
      console.error('导入草稿解析失败', e)
      return null
    }
  })()

  const [filename, setFilename] = useState(() => importDraft?.filename ?? '')
  const [team, setTeam] = useState(() => importDraft?.team ?? 'auto')
  const [bizDate, setBizDate] = useState(() => importDraft?.bizDate ?? yesterday())
  const [mode, setMode] = useState(() => importDraft?.mode ?? 'merge')
  const [busy, setBusy] = useState(false)
  const [parsing, setParsing] = useState(false)
  /** 业务日期是从文件名推出来的（不是默认的前一天），弹窗里要标出来 */
  const [dateFromFile, setDateFromFile] = useState(() => importDraft?.dateFromFile ?? false)
  const [error, setError] = useState('')
  /** 成功提示（P2-2）：删除 / 操作成功走绿色 alert good，不再借道红框的 setError */
  const [notice, setNotice] = useState('')
  const [report, setReport] = useState<CommitReport | null>(() => importDraft?.report ?? null)
  const [batches, setBatches] = useState<any[]>([])
  /** 拖拽悬停高亮 */
  const [dragOver, setDragOver] = useState(false)
  /** 该日已有数据状态：rows == null 表示还没查/没选 */
  const [dayStatus, setDayStatus] = useState<DayStatus>({ checked: false })
  /** 该日状态查询失败（P2-5）：失败要显示出来 + 可重试，不能把删除按钮永久灰掉 */
  const [dayStatusError, setDayStatusError] = useState('')
  /** 入库后再查一次当日状态：否则「删除当前日期 / 团别数据」按钮还停在入库前的禁用态 */
  const [statusNonce, setStatusNonce] = useState(0)
  /** 导入类型（2026-10-10）：daily = 音浪按日；month = 月度时长（平台月报整月值，时长只按月导） */
  const [kind, setKind] = useState<'daily' | 'month'>(() => importDraft?.kind ?? 'daily')
  /** 月度时长：业务年月 + 只认时长列的解析结果 */
  const [year, setYear] = useState(() => importDraft?.year ?? new Date().getFullYear())
  const [monthNum, setMonthNum] = useState(() => importDraft?.monthNum ?? new Date().getMonth() + 1)
  const [monthRows, setMonthRows] = useState<Row[]>(() => importDraft?.monthRows ?? [])
  /** 上传时的模式建议（文件名带月份 + 文件里只有时长列 → 建议按月） */
  const [suggest, setSuggest] = useState<UploadResp['suggest'] | null>(() => importDraft?.suggest ?? null)
  /** 月度时长的服务端预览（匹配情况 + 与日明细对账） */
  const [monthPreview, setMonthPreview] = useState<MonthPreview | null>(null)
  /** 该月已导入的整月时长（决定「新增 / 覆盖」提示与删除按钮可用态） */
  const [monthStatus, setMonthStatus] = useState<MonthStatus | null>(null)
  /** 月度状态查询失败（P2-5）：同 day-status，显示出来 + 可重试 */
  const [monthStatusError, setMonthStatusError] = useState('')

  const fileInputRef = useRef<HTMLInputElement>(null)

  const [rows, setRows] = useState<Row[]>(() => importDraft?.rows ?? [])

  /** 草稿保存：任一草稿字段变化就写回 sessionStorage（纯副作用，不阻塞渲染） */
  useEffect(() => {
    const draft: ImportDraft = {
      kind,
      team,
      bizDate,
      mode,
      year,
      monthNum,
      dateFromFile,
      filename,
      rows,
      monthRows,
      suggest,
      report,
    }
    try {
      sessionStorage.setItem(IMPORT_DRAFT_KEY, JSON.stringify(draft))
    } catch {
      /* 隐私模式 / 容量超限：草稿存不下就算了，页面照常工作 */
    }
  }, [kind, team, bizDate, mode, year, monthNum, dateFromFile, filename, rows, monthRows, suggest, report])

  /** 月度时长的业务月份 YYYY-MM */
  const period = `${year}-${String(monthNum).padStart(2, '0')}`
  /** 年份下拉：前后各留几年，运营补录去年 / 提前导下月都够用（按当前年生成，选完不会跳） */
  const yearOptions = Array.from({ length: 6 }, (_, i) => new Date().getFullYear() - 3 + i)

  /** 预览弹窗：解析出内容就打开；取消/入库后 rows 清空即关闭 */
  const previewOpen = rows.length > 0 || monthRows.length > 0

  const loadBatches = useCallback(() => {
    api('/api/admin/import/batches')
      .then((d) => setBatches(d.items ?? []))
      .catch((e) => {
        console.error('批次列表加载失败', e)
        setError(String(e.message ?? e))
      })
  }, [])

  useEffect(loadBatches, [loadBatches])

  // 机器人私聊发 CSV 导入后，这里的批次列表要立刻出现新记录
  useAutoRefresh(loadBatches, ['import'])

  /** 选了日期/团别就实时查该日已入库行数 → 提示「覆盖 / 新增」口径。
      文件也是可能几秒内连续换的，300ms 抖一下省请求。 */
  useEffect(() => {
    if (!bizDate || !team) {
      setDayStatus({ checked: false })
      setDayStatusError('')
      return
    }
    setDayStatus((s) => ({ ...s, checked: false }))
    setDayStatusError('')
    const t = setTimeout(() => {
      api<DayStatus>(
        `/api/admin/import/day-status?bizDate=${bizDate}&team=${team}`,
      )
        .then((d) => setDayStatus(d))
        .catch((e) => {
          // P2-5：失败不再静默 —— 记录到控制台 + 页面显示「状态获取失败」+ 重试，
          // 删除按钮也不会被永久灰死（失败态下允许按，由后端兜底口径）
          console.error('day-status 查询失败', e)
          setDayStatusError(String(e?.message ?? e))
        })
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

  /** 月度时长：选了年月就查「该月已导入情况」+ 服务端匹配预览（弹窗里看匹配 / 未建档 / 差异）。
      文件几秒内连着换也可能，300ms 抖一下省请求。 */
  useEffect(() => {
    if (kind !== 'month') {
      setMonthPreview(null)
      return
    }
    if (!monthRows.length) {
      setMonthPreview(null)
      return
    }
    const t = setTimeout(() => {
      api<MonthPreview>('/api/admin/import/month/preview', {
        method: 'POST',
        body: JSON.stringify({ rows: monthRows, period, team, filename }),
      })
        .then(setMonthPreview)
        .catch((e) => {
          console.error('月度时长预览失败', e)
          setMonthPreview(null)
          setError(String(e.message ?? e))
        })
    }, 300)
    return () => clearTimeout(t)
  }, [kind, monthRows, period, team, filename, statusNonce])

  /** 该月已导入的整月时长（决定「新增 / 覆盖」提示与删除按钮可用态） */
  useEffect(() => {
    if (kind !== 'month') {
      setMonthStatus(null)
      setMonthStatusError('')
      return
    }
    setMonthStatusError('')
    const t = setTimeout(() => {
      api<MonthStatus>(`/api/admin/import/month-status?period=${period}&team=${team}`)
        .then(setMonthStatus)
        .catch((e) => {
          console.error('month-status 查询失败', e)
          setMonthStatus(null)
          setMonthStatusError(String(e?.message ?? e))
        })
    }, 300)
    return () => clearTimeout(t)
  }, [kind, period, team, statusNonce])

  async function uploadFile(file: File) {
    reset()
    // 文件名带业务日期就直接带上（2026-10-07-男团.csv 这类），不用再手选；
    // 团别不看文件名 —— 一个文件里男女混着也能导
    const fileDate = dateFromFilename(file.name)
    setDateFromFile(!!fileDate)
    if (fileDate) setBizDate(fileDate)
    const fd = new FormData()
    fd.append('file', file)
    fd.append('filename', file.name)
    setParsing(true)
    try {
      const d = await api<UploadResp>('/api/admin/import/upload', {
        method: 'POST',
        body: fd,
      })
      const monthList = d.monthRows ?? []
      if (!d.rows?.length && !monthList.length) {
        setError('没解析出任何主播行：确认文件是 CSV / TSV / 文本，且每行至少一个名字。')
        return
      }
      setRows(d.rows ?? [])
      setMonthRows(monthList)
      setFilename(d.filename)
      setSuggest(d.suggest ?? null)
      // 月度时长文件（只有时长列、没有音浪列）直接预选「时长（按月）」，
      // 年份 / 月份按文件名猜（「9月份时长.csv」→ 当年 9 月），弹窗里还能改
      if (d.suggest?.kind === 'month-duration') {
        setKind('month')
        if (d.suggest.year) setYear(d.suggest.year)
        if (d.suggest.month) setMonthNum(d.suggest.month)
      } else {
        setKind('daily')
      }
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
      const d =
        kind === 'month'
          ? await api<CommitReport>('/api/admin/import/month/commit', {
              method: 'POST',
              body: JSON.stringify({ rows: monthRows, period, team, mode, filename }),
            })
          : await api<CommitReport>('/api/admin/import/commit', {
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
    // 月度时长：删的是「某月已导入的整月值」，与按日删某天分开
    if (kind === 'month') {
      const label = `${period} ${teamLabel(team)}`
      if (!confirm(`确认删除 ${label} 已导入的月度时长？对应导入批次也会删除，操作不可恢复。`))
        return
      setBusy(true)
      reset()
      try {
        const d = await api<DeleteMonthReport>(
          `/api/admin/import/month?period=${period}&team=${team}`,
          { method: 'DELETE' },
        )
        setStatusNonce((n) => n + 1)
        loadBatches()
        setNotice(`已删除 ${d.rows} 条月度时长、${d.batches} 个导入批次。`)
      } catch (e: any) {
        setError(e.message ?? String(e))
      } finally {
        setBusy(false)
      }
      return
    }
    const label = `${bizDate} ${teamLabel(team)}`
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
      setNotice(`已删除 ${d.metrics} 条音浪数据、${d.batches} 个导入批次。`)
    } catch (e: any) {
      setError(e.message ?? String(e))
    } finally {
      setBusy(false)
    }
  }

  function reset() {
    setError('')
    setNotice('')
    setReport(null)
  }

  /** 关预览弹窗：只丢解析结果，业务日期/团别/模式留着，方便换个文件接着导。
   *  草稿里也一并清掉解析结果，下次切页回来不会把已取消的弹窗再弹出来。 */
  function closePreview() {
    setRows([])
    setMonthRows([])
    setFilename('')
    setSuggest(null)
    setDateFromFile(false)
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

  const monthView: MonthPreviewRow[] =
    monthPreview?.rows ?? monthRows.map((r) => ({ raw: r, name: r.name, matched: false }))

  const canCommit =
    kind === 'month'
      ? monthRows.length > 0 && !!team && !busy
      : rows.length > 0 && !!team && !!bizDate && !busy
  /** 删除按钮可用态（P2-5）：状态查询失败时不再永久灰 —— 失败也能按（后端会按真实
   *  数据口径返回删除报告），页面上同时给出「状态获取失败 + 重试」提示。 */
  const canDelete =
    kind === 'month'
      ? !busy && (!!monthStatusError || (monthStatus?.rows ?? 0) > 0)
      : !!bizDate && !!team && !busy && (!!dayStatusError || (dayStatus.checked && (dayStatus.rows ?? 0) > 0))

  const errorBox = error ? <div className="alert bad">{error}</div> : null
  /** 成功提示走绿色 good 样式（P2-2），删除 / 操作成功不再显示成红框 */
  const noticeBox = notice ? <div className="alert good">{notice}</div> : null

  /** 该日/该月状态获取失败提示（P2-5）：黄框 + 重试按钮，不再静默成空态 */
  const statusErrorHint = (() => {
    const err = kind === 'month' ? monthStatusError : dayStatusError
    if (!err) return null
    return (
      <div className="row">
        <span className="alert warn" style={{ flex: 1 }}>
          ⚠️ 已导入状态获取失败：{err}
        </span>
        <button onClick={() => setStatusNonce((n) => n + 1)}>重试</button>
      </div>
    )
  })()

  const teamWord = team === 'auto' ? '' : team === 'female' ? '女团' : '男团'
  const dayHint = (() => {
    if (!dayStatus.checked || dayStatus.rows == null) return null
    const n = dayStatus.rows
    const last = dayStatus.lastBatch
    const lastTxt = last ? `（上次导入：${last.at} ${last.filename}）` : ''
    if (n === 0)
      return (
        <span className="alert good" style={{ flex: 1 }}>
          📭 {bizDate} {teamWord}还没有数据，这次导入是<b>新增</b>
        </span>
      )
    return (
      <span className="alert warn" style={{ flex: 1 }}>
        ⚠️ {bizDate} {teamWord}已有 <b>{n}</b> 行数据{lastTxt}
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
        <h2>导入数据</h2>

        {/* 全页唯一入口：拖到这里，或点这里选文件 */}
        <div
          className={'dropzone' + (dragOver ? ' is-over' : '') + (parsing ? ' is-busy' : '')}
          role="button"
          tabIndex={0}
          aria-label="拖入或点击选择数据文件（CSV / TSV / 文本）"
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
          <b>音浪按日导</b>：列序不严格，名字必填，数字按「当日音浪 → 累计音浪」依次识别，开播 / 未开播自动识别；
          <b>默认归属前一天（数据晚一天）；文件名带日期（如 2026-10-07-男团.csv）会自动填好。</b>
          <br />
          <b>时长按月导</b>：拖入月度时长文件（如 <code>9月份时长.csv</code>）会自动切到「时长（按月）」，
          选好年份 + 月份后入库；整月时长是月榜 / 个人月卡在用的时长，值相同的记录不会被覆盖。
          解析结果先在弹窗里逐行核对，确认才入库。
        </p>

        {errorBox}
        {noticeBox}
        {/* 状态获取失败提示挂在主卡片（删除按钮旁边）：P2-5 —— 失败可见 + 重试，不再静默 */}
        {statusErrorHint && <div style={{ marginTop: 8 }}>{statusErrorHint}</div>}
        <div className="row">
          <button className="danger" disabled={!canDelete} onClick={deleteDay}>
            {kind === 'month'
              ? `删除 ${period} 月度时长${team === 'auto' ? '（不限团别）' : ''}`
              : team === 'auto'
                ? '删除当前日期数据（不限团别）'
                : '删除当前日期 / 团别数据'}
          </button>
          <span className="hint">
            {kind === 'month'
              ? `删除 ${period} 已导入的整月时长（月榜 / 个人月卡立刻回落成日明细口径）；没有解析中的文件也可以操作。`
              : team === 'auto'
                ? '删除该业务日期的全部音浪数据（男女团一起）；没有解析中的文件也可以操作。'
                : '按上方业务日期和团别删除已入库的音浪数据；没有解析中的文件也可以操作。'}
          </span>
        </div>
        {report && (
          <div className="alert good">
            导入完成：成功 {report.okRows} 行，失败 {report.failedRows} 行（批次 #{report.batchId}）
            {!!report.period && (
              <div>
                ⏱️ 月度时长口径 {report.period}，本次写入时长合计{' '}
                <b>{hourText(report.minutesTotal ?? 0)}</b>（月榜 / 个人月卡以它为准）
              </div>
            )}
            {!!report.droppedMinutes && (
              <div>
                ⏱️ 文件里有 {report.droppedMinutes} 行的「时长」列已忽略：<b>每日不导时长，只有月时长</b>
                —— 要导时长请用「时长（按月）」。
              </div>
            )}
            {!!report.unchanged && (
              <div>
                ♻️ 另有 {report.unchanged} 行{report.period ? '月度时长' : '音浪值'}与库里完全相同，未覆盖（改过的值才会覆盖）
              </div>
            )}
            {/* 导入结果里的「另有 N 行已暂存」一行已移除（2026-10-14 用户口径：
                暂存内容不需要显示）。后端仍照旧落 import_pending、建档/绑号后自动补账，
                只是本次结果不再逐条列出暂存的人名。 */}
            {!!report.skipped?.length && (
              <div>
                ⏭️ 跳过 {report.skipped.length} 行
                {report.period ? '（文件里没有时长）' : '（主播已建档但当天没有数值）'}：
                {report.skipped.slice(0, 8).join('、')}
                {report.skipped.length > 8 ? ` 等 ${report.skipped.length} 行` : ''}
              </div>
            )}
            {report.failures.map((f, i) => (
              <div key={i}>
                · {f.name}：{f.reason}
              </div>
            ))}
            {/* 「当日缺数据」只对按日导入有意义；月度时长没有「某一天」的概念 */}
            {!report.period &&
              (report.missing.length === 0 ? (
                <div>✅ 该团当天数据已齐</div>
              ) : (
                <div>
                  ⚠️ 还有 {report.missing.length} 人当天没有数据：{report.missing.join('、')}
                </div>
              ))}
          </div>
        )}
      </section>

      <section className="card">
        <h2>导入批次</h2>
        <p className="hint">数据一律以本地库为准（远程库只是开发期同步参考，已不再使用）。</p>
        {batches.length === 0 ? (
          <p className="hint">还没有导入记录</p>
        ) : (
          <div className="table-scroll">
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
                  <td>
                    {b.source === 'admin-import-month'
                      ? `${String(b.bizDate ?? '').slice(0, 7)} · 月度时长`
                      : (b.bizDate ?? '—')}
                  </td>
                  <td>{b.team ? teamLabel(b.team) : '—'}</td>
                  <td>{b.importedRows}</td>
                  <td className="num">{b.okRows}</td>
                  <td className={b.failedRows > 0 ? 'bad-num' : 'num'}>{b.failedRows}</td>
                  <td className="hint">{b.note ?? '—'}</td>
                  <td className="hint">{b.createdAt}</td>
                </tr>
              ))}
              </tbody>
            </table>
          </div>
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
                {filename} ·{' '}
                {kind === 'month'
                  ? `解析到 ${monthRows.length} 人整月时长`
                  : `解析到 ${rows.length} 行`}
                ，核对无误再入库
              </span>
            </div>

            {/* 导入类型：音浪按日 / 时长按月（用户口径 2026-10-10：每日不能导入时长，只有月时长） */}
            <div className="row">
              <span className="hint">导入类型</span>
              <button
                className={kind === 'daily' ? 'primary' : ''}
                disabled={busy}
                onClick={() => setKind('daily')}
              >
                音浪（按日）
              </button>
              <button
                className={kind === 'month' ? 'primary' : ''}
                disabled={busy}
                onClick={() => setKind('month')}
              >
                时长（按月）
              </button>
              {suggest?.kind === 'month-duration' && kind === 'daily' && (
                <span className="hint">
                  ⚠️ 这个文件只有「时长」列（没有音浪列），建议切到「时长（按月）」
                </span>
              )}
              {kind === 'month' && !monthRows.length && (
                <span className="hint">⚠️ 这个文件里没解析到时长列，换个文件或切回「音浪（按日）」</span>
              )}
            </div>

            {kind === 'month' && suggest?.reason && (
              <p className="hint">
                {suggest.reason}
                {suggest.fromFilename ? '（年份 / 月份取自文件名，可改）' : ''}
              </p>
            )}

            <div className="row">
              <label>
                团别{' '}
                <select value={team} onChange={(e) => setTeam(e.target.value)}>
                  <option value="auto">自动（不分男女团）</option>
                  <option value="male">只按男团校验</option>
                  <option value="female">只按女团校验</option>
                </select>
              </label>
              {kind === 'month' ? (
                <>
                  <label>
                    年份{' '}
                    <select value={year} onChange={(e) => setYear(Number(e.target.value))}>
                      {yearOptions.map((y) => (
                        <option key={y} value={y}>
                          {y} 年
                        </option>
                      ))}
                    </select>
                  </label>
                  <label>
                    月份{' '}
                    <select value={monthNum} onChange={(e) => setMonthNum(Number(e.target.value))}>
                      {Array.from({ length: 12 }, (_, i) => i + 1).map((m) => (
                        <option key={m} value={m}>
                          {m} 月
                        </option>
                      ))}
                    </select>
                  </label>
                </>
              ) : (
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
              )}
              <label>
                模式{' '}
                <select value={mode} onChange={(e) => setMode(e.target.value)}>
                  <option value="merge">覆盖（幂等）</option>
                  <option value="reject">{kind === 'month' ? '该月已有则拒绝' : '当日已有则拒绝'}</option>
                </select>
              </label>
            </div>

            {kind === 'daily' && dayHint && <div className="row">{dayHint}</div>}
            {statusErrorHint && <div className="row" style={{ marginTop: 8 }}>{statusErrorHint}</div>}

            {kind === 'month' && monthPreview && (
              <div className="row">
                <span className="alert good" style={{ flex: 1 }}>
                  📅 {monthPreview.period}：匹配 <b>{monthPreview.matched}</b> / {monthPreview.total} 人
                  {monthPreview.unmatched > 0 && (
                    <>
                      ，未建档 <b>{monthPreview.unmatched}</b> 人（提交时会报失败：月度时长不进暂存区，
                      先在「主播台账」建档再重导）
                    </>
                  )}
                  ；整月时长合计 <b>{hourText(monthPreview.minutesTotal)}</b>
                </span>
              </div>
            )}

            {kind === 'month' && monthStatus && (
              <div className="row">
                {monthStatus.rows > 0 ? (
                  <span className="alert warn" style={{ flex: 1 }}>
                    ⚠️ {monthStatus.period} 已有 <b>{monthStatus.rows}</b> 人的整月时长（合计{' '}
                    {hourText(monthStatus.minutes)}）
                    {monthStatus.lastBatch
                      ? `（上次导入：${monthStatus.lastBatch.at} ${monthStatus.lastBatch.filename}）`
                      : ''}
                    ：这次导入会覆盖值不同的记录，值相同的保持不动
                  </span>
                ) : (
                  <span className="alert good" style={{ flex: 1 }}>
                    📭 {monthStatus.period} 还没有月度时长，这次导入是<b>新增</b>
                  </span>
                )}
              </div>
            )}

            {kind === 'month' && !!monthPreview?.diffs.length && (
              <div className="row">
                <span className="alert warn" style={{ flex: 1 }}>
                  ⚖️ {monthPreview.diffs.length} 人该月既有日明细时长又与月报不一致（
                  {monthPreview.diffs
                    .slice(0, 5)
                    .map((x) => `${x.name} 月报 ${hourText(x.imported)} / 日明细 ${hourText(x.daily)}`)
                    .join('、')}
                  {monthPreview.diffs.length > 5 ? ' 等' : ''}
                  ）：月榜 / 个人月卡按<b>月报</b>算，日明细只用于日榜
                </span>
              </div>
            )}

            <div className="pop-table">
              {kind === 'month' ? (
                <table>
                  <thead>
                    <tr>
                      <th>#</th>
                      <th>名字</th>
                      <th>整月时长</th>
                      <th>分钟</th>
                      <th>匹配</th>
                      <th>抖音号</th>
                    </tr>
                  </thead>
                  <tbody>
                    {monthView.map((r, i) => (
                      <tr key={i}>
                        <td className="hint">{i + 1}</td>
                        <td>{r.name}</td>
                        <td className="num">
                          {r.raw.minutes == null ? '—' : hourText(r.raw.minutes)}
                        </td>
                        <td className="num">{r.raw.minutes ?? '—'}</td>
                        <td>
                          {r.teamConflict
                            ? '⚠️ 团别冲突'
                            : r.matched
                              ? '✅ 已建档'
                              : monthPreview
                                ? '❓ 未建档'
                                : '—'}
                        </td>
                        <td>{r.raw.douyinNo ?? r.raw.anchorId ?? '—'}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              ) : (
                <table>
                  <thead>
                    <tr>
                      <th>#</th>
                      <th>名字</th>
                      <th>当日音浪</th>
                      <th>累计音浪</th>
                      <th>抖音号</th>
                      <th>开播</th>
                    </tr>
                  </thead>
                  <tbody>
                    {rows.map((r, i) => (
                      <tr key={i}>
                        <td className="hint">{i + 1}</td>
                        <td>{r.name}</td>
                        <td className="num">{r.wave ?? '—'}</td>
                        <td className="num">{r.cumulativeWave ?? '—'}</td>
                        <td>{r.douyinNo ?? r.anchorId ?? '—'}</td>
                        <td>{r.isLive == null ? '—' : r.isLive ? '开播' : '未开播'}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </div>

            {errorBox}

            <div className="pop-foot">
              <span className="hint">
                {kind === 'month'
                  ? '入库后不可撤销；导错可先「删除该月月度时长」再重导。'
                  : '入库后不可撤销；同一天导错可先「删除当前日期 / 团别数据」再重导。'}
              </span>
              <button disabled={busy} onClick={closePreview}>
                取消
              </button>
              <button className="primary" disabled={!canCommit} onClick={commit}>
                {busy
                  ? '导入中…'
                  : kind === 'month'
                    ? `确认导入 ${period} 整月时长（${monthRows.length} 人）`
                    : `确认导入 ${rows.length} 行`}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
