/**
 * 长图导出：DOM → SVG foreignObject → Canvas → PNG Blob。
 *
 * 两个坑（Chrome 实测，别再踩）：
 * 1. SVG 不编码直接拼 `data:` URL，样式里的 `#` 颜色会被当成 URL 片段截断 → 图片解码失败
 * 2. 换成 blob: URL 能解码，但画布会被判为「被污染」，toBlob 抛 SecurityError
 * 所以是 encodeURIComponent 的 data URL + 自己画 canvas。
 *
 * 另一个前提：foreignObject 只认内联样式，外部 CSS 带不进去。
 * 所以这里不克隆页面节点，而是用 buildExportNode() 现拼一段纯内联样式的 HTML，
 * 字体名一律写单引号（裸双引号会把 style="..." 提前截断，产出非法 XML）。
 */

export interface ExportOptions {
  /** 输出倍率，2 = Retina */
  scale?: number;
  backgroundColor?: string;
}

function nextFrames(n = 2): Promise<void> {
  return new Promise((resolve) => {
    let left = n;
    const step = () => (--left <= 0 ? resolve() : requestAnimationFrame(step));
    requestAnimationFrame(step);
  });
}

export async function nodeToPngBlob(node: HTMLElement, options: ExportOptions = {}): Promise<Blob> {
  const { scale = 2, backgroundColor = '#ffffff' } = options;

  await nextFrames();

  const rect = node.getBoundingClientRect();
  const width = Math.ceil(rect.width) || node.offsetWidth;
  const height = Math.ceil(rect.height) || node.offsetHeight;
  if (!width || !height) throw new Error(`导出节点尺寸异常：${width}×${height}`);

  const wrapper = document.createElement('div');
  wrapper.setAttribute('xmlns', 'http://www.w3.org/1999/xhtml');
  wrapper.style.cssText = `margin:0;padding:0;width:${width}px;background:${backgroundColor};`;
  wrapper.appendChild(node.cloneNode(true));

  const serialized = new XMLSerializer().serializeToString(wrapper);
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}">` +
    `<foreignObject x="0" y="0" width="${width}" height="${height}">${serialized}</foreignObject></svg>`;

  const encoded = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
  try {
    const img = new Image();
    img.src = encoded;
    try {
      await img.decode();
    } catch (e) {
      throw new Error(`SVG 无法解码（${width}×${height}）：${(e as Error).message}`);
    }

    const canvas = document.createElement('canvas');
    canvas.width = Math.round(width * scale);
    canvas.height = Math.round(height * scale);
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('无法获取 canvas 2d 上下文');
    ctx.fillStyle = backgroundColor;
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    ctx.drawImage(img, 0, 0, canvas.width, canvas.height);

    return await new Promise<Blob>((resolve, reject) => {
      canvas.toBlob((b) => (b ? resolve(b) : reject(new Error('canvas 转 PNG 失败'))), 'image/png');
    });
  } catch (e) {
    if (e instanceof DOMException && e.name === 'SecurityError') {
      throw new Error('画布被跨域内容污染，导出中止');
    }
    throw e;
  }
}

export function downloadBlob(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

/* ------------------------------ 长图内容 ------------------------------ */

export interface ExportGroup {
  name: string;
  slot: string;
  players: Array< { name: string; leader?: string | null; wave: number } >;
}

export interface ExportInput {
  /** 头部大标题（方案名 / 品牌名） */
  title: string;
  /** 头部顶部小字（缺省 PK GROUP · 日期） */
  brandLine?: string;
  /** 头部信息行：赛事 · 共 N 人 · 开赛时间 / 间隔 */
  metaLine?: string;
  groups: ExportGroup[];
  /** 隐藏音浪数值时只出名次与名单 */
  hideWave?: boolean;
  /** 成员统计行里的口径名（如「近 3 天音浪」） */
  metricLabel?: string;
  /** 底部赛制说明（内置规则，如初级赛四轮晋级/复活规则） */
  rules?: { title: string; items: string[] };
  /** 底部落款 */
  footer?: string;
}

const esc = (s: string) =>
  String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]!));

