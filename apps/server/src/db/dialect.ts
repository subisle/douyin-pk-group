import type { SourceId } from './mysql.js';

/**
 * 两个库的 SQL 方言差异集中在这里。对外统一约定：
 * - 人员表查询返回：id / name / avatar_url / status
 * - 日期查询返回：d（YYYY-MM-DD）
 * - 音浪查询返回：k（人员 id）/ w（区间内音浪合计）
 */
export interface Dialect {
  anchorsSql: string;
  datesSql: string;
  /** 基线日 SQL：首日记的是累计音浪。没有这个问题的库返回 null */
  baselineSql: string | null;
  wavesSql: (placeholders: string) => string;
}

/** A：douyinxs —— person / account / daily_metric */
const DIALECT_A: Dialect = {
  anchorsSql: `SELECT id, name, avatar_url, status FROM person
     WHERE deleted_at IS NULL AND (status IS NULL OR status <> 'left')
     ORDER BY name`,
  datesSql: `SELECT DATE_FORMAT(biz_date, '%Y-%m-%d') AS d FROM daily_metric GROUP BY d ORDER BY d DESC`,
  baselineSql: `SELECT DATE_FORMAT(MIN(biz_date), '%Y-%m-%d') AS d FROM daily_metric`,
  wavesSql: (ph) => `SELECT person_id AS k, SUM(wave) AS w FROM daily_metric
     WHERE wave_reliable = 1 AND DATE_FORMAT(biz_date, '%Y-%m-%d') IN (${ph})
     GROUP BY person_id`,
};

/** B：userxs —— persons / accounts / wave_stats（anchor_id 是关联键） */
const DIALECT_B: Dialect = {
  anchorsSql: `SELECT id, name, NULL AS avatar_url, 'active' AS status FROM persons ORDER BY name`,
  datesSql: `SELECT DATE_FORMAT(date, '%Y-%m-%d') AS d FROM wave_stats GROUP BY d ORDER BY d DESC`,
  baselineSql: null,
  wavesSql: (ph) => `SELECT a.person_id AS k, SUM(ws.wave_value) AS w FROM wave_stats ws
     JOIN accounts a ON a.anchor_id = ws.anchor_id
     WHERE DATE_FORMAT(ws.date, '%Y-%m-%d') IN (${ph})
     GROUP BY a.person_id`,
};

export function dialectOf(id: SourceId): Dialect {
  return id === 'B' ? DIALECT_B : DIALECT_A;
}
