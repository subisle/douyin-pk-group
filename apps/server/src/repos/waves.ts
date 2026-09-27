import { queryReadonly, getActiveId, getSource, type SourceId } from '../db/mysql.js';
import { dialectOf } from '../db/dialect.js';
import type { WaveMetric } from '@pk/shared';

interface WaveRow {
  k: number | string;
  w: string | number;
}

interface DateRow {
  d: string;
}

/** 每个数据源一套缓存，切源后不会串到另一个库的数据 */
interface DateCache {
  at: number;
  dates: string[];
  baseline: string | null;
}
const dateCaches = new Map<SourceId, DateCache>();
const waveCaches = new Map<string, { at: number; data: Record<string, number> }>();
const TTL = Number(process.env.WAVE_CACHE_TTL ?? 600) * 1000;

async function getDatesInfo(force = false, sourceId?: SourceId): Promise<DateCache> {
  const src = sourceId ?? getActiveId();
  const cfg = getSource(src);
  const hit = dateCaches.get(src);
  if (!force && hit && Date.now() - hit.at < TTL) return hit;

  const dia = dialectOf(src);
  const rows = await queryReadonly<DateRow>(dia.datesSql, [], src);
  let dates = rows.map((r) => r.d);

  let baseline: string | null = null;
  if (cfg.excludeBaselineDay && dia.baselineSql && dates.length > 1) {
    const b = await queryReadonly<DateRow>(dia.baselineSql, [], src);
    baseline = b[0]?.d ?? null;
    if (baseline) dates = dates.filter((d) => d !== baseline);
  }

  const info: DateCache = { at: Date.now(), dates, baseline };
  dateCaches.set(src, info);
  return info;
}

/** 可用于统计的日期（A 库已剔除基线日） */
export async function getAvailableDates(force = false, sourceId?: SourceId): Promise<string[]> {
  return (await getDatesInfo(force, sourceId)).dates;
}

/**
 * 基线日 = 最早那个采集日（限 A 库）。那天没有上一个快照（prev_snapshot_date 全为 NULL），
 * daily_metric.wave 记的是「账号累计音浪」而不是当日增量，会比正常日高出 10~40 倍
 * （例：浩龙 09-21 = 140.6 万，09-22 / 09-23 只有 16.0 万 / 5.0 万）。
 * 参与求和会把人虚高到百万级，分组直接失衡，所以聚合时剔除。
 */
export async function getBaselineDate(force = false, sourceId?: SourceId): Promise<string | null> {
  return (await getDatesInfo(force, sourceId)).baseline;
}

/**
 * 音浪口径：
 * - lastN：最近 N 个可用日期求和
 * - total：全部可用日期求和
 */
export async function getWaves(
  metric: WaveMetric,
  days = 3,
  personIds?: string[],
  force = false,
  sourceId?: SourceId,
): Promise<{ metric: WaveMetric; days: number; dates: string[]; waves: Record<string, number> }> {
  const src = sourceId ?? getActiveId();
  const available = (await getDatesInfo(force, src)).dates;
  const useDates = metric === 'lastN' ? available.slice(0, Math.max(1, days)) : available;

  const cacheKey = `${src}:${metric}:${days}`;
  const hit = waveCaches.get(cacheKey);
  if (!force && hit && Date.now() - hit.at < TTL) {
    return { metric, days, dates: useDates, waves: filterIds(hit.data, personIds) };
  }

  let waves: Record<string, number> = {};
  if (useDates.length > 0) {
    const placeholders = useDates.map(() => '?').join(',');
    const rows = await queryReadonly<WaveRow>(dialectOf(src).wavesSql(placeholders), useDates, src);
    waves = Object.fromEntries(rows.map((r) => [String(r.k), Number(r.w ?? 0)]));
  }

  waveCaches.set(cacheKey, { at: Date.now(), data: waves });
  return { metric, days, dates: useDates, waves: filterIds(waves, personIds) };
}

function filterIds(data: Record<string, number>, personIds?: string[]): Record<string, number> {
  if (!personIds || personIds.length === 0) return data;
  const set = new Set(personIds);
  return Object.fromEntries(Object.entries(data).filter(([k]) => set.has(k)));
}

export function clearWaveCache(sourceId?: SourceId): void {
  if (sourceId) {
    dateCaches.delete(sourceId);
    for (const key of [...waveCaches.keys()]) if (key.startsWith(`${sourceId}:`)) waveCaches.delete(key);
    return;
  }
  dateCaches.clear();
  waveCaches.clear();
}
