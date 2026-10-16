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

use axum::body::Body;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// 构建失败时为 None：服务只提供 API，不挂静态目录。
///
/// 这里只记录「有没有」可用于 SPA 回退的 index.html，不缓存内容：
/// 内容每次回退时从磁盘现读。前端重新构建后 index.html 里的资源指纹会变，
/// 若缓存旧内容，旧 index.html 引用的 js 已被删掉，而 /assets/xxx.js 这种
/// 未命中请求又会回退到 index.html（200 + text/html），浏览器按 module 加载
/// 时因 MIME 不对直接拒绝执行 —— 表现是页面整片空白且控制台没有报错。
/// index.html 几百字节，回退也只在 SPA 导航时发生，现读完全够用。
pub type Index = bool;

pub async fn load_index(dist_dir: &Path) -> Index {
    let path = dist_dir.join("index.html");
    match tokio::fs::metadata(&path).await {
        Ok(meta) if meta.len() > 0 => true,
        Ok(_) => false,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "index.html 为空");
            false
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

    // 对外展示页（2026-10-10）：根路径 = 展示页（男女共用，页内弹团别选择卡），
    // 管理后台挪到 /admin。
    if path == "/" {
        return crate::public_teams::st_root().await;
    }

    // 管理后台：/admin 与 /admin/* —— 剥掉前缀后按 dist 解析
    // （/admin → index.html；/admin/assets/x.js → dist/assets/x.js）
    if let Some(rest) = path.strip_prefix("/admin") {
        let new_path = match rest {
            "" | "/" => "/",
            p => p,
        };
        return serve_dist(&dist_dir, index, new_path).await;
    }

    // API 没命中路由：给前端一个能解析的 JSON 404
    if path.starts_with("/api/") {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("接口不存在：{path}") })),
        )
            .into_response();
    }

    serve_dist(&dist_dir, index, path).await
}

/// 按路径给静态资源的缓存策略：
/// - `/assets/*` 是 vite 内容哈希产物（文件名带指纹），内容不可变 → 长缓存；
/// - `/fonts/*` 是自托管字体（无指纹），给 7 天短缓存，改字体最多一周内生效；
/// - 其余（index.html、SPA 回退）必须 no-cache，否则前端重构建后旧 index.html
///   会引用已被删除的旧指纹资源。
fn cache_control(path: &str) -> &'static str {
    if path.starts_with("/assets/") {
        "public, max-age=31536000, immutable"
    } else if path.starts_with("/fonts/") {
        "public, max-age=604800"
    } else {
        "no-cache"
    }
}

