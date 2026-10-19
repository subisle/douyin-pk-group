# PK 挑战赛分组程序

后端是**全 Rust 单技术栈**（axum + sqlx 0.8 / PostgreSQL）：业务与运行时状态统一落在
PostgreSQL（本机开发库 `douyin_dev`，`127.0.0.1:3318`，唯一事实源，见
[docs/16-数据库资产总览.md](docs/16-数据库资产总览.md)；生产 147 / 内网 rk3318 暂仍为
MySQL/MariaDB，PG 上线切换见 docs/17）。按音浪高低做均衡分组，支持手动拖拽调整与导出图片。前端保留 React 只做
界面与拖拽，所有分组算法都在 `pk-core`（Rust）里算，浏览器不再跑第二份算法。
采集 / 导入 / 机器人等数据管理职责同样以 `pk-server` 模块承载。

## 架构

```
rust/crates/pk-core     算法核心：名单解析 / 内置约束 / 均衡分组（纯函数，零 IO，有单测）
rust/crates/pk-server   HTTP 服务：axum + sqlx(MySQL) + 静态托管
  ├─ auth.rs            管理员口令鉴权（内存 token）
  ├─ public.rs          对外只读展示数据（裁剪字段 + 实时音浪）
  ├─ grouping.rs        /api/group/*：arrange / inspect / polish / constraints
  ├─ matching.rs        名单 -> 主播匹配 + read_id（BIGINT UNSIGNED 兼容）
  ├─ dialect.rs         LOCAL / A 库 SQL 方言 + 日期白名单内联
  ├─ pgrs.rs            只读层：PgPool 连接池 + 只读守卫 + `?`→`$N` 重写（内建）
  ├─ writer.rs          写层：writer 池（DB_WRITER_*）+ 写守卫（拦 DDL）
  ├─ admin.rs           数据管理 API（/api/admin/*）：导入 / 主播台账 / 采集
  ├─ bot/               机器人：QQ / 微信 iLink，确定性命令路由
  └─ import/            CSV / 粘贴 / 补录，字段映射 + 校验报告
apps/douyin-web      现役运营后台（React + Vite，history 路由挂在 /admin 下）—— 唯一前端
  ├─ src/lib/router.ts       路由表 + popstate：/admin/data|import|streamers|claims|lineage|group|bots
  ├─ src/DashboardPage.tsx   数据页：概况 KPI / Top10 / 未播预警 / 团别趋势
  ├─ src/ImportPage.tsx      数据导入：拖拽或选文件 → 弹窗预览 → 确认入库 → 批次列表
  ├─ src/StreamerPage.tsx    主播台账（右键：编辑 / 师傅 / 抖音号 / 我的账号 / 合并 / 删除）
  ├─ src/LineagePage.tsx     族谱画布；src/BotPage.tsx 机器人（QQ/微信）配置
  └─ src/pk/GroupPage.tsx    PK 分组看板（拖拽、约束、方案、导出）
apps/server          pk-server (Rust) 的 pnpm 包装：`pnpm dev:api` 直接跑 cargo
```

> 2026-10 收敛：旧管理端 `apps/web`（hash 路由）、`packages/shared`、`server-go`
> 过渡服务已全部删除。前端只剩 `apps/douyin-web`，后端只剩 Rust `pk-server`。

数据流：前端把 players 发到 `/api/group/arrange`（自动分组）或 `/api/group/inspect`（拖拽后体检），
后端返回分组结果 + 均衡度 + 约束冲突，前端只负责展示。这样算法只有一份实现，不会前后端不一致。

## 管理员后台 vs 对外展示

现役后台是 `apps/douyin-web`，history 路由统一挂在 `/admin` 下（`spa.rs` 剥前缀后
按 dist 解析，深层链接刷新也回退到 index.html）：

| 路径 | 谁能看 | 内容 |
| --- | --- | --- |
| `/admin/data` | 需口令 | 数据页：概况 KPI、Top10、未播预警、团别趋势 |
| `/admin/import` | 需口令 | 导入：拖拽/选文件 → 弹窗预览 → 确认入库 → 批次列表（无粘贴入口，**不分男女团**）；导入类型分**按日**（音浪） / **按月**（时长、音浪可勾选） |
| `/admin/streamers` | 需口令 | 主播台账：检索、新建、别名、师傅、**多抖音号**（导入合并音浪）、「我的」账号 |
| ~~`/admin/claims`~~ | — | 抖音号绑定审计 —— **该功能已于 2026-10-16 下线：导航与接口移除，表保留**（历史申请与截图仍在 `anchor_claims` / `my_bind_requests` 表里） |
| `/admin/lineage` | 需口令 | 族谱画布 |
| `/admin/group` | 需口令 | PK 分组：解析、拖拽、锁定、撤销、均衡、导出、方案管理 |
| `/admin/bots` | 需口令 | 机器人（QQ / 微信）与会话、**推送队列**、公开站点设置 |
| `/` | 任何人 | 对外展示页（与 `/st/m` 同一份页面） |
| `/s/:planId` | 任何人 | 对外只读：只给分组名单和开赛时段，**不展示音浪** |