/** 参考图风格：90.3万 这种一位小数 */
const wanDec = (n: number) => {
  n = Math.round(n || 0);
  if (n < 10000) return String(n);
  return (n / 10000).toFixed(1).replace(/\.0$/, '') + '万';
};

/** 竞赛排名：并列同名次，下一位跳号（1,1,3） */
function rankOf(wave: number, sortedDesc: number[]): number {
  const at = sortedDesc.findIndex((w) => w <= wave);
  return at < 0 ? sortedDesc.length : at + 1;
}

/** 分组卡配色（对照参考图 8 色循环：粉/紫/青/橙/绿/红/蓝/青绿） */
const THEMES = [
  { bg: '#fdeaf1', bd: '#f6a8c5', tc: '#e04f8a' },
  { bg: '#f0ecfd', bd: '#b3a1f2', tc: '#7a58e0' },
  { bg: '#e4f7fb', bd: '#7fd0e2', tc: '#189db8' },
  { bg: '#fdf3dd', bd: '#f4c46a', tc: '#dd8f1d' },
  { bg: '#e7f7ec', bd: '#82cb9b', tc: '#2f9e5d' },
  { bg: '#fdebe9', bd: '#f19a97', tc: '#dd4f4c' },
  { bg: '#e9eefc', bd: '#98aaee', tc: '#4c63d6' },
  { bg: '#e6f6f1', bd: '#74c7ab', tc: '#1c9b77' },
];

/** 拼一段纯内联样式的长图 DOM，宽 1024，挂到离屏容器后交给 nodeToPngBlob。
 *  版式对照参考图：渐变头部卡（品牌 + 场次信息 + N组/N人 胶囊）
 *  → 两列彩色分组卡（组员两列胶囊，含组内/全场名次统计行）→ 底部落款。 */
