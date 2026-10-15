import type { CSSProperties } from 'react'
import { ANNOUNCE_ICONS, iconText } from './announceIcons'

/** 图标选择器（2026-10-16）：**图形预览 + 中文名下拉**，替代原来的自由文本输入。
 *
 *  原来公告位的图标是 `<input placeholder="如「喇叭公告」…">`，运营看到的是回显的
 *  `campaign` / `person` 这类内部标识符（用户点单：这些要去掉）。现在：
 *    · 值仍是白名单内名（数据层、后端契约一字不变）；
 *    · 界面上只见中文名与实际图形，手输不进白名单外的名字；
 *    · 空值统一表达为「不显示图标」，由各调用方按自己的默认图标回落。
 *  外观（尺寸 / 边框 / focus 环）走 `.icon-pick`，与全站控件同一套 token。 */
export default function IconSelect({
  value,
  onChange,
  emptyLabel = '不显示图标',
  style,
}: {
  value: string
  onChange: (v: string) => void
  emptyLabel?: string
  style?: CSSProperties
}) {
  const known = ANNOUNCE_ICONS.includes(value) ? value : ''
  return (
    <span className="icon-pick" style={style}>
      <span
        className="icon-pick-glyph material-symbols-outlined"
        aria-hidden="true"
        title={known ? iconText(known) : emptyLabel}
      >
        {known || 'block'}
      </span>
      <select value={known} onChange={(e) => onChange(e.target.value)}>
        <option value="">{emptyLabel}</option>
        {ANNOUNCE_ICONS.map((n) => (
          <option key={n} value={n}>
            {iconText(n)}
          </option>
        ))}
      </select>
    </span>
  )
}
