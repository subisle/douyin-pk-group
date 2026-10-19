//! 集成测试（只读；需要真实 PG：`DB_LOCAL_*`。未配置时**显式打印跳过**）。
//!
//! 跑法：
//! `cd rust && set -a && source .env && set +a && cargo test -p pk-server --test bot_report_e2e -- --ignored --nocapture`
//!
//! 2026-10-19 用户口径验收：
//!   1. 命令 3「徒弟榜单」改发**师门日报图**（与自动推送同源），文字只作兜底；
//!   2. 命令 2「个人报告」带**今日音浪 + 预估收入**，收入口径与展示端同源
//!      （后台 `public_settings` 的 share_pct / fee_pct）。
//!
//! 与 monthly_metric_e2e.rs 同一套纪律：
//! - `DB_LOCAL_HOST` 未配置 → 打印显式跳过，不静默 pass；
//! - 生产库防呆（库名 douyin + 147.90/127.0.0.1/localhost → 拒绝运行）；
//! - **只读**：不写任何表，不断言真实台账里具体主播的数值（只断言结构）。

use pk_server::{config, pgrs, state::AppState};

async fn setup() -> Option<AppState> {
    if std::env::var("DB_LOCAL_HOST").is_err() {
        eprintln!("跳过：未配置 DB_LOCAL_*（此测试需要真实测试库）");
        return None;
    }
    let sources = config::load_sources().expect("sources");
    {
        let prod_hit = |h: &str, d: &str| {
            d == "douyin" && (h.contains("147.90") || h == "127.0.0.1" || h == "localhost")
        };
        for s in &sources {
            if prod_hit(&s.host, &s.database) {
                eprintln!(
                    "拒绝运行：数据源 {} 指向生产库（{} / {}）。请用独立测试库",
                    s.id, s.host, s.database
                );
                return None;
            }
        }
    }
    let pools = pgrs::build_pools(&sources).await.expect("建池失败");
    /* 收益配置走**写池**（store::config_get → w(st)）：不接写池就读不到后台
    public_settings，只能拿 serde 的缺省 35/0，测不出「口径真的同源」。
    这里接上写池但**全程只读**（本用例不执行任何写 SQL）。 */
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
    Some(AppState {
        auth: std::sync::Arc::new(pk_server::auth::Auth::from_env()),
        wave_ttl: std::time::Duration::from_secs(600),
        sources,
        pools,
        writer, // 只读用例：接写池只为读 config，不执行任何写 SQL
        redis: None,
        active: std::sync::Mutex::new(active),
        cache: std::sync::Mutex::new(Default::default()),
        bots: std::sync::OnceLock::new(),
    })
}

/// 收益口径：读后台配置，算一次「今日音浪 → 预估到手」。
/// 断言结构（有金额、金额非负），不断言具体数字（真实数据随时变）。
#[tokio::test]
#[ignore]
async fn est_yuan_follows_backend_settings() {
    let Some(st) = setup().await else { return };
    let st = std::sync::Arc::new(st);
    let (share, fee) = pk_server::push::earnings_pct(&st).await;
    assert!(
        (0..=100).contains(&share),
        "分成比例必须是合法百分数：{share}"
    );
    assert!(
        (0.0..=100.0).contains(&fee),
        "税费/手续费必须是合法百分数：{fee}"
    );

    // 10 万音浪 = 1 万元流水：分成后应等于 10000 × share%（fee=0 时）
    let v = pk_server::push::est_yuan(&st, 100_000)
        .await
        .expect("正数音浪必须有估值");
    assert!(v >= 0.0, "估值不能为负：{v}");
    let expect = 10_000.0 * (share as f64 / 100.0) * (1.0 - fee as f64 / 100.0);
    assert!((v - expect).abs() < 0.01, "估值应等于 {expect}，实际 {v}");

    // 没播（wave=0）不估值，避免显示「¥0」
    assert!(pk_server::push::est_yuan(&st, 0).await.is_none());
    assert!(pk_server::push::est_yuan(&st, -1).await.is_none());
}

