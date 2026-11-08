# 31 · 直播监控 wxbot 权限、分组推送与赛程提醒设计

> 状态：本地设计稿，**先本地开发，不部署远端**  
> 日期：2026-11-08  
> 适用：初级 / 中级争霸赛晋级赛、决赛，以及后续同构多人 PK 赛事  
> 关联：`docs/09-机器人口令与权限结构.md`、`docs/23-师门日报与管理员消息下发设计.md`、`live.rs`、`live_api.rs`、`LiveMonitorPage.tsx`

## 1. 本轮要解决的问题

1. wxbot 管理员不再只有“是 / 否”，要按团别分权：
   - 帅帅：全团管理员
   - 鹏先生：全团管理员
   - 饭团：女团管理员
2. 直播实时分推送从“按一轮 PK / 按个人”升级为“按组推送”。
3. PK 结束后进入“连麦说会话”阶段；会话结束时，wxbot 提醒下一组准备。
4. 如果一组名单已齐且 PK 已开始，系统直接开始记录，不再等管理员手动点开始。
5. 当前阶段只做本地开发和验证，不部署、不同步远端数据库。

## 2. 当前实现与缺口

### 2.1 已经具备

- `live_event -> live_stage -> live_group -> live_round -> live_round_seat` 的赛事运行态。
- 采集器通过 Playwright 监听直播间 `linkmic/list`，上报：
  - `battleId`
  - `startTimeMs` / `durationSecs`
  - `battleStatus`
  - 每个参与者的 UID、昵称、分数
  - 房间在线状态、采集健康度
- 后端已经支持：
  - 自动识别 battleId、落采样、并列排名
  - 提前结束、掉线、重复对局、待确认、作废
  - 小组定稿、阶段冻结、晋级名单
  - 从绑定的晚上方案直接生成晋级赛小组
  - 晋级赛全部定稿后生成决赛
  - 实时分和组内个人成绩差距展示
- 管理端已有：
  - 赛事级推送开关与间隔
  - 直播房间、采集器心跳、实时分、赛程、晋级名单
  - 生成晋级赛、生成决赛、冻结晋级、强制定稿等人工兜底

### 2.2 目前缺口

- wxbot 管理员只有 `bot_sessions.is_admin` 布尔值，没有团别范围。
- 直播推送目标主要是“当前 round 的参赛者 + `live_pk_report=1`”，不是按组生成统一推送任务。
- 没有“PK 已结束但还在连麦说话”的状态。
- 没有“连麦说会话结束 → 通知下一组准备”的事件和去重任务。
- 名单匹配基本靠 UID 集合，未完整区分：
  - 全名单匹配
  - 部分到齐
  - 完全无法匹配
- 没有“全名单到齐后立即开始正式记录”的明确状态标记。
- 推送任务没有 `group_id` 维度，无法天然按组去重。
- 没有 wxbot 管理员团别范围与赛事团别的交集过滤。

## 3. 设计原则

1. **分数与权限分离**：直播采集、比分计算、消息发送彼此解耦；采集器只上报观测。
2. **按组为最小业务单元**：一个小组 = 一次 PK 生命周期；所有分数推送、结束提醒、下一组提醒都以 `group_id` 为键。
3. **自动记录优先，人工兜底**：名单齐全且 PK 已开始就自动进入记录；异常时保留人工选正式局、作废、强制定稿。
4. **wxbot 权限 fail-closed**：没有明确 scope 的管理员不接收管理级分组推送，也不能执行跨团命令。
5. **发送闸门不可绕过**：所有 wxbot 消息仍必须经过 `PUSH_OUT_ENABLED`、赛事级开关、收件人开关和现有 `send` 窄口。
6. **幂等与可恢复**：重复观测、采集器重启、网络抖动不能造成重复提醒或重复计分。
7. **本地先验**：先通过本地 mock / dry-run 验证完整状态机，再考虑远端灰度。

## 4. wxbot 管理员团别设计

### 4.1 目标权限

| wxbot 管理员 | 团别范围 | 可查看/接收 |
|---|---|---|
| 帅帅 | `all` | 男团 + 女团 + 全团视角 |
| 鹏先生 | `all` | 男团 + 女团 + 全团视角 |
| 饭团 | `female` | 只允许女团 |

