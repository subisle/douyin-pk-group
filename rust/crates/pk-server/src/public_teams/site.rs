//! public_teams::site —— 自 public_teams.rs 按业务域拆分（对外语义不变）。

use super::*;

/* ------------------------------- 站点数据 ------------------------------- */

/// GET /api/public/teams/{team}：站点配置 + 最新日期
/// 团链接被关闭时返回 published=false（前端展示「暂未公布」占位，不是 404）
/// 展示端登录门（2026-10-10）：要「我的」登录态（token 走 X-My-Token 头）
pub async fn site(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    let query_team = data_team(&team, &token_team(&headers)).to_string();
    gate_team(&st, s.login_required, &team, &headers).await?;
    let dates = crate::repos::get_available_dates(&st, None, false).await?;
    // 公告弹窗：开启时随站点配置一起下发；关闭时不下发（前端不弹）
    // load_effective（2026-10-14）：items 为空时按 celebrate/champ_banner 内存合成一条
    let ann = Announcement::load_effective(&st).await;
    // 🚩 冠军展示位（2026-10-13）：管理员可配（star_title/star_override，与推送图共用配置）。
    // 手动指定「称号|人名|音浪」优先；没配就自动取最近一个业务日的当日音浪第一。
    let star = {
        let ov = crate::store::config_get(&st, "star_override").await;
        let auto = star_auto(&st, &query_team, "").await;
        match ov.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(v) => match v.splitn(3, '|').collect::<Vec<_>>()[..] {
                [t, n, w] => serde_json::json!({
                    "title": t.trim(), "name": n.trim(),
                    "wave": w.trim().parse::<i64>().unwrap_or(0),
                }),
                [t, n] => serde_json::json!({ "title": t.trim(), "name": n.trim(), "wave": 0 }),
                [t] => serde_json::json!({
                    "title": t.trim(), "name": auto.0, "wave": auto.1,
                }),
                _ => auto_json(&auto),
            },
            None => auto_json(&auto),
        }
    };
    /* 🏆 争霸赛冠军横幅（2026-10-13）：管理员整条文案可编（config champ_banner，
    格式「称号|人名|音浪」，空 = 不显示横幅）。与 🚩每日之星独立。 */
    let champ = match crate::store::config_get(&st, "champ_banner")
        .await
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(v) => match v.splitn(3, '|').collect::<Vec<_>>()[..] {
            [t, n, w] => serde_json::json!({
                "title": t.trim(), "name": n.trim(),
                "wave": w.trim().parse::<i64>().unwrap_or(0),
            }),
            [t, n] => serde_json::json!({ "title": t.trim(), "name": n.trim(), "wave": 0 }),
            [t] => serde_json::json!({ "title": t.trim(), "name": "", "wave": 0 }),
            _ => serde_json::Value::Null,
        },
        None => serde_json::Value::Null,
    };
    /* 引导弹窗：开启时随站点一起来（管理端全量形态）。但公开响应里不能出现
    enabled=false 的公告条 —— 把嵌套的 items 换成公开裁剪口径，公开响应任何位置
    都只含上线条目。 */
    let announcement_json = if ann.enabled {
        let mut v = ann.to_json();
        v["items"] = ann.public_items_json();
        v
    } else {
        Value::Null
    };
    Ok(Json(json!({
        "published": true,
        "team": query_team,
        "latestDate": dates.first(),
        "dates": dates,
        "star": star,
        "champ": champ,
        "announcement": announcement_json,
        // 公告位（2026-10-13）：登录页 / 展示页（看板）/ 个人页三处，全量下发，
        // 前端各页各取所需（登录页在登录前拿不到 site，另有 /api/public/announcements）
        "placements": ann.placements,
        // 庆祝配置（2026-10-13）：登录后展示页/我的页的彩带庆祝 + 冠军横幅用
        "celebrate": ann.celebrate,
        // 展示页公告条（2026-10-14）新增键：数组顺序 = 从上到下；只含 enabled=true，
        // 是展示端公告/冠军横幅的**唯一来源**（champ/celebrate 保留兼容，页面不再单独渲染）
        "items": ann.public_items_json(),
        // 每日之星弹幕带（2026-10-18 v2）：常驻带配置，永远下发归一化后的完整对象
        // （enabled=false 也下发；前端按 enabled 判断，老前端忽略新键）
        "starDanmaku": serde_json::to_value(ann.star_danmaku.normalized())
            .unwrap_or(serde_json::Value::Null),
        "settings": {
            "daily": s.daily,
            "monthly": s.monthly,
            "yearly": s.yearly,
            "duration": s.duration,
            "trend": s.trend,
            "wave": s.wave,
            // 页面级显隐（2026-10-10）：每日报告 / 分组 / 族谱 / 分组子页 / 登录开关
            "dataEnabled": s.data_enabled,
            "groupEnabled": s.group_enabled,
            "lineageEnabled": s.lineage_enabled,
            "groupTypes": s.group_types,
            "loginRequired": s.login_required,
        },
    })))
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub date: Option<String>,
    pub period: Option<String>,
}

/// GET /api/public/announcements：登录页用的轻量公告端点（2026-10-13，无需登录）。
///
/// 为什么不直接用 site()：登录门出现时用户还没登录，site() 要过登录门
/// （gate_team），未登录拿不到 → 登录页公告/庆祝配置必须走一个不需要登录的
/// 独立端点。只下发登录前需要的部分：
/// - login：登录门卡片上方那条公告（enabled/text/icon）；
/// - celebrate：登录后彩带庆祝（enabled/confetti/title/name/wave），
///   title/name 空时前端回退推送配置 star_title / star_override 的值。
///
/// 不下发 steps / version 等引导弹窗字段。不带团别参数、也不查 team_enabled：
/// 公告/庆祝是全站性配置，且团关闭时展示页（含登录门）照样要能取到这两块。
/// 2026-10-14：新增 `items`（展示页公告条，site 未回来前的兜底数据源），
/// 与 site 同口径（load_effective + public_items_json）。
pub async fn public_announcements(State(st): State<ArcState>) -> ApiResult {
    let ann = Announcement::load_effective(&st).await;
    // 登录页图标归一化先算好再进 json!（宏的值位置放不了语句块），与 placement_json 同口径
    let login_icon = ann.placements.login.icon.trim();
    let login_icon = if STEP_ICON_WHITELIST.contains(&login_icon) {
        login_icon
    } else {
        ""
    };
    Ok(Json(json!({
        "login": {
            "enabled": ann.placements.login.enabled,
            "text": ann.placements.login.text,
            "icon": login_icon,
        },
        // 庆祝配置裁剪形态（celebrate_public_json），登录后彩带庆祝用
        "celebrate": ann.celebrate_public_json(),
        // 展示页公告条（2026-10-14）：与 site.items 同口径，登录门弹出时页面就能兜底渲染
        "items": ann.public_items_json(),
    })))
}

