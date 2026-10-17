# 数据库对齐工具（scripts/db-align）

按 [docs/17-数据库对齐与64部署方案.md](../../docs/17-数据库对齐与64部署方案.md) §5/§9 实现的本地可运行版本。
**本目录只负责「数据对齐」；域名切换与线上部署不在此范围。**

## 1. 文件与职责

| 文件 | 职责 |
|---|---|
| `tables.manifest` | **唯一分类来源**：27 张表 → A/B 类 + 冲突键 + 对账键 + 追加表判重键（对应 docs/17 §5.1/§5.2；2026-10-16 新增 A 类 `person_avatar`（`check_cols` 不含 `synced_at`）与 `monthly_wave`（按月导入的整月音浪，026）） |
| `common.sh` | 传输层：源/目标两端执行器、备份、清单解析、安全检查 |
| `engine.sh` | 同步引擎：COPY + `\copy` + `ON CONFLICT` upsert、config 按键同步、identity 序列复位 |
| `local.env.example` | 本地参数模板（复制为 `local.env`，**不入库**） |
| `../dbcheck/dbcheck.sh` + `checksum.sql` | 对账：行数 + 业务列 md5 校验和，两端 diff |
| `../push-a-to-64.sh` | A 类业务数据：本机 → 目标 |
| `../pull-b-from-64.sh` | B 类运行时数据：目标 → 本机 |
| `../deploy/bootstrap-roles-64.sh` + `.sql` | 目标端角色/权限引导（幂等） |
| `../backup/backup-pg-64.sh` | 目标端 PG 备份（含 `pg_restore -l` 可读性校验 + 保留策略） |

## 2. 本地 / 线上配置分离

- **本地参数**只放 `scripts/db-align/local.env`（已 gitignore，绝不入库）：
  `SRC_PG_*`（本机事实源）、`TARGET_MODE=local` + `DST_PG_*`（本机对练库）。
- **线上参数**走环境变量，部署文件里不出现本地值：
  `TARGET_MODE=docker-ssh`、`PK64_HOST=root@<64_HOST>`、`PK64_CONTAINER=pk-postgres`、`DST_PG_USER`。
- **环境变量优先于 `local.env`**（命令行可临时覆盖，用于验证与排障）。

## 3. 本地怎么跑（不碰线上、不碰真实开发库）

本地验证用「两个副本库」当沙盘：`SRC` 扮演本机、`DST` 扮演 64。

```bash
cd /Volumes/2t/it/抖音pk分组
PG=/opt/homebrew/opt/postgresql@17/bin

# ① 建沙盘（从真实开发库各复制一份；真实库只读）
cp scripts/db-align/local.env.example scripts/db-align/local.env
$PG/dropdb -h 127.0.0.1 -p 3318 -U zmy --if-exists douyin_align_src
$PG/dropdb -h 127.0.0.1 -p 3318 -U zmy --if-exists douyin_align_dst
$PG/createdb -h 127.0.0.1 -p 3318 -U zmy douyin_align_src
$PG/createdb -h 127.0.0.1 -p 3318 -U zmy douyin_align_dst
$PG/pg_dump -h 127.0.0.1 -p 3318 -U zmy -d douyin_dev -Fc | \
  $PG/pg_restore -h 127.0.0.1 -p 3318 -U zmy -d douyin_align_src --no-owner
$PG/pg_dump -h 127.0.0.1 -p 3318 -U zmy -d douyin_dev -Fc | \
  $PG/pg_restore -h 127.0.0.1 -p 3318 -U zmy -d douyin_align_dst --no-owner

# ② 目标端角色/权限引导（幂等）
scripts/deploy/bootstrap-roles-64.sh

# ③ 对账基线（应为全 OK）
scripts/dbcheck/dbcheck.sh --class A
scripts/dbcheck/dbcheck.sh --class B

# ④ A 类推送：本机 → 目标（默认先备份目标端、推完自动对账）
CONFIRM=1 scripts/push-a-to-64.sh

# ⑤ B 类回拉：目标 → 本机（默认先备份本机源库、拉完自动对账）
CONFIRM=1 scripts/pull-b-from-64.sh
```

