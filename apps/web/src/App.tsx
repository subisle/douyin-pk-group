import { useEffect, useMemo } from 'react';
import { findViolations } from '@pk/shared';
import { useStore } from './store.js';
import { TopBar } from './components/TopBar.js';
import { WaveConfig } from './components/WaveConfig.js';
import { RosterImport } from './components/RosterImport.js';
import { StatsBar } from './components/StatsBar.js';
import { GroupBoard } from './components/GroupBoard.js';
import { ExportBar } from './components/ExportBar.js';
import { HistoryPlans } from './components/HistoryPlans.js';
import { Button, Tag, Toaster } from './components/ui.js';
import { AlertIcon, ChevronDownIcon, EditIcon, RefreshIcon } from './components/icons.js';

export default function App() {
  const view = useStore((s) => s.view);
  const setView = useStore((s) => s.setView);
  const buckets = useStore((s) => s.buckets);
  const players = useStore((s) => s.players);
  const unmatched = useStore((s) => s.unmatched);
  const excluded = useStore((s) => s.excluded);
  const loading = useStore((s) => s.loading);
  const error = useStore((s) => s.error);
  const toast = useStore((s) => s.toast);
  const init = useStore((s) => s.init);
  const addBucket = useStore((s) => s.addBucket);
  const regroup = useStore((s) => s.regroup);
  const constraints = useStore((s) => s.constraints);

  useEffect(() => {
    void init();
  }, [init]);

  const activeCount = players.filter((p) => !p.excluded).length;

  const violations = useMemo(
    () => findViolations(buckets.map((b) => b.players), constraints()),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [buckets, constraints],
  );

  return (
    <div className="mx-auto flex max-w-[1440px] flex-col gap-4 px-5 py-5 pb-16">
      <TopBar />
      <WaveConfig onAddBucket={addBucket} />

      {view === 'roster' ? (
        <RosterImport />
      ) : (
        <>
          <button
            type="button"
            onClick={() => setView('roster')}
            className="flex items-center justify-between rounded-card bg-subtle px-3.5 py-3 text-left transition-colors hover:brightness-95"
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
            <div className="flex flex-wrap items-center gap-2 rounded-card border border-warn-soft bg-warn-soft px-3.5 py-3">
              <span className="text-warn-ink">
                <AlertIcon size={16} />
              </span>
              <span className="text-[13px] text-warn-ink">
                {unmatched.length} 人未匹配到数据库，按 0 音浪参与分组
              </span>
              <div className="flex flex-wrap gap-1.5">
                {unmatched.map((u) => (
                  <Tag key={u.row.name} tone="warn">
                    {u.row.name}
                  </Tag>
                ))}
              </div>
              <span className="flex-1" />
              <Button size="sm" variant="ghost" icon={<EditIcon size={14} />} onClick={() => setView('roster')}>
                去处理
              </Button>
            </div>
          )}

          {excluded.length > 0 && (
            <div className="rounded-card bg-subtle px-3.5 py-2.5 text-[12px] text-ink2">
              已排除：{excluded.map((e) => e.name).join('、')}
            </div>
          )}

          {error && (
            <div className="rounded-card border border-bad-soft bg-bad-soft px-3.5 py-2.5 text-[13px] text-bad">{error}</div>
          )}

          {buckets.length === 0 ? (
            <div className="flex flex-col items-center gap-2 rounded-panel border border-dashed border-line2 bg-surface py-20">
              <span className="text-[14px] text-ink2">{loading ? '正在解析名单…' : '点击「解析并分组」生成分组'}</span>
              {!loading && <span className="text-[12px] text-ink3">支持蛇形均衡 / 同门回避 / 随机三种策略</span>}
            </div>
          ) : (
            <div className="flex flex-col gap-3">
              {violations.length > 0 && (
                <div className="flex flex-wrap items-center gap-2 rounded-card border border-bad-soft bg-bad-soft px-3.5 py-3">
                  <span className="text-bad">
                    <AlertIcon size={16} />
                  </span>
                  <span className="text-[13px] font-medium text-bad">
                    {violations.length} 条内置条件未满足
                  </span>
                  <div className="flex flex-wrap gap-1.5">
                    {violations.map((v) => (
                      <Tag key={v.message} tone="bad">
                        {v.message}
                      </Tag>
                    ))}
                  </div>
                  <span className="flex-1" />
                  <Button size="sm" variant="ghost" icon={<RefreshIcon size={14} />} onClick={regroup}>
                    自动修正
                  </Button>
                </div>
              )}
              <StatsBar buckets={buckets} />
              <GroupBoard buckets={buckets} />
            </div>
          )}

          <ExportBar buckets={buckets} />
        </>
      )}

      <HistoryPlans />
      <Toaster message={toast} />
    </div>
  );
}
