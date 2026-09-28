import { useStore } from '../store.js';
import { ROUTES, go, type Route } from '../lib/nav.js';
import { Button, Pill } from './ui.js';
import { ChartIcon, GridIcon, HistoryIcon, HomeIcon, LayersIcon, LogoutIcon } from './icons.js';
import { useAuthEnabled } from './AuthGate.js';

const ICONS: Record<Route, React.ReactNode> = {
  home: <HomeIcon size={15} />,
  data: <ChartIcon size={15} />,
  board: <GridIcon size={15} />,
};

/**
 * 应用顶栏：品牌 + 三段导航 + 数据源状态。
 * 导航状态存在 URL hash 里，所以这一栏只负责渲染 + 跳转，不持有状态。
 */
export function NavBar({ route }: { route: Route }) {
  const dbOk = useStore((s) => s.dbOk);
  const latestDate = useStore((s) => s.latestDate);
  const anchorCount = useStore((s) => s.anchorCount);
  const loading = useStore((s) => s.loading);
  const logout = useStore((s) => s.logout);
  const openPlans = useStore((s) => s.openPlans);
  const authEnabled = useAuthEnabled();

  return (
    <div className="appbar">
      <div className="mx-auto w-full max-w-[1440px] px-5 sm:px-6">
        <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 py-3">
          <div className="flex items-center gap-2.5">
            <span className="grid h-8 w-8 place-items-center rounded-ctl bg-brand text-white shadow-raise">
              <LayersIcon size={16} />
            </span>
            <div className="leading-tight">
              <h1 className="text-[16px] font-bold tracking-tight text-ink">PK 挑战赛 · 均衡分组</h1>
              <p className="text-[11px] text-ink3">抖音音浪赛 · 分组与公布</p>
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2.5">
            <Pill tone={dbOk ? 'good' : dbOk === false ? 'bad' : 'neutral'}>
              <span
                className="h-1.5 w-1.5 rounded-full"
                style={{ background: dbOk ? '#12b76a' : dbOk === false ? '#f04438' : '#98a2b3' }}
              />
              {loading ? '读取中' : dbOk ? 'douyinxs 已连接' : dbOk === false ? '数据库未连接' : '检测中'}
            </Pill>
            {latestDate && <span className="text-[12px] text-ink2">数据截止 {latestDate}</span>}
            {anchorCount > 0 && <span className="text-[12px] text-ink3">主播库 {anchorCount} 人</span>}
            <Button icon={<HistoryIcon size={14} />} onClick={() => void openPlans()}>
              历史方案
            </Button>
            {authEnabled && (
              <Button icon={<LogoutIcon size={14} />} onClick={() => void logout()} title="退出登录">
                退出
              </Button>
            )}
          </div>
        </div>

        <nav aria-label="主导航" className="-mb-px flex items-end gap-1 overflow-x-auto">
          {ROUTES.map((r) => (
            <button
              key={r.id}
              type="button"
              onClick={() => go(r.id)}
              aria-current={route === r.id ? 'page' : undefined}
              title={r.hint}
              className="navlink shrink-0"
            >
              {ICONS[r.id]}
              {r.label}
            </button>
          ))}
        </nav>
      </div>
    </div>
  );
}
