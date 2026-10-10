//! 运行时状态存储端到端测试：真实 MySQL（需要 DB_LOCAL_* / DB_WRITER_* 环境变量）。
//! 未配置数据库时自动跳过（CI / 本地纯单测不依赖库）。
//!
//! 2026-10-10 单库化：这批表从 SQLite 并入 MySQL（migrations/003_runtime.sql），
//! 旧 sqlite.rs 的落库断言全部迁到这里，仍覆盖原有口径：
//! 别名、配置、方案（上架互斥）、名单（按赛事类型）、展示端账号（登录/管理员标记）、
//! 绑定申请（唯一待审/裁决/回滚）。
//!
//! 测试用键全部带 `__e2e_` 前缀 / 独立 person_id 段，结束即清理，不污染真实数据。

use pk_server::store;
use pk_server::{config, mysql, state::AppState};

async fn setup() -> Option<AppState> {
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
    Some(AppState {
        auth: std::sync::Arc::new(pk_server::auth::Auth::from_env()),
        wave_ttl: std::time::Duration::from_secs(600),
        sources,
        pools,
        writer,
        active: std::sync::Mutex::new(active),
        cache: std::sync::Mutex::new(Default::default()),
        bots: std::sync::OnceLock::new(),
        wb: std::sync::OnceLock::new(),
    })
}

/// 写池不可用（writer 没配）→ 这些测试没有意义，跳过
fn need_writer(st: &AppState) -> bool {
    if st.writer.is_none() {
        eprintln!("跳过：douyin_writer 未配置");
        return false;
    }
    true
}

#[tokio::test]
async fn alias_roundtrip() {
    let Some(st) = setup().await else { return };
    if !need_writer(&st) {
        return;
    }
    store::upsert_alias(&st, "__e2e_alias__", "真实名字")
        .await
        .expect("写别名");
    assert_eq!(
        store::list_aliases(&st)
            .await
            .get("__e2e_alias__")
            .map(String::as_str),
        Some("真实名字")
    );
    store::upsert_alias(&st, "__e2e_alias__", "改名了")
        .await
        .expect("改别名");
    assert_eq!(
        store::list_aliases(&st)
            .await
            .get("__e2e_alias__")
            .map(String::as_str),
        Some("改名了")
    );
    store::delete_alias(&st, "__e2e_alias__")
        .await
        .expect("删别名");
    assert!(!store::list_aliases(&st).await.contains_key("__e2e_alias__"));
}

#[tokio::test]
async fn config_roundtrip() {
    let Some(st) = setup().await else { return };
    if !need_writer(&st) {
        return;
    }
    store::config_set(&st, "__e2e_cfg__", "值1").await;
    assert_eq!(
        store::config_get(&st, "__e2e_cfg__").await.as_deref(),
        Some("值1")
    );
    store::config_set(&st, "__e2e_cfg__", "值2").await;
    assert_eq!(
        store::config_get(&st, "__e2e_cfg__").await.as_deref(),
        Some("值2")
    );
    assert!(store::config_get(&st, "__e2e_不存在__").await.is_none());
}

#[tokio::test]
async fn plan_publish_mutex_per_event_type() {
    let Some(st) = setup().await else { return };
    if !need_writer(&st) {
        return;
    }
    let a = store::save_plan(&st, "__e2e方案A", "lastN", 3, "{}", "初级争霸赛小组赛")
        .await
        .expect("存方案A");
    let b = store::save_plan(&st, "__e2e方案B", "lastN", 3, "{}", "初级争霸赛小组赛")
        .await
        .expect("存方案B");
    let c = store::save_plan(&st, "__e2e方案C", "lastN", 3, "{}", "月终争霸赛")
        .await
        .expect("存方案C");

    assert!(store::set_published(&st, a, true).await, "上架A");
    assert!(
        store::set_published(&st, c, true).await,
        "不同类型上架互不影响"
    );
    assert!(store::set_published(&st, b, true).await, "上架B成功");
    // 同类型的 A 被自动下架，不同类型的 C 保留
    assert!(
        !store::get_plan(&st, a).await.unwrap().published,
        "同类型互斥：A 被下架"
    );
    assert!(store::get_plan(&st, b).await.unwrap().published);
    assert!(store::get_plan(&st, c).await.unwrap().published);

    // 公开端只能取到已发布的方案
    assert!(store::get_published_plan(&st, a).await.is_none());
    assert!(store::get_published_plan(&st, b).await.is_some());

    // 下架后公开端取不到
    assert!(store::set_published(&st, b, false).await);
    assert!(store::get_published_plan(&st, b).await.is_none());

    store::delete_plan(&st, a).await.expect("删除A");
    store::delete_plan(&st, b).await.expect("删除B");
    store::delete_plan(&st, c).await.expect("删除C");
    assert!(store::get_plan(&st, a).await.is_none(), "删除后不存在");
}

