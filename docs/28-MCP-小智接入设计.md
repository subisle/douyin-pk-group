# MCP / 小智 AI 接入设计

> 状态：设计稿，暂不实现  
> 目标：让小智通过 MCP 调用本项目的只读数据能力，支持自然语言查询榜单、个人数据和数据状态。

## 1. 结论

**使用 MCP 对接小智，同时保留现有 API 作为数据层。**

不是二选一：

- 现有 `/api/public`、`/api/admin` 继续作为浏览器、管理后台和 QQ/微信机器人的接口；
- MCP 是面向小智的工具层，只暴露经过筛选的只读工具；
- MCP 工具内部直接调用本项目的 service/query 函数，或受控调用内部 API，不把任意 REST 接口地址和数据库能力暴露给大模型。

不建议让小智直接调用任意 REST API，原因：

1. 大模型需要的是明确的工具名称、参数和简短结果，而不是原始接口树；
2. 直接暴露 API 会扩大权限面，容易把管理接口、手机号、抖音号、内部 personId 带出去；
3. MCP 可以统一做用户识别、团别限制、限流、审计和语音友好的结果裁剪。

## 2. 推荐总体架构

```text
小智设备 / 小智云端 LLM
        │
        │ MCP: initialize / tools/list / tools/call
        ▼
pk-server MCP Adapter
  ├─ MCP 协议解析与工具注册
  ├─ 小智用户 -> 项目账号身份映射
  ├─ 团别 / 权限 / 限流 / 审计
  └─ 只读 Tool Dispatcher
        │
        ▼
现有业务 service / query 层
  ├─ 日榜 / 月榜 / 年榜
  ├─ 个人数据 / 我的抖音号
  ├─ 名册搜索
  ├─ 公告 / 数据状态
  └─ 分组方案（后续）
```

建议 MCP Adapter 放在 `pk-server` 进程内，复用 `AppState` 和现有查询函数，不新增第二套数据库访问逻辑。

## 3. 传输方式

### 3.1 标准 MCP 客户端

优先实现标准 **Streamable HTTP MCP**：

- `POST /mcp`：JSON-RPC 请求（`initialize`、`tools/list`、`tools/call`、`ping`）；
- `GET /mcp`：需要 SSE 的 MCP 客户端可选使用；
- `DELETE /mcp`：有会话状态时用于关闭会话。

生产环境只通过 Caddy/Cloudflare 暴露 HTTPS/WSS，不直接暴露容器端口。

### 3.2 自建小智服务

自建 `xiaozhi-esp32-server` 的 MCP 接入点使用 WebSocket 时，增加兼容端点：

```text
wss://pk.subisle.com/mcp/ws?token=<connector-token>
```

该 WebSocket 端点复用同一个 MCP Dispatcher，不复制工具逻辑。小智侧的 `mcp_endpoint` 指向这个地址。

如果实际使用的是小智官方云端，则优先使用其支持的 Streamable HTTP/SSE 接入方式；官方云和自建服务的具体地址在实施前确认。

## 4. 身份与权限

### 4.1 两层身份

1. **连接身份**：小智服务连接本项目的 MCP connector token，用于证明请求来自已授权的小智实例；
2. **用户身份**：小智传入的设备/用户 ID，通过 `mcp_identities` 映射到现有 `my_accounts` / `person`。

建议新增配置：

```env
MCP_ENABLED=1
MCP_CONNECTOR_TOKEN=<random-32-bytes>
MCP_PUBLIC_TOOLS=1
MCP_PRIVATE_TOOLS=1
```

`MCP_CONNECTOR_TOKEN` 只放环境变量或数据库配置，不写入前端、仓库或日志。

用户身份优先级：

1. 小智传入的 `X-Xiaozhi-User-Id` 若已绑定，取对应 `my_accounts`；
2. 未绑定时只允许公共榜单工具，个人工具返回“请先绑定账号”；
3. 管理员身份单独配置，普通用户不能通过说“查管理员数据”越权。

如果小智无法稳定传用户 ID，则增加一次性绑定码流程：

```text
小智说：绑定码 4821
用户在展示端「我的」页生成/查看绑定码
服务端把该小智用户 ID 绑定到 my_accounts
```

绑定码一次性、5 分钟有效；绑定记录写入 `mcp_identities`，不保存明文手机号或密码。

### 4.2 团别规则

- 男团账号只能查男团和本人数据；
- 女团账号只能查女团和本人数据；
- 全团/管理员账号可以显式查 `all`，或分别查男女团；
- 个人数据工具不从参数接收姓名，只从 MCP 会话身份取本人；
- 公共榜单可以按 `team=male|female` 查询，不返回手机号、抖音号、内部 `personId`。

