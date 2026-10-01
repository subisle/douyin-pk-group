import type { AnchorBrief, ParsedRow, Player, WaveMetric } from '@pk/shared';
import type { ArrangeResponse, ConstraintsMeta, InspectResponse, PolishResponse } from './types.js';

const BASE = '/api';
const TOKEN_KEY = 'pk_admin_token';

/** 管理员 token 存本地；清掉即视为未登录 */
export function getToken(): string | null {
  try {
    return localStorage.getItem(TOKEN_KEY);
  } catch {
    return null;
  }
}

export function setToken(token: string | null) {
  try {
    if (token) localStorage.setItem(TOKEN_KEY, token);
    else localStorage.removeItem(TOKEN_KEY);
  } catch {
    /* 隐私模式下 localStorage 会抛，忽略即可 */
  }
}

/** token 失效时广播，由 <AuthGate> 切回登录页 */
export const UNAUTHORIZED_EVENT = 'pk:unauthorized';

async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const token = getToken();
  const res = await fetch(BASE + path, {
    ...init,
    headers: {
      'Content-Type': 'application/json',
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...(init?.headers ?? {}),
    },
  });
  if (res.status === 401) {
    setToken(null);
    window.dispatchEvent(new CustomEvent(UNAUTHORIZED_EVENT));
  }
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`${res.status} ${text}`);
  }
  return res.json() as Promise<T>;
}

/** 公开只读端点：不带 token，也不触发 401 广播（展示页给外网人看） */
async function pubReq<T>(path: string): Promise<T> {
  const res = await fetch(BASE + path);
  if (!res.ok) throw new Error(res.status === 404 ? '展示页不存在或已下架' : `${res.status}`);
  return res.json() as Promise<T>;
}

export interface UnmatchedItem {
  row: ParsedRow;
  candidates: AnchorBrief[];
}

export interface ParseResponse {
  players: Player[];
  unmatched: UnmatchedItem[];
  excluded: ParsedRow[];
  aliasMap: Record<string, string>;
  dates: string[];
  metric: WaveMetric;
  days: number;
}

export interface SourceBrief {
  id: string;
  label: string;
  note: string;
  port: number;
  database: string;
  excludeBaselineDay: boolean;
}

export interface SourceList {
  active: string;
  activeLabel: string;
  items: SourceBrief[];
}

/** 当前名单的持久化状态。`saved: false` = 库里还没存过，前端保留 DEFAULT_ROSTER */
export interface RosterState {
  saved: boolean;
  text: string | null;
  updatedAt: string | null;
}

export interface PlanBrief {
  id: number;
  title: string;
  metric: string;
  days: number;
  payload: string;
  created_at: string;
  /** 是否已发布到对外展示页 */
  published: boolean;
}

/** 方案落库结构：直接存完整 Player，读取即可还原看板（含拖拽所需的 key） */
export interface SavedPlan {
  title: string;
  metric: WaveMetric;
  days: number;
  buckets: Array<{ name: string; players: Player[] }>;
  /** 对外展示页用的可选信息（老方案没有也能打开） */
  brand?: string;
  schedule?: { start: string; intervalMin: number };
  /** 展示页模块显隐（老方案缺省全开） */
  display?: DisplayConfig;
}

export interface PlanDetail {
  id: number;
  title: string;
  metric: string;
  days: number;
  payload: SavedPlan;
  created_at: string;
  published: boolean;
}

/* ---------------- 对外展示页的数据结构（后端 public.rs） ---------------- */

/** 展示页模块显隐（管理端在「配置」里设置，随方案落库；老方案缺省全开） */
export interface DisplayConfig {
  /** 音浪数值：关掉后对外不出现任何音浪数字 */
  wave: boolean;
  /** 音浪排行榜 */
  ranking: boolean;
  /** 每日音浪（按日合计条目，无曲线图） */
  daily: boolean;
  /** PK 分组 */
  groups: boolean;
}

export const DEFAULT_DISPLAY: DisplayConfig = { wave: true, ranking: true, daily: true, groups: true };

export interface PublicPlayer {
  name: string;
  leader?: string;
  avatar?: string | null;
  wave: number;
  /** 全场音浪名次 */
  rank: number;
  /** 组内音浪名次 */
  group_rank: number;
}

export interface PublicGroup {
  name: string;
  /** 开赛时间，如 "08:15" */
  slot: string;
  count: number;
  /** 组内音浪合计 */
  total: number;
  players: PublicPlayer[];
  /** true = 前端内置的空组占位，后端没有这组数据 */
  builtin?: boolean;
}

/** 曲线图数据：每组一条每日音浪曲线（后端算好的，前端只负责画） */
export interface PublicTrend {
  /** 采集日期，正序（老 → 新），与每条线的 values 下标一一对应 */
  dates: string[];
  series: Array<{ name: string; values: number[] }>;
}

/** 按日期明细：每组每天、每人每天的音浪与开播状态 */
export interface PublicDaily {
  /** 采集日期，正序（老 → 新） */
  dates: string[];
  groups: Array<{
    name: string;
    /** 该组每日合计，与 trend.series 同源 */
    values: number[];
    players: Array<{
      name: string;
      leader?: string;
      values: number[];
      /** 每天是否开播（daily_metric.is_live），与 values 下标一一对应；1=开播 0=未开播 */
      live?: number[];
    }>;
  }>;
}

