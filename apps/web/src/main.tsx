import React from 'react';
import { createRoot } from 'react-dom/client';
import './index.css';
import App from './App.js';
import { AuthGate } from './components/AuthGate.js';
import { ShareView } from './components/ShareView.js';

/**
 * 极简路由：
 * - /s/:planId  对外只读展示页（无需口令，手机 / PC 自适应）
 * - 其余        管理员后台，包在登录门里
 */
const shareMatch = window.location.pathname.match(/^\/s\/(\d+)\/?$/);

createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    {shareMatch ? <ShareView planId={shareMatch[1]} /> : <AuthGate><App /></AuthGate>}
  </React.StrictMode>,
);
