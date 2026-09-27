import { DatabaseSync } from 'node:sqlite';
import fs from 'node:fs';
import path from 'node:path';
import 'dotenv/config';

let db: DatabaseSync | null = null;

export function getDb(): DatabaseSync {
  if (db) return db;
  const file = path.resolve(process.env.SQLITE_PATH ?? './data/app.db');
  fs.mkdirSync(path.dirname(file), { recursive: true });
  db = new DatabaseSync(file);
  db.exec(`
    CREATE TABLE IF NOT EXISTS aliases (
      alias TEXT PRIMARY KEY,
      real_name TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS plans (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      title TEXT NOT NULL,
      metric TEXT NOT NULL,
      days INTEGER NOT NULL DEFAULT 3,
      payload TEXT NOT NULL,
      created_at TEXT NOT NULL
    );
  `);
  return db;
}

export function listAliases(): Record<string, string> {
  const rows = getDb().prepare('SELECT alias, real_name FROM aliases').all() as Array<{
    alias: string;
    real_name: string;
  }>;
  return Object.fromEntries(rows.map((r) => [r.alias, r.real_name]));
}

export function upsertAlias(alias: string, realName: string): void {
  getDb()
    .prepare('INSERT INTO aliases (alias, real_name) VALUES (?, ?) ON CONFLICT(alias) DO UPDATE SET real_name = excluded.real_name')
    .run(alias, realName);
}

export function deleteAlias(alias: string): void {
  getDb().prepare('DELETE FROM aliases WHERE alias = ?').run(alias);
}

export interface PlanRow {
  id: number;
  title: string;
  metric: string;
  days: number;
  payload: string;
  created_at: string;
}

export function savePlan(title: string, metric: string, days: number, payload: string): number {
  const res = getDb()
    .prepare('INSERT INTO plans (title, metric, days, payload, created_at) VALUES (?, ?, ?, ?, ?)')
    .run(title, metric, days, payload, new Date().toISOString());
  return Number(res.lastInsertRowid);
}

export function listPlans(): PlanRow[] {
  return getDb()
    .prepare('SELECT id, title, metric, days, payload, created_at FROM plans ORDER BY id DESC LIMIT 50')
    .all() as PlanRow[];
}

export function getPlan(id: number): PlanRow | undefined {
  return getDb()
    .prepare('SELECT id, title, metric, days, payload, created_at FROM plans WHERE id = ?')
    .get(id) as PlanRow | undefined;
}

export function deletePlan(id: number): void {
  getDb().prepare('DELETE FROM plans WHERE id = ?').run(id);
}
