import { useEffect, useState } from 'react'

/** 客户端分页（2026-10-16 表格规范 / Carbon「列表状态写进 URL」）。
 *
 *  列表接口一次返回全部行（主播台账 / 访客日志），页面按页切片，避免一次往 DOM 里塞
 *  上千行。页码与每页条数写进 URL query（`?page=2&size=50`）：刷新、浏览器回退、
 *  分享链接、右键「在新标签打开」都停在同一页；非法值回落第 1 页 / 50 条。
 *
 *  用法：
 *    const pager = usePager(items.length, `${q}|${team}`)   // 筛选条件变化 → 自动回第 1 页
 *    {pager.slice(items).map(...)}
 *    <Pager {...pager} />
 */
const SIZES = [20, 50, 100]
const DEFAULT_SIZE = 50

function readUrl(): { page: number; size: number } {
  if (typeof location === 'undefined') return { page: 1, size: DEFAULT_SIZE }
  const sp = new URLSearchParams(location.search)
  const rawSize = Number(sp.get('size'))
  const size = SIZES.includes(rawSize) ? rawSize : DEFAULT_SIZE
  const page = Math.max(1, Math.floor(Number(sp.get('page')) || 1))
  return { page, size }
}

export function usePager(total: number, resetKey = '') {
  const [state, setState] = useState(readUrl)
  const pages = Math.max(1, Math.ceil(total / state.size))
  // 行数变少（切筛选 / 删行）时页码越界 → 落到最后一页，不留空白页
  const page = Math.min(state.page, pages)

  useEffect(() => {
    const sp = new URLSearchParams(location.search)
    sp.set('page', String(page))
    sp.set('size', String(state.size))
    history.replaceState(null, '', location.pathname + '?' + sp.toString())
  }, [page, state.size])

  // 筛选条件变了 → 回到第 1 页（否则用户会停在一个语义完全不同的「第 3 页」）
  useEffect(() => {
    setState((s) => (s.page === 1 ? s : { ...s, page: 1 }))
  }, [resetKey])

  return {
    page,
    size: state.size,
    pages,
    total,
    slice: <T,>(rows: T[]): T[] => rows.slice((page - 1) * state.size, page * state.size),
    setPage: (p: number) => setState((s) => ({ ...s, page: Math.min(Math.max(1, p), pages) })),
    setSize: (n: number) => setState((s) => ({ ...s, size: n, page: 1 })),
  }
}

/** 分页条：左「共 N 条 · 第 x/y 页」，右「每页条数 + 上一页/下一页」。
 *  总页数只有 1 页时仍然显示（运营能看到总条数），但翻页按钮置灰。 */
export default function Pager(p: ReturnType<typeof usePager>) {
  const from = p.total === 0 ? 0 : (p.page - 1) * p.size + 1
  const to = Math.min(p.total, p.page * p.size)
  return (
    <div
      className="hint"
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: 10,
        marginTop: 8,
        flexWrap: 'wrap',
      }}
    >
      <span>
        共 {p.total} 条{from > 0 && ` · 当前 ${from}–${to}`}
      </span>
      <span style={{ marginLeft: 'auto', display: 'flex', alignItems: 'center', gap: 6 }}>
        每页
        <select
          value={p.size}
          onChange={(e) => p.setSize(Number(e.target.value))}
          style={{ minHeight: 28, padding: '2px 6px', fontSize: 12.5 }}
          aria-label="每页条数"
        >
          {SIZES.map((n) => (
            <option key={n} value={n}>
              {n}
            </option>
          ))}
        </select>
        条
      </span>
      <button onClick={() => p.setPage(p.page - 1)} disabled={p.page <= 1}>
        上一页
      </button>
      <span style={{ fontVariantNumeric: 'tabular-nums' }}>
        {p.page} / {p.pages}
      </span>
      <button onClick={() => p.setPage(p.page + 1)} disabled={p.page >= p.pages}>
        下一页
      </button>
    </div>
  )
}
