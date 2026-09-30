/**
 * 这里只保留「配置 + 纯展示」用的类型和函数。
 *
 * 分组算法（互斥修复、固定位插入、最小间隔、均衡微调、违规检查）的权威实现
 * 全在 Rust 侧 `pk-core`，浏览器通过 /api/group/* 调用，不再跑第二份 TS 实现。
 */

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

/** 最低组号：这个人必须从指定组号开始，允许更靠后 */
export interface MinGroupRule {
  name: string;
  group: number;
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
  minGroup?: MinGroupRule[];
}

/**
 * 内置条件（用户拍板）：
 * - 玖玉 / 玖妹 / 玖玥 三人两两不能同组
 * - 啸泽固定在第 1 组，啸帆固定在最后一组
 * - 啸昭和啸寻至少相隔 4 组（组号差 ≥ 4）
 * - 浩阳必须在第 6 组或更后
 * - 第 1 组 08:15 开赛，之后每 15 分钟一组
 */
export const DEFAULT_CONSTRAINTS: ConstraintSet = {
  excludeTogether: [
    ['玖玉', '玖妹', '玖玥'],
    ['狼影', '浩冬'],
  ],
  pinned: [
    { name: '啸泽', group: 1 },
    { name: '啸阳', group: 1 },
    { name: '啸帆', group: 'last' },
  ],
  schedule: { start: '08:15', intervalMin: 15 },
  minGap: [{ a: '啸昭', b: '啸寻', gap: 4 }],
  minGroup: [{ name: '浩阳', group: 6 }],
};

function normName(name: string): string {
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

/** 约束是否命中了某人 */
export function isPinnedName(name: string, cs: ConstraintSet = DEFAULT_CONSTRAINTS): PinnedRule | undefined {
  const n = normName(name);
  return cs.pinned.find((r) => normName(r.name) === n);
}
