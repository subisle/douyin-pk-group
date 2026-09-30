import { formatWave, slotTime, type Player, type ScheduleRule, type WaveMetric } from '@pk/shared';
import type { Bucket } from '../types.js';
import { useStore } from '../store.js';
import { downloadBlob, nodeToPngBlob } from '../lib/exportImage.js';
import { adjacentGapLine, buildRankContext, groupColor } from '../lib/board.js';
import { Button } from './ui.js';
import { CopyIcon, DownloadIcon, RefreshIcon, SaveIcon } from './icons.js';

/* 字体名一律用单引号：这里的 CSS 要嵌进 style="..." 里，双引号会把属性提前截断 */
const FONT = "-apple-system, BlinkMacSystemFont, 'PingFang SC', 'Noto Sans SC', 'Microsoft YaHei', sans-serif";
const INK = '#111827';
const INK2 = '#6B7280';

const PAGE_W = 1024;

function esc(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c] ?? c);
}

function buildExportHtml(
  buckets: Bucket[],
  brand: string,
  schedule: ScheduleRule,
  period: string,
  strategyLabel: string,
): string {
  const ctx = buildRankContext(buckets);
  const totalPlayers = buckets.reduce((s, b) => s + b.players.length, 0);

  const header = `
    <div style="display:flex;align-items:center;justify-content:space-between;gap:16px;
                background:linear-gradient(100deg,#7C3AED 0%,#C026D3 38%,#EC4899 66%,#FB923C 100%);
                border-radius:24px;padding:22px 26px;color:#ffffff;
                box-shadow:0 14px 30px -12px rgba(124,58,237,.45)">
      <div style="min-width:0">
        <div style="font-size:11px;font-weight:600;letter-spacing:3px;opacity:.85">${esc('XINGHAI YICHUANG · PK GROUP')}</div>
        <div style="display:flex;align-items:center;gap:10px;margin-top:7px">
          <span style="width:34px;height:34px;border-radius:50%;background:rgba(255,255,255,.22);
                       display:flex;align-items:center;justify-content:center;font-size:15px;font-weight:700">${esc(brand.slice(0, 1))}</span>
          <span style="font-size:30px;font-weight:800;letter-spacing:1px">${esc(brand)}</span>
        </div>
        <div style="margin-top:8px;font-size:12.5px;opacity:.92">
          ${esc(period)} · ${esc(strategyLabel)} · 共 ${totalPlayers} 人 · ${buckets.length} 组 ·
          ${slotTime(0, schedule)} 起 / 间隔 ${schedule.intervalMin} 分
        </div>
      </div>
      <div style="display:flex;gap:10px;flex:none">
        <div style="min-width:64px;padding:10px 14px;border-radius:999px;background:rgba(255,255,255,.18);text-align:center">
          <div style="font-size:22px;font-weight:800;line-height:1.1">${buckets.length}</div>
          <div style="font-size:11px;opacity:.9">组</div>
        </div>
        <div style="min-width:64px;padding:10px 14px;border-radius:999px;background:#ffffff;color:#7C3AED;text-align:center">
          <div style="font-size:22px;font-weight:800;line-height:1.1">${totalPlayers}</div>
          <div style="font-size:11px;opacity:.75">人</div>
        </div>
      </div>
    </div>`;

  const cards = buckets
    .map((b, gi) => {
      const c = groupColor(gi);
      const half = Math.ceil(b.players.length / 2);
      const col = (arr: Player[], offset: number) =>
        arr
          .map((p, i) => {
            const rank = ctx.rankOf.get(p.key) ?? 0;
            return `<div style="margin-bottom:9px">
              <div style="display:flex;align-items:center;gap:8px;background:${c.soft};border-radius:999px;padding:7px 12px">
                <span style="width:20px;height:20px;border-radius:50%;background:#ffffff;color:${c.ink};
                             font-size:11px;font-weight:700;display:flex;align-items:center;justify-content:center;flex:none">${offset + i + 1}</span>
                <span style="min-width:0;flex:0 1 auto;font-size:14px;font-weight:600;color:${INK};
                             overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(p.name)}</span>
                <span style="flex:none;font-size:14px;font-weight:600;color:${c.ink}">第${rank}名</span>
              </div>
              <div style="padding:3px 8px 0 8px;font-size:10.5px;line-height:1.45;color:${INK2}">${esc(adjacentGapLine(p, rank, ctx.waveAt))}</div>
            </div>`;
          })
          .join('');
      return `<div style="background:#ffffff;border-radius:20px;padding:18px 18px 10px;
                          border-top:6px solid ${c.bar};box-shadow:0 10px 26px -14px rgba(17,24,39,.18)">
        <div style="background:${c.soft};border-radius:16px;padding:11px 16px;margin-bottom:14px">
          <span style="font-size:17px;font-weight:800;color:${c.ink}">第 ${gi + 1} 组</span>
          <div style="margin-top:2px;font-size:12px;color:${c.ink};opacity:.78">
            ${slotTime(gi, schedule)} 开赛 · ${b.players.length} 人
          </div>
        </div>
        <div style="display:flex;gap:14px">
          <div style="flex:1;min-width:0">${col(b.players.slice(0, half), 0)}</div>
          <div style="flex:1;min-width:0">${col(b.players.slice(half), half)}</div>
        </div>
      </div>`;
    })
    .join('');

  const today = new Date().toISOString().slice(0, 10);
  return `<div style="font-family:${FONT};padding:26px;background:linear-gradient(160deg,#F3F0FF 0%,#FDF2F8 46%,#E6FBFF 100%);">
    ${header}
    <div style="display:grid;grid-template-columns:1fr 1fr;gap:18px;margin-top:18px;align-items:start">${cards}</div>
    <div style="margin-top:16px;text-align:center;font-size:11.5px;color:#9CA3AF">
      ${esc(brand)} · 分组导出 · 排名口径 ${esc(period)} · ${today}
    </div>
  </div>`;
}

