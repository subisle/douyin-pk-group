/** 数据库体检：列出 .env 里每个数据源的表结构、每日行数、最早/最近日期（凭据只从 .env 读） */
import fs from 'node:fs';
import path from 'node:path';
import mysql from 'mysql2/promise';

const envPath = path.resolve(import.meta.dirname ?? '.', '../apps/server/.env');
const env: Record<string, string> = {};
for (const line of fs.readFileSync(envPath, 'utf8').split('\n')) {
  if (line.trim().startsWith('#')) continue;
  const m = line.match(/^\s*([A-Z0-9_]+)\s*=\s*(.*?)\s*$/);
  if (m) env[m[1]] = m[2];
}

const SOURCES = ['DB_A', 'DB_B']
  .map((p) =>
    env[`${p}_HOST`]
      ? {
          label: p,
          cfg: {
            host: env[`${p}_HOST`],
            port: Number(env[`${p}_PORT`] ?? 3306),
            user: env[`${p}_USER`],
            password: env[`${p}_PASSWORD`] ?? '',
            database: env[`${p}_NAME`],
            connectionLimit: 2,
            connectTimeout: 15000,
          } as mysql.PoolOptions,
        }
      : null,
  )
  .filter((x): x is { label: string; cfg: mysql.PoolOptions } => x !== null);

async function main() {
  for (const c of SOURCES) {
    console.log(`\n########## ${c.label} ${c.cfg.host}:${(c.cfg as { port: number }).port}/${c.cfg.database} ##########`);
    let pool: mysql.Pool;
    try {
      pool = mysql.createPool(c.cfg);
    } catch (e) {
      console.log('❌ 建池失败：', (e as Error).message);
      continue;
    }
    try {
      const [tabs] = await pool.query(
        `SELECT table_name AS t FROM information_schema.tables WHERE table_schema = DATABASE() ORDER BY table_name`,
      );
      console.log('表：', (tabs as Array<{ t: string }>).map((r) => r.t).join(' '));
    } catch (e) {
      console.log('❌ 查询失败：', (e as Error).message);
    }
    await pool.end();
  }
  process.exit(0);
}

void main();
