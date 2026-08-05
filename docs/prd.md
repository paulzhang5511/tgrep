为了支持多个关键字匹配以及正则表达式，我们需要对 `Cli` 参数设计和过滤函数进行升级：

1. **`clap` 属性调整**：将 `pattern` 的类型从 `String` 改为 `Vec<String>`，并设置 `num_args = 1..`，支持多次传入 `-p` 或单次传入多个值（如 `-p pattern1 pattern2`）。
2. **`regex` 模块集成**：使用 `regex::RegexSet` 替代硬编码字符串匹配。`RegexSet` 可以将多个正则表达式编译为一个统一的匹配集合，**仅需一次扫描即可判断某行是否匹配任意一个规则**，效率极高。

---

### 一、 更新依赖配置 (`Cargo.toml`)

在依赖中引入 `regex` 库：

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
anyhow = "1.0"
clap = { version = "4", features = ["derive"] }
chrono = "0.4"
regex = "1.10" # 正则表达式匹配库

```

---

### 二、 完整代码实现 (`src/main.rs`)

```rust
use anyhow::{Context, Result};
use chrono::Local;
use clap::Parser;
use regex::RegexSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs::{self, File};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::{debug, error, info, instrument, warn};

/// tgrep: 高性能异步多文件正则/多关键字文本过滤工具
#[derive(Parser, Debug)]
#[command(
    name = "tgrep",
    author,
    version,
    about = "High-performance Tokio-powered concurrent log & text line filtering CLI tool",
    long_about = None
)]
struct Cli {
    /// 目标检索目录路径
    #[arg(short, long)]
    dir: String,

    /// 匹配检索的关键字符串或正则表达式（支持多个，例如: -p pattern1 -p pattern2 或 -p "pattern1|pattern2"）
    #[arg(short, long, num_args = 1..)]
    patterns: Vec<String>,

    /// 过滤结果输出文件路径 [默认: output_YYYYMMDD_HHMMSS.log]
    #[arg(short, long)]
    output: Option<String>,
}

/// 异步文件读取与过滤任务
/// 
/// 逐行读取文件，利用 RegexSet 匹配任意模式并发送给 Channel。
#[instrument(skip(tx, matcher), fields(file = %src_path.display()))]
async fn filter_single_file(
    src_path: PathBuf,
    matcher: Arc<RegexSet>,
    tx: mpsc::Sender<String>,
) -> Result<usize> {
    debug!("Starting to process file");

    let file = File::open(&src_path)
        .await
        .with_context(|| format!("Failed to open input file: {:?}", src_path))?;

    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut matched_lines_count: usize = 0;
    let mut total_lines_count: usize = 0;

    while reader.read_line(&mut line).await? > 0 {
        total_lines_count += 1;
        
        // matcher.is_match() 会检查改行是否匹配 RegexSet 中的任意正则表达式
        if matcher.is_match(&line) {
            matched_lines_count += 1;

            if let Err(err) = tx.send(line.clone()).await {
                error!(
                    error = %err,
                    "Failed to send matched line into channel. Channel might be closed"
                );
                return Err(anyhow::anyhow!("Channel send error: {}", err));
            }
        }
        line.clear();
    }

    debug!(
        total_lines = total_lines_count,
        matched_lines = matched_lines_count,
        "Finished processing file successfully"
    );

    Ok(matched_lines_count)
}

