import { useMemo, useState } from 'react';
import { DEFAULT_EXCLUDE, DEFAULT_ROSTER, parseRoster, formatWave, type ParsedRow, type Player } from '@pk/shared';
import { useStore } from '../store.js';
import { api } from '../api.js';
import { Button, Field, Panel, Pill, Tag, inputClass } from './ui.js';
import { AlertIcon, BanIcon, CheckBadge, ChevronLeftIcon, EditIcon, SearchIcon, SwapIcon } from './icons.js';

export function RosterImport() {
  const text = useStore((s) => s.text);
  const patch = useStore((s) => s.patch);
  const setView = useStore((s) => s.setView);
  const players = useStore((s) => s.players);
  const excluded = useStore((s) => s.excluded);
  const aliasMap = useStore((s) => s.aliasMap);
  const parse = useStore((s) => s.parse);
  const rosterSaving = useStore((s) => s.rosterSaving);
  const rosterSavedAt = useStore((s) => s.rosterSavedAt);

  const rows = useMemo(() => parseRoster(text, { aliases: aliasMap, exclude: DEFAULT_EXCLUDE }), [text, aliasMap]);
  const activeRows = rows.filter((r) => !r.excluded);
  const lineCount = rows.length;

  const hitCount = players.filter((p) => p.matched).length;
  const missCount = players.filter((p) => !p.matched).length;
  const aliasCount = Object.keys(aliasMap).length;

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2.5">
          <span className="flex h-7 w-7 items-center justify-center rounded-ctl bg-brand-soft text-brand">
            <EditIcon size={16} />
          </span>
          <h2 className="text-[20px] font-semibold leading-7 text-ink">名单导入与匹配预览</h2>
        </div>
        <div className="flex gap-2">
          <Button variant="primary" onClick={() => void parse(false)}>
            重新解析
          </Button>
          <Button icon={<ChevronLeftIcon size={14} />} onClick={() => setView('board')}>
            返回看板
          </Button>
        </div>
      </div>

      <div className="flex flex-wrap items-start gap-4">
        <Panel className="w-[620px] shrink-0 p-4">
          <div className="flex flex-col gap-3">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <span className="text-[15px] font-semibold text-ink">名单文本</span>
                <span className="text-[12px] text-ink3">{lineCount} 行 · 接龙格式</span>
                <RosterSaved saving={rosterSaving} savedAt={rosterSavedAt} />
              </div>
              <Button size="sm" variant="subtle" onClick={() => patch({ text: DEFAULT_ROSTER })}>
                恢复默认
              </Button>
            </div>
            <textarea
              className="h-[520px] w-full resize-none rounded-ctl border border-line bg-canvas px-3.5 py-3 text-[13px] leading-6 text-ink outline-none focus:border-brand"
              value={text}
              onChange={(e) => patch({ text: e.target.value })}
              spellCheck={false}
            />
            <p className="flex items-center gap-1.5 text-[12px] text-ink3">
              每行第一个名字 = 参赛主播，其余名字 = 队长 / 师傅（不参赛）
            </p>
          </div>
        </Panel>

        <div className="flex min-w-[360px] flex-1 flex-col gap-3">
          <div className="flex flex-wrap gap-2">
            <Pill tone="good">
              <span className="h-1.5 w-1.5 rounded-full bg-good" />
              {hitCount} 人命中
            </Pill>
            {missCount > 0 && (
              <Pill tone="bad">
                <span className="h-1.5 w-1.5 rounded-full bg-bad" />
                {missCount} 人待确认
              </Pill>
            )}
            {excluded.length > 0 && <Pill tone="neutral">{excluded.length} 人排除</Pill>}
            {aliasCount > 0 && <Pill tone="info">{aliasCount} 项别名映射</Pill>}
          </div>

          <Panel className="p-1">
            <div className="max-h-[420px] overflow-auto">
              {activeRows.map((row, i) => (
                <MatchRow key={`${row.raw}-${i}`} row={row} player={players[i]} />
              ))}
              {excluded.map((row) => (
                <ExcludedRow key={row.raw} row={row} />
              ))}
              {rows.length === 0 && (
                <div className="px-3 py-10 text-center text-[13px] text-ink3">粘贴名单后显示解析结果</div>
              )}
            </div>
          </Panel>

          <Panel className="flex flex-col gap-2.5 p-4">
            <span className="text-[15px] font-semibold text-ink">匹配规则</span>
            <Rule dot="bg-brand" text="精确匹配：名字与主播库 person.name 完全一致" />
            <Rule dot="bg-brand" text="归一化：去空格 / 全角半角 / 繁简转换后再匹配" />
            <Rule dot="bg-brand" text="别名映射：接龙别名（如 阿楠 → 南方楠）自动转换" />
            <Rule dot="bg-bad" text="未命中：标红并给出候选，可绑定或手动填音浪（未绑定按 0 参与分组）" />
          </Panel>
        </div>
      </div>
    </div>
  );
}

function Rule({ dot, text }: { dot: string; text: string }) {
  return (
    <div className="flex items-start gap-2">
      <span className={`mt-[7px] h-1.5 w-1.5 shrink-0 rounded-full ${dot}`} />
      <span className="text-[13px] leading-5 text-ink2">{text}</span>
    </div>
  );
}

