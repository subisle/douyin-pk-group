import { create } from 'zustand';
import {
  DEFAULT_CONSTRAINTS,
  DEFAULT_ROSTER,
  type ConstraintSet,
  type GroupOptions,
  type ParsedRow,
  type Player,
  type WaveMetric,
} from '@pk/shared';
import {
  api,
  setToken,
  DEFAULT_DISPLAY,
  UNAUTHORIZED_EVENT,
  type DisplayConfig,
  type PlanBrief,
  type SourceBrief,
  type SourceList,
  type UnmatchedItem,
} from './api.js';
import type { BalanceStat, Bucket, Violation } from './types.js';

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

/** 更新展示页模块显隐（部分字段） */
export type DisplayPatch = Partial<DisplayConfig>;

/** 一份看板快照，用于撤销/重做 */
interface Snapshot {
  buckets: Bucket[];
  unassigned: Player[];
}

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
  /** 对外展示页模块显隐（保存方案时随 payload 落库） */
  display: DisplayConfig;
  /** 第 1 组开赛时间 */
  startTime: string;
  /** 组间间隔（分钟） */
  intervalMin: number;
  /** 名单正在防抖写入后端（UI 显示「保存中」） */
  rosterSaving: boolean;
  /** 最近一次成功落库的名单时间（RFC3339）；null = 还没存过 */
  rosterSavedAt: string | null;

  /* 数据 */
  sourceId: string;
  sourceLabel: string;
  sources: SourceBrief[];
  players: Player[];
  buckets: Bucket[];
  /** 还没分到任何组的人（分组后中途加入 / 从组里移出） */
  unassigned: Player[];
  /** 锁住的人：自动分组 / 均衡微调都不会动他们 */
  locked: string[];
  unmatched: UnmatchedItem[];
  excluded: ParsedRow[];
  aliasMap: Record<string, string>;
  dates: string[];
  latestDate: string | null;
  /** 首次采集日：记的是累计音浪 */
  baselineDate: string | null;
  anchorCount: number;

  /* 内置条件：从 /api/group/constraints 拉的权威副本，null 表示还没拿到 */
  constraintSet: ConstraintSet | null;

  /* 后端计算结果（前端不再自己算） */
  balance: BalanceStat | null;
  violations: Violation[];
  slots: string[];

  /* 撤销 / 重做 */
  past: Snapshot[];
  future: Snapshot[];
  /**
   * 拖拽事务的起点快照。onDragOver 每移动一格就会触发一次落位，
   * 逐次 pushHistory 会把一次拖拽拆成几十条撤销记录（撤销一次没反应）。
   * 所以拖拽期间只在 beginDrag 抓一次快照，endDrag 时确有变化才记一条。
   */
  dragSnapshot: Snapshot | null;

  /* 运行状态 */
  dbOk: boolean | null;
  loading: boolean;
  error: string;
  toast: string | null;
  plans: PlansState;
  /** 最近一次保存出来的方案 id，分享链接就是它 */
  lastPlanId: number | null;

  /* 动作 */
  setView: (v: View) => void;
  patch: (cfg: Partial<Patchable>) => void;
  setDisplay: (cfg: DisplayPatch) => void;
  notify: (msg: string | null) => void;
  constraints: () => ConstraintSet;
  /** 只把开赛时间发给后端，互斥/固定位/间隔由后端内置条件权威决定 */
  scheduleInput: () => { schedule: { start: string; intervalMin: number } };

  init: () => Promise<void>;
  switchSource: (id: string) => Promise<void>;
  parse: (force?: boolean) => Promise<void>;
  regroup: () => Promise<void>;
  refreshDerived: () => Promise<void>;
  polish: () => Promise<void>;

  addBucket: () => void;
  /** 把某组移动到指定组号（1 起），不影响组成员 */
  moveBucketTo: (id: string, groupNumber: number) => void;
  /** 拖动组列：把 id 组挪到 overId 组当前所在的位置 */
  moveBucketOver: (id: string, overId: string) => void;
  /** 每组内部按音浪从高到低重排（不跨组移动） */
  sortBucketsByWave: () => void;
  removeBucket: (id: string) => void;
  renameBucket: (id: string, name: string) => void;
  moveCard: (activeKey: string, toBucketId: string, overKey: string | null) => void;
  /** 拖拽事务：beginDrag 抓起点快照，endDrag 时确有变化才记一条撤销 */
  beginDrag: () => void;
  endDrag: () => void;
  /** 点选移动（移动端友好）：把某人放进某组末尾 */
  assignPlayer: (playerKey: string, toBucketId: string | null) => void;
  /** 交换两个人所在的组（跨组或同组） */
  swapPlayers: (keyA: string, keyB: string) => void;
  /** 锁定 / 解锁某人，锁定后不参与自动分组与均衡 */
  toggleLock: (playerKey: string) => void;
  /** 从组里移出到「未分组」区 */
  unassign: (playerKey: string) => void;
  /** 把未分组的人放回指定组 */
  assignFromPool: (playerKey: string, toBucketId: string) => void;

  undo: () => void;
  redo: () => void;

  /** 增补人员：把新主播加进已定稿的组（不重排其他人） */
  addToBucket: (bucketId: string, name: string) => Promise<boolean>;
  makeShareLink: () => Promise<string | null>;
  /** 上架 / 下架某个方案的对外展示页 */
  setPublished: (id: number, published: boolean) => Promise<void>;
  logout: () => Promise<void>;
  bind: (rowName: string, personId: string, anchorName: string) => Promise<void>;
  setManualWave: (rowName: string, wave: number) => void;
  savePlanNow: () => Promise<void>;
  openPlans: () => Promise<void>;
  closePlans: () => void;
  loadPlan: (id: number) => Promise<void>;
  removeSavedPlan: (id: number) => Promise<void>;
}

