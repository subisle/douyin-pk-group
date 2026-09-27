import { useState } from 'react';
import { useStore } from '../store.js';
import { Button, Panel } from './ui.js';
import { BalanceIcon, LockIcon, RedoIcon, TrashIcon, UndoIcon, UserSearchIcon } from './icons.js';

/** 看板工具条：撤销/重做/均衡/锁定数 + 未分组池 */
export function BoardTools() {
  const undo = useStore((s) => s.undo);
  const redo = useStore((s) => s.redo);
  const canUndo = useStore((s) => s.past.length > 0);
  const canRedo = useStore((s) => s.future.length > 0);
  const polish = useStore((s) => s.polish);
  const locked = useStore((s) => s.locked);
  const unassigned = useStore((s) => s.unassigned);
  const assignFromPool = useStore((s) => s.assignFromPool);
  const [filter, setFilter] = useState('');

  const shown = filter ? unassigned.filter((p) => p.name.includes(filter)) : unassigned;

  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center gap-2 rounded-card border border-line bg-surface px-3 py-2">
        <Button size="sm" variant="subtle" icon={<UndoIcon size={14} />} disabled={!canUndo} onClick={undo}>
          撤销
        </Button>
        <Button size="sm" variant="subtle" icon={<RedoIcon size={14} />} disabled={!canRedo} onClick={redo}>
          重做
        </Button>
        <Button size="sm" variant="subtle" icon={<BalanceIcon size={14} />} onClick={polish}>
          均衡微调
        </Button>
        <span className="flex-1" />
        {locked.length > 0 && (
          <span className="flex items-center gap-1 text-[12px] text-ink2">
            <LockIcon size={13} />
            {locked.length} 人已锁定
          </span>
        )}
      </div>

      {unassigned.length > 0 && (
        <Panel className="px-3 py-2.5">
          <div className="mb-2 flex items-center gap-2">
            <UserSearchIcon size={14} className="text-ink2" />
            <span className="text-[13px] font-medium text-ink">未分组（{unassigned.length}）</span>
            <span className="text-[12px] text-ink2">点名字旁边的组号即可归队</span>
            <span className="flex-1" />
            <input
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              placeholder="找人…"
              className="w-24 rounded-ctl border border-line bg-canvas px-2 py-1 text-[12px] outline-none focus:border-brand"
            />
          </div>
          <div className="flex flex-wrap gap-1.5">
            {shown.map((p) => (
              <UnassignedChip key={p.key} playerKey={p.key} name={p.name} onAssign={assignFromPool} />
            ))}
            {shown.length === 0 && <span className="text-[12px] text-ink3">没有匹配的人</span>}
          </div>
        </Panel>
      )}
    </div>
  );
}

function UnassignedChip({
  playerKey,
  name,
  onAssign,
}: {
  playerKey: string;
  name: string;
  onAssign: (key: string, bucketId: string) => void;
}) {
  const buckets = useStore((s) => s.buckets);
  const [open, setOpen] = useState(false);
  return (
    <div className="relative">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex items-center gap-1 rounded-ctl border border-line2 bg-canvas px-2 py-1 text-[12px] text-ink transition-colors hover:border-brand"
      >
        {name}
        <TrashIcon size={12} className="text-ink3" />
      </button>
      {open && (
        <div className="absolute left-0 top-full z-20 mt-1 flex flex-col gap-0.5 rounded-ctl border border-line bg-surface p-1 shadow-lg">
          {buckets.map((b, i) => (
            <button
              key={b.id}
              type="button"
              onClick={() => {
                onAssign(playerKey, b.id);
                setOpen(false);
              }}
              className="rounded-ctl px-2 py-1 text-left text-[12px] text-ink hover:bg-subtle"
            >
              第 {i + 1} 组
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