/// person_id → **同源头像地址**（`/api/public/avatars/<pid>?v=<hash>`）。
///
/// 数据源是本地留存的 `person_avatar`（抖音头像字节 + 内容 md5 前 16 位，见 migration 025），
/// 由 `pk-server sync-avatars` **每 7 天**同步一遍（7 天判定靠 `synced_at`，见 avatar_sync.rs）。
/// 展示端「主播列表」（领奖台 + 名次行）用：图片走本站 `/api/public/avatars/{person_id}`
/// 同源下发（跨域抖音 CDN 一旦改策略/封 Referer，页面就有本地兜底），`?v=<hash>` 用来破缓存 ——
/// 图一变 hash 就变、URL 就变，所以这个端点是 immutable 长缓存（见 avatars.rs 头注释）。
///
/// 只有主播列表里的人有真头像：没同步过 / 账号已注销 / 没登记抖音号的人不入 map，
/// 前端回落姓名首字色块。`image IS NULL` 的人本就不该出现在这里（下发端也是 404）。
/// hash 为空（历史行 / 同步异常）时退化成不带 `?v=` 的地址：仍然出图，只是失去换图破缓存。
///
/// 装饰性字段：表不存在（迁移没跑）或查询失败只 debug + 空 map，不让榜单整体 500。
async fn avatar_map(st: &ArcState, source: &str) -> HashMap<u64, String> {
    let sql = "SELECT person_id, hash FROM person_avatar WHERE image IS NOT NULL";
    match pgrs::query(&st.pools, source, sql).await {
        Ok(rows) => rows.iter().fold(HashMap::new(), |mut m, r| {
            let pid = row_pid(r, "person_id");
            if pid == 0 {
                return m;
            }
            // hash 列 NOT NULL DEFAULT ''，正常都能读；读不出只当没有 hash
            let hash = r.try_get::<String, _>("hash").unwrap_or_default();
            m.insert(pid, avatar_url(pid, &hash));
            m
        }),
        Err(e) => {
            /* 表不存在（迁移未跑）也走这里：榜单照常下发 avatar=null，前端回落首字色块。
            用 debug 而不是 warn，避免「迁移没跑」期间每次刷榜都刷一屏告警。 */
            tracing::debug!(error = %e, "本地留存头像读取失败，主播列表按无头像渲染");
            HashMap::new()
        }
    }
}

/// 头像地址（纯函数，抽出只为把「URL 形状」这条前后端契约钉进单测）：
/// hash 非空 → `/api/public/avatars/<pid>?v=<hash>`（换图破缓存）；
/// hash 为空（历史行 / 同步异常）→ 退化成不带 query 的地址，仍然出图。
/// 注意：这里**不判**图片是否存在 —— 端点自己会对 `image IS NULL` 的人 404。
pub(super) fn avatar_url(person_id: u64, hash: &str) -> String {
    let hash = hash.trim();
    if hash.is_empty() {
        format!("/api/public/avatars/{person_id}")
    } else {
        format!("/api/public/avatars/{person_id}?v={hash}")
    }
}

