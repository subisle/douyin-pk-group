//! 小智 AI MCP 接入（2026-10-24）。
//!
//! 小智云端提供一个出站 WebSocket MCP endpoint：
//! `wss://api.xiaozhi.me/mcp/?token=...`。本模块作为 MCP Server 连接该地址，
//! 接收 MCP JSON-RPC 请求并返回工具结果。只暴露管理员只读数据，不接入用户账号。
//!
//! 该 endpoint 的 token 只从环境变量 `XIAOZHI_MCP_ENDPOINT` 读取，不写仓库、
//! 不打印到日志。连接断开后指数退避重连。

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sqlx::Row as _;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::dialect;
use crate::report;
use crate::repos;
use crate::state::AppState;

const PROTOCOL_VERSION: &str = "2024-11-05";
const MAX_ROWS: usize = 50;

/// 启动小智 MCP 出站连接。未配置 endpoint 时安静跳过（本地开发不需要小智）。
pub fn spawn_xiaozhi(st: Arc<AppState>) {
    let Some(endpoint) = std::env::var("XIAOZHI_MCP_ENDPOINT")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    else {
        return;
    };

    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);
        loop {
            match run_connection(&st, &endpoint).await {
                Ok(()) => backoff = Duration::from_secs(1),
                // 不打印 e：底层 WebSocket 错误可能包含带 token 的 endpoint。
                Err(_) => tracing::warn!("小智 MCP 连接失败或断开，稍后重连"),
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    });
}

async fn run_connection(st: &Arc<AppState>, endpoint: &str) -> anyhow::Result<()> {
    tracing::info!("小智 MCP 正在连接");
    let (ws, _) = connect_async(endpoint).await?;
    tracing::info!("小智 MCP 已连接");
    let (mut write, mut read) = ws.split();

    while let Some(frame) = read.next().await {
        let frame = frame?;
        let text = match frame {
            Message::Text(text) => text.to_string(),
            Message::Binary(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Message::Ping(payload) => {
                write.send(Message::Pong(payload)).await?;
                continue;
            }
            Message::Pong(_) | Message::Close(_) | Message::Frame(_) => continue,
        };
        if let Some(response) = handle_message(st, &text).await {
            write.send(Message::Text(response.to_string())).await?;
        }
    }
    Ok(())
}

async fn handle_message(st: &Arc<AppState>, text: &str) -> Option<Value> {
    let request: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Some(rpc_error(Value::Null, -32700, "JSON 解析失败")),
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();

    if method.is_empty() || method.starts_with("notifications/") {
        return None;
    }
    tracing::info!(method, "小智 MCP 请求");

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": request
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(PROTOCOL_VERSION),
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "douyin-pk-mcp",
                "version": "1.0.0"
            }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_defs() })),
        "tools/call" => match call_tool(st, &request).await {
            Ok(value) => Ok(value),
            Err(e) => Ok(json!({
                "content": [{ "type": "text", "text": format!("工具执行失败：{e}") }],
                "isError": true
            })),
        },
        _ => Err(format!("不支持的方法：{method}")),
    };

    Some(match result {
        Ok(result) => rpc_ok(id, result),
        Err(message) => rpc_error(id, -32601, &message),
    })
}

async fn call_tool(st: &Arc<AppState>, request: &Value) -> anyhow::Result<Value> {
    let name = request
        .pointer("/params/name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("缺少工具名称"))?;
    let args = request
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let value = match name {
        "get_team_ranking" => get_team_ranking(st, &args).await,
        "get_team_summary" => get_team_summary(st, &args).await,
        "search_anchor" => search_anchor(st, &args).await,
        "get_anchor_rank" => get_anchor_rank(st, &args).await,
        "get_daily_briefing" => get_daily_briefing(st, &args).await,
        "get_data_status" => get_data_status(st).await,
        _ => Err(anyhow::anyhow!("未知工具：{name}")),
    }?;

    let text = serde_json::to_string(&value).unwrap_or_else(|_| "{}".into());
    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false
    }))
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

