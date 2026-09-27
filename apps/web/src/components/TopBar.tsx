import type { WaveMetric } from '@pk/shared';
import { useStore } from '../store.js';
import { Button, Pill } from './ui.js';
import { HistoryIcon, LayersIcon } from './icons.js';

export function TopBar() {
  const dbOk = useStore((s) => s.dbOk);
  const latestDate = useStore((s) => s.latestDate);
  const dates = useStore((s) => s.dates);
  const baselineDate = useStore((s) => s.baselineDate);
  const anchorCount = useStore((s) => s.anchorCount);
  const openPlans = useStore((s) => s.openPlans);
  const loading = useStore((s) => s.loading);

  return (
    <header className="flex flex-wrap items-center justify-between gap-3">
      <div className="flex items-center gap-2.5">
        <span className="flex h-7 w-7 items-center justify-center rounded-ctl bg-brand">
          <LayersIcon />
        </span>
        <h1 className="text-[20px] font-semibold leading-7 text-ink">PK 挑战赛 · 均衡分组</h1>
      </div>

      <div className="flex flex-wrap items-center gap-3">
        <Pill tone={dbOk ? 'good' : dbOk === false ? 'bad' : 'neutral'}>
          <span
            className="h-1.5 w-1.5 rounded-full"
            style={{ background: dbOk ? '#12b76a' : dbOk === false ? '#f04438' : '#98a2b3' }}
          />
          {loading ? '读取中' : dbOk ? 'douyinxs 已连接' : dbOk === false ? '数据库未连接' : '检测中'}
        </Pill>
        {latestDate && <span className="text-[12px] text-ink2">数据截止 {latestDate}</span>}
        {dates.length > 0 && <span className="text-[12px] text-ink3">可用日期 {dates.length} 天</span>}
        {baselineDate && (
          <span className="text-[12px] text-ink3" title="首次采集那天记的是账号累计音浪，已作为基数计入总音浪排名">
            累计基数日 {baselineDate}
          </span>
        )}
        {anchorCount > 0 && <span className="text-[12px] text-ink3">主播库 {anchorCount} 人</span>}
        <Button icon={<HistoryIcon size={14} />} onClick={() => void openPlans()}>
          历史方案
        </Button>
      </div>
    </header>
  );
}

export const METRIC_LABEL: Record<WaveMetric, string> = {
  lastN: '近 N 天',
  total: '总音浪',
};
