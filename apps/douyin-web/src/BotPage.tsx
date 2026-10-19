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

/** 后端 phase 是 idle / awaiting_scan 这类内名：表里没有的状态一律显示中文兜底，
 *  不把英文内名漏到界面上（DESIGN §4.1）。 */
const phaseText = (phase: string) => PHASE_LABEL[phase] ?? '状态未知'

/** 扫码状态机（后端 wait / scaned / need_verifycode / verify_code_blocked /
 *  expired / confirmed / scaned_but_redirect / binded_redirect）→ 运营看得懂的中文。 */
const QR_STATUS_LABEL: Record<string, string> = {
  wait: '等待扫码',
  scaned: '已扫码，请在手机上确认',
  confirmed: '已确认，正在保存登录',
  need_verifycode: '需要在手机上输入验证码',
  verify_code_blocked: '验证码错误次数过多，稍后再试',
  expired: '二维码已过期，请重新生成',
  scaned_but_redirect: '已扫码，正在切换服务器',
  binded_redirect: '已绑定，正在切换服务器',
}
const qrStatusText = (status: string) => QR_STATUS_LABEL[status] ?? '状态未知'

/** 团别 → 中文（后端给 male / female；未知值不把英文原样漏到界面） */
const teamText = (team?: string) => (team === 'female' ? '女团' : team === 'male' ? '男团' : '—')

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
  /** 师门日报只读概览（docs/23 §4.3）：已绑定微信的人里，名下有直系徒弟的「师傅」名单。
      口径与后端 hasDisciples 一致（person_id>0 且本人无师傅 master_id IS NULL，
      且存在未删除的直系徒弟）。开关值本身在展示端「我的」页由用户自己设，
      管理端只读展示，不做交互开关。 */
  const [lineageBind, setLineageBind] = useState<{
    total: number
    items: { name: string; team: string; disciples: number }[]
  } | null>(null)

  const loadLineageBind = useCallback(() => {
    api<{ items: any[] }>('/api/admin/streamers')
      .then((d) => {
        const rows = (d.items ?? []).filter(
          (a: any) => a.wechatBound && !a.masterId && a.disciples > 0,
        )
        setLineageBind({
          total: rows.length,
          items: rows.map((a: any) => ({
            name: a.name,
            team: a.team,
            disciples: a.disciples,
          })),
        })
      })
      .catch(() => {
        // 概览加载失败只留空态
        setLineageBind(null)
      })
  }, [])

  useEffect(() => {
    loadLineageBind()
    const t = setInterval(loadLineageBind, 60000)
    return () => clearInterval(t)
  }, [loadLineageBind])

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
              状态：<b className={qq.connected ? 'ok' : 'warn'}>{phaseText(qq.phase)}</b>
              {qq.note && <span className="hint">（{qq.note}）</span>}
              <span className="hint"> · 消息数 {qq.messageCount ?? 0}</span>
            </p>
            <p className="hint">
              只需要填开放平台的 <b>AppID</b> 和 <b>AppSecret</b>（QQ 开放平台 → 机器人管理页可查），
              保存后点「启动」即可，全程实时在线。
            </p>
            <div className="row row-end">
              <label className="field">
                AppID
                <input
                  placeholder="如 1027xxxxxx"
                  style={{ maxWidth: 200 }}
                  value={qqAppId}
                  onChange={(e) => setQqAppId(e.target.value)}
                />
              </label>
              <label className="field">
                AppSecret
                <input
                  type="password"
                  placeholder={qqCred?.secretSet ? '已设置，不改留空' : '开放平台机器人管理页可查'}
                  style={{ maxWidth: 240 }}
                  value={qqAppSecret}
                  onChange={(e) => setQqAppSecret(e.target.value)}
                />
              </label>
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
              状态：<b className={wx.connected ? 'ok' : 'warn'}>{phaseText(wx.phase)}</b>
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
                  />
                ) : qr.url && /^https?:\/\//i.test(qr.url) ? (
                  /* 2026-10-17 审计 5-P3（遗留 N3）：上游微信协议响应原样透传的 url
                  必须过 scheme 白名单才进 href，javascript: 一律不渲染。 */
                  <p>
                    <a href={qr.url} target="_blank" rel="noreferrer">
                      打开二维码链接
                    </a>
                  </p>
                ) : null}
                <p className="hint">
                  二维码标识：{qr.qrcode}
                  {qrStatus && <> · 状态：{qrStatusText(qrStatus)}</>}
                </p>
                {needVerify && (
                  <div className="row">
                    <label className="field">
                      微信发来的验证码
                      <input
                        placeholder="如 1234"
                        value={verifyCode}
                        onChange={(e) => setVerifyCode(e.target.value)}
                        style={{ maxWidth: 200 }}
                      />
                      <span className="field-hint">填完不用点保存，下一轮轮询会自动带上重新校验。</span>
                    </label>
                  </div>
                )}
              </div>
            )}
            <div className="row row-end">
              <label className="field">
                手工粘贴登录令牌（token）
                <input
                  placeholder="粘贴 iLink 登录令牌"
                  value={manualToken}
                  onChange={(e) => setManualToken(e.target.value)}
                />
              </label>
              <button disabled={!manualToken.trim()} onClick={saveToken}>
                保存令牌
              </button>
            </div>
          </section>
        )}
      </div>

      <section className="card">
        <h2>微信推送 · 机器人加好友二维码（对接主路径）</h2>
        <p className="hint">
          主播在展示端「我的」页<b>扫这张码加机器人</b>：扫码成功那一刻，服务端已按账号名保存好微信凭证；
          之后用<b>同一个微信给机器人随便发一条消息</b>，机器人就会自动完成对接并回一条欢迎语。
          4 位对接码<b>不再是必需步骤</b>，仍保留为可选兜底：也可以把页面上的 4 位对接码发给机器人。
          没配这张码时，「我的」页只显示提示和对接码，加过机器人的主播仍可对接。
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
            还没配二维码：主播在「我的」页看不到加机器人的入口，只能干等或口头拉群。
            建议尽快上传机器人名片 / 加好友二维码。
          </div>
        )}
        {addQrMsg && <div className="alert good">{addQrMsg}</div>}
      </section>

      <section className="card">
        <h2>师门日报推送概览（只读）</h2>
        <p className="hint">
          开了「师门日报」开关（第三开关）的师傅，会在数据入库延迟推送时收到
          「名下徒弟音浪 + 与上日差距」的分页图（docs/23 §4）。
          <b>有徒弟才算师傅</b>：名下没有直系徒弟的绑定不会推空图；同一业务日只推一次。
          开关值由用户在展示端「我的」页自己设置（或后端一键开启），管理端只读展示，不在这里改。
          {lineageBind && (
            <>
              {' '}
              当前已绑定微信的账号里，<b>{lineageBind.total}</b> 人名下有直系徒弟
              （这批人开开关后即可收到）。
            </>
          )}
        </p>
        {!lineageBind ? (
          <div className="alert warn">
            ⚠️ 师门概览没读出来：主播台账接口暂时不可用（每 60 秒会自动重试一次）
            <button onClick={loadLineageBind}>重试</button>
          </div>
        ) : lineageBind.items.length === 0 ? (
          <div className="empty">
            已绑定微信的账号里，还没有名下有直系徒弟的人；等师徒关系建立后会自动出现在这里。
          </div>
        ) : (
          <div className="table-scroll">
            <table>
              <thead>
                <tr>
                  <th>主播</th>
                  <th>团别</th>
                  <th>直系徒弟数</th>
                  <th>师门日报开关</th>
                </tr>
              </thead>
              <tbody>
                {lineageBind.items.map((a) => (
                  <tr key={a.name}>
                    <td>{a.name}</td>
                    <td>{teamText(a.team)}</td>
                    <td>{a.disciples}</td>
                    <td className="hint">在展示端「我的」页查看 / 设置</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      <PublicSettingsCard />
      <PushQueueCard />
      <MyAccountsCard />

      <section className="card">
        <h2>会话管理（群聊 / 私聊）</h2>
        <p className="hint">
          机器人收到过消息的会话都在这里；勾选「管理员」的会话才能触发管理员命令（导入 / 导出 /
          查询等）。微信里完成推送绑定的主播私聊还能用用户命令（每日报告 / 个人报告 /
          暂停接收每日推送 / 开启接收每日推送 / 帮助）。群聊和私聊各自独立勾选。
        </p>
        {sessions.length === 0 ? (
          <div className="empty">
            还没有会话记录：在 QQ / 微信里给机器人发一条消息，这里就会出现该会话。
          </div>
        ) : (
          <div className="table-scroll">
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
          </div>
        )}
      </section>
    </div>
  )
}

/* 推送队列（2026-10-17 新增，审计 6/P1-2）：后端 /api/admin/push/jobs 与
   /api/admin/push/run 早就有，但前端**没有任何入口** —— 上线后推送一旦出问题，
   运营在界面上既看不到队列、也没有「立刻再推一次」的按钮，只能登服务器 curl。
   这里补上：看最近任务 + 手动立即推送（立即推送仍然走 PUSH_OUT_ENABLED 出网闸门）。 */
function PushQueueCard() {
  const [jobs, setJobs] = useState<any[]>([])
  const [stats, setStats] = useState<any>(null)
  const [delayMinutes, setDelayMinutes] = useState<number | null>(null)
  const [team, setTeam] = useState('male')
  const [bizDate, setBizDate] = useState('')
  const [busy, setBusy] = useState(false)
  const [msg, setMsg] = useState('')
  const [err, setErr] = useState('')

  const load = useCallback(() => {
    api('/api/admin/push/jobs')
      .then((d: any) => {
        setJobs(d?.items ?? [])
        setStats(d?.stats ?? null)
        setDelayMinutes(typeof d?.delayMinutes === 'number' ? d.delayMinutes : null)
      })
      .catch((e) => setErr(String(e?.message ?? e)))
  }, [])

  useEffect(() => {
    load()
  }, [load])
  // 推送任务有变化就自动刷新（导入完成 / 别的页签推过）
  useAutoRefresh(load, ['import', 'bot'])

  async function runNow() {
    if (!bizDate.trim()) {
      setErr('请填业务日期（YYYY-MM-DD）')
      return
    }
    setBusy(true)
    setErr('')
    setMsg('')
    try {
      await api(
        `/api/admin/push/run?team=${encodeURIComponent(team)}&bizDate=${encodeURIComponent(bizDate.trim())}`,
        { method: 'POST', body: '{}' },
      )
      setMsg(`已立即推送：${team === 'female' ? '女团' : '男团'} ${bizDate.trim()}`)
      load()
    } catch (e) {
      setErr(String((e as any)?.message ?? e))
    } finally {
      setBusy(false)
    }
  }

  const statusText = (s: string) => {
    // 2026-10-17 审计 5-P2：后端真实状态集是 pending / running / running:N（重试中）/
    // failed:N（退避等待重试）/ done / failed（终态）—— 原映射里的 `sending` 不存在，
    // 重试态会裸显原始码。`failed:N` 与终态 `failed` 同样显示「失败」+ 重试说明。
    if (s === 'pending') return '待发'
    if (s === 'running') return '发送中'
    if (s === 'done') return '已完成'
    if (s === 'failed') return '失败'
    if (s.startsWith('running:')) return '发送中（重试）'
    if (s.startsWith('failed:')) return '失败（等待重试）'
    return s
  }

  return (
    <section className="card">
      <h2>推送队列</h2>
      <p className="hint">
        数据入库后按{delayMinutes == null ? '配置的延迟' : ` ${delayMinutes} 分钟 `}延迟推送；
        这里能看队列有没有推出去，也能手动立刻再推一次（仍受出网闸门控制）。
      </p>
      {stats && (
        /* 2026-10-17 审计 5-P2：后端 push_stats 返回的是 {bound, enabled}
        （已绑定微信人数 / 已开推送人数），原先读 pending/sending/done/failed
        四个不存在的字段 → 统计行恒显示 0。 */
        <p className="hint">
          已绑定微信 {stats.bound ?? 0} 人 · 已开推送 {stats.enabled ?? 0} 人
        </p>
      )}
      <div className="row" style={{ gap: 8, alignItems: 'flex-end', flexWrap: 'wrap' }}>
        <label className="field">
          团别
          <select value={team} onChange={(e) => setTeam(e.target.value)}>
            <option value="male">男团</option>
            <option value="female">女团</option>
          </select>
        </label>
        <label className="field">
          业务日期
          <input
            placeholder="YYYY-MM-DD"
            value={bizDate}
            onChange={(e) => setBizDate(e.target.value)}
          />
        </label>
        <button className="primary" disabled={busy} onClick={runNow}>
          {busy ? '推送中…' : '立即推送'}
        </button>
      </div>
      {jobs.length > 0 && (
        <table style={{ marginTop: 8 }}>
          <thead>
            <tr>
              <th>团别</th>
              <th>业务日</th>
              <th>状态</th>
              <th>计划时间</th>
              <th>备注</th>
            </tr>
          </thead>
          <tbody>
            {jobs.map((j: any, i: number) => (
              <tr key={j.id ?? i}>
                <td>{teamText(j.team)}</td>
                <td>{j.bizDate ?? j.biz_date ?? '—'}</td>
                <td>{statusText(String(j.status ?? '—'))}</td>
                <td>{j.runAt ?? j.run_at ?? '—'}</td>
                <td>{j.note || '—'}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {msg && <div className="alert good">{msg}</div>}
      {err && <div className="alert bad">{err}</div>}
    </section>
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
    ['duration', '直播时长（日 / 月 / 年榜）'],
    ['trend', '涨跌/对照'],
    ['wave', '音浪数值（日 / 月 / 年榜）'],
    ['absentDays', '未播天数（日 / 月 / 年榜）'],
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

  /** 🚩 小红旗条文本/图标字段：blur 时保存（空串 = 清除恢复默认）。
      注：champBanner 的入口已下线（改由独立的公告配置页配多条），这里不再收该字段，
      后端「传了才改」语义下不传 = 保持原值。 */
  async function saveStarText(
    key: 'starTitle' | 'starOverride' | 'starIcon',
    value: string,
  ) {
    try {
      const d = await api('/api/admin/public/settings', {
        method: 'POST',
        body: JSON.stringify({ [key]: value }),
      })
      setSettings((prev: any) => ({ ...prev, ...d.settings, [key]: value }))
      setMsg('已保存')
      setTimeout(() => setMsg(''), 1500)
    } catch (e: any) {
      setMsg(e.message ?? String(e))
    }
  }

  /** 🚩 小红旗图标：选图后客户端缩放裁剪成 ≤128×128 方形 PNG，再保存为 data URL */
  function onStarIconFile(file: File | undefined) {
    if (!file) return
    const reader = new FileReader()
    reader.onload = () => {
      const img = new Image()
      img.onload = () => {
        try {
          const side = Math.min(img.naturalWidth || img.width, img.naturalHeight || img.height)
          if (!side || side <= 0) throw new Error('图片尺寸无效')
          const sx = ((img.naturalWidth || img.width) - side) / 2
          const sy = ((img.naturalHeight || img.height) - side) / 2
          const canvas = document.createElement('canvas')
          canvas.width = 128
          canvas.height = 128
          const ctx = canvas.getContext('2d')
          if (!ctx) throw new Error('canvas 不可用')
          ctx.imageSmoothingEnabled = true
          ctx.drawImage(img, sx, sy, side, side, 0, 0, 128, 128)
          const dataUrl = canvas.toDataURL('image/png')
          void saveStarText('starIcon', dataUrl)
        } catch (err: any) {
          setMsg(err?.message ?? String(err))
        }
      }
      img.onerror = () => setMsg('图片读取失败')
      img.src = String(reader.result)
    }
    reader.onerror = () => setMsg('文件读取失败')
    reader.readAsDataURL(file)
  }

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

  /* 推送图字段显隐（2026-10-17 用户口径「字段后端支持显示和不显示」）：
     config `push_report_fields` 一份配置同时作用于日报图（月榜图）、师门日报图，
     以及「消息下发」里发的师门日报图；缺字段 = 显示（不会误伤没动过的列）。 */
  const PUSH_FIELD_GROUPS: {
    key: 'daily' | 'lineage'
    title: string
    items: [string, string][]
  }[] = [
    {
      key: 'daily',
      title: '推送图字段 · 每日报告（月榜图）',
      items: [
        ['dailyStar', '每日之星（头部）'],
        ['rank', '排名'],
        ['name', '主播姓名'],
        ['absentDays', '当月未播天数'],
        ['wave', '音浪数值 / 条形图'],
        ['totalWave', '距上一名 / 累计总音浪'],
        ['status', '未播状态'],
      ],
    },
    {
      key: 'lineage',
      title: '推送图字段 · 师门日报图',
      items: [
        ['anchor', '主播（师傅行高亮）'],
        ['todayWave', '今日音浪'],
        ['yesterdayWave', '昨日音浪'],
        ['waveDiff', '较昨日'],
        ['monthTotal', '本月累计'],
        ['status', '状态'],
      ],
    },
  ]

  /** 字段显隐整包保存：后端缺字段回落「显示」（serde default）。 */
  async function savePushFields(next: any) {
    try {
      const d = await api('/api/admin/public/settings', {
        method: 'POST',
        body: JSON.stringify({ pushReportFields: next }),
      })
      setSettings((prev: any) => ({ ...prev, ...d.settings, pushReportFields: next }))
      setMsg('已保存')
      setTimeout(() => setMsg(''), 1500)
    } catch (e: any) {
      setMsg(e.message ?? String(e))
    }
  }

  /** 预估收益口径（2026-10-19）：分成 / 手续费百分数，失焦保存（非法/越界回落缺省值）。 */
  async function savePct(key: 'sharePct' | 'feePct', raw: string, dflt: number) {
    const n = Number(raw)
    // 税费 / 手续费支持小数（2026-10-20 实测 0.624%）；仍夹回 0~100。
    const v =
      Number.isFinite(n) && n >= 0 && n <= 100
        ? key === 'feePct'
          ? Math.round(n * 1000) / 1000
          : Math.round(n)
        : dflt
    try {
      const d = await api('/api/admin/public/settings', {
        method: 'POST',
        body: JSON.stringify({ [key]: v }),
      })
      setSettings((prev: any) => ({ ...prev, ...d.settings }))
      setMsg('已保存')
      setTimeout(() => setMsg(''), 1500)
    } catch (e: any) {
      setMsg(e.message ?? String(e))
    }
  }

  function togglePushField(group: 'daily' | 'lineage', key: string, on: boolean) {
    const cur = settings?.pushReportFields ?? {}
    void savePushFields({ ...cur, [group]: { ...(cur[group] ?? {}), [key]: on } })
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

          <h3 className="flag-title">🚩 流动小红旗条（每日推送图头部）</h3>
          <div className="field-list">
            <label className="field">
              称号
              <input
                defaultValue={settings.starTitle ?? ''}
                placeholder="每日之星"
                onBlur={(e) => {
                  if (e.target.value !== (settings.starTitle ?? '')) saveStarText('starTitle', e.target.value)
                }}
              />
              <span className="field-hint">
                留空 = 恢复默认「每日之星」；也可以改成如「初级争霸赛冠军」。失焦即保存。
              </span>
            </label>
            <label className="field">
              手动指定（可选）
              <input
                defaultValue={settings.starOverride ?? ''}
                placeholder="初级争霸赛冠军|狼哲|285917"
                onBlur={(e) => {
                  if (e.target.value !== (settings.starOverride ?? '')) saveStarText('starOverride', e.target.value)
                }}
              />
              <span className="field-hint">
                按「称号|人名|音浪」「称号|人名」或只写称号；留空 = 自动取当日音浪第一。
              </span>
            </label>
            <label className="field">
              <span className="field-head">
                小红旗图标（可选）
                {typeof settings.starIcon === 'string' && settings.starIcon && (
                  <>
                    <img src={settings.starIcon} width={28} height={28} alt="当前小红旗图标" />
                    <button type="button" className="linkish" onClick={() => saveStarText('starIcon', '')}>
                      清除图标
                    </button>
                  </>
                )}
              </span>
              <input
                type="file"
                accept="image/*"
                onChange={(e) => {
                  onStarIconFile(e.target.files?.[0])
                  e.target.value = ''
                }}
              />
              <span className="field-hint">留空 = 默认 🚩 emoji；上传图片后推送图头部用该图标。</span>
            </label>
            <p className="hint">
              🏆 争霸赛冠军横幅已挪到独立的公告配置页（展示页公告条，可配多条）——这里不再单独设置，避免两处撞车。
            </p>
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

          <h3 className="flag-title">推送图字段（显示 / 隐藏）</h3>
          <p className="hint" style={{ margin: '0 0 4px' }}>
            字段显隐由<b>后端配置</b>（config <code>push_report_fields</code>）：微信推送、
            「消息下发」里的师门日报图都按这里的勾选渲染。关掉的列 = 图里表头与数值一起不渲染，
            不会因为列少了而错位；<b>每日之星</b>默认显示在日报图头部（与日期同一行），
            关掉后头部只留日期。
          </p>
          {PUSH_FIELD_GROUPS.map((g) => (
            <div key={g.key}>
              <div className="field-hint" style={{ margin: '8px 0 2px', fontWeight: 600 }}>
                {g.title}
              </div>
              <div className="flag-grid flag-grid-compact">
                {g.items.map(([key, label]) => (
                  <FlagRow
                    key={key}
                    label={label}
                    checked={settings.pushReportFields?.[g.key]?.[key] !== false}
                    onChange={(v) => togglePushField(g.key, key, v)}
                  />
                ))}
              </div>
            </div>
          ))}

          <h3 className="flag-title">数据字段</h3>
          <div className="flag-grid flag-grid-compact">
            {FLAGS.map(([key, label]) => (
              <FlagRow key={key} label={label} checked={settings[key] !== false} onChange={(v) => toggle(key, v)} />
            ))}
          </div>

          <h3 className="flag-title">预估收益口径（展示端「我的」页）</h3>
          <p className="hint" style={{ margin: '0 0 4px' }}>
            音浪兑人民币固定 10:1（1 元 = 10 音浪），<b>主播分成比例与手续费由这里配</b>：
            展示端「本月预估到手」「我的抖音号」的号级估算都按
            <code>音浪 ÷ 10 × 分成% ×（1 − 税费/手续费%）</code>算，并把这个口径写在卡片上。
            抖音实际分成不固定（个人主播约 30%，公会主播按等级 30%~55%，2026 年起改为任务制弹性分成），
            提现另有门槛与单笔手续费 —— 这里填的是<b>估算口径</b>，改规则时不用改代码。
          </p>
          <div className="field-list">
            <label className="field">
              主播分成比例（%）
              <input
                type="number"
                min={0}
                max={100}
                step={1}
                defaultValue={settings.sharePct ?? 35}
                onBlur={(e) => savePct('sharePct', e.target.value, 35)}
              />
              <span className="field-hint">0~100，缺省 35；失焦即保存。</span>
            </label>
            <label className="field">
              税费 / 手续费（%）
              <input
                type="number"
                min={0}
                max={100}
                step={0.001}
                defaultValue={settings.feePct ?? 0}
                onBlur={(e) => savePct('feePct', e.target.value, 0)}
              />
              <span className="field-hint">0~100，支持小数（实测约 0.624）；只用于估算展示，实际扣费以抖音为准。</span>
            </label>
          </div>
        </>
      ) : loadError ? (
        <div className="alert warn">
          ⚠️ 设置没读出来：{loadError}（点「重试」再读一次；一直失败说明服务端接口不可用）
          <button onClick={load}>重试</button>
        </div>
      ) : (
        <p className="hint">正在读取公开站点设置…</p>
      )}
      {msg && <div className="alert good">{msg}</div>}
    </section>
  )
}

/** 我的页面账号（2026-10-10；2026-10-14 注册功能已下线，只剩后台开号一条路径）：
    展示端「我的」页的登录账号，由管理员在这里手动设置（也能在主播管理右键单个主播设置）。
    历史自助注册账号 name 先是手机号占位、bound=0，等绑定主播后 name = 主播姓名；
    绑定后用户自己不能解绑，只有这里能改绑。 */
function MyAccountsCard() {
  const [items, setItems] = useState<any[]>([])
  /** 首屏读取完成前不渲染空态（否则「还没有账号」会先闪一下假空态） */
  const [accLoaded, setAccLoaded] = useState(false)
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
      .then((d) => {
        setItems(d.items ?? [])
        setAccLoaded(true)
      })
      .catch((e) => {
        // P2-5：失败显示出来（别渲染成「还没有账号」的假空态）
        console.error('「我的」账号列表加载失败', e)
        setItems([])
        setErr(String(e?.message ?? e))
        setAccLoaded(true)
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
        // 2026-10-18 口令口径：4~64 字符、支持中英文/特殊字符/任意符号、服务端不 trim；
        // 这里不 trim，避免吃掉密码首尾空格。
        body: JSON.stringify({ name: name.trim(), phone: phone.trim(), pass }),
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

  /** 改「可看团别」（迁移 024）：走专用端点，不动密码 —— 上面的 POST 不传 pass
      会自动生成新密码（手机号后四位），用它改范围等于顺手把人家密码重置了。 */
  async function setViewTeam(name: string, viewTeam: string) {
    setErr('')
    setMsg('')
    try {
      await api(`/api/admin/my-accounts/${encodeURIComponent(name)}/view-team`, {
        method: 'PUT',
        body: JSON.stringify({ viewTeam }),
      })
      setMsg(`「${name}」可看团别已更新（旧登录态已吊销）`)
      setTimeout(() => setMsg(''), 2500)
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
      <h2>「我的」页面账号（展示端登录用）</h2>
      <p className="hint">
        账号由<b>这里手动设置</b>（姓名要跟主播台账里的一致，姓名已存在时保存 = 覆盖更新）。
        <b>2026-10-18 起主播也可凭艺名在展示端自助认领开号</b>（登录门返回可认领 → 认领弹窗设置密码），
        这里仍是管理员兜底入口：补手机号 / 改密 / 删号 / 改绑。
        <b>手机号只用于登录</b>：微信推送对接 2026-10-16 起走「扫码 → 给机器人发一条消息」——
        主播在「我的」页扫码加机器人后，用同一个微信随便发一条消息就会自动对接成功，
        4 位对接码只是可选兜底（也可以把页面上的 4 位对接码发给机器人）。手机号不再是对接凭证。
        历史自助注册遗留的账号是「待绑定」状态：主播在「我的」页绑定主播姓名后才能看个人数据，
        <b>绑定后主播自己不能解绑，只有这里能改绑</b>。
      </p>
      <div className="row row-end">
        <label className="field">
          姓名
          <input
            placeholder="与主播台账里的一致"
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && name.trim() && pass && add()}
          />
        </label>
        <label className="field">
          手机号（选填）
          <input
            placeholder="如 13800000000"
            inputMode="numeric"
            value={phone}
            onChange={(e) => setPhone(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && name.trim() && pass && add()}
          />
        </label>
        <label className="field">
          密码
          <input
            placeholder="4~64 字符，任意字符（不 trim）"
            value={pass}
            onChange={(e) => setPass(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && name.trim() && pass && add()}
          />
        </label>
        <button className="primary" disabled={!name.trim() || !pass} onClick={add}>
          保存
        </button>
      </div>
      {items.length ? (
        <div className="table-scroll">
          <table>
            <thead>
              <tr>
                <th>账号 / 姓名</th>
              <th>手机号</th>
              <th>来源</th>
              <th>绑定状态</th>
              {/* 2026-10-18「我的页面账号」口径：只显示脱敏后的 QQ 邮箱与是否待改密，
                  不显示明文 QQ / 邮箱（后端 /api/admin/my-accounts 下发 emailMask/qqBound/mustChangePass）。 */}
              <th>QQ 邮箱</th>
              <th>可看团别</th>
              <th>添加时间</th>
              <th className="col-actions"></th>
            </tr>
          </thead>
          <tbody>
            {items.map((a) => (
              <tr key={a.name}>
                <td>{a.name}</td>
                <td className="hint">{a.phone}</td>
                <td className="hint">
                  {a.source === 'register' ? '自助注册' : a.source === 'claim' ? '艺名认领' : '后台添加'}
                </td>
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
                <td>
                  {/* QQ 绑定状态（脱敏）：后端新增字段缺失时显示「—」，不误报「未绑定」；
                      管理员账号不参与 QQ 绑定，直接显示「—」。 */}
                  {a.isAdmin ? (
                    <span className="hint">—</span>
                  ) : a.qqBound === undefined && a.emailMask === undefined ? (
                    <span className="hint" title="后端尚未下发 emailMask/qqBound，升级后自动显示">
                      —
                    </span>
                  ) : a.qqBound ? (
                    <span title="只显示脱敏地址，管理端不显示完整 QQ 号">{a.emailMask || '已绑定'}</span>
                  ) : a.emailMask ? (
                    <span className="hint" title="历史非 QQ 邮箱：忘记密码不支持自助重置，只能联系管理员">
                      {a.emailMask}（非 QQ）
                    </span>
                  ) : (
                    <span className="hint" title="未绑 QQ 的账号忘记密码只能联系管理员重置">
                      未绑定
                    </span>
                  )}
                  {a.mustChangePass && (
                    <span
                      className="badge b-paused"
                      style={{ marginLeft: 6 }}
                      title="该账号用临时密码登录过，正在等待用户自行修改密码"
                    >
                      待改密
                    </span>
                  )}
                </td>
                <td>
                  {/* 可看团别只对**管理员观看账号**开放：普通主播账号按绑定主播
                      所在团走，改了反而会和台账不一致，所以只展示不提供下拉。
                      注意 td 上不能挂 .hint —— 控件层是 `font: inherit`，12.5px 的提示字号
                      会把下拉框缩小一档，和页面其它控件不一致。 */}
                  {a.isAdmin ? (
                    <select
                      value={a.viewTeam ?? ''}
                      onChange={(e) => setViewTeam(a.name, e.target.value)}
                    >
                      <option value="any">全团（可切换）</option>
                      <option value="male">只看男团</option>
                      <option value="female">只看女团</option>
                      <option value="">沿用旧口径</option>
                    </select>
                  ) : (
                    <span className="hint">按绑定主播</span>
                  )}
                </td>
                <td className="hint">{a.createdAt}</td>
                <td className="row-actions">
                  {a.isAdmin ? (
                    <span className="hint">管理员账号不能改绑</span>
                  ) : (
                    <button onClick={() => setRebindFor(rebindFor === a.name ? '' : a.name)}>
                      {a.bound ? '改绑' : '绑定主播'}
                    </button>
                  )}
                  <button className="danger" onClick={() => remove(a.name)}>
                    删除
                  </button>
                </td>
              </tr>
            ))}
            </tbody>
          </table>
        </div>
      ) : !accLoaded ? (
        <p className="hint">正在读取「我的」账号…</p>
      ) : (
        <div className="empty">
          还没有账号：用上面的「姓名 + 密码」添加一个，主播才能登录展示端「我的」页。
        </div>
      )}
      {rebindFor && (
        <div className="row row-top">
          <label className="field" style={{ flex: 1, minWidth: 220 }}>
            把「{rebindFor}」改绑到哪位主播
            <input
              placeholder="输入姓名搜索，如：狼哲"
              value={rebindQ}
              onChange={(e) => searchRebind(e.target.value)}
              style={{ width: '100%' }}
            />
            {rebindHits.length ? (
              <span className="field-hint hint-btns">
                {rebindHits.map((h) => (
                  <button key={h.personId} onClick={() => doRebind(rebindFor, h.personId, h.name)}>
                    {h.name}（{h.team === 'female' ? '女团' : '男团'}）
                  </button>
                ))}
              </span>
            ) : (
              <span className="field-hint">
                输入主播姓名开始搜索；改绑后账号名会跟着变成主播姓名，登录仍可用手机号。
              </span>
            )}
          </label>
          <button onClick={() => { setRebindFor(''); setRebindQ(''); setRebindHits([]) }}>取消</button>
        </div>
      )}
      {msg && <div className="alert good">{msg}</div>}
      {err && <div className="alert bad">{err}</div>}
    </section>
  )
}
