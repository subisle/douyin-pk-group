/** 对比 蛇形均衡 vs 从高到低（内置条件都套用） */
import { DEFAULT_ROSTER, DEFAULT_CONSTRAINTS, autoGroup, computeBalance, type GroupOptions } from '../packages/shared/src/index.js';

const BASE = process.env.API ?? 'http://127.0.0.1:8787';

async function main() {
  const res = await fetch(`${BASE}/api/roster/parse`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ text: DEFAULT_ROSTER, metric: 'lastN', days: 3 }),
  });
  const { players } = (await res.json()) as { players: ReturnType<typeof Object>[] } as never as {
    players: Parameters<typeof autoGroup>[0];
  };

  for (const strategy of ['snake', 'sequential'] as const) {
    const opts: GroupOptions = { groupSize: 8, strategy, constraints: DEFAULT_CONSTRAINTS };
    const groups = autoGroup(players, opts);
    const b = computeBalance(groups);
    console.log(`\n== ${strategy} ==  极差 ${b.spread}%`);
    groups.forEach((grp, i) =>
      console.log(
        `  第 ${i + 1} 组  ${(grp.reduce((s, p) => s + p.wave, 0) / 10000).toFixed(1)}万  ${grp.map((p) => p.name).join(' ')}`,
      ),
    );
  }
  process.exit(0);
}

void main();
