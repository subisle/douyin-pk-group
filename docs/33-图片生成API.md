# 图片生成 API（管理员口令鉴权）

> 2026-11-10 用户口径：不另建图片 API 密钥体系，**直接使用管理员口令作为 API Key**。
>
> 本文只描述本地/服务端调用方式；是否部署由部署流程单独决定。

## 1. 鉴权

所有 `/api/images/*` 请求都必须带管理员口令。推荐放在 `X-API-Key` 请求头：

```bash
export ADMIN_PASSWORD='从服务器环境变量读取，不要写进脚本或 Git'
```

```http
X-API-Key: <ADMIN_PASSWORD>
```

也兼容标准 Bearer 形式：

```http
Authorization: Bearer <ADMIN_PASSWORD>
```

- 口令来源与后台管理员登录一致：`ADMIN_PASSWORD` 优先；未配置时按项目既有规则使用默认口令。
- 修改 `ADMIN_PASSWORD` 后需要重启 `pk-server` 才会生效。
- 管理员口令只放请求头，不要放进 URL、截图、日志或提交到 Git。
- 缺 Key 或 Key 错误返回 `401 application/json`。
- 图片接口**只读数据库**，不写数据、不发微信、不推进任何推送去重标记。

本地服务地址默认：

```text
http://127.0.0.1:8787
```

健康检查（同样需要 Key）：

```bash
curl --fail-with-body \
  -H "X-API-Key: $ADMIN_PASSWORD" \
  'http://127.0.0.1:8787/api/images/health'
```

成功响应：

```json
{
  "ok": true,
  "auth": "admin-password",
  "endpoints": [
    "/api/images/ranking.png",
    "/api/images/lineage.png"
  ]
}
```

## 2. 月榜长条图

```http
GET /api/images/ranking.png
```

| 参数 | 必填 | 可选值 | 默认值 | 说明 |
|---|---:|---|---|---|
| `period` | 否 | `YYYY-MM` | 当前月 | 统计月份 |
| `team` | 否 | `male` / `female` / `all` | `all` | 男团 / 女团 / 全团 |
| `metric` | 否 | `wave` / `minutes` | `wave` | 音浪 / 时长 |

示例：生成 2026 年 10 月男团音浪排名图：

```bash
curl --fail-with-body \
  -H "X-API-Key: $ADMIN_PASSWORD" \
  -o '2026-10-男团音浪排名.png' \
  'http://127.0.0.1:8787/api/images/ranking.png?period=2026-10&team=male&metric=wave'
```

一次生成男团、女团、全团各自的音浪和时长共 6 张：

```bash
for team in male female all; do
  for metric in wave minutes; do
    curl --fail-with-body \
      -H "X-API-Key: $ADMIN_PASSWORD" \
      -o "2026-10-${team}-${metric}.png" \
      "http://127.0.0.1:8787/api/images/ranking.png?period=2026-10&team=${team}&metric=${metric}"
  done
done
```

图口径与后台月榜一致：

- 整月导入值优先，缺整月值回落到日明细；
- 主号与副号按人合并；
- 音浪、时长分别排序；
- 只显示名次、主播、长条占比和对应数值；
- 不显示服务器来源、全员累计、上榜人数。

## 3. 师门图

```http
GET /api/images/lineage.png
```

`master` 必填，且必须是未删除、未离开的师傅艺名。

三种统计模式：

| 模式 | 参数 | 示例 |
|---|---|---|
| 月报 | `master` + `month=YYYY-MM` | `master=浩龙&month=2026-10` |
| 日报 | `master` + `date=YYYY-MM-DD` | `master=浩龙&date=2026-11-09` |
| 区间 | `master` + `from=YYYY-MM-DD` + `to=YYYY-MM-DD` | `master=浩龙&from=2026-11-01&to=2026-11-09` |

补充参数：

| 参数 | 默认值 | 说明 |
|---|---:|---|
| `page` | `0` | 第几页，从 0 开始；超出范围时自动取最后一页 |
| `qualified-minutes` | `360` | 区间模式每天合格分钟数 |
| `severe-minutes` | `240` | 区间模式每天最低分钟数 |

不传 `month`、`date`、`from/to` 时，日报模式自动使用当前最新业务日期。

示例：生成 11 月 1 日至最新日期区间图：

```bash
curl --fail-with-body \
  -H "X-API-Key: $ADMIN_PASSWORD" \
  -D /tmp/lineage-headers.txt \
  -o '师门总览-浩龙-2026-11-01至2026-11-09.png' \
  'http://127.0.0.1:8787/api/images/lineage.png?master=浩龙&from=2026-11-01&to=2026-11-09'
```

分页响应头：

```http
X-Lineage-Page: 1
X-Lineage-Pages: 1
Content-Type: image/png
Content-Disposition: inline; filename="lineage-2026-11-01_2026-11-09-page-1.png"
```

师门图复用 wxbot 现有渲染链路：`lineage_report` + `push::lineage_report_pngs_for`，因此版式、字段显隐、师傅名水印与微信推送图一致。

## 4. 错误响应

| 状态码 | 含义 |
|---:|---|
| `400` | 参数格式不对、师傅不存在、周期内无数据 |
| `401` | 缺少 API Key 或管理员口令错误 |
| `500` | 数据库查询或 PNG 渲染失败；响应不会回传 SQL 细节 |

示例：

```json
{"error":"缺少 X-API-Key"}
```

```json
{"error":"API Key 无效"}
```

## 5. 安全与运维约定

- 不新增强制独立的 API Key 表；管理员口令是唯一凭证。
- 请求头中的 Key 不应出现在反向代理访问日志中；部署时确认反代不记录 `X-API-Key`。
- 图片生成是只读操作，可与直播监控、音浪导入并行运行。
- 批量调用时建议串行或限制并发，避免同一进程同时进行大量 SVG→PNG 渲染。
- 生产调用示例：

```bash
BASE='http://43.142.30.113:81'
curl --fail-with-body \
  -H "X-API-Key: $ADMIN_PASSWORD" \
  -o /tmp/ranking.png \
  "$BASE/api/images/ranking.png?period=2026-10&team=all&metric=wave"
```
