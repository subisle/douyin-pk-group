//! pk-server：抖音 PK 挑战赛分组服务
//!
//! - 管理员后台 API：/api/*
//! - 对外只读展示页：/s/:planId（静态前端 + SPA 回退）

use pk_server::state::AppState;
use pk_server::*;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::routing::{delete, get, post, put};
use axum::Router;

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

    /* 显式迁移子命令：`pk-server migrate`（docs/00 §4 决策：DDL 不随启动自动跑） */
    if std::env::args().any(|a| a == "migrate") {
        let ep = config::load_migrator()?;
        tracing::info!(
            user = %ep.user,
            host = %ep.host,
            database = %ep.database,
            "以 douyin_migrator 账号执行显式迁移"
        );
        let applied = migrate::run(&ep).await?;
        match applied.is_empty() {
            true => println!("没有待应用的迁移。"),
            false => println!("已应用: {}", applied.join(", ")),
        }
        // 单库化（2026-10-10）：把旧 SQLite 的运行时数据（账号/方案/别名/配置/会话/
        // 绑定审计）幂等搬进 MySQL；搬过一次（config.sqlite_carried=1）就跳过。
        let pool = writer::connect(&ep).await?;
        store::carry_legacy_sqlite(&pool, &config::sqlite_path()).await?;
        return Ok(());
    }

    /* MySQL 只读数据源 */
    let sources = config::load_sources()?;
    let pools = mysql::build_pools(&sources).await?;
    let default_source = sources
        .iter()
        .find(|s| s.id == "LOCAL")
        .or_else(|| sources.first());
    let active = std::env::var("DB_SOURCE")
        .ok()
        .filter(|id| sources.iter().any(|s| &s.id == id));
    let active = active.unwrap_or_else(|| {
        default_source
            .expect("load_sources guarantees a source")
            .id
            .clone()
    });

    /* douyin_writer 业务写池（B 阶段：导入 / 主播台账写路径）。
    连不上不让服务挂：只读功能与管理页照常，管理写接口返回 503。 */
    let writer_pool = match config::load_writer() {
        Ok(ep) => match writer::connect(&ep).await {
            Ok(p) => {
                tracing::info!(user = %ep.user, host = %ep.host, database = %ep.database, "写池已连接（douyin_writer）");
                Some(p)
            }
            Err(e) => {
                tracing::error!(error = %e, "写池连接失败，管理写接口将不可用（只读功能不受影响）");
                None
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "未配置写库，管理写接口将不可用");
            None
        }
    };

    /* MIGRATE_ON_BOOT=true 时启动即迁移（默认关闭，需要 douyin_migrator 账号） */
    if config::migrate_on_boot() {
        match config::load_migrator() {
            Ok(ep) => match migrate::run(&ep).await {
                Ok(applied) if applied.is_empty() => tracing::info!("启动迁移：没有待应用版本"),
                Ok(applied) => tracing::info!(applied = applied.join(","), "启动迁移完成"),
                Err(e) => tracing::error!(error = %e, "启动迁移失败（服务继续启动）"),
            },
            Err(e) => {
                tracing::warn!(error = %e, "MIGRATE_ON_BOOT 已开启但没有 migrator 账号，跳过")
            }
        }
    }

    let st = Arc::new(AppState {
        auth: Arc::new(auth::Auth::from_env()),
        wave_ttl: repos::ttl_from_env(),
        sources,
        pools,
        writer: writer_pool,
        active: Mutex::new(active),
        cache: Mutex::new(Default::default()),
        bots: std::sync::OnceLock::new(),
    });

    /* 内置主播名册：person 表为空（全新部署）时自动装载 105 人种子数据 */
    roster_seed::seed_if_empty(&st).await;

    /* 内置展示端账号（shuai/shuai，账号=密码、管理员观看账号）：每次启动补齐。
    换库 / 全新部署 / 库被清空都不怕；已有同名账号只补 is_admin，不动运营改过的密码。 */
    let (builtin_created, builtin_skipped) = store::ensure_builtin_my_accounts(&st).await;
    if builtin_created > 0 {
        tracing::info!(created = builtin_created, "内置展示端账号已写入");
    }
    if builtin_skipped > 0 {
        tracing::warn!(skipped = builtin_skipped, "有内置展示端账号没写成功，原因见上面的日志");
    }

    /* 机器人模块（E 阶段）：配置了凭据的服务启动即自动连接（用户要求：启动 QQ 机器人都要启动）。
    QQ：AppID/Secret 齐 → 自动连 gateway；微信：SQLite 有登录 token → 自动恢复轮询，
    无 token 停在等待扫码（不影响启动）。失败只告警，服务照常起。 */
    let bot_manager = Arc::new(bot::BotManager::from_env(st.clone()).await);
    if let Some(qq) = bot_manager.qq.read().expect("qq 锁 poisoned").clone() {
        if let Err(e) = Arc::clone(&qq).start().await {
            tracing::warn!(error = %e, "QQ 机器人自动启动失败");
        }
    }
    if let Some(wx) = &bot_manager.wechat {
        if let Err(e) = Arc::clone(wx).start().await {
            tracing::warn!(error = %e, "微信机器人未自动启动（等待扫码或未配置）");
        }
    }
    st.bots
        .set(bot_manager)
        .map_err(|_| anyhow::anyhow!("bots 重复初始化"))?;

    /* 对外公开的只读端点：不要鉴权，单独一层 */
    let public = Router::new()
        .route("/api/public/plans", get(public::plans))
        .route("/api/public/plans/{id}", get(public::plan))
        // 阶段 D：团别公开站点（/st/m、/st/f 的数据接口）
        .route("/api/public/teams/{team}", get(public_teams::site))
        .route("/api/public/teams/{team}/daily", get(public_teams::daily))
        .route(
            "/api/public/teams/{team}/lineage",
            get(public_teams::lineage),
        )
        .route(
            "/api/public/teams/{team}/monthly",
            get(public_teams::monthly),
        )
        .route("/api/public/teams/{team}/yearly", get(public_teams::yearly))
        .route(
            "/api/public/teams/{team}/verify",
            post(public_teams::verify),
        )
        .route("/api/public/teams/{team}/my", get(public_teams::my))
        // 个人近 N 日音浪/时长序列（只下发本人）
        .route(
            "/api/public/teams/{team}/my/series",
            get(public_teams::my_series),
        )
        // 登录门轻量身份 + 抖音号绑定申请（展示端，本人 token）
        .route("/api/public/teams/{team}/my/who", get(public_teams::my_who))
        .route(
            "/api/public/teams/{team}/my/claim",
            get(public_teams::my_claim_get).post(public_teams::my_claim_post),
        )
        // 我的页面 · 微信推送（绑定机器人 + 追分目标 + 每日报告开关，2026-10-09）
        .route(
            "/api/public/teams/{team}/my/push",
            get(public_teams::my_push),
        )
        .route(
            "/api/public/teams/{team}/my/push/code",
            post(public_teams::my_push_code),
        )
        .route(
            "/api/public/teams/{team}/my/push/settings",
            post(public_teams::my_push_settings),
        )
        .route(
            "/api/public/teams/{team}/my/push/unbind",
            post(public_teams::my_push_unbind),
        )
        .route(
            "/api/public/teams/{team}/my/push/targets",
            post(public_teams::my_push_target_add),
        )
        .route(
            "/api/public/teams/{team}/my/push/targets/delete",
            post(public_teams::my_push_target_del),
        )
        .route(
            "/api/public/teams/{team}/my/push/search",
            get(public_teams::my_push_search),
        )
        // 机器人加好友二维码（管理员后台上传，展示端只读图片）
        .route(
            "/api/public/teams/{team}/my/push/qr",
            get(public_teams::my_push_qr),
        )
        // 对外展示页（静态 HTML，粉丝/主播手机直开）：/1 男团、/0 女团（好记），/st/m、/st/f 为别名
        // 子路径（/report、/group/junior、/lineage…）也是同一页，前端 history 路由接管
        .route("/1", get(public_teams::st_m))
        .route("/0", get(public_teams::st_0))
        .route("/st/{team}", get(public_teams::st_page))
        .route("/st/{team}/{*rest}", get(public_teams::st_page_sub))
        // 抖音号绑定截图走 JSON dataURL，放宽请求体上限（axum 默认 2MB）
        .layer(axum::extract::DefaultBodyLimit::max(4 * 1024 * 1024));

    /* API */
    let api = Router::new()
        .merge(public)
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/session", get(auth::session))
        // 数据变更推送（SSE）：后台页面据此自动刷新，不再轮询
        .route("/api/admin/events", get(events::sse))
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
        .route("/api/group/constraints", get(grouping::constraints_meta))
        // B 阶段管理端数据域：主播台账 + 导入三步流（写走 douyin_writer 池）
        .route(
            "/api/admin/streamers",
            get(admin::streamers).post(admin::create_streamer),
        )
        .route(
            "/api/admin/streamers/{id}",
            axum::routing::put(admin::update_streamer).delete(admin::delete_streamer),
        )
        .route("/api/admin/import/upload", post(admin::import_upload))
        .route("/api/admin/import/preview", post(admin::import_preview))
        .route("/api/admin/import/commit", post(admin::import_commit))
        .route(
            "/api/admin/import/day-status",
            get(admin::import_day_status),
        )
        .route("/api/admin/import/batches", get(admin::import_batches))
        .route("/api/admin/import/day", delete(admin::import_day_delete))
        // E 阶段机器人：状态 / 启停 / 微信扫码登录
        .route("/api/admin/bots/status", get(bot::bots_status))
        .route("/api/admin/bots/qq/start", post(bot::qq_start))
        .route("/api/admin/bots/qq/stop", post(bot::qq_stop))
        .route(
            "/api/admin/bots/qq/credentials",
            get(bot::qq_credentials).post(bot::qq_save_credentials),
        )
        .route("/api/admin/bots/weixin/start", post(bot::weixin_start))
        .route("/api/admin/bots/weixin/stop", post(bot::weixin_stop))
        .route("/api/admin/bots/weixin/qrcode", get(bot::weixin_qrcode))
        .route(
            "/api/admin/bots/weixin/qrcode/check",
            post(bot::weixin_qrcode_check),
        )
        .route("/api/admin/bots/weixin/token", post(bot::weixin_token))
        // 机器人「加好友」二维码：管理端上传，展示端「我的」页显示给用户扫
        .route(
            "/api/admin/bots/weixin/add-qr",
            get(bot::weixin_add_qr_get).post(bot::weixin_add_qr_set),
        )
        .route("/api/admin/bots/sessions", get(bot::bots_sessions))
        .route(
            "/api/admin/bots/sessions/admin",
            post(bot::bots_session_admin),
        )
        // 3328 移植：指标聚合 + 主播管理增强
        .route("/api/admin/dashboard", get(metrics::dashboard))
        .route("/api/admin/metrics/monthly", get(metrics::monthly))
        .route("/api/admin/metrics/yearly", get(metrics::yearly))
        .route(
            "/api/admin/streamers/duplicates",
            get(admin::streamer_duplicates),
        )
        .route(
            "/api/admin/streamers/{id}/accounts",
            post(admin::bind_account),
        )
        .route(
            "/api/admin/streamers/{id}/detail",
            get(admin::streamer_detail),
        )
        .route("/api/admin/streamers/{id}/master", put(admin::set_master))
        .route(
            "/api/admin/streamers/batch-delete",
            post(admin::batch_delete),
        )
        .route("/api/admin/streamers/{id}/merge", post(admin::merge_person))
        .route(
            "/api/admin/streamers/master-batch",
            post(admin::master_batch),
        )
        .route("/api/admin/report/daily.svg", get(report::daily_svg))
        .route(
            "/api/admin/public/settings",
            get(public_teams::get_settings).post(public_teams::update_settings),
        )
        // 展示端「我的」页面账号（手动设置：姓名 + 纯数字密码，手机号选填）
        .route(
            "/api/admin/my-accounts",
            get(public_teams::admin_my_accounts).post(public_teams::admin_my_account_set),
        )
        .route(
            "/api/admin/my-accounts/{name}",
            get(public_teams::admin_my_account_get).delete(public_teams::admin_my_account_delete),
        )
        // 抖音号绑定审计（展示端提交 + 截图凭证 → 管理端裁决）
        .route(
            "/api/admin/anchor-claims",
            get(public_teams::admin_anchor_claims),
        )
        .route(
            "/api/admin/anchor-claims/{id}/screenshot",
            get(public_teams::admin_anchor_claim_screenshot),
        )
        .route(
            "/api/admin/anchor-claims/{id}/decide",
            post(public_teams::admin_anchor_claim_decide),
        )
        .route("/api/admin/collect/status", get(admin::collect_status));

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
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}
