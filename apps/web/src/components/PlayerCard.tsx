import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import type { Player } from '@pk/shared';
import { formatWave } from '@pk/shared';
import { PinIcon } from './icons.js';
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

export function SortablePlayerCard({
  player,
  color,
  index,
  pinned,
  conflict,
  rankLine,
}: {
  player: Player;
  color: GroupColor;
  index: number;
  pinned?: boolean;
  conflict?: boolean;
  rankLine?: string;
}) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: player.key,
    data: { type: 'player', bucketId: undefined },
  });

  return (
    <div style={{ transform: CSS.Translate.toString(transform), transition, opacity: isDragging ? 0.4 : 1 }}>
      <div
        ref={setNodeRef}
        className={`${CARD_SHELL} ${conflict ? 'ring-2 ring-bad' : ''}`}
        style={{ background: conflict ? '#FEE4E2' : color.soft }}
        {...attributes}
        {...listeners}
      >
        <CardContent player={player} color={color} index={index} pinned={pinned} />
      </div>
      {rankLine && <div className="px-2 pt-[3px] text-[10.5px] leading-4 text-ink3">{rankLine}</div>}
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
