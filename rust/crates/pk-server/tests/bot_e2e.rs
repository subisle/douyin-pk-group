//! 机器人端到端集成测试：真实 AppState + 真库（需要 DB_LOCAL_* / DB_WRITER_* 环境变量）。
//! 未配置数据库时自动跳过（本地单测不依赖库）。
//!
//! 覆盖：QQ 免管理员、微信内置口令提权/非管理员静默、帮助、状态、导入（带指定日期）、导出 CSV、排行、日报、缺数据、
//! 已绑定普通用户权限矩阵（帮助只列 5 条 / 管理员命令与附件、日期被拒 / 推送开关只动 daily_report）。

use std::sync::Arc;

use pk_server::bot::{self, BotConfig, Inbound};
use pk_server::{config, pgrs, state::AppState};

/// 测试用管理员口令（2026-10-11 审计整改：不再依赖任何硬编码口令）。
/// 每进程生成一次随机值并写进 env：WECHAT_ADMIN_CODE 供 bot 私聊提权读取
/// （bot 侧 OnceLock 只在首次读一次），ADMIN_PASSWORD 供 Auth::from_env。
/// 每个用例都先经 setup() 走到这里，保证 env 在任何 bot 处理之前就绪。
fn ensure_test_admin_env() {
    static DONE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DONE.get_or_init(|| {
        let code = format!(
            "pk-test-admin-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_millis()
        );
        std::env::set_var("WECHAT_ADMIN_CODE", &code);
        std::env::set_var("ADMIN_PASSWORD", &code);
        code
    });
}

/// 本进程随机生成的微信提权口令（构造提权消息用）
fn test_admin_code() -> String {
    ensure_test_admin_env();
    std::env::var("WECHAT_ADMIN_CODE").expect("ensure_test_admin_env 已写入")
}

async fn setup() -> Option<Arc<AppState>> {
    ensure_test_admin_env();
    if std::env::var("DB_LOCAL_HOST").is_err() {
        eprintln!("跳过：未配置 DB_LOCAL_*（此测试需要真实测试库）");
        return None;
    }
    let sources = config::load_sources().expect("sources");
    // 防呆（2026-10-12）：e2e 会写库（建/删 __e2e 主播），绝不能对准生产。
    // 生产 MariaDB 的库名是 douyin —— 检测到直接拒绝，让测试显式指一个测试库。
    {
        let prod_hit = |h: &str, d: &str| {
            d == "douyin" && (h.contains("147.90") || h == "127.0.0.1" || h == "localhost")
        };
        for s in &sources {
            if prod_hit(&s.host, &s.database) {
                eprintln!("拒绝运行：数据源 {} 指向生产库（{} / {}）。e2e 测试必须用独立测试库（DB_LOCAL_NAME≠douyin）", s.id, s.host, s.database);
                return None;
            }
        }
        if let Ok(w) = config::load_writer() {
            if prod_hit(&w.host, &w.database) {
                eprintln!("拒绝运行：writer 指向生产库（{} / {}）。e2e 测试必须用独立测试库", w.host, w.database);
                return None;
            }
        }
    }
    let pools = pgrs::build_pools(&sources).await.expect("建池失败");
    let writer = match config::load_writer() {
        Ok(ep) => pk_server::writer::connect(&ep).await.ok(),
        Err(_) => None,
    };
    let active = sources
        .iter()
        .find(|s| s.id == "LOCAL")
        .or_else(|| sources.first())
        .expect("at least one source")
        .id
        .clone();
    Some(Arc::new(AppState {
        auth: Arc::new(pk_server::auth::Auth::from_env()),
        wave_ttl: std::time::Duration::from_secs(600),
        sources,
        pools,
        writer,
        // P0：测试不起 Redis（None = 全内存模式，与引入前一致）
        redis: None,
        active: std::sync::Mutex::new(active),
        cache: std::sync::Mutex::new(Default::default()),
        bots: std::sync::OnceLock::new(),
        wb: std::sync::OnceLock::new(),
    }))
}

fn cfg() -> BotConfig {
    BotConfig {
        qq_app_id: "test".into(),
        qq_app_secret: "test".into(),
        wechat_token: String::new(),
        qq_admins: vec!["ADMIN-CHAT".into()],
        wechat_admins: vec![],
        rate_limit_per_min: 100,
    }
}

fn inbound(text: &str) -> Inbound {
    Inbound {
        channel: "qq",
        conversation_id: "ADMIN-CHAT".into(),
        is_group: false,
        text: text.into(),
        attachments: vec![],
    }
}

fn inbound_stranger(text: &str) -> Inbound {
    Inbound {
        channel: "qq",
        conversation_id: "STRANGER-1".into(),
        is_group: true,
        text: text.into(),
        attachments: vec![],
    }
}

