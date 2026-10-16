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
//! - 只服务 `image IS NOT NULL` 的人 —— `image` 为空表示「没有可用头像」
//!   （账号已注销 / 抖音号查不到 / 没登记抖音号，见 migration 025 的列语义），
//!   这种人必须 404，让前端回落姓名首字色块，而不是给一张破图或占位图；
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
//! ## 为什么查不到也返回 404 而不是 500
//! 头像对榜单是**装饰性字段**：`person_avatar` 表没建（迁移没跑）、连接池抖动、行不存在，
//! 都不应该让展示端拿到 500 —— 前端对 404 的处理就是「没图，回落首字色块」，
//! 页面其余部分照常可用。查询失败只打 debug 日志（把库错误留在服务端），
//! 响应体为空，**绝不把内部错误外发**。

use super::*;
// super::* 里没有 StatusCode（mod.rs 只导入了 HeaderMap）：本文件全程用它出 404/200，
// 显式引入比满文件写 `axum::http::StatusCode::` 更省行
use axum::http::StatusCode;

/// GET /api/public/avatars/{person_id}
///
/// 成功：`200` + `Content-Type: <mime 或 image/jpeg>` +
/// `Cache-Control: public, max-age=31536000, immutable` + 图片字节体。
/// 未命中 / 查询失败：`404` 空体（装饰性资源，任何失败都不升级成 500）。
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
            tracing::debug!(person_id, error = %e, "头像查询失败，按无头像 404 下发");
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    let Some(row) = rows.first() else {
        // 该人没有本地留存头像（含 note=deregistered/not_found/no_douyin_no 的占位行）
        return StatusCode::NOT_FOUND.into_response();
    };
    // image 列是 BYTEA（可空，SQL 已过滤 NULL）：读不出字节同样按无图处理
    let Some(bytes) = row.try_get::<Option<Vec<u8>>, _>("image").ok().flatten() else {
        tracing::debug!(
            person_id,
            "person_avatar.image 读不出字节，按无头像 404 下发"
        );
        return StatusCode::NOT_FOUND.into_response();
    };
    if bytes.is_empty() {
        // 空字节能读出但没有任何像素：给 200 会让 <img> 直接裂图，按无图 404 更诚实
        return StatusCode::NOT_FOUND.into_response();
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
