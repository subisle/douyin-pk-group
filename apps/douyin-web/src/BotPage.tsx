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
  const [qr, setQr] = useState<{ qrcode: string; url: string } | null>(null)
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
    try {
      const d = await api<{ qrcode: string; url: string }>('/api/admin/bots/weixin/qrcode')
      setQr(d)
      // 轮询扫码状态，扫完自动停
      if (pollRef.current) clearInterval(pollRef.current)
      pollRef.current = setInterval(async () => {
        try {
          const r = await api<{ status: string; tokenSaved: boolean; nickname: string }>(
            '/api/admin/bots/weixin/qrcode/check',
            { method: 'POST', body: JSON.stringify({ qrcode: d.qrcode }) },
          )
          if (r.tokenSaved) {
            if (pollRef.current) clearInterval(pollRef.current)
            setQr(null)
            load()
          }
        } catch {
          /* 过期等错误下一次轮询再看 */
        }
      }, 3000)
    } catch (e: any) {
      setError(e.message ?? String(e))
    }
  }

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
              勾选「管理员」后命令才会生效。（群聊事件一律忽略）
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
                <p className="hint">
                  二维码标识：{qr.qrcode}
                  {qr.url && (
                    <>
                      {' '}
                      · <a href={qr.url} target="_blank" rel="noreferrer">打开二维码链接</a>
                    </>
                  )}
                </p>
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
        <h2>微信推送 · 加好友二维码</h2>
        <p className="hint">
          主播在「我的」页绑定推送时，扫的就是这张码：把<b>机器人的微信名片二维码</b>传上来
          （微信里打开机器人账号 → 右上角 →「二维码名片」保存即可）。绑定后数据一入库就会私聊推送
          追分提醒 / 每日报告。
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

      <PublicSettingsCard />

      <MyAccountsCard />

      <section className="card">
        <h2>会话管理（群聊 / 私聊）</h2>
        <p className="hint">
          机器人收到过消息的会话都在这里；勾选「管理员」的会话才能触发命令（导入 / 导出 /
          查询等）。群聊和私聊各自独立勾选。
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

  const load = useCallback(() => {
    api('/api/admin/public/settings')
      .then(setSettings)
      .catch(() => {})
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
  ]

  return (
    <section className="card">
      <h2>公开站点设置（/st/m · /st/f）</h2>
      <p className="hint">
        后端级开关：关闭后接口不下发对应数据（不是前端藏）。链接关闭时访问一律「暂未公布」。
      </p>
      {settings ? (
        <table>
          <tbody>
            {FLAGS.map(([key, label]) => (
              <tr key={key}>
                <td>{label}</td>
                <td>
                  <input
                    type="checkbox"
                    checked={!!settings[key]}
                    onChange={(e) => toggle(key, e.target.checked)}
                  />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : (
        <p className="hint">加载中…</p>
      )}
      {msg && <div className="alert good">{msg}</div>}
    </section>
  )
}

/** 我的页面账号（2026-10-10）：展示端「我的」页的登录账号。
    没有注册：账号在这里手动设置（也可在主播管理右键单个主播设置），
    一个姓名对应一个纯数字密码（1:1），手机号选填。
    主播在 /st/m、/st/f 的「我的」页输入 姓名+密码 验证后看自己的数据；
    姓名已存在时保存 = 覆盖更新。 */
function MyAccountsCard() {
  const [items, setItems] = useState<any[]>([])
  const [name, setName] = useState('')
  const [phone, setPhone] = useState('')
  const [pass, setPass] = useState('')
  const [msg, setMsg] = useState('')
  const [err, setErr] = useState('')

  const load = useCallback(() => {
    api('/api/admin/my-accounts')
      .then((d) => setItems(d.items ?? []))
      .catch(() => {})
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

  return (
    <section className="card">
      <h2>「我的」页面账号（/st/m · /st/f）</h2>
      <p className="hint">
        没有注册：主播在展示端「我的」页输入 姓名 + 密码（纯数字）验证后查看自己的数据。
        账号在这里手动设置（手机号选填），姓名要跟主播台账里的一致；姓名已存在时保存 = 覆盖更新。
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
          placeholder="密码（纯数字）"
          inputMode="numeric"
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
              <th>姓名</th>
              <th>手机号</th>
              <th>密码</th>
              <th>添加时间</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {items.map((a) => (
              <tr key={a.name}>
                <td>{a.name}</td>
                <td className="hint">{a.phone}</td>
                <td className="hint">{a.pass}</td>
                <td className="hint">{a.createdAt}</td>
                <td>
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
      {msg && <div className="alert good">{msg}</div>}
      {err && <div className="alert bad">{err}</div>}
    </section>
  )
}