fn tool_defs() -> Vec<Value> {
    vec![
        json!({
            "name": "get_team_ranking",
            "description": "查询男团或女团的日榜、月榜、年榜。返回排名、艺名、音浪、时长、未播天数和开播状态。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "team": { "type": "string", "enum": ["male", "female"], "description": "male=男团，female=女团" },
                    "period": { "type": "string", "enum": ["daily", "monthly", "yearly"], "description": "daily=日榜，monthly=月榜，yearly=年榜" },
                    "date": { "type": "string", "description": "日榜日期，格式 YYYY-MM-DD；缺省为最新业务日" },
                    "month": { "type": "string", "description": "月榜月份，格式 YYYY-MM；缺省为最新业务日所在月" },
                    "year": { "type": "string", "description": "年榜年份，格式 YYYY；缺省为最新业务日所在年" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "返回行数，默认 10" }
                },
                "required": ["team", "period"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "get_team_summary",
            "description": "查询男团或女团某个周期的汇总数据：总音浪、开播人数、未播人数和榜单前三。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "team": { "type": "string", "enum": ["male", "female"] },
                    "period": { "type": "string", "enum": ["daily", "monthly", "yearly"] },
                    "date": { "type": "string" },
                    "month": { "type": "string" },
                    "year": { "type": "string" }
                },
                "required": ["team", "period"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "search_anchor",
            "description": "按艺名或部分艺名搜索对外展示的主播，返回姓名和所属团。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "艺名或艺名片段" },
                    "team": { "type": "string", "enum": ["male", "female"], "description": "可选，限定团别" }
                },
                "required": ["query"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "get_anchor_rank",
            "description": "查询某位对外展示主播在指定周期的团内排名、音浪、时长和未播天数。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "主播艺名" },
                    "period": { "type": "string", "enum": ["daily", "monthly", "yearly"] },
                    "date": { "type": "string" },
                    "month": { "type": "string" },
                    "year": { "type": "string" }
                },
                "required": ["name", "period"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "get_daily_briefing",
            "description": "生成每日播报：最新业务日男团或女团的 TOP5、总音浪、开播和未播人数。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "team": { "type": "string", "enum": ["male", "female"], "description": "缺省为男团" },
                    "date": { "type": "string", "description": "YYYY-MM-DD，缺省为最新业务日" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "get_data_status",
            "description": "查询最新业务日、可用日期数量以及男团/女团对外展示人数。",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        }),
    ]
}

#[derive(Debug, Clone)]
struct RankRow {
    name: String,
    wave: i64,
    minutes: i64,
    absent_days: i64,
}

fn team_arg(args: &Value) -> anyhow::Result<&'static str> {
    match args.get("team").and_then(Value::as_str).unwrap_or("male") {
        "male" => Ok("male"),
        "female" => Ok("female"),
        other => Err(anyhow::anyhow!("team 只能是 male 或 female，收到 {other}")),
    }
}

fn period_arg(args: &Value) -> anyhow::Result<&str> {
    match args.get("period").and_then(Value::as_str) {
        Some("daily") => Ok("daily"),
        Some("monthly") => Ok("monthly"),
        Some("yearly") => Ok("yearly"),
        Some(other) => Err(anyhow::anyhow!(
            "period 只能是 daily/monthly/yearly，收到 {other}"
        )),
        None => Err(anyhow::anyhow!("缺少 period")),
    }
}

fn row_pid(r: &sqlx::postgres::PgRow) -> i64 {
    r.try_get::<i64, _>("person_id")
        .or_else(|_| r.try_get::<i64, _>("id"))
        .unwrap_or(0)
}

fn row_i64(r: &sqlx::postgres::PgRow, col: &str) -> i64 {
    r.try_get::<i64, _>(col)
        .or_else(|_| r.try_get::<i32, _>(col).map(i64::from))
        .unwrap_or(0)
}

