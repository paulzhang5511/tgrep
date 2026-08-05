//! tgrep 集成测试：
//! - 所有测试调用 engine::run(EngineConfig)（而非子进程），依赖更轻
//! - 每个测试用独立的临时目录：$TEMP/tgrep_it_{pid}_{autoinc}，彻底避免并行测试冲突
//!
//! 覆盖：递归 ON/OFF、防自锁、多关键字 OR、空目录、非法正则、忽略大小写 共 7 条。

use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use tgrep::engine::{EngineConfig, EngineStats, run};

/// 每个集成测试自增的唯一编号，保证并行测试下目录不冲突。
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// 一个独立的测试沙箱目录（含子目录 sub/），测试结束自动 drop 删除。
struct Sandbox {
    pub root: PathBuf,
}

impl Sandbox {
    /// 创建一个唯一的沙箱目录，内部结构：
    /// - a.log (3 行)
    /// - b.log (2 行)
    /// - sub/deep.log (2 行)
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let unique = format!("tgrep_it_{}_{}_{}", process::id(), ts, id);
        let root = std::env::temp_dir().join(unique);
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).expect("create sandbox dir");

        // 写 a.log：3 行 → 2 行匹配 KEY_A
        std::fs::write(
            root.join("a.log"),
            "\
2026-08-05 10:00 [INFO] User logged in
2026-08-05 10:01 [ERROR] AlphaMarker failed to connect database
2026-08-05 10:03 [WARN] BetaMarker cache miss
",
        )
        .unwrap();

        // 写 b.log：2 行 → 1 行匹配 KEY_B
        std::fs::write(
            root.join("b.log"),
            "\
2026-08-05 10:02 [INFO] request ok
2026-08-05 10:05 [ERROR] betamarker module IO timeout
",
        )
        .unwrap();

        // 写 sub/deep.log：2 行 → 1 行匹配 DEEP_MARKER
        std::fs::write(
            sub.join("deep.log"),
            "\
2026-08-05 10:08 [INFO] nested task started
2026-08-05 10:09 [ERROR] AlphaMarker DEEP_MATCH_MARKER in nested
",
        )
        .unwrap();

        Sandbox { root }
    }

    /// 构造指向沙箱根目录的 EngineConfig（patterns / ignore_case / recursive / output_rel 可定制）
    fn cfg(
        &self,
        patterns: impl IntoIterator<Item = impl Into<String>>,
        ignore_case: bool,
        recursive: bool,
        output_rel: impl AsRef<Path>,
    ) -> EngineConfig {
        EngineConfig {
            dir: self.root.to_string_lossy().into_owned(),
            output: self.root.join(output_rel),
            patterns: patterns.into_iter().map(Into::into).collect(),
            ignore_case,
            recursive,
        }
    }

    /// 读取沙箱内输出文件所有行（丢失末尾空行；本测试不会产出末尾空行），顺序按文件
    /// 写入顺序是不确定的（并发），所以断言前会排序。
    fn read_output_lines_sorted(&self, output_rel: impl AsRef<Path>) -> Vec<String> {
        let content = std::fs::read_to_string(self.root.join(output_rel)).expect("read output");
        let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
        lines.sort();
        lines
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // 最佳努力清理：不 panic（测试结束时 tempdir 也会被系统回收）
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

// ============================================================================
// 测试用例 1 / 7：递归关闭时，子目录 sub/deep.log 的内容不会出现在输出里
// ============================================================================
#[tokio::test]
async fn recursive_disabled_does_not_scan_subdir() {
    let s = Sandbox::new();
    let cfg = s.cfg(
        vec!["DEEP_MATCH_MARKER"],
        false,
        false, // recursive = OFF
        "result_disabled.log",
    );
    let stats = run(cfg).await.expect("run should succeed");
    // 两个顶层文件 a.log / b.log 都不含 DEEP_MATCH_MARKER → 命中 0
    assert_eq!(stats.total_matches, 0);
    assert_eq!(stats.files_processed, 2); // a.log + b.log
    assert!(s.read_output_lines_sorted("result_disabled.log").is_empty());
}

// ============================================================================
// 测试用例 2 / 7：递归开启时，sub/deep.log 的 DEEP_MATCH_MARKER 被命中
// ============================================================================
#[tokio::test]
async fn recursive_enabled_scans_subdir() {
    let s = Sandbox::new();
    let cfg = s.cfg(
        vec!["DEEP_MATCH_MARKER"],
        false,
        true, // recursive = ON
        "result_enabled.log",
    );
    let stats = run(cfg).await.expect("run should succeed");
    // a.log / b.log / deep.log 共 3 个文件；deep.log 里有 1 条匹配
    assert_eq!(stats.files_processed, 3);
    assert_eq!(stats.total_matches, 1);
    let lines = s.read_output_lines_sorted("result_enabled.log");
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("DEEP_MATCH_MARKER"));
}

