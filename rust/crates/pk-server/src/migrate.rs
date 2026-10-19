//! 显式迁移 runner：`pk-server migrate`
//!
//! docs/00 §4 决策（2026-10-04，TypeSafe 确认）：
//! - DDL 只由独立的 `douyin_migrator` 账号执行，**不随服务启动自动跑**
//! - 通过 `pk-server migrate` 子命令（或容器内 `docker exec pk-server migrate`）显式触发
//! - 已应用的版本登记在 schema_migrations，重复执行只补未应用的

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Row};

use crate::config::DbEndpoint;

/// 编译进二进制的迁移脚本（按 version 顺序应用）
const MIGRATIONS: &[(&str, &str)] = &[
    ("001_core", include_str!("migrations/001_core.sql")),
    (
        "002_import_audit",
        include_str!("migrations/002_import_audit.sql"),
    ),
    ("003_runtime", include_str!("migrations/003_runtime.sql")),
    ("004_push", include_str!("migrations/004_push.sql")),
    (
        "005_push_delay",
        include_str!("migrations/005_push_delay.sql"),
    ),
    (
        "006_import_pending",
        include_str!("migrations/006_import_pending.sql"),
    ),
    (
        "007_monthly_duration",
        include_str!("migrations/007_monthly_duration.sql"),
    ),
    (
        "008_my_register",
        include_str!("migrations/008_my_register.sql"),
    ),
    (
        "009_anchor_convergence",
        include_str!("migrations/009_anchor_convergence.sql"),
    ),
    (
        "010_my_pass_hash",
        include_str!("migrations/010_my_pass_hash.sql"),
    ),
    ("011_ip_geo", include_str!("migrations/011_ip_geo.sql")),
    (
        "012_my_phone_unique",
        include_str!("migrations/012_my_phone_unique.sql"),
    ),
    (
        "013_wx_push_accounts",
        include_str!("migrations/013_wx_push_accounts.sql"),
    ),
    (
        "014_ip_allowlist",
        include_str!("migrations/014_ip_allowlist.sql"),
    ),
    (
        "015_identity_unify",
        include_str!("migrations/015_identity_unify.sql"),
    ),
    (
        "016_streamer_phone",
        include_str!("migrations/016_streamer_phone.sql"),
    ),
    (
        "017_my_bind_requests",
        include_str!("migrations/017_my_bind_requests.sql"),
    ),
    (
        "018_identity_uniqueness",
        include_str!("migrations/018_identity_uniqueness.sql"),
    ),
    (
        "019_monthly_metric_unique_index",
        include_str!("migrations/019_monthly_metric_unique_index.sql"),
    ),
    // 020 之前只存在于磁盘、没被 include_str! 引用 → `pk-server migrate` 永不执行它，
    // 001 里的索引名撞车因此一个都没补上。这里补登记。
    (
        "020_fix_index_collisions",
        include_str!("migrations/020_fix_index_collisions.sql"),
    ),
    // 021：uk_anchor_date 跨表撞名，wave_snapshot 的唯一索引从未建出（灌库 ON CONFLICT
    // 直接失败 + 音浪可重复计入）。
    (
        "021_fix_wave_snapshot_unique",
        include_str!("migrations/021_fix_wave_snapshot_unique.sql"),
    ),
    // 022：师门日报开关（push_bindings.lineage_report / last_lineage_date）+
    // 管理员消息下发三张表（docs/23）。
    (
        "022_admin_messages",
        include_str!("migrations/022_admin_messages.sql"),
    ),
    // 023：消息下发支持自定义上传图片（admin_messages 加 image_blob / image_mime /
    // image_name 三列，task-14）。DDL 全 IF NOT EXISTS，可安全重放。
    (
        "023_send_image",
        include_str!("migrations/023_send_image.sql"),
    ),
    // 024：展示端账号的「可看团别」（my_accounts.view_team：''/any/male/female/all）。
    // 让管理员观看账号能按团收敛（如饭团只看女团），并允许全团账号在页内切团。
    (
        "024_my_view_team",
        include_str!("migrations/024_my_view_team.sql"),
    ),
    // 025：主播抖音头像**本地留存**（person_avatar：图 + 原图 URL + hash + synced_at）。
    // 展示端「主播列表」显示真头像；抖音接口以后改/封了也有本地副本；
    // 每 7 天刷新靠 synced_at 判定（见 src/avatar_sync.rs、docs/25）。
    (
        "025_person_avatar",
        include_str!("migrations/025_person_avatar.sql"),
    ),
    // 026：月度音浪导入（monthly_wave 一人一月一行）。用户口径 2026-10-16：
    // 「时长或者音浪都支持按月进行导入和显示」—— 月度音浪与月度时长同构，
    // 查询端统一「某月有导入月值就用它，没有才回落日明细求和」。
    (
        "026_monthly_wave",
        include_str!("migrations/026_monthly_wave.sql"),
    ),
    // 027：微信 iLink 会话 key 放不下（conversation_id 64 → 191）。2026-10-16
    // 「二维码绑定有问题」的根因：key `wxacct:{bot_id}:{user_id}` 实测 65 字符，
    // 落在 wx_sessions / push_bindings / bot_sessions 的 VARCHAR(64) 上写不进去。
    (
        "027_weixin_conversation_len",
        include_str!("migrations/027_weixin_conversation_len.sql"),
    ),
    // 028：绑定码改 4 位数字（10^4 空间）→ push_bindings(bind_code) 加部分唯一索引，
    // 保证「一个活码只挂一个账号」（两行同码会把微信绑到别人的账号上）。
    (
        "028_push_bind_code_unique",
        include_str!("migrations/028_push_bind_code_unique.sql"),
    ),
    // 029：解绑 = 停止数据传输。push_bindings 加 paused 列（0/1 smallint）：
    // push_unbind 置 1 → wxpush 补拉跳过 + poll_body 一轮内退出（不再收发任何数据）；
    // 重新对接（push_bind_account / push_bind_by_code）置回 0 = 恢复。
    // 顺带把 daily_report 列默认值改 1：新对接用户每日报告默认开启（存量行不动）。
    (
        "029_push_bindings_paused",
        include_str!("migrations/029_push_bindings_paused.sql"),
    ),
    // 030：存量方案的 event_type 回写（2026-10-09 改名后落库的方案一直没人回扫，
    // 老值在展示端与后台都按 eventType 精确过滤 → 历史方案凭空消失，见审计 4-9）。
    // 幂等，重跑无副作用。
    (
        "030_normalize_plan_event_type",
        include_str!("migrations/030_normalize_plan_event_type.sql"),
    ),
    // 031：daily_metric.anchor_id 回扫对齐当前主号（换主号后旧行不自愈，
    // 分号口径主号音浪会整段消失，见审计 P1-4）。幂等。
    (
        "031_fix_daily_metric_anchor",
        include_str!("migrations/031_fix_daily_metric_anchor.sql"),
    ),
    // 032：查询索引补齐（榜单/分号/方案列表的公共过滤条件）+ 清掉 001 里重复定义的
    // idx_name。纯索引改动，不改表结构不动语义，幂等可重跑。
    (
        "032_query_indexes",
        include_str!("migrations/032_query_indexes.sql"),
    ),
    // 033：对接码防爆破计数表（2026-10-17 审计 4-P1-2）。按码失败 ≥5 次作废该码、
    // 全局 1 小时失败 ≥600 次冷却 10 分钟，语义在 store/wxpush.rs::bind_code_fail_*。
    (
        "033_bind_code_fails",
        include_str!("migrations/033_bind_code_fails.sql"),
    ),
    // 034：推送队列 stale 抢占修复（2026-10-17 审计 6-P2-2）。push_jobs 加 taken_at
    // （进入 running 的时刻），stale 判定改看它 —— 原先用 created_at（入队时间）
    // 恒命中 5 分钟条件，长任务 60 秒必被并发重抢 → 重复推送。
    (
        "034_push_jobs_taken_at",
        include_str!("migrations/034_push_jobs_taken_at.sql"),
    ),
    // 035：扫码无主凭证收紧（2026-10-17 审计 4-P2-2）。wx_push_accounts 加
    // legacy_no_uid —— 激活的「无主兼容分支」只对历史存量行保留，
    // 新扫码（user_id 缺失）已在 wx.rs 确认侧直接拒绝，不再产生可冒认激活的行。
    (
        "035_wx_legacy_no_uid",
        include_str!("migrations/035_wx_legacy_no_uid.sql"),
    ),
    // 036：对接码加固（2026-10-17 第四轮审计 P1-2 / P2-10 / P2-11）。
    // bind_code_fails.code 放宽到 VARCHAR(160)（容纳后续会话维度键）；回填 035
    // 的存量「双空无主行」标记（legacy_no_uid=1）。幂等，可安全重放。
    (
        "036_bind_code_hardening",
        include_str!("migrations/036_bind_code_hardening.sql"),
    ),
    // 037：推送去重租约（2026-10-17 第四轮审计 P2-1 / P2-2 / P2-3）。
    // 把「谁占了哪个去重标记、占位前的旧值」持久化，崩溃/重启后可还原，
    // 并给 run() 单飞提供跨实例互斥。幂等，可安全重放。
    (
        "037_push_claim_leases",
        include_str!("migrations/037_push_claim_leases.sql"),
    ),
    // 038：展示端账号绑定邮箱（2026-10-17 用户口径）。忘记密码重置改为
    // 「随机密码发绑定邮箱」，email 列留空 = 未绑定。幂等，可安全重放。
    (
        "038_my_account_email",
        include_str!("migrations/038_my_account_email.sql"),
    ),
    // 039：忘记密码改「临时密码」模型（2026-10-18 用户拍板，契约
    // docs/26-账号绑定QQ邮箱与艺名登录-设计/05-实现契约-v2.md §1）。
    // 只加 3 列 temp_pass_hash / temp_pass_exp / must_change_pass；不新增 qq 列
    // （QQ 号复用 038 的 email，存 <qq>@qq.com）。幂等，可安全重放。
    (
        "039_my_temp_pass",
        include_str!("migrations/039_my_temp_pass.sql"),
    ),
    // 040：绑定邮箱（QQ 邮箱）全局唯一 —— 「同一 QQ 不能绑多个账号」的库层兜底；
    // 应用层先查重给友好文案，并发靠 23505 映射。空串不参与唯一。幂等。
    (
        "040_my_email_uniq",
        include_str!("migrations/040_my_email_uniq.sql"),
    ),
    // 041：在职主播艺名唯一 —— 「凭艺名认领开号」的前提（一个艺名只对应一个人）。
    // partial unique：只约束 deleted_at IS NULL 且 name <> '' 的行（软删/空名不参与）。
    (
        "041_person_name_uniq",
        include_str!("migrations/041_person_name_uniq.sql"),
    ),
    // 042：月度导入未匹配行暂存（2026-10-18「先有数据后绑定，数据要同步过来」）。
    // 月度值不能再摊进按日的 import_pending，单开同构表按 period 暂存，
    // 建档 / 绑号后由 absorb_pending_month 补进 monthly_wave / monthly_duration。
    (
        "042_import_pending_month",
        include_str!("migrations/042_import_pending_month.sql"),
    ),
    // 043：plans.created_at 列型对齐（2026-10-18 实机修复）。001_core 定义是
    // VARCHAR(40)，但早期/恢复出来的库里这列可能是 TIMESTAMPTZ —— 代码按 String
    // 解码、写 RFC3339 文本，列型不对就「方案列表恒空 + 保存方案 500」。
    // 幂等（列已是 varchar 则跳过），可安全重放。
    (
        "043_plans_created_at_text",
        include_str!("migrations/043_plans_created_at_text.sql"),
    ),
    // 044：直播 PK 实时监控与赛事运行态（2026-10-20）。plans 只是赛前分组快照，
    // 实时分数每几秒刷新，不能写进 payload；这里单开赛事/阶段/小组/对局/席位/
    // 采样/时间线/晋级/监控房间/采集 worker/推送作业各组表。
    // battle_id / room_id / UID 一律 VARCHAR（19 位超 JS 安全整数）。幂等。
    ("044_live_pk", include_str!("migrations/044_live_pk.sql")),
];

