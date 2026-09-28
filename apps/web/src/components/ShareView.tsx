/**
 * 对外展示页（只读、无需口令）：/s/:planId
 *
 * 展示「音浪数据 + 分组」两件事：
 * - 手机：单列卡片 + 顶部吸顶汇总条，组内按音浪排
 * - PC：多列网格 + 右侧全场音浪排行
 * - 趋势页：每组每日音浪曲线（后端算好，前端只画）
 *
 * 数据来自 /api/public/plans/:id（后端已裁掉 personId / key 等内部字段）。
 * 音浪由后端按方案口径实时取；取不到时 stale=true，页面用方案保存时的快照并标注。
 */
import { useEffect, useMemo, useState } from 'react';
import { formatWave } from '@pk/shared';
import { api, type PublicGroup, type PublicPlan, type PublicPlayer } from '../api.js';
import { TrendChart } from './TrendChart.js';
import { groupColor } from '../lib/board.js';
import { AlertIcon, ClockIcon, RefreshIcon, TrendIcon } from './icons.js';

type Tab = 'groups' | 'ranking' | 'trend';

export function ShareView({ planId }: { planId: string }) {
  const [plan, setPlan] = useState<PublicPlan | null>(null);
  const [error, setError] = useState('');
  const [tab, setTab] = useState<Tab>('groups');
  const [nonce, setNonce] = useState(0);

  useEffect(() => {
    let alive = true;
    setError('');
    setPlan(null);
    api
      .publicPlan(Number(planId))
      .then((p) => alive && setPlan(p))
      .catch((e) => alive && setError((e as Error).message));
    return () => {
      alive = false;
    };
  }, [planId, nonce]);

  // 榜单页长时间开着，顺手按分钟刷一次音浪
  useEffect(() => {
    const t = setInterval(() => setNonce((n) => n + 1), 60_000);
    return () => clearInterval(t);
  }, []);

  const period = useMemo(() => {
    if (!plan) return '';
    return plan.metric === 'total' ? '总音浪' : `近 ${plan.days} 天音浪`;
  }, [plan]);

  if (error) {
    return (
      <Frame>
        <div className="flex flex-col items-center gap-3 py-24 text-center">
          <AlertIcon size={28} className="text-ink3" />
          <p className="text-[15px] text-ink2">{error}</p>
        </div>
      </Frame>
    );
  }

  if (!plan) {
    return (
      <Frame>
        <div className="py-24 text-center text-[15px] text-ink3">加载中…</div>
      </Frame>
    );
  }

  return (
    <Frame>
      {/* ------------------------------ 头部 ------------------------------ */}
      <header
        className="rounded-2xl px-5 py-5 text-white shadow-card sm:px-7 sm:py-6"
        style={{ background: 'linear-gradient(120deg, #ec4899 0%, #8b5cf6 55%, #3b82f6 100%)' }}
      >
        <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
          {plan.brand && <span className="text-[22px] font-black tracking-wide sm:text-[26px]">{plan.brand}</span>}
          <span className="text-[15px] font-semibold opacity-90 sm:text-[17px]">{plan.title}</span>
        </div>

        {/* 汇总指标：手机 2 列，PC 一行铺开 */}
        <dl className="mt-4 grid grid-cols-2 gap-x-4 gap-y-3 sm:mt-5 sm:grid-cols-4 lg:grid-cols-5">
          <Stat label="总音浪" value={formatWave(plan.counts.totalWave)} accent />
          <Stat label="参赛人数" value={`${plan.counts.players} 人`} />
          <Stat label="分组数" value={`${plan.counts.groups} 组`} />
          <Stat label="统计口径" value={period} />
          <Stat
            label="数据截止"
            value={plan.dates[0] ?? (plan.stale ? '快照' : '-')}
            className="col-span-2 sm:col-span-1"
          />
        </dl>

        {plan.stale && (
          <p className="mt-3 rounded-lg bg-white/15 px-3 py-2 text-[12px] leading-relaxed">
            实时音浪暂时取不到，当前显示方案保存时的数据。
          </p>
        )}
      </header>

      {/* --------------------------- 吸顶汇总 + 切换 --------------------------- */}
      <div className="sticky top-0 z-10 -mx-4 mt-4 border-b border-line bg-canvas/92 px-4 py-2 backdrop-blur sm:-mx-5 sm:px-5">
        <div className="mx-auto flex max-w-[1180px] items-center gap-2">
          <TabButton active={tab === 'groups'} onClick={() => setTab('groups')}>
            分组
          </TabButton>
          <TabButton active={tab === 'ranking'} onClick={() => setTab('ranking')}>
            音浪排行
          </TabButton>
          {plan.trend && plan.trend.dates.length > 1 && (
            <TabButton active={tab === 'trend'} onClick={() => setTab('trend')}>
              趋势
            </TabButton>
          )}
          <span className="flex-1" />
          <span className="nums hidden text-[11.5px] text-ink3 sm:inline">
            更新于 {new Date(plan.createdAt).toLocaleString('zh-CN', { hour12: false })}
          </span>
          <button
            type="button"
            onClick={() => setNonce((n) => n + 1)}
            title="刷新音浪"
            className="flex h-7 w-7 items-center justify-center rounded-ctl text-ink3 transition-colors hover:bg-subtle hover:text-ink"
          >
            <RefreshIcon size={15} />
          </button>
        </div>
      </div>

      {tab === 'groups' && <GroupGrid groups={plan.groups} />}
      {tab === 'ranking' && <Ranking players={plan.ranking} total={plan.counts.totalWave} />}
      {tab === 'trend' && plan.trend && (
        <div className="py-4">
          <TrendChart trend={plan.trend} />
        </div>
      )}

      <footer className="pb-10 pt-6 text-center text-[11px] text-ink3">只读展示页 · 数据每小时更新</footer>
    </Frame>
  );
}