> 说明：当前库里“帅帅 / 浩雨 / 鹏先生台账 / 饭团”存在账号改名和 person 绑定差异，因此 wxbot 权限不能仅靠 `my_accounts.name` 猜，需要显式映射。

### 4.2 数据模型

新增表 `wxbot_admin`：

```sql
CREATE TABLE wxbot_admin (
  id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  display_name VARCHAR(64) NOT NULL UNIQUE, -- 帅帅 / 鹏先生 / 饭团
  account_name VARCHAR(64) NOT NULL DEFAULT '', -- my_accounts.name 或业务账号名
  person_id BIGINT NOT NULL DEFAULT 0,
  wx_account_name VARCHAR(64) NOT NULL DEFAULT '', -- wx_push_accounts.name
  scope VARCHAR(8) NOT NULL DEFAULT '',
  enabled SMALLINT NOT NULL DEFAULT 1,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  CHECK (scope IN ('', 'male', 'female', 'all'))
);
```

初始映射（2026-11-08 本地口径）：

```text
帅帅   -> account_name=shuai4444 -> scope=all
鹏先生 -> account_name=666       -> scope=all
饭团   -> account_name=饭团      -> scope=female
```

密码只在管理端录入，不写入本文档；展示端账号是否绑定主播与 wxbot scope 分开维护。

未绑定 `wx_account_name` 时，管理员状态显示“未绑定 wxbot”，不参与真实发送。

### 4.3 权限解析顺序

1. 微信入站会话 / 主动推送账号 → `wx_push_accounts.name`
2. `wxbot_admin.wx_account_name` 精确匹配
3. `enabled=1` 且 scope 覆盖当前赛事 `team`
4. 否则：
   - 不接收管理级分组/赛程提醒
   - 跨团命令返回“无权限”
   - 不打印敏感数据

### 4.4 与现有 `my_accounts.view_team` 的关系

- `my_accounts.view_team` 继续负责展示端页面观看权限。
- `wxbot_admin.scope` 负责 bot 命令和消息路由。
- 两套权限可以映射到同一业务人员，但**不共用同一个字段**，避免一处修改意外越权。
- 管理端保存 `wxbot_admin` 时，可提示“是否同步展示端 view_team”，默认不自动改。

## 5. 赛事团别

`live_event` 增加：

```sql
ALTER TABLE live_event ADD COLUMN team VARCHAR(8) NOT NULL DEFAULT 'male';
```

允许值：

```text
male | female | all
```

规则：

- 初级 / 中级晋级赛、决赛默认 `male`，后续女团赛事显式传 `female`。
- 分组推送和下一组提醒按 `event.team` 过滤管理员：
  - `male` 赛事 → scope=`all`
  - `female` 赛事 → scope=`all` 或 `female`
  - `all` 赛事 → scope=`all`
- 饭团不接收男团赛事通知。

## 6. 按组推送

### 6.1 推送粒度

以 `group_id` 为单位，不再以单个 `round_id + person` 为主入口。

推荐消息类型：

| kind | 触发 | 内容 |
|---|---|---|
| `group_started` | 全员到齐 + PK 开始 | 本组名单、开赛提示 |
| `group_score` | PK 进行中，按间隔 | 本组实时排名、分数（不展示距上一位） |
| `group_finished` | PK 结束 / 进入连麦会话 | 本组最终排名、晋级线 |
| `next_group_ready` | 连麦会话结束 | 下一组准备提醒 |
| `final_result` | 决赛结束 | 前三名、最终成绩 |

### 6.2 收件人

建议分两类：

1. 组内成员
   - 仅接收本组消息
   - 仍受 `push_bindings.live_pk_report` 控制
2. wxbot 管理员
   - 按 scope 接收：
     - `all`：男团、女团都接收
     - `female`：只接收女团
     - `male`：只接收男团
   - 管理员订阅开关可后续增加 `live_admin_report=1`

第一版建议：

- 组内成员：接收本组实时分与结束结果。
- 全团管理员：接收所有组。
- 饭团：仅女团赛事。
- 若业务只想让管理员看、不想让选手互看，可增加赛事级 `live_group_push_audience = members|admins|both`，默认 `members+admins`。

### 6.3 实时分消息模板