export function buildExportNode(input: ExportInput): HTMLElement {
  const totalPlayers = input.groups.reduce((s, g) => s + g.players.length, 0);
  const allWaves = input.groups
    .flatMap((g) => g.players.map((p) => p.wave || 0))
    .sort((a, b) => b - a);
  const statLine = (wave: number, groupRank: number) => {
    const parts = [`第${groupRank}位 · 全场第${rankOf(wave, allWaves)}位`];
    if (!input.hideWave) parts.push(`${input.metricLabel || '音浪'} ${wanDec(wave)}`);
    return parts.join(' · ');
  };

  const cards = input.groups
    .map((g, gi) => {
      const t = THEMES[gi % THEMES.length];
      const sorted = g.players.map((p) => p.wave || 0).sort((a, b) => b - a);
      // 两列成员胶囊：奇数个时最后一格补一个占位，保持左右对称
      const chips = g.players.map((p, i) => {
        const wave = p.wave || 0;
        const groupRank = rankOf(wave, sorted);
        return `<div style="background:rgba(255,255,255,.8);border-radius:10px;padding:8px 10px">
          <div style="font-size:14px;line-height:20px;color:#1f2937">
            <span style="color:${t.tc};font-weight:800;margin-right:6px">${i + 1}</span>
            <b style="font-weight:700">${esc(p.name)}</b>
          </div>
          <div style="margin-top:3px;font-size:11px;line-height:16px;color:#8590a2">${esc(statLine(wave, groupRank))}</div>
        </div>`;
      });
      if (chips.length % 2 === 1) chips.push('<div></div>');
      return `<div style="break-inside:avoid;margin-bottom:14px;background:${t.bg};border:2px solid ${t.bd};border-radius:16px;padding:14px 14px 12px">
        <div style="font-size:17px;font-weight:800;color:${t.tc}">${esc(g.name)}</div>
        <div style="margin-top:2px;font-size:12px;color:#7b8494">${esc(g.slot ? g.slot + ' 开赛' : '')}${g.players.length ? ' · ' + g.players.length + ' 人' : ''}</div>
        <div style="margin-top:10px;display:grid;grid-template-columns:1fr 1fr;gap:8px">${chips.join('') || '<div style="grid-column:1/3;color:#98a2b3;font-size:13px">空组</div>'}</div>
      </div>`;
    })
    .join('');

  const pill = (num: string, label: string, solid: boolean) =>
    `<div style="min-width:64px;text-align:center;border-radius:16px;padding:9px 16px;${solid ? 'background:#fff;color:#f4573d' : 'background:rgba(255,255,255,.28);color:#fff'}">
      <div style="font-size:22px;font-weight:800;line-height:1.1">${esc(num)}</div>
      <div style="font-size:10px;opacity:.85;margin-top:1px">${esc(label)}</div>
    </div>`;

  /* 赛制说明：内置规则整块打在标题下面、分组卡上面（用户 2026-10-07 要求放头部，
     先看规则再看分组）。不用 <ol>/<li>——foreignObject 里列表符号渲染不稳，
     改成手写序号的 div。 */
  const ruleBlock =
    input.rules && input.rules.items.length
      ? `<div style="margin-top:14px;border:2px solid #ffd9c9;background:#fff7f3;border-radius:16px;padding:14px 16px">
          <div style="font-size:15px;font-weight:800;color:#f4573d">${esc(input.rules.title)}</div>
          <div style="margin-top:9px;display:grid;gap:6px">
            ${input.rules.items
              .map(
                (t, i) =>
                  `<div style="font-size:12.5px;line-height:19px;color:#5b6472">
                    <span style="color:#f4573d;font-weight:800;margin-right:6px">${i + 1}</span>${esc(t)}
                  </div>`,
              )
              .join('')}
          </div>
        </div>`
      : '';

  /* ⚠️ box-sizing 必须内联：导出是把这段 HTML 序列化进 foreignObject，
    页面 styles.css 里的全局 `*{box-sizing:border-box}` 带不进去。不写这一行，
    1024 宽再叠加左右各 26 的内边距 = 1076，而画布按 1024 截 —— 右边缘会被裁掉
    约 52px（2026-10-18 实测：长图右侧规则文案、右侧分组卡边缘都被切）。 */
  const html = `<div style="box-sizing:border-box;width:1024px;padding:24px 26px 18px;background:#fff;font-family:-apple-system,'PingFang SC','Microsoft YaHei',sans-serif;color:#101828">
      <div style="display:flex;align-items:center;justify-content:space-between;background:linear-gradient(120deg,#ff9a3d 0%,#ff6a5e 55%,#ff4d94 100%);border-radius:20px;padding:20px 24px;color:#fff">
        <div style="min-width:0">
          <div style="font-size:11px;letter-spacing:2px;opacity:.85">${esc(input.brandLine || 'PK GROUP')}</div>
          <div style="margin-top:6px;font-size:28px;font-weight:800;letter-spacing:.02em">${esc(input.title)}</div>
          ${input.metaLine ? `<div style="margin-top:8px;font-size:12.5px;opacity:.95">${esc(input.metaLine)}</div>` : ''}
        </div>
        <div style="display:flex;gap:10px;flex:none;margin-left:16px">${pill(String(input.groups.length), '组', false)}${pill(String(totalPlayers), '人', true)}</div>
      </div>
      ${ruleBlock}
      <div style="column-count:2;column-gap:14px;margin-top:16px">${cards}</div>
      <div style="margin-top:10px;text-align:center;color:#98a2b3;font-size:11px">${esc(input.footer || '内部数据 · 请勿外传')}</div>
    </div>`;

  const stage = document.createElement('div');
  stage.className = 'export-stage';
  stage.style.cssText = `position:fixed;left:-10000px;top:0;width:1024px;background:#fff;z-index:-1;opacity:0;pointer-events:none;`;
  stage.innerHTML = html;
  document.body.appendChild(stage);
  return stage.firstElementChild as HTMLElement;
}

