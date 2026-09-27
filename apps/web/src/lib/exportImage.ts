/**
 * 长图导出：DOM → SVG foreignObject → Canvas → PNG Blob。
 *
 * 之前用 html-to-image 一直导不出，实测两个坑（Chrome）：
 * 1. SVG 不编码直接拼 `data:` URL，样式里的 `#` 颜色会被当成 URL 片段截断 → 图片解码失败
 * 2. 换成 blob: URL 能解码，但画布会被判为「被污染」，toBlob 抛 SecurityError
 * 所以最终方案是：encodeURIComponent 的 data URL + 自己画 canvas。
 *
 * 前提：要导出的节点必须是纯内联样式（外部 CSS 不会带进 foreignObject），
 * 且内联样式里不能出现裸双引号（会把 style="..." 属性截断，产生非法 XML）。
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

  /* 等布局稳定，否则可能量到 0 宽高 */
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
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}">`
    + `<foreignObject x="0" y="0" width="${width}" height="${height}">${serialized}</foreignObject></svg>`;

  /* 必须用 encodeURIComponent 的 data URL：
     - 不编码的话样式里的 `#` 颜色会被当成 URL 片段，图片直接截断 → 解码失败
     - 用 blob: URL 虽然能解码，但 Chrome 会把画布判为被污染，toBlob 抛 SecurityError */
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
    /* 画布被污染时给个明确提示，别让用户对着「SecurityError」发懵 */
    if (e instanceof DOMException && e.name === 'SecurityError') {
      throw new Error('画布被跨域内容污染，导出中止');
    }
    throw e;
  }
}

/** 触发浏览器下载 */
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
