import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  DEFAULT_DISPLAY,
  EVENT_TYPES,
  METRICS,
  SCORE_LABEL,
  STRATEGIES,
  STRENGTH_MODES,
  defaultStrengthOrder,
  metricLabel,
  metricOfStrategy,
  pk,
  type EventType,
} from './api';
import { buildExportNode, disposeExportNode, downloadBlob, nodeToPngBlob } from './exportImage';
import { REVIVAL_MINUTES, revivalAfterGroup, rulesOf } from './rules';
import { useAutoRefresh } from '../useAutoRefresh';
import { isoLocal } from '../lib/date';
import { registerLeaveGuard } from '../lib/router';
import type {
  BalanceStat,
  BreakRule,
  Bucket,
  ConstraintsMeta,
  DisplayConfig,
  InspectResponse,
  PlanBrief,
  Player,
  Strategy,
  StrengthMode,
  UnmatchedItem,
  Violation,
  WaveMetric,
} from './types';

let seq = 0;
const newId = () => `b${++seq}`;
const toBuckets = (groups: Player[][]): Bucket[] =>
  groups.map((players, i) => ({ id: newId(), name: `第 ${i + 1} 组`, players }));

const wan = (n: number) => {
  n = Math.round(Number(n) || 0);
  if (n < 10000) return String(n);
  const w = Math.floor(n / 10000);
  const r = n % 10000;
  return r === 0 ? `${w}万` : `${w}万${String(r).padStart(4, '0')}`;
};

const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v)) as T;

/** 月终争霸赛 / 两级晋级赛的加长休息点：第 3 组之后（分钟数可调） */
const BREAK_AFTER_GROUP = 3;

/** 有复活赛的赛事：初级 / 中级争霸赛小组赛。
 *  复活赛是**一场 20 分钟的比赛**（不是休息），只打一场，落在「上半程最后一组」之后：
 *  8 个组 / 7 个组 → 第 4 组后，6 个组 → 第 3 组后，即第 ceil(组数/2) 组之后。位置和时长都不可改。 */
const REVIVAL_EVENTS: readonly EventType[] = ['初级争霸赛小组赛', '中级争霸赛小组赛'];

/** 自动生成的标题前缀（「初级争霸赛小组赛 · PK 分组 …」）。切赛事后要能识别出来清掉，
 *  否则导出长图/方案列表会把上一场的赛事名带出去。老名字也一并认，兼容改名前的方案标题。 */
const AUTO_TITLE_RE = new RegExp(
  `^(${[...EVENT_TYPES, '初级', '中级争霸赛', '月底争霸赛', '初级晋级赛', '中级晋级赛', '晋级赛'].join('|')}) · PK 分组`,
);

interface Snapshot {
  buckets: Bucket[];
  unassigned: Player[];
}

/** 拖拽中的可视状态：跟手的幽灵卡片 + 落位提示 */
interface DragState {
  key: string;
  name: string;
  wave: number;
  x: number;
  y: number;
  bucketId: string | null;
  overKey: string | null;
  after: boolean;
}

