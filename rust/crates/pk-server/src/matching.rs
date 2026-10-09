//! 名单解析 + 主播匹配 + 音浪回填

use std::collections::HashMap;

use pk_core::{default_aliases, default_exclude, normalize_name, parse_roster, ParsedRow, Player};
use serde::Serialize;

use crate::repos;
use crate::state::{AnchorBrief, AppState};

#[derive(Serialize, Clone, Debug)]
pub struct UnmatchedItem {
    pub row: ParsedRow,
    pub candidates: Vec<AnchorBrief>,
}

#[derive(Serialize, Clone, Debug)]
pub struct MatchPayload {
    pub players: Vec<Player>,
    pub unmatched: Vec<UnmatchedItem>,
    pub excluded: Vec<ParsedRow>,
    #[serde(rename = "aliasMap")]
    pub alias_map: HashMap<String, String>,
    pub dates: Vec<String>,
    pub metric: String,
    pub days: i64,
    /// 名单里带分数的行数 > 0 说明音浪来自名单（晋级赛名单格式），前端据此换口径文案
    pub scored: i64,
}

pub struct MatchOptions<'a> {
    pub text: &'a str,
    pub metric: &'a str,
    pub days: i64,
    pub aliases: Option<HashMap<String, String>>,
    pub exclude: Option<Vec<String>>,
    pub force: bool,
}

fn uniq_candidates(name: &str, anchors: &[AnchorBrief]) -> Vec<AnchorBrief> {
    let key = normalize_name(name);
    let exact: Vec<AnchorBrief> = anchors.iter().filter(|a| a.name == name).cloned().collect();
    if !exact.is_empty() {
        return exact.into_iter().take(10).collect();
    }
    let partial: Vec<AnchorBrief> = anchors
        .iter()
        .filter(|a| {
            normalize_name(&a.name).contains(&key) || key.contains(&normalize_name(&a.name))
        })
        .cloned()
        .collect();
    if !partial.is_empty() {
        return partial.into_iter().take(10).collect();
    }
    // 末字或首字相同的近似候选
    anchors
        .iter()
        .filter(|a| {
            let n = normalize_name(&a.name);
            let n = n.chars().collect::<Vec<_>>();
            let k = key.chars().collect::<Vec<_>>();
            n.len() > 1 && k.len() > 1 && (n.last() == k.last() || n.first() == k.first())
        })
        .take(10)
        .cloned()
        .collect()
}

pub async fn match_roster(st: &AppState, opts: MatchOptions<'_>) -> anyhow::Result<MatchPayload> {
    let exclude = opts.exclude.unwrap_or_else(default_exclude);
    let mut alias_map = default_aliases();
    alias_map.extend(crate::store::list_aliases(st).await);
    if let Some(a) = opts.aliases {
        alias_map.extend(a);
    }

    let anchors = repos::load_anchors(st, None, opts.force).await?;
    let rows = parse_roster(
        opts.text,
        &pk_core::ParseOptions {
            aliases: Some(alias_map.clone()),
            exclude,
        },
    );

    let mut by_name: HashMap<&str, &AnchorBrief> = HashMap::new();
    let mut by_norm: HashMap<String, Vec<&AnchorBrief>> = HashMap::new();
    for a in &anchors {
        by_name.entry(a.name.as_str()).or_insert(a);
        by_norm.entry(normalize_name(&a.name)).or_default().push(a);
    }

    let mut players: Vec<Player> = Vec::new();
    // 该行的音浪是否来自「名单自带分数」（是的话不要被库里的音浪覆盖）
    let mut from_roster: Vec<bool> = Vec::new();
    let mut unmatched: Vec<UnmatchedItem> = Vec::new();
    let mut excluded: Vec<ParsedRow> = Vec::new();

    for (i, row) in rows.into_iter().enumerate() {
        if row.excluded {
            excluded.push(row);
            continue;
        }
        let hit = by_name.get(row.name.as_str()).copied().or_else(|| {
            by_norm
                .get(&normalize_name(&row.name))
                .and_then(|v| v.first().copied())
        });
        let key = format!("{}#{}", row.name, i);
        let scored = row.score.is_some();
        match hit {
            Some(h) => players.push(Player {
                key,
                raw: row.raw,
                name: row.name,
                leader: row.leader,
                person_id: Some(h.person_id.clone()),
                matched: true,
                wave: row.score.unwrap_or(0.0),
                avatar: h.avatar.clone(),
                excluded: false,
            }),
            None => {
                let candidates = uniq_candidates(&row.name, &anchors);
                players.push(Player {
                    key,
                    raw: row.raw.clone(),
                    name: row.name.clone(),
                    leader: row.leader.clone(),
                    person_id: None,
                    matched: false,
                    wave: row.score.unwrap_or(0.0),
                    avatar: None,
                    excluded: false,
                });
                unmatched.push(UnmatchedItem { row, candidates });
            }
        }
        from_roster.push(scored);
    }

    let wave_info = repos::get_waves(st, opts.metric, opts.days, None, opts.force, None).await?;
    for (i, p) in players.iter_mut().enumerate() {
        // 名单自带分数的行保持原值（晋级赛名单就是这么发的，库里没有这个数）
        if from_roster.get(i).copied().unwrap_or(false) {
            continue;
        }
        p.wave = p
            .person_id
            .as_ref()
            .and_then(|id| wave_info.waves.get(id))
            .copied()
            .unwrap_or(0.0);
    }

    Ok(MatchPayload {
        players,
        unmatched,
        excluded,
        alias_map,
        dates: wave_info.dates,
        metric: opts.metric.to_string(),
        days: opts.days,
        scored: from_roster.iter().filter(|x| **x).count() as i64,
    })
}
