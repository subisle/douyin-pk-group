/**
 * 对外展示页（只读、无需口令）：/s/:planId · 苹果风
 *
 * 信息结构四块，每块都由管理端「展示页显示」开关控制（随方案落库）：
 *   1. 头部汇总  品牌 + 方案名 + 总音浪大数字 + 人数 / 组数 / 赛程时长
 *   2. 排行榜    全场名次（音浪关闭时后端直接不返回，模块整体消失）
 *   3. 每日音浪  按日合计圆角横条（不做曲线图）
 *   4. PK 分组   组卡 + 组内名单（组内按音浪名次排）
 *
 * 音浪数值关闭时：页面只剩名单和赛程，任何音浪数字都不下发（后端已清零）。
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

function fmtDuration(min: number): string {
  const h = Math.floor(min / 60);
  const m = min % 60;
  return h ? (m ? `${h} 小时 ${m} 分` : `${h} 小时`) : `${m} 分钟`;
}

function shortGroup(name: string): string {
  const m = name.match(/(\d+)/);
  return m ? `${m[1]}组` : name;
}

/* ------------------------------ 页内分页 ------------------------------ */
/** 模块页：纯组件状态切换（不动 URL），对外链接固定就是 /s/:planId */
type Tab = 'rank' | 'daily' | 'groups';

