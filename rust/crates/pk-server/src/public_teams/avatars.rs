//! public_teams::avatars —— 展示端**同源头像**下发（2026-10-16）。
//!
//! ## 这个端点是干什么的
//! 展示端「主播列表」（领奖台 + 名次行）要渲染主播的抖音真实头像。抖音的头像 URL
//! 落在 `*.douyinpic.com` 这类**跨域 CDN** 上：展示端是公开站点，把跨域 `<img>` 直接
//! 指过去，一旦抖音改了路径策略 / 封了 Referer，页面头像会集体变成碎图，而且没有任何
//! 本地兜底。所以头像字节由 T1（`avatar_sync`）**同步留存到本地库** `person_avatar`，
//! 再由本端点从**同源**下发。
//!
//! ## 契约（与榜单同一个数据面）
//! - 只按 `person_id` 取图，路径参数名固定 `person_id`；
//! - 有 `image` 时下发真实抖音头像；没有时生成**品牌默认 SVG**，
//!   （账号已注销 / 抖音号查不到 / 没登记抖音号），保证展示端不再出现空洞；
//! - **不在榜的人也有图**：`person_avatar` 与榜单是两张表，本端点不做「是否在榜」
//!   的鉴权/存在性判断。展示端只渲染后端 DTO（daily / period_public）下发的 item，
//!   而 DTO 只给榜上的人拼 avatar 地址，所以这个端点即使能被直接枚举 person_id 拿到
//!   某人的头像，也不构成额外的信息泄露面 —— 头像本身就是抖音公开资料。
//!
//! ## 为什么可以 immutable 长缓存
//! 前端 URL 形如 `/api/public/avatars/<person_id>?v=<hash>`，`hash` 是 `image` 内容
//! 前 16 位 md5（migration 025）。**图片一变 hash 就变、URL 就变**，同一个 URL 的
//! 响应内容保证不变，因此 `Cache-Control: immutable` + 一年 max-age 是安全的：
//! 换图时前端拿到的是新 URL，不会命中旧缓存。hash 为空（历史行 / 同步异常）时前端
//! 退化成不带 `?v=` 的地址，仍然能出图，只是失去「换图即破缓存」的保证。
//!
//! ## 为什么人物不存在返回 404、图片缺失返回默认头像
//! 头像对榜单是**装饰性字段**：`person_avatar` 表没建（迁移没跑）、连接池抖动、行不存在，
//! 都不应该让展示端拿到 500。只要人物仍在职，就回退到稳定 SVG；人物不存在 / 已删除 /
//! 已离团才 404。查询失败只打 debug 日志，内部错误绝不外发。

use super::*;
// super::* 里没有 StatusCode（mod.rs 只导入了 HeaderMap）：本文件全程用它出 404/200，
// 显式引入比满文件写 `axum::http::StatusCode::` 更省行
use axum::http::StatusCode;

/// 没有抖音头像时的稳定缓存版本号。URL 带它，后续代码升级时可换版本破缓存。
pub(crate) const FALLBACK_AVATAR_HASH: &str = "fallback-v2";

/// 轻量取 hash：给 /my 组装同源头像 URL 用；无真实图时返回 fallback 版本。
pub(crate) async fn avatar_hash(st: &ArcState, person_id: u64) -> Option<String> {
    if person_id == 0 {
        return None;
    }
    // 没有真实图片也返回 fallback 版本：前端统一走头像端点，不再把 null 当“没头像”。
    // 这样已注销号、未登记抖音号、同步失败的人都有一张稳定的品牌默认头像。
    let sql = format!(
        "SELECT COALESCE(NULLIF(pa.hash, ''), '{}') AS hash \
           FROM person p LEFT JOIN person_avatar pa \
             ON pa.person_id = p.id AND pa.image IS NOT NULL \
          WHERE p.id = ? AND p.deleted_at IS NULL \
            AND (p.status IS NULL OR p.status <> 'left') LIMIT 1",
        FALLBACK_AVATAR_HASH
    );
    pgrs::query_with_binds(&st.pools, &sid(st), &sql, |q| q.bind(person_id as i64))
        .await
        .ok()?
        .first()?
        .try_get::<String, _>("hash")
        .ok()
}

/// 生成品牌默认头像（SVG）：没有真实抖音图时仍保证每个展示位都有头像。
///
/// 不读取外网、不落磁盘；按 person_id 固定配色，姓名取前两个非空白字符，
/// URL 带 [`FALLBACK_AVATAR_HASH`] 可安全长缓存。XML 特殊字符在这里统一转义。
fn fallback_avatar_svg(person_id: u64, name: &str) -> String {
    const PALETTE: &[(&str, &str, &str)] = &[
        ("#5B8DEF", "#7C3AED", "#C7D2FE"),
        ("#E58F5A", "#D94B73", "#FDE2C8"),
        ("#31A383", "#0E7490", "#C8F3E5"),
        ("#9A6FD0", "#5B4BC4", "#E8DDFB"),
        ("#D96A93", "#BE3455", "#FAD4E2"),
        ("#4C91C9", "#2563A8", "#D4EAFE"),
        ("#B8894A", "#8B5E22", "#F4E2C4"),
        ("#6F9A52", "#397A38", "#DDEFC8"),
    ];
    let (a, b, ring) = PALETTE[(person_id as usize) % PALETTE.len()];
    let label: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_whitespace())
        .take(2)
        .collect();
    let label = if label.is_empty() { "?" } else { label.as_str() };
    let font_size = if label.chars().count() > 1 { 70 } else { 92 };
    let label = xml_escape(label);
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="200" viewBox="0 0 200 200" role="img" aria-label="默认头像">
<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="{a}"/><stop offset="1" stop-color="{b}"/></linearGradient></defs>
<rect width="200" height="200" rx="100" fill="url(#g)"/>
<circle cx="165" cy="34" r="54" fill="{ring}" fill-opacity=".20"/>
<circle cx="34" cy="169" r="78" fill="#fff" fill-opacity=".08"/>
<circle cx="100" cy="94" r="70" fill="none" stroke="#fff" stroke-opacity=".18" stroke-width="2"/>
<text x="100" y="116" text-anchor="middle" dominant-baseline="middle" font-family="PingFang SC, Hiragino Sans GB, Noto Sans CJK SC, sans-serif" font-size="{font_size}" font-weight="700" fill="#fff">{label}</text>
<text x="100" y="170" text-anchor="middle" font-family="Menlo, Consolas, monospace" font-size="14" font-weight="700" letter-spacing="4" fill="#fff" fill-opacity=".78">PK</text>
</svg>"##
    )
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

