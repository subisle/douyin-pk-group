//! 访客 IP 归属地门禁 + 访问日志。
//!
//! 数据源全部来自 ihmily/ip-info-api 收集的免费接口
//! （<https://github.com/ihmily/ip-info-api>）：全部并行查询、投票合并、MySQL 缓存 7 天。
//! 展示端只放行河南 / 安徽——有任一接口识别出河南 / 安徽就放行；都没识别出来就不放行；
//! 所有接口都失败也拒绝（外部接口集体故障时门禁不放水）。
//! 本地开发用 `IP_GATE_DISABLED=1` 关掉拦截（访问日志照记）。
//!
//! 访问日志（access_logs）：页面访问记 kind=api（同 IP + 同路径 5 分钟去重），
//! 登录尝试记 kind=login（带账号与成败）；外省被拒的访问同样入库，后台「访客日志」页可查。
//!
//! 数据源集合可配置（逗号分隔，忽略未知名字，解析后为空回默认）：
//! - `IP_GATE_SOURCES`：门禁（访客检查）用源，默认 pconline,baidu,ip9（中文源）；
//! - `IP_QUERY_SOURCES`：IP 查询用源，默认 ip.sb,ip2location,ipwhois（ip.sb / ip2location
//!   实测 ~1s 稳定，ipinfo 的 demo 端点 6s 超时不稳定，不作为默认）。
//!
//! 合法名字即下面 `ALL_SOURCE_NAMES` 的 19 个；`GET /api/admin/ip-lookup` 诊断接口
//! 走 Query 集且不读不写 ip_geo_cache。

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use futures_util::future::join_all;
use serde_json::Value;
use sqlx::Row;

use crate::state::AppState;

type ArcState = Arc<AppState>;

const CACHE_TTL_DAYS: i64 = 7;
const LOOKUP_TIMEOUT: Duration = Duration::from_millis(3000);
const MAX_LOG_NAME: usize = 64;
/// 同一 IP 命中同一路径的访问日志去重窗口：页面一次加载会打多个接口，不去重会淹掉日志
const ACCESS_DEDUP_MINUTES: i64 = 5;
const MAX_SOURCE_LEN: usize = 180;

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct IpGeo {
    pub province: String,
    pub city: String,
    pub isp: String,
    /// 命中的来源列表（逗号分隔，排查用）
    pub source: String,
    /// 门禁判定结果（任一来源识别出河南 / 安徽 → true）
    pub allowed: bool,
}

/// 本地开发 / 内网联调：`IP_GATE_DISABLED=1` 时不做地区拦截（访问日志照记）
pub fn gate_disabled() -> bool {
    matches!(
        std::env::var("IP_GATE_DISABLED").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/* ------------------------------ 数据源集合 ------------------------------ */

/// 19 个数据源的规范名（顺序 = 源表顺序；后台诊断接口按这个顺序稳定输出）。
const ALL_SOURCE_NAMES: [&str; 19] = [
    "pconline",
    "baidu",
    "ip9",
    "ip-api",
    "ip.sb",
    "geojs",
    "ipwhois",
    "db-ip",
    "ipinfo",
    "ipquery",
    "ipdata.info",
    "ip2location",
    "freeipapi",
    "nc.gy",
    "ipwtf",
    "realip.cc",
    "geolocation-db",
    "ipapi.co",
    "db-ip-demo",
];

/// 数据源用途：Gate=访客门禁检查，Query=IP 查询（后台诊断）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceSet {
    Gate,
    Query,
}

impl SourceSet {
    fn env_key(self) -> &'static str {
        match self {
            SourceSet::Gate => "IP_GATE_SOURCES",
            SourceSet::Query => "IP_QUERY_SOURCES",
        }
    }

    fn default_names(self) -> &'static [&'static str] {
        match self {
            /* 门禁默认全用中文源：省份 / 城市 / 运营商准，判定不中英混排。
            只放行河南 / 安徽，3 个中文源足够且省外部配额。 */
            SourceSet::Gate => &["pconline", "baidu", "ip9"],
            /* IP 查询默认三源：实测 ip.sb ~1.0-1.2s、ip2location ~1.02s、ipwhois ~1.0s；
            ipinfo 的 demo 端点 6s 超时不稳，弃用。 */
            SourceSet::Query => &["ip.sb", "ip2location", "ipwhois"],
        }
    }

    /// 打印 / 排查用的短名
    pub fn label(self) -> &'static str {
        match self {
            SourceSet::Gate => "gate",
            SourceSet::Query => "query",
        }
    }
}

/// 解析逗号分隔的数据源名：去空白、忽略未知名字、去重（保留输入顺序）；
/// 解析后为空（没配 / 空串 / 全是未知名字）回退到该集合的默认源。
fn parse_source_names(raw: Option<&str>, defaults: &[&'static str]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for part in raw.unwrap_or_default().split(',') {
        let name = part.trim();
        if name.is_empty() {
            continue;
        }
        let canon = ALL_SOURCE_NAMES
            .iter()
            .copied()
            .find(|n| n.eq_ignore_ascii_case(name));
        let Some(canon) = canon else { continue };
        if !out.contains(&canon) {
            out.push(canon);
        }
    }
    if out.is_empty() {
        defaults.to_vec()
    } else {
        out
    }
}

/// 纯函数版：显式传入原始配置串（单测用，不读环境）。
fn source_names_for(set: SourceSet, raw: Option<&str>) -> Vec<&'static str> {
    parse_source_names(raw, set.default_names())
}

/// 当前配置下某个用途实际要用的源（读环境变量，未配置 / 全未知名字走默认）。
pub fn source_names(set: SourceSet) -> Vec<&'static str> {
    source_names_for(set, std::env::var(set.env_key()).ok().as_deref())
}

/* ------------------------------- 请求 IP ------------------------------- */

/// 生产流量经 Caddy / Cloudflare 到本机。只信任回环 / 私网连接传来的转发头，
/// 避免公网直连伪造 X-Forwarded-For。
pub fn request_ip(addr: SocketAddr, headers: &HeaderMap) -> String {
    if is_internal(&addr.ip()) {
        let names = ["cf-connecting-ip", "x-real-ip", "x-forwarded-for"];
        for name in names {
            if let Some(raw) = headers.get(name).and_then(|v| v.to_str().ok()) {
                let first = raw.split(',').next().unwrap_or_default().trim();
                if let Ok(ip) = first.parse::<IpAddr>() {
                    return ip.to_string();
                }
            }
        }
    }
    addr.ip().to_string()
}

