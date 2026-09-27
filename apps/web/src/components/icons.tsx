import type { ReactNode } from 'react';

interface IconProps {
  size?: number;
  className?: string;
}

interface StyledIconProps extends IconProps {
  children: ReactNode;
}

/** 统一 16 viewBox 线性图标：stroke 1.4、圆头圆角，保证与文字混排时重量一致 */
function Svg({ size = 16, className, children }: StyledIconProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      className={className}
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

const S = {
  stroke: '#000000',
  width: 1.4,
  linecap: 'round' as const,
  linejoin: 'round' as const,
};

export function LayersIcon(p: IconProps) {
  return (
    <Svg size={p.size ?? 28} className={p.className}>
      <rect x="2.2" y="2.2" width="4.6" height="4.6" rx="1.2" stroke="#ffffff" strokeWidth="1.6" />
      <rect x="9.2" y="2.2" width="4.6" height="4.6" rx="1.2" stroke="#ffffff" strokeWidth="1.6" opacity="0.55" />
      <rect x="2.2" y="9.2" width="4.6" height="4.6" rx="1.2" stroke="#ffffff" strokeWidth="1.6" opacity="0.55" />
      <rect x="9.2" y="9.2" width="4.6" height="4.6" rx="1.2" stroke="#ffffff" strokeWidth="1.6" />
    </Svg>
  );
}

export function EditIcon(p: IconProps) {
  return (
    <Svg {...p}>
      <rect x="2.8" y="2.2" width="10.4" height="11.6" rx="2" stroke="currentColor" strokeWidth={S.width} />
      <path d="M5.4 5.6h5.2M5.4 8h5.2M5.4 10.4h3" stroke="currentColor" strokeWidth={S.width} strokeLinecap={S.linecap} />
    </Svg>
  );
}

export function RefreshIcon(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M13.6 8a5.6 5.6 0 1 1-1.7-4" stroke="currentColor" strokeWidth={S.width} strokeLinecap={S.linecap} />
      <path d="M13.6 2.6v3.2h-3.2" stroke="currentColor" strokeWidth={S.width} strokeLinecap={S.linecap} strokeLinejoin={S.linejoin} />
    </Svg>
  );
}

export function PlusIcon(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M8 3.2v9.6M3.2 8h9.6" stroke="currentColor" strokeWidth={1.5} strokeLinecap={S.linecap} />
    </Svg>
  );
}

export function CopyIcon(p: IconProps) {
  return (
    <Svg {...p}>
      <rect x="5.6" y="5.6" width="8.2" height="8.2" rx="1.8" stroke="currentColor" strokeWidth={S.width} />
      <path
        d="M10.4 5.6V4.2A1.8 1.8 0 0 0 8.6 2.4H4.2A1.8 1.8 0 0 0 2.4 4.2v4.4a1.8 1.8 0 0 0 1.8 1.8h1.4"
        stroke="currentColor"
        strokeWidth={S.width}
      />
    </Svg>
  );
}

export function SaveIcon(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M3.2 2.4h6.2L12.8 5.8v7.8H3.2z" stroke="currentColor" strokeWidth={S.width} strokeLinejoin={S.linejoin} />
      <path d="M5.8 2.4v3.6h3.8V2.4" stroke="currentColor" strokeWidth={S.width} strokeLinejoin={S.linejoin} />
      <rect x="5.4" y="9" width="5.2" height="3" stroke="currentColor" strokeWidth={S.width} />
    </Svg>
  );
}

export function DownloadIcon(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M8 2.2v7.6" stroke="currentColor" strokeWidth={1.5} strokeLinecap={S.linecap} />
      <path d="M4.6 7L8 10.4 11.4 7" stroke="currentColor" strokeWidth={1.5} strokeLinecap={S.linecap} strokeLinejoin={S.linejoin} />
      <path d="M2.8 13.6h10.4" stroke="currentColor" strokeWidth={1.5} strokeLinecap={S.linecap} />
    </Svg>
  );
}

export function ChevronDownIcon(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M4 6.2L8 10l4-3.8" stroke="currentColor" strokeWidth={S.width} strokeLinecap={S.linecap} strokeLinejoin={S.linejoin} />
    </Svg>
  );
}

