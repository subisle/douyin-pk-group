import { DEFAULT_CONSTRAINTS, type WaveMetric } from '@pk/shared';
import { useStore, type Strategy } from '../store.js';
import { api } from '../api.js';
import { Button, Field, Panel, Segmented, Toggle, inputClass, selectClass } from './ui.js';
import { ClockIcon, PinIcon, PlusIcon, RefreshIcon } from './icons.js';

/** 内置条件一句话说明 */
function constraintSummary(): string {
  const { excludeTogether, pinned, minGap, minGroup } = DEFAULT_CONSTRAINTS;
  const parts = excludeTogether.map((s) => `${s.join(' / ')} 不同组`);
  for (const r of pinned) parts.push(`${r.name} 固定${r.group === 'last' ? '末组' : `第 ${r.group} 组`}`);
  for (const r of minGap ?? []) parts.push(`${r.a} / ${r.b} 至少隔 ${r.gap} 组`);
  for (const r of minGroup ?? []) parts.push(`${r.name} 从第 ${r.group} 组开始`);
  return parts.join(' · ');
}

const STRATEGIES: Array<{ value: Strategy; label: string; title: string }> = [
  { value: 'snake', label: '蛇形均衡', title: '强弱搭配，各组总音浪最接近' },
  { value: 'snake-no-master', label: '蛇形 + 同门回避', title: '蛇形基础上尽量拆开同师傅的人' },
  { value: 'sequential', label: '总音浪从高到低', title: '按累计总音浪降序依次直填，前 8 人第 1 组；忽略内置条件' },
  {
    value: 'sequential-last-n',
    label: '近 N 天从高到低',
    title: '按最近 N 天音浪合计降序依次直填，天数沿用左侧「统计天数」；忽略内置条件',
  },
  { value: 'random', label: '随机', title: '随机洗牌后依次切组' },
];

export function WaveConfig({ onAddBucket }: { onAddBucket: () => void }) {
  const { metric, days, groupSize, strategy, title, brand, startTime, intervalMin, loading, display } = useStore();
  const patch = useStore((s) => s.patch);
  const setDisplay = useStore((s) => s.setDisplay);
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
            onChange={(v) =>
              // 口径决定「从高到低」按哪一列排序，两个策略各绑定一种口径，
              // 手动切口径时把策略一起换掉，免得出现「近 N 天从高到低 + 总音浪」这种自相矛盾的组合
              patch(
                v === 'total' && strategy === 'sequential-last-n'
                  ? { metric: v, strategy: 'sequential' }
                  : { metric: v },
              )
            }
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
            className={`${selectClass} w-[164px]`}
            value={strategy}
            title={STRATEGIES.find((s) => s.value === strategy)?.title}
            onChange={(e) => {
              const value = e.target.value as Strategy;
              // 「从高到低」的排序依据就是音浪口径，选策略时把口径一起钉死
              if (value === 'sequential') patch({ strategy: value, metric: 'total' });
              else if (value === 'sequential-last-n') patch({ strategy: value, metric: 'lastN' });
              else patch({ strategy: value });
            }}
          >
            {STRATEGIES.map((s) => (
              <option key={s.value} value={s.value} title={s.title}>
                {s.value === 'sequential-last-n' ? `近 ${days} 天从高到低` : s.label}
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

      {/* 展示页模块显隐：随方案落库，/s/:id 只渲染打开的模块 */}
      <div className="mt-3 flex flex-wrap items-center gap-x-4 gap-y-2 border-t border-line pt-3">
        <span className="text-[12px] font-medium text-ink">展示页显示</span>
        <Toggle
          label="音浪数值"
          checked={display.wave}
          title="关掉后展示页不出现任何音浪数字，只剩名单和赛程"
          onChange={(v) => setDisplay({ wave: v, ...(v ? {} : { ranking: false, daily: false }) })}
        />
        <Toggle label="音浪排行榜" checked={display.ranking} onChange={(v) => setDisplay({ ranking: v })} />
        <Toggle label="每日音浪" checked={display.daily} onChange={(v) => setDisplay({ daily: v })} />
        <Toggle label="PK 分组" checked={display.groups} onChange={(v) => setDisplay({ groups: v })} />
        <span className="text-[11px] text-ink3">保存方案或点「分享链接」后生效</span>
      </div>
    </Panel>
  );
}
