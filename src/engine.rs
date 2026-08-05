//! 并发过滤引擎：
//! - 第一步：收集文件（可选 BFS 递归），预先创建输出文件并 canonicalize 防自锁
//! - 第二步：编译多模式 MatchSet
//! - 第三步：启动单点异步 Writer（独占写输出文件）+ JoinSet 并发 Reader 匹配
//! - 第四步：drop(tx) 解锁 Writer，等待所有任务结束并统计

use anyhow::{Context, Result};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs::{self, File};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::{debug, error, info, instrument, warn};

use crate::matcher::MatchSet;

/// 有界 Channel 容量：在 Reader 产出远快于 Writer 写入时，提供内存反压保护。
const DEFAULT_CHANNEL_CAPACITY: usize = 1024;

/// 传入 `engine::run` 的配置。字段全部 owned，便于跨 task move。
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// 目标检索目录路径
    pub dir: String,
    /// 过滤结果输出文件路径（由 main 负责默认文件名生成后传入）
    pub output: PathBuf,
    /// 匹配检索的关键字/正则表达式集合（至少 1 项，要求在上层已校验）
    pub patterns: Vec<String>,
    /// 是否忽略大小写
    pub ignore_case: bool,
    /// 是否递归扫描所有子目录
    pub recursive: bool,
}

/// `engine::run` 的返回统计。
#[derive(Debug, Default, Clone, Copy)]
pub struct EngineStats {
    /// 实际 spawn 并完成处理的文件数（排除了跳过项，如输出文件自身）
    pub files_processed: usize,
    /// 所有文件匹配到的总行数（= Writer 实际写入行数）
    pub total_matches: usize,
}

/// 执行一次 tgrep 过滤。
///
/// 典型顺序（与 [plan.md](../docs/plan.md) Task 5 一致）：
/// 1. 预创建 `cfg.output`（空文件或截断），以便 `canonicalize` 拿到真实绝对路径用于防自锁
/// 2. 收集所有待处理文件路径（recursive ? BFS : 单级 read_dir）
/// 3. `MatchSet::compile`
/// 4. 启动单 Writer task + 多 Reader tasks
/// 5. drop(tx)，聚合 JoinSet，等待 Writer 收尾 flush
#[instrument(name = "engine_run", skip_all, fields(
    directory = %cfg.dir,
    output = %cfg.output.display(),
    patterns_count = cfg.patterns.len(),
    ignore_case = cfg.ignore_case,
    recursive = cfg.recursive,
))]
pub async fn run(cfg: EngineConfig) -> Result<EngineStats> {
    info!("Initializing directory processing task");

    // ------------------------------------------------------------
    // 1. 预创建空的输出文件，为后续 canonicalize 防自锁判定提供真实路径
    // ------------------------------------------------------------
    File::create(&cfg.output).await.with_context(|| {
        format!(
            "Failed to pre-create/truncate output file: {:?}",
            cfg.output
        )
    })?;
    let abs_output = fs::canonicalize(&cfg.output)
        .await
        .with_context(|| format!("Failed to canonicalize output path: {:?}", cfg.output))
        .ok(); // 某些文件系统下 canonicalize 失败时退化为不做自锁过滤（warn! 由调用方决定是否提示）
    if let Some(ref p) = abs_output {
        debug!(abs_output = %p.display(), "Output file canonicalized for self-lock prevention");
    }

    // ------------------------------------------------------------
    // 2. 收集所有待处理文件路径
    // ------------------------------------------------------------
    let files = collect_files(Path::new(&cfg.dir), cfg.recursive, abs_output.as_deref()).await?;
    info!(
        total_files_collected = files.len(),
        "Finished collecting candidate files"
    );

    // ------------------------------------------------------------
    // 3. 编译 MatchSet（逐模式校验 + 组装 RegexSet）
    // ------------------------------------------------------------
    let matcher = MatchSet::compile(&cfg.patterns, cfg.ignore_case)?;
    let matcher = Arc::new(matcher);
    debug!("MatchSet compiled successfully");

    // ------------------------------------------------------------
    // 4. 启动单点写入 Task（独占写输出文件）
    // ------------------------------------------------------------
    let (tx, mut rx) = mpsc::channel::<String>(DEFAULT_CHANNEL_CAPACITY);
    let out_path_for_writer = cfg.output.clone();
    let writer_handle = tokio::spawn(async move {
        debug!(output_path = %out_path_for_writer.display(), "Initializing output writer");

        // 上面已经预 create 过一次，这里再次 create = O_TRUNC，等价：文件已空，直接写即可。
        let out_file = File::create(&out_path_for_writer).await.with_context(|| {
            format!(
                "Failed to create output file for writer: {:?}",
                out_path_for_writer
            )
        })?;
        let mut writer = BufWriter::new(out_file);
        let mut written_count: usize = 0;

        while let Some(line) = rx.recv().await {
            writer.write_all(line.as_bytes()).await.with_context(|| {
                format!(
                    "Failed to write line #{}. output: {:?}",
                    written_count + 1,
                    out_path_for_writer
                )
            })?;
            written_count += 1;
        }

        writer.flush().await.with_context(|| {
            format!("Failed to flush output writer to {:?}", out_path_for_writer)
        })?;
        info!(
            total_written = written_count,
            output_path = %out_path_for_writer.display(),
            "Output writer successfully completed and flushed"
        );
        Ok::<usize, anyhow::Error>(written_count)
    });

    // ------------------------------------------------------------
    // 5. 为每个文件 spawn 并发 Reader Task
    // ------------------------------------------------------------
    let mut join_set = JoinSet::new();
    for file_path in files {
        info!(file = %file_path.display(), "Spawning worker task for file");
        let tx_clone = tx.clone();
        let matcher_clone = Arc::clone(&matcher);
        join_set.spawn(async move { filter_single_file(file_path, matcher_clone, tx_clone).await });
    }

    // 释放主调度器持有的 tx。只有当所有 worker task 结束后，
    // 所有的 tx 才会完全 drop，rx 才能正确收到关闭信号。
    drop(tx);
    debug!("Primary sender channel dropped, awaiting reading tasks finish");

    // ------------------------------------------------------------
    // 6. 聚合 Reader Task 统计
    // ------------------------------------------------------------
    let mut stats = EngineStats::default();
    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok(matched_count)) => {
                stats.files_processed += 1;
                stats.total_matches += matched_count;
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
        processed_files = stats.files_processed,
        total_matches = stats.total_matches,
        "All file reading tasks completed"
    );

    // ------------------------------------------------------------
    // 7. 等待 Writer 收尾并返回统计
    // ------------------------------------------------------------
    let _written = writer_handle
        .await
        .context("Writer task panicked")?
        .context("Writer task returned an IO or flush error")?;
    // written 应等于 total_matches（语义上 1:1），debug 下可断言，release 下信任统计。
    debug_assert_eq!(
        _written, stats.total_matches,
        "Writer written count != aggregated matched count"
    );

    info!("Directory processing completed successfully");
    Ok(stats)
}

