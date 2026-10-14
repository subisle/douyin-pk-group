/* 消息下发页（/admin/send，docs/23 §5/§6）：
 * - 编辑卡片 + 收件人三种圈人（多选 / 按师傅 / 批量）+ 右侧手机弹窗实时预览；
 * - 发送确认（N>100 二次确认）→ 跳转发送详情（3s 轮询直到无 pending）；
 * - 发送历史列表。
 * 防注入：标题/正文一律 React 文本节点渲染（预览、详情、历史都一样），全页没有
 * dangerouslySetInnerHTML，也不引外部 HTML。
 * 样式只复用 styles.css 既有类，页面私有的布局类全部 send- 前缀，写在本文件
 * 底部的局部 <style> 里，不动全局 styles.css。 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  createSend,
  fetchSendMessageDetail,
  fetchSendMessages,
  fetchSendTargets,
  postEnableLineage,
  type SendChannel,
  type SendMessage,
  type SendMessageDetail,
  type SendOutboxRow,
  type SendTarget,
} from './api'
import { isoLocal } from './lib/date'

/** outbox 四态 → 徽标（配色沿用 styles.css 的 badge 系列） */
const OUTBOX_STATUS: Record<SendOutboxRow['status'], { label: string; cls: string }> = {
  pending: { label: '待发', cls: 'badge b-paused' },
  sent: { label: '已发', cls: 'badge b-active' },
  failed: { label: '失败', cls: 'badge b-left' },
  skipped: { label: '跳过', cls: 'badge' },
}

/** 圈人意图 → 中文（历史列表展示用） */
const TARGET_LABEL: Record<string, string> = {
  persons: '多选',
  masters: '按师傅',
  team: '按团',
  all: '全部',
}

/** 图片类型 → 中文（detail 里可能看到后端扩展的 daily/month，一并兜住） */
const IMAGE_LABEL: Record<string, string> = {
  none: '无图',
  lineage: '师门日报',
  daily: '每日报告',
  month: '月榜',
}

const CHANNEL_LABEL: Record<string, string> = {
  wxbot: 'wxbot 推送',
  popup: 'web 弹窗',
}

function teamLabel(team: string): string {
  if (team === 'male') return '男团'
  if (team === 'female') return '女团'
  return team || '—'
}

/** 2026-10-14T18:30:05Z → 2026-10-14 18:30:05（后端 ISO 串直接裁，不引入时区换算） */
function fmtTime(v: string | null | undefined): string {
  if (!v) return '—'
  return String(v).replace('T', ' ').slice(0, 19)
}

/** 渠道串（后端逗号分隔，容忍数组返回）→ ['wxbot','popup'] */
function parseChannels(ch: string | string[]): string[] {
  const raw = Array.isArray(ch) ? ch : String(ch).split(',')
  return raw.map((x) => x.trim()).filter(Boolean)
}

/** 历史消息的渠道列展示 */
function channelText(ch: string | string[]): string {
  const list = parseChannels(ch)
  return list.length ? list.map((c) => CHANNEL_LABEL[c] ?? c).join(' + ') : '—'
}

/** ✓ / ✗ 单元格：绑定状态一眼可扫 */
function Mark({ ok, tip }: { ok: boolean; tip: string }) {
  return (
    <span title={tip} className={ok ? 'ok' : 'hint'}>
      {ok ? '✓' : '✗'}
    </span>
  )
}

type View = { kind: 'compose' } | { kind: 'detail'; id: number }