/// GET /api/public/teams/{team}/daily?date=
/// DTO 白名单：名次 / 名字 / 音浪 / 分钟 / 开播 / 头像——绝无抖音号
pub async fn daily(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    let query_team = data_team(&team, &token_team(&headers)).to_string();
    gate_team(&st, s.login_required, &team, &headers).await?;
    if !s.daily {
        return Ok(Json(json!({ "published": true, "dailyEnabled": false })));
    }
    // 每日报告页整体显隐（2026-10-10）：关掉后榜单数据不下发
    if !s.data_enabled {
        return Ok(Json(json!({ "published": true, "dailyEnabled": false })));
    }
    let dates = crate::repos::get_available_dates(&st, None, false).await?;
    let date = match q.date.as_deref() {
        Some(d) if valid_date_pub(d) && dates.iter().any(|x| x == d) => d.to_string(),
        _ => dates.first().cloned().unwrap_or_default(),
    };
    if date.is_empty() {
        return Err(ApiError::bad_request("暂未公布数据"));
    }
    let source = sid(&st);
    // 头像（主播列表用）：一次查完，两百行以内，跟 total/absent 一样在内存 join
    let avatars = avatar_map(&st, &source).await;
    // 前一业务日（用于名次变动 ↑↓）
    let idx = dates.iter().position(|d| d == &date);
    let prev_date = idx.and_then(|i| dates.get(i + 1)).cloned();

    /* 全团在册主播都要出现在榜上：当天没有采集记录的人走 LEFT JOIN 得到 0 音浪 /
    未开播，前端标记为「未开播」。只在 daily_metric 里捞人的话，某天只录了 1 个人时
    榜单就真只剩 1 行，跟库里的在册人数对不上。
    hide_in_daily_report = 1 的主播（管理端逐人设置）完全不进榜、不占名次。 */
    let sql = format!(
        "SELECT p.id AS person_id, p.name, \
         (COALESCE(SUM(d.wave), 0))::BIGINT AS wave, \
         (COALESCE(SUM(d.minutes), 0))::BIGINT AS minutes, \
         CASE WHEN COALESCE(SUM(d.wave), 0) >= 2 THEN 1 ELSE 0 END AS live, \
         COUNT(d.id) AS nrow \
         FROM person p \
         LEFT JOIN daily_metric d ON d.person_id = p.id \
           AND TO_CHAR(d.biz_date, 'YYYY-MM-DD') = '{date}' \
           AND d.wave_reliable = 1 \
         WHERE p.deleted_at IS NULL \
         AND ('{query_team}' = '' OR p.gender = '{query_team}') \
         AND p.hide_in_daily_report = 0 \
         GROUP BY p.id, p.name \
         ORDER BY nrow DESC, wave DESC, p.name ASC"
    );
    let rows = pgrs::query(&st.pools, &source, &sql).await?;

    // 当月未播天数 + 历史累计总音浪 + 前一日名次（内存 join）
    // 未播口径（2026-10-09 用户定；2026-10-10 P1-10 统一引用 dialect::NOT_LIVE_WAVE_THRESHOLD）：
    // 当天导入音浪 < 2 = 未开播；
    // totalWave = 历史累计总音浪（2026-10-11 用户口径，展示页列「累计总音浪」）。
    // P1-7：只聚合可信行（wave_reliable=1），与 WAVES_A / 月年榜口径一致。
    let month = &date[..7];
    // 与日报共用 dialect::month_absent_sql（唯一口径：有数据行且音浪 < 2 才算未播）
    let absent_sql = crate::dialect::month_absent_sql(month);
    let total_sql = "SELECT person_id, (SUM(wave))::BIGINT AS total \
         FROM daily_metric WHERE wave_reliable = 1 GROUP BY person_id"
        .to_string();
    let mut absent_map: HashMap<u64, i64> = Default::default();
    for r in pgrs::query(&st.pools, &source, &absent_sql).await? {
        absent_map.insert(row_pid(&r, "person_id"), row_i64(&r, "n"));
    }
    let mut total_map: HashMap<u64, i64> = Default::default();
    for r in pgrs::query(&st.pools, &source, &total_sql).await? {
        total_map.insert(row_pid(&r, "person_id"), row_i64(&r, "total"));
    }
    let mut prev_rank: HashMap<u64, usize> = Default::default();
    if let Some(pd) = &prev_date {
        let psql = format!(
            "SELECT p.id AS person_id \
             FROM daily_metric d JOIN person p ON p.id = d.person_id \
             WHERE p.deleted_at IS NULL \
             AND ('{query_team}' = '' OR p.gender = '{query_team}') \
             AND p.hide_in_daily_report = 0 AND d.wave_reliable = 1 \
             AND TO_CHAR(d.biz_date, 'YYYY-MM-DD') = '{pd}' \
             GROUP BY p.id, p.name ORDER BY SUM(d.wave) DESC"
        );
        for (i, r) in pgrs::query(&st.pools, &source, &psql)
            .await?
            .iter()
            .enumerate()
        {
            prev_rank.insert(row_pid(r, "person_id"), i + 1);
        }
    }

    // 名次只在「当天有数据」的人之间排：没采集记录的人不给名次，前端显示「—」，
    // 否则明明没数据的人会因为有 80 个同僚也排在 40 名，读者一眼看不出他没播。
    let mut last_wave: Option<i64> = None;
    let mut last_rank = 0usize;
    let mut ranked = 0usize;
    let mut items: Vec<Value> = Vec::with_capacity(rows.len());
    for r in rows.iter() {
        let pid = row_pid(r, "person_id");
        let has_data = row_i64(r, "nrow") > 0;
        let cur = row_i64(r, "wave");
        let rank = if has_data {
            ranked += 1;
            if Some(cur) == last_wave {
                last_rank
            } else {
                last_rank = ranked;
                last_rank
            }
        } else {
            0
        };
        if has_data {
            last_wave = Some(cur);
        }
        let delta = match prev_rank.get(&pid).copied() {
            // 前一天压根没他（没数据）或今天没数据：都不算升降
            Some(pr) if has_data => Some(if pr > rank {
                json!({ "dir": "up", "n": pr - rank })
            } else if pr < rank {
                json!({ "dir": "down", "n": rank - pr })
            } else {
                json!({ "dir": "flat" })
            }),
            _ => None,
        };
        let mut item = json!({
            // P2-19：公开榜单不再下发 personId（对齐 public.rs 的白名单策略 ——
            // 展示端团数据视图只按 name/isLive/rank 渲染，不依赖这个内部 id；
            // 需要 id 的只有 push 搜索/绑定接口，那些接口单独下发）
            "name": r.try_get::<String, _>("name").unwrap_or_default(),
            "wave": cur,
            "isLive": row_live(r),
            "hasData": has_data,
            "absentDays": absent_map.get(&pid).copied().unwrap_or(0),
            "totalWave": total_map.get(&pid).copied().unwrap_or(0),
            "rank": rank,
        });
        /* 抖音头像（2026-10-16）：主播列表渲染真头像用，本地留存 + 同源地址；没有就是 null，
        前端回落姓名首字色块。地址里只有 person_id 与内容 hash（不是抖音号、不是 CDN 契约 URL），
        所以「公开 DTO 不下发 personId」的白名单口径不受影响。 */
        item["avatar"] = match avatars.get(&pid) {
            Some(u) => json!(u),
            None => Value::Null,
        };
        if s.duration {
            item["minutes"] = json!(row_i64(r, "minutes"));
        }
        if s.trend {
            item["delta"] = delta.unwrap_or(Value::Null);
        }
        if !s.wave {
            // 音浪数值显隐（后台开关）：关掉后不下发任何数字
            item["wave"] = Value::Null;
            item["totalWave"] = Value::Null;
        }
        items.push(item);
    }
    Ok(Json(json!({
        "published": true,
        "date": date,
        "teamSize": rows.len(),
        // P2-13：liveCount 语义对齐为「开播人数」（当日音浪 ≥ 未播阈值），
        // 原来统计的是「有数据人数」，字段名与口径不符
        "liveCount": items
            .iter()
            .filter(|i| {
                i["wave"].as_i64().map(is_live_wave).unwrap_or(false)
            })
            .count(),
        "items": items,
    })))
}
/// GET /api/public/teams/{team}/lineage：师门树（公开端画布用）
///
/// DTO 白名单：id / 名字 / 师傅 id+名 / 状态 / 代数 / 直系徒弟数——绝无抖音号。
/// 师傅不在本团（含 master 为空）的当作树根。离团（status='left'）的人也在树里
/// （家族成员不因离团消失），前端负责淡显。
///
/// 访问口径（2026-10-12 安全审计修复。产品口径：族谱页给**有师门归属**的用户看，
/// 管理员看全部）：
/// - **永远要求有效 my token**——即使 login_required=false 也不放行游客：族谱是全团
///   师承关系数据，游客可见等于把内部师门结构对公网开放。token 缺失/失效 → 401；
/// - 管理员观看账号（my_accounts.is_admin=1）→ 行为不变：可看任意团，
///   team=all 时返回两团全量；
/// - 普通绑定用户：本人**有师傅**（master_id 非空/非 0）或**有徒弟**（直系徒弟数 > 0）
///   才放行，数据范围维持全团（与现状一致）；否则 403 + code=lineage_forbidden
///   （前端据此显示「族谱仅对师门成员开放」，区别于地区门禁的 403）；
/// - 未绑定账号（person_id=0 且非管理员）→ 403 同上；team=all 仅管理员可用。
///
/// 返回类型用 Response 而不是 ApiResult：403 需要带自定义 code 字段，ApiError 的
/// 错误体只有 {error}。
pub async fn lineage(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<MyQuery>,
    headers: HeaderMap,
) -> Response {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Json(json!({ "published": false })).into_response();
    }
    if !s.lineage_enabled {
        return Json(json!({ "published": true, "lineageEnabled": false })).into_response();
    }
    /* 音浪口径（2026-10-12 用户口径「支持日期查看音浪」）：
    daily=最新业务日当天；monthly/yearly=最新业务日所在月/年。
    date 参数可指定基准日（非法直接回落，不报错——族谱页不因脏参数打不开）。 */
    let dates: Vec<String> = crate::repos::get_available_dates(&st, None, false)
        .await
        .unwrap_or_default();
    let (period, base_date) = lineage_window(
        q.period.as_deref(),
        q.date.as_deref(),
        dates.first().map(String::as_str),
    );
    // 族谱门禁：永远要有效 token（不放行游客），身份判定见上方口径注释
    let Some(token) = header_my_token(&headers) else {
        return ApiError::auth("请先登录").into_response();
    };
    let Some((pid, my_team, account_name)) = my_token_take(&token) else {
        return ApiError::auth("登录已过期，请重新登录").into_response();
    };
    let is_admin = crate::store::my_account_is_admin(&st, &account_name).await;
    let query_team = if is_admin {
        // 管理员：行为不变（任意团；all = 两团全量，即 SQL 里团别条件恒真）
        if team == "all" {
            String::new()
        } else {
            team.clone()
        }
    } else {
        // 普通账号：token 团别与路径团别不符照旧 401（与登录门同口径）。
        // 空团别（团别待定 / view_team=any）放行任何路径团别 —— 与 /my* 系列共用
        // super::my_team_allows，两处口径必须一致，否则「看板切了男团再点我的」会 401。
        if !super::my_team_allows(&my_team, &team) {
            return ApiError::auth("该账号不属于本团").into_response();
        }
        let (has_master, disciples) = match person_lineage_membership(&st, pid).await {
            Ok(v) => v,
            Err(e) => return ApiError::from(e).into_response(),
        };
        if !lineage_view_allowed(false, pid, has_master, disciples, &team) {
            return lineage_forbidden();
        }
        team.clone()
    };
    /* 族谱只显示有师门归属的人（2026-10-09 规则）：没有师傅且没有徒弟的孤立节点
    不下发（画布和导出图的「没有师傅」名单自动联动）。
    与榜单显隐（hide_in_daily_report）是两个独立配置：榜单隐藏的人只要挂在
    师门下，族谱照常显示。 */
    let sql = format!(
        "SELECT p.id, p.name, p.master_id, m.name AS master_name, p.status, p.generation, \
         (SELECT COUNT(*) FROM person k WHERE k.master_id = p.id AND k.deleted_at IS NULL) AS disciples \
         FROM person p LEFT JOIN person m ON m.id = p.master_id \
         WHERE p.deleted_at IS NULL \
         AND ('{query_team}' = '' OR p.gender = '{query_team}') \
         AND (p.master_id IS NOT NULL \
              OR EXISTS(SELECT 1 FROM person k WHERE k.master_id = p.id AND k.deleted_at IS NULL))"
    );
    let rows = match pgrs::query(&st.pools, &sid(&st), &sql).await {
        Ok(r) => r,
        Err(e) => return ApiError::from(e).into_response(),
    };

    // 个人音浪（2026-10-12 改按 period/date 窗口过滤；与榜单口径一致 wave_reliable=1。
    // 2026-10-16：月 / 年优先用导入的整月音浪，库没跑 026 迁移时按日明细口径回落）
    let mut wave_map: HashMap<u64, i64> = Default::default();
    match dialect::query_month_wave_fallback(
        &st.pools,
        &sid(&st),
        &lineage_wave_window_sql(&period, &base_date, true),
        &lineage_wave_window_sql(&period, &base_date, false),
    )
    .await
    {
        Ok(rs) => {
            for r in &rs {
                wave_map.insert(row_pid(r, "person_id"), row_i64(r, "total"));
            }
        }
        Err(e) => return ApiError::from(e).into_response(),
    }

    let mut nodes: Vec<Value> = Vec::with_capacity(rows.len());
    let mut ids: std::collections::HashSet<u64> = Default::default();
    let mut children: HashMap<u64, Vec<u64>> = Default::default();
    for r in &rows {
        let id = row_pid(r, "id");
        ids.insert(id);
        if let Some(master) = read_opt_u64_field(r, "master_id").filter(|m| ids.contains(m)) {
            children.entry(master).or_default().push(id);
        }
    }
    // 门下累计总音浪（自己 + 所有子孙）：带环保护的记忆化递归（见 subtree_wave_sum）。
    // 2026-10-14 审计 P1-6：这份曾经是「先递归、后 memo」的第二份拷贝 —— 自指/成环
    // 时无限递归打崩进程，现在两处共用同一份实现。
    let mut subtree_memo: HashMap<u64, i64> = Default::default();
    let mut subtree_stack: std::collections::HashSet<u64> = Default::default();
    // 真实 id/masterId 先单独收集，最后统一重映射为响应内序号（见 lineage_remap_ids）
    let mut raw_ids: Vec<(u64, Option<u64>)> = Vec::with_capacity(rows.len());
    for r in &rows {
        let id = row_pid(r, "id");
        let master_id = read_opt_u64_field(r, "master_id").filter(|m| ids.contains(m));
        let wave = wave_map.get(&id).copied().unwrap_or(0);
        let total_wave = subtree_wave_sum(
            id,
            &children,
            &wave_map,
            &mut subtree_memo,
            &mut subtree_stack,
        );
        raw_ids.push((id, master_id));
        nodes.push(json!({
            "name": r.try_get::<String, _>("name").unwrap_or_default(),
            "masterName": r.try_get::<Option<String>, _>("master_name").ok().flatten(),
            "status": r.try_get::<String, _>("status").unwrap_or_default(),
            /* person.generation 是 INT4（001_core.sql），按 Option<i64> 读会 ColumnDecode
            被 `.ok().flatten()` 吞成 null（族谱上「第几代」永远空）—— 走类型回退 helper */
            "generation": row_opt_i64(r, "generation"),
            "disciples": row_i64(r, "disciples"),
            // 音浪数值显隐（后台开关）：关掉后不下发任何数字
            "wave": if s.wave { json!(wave) } else { Value::Null },
            "totalWave": if s.wave { json!(total_wave) } else { Value::Null },
        }));
    }
    for (node, (id, master)) in nodes.iter_mut().zip(lineage_remap_ids(&raw_ids)) {
        node["id"] = json!(id);
        node["masterId"] = match master {
            Some(m) => json!(m),
            None => Value::Null,
        };
    }
    Json(json!({
        "published": true,
        "team": team,
        "nodes": nodes,
        // 2026-10-12：音浪口径与基准日（前端切日/月/年时回显 + 请求下一次窗口用）
        "period": period,
        "date": base_date,
        // 前端据此区分管理员（全组树）与普通主播（只看自己徒弟）
        "viewerIsAdmin": is_admin,
    }))
    .into_response()
}

