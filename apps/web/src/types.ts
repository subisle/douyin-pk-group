import type { Player } from '@pk/shared';

/** 一个分组桶：id 稳定以便 dnd-kit 定位容器，name 可编辑 */
export interface Bucket {
  id: string;
  name: string;
  players: Player[];
}

/** 均衡统计（后端 pk-core 计算，前端只展示） */
export interface BalanceStat {
  groups: Array<{ index: number; count: number; total: number; avg: number }>;
  max: number;
  min: number;
  mean: number;
  spread: number;
  stdDev: number;
}

/** 约束违规（后端 findViolations 的输出） */
export interface Violation {
  /** exclude | pin | gap */
  kind: string;
  groupIndex: number;
  message: string;
  names: string[];
}

export interface ArrangeResponse {
  groups: Player[][];
  balance: BalanceStat;
  slots: string[];
  violations: Violation[];
}

export interface InspectResponse {
  balance: BalanceStat;
  violations: Violation[];
  slots: string[];
}

export interface ConstraintsMeta {
  excludeTogether: string[][];
  pinned: Array<{ name: string; group: number | 'last' }>;
  schedule: { start: string; intervalMin: number };
  minGap: Array<{ a: string; b: string; gap: number }>;
  minGroup: Array<{ name: string; group: number }>;
}

export interface PolishResponse extends InspectResponse {
  groups: Player[][];
}
