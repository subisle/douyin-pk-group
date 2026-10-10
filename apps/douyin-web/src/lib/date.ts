/** 本地时区的 YYYY-MM-DD（P2-6）：
 *  别用 `new Date().toISOString().slice(0, 10)` —— 那是 UTC，早 8 点前会差一天
 *  （文件名、导出长图落款都会落到昨天）。与展示端 st_page.html 的 iso() 同一写法。 */
export function isoLocal(d: Date = new Date()): string {
  const pad2 = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`
}
