import { api } from '../api';
import type {
  ArrangeResponse,
  ConstraintsMeta,
  InspectResponse,
  ParseResponse,
  PlanBrief,
  PlanDetail,
  Player,
  PolishResponse,
  RosterState,
  SavedPlan,
  Strategy,
  BreakRule,
  StrengthMode,
  StrengthOrder,
  WaveMetric,
} from './types';

export interface ScheduleInput {
  start: string;
  intervalMin: number;
  /** 加长休息点（如第 3 组后隔 20 分钟） */
  breaks?: BreakRule[];
}

/** 强度位次三种形态：关闭 / 集中 / 每组一个最强 */
export const STRENGTH_MODES: Array< { value: 'off' | StrengthMode; label: string; hint: string } > = [
  { value: 'top', label: '每组一个最强', hint: '各组都有一个头牌，第 3 组的头牌是全场最强、第 4 组次之、第 1 组最弱' },
  { value: 'total', label: '集中 · 第 3 组最强', hint: '按组总音浪排位次，强者往第 3 组集中，第 1 组垫底' },
  { value: 'off', label: '不排位次', hint: '只做均衡，各组实力平均' },
];

export type { Player, Strategy, WaveMetric };

/** 赛事类型（2026-10-09 用户定名）：
 *  - 初级争霸赛小组赛（月初）、中级争霸赛小组赛（月中）：白天的分组赛，
 *    分出组数后在第 ceil(组数/2) 组结束后打一场 20 分钟复活赛（8/7 组 → 第 4 组后，6 组 → 第 3 组后）
 *  - 月终争霸赛（月末）：只分组，没有小组赛 / 复活赛 / 晋级赛流程
 *  - 初级争霸赛晋级赛 / 中级争霸赛晋级赛（晚上 21:15 起）：名单自带分数、每组一个最强
 *  值与 Rust sqlite::EVENT_TYPES 保持一致，直接用中文，避免两套映射。
 *  注意：Rust 侧另有 LEGACY_EVENT_RENAMES，把改名前的老值（初级/月底争霸赛/晋级赛 等）
 *  在原地迁到新值，新方案/前端下拉不再出现老名。 */
export const EVENT_TYPES = [
  '初级争霸赛小组赛',
  '中级争霸赛小组赛',
  '月终争霸赛',
  '初级争霸赛晋级赛',
  '中级争霸赛晋级赛',
] as const;
export type EventType = (typeof EVENT_TYPES)[number];

/** 名单自带分数时的口径文案（此时口径/天数是无效的，音浪就是名单里的分数） */
export const SCORE_LABEL = '名单自带分数';

export const STRATEGIES: Array< { value: Strategy; label: string; hint: string } > = [
  { value: 'snake', label: '蛇形均衡', hint: 'S 型折返，各组实力最平均（默认）；内置条件优先' },
  { value: 'snake-no-master', label: '蛇形 + 同门回避', hint: '蛇形基础上让同门不同组；内置条件优先' },
  { value: 'sequential', label: '梯队 · 累计音浪', hint: '按累计音浪从高到低切块，第 1 组最强；内置条件照样优先（固定位/互斥/间隔先满足）' },
  { value: 'sequential-last-n', label: '梯队 · 近 N 天', hint: '按近 N 天音浪从高到低切块；内置条件照样优先（固定位/互斥/间隔先满足）' },
  { value: 'random', label: '随机', hint: '洗牌后切组；内置条件优先' },
];

/** 梯队式策略的排序依据只有一列 wave，口径错了梯队就错了，所以强制联动。
 *  非梯队策略返回 null：保持运营自己选的口径不动。 */
export function metricOfStrategy(s: Strategy): WaveMetric | null {
  if (s === 'sequential-last-n') return 'lastN';
  if (s === 'sequential') return 'total';
  return null;
}

/** 口径下拉。last1 与「近 N 天 + 天数=1」等价，单独列一项省得每次改数字 */
export const METRICS: Array< { value: WaveMetric; label: string; hint: string } > = [
  { value: 'lastN', label: '近 N 天', hint: '最近 N 个有效业务日的音浪合计，天数可在旁边改' },
  { value: 'last1', label: '最近 1 天', hint: '只取最近一个有效业务日' },
  { value: 'month', label: '本月累计', hint: '最新数据所在自然月、从 1 号算起的合计' },
  { value: 'total', label: '累计总音浪', hint: '含基线日快照的累计总音浪' },
];

