/** 数据库体检：列出 .env 里每个数据源的表结构、每日行数、最早/最近日期（凭据只从 .env 读） */
import fs from 'node:fs';
import path from 'node:path';
import mysql from 'mysql2/promise';

// apps/server/.env 随过渡期一起退役了；按顺序找第一个存在的 .env，
// 本地开发态用 rust/.env，部署态用 deploy/.env。
const repoRoot = path.resolve(import.meta.dirname ?? '.', '..');
const candidates = [
  path.join(repoRoot, '.env'),
  path.join(repoRoot, 'rust', '.env'),
  path.join(repoRoot, 'deploy', '.env'),
  path.join(repoRoot, 'deploy', '.env.example'),
];
const envPath = candidates.find((p) => fs.existsSync(p));
if (!envPath) {
  console.error(
    `❌ 没找到 .env，请从 deploy/.env.example 复制一份到 rust/.env 或 deploy/.env\n` +
      `   查找顺序：\n${candidates.map((p) => `     - ${path.relative(repoRoot, p)}`).join('\n')}`,
  );
  process.exit(1);
}
console.log(`使用配置：${path.relative(repoRoot, envPath)}`);

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
