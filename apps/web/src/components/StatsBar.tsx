import { useMemo } from 'react';
import { computeBalance, formatWave } from '@pk/shared';
import type { Bucket } from '../types.js';
import { Tag } from './ui.js';

function Stat({ label, value, extra }: { label: string; value: string; extra?: React.ReactNode }) {
  return (
    <div className="flex flex-1 flex-col gap-1 rounded-card border border-line bg-surface px-3 py-2.5">
      <span className="text-[12px] leading-none text-ink2">{label}</span>
      <span className="nums text-[22px] font-semibold leading-none text-ink">{value}</span>
      {extra}
    </div>
  );
}

export function StatsBar({ buckets }: { buckets: Bucket[] }) {
  const balance = useMemo(() => computeBalance(buckets.map((b) => b.players)), [buckets]);
  const count = buckets.reduce((s, b) => s + b.players.length, 0);
  const total = buckets.reduce((s, b) => s + b.players.reduce((t, p) => t + p.wave, 0), 0);

  const spreadTone = balance.spread < 5 ? 'good' : balance.spread < 15 ? 'warn' : 'bad';
  const spreadLabel = balance.spread < 5 ? '优秀' : balance.spread < 15 ? '可接受' : '偏大';

  return (
    <div className="flex flex-wrap gap-3">
      <Stat label="总音浪" value={formatWave(total)} />
      <Stat label="参赛人数" value={`${count} 人`} />
      <Stat label="分组数" value={`${buckets.length} 组`} />
      <Stat label="最高组音浪" value={formatWave(balance.max)} />
      <Stat label="最低组音浪" value={formatWave(balance.min)} />
      <Stat
        label="组间极差比"
        value={`${balance.spread}%`}
        extra={<Tag tone={spreadTone}>{spreadLabel}</Tag>}
      />
    </div>
  );
}
