import { useStore } from '../store.js';
import { Button, Modal } from './ui.js';
import { TrashIcon } from './icons.js';

export function HistoryPlans() {
  const { open, items, loading } = useStore((s) => s.plans);
  const closePlans = useStore((s) => s.closePlans);
  const loadPlan = useStore((s) => s.loadPlan);
  const removeSavedPlan = useStore((s) => s.removeSavedPlan);

  return (
    <Modal open={open} onClose={closePlans} title="历史方案" width={560}>
      {loading && <div className="px-4 py-10 text-center text-[13px] text-ink3">读取中…</div>}
      {!loading && items.length === 0 && (
        <div className="px-4 py-10 text-center text-[13px] text-ink3">还没有保存过方案</div>
      )}
      <div className="divide-y divide-line">
        {items.map((p) => (
          <div key={p.id} className="flex items-center gap-3 px-4 py-3">
            <div className="min-w-0 flex-1">
              <div className="truncate text-[14px] font-medium text-ink">{p.title}</div>
              <div className="mt-0.5 text-[12px] text-ink3">
                {p.metric === 'total' ? '总音浪' : `近 ${p.days} 天`} · {p.created_at}
              </div>
            </div>
            <Button size="sm" variant="primary" onClick={() => void loadPlan(p.id)}>
              载入
            </Button>
            <button
              type="button"
              className="text-ink3 hover:text-bad"
              title="删除方案"
              onClick={() => void removeSavedPlan(p.id)}
            >
              <TrashIcon size={15} />
            </button>
          </div>
        ))}
      </div>
    </Modal>
  );
}
