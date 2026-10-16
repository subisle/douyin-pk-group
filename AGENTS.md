# 项目约定（本地测试阶段）

> 2026-10-16 用户口径，本地开发/测试期间长期有效。**以下三条是纪律，不是建议。**

## 1. 只做本地，不推上游

本轮只把**本地**跑通做好。64 / 147 / rk3318 等远端环境、部署脚本、生产配置
**一律不动**，也不要主动去同步/推送。远端有差异先记下来，等明确要求再说。

## 2. 不备份数据库

测试阶段**不做数据库备份**（`backups/` 下不再新增 dump/CSV 备份文件）。
改动直接在本地库上做；需要回滚就靠迁移脚本与 SQL 反向操作，不靠备份文件。

## 3. 对外发送：闸门口径（2026-10-17 起**全开**）

> **2026-10-17 用户口径变更**：上线在即，对外发送（微信推送 / QQ 回复 / 管理员消息下发）
> 改为**默认全开**。下面「测试阶段默认关」的旧纪律自本日起作废，保留仅供追溯。

**当前口径 = 全开**：`PUSH_OUT_ENABLED=1`、`PUSH_OUT_ALLOW` 留空（不限制收件人）。

- 统一开关：`PUSH_OUT_ENABLED`
  - `1` / `true` = 真实发送（**当前值，本机与生产一致**）；
  - 未配置 / `0` = 所有对外发送只记 WARN 日志、不真发（**仅在排查/回滚时临时改**）。
- 白名单：`PUSH_OUT_ALLOW`（逗号分隔的展示端账号名）。**留空 = 不额外限制、全部放行**；
  只想灰度给几个人时才填名字。
- 闸门装在所有出网的必经窄口，新增发送路径也绕不过：
  - `bot/weixin.rs::user_send_text`（微信文本，含推送与管理员下发）
  - `bot/weixin.rs::WxTransport::send_proactive`（管理员全局机器人通道）
  - `store/wxmedia.rs::user_send_image`（微信图片，未开时连 CDN 上传都不做）
  - `bot/qq.rs::send`（QQ 回复/发送）
  - `push.rs::send_binding` / `send_binding_images`（推送层，双保险）
- 单测：`push::tests::push_out_*`、`out_blocked_gate`、`blocked_is_distinguishable_from_success`。
  ⚠️ 这些单测钉的是**开关解析语义**（`1/true` 才开），不是「默认关闭」——
  2026-10-17 曾尝试把 `yes/on` 也认作开，因与本纪律冲突已撤回。

**⚠️ 被闸门拦下 ≠ 发送成功**（2026-10-17 修复）：`send_binding*` 拦截时返回
`Err(Blocked)`，调用方据此**不推进**去重标记（`last_report_date` / `last_lineage_date` /
`last_notice_date`），否则关闸期间的消息会被永久吞掉（标记已消耗、开闸也不补推）。

**回滚方式**：把 `PUSH_OUT_ENABLED` 设回 `0`（或删掉该变量）即可恢复「只记日志不真发」，
无需改代码。

## 附：本地环境速查

| 项 | 值 |
|---|---|
| 服务 | `cd rust && cargo run -p pk-server`（读 `rust/.env`，端口 8787） |
| 数据库 | PG `127.0.0.1:3318` / `douyin_dev`（读 `pk_reader`，写 `pk_writer`，属主 `zmy`） |
| 管理员口令 | `rust/.env` 的 `ADMIN_PASSWORD`（fail-closed，未配则管理接口全关） |
| 展示端账号 | 浩雨=any（总管理员，原「帅帅」，开发期绑 person 20 浩雨）/ 饭团=female（女团）/ 鹏先生=any（绑 person 1027） |
| 迁移 | `pk-server migrate`（本机表属主是 `zmy`，用 `DB_MIGRATOR_USER=zmy` 跑） |
