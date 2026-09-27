/** 用真实 Chrome 跑一遍「导出图片」，抓控制台错误与页面提示 */
import { chromium } from 'playwright';

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function main() {
  const browser = await chromium.launch({ channel: 'chrome', headless: true });
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });

  const logs: string[] = [];
  page.on('console', (m) => logs.push(`[${m.type()}] ${m.text()}`));
  page.on('pageerror', (e) => logs.push(`[pageerror] ${e.message}`));

  await page.goto('http://127.0.0.1:5173/', { waitUntil: 'networkidle' });
  console.log('页面已打开');

  await page.getByRole('button', { name: /解析并分组/ }).click();
  await wait(6000);
  const stat = await page.locator('body').innerText();
  console.log('=== 页面片段 ===');
  console.log(stat.slice(0, 400).replace(/\n+/g, ' | '));

  // 导出
  const downloadPromise = page.waitForEvent('download', { timeout: 15000 }).catch(() => null);
  await page.getByRole('button', { name: /导出图片/ }).click();
  await wait(6000);
  const download = await downloadPromise;
  console.log('\n=== 下载结果 ===');
  console.log(download ? `✅ 成功：${download.suggestedFilename()}` : '❌ 没有触发下载');

  console.log('\n=== 控制台 ===');
  console.log(logs.slice(-25).join('\n') || '(空)');

  await browser.close();
  process.exit(0);
}

void main();
