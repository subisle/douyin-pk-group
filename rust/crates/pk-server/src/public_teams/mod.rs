//! 公开端团别站点后端（阶段 D，docs/08）：
//! - /st/m、/st/f 两个独立链接，链接即团别
//! - 后端级显隐开关：关闭后不下发数据（不是前端藏）
//! - 「我的」页面验证：姓名+密码（纯数字；账号由管理员在后台手动添加，没有注册）
//! - 公开 API 一律 DTO 白名单，绝不下发密码、抖音号、全量候选名单
//! 拆分（2026-10-12）：按业务域分文件，对外路径 `public_teams::*` 不变。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use crate::dialect::{self, is_live_wave};
use crate::pgrs;
use crate::routes::ApiError;
use crate::state::AppState;
use axum::response::{IntoResponse, Response};
pub(crate) type ApiResult = Result<Json<Value>, ApiError>;
pub(crate) type ArcState = Arc<AppState>;
/// 公开端一律读「当前生效数据源」，不能写死某个源 id。
/// 早先版本在这里硬编码 `LOCAL`，结果把源切到 A（sqlpub）或直接只配 A 时，
/// 展示页全部接口报「未知数据源 LOCAL」——页面看着是空的，库里其实有数据。
pub(crate) fn sid(st: &AppState) -> String {
    st.active_id()
}

mod admin;
mod binds;
mod minutes;
mod my;
mod pages;
mod settings;
mod site;
mod wx;

pub use admin::*;
pub use binds::*;
pub use minutes::*;
pub use my::*;
pub use pages::*;
pub use settings::*;
pub use site::*;
pub use wx::*;
