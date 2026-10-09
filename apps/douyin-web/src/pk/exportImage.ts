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

  const html = `<div style="width:1024px;padding:24px 26px 18px;background:#fff;font-family:-apple-system,'PingFang SC','Microsoft YaHei',sans-serif;color:#101828">
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
