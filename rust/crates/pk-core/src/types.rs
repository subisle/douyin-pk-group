use serde::{Deserialize, Serialize};

use crate::constraints::ConstraintSet;

/// 音浪口径：lastN = 近 N 天增量；total = 累计总音浪（含基线日快照）；
/// month = 最新数据所在自然月的增量合计（本月累计）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WaveMetric {
    LastN,
    /// 最近 1 个有效业务日（等价于 lastN + days=1）
    Last1,
    /// 本月累计
    Month,
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
/// （`Eq` 去掉了：`score` 是 f64，只有 PartialEq）
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedRow {
    pub raw: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<String>,
    /// 命中了别名表时的映射结果，如 阿楠 → 南方楠
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias_applied: Option<String>,
    /// 名单自带的分数（`狼哲 71259` 这种「名字 + 分数」格式）。
    /// 有值时直接当音浪用、不去查库——晋级赛名单就是这么发的。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
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
    /// 梯队式 · 近 N 天口径：按最近 N 天音浪合计降序依次直填。
    /// 与 Sequential 的算法完全一致（都按 `wave` 降序切块），
    /// 区别只在语义：调用方用这个变体时，`wave` 必须用 lastN 口径填充。
    #[serde(rename = "sequential-last-n")]
    SequentialLastN,
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
