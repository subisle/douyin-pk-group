# RK3318 合体部署

这套 Compose 定义「一个盒子、一个主库、两个服务」：

- `mariadb`：本地唯一事实源。
- `douyin-server`（Go 过渡）：采集 / 导入 / 机器人 / 数据管理 API，只拿写账号。
  **Rust `pk-server` B 阶段已实现主播台账与导入三步流后，此服务进入退役倒计时**
  （见 docs/00 §7 阶段 F）。
- `pk-server`：分组与展示 + 数据管理（`pk_reader` 读池 + `douyin_writer` 写池分账号）。

入口复用宿主机现有 Nginx，不额外常驻网关：

```text
/dy/*  → 127.0.0.1:8080  douyin-server（Go 过渡服务，退役倒计时；pk-server 无此路由）
/s/*   → 127.0.0.1:8787  pk-server
/      → 127.0.0.1:8787  pk-server
```

> 端口说明（P1-5）：该 compose 两个应用服务**不发布宿主端口**——容器间走内部网络
> （pk-server 直连 `mariadb:3306`），宿主机 Nginx 反代到容器 IP/宿主地址。若未来给
> 服务加 `ports:`，一律绑 `127.0.0.1:` 前缀；容器内 pk-server 若需要容器外直连探活，
> 必须显式在 `.env` 配 `HOST=0.0.0.0`（Rust 端默认 `127.0.0.1` 的改动由后端侧负责）。

两个服务各出一个离线镜像包（`pk-server-arm64.tar.gz` + `douyin-server-arm64.tar.gz`），
由 `scripts/deploy-rk3318.sh` 一次性下载并 `docker load`，然后一起 `compose up`。

## 首次启动

```bash
cd deploy/rk3318
cp .env.example .env
# 修改四组密码（MARIADB_ROOT_PASSWORD / DOUYIN_WRITER_PASSWORD / PK_READER_PASSWORD / DOUYIN_MIGRATOR_PASSWORD）
docker compose up -d
```

账号与权限（docs/00 §4，三分离）：

- `pk_reader`：只读（SELECT），由 `initdb/01-pk-reader.sh` 创建，密码取 `.env` 的
  `PK_READER_PASSWORD`。
- `douyin_writer`：业务写（SELECT/INSERT/UPDATE/DELETE，**无 DDL**），密码取
  `DOUYIN_WRITER_PASSWORD`。
- `douyin_migrator`：DDL 专用，密码取 `DOUYIN_MIGRATOR_PASSWORD`（initdb 脚本需创建并授权）。

## 数据库迁移（显式，不随启动自动跑）

docs/00 §4 决策（2026-10-04）：DDL 只由 `douyin_migrator` 账号在显式迁移时执行，
`pk-server` 启动**不做任何 DDL**：

```bash
docker compose exec pk-server /usr/local/bin/pk-server migrate
# 输出：已应用: 001_core, 002_import_audit（已应用过的版本自动跳过）
```

紧急情况下可临时在 compose 里给 pk-server 加 `MIGRATE_ON_BOOT: "true"`，迁移完成后必须移除。

## B 阶段管理接口（pk-server）

```text
GET  /api/admin/streamers?q=&team=    主播台账（含别名 / 主号）
POST /api/admin/streamers             建档（name+team 必填，douyinNo 全局唯一）
PUT  /api/admin/streamers/{id}        编辑（masterId=0 清空师傅）
DEL  /api/admin/streamers/{id}        软删
POST /api/admin/import/preview        导入预览（解析+匹配+团别冲突，不落库）
POST /api/admin/import/commit         提交（merge 幂等 / reject 拒绝重复，import_batch 审计）
GET  /api/admin/import/batches        最近导入批次
```

密码建议只用字母数字，避免 DSN / SQL 中出现特殊字符。

## E 阶段机器人（pk-server 内置）

`.env` 追加（缺省不启用对应渠道）：

```bash
# QQ 官方机器人（开放平台）
BOT_QQ_APP_ID=
BOT_QQ_APP_SECRET=
BOT_QQ_ADMINS=          # 管理员会话 openid，逗号分隔
# 微信 iLink（扫码后会话 token 落 MySQL bot_sessions，单库化后不再落 SQLite；也可手工粘贴）
BOT_WECHAT_ADMINS=
# BOT_AUTOSTART 已废弃：配置了凭据的渠道随服务启动自动连接
BOT_RATE_LIMIT=10       # 每会话每分钟消息上限
```

管理接口（需管理员口令）：

```text
GET  /api/admin/bots/status             渠道状态汇总
POST /api/admin/bots/qq/start|stop      QQ 连接启停
POST /api/admin/bots/weixin/start|stop  微信轮询启停（未登录会提示先扫码）
GET  /api/admin/bots/weixin/qrcode      取登录二维码
POST /api/admin/bots/weixin/qrcode/check  轮询扫码状态（成功自动保存 token）
POST /api/admin/bots/weixin/token       手工粘贴 token
```

机器人命令（仅管理员会话生效，确定性路由不调模型）：
帮助 / 状态 / 排行 [N] / 总榜 / 查询 <名字> / 导出 今日|日期|总榜 /
导入 <男团|女团> [日期] + 数据行或 CSV / 分组