/** 名单自动落库的状态提示。空 / 全空白时后端拒收，这时明说「不会保存」，
 * 免得用户以为清空之后还能靠自动保存找回来。 */
function RosterSaved({ saving, savedAt }: { saving: boolean; savedAt: string | null }) {
  const text = useStore((s) => s.text);
  const empty = text.trim().length === 0;
  if (empty) return <span className="text-[12px] text-bad">空白名单不会保存</span>;
  if (saving) return <span className="text-[12px] text-ink3">保存中…</span>;
  if (!savedAt) return null;
  return <span className="text-[12px] text-good">已保存 {new Date(savedAt).toLocaleTimeString('zh-CN')}</span>;
}

function RowShell({ children, onClick }: { children: React.ReactNode; onClick?: () => void }) {
  return (
    <div
      className={`flex h-11 items-center gap-2 px-3 ${onClick ? 'cursor-pointer hover:bg-canvas' : ''}`}
      onClick={onClick}
    >
      {children}
    </div>
  );
}

function MatchRow({ row, player }: { row: ParsedRow; player?: Player }) {
  const [open, setOpen] = useState(false);
  const wave = player?.wave ?? 0;
  const matched = player?.matched ?? false;
  const aliased = Boolean(row.aliasApplied);

  if (!matched && player) {
    return (
      <>
        <RowShell onClick={() => setOpen((v) => !v)}>
          <span className="text-bad">
            <AlertIcon size={16} />
          </span>
          <span className="text-[13px] font-medium text-ink">{row.name}</span>
          <Tag tone="bad">未匹配</Tag>
          <span className="flex-1" />
          <span className="nums text-[13px] font-semibold text-ink3">{formatWave(wave)}</span>
        </RowShell>
        {open && <UnmatchedPanel row={row} onDone={() => setOpen(false)} />}
      </>
    );
  }

  return (
    <RowShell>
      <CheckBadge />
      <span className="text-[13px] font-medium text-ink">{player?.name ?? row.name}</span>
      {aliased && (
        <>
          <span className="text-brand">
            <SwapIcon size={14} />
          </span>
          <span className="text-[13px] font-medium text-brand2">{row.aliasApplied}</span>
          <Tag tone="info">别名</Tag>
        </>
      )}
      {row.leader && <span className="text-[12px] text-ink3">师傅 {row.leader}</span>}
      <span className="flex-1" />
      <span className="nums text-[13px] font-semibold text-ink2">{formatWave(wave)}</span>
    </RowShell>
  );
}

function ExcludedRow({ row }: { row: ParsedRow }) {
  return (
    <RowShell>
      <span className="text-ink3">
        <BanIcon size={16} />
      </span>
      <span className="text-[13px] font-medium text-ink2">{row.name}</span>
      <Tag tone="neutral">已排除 · 不参与分组</Tag>
      <span className="flex-1" />
      <span className="nums text-[13px] font-semibold text-ink3">—</span>
    </RowShell>
  );
}

function UnmatchedPanel({ row, onDone }: { row: ParsedRow; onDone: () => void }) {
  const unmatched = useStore((s) => s.unmatched);
  const bind = useStore((s) => s.bind);
  const setManualWave = useStore((s) => s.setManualWave);
  const [q, setQ] = useState('');
  const [list, setList] = useState(() => unmatched.find((u) => u.row.name === row.name)?.candidates ?? []);
  const [waveInput, setWaveInput] = useState('');

  const search = async (kw: string) => {
    setQ(kw);
    const r = await api.anchors(kw);
    setList(r.items);
  };

  return (
    <div className="flex flex-col gap-2 border-y border-line bg-canvas px-3 py-3">
      <div className="flex items-center gap-2">
        <div className="relative">
          <span className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-ink3">
            <SearchIcon size={14} />
          </span>
          <input
            className={`${inputClass} w-[220px] pl-7`}
            placeholder="搜索主播并绑定"
            value={q}
            onChange={(e) => void search(e.target.value)}
          />
        </div>
        <span className="text-[12px] text-ink3">或</span>
        <Field label="手动填音浪（万）" className="w-[150px]">
          <input
            className={`${inputClass} w-full nums`}
            placeholder="如 12.5"
            value={waveInput}
            onChange={(e) => setWaveInput(e.target.value)}
          />
        </Field>
        <Button
          size="sm"
          onClick={() => {
            const w = Math.round(Number(waveInput || 0) * 10000);
            setManualWave(row.name, w);
            onDone();
          }}
        >
          确定
        </Button>
      </div>

      <div className="max-h-[160px] overflow-auto rounded-card border border-line bg-surface">
        {list.length === 0 && <div className="px-3 py-2 text-[12px] text-ink3">无候选，可搜索或手动填音浪</div>}
        {list.map((a) => (
          <div
            key={a.personId}
            className="cursor-pointer px-3 py-1.5 text-[13px] text-ink hover:bg-brand-soft"
            onClick={() => {
              void bind(row.name, a.personId, a.name);
              onDone();
            }}
          >
            {a.name}
          </div>
        ))}
      </div>
    </div>
  );
}
