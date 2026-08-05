//! tgrep 二进制入口：仅是一个 thin wrapper，装配 CLI 解析、日志初始化、
//! 默认文件名生成，并调用 `tgrep::engine::run`。
//!
//! 所有业务模块都在库层（src/lib.rs + src/*.rs），便于集成测试直接 `use tgrep::...`。

use anyhow::Result;
use chrono::Local;
use clap::Parser;
use std::path::PathBuf;
use tracing::{error, info};

use tgrep::cli::Cli;
use tgrep::engine::{EngineConfig, EngineStats};
use tgrep::logger::init_logging;

#[tokio::main]
async fn main() -> Result<()> {
    // 1. 解析命令行参数（clap 在 parse 失败时自动打印 help/错误并退出）
    let cli = Cli::parse();

    // 2. 初始化 tracing 日志订阅器（默认 info，RUST_LOG 优先）
    //    放在 parse 之后，避免 --help/--version 场景下多余的日志初始化
    init_logging(Some("info"));
    info!("tgrep CLI initialized");

    // 3. 若未指定输出路径，按本地时间生成 output_YYYYMMDD_HHMMSS.log
    let output_file: PathBuf = match cli.output {
        Some(path) => PathBuf::from(path),
        None => {
            let now = Local::now();
            let generated = format!("output_{}.log", now.format("%Y%m%d_%H%M%S"));
            info!(
                generated_filename = %generated,
                "No output path specified. Automatically generated default output filename"
            );
            PathBuf::from(generated)
        }
    };

    // 4. 组装引擎配置（所有字段 owned，便于跨 task move）
    let engine_cfg = EngineConfig {
        dir: cli.dir,
        output: output_file.clone(),
        patterns: cli.patterns,
        ignore_case: cli.ignore_case,
        recursive: cli.recursive,
    };

    // 5. 运行引擎
    let stats: EngineStats = match tgrep::engine::run(engine_cfg).await {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "Application exited with fatal error");
            // CAUTION: process::exit 绕过任何析构。但这里所有异步任务都已在
            // Err 路径下聚合失败或未启动，没有泄漏的资源。
            std::process::exit(1);
        }
    };

    info!(
        processed_files = stats.files_processed,
        total_matches = stats.total_matches,
        output_file = %output_file.display(),
        "Application execution completed"
    );
    Ok(())
}