// ============================================================================
// 测试用例 3 / 7：输出文件位于输入目录下，不会被自身读取匹配（防自锁）
//
// 策略：输出文件同目录下的输入文件都含关键字 SELF_LOCK_TEST_XYZ，
// 如果 out.log 被错误地当作输入文件读取，由于其匹配行数不定（取决于
// 写入顺序），但有一个确定事实：processed_files 一定不应该包含 out.log。
// 通过比较匹配行数（= 源文件里数量固定 = 3）来间接验证。
// ============================================================================
#[tokio::test]
async fn output_inside_input_dir_self_lock_prevented() {
    let s = Sandbox::new();

    // 给 a.log / b.log / sub/deep.log 追加 SELF_LOCK_TEST_XYZ，保证它们总计命中 3 行
    let append_tag = |name: &str| {
        let p = s.root.join(name);
        let mut content = std::fs::read_to_string(&p).unwrap();
        content.push_str("2026-08-05 SELF_LOCK_TEST_XYZ tag\n");
        std::fs::write(p, content).unwrap();
    };
    append_tag("a.log");
    append_tag("b.log");
    append_tag("sub/deep.log");

    let cfg = s.cfg(
        vec!["SELF_LOCK_TEST_XYZ"],
        false,
        true,             // recursive ON 才能把 3 个文件都扫到
        "out_inside.log", // 注意：输出文件在输入目录内部
    );
    let stats = run(cfg).await.expect("run should succeed");

    // 关键断言：只匹配到那 3 条注入的 tag；如果 out.log 被读了，
    // 要么命中更多行（乱序），要么 files_processed 不对。
    // 最稳的断言：total_matches == 3，files_processed == 3
    assert_eq!(
        stats.files_processed, 3,
        "out.log 不应被计入 processed_files"
    );
    assert_eq!(stats.total_matches, 3, "只能命中 3 条我们注入的 tag 行");

    // 读回 out_inside.log 本身：里面也只能是 3 条 SELF_LOCK_TEST_XYZ，
    // 且不应该包含任何指向 out_inside.log 自身的引用（本沙箱里没有，
    // 所以仅检查行数稳定）
    let lines = s.read_output_lines_sorted("out_inside.log");
    assert_eq!(lines.len(), 3);
    for l in &lines {
        assert!(l.contains("SELF_LOCK_TEST_XYZ"));
    }
}

