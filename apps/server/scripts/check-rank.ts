/** 检查音浪排名：谁才是第1名，玖玉/玖妹/玖玥排第几 */
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

const [dates] = await pool.query<mysql.QueryResult>(
  'SELECT DISTINCT biz_date FROM daily_metric ORDER BY biz_date DESC LIMIT 5');
const dateList = dates as { biz_date: string }[];
console.log('最近日期:', dateList.map((r) => r.biz_date));

const [base] = await pool.query('SELECT MIN(biz_date) AS d FROM daily_metric');
const baseRow = (base as { d: string }[])[0];
console.log('基线日(被排除):', baseRow.d);

const latest = dateList[0].biz_date;
const [rows] = await pool.query(
  `SELECT p.name, p.id, SUM(m.wave) AS wave
   FROM daily_metric m JOIN person p ON p.id = m.person_id
   WHERE m.biz_date = ? AND m.wave_reliable = 1
   GROUP BY p.id, p.name ORDER BY wave DESC LIMIT 12`, [latest]);
console.log('\n' + latest + ' Top12 音浪排名:');
(rows as any[]).forEach((r, i) => console.log('  第' + (i + 1) + '名 ' + r.name + '  ' + Number(r.wave).toLocaleString()));

const [three] = await pool.query(
  `SELECT p.name, SUM(m.wave) AS wave
   FROM daily_metric m JOIN person p ON p.id = m.person_id
   WHERE m.biz_date = ? AND p.name IN ('玖玉','玖妹','玖玥') AND m.wave_reliable = 1
   GROUP BY p.name`, [latest]);
console.log('\n三人音浪:', (three as any[]).map((r) => r.name + '=' + Number(r.wave).toLocaleString()));

const [dup] = await pool.query(
  "SELECT name, COUNT(*) AS c FROM person WHERE name LIKE '玖%' GROUP BY name");
console.log('\n玖字头人员:', (dup as any[]).map((r) => r.name + ' x' + r.c));

await pool.end();
