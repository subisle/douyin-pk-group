/**
 * 族谱导出图片：把整棵树拼成一张独立 SVG（纯内联属性，不依赖页面 CSS）→ Canvas → PNG。
 *
 * 为什么不用页面里那个 SVG 画布直接序列化：画布节点靠 CSS 类上色（`.lcn-node rect{...}`），
 * 外部样式表带不进序列化结果；这里按布局数据现拼一份带内联属性的 SVG，颜色/字体都可控。
 *
 * 顺带复用 exportImage.ts 的两个坑结论：data URL 必须 encodeURIComponent（否则 # 被当片段截断）、
 * 不要用 blob: URL（画布会被判污染，toBlob 抛 SecurityError）。
 */

import { layoutForest, W, PAD, type CanvasRow, type CNode } from './LineageCanvas'
import { downloadBlob } from './pk/exportImage'
import { isoLocal } from './lib/date'

/* 与展示端画布同源的 8 色循环（改配色要三处同步：这里、pk/exportImage.ts、st_page.html） */
const THEMES = [
  { bg: '#fdeaf1', bd: '#f6a8c5', tc: '#e04f8a' },
  { bg: '#f0ecfd', bd: '#b3a1f2', tc: '#7a58e0' },
  { bg: '#e4f7fb', bd: '#7fd0e2', tc: '#189db8' },
  { bg: '#fdf3dd', bd: '#f4c46a', tc: '#dd8f1d' },
  { bg: '#e7f7ec', bd: '#82cb9b', tc: '#2f9e5d' },
  { bg: '#fdebe9', bd: '#f19a97', tc: '#dd4f4c' },
  { bg: '#e9eefc', bd: '#98aaee', tc: '#4c63d6' },
  { bg: '#e6f6f1', bd: '#74c7ab', tc: '#1c9b77' },
]

const FONT = "-apple-system,BlinkMacSystemFont,'PingFang SC','Hiragino Sans GB',sans-serif"
const HEAD_H = 132
const FOOT_H = 64

const esc = (s: unknown) =>
  String(s == null ? '' : s).replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c] as string),
  )

/** 名字列表按可用宽度折行（中文按 1 字宽、ASCII 按 0.55 估） */
function wrapNames(names: string[], maxW: number, fontPx = 12): string[] {
  const wOf = (s: string) => [...s].reduce((w, ch) => w + (/[\x00-\xff]/.test(ch) ? fontPx * 0.55 : fontPx), 0)
  const lines: string[] = []
  let cur = ''
  for (const n of names) {
    const candidate = cur ? cur + '、' + n : n
    if (cur && wOf(candidate) > maxW) {
      lines.push(cur)
      cur = n
    } else {
      cur = candidate
    }
  }
  if (cur) lines.push(cur)
  return lines
}

export interface LineageMeta {
  title: string
  subtitle: string
  footer: string
  /** 底部名单（没师傅的人）。不传就按 rows 自己算；传了就用它——
   *  因为「没师傅又没徒弟」的散人不画在树里，名单要用全量数据 */
  orphanNames?: string[]
}

