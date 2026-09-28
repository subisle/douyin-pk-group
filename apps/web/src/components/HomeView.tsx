import { formatWave, type WaveMetric } from '@pk/shared';
import { useStore, type Strategy } from '../store.js';
import { go } from '../lib/nav.js';
import { Button, Pill } from './ui.js';
import {
  AlertIcon,
  BalanceIcon,
  ChevronLeftIcon,
  ClockIcon,
  LayersIcon,
  UserSearchIcon,
} from './icons.js';
import '../styles/home.css';

type Tone = 'good' | 'warn' | 'bad' | 'neutral';

const STRATEGY_LABEL: Record<Strategy, string> = {
  snake: '蛇形均衡',
  'snake-no-master': '蛇形 + 同门回避',
  sequential: '从高到低',
  random: '随机',
};

function metricLabel(metric: WaveMetric, days: number): string {
  return metric === 'lastN' ? `近 ${days} 天` : '总音浪';
}

/** 把 YYYY-MM-DD 换算成「距今天数」，解析不了就返回 null（不假装新鲜、也不假装过期） */
function daysSince(iso: string | null): number | null {
  if (!iso) return null;
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(iso.trim());
  if (!m) return null;
  const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  if (Number.isNaN(d.getTime())) return null;
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  return Math.round((today.getTime() - d.getTime()) / 86400000);
}

function freshnessCaption(gap: number | null, latestDate: string | null): string {
  if (!latestDate) return '还没有采集到任何日期';
  if (gap === null) return `最新日期 ${latestDate}`;
  if (gap === 0) return `${latestDate} · 今天`;
  if (gap === 1) return `${latestDate} · 昨天`;
  return `${latestDate} · ${gap} 天前`;
}