fn inbound_wechat_stranger(text: &str) -> Inbound {
    Inbound {
        channel: "weixin",
        conversation_id: "WECHAT-STRANGER".into(),
        is_group: false,
        text: text.into(),
        attachments: vec![],
    }
}

#[tokio::test]
async fn wechat_builtin_code_promotes_private_session() {
    let Some(st) = setup().await else { return };
    let cfg = cfg();
    let conversation_id = format!("WECHAT-CODE-{}", chrono::Utc::now().timestamp_millis());
    let inbound = Inbound {
        channel: "weixin",
        conversation_id: conversation_id.clone(),
        is_group: false,
        text: test_admin_code(),
        attachments: vec![],
    };
    let out = bot::BotRouter::handle(&st, &cfg, inbound)
        .await
        .expect("提权口令应有回复");
    assert_eq!(out.text, "已成为管理员");
    assert!(
        sqlx::query_scalar::<_, i8>(
            "SELECT is_admin FROM bot_sessions WHERE channel = 'weixin' AND conversation_id = ?",
        )
        .bind(&conversation_id)
        .fetch_one(st.writer.as_ref().expect("需要写池"))
        .await
        .expect("应查到已提权会话")
            == 1,
        "提权口令应写入管理员标记"
    );
}

/// 单独的会话 ID，避免和文本导入用例抢同一个待确认导入槽位（PENDING 是全局的）。
fn cfg_with(chat: &str) -> BotConfig {
    let mut c = cfg();
    c.qq_admins.push(chat.to_string());
    c
}

/// 附件白名单只放行 QQ CDN 域名，本地测试服务在 127.0.0.1 —— 跑之前追加放行。
/// 进程内 env 是全局的，多个测试各调一次无副作用。
fn allow_local_attachment_host() {
    let key = "BOT_ATTACHMENT_HOSTS";
    let cur = std::env::var(key).unwrap_or_default();
    for h in ["127.0.0.1", "localhost"] {
        if !cur.split(',').any(|s| s.trim() == h) {
            let next = if cur.is_empty() {
                h.to_string()
            } else {
                format!("{cur},{h}")
            };
            std::env::set_var(key, next);
        }
    }
}

fn inbound_from(chat: &str, file: &str, url: &str) -> Inbound {
    Inbound {
        channel: "qq",
        conversation_id: chat.into(),
        is_group: false,
        text: String::new(),
        attachments: vec![bot::Attachment {
            url: url.into(),
            file_name: file.into(),
        }],
    }
}

/// 起一个极小 HTTP 服务，模拟 QQ 附件直链（下载附件走的是一次普通 GET）。
async fn serve_text(body: &'static str) -> Option<String> {
    // 测试服务器在本机，而下载白名单只放行 QQ CDN 域名 —— 一律追加放行，
    // 否则所有走附件的用例都会被 download_text 挡在门外（假失败）。
    allow_local_attachment_host();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.ok()?;
    let addr = listener.local_addr().ok()?;
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/csv; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Some(format!("http://{addr}/data.csv"))
}

/// 清掉某个业务日期的测试数据：这些用例写的是真库，用固定日期，重复跑必须幂等，
/// 否则「值相同不覆盖」会让第二次跑变成「导入 0 行」，断言就假失败。
async fn clean_date(st: &Arc<AppState>, date: &str) {
    if let Some(pool) = st.writer.as_ref() {
        let _ = sqlx::query("DELETE FROM daily_metric WHERE biz_date = ?")
            .bind(date)
            .execute(pool)
            .await;
        let _ = sqlx::query("DELETE FROM import_batch WHERE biz_date = ?")
            .bind(date)
            .execute(pool)
            .await;
    }
}

/// 某业务日的 daily_metric 行数（权限用例断言「没落库」用）
async fn metric_rows(pool: &sqlx::PgPool, date: &str) -> i64 {
    sqlx::query(
        "SELECT COUNT(*) AS n FROM daily_metric WHERE DATE_FORMAT(biz_date, '%Y-%m-%d') = ?",
    )
    .bind(date)
    .map(|r: sqlx::postgres::PgRow| sqlx::Row::try_get::<i64, _>(&r, "n").unwrap_or(0))
    .fetch_one(pool)
    .await
    .unwrap_or(0)
}

/// 某业务日的 import_batch 行数（权限用例断言「没导入」用）
async fn batch_rows(pool: &sqlx::PgPool, date: &str) -> i64 {
    sqlx::query(
        "SELECT COUNT(*) AS n FROM import_batch WHERE DATE_FORMAT(biz_date, '%Y-%m-%d') = ?",
    )
    .bind(date)
    .map(|r: sqlx::postgres::PgRow| sqlx::Row::try_get::<i64, _>(&r, "n").unwrap_or(0))
    .fetch_one(pool)
    .await
    .unwrap_or(0)
}

