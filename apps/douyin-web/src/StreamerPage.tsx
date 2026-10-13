import { useCallback, useEffect, useMemo, useState } from 'react'
import { api } from './api'
import { useAutoRefresh } from './useAutoRefresh'

/** 抖音号单元格：douyinNos 按 ' / ' 拆开竖排，第一个标「主号」；douyinCount 缺失时退化用号数判断。 */
function renderDouyinCell(s: any) {
  const nos = String(s.douyinNos ?? '')
    .split(' / ')
    .map((x: string) => x.trim())
    .filter(Boolean)
  // 老接口没有 douyinNos 时退回主号单个字段
  if (nos.length === 0 && s.douyinNo) nos.push(String(s.douyinNo).trim())
  const numCount = nos.length
  // 后端还没下发 douyinCount 时用已登记的号数兜底，保证不崩
  const total = typeof s.douyinCount === 'number' ? s.douyinCount : numCount
  const unregistered = Math.max(total - numCount, 0)

  if (total <= 0) {
    return (
      <td className="hint" title="这个主播名下还没有任何账号（右键 → 查看多抖音号 可绑定）">
        未绑定抖音号
      </td>
    )
  }
  if (numCount === 0) {
    return (
      <td className="hint" title="名下已有账号，但还没登记抖音号（可右键 → 查看多抖音号 补充）">
        未登记抖音号{s.anchorId ? `（采集号 ${s.anchorId}）` : ''}
      </td>
    )
  }
  return (
    <td title="主号在前；一个主播可挂多个抖音号，详情里绑定，导入时音浪合并">
      <div style={{ display: 'flex', flexDirection: 'column', gap: 4, alignItems: 'flex-start' }}>
        {nos.map((no, i) => (
          <span key={`${no}-${i}`} style={{ whiteSpace: 'nowrap' }}>
            {i === 0 && (
              <span className="badge b-active" style={{ marginRight: 4 }}>
                主号
              </span>
            )}
            {no}
          </span>
        ))}
      </div>
      {total > 1 && (
        <div className="hint" style={{ marginTop: 4 }}>
          {total} 个号
          {unregistered > 0 ? ` · 另有 ${unregistered} 个号未登记抖音号` : ''}
        </div>
      )}
    </td>
  )
}

/** 微信机器人单元格：优先新字段 wxbot（null=未扫码 / activated=能否主动推送），
 *  用 wechatPush 叠加推送开关；wxbot 字段还没到位时退回旧 wechatBound/wechatPush 三态。 */
function renderWxbotCell(s: any) {
  const wx = s.wxbot // undefined = 后端字段未到位；null = 没有扫码记录
  // 字段缺失，或字段为 null 但旧的推送开关说已绑定 —— 都退回旧口径，避免丢信息
  if (wx === undefined || (wx == null && s.wechatBound)) {
    if (!s.wechatBound) {
      return (
        <td title="未绑定微信机器人（主播在展示端「我的」页扫码后发手机号绑定）">
          <span className="hint">未绑定</span>
        </td>
      )
    }
    return (
      <td
        title={
          s.wechatPush
            ? '已绑定微信机器人，推送开着（数据入库约 30 分钟后私聊推送）'
            : '已绑定微信机器人，但两个推送开关都关着，不会收到任何推送'
        }
      >
        {s.wechatPush ? (
          <span className="badge b-active">已推送</span>
        ) : (
          <span className="badge b-paused">已绑·未开</span>
        )}
      </td>
    )
  }
  if (wx == null) {
    return (
      <td title="未绑定微信机器人（主播在展示端「我的」页扫码后发手机号绑定）">
        <span className="hint">未绑定</span>
      </td>
    )
  }
  const boundAt = wx.boundAt ? `（绑定于 ${wx.boundAt}）` : ''
  const pushOff = s.wechatPush === false
  const pushOffBadge = pushOff ? (
    <span className="badge b-left" style={{ marginLeft: 6 }} title="绑了但两个推送开关都关着，收不到推送">
      未开推送
    </span>
  ) : null
  if (!wx.activated) {
    return (
      <td title={`已扫码，还没给机器人发过消息，推送发不出去${boundAt}`}>
        <span className="badge b-paused">待激活</span>
        {pushOffBadge}
      </td>
    )
  }
  return (
    <td title={`已绑定微信机器人，能主动推送${boundAt}`}>
      <span className="badge b-active">已绑定</span>
      {wx.nickname ? (
        <span className="hint" style={{ marginLeft: 6 }}>
          {wx.nickname}
        </span>
      ) : null}
      {pushOffBadge}
    </td>
  )
}


