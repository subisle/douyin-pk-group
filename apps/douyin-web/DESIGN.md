# 管理端设计规范（apps/douyin-web）

> 2026-10-16 立。**唯一设计锚**：改管理端界面前先读这里；与本文冲突的写法视为待清理项。
> 来源：从现有代码提取 + IBM Carbon 的选型/状态/间距方法（借方法，不借脸）。零依赖，样式全在
> `src/styles.css`（另有 `src/lineage.css` 只管族谱画布）。

## 0. 定位与功能契约

内部运营工具（主播台账 / 数据导入 / 公告 / 机器人 / 消息下发），受众是运营同学，**信任与效率优先**，
不是营销页面。三条硬契约：① 三秒内看清「这是什么数据、现在什么状态」；② 每个危险动作可确认或可撤销，
失败必须说清下一步；③ 键盘可达、`focus-visible` 始终可见。

拨盘（数字是给实现看的，人话在括号里）：视觉冒险度 **3**（克制的企业控制台，不玩噱头）·
动效强度 **2**（瞬时反馈为主，无入场编排）· 信息密度 **6**（一屏尽量多装，但分组差必须保留）。

## 1. Token（语义命名，不按色值命名）

| 角色 | token | 说明 |
|---|---|---|
| 画布 / 卡片 | `--canvas` `--card` | 灰底白卡，不靠重阴影分层 |
| 文字 | `--ink` `--ink2` `--ink3` | 主 / 次 / 弱；弱文字只用于提示，不承载关键信息 |
| 线 | `--line` `--line-strong` | 1px 发丝线；**没有 `--hairline`**（历史遗留名，见 §7） |
| 主色 | `--brand` `--brand-dark` `--brand-soft` | 靛蓝；主按钮、选中态、焦点环 |
| 语义 | `--good` `--warn` `--bad`（各带 `-bg`） | 状态徽标、告警条；颜色不作唯一信息通道 |
| 圆角 | `--radius`(12) `--radius-ctl`(8) | 卡片 12、控件 8 |
| 焦环 | `--ring` | 全局 `:focus-visible` 与输入框 focus 共用 |
| 动效 | `--ease` | 150ms；`prefers-reduced-motion` 下统一降到 0.01ms |

## 2. 排版与间距

- 字体：`-apple-system / PingFang SC / Noto Sans SC`（系统中文栈，**不引装饰 webfont**）；UI **禁用斜体**。
- 字阶按 role：区块标题 15px / 卡片标题 13.5–14.5px / 正文与控件 13–14px / 字段标签 12.5px / 提示 12px / 数字用 `tabular-nums`。
- 间距：组件内 4–8px，组内 10px，区块间 14–20px；**label 与控件的距离必须小于控件与下一字段的距离**。
- 页面级节奏走 8px 倍数；卡片内不嵌套第二套卡片样式（用 layer 而非再套一层边框）。

## 3. 控件层（2026-10-16 统一，写在 styles.css 顶部）

- 默认控件（`input`/`select`/`textarea`）：34px 高、8px 圆角、1px `--line-strong`、hover 提线、focus `--ring`。
  新代码**不要**再抄内联尺寸；要覆盖就用内联（例如故意的就地编辑无边框输入）。
- 字段解剖：`.field`（label 在上）+ 控件 + `.field-hint`（说明在下）。错误态用 `.field-error` 文案，不用红色边框单独承担语义。
- 按钮层级：`button.primary`（主操作，一屏一个）> 默认（次要）> `button.danger`（破坏性，文案要写清对象与后果）。
- 图标选择器：`<IconSelect>`（`src/IconSelect.tsx`）——**图形预览 + 中文名下拉**，值为白名单内名。
  白名单与中文名在 `src/announceIcons.ts`，与后端 `STEP_ICON_WHITELIST`、展示端 `st_page.html` 三处同口径。

## 4. 硬规则（用户口径 + 无障碍）

1. **界面上不出现内部标识符**：`campaign`、`emoji_events`、`tone`、`uk_wx_push_user_uniq` 这类内名只留在数据层；
   对运营一律给中文名 + 图形（用户点单原话：「这些去除」）。
2. Material Symbols 的连字文本不算「可见文本」——但**必须引字体**（`index.html` 的 Google Fonts 链接），否则会退化成生字。
3. 一个页面只有一个主操作；危险操作（删除 / 合并 / 批量）必须写明对象，二次确认不用同一个「确定」。
4. 空态写「现在是什么状态 + 最合理的下一步」（例：「还没有公告条，点下面『+ 添加公告条』新建一条」）。
5. 加载中不平铺骨架、不清空已有内容；失败给重试入口与原因，不给「出错了」。
6. 状态变化用文本 + 图标，不只靠颜色。

