import type { ButtonHTMLAttributes, ReactNode } from 'react';

/* ---------------------------------- Button --------------------------------- */

type Variant = 'primary' | 'dark' | 'ghost' | 'subtle';

const VARIANTS: Record<Variant, string> = {
  primary: 'bg-brand text-white hover:bg-brand2',
  dark: 'bg-ink text-white hover:opacity-90',
  ghost: 'bg-surface border border-line2 text-ink2 hover:bg-subtle',
  subtle: 'bg-subtle text-ink2 hover:brightness-95',
};

interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: 'sm' | 'md';
  icon?: ReactNode;
}

export function Button({ variant = 'ghost', size = 'md', icon, className = '', children, ...rest }: ButtonProps) {
  return (
    <button
      type="button"
      className={`inline-flex shrink-0 items-center justify-center gap-1.5 rounded-ctl font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-50 ${
        size === 'sm' ? 'h-7 px-2.5 text-[12px]' : 'h-8 px-3 text-[13px]'
      } ${VARIANTS[variant]} ${className}`}
      {...rest}
    >
      {icon}
      {children}
    </button>
  );
}

/* ----------------------------------- Tag ----------------------------------- */

type Tone = 'good' | 'warn' | 'bad' | 'info' | 'neutral';

const TONES: Record<Tone, string> = {
  good: 'bg-good-soft text-good-ink',
  warn: 'bg-warn-soft text-warn-ink',
  bad: 'bg-bad-soft text-bad',
  info: 'bg-brand-soft text-brand2',
  neutral: 'bg-subtle text-ink2',
};

export function Tag({ tone = 'neutral', children, className = '' }: { tone?: Tone; children: ReactNode; className?: string }) {
  return (
    <span className={`inline-flex items-center rounded-[4px] px-1.5 py-[3px] text-[11px] font-medium leading-none ${TONES[tone]} ${className}`}>
      {children}
    </span>
  );
}

/** 圆角胶囊，用于状态小标签（如「优秀」「已排除」） */
export function Pill({ tone = 'neutral', children }: { tone?: Tone; children: ReactNode }) {
  return (
    <span className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-[5px] text-[12px] font-medium leading-none ${TONES[tone]}`}>
      {children}
    </span>
  );
}

/* ---------------------------------- Field ---------------------------------- */

export function Field({ label, hint, children, className = '' }: { label: string; hint?: string; children: ReactNode; className?: string }) {
  return (
    <label className={`flex flex-col gap-1.5 ${className}`}>
      <span className="text-[12px] leading-none text-ink2">{label}</span>
      {children}
      {hint && <span className="text-[11px] leading-none text-ink3">{hint}</span>}
    </label>
  );
}

/* --------------------------------- Controls -------------------------------- */

export const inputClass =
  'h-8 rounded-ctl border border-line2 bg-surface px-2.5 text-[13px] text-ink outline-none transition-colors placeholder:text-ink3 focus:border-brand disabled:bg-subtle disabled:text-ink3';

export const selectClass = `${inputClass} cursor-pointer pr-2`;

/* -------------------------------- Segmented -------------------------------- */

export function Segmented<T extends string>({
  options,
  value,
  onChange,
}: {
  options: Array<{ value: T; label: string; title?: string }>;
  value: T;
  onChange: (v: T) => void;
}) {
  return (
    <div className="inline-flex rounded-ctl bg-subtle p-0.5">
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          title={o.title}
          onClick={() => onChange(o.value)}
          className={`h-7 rounded-[6px] px-3 text-[13px] leading-none transition-colors ${
            value === o.value ? 'bg-surface font-medium text-ink shadow-card' : 'text-ink2 hover:text-ink'
          }`}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

/* ---------------------------------- Toggle --------------------------------- */

/** iOS 风格开关：展示页模块显隐这类「开 / 关」配置用 */
export function Toggle({
  checked,
  onChange,
  label,
  title,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  title?: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      title={title}
      onClick={() => onChange(!checked)}
      className="flex items-center gap-2 text-[12.5px] text-ink2"
    >
      <span
        className={`relative inline-block h-[20px] w-[34px] shrink-0 rounded-full transition-colors ${
          checked ? 'bg-good' : 'bg-line2'
        }`}
      >
        <span
          className={`absolute top-[2px] h-[16px] w-[16px] rounded-full bg-white shadow-card transition-all ${
            checked ? 'left-[16px]' : 'left-[2px]'
          }`}
        />
      </span>
      <span className={checked ? 'text-ink' : ''}>{label}</span>
    </button>
  );
}

/* --------------------------------- Surfaces -------------------------------- */

export function Panel({ children, className = '' }: { children: ReactNode; className?: string }) {
  return <div className={`rounded-panel border border-line bg-surface ${className}`}>{children}</div>;
}

export function Modal({
  open,
  onClose,
  title,
  children,
  width = 520,
}: {
  open: boolean;
  onClose: () => void;
  title: string;
  children: ReactNode;
  width?: number;
}) {
  if (!open) return null;
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-ink/30 p-4" onClick={onClose}>
      <div
        className="max-h-[80vh] w-full overflow-hidden rounded-panel bg-surface shadow-pop"
        style={{ maxWidth: width }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between border-b border-line px-4 py-3">
          <span className="text-[15px] font-semibold text-ink">{title}</span>
          <button type="button" onClick={onClose} className="text-ink3 hover:text-ink2" aria-label="关闭">
            <span className="text-[18px] leading-none">&times;</span>
          </button>
        </div>
        <div className="max-h-[calc(80vh-52px)] overflow-auto">{children}</div>
      </div>
    </div>
  );
}

/* ---------------------------------- Toaster -------------------------------- */

export function Toaster({ message }: { message: string | null }) {
  if (!message) return null;
  return (
    <div className="pointer-events-none fixed bottom-6 left-1/2 z-[60] -translate-x-1/2">
      <div className="rounded-ctl bg-ink px-3.5 py-2 text-[13px] text-white shadow-pop">{message}</div>
    </div>
  );
}
