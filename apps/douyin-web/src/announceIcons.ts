/** 公告图标表（2026-10-16 从 AnnouncePage 抽出来共用）：
 *
 *  - 白名单与后端 `pk-server/src/public_teams.rs` 的 `STEP_ICON_WHITELIST`、
 *    展示端 `st_page.html` 的 `ANNOUNCE_ICONS` 三处同口径，改一处要同步另两处；
 *  - 管理端界面上**只出现中文名**（`ICON_LABELS`），`campaign` / `emoji_events`
 *    这类内部标识符只在数据层流转、不落到运营眼前（用户点单：界面上不该看到内名）。
 */

/** 图标白名单（Material Symbols 内名，顺序即下拉顺序） */
export const ANNOUNCE_ICONS = [
  'auto_awesome', 'military_tech', 'qr_code_scanner', 'notifications_active',
  'insights', 'tips_and_updates', 'campaign', 'star', 'bolt', 'groups',
  'emoji_events', 'local_fire_department', 'trending_up', 'calendar_month',
  'chat_bubble', 'lock', 'person', 'smart_toy', 'workspace_premium',
]

/** 内名 → 运营看的中文名（缺表时回落到内名，不会渲染成空白） */
export const ICON_LABELS: Record<string, string> = {
  auto_awesome: '闪光',
  military_tech: '勋章',
  qr_code_scanner: '扫码',
  notifications_active: '铃铛',
  insights: '数据洞察',
  tips_and_updates: '提示灯泡',
  campaign: '喇叭公告',
  star: '星标',
  bolt: '闪电',
  groups: '群组',
  emoji_events: '奖杯',
  local_fire_department: '火焰',
  trending_up: '上升趋势',
  calendar_month: '日历',
  chat_bubble: '对话气泡',
  lock: '锁',
  person: '人形',
  smart_toy: '机器人',
  workspace_premium: '高级徽章',
}

/** 图标显示名：中文名优先（界面上永远不出现内名） */
export const iconText = (name: string): string => ICON_LABELS[name] ?? name