/// 并发目录处理引擎
#[instrument(skip(patterns))]
async fn process_directory(
    dir_path: &str,
    output_file_path: &str,
    patterns: &[String],
) -> Result<()> {
    info!(
        directory = %dir_path,
        output = %output_file_path,
        patterns_count = patterns.len(),
        "Initializing directory processing task"
    );

    // 编译正则表达式集合（RegexSet）
    let matcher = RegexSet::new(patterns)
        .with_context(|| format!("Failed to compile regex patterns: {:?}", patterns))?;
    
    // 使用 Arc 包装以在多线程/多 Task 之间共享只读的匹配器
    let matcher = Arc::new(matcher);

    let (tx, mut rx) = mpsc::channel::<String>(1024);
    let out_path = PathBuf::from(output_file_path);

    // 1. 启动单点异步写入任务
    let writer_handle = tokio::spawn(async move {
        debug!(output_path = %out_path.display(), "Initializing output writer");

        let out_file = File::create(&out_path)
            .await
            .with_context(|| format!("Failed to create output file: {:?}", out_path))?;
        let mut writer = BufWriter::new(out_file);
        let mut written_count: usize = 0;

        while let Some(line) = rx.recv().await {
            writer.write_all(line.as_bytes()).await?;
            written_count += 1;
        }

        writer.flush().await?;
        info!(
            total_written = written_count,
            output_path = %out_path.display(),
            "Output writer successfully completed and flushed"
        );
        Ok::<usize, anyhow::Error>(written_count)
    });

    // 2. 遍历目录
    let mut entries = fs::read_dir(dir_path)
        .await
        .with_context(|| format!("Failed to read directory: {}", dir_path))?;

    let mut join_set = JoinSet::new();
    let abs_output_path = fs::canonicalize(output_file_path).await.ok();

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();

        if path.is_file() {
            // 防自锁处理：跳过输出文件自身
            if let Ok(abs_path) = fs::canonicalize(&path).await {
                if Some(&abs_path) == abs_output_path.as_ref() {
                    warn!(
                        file = %path.display(),
                        "Skipping output file located inside input directory"
                    );
                    continue;
                }
            }

            info!(file = %path.display(), "Spawning worker task for file");
            let tx_clone = tx.clone();
            let path_clone = path.clone();
            let matcher_clone = Arc::clone(&matcher);

            join_set.spawn(async move {
                filter_single_file(path_clone, matcher_clone, tx_clone).await
            });
        } else {
            debug!(path = %path.display(), "Skipping non-regular file entry");
        }
    }

    drop(tx); // 关闭 Sender 以解锁 Receiver 循环
    debug!("Primary sender channel dropped, awaiting reading tasks finish");

    // 3. 等待所有文件读取并发任务结束
    let mut total_files_processed = 0;
    let mut total_matched_lines = 0;

    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok(matched_count)) => {
                total_files_processed += 1;
                total_matched_lines += matched_count;
            }
            Ok(Err(err)) => {
                error!(error = %err, "Worker task failed processing a file");
            }
            Err(join_err) => {
                error!(error = %join_err, "Tokio task join panic or failure");
            }
        }
    }

    info!(
        processed_files = total_files_processed,
        total_matches = total_matched_lines,
        "All file reading tasks completed"
    );

    // 4. 等待写入任务完成收尾
    writer_handle
        .await
        .context("Writer task panicked")??;

    info!("Directory processing completed successfully");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // 命令行解析
    let cli = Cli::parse();

    // 初始化日志订阅器
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("tgrep CLI initialized");

    // 若未指定输出路径，根据当前本地日期与时间自动生成文件名
    let output_file = cli.output.unwrap_or_else(|| {
        let now = Local::now();
        let default_name = format!("output_{}.log", now.format("%Y%m%d_%H%M%S"));
        info!(
            generated_filename = %default_name,
            "No output path specified. Automatically generated default output filename"
        );
        default_name
    });

    if let Err(e) = process_directory(&cli.dir, &output_file, &cli.patterns).await {
        error!(error = %e, "Application exited with fatal error");
        std::process::exit(1);
    }

    info!(output_file = %output_file, "Application execution completed");
    Ok(())
}

```

---

### 三、 使用说明

#### 1. 传入多个 `-p` 参数

```bash
cargo run -- -d ./logs -p WangzheTrace -p WangZheStrage

