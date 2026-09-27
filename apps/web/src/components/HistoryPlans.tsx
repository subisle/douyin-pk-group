import { useStore } from '../store.js';
import { Button, Modal, Pill, Tag } from './ui.js';
import { CopyIcon, GlobeIcon, TrashIcon } from './icons.js';

export function HistoryPlans() {
  const { open, items, loading } = useStore((s) => s.plans);
  const closePlans = useStore((s) => s.closePlans);
  const loadPlan = useStore((s) => s.loadPlan);
  const removeSavedPlan = useStore((s) => s.removeSavedPlan);
  const setPublished = useStore((s) => s.setPublished);
  const notify = useStore((s) => s.notify);

  const copyLink = async (id: number) => {
    const url = `${location.origin}/s/${id}`;
    try {
      await navigator.clipboard.writeText(url);
      notify(`展示链接已复制：${url}`);
    } catch {
      notify(`展示链接：${url}`);
    }
  };

  return (
    <Modal open={open} onClose={closePlans} title="历史方案" width={600}>
      {loading && <div className="px-4 py-10 text-center text-[13px] text-ink3">读取中…</div>}
      {!loading && items.length === 0 && (
        <div className="px-4 py-10 text-center text-[13px] text-ink3">还没有保存过方案</div>
      )}
      <div className="divide-y divide-line">
        {items.map((p) => (
          <div key={p.id} className="flex items-center gap-3 px-4 py-3">
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2">
                <span className="truncate text-[14px] font-medium text-ink">{p.title}</span>
                {p.published ? (
                  <Pill tone="good">
                    <GlobeIcon size={11} />
                    已上架
                  </Pill>
                ) : (
                  <Tag tone="neutral">未上架</Tag>
                )}
              </div>
              <div className="mt-0.5 text-[12px] text-ink3">
                #{p.id} · {p.metric === 'total' ? '总音浪' : `近 ${p.days} 天`} · {p.created_at}
              </div>
            </div>

            {p.published && (
              <Button
                size="sm"
                variant="ghost"
                icon={<CopyIcon size={13} />}
                title="复制对外展示链接"
                onClick={() => void copyLink(p.id)}
              >
                链接
              </Button>
            )}
            <Button
              size="sm"
              variant={p.published ? 'subtle' : 'primary'}
              onClick={() => void setPublished(p.id, !p.published)}
              title={p.published ? '下架后 /s 链接立即失效' : '上架后可访问 /s 链接'}
            >
              {p.published ? '下架' : '上架'}
            </Button>
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
