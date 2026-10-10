/** 对比 蛇形均衡 vs 从高到低（内置条件都套用） */
import { DEFAULT_ROSTER } from '../packages/shared/src/roster.js';
import type { Player } from '../packages/shared/src/types.js';

const BASE = process.env.API ?? 'http://127.0.0.1:8787';

interface ArrangeResponse {
  groups: Player[][];
  balance: { spread: number };
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    ...init,
    headers: { 'Content-Type': 'application/json', ...(init?.headers ?? {}) },
  });
  if (!res.ok) throw new Error(`${res.status} ${await res.text()}`);
  return (await res.json()) as T;
}

async function main() {
  const { players } = await request<{ players: Player[] }>('/api/roster/parse', {
    method: 'POST',
    body: JSON.stringify({ text: DEFAULT_ROSTER, metric: 'lastN', days: 3 }),
  });

  for (const strategy of ['snake', 'sequential-last-n'] as const) {
    const result = await request<ArrangeResponse>('/api/group/arrange', {
      method: 'POST',
      body: JSON.stringify({ players, groupSize: 8, strategy }),
    });
    const { groups, balance: b } = result;
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