/// push_bindings 的两个开关（(daily_report, notify_target)，i8 便于和 1/0 直接比）
async fn push_flags(pool: &sqlx::PgPool, name: &str) -> (i8, i8) {
    sqlx::query("SELECT daily_report, notify_target FROM push_bindings WHERE name = ?")
        .bind(name)
        .map(|r: sqlx::postgres::PgRow| {
            (
                sqlx::Row::try_get::<i8, _>(&r, "daily_report").unwrap_or(-1),
                sqlx::Row::try_get::<i8, _>(&r, "notify_target").unwrap_or(-1),
            )
        })
        .fetch_one(pool)
        .await
        .expect("应能查到 push_bindings 行")
}

/// 私聊发 CSV 文件 + 文件名带日期与团别 → 直接入库，不用再回一句话确认。
/// 用独立的 2019-03-13 当业务日期（不碰真实数据），开跑先清干净 → 可反复跑。
///
/// 附件下载有域名白名单（防 SSRF），测试服务器是 127.0.0.1，必须显式放行，
/// 否则 download_text 直接拒绝 —— 2026-10-10 白名单上线后这个用例就一直是假失败。
#[tokio::test]
async fn csv_attachment_with_date_and_team_hints_is_imported() {
    let Some(st) = setup().await else { return };
    allow_local_attachment_host();
    clean_date(&st, "2019-03-13").await;
    let cfg = cfg_with("CSV-USER-A");
    let Some(url) = serve_text("浩龙,77777\n南方楠,66666\n").await else {
        return;
    };
    let out = bot::BotRouter::handle(
        &st,
        &cfg,
        inbound_from("CSV-USER-A", "2019-03-13-男团.csv", &url),
    )
    .await
    .expect("附件导入应有回复");
    assert!(out.text.contains("导入成功 2 行"), "实际：{}", out.text);
    assert!(out.text.starts_with('✅'), "实际：{}", out.text);
    clean_date(&st, "2019-03-13").await;
}

/// 用户口径（2026-10-10）：先发「10.1」报业务日期 → 再发 CSV，**不分男女团**，
/// 一个文件里男女混着也能导；两个文件都按报过的日期入库（日期黏住，不用每个文件都报）。
#[tokio::test]
async fn report_date_then_two_files_import_team_agnostic() {
    let Some(st) = setup().await else { return };
    let Some(pool) = st.writer.clone() else {
        return;
    };
    let date = "2019-03-16";
    clean_date(&st, date).await;

    // 从真名册里挑一男一女，验证同一文件里混合团别
    let pick = |gender: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query("SELECT name FROM person WHERE deleted_at IS NULL AND gender = ? AND status <> 'left' ORDER BY id LIMIT 1")
                .bind(gender)
                .map(|r: sqlx::postgres::PgRow| sqlx::Row::try_get::<String, _>(&r, "name").unwrap_or_default())
                .fetch_optional(&pool)
                .await
                .ok()
                .flatten()
                .unwrap_or_default()
        }
    };
    let male = pick("male").await;
    let female = pick("female").await;
    assert!(!male.is_empty() && !female.is_empty(), "名册里要有一男一女");

    let cfg = cfg_with("CSV-USER-D");

    // ① 报日期（「10.1」这种写法）→ 机器人记下来
    let mut msg = inbound(date);
    msg.conversation_id = "CSV-USER-D".into();
    let ack = bot::BotRouter::handle(&st, &cfg, msg)
        .await
        .expect("报日期应有回复");
    assert!(ack.text.contains("已记下业务日期"), "实际：{}", ack.text);
    assert!(ack.text.contains(date), "实际：{}", ack.text);

    // ② 第一个文件（文件名不带任何提示；男女混在一个文件里）
    let Some(url1) = serve_text(Box::leak(
        format!("{male},111111\n{female},222222\n").into_boxed_str(),
    ))
    .await
    else {
        return;
    };
    let out1 = bot::BotRouter::handle(&st, &cfg, inbound_from("CSV-USER-D", "export.csv", &url1))
        .await
        .expect("文件导入应有回复");
    assert!(out1.text.contains("导入成功 2 行"), "实际：{}", out1.text);
    let head = out1.text.lines().next().unwrap_or_default();
    assert!(
        !head.contains("男团") && !head.contains("女团"),
        "不分男女团时标题不该带团别：{head}"
    );
    assert!(
        out1.text.contains(date),
        "回复要回显业务日期：{}",
        out1.text
    );

    // ③ 第二个文件：日期要黏住（不用再报一次）
    let Some(url2) = serve_text(Box::leak(format!("{male},333333\n").into_boxed_str())).await
    else {
        return;
    };
    let out2 = bot::BotRouter::handle(&st, &cfg, inbound_from("CSV-USER-D", "export2.csv", &url2))
        .await
        .expect("第二个文件也应有回复");
    assert!(out2.text.contains("导入成功 1 行"), "实际：{}", out2.text);

    // ④ 两个文件都落在报过的日期上，团别是 mixed，男女都进了
    let (rows, genders, team): (i64, i64, String) = (
        sqlx::query("SELECT COUNT(*) AS n FROM daily_metric WHERE DATE_FORMAT(biz_date,'%Y-%m-%d') = ?")
            .bind(date)
            .map(|r: sqlx::postgres::PgRow| sqlx::Row::try_get::<i64, _>(&r, "n").unwrap_or(0))
            .fetch_one(&pool)
            .await
            .unwrap_or(0),
        sqlx::query("SELECT COUNT(DISTINCT p.gender) AS n FROM daily_metric d JOIN person p ON p.id = d.person_id WHERE DATE_FORMAT(d.biz_date,'%Y-%m-%d') = ?")
            .bind(date)
            .map(|r: sqlx::postgres::PgRow| sqlx::Row::try_get::<i64, _>(&r, "n").unwrap_or(0))
            .fetch_one(&pool)
            .await
            .unwrap_or(0),
        sqlx::query("SELECT team FROM import_batch WHERE DATE_FORMAT(biz_date,'%Y-%m-%d') = ? ORDER BY id LIMIT 1")
            .bind(date)
            .map(|r: sqlx::postgres::PgRow| sqlx::Row::try_get::<String, _>(&r, "team").unwrap_or_default())
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_default(),
    );
    assert_eq!(rows, 2, "两个人两个文件共 2 行（同一人重复导入只更新）");
    assert_eq!(genders, 2, "男女混在一个文件里都要入库");
    assert_eq!(team, "mixed", "批次审计记 mixed");

    clean_date(&st, date).await;
}