export default function GroupPage() {
  /* ---------------- 配置 ---------------- */
  /** 赛事类型：初级/中级争霸赛小组赛（月初、月中）、月终争霸赛（月末）、两级晋级赛。
   *  切换类型会换名单草稿和方案列表，并清空看板——各场比赛的名单本来就不一样。 */
  const [eventType, setEventType] = useState<EventType>('初级争霸赛小组赛');
  const [text, setText] = useState('');
  const [metric, setMetric] = useState<WaveMetric>('lastN');
  const [days, setDays] = useState(3);
  const [strategy, setStrategy] = useState<Strategy>('snake');
  const [groupSize, setGroupSize] = useState(8);
  const [startTime, setStartTime] = useState('12:15');
  const [intervalMin, setIntervalMin] = useState(15);

  /* ---------------- 数据 ---------------- */
  const [players, setPlayers] = useState<Player[]>([]);
  const [buckets, setBuckets] = useState<Bucket[]>([]);
  const [unassigned, setUnassigned] = useState<Player[]>([]);
  const [locked, setLocked] = useState<string[]>([]);
  /** 「识别不到」的行：原始行 + 库里候选，逐行显示在名单下面（点候选即建别名） */
  const [unmatched, setUnmatched] = useState<UnmatchedItem[]>([]);
  /** 名单里带分数的行数（晋级赛名单 `名字 分数`）：> 0 时口径改用名单分数 */
  const [scoredCount, setScoredCount] = useState(0);
  const [excluded, setExcluded] = useState<string[]>([]);
  const [dates, setDates] = useState<string[]>([]);
  const [balance, setBalance] = useState<BalanceStat | null>(null);
  const [violations, setViolations] = useState<Violation[]>([]);
  const [slots, setSlots] = useState<string[]>([]);
  const [meta, setMeta] = useState<ConstraintsMeta | null>(null);

  /* ---------------- 方案 ---------------- */
  const [title, setTitle] = useState('');
  const [plans, setPlans] = useState<PlanBrief[]>([]);
  const [planId, setPlanId] = useState<number | null>(null);
  const [display, setDisplay] = useState<DisplayConfig>(DEFAULT_DISPLAY);
  /** 当前弹出「⋯」操作菜单的玩家 key（参照原版 PlayerCard 的单按钮菜单） */
  const [menuFor, setMenuFor] = useState<string | null>(null);
  /** 强度位次形态：默认「每组一个最强」（第 3 组头牌最强、第 4 组次之、第 1 组最弱）；
   *  off = 不排位次只做均衡。强制条件始终优先满足。 */
  const [strengthMode, setStrengthMode] = useState<StrengthMode | 'off'>('top');
  const strengthOn = strengthMode !== 'off';
  /** 第 3 组打完到下组开赛的间隔（加长休息），与常规间隔分开配。
   *  只在**没有复活赛**的赛事生效（月终争霸赛、两级晋级赛）；小组赛的 20 分钟由复活赛占。 */
  const [longBreak, setLongBreak] = useState(20);

  /* ---------------- 交互状态 ---------------- */
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState('');
  const [error, setError] = useState('');
  const [msg, setMsg] = useState('');
  /* 拖拽：指针事件实现（鼠标 / 触屏 / 触控笔同一套），不用 HTML5 draggable——
     后者在触屏上完全不触发，而且没有跟手的幽灵卡片。 */
  const [drag, setDrag] = useState<DragState | null>(null);
  const dragRef = useRef<{
    key: string;
    name: string;
    wave: number;
    startX: number;
    startY: number;
    offX: number;
    offY: number;
    w: number;
    h: number;
    active: boolean;
    x: number;
    y: number;
    scrollDir: number;
  } | null>(null);
  const rafRef = useRef(0);
  const scrollRef = useRef(0);
  const [past, setPast] = useState<Snapshot[]>([]);
  const [future, setFuture] = useState<Snapshot[]>([]);
  const [addName, setAddName] = useState('');
  const [addTo, setAddTo] = useState('');

  /* 点击卡片外任意处关闭 ⋯ 菜单（菜单内部点击已 stopPropagation） */
  useEffect(() => {
    if (!menuFor) return;
    const close = () => setMenuFor(null);
    document.addEventListener('click', close);
    return () => document.removeEventListener('click', close);
  }, [menuFor]);

  /* P1-12：未保存的看板在切页前确认。看板上有分组内容就算「未保存」
     （拖半小时的成果不该被一次切页清零）；保存过方案也只是防手滑，确认一次即可。
     守卫在 App 的路由跳转（go）里生效；浏览器刷新/关标签另有 beforeunload。 */
  useEffect(() => {
    const unregister = registerLeaveGuard(() => {
      if (buckets.length === 0) return null;
      return 'PK 分组看板上有未保存的分组';
    });
    return unregister;
  }, [buckets.length]);

  /* P1-12：浏览器直接刷新 / 关标签也拦一道（SPA 内 confirm 管不到这两条路径） */
  useEffect(() => {
    const onBeforeUnload = (e: BeforeUnloadEvent) => {
      if (buckets.length === 0) return;
      e.preventDefault();
      e.returnValue = '';
    };
    window.addEventListener('beforeunload', onBeforeUnload);
    return () => window.removeEventListener('beforeunload', onBeforeUnload);
  }, [buckets.length]);

  /* 加长停留点：小组赛是复活赛（20 分钟，位置随组数走），其余赛事是第 3 组后的可调休息。 */
  /** 本赛事有没有复活赛（初级/中级争霸赛小组赛），跟有没有分组无关 */
  const revivalEvent = REVIVAL_EVENTS.includes(eventType);
  /** 已分好组，复活赛位置能算出来了 */
  const revivalOn = revivalEvent && buckets.length > 0;
  /** 复活赛落点：第 N 组（1 起）比完就打 */
  const revivalAfter = revivalAfterGroup(buckets.length);
  /** 加长停留发生在第几组之后（1 起）；没分组时先按老位置占位，分完组立刻纠正 */
  const restAfterGroup = revivalOn ? revivalAfter : BREAK_AFTER_GROUP;
  /** 加长停留的分钟数 */
  const restMin = revivalEvent ? REVIVAL_MINUTES : longBreak;
  /** 本轮赛程是否真的有加长停留（没分组的复活赛不算，那时还没有赛程） */
  const restOn = restAfterGroup > 0 && (revivalOn || (!revivalEvent && longBreak > 0));

  /** 赛程里的加长停留点（与 intervalMin 不同走这条） */
  const breaks = useMemo<BreakRule[]>(
    () => (restOn ? [{ afterGroup: restAfterGroup, minutes: restMin }] : []),
    [restOn, restAfterGroup, restMin],
  );

  /** 分组/体检都带上的约束：赛程（含加长休息）+ 强度位次形态 */
  const scheduleInput = useCallback(
    () => ({
      schedule: { start: startTime, intervalMin, breaks },
      strengthOrder: strengthOn
        ? { enabled: true, mode: strengthMode as StrengthMode }
        : { enabled: false },
    }),
    [startTime, intervalMin, breaks, strengthOn, strengthMode],
  );

  /* ---------------- 体检（拖拽后重算均衡与冲突） ---------------- */
  /** P1-13：inspect 并发序号。连拖两次会并发两个请求，旧请求后到就会把组头
   *  开赛时间刷回旧分组的 slots（还会被印进导出长图）。自增序号只认最后一次
   *  发出的请求；in-flight 的 promise 留给导出长图等它完成。 */
  const inspectSeqRef = useRef(0);
  const inspectInflightRef = useRef<Promise<InspectResponse | null> | null>(null);

  async function inspectNow(next: Bucket[]) {
    if (!next.length) {
      inspectSeqRef.current += 1;
      inspectInflightRef.current = null;
      setBalance(null);
      setViolations([]);
      setSlots([]);
      return;
    }
    const seqAtRequest = ++inspectSeqRef.current;
    const req = pk
      .inspect({ groups: next.map((b) => b.players), constraints: scheduleInput() })
      .then((r): InspectResponse => r)
      .catch((e) => {
        // 只有最新的那次才允许往页面上报错；被更新的拖拽盖掉的旧响应静默丢弃
        if (seqAtRequest === inspectSeqRef.current) setError((e as Error).message);
        return null;
      })
      .finally(() => {
        if (inspectInflightRef.current === req) inspectInflightRef.current = null;
      });
    inspectInflightRef.current = req;
    const r = await req;
    if (r == null || seqAtRequest !== inspectSeqRef.current) return; // 过期响应：丢弃
    setBalance(r.balance);
    setViolations(r.violations || []);
    setSlots(r.slots || []);
  }

  const pushHistory = useCallback((snap?: Snapshot) => {
    const cur: Snapshot = snap ?? { buckets: clone(buckets), unassigned: clone(unassigned) };
    setPast((p) => [...p.slice(-29), cur]);
    setFuture([]);
  }, [buckets, unassigned]);

  const applyGroups = useCallback(
    (next: Bucket[], nextUnassigned?: Player[], history = true) => {
      if (history) pushHistory();
      setBuckets(next);
      if (nextUnassigned) setUnassigned(nextUnassigned);
      void inspectNow(next);
    },
    [pushHistory],
  );

  /* ---------------- 初始化 ---------------- */
  useEffect(() => {
    pk.constraints()
      .then((m) => {
        setMeta(m);
        setStartTime(m.schedule?.start || '12:15');
        setIntervalMin(m.schedule?.intervalMin || 15);
      })
      .catch(() => {});
    // 方案列表由下面的 [eventType] effect 拉（P1-14 过滤需要跟着赛事类型走）
  }, []);

  /* 方案在别处（另一个页签/设备）被保存、上架、删除时自动刷新列表。
     loadPlans 每次渲染都是新函数没关系——hook 内部用 ref 持有，连接不会重建。 */
  useAutoRefresh(() => {
    void loadPlans();
  }, ['plans']);

  /* P1-14：切赛事类型后重拉方案列表（loadPlans 按 eventType 过滤）。
     eventType 变化时函数式组件已重新渲染，直接在这里重拉即可。 */
  useEffect(() => {
    void loadPlans();
    // loadPlans 引用每次渲染都变，只跟 eventType 走
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [eventType]);

  /* 名单草稿按赛事类型各存一份：切类型就换名单，没存过则清空输入框重填。
     坑：拉草稿是异步的，用户可能在返回前就已经粘贴了新名单——那种情况绝不能用
     旧草稿（或空串）盖掉用户刚贴的东西，所以用 typedRef 记「用户已经自己动手了」。 */
  const typedRef = useRef(false);
  useEffect(() => {
    let dead = false;
    typedRef.current = false;
    pk.getRoster(eventType)
      .then((r) => {
        if (dead || typedRef.current) return;
        setText(r.saved && r.text ? r.text : '');
      })
      .catch(() => {});
    return () => {
      dead = true;
    };
  }, [eventType]);

  /** 切换赛事：清空看板（上一场的人不属于这一场），重新拉对应名单 */
  function switchEvent(t: EventType) {
    if (t === eventType) return;
    if (
      (buckets.length > 0 || players.length > 0) &&
      !confirm(`切换到「${t}」会清空当前看板和名单（未保存的分组会丢失），继续？`)
    ) {
      return;
    }
    setEventType(t);
    setBuckets([]);
    setPlayers([]);
    setUnassigned([]);
    setLocked([]);
    setUnmatched([]);
    setExcluded([]);
    setDates([]);
    setBalance(null);
    setViolations([]);
    setSlots([]);
    setPlanId(null);
    setPast([]);
    setFuture([]);
    /* 自动生成的标题带着旧赛事名（「初级 · PK 分组 …」），切赛事后要清掉，
       否则导出长图/方案列表会把上一场的赛事名带出去；手工改过的标题不动。 */
    setTitle((t) => (AUTO_TITLE_RE.test(t) ? '' : t));
    setMsg(`已切换到「${t}」`);
  }

  async function loadPlans() {
    try {
      const r = await pk.listPlans();
      // P1-14：方案列表按当前赛事类型过滤（后端 /api/plans 无 type 参数，前端过滤；
      // eventType 为空的老方案不归类，不在任何赛事页出现，避免跨赛事载入/上架）
      setPlans((r.items || []).filter((p) => p.eventType === eventType));
    } catch (e) {
      // P2-5：方案列表加载失败不再静默 —— 显示出来（载入/上架按钮全会没数据）
      console.error('方案列表加载失败', e);
      setError((e as Error).message);
      setPlans([]);
    }
  }

  /* ---------------- 名单 ---------------- */
  async function saveRoster() {
    if (!text.trim()) return;
    setBusy('roster');
    try {
      await pk.saveRoster(text, eventType);
      setMsg(`${eventType}名单已保存`);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy('');
    }
  }

  /** 识别不到的行，点候选一键修：把「名单里的写法 → 库里真名」记成别名，然后重新解析分组。
   *  记一次以后这条写法就一直认得（别名表在后端 SQLite，跨设备生效）。 */
  async function linkAlias(rowName: string, realName: string) {
    setBusy('alias');
    setError('');
    try {
      await pk.saveAlias(rowName, realName);
      setMsg(`已记住「${rowName}」= ${realName}，正在重新分组…`);
      await parseAndGroup(false);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy('');
    }
  }

  /** 解析名单 → 直接按当前策略分组 */
  async function parseAndGroup(force = false) {
    if (!text.trim()) {
      setError('先粘贴或填入名单');
      return;
    }
    setLoading(true);
    setError('');
    setMsg('');
    try {
      const res = await pk.parse({ text, metric, days, force });
      const active = res.players.filter((p) => !p.excluded);
      if (!active.length) {
        setError('名单里没有可参赛的人');
        setLoading(false);
        return;
      }
      setPlayers(res.players);
      setUnmatched(res.unmatched);
      setScoredCount(res.scored || 0);
      setExcluded(res.excluded.map((e) => e.name));
      setDates(res.dates || []);
      const arranged = await pk.arrange({
        players: res.players,
        groupSize,
        strategy,
        constraints: scheduleInput(),
      });
      const next = toBuckets(arranged.groups);
      setPast([]);
      setFuture([]);
      setLocked([]);
      setUnassigned([]);
      setBuckets(next);
      setBalance(arranged.balance);
      setViolations(arranged.violations || []);
      setSlots(arranged.slots || []);
      setMsg(
        `已解析 ${active.length} 人，分成 ${arranged.groups.length} 组` +
          (res.unmatched.length ? ` · ${res.unmatched.length} 行识别不到（见下面红框）` : ''),
      );
      if (!title) setTitle(`${eventType} · PK 分组 ${(res.dates?.[0]) || isoLocal()}`);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setLoading(false);
    }
  }

  /** 重新分组：名单不变，只换策略/组人数/赛程 */
  async function regroup() {
    if (!players.length) {
      setError('先解析名单');
      return;
    }
    setLoading(true);
    try {
      const lockedPlayers = players.filter((p) => locked.includes(p.key));
      const arranged = await pk.arrange({
        players,
        groupSize,
        strategy,
        constraints: scheduleInput(),
      });
      let groups = arranged.groups.map((g) => g.filter((p) => !locked.includes(p.key)));
      // 锁定的人按原组号插回；原组越界就放第 1 组
      for (const lp of lockedPlayers) {
        const gi = buckets.findIndex((b) => b.players.some((p) => p.key === lp.key));
        const target = gi >= 0 && gi < groups.length ? gi : 0;
        groups[target].push(lp);
      }
      const next = groups.map((playersInGroup, i) => ({
        id: buckets[i]?.id ?? newId(),
        name: `第 ${i + 1} 组`,
        players: playersInGroup,
      }));
      applyGroups(next);
      setMsg('已重新分组');
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setLoading(false);
    }
  }

  /* ---------------- 看板操作 ---------------- */
  /**
   * 落位。`after` 决定插在目标卡片「下面」还是「上面」——由指针落在卡片的上半还是下半决定，
   * 这样插入线画在哪、人最后就在哪，不会出现「线在下面、人跑上面」。
   */
  function moveCard(activeKey: string, toBucketId: string, overKey: string | null, after = false) {
    const next = clone(buckets);
    const from = next.find((b) => b.players.some((p) => p.key === activeKey));
    const to = next.find((b) => b.id === toBucketId);
    if (!from || !to) return;
    if (from.id === to.id) {
      const arr = from.players;
      const oldIndex = arr.findIndex((p) => p.key === activeKey);
      let landed = overKey ? arr.findIndex((p) => p.key === overKey) : arr.length - 1;
      if (landed < 0) landed = arr.length - 1;
      let newIndex = after ? landed + 1 : landed;
      // 先摘后插：摘掉的那个人在落点之前时，后面的下标会整体前移一格
      if (oldIndex < newIndex) newIndex -= 1;
      if (oldIndex < 0 || oldIndex === newIndex) return;
      const [moved] = arr.splice(oldIndex, 1);
      arr.splice(newIndex, 0, moved);
    } else {
      const idx = from.players.findIndex((p) => p.key === activeKey);
      if (idx < 0) return;
      const [moved] = from.players.splice(idx, 1);
      const at = overKey ? to.players.findIndex((x) => x.key === overKey) : -1;
      const insertAt = at < 0 ? to.players.length : after ? at + 1 : at;
      to.players.splice(insertAt, 0, moved);
    }
    applyGroups(next);
  }

  function removeCard(key: string) {
    const next = clone(buckets);
    for (const b of next) {
      const i = b.players.findIndex((p) => p.key === key);
      if (i >= 0) {
        const [moved] = b.players.splice(i, 1);
        applyGroups(next, [...unassigned, moved]);
        return;
      }
    }
  }

  function assignTo(bucketId: string, key: string) {
    const p = unassigned.find((x) => x.key === key);
    if (!p) return;
    const next = clone(buckets);
    const to = next.find((b) => b.id === bucketId);
    if (!to) return;
    to.players.push(p);
    applyGroups(next, unassigned.filter((x) => x.key !== key));
  }

  async function addPerson() {
    const name = addName.trim();
    if (!name) return;
    setBusy('add');
    try {
      const res = await pk.parse({ text: name, metric, days });
      const p = res.players.find((x) => !x.excluded);
      if (!p) throw new Error('库里没有这个人，先去「主播管理」建档');
      const next = clone(buckets);
      const to = next.find((b) => b.id === addTo) ?? next[0];
      if (!to) throw new Error('先分组，再往里加人');
      if (next.some((b) => b.players.some((x) => x.name === p.name))) throw new Error(`${p.name} 已经在看板里了`);
      to.players.push(p);
      setPlayers((ps) => [...ps, p]);
      applyGroups(next);
      setAddName('');
      setMsg(`已把 ${p.name} 加入${to.name}`);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy('');
    }
  }

  async function polish() {
    if (!buckets.length) return;
    setBusy('polish');
    try {
      const r = await pk.polish({
        groups: buckets.map((b) => b.players),
        protected: locked,
        constraints: scheduleInput(),
      });
      const next = r.groups.map((playersInGroup, i) => ({
        id: buckets[i]?.id ?? newId(),
        name: buckets[i]?.name ?? `第 ${i + 1} 组`,
        players: playersInGroup,
      }));
      applyGroups(next);
      setMsg('已做一轮均衡微调');
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy('');
    }
  }

  /** 自动修正：重新分组（锁定的人保持在原组） */
  const autoFix = regroup;

  function undo() {
    const prev = past[past.length - 1];
    if (!prev) return;
    setPast((p) => p.slice(0, -1));
    setFuture((f) => [...f, { buckets: clone(buckets), unassigned: clone(unassigned) }]);
    setBuckets(prev.buckets);
    setUnassigned(prev.unassigned);
    void inspectNow(prev.buckets);
  }

  function redo() {
    const next = future[future.length - 1];
    if (!next) return;
    setFuture((f) => f.slice(0, -1));
    setPast((p) => [...p, { buckets: clone(buckets), unassigned: clone(unassigned) }]);
    setBuckets(next.buckets);
    setUnassigned(next.unassigned);
    void inspectNow(next.buckets);
  }

  function toggleLock(key: string) {
    setLocked((l) => (l.includes(key) ? l.filter((k) => k !== key) : [...l, key]));
  }

  /* ---------------- 方案 ---------------- */
  async function savePlanFn() {
    if (!buckets.length) {
      setError('看板是空的，先分组');
      return;
    }
    setBusy('save');
    try {
      const r = await pk.savePlan({
        title: title.trim() || '未命名方案',
        metric,
        days,
        eventType,
        payload: {
          title: title.trim() || '未命名方案',
          metric,
          days,
          strategy,
          strengthOrder: { enabled: strengthOn, mode: strengthOn ? strengthMode : undefined },
          buckets: buckets.map((b) => ({ name: b.name, players: b.players })),
          schedule: { start: startTime, intervalMin, breaks },
          display,
        },
      });
      setPlanId(r.id);
      setMsg(`${eventType}方案已保存（#${r.id}）`);
      await loadPlans();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy('');
    }
  }

  async function loadPlan(id: number) {
    setBusy('load');
    try {
      const d = await pk.getPlan(id);
      const p = d.payload || ({} as never);
      setBuckets((p.buckets || []).map((b: { name: string; players: Player[] }, i: number) => ({
        id: newId(),
        name: b.name || `第 ${i + 1} 组`,
        players: b.players || [],
      })));
      setUnassigned([]);
      setPast([]);
      setFuture([]);
      setLocked([]);
      setPlanId(id);
      setTitle(d.title || '');
      if (p.metric) setMetric(p.metric);
      if (p.days) setDays(p.days);
      if (p.strategy) setStrategy(p.strategy);
      if (p.strengthOrder) {
        setStrengthMode(
          p.strengthOrder.enabled === false ? 'off' : (p.strengthOrder.mode ?? 'total'),
        );
      }
      // 落点由组数决定（复活赛）或固定第 3 组（其余赛事），这里只把分钟数读回来
      const b0 = (p.schedule?.breaks || [])[0] as BreakRule | undefined;
      if (b0) setLongBreak(b0.minutes);
      if (p.schedule?.start) setStartTime(p.schedule.start);
      if (p.schedule?.intervalMin) setIntervalMin(p.schedule.intervalMin);
      if (p.display) setDisplay({ ...DEFAULT_DISPLAY, ...p.display });
      setPlayers((p.buckets || []).flatMap((b: { players: Player[] }) => b.players || []));
      setMsg(`已载入方案 #${id}`);
      void inspectNow(
        (p.buckets || []).map((b: { name: string; players: Player[] }, i: number) => ({
          id: `tmp${i}`,
          name: b.name,
          players: b.players || [],
        })),
      );
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy('');
    }
  }

  async function publish(id: number, published: boolean) {
    try {
      await pk.setPublished(id, published);
      await loadPlans();
      setMsg(
        published
          ? `方案 #${id} 已上架；展示端同类型只显示一场，其它同类型方案已自动下架（不同类型互不影响）`
          : `方案 #${id} 已下架`,
      );
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function removePlan(id: number) {
    if (!confirm(`确认删除方案 #${id}？`)) return;
    try {
      await pk.deletePlan(id);
      if (planId === id) setPlanId(null);
      await loadPlans();
    } catch (e) {
      setError((e as Error).message);
    }
  }

  /* ---------------- 导出长图 ---------------- */
  async function exportImage() {
    if (!buckets.length) {
      setError('看板是空的');
      return;
    }
    setBusy('export');
    try {
      // P1-13：导出前强制等最新的 inspect 落地 —— 拖完立刻点导出时，
      // slots 可能还是上一次分组的开赛时间，错的时间会被印进长图发到群里。
      if (inspectInflightRef.current) {
        await inspectInflightRef.current;
      }
      const node = buildExportNode({
        title: title.trim() || 'PK 分组',
        brandLine: `PK GROUP · ${dates[0] || isoLocal()}`,
        metaLine: `${eventType} · 共 ${buckets.reduce((s, b) => s + b.players.length, 0)} 人 · ${startTime} 后 / 间隔 ${intervalMin} 分${restOn ? ` · 第 ${restAfterGroup} 组后${revivalEvent ? '复活赛' : '休息'} ${restMin} 分` : ''}`,
        groups: buckets.map((b, i) => ({
          name: b.name,
          slot: slots[i] || '',
          players: b.players.map((p) => ({ name: p.name, leader: p.leader, wave: p.wave || 0 })),
        })),
        hideWave: !display.wave,
        metricLabel: waveLabel,
        rules: eventRule,
        footer: `${title.trim() || 'PK 分组'} · ${eventType} | ${isoLocal()}`,
      });
      try {
        const blob = await nodeToPngBlob(node, { scale: 2 });
        downloadBlob(blob, `${title.trim() || 'pk'}-${dates[0] || 'plan'}.png`);
        setMsg('长图已导出');
      } finally {
        disposeExportNode(node);
      }
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy('');
    }
  }

  /* ---------------- 派生 ---------------- */
  const totalPlayers = useMemo(() => buckets.reduce((s, b) => s + b.players.length, 0), [buckets]);

  const sequential = strategy === 'sequential' || strategy === 'sequential-last-n';

  /** 位次达成情况：top 模式看各组头牌，total 模式看各组总音浪 */
  const strengthHit = useMemo(() => {
    if (!strengthOn || buckets.length < 2) return null;
    const order = defaultStrengthOrder(buckets.length);
    const value = (b: Bucket) =>
      strengthMode === 'top'
        ? b.players.reduce((mx, p) => Math.max(mx, p.wave || 0), 0)
        : b.players.reduce((s, p) => s + (p.wave || 0), 0);
    for (let i = 0; i + 1 < order.length; i += 1) {
      const strong = buckets[order[i] - 1];
      const weak = buckets[order[i + 1] - 1];
      if (!(value(strong) > value(weak))) return false;
    }
    return true;
  }, [buckets, strengthOn, strengthMode]);

  const orderText = defaultStrengthOrder(buckets.length || 1)
    .map((g) => `第 ${g} 组`)
    .join(' > ');

  /** 第 i 组（0 起）开赛时间预览，含复活赛 / 第 3 组后的加长休息 */
  const slotAt = (i: number) => {
    const [h, m] = startTime.split(':').map((x) => Number(x) || 0);
    let t = h * 60 + m;
    for (let k = 0; k < i; k += 1) {
      t += intervalMin;
      if (restOn && k + 1 === restAfterGroup) t += restMin;
    }
    const hh = Math.floor(t / 60) % 24;
    return `${String(hh).padStart(2, '0')}:${String(t % 60).padStart(2, '0')}`;
  };
  const spreadPct = balance && balance.max ? ((balance.max - balance.min) / balance.max) * 100 : 0;
  /** 该赛事的赛制说明（内置），会随导出长图一起打出来 */
  const eventRule = rulesOf(eventType);
  /** 名单自带分数（晋级赛名单）时口径不再是库里音浪，文案整体换掉 */
  const byScore = scoredCount > 0;
  const waveLabel = byScore ? SCORE_LABEL : `${metricLabel(metric, days)}音浪`;
  const violationNames = useMemo(() => {
    const set = new Set<string>();
    for (const v of violations) for (const n of v.names) set.add(n);
    return set;
  }, [violations]);

  /* ---------------- 拖拽实现 ---------------- */
  /** 命中测试：指针下方最近的一张卡片 / 一个组，以及插在上半还是下半 */
  function hitTest(x: number, y: number): { bucketId: string | null; overKey: string | null; after: boolean } {
    const el = document.elementFromPoint(x, y) as HTMLElement | null;
    if (!el) return { bucketId: null, overKey: null, after: false };
    const card = el.closest('[data-player]') as HTMLElement | null;
    const group = el.closest('[data-bucket]') as HTMLElement | null;
    const bucketId = group?.dataset.bucket ?? null;
    if (!card) return { bucketId, overKey: null, after: false };
    const r = card.getBoundingClientRect();
    return { bucketId, overKey: card.dataset.player ?? null, after: y > r.top + r.height / 2 };
  }

  function stopDrag() {
    cancelAnimationFrame(rafRef.current);
    cancelAnimationFrame(scrollRef.current);
    window.removeEventListener('pointermove', onDragMove);
    window.removeEventListener('pointerup', onDragEnd);
    window.removeEventListener('pointercancel', onDragEnd);
    window.removeEventListener('keydown', onDragKey);
    document.body.classList.remove('pk-dragging');
    dragRef.current = null;
    setDrag(null);
  }

  function onDragMove(e: PointerEvent) {
    const d = dragRef.current;
    if (!d) return;
    // 起手阈值：6px 以内算点击，不然锁按钮和下拉就没法点了
    if (!d.active) {
      if (Math.hypot(e.clientX - d.startX, e.clientY - d.startY) < 6) return;
      d.active = true;
      document.body.classList.add('pk-dragging');
    }
    d.x = e.clientX;
    d.y = e.clientY;
    // 拖到视口上下边缘时自动滚，13 组的看板一屏装不下
    const edge = 90;
    d.scrollDir = e.clientY < edge ? -1 : e.clientY > window.innerHeight - edge ? 1 : 0;
    cancelAnimationFrame(rafRef.current);
    rafRef.current = requestAnimationFrame(() => {
      const cur = dragRef.current;
      if (!cur || !cur.active) return;
      setDrag({ key: cur.key, name: cur.name, wave: cur.wave, x: cur.x, y: cur.y, ...hitTest(cur.x, cur.y) });
    });
  }

  function autoScroll() {
    const d = dragRef.current;
    if (!d || !d.active) return;
    if (d.scrollDir) window.scrollBy(0, d.scrollDir * 14);
    scrollRef.current = requestAnimationFrame(autoScroll);
  }

  function onDragEnd() {
    const d = dragRef.current;
    if (d && d.active) {
      const hit = hitTest(d.x, d.y);
      if (hit.bucketId) moveCard(d.key, hit.bucketId, hit.overKey, hit.after);
    }
    stopDrag();
  }

  function onDragKey(e: KeyboardEvent) {
    if (e.key === 'Escape') stopDrag();
  }

  /** 卡片按下：鼠标可以整张卡起拖；触屏只能从左侧把手起拖，
      否则手指一碰卡片就不能滚页面了（看板有十几组，滚不动等于废掉）。 */
  function startDrag(e: React.PointerEvent, p: Player) {
    if (e.button !== undefined && e.button !== 0) return;
    const target = e.target as HTMLElement;
    if (target.closest('button, select, input')) return;
    if (e.pointerType !== 'mouse' && !target.closest('[data-grip]')) return;
    const card = (e.currentTarget as HTMLElement).getBoundingClientRect();
    dragRef.current = {
      key: p.key,
      name: p.name,
      wave: p.wave || 0,
      startX: e.clientX,
      startY: e.clientY,
      offX: e.clientX - card.left,
      offY: e.clientY - card.top,
      w: card.width,
      h: card.height,
      active: false,
      x: e.clientX,
      y: e.clientY,
      scrollDir: 0,
    };
    window.addEventListener('pointermove', onDragMove);
    window.addEventListener('pointerup', onDragEnd);
    window.addEventListener('pointercancel', onDragEnd);
    window.addEventListener('keydown', onDragKey);
    scrollRef.current = requestAnimationFrame(autoScroll);
  }

  function changeStrategy(s: Strategy) {
    setStrategy(s);
    const m = metricOfStrategy(s);
    if (m) setMetric(m);
  }

  return (
    <div className="page pk">
      <section className="card">
        <h2>名单与配置</h2>
        <textarea
          className="pk-roster"
          value={text}
          placeholder={
            '一行一个主播，第一个名字是参赛者，后面是队长/师傅（只标注不参赛）\n' +
            '例：啸安➕狼兴\n' +
            '也可以在名字后面写分数（晋级赛名单格式），分数直接当音浪用：狼哲 71259'
          }
          onChange={(e) => {
            typedRef.current = true;
            setText(e.target.value);
          }}
        />
        <div className="row">
          <button className="primary" disabled={loading} onClick={() => parseAndGroup(false)}>
            {loading ? '分组中…' : '解析并分组'}
          </button>
          <button disabled={loading} onClick={() => parseAndGroup(true)}>
            刷新音浪后分组
          </button>
          <button disabled={!text.trim() || busy === 'roster'} onClick={saveRoster}>
            保存名单
          </button>
          <span className="hint">名单每次都是现贴现解析（一行一个主播）</span>
        </div>
        <div className="row">
          <label>
            赛事{' '}
            <select value={eventType} onChange={(e) => switchEvent(e.target.value as EventType)}>
              {EVENT_TYPES.map((t) => (
                <option key={t} value={t}>
                  {t}
                </option>
              ))}
            </select>
          </label>
          <label>
            口径{' '}
            <select
              value={byScore ? 'score' : metric}
              onChange={(e) => setMetric(e.target.value as WaveMetric)}
              disabled={byScore || strategy === 'sequential' || strategy === 'sequential-last-n'}
              title={byScore ? '名单里带了分数（名字 分数），直接按名单分数分组，不看库里音浪' : ''}
            >
              {byScore && <option value="score">{SCORE_LABEL}</option>}
              {METRICS.map((m) => (
                <option key={m.value} value={m.value} title={m.hint}>
                  {m.label}
                </option>
              ))}
            </select>
          </label>
          <label>
            天数{' '}
            <input
              type="number"
              min={1}
              max={30}
              style={{ width: 64 }}
              value={metric === 'last1' ? 1 : days}
              disabled={byScore || metric !== 'lastN'}
              title={byScore ? '名单自带分数，不需要天数' : metric === 'lastN' ? '近 N 天的 N' : '只有「近 N 天」需要填天数'}
              onChange={(e) => setDays(Math.max(1, Number(e.target.value) || 1))}
            />
          </label>
          <label>
            策略{' '}
            <select value={strategy} onChange={(e) => changeStrategy(e.target.value as Strategy)}>
              {STRATEGIES.map((s) => (
                <option key={s.value} value={s.value} title={s.hint}>
                  {s.label}
                </option>
              ))}
            </select>
          </label>
          <label>
            强度位次{' '}
            <select
              value={strengthMode}
              disabled={sequential}
              title="和策略一起配：先满足强制条件（固定位/互斥/间隔），再排这个位次"
              onChange={(e) => setStrengthMode(e.target.value as StrengthMode | 'off')}
            >
              {STRENGTH_MODES.map((m) => (
                <option key={m.value} value={m.value} title={m.hint}>
                  {m.label}
                </option>
              ))}
            </select>
          </label>
          <label>
            每组人数{' '}
            <input
              type="number"
              min={2}
              max={20}
              style={{ width: 64 }}
              value={groupSize}
              onChange={(e) => setGroupSize(Math.max(2, Number(e.target.value) || 8))}
            />
          </label>
          <label>
            开赛{' '}
            <input
              type="time"
              style={{ width: 96 }}
              value={startTime}
              onChange={(e) => setStartTime(e.target.value)}
            />
          </label>
          <label>
            间隔(分){' '}
            <input
              type="number"
              min={1}
              max={120}
              style={{ width: 64 }}
              value={intervalMin}
              onChange={(e) => setIntervalMin(Math.max(1, Number(e.target.value) || 15))}
            />
          </label>
        </div>
        {(strategy === 'sequential' || strategy === 'sequential-last-n') && (
          <p className="hint">
            梯队式按「{waveLabel}」从高到低切块，第 1 组最强；口径由策略锁定。
          </p>
        )}
        <p className="hint">
          名单 {players.filter((p) => !p.excluded).length} 人
          {unmatched.length > 0 && ` · 识别不到 ${unmatched.length} 行`}
          {excluded.length > 0 && ` · 不参赛 ${excluded.length}`}
          {byScore && ` · 自带分数 ${scoredCount} 行（按名单分数分组，不看库里音浪）`}
          {!byScore && dates.length > 0 && ` · 数据日期 ${dates[0]}`}
        </p>
        {unmatched.length > 0 && (
          <div className="alert bad pk-unmatched">
            <b>识别不到 {unmatched.length} 行</b>
            <div className="pk-unmatched-list">
              {unmatched.map((u, i) => (
                <div key={`${u.row.raw}#${i}`} className="pk-unmatched-row">
                  <span className="pk-unmatched-raw" title={u.row.raw}>
                    {u.row.raw}
                  </span>
                  {u.candidates.length > 0 ? (
                    <>
                      <span className="hint">是不是</span>
                      {u.candidates.slice(0, 6).map((c) => (
                        <button
                          key={c.personId}
                          className="pk-cand"
                          disabled={busy === 'alias'}
                          title={`记为别名：${u.row.name} = ${c.name}`}
                          onClick={() => linkAlias(u.row.name, c.name)}
                        >
                          {c.name}
                        </button>
                      ))}
                      <span className="hint">（点一下就记住，下次自动认）</span>
                    </>
                  ) : (
                    <span className="hint">库里没有相近的人，先去「主播管理」建档</span>
                  )}
                </div>
              ))}
            </div>
          </div>
        )}
        {meta && (
          <details className="pk-meta">
            <summary>内置条件（后端 pk-core 权威定义）</summary>
            <div className="pk-meta-body">
              {meta.excludeTogether?.length > 0 && (
                <div>组内互斥：{meta.excludeTogether.map((s) => s.join(' / ')).join('；')}</div>
              )}
              {meta.pinned?.length > 0 && (
                <div>
                  固定位：
                  {meta.pinned
                    .map((p) => `${p.name} → 第 ${p.group === 'last' ? '末' : p.group} 组`)
                    .join('；')}
                </div>
              )}
              {meta.minGap?.length > 0 && (
                <div>最少间隔：{meta.minGap.map((g) => `${g.a} ↔ ${g.b} 隔 ${g.gap} 组`).join('；')}</div>
              )}
              {meta.minGroup?.length > 0 && (
                <div>起始组：{meta.minGroup.map((g) => `${g.name} 从第 ${g.group} 组起`).join('；')}</div>
              )}
              <div className="hint">改规则要动 Rust 的 constraints.rs，前端改不了</div>
            </div>
          </details>
        )}
      </section>

      {violations.length > 0 && (
        <div className="alert bad pk-alert">
          <span>⚠️ {violations.length} 条约束冲突：{violations.map((v) => v.message).join('；')}</span>
          <button onClick={autoFix}>自动修正</button>
        </div>
      )}

      <section className="card">
        <h2>
          分组看板
          {balance && (
            <span className="pk-spread">
              极差比 {spreadPct.toFixed(2)}% · 最高 {wan(balance.max)} · 最低 {wan(balance.min)} · 平均{' '}
              {wan(balance.mean)}
            </span>
          )}
        </h2>
        <div className="row">
          <button disabled={!players.length || loading} onClick={regroup}>
            重新分组
          </button>
          <button disabled={!buckets.length || busy === 'polish'} onClick={polish}>
            {busy === 'polish' ? '微调中…' : '均衡微调'}
          </button>
          <button disabled={!past.length} onClick={undo}>
            撤销
          </button>
          <button disabled={!future.length} onClick={redo}>
            重做
          </button>
          <span className="hint">
            {buckets.length} 组 · {totalPlayers} 人 · 锁定 {locked.length} 人
            {unassigned.length > 0 && ` · 未分组 ${unassigned.length}`}
          </span>
          {buckets.length > 0 && (
            <span className="hint">鼠标直接拖卡片 · 触屏按住左侧把手 ⋮⋮ 拖 · 也可用卡片上的 ⋯ 菜单换组 · Esc 取消</span>
          )}
        </div>

        {/* 强度位次的下拉已经挪到上面「策略」旁边，这里只留达成情况 */}
        <div className="row pk-strength">
          <span className="hint">
            {sequential
              ? '梯队式按实力切块，不套强度位次（内置条件照样优先：固定位/互斥/间隔都先满足）'
              : strengthOn
                ? `强度位次「${STRENGTH_MODES.find((m) => m.value === strengthMode)?.label}」：强制条件先满足，再按${
                    strengthMode === 'top' ? '各组头牌' : '各组总音浪'
                  }排成「${orderText}」${strengthHit === null ? '' : strengthHit ? ' · 已达成 ✅' : ' · 未完全达成（约束挡住了）⚠️'}`
                : '强度位次已关闭：只做均衡，各组实力平均'}
          </span>
        </div>

        <div className="row pk-strength">
          <label>
            {revivalEvent
              ? revivalOn
                ? `第 ${restAfterGroup} 组后复活赛`
                : '复活赛'
              : `第 ${BREAK_AFTER_GROUP} 组后休息`}{' '}
            <input
              type="number"
              min={0}
              max={120}
              style={{ width: 64 }}
              value={restMin}
              disabled={revivalEvent}
              title={
                revivalEvent
                  ? `复活赛固定 20 分钟${
                      revivalOn
                        ? `，位置由组数决定（${buckets.length} 组 → 第 ${restAfterGroup} 组后）`
                        : '，位置分完组后自动定（8 组 / 7 组 → 第 4 组后，6 组 → 第 3 组后）'
                    }，不可改`
                  : '月终争霸赛 / 晋级赛可自己填，填 0 = 不插加长休息'
              }
              onChange={(e) => setLongBreak(Math.max(0, Number(e.target.value) || 0))}
            />
            分钟
            {revivalEvent && <span className="pk-tag">固定</span>}
          </label>
          <span className="hint">
            {revivalEvent
              ? revivalOn
                ? `复活赛只打一场、20 分钟：共 ${buckets.length} 组 → 第 ${restAfterGroup} 组 ${slotAt(
                    restAfterGroup - 1,
                  )} 开赛，紧接着打复活赛，第 ${restAfterGroup + 1} 组顺延到 ${slotAt(restAfterGroup)}`
                : '复活赛只打一场、20 分钟，落在「上半程最后一组」之后：8 组 / 7 组 → 第 4 组后，6 组 → 第 3 组后；分完组这里会自动定位'
              : restMin > 0
                ? `第 1 组 ${slotAt(0)}、第 2 组 ${slotAt(1)}、第 3 组 ${slotAt(2)}，之后休息 ${restMin} 分钟，第 4 组 ${slotAt(3)}`
                : `每组间隔统一 ${intervalMin} 分钟（不插加长休息）`}
          </span>
        </div>

        {buckets.length === 0 ? (
          <p className="hint">还没有分组，先在上面解析名单</p>
        ) : (
          <div className="pk-board">
            {buckets.map((b, gi) => {
              const total = b.players.reduce((s, p) => s + (p.wave || 0), 0);
              const bad = violations.some((v) => v.groupIndex === gi);
              return (
                <div
                  key={b.id}
                  data-bucket={b.id}
                  className={
                    'pk-group' +
                    (bad ? ' bad' : '') +
                    (drag && drag.bucketId === b.id ? ' drop-active' : '')
                  }
                >
                  <div className="pk-group-head">
                    <b>{b.name}</b>
                    <span className="pk-slot">{slots[gi] || '—'}</span>
                    <span className="pk-total">
                      {b.players.length} 人 · {wan(total)}
                    </span>
                  </div>
                  <div className="pk-list">
                    {b.players.map((p, pi) => {
                      const isBad = violationNames.has(p.name);
                      const isLocked = locked.includes(p.key);
                      const isTarget = drag?.overKey === p.key;
                      return (
                        <div
                          key={p.key}
                          data-player={p.key}
                          className={
                            'pk-card' +
                            (isBad ? ' bad' : '') +
                            (isLocked ? ' locked' : '') +
                            (drag?.key === p.key ? ' dragging' : '') +
                            (isTarget ? (drag?.after ? ' drop-after' : ' drop-before') : '')
                          }
                          onPointerDown={(e) => startDrag(e, p)}
                        >
                          <span className="pk-grip" data-grip title="按住拖动（触屏从这里起拖）">
                            ⋮⋮
                          </span>
                          <span className="pk-idx">{String(pi + 1).padStart(2, '0')}</span>
                          <span className="pk-name" title={p.name}>
                            {p.name}
                            {p.leader && <span className="pk-leader">{p.leader}</span>}
                            {!p.matched && <span className="pk-leader warn">识别不到</span>}
                          </span>
                          <span className="pk-wave">{wan(p.wave || 0)}</span>
                          {/* 操作收进单 ⋯ 按钮（原版 PlayerCard 做法）：不常驻占宽，
                              姓名才有空间完整显示。锁定时按钮显示 🔒 提示状态。 */}
                          <span className="pk-ops" onPointerDown={(e) => e.stopPropagation()}>
                            <button
                              className={`pk-ico more${isLocked ? ' on' : ''}`}
                              title={isLocked ? '锁定中 · 点开管理' : '更多操作'}
                              onClick={(e) => {
                                e.stopPropagation();
                                setMenuFor(menuFor === p.key ? null : p.key);
                              }}
                            >
                              {isLocked ? '🔒' : '⋯'}
                            </button>
                            {menuFor === p.key && (
                              <span className="pk-menu" onClick={(e) => e.stopPropagation()}>
                                <button
                                  onClick={() => {
                                    toggleLock(p.key);
                                    setMenuFor(null);
                                  }}
                                >
                                  {isLocked ? '🔓 解锁（允许重排）' : '🔒 锁定（不参与重排）'}
                                </button>
                                <span className="pk-menu-label">移到其它组</span>
                                <span className="pk-menu-groups">
                                  {buckets.map((x) => (
                                    <button
                                      key={x.id}
                                      className={x.id === b.id ? 'cur' : ''}
                                      onClick={() => {
                                        moveCard(p.key, x.id, null);
                                        setMenuFor(null);
                                      }}
                                    >
                                      {x.id === b.id ? `✓ ${x.name}` : x.name}
                                    </button>
                                  ))}
                                </span>
                                <button
                                  className="danger"
                                  onClick={() => {
                                    removeCard(p.key);
                                    setMenuFor(null);
                                  }}
                                >
                                  ✕ 移出看板
                                </button>
                              </span>
                            )}
                          </span>
                        </div>
                      );
                    })}
                    {b.players.length === 0 && <div className="pk-empty">拖人进来</div>}
                  </div>
                </div>
              );
            })}
          </div>
        )}

        {unassigned.length > 0 && (
          <div className="pk-unassigned">
            <b>未分组（{unassigned.length}）</b>
            <div className="row">
              {unassigned.map((p) => (
                <span key={p.key} className="pk-chip">
                  {p.name}
                  <select
                    value=""
                    onChange={(e) => e.target.value && assignTo(e.target.value, p.key)}
                  >
                    <option value="">放到…</option>
                    {buckets.map((b) => (
                      <option key={b.id} value={b.id}>
                        {b.name}
                      </option>
                    ))}
                  </select>
                </span>
              ))}
            </div>
          </div>
        )}

        <div className="row">
          <input
            placeholder="定稿后补一个人，输入名字"
            value={addName}
            onChange={(e) => setAddName(e.target.value)}
          />
          <select value={addTo || buckets[0]?.id || ''} onChange={(e) => setAddTo(e.target.value)}>
            {buckets.map((b) => (
              <option key={b.id} value={b.id}>
                {b.name}
              </option>
            ))}
          </select>
          <button disabled={!addName.trim() || busy === 'add'} onClick={addPerson}>
            {busy === 'add' ? '查询中…' : '加入该组'}
          </button>
          <span className="hint">加人不重排整张看板，插在组尾</span>
        </div>
      </section>

      <section className="card">
        <h2>
          方案{planId && <span className="hint"> · 当前 #{planId}</span>}
        </h2>
        <div className="row">
          <input
            placeholder="方案标题"
            value={title}
            style={{ minWidth: 220 }}
            onChange={(e) => setTitle(e.target.value)}
          />
          <button className="primary" disabled={busy === 'save'} onClick={savePlanFn}>
            {busy === 'save' ? '保存中…' : '保存方案'}
          </button>
          <button disabled={busy === 'export'} onClick={exportImage}>
            {busy === 'export' ? '导出中…' : '导出长图'}
          </button>
        </div>
        <div className="row">
          <label className="pk-check">
            <input
              type="checkbox"
              checked={display.groups}
              onChange={(e) => setDisplay({ ...display, groups: e.target.checked })}
            />
            展示页公开分组名单
          </label>
          <label className="pk-check">
            <input
              type="checkbox"
              checked={display.wave}
              onChange={(e) => setDisplay({ ...display, wave: e.target.checked })}
            />
            展示音浪数值
          </label>
          <label className="pk-check">
            <input
              type="checkbox"
              checked={display.ranking}
              onChange={(e) => setDisplay({ ...display, ranking: e.target.checked })}
            />
            展示总排行
          </label>
          <label className="pk-check">
            <input
              type="checkbox"
              checked={display.daily}
              onChange={(e) => setDisplay({ ...display, daily: e.target.checked })}
            />
            展示每日明细
          </label>
          <span className="hint">关掉后对外接口不下发该类数据，不是前端藏</span>
        </div>

        {eventRule && (
          <details className="pk-meta">
            <summary>赛制说明（会随导出长图一起打出来）</summary>
            <div className="pk-meta-body">
              <div style={{ fontWeight: 700 }}>{eventRule.title}</div>
              <div style={{ marginTop: 6, display: 'grid', gap: 4 }}>
                {eventRule.items.map((t, i) => (
                  <div key={t}>
                    {i + 1}. {t}
                  </div>
                ))}
              </div>
              <div className="hint">规则内置在 apps/douyin-web/src/pk/rules.ts，改文案改那里</div>
            </div>
          </details>
        )}

        {plans.length === 0 ? (
          <p className="hint">还没有保存过方案</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>标题</th>
                <th>赛事</th>
                <th>口径</th>
                <th>时间</th>
                <th>状态</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {plans.map((p) => (
                <tr key={p.id}>
                  <td>
                    {p.title}
                    {planId === p.id && <span className="pk-tag">当前</span>}
                  </td>
                  <td>{p.eventType || '—'}</td>
                  <td>{metricLabel(p.metric, p.days)}</td>
                  <td className="hint">{String(p.created_at).slice(0, 16).replace('T', ' ')}</td>
                  <td>{p.published ? <span className="ok">已上架</span> : '未上架'}</td>
                  <td>
                    <button disabled={busy === 'load'} onClick={() => loadPlan(p.id)}>
                      载入
                    </button>
                    <button onClick={() => publish(p.id, !p.published)}>
                      {p.published ? '下架' : '上架'}
                    </button>
                    <button className="danger" onClick={() => removePlan(p.id)}>
                      删除
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>

      {/* 跟手的幽灵卡片：原来只有源卡片半透明，眼睛跟不上人去哪了 */}
      {drag && (
        <div
          className="pk-ghost"
          style={{ transform: `translate(${drag.x - 28}px, ${drag.y - 16}px)` }}
        >
          {drag.name}
          <span className="pk-wave">{wan(drag.wave)}</span>
        </div>
      )}

      {error && <div className="alert bad">{error}</div>}
      {msg && <div className="alert good">{msg}</div>}
    </div>
  );
}
