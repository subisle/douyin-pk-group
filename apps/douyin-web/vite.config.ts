import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// 后端地址可覆盖：本地开发默认打本机的 pk-server（8787）。
// 之前写死 8790（115 测试实例），换个环境就得改文件，不如走环境变量。
const target = process.env.API_TARGET || 'http://127.0.0.1:8787'

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      '/api': target,
    },
  },
  build: {
    // React/ReactDOM 拆成独立的稳定 vendor chunk（内容不随业务页变），
    // 配合 spa.rs 对 /assets/* 的 immutable 缓存：用户二次打开只拉变了的那几个
    // 页面 chunk，vendor 直接命中浏览器缓存，不再重复下载。
    rollupOptions: {
      output: {
        manualChunks(id) {
          // pnpm 把包放在 node_modules/.pnpm/<pkg>@<ver>/node_modules/<pkg>/ 下，
          // 这里兼容 npm / pnpm 两种布局，把 react + react-dom + scheduler 一起
          // 拆成稳定的 vendor chunk。
          if (/[\\/]node_modules[\\/](?:\.pnpm[\\/][^\\/]+[\\/]node_modules[\\/])?(react|react-dom|scheduler)[\\/]/.test(id)) {
            return 'react'
          }
        },
      },
    },
  },
})
