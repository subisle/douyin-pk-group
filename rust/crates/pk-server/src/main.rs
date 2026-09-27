//! pk-server：抖音 PK 挑战赛分组服务
//!
//! - 管理员后台 API：/api/*
//! - 对外只读展示页：/s/:planId（静态前端 + SPA 回退）

mod config;
mod dialect;
mod grouping;
mod matching;
mod mysql;
mod repos;
mod routes;
mod sqlite;
mod state;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::routing::{delete, get, post};
use axum::Router;
use tower_http::services::{ServeDir, ServeFile};

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
        wave_ttl: repos::ttl_from_env(),
        sources,
        pools,
        active: Mutex::new(active),
        cache: Mutex::new(Default::default()),
    });

    /* API */
    let api = Router::new()
        .route("/api/health", get(routes::health))
        .route("/api/sources", get(routes::sources))
        .route("/api/sources/active", post(routes::set_active))
        .route("/api/sources/{id}/test", post(routes::test_source))
        .route("/api/meta", get(routes::meta))
        .route("/api/roster/parse", post(routes::roster_parse))
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
        // 分组算法（权威实现都在 pk-core，浏览器不再跑第二份）
        .route("/api/group/arrange", post(grouping::arrange))
        .route("/api/group/inspect", post(grouping::inspect))
        .route("/api/group/polish", post(grouping::polish))
        .route("/api/group/constraints", get(grouping::constraints_meta))
        .layer(tower_http::cors::CorsLayer::permissive());

    /* 前端静态资源（生产单端口模式） */
    let dist_dir = std::env::var("WEB_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../../web/dist"));
    let index_html = dist_dir.join("index.html");
    let has_dist = index_html.exists();

    let app = if has_dist {
        tracing::info!(dir = %dist_dir.display(), "静态目录已挂载");
        api.fallback_service(ServeDir::new(&dist_dir).not_found_service(ServeFile::new(index_html)))
    } else {
        tracing::warn!(dir = %dist_dir.display(), "没有前端构建产物，仅提供 API（先跑 pnpm --filter @pk/web build）");
        api
    };

    let app = app.with_state(st);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8787);
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("pk-server 启动 http://{addr}（管理员 /  ·  对外展示 /s/:id）");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
