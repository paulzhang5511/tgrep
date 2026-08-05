//! tgrep 库层：被集成测试 `tests/*` 引用。
//! `main.rs` 是 thin binary，仅做 CLI 解析 + 调用 `engine::run`；
//! 所有业务模块在此处重新导出为 `pub`，供库调用方（集成测试）直接使用。

pub mod cli;
pub mod engine;
pub mod logger;
pub mod matcher;
