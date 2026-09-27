/**
 * 对外展示页（只读）：/s/:planId
 * 只能看到分组（组名 / 开赛时间 / 人数 / 名单），不显示音浪、不能编辑。
 */
import { useEffect, useState } from 'react';
import type { Player } from '@pk/shared';
import { slotTime } from '@pk/shared';
import { api, type SavedPlan } from '../api.js';
import { groupColor } from '../lib/board.js';

interface PlanDetail {
  id: number;
  title: string;
  payload: SavedPlan;
  created_at: string;
}

export function ShareView({ planId }: { planId: string }) {
  const [plan, setPlan] = useState<PlanDetail | null>(null);
  const [error, setError] = useState('');

  useEffect(() => {
    api
      .getPlan(Number(planId))
      .then((p) => setPlan(p as unknown as PlanDetail))
      .catch((e) => setError((e as Error).message));
  }, [planId]);

  if (error) {
    return (
      <div className="flex min-h-screen items-center justify-center bg-canvas p-8 text-[15px] text-ink2">
        展示页不存在或已失效
      </div>
    );
  }
  if (!plan) {
    return (
      <div className="flex min-h-screen items-center justify-center bg-canvas p-8 text-[15px] text-ink3">
        加载中…
      </div>
    );
  }

  const { payload } = plan;
  const schedule = payload.schedule ?? { start: '08:15', intervalMin: 15 };
  const brand = payload.brand ?? '';
  const total = payload.buckets.reduce((s, b) => s + b.players.length, 0);

  return (
    <div className="min-h-screen bg-canvas">
      {/* 头部：与导出长图同款的渐变卡 */}
      <div className="mx-auto max-w-[1024px] px-4 pt-6">
        <div
          className="rounded-2xl px-5 py-4 text-white shadow-card"
          style={{ background: 'linear-gradient(120deg, #ec4899 0%, #8b5cf6 55%, #3b82f6 100%)' }}
        >
          <div className="flex flex-wrap items-center gap-2">
            {brand && <span className="text-[20px] font-black tracking-wide">{brand}</span>}
            <span className="text-[15px] font-semibold opacity-90">{payload.title}</span>
          </div>
          <div className="mt-1.5 flex flex-wrap items-center gap-2 text-[12px] opacity-90">
            <span className="rounded-full bg-white/20 px-2.5 py-0.5">{payload.buckets.length} 组</span>
            <span className="rounded-full bg-white/20 px-2.5 py-0.5">{total} 人</span>
            <span className="rounded-full bg-white/20 px-2.5 py-0.5">首组 {schedule.start} 开赛</span>
          </div>
        </div>
      </div>

      {/* 分组网格：手机单列 / 桌面多列 */}
      <div
        className="mx-auto grid max-w-[1024px] items-start gap-4 px-4 py-5"
        style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(300px, 1fr))' }}
      >
        {payload.buckets.map((b, i) => {
          const color = groupColor(i);
          return (
            <div
              key={i}
              className="rounded-xl border-t-[5px] bg-surface p-3 pb-2 shadow-card"
              style={{ borderTopColor: color.bar }}
            >
              <div className="rounded-2xl px-3.5 py-2.5" style={{ background: color.soft }}>
                <div className="flex items-center justify-between gap-2">
                  <span className="truncate text-[15px] font-bold" style={{ color: color.ink }}>
                    {b.name}
                  </span>
                  <span className="nums shrink-0 text-[11.5px] font-medium" style={{ color: color.ink, opacity: 0.82 }}>
                    {b.players.length} 人
                  </span>
                </div>
                <div
                  className="mt-0.5 flex items-center gap-1.5 text-[11.5px]"
                  style={{ color: color.ink, opacity: 0.82 }}
                >
                  <span className="nums font-medium">{slotTime(i, schedule)}</span>
                  <span>开赛</span>
                </div>
              </div>
              <div className="mt-2.5 flex flex-col gap-1 pb-1">
                {b.players.map((p: Player, j: number) => (
                  <div
                    key={p.key ?? j}
                    className="flex items-center gap-2 rounded-full px-2.5 py-1.5"
                    style={{ background: color.soft }}
                  >
                    <span
                      className="flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-white text-[10.5px] font-bold"
                      style={{ color: color.ink }}
                    >
                      {j + 1}
                    </span>
                    <span className="truncate text-[13px] font-semibold text-ink">{p.name}</span>
                    {p.leader && (
                      <span className="ml-auto shrink-0 text-[10.5px] text-ink3">师傅 {p.leader}</span>
                    )}
                  </div>
                ))}
              </div>
            </div>
          );
        })}
      </div>

      <div className="pb-8 pt-2 text-center text-[11px] text-ink3">
        本页面为只读展示 · 来自分组方案「{payload.title}」
      </div>
    </div>
  );
}
