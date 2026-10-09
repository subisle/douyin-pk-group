import type { ConstraintSet } from './constraints.js';

export type WaveMetric = 'lastN' | 'total';

export interface Player {
  key: string;
  raw: string;
  name: string;
  leader?: string;
  /** 库里的主播 id。不同数据源主键类型不同（A 库是 bigint，B 库是 int），统一按字符串处理 */
  personId: string | null;
  matched: boolean;
  wave: number;
  avatar?: string | null;
  excluded: boolean;
}

export interface ParsedRow {
  raw: string;
  name: string;
  leader?: string;
  aliasApplied?: string;
  excluded?: boolean;
}

export interface GroupStat {
  index: number;
  count: number;
  total: number;
  avg: number;
}

export interface BalanceStat {
  groups: GroupStat[];
  max: number;
  min: number;
  mean: number;
  spread: number;
  stdDev: number;
}

export interface GroupOptions {
  groupSize: number;
  strategy: 'snake' | 'random' | 'snake-no-master' | 'sequential' | 'sequential-last-n';
  /** 内置条件：互斥 / 固定位 / 开赛时间。缺省用 DEFAULT_CONSTRAINTS */
  constraints?: ConstraintSet;
}

export interface MatchResult {
  rows: ParsedRow[];
  matched: Array<{ row: ParsedRow; personId: number; name: string; avatar?: string | null }>;
  unmatched: ParsedRow[];
  excluded: ParsedRow[];
}

export interface AnchorBrief {
  personId: string;
  name: string;
  avatar?: string | null;
}
