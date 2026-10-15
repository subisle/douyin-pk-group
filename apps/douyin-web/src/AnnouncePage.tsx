import { useCallback, useEffect, useRef, useState, type CSSProperties } from 'react'
import { api } from './api'

/** 公告配置页（2026-10-14）：从「机器人」页整体搬出来的公告 UI。状态与保存逻辑不变——
    仍由一个组件持有全部状态，一次整包 POST /api/admin/announcement。底部 ann-savebar 统一保存。
    （2026-10-14 运营要求：引导弹窗的编辑区块已下线不再渲染，但 steps / enabled 的
    state 与 load / save 保留——save 是整包 POST，把后端已有的引导配置原样回传，
    别把库里的 steps / enabled 清掉。） */

/** 引导图标白名单：与后端 pk-server/src/public_teams.rs 的 STEP_ICON_WHITELIST
    保持一致。展示端 st_page.html 里也有一份同名表做双重校验；两边同步改。 */
const ANNOUNCE_ICONS = [
  'auto_awesome', 'military_tech', 'qr_code_scanner', 'notifications_active',
  'insights', 'tips_and_updates', 'campaign', 'star', 'bolt', 'groups',
  'emoji_events', 'local_fire_department', 'trending_up', 'calendar_month',
  'chat_bubble', 'lock', 'person', 'smart_toy', 'workspace_premium',
]
/** 图标英文内名 → 运营看的中文显示名（下拉里显示「中文名（内名）」，值仍传英文名） */
const ICON_LABELS: Record<string, string> = {
  auto_awesome: '闪光',
  military_tech: '勋章',
  qr_code_scanner: '扫码',
  notifications_active: '铃铛',
  insights: '数据洞察',
  tips_and_updates: '提示灯泡',
  campaign: '喇叭公告',
  star: '星标',
  bolt: '闪电',
  groups: '群组',
  emoji_events: '奖杯',
  local_fire_department: '火焰',
  trending_up: '上升趋势',
  calendar_month: '日历',
  chat_bubble: '对话气泡',
  lock: '锁',
  person: '人形',
  smart_toy: '机器人',
  workspace_premium: '高级徽章',
}

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
const PLACEMENTS: { key: PlacementKey; label: string }[] = [
  { key: 'login', label: '登录页' },
  { key: 'home', label: '看板' },
  { key: 'my', label: '个人页' },
]

/** v2 加载前 / 加载失败时的缺省值：全部关闭 + 空文案（GET 成功后被真实数据覆盖） */
const EMPTY_PLACEMENTS: PlacementsMap = {
  login: { enabled: false, text: '', icon: '' },
  home: { enabled: false, text: '', icon: '' },
  my: { enabled: false, text: '', icon: '' },
}
const EMPTY_CELEBRATE: CelebrateCfg = { enabled: false, confetti: false, title: '', name: '', wave: '0' }

/** 展示页公告条（2026-10-14）：可配多条，数组顺序 = 展示端从上到下。
    对应后端 Announcement.items[idx]；tone：brand = 紫金渐变（原庆祝条样式）、
    plain = 白底描边（原冠军条样式）。wave 同 celebrate 存字符串方便 number 输入框，
    保存时转数字（0 = 不显示数值）。
    popup（2026-10-15 后端新增）：true = 该条除横幅外在展示端弹窗弹出一次；
    老后端不下发该字段（undefined），按 false 处理。 */
type AnnounceTone = 'brand' | 'plain'
type AnnounceItemCfg = {
  enabled: boolean
  icon: string
  title: string
  name: string
  wave: string
  text: string
  tone: AnnounceTone
  popup?: boolean
}

/** 「+ 添加公告条」的新条目：默认不启用，免得文案还没填就在展示端顶出一条空公告
    （popup 默认 false：不弹窗，只走横幅） */
const EMPTY_ANNOUNCE_ITEM: AnnounceItemCfg = {
  enabled: false,
  icon: '',
  title: '',
  name: '',
  wave: '0',
  text: '',
  tone: 'plain',
  popup: false,
}