/** 口径的中文名（方案列表、导出图副标题用） */
export function metricLabel(m: string, days: number): string {
  if (m === 'lastN') return `近 ${days} 天`;
  if (m === 'last1') return '最近 1 天';
  if (m === 'month') return '本月累计';
  return '累计';
}

/** 默认强度位次：第 3 组最强、第 4 组次强，之后组号越大越弱，第 2 组次弱、第 1 组最弱。
 *  与后端 default_strength_order 保持一致。 */
export function defaultStrengthOrder(groupCount: number): number[] {
  if (groupCount < 3) return Array.from({ length: groupCount }, (_, i) => i + 1);
  const v: number[] = [];
  for (let i = 3; i <= groupCount; i += 1) v.push(i);
  v.push(2, 1);
  return v;
}

export const DEFAULT_DISPLAY = { wave: true, ranking: true, daily: true, groups: true };

export const pk = {
  parse: (body: { text: string; metric: WaveMetric; days: number; force?: boolean }) =>
    api<ParseResponse>('/api/roster/parse', { method: 'POST', body: JSON.stringify(body) }),

  getRoster: (eventType: string) =>
    api<RosterState>(`/api/roster?type=${encodeURIComponent(eventType)}`),
  saveRoster: (text: string, eventType: string) =>
    api< { ok: boolean; updatedAt: string } >('/api/roster', {
      method: 'PUT',
      body: JSON.stringify({ text, type: eventType }),
    }),

  arrange: (body: {
    players: Player[];
    groupSize: number;
    strategy: Strategy;
    constraints?: { schedule?: ScheduleInput; strengthOrder?: StrengthOrder };
  }) => api<ArrangeResponse>('/api/group/arrange', { method: 'POST', body: JSON.stringify(body) }),

  /** 拖拽后体检：重算均衡度与约束冲突 */
  inspect: (body: {
    groups: Player[][];
    constraints?: { schedule?: ScheduleInput; strengthOrder?: StrengthOrder };
  }) =>
    api<InspectResponse>('/api/group/inspect', { method: 'POST', body: JSON.stringify(body) }),

  /** 均衡微调：只接受让极差变小的交换，protected 里的人不动 */
  polish: (body: { groups: Player[][]; protected?: string[]; constraints?: { schedule?: ScheduleInput } }) =>
    api<PolishResponse>('/api/group/polish', { method: 'POST', body: JSON.stringify(body) }),

  constraints: () => api<ConstraintsMeta>('/api/group/constraints'),
  refreshCache: () => api< { ok: boolean } >('/api/cache/refresh', { method: 'POST', body: '{}' }),

  /** 别名表：把名单里认不出来的写法钉到库里真名（「识别不到」列表点候选一键修） */
  aliases: () => api< { items: Record<string, string> } >('/api/aliases'),
  saveAlias: (alias: string, realName: string) =>
    api< { ok: boolean } >('/api/aliases', {
      method: 'POST',
      body: JSON.stringify({ alias, realName }),
    }),
  removeAlias: (alias: string) =>
    api< { ok: boolean } >(`/api/aliases/${encodeURIComponent(alias)}`, { method: 'DELETE' }),

  listPlans: () => api< { items: PlanBrief[] } >('/api/plans'),
  getPlan: (id: number) => api<PlanDetail>(`/api/plans/${id}`),
  savePlan: (body: {
    title: string;
    metric: WaveMetric;
    days: number;
    eventType?: string;
    payload: SavedPlan;
  }) => api< { id: number } >('/api/plans', { method: 'POST', body: JSON.stringify(body) }),
  deletePlan: (id: number) => api< { ok: boolean } >(`/api/plans/${id}`, { method: 'DELETE' }),
  setPublished: (id: number, published: boolean) =>
    api< { ok: boolean; id: number; published: boolean } >(`/api/plans/${id}/publish`, {
      method: 'POST',
      body: JSON.stringify({ published }),
    }),

  /** 在册主播（用来一键生成名单） */
  streamers: (team?: string) =>
    api< { items: Array< { personId: number; name: string; team: string; status: string } > } >(
      '/api/admin/streamers' + (team ? `?team=${team}` : ''),
    ),
};