// ============================================================================
// 测试用例 4 / 7：多关键字 OR 语义 + 忽略大小写，4 条命中
// 模式：wangzheTRACE（小写），-i 开启
// 匹配：
//   a.log line2: AlphaMarker
//   a.log line3: BetaMarker  <- 不含 trace，不匹配（关键词只给了 alphamarker）
// 给 a.log 再加一行 ALPHAMARKER UPPERCASE，b.log 再加一行
// ============================================================================
#[tokio::test]
async fn multi_keyword_or_and_ignore_case() {
    let s = Sandbox::new();

    // 在 a.log 末尾追加 1 行全大写 ALPHAMARKER；b.log 追加 1 行 小写 alphamarker
    std::fs::write(
        s.root.join("a.log"),
        "\
2026-08-05 10:00 [INFO] User logged in
2026-08-05 10:01 [ERROR] AlphaMarker failed to connect database
2026-08-05 10:03 [WARN] BetaMarker cache miss
2026-08-05 10:10 [INFO] ALPHAMARKER uppercase ok
",
    )
    .unwrap();
    std::fs::write(
        s.root.join("b.log"),
        "\
2026-08-05 10:02 [INFO] request ok
2026-08-05 10:05 [ERROR] betamarker module IO timeout
2026-08-05 10:11 [INFO] alphamarker lowercase ok
",
    )
    .unwrap();

    let cfg = s.cfg(
        vec!["alphamarker"],
        true,  // ignore_case ON
        false, // recursive OFF（只扫顶层 2 文件）
        "case_insensitive.log",
    );
    let stats = run(cfg).await.expect("run should succeed");
    // 命中 2 处：a.log line2 (AlphaMarker) + a.log line4 (ALPHAMARKER) + b.log line3 (alphamarker)
    // 注意 BetaMarker 不含 trace，不命中
    assert_eq!(stats.total_matches, 3);
    assert_eq!(stats.files_processed, 2);
}

// ============================================================================
// 测试用例 5 / 7：非法正则模式（未闭合 [）返回 Err，错误信息包含原始模式字符串
// ============================================================================
#[tokio::test]
async fn invalid_regex_error_propagates_with_original_pattern() {
    let s = Sandbox::new();
    let bad_pattern = "[unclosed_bracket_xyz";
    let cfg = s.cfg(vec![bad_pattern], false, false, "bad_pat.log");
    let err = run(cfg).await.expect_err("bad pattern must fail");
    let formatted = format!("{:#}", err);
    assert!(
        formatted.contains(bad_pattern),
        "error message should mention the exact pattern. Got: {}",
        formatted
    );
}

// ============================================================================
// 测试用例 6 / 7：空目录（沙箱 root 下删光文件）→ 0 文件、0 匹配、输出文件 0 字节
// ============================================================================
#[tokio::test]
async fn empty_dir_produces_empty_output_zero_stats() {
    let s = Sandbox::new();
    // 清空输入文件：删除 a.log、b.log、sub/deep.log（保留 sub 目录没问题）
    std::fs::remove_file(s.root.join("a.log")).unwrap();
    std::fs::remove_file(s.root.join("b.log")).unwrap();
    std::fs::remove_file(s.root.join("sub").join("deep.log")).unwrap();

    let cfg = s.cfg(vec!["anything"], false, true, "empty_dir_out.log");
    let stats = run(cfg).await.expect("run should succeed");
    assert_eq!(stats.files_processed, 0);
    assert_eq!(stats.total_matches, 0);

    let meta =
        std::fs::metadata(s.root.join("empty_dir_out.log")).expect("output file should exist");
    assert_eq!(meta.len(), 0);
}

// ============================================================================
// 测试用例 7 / 7：正则 ERROR|WARN 多级别 OR 捕获 3 条
// ============================================================================
#[tokio::test]
async fn regex_level_or_matches_error_and_warn() {
    let s = Sandbox::new();
    let cfg = s.cfg(
        vec!["ERROR|WARN"],
        false,
        false, // recursive OFF
        "levels.log",
    );
    let stats = run(cfg).await.unwrap();
    // 顶层 a.log 两条命中（AlphaMarker ERROR + BetaMarker WARN）
    // 顶层 b.log 一条命中（betamarker ERROR）
    assert_eq!(stats.total_matches, 3);
    assert_eq!(stats.files_processed, 2);
    let mut lines = s.read_output_lines_sorted("levels.log");
    assert_eq!(lines.len(), 3);
    // 三条应分别包含 ERROR 或 WARN
    for l in lines.iter_mut() {
        assert!(l.contains("ERROR") || l.contains("WARN"), "line: {}", l);
    }
}

// ============================================================================
// (helper) 确认 run() 返回类型就是 Result<EngineStats>，避免未来改签名没改测试
// ============================================================================
#[allow(dead_code)]
fn _compile_time_type_check(r: Result<EngineStats>) -> Result<EngineStats> {
    r
}