#[tokio::test]
async fn roster_saved_per_event_type() {
    let Some(st) = setup().await else { return };
    if !need_writer(&st) {
        return;
    }
    // 从没保存过 → None（前端据此清空输入框，不能和「被清空」混淆）
    store::save_roster(&st, "__e2e_类型1", "张三\n李四").await;
    store::save_roster(&st, "__e2e_类型1", "李四").await;
    store::save_roster(&st, "__e2e_类型2", "王五").await;
    let (text, updated_at) = store::load_roster(&st, "__e2e_类型1")
        .await
        .expect("应已保存");
    assert_eq!(text, "李四");
    assert!(!updated_at.is_empty());
    assert_eq!(
        store::load_roster(&st, "__e2e_类型2").await.unwrap().0,
        "王五"
    );
    // 清理（置空等价删除语义）
    store::save_roster(&st, "__e2e_类型1", "").await;
    store::save_roster(&st, "__e2e_类型2", "").await;
}

#[tokio::test]
async fn my_account_login_and_admin_flag() {
    let Some(st) = setup().await else { return };
    if !need_writer(&st) {
        return;
    }
    store::my_account_set(&st, "__e2e主播", "13800009999", "13312345678")
        .await
        .expect("建档成功");
    // 手机号路径（带横线归一化）
    assert_eq!(
        store::my_account_check_login(&st, "138-0000-9999", "13312345678")
            .await
            .as_deref(),
        Some("__e2e主播"),
        "手机号+密码 → 账号姓名"
    );
    // 姓名路径（兼容）
    assert_eq!(
        store::my_account_check_login(&st, "__e2e主播", "13312345678")
            .await
            .as_deref(),
        Some("__e2e主播")
    );
    assert!(store::my_account_check_login(&st, "__e2e主播", "999999")
        .await
        .is_none());
    // 管理员标记：upsert 不丢、get 回显
    assert!(store::my_account_set_admin(&st, "__e2e主播", true).await);
    store::my_account_set(&st, "__e2e主播", "13900002222", "13312345678")
        .await
        .expect("覆盖更新");
    assert!(
        store::my_account_is_admin(&st, "__e2e主播").await,
        "upsert 保留 is_admin"
    );
    assert_eq!(
        store::my_account_get(&st, "__e2e主播").await.unwrap()["isAdmin"],
        true
    );
    // 同手机号绑别人 → 拒（__e2e主播 当前手机号是 13900002222）
    assert!(
        store::my_account_set(&st, "__e2e主播2", "139 0000 2222", "13312345679")
            .await
            .is_err(),
        "同手机号已绑其它姓名 → 拒绝"
    );
    // 同密码绑别人 → 拒
    assert!(
        store::my_account_set(&st, "__e2e主播2", "", "13312345678")
            .await
            .is_err(),
        "同密码已绑其它姓名 → 拒绝"
    );
    // 删除
    assert!(store::my_account_delete(&st, "__e2e主播").await);
    store::my_account_delete(&st, "__e2e主播2").await; // 防残留（上面那条拒绝断言可能因数据残留绕过）
    assert!(
        store::my_account_check_login(&st, "__e2e主播", "13312345678")
            .await
            .is_none()
    );
}

/// 微信绑定：手机号路径（2026-10-10）+ 换号重绑时旧账号不再占着会话。
/// 这段是本次改动里最容易出错的地方：绑定写的是 upsert，
/// 若 ON DUPLICATE 把 team 覆盖成空串，推送时按团别就查不到人。
#[tokio::test]
async fn push_bind_by_phone_roundtrip() {
    let Some(st) = setup().await else { return };
    if !need_writer(&st) {
        return;
    }
    let name = "__e2e微信绑定";
    let phone = "13700001111";
    store::my_account_set(&st, name, phone, "13312340001")
        .await
        .expect("建档成功");

    // 手机号绑定（带空格横线也要认：用户从微信里粘贴常带分隔符）
    let got = store::push_bind_by_phone(&st, "137 0000-1111", "conv-e2e-1", "conv-e2e-1").await;
    assert!(got.is_some(), "登记过的手机号要能绑上");
    let (bound_name, _team) = got.unwrap();
    assert_eq!(bound_name, name);
    assert_eq!(
        store::push_state(&st, name).await["bound"],
        true,
        "绑定后 bound=true"
    );

    // 换一个账号绑同一条微信会话：旧账号必须被踢掉，否则两个人收同一份推送
    let name2 = "__e2e微信绑定2";
    store::my_account_set(&st, name2, "13700002222", "13312340002")
        .await
        .expect("建档成功");
    let _ = store::push_bind_by_phone(&st, "13700002222", "conv-e2e-1", "conv-e2e-1").await;
    assert_eq!(
        store::push_state(&st, name).await["bound"],
        false,
        "同一微信会话换绑后，旧账号要解绑"
    );
    assert_eq!(store::push_state(&st, name2).await["bound"], true);

    // 没登记的手机号 → None（不能随便猜个号就绑上）
    assert!(
        store::push_bind_by_phone(&st, "13700009999", "conv-e2e-2", "conv-e2e-2")
            .await
            .is_none()
    );
    // _team 可能为空（台账里没这个人 = 管理员观看账号），这里不断言

    store::push_unbind(&st, name).await;
    store::push_unbind(&st, name2).await;
    store::my_account_delete(&st, name).await;
    store::my_account_delete(&st, name2).await;
}

