//! 微信扫码绑定的库层回归（2026-10-16 用户报障「二维码绑定有问题」）。
//!
//! 根因：iLink 会话 key `wxacct:{bot_id}:{user_id}` 实测 **65 字符**，而
//! `wx_sessions` / `push_bindings` / `bot_sessions` 的 `conversation_id` 是 `VARCHAR(64)`：
//!   · `wx_sessions` 写上下文报 `value too long for type character varying(64)`
//!     → 主动推送永远取不到 `context_token`；
//!   · `push_bindings` 的写入是 `let _ =`（静默）→ 「已绑定」状态落不下去，
//!     展示端「我的」页一直显示未绑定 / 让用户再扫一次。
//! 迁移 `027_weixin_conversation_len` 把三列放宽到 191。
//!
//! 本用例**真连本地 PostgreSQL**，用与生产完全相同的两个写入口
//! （`store::wx_session_upsert` / `store::push_bind_account`）验证 65 字符 key 能落库，
//! 并覆盖「同一 key 幂等重写」与清理。默认 `#[ignore]`（CI 无库时报 ignored，不假绿）：
//!
//! ```bash
//! cd rust && set -a && source .env && set +a && \
//!   cargo test -p pk-server --test wx_binding_e2e -- --ignored --nocapture
//! ```
//!
//! 2026-10-16 增补第二条用例：4 位数字绑定码全链路（生成 → 撞码唯一 → 发码绑定 → 码清空）。
//! 4 位只有 10^4 种取值，必须钉死「一个活码只挂一个账号」，否则会把微信绑到别人的账号上。

use pk_server::{config, pgrs, state::AppState};
use sqlx::Row;

/// 与线上同形状的一个真实 key（65 字符）——长度本身就是本用例的被测对象
const CONV: &str = "wxacct:9558d8586108@im.bot:o9cq808v597xaUoY0c-VHGRBHoDk@im.wechat";
const USER: &str = "o9cq808v597xaUoY0c-VHGRBHoDk@im.wechat";
const NAME: &str = "__e2e微信绑定";

