import { useCallback, useEffect, useState, type CSSProperties } from 'react'
import { api } from './api'
import IconSelect from './IconSelect'

/** 公告配置页（2026-10-14）：从「机器人」页整体搬出来的公告 UI。状态与保存逻辑不变——
    仍由一个组件持有全部状态，一次整包 POST /api/admin/announcement。底部 ann-savebar 统一保存。
    （2026-10-14 运营要求：引导弹窗的编辑区块已下线不再渲染，但 steps / enabled 的
    state 与 load / save 保留——save 是整包 POST，把后端已有的引导配置原样回传，
    别把库里的 steps / enabled 清掉。） */

/** 图标白名单 / 中文名已抽到 `announceIcons.ts`（2026-10-16）：
    图标控件统一走 `<IconSelect>`，界面上只见中文名与图形，不再回显 campaign 这类内名。 */
/** 横幅对齐三选项（2026-10-16）：值 = 后端 AnnounceItem.align，图标 = 左/中/右对齐。
    「点图标即设置」——和样式/图标一样走 setItemSaved 即时保存。 */
const ANNOUNCE_ALIGNS: { v: AnnounceAlign; i: string; t: string }[] = [
  { v: 'left', i: 'format_align_left', t: '居左（原版式）' },
  { v: 'center', i: 'format_align_center', t: '居中' },
  { v: 'right', i: 'format_align_right', t: '居右' },
]

/* -------------- 公告系统 v2（2026-10-13）类型定义 -------------- */

/** 公告位 v2：单个位置（登录页 / 看板 / 个人页）的配置，结构与后端
    pk-server public_teams/settings.rs 的 Announcement.placements.<key> 一一对应
    （serde default：旧数据缺字段 = 关闭 + 空文案，load 时逐字段兜底）。 */
type PlacementCfg = { enabled: boolean; text: string; icon: string }
type PlacementKey = 'login' | 'home' | 'my'
type PlacementsMap = Record<PlacementKey, PlacementCfg>

/** 庆祝配置 v2：每日之星 / 争霸赛冠军的庆祝元素（总开关 / 彩带 / 标题 / 人名 / 音浪数）。
    wave（音浪数）后端是数字、0 = 不显示；state 里存字符串方便 number 输入框编辑，保存时转回数字。 */
type CelebrateCfg = { enabled: boolean; confetti: boolean; title: string; name: string; wave: string }

/** 公告位 v2 三个位置：key 与后端 Announcement.placements 的字段一一对应 */
/** 公告位三处（2026-10-16）：`defIcon` = 展示端该位置**留空时的默认图标**
    （st_page.html 的 annBarHtml 第三参：登录页 campaign / 看板 notifications / 个人页 person）。
    管理端头部预览按它显示，避免以前一律显示 `campaign` 造成「预览与展示端不一致」。 */
const PLACEMENTS: { key: PlacementKey; label: string; defIcon: string }[] = [
  { key: 'login', label: '登录页', defIcon: 'campaign' },
  { key: 'home', label: '看板', defIcon: 'notifications_active' },
  { key: 'my', label: '个人页', defIcon: 'person' },
]

/** v2 加载前 / 加载失败时的缺省值：全部关闭 + 空文案（GET 成功后被真实数据覆盖） */
const EMPTY_PLACEMENTS: PlacementsMap = {
  login: { enabled: false, text: '', icon: '' },
  home: { enabled: false, text: '', icon: '' },
  my: { enabled: false, text: '', icon: '' },
}
const EMPTY_CELEBRATE: CelebrateCfg = { enabled: false, confetti: false, title: '', name: '', wave: '0' }

/** 看板弹幕（2026-10-19 后端新增，2026-10-21 收敛为只滚日榜冠军）：
    后端字段仍保留 contestChampion 以兼容旧配置，但展示端已不再输出争霸赛冠军胶囊。
    字段为 { enabled, top: number, dailyChampion, contestChampion }，top 是 390 宽视口下
    弹幕位置微调偏移（-80..80；默认 0 = 前三名分数与「名次/主播」之间）。
    与 celebrate.wave 同口径：state 里存字符串方便 number 输入框编辑，保存时转回数字。 */
type DanmakuCfg = { enabled: boolean; top: string; dailyChampion: boolean; contestChampion: boolean }
const EMPTY_DANMAKU: DanmakuCfg = { enabled: false, top: '0', dailyChampion: true, contestChampion: true }

/** 展示页公告条（2026-10-14）：可配多条，数组顺序 = 展示端从上到下。
    对应后端 Announcement.items[idx]；tone：brand = 紫金渐变（原庆祝条样式）、
    plain = 白底描边（原冠军条样式）。wave 同 celebrate 存字符串方便 number 输入框，
    保存时转数字（0 = 不显示数值）。
    popup（2026-10-15 后端新增）：true = 该条除横幅外在展示端弹窗弹出一次；
    老后端不下发该字段（undefined），按 false 处理。
    align（2026-10-16 后端新增）：横幅对齐 left（默认，原版式）/ center / right，
    展示端把「图标 + 文案」整组靠中/靠右；老后端不下发 → left。
    seat / period（2026-10-16 后端新增）：三甲席位（0 冠军 / 1 亚军 / 2 季军 / -1 未指定）
    与成绩月份（YYYY-MM，空 = 不指定）。管理端「选主播 + 选月份」自动带出该月成绩，
    老后端不下发 → -1 / ''。 */
type AnnounceTone = 'brand' | 'plain'
type AnnounceAlign = 'left' | 'center' | 'right'
type AnnounceItemCfg = {
  enabled: boolean
  /** 适用团别：'' = 通用；male = 仅男团；female = 仅女团。 */
  team: '' | 'male' | 'female'
  icon: string
  title: string
  name: string
  wave: string
  text: string
  tone: AnnounceTone
  popup?: boolean
  align?: AnnounceAlign
  seat?: number
  period?: string
  /** 抖音头像同源地址（2026-10-16）：选主播时带出 `/api/public/avatars/{pid}?v={hash}`；
      空 / 404 时展示端回落首字色块。 */
  avatar?: string
}