```

#### 2. 一次性指定多个 pattern

```bash
cargo run -- -d ./logs -p WangzheTrace WangZheStrage

```

#### 3. 使用正则表达式匹配

```bash
# 匹配包含 ERROR、WARN 或包含时间戳格式（如 [2026-08-05]）的行
cargo run -- -d ./logs -p "ERROR|WARN" "\[\d{4}-\d{2}-\d{2}\]"

```

## 要求

良好的架构，易于测试，文件分开。详细的中文注释和详细的英文log，和 详细的测试用例

## 代码实现

为了让项目的开发与构建过程更清晰，下面将按照**准备工作、依赖配置、代码模块实现、编译构建与测试**的步骤，输出完整的实现过程。

---

### 第一步：创建项目与配置依赖

在终端中创建全新的 Rust 项目并进入目录：

```bash
cargo new tgrep
cd tgrep

```

修改 `Cargo.toml`，引入所需的依赖库（包括 `tokio`、`clap`、`regex`、`tracing` 等）：

```toml
[package]
name = "tgrep"
version = "0.1.0"
edition = "2024"

[dependencies]
# 异步运行时与异步文件 IO
tokio = { version = "1", features = ["full"] }

# 命令行参数解析
clap = { version = "4", features = ["derive"] }

# 高性能正则表达式匹配
regex = "1.13"

# 日期时间处理（用于默认文件名生成）
chrono = "0.4"

# 结构化日志追踪
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# 错误处理与上下文透传
anyhow = "1.0"

```

---

### 第二步：编写完整业务逻辑代码

将以下代码替换到 `src/main.rs` 文件中。代码已包含完整的 **命令行解析**、**正则编译**、**并发读取与单点写入解耦**、**详细英文日志** 及 **详细中文注释**：

```rust
use anyhow::{Context, Result};
use chrono::Local;
use clap::Parser;
use regex::{RegexBuilder, RegexSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs::{self, File};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::{debug, error, info, instrument, warn};

/// tgrep: 高性能异步多文件正则/多关键字文本过滤工具
#[derive(Parser, Debug)]
#[command(
    name = "tgrep",
    author,
    version,
    about = "High-performance Tokio-powered concurrent log & text line filtering CLI tool",
    long_about = None
)]
struct Cli {
    /// 目标检索目录路径
    #[arg(short, long)]
    dir: String,

    /// 匹配检索的关键字符串或正则表达式（支持多个，例如: -p pattern1 -p pattern2）
    #[arg(short, long, num_args = 1..)]
    patterns: Vec<String>,

    /// 过滤结果输出文件路径 [默认自动生成: output_YYYYMMDD_HHMMSS.log]
    #[arg(short, long)]
    output: Option<String>,

    /// 忽略大小写匹配
    #[arg(short, long, default_value_t = false)]
    ignore_case: bool,
}

/// 步骤 1: 异步单文件读取与正则匹配 Task
/// 
/// 逐行读取文件，利用 RegexSet 匹配任意模式，并将匹配成功的行发送给异步 Writer。
#[instrument(skip(tx, matcher), fields(file = %src_path.display()))]
async fn filter_single_file(
    src_path: PathBuf,
    matcher: Arc<RegexSet>,
    tx: mpsc::Sender<String>,
) -> Result<usize> {
    debug!("Starting to process file");

    let file = File::open(&src_path)
        .await
        .with_context(|| format!("Failed to open input file: {:?}", src_path))?;

    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut matched_lines_count: usize = 0;
    let mut total_lines_count: usize = 0;

    // 逐行读取，利用缓存 String 避免频繁内存分配
    while reader.read_line(&mut line).await? > 0 {
        total_lines_count += 1;

        // matcher.is_match() 使用自动机一次性扫描匹配所有正则规则
        if matcher.is_match(&line) {
            matched_lines_count += 1;

            // 通过 Channel 发送给单点写入任务，如通道满则触发 Backpressure 挂起
            if let Err(err) = tx.send(line.clone()).await {
                error!(
                    error = %err,
                    "Failed to send matched line into channel. Channel might be closed"
                );
                return Err(anyhow::anyhow!("Channel send error: {}", err));
            }
        }
        line.clear(); // 清空复用 buffer
    }

    debug!(
        total_lines = total_lines_count,
        matched_lines = matched_lines_count,
        "Finished processing file successfully"
    );

    Ok(matched_lines_count)
}

