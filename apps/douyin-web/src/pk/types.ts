/** PK 分组页用到的后端数据结构。
 *  字段命名与 Rust `pk-core` / `pk-server` 的 serde 输出保持一致（camelCase），
 *  分组算法只有后端一份权威实现，这里只做展示与交互。 */

/** 音浪口径：近 N 天 / 最近 1 天 / 本月累计 / 累计总音浪 */
export type WaveMetric = 'lastN' | 'last1' | 'month' | 'total';

/** 强度位次形态：
 *  - total 集中：按组总音浪排，第 3 组整体最强（强者集中）
 *  - top 每组一个最强：各组都有一个头牌，头牌之间再排位次（第 3 组头牌最强） */
export type StrengthMode = 'total' | 'top';

/** 强度位次：各组实力按「最强 → 最弱」的组号排列（1 起）。
 *  order 留空时后端按默认规则生成：第 3 组最强、第 4 组次强、第 1 组最弱。 */
export interface StrengthOrder {
  enabled: boolean;
  mode?: StrengthMode;
  order?: number[];
}

/** 加长休息点：第 afterGroup 组之后到下组开始，间隔 minutes 分钟 */
export interface BreakRule {
  afterGroup: number;
  minutes: number;
}

export type Strategy = 'snake' | 'snake-no-master' | 'sequential' | 'sequential-last-n' | 'random';

/** PK 展板的一键分组配置：整包存取（GET / PUT /api/group/board-config），
 *  字段与 GroupPage 页面上散着的配置一一对应，只是搬进了后端，展板页不再自带输入名单。 */
export interface BoardConfig {
  eventType: import('./api').EventType;
  metric: WaveMetric;
  /** 口径为 lastN 时的天数（1..30） */
  days: number;
  strategy: Strategy;
  /** 'top' | 'total' 排位次，'off' 只做均衡 */
  strengthMode: 'top' | 'total' | 'off';
  /** 每组人数（2..20） */
  groupSize: number;
  /** 开赛时间 'HH:MM' */
  startTime: string;
  /** 组间间隔分钟（1..120） */
  intervalMin: number;
  /** 第 3 组后的加长休息分钟（0..120，0 = 不插；小组赛是固定复活赛，此值不生效） */
  longBreak: number;
}

export interface Player {
  key: string;
  raw?: string;
  name: string;
  leader?: string | null;
  personId?: string | null;
  matched?: boolean;
  wave?: number;
  avatar?: string | null;
  excluded?: boolean;
}

export interface ParsedRow {
  raw: string;
  name: string;
  leader?: string | null;
  aliasApplied?: string | null;
  /** 名单自带的分数（`狼哲 71259` 格式），有值时直接当音浪用 */
  score?: number | null;
  excluded?: boolean;
}

export interface UnmatchedItem {
  row: ParsedRow;
  candidates: Array< { personId: string; name: string; avatar?: string | null } >;
}

export interface ParseResponse {
  players: Player[];
  unmatched: UnmatchedItem[];
  excluded: ParsedRow[];
  aliasMap: Record<string, string>;
  dates: string[];
  metric: WaveMetric;
  days: number;
  /** 名单里带分数的行数（> 0 表示音浪来自名单本身，如晋级赛名单） */
  scored?: number;
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

/** 约束违规（后端 find_violations 的输出） */
export interface Violation {
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

export interface PolishResponse extends InspectResponse {
  groups: Player[][];
}

export interface ConstraintsMeta {
  excludeTogether: string[][];
  pinned: Array< { name: string; group: number | 'last' } >;
  schedule: { start: string; intervalMin: number };
  minGap: Array< { a: string; b: string; gap: number } >;
  minGroup: Array< { name: string; group: number } >;
}

/** 看板上的一组：id 稳定，拖拽与渲染都靠它定位 */
export interface Bucket {
  id: string;
  name: string;
  players: Player[];
}

export interface RosterState {
  saved: boolean;
  text: string | null;
  updatedAt: string | null;
}

/** 对外展示页模块显隐，随方案落库（老方案缺省全开） */
export interface DisplayConfig {
  wave: boolean;
  ranking: boolean;
  daily: boolean;
  groups: boolean;
}

export interface SavedPlan {
  title: string;
  metric: WaveMetric;
  days: number;
  strategy?: Strategy;
  /** 分组时是否套用了强度位次（老方案缺省 = 未套用） */
  strengthOrder?: StrengthOrder;
  buckets: Array< { name: string; players: Player[] } >;
  brand?: string;
  schedule?: { start: string; intervalMin: number; breaks?: BreakRule[] };
  display?: DisplayConfig;
  /** 分组时用的数据日期（`YYYY-MM-DD` 降序，第一个是「最近有效业务日」）。
   *  2026-10-18 起随方案落库：载入老方案时用来还原导出长图头部日期；缺省 = 老方案没存。 */
  dates?: string[];
}

export interface PlanBrief {
  id: number;
  title: string;
  metric: string;
  days: number;
  /** 后端两处字段名不一致（2026-10-18 实读）：
   *  - 列表 `GET /api/plans`：`store::list_plans` 直接把 `PlanRow` 序列化出去 → **created_at**
   *  - 详情 `GET /api/plans/{id}`：`json!` 宏手写 → **createdAt**
   *  两个都认，方案列表的时间列才不会恒显示 undefined。 */
  createdAt?: string;
  created_at?: string;
  published: boolean;
  /** 赛事类型（初级/中级争霸赛小组赛、月终争霸赛、两级争霸赛晋级赛），空串 = 未分类的老方案 */
  eventType?: string;
}

export interface PlanDetail extends PlanBrief {
  payload: SavedPlan;
}
