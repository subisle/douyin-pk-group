//! 抖音头像同步 + 本地留存（`pk-server sync-avatars`）
//!
//! 背景（2026-10-16 用户口径）：
//!   1. 展示端主界面「主播列表」要显示主播的**抖音真实头像**；
//!   2. **已注销的账号不用取头像**；
//!   3. 头像要**本地留存** —— 抖音那个页面接口是非官方契约，哪天改/封，
//!      页面照样有图（这是「防止以后更新不了」的兜底）；
//!   4. **每 7 天刷一遍**，由服务内定时器驱动，不依赖外部 cron；
//!   5. **后续新加的主播自动纳入**，且**全部在册主播都同步**（不看是否展示；
//!      展示不展示由后端榜单口径决定）。
//!
//! ## 为什么不写 `person.avatar_url`、也不落文件系统
//! 图 + 元数据进 `person_avatar`（migration 025），口径同 023 对 admin_messages
//! 图片的处理：图片单存一列，列表/详情只投影小字段；文件系统副本不随库走
//! （换机/恢复/迁移就丢），本项目的事实源是 PG。展示端也不再直连抖音 CDN，
//! 而是走同源 `GET /api/public/avatars/{person_id}?v={hash}`（T2 实现）——
//! 所以这里唯一的职责是「把图、来源、内容指纹可靠地落到本地」。
//!
//! ## 数据来源（两步）
//! ① `GET https://www.iesdouyin.com/web/api/v2/user/info/?unique_id=<抖音号>`
//!    （iPhone UA，免签名，实测可用；解析见 [`parse_lookup`]）
//! ② `GET https://p3.douyinpic.com/aweme/200x200/<uri>.jpeg`
//!    （CDN 尺寸桶**只认 100/200/720/1080**，其它尺寸直接 400）
//!
//! ## 写入语义（五句话，验收就照这五条看）
//! - **新主播自动纳入**：候选 = 有抖音号的人里 `person_avatar` **无行或已过期**；
//!   没登记抖音号的人不写行、不计候选 —— 以后补登记抖音号时立刻被捞到。
//! - **全部号都注销才清空**：`special_state_info.special_state=1` 只说明**那个号**
//!   注销了；一个人名下可能有好几个号（主号 + 备用号），所以**看完所有号**才下结论 ——
//!   任一账号还能出图就用它（活号优先），只有**名下全部号都注销**才把本地图置空 +
//!   `note='deregistered'`（用户口径：注销的不用头像，但活人不能被主号注销误清）。
//! - **查不到保留旧图**：`status_code≠0` / 有 user_info 但没头像字段 →
//!   **image / mime / hash / source_url 一个字都不动**，只写 `note='not_found'` +
//!   `synced_at`（见 [`note_not_found`]）。2026-10-16 复核口径：抖音哪天改成
//!   「合法 JSON 但 status_code 非 0」，一轮自动同步不能把全部留存的头像清空，
//!   否则本地留存的意义当场归零；首次探测就没行的人仍插一行 `image=NULL`，
//!   保持「有抖音号的人都有行」的断言。7 天后再试。
//! - **失败保留旧图**：网络/解析/HTTP 状态码不对/非 JPEG/超限 → **一个字都不写**，
//!   上一次的图原样留着，下轮再试。
//! - **7 天新鲜度**：`synced_at` 是唯一判据（「查不到 / 已注销」也算探测过），
//!   所以服务每次启动不会去戳抖音；只有 `--force` 才无条件重抓。
//!
//! [`run`] 同时给子命令和 [`spawn_scheduler`] 复用：幂等、可反复跑。

use anyhow::{bail, Context};
use serde_json::Value;
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use std::time::Duration;

/// iPhone Safari UA：老接口对桌面 UA 走风控，实测这个稳定返回数据。
const UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) \
     AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";

/// 抖音开放页面接口（非官方契约，失效时改这里 + docs/25）
const USER_INFO: &str = "https://www.iesdouyin.com/web/api/v2/user/info/";

/// 每张头像的 CDN 前缀（p3 是实测可用分片，p26/p11 等价）
const AVATAR_CDN: &str = "https://p3.douyinpic.com/aweme";

/// 头像尺寸桶：抖音 CDN **只认 100/200/720/1080 这几档**（其它尺寸返回 400 JSON）。
/// 200 在 2x 屏上撑 80px 的领奖台头像足够，单张约 3KB（720 要 16KB）。
const AVATAR_SIZE: &str = "200x200";

/// 单张头像的字节上限：正常 200x200 约 3KB，64KB 足够宽松；
/// 超了说明拿到的不是我们要的那个尺寸桶（或 CDN 返回了别的东西），宁可判失败。
const MAX_IMAGE_BYTES: usize = 64 * 1024;

/// 内容指纹长度：`person_avatar.hash` 是 VARCHAR(16)，取 md5(hex) 前 16 位。
/// 展示端拿它拼 `?v=<hash>` 破缓存，所以对「内容变了」必须敏感。
const HASH_LEN: usize = 16;

/// `person_avatar.source_url` 是 VARCHAR(512)：超长的 uri 直接判失败，
/// 不让一条脏数据把整轮同步的 SQL 打断。
const MAX_SOURCE_URL: usize = 512;

/// 探测结论落进 `person_avatar.note` 的两个取值（'' 表示正常有图）
const NOTE_DEREGISTERED: &str = "deregistered";
const NOTE_NOT_FOUND: &str = "not_found";

pub const USAGE: &str =
    "用法：pk-server sync-avatars [--dry-run] [--force] [--limit N] [--days N] [--delay-ms N]

  把在册主播的抖音头像抓下来，图和元数据留在本地 person_avatar 表
  （展示端「主播列表」经 /api/public/avatars/<person_id> 读它）。

  范围：全部未删除主播；没登记抖音号的人不写行、也不计入候选。
  新鲜度：person_avatar 无行或 synced_at 超过 N 天（默认 7 天）才重抓。

  --dry-run       只探测打印，不写库
  --force         忽略新鲜度，全部重抓一遍
  --limit N       最多处理 N 个候选主播（调试用）
  --days N        新鲜度天数，默认 7
  --delay-ms N    每次请求之间的间隔，默认 600ms（别打快，接口/CDN 会限流）
  -h, --help      打印本说明";