/// 迁移建连（migrator 账号；同样关语句缓存）
async fn connect(ep: &DbEndpoint) -> anyhow::Result<PgPool> {
    let opts: PgConnectOptions = ep
        .url()
        .parse()
        .map_err(|e| anyhow::anyhow!("迁移库连接串解析失败: {e}"))?;
    PgPoolOptions::new()
        .max_connections(ep.pool_limit)
        .min_connections(0)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .idle_timeout(std::time::Duration::from_secs(30))
        .connect_with(opts)
        .await
        .map_err(|e| anyhow::anyhow!("迁移库连接失败（账号 {}）: {e}", ep.user))
}

/// 把迁移脚本拆成单条语句：去掉 `--` 注释行后按 `;` 分割。
/// PG 版注意：`$$...$$`（plpgsql 函数体）内部的分号不是语句边界，先保护再切。
fn split_statements(sql: &str) -> Vec<String> {
    let no_comments: String = sql
        .lines()
        .filter(|l| !l.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    // 把 $$...$$ 段落替换成不含分号的占位符后按 ; 切，切完再还原
    let mut dollar_bodies: Vec<String> = Vec::new();
    let mut masked = String::with_capacity(no_comments.len());
    let mut rest = no_comments.as_str();
    while let Some(pos) = rest.find("$$") {
        // 第一个 $$ 之前的内容原样保留（去掉 $$ 本身）
        masked.push_str(&rest[..pos]);
        rest = &rest[pos + 2..];
        if let Some(end) = rest.find("$$") {
            // 整个函数体（含两侧 $$）作为占位符替身，内部无 ';'
            dollar_bodies.push(format!("$$ {} $$", &rest[..end]));
            masked.push_str(&format!("\x00DOLLAR{}\x00", dollar_bodies.len() - 1));
            rest = &rest[end + 2..];
        } else {
            // 未闭合（不该发生）：原样返回剩余
            masked.push_str("$$");
            masked.push_str(rest);
            rest = "";
            break;
        }
    }
    masked.push_str(rest);
    let mut chunks: Vec<String> = Vec::new();
    for stmt in masked.split(';') {
        let t = stmt.trim();
        if t.is_empty() {
            continue;
        }
        // 还原被保护的 $$...$$ 段落
        let mut restored = t.to_string();
        for (i, body) in dollar_bodies.iter().enumerate() {
            restored = restored.replace(&format!("\x00DOLLAR{i}\x00"), body);
        }
        chunks.push(restored);
    }
    chunks
}

/// 识别「迁移账号不是表属主」类错误（docs/22-数据库运维说明.md）。
///
/// 背景（2026-10-14 本机实踩）：douyin_dev 的表属主是 zmy，而 rust/.env 里
/// DB_WRITER_USER=pk_writer —— 019 的 `CREATE INDEX` 一执行就报
/// `must be owner of table monthly_metric`。schema_migrations 里 019 没登记，
/// 下次 migrate 会原样再失败一遍：这类错误永远不会自愈，必须人工把属主
/// 移交过来（scripts/deploy/bootstrap-roles-64.sql §5）或用属主账号跑迁移。
///
/// PG 的权限报错分四类，全部归一识别（SQLSTATE 0LP01 也算上——它出现在
/// ALTER TABLE ... OWNER 需要的额外前置上）：
/// - `must be owner of table x` / `... of relation x`（CREATE INDEX / ALTER TABLE）
/// - `permission denied for table x` / `... for relation x`（GRANT 系 DDL）
/// - `must have create privilege on schema public`（新表落在别人拥有的 schema）
/// - `must be superuser` / `permission denied to change owner`（改属主本身）
///
/// 返回 (命中与否, 表名)。表名从报错原文里截取：拿不到就返回 None，
/// **不做猜测** —— 文案里宁可让运维去看完整报错，也不能指错表。
fn detect_ownership_error(msg: &str) -> (bool, Option<String>) {
    let m = msg.to_ascii_lowercase();
    let hit = m.contains("must be owner of")
        || m.contains("permission denied for table")
        || m.contains("permission denied for relation")
        || m.contains("must have create privilege on schema")
        || m.contains("must be superuser")
        || m.contains("permission denied to change owner");
    let table = ["table ", "relation "].iter().find_map(|tag| {
        let idx = m.find(tag)?;
        let rest = msg[idx + tag.len()..].trim_start();
        // 带引号的对象名（可含空格/标点，PG 对大小写敏感名会加引号）：取到配对引号为止
        if let Some(quoted) = rest.strip_prefix('"') {
            let end = quoted.find('"')?;
            let name = &quoted[..end];
            return (!name.is_empty()).then(|| name.to_string());
        }
        // 裸对象名：字母数字 + _ $ .（schema 限定名含点）；结尾标点不属于名字
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || "_$.".contains(*c))
            .collect();
        let name = name.trim_end_matches('.').to_string();
        (!name.is_empty()).then_some(name)
    });
    (hit, table)
}

