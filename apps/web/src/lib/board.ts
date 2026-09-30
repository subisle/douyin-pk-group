import { formatWave, type Player } from '@pk/shared';
import type { Bucket } from '../types.js';

/** 组卡配色：顶色条 + 粉彩底，按组序号循环（看板与导出长图共用） */
export interface GroupColor {
  bar: string;
  soft: string;
  ink: string;
}

export const GROUP_COLORS: GroupColor[] = [
  { bar: '#EC4899', soft: '#FCE7F3', ink: '#BE185D' },
  { bar: '#8B5CF6', soft: '#EDE9FE', ink: '#6D28D9' },
  { bar: '#06B6D4', soft: '#CFFAFE', ink: '#0E7490' },
  { bar: '#F59E0B', soft: '#FEF3C7', ink: '#B45309' },
  { bar: '#10B981', soft: '#D1FAE5', ink: '#047857' },
  { bar: '#F43F5E', soft: '#FFE4E6', ink: '#BE123C' },
  { bar: '#3B82F6', soft: '#DBEAFE', ink: '#1D4ED8' },
  { bar: '#22C55E', soft: '#DCFCE7', ink: '#15803D' },
  { bar: '#A855F7', soft: '#F3E8FF', ink: '#7E22CE' },
  { bar: '#0EA5E9', soft: '#E0F2FE', ink: '#0369A1' },
];

export function groupColor(index: number): GroupColor {
  return GROUP_COLORS[index % GROUP_COLORS.length];
}

/* ------------------------------- 排名与差距 ------------------------------- */

const TIERS = [30, 20, 10, 5, 3];

export interface RankContext {
  rankOf: Map<string, number>;
  waveAt: (rank: number) => number;
}

export function buildRankContext(buckets: Bucket[]): RankContext {
  const all = buckets.flatMap((b) => b.players).filter((p) => !p.excluded);
  const sorted = [...all].sort((a, b) => b.wave - a.wave || a.name.localeCompare(b.name, 'zh-Hans-CN'));
  const rankOf = new Map<string, number>();
  sorted.forEach((p, i) => rankOf.set(p.key, i + 1));
  const waveAt = (rank: number) => sorted[rank - 1]?.wave ?? 0;
  return { rankOf, waveAt };
}

/**
 * 每人一行差距说明：不显示自身音浪，只显示「当前第几名」+ 到前 N 名还差多少音浪。
 * - 排名 4~30+：取比自己高的最近三档（前 30 / 前 20 / 前 10 / 前 5 / 前 3）
 * - 前三名：距离其余前排各差多少（第 3 名 → 距第 2、第 1）
 * - 第 1 名：显示领先第 2 名多少
 */
export function gapLine(p: Player, rank: number, waveAt: (r: number) => number): string {
  const my = p.wave;
  const fmt = (g: number) => (g > 0 ? formatWave(g) : '同分');

  if (rank === 1) {
    const lead = my - waveAt(2);
    return `第1名 · 领先第2名 ${fmt(lead)}`;
  }
  if (rank <= 3) {
    const parts = [2, 1].filter((t) => t < rank).map((t) => `距第${t}名 ${fmt(waveAt(t) - my)}`);
    return `第${rank}名 · ${parts.join(' · ')}`;
  }
  const targets = TIERS.filter((t) => t < rank).slice(0, 3);
  const parts = targets.map((t) => `距前${t} ${fmt(waveAt(t) - my)}`);
  return `第${rank}名 · ${parts.join(' · ')}`;
}

/** 导出图专用：只看相邻名次差距，名字旁边单独带全局名次 */
export function adjacentGapLine(p: Player, rank: number, waveAt: (r: number) => number): string {
  if (rank <= 0) return '';
  const my = p.wave;
  const fmt = (value: number) => (value > 0 ? formatWave(value) : '同分');
  const gapTo = (targetRank: number) => {
    const target = waveAt(targetRank);
    return target > my ? `距第${targetRank}名 ${fmt(target - my)}` : `领先第${targetRank}名 ${fmt(my - target)}`;
  };

  if (rank === 1) {
    return `领先第2名 ${fmt(my - waveAt(2))}`;
  }
  const parts: string[] = [];
  if (rank >= 3) parts.push(gapTo(rank - 2));
  parts.push(gapTo(rank - 1));
  return parts.join(' · ');
}