导航项是真链接，刷新、前进后退、分享链接都停在同一页；认不出的路径 replace 回
`/admin/data`。开发态（`pnpm --filter @pk/douyin-web dev`，5173）不带前缀，base 自动识别。

~~旧管理端 `apps/web` 的 hash 路由~~ 已于 2026-10 删除，前端只有 `apps/douyin-web` 一套。

**鉴权**（`rust/crates/pk-server/src/auth.rs`）

- `/api/*` 需要管理员口令，`Authorization: Bearer <token>`
- 只有 `/api/public/*`、`/api/auth/login`、`/api/health` 免鉴权
- 口令换 token 走 `POST /api/auth/login`，token 只存内存，进程重启即失效，空闲 8 小时自动清理
- 管理员口令只读 `ADMIN_PASSWORD`（可轮换、不用重编二进制）。**环境变量优先**：配置了就用它（短口令只打 WARN 不拦截）；未配置/留空时用内置默认口令 `shuai4444`（`auth::DEFAULT_ADMIN_PASSWORD`，2026-10-19 用户口径）兜底，改后重启生效
- CORS 默认只允许同源（前后端同端口），跨域时用 `CORS_ORIGINS` 显式放行

**展示端「我的」账号**（`public_teams.rs` + `store.rs`）

- 登录：手机号（或姓名）+ 密码。**账号只有一条创建路径：管理员在后台开号** ——
  「机器人 → 我的页面账号」单条（`POST /api/admin/my-accounts`），或「主播台账 → 批量开号」
  （`POST /api/admin/my-accounts/batch`）。**手机号自助注册已于 2026-10-14 彻底移除**
  （不是隐藏开关）：登录门「注册」页签 / OAuth 授权步骤 / 姓名核对全部删除，后端 6 条
  `/api/public/my/register*` 路由、WorkBuddy 网关模块 `wbgateway.rs`（整文件）及其启动接线一并删除，
  `PublicSettings` 不再下发 `myRegister` 字段，管理后台「公开站点设置 → 开放手机号注册」开关已删，
  注册链路的展示端自检脚本（`scripts/check/` 下，415 行）已删。**没有任何客户端能自助开号**
- 后台开号即绑定：账号名 = 台账主播姓名、`bound=1`、团别按台账走，不用再绑一次；
  手机号另存 `phone` 列，所以手机号和姓名都能登录。
  **绑定后用户自己不能解绑/换绑，只有管理员能在后台「我的页面账号 → 改绑」**
- 忘记密码（登录门，**未登录**场景）：`POST /api/public/my/password/reset {phone}` → `{ok, password}`，
  重置为**随机临时密码**（CSPRNG，不再是手机号后四位）并在本次响应里下发一次，旧登录态同时吊销；
  只受每 IP 每分钟 12 次限流（注册开关已删，此端点不再受它约束）。
  ⚠️ 无短信通道：**知道某主播手机号即可重置该账号并拿到新密码**，是产品确认保留的已知残留风险（见 docs/24 §6）
- **重置密码（个人页，已登录；2026-10-15 起不再校验旧密码）**：`POST /api/public/my/password`，
  请求头 `X-My-Token`，请求体 `{"newPass"}`（**不再需要旧密码**；旧版前端仍带 `oldPass` 时一律忽略、不报错）→
  `200 {"ok":true,"token":"<新的 my token>"}`（旧 token 全部吊销，并换发同身份新 token，前端覆盖本地即可）；
  失败 `400`（密码必须是 4~20 位字母或数字 / 该账号不支持自助改密）、
  `401`（登录已过期）。展示端「我的」页入口文案为「重置密码」，对话框只有 新密码 / 确认新密码（两次不一致由前端拦下）
