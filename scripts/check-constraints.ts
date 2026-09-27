/**
 * 内置条件自检（真实名单 + 真实音浪）：
 * 1) 啸泽是否在第 1 组、啸帆是否在最后一组
 * 2) 玖玉 / 玖妹 / 玖玥 是否两两不同组（名单里补上玖玉、玖妹后验）
 * 3) 啸昭 / 啸寻 是否至少相隔 4 组
 * 4) 开赛时间列是否 08:15 起每 15 分钟一组
 * 5) 顺带打印极差，对比四种策略/约束组合
 *
 * 用法：cd apps/server && node --import tsx ../../scripts/check-constraints.ts
 */
import {
  DEFAULT_ROSTER,
  DEFAULT_CONSTRAINTS,
  autoGroup,
  computeBalance,
  findViolations,
  slotTime,
  type ConstraintSet,
  type Player,
} from '../packages/shared/src/index.js';

const BASE = process.env.API ?? 'http://127.0.0.1:8787';

async function parse(text: string): Promise<Player[]> {
  const res = await fetch(`${BASE}/api/roster/parse`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ text, metric: 'total', days: 3 }),
  });
  return ((await res.json()) as { players: Player[] }).players;
}

async function main() {
  const players = await parse(DEFAULT_ROSTER);
  const active = players.filter((p) => !p.excluded);
  const groups = autoGroup(players, { groupSize: 8, strategy: 'snake', constraints: DEFAULT_CONSTRAINTS });

  console.log(`参赛 ${active.length} 人 → ${groups.length} 组（每组上限 8）\n`);
  groups.forEach((g, i) => {
    const total = g.reduce((s, p) => s + p.wave, 0);
    console.log(
      `第 ${i + 1} 组  ${slotTime(i, DEFAULT_CONSTRAINTS.schedule)}  ${String(g.length).padStart(2)} 人  ${(
        total / 10000
      ).toFixed(1)}万  ${g.map((p) => p.name).join(' ')}`,
    );
  });

  const b = computeBalance(groups);
  console.log(`\n最高 ${(b.max / 10000).toFixed(1)}万 / 最低 ${(b.min / 10000).toFixed(1)}万 / 极差 ${b.spread}%`);

  const bad = findViolations(groups, DEFAULT_CONSTRAINTS);
  const at = (name: string) => groups.findIndex((g) => g.some((p) => p.name === name)) + 1;
  console.log(
    `\n啸昭=第 ${at('啸昭')} 组，啸寻=第 ${at('啸寻')} 组（需组号差 ≥ 4，实际差 ${Math.abs(at('啸昭') - at('啸寻'))}）`,
  );
  console.log(bad.length ? `\n❌ 冲突：\n${bad.map((v) => '  - ' + v.message).join('\n')}` : '\n✅ 内置条件全部满足');

  /* 名单里目前只有 玖玥，玖玉 / 玖妹 不在名单上；补进去验互斥是否真的生效 */
  const withJiu = `${DEFAULT_ROSTER}\n56. 玖玉\n57. 玖妹`;
  const g2 = autoGroup(await parse(withJiu), {
    groupSize: 8,
    strategy: 'snake',
    constraints: DEFAULT_CONSTRAINTS,
  });
  console.log(`\n【补上 玖玉 / 玖妹 后】${g2.length} 组`);
  g2.forEach((g, i) => {
    const hit = g.filter((p) => ['玖玉', '玖妹', '玖玥'].includes(p.name));
    console.log(`  第 ${i + 1} 组 ${slotTime(i, DEFAULT_CONSTRAINTS.schedule)}  玖系：${hit.map((p) => p.name).join(' ') || '—'}`);
  });
  const bad2 = findViolations(g2, DEFAULT_CONSTRAINTS);
  console.log(bad2.length ? `❌ 冲突：${bad2.map((v) => v.message).join('；')}` : '✅ 三人已分散在不同组');

  /* 均衡度对比 */
  console.log('\n【极差对比】');
  const run = (label: string, constraints: ConstraintSet, strategy: 'snake' | 'snake-no-master') => {
    const gs = autoGroup(players, { groupSize: 8, strategy, constraints });
    const s = computeBalance(gs);
    console.log(
      `  ${label.padEnd(16)} ${gs.map((g) => (g.reduce((a, p) => a + p.wave, 0) / 10000).toFixed(0)).join('/')}万  极差 ${s.spread}%`,
    );
  };
  const off: ConstraintSet = { ...DEFAULT_CONSTRAINTS, pinned: [], excludeTogether: [] };
  const pinOnly: ConstraintSet = { ...DEFAULT_CONSTRAINTS, excludeTogether: [] };
  run('无约束', off, 'snake');
  run('仅固定位', pinOnly, 'snake');
  run('全约束', DEFAULT_CONSTRAINTS, 'snake');
  run('全约束+同门回避', DEFAULT_CONSTRAINTS, 'snake-no-master');
}

void main();