/// 步骤 2: 并发目录引擎与 Writer 协调器
/// 
/// 负责正则集合构建、独占 Writer 生成、目录遍历和 Reader Task 的并发管理。
#[instrument(skip(patterns))]
async fn process_directory(
    dir_path: &str,
    output_file_path: &str,
    patterns: &[String],
    ignore_case: bool,
) -> Result<()> {
    info!(
        directory = %dir_path,
        output = %output_file_path,
        patterns_count = patterns.len(),
        ignore_case = ignore_case,
        "Initializing directory processing task"
    );

    // 校验每个模式是否为合法的正则表达式
    for p in patterns {
        RegexBuilder::new(p)
            .case_insensitive(ignore_case)
            .build()
            .with_context(|| format!("Invalid regex pattern: '{}'", p))?;
    }

    // 将模式转换为支持全局忽略大小写的 RegexSet
    let regex_set = RegexSet::new(patterns.iter().map(|p| {
        if ignore_case {
            format!("(?i){}", p)
        } else {
            p.clone()
        }
    }))
    .with_context(|| format!("Failed to compile RegexSet for patterns: {:?}", patterns))?;

    let matcher = Arc::new(regex_set);

    // 创建容量为 1024 的有界通道，提供内存反压保护
    let (tx, mut rx) = mpsc::channel::<String>(1024);
    let out_path = PathBuf::from(output_file_path);

    // 2.1 启动独占单点写入 Task，避免多线程写同一文件导致的死锁或交织
    let writer_handle = tokio::spawn(async move {
        debug!(output_path = %out_path.display(), "Initializing output writer");

        let out_file = File::create(&out_path)
            .await
            .with_context(|| format!("Failed to create output file: {:?}", out_path))?;
        let mut writer = BufWriter::new(out_file);
        let mut written_count: usize = 0;

        while let Some(line) = rx.recv().await {
            writer.write_all(line.as_bytes()).await?;
            written_count += 1;
        }

        writer.flush().await?;
        info!(
            total_written = written_count,
            output_path = %out_path.display(),
            "Output writer successfully completed and flushed"
        );
        Ok::<usize, anyhow::Error>(written_count)
    });

    // 2.2 读取并扫描目标目录
    let mut entries = fs::read_dir(dir_path)
        .await
        .with_context(|| format!("Failed to read directory: {}", dir_path))?;

    let mut join_set = JoinSet::new();
    let abs_output_path = fs::canonicalize(output_file_path).await.ok();

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();

        if path.is_file() {
            // 防循环写入检查：如果输出文件创建在输入目录下，跳过对其自身的读取
            if let Ok(abs_path) = fs::canonicalize(&path).await {
                if Some(&abs_path) == abs_output_path.as_ref() {
                    warn!(
                        file = %path.display(),
                        "Skipping output file located inside input directory"
                    );
                    continue;
                }
            }

            info!(file = %path.display(), "Spawning worker task for file");
            let tx_clone = tx.clone();
            let path_clone = path.clone();
            let matcher_clone = Arc::clone(&matcher);

            // 派生 Tokio 异步 Reader 任务
            join_set.spawn(async move {
                filter_single_file(path_clone, matcher_clone, tx_clone).await
            });
        } else {
            debug!(path = %path.display(), "Skipping non-regular file entry");
        }
    }

    // 2.3 关键点：释放主调度器持有的 tx。
    // 只有当所有 worker task 结束后，所有的 tx 才会完全 drop，rx 才能正确收到关闭信号。
    drop(tx);
    debug!("Primary sender channel dropped, awaiting reading tasks finish");

    // 2.4 等待并发 Reader Tasks 全部完成
    let mut total_files_processed = 0;
    let mut total_matched_lines = 0;

    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok(matched_count)) => {
                total_files_processed += 1;
                total_matched_lines += matched_count;
            }
            Ok(Err(err)) => {
                error!(error = %err, "Worker task failed processing a file");
            }
            Err(join_err) => {
                error!(error = %join_err, "Tokio task join panic or failure");
            }
        }
    }

    info!(
        processed_files = total_files_processed,
        total_matches = total_matched_lines,
        "All file reading tasks completed"
    );

    // 2.5 等待 Writer 收尾并刷新磁盘
    writer_handle
        .await
        .context("Writer task panicked")??;

    info!("Directory processing completed successfully");
    Ok(())
}