## 5. 首期工具清单

首期只做只读工具，不做导入、删除、改绑、推送开关等写操作。

| 工具 | 用途 | 权限 |
| --- | --- | --- |
| `get_team_ranking` | 查询日/月/年团内榜单 | 已授权小智用户 |
| `get_team_summary` | 查询总音浪、开播人数、缺数据人数 | 已授权小智用户 |
| `search_anchor` | 按艺名/部分艺名找人 | 已授权小智用户 |
| `get_anchor_rank` | 查某位主播的团别、名次、音浪、时长、未播天数 | 已授权小智用户 |
| `get_my_data` | 查当前小智用户自己的日/月/年数据 | 已绑定账号 |
| `get_my_accounts` | 查当前用户名下多个抖音号的音浪 | 已绑定账号 |
| `get_data_status` | 查最新业务日、数据更新时间、是否有缺数据 | 已授权小智用户 |
| `get_announcements` | 查当前有效公告/每日冠军等公开信息 | 已授权小智用户 |

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

### 6.2 `get_my_data`

无用户输入参数，身份完全来自 MCP 会话：

```json
{
  "scope": "daily|monthly|yearly|total",
  "date": "2026-10-23"
}
```

只返回本人数据；如果未绑定账号，返回 `BIND_REQUIRED`，不尝试按姓名猜测。

### 6.3 语音友好的返回约定

所有工具返回：

- `summary`：一到两句可直接朗读的中文摘要；
- `data`：结构化数据，供小智进一步解释；
- `actions`：可选，例如“要不要查看前 10 名”；
- 错误统一为 `code` + `message`，例如 `AUTH_REQUIRED`、`TEAM_FORBIDDEN`、`NO_DATA`、`RATE_LIMITED`。

MCP 的 `content` 放 JSON 文本，同时如客户端支持 `structuredContent` 则放同一份结构化结果。

## 7. 安全与稳定性

- 只接受 HTTPS/WSS；connector token 不进 URL 日志时优先走 Header；
- WebSocket 兼容模式若必须用 query token，使用 TLS、短 token、可轮换，并在 Caddy 关闭 query 日志；
- 单用户限流：建议 20 次/分钟；单次工具最多返回 50 行；
- 单次查询超时 3 秒，超时返回可读错误，不阻塞小智会话；
- 工具调用日志记录：用户标识、工具名、参数摘要、耗时、结果状态；不记录密码、完整手机号、完整抖音号；
- 所有查询只读，使用现有 reader 池；禁止 MCP 工具执行任意 SQL、DDL 或管理写接口；
- 对 `team=all`、个人数据和管理员工具做服务端二次校验，不能只依赖小智的 prompt。

## 8. 分阶段实施

### P0：协议与公共数据

- MCP 协议层：initialize / tools/list / tools/call / ping；
- connector token 校验；
- `get_team_ranking`、`get_team_summary`、`search_anchor`、`get_anchor_rank`、`get_data_status`；
- 本地 MCP 客户端冒烟测试。

### P1：个人数据

- `mcp_identities` 表和绑定码流程；
- `get_my_data`、`get_my_accounts`；
- 用户身份、团别和限流审计。

### P2：小智通道和语音体验

- 官方云 / 自建小智实际传输方式联调；
- 语音摘要、追问建议、错误话术；
- 监控面板和调用日志。

### P3：导出和高级查询

- `export_ranking_csv`：生成短时下载地址，或通过小智支持的附件通道发送；
- `get_group_plan`；
- 管理员专用工具单独授权，不与普通用户工具混在同一 token 权限里。

## 9. 需要确认的产品项

1. 小智使用的是官方 `xiaozhi.me` 云端，还是自建 `xiaozhi-esp32-server`？
2. 小智是否能稳定传入用户/设备 ID，还是需要我们的绑定码流程？
3. 普通用户是否允许查询另一个团的公共榜单，还是只能查看本团？
4. 首期是否需要语音查询“我的预计到手”，还是先不暴露收益数据？
5. CSV 导出在小智上要“下载链接”还是直接附件？小智客户端是否支持接收文件？

## 10. 参考

- [QQ 机器人不是本设计的 MCP 传输层；QQ 导出仍走现有机器人通道]
- [MCP 官方规范](https://modelcontextprotocol.io/specification)
- [小智 ESP32 MCP 接入说明](https://github.com/xinnan-tech/xiaozhi-esp32-server/blob/main/docs/mcp-endpoint-enable.md)