```text
【晋级第 3 组 · 实时分】
08:45 开赛 · 6 人
晋级：第 1 名

1. 狼哲  128,500
2. 玖玉  116,300
3. 浩鸣   98,700
...

更新时间：19:03:25
```

规则：

- 按组内分数从高到低。
- 同分同名次。
- 只显示组内名次和分数，不显示“距上一位”，也不显示全场跨组排名。
- 分数变化小于阈值时可跳过推送，避免消息刷屏。

### 6.4 去重与频率

新增 `live_push_job.group_id` 字段。

`dedup_key`：

```text
live:group_score:{event_id}:{group_id}:{time_bucket}
```

- `time_bucket = floor(now / push_interval_secs)`
- 同一个组、同一个时间桶只入队一次。
- 人工“立即推送本组”仍使用显式按钮，允许当次强制入队。

## 7. PK 生命周期与“准备下一组”提醒

### 7.1 采集阶段

采集器上报扩展到：

```json
{
  "webRid": "123360021147",
  "roomOnline": true,
  "nowMs": 1790000000000,
  "linkmicPhase": "idle|pk|talk",
  "linkmicUserCount": 6,
  "battle": { "...": "..." },
  "health": "ok"
}
```

阶段定义：

| phase | 判定 |
|---|---|
| `idle` | 没有 battle，连麦人数 < 2，或房间离线 |
| `pk` | battle 存在且 `battleStatus=1`，且未到预计结束 |
| `talk` | battle 已结束，但连麦用户仍在 / 会话仍存在 |
| `ended` | 连麦用户少于 2，或连续 N 拍无会话，确认本轮 PK 生命周期结束 |

### 7.2 状态机

```text
pending
  └─ 全员到齐 + PK 开始
       └─ running(pk)
            ├─ PK 结束，进入连麦说话
            │    └─ talking
            │         └─ 连麦会话结束
            │              ├─ finalized
            │              └─ next_group_ready 已通知
            └─ 异常：掉线 / 提前结束 / 重复局
                 └─ pending_review / waiting_restart
```

### 7.3 “准备下一组”触发规则

触发条件必须同时满足：

1. 当前组已有正式局，且已完成/定稿。
2. 观测到 `talk` 阶段。
3. `talk` 结束：
   - `linkmicPhase=ended`，或
   - 连续 2 次观测 `linkmicUserCount < 2`，间隔约 5 秒。
4. 当前组不是当前阶段最后一组，且下一组存在、尚未开始。
6. 本组 `next_group_ready` 尚未成功发送。

消息模板：

```text
【准备下一组】
晋级第 2 组连麦会话已结束。

下一组：晋级第 3 组
开赛：08:45
名单：狼哲、狼小宝、浩杰、啸辰、浩艺、浩龙

请提前拉人，满员后直接开 PK。
```

最后一场：

```text
【本场结束】
晋级第 7 组连麦会话已结束。
本场晋级赛已全部完成，准备生成决赛。
```

### 7.4 去重

```text
dedup_key = live:next_group_ready:{event_id}:{group_id}
```

- 只有真正发送成功才推进去重状态。
- 被出网闸门拦截时保持 `pending`，不写 `next_ready_notified_at`。

## 8. 人齐自动开始记录

### 8.1 名单匹配

每个小组已有 `live_group_seat`：

- `person_id`
- `douyin_uid`
- `nickname`
- `seed_order`

采集到 PK 参与者后，生成两组 UID：

```text
expected = group.seat.douyin_uid != ''
actual   = battle.scores[].userId
```

匹配结果：

| 结果 | 条件 | 行为 |
|---|---|---|
| `full` | expected 与 actual 集合完全一致 | 自动开始正式记录 |
| `partial` | actual 是 expected 的真子集 | 暂存采样，标记“缺人”，不自动占用正式局 |
| `extra` | actual 包含 expected 之外的人 | 需要管理员确认，避免拉错人 |
| `none` | 无匹配 UID / 无法识别 | 回退到“赛程顺序第一组”，但标记自动匹配失败 |

### 8.2 自动开始规则

当以下条件同时满足：

1. `battleStatus = 1`
2. `actual == expected`（`full`）
3. 该组尚未开始或仍未定稿
4. 当前 battleId 未被其它组占用

立即：

- 创建 `live_round`
- `round.status=live`
- `group.status=running`
- 从第一拍开始写样本
- 推送 `group_started`
- 记录 `full_roster_at`

