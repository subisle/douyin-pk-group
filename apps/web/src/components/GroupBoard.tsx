import { useMemo, useState } from 'react';
import {
  DndContext,
  DragOverlay,
  PointerSensor,
  TouchSensor,
  closestCorners,
  useDroppable,
  useSensor,
  useSensors,
  type DragEndEvent,
  type DragOverEvent,
  type DragStartEvent,
} from '@dnd-kit/core';
import { SortableContext, verticalListSortingStrategy } from '@dnd-kit/sortable';
import { formatWave, isPinnedName, resolvePinnedIndex, slotTime, type Player } from '@pk/shared';
import type { Bucket } from '../types.js';
import { useStore } from '../store.js';
import { SortablePlayerCard, DragPreview, type CardMenuData } from './PlayerCard.js';
import { AlertIcon, ClockIcon, CloseIcon, PinIcon } from './icons.js';
import { buildRankContext, gapLine, groupColor } from '../lib/board.js';

const COL_PREFIX = 'col-';

function bucketOf(buckets: Bucket[], id: string): Bucket | undefined {
  if (id.startsWith(COL_PREFIX)) return buckets.find((b) => b.id === id.slice(COL_PREFIX.length));
  return buckets.find((b) => b.players.some((p) => p.key === id));
}

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
  const startTime = useStore((s) => s.startTime);
  const intervalMin = useStore((s) => s.intervalMin);
  const constraints = useStore((s) => s.constraints);
  const violations = useStore((s) => s.violations);
  const locked = useStore((s) => s.locked);
  const toggleLock = useStore((s) => s.toggleLock);
  const [activeKey, setActiveKey] = useState<string | null>(null);

  const cs = useMemo(() => constraints(), [constraints]);
  const schedule = { start: startTime, intervalMin };

  /** 每组是否违反内置条件 + 涉及的人名（后端算好，用于卡片高亮） */
  const violationsByGroup = useMemo(() => {
    const m = new Map<number, Set<string>>();
    for (const v of violations) {
      const set = m.get(v.groupIndex) ?? new Set<string>();
      v.names.forEach((n) => set.add(n));
      m.set(v.groupIndex, set);
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

  const onDragStart = (e: DragStartEvent) => setActiveKey(String(e.active.id));

  const onDragOver = (e: DragOverEvent) => {
    const { active, over } = e;
    if (!over) return;
    const from = bucketOf(buckets, String(active.id));
    const to = bucketOf(buckets, String(over.id));
    if (!from || !to || from.id === to.id) return;
    const overKey = String(over.id).startsWith(COL_PREFIX) ? null : String(over.id);
    moveCard(String(active.id), to.id, overKey);
  };

  const onDragEnd = (e: DragEndEvent) => {
    const { active, over } = e;
    setActiveKey(null);
    if (!over) return;
    const from = bucketOf(buckets, String(active.id));
    const to = bucketOf(buckets, String(over.id));
    if (!from || !to) return;
    const overKey = String(over.id).startsWith(COL_PREFIX) ? null : String(over.id);
    moveCard(String(active.id), to.id, overKey);
  };

  const activePlayer = activeKey ? buckets.flatMap((b) => b.players).find((p) => p.key === activeKey) : null;
  const activeIndex = activePlayer
    ? (buckets.find((b) => b.players.some((p) => p.key === activeKey))?.players.findIndex((p) => p.key === activeKey) ?? 0)
    : 0;

  return (
    <DndContext
      sensors={sensors}
      collisionDetection={closestCorners}
      onDragStart={onDragStart}
      onDragOver={onDragOver}
      onDragEnd={onDragEnd}
      onDragCancel={() => setActiveKey(null)}
    >
      {overCount > 0 && (
        <div className="mb-3 flex flex-wrap items-center gap-2 rounded-card border border-warn-soft bg-warn-soft px-3.5 py-2.5 text-[12.5px] text-warn-ink">
          <AlertIcon size={15} />
          有 {overCount} 组超过 {capacity} 人上限，超出的人已标在组头上，可以直接移出或交换
        </div>
      )}
      <div className="grid items-start gap-4" style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(300px, 1fr))' }}>
        {buckets.map((b, i) => (
          <GroupColumn
            key={b.id}
            bucket={b}
            index={i}
            color={groupColor(i)}
            time={slotTime(i, schedule)}
            conflictNames={violationsByGroup.get(i) ?? EMPTY_NAMES}
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
      <DragOverlay>
        {activePlayer ? (
          <DragPreview
            player={activePlayer}
            color={groupColor(buckets.findIndex((b) => b.players.some((p) => p.key === activePlayer.key)))}
            index={activeIndex + 1}
            pinned={Boolean(isPinnedName(activePlayer.name, cs))}
          />
        ) : null}
      </DragOverlay>
    </DndContext>
  );
}

const EMPTY_NAMES: Set<string> = new Set();
const EMPTY_LIST: string[] = [];

function GroupColumn({
  bucket,
  index,
  color,
  time,
  conflictNames,
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
  conflictNames: Set<string>;
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
  const { setNodeRef, isOver } = useDroppable({ id: COL_PREFIX + bucket.id });

  const cs = constraints();
  const total = bucket.players.reduce((s, p) => s + p.wave, 0);
  const delta = mean ? ((total - mean) / mean) * 100 : 0;
  const deltaText = `${delta >= 0 ? '+' : ''}${delta.toFixed(1)}%`;
  const empty = bucket.players.length === 0;
  const bad = conflictNames.size > 0;
  const over = bucket.players.length > capacity;

  return (
    <div
      ref={setNodeRef}
      className={`group rounded-panel border-t-[5px] bg-surface p-3 pb-2 shadow-card transition-shadow ${
        isOver ? 'ring-2 ring-brand' : bad ? 'ring-2 ring-bad' : ''
      }`}
      style={{ borderTopColor: color.bar }}
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
        {bad && (
          <div className="mt-1.5 rounded-[6px] bg-white/70 px-2 py-1 text-[11px] leading-4 text-bad">
            违反内置条件：{[...conflictNames].join('、')}
          </div>
        )}
      </div>

      <SortableContext items={bucket.players.map((p) => p.key)} strategy={verticalListSortingStrategy}>
        <div className="mt-2.5 flex min-h-[72px] flex-col gap-1">
          {bucket.players.map((p: Player, i: number) => (
            <SortablePlayerCard
              key={p.key}
              player={p}
              color={color}
              index={i + 1}
              pinned={Boolean(isPinnedName(p.name, cs))}
              conflict={conflictNames.has(p.name)}
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
