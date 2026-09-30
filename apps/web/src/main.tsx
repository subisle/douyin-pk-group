import React from 'react';
import { createRoot } from 'react-dom/client';
import './index.css';
import App from './App.js';
import { AuthGate } from './components/AuthGate.js';
import { ShareView } from './components/ShareView.js';

/**
 * 极简路由：
 * - /s/:planId  对外只读展示页（指定方案）
 * - /st         对外公告页（固定短链，自动指向最新上架的方案，URL 不变）
 * - 其余        管理员后台，包在登录门里
 */
const shareMatch = window.location.pathname.match(/^\/s\/(\d+)\/?$/);
const isAnnouncement = /^\/st\/?$/.test(window.location.pathname);

createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    {shareMatch ? (
      <ShareView planId={shareMatch[1]} />
    ) : isAnnouncement ? (
      <ShareView />
    ) : (
      <AuthGate>
        <App />
      </AuthGate>
    )}
  </React.StrictMode>,
);