不需要管理员点击“开始”。

### 8.3 异常兜底

- 缺人：继续记录样本，但状态 `waiting_roster`；不推进正式晋级。
- 多出来的人：记录但 `pending_review`，管理员选择是否作废/重开。
- 采集器断线：暂停推送，不把 stale 快照当结束。
- 重复 battleId：只落一局，不重复入队。

## 9. 数据模型改动

### 9.1 `wxbot_admin`

见 §4.2。

### 9.2 `live_event.team`

见 §5。

### 9.3 `live_round` 增列

```sql
ALTER TABLE live_round ADD COLUMN full_roster_at TIMESTAMPTZ;
ALTER TABLE live_round ADD COLUMN talk_started_at TIMESTAMPTZ;
ALTER TABLE live_round ADD COLUMN talk_ended_at TIMESTAMPTZ;
ALTER TABLE live_round ADD COLUMN linkmic_phase VARCHAR(16) NOT NULL DEFAULT '';
ALTER TABLE live_round ADD COLUMN roster_match VARCHAR(16) NOT NULL DEFAULT '';
```

### 9.4 `live_group` 增列

```sql
ALTER TABLE live_group ADD COLUMN next_ready_notified_at TIMESTAMPTZ;
ALTER TABLE live_group ADD COLUMN last_group_push_at TIMESTAMPTZ;
```

### 9.5 `live_push_job` 增列

```sql
ALTER TABLE live_push_job ADD COLUMN group_id BIGINT NOT NULL DEFAULT 0;
```

`kind` 扩展：

```text
group_started
group_score
group_finished
next_group_ready
final_result
```

### 9.6 观测结构扩展

`Observation` 增加：

```rust
#[serde(default, rename = "linkmicPhase")]
pub linkmic_phase: String,
#[serde(default, rename = "linkmicUserCount")]
pub linkmic_user_count: i32,
#[serde(default, rename = "linkmicUserIds")]
pub linkmic_user_ids: Vec<String>,
```

采集器负责从页面快照派生，后端负责二次校验。

## 10. API 设计

### 10.1 wxbot 管理员

```text
GET  /api/admin/wxbot/admins
POST /api/admin/wxbot/admins
PUT  /api/admin/wxbot/admins/{id}
```

字段：

```json
{
  "displayName": "饭团",
  "scope": "female",
  "wxAccountName": "饭团",
  "enabled": true
}
```

### 10.2 按组分数

```text
GET  /api/live/groups/{groupId}/scoreboard
POST /api/live/groups/{groupId}/push
```

`scoreboard` 返回：

- 组名
- 当前阶段
- 当前 roundId
- 最新分、组内排名、距上一位、距组内第一
- 晋级名额

### 10.3 下一组提醒

```text
POST /api/live/groups/{groupId}/notify-next
```

仅管理员可用，用于人工补发。自动路径由采集状态机触发。

## 11. 管理端 UI

### 11.1 机器人页

新增“wxbot 管理员”表格：

| 显示名 | 团别范围 | 微信账号 | 状态 | 操作 |
|---|---|---|---|---|
| 帅帅 | 全团 | shuai4444 | 已绑定 / 未绑定 | 编辑 |
| 鹏先生 | 全团 | 待绑定 | 未绑定 | 编辑 |
| 饭团 | 女团 | 待绑定 | 未绑定 | 编辑 |

操作：

- 编辑 scope
- 绑定 `wx_push_accounts.name`
- 启用/停用
- 发送测试消息

### 11.2 直播监控页

新增：

- 当前赛事团别标签
- “本组实时分推送”开关
- “下一组准备提醒”开关
- 每组的：
  - 已到齐人数 / 应到人数
  - 匹配状态：全员到齐 / 缺人 / 多余人员
  - PK 阶段：等待 / PK / 连麦会话 / 已结束
  - 最近一次组推送时间
  - 下一组提醒状态

## 12. 幂等、并发与时序

1. `ingest` 在事务中锁当前 group / battleId。
2. 同一 battleId 只允许一个正式 round。
3. 组推送任务用唯一 `dedup_key` 防重复。
4. `next_group_ready` 只在 talk→ended 的第一次转换触发。
5. 采集器重启后，靠 `battleId` + `group_id` 恢复，不重复提醒。
6. 被闸门拦截的推送保持 pending，不算成功。
7. 所有发送成功后再写 `notified_at`。

