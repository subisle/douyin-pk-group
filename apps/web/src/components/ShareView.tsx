/**
 * 对外展示页（只读、无需口令）：/s/:planId
 *
 * 这一页只回答六件事：分好的组、每人音浪、哪一天、曲线、赛程时长、第几页。
 * 顺序也是按「先看赛程，再看分组，最后看走势」排的：
 *   1. 头部    方案名 + 总音浪 / 人数 / 组数
 *   2. 日期条  年-月-日切换（吸顶，滚到哪儿都能换）
 *   3. 赛程条  几点开赛、每组多久、全程多久
 *   4. 分组    组分页，一页一组（手机）/ 一页三组（PC）
 *   5. 曲线    每日音浪折线
 *
 * 数据来自 /api/public/plans/:id（后端已裁掉 personId / key 等内部字段）。
 * 音浪由后端按方案口径实时取；取不到时 stale=true，页面用方案保存时的快照并标注。
 */
import { useEffect, useMemo, useState } from 'react';
import { formatWave } from '@pk/shared';
import { api, type PublicGroup, type PublicPlan } from '../api.js';
import { TrendChart } from './TrendChart.js';
import { groupColor } from '../lib/board.js';
import { AlertIcon, ChevronLeftIcon, ClockIcon, RefreshIcon, TrendIcon } from './icons.js';

/** 一页放几组：手机一屏一组，PC 一屏三组 */
function groupsPerPage(width: number): number {
  if (width >= 1024) return 3;
  if (width >= 640) return 2;
  return 1;
}

/** 内置分组数：后端分组不足 9 组时补空组占位，把第 9 组的赛程时段 / 分页 / 配色先占好位 */
const BUILTIN_GROUPS = 9;

/**
 * 后端返回的分组不足 9 组时，内置补空组占位。
 * 补位组没有人员、没有音浪，只占一个赛程时段和一张卡位；等后台真的分出第 9 组，
 * 这里的占位会被真实数据顶掉，不用改前端。
 */
function withBuiltinGroups(groups: PublicGroup[]): PublicGroup[] {
  if (groups.length >= BUILTIN_GROUPS) return groups;
  const out = groups.slice();
  for (let i = groups.length; i < BUILTIN_GROUPS; i += 1) {
    out.push({ name: `第 ${i + 1} 组`, slot: '', count: 0, total: 0, players: [], builtin: true });
  }
  return out;
}

function parseHm(s: string): number {
  const [h, m] = s.split(':').map((x) => Number(x));
  return (Number.isFinite(h) ? h : 0) * 60 + (Number.isFinite(m) ? m : 0);
}

function fmtHm(min: number): string {
  const v = ((min % 1440) + 1440) % 1440;
  return `${String(Math.floor(v / 60)).padStart(2, '0')}:${String(v % 60).padStart(2, '0')}`;
}

function fmtDuration(min: number): string {
  const h = Math.floor(min / 60);
  const m = min % 60;
  return h ? (m ? `${h} 小时 ${m} 分` : `${h} 小时`) : `${m} 分钟`;
}

function useViewportWidth(): number {
  const [w, setW] = useState(() => (typeof window === 'undefined' ? 1280 : window.innerWidth));
  useEffect(() => {
    const on = () => setW(window.innerWidth);
    on();
    window.addEventListener('resize', on);
    return () => window.removeEventListener('resize', on);
  }, []);
  return w;
}