- 登录门密码输入框已去掉 `inputmode="numeric"`（密码是 4~20 位字母或数字，手机上要能打字母）
- **可看团别（`view_team`，2026-10-15，迁移 `024`）**：管理员观看账号可按团分权 ——
  `any` = 全团（男女团都能看，页内可切「全团/男团/女团」）、`male` / `female` = 钉死某团、
  `''`（默认）= 沿用旧口径（绑了主播按主播所在团）。钉死团别优先级高于绑定关系；
  换团条只在 `switchable=true`（`any`）时渲染，钉死团的账号没有入口。
  改范围走 **专用端点 `PUT /api/admin/my-accounts/{name}/view-team`**（只改团别、不动密码 ——
  复用 `POST /api/admin/my-accounts` 会在不传 `pass` 时自动生成新密码，等于顺手重置）；
  后台两处入口：机器人页「我的页面账号」列表管理员行下拉、主播台账右键弹窗。
  改动会吊销该账号全部登录态。当前配置：浩雨=any（总管理员，2026-10-16 开发期由「帅帅」改绑台账主播浩雨，口令见运维手册）、饭团=female、鹏先生=any。
  完整口径见 [docs/09 §5.4](docs/09-机器人口令与权限结构.md)

**对外展示页**（`rust/crates/pk-server/src/public.rs`）

- 方案要先「上架」才对外可见：保存方案后点「分享链接」会自动上架；历史方案里也能上架/下架
- 接口 `GET /api/public/plans/:id` 已裁剪字段：**不返回** personId / key / 数据源 / 别名等内部信息
- 音浪按方案口径（`total` 或 `lastN`）**实时取**，走 WaveCache；数据库挂了自动降级用方案快照并标 `stale`
- 页面两个视图：**分组**（组内音浪排 + 组间占比条）和**音浪排行**（全场名次，同分并列）
- 手机：单列卡片 + 吸顶汇总条；≥640 两列；≥1024 三列；另有 ≥640 才显示的组内音浪条
- **主播列表真头像（2026-10-16，迁移 `025`）**：榜单 item 带 `avatar`（同源 `/api/public/avatars/<personId>?v=<hash>`，无图为 `null`，前端回落姓名首字色块）；图是抖音头像的**本地留存副本**（`person_avatar`，服务内定时器每 7 天刷新，接口失效页面照样有图），完整口径见 [docs/25](docs/25-展示端抖音头像.md)

**后台展示页入口**：`TopBar` 的「历史方案」里有上架状态、复制链接、上架/下架。

## 本地开发

```bash
# 后端（Rust）
cd rust && cargo run -p pk-server          # 默认 8787，读 rust/.env

# 前端（现役管理端 douyin-web）
pnpm install && pnpm -F @pk/douyin-web dev  # 5173，/api 代理到 8787
```

数据库凭据在 `rust/.env`（不要提交到仓库；部署态放在 `deploy/.env`，可从
`deploy/.env.example` 复制）。Rust 端自己解析 `.env`（值里有中文和空格，dotenvy 会报错）。

## 本地自检命令清单

| 命令 | 用途 | 是否写库 |
| --- | --- | --- |
| `cd rust && cargo test --workspace` | pk-core / pk-server 全部单元与集成测试 | 不写（测试自建/自清测试数据） |
| `node scripts/check/uicheck-admin.mjs [BASE] [口令]` | 管理端（apps/douyin-web）路由与渲染自检：深链、刷新、导航、导入预览弹窗 | **默认会写**：第 7 段写 2019-01-01 测试日并当场删除；`UICHECK_READONLY=1` 时跳过该段，只跑路由与渲染断言（部署脚本默认只读） |

部署自检走的是只读形态：`scripts/deploy/host.sh` 内部以
`UICHECK_READONLY=1 PREFIX=/admin` 调 `uicheck-admin.mjs`；全量自检（含入库回环）
请在测试环境手动跑。

## 构建与部署

> **🎯 147 容器化部署计划（2026-10-12 定稿）**：pk-server/PG/Redis 全容器化，
> GitHub Actions 构建双架构镜像推 GHCR，147 一键更新（健康检查失败自动回滚）。
> 详见 **[docs/deploy/147-docker-plan.md](docs/deploy/147-docker-plan.md)**；
> 工件：`docker-release.yml`（CI）、`deploy/docker-compose.prod.yml`、
> `scripts/deploy/update-147.sh`（一键更新）、`backup-pg-147.sh`（每日备份）、
> `restore-pg-147.sh`（恢复）。
>
> **现役生产有两处（2026-10-11）**：
>
> - **147**（`147.90.***.***` / Debian 12 x86_64，systemd 服务 `/opt/pk-server`，
>   对外 `https://pk.subisle.com`）—— 对外主生产；一键脚本 `scripts/deploy/147.sh`，
>   数据上线 `scripts/deploy/upload-db-vps147.sh`（本机 `douyin_dev` 整体 dump → 灌进 147 的 `pk-mariadb`）。
> - **rk3318**（arm64 / Docker，`/opt/pk-server`）—— arm64 生产，一键脚本 `scripts/deploy-rk3318.sh`
>   （GitHub Actions 出 arm64 镜像包 → scp → `docker load` → 重启容器）。
>
> 64 / 115 的历史流程仅作参考。

