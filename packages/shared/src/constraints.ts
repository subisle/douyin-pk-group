import type { Player } from './types.js';

/** 固定位：把某人钉死在指定组（1 起数；'last' = 最后一组） */
export interface PinnedRule {
  name: string;
  group: number | 'last';
}

/** 开赛时间：第 1 组 start 开始，之后每组顺延 intervalMin 分钟 */
export interface ScheduleRule {
  start: string; // "08:15"
  intervalMin: number;
}

/** 最小间隔：两个人所在组号之差必须 ≥ gap（组数不足时约束自动放宽跳过） */
export interface MinGapRule {
  a: string;
  b: string;
  gap: number;
}

export interface ConstraintSet {
  /** 互斥组：同一数组内的人两两不能分到同一组 */
  excludeTogether: string[][];
  /** 固定位 */
  pinned: PinnedRule[];
  /** 开赛时间 */
  schedule: ScheduleRule;
  /** 最小间隔（可选，向后兼容旧调用） */
  minGap?: MinGapRule[];
}

/**
 * 内置条件（用户拍板）：
 * - 玖玉 / 玖妹 / 玖玥 三人两两不能同组
 * - 啸泽固定在第 1 组，啸帆固定在最后一组
 * - 啸昭和啸寻至少相隔 4 组（组号差 ≥ 4）
 * - 第 1 组 08:15 开赛，之后每 15 分钟一组
 */
export const DEFAULT_CONSTRAINTS: ConstraintSet = {
  excludeTogether: [['玖玉', '玖妹', '玖玥']],
  pinned: [
    { name: '啸泽', group: 1 },
    { name: '啸帆', group: 'last' },
  ],
  schedule: { start: '08:15', intervalMin: 15 },
  minGap: [{ a: '啸昭', b: '啸寻', gap: 4 }],
};

export function normName(name: string): string {
  return name.trim();
}

/** 第 index 组（0 起）的开赛时间，如 08:15 / 08:30 */
export function slotTime(index: number, schedule: ScheduleRule = DEFAULT_CONSTRAINTS.schedule): string {
  const [h, m] = schedule.start.split(':').map((x) => Number(x) || 0);
  const total = h * 60 + m + Math.max(0, index) * Math.max(0, schedule.intervalMin);
  const hh = Math.floor(total / 60) % 24;
  const mm = total % 60;
  return `${String(hh).padStart(2, '0')}:${String(mm).padStart(2, '0')}`;
}

/** 固定位解析成实际的组下标（0 起） */
export function resolvePinnedIndex(rule: PinnedRule, groupCount: number): number {
  if (rule.group === 'last') return Math.max(0, groupCount - 1);
  return Math.min(Math.max(0, rule.group - 1), Math.max(0, groupCount - 1));
}

/** 按名字在名单里找人（忽略首尾空格） */
export function findByName<T extends { name: string }>(list: T[], name: string): T | undefined {
  const target = normName(name);
  return list.find((x) => normName(x.name) === target);
}

/** 约束是否命中了某人 */
export function isPinnedName(name: string, cs: ConstraintSet = DEFAULT_CONSTRAINTS): PinnedRule | undefined {
  const n = normName(name);
  return cs.pinned.find((r) => normName(r.name) === n);
}

export function excludedSetsOf(name: string, cs: ConstraintSet = DEFAULT_CONSTRAINTS): string[][] {
  const n = normName(name);
  return cs.excludeTogether.filter((set) => set.some((x) => normName(x) === n));
}

const sumWave = (g: Player[]) => g.reduce((s, p) => s + p.wave, 0);

/**
 * 互斥修复：把同一互斥组里多出来的人换到没有该组其他人的组，
 * 交换时优先不破坏音浪均衡，且绝不移动固定位的人。
 */