async fn setup() -> Option<AppState> {
    if std::env::var("DB_LOCAL_HOST").is_err() {
        eprintln!("跳过：未配置 DB_LOCAL_*（此测试需要真实测试库）");
        return None;
    }
    let sources = config::load_sources().expect("sources");
    {
        // 生产库防呆（与 store_e2e.rs 同一口径）：会写库，绝不能对准生产
        let prod_hit = |h: &str, d: &str| {
            d == "douyin" && (h.contains("147.90") || h == "127.0.0.1" || h == "localhost")
        };
        for s in &sources {
            if prod_hit(&s.host, &s.database) {
                eprintln!(
                    "拒绝运行：数据源 {} 指向生产库（{} / {}）。e2e 必须用独立测试库",
                    s.id, s.host, s.database
                );
                return None;
            }
        }
        if let Ok(w) = config::load_writer() {
            if prod_hit(&w.host, &w.database) {
                eprintln!("拒绝运行：writer 指向生产库（{} / {}）", w.host, w.database);
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
    Some(AppState {
        auth: std::sync::Arc::new(pk_server::auth::Auth::from_env()),
        wave_ttl: std::time::Duration::from_secs(600),
        sources,
        pools,
        writer,
        // 测试不起 Redis（None = 全内存模式，与引入前一致）
        redis: None,
        active: std::sync::Mutex::new(active),
        cache: std::sync::Mutex::new(Default::default()),
        bots: std::sync::OnceLock::new(),
    })
}

async fn cleanup(st: &AppState) {
    let Some(pool) = st.writer.as_ref() else {
        return;
    };
    for sql in [
        format!("DELETE FROM wx_sessions WHERE conversation_id = '{CONV}'"),
        format!("DELETE FROM push_bindings WHERE name = '{NAME}'"),
        format!("DELETE FROM wx_push_accounts WHERE name = '{NAME}'"),
        format!("DELETE FROM bot_sessions WHERE conversation_id = '{CONV}'"),
    ] {
        let _ = sqlx::query(&sql).execute(pool).await;
    }
}

/// 65 字符的 iLink 会话 key：两条写入路径都必须成功（迁移 027 前第一条会报列长错）。
#[tokio::test]
#[ignore = "需要真实 PostgreSQL（DB_LOCAL_*）；跑法见文件头，加 -- --ignored"]
async fn weixin_65char_conversation_key_roundtrip() {
    let Some(st) = setup().await else { return };
    if st.writer.is_none() {
        eprintln!("跳过：没有写库连接（DB_WRITER_*）");
        return;
    }
    let pool = st.writer.as_ref().expect("writer");
    cleanup(&st).await;

    assert_eq!(CONV.chars().count(), 65, "本用例的被测前提：key 必须 >64");
    println!(
        "[wx_binding_e2e] 真跑断言（非跳过）：key 长度={}",
        CONV.chars().count()
    );

    // 1. 私聊上下文（主动推送要用的 context_token 存在这里）
    pk_server::store::wx_session_upsert(&st, CONV, USER, "ctx-token-e2e").await;
    let row =
        sqlx::query("SELECT to_user_id, context_token FROM wx_sessions WHERE conversation_id = $1")
            .bind(CONV)
            .fetch_optional(pool)
            .await
            .expect("查 wx_sessions");
    let row = row.expect("wx_sessions 必须能存下 65 字符 key（迁移 027）");
    assert_eq!(
        row.try_get::<String, _>("context_token")
            .unwrap_or_default(),
        "ctx-token-e2e"
    );

    // 2. 绑定落 push_bindings（展示端「已绑定」的唯一口径）
    pk_server::store::push_bind_account(&st, NAME, "male", CONV, USER).await;
    let saved: String = sqlx::query("SELECT conversation_id FROM push_bindings WHERE name = $1")
        .bind(NAME)
        .fetch_one(pool)
        .await
        .expect("查 push_bindings")
        .try_get("conversation_id")
        .unwrap_or_default();
    assert_eq!(
        saved, CONV,
        "push_bindings 必须原样存下 key（旧写法静默失败 → 一直显示未绑定）"
    );
    assert_eq!(
        pk_server::store::push_binding_conversation(&st, NAME).await,
        CONV,
        "读回口径要一致（激活判定用它）"
    );
    // 展示端「已绑定」看的就是 push_state.bound（= push_bindings.conversation_id 非空）：
    // 用户报障时这一位一直是 false，页面才会一遍遍让用户重新扫码。
    let state = pk_server::store::push_state(&st, NAME).await;
    assert_eq!(
        state.get("bound").and_then(|v| v.as_bool()),
        Some(true),
        "写入后展示端必须看到「已绑定」：{state}"
    );

    // 3. 幂等：重复激活不炸、值不变
    pk_server::store::wx_session_upsert(&st, CONV, USER, "ctx-token-e2e").await;
    pk_server::store::push_bind_account(&st, NAME, "male", CONV, USER).await;
    let again: String = sqlx::query("SELECT conversation_id FROM push_bindings WHERE name = $1")
        .bind(NAME)
        .fetch_one(pool)
        .await
        .expect("查 push_bindings")
        .try_get("conversation_id")
        .unwrap_or_default();
    assert_eq!(again, CONV, "重复激活必须幂等");

    // 4. 清理干净
    cleanup(&st).await;
    let left: i64 = sqlx::query("SELECT COUNT(*) AS n FROM push_bindings WHERE name = $1")
        .bind(NAME)
        .fetch_one(pool)
        .await
        .expect("清理后核查")
        .try_get("n")
        .unwrap_or(-1);
    assert_eq!(left, 0, "清理不干净会污染真实数据");
    let left_sess: i64 =
        sqlx::query("SELECT COUNT(*) AS n FROM wx_sessions WHERE conversation_id = $1")
            .bind(CONV)
            .fetch_one(pool)
            .await
            .expect("清理后核查")
            .try_get("n")
            .unwrap_or(-1);
    assert_eq!(left_sess, 0, "会话行也要清干净");
}

/// 4 位数字绑定码全链路（2026-10-16 口径：只发 4 位数字即可绑定，不再要手机号）：
/// ① push_rotate_code 生成的码是 4 位数字；② 两个账号先后摇码不撞号（部分唯一索引兜底）；
/// ③ 拿码走 push_bind_by_code 能把会话接到账号上、码用掉即清；④ 错码绑不上。
#[tokio::test]
#[ignore = "需要真实 PostgreSQL（DB_LOCAL_*）；跑法见文件头，加 -- --ignored"]
async fn four_digit_bind_code_roundtrip_and_uniqueness() {
    let Some(st) = setup().await else { return };
    if st.writer.is_none() {
        eprintln!("跳过：没有写库连接（DB_WRITER_*）");
        return;
    }
    let pool = st.writer.as_ref().expect("writer");
    const CONV2: &str = "wxacct:e2e-bot@test:e2e-user-4digit";
    cleanup(&st).await;
    let _ = sqlx::query(&format!(
        "DELETE FROM push_bindings WHERE name IN ('{NAME}2','{NAME}3')
           OR conversation_id = '{CONV2}'"
    ))
    .execute(pool)
    .await;

    // ① 生成：4 位纯数字
    let code1 = pk_server::store::push_rotate_code(&st, NAME, "male", 999998)
        .await
        .expect("摇第一个码");
    assert_eq!(code1.len(), 4, "绑定码必须是 4 位：{code1}");
    assert!(
        code1.bytes().all(|b| b.is_ascii_digit()),
        "绑定码必须是纯数字：{code1}"
    );
    println!("[wx_binding_e2e] 账号1 码={code1}");

    // ② 唯一性：另一个账号摇码，不允许与 code1 相同（部分唯一索引 + 重摇兜底）
    let code2 = pk_server::store::push_rotate_code(&st, &format!("{NAME}2"), "male", 999997)
        .await
        .expect("摇第二个码");
    assert_ne!(code1, code2, "两个活码不能相同：{code1} / {code2}");

    // ③ 绑定：正确码接上会话，且是一次性的（码列清空）
    let hit = pk_server::store::push_bind_by_code(&st, &code1, CONV2, "e2e-user")
        .await
        .expect("正确的 4 位码必须能绑定");
    assert_eq!(hit.0, NAME, "绑到的账号要正是摇码的账号");
    let row = sqlx::query("SELECT conversation_id, bind_code FROM push_bindings WHERE name = $1")
        .bind(NAME)
        .fetch_one(pool)
        .await
        .expect("查绑定行");
    assert_eq!(
        row.try_get::<String, _>("conversation_id")
            .unwrap_or_default(),
        CONV2
    );
    assert_eq!(
        row.try_get::<String, _>("bind_code").unwrap_or_default(),
        "",
        "码用掉即清（一次性）"
    );
    // 同一个码第二次用：绑不上
    assert!(
        pk_server::store::push_bind_by_code(&st, &code1, CONV2, "e2e-user")
            .await
            .is_none(),
        "码是一次性的，第二次必须失败"
    );

    // ④ 错码 / 过期码绑不上
    assert!(
        pk_server::store::push_bind_by_code(&st, "0000", CONV2, "e2e-user")
            .await
            .is_none()
            || code2 == "0000",
        "随机错码不该绑上（极小概率撞上 code2，撞上则本轮跳过断言）"
    );

    // 清理干净
    let _ = sqlx::query(&format!(
        "DELETE FROM push_bindings WHERE name IN ('{NAME}','{NAME}2','{NAME}3')
           OR conversation_id = '{CONV2}'"
    ))
    .execute(pool)
    .await;
    let left: i64 =
        sqlx::query("SELECT COUNT(*) AS n FROM push_bindings WHERE name LIKE '__e2e微信绑定%'")
            .fetch_one(pool)
            .await
            .expect("清理后核查")
            .try_get("n")
            .unwrap_or(-1);
    assert_eq!(left, 0, "清理不干净会污染真实数据");
}