/// 同步参数
#[derive(Debug, Clone)]
pub struct Options {
    /// 只探测打印，不写库
    pub dry_run: bool,
    /// 忽略新鲜度（等价于 `--days 0` 的语义，但更直白）：无条件重抓
    pub force: bool,
    /// 最多处理多少个候选主播（调试用；多抖音号的仍会依次探测）
    pub limit: Option<usize>,
    /// 每次 HTTP 请求之间的间隔（节流）
    pub delay: Duration,
    /// 新鲜度天数：`synced_at` 超过它就重抓
    pub days: i64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            dry_run: false,
            force: false,
            limit: None,
            delay: Duration::from_millis(600),
            days: 7,
        }
    }
}

impl Options {
    /// 解析子命令后面的参数（`args` 里不含 `sync-avatars` 本身）。
    /// 只支持 `--k v` / `--k=v` 两种写法，未知参数直接报错，避免静默忽略拼错的开关。
    pub fn from_args<I: IntoIterator<Item = String>>(args: I) -> anyhow::Result<Self> {
        let mut o = Options::default();
        let mut it = args.into_iter().peekable();
        while let Some(a) = it.next() {
            let (key, inline) = match a.split_once('=') {
                Some((k, v)) => (k.to_string(), Some(v.to_string())),
                None => (a.clone(), None),
            };
            let mut take_value = |name: &str| -> anyhow::Result<String> {
                inline
                    .clone()
                    .or_else(|| it.next())
                    .ok_or_else(|| anyhow::anyhow!("{name} 缺少参数值"))
            };
            match key.as_str() {
                "--dry-run" => o.dry_run = true,
                "--force" => o.force = true,
                "--limit" => {
                    let v = take_value("--limit")?;
                    o.limit = Some(
                        v.parse()
                            .map_err(|_| anyhow::anyhow!("--limit 需要正整数，收到 {v}"))?,
                    );
                }
                "--delay-ms" => {
                    let v = take_value("--delay-ms")?;
                    let ms: u64 = v
                        .parse()
                        .map_err(|_| anyhow::anyhow!("--delay-ms 需要整数，收到 {v}"))?;
                    o.delay = Duration::from_millis(ms);
                }
                "--days" => {
                    let v = take_value("--days")?;
                    let d: i64 = v
                        .parse()
                        .map_err(|_| anyhow::anyhow!("--days 需要整数，收到 {v}"))?;
                    if d < 0 {
                        bail!("--days 不能为负，收到 {v}");
                    }
                    o.days = d;
                }
                // 帮助走 `Err` 通道：main.rs 统一打印后退出，和现有子命令口径一致
                "-h" | "--help" => bail!("{USAGE}"),
                other => bail!("未知参数 {other}\n\n{USAGE}"),
            }
        }
        Ok(o)
    }
}

/// 单个人的探测结论
#[derive(Debug, PartialEq, Eq)]
enum Lookup {
    /// 拿到头像资产（已拼成 CDN URL）
    Found(String),
    /// **这个号**已注销；注意「名下所有号都注销」才是清图条件，见 [`verdict_of`]
    Deregistered,
    /// 号查不到（改名/号废/接口换了）：**保留已留存的图**，只记 'not_found' +
    /// synced_at，7 天后再试（2026-10-16 复核口径，见 [`note_not_found`]）
    NotFound(String),
}

/// 一轮同步的结果（`report()` 供 main / 定时器打印）
#[derive(Debug, Default)]
pub struct Summary {
    /// 本轮候选主播数（有抖音号 且 无行/已过期；**不受 --limit 截断**）
    pub candidates: usize,
    /// 实际发起的探测次数（一个人多号会多次）
    pub probed: usize,
    /// 成功取到图并通过校验的人数（= updated + unchanged）
    pub downloaded: usize,
    /// 图真的换了、写入新图的人数
    pub updated: usize,
    /// 图没变、只把 synced_at 推到现在的人数
    pub unchanged: usize,
    /// 因为「已注销」把**已有的图**置空的次数（not_found 不清图，只记 note）
    pub cleared: usize,
    /// 判定为已注销的人数
    pub deregistered: usize,
    /// 本轮判定为号查不到的人数
    pub not_found: usize,
    /// not_found 里本来就有图、这次原样保住的人数（留存要求的直接证据）
    pub kept_image: usize,
    /// 没登记抖音号的人（不写行、不计候选）
    pub no_douyin_no: usize,
    /// 有抖音号但还在新鲜期内、本轮跳过的人
    pub fresh_skipped: usize,
    /// 失败人数（网络/解析/非 JPEG/超限）：**不写库，保留旧图**
    pub failed: usize,
}

impl Summary {
    pub fn report(&self, dry_run: bool, days: i64) -> String {
        format!(
            "头像同步{}（新鲜度 {days} 天）：候选 {} 人（另有未登记抖音号 {} 人 · 未过期跳过 {} 人），探测 {} 次\n  \
             留存 {} 张（新图 {} · 无变化 {}）· 已注销 {} 人（清空原图 {} 人）\n  \
             号查不到 {} 人（其中保住原图 {} 人）· 失败 {} 人（保留上一次的图，下轮再试）",
            if dry_run { "（--dry-run，未写库）" } else { "" },
            self.candidates,
            self.no_douyin_no,
            self.fresh_skipped,
            self.probed,
            self.downloaded,
            self.updated,
            self.unchanged,
            self.deregistered,
            self.cleared,
            self.not_found,
            self.kept_image,
            self.failed,
        )
    }
}

