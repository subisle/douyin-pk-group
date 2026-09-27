import type { BalanceStat, GroupOptions, GroupStat, Player } from './types.js';
import {
  DEFAULT_CONSTRAINTS,
  findByName,
  gapConflicts,
  placePinned,
  polishBalance,
  resolveExclusions,
  resolveMinGaps,
  resolvePinnedIndex,
  type MinGapRule,
} from './constraints.js';

export function groupCount(total: number, groupSize: number): number {
  return Math.max(1, Math.ceil(total / Math.max(1, groupSize)));
}

function byWaveDesc(a: Player, b: Player): number {
  if (b.wave !== a.wave) return b.wave - a.wave;
  return a.name.localeCompare(b.name, 'zh-Hans-CN');
}

/**
 * 蛇形（S 型折返）均衡分组：按音浪降序，奇数轮反向分配，
 * 使各组总音浪自然收敛。第 1 名与第 14 名同组、第 2 名与第 13 名同组……
 */
export function snakeGroup(players: Player[], groupSize: number): Player[][] {
  const sorted = [...players].sort(byWaveDesc);
  const g = groupCount(sorted.length, groupSize);
  const groups: Player[][] = Array.from({ length: g }, () => []);
  sorted.forEach((p, i) => {
    const round = Math.floor(i / g);
    const idx = i % g;
    const target = round % 2 === 0 ? idx : g - 1 - idx;
    groups[target].push(p);
  });
  return groups;
}

/**
 * 从高到低（梯队式）：按音浪降序依次切组，第 1 组全是最高的一档，
 * 最后一组最低。不做均衡——各组的强弱本身就是阶梯。
 */
export function sequentialGroup(players: Player[], groupSize: number): Player[][] {
  const sorted = [...players].sort(byWaveDesc);
  const g = groupCount(sorted.length, groupSize);
  const groups: Player[][] = Array.from({ length: g }, () => []);
  const base = Math.floor(sorted.length / g);
  const extra = sorted.length % g;
  let cursor = 0;
  for (let i = 0; i < g; i++) {
    const size = base + (i < extra ? 1 : 0);
    groups[i] = sorted.slice(cursor, cursor + size);
    cursor += size;
  }
  return groups;
}

export function randomGroup(players: Player[], groupSize: number, seed?: number): Player[][] {
  const arr = [...players];
  const rand = seed === undefined ? Math.random : mulberry32(seed);
  for (let i = arr.length - 1; i > 0; i--) {
    const j = Math.floor(rand() * (i + 1));
    [arr[i], arr[j]] = [arr[j], arr[i]];
  }
  const sorted = arr.sort(byWaveDesc);
  const g = groupCount(arr.length, groupSize);
  const groups: Player[][] = Array.from({ length: g }, () => []);
  let cursor = 0;
  const base = Math.floor(arr.length / g);
  const extra = arr.length % g;
  for (let i = 0; i < g; i++) {
    const size = base + (i < extra ? 1 : 0);
    groups[i] = sorted.slice(cursor, cursor + size);
    cursor += size;
  }
  return groups;
}