export default function StreamerPage() {
  const [items, setItems] = useState<any[]>([])
  const [selected, setSelected] = useState<Set<number>>(new Set())
  const [detail, setDetail] = useState<any | null>(null)
  const [dupes, setDupes] = useState<any | null>(null)
  const [bindAnchorId, setBindAnchorId] = useState('')
  const [bindDouyinNo, setBindDouyinNo] = useState('')
  const [q, setQ] = useState('')
  const [team, setTeam] = useState('')
  const [name, setName] = useState('')
  const [douyinNo, setDouyinNo] = useState('')
  const [newTeam, setNewTeam] = useState('male')
  const [error, setError] = useState('')
  const [msg, setMsg] = useState('')
  const [masterEdit, setMasterEdit] = useState<{ id: number; name: string; masterId: number | null } | null>(null)
  const [mq, setMq] = useState('')
  const [savingMaster, setSavingMaster] = useState(false)
  /** 表格行右键菜单：坐标 + 目标主播 */
  const [ctxMenu, setCtxMenu] = useState<{ x: number; y: number; person: any } | null>(null)
  /** 编辑资料弹窗（名字 / 团别 / 主抖音号 / 手机号） */
  const [editPop, setEditPop] = useState<{ personId: number; name: string; team: string; douyinNo: string; phone: string } | null>(null)
  const [savingEdit, setSavingEdit] = useState(false)
  /** 合并弹窗：把「别人」并进 target（保留 target，账号与历史数据都归到 TA 名下） */
  const [mergePop, setMergePop] = useState<{ targetId: number; targetName: string; sourceId: number | null; q: string } | null>(null)
  const [merging, setMerging] = useState(false)
  /** 「我的」页面账号弹窗（右键主播 → 设置手机号/密码；主播用 姓名+密码 登录） */
  const [myAccPop, setMyAccPop] = useState<{ name: string; phone: string; pass: string; exists: boolean } | null>(null)
  /** 批量开号弹窗：给勾选的主播批量建展示端登录账号（result 非空 = 展示结果表） */
  const [batchAcc, setBatchAcc] = useState<{ defaultPass: string; forceReset: boolean; result: any | null } | null>(null)
  const [batchAccBusy, setBatchAccBusy] = useState(false)

  /** 导入暂存区：有抖音号/ID 但台账还没建档的行（不进榜单/统计，建档后自动补账） */
  const [pending, setPending] = useState<any[]>([])
  /** 暂存区默认收起：行数多时不再把台账顶到下面，需要处理再展开 */
  const [pendingOpen, setPendingOpen] = useState(false)

  const loadPending = useCallback(() => {
    // 线上没跑 006 迁移时接口会 404/500：静默忽略，不影响台账页
    api('/api/admin/import/pending')
      .then((d) => setPending(d.items ?? []))
      .catch(() => setPending([]))
  }, [])

  const load = useCallback(() => {
    const qs = new URLSearchParams()
    if (q) qs.set('q', q)
    if (team) qs.set('team', team)
    api(`/api/admin/streamers?${qs}`)
      .then((d) => setItems(d.items ?? []))
      .catch((e) => setError(e.message ?? String(e)))
    loadPending()
  }, [q, team, loadPending])

  useEffect(load, [load])

  /** 重名提示：姓名是行的唯一标识，从当前 items 现算同名（不在后端接口里新增）。 */
  const dupNames = useMemo(() => {
    const m = new Map<string, number>()
    for (const s of items) {
      const k = String(s.name ?? '').trim()
      if (!k) continue
      m.set(k, (m.get(k) ?? 0) + 1)
    }
    return m
  }, [items])

  // 别名增删 / 别处改了台账时自动刷新
  useAutoRefresh(load, ['roster'])

  /* 对外显隐开关：勾上 = 显示；取消 = 隐藏（hide_in_daily_report=1，榜单接口不下发此人） */
  async function toggleVisible(s: any) {
    const next = !s.hideInDailyReport
    try {
      await api(`/api/admin/streamers/${s.personId}`, {
        method: 'PUT',
        body: JSON.stringify({ hideInDailyReport: next }),
      })
      setItems((prev: any[]) =>
        prev.map((x) => (x.personId === s.personId ? { ...x, hideInDailyReport: next } : x)),
      )
      setMsg(next ? `${s.name} 已恢复对外显示` : `${s.name} 已隐藏（每日报告不再出现）`)
      setTimeout(() => setMsg(''), 2000)
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  async function create() {
    setError('')
    setMsg('')
    try {
      const d = await api<{ personId: number; absorbed?: number }>('/api/admin/streamers', {
        method: 'POST',
        body: JSON.stringify({ name, team: newTeam, douyinNo: douyinNo || undefined }),
      })
      setMsg(
        `已建档：${name}` +
          (d.absorbed
            ? `（已把暂存区里该抖音号的 ${d.absorbed} 行数据补账入库，现在开始出现在榜单里）`
            : ''),
      )
      setName('')
      setDouyinNo('')
      load()
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  /** 暂存行「建档」：把名字 / 抖音号填进上面的新建表单，运营补团别后点建档 */
  function prefillArchive(p: any) {
    setName(p.name ?? '')
    setDouyinNo(p.douyinNo ?? '')
    setMsg(`已填入：${p.name}（抖音号 ${p.douyinNo}）—— 选好团别点「建档」，暂存数据会自动补账`)
    window.scrollTo({ top: 0, behavior: 'smooth' })
  }

  async function discardPending(p: any) {
    if (!confirm(`丢弃暂存行「${p.name}（${p.douyinNo}）」？该行数据不入库。`)) return
    try {
      await api(`/api/admin/import/pending?id=${p.id}`, { method: 'DELETE' })
      loadPending()
      setMsg(`已丢弃暂存行：${p.name}`)
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  /* ---------------- 右键菜单 / 编辑 / 合并 ---------------- */

  // 点任意处关闭右键菜单（菜单内部点击已 stopPropagation）
  useEffect(() => {
    if (!ctxMenu) return
    const close = () => setCtxMenu(null)
    window.addEventListener('click', close)
    window.addEventListener('scroll', close, true)
    return () => {
      window.removeEventListener('click', close)
      window.removeEventListener('scroll', close, true)
    }
  }, [ctxMenu])

  /** 右键 → 设置「我的」页面账号：手机号（选填）+ 纯数字密码；主播用 姓名+密码 登录，没有注册 */
  async function openMyAccount(name: string) {
    setError('')
    setMsg('')
    try {
      const d = await api(`/api/admin/my-accounts/${encodeURIComponent(name)}`)
      setMyAccPop({
        name,
        phone: d.found ? d.phone ?? '' : '',
        pass: '',
        exists: !!d.found,
      })
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  async function saveMyAccount() {
    if (!myAccPop) return
    setError('')
    setMsg('')
    try {
      await api('/api/admin/my-accounts', {
        method: 'POST',
        body: JSON.stringify({
          name: myAccPop.name,
          phone: myAccPop.phone.trim(),
          ...(myAccPop.pass.trim() ? { pass: myAccPop.pass.trim() } : {}),
        }),
      })
      setMsg(`已保存「${myAccPop.name}」的我的页面账号`)
      setMyAccPop(null)
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  async function removeMyAccount() {
    if (!myAccPop || !myAccPop.exists) return
    setError('')
    setMsg('')
    try {
      await api('/api/admin/my-accounts/' + encodeURIComponent(myAccPop.name), { method: 'DELETE' })
      setMsg(`已删除「${myAccPop.name}」的我的页面账号`)
      setMyAccPop(null)
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  /** 编辑资料：改名 / 团别 / 主抖音号 / 手机号（PUT /api/admin/streamers/{id}，空手机号 = 清除登记） */
  async function saveEdit() {
    if (!editPop || savingEdit) return
    setSavingEdit(true)
    setError('')
    setMsg('')
    try {
      await api(`/api/admin/streamers/${editPop.personId}`, {
        method: 'PUT',
        body: JSON.stringify({
          name: editPop.name.trim() || undefined,
          team: editPop.team,
          douyinNo: editPop.douyinNo.trim() || undefined,
          phone: editPop.phone.trim(),
        }),
      })
      setMsg(`已保存「${editPop.name}」的资料`)
      setEditPop(null)
      load()
    } catch (e: any) {
      setError(e.message ?? String(e))
    } finally {
      setSavingEdit(false)
    }
  }

  /** 合并：把 source 并进 target（账号改绑、同日数据求和并入、source 软删） */
  async function doMerge() {
    if (!mergePop?.sourceId || merging) return
    setMerging(true)
    setError('')
    setMsg('')
    try {
      const sourceName = items.find((i) => i.personId === mergePop.sourceId)?.name ?? `#${mergePop.sourceId}`
      await api(`/api/admin/streamers/${mergePop.targetId}/merge`, {
        method: 'POST',
        body: JSON.stringify({ sourceId: mergePop.sourceId }),
      })
      setMsg(`已把「${sourceName}」并入「${mergePop.targetName}」：账号、别名与历史数据都归到 TA 名下`)
      setMergePop(null)
      load()
      loadDupes()
    } catch (e: any) {
      setError(e.message ?? String(e))
    } finally {
      setMerging(false)
    }
  }

  const loadDupes = useCallback(() => {
    api('/api/admin/streamers/duplicates')
      .then(setDupes)
      .catch(() => {})
  }, [])

  useEffect(() => {
    loadDupes()
  }, [loadDupes])

  async function showDetail(personId: number) {
    try {
      const d = await api(`/api/admin/streamers/${personId}/detail`)
      setDetail(d)
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  /** 绑定抖音号（2026-10-10：只填抖音号也能绑，采集 ID 选填；一人可挂多个，导入合并音浪） */
  async function bindAccount() {
    if (!detail) return
    setError('')
    try {
      const bound = await api<{ absorbed?: number }>(
        `/api/admin/streamers/${detail.personId}/accounts`,
        {
          method: 'POST',
          body: JSON.stringify({
            douyinNo: bindDouyinNo.trim(),
            anchorId: bindAnchorId.trim() || undefined,
          }),
        },
      )
      setBindAnchorId('')
      setBindDouyinNo('')
      const d = await api(`/api/admin/streamers/${detail.personId}/detail`)
      setDetail(d)
      setMsg(
        `已给「${detail.name}」绑定抖音号` +
          (bound.absorbed ? `（暂存区 ${bound.absorbed} 行数据已补账入库）` : ''),
      )
      setTimeout(() => setMsg(''), 2000)
      load()
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  /** 解绑一个抖音号（主号解绑后自动把剩下的第一个提为主号） */
  async function unbindAccount(accountId: number, douyinNo: string) {
    if (!detail) return
    if (!confirm(`确认解绑「${detail.name}」的抖音号 ${douyinNo}？该号之后的音浪不再并入 TA 名下。`))
      return
    setError('')
    try {
      await api(`/api/admin/streamers/${detail.personId}/accounts/${accountId}`, {
        method: 'DELETE',
      })
      const d = await api(`/api/admin/streamers/${detail.personId}/detail`)
      setDetail(d)
      load()
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  /** 设为主号（导入回填 anchor_id 用它） */
  async function setPrimary(accountId: number) {
    if (!detail) return
    setError('')
    try {
      await api(`/api/admin/streamers/${detail.personId}/accounts`, {
        method: 'POST',
        body: JSON.stringify({
          // 复用绑定接口做幂等 upsert：传原号 + isPrimary=true
          douyinNo: (detail.accounts ?? []).find((a: any) => a.id === accountId)?.douyinNo,
          isPrimary: true,
        }),
      })
      const d = await api(`/api/admin/streamers/${detail.personId}/detail`)
      setDetail(d)
      load()
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  async function batchDelete() {
    if (selected.size === 0) return
    if (!confirm(`确认批量删除选中的 ${selected.size} 个主播？（软删）`)) return
    setError('')
    try {
      await api('/api/admin/streamers/batch-delete', {
        method: 'POST',
        body: JSON.stringify({ ids: [...selected] }),
      })
      setSelected(new Set())
      load()
      loadDupes()
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  function toggleSel(personId: number) {
    const next = new Set(selected)
    if (next.has(personId)) next.delete(personId)
    else next.add(personId)
    setSelected(next)
  }

  /** 批量开号：给指定主播批量建展示端登录账号（有手机号的初始密码=手机号后四位，没有的用统一口令） */
  async function submitBatchAcc(ids: number[]) {
    if (!batchAcc || batchAccBusy || ids.length === 0) return
    setBatchAccBusy(true)
    setError('')
    setMsg('')
    try {
      const d = await api('/api/admin/my-accounts/batch', {
        method: 'POST',
        body: JSON.stringify({
          items: ids.map((personId) => ({ personId })),
          defaultPass: batchAcc.defaultPass.trim() || undefined,
          forceReset: batchAcc.forceReset,
        }),
      })
      setBatchAcc({ ...batchAcc, result: d })
      load()
    } catch (e: any) {
      setError(e.message ?? String(e))
    } finally {
      setBatchAccBusy(false)
    }
  }

  /** 复制初始密码（运营抄给主播用） */
  function copyPass(pass: string) {
    navigator.clipboard?.writeText(pass).then(
      () => {
        setMsg('已复制初始密码')
        setTimeout(() => setMsg(''), 2000)
      },
      () => setError('复制失败，请手动选中复制'),
    )
  }

  async function remove(personId: number, streamerName: string) {
    if (!confirm(`确认删除主播「${streamerName}」？（软删，历史数据保留）`)) return
    setError('')
    try {
      await api(`/api/admin/streamers/${personId}`, { method: 'DELETE' })
      load()
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  function openMasterEdit(s: any) {
    setMq('')
    setMasterEdit({ id: s.personId, name: s.name, masterId: s.masterId ?? null })
  }

  async function saveMaster(masterId: number | null) {
    if (!masterEdit || savingMaster) return
    if (masterId === masterEdit.masterId) {
      setMasterEdit(null)
      return
    }
    setSavingMaster(true)
    setError('')
    setMsg('')
    try {
      const r = await api<{ ok: boolean; masterName?: string | null }>(
        `/api/admin/streamers/${masterEdit.id}/master`,
        { method: 'PUT', body: JSON.stringify({ masterId }) },
      )
      setMsg(
        masterId == null
          ? `已清除「${masterEdit.name}」的师傅`
          : `已把「${masterEdit.name}」的师傅设为「${r.masterName ?? ''}」`,
      )
      setMasterEdit(null)
      load()
    } catch (e: any) {
      setError(e.message ?? String(e))
    } finally {
      setSavingMaster(false)
    }
  }

  return (
    <div className="page">
      <section className="card">
        <h2>新建主播</h2>
        <div className="row">
          <input placeholder="名字（必填）" value={name} onChange={(e) => setName(e.target.value)} />
          <select value={newTeam} onChange={(e) => setNewTeam(e.target.value)}>
            <option value="male">男团</option>
            <option value="female">女团</option>
          </select>
          <input
            placeholder="抖音号（全局唯一）"
            value={douyinNo}
            onChange={(e) => setDouyinNo(e.target.value)}
          />
          <button className="primary" disabled={!name.trim()} onClick={create}>
            建档
          </button>
        </div>
        {error && <div className="alert bad">{error}</div>}
        {msg && <div className="alert good">{msg}</div>}
      </section>

      {/* 导入暂存区：导入时有抖音号/ID、但台账还没有这个主播的行。
          它们不进台账 / 榜单 / 统计，只有在这里建档或绑定该抖音号之后才会补账显示。
          默认收起成一行（行数多时不占位），点「展开」才显示明细与操作。 */}
      {pending.length > 0 && (
        <section className="card">
          <div className="row" style={{ margin: 0, justifyContent: 'space-between' }}>
            <span className="hint" style={{ margin: 0 }}>
              导入暂存 <b>{pending.length}</b> 行未建档：不显示在台账、榜单和任何统计里，
              建档（填同样抖音号）后数据自动补账入库。
            </span>
            <button className="ghost" onClick={() => setPendingOpen((v) => !v)}>
              {pendingOpen ? '收起' : '展开处理'}
            </button>
          </div>
          {pendingOpen && (
            <div className="table-scroll" style={{ marginTop: 10 }}>
              <table>
                <thead>
                  <tr>
                    <th>名字</th>
                    <th>抖音号 / 采集 ID</th>
                    <th>业务日期</th>
                    <th>当日音浪</th>
                    <th>时长(分)</th>
                    <th>来源批次</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  {pending.map((p) => (
                    <tr key={p.id}>
                      <td>{p.name}</td>
                      <td className="hint">{p.douyinNo}</td>
                      <td className="hint">{p.bizDate ?? '—'}</td>
                      <td className="num">{p.wave ?? '—'}</td>
                      <td className="num">{p.minutes ?? '—'}</td>
                      <td className="hint">#{p.batchId ?? '—'}</td>
                      <td>
                        <button className="primary" onClick={() => prefillArchive(p)}>
                          建档
                        </button>
                        <button className="danger" style={{ marginLeft: 6 }} onClick={() => discardPending(p)}>
                          丢弃
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </section>
      )}

      <section className="card">
        <h2>主播台账（{items.length} 人）</h2>
        <p className="hint">
          <b>右键点任意一行</b>：编辑资料（名字 / 团别 / 抖音号 / 手机号）、设置师傅、查看多抖音号、
          设置「我的」页面账号（手机号 / 密码）、合并他人、删除。一个主播可以挂多个抖音号（详情里绑定），
          两个号其实是同一个人时用「合并」。
          <b>「对外」列</b>：取消勾选 = 该主播从对外展示的每日报告/月榜/年榜里消失（接口不下发，不占名次）。
        </p>
        <div className="row">
          <input placeholder="搜索名字…" value={q} onChange={(e) => setQ(e.target.value)} />
          <select value={team} onChange={(e) => setTeam(e.target.value)}>
            <option value="">全部团别</option>
            <option value="male">男团</option>
            <option value="female">女团</option>
          </select>
        </div>
        <div className="row">
          <button disabled={selected.size === 0} onClick={batchDelete}>
            批量删除（已选 {selected.size}）
          </button>
          <button
            disabled={selected.size === 0}
            onClick={() => setBatchAcc({ defaultPass: '', forceReset: false, result: null })}
          >
            批量开号（已选 {selected.size}）
          </button>
          {dupes && (dupes.sameName?.length > 0 || dupes.sameDouyinNo?.length > 0) && (
            <span className="alert bad" style={{ flex: 1 }}>
              ⚠️ 重复：同名 {dupes.sameName.length} 组 · 同抖音号 {dupes.sameDouyinNo.length} 组
              （{dupes.sameName.map((d: any) => d.name).join('、')}
              {dupes.sameDouyinNo.map((d: any) => d.douyinNo).join('、')}）
            </span>
          )}
        </div>
        {items.length === 0 ? (
          <div className="empty">还没有主播，先在上面建档或导入数据</div>
        ) : (
          <div className="table-scroll">
            <table style={{ minWidth: 1000 }}>
              <thead>
                <tr>
                  <th>姓名</th>
                  <th title="一个主播可挂多个抖音号；第一个是主号（SQL 按 is_primary DESC 排序）">抖音号</th>
                  <th title="主播手机号（展示端登录用）；已开号 = 已有登录账号">手机号</th>
                  <th title="微信推送机器人：未绑定 / 已扫码待激活 / 已绑定（显示微信昵称）">微信机器人</th>
                  <th>师傅</th>
                  <th>状态</th>
                  <th title="是否出现在对外展示的每日报告/月榜/年榜里">对外</th>
                  <th>别名</th>
                  <th></th>
              </tr>
            </thead>
            <tbody>
              {items.map((s) => (
                <tr
                  key={s.personId}
                  style={{ opacity: s.status === 'left' ? 0.55 : undefined }}
                  onContextMenu={(e) => {
                    e.preventDefault()
                    setCtxMenu({ x: e.clientX, y: e.clientY, person: s })
                  }}
                >
                  <td>
                    <span style={{ fontWeight: 600 }}>{s.name}</span>
                    <span
                      className={`badge ${s.team === 'female' ? 'b-female' : 'b-male'}`}
                      style={{ marginLeft: 6 }}
                    >
                      {s.team === 'female' ? '女团' : s.team === 'male' ? '男团' : s.team}
                    </span>
                    {s.status === 'left' && (
                      <span className="badge b-left" style={{ marginLeft: 6 }}>
                        已离开
                      </span>
                    )}
                    {(dupNames.get(String(s.name ?? '').trim()) ?? 0) > 1 && (
                      <span
                        className="badge"
                        style={{ marginLeft: 6, color: 'var(--bad)', background: 'var(--bad-bg)' }}
                        title={`「${s.name}」在列表里有重名：姓名要做唯一标识，请用右键「合并」或改名处理`}
                      >
                        重名
                      </span>
                    )}
                  </td>
                  {renderDouyinCell(s)}
                  <td>
                    {s.phone ? (
                      <>
                        {s.phone}
                        {s.hasAccount && (
                          <span
                            className="badge b-active"
                            style={{ marginLeft: 6 }}
                            title="已开展示端登录账号"
                          >
                            已开号
                          </span>
                        )}
                      </>
                    ) : (
                      <span className="hint">
                        未登记
                        {s.hasAccount && (
                          <span
                            className="badge b-paused"
                            style={{ marginLeft: 6 }}
                            title="已开号但没登记手机号：只能姓名登录，用不了「忘记密码」和微信绑定"
                          >
                            已开号
                          </span>
                        )}
                      </span>
                    )}
                  </td>
                  {renderWxbotCell(s)}
                  <td>
                    <button className="linkish" title="点击设置师傅" onClick={() => openMasterEdit(s)}>
                      {s.masterName ?? '—'}
                    </button>
                    {!!s.disciples && (
                      <span className="disciples" title={`徒弟 ${s.disciples} 人`}>
                        {s.disciples}徒
                      </span>
                    )}
                  </td>
                  <td>
                    <span
                      className={
                        s.status === 'left'
                          ? 'badge b-left'
                          : s.status === 'paused'
                            ? 'badge b-paused'
                            : 'badge b-active'
                      }
                    >
                      {s.status === 'left' ? '已离开' : s.status === 'paused' ? '暂停' : '在职'}
                    </span>
                  </td>
                  <td title={s.hideInDailyReport ? '已隐藏：对外榜单不下发此人' : '对外显示：出现在每日报告/月榜/年榜'}>
                    <input
                      type="checkbox"
                      checked={!s.hideInDailyReport}
                      onChange={() => toggleVisible(s)}
                    />
                  </td>
                  <td className="hint">{s.aliases?.join('、') || '—'}</td>
                  <td>
                    <input
                      type="checkbox"
                      checked={selected.has(s.personId)}
                      onChange={() => toggleSel(s.personId)}
                    />
                    <button style={{ marginLeft: 6 }} onClick={() => showDetail(s.personId)}>
                      账号
                    </button>
                    <button className="danger" onClick={() => remove(s.personId, s.name)}>
                      删除
                    </button>
                  </td>
                </tr>
              ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {detail && (
        <section className="card">
          <h2>
            {detail.name}（{detail.team === 'male' ? '男团' : '女团'}）· #{detail.personId}
            <button style={{ float: 'right' }} onClick={() => setDetail(null)}>
              关闭
            </button>
          </h2>
          <p className="hint">
            一个主播可以挂<b>多个抖音号</b>：导入时这些号的音浪会<b>合并</b>记到 TA 名下
            （当日音浪求和、累计取最大、时长求和）。抖音号全局唯一，已被别人占用的号绑不上。
          </p>
          <table>
            <thead>
              <tr>
                <th>抖音号</th>
                <th>采集 ID</th>
                <th title="导入时 daily_metric.anchor_id 回填用这个号">主号</th>
                <th>状态</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {(detail.accounts ?? []).map((a: any) => (
                <tr key={a.id}>
                  <td>{a.douyinNo || '—'}</td>
                  <td className="hint">{a.anchorId}</td>
                  <td>
                    {a.isPrimary ? (
                      <span className="badge b-active">主号</span>
                    ) : (
                      <button
                        className="linkish"
                        title="设为主号（导入回填 anchor_id 用它）"
                        onClick={() => setPrimary(a.id)}
                      >
                        设为主号
                      </button>
                    )}
                  </td>
                  <td>{a.status}</td>
                  <td>
                    <button
                      className="danger"
                      onClick={() => unbindAccount(a.id, a.douyinNo)}
                    >
                      解绑
                    </button>
                  </td>
                </tr>
              ))}
              {!(detail.accounts ?? []).length && (
                <tr>
                  <td colSpan={5} className="hint">
                    还没绑定抖音号
                  </td>
                </tr>
              )}
            </tbody>
          </table>
          <div className="row">
            <input
              placeholder="抖音号（必填，6~20 位字母数字_.）"
              value={bindDouyinNo}
              onChange={(e) => setBindDouyinNo(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && bindDouyinNo.trim() && bindAccount()}
            />
            <input
              placeholder="采集 ID（选填，没有就同抖音号）"
              value={bindAnchorId}
              onChange={(e) => setBindAnchorId(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && bindDouyinNo.trim() && bindAccount()}
            />
            <button
              className="primary"
              disabled={!bindDouyinNo.trim()}
              onClick={bindAccount}
            >
              {detail.accounts?.length ? '再加一个号' : '绑定为主号'}
            </button>
          </div>
        </section>
      )}

      {masterEdit && (
        <div
          className="mask"
          onClick={() => {
            if (!savingMaster) setMasterEdit(null)
          }}
        >
          <div className="master-pop" onClick={(e) => e.stopPropagation()}>
            <h2 style={{ marginBottom: 0 }}>
              设置师傅 · {masterEdit.name}
              <button className="linkish" style={{ float: 'right' }} onClick={() => setMasterEdit(null)}>
                关闭
              </button>
            </h2>
            <input
              autoFocus
              placeholder="搜索名字…"
              value={mq}
              onChange={(e) => setMq(e.target.value)}
            />
            {masterEdit.masterId != null && (
              <button disabled={savingMaster} onClick={() => saveMaster(null)}>
                清除师傅（当前：
                {items.find((i) => i.personId === masterEdit.masterId)?.name ??
                  `#${masterEdit.masterId}`}
                ）
              </button>
            )}
            <ul className="master-list">
              {items
                .filter(
                  (p) =>
                    p.personId !== masterEdit.id &&
                    (!mq.trim() || p.name.includes(mq.trim())),
                )
                .map((p) => (
                  <li key={p.personId}>
                    <button disabled={savingMaster} onClick={() => saveMaster(p.personId)}>
                      <b>{p.name}</b>
                      <span className="hint">
                        {p.team === 'male' ? '男团' : p.team === 'female' ? '女团' : p.team}
                        {p.personId === masterEdit.masterId ? ' · 当前' : ''}
                        {p.masterName ? ` · 师傅 ${p.masterName}` : ''}
                        {!!p.disciples && ` · ${p.disciples}徒`}
                      </span>
                    </button>
                  </li>
                ))}
              {items.filter(
                (p) =>
                  p.personId !== masterEdit.id && (!mq.trim() || p.name.includes(mq.trim())),
              ).length === 0 && <li className="hint" style={{ padding: 8 }}>没有匹配的主播</li>}
            </ul>
            <div className="hint">点选列表中的人设为师傅；输入名字可搜索。</div>
          </div>
        </div>
      )}

      {/* 表格行右键菜单 */}
      {ctxMenu && (
        <div
          className="ctx-menu"
          style={{ left: ctxMenu.x, top: ctxMenu.y }}
          onClick={(e) => e.stopPropagation()}
          onContextMenu={(e) => e.preventDefault()}
        >
          <button
            onClick={() => {
              const s = ctxMenu.person
              setEditPop({ personId: s.personId, name: s.name, team: s.team === 'female' ? 'female' : 'male', douyinNo: s.douyinNo ?? '', phone: s.phone ?? '' })
              setCtxMenu(null)
            }}
          >
            ✏️ 编辑资料（名字 / 团别 / 抖音号 / 手机号）
          </button>
          <button
            onClick={() => {
              openMasterEdit(ctxMenu.person)
              setCtxMenu(null)
            }}
          >
            🧑‍🏫 设置师傅
          </button>
          <button
            onClick={() => {
              showDetail(ctxMenu.person.personId)
              setCtxMenu(null)
            }}
          >
            📱 查看多抖音号（绑定账号）
          </button>
          <button
            onClick={() => {
              openMyAccount(ctxMenu.person.name)
              setCtxMenu(null)
            }}
          >
            🔐 我的页面账号（手机号 / 密码）
          </button>
          <button
            onClick={() => {
              setMergePop({ targetId: ctxMenu.person.personId, targetName: ctxMenu.person.name, sourceId: null, q: '' })
              setCtxMenu(null)
            }}
          >
            🔀 合并：把别的主播并进「{ctxMenu.person.name}」
          </button>
          <button
            className="danger"
            onClick={() => {
              remove(ctxMenu.person.personId, ctxMenu.person.name)
              setCtxMenu(null)
            }}
          >
            🗑 删除（软删）
          </button>
        </div>
      )}

      {/* 编辑资料弹窗 */}
      {editPop && (
        <div className="mask" onClick={() => !savingEdit && setEditPop(null)}>
          <div className="master-pop" onClick={(e) => e.stopPropagation()}>
            <h2 style={{ marginBottom: 0 }}>
              编辑资料 · #{editPop.personId}
              <button className="linkish" style={{ float: 'right' }} onClick={() => setEditPop(null)}>
                关闭
              </button>
            </h2>
            <label className="hint" style={{ display: 'block', marginTop: 8 }}>
              名字
            </label>
            <input
              autoFocus
              value={editPop.name}
              onChange={(e) => setEditPop({ ...editPop, name: e.target.value })}
            />
            <label className="hint" style={{ display: 'block', marginTop: 8 }}>
              团别
            </label>
            <select value={editPop.team} onChange={(e) => setEditPop({ ...editPop, team: e.target.value })}>
              <option value="male">男团</option>
              <option value="female">女团</option>
            </select>
            <label className="hint" style={{ display: 'block', marginTop: 8 }}>
              主抖音号（其他抖音号在「查看多抖音号」里绑定）
            </label>
            <input
              placeholder="抖音号；留空 = 不修改"
              value={editPop.douyinNo}
              onChange={(e) => setEditPop({ ...editPop, douyinNo: e.target.value })}
            />
            <label className="hint" style={{ display: 'block', marginTop: 8 }}>
              手机号（展示端登录与批量开号用；清空 = 取消登记）
            </label>
            <input
              inputMode="numeric"
              placeholder="未登记"
              value={editPop.phone}
              onChange={(e) => setEditPop({ ...editPop, phone: e.target.value })}
            />
            <div className="row" style={{ marginTop: 10 }}>
              <button className="primary" disabled={savingEdit || !editPop.name.trim()} onClick={saveEdit}>
                {savingEdit ? '保存中…' : '保存'}
              </button>
              <button disabled={savingEdit} onClick={() => setEditPop(null)}>
                取消
              </button>
            </div>
          </div>
        </div>
      )}

      {/* 「我的」页面账号弹窗（右键主播 → 设置手机号/密码） */}
      {myAccPop && (
        <div className="mask" onClick={() => setMyAccPop(null)}>
          <div className="master-pop" onClick={(e) => e.stopPropagation()}>
            <h2 style={{ marginBottom: 0 }}>
              我的页面账号 · {myAccPop.name}
              <button className="linkish" style={{ float: 'right' }} onClick={() => setMyAccPop(null)}>
                关闭
              </button>
            </h2>
            <label className="hint" style={{ display: 'block', marginTop: 8 }}>手机号（选填，纯数字）</label>
            <input
              value={myAccPop.phone}
              inputMode="numeric"
              placeholder="未设置"
              onChange={(e) => setMyAccPop({ ...myAccPop, phone: e.target.value })}
            />
            <label className="hint" style={{ display: 'block', marginTop: 8 }}>
              {myAccPop.exists ? '新密码（纯数字，4~20 位；留空 = 不修改）' : '密码（纯数字，4~20 位）'}
            </label>
            <input
              value={myAccPop.pass}
              inputMode="numeric"
              placeholder="4~20 位数字"
              onChange={(e) => setMyAccPop({ ...myAccPop, pass: e.target.value })}
            />
            <div className="hint" style={{ marginTop: 8 }}>
              保存后主播在展示端「我的」页用 姓名 + 密码 登录看自己的数据；没有注册，账号只在这里设置。
              {myAccPop.exists ? ' 该主播已有账号：新密码留空则保持原密码，填写后覆盖更新。' : ''}
            </div>
            <div className="row" style={{ marginTop: 10 }}>
              {myAccPop.exists && (
                <button className="danger" onClick={removeMyAccount}>
                  删除账号
                </button>
              )}
              <button className="primary" onClick={saveMyAccount}>
                保存
              </button>
              <button onClick={() => setMyAccPop(null)}>取消</button>
            </div>
          </div>
        </div>
      )}

      {/* 合并弹窗：选一个「被并入」的主播 */}
      {mergePop && (
        <div className="mask" onClick={() => !merging && setMergePop(null)}>
          <div className="master-pop" onClick={(e) => e.stopPropagation()}>
            <h2 style={{ marginBottom: 0 }}>
              合并到「{mergePop.targetName}」
              <button className="linkish" style={{ float: 'right' }} onClick={() => setMergePop(null)}>
                关闭
              </button>
            </h2>
            <p className="hint">
              一个人有两个 / 三个抖音号、被建档成多条记录时：在这里选出<b>多余的记录</b>，
              它的账号、别名和全部历史数据会并进「{mergePop.targetName}」，然后被软删。
            </p>
            <input
              autoFocus
              placeholder="搜索要合并掉的主播…"
              value={mergePop.q}
              onChange={(e) => setMergePop({ ...mergePop, q: e.target.value })}
            />
            <ul className="master-list">
              {items
                .filter(
                  (p) =>
                    p.personId !== mergePop.targetId &&
                    (!mergePop.q.trim() || p.name.includes(mergePop.q.trim())),
                )
                .map((p) => (
                  <li key={p.personId}>
                    <button
                      className={mergePop.sourceId === p.personId ? 'sel' : ''}
                      disabled={merging}
                      onClick={() => setMergePop({ ...mergePop, sourceId: p.personId })}
                    >
                      <b>{p.name}</b>
                      <span className="hint">
                        {p.team === 'male' ? '男团' : p.team === 'female' ? '女团' : p.team}
                        {p.douyinNos || p.douyinNo ? ` · ${p.douyinNos || p.douyinNo}` : ''}
                      </span>
                    </button>
                  </li>
                ))}
              {items.filter(
                (p) =>
                  p.personId !== mergePop.targetId &&
                  (!mergePop.q.trim() || p.name.includes(mergePop.q.trim())),
              ).length === 0 && (
                <li className="hint" style={{ padding: 8 }}>
                  没有匹配的主播
                </li>
              )}
            </ul>
            <div className="row" style={{ marginTop: 10 }}>
              <button className="primary" disabled={!mergePop.sourceId || merging} onClick={doMerge}>
                {merging
                  ? '合并中…'
                  : mergePop.sourceId
                    ? `确认把「${items.find((i) => i.personId === mergePop.sourceId)?.name ?? ''}」并进去`
                    : '先在上面选一个要合并掉的主播'}
              </button>
              <button disabled={merging} onClick={() => setMergePop(null)}>
                取消
              </button>
            </div>
          </div>
        </div>
      )}

      {/* 批量开号弹窗：给勾选的主播批量建展示端登录账号，结果表里的初始密码要抄给主播 */}
      {batchAcc && (
        <div className="mask" onClick={() => !batchAccBusy && setBatchAcc(null)}>
          <div className="master-pop" onClick={(e) => e.stopPropagation()}>
            <h2 style={{ marginBottom: 0 }}>
              批量开号 · 已选 {selected.size} 人
              <button className="linkish" style={{ float: 'right' }} onClick={() => setBatchAcc(null)}>
                关闭
              </button>
            </h2>
            {!batchAcc.result ? (
              <>
                <label className="hint" style={{ display: 'block', marginTop: 8 }}>
                  统一初始口令（选填）
                </label>
                <input
                  autoFocus
                  inputMode="numeric"
                  placeholder="无手机号的主播用此口令；有手机号的初始密码 = 手机号后四位"
                  value={batchAcc.defaultPass}
                  onChange={(e) => setBatchAcc({ ...batchAcc, defaultPass: e.target.value })}
                />
                <label className="hint" style={{ display: 'block', marginTop: 8 }}>
                  <input
                    type="checkbox"
                    checked={batchAcc.forceReset}
                    onChange={(e) => setBatchAcc({ ...batchAcc, forceReset: e.target.checked })}
                  />{' '}
                  同时重置已有账号的密码
                </label>
                <div className="row" style={{ marginTop: 10 }}>
                  <button
                    className="primary"
                    disabled={batchAccBusy}
                    onClick={() => submitBatchAcc([...selected])}
                  >
                    {batchAccBusy ? '开号中…' : `确认开号（${selected.size} 人）`}
                  </button>
                  <button disabled={batchAccBusy} onClick={() => setBatchAcc(null)}>
                    取消
                  </button>
                </div>
              </>
            ) : (
              <>
                <p className="hint" style={{ marginTop: 8 }}>
                  结果：新开 {batchAcc.result.created ?? 0} · 更新 {batchAcc.result.updated ?? 0} ·
                  跳过 {batchAcc.result.skipped ?? 0} · 失败 {batchAcc.result.failed ?? 0}。
                  初始密码只在这里显示一次，请复制抄给主播。
                </p>
                <div className="table-scroll">
                  <table>
                    <thead>
                      <tr>
                        <th>姓名</th>
                        <th>手机号</th>
                        <th>状态</th>
                        <th>原因</th>
                        <th>初始密码</th>
                      </tr>
                    </thead>
                    <tbody>
                      {(batchAcc.result.items ?? []).map((r: any) => (
                        <tr key={r.personId}>
                          <td>{r.name}</td>
                          <td className="hint">{r.phone || '—'}</td>
                          <td>
                            <span
                              className={
                                r.status === 'created'
                                  ? 'badge b-active'
                                  : r.status === 'updated'
                                    ? 'badge b-paused'
                                    : r.status === 'skipped'
                                      ? 'badge'
                                      : 'badge b-left'
                              }
                            >
                              {r.status === 'created'
                                ? '新开'
                                : r.status === 'updated'
                                  ? '更新'
                                  : r.status === 'skipped'
                                    ? '跳过'
                                    : '失败'}
                            </span>
                          </td>
                          <td className="hint">{r.note || '—'}</td>
                          <td>
                            {r.initialPass ? (
                              <>
                                {r.initialPass}
                                <button
                                  className="linkish"
                                  style={{ marginLeft: 6 }}
                                  onClick={() => copyPass(r.initialPass)}
                                >
                                  复制
                                </button>
                              </>
                            ) : (
                              '—'
                            )}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
                <div className="row" style={{ marginTop: 10 }}>
                  {(batchAcc.result.failed ?? 0) > 0 && (
                    <button
                      disabled={batchAccBusy}
                      onClick={() =>
                        submitBatchAcc(
                          (batchAcc.result.items ?? [])
                            .filter((r: any) => r.status === 'failed')
                            .map((r: any) => r.personId),
                        )
                      }
                    >
                      {batchAccBusy ? '重试中…' : `只重试失败项（${batchAcc.result.failed}）`}
                    </button>
                  )}
                  <button className="primary" disabled={batchAccBusy} onClick={() => setBatchAcc(null)}>
                    完成
                  </button>
                </div>
              </>
            )}
          </div>
        </div>
      )}
    </div>
  )
}