/// 一个在册主播（含 `person_avatar` 现有状态，用于判断「图有没有变」）
#[derive(Debug)]
struct Candidate {
    person_id: u64,
    name: String,
    /// person_avatar 里已经有图（已注销 → 计入 cleared；号查不到 → 计入 kept_image）
    has_image: bool,
    /// person_avatar 里记的内容指纹（判断「图没变，只刷 synced_at」）
    hash: String,
    /// 名下抖音号，按 `is_primary DESC, id` 排好序（多号依次试）
    douyin_nos: Vec<String>,
}

/// 本轮同步范围：在册主播的三种去处
#[derive(Debug, Default)]
struct Scope {
    /// 需要真探测的人：有抖音号 + （无 person_avatar 行 或 synced_at 超期）
    candidates: Vec<Candidate>,
    /// 有抖音号但还在新鲜期内
    fresh_skipped: usize,
    /// 一个抖音号都没登记（不写行、不计候选）
    no_douyin_no: usize,
}

/// 主流程：探测 → 写 `person_avatar`。幂等，可反复跑。
pub async fn run(pool: &PgPool, opt: &Options) -> anyhow::Result<Summary> {
    let scope = load_scope(pool, opt).await?;
    let mut sum = Summary {
        candidates: scope.candidates.len(),
        fresh_skipped: scope.fresh_skipped,
        no_douyin_no: scope.no_douyin_no,
        ..Default::default()
    };
    // 没有候选就别建 HTTP 客户端：定时器大部分轮次走的就是这条空转路径
    if scope.candidates.is_empty() {
        return Ok(sum);
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        // 连接建立阶段的超时：本机走代理时曾出现「连接池挂死 40 分钟」——
        // reqwest 默认 connect 阶段无上限，代理半死不活时整个同步器卡在
        // TLS/CONN 握手上，10s 超时保证坏节点快速失败进下一轮重试。
        .connect_timeout(Duration::from_secs(10))
        // 池策略：空闲连接只留 30s、每主机最多 2 条。抖音/代理侧常单方面掐
        // 长连接，留在池里的「半死连接」下一个请求复用就挂死；掐短 + 少留，
        // 代价只是每次多一次握手（同步器本来就 600ms/请求串行，不在乎）。
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(2)
        .build()
        .context("构建 HTTP 客户端失败")?;

    let total = scope.candidates.len();
    for (i, p) in scope.candidates.iter().enumerate() {
        if let Some(limit) = opt.limit {
            if i >= limit {
                break;
            }
        }
        let tag = format!("[{}/{}] {} (id {})", i + 1, total, p.name, p.person_id);
        match probe_person(&client, p, opt, &mut sum).await {
            PersonOutcome::Image { url, bytes, hash } => {
                sum.downloaded += 1;
                if p.has_image && p.hash == hash {
                    // hash 由内容算出，hash 相同即内容相同：只推 synced_at，省一次 3KB BYTEA 写入
                    sum.unchanged += 1;
                    println!("{tag} 头像无变化（只刷新 synced_at）");
                    if !opt.dry_run {
                        touch_image(pool, p.person_id).await?;
                    }
                } else {
                    sum.updated += 1;
                    println!(
                        "{tag} 头像已留存 {url}（{} 字节，hash {hash}）",
                        bytes.len()
                    );
                    if !opt.dry_run {
                        write_image(pool, p.person_id, &bytes, &url, &hash).await?;
                    }
                }
            }
            PersonOutcome::Deregistered => {
                sum.deregistered += 1;
                if p.has_image {
                    sum.cleared += 1;
                }
                println!(
                    "{tag} 账号已注销，{}",
                    if p.has_image {
                        "清空本地图"
                    } else {
                        "无需头像"
                    }
                );
                if !opt.dry_run {
                    clear_image(pool, p.person_id, NOTE_DEREGISTERED).await?;
                }
            }
            PersonOutcome::NotFound(msg) => {
                sum.not_found += 1;
                if p.has_image {
                    sum.kept_image += 1;
                }
                tracing::warn!(person = %p.name, id = p.person_id, note = %msg, kept_image = p.has_image, "抖音号查不到，保留已留存的原图（只记 note），等新鲜度过期再试");
                println!(
                    "{tag} 抖音号查不到（{msg}），{}",
                    if p.has_image {
                        "保留已留存的原图（只记 note，7 天后再试）"
                    } else {
                        "首次探测无图，记 note 后 7 天后再试"
                    }
                );
                if !opt.dry_run {
                    // 关键：只写 note + synced_at，**绝不动 image/mime/hash/source_url**
                    note_not_found(pool, p.person_id).await?;
                }
            }
            PersonOutcome::Failed(msg) => {
                // 关键：这里**绝不写库**，上一次的图原样留着（用户口径「防止以后更新不了」）
                sum.failed += 1;
                tracing::warn!(person = %p.name, id = p.person_id, error = %msg, "头像同步失败，保留上一次的图");
                println!("{tag} 同步失败，保留上一次的图（下轮再试）：{msg}");
            }
        }
    }
    Ok(sum)
}

/// 一个人的最终结论
enum PersonOutcome {
    /// 图到手（已过 content-type / 魔数 / 大小校验）
    Image {
        url: String,
        bytes: Vec<u8>,
        hash: String,
    },
    /// **名下所有**抖音号都已注销（任一账号能出图都优先于本结论）→ 这时才清图
    Deregistered,
    /// 没有任何号出图，但有号给出确定的「查不到」→ 记 'not_found'，保留已留存的图
    NotFound(String),
    /// 没有任何号出图，且全是网络/解析/非 JPEG/超限 → 不写库，保留旧图
    Failed(String),
}

/// 单个抖音号的探测产物（纯数据：网络层的返回值，单测直接构造）
#[derive(Debug, PartialEq, Eq, Clone)]
enum Attempt {
    /// 接口给了头像资产（能不能下下来、是不是合格 JPEG 还没验证）
    Found(String),
    /// 这个号已注销
    Deregistered,
    /// 这个号查不到
    NotFound(String),
    /// 这个号的网络/解析失败
    Failed(String),
}

/// 「看完名下所有号」的结论（纯函数产物）
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// 有号能出图 → 用它的资产（活号优先）
    Image(String),
    /// 没有任何号出图，且**所有**号都注销 → 才清图
    AllDeregistered,
    /// 没有任何号出图，但有确定的「查不到」→ 保留旧图，只记 note
    NoAvatar(String),
    /// 没有任何号出图，全部是失败 → 不写库
    AllFailed(String),
}

