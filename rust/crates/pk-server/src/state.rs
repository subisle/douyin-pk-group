//! 全局状态：数据源（LOCAL / A）、连接池、写池、查询缓存

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use std::sync::Arc;

use crate::auth::Auth;
use crate::config::SourceConfig;
use crate::pgrs::Pools;

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
    /// key = `{source}:{days}`，展示页曲线图用的每日增量序列
    pub series: HashMap<String, Cached<WaveSeries>>,
}

/// 每日音浪序列（展示页每日排名 / 曲线数据源）。
/// `dates` 正序（老 → 新），`series` 里每个人一条与 dates 等长、下标一一对应的数组；
/// 某天没数据就补 0，这样前端可以直接按下标画折线，不用再对齐日期。
/// `live` 与 series 同构，1 = 当天开播、0 = 未开播（源里没有该字段时恒为 0）。
#[derive(Clone, Debug, Default)]
pub struct WaveSeries {
    pub dates: Vec<String>,
    pub series: HashMap<String, Vec<f64>>,
    pub live: HashMap<String, Vec<i16>>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DateInfo {
    /// 全部采集日期，倒序；含基线日（它是累计基数，total 口径必须计入）
    pub dates: Vec<String>,
    /// 基线日 = 最早那个采集日。那天 wave 记的是累计音浪而非当日增量
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
    /// douyin_writer 业务写池（B 阶段：导入 / 主播台账写路径）。
    /// None = 写库账号未配置或连不上：管理写接口返回 503，只读功能照常。
    pub writer: Option<sqlx::PgPool>,
    /// Redis 缓存 / 锁池（P0，docs/17 §7.3）。
    /// None = `REDIS_URL` 未配置或建池失败：缓存层整体降级（`store::cache` 一律返回
    /// None/false，调用方回落内存缓存 + 直查 PG），行为与引入 Redis 之前一致。
    pub redis: Option<crate::store::cache::RedisPool>,
    /// 当前数据源 id（切换后缓存整体失效）
    pub active: Mutex<String>,
    pub cache: Mutex<WaveCache>,
    pub wave_ttl: Duration,
    /// 机器人模块（E 阶段）。AppState 先建，BotManager 后挂（它反向持有 AppState）。
    pub bots: std::sync::OnceLock<Arc<crate::bot::BotManager>>,
    /// WorkBuddy 网关客户端（2026-10-10 手机号注册用）：没配 WORKBUDDY_PANEL_PASSWORD 就是空的，
    /// 注册页据此隐藏「添加账号 (OAuth)」步骤，其余注册照常。
    pub wb: std::sync::OnceLock<Arc<crate::wbgateway::WbGateway>>,
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
        c.series.clear();
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
