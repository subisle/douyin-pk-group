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
    assert!(store::list_aliases(&st)
        .await
        .get("__e2e_alias__")
        .is_none());
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
    assert_eq!(
        store::anchor_claim_list(&st, "pending", 200)
            .await
            .iter()
            .any(|r| r["id"] == id),
        true
    );
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
