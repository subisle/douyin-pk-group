//! 全局状态：数据源、连接池、SQLite、查询缓存

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::auth::Auth;
use crate::config::SourceConfig;
use crate::mysql::Pools;

pub struct Cached<T> {
    pub at: Instant,
    pub value: T,
}

impl<T> Cached<T> {
    pub fn fresh(&self, ttl: Duration) -> Option<&T> {
        (self.at.elapsed() < ttl).then_some(&self.value)
    }
    pub fn new(value: T) -> Self {
        Self {
            at: Instant::now(),
            value,
        }
    }
}

#[derive(Default)]
pub struct WaveCache {
    /// 每个数据源一套日期信息（含基线日标记）
    pub dates: HashMap<String, Cached<DateInfo>>,
    /// key = `{source}:{metric}:{days}`
    pub waves: HashMap<String, Cached<HashMap<String, f64>>>,
    /// key = source id
    pub anchors: HashMap<String, Cached<Vec<AnchorBrief>>>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DateInfo {
    /// 全部采集日期，倒序；含基线日（它是累计基数，total 口径必须计入）
    pub dates: Vec<String>,
    /// 基线日 = 最早那个采集日（限 A 库）。那天 wave 记的是累计音浪而非当日增量
    pub baseline: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AnchorBrief {
    #[serde(rename = "personId")]
    pub person_id: String,
    pub name: String,
    pub avatar: Option<String>,
}

pub struct AppState {
    /// 管理员鉴权（口令 -> 内存 token）
    pub auth: std::sync::Arc<Auth>,
    pub sources: Vec<SourceConfig>,
    pub pools: Pools,
    /// 当前数据源 id（切换后缓存整体失效）
    pub active: Mutex<String>,
    pub cache: Mutex<WaveCache>,
    pub wave_ttl: Duration,
}

impl AppState {
    pub fn active_id(&self) -> String {
        self.active.lock().expect("active 锁 poisoned").clone()
    }

    pub fn set_active(&self, id: &str) -> anyhow::Result<()> {
        if !self.sources.iter().any(|s| s.id == id) {
            anyhow::bail!("未知数据源：{id}");
        }
        *self.active.lock().expect("active 锁 poisoned") = id.to_string();
        self.clear_cache();
        Ok(())
    }

    pub fn source(&self, id: Option<&str>) -> anyhow::Result<&SourceConfig> {
        let id = id.map(str::to_string).unwrap_or_else(|| self.active_id());
        self.sources
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| anyhow::anyhow!("未知数据源：{id}"))
    }

    pub fn clear_cache(&self) {
        let mut c = self.cache.lock().expect("cache 锁 poisoned");
        c.waves.clear();
        c.dates.clear();
        c.anchors.clear();
    }
}

/// 音浪缓存的对外结构
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct WaveInfo {
    pub metric: String,
    pub days: i64,
    pub dates: Vec<String>,
    pub waves: HashMap<String, f64>,
}

pub type WaveMap = HashMap<String, f64>;