/// 延迟推送队列（2026-10-10）：同一 (团, 业务日) 只排一队，抢占后别人拿不到。
#[tokio::test]
async fn push_job_queue_is_idempotent_and_exclusive() {
    let Some(st) = setup().await else { return };
    if !need_writer(&st) {
        return;
    }
    let team = "__e2e_team";
    let date = "2026-01-01";
    let id1 = store::push_job_enqueue(&st, team, date, "2026-01-01 10:00:00").await;
    assert!(id1 > 0, "排队要拿到 id");
    let id2 = store::push_job_enqueue(&st, team, date, "2026-01-01 11:00:00").await;
    assert_eq!(id1, id2, "同一 (团, 日) 重复导入只刷新，不叠加任务");

    assert!(
        store::push_job_take(&st, "2026-01-01 09:00:00")
            .await
            .is_none(),
        "没到点的任务不能被取走"
    );
    let taken = store::push_job_take(&st, "2026-01-01 12:00:00").await;
    assert!(taken.is_some(), "到点的任务要取得到");
    assert_eq!(taken.unwrap().0, id1);
    assert!(
        store::push_job_take(&st, "2026-01-01 12:00:00")
            .await
            .is_none(),
        "抢占后同一任务不能再被取一次"
    );

    store::push_job_finish(&st, id1, true, "").await;
    assert!(
        store::push_job_take(&st, "2026-01-01 12:00:00")
            .await
            .is_none(),
        "已完成的任务不再重跑"
    );
}

