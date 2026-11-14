import { defineConfig, type Plugin } from 'vite'
import react from '@vitejs/plugin-react'
import { readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { brotliCompressSync, constants as zlibConstants, gzipSync } from 'node:zlib'

// 后端地址可覆盖：本地开发默认打本机的 pk-server（8787）。
// 之前写死 8790（115 测试实例），换个环境就得改文件，不如走环境变量。
const target = process.env.API_TARGET || 'http://127.0.0.1:8787'

/** 构建期预压缩：服务端可直接返回 .br/.gz，避免每个缓存未命中请求重复压缩。 */
function precompressAssets(): Plugin {
  const extensions = new Set(['.html', '.js', '.mjs', '.css', '.json', '.svg', '.txt', '.xml'])
  return {
    name: 'pk-precompress-assets',
    apply: 'build',
    closeBundle() {
      const dist = join(process.cwd(), 'dist')
      const files = [join(dist, 'index.html'), join(dist, 'resources/videos.json')]
      const assets = join(dist, 'assets')
      try {
        for (const name of readdirSync(assets)) files.push(join(assets, name))
      } catch {
        /* assets 目录不存在时只压缩根文件 */
      }
      for (const file of files) {
        let stat
        try {
          stat = statSync(file)
        } catch {
          continue
        }
        if (!stat.isFile() || stat.size < 512) continue
        const dot = file.lastIndexOf('.')
        const ext = dot >= 0 ? file.slice(dot).toLowerCase() : ''
        if (!extensions.has(ext)) continue
        const raw = readFileSync(file)
        const gz = gzipSync(raw, { level: 9 })
        const br = brotliCompressSync(raw, {
          params: {
            [zlibConstants.BROTLI_PARAM_QUALITY]: 11,
            [zlibConstants.BROTLI_PARAM_SIZE_HINT]: raw.length,
          },
        })
        if (gz.length < raw.length) writeFileSync(`${file}.gz`, gz)
        if (br.length < raw.length) writeFileSync(`${file}.br`, br)
      }
    },
  }
}

export default defineConfig({
  plugins: [react(), precompressAssets()],
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
