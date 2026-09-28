/**
 * PK 分组页：名单解析 → 分组看板 → 均衡微调 → 导出。
 *
 * 这一页承载全部「动手」的动作，所以信息密度最高：DENSITY 7。
 * 视觉上刻意保持安静（低饱和底 + 单一品牌色），让组卡的九种颜色
 * 成为屏幕上唯一的高饱和元素——那是这页唯一需要被快速扫读的东西。
 *
 * 页面分三块：方案配置（输入）→ 分组结果（重心）→ 导出（收尾）。
 * 层级靠区块间距、标题字号和 .tile 抬升面表达，不靠加粗。
 */
import { useEffect, useMemo, useState } from 'react';
import { useStore } from '../store.js';
import { WaveConfig } from './WaveConfig.js';
import { RosterImport } from './RosterImport.js';
import { StatsBar } from './StatsBar.js';
import { BoardTools } from './BoardTools.js';
import { GroupBoard } from './GroupBoard.js';
import { ExportBar } from './ExportBar.js';
import { HistoryPlans } from './HistoryPlans.js';
import { Button, Tag, Toaster } from './ui.js';
import {
  AlertIcon,
  BalanceIcon,
  ChevronDownIcon,
  EditIcon,
  GridIcon,
  RefreshIcon,
} from './icons.js';
import '../styles/board.css';

