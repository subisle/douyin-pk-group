# 项目约定（本地测试阶段）

> 2026-10-16 用户口径，本地开发/测试期间长期有效。**以下三条是纪律，不是建议。**

## 1. 只做本地，不推上游

本轮只把**本地**跑通做好。64 / 147 / rk3318 等远端环境、部署脚本、生产配置
**一律不动**，也不要主动去同步/推送。远端有差异先记下来，等明确要求再说。

## 2. 不备份数据库

测试阶段**不做数据库备份**（`backups/` 下不再新增 dump/CSV 备份文件）。
改动直接在本地库上做；需要回滚就靠迁移脚本与 SQL 反向操作，不靠备份文件。

## 3. 不向任何 bot 发消息（除管理员三位登录后触发）

**默认一律不真发**。本地库里有真实推送绑定与待发任务（`push_jobs`），
QQ 机器人服务启动即连 gateway —— 不设闸就是真发到主播手机上。

- 统一开关：`PUSH_OUT_ENABLED`
  - 未配置 / `0`（**默认，测试阶段必须保持这个**）= 所有对外发送只记 WARN 日志、不真发；
  - `1` = 恢复真实发送（仅在主动验证发送链路时临时打开，验完立刻改回 `0`）。
- 闸门装在所有出网的必经窄口，新增发送路径也绕不过：
  - `bot/weixin.rs::user_send_text`（微信文本，含推送与管理员下发）
  - `bot/weixin.rs::WxTransport::send_proactive`（管理员全局机器人通道）
  - `store/wxmedia.rs::user_send_image`（微信图片，未开时连 CDN 上传都不做）
  - `bot/qq.rs::send`（QQ 回复/发送）
  - `push.rs::send_binding` / `send_binding_images`（推送层，双保险）
- 有单测守住：`push::tests::push_out_*`、`out_blocked_gate`（默认必须关闭）。
- **例外**：只有管理员三位（浩雨 / 饭团 / 鹏先生）**本人登录后在界面上主动触发**
  的发送才允许真实出网；在此之前一律拦截。

## 附：本地环境速查

| 项 | 值 |
|---|---|
| 服务 | `cd rust && cargo run -p pk-server`（读 `rust/.env`，端口 8787） |
| 数据库 | PG `127.0.0.1:3318` / `douyin_dev`（读 `pk_reader`，写 `pk_writer`，属主 `zmy`） |
| 管理员口令 | `rust/.env` 的 `ADMIN_PASSWORD`（fail-closed，未配则管理接口全关） |
| 展示端账号 | 浩雨=any（总管理员，原「帅帅」，开发期绑 person 20 浩雨）/ 饭团=female（女团）/ 鹏先生=any（绑 person 1027） |
| 迁移 | `pk-server migrate`（本机表属主是 `zmy`，用 `DB_MIGRATOR_USER=zmy` 跑） |
