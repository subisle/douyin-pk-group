import { useCallback, useEffect, useMemo, useState } from 'react'
import { api } from './api'
import Pager, { usePager } from './Pager'
import { useAutoRefresh } from './useAutoRefresh'

/** 头像兜底色：一组柔和色，按 personId 取模挑选 —— 同一个人颜色永远一样，不同人尽量错开。 */
const AVATAR_COLORS = [
  '#5b8def', '#e58f5a', '#4caf8e', '#9a6fd0',
  '#d96a93', '#5aa9c9', '#b8894a', '#6f9a52',
]
function avatarColor(personId: number): string {
  return AVATAR_COLORS[Math.abs(personId) % AVATAR_COLORS.length]
}

/** 姓名首字圆形色块：没有本地留存头像（或头像加载失败）时的兜底，
 *  保证「有图用抖音真实头像，无图回落首字色块、不得破图」（见 docs/08 展示端需求）。 */
function NameBlock({ s }: { s: any }) {
  const ch = String(s.name ?? '').trim().charAt(0) || '?'
  return (
    <span
      className="avatar-block"
      style={{ background: avatarColor(s.personId) }}
      title="暂无抖音头像（本地未留存）"
      aria-hidden
    >
      {ch}
    </span>
  )
}

/** 主播头像：hasAvatar=true（person_avatar 表有本地留存图）时请求同源头像端点；
 *  avatarHash 作缓存破除（?v=hash），hash 为空但有图时不带 ?v=。
 *  字段缺失（旧后端）等同无图。404/加载失败时记下失败 URL 落回首字色块 ——
 *  存 URL 而不是布尔值，hash 变化（头像更新）后能自动再试，不用人为重置。 */
function StreamerAvatar({ s }: { s: any }) {
  const [failedSrc, setFailedSrc] = useState<string | null>(null)
  const src = s.hasAvatar
    ? `/api/public/avatars/${s.personId}${s.avatarHash ? `?v=${s.avatarHash}` : ''}`
    : null
  const showImg = !!src && src !== failedSrc
  return (
    <span className="avatar-cell">
      {showImg && src ? (
        <img
          className="avatar-img"
          src={src}
          alt={String(s.name ?? '')}
          title="抖音头像（本地留存）"
          loading="lazy"
          width={32}
          height={32}
          onError={() => setFailedSrc(src)}
        />
      ) : (
        <NameBlock s={s} />
      )}
    </span>
  )
}

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
        {/* 长标识文本单行省略（完整内容在 title），避免整行被两行文本撑高 */}
        <span className="cell-clip">未登记抖音号{s.anchorId ? `（采集号 ${s.anchorId}）` : ''}</span>
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
        <td title="未绑定微信机器人（主播在展示端「我的」页扫码加机器人，再用同一个微信给机器人发一条消息即自动对接）">
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
      <td title="未绑定微信机器人（主播在展示端「我的」页扫码加机器人，再用同一个微信给机器人发一条消息即自动对接）">
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


/** 危险操作二次确认（2026-10-16）：window.confirm 的按钮由浏览器写死成「确定 / 取消」，
    既写不下「删的是谁、删完会怎样」，也做不到 DESIGN §4.3 的「二次确认不用同一个确定」。
    改成页内确认弹窗：标题写对象（名字 + 编号）、正文写后果、按钮写具体动作。
    合并走自己的弹窗（mergePop，要先选人），这里只管「对象已经在手上」的几类。

    弹窗按钮规范（2026-10-16 Lead 定，起因：按位置猜选择器把 danger 当成了取消）：
    ① 危险按钮永远 `className="danger"`，文案必须写「动作 + 对象」（如「删除「三宝」」），
       禁止用「确定 / 确认」这类不带对象的词；
    ② 取消按钮永远排在**最后一位**，并给 `className="ghost"`；
    ③ 正文必须给出对象（名字 + 编号）与后果（软删 / 数据保留 / 踢下线等）。
    自动化验收只准用 `.mask .master-pop .row button.ghost` 点取消，禁止用「第一个按钮」定位。 */
type DangerConfirm =
  | { kind: 'remove'; personId: number; name: string }
  | { kind: 'batchRemove'; ids: number[]; names: string[] }
  | { kind: 'unbind'; personId: number; personName: string; accountId: number; accountLabel: string }
  | { kind: 'removeMyAccount'; name: string }