let toastTimer: ReturnType<typeof setTimeout> | null = null;
let inspectTimer: ReturnType<typeof setTimeout> | null = null;
let rosterTimer: ReturnType<typeof setTimeout> | null = null;
/** 名单保存的序号：只认最后一次发出的请求，挡掉慢响应把新状态盖掉 */
let rosterSeq = 0;

/**
 * 名单自动落库的防抖间隔。名单是整份 PUT 覆盖写，每敲一个字打一次后端
 * 纯属浪费，而且中途的半截名单会反复覆盖掉库里已经完整的名单。
 */
const ROSTER_SAVE_DEBOUNCE = 800;

const normalizePlayerName = (name: string) => name.replace(/[\s\u3000]/g, '');

const MAX_HISTORY = 50;

const cloneBuckets = (buckets: Bucket[]): Bucket[] =>
  buckets.map((b) => ({ id: b.id, name: b.name, players: [...b.players] }));

const snapshotOf = (buckets: Bucket[], unassigned: Player[]): Snapshot => ({
  buckets: cloneBuckets(buckets),
  unassigned: [...unassigned],
});

/**
 * 只比「谁在第几组的第几位」，不比音浪等易变字段——拖拽只关心版式有没有动过。
 * 没有这一步，用户把卡片在原位松手也会留下一条空撤销。
 */
function sameLayout(a: Snapshot, b: Snapshot): boolean {
  if (a.buckets.length !== b.buckets.length) return false;
  for (let i = 0; i < a.buckets.length; i++) {
    if (a.buckets[i].id !== b.buckets[i].id) return false;
    const x = a.buckets[i].players;
    const y = b.buckets[i].players;
    if (x.length !== y.length) return false;
    for (let j = 0; j < x.length; j++) if (x[j].key !== y[j].key) return false;
  }
  return a.unassigned.length === b.unassigned.length
    && a.unassigned.every((p, i) => p.key === b.unassigned[i].key);
}

