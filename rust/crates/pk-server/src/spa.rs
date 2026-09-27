//! 静态资源 + SPA 回退。
//!
//! 自己实现而不用 `ServeDir::not_found_service`，因为后者在找不到文件时
//! 会把 **404 状态码** 一起带出来：分享链接 `/s/12` 于是变成
//! 「200 内容 + 404 状态」，浏览器、微信、SEO 都不认。
//!
//! 规则：
//! - `/api/*` 没命中路由 → JSON 404（不能返回 HTML，前端要靠 JSON 报错）
//! - 真实文件命中 → 按扩展名给 Content-Type
//! - 其余路径 → index.html + 200（前端路由自己解析）

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// 构建失败时为 None：服务只提供 API，不挂静态目录
pub type Index = Option<Arc<Vec<u8>>>;

pub async fn load_index(dist_dir: &Path) -> Index {
    let path = dist_dir.join("index.html");
    match tokio::fs::read(&path).await {
        Ok(bytes) if !bytes.is_empty() => Some(Arc::new(bytes)),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "index.html 为空");
            None
        }
    }
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("map") => "application/json; charset=utf-8",
        Some("txt") => "text/plain; charset=utf-8",
        Some("webmanifest") => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

/// 把 URL 路径安全地拼到 dist 目录下。
/// 返回 None 表示路径逃出了根目录（`../`、编码过的 `%2e%2e` 之类）。
fn resolve(dist_dir: &Path, uri_path: &str) -> Option<PathBuf> {
    // 先按 % 编码解一遍，再挡掉 .. 和绝对路径
    let decoded = percent_decode(uri_path);
    if decoded.contains('\0') {
        return None;
    }
    let mut out = dist_dir.to_path_buf();
    for seg in decoded.split('/') {
        match seg {
            "" | "." => continue,
            // 任何试图往上跳的片段都直接拒掉，不做 normalize 兜底
            ".." => return None,
            s => out.push(s),
        }
    }
    // 目录请求（/assets/）本身不是文件，交给上层回退到 index.html
    if out.is_dir() {
        return None;
    }
    out.is_file().then_some(out)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub async fn handler(
    axum::extract::State((dist_dir, index)): axum::extract::State<(PathBuf, Index)>,
    uri: Uri,
) -> Response {
    let path = uri.path();

    // API 没命中路由：给前端一个能解析的 JSON 404
    if path.starts_with("/api/") {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("接口不存在：{path}") })),
        )
            .into_response();
    }

    // 真实文件
    if let Some(file) = resolve(&dist_dir, path) {
        if let Ok(bytes) = tokio::fs::read(&file).await {
            let ct = content_type(&file);
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, ct)
                .header(header::CACHE_CONTROL, "no-cache")
                .body(Body::from(bytes))
                .expect("构造静态资源响应失败");
        }
    }

    // 其余交给前端路由
    match index {
        // Arc<Vec<u8>> 不走 Body::from，克隆成本也就一次 memcpy
        Some(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from(bytes.as_ref().clone()))
            .expect("构造 SPA 响应失败"),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(percent_decode("/a%20b"), "/a b");
        assert_eq!(percent_decode("/%E5%A4%AE"), "/央");
        // 残缺的 % 序列原样保留
        assert_eq!(percent_decode("/100%"), "/100%");
        assert_eq!(percent_decode("/a%zz"), "/a%zz");
    }

    #[test]
    fn blocks_directory_traversal() {
        let root = Path::new("/srv/web");
        // 目录本身不是文件，回退到 index.html
        assert_eq!(resolve(root, "/"), None);
        assert_eq!(resolve(root, "/assets/"), None);
        // 上跳必须被拒掉
        assert_eq!(resolve(root, "/../etc/passwd"), None);
        assert_eq!(resolve(root, "/assets/../../etc/passwd"), None);
        // 编码过的上跳同样要拒
        assert_eq!(resolve(root, "/%2e%2e/etc/passwd"), None);
        assert_eq!(resolve(root, "/a/../../secret"), None);
    }

    #[test]
    fn rejects_embedded_nul() {
        assert_eq!(resolve(Path::new("/srv/web"), "/a%00.png"), None);
    }

    #[test]
    fn content_types_cover_frontend_assets() {
        assert_eq!(
            content_type(Path::new("/a/index.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            content_type(Path::new("/a/index.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type(Path::new("/a/index.css")),
            "text/css; charset=utf-8"
        );
        assert_eq!(content_type(Path::new("/a/x.svg")), "image/svg+xml");
        assert_eq!(content_type(Path::new("/a/x.woff2")), "font/woff2");
        assert_eq!(
            content_type(Path::new("/a/x.unknown")),
            "application/octet-stream"
        );
    }

    #[tokio::test]
    async fn api_404_is_json_not_html() {
        let dir = std::env::temp_dir().join(format!("pk-spa-test-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("index.html"), b"<html>ok</html>")
            .await
            .unwrap();
        let index = load_index(&dir).await;
        let state = (dir.clone(), index);

        let res = handler(axum::extract::State(state), Uri::from_static("/api/nope")).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );

        // 未知路径 -> index.html + 200（这是分享链接 /s/12 依赖的行为）
        let res = handler(
            axum::extract::State((dir.clone(), load_index(&dir).await)),
            Uri::from_static("/s/12"),
        )
        .await;
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "SPA 回退必须是 200，不能带 404"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
