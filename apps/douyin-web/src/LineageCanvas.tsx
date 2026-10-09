import { useCallback, useEffect, useMemo, useRef, useState } from 'react'

/** 树图画布需要的最小字段（来自 /api/admin/streamers 一行） */
export interface CanvasRow {
  personId: number
  name: string
  team: string
  masterId: number | null
  disciples: number
  status?: string
}

export interface CNode {
  id: number
  name: string
  team: string
  status?: string
  disciples: number
  masterId: number | null
  kids: CNode[]
  depth: number
  x: number
  y: number
}

export interface Edge {
  from: CNode
  to: CNode
}

const norm = (s: string) => (s || '').replace(/\s+/g, '')

/* 布局常量（横向，从左到右）：祖师在最左，一代向右一列；同一层的兄弟纵向排开，
   父节点纵向居中于最上/最下子节点之间。叶节点各占一行。 */
export const W = 104
export const COL = 150
export const ROW = 62
export const PAD = 52

export interface Layout {
  byId: Map<number, CNode>
  nodes: CNode[]
  edges: Edge[]
  roots: CNode[]
  width: number
  height: number
}

export function layoutForest(rows: CanvasRow[]): Layout {
  const byId = new Map<number, CNode>()
  for (const r of rows) {
    byId.set(r.personId, {
      id: r.personId,
      name: r.name,
      team: r.team,
      status: r.status,
      disciples: r.disciples,
      masterId: r.masterId,
      kids: [],
      depth: 0,
      x: 0,
      y: 0,
    })
  }
  const roots: CNode[] = []
  for (const n of byId.values()) {
    if (n.masterId != null && n.masterId !== n.id && byId.has(n.masterId)) {
      byId.get(n.masterId)!.kids.push(n)
    } else {
      roots.push(n)
    }
  }
  // 子树大的排前面（后端保证无环，这里仍加 visited 兜底防脏数据死循环）
  const seen = new Set<number>()
  const sizeOf = (n: CNode): number => {
    if (seen.has(n.id)) return 0
    seen.add(n.id)
    let s = 1
    for (const k of n.kids) s += sizeOf(k)
    return s
  }
  const size = (n: CNode): number => {
    seen.clear()
    return sizeOf(n)
  }
  const cmp = (a: CNode, b: CNode) =>
    size(b) - size(a) || a.name.localeCompare(b.name, 'zh-Hans-CN')
  roots.sort(cmp)
  for (const n of byId.values()) n.kids.sort(cmp)

  let cursor = PAD
  let maxDepth = 0
  const place = (n: CNode, depth: number) => {
    n.depth = depth
    maxDepth = Math.max(maxDepth, depth)
    if (!n.kids.length) {
      n.y = cursor + ROW / 2
      cursor += ROW
    } else {
      for (const k of n.kids) place(k, depth + 1)
      n.y = (n.kids[0].y + n.kids[n.kids.length - 1].y) / 2
    }
    n.x = PAD + depth * COL
  }
  for (const r of roots) place(r, 0)

  const nodes: CNode[] = []
  const edges: Edge[] = []
  const walk = (n: CNode) => {
    nodes.push(n)
    for (const k of n.kids) {
      edges.push({ from: n, to: k })
      walk(k)
    }
  }
  for (const r of roots) walk(r)
  return {
    byId,
    nodes,
    edges,
    roots,
    width: PAD * 2 + maxDepth * COL + W,
    height: Math.max(cursor + PAD, 320),
  }
}

export interface FocusReq {
  id: number
  /** 自增序号：同一个 id 也能重复触发居中 */
  n: number
}

interface Props {
  rows: CanvasRow[]
  selId: number | null
  onSelect: (id: number) => void
  /** 搜索词：命中节点加橙圈（回车定位由父层通过 focusReq 触发） */
  query: string
  focusReq: FocusReq | null
}

