import { queryReadonly, getActiveId, type SourceId } from '../db/mysql.js';
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
  const hit = dateCaches.get(src);
  if (!force && hit && Date.now() - hit.at < TTL) return hit;

  const dia = dialectOf(src);
  const rows = await queryReadonly<DateRow>(dia.datesSql, [], src);
  const dates = rows.map((r) => r.d);

  // 基线日仅作标记（首日 wave 是「账号累计音浪」快照，不是当日增量）。
  // 它是总音浪的基数，必须计入 total 口径；只有 lastN（近N日增量）口径要排除它。
  let baseline: string | null = null;
  if (dia.baselineSql) {
    const b = await queryReadonly<DateRow>(dia.baselineSql, [], src);
    baseline = b[0]?.d ?? null;
  }

  const info: DateCache = { at: Date.now(), dates, baseline };
  dateCaches.set(src, info);
  return info;
}

/** 全部采集日期（含基线日：它是累计基数，total 口径必须计入） */
export async function getAvailableDates(force = false, sourceId?: SourceId): Promise<string[]> {
  return (await getDatesInfo(force, sourceId)).dates;
}

/**
 * 基线日 = 最早那个采集日（限 A 库）。那天没有上一个快照（prev_snapshot_date 全为 NULL），
 * daily_metric.wave 记的是「账号累计音浪」而不是当日增量
 * （例：玖玉 09-21 = 167 万，而之后每天只有几千到两万）。
 * 注意：它不是脏数据——它正是总音浪的基数，「总计」口径必须计入；
 * 只有「近N天」这类增量口径需要排除，否则会把百万级快照算进近几日的增量里。
 */
export async function getBaselineDate(force = false, sourceId?: SourceId): Promise<string | null> {
  return (await getDatesInfo(force, sourceId)).baseline;
}

/**
 * 音浪口径：
 * - total：全部日期求和 = 基线日累计快照 + 之后每日增量 ≈ 真实累计总音浪
 * - lastN：最近 N 个「增量日」求和（排除基线日，避免把累计快照算进近几日）
 */
export async function getWaves(
  metric: WaveMetric,
  days = 3,
  personIds?: string[],
  force = false,
  sourceId?: SourceId,
): Promise<{ metric: WaveMetric; days: number; dates: string[]; waves: Record<string, number> }> {
  const src = sourceId ?? getActiveId();
  const info = await getDatesInfo(force, src);
  // lastN 只在增量日里取近 N 天；total 用全部日期（含基线日累计基数）
  const deltaDates = info.baseline ? info.dates.filter((d) => d !== info.baseline) : info.dates;
  const useDates = metric === 'lastN' ? deltaDates.slice(0, Math.max(1, days)) : info.dates;

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