构建全部走 GitHub Actions（`.github/workflows/ci.yml`），本地不产出部署物：
- `rust` job：`cargo test` / `cargo clippy -D warnings` / `cargo fmt --check`
- `web` job：`tsc --noEmit` + `vite build`（只构建现役 `@pk/douyin-web`）
- `docker` job：构建并推 ghcr 镜像（只有 `pk-server`），默认 amd64；
  `amd64-bundle.yml` 另出「可 `docker load` 的离线包」挂 rolling release

镜像里的前端固定是 `apps/douyin-web`（现役统一后台，history 路由挂 `/admin`）。

一条命令发布到 147：

```bash
PK147_PASS='<root 口令>' scripts/deploy/147.sh
# 等 CI 绿灯 → 触发 amd64 镜像包 → scp → docker load → 从镜像里取二进制 + 前端
# → 备份替换 /opt/pk-server/{pk-server,web} → pk-server migrate → systemctl restart → 只读自检
```

147 的形态（与容器部署不同，别混用）：pk-server 是 systemd 服务
`/etc/systemd/system/pk-server.service`，进程就是 `/opt/pk-server/pk-server`，
环境变量在 `/opt/pk-server/pk.env`（`WEB_DIST=/opt/pk-server/web`，监听 `127.0.0.1:8787`），
数据库是本机容器 `pk-mariadb`，公网入口是 Caddy。旧的二进制/前端留在
`/opt/pk-server/pk-server.bak-<时间戳>` 与 `web.bak-<时间戳>`，回滚就是换回去再重启。
升级前会跑 `pk-server migrate`（DDL 由迁移账号执行，按 `schema_migrations` 幂等；
迁移账号必须是表属主，报「must be owner」的处置见 docs/22-数据库运维说明.md）。

通用 compose（`deploy/docker-compose.yml`）：

```bash
cd deploy
cp .env.example .env
docker compose -f docker-compose.yml up -d
```

服务端口只绑 `127.0.0.1`（`127.0.0.1:${PK_PORT:-6666}:8787`），公网入口交给 Caddy 反代，
**不要把端口直接发布到 0.0.0.0**。容器内 pk-server 的 `HOST` 默认 `127.0.0.1`（Rust 端
改动由后端侧负责）；若部署形态需要容器外直连探活，必须在 `.env` **显式配 `HOST=0.0.0.0`**
——宿主侧端口仍只绑 127.0.0.1，公网进不来。
`deploy/Caddyfile.example` 是可直接用的配置：`/s/*` 放开展示页并给短缓存，
其余路径 `no-store` 防止后台看到旧分组，另加了限流。

```bash
cp deploy/Caddyfile.example /etc/caddy/Caddyfile.d/pk-group.caddy
caddy reload
```

拿到的对外展示 URL 就是 `https://你的域名/s/<方案号>`。

## 名单规则

- 每行第一个名字 = 参赛主播；后面的名字 = 队长/师傅（只标注，不参赛）
  - `啸安➕狼兴` → 参赛者 啸安，师傅 狼兴
  - `啸阳（狼泽）` → 参赛者 啸阳，师傅 狼泽
- 别名映射：默认 `阿楠 → 南方楠`，可在「主播台账」里增改（`aliases` 表，MySQL）
- 排除名单：默认 `目`
- 匹配不上的主播会标红，可下拉手动绑定，未绑定按 0 音浪参与分组

## 内置约束

- 玖玉 / 玖妹 / 玖玥 三人两两不能同组
- 狼影和浩冬不能同组
- 啸泽固定第 1 组，啸帆固定最后一组
- 啸阳固定第 1 组，浩阳从第 6 组开始
- 啸昭与啸寻至少相隔 4 组
- 第 1 组 08:15 开赛，之后每 15 分钟一组

约束在 Rust 端权威实现（`pk-core/src/constraints.rs`），前端启动时从
`GET /api/group/constraints` 拉同一份副本用于高亮展示，不自己再抄一份
（后端连不上时用 `apps/douyin-web/src/pk/rules.ts` 里的常量兜底）。

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

1. 所有 SQL 集中在 `pk-server/src/dialect.rs` 等模块内，不接受外部传入的 SQL 字符串
2. `pgrs::query` / `pgrs::query_with_binds` 的只读守卫只放行 `SELECT`，含写操作关键字
   （按词边界，`deleted_at` 这类列名不算）直接抛错
3. 分组方案、别名等写操作走 writer 池（`DB_WRITER_*`，账号 `pk_writer`）落到 PG 业务库
   （与业务数据同库，见 docs/00 单库决策）

