const ORIGIN = 'https://pk.subisle.com'

/**
 * Pages 同域 API 代理：
 * - 浏览器始终访问 app.subisle.com/api/*，不需要改前端代码或 CORS
 * - 上游仍是 pk.subisle.com，登录头（Authorization / X-My-Token）原样转发
 * - 不在 Pages Function 内缓存；公开展示接口由 Cloudflare Cache Rule 在 pk 侧缓存
 * - 流式响应（例如 /api/public/events SSE）保留 response.body，不整包缓冲
 */
export async function onRequest(context) {
  const request = context.request
  const incoming = new URL(request.url)
  const target = new URL(incoming.pathname + incoming.search, ORIGIN)

  const headers = new Headers(request.headers)
  headers.delete('host')
  headers.delete('content-length')

  const init = {
    method: request.method,
    headers,
    redirect: 'manual',
  }
  if (request.method !== 'GET' && request.method !== 'HEAD') {
    init.body = request.body
  }

  try {
    const upstream = await fetch(target.toString(), init)
    const responseHeaders = new Headers(upstream.headers)
    responseHeaders.set('X-PK-Proxy', 'cloudflare-pages')
    const edgeCache = upstream.headers.get('cf-cache-status')
    if (edgeCache) responseHeaders.set('X-PK-Edge-Cache', edgeCache)
    return new Response(upstream.body, {
      status: upstream.status,
      statusText: upstream.statusText,
      headers: responseHeaders,
    })
  } catch (error) {
    return Response.json(
      { error: '上游 API 暂时不可用', detail: String(error?.message || error) },
      { status: 502, headers: { 'Cache-Control': 'no-store' } },
    )
  }
}
