# PK 挑战赛分组程序

只读接入 `douyinxs` 数据库，按音浪高低做均衡分组，支持手动拖拽调整与导出图片。

## 启动

```bash
pnpm install
pnpm dev          # 同时启动后端 8787 与前端 5173
```

打开 http://localhost:5173

- 后端：`http://localhost:8787/api/health`
- 前端通过 Vite 代理 `/api` → `8787`
- 数据库凭据在 `apps/server/.env`（不要提交到仓库）

## 使用流程

1. 顶部确认「音浪口径」：近 N 天（默认 3 天）或总音浪
2. 点「解析并分组」，名单文本默认内置，可展开编辑（接龙格式直接粘贴）
3. 看板里拖动卡片跨组调整，顶部实时显示各组总音浪与极差
4. 「导出图片」生成 PNG 长图，「复制文字版」复制到剪贴板，「保存方案」存到 SQLite

## 名单规则

- 每行第一个名字 = 参赛主播；后面的名字 = 队长/师傅（只标注，不参赛）
  - `啸安➕狼兴` → 参赛者 啸安，师傅 狼兴
  - `啸阳（狼泽）` → 参赛者 啸阳，师傅 狼泽
- 别名映射：默认 `阿楠 → 南方楠`，可在 `packages/shared/src/roster.ts` 的 `DEFAULT_ALIASES` 增改
- 排除名单：默认 `目`，见 `DEFAULT_EXCLUDE`
- 匹配不上的主播会标红，可下拉手动绑定，未绑定按 0 音浪参与分组

## 只读约束

数据库只做查询，三层保障：

1. 所有 SQL 集中在 `apps/server/src/repos/`，不接受外部传入的 SQL 字符串
2. `queryReadonly()` 只放行 `SELECT / SHOW`，含写操作关键字直接抛错
3. 分组方案、别名等写操作落到本地 SQLite（`apps/server/data/app.db`）

## 目录

```
packages/shared   名单解析 + 蛇形分组算法（前后端共用）
apps/server       Fastify + mysql2 只读查询 + node:sqlite
apps/web          React + Vite + dnd-kit 拖拽 + html-to-image 导出
docs/             设计文档
```

## 已知事项

- 本机 `mysql` CLI（9.7.1）连不上该库（`ERROR 2000`），必须用 Node `mysql2`
- 免费库连接数极少，连接池上限 3，音浪结果缓存 10 分钟；改数据后点「强制刷新」
- 库里 `daily_metric` 目前只有 3 天数据，故「近 3 天」与「总音浪」当前结果相同
