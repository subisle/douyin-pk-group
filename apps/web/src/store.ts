import { create } from 'zustand';
import {
  DEFAULT_CONSTRAINTS,
  DEFAULT_ROSTER,
  autoGroup,
  findViolations,
  type ConstraintSet,
  type GroupOptions,
  type ParsedRow,
  type Player,
  type WaveMetric,
} from '@pk/shared';
import { api, type PlanBrief, type SourceBrief, type SourceList, type UnmatchedItem } from './api.js';
import type { Bucket } from './types.js';

export type Strategy = GroupOptions['strategy'];
export type View = 'board' | 'roster';
export type SourceId = string;

let seq = 0;
const bucketId = () => `b${++seq}`;

export function toBuckets(groups: Player[][]): Bucket[] {
  return groups.map((players, i) => ({ id: bucketId(), name: `第 ${i + 1} 组`, players }));
}

export function toPlayerGroups(buckets: Bucket[]): Player[][] {
  return buckets.map((b) => b.players);
}

export interface PlansState {
  open: boolean;
  items: PlanBrief[];
  loading: boolean;
}

export type Patchable = Pick<
  State,
  'text' | 'metric' | 'days' | 'groupSize' | 'strategy' | 'title' | 'brand' | 'startTime' | 'intervalMin'
>;

interface State {
  view: View;
  configOpen: boolean;

  /* 配置 */
  text: string;
  metric: WaveMetric;
  days: number;
  groupSize: number;
  strategy: Strategy;
  title: string;
  /** 导出长图顶部的公会 / 品牌名 */
  brand: string;
  /** 第 1 组开赛时间 */
  startTime: string;
  /** 组间间隔（分钟） */
  intervalMin: number;

  /* 数据 */
  sourceId: string;
  sourceLabel: string;
  sources: SourceBrief[];
  players: Player[];
  buckets: Bucket[];
  unmatched: UnmatchedItem[];
  excluded: ParsedRow[];
  aliasMap: Record<string, string>;
  dates: string[];
  latestDate: string | null;
  /** 首次采集日：记的是累计音浪，统计时剔除 */
  baselineDate: string | null;
  anchorCount: number;

  /* 运行状态 */
  dbOk: boolean | null;
  loading: boolean;
  error: string;
  toast: string | null;
  plans: PlansState;

  /* 动作 */
  setView: (v: View) => void;
  patch: (cfg: Partial<Patchable>) => void;
  notify: (msg: string | null) => void;
  /** 内置条件（互斥 / 固定位写死，开赛时间可配） */
  constraints: () => ConstraintSet;

  init: () => Promise<void>;
  switchSource: (id: string) => Promise<void>;
  parse: (force?: boolean) => Promise<void>;
  regroup: () => void;
  addBucket: () => void;
  removeBucket: (id: string) => void;
  renameBucket: (id: string, name: string) => void;
  moveCard: (activeKey: string, toBucketId: string, overKey: string | null) => void;
  /** 增补人员：把新主播加进已定稿的组（不重排其他人），返回是否成功 */
  addToBucket: (bucketId: string, name: string) => Promise<boolean>;
  bind: (rowName: string, personId: string, anchorName: string) => Promise<void>;
  setManualWave: (rowName: string, wave: number) => void;
  savePlanNow: () => Promise<void>;
  openPlans: () => Promise<void>;
  closePlans: () => void;
  loadPlan: (id: number) => Promise<void>;
  removeSavedPlan: (id: number) => Promise<void>;
}

let toastTimer: ReturnType<typeof setTimeout> | null = null;

