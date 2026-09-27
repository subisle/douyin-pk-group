import mysql, { type Pool } from 'mysql2/promise';
import 'dotenv/config';

const ALLOWED_PREFIX = /^\s*(select|show|describe|explain)\s/i;
const FORBIDDEN = /\b(insert|update|delete|drop|alter|create|truncate|grant|revoke|replace|rename|lock|set\s+global|load_file|into\s+outfile|handler|call)\b/i;

/**
 * 两个数据源共用一套业务代码，只换连接池 + SQL 方言。
 * A：douyinxs（mysql7:3312）—— 新 schema：person / account / daily_metric
 * B：userxs（mysql5:3310）—— 老 schema：persons / accounts / wave_stats
 */
export type SourceId = 'A' | 'B';

export interface SourceConfig {
  id: SourceId;
  label: string;
  note: string;
  host: string;
  port: number;
  user: string;
  database: string;
  poolLimit: number;
  /** 首日是不是累计快照（A 库是，B 库不是） */
  excludeBaselineDay: boolean;
}

function readSource(id: SourceId): SourceConfig | null {
  const p = `DB_${id}_`;
  const host = process.env[`${p}HOST`];
  const user = process.env[`${p}USER`];
  const name = process.env[`${p}NAME`];
  if (!host || !user || !name) return null;
  return {
    id,
    label: process.env[`${p}LABEL`] ?? `数据源 ${id} · ${user}@${name}`,
    note: process.env[`${p}NOTE`] ?? '',
    host,
    port: Number(process.env[`${p}PORT`] ?? 3306),
    user,
    database: name,
    poolLimit: Number(process.env[`${p}POOL_LIMIT`] ?? 3),
    excludeBaselineDay: process.env[`${p}EXCLUDE_BASELINE`] !== '0',
  };
}

/** 兼容旧写法：没有 DB_A_* 时退回 DB_HOST/DB_USER/... */
function legacySource(): SourceConfig | null {
  const host = process.env.DB_HOST;
  if (!host) return null;
  return {
    id: 'A',
    label: process.env.DB_LABEL ?? '默认数据源',
    note: '',
    host,
    port: Number(process.env.DB_PORT ?? 3306),
    user: process.env.DB_USER ?? '',
    database: process.env.DB_NAME ?? '',
    poolLimit: Number(process.env.DB_POOL_LIMIT ?? 3),
    excludeBaselineDay: process.env.DB_EXCLUDE_BASELINE !== '0',
  };
}

const list: SourceConfig[] = [readSource('A'), readSource('B'), legacySource()]
  .filter((x): x is SourceConfig => x !== null)
  .filter((x, i, arr) => arr.findIndex((y) => y.id === x.id) === i);

if (list.length === 0) {
  throw new Error('没有配置任何数据源：请设置 DB_A_* （或 DB_HOST 等旧变量）');
}

export const SOURCES: SourceConfig[] = list;

let activeId: SourceId = (process.env.DB_SOURCE as SourceId) ?? list[0].id;
if (!SOURCES.some((s) => s.id === activeId)) activeId = list[0].id;

const passwords: Record<string, string> = {
  A: process.env.DB_A_PASSWORD ?? process.env.DB_PASSWORD ?? '',
  B: process.env.DB_B_PASSWORD ?? '',
};

export function listSources(): Array<Omit<SourceConfig, 'host' | 'user'> & { id: SourceId }> {
  return SOURCES.map((s) => ({
    id: s.id,
    label: s.label,
    note: s.note,
    port: s.port,
    database: s.database,
    poolLimit: s.poolLimit,
    excludeBaselineDay: s.excludeBaselineDay,
  }));
}

export function getSource(id?: SourceId): SourceConfig {
  const target = id ?? activeId;
  const hit = SOURCES.find((s) => s.id === target);
  if (!hit) throw new Error(`未知数据源：${target}`);
  return hit;
}

export function getActiveId(): SourceId {
  return activeId;
}

export function setActiveId(id: SourceId): void {
  if (!SOURCES.some((s) => s.id === id)) throw new Error(`未知数据源：${id}`);
  activeId = id;
}

const pools = new Map<SourceId, Pool>();

export function getPool(id?: SourceId): Pool {
  const cfg = getSource(id);
  const hit = pools.get(cfg.id);
  if (hit) return hit;
  const pool = mysql.createPool({
    host: cfg.host,
    port: cfg.port,
    user: cfg.user,
    password: passwords[cfg.id],
    database: cfg.database,
    waitForConnections: true,
    connectionLimit: cfg.poolLimit,
    maxIdle: cfg.poolLimit,
    idleTimeout: 30000,
    connectTimeout: 10000,
    enableKeepAlive: true,
    keepAliveInitialDelay: 10000,
  });
  pools.set(cfg.id, pool);
  return pool;
}

/** 只读查询守卫：只接受 SELECT/SHOW，禁止任何写操作语句 */
export async function queryReadonly<T = Record<string, unknown>>(
  sql: string,
  params: unknown[] = [],
  sourceId?: SourceId,
): Promise<T[]> {
  if (FORBIDDEN.test(sql)) {
    throw new Error('只读连接拒绝执行：检测到写操作关键字');
  }
  if (!ALLOWED_PREFIX.test(sql)) {
    throw new Error('只读连接拒绝执行：仅允许 SELECT / SHOW 语句');
  }
  const [rows] = await getPool(sourceId).query(sql, params);
  return rows as T[];
}

export async function healthCheck(sourceId?: SourceId): Promise<{ ok: boolean; version?: string; error?: string }> {
  try {
    const rows = await queryReadonly<{ v: string }>('SELECT VERSION() AS v', [], sourceId);
    return { ok: true, version: rows[0]?.v };
  } catch (e) {
    return { ok: false, error: (e as Error).message };
  }
}
