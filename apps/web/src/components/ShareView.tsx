/**
 * 对外展示页（只读、无需口令）：/s/:planId 与 /st 公告页 · 苹果风
 *
 * 导航栏置顶，页面内三个分页（纯组件状态切换，URL 固定不变）：
 *   1. 每日音浪  支持选择日期的当日排名，每行带 开播 / 未开播 状态（daily_metric.is_live）
 *   2. PK 分组   只在比赛日展示（后端 matchDay：最新采集日 == 今天），组内按音浪名次排
 *   3. 总音浪    全场累计排名，同样带开播状态（最新一天）
 *
 * 管理端「展示页显示」开关继续生效；音浪数值关闭时后端不下发任何数字，
 * 每日/总榜模块随之消失，页面只剩分组名单。
 * 数据来自 /api/public/plans/:id；榜单页开着时每分钟静默刷新一次。
 */
import { useEffect, useMemo, useRef, useState } from 'react';
import { formatWave, slotTime } from '@pk/shared';
import { api, DEFAULT_DISPLAY, type PublicGroup, type PublicPlan } from '../api.js';
import { groupColor } from '../lib/board.js';
import { AlertIcon, RefreshIcon } from './icons.js';
import '../styles/share.css';

/** 展示页内置 9 组占位：后端分组不足时补空组，把时段和卡位先占好 */
const BUILTIN_GROUPS = 9;

function withBuiltinGroups(groups: PublicGroup[]): PublicGroup[] {
  if (groups.length >= BUILTIN_GROUPS) return groups;
  const out = groups.slice();
  for (let i = groups.length; i < BUILTIN_GROUPS; i += 1) {
    out.push({ name: `第 ${i + 1} 组`, slot: '', count: 0, total: 0, players: [], builtin: true });
  }
  return out;
}

function shortGroup(name: string): string {
  const m = name.match(/(\d+)/);
  return m ? `${m[1]}组` : name;
}

/* ------------------------------ 页内分页 ------------------------------ */
/** 模块页：纯组件状态切换（不动 URL），对外链接固定就是 /s/:planId 或 /st */
type Tab = 'daily' | 'groups' | 'total';

const TAB_META: Array<{ key: Tab; label: string }> = [
  { key: 'daily', label: '每日音浪' },
  { key: 'groups', label: 'PK 分组' },
  { key: 'total', label: '总音浪' },
];

