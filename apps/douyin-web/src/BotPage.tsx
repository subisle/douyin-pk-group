import { useCallback, useEffect, useRef, useState } from 'react'
import { api } from './api'
import { useAutoRefresh } from './useAutoRefresh'

const PHASE_LABEL: Record<string, string> = {
  idle: '未启动',
  connecting: '连接中',
  ready: '已连接',
  running: '运行中',
  error: '出错',
  stopped: '已停止',
  awaiting_scan: '等待扫码',
  session_expired: '登录过期',
  disconnected: '未连接',
}

export default function BotPage() {
  const [status, setStatus] = useState<any>(null)
  const [sessions, setSessions] = useState<any[]>([])
  const [error, setError] = useState('')
  const [qr, setQr] = useState<{ qrcode: string; imgContent?: string; url?: string } | null>(null)
  const [qrStatus, setQrStatus] = useState<string>('')
  const [needVerify, setNeedVerify] = useState(false)
  const [verifyCode, setVerifyCode] = useState('')
  /** 轮询闭包里读的最新值（state 在 setInterval 闭包里是旧值） */
  const needVerifyRef = useRef(false)
  const verifyCodeRef = useRef('')
  useEffect(() => {
    needVerifyRef.current = needVerify
  }, [needVerify])
  useEffect(() => {
    verifyCodeRef.current = verifyCode
  }, [verifyCode])
  const [manualToken, setManualToken] = useState('')
  const pollRef = useRef<number | null>(null)
  /** QQ 机器人凭据（AppID 明文回显，Secret 只回是否已设置） */
  const [qqCred, setQqCred] = useState<{ appId: string; secretSet: boolean; source: string } | null>(null)
  const [qqAppId, setQqAppId] = useState('')
  const [qqAppSecret, setQqAppSecret] = useState('')
  const [credMsg, setCredMsg] = useState('')

  /** 「我的」页微信推送：机器人加好友二维码（展示端给用户扫） */
  const [addQrSet, setAddQrSet] = useState(false)
  const [addQrMsg, setAddQrMsg] = useState('')
  const addQrFileRef = useRef<HTMLInputElement | null>(null)
  /** 延迟推送队列（运营查有没有推出去 / 立刻催一次） */
  const [pushJobs, setPushJobs] = useState<any>(null)
  const [pushMsg, setPushMsg] = useState('')
  /** 推送队列加载失败（P2-5）：不再渲染成「还没有推送任务」的假空态 */
  const [pushError, setPushError] = useState('')
  const [pushBusy, setPushBusy] = useState<string | null>(null)

  const loadPushJobs = useCallback(() => {
    api('/api/admin/push/jobs')
      .then(setPushJobs)
      .catch((e) => {
        // P2-5：失败显示出来（空态文案会在下面提示「还没有推送任务」，加一条错误态避免误导）
        console.error('推送队列加载失败', e)
        setPushJobs(null)
        setPushMsg('')
        setPushError(String(e?.message ?? e))
      })
  }, [])

  useEffect(() => {
    loadPushJobs()
    const t = setInterval(loadPushJobs, 30000)
    return () => clearInterval(t)
  }, [loadPushJobs])

  async function runPushNow(team: string, bizDate: string) {
    setPushBusy(`${team}:${bizDate}`)
    setPushMsg('')
    try {
      await api(
        `/api/admin/push/run?team=${encodeURIComponent(team)}&bizDate=${encodeURIComponent(bizDate)}`,
        { method: 'POST' },
      )
      setPushMsg(`已立即推送：${team === 'female' ? '女团' : '男团'} ${bizDate}`)
      loadPushJobs()
    } catch (e: any) {
      setPushMsg(e.message ?? String(e))
    } finally {
      setPushBusy(null)
    }
  }

  const load = useCallback(async () => {
    try {
      const [s, sess, cred, addQr] = await Promise.all([
        api('/api/admin/bots/status'),
        api('/api/admin/bots/sessions'),
        api<{ appId: string; secretSet: boolean; source: string }>('/api/admin/bots/qq/credentials'),
        api<{ configured: boolean }>('/api/admin/bots/weixin/add-qr'),
      ])
      setStatus(s)
      setSessions(sess.items ?? [])
      setQqCred(cred)
      setAddQrSet(!!addQr.configured)
      // 已保存过的 AppID 带出来（Secret 不回传，placeholder 显示「已设置」）
      setQqAppId((cur) => cur || cred.appId)
    } catch (e: any) {
      console.error('机器人状态加载失败', e)
      setError(e.message ?? String(e))
    }
  }, [])

  useEffect(() => {
    load()
    // 20s 兜底轮询（看连接是否还活着）；消息到达靠 SSE 即时刷新
    const t = setInterval(load, 20000)
    return () => clearInterval(t)
  }, [load])

  // 机器人收到消息 / 有导入时立刻刷新状态与会话列表
  useAutoRefresh(load, ['bot', 'import'])

  async function saveAddQr(dataUrl: string) {
    setAddQrMsg('')
    try {
      const d = await api<{ configured: boolean }>('/api/admin/bots/weixin/add-qr', {
        method: 'POST',
        body: JSON.stringify({ qr: dataUrl }),
      })
      setAddQrSet(!!d.configured)
      setAddQrMsg(d.configured ? '已保存，展示端「我的」页会显示这张码' : '已清除')
    } catch (e: any) {
      setAddQrMsg(e.message ?? String(e))
    }
  }

  /** 图片压缩成 dataURL 再上传：二维码最长边 720、白底铺满，控制体积 */
  async function pickAddQr(file: File) {
    let dataUrl = ''
    try {
      dataUrl = await new Promise<string>((resolve, reject) => {
        const fr = new FileReader()
        fr.onerror = () => reject(new Error('读图失败'))
        fr.onload = () => {
          const img = new Image()
          img.onerror = () => reject(new Error('图片读不出来，换一张试试'))
          img.onload = () => {
            const max = 720
            const ratio = Math.min(1, max / Math.max(img.width, img.height))
            const cv = document.createElement('canvas')
            cv.width = Math.round(img.width * ratio)
            cv.height = Math.round(img.height * ratio)
            const ctx = cv.getContext('2d')
            if (!ctx) return reject(new Error('画布创建失败'))
            ctx.fillStyle = '#fff'
            ctx.fillRect(0, 0, cv.width, cv.height)
            ctx.drawImage(img, 0, 0, cv.width, cv.height)
            resolve(cv.toDataURL('image/png'))
          }
          img.src = String(fr.result)
        }
        fr.readAsDataURL(file)
      })
    } catch (e: any) {
      setAddQrMsg(e.message ?? String(e))
      return
    }
    await saveAddQr(dataUrl)
  }

  async function saveCred() {
    setError('')
    setCredMsg('')
    try {
      await api('/api/admin/bots/qq/credentials', {
        method: 'POST',
        body: JSON.stringify({
          appId: qqAppId.trim() || undefined,
          appSecret: qqAppSecret.trim() || undefined,
        }),
      })
      setQqAppSecret('')
      setCredMsg('凭据已保存并生效，点「启动」连接')
      load()
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  async function act(channel: 'qq' | 'weixin', action: 'start' | 'stop') {
    setError('')
    try {
      await api(`/api/admin/bots/${channel}/${action}`, { method: 'POST' })
      load()
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  async function fetchQr() {
    setError('')
    setQrStatus('')
    setNeedVerify(false)
    setVerifyCode('')
    try {
      const d = await api<{ qrcode: string; imgContent?: string; url?: string }>(
        '/api/admin/bots/weixin/qrcode',
      )
      setQr(d)
      // 轮询扫码状态，扫完自动停（1.0.1 全状态机：need_verifycode 出验证码框带 verifyCode 重轮）
      if (pollRef.current) clearInterval(pollRef.current)
      pollRef.current = setInterval(async () => {
        try {
          const r = await api<{ status: string; tokenSaved: boolean; nickname: string }>(
            '/api/admin/bots/weixin/qrcode/check',
            {
              method: 'POST',
              body: JSON.stringify({
                qrcode: d.qrcode,
                ...(needVerifyRef.current && verifyCodeRef.current
                  ? { verifyCode: verifyCodeRef.current }
                  : {}),
              }),
            },
          )
          setQrStatus(r.status)
          if (r.status === 'need_verifycode') setNeedVerify(true)
          if (r.tokenSaved) {
            if (pollRef.current) clearInterval(pollRef.current)
            pollRef.current = null
            setQr(null)
            setNeedVerify(false)
            setQrStatus('')
            load()
          }
        } catch (e) {
          // P2-5：不再完全静默 —— 过期等错误下一次轮询会重试，但要留痕到控制台
          console.error('扫码状态轮询失败', e)
        }
      }, 3000)
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  /* P2-3：扫码轮询的定时器要随组件卸载清掉 —— 之前只靠「扫完自动停」，
     切到别的页面（或 401 回登录门）后每 3s 的 POST 会一直打下去。 */
  useEffect(() => {
    return () => {
      if (pollRef.current) {
        clearInterval(pollRef.current)
        pollRef.current = null
      }
    }
  }, [])

  async function saveToken() {
    setError('')
    try {
      await api('/api/admin/bots/weixin/token', {
        method: 'POST',
        body: JSON.stringify({ token: manualToken }),
      })
      setManualToken('')
      load()
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  async function toggleAdmin(s: any) {
    try {
      await api('/api/admin/bots/sessions/admin', {
        method: 'POST',
        body: JSON.stringify({
          channel: s.channel,
          conversationId: s.conversationId,
          isAdmin: !s.isAdmin,
        }),
      })
      load()
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

  const qq = status?.channels?.find((c: any) => c.channel === 'qq')
  const wx = status?.channels?.find((c: any) => c.channel === 'weixin')

  return (
    <div className="page">
      {error && <div className="alert bad">{error}</div>}

      <div className="cards">
        {qq && (
          <section className="card">
            <h2>QQ 机器人</h2>
            <p>
              状态：<b className={qq.connected ? 'ok' : 'warn'}>{PHASE_LABEL[qq.phase] ?? qq.phase}</b>
              {qq.note && <span className="hint">（{qq.note}）</span>}
              <span className="hint"> · 消息数 {qq.messageCount ?? 0}</span>
            </p>
            <p className="hint">
              只需要填开放平台的 <b>AppID</b> 和 <b>AppSecret</b>（QQ 开放平台 → 机器人管理页可查），
              保存后点「启动」即可，全程实时在线。
            </p>
            <div className="row">
              <input
                placeholder="AppID"
                style={{ maxWidth: 200 }}
                value={qqAppId}
                onChange={(e) => setQqAppId(e.target.value)}
              />
              <input
                type="password"
                placeholder={qqCred?.secretSet ? 'AppSecret（已设置，不改留空）' : 'AppSecret'}
                style={{ maxWidth: 240 }}
                value={qqAppSecret}
                onChange={(e) => setQqAppSecret(e.target.value)}
              />
              <button
                disabled={!((qqAppId.trim() || qqCred?.appId) && qqAppSecret.trim())}
                onClick={saveCred}
              >
                保存凭据
              </button>
            </div>
            {credMsg && <div className="alert good">{credMsg}</div>}
            {qqCred?.source === 'none' && (
              <div className="alert warn">还没有配置 AppID / AppSecret，机器人连不上。</div>
            )}
            <div className="row">
              <button onClick={() => act('qq', 'start')}>启动</button>
              <button onClick={() => act('qq', 'stop')}>停止</button>
            </div>
            <p className="hint">
              本部署<b>只做私聊</b>：在 QQ 里私聊机器人发消息后，会话会出现在下方列表，
              QQ 私聊免管理员白名单（直接按管理员处理），列表里可查看会话。（群聊事件一律忽略）
            </p>
            <p className="hint">
              直接私聊发 <b>CSV / 文本文件</b>即可导入数据库；文件名带上日期和团别
              （如 <code>2026-10-07-男团.csv</code>）就能直接入库，不用再回一句确认。
              导入完成后，打开着的后台页面会<b>自动刷新</b>。
            </p>
          </section>
        )}
        {wx && (
          <section className="card">
            <h2>微信机器人</h2>
            <p>
              状态：<b className={wx.connected ? 'ok' : 'warn'}>{PHASE_LABEL[wx.phase] ?? wx.phase}</b>
              {wx.nickname && <span>（{wx.nickname}）</span>}
              {wx.note && <span className="hint">（{wx.note}）</span>}
            </p>
            <div className="row">
              <button onClick={() => act('weixin', 'start')}>启动</button>
              <button onClick={() => act('weixin', 'stop')}>停止</button>
              <button onClick={fetchQr}>扫码登录</button>
            </div>
            {qr && (
              <div className="qr-box">
                <p>请用微信扫描（登录后自动保存，无需其他操作）：</p>
                {qr.imgContent ? (
                  <img
                    alt="微信登录二维码"
                    src={qr.imgContent.startsWith('data:') ? qr.imgContent : `data:image/png;base64,${qr.imgContent}`}
                    style={{ maxWidth: 240, borderRadius: 8 }}
                  />
                ) : qr.url ? (
                  <p>
                    <a href={qr.url} target="_blank" rel="noreferrer">
                      打开二维码链接
                    </a>
                  </p>
                ) : null}
                <p className="hint">
                  二维码标识：{qr.qrcode}
                  {qrStatus && <> · 状态：{qrStatus}</>}
                </p>
                {needVerify && (
                  <div className="row">
                    <input
                      placeholder="微信发来的验证码"
                      value={verifyCode}
                      onChange={(e) => setVerifyCode(e.target.value)}
                      style={{ maxWidth: 200 }}
                    />
                    <span className="hint">输入后轮询会自动带上重新校验</span>
                  </div>
                )}
              </div>
            )}
            <div className="row">
              <input
                placeholder="或手工粘贴 iLink token"
                value={manualToken}
                onChange={(e) => setManualToken(e.target.value)}
              />
              <button disabled={!manualToken.trim()} onClick={saveToken}>
                保存 token
              </button>
            </div>
          </section>
        )}
      </div>

      <section className="card">
        <h2>微信推送 · 加好友二维码（旧流程兜底）</h2>
        <p className="hint">
          现役流程是主播在「我的」页<b>自己扫码绑定微信推送</b>（官方 openclaw-weixin 协议，
          扫码后由服务端按账号长轮询）。下面这张二维码只作为旧流程（加好友 → 发手机号 + 绑定码）的兜底，
          新部署可以不配。
        </p>
        <div className="row">
          <button onClick={() => addQrFileRef.current?.click()}>
            {addQrSet ? '更换二维码' : '上传二维码'}
          </button>
          {addQrSet && <button onClick={() => saveAddQr('')}>清除</button>}
          <input
            ref={addQrFileRef}
            type="file"
            accept="image/*"
            hidden
            onChange={(e) => {
              const f = e.target.files?.[0]
              if (f) pickAddQr(f)
              e.target.value = ''
            }}
          />
        </div>
        {addQrSet && (
          <p>
            当前：<b className="ok">已配置</b>
          </p>
        )}
        {!addQrSet && (
          <div className="alert warn">
            还没配二维码：主播在「我的」页只能看到绑定码，看不到扫码入口。
          </div>
        )}
        {addQrMsg && <div className="alert good">{addQrMsg}</div>}
      </section>

      <section className="card">
        <h2>推送队列（入库后 {pushJobs?.delayMinutes ?? 30} 分钟推）</h2>
        <p className="hint">
          数据入库不立刻推，先排队等一轮导入的几个文件（男女团 / 多抖音号）都到齐，到点统一推一次。
          同一团同一业务日只排一队，重复导入只刷新不叠加。
          {pushJobs?.stats && (
            <>
              {' '}
              当前：<b>{pushJobs.stats.bound}</b> 人已绑定微信，其中{' '}
              <b>{pushJobs.stats.enabled}</b> 人开了推送开关。
            </>
          )}
        </p>
        {pushJobs?.items?.length ? (
          <table>
            <thead>
              <tr>
                <th>团别</th>
                <th>业务日</th>
                <th>计划推送时间</th>
                <th>状态</th>
                <th>完成时间</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {pushJobs.items.map((j: any) => (
                <tr key={j.id}>
                  <td>{j.team === 'female' ? '女团' : j.team === 'male' ? '男团' : j.team}</td>
                  <td>{j.bizDate}</td>
                  <td className="hint">{j.runAt?.slice(11, 16) || j.runAt}</td>
                  <td>
                    <span
                      className={
                        j.status === 'done'
                          ? 'badge b-active'
                          : j.status === 'failed'
                            ? 'badge b-left'
                            : j.status === 'running'
                              ? 'badge b-paused'
                              : 'badge'
                      }
                    >
                      {j.status === 'done'
                        ? '已推送'
                        : j.status === 'failed'
                          ? '失败'
                          : j.status === 'running'
                            ? '推送中'
                            : '排队中'}
                    </span>
                  </td>
                  <td className="hint">{j.doneAt || '—'}</td>
                  <td>
                    <button
                      disabled={pushBusy === `${j.team}:${j.bizDate}`}
                      onClick={() => runPushNow(j.team, j.bizDate)}
                    >
                      {pushBusy === `${j.team}:${j.bizDate}` ? '推送中…' : '立即推送'}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : pushError ? (
          <div className="alert warn">
            ⚠️ 推送队列加载失败：{pushError}（会随轮询自动重试）
          </div>
        ) : (
          <p className="hint">还没有推送任务：导入一轮数据后这里会出现。</p>
        )}
        {pushJobs?.items?.some((j: any) => j.status === 'failed' && j.note) && (
          <div className="alert bad">
            失败原因：
            {pushJobs.items.find((j: any) => j.status === 'failed' && j.note)?.note}
          </div>
        )}
        {pushMsg && <div className="alert good">{pushMsg}</div>}
      </section>

      <PublicSettingsCard />

      <CommissionSettingsCard />

      <MyAccountsCard />

      <section className="card">
        <h2>会话管理（群聊 / 私聊）</h2>
        <p className="hint">
          机器人收到过消息的会话都在这里；勾选「管理员」的会话才能触发管理员命令（导入 / 导出 /
          查询等）。微信里完成推送绑定的主播私聊还能用用户命令（每日报告 / 个人报告 /
          暂停接收每日推送 / 开启接收每日推送 / 帮助）。群聊和私聊各自独立勾选。
        </p>
        {sessions.length === 0 ? (
          <p className="hint">还没有会话记录：在 QQ / 微信里给机器人发一条消息即可出现</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>渠道</th>
                <th>类型</th>
                <th>会话 ID</th>
                <th>最近消息</th>
                <th>最近时间</th>
                <th>管理员</th>
              </tr>
            </thead>
            <tbody>
              {sessions.map((s) => (
                <tr key={`${s.channel}:${s.conversationId}`}>
                  <td>{s.channel === 'qq' ? 'QQ' : '微信'}</td>
                  <td>{s.kind === 'group' ? '群聊' : '私聊'}</td>
                  <td className="hint">{s.conversationId}</td>
                  <td className="hint">{s.lastText || '—'}</td>
                  <td className="hint">{s.lastSeen}</td>
                  <td>
                    <input type="checkbox" checked={s.isAdmin} onChange={() => toggleAdmin(s)} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
    </div>
  )
}

function PublicSettingsCard() {
  const [settings, setSettings] = useState<any>(null)
  const [msg, setMsg] = useState('')
  /** 设置加载失败（P2-5）：显示出来，别停在「加载中…」 */
  const [loadError, setLoadError] = useState('')

  const load = useCallback(() => {
    api('/api/admin/public/settings')
      .then(setSettings)
      .catch((e) => {
        // P2-5：设置加载失败不再渲染成「加载中…」的永久假态
        console.error('公开站点设置加载失败', e)
        setSettings(null)
        setLoadError(String(e?.message ?? e))
      })
  }, [])

  useEffect(() => {
    load()
  }, [load])

  async function toggle(key: string, value: boolean) {
    try {
      const d = await api('/api/admin/public/settings', {
        method: 'POST',
        body: JSON.stringify({ [key]: value }),
      })
      setSettings(d.settings)
      setMsg('已保存')
      setTimeout(() => setMsg(''), 1500)
    } catch (e: any) {
      setMsg(e.message ?? String(e))
    }
  }

  const FLAGS: [string, string][] = [
    ['maleEnabled', '男团链接显示'],
    ['femaleEnabled', '女团链接显示'],
    ['daily', '每日报告'],
    ['monthly', '每月数据'],
    ['yearly', '每年数据'],
    ['duration', '直播时长'],
    ['trend', '涨跌/对照'],
    ['wave', '音浪数值（每日报告）'],
    ['commissionEnabled', '徒弟提成（「我的」页）'],
  ]

  /** 页面级显隐（2026-10-10）：关掉的主导航在展示端整体消失 */
  const PAGE_FLAGS: [string, string][] = [
    ['dataEnabled', '每日报告页'],
    ['groupEnabled', '分组页'],
    ['lineageEnabled', '族谱页（男团）'],
  ]

  /** 分组子页显隐：key = 赛事类型（与展示端子导航一一对应） */
  const GROUP_TYPE_FLAGS: [string, string][] = [
    ['初级争霸赛小组赛', '初级小组赛'],
    ['中级争霸赛小组赛', '中级小组赛'],
    ['月终争霸赛', '月终争霸赛'],
    ['初级争霸赛晋级赛', '初级晋级赛'],
    ['中级争霸赛晋级赛', '中级晋级赛'],
  ]

  async function toggleGroupType(key: string, on: boolean) {
    const cur = (settings?.groupTypes ?? {}) as Record<string, boolean>
    try {
      const d = await api('/api/admin/public/settings', {
        method: 'POST',
        body: JSON.stringify({ groupTypes: { ...cur, [key]: on } }),
      })
      setSettings(d.settings)
      setMsg('已保存')
      setTimeout(() => setMsg(''), 1500)
    } catch (e: any) {
      setMsg(e.message ?? String(e))
    }
  }

  /** 开关行：label + iOS 风格滑动开关（2026-10-10 优化布局） */
  function FlagRow({ label, hint, checked, onChange }: { label: string; hint?: string; checked: boolean; onChange: (v: boolean) => void }) {
    return (
      <label className="flag-row">
        <span className="flag-text">
          <span className="flag-label">{label}</span>
          {hint && <span className="flag-hint">{hint}</span>}
        </span>
        <input type="checkbox" className="switch" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      </label>
    )
  }

  return (
    <section className="card">
      <h2>公开站点设置（pk.subisle.com）</h2>
      <p className="hint">
        后端级开关：关闭后接口不下发对应数据（不是前端藏）。链接关闭时访问一律「暂未公布」。
        <b>关掉「开启登录门」则全面开放</b>：打开就能看榜单和分组，但没有「我的」页签。
        男女已共用一个网址，进页先选「男团 / 女团」。
      </p>
      {settings ? (
        <>
          <h3 className="flag-title">访问与页面</h3>
          <div className="flag-grid">
            <FlagRow
              label="开启登录门"
              hint="关 = 任何人直接看；开 = 要姓名/手机号 + 密码"
              checked={!!settings.loginRequired}
              onChange={(v) => toggle('loginRequired', v)}
            />
            <FlagRow
              label="开放手机号注册"
              hint="开 = 登录门显示「注册」（手机号 = 账号、短信验证码 = 密码、姓名须在主播台账里，注册即绑定，同步走 WorkBuddy 网关授权）；默认关"
              checked={!!settings.myRegister}
              onChange={(v) => toggle('myRegister', v)}
            />
            {PAGE_FLAGS.map(([key, label]) => (
              <FlagRow
                key={key}
                label={label}
                hint="主导航显隐"
                checked={!!settings[key]}
                onChange={(v) => toggle(key, v)}
              />
            ))}
          </div>

          <h3 className="flag-title">分组子页显隐（按赛事类型）</h3>
          <div className="flag-grid flag-grid-compact">
            {GROUP_TYPE_FLAGS.map(([key, label]) => (
              <FlagRow
                key={key}
                label={label}
                checked={settings.groupTypes?.[key] !== false}
                onChange={(v) => toggleGroupType(key, v)}
              />
            ))}
          </div>

          <h3 className="flag-title">数据字段</h3>
          <div className="flag-grid flag-grid-compact">
            {FLAGS.map(([key, label]) => (
              <FlagRow key={key} label={label} checked={!!settings[key]} onChange={(v) => toggle(key, v)} />
            ))}
          </div>
        </>
      ) : loadError ? (
        <div className="alert warn">
          ⚠️ 设置加载失败：{loadError}
          <button style={{ marginLeft: 8 }} onClick={load}>
            重试
          </button>
        </div>
      ) : (
        <p className="hint">加载中…</p>
      )}
      {msg && <div className="alert good">{msg}</div>}
    </section>
  )
}

/** 徒弟提成设置（1.0.1）：折算比例（音浪:人民币）+ 排除名单（师傅不参与提成统计）。
    比例改完即时生效、不追溯（历史按当前比例现算）。排除名单按 person.id 存，
    添加时按姓名搜索台账（与「我的」账号改绑同一交互）。 */
function CommissionSettingsCard() {
  const [wavePerYuan, setWavePerYuan] = useState<string>('')
  const [rate, setRate] = useState<number | null>(null)
  const [excluded, setExcluded] = useState<{ id: number; name: string }[]>([])
  const [msg, setMsg] = useState('')
  const [err, setErr] = useState('')
  const [q, setQ] = useState('')
  const [hits, setHits] = useState<{ personId: number; name: string }[]>([])

  const load = useCallback(() => {
    api<{ wavePerYuan: number; excludedMasterIds: number[] }>('/api/admin/commission/settings')
      .then((d) => {
        setRate(d.wavePerYuan)
        setWavePerYuan(String(d.wavePerYuan))
        // 排除名单只有 id：批量台账里捞名字（一次拉全量，150 人规模没问题）
        const ids = d.excludedMasterIds ?? []
        if (!ids.length) {
          setExcluded([])
          return
        }
        api<{ items: { personId: number; name: string }[] }>('/api/admin/streamers')
          .then((s) => {
            const byId = new Map((s.items ?? []).map((it: any) => [it.personId, it.name]))
            setExcluded(ids.map((id) => ({ id, name: byId.get(id) ?? `#${id}` })))
          })
          .catch(() => setExcluded(ids.map((id) => ({ id, name: `#${id}` }))))
      })
      .catch((e) => setErr(e.message ?? String(e)))
  }, [])

  useEffect(() => {
    load()
  }, [load])

  async function saveRate() {
    setErr('')
    setMsg('')
    const n = Number(wavePerYuan)
    if (!Number.isFinite(n) || n <= 0) {
      setErr('提成比例必须是大于 0 的数字')
      return
    }
    try {
      const d = await api<{ settings: { wavePerYuan: number } }>('/api/admin/commission/settings', {
        method: 'POST',
        body: JSON.stringify({ wavePerYuan: n }),
      })
      setRate(d.settings.wavePerYuan)
      setWavePerYuan(String(d.settings.wavePerYuan))
      setMsg('已保存，个人页即时生效')
      setTimeout(() => setMsg(''), 1500)
    } catch (e: any) {
      setErr(e.message ?? String(e))
    }
  }

  async function saveExcluded(ids: number[]) {
    setErr('')
    setMsg('')
    try {
      const d = await api<{ settings: { excludedMasterIds: number[] } }>('/api/admin/commission/settings', {
        method: 'POST',
        body: JSON.stringify({ excludedMasterIds: ids }),
      })
      // 用返回的权威列表回填名字（保持前后端一致）
      const ids2 = d.settings.excludedMasterIds ?? []
      api<{ items: { personId: number; name: string }[] }>('/api/admin/streamers')
        .then((s) => {
          const byId = new Map((s.items ?? []).map((it: any) => [it.personId, it.name]))
          setExcluded(ids2.map((id) => ({ id, name: byId.get(id) ?? `#${id}` })))
        })
        .catch(() => setExcluded(ids2.map((id) => ({ id, name: `#${id}` }))))
      setMsg('已保存')
      setTimeout(() => setMsg(''), 1500)
    } catch (e: any) {
      setErr(e.message ?? String(e))
    }
  }

  async function search(key: string) {
    setQ(key)
    if (!key.trim()) {
      setHits([])
      return
    }
    try {
      const d = await api<{ items: { personId: number; name: string }[] }>(
        `/api/admin/streamers?q=${encodeURIComponent(key.trim())}`,
      )
      setHits((d.items ?? []).slice(0, 8))
    } catch {
      setHits([])
    }
  }

  return (
    <section className="card">
      <h2>徒弟提成设置</h2>
      <p className="hint">
        「我的」页给<b>有直系徒弟</b>的主播显示提成卡：徒弟（只算直系，徒孙不计）的音浪按
        音浪:人民币 比例折算成金额。改比例即时生效、<b>不追溯</b>（按当前比例现算）。
        「不参与提成」的师傅（如鹏先生）个人页不显示提成卡，其徒弟音浪不计入。
      </p>
      <div className="row">
        <span className="hint" style={{ whiteSpace: 'nowrap' }}>
          音浪:人民币
        </span>
        <input
          inputMode="decimal"
          placeholder="10"
          style={{ maxWidth: 120 }}
          value={wavePerYuan}
          onChange={(e) => setWavePerYuan(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && saveRate()}
        />
        <span className="hint">: 1</span>
        <button disabled={wavePerYuan === String(rate)} onClick={saveRate}>
          保存比例
        </button>
      </div>
      <div className="row" style={{ alignItems: 'flex-start', flexWrap: 'wrap', marginTop: 8 }}>
        <span className="hint" style={{ whiteSpace: 'nowrap' }}>
          不参与提成的师傅：
        </span>
        {excluded.length === 0 && <span className="hint">（无）</span>}
        {excluded.map((x) => (
          <span key={x.id} className="badge" style={{ marginRight: 6, marginBottom: 4 }}>
            {x.name}{' '}
            <button
              style={{ border: 'none', background: 'none', cursor: 'pointer', padding: 0 }}
              title="移除"
              onClick={() => saveExcluded(excluded.filter((y) => y.id !== x.id).map((y) => y.id))}
            >
              ×
            </button>
          </span>
        ))}
      </div>
      <div className="row" style={{ alignItems: 'flex-start', flexWrap: 'wrap' }}>
        <input
          placeholder="输入师傅姓名搜索后加入排除名单"
          value={q}
          onChange={(e) => search(e.target.value)}
          style={{ maxWidth: 280 }}
        />
      </div>
      {hits.length > 0 && (
        <div className="hint" style={{ marginTop: 6 }}>
          {hits.map((h) => (
            <button
              key={h.personId}
              style={{ marginRight: 6, marginBottom: 6 }}
              disabled={excluded.some((x) => x.id === h.personId)}
              onClick={() => saveExcluded([...excluded.map((x) => x.id), h.personId])}
            >
              {h.name}
            </button>
          ))}
        </div>
      )}
      {msg && <div className="alert good">{msg}</div>}
      {err && <div className="alert bad">{err}</div>}
    </section>
  )
}

/** 我的页面账号（2026-10-10）：展示端「我的」页的登录账号。
    两种来源：① 管理员在这里手动设置（也能在主播管理右键单个主播设置）；
    ② 主播在展示端登录门「注册」自助创建（手机号 = 账号、短信验证码 = 密码，
    注册时同时把 WorkBuddy 账号加进用量看板的账号池）。
    自助注册出来的账号 name 先是手机号占位、bound=0，等绑定主播后 name = 主播姓名；
    绑定后用户自己不能解绑，只有这里能改绑。 */
function MyAccountsCard() {
  const [items, setItems] = useState<any[]>([])
  const [name, setName] = useState('')
  const [phone, setPhone] = useState('')
  const [pass, setPass] = useState('')
  const [msg, setMsg] = useState('')
  const [err, setErr] = useState('')
  const [rebindFor, setRebindFor] = useState('')
  const [rebindQ, setRebindQ] = useState('')
  const [rebindHits, setRebindHits] = useState<any[]>([])

  const load = useCallback(() => {
    api('/api/admin/my-accounts')
      .then((d) => setItems(d.items ?? []))
      .catch((e) => {
        // P2-5：失败显示出来（别渲染成「还没有账号」的假空态）
        console.error('「我的」账号列表加载失败', e)
        setItems([])
        setErr(String(e?.message ?? e))
      })
  }, [])

  useEffect(() => {
    load()
  }, [load])

  async function add() {
    setErr('')
    setMsg('')
    try {
      await api('/api/admin/my-accounts', {
        method: 'POST',
        body: JSON.stringify({ name: name.trim(), phone: phone.trim(), pass: pass.trim() }),
      })
      setName('')
      setPhone('')
      setPass('')
      setMsg('已保存')
      setTimeout(() => setMsg(''), 1500)
      load()
    } catch (e: any) {
      setErr(e.message ?? String(e))
    }
  }

  async function remove(name: string) {
    setErr('')
    setMsg('')
    try {
      await api('/api/admin/my-accounts/' + encodeURIComponent(name), { method: 'DELETE' })
      setMsg('已删除')
      setTimeout(() => setMsg(''), 1500)
      load()
    } catch (e: any) {
      setErr(e.message ?? String(e))
    }
  }

  /** 改绑/绑定主播：展示端用户自己改不了，只有管理员能改（用户口径 2026-10-10） */
  async function searchRebind(key: string) {
    setRebindQ(key)
    if (!key.trim()) {
      setRebindHits([])
      return
    }
    try {
      const d = await api<{ items: any[] }>(`/api/admin/streamers?q=${encodeURIComponent(key.trim())}`)
      setRebindHits((d.items ?? []).slice(0, 8))
    } catch (e) {
      console.error('主播搜索失败', e)
      setRebindHits([])
    }
  }

  async function doRebind(account: string, personId: number, anchorName: string) {
    setErr('')
    setMsg('')
    try {
      await api(`/api/admin/my-accounts/${encodeURIComponent(account)}/bind`, {
        method: 'POST',
        body: JSON.stringify({ personId }),
      })
      setMsg(`「${account}」已绑定主播「${anchorName}」`)
      setRebindFor('')
      setRebindQ('')
      setRebindHits([])
      load()
    } catch (e: any) {
      setErr(e.message ?? String(e))
    }
  }

  return (
    <section className="card">
      <h2>「我的」页面账号（/st/m · /st/f）</h2>
      <p className="hint">
        账号来源两种：<b>这里手动设置</b>（姓名要跟主播台账里的一致，姓名已存在时保存 = 覆盖更新），
        或主播在展示端登录门点「注册」自助创建（手机号 = 账号、短信验证码 = 密码）。
        <b>手机号不只是登录方式，也是微信绑定的凭证</b>：填了手机号，主播加机器人后发手机号就能绑定推送；
        没填就只能用「我的」页的 6 位绑定码。
        自助注册的账号是「待绑定」状态：主播在「我的」页绑定主播姓名后才能看个人数据，
        <b>绑定后主播自己不能解绑，只有这里能改绑</b>。
      </p>
      <div className="row">
        <input
          placeholder="姓名（与台账一致）"
          value={name}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && name.trim() && pass.trim() && add()}
        />
        <input
          placeholder="手机号（选填）"
          inputMode="numeric"
          value={phone}
          onChange={(e) => setPhone(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && name.trim() && pass.trim() && add()}
        />
        <input
          placeholder="密码（4~20 位字母或数字）"
          value={pass}
          onChange={(e) => setPass(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && name.trim() && pass.trim() && add()}
        />
        <button className="primary" disabled={!name.trim() || !pass.trim()} onClick={add}>
          保存
        </button>
      </div>
      {items.length ? (
        <table>
          <thead>
            <tr>
              <th>账号 / 姓名</th>
              <th>手机号</th>
              <th>来源</th>
              <th>绑定状态</th>
              <th>添加时间</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {items.map((a) => (
              <tr key={a.name}>
                <td>{a.name}</td>
                <td className="hint">{a.phone}</td>
                <td className="hint">{a.source === 'register' ? '自助注册' : '后台添加'}</td>
                <td className="hint">
                  {a.bound ? (
                    a.isAdmin ? (
                      '管理员账号'
                    ) : (
                      <span className="badge b-active">已绑主播</span>
                    )
                  ) : (
                    <span className="badge b-paused">待绑定</span>
                  )}
                </td>
                <td className="hint">{a.createdAt}</td>
                <td>
                  {a.isAdmin ? null : (
                    <button onClick={() => setRebindFor(rebindFor === a.name ? '' : a.name)}>
                      {a.bound ? '改绑' : '绑定主播'}
                    </button>
                  )}{' '}
                  <button className="danger" onClick={() => remove(a.name)}>
                    删除
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : (
        <p className="hint">还没有账号。添加后主播才能登录「我的」页。</p>
      )}
      {rebindFor && (
        <div className="row" style={{ alignItems: 'flex-start', flexWrap: 'wrap' }}>
          <div style={{ flex: 1, minWidth: 220 }}>
            <input
              placeholder={`「${rebindFor}」改绑到哪位主播？输入名字搜索`}
              value={rebindQ}
              onChange={(e) => searchRebind(e.target.value)}
              style={{ width: '100%' }}
            />
            {rebindHits.length ? (
              <div className="hint" style={{ marginTop: 6 }}>
                {rebindHits.map((h) => (
                  <button
                    key={h.personId}
                    style={{ marginRight: 6, marginBottom: 6 }}
                    onClick={() => doRebind(rebindFor, h.personId, h.name)}
                  >
                    {h.name}（{h.team === 'male' ? '男团' : '女团'}）
                  </button>
                ))}
              </div>
            ) : (
              <div className="hint" style={{ marginTop: 6 }}>
                输入主播姓名开始搜索；改绑后账号名会跟着变成主播姓名，登录仍可用手机号。
              </div>
            )}
          </div>
          <button onClick={() => { setRebindFor(''); setRebindQ(''); setRebindHits([]) }}>取消</button>
        </div>
      )}
      {msg && <div className="alert good">{msg}</div>}
      {err && <div className="alert bad">{err}</div>}
    </section>
  )
}