export function ShareView({ planId }: { planId: string }) {
  const [plan, setPlan] = useState<PublicPlan | null>(null);
  const [error, setError] = useState('');
  const [nonce, setNonce] = useState(0);
  /** -1 = 按方案口径（总音浪 / 近 N 天）；>=0 = 按 daily.dates 下标看某一天 */
  const [dateIdx, setDateIdx] = useState(-1);
  const [page, setPage] = useState(0);
  const width = useViewportWidth();
  const perPage = groupsPerPage(width);

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

  // 日期或分页粒度一变，回到第一页，否则可能停在不存在的页码上
  useEffect(() => setPage(0), [dateIdx, perPage]);

  /** 按选中日期重算分组（名次依赖音浪，必须重排） */
  const dayView = useMemo(() => {
    if (!plan?.daily || dateIdx < 0) return null;
    const d = plan.daily;
    if (dateIdx >= d.dates.length) return null;
    const i = dateIdx;
    const groups: PublicGroup[] = d.groups.map((dg, gi) => {
      const players = dg.players
        .map((p) => ({
          name: p.name,
          leader: p.leader,
          wave: p.values[i] ?? 0,
          rank: 0,
          group_rank: 0,
        }))
        .sort((a, b) => b.wave - a.wave || a.name.localeCompare(b.name, 'zh-Hans-CN'));
      players.forEach((p, k) => {
        p.group_rank = k + 1;
      });
      return {
        name: dg.name,
        slot: plan.groups[gi]?.slot ?? '',
        count: players.length,
        total: players.reduce((s, p) => s + p.wave, 0),
        players,
      };
    });
    const all = groups.flatMap((g) => g.players).sort((a, b) => b.wave - a.wave);
    all.forEach((p, k) => {
      p.rank = k + 1;
    });
    return { groups, total: all.reduce((s, p) => s + p.wave, 0), date: d.dates[i] };
  }, [plan, dateIdx]);

  const metricLabel = useMemo(() => {
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

  const realGroups = dayView?.groups ?? plan.groups;
  // 不足 9 组时内置补空组占位，第 9 组的时段和卡位先占好
  const groups = withBuiltinGroups(realGroups);
  const builtinCount = groups.length - realGroups.length;
  const totalWave = dayView?.total ?? plan.counts.totalWave;
  const pageCount = Math.max(1, Math.ceil(groups.length / perPage));
  // 夹住页码：窄屏翻到第 5 页后拉宽窗口，perPage 变小会让当前页越界，
  // 不夹的话 slice 切出空数组，分组区会闪一帧空白
  const safePage = Math.min(page, pageCount - 1);
  const pageFrom = safePage * perPage;
  const shown = groups.slice(safePage * perPage, safePage * perPage + perPage);

  // 赛程：开赛时刻 + 每组间隔 = 每组时段；末组结束时刻 = 全程时长
  const step = Math.max(1, plan.schedule.intervalMin);
  const startMin = parseHm(plan.schedule.start);
  const slots = groups.map((g, i) => ({
    name: g.name,
    from: startMin + i * step,
    to: startMin + (i + 1) * step,
    index: i,
  }));
  const fullMin = groups.length * step;

  const jumpTo = (i: number) => setPage(Math.min(Math.floor(i / perPage), pageCount - 1));

  return (
    <Frame>
      {/* ------------------------------ 头部 ------------------------------ */}
      <header className="masthead relative overflow-hidden rounded-2xl shadow-pop">
        <div
          className="relative px-5 py-4 text-white sm:px-7 sm:py-5"
          style={{ background: 'linear-gradient(135deg, #1b2430 0%, #26364a 55%, #33415a 100%)' }}
        >
          <div className="flex items-start gap-3">
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-baseline gap-x-2.5 gap-y-0.5">
                {plan.brand && (
                  <span className="text-[20px] font-black tracking-wide sm:text-[24px]">{plan.brand}</span>
                )}
                <span className="text-[14px] font-semibold text-white/80 sm:text-[16px]">{plan.title}</span>
              </div>
            </div>
            <button
              type="button"
              onClick={() => setNonce((n) => n + 1)}
              title="刷新音浪"
              aria-label="刷新音浪"
              className="flex h-8 w-8 shrink-0 items-center justify-center rounded-ctl bg-white/10 text-white/85 transition-colors hover:bg-white/20 hover:text-white"
            >
              <RefreshIcon size={15} />
            </button>
          </div>

          {/* 汇总：手机 2 列，PC 一行铺开 */}
          <dl className="mt-3.5 grid grid-cols-2 gap-x-4 gap-y-3 sm:grid-cols-4">
            <Stat label="总音浪" value={formatWave(totalWave)} accent />
            <Stat label="参赛人数" value={`${plan.counts.players} 人`} />
            <Stat label="分组" value={`${plan.counts.groups} 组`} />
            <Stat label={dayView ? '统计日期' : '统计口径'} value={dayView ? dayView.date : metricLabel} />
          </dl>

          {plan.stale && (
            <p className="mt-3 rounded-lg bg-white/12 px-3 py-2 text-[12px] leading-relaxed text-white/90">
              实时音浪暂时取不到，当前显示方案保存时的数据。
            </p>
          )}
        </div>
      </header>

      {/* --------------------------- 日期条（吸顶） --------------------------- */}
      {plan.daily && plan.daily.dates.length > 0 && (
        <div className="sticky top-0 z-20 -mx-4 mt-3 border-b border-line bg-canvas/94 px-4 py-2 backdrop-blur sm:-mx-5 sm:px-5">
          <div className="mx-auto flex max-w-[1180px] items-center gap-2">
            <span className="shrink-0 text-[11.5px] font-medium text-ink3">日期</span>
            <div className="hide-scrollbar flex min-w-0 flex-1 gap-1.5 overflow-x-auto">
              <DateChip active={dateIdx === -1} onClick={() => setDateIdx(-1)}>
                {metricLabel}
              </DateChip>
              {plan.daily.dates.map((d, i) => (
                <DateChip key={d} active={dateIdx === i} onClick={() => setDateIdx(i)}>
                  {d}
                </DateChip>
              ))}
            </div>
          </div>
        </div>
      )}

      {/* ------------------------------ 赛程时长 ------------------------------ */}
      <section className="mt-3 rounded-xl border border-line bg-surface px-4 py-3 shadow-card">
        <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
          <h2 className="flex items-center gap-1.5 text-[13px] font-bold text-ink">
            <ClockIcon size={13} className="text-ink3" />
            赛程时长
          </h2>
          <span className="nums text-[12.5px] text-ink2">
            {fmtHm(startMin)} 开赛 · 每组 {step} 分钟 · 共 {groups.length} 组 · 全程{' '}
            <b className="font-bold text-ink">{fmtDuration(fullMin)}</b>（至 {fmtHm(startMin + fullMin)}）
          </span>
        </div>
        {/* 每组时段：手机上横向滑动，点一下直接跳到该组所在页 */}
        <div className="rail hide-scrollbar mt-2.5 flex gap-1.5 overflow-x-auto pb-1">
          {slots.map((s) => {
            const onPage = s.index >= pageFrom && s.index < pageFrom + perPage;
            const c = groupColor(s.index);
            return (
              <button
                key={s.name}
                type="button"
                onClick={() => jumpTo(s.index)}
                aria-current={onPage ? 'true' : undefined}
                data-group-slot={s.name}
                className={`flex shrink-0 items-center gap-1.5 rounded-ctl border px-2.5 py-1.5 text-[11.5px] transition-all ${
                  onPage
                    ? 'border-transparent text-ink shadow-card'
                    : 'border-line bg-subtle text-ink2 hover:-translate-y-px hover:border-line2 hover:bg-surface'
                }`}
                style={onPage ? { background: c.soft, color: c.ink } : undefined}
              >
                <span className="h-1.5 w-1.5 shrink-0 rounded-full" style={{ background: c.bar }} />
                <b className="font-semibold">{s.name}</b>
                <span className="nums opacity-70">
                  {fmtHm(s.from)}–{fmtHm(s.to)}
                </span>
              </button>
            );
          })}
        </div>
      </section>

      {/* ------------------------------ 分组 + 分页 ------------------------------ */}
      <section className="mt-3">
        <div className="mb-2 flex items-center gap-2">
          <h2 className="text-[13px] font-bold text-ink">
            分组
            {dayView && <span className="ml-1.5 font-normal text-ink3">{dayView.date}</span>}
          </h2>
          <span className="nums text-[11.5px] text-ink3">共 {groups.length} 组</span>
          {builtinCount > 0 && (
            <span className="text-[11.5px] text-ink3">含 {builtinCount} 个内置待补位</span>
          )}
          <span className="flex-1" />
          {pageCount > 1 && (
            <Pager page={safePage} pageCount={pageCount} onChange={setPage} />
          )}
        </div>

        {realGroups.length === 0 ? (
          <div className="py-16 text-center text-[14px] text-ink3">这个方案还没有分组</div>
        ) : (
          <div className="grid grid-cols-1 items-start gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {shown.map((g, i) => (
              <GroupCard
                key={g.name}
                group={g}
                colorIndex={safePage * perPage + i}
                step={step}
                from={slots[safePage * perPage + i]?.from}
              />
            ))}
          </div>
        )}

        {pageCount > 1 && (
          <div className="mt-3 flex justify-center">
            <Pager page={safePage} pageCount={pageCount} onChange={setPage} wide />
          </div>
        )}
      </section>

      {/* ------------------------------ 曲线图 ------------------------------ */}
      {plan.trend && plan.trend.dates.length > 1 && (
        <section className="mt-4 rounded-xl border border-line bg-surface px-3 py-3 shadow-card sm:px-4">
          <div className="mb-1 flex items-center gap-1.5 px-1">
            <TrendIcon size={14} className="text-ink3" />
            <h2 className="text-[13px] font-bold text-ink">每日音浪曲线</h2>
            <span className="flex-1" />
            <span className="nums text-[11.5px] text-ink3">
              {plan.trend.dates[0]} ~ {plan.trend.dates[plan.trend.dates.length - 1]}
            </span>
          </div>
          <TrendChart trend={plan.trend} />
        </section>
      )}

      <footer className="pb-8 pt-6 text-center">
        <p className="text-[11px] text-ink3">
          共 {plan.counts.players} 人 · {groups.length} 组 · 更新于{' '}
          {new Date(plan.createdAt).toLocaleString('zh-CN', { hour12: false })}
        </p>
        <p className="mt-1.5 text-[11px] text-ink3/80">按组名找到自己那一组，按组头时段开赛</p>
      </footer>
    </Frame>
  );
}