async fn fallback_avatar_response(st: &ArcState, person_id: i64) -> Response {
    let sql = "SELECT name FROM person WHERE id = ? AND deleted_at IS NULL \
               AND (status IS NULL OR status <> 'left') LIMIT 1";
    let Ok(rows) = pgrs::query_with_binds(&st.pools, &sid(st), sql, |q| q.bind(person_id)).await
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(name) = rows
        .first()
        .and_then(|r| r.try_get::<String, _>("name").ok())
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let svg = fallback_avatar_svg(person_id.max(0) as u64, &name);
    (
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "image/svg+xml; charset=utf-8".to_string(),
            ),
            (
                axum::http::header::CACHE_CONTROL,
                "public, max-age=31536000, immutable".to_string(),
            ),
        ],
        svg.into_bytes(),
    )
        .into_response()
}

/// GET /api/public/avatars/{person_id}
///
/// 成功：`200` + `Content-Type: <mime 或 image/jpeg>` +
/// `Cache-Control: public, max-age=31536000, immutable` + 图片字节体。
/// 未命中 / 没有真实图：返回稳定的品牌默认 SVG；只有人物不存在、已删除或已离团才 404。
pub async fn avatar(State(st): State<ArcState>, Path(person_id): Path<i64>) -> Response {
    // 负 id / 0 不可能是有效 person_id：直接 404，省一次库查询（脏路径不放大成查询压力）
    if person_id <= 0 {
        return StatusCode::NOT_FOUND.into_response();
    }
    // SQL 必须只读（pgrs::readonly_guard），且用 `?` 占位符交给 pq::rewrite 重写成 $1
    let sql = "SELECT image, mime FROM person_avatar WHERE person_id = ? AND image IS NOT NULL";
    let rows = match pgrs::query_with_binds(&st.pools, &sid(&st), sql, |q| q.bind(person_id)).await
    {
        Ok(rows) => rows,
        Err(e) => {
            /* 表不存在（迁移未跑）/ 池不可用 / 查询异常：对调用方一律等价于「没有这张图」。
            日志走 debug：这是装饰性资源的高频路径，缺迁移时刷 warn 会把日志淹掉。 */
            tracing::debug!(person_id, error = %e, "头像查询失败，改下发品牌默认头像");
            return fallback_avatar_response(&st, person_id).await;
        }
    };
    let Some(row) = rows.first() else {
        // 该人没有本地留存头像：生成稳定的品牌默认头像，避免展示端出现空洞。
        return fallback_avatar_response(&st, person_id).await;
    };
    // image 列是 BYTEA（可空，SQL 已过滤 NULL）：读不出字节同样按无图处理
    let Some(bytes) = row.try_get::<Option<Vec<u8>>, _>("image").ok().flatten() else {
        tracing::debug!(
            person_id,
            "person_avatar.image 读不出字节，改下发品牌默认头像"
        );
        return fallback_avatar_response(&st, person_id).await;
    };
    if bytes.is_empty() {
        // 空字节能读出但没有任何像素：不能给空 200，改下发品牌默认头像。
        return fallback_avatar_response(&st, person_id).await;
    }
    /* mime 由同步入口按文件魔数校验后写入（025 注释：非 JPEG 一律不收），这里只做
    兜底：列缺失/为空时回落 image/jpeg，避免下发 `Content-Type: `（空值）让浏览器嗅探。 */
    let mime = row
        .try_get::<String, _>("mime")
        .ok()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "image/jpeg".to_string());
    (
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, mime),
            (
                axum::http::header::CACHE_CONTROL,
                "public, max-age=31536000, immutable".to_string(),
            ),
        ],
        bytes,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_avatar_is_stable_svg_and_escapes_name() {
        let svg = fallback_avatar_svg(185, "<&啸");
        assert!(svg.starts_with("<svg"), "{svg}");
        assert!(svg.contains("viewBox=\"0 0 200 200\""), "{svg}");
        assert!(svg.contains(">&lt;&amp;</text>"), "姓名必须 XML 转义：{svg}");
        assert!(svg.contains(">PK</text>"), "{svg}");
        assert_eq!(FALLBACK_AVATAR_HASH, "fallback-v2");
    }

    #[test]
    fn fallback_avatar_handles_empty_name_and_negative_id() {
        let svg = fallback_avatar_svg(u64::MAX, "   ");
        assert!(svg.contains(">?</text>"), "{svg}");
    }
}
