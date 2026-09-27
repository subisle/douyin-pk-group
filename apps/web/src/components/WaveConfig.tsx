import { DEFAULT_CONSTRAINTS, type WaveMetric } from '@pk/shared';
import { useStore, type Strategy } from '../store.js';
import { api } from '../api.js';
import { Button, Field, Panel, Segmented, inputClass, selectClass } from './ui.js';
import { ClockIcon, PinIcon, PlusIcon, RefreshIcon } from './icons.js';

/** 内置条件一句话说明 */
function constraintSummary(): string {
  const { excludeTogether, pinned } = DEFAULT_CONSTRAINTS;
  const parts = excludeTogether.map((s) => `${s.join(' / ')} 不同组`);
  for (const r of pinned) parts.push(`${r.name} 固定${r.group === 'last' ? '末组' : `第 ${r.group} 组`}`);
  return parts.join(' · ');
}

const STRATEGIES: Array<{ value: Strategy; label: string; title: string }> = [
  { value: 'snake', label: '蛇形均衡', title: '强弱搭配，各组总音浪最接近' },
  { value: 'snake-no-master', label: '蛇形 + 同门回避', title: '蛇形基础上尽量拆开同师傅的人' },
  { value: 'sequential', label: '从高到低', title: '按音浪降序依次切组，第 1 组最强，不做均衡' },
  { value: 'random', label: '随机', title: '随机洗牌后依次切组' },
];

export function WaveConfig({ onAddBucket }: { onAddBucket: () => void }) {
  const { metric, days, groupSize, strategy, title, brand, startTime, intervalMin, loading } = useStore();
  const patch = useStore((s) => s.patch);
  const parse = useStore((s) => s.parse);

  const forceRefresh = async () => {
    try {
      await api.refreshCache();
      await parse(true);
      useStore.getState().notify('已强制刷新缓存');
    } catch (e) {
      useStore.getState().notify(`刷新失败：${(e as Error).message}`);
    }
  };

  return (
    <Panel className="p-4">
      <div className="flex flex-wrap items-end gap-4">
        <Field label="音浪口径">
          <Segmented<WaveMetric>
            options={[
              { value: 'lastN', label: `近 ${days} 天` },
              { value: 'total', label: '总音浪' },
            ]}
            value={metric}
            onChange={(v) => patch({ metric: v })}
          />
        </Field>

        <Field label="统计天数">
          <input
            type="number"
            min={1}
            max={30}
            className={`${inputClass} w-[72px] nums`}
            value={days}
            disabled={metric === 'total'}
            onChange={(e) => patch({ days: Math.max(1, Math.min(30, Number(e.target.value) || 1)) })}
          />
        </Field>

        <Field label="每组上限">
          <input
            type="number"
            min={2}
            max={30}
            className={`${inputClass} w-[72px] nums`}
            value={groupSize}
            onChange={(e) => patch({ groupSize: Math.max(2, Math.min(30, Number(e.target.value) || 8)) })}
          />
        </Field>

        <Field label="分组策略">
          <select
            className={`${selectClass} w-[150px]`}
            value={strategy}
            title={STRATEGIES.find((s) => s.value === strategy)?.title}
            onChange={(e) => patch({ strategy: e.target.value as Strategy })}
          >
            {STRATEGIES.map((s) => (
              <option key={s.value} value={s.value} title={s.title}>
                {s.label}
              </option>
            ))}
          </select>
        </Field>

        <Field label="第 1 组开赛">
          <input
            type="time"
            className={`${inputClass} w-[92px] nums`}
            value={startTime}
            onChange={(e) => patch({ startTime: e.target.value || '08:15' })}
          />
        </Field>

        <Field label="组间隔（分钟）">
          <input
            type="number"
            min={1}
            max={120}
            className={`${inputClass} w-[72px] nums`}
            value={intervalMin}
            onChange={(e) => patch({ intervalMin: Math.max(1, Math.min(120, Number(e.target.value) || 15)) })}
          />
        </Field>

        <Field label="品牌名" className="min-w-[110px]">
          <input className={`${inputClass} w-[130px]`} value={brand} onChange={(e) => patch({ brand: e.target.value })} />
        </Field>

        <Field label="方案名" className="min-w-[140px] flex-1">
          <input className={`${inputClass} w-full`} value={title} onChange={(e) => patch({ title: e.target.value })} />
        </Field>

        <div className="flex gap-2">
          <Button variant="primary" icon={<RefreshIcon size={14} />} onClick={() => void parse(false)} disabled={loading}>
            {loading ? '解析中…' : '解析并分组'}
          </Button>
          <Button onClick={() => void forceRefresh()} disabled={loading} title="重新从数据库拉取最新音浪">
            手动读取
          </Button>
          <Button icon={<PlusIcon size={14} />} onClick={onAddBucket}>
            新增空组
          </Button>
        </div>
      </div>

      <div className="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1.5 border-t border-line pt-3 text-[12px] text-ink2">
        <span className="flex items-center gap-1 font-medium text-ink">
          <PinIcon size={13} />
          内置条件
        </span>
        <span>{constraintSummary()}</span>
        <span className="flex items-center gap-1">
          <ClockIcon size={13} />
          按顺序每 {intervalMin} 分钟一组
        </span>
      </div>
    </Panel>
  );
}
