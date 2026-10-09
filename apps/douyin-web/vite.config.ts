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
})