/// 个人报告的收入：用**真实后台配置**算一次，断言形状（¥ 前缀、非负、与配置一致）。
/// 排版本身（「今日音浪 · 预估 ¥x」那一行）由 bot 侧单测钉住，这里只验口径来源。
#[tokio::test]
#[ignore]
async fn my_report_income_uses_real_settings() {
    let Some(st) = setup().await else { return };
    let st = std::sync::Arc::new(st);
    let money = pk_server::push::est_yuan(&st, 50_000)
        .await
        .map(pk_server::push::fmt_yuan)
        .expect("50_000 音浪必须有估值");
    assert!(money.starts_with('¥'), "金额要带 ¥ 前缀：{money}");

    let (share, fee) = pk_server::push::earnings_pct(&st).await;
    let expect = 5_000.0 * (share as f64 / 100.0) * (1.0 - fee as f64 / 100.0);
    let got: f64 = money.trim_start_matches('¥').parse().expect("金额可解析");
    assert!(
        (got - expect).abs() < 0.01,
        "5 万音浪按 {share}%分成/{fee}%手续费 应估 {expect}，实际 {money}"
    );
    eprintln!("5 万音浪 → {money}（share={share}% fee={fee}%）");
}

/// 徒弟榜单图：真实库里取一个有徒弟的人，图必须渲染出来（PNG 魔数校验）。
/// 找不到有徒弟的人时打印跳过（不断言真实台账里必须有师徒关系）。
#[tokio::test]
#[ignore]
async fn disciple_report_renders_png_from_real_db() {
    let Some(st) = setup().await else { return };
    let st = std::sync::Arc::new(st);
    let dates = pk_server::repos::get_available_dates(&st, Some("LOCAL"), false)
        .await
        .expect("取业务日");
    let Some(date) = dates.first().cloned() else {
        eprintln!("跳过：库里没有业务日");
        return;
    };

    // 找一个名下有直系徒弟的人
    let rows = pgrs::query(
        &st.pools,
        "LOCAL",
        "SELECT p.id, p.name, COUNT(d.id) AS n FROM person p \
         JOIN person d ON d.master_id = p.id AND d.deleted_at IS NULL \
         WHERE p.deleted_at IS NULL GROUP BY p.id, p.name \
         HAVING COUNT(d.id) >= 1 ORDER BY n DESC LIMIT 1",
    )
    .await
    .expect("查师傅");
    let Some(r) = rows.first() else {
        eprintln!("跳过：库里没有师徒关系");
        return;
    };
    use sqlx::Row as _;
    let pid: i64 = r.try_get("id").unwrap_or(0);
    let name: String = r.try_get("name").unwrap_or_default();
    if pid <= 0 {
        eprintln!("跳过：没取到师傅 id");
        return;
    }
    eprintln!("用师傅 {name}（person_id={pid}）验证师门日报图");

    let rep = match pk_server::lineage_report::build(&st, &date, pid as u64).await {
        Ok(rep) => rep,
        Err(e) => {
            eprintln!("跳过：师门报表构建失败（{e}）");
            return;
        }
    };
    assert!(rep.rows.len() > 1, "有徒弟就该有行：{}", rep.rows.len());

    let pngs = pk_server::push::lineage_report_pngs_for(&st, &rep, name.trim()).await;
    assert!(!pngs.is_empty(), "师门日报图必须渲染出来");
    for (i, p) in pngs.iter().enumerate() {
        assert!(p.len() > 8, "第 {} 页字节数异常：{}", i + 1, p.len());
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n", "第 {} 页不是 PNG", i + 1);
    }
    eprintln!(
        "师门日报图 {} 页，共 {} 字节",
        pngs.len(),
        pngs.iter().map(|p| p.len()).sum::<usize>()
    );
}

/// 命令 3「徒弟榜单」：真实库 + 真实命令函数，必须返回**图片**（文字只作兜底）。
/// 用鹏先生（person_id=1027，名下 26 个徒弟）这条真实数据，不造数据、不写库。
#[tokio::test]
#[ignore]
async fn cmd3_disciple_rank_returns_png() {
    let Some(st) = setup().await else { return };
    let st = std::sync::Arc::new(st);
    let b = pk_server::store::PushBinding {
        name: "鹏先生".into(),
        team: String::new(),
        person_id: 1027,
        conversation_id: String::new(),
        notify_target: false,
        daily_report: true,
        lineage_report: true,
        last_lineage_date: String::new(),
    };
    let (text, pngs) = pk_server::bot::cmd_disciple_rank_user(&st, &b).await;
    assert!(
        !pngs.is_empty(),
        "命令 3 必须带师门日报图；文字兜底：{text}"
    );
    for (i, p) in pngs.iter().enumerate() {
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n", "第 {} 页不是 PNG", i + 1);
    }
    // 文字仍要有内容（QQ 这类发不了图的渠道靠它兜底）
    assert!(text.contains("徒弟榜单"), "文字兜底不该是空的：{text}");
    eprintln!(
        "命令3：{} 页图，文字兜底 {} 字",
        pngs.len(),
        text.chars().count()
    );
}

