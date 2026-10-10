//! pk-server 库入口：模块以 pub 暴露给集成测试与二进制。

pub mod admin;
pub mod auth;
pub mod bot;
pub mod config;
pub mod csvparse;
pub mod dialect;
pub mod events;
pub mod grouping;
pub mod matching;
pub mod metrics;
pub mod migrate;
pub mod mysql;
pub mod public;
pub mod public_teams;
pub mod push;
pub mod report;
pub mod repos;
pub mod roster_seed;
pub mod routes;
pub mod spa;
pub mod state;
pub mod store;
pub mod wbgateway;
pub mod writer;