export function buildLineageSvg(
  rows: CanvasRow[],
  meta: LineageMeta,
): { svg: string; width: number; height: number } {
  const lay = layoutForest(rows)
  const dy = HEAD_H
  const maxDepth = lay.nodes.reduce((m, n) => Math.max(m, n.depth), 0)
  /* 门色：layoutForest 已把 roots 按子树大小排好，直接按该顺序取色 = 与画布完全一致 */
  const rootTheme = new Map<number, (typeof THEMES)[number]>()
  lay.roots.forEach((r, i) => rootTheme.set(r.id, THEMES[i % THEMES.length]))
  const themeFor = (n: CNode): (typeof THEMES)[number] | null => {
    if (n.depth > 1) return null
    let cur: CNode = n
    const guard = new Set<number>()
    while (guard.size < 60) {
      guard.add(cur.id)
      if (rootTheme.has(cur.id)) return rootTheme.get(cur.id) as (typeof THEMES)[number]
      const pid = cur.masterId
      const parent = pid != null ? lay.byId.get(pid) : undefined
      if (!parent) return null
      cur = parent
    }
    return null
  }

  /* ---- 排版：先按师门拆树，再把树贪心装列（目标是接近方形，别出一条 1:8 的长条） ---- */
  const GAP_X = 56
  const GAP_Y = 44
  interface TreeBox {
    root: CNode
    nodes: CNode[]
    edges: { from: CNode; to: CNode }[]
    minX: number
    minY: number
    w: number
    h: number
    x: number
    y: number
  }
  const boxes: TreeBox[] = lay.roots.map((root) => {
    const nodes: CNode[] = []
    const edges: { from: CNode; to: CNode }[] = []
    const walk = (n: CNode) => {
      nodes.push(n)
      for (const k of n.kids) {
        edges.push({ from: n, to: k })
        walk(k)
      }
    }
    walk(root)
    const minX = Math.min(...nodes.map((n) => n.x)) - W / 2
    const minY = Math.min(...nodes.map((n) => n.y)) - 26
    const maxX = Math.max(...nodes.map((n) => n.x)) + W / 2
    const maxY = Math.max(...nodes.map((n) => n.y)) + 26
    return { root, nodes, edges, minX, minY, w: maxX - minX, h: maxY - minY, x: 0, y: 0 }
  })
  const colW = Math.max(...boxes.map((b) => b.w), 320)
  const sumH = boxes.reduce((s, b) => s + b.h + GAP_Y, 0)
  const targetH = Math.max(Math.max(...boxes.map((b) => b.h)), Math.sqrt(sumH * colW))
  const cols: { boxes: TreeBox[]; w: number; h: number }[] = []
  let cur = { boxes: [] as TreeBox[], w: 0, h: 0 }
  for (const b of boxes) {
    if (cur.boxes.length && cur.h + b.h + GAP_Y > targetH) {
      cols.push(cur)
      cur = { boxes: [], w: 0, h: 0 }
    }
    cur.boxes.push(b)
    cur.w = Math.max(cur.w, b.w)
    cur.h += b.h + (cur.boxes.length > 1 ? GAP_Y : 0)
  }
  if (cur.boxes.length) cols.push(cur)
  let cx = PAD
  let bodyH = 0
  for (const c of cols) {
    let cy = 0
    for (const b of c.boxes) {
      b.x = cx
      b.y = cy
      cy += b.h + GAP_Y
    }
    bodyH = Math.max(bodyH, c.h)
    cx += c.w + GAP_X
  }
  const width = Math.max(cx - GAP_X + PAD, 640)
  /* 没有师傅的人：图底部再补一份文字名单（树里他们是孤立的单节点小树，容易漏看） */
  const orphans = meta.orphanNames ?? rows.filter((r) => r.masterId == null).map((r) => r.name)
  const listLines = orphans.length ? wrapNames(orphans, width - PAD * 2) : []
  const listH = orphans.length ? 50 + listLines.length * 20 : 0
  const listTop = HEAD_H + bodyH
  const height = HEAD_H + bodyH + listH + FOOT_H
  const posX = (b: TreeBox, n: CNode) => b.x + (n.x - b.minX)
  const posY = (b: TreeBox, n: CNode) => b.y + (n.y - b.minY) + dy

  let edges = ''
  let nodes = ''
  /* 男女团混编时（后台「全部团别」）节点左上角加团别小色点，与画布一致 */
  const mixed = rows.some((r) => r.team === 'female') && rows.some((r) => r.team !== 'female')
  for (const b of boxes) {
    for (const e of b.edges) {
      const th = themeFor(e.to)
      const stroke = e.to.depth === 1 && th ? th.bd : '#C3CDDC'
      const x1 = posX(b, e.from) + W / 2
      const x2 = posX(b, e.to) - W / 2
      const mx = (x1 + x2) / 2
      edges += `<path d="M${x1},${posY(b, e.from)} H${mx} V${posY(b, e.to)} H${x2}" fill="none" stroke="${stroke}" stroke-width="1.5"/>`
    }
    for (const n of b.nodes) {
      const th = themeFor(n)
      const off = n.status === 'left'
      const fill = n.depth === 0 && th ? th.bg : '#FFFFFF'
      const stroke = th ? th.bd : '#E5E7EB'
      const sw = n.depth === 0 ? 1.5 : 1
      const nameFill = th && !off ? th.tc : '#0b1c30'
      const sub = n.disciples > 0 ? `${n.disciples} 徒` : off ? '已离团' : `第 ${n.depth + 1} 代`
      const nx = posX(b, n)
      const ny = posY(b, n)
      nodes +=
        `<g opacity="${off ? 0.45 : 1}">` +
        `<rect x="${nx - W / 2}" y="${ny - 26}" width="${W}" height="52" rx="11" fill="${fill}" stroke="${stroke}" stroke-width="${sw}"/>` +
        (mixed
          ? `<circle cx="${nx - W / 2 + 10}" cy="${ny - 16}" r="3.2" fill="${n.team === 'female' ? '#db2777' : '#3b5bdb'}"/>`
          : '') +
        `<text x="${nx}" y="${ny - 3}" text-anchor="middle" font-family="${FONT}" font-size="13" font-weight="700" fill="${nameFill}">${esc(n.name)}</text>` +
        `<text x="${nx}" y="${ny + 16}" text-anchor="middle" font-family="${FONT}" font-size="10" fill="#94a3b8">${esc(sub)}</text>` +
        `</g>`
    }
  }

  /* 装列排版后每棵树的 x 各自独立，「第 N 代」全局标尺不再对齐，故不画标尺 */
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}">` +
    `<rect width="${width}" height="${height}" fill="#ffffff"/>` +
    `<rect width="${width}" height="${HEAD_H}" fill="#F8FAFF"/>` +
    `<text x="${PAD}" y="56" font-family="${FONT}" font-size="26" font-weight="800" fill="#0b1c30">${esc(meta.title)}</text>` +
    `<text x="${PAD}" y="84" font-family="${FONT}" font-size="13" fill="#64748b">${esc(meta.subtitle)}</text>` +
    `<line x1="0" y1="${HEAD_H}" x2="${width}" y2="${HEAD_H}" stroke="#E5E7EB" stroke-width="1"/>` +
    edges + nodes +
    (listH
      ? `<line x1="0" y1="${listTop + 6}" x2="${width}" y2="${listTop + 6}" stroke="#E5E7EB" stroke-width="1"/>` +
        `<text x="${PAD}" y="${listTop + 32}" font-family="${FONT}" font-size="13" font-weight="700" fill="#475467">没有师傅（${orphans.length} 人）</text>` +
        listLines
          .map(
            (ln, i) =>
              `<text x="${PAD}" y="${listTop + 54 + i * 20}" font-family="${FONT}" font-size="12" fill="#64748b">${esc(ln)}</text>`,
          )
          .join('')
      : '') +
    `<line x1="0" y1="${height - FOOT_H + 12}" x2="${width}" y2="${height - FOOT_H + 12}" stroke="#E5E7EB" stroke-width="1"/>` +
    `<text x="${width / 2}" y="${height - 24}" text-anchor="middle" font-family="${FONT}" font-size="12" fill="#94a3b8">${esc(meta.footer)}</text>` +
    `</svg>`

  return { svg, width, height }
}