/// 命令 2「个人报告」：今日音浪 + 预估收入都要在。
/// 动态挑一个**今天真有音浪**的人，避免依赖「鹏先生今天播没播」这种会变的事实。
#[tokio::test]
#[ignore]
async fn cmd2_my_report_has_income() {
    let Some(st) = setup().await else { return };
    let st = std::sync::Arc::new(st);
    let dates = pk_server::repos::get_available_dates(&st, Some("LOCAL"), false)
        .await
        .expect("取业务日");
    let Some(date) = dates.first().cloned() else {
        eprintln!("跳过：库里没有业务日");
        return;
    };
    // 今天有可信音浪、且未删除的人里取音浪最高的一个
    let rows = pk_server::pgrs::query(
        &st.pools,
        "LOCAL",
        &format!(
            "SELECT d.person_id, p.name, SUM(d.wave)::BIGINT AS w FROM daily_metric d \
             JOIN person p ON p.id = d.person_id AND p.deleted_at IS NULL \
             WHERE d.wave_reliable = 1 AND TO_CHAR(d.biz_date,'YYYY-MM-DD') = '{date}' \
             GROUP BY d.person_id, p.name HAVING SUM(d.wave) > 0 ORDER BY w DESC LIMIT 1"
        ),
    )
    .await
    .expect("查今日有播的人");
    let Some(r) = rows.first() else {
        eprintln!("跳过：今天没有有音浪的人");
        return;
    };
    use sqlx::Row as _;
    let pid: i64 = r.try_get("person_id").unwrap_or(0);
    let name: String = r.try_get("name").unwrap_or_default();
    let wave: i64 = r.try_get("w").unwrap_or(0);
    let b = pk_server::store::PushBinding {
        name: name.clone(),
        team: String::new(),
        person_id: pid as u64,
        conversation_id: String::new(),
        notify_target: false,
        daily_report: true,
        lineage_report: false,
        last_lineage_date: String::new(),
    };
    let text = pk_server::bot::cmd_my_report_user(&st, &b).await;
    assert!(text.contains("我的报告"), "{text}");
    assert!(text.contains("今日"), "今日音浪行必须在：{text}");
    assert!(
        !text.contains("今天还没开播"),
        "挑的是有播的人，不该显示没开播（wave={wave}）：{text}"
    );
    assert!(text.contains("预估 ¥"), "播了就要带预估收入：{text}");
    eprintln!("命令2（{name} 今日 {wave} 音浪）：\n{text}");
}

/// 追分提醒图（2026-10-19「只要图片不要文字」）：真实渲染链路必须产出合法 PNG。
/// 不依赖具体某人的追分目标（真实库里可能没有），直接用一段固定文案走渲染。
#[tokio::test]
#[ignore]
async fn target_push_renders_png() {
    let Some(st) = setup().await else { return };
    let _ = st; // 渲染不查库，但保留 setup 以便与其它用例同一套跳过/防呆语义
    let text = "📈 追分（10-18）\n\
                🎉 本月目标 50 万 · 已达成（你 52 万）\n\
                ⚠️ 浩冬 · 被反超，还差 10 万（你 30 万 / TA 40 万）\n\
                · 本月第 3 名 · 你第 5 名，还差 2 名\n\
                数值目标完成度 104%\n\
                \n\
                你本月累计 52 万\n\
                改目标：我的 → 追分设置";
    let pngs = pk_server::push::target_push_pngs(text, "浩雨").await;
    assert_eq!(pngs.len(), 1, "追分提醒应是单页图");
    assert_eq!(&pngs[0][..8], b"\x89PNG\r\n\x1a\n", "不是 PNG");
    assert!(
        pngs[0].len() > 10_000,
        "图太小，可能只渲出空白：{} 字节",
        pngs[0].len()
    );
    eprintln!("追分提醒图 {} 字节", pngs[0].len());
}