/// 内置展示端账号：跟着代码走（换库 / 全新部署都在），启动补齐且幂等。
/// 幂等这块是重点：运营自己改过密码后重启，不能被内置值冲掉。
/// P1-2（2026-10-10）：内置账号改从 BUILTIN_MY_ACCOUNTS 环境变量读，默认不创建。
/// 测试用独立测试账号显式注入 env，结束清理（不再用硬编码 shuai/shuai）。
#[tokio::test]
async fn builtin_my_account_seeded_and_idempotent() {
    let Some(st) = setup().await else { return };
    if !need_writer(&st) {
        return;
    }
    // 测试账号（__e2e_ 前缀，结束清理）；「name:pass」和「name」（密码=name）两种写法都覆盖
    std::env::set_var("BUILTIN_MY_ACCOUNTS", "__e2e内置甲:__e2e内置甲,__e2e内置乙");
    // 先清理可能的历史残留，保证从零创建这条路径被真实执行
    store::my_account_delete(&st, "__e2e内置甲").await;
    store::my_account_delete(&st, "__e2e内置乙").await;

    let (created, skipped) = store::ensure_builtin_my_accounts(&st).await;
    assert_eq!(created, 2, "env 注入的两个内置账号都应新建");
    assert_eq!(skipped, 0);

    let builtin: [(&str, &str); 2] = [
        ("__e2e内置甲", "__e2e内置甲"),
        ("__e2e内置乙", "__e2e内置乙"), // 「name」写法：密码 = 账号本身
    ];
    for (name, pass) in builtin {
        assert!(
            store::my_account_is_admin(&st, name).await,
            "内置账号 {name} 必须是管理员观看账号（台账里没有同名主播也能登录）"
        );
        assert_eq!(
            store::my_account_check_login(&st, name, pass)
                .await
                .as_deref(),
            Some(name),
            "内置账号 {name}/{pass} 要能登录"
        );
    }

    // 运营改过密码后重启：不能被内置值冲掉（幂等的核心）
    store::my_account_set(&st, "__e2e内置甲", "", "__e2e新密码")
        .await
        .expect("运营改密码");

    // 再跑一次：不重复创建、不改密码
    let (created_again, _) = store::ensure_builtin_my_accounts(&st).await;
    assert_eq!(created_again, 0, "第二次启动不应重复创建内置账号");
    for (name, pass) in builtin {
        assert_eq!(
            store::my_account_check_login(&st, name, pass)
                .await
                .as_deref(),
            Some(name),
            "补齐逻辑不能改掉已有密码"
        );
    }
    // 改过的密码仍然有效、内置默认密码已失效
    assert_eq!(
        store::my_account_check_login(&st, "__e2e内置甲", "__e2e新密码")
            .await
            .as_deref(),
        Some("__e2e内置甲"),
        "运营改过的密码要继续能登录"
    );
    assert!(
        store::my_account_check_login(&st, "__e2e内置甲", "__e2e内置甲")
            .await
            .is_none(),
        "旧密码应已被运营改掉"
    );

    // 未配置 env → 一个账号都不创建（默认安全）
    std::env::remove_var("BUILTIN_MY_ACCOUNTS");
    let (created_none, _) = store::ensure_builtin_my_accounts(&st).await;
    assert_eq!(
        created_none, 0,
        "未配置 BUILTIN_MY_ACCOUNTS 时不创建任何账号"
    );

    // 清理（内置账号改为测试专用，不再留给生产兜底）
    store::my_account_delete(&st, "__e2e内置甲").await;
    store::my_account_delete(&st, "__e2e内置乙").await;
    assert!(
        store::my_account_check_login(&st, "__e2e内置甲", "__e2e新密码")
            .await
            .is_none()
    );
    assert!(
        store::my_account_check_login(&st, "__e2e内置乙", "__e2e内置乙")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn anchor_claim_roundtrip() {
    let Some(st) = setup().await else { return };
    if !need_writer(&st) {
        return;
    }
    const PID: i64 = 999_900_001; // e2e 专用 person_id 段，结束时清理
    let shot = b"e2e-fake-jpeg".to_vec();
    // 格式/截图缺失直接拒
    assert!(store::anchor_claim_insert(
        &st,
        PID,
        "e2e主播",
        "male",
        "bad no!",
        &shot,
        "image/jpeg"
    )
    .await
    .is_err());
    assert!(
        store::anchor_claim_insert(&st, PID, "e2e主播", "male", "e2e_dy01", &[], "image/jpeg")
            .await
            .is_err()
    );
    let id =
        store::anchor_claim_insert(&st, PID, "e2e主播", "male", "e2e_dy01", &shot, "image/jpeg")
            .await
            .expect("提交成功");
    // 唯一待审
    assert!(
        store::anchor_claim_insert(&st, PID, "e2e主播", "male", "e2e_dy02", &shot, "image/jpeg")
            .await
            .is_err(),
        "已有待审 → 拒绝第二笔"
    );
    // mine / list
    assert_eq!(
        store::anchor_claim_mine(&st, PID).await.unwrap()["douyinNo"],
        "e2e_dy01"
    );
    assert!(store::anchor_claim_list(&st, "pending", 200)
        .await
        .iter()
        .any(|r| r["id"] == id));
    // 截图本体
    let (bytes, mime) = store::anchor_claim_image(&st, id).await.expect("截图存在");
    assert_eq!(bytes, shot);
    assert_eq!(mime, "image/jpeg");
    // 驳回
    let (pid, no, name) = store::anchor_claim_decide(&st, id, false, "e2e 驳回原因", "admin")
        .await
        .expect("裁决成功");
    assert_eq!(
        (pid, no.as_str(), name.as_str()),
        (PID, "e2e_dy01", "e2e主播")
    );
    assert_eq!(
        store::anchor_claim_mine(&st, PID).await.unwrap()["status"],
        "rejected"
    );
    // 驳回后可重新提交并通过
    let id2 =
        store::anchor_claim_insert(&st, PID, "e2e主播", "male", "e2e_dy01", &shot, "image/jpeg")
            .await
            .expect("驳回后可重新提交");
    assert!(store::anchor_claim_decide(&st, id2, true, "", "admin")
        .await
        .is_some());
    assert!(
        store::anchor_claim_decide(&st, id2, true, "", "admin")
            .await
            .is_none(),
        "已裁决不可再改"
    );
    assert_eq!(
        store::anchor_claim_mine(&st, PID).await.unwrap()["status"],
        "approved"
    );
    // 回滚
    store::anchor_claim_revert(&st, id2).await;
    assert_eq!(
        store::anchor_claim_mine(&st, PID).await.unwrap()["status"],
        "pending"
    );

    // 清理：该 person 的所有申请
    if let Some(pool) = st.writer.as_ref() {
        sqlx::query("DELETE FROM anchor_claims WHERE person_id = ?")
            .bind(PID)
            .execute(pool)
            .await
            .expect("清理测试申请");
    }
}

/// 展示端手机号注册 → 绑定主播 → 管理员改绑（2026-10-10 用户口径）：
/// 手机号 = 账号、短信验证码 = 密码；绑定一次后用户自己不能解绑/换绑，只有管理员能改。
/// 用随机手机号 + 随机验证码，跑完把自己建的行删干净（不碰真实账号）。
#[tokio::test]
async fn my_register_bind_and_admin_rebind() {
    let Some(st) = setup().await else { return };
    if !need_writer(&st) {
        return;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间")
        .as_nanos();
    let phone = format!("199{:08}", nanos % 100_000_000);

    // 验证码撞上别人已有的密码（uk_my_pass 1:1）就换一个再试
    let mut registered = None;
    for i in 0..3u128 {
        let code = format!("{:06}", (nanos / 7 + i) % 1_000_000);
        match store::my_account_register(&st, &phone, &code, "e2e-state").await {
            Ok(name) => {
                registered = Some((name, code));
                break;
            }
            Err(e) if e.contains("验证码") => continue,
            Err(e) => panic!("注册应当成功，却失败：{e}"),
        }
    }
    let Some((login_name, code)) = registered else {
        eprintln!("跳过：三次随机验证码都被占用（极小概率）");
        return;
    };

    // 注册瞬间 name 先占位成手机号、bound=0
    assert_eq!(login_name, phone);
    let info = store::my_account_info(&st, &phone)
        .await
        .expect("按手机号查得到");
    assert!(!info.bound, "刚注册还没绑主播");
    assert_eq!(info.source, "register");
    assert_eq!(info.bind_name(), "", "没绑主播时不把手机号当姓名下发");
    assert_eq!(
        store::my_account_check_login(&st, &phone, &code)
            .await
            .as_deref(),
        Some(phone.as_str()),
        "手机号 + 验证码可以登录"
    );

    // 同手机号不能重复注册
    let dup = store::my_account_register(&st, &phone, "135790", "").await;
    assert!(dup.is_err(), "同手机号重复注册应当被拒");

    // 台账里挑两个还没被占用的主播：第一个给用户自助绑定，第二个给管理员改绑
    let anchors = store::all_anchor_search(&st, &st.active_id(), "啸", 10).await;
    let mut bind_anchor: Option<(i64, String)> = None;
    for a in &anchors {
        let pid = a.get("personId").and_then(|v| v.as_i64()).unwrap_or(0);
        let name = a
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if pid <= 0 || name.is_empty() {
            continue;
        }
        if store::my_account_bind(&st, &phone, pid).await.is_ok() {
            bind_anchor = Some((pid, name));
            break;
        }
    }
    let Some((_pid, anchor_name)) = bind_anchor else {
        eprintln!("跳过：台账里没有可用于绑定的主播（候选都被占用或库为空）");
        store::my_account_delete(&st, &phone).await;
        return;
    };
    let info2 = store::my_account_info(&st, &phone)
        .await
        .expect("绑定后按手机号仍能查到");
    assert!(info2.bound, "绑定后 bound=1");
    assert!(!info2.bound_at.is_empty(), "记了绑定时间");
    assert_eq!(info2.name, anchor_name, "账号名改写成主播姓名");
    assert_eq!(
        store::my_account_check_login(&st, &phone, &code)
            .await
            .as_deref(),
        Some(anchor_name.as_str()),
        "绑定后手机号照样能登录"
    );

    // 用户自己再绑一次 → 拒
    let mut rebound_admin: Option<(i64, String)> = None;
    for a in &anchors {
        let pid = a.get("personId").and_then(|v| v.as_i64()).unwrap_or(0);
        let name = a
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if pid <= 0 || name == anchor_name {
            continue;
        }
        let again = store::my_account_bind(&st, &anchor_name, pid).await;
        assert!(
            again.is_err(),
            "已绑定主播的账号，用户自己不能再改（{name}）"
        );
        assert!(again.unwrap_err().contains("已绑定"));
        // 管理员改绑到同一个目标 → 允许
        if let Ok(r) = store::my_account_admin_bind(&st, &anchor_name, pid).await {
            rebound_admin = Some((pid, r.name));
            break;
        }
    }
    let Some((_pid2, anchor2)) = rebound_admin else {
        eprintln!("跳过管理员改绑断言：没有第二个可用主播");
        store::my_account_delete(&st, &anchor_name).await;
        return;
    };
    assert_eq!(
        store::my_account_check_login(&st, &phone, &code)
            .await
            .as_deref(),
        Some(anchor2.as_str()),
        "改绑后按手机号登录拿到新主播"
    );

    // 清理：账号名可能落在任何一次绑定后的名字上
    store::my_account_delete(&st, &anchor2).await;
    store::my_account_delete(&st, &anchor_name).await;
    store::my_account_delete(&st, &phone).await;
    assert!(
        store::my_account_info(&st, &phone).await.is_none(),
        "测试账号已清理"
    );
}

/* ==================== 管理端数据域 e2e（P1-9 / P2-10 / P2-11，2026-10-10） ==================== */

use axum::Json;
use pk_server::admin::{self, ImportRow};
use sqlx::Row;

const MERGE_PREFIX: &str = "__e2e合并";
const E2E_DAILY_DATE: &str = "2999-01-05"; // 独立测试日期段（2999 年），绝不撞真实数据
const E2E_DAILY_DATE2: &str = "2999-01-06";

/// 建两个测试 person + 各自 account（独立 anchor_id 段），返回 id。
/// 已有残留先清掉，保证测试从干净状态开始。
async fn setup_two_people(st: &AppState) -> (u64, u64) {
    let pool = st.writer.as_ref().expect("writer");
    cleanup_admin_e2e(st).await;
    let tid = sqlx::query("INSERT INTO person (name, gender, status) VALUES (?, 'male', 'active')")
        .bind(format!("{MERGE_PREFIX}目标甲"))
        .execute(pool)
        .await
        .expect("建目标主播")
        .last_insert_id();
    let sid = sqlx::query("INSERT INTO person (name, gender, status) VALUES (?, 'male', 'active')")
        .bind(format!("{MERGE_PREFIX}来源乙"))
        .execute(pool)
        .await
        .expect("建来源主播")
        .last_insert_id();
    for (pid, anchor) in [
        (tid, format!("{MERGE_PREFIX}_anchor_t")),
        (sid, format!("{MERGE_PREFIX}_anchor_s")),
    ] {
        sqlx::query(
            "INSERT INTO account (person_id, anchor_id, douyin_no, is_primary) VALUES (?,?,?,1)",
        )
        .bind(pid as i64)
        .bind(&anchor)
        .bind(&anchor)
        .execute(pool)
        .await
        .expect("建测试账号");
    }
    (tid, sid)
}

/// 挂一天数据（写真实 daily_metric 行；wave = cumulative 给定值，minutes=10、is_live=1）
async fn insert_daily_row(st: &AppState, person_id: u64, anchor: &str, date: &str, wave: i64) {
    let pool = st.writer.as_ref().expect("writer");
    sqlx::query(
        "INSERT INTO daily_metric (person_id, anchor_id, biz_date, wave, cumulative_wave, wave_reliable, minutes, is_live) \
         VALUES (?, ?, ?, ?, ?, 1, 10, 1)",
    )
    .bind(person_id as i64)
    .bind(anchor)
    .bind(date)
    .bind(wave)
    .bind(wave)
    .execute(pool)
    .await
    .expect("写测试日明细");
}

/// 读某 person 某日的 wave（没数据返回 None）
async fn read_daily_wave(st: &AppState, person_id: u64, date: &str) -> Option<i64> {
    let pool = st.writer.as_ref().expect("writer");
    sqlx::query("SELECT wave FROM daily_metric WHERE person_id = ? AND biz_date = ?")
        .bind(person_id as i64)
        .bind(date)
        .map(|r: sqlx::mysql::MySqlRow| r.try_get::<i64, _>(0).unwrap_or(0))
        .fetch_optional(pool)
        .await
        .expect("读日明细")
}

async fn count_i64(st: &AppState, sql: &str) -> i64 {
    let pool = st.writer.as_ref().expect("writer");
    sqlx::query(sql)
        .fetch_one(pool)
        .await
        .expect("计数查询")
        .try_get::<i64, _>(0)
        .unwrap_or(-1)
}

/// 管理端 e2e 的公共清理：测试主播（__e2e合并 前缀）名下的一切 + 独立测试日期段
async fn cleanup_admin_e2e(st: &AppState) {
    let pool = st.writer.as_ref().expect("writer");
    for sql in [
        "DELETE FROM daily_metric WHERE biz_date BETWEEN '2999-01-01' AND '2999-12-31'".to_string(),
        "DELETE FROM wave_snapshot WHERE biz_date BETWEEN '2999-01-01' AND '2999-12-31'".to_string(),
        format!(
            "DELETE d FROM daily_metric d JOIN person p ON p.id = d.person_id WHERE p.name LIKE '{MERGE_PREFIX}%'"
        ),
        format!("DELETE FROM account WHERE anchor_id LIKE '{MERGE_PREFIX}%' OR douyin_no LIKE '{MERGE_PREFIX}%'"),
        format!("DELETE FROM import_pending WHERE name LIKE '{MERGE_PREFIX}%' OR douyin_no LIKE '{MERGE_PREFIX}%'"),
        "DELETE FROM import_batch WHERE filename LIKE '__e2e_%'".to_string(),
        format!("DELETE FROM person WHERE name LIKE '{MERGE_PREFIX}%'"),
    ] {
        let _ = sqlx::query(&sql).execute(pool).await;
    }
}

/// 直接调用 merge_person handler（Axum 提取器在集成测试里手工构造）
async fn call_merge(
    st: std::sync::Arc<AppState>,
    target_id: u64,
    source_id: u64,
) -> Result<serde_json::Value, ()> {
    match admin::merge_person(
        axum::extract::State(st),
        axum::extract::Path(target_id),
        axum::Json(admin::MergeBody {
            source_id: Some(source_id),
        }),
    )
    .await
    {
        Ok(Json(v)) => Ok(v),
        Err(_) => Err(()),
    }
}

/// 直接调用 unbind_account handler
async fn call_unbind(st: std::sync::Arc<AppState>, pid: u64, acc_id: u64) -> Result<(), ()> {
    match admin::unbind_account(axum::extract::State(st), axum::extract::Path((pid, acc_id))).await
    {
        Ok(_) => Ok(()),
        Err(_) => Err(()),
    }
}

/// P1-9：两人任一天都有数据（同日冲突）也能合并——冲突日求和、非冲突日照搬，不 500。
/// （修复前：先并值再整批改绑，撞 uk_person_date 唯一键整个合并 500。）
#[tokio::test]
async fn merge_person_combines_same_day_conflicts() {
    let Some(st) = setup().await else { return };
    if !need_writer(&st) {
        return;
    }
    let st = std::sync::Arc::new(st);
    let (target_id, source_id) = setup_two_people(&st).await;
    let target_anchor = format!("{MERGE_PREFIX}_anchor_t");
    let source_anchor = format!("{MERGE_PREFIX}_anchor_s");

    // 目标：D1 有 100；来源：D1 有 50（冲突日）、D2 有 70（独占日）
    insert_daily_row(&st, target_id, &target_anchor, E2E_DAILY_DATE, 100).await;
    insert_daily_row(&st, source_id, &source_anchor, E2E_DAILY_DATE, 50).await;
    insert_daily_row(&st, source_id, &source_anchor, E2E_DAILY_DATE2, 70).await;

    let out = call_merge(st.clone(), target_id, source_id)
        .await
        .expect("合并应成功（修复前同日冲突会撞 uk_person_date 整批 500）");
    assert_eq!(out["ok"], true);

    // 冲突日：100 + 50 = 150（目标行吸收来源行的量）
    assert_eq!(
        read_daily_wave(&st, target_id, E2E_DAILY_DATE).await,
        Some(150),
        "同日冲突要合并求和"
    );
    // 独占日：整行改绑到目标名下
    assert_eq!(
        read_daily_wave(&st, target_id, E2E_DAILY_DATE2).await,
        Some(70),
        "来源独占日要整行改绑"
    );
    // 来源名下不再有日明细
    assert_eq!(read_daily_wave(&st, source_id, E2E_DAILY_DATE).await, None);
    assert_eq!(read_daily_wave(&st, source_id, E2E_DAILY_DATE2).await, None);
    // 来源已软删、账号改绑到目标
    let pool = st.writer.as_ref().expect("writer");
    let src_deleted: i64 =
        sqlx::query("SELECT COUNT(*) FROM person WHERE id = ? AND deleted_at IS NOT NULL")
            .bind(source_id as i64)
            .fetch_one(pool)
            .await
            .unwrap()
            .try_get(0)
            .unwrap_or(0);
    assert_eq!(src_deleted, 1, "来源主播应软删");
    let moved: i64 =
        sqlx::query("SELECT COUNT(*) FROM account WHERE person_id = ? AND anchor_id = ?")
            .bind(target_id as i64)
            .bind(format!("{MERGE_PREFIX}_anchor_s"))
            .fetch_one(pool)
            .await
            .unwrap()
            .try_get(0)
            .unwrap_or(0);
    assert_eq!(moved, 1, "来源账号应改绑到目标名下");

    cleanup_admin_e2e(&st).await;
    assert_eq!(
        count_i64(
            &st,
            "SELECT COUNT(*) FROM daily_metric WHERE biz_date BETWEEN '2999-01-01' AND '2999-12-31'",
        )
        .await,
        0,
        "测试日明细已清理"
    );
}

/// P2-11：有账号但全部 is_primary=0 时，导入回填 anchor_id 前自动收敛出主号。
/// 走完整导入链路（commit_import）验证：修复前这类人 anchor_id=''（生产 1000001/1000004）。
#[tokio::test]
async fn ensure_primary_converges_during_import() {
    let Some(st) = setup().await else { return };
    if !need_writer(&st) {
        return;
    }
    let st = std::sync::Arc::new(st);
    let pool = st.writer.as_ref().expect("writer");
    let name = format!("{MERGE_PREFIX}主号收敛");
    let anchor = format!("{MERGE_PREFIX}_anchor_np");
    // 清理历史残留（姓名无唯一约束，但账号/明细要干净）
    let _ = sqlx::query("DELETE FROM person WHERE name = ?")
        .bind(&name)
        .execute(pool)
        .await;

    let pid = sqlx::query("INSERT INTO person (name, gender, status) VALUES (?, 'male', 'active')")
        .bind(&name)
        .execute(pool)
        .await
        .expect("建测试主播")
        .last_insert_id();
    // 复刻生产 1000001/1000004：有 account 行但 is_primary=0
    sqlx::query(
        "INSERT INTO account (person_id, anchor_id, douyin_no, is_primary) VALUES (?,?,?,0)",
    )
    .bind(pid as i64)
    .bind(&anchor)
    .bind(&anchor)
    .execute(pool)
    .await
    .expect("建无主号账号");
    // 收敛前：PRIMARY_ANCHOR_SQL 取不到（这正是 anchor_id='' 的根因）
    let before: Option<String> = sqlx::query(pk_server::dialect::PRIMARY_ANCHOR_SQL)
        .bind(pid as i64)
        .map(|r: sqlx::mysql::MySqlRow| r.try_get::<String, _>(0).unwrap_or_default())
        .fetch_optional(pool)
        .await
        .expect("查主号");
    assert_eq!(before, None, "收敛前不该有主号（复刻生产问题现场）");

    // 走导入：write_daily_row 里会先 ensure_primary 再回填 anchor_id
    let rows = vec![ImportRow {
        name: name.clone(),
        wave: Some(88.0),
        ..Default::default()
    }];
    let report = admin::commit_import(&st, "auto", E2E_DAILY_DATE, rows, "merge", "__e2e_np.csv")
        .await
        .expect("导入");
    assert_eq!(report.ok_rows, 1, "命中主播（按名字匹配）");

    // 收敛后：主号存在、anchor_id 就位
    let anchor_after: Option<String> = sqlx::query(pk_server::dialect::PRIMARY_ANCHOR_SQL)
        .bind(pid as i64)
        .map(|r: sqlx::mysql::MySqlRow| r.try_get::<String, _>(0).unwrap_or_default())
        .fetch_optional(pool)
        .await
        .expect("查主号");
    assert_eq!(
        anchor_after.as_deref(),
        Some(anchor.as_str()),
        "导入后该账号应被收敛为主号"
    );
    let dm_anchor: String =
        sqlx::query("SELECT anchor_id FROM daily_metric WHERE person_id = ? AND biz_date = ?")
            .bind(pid as i64)
            .bind(E2E_DAILY_DATE)
            .map(|r: sqlx::mysql::MySqlRow| r.try_get::<String, _>(0).unwrap_or_default())
            .fetch_one(pool)
            .await
            .expect("读日明细");
    assert_eq!(
        dm_anchor, anchor,
        "daily_metric.anchor_id 不再是空串（P2-11 的直接病灶）"
    );

    // 幂等：重导不应把主号改没（ensure_primary 二次调用零写入）
    let again = admin::commit_import(
        &st,
        "auto",
        E2E_DAILY_DATE,
        vec![ImportRow {
            name: name.clone(),
            wave: Some(88.0),
            ..Default::default()
        }],
        "merge",
        "__e2e_np.csv",
    )
    .await
    .expect("重导");
    assert_eq!(again.unchanged, 1, "同值重导不覆盖");
    let primary_cnt: i64 =
        sqlx::query("SELECT COUNT(*) FROM account WHERE person_id = ? AND is_primary = 1")
            .bind(pid as i64)
            .fetch_one(pool)
            .await
            .unwrap()
            .try_get(0)
            .unwrap_or(0);
    assert_eq!(primary_cnt, 1, "重导后主号有且只有一个");

    cleanup_admin_e2e(&st).await;
    assert_eq!(
        count_i64(
            &st,
            &format!("SELECT COUNT(*) FROM account WHERE person_id = {pid}"),
        )
        .await,
        0,
        "测试账号已清理"
    );
}

/// P2-10：解绑副号后 wave_snapshot 不产生孤儿——快照保留、汇总落 import_pending 留档。
#[tokio::test]
async fn unbind_account_keeps_snapshots_traced() {
    let Some(st) = setup().await else { return };
    if !need_writer(&st) {
        return;
    }
    let st = std::sync::Arc::new(st);
    let pool = st.writer.as_ref().expect("writer");
    let name = format!("{MERGE_PREFIX}解绑留档");
    let anchor = format!("{MERGE_PREFIX}_anchor_unbind");
    let douyin_no = format!("{MERGE_PREFIX}_no_unbind");
    let snap_date = "2999-02-01"; // 独立测试日期
    let snap_date2 = "2999-02-02";

    let pid = sqlx::query("INSERT INTO person (name, gender, status) VALUES (?, 'male', 'active')")
        .bind(&name)
        .execute(pool)
        .await
        .expect("建测试主播")
        .last_insert_id();
    let acc_id = sqlx::query(
        "INSERT INTO account (person_id, anchor_id, douyin_no, is_primary) VALUES (?,?,?,1)",
    )
    .bind(pid as i64)
    .bind(&anchor)
    .bind(&douyin_no)
    .execute(pool)
    .await
    .expect("建副号")
    .last_insert_id();
    // 该副号的历史快照（2 天）
    for (d, v) in [(snap_date, 500i64), (snap_date2, 700i64)] {
        sqlx::query("INSERT INTO wave_snapshot (anchor_id, biz_date, wave_value) VALUES (?,?,?)")
            .bind(&anchor)
            .bind(d)
            .bind(v)
            .execute(pool)
            .await
            .expect("写测试快照");
    }

    // 解绑（走 handler：内部处理快照留档）
    call_unbind(st.clone(), pid, acc_id)
        .await
        .expect("解绑应成功");

    // 1) 快照保留（不删历史）
    let snaps: i64 = sqlx::query("SELECT COUNT(*) FROM wave_snapshot WHERE anchor_id = ?")
        .bind(&anchor)
        .fetch_one(pool)
        .await
        .unwrap()
        .try_get(0)
        .unwrap_or(0);
    assert_eq!(snaps, 2, "快照必须保留，不能删历史");

    // 2) 账号已删
    let accounts: i64 = sqlx::query("SELECT COUNT(*) FROM account WHERE id = ?")
        .bind(acc_id as i64)
        .fetch_one(pool)
        .await
        .unwrap()
        .try_get(0)
        .unwrap_or(-1);
    assert_eq!(accounts, 0, "账号已解绑删除");

    // 3) 留档行进了 import_pending（按 anchor_id 认领、汇总合计 1200）
    let pending: Option<(String, i64)> = sqlx::query(
        "SELECT name, wave FROM import_pending WHERE douyin_no = ? AND absorbed_at IS NULL",
    )
    .bind(&anchor)
    .map(|r: sqlx::mysql::MySqlRow| {
        (
            r.try_get::<String, _>(0).unwrap_or_default(),
            r.try_get::<i64, _>(1).unwrap_or(0),
        )
    })
    .fetch_optional(pool)
    .await
    .expect("查留档行");
    let (pending_name, pending_wave) = pending.expect("解绑后应有留档行");
    assert_eq!(pending_wave, 1200, "留档合计 = 500 + 700");
    assert!(
        pending_name.contains(&douyin_no) && pending_name.contains("解绑"),
        "留档行要能看出是哪个号解绑的（实际：{pending_name}）"
    );

    // 4) 不存在的账号 → 仍报错（幂等性不回归）
    let err = call_unbind(st.clone(), pid, 999_999_999).await;
    assert!(err.is_err(), "不存在的账号要报错");

    cleanup_admin_e2e(&st).await;
    assert_eq!(
        count_i64(
            &st,
            &format!("SELECT COUNT(*) FROM wave_snapshot WHERE anchor_id = '{anchor}'"),
        )
        .await,
        0,
        "测试快照已清理"
    );
}