/// 族谱音浪窗口（纯函数）：period 非法回落 daily；date 非法回落最新业务日。
/// 返回 (生效口径, 基准日)。
pub(super) fn lineage_window(
    period: Option<&str>,
    date: Option<&str>,
    latest: Option<&str>,
) -> (String, String) {
    let period = match period.map(str::trim).filter(|s| !s.is_empty()) {
        Some("monthly") => "monthly",
        Some("yearly") => "yearly",
        _ => "daily",
    };
    // 日期白名单：YYYY-MM-DD 且真实存在（防注入 + 防脏数据，照 push::is_biz_date）
    let base = date
        .map(str::trim)
        .filter(|d| d.len() == 10 && chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok())
        .or(latest)
        .unwrap_or_default()
        .to_string();
    (period.to_string(), base)
}

/// 族谱音浪 SQL（纯函数，2026-10-12）：按 period 窗口条件聚合。
/// monthly/yearly 由基准日推出窗口（TO_CHAR 前缀匹配），daily 精确到日。
/// base_date 已由 lineage_window 白名单校验后才拼进 SQL。
///
/// 整月口径（2026-10-16 月度音浪导入）：monthly / yearly 优先用导入的整月音浪
/// （monthly_wave），**只有该月没导过整月值才回落日明细求和** —— 与月榜 / 个人月卡
/// （dialect::period_rollup_sql、minutes::my_wave_sql）同一套 COALESCE 口径。
/// daily 口径不动（就是当天日明细）；累计（totalWave）也不动（走日明细，累计再叠整月值 = 双算）。
/// `with_monthly_wave = false`（库没跑 026 迁移）时退回纯日明细 SQL —— 由调用方
/// [lineage] 走 dialect::query_month_wave_fallback 探测后决定，别让少一张表把族谱打挂。
pub(super) fn lineage_wave_window_sql(
    period: &str,
    base_date: &str,
    with_monthly_wave: bool,
) -> String {
    /* 双保险：调用方 lineage_window 已校验，但纯函数直接被测试/复用时也挡一道。
    日期形状非法 → 退回无窗口条件（空库口径）；绝不把外部字符串带进 SQL。 */
    let base_date = if base_date.len() == 10
        && chrono::NaiveDate::parse_from_str(base_date, "%Y-%m-%d").is_ok()
    {
        base_date
    } else {
        // 空串（库没数据）或形状非法 → 都按空库口径，绝不把外部字符串带进 SQL
        ""
    };
    let where_clause = if base_date.is_empty() {
        "WHERE wave_reliable = 1".to_string() // 没有任何业务日：不给窗口条件（全空库）
    } else {
        match period {
            "monthly" => format!(
                "WHERE wave_reliable = 1 AND TO_CHAR(biz_date, 'YYYY-MM') = '{}'",
                base_date.get(..7).unwrap_or(base_date)
            ),
            "yearly" => format!(
                "WHERE wave_reliable = 1 AND TO_CHAR(biz_date, 'YYYY') = '{}'",
                base_date.get(..4).unwrap_or(base_date)
            ),
            _ => format!(
                "WHERE wave_reliable = 1 AND TO_CHAR(biz_date, 'YYYY-MM-DD') = '{base_date}'"
            ),
        }
    };
    // daily 口径 / 缺表回落 / 库没数据：老 SQL 一字不动（月年口径才有整月音浪这回事）
    if !with_monthly_wave || base_date.is_empty() || (period != "monthly" && period != "yearly") {
        return format!(
            "SELECT person_id, (SUM(wave))::BIGINT AS total \
             FROM daily_metric {where_clause} GROUP BY person_id"
        );
    }
    /* 月 / 年：日明细按「每人每月」聚合后 LEFT JOIN monthly_wave，有整月值的月份用整月值，
    没有才用日明细的那一份（NOT EXISTS 与本支同一过滤，保证同一人同一月只走一路）。
    另外补上「导入了整月音浪、当月却没有可信日明细行」的人（平台月报常用来补采历史月份，
    否则这些人在族谱上永远是 0）—— 补齐行 mw 值由 LEFT JOIN 填上，不会与日明细双算。 */
    let month_rollup = format!(
        "SELECT d.person_id AS person_id, TO_CHAR(d.biz_date, 'YYYY-MM') AS m, SUM(d.wave) AS w \
         FROM daily_metric d {where_clause} GROUP BY d.person_id, m"
    );
    let month_pred = if period == "monthly" {
        format!("mw0.period = '{}'", base_date.get(..7).unwrap_or(base_date))
    } else {
        format!(
            "mw0.period LIKE '{}-%'",
            base_date.get(..4).unwrap_or(base_date)
        )
    };
    format!(
        "SELECT t.person_id, (SUM(COALESCE(mw.wave, t.w)))::BIGINT AS total FROM ( \
           {month_rollup} \
           UNION ALL \
           SELECT mw0.person_id AS person_id, mw0.period AS m, mw0.wave AS w \
             FROM monthly_wave mw0 \
            WHERE {month_pred} \
              AND NOT EXISTS (SELECT 1 FROM daily_metric d0 \
                               WHERE d0.person_id = mw0.person_id AND d0.wave_reliable = 1 \
                                 AND TO_CHAR(d0.biz_date, 'YYYY-MM') = mw0.period) \
         ) t \
         LEFT JOIN monthly_wave mw ON mw.person_id = t.person_id AND mw.period = t.m \
         GROUP BY t.person_id"
    )
}