/// 结论收敛（**纯函数**：不碰网络、不碰 DB，单测直接喂 [`Attempt`] 组）。
///
/// 优先级（2026-10-16 P2-1 修复口径）：
/// ① 任一账号出图 → [`Verdict::Image`]（**活号优先**：主号注销不再一票否决次号活号）；
/// ② 没有任何号出图 + **全部**号注销 → [`Verdict::AllDeregistered`]（这时才清图）；
/// ③ 其余：有确定「查不到」→ [`Verdict::NoAvatar`]（保留旧图只记 note）；
///    全是失败（含「注销 + 网络失败」这种没结论的组合）→ [`Verdict::AllFailed`]（不写库，
///    下轮再试 —— 拿不准就什么都不写，是这里唯一安全的默认）。
fn verdict_of(attempts: &[Attempt]) -> Verdict {
    if let Some(url) = attempts.iter().find_map(|a| match a {
        Attempt::Found(url) => Some(url.clone()),
        _ => None,
    }) {
        return Verdict::Image(url);
    }
    // 注意 `all()` 对空切片恒真：必须显式排除「一个号都没有」的情况
    if !attempts.is_empty() && attempts.iter().all(|a| matches!(a, Attempt::Deregistered)) {
        return Verdict::AllDeregistered;
    }
    if let Some(msg) = attempts.iter().find_map(|a| match a {
        Attempt::NotFound(msg) => Some(msg.clone()),
        _ => None,
    }) {
        return Verdict::NoAvatar(msg);
    }
    let msg = attempts
        .iter()
        .find_map(|a| match a {
            Attempt::Failed(msg) => Some(msg.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "没有可探测的抖音号".to_string());
    Verdict::AllFailed(msg)
}

/// 探测一个人：按 `is_primary DESC, id` **把所有号都看完**再下结论（见 [`verdict_of`]）。
///
/// 只有「接口给了头像资产」的号还要真把图下下来才算**出图**；所以这里按顺序把所有
/// 给过资产的号都试一遍下载，第一张下到位的胜出。多探测几个号的代价可以接受
/// （一人最多几个号，全程串行 + 节流）。
async fn probe_person(
    client: &reqwest::Client,
    p: &Candidate,
    opt: &Options,
    sum: &mut Summary,
) -> PersonOutcome {
    let mut attempts: Vec<Attempt> = Vec::with_capacity(p.douyin_nos.len());
    for no in p.douyin_nos.iter() {
        sum.probed += 1;
        // 注销 / 查不到都只是「这一个号」的结论，记下来继续看下一个号
        attempts.push(match fetch_lookup(client, no).await {
            Ok(Lookup::Found(url)) => Attempt::Found(url),
            Ok(Lookup::Deregistered) => Attempt::Deregistered,
            Ok(Lookup::NotFound(msg)) => Attempt::NotFound(format!("{no}: {msg}")),
            Err(e) => {
                tracing::warn!(person = %p.name, id = p.person_id, no = %no, error = %e, "抖音号查询失败");
                Attempt::Failed(format!("{no} 查询失败：{e}"))
            }
        });
        // 每个探测之间都歇一下：接口对连打敏感（同一个人的备用号也一样）
        pace(opt.delay).await;
    }

    match verdict_of(&attempts) {
        Verdict::Image(first_url) => {
            // 活号优先：先试 verdict 选中的那个号，失败再按原顺序试其它同样给过资产的号
            let rest = attempts.iter().filter_map(|a| match a {
                Attempt::Found(u) if u.as_str() != first_url.as_str() => Some(u.as_str()),
                _ => None,
            });
            let mut last_err = String::new();
            for url in std::iter::once(first_url.as_str()).chain(rest) {
                pace(opt.delay).await;
                match download_avatar(client, url).await {
                    Ok(bytes) => {
                        let hash = content_hash(&bytes);
                        return PersonOutcome::Image {
                            url: url.to_string(),
                            bytes,
                            hash,
                        };
                    }
                    Err(e) => {
                        last_err = format!("{url} 下载失败：{e}");
                        tracing::warn!(person = %p.name, id = p.person_id, url = %url, error = %e, "头像下载失败");
                    }
                }
            }
            // 给过资产的号一张都没下到：有确定「查不到」就保留旧图记 note，否则不写库
            match attempts.iter().find_map(|a| match a {
                Attempt::NotFound(msg) => Some(msg.clone()),
                _ => None,
            }) {
                Some(msg) => PersonOutcome::NotFound(format!("{msg}（另：{last_err}）")),
                None => PersonOutcome::Failed(last_err),
            }
        }
        Verdict::AllDeregistered => {
            tracing::info!(
                person = %p.name,
                id = p.person_id,
                numbers = p.douyin_nos.len(),
                "名下抖音号全部注销，清空本地图"
            );
            PersonOutcome::Deregistered
        }
        Verdict::NoAvatar(msg) => PersonOutcome::NotFound(msg),
        Verdict::AllFailed(msg) => PersonOutcome::Failed(msg),
    }
}

/// 同步范围 = 全部未删除主播。
///
/// 候选判定（**新主播自动纳入**的落点）：
/// - 有抖音号，且 `person_avatar` 无行 或 `synced_at < now() - days`；`--force` 时忽略后者；
/// - 没登记抖音号的人既不写行也不计候选 —— 以后补登记抖音号时立即变成候选。
///
/// `fresh` 用库里的 `now()` 算（不是本机时钟），避免和库的时间漂移打架。
async fn load_scope(pool: &PgPool, opt: &Options) -> anyhow::Result<Scope> {
    let sql = "SELECT p.id AS person_id, p.name, \
                      COALESCE(pa.hash, '') AS hash, \
                      COALESCE(pa.image IS NOT NULL, false) AS has_image, \
                      COALESCE(pa.synced_at >= now() - make_interval(days => ($1::bigint)::int), false) AS fresh \
               FROM person p LEFT JOIN person_avatar pa ON pa.person_id = p.id \
               WHERE p.deleted_at IS NULL ORDER BY p.id";
    // (人, 是否还在新鲜期内) —— fresh 单独放，方便 --force 时整体忽略
    let mut people: Vec<(Candidate, bool)> = sqlx::query(sql)
        .bind(opt.days)
        .fetch_all(pool)
        .await
        .context("读 person/person_avatar 失败")?
        .iter()
        .map(|r| {
            (
                Candidate {
                    person_id: r.try_get::<i64, _>("person_id").unwrap_or(0).max(0) as u64,
                    name: r.try_get::<String, _>("name").unwrap_or_default(),
                    has_image: r.try_get::<bool, _>("has_image").unwrap_or(false),
                    hash: r.try_get::<String, _>("hash").unwrap_or_default(),
                    douyin_nos: Vec::new(),
                },
                r.try_get::<bool, _>("fresh").unwrap_or(false),
            )
        })
        .collect();

    /* 一个人的多个号都登记在 account 里（主号 is_primary=1）。这里一次性读全量
    （百来行），按人分组，省得每人再查一次库；排序即探测顺序。 */
    let idx: HashMap<u64, usize> = people
        .iter()
        .enumerate()
        .map(|(i, (p, _))| (p.person_id, i))
        .collect();
    let acc_sql = "SELECT a.person_id, a.douyin_no FROM account a \
                   JOIN person p ON p.id = a.person_id \
                   WHERE p.deleted_at IS NULL AND a.douyin_no <> '' \
                   ORDER BY a.person_id, a.is_primary DESC, a.id";
    for r in sqlx::query(acc_sql)
        .fetch_all(pool)
        .await
        .context("读 account 失败")?
        .iter()
    {
        let pid = r.try_get::<i64, _>("person_id").unwrap_or(0).max(0) as u64;
        let no: String = r.try_get("douyin_no").unwrap_or_default();
        if let Some(i) = idx.get(&pid) {
            people[*i].0.douyin_nos.push(no);
        }
    }

    let mut scope = Scope::default();
    for (p, fresh) in people {
        if p.douyin_nos.is_empty() {
            scope.no_douyin_no += 1;
            continue;
        }
        if !opt.force && fresh {
            scope.fresh_skipped += 1;
            continue;
        }
        scope.candidates.push(p);
    }
    Ok(scope)
}

/// 定时器用：是否存在「有抖音号但需要重抓」的人（无行 或 已过期）。
/// 单独一条 EXISTS 查询，让空转轮次尽可能便宜。
async fn has_stale_candidates(pool: &PgPool, days: i64) -> anyhow::Result<bool> {
    let sql = "SELECT EXISTS ( \
                 SELECT 1 FROM person p \
                 JOIN account a ON a.person_id = p.id AND a.douyin_no <> '' \
                 LEFT JOIN person_avatar pa ON pa.person_id = p.id \
                 WHERE p.deleted_at IS NULL \
                   AND (pa.person_id IS NULL \
                        OR pa.synced_at < now() - make_interval(days => ($1::bigint)::int)) \
               ) AS stale";
    let stale: bool = sqlx::query(sql)
        .bind(days)
        .fetch_one(pool)
        .await
        .context("检查头像过期候选失败")?
        .try_get("stale")
        .unwrap_or(false);
    Ok(stale)
}

/// 落图：新增或覆盖（图 + 来源 URL + 内容指纹一起写，`note` 归零）
async fn write_image(
    pool: &PgPool,
    person_id: u64,
    bytes: &[u8],
    source_url: &str,
    hash: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO person_avatar (person_id, image, mime, source_url, hash, note, synced_at) \
         VALUES ($1, $2, 'image/jpeg', $3, $4, '', now()) \
         ON CONFLICT (person_id) DO UPDATE SET \
           image = EXCLUDED.image, mime = EXCLUDED.mime, source_url = EXCLUDED.source_url, \
           hash = EXCLUDED.hash, note = '', synced_at = now()",
    )
    .bind(person_id as i64)
    .bind(bytes)
    .bind(source_url)
    .bind(hash)
    .execute(pool)
    .await
    .with_context(|| format!("写 person_avatar 图片失败（id {person_id}）"))?;
    Ok(())
}

/// 图没变：只把 synced_at 推到 now()（内容/来源/指纹都不动，省一次 BYTEA 写入）
async fn touch_image(pool: &PgPool, person_id: u64) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO person_avatar (person_id, image, mime, source_url, hash, note, synced_at) \
         VALUES ($1, NULL, 'image/jpeg', '', '', '', now()) \
         ON CONFLICT (person_id) DO UPDATE SET synced_at = now()",
    )
    .bind(person_id as i64)
    .execute(pool)
    .await
    .with_context(|| format!("刷新 person_avatar.synced_at 失败（id {person_id}）"))?;
    Ok(())
}