## 5. 响应式断点

| 宽度 | 导航 | 内容 |
|---|---|---|
| >900px | 216px 左侧栏（图标 + 标签） | 网格按 `auto-fit minmax(240/160px,1fr)` 自适应列数 |
| 560–900px | 60px 图标窄栏 | 同上；表格横向滚动（`.table-scroll`） |
| ≤560px | 顶部横向可滚动导航条 | 单列；字段与卡片满宽；保存栏满宽主按钮 |

断点是**重新决定导航形态与列数**，不是等比缩小；任何页面在该宽度下不得出现横向溢出（`scrollWidth === innerWidth`）。

## 6. 状态与反馈

`hover` / `active` / `selected` / `focus-visible` / `disabled` / `read-only` 六种状态必须可区分，
不得用同一个深色底糊过去。`disabled` 要能查到原因与解锁路径；`read-only` 不能低对比成灰。

## 7. 已知待清理（Backlog，按影响排序）

1. `--hairline` 历史名已从代码里清掉（改 `--line`）；若再有新代码写它，视为违规。
2. 各页面仍散落大量内联样式（尤其 StreamerPage / BotPage / ImportPage），未收敛到 `.field` / 控件层。
   **已做**（2026-10-16，BotPage / ImportPage / ClaimPage 三页）：内联 `style` 从 40 处降到 5 处
   （只剩 width / maxWidth / flex / minWidth 这类纯布局），输入项一律 `.field`（label 在上）
   + 控件 + `.field-hint`（说明在下），placeholder 只留示例值；`/admin/import` 预览弹窗的
   团别 / 年份 / 月份 / 业务日期 / 模式也换成 `.field`。顺带补齐规范层（都在本文件 §3 的写法内）：
   `.field` 之前是 `display:block`，label 文字与控件都是 inline 级 → 会挤在同一行
   （「标签 [输入框]」并排），改成列向 flex 才是文档说的「label 在上」；新增 `.field-list`、
   `.field-head`、`.field-ctl`、`.field > input[type=file]`、`.row-top` / `.row-end`、
   `.row > .alert`、`.alert > button`、`.qr-box img`、`.shot-pop` / `.shot-actions`、`.nowrap`；
   删掉 `.row input/select/textarea` 里与默认控件层逐字重复的一整段（视觉不变）。
   **未做**：StreamerPage（以及 SendPage / VisitPage / LineagePage / DashboardPage 的零散内联样式）。
   （已知同类小尾巴：BotPage 表格里挂在 `td.hint` 上的下拉框会被 12.5px 提示字号缩小一档 ——
   已在本轮修掉 BotPage 的可看团别列；StreamerPage 里还有一处 28px 高的同类 select。）
3. 表格：**已做**（2026-10-16）粘性表头 + 容器内滚动、数字列 `.num` 右对齐与等宽数字、行 hover、
   表内紧凑按钮（`table button` 4px 10px）、操作列 `.row-actions` 单行靠右、空表 `.empty-cell` 占位
   —— 主播台账行高由 110px 降到 49~66px，一屏从 4 行到 9 行。**已做**（2026-10-16 续）长文本列宽策略 `.cell-clip`（单行省略 + title 留全文）、客户端分页
   `src/Pager.tsx`（`usePager(total, resetKey)` + `<Pager/>`：每页 20/50/100、页码与条数写进 URL、
   筛选变化回第 1 页、越界自动落最后一页；主播台账已接入，104 行 → 每页 50）。
   **未做**：访客日志等其余长列表接入分页；虚拟滚动（上千行才需要）。
4. 危险操作确认文案与对象未逐个审（批量删除 / 合并 / 解绑）。
5. 保存反馈只有「当前版本 xxx」，缺「保存中 / 已保存 / 失败」的持续状态。
6. 族谱画布（`lineage.css`）未纳入本次 token 校对。
7. 窄屏横向溢出：`.page` 是 grid，子项默认 `min-width: auto` = min-content，卡里
   `.table-scroll table { min-width: 640px }` 的 640px 会沿 min-content 一路顶宽 grid 轨道
   —— 实测 390px 视口下 `/admin/import` 溢出 318px、`/admin/bots` 溢出 377px、
   `/admin/streamers` 溢出 678px（§5 要求任何宽度都不得溢出，属于硬规矩破了）。
   **已做**（2026-10-16）：`.page > * { min-width: 0 }`；五个页面在 390 / 768 / 1280 三档
   实测 `scrollWidth === innerWidth`，表格仍在自己的 `.table-scroll` 里横向滚动（断点行为未动）。