export default function SendPage() {
  const [view, setView] = useState<View>({ kind: 'compose' })

  /* ---------------- 编辑区 ---------------- */
  const [title, setTitle] = useState('')
  const [content, setContent] = useState('')
  const [imageKind, setImageKind] = useState<'none' | 'lineage'>('none')
  const [imageDate, setImageDate] = useState(isoLocal())
  const [wxChk, setWxChk] = useState(true)
  const [popupChk, setPopupChk] = useState(true)

  /* ---------------- 收件人 ---------------- */
  const [tab, setTab] = useState<'multi' | 'masters' | 'batch'>('multi')
  const [q, setQ] = useState('')
  const [hits, setHits] = useState<SendTarget[]>([])
  const [searching, setSearching] = useState(false)
  const [searchErr, setSearchErr] = useState('')
  /** 全量名单（q 为空拉一次）：展开徒弟 / 「加上其徒弟」/ 批量人数估算都用它 */
  const [full, setFull] = useState<SendTarget[]>([])
  const [fullErr, setFullErr] = useState('')
  const [selPersons, setSelPersons] = useState<Map<number, SendTarget>>(new Map())
  const [selMasters, setSelMasters] = useState<Map<number, SendTarget>>(new Map())
  const [includeMaster, setIncludeMaster] = useState(true)
  const [batch, setBatch] = useState<'all' | 'male' | 'female'>('all')
  const [expanded, setExpanded] = useState<number | null>(null)

  /* ---------------- 动作 ---------------- */
  const [enableMsg, setEnableMsg] = useState('')
  const [pageErr, setPageErr] = useState('')
  const [sending, setSending] = useState(false)
  /** 发送确认弹窗：stage 1 = 常规确认；stage 2 = 人数超 100 的二次确认 */
  const [confirmBox, setConfirmBox] = useState<{ stage: 1 | 2; n: number; m: number } | null>(null)

  /* ---------------- 历史 ---------------- */
  const [history, setHistory] = useState<SendMessage[]>([])
  const [historyErr, setHistoryErr] = useState('')

  const loadHistory = useCallback(() => {
    fetchSendMessages(50)
      .then((d) => {
        setHistory(d.items ?? [])
        setHistoryErr('')
      })
      .catch((e: unknown) => setHistoryErr(e instanceof Error ? e.message : String(e)))
  }, [])

  useEffect(() => {
    loadHistory()
  }, [loadHistory])

  /** 全量名单：展开徒弟、批量人数、搜索兜底都靠它；后端没就绪时只提示，不挡编辑 */
  useEffect(() => {
    fetchSendTargets('')
      .then((d) => {
        setFull(d.items ?? [])
        setFullErr('')
      })
      .catch((e: unknown) => setFullErr(e instanceof Error ? e.message : String(e)))
  }, [])

  /* 搜索防抖 300ms（docs/23 §5.5 targets?q=）；seq 防竞态：慢的旧响应不覆盖新结果 */
  const seqRef = useRef(0)
  useEffect(() => {
    const t = window.setTimeout(async () => {
      const seq = ++seqRef.current
      setSearching(true)
      try {
        const d = await fetchSendTargets(q)
        if (seqRef.current === seq) {
          setHits(d.items ?? [])
          setSearchErr('')
        }
      } catch (e: unknown) {
        if (seqRef.current === seq) {
          setHits([])
          setSearchErr(e instanceof Error ? e.message : String(e))
        }
      } finally {
        if (seqRef.current === seq) setSearching(false)
      }
    }, 300)
    return () => window.clearTimeout(t)
  }, [q])

  const channels: SendChannel[] = useMemo(() => {
    const list: SendChannel[] = []
    if (wxChk) list.push('wxbot')
    if (popupChk) list.push('popup')
    return list
  }, [wxChk, popupChk])

  /** 展开某师傅的直系徒弟（全量名单口径，服务端最终权威） */
  const disciplesOf = useCallback(
    (masterId: number): SendTarget[] => full.filter((t) => t.masterId === masterId),
    [full],
  )

  /** 收件人数估算（确认弹窗与按钮旁提示；服务端展开才是最终数字） */
  const estimatedN = useMemo(() => {
    if (tab === 'multi') return selPersons.size
    if (tab === 'masters') {
      let n = 0
      for (const t of selMasters.values()) n += t.discipleCount + (includeMaster ? 1 : 0)
      return n
    }
    if (batch === 'all') return full.length
    return full.filter((t) => t.team === batch).length
  }, [tab, selPersons, selMasters, includeMaster, batch, full])

  const canSend =
    !!title.trim() && !!content.trim() && channels.length > 0 && estimatedN > 0 && !sending

  /** 组装 target（前端只传意图，docs/23 §5.2：展开与校验在服务端） */
  function buildTarget() {
    if (tab === 'multi') return { type: 'persons' as const, ids: [...selPersons.keys()] }
    if (tab === 'masters')
      return {
        type: 'masters' as const,
        ids: [...selMasters.keys()],
        includeMaster,
      }
    if (batch === 'all') return { type: 'all' as const }
    return { type: 'team' as const, team: batch }
  }

  function openSendConfirm() {
    if (!canSend) return
    setConfirmBox({ stage: 1, n: estimatedN, m: channels.length })
  }

  async function doSend() {
    if (sending) return
    setSending(true)
    setPageErr('')
    try {
      const d = await createSend({
        title: title.trim(),
        content,
        imageKind,
        imageDate,
        channels,
        target: buildTarget(),
      })
      setConfirmBox(null)
      setView({ kind: 'detail', id: d.messageId })
      loadHistory()
    } catch (e: unknown) {
      setPageErr(e instanceof Error ? e.message : String(e))
      setConfirmBox(null)
    } finally {
      setSending(false)
    }
  }

  async function enableLineage() {
    if (
      !window.confirm(
        '将为所有有徒弟的师傅开启「师门日报」自动推送（已开启的保持不变）。\n确定继续？',
      )
    )
      return
    setPageErr('')
    try {
      const d = await postEnableLineage()
      setEnableMsg(`已为 ${d.enabled} 位师傅开启师门日报`)
    } catch (e: unknown) {
      setPageErr(e instanceof Error ? e.message : String(e))
    }
  }

  /* ---------------- 勾选辅助 ---------------- */
  function togglePerson(t: SendTarget) {
    setSelPersons((prev) => {
      const next = new Map(prev)
      if (next.has(t.personId)) next.delete(t.personId)
      else next.set(t.personId, t)
      return next
    })
  }

  function toggleMaster(t: SendTarget) {
    setSelMasters((prev) => {
      const next = new Map(prev)
      if (next.has(t.personId)) next.delete(t.personId)
      else next.set(t.personId, t)
      return next
    })
  }

  /** 加上其徒弟：师傅本人 + 全部直系徒弟并入多选（去重由 Map 天然保证） */
  function addMasterWithDisciples(master: SendTarget) {
    setSelPersons((prev) => {
      const next = new Map(prev)
      next.set(master.personId, master)
      for (const d of disciplesOf(master.personId)) next.set(d.personId, d)
      return next
    })
  }

  /** 结果表当前行是否已按当前 Tab 的口径勾上 */
  function isPicked(t: SendTarget): boolean {
    return tab === 'masters' ? selMasters.has(t.personId) : selPersons.has(t.personId)
  }

  function togglePick(t: SendTarget) {
    if (tab === 'masters') toggleMaster(t)
    else togglePerson(t)
  }

  function clearPicks() {
    if (tab === 'masters') setSelMasters(new Map())
    else {
      setSelPersons(new Map())
      setExpanded(null)
    }
  }

  const pickedMap = tab === 'masters' ? selMasters : selPersons

  /* ---------------- 渲染 ---------------- */
  return (
    <div className="page">
      <style>{SEND_CSS}</style>

      {pageErr && <div className="alert bad">{pageErr}</div>}

      {/* 顶部说明条 */}
      <section className="card">
        <div className="row" style={{ margin: 0 }}>
          <span style={{ flex: 1, minWidth: 260 }}>
            <b>消息下发</b>
            <span className="hint" style={{ marginLeft: 8 }}>
              管理员手动给主播发文字 / 师门日报图（微信机器人私聊 + 展示端「我的」页弹窗），
              与每日自动推送互不相干，不会顶掉自动推送的去重。
            </span>
          </span>
          <button onClick={enableLineage}>为所有师傅开启师门日报</button>
        </div>
        {enableMsg && <div className="alert good">{enableMsg}</div>}
      </section>

      {view.kind === 'detail' ? (
        <DetailCard id={view.id} onBack={() => setView({ kind: 'compose' })} />
      ) : (
        <div className="send-cols">
          <div className="send-sticky">
            {/* 编辑卡片 */}
            <section className="card">
              <h2>编辑消息</h2>
              <div className="row">
                <input
                  style={{ flex: 1, minWidth: 220 }}
                  placeholder="弹窗标题（必填）"
                  maxLength={64}
                  value={title}
                  onChange={(e) => setTitle(e.target.value.slice(0, 64))}
                />
                <span className="hint">{title.length}/64</span>
              </div>
              <div className="row" style={{ alignItems: 'stretch' }}>
                <textarea
                  rows={5}
                  placeholder="正文（必填，纯文本，收件侧按 textContent 渲染）"
                  maxLength={1000}
                  value={content}
                  onChange={(e) => setContent(e.target.value.slice(0, 1000))}
                />
                <span className="hint" style={{ alignSelf: 'flex-end' }}>
                  {content.length}/1000
                </span>
              </div>
              <div className="row">
                <label>
                  图片类型{' '}
                  <select
                    value={imageKind}
                    onChange={(e) => setImageKind(e.target.value === 'lineage' ? 'lineage' : 'none')}
                  >
                    <option value="none">无</option>
                    <option value="lineage">师门日报</option>
                  </select>
                </label>
                <label>
                  图片日期{' '}
                  <input
                    type="date"
                    value={imageDate}
                    onChange={(e) => setImageDate(e.target.value)}
                  />
                </label>
                <label title="走微信机器人私聊推送（未绑定的收件人记跳过）">
                  <input
                    type="checkbox"
                    checked={wxChk}
                    onChange={(e) => setWxChk(e.target.checked)}
                  />{' '}
                  wxbot 推送
                </label>
                <label title="走展示端「我的」页弹窗（未开通账号的收件人记跳过）">
                  <input
                    type="checkbox"
                    checked={popupChk}
                    onChange={(e) => setPopupChk(e.target.checked)}
                  />{' '}
                  web 弹窗
                </label>
              </div>
              {imageKind === 'lineage' && (
                <p className="hint">
                  师门日报图按每位收件人实时渲染（水印 = 本人姓名）；无徒弟的收件人自动降级纯文本。
                </p>
              )}
              <div className="row" style={{ marginBottom: 0 }}>
                <button className="primary" disabled={!canSend} onClick={openSendConfirm}>
                  {sending ? '发送中…' : '发送'}
                </button>
                <span className="hint">
                  将发送给 <b className="num">{estimatedN}</b> 人 × 渠道{' '}
                  <b className="num">{channels.length}</b>（人数为前端估算，最终以服务端展开为准；
                  展开后超 500 人会被拒绝）
                </span>
              </div>
            </section>

            {/* 收件人卡片 */}
            <section className="card">
              <h2>
                收件人
                <span className="send-count" style={{ marginLeft: 8 }}>
                  已选 {pickedMap.size}
                </span>
                {tab !== 'batch' && pickedMap.size > 0 && (
                  <button className="linkish" style={{ marginLeft: 10 }} onClick={clearPicks}>
                    清空
                  </button>
                )}
              </h2>

              <div className="send-tabs">
                {(
                  [
                    ['multi', '多选'],
                    ['masters', '按师傅'],
                    ['batch', '批量'],
                  ] as const
                ).map(([k, label]) => (
                  <button
                    key={k}
                    className={tab === k ? 'on' : ''}
                    onClick={() => setTab(k)}
                    type="button"
                  >
                    {label}
                  </button>
                ))}
              </div>

              {tab === 'multi' && (
                <p className="hint">
                  勾选具体收件人；师傅行可「加上其徒弟」一键并入本人 + 全部直系徒弟，
                  或点行展开徒弟列表快捷勾选。
                </p>
              )}
              {tab === 'masters' && (
                <>
                  <p className="hint">
                    勾选若干师傅：发送时服务端展开为「每位师傅的直系徒弟
                    {includeMaster ? ' + 师傅本人' : ''}」。
                  </p>
                  <label className="hint" style={{ display: 'block', marginBottom: 4 }}>
                    <input
                      type="checkbox"
                      checked={includeMaster}
                      onChange={(e) => setIncludeMaster(e.target.checked)}
                    />{' '}
                    includeMaster：师傅本人也收一份（默认开）
                  </label>
                </>
              )}
              {tab === 'batch' && (
                <p className="hint">
                  按团批量圈人（服务端按「全部已绑定用户 / 在职团员的口径」展开）。
                </p>
              )}

              {tab !== 'batch' ? (
                <>
                  <div className="row">
                    <input
                      style={{ flex: 1, minWidth: 200 }}
                      placeholder="搜索名字…（300ms 防抖）"
                      value={q}
                      onChange={(e) => setQ(e.target.value)}
                    />
                    {searching && <span className="hint">搜索中…</span>}
                  </div>
                  {searchErr && <div className="alert warn">搜索失败：{searchErr}</div>}
                  <TargetTable
                    hits={hits}
                    tab={tab}
                    isPicked={isPicked}
                    onToggle={togglePick}
                    expanded={expanded}
                    onExpand={(pid) => setExpanded((cur) => (cur === pid ? null : pid))}
                    disciplesOf={disciplesOf}
                    fullReady={full.length > 0}
                    onAddDisciples={addMasterWithDisciples}
                  />
                </>
              ) : (
                <div className="row" style={{ flexDirection: 'column', alignItems: 'stretch', gap: 6 }}>
                  {(
                    [
                      ['all', `全部（约 ${full.length} 人）`],
                      ['male', `男团（约 ${full.filter((t) => t.team === 'male').length} 人）`],
                      ['female', `女团（约 ${full.filter((t) => t.team === 'female').length} 人）`],
                    ] as const
                  ).map(([k, label]) => (
                    <label key={k} style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                      <input
                        type="radio"
                        name="send-batch"
                        checked={batch === k}
                        onChange={() => setBatch(k)}
                      />
                      {label}
                    </label>
                  ))}
                  {fullErr && (
                    <div className="alert warn">名单加载失败（人数显示不出）：{fullErr}</div>
                  )}
                </div>
              )}

              {/* 已选明细（批量页无勾选明细） */}
              {tab !== 'batch' && pickedMap.size > 0 && (
                <div className="send-chips">
                  {[...pickedMap.values()].map((t) => (
                    <span key={t.personId} className="send-chip">
                      {t.name}
                      <button
                        type="button"
                        title="移出选择"
                        onClick={() => togglePick(t)}
                      >
                        ×
                      </button>
                    </span>
                  ))}
                </div>
              )}
            </section>
          </div>

          {/* 右侧：实时预览（宽屏并排、窄屏堆叠） */}
          <div className="send-sticky">
            <section className="card">
              <h2>弹窗预览</h2>
              <p className="hint">收件人在展示端「我的」页看到的样式（随编辑区即时变化）。</p>
              <div className="send-phone">
                <div className="send-screen">
                  <div className="send-screen-bar">展示端 · 我的</div>
                  <div className="send-popup">
                    <div className="send-popup-bar">📣 管理员消息</div>
                    <div className="send-popup-title">{title.trim() || '（标题）'}</div>
                    {imageKind !== 'none' && (
                      <div className="send-popup-img">
                        🖼 {IMAGE_LABEL[imageKind] ?? imageKind} · {imageDate || '（日期）'}
                        <div style={{ marginTop: 4 }}>（按收件人实时渲染）</div>
                      </div>
                    )}
                    <div className="send-popup-text">
                      {content || '（正文预览：在这里输入要下发的文字）'}
                    </div>
                    <div className="send-popup-btn">知道了</div>
                  </div>
                </div>
              </div>
            </section>
          </div>
        </div>
      )}

      {/* 历史列表（详情视图下也保留，方便点其他一条） */}
      <section className="card">
        <h2>
          发送历史
          <button className="linkish" style={{ marginLeft: 10 }} onClick={loadHistory}>
            刷新
          </button>
        </h2>
        {historyErr ? (
          <div className="alert warn">历史加载失败：{historyErr}</div>
        ) : history.length === 0 ? (
          <p className="hint">还没有发送记录。</p>
        ) : (
          <div className="table-scroll">
            <table className="send-table">
              <thead>
                <tr>
                  <th>时间</th>
                  <th>标题</th>
                  <th>图片</th>
                  <th>渠道</th>
                  <th>圈人</th>
                  <th>计数</th>
                </tr>
              </thead>
              <tbody>
                {history.map((m) => (
                  <tr
                    key={m.id}
                    className="send-row-link"
                    onClick={() => setView({ kind: 'detail', id: m.id })}
                  >
                    <td className="hint">{fmtTime(m.createdAt)}</td>
                    <td>
                      {m.title || <span className="hint">（无标题）</span>}
                      <div className="hint send-clamp">{m.content}</div>
                    </td>
                    <td className="hint">{IMAGE_LABEL[m.imageKind] ?? m.imageKind}</td>
                    <td className="hint">{channelText(m.channels)}</td>
                    <td className="hint">{TARGET_LABEL[m.targetType] ?? m.targetType}</td>
                    <td>
                      <CountBadges counts={m.counts} />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {/* 发送确认弹窗（N>100 时进入 stage 2 二次确认） */}
      {confirmBox && (
        <div className="mask" onClick={() => !sending && setConfirmBox(null)}>
          <div className="master-pop" onClick={(e) => e.stopPropagation()}>
            {confirmBox.stage === 1 ? (
              <>
                <h2 style={{ marginBottom: 0 }}>确认发送</h2>
                <p>
                  将发送给 <b className="num">{confirmBox.n}</b> 人 × 渠道{' '}
                  <b className="num">{confirmBox.m}</b>（
                  {channels.map((c) => CHANNEL_LABEL[c] ?? c).join(' + ')}）
                </p>
                <p className="hint">
                  标题：{title.trim() || '（无标题）'}
                  <br />
                  {imageKind === 'lineage' ? `附图：师门日报 · ${imageDate}` : '附图：无'}
                  <br />
                  圈人方式：{tab === 'multi' ? '多选' : tab === 'masters' ? '按师傅' : batch === 'all' ? '全部' : batch === 'male' ? '男团' : '女团'}
                  ；未绑定渠道的收件人该渠道记「跳过」。
                </p>
                <div className="row" style={{ marginBottom: 0 }}>
                  <button className="primary" disabled={sending} onClick={() =>
                    confirmBox.n > 100
                      ? setConfirmBox({ ...confirmBox, stage: 2 })
                      : doSend()
                  }>
                    {confirmBox.n > 100 ? '人数超 100，继续' : '确认发送'}
                  </button>
                  <button disabled={sending} onClick={() => setConfirmBox(null)}>
                    取消
                  </button>
                </div>
              </>
            ) : (
              <>
                <h2 style={{ marginBottom: 0 }}>二次确认</h2>
                <div className="alert warn">
                  本次将发送给 {confirmBox.n} 人（超过 100 人，单批约需几分钟逐个推送），
                  确定要继续吗？
                </div>
                <div className="row" style={{ marginBottom: 0 }}>
                  <button className="primary" disabled={sending} onClick={doSend}>
                    {sending ? '发送中…' : '确定发送'}
                  </button>
                  <button disabled={sending} onClick={() => setConfirmBox(null)}>
                    取消
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

/* ---------------- 搜索结果表（多选 / 按师傅共用，操作列随 Tab 切换） ---------------- */
function TargetTable(props: {
  hits: SendTarget[]
  tab: 'multi' | 'masters'
  isPicked: (t: SendTarget) => boolean
  onToggle: (t: SendTarget) => void
  expanded: number | null
  onExpand: (personId: number) => void
  disciplesOf: (masterId: number) => SendTarget[]
  fullReady: boolean
  onAddDisciples: (master: SendTarget) => void
}) {
  const { hits, tab, isPicked, onToggle, expanded, onExpand, disciplesOf, fullReady, onAddDisciples } =
    props
  if (hits.length === 0)
    return <div className="empty">没有匹配的主播（换个关键字，或清空搜索看全部）</div>
  return (
    <div className="table-scroll" style={{ maxHeight: 420, overflow: 'auto' }}>
      <table className="send-table">
        <thead>
          <tr>
            <th>{tab === 'masters' ? '选为师傅' : '选'}</th>
            <th>姓名</th>
            <th>团别</th>
            <th>师傅</th>
            <th title="微信机器人私聊推送">wxbot</th>
            <th title="展示端「我的」页弹窗">弹窗</th>
            <th title="直系徒弟数">徒弟</th>
            {tab === 'multi' && <th></th>}
          </tr>
        </thead>
        <tbody>
          {hits.map((t) => {
            const disciples = disciplesOf(t.personId)
            const isopen = expanded === t.personId
            return (
              <FragmentRow
                key={t.personId}
                t={t}
                tab={tab}
                picked={isPicked(t)}
                onToggle={onToggle}
                open={isopen}
                onExpand={onExpand}
                disciples={disciples}
                fullReady={fullReady}
                onAddDisciples={onAddDisciples}
                isPicked={isPicked}
              />
            )
          })}
        </tbody>
      </table>
    </div>
  )
}

/** 一行 + 可选的徒弟展开行（拆出来让 key/展开逻辑清楚一点） */
function FragmentRow(props: {
  t: SendTarget
  tab: 'multi' | 'masters'
  picked: boolean
  onToggle: (t: SendTarget) => void
  open: boolean
  onExpand: (personId: number) => void
  disciples: SendTarget[]
  fullReady: boolean
  onAddDisciples: (master: SendTarget) => void
  isPicked: (t: SendTarget) => boolean
}) {
  const { t, tab, picked, onToggle, open, onExpand, disciples, fullReady, onAddDisciples, isPicked } =
    props
  const canExpand = tab === 'multi' && (t.discipleCount > 0 || (fullReady && disciples.length > 0))
  return (
    <>
      <tr
        className="send-row-link"
        onClick={() => {
          if (canExpand) onExpand(t.personId)
        }}
      >
        <td onClick={(e) => e.stopPropagation()}>
          <input type="checkbox" checked={picked} onChange={() => onToggle(t)} />
        </td>
        <td>
          <span style={{ fontWeight: 600 }}>{t.name}</span>
          {canExpand && <span className="hint" style={{ marginLeft: 6 }}>{open ? '▾' : '▸'}</span>}
        </td>
        <td>
          <span className={`badge ${t.team === 'female' ? 'b-female' : 'b-male'}`}>
            {teamLabel(t.team)}
          </span>
        </td>
        <td className="hint">{t.masterName || '—'}</td>
        <td>
          <Mark ok={t.hasWx} tip={t.hasWx ? '已绑定微信机器人，可私聊推送' : '未绑定微信机器人，发送时该渠道记跳过'} />
        </td>
        <td>
          <Mark ok={t.hasPopup} tip={t.hasPopup ? '已开通「我的」页账号，可弹窗' : '未开通「我的」页账号，发送时该渠道记跳过'} />
        </td>
        <td className="hint">{t.discipleCount || '—'}</td>
        {tab === 'multi' && (
          <td onClick={(e) => e.stopPropagation()}>
            {(t.discipleCount > 0 || disciples.length > 0) && (
              <button
                title="把该师傅本人 + 全部直系徒弟并入下方选择"
                onClick={() => onAddDisciples(t)}
              >
                加上其徒弟
              </button>
            )}
          </td>
        )}
      </tr>
      {open && (
        <tr>
          <td colSpan={tab === 'multi' ? 8 : 7} style={{ background: 'var(--brand-soft)' }}>
            {disciples.length === 0 ? (
              <span className="hint">
                {fullReady
                  ? '该师傅名下暂无可发的直系徒弟'
                  : '徒弟名单还在加载（或全量名单不可用）——发送时以服务端展开为准'}
              </span>
            ) : (
              <div className="send-chips" style={{ margin: 0 }}>
                {disciples.map((d) => (
                  <span
                    key={d.personId}
                    className={isPicked(d) ? 'send-chip send-chip-on' : 'send-chip'}
                  >
                    <button
                      type="button"
                      title={isPicked(d) ? '点击移出选择' : '点击加入选择'}
                      onClick={(e) => {
                        e.stopPropagation()
                        onToggle(d)
                      }}
                    >
                      {isPicked(d) ? '✓ ' : '+ '}
                      {d.name}（{teamLabel(d.team)}）
                    </button>
                  </span>
                ))}
              </div>
            )}
          </td>
        </tr>
      )}
    </>
  )
}

/* ---------------- 计数徽标（历史列表 / 详情共用） ---------------- */
function CountBadges({ counts }: { counts?: SendMessage['counts'] }) {
  if (!counts) return <span className="hint">—</span>
  return (
    <span style={{ display: 'inline-flex', gap: 4, flexWrap: 'wrap' }}>
      {counts.sent > 0 && <span className="badge b-active">已发 {counts.sent}</span>}
      {counts.pending > 0 && <span className="badge b-paused">待发 {counts.pending}</span>}
      {counts.failed > 0 && <span className="badge b-left">失败 {counts.failed}</span>}
      {counts.skipped > 0 && <span className="badge">跳过 {counts.skipped}</span>}
      {counts.sent + counts.pending + counts.failed + counts.skipped === 0 && (
        <span className="hint">尚无 outbox 记录</span>
      )}
    </span>
  )
}

/* ---------------- 发送详情（3s 轮询直到无 pending） ---------------- */
function DetailCard({ id, onBack }: { id: number; onBack: () => void }) {
  const [data, setData] = useState<SendMessageDetail | null>(null)
  const [err, setErr] = useState('')

  useEffect(() => {
    let alive = true
    let timer: number | null = null
    const tick = async () => {
      try {
        const d = await fetchSendMessageDetail(id)
        if (!alive) return
        setData(d)
        setErr('')
        const hasPending = (d.outbox ?? []).some((r) => r.status === 'pending')
        if (!hasPending && timer != null) {
          window.clearInterval(timer)
          timer = null
        }
      } catch (e: unknown) {
        // 轮询中的单次失败不终止轮询（worker 还在跑，下一轮再试）
        if (alive) setErr(e instanceof Error ? e.message : String(e))
      }
    }
    void tick()
    timer = window.setInterval(tick, 3000)
    return () => {
      alive = false
      if (timer != null) window.clearInterval(timer)
    }
  }, [id])

  const m = data?.message
  return (
    <section className="card">
      <h2>
        发送详情 #{id}
        <button style={{ float: 'right' }} onClick={onBack}>
          返回列表
        </button>
      </h2>
      {err && <div className="alert warn">刷新失败（会自动重试）：{err}</div>}
      {!m ? (
        <p className="hint">加载中…</p>
      ) : (
        <>
          <p>
            <b>{m.title || '（无标题）'}</b>
            <span className="hint" style={{ marginLeft: 8 }}>
              {fmtTime(m.createdAt)} · {IMAGE_LABEL[m.imageKind] ?? m.imageKind}
              {m.imageDate ? ` · ${m.imageDate}` : ''} · 渠道 {channelText(m.channels)} · 圈人{' '}
              {TARGET_LABEL[m.targetType] ?? m.targetType}
            </span>
          </p>
          <p className="send-detail-content">{m.content}</p>
          <p>
            <CountBadges counts={m.counts} />
            {(m.counts?.pending ?? 0) > 0 && (
              <span className="hint" style={{ marginLeft: 8 }}>
                正在逐个推送（限速防风控），页面每 3 秒自动刷新…
              </span>
            )}
          </p>
          <div className="table-scroll">
            <table className="send-table">
              <thead>
                <tr>
                  <th>收件人</th>
                  <th>渠道</th>
                  <th>状态</th>
                  <th>错误信息</th>
                  <th>发送时间</th>
                  <th title="web 渠道：收件人在「我的」页点「知道了」后记录">已读时间</th>
                </tr>
              </thead>
              <tbody>
                {(data?.outbox ?? []).map((r) => (
                  <tr key={r.id}>
                    <td style={{ fontWeight: 600 }}>{r.recipientName}</td>
                    <td className="hint">{CHANNEL_LABEL[r.channel] ?? r.channel}</td>
                    <td>
                      <span className={OUTBOX_STATUS[r.status]?.cls ?? 'badge'}>
                        {OUTBOX_STATUS[r.status]?.label ?? r.status}
                      </span>
                    </td>
                    <td className="hint">{r.error || '—'}</td>
                    <td className="hint">{fmtTime(r.sentAt)}</td>
                    <td className="hint">
                      {r.channel === 'popup'
                        ? r.readAt
                          ? `已读 ${fmtTime(r.readAt)}`
                          : r.status === 'skipped'
                            ? '未开通'
                            : '未读'
                        : '—'}
                    </td>
                  </tr>
                ))}
                {(data?.outbox ?? []).length === 0 && (
                  <tr>
                    <td colSpan={6} className="hint">
                      还没有 outbox 记录（worker 尚未展开收件人）
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </>
      )}
    </section>
  )
}

/* ---------------- 页面私有样式（不动全局 styles.css） ---------------- */
const SEND_CSS = `
.send-cols { display: grid; grid-template-columns: minmax(0, 1fr) 340px; gap: 14px; align-items: start; }
.send-sticky { display: grid; gap: 14px; min-width: 0; }
.send-sticky:last-child { position: sticky; top: 16px; }
@media (max-width: 1020px) { .send-cols { grid-template-columns: 1fr; } .send-sticky:last-child { position: static; } }

.send-tabs { display: flex; gap: 4px; background: color-mix(in srgb, var(--ink) 5%, transparent); padding: 4px; border-radius: 10px; width: fit-content; margin: 8px 0; }
.send-tabs button { border: none; background: transparent; padding: 6px 16px; border-radius: 8px; font: inherit; font-size: 13px; cursor: pointer; color: var(--ink2); }
.send-tabs button.on { background: #fff; color: var(--brand); font-weight: 600; box-shadow: var(--shadow-card); }

.send-table th, .send-table td { padding: 6px 8px; text-align: left; white-space: normal; }
.send-row-link { cursor: pointer; }
.send-row-link:hover td { background: var(--brand-soft); }
.send-clamp { max-width: 320px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

.send-count { display: inline-block; background: var(--brand); color: #fff; border-radius: 999px; font-size: 11.5px; padding: 2px 10px; font-weight: 600; vertical-align: middle; }
.send-chips { display: flex; flex-wrap: wrap; gap: 6px; margin: 8px 0; }
.send-chip { display: inline-flex; align-items: center; gap: 4px; background: color-mix(in srgb, var(--ink) 5%, transparent); color: var(--ink2); border-radius: 999px; padding: 3px 6px 3px 10px; font-size: 12px; }
.send-chip button { border: none; background: transparent; cursor: pointer; color: inherit; font: inherit; font-size: 12px; padding: 0 2px; }
.send-chip-on { background: var(--brand-soft); color: var(--brand); font-weight: 600; }

/* 手机壳预览 */
.send-phone { width: 290px; max-width: 100%; margin: 10px auto 0; background: #101828; border-radius: 34px; padding: 10px; box-shadow: var(--shadow-pop); }
.send-screen { background: #eef0f4; border-radius: 24px; padding: 12px 10px 16px; min-height: 430px; display: flex; flex-direction: column; gap: 10px; }
.send-screen-bar { text-align: center; font-size: 11px; color: var(--ink3); }
.send-popup { background: #fff; border-radius: 14px; padding: 14px; box-shadow: 0 6px 24px rgb(16 24 40 / 12%); display: grid; gap: 8px; margin-top: 8px; }
.send-popup-bar { font-size: 11px; color: var(--ink3); letter-spacing: 0.02em; }
.send-popup-title { font-size: 15px; font-weight: 700; color: var(--ink); word-break: break-word; }
.send-popup-img { border: 1px dashed var(--brand); border-radius: 10px; padding: 16px 10px; text-align: center; color: var(--brand); font-size: 12px; background: var(--brand-soft); }
.send-popup-text { font-size: 13px; line-height: 1.65; color: var(--ink2); white-space: pre-wrap; word-break: break-word; min-height: 42px; }
.send-popup-btn { margin-top: 2px; text-align: center; background: var(--brand); color: #fff; border-radius: 999px; padding: 8px 0; font-size: 13px; font-weight: 600; }

.send-detail-content { white-space: pre-wrap; word-break: break-word; background: color-mix(in srgb, var(--ink) 4%, transparent); border-radius: 8px; padding: 10px 12px; font-size: 13.5px; }
`