**数据库访问层约定（2026-10-12 收敛，详见 docs/04 §数据库访问层统一约定）**：
SQL 一律写 MySQL 风格 `?` 占位符，读走 `pgrs` 入口（内建 `pq::rewrite` 重写为 `$N`），
写走 writer 池并手动包 `&crate::pq::rewrite("…")`。**新增 `sqlx::query` 调用必须过
`pq::rewrite` 或改走 `pgrs` 入口**，禁止裸传 `?` 字面量直连。

## 数据源与部署形态（历史问题存档）

「sqlpub douyinxs 远程源库 + 64 连不上 MySQL」是单库决策（2026-10-09，docs/00 §4）之前的
问题：现在业务与运行时状态统一在 MySQL（本机 `douyin_dev` / 生产 MariaDB `douyin`），
不再从 sqlpub 同步，生产不配 `DB_A_*` 远程源，`sync.rs` 与「同步主播」命令均已删除。
上线 = `scripts/deploy/upload-db-vps147.sh` 把本机库整体 dump → scp → 恢复进生产 MariaDB
（灌库前会先在目标机备份，见脚本头注释）。

### 把 147 生产数据拉回本机（2026-10-12，PG 迁移）

`scripts/pull-db-147.sh` 把 147（`https://pk.subisle.com`，MariaDB 容器 `pk-mariadb`）
的生产库 `douyin` 拉到本机，恢复进本机 PG `douyin_dev`（pg-migration 分支目标库）：

```bash
PK147_PASS='<147 root 口令>' CONFIRM_DROP=1 scripts/pull-db-147.sh
# 只拉 dump 不灌库（先看一眼再决定）：
PK147_PASS='<147 root 口令>' scripts/pull-db-147.sh --dump-only
```

链路：容器内 `mariadb-dump`（root 口令从 147 的 `/opt/pk-server/.env` 读，不落本地文件）
→ scp 到 `.dbmerge/` → `scripts/mysql2pg.py` 按目标库 `information_schema` 逐列对齐转换
→ 灌库前先 `pg_dump` 备份 → `DROP SCHEMA` + `001_core.sql` 重建 → 灌数据 → `setval` 复位
identity 序列 → 逐表行数对账。

转换器要处理的三处不对称，都已经在代码里解决：

1. **时区**：147 的 MariaDB 跑在 UTC，`mariadb-dump` 吐的是 UTC 墙上时间的裸串，目标列是
   `timestamptz`（会话时区 Asia/Shanghai）。直接用会被当成北京时间解释、整体差 8 小时，
   所以时间值统一写成 `'...'::timestamp AT TIME ZONE 'UTC'`。
2. **二进制 IP**：`varbinary(16)` 在 dump 里有引号包裹的转义序列（`'\x7f\0\0\x01'`）和
   裸字节两种形态，还有 IPv4 只存 4 字节、IPv6 16 字节混排；按 MariaDB 列类型逐列解，
   不做「先按逗号切再解」——裸字节里的引号/逗号/括号会让无差别切分错位。
3. **列集合不一致**：147 缺 `person.phone`（016 迁移没在那台跑过）、`my_accounts` 有生成列
   `phone_uniq`。以目标库列清单为准，多出的列丢掉、缺的列整个不进 INSERT 列清单让 PG
   用默认值，绝不补 NULL（会撞 NOT NULL）。

数据产物（`*.sql` / `*.dump` / `*.sql.gz`）已在 `.gitignore` 里，只入脚本本身。

本机 / 147 / rk3318 三处的**全部数据库实例、库名、表数、账号与待收尾项**汇总见
[docs/16-数据库资产总览.md](docs/16-数据库资产总览.md)（2026-10-12 实查快照）。

### 远程备份库（2026-10-11）

生产库仍是 MySQL/MariaDB 唯一事实源；另有远程 MySQL/MariaDB 备份库。147 部署会安装
`/opt/pk-server/backup-to-remote.sh`，每次导入成功后异步把生产库 dump 并导入远程备份库。
脚本带 `flock` 并发去重和数据指纹去重；备份失败只记录日志，不影响导入结果。

在 `/opt/pk-server/backup.env` 配置远程库（权限 `0600`）：

```bash
DB_BACKUP_HOST=backup.example.com
DB_BACKUP_PORT=3306
DB_BACKUP_USER=douyin_backup
DB_BACKUP_PASSWORD='...'
DB_BACKUP_NAME=douyin_backup
DB_BACKUP_CONTAINER=pk-mariadb
DB_BACKUP_CREATE=0
```

先在远程库执行 `CREATE DATABASE douyin_backup CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;`，
并给账号目标库的 `SELECT/INSERT/UPDATE/DELETE/CREATE/DROP/ALTER/INDEX/LOCK TABLES` 权限。
手动备份执行 `/opt/pk-server/backup-to-remote.sh`。