async fn latest_date(st: &Arc<AppState>) -> anyhow::Result<String> {
    repos::get_available_dates(st, None, false)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("还没有业务数据"))
}

async fn load_rank_rows(
    st: &Arc<AppState>,
    team: &str,
    period: &str,
    args: &Value,
) -> anyhow::Result<(String, Vec<RankRow>)> {
    if period == "daily" {
        let date = args
            .get("date")
            .and_then(Value::as_str)
            .filter(|d| valid_date(d))
            .map(str::to_string)
            .unwrap_or(latest_date(st).await?);
        let rep = report::build(st, &date, team).await?;
        let rows = rep
            .rows
            .into_iter()
            .map(|r| RankRow {
                name: r.name,
                wave: r.wave,
                minutes: r.minutes,
                absent_days: r.absent_days,
            })
            .collect();
        return Ok((date, rows));
    }

    let period = if period == "monthly" {
        args.get("month")
            .and_then(Value::as_str)
            .filter(|p| p.len() == 7 && dialect::valid_period_shape(p))
            .map(str::to_string)
            .unwrap_or(latest_date(st).await?.get(..7).unwrap_or("").to_string())
    } else {
        args.get("year")
            .and_then(Value::as_str)
            .filter(|p| p.len() == 4 && dialect::valid_period_shape(p))
            .map(str::to_string)
            .unwrap_or(latest_date(st).await?.get(..4).unwrap_or("").to_string())
    };
    if period.is_empty() {
        anyhow::bail!("没有可用的周期");
    }

    let roster_sql = format!(
        "SELECT id AS person_id, name FROM person \
         WHERE deleted_at IS NULL AND status <> 'left' AND hide_in_daily_report = 0 \
           AND gender = '{team}' ORDER BY name"
    );
    let roster = crate::pgrs::query(&st.pools, "LOCAL", &roster_sql).await?;
    let like = format!("{period}%");
    let stats = dialect::query_period(
        &st.pools,
        "LOCAL",
        dialect::ADMIN_PERIOD_SELECT,
        &like,
        team,
        " AND p.hide_in_daily_report = 0 AND p.status <> 'left'",
        "wave DESC",
    )
    .await?;
    let mut wave_map = std::collections::HashMap::new();
    let mut minute_map = std::collections::HashMap::new();
    for r in &stats {
        let pid = row_pid(r);
        wave_map.insert(pid, row_i64(r, "wave"));
        minute_map.insert(pid, row_i64(r, "minutes"));
    }
    let (lo, hi) = dialect::like_to_date_range(&like);
    let absent_sql = format!(
        "SELECT person_id, (SUM(CASE WHEN wave < 2 THEN 1 ELSE 0 END))::BIGINT AS n \
         FROM daily_metric WHERE wave_reliable = 1 \
           AND biz_date >= '{lo}'::date AND biz_date < '{hi}'::date GROUP BY person_id"
    );
    let mut absent_map = std::collections::HashMap::new();
    for r in &crate::pgrs::query(&st.pools, "LOCAL", &absent_sql).await? {
        absent_map.insert(row_pid(r), row_i64(r, "n"));
    }

    let mut rows: Vec<RankRow> = roster
        .iter()
        .map(|r| {
            let pid = row_pid(r);
            RankRow {
                name: r.try_get::<String, _>("name").unwrap_or_default(),
                wave: wave_map.get(&pid).copied().unwrap_or(0),
                minutes: minute_map.get(&pid).copied().unwrap_or(0),
                absent_days: absent_map.get(&pid).copied().unwrap_or(0),
            }
        })
        .collect();
    rows.sort_by(|a, b| b.wave.cmp(&a.wave).then_with(|| a.name.cmp(&b.name)));
    Ok((period, rows))
}

fn clamp_limit(args: &Value) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .clamp(1, MAX_ROWS as u64) as usize
}

fn team_label(team: &str) -> &'static str {
    if team == "female" {
        "女团"
    } else {
        "男团"
    }
}