/* -------------------------------- 布局壳 --------------------------------- */

function Frame({ children }: { children: React.ReactNode }) {
  return (
    <div className="min-h-screen bg-canvas">
      {/* mx-auto + max-w：手机满宽、PC 居中限宽，两端都成立 */}
      <div className="mx-auto flex max-w-[1180px] flex-col gap-0 px-4 pb-4 pt-4 sm:px-5 sm:pt-5">
        {children}
      </div>
    </div>
  );
}

function Stat({
  label,
  value,
  accent,
}: {
  label: string;
  value: string;
  accent?: boolean;
}) {
  return (
    <div>
      <dt className="text-[11px] text-white/60">{label}</dt>
      <dd className={`nums mt-0.5 text-[17px] font-bold sm:text-[19px] ${accent ? 'text-white' : 'text-white/90'}`}>
        {value}
      </dd>
    </div>
  );
}

function DateChip({
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
      aria-pressed={active}
      data-date-chip={children}
      className={`h-7 shrink-0 rounded-ctl px-2.5 text-[12px] font-medium transition-colors ${
        active ? 'bg-ink text-white' : 'bg-surface text-ink2 shadow-card hover:bg-subtle'
      }`}
    >
      {children}
    </button>
  );
}

function Pager({
  page,
  pageCount,
  onChange,
  wide,
}: {
  page: number;
  pageCount: number;
  onChange: (p: number) => void;
  wide?: boolean;
}) {
  return (
    <div
      data-pager="true"
      className={`flex items-center gap-1 ${wide ? '' : 'rounded-ctl border border-line bg-surface p-0.5 shadow-card'}`}
    >
      <button
        type="button"
        onClick={() => onChange(page - 1)}
        disabled={page <= 0}
        aria-label="上一页"
        className="flex h-6 w-6 items-center justify-center rounded-md text-ink2 transition-colors hover:bg-subtle disabled:opacity-30 disabled:hover:bg-transparent"
      >
        <ChevronLeftIcon size={13} />
      </button>
      <span className="nums px-1 text-[11.5px] text-ink2">
        {page + 1} / {pageCount}
      </span>
      <button
        type="button"
        onClick={() => onChange(page + 1)}
        disabled={page >= pageCount - 1}
        aria-label="下一页"
        className="flex h-6 w-6 rotate-180 items-center justify-center rounded-md text-ink2 transition-colors hover:bg-subtle disabled:opacity-30 disabled:hover:bg-transparent"
      >
        <ChevronLeftIcon size={13} />
      </button>
    </div>
  );
}