export function ShareView({ planId }: { planId?: string }) {
  const [plan, setPlan] = useState<PublicPlan | null>(null);
  const [error, setError] = useState('');
  const [nonce, setNonce] = useState(0);
  /** 公告模式（不带 planId）：每次刷新都重新解析「最新上架」，链接 /st 永远指最新方案 */
  const [latestId, setLatestId] = useState<number | null>(null);

  useEffect(() => {
    let alive = true;
    setError('');
    (async () => {
      try {
        // 指定方案：直接取；公告模式（无 planId）：先解析最新上架，拿到 id 后再取方案
        let id = planId ? Number(planId) : latestId;
        if (!id) {
          const r = await api.publicPlans();
          const first = r.items[0];
          if (!alive) return;
          if (!first) {
            setError('暂无已上架的公告，请先在管理端生成分享链接');
            return;
          }
          setLatestId(first.id); // 变化后重新触发本 effect 加载方案
          return;
        }
        setPlan(null);
        const p = await api.publicPlan(id);
        if (alive) setPlan(p);
      } catch (e) {
        if (alive) setError((e as Error).message);
      }
    })();
    return () => {
      alive = false;
    };
  }, [planId, nonce, latestId]);

  // 榜单页长时间开着，按分钟静默刷新
  useEffect(() => {
    const t = setInterval(() => setNonce((n) => n + 1), 60_000);
    return () => clearInterval(t);
  }, []);

  // 页内分页：纯组件状态，URL 不变；切换时如果滚过了切换条，滚回切换条位置
  const [tab, setTab] = useState<Tab>('daily');
  /** 选中的日期下标；-1 = 默认最新一天（数据刷新日期变多后自动跟上） */
  const [dayIdx, setDayIdx] = useState(-1);
  const tabsRef = useRef<HTMLElement | null>(null);
  const goTab = (t: Tab) => {
    setTab(t);
    requestAnimationFrame(() => {
      const top = tabsRef.current?.offsetTop ?? 0;
      if (window.scrollY > top) window.scrollTo({ top, behavior: 'smooth' });
    });
  };

  /* ---- 数据派生（hooks 必须全部在早退 return 之前，否则加载态切换会炸 React #310） ---- */

  // 每日明细扁平化：所有参赛者的每日音浪 + 开播标记
  const dailyPlayers = useMemo(() => {
    if (!plan?.daily) return [];
    const out: Array<{ name: string; leader?: string; values: number[]; live: number[] }> = [];
    for (const g of plan.daily.groups) {
      for (const p of g.players) {
        out.push({ name: p.name, leader: p.leader, values: p.values, live: p.live ?? [] });
      }
    }
    return out;
  }, [plan]);

  const dayDates = plan?.daily?.dates ?? [];
  // 选中的日期下标：默认最新一天；数据刷新（日期变多）后夹回有效范围
  const safeDayIdx = dayDates.length ? Math.min(dayIdx < 0 ? dayDates.length - 1 : dayIdx, dayDates.length - 1) : 0;

  // 总榜带开播状态（取每日序列里最新一天的 is_live）
  const totalRanking = useMemo(() => {
    if (!plan || !(plan.display ?? DEFAULT_DISPLAY).ranking) return [];
    const liveOf = new Map(dailyPlayers.map((p) => [p.name, p.live[p.live.length - 1] ?? 0]));
    return plan.ranking.map((p) => ({ ...p, live: liveOf.get(p.name) ?? 0 }));
  }, [plan, dailyPlayers]);

  // 当日排名：按选中日期的音浪降序，同分并列（并列后跳号）
  const dayRanking = useMemo(() => {
    if (!dailyPlayers.length) return [];
    const sorted = dailyPlayers
      .map((p) => ({ name: p.name, leader: p.leader, wave: p.values[safeDayIdx] ?? 0, live: p.live[safeDayIdx] ?? 0 }))
      .sort((a, b) => b.wave - a.wave || a.name.localeCompare(b.name, 'zh-Hans-CN'));
    let lastWave = Number.NaN;
    let lastRank = 0;
    return sorted.map((p, i) => {
      const rank = p.wave === lastWave ? lastRank : i + 1;
      lastWave = p.wave;
      lastRank = rank;
      return { ...p, rank };
    });
  }, [dailyPlayers, safeDayIdx]);

  // 某一天的名次表（同分并列，与 dayRanking 同一套规则）
  const ranksAt = useMemo(() => {
    const byIdx = new Map<number, Map<string, number>>();
    const dayCount = dailyPlayers[0]?.values.length ?? 0;
    for (let idx = 0; idx < dayCount; idx++) {
      const sorted = dailyPlayers
        .map((p) => ({ name: p.name, w: p.values[idx] ?? 0 }))
        .sort((a, b) => b.w - a.w || a.name.localeCompare(b.name, 'zh-Hans-CN'));
      const m = new Map<string, number>();
      let lastWave = Number.NaN;
      let lastRank = 0;
      sorted.forEach((p, i) => {
        const rank = p.w === lastWave ? lastRank : i + 1;
        lastWave = p.w;
        lastRank = rank;
        m.set(p.name, rank);
      });
      byIdx.set(idx, m);
    }
    return byIdx;
  }, [dailyPlayers]);

  // 每日页的升降：当日名次 - 前一日名次（正=上升）。第一天没有对比，不显示。
  const dayMoves = useMemo(() => {
    const cur = ranksAt.get(safeDayIdx);
    const prev = ranksAt.get(safeDayIdx - 1);
    if (!cur || !prev) return new Map<string, number>();
    return new Map([...cur.entries()].map(([name, rank]) => [name, (prev.get(name) ?? prev.size + 1) - rank]));
  }, [ranksAt, safeDayIdx]);

  // 总榜的升降：当前累计名次 vs 前一日累计名次。
  // 基数必须用「当前总量 − 最新一天增量」——直接用每日序列累加会漏掉基线日，
  // 排出来的"昨日名次"整个是错的。
  const totalMoves = useMemo(() => {
    if (!plan?.ranking?.length || !dailyPlayers.length) return new Map<string, number>();
    const last = (dailyPlayers[0]?.values.length ?? 1) - 1;
    const curWave = new Map(plan.ranking.map((p) => [p.name, p.wave]));
    const prevTotal = new Map(
      dailyPlayers.map((p) => {
        const cur = curWave.get(p.name) ?? 0;
        return [p.name, cur - (p.values[last] ?? 0)];
      }),
    );
    const prevRank = new Map(
      [...prevTotal.entries()]
        .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0], 'zh-Hans-CN'))
        .map(([n], i) => [n, i + 1]),
    );
    return new Map(plan.ranking.map((p) => [p.name, (prevRank.get(p.name) ?? prevRank.size + 1) - p.rank]));
  }, [plan, dailyPlayers]);

  if (error) {
    return (
      <div className="sh">
        <div className="sh-wrap flex flex-col items-center gap-3 py-32 text-center">
          <span style={{ color: 'var(--sh-ink3)' }}>
            <AlertIcon size={28} />
          </span>
          <p className="text-[15px]" style={{ color: 'var(--sh-ink2)' }}>
            {error}
          </p>
        </div>
      </div>
    );
  }

  if (!plan) {
    return (
      <div className="sh">
        <div className="sh-wrap py-32 text-center text-[15px]" style={{ color: 'var(--sh-ink3)' }}>
          加载中…
        </div>
      </div>
    );
  }

  const display = { ...DEFAULT_DISPLAY, ...(plan.display ?? {}) };
  const groups = withBuiltinGroups(plan.groups);
  const realCount = plan.groups.length;
  const step = Math.max(1, plan.schedule.intervalMin);
  const wave = display.wave;

  // 分页：只把「开着且真有内容」的模块放进切换条；只有一个模块时不渲染切换条
  const allowed: Tab[] = [];
  if (wave && display.daily && dailyPlayers.length > 0) allowed.push('daily');
  // 分组页只在比赛日展示（后端 matchDay：最新采集日 == 今天）
  if (display.groups && plan.matchDay) allowed.push('groups');
  if (wave && display.ranking && totalRanking.length > 0) allowed.push('total');
  const active = allowed.includes(tab) ? tab : allowed[0];
  // 选中日期切到别的页再回来还保持原选择；数据缺失时夹回最新
  const selIdx = Math.min(safeDayIdx, Math.max(0, dayDates.length - 1));

  /** 名次升降徽标：涨红跌绿（docs/07 国内口径）；undefined 不显示，0 显示平 */
  const moveBadge = (mv?: number) => {
    if (mv === undefined) return null;
    if (mv > 0) return <span className="sh-mv up">▲{mv}</span>;
    if (mv < 0) return <span className="sh-mv down">▼{-mv}</span>;
    return <span className="sh-mv flat">—</span>;
  };

  const renderRankRow = (
    p: { rank: number; name: string; wave: number; live: number },
    showBadge: boolean,
    mv?: number,
  ) => (
    <div key={p.name} className="sh-rank">
      <span className={`sh-rank-n ${p.rank <= 3 ? `m${p.rank}` : ''}`}>{p.rank}</span>
      {moveBadge(mv)}
      <span className="sh-rank-name">{p.name}</span>
      {showBadge && <span className={`sh-live ${p.live ? 'on' : 'off'}`}>{p.live ? '开播' : '未开播'}</span>}
      <span className="sh-rank-v nums">{formatWave(p.wave)}</span>
    </div>
  );

  return (
    <div className="sh">
      {/* ------------------------------ 导航栏（头部） ------------------------------ */}
      <nav className="sh-nav">
        <div className="sh-nav-in">
          <span className="sh-nav-brand">{plan.brand || 'PK 挑战赛'}</span>
          <span className="sh-nav-title">{plan.title}</span>
          <span className="flex-1" />
          {plan.stale && <span className="sh-badge sh-badge-warn" style={{ marginTop: 0 }}>快照数据</span>}
          <button type="button" className="sh-btn sh-btn-ghost" onClick={() => setNonce((n) => n + 1)} aria-label="刷新">
            <RefreshIcon size={14} />
          </button>
        </div>
      </nav>

      <div className="sh-wrap">
        {/* ------------------------------ 分页切换（页内状态，URL 不变） ------------------------------ */}
        {allowed.length > 1 && (
          <nav ref={tabsRef} className="sh-tabs">
            <div className="sh-seg" role="tablist">
              {TAB_META.filter((t) => allowed.includes(t.key)).map((t) => (
                <button
                  key={t.key}
                  type="button"
                  role="tab"
                  aria-selected={active === t.key}
                  className={active === t.key ? 'on' : ''}
                  onClick={() => goTab(t.key)}
                >
                  {t.label}
                </button>
              ))}
            </div>
          </nav>
        )}

        {/* ------------------------------ 第一页：每日音浪排名 ------------------------------ */}
        {active === 'daily' && dayDates.length > 0 && (
          <section className="sh-sec sh-sec-first">
            <div className="sh-sec-head">
              <p>{dayDates[selIdx]} 音浪排名 · 点日期切换</p>
            </div>
            <div className="sh-slots hide-scrollbar" style={{ marginTop: 0 }}>
              {dayDates.map((d, i) => (
                <button
                  key={d}
                  type="button"
                  className={`sh-slot sh-daychip ${i === selIdx ? 'on' : ''}`}
                  onClick={() => setDayIdx(i)}
                >
                  {i === selIdx ? '● ' : ''}
                  {d}
                </button>
              ))}
            </div>
            <div className="sh-card sh-ranks">
              {dayRanking.map((p) => renderRankRow(p, true, dayMoves.get(p.name)))}
            </div>
          </section>
        )}

        {/* ------------------------------ 第二页：PK 分组（仅比赛日） ------------------------------ */}
        {active === 'groups' &&
          (realCount > 0 ? (
            <section className="sh-sec sh-sec-first">
              <div className="sh-sec-head">
                <p>
                  共 {realCount} 组{wave ? ' · 组内按音浪名次排列' : ''}
                  {' · '}
                  {plan.schedule.start} 起 · 每组 {step} 分钟 · 全程 {Math.floor((groups.length * step) / 60)} 小时
                  {(groups.length * step) % 60 ? ` ${(groups.length * step) % 60} 分` : ''}
                </p>
              </div>
              <div className="sh-groups">
                {groups.map((g, i) => {
                  const c = groupColor(i);
                  return (
                    <section key={g.name} className="sh-card sh-gc" style={g.builtin ? { opacity: 0.7 } : undefined}>
                      <header className="sh-gc-head">
                        <span className="sh-gc-dot" style={{ background: c.bar }} />
                        <b>{g.name}</b>
                        <span>{g.builtin ? '待补员' : `${slotTime(i, plan.schedule)} 开赛 · ${g.count} 人`}</span>
                      </header>
                      {wave && g.total > 0 && (
                        <p className="sh-gc-total nums">
                          组合计 <b>{formatWave(g.total)}</b> 音浪
                        </p>
                      )}
                      <ul className="sh-people">
                        {g.players.map((p) => (
                          <li key={p.name} className="sh-person">
                            <span className="sh-person-n">{p.group_rank}</span>
                            <span className="sh-person-name">{p.name}</span>
                            {p.leader && <span className="sh-person-leader">师门 {p.leader}</span>}
                            {wave && <span className="sh-person-v nums">{formatWave(p.wave)}</span>}
                          </li>
                        ))}
                        {g.players.length === 0 && (
                          <li className="sh-empty">{g.builtin ? '内置占位 · 后台分出该组后自动填实' : '暂无人员'}</li>
                        )}
                      </ul>
                    </section>
                  );
                })}
              </div>
            </section>
          ) : (
            <section className="sh-sec sh-sec-first">
              <div className="sh-card sh-empty">这个方案还没有分组</div>
            </section>
          ))}

        {/* ------------------------------ 第三页：总音浪 ------------------------------ */}
        {active === 'total' && totalRanking.length > 0 && (
          <section className="sh-sec sh-sec-first">
            <div className="sh-sec-head">
              <p>累计总音浪 · 全场名次 · 共 {totalRanking.length} 人 · 升降对比前一日 · 开播状态取最新一天</p>
            </div>
            <div className="sh-card sh-ranks">
              {totalRanking.map((p) => renderRankRow(p, true, totalMoves.get(p.name)))}
            </div>
          </section>
        )}

        <footer className="sh-ft">
          更新于 {new Date(plan.createdAt).toLocaleString('zh-CN', { hour12: false })}
          {plan.dates[0] ? ` · 数据截至 ${plan.dates[0]}` : ''}
        </footer>
      </div>
    </div>
  );
}