export function ChevronLeftIcon(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M9.8 3.4L5.2 8l4.6 4.6" stroke="currentColor" strokeWidth={S.width} strokeLinecap={S.linecap} strokeLinejoin={S.linejoin} />
    </Svg>
  );
}

export function HistoryIcon(p: IconProps) {
  return (
    <Svg {...p}>
      <circle cx="8" cy="8" r="6" stroke="currentColor" strokeWidth={S.width} />
      <path d="M8 4.6V8l2.4 1.4" stroke="currentColor" strokeWidth={S.width} strokeLinecap={S.linecap} strokeLinejoin={S.linejoin} />
    </Svg>
  );
}

export function SearchIcon(p: IconProps) {
  return (
    <Svg {...p}>
      <circle cx="7" cy="7" r="4.6" stroke="currentColor" strokeWidth={S.width} />
      <path d="M10.4 10.4l3 3" stroke="currentColor" strokeWidth={S.width} strokeLinecap={S.linecap} />
    </Svg>
  );
}

export function AlertIcon(p: IconProps) {
  return (
    <Svg {...p}>
      <circle cx="8" cy="8" r="6.2" stroke="currentColor" strokeWidth={S.width} />
      <path d="M8 5v4" stroke="currentColor" strokeWidth={S.width} strokeLinecap={S.linecap} />
      <circle cx="8" cy="11.4" r="0.9" fill="currentColor" />
    </Svg>
  );
}

export function SwapIcon(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M3.6 5.4h9.2l-2.4-2.4M12.4 10.6H3.2l2.4 2.4" stroke="currentColor" strokeWidth={S.width} strokeLinecap={S.linecap} strokeLinejoin={S.linejoin} />
    </Svg>
  );
}

export function BanIcon(p: IconProps) {
  return (
    <Svg {...p}>
      <circle cx="8" cy="8" r="6.2" stroke="currentColor" strokeWidth={S.width} />
      <path d="M3.8 3.8l8.4 8.4" stroke="currentColor" strokeWidth={S.width} strokeLinecap={S.linecap} />
    </Svg>
  );
}

/** 命中：实心绿圆 + 白勾 */
export function CheckBadge({ size = 16 }: IconProps) {
  return (
    <Svg size={size}>
      <circle cx="8" cy="8" r="6.6" fill="#12b76a" />
      <path d="M5 8.2l2.1 2.1L11.2 6" stroke="#ffffff" strokeWidth="1.5" strokeLinecap={S.linecap} strokeLinejoin={S.linejoin} />
    </Svg>
  );
}

export function TrashIcon(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M2.8 4.6h10.4M6.4 4.6V2.8h3.2v1.8M4.4 4.6l.7 8.6h5.8l.7-8.6" stroke="currentColor" strokeWidth={S.width} strokeLinecap={S.linecap} strokeLinejoin={S.linejoin} />
    </Svg>
  );
}

export function CloseIcon(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M4 4l8 8M12 4l-8 8" stroke="currentColor" strokeWidth={1.5} strokeLinecap={S.linecap} />
    </Svg>
  );
}

/** 固定位（钉住）：锁体 + 锁梁 */
export function PinIcon(p: IconProps) {
  return (
    <Svg {...p}>
      <rect x="3.4" y="7" width="9.2" height="6.4" rx="1.6" stroke="currentColor" strokeWidth={S.width} />
      <path d="M5.8 7V5.4a2.2 2.2 0 014.4 0V7" stroke="currentColor" strokeWidth={S.width} strokeLinecap={S.linecap} />
    </Svg>
  );
}

/** 开赛时间：时钟 */
export function ClockIcon(p: IconProps) {
  return (
    <Svg {...p}>
      <circle cx="8" cy="8" r="6.2" stroke="currentColor" strokeWidth={S.width} />
      <path d="M8 4.8V8l2.4 1.6" stroke="currentColor" strokeWidth={S.width} strokeLinecap={S.linecap} strokeLinejoin={S.linejoin} />
    </Svg>
  );
}
