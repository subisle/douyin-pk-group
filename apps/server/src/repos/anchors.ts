import { queryReadonly, getActiveId, type SourceId } from '../db/mysql.js';
import { dialectOf } from '../db/dialect.js';
import type { AnchorBrief } from '@pk/shared';

interface PersonRow {
  id: number | string;
  name: string;
  avatar_url: string | null;
  status: string | null;
}

const caches = new Map<SourceId, { at: number; rows: AnchorBrief[] }>();
const TTL = 10 * 60 * 1000;

export async function loadAnchors(force = false, sourceId?: SourceId): Promise<AnchorBrief[]> {
  const src = sourceId ?? getActiveId();
  const cache = caches.get(src);
  if (!force && cache && Date.now() - cache.at < TTL) return cache.rows;
  const rows = await queryReadonly<PersonRow>(dialectOf(src).anchorsSql, [], src);
  const brief: AnchorBrief[] = rows.map((r) => ({
    personId: String(r.id),
    name: r.name,
    avatar: r.avatar_url ?? null,
  }));
  caches.set(src, { at: Date.now(), rows: brief });
  return brief;
}

export async function searchAnchors(q: string, limit = 20, sourceId?: SourceId): Promise<AnchorBrief[]> {
  const all = await loadAnchors(false, sourceId);
  const key = q.trim();
  if (!key) return all.slice(0, limit);
  const exact = all.filter((a) => a.name === key);
  const partial = all.filter((a) => a.name !== key && a.name.includes(key));
  return [...exact, ...partial].slice(0, limit);
}

export function clearAnchorCache(): void {
  caches.clear();
}
