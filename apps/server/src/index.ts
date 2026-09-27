import Fastify from 'fastify';
import cors from '@fastify/cors';
import 'dotenv/config';
import { healthCheck, listSources, setActiveId, getActiveId, getSource, type SourceId } from './db/mysql.js';
import { loadAnchors, searchAnchors, clearAnchorCache } from './repos/anchors.js';
import { getAvailableDates, getBaselineDate, getWaves, clearWaveCache } from './repos/waves.js';
import { matchRoster } from './services/match.js';
import {
  deleteAlias,
  deletePlan,
  getPlan,
  listAliases,
  listPlans,
  savePlan,
  upsertAlias,
} from './db/sqlite.js';
import type { WaveMetric } from '@pk/shared';

const app = Fastify({ logger: { level: 'info' } });
await app.register(cors, { origin: true });

app.get('/api/health', async () => {
  const db = await healthCheck();
  return { server: 'ok', db };
});

/** 数据源列表 + 当前源（前端切换用） */
app.get('/api/sources', async () => ({
  active: getActiveId(),
  activeLabel: getSource().label,
  items: listSources(),
}));

app.post('/api/sources/active', async (req) => {
  const body = (req.body ?? {}) as { id?: string };
  if (!body.id) throw app.httpErrors.badRequest('缺少数据源 id');
  setActiveId(body.id as SourceId);
  clearWaveCache();
  clearAnchorCache();
  return { active: getActiveId() };
});

app.post('/api/sources/:id/test', async (req) => {
  const id = (req.params as { id: string }).id as SourceId;
  const db = await healthCheck(id);
  let rows = 0;
  let latest: string | null = null;
  if (db.ok) {
    const dates = await getAvailableDates(true, id);
    latest = dates[0] ?? null;
    rows = (await loadAnchors(true, id)).length;
  }
  return { id, ...db, latestDate: latest, anchorCount: rows };
});

app.get('/api/meta', async () => {
  const [dates, baselineDate, anchors, aliases] = await Promise.all([
    getAvailableDates(),
    getBaselineDate(),
    loadAnchors(),
    Promise.resolve(listAliases()),
  ]);
  return {
    dates,
    latestDate: dates[0] ?? null,
    baselineDate,
    anchorCount: anchors.length,
    aliases,
    sourceId: getActiveId(),
    sourceLabel: getSource().label,
  };
});

app.post('/api/roster/parse', async (req) => {
  const body = (req.body ?? {}) as {
    text?: string;
    metric?: WaveMetric;
    days?: number;
    aliases?: Record<string, string>;
    exclude?: string[];
    force?: boolean;
  };
  if (!body.text?.trim()) throw app.httpErrors.badRequest('缺少名单文本');
  return matchRoster({
    text: body.text,
    metric: body.metric ?? 'lastN',
    days: body.days ?? 3,
    aliases: body.aliases,
    exclude: body.exclude,
    force: body.force,
  });
});

app.get('/api/anchors', async (req) => {
  const q = String((req.query as { q?: string }).q ?? '');
  return { items: await searchAnchors(q, 30) };
});

app.post('/api/waves', async (req) => {
  const body = (req.body ?? {}) as { metric?: WaveMetric; days?: number; personIds?: string[]; force?: boolean };
  return getWaves(body.metric ?? 'lastN', body.days ?? 3, body.personIds, body.force);
});

app.post('/api/cache/refresh', async () => {
  clearWaveCache();
  clearAnchorCache();
  await getAvailableDates(true);
  await loadAnchors(true);
  return { ok: true };
});

app.get('/api/aliases', async () => ({ items: listAliases() }));
app.post('/api/aliases', async (req) => {
  const { alias, realName } = (req.body ?? {}) as { alias?: string; realName?: string };
  if (!alias || !realName) throw app.httpErrors.badRequest('缺少 alias 或 realName');
  upsertAlias(alias, realName);
  return { ok: true };
});
app.delete('/api/aliases/:alias', async (req) => {
  const { alias } = req.params as { alias: string };
  deleteAlias(decodeURIComponent(alias));
  return { ok: true };
});

app.get('/api/plans', async () => ({ items: listPlans() }));
app.post('/api/plans', async (req) => {
  const { title, metric, days, payload } = (req.body ?? {}) as {
    title?: string;
    metric?: string;
    days?: number;
    payload?: unknown;
  };
  if (!payload) throw app.httpErrors.badRequest('缺少 payload');
  const id = savePlan(title ?? '未命名方案', metric ?? 'lastN', days ?? 3, JSON.stringify(payload));
  return { id };
});
app.get('/api/plans/:id', async (req) => {
  const { id } = req.params as { id: string };
  const row = getPlan(Number(id));
  if (!row) throw app.httpErrors.notFound('方案不存在');
  return { ...row, payload: JSON.parse(row.payload) };
});
app.delete('/api/plans/:id', async (req) => {
  const { id } = req.params as { id: string };
  deletePlan(Number(id));
  return { ok: true };
});

const port = Number(process.env.PORT ?? 8787);
try {
  await app.listen({ port, host: '0.0.0.0' });
  const db = await healthCheck();
  console.log(`[server] http://localhost:${port}  database=${db.ok ? db.version : 'FAILED ' + db.error}`);
} catch (err) {
  console.error('[server] 启动失败', err);
  process.exit(1);
}