export interface PublicPlan {
  id: number;
  title: string;
  brand: string;
  metric: WaveMetric;
  days: number;
  schedule: { start: string; intervalMin: number };
  /** 展示页模块显隐（老方案缺省全开） */
  display?: DisplayConfig;
  /** true = 最新采集日就是今天（比赛日）；PK 分组页只在比赛日展示 */
  matchDay?: boolean;
  createdAt: string;
  /** 参与统计的采集日期（倒序） */
  dates: string[];
  /** true = 实时音浪取失败，当前是方案保存时的快照 */
  stale: boolean;
  counts: { groups: number; players: number; totalWave: number };
  groups: PublicGroup[];
  ranking: PublicPlayer[];
  /** 每日音浪曲线；取不到时后端省略该字段 */
  trend?: PublicTrend | null;
  /** 按年月日切换用的每日明细；取不到时后端省略该字段 */
  daily?: PublicDaily | null;
}

export const api = {
  health: () => req<{ server: string; db: { ok: boolean; version?: string; error?: string } }>('/health'),

  sources: () => req<SourceList>('/sources'),
  setSource: (id: string) => req<{ active: string }>('/sources/active', { method: 'POST', body: JSON.stringify({ id }) }),
  testSource: (id: string) =>
    req<{ id: string; ok: boolean; version?: string; error?: string; latestDate: string | null; anchorCount: number }>(
      `/sources/${id}/test`,
      { method: 'POST', body: '{}' },
    ),
  meta: () =>
    req<{
      dates: string[];
      latestDate: string | null;
      baselineDate: string | null;
      anchorCount: number;
      aliases: Record<string, string>;
      sourceId: string;
      sourceLabel: string;
    }>('/meta'),

  parse: (body: { text: string; metric: WaveMetric; days: number; aliases?: Record<string, string>; force?: boolean }) =>
    req<ParseResponse>('/roster/parse', { method: 'POST', body: JSON.stringify(body) }),

  /** 名单持久化：改完自动存，刷新页面不用重新粘 */
  getRoster: () => req<RosterState>('/roster'),
  saveRoster: (text: string) => req<{ ok: boolean; updatedAt: string }>('/roster', { method: 'PUT', body: JSON.stringify({ text }) }),

  waves: (body: { metric: WaveMetric; days: number; personIds?: string[]; force?: boolean }) =>
    req<{ metric: WaveMetric; days: number; dates: string[]; waves: Record<string, number> }>('/waves', {
      method: 'POST',
      body: JSON.stringify(body),
    }),

  anchors: (q: string) => req<{ items: AnchorBrief[] }>(`/anchors?q=${encodeURIComponent(q)}`),

  refreshCache: () => req<{ ok: boolean }>('/cache/refresh', { method: 'POST', body: '{}' }),

  /* ---------------- 分组算法（权威实现 = Rust pk-core） ---------------- */
  arrange: (body: {
    players: Player[];
    groupSize: number;
    strategy: string;
    constraints?: { schedule?: { start: string; intervalMin: number } };
  }) => req<ArrangeResponse>('/group/arrange', { method: 'POST', body: JSON.stringify(body) }),

  inspect: (body: {
    groups: Player[][];
    constraints?: { schedule?: { start: string; intervalMin: number } };
  }) => req<InspectResponse>('/group/inspect', { method: 'POST', body: JSON.stringify(body) }),

  polish: (body: {
    groups: Player[][];
    protected?: string[];
    constraints?: { schedule?: { start: string; intervalMin: number } };
  }) => req<PolishResponse>('/group/polish', { method: 'POST', body: JSON.stringify(body) }),

  constraints: () => req<ConstraintsMeta>('/group/constraints'),

  /* ---------------- 鉴权 ---------------- */
  session: () => req<{ enabled: boolean; ok: boolean }>('/auth/session'),
  login: (password: string) => req<{ token: string; enabled: boolean }>('/auth/login', {
    method: 'POST',
    body: JSON.stringify({ password }),
  }),
  logout: () => req<{ ok: boolean }>('/auth/logout', { method: 'POST', body: '{}' }),

  listPlans: () => req<{ items: PlanBrief[] }>('/plans'),
  getPlan: (id: number) => req<PlanDetail>(`/plans/${id}`),
  savePlan: (body: { title: string; metric: string; days: number; payload: SavedPlan }) =>
    req<{ id: number }>('/plans', { method: 'POST', body: JSON.stringify(body) }),
  deletePlan: (id: number) => req<{ ok: boolean }>(`/plans/${id}`, { method: 'DELETE' }),

  /** 上架 / 下架对外展示页；上架后 /s/:id 才可访问 */
  setPublished: (id: number, published: boolean) =>
    req<{ ok: boolean; id: number; published: boolean }>(`/plans/${id}/publish`, {
      method: 'POST',
      body: JSON.stringify({ published }),
    }),

  /** 公开只读：不需要管理员 token */
  publicPlans: () => pubReq<{ items: Array<{ id: number; title: string }> }>('/public/plans'),
  publicPlan: (id: number) => pubReq<PublicPlan>(`/public/plans/${id}`),
};