export function HomeView() {
  const dbOk = useStore((s) => s.dbOk);
  const latestDate = useStore((s) => s.latestDate);
  const dates = useStore((s) => s.dates);
  const baselineDate = useStore((s) => s.baselineDate);
  const anchorCount = useStore((s) => s.anchorCount);

  const title = useStore((s) => s.title);
  const brand = useStore((s) => s.brand);
  const metric = useStore((s) => s.metric);
  const days = useStore((s) => s.days);
  const groupSize = useStore((s) => s.groupSize);
  const strategy = useStore((s) => s.strategy);
  const startTime = useStore((s) => s.startTime);
  const intervalMin = useStore((s) => s.intervalMin);

  const players = useStore((s) => s.players);
  const buckets = useStore((s) => s.buckets);
  const balance = useStore((s) => s.balance);
  const violations = useStore((s) => s.violations);

  const activePlayers = players.filter((p) => !p.excluded);
  const activeCount = activePlayers.length;
  const totalWave = activePlayers.reduce((sum, p) => sum + (Number.isFinite(p.wave) ? p.wave : 0), 0);

  const hasRoster = players.length > 0;
  const hasGroups = buckets.length > 0;

  /* --- 数据新鲜度判定：断库 > 检测中 > 无数据 > 滞后 > 正常 --- */
  const gap = daysSince(latestDate);
  const hasData = dates.length > 0 && Boolean(latestDate);

  let heroTone: Tone = 'neutral';
  let statusText: string;
  if (dbOk === false) {
    heroTone = 'bad';
    statusText = '数据库未连接';
  } else if (dbOk === null) {
    heroTone = 'neutral';
    statusText = '检测中';
  } else if (!hasData) {
    heroTone = 'neutral';
    statusText = '暂无可用数据';
  } else if (gap !== null && gap >= 2) {
    heroTone = 'warn';
    statusText = `数据滞后 ${gap} 天`;
  } else {
    heroTone = 'good';
    statusText = '数据为最新';
  }

  /* 横幅下方只挂一条说明：与上面的状态互斥，避免同时出现「滞后」和「已就绪」 */
  const freshnessNote: { tone: Tone; text: string } | null =
    dbOk === false
      ? { tone: 'bad', text: '数据库连接异常，音浪明细暂时读不出来。请先确认后端服务可用，再回来分组。' }
      : gap !== null && gap >= 2
        ? { tone: 'warn', text: `最新数据已滞后 ${gap} 天，用它出方案会低估近期表现。建议先重新采集再分组。` }
        : dbOk === null
          ? { tone: 'neutral', text: '正在检测数据源连接，稍等片刻。' }
          : hasData
            ? { tone: 'good', text: `数据已更新到 ${latestDate}，可以直接进「PK 分组」解析名单。` }
            : { tone: 'neutral', text: '还没有采集到可用的音浪日期，先到「PK 分组」解析名单。' };

  return (
    <div className="page">
      {/* 1. 数据新鲜度 */}
      <section className="home-hero home-rise" data-tone={heroTone} aria-label="数据新鲜度">
        <div className="home-hero-main">
          <div className="min-w-0">
            <Pill tone={heroTone}>
              <span className="h-1.5 w-1.5 rounded-full" style={{ background: 'currentColor' }} />
              {statusText}
            </Pill>
            <div className="mt-3 flex items-baseline gap-2">
              <span className="figure text-[30px] leading-none">{latestDate ?? '——'}</span>
            </div>
            <p className="mt-2 text-[12.5px] text-ink2">{freshnessCaption(gap, latestDate)}</p>
          </div>

          <div className="home-hero-stats">
            <div className="home-stat">
              <span className="home-stat-label">可用天数</span>
              <span className="home-stat-value figure">
                {dates.length}
                <span className="ml-0.5 text-[12px] font-medium text-ink3">天</span>
              </span>
            </div>
            <div className="home-stat">
              <span className="home-stat-label">主播库</span>
              <span className="home-stat-value figure">
                {anchorCount > 0 ? anchorCount.toLocaleString('en-US') : '——'}
                {anchorCount > 0 && <span className="ml-0.5 text-[12px] font-medium text-ink3">人</span>}
              </span>
            </div>
            <div className="home-stat">
              <span className="home-stat-label">累计基数日</span>
              <span className="home-stat-value">{baselineDate ?? '——'}</span>
            </div>
          </div>
        </div>

        {freshnessNote && (
          <div className="home-hero-note">
            <div className="home-note" data-tone={freshnessNote.tone}>
              {freshnessNote.tone === 'good' ? (
                <BalanceIcon size={14} className="home-note-icon" />
              ) : (
                <AlertIcon size={14} className="home-note-icon" />
              )}
              <span>{freshnessNote.text}</span>
            </div>
          </div>
        )}
      </section>

      {/* 2. 当前方案概况 */}
      <section className="tile p-5 home-rise-d1" aria-label="当前方案概况">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="min-w-0">
            <h2 className="sect-title text-[18px]">{title || '未命名方案'}</h2>
            <p className="mt-1 flex items-center gap-1.5 text-[12.5px] text-ink2">
              <LayersIcon size={14} />
              <span className="truncate">{brand || '未设置品牌'}</span>
            </p>
          </div>
          {hasGroups && (
            <Pill tone="info">
              {buckets.length} 组 · {activeCount} 人
            </Pill>
          )}
        </div>

        {hasRoster ? (
          <div className="home-defs mt-5">
            <div className="home-def">
              <span className="home-def-label">音浪口径</span>
              <span className="home-def-value">{metricLabel(metric, days)}</span>
            </div>
            <div className="home-def">
              <span className="home-def-label">每组上限</span>
              <span className="home-def-value">{groupSize} 人</span>
            </div>
            <div className="home-def">
              <span className="home-def-label">分组策略</span>
              <span className="home-def-value">{STRATEGY_LABEL[strategy]}</span>
            </div>
            <div className="home-def">
              <span className="home-def-label">第 1 组开赛</span>
              <span className="home-def-value flex items-center gap-1">
                <ClockIcon size={13} />
                {startTime || '——'}
              </span>
            </div>
            <div className="home-def">
              <span className="home-def-label">组间间隔</span>
              <span className="home-def-value">{intervalMin} 分钟</span>
            </div>
            <div className="home-def">
              <span className="home-def-label">参赛人数</span>
              <span className="home-def-value">{activeCount} 人</span>
            </div>
            <div className="home-def">
              <span className="home-def-label">分组数</span>
              <span className="home-def-value">{buckets.length} 组</span>
            </div>
            <div className="home-def">
              <span className="home-def-label">参赛总音浪</span>
              <span className="home-def-value figure">{formatWave(totalWave)}</span>
            </div>
          </div>
        ) : (
          <div className="home-note mt-5" data-tone="neutral">
            <AlertIcon size={14} className="home-note-icon" />
            <span>名单还没解析，参赛人数和分组结果要等解析后才出现。</span>
          </div>
        )}

        {!hasRoster && (
          <div className="mt-4">
            <Button variant="primary" icon={<LayersIcon size={14} />} onClick={() => go('board')}>
              去 PK 分组
            </Button>
          </div>
        )}
      </section>

      {/* 3. 均衡度摘要：只有真的分了组才出现 */}
      {hasGroups && (
        <section className="tile p-5 home-rise-d2" aria-label="均衡度摘要">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <h3 className="sect-title flex items-center gap-2">
              <BalanceIcon size={16} />
              均衡度摘要
            </h3>
            {balance && (
              <span className="text-[12px] text-ink3">共 {buckets.length} 组参赛</span>
            )}
          </div>

          {balance && (
            <div className="home-defs mt-4">
              <div className="home-def">
                <span className="home-def-label">组均音浪</span>
                <span className="home-def-value figure">{formatWave(balance.mean)}</span>
              </div>
              <div className="home-def">
                <span className="home-def-label">最高组</span>
                <span className="home-def-value figure">{formatWave(balance.max)}</span>
              </div>
              <div className="home-def">
                <span className="home-def-label">最低组</span>
                <span className="home-def-value figure">{formatWave(balance.min)}</span>
              </div>
              <div className="home-def">
                <span className="home-def-label">组间极差</span>
                <span className="home-def-value figure">{formatWave(balance.spread)}</span>
              </div>
              <div className="home-def">
                <span className="home-def-label">标准差</span>
                <span className="home-def-value figure">{formatWave(balance.stdDev)}</span>
              </div>
            </div>
          )}

          <div className="mt-4">
            {violations.length > 0 ? (
              <div className="home-note" data-tone="warn">
                <AlertIcon size={14} className="home-note-icon" />
                <div className="min-w-0">
                  <p className="font-semibold">{violations.length} 条内置条件还没满足</p>
                  <ul className="mt-1 list-inside list-disc space-y-0.5">
                    {violations.slice(0, 3).map((v, i) => (
                      <li key={`${v.kind}-${v.groupIndex}-${i}`}>{v.message}</li>
                    ))}
                  </ul>
                  {violations.length > 3 && (
                    <p className="mt-1">还有 {violations.length - 3} 条，进分组页逐条处理。</p>
                  )}
                </div>
              </div>
            ) : (
              <div className="home-note" data-tone="good">
                <BalanceIcon size={14} className="home-note-icon" />
                <span>内置条件（互斥 / 固定位 / 最小间隔）均已满足。</span>
              </div>
            )}
          </div>
        </section>
      )}

      {/* 4. 去哪：两个子页入口 */}
      <section className="home-rise-d3" aria-label="页面入口">
        <div className="sect-head">
          <h3 className="sect-title">去哪</h3>
        </div>
        <div className="home-entries">
          <button type="button" className="home-entry" onClick={() => go('data')}>
            <span className="home-entry-icon">
              <UserSearchIcon size={18} />
            </span>
            <span className="home-entry-body">
              <span className="home-entry-title">主播数据</span>
              <span className="home-entry-desc">
                按主播看逐日音浪明细和排行，确认谁在榜、谁掉线。
              </span>
              <span className="mt-1 text-[12px] text-ink3">
                {anchorCount > 0 ? `主播库 ${anchorCount.toLocaleString('en-US')} 人` : '主播库待连接'}
                {dates.length > 0 ? ` · ${dates.length} 天数据` : ''}
              </span>
            </span>
            <ChevronLeftIcon size={16} className="home-entry-go rotate-180" />
          </button>

          <button type="button" className="home-entry" onClick={() => go('board')}>
            <span className="home-entry-icon">
              <LayersIcon size={18} />
            </span>
            <span className="home-entry-body">
              <span className="home-entry-title">PK 分组</span>
              <span className="home-entry-desc">
                解析名单、按策略均衡分组，拖拽微调后导出并公布名单。
              </span>
              <span className="mt-1 text-[12px] text-ink3">
                {hasGroups ? `当前 ${buckets.length} 组 · ${activeCount} 人` : '尚未分组'}
              </span>
            </span>
            <ChevronLeftIcon size={16} className="home-entry-go rotate-180" />
          </button>
        </div>
      </section>
    </div>
  );
}
