import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { api } from './api'
import { useAutoRefresh } from './useAutoRefresh'
import LineageCanvas, { type FocusReq } from './LineageCanvas'
import { exportLineagePng } from './lineageExport'
import './lineage.css'

/** /api/admin/streamers 的一行（族谱只用得上这几列） */
interface Row {
  personId: number
  name: string
  team: string
  masterId: number | null
  masterName: string | null
  disciples: number
  generation: number | null
  status?: string
  joinedAt?: string | null
  douyinNo?: string | null
  douyinNos?: string | null
  aliases?: string[]
}

/** 树节点：depth 0 = 师门根（没有师傅，或师傅不在册/被筛掉） */
interface TreeNode {
  row: Row
  depth: number
  /** 师门链：从祖师到自己 */
  chain: Row[]
  kids: TreeNode[]
  /** 自己命中搜索词 */
  hit: boolean
  /** 自己或后代命中搜索词（搜索时用来决定展开哪支、保留哪些师门） */
  branchHit: boolean
}

const teamLabel = (t?: string) => (t === 'female' ? '女团' : '男团')
const norm = (s: string) => (s || '').replace(/\s+/g, '')
/** 2026-10-09 → 2026年10月9日 */
const todayCn = () => {
  const d = new Date()
  return `${d.getFullYear()}年${d.getMonth() + 1}月${d.getDate()}日`
}

/** 按 master_id 组树。指向的人不在（被筛掉 / 已删除 / 自指）时当师门根，避免整棵子树消失 */
function buildForest(rows: Row[], q: string): TreeNode[] {
  const byId = new Map(rows.map((r) => [r.personId, r]))
  const kidsOf = new Map<number, Row[]>()
  const roots: Row[] = []
  for (const r of rows) {
    const pid = r.masterId
    if (pid != null && pid !== r.personId && byId.has(pid)) {
      const arr = kidsOf.get(pid)
      if (arr) arr.push(r)
      else kidsOf.set(pid, [r])
    } else {
      roots.push(r)
    }
  }
  const key = norm(q)
  // 徒弟多的排前面（大支先看），同数量按姓名
  const sortFn = (a: Row, b: Row) =>
    b.disciples - a.disciples || a.name.localeCompare(b.name, 'zh-Hans-CN')
  roots.sort(sortFn)
  for (const arr of kidsOf.values()) arr.sort(sortFn)

  const visited = new Set<number>()
  const walk = (row: Row, depth: number, chain: Row[]): TreeNode => {
    visited.add(row.personId)
    const kids = (kidsOf.get(row.personId) ?? [])
      .filter((c) => !visited.has(c.personId))
      .map((c) => walk(c, depth + 1, [...chain, row]))
    const hit = !!key && norm(row.name).includes(key)
    return {
      row,
      depth,
      chain: [...chain, row],
      kids,
      hit,
      branchHit: hit || kids.some((k) => k.branchHit),
    }
  }
  return roots.map((r) => walk(r, 0, []))
}

function flatten(list: TreeNode[]): TreeNode[] {
  return list.flatMap((n) => [n, ...flatten(n.kids)])
}

interface NodeProps {
  node: TreeNode
  sel: number | null
  isOpen: (n: TreeNode) => boolean
  onToggle: (n: TreeNode) => void
  onSelect: (id: number) => void
}

/** 一个节点 + 它的子树（纯展示，状态都在 LineagePage 里） */
function TreeNodeView({ node, sel, isOpen, onToggle, onSelect }: NodeProps) {
  const r = node.row
  const hasKids = node.kids.length > 0
  const opened = isOpen(node)
  return (
    <div className="lin-node">
      <div className={`lin-row${sel === r.personId ? ' cur' : ''}${node.hit ? ' hit' : ''}`}>
        <button
          className="lin-caret"
          disabled={!hasKids}
          title={hasKids ? (opened ? '收起' : `展开 ${node.kids.length} 个徒弟`) : '没有徒弟'}
          onClick={() => hasKids && onToggle(node)}
        >
          {hasKids ? (opened ? '▾' : '▸') : '·'}
        </button>
        <button
          className="lin-name"
          title={r.aliases?.length ? `别名：${r.aliases.join('、')}` : r.name}
          onClick={() => onSelect(r.personId)}
        >
          {r.name}
        </button>
        <span className={`lin-team${r.team === 'female' ? ' f' : ''}`}>{teamLabel(r.team)}</span>
        {r.disciples > 0 && <span className="lin-badge">{r.disciples} 徒</span>}
        <span className="lin-gen">第 {node.depth + 1} 代</span>
        {r.status === 'left' && <span className="lin-left">已离团</span>}
      </div>
      {hasKids && opened && (
        <div className="lin-children">
          {node.kids.map((k) => (
            <TreeNodeView key={k.row.personId} node={k} sel={sel} isOpen={isOpen} onToggle={onToggle} onSelect={onSelect} />
          ))}
        </div>
      )}
    </div>
  )
}

