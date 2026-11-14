import { useEffect, useMemo, useState } from 'react'
import { createPortal } from 'react-dom'

type Props = {
  value: string
  onChange: (value: string) => void
  highlightedDates?: string[]
  disabled?: boolean
}

const WEEKDAYS = ['一', '二', '三', '四', '五', '六', '日']

const pad = (n: number) => String(n).padStart(2, '0')
const ymd = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`

function parseYmd(value: string): Date | null {
  const m = value.match(/^(\d{4})-(\d{2})-(\d{2})$/)
  if (!m) return null
  const date = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]))
  return Number.isNaN(date.getTime()) ? null : date
}

/** 周一起始的 6×7 日期网格。固定 42 格可避免切月时弹层高度跳动。 */
function monthCells(view: Date): Date[] {
  const first = new Date(view.getFullYear(), view.getMonth(), 1)
  const mondayOffset = (first.getDay() + 6) % 7
  const start = new Date(first)
  start.setDate(first.getDate() - mondayOffset)
  return Array.from({ length: 42 }, (_, i) => {
    const d = new Date(start)
    d.setDate(start.getDate() + i)
    return d
  })
}

export default function DateCalendar({ value, onChange, highlightedDates = [], disabled = false }: Props) {
  const selected = parseYmd(value) ?? new Date()
  const today = ymd(new Date())
  const [open, setOpen] = useState(false)
  const [view, setView] = useState(() => new Date(selected.getFullYear(), selected.getMonth(), 1))
  const highlighted = useMemo(() => new Set(highlightedDates), [highlightedDates])
  const cells = useMemo(() => monthCells(view), [view])

  useEffect(() => {
    const d = parseYmd(value)
    if (d) setView(new Date(d.getFullYear(), d.getMonth(), 1))
  }, [value])

  useEffect(() => {
    if (!open) return
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false)
    }
    document.addEventListener('keydown', onKeyDown)
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [open])

  const shiftMonth = (delta: number) => {
    setView((v) => new Date(v.getFullYear(), v.getMonth() + delta, 1))
  }

  const pick = (d: Date) => {
    onChange(ymd(d))
    setOpen(false)
  }

  return (
    <div className="date-picker">
      <button
        type="button"
        className="date-picker-trigger"
        aria-haspopup="dialog"
        aria-expanded={open}
        disabled={disabled}
        onClick={() => setOpen((v) => !v)}
      >
        <span>{value || '选择日期'}</span>
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <rect x="3.5" y="5.5" width="17" height="15" rx="2.5" />
          <path d="M7.5 3.5v4M16.5 3.5v4M3.5 9.5h17" />
        </svg>
      </button>

      {open &&
        createPortal(
          <div
            className="date-picker-mask"
            onPointerDown={(e) => {
              if (e.target === e.currentTarget) setOpen(false)
            }}
          >
            <div className="date-picker-pop" role="dialog" aria-label="选择删除日期">
              <div className="date-picker-head">
                <button type="button" onClick={() => shiftMonth(-1)} aria-label="上个月">
                  ‹
                </button>
                <b>
                  {view.getFullYear()} 年 {view.getMonth() + 1} 月
                </b>
                <button type="button" onClick={() => shiftMonth(1)} aria-label="下个月">
                  ›
                </button>
              </div>
              <div className="date-picker-week" aria-hidden="true">
                {WEEKDAYS.map((w) => (
                  <span key={w}>{w}</span>
                ))}
              </div>
              <div className="date-picker-grid">
                {cells.map((d) => {
                  const day = ymd(d)
                  const currentMonth = d.getMonth() === view.getMonth()
                  return (
                    <button
                      type="button"
                      key={day}
                      className={[
                        'date-picker-day',
                        currentMonth ? '' : 'is-outside',
                        day === value ? 'is-selected' : '',
                        day === today ? 'is-today' : '',
                        highlighted.has(day) ? 'has-data' : '',
                      ]
                        .filter(Boolean)
                        .join(' ')}
                      aria-label={`${day}${highlighted.has(day) ? '，有导入数据' : ''}`}
                      aria-pressed={day === value}
                      onClick={() => pick(d)}
                    >
                      {d.getDate()}
                    </button>
                  )
                })}
              </div>
              <div className="date-picker-foot">
                <span>
                  <i className="has-data" /> 有导入数据
                </span>
                <button type="button" onClick={() => pick(new Date())}>
                  今天
                </button>
              </div>
            </div>
          </div>,
          document.body,
        )}
    </div>
  )
}
