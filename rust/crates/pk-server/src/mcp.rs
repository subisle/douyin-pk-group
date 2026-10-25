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

use chrono::Datelike as _;
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
const HEARTBEAT_SECS: u64 = 20;

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
            // 成功建连后由 run_connection 把 backoff 重置为 1 秒；只有连不上时
            // 才指数退避，避免「云端周期性回收连接」把恢复时间越拖越长。
            match run_connection(&st, &endpoint, &mut backoff).await {
                // 不打印 e：底层 WebSocket 错误可能包含带 token 的 endpoint。
                Err(_) => tracing::warn!("小智 MCP 连接失败或断开，稍后重连"),
                Ok(()) => {}
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    });
}

async fn run_connection(
    st: &Arc<AppState>,
    endpoint: &str,
    backoff: &mut Duration,
) -> anyhow::Result<()> {
    tracing::info!("小智 MCP 正在连接");
    let (ws, _) = connect_async(endpoint).await?;
    tracing::info!("小智 MCP 已连接");
    *backoff = Duration::from_secs(1);
    let (mut write, mut read) = ws.split();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // interval 的第一次 tick 是立即触发，先吃掉，避免刚连上就多发一个 ping。
    heartbeat.tick().await;

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                // WebSocket 控制帧会穿过大多数代理，比只依赖小智的 JSON-RPC ping
                // 更能避免 Cloudflare / 网关按空闲时间回收长连接。
                write.send(Message::Ping(Vec::new().into())).await?;
            }
            next = read.next() => {
                let Some(frame) = next else {
                    tracing::warn!("小智 MCP 连接 EOF，准备重连");
                    break;
                };
                let frame = frame?;
                let text = match frame {
                    Message::Text(text) => text.to_string(),
                    Message::Binary(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                    Message::Ping(payload) => {
                        write.send(Message::Pong(payload)).await?;
                        continue;
                    }
                    Message::Pong(_) | Message::Frame(_) => continue,
                    Message::Close(_) => {
                        tracing::warn!("小智 MCP 收到关闭帧，准备重连");
                        break;
                    }
                };
                if let Some(response) = handle_message(st, &text).await {
                    write.send(Message::Text(response.to_string())).await?;
                }
            }
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
                "version": "1.3.0"
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
        "get_personal_daily_briefing" => get_personal_daily_briefing(st, &args).await,
        "get_daily_briefing" => get_daily_briefing(st, &args).await,
        "get_data_status" => get_data_status(st).await,
        _ => Err(anyhow::anyhow!("未知工具：{name}")),
    }?;

    // 只把小智可直接朗读的中文交给模型，不把结构化 JSON 塞进上下文。
    // 否则模型可能再次概括并读出 waveValue 等无单位原始整数。
    let text = spoken_text(&value);
    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false
    }))
}