## 13. 本地开发计划

### Phase 1：权限与数据

- 新增 `wxbot_admin` 表和管理 API。
- 写入帅帅 / 鹏先生 / 饭团三条本地测试数据。
- 增加 `live_event.team`。
- 补权限单测。

### Phase 2：采集阶段与自动记录

- Observation 增加 `linkmicPhase` / `linkmicUserCount`。
- 实现 full/partial/extra/none 名单匹配。
- 全员到齐时自动 `running + 记录样本`。
- 补状态机单测和合成观测测试。

### Phase 3：按组推送

- `live_push_job` 增加 group_id。
- 实现 `group_score` / `group_finished`。
- 生成组内实时分文本。
- 按 `event.team + wxbot_admin.scope` 过滤收件人。

### Phase 4：连麦结束提醒

- 实现 talk→ended 判定与防抖。
- 生成 `next_group_ready` 消息。
- 最后一场生成“准备决赛 / 本场结束”消息。
- 人工补发按钮。

### Phase 5：本地验收

- 使用本地 mock 观测，不接真实 wxbot。
- `PUSH_OUT_ENABLED=0` 时只验证入队、去重、日志。
- 通过测试后再用 `PUSH_OUT_ENABLED=1` 做单个管理员灰度。

## 14. 测试用例

### 权限

- 帅帅 all：男团 + 女团赛事都可见。
- 鹏先生 all：男团 + 女团赛事都可见。
- 饭团 female：只可见女团；男团赛事不发送。
- 未知 wxbot：无管理权限。
- 禁用管理员：不接收消息。

### 自动开始

- 6 人应到 6 人实到 → 自动 live + 开始采样。
- 6 人应到 5 人实到 → partial，不自动占用正式局。
- 多 1 人 → extra，进入待确认。
- UID 全空 → none，按赛程顺序但标记告警。

### 组推送

- 同一组 60 秒内重复观测 → 只一条 group_score。
- 分数更新跨时间桶 → 允许下一条。
- 闸门关闭 → job 保持 pending，不写成功标记。
- 人工“立即推送本组” → 允许强制发送一次。

### 连麦结束提醒

- PK 结束 + talk 持续 10 秒 → 不提醒。
- talk 结束持续 2 拍 → 只提醒一次。
- 采集器抖动少一拍 → 不误报。
- 最后一组结束 → 发“准备决赛/本场结束”。
- 重复 battleId / 重复观测 → 不重复提醒。

## 15. 验收标准

- 帅帅和鹏先生能接收男、女团两类直播分组提醒；饭团只接收女团。
- 每个组最多一条同周期实时分消息；不会跨组串分。
- PK 结束后的连麦说话阶段不被误判为下一组可开。
- 连麦会话结束后 5~10 秒内发出下一组准备提醒。
- 全员到齐且 PK 开始后，无需手动点击即可产生 `live_round` 和采样。
- 缺人、多人、断线、重复局均有明确状态和管理员兜底入口。
- `PUSH_OUT_ENABLED=0` 时没有任何真实外发，且任务可恢复。

## 16. 需要产品确认的边界

1. 组内实时分是否允许所有组员互看完整排名，还是只能看自己的分和名次？
2. “准备下一组”只通知管理员，还是同时通知下一组全部成员？
3. 连麦会话结束判定是否采用“连续 2 拍人数 < 2”，还是等运营手动点击结束？
4. 鹏先生 / 饭团当前没有 wxbot 绑定，需要扫码后绑定到哪个微信账号？
5. 决赛是否只推前 3 名，还是全员成绩都推？

## 17. 本地备注（2026-11-08）

- 本设计只落文档，不改远端服务器。
- 当前本地已能抓取测试直播间：
  - webRid：`123360021147`
  - 已捕获 8 人 PK 实时分
  - 采集器心跳和房间健康度正常
- 当前本地 wxbot 账号表有 `shuai4444` 的 token/session，但实际发送返回 `微信接口错误 (-2): prepare failed`，说明登录/session 已失效；真实推送前需要重新扫码。
- 远端 wxbot 当前也未登录（`hasToken=false`），不能把“入队”当成“已发送”。