export const useStore = create<State>((set, get) => ({
  view: 'board',
  configOpen: false,

  text: DEFAULT_ROSTER,
  metric: 'total', // 排名按累计总音浪（与抖音榜单一致），近N天增量口径可手动切换
  days: 3,
  groupSize: 8,
  strategy: 'snake',
  title: 'PK 挑战赛分组',
  brand: 'ST-001',
  startTime: DEFAULT_CONSTRAINTS.schedule.start,
  intervalMin: DEFAULT_CONSTRAINTS.schedule.intervalMin,

  sourceId: '',
  sourceLabel: '',
  sources: [],
  players: [],
  buckets: [],
  unmatched: [],
  excluded: [],
  aliasMap: {},
  dates: [],
  latestDate: null,
  baselineDate: null,
  anchorCount: 0,

  dbOk: null,
  loading: false,
  error: '',
  toast: null,
  plans: { open: false, items: [], loading: false },

  setView: (v) => set({ view: v }),
  patch: (cfg) => set(cfg),

  constraints: () => {
    const { startTime, intervalMin } = get();
    return { ...DEFAULT_CONSTRAINTS, schedule: { start: startTime, intervalMin } };
  },

  notify: (msg) => {
    if (toastTimer) clearTimeout(toastTimer);
    set({ toast: msg });
    if (msg) {
      toastTimer = setTimeout(() => set({ toast: null }), 2200);
    }
  },

  init: async () => {
    try {
      const s = await api.sources();
      set({ sources: s.items, sourceId: s.active });
    } catch {
      /* 老后端没有 /api/sources，忽略 */
    }
    try {
      const h = await api.health();
      set({ dbOk: h.db.ok });
    } catch {
      set({ dbOk: false });
    }
    try {
      const m = await api.meta();
      set({
        dates: m.dates,
        latestDate: m.latestDate,
        baselineDate: m.baselineDate,
        anchorCount: m.anchorCount,
        aliasMap: m.aliases,
        sourceId: m.sourceId ?? get().sourceId,
      });
    } catch {
      /* meta 失败不阻塞，等点「解析并分组」时再报错 */
    }
  },

  switchSource: async (id) => {
    const prev = get().sourceId;
    if (id === prev) return;
    set({ sourceId: id, dbOk: null, loading: true, error: '' });
    try {
      await api.setSource(id);
      const m = await api.meta();
      set({
        dates: m.dates,
        latestDate: m.latestDate,
        baselineDate: m.baselineDate,
        anchorCount: m.anchorCount,
        aliasMap: m.aliases,
        sourceId: m.sourceId ?? id,
        sourceLabel: m.sourceLabel,
        dbOk: true,
      });
      await get().parse(true);
      const cur = get().sources.find((s) => s.id === id);
      get().notify(`已切到「${cur?.label ?? id}」`);
    } catch (e) {
      set({ dbOk: false, sourceId: prev, loading: false, error: `切换数据源失败：${(e as Error).message}` });
    }
  },

  parse: async (force = false) => {
    set({ loading: true, error: '' });
    try {
      const res = await api.parse({ text: get().text, metric: get().metric, days: get().days, force });
      const active = res.players.filter((p) => !p.excluded);
      const groups = active.length
        ? autoGroup(res.players, { groupSize: get().groupSize, strategy: get().strategy, constraints: get().constraints() })
        : [];
      set({
        players: res.players,
        unmatched: res.unmatched,
        excluded: res.excluded,
        aliasMap: res.aliasMap,
        dates: res.dates,
        buckets: toBuckets(groups),
        view: 'board',
      });
      get().notify(`已解析 ${active.length} 人，分成 ${groups.length} 组`);
    } catch (e) {
      set({ error: (e as Error).message });
    } finally {
      set({ loading: false });
    }
  },

  regroup: () => {
    const { players, groupSize, strategy } = get();
    if (!players.length) return;
    const buckets = toBuckets(autoGroup(players, { groupSize, strategy, constraints: get().constraints() }));
    set({ buckets });
    const bad = findViolations(buckets.map((b) => b.players), get().constraints());
    get().notify(bad.length ? `已重新分组，仍有 ${bad.length} 条约束冲突` : '已按当前策略重新分组');
  },

  addBucket: () => {
    set((s) => ({ buckets: [...s.buckets, { id: bucketId(), name: `第 ${s.buckets.length + 1} 组`, players: [] }] }));
  },

  removeBucket: (id) => {
    const target = get().buckets.find((b) => b.id === id);
    if (!target) return;
    if (target.players.length) {
      get().notify('该组还有成员，请先移出');
      return;
    }
    set((s) => ({ buckets: s.buckets.filter((b) => b.id !== id) }));
  },

  renameBucket: (id, name) => {
    set((s) => ({ buckets: s.buckets.map((b) => (b.id === id ? { ...b, name } : b)) }));
  },

  moveCard: (activeKey, toBucketId, overKey) => {
    set((s) => {
      const next = s.buckets.map((b) => ({ ...b, players: [...b.players] }));
      const from = next.find((b) => b.players.some((p) => p.key === activeKey));
      const to = next.find((b) => b.id === toBucketId);
      if (!from || !to) return s;

      if (from.id === to.id) {
        const arr = from.players;
        const oldIndex = arr.findIndex((p) => p.key === activeKey);
        const newIndex = overKey ? arr.findIndex((p) => p.key === overKey) : arr.length - 1;
        if (oldIndex < 0 || newIndex < 0 || oldIndex === newIndex) return s;
        const [p] = arr.splice(oldIndex, 1);
        arr.splice(newIndex, 0, p);
      } else {
        const idx = from.players.findIndex((p) => p.key === activeKey);
        if (idx < 0) return s;
        const [p] = from.players.splice(idx, 1);
        const at = overKey ? Math.max(0, to.players.findIndex((x) => x.key === overKey)) : to.players.length;
        to.players.splice(at, 0, p);
      }
      return { buckets: next };
    });
  },

  /** 增补人员：只解析这一个名字（带音浪），插到指定组末尾，不动其他人 */
  addToBucket: async (bucketId, name) => {
    const trimmed = name.trim();
    if (!trimmed) return false;
    const norm = (s: string) => s.replace(/[\s\u3000]/g, '');
    const dup = get().buckets.some((b) => b.players.some((p) => norm(p.name) === norm(trimmed)));
    if (dup) {
      get().notify(`${trimmed} 已经在分组里了`);
      return false;
    }
    try {
      const res = await api.parse({ text: trimmed, metric: get().metric, days: get().days });
      const hit = res.players.find((p) => !p.excluded);
      if (!hit) {
        get().notify(`名单里没有匹配到「${trimmed}」，请检查名字或先绑定账号`);
        return false;
      }
      set((s) => ({
        buckets: s.buckets.map((b) =>
          b.id === bucketId ? { ...b, players: [...b.players, hit] } : b,
        ),
      }));
      const bad = findViolations(get().buckets.map((b) => b.players), get().constraints());
      const mine = bad.filter((v) => v.names.includes(hit.name));
      const unmatchedHint = hit.matched ? '' : '（未匹配到账号，音浪按 0 计，可到「名单」页绑定）';
      get().notify(
        mine.length
          ? `已把 ${hit.name} 加入，但有约束冲突：${mine[0].message}`
          : `已把 ${hit.name} 增补进该组${unmatchedHint}`,
      );
      return true;
    } catch (e) {
      get().notify(`增补失败：${(e as Error).message}`);
      return false;
    }
  },

  bind: async (rowName, personId, anchorName) => {
    const { metric, days, players, buckets, unmatched } = get();
    const w = await api.waves({ metric, days, personIds: [personId] });
    const wave = w.waves[String(personId)] ?? 0;
    const patch = (p: Player) => (p.name === rowName ? { ...p, personId, matched: true, wave, name: anchorName } : p);
    set({
      players: players.map(patch),
      buckets: buckets.map((b) => ({ ...b, players: b.players.map(patch) })),
      unmatched: unmatched.filter((u) => u.row.name !== rowName),
    });
    get().notify(`已绑定 ${rowName} → ${anchorName}`);
  },

  setManualWave: (rowName, wave) => {
    const patch = (p: Player) => (p.name === rowName ? { ...p, wave } : p);
    set((s) => ({
      players: s.players.map(patch),
      buckets: s.buckets.map((b) => ({ ...b, players: b.players.map(patch) })),
      unmatched: s.unmatched.filter((u) => u.row.name !== rowName),
      toast: `已手动设置 ${rowName} 的音浪`,
    }));
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(() => set({ toast: null }), 2200);
  },

  savePlanNow: async () => {
    const { title, metric, days, buckets, notify } = get();
    if (!buckets.length) {
      notify('还没有分组结果');
      return;
    }
    try {
      await api.savePlan({
        title,
        metric,
        days,
        payload: {
          title,
          metric,
          days,
          buckets: buckets.map((b) => ({ name: b.name, players: b.players })),
        },
      });
      notify('方案已保存');
    } catch (e) {
      notify(`保存失败：${(e as Error).message}`);
    }
  },

  openPlans: async () => {
    set({ plans: { open: true, items: [], loading: true } });
    try {
      const { items } = await api.listPlans();
      set({ plans: { open: true, items, loading: false } });
    } catch (e) {
      set({ plans: { open: true, items: [], loading: false } });
      get().notify(`读取失败：${(e as Error).message}`);
    }
  },

  closePlans: () => set((s) => ({ plans: { ...s.plans, open: false } })),

  loadPlan: async (id) => {
    try {
      const p = await api.getPlan(id);
      set({
        title: p.payload.title || p.title,
        metric: p.payload.metric ?? 'lastN',
        days: p.payload.days ?? 3,
        buckets: (p.payload.buckets ?? []).map((b) => ({
          id: bucketId(),
          name: b.name,
          players: b.players ?? [],
        })),
        view: 'board',
        plans: { ...get().plans, open: false },
      });
      get().notify('方案已载入');
    } catch (e) {
      get().notify(`载入失败：${(e as Error).message}`);
    }
  },

  removeSavedPlan: async (id) => {
    await api.deletePlan(id);
    set((s) => ({ plans: { ...s.plans, items: s.plans.items.filter((p) => p.id !== id) } }));
    get().notify('方案已删除');
  },
}));