export function disposeExportNode(node: HTMLElement): void {
  node.parentElement?.remove();
}

/* --------------------------- 总排名分差图 --------------------------- */

/** 分差图里的一个参赛者：来自哪一组 + 音浪（或名单自带分数）。 */
export interface GapPlayerInput {
  name: string;
  /** 组名（「第 3 组」），印章颜色按 groupIndex 循环取 */
  groupName: string;
  /** 组序号（0 起），决定组别印章配色，与分组长图同一套 THEMES */
  groupIndex: number;
  wave: number;
}

export interface GapExportInput {
  /** 头部大标题（方案名 / 品牌名） */
  title: string;
  /** 头部顶部小字（缺省 PK GROUP · 总排名分差） */
  brandLine?: string;
  /** 头部信息行：赛事 · 口径 · 共 N 组 N 人 */
  metaLine?: string;
  /** 全部参赛者（不要求有序，内部按 wave 从高到低排总排名） */
  players: GapPlayerInput[];
  /** 数值口径名（如「近 3 天音浪」/「名单分数」） */
  metricLabel?: string;
  /** 底部落款 */
  footer?: string;
}

/** 前三名奖牌配色：金 / 银 / 铜（与榜单图口径一致，颜色别乱改） */
const RANK_THEMES = [
  { bg: 'linear-gradient(135deg,#ffd873,#f0a51e)', fg: '#6d4300' },
  { bg: 'linear-gradient(135deg,#e8edf4,#b7c2d0)', fg: '#39465a' },
  { bg: 'linear-gradient(135deg,#f3c9a4,#c9834a)', fg: '#5a2e0c' },
];

/** 固定档位「门槛」：每行标出离前10 / 前5 / 前三 / 前2 / 前1 还差多少分。
 *  按**顺序位**取门槛（第 k 位那个人的分数），并列时门槛 = 该位置的分数。 */
const MILESTONES = [
  { label: '前10', k: 10 },
  { label: '前5', k: 5 },
  { label: '前三', k: 3 },
  { label: '前2', k: 2 },
  { label: '前1', k: 1 },
] as const;

/** 拼一张「总排名 + 相邻名次分差 + 前 K 门槛差」的竖图，宽 1024，挂到离屏容器后交给 nodeToPngBlob。
 *  版式：渐变头部卡（品牌 + 标题 + N组/N人）→ 分差概览 + 名次门槛 + 各组名次分布
 *  → 两列总排名行（名次奖牌 + 姓名 + 组别印章 + 音浪条 + 距上一位差多少 + 距前10/前5/前三/前2/前1 差多少）→ 落款。
 *  与分组长图分开：这张图只看「所有人在全场总榜的位次和互相差多少分」。 */
