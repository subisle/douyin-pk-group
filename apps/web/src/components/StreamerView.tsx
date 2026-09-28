import { useEffect, useMemo, useRef, useState } from 'react';
import { formatWave, type AnchorBrief, type Player, type WaveMetric } from '@pk/shared';
import { useStore } from '../store.js';
import { api } from '../api.js';
import { go } from '../lib/nav.js';
import { Button, Segmented, Tag, inputClass } from './ui.js';
import {
  AlertIcon,
  ChevronLeftIcon,
  DownloadIcon,
  GlobeIcon,
  RefreshIcon,
  SearchIcon,
  SortIcon,
  TrendIcon,
  UserSearchIcon,
  UsersIcon,
} from './icons.js';
import '../styles/streamer.css';

/** 搜索防抖：连续敲键只在停顿后发一次请求 */
const SEARCH_DEBOUNCE_MS = 320;

type SortKey = 'waveDesc' | 'waveAsc' | 'nameAsc';

const SORT_OPTIONS: Array<{ value: SortKey; label: string; title: string }> = [
  { value: 'waveDesc', label: '音浪 高→低', title: '按音浪从高到低' },
  { value: 'waveAsc', label: '音浪 低→高', title: '按音浪从低到高' },
  { value: 'nameAsc', label: '名字', title: '按名字拼音排序' },
];

/** 把后端抛出来的原始错误翻成人能看懂的一句话 + 下一步 */
function reasonOf(e: unknown): string {
  const raw = e instanceof Error ? e.message : String(e ?? '');
  const clean = raw.replace(/\s+/g, ' ').trim();
  if (!clean) return '接口没有返回错误详情';
  if (/^401\b/.test(clean)) return '登录状态已失效（401），请在右上角重新登录后再试';
  if (/^403\b/.test(clean)) return '当前账号没有访问权限（403）';
  if (/failed to fetch|networkerror|load failed|network request failed/i.test(clean)) {
    return '连不上后端接口（网络错误），请确认服务已启动后重试';
  }
  return clean.length > 140 ? `${clean.slice(0, 140)}…` : clean;
}

function metricText(metric: WaveMetric, days: number): string {
  return metric === 'total' ? '累计音浪' : `近 ${days} 天音浪`;
}

function todayStamp(): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

