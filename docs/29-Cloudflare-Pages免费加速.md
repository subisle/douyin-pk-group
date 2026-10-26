# 29 · Cloudflare Pages 免费加速层

> 2026-10-26 落地。目标是在**不备案、不改 64 数据库和 Caddy** 的前提下，减少
> 大陆访问的静态资源回源与重复 API 请求。它不是大陆 CDN，不能替代 ICP + EdgeOne/ESA。

## 1. 结构

```text
公开展示页（原来的根路径不变）
  pk.subisle.com/
    ├─ HTML / CSS / JS → Cloudflare 边缘缓存 30 秒
    ├─ 公告 / 计划 / 头像 → Cloudflare 边缘缓存 30 秒
    └─ 登录后的榜单与个人数据 → 不缓存，仍按 token 校验

管理端（静态 SPA 拆到 Pages）
  pk.subisle.com/admin* → 302 → app.subisle.com/admin*
  app.subisle.com
    ├─ HTML / JS / CSS / 字体 → Cloudflare Pages
    └─ /api/* → Pages Function 同域代理
                     ↓
                pk.subisle.com/api/*
                     ↓
                     64 源站
```

- 公开展示页是 Rust 内嵌的 674KB HTML，不能直接拿 React 管理端 Pages 文件替换；
  因此主域保留原页面，只做 HTML 边缘缓存。
- React 管理端 `apps/douyin-web` 拆到 `app.subisle.com`，`/admin*` 自动跳转过去。
- Pages Function 原样转发 `Authorization`、`X-My-Token` 等请求头，不需要改前端或 CORS。
- 登录、个人、管理端、写操作、SSE 全部不缓存。

## 2. 代码

| 文件 | 作用 |
| --- | --- |
| `apps/douyin-web/functions/api/[[path]].js` | Pages Function：`app.subisle.com/api/*` → `pk.subisle.com/api/*` |
| `apps/douyin-web/public/_routes.json` | 只有 `/api/*` 触发 Pages Function，静态资源不计 Function 调用 |
| `apps/douyin-web/public/_redirects` | SPA history 路由回退到 `index.html` |
| `apps/douyin-web/public/_headers` | `/assets/*`、`/fonts/*` 一年 immutable；HTML 不复用旧壳 |
| `.github/workflows/pages.yml` | 前端变更后自动构建并部署 Cloudflare Pages |

## 3. Cloudflare 资源

- Pages 项目：`pk-web`
- 自定义域名：`app.subisle.com`
- 管理员入口：`pk.subisle.com/admin*` 302 → `https://app.subisle.com/admin*`
- 公开展示页缓存：`/`、`/report`、`/lineage`、`/my*`、`/wechat*`、`/group*`、`/st/*`
- 真正公开的 API 缓存：
  - `/api/public/announcements`
  - `/api/public/plans`
  - `/api/public/avatars/*`
- 登录榜单、`/api/public/teams/*`、个人接口、验证码、管理端和所有 POST 请求均不缓存。
- 边缘 TTL：30 秒；数据导入后最多 30 秒自动刷新，不需要手工清缓存。
- IP 访问门禁若以后重新开启，必须同步复核 HTML 缓存规则，避免缓存绕过地域限制。

## 4. 部署

GitHub Actions 需要仓库 Secrets：

- `CLOUDFLARE_API_TOKEN`
- `CLOUDFLARE_ACCOUNT_ID`

手动部署：

```bash
pnpm install --frozen-lockfile
pnpm -F @pk/douyin-web build
cd apps/douyin-web
CLOUDFLARE_API_TOKEN=... CLOUDFLARE_ACCOUNT_ID=... \
  pnpm dlx wrangler@4.152.0 pages deploy dist --project-name pk-web --branch main
```

## 5. 回滚

1. 删除 `pk.subisle.com/admin*` 的 302 Page Rule；
2. 删除 Pages 自定义域名 `app.subisle.com`；
3. `pk.subisle.com` 原 A 记录、公开展示页和 64 源站不受影响，API 始终可用。

## 6. 效果边界

- 免费、无需备案可以改善：首屏静态资源、重复访问、公共数据边缘命中、源站压力；
- 不能解决：大陆到 Cloudflare 境外 PoP 的国际链路抖动；
- 若需要稳定大陆节点，仍必须做 ICP 备案并使用 EdgeOne、阿里云 ESA 等国内 CDN。
