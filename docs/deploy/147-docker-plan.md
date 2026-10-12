# 147 生产部署计划：全面容器化 + GitHub 构建 + 一键更新

> 2026-10-12 定稿。目标机：147.90.235.187（pk.subisle.com，Debian 12 x86_64，2G 内存）。

## 0. 现状与目标

| 组件 | 现状 | 目标 |
|---|---|---|
| pk-server | systemd 裸进程（musl 二进制 /opt/pk-server/pk-server） | **Docker 容器**（GitHub Actions 构建，GHCR 托管） |
| 数据库 | MariaDB(pk-mariadb 容器，3306) + PG(pk-postgres 容器，5432，已起未迁数据) | **PostgreSQL 为唯一业务库**（douyin），MariaDB 保留只读供数据比对，迁移完成后下线 |
| Redis | 未部署 | **redis:7-alpine 容器**（缓存层，REDIS_URL 接入；代码已原生支持，不配则走进程内缓存） |
| 前端 | 镜像内 /app/web（Dockerfile 多阶段构建） | 同左（镜像自带） |
| 更新 | 手动 scp + systemctl restart | **147 上一键脚本**：ghcr 拉镜像 → 迁移 → 滚动重启 → 健康检查 → 失败回滚 |

## 1. GitHub 构建（GHCR 双架构）

- workflow：`.github/workflows/docker-release.yml`（见本目录新增）
- 触发：push tag `v*` 或手动 dispatch
- 产出：`ghcr.io/<owner>/pk-server:{tag,latest}`（linux/amd64 + linux/arm64，buildx 多平台，零 QEMU——Dockerfile 已做交叉编译）
- 登录：147 上 `docker login ghcr.io` 用 PAT（read:packages）。**注意 147 出海正常，直连 ghcr 可拉**。

## 2. 147 容器拓扑（/opt/pk-server/docker-compose.prod.yml，见新增）

```
caddy(已有, 80/443) → pk-server:8787
pk-server(ghcr 镜像) → pk-postgres:5432(douyin 库)
                     → pk-redis:6379(缓存, 可选)
                     → 数据源 A/B(远程 sqlpub，读音浪)
pk-mariadb(过渡期保留, 只读比对)
```

- 持久化（全部 bind mount 到 /opt/pk-server，宿主机可备份）：
  - `pg-data` → /opt/pk-server/pg-data
  - `redis-data` → /opt/pk-server/redis-data（appendonly yes）
  - `pk-data` → /opt/pk-server/data（应用状态/上传临时）
  - `backups` → /opt/pk-server/backups（每日 pg_dump，cron 保留 14 天）
- 资源限制：pk-server 512M / PG 512M / redis 128M（2G 内存机器）
- 网络：全部 127.0.0.1 绑定，仅 caddy 暴露公网

## 3. 数据迁移（MariaDB → PG，一次性）

1. 本机已有现成镜像链路：`scripts/pull-db-147.sh`（SFTP）+ `scripts/mysql2pg.py`
2. 147 本机直接迁（免传输）：
   `docker exec pk-mariadb mariadb-dump ... | docker exec -i pk-postgres psql`（经 scripts/mysql2pg.py 转方言）
   —— 或先在本机转换好 SQL，scp 上去灌。
3. 迁移后跑 `pk-server migrate`（PG 方言 MIGRATIONS 已就绪）+ 行数对账（脚本 5/5 步已有）。
4. 切换日：停 systemd pk-server → 起 compose pk-server（env 指向 PG）→ 观察 journal/日志。
5. 回滚：systemctl start pk-server（旧进程还在，env 指回 MariaDB）。

## 4. 一键更新（147 上 /opt/pk-server/update.sh，见新增）

流程：登录 ghcr → pull 新镜像（记录当前 tag）→ docker compose up -d → 跑 migrations 容器内命令 → health 检查（/api/health 3 次 5s）→ 失败自动回滚上一 tag 并告警。支持 `update.sh v1.2.3` 指定版本，缺省 latest。

## 5. 备份（持久化保障）

- PG：cron 每日 03:30 `pg_dump -Fc → /opt/pk-server/backups/douyin-YYYYMMDD.dump.gz`，保留 14 天
- 现有 backup-to-remote.sh（异地 userxs 库）继续作为第二备份，改为从 PG 读
- 恢复演练：restore 脚本见 scripts/deploy/restore-pg-147.sh（新增）

## 6. 上线清单（按序）

- [ ] ghcr PAT（read:packages）写入 147 /root/.docker/config.json
- [ ] push tag v1.0.2 → Actions 构建双架构镜像
- [ ] 147 数据迁移 MariaDB→PG + 对账
- [ ] compose up（pk-server + redis 并入，PG 已有）
- [ ] Caddy 不动（仍指 127.0.0.1:8787）
- [ ] 验证 7 项（health/榜单/推送/cron 备份/回滚演练）
- [ ] MariaDB 下线（保留 dump 归档 30 天）