export function BoardView() {
  const view = useStore((s) => s.view);
  const setView = useStore((s) => s.setView);
  const buckets = useStore((s) => s.buckets);
  const players = useStore((s) => s.players);
  const unmatched = useStore((s) => s.unmatched);
  const excluded = useStore((s) => s.excluded);
  const loading = useStore((s) => s.loading);
  const error = useStore((s) => s.error);
  const toast = useStore((s) => s.toast);
  const addBucket = useStore((s) => s.addBucket);
  const regroup = useStore((s) => s.regroup);
  const polish = useStore((s) => s.polish);
  const violations = useStore((s) => s.violations);
  const undo = useStore((s) => s.undo);
  const redo = useStore((s) => s.redo);
  const canUndo = useStore((s) => s.past.length > 0);
  const canRedo = useStore((s) => s.future.length > 0);
  const unassigned = useStore((s) => s.unassigned);

  /* 全局找人 + 紧凑模式：组一多就靠这两个按钮定位 */
  const [query, setQuery] = useState('');
  const [dense, setDense] = useState(false);

  const highlight = useMemo(() => {
    const q = query.trim();
    if (!q) return undefined;
    const set = new Set<string>();
    for (const p of [...buckets.flatMap((b) => b.players), ...unassigned]) {
      if (p.name.includes(q)) set.add(p.key);
    }
    return set;
  }, [query, buckets, unassigned]);

  /* Cmd/Ctrl+Z 撤销、Cmd/Ctrl+Shift+Z 重做；在输入框里让给浏览器原生行为 */
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      const el = e.target as HTMLElement | null;
      const typing = Boolean(
        el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable),
      );
      if (typing) return;
      const k = e.key.toLowerCase();
      if (k === 'z' && !e.shiftKey) {
        e.preventDefault();
        undo();
      } else if ((k === 'z' && e.shiftKey) || k === 'y') {
        e.preventDefault();
        redo();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [undo, redo]);

  const activeCount = players.filter((p) => !p.excluded).length;
  /* 只给标题行右侧做计数展示，不参与任何渲染判断 */
  const groupedCount = buckets.reduce((s, b) => s + b.players.length, 0);

  return (
    <>
      <div className="page">
        {/* ── 1. 方案配置：这一块是输入，刻意不加抬升面，退到背景里 ── */}
        <section className="board-sect board-rise" aria-label="方案配置">
          <div className="sect-head">
            <h2 className="sect-title">口径、每组人数与分组策略</h2>
            <span className="board-sect-meta">名单和参数都在这一块里改</span>
          </div>
          <div className="board-config">
            <WaveConfig onAddBucket={addBucket} />

            {view === 'roster' ? (
              <RosterImport />
            ) : (
              <>
                <button
                  type="button"
                  onClick={() => setView('roster')}
                  className="board-roster-toggle flex items-center justify-between rounded-card bg-subtle px-3.5 py-3 text-left transition-colors hover:brightness-95"
                >
                  <span className="flex items-center gap-2">
                    <span className="text-[13px] font-medium text-ink">名单文本</span>
                    <span className="text-[12px] text-ink2">
                      {activeCount ? `${activeCount} 人参赛` : '尚未解析'}
                      {players.length ? ` · ${players.filter((p) => p.matched).length} 人命中库` : ''}
                    </span>
                  </span>
                  <span className="flex items-center gap-1 text-[12px] text-ink2">
                    展开编辑
                    <ChevronDownIcon size={14} />
                  </span>
                </button>

                {unmatched.length > 0 && (
                  <div className="board-note" data-tone="warn">
                    <span className="board-note-icon">
                      <AlertIcon size={16} />
                    </span>
                    <span className="board-note-text">
                      {unmatched.length} 人未匹配到数据库，按 0 音浪参与分组
                    </span>
                    <div className="board-note-tags">
                      {unmatched.map((u) => (
                        <Tag key={u.row.name} tone="warn">
                          {u.row.name}
                        </Tag>
                      ))}
                    </div>
                    <span className="board-note-spacer" />
                    <Button size="sm" variant="ghost" icon={<EditIcon size={14} />} onClick={() => setView('roster')}>
                      去处理
                    </Button>
                  </div>
                )}

                {excluded.length > 0 && (
                  <div className="board-note board-note-tight" data-tone="neutral">
                    <span className="board-note-text">已排除：{excluded.map((e) => e.name).join('、')}</span>
                  </div>
                )}

                {error && (
                  <div className="board-note" data-tone="bad">
                    <span className="board-note-text">{error}</span>
                  </div>
                )}
              </>
            )}
          </div>
        </section>

        {/* ── 2. 分组结果：整页重心，统计 / 工具 / 看板都在这块 ── */}
        {view !== 'roster' && (
          <section className="board-sect board-sect--focus board-rise-d1" aria-label="分组结果">
            <div className="sect-head">
              <div className="board-sect-id">
                <span className="sect-label">分组结果</span>
                <h2 className="sect-title">看板与均衡数据</h2>
              </div>
              <span className="board-sect-meta">
                {buckets.length ? `${buckets.length} 组 · ${groupedCount} 人` : '尚未分组'}
              </span>
            </div>

            <div className="board-focus">
              {buckets.length === 0 ? (
                <div className="board-empty">
                  <span className="board-empty-icon">
                    <GridIcon size={20} />
                  </span>
                  <p className="board-empty-title">
                    {loading ? '正在解析名单…' : '点击「解析并分组」生成分组'}
                  </p>
                  {!loading && (
                    <>
                      <p className="board-empty-hint">支持蛇形均衡 / 同门回避 / 随机三种策略</p>
                      <ol className="board-empty-steps">
                        <li className="board-empty-step">
                          <b>1</b>确认音浪口径与每组人数
                        </li>
                        <li className="board-empty-step">
                          <b>2</b>把名单粘进「名单文本」
                        </li>
                        <li className="board-empty-step">
                          <b>3</b>解析后拖动卡片微调
                        </li>
                      </ol>
                    </>
                  )}
                </div>
              ) : (
                <>
                  {violations.length > 0 && (
                    <div className="board-note" data-tone="bad">
                      <span className="board-note-icon">
                        <AlertIcon size={16} />
                      </span>
                      <span className="board-note-text font-medium">
                        {violations.length} 条内置条件未满足
                      </span>
                      <div className="board-note-tags">
                        {violations.map((v) => (
                          <Tag key={v.message} tone="bad">
                            {v.message}
                          </Tag>
                        ))}
                      </div>
                      <span className="board-note-spacer" />
                      <Button size="sm" variant="ghost" icon={<BalanceIcon size={14} />} onClick={polish}>
                        均衡微调
                      </Button>
                      <Button size="sm" variant="ghost" icon={<RefreshIcon size={14} />} onClick={regroup}>
                        自动修正
                      </Button>
                    </div>
                  )}
                  <div className="board-stats">
                    <StatsBar buckets={buckets} />
                  </div>
                  <BoardTools
                    query={query}
                    onQueryChange={setQuery}
                    hitCount={highlight?.size ?? 0}
                    dense={dense}
                    onDenseChange={setDense}
                  />
                  <GroupBoard buckets={buckets} highlight={highlight} dense={dense} />
                </>
              )}
            </div>
          </section>
        )}

        {/* ── 3. 导出：收尾区，保持平铺，不与看板争视线 ── */}
        {view !== 'roster' && (
          <section className="board-sect board-sect--export board-rise-d2" aria-label="导出">
            <div className="sect-head">
              <h2 className="sect-title">导出与存档</h2>
              <span className="board-sect-meta">长图、文字版、分享链接与历史方案</span>
            </div>
            <ExportBar buckets={buckets} />
          </section>
        )}
      </div>

      <HistoryPlans />
      <Toaster message={toast} />
    </>
  );
}