/// 族谱访问判定（纯函数，口径与原因见 lineage 注释）：
/// 管理员恒放行；普通用户 team=all 拒绝（all 仅管理员）、未绑定（person_id=0）拒绝、
/// 无师门归属（没师傅也没徒弟）拒绝。
pub fn lineage_view_allowed(
    is_admin: bool,
    person_id: u64,
    has_master: bool,
    disciples: i64,
    team: &str,
) -> bool {
    if is_admin {
        return true;
    }
    if team == "all" || person_id == 0 {
        return false;
    }
    has_master || disciples > 0
}

/// 403 + code=lineage_forbidden：前端据此显示「族谱仅对师门成员开放」
/// （必须和地区门禁的 403 区分开，后者是 IP 白名单拦截、文案不同）。
pub(super) fn lineage_forbidden() -> Response {
    (
        axum::http::StatusCode::FORBIDDEN,
        Json(json!({
            "error": "族谱仅对师门成员开放（有师傅或徒弟的账号可见）",
            "code": "lineage_forbidden",
        })),
    )
        .into_response()
}

/// 本人师门归属：(有师傅, 直系徒弟数)。person_id=0（未绑定）或档案不存在 → (false, 0)。
pub(super) async fn person_lineage_membership(
    st: &AppState,
    person_id: u64,
) -> anyhow::Result<(bool, i64)> {
    if person_id == 0 {
        return Ok((false, 0));
    }
    let sql = "SELECT p.master_id, \
         (SELECT COUNT(*) FROM person k WHERE k.master_id = p.id AND k.deleted_at IS NULL) AS disciples \
         FROM person p WHERE p.id = ? AND p.deleted_at IS NULL LIMIT 1";
    let rows =
        pgrs::query_with_binds(&st.pools, &sid(st), sql, |q| q.bind(person_id as i64)).await?;
    Ok(match rows.first() {
        Some(r) => (
            read_opt_u64_field(r, "master_id")
                .map(|m| m > 0)
                .unwrap_or(false),
            row_i64(r, "disciples"),
        ),
        None => (false, 0),
    })
}