## 导入格式：音浪按日 / 时长与音浪按月

用户口径（2026-10-19 更新）：**按日文件带「开播有效时长」列时，按日也写时长**（`daily_metric.minutes`）；
文件没有时长列则保留库里旧值、不清零。2026-10-16 起**音浪也支持按月导入**。
所以「数据导入」页有两种导入类型：

| 类型 | 文件 | 入库 | 说明 |
| --- | --- | --- | --- |
| 按日 | 每天的音浪 / 概览导出（`主播名,当日音浪,累计`，文件名带日期更好） | `daily_metric`（音浪 + 时长） | 列序不严格；**文件里有「开播有效时长 / 时长」列就按日写 `daily_metric.minutes`**（预览出「当日时长」列、结果显示时长合计），没有该列的老文件不改动库里已有当日时长 |
| 按月 | 平台月报：月时长（`9月份时长.csv` 的「开播有效时长」）、月概览列表的「总音浪」，一人一行 | `monthly_duration` 时长 / `monthly_wave` 音浪（各自一人一月一行） | 时长与音浪**可勾选（可同时导）**，弹窗里勾哪个写哪个；文件**只有时长列、没有音浪列**时会自动切到「按月」，选年份 + 月份后入库 |

- **按月两种指标**：一次提交可同时写时长（`monthly_duration`）与音浪（`monthly_wave`）；
  请求体 `metrics: ["minutes","wave"]` 的任意子集，不带 = 两种都导，**空数组报 400**（不能静默当成都导）。
- **自动识别**：文件里**只有时长列、没有音浪列**才判成按月文件；时长 + 总音浪同表的概览列表
  （如 `10.14….csv`）默认仍按日，运营在弹窗里手动切「按月」。
  文件名带月份（`9月份时长.csv` → 当年 9 月）会直接填好年份 / 月份，弹窗里可改。
- **不读音浪列**：月度文件里有「分成比-直播音浪收益」这种含「音浪」字样的比例列，
  解析时按列名排除，绝不会把 35 当成音浪（`csvparse::find_wave_column`）。
- **幂等**：同一个月份的整月值「相同不覆盖、不同才覆盖」，时长与音浪**各自判定**；整月值重导不会翻倍。
- **展示口径**：月榜 / 年榜 / 个人月卡的「时长」与「音浪」都**优先用整月导入值**，某月没有整月记录
  （历史月份）才回落到 `daily_metric` 求和；只导了整月音浪、当月没有日明细的人也会进月 / 年榜。
  日榜、每日明细与**累计（历史累计总音浪）**口径完全不变。
- **时长显示**：展示端时长统一显示 `x时x分`（`108` → `1时48分`、整点 `120` → `2时`、
  不足 1 小时 `45` → `45分`），去掉秒，日 / 月 / 年三个口径共用。
- **不进暂存区**：月度值未匹配到主播时按失败报出（`import_pending` 是按「某一天」补账的，
  整月值摊到一天会失真）—— 先在台账建档 / 绑号，再重导该月文件。
- **机器人也能导**：私聊把 `9月份时长.csv` 发给 QQ / 微信机器人，**只有时长列的附件**自动按文件名里的月份
  导入时长（文件名没有月份会回一句提示，让你改名或去管理端选月份）；带音浪的月报请在管理端导。
- **删除**：导入页切到「按月」后，删除按钮一次删掉该月已导入的整月时长**与整月音浪**。
- 库要先跑一次 `pk-server migrate`（`007_monthly_duration` / `026_monthly_wave`）；没跑也不会让页面打不开 ——
  月 / 年对应指标会自动回落到日明细口径（两张整月表互不牵连）。

## 一人多号：抖音号与音浪合并

一个主播挂多个抖音号时，运营按抖音号导出的文件里会有这个人的多行。导入时**同一
person 的行先合并再入库**（`admin::merge_rows_by_person`），合并口径：

| 字段 | 口径 | 为什么 |
| --- | --- | --- |
| 当日音浪 `wave` | **求和** | 每个号当天各自播的，加起来才是这个人当天的量 |
| 累计 `cumulative_wave` | **取最大** | 各号累计快照不是同一个池子，求和会虚高；取最大不重复计数 |
| 时长 `minutes` | 求和 | 月度时长 / 月度音浪同样按人求和（一人多号各自播的加起来） |
| `is_live` | 任一为 true | 有一个号在播就是开播了 |

不合并的话，`daily_metric` 按 (person, date) 唯一，后一行会覆盖前一行 —— 主播看到的
音浪只是他最后一个号的。

