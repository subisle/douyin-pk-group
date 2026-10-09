import { useCallback, useEffect, useState } from 'react'
import { api } from './api'
import { useAutoRefresh } from './useAutoRefresh'

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
  /** 编辑资料弹窗（名字 / 团别 / 主抖音号） */
  const [editPop, setEditPop] = useState<{ personId: number; name: string; team: string; douyinNo: string } | null>(null)
  const [savingEdit, setSavingEdit] = useState(false)
  /** 合并弹窗：把「别人」并进 target（保留 target，账号与历史数据都归到 TA 名下） */
  const [mergePop, setMergePop] = useState<{ targetId: number; targetName: string; sourceId: number | null; q: string } | null>(null)
  const [merging, setMerging] = useState(false)
  /** 「我的」页面账号弹窗（右键主播 → 设置手机号/密码；主播用 姓名+密码 登录） */
  const [myAccPop, setMyAccPop] = useState<{ name: string; phone: string; pass: string; exists: boolean } | null>(null)

  const load = useCallback(() => {
    const qs = new URLSearchParams()
    if (q) qs.set('q', q)
    if (team) qs.set('team', team)
    api(`/api/admin/streamers?${qs}`)
      .then((d) => setItems(d.items ?? []))
      .catch((e) => setError(e.message ?? String(e)))
  }, [q, team])

  useEffect(load, [load])

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
      await api('/api/admin/streamers', {
        method: 'POST',
        body: JSON.stringify({ name, team: newTeam, douyinNo: douyinNo || undefined }),
      })
      setMsg(`已建档：${name}`)
      setName('')
      setDouyinNo('')
      load()
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
        pass: d.found ? d.pass ?? '' : '',
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
          pass: myAccPop.pass.trim(),
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

  /** 编辑资料：改名 / 团别 / 主抖音号（PUT /api/admin/streamers/{id}） */
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

  async function bindAccount() {
    if (!detail) return
    setError('')
    try {
      await api(`/api/admin/streamers/${detail.personId}/accounts`, {
        method: 'POST',
        body: JSON.stringify({
          anchorId: bindAnchorId,
          douyinNo: bindDouyinNo || undefined,
          isPrimary: true,
        }),
      })
      setBindAnchorId('')
      setBindDouyinNo('')
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

      <section className="card">
        <h2>主播台账（{items.length} 人）</h2>
        <p className="hint">
          <b>右键点任意一行</b>：编辑资料（名字 / 团别 / 抖音号）、设置师傅、查看多抖音号、
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
          <table>
            <thead>
              <tr>
                <th>名字</th>
                <th>团别</th>
                <th>师傅</th>
                <th>抖音号</th>
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
                  <td>{s.name}</td>
                  <td>
                    <span className={`badge ${s.team === 'female' ? 'b-female' : 'b-male'}`}>
                      {s.team === 'female' ? '女团' : s.team === 'male' ? '男团' : s.team}
                    </span>
                  </td>
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
                  <td className="hint" title="主号在前；详情可绑定多账号">
                    {s.douyinNos ?? s.douyinNo ?? '—'}
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
          <table>
            <thead>
              <tr>
                <th>采集 ID</th>
                <th>抖音号</th>
                <th>主号</th>
                <th>状态</th>
              </tr>
            </thead>
            <tbody>
              {(detail.accounts ?? []).map((a: any) => (
                <tr key={a.id}>
                  <td>{a.anchorId}</td>
                  <td>{a.douyinNo}</td>
                  <td>{a.isPrimary ? '✓' : ''}</td>
                  <td>{a.status}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <div className="row">
            <input
              placeholder="采集 ID（anchorId，必填）"
              value={bindAnchorId}
              onChange={(e) => setBindAnchorId(e.target.value)}
            />
            <input
              placeholder="抖音号（可选）"
              value={bindDouyinNo}
              onChange={(e) => setBindDouyinNo(e.target.value)}
            />
            <button disabled={!bindAnchorId.trim()} onClick={bindAccount}>
              绑定为 {detail.accounts?.length ? '账号' : '主号'}
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
              setEditPop({ personId: s.personId, name: s.name, team: s.team === 'female' ? 'female' : 'male', douyinNo: s.douyinNo ?? '' })
              setCtxMenu(null)
            }}
          >
            ✏️ 编辑资料（名字 / 团别 / 抖音号）
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
            <label className="hint" style={{ display: 'block', marginTop: 8 }}>密码（纯数字，4~20 位）</label>
            <input
              value={myAccPop.pass}
              inputMode="numeric"
              placeholder="4~20 位数字"
              onChange={(e) => setMyAccPop({ ...myAccPop, pass: e.target.value })}
            />
            <div className="hint" style={{ marginTop: 8 }}>
              保存后主播在展示端「我的」页用 姓名 + 密码 登录看自己的数据；没有注册，账号只在这里设置。
              {myAccPop.exists ? ' 该主播已有账号，保存=覆盖更新。' : ''}
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
    </div>
  )
}