/// 按 dist 解析：真实文件 → 静态资源 404 → index.html 回退
async fn serve_dist(dist_dir: &Path, index: Index, path: &str) -> Response {
    // 真实文件
    if let Some(file) = resolve(dist_dir, path) {
        if let Ok(bytes) = tokio::fs::read(&file).await {
            let ct = content_type(&file);
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, ct)
                .header(header::CACHE_CONTROL, cache_control(path))
                .body(Body::from(bytes))
                .expect("构造静态资源响应失败");
        }
    }

    // 构建产物里的静态资源（vite 固定输出到 /assets/）没命中就是缺文件，
    // 不能回退成 index.html：浏览器按 module/css 加载时拿到 text/html 会直接
    // 拒绝执行，页面整片空白且控制台无报错，很难查。这里明确 404。
    if path.starts_with("/assets/") {
        return (StatusCode::NOT_FOUND, "asset not found").into_response();
    }

    // 其余交给前端路由
    if !index {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match tokio::fs::read(dist_dir.join("index.html")).await {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from(bytes))
            .expect("构造 SPA 响应失败"),
        // 启动时有、现在没了（构建中途重跑）：按 404 处理，别把错误当 HTML 吐给浏览器
        Err(e) => {
            tracing::warn!(error = %e, "回退 index.html 读取失败");
            (StatusCode::NOT_FOUND, "not found").into_response()
        }
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
    fn cache_policy_distinguishes_assets_fonts_and_html() {
        // vite 指纹产物可长缓存（内容不可变）
        assert_eq!(
            cache_control("/assets/index-DetAf99v.js"),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            cache_control("/assets/index.css"),
            "public, max-age=31536000, immutable"
        );
        // 自托管字体无指纹 → 短缓存，改字体不会卡一年
        assert_eq!(
            cache_control("/fonts/material-symbols-outlined.woff2"),
            "public, max-age=604800"
        );
        // HTML 与 SPA 回退必须 no-cache，前端重构建后旧 index.html 不能继续用
        assert_eq!(cache_control("/"), "no-cache");
        assert_eq!(cache_control("/admin"), "no-cache");
        assert_eq!(cache_control("/s/12"), "no-cache");
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

    /// 回归：曾经把 index.html 在启动时读进内存缓存，前端重新构建后
    /// 资源指纹变了，旧 index.html 引用的 js 已被删除 → 页面空白且无报错。
    #[tokio::test]
    async fn spa_fallback_follows_rebuilt_index() {
        let dir = std::env::temp_dir().join(format!("pk-spa-rebuild-{}", std::process::id()));
        tokio::fs::create_dir_all(dir.join("assets")).await.unwrap();
        tokio::fs::write(dir.join("index.html"), b"<html>v1</html>")
            .await
            .unwrap();
        // 启动时加载一次，模拟服务启动
        let index = load_index(&dir).await;
        assert!(index, "index.html 存在时 load_index 应为 true");

        // 前端重新构建：index.html 换成新指纹，新 js 上线，旧 js 删除
        tokio::fs::write(dir.join("assets/index-new.js"), b"console.log(1)")
            .await
            .unwrap();
        tokio::fs::write(dir.join("index.html"), b"<html>v2</html>")
            .await
            .unwrap();

        let res = handler(
            axum::extract::State((dir.clone(), index)),
            Uri::from_static("/s/1"),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&body),
            "<html>v2</html>",
            "SPA 回退必须反映重新构建后的 index.html，而不是启动时的旧内容"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// 回归：缺失的 /assets/*.js 曾经回退成 index.html（200 + text/html），
    /// 浏览器按 module 加载时因 MIME 不对静默拒绝执行。
    #[tokio::test]
    async fn missing_asset_is_404_not_html() {
        let dir = std::env::temp_dir().join(format!("pk-spa-asset-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("index.html"), b"<html>ok</html>")
            .await
            .unwrap();
        let index = load_index(&dir).await;

        let res = handler(
            axum::extract::State((dir.clone(), index)),
            Uri::from_static("/assets/index-gone.js"),
        )
        .await;
        assert_eq!(
            res.status(),
            StatusCode::NOT_FOUND,
            "缺失的构建产物必须 404，不能回退成 HTML"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// 管理后台挂在 /admin：`/admin` 与 `/admin/*` 都要剥前缀后按 dist 解析，
    /// 否则管理端一刷新深层链接（/admin/import、/admin/group）就 404。
    #[tokio::test]
    async fn admin_prefix_serves_spa_and_assets() {
        use std::str::FromStr;

        let dir = std::env::temp_dir().join(format!("pk-spa-admin-{}", std::process::id()));
        tokio::fs::create_dir_all(dir.join("assets")).await.unwrap();
        tokio::fs::write(dir.join("index.html"), b"<html>admin</html>")
            .await
            .unwrap();
        tokio::fs::write(dir.join("assets/index-new.js"), b"console.log(1)")
            .await
            .unwrap();

        for path in ["/admin", "/admin/", "/admin/import", "/admin/group"] {
            let res = handler(
                axum::extract::State((dir.clone(), load_index(&dir).await)),
                Uri::from_str(path).unwrap(),
            )
            .await;
            assert_eq!(res.status(), StatusCode::OK, "{path} 应回退到 index.html");
            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&body), "<html>admin</html>");
        }

        // 构建产物：/admin/assets/x.js 要能拿到真文件，缺文件时 404 而不是 HTML
        let res = handler(
            axum::extract::State((dir.clone(), load_index(&dir).await)),
            Uri::from_str("/admin/assets/index-new.js").unwrap(),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/javascript; charset=utf-8")
        );

        let res = handler(
            axum::extract::State((dir.clone(), load_index(&dir).await)),
            Uri::from_str("/admin/assets/index-gone.js").unwrap(),
        )
        .await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
