import React from 'react';
import { createRoot } from 'react-dom/client';
import './index.css';
import App from './App.js';
import { ShareView } from './components/ShareView.js';

/** 极简路由：/s/:planId = 对外只读展示页，其余走主应用 */
const shareMatch = window.location.pathname.match(/^\/s\/(\d+)\/?$/);

createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    {shareMatch ? <ShareView planId={shareMatch[1]} /> : <App />}
  </React.StrictMode>,
);
