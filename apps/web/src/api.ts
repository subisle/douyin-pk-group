import type { AnchorBrief, ParsedRow, Player, WaveMetric } from '@pk/shared';
import type { ArrangeResponse, ConstraintsMeta, InspectResponse, PolishResponse } from './types.js';

const BASE = '/api';

async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(BASE + path, {
    ...init,
    headers: { 'Content-Type': 'application/json', ...(init?.headers ?? {}) },
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`${res.status} ${text}`);
  }
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

export interface PlanBrief {
  id: number;
  title: string;
  metric: string;
  days: number;
  payload: string;
  created_at: string;
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
}

export interface PlanDetail {
  id: number;
  title: string;
  metric: string;
  days: number;
  payload: SavedPlan;
  created_at: string;
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

  listPlans: () => req<{ items: PlanBrief[] }>('/plans'),
  getPlan: (id: number) => req<PlanDetail>(`/plans/${id}`),
  savePlan: (body: { title: string; metric: string; days: number; payload: SavedPlan }) =>
    req<{ id: number }>('/plans', { method: 'POST', body: JSON.stringify(body) }),
  deletePlan: (id: number) => req<{ ok: boolean }>(`/plans/${id}`, { method: 'DELETE' }),
};
