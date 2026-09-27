# PK 挑战赛分组程序

只读接入 `douyinxs` 数据库，按音浪高低做均衡分组，支持手动拖拽调整与导出图片。
**后端已全量迁移到 Rust**（axum + sqlx/MySQL + SQLite），前端保留 React 只做界面与拖拽，
所有分组算法都在 `pk-core`（Rust）里算，浏览器不再跑第二份算法。

## 架构

```
rust/crates/pk-core     算法核心：名单解析 / 内置约束 / 均衡分组（纯函数，零 IO，有单测）
rust/crates/pk-server   HTTP 服务：axum + sqlx(MySQL 只读) + SQLite + 静态托管
  ├─ auth.rs            管理员口令鉴权（内存 token）
  ├─ public.rs          对外只读展示数据（裁剪字段 + 实时音浪）
  ├─ grouping.rs        /api/group/*：arrange / inspect / polish / constraints
  ├─ matching.rs        名单 -> 主播匹配 + read_id（BIGINT UNSIGNED 兼容）
  ├─ dialect.rs         A/B 库 SQL 方言 + 日期白名单内联
  └─ mysql.rs           多数据源连接池 + 只读守卫
apps/web                React + Vite + dnd-kit 拖拽 + html-to-image 导出
  ├─ components/AuthGate.tsx   登录门
  └─ components/ShareView.tsx  对外展示页
packages/shared         前端仍用的展示工具与类型（算法已全部迁到 Rust，这里只留纯函数）
```

数据流：前端把 players 发到 `/api/group/arrange`（自动分组）或 `/api/group/inspect`（拖拽后体检），
后端返回分组结果 + 均衡度 + 约束冲突，前端只负责展示。这样算法只有一份实现，不会前后端不一致。

## 管理员后台 vs 对外展示

| 路径 | 谁能看 | 内容 |
| --- | --- | --- |
| `/` | 需口令 | 管理员看板：解析、拖拽、锁定、撤销、均衡、导出、方案管理 |
| `/s/:planId` | 任何人 | 对外只读：音浪数据 + 分组，手机单列 / 桌面多列自适应 |

**鉴权**（`rust/crates/pk-server/src/auth.rs`）

- `.env` 里设了 `ADMIN_PASSWORD` → `/api/*` 全部要口令，`Authorization: Bearer <token>`
- 只有 `/api/public/*`、`/api/auth/login`、`/api/health` 免鉴权
- 口令换 token 走 `POST /api/auth/login`，token 只存内存，进程重启即失效，空闲 8 小时自动清理
- **没设 `ADMIN_PASSWORD` 时后端完全不鉴权**（本地开发方便），启动日志会打警告；生产务必配上
- CORS 默认只允许同源（前后端同端口），跨域时用 `CORS_ORIGINS` 显式放行

**对外展示页**（`rust/crates/pk-server/src/public.rs`）

- 方案要先「上架」才对外可见：保存方案后点「分享链接」会自动上架；历史方案里也能上架/下架
- 接口 `GET /api/public/plans/:id` 已裁剪字段：**不返回** personId / key / 数据源 / 别名等内部信息
- 音浪按方案口径（`total` 或 `lastN`）**实时取**，走 WaveCache；数据库挂了自动降级用方案快照并标 `stale`
- 页面两个视图：**分组**（组内音浪排 + 组间占比条）和**音浪排行**（全场名次，同分并列）
- 手机：单列卡片 + 吸顶汇总条；≥640 两列；≥1024 三列；另有 ≥640 才显示的组内音浪条

**后台展示页入口**：`TopBar` 的「历史方案」里有上架状态、复制链接、上架/下架。

## 本地开发

```bash
# 后端（Rust）
cd rust && cargo run -p pk-server          # 默认 8787，读 ../apps/server/.env

# 前端
pnpm install && pnpm --filter @pk/web dev   # 5173，/api 代理到 8787
```

数据库凭据在 `apps/server/.env`（不要提交到仓库）。Rust 端自己解析 `.env`（值里有中文和空格，dotenvy 会报错）。

## 构建与部署

构建全部走 GitHub Actions（`.github/workflows/ci.yml`）：
- `rust` job：`cargo test` / `cargo clippy -D warnings` / `cargo fmt --check`
- `web` job：`tsc --noEmit` + `vite build`
- `docker` job：构建多架构镜像（amd64 + arm64）推到 `ghcr.io/subisle/douyin-pk-group`

部署到 64（镜像由 Actions 构建，本机只 pull）：

```bash
cd deploy
cp .env.example .env   # 至少填 ADMIN_PASSWORD 和 DB_A_* 只读账号
docker compose -f docker-compose.yml up -d
```

