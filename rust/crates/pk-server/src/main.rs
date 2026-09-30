//! pk-server：抖音 PK 挑战赛分组服务
//!
//! - 管理员后台 API：/api/*
//! - 对外只读展示页：/s/:planId（静态前端 + SPA 回退）

mod auth;
mod config;
mod dialect;
mod grouping;
mod matching;
mod mysql;
mod public;
mod repos;
mod routes;
mod spa;
mod sqlite;
mod state;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::routing::{delete, get, post};
use axum::Router;

use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Docker HEALTHCHECK 用：探活自身，不连数据库
    if std::env::args().any(|a| a == "--health-check") {
        return Ok(());
    }

    // .env 值里带空格和中文（`主库 · douyinxs`），shell source 和 dotenvy 都处理不了，自己解析
    let env_path = std::env::var("ENV_FILE")
        .or_else(|_| std::env::var("DOTENV_PATH"))
        .unwrap_or_else(|_| "../apps/server/.env".to_string());
    config::load_dotenv(&env_path);
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    /* SQLite（别名 + 方案） */
    sqlite::init(&config::sqlite_path())?;

    /* MySQL 只读数据源 */
    let sources = config::load_sources()?;
    let pools = mysql::build_pools(&sources).await?;
    let active = std::env::var("DB_SOURCE")
        .ok()
        .filter(|id| sources.iter().any(|s| &s.id == id));
    let active = active.unwrap_or_else(|| sources[0].id.clone());
    let st = Arc::new(AppState {
        auth: Arc::new(auth::Auth::from_env()),
        wave_ttl: repos::ttl_from_env(),
        sources,
        pools,
        active: Mutex::new(active),
        cache: Mutex::new(Default::default()),
    });

    /* 对外公开的只读端点：不要鉴权，单独一层 */
    let public = Router::new()
        .route("/api/public/plans", get(public::plans))
        .route("/api/public/plans/{id}", get(public::plan));

    /* API */
    let api = Router::new()
        .merge(public)
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/session", get(auth::session))
        .route("/api/health", get(routes::health))
        .route("/api/sources", get(routes::sources))
        .route("/api/sources/active", post(routes::set_active))
        .route("/api/sources/{id}/test", post(routes::test_source))
        .route("/api/meta", get(routes::meta))
        .route("/api/roster/parse", post(routes::roster_parse))
        .route(
            "/api/roster",
            get(routes::get_roster).put(routes::put_roster),
        )
        .route("/api/anchors", get(routes::anchors))
        .route("/api/waves", post(routes::waves))
        .route("/api/cache/refresh", post(routes::cache_refresh))
        .route(
            "/api/aliases",
            get(routes::list_aliases).post(routes::upsert_alias),
        )
        .route("/api/aliases/{alias}", delete(routes::remove_alias))
        .route("/api/plans", get(routes::plans).post(routes::save_plan))
        .route(
            "/api/plans/{id}",
            get(routes::get_plan).delete(routes::delete_plan),
        )
        .route("/api/plans/{id}/publish", post(routes::set_plan_published))
        // 分组算法（权威实现都在 pk-core，浏览器不再跑第二份）
        .route("/api/group/arrange", post(grouping::arrange))
        .route("/api/group/inspect", post(grouping::inspect))
        .route("/api/group/polish", post(grouping::polish))
        .route("/api/group/constraints", get(grouping::constraints_meta));

    // 鉴权：ADMIN_PASSWORD 配了就拦，没配等于本地开发全放行。
    // 公开端点、登录、探活接口在 auth.rs 的白名单里。
    let cors = if let Ok(origins) = std::env::var("CORS_ORIGINS") {
        let list: Vec<axum::http::HeaderValue> = origins
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter_map(|s| match s.parse() {
                Ok(v) => Some(v),
                Err(_) => {
                    tracing::warn!(origin = s, "CORS_ORIGINS 里的地址不合法，已忽略");
                    None
                }
            })
            .collect();
        tracing::info!(count = list.len(), "CORS 白名单已启用");
        tower_http::cors::CorsLayer::new()
            .allow_origin(list)
            .allow_methods(tower_http::cors::Any)
            .allow_headers(tower_http::cors::Any)
    } else {
        // 默认只允许同源：后台和展示页都由本服务托管，不需要放开跨域
        tracing::info!("未设置 CORS_ORIGINS，仅允许同源调用");
        tower_http::cors::CorsLayer::new()
    };
    let api = api
        .layer(axum::middleware::from_fn_with_state(
            st.clone(),
            auth::require_admin,
        ))
        .layer(cors);

    /* 前端静态资源 + SPA 回退（生产单端口模式）。
    启动时只判断 index.html 在不在，内容由 spa::handler 每次回退时现读，
    这样前端重新构建不用重启服务。 */
    let dist_dir = std::env::var("WEB_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../../web/dist"));
    let index = spa::load_index(&dist_dir).await;

    let app = match index {
        true => {
            tracing::info!(dir = %dist_dir.display(), "静态目录已挂载");
            // 注意：回退必须自己给 200。用 ServeDir::not_found_service 的话，
            // 分享链接 /s/:id 会变成「HTML 内容 + 404 状态码」。
            let spa_router = Router::new()
                .fallback(spa::handler)
                .with_state((dist_dir.clone(), index));
            api.fallback_service(spa_router)
        }
        false => {
            tracing::warn!(dir = %dist_dir.display(), "没有前端构建产物，仅提供 API（先跑 pnpm --filter @pk/web build）");
            api
        }
    };

    let app = app.with_state(st.clone());

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8787);
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let addr: std::net::SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|_| anyhow::anyhow!("无效的监听地址: {host}:{port}"))?;
    tracing::info!("pk-server 启动 http://{addr}（管理员 /  ·  对外展示 /s/:id）");
    tracing::info!(
        auth = if st.auth.enabled() {
            "已开启（需口令）"
        } else {
            "未开启（公开）"
        },
        "管理员鉴权状态"
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
