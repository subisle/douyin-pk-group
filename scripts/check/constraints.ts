/**
 * 内置条件自检（真实名单 + 真实音浪）：
 * 1) 啸泽是否在第 1 组、啸帆是否在最后一组
 * 2) 玖玉 / 玖妹 / 玖玥 是否两两不同组（名单里补上玖玉、玖妹后验）
 * 3) 啸昭 / 啸寻 是否至少相隔 4 组
 * 4) 开赛时间列是否 08:15 起每 15 分钟一组
 * 5) 顺带打印极差，对比四种策略/约束组合
 *
 * 用法：node --import tsx scripts/check-constraints.ts
 */
import { DEFAULT_ROSTER } from '../packages/shared/src/roster.js';
import type { ConstraintSet } from '../packages/shared/src/constraints.js';
import type { Player } from '../packages/shared/src/types.js';

const BASE = process.env.API ?? 'http://127.0.0.1:8787';

interface ArrangeResponse {
  groups: Player[][];
  balance: { max: number; min: number; spread: number };
  slots: string[];
  violations: Array<{ message: string }>;
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    ...init,
    headers: { 'Content-Type': 'application/json', ...(init?.headers ?? {}) },
  });
  if (!res.ok) throw new Error(`${res.status} ${await res.text()}`);
  return (await res.json()) as T;
}

async function parse(text: string): Promise<Player[]> {
  const payload = await request<{ players: Player[] }>('/api/roster/parse', {
    method: 'POST',
    body: JSON.stringify({ text, metric: 'total', days: 3 }),
  });
  return payload.players;
}

async function arrange(players: Player[], constraints: ConstraintSet): Promise<ArrangeResponse> {
  return request<ArrangeResponse>('/api/group/arrange', {
    method: 'POST',
    body: JSON.stringify({
      players,
      groupSize: 8,
      strategy: 'snake',
      constraints: { full: constraints },
    }),
  });
}

async function main() {
  const players = await parse(DEFAULT_ROSTER);
  const active = players.filter((p) => !p.excluded);
  const defaultConstraints = await request<ConstraintSet>('/api/group/constraints');
  const arranged = await arrange(players, defaultConstraints);
  const { groups } = arranged;

  console.log(`参赛 ${active.length} 人 → ${groups.length} 组（每组上限 8）\n`);
  groups.forEach((g, i) => {
    const total = g.reduce((s, p) => s + p.wave, 0);
    console.log(
      `第 ${i + 1} 组  ${arranged.slots[i]}  ${String(g.length).padStart(2)} 人  ${(
        total / 10000
      ).toFixed(1)}万  ${g.map((p) => p.name).join(' ')}`,
    );
  });

  const b = arranged.balance;
  console.log(`\n最高 ${(b.max / 10000).toFixed(1)}万 / 最低 ${(b.min / 10000).toFixed(1)}万 / 极差 ${b.spread}%`);

  const at = (name: string) => groups.findIndex((g) => g.some((p) => p.name === name)) + 1;
  console.log(
    `\n啸昭=第 ${at('啸昭')} 组，啸寻=第 ${at('啸寻')} 组（需组号差 ≥ 4，实际差 ${Math.abs(at('啸昭') - at('啸寻'))}）`,
  );
  console.log(
    arranged.violations.length
      ? `\n❌ 冲突：\n${arranged.violations.map((v) => '  - ' + v.message).join('\n')}`
      : '\n✅ 内置条件全部满足',
  );

  /* 名单里目前只有 玖玥，玖玉 / 玖妹 不在名单上；补进去验互斥是否真的生效 */
  const withJiu = `${DEFAULT_ROSTER}\n56. 玖玉\n57. 玖妹`;
  const g2Response = await arrange(await parse(withJiu), defaultConstraints);
  const g2 = g2Response.groups;
  console.log(`\n【补上 玖玉 / 玖妹 后】${g2.length} 组`);
  g2.forEach((g, i) => {
    const hit = g.filter((p) => ['玖玉', '玖妹', '玖玥'].includes(p.name));
    console.log(`  第 ${i + 1} 组 ${g2Response.slots[i]}  玖系：${hit.map((p) => p.name).join(' ') || '—'}`);
  });
  console.log(
    g2Response.violations.length
      ? `❌ 冲突：${g2Response.violations.map((v) => v.message).join('；')}`
      : '✅ 三人已分散在不同组',
  );

  /* 均衡度对比 */
  console.log('\n【极差对比】');
  const run = async (label: string, constraints: ConstraintSet, strategy: 'snake' | 'snake-no-master') => {
    const result = await request<ArrangeResponse>('/api/group/arrange', {
      method: 'POST',
      body: JSON.stringify({ players, groupSize: 8, strategy, constraints: { full: constraints } }),
    });
    const { groups: gs, balance: s } = result;
    console.log(
      `  ${label.padEnd(16)} ${gs.map((g) => (g.reduce((a, p) => a + p.wave, 0) / 10000).toFixed(0)).join('/')}万  极差 ${s.spread}%`,
    );
  };
  const off: ConstraintSet = { ...defaultConstraints, pinned: [], excludeTogether: [] };
  const pinOnly: ConstraintSet = { ...defaultConstraints, excludeTogether: [] };
  await run('无约束', off, 'snake');
  await run('仅固定位', pinOnly, 'snake');
  await run('全约束', defaultConstraints, 'snake');
  await run('全约束+同门回避', defaultConstraints, 'snake-no-master');
}

void main();