镜像 tag：`latest` 和 commit SHA。构建完成后拉取：

```bash
docker compose -f deploy/docker-compose.yml pull && docker compose -f deploy/docker-compose.yml up -d
```

服务只监听 `127.0.0.1:8787`，公网入口交给 Caddy 反代。
`deploy/Caddyfile.example` 是可直接用的配置：`/s/*` 放开展示页并给短缓存，
其余路径 `no-store` 防止后台看到旧分组，另加了限流。

```bash
# 64 上
cp deploy/Caddyfile.example /etc/caddy/Caddyfile.d/pk-group.caddy
caddy reload
```

拿到的对外展示 URL 就是 `https://你的域名/s/<方案号>`。

## 本地开发机上验证（64）

代码在 64 上编译测试，本机不装 Rust 工具链：

```bash
scripts/sync64.sh            # rsync 到 64:/opt/it/douyin-pk-group
scripts/sync64.sh --check    # 同步后跑 cargo fmt / clippy / test + tsc
```

用 rsync 而不是 tar：tar 是覆盖式，本地删掉的文件在 64 上会残留，编译报错会指向一个本地已不存在的文件。

## 名单规则

- 每行第一个名字 = 参赛主播；后面的名字 = 队长/师傅（只标注，不参赛）
  - `啸安➕狼兴` → 参赛者 啸安，师傅 狼兴
  - `啸阳（狼泽）` → 参赛者 啸阳，师傅 狼泽
- 别名映射：默认 `阿楠 → 南方楠`，可在 SQLite `aliases` 表增改
- 排除名单：默认 `目`
- 匹配不上的主播会标红，可下拉手动绑定，未绑定按 0 音浪参与分组

## 内置约束

- 玖玉 / 玖妹 / 玖玥 三人两两不能同组
- 啸泽固定第 1 组，啸帆固定最后一组
- 啸昭与啸寻至少相隔 4 组
- 第 1 组 08:15 开赛，之后每 15 分钟一组

约束在 Rust 端权威实现（`pk-core/src/constraints.rs`），前端启动时从
`GET /api/group/constraints` 拉同一份副本用于高亮展示，不自己再抄一份
（`packages/shared/src/constraints.ts` 里的常量只作后端连不上时的兜底）。

组数不足以满足时**不会静默失败**，而是在看板上明确报出未满足的约束。

## 分组页面能处理的各种情况

- 跨组/组内拖拽，手机上可用「卡片菜单 → 移动」代替拖拽
- 撤销 / 重做（最多 50 步）
- 锁定某人：自动分组与均衡微调都不动他
- 均衡微调：只接受让极差变小的交换
- 自动修正：一键按当前策略重排并修复约束
- 两组交换：卡片菜单 → 交换 → 选目标
- 移出到「未分组」区，再点名字归回任意组
- 增补人员：组头 `+` 输入名字，只插这一人不重排
- 手动改音浪、绑定未匹配账号
- 删除非空组时，成员自动进「未分组」而不是被拒
- **超员提醒**：组头显示 `9/8`，超员组数在看板上单独提示（上限跟着「每组人数」配置走）
- **组内按音浪排序**：一键把每组内部按音浪从高到低重排（不跨组移动）
- **全局找人**：工具条搜索框，命中的卡片高亮并滚动到视野内，组内和未分组一起命中
- **紧凑模式**：一屏多看几组，人数多的时候用
- **快捷键**：`Cmd/Ctrl+Z` 撤销、`Cmd/Ctrl+Shift+Z` 重做（输入框内不劫持）

## 只读约束（数据库）

数据库只做查询，三层保障：

1. 所有 SQL 集中在 `pk-server/src/dialect.rs`，不接受外部传入的 SQL 字符串
2. `queryReadonly()` 只放行 `SELECT / SHOW`，含写操作关键字（按词边界，`deleted_at` 这类列名不算）直接抛错
3. 分组方案、别名等写操作落到本地 SQLite（`apps/server/data/app.db`）

## 已知事项

- 免费库对 prepared statement 兼容性不好（报 1835），所以日期参数是**校验后内联**进 SQL 的（只允许 `YYYY-MM-DD`）
- 连接池上限 3，音浪/主播结果缓存 10 分钟（`WAVE_CACHE_TTL`）；改数据后点「强制刷新」
- `daily_metric` 首日 `prev_snapshot_date IS NULL`，那天 wave 是**累计音浪**（总音浪的基数）：
  `total` 口径计入，`lastN`（近 N 天增量）口径排除