fn is_internal(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// 展示端门禁中间件：挂在 /api/public/* 这一层。
/// 放行 / 拒绝都记访问日志；被拒返回 403（前端展示「当前地区暂不支持访问」，不等于掉登录）。
pub async fn ip_gate_mw(
    State(st): State<ArcState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let ip = request_ip(addr, req.headers());
    let path = req.uri().path().to_string();
    /* verify 与团别无关登录口 /my/login 都交由 handler 自己过门禁：那里能读到请求体里的
    account，被拦下的外省尝试也要落一条登录日志（后台「访客日志」看得到谁在门外
    试了什么账号），所以中间件对这两个口直接放行——不拦、也不记重复的 api 条目 */
    if path.ends_with("/verify") || path.ends_with("/my/login") {
        return next.run(req).await;
    }
    let query = req.uri().query().unwrap_or_default().to_string();
    let team = team_from_path(&path);
    let account = account_from_query(&query);
    let geo = geo_for_ip(&st, &ip).await.unwrap_or_default();
    let allowed = gate_disabled() || geo.allowed;
    log_access(
        &st,
        AccessLog {
            kind: "api",
            ip: &ip,
            geo: &geo,
            team: &team,
            path: &path,
            account: &account,
            allowed,
            login_ok: false,
        },
    )
    .await;
    if !allowed {
        tracing::info!(
            ip = %ip, team = %team, province = %geo.province, path = %path,
            "展示端访问被地区规则拒绝"
        );
        return (
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({ "error": "当前地区暂不支持访问" })),
        )
            .into_response();
    }
    next.run(req).await
}

/// `/api/public/teams/{team}/...` → team；其他公开路径没有团别
fn team_from_path(path: &str) -> String {
    let mut segs = path.trim_start_matches('/').split('/');
    if segs.next() != Some("api") || segs.next() != Some("public") || segs.next() != Some("teams") {
        return String::new();
    }
    segs.next().unwrap_or_default().to_string()
}

/// my token 是登录态唯一的凭据：能从 query 里认出来就把账号带进访问日志
fn account_from_query(query: &str) -> String {
    for pair in query.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        if k == "token" || k == "access_token" {
            if let Some((_, _, name)) = crate::public_teams::my_token_peek(v) {
                return name;
            }
        }
    }
    String::new()
}

/* ------------------------------- 归属地查询 ------------------------------- */

#[derive(Default)]
struct RawGeo {
    province: String,
    city: String,
    isp: String,
}

impl RawGeo {
    fn new(province: impl Into<String>, city: impl Into<String>, isp: impl Into<String>) -> Self {
        Self {
            province: province.into(),
            city: city.into(),
            isp: isp.into(),
        }
    }
}

struct Source {
    name: &'static str,
    url: String,
    /// pconline 返回 GBK（其余都是 UTF-8）
    gbk: bool,
    parse: fn(&Value) -> RawGeo,
}

/// 并行查询一批源（3s 超时）：返回 (源名, 结果)；HTTP 失败 / 解析失败 = None。
/// 门禁（geo_for_ip）与后台诊断接口（admin_ip_lookup）共用，保证两边口径一致。
async fn query_sources(sources: Vec<Source>) -> Vec<(&'static str, Option<RawGeo>)> {
    let Ok(client) = reqwest::Client::builder()
        .timeout(LOOKUP_TIMEOUT)
        .user_agent("pk-server-ip-gate/1.0 (+https://pk.subisle.com)")
        .build()
    else {
        return sources.into_iter().map(|s| (s.name, None)).collect();
    };
    let futures = sources.into_iter().map(|s| {
        let client = client.clone();
        async move { (s.name, fetch_source(&client, &s).await) }
    });
    join_all(futures).await
}

