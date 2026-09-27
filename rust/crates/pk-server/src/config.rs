//! 运行配置：数据源 A/B（都来自 .env，密钥不进仓库）

use anyhow::{anyhow, Result};

#[derive(Clone, Debug)]
pub struct SourceConfig {
    pub id: String,
    pub label: String,
    pub note: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
    pub pool_limit: u32,
    /// 首日是累计快照，统计时是否剔除（只对增量口径有意义）
    pub exclude_baseline: bool,
}

impl SourceConfig {
    /// 拼 sqlx 连接串（密码做 percent-encode）
    pub fn url(&self) -> String {
        format!(
            "mysql://{}:{}@{}:{}/{}",
            urlencode(&self.user),
            urlencode(&self.password),
            self.host,
            self.port,
            self.database
        )
    }

    pub fn public(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "label": self.label,
            "note": self.note,
            "port": self.port,
            "database": self.database,
            "poolLimit": self.pool_limit,
            "excludeBaselineDay": self.exclude_baseline,
        })
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 极简 .env 解析：容忍值里的中文与空格（`主库 · douyinxs`），
/// dotenvy 0.15 在这类值上报错，所以自己来。已存在的环境变量优先，不覆盖。
pub fn load_dotenv(path: &str) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|x| x.strip_suffix('"'))
            .unwrap_or(v);
        let v = v
            .strip_prefix('\'')
            .and_then(|x| x.strip_suffix('\''))
            .unwrap_or(v);
        if std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
        }
    }
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn env_or(key: &str, default: &str) -> String {
    env(key).unwrap_or_else(|| default.to_string())
}

fn read_source(id: &str) -> Result<SourceConfig> {
    let host = match env(&format!("DB_{id}_HOST")) {
        Some(h) => h,
        // 旧变量兜底：DB_HOST 只映射到 A
        None if id == "A" => env("DB_HOST").ok_or_else(|| {
            anyhow!("没有配置任何数据源：请设置 DB_A_HOST（DB_HOST 等旧变量也识别）")
        })?,
        None => return Err(anyhow!("数据源 {id} 未配置 DB_{id}_HOST")),
    };
    Ok(SourceConfig {
        id: id.to_string(),
        label: env_or(&format!("DB_{id}_LABEL"), &format!("数据源 {id}")),
        note: env_or(&format!("DB_{id}_NOTE"), ""),
        host,
        port: env_or(&format!("DB_{id}_PORT"), "3306")
            .parse()
            .unwrap_or(3306),
        user: env_or(&format!("DB_{id}_USER"), ""),
        password: env(&format!("DB_{id}_PASSWORD"))
            .or_else(|| if id == "A" { env("DB_PASSWORD") } else { None })
            .unwrap_or_default(),
        database: env_or(&format!("DB_{id}_NAME"), ""),
        pool_limit: env_or(&format!("DB_{id}_POOL_LIMIT"), "3")
            .parse()
            .unwrap_or(3),
        exclude_baseline: env(&format!("DB_{id}_EXCLUDE_BASELINE")).as_deref() != Some("0"),
    })
}

/// 读出全部数据源；A 缺失直接报错，B 缺失就跳过
pub fn load_sources() -> Result<Vec<SourceConfig>> {
    let mut list: Vec<SourceConfig> = ["A", "B"]
        .iter()
        .filter_map(|id| read_source(id).ok())
        .collect();
    // 去重（DB_HOST 兜底可能让 A/B 指向同一个库）
    let mut seen = std::collections::HashSet::new();
    list.retain(|s| seen.insert(s.id.clone()));
    if list.is_empty() {
        return Err(anyhow!(
            "没有配置任何数据源：请设置 DB_A_* （或 DB_HOST 等旧变量）"
        ));
    }
    Ok(list)
}

pub fn sqlite_path() -> String {
    env_or("SQLITE_PATH", "./data/app.db")
}

pub fn wave_cache_ttl_secs() -> u64 {
    env_or("WAVE_CACHE_TTL", "600").parse().unwrap_or(600)
}