/* --------------------------------- 布局壳 --------------------------------- */

function Frame({ children }: { children: React.ReactNode }) {
  return (
    <div className="min-h-screen bg-canvas">
      {/* mx-auto + max-w：手机满宽、PC 居中限宽，两端都成立 */}
      <div className="mx-auto flex max-w-[1180px] flex-col gap-0 px-4 pt-4 sm:px-5 sm:pt-6">{children}</div>
    </div>
  );
}

function Stat({
  label,
  value,
  accent,
  className = '',
}: {
  label: string;
  value: string;
  accent?: boolean;
  className?: string;
}) {
  return (
    <div className={className}>
      <dt className="text-[11px] opacity-75">{label}</dt>
      <dd className={`nums mt-0.5 text-[17px] font-bold sm:text-[19px] ${accent ? 'text-white' : ''}`}>{value}</dd>
    </div>
  );
}

function TabButton({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`h-7 rounded-ctl px-3 text-[12.5px] font-medium transition-colors ${
        active ? 'bg-ink text-white' : 'text-ink2 hover:bg-subtle'
      }`}
    >
      {children}
    </button>
  );
}

/* --------------------------------- 分组视图 --------------------------------- */

function GroupGrid({ groups }: { groups: PublicGroup[] }) {
  if (groups.length === 0) {
    return <div className="py-20 text-center text-[14px] text-ink3">这个方案还没有分组</div>;
  }
  const maxTotal = Math.max(...groups.map((g) => g.total), 1);
  return (
    // 手机单列；≥640 两列；≥1024 三列 —— 不用 JS 算列数，纯 CSS 断点
    <div className="grid grid-cols-1 items-start gap-3 py-4 sm:grid-cols-2 sm:gap-4 lg:grid-cols-3">
      {groups.map((g, i) => (
        <GroupCard key={i} group={g} colorIndex={i} maxTotal={maxTotal} />
      ))}
    </div>
  );
}