**认人（2026-10-10）**：文件里给了「抖音号」和「主播ID」两列时**两列都会试**（抖音号 →
采集 ID → 名字归一化）。以前只看抖音号，台账里的账号只有采集 ID 时整批人匹配不上。
抖音后台导出的 CSV 首行带 UTF-8 BOM，「主播ID」列现在也能正确识别。

管理端「主播台账 → 账号」里可以加号、设主号、解绑（只填抖音号即可，采集 ID 选填）。
**抖音号全局唯一**，已被别人占用的号绑不上。

## 微信推送（展示端「我的」页）

- **入口（2026-10-15）**：「我的」页列表项由「推送机器人服务」改名为**「消息设置」**
  （副标题：微信机器人对接与推送开关），子页 `/my/wechat`；**保留 `/my/push` 作为别名**，
  旧链接打开同一页（不 404、不落回列表）。页内两段：① 微信对接（二维码 / 解绑 / 绑定状态）
  ② 推送开关（追分提醒 / 每日报告 / 师门日报，师门日报仅名下有直系徒弟时显示）。
  **每日报告默认开启**；**个人报告与每日报告分开**，仍在微信里发送「个人报告」单独查看。
- **绑定（2026-10-19 口径）**：进入「消息设置」页**自动获取官方扫码二维码**，不再先点按钮；
  用户截屏后用微信「扫一扫 → 相册」识别，手机上确认即完成对接（2 秒轮询状态）。
  4 位对接码仍在同一次 `start` 响应里生成，但收进「扫码遇到问题」折叠兜底，不抢主流程。
  未完成对接且二维码仍有效的账号再次进入页面时，会就地恢复轮询，不会重复请求上游。
- **追分**：一律比**本月累计**；口径「**分数是分数，追人是追人**」—— 追人 ≤5、目标分数 ≤5
  **各自独立计数**，**追名次**保留可列出 / 可删除（前端不再提供新增入口）；追人支持多选批量添加
  （`POST /api/public/teams/{team}/my/push/targets/batch`）。
- **每日报告**：推送月榜长图（固定 2 页、收件人水印、本人行高亮），**同一业务日只推一次**。
  2026-10-19 起月榜末列改为「距上一名」，去掉与「本月累计」重复的列；音浪值移到进度条右侧，
  提高浅色轨道上的可读性。图片必须整组两页都转换成功才发送/缓存；任一组页失败则退回文字，
  已部分送达时不再整组重发，避免用户收到重复页。
- **触发**：数据入库后**延迟 30 分钟**（`PUSH_DELAY_MINUTES`）统一推，等一轮导入的
  几个文件都到齐，避免推半成品榜单。后台「机器人」页有推送队列表格和「立即推送」按钮。
- **分批（1.0.1，150 人规模）**：每 `WX_PUSH_BATCH_SIZE`（默认 20）人停
  `WX_PUSH_BATCH_INTERVAL_SECS`（默认 3s），防 iLink 频控；单人失败跳过不拖累整批。
- 两个开关都关着的人**不推**。

**「我的」页提成与同期对照（1.0.1；提成已下线 2026-10-11，d30a13a）**

- ~~有直系徒弟的主播个人页显示**徒弟提成**卡（今日/本月/本年）：直系徒弟音浪 ÷ 比例
  折成人民币，默认 10 音浪 = 1 元；**徒孙不计**、排除名单里的师傅（如鹏先生）不显示。
  比例与排除名单在后台「机器人 → 徒弟提成设置」改，即时生效、不追溯。~~
  **已下线**：提成卡与「徒弟提成设置」随展示端 Stitch 改版（2026-10-11，d30a13a）重写移除，
  产品确认不再恢复（2026-10-11 审计确认）。
