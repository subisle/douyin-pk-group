/* 消息下发页（/admin/send，docs/23 §5/§6）：
 * - 编辑卡片 + 收件人三种圈人（多选 / 按师傅 / 批量）；
 * - 发送确认（N>100 二次确认）→ 跳转发送详情（3s 轮询直到无 pending）；
 * - 发送历史列表。
 * 2026-10-15 用户口径：**去掉弹窗预览** —— 原右侧手机壳预览区是纯前端装饰
 * （只读本地 state，不走任何接口），删掉不影响发送主链路。
 * 2026-10-15 task-14/17：支持随消息发**一张 PNG 上传图**（后端按文件头魔数校验、解码后
 * ≤5MB；前端本地同口径拦截并给提示），与服务端渲染图（imageKind）互斥。
 * 防注入：标题/正文一律 React 文本节点渲染（详情、历史都一样），全页没有
 * dangerouslySetInnerHTML，也不引外部 HTML。
 * 样式只复用 styles.css 既有类，页面私有的布局类全部 send- 前缀，写在本文件
 * 底部的局部 <style> 里，不动全局 styles.css。 */
import { useCallback, useEffect, useMemo, useRef, useState, type ChangeEvent } from 'react'
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
import Pager, { usePager } from './Pager'

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
  custom: '上传图',
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

/* ---------------- 上传图（task-14/15）：只收 PNG、解码后 ≤5MB ---------------- */

/** 与后端 `MAX_IMAGE_BYTES` 同口径（5MB，按**解码后**字节算）；超限本地就拦，别撞 8MB body limit */
const MAX_UPLOAD_BYTES = 5 * 1024 * 1024
/** PNG 文件头魔数：后端 `sniff_image_mime` 只信字节，前端同样不能只信 `file.type` */
const PNG_MAGIC = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]

/** 已选上传图（存裸 base64；缩略图 src 自己拼 dataURL，不依赖浏览器给的 mime） */
type UploadImg = { name: string; bytes: number; base64: string }

/** 文件前 8 字节是否为 PNG 魔数 */
async function hasPngMagic(file: File): Promise<boolean> {
  const head = new Uint8Array(await file.slice(0, PNG_MAGIC.length).arrayBuffer())
  return head.length === PNG_MAGIC.length && PNG_MAGIC.every((b, i) => head[i] === b)
}

/** 读成**裸 base64**（剥掉 dataURL 前缀；浏览器 type 为空时会拼成 octet-stream，不能直接用） */
function readFileAsBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const fr = new FileReader()
    fr.onload = () => {
      const s = String(fr.result ?? '')
      const comma = s.indexOf(',')
      resolve(comma >= 0 ? s.slice(comma + 1) : s)
    }
    fr.onerror = () => reject(new Error('读取文件失败，请重试'))
    fr.readAsDataURL(file)
  })
}

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  return `${(n / 1024 / 1024).toFixed(2)} MB`
}

