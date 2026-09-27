use serde::{Deserialize, Serialize};

use crate::constraints::ConstraintSet;

/// 音浪口径：lastN = 近 N 天增量；total = 累计总音浪（含基线日快照）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WaveMetric {
    LastN,
    #[default]
    Total,
}

/// 一个参赛主播。key 全局唯一（`名字#序号`），分组算法全程只认 key
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Player {
    pub key: String,
    #[serde(default)]
    pub raw: String,
    pub name: String,
    /// 队长/师傅，只用于同门回避，不参赛
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<String>,
    /// 库里的主播 id。不同数据源主键类型不同，统一按字符串处理
    #[serde(rename = "personId", default)]
    pub person_id: Option<String>,
    #[serde(default)]
    pub matched: bool,
    #[serde(default)]
    pub wave: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(default)]
    pub excluded: bool,
}

impl Player {
    pub fn new(key: impl Into<String>, name: impl Into<String>, wave: f64) -> Self {
        Self {
            key: key.into(),
            raw: String::new(),
            name: name.into(),
            leader: None,
            person_id: None,
            matched: true,
            wave,
            avatar: None,
            excluded: false,
        }
    }

    pub fn with_leader(mut self, leader: impl Into<String>) -> Self {
        self.leader = Some(leader.into());
        self
    }

    pub fn leader_name(&self) -> &str {
        self.leader.as_deref().map(str::trim).unwrap_or("")
    }
}

/// 接龙名单解析出来的一行
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedRow {
    pub raw: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<String>,
    /// 命中了别名表时的映射结果，如 阿楠 → 南方楠
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias_applied: Option<String>,
    #[serde(default)]
    pub excluded: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupStat {
    pub index: usize,
    pub count: usize,
    pub total: f64,
    pub avg: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceStat {
    pub groups: Vec<GroupStat>,
    pub max: f64,
    pub min: f64,
    pub mean: i64,
    /// 极差百分比，保留一位小数
    pub spread: f64,
    pub std_dev: i64,
}

/// 分组策略
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
    /// 蛇形均衡（默认）
    #[default]
    Snake,
    /// 随机
    Random,
    /// 蛇形 + 同门回避
    SnakeNoMaster,
    /// 梯队式（高到低切组，不做均衡）
    Sequential,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupOptions {
    pub group_size: usize,
    pub strategy: Strategy,
    /// 内置条件：互斥 / 固定位 / 开赛时间。缺省用 DEFAULT_CONSTRAINTS
    #[serde(default)]
    pub constraints: Option<ConstraintSet>,
}

impl GroupOptions {
    pub fn new(group_size: usize, strategy: Strategy) -> Self {
        Self {
            group_size,
            strategy,
            constraints: None,
        }
    }
}
