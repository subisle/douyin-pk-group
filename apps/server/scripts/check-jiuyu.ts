/** 深挖玖玉：历史音浪、reliable 标记、账号映射 */
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

// 玖玉 的 person 与账号
const persons = await q("SELECT * FROM person WHERE name = '玖玉'");
console.log('person 表记录:', JSON.stringify(persons, null, 1));

// 对比 浩冬 与 玖玉 的每日数据
for (const name of ['玖玉', '浩冬', '玖妹']) {
  const hist = await q(
    `SELECT m.biz_date, m.wave, m.wave_reliable
     FROM daily_metric m JOIN person p ON p.id = m.person_id
     WHERE p.name = ? ORDER BY m.biz_date DESC LIMIT 8`, [name]);
  console.log('\n' + name + ' 最近8天:');
  hist.forEach((r) => console.log('  ', new Date(r.biz_date).toISOString().slice(0, 10), 'wave=' + r.wave, 'reliable=' + r.wave_reliable));
}

// reliable=0 的整体情况（最近一天）
const [rows] = await pool.query(
  `SELECT p.name, m.wave, m.wave_reliable
   FROM daily_metric m JOIN person p ON p.id = m.person_id
   WHERE m.biz_date = (SELECT MAX(biz_date) FROM daily_metric)
     AND (m.wave_reliable = 0 OR m.wave > 0)
   ORDER BY m.wave DESC LIMIT 50`);
const all = rows as any[];
console.log('\n最新一天全部人员(含 unreliable), 共', all.length, '人');
console.log('unreliable 名单:', all.filter((r) => r.wave_reliable === 0).map((r) => r.name + '(' + r.wave + ')'));

await pool.end();