export function buildGapExportNode(input: GapExportInput): HTMLElement {
  const players = [...input.players].sort((a, b) => (b.wave || 0) - (a.wave || 0));
  const total = players.length;
  const maxWave = players.reduce((m, p) => Math.max(m, p.wave || 0), 0) || 1;
  const metricLabel = input.metricLabel || '音浪';

  /* 总排名（并列同名次，下一位跳号）与「距上一位差多少分」 */
  const ranks: number[] = [];
  const gaps: number[] = [];
  players.forEach((p, i) => {
    const wave = p.wave || 0;
    const prev = i > 0 ? players[i - 1].wave || 0 : wave;
    ranks.push(i > 0 && wave === prev ? ranks[i - 1] : i + 1);
    gaps.push(i === 0 ? 0 : prev - wave);
  });

  /* 各组名次分布：一眼看出每个组的人在总榜占了哪一段 */
  const legend = new Map<number, { name: string; best: number; worst: number; count: number }>();
  players.forEach((p, i) => {
    const cur = legend.get(p.groupIndex);
    if (!cur) legend.set(p.groupIndex, { name: p.groupName, best: ranks[i], worst: ranks[i], count: 1 });
    else {
      cur.best = Math.min(cur.best, ranks[i]);
      cur.worst = Math.max(cur.worst, ranks[i]);
      cur.count += 1;
    }
  });
  const legendRows = [...legend.entries()]
    .sort((a, b) => a[0] - b[0])
    .map(([gi, g]) => {
      const t = THEMES[gi % THEMES.length];
      const range = g.best === g.worst ? `第${g.best}名` : `第${g.best}–${g.worst}名`;
      return `<div style="display:flex;align-items:center;gap:5px;min-width:0;font-size:11px;color:#475467">
        <span style="width:8px;height:8px;border-radius:3px;flex:none;background:${t.tc}"></span>
        <b style="flex:none;color:${t.tc}">${esc(g.name)}</b>
        <span style="color:#98a2b3;white-space:nowrap">${esc(range)} · ${g.count}人</span>
      </div>`;
    })
    .join('');

  /* 分差概览：最大断层 / 平均分差 / 并列人数 */
  const nonZeroGaps = gaps.filter((g) => g > 0);
  const maxGap = nonZeroGaps.length ? Math.max(...nonZeroGaps) : 0;
  const maxGapAt = gaps.indexOf(maxGap);
  const avgGap = nonZeroGaps.length ? nonZeroGaps.reduce((s, g) => s + g, 0) / nonZeroGaps.length : 0;
  /* 并列人数 = 落在并列名次里的总人数（含并列的第一人），不是「多出来的人」 */
  const tieCount = (() => {
    const byWave = new Map<number, number>();
    players.forEach((p) => byWave.set(p.wave || 0, (byWave.get(p.wave || 0) || 0) + 1));
    return [...byWave.values()].filter((c) => c > 1).reduce((s, c) => s + c, 0);
  })();
  const maxGapText =
    maxGap > 0 && maxGapAt > 0 ? `第${ranks[maxGapAt - 1]}→${ranks[maxGapAt]}名 差${wanDec(maxGap)}` : '—';
  const stat = (value: string, label: string) =>
    `<div style="min-width:0">
      <div style="font-size:15px;font-weight:800;color:#101828;line-height:1.2;white-space:nowrap">${esc(value)}</div>
      <div style="margin-top:3px;font-size:10px;color:#98a2b3">${esc(label)}</div>
    </div>`;

  /* 各档门槛分：第 k 位那个人的分数（并列时就是该位置的分数）；人数不够的档位不渲染 */
  const milestoneTargets = MILESTONES.map(({ k }) => (k <= total ? players[k - 1].wave || 0 : null));
  const milestoneCols = MILESTONES.filter((_, mi) => milestoneTargets[mi] !== null).length;
  const milestoneCells = MILESTONES.map(({ label }, mi) => {
    const target = milestoneTargets[mi];
    return target === null ? '' : stat(wanDec(target), `${label}门槛`);
  })
    .filter(Boolean)
    .join('');
  const milestoneCard =
    milestoneCols > 0
      ? `<div style="margin-top:12px;border:1px solid #eef1f6;border-radius:14px;padding:11px 14px;display:flex;align-items:center;gap:16px">
        <div style="flex:none">
          <div style="font-size:12.5px;font-weight:800;color:#344054">名次门槛</div>
          <div style="margin-top:3px;font-size:10px;color:#98a2b3">追到该名次要多少分</div>
        </div>
        <div style="flex:1;display:grid;grid-template-columns:repeat(${milestoneCols},1fr);gap:10px">${milestoneCells}</div>
      </div>`
      : '';

  /* 排名行：名次奖牌 + 姓名/组别印章 + 分值；第二行音浪条 + 距上一位差多少；
   * 第三行固定档位——已进到哪一档（绿）+ 逐档还差多少分（前10/前5/前三/前2/前1，红）。 */
  const rows = players
    .map((p, i) => {
      const wave = p.wave || 0;
      const rank = ranks[i];
      const gap = gaps[i];
      const t = THEMES[p.groupIndex % THEMES.length];
      const medal = rank <= 3 ? RANK_THEMES[rank - 1] : null;
      const badgeStyle = medal
        ? `background:${medal.bg};color:${medal.fg}`
        : 'background:#f2f4f8;color:#667085';
      const pct = Math.max(2, Math.round((wave / maxWave) * 100));
      const gapText =
        i === 0 ? '总榜第 1 名' : gap === 0 ? '与上一位并列' : `距上一位差 ${wanDec(gap)}`;
      /* 固定档位：已进到的最紧一档（如「已进前5」）+ 还没追上的档位逐档差多少分。
         位置 = i + 1；k < 位置 的档位才算「还没进」，k >= 位置的档位都算已进。 */
      const position = i + 1;
      const achieved = MILESTONES.filter(
        (m, mi) => milestoneTargets[mi] !== null && position <= m.k,
      );
      const achievedLabel = achieved.length ? achieved[achieved.length - 1].label : '';
      const aheadChips = MILESTONES.map((m, mi) => {
        const target = milestoneTargets[mi];
        if (target === null || m.k >= position) return '';
        const diff = target - wave;
        const text = diff > 0 ? `${m.label} 差${wanDec(diff)}` : `${m.label} 持平`;
        return `<span style="color:${diff > 0 ? '#d95b4f' : '#dd8f1d'};font-weight:${diff > 0 ? 700 : 500}">${esc(text)}</span>`;
      })
        .filter(Boolean)
        .join('');
      const milestoneLine = `<div style="margin-top:5px;display:flex;flex-wrap:wrap;gap:2px 8px;font-size:10px;line-height:15px;color:#98a2b3">
          ${achievedLabel ? `<span style="color:#2f9e5d;font-weight:700">已进${esc(achievedLabel)}</span>` : ''}${aheadChips}
        </div>`;
      return `<div style="break-inside:avoid;margin-bottom:7px;border:1px solid #eef1f6;border-radius:11px;padding:8px 10px 9px;background:#fff">
        <div style="display:flex;align-items:center;gap:7px">
          <div style="width:25px;height:25px;border-radius:8px;flex:none;text-align:center;line-height:25px;font-size:11.5px;font-weight:800;${badgeStyle}">${rank}</div>
          <div style="flex:1;min-width:0;display:flex;align-items:center;gap:6px">
            <b style="font-size:14px;color:#101828;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">${esc(p.name)}</b>
            <span style="flex:none;font-size:10px;padding:1px 5px;border-radius:6px;background:${t.bg};border:1px solid ${t.bd};color:${t.tc};font-weight:700;white-space:nowrap">${esc(p.groupName)}</span>
          </div>
          <div style="flex:none;text-align:right">
            <div style="font-size:14px;font-weight:800;color:#101828;line-height:1.15">${wanDec(wave)}</div>
            <div style="font-size:9px;color:#98a2b3">${esc(metricLabel)}</div>
          </div>
        </div>
        <div style="margin-top:6px;display:flex;align-items:center;gap:8px">
          <div style="flex:1;height:5px;border-radius:3px;background:#f1f4f9;overflow:hidden">
            <div style="width:${pct}%;height:100%;border-radius:3px;background:linear-gradient(90deg,${t.bd},${t.tc})"></div>
          </div>
          <div style="flex:none;font-size:10.5px;font-weight:${gap > 0 ? 700 : 400};color:${gap > 0 ? '#d95b4f' : '#98a2b3'};white-space:nowrap">${esc(gapText)}</div>
        </div>
        ${milestoneLine}
      </div>`;
    })
    .join('');

  const pill = (num: string, label: string, solid: boolean) =>
    `<div style="min-width:64px;text-align:center;border-radius:16px;padding:9px 16px;${solid ? 'background:#fff;color:#f4573d' : 'background:rgba(255,255,255,.28);color:#fff'}">
      <div style="font-size:22px;font-weight:800;line-height:1.1">${esc(num)}</div>
      <div style="font-size:10px;opacity:.85;margin-top:1px">${esc(label)}</div>
    </div>`;

  /* 同 buildExportNode：box-sizing 必须内联，否则 1024 宽 + 左右内边距会被画布裁掉右边缘 */
  const html = `<div style="box-sizing:border-box;width:1024px;padding:24px 26px 18px;background:#fff;font-family:-apple-system,'PingFang SC','Microsoft YaHei',sans-serif;color:#101828">
      <div style="display:flex;align-items:center;justify-content:space-between;background:linear-gradient(120deg,#ff9a3d 0%,#ff6a5e 55%,#ff4d94 100%);border-radius:20px;padding:20px 24px;color:#fff">
        <div style="min-width:0">
          <div style="font-size:11px;letter-spacing:2px;opacity:.85">${esc(input.brandLine || 'PK GROUP · 总排名分差')}</div>
          <div style="margin-top:6px;font-size:28px;font-weight:800;letter-spacing:.02em">${esc(input.title)}</div>
          ${input.metaLine ? `<div style="margin-top:8px;font-size:12.5px;opacity:.95">${esc(input.metaLine)}</div>` : ''}
        </div>
        <div style="display:flex;gap:10px;flex:none;margin-left:16px">${pill(String(legend.size), '组', false)}${pill(String(total), '人', true)}</div>
      </div>

      <div style="margin-top:14px;background:#fff8f2;border:2px solid #ffd9c9;border-radius:16px;padding:12px 16px;display:flex;align-items:center;gap:20px">
        <div style="flex:none">
          <div style="font-size:13px;font-weight:800;color:#f4573d">分差概览</div>
          <div style="margin-top:3px;font-size:10px;color:#b0a9a3">按总排名相邻两位计算</div>
        </div>
        <div style="flex:1;display:grid;grid-template-columns:repeat(3,1fr);gap:10px">
          ${stat(maxGapText, '最大断层')}${stat(wanDec(avgGap), '平均分差')}${stat(`${tieCount} 人`, '并列人数')}
        </div>
      </div>

      ${milestoneCard}

      ${legend.size > 1 ? `<div style="margin-top:12px;border:1px solid #eef1f6;border-radius:14px;padding:11px 14px">
        <div style="font-size:12.5px;font-weight:800;color:#344054">各组名次分布<span style="margin-left:6px;font-weight:400;font-size:10px;color:#98a2b3">总榜里每组占了哪一段</span></div>
        <div style="margin-top:8px;display:grid;grid-template-columns:repeat(4,1fr);gap:7px 12px">${legendRows}</div>
      </div>` : ''}

      <div style="margin-top:16px;display:flex;align-items:baseline;justify-content:space-between">
        <div style="font-size:15px;font-weight:800;color:#101828">总排名 · 全员</div>
        <div style="font-size:10.5px;color:#98a2b3">共 ${total} 人 · 按${esc(metricLabel)}从高到低</div>
      </div>
      <div style="column-count:2;column-gap:14px;margin-top:10px">${rows}</div>
      <div style="margin-top:10px;text-align:center;color:#98a2b3;font-size:11px">${esc(input.footer || '内部数据 · 请勿外传')}</div>
    </div>`;

  const stage = document.createElement('div');
  stage.className = 'export-stage';
  stage.style.cssText = `position:fixed;left:-10000px;top:0;width:1024px;background:#fff;z-index:-1;opacity:0;pointer-events:none;`;
  stage.innerHTML = html;
  document.body.appendChild(stage);
  return stage.firstElementChild as HTMLElement;
}