/** 公告配置页：📢 公告位 / 📣 展示页公告条 / 🎉 庆祝配置 三个编辑区块
    共用一份状态，底部 ann-savebar 一次整包保存（引导配置只回传、不再编辑）。 */
export default function AnnouncePage() {
  const [data, setData] = useState<{ enabled: boolean; version: number } | null>(null)
  /* 引导步骤（2026-10-14 审计修复）：state 原先只声明 title/content/icon，但后端
     to_json 会下发 `target`（spotlight 指向的目标元素选择器），save 又是整包 POST ——
     于是运营在后台保存一次，每条步骤的 target 就被写成 ''，展示端所有 spotlight 引导
     退化成居中卡片（卡片文案却还写着「点分组页签」，直接误导用户）。补上该字段：
     加载时保留、保存时原样回传。 */
  const [steps, setSteps] = useState<
    { title: string; content: string; icon: string; target: string }[]
  >([])
  const [enabled, setEnabled] = useState(false)
  // 公告位 v2（登录页 / 看板 / 个人页）：受控输入，GET 后逐字段兜底填充
  const [placements, setPlacements] = useState<PlacementsMap>(EMPTY_PLACEMENTS)
  // 庆祝配置 v2：受控输入；wave 在 state 里是字符串（number 输入框好编辑），保存时转数字
  const [celebrate, setCelebrate] = useState<CelebrateCfg>(EMPTY_CELEBRATE)
  // 展示页公告条（2026-10-14）：多条列表，顺序即展示顺序
  const [items, setItems] = useState<AnnounceItemCfg[]>([])
  /** GET 里有没有 items 这个 key：老后端不下发时保存不发 items，避免整包替换把已有配置清空 */
  const [itemsLoaded, setItemsLoaded] = useState(false)
  const [msg, setMsg] = useState('')
  const [err, setErr] = useState('')
  const [busy, setBusy] = useState(false)
  /** 保存成功提示的自动消失定时器（卸载时清理，避免对已卸载组件 setState） */
  const msgTimer = useRef<number | null>(null)
  useEffect(
    () => () => {
      if (msgTimer.current) window.clearTimeout(msgTimer.current)
    },
    [],
  )

  const ctl: CSSProperties = {
    padding: '7px 10px',
    border: '1px solid var(--hairline)',
    borderRadius: 8,
    background: '#fff',
    font: 'inherit',
  }

  // 公告系统 v2：单行输入框样式，对齐上面 starTitle / starIcon 的内联写法
  // （height 34 + hairline 边框 + 圆角 8），让新区块和旧区块视觉一致
  const input34: CSSProperties = {
    display: 'block',
    width: '100%',
    height: 34,
    marginTop: 4,
    border: '1px solid var(--hairline, #EAECF0)',
    borderRadius: 8,
    padding: '0 10px',
    fontSize: 13,
  }

  const load = useCallback(() => {
    // v2：后端 to_json 全量下发 placements / celebrate；泛型只声明用到的字段
    api<{
      enabled: boolean
      steps: { title: string; content: string; icon: string; target?: string }[]
      version: number
      placements?: Partial<Record<PlacementKey, Partial<PlacementCfg>>>
      celebrate?: Partial<CelebrateCfg>
      /** 展示页公告条（2026-10-14）：全量下发（含 enabled=false），编辑初值就是它 */
      items?: {
        enabled?: boolean
        icon?: string
        title?: string
        name?: string
        wave?: number | string
        text?: string
        tone?: string
        /** 2026-10-15 后端新增；老后端不下发（undefined） */
        popup?: boolean
      }[]
    }>('/api/admin/announcement')
      .then((d) => {
        setData(d)
        setSteps(
          d.steps?.length
            ? d.steps.map((s) => ({ ...s, target: s.target ?? '' }))
            : [{ title: '', content: '', icon: '', target: '' }],
        )
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
        // 公告条初始化：数组整体替换（顺序即展示顺序）；老后端没有 items key 时保持空列表，
        // 且不标 loaded —— 保存时不发 items，防止把库里的多条整包清空。
        // popup 兜底 !!：老后端不下发该字段 = undefined → false（不弹窗）
        if (Array.isArray(d.items)) {
          setItems(
            d.items.map((it) => ({
              enabled: !!it?.enabled,
              icon: it?.icon ?? '',
              title: it?.title ?? '',
              name: it?.name ?? '',
              wave: String(it?.wave ?? 0),
              text: it?.text ?? '',
              tone: it?.tone === 'brand' ? 'brand' : 'plain',
              popup: !!it?.popup,
            })),
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

  /** 保存整个 Announcement JSON（引导 + 公告位 v2 + 庆祝 v2 + 公告条一起 POST）。
      overrides：开关类控件在同一 tick 里 setState 还没生效，把新值直接带进来
      避免用旧 state 覆盖刚点完的开关；文本类控件失焦时不传、走最新 state。 */
  async function save(overrides?: {
    enabled?: boolean
    placements?: PlacementsMap
    celebrate?: CelebrateCfg
    items?: AnnounceItemCfg[]
  }) {
    const en = overrides?.enabled ?? enabled
    const pl = overrides?.placements ?? placements
    const ce = overrides?.celebrate ?? celebrate
    const its = overrides?.items ?? items
    setBusy(true)
    setMsg('')
    setErr('') // 每次保存先清掉上一次的失败提示，避免旧错误一直挂着
    try {
      // items 是整包替换语义：只在「GET 下发过 items」或运营手动加过条目时才带上，
      // 老后端 / 未加载时省略该字段，避免把库里的多条清空
      const body: Record<string, unknown> = {
        enabled: en,
        steps,
        // 公告位 v2：三个位置原样上传，后端逐项做 enabled / 文案长度 / 图标白名单校验
        placements: pl,
        // 庆祝配置 v2：wave 转回数字（非数字 / 负数兜底 0 = 不显示）
        celebrate: { ...ce, wave: Math.max(0, Math.round(Number(ce.wave) || 0)) },
      }
      if (itemsLoaded) {
        // 展示页公告条：数组顺序 = 展示端从上到下；wave 转数字，tone 只收 brand / plain。
        // popup（2026-10-15 新增）布尔原样上传；老后端 state 里可能是 undefined → 按 false 发
        body.items = its.map((it) => ({
          enabled: it.enabled,
          icon: it.icon.trim(),
          title: it.title,
          name: it.name,
          wave: Math.max(0, Math.round(Number(it.wave) || 0)),
          text: it.text,
          tone: it.tone === 'brand' ? 'brand' : 'plain',
          popup: !!it.popup,
        }))
      }
      const d = await api<{ announcement: { version: number } }>('/api/admin/announcement', {
        method: 'POST',
        // v2：保存 = 整个 Announcement JSON 一起下发（引导 + 公告位 + 庆祝 + 公告条），
        // 后端 update_announcement 按字段合并后 version 自增（展示端重新弹出引导）。
        body: JSON.stringify(body),
      })
      setData((prev) => (prev ? { ...prev, enabled: en, version: d.announcement.version } : prev))
      setMsg('已保存（版本 ' + d.announcement.version + '，展示端会重新弹出）')
      // 定时器句柄存 ref：本页大量「失焦即保存」，改完立刻切页会留下悬挂定时器
      // 对已卸载组件 setState（2026-10-14 审计修复）。
      if (msgTimer.current) window.clearTimeout(msgTimer.current)
      msgTimer.current = window.setTimeout(() => setMsg(''), 2200)
    } catch (e: any) {
      /* 2026-10-14 审计修复：原先失败也进 msg，而 msg 渲染成绿色的 .alert.good ——
         本页到处是「失焦即保存」，保存失败被显示成绿色成功，运营改完切走才发现配置丢了。
         失败改走 err（红色 .alert.bad）。 */
      setErr(e?.message ?? String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="page ann-page">
      {/* 2026-10-14：引导弹窗编辑区块已按运营要求下线。steps / enabled 的 state 与
          load / save 保留——save 是整包 POST /api/admin/announcement，把后端已有的
          引导配置原样回传，别把库里的 steps 清掉。 */}
      {/* 配置加载失败提示（含重试）：data 为空 = 整包 GET 还没成功过 */}
      {!data && err && (
        <div className="alert warn">
          ⚠️ 配置加载失败：{err}
          <button style={{ marginLeft: 8 }} onClick={load}>
            重试
          </button>
        </div>
      )}

      {/* ============ 区块 2 · 公告位（2026-10-13） ============ */}
      <section className="card ann-sec">
        <h2 className="ann-sec-head">📢 公告位（登录页 / 看板 / 个人页）</h2>
        <p className="ann-sec-sub">
          三个位置各一条公告：开启后在展示端对应页面显示，文案支持换行（每行一条）。
          开关 / 文本框失焦即时整包保存。
        </p>
        <div className="ann-grid ann-grid-3">
          {PLACEMENTS.map(({ key, label }) => {
            const p = placements[key]
            return (
              <div
                key={key}
                style={{
                  border: '1px solid var(--hairline)',
                  borderRadius: 10,
                  padding: 10,
                  display: 'flex',
                  flexDirection: 'column',
                  gap: 6,
                }}
              >
                {/* 位置标题行：图标即时预览（Material Symbols）+ 位置名 + 启用开关 */}
                <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                  <span className="material-symbols-outlined" style={{ fontSize: 18 }}>
                    {p.icon || 'campaign'}
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
                {/* 图标：Material Symbols 图标名，失焦保存（后端有图标白名单，
                    白名单外回落默认喇叭）。给运营的中文对照见 ICON_LABELS / 下拉版本。 */}
                <input
                  placeholder="图标（可不填）：如「喇叭公告」「铃铛」「人形」，留空用默认喇叭"
                  value={p.icon}
                  onChange={(e) => setPlacements((prev) => ({ ...prev, [key]: { ...prev[key], icon: e.target.value } }))}
                  onBlur={() => save()}
                  maxLength={48}
                  style={ctl}
                />
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
              style={{ border: '1px solid var(--hairline)', borderRadius: 10, padding: 10, display: 'flex', flexDirection: 'column', gap: 6 }}
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
                {/* 图标：只列后端白名单内的 Material Symbols 名，空 = 不带图标（下拉即保存） */}
                <label style={{ fontSize: 12.5, color: 'var(--ink3)' }}>
                  图标（可不选）
                  <select value={it.icon} onChange={(e) => setItemSaved(i, { icon: e.target.value })} style={input34}>
                    <option value="">不显示图标</option>
                    {ANNOUNCE_ICONS.map((n) => (
                      <option key={n} value={n}>
                        {ICON_LABELS[n] ?? n}（{n}）
                      </option>
                    ))}
                  </select>
                </label>
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
                <label style={{ fontSize: 12.5, color: 'var(--ink3)' }}>
                  人名
                  <input
                    value={it.name}
                    placeholder="狼哲"
                    onChange={(e) => setItem(i, { name: e.target.value })}
                    onBlur={() => save()}
                    maxLength={32}
                    style={input34}
                  />
                </label>
                {/* 音浪数：数字输入，0 = 不显示；state 存字符串，保存时统一转数字 */}
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
            border: '1px solid var(--hairline)',
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

      {/* 保存区：三个编辑区块共用一次整包保存（公告位 + 公告条 + 庆祝；引导只回传不编辑） */}
      <div className="ann-savebar">
        {/* 手动保存按钮：save 现在带 overrides 参数，onClick 必须包一层箭头函数，
            否则 click 事件对象会被当成 overrides 传进去 */}
        <button className="primary" disabled={busy} onClick={() => save()}>
          {busy ? '保存中…' : '保存全部（公告位 / 公告条 / 庆祝）'}
        </button>
        {data && <span className="hint">当前版本 {data.version}</span>}
        <span style={{ fontSize: 11.5, color: 'var(--ink3)' }}>
          文本框失焦或开关切换后自动保存（公告位 + 公告条 + 庆祝整体一起保存，版本前进；
          原有引导配置会原样保留一并回传）。
        </span>
        {msg && <div className="alert good">{msg}</div>}
        {/* 2026-10-14 审计修复：保存失败现在走 err，这里补上红色提示条
            （原先错误文案被套进绿色的 .alert.good，运营以为保存成功了） */}
        {err && <div className="alert bad">{err}</div>}
      </div>
    </div>
  )
}
