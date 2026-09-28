/**
 * 展示页的每日音浪曲线图。
 *
 * 自己画 SVG 而不引图表库：这个页面是给外部看的公开页，体积和加载速度敏感，
 * 而这里的需求很窄（几条折线 + 一点交互），引库反而更重。
 *
 * 响应式做法：用 ResizeObserver 量出容器宽度，按真实像素画，而不是
 * viewBox 整体缩放——后者会把坐标轴文字一起拉大拉小，手机上要么糊要么过大。
 */
import { useEffect, useMemo, useRef, useState } from 'react';
import { formatWave } from '@pk/shared';
import type { PublicTrend } from '../api.js';
import { groupColor } from '../lib/board.js';

/** 坐标轴留白：左边给刻度文字，下边给日期 */
const PAD = { top: 12, right: 12, bottom: 26, left: 44 };

export function TrendChart({ trend }: { trend: PublicTrend }) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  // null = 全部显示；点某条则只看那一条
  const [active, setActive] = useState<number | null>(null);

  // 容器宽度随屏幕/旋转变化，曲线要跟着重画
  useEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => setWidth(entry.contentRect.width));
    ro.observe(el);
    setWidth(el.getBoundingClientRect().width);
    return () => ro.disconnect();
  }, []);

  const { dates, series } = trend;
  const max = useMemo(
    () => Math.max(1, ...series.flatMap((s) => s.values)),
    [series],
  );

  // 手机矮一点，PC 高一点
  const height = width >= 640 ? 240 : 190;
  const innerW = Math.max(0, width - PAD.left - PAD.right);
  const innerH = height - PAD.top - PAD.bottom;
  const n = dates.length;

  const x = (i: number) => (n <= 1 ? innerW / 2 : (i / (n - 1)) * innerW);
  const y = (v: number) => innerH - (v / max) * innerH;

  // 只画首/尾和中间等分几处，日期多时挤在一起反而看不清
  const labelIdx = useMemo(() => {
    if (n <= 1) return [0];
    const want = width >= 640 ? 6 : 4;
    if (n <= want) return dates.map((_, i) => i);
    const step = (n - 1) / (want - 1);
    return Array.from({ length: want }, (_, i) => Math.round(i * step));
  }, [n, width]);

  if (width === 0) {
    // 首帧量不到宽度，先占个位避免布局跳动
    return <div ref={wrapRef} style={{ height }} />;
  }

  return (
    <div className="rounded-xl bg-surface p-3 shadow-card sm:p-4">
      <div className="mb-2 flex items-baseline gap-2 text-[12px] text-ink2">
        每日音浪趋势
        <span className="flex-1" />
        <span className="nums text-ink3">近 {n} 天</span>
      </div>

      <div ref={wrapRef}>
        <svg width={width} height={height} role="img" aria-label="每日音浪趋势曲线图">
          <g transform={`translate(${PAD.left},${PAD.top})`}>
            {/* 横向网格 + 刻度 */}
            {[0, 0.5, 1].map((r) => (
              <g key={r}>
                <line
                  x1={0}
                  x2={innerW}
                  y1={innerH * r}
                  y2={innerH * r}
                  stroke="currentColor"
                  className="text-line"
                  strokeDasharray={r === 1 ? undefined : '3 3'}
                />
                <text
                  x={-8}
                  y={innerH * r}
                  textAnchor="end"
                  dominantBaseline="middle"
                  className="nums fill-ink3 text-[10px]"
                >
                  {formatWave(max * (1 - r))}
                </text>
              </g>
            ))}

            {series.map((s, i) => {
              const dim = active !== null && active !== i;
              const color = groupColor(i);
              const pts = s.values.map((v, j) => `${x(j)},${y(v)}`).join(' ');
              return (
                <g key={s.name} opacity={dim ? 0.18 : 1} className="transition-opacity">
                  {s.values.length > 1 && (
                    <polyline
                      points={pts}
                      fill="none"
                      stroke={color.bar}
                      strokeWidth={active === i ? 2.5 : 1.8}
                      strokeLinejoin="round"
                      strokeLinecap="round"
                    />
                  )}
                  {/* 只画首尾点：点多时圆点会盖住折线 */}
                  {s.values.map((v, j) =>
                    j === 0 || j === s.values.length - 1 ? (
                      <circle key={j} cx={x(j)} cy={y(v)} r={2.4} fill={color.bar} />
                    ) : null,
                  )}
                </g>
              );
            })}

            {labelIdx.map((i) => (
              <text
                key={i}
                x={x(i)}
                y={innerH + 16}
                textAnchor={i === 0 ? 'start' : i === n - 1 ? 'end' : 'middle'}
                className="nums fill-ink3 text-[10px]"
              >
                {dates[i]?.slice(5)}
              </text>
            ))}
          </g>
        </svg>
      </div>

      {/* 图例：手机上自动换行，点一下只看那组，再点取消 */}
      <div className="mt-2 flex flex-wrap gap-x-3 gap-y-1.5">
        {series.map((s, i) => {
          const color = groupColor(i);
          const on = active === null || active === i;
          return (
            <button
              key={s.name}
              type="button"
              onClick={() => setActive((cur) => (cur === i ? null : i))}
              className="flex items-center gap-1.5 text-[11.5px] transition-opacity"
              style={{ opacity: on ? 1 : 0.35 }}
            >
              <span
                className="h-2 w-2 shrink-0 rounded-full"
                style={{ background: color.bar }}
              />
              <span className="text-ink2">{s.name}</span>
            </button>
          );
        })}
      </div>
    </div>
  );
}
