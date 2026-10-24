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
- 不提供个人数据、我的抖音号、密码、手机号、推送开关等用户能力；
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
| `get_daily_briefing` | 每日播报：TOP5、总音浪、开播/未播人数 | 管理员 |
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

- `period=daily` 使用 `date`，缺省为最新业务日；
- `period=monthly` 使用 `month`，缺省为最新业务日所在月；
- `period=yearly` 使用 `year`，缺省为最新业务日所在年；
- `limit` 默认 10，最大 50。

返回精简结构：

```json
{
  "summary": "2026-10-23 男团日榜，浩龙第 1，共 89 人。",
  "asOf": "2026-10-23",
  "team": "male",
  "period": "daily",
  "rows": [
    {"rank": 1, "name": "浩龙", "wave": 2245607, "minutes": 4529, "live": true, "absentDays": 0}
  ],
  "meta": {"total": 89, "truncated": true}
}
```

### 6.2 `get_daily_briefing`

参数：

```json
{
  "team": "male|female",
  "date": "2026-10-23"
}
```

`team` 缺省为男团，`date` 缺省为最新业务日。返回一段可直接朗读的每日播报，
以及 TOP5、总音浪、开播人数和未播人数。它只读公开名册，不涉及任何用户身份。

### 6.3 语音友好的返回约定

所有工具返回：

- `summary`：一到两句可直接朗读的中文摘要；
- `data`：结构化数据，供小智进一步解释；
- `actions`：可选，例如“要不要查看前 10 名”；
- 错误统一为 `code` + `message`，例如 `AUTH_REQUIRED`、`TEAM_FORBIDDEN`、`NO_DATA`、`RATE_LIMITED`。

MCP 的 `content` 放 JSON 文本，同时如客户端支持 `structuredContent` 则放同一份结构化结果。

## 7. 安全与稳定性

- 只接受 WSS；endpoint token 不打印、不写入仓库；
- 单连接限流，单次工具最多返回 50 行；
- 单次查询超时 3 秒，超时返回可读错误，不阻塞小智会话；
- 工具调用日志只记录工具名、参数摘要、耗时和结果状态；不记录 endpoint token、密码、手机号、完整抖音号；
- 所有查询只读，使用现有 reader 池；禁止 MCP 工具执行任意 SQL、DDL 或管理写接口；
- 小智只作为管理员入口；不提供普通用户身份，不接受客户端传入的手机号或个人 token。

## 8. 分阶段实施

### P0：协议与管理员数据

- MCP 协议层：initialize / tools/list / tools/call / ping；
- 小智出站 WSS 连接 + endpoint token；
- `get_team_ranking`、`get_team_summary`、`search_anchor`、`get_anchor_rank`、`get_daily_briefing`、`get_data_status`；
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

