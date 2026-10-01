import { useCallback, useMemo, useState } from 'react';
import {
  DndContext,
  DragOverlay,
  PointerSensor,
  TouchSensor,
  pointerWithin,
  rectIntersection,
  useDroppable,
  useSensor,
  useSensors,
  type CollisionDetection,
  type DragEndEvent,
  type DragStartEvent,
  type DroppableContainer,
} from '@dnd-kit/core';
import {
  SortableContext,
  rectSortingStrategy,
  useSortable,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { formatWave, isPinnedName, resolvePinnedIndex, slotTime, type Player } from '@pk/shared';
import type { Bucket, Violation } from '../types.js';
import { useStore } from '../store.js';
import { SortablePlayerCard, DragPreview, type CardMenuData } from './PlayerCard.js';
import { AlertIcon, ClockIcon, CloseIcon, GripIcon, PinIcon } from './icons.js';
import { buildRankContext, gapLine, groupColor } from '../lib/board.js';

/** 组列的两种身份：col- 是「把人放进这组」的落点，grp- 是组列本身（可拖动排序） */
const COL_PREFIX = 'col-';
const GRP_PREFIX = 'grp-';

/** id 里带组前缀就还原成 bucket id，否则返回 null（表示这是个主播卡片 id） */
function bucketIdOf(id: string): string | null {
  if (id.startsWith(COL_PREFIX)) return id.slice(COL_PREFIX.length);
  if (id.startsWith(GRP_PREFIX)) return id.slice(GRP_PREFIX.length);
  return null;
}

function bucketOf(buckets: Bucket[], id: string): Bucket | undefined {
  const bid = bucketIdOf(id);
  if (bid !== null) return buckets.find((b) => b.id === bid);
  return buckets.find((b) => b.players.some((p) => p.key === id));
}

/** 一次拖拽的标的：kind 决定按「人」还是按「组」落位 */
interface ActiveDrag {
  kind: 'player' | 'group';
  id: string;
}

/**
 * 落点判定。姓名卡和组列共用一个 DndContext，所以先按「正在拖什么」过滤类型，
 * 再算命中。
 *
 * 用 pointerWithin（指针压在谁身上）而不是 closestCorners（角点距离）：
 * 拖拽时代理卡自身不做位移（见 PlayerCard 里 isDragging 时清掉 transform），
 * dnd-kit 记的拖拽矩形停在原处，角点距离会算到别的列去 —— 实测指针点在
 * 第 2 组，松手却落进了第 6 组。
 *
 * pointerWithin 一无所获时（指针落在组与组的空隙里）退回矩形相交，
 * 保证总有落点而不是直接丢掉这次拖拽。
 */
const collisionDetection: CollisionDetection = (args) => {
  const { active, droppableContainers } = args;
  const draggingGroup = active.data.current?.type === 'group';
  const keep = (c: DroppableContainer) => (c.data.current?.type === 'group') === draggingGroup;
  const candidates = droppableContainers.filter(keep);

  const hits = pointerWithin({ ...args, droppableContainers: candidates }).filter(
    (c) => c.id !== active.id,
  );
  if (hits.length > 0) {
    // 卡片和它所在的组列同时命中时，卡片更具体，优先用卡片定插入位
    const cards = hits.filter((c) => c.data?.current?.type !== 'player-dest');
    return cards.length > 0 ? cards : hits;
  }
  return rectIntersection({ ...args, droppableContainers: candidates });
};

export function GroupBoard({
  buckets,
  highlight,
  dense,
}: {
  buckets: Bucket[];
  /** 搜索命中的 player.key 集合 */
  highlight?: Set<string>;
  /** 紧凑模式：一屏尽量多看几个人 */
  dense?: boolean;
}) {
  const moveCard = useStore((s) => s.moveCard);
  const moveBucketOver = useStore((s) => s.moveBucketOver);
  const beginDrag = useStore((s) => s.beginDrag);
  const endDrag = useStore((s) => s.endDrag);
  const startTime = useStore((s) => s.startTime);
  const intervalMin = useStore((s) => s.intervalMin);
  const constraints = useStore((s) => s.constraints);
  const violations = useStore((s) => s.violations);
  const locked = useStore((s) => s.locked);
  const toggleLock = useStore((s) => s.toggleLock);
  const [active, setActive] = useState<ActiveDrag | null>(null);
  /**
   * 拖拽中 overlay 的尺寸，必须和原节点一致。
   *
   * overlay 挂在 DndContext 根下的 fixed 容器里，那个容器是 shrink-to-fit 的，
   * 所以 CARD_SHELL 的 w-full 在这里退化成内容宽度。原卡片是撑满组列的，
   * 一旦 overlay 变窄，从卡片右侧抓起时光标就落到框外了——看着像「鼠标离框很远」，
   * 而且 pointerWithin 按光标判命中，光标和看到的框错位，投递也跟着偏远。
   */
  const [activeSize, setActiveSize] = useState<number | null>(null);
  const cs = useMemo(() => constraints(), [constraints]);
  const schedule = { start: startTime, intervalMin };

  /** 每组违反的内置条件（后端算好，用于卡片高亮和组头文案） */
  const violationsByGroup = useMemo(() => {
    const m = new Map<number, Violation[]>();
    for (const v of violations) {
      m.set(v.groupIndex, [...(m.get(v.groupIndex) ?? []), v]);
    }
    return m;
  }, [violations]);

  const assignPlayer = useStore((s) => s.assignPlayer);
  const swapPlayers = useStore((s) => s.swapPlayers);
  const unassign = useStore((s) => s.unassign);

  /** 卡片操作菜单的公共部分：目标组 / 可交换的人 */
  const menuBase = useMemo(() => {
    const groups: Array<[number, string]> = buckets.map((b, i) => [i, b.name]);
    const swapTargets = buckets.flatMap((b, gi) =>
      b.players.map((p) => ({ key: p.key, name: p.name, groupIndex: gi })),
    );
    return {
      groups,
      swapTargets,
      onToggleLock: (key: string) => toggleLock(key),
      onMove: (key: string, gi: number) => {
        const target = buckets[gi];
        if (target) assignPlayer(key, target.id);
      },
      onSwap: (key: string, other: string) => swapPlayers(key, other),
      onUnassign: (key: string) => unassign(key),
    };
  }, [buckets, assignPlayer, swapPlayers, unassign, toggleLock]);

  /** 每组被钉住的人 */
  const pinnedByGroup = useMemo(() => {
    const m = new Map<number, string[]>();
    for (const r of cs.pinned) {
      const i = resolvePinnedIndex(r, buckets.length);
      m.set(i, [...(m.get(i) ?? []), r.name]);
    }
    return m;
  }, [cs, buckets.length]);

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
    useSensor(TouchSensor, { activationConstraint: { delay: 150, tolerance: 5 } }),
  );

  const rank = useMemo(() => buildRankContext(buckets), [buckets]);
  /** 组列在 SortableContext 里的 id 列表：memo 住，避免每次渲染换新数组 */
  const groupOrder = useMemo(() => buckets.map((b) => GRP_PREFIX + b.id), [buckets]);

  // 每组人数上限：跟着「每组人数」配置走，超员是分组时最常见的问题，直接在组头标出来
  const capacity = useStore((s) => s.groupSize);
  const overCount = useMemo(
    () => buckets.filter((b) => b.players.length > capacity).length,
    [buckets, capacity],
  );

  const mean = useMemo(() => {
    const totals = buckets.map((b) => b.players.reduce((s, p) => s + p.wave, 0));
    return totals.length ? totals.reduce((a, b) => a + b, 0) / totals.length : 0;
  }, [buckets]);

  /**
   * 落位。只在 onDragEnd 调一次。
   *
   * 为什么拖动过程中不实时搬（原来 onDragOver 会在每次指针移动时搬）：
   * 跨组搬运会让「正在被拖的那张卡」换掉 DOM 父节点，dnd-kit 记的拖拽矩形
   * 随之错位，命中判定就在两个组之间来回弹。实测一次拖拽触发 28 次
   * moveCard + 28 次 refreshDerived，GroupBoard 重渲染 58 次，最后撞上
   * React 的嵌套更新上限 —— 报 #185（Maximum update depth exceeded），
   * 整个看板白屏，表现为「一拖姓名就坏掉」。
   *
   * 拖动过程中的反馈交给 useDroppable 的 isOver（组列会亮描边），
   * 落位则等松手时一次做完，行为可预期也不会抖。
   */
  const applyDrop = (activeId: string, overId: string, kind: 'player' | 'group') => {
    if (kind === 'group') {
      const from = bucketIdOf(activeId);
      const to = bucketIdOf(overId);
      if (from && to && from !== to) moveBucketOver(from, to);
      return;
    }
    const from = bucketOf(buckets, activeId);
    const to = bucketOf(buckets, overId);
    if (!from || !to) return;
    // over 是组列时没有具体落点，交给 moveCard 追加到该组末尾
    moveCard(activeId, to.id, bucketIdOf(overId) === null ? overId : null);
  };

  const onDragStart = (e: DragStartEvent) => {
    setActiveSize(e.active.rect.current?.initial?.width ?? null);
    setActive({ kind: e.active.data.current?.type === 'group' ? 'group' : 'player', id: String(e.active.id) });
    beginDrag();
  };

  const onDragEnd = (e: DragEndEvent) => {
    // 这里不再区分同组 / 跨组：同组换顺序与跨组搬人都要落到目标位
    if (e.over) applyDrop(String(e.active.id), String(e.over.id), e.active.data.current?.type === 'group' ? 'group' : 'player');
    endDrag();
    setActive(null);
    setActiveSize(null);
  };

  const onDragCancel = () => {
    endDrag();
    setActive(null);
    setActiveSize(null);
  };

  const activeId = active?.id ?? null;
  const activePlayer =
    active?.kind === 'player' ? buckets.flatMap((b) => b.players).find((p) => p.key === activeId) : null;
  const activeIndex = activePlayer
    ? (buckets.find((b) => b.players.some((p) => p.key === activeId))?.players.findIndex((p) => p.key === activeId) ?? 0)
    : 0;
  const activeBucket = active?.kind === 'group' ? buckets.find((b) => GRP_PREFIX + b.id === activeId) : undefined;
  const activeBucketIndex = activeBucket ? buckets.findIndex((b) => b.id === activeBucket.id) : 0;

  return (
    <DndContext
      sensors={sensors}
      collisionDetection={collisionDetection}
      onDragStart={onDragStart}
      onDragEnd={onDragEnd}
      onDragCancel={onDragCancel}
    >
      {overCount > 0 && (
        <div className="mb-3 flex flex-wrap items-center gap-2 rounded-card border border-warn-soft bg-warn-soft px-3.5 py-2.5 text-[12.5px] text-warn-ink">
          <AlertIcon size={15} />
          有 {overCount} 组超过 {capacity} 人上限，超出的人已标在组头上，可以直接移出或交换
        </div>
      )}
      <SortableContext items={groupOrder} strategy={rectSortingStrategy}>
        <div className="grid items-start gap-4" style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(300px, 1fr))' }}>
          {buckets.map((b, i) => (
            <GroupColumn
              key={b.id}
              bucket={b}
              index={i}
              color={groupColor(i)}
              time={slotTime(i, schedule)}
              conflicts={violationsByGroup.get(i) ?? EMPTY_VIOLATIONS}
              pinnedNames={pinnedByGroup.get(i) ?? EMPTY_LIST}
              rank={rank}
              mean={mean}
              menuBase={menuBase}
              highlight={highlight}
              dense={dense}
              capacity={capacity}
            />
          ))}
        </div>
      </SortableContext>
      <DragOverlay>
        {activeBucket ? (
          <div
            className="rounded-panel border-t-[5px] bg-surface p-3 shadow-drag"
            style={{ borderTopColor: groupColor(activeBucketIndex).bar, width: activeSize ?? undefined }}
          >
            <div className="rounded-2xl px-3.5 py-2.5" style={{ background: groupColor(activeBucketIndex).soft }}>
              <div className="truncate text-[15px] font-bold leading-5" style={{ color: groupColor(activeBucketIndex).ink }}>
                {activeBucket.name}
              </div>
              <div className="mt-0.5 text-[11.5px] leading-4" style={{ color: groupColor(activeBucketIndex).ink, opacity: 0.82 }}>
                <span className="nums font-semibold">{activeBucket.players.length}</span> 人 · 拖到别组可换顺序
              </div>
            </div>
          </div>
        ) : activePlayer ? (
          <DragPreview
            player={activePlayer}
            color={groupColor(buckets.findIndex((b) => b.players.some((p) => p.key === activePlayer.key)))}
            index={activeIndex + 1}
            pinned={Boolean(isPinnedName(activePlayer.name, cs))}
            width={activeSize ?? undefined}
          />
        ) : null}
      </DragOverlay>
    </DndContext>
  );
}