function csvCell(v: string): string {
  return /[",\r\n]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v;
}

/** 纯前端生成 CSV：BOM 前缀让 Excel 正确识别 UTF-8，否则中文全是乱码 */
function downloadCsv(filename: string, rows: string[][]): void {
  const body = rows.map((r) => r.map(csvCell).join(',')).join('\r\n');
  const blob = new Blob([`\uFEFF${body}`], { type: 'text/csv;charset=utf-8;' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

function matches(player: Player, needle: string): boolean {
  if (!needle) return true;
  const n = needle.toLowerCase();
  return player.name.toLowerCase().includes(n) || (player.leader ?? '').toLowerCase().includes(n);
}

interface SearchState {
  status: 'idle' | 'loading' | 'ok' | 'error';
  q: string;
  items: AnchorBrief[];
  error: string;
}

const IDLE: SearchState = { status: 'idle', q: '', items: [], error: '' };

/** 头像：有图用图，挂了就退回名字首字，绝不留破图占位 */
function Avatar({ name, avatar, size = 22 }: { name: string; avatar?: string | null; size?: number }) {
  const initial = name.trim().slice(0, 1) || '·';
  return (
    <span className="streamer-avatar" style={{ width: size, height: size, fontSize: Math.round(size * 0.5) }}>
      {initial}
      {avatar ? (
        <img
          src={avatar}
          alt=""
          loading="lazy"
          onError={(e) => {
            e.currentTarget.style.display = 'none';
          }}
        />
      ) : null}
    </span>
  );
}

export function StreamerView() {
  const players = useStore((s) => s.players);
  const loading = useStore((s) => s.loading);
  const error = useStore((s) => s.error);
  const dbOk = useStore((s) => s.dbOk);
  const metric = useStore((s) => s.metric);
  const days = useStore((s) => s.days);
  const anchorCount = useStore((s) => s.anchorCount);
  const latestDate = useStore((s) => s.latestDate);
  const sourceLabel = useStore((s) => s.sourceLabel);
  const parse = useStore((s) => s.parse);
  const setView = useStore((s) => s.setView);

  const [filter, setFilter] = useState('');
  const [sortKey, setSortKey] = useState<SortKey>('waveDesc');
  const [q, setQ] = useState('');
  const [retryTick, setRetryTick] = useState(0);
  const [search, setSearch] = useState<SearchState>(IDLE);

  /* 防抖 + 竞态守卫：只有最后一次请求的结果会被采纳 */
  const seq = useRef(0);
  useEffect(() => {
    const term = q.trim();
    if (!term) {
      setSearch(IDLE);
      return;
    }
    setSearch((prev) => ({ ...prev, status: 'loading', q: term, error: '' }));
    const timer = window.setTimeout(() => {
      const id = ++seq.current;
      api
        .anchors(term)
        .then((res) => {
          if (seq.current !== id) return;
          setSearch({ status: 'ok', q: term, items: res.items ?? [], error: '' });
        })
        .catch((e: unknown) => {
          if (seq.current !== id) return;
          setSearch({ status: 'error', q: term, items: [], error: reasonOf(e) });
        });
    }, SEARCH_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [q, retryTick]);

  const matchedCount = useMemo(() => players.filter((p) => p.matched).length, [players]);
  const excludedCount = useMemo(() => players.filter((p) => p.excluded).length, [players]);

  /** 排除的人在两种音浪排序里都沉底，避免占着榜首都算成绩 */
  const ranked = useMemo(() => {
    const cmp: Record<SortKey, (a: Player, b: Player) => number> = {
      waveDesc: (a, b) => b.wave - a.wave || a.name.localeCompare(b.name, 'zh-Hans-CN'),
      waveAsc: (a, b) => a.wave - b.wave || a.name.localeCompare(b.name, 'zh-Hans-CN'),
      nameAsc: (a, b) => a.name.localeCompare(b.name, 'zh-Hans-CN'),
    };
    const by = cmp[sortKey];
    return [...players].sort((a, b) => Number(a.excluded) - Number(b.excluded) || by(a, b));
  }, [players, sortKey]);

  const visible = useMemo(() => ranked.filter((p) => matches(p, filter.trim())), [ranked, filter]);
  const top3 = useMemo(() => [...ranked].filter((p) => !p.excluded).slice(0, 3), [ranked]);

  const rosterNames = useMemo(() => new Set(players.map((p) => p.name)), [players]);

  const emptyRoster = players.length === 0 && !loading;
  const rosterError = error && players.length === 0;

  const goParse = () => {
    setView('roster');
    go('board');
  };

  const exportCsv = () => {
    const rows: string[][] = [
      ['排名', '名字', '师傅', '音浪', '音浪原始值', '匹配状态', '是否排除'],
      ...visible.map((p, i) => [
        String(i + 1),
        p.name,
        p.leader ?? '',
        formatWave(p.wave),
        String(p.wave),
        p.matched ? '已命中' : '未命中',
        p.excluded ? '是' : '否',
      ]),
    ];
    downloadCsv(`主播数据_${todayStamp()}.csv`, rows);
  };

  return (
    <div className="page">
      {/* 概览：一个整块面板，3 秒内看完人数 / 命中率 / Top 3 */}
      <section className="tile streamer-summary" aria-label="名单概览">
        <div className="streamer-summary-main">
          <div className="streamer-metric">
            <span className="streamer-metric-label">
              <UsersIcon size={13} />
              名单人数
            </span>
            <span className="figure streamer-metric-value nums">{players.length}</span>
          </div>
          <div className="streamer-divider" aria-hidden="true" />
          <div className="streamer-metric">
            <span className="streamer-metric-label">已命中数据库</span>
            <span className="figure streamer-metric-value nums streamer-metric-hit">{matchedCount}</span>
            <span className="streamer-metric-sub nums">
              未命中 {players.length - matchedCount}
              {excludedCount > 0 ? ` · 已排除 ${excludedCount}` : ''}
            </span>
          </div>
          <div className="streamer-divider" aria-hidden="true" />
          <div className="streamer-metric streamer-metric-scope">
            <span className="streamer-metric-label">
              <GlobeIcon size={13} />
              数据源
            </span>
            <span className="streamer-metric-value streamer-scope-value">
              {sourceLabel || '未连接'}
              <span className="streamer-scope-sub nums">
                {metricText(metric, days)}
                {latestDate ? ` · 截止 ${latestDate}` : ''}
              </span>
            </span>
          </div>
        </div>

        <div className="streamer-top3">
          <span className="streamer-top3-label">
            <TrendIcon size={13} />
            音浪 Top 3
          </span>
          {top3.length === 0 ? (
            <span className="streamer-top3-empty">暂无音浪数据</span>
          ) : (
            <ol className="streamer-top3-list">
              {top3.map((p, i) => (
                <li key={p.key} className="streamer-top3-item">
                  <span className="streamer-top3-rank nums">{i + 1}</span>
                  <span className="streamer-top3-name" title={p.name}>
                    {p.name}
                  </span>
                  <span className="streamer-top3-wave nums" title={`${p.wave} 音浪`}>
                    {formatWave(p.wave)}
                  </span>
                </li>
              ))}
            </ol>
          )}
        </div>
      </section>

      {dbOk === false && !rosterError ? (
        <div className="tile streamer-banner streamer-banner-warn" role="status">
          <AlertIcon size={14} />
          <div className="streamer-banner-body">
            <span className="streamer-banner-title">数据库未连接，下面的音浪可能是缓存值</span>
            <span className="streamer-banner-hint">先在 PK 分组页确认数据源可用，再回本页看实时音浪。</span>
          </div>
        </div>
      ) : null}

      {/* 排行表：只读，不参与分组编辑 */}
      <section aria-label="名单音浪排行">
        <div className="sect-head streamer-sect">
          <div>
            <span className="sect-label">已解析名单</span>
            <h2 className="sect-title">音浪排行</h2>
          </div>
          <div className="streamer-toolbar">
            <div className="streamer-search">
              <SearchIcon size={14} />
              <input
                className={inputClass}
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
                placeholder="名单内按名字 / 师傅筛选"
                aria-label="名单内筛选"
              />
            </div>
            <Segmented options={SORT_OPTIONS} value={sortKey} onChange={setSortKey} />
            <Button
              icon={<DownloadIcon size={14} />}
              onClick={exportCsv}
              disabled={visible.length === 0}
              title="导出当前筛选结果为 CSV"
            >
              导出 CSV
            </Button>
          </div>
        </div>

        <div className="tile streamer-panel">
          {loading && players.length === 0 ? (
            <div className="streamer-skel-wrap" aria-busy="true">
              <p className="streamer-state-title">正在加载名单与音浪…</p>
              <div className="streamer-skel streamer-skel-title" />
              {Array.from({ length: 6 }, (_, i) => (
                <div key={i} className="streamer-skel streamer-skel-row" />
              ))}
            </div>
          ) : rosterError ? (
            <div className="streamer-state" role="alert">
              <span className="streamer-state-icon streamer-state-icon-bad">
                <AlertIcon size={16} />
              </span>
              <p className="streamer-state-title">名单加载失败</p>
              <p className="streamer-state-desc">原因：{error}</p>
              <p className="streamer-state-desc">下一步：点下方重试；若仍失败，去 PK 分组页重新解析名单或检查数据库连接。</p>
              <Button variant="primary" icon={<RefreshIcon size={14} />} onClick={() => void parse()}>
                重试
              </Button>
            </div>
          ) : emptyRoster ? (
            <div className="streamer-state">
              <span className="streamer-state-icon">
                <UsersIcon size={16} />
              </span>
              <p className="streamer-state-title">还没有解析任何名单</p>
              <p className="streamer-state-desc">本页只展示名单内人员的音浪明细与排行，不参与分组编辑。</p>
              <p className="streamer-state-desc">下一步：去 PK 分组页粘贴名单文本并解析，解析完自动回到这里。</p>
              <Button variant="primary" icon={<ChevronLeftIcon size={14} />} onClick={goParse}>
                去 PK 分组页解析
              </Button>
            </div>
          ) : (
            <>
              <div className="streamer-panel-head">
                <span className="streamer-panel-note nums">
                  共 {visible.length} 人
                  {filter.trim() ? ` · 已按「${filter.trim()}」筛选` : ''}
                  {loading ? ' · 更新中' : ''}
                </span>
                <span className="streamer-panel-note">
                  <SortIcon size={13} />
                  {SORT_OPTIONS.find((o) => o.value === sortKey)?.title}
                </span>
              </div>
              <div className="rail streamer-table-scroll">
                <table className="grid-table streamer-table">
                  <thead>
                    <tr>
                      <th className="streamer-col-rank">排名</th>
                      <th>名字</th>
                      <th className="streamer-col-master">师傅</th>
                      <th className="streamer-col-wave">音浪</th>
                      <th className="streamer-col-state">状态</th>
                    </tr>
                  </thead>
                  <tbody>
                    {visible.length === 0 ? (
                      <tr>
                        <td colSpan={5}>
                          <div className="streamer-inline-empty">
                            <p className="streamer-state-title">名单里没有匹配「{filter.trim()}」的人</p>
                            <p className="streamer-state-desc">
                              下一步：换个关键字，或清空筛选框看全部 {players.length} 人；如果这个人还没进名单，去 PK 分组页补一行再解析。
                            </p>
                            <Button size="sm" onClick={() => setFilter('')}>
                              清空筛选
                            </Button>
                          </div>
                        </td>
                      </tr>
                    ) : (
                      visible.map((p, i) => (
                        <tr
                          key={p.key}
                          className={i < 3 && !p.excluded ? 'streamer-row-top' : p.excluded ? 'streamer-row-off' : undefined}
                        >
                          <td className="streamer-col-rank">
                            <span className={`streamer-rank nums${i < 3 && !p.excluded ? ' streamer-rank-top' : ''}`}>
                              {i + 1}
                            </span>
                          </td>
                          <td>
                            <div className="streamer-name-cell">
                              <Avatar name={p.name} avatar={p.avatar} />
                              <span className="streamer-name" title={p.name}>
                                {p.name}
                              </span>
                            </div>
                          </td>
                          <td className="streamer-col-master">
                            {p.leader ? (
                              <span className="streamer-master" title={p.leader}>
                                {p.leader}
                              </span>
                            ) : (
                              <span className="streamer-master streamer-master-empty">—</span>
                            )}
                          </td>
                          <td className="streamer-col-wave">
                            <span className="streamer-wave nums" title={`${p.wave} 音浪`}>
                              {formatWave(p.wave)}
                            </span>
                          </td>
                          <td className="streamer-col-state">
                            {p.excluded ? (
                              <Tag tone="neutral">已排除</Tag>
                            ) : p.matched ? (
                              <Tag tone="good">已命中</Tag>
                            ) : (
                              <Tag tone="warn">未命中</Tag>
                            )}
                          </td>
                        </tr>
                      ))
                    )}
                  </tbody>
                </table>
              </div>
            </>
          )}
        </div>
      </section>

      {/* 全库检索：语义和上面的名单表不同，物理隔开，不混进同一张表 */}
      <section aria-label="主播库检索">
        <div className="sect-head streamer-sect">
          <div>
            <h2 className="sect-title">主播库检索</h2>
            <p className="streamer-sect-sub">
              搜的是主播库（{anchorCount > 0 ? `${anchorCount} 人` : '全部'}），不在上面的名单里
            </p>
          </div>
          <div className="streamer-search streamer-search-wide">
            <UserSearchIcon size={14} />
            <input
              className={inputClass}
              value={q}
              onChange={(e) => setQ(e.target.value)}
              placeholder="输入名字检索主播库"
              aria-label="主播库检索"
            />
            {q ? (
              <button type="button" className="streamer-search-clear" onClick={() => setQ('')} aria-label="清空检索">
                ×
              </button>
            ) : null}
          </div>
        </div>

        <div className="tile streamer-panel">
          {search.status === 'idle' ? (
            <div className="streamer-state">
              <span className="streamer-state-icon">
                <UserSearchIcon size={16} />
              </span>
              <p className="streamer-state-title">按名字查主播库</p>
              <p className="streamer-state-desc">
                结果只用来核对人是否在库里，查到后要生效还得回 PK 分组页加进名单。
              </p>
            </div>
          ) : search.status === 'loading' ? (
            <div className="streamer-skel-wrap" aria-busy="true">
              <p className="streamer-state-title">正在检索「{search.q}」…</p>
              {Array.from({ length: 3 }, (_, i) => (
                <div key={i} className="streamer-skel streamer-skel-row" />
              ))}
            </div>
          ) : search.status === 'error' ? (
            <div className="streamer-state" role="alert">
              <span className="streamer-state-icon streamer-state-icon-bad">
                <AlertIcon size={16} />
              </span>
              <p className="streamer-state-title">检索「{search.q}」失败</p>
              <p className="streamer-state-desc">原因：{search.error}</p>
              <p className="streamer-state-desc">下一步：点重试；连着失败就先去 PK 分组页确认数据库已连接。</p>
              <Button variant="primary" icon={<RefreshIcon size={14} />} onClick={() => setRetryTick((n) => n + 1)}>
                重试
              </Button>
            </div>
          ) : search.items.length === 0 ? (
            <div className="streamer-state">
              <span className="streamer-state-icon">
                <SearchIcon size={16} />
              </span>
              <p className="streamer-state-title">主播库里没有「{search.q}」</p>
              <p className="streamer-state-desc">
                下一步：换个关键字或只输姓氏试试；确认对方确实在库里，再把 TA 加进名单重新解析。
              </p>
            </div>
          ) : (
            <div className="streamer-hits">
              <div className="streamer-panel-head">
                <span className="streamer-panel-note nums">命中 {search.items.length} 人</span>
                <span className="streamer-panel-note">命中项只作核对，不改名单</span>
              </div>
              <ul className="streamer-hit-list">
                {search.items.map((a) => {
                  const inRoster = rosterNames.has(a.name);
                  return (
                    <li key={a.personId} className="streamer-hit">
                      <Avatar name={a.name} avatar={a.avatar} size={24} />
                      <span className="streamer-hit-name" title={a.name}>
                        {a.name}
                      </span>
                      {inRoster ? <Tag tone="info">已在名单</Tag> : <Tag tone="neutral">库中其他人</Tag>}
                    </li>
                  );
                })}
              </ul>
            </div>
          )}
        </div>
      </section>
    </div>
  );
}