/// `not_found` 的写入：**只动 note + synced_at**。
///
/// `DO UPDATE` 子句里**没有** image / mime / hash / source_url 的赋值，所以已留存的图
/// 原样不动；只有「本来没有行」的人在 INSERT 分支拿到一行 image=NULL 的空行 ——
/// 既满足「接口改版也不许清空已留存的图」，又保持「有抖音号的人都有行」的验收断言。
/// （2026-10-16 复核口径：status_code 非 0 只说明「这次没查到」，不说明「这个人没头像」。）
const SQL_NOTE_NOT_FOUND: &str =
    "INSERT INTO person_avatar (person_id, image, mime, source_url, hash, note, synced_at) \
     VALUES ($1, NULL, 'image/jpeg', '', '', $2, now()) \
     ON CONFLICT (person_id) DO UPDATE SET note = EXCLUDED.note, synced_at = EXCLUDED.synced_at";

/// 记「号查不到」：保留图，只推 note + synced_at（7 天内不再重试）
async fn note_not_found(pool: &PgPool, person_id: u64) -> anyhow::Result<()> {
    sqlx::query(SQL_NOTE_NOT_FOUND)
        .bind(person_id as i64)
        .bind(NOTE_NOT_FOUND)
        .execute(pool)
        .await
        .with_context(|| format!("写 person_avatar.note='not_found' 失败（id {person_id}）"))?;
    Ok(())
}