/** 历史 / 详情的图片列文案：custom 带文件名与体积，其余按 IMAGE_LABEL（渲染图带日期） */
function imageText(m: {
  imageKind: string
  imageDate?: string | null
  imageName?: string
  imageBytes?: number
}): string {
  const label = IMAGE_LABEL[m.imageKind] ?? m.imageKind
  if (m.imageKind === 'custom') {
    return `${label}${m.imageName ? ` ${m.imageName}` : ''}${
      m.imageBytes ? `（${fmtBytes(m.imageBytes)}）` : ''
    }`
  }
  return m.imageDate ? `${label} · ${m.imageDate}` : label
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
  /** 上传图（task-14）：null = 本次不带图 —— 提交时**不能**带 imageBase64 字段（空串会被判 400） */
  const [upload, setUpload] = useState<UploadImg | null>(null)
  const [uploadErr, setUploadErr] = useState('')

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

  /** 选图：魔数 + type/扩展名 + 5MB 三重本地校验；任一条不过就不落 state，只给中文提示 */
  async function onPickImage(e: ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0]
    e.target.value = '' // 清空以便再次选同一个文件还能触发 onChange
    if (!file) return
    setUploadErr('')
    if (file.type !== 'image/png' || !/\.png$/i.test(file.name)) {
      setUpload(null)
      setUploadErr(`只支持 PNG 图片（当前：${file.type || '未知类型'}，${file.name}）`)
      return
    }
    if (file.size > MAX_UPLOAD_BYTES) {
      setUpload(null)
      setUploadErr(
        `图片太大：${fmtBytes(file.size)}，上限 5MB（后端按解码后体积校验，超限会 400）`,
      )
      return
    }
    try {
      if (!(await hasPngMagic(file))) {
        setUpload(null)
        setUploadErr('这不是 PNG 图片（文件内容与扩展名不符），请换一张')
        return
      }
      const base64 = await readFileAsBase64(file)
      if (!base64) {
        setUpload(null)
        setUploadErr('图片内容为空，请重新选择')
        return
      }
      setUpload({ name: file.name, bytes: file.size, base64 })
      setImageKind('none') // 与服务端渲染图（师门日报）互斥：同时给后端会 400
    } catch (err: unknown) {
      setUpload(null)
      setUploadErr(err instanceof Error ? err.message : String(err))
    }
  }

  function removeUpload() {
    setUpload(null)
    setUploadErr('')
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
        // 上传图与 imageKind 互斥：选图时固定 none（后端据 imageBase64 落 image_kind=custom）
        imageKind: upload ? 'none' : imageKind,
        imageDate,
        channels,
        target: buildTarget(),
        // 未选图时**不带** imageBase64（缺字段 = 没带图；带空串会被后端判 400）
        ...(upload
          ? { imageBase64: upload.base64, imageMime: 'image/png', imageName: upload.name }
          : {}),
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
          <div className="send-stack">
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
                <label title="随消息发给收件人的一张图片（与服务端渲染图互斥）">
                  选择图片{' '}
                  <input
                    type="file"
                    accept="image/png"
                    disabled={sending}
                    onChange={onPickImage}
                  />
                </label>
                <span className="hint">只支持 PNG，≤5MB（后端按文件头魔数校验）</span>
              </div>
              {upload && (
                <div className="row send-upload">
                  <img
                    className="send-upload-thumb"
                    src={`data:image/png;base64,${upload.base64}`}
                    alt={upload.name}
                  />
                  <span className="send-upload-meta">
                    {upload.name} · {fmtBytes(upload.bytes)}
                  </span>
                  <button type="button" onClick={removeUpload} disabled={sending}>
                    移除
                  </button>
                </div>
              )}
              {uploadErr && <div className="alert warn">{uploadErr}</div>}
              <div className="row">
                <label>
                  图片类型{' '}
                  <select
                    value={upload ? 'none' : imageKind}
                    disabled={!!upload}
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
                    disabled={!!upload}
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
              {upload && (
                <p className="hint">
                  已选上传图：与「图片类型」里的服务端渲染图**互斥**（同时提交后端会 400），
                  本次按上传图发送（后端把 imageKind 落成 custom）。
                </p>
              )}
              {imageKind === 'lineage' && !upload && (
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
                    filtering={`${q}|${tab}`}
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
                    <td title={m.title || undefined}>
                      <span className="cell-clip">{m.title || <span className="hint">（无标题）</span>}</span>
                      <div className="hint send-clamp">{m.content}</div>
                    </td>
                    <td className="hint">{imageText(m)}</td>
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
                  {upload
                    ? `附图：上传图 ${upload.name}（${fmtBytes(upload.bytes)}）`
                    : imageKind === 'lineage'
                      ? `附图：师门日报 · ${imageDate}`
                      : '附图：无'}
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
  /** 客户端筛选口径（搜索词 + 圈人 Tab）：一变就回第 1 页 */
  filtering: string
}) {
  const { hits, tab, isPicked, onToggle, expanded, onExpand, disciplesOf, fullReady, onAddDisciples, filtering } =
    props
  /* 客户端分页（2026-10-16 表格规范）：全量名单时这张表能有几百行。
     勾选集合在父级（Map<personId>），切片只影响渲染，翻页不丢选中；
     展开态是 personId，翻到别页时那一行不在切片里，自然不渲染、也不报错。 */
  const pager = usePager(hits.length, filtering)
  const rows = pager.slice(hits)
  if (hits.length === 0)
    return <div className="empty">没有匹配的主播（换个关键字，或清空搜索看全部）</div>
  return (
    <>
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
              <th className="num" title="直系徒弟数">徒弟</th>
              {tab === 'multi' && <th className="col-actions"></th>}
            </tr>
          </thead>
          <tbody>
            {rows.map((t) => {
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
      {/* 分页条放滚动容器外：不跟着表头一起被滚走 */}
      <Pager {...pager} />
    </>
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
        <td title={t.name}>
          <span style={{ display: 'flex', alignItems: 'center', gap: 6, minWidth: 0 }}>
            <span className="cell-clip" style={{ fontWeight: 600 }}>
              {t.name}
            </span>
            {canExpand && <span className="hint">{open ? '▾' : '▸'}</span>}
          </span>
        </td>
        <td>
          <span className={`badge ${t.team === 'female' ? 'b-female' : 'b-male'}`}>
            {teamLabel(t.team)}
          </span>
        </td>
        <td className="hint" title={t.masterName || undefined}>
          <span className="cell-clip">{t.masterName || '—'}</span>
        </td>
        <td>
          <Mark ok={t.hasWx} tip={t.hasWx ? '已绑定微信机器人，可私聊推送' : '未绑定微信机器人，发送时该渠道记跳过'} />
        </td>
        <td>
          <Mark ok={t.hasPopup} tip={t.hasPopup ? '已开通「我的」页账号，可弹窗' : '未开通「我的」页账号，发送时该渠道记跳过'} />
        </td>
        <td className="hint num">{t.discipleCount || '—'}</td>
        {tab === 'multi' && (
          <td className="row-actions" onClick={(e) => e.stopPropagation()}>
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

  /* 收件人明细一次最多 500 行（批量圈人的展开上限），同样客户端分页。
     该表没有客户端筛选 → resetKey 传空串（不因 3s 轮询刷新而跳页）。 */
  const outbox = data?.outbox ?? []
  const pager = usePager(outbox.length, '')

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
              {fmtTime(m.createdAt)} · {imageText(m)} · 渠道 {channelText(m.channels)} · 圈人{' '}
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
                {pager.slice(outbox).map((r) => (
                  <tr key={r.id}>
                    <td style={{ fontWeight: 600 }} title={r.recipientName}>
                      <span className="cell-clip">{r.recipientName}</span>
                    </td>
                    <td className="hint">{CHANNEL_LABEL[r.channel] ?? r.channel}</td>
                    <td>
                      <span className={OUTBOX_STATUS[r.status]?.cls ?? 'badge'}>
                        {OUTBOX_STATUS[r.status]?.label ?? r.status}
                      </span>
                    </td>
                    <td className="hint" title={r.error || undefined}>
                      <span className="cell-clip">{r.error || '—'}</span>
                    </td>
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
                {outbox.length === 0 && (
                  <tr>
                    <td colSpan={6} className="hint empty-cell">
                      还没有 outbox 记录（worker 尚未展开收件人）
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
          <Pager {...pager} />
        </>
      )}
    </section>
  )
}

/* ---------------- 页面私有样式（不动全局 styles.css） ---------------- */
const SEND_CSS = `
.send-stack { display: grid; gap: 14px; min-width: 0; }

.send-tabs { display: flex; gap: 4px; background: color-mix(in srgb, var(--ink) 5%, transparent); padding: 4px; border-radius: 10px; width: fit-content; margin: 8px 0; }
.send-tabs button { border: none; background: transparent; padding: 6px 16px; border-radius: 8px; font: inherit; font-size: 13px; cursor: pointer; color: var(--ink2); }
.send-tabs button.on { background: #fff; color: var(--brand); font-weight: 600; box-shadow: var(--shadow-card); }

.send-table th, .send-table td { padding: 6px 8px; text-align: left; white-space: normal; }
/* 数字列（徒弟数）：全局 td.num 的右对齐与上面的 .send-table td 同权重、被后者压成
   左对齐，这里按表格规范补回右对齐 + 等宽数字（不改全局 styles.css） */
.send-table td.num, .send-table th.num { text-align: right; font-variant-numeric: tabular-nums; }
.send-row-link { cursor: pointer; }
.send-row-link:hover td { background: var(--brand-soft); }
.send-clamp { max-width: 320px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

.send-count { display: inline-block; background: var(--brand); color: #fff; border-radius: 999px; font-size: 11.5px; padding: 2px 10px; font-weight: 600; vertical-align: middle; }
.send-chips { display: flex; flex-wrap: wrap; gap: 6px; margin: 8px 0; }
.send-chip { display: inline-flex; align-items: center; gap: 4px; background: color-mix(in srgb, var(--ink) 5%, transparent); color: var(--ink2); border-radius: 999px; padding: 3px 6px 3px 10px; font-size: 12px; }
.send-chip button { border: none; background: transparent; cursor: pointer; color: inherit; font: inherit; font-size: 12px; padding: 0 2px; }
.send-chip-on { background: var(--brand-soft); color: var(--brand); font-weight: 600; }

/* 上传图：缩略图 + 文件名/体积 + 移除按钮 */
.send-upload { align-items: center; gap: 10px; }
.send-upload-thumb { width: 72px; height: 72px; object-fit: contain; border: 1px solid var(--line); border-radius: 8px; background: #fff; }
.send-upload-meta { font-size: 12px; color: var(--ink2); word-break: break-all; }

.send-detail-content { white-space: pre-wrap; word-break: break-word; background: color-mix(in srgb, var(--ink) 4%, transparent); border-radius: 8px; padding: 10px 12px; font-size: 13.5px; }
`