export default function LineagePage() {
  const [items, setItems] = useState<Row[]>([])
  const [q, setQ] = useState('')
  const [team, setTeam] = useState('')
  const [view, setView] = useState<'tree' | 'list' | 'gen'>('tree')
  const [expanded, setExpanded] = useState<Set<number>>(new Set())
  /** 字辈视图里收起的字辈（存首字） */
  const [genFolded, setGenFolded] = useState<Set<string>>(new Set())
  const [sel, setSel] = useState<number | null>(null)
  const [error, setError] = useState('')
  const [msg, setMsg] = useState('')

  /** 树图居中请求：详情面板点链 / 搜索回车时让画布把人带到眼前 */
  const focusN = useRef(0)
  const [focusReq, setFocusReq] = useState<FocusReq | null>(null)
  const focusOn = useCallback((id: number, select: boolean) => {
    focusN.current += 1
    setFocusReq({ id, n: focusN.current })
    if (select) setSel(id)
  }, [])

  /** 导出整棵族谱为 PNG（按当前团别筛选；树只画师门，「没师傅又没徒弟」的散人只进底部名单） */
  const [exporting, setExporting] = useState(false)
  async function doExport(rows: Row[], stats: { schools: number; people: number; deepest: number }) {
    if (exporting) return
    setExporting(true)
    setError('')
    setMsg('')
    try {
      const scope = team ? teamLabel(team) : '男女团'
      const r = await exportLineagePng(rows, {
        title: `${scope} · 族谱`,
        subtitle: `师门 ${rows.filter((x) => x.masterId == null).length} 支 · 上树 ${rows.length} 人 · 最深 ${stats.deepest} 代`,
        footer: `抖音PK分组 · 生成于 ${todayCn()}`,
        orphanNames: shown.filter((i) => i.masterId == null).map((i) => i.name),
      })
      setMsg(`已导出族谱图片（${r.width}×${r.height}）`)
    } catch (e: any) {
      setError(e?.message ?? String(e))
    } finally {
      setExporting(false)
    }
  }

  /** 改师傅弹窗（复用主播管理那套 mask + master-pop） */
  const [masterEdit, setMasterEdit] = useState<Row | null>(null)
  const [mq, setMq] = useState('')
  const [savingMaster, setSavingMaster] = useState(false)

  /** 批量登记师门（微信群接龙粘贴） */
  const [batchOpen, setBatchOpen] = useState(false)
  const [batchText, setBatchText] = useState('')
  const [batchCreate, setBatchCreate] = useState(false)
  const [batchBusy, setBatchBusy] = useState(false)
  interface BatchItem {
    raw: string
    name: string
    master: string | null
    action: 'attach' | 'create' | 'unchanged' | 'skip' | 'error'
    note: string
  }
  const [batchResult, setBatchResult] = useState<BatchItem[] | null>(null)

  async function submitBatch() {
    if (batchBusy || !batchText.trim()) return
    setBatchBusy(true)
    setError('')
    setMsg('')
    try {
      const r = await api<{ updated: number; created: number; skipped: number; failed: number; report: BatchItem[] }>(
        '/api/admin/streamers/master-batch',
        { method: 'POST', body: JSON.stringify({ text: batchText, createMissing: batchCreate }) },
      )
      setBatchResult(r.report ?? [])
      setMsg(
        `批量登记完成：挂接 ${r.updated} · 新建 ${r.created} · 跳过 ${r.skipped}` +
          (r.failed ? ` · 失败 ${r.failed}` : ''),
      )
      load()
    } catch (e: any) {
      setError(e.message ?? String(e))
    } finally {
      setBatchBusy(false)
    }
  }

  const load = useCallback(() => {
    api<{ items: Row[] }>('/api/admin/streamers')
      .then((d) => {
        setItems(d.items ?? [])
        // 默认全部展开（104 人 4 代，一屏能看完）；「全部收起」后按需点开
        setExpanded(new Set((d.items ?? []).map((i: Row) => i.personId)))
      })
      .catch((e) => setError(e.message ?? String(e)))
  }, [])
  useEffect(load, [load])
  // 别处改了师徒关系 → 自动重拉
  useAutoRefresh(load, ['roster'])

  const shown = useMemo(() => (team ? items.filter((i) => i.team === team) : items), [items, team])
  /** 上树的人：没师傅又没徒弟的散人不画进树里（有徒弟的师门根保留，否则整棵树就没了） */
  const treeRows = useMemo(
    () => shown.filter((r) => !(r.masterId == null && r.disciples === 0)),
    [shown],
  )
  const forest = useMemo(() => buildForest(shown, q), [shown, q])
  const searching = norm(q).length > 0
  const visible = searching ? forest.filter((n) => n.branchHit) : forest

  /** 字辈视图：按名字第一个字分组（如「浩」→ 浩字辈） */
  const genGroups = useMemo(() => {
    const map = new Map<string, Row[]>()
    for (const r of shown) {
      const c = norm(r.name).charAt(0)
      if (!c) continue
      const arr = map.get(c)
      if (arr) arr.push(r)
      else map.set(c, [r])
    }
    return [...map.entries()]
      .map(([char, people]) => ({ char, people }))
      .sort(
        (a, b) =>
          b.people.length - a.people.length || a.char.localeCompare(b.char, 'zh-Hans-CN'),
      )
  }, [shown])

  const stats = useMemo(() => {

    const all = flatten(forest)
    return {
      schools: forest.length,
      people: shown.length,
      withMaster: shown.filter((i) => i.masterId != null && i.masterId !== i.personId).length,
      deepest: all.length ? all.reduce((m, n) => Math.max(m, n.depth), 0) + 1 : 0,
      hits: searching ? all.filter((n) => n.hit).length : 0,
    }
  }, [forest, shown, searching])

  const byId = useMemo(() => new Map(items.map((i) => [i.personId, i])), [items])
  /** 没有师傅的人（师门根/散人）——单独列成文字，方便逐个去指定师傅 */
  const orphans = useMemo(() => shown.filter((i) => i.masterId == null), [shown])
  const selNode = useMemo(
    () => (sel == null ? null : flatten(forest).find((n) => n.row.personId === sel) ?? null),
    [forest, sel],
  )

  /** 搜索回车：在命中的人之间循环定位（树图/列表都受益） */
  const hitList = useMemo(() => {
    const key = norm(q)
    if (!key) return []
    return shown.filter((i) => norm(i.name).includes(key)).map((i) => i.personId)
  }, [q, shown])
  const hitIdx = useRef(0)
  useEffect(() => {
    hitIdx.current = 0
  }, [q])
  const onSearchKey = (e: React.KeyboardEvent) => {
    if (e.key !== 'Enter' || hitList.length === 0) return
    if (hitIdx.current >= hitList.length) hitIdx.current = 0
    focusOn(hitList[hitIdx.current], false)
    hitIdx.current += 1
    setMsg(`命中 ${hitList.length} 人，第 ${hitIdx.current} 个已定位`)
  }

  /** 搜索时按命中自动展开；平时按用户点开的集合 */
  const isOpen = useCallback(
    (n: TreeNode) => (searching ? n.branchHit : expanded.has(n.row.personId)),
    [expanded, searching],
  )
  const toggle = useCallback((n: TreeNode) => {
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(n.row.personId)) next.delete(n.row.personId)
      else next.add(n.row.personId)
      return next
    })
  }, [])
  const expandAll = (v: boolean) =>
    setExpanded(v ? new Set(flatten(forest).map((n) => n.row.personId)) : new Set())

  /** 设/清师傅：null = 清除。改完重新拉数据（族谱结构变了） */
  async function saveMaster(row: Row, masterId: number | null) {
    if (savingMaster) return
    setSavingMaster(true)
    setError('')
    setMsg('')
    try {
      const r = await api<{ masterName?: string | null }>(
        `/api/admin/streamers/${row.personId}/master`,
        { method: 'PUT', body: JSON.stringify({ masterId }) },
      )
      setMsg(
        masterId == null
          ? `已清除「${row.name}」的师傅`
          : `已把「${row.name}」的师傅设为「${r.masterName ?? ''}」`,
      )
      setMasterEdit(null)
      load()
    } catch (e: any) {
      setError(e.message ?? String(e))
    } finally {
      setSavingMaster(false)
    }
  }

  const cands = items.filter((i) => {
    if (masterEdit && i.personId === masterEdit.personId) return false
    if (!mq.trim()) return true
    return norm(i.name).includes(norm(mq))
  })

  return (
    <div className="page">
      <section className="card">
        <h2>
          族谱
          <span className="hint">师门关系：师傅 → 徒弟（数据来自主播管理的师傅字段，可在右侧改）</span>
        </h2>
        <div className="lin-stats">
          <div className="lin-stat"><b>{stats.schools}</b><span>师门</span></div>
          <div className="lin-stat"><b>{stats.people}</b><span>在册主播</span></div>
          <div className="lin-stat"><b>{stats.withMaster}</b><span>有师傅</span></div>
          <div className="lin-stat"><b>{stats.deepest}</b><span>最深代数</span></div>
          {searching && (
            <div className="lin-stat s-hit"><b>{stats.hits}</b><span>命中「{q}」</span></div>
          )}
        </div>
        <div className="row lin-tools">
          <input
            placeholder="搜主播名，回车定位"
            value={q}
            style={{ minWidth: 180 }}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={onSearchKey}
          />
          <select value={team} onChange={(e) => setTeam(e.target.value)}>
            <option value="">全部团别</option>
            <option value="male">男团</option>
            <option value="female">女团</option>
          </select>
          <div className="lin-viewseg">
            <button className={view === 'tree' ? 'on' : ''} onClick={() => setView('tree')}>
              树图
            </button>
            <button className={view === 'list' ? 'on' : ''} onClick={() => setView('list')}>
              列表
            </button>
            <button className={view === 'gen' ? 'on' : ''} onClick={() => setView('gen')}>
              字辈
            </button>
          </div>
          {view === 'list' && (
            <>
              <button onClick={() => expandAll(true)}>全部展开</button>
              <button onClick={() => expandAll(false)}>全部收起</button>
            </>
          )}
          <button disabled={exporting} onClick={() => doExport(treeRows, stats)}>
            {exporting ? '导出中…' : '导出图片'}
          </button>
          <button
            className="primary"
            onClick={() => {
              setBatchOpen(true)
              setBatchResult(null)
            }}
          >
            批量登记师门
          </button>
        </div>
        {error && <div className="alert bad">{error}</div>}
        {msg && <div className="alert good">{msg}</div>}
      </section>

      <div className="lin-wrap">
        <div className="lin-main">
          <section className={`card lin-tree${view === 'tree' ? ' tree-mode' : ''}`}>
            {view === 'tree' ? (
              <LineageCanvas rows={treeRows} selId={sel} onSelect={setSel} query={q} focusReq={focusReq} />
            ) : view === 'gen' ? (
              genGroups.length === 0 ? (
                <p className="hint">没有可显示的主播</p>
              ) : (
                <div className="lin-gen-groups">
                  {genGroups.map((g) => {
                    const folded = genFolded.has(g.char)
                    const people = g.people.filter((p) => !searching || norm(p.name).includes(norm(q)))
                    if (searching && people.length === 0) return null
                    return (
                      <div key={g.char} className="lin-gen-group">
                        <button
                          className="lin-gen-head"
                          onClick={() =>
                            setGenFolded((prev) => {
                              const next = new Set(prev)
                              if (next.has(g.char)) next.delete(g.char)
                              else next.add(g.char)
                              return next
                            })
                          }
                        >
                          <span className="lin-gen-char">{g.char}</span>
                          <span className="lin-gen-title">{g.char}字辈</span>
                          <span className="lin-badge sm">{g.people.length} 人</span>
                          <span className="lin-gen-caret">{folded ? '▸' : '▾'}</span>
                        </button>
                        {!folded && (
                          <div className="lin-chips lin-gen-people">
                            {people.map((p) => (
                              <button
                                key={p.personId}
                                className={`lin-chip${sel === p.personId ? ' on' : ''}${p.status === 'left' ? ' dim' : ''}`}
                                title={p.masterName ? `师傅：${p.masterName}` : '没有师傅'}
                                onClick={() => focusOn(p.personId, true)}
                              >
                                {p.name}
                                {p.disciples > 0 && <span className="lin-badge sm">{p.disciples} 徒</span>}
                              </button>
                            ))}
                          </div>
                        )}
                      </div>
                    )
                  })}
                </div>
              )
            ) : visible.length === 0 ? (
              <p className="hint">{searching ? `没有找到「${q}」` : '没有可显示的主播'}</p>
            ) : (
              visible.map((n) => (
                <TreeNodeView
                  key={n.row.personId}
                  node={n}
                  sel={sel}
                  isOpen={isOpen}
                  onToggle={toggle}
                  onSelect={setSel}
                />
              ))
            )}
          </section>

          {orphans.length > 0 && (
            <section className="card lin-orphans">
              <div className="lin-sec-t">
                没有师傅（{orphans.length} 人）
                <span className="hint">点名字 → 画布定位，右侧可指定师傅</span>
              </div>
              <div className="lin-chips">
                {orphans.map((o) => (
                  <button
                    key={o.personId}
                    className={`lin-chip${sel === o.personId ? ' on' : ''}`}
                    onClick={() => focusOn(o.personId, true)}
                  >
                    {o.name}
                    {o.disciples > 0 && <span className="lin-badge sm">{o.disciples} 徒</span>}
                  </button>
                ))}
              </div>
            </section>
          )}
        </div>

        <aside className="card lin-detail">
          {!selNode ? (
            <p className="hint">点左边的人名看详情</p>
          ) : (
            <>
              <h3>{selNode.row.name}</h3>
              <div className="lin-kv">
                <span>团别</span>
                <b>{teamLabel(selNode.row.team)}</b>
                <span>代数</span>
                <b>
                  第 {selNode.depth + 1} 代
                  {selNode.row.generation != null && selNode.row.generation !== selNode.depth
                    ? `（档案写第 ${selNode.row.generation + 1} 代）`
                    : ''}
                </b>
                <span>直系徒弟</span>
                <b>{selNode.kids.length} 人</b>
                <span>门下总计</span>
                <b>{flatten([selNode]).length - 1} 人</b>
                <span>抖音号</span>
                <b>{selNode.row.douyinNos || selNode.row.douyinNo || '—'}</b>
                {!!selNode.row.aliases?.length && (
                  <>
                    <span>别名</span>
                    <b>{selNode.row.aliases.join('、')}</b>
                  </>
                )}
                <span>入团</span>
                <b>{selNode.row.joinedAt || '—'}</b>
              </div>

              <div className="lin-sec">
                <div className="lin-sec-t">师门链（祖师 → 本人）</div>
                <div className="lin-chips">
                  {selNode.chain.map((c, i) => (
                    <span key={c.personId} className="lin-chain-item">
                      {i > 0 && <span className="lin-arrow">→</span>}
                      <button className="lin-chip" onClick={() => focusOn(c.personId, true)}>
                        {c.name}
                      </button>
                    </span>
                  ))}
                  {selNode.chain.length === 1 && <span className="hint">（师门根，没有师傅）</span>}
                </div>
              </div>

              {selNode.kids.length > 0 && (
                <div className="lin-sec">
                  <div className="lin-sec-t">直系徒弟 {selNode.kids.length} 人</div>
                  <div className="lin-chips">
                  {selNode.kids.map((k) => (
                    <button
                      className="lin-chip"
                      key={k.row.personId}
                      onClick={() => focusOn(k.row.personId, true)}
                    >
                      {k.row.name}
                      {k.row.disciples > 0 && <span className="lin-badge sm">{k.row.disciples} 徒</span>}
                    </button>
                  ))}
                  </div>
                </div>
              )}

              <div className="lin-sec">
                <div className="lin-sec-t">师傅</div>
                <div className="lin-chips">
                  {selNode.row.masterId != null ? (
                    <>
                      <button
                        className="lin-chip"
                        onClick={() => {
                          const mid = selNode.row.masterId
                          if (mid != null && byId.has(mid)) focusOn(mid, true)
                        }}
                      >
                        {selNode.row.masterName || `#${selNode.row.masterId}`}
                      </button>
                      <button className="lin-chip ghost" onClick={() => setMasterEdit(selNode.row)}>
                        改师傅
                      </button>
                      <button
                        className="lin-chip danger"
                        disabled={savingMaster}
                        onClick={() => saveMaster(selNode.row, null)}
                      >
                        清除师傅
                      </button>
                    </>
                  ) : (
                    <>
                      <span className="hint">没有师傅（师门根）</span>
                      <button className="lin-chip ghost" onClick={() => setMasterEdit(selNode.row)}>
                        指定师傅
                      </button>
                    </>
                  )}
                </div>
              </div>
            </>
          )}
        </aside>
      </div>

      {masterEdit && (
        <div className="mask" onClick={() => !savingMaster && setMasterEdit(null)}>
          <div className="master-pop" onClick={(e) => e.stopPropagation()}>
            <h2 style={{ marginBottom: 0 }}>
              设置师傅 · {masterEdit.name}
              <button className="linkish" style={{ float: 'right' }} onClick={() => setMasterEdit(null)}>
                关闭
              </button>
            </h2>
            <input autoFocus placeholder="搜师傅名" value={mq} onChange={(e) => setMq(e.target.value)} />
            {masterEdit.masterId != null && (
              <button disabled={savingMaster} onClick={() => saveMaster(masterEdit, null)}>
                清除师傅（当前：{byId.get(masterEdit.masterId)?.name ?? `#${masterEdit.masterId}`}）
              </button>
            )}
            <ul className="master-list">
              {cands.slice(0, 200).map((c) => (
                <li key={c.personId}>
                  <button disabled={savingMaster} onClick={() => saveMaster(masterEdit, c.personId)}>
                    <b>{c.name}</b>
                    <span className="hint">
                      {teamLabel(c.team)}
                      {c.disciples > 0 && ` · ${c.disciples} 徒`}
                      {c.generation != null && ` · 第 ${c.generation + 1} 代`}
                    </span>
                  </button>
                </li>
              ))}
              {cands.length === 0 && (
                <li className="hint" style={{ padding: 10 }}>
                  没有匹配的人
                </li>
              )}
            </ul>
            <p className="hint">不能设成自己或自己的徒子徒孙（后端会拦住成环）</p>
          </div>
        </div>
      )}

      {batchOpen && (
        <div className="mask" onClick={() => !batchBusy && setBatchOpen(false)}>
          <div className="master-pop batch-pop" onClick={(e) => e.stopPropagation()}>
            <h2 style={{ marginBottom: 0 }}>
              批量登记师门
              <button className="linkish" style={{ float: 'right' }} onClick={() => setBatchOpen(false)}>
                关闭
              </button>
            </h2>
            <p className="hint" style={{ margin: 0 }}>
              把微信群接龙原文整段粘进来。每行认一条「名字 师傅」，序号、加号、括号都能隔开
              （如 <code>1. 啸安➕狼兴</code>）；只写名字的行会跳过、不会清掉现有师傅。
            </p>
            <textarea
              autoFocus
              rows={9}
              value={batchText}
              placeholder={'1. 啸安➕狼兴\n2. 狼博 浩龙\n3. 凤九（南方楠）'}
              onChange={(e) => setBatchText(e.target.value)}
            />
            <label className="batch-opt">
              <input
                type="checkbox"
                checked={batchCreate}
                onChange={(e) => setBatchCreate(e.target.checked)}
              />
              库里没有的主播/师傅自动创建（团别记「未知」，稍后人工补）
            </label>
            <button className="primary" disabled={batchBusy || !batchText.trim()} onClick={submitBatch}>
              {batchBusy ? '登记中…' : '解析并登记'}
            </button>
            {batchResult && (
              <ul className="batch-result">
                {batchResult.map((it, i) => (
                  <li key={i} className={`batch-item ${it.action}`}>
                    <b>{it.name}</b>
                    {it.master && <span> → {it.master}</span>}
                    <span className="hint">{it.note}</span>
                  </li>
                ))}
                {batchResult.length === 0 && <li className="hint">没有解析出任何行</li>}
              </ul>
            )}
          </div>
        </div>
      )}
    </div>
  )
}
