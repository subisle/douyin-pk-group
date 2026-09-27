# PK 挑战赛分组程序

只读接入 `douyinxs` 数据库，按音浪高低做均衡分组，支持手动拖拽调整与导出图片。
**后端已全量迁移到 Rust**（axum + sqlx/MySQL + SQLite），前端保留 React 只做界面与拖拽，
所有分组算法都在 `pk-core`（Rust）里算，浏览器不再跑第二份算法。

## 架构

```
rust/crates/pk-core     算法核心：名单解析 / 内置约束 / 均衡分组（纯函数，零 IO，有单测）
rust/crates/pk-server   HTTP 服务：axum + sqlx(MySQL 只读) + SQLite + 静态托管
apps/web                React + Vite + dnd-kit 拖拽 + html-to-image 导出
packages/shared         前端仍用的展示工具（formatWave / slotTime / 默认名单 / 类型）
```

数据流：前端把 players 发到 `/api/group/arrange`（自动分组）或 `/api/group/inspect`（拖拽后体检），
后端返回分组结果 + 均衡度 + 约束冲突，前端只负责展示。这样算法只有一份实现，不会前后端不一致。

## 管理员后台 vs 对外展示

- `/`            管理员看板：解析、拖拽、锁定、撤销、均衡、导出
- `/s/:planId`   对外只读展示页：只有分组与名单，**不显示音浪/排名，不能编辑**；手机单列、桌面多列自适应

保存方案后点「分享链接」拿到 `/s/:id`，可直接发群里。

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
cp .env.example .env   # 填 MySQL 只读账号
docker compose -f docker-compose.yml up -d
```

服务只监听 `127.0.0.1:8787`，公网入口交给 Caddy 反代。

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

约束在 Rust 端权威实现（`pk-core/src/constraints.rs`）。组数不足以满足时**不会静默失败**，
而是在看板上明确报出未满足的约束。

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