/// 族谱节点 id 重映射（2026-10-12 安全审计，对齐 daily 接口 P2-19：公开端点不下发内部主键）：
/// 真实 person.id → 本次响应内的序号 1..N，masterId 同步映射；
/// 未出现在节点集里的 masterId → None（前端把这种人当树根，与原口径一致）。
/// 前端只在一棵响应树内用这些 id 建图，序号映射够用。
pub fn lineage_remap_ids(nodes: &[(u64, Option<u64>)]) -> Vec<(u64, Option<u64>)> {
    let mut seq: HashMap<u64, u64> = HashMap::with_capacity(nodes.len());
    for (i, (id, _)) in nodes.iter().enumerate() {
        seq.insert(*id, (i + 1) as u64);
    }
    nodes
        .iter()
        .map(|(id, master)| (seq[id], master.and_then(|m| seq.get(&m).copied())))
        .collect()
}

/// 门下累计音浪（含自己 + 所有子孙）：**进入即标记**的记忆化递归。
///
/// 2026-10-14 审计 P1-6：原来 `lineage` 与 `lineage_wave_summary` 各有一份
/// 「先递归、后 memo.insert」的实现，而 children 由 `person.master_id` 直接构建、
/// 不过滤自指 —— 库里只要有一行 `master_id = 自己`（或 A→B→A 的环），递归就永不触底
/// → stack overflow → 整个进程 abort。`GET /api/public/teams/{team}/my` 与族谱接口
/// 都是公开可达的，所以脏数据最多「算错」，绝不能崩进程。
///
/// 现在进入节点先登记 `on_stack`：再次遇到同一节点（成环）直接截断返回 0。
/// 递归深度被「未重复节点数」限住（每人只可能在栈上一次），有环也必然终止。
fn subtree_wave_sum(
    id: u64,
    children: &HashMap<u64, Vec<u64>>,
    wave: &HashMap<u64, i64>,
    memo: &mut HashMap<u64, i64>,
    on_stack: &mut std::collections::HashSet<u64>,
) -> i64 {
    if let Some(v) = memo.get(&id) {
        return *v;
    }
    if !on_stack.insert(id) {
        // 脏数据成环：这一支截断（环内节点已经各自计过一次，不再重复累加）
        return 0;
    }
    let mut total = wave.get(&id).copied().unwrap_or(0);
    if let Some(kids) = children.get(&id) {
        for k in kids {
            total += subtree_wave_sum(*k, children, wave, memo, on_stack);
        }
    }
    on_stack.remove(&id);
    memo.insert(id, total);
    total
}

/// 「徒弟 + 师傅」门下累计音浪统计（2026-10-11）：
/// 师傅自己 + 所有子孙的可信音浪（wave_reliable=1）总和。
/// 返回 (直系徒弟数, 徒弟音浪总和[不含自己], 门下累计音浪[含自己])。
pub async fn lineage_wave_summary(
    st: &AppState,
    team: &str,
    person_id: u64,
) -> anyhow::Result<(i64, i64, i64)> {
    if person_id == 0 {
        return Ok((0, 0, 0));
    }
    let source = sid(st);
    // 本团 person 树（只认有师门归属关系的那部分，与族谱同口径）
    /* 2026-10-17 审计 4-P2-1：team 不再内联进 SQL —— 本函数由「我的」个人页用
    token 里的团别调用（minutes.rs 的 lineage_wave_summary(&st, &my_team, …)），
    而 token 读侧（my_token_required）没有白名单；原先 `gender = '{team}'` 是残留
    注入面。改两处 `?` 绑定（空串 = 不过滤，与旧行为一致）。 */
    let tree_sql = "SELECT id, master_id FROM person \
         WHERE deleted_at IS NULL AND (? = '' OR gender = ?)";
    let rows = pgrs::query_with_binds(&st.pools, &source, tree_sql, |q| {
        q.bind(team.to_string()).bind(team.to_string())
    })
    .await?;
    let mut ids: std::collections::HashSet<u64> = Default::default();
    let mut children: HashMap<u64, Vec<u64>> = Default::default();
    for r in &rows {
        let id = row_pid(r, "id");
        ids.insert(id);
    }
    for r in &rows {
        let id = row_pid(r, "id");
        if let Some(master) = read_opt_u64_field(r, "master_id").filter(|m| ids.contains(m)) {
            children.entry(master).or_default().push(id);
        }
    }
    // 每人累计总音浪（与榜单 totalWave 同口径）
    let wave_sql = "SELECT person_id, (SUM(wave))::BIGINT AS total \
         FROM daily_metric WHERE wave_reliable = 1 GROUP BY person_id"
        .to_string();
    let mut wave_map: HashMap<u64, i64> = Default::default();
    for r in pgrs::query(&st.pools, &source, &wave_sql).await? {
        wave_map.insert(row_pid(&r, "person_id"), row_i64(&r, "total"));
    }
    // 递归门下累计（记忆化 + 进入即标记，见 subtree_wave_sum 的环保护说明）
    let mut memo: HashMap<u64, i64> = Default::default();
    let mut on_stack: std::collections::HashSet<u64> = Default::default();
    let total_wave = subtree_wave_sum(person_id, &children, &wave_map, &mut memo, &mut on_stack);
    let self_wave = wave_map.get(&person_id).copied().unwrap_or(0);
    let disciple_count = children
        .get(&person_id)
        .map(|v| v.len() as i64)
        .unwrap_or(0);
    Ok((disciple_count, total_wave - self_wave, total_wave))
}

pub(super) fn read_opt_u64_field(r: &sqlx::postgres::PgRow, col: &str) -> Option<u64> {
    // 原来第二/第三个分支逐字重复了第一个（死代码），int4 列读不出来 ——
    // 统一走 row_opt_i64 的类型回退（2026-10-14 审计 P0-2 同类清理）
    row_opt_i64(r, col).map(|v| v.max(0) as u64)
}

