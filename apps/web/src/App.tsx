import { useEffect } from 'react';
import { useStore } from './store.js';
import { useRoute } from './lib/useRoute.js';
import { NavBar } from './components/NavBar.js';
import { HomeView } from './components/HomeView.js';
import { StreamerView } from './components/StreamerView.js';
import { BoardView } from './components/BoardView.js';

export default function App() {
  const route = useRoute();
  const init = useStore((s) => s.init);

  useEffect(() => {
    void init();
  }, [init]);

  return (
    <div className="min-h-screen bg-canvas" style={{ backgroundImage: 'var(--color-canvas-glow)' }}>
      <NavBar route={route} />
      {route === 'home' && <HomeView />}
      {route === 'data' && <StreamerView />}
      {route === 'board' && <BoardView />}
    </div>
  );
}