fn spoken_text(value: &Value) -> &str {
    value
        .get("speechText")
        .and_then(Value::as_str)
        .or_else(|| value.get("summary").and_then(Value::as_str))
        .unwrap_or("操作已完成，但没有可朗读的内容。")
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
            "description": "查询男团或女团的日榜、月榜、年榜。返回排名、艺名、音浪、时长、未播天数和开播状态。音浪使用中文口语单位，例如5万2千；回答时请保留单位，不要改说原始整数。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "team": { "type": "string", "enum": ["male", "female"], "description": "male=男团，female=女团" },
                    "period": { "type": "string", "enum": ["daily", "monthly", "yearly"], "description": "daily=日榜，monthly=月榜，yearly=年榜" },
                    "date": { "type": "string", "description": "日榜日期：YYYY-MM-DD、today/今天、yesterday/昨天或 latest；缺省为最新业务日。今天尚无数据时自动落到最新业务日" },
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
            "description": "查询男团或女团某个周期的汇总数据：总音浪、开播人数、未播人数和榜单前三。音浪使用中文口语单位，例如5万2千；回答时请保留单位。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "team": { "type": "string", "enum": ["male", "female"] },
                    "period": { "type": "string", "enum": ["daily", "monthly", "yearly"] },
                    "date": { "type": "string", "description": "YYYY-MM-DD、today/今天、yesterday/昨天或 latest；今天无数据时落到最新业务日" },
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
            "description": "查询某位对外展示主播在指定周期的团内排名、音浪、时长和未播天数。音浪使用中文口语单位，例如5万2千；回答时请保留单位。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "主播艺名" },
                    "period": { "type": "string", "enum": ["daily", "monthly", "yearly"] },
                    "date": { "type": "string", "description": "YYYY-MM-DD、today/今天、yesterday/昨天或 latest；今天无数据时落到最新业务日" },
                    "month": { "type": "string" },
                    "year": { "type": "string" }
                },
                "required": ["name", "period"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "get_personal_daily_briefing",
            "description": "生成某位主播在指定业务日的个人播报：音浪、团内名次、时长、上下名次差距、本月累计与月榜名次、未播天数、预估到手和一句状态总结。用户说“个人每日播报/播报某主播/查某主播今天怎么样”时优先调用。日期可传 YYYY-MM-DD、today/今天、yesterday/昨天或 latest；今天尚无数据时服务端会落到最新业务日，播报必须使用返回的真实日期，禁止把昨日数据说成今天。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "主播艺名" },
                    "date": { "type": "string", "description": "YYYY-MM-DD、today/今天、yesterday/昨天或 latest；缺省为最新业务日。今天尚无数据时自动落到最新业务日" }
                },
                "required": ["name"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "get_daily_briefing",
            "description": "生成约3分钟中文语音播报稿：先给一句整体总结，再直接播报指定业务日男团或女团前十名、音浪、时长、未播天数、相邻名次差距和逐名评价。用户说“每日播报/今天播报/前十名播报”时必须调用此工具，并原样朗读返回文本。日期可传 YYYY-MM-DD、today/今天、yesterday/昨天或 latest；今天尚无数据时服务端会落到最新业务日。必须按返回的真实日期播报，禁止把昨日数据说成今天。不要寒暄，不要回答有没有数据，不要说“我查一下/以下是/有数据”。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "team": { "type": "string", "enum": ["male", "female"], "description": "缺省为男团" },
                    "date": { "type": "string", "description": "YYYY-MM-DD、today/今天、yesterday/昨天或 latest；缺省为最新业务日。今天尚无数据时自动落到最新业务日" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "get_data_status",
            "description": "查询今天日期、最新业务日、数据滞后天数（freshness）、可用日期数量以及男团/女团对外展示人数。用户问“今天数据”时先用本工具确认实际最新业务日，再查榜；不得自行假设当天已有数据。",
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
        let latest = latest_date(st).await?;
        let date = resolve_daily_date(
            args.get("date").and_then(Value::as_str),
            chrono::Local::now().date_naive(),
            &latest,
        );
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

fn period_label(period: &str) -> &'static str {
    match period {
        "monthly" => "月榜",
        "yearly" => "年榜",
        _ => "日榜",
    }
}

fn speech_date(date: &str) -> String {
    match chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") {
        Ok(d) => format!("{}年{}月{}日", d.year(), d.month(), d.day()),
        Err(_) => date.to_string(),
    }
}

fn speech_month(month: &str) -> String {
    let raw = format!("{month}-01");
    match chrono::NaiveDate::parse_from_str(&raw, "%Y-%m-%d") {
        Ok(d) => format!("{}年{}月", d.year(), d.month()),
        Err(_) => month.to_string(),
    }
}

fn speech_date_with_relation(date: &str, today: chrono::NaiveDate) -> String {
    let label = speech_date(date);
    let Ok(d) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") else {
        return label;
    };
    match (today - d).num_days() {
        0 => format!("{label}（今天）"),
        1 => format!("{label}（昨日数据）"),
        _ => label,
    }
}

/// 把 MCP 的日期参数收敛成真实业务日。
///
/// 数据本身就是「次日导入前一天」：今天来问时，今天通常还没有数据。
/// 这里接受日期、today/今日、yesterday/昨日、latest/最新；今天或未来日期
/// 超过库里的最新业务日时，明确落到最新业务日，绝不把昨天数据标成今天。
fn resolve_daily_date(raw: Option<&str>, today: chrono::NaiveDate, latest: &str) -> String {
    let raw = raw.unwrap_or("").trim();
    if raw.is_empty()
        || matches!(
            raw.to_ascii_lowercase().as_str(),
            "latest" | "最新" | "最新数据" | "最新业务日"
        )
    {
        return latest.to_string();
    }
    let lower = raw.to_ascii_lowercase();
    let candidate = match lower.as_str() {
        "today" | "今日" | "今天" => today,
        "yesterday" | "昨日" | "昨天" => today - chrono::Duration::days(1),
        _ if valid_date(raw) => {
            chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").expect("valid_date 已校验过格式")
        }
        _ => return latest.to_string(),
    };
    let candidate_s = candidate.format("%Y-%m-%d").to_string();
    let latest_parsed = chrono::NaiveDate::parse_from_str(latest, "%Y-%m-%d").ok();
    // 显式历史日期照查；今天/未来，或今天不存在的自然语言日期，一律回到
    // 当前最新业务日，避免模型自己传入今天的日历日期后返回一份空榜。
    if candidate > today {
        return latest.to_string();
    }
    if candidate_s.as_str() > latest {
        let relative = matches!(
            lower.as_str(),
            "today" | "今日" | "今天" | "yesterday" | "昨日" | "昨天"
        );
        if relative || candidate == today || latest_parsed.is_none() {
            return latest.to_string();
        }
    }
    candidate_s
}

