import {
  DEFAULT_ALIASES,
  DEFAULT_EXCLUDE,
  normalizeName,
  parseRoster,
  type AnchorBrief,
  type ParsedRow,
  type Player,
  type WaveMetric,
} from '@pk/shared';
import { loadAnchors } from '../repos/anchors.js';
import { getWaves } from '../repos/waves.js';
import { listAliases } from '../db/sqlite.js';

export interface MatchPayload {
  players: Player[];
  unmatched: Array<{ row: ParsedRow; candidates: AnchorBrief[] }>;
  excluded: ParsedRow[];
  aliasMap: Record<string, string>;
  dates: string[];
  metric: WaveMetric;
  days: number;
}

function uniqCandidates(name: string, anchors: AnchorBrief[]): AnchorBrief[] {
  const key = normalizeName(name);
  const exact = anchors.filter((a) => a.name === name);
  if (exact.length) return exact.slice(0, 10);
  const partial = anchors.filter((a) => normalizeName(a.name).includes(key) || key.includes(normalizeName(a.name)));
  if (partial.length) return partial.slice(0, 10);
  // 末字或首字相同的近似候选
  const loose = anchors.filter((a) => {
    const n = normalizeName(a.name);
    return n.length > 1 && key.length > 1 && (n.slice(-1) === key.slice(-1) || n[0] === key[0]);
  });
  return loose.slice(0, 10);
}

export async function matchRoster(options: {
  text: string;
  metric?: WaveMetric;
  days?: number;
  aliases?: Record<string, string>;
  exclude?: string[];
  force?: boolean;
}): Promise<MatchPayload> {
  const { text, metric = 'lastN', days = 3, exclude = DEFAULT_EXCLUDE, force } = options;
  const aliasMap = { ...DEFAULT_ALIASES, ...listAliases(), ...(options.aliases ?? {}) };
  const anchors = await loadAnchors(force);
  const rows = parseRoster(text, { aliases: aliasMap, exclude });

  const byName = new Map<string, AnchorBrief>();
  const byNorm = new Map<string, AnchorBrief[]>();
  for (const a of anchors) {
    if (!byName.has(a.name)) byName.set(a.name, a);
    const k = normalizeName(a.name);
    const arr = byNorm.get(k) ?? [];
    arr.push(a);
    byNorm.set(k, arr);
  }

  const players: Player[] = [];
  const unmatched: MatchPayload['unmatched'] = [];
  const excluded: ParsedRow[] = [];

  rows.forEach((row, i) => {
    if (row.excluded) {
      excluded.push(row);
      return;
    }
    const direct = byName.get(row.name);
    const normHit = direct ? undefined : (byNorm.get(normalizeName(row.name)) ?? [])[0];
    const hit = direct ?? normHit;
    if (hit) {
      players.push({
        key: `${row.name}#${i}`,
        raw: row.raw,
        name: row.name,
        leader: row.leader,
        personId: hit.personId,
        matched: true,
        wave: 0,
        avatar: hit.avatar ?? null,
        excluded: false,
      });
    } else {
      players.push({
        key: `${row.name}#${i}`,
        raw: row.raw,
        name: row.name,
        leader: row.leader,
        personId: null,
        matched: false,
        wave: 0,
        excluded: false,
      });
      unmatched.push({ row, candidates: uniqCandidates(row.name, anchors) });
    }
  });

  const waveInfo = await getWaves(metric, days, undefined, force);
  for (const p of players) {
    p.wave = p.personId ? (waveInfo.waves[String(p.personId)] ?? 0) : 0;
  }

  return {
    players,
    unmatched,
    excluded,
    aliasMap,
    dates: waveInfo.dates,
    metric,
    days,
  };
}