/* --------------------------------- 分组卡 --------------------------------- */

function GroupCard({
  group,
  colorIndex,
  step,
  from,
}: {
  group: PublicGroup;
  colorIndex: number;
  step: number;
  from?: number;
}) {
  const color = groupColor(colorIndex);
  // 公布页只回答「谁在第几组、几点开赛」：音浪、口径、人均、组间占比一律不上版面。
  // 均衡结果对外只通过「组内序号」和「各组人数一致」体现，不摊开具体数字。
  const mentors = new Set(group.players.map((p) => p.leader).filter(Boolean));
  return (
    <section
      data-group-card={group.name}
      className={`flex flex-col overflow-hidden rounded-xl bg-surface shadow-pop ${
        group.builtin ? 'opacity-75' : ''
      }`}
    >
      {/* 顶色条：9 组同屏时，扫一眼色条就能数出这是第几组 */}
      <div className="h-1 w-full shrink-0" style={{ background: color.bar }} />

      <header className="px-3.5 pb-2.5 pt-3" style={{ background: color.soft }}>
        <div className="flex items-baseline justify-between gap-2">
          <h3 className="truncate text-[15px] font-bold" style={{ color: color.ink }}>
            {group.name}
          </h3>
          {from !== undefined && !group.builtin && (
            <span
              className="nums shrink-0 text-[12.5px] font-semibold"
              style={{ color: color.ink, opacity: 0.9 }}
            >
              {fmtHm(from)}–{fmtHm(from + step)}
            </span>
          )}
          {group.builtin && (
            <span
              className="shrink-0 rounded-full border border-current px-2 py-0.5 text-[11px] font-semibold"
              style={{ color: color.ink }}
            >
              待补员
            </span>
          )}
        </div>
        <div
          className="mt-1 flex items-center gap-1.5 text-[11.5px] font-medium"
          style={{ color: color.ink, opacity: 0.72 }}
        >
          {from !== undefined && !group.builtin && (
            <>
              <ClockIcon size={11} />
              <span className="nums">{fmtHm(from)} 开赛</span>
            </>
          )}
          {group.count > 0 && (
            <>
              <span className="opacity-40">·</span>
              <span className="nums">{group.count} 人</span>
            </>
          )}
          {mentors.size > 0 && (
            <>
              <span className="opacity-40">·</span>
              <span className="nums">{mentors.size} 位师傅</span>
            </>
          )}
        </div>
      </header>

      <ul className="roster flex flex-1 flex-col px-1.5 py-1">
        {group.players.map((p) => (
          <li
            key={p.name}
            data-player-card={p.name}
            className="flex items-center gap-2.5 rounded-lg px-2 py-[7px] transition-colors"
          >
            <span className="nums w-4 shrink-0 text-right text-[11.5px] font-semibold text-ink3">
              {p.group_rank}
            </span>
            <span className="min-w-0 flex-1 truncate text-[15px] font-semibold leading-6 text-ink">
              {p.name}
            </span>
            {p.leader && (
              <span className="shrink-0 rounded-full bg-subtle px-2 py-[1px] text-[11px] text-ink2">
                师傅 {p.leader}
              </span>
            )}
          </li>
        ))}
        {group.players.length === 0 && (
          <li className="px-2 py-6 text-center text-[12px] text-ink3">
            {group.builtin ? '内置占位，后台分出该组后自动填实' : '暂无人员'}
          </li>
        )}
      </ul>
    </section>
  );
}
