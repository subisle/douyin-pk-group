import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  EVENT_TYPES,
  METRICS,
  STRATEGIES,
  STRENGTH_MODES,
  metricOfStrategy,
  pk,
  type BoardConfig,
  type EventType,
} from './api';
import { REVIVAL_MINUTES, revivalAfterGroup } from './rules';
import { registerLeaveGuard } from '../lib/router';
import type {
  BalanceStat,
  BreakRule,
  Bucket,
  InspectResponse,
  Player,
  Strategy,
  StrengthMode,
  Violation,
} from './types';

/* ============ 工具（与 GroupPage 同款，只留展板用得到的） ============ */

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
 *  复活赛是**一场 20 分钟的比赛**（不是休息），只打一场，落在第 ceil(组数/2) 组之后；
 *  位置和时长都不可改，longBreak 配置对这类赛事不生效。 */
const REVIVAL_EVENTS: readonly EventType[] = ['初级争霸赛小组赛', '中级争霸赛小组赛'];

/** 后端没存过配置时的兜底值：与 GroupPage 首屏默认一致，别让展板算出非法参数。 */
const DEFAULT_BOARD_CONFIG: BoardConfig = {
  eventType: '初级争霸赛小组赛',
  metric: 'lastN',
  days: 3,
  strategy: 'snake',
  strengthMode: 'top',
  groupSize: 8,
  startTime: '12:15',
  intervalMin: 15,
  longBreak: 20,
};