/// 用户口径（2026-10-12）：「**如果同日期导入就是覆盖**」——
/// 同一天再导一次，值被**替换**（不是求和、也不是新增一行）。
/// 日期用远期过去的固定日期（不碰真实业务日），文件名带日期让入库日确定。
#[tokio::test]
async fn same_date_reimport_overwrites_value() {
    let Some(st) = setup().await else { return };
    let Some(pool) = st.writer.clone() else {
        return;
    };
    let date = "2019-03-21";
    clean_date(&st, date).await;

    // 挑一个**姓名唯一**在册主播：重名主播 resolve 会判歧义，跟本用例无关
    let name: String = sqlx::query(
        "SELECT p.name FROM person p WHERE p.deleted_at IS NULL AND p.status <> 'left' \
         AND (SELECT COUNT(*) FROM person p2 WHERE p2.name = p.name AND p2.deleted_at IS NULL) = 1 \
         ORDER BY p.id LIMIT 1",
    )
    .map(|r: sqlx::postgres::PgRow| sqlx::Row::try_get::<String, _>(&r, "name").unwrap_or_default())
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten()
    .unwrap_or_default();
    assert!(!name.is_empty(), "名册里要有一个姓名唯一的主播");

    let cfg = cfg_with("CSV-USER-OW");
    let file = format!("{date}.csv");

    // ① 第一次：1000
    let Some(url1) = serve_text(Box::leak(format!("{name},1000\n").into_boxed_str())).await else {
        return;
    };
    let out1 = bot::BotRouter::handle(&st, &cfg, inbound_from("CSV-USER-OW", &file, &url1))
        .await
        .expect("第一次导入应有回复");
    assert!(out1.text.contains("导入成功 1 行"), "实际：{}", out1.text);
    assert!(
        out1.text.contains(date),
        "回复要回显业务日期：{}",
        out1.text
    );

    // ② 第二次：同日期同一个人，改成 2500
    let Some(url2) = serve_text(Box::leak(format!("{name},2500\n").into_boxed_str())).await else {
        return;
    };
    let out2 = bot::BotRouter::handle(&st, &cfg, inbound_from("CSV-USER-OW", &file, &url2))
        .await
        .expect("第二次导入应有回复");
    assert!(
        out2.text.contains(date),
        "回复要回显业务日期：{}",
        out2.text
    );

    // ③ 同人同日只有一行，且值是最后一次的 2500（覆盖，不是 1000+2500）
    let (rows, wave): (i64, i64) = (
        sqlx::query(
            "SELECT COUNT(*) AS n FROM daily_metric d JOIN person p ON p.id = d.person_id \
             WHERE p.name = ? AND DATE_FORMAT(d.biz_date,'%Y-%m-%d') = ?",
        )
        .bind(&name)
        .bind(date)
        .map(|r: sqlx::postgres::PgRow| sqlx::Row::try_get::<i64, _>(&r, "n").unwrap_or(0))
        .fetch_one(&pool)
        .await
        .unwrap_or(0),
        sqlx::query(
            "SELECT COALESCE(SUM(d.wave), 0) AS w FROM daily_metric d JOIN person p ON p.id = d.person_id \
             WHERE p.name = ? AND DATE_FORMAT(d.biz_date,'%Y-%m-%d') = ?",
        )
        .bind(&name)
        .bind(date)
        .map(|r: sqlx::postgres::PgRow| sqlx::Row::try_get::<i64, _>(&r, "w").unwrap_or(0))
        .fetch_one(&pool)
        .await
        .unwrap_or(0),
    );
    assert_eq!(rows, 1, "同人同日只该有一行（覆盖，不是新增）");
    assert_eq!(wave, 2500, "值应是最后一次的 2500，不是 1000+2500=3500");

    clean_date(&st, date).await;
}