/** 导出并下载 PNG；返回图片尺寸，便于调用方提示 */
export async function exportLineagePng(
  rows: CanvasRow[],
  meta: LineageMeta,
  scale = 2,
): Promise<{ width: number; height: number; bytes: number }> {
  const { svg, width, height } = buildLineageSvg(rows, meta)
  /* 太长（108 人 ≈ 7000px）时降倍率，别撞 canvas 尺寸上限 */
  const s = Math.max(1, Math.min(scale, 15000 / height))
  const encoded = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`
  const img = new Image()
  img.src = encoded
  try {
    await img.decode()
  } catch (e) {
    throw new Error(`族谱图片解码失败（${width}×${height}）：${(e as Error).message}`)
  }
  const canvas = document.createElement('canvas')
  canvas.width = Math.round(width * s)
  canvas.height = Math.round(height * s)
  const ctx = canvas.getContext('2d')
  if (!ctx) throw new Error('无法获取 canvas 2d 上下文')
  ctx.fillStyle = '#ffffff'
  ctx.fillRect(0, 0, canvas.width, canvas.height)
  ctx.drawImage(img, 0, 0, canvas.width, canvas.height)
  const blob = await new Promise<Blob>((resolve, reject) =>
    canvas.toBlob((b) => (b ? resolve(b) : reject(new Error('canvas 转 PNG 失败'))), 'image/png'),
  )
  /* 文件名日期用本地时区（P2-6）：toISOString 是 UTC，早 8 点前导出文件名会差一天 */
  downloadBlob(blob, `${meta.title.replace(/[\\/:*?"<>|\s]+/g, '')}-${isoLocal()}.png`)
  return { width: canvas.width, height: canvas.height, bytes: blob.size }
}
