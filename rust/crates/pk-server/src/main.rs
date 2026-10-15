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
    // Docker HEALTHCHECK 用：探活自身，不连数据库（P2-21：打一条日志便于排查误用）
    if std::env::args().any(|a| a == "--health-check") {
        tracing_subscriber::fmt()
            .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
            .init();
        tracing::info!("--health-check：进程可运行（按约定不连库，返回即成功）");
        return Ok(());
    }

    // .env 值里带空格和中文，shell source 和 dotenvy 都处理不了，自己解析。
    // apps/server/.env 随过渡期退役了，现在按顺序找第一个存在的文件：
    // 显式 ENV_FILE / DOTENV_PATH → rust/.env（cargo run 就在 rust/ 下）→ 仓库根 → /app/.env（容器）
    let env_path = std::env::var("ENV_FILE")
        .or_else(|_| std::env::var("DOTENV_PATH"))
        .ok()
        .filter(|p| !p.is_empty())
        .or_else(|| {
            [
                ".env",
                "../.env",
                "/app/.env",
                "../deploy/.env",
                "deploy/.env",
            ]
            .iter()
            .map(PathBuf::from)
            .find(|p| p.exists())
            .map(|p| p.display().to_string())
        })
        .unwrap_or_else(|| ".env".to_string());
    if let Ok(p) = std::fs::canonicalize(&env_path) {
        tracing::info!(path = %p.display(), "已加载 .env");
    } else {
        tracing::info!(path = %env_path, "没有找到 .env，全部配置走环境变量");
    }
    config::load_dotenv(&env_path);
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    /* P1-5：无 ADMIN_PASSWORD 时拒绝启动。
    之前的行为是「整个 /api 不鉴权只打日志」，一旦按仓库资产部署（HOST=0.0.0.0
    + compose 直接发布端口），管理 API 就裸奔在公网。现在默认拒绝启动；
    本地开发确实不想配口令时，显式设 ALLOW_NO_ADMIN_PASSWORD=1 放行。 */
    if !auth::Auth::password_configured()
        && std::env::var("ALLOW_NO_ADMIN_PASSWORD").as_deref() != Ok("1")
    {
        eprintln!(
            "拒绝启动：未设置 ADMIN_PASSWORD（管理 API 将完全无鉴权）。\n\
             - 正常部署：在 .env / 环境里配置 ADMIN_PASSWORD\n\
             - 本地开发确实不需要口令：显式设 ALLOW_NO_ADMIN_PASSWORD=1"
        );
        anyhow::bail!("未设置 ADMIN_PASSWORD（设置 ALLOW_NO_ADMIN_PASSWORD=1 可显式放行）");
    }
    if !auth::Auth::password_configured() {
        tracing::warn!("ALLOW_NO_ADMIN_PASSWORD=1：本次启动 /api 不鉴权（仅限本地开发）");
    }

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
        // 绑定审计）幂等搬进业务库；搬过一次（config.sqlite_carried=1）就跳过。
        let pool = writer::connect(&ep).await?;
        store::carry_legacy_sqlite(&pool, &config::sqlite_path()).await?;
        return Ok(());
    }

    /* PostgreSQL 只读数据源 */
    let sources = config::load_sources()?;
    let pools = pgrs::build_pools(&sources).await?;
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

    /* Redis 缓存 / 锁池（P0，docs/17 §7.3）。未配置 REDIS_URL = 全内存模式（不告警）；
    配了但连不上只 warn，不阻断启动 —— 与上面写池失败的处理风格一致。
    真连不上时由 /api/health 报 redis:"down"，各调用点静默回落内存 + 直查 PG。 */
    let redis_pool = store::cache::init_pool();

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
        redis: redis_pool,
        active: Mutex::new(active),
        cache: Mutex::new(Default::default()),
        bots: std::sync::OnceLock::new(),
    });
    /* Redis 30s PING 探针（P0）：刷新 store::cache 内部的降级标志，断开/恢复各留一条日志。
    未配置 REDIS_URL 时不启动；探针只做「刷新 + 记录」，不参与任何业务判断。 */
    if st.redis.is_some() {
        let probe_st = st.clone();
        tokio::spawn(async move {
            let token = push::shutdown_token();
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            tick.tick().await; // 首拍立即返回：吃掉它，首探放到 30s 后
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = tick.tick() => {
                        let s = store::cache::status(&probe_st).await;
                        tracing::debug!(
                            redis = s,
                            degraded = store::cache::degraded(),
                            "Redis 探针"
                        );
                    }
                }
            }
        });
    }
    /* 内置主播名册：person 表为空（全新部署）时自动装载 105 人种子数据 */
    roster_seed::seed_if_empty(&st).await;

    /* 内置展示端账号（shuai/shuai，账号=密码、管理员观看账号）：每次启动补齐。
    换库 / 全新部署 / 库被清空都不怕；已有同名账号只补 is_admin，不动运营改过的密码。 */
    let (builtin_created, builtin_skipped) = store::ensure_builtin_my_accounts(&st).await;
    if builtin_created > 0 {
        tracing::info!(created = builtin_created, "内置展示端账号已写入");
    }
    if builtin_skipped > 0 {
        tracing::warn!(
            skipped = builtin_skipped,
            "有内置展示端账号没写成功，原因见上面的日志"
        );
    }

    /* 机器人模块（E 阶段）：配置了凭据的服务启动即自动连接（用户要求：启动 QQ 机器人都要启动）。
    QQ：AppID/Secret 齐 → 自动连 gateway；微信：库里有登录 token → 自动恢复轮询，
    无 token 停在等待扫码（不影响启动）。失败只告警，服务照常起。 */
    let bot_manager = Arc::new(bot::BotManager::from_env(st.clone()).await);
    let qq = bot_manager.qq.read().expect("qq 锁 poisoned").clone();
    if let Some(qq) = qq {
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

    /* 用户自助扫码的微信推送通道（2026-10-11，多账号）：恢复每个已扫码账号的长轮询，
    并起一条 supervisor 每 30 秒扫表补拉（任务 panic / 异常退出会被重新拉起）。 */
    crate::wxpush::bootstrap(&st).await;
    let wxpush_supervisor = tokio::spawn(crate::wxpush::supervisor_loop(st.clone()));

    /* 延迟推送调度器（2026-10-10）：每 60 秒扫一次 push_jobs，把到点的任务推出去。
    写库不可用时 push_job_take 直接返回 None，循环空转，不影响服务。
    P1-20：调度循环监听全局 shutdown token，graceful shutdown 时随之退出。 */
    let scheduler = tokio::spawn(push::scheduler_loop(st.clone()));

    /* 对外公开的只读端点：不要鉴权，单独一层 */
    /* 展示端公开 API（2026-10-10 门禁）：只有河南 / 安徽访客放行，
    放行 / 拒绝都写访问日志（access_logs，后台「访客日志」页）。 */
    let public_api = Router::new()
        .route("/api/public/plans", get(public::plans))
        .route("/api/public/plans/{id}", get(public::plan))
        // 公告系统 v2（2026-10-13）：登录页公告 + 庆祝配置，无需登录。
        // 登录门出现时用户还没登录，site() 过不了登录门拿不到配置，
        // 所以登录页走这个轻量端点（只下发 login 公告位 + celebrate，不带 steps）。
        .route(
            "/api/public/announcements",
            get(public_teams::public_announcements),
        )
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
        // 我的抖音号：每个号的日/月/年/累计音浪（2026-10-12，一人多号时看出哪个号在跑）
        .route(
            "/api/public/teams/{team}/star",
            get(public_teams::star_of_day),
        )
        .route(
            "/api/public/teams/{team}/my/accounts",
            get(public_teams::my_accounts),
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
        // 追人目标批量多选（契约 B 节 2026-10-15）：与上面单条添加入口同组、同鉴权口径
        // （token 走 X-My-Token 头，handler 内 my_ident 校验登录态 + 团别）。
        .route(
            "/api/public/teams/{team}/my/push/targets/batch",
            post(public_teams::my_push_targets_batch),
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
        .route(
            "/api/public/teams/{team}/my/push/wechat/start",
            post(public_teams::my_push_wechat_start),
        )
        .route(
            "/api/public/teams/{team}/my/push/wechat/poll",
            post(public_teams::my_push_wechat_poll),
        )
        // 展示端登录 / 密码（2026-10-14 口径：注册功能已彻底移除，保留忘记密码重置，
        // 新增已登录后的自助改密）。这几个接口不带团别。
        .route("/api/public/my/login", post(public_teams::my_login))
        .route(
            "/api/public/my/password",
            post(public_teams::my_password_set),
        )
        .route(
            "/api/public/my/password/reset",
            post(public_teams::my_password_reset),
        )
        .route(
            "/api/public/my/bind/search",
            get(public_teams::my_bind_search),
        )
        .route("/api/public/my/bind", post(public_teams::my_bind))
        .layer(axum::middleware::from_fn_with_state(
            st.clone(),
            ipinfo::ip_gate_mw,
        ));
    /* HTML 页面也拦（2026-10-11）：外省访客打开页面直接看到「暂不支持访问」提示页，
    只有安徽 / 河南（以及白名单例外 IP）能进页面。 */
    let public_pages = Router::new()
        // 对外展示页（静态 HTML，粉丝/主播手机直开）：/ 为唯一入口（2026-10-11 起
        // 不分男女、不弹团别选择卡），/1、/0、/st/m、/st/f 为兼容旧链接的别名；
        // 子路径（/report、/lineage、/my、/group/*）也是同一页，前端 history 路由接管
        .route("/", get(public_teams::st_root))
        .route("/report", get(public_teams::st_root))
        .route("/lineage", get(public_teams::st_root))
        .route("/my", get(public_teams::st_root))
        .route("/group", get(public_teams::st_root))
        .route("/group/{key}", get(public_teams::st_root))
        .route("/1", get(public_teams::st_m))
        .route("/0", get(public_teams::st_0))
        .route("/st/{team}", get(public_teams::st_page))
        .route("/st/{team}/{*rest}", get(public_teams::st_page_sub))
        .layer(axum::middleware::from_fn_with_state(
            st.clone(),
            ipinfo::ip_gate_page_mw,
        ));

    let public = public_api
        .merge(public_pages)
        // 抖音号绑定截图走 JSON dataURL，放宽请求体上限（axum 默认 2MB）
        .layer(axum::extract::DefaultBodyLimit::max(4 * 1024 * 1024));

    /* API */
    let api = Router::new()
        .merge(public)
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/session", get(auth::session))
        // 2026-10-12 审计 L1：handler 早就有（长效 token 换一次性 ticket，给
        // EventSource/<img> 等带不了 Authorization 头的场景），路由一直漏挂，
        // 前端日报图 URL 因此 401。补上。
        .route("/api/auth/ticket", post(auth::issue_ticket))
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
        // 月度时长（平台月报整月导入：每日不能导入时长，只有月时长）
        .route(
            "/api/admin/import/month/preview",
            post(admin::import_month_preview),
        )
        .route(
            "/api/admin/import/month/commit",
            post(admin::import_month_commit),
        )
        .route(
            "/api/admin/import/month-status",
            get(admin::import_month_status),
        )
        .route(
            "/api/admin/import/month",
            delete(admin::import_month_delete),
        )
        // 暂存区：有抖音号/ID 但还没建档的导入行（不进榜单和统计，建档/绑号后自动补账）
        .route(
            "/api/admin/import/pending",
            get(admin::import_pending_list).delete(admin::import_pending_delete),
        )
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
        // 延迟推送队列（2026-10-10）：查看排队情况 + 立刻催一次
        .route("/api/admin/push/jobs", get(bot::push_jobs))
        .route("/api/admin/push/run", post(bot::push_run_now))
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
            "/api/admin/streamers/{id}/accounts/{accountId}",
            axum::routing::delete(admin::unbind_account),
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
        // 管理员改绑主播（用户自己不能解绑/换绑）
        .route(
            "/api/admin/my-accounts/{name}/bind",
            post(public_teams::admin_my_account_bind),
        )
        // 批量开号（2026-10-11 docs/13）：一次给多个在职主播开「展示端登录账号」，
        // 初始密码 = 手机号后四位；单条 /api/admin/my-accounts 保持原样（向后兼容）
        .route(
            "/api/admin/my-accounts/batch",
            post(admin::batch_my_accounts),
        )
        // 主播手机号单独改（016 起 person.phone；要过三道查重，失败给准确原因）
        .route(
            "/api/admin/streamers/{id}/phone",
            axum::routing::put(admin::set_person_phone),
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
        // 展示端主播绑定申请的裁决（2026-10-12 审计 L1 同款遗漏：
        // handler 与导出都在，路由一直没挂，申请永远停在 pending）
        .route(
            "/api/admin/my-bind-requests",
            get(public_teams::admin_my_bind_requests),
        )
        .route(
            "/api/admin/my-bind-requests/{id}/approve",
            post(public_teams::admin_my_bind_request_approve),
        )
        .route(
            "/api/admin/my-bind-requests/{id}/reject",
            post(public_teams::admin_my_bind_request_reject),
        )
        .route("/api/admin/collect/status", get(admin::collect_status))
        // 访客日志（2026-10-10）：展示端访问 / 登录记录，含外省被拒的
        .route("/api/admin/visits", get(ipinfo::admin_visits))
        // IP 归属地现场查询（2026-10-11，诊断用）：走 IP_QUERY_SOURCES 集，绕过 ip_geo_cache
        .route("/api/admin/ip-lookup", get(ipinfo::admin_ip_lookup))
        // 展示端访问 IP 白名单（2026-10-11）：表非空即白名单授权模式
        .route(
            "/api/admin/ip-allowlist",
            get(ipinfo::admin_ip_allowlist_list).post(ipinfo::admin_ip_allowlist_add),
        )
        .route(
            "/api/admin/ip-allowlist/{id}",
            delete(ipinfo::admin_ip_allowlist_delete),
        )
        // 公告弹窗（2026-10-11）：展示端打开时弹出的公告，内容由后台配置
        .route(
            "/api/admin/announcement",
            get(public_teams::get_announcement).post(public_teams::update_announcement),
        )
        // 管理员消息下发（docs/23 §5）：收件人解析 + outbox 队列 + 限速 worker
        // task-14：这条路由收自定义上传图（base64），要按单条路由放宽 body limit
        // （全局是 4MB，5MB 原图 base64 后 ≈6.7MB）。DefaultBodyLimit 是往请求
        // extensions 里 insert，内层覆盖外层，所以这里比全局那层更靠内 → 生效 8MB。
        .route(
            "/api/admin/send",
            post(admin_send::send_create)
                .layer(axum::extract::DefaultBodyLimit::max(admin_send::SEND_BODY_LIMIT)),
        )
        .route("/api/admin/send/targets", get(admin_send::send_targets))
        .route("/api/admin/send/messages", get(admin_send::send_messages))
        .route(
            "/api/admin/send/messages/{id}",
            get(admin_send::send_message_detail),
        )
        .route(
            "/api/admin/send/enable-lineage",
            post(admin_send::enable_lineage),
        )
        // 消息下发 · 展示端弹窗渠道（docs/23 §6，X-My-Token 鉴权在 handler 内做，
        // 路径在 auth.rs 的 /api/public/ 免鉴权前缀内）
        .route("/api/public/my/messages", get(admin_send::my_messages))
        .route(
            "/api/public/my/messages/{id}/read",
            post(admin_send::my_message_read),
        )
        .route(
            "/api/public/my/messages/{id}/image.png",
            get(admin_send::my_message_image),
        );

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
    // apps/web（旧 hash 路由后台）已随 2026-10 的收敛退役，镜像里只有
    // apps/douyin-web/dist；默认值跟着改，本地 `cargo run` 不再 mount 一个
    // 永远不存在的 ../../web/dist。
    let dist_dir = std::env::var("WEB_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../../apps/douyin-web/dist"));
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
            tracing::warn!(dir = %dist_dir.display(), "没有前端构建产物，仅提供 API（先跑 pnpm --filter @pk/douyin-web build）");
            api
        }
    }
    // 2026-10-12 安全审计 L4：基础安全响应头（全站，含静态页）。
    // XFO/CSP frame-ancestors 防点击劫持，nosniff 防 MIME 嗅探，referrer 收敛。
    .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
        axum::http::header::X_FRAME_OPTIONS,
        axum::http::HeaderValue::from_static("DENY"),
    ))
    .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        axum::http::HeaderValue::from_static("nosniff"),
    ))
    .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
        axum::http::header::CONTENT_SECURITY_POLICY,
        axum::http::HeaderValue::from_static("frame-ancestors 'none'"),
    ))
    .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
        axum::http::header::REFERRER_POLICY,
        axum::http::HeaderValue::from_static("strict-origin-when-cross-origin"),
    ));

    // 登录态持久化（2026-10-12）：把 AppState 注册给 token 函数，
    // my token put/take/revoke 由此落到 Redis——容器重启后 24h 内登录态自动恢复。
    public_teams::admin::register_app_state(st.clone());

    let app = app.with_state(st.clone());

    /* 2026-10-14 审计 P2：4MB 请求体上限提到**最外层**。
    原先只挂在 public 子路由上 —— axum 的 layer 只包裹「注册于它之前」的路由，
    所以 /api/admin/import/upload 等管理端上传口一直在用 axum 默认的 2MB，
    而 admin.rs 的 413 文案写的是「文件超过 4MB 上限」，2MB 就被拒还谎报 4MB。
    统一 4MB（管理端 CSV/Excel 上传需要），文案与实际阈值一致。 */
    let app = app.layer(axum::extract::DefaultBodyLimit::max(4 * 1024 * 1024));

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8787);
    // P1-5：默认只听回环地址。生产要对外时必须显式设 HOST=0.0.0.0
    //（且正常情况下公网入口由 Caddy 反代收口，后端不该直接暴露）。
    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let addr: std::net::SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|_| anyhow::anyhow!("无效的监听地址: {host}:{port}"))?;
    tracing::info!("pk-server 启动 http://{addr}（管理员 /admin  ·  对外展示 /）");
    tracing::info!(
        auth = if st.auth.enabled() {
            "已开启（需口令）"
        } else {
            "未开启（公开）"
        },
        "管理员鉴权状态"
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;

    /* P1-20：优雅退出 —— SIGTERM / Ctrl+C 后停止接受新连接，等在途请求结束
    （含 SSE；axum graceful wait 有限流，不会永远等）再退出。调度循环由
    shutdown token 通知退出。systemctl restart / compose recreate 时不再硬断。 */
    let shutdown = async {
        let ctrl_c = async {
            let _ = tokio::signal::ctrl_c().await;
        };
        #[cfg(unix)]
        let terminate = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut sig) => {
                    sig.recv().await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "SIGTERM 监听失败（优雅退出仅剩 Ctrl+C）");
                    std::future::pending::<()>().await;
                }
            }
        };
        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();
        tokio::select! {
            _ = ctrl_c => tracing::info!("收到 Ctrl+C，开始优雅退出"),
            _ = terminate => tracing::info!("收到 SIGTERM，开始优雅退出"),
        }
        // 通知调度循环退出（它最多在当前 tick 收尾后停止）
        push::shutdown_token().cancel();
    };

    let serve = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown);

    match serve.await {
        Ok(()) => {
            tracing::info!("HTTP 服务已停止接受连接，等待后台任务退出…");
            // 调度循环收到 token 后退出（最多等一轮 tick 的收尾时间）
            let _ = tokio::time::timeout(std::time::Duration::from_secs(10), scheduler).await;
            // wxpush supervisor 收到同一个 token 后退出；不等它，轮询任务自己会在
            // 等待点退出并把 cursor flush 落库（硬超时兜底 10 秒）。
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(10), wxpush_supervisor).await;
            tracing::info!("pk-server 已退出");
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}