/// 单源取数：HTTP 成功 + JSON 可解析才算拿到结果（省份可能为空，交给 merge 判定）。
async fn fetch_source(client: &reqwest::Client, s: &Source) -> Option<RawGeo> {
    let resp = client.get(&s.url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().await.ok()?;
    let text = if s.gbk {
        encoding_rs::GBK.decode(&bytes).0.into_owned()
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let json: Value = serde_json::from_str(text.trim()).ok()?;
    Some((s.parse)(&json))
}

/// 缓存放 MySQL：查过的 IP 7 天内不再打外部接口
pub async fn geo_for_ip(st: &ArcState, ip: &str) -> anyhow::Result<IpGeo> {
    let parsed: IpAddr = ip.trim().parse()?;
    if is_internal(&parsed) {
        return Ok(IpGeo {
            province: "内网".into(),
            source: "local".into(),
            ..Default::default()
        });
    }
    let packed = pack_ip(parsed);
    let Some(pool) = st.writer.as_ref() else {
        anyhow::bail!("写库不可用，无法查询 IP 归属地缓存");
    };

    if let Ok(Some(row)) = sqlx::query(
        "SELECT province, city, isp, source, allowed, looked_up_at FROM ip_geo_cache WHERE ip = ?",
    )
    .bind(&packed)
    .fetch_optional(pool)
    .await
    {
        let looked_up_at: chrono::DateTime<chrono::Utc> =
            row.try_get(5).unwrap_or_else(|_| chrono::Utc::now());
        if chrono::Utc::now()
            .signed_duration_since(looked_up_at)
            .num_days()
            < CACHE_TTL_DAYS
        {
            return Ok(IpGeo {
                province: row.try_get(0).unwrap_or_default(),
                city: row.try_get(1).unwrap_or_default(),
                isp: row.try_get(2).unwrap_or_default(),
                source: row.try_get(3).unwrap_or_default(),
                allowed: row.try_get::<i8, _>(4).unwrap_or(0) == 1,
            });
        }
    }

    let probes = query_sources(sources_for(&parsed.to_string(), SourceSet::Gate)).await;
    let raws: Vec<(&str, RawGeo)> = probes
        .into_iter()
        .filter_map(|(name, geo)| geo.map(|g| (name, g)))
        .collect();
    let geo = merge(&raws);
    if geo.province.is_empty() {
        anyhow::bail!("所有 IP 归属接口都没返回省份");
    }

    let _ = sqlx::query(
        "INSERT INTO ip_geo_cache (ip, province, city, isp, source, allowed, looked_up_at) \
         VALUES (?, ?, ?, ?, ?, ?, UTC_TIMESTAMP()) \
         ON DUPLICATE KEY UPDATE province = VALUES(province), city = VALUES(city), \
         isp = VALUES(isp), source = VALUES(source), allowed = VALUES(allowed), \
         looked_up_at = UTC_TIMESTAMP()",
    )
    .bind(&packed)
    .bind(&geo.province)
    .bind(&geo.city)
    .bind(&geo.isp)
    .bind(&geo.source)
    .bind(geo.allowed as i8)
    .execute(pool)
    .await;
    Ok(geo)
}

/// 全部 19 个数据源（ihmily/ip-info-api 里支持「指定 IP 查询」的免费接口），顺序固定。
/// 中文源放前面：合并时城市 / 运营商优先取中文源，前端展示不中英混排。
fn all_sources(ip: &str) -> Vec<Source> {
    let s = |name: &'static str, url: String, parse: fn(&Value) -> RawGeo| Source {
        name,
        url,
        gbk: false,
        parse,
    };
    vec![
        Source {
            name: "pconline",
            url: format!("https://whois.pconline.com.cn/ipJson.jsp?ip={ip}&json=true"),
            gbk: true,
            parse: parse_pconline,
        },
        s(
            "baidu",
            format!("https://opendata.baidu.com/api.php?co=&resource_id=6006&oe=utf8&query={ip}"),
            parse_baidu,
        ),
        s("ip9", format!("https://ip9.com.cn/get?ip={ip}"), parse_ip9),
        s(
            "ip-api",
            format!(
                "http://ip-api.com/json/{ip}?lang=zh-CN&fields=status,country,regionName,city,isp"
            ),
            parse_ip_api,
        ),
        s(
            "ip.sb",
            format!("https://api.ip.sb/geoip/{ip}"),
            parse_ip_sb,
        ),
        s(
            "geojs",
            format!("https://get.geojs.io/v1/ip/geo/{ip}.json"),
            parse_geojs,
        ),
        s(
            "ipwhois",
            format!("https://ipwhois.app/json/{ip}?format=json"),
            parse_ipwhois,
        ),
        s(
            "db-ip",
            format!("https://api.db-ip.com/v2/free/{ip}"),
            parse_db_ip,
        ),
        s(
            "ipinfo",
            format!("https://ipinfo.io/widget/demo/{ip}"),
            parse_ipinfo_widget,
        ),
        s(
            "ipquery",
            format!("https://api.ipquery.io/{ip}?format=json"),
            parse_ipquery,
        ),
        s(
            "ipdata.info",
            format!("https://ipdata.info/json/{ip}"),
            parse_ipdata_info,
        ),
        s(
            "ip2location",
            format!("https://api.ip2location.io/?ip={ip}"),
            parse_ip2location,
        ),
        s(
            "freeipapi",
            format!("https://freeipapi.com/api/json/{ip}"),
            parse_freeipapi,
        ),
        s(
            "nc.gy",
            format!("https://ip.nc.gy/json?ip={ip}"),
            parse_ncgy,
        ),
        s(
            "ipwtf",
            format!("https://ipwtf.com/api/whois/{ip}"),
            parse_ipwtf,
        ),
        s(
            "realip.cc",
            format!("https://realip.cc/?ip={ip}"),
            parse_realip,
        ),
        s(
            "geolocation-db",
            format!("https://geolocation-db.com/json/{ip}"),
            parse_geolocation_db,
        ),
        s(
            "ipapi.co",
            format!("https://ipapi.co/{ip}/json/"),
            parse_ipapi_co,
        ),
        s(
            "db-ip-demo",
            format!("https://db-ip.com/demo/home.php?s={ip}"),
            parse_dbip_demo,
        ),
    ]
}

/// 按名字过滤源表（保留源表顺序；未知名字自然被忽略）。
fn sources_named(ip: &str, names: &[&str]) -> Vec<Source> {
    all_sources(ip)
        .into_iter()
        .filter(|s| names.contains(&s.name))
        .collect()
}

/// 当前配置下某个用途实际要用的源（Gate=门禁 / Query=IP 查询，见 SourceSet）。
fn sources_for(ip: &str, set: SourceSet) -> Vec<Source> {
    sources_named(ip, &source_names(set))
}

/// 合并：省份投票（票数最多者展示；并列优先河南 / 安徽），
/// 放行判定 = 任一数据源识别出河南 / 安徽（原口径：宁可对属地的误报放行，也别把真人挡在门外）。
/// 城市 / 运营商取「说中最终省份」的第一个来源（中文源在列表前面）。
fn merge(raws: &[(&str, RawGeo)]) -> IpGeo {
    let mut votes: HashMap<String, i64> = HashMap::new();
    let mut allowed = false;
    let mut sources: Vec<&str> = Vec::new();
    for (name, raw) in raws {
        let province = normalize_province(&raw.province);
        if province.is_empty() {
            continue;
        }
        if !sources.contains(name) {
            sources.push(name);
        }
        if allowed_province(&province) {
            allowed = true;
        }
        *votes.entry(province).or_insert(0) += 1;
    }
    let province = votes
        .iter()
        .max_by_key(|(p, n)| (**n, allowed_province(p) as i64))
        .map(|(p, _)| p.clone())
        .unwrap_or_default();

    let mut city = String::new();
    let mut isp = String::new();
    for (_, raw) in raws {
        if normalize_province(&raw.province) != province {
            continue;
        }
        if city.is_empty() && !raw.city.trim().is_empty() {
            city = trim_city(&raw.city);
        }
        if isp.is_empty() && !raw.isp.trim().is_empty() {
            isp = raw.isp.trim().to_string();
        }
        if !city.is_empty() && !isp.is_empty() {
            break;
        }
    }
    if city.is_empty() {
        city = raws
            .iter()
            .map(|(_, r)| r.city.trim().to_string())
            .find(|c| !c.is_empty())
            .unwrap_or_default();
    }
    if isp.is_empty() {
        isp = raws
            .iter()
            .map(|(_, r)| r.isp.trim().to_string())
            .find(|c| !c.is_empty())
            .unwrap_or_default();
    }
    let mut source = sources.join(",");
    if source.chars().count() > MAX_SOURCE_LEN {
        source = source.chars().take(MAX_SOURCE_LEN).collect();
    }
    IpGeo {
        province,
        city,
        isp,
        source,
        allowed,
    }
}

/* ------------------------------- 省份归一 ------------------------------- */

/// 中英文省名 → 中文简称。英文源（ip.sb / geojs / db-ip…）返回 "Henan Sheng" 这类值。
const PROVINCE_EN: &[(&str, &str)] = &[
    ("北京", "beijing"),
    ("天津", "tianjin"),
    ("河北", "hebei"),
    ("山西", "shanxi"),
    ("内蒙古", "inner mongolia"),
    ("辽宁", "liaoning"),
    ("吉林", "jilin"),
    ("黑龙江", "heilongjiang"),
    ("上海", "shanghai"),
    ("江苏", "jiangsu"),
    ("浙江", "zhejiang"),
    ("安徽", "anhui"),
    ("福建", "fujian"),
    ("江西", "jiangxi"),
    ("山东", "shandong"),
    ("河南", "henan"),
    ("湖北", "hubei"),
    ("湖南", "hunan"),
    ("广东", "guangdong"),
    ("广西", "guangxi"),
    ("海南", "hainan"),
    ("重庆", "chongqing"),
    ("四川", "sichuan"),
    ("贵州", "guizhou"),
    ("云南", "yunnan"),
    ("西藏", "tibet"),
    ("陕西", "shaanxi"),
    ("甘肃", "gansu"),
    ("青海", "qinghai"),
    ("宁夏", "ningxia"),
    ("新疆", "xinjiang"),
    ("香港", "hong kong"),
    ("澳门", "macao"),
    ("台湾", "taiwan"),
];

/// "河南省" / "Henan Sheng" / "Henan" → "河南"；认不出原样返回（日志里照样能看）
pub fn normalize_province(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() || t == "*" || t == "-" {
        return String::new();
    }
    for (zh, _) in PROVINCE_EN {
        if t.contains(zh) {
            return (*zh).to_string();
        }
    }
    let lower = t.to_lowercase().replace(['.', '_'], " ");
    let mut lower = lower.trim().to_string();
    for suffix in [
        " province",
        " sheng",
        " shi",
        " municipality",
        " autonomous region",
    ] {
        if let Some(stripped) = lower.strip_suffix(suffix) {
            lower = stripped.trim().to_string();
        }
    }
    for (zh, en) in PROVINCE_EN {
        if lower == *en || lower.contains(en) {
            return (*zh).to_string();
        }
    }
    t.to_string()
}

/// 门禁口径：河南 / 安徽
pub fn allowed_province(province: &str) -> bool {
    let p = normalize_province(province);
    p == "河南" || p == "安徽"
}

/* ------------------------------- 各源解析 ------------------------------- */

fn text(v: &Value, keys: &[&str]) -> String {
    let mut cur = v;
    for k in keys {
        // JSON 路径里 "0" 表示数组下标（nc.gy 的 subdivisions[0]）
        let next = match cur {
            Value::Array(list) => k.parse::<usize>().ok().and_then(|i| list.get(i)),
            _ => cur.get(*k),
        };
        match next {
            Some(x) => cur = x,
            None => return String::new(),
        }
    }
    match cur {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// "河南省郑州市 联通" → 省=河南 市=郑州 isp=联通
fn split_location(loc: &str) -> RawGeo {
    let mut parts = loc.split_whitespace();
    let area = parts.next().unwrap_or_default();
    let isp = parts.next().unwrap_or_default().trim();
    for (zh, _) in PROVINCE_EN {
        if let Some(pos) = area.find(zh) {
            let end = pos + zh.len();
            return RawGeo::new(*zh, trim_city(&area[end..]), isp);
        }
    }
    RawGeo::new("", trim_city(area), isp)
}

fn trim_city(raw: &str) -> String {
    let mut s = raw.trim();
    loop {
        let mut changed = false;
        for p in [
            "维吾尔自治区",
            "壮族自治区",
            "回族自治区",
            "自治区",
            "特别行政区",
            "省",
            "市",
        ] {
            if let Some(rest) = s.strip_prefix(p) {
                s = rest.trim_start();
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // 尾巴上的行政级别也去掉：郑州市 → 郑州
    for suffix in ["自治州", "地区", "市", "盟"] {
        if let Some(rest) = s.strip_suffix(suffix) {
            s = rest;
        }
    }
    s.trim().to_string()
}

fn parse_pconline(v: &Value) -> RawGeo {
    if !v
        .get("err")
        .and_then(Value::as_str)
        .unwrap_or("")
        .is_empty()
    {
        return RawGeo::default();
    }
    let addr = text(v, &["addr"]);
    RawGeo {
        province: text(v, &["pro"]),
        city: text(v, &["city"]),
        isp: addr
            .split_whitespace()
            .last()
            .unwrap_or_default()
            .to_string(),
    }
}

fn parse_baidu(v: &Value) -> RawGeo {
    let loc = v
        .get("data")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .map(|d| text(d, &["location"]))
        .unwrap_or_default();
    split_location(&loc)
}

fn parse_ip9(v: &Value) -> RawGeo {
    let d = v.get("data").unwrap_or(&Value::Null);
    RawGeo::new(text(d, &["prov"]), text(d, &["city"]), text(d, &["isp"]))
}

fn parse_ip_api(v: &Value) -> RawGeo {
    if v.get("status").and_then(Value::as_str) != Some("success") {
        return RawGeo::default();
    }
    RawGeo::new(
        text(v, &["regionName"]),
        text(v, &["city"]),
        text(v, &["isp"]),
    )
}

fn parse_ip_sb(v: &Value) -> RawGeo {
    let isp = {
        let s = text(v, &["isp"]);
        if s.is_empty() {
            text(v, &["organization"])
        } else {
            s
        }
    };
    RawGeo::new(text(v, &["region"]), text(v, &["city"]), isp)
}

fn parse_geojs(v: &Value) -> RawGeo {
    let isp = {
        let s = text(v, &["organization_name"]);
        if s.is_empty() {
            text(v, &["organization"])
        } else {
            s
        }
    };
    RawGeo::new(text(v, &["region"]), text(v, &["city"]), isp)
}

fn parse_ipwhois(v: &Value) -> RawGeo {
    if !v.get("success").and_then(Value::as_bool).unwrap_or(false) {
        return RawGeo::default();
    }
    let isp = {
        let s = text(v, &["isp"]);
        if s.is_empty() {
            text(v, &["org"])
        } else {
            s
        }
    };
    RawGeo::new(text(v, &["region"]), text(v, &["city"]), isp)
}

fn parse_db_ip(v: &Value) -> RawGeo {
    RawGeo::new(text(v, &["stateProv"]), text(v, &["city"]), "")
}

fn parse_ipinfo_widget(v: &Value) -> RawGeo {
    let d = v.get("data").unwrap_or(&Value::Null);
    RawGeo::new(text(d, &["region"]), text(d, &["city"]), text(d, &["org"]))
}

fn parse_ipquery(v: &Value) -> RawGeo {
    RawGeo::new(
        text(v, &["location", "state"]),
        text(v, &["location", "city"]),
        text(v, &["isp", "isp"]),
    )
}

fn parse_ipdata_info(v: &Value) -> RawGeo {
    let isp = {
        let s = text(v, &["isp"]);
        if !s.is_empty() {
            s
        } else {
            let o = text(v, &["org"]);
            if o.is_empty() {
                text(v, &["organization"])
            } else {
                o
            }
        }
    };
    RawGeo::new(text(v, &["region"]), text(v, &["city"]), isp)
}

fn parse_ip2location(v: &Value) -> RawGeo {
    RawGeo::new(
        text(v, &["region_name"]),
        text(v, &["city_name"]),
        text(v, &["as"]),
    )
}

fn parse_freeipapi(v: &Value) -> RawGeo {
    RawGeo::new(text(v, &["regionName"]), text(v, &["cityName"]), "")
}

fn parse_ncgy(v: &Value) -> RawGeo {
    let province = {
        let zh = text(v, &["subdivisions", "0", "names", "zh-CN"]);
        if !zh.is_empty() {
            zh
        } else {
            text(v, &["subdivisions", "0", "names", "en"])
        }
    };
    let city = {
        let zh = text(v, &["city", "names", "zh-CN"]);
        if !zh.is_empty() {
            zh
        } else {
            text(v, &["city", "names", "en"])
        }
    };
    RawGeo::new(province, city, "")
}

fn parse_ipwtf(v: &Value) -> RawGeo {
    let d = v.get("ipv4").unwrap_or(v);
    RawGeo::new(
        text(d, &["regionName"]),
        text(d, &["cityName"]),
        text(d, &["as"]),
    )
}

fn parse_realip(v: &Value) -> RawGeo {
    RawGeo::new(
        text(v, &["province"]),
        text(v, &["city"]),
        text(v, &["isp"]),
    )
}

fn parse_geolocation_db(v: &Value) -> RawGeo {
    RawGeo::new(text(v, &["state"]), text(v, &["city"]), "")
}

fn parse_ipapi_co(v: &Value) -> RawGeo {
    RawGeo::new(text(v, &["region"]), text(v, &["city"]), text(v, &["org"]))
}

fn parse_dbip_demo(v: &Value) -> RawGeo {
    let d = v.get("demoInfo").unwrap_or(&Value::Null);
    RawGeo::new(
        text(d, &["stateProv"]),
        text(d, &["city"]),
        text(d, &["isp"]),
    )
}

/* ------------------------------- 访问日志 ------------------------------- */

/// 记一条访问日志所需的全部上下文。
///
/// 收敛成结构体而不是 9 个位置参数：调用点散在门禁中间件与两个登录口，
/// 位置参数很容易把 `allowed` / `login_ok` 传反，而这两个字段都是 bool，
/// 编译器查不出来。
pub struct AccessLog<'a> {
    pub kind: &'a str,
    pub ip: &'a str,
    pub geo: &'a IpGeo,
    pub team: &'a str,
    pub path: &'a str,
    pub account: &'a str,
    pub allowed: bool,
    pub login_ok: bool,
}

/// 记一条访问日志：登录事件（kind=login）每次都写；普通接口访问 5 分钟内同 IP + 同路径去重。
pub async fn log_access(st: &ArcState, e: AccessLog<'_>) {
    let kind = e.kind;
    let ip = e.ip;
    let geo = e.geo;
    let team = e.team;
    let path = e.path;
    let login_ok = e.login_ok;
    let Some(pool) = st.writer.as_ref() else {
        return;
    };
    let Ok(parsed) = ip.trim().parse::<IpAddr>() else {
        return;
    };
    let packed = pack_ip(parsed);
    let account = truncate_name(e.account);
    let allowed = e.allowed;
    let result = if kind == "login" {
        sqlx::query(
            "INSERT INTO access_logs \
             (kind, ip, province, city, isp, geo_source, team, path, account_name, allowed, login_ok, dedup_key) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)",
        )
        .bind(kind)
        .bind(&packed)
        .bind(&geo.province)
        .bind(&geo.city)
        .bind(&geo.isp)
        .bind(&geo.source)
        .bind(team)
        .bind(path)
        .bind(&account)
        .bind(allowed as i8)
        .bind(login_ok as i8)
        .execute(pool)
        .await
    } else {
        /* 去重：dedup_key = ip|path|5 分钟桶，靠唯一键 + INSERT IGNORE 精确挡住并发
        重复请求（先 SELECT 再 INSERT 在并发下会放过一批，INSERT..SELECT 自查同表
        还会在多实例场景报 1213 deadlock）。窗口按 5 分钟桶对齐，不做滑动窗口。 */
        let bucket = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() / (ACCESS_DEDUP_MINUTES as u64 * 60))
            .unwrap_or(0);
        let dedup_key = format!("{ip}|{path}|{bucket}");
        sqlx::query(
            "INSERT IGNORE INTO access_logs \
             (kind, ip, province, city, isp, geo_source, team, path, account_name, allowed, login_ok, dedup_key) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?)",
        )
        .bind(kind)
        .bind(&packed)
        .bind(&geo.province)
        .bind(&geo.city)
        .bind(&geo.isp)
        .bind(&geo.source)
        .bind(team)
        .bind(path)
        .bind(&account)
        .bind(allowed as i8)
        .bind(&dedup_key)
        .execute(pool)
        .await
    };
    if let Err(e) = result {
        tracing::warn!(error = %e, ip = %ip, path = %path, "访问日志写入失败");
    }
}

/// 登录尝试（verify）专用：带账号与成败，每次都记
pub async fn log_login(st: &ArcState, ip: &str, geo: &IpGeo, team: &str, account: &str, ok: bool) {
    let path = format!("/api/public/teams/{team}/verify");
    log_login_path(st, ip, geo, team, &path, account, ok).await;
}

/// 同上，但日志里记真实请求路径（团别无关的登录口 /api/public/my/login 用）。
/// team 传账号最终归属的团；团别待定（管理员观看 / 还没绑主播）传空串。
pub async fn log_login_path(
    st: &ArcState,
    ip: &str,
    geo: &IpGeo,
    team: &str,
    path: &str,
    account: &str,
    ok: bool,
) {
    let allowed = gate_disabled() || geo.allowed;
    log_access(
        st,
        AccessLog {
            kind: "login",
            ip,
            geo,
            team,
            path,
            account,
            allowed,
            login_ok: ok,
        },
    )
    .await;
}

/* ------------------------------- 后台查询 ------------------------------- */

#[derive(serde::Deserialize)]
pub struct VisitsQuery {
    pub limit: Option<u32>,
    /// IP / 账号模糊搜索
    pub q: Option<String>,
    /// visit / api / login
    pub kind: Option<String>,
    /// "1" = 只看被拒的（外省）
    pub blocked: Option<String>,
}

/// GET /api/admin/visits —— 访客日志（管理端「访客日志」页）
pub async fn admin_visits(
    State(st): State<ArcState>,
    Query(q): Query<VisitsQuery>,
) -> Result<axum::Json<Value>, crate::routes::ApiError> {
    let Some(pool) = st.writer.as_ref() else {
        return Err(anyhow::anyhow!("写库不可用，取不到访客日志").into());
    };
    let limit = q.limit.unwrap_or(200).min(500) as i64;
    let mut sql = String::from(
        "SELECT id, kind, INET6_NTOA(ip) AS ip, province, city, isp, geo_source, team, path, \
         account_name, allowed, login_ok, \
         DATE_FORMAT(CONVERT_TZ(created_at, '+00:00', '+08:00'), '%Y-%m-%d %H:%i:%s') AS ts \
         FROM access_logs WHERE 1 = 1",
    );
    if let Some(kind) = q.kind.as_deref().filter(|s| !s.is_empty()) {
        if ["api", "login", "visit"].contains(&kind) {
            sql.push_str(" AND kind = ?");
        }
    }
    if q.blocked.as_deref() == Some("1") {
        sql.push_str(" AND allowed = 0");
    }
    let search =
        q.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| format!("%{s}%"));
    if search.is_some() {
        sql.push_str(" AND (INET6_NTOA(ip) LIKE ? OR account_name LIKE ?)");
    }
    sql.push_str(" ORDER BY id DESC LIMIT ?");

    let mut query = sqlx::query(&sql);
    if let Some(kind) = q.kind.as_deref().filter(|s| !s.is_empty()) {
        if ["api", "login", "visit"].contains(&kind) {
            query = query.bind(kind);
        }
    }
    if let Some(pat) = search.as_deref() {
        query = query.bind(pat).bind(pat);
    }
    query = query.bind(limit);
    let rows = query.fetch_all(pool).await?;

    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.try_get::<u64, _>(0).unwrap_or(0),
                "kind": r.try_get::<String, _>(1).unwrap_or_default(),
                "ip": r.try_get::<Option<String>, _>(2).unwrap_or_default().unwrap_or_default(),
                "province": r.try_get::<String, _>(3).unwrap_or_default(),
                "city": r.try_get::<String, _>(4).unwrap_or_default(),
                "isp": r.try_get::<String, _>(5).unwrap_or_default(),
                "source": r.try_get::<String, _>(6).unwrap_or_default(),
                "team": r.try_get::<String, _>(7).unwrap_or_default(),
                "path": r.try_get::<String, _>(8).unwrap_or_default(),
                "account": r.try_get::<String, _>(9).unwrap_or_default(),
                "allowed": r.try_get::<i8, _>(10).unwrap_or(0) == 1,
                "loginOk": r.try_get::<i8, _>(11).unwrap_or(0) == 1,
                "createdAt": r.try_get::<String, _>(12).unwrap_or_default(),
            })
        })
        .collect();

    /* sqlx 把连接会话时区固定成 +00:00（sqlx-mysql 默认），TIMESTAMP 列读出来是 UTC；
    访客日志给人看，统一换算成北京时间再判断「今天」。 */
    let stats = sqlx::query(
        "SELECT \
           CAST((SELECT COUNT(*) FROM access_logs) AS SIGNED) AS all_rows, \
           CAST(COUNT(*) AS SIGNED) AS today, \
           CAST(COUNT(DISTINCT ip) AS SIGNED) AS today_uniq, \
           CAST(COALESCE(SUM(allowed = 1), 0) AS SIGNED) AS today_allowed, \
           CAST(COALESCE(SUM(allowed = 0), 0) AS SIGNED) AS today_blocked, \
           CAST(COALESCE(SUM(kind = 'login'), 0) AS SIGNED) AS today_logins \
         FROM access_logs \
         WHERE DATE(CONVERT_TZ(created_at, '+00:00', '+08:00')) = \
               DATE(CONVERT_TZ(NOW(), '+00:00', '+08:00'))",
    )
    .fetch_one(pool)
    .await?;
    let stats = serde_json::json!({
        "all": stats.try_get::<i64, _>(0).unwrap_or(0),
        "today": stats.try_get::<i64, _>(1).unwrap_or(0),
        "todayUniq": stats.try_get::<i64, _>(2).unwrap_or(0),
        "todayAllowed": stats.try_get::<i64, _>(3).unwrap_or(0),
        "todayBlocked": stats.try_get::<i64, _>(4).unwrap_or(0),
        "todayLogins": stats.try_get::<i64, _>(5).unwrap_or(0),
    });

    Ok(axum::Json(serde_json::json!({
        "items": items,
        "stats": stats,
    })))
}