const EMPTY_LIST: string[] = [];
const EMPTY_VIOLATIONS: Violation[] = [];

function GroupColumn({
  bucket,
  index,
  color,
  time,
  conflicts,
  pinnedNames,
  rank,
  mean,
  menuBase,
  highlight,
  dense,
  capacity,
}: {
  bucket: Bucket;
  index: number;
  color: ReturnType<typeof groupColor>;
  time: string;
  conflicts: Violation[];
  pinnedNames: string[];
  rank: ReturnType<typeof buildRankContext>;
  mean: number;
  menuBase: Omit<CardMenuData, 'swapTargets' | 'locked'> & { swapTargets: Array<{ key: string; name: string; groupIndex: number }> };
  highlight?: Set<string>;
  dense?: boolean;
  capacity: number;
}) {
  const renameBucket = useStore((s) => s.renameBucket);
  const removeBucket = useStore((s) => s.removeBucket);
  const addToBucket = useStore((s) => s.addToBucket);
  const constraints = useStore((s) => s.constraints);
  const locked = useStore((s) => s.locked);
  const [editing, setEditing] = useState(false);
  const [adding, setAdding] = useState(false);
  const [newName, setNewName] = useState('');
  const [addBusy, setAddBusy] = useState(false);
  // 一个组列同时是两种落点：grp- 供组列之间换位，col- 供把人拖进来。
  // useSortable 自带一个同 id 的 droppable，所以两个 ref 都要挂到同一个 DOM 节点。
  const {
    setNodeRef: setSortableRef,
    attributes,
    listeners,
    transform,
    transition,
    isDragging,
  } = useSortable({ id: GRP_PREFIX + bucket.id, data: { type: 'group' } });
  const { setNodeRef: setDropRef, isOver } = useDroppable({
    id: COL_PREFIX + bucket.id,
    data: { type: 'player-dest' },
  });
  // 这个 ref 回调必须身份稳定：React 19 看到新的 ref 函数会在每次重渲染时
  // 先 ref(null) 再 ref(node)，dnd-kit 那就等于每帧注销+注册一次，
  // 内部 setState 再触发重渲染，直接 Maximum update depth exceeded。
  const attachRef = useCallback(
    (node: HTMLDivElement | null) => {
      setSortableRef(node);
      setDropRef(node);
    },
    [setSortableRef, setDropRef],
  );
  const moveBucketTo = useStore((s) => s.moveBucketTo);
  const bucketCount = useStore((s) => s.buckets.length);
  // 组号输入用草稿态：直接受控写入的话，清空输入框 Number('') = 0，
  // 会被夹成「第 1 组」，手一抖整组就跑到最前面去了。
  const [orderDraft, setOrderDraft] = useState<string | null>(null);
  const commitOrder = () => {
    const raw = orderDraft?.trim();
    setOrderDraft(null);
    if (!raw) return;
    const value = Number(raw);
    if (Number.isFinite(value)) moveBucketTo(bucket.id, value);
  };

  const cs = constraints();
  /**
   * SortableContext 的 items 必须 memo 住。
   * 它内部拿 items 与 previousItemsRef 比对决定要不要重新测量，而 ref 是在
   * passive effect 里更新的；一旦 items 每次渲染都是新数组，layout effect 里
   * 的 setState 会同步触发重渲染，passive effect 还没跑，itemsHaveChanged
   * 恒为 true —— 无限重复测量，最后 React 报 #185（Maximum update depth
   * exceeded），看板直接白屏。拖不动就是被这个卡死的。
   */
  const playerOrder = useMemo(() => bucket.players.map((p) => p.key), [bucket.players]);
  const total = bucket.players.reduce((s, p) => s + p.wave, 0);
  const delta = mean ? ((total - mean) / mean) * 100 : 0;
  const deltaText = `${delta >= 0 ? '+' : ''}${delta.toFixed(1)}%`;
  const empty = bucket.players.length === 0;
  const bad = conflicts.length > 0;
  const over = bucket.players.length > capacity;

  return (
    <div
      ref={attachRef}
      // data-group-col 给 uicheck / e2e 用：靠 Tailwind 类名定位会被无关改动打断
      data-group-col={bucket.name}
      className={`group rounded-panel border-t-[5px] bg-surface p-3 pb-2 shadow-card transition-shadow ${
        bad ? 'ring-2 ring-bad' : isOver ? 'ring-2 ring-brand' : ''
      }`}
      style={{
        borderTopColor: color.bar,
        // 与 PlayerCard 一致：浮层跟着指针走，本体别再跟着位移，否则会错位
        transform: isDragging ? undefined : CSS.Translate.toString(transform),
        transition,
        opacity: isDragging ? 0.45 : 1,
      }}
    >
      {/* 组头：粉彩 pill，与导出长图同款 */}
      <div className="rounded-2xl px-3.5 py-2.5" style={{ background: color.soft }}>
        <div className="flex items-center justify-between gap-1">
          {editing ? (
            <input
              autoFocus
              defaultValue={bucket.name}
              className="h-6 w-full rounded-[4px] border border-white bg-white/80 px-1 text-[14px] font-bold outline-none"
              style={{ color: color.ink }}
              onBlur={(e) => {
                renameBucket(bucket.id, e.target.value.trim() || bucket.name);
                setEditing(false);
              }}
              onKeyDown={(e) => {
                if (e.key === 'Enter') (e.target as HTMLInputElement).blur();
                if (e.key === 'Escape') setEditing(false);
              }}
            />
          ) : (
            <button
              type="button"
              onClick={() => setEditing(true)}
              title="点击重命名"
              className="truncate text-[15px] font-bold leading-5 hover:opacity-80"
              style={{ color: color.ink }}
            >
              {bucket.name}
            </button>
          )}
          {pinnedNames.length > 0 && (
            <span className="flex shrink-0 items-center gap-0.5 text-[11px] opacity-80" title="固定在该组">
              <PinIcon size={11} />
              <span className="truncate">{pinnedNames.join('、')}</span>
            </span>
          )}
          <span
            aria-label="修改组顺序"
            title="输入目标组号，直接调整顺序"
            className="flex h-5 w-8 shrink-0 items-center justify-center rounded-[6px] border border-white bg-white/80 text-[11px] font-bold leading-none text-ink3 focus:border-brand focus:outline-none"
          >
            <input
              type="number"
              min={1}
              max={bucketCount}
              value={orderDraft ?? String(index + 1)}
              className="h-full w-full bg-transparent text-center outline-none"
              onChange={(e) => setOrderDraft(e.target.value)}
              onBlur={commitOrder}
              onKeyDown={(e) => {
                if (e.key === 'Enter') (e.target as HTMLInputElement).blur();
                if (e.key === 'Escape') {
                  setOrderDraft(null);
                  (e.target as HTMLInputElement).blur();
                }
              }}
            />
          </span>
          <button
            type="button"
            aria-label="拖动调整组顺序"
            title="拖动调整组顺序"
            className="flex h-5 w-5 shrink-0 cursor-grab items-center justify-center rounded-full bg-white/75 transition-colors hover:bg-white active:cursor-grabbing"
            style={{ color: color.ink }}
            {...attributes}
            {...listeners}
          >
            <GripIcon size={12} />
          </button>
          <button
            type="button"
            onClick={() => setAdding((v) => !v)}
            title="增补人员到该组"
            className="ml-auto flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-white/75 text-[14px] font-bold leading-none transition-transform hover:scale-110"
            style={{ color: color.ink }}
          >
            {adding ? '×' : '+'}
          </button>
        </div>
        <div className="mt-0.5 flex items-center gap-1.5 text-[11.5px] leading-4" style={{ color: color.ink, opacity: 0.82 }}>
          <ClockIcon size={12} />
          <span className="nums font-medium">{time}</span>
          <span>开赛</span>
          <span>·</span>
          <span className={`nums font-semibold ${over ? 'text-bad' : ''}`} style={over ? { opacity: 1 } : undefined}>
            {bucket.players.length}
            {over && <span className="text-bad">/{capacity}</span>}
          </span>
          <span>人</span>
          {!empty && (
            <>
              <span>·</span>
              <span className="nums font-semibold" style={{ opacity: 1 }}>
                {formatWave(total)}
              </span>
              <span className="nums">({deltaText})</span>
            </>
          )}
        </div>
        {conflicts.length > 0 && (
          <div className="mt-1.5 rounded-[6px] bg-white/70 px-2 py-1 text-[11px] leading-4 text-bad">
            违反内置条件：
            {conflicts.map((v) => (
              <div key={`${v.kind}-${v.groupIndex}-${v.message}`}>{v.message}</div>
            ))}
          </div>
        )}
      </div>

      <SortableContext items={playerOrder} strategy={verticalListSortingStrategy}>
        <div className="mt-2.5 flex min-h-[72px] flex-col gap-1">
          {bucket.players.map((p: Player, i: number) => (
            <SortablePlayerCard
              key={p.key}
              player={p}
              color={color}
              index={i + 1}
              pinned={Boolean(isPinnedName(p.name, cs))}
              conflict={conflicts.some((v) => v.names.includes(p.name))}
              rankLine={gapLine(p, rank.rankOf.get(p.key) ?? 0, rank.waveAt)}
              highlight={highlight?.has(p.key)}
              dense={dense}
              menu={{
                ...menuBase,
                locked: locked.includes(p.key),
                swapTargets: menuBase.swapTargets.filter((t) => t.key !== p.key && t.groupIndex === index)
                  .concat(menuBase.swapTargets.filter((t) => t.key !== p.key && t.groupIndex !== index)),
              }}
            />
          ))}
          {empty && (
            <div className="flex flex-1 items-center justify-center rounded-card border border-dashed border-line2 py-5 text-[11px] text-ink3">
              拖到这里
            </div>
          )}
          {empty && (
            <button
              type="button"
              onClick={() => removeBucket(bucket.id)}
              className="mt-0.5 self-end text-ink3 opacity-0 transition-opacity hover:text-bad group-hover:opacity-100"
              title="删除该空组"
            >
              <CloseIcon size={13} />
            </button>
          )}
        </div>
      </SortableContext>

      {adding && (
        <div className="mt-1.5 flex items-center gap-1.5">
          <input
            autoFocus
            value={newName}
            disabled={addBusy}
            placeholder="输入主播名，回车增补"
            className="h-7 flex-1 rounded-[6px] border border-line2 bg-white px-2 text-[12px] outline-none focus:border-brand"
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                const n = newName.trim();
                if (!n || addBusy) return;
                setAddBusy(true);
                void addToBucket(bucket.id, n).then(() => {
                  setAddBusy(false);
                  setNewName('');
                });
              }
              if (e.key === 'Escape') {
                setAdding(false);
                setNewName('');
              }
            }}
          />
          <button
            type="button"
            disabled={addBusy || !newName.trim()}
            onClick={() => {
              const n = newName.trim();
              if (!n || addBusy) return;
              setAddBusy(true);
              void addToBucket(bucket.id, n).then(() => {
                setAddBusy(false);
                setNewName('');
              });
            }}
            className="h-7 shrink-0 rounded-[6px] px-2.5 text-[12px] font-semibold text-white disabled:opacity-40"
            style={{ background: color.bar }}
          >
            {addBusy ? '…' : '加入'}
          </button>
        </div>
      )}
    </div>
  );
}