/// GET /api/public/teams/{team}/monthly?period=YYYY-MM（或 /yearly?period=YYYY）
pub async fn monthly(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    let query_team = data_team(&team, &token_team(&headers)).to_string();
    gate_team(&st, s.login_required, &team, &headers).await?;
    // 显隐判定用**路径上的团**（all 恒为开放），数据过滤才用 query_team。
    // 以前把 query_team 传进去判 team_enabled，匿名访客 token 为空 → data_team 返回 ""
    // → team_enabled("") == false，月/年榜对所有人 published:false（前端 .slice 崩）。
    period_public(&st, &team, &query_team, &q, true).await
}

pub async fn yearly(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    let query_team = data_team(&team, &token_team(&headers)).to_string();
    gate_team(&st, s.login_required, &team, &headers).await?;
    period_public(&st, &team, &query_team, &q, false).await
}

/// `gate` = URL 路径上的团（只用来判显隐：all 恒开放）；`team` = 实际数据过滤用的团
/// （匿名 all → 空串 = 不过滤，绑了男/女团的主播 → male/female）。
pub(super) async fn period_public(
    st: &Arc<AppState>,
    gate: &str,
    team: &str,
    q: &ListQuery,
    is_month: bool,
) -> ApiResult {
    let s = PublicSettings::load(st).await;
    if !s.team_enabled(gate) {
        return Ok(Json(json!({ "published": false })));
    }
    let enabled = if is_month { s.monthly } else { s.yearly };
    if !enabled {
        return Ok(Json(json!({ "published": true, "periodEnabled": false })));
    }
    /* period 形状校验（2026-10-17 审计 4-P3-1）：原先只查「字符集 + 长度」，
    `2026-1-`（月）/ `20-2`（年）这类畸形窗口能通过 → like_to_date_range 认不出
    形状 → 退回「全时间范围」，月/年榜会把**全时段聚合**当该窗口返回（数值错，
    非注入）。现在统一走 dialect::valid_period_shape（严格 YYYY-MM / YYYY），
    非法回退当前窗口并记 debug，畸形输入不再进 period_rollup_sql。 */
    let want_len = if is_month { 7 } else { 4 };
    let default_period = if is_month {
        chrono::Local::now().format("%Y-%m").to_string()
    } else {
        chrono::Local::now().format("%Y").to_string()
    };
    let period = match q.period.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) if dialect::valid_period_shape(p) && p.len() == want_len => p.to_string(),
        Some(p) => {
            tracing::debug!(period = %p, is_month, "月/年榜 period 形状非法，回退当前窗口");
            default_period
        }
        None => default_period,
    };
    // 与 daily 口径对齐：离队的人不进榜（P2-19：也不再下发 personId，
    // 前端月/年榜只按 name/rank/wave 渲染）。
    // P1-7：period_rollup_sql 内层已统一按 wave_reliable=1 过滤。
    // 整月口径 2026-10-10 / 2026-10-16（用户口径）：月 / 年里的「时长」优先用按月导入的整月值
    // （monthly_duration，平台月报）、「音浪」优先用整月音浪（monthly_wave），没有整月记录的
    // 月份才用日明细求和 —— 见 dialect::period_rollup_sql。缺哪张整月表就只回落哪一项
    // （dialect::query_period 里两个开关互相独立）。
    let rows = dialect::query_period(
        &st.pools,
        &sid(st),
        "p.id AS person_id, p.name, \
         (SUM({wave}))::BIGINT AS wave, \
         (SUM({minutes}))::BIGINT AS minutes, \
         (SUM(t.live))::BIGINT AS live_days, \
         (SUM(t.off))::BIGINT AS off_days",
        &format!("{period}%"),
        team,
        " AND p.status <> 'left' AND p.hide_in_daily_report = 0",
        "wave DESC, p.name ASC",
    )
    .await?;
    // 头像（主播列表用）：与 daily 同口径，一次查完在内存 join
    let avatars = avatar_map(st, &sid(st)).await;
    // 名次：同音浪并列，下一档跳号（与 daily / 分组页的名次算法一致）
    let mut last_wave: Option<i64> = None;
    let mut last_rank = 0usize;
    let mut items: Vec<Value> = Vec::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        let pid = row_pid(r, "person_id");
        let wave = row_i64(r, "wave");
        let rank = if Some(wave) == last_wave {
            last_rank
        } else {
            last_rank = i + 1;
            last_rank
        };
        last_wave = Some(wave);
        let mut item = json!({
            // P2-19：公开月/年榜不再下发 personId（对齐 public.rs 白名单策略）
            "name": r.try_get::<String, _>("name").unwrap_or_default(),
            "wave": wave,
            "rank": rank,
        });
        /* 抖音头像（2026-10-16）：与 daily 同口径 —— 本地留存、同源地址，没有就是 null
        （前端回落姓名首字色块）。不设 personId 的白名单口径不变：URL 里只有 person_id
        与内容 hash，不含抖音号。 */
        item["avatar"] = match avatars.get(&pid) {
            Some(u) => json!(u),
            None => Value::Null,
        };
        if s.duration {
            item["minutes"] = json!(row_i64(r, "minutes"));
            item["liveDays"] = json!(row_i64(r, "live_days"));
        }
        item["absentDays"] = json!(row_i64(r, "off_days"));
        if !s.wave {
            item["wave"] = Value::Null;
        }
        items.push(item);
    }
    Ok(Json(
        json!({ "published": true, "period": period, "items": items }),
    ))
}

/// 读整数列（可空列也能读）：PG 里 int2 / int4 / int8 / bool / 数字文本都可能出现。
///
/// 2026-10-14 审计 P0-2 收口：本项目反复踩「SQL 表达式类型与 Rust 读取类型不精确匹配
/// → ColumnDecode → 被 unwrap_or(0) 吞成 0」的坑。本文件里 `live` 就是
/// `CASE WHEN ... THEN 1 ELSE 0 END`（= integer），而下面 row_live 原来只试
/// i16 / i64 / 文本三种：当日榜的 `isLive` 曾经恒 false。统一在这里做类型回退。
pub(super) fn row_opt_i64(r: &sqlx::postgres::PgRow, col: &str) -> Option<i64> {
    if let Ok(v) = r.try_get::<Option<i64>, _>(col) {
        return v;
    }
    if let Ok(v) = r.try_get::<Option<i32>, _>(col) {
        return v.map(i64::from);
    }
    if let Ok(v) = r.try_get::<Option<i16>, _>(col) {
        return v.map(i64::from);
    }
    if let Ok(v) = r.try_get::<Option<bool>, _>(col) {
        return v.map(i64::from);
    }
    // 兜底：个别环境把计数列灌成了文本（历史 MySQL → PG 灌库出过）
    r.try_get::<Option<String>, _>(col)
        .ok()
        .flatten()
        .and_then(|s| s.trim().parse::<i64>().ok())
}