/// 收集目录下所有需要处理的文件路径（已过滤掉输出文件自身）。
///
/// - `recursive = false`：只收集 `root` 直接子文件
/// - `recursive = true`：用 `VecDeque` 做 BFS，把所有子目录 push_back 再逐个 pop_front 读
/// - 所有 `canonicalize(&path) == Some(abs_output)` 的文件会被 warn 并跳过（防自锁）
/// - 任一级 `read_dir` 失败：`warn!` + 跳过该子树（不终止全局）
#[instrument(skip(abs_output))]
async fn collect_files(
    root: &Path,
    recursive: bool,
    abs_output: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut queue = VecDeque::new();
    queue.push_back(root.to_path_buf());

    while let Some(dir) = queue.pop_front() {
        let mut entries = match fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(err) => {
                warn!(
                    directory = %dir.display(),
                    error = %err,
                    "Failed to read directory, skipping this subtree"
                );
                continue;
            }
        };
        loop {
            let entry_opt = match entries.next_entry().await {
                Ok(opt) => opt,
                Err(err) => {
                    warn!(
                        directory = %dir.display(),
                        error = %err,
                        "Error iterating directory entries, continuing with next"
                    );
                    continue;
                }
            };
            let entry = match entry_opt {
                Some(e) => e,
                None => break, // 当前目录读完了，退出 loop
            };
            let path = entry.path();
            let is_file = match path.try_exists() {
                Ok(true) => path.is_file(),
                Ok(false) => {
                    debug!(path = %path.display(), "Path vanished between read_dir and stat, skipping");
                    continue;
                }
                Err(err) => {
                    warn!(path = %path.display(), error = %err, "Failed to stat path, skipping");
                    continue;
                }
            };

            if is_file {
                if let Some(out_abs) = abs_output
                    && let Ok(file_abs) = fs::canonicalize(&path).await
                    && file_abs.as_path() == out_abs
                {
                    warn!(
                        file = %path.display(),
                        "Skipping output file located inside input directory"
                    );
                    continue;
                }
                out.push(path);
            } else if recursive && path.is_dir() {
                debug!(path = %path.display(), "Queuing directory for recursive scan");
                queue.push_back(path);
            } else if recursive {
                debug!(path = %path.display(), "Skipping non-regular / non-dir entry");
            }
        }
    }

    Ok(out)
}

/// 异步单文件读取 + 行匹配 Task。
///
/// 逐行读取文件，命中任意模式时将行内容通过 `tx` 发送给 Writer；
/// 读取完毕后返回本文件命中的行数。
#[instrument(skip(tx, matcher), fields(file = %src_path.display()))]
async fn filter_single_file(
    src_path: PathBuf,
    matcher: Arc<MatchSet>,
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

    // 复用同一个 String buffer，避免每行重新分配内存。
    while reader.read_line(&mut line).await? > 0 {
        total_lines_count += 1;

        // matcher.is_match() 使用自动机一次性扫描匹配所有正则规则（OR 语义）
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