function GroupCard({
  group,
  colorIndex,
  maxTotal,
}: {
  group: PublicGroup;
  colorIndex: number;
  maxTotal: number;
}) {
  const color = groupColor(colorIndex);
  const maxWave = Math.max(...group.players.map((p) => p.wave), 1);
  return (
    <section
      className="overflow-hidden rounded-xl border-t-[5px] bg-surface shadow-card"
      style={{ borderTopColor: color.bar }}
    >
      <div className="px-3.5 py-3" style={{ background: color.soft }}>
        <div className="flex items-baseline justify-between gap-2">
          <h2 className="truncate text-[15px] font-bold" style={{ color: color.ink }}>
            {group.name}
          </h2>
          <span className="shrink-0 text-[15px] font-black" style={{ color: color.ink }}>
            {formatWave(group.total)}
          </span>
        </div>
        <div
          className="mt-1 flex flex-wrap items-center gap-x-2.5 gap-y-0.5 text-[11.5px]"
          style={{ color: color.ink, opacity: 0.85 }}
        >
          <span className="nums flex items-center gap-1 font-medium">
            <ClockIcon size={11} />
            {group.slot} 开赛
          </span>
          <span className="nums">{group.count} 人</span>
          <span className="nums">人均 {formatWave(group.count ? group.total / group.count : 0)}</span>
        </div>
        {/* 组间音浪占比条，一眼看出哪组偏重 */}
        <div className="mt-2 h-1 w-full overflow-hidden rounded-full bg-white/60">
          <div
            className="h-full rounded-full"
            style={{ width: `${Math.max(4, (group.total / maxTotal) * 100)}%`, background: color.bar }}
          />
        </div>
      </div>

      <ul className="flex flex-col gap-1 px-2 py-2">
        {group.players.map((p) => (
          <li
            key={p.name}
            className="flex items-center gap-2 rounded-full py-1.5 pl-1.5 pr-2.5"
            style={{ background: color.soft }}
          >
            <span
              className="nums flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-white text-[10.5px] font-bold"
              style={{ color: color.ink }}
            >
              {p.group_rank}
            </span>
            <span className="min-w-0 flex-1 truncate text-[13px] font-semibold text-ink">{p.name}</span>
            {p.leader && <span className="shrink-0 text-[10.5px] text-ink3">师傅 {p.leader}</span>}
            <span className="nums shrink-0 text-[12px] font-semibold text-ink2">{formatWave(p.wave)}</span>
            <span
              className="hidden h-1 w-12 shrink-0 overflow-hidden rounded-full bg-white sm:block"
              title={`组内第 ${p.group_rank} 名`}
            >
              <span
                className="block h-full rounded-full"
                style={{ width: `${Math.max(6, (p.wave / maxWave) * 100)}%`, background: color.bar }}
              />
            </span>
          </li>
        ))}
        {group.players.length === 0 && <li className="py-3 text-center text-[12px] text-ink3">暂无人员</li>}
      </ul>
    </section>
  );
}

/* -------------------------------- 音浪排行 -------------------------------- */

function Ranking({ players, total }: { players: PublicPlayer[]; total: number }) {
  if (players.length === 0) {
    return <div className="py-20 text-center text-[14px] text-ink3">暂无音浪数据</div>;
  }
  const max = Math.max(...players.map((p) => p.wave), 1);
  return (
    <div className="mx-auto w-full max-w-[720px] py-4">
      <div className="overflow-hidden rounded-xl bg-surface shadow-card">
        <div className="flex items-center gap-2 border-b border-line px-4 py-2.5 text-[12px] text-ink2">
          <TrendIcon size={14} />
          全场音浪排行
          <span className="flex-1" />
          <span className="nums">合计 {formatWave(total)}</span>
        </div>
        <ul className="divide-y divide-line">
          {players.map((p, i) => (
            <li key={`${p.name}-${i}`} className="flex items-center gap-3 px-4 py-2.5">
              <span
                className={`nums flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-[11.5px] font-bold ${
                  i < 3 ? 'bg-brand text-white' : 'bg-subtle text-ink2'
                }`}
              >
                {p.rank}
              </span>
              <span className="min-w-0 flex-1">
                <span className="flex items-baseline gap-1.5">
                  <span className="truncate text-[13.5px] font-semibold text-ink">{p.name}</span>
                  {p.leader && (
                    <span className="shrink-0 text-[11px] text-ink3">师傅 {p.leader}</span>
                  )}
                </span>
                <span className="mt-1 block h-1 w-full overflow-hidden rounded-full bg-subtle">
                  <span
                    className="block h-full rounded-full bg-brand"
                    style={{ width: `${Math.max(3, (p.wave / max) * 100)}%` }}
                  />
                </span>
              </span>
              <span className="nums w-16 shrink-0 text-right text-[13px] font-bold text-ink">{formatWave(p.wave)}</span>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
