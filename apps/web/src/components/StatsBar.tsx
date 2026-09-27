import { formatWave } from '@pk/shared';
import { useStore } from '../store.js';
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
  // 均衡数据由后端算好后放在 store（前端不跑第二份算法）
  const balance = useStore((s) => s.balance);
  const locked = useStore((s) => s.locked);

  const count = buckets.reduce((s, b) => s + b.players.length, 0);
  const total = buckets.reduce((s, b) => s + b.players.reduce((t, p) => t + p.wave, 0), 0);
  const capacity = buckets.length * Math.max(8, Math.max(...buckets.map((b) => b.players.length), 0));

  const spread = balance?.spread ?? 0;
  const spreadTone = spread < 5 ? 'good' : spread < 15 ? 'warn' : 'bad';
  const spreadLabel = spread < 5 ? '优秀' : spread < 15 ? '可接受' : '偏大';

  return (
    <div className="flex flex-wrap gap-3">
      <Stat label="总音浪" value={formatWave(total)} />
      <Stat label="参赛人数" value={`${count} 人`} extra={locked.length ? <span className="text-[11px] text-ink3">{locked.length} 人已锁定</span> : undefined} />
      <Stat label="分组数" value={`${buckets.length} 组`} extra={<span className="text-[11px] text-ink3">容量 {count}/{capacity}</span>} />
      <Stat label="最高组音浪" value={formatWave(balance?.max ?? 0)} />
      <Stat label="最低组音浪" value={formatWave(balance?.min ?? 0)} />
      <Stat
        label="组间极差比"
        value={`${spread}%`}
        extra={<Tag tone={spreadTone}>{spreadLabel}</Tag>}
      />
    </div>
  );
}
