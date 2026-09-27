import { useState, useRef, useEffect } from 'react';
import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import type { Player } from '@pk/shared';
import { formatWave } from '@pk/shared';
import { PinIcon, LockIcon, SwapIcon, MoreIcon } from './icons.js';
import type { GroupColor } from '../lib/board.js';

export const CARD_SHELL =
  'flex w-full items-center gap-2 rounded-full px-2.5 py-[7px] shadow-card cursor-grab select-none active:cursor-grabbing';

/** 头像位：库里有 avatar 就用图，否则显示组内序号（白底彩字圈，和导出长图一致） */
function Initial({ player, color, index, size = 22 }: { player: Player; color: GroupColor; index: number; size?: number }) {
  if (player.avatar) {
    return (
      <img
        src={player.avatar}
        alt={player.name}
        className="shrink-0 rounded-full bg-white object-cover"
        style={{ width: size, height: size }}
        loading="lazy"
        onError={(e) => {
          e.currentTarget.style.display = 'none';
        }}
      />
    );
  }
  return (
    <span
      className="flex shrink-0 items-center justify-center rounded-full bg-white text-[11px] font-bold"
      style={{ width: size, height: size, color: color.ink }}
    >
      {index}
    </span>
  );
}

function NameRow({ player, pinned }: { player: Player; pinned?: boolean }) {
  return (
    <div className="flex min-w-0 items-center gap-1">
      <span className="truncate text-[13.5px] font-semibold leading-5 text-ink">{player.name}</span>
      {pinned && (
        <span className="shrink-0 text-ink3" title="固定在指定组" style={{ display: 'inline-flex', alignItems: 'center' }}>
          <PinIcon size={12} />
        </span>
      )}
      {!player.matched && (
        <span className="shrink-0 rounded-[4px] bg-white/80 px-1 py-[2px] text-[10px] font-medium leading-none text-bad">
          未匹配
        </span>
      )}
    </div>
  );
}

/** 卡片内容：看板实体卡与拖拽浮层共用同一套排版 */
export function CardContent({
  player,
  color,
  index,
  pinned,
}: {
  player: Player;
  color: GroupColor;
  index: number;
  pinned?: boolean;
}) {
  return (
    <>
      <Initial player={player} color={color} index={index} />
      <div className="min-w-0 flex-1">
        <NameRow player={player} pinned={pinned} />
        {player.leader && <div className="truncate text-[11px] leading-4 text-ink3">师傅 {player.leader}</div>}
      </div>
      <span className="nums shrink-0 text-[12.5px] font-semibold" style={{ color: color.ink }}>
        {formatWave(player.wave)}
      </span>
    </>
  );
}

export interface CardMenuData {
  locked: boolean;
  /** 目标组下拉：[组下标, 组名] */
  groups: Array<[number, string]>;
  /** 可交换的人（同组优先） */
  swapTargets: Array<{ key: string; name: string; groupIndex: number }>;
  onToggleLock: (key: string) => void;
  onMove: (key: string, groupIndex: number) => void;
  onSwap: (key: string, otherKey: string) => void;
  onUnassign: (key: string) => void;
}

