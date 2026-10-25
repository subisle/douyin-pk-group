# MCP / 小智 AI 接入设计

> 状态：P0 已实现（管理员只读 MCP；小智官方云端 endpoint 已接入）  
> 目标：让小智管理员通过 MCP 调用本项目的只读数据能力，支持自然语言查询榜单、每日播报和数据状态。

## 1. 结论

**使用 MCP 对接小智，同时保留现有 API 作为数据层。**

不是二选一：

- 现有 `/api/public`、`/api/admin` 继续作为浏览器、管理后台和 QQ/微信机器人的接口；
- MCP 是面向小智的工具层，只暴露经过筛选的只读工具；
- MCP 工具内部直接调用本项目的 service/query 函数，或受控调用内部 API，不把任意 REST 接口地址和数据库能力暴露给大模型。

不建议让小智直接调用任意 REST API，原因：

1. 大模型需要的是明确的工具名称、参数和简短结果，而不是原始接口树；
2. 直接暴露 API 会扩大权限面，容易把管理接口、手机号、抖音号、内部 personId 带出去；
3. MCP 可以统一做管理员鉴权、团别限制、限流、审计和语音友好的结果裁剪。

## 2. 推荐总体架构

```text
小智设备 / 小智云端 LLM
        │
        │ MCP: initialize / tools/list / tools/call
        ▼
pk-server MCP Adapter
  ├─ MCP 协议解析与工具注册
  ├─ 管理员 connector token 校验
  ├─ 团别 / 限流 / 审计
  └─ 只读 Tool Dispatcher
        │
        ▼
现有业务 service / query 层
  ├─ 日榜 / 月榜 / 年榜
  ├─ 名册搜索
  ├─ 每日播报
  ├─ 公告 / 数据状态
  └─ 分组方案（后续）
```

建议 MCP Adapter 放在 `pk-server` 进程内，复用 `AppState` 和现有查询函数，不新增第二套数据库访问逻辑。

## 3. 传输方式

小智官方云端提供的是**出站 WebSocket MCP endpoint**：

```text
wss://api.xiaozhi.me/mcp/?token=<endpoint-token>
```

本项目作为 MCP Server 主动连接该 endpoint，接收 `initialize`、`tools/list`、
`tools/call` 和 `ping`，通过同一个 WebSocket 返回 JSON-RPC 结果。

- 不新增公网 MCP 入口；
- 不通过 Caddy 暴露 `/mcp`；
- endpoint token 只放生产环境变量 `XIAOZHI_MCP_ENDPOINT`，不写仓库、不打印日志；
- 断线自动指数退避重连。

## 4. 身份与权限

**当前只对接管理员，没有普通用户身份。**

小智 endpoint token 即管理员连接凭证；所有工具都是管理员只读工具。服务端不接受
小智传入的普通用户名、手机号或个人 token。

- 所有工具都允许查询男团、女团对外展示数据；
- 允许按艺名生成个人每日播报，但只使用已对外展示的音浪、时长、名次、未播天数等统计；
- 不提供账号登录、我的抖音号、密码、手机号、推送开关等用户能力；
- 不提供导入、删除、改绑、发布等写操作；
- 不返回手机号、抖音号、内部 `personId`；
- 如 endpoint token 泄露，直接在小智后台重新生成并替换环境变量。

建议配置：

```env
XIAOZHI_MCP_ENDPOINT=wss://api.xiaozhi.me/mcp/?token=<endpoint-token>
```

## 5. 首期工具清单

首期只做只读工具，不做导入、删除、改绑、推送开关等写操作。

| 工具 | 用途 | 权限 |
| --- | --- | --- |
| `get_team_ranking` | 查询日/月/年团内榜单 | 管理员 |
| `get_team_summary` | 查询总音浪、开播人数、未播人数 | 管理员 |
| `search_anchor` | 按艺名/部分艺名找人 | 管理员 |
| `get_anchor_rank` | 查某位主播的团别、名次、音浪、时长、未播天数 | 管理员 |
| `get_personal_daily_briefing` | 个人每日播报：今日表现、上下名次差距、本月累计/月榜、预估到手和状态总结 | 管理员 |
| `get_daily_briefing` | 约3分钟语音稿：先总结，再播前十名、逐名评价和卡位差距 | 管理员 |
| `get_data_status` | 查最新业务日、可用日期数量、展示人数 | 管理员 |

后续按需要增加：

- `get_group_plan`：查询已发布分组方案；
- `export_ranking_csv`：生成 CSV 并提供短时下载地址；
- `get_lineage`：族谱仅对已绑定师门关系的账号开放。

## 6. 工具参数和返回

### 6.1 `get_team_ranking`

```json
{
  "team": "male|female",
  "period": "daily|monthly|yearly",
  "date": "2026-10-23",
  "month": "2026-10",
  "year": "2026",
  "limit": 10
}
```

说明：

- `period=daily` 使用 `date`，缺省为最新业务日；`date` 也接受 `today/今天`、`yesterday/昨天`、`latest/最新`；
- `period=monthly` 使用 `month`，缺省为最新业务日所在月；
- `period=yearly` 使用 `year`，缺省为最新业务日所在年；
- `limit` 默认 10，最大 50。