/// 用户口径（2026-10-12）：「11 号接收的就是 10 号的数据，**前提是没指定日期**」——
/// 正文和文件名都没日期时，业务日 = 收到日 − 1。
///
/// 用**虚构抖音号**跑这条用例：它匹配不到台账、落 `import_pending`（不写 daily_metric），
/// 所以既证明了日期口径，又不会污染真实「昨天」的业务数据。
#[tokio::test]
async fn attachment_without_any_date_uses_yesterday() {
    let Some(st) = setup().await else { return };
    let Some(pool) = st.writer.clone() else {
        return;
    };
    let yesterday = (chrono::Local::now().date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let no = "__e2e_nodate_76543210";

    // 幂等：先清掉本用例上一轮可能留下的暂存行与批次审计行
    let purge = |pool: sqlx::PgPool| async move {
        let _ = sqlx::query("DELETE FROM import_pending WHERE douyin_no = ?")
            .bind("__e2e_nodate_76543210")
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM import_batch WHERE filename = ?")
            .bind("机器人·主播榜.csv")
            .execute(&pool)
            .await;
    };
    purge(pool.clone()).await;

    let cfg = cfg_with("CSV-USER-ND");
    let csv = format!("主播ID,主播昵称,抖音号,音浪\n__e2e无日期号,__e2e无日期号,{no},12345\n");
    let Some(url) = serve_text(Box::leak(csv.into_boxed_str())).await else {
        return;
    };
    // 文件名故意不带日期（"主播榜.csv"），正文也不带
    let out = bot::BotRouter::handle(&st, &cfg, inbound_from("CSV-USER-ND", "主播榜.csv", &url))
        .await
        .expect("附件导入应有回复");
    assert!(
        out.text.contains(&yesterday),
        "回复的业务日应是「收到日 − 1」= {}，实际：{}",
        yesterday,
        out.text
    );

    let (biz_date, wave): (String, i64) = (
        sqlx::query(
            "SELECT DATE_FORMAT(biz_date,'%Y-%m-%d') AS d FROM import_pending WHERE douyin_no = ?",
        )
        .bind(no)
        .map(|r: sqlx::postgres::PgRow| {
            sqlx::Row::try_get::<String, _>(&r, "d").unwrap_or_default()
        })
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_default(),
        sqlx::query("SELECT COALESCE(wave, -1) AS w FROM import_pending WHERE douyin_no = ?")
            .bind(no)
            .map(|r: sqlx::postgres::PgRow| sqlx::Row::try_get::<i64, _>(&r, "w").unwrap_or(-1))
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten()
            .unwrap_or(-1),
    );
    assert_eq!(biz_date, yesterday, "没指定日期就落「收到日 − 1」");
    assert_eq!(wave, 12345, "值要原样暂存");

    purge(pool).await;
}

/// 下载失败要明确回报，不能让用户以为导过了。
#[tokio::test]
async fn attachment_download_failure_is_reported() {
    let Some(st) = setup().await else { return };
    let cfg = cfg_with("CSV-USER-C");
    // 指向一个没人监听的端口
    let out = bot::BotRouter::handle(
        &st,
        &cfg,
        inbound_from("CSV-USER-C", "x.csv", "http://127.0.0.1:9/nothing.csv"),
    )
    .await
    .expect("应回复下载失败");
    assert!(out.text.contains("附件下载失败"), "实际：{}", out.text);
}

#[tokio::test]
async fn qq_does_not_require_admin_and_wechat_stranger_is_ignored() {
    let Some(st) = setup().await else { return };
    let cfg = cfg();
    assert!(
        bot::BotRouter::handle(&st, &cfg, inbound_stranger("帮助"))
            .await
            .is_some(),
        "QQ 消息不应要求管理员"
    );
    assert!(
        bot::BotRouter::handle(&st, &cfg, inbound_wechat_stranger("帮助"))
            .await
            .is_none()
    );
}

/// 2026-10-10 权限分层 e2e：已绑定的微信私聊 = 普通用户（docs/09 §2 权限矩阵）。
/// 直写真库 `push_bindings` 构造「已绑定」（唯一 name / conversation_id，不走绑定流程），
/// 覆盖：未绑定静默 → 帮助只列 5 条 → 管理员命令（中文名 / 英文别名大小写 / 导入命令）
/// 一律被拒且不落库 → 纯日期与 CSV 附件只回「管理员功能」→ 暂停 / 开启只动 daily_report。
/// 收尾清理自己写的 push_bindings / bot_sessions / 业务数据行，可反复跑。
#[tokio::test]
async fn wechat_bound_user_permission_matrix() {
    let Some(st) = setup().await else { return };
    let Some(pool) = st.writer.clone() else {
        eprintln!("跳过：未配置写池（此用例需要真库写权限）");
        return;
    };
    let cfg = cfg();
    let stamp = chrono::Utc::now().timestamp_millis();
    let name = format!("__e2e_user_{stamp}");
    let conversation_id = format!("WECHAT-USER-{stamp}");
    let date_attach = "2019-02-25";
    let date_text = "2019-02-26";
    clean_date(&st, date_attach).await;
    clean_date(&st, date_text).await;

    let conv = conversation_id.clone();
    let user_msg = move |text: &str| Inbound {
        channel: "weixin",
        conversation_id: conv.clone(),
        is_group: false,
        text: text.into(),
        attachments: vec![],
    };

    // ① 未绑定：连「帮助」都完全静默（角色只由 push_bindings 命中决定）
    assert!(
        bot::BotRouter::handle(&st, &cfg, user_msg("帮助"))
            .await
            .is_none(),
        "未绑定的微信私聊「帮助」应静默"
    );

    // ② 构造绑定：notify_target 先开着，用来验证「暂停」不会把追分提醒一起清零
    sqlx::query(
        "INSERT INTO push_bindings
           (name, team, person_id, conversation_id, notify_target, daily_report, updated_at)
         VALUES (?, 'male', 0, ?, 1, 1, ?)",
    )
    .bind(&name)
    .bind(&conversation_id)
    .bind(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string())
    .execute(&pool)
    .await
    .expect("写入测试绑定行");

    // ③ 帮助：只列普通用户 5 条，不出现管理员命令名（独立断言，不依赖正面文案）
    let help = bot::BotRouter::handle(&st, &cfg, user_msg("帮助"))
        .await
        .expect("已绑定用户发「帮助」应有回复");
    assert!(help.text.contains("可用命令"), "实际：{}", help.text);
    assert!(help.text.contains("每日报告"), "实际：{}", help.text);
    assert!(
        !help.text.contains("排行"),
        "普通用户帮助不该出现「排行」：{}",
        help.text
    );
    assert!(
        !help.text.contains("导入"),
        "普通用户帮助不该出现「导入」：{}",
        help.text
    );

    // ④ 管理员命令：中文名 / 英文别名大小写 / 文本导入命令 → 拒绝，且拒绝不执行
    for (text, marker) in [
        ("状态", "「状态」"),
        ("RANK", "「排行」"),
        ("导入 男团 2019-02-26\n南方楠 55555", "「导入」"),
    ] {
        let out = bot::BotRouter::handle(&st, &cfg, user_msg(text))
            .await
            .unwrap_or_else(|| panic!("「{text}」应有拒绝回复"));
        assert!(
            out.text.contains("管理员命令"),
            "「{text}」实际：{}",
            out.text
        );
        assert!(
            out.text.contains("没有权限"),
            "「{text}」实际：{}",
            out.text
        );
        assert!(out.text.contains(marker), "「{text}」实际：{}", out.text);
    }
    assert_eq!(
        batch_rows(&pool, date_text).await,
        0,
        "被拒的文本导入不能产生 import_batch"
    );
    assert_eq!(
        metric_rows(&pool, date_text).await,
        0,
        "被拒的文本导入不能落 daily_metric"
    );

    // ⑤ 纯日期黏住（如「10.1」）是管理员流程：只回提示，不设置业务日期
    let date_msg = bot::BotRouter::handle(&st, &cfg, user_msg("10.1"))
        .await
        .expect("纯日期应回管理员功能提示");
    assert!(
        date_msg.text.contains("管理员功能"),
        "实际：{}",
        date_msg.text
    );
    assert!(
        date_msg.text.contains("没有权限"),
        "实际：{}",
        date_msg.text
    );

    // ⑥ CSV 附件：提示管理员功能，不下载、不导入（附件直链可访问，导入没发生才算过）
    let Some(url) = serve_text("南方楠,55555\n").await else {
        return;
    };
    let attach = Inbound {
        channel: "weixin",
        conversation_id: conversation_id.clone(),
        is_group: false,
        text: String::new(),
        attachments: vec![bot::Attachment {
            url,
            file_name: "2019-02-25-男团.csv".into(),
        }],
    };
    let attach_out = bot::BotRouter::handle(&st, &cfg, attach)
        .await
        .expect("普通用户发附件应回管理员功能提示");
    assert!(
        attach_out.text.contains("管理员功能"),
        "实际：{}",
        attach_out.text
    );
    assert_eq!(
        batch_rows(&pool, date_attach).await,
        0,
        "普通用户的附件不能被导入"
    );
    assert_eq!(
        metric_rows(&pool, date_attach).await,
        0,
        "普通用户的附件不能落 daily_metric"
    );

    // ⑦ 暂停 / 开启接收每日推送：只动 daily_report，notify_target 原值保持
    let pause = bot::BotRouter::handle(&st, &cfg, user_msg("暂停接收每日推送"))
        .await
        .expect("暂停应有回复");
    assert!(pause.text.contains("已暂停"), "实际：{}", pause.text);
    assert_eq!(
        push_flags(&pool, &name).await,
        (0, 1),
        "暂停只关 daily_report，不能连带清零 notify_target"
    );

    let resume = bot::BotRouter::handle(&st, &cfg, user_msg("开启接收每日推送"))
        .await
        .expect("开启应有回复");
    assert!(resume.text.contains("已开启"), "实际：{}", resume.text);
    assert_eq!(
        push_flags(&pool, &name).await,
        (1, 1),
        "开启恢复 daily_report，notify_target 仍不受影响"
    );

    // ⑧ 报告类命令可用：数据口径不断言（测试库可能没有数据），
    //    只要求回复非空且不是权限 / 未知命令话术
    for text in ["每日报告", "个人报告"] {
        let out = bot::BotRouter::handle(&st, &cfg, user_msg(text))
            .await
            .unwrap_or_else(|| panic!("「{text}」应有回复"));
        assert!(!out.text.is_empty(), "「{text}」回复不能为空");
        assert!(
            !out.text.contains("没有权限"),
            "「{text}」不该被拒：{}",
            out.text
        );
        assert!(
            !out.text.contains("没认出"),
            "「{text}」不该是未知命令：{}",
            out.text
        );
    }

    // 收尾：清掉本用例写的业务数据 / 绑定行 / 会话登记
    clean_date(&st, date_attach).await;
    clean_date(&st, date_text).await;
    let _ = sqlx::query("DELETE FROM push_bindings WHERE name = ?")
        .bind(&name)
        .execute(&pool)
        .await;
    let _ =
        sqlx::query("DELETE FROM bot_sessions WHERE channel = 'weixin' AND conversation_id = ?")
            .bind(&conversation_id)
            .execute(&pool)
            .await;
}

#[tokio::test]
async fn help_and_status_reply() {
    let Some(st) = setup().await else { return };
    let cfg = cfg();
    let help = bot::BotRouter::handle(&st, &cfg, inbound("帮助"))
        .await
        .expect("帮助应有回复");
    assert!(help.text.contains("可用命令"), "实际：{}", help.text);
    let status = bot::BotRouter::handle(&st, &cfg, inbound("状态"))
        .await
        .expect("状态应有回复");
    assert!(status.text.contains("数据状态"), "实际：{}", status.text);
}

#[tokio::test]
async fn import_with_explicit_date() {
    let Some(st) = setup().await else { return };
    clean_date(&st, "2019-03-14").await;
    let cfg = cfg();
    let text = "导入 男团 2019-03-14\n南方楠 88888";
    let out = bot::BotRouter::handle(&st, &cfg, inbound(text))
        .await
        .expect("导入应有回复");
    // 2026-10-07 起导入文案精简为「✅ 日期 男团 导入成功 N 行」
    assert!(out.text.contains("导入成功 1 行"), "实际：{}", out.text);
    clean_date(&st, "2019-03-14").await;
}

#[tokio::test]
async fn export_csv_by_date() {
    let Some(st) = setup().await else { return };
    clean_date(&st, "2019-03-15").await;
    let cfg = cfg();
    // 先导入一行确定的数据再按显式日期导出，不依赖库里是否已有「今天」的真实数据
    let seeded = bot::BotRouter::handle(&st, &cfg, inbound("导入 男团 2019-03-15\n浩龙 77777"))
        .await
        .expect("导入应有回复");
    assert!(
        seeded.text.contains("导入成功 1 行"),
        "实际：{}",
        seeded.text
    );
    let out = bot::BotRouter::handle(&st, &cfg, inbound("导出 2019-03-15"))
        .await
        .expect("导出应有回复");
    assert!(out.text.starts_with("名字,"), "实际：{}", out.text);
    // CSV 是机器格式：数值不带千分位逗号（否则一行会被拆成三列）
    assert!(out.text.contains("浩龙,77777"), "实际：{}", out.text);
    clean_date(&st, "2019-03-15").await;
}

#[tokio::test]
async fn rank_and_daily_report() {
    let Some(st) = setup().await else { return };
    let cfg = cfg();
    let rank = bot::BotRouter::handle(&st, &cfg, inbound("排行 5"))
        .await
        .expect("排行应有回复");
    assert!(rank.text.contains("音浪榜"), "实际：{}", rank.text);
    let report = bot::BotRouter::handle(&st, &cfg, inbound("日报 男团"))
        .await
        .expect("日报应有回复");
    assert!(report.text.contains("日报（"), "实际：{}", report.text);
    assert!(
        report.text.contains("未开播") || report.text.contains("⚠️"),
        "实际：{}",
        report.text
    );
}

#[tokio::test]
async fn missing_data_check() {
    let Some(st) = setup().await else { return };
    let cfg = cfg();
    let out = bot::BotRouter::handle(&st, &cfg, inbound("缺数据 男团 2026-10-06"))
        .await
        .expect("缺数据应有回复");
    assert!(
        out.text.contains("数据已齐") || out.text.contains("缺数据"),
        "实际：{}",
        out.text
    );
}

#[tokio::test]
async fn unknown_command_gets_hint_reply() {
    let Some(st) = setup().await else { return };
    let cfg = cfg();
    let out = bot::BotRouter::handle(&st, &cfg, inbound("blahblah"))
        .await
        .expect("未知命令应回复提示");
    assert!(out.text.contains("不认识"), "实际：{}", out.text);
}

/// 回归（2026-10-10 清库后第一天）：文件里同时有「当日音浪」和「累计音浪」时，
/// 必须用当日音浪入库（累计只当快照）。以前走「累计差分」，清库后没有前一天快照，
/// 会把累计值当成当日音浪；而且 MIN(biz_date) 会让首日被误判成基线日，从趋势/导出里消失。
#[tokio::test]
async fn first_import_uses_explicit_daily_wave_and_is_exportable() {
    let Some(st) = setup().await else { return };
    let cfg = cfg();
    let date = "2019-03-11"; // 专用测试日，结束即清理
    let text = format!("导入 男团 {date}\n浩龙 123456 9999999");
    let out = bot::BotRouter::handle(&st, &cfg, inbound(&text))
        .await
        .expect("导入应有回复");
    assert!(out.text.contains("导入成功 1 行"), "实际：{}", out.text);

    let pool = st.writer.as_ref().expect("需要写池");
    let row = sqlx::query(
        "SELECT d.wave, d.cumulative_wave, d.wave_reliable FROM daily_metric d
         JOIN person p ON p.id = d.person_id
         WHERE d.biz_date = ? AND p.name = '浩龙'",
    )
    .bind(date)
    .fetch_one(pool)
    .await
    .expect("应查到刚导入的行");
    let wave: i64 = sqlx::Row::try_get(&row, "wave").unwrap_or_default();
    let cum: i64 = sqlx::Row::try_get(&row, "cumulative_wave").unwrap_or_default();
    assert_eq!(wave, 123456, "当日音浪要用显式值，不能被算成累计值");
    assert_eq!(cum, 9999999, "累计音浪要留作快照，供后一天差分");

    // 首日不是基线日：导出能查到（基线日会被 get_wave_series 剔除）
    let out = bot::BotRouter::handle(&st, &cfg, inbound(&format!("导出 {date}")))
        .await
        .expect("导出应有回复");
    assert!(
        out.text.contains("浩龙,123456"),
        "清库后第一天必须能导出，实际：{}",
        out.text
    );

    // 清理
    let _ = sqlx::query("DELETE FROM daily_metric WHERE biz_date = ?")
        .bind(date)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM import_batch WHERE biz_date = ?")
        .bind(date)
        .execute(pool)
        .await;
}