/** 撤销快照：展板没有锁定/未分组区，只需记住 buckets 本身 */
interface Snapshot {
  buckets: Bucket[];
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

export default function BoardPage() {
  /* ---------------- 配置（整包一份，与后端 board-config 契约一一对应） ---------------- */
  const [config, setConfig] = useState<BoardConfig>(DEFAULT_BOARD_CONFIG);

  /* ---------------- 数据 ---------------- */
  const [buckets, setBuckets] = useState<Bucket[]>([]);
  const [balance, setBalance] = useState<BalanceStat | null>(null);
  const [violations, setViolations] = useState<Violation[]>([]);
  const [slots, setSlots] = useState<string[]>([]);
  /** 名单来源提示用的参赛人数（一键分组时记录） */
  const [rosterTotal, setRosterTotal] = useState(0);

  /* ---------------- 交互状态 ---------------- */
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const [msg, setMsg] = useState('');
  const [past, setPast] = useState<Snapshot[]>([]);
  const [future, setFuture] = useState<Snapshot[]>([]);
  /** 名字飞入动画（2026-10-16 用户口径「点击一键分组后有特效，名字随机飞入分组框」）：
      groupNow 成功后置 true，动画只播这一轮（replayKey 变化重触发）；拖动开始即停。 */
  const [flyIn, setFlyIn] = useState(false);
  const [flyKey, setFlyKey] = useState(0);
  /* 拖拽：指针事件实现（鼠标 / 触屏 / 触控笔同一套），从 GroupPage 原样搬来——
     不用 HTML5 draggable，后者在触屏上完全不触发，也没有跟手的幽灵卡片。 */
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

  /* 初始化：拉后端已保存的配置。没存过就保持默认值，等运营第一次点「保存配置」。 */
  useEffect(() => {
    pk.getBoardConfig()
      .then((r) => {
        if (r.saved && r.config) setConfig({ ...DEFAULT_BOARD_CONFIG, ...r.config });
      })
      .catch(() => {}); // 拉不到配置不挡页面，一键分组时还会再拉一次并给出明确报错
  }, []);

  /* P1-12 同款：看板上有分组内容时，切页/刷新前都拦一道确认。
     展板的数据不落库（没有「保存方案」），一次手滑就是全部重排，所以更要防。 */
  useEffect(() => {
    const unregister = registerLeaveGuard(() => {
      if (buckets.length === 0) return null;
      return 'PK 展板上有未保存的分组';
    });
    return unregister;
  }, [buckets.length]);

  useEffect(() => {
    const onBeforeUnload = (e: BeforeUnloadEvent) => {
      if (buckets.length === 0) return;
      e.preventDefault();
      e.returnValue = '';
    };
    window.addEventListener('beforeunload', onBeforeUnload);
    return () => window.removeEventListener('beforeunload', onBeforeUnload);
  }, [buckets.length]);

  /* ---------------- 配置派生 ---------------- */
  /** 本赛事有没有复活赛（初级/中级争霸赛小组赛）：有则加长休息就是固定的 20 分钟复活赛 */
  const revivalEvent = REVIVAL_EVENTS.includes(config.eventType);
  const sequential = config.strategy === 'sequential' || config.strategy === 'sequential-last-n';

  /** 赛程里的加长休息点。groupCount 是分组数：
   *  - 小组赛的复活赛落点随组数走（ceil(组数/2)），20 分钟固定；
   *  - 其余赛事固定第 3 组后，分钟数用配置里的 longBreak（0 = 不插）。 */
  const breaksFor = useCallback(
    (groupCount: number): BreakRule[] => {
      if (revivalEvent) {
        const after = revivalAfterGroup(groupCount);
        return after > 0 ? [{ afterGroup: after, minutes: REVIVAL_MINUTES }] : [];
      }
      return config.longBreak > 0 ? [{ afterGroup: BREAK_AFTER_GROUP, minutes: config.longBreak }] : [];
    },
    [revivalEvent, config.longBreak],
  );

  /** 分组 / 体检都带上的约束：赛程（含加长休息）+ 强度位次形态。
   *  groupCount 传当前组数：复活赛落点随组数变，inspect 时带重算过的 breaks。 */
  const constraintsFor = useCallback(
    (groupCount: number) => ({
      schedule: {
        start: config.startTime,
        intervalMin: config.intervalMin,
        breaks: breaksFor(groupCount),
      },
      strengthOrder: {
        enabled: config.strengthMode !== 'off',
        mode: config.strengthMode !== 'off' ? (config.strengthMode as StrengthMode) : undefined,
      },
    }),
    [config.startTime, config.intervalMin, config.strengthMode, breaksFor],
  );

  /* ---------------- 体检（拖拽后重算均衡与冲突） ---------------- */
  /* P1-13 同款：inspect 并发序号。连拖两次会并发两个请求，旧请求后到就会把组头
     开赛时间刷回旧分组的 slots。自增序号只认最后一次发出的请求。 */
  const inspectSeqRef = useRef(0);
  const inspectInflightRef = useRef<Promise<InspectResponse | null> | null>(null);

  const inspectNow = useCallback(
    async (next: Bucket[]) => {
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
        .inspect({ groups: next.map((b) => b.players), constraints: constraintsFor(next.length) })
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
    },
    [constraintsFor],
  );

  /* ---------------- 撤销 / 重做 ---------------- */
  const pushHistory = useCallback((snap?: Snapshot) => {
    const cur: Snapshot = snap ?? { buckets: clone(buckets) };
    setPast((p) => [...p.slice(-29), cur]);
    setFuture([]);
  }, [buckets]);

  const applyGroups = useCallback(
    (next: Bucket[], history = true) => {
      if (history) pushHistory();
      setBuckets(next);
      void inspectNow(next);
    },
    [pushHistory, inspectNow],
  );

  function undo() {
    const prev = past[past.length - 1];
    if (!prev) return;
    setPast((p) => p.slice(0, -1));
    setFuture((f) => [...f, { buckets: clone(buckets) }]);
    setBuckets(prev.buckets);
    void inspectNow(prev.buckets);
  }

  function redo() {
    const next = future[future.length - 1];
    if (!next) return;
    setFuture((f) => f.slice(0, -1));
    setPast((p) => [...p, { buckets: clone(buckets) }]);
    setBuckets(next.buckets);
    void inspectNow(next.buckets);
  }

  /* ---------------- 配置表单 ---------------- */
  function update(patch: Partial<BoardConfig>) {
    setConfig((c) => ({ ...c, ...patch }));
  }

  /** 换策略。梯队式（sequential / sequential-last-n）的排序依据只有一列 wave，
   *  口径错了梯队就错了，所以像 GroupPage.changeStrategy 一样把口径一起锁掉。 */
  function changeStrategy(s: Strategy) {
    const m = metricOfStrategy(s);
    setConfig((c) => (m ? { ...c, strategy: s, metric: m } : { ...c, strategy: s }));
  }

  /* ---------------- 一键分组（展板唯一的主流程） ----------------
     配置（后端已保存）→ 名单（PK 分组页已保存）→ 解析 → 分组，一条链跑完。 */
  async function groupNow() {
    setLoading(true);
    setError('');
    setMsg('');
    try {
      // 1. 配置必须已保存：展板自己不配参数入组，没配置就没法分组
      const cfg = await pk.getBoardConfig();
      if (!cfg.saved || !cfg.config) {
        throw new Error('展板配置还没保存：回后台「PK 分组 → 展板配置」卡保存后再来分组');
      }
      const c = { ...DEFAULT_BOARD_CONFIG, ...cfg.config };
      setConfig(c);

      // 2. 名单只在「PK 分组」页维护，展板只读不写——这是唯一依赖外部页面的地方
      const r = await pk.getRoster(c.eventType);
      if (!r.saved || !r.text) {
        throw new Error(`「PK 分组」页还没有保存「${c.eventType}」的名单，先去那里保存`);
      }

      // 3. 解析：取可参赛的人。识别不到的行不阻断（分组照跑），但要点名提醒
      const parsed = await pk.parse({ text: r.text, metric: c.metric, days: c.days });
      const active = parsed.players.filter((p) => !p.excluded);
      if (!active.length) throw new Error('名单里没有可参赛的人');
      const unmatchedN = parsed.unmatched.length;

      // 4. 分组。复活赛落点随组数变：arrange 先按「人数 ÷ 每组人数」估个组数把 breaks 算对，
      //    分完组后 inspect 再带真实的组数重算一遍（GroupPage 同一套口径）
      const estimatedGroups = Math.ceil(active.length / c.groupSize);
      const arranged = await pk.arrange({
        players: active,
        groupSize: c.groupSize,
        strategy: c.strategy,
        constraints: constraintsFor(estimatedGroups),
      });
      const next = toBuckets(arranged.groups);
      setPast([]);
      setFuture([]);
      setRosterTotal(active.length);
      setBuckets(next);
      setBalance(arranged.balance);
      setViolations(arranged.violations || []);
      setSlots(arranged.slots || []);
      // 特效：名字随机飞入分组框（每轮换 key 重触发；首次进入不播，重分组才播）
      setFlyKey((k) => k + 1);
      setFlyIn(true);
      setMsg(
        `已分组 ${active.length} 人，共 ${arranged.groups.length} 组` +
          (unmatchedN > 0 ? ` · ${unmatchedN} 行识别不到，去 PK 分组页处理` : ''),
      );
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setLoading(false);
    }
  }

  /* ---------------- 看板操作：拖拽落位 ---------------- */
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

  /* ---------------- 拖拽实现（GroupPage 原样搬来） ---------------- */
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
    // 起手阈值：6px 以内算点击，不然卡片上的其它交互就没法点了
    if (!d.active) {
      if (Math.hypot(e.clientX - d.startX, e.clientY - d.startY) < 6) return;
      d.active = true;
      document.body.classList.add('pk-dragging');
    }
    d.x = e.clientX;
    d.y = e.clientY;
    // 拖到视口上下边缘时自动滚，十几组的看板一屏装不下
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

  /* ---------------- 派生 ---------------- */
  const totalPlayers = useMemo(() => buckets.reduce((s, b) => s + b.players.length, 0), [buckets]);

  const spreadPct = balance && balance.max ? ((balance.max - balance.min) / balance.max) * 100 : 0;

  const violationNames = useMemo(() => {
    const set = new Set<string>();
    for (const v of violations) for (const n of v.names) set.add(n);
    return set;
  }, [violations]);

  return (
    <div className="page pk">
      {/* ② 一键分组：展板唯一的主按钮 */}
      <div className="pk-board-primary-row">
        <button className="primary pk-board-primary" disabled={loading} onClick={groupNow}>
          {loading ? '分组中…' : '一键分组'}
        </button>
        {buckets.length > 0 && (
          <span className="hint">
            名单来自「PK 分组」页已保存的「{config.eventType}」名单 · 共 {rosterTotal} 人
          </span>
        )}
      </div>

      {violations.length > 0 && (
        <div className="alert bad pk-alert">
          <span>
            ⚠️ {violations.length} 条约束冲突：{violations.map((v) => v.message).join('；')}
          </span>
        </div>
      )}

      {/* ③ 分组看板：大卡片展示 + 拖人调整 */}
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
          <button disabled={!past.length} onClick={undo}>
            撤销
          </button>
          <button disabled={!future.length} onClick={redo}>
            重做
          </button>
          <span className="hint">
            {buckets.length} 组 · {totalPlayers} 人
          </span>
          {buckets.length > 0 && (
            <span className="hint">鼠标直接拖卡片 · 触屏按住左侧把手 ⋮⋮ 拖 · Esc 取消 · 拖完自动重算</span>
          )}
        </div>

        {buckets.length === 0 ? (
          <p className="hint">还没有分组，点上面的「一键分组」</p>
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
                      const isTarget = drag?.overKey === p.key;
                      return (
                        <div
                          key={p.key}
                          data-player={p.key}
                          className={
                            'pk-card' +
                            (isBad ? ' bad' : '') +
                            (drag?.key === p.key ? ' dragging' : '') +
                            (isTarget ? (drag?.after ? ' drop-after' : ' drop-before') : '') +
                            (flyIn && !drag ? ' pk-fly-in' : '')
                          }
                          style={
                            flyIn && !drag
                              ? ({
                                  /* 随机飞入：水平起点 -60~60px、垂直起点 -80~-160px，延迟 0~0.5s
                                     —— 每张卡独立随机，重分组换 flyKey 重新随机 */
                                  '--fly-x': `${(Math.random() * 120 - 60).toFixed(0)}px`,
                                  '--fly-y': `${(-(Math.random() * 80 + 80)).toFixed(0)}px`,
                                  animationDelay: `${(Math.random() * 0.5).toFixed(2)}s`,
                                } as React.CSSProperties)
                              : undefined
                          }
                          onAnimationEnd={(e) => {
                            if (flyIn && e.animationName === 'pkFlyIn') setFlyIn(false);
                          }}
                          onPointerDown={(e) => {
                            if (flyIn) setFlyIn(false); // 拖动开始：特效立刻收
                            startDrag(e, p);
                          }}
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
      </section>

      {/* 跟手的幽灵卡片：只有源卡片半透明的话，眼睛跟不上人去哪了 */}
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