/// 把权限类执行错误翻译成可操作的中文处置说明（不自动改权限，改属主必须人工确认）。
fn ownership_hint(msg: &str, user: &str) -> Option<String> {
    let (hit, table) = detect_ownership_error(msg);
    if !hit {
        return None;
    }
    let target = table.as_deref().unwrap_or("报错指向的对象");
    Some(format!(
        "\n\n【属主权限不足】迁移账号 {user} 不是表 {target} 的属主，这条 DDL 被拒绝。\
迁移**不会自愈**：schema_migrations 里该版本未登记，重跑还是同一个错。二选一处置：\n\
  1) 由属主账号执行一次移交（推荐，生产=64 上容器内 psql）：\n\
     ALTER TABLE {target} OWNER TO {user};\n\
     （全库一次到位：跑 scripts/deploy/bootstrap-roles-64.sh，见 docs/22-数据库运维说明.md）\n\
  2) 换属主账号跑迁移：pk-server migrate 改用表的属主连接（DB_WRITER_USER=属主）。\n\
查当前属主：SELECT tablename, tableowner FROM pg_tables WHERE schemaname='public' AND tablename='{target}';"
    ))
}

/// 执行迁移，返回本次应用的版本列表。幂等，可重复执行。
pub async fn run(ep: &DbEndpoint) -> anyhow::Result<Vec<String>> {
    let pool = connect(ep).await?;
    run_with(&pool, &ep.user).await
}