export default function LineageCanvas({ rows, selId, onSelect, query, focusReq }: Props) {
  const wrapRef = useRef<HTMLDivElement>(null)
  const gRef = useRef<SVGGElement>(null)
  const view = useRef({ x: 0, y: 0, s: 1 })
  const [ready, setReady] = useState(0)
  /** 全屏：优先原生 Fullscreen API，不可用时退到 .fs 铺满 */
  const [fsNative, setFsNative] = useState(false)
  const [fsCss, setFsCss] = useState(false)
  const isFs = fsNative || fsCss

  const lay = useMemo(() => layoutForest(rows), [rows])

  const apply = useCallback(() => {
    const g = gRef.current
    if (g) {
      const v = view.current
      g.setAttribute('transform', `translate(${v.x},${v.y}) scale(${v.s})`)
    }
  }, [])

  /** 复位：横向树，祖师贴左边、纵向居中到最大师门 */
  const fit = useCallback(() => {
    const wrap = wrapRef.current
    if (!wrap || !lay.roots.length) return
    const w = wrap.clientWidth
    const h = wrap.clientHeight
    const root = lay.roots[0]
    const s = Math.max(0.55, Math.min(0.95, Math.min((w - 48) / 480, 1)))
    view.current = { s, x: 16, y: h / 2 - root.y * s }
    apply()
  }, [lay, apply])

  const focusNode = useCallback(
    (id: number) => {
      const wrap = wrapRef.current
      const n = lay.byId.get(id)
      if (!wrap || !n) return
      const w = wrap.clientWidth
      const h = wrap.clientHeight
      const s = Math.max(view.current.s, 0.85)
      view.current = { s, x: w / 2 - n.x * s, y: h / 2 - n.y * s }
      apply()
    },
    [lay, apply],
  )

  // 数据变了（团别筛选 / 自动刷新）→ 重新布局并复位
  useEffect(() => {
    fit()
    setReady((n) => n + 1)
  }, [fit])

  // 外部请求居中（详情面板点链 / 搜索回车）
  useEffect(() => {
    if (focusReq) focusNode(focusReq.id)
  }, [focusReq, focusNode, ready])

  // 滚轮缩放：React 的 onWheel 是 passive 的，必须原生监听才能 preventDefault
  useEffect(() => {
    const wrap = wrapRef.current
    if (!wrap) return
    const onWheel = (e: WheelEvent) => {
      e.preventDefault()
      const r = wrap.getBoundingClientRect()
      zoomAt(e.clientX - r.left, e.clientY - r.top, e.deltaY < 0 ? 1.12 : 0.89)
    }
    wrap.addEventListener('wheel', onWheel, { passive: false })
    return () => wrap.removeEventListener('wheel', onWheel)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const zoomAt = useCallback(
    (px: number, py: number, k: number) => {
      const v = view.current
      const ns = Math.min(2.5, Math.max(0.25, v.s * k))
      const real = ns / v.s
      v.x = px - (px - v.x) * real
      v.y = py - (py - v.y) * real
      v.s = ns
      apply()
    },
    [apply],
  )

  // 拖动平移 + 点选（移动距离小于 6px 算点击）
  const pan = useRef({ id: -1, lx: 0, ly: 0, moved: 0 })
  const onCtl = useRef(false)
  const onPointerDown = (e: React.PointerEvent) => {
    /* 缩放/全屏按钮交给原生 click：抢指针会让按钮的 click 不触发 */
    if ((e.target as HTMLElement).closest?.('.lcn-ctl')) {
      onCtl.current = true
      return
    }
    if (pan.current.id !== -1) return
    pan.current = { id: e.pointerId, lx: e.clientX, ly: e.clientY, moved: 0 }
    wrapRef.current?.setPointerCapture(e.pointerId)
  }
  const onPointerMove = (e: React.PointerEvent) => {
    const p = pan.current
    if (p.id !== e.pointerId) return
    const dx = e.clientX - p.lx
    const dy = e.clientY - p.ly
    p.lx = e.clientX
    p.ly = e.clientY
    p.moved += Math.abs(dx) + Math.abs(dy)
    view.current.x += dx
    view.current.y += dy
    apply()
  }
  const onPointerUp = (e: React.PointerEvent) => {
    if (onCtl.current) {
      onCtl.current = false
      return
    }
    const p = pan.current
    if (p.id !== e.pointerId) return
    pan.current.id = -1
    if (p.moved < 6) {
      const el = document.elementFromPoint(e.clientX, e.clientY)
      const g = el && el.closest ? el.closest('.lcn-node') : null
      const id = g ? Number((g as HTMLElement).dataset.id) : NaN
      if (!Number.isNaN(id) && lay.byId.has(id)) onSelect(id)
    }
  }

  const zoomStep = (k: number) => {
    const wrap = wrapRef.current
    if (!wrap) return
    zoomAt(wrap.clientWidth / 2, wrap.clientHeight / 2, k)
  }

  /** 全屏开关：原生 API 优先，失败（如浏览器不支持）则退回 CSS 铺满 */
  const toggleFs = useCallback(() => {
    const el = wrapRef.current
    if (!el) return
    if (document.fullscreenElement) {
      document.exitFullscreen().catch(() => {})
      return
    }
    if (fsCss) {
      setFsCss(false)
      return
    }
    const req = el.requestFullscreen ? el.requestFullscreen.bind(el) : null
    if (req) req().catch(() => setFsCss(true))
    else setFsCss(true)
  }, [fsCss])

  // 原生全屏状态跟随浏览器（ESC 退出也要同步图标）
  useEffect(() => {
    const onCh = () => setFsNative(!!document.fullscreenElement)
    document.addEventListener('fullscreenchange', onCh)
    return () => document.removeEventListener('fullscreenchange', onCh)
  }, [])

  // 容器尺寸变了（进/出全屏），保持当前 transform 即可，SVG 自适应
  useEffect(() => {
    apply()
  }, [isFs, apply])

  // 选中人的祖师链
  const anc = useMemo(() => {
    const s = new Set<number>()
    let cur = selId
    let guard = 0
    while (cur != null && lay.byId.has(cur) && guard++ < 60) {
      s.add(cur)
      cur = lay.byId.get(cur)!.masterId
    }
    return s
  }, [selId, lay])

  // 搜索命中
  const hits = useMemo(() => {
    const key = norm(query)
    if (!key) return null
    const s = new Set<number>()
    for (const n of lay.nodes) if (norm(n.name).includes(key)) s.add(n.id)
    return s
  }, [query, lay])

  const mixed = useMemo(() => {
    let male = false
    let female = false
    for (const r of rows) {
      if (r.team === 'female') female = true
      else male = true
    }
    return male && female
  }, [rows])

  return (
    <div
      className={`lin-canvas${fsCss ? ' fs' : ''}`}
      ref={wrapRef}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={() => {
        onCtl.current = false
        pan.current.id = -1
      }}
    >
      <span className="lcn-tip">拖动平移 · 滚轮缩放 · 点人看详情</span>
      <div className="lcn-ctl">
        <button title="放大" onClick={() => zoomStep(1.25)}>
          +
        </button>
        <button title="缩小" onClick={() => zoomStep(0.8)}>
          −
        </button>
        <button title="复位" onClick={fit}>
          ⌖
        </button>
        <button title={isFs ? '退出全屏' : '全屏'} onClick={toggleFs}>
          {isFs ? (
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <path d="M8 3v3a2 2 0 0 1-2 2H3M16 3v3a2 2 0 0 0 2 2h3M8 21v-3a2 2 0 0 0-2-2H3M16 21v-3a2 2 0 0 1 2-2h3" />
            </svg>
          ) : (
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <path d="M8 3H5a2 2 0 0 0-2 2v3M16 3h3a2 2 0 0 1 2 2v3M8 21H5a2 2 0 0 1-2-2v-3M16 21h3a2 2 0 0 0 2-2v-3" />
            </svg>
          )}
        </button>
      </div>
      <svg>
        <g ref={gRef}>
          {lay.edges.map((e) => {
            const hot = anc.has(e.to.id) && anc.has(e.from.id)
            const dim = anc.size > 0 && !hot
            /* 横向连线：父节点右缘 → 子节点左缘，中间走一段竖线 */
            const x1 = e.from.x + W / 2
            const x2 = e.to.x - W / 2
            const mx = (x1 + x2) / 2
            return (
              <path
                key={e.to.id}
                className={`lcn-edge${hot ? ' hot' : dim ? ' dim' : ''}`}
                d={`M${x1},${e.from.y} H${mx} V${e.to.y} H${x2}`}
              />
            )
          })}
          {lay.nodes.map((n) => {
            const off = n.status === 'left'
            const cls = [
              'lcn-node',
              off ? 'off' : '',
              selId === n.id ? 'sel' : '',
              anc.size > 0 && anc.has(n.id) && selId !== n.id ? 'anc' : '',
              anc.size > 0 && !anc.has(n.id) ? 'dim2' : '',
              hits?.has(n.id) ? 'hit' : '',
              n.team === 'female' ? 'f' : '',
            ]
              .filter(Boolean)
              .join(' ')
            const sub = n.disciples > 0 ? `${n.disciples} 徒` : off ? '已离团' : `第 ${n.depth + 1} 代`
            return (
              <g key={n.id} className={cls} data-id={n.id} transform={`translate(${n.x},${n.y})`}>
                <rect x={-W / 2} y={-26} width={W} height={52} rx={11} />
                {mixed && <circle className="lcn-dot" cx={-W / 2 + 10} cy={-26 + 10} r={3.2} />}
                <text className="lcn-name" y={-3}>
                  {n.name}
                </text>
                <text className="lcn-sub" y={16}>
                  {sub}
                </text>
              </g>
            )
          })}
        </g>
      </svg>
      {lay.nodes.length === 0 && <p className="lcn-empty">没有可显示的主播</p>}
    </div>
  )
}