/** 席位选项（2026-10-16）：值 = 后端 seat 字段；展示端弹窗徽章图按席位走。 */
const ANNOUNCE_SEATS: { v: number; t: string }[] = [
  { v: 0, t: '🥇 冠军' },
  { v: 1, t: '🥈 亚军' },
  { v: 2, t: '🥉 季军' },
  { v: -1, t: '不指定（按顺序）' },
]

/** 席位选项的月榜行（GET /api/admin/announcement/seat-options 下发） */
type SeatRow = { personId: number; name: string; team: string; wave: number; avatar?: string }

/** 「+ 添加公告条」的新条目：默认不启用，免得文案还没填就在展示端顶出一条空公告
    （popup 默认 false：不弹窗，只走横幅；align 默认 left：保持原版式） */
const EMPTY_ANNOUNCE_ITEM: AnnounceItemCfg = {
  enabled: false,
  team: '',
  icon: '',
  title: '',
  name: '',
  wave: '0',
  text: '',
  tone: 'plain',
  popup: false,
  align: 'left',
  seat: -1,
  period: '',
  avatar: '',
}

/** 当月 YYYY-MM（席位月份选择器的缺省值）：本机时区，与后端 seat-options 同口径。 */
function curMonth() {
  const d = new Date()
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}`
}

/** 本地时间 HH:MM:SS（保存成功时间戳用）。
    手写而不用 toLocaleTimeString：后者跟随浏览器 locale，英文环境下会变成 12 小时制
    「02:03:22 PM」，运营读不出「刚存的是哪一次」，也跟中文界面不一致。 */
function fmtClock(d: Date) {
  const p = (n: number) => String(n).padStart(2, '0')
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
}

/** 公告配置页：新手引导只保留开关；其余公告位 / 公告条 / 庆祝 / 弹幕共用保存。 */
export default function AnnouncePage() {
  const [data, setData] = useState<{ enabled: boolean; version: number } | null>(null)
  /* 新手引导内容固定在展示端；后台只维护 enabled 总开关。 */
  const [enabled, setEnabled] = useState(false)
  // 公告位 v2（登录页 / 看板 / 个人页）：受控输入，GET 后逐字段兜底填充
  const [placements, setPlacements] = useState<PlacementsMap>(EMPTY_PLACEMENTS)
  // 庆祝配置 v2：受控输入；wave 在 state 里是字符串（number 输入框好编辑），保存时转数字
  const [celebrate, setCelebrate] = useState<CelebrateCfg>(EMPTY_CELEBRATE)
  // 看板弹幕（2026-10-19 后端新增）：受控输入；top 在 state 里是字符串（同 celebrate.wave）
  const [danmaku, setDanmaku] = useState<DanmakuCfg>(EMPTY_DANMAKU)
  /** GET 里有没有 danmaku 这个 key：老后端不下发时保存不发 danmaku（同 items 的 itemsLoaded 口径） */
  const [danmakuLoaded, setDanmakuLoaded] = useState(false)
  // 展示页公告条（2026-10-14）：多条列表，顺序即展示顺序
  const [items, setItems] = useState<AnnounceItemCfg[]>([])
  /** GET 里有没有 items 这个 key：老后端不下发时保存不发 items，避免整包替换把已有配置清空 */
  const [itemsLoaded, setItemsLoaded] = useState(false)
  /** 加载失败原因（只有整包 GET 失败才写这里，顶部「配置加载失败 + 重试」用它） */
  const [err, setErr] = useState('')
  /** 保存失败原因（2026-10-16）：与 err 分开 —— 本页到处「失焦即保存」，
      保存失败必须挂在保存条上（红字 + 重试），不能被「加载失败」的文案语义混用。 */
  const [saveErr, setSaveErr] = useState('')
  /** 最近一次保存成功的本地时间；null = 本次会话还没成功保存过 / 上一次保存失败。
      持续显示不自动消失（DESIGN §7.5：缺的是「保存中 / 已保存 / 失败」的持续状态），
      连续保存时秒数会变，运营据此确认这次确实发出去了。 */
  const [savedAt, setSavedAt] = useState<Date | null>(null)
  const [busy, setBusy] = useState(false)
  /* ---- 三甲席位选人（2026-10-16 后端 seat-options）：月份 + 该月成绩榜缓存 ----
     seatPeriod：选择器当前月份（整页一份，选人时按它拉榜）；seatRows / seatLoading /
     seatErr：该月榜行 / 拉取中 / 失败原因。换月即拉，失败不阻塞手填。 */
  const [seatPeriod, setSeatPeriod] = useState(curMonth())
  const [seatRows, setSeatRows] = useState<SeatRow[]>([])
  const [seatLoading, setSeatLoading] = useState(false)
  const [seatErr, setSeatErr] = useState('')

  /** 拉某月成绩榜（失败静默置 seatErr，不阻塞手填路径）。 */
  const loadSeatRows = useCallback((period: string) => {
    setSeatLoading(true)
    setSeatErr('')
    api<{ items: SeatRow[] }>(
      `/api/admin/announcement/seat-options?period=${encodeURIComponent(period)}`,
    )
      .then((d) => {
        setSeatRows(Array.isArray(d.items) ? d.items : [])
        setSeatLoading(false)
      })
      .catch((e) => {
        setSeatRows([])
        setSeatErr(e?.message ?? String(e))
        setSeatLoading(false)
      })
  }, [])

  useEffect(() => {
    loadSeatRows(seatPeriod)
  }, [loadSeatRows, seatPeriod])

  /** 公告位字段的控件样式（2026-10-16 统一解剖）：块级 + 满宽 + 34px + 8px 圆角，
      label 在上、控件在下，与「展示页公告条」区块的字段一致（此前是行内宽度、两套尺寸）。 */
  const ctl: CSSProperties = {
    display: 'block',
    width: '100%',
    minHeight: 34,
    marginTop: 4,
    padding: '7px 10px',
    border: '1px solid var(--line)',
    borderRadius: 8,
    background: '#fff',
    font: 'inherit',
  }

  // 公告系统 v2：单行输入框样式，与全站控件层同一套 token（34px 高 + 发丝线 + 8px 圆角），
  // label 在上、控件在下（marginTop 4），让各区块字段解剖保持一致。
  const input34: CSSProperties = {
    display: 'block',
    width: '100%',
    height: 34,
    marginTop: 4,
    border: '1px solid var(--line)',
    borderRadius: 8,
    padding: '0 10px',
    fontSize: 13,
  }

  const load = useCallback(() => {
    // v2：后端 to_json 全量下发 placements / celebrate；泛型只声明用到的字段
    api<{
      enabled: boolean
      version: number
      placements?: Partial<Record<PlacementKey, Partial<PlacementCfg>>>
      celebrate?: Partial<CelebrateCfg>
      /** 看板弹幕（2026-10-19 后端新增）；老后端不下发该 key（undefined） */
      danmaku?: { enabled?: boolean; top?: number; dailyChampion?: boolean; contestChampion?: boolean }
      /** 展示页公告条（2026-10-14）：全量下发（含 enabled=false），编辑初值就是它 */
      items?: {
        enabled?: boolean
        /** 2026-10-22 后端新增：'' = 通用；male / female = 仅该团 */
        team?: string
        icon?: string
        title?: string
        name?: string
        wave?: number | string
        text?: string
        tone?: string
        /** 2026-10-15 后端新增；老后端不下发（undefined） */
        popup?: boolean
        /** 2026-10-16 后端新增：left（默认）/ center / right；老后端不下发 → left */
        align?: string
        /** 2026-10-16 后端新增：三甲席位 0/1/2；老后端不下发 → -1（未指定） */
        seat?: number
        /** 2026-10-16 后端新增：成绩月份 YYYY-MM；老后端不下发 → '' */
        period?: string
        /** 2026-10-16 后端新增：抖音头像同源地址；老后端不下发 → '' */
        avatar?: string
      }[]
    }>('/api/admin/announcement')
      .then((d) => {
        setData(d)
        setEnabled(!!d.enabled)
        // 公告位 v2 初始化：旧数据（serde default，字段可能整体缺失 / 某项缺失）逐字段兜底
        const p = d.placements ?? {}
        setPlacements({
          login: { enabled: !!p.login?.enabled, text: p.login?.text ?? '', icon: p.login?.icon ?? '' },
          home: { enabled: !!p.home?.enabled, text: p.home?.text ?? '', icon: p.home?.icon ?? '' },
          my: { enabled: !!p.my?.enabled, text: p.my?.text ?? '', icon: p.my?.icon ?? '' },
        })
        // 庆祝配置 v2 初始化：wave 数字转字符串给输入框（0 = 不显示）
        const c = d.celebrate ?? {}
        setCelebrate({
          enabled: !!c.enabled,
          confetti: !!c.confetti,
          title: c.title ?? '',
          name: c.name ?? '',
          wave: String(c.wave ?? 0),
        })
        // 看板弹幕初始化（2026-10-19 后端新增）：老后端不下发 danmaku key → 保持 EMPTY
        // 且不标 loaded（保存时不发该字段，同 items 的 itemsLoaded 口径）。两个冠军开关
        // 只认显式 false（字段缺失按默认开处理），top 缺失兜底 0（自然位）。
        if (d.danmaku) {
          setDanmaku({
            enabled: !!d.danmaku.enabled,
            top: String(d.danmaku.top ?? 0),
            dailyChampion: d.danmaku.dailyChampion !== false,
            contestChampion: d.danmaku.contestChampion !== false,
          })
          setDanmakuLoaded(true)
        } else {
          setDanmaku(EMPTY_DANMAKU)
          setDanmakuLoaded(false)
        }
        // 公告条初始化：数组整体替换（顺序即展示顺序）；老后端没有 items key 时保持空列表，
        // 且不标 loaded —— 保存时不发 items，防止把库里的多条整包清空。
        // popup 兜底 !!：老后端不下发该字段 = undefined → false（不弹窗）
        // align 同口径兜底：只认 center / right，其余（含老后端 undefined）= left（原版式）
        // seat / period（2026-10-16）兜底：seat 只认 0/1/2，其余（含老后端 undefined）= -1；
        // period 只收 YYYY-MM 形状，其余 = ''（与后端 parse_items 同一白名单）
        if (Array.isArray(d.items)) {
          setItems(
            d.items.map((it) => {
              const seat = it?.seat
              const period = String(it?.period ?? '')
              return {
                enabled: !!it?.enabled,
                team: it?.team === 'male' || it?.team === 'female' ? it.team : '',
                icon: it?.icon ?? '',
                title: it?.title ?? '',
                name: it?.name ?? '',
                wave: String(it?.wave ?? 0),
                text: it?.text ?? '',
                tone: it?.tone === 'brand' ? 'brand' : 'plain',
                popup: !!it?.popup,
                align: it?.align === 'center' ? 'center' : it?.align === 'right' ? 'right' : 'left',
                seat: seat === 0 || seat === 1 || seat === 2 ? seat : -1,
                period: /^\d{4}-\d{2}$/.test(period) ? period : '',
                avatar:
                  typeof it?.avatar === 'string' &&
                  it.avatar.startsWith('/api/public/avatars/')
                    ? it.avatar
                    : '',
              }
            }),
          )
          setItemsLoaded(true)
        } else {
          setItems([])
          setItemsLoaded(false)
        }
        setErr('')
      })
      .catch((e) => setErr(e.message ?? String(e)))
  }, [])

  useEffect(() => {
    load()
  }, [load])

  /* ---- 展示页公告条（多条，2026-10-14） ---- */

  /** 改第 i 条但不落库：文本框用，失焦时 save() 走最新 state */
  function setItem(i: number, patch: Partial<AnnounceItemCfg>) {
    setItems((prev) => prev.map((it, idx) => (idx === i ? { ...it, ...patch } : it)))
  }
  /** 改第 i 条并立即保存：开关 / 下拉用（同一 tick 里 state 还没更新，
      把算好的新数组作为 overrides 传给 save，避免旧的 items 覆盖刚点的改动） */
  function setItemSaved(i: number, patch: Partial<AnnounceItemCfg>) {
    const next = items.map((it, idx) => (idx === i ? { ...it, ...patch } : it))
    setItems(next)
    void save({ items: next })
  }
  function addItem() {
    // 一旦手动加过条目，就允许保存 items（老后端场景下也按运营意图发整包）
    setItemsLoaded(true)
    setItems((prev) => [...prev, { ...EMPTY_ANNOUNCE_ITEM }])
  }
  function removeItem(i: number) {
    const next = items.filter((_, idx) => idx !== i)
    setItems(next)
    void save({ items: next })
  }
  /** 上移 / 下移：数组顺序就是展示端从上到下的顺序 */
  function moveItem(i: number, dir: -1 | 1) {
    const j = i + dir
    if (j < 0 || j >= items.length) return
    const next = [...items]
    const [moved] = next.splice(i, 1)
    next.splice(j, 0, moved)
    setItems(next)
    void save({ items: next })
  }

  /** 保存公告配置（新手引导只提交 enabled；公告位 / 庆祝 / 公告条一起 POST）。
      overrides：开关类控件在同一 tick 里 setState 还没生效，把新值直接带进来
      避免用旧 state 覆盖刚点完的开关；文本类控件失焦时不传、走最新 state。 */
  async function save(overrides?: {
    enabled?: boolean
    placements?: PlacementsMap
    celebrate?: CelebrateCfg
    danmaku?: DanmakuCfg
    items?: AnnounceItemCfg[]
  }) {
    const en = overrides?.enabled ?? enabled
    const pl = overrides?.placements ?? placements
    const ce = overrides?.celebrate ?? celebrate
    const dm = overrides?.danmaku ?? danmaku
    const its = overrides?.items ?? items
    setBusy(true)
    setSaveErr('') // 每次保存先清掉上一次的失败提示，避免旧错误一直挂着
    try {
      // items 是整包替换语义：只在「GET 下发过 items」或运营手动加过条目时才带上，
      // 老后端 / 未加载时省略该字段，避免把库里的多条清空
      const body: Record<string, unknown> = {
        enabled: en,
        // 公告位 v2：三个位置原样上传，后端逐项做 enabled / 文案长度 / 图标白名单校验
        placements: pl,
        // 庆祝配置 v2：wave 转回数字（非数字 / 负数兜底 0 = 不显示）
        celebrate: { ...ce, wave: Math.max(0, Math.round(Number(ce.wave) || 0)) },
      }
      if (danmakuLoaded) {
        // 看板弹幕（2026-10-19 后端新增，部分更新语义）：只在 GET 下发过 danmaku 才带上，
        // 老后端（不下发该 key）保存时不发，保持「不下发就不回传」的一致性（同 items 口径）。
        // top 转数字：空 / 非数字兜底 0（自然位）；后端还会 clamp -80..=80，
        // 保存侧只把「改坏成负数」的输入抬回 0，不挡合法值（含 0 本身）。
        // 注意关着也回传 state 里的 top —— 部分更新下带了字段就写库，
        // 发硬编码 0 会把「关着但自定义过位置」的值悄悄覆盖掉。
        body.danmaku = {
          enabled: dm.enabled,
          top: Math.max(-80, Math.min(80, Math.round(Number(dm.top) || 0))),
          dailyChampion: !!dm.dailyChampion,
          contestChampion: !!dm.contestChampion,
        }
      }
      if (itemsLoaded) {
        // 展示页公告条：数组顺序 = 展示端从上到下；wave 转数字，tone 只收 brand / plain。
        // popup（2026-10-15 新增）布尔原样上传；老后端 state 里可能是 undefined → 按 false 发
        // align（2026-10-16 新增）只发 left / center / right，后端再走一遍归一白名单
        // seat / period（2026-10-16 新增）：seat 只发 -1/0/1/2，period 只发 YYYY-MM（空串可发）
        body.items = its.map((it) => ({
          enabled: it.enabled,
          team: it.team === 'male' || it.team === 'female' ? it.team : '',
          icon: it.icon.trim(),
          title: it.title,
          name: it.name,
          wave: Math.max(0, Math.round(Number(it.wave) || 0)),
          text: it.text,
          tone: it.tone === 'brand' ? 'brand' : 'plain',
          popup: !!it.popup,
          align: it.align === 'center' ? 'center' : it.align === 'right' ? 'right' : 'left',
          seat: it.seat === 0 || it.seat === 1 || it.seat === 2 ? it.seat : -1,
          period: /^\d{4}-\d{2}$/.test(String(it.period ?? '')) ? it.period : '',
          avatar: String(it.avatar ?? '').startsWith('/api/public/avatars/') ? it.avatar : '',
        }))
      }
      const d = await api<{ announcement: { version: number } }>('/api/admin/announcement', {
        method: 'POST',
        // 保存 = 引导开关 + 公告位 + 庆祝 + 公告条；引导步骤由展示端固定，不再回传。
        body: JSON.stringify(body),
      })
      setData((prev) => (prev ? { ...prev, enabled: en, version: d.announcement.version } : prev))
      /* 保存成功：记下本地时间，紧挨按钮持续显示「已保存 · HH:MM:SS」。
         不做「无改动就禁用按钮」的改动检测——要深比较整包 state（含 steps / 公告条数组），
         成本高于收益；连续保存时时间戳逐次变化，运营照样能确认这一次真发出去了。
         原先这里是绿色 .alert.good + 2.2s 自动消失，运营切页回来就看不到结果了。 */
      setSavedAt(new Date())
    } catch (e: any) {
      /* 2026-10-14 审计修复：原先失败也进 msg，而 msg 渲染成绿色的 .alert.good ——
         本页到处是「失焦即保存」，保存失败被显示成绿色成功，运营改完切走才发现配置丢了。
         2026-10-16：失败挂在保存条上（红字「保存失败：原因」+ 重试按钮），
         且不清空任何已填内容 —— state 原样保留，点「重试」就是重发同一份配置。 */
      setSaveErr(e?.message ?? String(e))
      // 上一次的成功时间不再代表当前这份内容，清掉避免误读成「已经存好了」
      setSavedAt(null)
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="page ann-page">
      {/* 新手引导不在此页编辑内容，仅保留总开关。 */}
      {/* 配置加载失败提示（含重试）：data 为空 = 整包 GET 还没成功过 */}
      {!data && err && (
        <div className="alert warn">
          ⚠️ 配置加载失败：{err}
          <button style={{ marginLeft: 8 }} onClick={load}>
            重试
          </button>
        </div>
      )}

      {/* 新手引导：内容与步骤固定在展示端，后台只保留总开关。 */}
      {data && (
        <section className="card ann-sec">
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 16 }}>
            <div>
              <h2 style={{ margin: 0 }}>新手引导</h2>
              <p className="hint" style={{ margin: '6px 0 0' }}>
                开启后按用户记录展示时间；同一用户 7 天内只提示一次。
              </p>
            </div>
            <label style={{ display: 'inline-flex', alignItems: 'center', gap: 8, whiteSpace: 'nowrap' }}>
              <input
                className="switch"
                type="checkbox"
                checked={enabled}
                onChange={(e) => {
                  const next = e.target.checked
                  setEnabled(next)
                  void save({ enabled: next })
                }}
              />
              <span>{enabled ? '已开启' : '已关闭'}</span>
            </label>
          </div>
        </section>
      )}

      {/* ============ 区块 2 · 公告位（2026-10-13） ============ */}
      <section className="card ann-sec">
        <h2 className="ann-sec-head">📢 公告位（登录页 / 看板 / 个人页）</h2>
        <p className="ann-sec-sub">
          三个位置各一条公告：开启后在展示端对应页面显示，文案支持换行（每行一条）。
          开关 / 文本框失焦即时整包保存。
        </p>
        <div className="ann-grid ann-grid-3">
          {PLACEMENTS.map(({ key, label, defIcon }) => {
            const p = placements[key]
            return (
              <div
                key={key}
                style={{
                  border: '1px solid var(--line)',
                  borderRadius: 10,
                  padding: 10,
                  display: 'flex',
                  flexDirection: 'column',
                  gap: 6,
                }}
              >
                {/* 位置标题行：图标即时预览（Material Symbols）+ 位置名 + 启用开关。
                    预览用**该位置的真实默认图标**（展示端 annBarHtml 的 defIcon），
                    与原来一律显示 campaign 不同 —— 空图标时所见即展示端所得。 */}
                <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                  <span className="material-symbols-outlined" style={{ fontSize: 18 }} aria-hidden="true">
                    {p.icon || defIcon}
                  </span>
                  <b style={{ fontSize: 13 }}>{label}</b>
                  <label style={{ marginLeft: 'auto', display: 'flex', alignItems: 'center', gap: 6, cursor: 'pointer', fontSize: 12, color: 'var(--ink3)' }}>
                    启用
                    {/* 开关即时保存：同 tick 内 state 未更新，把新值作为 overrides
                        传给 save，避免保存时旧值覆盖刚点完的开关 */}
                    <input
                      type="checkbox"
                      className="switch"
                      checked={p.enabled}
                      onChange={(e) => {
                        const next = { ...placements, [key]: { ...placements[key], enabled: e.target.checked } }
                        setPlacements(next)
                        save({ placements: next })
                      }}
                    />
                  </label>
                </div>
                {/* 文案：多行，支持换行（展示端按换行渲染多行公告） */}
                <textarea
                  placeholder={'公告文案（支持换行）\n每行一条，展示端原样换行显示'}
                  value={p.text}
                  onChange={(e) => setPlacements((prev) => ({ ...prev, [key]: { ...prev[key], text: e.target.value } }))}
                  onBlur={() => save()}
                  rows={3}
                  maxLength={300}
                  style={{ ...ctl, resize: 'vertical', lineHeight: 1.6 }}
                />
                {/* 图标（2026-10-16 改）：原来是自由文本输入，运营看到的是回显的
                    `campaign` / `person` 内名（用户点单去掉）。现在只给白名单内的
                    中文名 + 图形预览，选完即时保存；留空 = 展示端用该位置的默认图标。 */}
                <label style={{ fontSize: 12.5, color: 'var(--ink3)' }}>
                  图标
                  <IconSelect
                    value={p.icon}
                    onChange={(v) => {
                      const next = { ...placements, [key]: { ...placements[key], icon: v } }
                      setPlacements(next)
                      save({ placements: next })
                    }}
                    emptyLabel="不显示（用默认图标）"
                    style={{ marginTop: 4 }}
                  />
                </label>
              </div>
            )
          })}
        </div>
      </section>

      {/* ============ 区块 3 · 展示页公告条（2026-10-14） ============ */}
      <section className="card ann-sec">
        <h2 className="ann-sec-head">📣 展示页公告条（多条）</h2>
        <p className="ann-sec-sub">
          这里的每一条 = 展示端顶部一条公告，数组顺序即展示顺序（用「↑ / ↓」调）。
          文本框失焦 / 开关切换后自动整体保存；「启用」关掉的条目会留在配置里，但展示端不显示。
          {/* 2026-10-15 后端新增 popup 字段说明 */}
          开启弹窗后，该条公告在展示端除横幅外还会以弹窗形式弹出一次（直到下次保存）。
        </p>
        <p className="hint" style={{ margin: 0 }}>
          样式「紫金渐变」= 原庆祝条的紫底金字，样式「白底描边」= 原冠军条的白底带边框。
        </p>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
          {items.length === 0 && <span className="hint">还没有公告条，点下面的「+ 添加公告条」新建一条。</span>}
          {items.map((it, i) => (
            <div
              key={i}
              style={{ border: '1px solid var(--line)', borderRadius: 10, padding: 10, display: 'flex', flexDirection: 'column', gap: 6 }}
            >
              <div style={{ display: 'flex', alignItems: 'center', gap: 6, flexWrap: 'wrap' }}>
                <span className="material-symbols-outlined" style={{ fontSize: 18 }}>
                  {it.icon || 'campaign'}
                </span>
                <b style={{ fontSize: 13 }}>第 {i + 1} 条</b>
                <label style={{ marginLeft: 'auto', display: 'flex', alignItems: 'center', gap: 6, cursor: 'pointer', fontSize: 12, color: 'var(--ink3)' }}>
                  启用
                  {/* 开关即时保存：新数组作为 overrides 传给 save，避免旧 state 覆盖 */}
                  <input
                    type="checkbox"
                    className="switch"
                    checked={it.enabled}
                    onChange={(e) => setItemSaved(i, { enabled: e.target.checked })}
                  />
                </label>
                {/* 弹窗提醒（2026-10-15 后端新增 popup）：开启后该条除横幅外还会在展示端
                    弹窗弹出一次；同样走 setItemSaved 即时保存，模式与「启用」开关一致 */}
                <label style={{ display: 'flex', alignItems: 'center', gap: 6, cursor: 'pointer', fontSize: 12, color: 'var(--ink3)' }}>
                  弹窗提醒
                  <input
                    type="checkbox"
                    className="switch"
                    checked={!!it.popup}
                    onChange={(e) => setItemSaved(i, { popup: e.target.checked })}
                  />
                </label>
                <button className="ghost" title="上移" disabled={i === 0} onClick={() => moveItem(i, -1)}>
                  ↑
                </button>
                <button className="ghost" title="下移" disabled={i === items.length - 1} onClick={() => moveItem(i, 1)}>
                  ↓
                </button>
                <button className="ghost" onClick={() => removeItem(i)}>
                  删除
                </button>
              </div>
              <div className="ann-grid ann-grid-fields">
                {/* 适用团别（2026-10-22）：争霸赛等单团内容钉到男/女团；
                    通用公告留空，两个团都会显示。下拉即保存。 */}
                <label style={{ fontSize: 12.5, color: 'var(--ink3)' }}>
                  适用团别
                  <select
                    value={it.team ?? ''}
                    onChange={(e) =>
                      setItemSaved(i, {
                        team: e.target.value === 'male' ? 'male' : e.target.value === 'female' ? 'female' : '',
                      })
                    }
                    style={input34}
                  >
                    <option value="">全部团别</option>
                    <option value="male">仅男团</option>
                    <option value="female">仅女团</option>
                  </select>
                </label>
                {/* 样式：brand = 紫金渐变，plain = 白底描边（下拉即保存） */}
                <label style={{ fontSize: 12.5, color: 'var(--ink3)' }}>
                  样式
                  <select
                    value={it.tone}
                    onChange={(e) => setItemSaved(i, { tone: e.target.value === 'brand' ? 'brand' : 'plain' })}
                    style={input34}
                  >
                    <option value="brand">紫金渐变（原庆祝条样式）</option>
                    <option value="plain">白底描边（原冠军条样式）</option>
                  </select>
                </label>
                {/* 图标：与公告位共用同一个选择器（只列白名单中文名 + 图形预览，选完即存） */}
                <label style={{ fontSize: 12.5, color: 'var(--ink3)' }}>
                  图标
                  <IconSelect
                    value={it.icon}
                    onChange={(v) => setItemSaved(i, { icon: v })}
                    style={{ marginTop: 4 }}
                  />
                </label>
                {/* 对齐（2026-10-16 后端新增 align）：三个对齐图标 = 居左/居中/居右，点了即保存。
                    center/right 时展示端把「图标 + 文案」整组靠中/靠右；left = 原版式（默认） */}
                <div style={{ fontSize: 12.5, color: 'var(--ink3)' }}>
                  对齐
                  <div style={{ display: 'flex', gap: 4, marginTop: 2 }}>
                    {ANNOUNCE_ALIGNS.map((o) => {
                      const on = (it.align ?? 'left') === o.v
                      return (
                        <button
                          key={o.v}
                          type="button"
                          className="ghost"
                          title={o.t}
                          aria-pressed={on}
                          onClick={() => setItemSaved(i, { align: o.v })}
                          style={{
                            width: 36,
                            height: 30,
                            padding: 0,
                            display: 'inline-flex',
                            alignItems: 'center',
                            justifyContent: 'center',
                            /* 管理端的变量名是 --brand/--line（不是展示端的 --primary/--hairline） */
                            border: `1px solid ${on ? 'var(--brand)' : 'var(--line)'}`,
                            background: on ? 'var(--brand)' : 'transparent',
                            color: on ? '#fff' : 'var(--ink3)',
                          }}
                        >
                          <span className="material-symbols-outlined" style={{ fontSize: 16 }}>
                            {o.i}
                          </span>
                        </button>
                      )
                    })}
                  </div>
                </div>
                <label style={{ fontSize: 12.5, color: 'var(--ink3)' }}>
                  标题
                  <input
                    value={it.title}
                    placeholder="初级争霸赛冠军"
                    onChange={(e) => setItem(i, { title: e.target.value })}
                    onBlur={() => save()}
                    maxLength={64}
                    style={input34}
                  />
                </label>
                {/* 三甲席位（2026-10-16 后端 seat）：冠军/亚军/季军，展示端弹窗徽章图按它走；
                    下拉即保存（与样式/图标同口径）。「不指定」= 按数组顺序兜底 */}
                <label style={{ fontSize: 12.5, color: 'var(--ink3)' }}>
                  席位
                  <select
                    value={it.seat ?? -1}
                    onChange={(e) => {
                      const v = Number(e.target.value)
                      setItemSaved(i, { seat: v === 0 || v === 1 || v === 2 ? v : -1 })
                    }}
                    style={input34}
                  >
                    {ANNOUNCE_SEATS.map((o) => (
                      <option key={o.v} value={o.v}>
                        {o.t}
                      </option>
                    ))}
                  </select>
                </label>
                {/* 选主播（2026-10-16 用户口径「直接选择主播 x 月成绩」）：选月份拉当月榜，
                    选人自动带出人名 + 成绩（可再手改）；period 一并保存记录成绩口径 */}
                <label style={{ fontSize: 12.5, color: 'var(--ink3)' }}>
                  成绩月份（选人自动带出成绩）
                  <input
                    type="month"
                    value={/^\d{4}-\d{2}$/.test(it.period ?? '') ? it.period! : seatPeriod}
                    onChange={(e) => {
                      const p = e.target.value
                      if (/^\d{4}-\d{2}$/.test(p)) {
                        setItem(i, { period: p })
                        setSeatPeriod(p)
                      }
                    }}
                    onBlur={() => save()}
                    style={input34}
                  />
                </label>
                <label style={{ fontSize: 12.5, color: 'var(--ink3)' }}>
                  选主播（{seatPeriod} 月榜）
                  <select
                    value=""
                    disabled={seatLoading || seatRows.length === 0}
                    onChange={(e) => {
                      const row = seatRows.find((r) => String(r.personId) === e.target.value)
                      if (!row) return
                      // 选人即带出：人名 + 该月音浪 + 成绩月份 + 抖音头像（period 记录口径）
                      setItemSaved(i, {
                        name: row.name,
                        wave: String(row.wave),
                        period: /^\d{4}-\d{2}$/.test(it.period ?? '') ? it.period : seatPeriod,
                        avatar: row.avatar ?? '',
                      })
                    }}
                    style={input34}
                  >
                    <option value="">
                      {seatLoading
                        ? '成绩加载中…'
                        : seatErr
                          ? `加载失败（可手填）：${seatErr}`
                          : seatRows.length === 0
                            ? '该月暂无成绩数据'
                            : `选人自动填人名与成绩（共 ${seatRows.length} 人）`}
                    </option>
                    {seatRows.map((r) => (
                      <option key={r.personId} value={String(r.personId)}>
                        {r.name} · {r.team === 'male' ? '男团' : r.team === 'female' ? '女团' : '未分团'} · {r.wave.toLocaleString('zh-CN')}
                      </option>
                    ))}
                  </select>
                </label>
                <label style={{ fontSize: 12.5, color: 'var(--ink3)' }}>
                  人名（可手改）
                  <input
                    value={it.name}
                    placeholder="狼哲"
                    onChange={(e) => setItem(i, { name: e.target.value })}
                    onBlur={() => save()}
                    maxLength={32}
                    style={input34}
                  />
                </label>
                {/* 音浪数：数字输入，0 = 不显示；选主播自动带出，也可手填覆盖 */}
                <label style={{ fontSize: 12.5, color: 'var(--ink3)' }}>
                  音浪（0 = 不显示）
                  <input
                    type="number"
                    min={0}
                    value={it.wave}
                    placeholder="285917"
                    onChange={(e) => setItem(i, { wave: e.target.value })}
                    onBlur={() => save()}
                    style={input34}
                  />
                </label>
              </div>
              <textarea
                placeholder="公告正文（支持换行）"
                value={it.text}
                onChange={(e) => setItem(i, { text: e.target.value })}
                onBlur={() => save()}
                rows={2}
                maxLength={300}
                style={{ ...ctl, resize: 'vertical', lineHeight: 1.6 }}
              />
            </div>
          ))}
          <button className="ghost" onClick={addItem}>
            + 添加公告条
          </button>
        </div>
      </section>

      {/* ============ 区块 4 · 庆祝配置（2026-10-13） ============ */}
      <section className="card ann-sec">
        <h2 className="ann-sec-head">🎉 庆祝配置（每日之星 / 争霸赛冠军）</h2>
        <p className="ann-sec-sub">
          展示端庆祝横幅：总开关关掉 = 完全不渲染，其余字段保留但不生效；
          「登录后彩带动画」需要总开关开着才生效。
        </p>
        <div
          style={{
            border: '1px solid var(--line)',
            borderRadius: 10,
            padding: 10,
            display: 'grid',
            gap: 8,
          }}
        >
          {/* 总开关：开启庆祝（关 = 展示端不渲染庆祝横幅，其余字段保留但不生效） */}
          <label style={{ display: 'flex', alignItems: 'center', gap: 8, cursor: 'pointer', fontSize: 12.5 }}>
            <input
              type="checkbox"
              className="switch"
              checked={celebrate.enabled}
              onChange={(e) => {
                // 开关即时保存：新值作为 overrides 传入，避免旧 state 覆盖
                const next = { ...celebrate, enabled: e.target.checked }
                setCelebrate(next)
                save({ celebrate: next })
              }}
            />
            开启庆祝
          </label>
          {/* 彩带开关：登录后彩带动画（需总开关开着才生效） */}
          <label style={{ display: 'flex', alignItems: 'center', gap: 8, cursor: 'pointer', fontSize: 12.5 }}>
            <input
              type="checkbox"
              className="switch"
              checked={celebrate.confetti}
              onChange={(e) => {
                // 开关即时保存：新值作为 overrides 传入，避免旧 state 覆盖
                const next = { ...celebrate, confetti: e.target.checked }
                setCelebrate(next)
                save({ celebrate: next })
              }}
            />
            登录后彩带动画
          </label>
          <div className="ann-grid ann-grid-fields">
            {/* 庆祝标题：展示端庆祝横幅的大字（placeholder 对齐需求：初级争霸赛冠军） */}
            <label style={{ fontSize: 12.5, color: 'var(--ink3)' }}>
              庆祝标题
              <input
                value={celebrate.title}
                placeholder="初级争霸赛冠军"
                onChange={(e) => setCelebrate((prev) => ({ ...prev, title: e.target.value }))}
                onBlur={() => save()}
                maxLength={64}
                style={input34}
              />
            </label>
            {/* 人名：庆祝的主角（placeholder 对齐需求：狼哲） */}
            <label style={{ fontSize: 12.5, color: 'var(--ink3)' }}>
              人名
              <input
                value={celebrate.name}
                placeholder="狼哲"
                onChange={(e) => setCelebrate((prev) => ({ ...prev, name: e.target.value }))}
                onBlur={() => save()}
                maxLength={32}
                style={input34}
              />
            </label>
            {/* 音浪数：数字输入，0 = 不显示；state 存字符串，保存时统一转数字 */}
            <label style={{ fontSize: 12.5, color: 'var(--ink3)' }}>
              音浪数（0 = 不显示）
              <input
                type="number"
                min={0}
                value={celebrate.wave}
                placeholder="285917"
                onChange={(e) => setCelebrate((prev) => ({ ...prev, wave: e.target.value }))}
                onBlur={() => save()}
                style={input34}
              />
            </label>
          </div>
        </div>
      </section>

      {/* ============ 区块 5 · 看板弹幕（2026-10-19 后端新增） ============ */}
      <section className="card ann-sec">
        <h2 className="ann-sec-head">🎈 看板弹幕</h2>
        <p className="ann-sec-sub">
          展示页「前三名分数」与「名次/主播」之间的飘字胶囊，只滚当天榜单冠军；位置可上下微调（默认 0 = 自然位）。
        </p>
        <div
          style={{
            border: '1px solid var(--line)',
            borderRadius: 10,
            padding: 10,
            display: 'grid',
            gap: 8,
          }}
        >
          {/* 总开关：展示端渲染弹幕轨道（关 = 完全不渲染，其余字段保留但不生效） */}
          <label style={{ display: 'flex', alignItems: 'center', gap: 8, cursor: 'pointer', fontSize: 12.5 }}>
            <input
              type="checkbox"
              className="switch"
              checked={danmaku.enabled}
              onChange={(e) => {
                // 开关即时保存：新值作为 overrides 传入，避免旧 state 覆盖
                const next = { ...danmaku, enabled: e.target.checked }
                setDanmaku(next)
                save({ danmaku: next })
              }}
            />
            开启看板弹幕
          </label>
          <div className="ann-grid ann-grid-fields">
            {/* 显示每日冠军：榜单每日之星的飘字胶囊开关（切换即保存，同上） */}
            <label style={{ display: 'flex', alignItems: 'center', gap: 8, cursor: 'pointer', fontSize: 12.5 }}>
              <input
                type="checkbox"
                className="switch"
                checked={danmaku.dailyChampion}
                onChange={(e) => {
                  const next = { ...danmaku, dailyChampion: e.target.checked }
                  setDanmaku(next)
                  save({ danmaku: next })
                }}
              />
              显示每日冠军
            </label>
            {/* 位置微调：number 输入，state 存字符串（同 celebrate.wave），失焦转数字保存；
                后端 clamp -80..=80，这里只做提示，保存侧兜底见 save() */}
            <label style={{ fontSize: 12.5, color: 'var(--ink3)' }}>
              位置微调 offset(px)（-80 ~ 80，负=上移，正=下移，默认 0）
              <input
                type="number"
                min={-80}
                max={80}
                value={danmaku.top}
                placeholder="0"
                onChange={(e) => setDanmaku((prev) => ({ ...prev, top: e.target.value }))}
                onBlur={() => save()}
                style={input34}
              />
            </label>
          </div>
        </div>
      </section>

      {/* 保存区：三个编辑区块共用一次整包保存（公告位 + 公告条 + 庆祝；引导只回传不编辑） */}
      <div className="ann-savebar">
        {/* 手动保存按钮：save 现在带 overrides 参数，onClick 必须包一层箭头函数，
            否则 click 事件对象会被当成 overrides 传进去。
            保存中：disabled + 文案变「保存中…」（DESIGN §6：disabled 必须能看出是哪种状态）。
            busy 时按钮不可点 = 不会重入，连续保存只能一次一次来，时间戳也就一次一次更新。 */}
        <button className="primary" disabled={busy} aria-busy={busy} onClick={() => save()}>
          {busy ? '保存中…' : '保存全部（公告位 / 公告条 / 庆祝 / 弹幕）'}
        </button>
        {/* 保存成功：按钮旁持续显示「已保存 · HH:MM:SS」（本地时间，24 小时制）。
            状态变化用图标 + 文本，不只靠颜色（DESIGN §4.6）；role=status 让读屏也能收到。
            不自动消失：本页到处「失焦即保存」，一闪而过的提示等于没有。 */}
        {savedAt && (
          <span
            role="status"
            style={{
              display: 'inline-flex',
              alignItems: 'center',
              gap: 4,
              fontSize: 12.5,
              color: 'var(--good)',
              whiteSpace: 'nowrap',
            }}
          >
            <span className="material-symbols-outlined" style={{ fontSize: 16 }} aria-hidden="true">
              check_circle
            </span>
            已保存 · {fmtClock(savedAt)}
          </span>
        )}
        {data && <span className="hint">当前版本 {data.version}</span>}
        <span style={{ fontSize: 11.5, color: 'var(--ink3)' }}>
          文本框失焦或开关切换后自动保存（公告位 + 公告条 + 庆祝 + 弹幕整体一起保存）。
          新手引导只由上方总开关控制，同一用户 7 天内不会重复提示。
        </span>
        {/* 保存失败：红字原因 + 「重试」按钮（DESIGN §4.5：失败给重试入口与原因，不给「出错了」）。
            已在页面上的内容一个字段都不清 —— 失败后 state 保持原样，重试就是重发同一份。
            独占一行（flex 1 1 100%）与既有 .ann-savebar > .alert 的排版一致。 */}
        {saveErr && (
          <span
            role="alert"
            style={{
              flex: '1 1 100%',
              display: 'inline-flex',
              alignItems: 'center',
              flexWrap: 'wrap',
              gap: 8,
              fontSize: 12.5,
              color: 'var(--bad)',
            }}
          >
            <span className="material-symbols-outlined" style={{ fontSize: 16 }} aria-hidden="true">
              error
            </span>
            保存失败：{saveErr}
            <button onClick={() => save()} disabled={busy}>
              重试
            </button>
            <span className="hint" style={{ color: 'var(--ink3)' }}>
              已填内容没有丢，可直接重试；连续失败先把上面红字原因发给技术。
            </span>
          </span>
        )}
      </div>
    </div>
  )
}