- 日/月/年数据卡带**同期对照**：较上一业务日 / 上月 / 去年同期的涨跌（涨红跌绿）。
- 微信 iLink 网关已对齐 [Tencent/openclaw-weixin](https://github.com/Tencent/openclaw-weixin)
  协议（扫码状态机、验证码、主机切换、-14 暂停重试、媒体识别）。

**机器人命令（2026-10-10 分层）**

| 角色 | 可用命令 |
| --- | --- |
| 普通用户（微信私聊已完成推送绑定） | `每日报告`（daily）/ `个人报告`（me、my）/ `暂停接收每日推送`（暂停推送、pause）/ `开启接收每日推送`（开启推送、resume）/ `帮助`（help） |
| 管理员（QQ 全部会话 + 微信白名单 / 会话提权） | 以上 5 条 + 状态 / 分组 / 总榜 / 排行 [N] / 查询 <主播名> / 导出 今日\|YYYY-MM-DD\|总榜 / 日报 [男团\|女团] [日期] / 缺数据 / 导入（含黏住日期、CSV 附件） |

未绑定的微信私聊完全静默；普通用户发管理员命令会被拒绝并提示发「帮助」。完整口径（判定顺序、
绑定流程、推送开关与去重）见 [docs/09-机器人口令与权限结构.md](docs/09-机器人口令与权限结构.md)。

## 访客地域门禁 + 访问日志（2026-10-10）

- **规则**：展示端只对**河南 / 安徽**（省级）+ **江苏徐州**（城市级白名单，萧县 IP 归属地常落徐州）
  访客开放，判定全在后端做（**不用 Cloudflare 规则**）；本地先开发测试，改完再部署生产 147（`pk.subisle.com`）。
- **例外通道**：不在上述地区的固定 IP 可单独放行 —— 环境变量 `IP_GATE_ALLOWLIST=1.2.3.4,10.0.0.0/24`
  （精确 IP 或 CIDR），或写进 `ip_allowlist` 表（迁移 `014_ip_allowlist`，后台「访客日志」页可增删）；
  两处命中即放行，访问日志照记。
- **数据源与判定**：来源是 ihmily/ip-info-api（<https://github.com/ihmily/ip-info-api>）收集的
  免费接口，共 19 个可选；**默认各只打 3 个** —— 门禁用 pconline / baidu / ip9（`IP_GATE_SOURCES`
  可配），后台 IP 查询用 ip.sb / ip2location / ipwhois（`IP_QUERY_SOURCES` 可配；每个列表除默认 3 个
  外还有 16 个仍可选）。逗号分隔、未知名忽略、留空回默认；选中的源**并行查询、投票合并**：
  任一来源识别出河南 / 安徽 / 江苏徐州即放行；都没识别出来或全部失败一律拒绝（**fail-closed**）。
- **缓存与表**：归属结果落 MySQL `ip_geo_cache`，**缓存 7 天**；表结构与日志表同属迁移
  `011_ip_geo`（147 发布脚本会自动跑 `pk-server migrate`）。loopback / 私网 IP
  不发外部查询，记为「内网」且**默认拒绝**（本机调试用 `IP_GATE_DISABLED=1`）。
- **拦截位置**：中间件 `ip_gate_mw` 挂在 `/api/public/*`、`ip_gate_page_mw` 挂在 HTML 页面 —— 外省
  访客页面照常打开，第一个数据接口返回 403 `{"error":"当前地区暂不支持访问"}`，前端整页显示
  该提示（**不是掉登录**，不清 token、不弹登录门）。
- **访问日志**（表 `access_logs`）：`kind=api` 同 IP + 同路径 **5 分钟对齐桶去重**（非滑动窗口），
  `kind=login` 记每次登录尝试（带账号与成败）；登录了带账号（token 解析），没登录只记 IP；
  **外省被拒的访问也入库**。
- **后台**：「访客日志」页 `/admin/visits`（导航同名），接口 `GET /api/admin/visits`
  （`limit` / `q` / `kind` / `blocked`，返回 `items` + 今日访问 / 独立 IP / 放行 / 拦截 / 登录）；
  同页新增「IP 查询」面板：`GET /api/admin/ip-lookup?ip=…` 手动查归属地（不读不写缓存）。
- **本地开发**：环境变量 `IP_GATE_DISABLED=1` 关闭地区拦截（**访问日志照记**）；生产留空。
- **本地自检**：本机不在豫 / 皖时，跑 `scripts/check/uicheck-login.mjs` / `uicheck-public.mjs` 等直连
  `/api/public/*` 的自检要用 `IP_GATE_DISABLED=1` 的服务，否则接口 403 会让自检误判。
  （原注册链路自检脚本（`scripts/check/` 下 415 行）已随注册功能于 2026-10-14 删除，勿再按旧文档调用。）

## 已知事项

- **数据库访问（2026-10-12 起）**：新增 `sqlx::query` 调用必须过 `pq::rewrite`（或改走
  `pgrs::query` / `pgrs::query_with_binds` 入口，重写内建）；写路径一律走 writer 池
  （`DB_WRITER_*`），禁止拿只读池执行写。同一条 SQL 里 `?` 与 `$N` 不混用。
  约定全文见 docs/04，改造档案见 docs/18。
- 免费库对 prepared statement 兼容性不好（报 1835），所以日期参数是**校验后内联**进 SQL 的（只允许 `YYYY-MM-DD`）
- 连接池上限 3，音浪/主播结果缓存 10 分钟（`WAVE_CACHE_TTL`）；改数据后点「强制刷新」
- `daily_metric` 首日 `prev_snapshot_date IS NULL`，那天 wave 是**累计音浪**（总音浪的基数）：
  `total` 口径计入，`lastN`（近 N 天增量）口径排除
