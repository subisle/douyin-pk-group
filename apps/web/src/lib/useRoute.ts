import { useEffect, useState } from 'react';
import { routeFromHash, type Route } from './nav.js';

/** 订阅 hashchange，把 URL 里的路由读成 React 状态 */
export function useRoute(): Route {
  const [route, setRoute] = useState<Route>(() =>
    typeof window === 'undefined' ? 'home' : routeFromHash(window.location.hash),
  );
  useEffect(() => {
    const on = () => setRoute(routeFromHash(window.location.hash));
    window.addEventListener('hashchange', on);
    return () => window.removeEventListener('hashchange', on);
  }, []);
  return route;
}
