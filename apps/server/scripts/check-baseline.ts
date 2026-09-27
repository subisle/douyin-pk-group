/** 验证：基线日(累计快照)排名 vs 近5日增量排名 */
import fs from 'node:fs';
import mysql from 'mysql2/promise';

const envPath = '/Volumes/2t/it/抖音pk分组/apps/server/.env';
const env: Record<string, string> = {};
for (const line of fs.readFileSync(envPath, 'utf8').split('\n')) {
  const m = line.match(/^\s*([A-Z0-9_]+)\s*=\s*(.*?)\s*$/);
  if (m) env[m[1]] = m[2];
}
const pool = mysql.createPool({
  host: env.DB_A_HOST, port: Number(env.DB_A_PORT ?? 3306), user: env.DB_A_USER,
  password: env.DB_A_PASSWORD ?? '', database: env.DB_A_NAME, connectionLimit: 2,
});
const q = async (sql: string, params: unknown[] = []) => {
  const [rows] = await pool.query(sql, params);
  return rows as any[];
};

const baseline = (await q('SELECT MIN(biz_date) AS d FROM daily_metric'))[0].d;
const w1 = `
  SELECT p.name,
    SUM(CASE WHEN m.biz_date = ? THEN m.wave ELSE 0 END) AS snap,
    SUM(CASE WHEN m.biz_date > ? THEN m.wave ELSE 0 END) AS delta,
    SUM(CASE WHEN m.biz_date >= ? THEN m.wave ELSE 0 END) AS total
  FROM daily_metric m JOIN person p ON p.id = m.person_id
  WHERE m.wave_reliable = 1
  GROUP BY p.id, p.name`;

const rows = await q(w1, [baseline, baseline, baseline]);
const fmt = (n: number) => (n / 10000).toFixed(1) + '万';
const bySnap = [...rows].sort((a, b) => b.snap - a.snap);
console.log('=== 基线日快照(≈累计总音浪) Top10 ===');
bySnap.slice(0, 10).forEach((r, i) =>
  console.log(`  第${i + 1}名 ${r.name}  快照=${fmt(r.snap)}  近5日增量=${fmt(r.delta)}  总计=${fmt(r.total)}`));

const jy = bySnap.findIndex((r) => r.name === '玖玉') + 1;
console.log(`\n玖玉在快照榜排第 ${jy} 名`);

// 近5日增量榜
const byDelta = [...rows].sort((a, b) => b.delta - a.delta);
console.log('\n=== 近5日增量榜 Top5 ===');
byDelta.slice(0, 5).forEach((r, i) =>
  console.log(`  第${i + 1}名 ${r.name}  增量=${fmt(r.delta)}`));
const jyd = byDelta.findIndex((r) => r.name === '玖玉') + 1;
console.log(`玖玉在增量榜排第 ${jyd} 名`);

await pool.end();