export function SortablePlayerCard({
  player,
  color,
  index,
  pinned,
  conflict,
  highlight,
  dense,
  rankLine,
  menu,
}: {
  player: Player;
  color: GroupColor;
  index: number;
  pinned?: boolean;
  conflict?: boolean;
  /** 搜索命中：描边高亮并滚动到可视区 */
  highlight?: boolean;
  /** 紧凑模式：隐藏名次行，卡片更矮 */
  dense?: boolean;
  rankLine?: string;
  menu?: CardMenuData;
}) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: player.key,
    data: { type: 'player', bucketId: undefined },
  });
  const [open, setOpen] = useState(false);
  const [tab, setTab] = useState<'move' | 'swap'>('move');
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('mousedown', onDoc);
    return () => document.removeEventListener('mousedown', onDoc);
  }, [open]);

  // 搜索命中时滚到视野内（nearest：已经在屏内就不动，避免整页乱跳）
  useEffect(() => {
    if (highlight) ref.current?.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
  }, [highlight]);

  return (
    <div style={{ transform: CSS.Translate.toString(transform), transition, opacity: isDragging ? 0.4 : 1 }}>
      <div
        ref={setNodeRef}
        data-player-card={player.name}
        className={`${CARD_SHELL} ${dense ? 'py-[4px]' : ''} ${
          conflict ? 'ring-2 ring-bad' : highlight ? 'ring-2 ring-brand' : ''
        }`}
        style={{ background: conflict ? '#FEE4E2' : color.soft }}
        {...attributes}
        {...listeners}
      >
        <CardContent player={player} color={color} index={index} pinned={pinned} />
        {menu && (
          <div className="relative shrink-0" ref={ref} onPointerDown={(e) => e.stopPropagation()}>
            <button
              type="button"
              aria-label="更多操作"
              onClick={() => setOpen((v) => !v)}
              className="flex h-5 w-5 items-center justify-center rounded-full text-ink3 hover:bg-white/60 hover:text-ink"
            >
              {menu.locked ? <LockIcon size={12} /> : <MoreIcon size={12} />}
            </button>
            {open && (
              <div className="absolute right-0 top-full z-30 mt-1 w-44 rounded-ctl border border-line bg-surface p-1 text-[12px] shadow-drag">
                <button
                  type="button"
                  onClick={() => {
                    menu.onToggleLock(player.key);
                    setOpen(false);
                  }}
                  className="flex w-full items-center gap-1.5 rounded-ctl px-2 py-1 text-left text-ink hover:bg-subtle"
                >
                  <LockIcon size={12} />
                  {menu.locked ? '解锁（允许重排）' : '锁定（不参与重排）'}
                </button>
                <button
                  type="button"
                  onClick={() => {
                    menu.onUnassign(player.key);
                    setOpen(false);
                  }}
                  className="w-full rounded-ctl px-2 py-1 text-left text-ink hover:bg-subtle"
                >
                  移出到未分组
                </button>
                <div className="my-1 h-px bg-line" />
                <div className="flex gap-1 px-1 pb-1">
                  <button
                    type="button"
                    onClick={() => setTab('move')}
                    className={`rounded-ctl px-1.5 py-0.5 ${tab === 'move' ? 'bg-brand text-white' : 'text-ink2 hover:bg-subtle'}`}
                  >
                    移动
                  </button>
                  <button
                    type="button"
                    onClick={() => setTab('swap')}
                    className={`rounded-ctl px-1.5 py-0.5 ${tab === 'swap' ? 'bg-brand text-white' : 'text-ink2 hover:bg-subtle'}`}
                  >
                    交换
                  </button>
                </div>
                <div className="max-h-40 overflow-y-auto">
                  {tab === 'move'
                    ? menu.groups.map(([gi, name]) => (
                        <button
                          key={gi}
                          type="button"
                          onClick={() => {
                            menu.onMove(player.key, gi);
                            setOpen(false);
                          }}
                          className="w-full rounded-ctl px-2 py-1 text-left text-ink hover:bg-subtle"
                        >
                          → {name}
                        </button>
                      ))
                    : menu.swapTargets.map((t) => (
                        <button
                          key={t.key}
                          type="button"
                          onClick={() => {
                            menu.onSwap(player.key, t.key);
                            setOpen(false);
                          }}
                          className="flex w-full items-center gap-1.5 rounded-ctl px-2 py-1 text-left text-ink hover:bg-subtle"
                        >
                          <SwapIcon size={11} />
                          {t.name}
                          <span className="text-ink3">（第 {t.groupIndex + 1} 组）</span>
                        </button>
                      ))}
                  {tab === 'swap' && menu.swapTargets.length === 0 && (
                    <div className="px-2 py-1 text-ink3">没有别人可交换</div>
                  )}
                </div>
              </div>
            )}
          </div>
        )}
      </div>
      {rankLine && !dense && <div className="px-2 pt-[3px] text-[10.5px] leading-4 text-ink3">{rankLine}</div>}
    </div>
  );
}

/** 拖拽时跟随指针的浮层卡 */
export function DragPreview({
  player,
  color,
  index,
  pinned,
}: {
  player: Player;
  color: GroupColor;
  index: number;
  pinned?: boolean;
}) {
  return (
    <div className={`${CARD_SHELL} shadow-drag ring-2 ring-brand`} style={{ background: color.soft }}>
      <CardContent player={player} color={color} index={index} pinned={pinned} />
    </div>
  );
}