fn row_json(rank: usize, r: &RankRow) -> Value {
    json!({
        "rank": rank,
        "name": r.name,
        "wave": r.wave,
        "minutes": r.minutes,
        "absentDays": r.absent_days,
        "live": r.wave >= 2
    })
}

async fn get_team_ranking(st: &Arc<AppState>, args: &Value) -> anyhow::Result<Value> {
    let team = team_arg(args)?;
    let period = period_arg(args)?;
    let (as_of, rows) = load_rank_rows(st, team, period, args).await?;
    let limit = clamp_limit(args);
    let shown: Vec<Value> = rows
        .iter()
        .take(limit)
        .enumerate()
        .map(|(i, r)| row_json(i + 1, r))
        .collect();
    let summary = if let Some(first) = rows.first() {
        format!(
            "{} {} 共 {} 人，{}第 1，音浪 {}。",
            team_label(team),
            as_of,
            rows.len(),
            first.name,
            first.wave
        )
    } else {
        format!("{} {} 没有对外展示数据。", team_label(team), as_of)
    };
    Ok(json!({
        "summary": summary,
        "asOf": as_of,
        "team": team,
        "period": period,
        "rows": shown,
        "total": rows.len()
    }))
}

async fn get_team_summary(st: &Arc<AppState>, args: &Value) -> anyhow::Result<Value> {
    let team = team_arg(args)?;
    let period = period_arg(args)?;
    let (as_of, rows) = load_rank_rows(st, team, period, args).await?;
    let total_wave: i64 = rows.iter().map(|r| r.wave).sum();
    let live = rows.iter().filter(|r| r.wave >= 2).count();
    let top3: Vec<Value> = rows
        .iter()
        .take(3)
        .enumerate()
        .map(|(i, r)| row_json(i + 1, r))
        .collect();
    Ok(json!({
        "summary": format!("{} {} 共 {} 人，总音浪 {}，开播 {} 人，未播 {} 人。", team_label(team), as_of, rows.len(), total_wave, live, rows.len().saturating_sub(live)),
        "asOf": as_of,
        "team": team,
        "period": period,
        "totalWave": total_wave,
        "liveCount": live,
        "offlineCount": rows.len().saturating_sub(live),
        "top3": top3
    }))
}

async fn search_anchor(st: &Arc<AppState>, args: &Value) -> anyhow::Result<Value> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if query.is_empty() {
        anyhow::bail!("query 不能为空");
    }
    let team_clause = match args.get("team").and_then(Value::as_str) {
        Some("male") => " AND gender = 'male'",
        Some("female") => " AND gender = 'female'",
        Some(other) => anyhow::bail!("team 只能是 male 或 female，收到 {other}"),
        None => "",
    };
    let sql = format!(
        "SELECT name, gender FROM person \
         WHERE deleted_at IS NULL AND status <> 'left' AND hide_in_daily_report = 0{team_clause} \
           AND name ILIKE ? ORDER BY name LIMIT 20"
    );
    let pattern = format!("%{query}%");
    let rows = crate::pgrs::query_with_binds(&st.pools, "LOCAL", &sql, |q| q.bind(pattern)).await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "name": r.try_get::<String, _>("name").unwrap_or_default(),
                "team": r.try_get::<String, _>("gender").unwrap_or_default()
            })
        })
        .collect();
    Ok(json!({
        "summary": format!("搜索到 {} 位对外展示主播。", items.len()),
        "items": items
    }))
}