export function resolveExclusions(
  groups: Player[][],
  sets: string[][],
  protectedKeys: Set<string> = new Set(),
  maxIter = 300,
): Player[][] {
  const setList = sets.map((s) => new Set(s.map(normName))).filter((s) => s.size > 1);
  if (!setList.length) return groups.map((g) => [...g]);

  const result = groups.map((g) => [...g]);
  const countIn = (g: Player[], s: Set<string>) => g.reduce((n, p) => n + (s.has(normName(p.name)) ? 1 : 0), 0);
  const conflictCount = () =>
    result.reduce((n, g) => n + setList.reduce((m, s) => m + Math.max(0, countIn(g, s) - 1), 0), 0);

  const total = result.reduce((s, g) => s + sumWave(g), 0);
  const mean = result.length ? total / result.length : 0;

  for (let iter = 0; iter < maxIter; iter++) {
    if (conflictCount() === 0) break;
    let best: { ai: number; pi: number; bi: number; qi: number; score: number } | null = null;

    for (let ai = 0; ai < result.length; ai++) {
      const sumA = sumWave(result[ai]);
      for (let si = 0; si < setList.length; si++) {
        const s = setList[si];
        if (countIn(result[ai], s) < 2) continue;
        for (let pi = 0; pi < result[ai].length; pi++) {
          const p = result[ai][pi];
          if (!s.has(normName(p.name)) || protectedKeys.has(p.key)) continue;
          for (let bi = 0; bi < result.length; bi++) {
            if (bi === ai) continue;
            if (countIn(result[bi], s) > 0) continue;
            const sumB = sumWave(result[bi]);
            for (let qi = 0; qi < result[bi].length; qi++) {
              const q = result[bi][qi];
              if (s.has(normName(q.name)) || protectedKeys.has(q.key)) continue;

              // 交换是否会引入其它互斥组的冲突
              let extra = 0;
              for (const ss of setList) {
                if (ss === s) continue;
                const before = Math.max(0, countIn(result[ai], ss) - 1) + Math.max(0, countIn(result[bi], ss) - 1);
                const ca = countIn(result[ai], ss) + (ss.has(normName(q.name)) ? 1 : 0);
                const cb = countIn(result[bi], ss) + (ss.has(normName(p.name)) ? 1 : 0);
                extra += Math.max(0, ca - 1) + Math.max(0, cb - 1) - before;
              }

              // 均衡损伤：交换后两组与均值的偏离变化
              const before = Math.abs(sumA - mean) + Math.abs(sumB - mean);
              const after =
                Math.abs(sumA - p.wave + q.wave - mean) + Math.abs(sumB - q.wave + p.wave - mean);
              const score = extra * 1e9 + (after - before);
              if (!best || score < best.score) best = { ai, pi, bi, qi, score };
            }
          }
        }
      }
    }

    if (!best) break;
    const { ai, pi, bi, qi } = best;
    const tmp = result[ai][pi];
    result[ai][pi] = result[bi][qi];
    result[bi][qi] = tmp;
  }
  return result;
}

/** 把固定位的人放回指定组（按音浪降序插入，保持组内排序） */
export function placePinned(
  groups: Player[][],
  players: Player[],
  pinned: Map<string, number>,
): Player[][] {
  const result = groups.map((g) => [...g]);
  for (const [key, gi] of pinned) {
    const p = players.find((x) => x.key === key);
    if (!p) continue;
    while (result.length <= gi) result.push([]);
    const arr = result[gi];
    if (arr.some((x) => x.key === key)) continue;
    const at = arr.findIndex((x) => x.wave < p.wave);
    if (at < 0) arr.push(p);
    else arr.splice(at, 0, p);
  }
  return result;
}

/**
 * 均衡微调：在不破坏内置条件的前提下，反复做「最重组的重物 ↔ 最轻组的轻物」交换，
 * 把组间极差压下去。只接受能让极差变小的交换，所以任何策略加这一步都只会更好。
 */