#[derive(serde::Deserialize)]
pub struct IpLookupQuery {
    ip: Option<String>,
}

/// `GET /api/admin/ip-lookup?ip=1.2.3.4` —— IP 归属地现场查询（诊断用）：
/// 走 `IP_QUERY_SOURCES` 集，实时打外部接口，**不读也不写** `ip_geo_cache`；
/// 每个源的成败都返回（ok=false 时三字段空串），合并结论平铺在顶层
/// （province / city / isp / source / allowed，判定口径与门禁完全一致）。
/// 内网 IP 直接返回「内网」且不打外部接口。
pub async fn admin_ip_lookup(
    Query(q): Query<IpLookupQuery>,
) -> Result<axum::Json<Value>, crate::routes::ApiError> {
    let raw = q.ip.unwrap_or_default();
    let parsed: IpAddr = raw
        .trim()
        .parse()
        .map_err(|_| crate::routes::ApiError::bad_request("IP 格式不对"))?;
    let ip = parsed.to_string();

    if is_internal(&parsed) {
        return Ok(axum::Json(serde_json::json!({
            "ip": ip,
            "province": "内网",
            "city": "",
            "isp": "",
            "source": "local",
            "allowed": false,
            "sources": [],
        })));
    }

    let probes = query_sources(sources_for(&ip, SourceSet::Query)).await;
    let mut raws: Vec<(&str, RawGeo)> = Vec::new();
    let mut sources = Vec::new();
    for (name, geo) in probes {
        match geo {
            Some(g) if !g.province.trim().is_empty() || !g.city.trim().is_empty() => {
                sources.push(serde_json::json!({
                    "name": name,
                    "ok": true,
                    "province": g.province,
                    "city": g.city,
                    "isp": g.isp,
                }));
                raws.push((name, RawGeo::new(&g.province, &g.city, &g.isp)));
            }
            _ => sources.push(serde_json::json!({
                "name": name,
                "ok": false,
                "province": "",
                "city": "",
                "isp": "",
            })),
        }
    }

    let merged = merge(&raws);
    Ok(axum::Json(serde_json::json!({
        "ip": ip,
        "province": merged.province,
        "city": merged.city,
        "isp": merged.isp,
        "source": merged.source,
        "allowed": merged.allowed,
        "sources": sources,
    })))
}