async fn get_anchor_rank(st: &Arc<AppState>, args: &Value) -> anyhow::Result<Value> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if name.is_empty() {
        anyhow::bail!("name 不能为空");
    }
    let period = period_arg(args)?;
    let name_owned = name.to_string();
    let rows = crate::pgrs::query_with_binds(
        &st.pools,
        "LOCAL",
        "SELECT name, gender FROM person WHERE deleted_at IS NULL AND status <> 'left' AND hide_in_daily_report = 0 AND name = ? LIMIT 2",
        |q| q.bind(name_owned),
    ).await?;
    if rows.is_empty() {
        anyhow::bail!("没有找到对外展示主播「{name}」");
    }
    if rows.len() > 1 {
        anyhow::bail!("主播「{name}」存在重名，请先在台账中处理");
    }
    let team = rows[0].try_get::<String, _>("gender").unwrap_or_default();
    let (as_of, all) = load_rank_rows(st, &team, period, args).await?;
    let Some((idx, row)) = all.iter().enumerate().find(|(_, r)| r.name == name) else {
        anyhow::bail!("主播「{name}」在 {} 榜单中没有数据", as_of);
    };
    Ok(json!({
        "summary": format!("{}在{} {} 排名第 {}，音浪 {}，时长 {} 分钟，未播 {} 天。", name, team_label(&team), as_of, idx + 1, row.wave, row.minutes, row.absent_days),
        "asOf": as_of,
        "team": team,
        "period": period,
        "rank": idx + 1,
        "name": name,
        "wave": row.wave,
        "minutes": row.minutes,
        "absentDays": row.absent_days
    }))
}

async fn get_daily_briefing(st: &Arc<AppState>, args: &Value) -> anyhow::Result<Value> {
    let team = team_arg(args)?;
    let mut a = args.clone();
    a["period"] = json!("daily");
    let (as_of, rows) = load_rank_rows(st, team, "daily", &a).await?;
    let top: Vec<Value> = rows
        .iter()
        .take(5)
        .enumerate()
        .map(|(i, r)| row_json(i + 1, r))
        .collect();
    let total_wave: i64 = rows.iter().map(|r| r.wave).sum();
    let live = rows.iter().filter(|r| r.wave >= 2).count();
    let summary = format!(
        "{} {} 每日播报：总音浪 {}，开播 {} 人，未播 {} 人。前三名：{}。",
        team_label(team),
        as_of,
        total_wave,
        live,
        rows.len().saturating_sub(live),
        rows.iter()
            .take(3)
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>()
            .join("、")
    );
    Ok(json!({
        "summary": summary,
        "asOf": as_of,
        "team": team,
        "totalWave": total_wave,
        "liveCount": live,
        "offlineCount": rows.len().saturating_sub(live),
        "top5": top
    }))
}

async fn get_data_status(st: &Arc<AppState>) -> anyhow::Result<Value> {
    let dates = repos::get_available_dates(st, None, false).await?;
    let latest = dates.first().cloned().unwrap_or_default();
    let mut counts = serde_json::Map::new();
    for team in ["male", "female"] {
        let rows = crate::pgrs::query_with_binds(
            &st.pools,
            "LOCAL",
            "SELECT COUNT(*) AS n FROM person WHERE deleted_at IS NULL AND status <> 'left' AND hide_in_daily_report = 0 AND gender = ?",
            |q| q.bind(team),
        ).await?;
        counts.insert(
            team.to_string(),
            json!(rows.first().map(|r| row_i64(r, "n")).unwrap_or(0)),
        );
    }
    Ok(json!({
        "summary": if latest.is_empty() { "暂无可用的业务数据。".to_string() } else { format!("最新业务日 {latest}，共有 {} 个业务日。", dates.len()) },
        "latestDate": latest,
        "availableDates": dates.len(),
        "visiblePersons": counts
    }))
}

fn valid_date(s: &str) -> bool {
    s.len() == 10 && chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_stable() {
        let names: Vec<String> = tool_defs()
            .iter()
            .filter_map(|v| v.get("name").and_then(Value::as_str).map(str::to_string))
            .collect();
        assert!(names.contains(&"get_team_ranking".to_string()));
        assert!(names.contains(&"get_daily_briefing".to_string()));
        assert!(names.contains(&"get_data_status".to_string()));
    }

    #[test]
    fn date_validation_is_strict() {
        assert!(valid_date("2026-10-24"));
        assert!(!valid_date("2026-10-2"));
        assert!(!valid_date("2026-13-01"));
    }
}
