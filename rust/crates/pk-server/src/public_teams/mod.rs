//! 公开端团别站点后端（阶段 D，docs/08）：
//! - /st/m、/st/f 两个独立链接，链接即团别
//! - 后端级显隐开关：关闭后不下发数据（不是前端藏）
//! - 「我的」页面验证：姓名+密码（纯数字；账号由管理员在后台手动添加，没有注册）
//! - 公开 API 一律 DTO 白名单，绝不下发密码、抖音号、全量候选名单
//!
//! 拆分（2026-10-12）：按业务域分文件，对外路径 `public_teams::*` 不变。

use crate::dialect;
use crate::pgrs;
use crate::routes::ApiError;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
pub(crate) type ApiResult = Result<Json<Value>, ApiError>;
pub(crate) type ArcState = Arc<AppState>;
/// 公开端一律读「当前生效数据源」，不能写死某个源 id。
/// 早先版本在这里硬编码 `LOCAL`，结果把源切到 A（sqlpub）或直接只配 A 时，
/// 展示页全部接口报「未知数据源 LOCAL」——页面看着是空的，库里其实有数据。
pub(crate) fn sid(st: &AppState) -> String {
    st.active_id()
}

/// 「我的」系列接口（`/api/public/teams/{team}/my*`）的**团别门**。
///
/// 口径（2026-10-16 修「我的页登录死循环」）：
/// - 路径团别 `all`：恒放行（`/my` 的数据本来就只按 token 里的 person_id 取，与团别无关）；
/// - token 团别**为空** = 「团别待定 / 可看全团」：放行任何路径团别。
///   这是 `view_team=any` 的账号（如现在的总管理员「浩雨」）登录后的正常形态 ——
///   登录下发 `team=""`，前端随后套用上次选的视角（男团/女团）请求 `/teams/male/my/*`。
///   旧写法 `my_team != team && team != "all"` 把空团别判成「不属于本团」直接 401，
///   前端把 401 当登录态失效 → 清 token → 弹登录门 → 登录成功又 reload → **无限循环**。
/// - 其余：必须与 token 团别一致（钉死某团的账号不能看别的团）。
///
/// 与 `admin::data_team`（空 = 用路径团别）和族谱 `site.rs` 里那条判断同一口径。
pub(crate) fn my_team_allows(my_team: &str, path_team: &str) -> bool {
    path_team == "all" || my_team.is_empty() || my_team == path_team
}

pub mod admin;
mod avatars;
mod binds;
mod minutes;
mod my;
mod pages;
mod settings;
mod site;
mod wx;

pub use admin::*;
pub use avatars::*;
pub use binds::*;
pub use minutes::*;
pub use my::*;
pub use pages::*;
pub use settings::*;
pub use site::*;
pub use wx::*;

#[cfg(test)]
mod team_gate_tests {
    use super::my_team_allows;

    /// 「我的」团别门：空团别（团别待定 / view_team=any）必须放行任何路径团别 ——
    /// 这是「我的」页登录死循环的根因回归（旧写法把空团别判成不属于本团 → 401 → 前端清 token 弹门）。
    #[test]
    fn my_team_allows_empty_token_team_is_any() {
        // 空 token 团别（总管理员 / 团别待定）= 任意团都放行
        assert!(my_team_allows("", "all"));
        assert!(my_team_allows("", "male"));
        assert!(my_team_allows("", "female"));
        // 路径 all 恒放行（/my 数据只按 token 里的 person_id 取，与团别无关）
        assert!(my_team_allows("male", "all"));
        assert!(my_team_allows("female", "all"));
        // 钉死某团的账号：只放行本团
        assert!(my_team_allows("male", "male"));
        assert!(my_team_allows("female", "female"));
        assert!(!my_team_allows("male", "female"));
        assert!(!my_team_allows("female", "male"));
        // 未知/脏团别串不越权
        assert!(!my_team_allows("male", "bogus"));
    }
}
