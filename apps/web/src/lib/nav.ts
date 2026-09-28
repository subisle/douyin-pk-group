/**
 * 三个顶层页面的路由。
 *
 * 状态只存在 hash 里，不占 React context：这样刷新、前进后退、直接把
 * 「主播数据」页的链接发群里都能落到同一个页面（preflight A4.1 要求
 * tab 状态可分享、可回退）。
 */
export type Route = 'home' | 'data' | 'board';

export const ROUTES: Array<{ id: Route; label: string; hint: string }> = [
  { id: 'home', label: '首页', hint: '今日概览与入口' },
  { id: 'data', label: '主播数据', hint: '音浪明细与排行' },
  { id: 'board', label: 'PK 分组', hint: '均衡分组与导出' },
];

const IDS = new Set<string>(ROUTES.map((r) => r.id));

export function routeFromHash(hash: string): Route {
  const id = hash.replace(/^#\/?/, '');
  return IDS.has(id) ? (id as Route) : 'home';
}

export function hrefFor(route: Route): string {
  return `#/${route}`;
}

/** 切页时顺手把页面滚回顶部，否则从长列表切过去会停在半空 */
export function go(route: Route): void {
  if (routeFromHash(window.location.hash) === route) {
    window.scrollTo({ top: 0, behavior: 'smooth' });
    return;
  }
  window.location.hash = `/${route}`;
}