function mulberry32(seed: number) {
  let a = seed >>> 0;
  return function () {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function leaderOf(p: Player): string {
  return (p.leader ?? '').trim();
}

function sameLeaderConflicts(groups: Player[][]): number {
  let n = 0;
  for (const g of groups) {
    const seen = new Map<string, number>();
    for (const p of g) {
      const l = leaderOf(p);
      if (!l) continue;
      seen.set(l, (seen.get(l) ?? 0) + 1);
    }
    for (const c of seen.values()) if (c > 1) n += c - 1;
  }
  return n;
}

/** 同门回避：蛇形之后做局部交换，把同队长的人尽量拆到不同组，交换时尽量少破坏音浪均衡 */
export function resolveMasterConflicts(groups: Player[][], maxIter = 400): Player[][] {
  const result = groups.map((g) => [...g]);
  for (let iter = 0; iter < maxIter; iter++) {
    if (sameLeaderConflicts(result) === 0) break;
    let best: { ai: number; bi: number; pi: number; qi: number; cost: number } | null = null;
    for (let ai = 0; ai < result.length; ai++) {
      const leadersA = new Set(result[ai].map(leaderOf).filter(Boolean));
      for (let pi = 0; pi < result[ai].length; pi++) {
        const p = result[ai][pi];
        const lp = leaderOf(p);
        if (!lp) continue;
        for (let bi = 0; bi < result.length; bi++) {
          if (bi === ai) continue;
          const leadersB = new Set(result[bi].map(leaderOf).filter(Boolean));
          if (leadersB.has(lp)) continue;
          for (let qi = 0; qi < result[bi].length; qi++) {
            const q = result[bi][qi];
            const lq = leaderOf(q);
            if (lq && leadersA.has(lq) && lq !== lp) continue;
            const sumA = result[ai].reduce((s, x) => s + x.wave, 0);
            const sumB = result[bi].reduce((s, x) => s + x.wave, 0);
            const before = Math.abs(sumA - sumB);
            const after = Math.abs(sumA - p.wave + q.wave - (sumB - q.wave + p.wave));
            const gain = before - after;
            const cost = -gain;
            if (!best || cost < best.cost) best = { ai, bi, pi, qi, cost };
          }
        }
      }
    }
    if (!best) break;
    const { ai, bi, pi, qi } = best;
    const tmp = result[ai][pi];
    result[ai][pi] = result[bi][qi];
    result[bi][qi] = tmp;
    if (sameLeaderConflicts(result) === 0) break;
  }
  return result;
}

/**
 * 自动分组 + 内置条件。
 * 顺序：固定位的人先抽出来 → 其余人按策略分 → 固定位插回指定组 → 互斥修复（不移动固定位的人）
 */
export function autoGroup(players: Player[], options: GroupOptions): Player[][] {
  const active = players.filter((p) => !p.excluded);
  if (active.length === 0) return [];

  const cs = options.constraints ?? DEFAULT_CONSTRAINTS;
  const g = groupCount(active.length, options.groupSize);

  /* 1. 固定位 */
  const pinned = new Map<string, number>();
  for (const rule of cs.pinned) {
    const hit = findByName(active, rule.name);
    if (hit) pinned.set(hit.key, resolvePinnedIndex(rule, g));
  }
  const pinnedKeys = new Set(pinned.keys());
  const rest = active.filter((p) => !pinnedKeys.has(p.key));

  /* 2. 其余人按策略分组（保证组数与固定位解析时一致） */
  let groups =
    options.strategy === 'random'
      ? randomGroup(rest, options.groupSize)
      : options.strategy === 'sequential'
        ? sequentialGroup(rest, options.groupSize)
        : snakeGroup(rest, options.groupSize);
  if (options.strategy === 'snake-no-master') {
    groups = resolveMasterConflicts(groups);
  }
  while (groups.length < g) groups.push([]);

  /* 3. 固定位插回 */
  groups = placePinned(groups, active, pinned);

  /* 4. 互斥修复 */
  groups = resolveExclusions(groups, cs.excludeTogether, pinnedKeys);

  /* 4.5 最小间隔修复（啸昭/啸寻这类「至少相隔 N 组」） */
  const minGapRules = cs.minGap ?? [];
  if (minGapRules.length) {
    groups = resolveMinGaps(groups, minGapRules, pinnedKeys, cs.excludeTogether);
  }

  /* 5. 均衡微调：只接受让极差变小的交换，绝不移动固定位、绝不制造新的互斥/间隔冲突。
     从高到低（梯队式）除外——均衡会把阶梯打散，失去这种分组的意义 */
  if (options.strategy !== 'sequential') {
    groups = polishBalance(groups, {
      protectedKeys: pinnedKeys,
      excludeSets: cs.excludeTogether,
      avoidMaster: options.strategy === 'snake-no-master',
      minGapRules,
    });
  }

  return groups.filter((grp) => grp.length > 0);
}

export function computeBalance(groups: Player[][]): BalanceStat {
  const stats: GroupStat[] = groups.map((g, index) => {
    const total = g.reduce((s, p) => s + p.wave, 0);
    return { index, count: g.length, total, avg: g.length ? Math.round(total / g.length) : 0 };
  });
  const totals = stats.map((s) => s.total);
  const max = totals.length ? Math.max(...totals) : 0;
  const min = totals.length ? Math.min(...totals) : 0;
  const mean = totals.length ? totals.reduce((a, b) => a + b, 0) / totals.length : 0;
  const variance = totals.length ? totals.reduce((s, t) => s + (t - mean) ** 2, 0) / totals.length : 0;
  return {
    groups: stats,
    max,
    min,
    mean: Math.round(mean),
    spread: mean ? Number((((max - min) / mean) * 100).toFixed(1)) : 0,
    stdDev: Math.round(Math.sqrt(variance)),
  };
}

/** 拖拽微调：把 player 移动到 targetGroup 的指定位置 */
export function movePlayer(
  groups: Player[][],
  from: { group: number; index: number },
  to: { group: number; index: number },
): Player[][] {
  const next = groups.map((g) => [...g]);
  if (!next[from.group] || !next[to.group]) return groups;
  const [p] = next[from.group].splice(from.index, 1);
  if (!p) return groups;
  const insertAt = Math.max(0, Math.min(to.index, next[to.group].length));
  next[to.group].splice(insertAt, 0, p);
  return next.filter((g, i) => g.length > 0 || i === to.group);
}
