export * from './types.js';
export * from './roster.js';
export * from './constraints.js';

export function formatWave(n: number): string {
  if (n >= 1_0000_0000) return (n / 1_0000_0000).toFixed(2) + '亿';
  if (n >= 1_0000) return (n / 1_0000).toFixed(1) + '万';
  return String(n);
}