/// `user` 只用于权限报错的中文提示文案（谁是迁移账号，提示就指谁）。
pub async fn run_with(pool: &PgPool, user: &str) -> anyhow::Result<Vec<String>> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version VARCHAR(64) NOT NULL,
            applied_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (version)
        )",
    )
    .execute(pool)
    .await?;

    let applied_before: Vec<String> =
        sqlx::query(&crate::pq::rewrite("SELECT version FROM schema_migrations"))
            .fetch_all(pool)
            .await?
            .iter()
            .filter_map(|r| r.try_get::<String, _>(0).ok())
            .collect();

    let mut applied_now = Vec::new();
    for (version, sql) in MIGRATIONS {
        if applied_before.iter().any(|v| v == version) {
            tracing::info!(version, "迁移已应用，跳过");
            continue;
        }
        for stmt in split_statements(sql) {
            // 走 raw_sql：迁移脚本没有任何绑定参数，raw_sql 支持 DDL / 多语句。
            if let Err(e) = sqlx::raw_sql(&stmt).execute(pool).await {
                // 权限不足（账号不是表属主等）→ 给出可操作处置说明，别让运维只看到一句裸英文。
                let msg = format!("{e}");
                if let Some(hint) = ownership_hint(&msg, user) {
                    tracing::error!(version, hint = %hint.trim(), "迁移因数据库属主权限失败");
                    anyhow::bail!("迁移 {version} 执行失败: {e}\n语句: {stmt}{hint}");
                }
                anyhow::bail!("迁移 {version} 执行失败: {e}\n语句: {stmt}");
            }
        }
        sqlx::query(&crate::pq::rewrite(
            "INSERT INTO schema_migrations (version) VALUES (?)",
        ))
        .bind(version)
        .execute(pool)
        .await?;
        tracing::info!(version, "迁移已应用");
        applied_now.push(version.to_string());
    }
    Ok(applied_now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_statements_and_strips_comments() {
        let sql = "-- 注释 ; 里有分号也不影响\nCREATE TABLE a (id INT);\n\nALTER TABLE a\n  ADD COLUMN x INT;\n";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].starts_with("CREATE TABLE a"));
        assert!(stmts[1].starts_with("ALTER TABLE a"));
    }

    #[test]
    fn embedded_migrations_split_cleanly() {
        for (version, sql) in MIGRATIONS {
            let stmts = split_statements(sql);
            assert!(!stmts.is_empty(), "{version} 至少要有一条语句");
            for s in &stmts {
                let upper = s.to_ascii_uppercase();
                assert!(
                    upper.starts_with("CREATE")
                        || upper.starts_with("ALTER")
                        || upper.starts_with("INSERT")
                        || upper.starts_with("UPDATE")
                        || upper.starts_with("DROP TRIGGER")
                        || upper.starts_with("SELECT")
                        // 043 起允许 DO $$ ... $$ 匿名块（幂等条件迁移的唯一手段，
                        // split_statements 已保证块体作为一条完整语句）。
                        || upper.starts_with("DO"),
                    "{version} 里出现意外语句: {s}"
                );
            }
        }
    }

    #[test]
    fn versions_are_ordered() {
        let mut names: Vec<&str> = MIGRATIONS.iter().map(|(v, _)| *v).collect();
        let sorted = names.clone();
        names.sort();
        assert_eq!(names, sorted, "迁移必须按文件名顺序排列");
    }

    /// 2026-10-14 实踩：douyin_dev 表属主是 zmy，pk_writer 跑 019 的
    /// CREATE INDEX 报 must be owner —— 必须被识别成「属主权限」类错误。
    #[test]
    fn ownership_error_detection() {
        // PG 真实报错原文（sqlx Display 里就是这段）
        for (msg, table) in [
            (
                "must be owner of table monthly_metric",
                Some("monthly_metric"),
            ),
            (
                "error returned from database: must be owner of relation monthly_metric",
                Some("monthly_metric"),
            ),
            (
                "permission denied for table import_pending",
                Some("import_pending"),
            ),
            (
                "permission denied for relation wave_snapshot",
                Some("wave_snapshot"),
            ),
            ("must have create privilege on schema public", None),
            ("must be superuser to alter owners", None),
            ("permission denied to change owner", None),
        ] {
            let (hit, got) = detect_ownership_error(msg);
            assert!(hit, "应识别为属主权限错误: {msg}");
            assert_eq!(got.as_deref(), table, "表名提取错: {msg}");
        }

        // 普通错误绝不能误判（否则 migrate 报错会被错误文案带偏）
        for msg in [
            "there is no unique or exclusion constraint matching the ON CONFLICT specification",
            "relation \"schema_migrations\" already exists",
            "column \"wave\" does not exist",
        ] {
            assert!(!detect_ownership_error(msg).0, "误判成属主错误: {msg}");
        }
    }

    /// 表名截取的边界：不吞标点、不带引号、拿不到就 None（不猜）。
    #[test]
    fn ownership_table_extraction_edges() {
        // 带引号的对象名
        assert_eq!(
            detect_ownership_error("must be owner of table \"My Table\"").1,
            Some("My Table".into()),
            "带引号对象名要剥引号"
        );
        // 句号结尾（sqlx 拼接文案）不吞标点
        let (hit, got) = detect_ownership_error("must be owner of table monthly_metric.");
        assert!(hit);
        assert_eq!(got.as_deref(), Some("monthly_metric"));
        // 摸不到对象名 → None，不硬猜
        assert_eq!(
            detect_ownership_error("must have create privilege on schema public").1,
            None
        );
    }

    /// 提示文案：点名账号与表、给出 ALTER TABLE 语句和 runbook 指引。
    #[test]
    fn ownership_hint_names_account_and_table() {
        let h = ownership_hint("must be owner of table monthly_metric", "pk_writer")
            .expect("属主错误必须给出提示");
        assert!(h.contains("pk_writer"));
        assert!(h.contains("monthly_metric"));
        assert!(h.contains("ALTER TABLE monthly_metric OWNER TO pk_writer"));
        assert!(h.contains("bootstrap-roles-64.sh"));
        assert!(h.contains("docs/22-数据库运维说明.md"));
        // 普通错误不附加提示
        assert!(ownership_hint("column \"x\" does not exist", "pk_writer").is_none());
    }

    /// 磁盘上的每个 `migrations/0xx_*.sql` 都必须被登记进 MIGRATIONS
    /// （2026-10-14 审计：020 文件存在却漏登记 → 索引撞车修复从未执行）。
    /// 上一条「编号连续」测试防不住这种漏登记——它只检查已登记项之间的关系。
    #[test]
    fn every_migration_file_is_registered() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/migrations");
        let mut on_disk: Vec<String> = std::fs::read_dir(dir)
            .expect("读取 migrations 目录失败")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".sql"))
            .map(|n| n.trim_end_matches(".sql").to_string())
            .collect();
        on_disk.sort();

        let mut registered: Vec<&str> = MIGRATIONS.iter().map(|(v, _)| *v).collect();
        registered.sort();

        let missing: Vec<&String> = on_disk
            .iter()
            .filter(|n| !registered.contains(&n.as_str()))
            .collect();
        let orphan: Vec<&&str> = registered
            .iter()
            .filter(|v| !on_disk.iter().any(|n| n == *v))
            .collect();
        assert!(
            missing.is_empty(),
            "这些迁移文件在磁盘上但没被 include_str! 登记（永远不会被应用）: {missing:?}"
        );
        assert!(
            orphan.is_empty(),
            "MIGRATIONS 登记了但磁盘上没有对应文件（编译期就该报错）: {orphan:?}"
        );
    }

    /// 编号连续且无重复（xxx_name 前缀，001 起），防止登记时漏号/撞号。
    #[test]
    fn version_numbers_are_unique_and_contiguous() {
        let mut nums: Vec<u32> = MIGRATIONS
            .iter()
            .map(|(v, _)| {
                let (num, rest) = v.split_once('_').unwrap_or_else(|| {
                    panic!("迁移版本 {v} 不符合 xxx_name 命名");
                });
                assert!(!rest.is_empty(), "迁移版本 {v} 缺少下划线后的名字");
                num.parse()
                    .unwrap_or_else(|_| panic!("迁移版本 {v} 的编号 {num} 不是数字"))
            })
            .collect();
        nums.sort_unstable();
        nums.dedup();
        assert_eq!(
            nums,
            (1..=nums.len() as u32).collect::<Vec<_>>(),
            "迁移编号必须从 001 起连续且无重复"
        );
    }
}