pub(super) fn row_pid(r: &sqlx::postgres::PgRow, col: &str) -> u64 {
    row_opt_i64(r, col).map(|v| v.max(0) as u64).unwrap_or(0)
}

pub(super) fn valid_date_pub(s: &str) -> bool {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

pub(super) fn row_live(r: &sqlx::postgres::PgRow) -> bool {
    row_opt_i64(r, "live") == Some(1)
}

pub(super) fn row_i64(r: &sqlx::postgres::PgRow, col: &str) -> i64 {
    row_opt_i64(r, col).unwrap_or(0)
}

/// 自动冠军：最近一个业务日的当日音浪第一（绕过登录门，直接查只读池）
async fn star_auto(st: &AppState, team: &str, date: &str) -> (String, i64) {
    /* 2026-10-13：按所选日期取每日之星（前端切换日期时横幅跟随）；
    date 为空 = 最近业务日。all 团全量口径，male/female 按团过滤。 */
    let date_filter = if date.is_empty() {
        "TO_CHAR(d.biz_date,'YYYY-MM-DD') = (SELECT MAX(TO_CHAR(biz_date,'YYYY-MM-DD')) FROM daily_metric)".to_string()
    } else {
        format!("TO_CHAR(d.biz_date,'YYYY-MM-DD') = '{date}'")
    };
    let team_filter = if team == "all" {
        String::new()
    } else {
        format!("AND p.gender = '{team}' ")
    };
    let sql = format!(
        "SELECT p.name, SUM(d.wave)::BIGINT AS w \
         FROM daily_metric d JOIN person p ON p.id = d.person_id \
         WHERE d.wave_reliable = 1 {team_filter}\
           AND {date_filter} \
         GROUP BY p.id, p.name ORDER BY w DESC LIMIT 1"
    );
    match crate::pgrs::query(&st.pools, &st.active_id(), &sql).await {
        Ok(rows) => match rows.first() {
            Some(r) => (
                r.try_get::<String, _>("name").unwrap_or_default(),
                r.try_get::<i64, _>("w").unwrap_or(0),
            ),
            None => (String::new(), 0),
        },
        Err(_) => (String::new(), 0),
    }
}

/// GET /api/public/teams/{team}/star?date=YYYY-MM-DD：所选日期的每日之星
/// （前端切换日期时横幅跟随；未登录可看——榜单数据本来就公开）
pub async fn star_of_day(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let s = PublicSettings::load(&st).await;
    if !s.team_enabled(&team) {
        return Ok(Json(json!({ "published": false })));
    }
    let date = q.get("date").cloned().unwrap_or_default();
    let date = if date.is_empty() {
        String::new()
    } else {
        // 简单格式校验（防注入：只允许数字与连字符）
        if date.chars().all(|c| c.is_ascii_digit() || c == '-') {
            date
        } else {
            String::new()
        }
    };
    let (name, wave) = star_auto(&st, &team, &date).await;
    if name.is_empty() {
        return Ok(Json(json!({ "title": "每日之星", "name": "", "wave": 0 })));
    }
    Ok(Json(
        json!({ "title": "每日之星", "name": name, "wave": wave }),
    ))
}

fn auto_json(a: &(String, i64)) -> serde_json::Value {
    serde_json::json!({ "title": "每日之星", "name": a.0, "wave": a.1 })
}

/* --------------------------------- 单测 --------------------------------- */

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(
        edges: &[(u64, u64)],
        waves: &[(u64, i64)],
    ) -> (HashMap<u64, Vec<u64>>, HashMap<u64, i64>) {
        let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
        for (master, child) in edges {
            children.entry(*master).or_default().push(*child);
        }
        let wave: HashMap<u64, i64> = waves.iter().copied().collect();
        (children, wave)
    }

    fn total(start: u64, children: &HashMap<u64, Vec<u64>>, wave: &HashMap<u64, i64>) -> i64 {
        let mut memo: HashMap<u64, i64> = HashMap::new();
        let mut on_stack: std::collections::HashSet<u64> = std::collections::HashSet::new();
        subtree_wave_sum(start, children, wave, &mut memo, &mut on_stack)
    }

    /// 正常师门树：门下累计 = 自己 + 所有子孙（含隔代）
    #[test]
    fn subtree_sums_whole_tree() {
        // 1 → 2, 1 → 3, 2 → 4
        let (children, wave) = tree(
            &[(1, 2), (1, 3), (2, 4)],
            &[(1, 10), (2, 20), (3, 30), (4, 40)],
        );
        assert_eq!(total(1, &children, &wave), 100);
        assert_eq!(total(2, &children, &wave), 60);
        assert_eq!(total(4, &children, &wave), 40);
    }

    /// P1-6 回归：**自指**（master_id = 自己）绝不能无限递归。
    /// 修复前这里是 stack overflow（公开端点可打崩进程）。
    #[test]
    fn subtree_survives_self_reference() {
        let (children, wave) = tree(&[(7, 7)], &[(7, 5)]);
        assert_eq!(total(7, &children, &wave), 5, "自指只算自己一次");
    }

    /// P1-6 回归：**2 环**（A→B→A）同样必须终止
    #[test]
    fn subtree_survives_two_cycle() {
        let (children, wave) = tree(&[(1, 2), (2, 1)], &[(1, 3), (2, 4)]);
        assert_eq!(
            total(1, &children, &wave),
            7,
            "两人各算一次，不重复也不死循环"
        );
        // 从环上任意一点进入都要能收敛
        assert_eq!(total(2, &children, &wave), 7);
    }

    /// 长环（3 环）+ 环上挂着正常子树：仍然终止且不重复累加
    #[test]
    fn subtree_survives_three_cycle_with_branch() {
        // 1→2→3→1，且 2 还有徒弟 4
        let (children, wave) = tree(
            &[(1, 2), (2, 3), (3, 1), (2, 4)],
            &[(1, 1), (2, 2), (3, 3), (4, 4)],
        );
        assert_eq!(total(1, &children, &wave), 10);
    }

    /// 头像 URL 契约（2026-10-16）：`?v=<hash>` 只在 hash 非空时出现。
    /// 前端（T3 主播列表）与 `public_teams::avatars` 端点按这个形状对接：
    /// hash 为空时必须退化成不带 query 的地址（仍出图，只是换图不破缓存）。
    #[test]
    fn avatar_url_shape() {
        assert_eq!(
            avatar_url(1027, "0f1e2d3c4b5a6978"),
            "/api/public/avatars/1027?v=0f1e2d3c4b5a6978"
        );
        assert_eq!(avatar_url(1027, ""), "/api/public/avatars/1027");
        assert_eq!(avatar_url(1027, "   "), "/api/public/avatars/1027");
    }
}
