/**
 * 管理员后台的登录门。
 *
 * 后端在 ADMIN_PASSWORD 没配时完全不鉴权（本地开发友好），所以这里先问
 * /api/auth.session：enabled=false 就直接放行；enabled=true 且手里的 token
 * 失效就停在登录页。任何请求返回 401 也会广播事件把人踢回来。
 */
import { useCallback, useEffect, useState, type ReactNode } from 'react';
import { api, setToken, UNAUTHORIZED_EVENT } from '../api.js';
import { Button } from './ui.js';
import { LayersIcon, LockIcon } from './icons.js';

type Phase = 'checking' | 'need-login' | 'in';

/* 模块级缓存：AuthGate 已经问过 /api/auth.session，TopBar 不用再问一遍 */
let authEnabled: boolean | null = null;
const authListeners = new Set<() => void>();

/** 后端是否要求口令。false 时不显示「退出登录」——没得可退 */
export function useAuthEnabled(): boolean {
  const [enabled, setEnabled] = useState(authEnabled ?? false);
  useEffect(() => {
    const l = () => setEnabled(authEnabled ?? false);
    authListeners.add(l);
    return () => {
      authListeners.delete(l);
    };
  }, []);
  return enabled;
}

function publishAuthEnabled(v: boolean) {
  authEnabled = v;
  authListeners.forEach((l) => l());
}

export function AuthGate({ children }: { children: ReactNode }) {
  const [phase, setPhase] = useState<Phase>('checking');
  const [password, setPassword] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        const s = await api.session();
        if (!alive) return;
        publishAuthEnabled(s.enabled);
        setPhase(!s.enabled || s.ok ? 'in' : 'need-login');
      } catch {
        // 后端没起来或网络不通：先按已登录处理，让真正的页面去报错更有信息量
        if (alive) setPhase('in');
      }
    })();
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => {
    const onUnauthorized = () => setPhase((p) => (p === 'in' ? 'need-login' : p));
    window.addEventListener(UNAUTHORIZED_EVENT, onUnauthorized);
    return () => window.removeEventListener(UNAUTHORIZED_EVENT, onUnauthorized);
  }, []);

  const submit = useCallback(async () => {
    if (!password || busy) return;
    setBusy(true);
    setError('');
    try {
      const { token } = await api.login(password);
      setToken(token);
      setPassword('');
      setPhase('in');
    } catch (e) {
      setError((e as Error).message.replace(/^\d+\s*/, '') || '口令不正确');
    } finally {
      setBusy(false);
    }
  }, [password, busy]);

  if (phase === 'in') return <>{children}</>;

  return (
    <div className="flex min-h-screen items-center justify-center bg-canvas px-5 py-10">
      <div className="w-full max-w-[360px]">
        <div className="flex flex-col items-center gap-3">
          <span className="flex h-11 w-11 items-center justify-center rounded-2xl bg-brand shadow-card">
            <LayersIcon size={24} />
          </span>
          <h1 className="text-[19px] font-semibold text-ink">PK 挑战赛 · 均衡分组</h1>
          <p className="text-center text-[12.5px] text-ink2">管理员后台，请输入口令</p>
        </div>

        <form
          className="mt-6 flex flex-col gap-3 rounded-panel border border-line bg-surface p-5 shadow-card"
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <label className="flex flex-col gap-1.5">
            <span className="text-[12px] font-medium text-ink2">管理员口令</span>
            <input
              type="password"
              autoFocus
              autoComplete="current-password"
              value={password}
              onChange={(e) => {
                setPassword(e.target.value);
                setError('');
              }}
              placeholder="ADMIN_PASSWORD"
              className="h-9 rounded-ctl border border-line bg-canvas px-3 text-[14px] text-ink outline-none focus:border-brand"
            />
          </label>

          {error && <div className="rounded-ctl bg-bad-soft px-3 py-2 text-[12.5px] text-bad">{error}</div>}

          <Button type="submit" variant="primary" icon={<LockIcon size={14} />} disabled={!password || busy}>
            {busy ? '验证中…' : '进入后台'}
          </Button>

          <p className="text-center text-[11.5px] leading-relaxed text-ink3">
            对外展示页无需口令，直接访问
            <code className="mx-1 rounded bg-subtle px-1 py-0.5">/s/方案号</code>
          </p>
        </form>

        {phase === 'checking' && (
          <p className="mt-4 text-center text-[12px] text-ink3">正在检查登录状态…</p>
        )}
      </div>
    </div>
  );
}