export const useStore = create<State>((set, get) => {
  /** 记一次快照（调用前先拿当前状态） */
  const pushHistory = (snap: Snapshot = snapshotOf(get().buckets, get().unassigned)) => {
    const next = [...get().past, snap];
    if (next.length > MAX_HISTORY) next.shift();
    set({ past: next, future: [] });
  };

  /**
   * 拖拽中的落位不再各自记历史：整段拖拽由 beginDrag/endDrag 合成一条。
   * 非拖拽来源（菜单移动、增删组）仍旧逐次记。
   */
  const pushHistoryUnlessDragging = () => {
    if (!get().dragSnapshot) pushHistory();
  };

  /**
   * 把名单整份存回后端。空 / 全空白一律不存：名单是手抄出来的，
   * 误清一次就再也找不回来，宁可让页面停在空白状态等用户自己改回来。
   */
  const persistRoster = async (text: string) => {
    if (!text.trim()) {
      set({ rosterSaving: false });
      return;
    }
    const seq = ++rosterSeq;
    set({ rosterSaving: true });
    try {
      const r = await api.saveRoster(text);
      if (seq !== rosterSeq) return;
      set({ rosterSaving: false, rosterSavedAt: r.updatedAt });
    } catch (e) {
      if (seq !== rosterSeq) return;
      set({ rosterSaving: false });
      get().notify(`名单保存失败：${(e as Error).message}`);
    }
  };

  const scheduleRosterSave = (text: string) => {
    if (rosterTimer) clearTimeout(rosterTimer);
    // 输入被清空时要把待发的那次也撤掉：不然清空前敲下的半截名单
    // 会在防抖窗口结束后照样写进库里
    if (!text.trim()) {
      rosterTimer = null;
      set({ rosterSaving: false });
      return;
    }
    set({ rosterSaving: true });
    rosterTimer = setTimeout(() => {
      rosterTimer = null;
      void persistRoster(text);
    }, ROSTER_SAVE_DEBOUNCE);
  };

  return {
    view: 'board',
    configOpen: false,

    text: DEFAULT_ROSTER,
    metric: 'total',
    days: 3,
    groupSize: 8,
    strategy: 'snake',
    title: 'PK 挑战赛分组',
    brand: 'ST-001',
    display: DEFAULT_DISPLAY,
    startTime: DEFAULT_CONSTRAINTS.schedule.start,
    intervalMin: DEFAULT_CONSTRAINTS.schedule.intervalMin,
    rosterSaving: false,
    rosterSavedAt: null,

    sourceId: '',
    sourceLabel: '',
    sources: [],
    players: [],
    buckets: [],
    unassigned: [],
    locked: [],
    unmatched: [],
    excluded: [],
    aliasMap: {},
    dates: [],
    latestDate: null,
    baselineDate: null,
    anchorCount: 0,

    balance: null,
    violations: [],
    slots: [],

    past: [],
    future: [],
    dragSnapshot: null,

    dbOk: null,
    loading: false,
    error: '',
    toast: null,
    plans: { open: false, items: [], loading: false },
    lastPlanId: null,
    constraintSet: null,

    setView: (v) => set({ view: v }),
    // 名单是唯一会自动落库的字段：走 patch 的入口统一在这里挂钩，
    // 免得别处 set({ text }) 绕过保存逻辑
    patch: (cfg) => {
      set(cfg);
      if (cfg.text !== undefined) scheduleRosterSave(cfg.text);
    },

    setDisplay: (cfg) => set((s) => ({ display: { ...s.display, ...cfg } })),

    constraints: () => {
      const { startTime, intervalMin, constraintSet } = get();
      // 权威副本在 Rust（/api/group/constraints）。这里的 DEFAULT_CONSTRAINTS
      // 只是后端连不上时的兜底，两边不会各改各的。
      return { ...(constraintSet ?? DEFAULT_CONSTRAINTS), schedule: { start: startTime, intervalMin } };
    },
    scheduleInput: () => {
      const { startTime, intervalMin } = get();
      return { schedule: { start: startTime, intervalMin } };
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
      // 内置条件（互斥/固定位/最小间隔）以后端为准，前端只用来做展示高亮
      try {
        set({ constraintSet: await api.constraints() });
      } catch {
        /* 拉不到就继续用本地兜底常量 */
      }
      // 名单落库：存过就用存的，没存过（或老后端没这个接口）就保留 DEFAULT_ROSTER
      try {
        const r = await api.getRoster();
        // 只在用户还没动过 textarea 时覆盖，否则会把刚输入的名单冲掉
        if (r.saved && r.text && get().text === DEFAULT_ROSTER) {
          set({ text: r.text, rosterSavedAt: r.updatedAt });
        }
      } catch {
        /* 老后端没有 /api/roster，保持 DEFAULT_ROSTER */
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
        if (!active.length) {
          set({ players: res.players, buckets: [], unassigned: [], balance: null, violations: [], slots: [] });
            get().notify('名单里没有可参赛的人');
          return;
        }
        const seenNames = new Map<string, string>();
        const duplicateNames = new Set<string>();
        for (const player of active) {
          const key = normalizePlayerName(player.name);
          if (seenNames.has(key)) {
            duplicateNames.add(player.name);
          } else {
            seenNames.set(key, player.name);
          }
        }
        if (duplicateNames.size) {
          const names = [...duplicateNames].join('、');
          set({
            error: `名单存在重复人名：${names}`,
            players: res.players,
            unmatched: res.unmatched,
            excluded: res.excluded,
            aliasMap: res.aliasMap,
            dates: res.dates,
            buckets: [],
            unassigned: [],
            balance: null,
            violations: [],
            slots: [],
          });
          useStore.getState().notify(`存在重复人名：${names}`);
          return;
        }
        const arranged = await api.arrange({
          players: res.players,
          groupSize: get().groupSize,
          strategy: get().strategy,
          constraints: get().scheduleInput(),
        });
        set({
          players: res.players,
          unmatched: res.unmatched,
          excluded: res.excluded,
          aliasMap: res.aliasMap,
          dates: res.dates,
          buckets: toBuckets(arranged.groups),
          unassigned: [],
          balance: arranged.balance,
          violations: arranged.violations,
          slots: arranged.slots,
          past: [],
          future: [],
          locked: [],
          view: 'board',
        });
        get().notify(`已解析 ${active.length} 人，分成 ${arranged.groups.length} 组`);
      } catch (e) {
        set({ error: (e as Error).message });
      } finally {
        set({ loading: false });
      }
    },

    regroup: async () => {
      const { players, groupSize, strategy, locked } = get();
      if (!players.length) return;
      set({ loading: true });
      try {
        pushHistory();
        // 锁定的人先摘出来，分组后再按原组插回
        const arranged = await api.arrange({
          players,
          groupSize,
          strategy,
          constraints: get().scheduleInput(),
        });
        let groups = arranged.groups;
        if (locked.length) {
          const lockedPlayers = players.filter((p) => locked.includes(p.key));
          const flat = groups.flat();
          const rest = flat.filter((p) => !locked.includes(p.key));
          // 锁定的按之前所在组号插回
          const prev = get().buckets;
          groups = arranged.groups.map((g) => g.filter((p) => !locked.includes(p.key)));
          for (const lp of lockedPlayers) {
            const gi = prev.findIndex((b) => b.players.some((p) => p.key === lp.key));
            const target = gi >= 0 && gi < groups.length ? gi : 0;
            groups[target].push(lp);
          }
          void rest;
        }
        set({
          buckets: toBuckets(groups),
          balance: arranged.balance,
          violations: arranged.violations,
          slots: arranged.slots,
        });
        const bad = arranged.violations;
        get().notify(bad.length ? `已重新分组，仍有 ${bad.length} 条约束冲突` : '已按当前策略重新分组');
      } catch (e) {
        get().notify(`重新分组失败：${(e as Error).message}`);
      } finally {
        set({ loading: false });
      }
    },

    /** 拖拽/编辑后重算均衡与约束（防抖） */
    refreshDerived: async () => {
      if (inspectTimer) clearTimeout(inspectTimer);
      inspectTimer = setTimeout(async () => {
        const { buckets } = get();
        if (!buckets.length) {
          set({ balance: null, violations: [], slots: [] });
          return;
        }
        try {
          const res = await api.inspect({
            groups: toPlayerGroups(buckets),
            constraints: get().scheduleInput(),
          });
          set({ balance: res.balance, violations: res.violations, slots: res.slots });
        } catch {
          /* 体检失败不打断编辑 */
        }
      }, 180);
    },

    /** 均衡微调：只让极差变小，且不动锁定的人 */
    polish: async () => {
      const { buckets, locked } = get();
      if (!buckets.length) return;
      try {
        pushHistory();
        const res = await api.polish({
          groups: toPlayerGroups(buckets),
          protected: locked,
          constraints: get().scheduleInput(),
        });
        set({
          buckets: toBuckets(res.groups),
          balance: res.balance,
          violations: res.violations,
          slots: res.slots,
        });
        get().notify(`均衡微调完成，极差 ${res.balance.spread}%`);
      } catch (e) {
        get().notify(`均衡失败：${(e as Error).message}`);
      }
    },

    addBucket: () => {
      pushHistory();
      set((s) => ({ buckets: [...s.buckets, { id: bucketId(), name: `第 ${s.buckets.length + 1} 组`, players: [] }] }));
      void get().refreshDerived();
    },

    moveBucketTo: (id, groupNumber) => {
      const { buckets } = get();
      const fromIndex = buckets.findIndex((b) => b.id === id);
      // Number.isFinite 这道必须有：NaN 过不了 `< 0` 判断，会一路漏到 splice(NaN)
      if (fromIndex < 0 || !Number.isFinite(groupNumber) || buckets.length < 2) return;
      // 组号是 1 起，夹到 [1, 组数]
      const toIndex = Math.max(0, Math.min(buckets.length - 1, Math.round(groupNumber) - 1));
      if (fromIndex === toIndex) return;
      pushHistoryUnlessDragging();
      const next = [...buckets];
      const [moved] = next.splice(fromIndex, 1);
      next.splice(toIndex, 0, moved);
      set({ buckets: next });
      void get().refreshDerived();
    },

    moveBucketOver: (id, overId) => {
      const { buckets } = get();
      const fromIndex = buckets.findIndex((b) => b.id === id);
      const overIndex = buckets.findIndex((b) => b.id === overId);
      if (fromIndex < 0 || overIndex < 0 || fromIndex === overIndex) return;
      pushHistoryUnlessDragging();
      const next = [...buckets];
      const [moved] = next.splice(fromIndex, 1);
      // 插入位必须用「移除之前」的 overIndex。用了移除之后的下标，
      // 把第 1 组拖到第 2 组上会算成插回第 0 位 —— 等于没动。
      next.splice(overIndex, 0, moved);
      set({ buckets: next });
      void get().refreshDerived();
    },

    sortBucketsByWave: () => {
      const { buckets, notify } = get();
      if (buckets.every((b) => b.players.length < 2)) {
        notify('每组都不足 2 人，无需排序');
        return;
      }
      pushHistory();
      const byWave = (a: Player, b: Player) =>
        b.wave - a.wave || a.name.localeCompare(b.name, 'zh-Hans-CN');
      set({
        buckets: buckets.map((b) => ({ ...b, players: [...b.players].sort(byWave) })),
      });
      void get().refreshDerived();
      notify('已按音浪从高到低排列各组');
    },

    removeBucket: (id) => {
      const target = get().buckets.find((b) => b.id === id);
      if (!target) return;
      // 有人就移到未分组区，不阻止删除
      pushHistory();
      set((s) => ({
        buckets: s.buckets.filter((b) => b.id !== id).map((b) => ({ ...b, name: b.name })),
        unassigned: [...s.unassigned, ...target.players],
      }));
      get().notify(target.players.length ? `已删除该组，${target.players.length} 人移到未分组` : '已删除该组');
      void get().refreshDerived();
    },

    renameBucket: (id, name) => {
      set((s) => ({ buckets: s.buckets.map((b) => (b.id === id ? { ...b, name } : b)) }));
    },

    moveCard: (activeKey, toBucketId, overKey) => {
      // 先在副本上算出结果：算完才知道有没有真的动过。
      // 之前是无条件 pushHistory + 无条件 set，onDragOver 每触发一次就塞一条撤销，
      // 原位松手也照塞，于是「撤销一次没反应」。
      const next = cloneBuckets(get().buckets);
      const from = next.find((b) => b.players.some((p) => p.key === activeKey));
      const to = next.find((b) => b.id === toBucketId);
      if (!from || !to) return;

      if (from.id === to.id) {
        const arr = from.players;
        const oldIndex = arr.findIndex((p) => p.key === activeKey);
        // over 可能就是它自己（原地松手），找不到落点就当追加到末尾
        const landed = overKey ? arr.findIndex((p) => p.key === overKey) : arr.length - 1;
        const newIndex = landed < 0 ? arr.length - 1 : landed;
        if (oldIndex < 0 || oldIndex === newIndex) return;
        const [moved] = arr.splice(oldIndex, 1);
        arr.splice(newIndex, 0, moved);
      } else {
        const idx = from.players.findIndex((p) => p.key === activeKey);
        if (idx < 0) return;
        const [moved] = from.players.splice(idx, 1);
        // 原来这里用 Math.max(0, -1) = 0，落点一旦失效就把人甩到目标组最前面
        const at = overKey ? to.players.findIndex((x) => x.key === overKey) : -1;
        to.players.splice(at < 0 ? to.players.length : at, 0, moved);
      }

      pushHistoryUnlessDragging();
      set({ buckets: next });
      void get().refreshDerived();
    },

    beginDrag: () => {
      // 只抓起点，不入栈；一次拖拽 = 一条撤销
      if (get().dragSnapshot) return;
      set({ dragSnapshot: snapshotOf(get().buckets, get().unassigned) });
    },

    endDrag: () => {
      const snap = get().dragSnapshot;
      if (!snap) return;
      set({ dragSnapshot: null });
      // 版式没动过就不留空撤销
      if (sameLayout(snap, snapshotOf(get().buckets, get().unassigned))) return;
      pushHistory(snap);
    },

    assignPlayer: (playerKey, toBucketId) => {
      pushHistory();
      set((s) => {
        // 先从任何地方摘出来
        let player: Player | undefined;
        const strip = (list: Player[]) => {
          const i = list.findIndex((p) => p.key === playerKey);
          if (i >= 0) player = list[i];
          return list.filter((p) => p.key !== playerKey);
        };
        let buckets = s.buckets.map((b) => ({ ...b, players: strip(b.players) }));
        const unassigned = strip(s.unassigned);
        if (!player) return s;
        if (toBucketId === null) {
          return { buckets, unassigned: [...unassigned, player!] };
        }
        buckets = buckets.map((b) => (b.id === toBucketId ? { ...b, players: [...b.players, player!] } : b));
        return { buckets, unassigned };
      });
      void get().refreshDerived();
    },

    swapPlayers: (keyA, keyB) => {
      if (keyA === keyB) return;
      pushHistory();
      set((s) => {
        const buckets = s.buckets.map((b) => ({ ...b, players: [...b.players] }));
        const findLoc = (key: string) => {
          for (let gi = 0; gi < buckets.length; gi++) {
            const pi = buckets[gi].players.findIndex((p) => p.key === key);
            if (pi >= 0) return { gi, pi };
          }
          return null;
        };
        const a = findLoc(keyA);
        const b = findLoc(keyB);
        if (!a || !b) return s;
        const pa = buckets[a.gi].players[a.pi];
        const pb = buckets[b.gi].players[b.pi];
        buckets[a.gi].players[a.pi] = pb;
        buckets[b.gi].players[b.pi] = pa;
        return { buckets };
      });
      void get().refreshDerived();
    },

    toggleLock: (playerKey) => {
      set((s) => ({
        locked: s.locked.includes(playerKey)
          ? s.locked.filter((k) => k !== playerKey)
          : [...s.locked, playerKey],
      }));
      const now = get().locked.includes(playerKey);
      get().notify(now ? '已锁定：自动分组与均衡都不会动他' : '已解锁');
    },

    unassign: (playerKey) => {
      get().assignPlayer(playerKey, null);
    },

    assignFromPool: (playerKey, toBucketId) => {
      get().assignPlayer(playerKey, toBucketId);
    },

    undo: () => {
      const { past, future, buckets, unassigned } = get();
      const prev = past[past.length - 1];
      if (!prev) {
        get().notify('没有可撤销的操作');
        return;
      }
      const cur: Snapshot = { buckets: cloneBuckets(buckets), unassigned: [...unassigned] };
      set({
        past: past.slice(0, -1),
        future: [cur, ...future].slice(0, MAX_HISTORY),
        buckets: prev.buckets,
        unassigned: prev.unassigned,
      });
      void get().refreshDerived();
    },

    redo: () => {
      const { past, future, buckets, unassigned } = get();
      const next = future[0];
      if (!next) {
        get().notify('没有可重做的操作');
        return;
      }
      const cur: Snapshot = { buckets: cloneBuckets(buckets), unassigned: [...unassigned] };
      set({
        past: [...past, cur].slice(-MAX_HISTORY),
        future: future.slice(1),
        buckets: next.buckets,
        unassigned: next.unassigned,
      });
      void get().refreshDerived();
    },

    addToBucket: async (bucketId, name) => {
      const trimmed = name.trim();
      if (!trimmed) return false;
      const norm = normalizePlayerName;
      const dup = [...get().buckets.flatMap((b) => b.players), ...get().unassigned]
        .some((p) => norm(p.name) === norm(trimmed));
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
        pushHistory();
        set((s) => ({
          buckets: s.buckets.map((b) =>
            b.id === bucketId ? { ...b, players: [...b.players, hit] } : b,
          ),
        }));
        await get().refreshDerived();
        const bad = get().violations.filter((v) => v.names.includes(hit.name));
        const unmatchedHint = hit.matched ? '' : '（未匹配到账号，音浪按 0 计，可到「名单」页绑定）';
        get().notify(
          bad.length
            ? `已把 ${hit.name} 加入，但有约束冲突：${bad[0].message}`
            : `已把 ${hit.name} 增补进该组${unmatchedHint}`,
        );
        return true;
      } catch (e) {
        get().notify(`增补失败：${(e as Error).message}`);
        return false;
      }
    },

    makeShareLink: async () => {
      const { title, metric, days, strategy, buckets, brand, display, startTime, intervalMin, notify } = get();
      if (!buckets.length) {
        notify('还没有分组结果');
        return null;
      }
      try {
        const { id } = await api.savePlan({
          title,
          metric,
          days,
          payload: {
            title,
            metric,
            days,
            strategy,
            brand,
            display,
            schedule: { start: startTime, intervalMin },
            buckets: buckets.map((b) => ({ name: b.name, players: b.players })),
          },
        });
        // 展示页只读已发布的方案，所以存完直接上架
        await api.setPublished(id, true);
        set((s) => ({ lastPlanId: id, plans: { ...s.plans, items: s.plans.items.map((p) => (p.id === id ? { ...p, published: true } : p)) } }));
        const url = `${location.origin}/s/${id}`;
        try {
          await navigator.clipboard.writeText(url);
          notify(`已上架并复制展示链接：${url}`);
        } catch {
          notify(`已上架，展示链接：${url}`);
        }
        return url;
      } catch (e) {
        notify(`生成链接失败：${(e as Error).message}`);
        return null;
      }
    },

    setPublished: async (id, published) => {
      try {
        await api.setPublished(id, published);
        set((s) => ({
          lastPlanId: published ? id : s.lastPlanId === id ? null : s.lastPlanId,
          plans: { ...s.plans, items: s.plans.items.map((p) => (p.id === id ? { ...p, published } : p)) },
        }));
        get().notify(published ? `方案 #${id} 已上架，可访问 /s/${id}` : `方案 #${id} 已下架`);
      } catch (e) {
        get().notify(`操作失败：${(e as Error).message}`);
      }
    },

    logout: async () => {
      try {
        await api.logout();
      } catch {
        /* 后端没开鉴权时 logout 无所谓，本地 token 照样清 */
      }
      setToken(null);
      window.dispatchEvent(new Event(UNAUTHORIZED_EVENT));
      get().notify('已退出登录');
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
      await get().refreshDerived();
      get().notify(`已绑定 ${rowName} → ${anchorName}`);
    },

    setManualWave: (rowName, wave) => {
      pushHistory();
      const patch = (p: Player) => (p.name === rowName ? { ...p, wave } : p);
      set((s) => ({
        players: s.players.map(patch),
        buckets: s.buckets.map((b) => ({ ...b, players: b.players.map(patch) })),
        unassigned: s.unassigned.map(patch),
      }));
      if (toastTimer) clearTimeout(toastTimer);
      set({ toast: `已手动设置 ${rowName} 的音浪` });
      toastTimer = setTimeout(() => set({ toast: null }), 2200);
      void get().refreshDerived();
    },

    savePlanNow: async () => {
      const { title, metric, days, strategy, buckets, brand, display, startTime, intervalMin, notify } = get();
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
            strategy,
            brand,
            display,
            schedule: { start: startTime, intervalMin },
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
          lastPlanId: id,
          title: p.payload.title || p.title,
          metric: p.payload.metric ?? 'lastN',
          days: p.payload.days ?? 3,
          strategy: p.payload.strategy ?? 'snake',
          display: { ...DEFAULT_DISPLAY, ...(p.payload.display ?? {}) },
          buckets: (p.payload.buckets ?? []).map((b) => ({
            id: bucketId(),
            name: b.name,
            players: b.players ?? [],
          })),
          view: 'board',
          past: [],
          future: [],
          plans: { ...get().plans, open: false },
        });
        await get().refreshDerived();
        get().notify('方案已载入');
      } catch (e) {
        get().notify(`载入失败：${(e as Error).message}`);
      }
    },

    removeSavedPlan: async (id) => {
      await api.deletePlan(id);
      set((s) => ({
        lastPlanId: s.lastPlanId === id ? null : s.lastPlanId,
        plans: { ...s.plans, items: s.plans.items.filter((p) => p.id !== id) },
      }));
      get().notify('方案已删除');
    },
  };
});
