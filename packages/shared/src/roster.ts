import type { ParsedRow } from './types.js';

// 序号：半角/全角数字 + 「.、)．·）」均可（微信里粘出来常是全角 `１５．`）
const LEADING_INDEX = /^\s*[0-9０-９]+\s*[.、)．·）]\s*/;
// 整个 token 都是数字 = 序号（没有主播叫纯数字），如没跟序号符的 `15 浩龙`
const ALL_DIGITS = /^[0-9０-９]+$/;
const SEPARATORS = /[\s\u3000\uFF0B+➕＋（(（—–\-_~,，、/]+/;

export interface ParseOptions {
  aliases?: Record<string, string>;
  exclude?: string[];
}

export function normalizeName(input: string): string {
  return input
    .replace(/\uFF08/g, '(')
    .replace(/\uFF09/g, ')')
    .replace(/[\s\u3000]/g, '')
    .trim();
}

export function applyAlias(name: string, aliases?: Record<string, string>): { name: string; aliasApplied?: string } {
  if (!aliases) return { name };
  const hit = aliases[normalizeName(name)] ?? aliases[name];
  if (hit) return { name: hit, aliasApplied: hit };
  return { name };
}

/**
 * 解析接龙名单。
 * 规则：行首序号去掉；按分隔符切分后【第一个 token = 参赛主播】，其余 token = 队长/师傅（仅标注，不参赛）。
 * 例：啸安➕狼兴 -> 啸安（队长 狼兴）；啸阳（狼泽） -> 啸阳（队长 狼泽）
 */
export function parseRoster(text: string, options: ParseOptions = {}): ParsedRow[] {
  const { aliases, exclude = [] } = options;
  const excludeSet = new Set(exclude.map(normalizeName));
  const rows: ParsedRow[] = [];

  for (const line of text.split(/\r?\n/)) {
    const raw = line.trim();
    if (!raw) continue;
    const body = raw.replace(LEADING_INDEX, '').trim();
    if (!body) continue;

    const tokens = body.split(SEPARATORS).filter(Boolean);
    if (tokens.length === 0) continue;

    // 首个 token 是纯数字 = 序号没跟序号符（`15 浩龙` / `16）玖柒`），跳过它再取人名；
    // `名字 师傅`（用在册主播填充生成的格式）不受影响——人名不是纯数字。
    const start = tokens.length > 1 && ALL_DIGITS.test(tokens[0]) ? 1 : 0;
    const first = tokens[start];
    if (!first) continue;
    const rest = tokens.slice(start + 1).filter((t) => normalizeName(t) !== normalizeName(first));
    const { name, aliasApplied } = applyAlias(first, aliases);

    rows.push({
      raw,
      name,
      leader: rest.length ? rest.join(' ') : undefined,
      aliasApplied,
      excluded: excludeSet.has(normalizeName(name)),
    });
  }

  return rows;
}

export const DEFAULT_ALIASES: Record<string, string> = {
  阿楠: '南方楠',
};

export const DEFAULT_EXCLUDE: string[] = ['目'];

export const DEFAULT_ROSTER = `1. 目
2. 浩杰
3. 浩宁
4. 浩延
5. 浩鸣   李斌
6. 浩艺
7. 浩阳
8. 浩运
9. 浩龙
10. 玖玥
11. 狼兴
12. 凤九
13. 狼辉
14. 浩哲
15. 啸安➕狼兴
16. 狼耀
17. 浩玟
18. 狼征
19. 狼小宝
20. 啸恒
21. 狼澈
22. 啸阳（狼泽）
23. 啸辰
24. 狼途
25. 啸泽
26. 啸森——狼俊
27. 狼岳
28. 狼明+浩龙
29. 玖柒
30. 狼九
31. 狼哲
32. 浩冬
33. 狼影
34. 狼佑
35. 浩辰
36. 狼艺
37. 狼雨
38. 阿楠
39. 浩雨
40. 啸帆
41. 狼俊
42. 玖雪
43. 啸强（狼俊）
44. 狼赫+浩雨
45. 浩森
46.  啸宇 + 狼俊
47. 浩启
48. 狼泽
49. 啸弘 （狼泽）
50. 啸寻
51. 啸昭
52. 狼仔
53. 狼轩
54. 狼辰—浩杰
55. 浩月`;