fn data_lag_days(latest: &str, today: chrono::NaiveDate) -> i64 {
    chrono::NaiveDate::parse_from_str(latest, "%Y-%m-%d")
        .map(|d| (today - d).num_days().max(0))
        .unwrap_or(0)
}

fn pct(part: i64, total: i64) -> i64 {
    if total <= 0 {
        return 0;
    }
    (i128::from(part.max(0)) * 100 / i128::from(total)) as i64
}

/// 语音可读的音浪数字：52542 -> 5万2千。
/// 一万以上按万/千取整展示；低于一万保留千位，避免 TTS 读一长串数字。
fn wave_text(wave: i64) -> String {
    let n = wave.max(0);
    if n >= 10_000 {
        let wan = n / 10_000;
        let qian = (n % 10_000) / 1_000;
        return if qian > 0 {
            format!("{wan}万{qian}千")
        } else {
            format!("{wan}万")
        };
    }
    if n >= 1_000 {
        return format!("{}千", n / 1_000);
    }
    n.to_string()
}

fn row_json(rank: usize, r: &RankRow) -> Value {
    json!({
        "rank": rank,
        "name": r.name,
        "wave": wave_text(r.wave),
        "waveValue": r.wave,
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
    let visible_rows = rows.iter().take(limit).collect::<Vec<_>>();
    let shown: Vec<Value> = visible_rows
        .iter()
        .enumerate()
        .map(|(i, r)| row_json(i + 1, r))
        .collect();
    let summary = if let Some(first) = rows.first() {
        format!(
            "{} {} {}共{}人，第一名{}，音浪{}。",
            team_label(team),
            as_of,
            period_label(period),
            rows.len(),
            first.name,
            wave_text(first.wave)
        )
    } else {
        format!(
            "{} {} {}没有对外展示数据。",
            team_label(team),
            as_of,
            period_label(period)
        )
    };
    let mut speech_text = if visible_rows.is_empty() {
        summary.clone()
    } else {
        format!(
            "{} {} {}共{}人。",
            team_label(team),
            as_of,
            period_label(period),
            rows.len()
        )
    };
    if !visible_rows.is_empty() {
        speech_text.push(' ');
        for (i, row) in visible_rows.iter().enumerate() {
            if i > 0 {
                speech_text.push(' ');
            }
            speech_text.push_str(&format!(
                "第{}名{}，音浪{}，时长{}，未播{}天，{}。",
                i + 1,
                row.name,
                wave_text(row.wave),
                speech_minutes(row.minutes),
                row.absent_days,
                if row.wave >= 2 {
                    "已开播"
                } else {
                    "未形成有效开播"
                }
            ));
        }
        if rows.len() > visible_rows.len() {
            speech_text.push_str(&format!(
                "以上为前{}名，完整榜单共{}人。",
                visible_rows.len(),
                rows.len()
            ));
        }
    }
    Ok(json!({
        "speechText": speech_text,
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
    let top3_rows = rows.iter().take(3).collect::<Vec<_>>();
    let top3: Vec<Value> = top3_rows
        .iter()
        .enumerate()
        .map(|(i, r)| row_json(i + 1, r))
        .collect();
    let offline = rows.len().saturating_sub(live);
    let summary = format!(
        "{} {} {}共{}人，总音浪{}，开播{}人，未播或暂无有效音浪{}人。",
        team_label(team),
        as_of,
        period_label(period),
        rows.len(),
        wave_text(total_wave),
        live,
        offline
    );
    let mut speech_text = summary.clone();
    if !top3_rows.is_empty() {
        speech_text.push(' ');
        for (i, row) in top3_rows.iter().enumerate() {
            if i > 0 {
                speech_text.push(' ');
            }
            speech_text.push_str(&format!(
                "第{}名{}，音浪{}，时长{}，未播{}天。",
                i + 1,
                row.name,
                wave_text(row.wave),
                speech_minutes(row.minutes),
                row.absent_days
            ));
        }
    }
    Ok(json!({
        "speechText": speech_text,
        "summary": summary,
        "asOf": as_of,
        "team": team,
        "period": period,
        "totalWave": wave_text(total_wave),
        "totalWaveValue": total_wave,
        "liveCount": live,
        "offlineCount": offline,
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
    let found: Vec<(String, String)> = rows
        .iter()
        .map(|r| {
            (
                r.try_get::<String, _>("name").unwrap_or_default(),
                r.try_get::<String, _>("gender").unwrap_or_default(),
            )
        })
        .collect();
    let items: Vec<Value> = found
        .iter()
        .map(|(name, team)| {
            json!({
                "name": name,
                "team": team
            })
        })
        .collect();
    let summary = if found.is_empty() {
        format!("没有找到名称包含“{query}”的对外展示主播。")
    } else {
        format!("搜索到 {} 位对外展示主播。", found.len())
    };
    let speech_text = if found.is_empty() {
        summary.clone()
    } else {
        let names = found
            .iter()
            .map(|(name, team)| format!("{name}，{}", team_label(team)))
            .collect::<Vec<_>>()
            .join("；");
        format!("{} 分别是：{}。", summary, names)
    };
    Ok(json!({
        "speechText": speech_text,
        "summary": summary,
        "items": items
    }))
}

async fn anchor_team(st: &Arc<AppState>, name: &str) -> anyhow::Result<String> {
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
    Ok(rows[0].try_get::<String, _>("gender").unwrap_or_default())
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
    let team = anchor_team(st, name).await?;
    let (as_of, all) = load_rank_rows(st, &team, period, args).await?;
    let Some((idx, row)) = all.iter().enumerate().find(|(_, r)| r.name == name) else {
        anyhow::bail!("主播「{name}」在 {} 榜单中没有数据", as_of);
    };
    let summary = format!(
        "{}在{} {} {}排名第{}，音浪{}，时长{}，未播{}天，{}。",
        name,
        team_label(&team),
        as_of,
        period_label(period),
        idx + 1,
        wave_text(row.wave),
        speech_minutes(row.minutes),
        row.absent_days,
        if row.wave >= 2 {
            "已开播"
        } else {
            "未形成有效开播"
        }
    );
    Ok(json!({
        "speechText": summary.clone(),
        "summary": summary,
        "asOf": as_of,
        "team": team,
        "period": period,
        "rank": idx + 1,
        "name": name,
        "wave": wave_text(row.wave),
        "waveValue": row.wave,
        "minutes": row.minutes,
        "absentDays": row.absent_days
    }))
}

pub(crate) async fn get_personal_daily_briefing(
    st: &Arc<AppState>,
    args: &Value,
) -> anyhow::Result<Value> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if name.is_empty() {
        anyhow::bail!("name 不能为空");
    }
    let team = anchor_team(st, name).await?;
    let latest = latest_date(st).await?;
    let date = resolve_daily_date(
        args.get("date").and_then(Value::as_str),
        chrono::Local::now().date_naive(),
        &latest,
    );
    let daily_args = json!({ "date": date });
    let (_, daily_rows) = load_rank_rows(st, &team, "daily", &daily_args).await?;
    let month = date.get(..7).unwrap_or(&date).to_string();
    let month_args = json!({ "month": month });
    let (_, monthly_rows) = load_rank_rows(st, &team, "monthly", &month_args).await?;

    let Some((day_idx, day_row)) = daily_rows.iter().enumerate().find(|(_, r)| r.name == name)
    else {
        anyhow::bail!("主播「{name}」在 {} 日榜中没有数据", date);
    };
    let day_rank = day_idx + 1;
    let day_wave = day_row.wave;
    let day_minutes = day_row.minutes;
    let day_absent = day_row.absent_days;
    let month_hit = monthly_rows
        .iter()
        .enumerate()
        .find(|(_, r)| r.name == name)
        .map(|(idx, r)| (idx + 1, r.wave, r.absent_days));
    let est_value = if day_wave >= 2 {
        crate::push::est_yuan(st, day_wave).await
    } else {
        None
    };
    let est_speech =
        est_value.map(|v| crate::push::fmt_yuan(v).trim_start_matches('¥').to_string());
    let speech = build_personal_daily_speech(
        name,
        &team,
        &date,
        &month,
        &daily_rows,
        &monthly_rows,
        est_speech.as_deref(),
    );
    let (month_rank, month_wave, month_absent) = month_hit
        .map(|(rank, wave, absent)| (Some(rank), wave, absent))
        .unwrap_or((None, 0, day_absent));
    Ok(json!({
        "speechText": speech,
        "summary": format!("{name} {} 个人每日播报。", date),
        "asOf": date,
        "team": team,
        "name": name,
        "rank": day_rank,
        "wave": wave_text(day_wave),
        "waveValue": day_wave,
        "minutes": day_minutes,
        "absentDays": month_absent,
        "month": month,
        "monthRank": month_rank,
        "monthWave": wave_text(month_wave),
        "monthWaveValue": month_wave,
        "estimatedIncome": est_value.map(crate::push::fmt_yuan)
    }))
}

pub(crate) async fn get_daily_briefing(st: &Arc<AppState>, args: &Value) -> anyhow::Result<Value> {
    let team = team_arg(args)?;
    let mut a = args.clone();
    a["period"] = json!("daily");
    let (as_of, rows) = load_rank_rows(st, team, "daily", &a).await?;
    let top: Vec<Value> = rows
        .iter()
        .take(10)
        .enumerate()
        .map(|(i, r)| row_json(i + 1, r))
        .collect();
    let total_wave: i64 = rows.iter().map(|r| r.wave).sum();
    let live = rows.iter().filter(|r| r.wave >= 2).count();
    let speech = build_daily_speech(team, &as_of, &rows);
    let summary = format!(
        "{} {} 每日播报已生成，约3分钟，包含前十名和逐名评价。",
        team_label(team),
        as_of
    );
    Ok(json!({
        "summary": summary,
        "speechText": speech,
        "asOf": as_of,
        "team": team,
        "totalWave": wave_text(total_wave),
        "totalWaveValue": total_wave,
        "liveCount": live,
        "offlineCount": rows.len().saturating_sub(live),
        "top10": top
    }))
}

fn speech_minutes(minutes: i64) -> String {
    if minutes <= 0 {
        return "没有登记有效时长".into();
    }
    let h = minutes / 60;
    let m = minutes % 60;
    match (h, m) {
        (0, _) => format!("{m}分钟"),
        (_, 0) => format!("{h}小时"),
        _ => format!("{h}小时{m}分钟"),
    }
}

fn speech_gap(wave: i64, previous: Option<i64>) -> String {
    match previous {
        Some(p) if p > wave => format!("距离上一名还差{}音浪", wave_text(p - wave)),
        Some(p) if p < wave => format!("领先上一名{}音浪", wave_text(wave - p)),
        Some(_) => "与上一名音浪持平".into(),
        None => String::new(),
    }
}

fn absent_text(days: i64) -> String {
    if days > 0 {
        format!("本月未播{days}天")
    } else {
        "本月暂无未播记录".into()
    }
}

fn daily_overview(rows: &[RankRow]) -> String {
    let total_wave: i64 = rows.iter().map(|r| r.wave).sum();
    let live = rows.iter().filter(|r| r.wave >= 2).count();
    let average = if rows.is_empty() {
        0
    } else {
        total_wave / rows.len() as i64
    };
    let top3_wave: i64 = rows.iter().take(3).map(|r| r.wave).sum();
    let top10_wave: i64 = rows.iter().take(10).map(|r| r.wave).sum();
    let mut out = format!(
        "当日有效开播{}人，全团总音浪{}，人均音浪约{}。",
        live,
        wave_text(total_wave),
        wave_text(average)
    );
    let Some(first) = rows.first() else {
        return out;
    };
    out.push_str(&format!(
        "{}以音浪{}领跑",
        first.name,
        wave_text(first.wave)
    ));
    match rows.get(1) {
        Some(second) if first.wave > second.wave => out.push_str(&format!(
            "，领先第二名{}",
            wave_text(first.wave - second.wave)
        )),
        Some(_) => out.push_str("，与第二名音浪持平"),
        None => out.push_str("，目前独自处在榜首"),
    }
    out.push_str(&format!(
        "。前三名贡献全团{}%的音浪，前十名贡献{}%。",
        pct(top3_wave, total_wave),
        pct(top10_wave, total_wave)
    ));
    out
}

fn daily_gap_line(rows: &[RankRow], idx: usize) -> String {
    let row = &rows[idx];
    if idx == 0 {
        return match rows.get(1) {
            Some(second) if row.wave > second.wave => {
                format!("领先第二名{}音浪", wave_text(row.wave - second.wave))
            }
            Some(_) => "与第二名音浪持平".into(),
            None => "目前独自处在榜首".into(),
        };
    }
    speech_gap(row.wave, rows.get(idx - 1).map(|r| r.wave))
}

fn daily_row_evaluation(rows: &[RankRow], idx: usize) -> String {
    let row = &rows[idx];
    let rank = idx + 1;
    if row.wave < 2 {
        return "当日还没有形成有效音浪，需要先确认开播状态和数据是否入库。".into();
    }
    if rank == 1 {
        if rows.len() == 1 {
            return "目前独自处在榜首，继续稳定开播就能保持优势。".into();
        }
        let gap = (row.wave - rows[1].wave).max(0);
        return if gap == 0 {
            "与第二名音浪持平，榜首位置并不稳，后续竞争会很紧。".into()
        } else if gap <= row.wave / 20 {
            "领先优势不大，守榜压力已经开始显现。".into()
        } else {
            "领跑优势明显，守榜压力相对可控。".into()
        };
    }
    if rank <= 3 {
        let gap = (rows[0].wave - row.wave).max(0);
        return if gap == 0 {
            "与榜首音浪持平，第一集团竞争非常胶着。".into()
        } else {
            format!(
                "处在第一梯队，距离榜首还差{}音浪，仍有继续冲击的空间。",
                wave_text(gap)
            )
        };
    }
    if rank <= 6 {
        if let Some(third) = rows.get(2) {
            let gap = (third.wave - row.wave).max(0);
            return format!(
                "位于头部中段，距离前三还差{}音浪，稳定开播是继续缩小差距的关键。",
                wave_text(gap)
            );
        }
        return "位于头部中段，继续保持稳定开播和互动转化。".into();
    }
    if rank <= 10 {
        if let Some(eleventh) = rows.get(10) {
            let cushion = (row.wave - eleventh.wave).max(0);
            return if cushion == 0 {
                "与第十一名卡在同一水平，前十位置随时可能变化。".into()
            } else {
                format!(
                    "处在前十卡位区，目前领先第十一名{}音浪，接下来要守住位置。",
                    wave_text(cushion)
                )
            };
        }
        return "已经进入前十，继续保持稳定开播。".into();
    }
    "排名还有提升空间，重点是保持开播时长和互动转化。".into()
}

fn build_daily_speech(team: &str, as_of: &str, rows: &[RankRow]) -> String {
    let date = speech_date_with_relation(as_of, chrono::Local::now().date_naive());
    if rows.is_empty() {
        return format!("{date}，{}暂时没有可播报的数据。", team_label(team));
    }
    let total_wave: i64 = rows.iter().map(|r| r.wave).sum();
    let mut out = format!("{date}，{}每日播报。", team_label(team));
    out.push_str(&daily_overview(rows));
    out.push(' ');
    for (i, row) in rows.iter().take(10).enumerate() {
        let status = if row.wave >= 2 {
            "已开播"
        } else {
            "未形成有效开播"
        };
        out.push_str(&format!(
            "第{}名，{}。当日音浪{}，占全团{}%，{}，直播时长{}，{}。{}。评价：{}",
            i + 1,
            row.name,
            wave_text(row.wave),
            pct(row.wave, total_wave),
            status,
            speech_minutes(row.minutes),
            absent_text(row.absent_days),
            daily_gap_line(rows, i),
            daily_row_evaluation(rows, i),
        ));
        if i + 1 < rows.len().min(10) {
            out.push(' ');
        }
    }
    if rows.len() > 10 {
        let tenth = &rows[9];
        let eleventh = &rows[10];
        out.push_str(&format!(
            " 第十名门槛是{}，音浪{}；第十一名{}，音浪{}，距离前十还差{}音浪。",
            tenth.name,
            wave_text(tenth.wave),
            eleventh.name,
            wave_text(eleventh.wave),
            wave_text((tenth.wave - eleventh.wave).max(0))
        ));
    }
    out
}

fn personal_evaluation(rows: &[RankRow], idx: usize) -> String {
    let row = &rows[idx];
    let rank = idx + 1;
    if row.wave < 2 {
        return "当日还没有形成有效音浪，先确认是否开播以及数据是否入库。".into();
    }
    if rank == 1 {
        return "当日处在领跑位置，守榜状态不错。".into();
    }
    if rank <= 3 {
        let gap = (rows[0].wave - row.wave).max(0);
        return format!("当日稳在第一梯队，距离榜首还差{}音浪。", wave_text(gap));
    }
    if rank <= 10 {
        if let Some(third) = rows.get(2) {
            let gap = (third.wave - row.wave).max(0);
            return format!(
                "当日进入前十，距离前三还差{}音浪，继续稳定开播就有追赶空间。",
                wave_text(gap)
            );
        }
    }
    if let Some(tenth) = rows.get(9) {
        let gap = (tenth.wave - row.wave).max(0);
        return format!(
            "距离前十还差{}音浪，重点是保持开播时长和互动转化。",
            wave_text(gap)
        );
    }
    "继续保持稳定开播和互动转化。".into()
}

fn build_personal_daily_speech(
    name: &str,
    team: &str,
    as_of: &str,
    month: &str,
    daily_rows: &[RankRow],
    monthly_rows: &[RankRow],
    estimated_income: Option<&str>,
) -> String {
    let Some((day_idx, day)) = daily_rows.iter().enumerate().find(|(_, r)| r.name == name) else {
        return format!(
            "{}，{}个人每日播报。该业务日暂时没有可播报的数据。",
            name,
            speech_date_with_relation(as_of, chrono::Local::now().date_naive())
        );
    };
    let day_rank = day_idx + 1;
    let total_wave: i64 = daily_rows.iter().map(|r| r.wave).sum();
    let month_hit = monthly_rows
        .iter()
        .enumerate()
        .find(|(_, r)| r.name == name);
    let month_absent = month_hit
        .map(|(_, r)| r.absent_days)
        .unwrap_or(day.absent_days);
    let duration = if day.minutes > 0 {
        format!("直播时长{}", speech_minutes(day.minutes))
    } else {
        "当日没有登记有效时长".to_string()
    };
    let mut out = format!(
        "{}，{}个人每日播报。",
        name,
        speech_date_with_relation(as_of, chrono::Local::now().date_naive())
    );
    out.push_str(&format!(
        "当日音浪{}，在{}排名第{}，{}。",
        wave_text(day.wave),
        team_label(team),
        day_rank,
        if day.wave >= 2 {
            "已开播"
        } else {
            "未形成有效开播"
        }
    ));
    out.push_str(&format!("{}，{}。", duration, absent_text(month_absent)));
    if day_rank == 1 {
        match daily_rows.get(1) {
            Some(second) if day.wave > second.wave => out.push_str(&format!(
                "领先第二名{}音浪。",
                wave_text(day.wave - second.wave)
            )),
            Some(_) => out.push_str("与第二名音浪持平。"),
            None => out.push_str("目前独自处在榜首。"),
        }
    } else if let Some(previous) = daily_rows.get(day_idx - 1) {
        out.push_str(&format!("{}。", speech_gap(day.wave, Some(previous.wave))));
    }
    out.push_str(&format!("当日音浪占全团{}%。", pct(day.wave, total_wave)));
    match month_hit {
        Some((month_idx, month_row)) => out.push_str(&format!(
            "{}累计音浪{}，月榜第{}名。",
            speech_month(month),
            wave_text(month_row.wave),
            month_idx + 1,
        )),
        None => out.push_str(&format!(
            "{}累计暂时没有可播报的数据。",
            speech_month(month)
        )),
    }
    if let Some(est) = estimated_income {
        out.push_str(&format!("按当前分成估算，当日预计到手约{est}元。"));
    }
    out.push_str(&format!(
        "状态总结：{}",
        personal_evaluation(daily_rows, day_idx)
    ));
    out
}

async fn get_data_status(st: &Arc<AppState>) -> anyhow::Result<Value> {
    let dates = repos::get_available_dates(st, None, false).await?;
    let latest = dates.first().cloned().unwrap_or_default();
    let today = chrono::Local::now().date_naive();
    let calendar_today = today.format("%Y-%m-%d").to_string();
    let lag_days = if latest.is_empty() {
        0
    } else {
        data_lag_days(&latest, today)
    };
    let freshness = if latest.is_empty() {
        "暂无业务数据".to_string()
    } else {
        match lag_days {
            0 => "今日数据".to_string(),
            1 => "昨日数据".to_string(),
            n => format!("{n}天前数据"),
        }
    };
    let mut counts = serde_json::Map::new();
    let mut male_count = 0;
    let mut female_count = 0;
    for team in ["male", "female"] {
        let rows = crate::pgrs::query_with_binds(
            &st.pools,
            "LOCAL",
            "SELECT COUNT(*) AS n FROM person WHERE deleted_at IS NULL AND status <> 'left' AND hide_in_daily_report = 0 AND gender = ?",
            |q| q.bind(team),
        ).await?;
        let count = rows.first().map(|r| row_i64(r, "n")).unwrap_or(0);
        counts.insert(team.to_string(), json!(count));
        match team {
            "male" => male_count = count,
            "female" => female_count = count,
            _ => {}
        }
    }
    let summary = if latest.is_empty() {
        format!("今天日期{calendar_today}；暂无可用的业务数据。")
    } else {
        format!(
            "今天日期{calendar_today}；最新业务日是{latest}，属于{freshness}，距今{lag_days}天；共有{}个业务日，男团对外展示{}人，女团对外展示{}人。",
            dates.len(),
            male_count,
            female_count
        )
    };
    Ok(json!({
        "speechText": summary.clone(),
        "summary": summary,
        "calendarToday": calendar_today,
        "latestDate": latest,
        "dataLagDays": lag_days,
        "freshness": freshness,
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
        assert!(names.contains(&"get_personal_daily_briefing".to_string()));
        assert!(names.contains(&"get_daily_briefing".to_string()));
        assert!(names.contains(&"get_data_status".to_string()));
    }

    #[test]
    fn date_validation_is_strict() {
        assert!(valid_date("2026-10-24"));
        assert!(!valid_date("2026-10-2"));
        assert!(!valid_date("2026-13-01"));
    }

    #[test]
    fn wave_text_uses_chinese_units() {
        assert_eq!(wave_text(52_542), "5万2千");
        assert_eq!(wave_text(12_000), "1万2千");
        assert_eq!(wave_text(10_000), "1万");
        assert_eq!(wave_text(9_999), "9千");
        assert_eq!(wave_text(999), "999");
    }

    #[test]
    fn spoken_text_never_exposes_raw_wave_value() {
        let value = json!({
            "speechText": "音浪5万2千",
            "waveValue": 52_542
        });
        assert_eq!(spoken_text(&value), "音浪5万2千");
    }

    #[test]
    fn speech_date_and_month_are_natural() {
        assert_eq!(speech_date("2026-10-24"), "2026年10月24日");
        assert_eq!(speech_month("2026-10"), "2026年10月");
    }

    #[test]
    fn relative_dates_land_on_latest_real_business_day() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        let latest = "2026-10-24";
        assert_eq!(resolve_daily_date(None, today, latest), latest);
        assert_eq!(resolve_daily_date(Some("today"), today, latest), latest);
        assert_eq!(resolve_daily_date(Some("今天"), today, latest), latest);
        assert_eq!(
            resolve_daily_date(Some("2026-10-25"), today, latest),
            latest
        );
        assert_eq!(resolve_daily_date(Some("yesterday"), today, latest), latest);
        assert_eq!(
            resolve_daily_date(Some("2026-10-20"), today, latest),
            "2026-10-20"
        );
        assert_eq!(
            resolve_daily_date(Some("2026-10-26"), today, latest),
            latest
        );
    }

    #[test]
    fn speech_labels_yesterday_instead_of_calling_it_today() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        assert_eq!(
            speech_date_with_relation("2026-10-24", today),
            "2026年10月24日（昨日数据）"
        );
        assert_eq!(data_lag_days("2026-10-24", today), 1);
        assert_eq!(data_lag_days("2026-10-25", today), 0);
    }

    #[test]
    fn daily_speech_has_top_ten_and_three_minute_script() {
        let rows: Vec<RankRow> = (1..=12)
            .map(|i| RankRow {
                name: format!("主播{i}"),
                wave: 100_000 - i * 6_000,
                minutes: 1_200 - i * 40,
                absent_days: i % 4,
            })
            .collect();
        let speech = build_daily_speech("male", "2026-10-24", &rows);
        let expected_date =
            speech_date_with_relation("2026-10-24", chrono::Local::now().date_naive());
        assert!(
            speech.starts_with(&format!("{expected_date}，男团每日播报。")),
            "{speech}"
        );
        assert!(speech.contains("前三名贡献全团"), "{speech}");
        assert!(speech.contains("第十名"));
        assert!(speech.contains("评价："));
        assert!(speech.contains("音浪9万4千"));
        assert!(!speech.contains("94000"));
        assert!(!speech.contains("今天有效开播"), "{speech}");
        for filler in ["各位好", "以下是", "有没有数据", "我查一下", "有数据"] {
            assert!(!speech.contains(filler), "播报不应出现开场套话：{filler}");
        }
        assert!(
            speech.chars().count() >= 800,
            "播报稿过短：{}",
            speech.chars().count()
        );
    }

    #[test]
    fn personal_daily_speech_is_direct_and_summarizes() {
        let daily_rows: Vec<RankRow> = (1..=12)
            .map(|i| RankRow {
                name: format!("主播{i}"),
                wave: 100_000 - i * 6_000,
                minutes: 1_200 - i * 40,
                absent_days: i % 4,
            })
            .collect();
        let monthly_rows: Vec<RankRow> = (1..=12)
            .map(|i| RankRow {
                name: format!("主播{i}"),
                wave: 800_000 - i * 30_000,
                minutes: 20_000 - i * 500,
                absent_days: i % 3,
            })
            .collect();
        let speech = build_personal_daily_speech(
            "主播3",
            "male",
            "2026-10-24",
            "2026-10",
            &daily_rows,
            &monthly_rows,
            Some("1750"),
        );
        let expected_date =
            speech_date_with_relation("2026-10-24", chrono::Local::now().date_naive());
        assert!(
            speech.starts_with(&format!("主播3，{expected_date}个人每日播报。")),
            "{speech}"
        );
        assert!(speech.contains("当日音浪8万2千"), "{speech}");
        assert!(speech.contains("排名第3"), "{speech}");
        assert!(speech.contains("月榜第3"), "{speech}");
        assert!(speech.contains("预计到手约1750元"), "{speech}");
        assert!(speech.contains("状态总结："), "{speech}");
        assert!(!speech.contains("82000"), "{speech}");
        assert!(!speech.contains("今天音浪"), "{speech}");
        for filler in ["有没有数据", "我查一下", "有数据", "以下是"] {
            assert!(!speech.contains(filler), "个人播报不应有开场套话：{filler}");
        }
    }
}