/** 账号在确认文案里的叫法：优先抖音号，没登记抖音号就用采集 ID，再没有就用行号 ——
    台账里有「只有采集号、没登记抖音号」的行，直接拼 a.douyinNo 会写出「抖音号 」这种空对象。 */
function accountLabel(a: any) {
  const no = String(a?.douyinNo ?? '').trim()
  if (no) return `抖音号 ${no}`
  const anchor = String(a?.anchorId ?? '').trim()
  if (anchor) return `采集号 ${anchor}`
  return `账号 #${a?.id ?? '?'}`
}

/** 批量删除的对象摘要：前 3 个名字 + 「等」+ 总数（勾选跨分页保留，items 是全量名单）。
    勾选的人都不在当前筛选结果里时退回「共 N 人」，不让弹窗出现空对象。 */
function batchTargetText(names: string[], total: number) {
  const head = names.slice(0, 3).join('、')
  if (!head) return `共 ${total} 人（不在当前筛选结果里）`
  return `${head}${total > 3 ? ' 等' : ''}（共 ${total} 人）`
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
  // 分页（2026-10-16）：筛选条件（q/team）变化时自动回第 1 页
  const pager = usePager(items.length, `${q}|${team}`)
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
  /** 危险操作确认弹窗的待确认对象（删除 / 批量删除 / 解绑 / 删账号）；null = 没有待确认动作。
      非空时才会渲染确认弹窗，confirmBusy = 正在提交，期间按钮禁用、点遮罩不关闭。 */
  const [confirmPop, setConfirmPop] = useState<DangerConfirm | null>(null)
  const [confirmBusy, setConfirmBusy] = useState(false)
  /** 「我的」页面账号弹窗（右键主播 → 设置手机号/密码；主播用 姓名+密码 登录） */
  const [myAccPop, setMyAccPop] = useState<{
    name: string
    phone: string
    pass: string
    exists: boolean
    viewTeam: string
    isAdmin: boolean
    /** 以下三项由后端 T1/T2 在 /api/admin/my-accounts* 下发（2026-10-18 账号体系口径）：
        只给脱敏值，管理端不显示明文 QQ / 邮箱。字段缺失（旧后端）时 qqStatusKnown=false。 */
    emailMask: string
    qqBound: boolean
    mustChangePass: boolean
    qqStatusKnown: boolean
  } | null>(null)
  /** 批量开号弹窗：给勾选的主播批量建展示端登录账号（result 非空 = 展示结果表） */
  const [batchAcc, setBatchAcc] = useState<{ defaultPass: string; forceReset: boolean; result: any | null } | null>(null)
  const [batchAccBusy, setBatchAccBusy] = useState(false)

  /* 导入暂存区相关 state / 拉取 / 建档 / 丢弃已整块移除（2026-10-14 用户口径：
     暂存内容不需要在台账页显示）。后端端点与补账逻辑不动，只是前端不再呈现。 */

  const load = useCallback(() => {
    const qs = new URLSearchParams()
    if (q) qs.set('q', q)
    if (team) qs.set('team', team)
    api(`/api/admin/streamers?${qs}`)
      .then((d) => setItems(d.items ?? []))
      .catch((e) => setError(e.message ?? String(e)))
  }, [q, team])

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

  /** 主播名字：列表里找不到时退回 #id —— 确认文案里不允许出现空白对象 */
  const nameOf = (personId: number) =>
    items.find((i) => i.personId === personId)?.name ?? `#${personId}`

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

  /** 补账提示（2026-10-18）：建档/绑号会同时补「按日」与「月度」两个暂存区。
      月度是 042 新链路（先导月报、事后才建档/绑号的整月数据），有补到就一并说出来，
      否则运营会以为那个月的月榜数据没回来。 */
  function absorbedText(daily?: number, month?: number) {
    const bits: string[] = []
    if (daily) bits.push(`按日 ${daily} 行`)
    if (month) bits.push(`月度 ${month} 行`)
    if (!bits.length) return ''
    return `（暂存区已补账入库：${bits.join(' + ')}，现在开始出现在榜单里）`
  }

  async function create() {
    setError('')
    setMsg('')
    try {
      const d = await api<{ personId: number; absorbed?: number; absorbedMonth?: number }>(
        '/api/admin/streamers',
        {
          method: 'POST',
          body: JSON.stringify({ name, team: newTeam, douyinNo: douyinNo || undefined }),
        },
      )
      setMsg(`已建档：${name}` + absorbedText(d.absorbed, d.absorbedMonth))
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

  /** 右键 → 设置「我的」页面账号：手机号（选填）+ 密码（4~64 字符，任意字符、不 trim）；
      主播用 姓名+密码 登录，没有注册。2026-10-18 起「凭艺名直接认领开号」由展示端完成，
      管理端这里仍可手工开号/改密/删号。 */
  async function openMyAccount(name: string) {
    setError('')
    setMsg('')
    try {
      const d = await api(`/api/admin/my-accounts/${encodeURIComponent(name)}`)
      // QQ 绑定状态：后端 2026-10-18 新增 emailMask/qqBound/mustChangePass（脱敏，不发明文）；
      // 旧后端没有这三个字段时标记 qqStatusKnown=false，弹窗显示「暂不支持显示」而不是误报「未绑定」。
      const qqStatusKnown =
        d.qqBound !== undefined || d.emailMask !== undefined || d.mustChangePass !== undefined
      setMyAccPop({
        name,
        phone: d.found ? d.phone ?? '' : '',
        pass: '',
        exists: !!d.found,
        // 可看团别（迁移 024）：'' 沿用旧口径（绑了主播按主播所在团）
        viewTeam: d.found ? d.viewTeam ?? '' : '',
        isAdmin: !!d.isAdmin,
        emailMask: d.found ? String(d.emailMask ?? '') : '',
        qqBound: d.found ? d.qqBound === true : false,
        mustChangePass: d.found ? d.mustChangePass === true : false,
        qqStatusKnown: d.found && qqStatusKnown,
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
          // 2026-10-18 口令口径：4~64 字符、支持中英文/特殊字符/任意符号、服务端不 trim；
          // 这里不再 trim，避免把用户密码里的首尾空格吃掉（空串仍表示「不修改」）。
          ...(myAccPop.pass ? { pass: myAccPop.pass } : {}),
        }),
      })
      /* 可看团别走**专用端点**：上面那个 POST 不传 pass 时会自动生成新密码
         （手机号后四位），把 viewTeam 塞进去会顺手把密码重置掉。 */
      if (myAccPop.exists) {
        await api(`/api/admin/my-accounts/${encodeURIComponent(myAccPop.name)}/view-team`, {
          method: 'PUT',
          body: JSON.stringify({ viewTeam: myAccPop.viewTeam }),
        })
      }
      setMsg(`已保存「${myAccPop.name}」的我的页面账号`)
      setMyAccPop(null)
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  /** 删除「我的」页面登录账号：先弹确认（对象 = 哪个账号、后果 = 登录失效 + 踢下线）。
      以前这个 danger 按钮点了就直接删，没有任何二次确认。 */
  function askRemoveMyAccount() {
    if (!myAccPop || !myAccPop.exists) return
    setError('')
    setMsg('')
    setConfirmPop({ kind: 'removeMyAccount', name: myAccPop.name })
  }

  async function doRemoveMyAccount(name: string) {
    try {
      await api('/api/admin/my-accounts/' + encodeURIComponent(name), { method: 'DELETE' })
      setMsg(`已删除「${name}」的我的页面登录账号`)
      setMyAccPop(null)
      return true
    } catch (e: any) {
      setError(e.message ?? String(e))
      return false
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
      const bound = await api<{ absorbed?: number; absorbedMonth?: number }>(
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
        `已给「${detail.name}」绑定抖音号` + absorbedText(bound.absorbed, bound.absorbedMonth),
      )
      setTimeout(() => setMsg(''), 2000)
      load()
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  /** 解绑一个抖音号（主号解绑后自动把剩下的第一个提为主号）。
      先弹页内确认：对象 = 谁名下的哪个号，后果 = 之后的音浪不再并入。 */
  function askUnbind(a: any) {
    if (!detail) return
    setError('')
    setMsg('')
    setConfirmPop({
      kind: 'unbind',
      personId: detail.personId,
      personName: detail.name,
      accountId: a.id,
      accountLabel: accountLabel(a),
    })
  }

  async function doUnbind(personId: number, accountId: number) {
    try {
      await api(`/api/admin/streamers/${personId}/accounts/${accountId}`, {
        method: 'DELETE',
      })
      const d = await api(`/api/admin/streamers/${personId}/detail`)
      setDetail(d)
      load()
      return true
    } catch (e: any) {
      setError(e.message ?? String(e))
      return false
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

  /** 批量删除：先弹确认，写清条数 + 前 3 个名字（对象来自勾选集合，跨分页保留） */
  function askBatchDelete() {
    if (selected.size === 0) return
    setError('')
    setMsg('')
    const ids = [...selected]
    const names = items.filter((i) => selected.has(i.personId)).map((i) => String(i.name ?? ''))
    setConfirmPop({ kind: 'batchRemove', ids, names })
  }

  async function doBatchDelete(ids: number[]) {
    try {
      await api('/api/admin/streamers/batch-delete', {
        method: 'POST',
        body: JSON.stringify({ ids }),
      })
      setSelected(new Set())
      load()
      loadDupes()
      return true
    } catch (e: any) {
      setError(e.message ?? String(e))
      return false
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

  /** 单个删除 = 软删（后端 deleted_at，历史数据保留）。
      这里只负责开确认弹窗：对象写「名字 + 编号 #personId」，后果写「台账不再显示、历史数据保留」 */
  function askRemove(personId: number, streamerName: string) {
    setError('')
    setMsg('')
    setConfirmPop({ kind: 'remove', personId, name: streamerName })
  }

  /** 确认后真正执行：接口与刷新顺序不变（DELETE → load） */
  async function doRemove(personId: number) {
    try {
      await api(`/api/admin/streamers/${personId}`, { method: 'DELETE' })
      load()
      return true
    } catch (e: any) {
      setError(e.message ?? String(e))
      return false
    }
  }

  /** 确认弹窗的文案：标题写对象、正文写后果、按钮写动作（按 kind 现算，不用同一个「确定」） */
  function confirmCopy(c: DangerConfirm): { title: string; body: string; label: string } {
    switch (c.kind) {
      case 'remove':
        return {
          title: `删除主播「${c.name}」（编号 #${c.personId}）`,
          body:
            '软删：确认后 TA 从主播列表里消失（不再出现在名单里），已入库的历史数据' +
            '（音浪、账号、别名）保留在库里，不做物理删除。',
          label: `删除「${c.name}」`,
        }
      case 'batchRemove':
        return {
          title: `批量删除 ${c.ids.length} 个主播`,
          body:
            `对象：${batchTargetText(c.names, c.ids.length)}。` +
            '软删：确认后这些人从主播列表里消失，已入库的历史数据保留在库里，不做物理删除。',
          label: `删除这 ${c.ids.length} 个主播`,
        }
      case 'unbind':
        return {
          title: `解绑「${c.personName}」（编号 #${c.personId}）的 ${c.accountLabel}`,
          body:
            `对象：「${c.personName}」（编号 #${c.personId}）名下的 ${c.accountLabel}。` +
            '确认后该号之后的音浪不再并入 TA 名下（已入库的历史数据保留）；如果解绑的是主号，' +
            '剩下的第一个号会自动提为主号。',
          label: `解绑 ${c.accountLabel}`,
        }
      case 'removeMyAccount':
        return {
          title: `删除「${c.name}」的展示端登录账号`,
          body:
            `对象：姓名「${c.name}」的「我的」页面登录账号。` +
            '确认后该账号立即失效（已经登录的会被踢下线），不能再用「姓名 + 密码」登录；' +
            '主播列表与音浪数据不受影响。',
          label: `删除「${c.name}」的登录账号`,
        }
    }
  }

  /** 确认弹窗里点主按钮：按对象类型调对应的执行函数；成功才关闭弹窗，
      失败保留弹窗并把原因（error）显示在弹窗里，运营可以直接再点一次重试。 */
  async function runConfirm() {
    const c = confirmPop
    if (!c || confirmBusy) return
    setConfirmBusy(true)
    setError('')
    try {
      let ok = false
      if (c.kind === 'remove') ok = await doRemove(c.personId)
      else if (c.kind === 'batchRemove') ok = await doBatchDelete(c.ids)
      else if (c.kind === 'unbind') ok = await doUnbind(c.personId, c.accountId)
      else ok = await doRemoveMyAccount(c.name)
      if (ok) setConfirmPop(null)
    } finally {
      setConfirmBusy(false)
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

  /** 待确认动作的文案（null = 没弹窗；渲染时直接取，不在 state 里存 JSX） */
  const confirmCopyView = confirmPop ? confirmCopy(confirmPop) : null

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

      {/* 导入暂存区（2026-10-14 用户口径：不再显示）
          原先这里会挂一张「导入暂存 N 行未建档」卡片（收起态仍占一行，展开后是明细 +
          建档/丢弃按钮）。运营口径是暂存内容不需要在台账页出现，故整块移除。
          后端 /api/admin/import/pending 与 import_pending 表**保持原样**：导入时的
          暂存落库、建档/绑号后的自动补账逻辑都不受影响，只是前端不再呈现。 */}

      <section className="card">
        <h2>主播列表（{items.length} 人）</h2>
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
          <button disabled={selected.size === 0} onClick={askBatchDelete}>
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
              ⚠️ 疑似重复，请人工核对（没有重复时这条不会出现）：
              {[
                dupes.sameName?.length > 0 &&
                  `同名 ${dupes.sameName.length} 组（${dupes.sameName
                    .map((d: any) => d.name)
                    .join('、')}）—— 可能是同一个人建了两份档案，核对后可合并`,
                dupes.sameDouyinNo?.length > 0 &&
                  `同抖音号 ${dupes.sameDouyinNo.length} 组（${dupes.sameDouyinNo
                    .map((d: any) => `${d.douyinNo}＝${d.names || d.personIds}`)
                    .join('；')}）—— 一个抖音号挂在两个不同主播名下，核对归属后把号改绑到正确的人`,
              ]
                .filter(Boolean)
                .join('；')}
            </span>
          )}
        </div>
        {items.length === 0 ? (
          <div className="empty">还没有主播，先在上面建档或导入数据</div>
        ) : (
          <>
          <div className="table-scroll">
            <table style={{ minWidth: 1000 }}>
              <thead>
                <tr>
                  <th title="姓名（前面显示抖音头像，本地留存；无图时显示姓名首字色块）">姓名</th>
                  <th title="一个主播可挂多个抖音号；第一个是主号（SQL 按 is_primary DESC 排序）">抖音号</th>
                  <th title="是否登记了手机号（展示端登录用）；具体号码在编辑资料里查看">手机号</th>
                  <th title="微信推送机器人：未绑定 / 已扫码待激活 / 已绑定（显示微信昵称）">微信机器人</th>
                  <th>师傅</th>
                  <th>状态</th>
                  <th title="是否出现在对外展示的每日报告/月榜/年榜里">对外</th>
                  <th>别名</th>
                  <th className="col-actions"></th>
              </tr>
            </thead>
            <tbody>
              {pager.slice(items).map((s) => (
                <tr
                  key={s.personId}
                  style={{ opacity: s.status === 'left' ? 0.55 : undefined }}
                  onContextMenu={(e) => {
                    e.preventDefault()
                    setCtxMenu({ x: e.clientX, y: e.clientY, person: s })
                  }}
                >
                  <td>
                    {/* 头像只在列表行出现；右键菜单/详情弹窗等维持原样 */}
                    <StreamerAvatar s={s} />
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
                  {/* 手机号只显示「有 / 没有」：号码本身是敏感信息，列表一眼扫状态即可，
                  查看/修改走右键「编辑资料」；已开号徽标保留（它与手机号是两回事） */}
                  <td>
                    {s.phone ? (
                      <>
                        <span className="badge b-active" title={s.hasAccount ? '已登记手机号，且已开展示端登录账号' : '已登记手机号'}>
                          有
                        </span>
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
                        没有
                        {s.hasAccount && (
                          <span
                            className="badge b-paused"
                            style={{ marginLeft: 6 }}
                            title="已开号但没登记手机号：只能姓名登录，用不了「忘记密码」"
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
                  {/* 操作列（2026-10-16 规范）：勾选 + 账号 + 删除一行内联、靠右、不换行 */}
                  <td className="row-actions">
                    <input
                      type="checkbox"
                      checked={selected.has(s.personId)}
                      onChange={() => toggleSel(s.personId)}
                    />
                    <button onClick={() => showDetail(s.personId)}>
                      账号
                    </button>
                    <button
                      className="danger"
                      title={`删除「${s.name}」（编号 #${s.personId}）：软删，列表不再显示，历史数据保留`}
                      onClick={() => askRemove(s.personId, s.name)}
                    >
                      删除
                    </button>
                  </td>
                </tr>
              ))}
              </tbody>
            </table>
          </div>
          <Pager {...pager} />
          </>
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
                <th className="col-actions"></th>
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
                      title={`解绑「${detail.name}」（编号 #${detail.personId}）的 ${accountLabel(a)}`}
                      onClick={() => askUnbind(a)}
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
              askRemove(ctxMenu.person.personId, ctxMenu.person.name)
              setCtxMenu(null)
            }}
          >
            🗑 删除「{ctxMenu.person.name}」（软删）
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
            {/* QQ 绑定状态（2026-10-18）：只展示后端下发的脱敏值 emailMask，管理端不出现明文 QQ / 邮箱；
                绑定/解绑由主播在展示端「我的」页自助完成，管理员只做兜底改密。 */}
            {myAccPop.exists && !myAccPop.isAdmin && (
              <div className="hint" style={{ marginTop: 8 }}>
                QQ 邮箱：
                {!myAccPop.qqStatusKnown ? (
                  <span title="后端尚未下发 emailMask/qqBound，升级后自动显示">—（暂不显示绑定状态）</span>
                ) : myAccPop.qqBound ? (
                  <span title="只显示脱敏地址，管理端不显示完整 QQ 号">
                    {myAccPop.emailMask || '已绑定'}
                  </span>
                ) : myAccPop.emailMask ? (
                  <span title="历史非 QQ 邮箱：忘记密码不支持自助重置，只能联系管理员">
                    {myAccPop.emailMask}（非 QQ 邮箱，不能自助重置）
                  </span>
                ) : (
                  <span title="未绑定 QQ 的账号忘记密码只能联系管理员重置">
                    未绑定（忘记密码只能联系管理员重置）
                  </span>
                )}
                {myAccPop.mustChangePass && (
                  <span
                    className="badge b-paused"
                    style={{ marginLeft: 6 }}
                    title="该账号用临时密码登录过；展示端会提示修改密码（有效期内可重复登录，不做强制拦截）"
                  >
                    待改密
                  </span>
                )}
                <div style={{ marginTop: 4 }}>
                  绑定 / 解绑 QQ 由主播在展示端「我的」页自助完成；管理员在此改密会同时清掉「待改密」标记。
                </div>
              </div>
            )}
            <label className="hint" style={{ display: 'block', marginTop: 8 }}>
              {myAccPop.exists
                ? '新密码（4~64 字符，支持中英文/特殊字符；留空 = 不修改）'
                : '密码（4~64 字符，支持中英文/特殊字符；留空 = 按手机号后四位生成）'}
            </label>
            <input
              value={myAccPop.pass}
              placeholder="4~64 字符，任意字符均可（不自动去空格）"
              onChange={(e) => setMyAccPop({ ...myAccPop, pass: e.target.value })}
            />
            {/* 可看团别（2026-10-15）：只对**管理员观看账号**开放 —— 普通主播账号
                按绑定主播所在团走（'' 沿用旧口径），改了反而会和台账不一致。 */}
            {myAccPop.isAdmin && (
              <>
                <label className="hint" style={{ display: 'block', marginTop: 8 }}>可看团别</label>
                <select
                  value={myAccPop.viewTeam}
                  onChange={(e) => setMyAccPop({ ...myAccPop, viewTeam: e.target.value })}
                >
                  <option value="any">全团（男团女团都能看，且可自己切换）</option>
                  <option value="male">只看男团</option>
                  <option value="female">只看女团</option>
                  <option value="">沿用旧口径（按绑定主播所在团）</option>
                </select>
                <div className="hint" style={{ marginTop: 6 }}>
                  改完会立即吊销该账号的登录态（旧登录还挂着原来的团别）。
                </div>
              </>
            )}
            <div className="hint" style={{ marginTop: 8 }}>
              保存后主播在展示端「我的」页用 姓名 + 密码 登录看自己的数据；没有注册，账号只在这里设置。
              {myAccPop.exists ? ' 该主播已有账号：新密码留空则保持原密码，填写后覆盖更新。' : ''}
            </div>
            <div className="row" style={{ marginTop: 10 }}>
              {myAccPop.exists && (
                <button className="danger" onClick={askRemoveMyAccount}>
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

      {/* 危险操作确认弹窗（删除 / 批量删除 / 解绑 / 删登录账号）：
          标题 = 对象（名字 + 编号），正文 = 后果，主按钮 = 具体动作（不是「确定」）。
          失败时不关弹窗，把 error 显示在弹窗里，运营可以再点一次重试或取消。 */}
      {confirmPop && confirmCopyView && (
        <div className="mask" onClick={() => !confirmBusy && setConfirmPop(null)}>
          <div className="master-pop" onClick={(e) => e.stopPropagation()}>
            <h2 style={{ marginBottom: 0 }}>
              {confirmCopyView.title}
              <button
                className="linkish"
                style={{ float: 'right' }}
                disabled={confirmBusy}
                onClick={() => setConfirmPop(null)}
              >
                关闭
              </button>
            </h2>
            <p className="hint" style={{ margin: 0 }}>
              {confirmCopyView.body}
            </p>
            {error && <div className="alert bad">{error}</div>}
            <div className="row" style={{ marginTop: 10 }}>
              {/* 危险按钮（danger）+ 取消（ghost，固定最后一位）：见文件上方「弹窗按钮规范」 */}
              <button className="danger" disabled={confirmBusy} onClick={runConfirm}>
                {confirmBusy ? '处理中…' : confirmCopyView.label}
              </button>
              <button className="ghost" disabled={confirmBusy} onClick={() => setConfirmPop(null)}>
                取消
              </button>
            </div>
          </div>
        </div>
      )}

      {/* 合并弹窗：选一个「被并入」的主播。标题直接写「把 A 并入 B」，
          正文按后端既有语义交代账号 / 音浪 / 别名怎么处理（不写「确定要」这类空话）。 */}
      {mergePop && (
        <div className="mask" onClick={() => !merging && setMergePop(null)}>
          {/* 内联 maxHeight + 内部滚动：候选人列表本来就长（100+ 行 × .master-list 320px），
              加上「合并后怎么算」三条说明后，矮视口（实测 633px 高）会把底部主按钮/取消顶出屏幕。
              .mask 是 fixed 且不可滚动，所以让弹窗自己在视口内滚（不新增 CSS，不改全局 .master-pop）。 */}
          <div
            className="master-pop"
            style={{ maxHeight: 'calc(100vh - 96px)', overflowY: 'auto' }}
            onClick={(e) => e.stopPropagation()}
          >
            <h2 style={{ marginBottom: 0 }}>
              {mergePop.sourceId
                ? `把「${nameOf(mergePop.sourceId)}」并入「${mergePop.targetName}」（编号 #${mergePop.targetId}）`
                : `合并到「${mergePop.targetName}」（编号 #${mergePop.targetId}）`}
              <button className="linkish" style={{ float: 'right' }} onClick={() => setMergePop(null)}>
                关闭
              </button>
            </h2>
            <p className="hint">
              一个人有两个 / 三个抖音号、被建档成多条记录时：在这里选出<b>多余的记录</b>，
              它会并入「{mergePop.targetName}」，然后被软删（列表不再显示，数据保留在库里）。
            </p>
            {/* 合并后到底怎么算（2026-10-16 按后端 merge_person 的既有语义写，没有新增规则）：
                账号改绑（同 anchor 去重）、同日指标求和并入（音浪/时长求和、累计取较大值）、
                其余日期整条改绑、my_accounts 一人一号时 source 账号不动留给人工处理。 */}
            <div className="hint" style={{ marginTop: -2 }}>
              合并后：
              <br />· 账号：多余记录名下的抖音号改绑到「{mergePop.targetName}」名下；TA 已经绑过的同一个号不重复绑。
              <br />· 音浪：同一天两边都有数据时，当天音浪与时长相加（累计取较大值）；其余日期整条转到 TA 名下。
              <br />· 若「{mergePop.targetName}」已开过展示端登录账号（我的页面），多余记录的登录账号不会自动改绑，事后到「我的页面账号」里人工处理。
            </div>
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
                        {/* 重名时靠编号区分对象（台账里有重名提示） */}
                        （#{p.personId}）
                        {p.team === 'male' ? ' 男团' : p.team === 'female' ? ' 女团' : ` ${p.team}`}
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
              {/* 合并 = 破坏性（source 会被软删）→ 危险按钮走 danger；取消固定最后一位 + ghost */}
              <button className="danger" disabled={!mergePop.sourceId || merging} onClick={doMerge}>
                {merging
                  ? '合并中…'
                  : mergePop.sourceId
                    ? `把「${nameOf(mergePop.sourceId)}」并入「${mergePop.targetName}」`
                    : '先在上面选一个要合并掉的主播'}
              </button>
              <button className="ghost" disabled={merging} onClick={() => setMergePop(null)}>
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