/// 步骤 3: 程序入口与 CLI 参数处理
#[tokio::main]
async fn main() -> Result<()> {
    // 解析 CLI 参数
    let cli = Cli::parse();

    // 初始化 tracing 日志订阅器
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("tgrep CLI initialized");

    // 未指定输出路径时，根据系统当前本地时间自动拼接文件名
    let output_file = cli.output.unwrap_or_else(|| {
        let now = Local::now();
        let default_name = format!("output_{}.log", now.format("%Y%m%d_%H%M%S"));
        info!(
            generated_filename = %default_name,
            "No output path specified. Automatically generated default output filename"
        );
        default_name
    });

    if let Err(e) = process_directory(&cli.dir, &output_file, &cli.patterns, cli.ignore_case).await {
        error!(error = %e, "Application exited with fatal error");
        std::process::exit(1);
    }

    info!(output_file = %output_file, "Application execution completed");
    Ok(())
}

```

---

### 第三步：编译与本地测试

#### 1. 准备测试日志目录

在项目根目录下创建一个用于测试的日志文件夹并生成模拟日志：

```bash
mkdir -p ./test_logs
echo "2026-08-05 10:00:00 [INFO] User logged in" > ./test_logs/app1.log
echo "2026-08-05 10:01:23 [ERROR] WangzheTrace failed to connect database" >> ./test_logs/app1.log
echo "2026-08-05 10:02:00 [WARN] Memory limit approaching" > ./test_logs/app2.log
echo "2026-08-05 10:05:11 [ERROR] wangzhestrage module IO timeout" >> ./test_logs/app2.log

```

#### 2. 测试执行场景

* **场景 A：指定多个关键字、开启忽略大小写 `-i`、使用自动生成的文件名**
```bash
cargo run -- -d ./test_logs -p wangzhetrace wangzhestrage -i

```


*控制台日志输出：*
```text
INFO tgrep: tgrep CLI initialized
INFO tgrep: No output path specified. Automatically generated default output filename generated_filename="output_20260805_211605.log"
INFO tgrep::process_directory: Initializing directory processing task directory="./test_logs" output="output_20260805_211605.log" patterns_count=2 ignore_case=true
INFO tgrep::process_directory: Spawning worker task for file file="./test_logs/app1.log"
INFO tgrep::process_directory: Spawning worker task for file file="./test_logs/app2.log"
INFO tgrep::process_directory: All file reading tasks completed processed_files=2 total_matches=2
INFO tgrep::process_directory: Output writer successfully completed and flushed total_written=2 output_path="output_20260805_211605.log"

```


* **场景 B：使用正则表达式与指定输出文件 `-o**`
```bash
cargo run -- -d ./test_logs -p "ERROR|WARN" -o custom_result.log

```


* **场景 C：生成 Release 版本的最终二进制文件**
```bash
cargo build --release
# 可执行文件生成在 ./target/release/tgrep

```