export function ExportBar({ buckets }: { buckets: Bucket[] }) {
  const brand = useStore((s) => s.brand);
  const metric = useStore((s) => s.metric);
  const days = useStore((s) => s.days);
  const dates = useStore((s) => s.dates);
  const strategy = useStore((s) => s.strategy);
  const regroup = useStore((s) => s.regroup);
  const savePlanNow = useStore((s) => s.savePlanNow);
  const makeShareLink = useStore((s) => s.makeShareLink);
  const notify = useStore((s) => s.notify);
  const startTime = useStore((s) => s.startTime);
  const intervalMin = useStore((s) => s.intervalMin);
  const schedule = { start: startTime, intervalMin };

  const strategyLabel: Record<string, string> = {
    snake: '蛇形均衡',
    'snake-no-master': '蛇形 + 同门回避',
    sequential: '从高到低',
    random: '随机分组',
  };

  const exportImage = async () => {
    if (!buckets.length) {
      notify('还没有分组结果');
      return;
    }
    const period: string = metric === 'lastN' ? `近 ${days} 天` : `总音浪（截止 ${dates[0] ?? '-'}）`;
    const node = document.createElement('div');
    node.className = 'export-stage';
    node.style.width = `${PAGE_W}px`;
    node.innerHTML = buildExportHtml(buckets, brand, schedule, period, strategyLabel[strategy] ?? '均衡分组');
    document.body.appendChild(node);
    try {
      const blob = await nodeToPngBlob(node, { scale: 2, backgroundColor: '#ffffff' });
      downloadBlob(blob, `${brand}-分组-${new Date().toISOString().slice(0, 10)}.png`);
      notify(`长图已导出（${(blob.size / 1024).toFixed(0)} KB）`);
    } catch (e) {
      console.error('导出长图失败：', e);
      notify(`导出失败：${(e as Error).message}`);
    } finally {
      node.remove();
    }
  };

  const copyText = async () => {
    const metricText: Record<WaveMetric, string> = { lastN: `近 ${days} 天`, total: '总音浪' };
    const ctx = buildRankContext(buckets);
    const lines = [`${brand} 分组（口径：${metricText[metric]} · 数据截止 ${dates[0] ?? '-'}）`];
    buckets.forEach((b, i) => {
      lines.push(
        `${b.name}（${slotTime(i, schedule)} 开赛 · ${b.players.length} 人）：${b.players
          .map((p) => `${p.name}(第 ${ctx.rankOf.get(p.key) ?? '-'} 名)`)
          .join('、')}`,
      );
    });
    await navigator.clipboard.writeText(lines.join('\n'));
    notify('文字版已复制到剪贴板');
  };

  return (
    <div className="flex flex-wrap items-center justify-between gap-2">
      <div className="flex flex-wrap gap-2">
        <Button icon={<RefreshIcon size={14} />} onClick={regroup}>
          重新自动分组
        </Button>
        <Button icon={<CopyIcon size={14} />} onClick={() => void copyText()}>
          复制文字版
        </Button>
        <Button icon={<SaveIcon size={14} />} onClick={() => void savePlanNow()}>
          保存方案
        </Button>
        <Button
          icon={<CopyIcon size={14} />}
          onClick={() => void makeShareLink()}
          title="保存当前分组并生成只读展示链接，复制后发到群里即可"
        >
          分享链接
        </Button>
      </div>
      <Button variant="dark" icon={<DownloadIcon size={14} />} onClick={() => void exportImage()}>
        导出图片
      </Button>
    </div>
  );
}