export interface PolishOptions {
  /** 不能移动的人（固定位） */
  protectedKeys?: Set<string>;
  /** 互斥组：交换后不能让同一组里出现两个 */
  excludeSets?: string[][];
  /** 是否同时避免同门（师傅）扎堆 */
  avoidMaster?: boolean;
  /** 最小间隔规则：交换后违约数不许增加 */
  minGapRules?: MinGapRule[];
  maxIter?: number;
}

function leaderOf(p: Player): string {
  return (p.leader ?? '').trim();
}

function masterConflicts(groups: Player[][]): number {
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

export function polishBalance(groups: Player[][], options: PolishOptions = {}): Player[][] {
  const {
    protectedKeys = new Set(),
    excludeSets = [],
    avoidMaster = false,
    minGapRules = [],
    maxIter = 400,
  } = options;
  const setList = excludeSets.map((s) => new Set(s.map(normName))).filter((s) => s.size > 1);
  const result = groups.map((g) => [...g]);
  if (result.length < 2) return result;
  const baseGapConflicts = gapConflicts(result, minGapRules);

  const totals = result.map((g) => sumWave(g));
  /** 交换后两组的新总和，据此算极差（O(g)，避免每候选重算全量） */
  const spreadAfter = (ai: number, bi: number, p: Player, q: Player): number => {
    const a = totals[ai] - p.wave + q.wave;
    const b = totals[bi] - q.wave + p.wave;
    let mx = -Infinity;
    let mn = Infinity;
    for (let i = 0; i < totals.length; i++) {
      const v = i === ai ? a : i === bi ? b : totals[i];
      if (v > mx) mx = v;
      if (v < mn) mn = v;
    }
    return mx - mn;
  };
  /** 交换后两组内各互斥组的人数是否超标 */
  const conflictsAfter = (ai: number, bi: number, p: Player, q: Player): number => {
    let n = 0;
    for (const s of setList) {
      const hp = s.has(normName(p.name)) ? 1 : 0;
      const hq = s.has(normName(q.name)) ? 1 : 0;
      const ca = result[ai].reduce((m, x) => m + (s.has(normName(x.name)) ? 1 : 0), 0) - hp + hq;
      const cb = result[bi].reduce((m, x) => m + (s.has(normName(x.name)) ? 1 : 0), 0) - hq + hp;
      if (ca > 1) n += ca - 1;
      if (cb > 1) n += cb - 1;
    }
    return n;
  };
  /** 交换后两组的同门重复数 */
  const masterAfter = (ai: number, bi: number, p: Player, q: Player): number => {
    const count = (arr: Player[], swapOut: Player, swapIn: Player) => {
      const seen = new Map<string, number>();
      for (const x of arr) {
        const l = leaderOf(x);
        if (!l) continue;
        seen.set(l, (seen.get(l) ?? 0) + 1);
      }
      const lo = leaderOf(swapOut);
      const li = leaderOf(swapIn);
      if (lo) seen.set(lo, (seen.get(lo) ?? 0) - 1);
      if (li) seen.set(li, (seen.get(li) ?? 0) + 1);
      let n = 0;
      for (const c of seen.values()) if (c > 1) n += c - 1;
      return n;
    };
    return count(result[ai], p, q) + count(result[bi], q, p);
  };

  let current = Math.max(...totals) - Math.min(...totals);

  for (let iter = 0; iter < maxIter; iter++) {
    let best: { ai: number; bi: number; pi: number; qi: number; cost: number } | null = null;
    for (let ai = 0; ai < result.length; ai++) {
      for (let bi = ai + 1; bi < result.length; bi++) {
        for (let pi = 0; pi < result[ai].length; pi++) {
          const p = result[ai][pi];
          if (protectedKeys.has(p.key)) continue;
          for (let qi = 0; qi < result[bi].length; qi++) {
            const q = result[bi][qi];
            if (protectedKeys.has(q.key)) continue;
            if (setList.length && conflictsAfter(ai, bi, p, q) > 0) continue;
            // 最小间隔：交换后违约数不许增加（起点可能是 0，也可能带着既有违约一起优化）
            if (minGapRules.length) {
              const tmpP = result[ai][pi];
              result[ai][pi] = q;
              result[bi][qi] = tmpP;
              const gapAfter = gapConflicts(result, minGapRules);
              result[ai][pi] = p;
              result[bi][qi] = q;
              if (gapAfter > baseGapConflicts) continue;
            }
            const cost =
              spreadAfter(ai, bi, p, q) + (avoidMaster ? masterAfter(ai, bi, p, q) * 1e6 : 0);
            if (!best || cost < best.cost) best = { ai, bi, pi, qi, cost };
          }
        }
      }
    }
    if (!best || best.cost >= current - 1) break;
    const { ai, bi, pi, qi } = best;
    const p = result[ai][pi];
    const q = result[bi][qi];
    result[ai][pi] = q;
    result[bi][qi] = p;
    totals[ai] += q.wave - p.wave;
    totals[bi] += p.wave - q.wave;
    current = best.cost;
  }
  return result;
}

export interface Violation {
  type: 'exclude' | 'pin' | 'gap';
  groupIndex: number;
  message: string;
  /** 涉及的人名，用于卡片高亮 */
  names: string[];
}

/** 检查当前分组有没有违反内置条件（手动拖拽后用来告警） */
export function findViolations(groups: Player[][], cs: ConstraintSet = DEFAULT_CONSTRAINTS): Violation[] {
  const out: Violation[] = [];

  for (const set of cs.excludeTogether) {
    const s = new Set(set.map(normName));
    groups.forEach((g, i) => {
      const hit = g.filter((p) => s.has(normName(p.name)));
      if (hit.length > 1) {
        out.push({
          type: 'exclude',
          groupIndex: i,
          message: `第 ${i + 1} 组：${hit.map((p) => p.name).join('、')} 不能同组`,
          names: hit.map((p) => p.name),
        });
      }
    });
  }

  for (const rule of cs.pinned) {
    const target = resolvePinnedIndex(rule, groups.length);
    const label = rule.group === 'last' ? '最后一组' : `第 ${rule.group} 组`;
    let at = -1;
    groups.forEach((g, i) => {
      if (g.some((p) => normName(p.name) === normName(rule.name))) at = i;
    });
    if (at < 0) {
      out.push({ type: 'pin', groupIndex: target, message: `名单里没有找到「${rule.name}」（应固定在${label}）`, names: [rule.name] });
    } else if (at !== target) {
      out.push({
        type: 'pin',
        groupIndex: at,
        message: `${rule.name} 应固定在${label}，当前在第 ${at + 1} 组`,
        names: [rule.name],
      });
    }
  }

  for (const rule of cs.minGap ?? []) {
    const ia = groupIndexOfName(groups, rule.a);
    const ib = groupIndexOfName(groups, rule.b);
    if (ia < 0 || ib < 0) continue;
    if (Math.abs(ia - ib) >= rule.gap) continue;
    out.push({
      type: 'gap',
      groupIndex: Math.min(ia, ib),
      message: `${rule.a}（第 ${ia + 1} 组）和 ${rule.b}（第 ${ib + 1} 组）需至少间隔 ${rule.gap} 组`,
      names: [rule.a, rule.b],
    });
  }

  return out;
}

function groupIndexOfName(groups: Player[][], name: string): number {
  const n = normName(name);
  return groups.findIndex((g) => g.some((p) => normName(p.name) === n));
}

/** 统计最小间隔违约数（找不到人不计） */
export function gapConflicts(groups: Player[][], rules: MinGapRule[] = []): number {
  let n = 0;
  for (const r of rules) {
    const ia = groupIndexOfName(groups, r.a);
    const ib = groupIndexOfName(groups, r.b);
    if (ia < 0 || ib < 0) continue;
    if (Math.abs(ia - ib) < r.gap) n++;
  }
  return n;
}

/**
 * 最小间隔修复：对违约的两人，把其中非固定位的一人换到满足间隔的组，
 * 换入者选择对音浪均衡损伤最小、且不制造互斥冲突的对象。
 */
export function resolveMinGaps(
  groups: Player[][],
  rules: MinGapRule[] = [],
  protectedKeys: Set<string> = new Set(),
  excludeSets: string[][] = [],
  maxIter = 200,
): Player[][] {
  const result = groups.map((g) => [...g]);
  const setList = excludeSets.map((s) => new Set(s.map(normName))).filter((s) => s.size > 1);
  if (!rules.length || result.length < 2) return result;

  /** 交换 p(在 gi) 与 q(在 ki) 后，两组成员中互斥组是否超 1 人 */
  const exclusionOk = (gi: number, ki: number, p: Player, q: Player): boolean => {
    for (const s of setList) {
      const cnt = (arr: Player[], out: Player, inn: Player) =>
        arr.reduce((m, x) => m + (s.has(normName(x.name)) ? 1 : 0), 0) -
        (s.has(normName(out.name)) ? 1 : 0) +
        (s.has(normName(inn.name)) ? 1 : 0);
      if (cnt(result[gi], p, q) > 1 || cnt(result[ki], q, p) > 1) return false;
    }
    return true;
  };

  const total = result.reduce((s, g) => s + sumWave(g), 0);
  const mean = result.length ? total / result.length : 0;

  for (let iter = 0; iter < maxIter; iter++) {
    if (gapConflicts(result, rules) === 0) break;
    let best: { gi: number; pi: number; ki: number; qi: number; score: number } | null = null;

    for (const r of rules) {
      const ia = groupIndexOfName(result, r.a);
      const ib = groupIndexOfName(result, r.b);
      if (ia < 0 || ib < 0 || Math.abs(ia - ib) >= r.gap) continue;

      // 两个方向都可尝试：动 a 或动 b（固定位优先不动）
      const moves: Array<{ mi: number; other: number }> = [];
      if (!result[ia].some((p) => protectedKeys.has(p.key) && normName(p.name) === normName(r.a)))
        moves.push({ mi: ia, other: ib });
      if (!result[ib].some((p) => protectedKeys.has(p.key) && normName(p.name) === normName(r.b)))
        moves.push({ mi: ib, other: ia });

      for (const { mi, other } of moves) {
        const fromArr = result[mi];
        for (let pi = 0; pi < fromArr.length; pi++) {
          const p = fromArr[pi];
          const nameP = normName(p.name);
          if (nameP !== normName(r.a) && nameP !== normName(r.b)) continue;
          if (protectedKeys.has(p.key)) continue;
          for (let ki = 0; ki < result.length; ki++) {
            if (ki === mi) continue;
            if (Math.abs(ki - other) < r.gap) continue;
            const sumFrom = sumWave(result[mi]);
            const sumTo = sumWave(result[ki]);
            for (let qi = 0; qi < result[ki].length; qi++) {
              const q = result[ki][qi];
              if (protectedKeys.has(q.key)) continue;
              if (!exclusionOk(mi, ki, p, q)) continue;
              const before = Math.abs(sumFrom - mean) + Math.abs(sumTo - mean);
              const after =
                Math.abs(sumFrom - p.wave + q.wave - mean) + Math.abs(sumTo - q.wave + p.wave - mean);
              const score = after - before;
              if (!best || score < best.score) best = { gi: mi, pi, ki, qi, score };
            }
          }
        }
      }
    }

    if (!best) break;
    const { gi, pi, ki, qi } = best;
    const tmp = result[gi][pi];
    result[gi][pi] = result[ki][qi];
    result[ki][qi] = tmp;
  }
  return result;
}