/// 清空语句：**只给「已注销」用**。图、指纹、来源一起清（图都没了，hash 作为
/// 「内容指纹」已经没有意义；展示端也只在 `image IS NOT NULL` 的行上读 hash）。
/// `not_found` 绝不能走这里 —— 那个分支用 [`note_not_found`]，必须保留已留存的图。
const SQL_CLEAR_IMAGE: &str =
    "INSERT INTO person_avatar (person_id, image, mime, source_url, hash, note, synced_at) \
     VALUES ($1, NULL, 'image/jpeg', '', '', $2, now()) \
     ON CONFLICT (person_id) DO UPDATE SET \
       image = NULL, source_url = '', hash = '', note = EXCLUDED.note, synced_at = now()";

/// 置空：**只给「已注销」用**（用户口径：注销的不用头像）
async fn clear_image(pool: &PgPool, person_id: u64, note: &str) -> anyhow::Result<()> {
    sqlx::query(SQL_CLEAR_IMAGE)
        .bind(person_id as i64)
        .bind(note)
        .execute(pool)
        .await
        .with_context(|| format!("置空 person_avatar 失败（id {person_id}）"))?;
    Ok(())
}

/// 查一个人的一个抖音号（网络/解析错误返回 Err，由调用方决定保留现值）
async fn fetch_lookup(client: &reqwest::Client, douyin_no: &str) -> anyhow::Result<Lookup> {
    let resp = client
        .get(USER_INFO)
        .query(&[("unique_id", douyin_no)])
        .header(reqwest::header::USER_AGENT, UA)
        .header(reqwest::header::REFERER, "https://www.douyin.com/")
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("HTTP {}", resp.status());
    }
    let body: Value = resp.json().await.context("解析抖音接口 JSON 失败")?;
    Ok(parse_lookup(&body))
}

/// 下载头像 JPEG 并校验：HTTP 200 + content-type 含 image/jpeg + 魔数 FF D8 FF +
/// 0 < 字节数 ≤ 64KB。任何一条不过就是 `Err`（调用方按失败处理，不写库）。
async fn download_avatar(client: &reqwest::Client, url: &str) -> anyhow::Result<Vec<u8>> {
    if url.len() > MAX_SOURCE_URL {
        bail!("头像 URL 过长（{} 字符，上限 {MAX_SOURCE_URL}）", url.len());
    }
    let resp = client
        .get(url)
        .header(reqwest::header::USER_AGENT, UA)
        .send()
        .await?;
    if resp.status() != reqwest::StatusCode::OK {
        bail!("HTTP {}", resp.status());
    }
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !ctype.contains("image/jpeg") {
        // 接口失效时常见：返回 HTML 错误页或 JSON，按失败处理（不写 note='bad_response'）
        bail!("content-type 不是 image/jpeg（{ctype}）");
    }
    let bytes = resp.bytes().await.context("读头像响应体失败")?;
    if bytes.is_empty() {
        bail!("头像响应体为空");
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        bail!(
            "头像过大（{} 字节，上限 {MAX_IMAGE_BYTES} 字节）",
            bytes.len()
        );
    }
    if !is_jpeg(&bytes) {
        bail!("魔数不是 JPEG（FF D8 FF）");
    }
    Ok(bytes.to_vec())
}

/// 解析接口响应（纯函数，单测直接喂 JSON 字符串）
fn parse_lookup(body: &Value) -> Lookup {
    let code = body
        .get("status_code")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    if code != 0 {
        let msg = body
            .get("status_msg")
            .and_then(Value::as_str)
            .unwrap_or("接口未返回数据");
        return Lookup::NotFound(msg.to_string());
    }
    let Some(u) = body.get("user_info").filter(|v| !v.is_null()) else {
        return Lookup::NotFound("无 user_info".into());
    };
    let deregistered = u
        .pointer("/special_state_info/special_state")
        .and_then(Value::as_i64)
        .map(|v| v == 1)
        .unwrap_or(false);
    if deregistered {
        return Lookup::Deregistered;
    }
    // avatar_thumb 是主选，空的（或只有空白）退回 avatar_medium —— 注意不能用
    // find_map + as_str：`uri: ""` 也会命中，必须显式跳过空串（v1 的老实现就栽在这）。
    let uri = ["avatar_thumb", "avatar_medium"]
        .iter()
        .filter_map(|k| u.pointer(&format!("/{k}/uri")).and_then(Value::as_str))
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or_default();
    if uri.is_empty() {
        // 有 user_info 但没头像资产：按「查不到」处理 —— **保留已留存的图**，
        // 只记 note='not_found' + synced_at，7 天后再试
        return Lookup::NotFound("无头像字段".into());
    }
    Lookup::Found(avatar_url(uri))
}

/// 头像资产 uri → 抖音 CDN URL（尺寸桶见 [`AVATAR_SIZE`]）
fn avatar_url(uri: &str) -> String {
    format!("{AVATAR_CDN}/{AVATAR_SIZE}/{uri}.jpeg")
}