const TAB_META: Array<{ key: Tab; label: string }> = [
  { key: 'rank', label: '音浪排行榜' },
  { key: 'daily', label: '每日音浪' },
  { key: 'groups', label: 'PK 分组' },
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
  const [tab, setTab] = useState<Tab>('rank');
  const tabsRef = useRef<HTMLElement | null>(null);
  const goTab = (t: Tab) => {
    setTab(t);
    requestAnimationFrame(() => {
      const top = tabsRef.current?.offsetTop ?? 0;
      if (window.scrollY > top) window.scrollTo({ top, behavior: 'smooth' });
    });
  };

  // 排行榜行里要显示「组别」胶囊，ranking 本身不带组名，用分组数据建一张名字映射。
  // hooks 必须在早退分支之前调用，否则加载态 → 数据态切换时钩子数量会变（React #310）。
  const groupOf = useMemo(() => {
    const m = new Map<string, PublicGroup>();
    if (plan) for (const g of plan.groups) for (const p of g.players) m.set(p.name, g);
    return m;
  }, [plan]);

  // 每日合计：各组当天音浪加总（后端已剔除基线日），最近一天的排最上
  const dayTotals = useMemo(() => {
    if (!plan?.daily) return [];
    const { dates, groups: gs } = plan.daily;
    return dates
      .map((d, i) => ({ date: d, total: gs.reduce((a, g) => a + (g.values[i] ?? 0), 0) }))
      .filter((x) => x.total > 0)
      .reverse();
  }, [plan]);

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
  const metricLabel = plan.metric === 'total' ? '总音浪' : `近 ${plan.days} 天音浪`;
  const playerCount = wave
    ? plan.counts.players
    : plan.groups.reduce((a, g) => a + g.count, 0);

  const fullMin = groups.length * step;
  // 归一化分母必须取最大日合计；取 dayTotals[0]（最近一天）会让更高的一天顶满溢出
  const maxDay = Math.max(...dayTotals.map((x) => x.total), 1);
  const ranking = wave && display.ranking ? plan.ranking : [];

  // 分页：只把「开着且真有内容」的模块放进切换条；只有一个模块时不渲染切换条
  const allowed: Tab[] = [];
  if (wave && display.ranking && ranking.length > 0) allowed.push('rank');
  if (wave && display.daily && dayTotals.length > 0) allowed.push('daily');
  if (display.groups) allowed.push('groups');
  // 当前生效的分页：状态失效（模块被关/没数据）就回退到第一个可用的
  const active = allowed.includes(tab) ? tab : allowed[0];

  return (
    <div className="sh">
      {/* ------------------------------ 毛玻璃顶栏 ------------------------------ */}
      <nav className="sh-nav">
        <div className="sh-nav-in">
          <span className="sh-nav-brand">{plan.brand || 'PK 挑战赛'}</span>
          <span className="sh-nav-title">{plan.title}</span>
          <span className="flex-1" />
          <button type="button" className="sh-btn sh-btn-ghost" onClick={() => setNonce((n) => n + 1)} aria-label="刷新">
            <RefreshIcon size={14} />
          </button>
        </div>
      </nav>

      <div className="sh-wrap">
        {/* ------------------------------ 头部：音浪 · 时长 ------------------------------ */}
        <header className="sh-hero">
          <p className="sh-eyebrow">{plan.brand || 'PK 挑战赛'}</p>
          <h1 className="sh-h1">{plan.title}</h1>
          {wave && (
            <>
              <div className="sh-figure nums">{formatWave(plan.counts.totalWave)}</div>
              <p className="sh-cap">
                {metricLabel}
                {plan.dates[0] ? ` · 数据截至 ${plan.dates[0]}` : ''}
              </p>
            </>
          )}
          {plan.stale && <span className="sh-badge sh-badge-warn">实时数据暂不可用 · 显示方案快照</span>}
        </header>

        {/* 汇总条：音浪关掉时只剩赛程信息 */}
        <dl className="sh-strip">
          {wave && (
            <div className="sh-cell">
              <dt>{metricLabel}</dt>
              <dd className="nums">{formatWave(plan.counts.totalWave)}</dd>
            </div>
          )}
          <div className="sh-cell">
            <dt>参赛人数</dt>
            <dd className="nums">
              {playerCount}
              <small>人</small>
            </dd>
          </div>
          <div className="sh-cell">
            <dt>分组</dt>
            <dd className="nums">
              {realCount}
              <small>组</small>
            </dd>
          </div>
          <div className="sh-cell">
            <dt>赛程时长</dt>
            <dd className="nums">
              {fmtDuration(fullMin)}
              <small>
                · {plan.schedule.start} 起 · {step} 分/组 · 至 {slotTime(groups.length, plan.schedule)}
              </small>
            </dd>
          </div>
        </dl>

        {/* 赛程胶囊条 */}
        <div className="sh-slots hide-scrollbar">
          {groups.map((g, i) => {
            const c = groupColor(i);
            return (
              <span key={g.name} className="sh-slot">
                <i style={{ background: c.bar }} />
                <b>{g.name}</b>
                <span className="nums">
                  {slotTime(i, plan.schedule)}–{slotTime(i + 1, plan.schedule)}
                </span>
                {g.builtin && <span style={{ color: 'var(--sh-ink3)' }}>待补员</span>}
              </span>
            );
          })}
        </div>

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

        {/* ------------------------------ 排行榜 ------------------------------ */}
        {active === 'rank' && ranking.length > 0 && (
          <section className="sh-sec">
            <div className="sh-sec-head">
              <p>{metricLabel} · 全场名次 · 共 {ranking.length} 人</p>
            </div>
            <div className="sh-card sh-ranks">
              {ranking.map((p) => (
                <div key={p.name} className="sh-rank">
                  <span className={`sh-rank-n ${p.rank <= 3 ? `m${p.rank}` : ''}`}>{p.rank}</span>
                  <span className="sh-rank-name">{p.name}</span>
                  <span className="sh-rank-g">{shortGroup(groupOf.get(p.name)?.name ?? '')}</span>
                  <span className="sh-rank-v nums">{formatWave(p.wave)}</span>
                </div>
              ))}
            </div>
          </section>
        )}

        {/* ------------------------------ 每日音浪 ------------------------------ */}
        {active === 'daily' && dayTotals.length > 0 && (
          <section className="sh-sec">
            <div className="sh-sec-head">
              <p>近 {dayTotals.length} 日 · 全体参赛者合计</p>
            </div>
            <div className="sh-card sh-days">
              {dayTotals.map((d) => (
                <div key={d.date} className="sh-day">
                  <span className="sh-day-d">{d.date.slice(5)}</span>
                  <span className="sh-day-track">
                    <i style={{ width: `${Math.max(4, (d.total / maxDay) * 100)}%` }} />
                  </span>
                  <span className="sh-day-v nums">{formatWave(d.total)}</span>
                </div>
              ))}
            </div>
          </section>
        )}

        {/* ------------------------------ PK 分组 ------------------------------ */}
        {active === 'groups' &&
          (realCount > 0 ? (
            <section className="sh-sec">
              <div className="sh-sec-head">
                <p>共 {realCount} 组{wave ? ' · 组内按音浪名次排列' : ''}</p>
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
            <section className="sh-sec">
              <div className="sh-card sh-empty">这个方案还没有分组</div>
            </section>
          ))}

        <footer className="sh-ft">
          更新于 {new Date(plan.createdAt).toLocaleString('zh-CN', { hour12: false })}
          <br />
          {wave ? '音浪数据实时读取 · 请勿外传' : '名单仅供参考 · 按组名找到自己那一组，按组头时段开赛'}
        </footer>
      </div>
    </div>
  );
}
