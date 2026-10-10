//! 机器人端到端集成测试：真实 AppState + 真库（需要 DB_LOCAL_* / DB_WRITER_* 环境变量）。
//! 未配置数据库时自动跳过（本地单测不依赖库）。
//!
//! 覆盖：非管理员静默、帮助、状态、导入（带指定日期）、导出 CSV、排行、日报、缺数据。

use std::sync::Arc;

use pk_server::bot::{self, BotConfig, Inbound};
use pk_server::{config, mysql, state::AppState};

async fn setup() -> Option<Arc<AppState>> {
    if std::env::var("DB_LOCAL_HOST").is_err() {
        eprintln!("跳过：未配置 DB_LOCAL_*（此测试需要真实测试库）");
        return None;
    }
    let sources = config::load_sources().expect("sources");
    let pools = mysql::build_pools(&sources).await.expect("建池失败");
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

/// 单独的会话 ID，避免和文本导入用例抢同一个待确认导入槽位（PENDING 是全局的）。
fn cfg_with(chat: &str) -> BotConfig {
    let mut c = cfg();
    c.qq_admins.push(chat.to_string());
    c
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

/// 私聊发 CSV 文件 + 文件名带日期与团别 → 直接入库，不用再回一句话确认。
/// 用独立的 2019-03-13 当业务日期（不碰真实数据），开跑先清干净 → 可反复跑。
#[tokio::test]
async fn csv_attachment_with_date_and_team_hints_is_imported() {
    let Some(st) = setup().await else { return };
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
                .map(|r: sqlx::mysql::MySqlRow| sqlx::Row::try_get::<String, _>(&r, "name").unwrap_or_default())
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
            .map(|r: sqlx::mysql::MySqlRow| sqlx::Row::try_get::<i64, _>(&r, "n").unwrap_or(0))
            .fetch_one(&pool)
            .await
            .unwrap_or(0),
        sqlx::query("SELECT COUNT(DISTINCT p.gender) AS n FROM daily_metric d JOIN person p ON p.id = d.person_id WHERE DATE_FORMAT(d.biz_date,'%Y-%m-%d') = ?")
            .bind(date)
            .map(|r: sqlx::mysql::MySqlRow| sqlx::Row::try_get::<i64, _>(&r, "n").unwrap_or(0))
            .fetch_one(&pool)
            .await
            .unwrap_or(0),
        sqlx::query("SELECT team FROM import_batch WHERE DATE_FORMAT(biz_date,'%Y-%m-%d') = ? ORDER BY id LIMIT 1")
            .bind(date)
            .map(|r: sqlx::mysql::MySqlRow| sqlx::Row::try_get::<String, _>(&r, "team").unwrap_or_default())
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
async fn non_admin_is_silently_ignored() {
    let Some(st) = setup().await else { return };
    let cfg = cfg();
    assert!(bot::BotRouter::handle(&st, &cfg, inbound_stranger("帮助"))
        .await
        .is_none());
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