/* ------------------------------- 工具 ------------------------------- */

fn pack_ip(ip: IpAddr) -> Vec<u8> {
    match ip {
        IpAddr::V4(v4) => v4.octets().to_vec(),
        IpAddr::V6(v6) => v6.octets().to_vec(),
    }
}

/// 测试用：管理端列表的 IP 字符串走 SQL 的 INET6_NTOA，不经这里
#[allow(dead_code)]
fn unpack_ip(bytes: &[u8]) -> String {
    match bytes.len() {
        4 => Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]).to_string(),
        16 => {
            let octets: [u8; 16] = bytes.try_into().unwrap_or([0; 16]);
            Ipv6Addr::from(octets).to_string()
        }
        _ => String::new(),
    }
}

fn truncate_name(name: &str) -> String {
    name.trim().chars().take(MAX_LOG_NAME).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_chinese_and_english_provinces() {
        for (raw, want) in [
            ("河南省", "河南"),
            ("河南", "河南"),
            ("Henan", "河南"),
            ("Henan Sheng", "河南"),
            ("henan province", "河南"),
            ("安徽省", "安徽"),
            ("Anhui", "安徽"),
            ("Guangdong Province", "广东"),
            ("广东", "广东"),
            ("Beijing", "北京"),
            ("Inner Mongolia", "内蒙古"),
            ("", ""),
        ] {
            assert_eq!(normalize_province(raw), want, "raw = {raw}");
        }
    }

    #[test]
    fn extracts_province_from_baidu_location() {
        let g = split_location("河南省郑州市 联通");
        assert_eq!(g.province, "河南");
        assert_eq!(g.city, "郑州");
        assert_eq!(g.isp, "联通");
        let g = split_location("北京市 电信");
        assert_eq!(g.province, "北京");
        assert_eq!(g.city, "");
    }

    #[test]
    fn merge_prefers_allowed_province_and_majority() {
        let raws = vec![
            ("pconline", RawGeo::new("河南省", "郑州市", "联通")),
            ("baidu", RawGeo::new("河南", "郑州", "联通")),
            ("ip-api", RawGeo::new("北京市", "北京", "联通")),
            ("ip.sb", RawGeo::new("Henan", "Zhengzhou", "Unicom")),
        ];
        let geo = merge(&raws);
        assert_eq!(geo.province, "河南");
        assert!(geo.allowed);
        assert_eq!(geo.city, "郑州");
        assert!(geo.source.contains("pconline"));

        let raws = vec![
            ("pconline", RawGeo::new("广东省", "广州市", "电信")),
            ("ip.sb", RawGeo::new("Guangdong", "Guangzhou", "Telecom")),
        ];
        let geo = merge(&raws);
        assert_eq!(geo.province, "广东");
        assert!(!geo.allowed);

        assert!(!merge(&[]).allowed);
        assert_eq!(merge(&[]).province, "");

        let raws = vec![("ipwhois", RawGeo::new("Anhui", "Hefei", ""))];
        let geo = merge(&raws);
        assert_eq!(geo.province, "安徽");
        assert!(geo.allowed);
    }

    #[test]
    fn parses_known_api_shapes() {
        let p = parse_pconline(&serde_json::json!({
            "ip": "123.15.57.1", "pro": "河南省", "city": "郑州市", "addr": "河南省郑州市 联通", "err": ""
        }));
        assert_eq!(
            (p.province.as_str(), p.city.as_str(), p.isp.as_str()),
            ("河南省", "郑州市", "联通")
        );

        let b = parse_baidu(&serde_json::json!({
            "data": [{ "location": "安徽省合肥市 电信" }]
        }));
        assert_eq!(
            (b.province.as_str(), b.city.as_str(), b.isp.as_str()),
            ("安徽", "合肥", "电信")
        );

        let i = parse_ip9(&serde_json::json!({
            "ret": 200, "data": { "prov": "河南", "city": "郑州", "isp": "中国联通" }
        }));
        assert_eq!(
            (i.province.as_str(), i.city.as_str(), i.isp.as_str()),
            ("河南", "郑州", "中国联通")
        );

        let s = parse_ip_sb(
            &serde_json::json!({ "region": "Henan", "city": "Zhengzhou", "isp": "China Unicom" }),
        );
        assert_eq!(s.province, "Henan");

        let w = parse_ipwhois(
            &serde_json::json!({ "success": true, "region": "Henan Sheng", "city": "Zhengzhou", "isp": "Unicom" }),
        );
        assert_eq!(normalize_province(&w.province), "河南");

        let d = parse_db_ip(&serde_json::json!({ "stateProv": "Anhui", "city": "Hefei" }));
        assert_eq!(d.province, "Anhui");

        let n = parse_ncgy(&serde_json::json!({
            "city": { "names": { "zh-CN": "郑州" } },
            "subdivisions": [{ "names": { "zh-CN": "河南" } }]
        }));
        assert_eq!((n.province.as_str(), n.city.as_str()), ("河南", "郑州"));

        let t = parse_ipwtf(&serde_json::json!({
            "ipv4": { "regionName": "Henan", "cityName": "Zhengzhou", "as": "Unicom" }
        }));
        assert_eq!(
            (t.province.as_str(), t.city.as_str(), t.isp.as_str()),
            ("Henan", "Zhengzhou", "Unicom")
        );

        let q = parse_ipquery(&serde_json::json!({
            "location": { "state": "Henan", "city": "Zhengzhou" },
            "isp": { "isp": "China Unicom" }
        }));
        assert_eq!(
            (q.province.as_str(), q.isp.as_str()),
            ("Henan", "China Unicom")
        );

        // 限流 / 失效响应不能当成省份
        assert!(parse_dbip_demo(&serde_json::json!({
            "status": "ok", "demoInfo": { "errorCode": "OVER_QUERY_LIMIT" }
        }))
        .province
        .is_empty());
        assert!(parse_ipwhois(&serde_json::json!({ "success": false }))
            .province
            .is_empty());
    }

    #[test]
    fn source_sets_use_documented_defaults() {
        for (set, want) in [
            (SourceSet::Gate, vec!["pconline", "baidu", "ip9"]),
            (SourceSet::Query, vec!["ip.sb", "ip2location", "ipwhois"]),
        ] {
            assert_eq!(source_names_for(set, None), want, "set = {set:?}");
            assert_eq!(source_names_for(set, Some("")), want, "空串应回默认");
            assert_eq!(source_names_for(set, Some("   ")), want);
            assert_eq!(
                source_names_for(set, Some("foo,bar")),
                want,
                "全未知名字应回默认"
            );
        }
    }

    #[test]
    fn source_names_trim_dedupe_and_ignore_unknown() {
        assert_eq!(
            source_names_for(SourceSet::Gate, Some("baidu, unknown ,ip9,baidu")),
            vec!["baidu", "ip9"]
        );
        assert_eq!(
            source_names_for(SourceSet::Query, Some("IP.SB, db-ip-demo, ip2location ")),
            vec!["ip.sb", "db-ip-demo", "ip2location"],
            "大小写不敏感，写回规范名，保留输入顺序"
        );
    }

    #[test]
    fn sources_named_filters_the_table_preserving_table_order() {
        assert_eq!(ALL_SOURCE_NAMES.len(), 19);
        assert_eq!(all_sources("1.2.3.4").len(), 19);
        let got: Vec<&str> = sources_named("1.2.3.4", &["ipwhois", "ip.sb", "nope"])
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(got, vec!["ip.sb", "ipwhois"], "输出保持源表顺序");
        let only = sources_named("1.2.3.4", &["pconline"]);
        assert_eq!(only.len(), 1);
        assert!(only[0].url.contains("1.2.3.4"));
        assert!(only[0].gbk, "pconline 是 GBK 源，过滤后属性不能丢");
    }

    /// 内网 IP 不打外部接口（离线也应稳定返回「内网」）；非法 IP 在 Live 实测里校验 400 响应体。
    #[tokio::test]
    async fn ip_lookup_internal_short_circuits() {
        let ok = match admin_ip_lookup(Query(IpLookupQuery {
            ip: Some("127.0.0.1".into()),
        }))
        .await
        {
            Ok(v) => v,
            Err(_) => panic!("内网 IP 应返回结果"),
        };
        assert_eq!(ok.0["province"], "内网");
        assert_eq!(ok.0["allowed"], false);
        assert_eq!(ok.0["sources"].as_array().map(Vec::len), Some(0));

        let err = admin_ip_lookup(Query(IpLookupQuery {
            ip: Some("abc".into()),
        }))
        .await;
        assert!(err.is_err(), "非法 IP 应报错");
    }

    #[test]
    fn request_ip_prefers_forwarded_headers_only_from_internal() {
        use axum::http::HeaderMap;
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "1.2.3.4, 10.0.0.1".parse().unwrap());
        let local: SocketAddr = "127.0.0.1:5555".parse().unwrap();
        assert_eq!(request_ip(local, &h), "1.2.3.4");
        let public: SocketAddr = "8.8.8.8:5555".parse().unwrap();
        assert_eq!(request_ip(public, &h), "8.8.8.8");

        let mut h = HeaderMap::new();
        h.insert("cf-connecting-ip", "223.5.5.5".parse().unwrap());
        assert_eq!(request_ip(local, &h), "223.5.5.5");
    }

    #[test]
    fn packs_and_unpacks_ips() {
        let v4: IpAddr = "123.15.57.1".parse().unwrap();
        assert_eq!(unpack_ip(&pack_ip(v4)), "123.15.57.1");
        let v6: IpAddr = "2408:8221:7917::1".parse().unwrap();
        assert_eq!(unpack_ip(&pack_ip(v6)), "2408:8221:7917::1");
    }

    #[test]
    fn team_and_account_from_request() {
        assert_eq!(team_from_path("/api/public/teams/male/daily"), "male");
        assert_eq!(team_from_path("/api/public/plans"), "");
        assert_eq!(account_from_query("a=1&token=unknown"), "");
    }

    /// 现场检查（默认跳过，会发真实网络请求）：对 `IPCHECK`（逗号分隔）里的每个 IP
    /// 逐源打印返回与合并结论——默认打 `IP_QUERY_SOURCES` 集（同后台 ip-lookup 接口），
    /// `IPCHECK_ALL=1` 时打全部 19 个源。用来核对门禁判定与上游接口的真实结果是否一致
    /// （数据源 / 解析 / 合并都复用生产同一套代码）。
    ///
    ///     IPCHECK=42.239.96.128,123.15.57.1 \
    ///       cargo test -p pk-server --lib -- --ignored --nocapture live_ip_lookup
    #[tokio::test]
    #[ignore]
    async fn live_ip_lookup() {
        let ips = std::env::var("IPCHECK")
            .unwrap_or_else(|_| "123.15.57.1,218.22.0.1,121.8.215.106".into());
        let all = matches!(
            std::env::var("IPCHECK_ALL").as_deref(),
            Ok("1") | Ok("true") | Ok("yes")
        );
        if all {
            println!("数据源集合：all（IPCHECK_ALL=1，全部 19 个源）");
        } else {
            println!(
                "数据源集合：{}（{}）",
                SourceSet::Query.label(),
                source_names(SourceSet::Query).join(",")
            );
        }
        let client = reqwest::Client::builder()
            .timeout(LOOKUP_TIMEOUT)
            .user_agent("pk-server-ip-gate/1.0 (check)")
            .build()
            .expect("build client");
        for ip in ips.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let parsed: IpAddr = ip.parse().expect("IPCHECK 里必须是合法 IP");
            println!("\n==== {ip} ====");
            let sources = if all {
                all_sources(&parsed.to_string())
            } else {
                sources_for(&parsed.to_string(), SourceSet::Query)
            };
            let futures = sources.into_iter().map(|s| {
                let client = client.clone();
                async move {
                    let result = match client.get(&s.url).send().await {
                        Ok(resp) if resp.status().is_success() => {
                            resp.bytes().await.ok().and_then(|bytes| {
                                let text = if s.gbk {
                                    encoding_rs::GBK.decode(&bytes).0.into_owned()
                                } else {
                                    String::from_utf8_lossy(&bytes).into_owned()
                                };
                                serde_json::from_str::<Value>(text.trim())
                                    .ok()
                                    .map(|v| (s.parse)(&v))
                            })
                        }
                        Ok(resp) => {
                            println!("  {:<14} HTTP {}", s.name, resp.status());
                            None
                        }
                        Err(e) => {
                            println!("  {:<14} 请求失败：{e}", s.name);
                            None
                        }
                    };
                    (s.name, result)
                }
            });
            let results = join_all(futures).await;
            let mut raws: Vec<(&str, RawGeo)> = Vec::new();
            for (name, geo) in &results {
                match geo {
                    Some(g) if !g.province.is_empty() || !g.city.is_empty() => {
                        println!("  {:<14} {:<10} {:<8} {}", name, g.province, g.city, g.isp);
                        raws.push((name, RawGeo::new(&g.province, &g.city, &g.isp)));
                    }
                    Some(_) => println!("  {name:<14} （返回了但没解析出省份/城市）"),
                    None => {}
                }
            }
            let merged = merge(&raws);
            println!(
                "  => 合并：{} {}（source={}） 放行={}",
                merged.province, merged.city, merged.source, merged.allowed
            );
        }
    }
}