预期输出：每张表一行 `搬运 N 行 · 目标 X → Y 行`，末尾 `对账通过：N 张表全部一致`。

### 常用参数

| 命令 | 说明 |
|---|---|
| `scripts/dbcheck/dbcheck.sh --class A\|B\|C\|all` | 按类对账；`--list` 只列清单不连库 |
| `CONFIRM=1` / `--yes` | 跳过交互确认（自动化） |
| `--no-backup` | 跳过写前备份（**不推荐**） |
| `--no-verify` | 跳过结束时的自动对账 |

## 4. 线上（64）怎么跑

```bash
export TARGET_MODE=docker-ssh PK64_HOST=root@<64_HOST> PK64_CONTAINER=pk-postgres
export DST_PG_DB=douyin_dev DST_PG_USER=douyin_dev
CONFIRM=1 scripts/push-a-to-64.sh        # 本机 → 64
CONFIRM=1 scripts/pull-b-from-64.sh      # 64 → 本机
```

写入前脚本一律先 `pg_dump` 备份目标端（`backups/db-align/pre-push-*.dump`），
备份不可读（`pg_restore -l` 失败）就中止，绝不在没有备份的情况下灌库。

## 5. 实现要点（踩过的坑，改动前先读）

1. **identity 列**：`id` 是 `GENERATED ALWAYS AS IDENTITY`。业务表用 `OVERRIDING SYSTEM VALUE`
   显式搬 id 保主键一致；但 **identity 列不能出现在 `DO UPDATE SET`**（会报
   `column "id" can only be updated to DEFAULT`），所以 SET 列表排除 identity 列。
2. **追加型日志表不搬 id**：`access_logs` 走 `insert_ignore`，由目标端自增 id、靠 `dedup_key` 判重。
   若搬 id 会与目标端既有主键冲突（`access_logs_pkey`）。
3. **NULL 判重键**：`access_logs` 有 237 行历史 `dedup_key IS NULL`，而 NULL 不参与
   `ON CONFLICT` 去重 —— 只靠 conflict 列会在**每次 pull 时重复插入**。
   故 `tables.manifest` 第 7 列给出 `nullkey`，NULL 时退化为自然键判重。
4. **临时表用 CTAS**（`CREATE TEMP TABLE ... AS SELECT ... WITH NO DATA`）而非 `LIKE t`：
   `LIKE` 会带上 id 的 NOT NULL 约束，与第 2 点冲突。
5. **序列复位**：显式写入 id 不推进 identity 序列，push/pull 收尾按 `max(id)` `setval`，
   否则目标端自身写入会撞车。
6. **对账固定时区**：`checksum.sql` 里 `SET TIME ZONE 'UTC'`，避免两端会话时区不同
   （64 容器可能 UTC）导致 timestamptz 文本渲染差异。
7. **macOS bash 3.2**：`$VAR` 后紧跟中文字符会被并入变量名，必须写 `${VAR}`；
   无 `mapfile`，用 `while read`。
8. **环境变量优先**：`align_load_env` 逐行解析且不覆盖已存在的环境变量。

## 6. 待确认项（设计文档未定义，实现时按保守口径处理）

| # | 待确认点 | 当前实现（保守） |
|---|---|---|
| Q1 | `access_logs` 的判重口径 | conflict=`dedup_key`；NULL 时退化为 `kind,ip,path,account_name,created_at`。该自然键在库里有 11 组重复，口径需业务确认 |
| Q2 | `wave_snapshot` / `duration_snapshot` / `identity_orphan_log` 三表无代码写入点 | 暂按 A/A/B 归类，首次真实对账后复核 |
| Q3 | A 类删除语义 | push **不删除**目标端多出的行，仅反连接提示；是否要删除待确认 |
| Q4 | `config` 的 `wechat_add_qr` | 代码可读写但当前库中无此键；已列入本机权威键，出现即同步 |
| Q5 | 推送触发方式 | 手动脚本（docs/17 Q5 建议）；定时对账未实现 |
| Q6 | `monthly_metric` 无业务唯一键 | conflict 用 `id`，对账用 `(person_id, period)` |