/// JPEG 魔数：FF D8 FF（JPEG SOI + 第一个标记段前缀）
fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF
}

/// 内容指纹 = md5(hex) 前 [`HASH_LEN`] 位。
/// 用字节算而不是用 URL 算：CDN URL 可能对同一张图给出不同参数，
/// 但展示端要的是「图变了才换 URL」，所以必须内容寻址。
fn content_hash(bytes: &[u8]) -> String {
    use md5::Digest;
    let digest = md5::Md5::digest(bytes);
    let mut out = String::with_capacity(HASH_LEN);
    for b in digest.iter().take(HASH_LEN / 2) {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// 每次 HTTP 之间的节流（默认 600ms，别并发打接口）
async fn pace(delay: Duration) {
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
}

/// 服务内定时器：启动即先跑一轮（**只在真有 stale 候选时才真跑**），
/// 之后每 `check_interval` 检查一次；用全局 shutdown token 优雅退出
/// （写法同 main.rs 的 Redis 30s 探针）。
///
/// 失败的人不写 synced_at，所以下一轮还会被捞到重试 —— 这正是「接口哪天恢复
/// 了，图自己就回来了」的路径。
pub fn spawn_scheduler(
    pool: PgPool,
    days: i64,
    check_interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let token = crate::push::shutdown_token();
        let mut tick = tokio::time::interval(check_interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // 吃掉 interval 的首个立即到期拍：首轮同步跑在下面（启动即跑一次），
        // 之后才按 check_interval 睡；否则启动瞬间会连跑两轮。
        tick.tick().await;
        loop {
            match has_stale_candidates(&pool, days).await {
                Ok(false) => tracing::debug!(days = days, "头像同步：没有过期的候选，跳过本轮"),
                Ok(true) => {
                    let opt = Options {
                        days,
                        ..Options::default()
                    };
                    match run(&pool, &opt).await {
                        Ok(s) => println!("{}", s.report(false, days)),
                        Err(e) => tracing::warn!(error = %e, "头像同步：本轮失败"),
                    }
                }
                Err(e) => tracing::warn!(error = %e, "头像同步：候选检查失败"),
            }
            tokio::select! {
                _ = token.cancelled() => {
                    tracing::info!("头像同步定时器收到退出信号，停止");
                    break;
                }
                _ = tick.tick() => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_found_builds_cdn_url() {
        let body = json!({
            "status_code": 0,
            "user_info": {
                "unique_id": "An21555",
                "sec_uid": "MS4wLjABAAAAx",
                "avatar_thumb": { "uri": "aweme-avatar/tos-cn-avt-0015_abc" }
            }
        });
        assert_eq!(
            parse_lookup(&body),
            Lookup::Found(
                "https://p3.douyinpic.com/aweme/200x200/aweme-avatar/tos-cn-avt-0015_abc.jpeg"
                    .into()
            )
        );
    }

    #[test]
    fn parse_avatar_medium_is_fallback() {
        let body = json!({
            "status_code": 0,
            "user_info": {
                "avatar_thumb": { "uri": "" },
                "avatar_medium": { "uri": "aweme-avatar/m" }
            }
        });
        assert_eq!(
            parse_lookup(&body),
            Lookup::Found("https://p3.douyinpic.com/aweme/200x200/aweme-avatar/m.jpeg".into())
        );
    }

    #[test]
    fn parse_deregistered_wins_over_missing_avatar() {
        // 注销号的真实长相：没有 sec_uid / 没有头像，但有 special_state_info
        let body = json!({
            "status_code": 0,
            "user_info": {
                "nickname": "用户0225242585090",
                "special_state_info": { "special_state": 1, "title": "账号已经注销" }
            }
        });
        assert_eq!(parse_lookup(&body), Lookup::Deregistered);
    }

    #[test]
    fn parse_bad_unique_id_is_not_found() {
        let body = json!({ "status_code": 5, "status_msg": "参数不合法" });
        assert_eq!(parse_lookup(&body), Lookup::NotFound("参数不合法".into()));
    }

    #[test]
    fn parse_user_info_without_avatar_is_not_found() {
        let body = json!({ "status_code": 0, "user_info": { "nickname": "x" } });
        assert_eq!(parse_lookup(&body), Lookup::NotFound("无头像字段".into()));
    }

    #[test]
    fn avatar_url_joins_cdn_bucket_and_suffix() {
        assert_eq!(
            avatar_url("aweme-avatar/x"),
            "https://p3.douyinpic.com/aweme/200x200/aweme-avatar/x.jpeg"
        );
    }

    #[test]
    fn jpeg_magic_rejects_non_jpeg() {
        assert!(is_jpeg(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00]));
        assert!(!is_jpeg(&[0x89, 0x50, 0x4E, 0x47])); // PNG
        assert!(!is_jpeg(&[0xFF, 0xD8])); // 太短
        assert!(!is_jpeg(&[0xFF, 0xD9, 0xFF])); // EOI，不是 SOI
        assert!(!is_jpeg(&[]));
    }

    #[test]
    fn content_hash_is_16_hex_and_content_addressed() {
        // md5("abc") = 900150983cd24fb0d6963f7d28e17f72，取前 16 位
        assert_eq!(content_hash(b"abc"), "900150983cd24fb0");
        assert_eq!(content_hash(b"abc").len(), HASH_LEN);
        assert_ne!(content_hash(b"abc"), content_hash(b"abd"));
    }

    #[test]
    fn args_defaults_are_documented_ones() {
        let o = Options::from_args(std::iter::empty()).unwrap();
        assert!(!o.dry_run && !o.force && o.limit.is_none());
        assert_eq!(o.days, 7);
        assert_eq!(o.delay, Duration::from_millis(600));
    }

    #[test]
    fn args_parse_both_spellings() {
        let o = Options::from_args(
            [
                "--dry-run",
                "--limit",
                "3",
                "--delay-ms=100",
                "--days",
                "1",
                "--force",
            ]
            .iter()
            .map(|s| s.to_string()),
        )
        .unwrap();
        assert!(o.dry_run && o.force);
        assert_eq!(o.limit, Some(3));
        assert_eq!(o.days, 1);
        assert_eq!(o.delay, Duration::from_millis(100));
    }

    #[test]
    fn args_reject_junk() {
        assert!(Options::from_args(["--nope".to_string()]).is_err());
        assert!(Options::from_args(["--limit".to_string()]).is_err());
        assert!(Options::from_args(["--limit=abc".to_string()]).is_err());
        assert!(Options::from_args(["--days=-1".to_string()]).is_err());
        assert!(Options::from_args(["--days".to_string()]).is_err());
    }

    // ===== T8（P2-1）：多号结论收敛，纯函数，不碰网络/DB =====

    /// ① 主号注销 + 次号活 → 必须用次号的图（主号注销不再一票否决活号）
    #[test]
    fn verdict_live_secondary_beats_deregistered_primary() {
        let live = "https://p3.douyinpic.com/aweme/200x200/live.jpeg";
        let a = vec![Attempt::Deregistered, Attempt::Found(live.to_string())];
        assert_eq!(verdict_of(&a), Verdict::Image(live.to_string()));
        // 顺序反过来结论一样：活号优先与主/次号无关
        let b = vec![Attempt::Found(live.to_string()), Attempt::Deregistered];
        assert_eq!(verdict_of(&b), Verdict::Image(live.to_string()));
    }

    /// ② 只有**全部**号注销才清图
    #[test]
    fn verdict_all_deregistered_is_the_only_clear_case() {
        let all = vec![Attempt::Deregistered, Attempt::Deregistered];
        assert_eq!(verdict_of(&all), Verdict::AllDeregistered);
        // 一个注销 + 一个查不到 → 不是「全部注销」，不能清图
        let mixed = vec![
            Attempt::Deregistered,
            Attempt::NotFound("Zhang06725: 参数不合法".into()),
        ];
        assert_ne!(verdict_of(&mixed), Verdict::AllDeregistered);
        // 一个注销 + 一个网络失败 → 拿不准，同样不能清图
        let unclear = vec![
            Attempt::Deregistered,
            Attempt::Failed("79147106153 查询失败：HTTP 500".into()),
        ];
        assert_ne!(verdict_of(&unclear), Verdict::AllDeregistered);
    }

    /// ③ 注销 + 查不到（没有活号）→ NotFound：保留已留存的图，只记 note
    #[test]
    fn verdict_deregistered_plus_not_found_keeps_image() {
        let a = vec![
            Attempt::Deregistered,
            Attempt::NotFound("Zhang06725: 参数不合法".into()),
        ];
        assert_eq!(
            verdict_of(&a),
            Verdict::NoAvatar("Zhang06725: 参数不合法".into())
        );
    }

    /// ④ 补一条：全是失败（或没有号）→ 不写库；有查不到就优先按查不到处理
    #[test]
    fn verdict_without_image_prefers_not_found_over_failure() {
        let failed = vec![Attempt::Failed("x 查询失败：timeout".into())];
        assert!(matches!(verdict_of(&failed), Verdict::AllFailed(_)));
        let mixed = vec![
            Attempt::Failed("x 查询失败：timeout".into()),
            Attempt::NotFound("y: 参数不合法".into()),
        ];
        assert_eq!(
            verdict_of(&mixed),
            Verdict::NoAvatar("y: 参数不合法".into())
        );
        // 边界：一个号都没有（正常不该发生）也不能被当成「全部注销」去清图
        assert!(matches!(verdict_of(&[]), Verdict::AllFailed(_)));
    }

    /// T6 核心：`not_found` 分支用的 SQL **不能出现任何 image/hash/source_url 赋值**
    /// （否则接口一改版就会把全部已留存头像清空）。
    #[test]
    fn note_not_found_sql_never_touches_image() {
        let sql = SQL_NOTE_NOT_FOUND;
        assert!(
            sql.contains("DO UPDATE SET note = EXCLUDED.note, synced_at = EXCLUDED.synced_at"),
            "not_found 的 DO UPDATE 只允许改 note + synced_at：{sql}"
        );
        for forbidden in ["image =", "mime =", "hash =", "source_url ="] {
            assert!(
                !sql.contains(forbidden),
                "not_found 分支不许写 {forbidden}（会清掉已留存的图）：{sql}"
            );
        }
        // 首次探测就没行的人仍要落一行（image=NULL），保证「有抖音号的人都有行」
        assert!(sql.starts_with("INSERT INTO person_avatar"));
        assert!(sql.contains("VALUES ($1, NULL, 'image/jpeg', '', '', $2, now())"));
    }

    /// 对照组：注销分支**必须**清图（用户口径：注销的不用头像），别被上面那条测反了。
    #[test]
    fn clear_image_sql_only_clears_for_deregistered() {
        assert!(SQL_CLEAR_IMAGE.contains("image = NULL"));
        assert!(SQL_CLEAR_IMAGE.contains("source_url = ''"));
        assert!(SQL_CLEAR_IMAGE.contains("hash = ''"));
        assert!(SQL_CLEAR_IMAGE.contains("note = EXCLUDED.note"));
    }

    /// report 文案要能一眼看出「查不到 N 人，其中保住原图 M 人」。
    #[test]
    fn report_shows_kept_image_among_not_found() {
        let s = Summary {
            not_found: 3,
            kept_image: 2,
            ..Default::default()
        };
        let r = s.report(false, 7);
        assert!(r.contains("号查不到 3 人"), "{r}");
        assert!(r.contains("其中保住原图 2 人"), "{r}");
        // kept_image 不能再混进 cleared（cleared 只表示注销清图）
        assert!(r.contains("已注销 0 人（清空原图 0 人）"), "{r}");
        let dry = Summary::default().report(true, 7);
        assert!(dry.contains("--dry-run"), "{dry}");
    }
}