返回给小智的 `content.text` 是可直接朗读的中文文本，例如：

```text
男团2026-10-23日榜共89人，第一名浩龙，音浪224万5千。
第1名浩龙，音浪224万5千，时长75小时29分钟，未播0天，已开播。
```

音浪统一使用“万/千”的口语单位，例如 `52542` 输出为 `5万2千`。
结构化字段只在服务端内部组装，不再以 JSON 文本交给语音模型，避免模型再次读出
`waveValue` 等无单位原始整数。

### 6.2 `get_daily_briefing`

参数：

```json
{
  "team": "male|female",
  "date": "2026-10-23"
}
```

`team` 缺省为男团，`date` 缺省为最新业务日。返回约 3 分钟的可直接朗读播报稿：
开头先给榜首优势、前三/前十集中度等整体总结，随后直接播前十名，不寒暄、不说明
“有没有数据”，不说“我查一下 / 以下是”。总音浪、人均音浪、逐名音浪、相邻差距和
第十名门槛全部使用中文口语单位（如 `5万2千`）。播报始终使用真实业务日期；若最新
业务日就是昨天，会明确说“昨日数据”，不会把昨天叫成今天。

### 6.3 `get_personal_daily_briefing`

参数：

```json
{
  "name": "浩龙",
  "date": "2026-10-23"
}
```

按艺名生成个人每日播报，包含指定业务日音浪、团内名次、时长、上下名次差距、本月
累计音浪、月榜名次、未播天数、预估到手和一句状态总结。播报同样从姓名和日期直接开始，
不先回答“有没有数据”；日期若为昨天，会明确标注“昨日数据”。

### 6.4 业务日期与“今天”口径

- 数据按“次日导入前一天”的业务口径运行，因此今天查询时通常最新业务日是昨天；
- MCP 接受 `YYYY-MM-DD`、`today/今天`、`yesterday/昨天`、`latest/最新`；
- 如果模型传入今天日期、但今天还没有业务数据，服务端自动落到当前最新业务日；
- `asOf` 永远是真实业务日期，播报以 `asOf` 为准，禁止把昨天数据说成今天；
- `get_data_status` 返回 `calendarToday`、`latestDate`、`dataLagDays`、`freshness`，
  供小智先确认日期再查询。

### 6.5 语音友好的返回约定

所有工具返回：

- `content.text`：只放可直接朗读的中文文本，不暴露原始 JSON 数字；
- `summary` / `speechText`：服务端内部摘要，`speechText` 优先；
- 音浪、总音浪、差距等数值统一转成“万/千”口语单位；
- 错误统一为可朗读的中文说明。

不向小智返回 `structuredContent` 和 `waveValue` 等原始整数，防止语音模型忽略单位、
把 `52542` 直接读成一长串数字。

## 7. 安全与稳定性

- 只接受 WSS；endpoint token 不打印、不写入仓库；连接每 20 秒发送 WebSocket 心跳，断开后 1 秒起自动重连；
- 单连接限流，单次工具最多返回 50 行；
- 单次查询超时 3 秒，超时返回可读错误，不阻塞小智会话；
- 工具调用日志只记录工具名、参数摘要、耗时和结果状态；不记录 endpoint token、密码、手机号、完整抖音号；
- 所有查询只读，使用现有 reader 池；禁止 MCP 工具执行任意 SQL、DDL 或管理写接口；
- 小智只作为管理员入口；不提供普通用户身份，不接受客户端传入的手机号或个人 token。

## 8. 分阶段实施

### P0：协议与管理员数据

- MCP 协议层：initialize / tools/list / tools/call / ping；
- 小智出站 WSS 连接 + endpoint token；
- `get_team_ranking`、`get_team_summary`、`search_anchor`、`get_anchor_rank`、`get_personal_daily_briefing`、`get_daily_briefing`、`get_data_status`；
- 本地 MCP 客户端冒烟测试。

### P1：小智语音体验

- 官方云 MCP endpoint 稳定性与重连联调；
- 语音摘要、追问建议、错误话术；
- 监控面板和调用日志。

### P2：导出和高级查询

- `export_ranking_csv`：生成短时下载地址，或通过小智支持的附件通道发送；
- `get_group_plan`；
- 如未来增加写操作，必须另建独立 token 和单独授权，不复用当前只读 endpoint。

## 9. 需要确认的产品项

1. 小智是否已有管理员/角色限制，还是只依赖 endpoint token？
2. 每日播报是用户主动问“今日播报”，还是需要小智侧另行配置定时触发？
3. CSV 导出在小智上要“下载链接”还是直接附件？小智客户端是否支持接收文件？

## 10. 参考

- [MCP 官方规范](https://modelcontextprotocol.io/specification)
- [小智 ESP32 MCP 接入说明](https://github.com/xinnan-tech/xiaozhi-esp32-server/blob/main/docs/mcp-endpoint-enable.md)

