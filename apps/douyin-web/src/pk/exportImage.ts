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
 *  → 两列彩色分组卡（只放组名、开赛时间和成员姓名）→ 底部落款。 */
export function buildExportNode(input: ExportInput): HTMLElement {
  const totalPlayers = input.groups.reduce((s, g) => s + g.players.length, 0);
  const cards = input.groups
    .map((g, gi) => {
      const t = THEMES[gi % THEMES.length];
      // 分组图只保留“谁在哪一组”，不混入音浪、全场名次或组内名次。
      const chips = g.players.map((p, i) =>
        `<div style="min-width:0;background:rgba(255,255,255,.82);border:1px solid rgba(255,255,255,.95);border-radius:10px;padding:9px 10px;display:flex;align-items:center;gap:8px">
          <span style="flex:none;font-size:11px;font-weight:800;color:${t.tc};font-variant-numeric:tabular-nums">${String(i + 1).padStart(2, '0')}</span>
          <b style="min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:14px;font-weight:700;color:#1f2937">${esc(p.name)}</b>
        </div>`
      );
      if (chips.length % 2 === 1) chips.push('<div></div>');
      return `<div style="break-inside:avoid;margin-bottom:14px;background:${t.bg};border:2px solid ${t.bd};border-radius:16px;padding:14px 14px 12px">
        <div style="display:flex;align-items:baseline;justify-content:space-between;gap:10px">
          <div style="font-size:18px;font-weight:800;color:${t.tc}">${esc(g.name)}</div>
          <div style="font-size:11px;color:#7b8494">${esc(g.slot ? g.slot + ' 开赛' : '')}${g.players.length ? (g.slot ? ' · ' : '') + g.players.length + ' 人' : ''}</div>
        </div>
        <div style="margin-top:10px;display:grid;grid-template-columns:1fr 1fr;gap:8px">${chips.join('') || '<div style="grid-column:1/3;color:#98a2b3;font-size:13px">空组</div>'}</div>
      </div>`;
    })
    .join('');

  const pill = (num: string, label: string, solid: boolean) =>
    `<div style="min-width:64px;text-align:center;border-radius:16px;padding:9px 16px;${solid ? 'background:#fff;color:#f4573d' : 'background:rgba(255,255,255,.28);color:#fff'}">
      <div style="font-size:22px;font-weight:800;line-height:1.1">${esc(num)}</div>
      <div style="font-size:10px;opacity:.85;margin-top:1px">${esc(label)}</div>
    </div>`;

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
  { label: '前1', k: 1 },
  { label: '前2', k: 2 },
  { label: '前三', k: 3 },
  { label: '前5', k: 5 },
  { label: '前10', k: 10 },
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

  /* 各档目标名次的当前门槛分。这里放“目标名次 + 当前持有人 + 分数”，
     让读者直接知道追到前 1/2/3/5/10 分别要多少分。 */
  const milestoneTargets = MILESTONES.flatMap((m) => {
    if (m.k > total) return [];
    const target = players[m.k - 1];
    return [{ label: m.label, k: m.k, name: target.name, score: target.wave || 0 }];
  });
  const milestoneCells = milestoneTargets
    .map(
      (t) => `<div style="min-width:0;padding:9px 10px;border-radius:11px;background:#fff;border:1px solid #f4e6df">
        <div style="font-size:11px;font-weight:800;color:#f4573d">${esc(t.label)}</div>
        <div style="margin-top:3px;font-size:12px;color:#475467;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">${esc(t.name)}</div>
        <div style="margin-top:3px;font-size:16px;font-weight:800;color:#101828;line-height:1.15">${wanDec(t.score)}</div>
        <div style="margin-top:1px;font-size:9px;color:#98a2b3">目标分</div>
      </div>`,
    )
    .join('');

  /* 排名行：名次 + 姓名 + 当前分；副行只展示“距上一位”，
     再列出该人尚未达到的目标名次还差多少分。已经超过的档位不再显示，减少噪音。 */
  const rows = players
    .map((p, i) => {
      const wave = p.wave || 0;
      const rank = ranks[i];
      const gap = gaps[i];
      const medal = rank <= 3 ? RANK_THEMES[rank - 1] : null;
      const badgeStyle = medal
        ? `background:${medal.bg};color:${medal.fg}`
        : 'background:#f2f4f8;color:#667085';
      const pct = Math.max(2, Math.round((wave / maxWave) * 100));
      const gapText =
        i === 0 ? '当前第 1 名' : gap === 0 ? '与上一位并列' : `距上一位还差 ${wanDec(gap)}`;
      const position = i + 1;
      const aheadChips = milestoneTargets
        .filter((t) => t.k < position)
        .map((t) => {
          const diff = t.score - wave;
          return `<span style="color:${diff > 0 ? '#d95b4f' : '#dd8f1d'};font-weight:${diff > 0 ? 700 : 500}">${esc(t.label)} ${diff > 0 ? '+' + wanDec(diff) : '持平'}</span>`;
        })
        .join('');
      const aheadLine = aheadChips
        ? `<div style="margin-top:5px;display:flex;flex-wrap:wrap;gap:2px 9px;font-size:10px;line-height:15px;color:#98a2b3">${aheadChips}</div>`
        : '';
      return `<div style="break-inside:avoid;margin-bottom:7px;border:1px solid #eef1f6;border-radius:11px;padding:8px 10px 9px;background:#fff">
        <div style="display:flex;align-items:center;gap:7px">
          <div style="width:25px;height:25px;border-radius:8px;flex:none;text-align:center;line-height:25px;font-size:11.5px;font-weight:800;${badgeStyle}">${rank}</div>
          <b style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:14px;color:#101828">${esc(p.name)}</b>
          <div style="flex:none;text-align:right">
            <div style="font-size:14px;font-weight:800;color:#101828;line-height:1.15">${wanDec(wave)}</div>
            <div style="font-size:9px;color:#98a2b3">${esc(metricLabel)}</div>
          </div>
        </div>
        <div style="margin-top:6px;display:flex;align-items:center;gap:8px">
          <div style="flex:1;height:5px;border-radius:3px;background:#f1f4f9;overflow:hidden">
            <div style="width:${pct}%;height:100%;border-radius:3px;background:linear-gradient(90deg,#f7b267,#f4573d)"></div>
          </div>
          <div style="flex:none;font-size:10.5px;font-weight:${gap > 0 ? 700 : 400};color:${gap > 0 ? '#d95b4f' : '#98a2b3'};white-space:nowrap">${esc(gapText)}</div>
        </div>
        ${aheadLine}
      </div>`;
    })
    .join('');

  const pill = (num: string, label: string, solid: boolean) =>
    `<div style="min-width:64px;text-align:center;border-radius:16px;padding:9px 16px;${solid ? 'background:#fff;color:#f4573d' : 'background:rgba(255,255,255,.28);color:#fff'}">
      <div style="font-size:22px;font-weight:800;line-height:1.1">${esc(num)}</div>
      <div style="font-size:10px;opacity:.85;margin-top:1px">${esc(label)}</div>
    </div>`;

  const milestoneCard = milestoneTargets.length
    ? `<div style="margin-top:14px;background:#fff8f2;border:1px solid #ffd9c9;border-radius:16px;padding:12px 14px">
        <div style="display:flex;align-items:baseline;justify-content:space-between;gap:12px">
          <div style="font-size:13px;font-weight:800;color:#f4573d">目标名次门槛</div>
          <div style="font-size:10px;color:#b0a9a3">追平看差值，超过目标还需再加 1 分</div>
        </div>
        <div style="margin-top:9px;display:grid;grid-template-columns:repeat(${milestoneTargets.length},1fr);gap:8px">${milestoneCells}</div>
      </div>`
    : '';

  const html = `<div style="box-sizing:border-box;width:1024px;padding:24px 26px 18px;background:#fff;font-family:-apple-system,'PingFang SC','Microsoft YaHei',sans-serif;color:#101828">
      <div style="display:flex;align-items:center;justify-content:space-between;background:linear-gradient(120deg,#ff9a3d 0%,#ff6a5e 55%,#ff4d94 100%);border-radius:20px;padding:20px 24px;color:#fff">
        <div style="min-width:0">
          <div style="font-size:11px;letter-spacing:2px;opacity:.85">${esc(input.brandLine || 'PK GROUP · 名次差距')}</div>
          <div style="margin-top:6px;font-size:28px;font-weight:800;letter-spacing:.02em">${esc(input.title)}</div>
          ${input.metaLine ? `<div style="margin-top:8px;font-size:12.5px;opacity:.95">${esc(input.metaLine)}</div>` : ''}
        </div>
        <div style="display:flex;gap:10px;flex:none;margin-left:16px">${pill(String(total), '人', true)}</div>
      </div>
      ${milestoneCard}
      <div style="margin-top:16px;display:flex;align-items:baseline;justify-content:space-between">
        <div style="font-size:15px;font-weight:800;color:#101828">名次差距 · 全员</div>
        <div style="font-size:10.5px;color:#98a2b3">按${esc(metricLabel)}从高到低 · 共 ${total} 人</div>
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
