# Task Checklist: tgrep v0.1 实现任务清单

> 配套文档：[spec.md](./spec.md)（规格）、[plan.md](./plan.md)（架构计划）  
> 执行顺序：自顶向下；每个任务完成后打勾；Checkpoint 处停等人审或继续。

---

## Phase 1: Foundation（基础层：依赖 + CLI + 日志 + Matcher）

- [ ] **Task 1: 配置 Cargo.toml 依赖**
  - **Acceptance:**
    - [ ] `Cargo.toml` 含 6 项依赖（tokio + features=full、clap + derive、regex 1.13、chrono 0.4、tracing 0.1、tracing-subscriber + env-filter、anyhow 1.0），edition=2024
    - [ ] `cargo check`（空 main.rs）退出 0，无 warning，生成 `Cargo.lock`
  - **Verify:**
    - [ ] `cargo check`
    - [ ] `grep -E 'tokio|clap|regex|chrono|tracing|anyhow' Cargo.toml | wc -l` = 6
  - **Files:** `Cargo.toml`
  - **Depends:** None
  - **Size:** XS（1 文件）

- [ ] **Task 2: 实现 src/cli.rs + 单元测试**
  - **Acceptance:**
    - [ ] `#[derive(Parser)] struct Cli` 5 字段齐全：`dir(-d)`、`patterns(-p, num_args=1..)`、`output(Option, -o)`、`ignore_case(-i, 默认 false)`、`recursive(-r, 默认 false)`
    - [ ] 单测断言：`-d logs -p a -p b -i -r -o out.log` 解析出 recursive=true、ignore_case=true、patterns=vec!["a","b"]
    - [ ] 单测断言：不传 `-r -i` 时两者都是 false
    - [ ] 单测断言：`-p foo bar` 单次多值 = vec!["foo","bar"]
    - [ ] 单测断言：完全不传 `-p` → clap 返回 Err
  - **Verify:**
    - [ ] `cargo test --lib cli::tests`
    - [ ] `cargo run -- --help` 显示 `-r/--recursive`、`-i/--ignore-case`
  - **Files:** `src/cli.rs`、`src/main.rs`（加 `mod cli;`）
  - **Depends:** Task 1
  - **Size:** S（1-2 文件）

- [ ] **Task 3: 实现 src/logger.rs**
  - **Acceptance:**
    - [ ] 导出 `pub fn init_logging(default: impl Into<Option<&'static str>>)`
    - [ ] 内部使用 `tracing_subscriber::fmt().with_env_filter(...)`，优先 `RUST_LOG`，否则 fallback 到 default
    - [ ] 使用 `try_init()` 或 `_ =` 忽略二次调用错误，不 panic
  - **Verify:**
    - [ ] `cargo check`
    - [ ] Code review：`try_init` 或等效保护存在
  - **Files:** `src/logger.rs`
  - **Depends:** Task 1
  - **Size:** XS（1 文件 < 40 行）

- [ ] **Task 4: 实现 src/matcher.rs + 完整单元测试**
  - **Acceptance:**
    - [ ] `pub struct MatchSet { inner: Arc<RegexSet> }`，derive(Clone, Debug)
    - [ ] `pub fn compile(patterns: &[String], ignore_case: bool) -> Result<Self>`：逐模式 `RegexBuilder::new(p).case_insensitive(ignore_case).build()` 单独校验；随后拼成 `(?i)` 前缀集合并 RegexSet::new
    - [ ] `pub fn is_match(&self, line: &str) -> bool`
    - [ ] ≥ 5 条单测：①字面量 ②正则 "A\|B" OR ③ignore_case 匹配大小写混合 ④非法正则 `[unclosed` Err 含原字符串 ⑤Clone 后 is_match 依然有效
  - **Verify:**
    - [ ] `cargo test --lib matcher::tests`
    - [ ] `cargo test --lib matcher::tests 2>&1 | grep 'test result: ok'`
  - **Files:** `src/matcher.rs`、`src/main.rs`（加 `mod matcher;`）
  - **Depends:** Task 1
  - **Size:** S（1-2 文件）

---

### 🔴 Checkpoint: Foundation（Tasks 1-4 完成后）
- [ ] `cargo check` 0 warnings
- [ ] `cargo test --lib` 全部通过
- [ ] `cargo run -- --help` 显示 -d/-p/-o/-i/-r 五项
- [ ] 人工：`main.rs` 中 `mod cli; mod logger; mod matcher;` 声明齐全

---

## Phase 2: Core Features（引擎 + 入口）

- [ ] **Task 5: 实现 src/engine.rs 核心引擎**
  - **Acceptance:**
    - [ ] 导出 `pub struct EngineConfig { dir: String, output: PathBuf, patterns: Vec<String>, ignore_case: bool, recursive: bool }`（或用 owned）
    - [ ] 导出 `pub struct EngineStats { files_processed: usize, total_matches: usize }`
    - [ ] 导出 `pub async fn run(cfg: EngineConfig) -> Result<EngineStats>`
    - [ ] **防自锁关键步骤**：收集文件前先 `File::create(&cfg.output).await.with_context(|| ...)?`，再 `fs::canonicalize(&cfg.output).await.ok()` 作为 `abs_output`
    - [ ] **递归实现**：`recursive=true` 用 `VecDeque<PathBuf>` BFS；`false` 单次 read_dir；每个子目录 read_dir 失败 warn! 跳过该子树
    - [ ] **收集 → 比对排除 abs_output → 统一 spawn 到 JoinSet**
    - [ ] 单写 task：`mpsc::channel(1024)` rx → BufWriter → 同一个 `&cfg.output`（File::create 截断是可接受的，因为已经先 create 过）
    - [ ] `filter_single_file(src_path, Arc<MatchSet>, tx)` 内部：`BufReader::new(file).read_line(&mut line)` → `matcher.is_match(&line)` → `tx.send(line.clone())`
    - [ ] 所有 tracing 结构化字段（directory / output / patterns_count / ignore_case / recursive / file / processed_files / total_matches / total_written）英文
    - [ ] 所有 IO 带 `.with_context(|| format!("... {:?}", path))`
  - **Verify:**
    - [ ] `cargo check` 0 warnings
    - [ ] Code review：BFS 用 VecDeque，push_back/pop_front 正确；abs_output 比较用 canonicalize
  - **Files:** `src/engine.rs`、`src/main.rs`（加 `mod engine;`）
  - **Depends:** Tasks 2, 4
  - **Size:** M（1-2 主文件，但内部约 200 行）

- [ ] **Task 6: 完成 src/main.rs 入口拼接**
  - **Acceptance:**
    - [ ] 顺序：`let cli = Cli::parse();` → `logger::init_logging(Some("info"));` → 生成默认 output（若 cli.output.is_none()：`output_{Local::now().format("%Y%m%d_%H%M%S")}.log`，info! 打 generated_filename）→ 组装 EngineConfig → `engine::run(cfg).await`；Err 时 `error!(error = %e, ...); process::exit(1)`；Ok 时 `info!(output_file = %output_file, "Application execution completed")`
    - [ ] main.rs **不含**任何 read_dir / read_line / Regex / BufWriter 代码
    - [ ] 行数 ≤ 100（不含空行和注释的软指标）
  - **Verify:**
    - [ ] 手动：`mkdir -p /tmp/t && echo foo > /tmp/t/a && cargo run -- -d /tmp/t -p foo -o /tmp/out.log` → `/tmp/out.log` 含 "foo"
    - [ ] 手动：`cargo run -- -d NO_SUCH_DIR -p foo ; echo $?` → 非 0
  - **Files:** `src/main.rs`
  - **Depends:** Tasks 3, 5
  - **Size:** S（1 文件）

---

### 🟡 Checkpoint: Core Features（Tasks 5-6 后）
- [ ] `cargo build --release` 0 warnings
- [ ] **手动冒烟 A（忽略大小写）**：`mkdir -p test_logs && printf '2026-08-05 [ERROR] WangzheTrace failed\n2026-08-05 [ERROR] wangzhestrage timeout\nINFO ok\n' > test_logs/a.log`；`cargo run -- -d ./test_logs -p wangzhetrace wangzhestrage -i`；`wc -l output_*.log` = 2
- [ ] **手动冒烟 B（正则）**：`printf 'INFO x\n[ERROR] a\n[WARN] b\n' > test_logs/b.log`；`cargo run -- -d ./test_logs -p "ERROR|WARN" -o result.log`；`grep -cE 'ERROR|WARN' result.log` = 行数匹配
- [ ] **手动冒烟 C（递归 ON/OFF）**：`mkdir -p test_logs/sub && echo 'DEEP MATCH' > test_logs/sub/deep.log`；① 无 `-r`：`grep DEEP <新生成的 output>` 0 行；② 加 `-r`：`grep DEEP <新生成的 output>` 1 行
- [ ] 人审：tracing 输出所有关键字段为英文且含 recursive

---

## Phase 3: Tests & Polish

- [ ] **Task 7: 编写 tests/integration_test.rs 集成测试**
  - **Acceptance:**
    - [ ] 每个 `#[tokio::test]` 使用唯一 temp dir：`std::env::temp_dir().join(format!("tgrep_it_{:x}_{}", std::process::id(), thread_rng 或自增原子))`；避免并行冲突
    - [ ] 构造目录：`a.log(3行→2匹配)` + `b.log(2行→1匹配)` + `sub/deep.log(2行→1匹配)`
    - [ ] ≥ 6 个测试函数：
      1. `recursive_disabled_does_not_scan_subdir`：`recursive=false`，输出中不含 deep.log 内容
      2. `recursive_enabled_scans_subdir`：`recursive=true`，输出含 deep.log 匹配行
      3. `output_inside_input_dir_self_lock`：out.log 放同目录，运行后 out.log **不含**自身（用唯一关键字如 `SELF_LOCK_PROBE_XYZ`，源文件里没有这个词，out.log 作为文件本身也不会有匹配词所以天然不含；强化断言：processed_files 不含 out.log 路径）
      4. `multi_keyword_or_semantics`：2 个 keyword，3 文件匹配行合计 = 预期数
      5. `regex_error_pattern_propagates`：pattern = `[unclosed`，`engine::run` 返回 Err，`format!("{:#}", err)` 含原字符串
      6. `empty_dir_produces_empty_output`：无文件，files_processed=0 且输出文件 size = 0
      7. （可选 bonus）`ignore_case_matches_mixed_case`
  - **Verify:**
    - [ ] `cargo test --test integration_test`
    - [ ] `cargo test --test integration_test -- --test-threads=4` 连续跑 2 次全绿（无 flaky）
  - **Files:** `tests/integration_test.rs`
  - **Depends:** Task 5
  - **Size:** M（1 文件 ≈ 250-350 行）

- [ ] **Task 8: 格式化 + Clippy + Spec 回填验证记录**
  - **Acceptance:**
    - [ ] `cargo fmt --check` exit 0（如有差异先执行 `cargo fmt`）
    - [ ] `cargo clippy -- -D warnings` exit 0；如有 allow 必须在代码上写 `#[allow(...)] // 理由：...`
    - [ ] `cargo test`（unit + integration）全绿
    - [ ] `cargo build --release` 成功；`./target/release/tgrep --help` 正常
    - [ ] 在 [spec.md](./spec.md) **末尾追加** `## Verified:` 段：逐条列出 spec 中 9 条 Success Criteria 的实测状态 + 命令 + 退出码/结果摘要（实测通过打 ✅，不通过打 ❌ 并说明原因，如有 allow lint 在此登记）
  - **Verify:**
    - [ ] 命令序列：`cargo fmt → cargo clippy -- -D warnings → cargo test → cargo build --release`，每条 exit 0
    - [ ] 人工检查 spec.md 的 `## Verified:` 段存在且 9 条都标注
  - **Files:** 全仓源码（格式化）、`docs/spec.md`（追加）
  - **Depends:** Task 7
  - **Size:** M（格式化多文件 + 文档回填）

---

### 🟢 Checkpoint: Complete
- [ ] spec 9 条 Success Criteria 中，实测通过 ≥ 8/9（最多 1 条观感类留人工确认）
- [ ] `docs/spec.md` → `## Verified:` 段完整
- [ ] `docs/plan.md` Risks 中登记过的 4 条风险，各自对应 Mitigation 至少有 1 条代码/测试证据
- [ ] 最终 Review 批准，准备进入实现

---

## 任务依赖速查（DAG）

```
Task1 ─┬─→ Task2 ──┐
       ├─→ Task3 ──┤
       └─→ Task4 ──┴─→ Task5 ──┬─→ Task6 ──┐
                                └───────────┴─→ Task7 ──→ Task8
```

## 可并行窗口（多 agent 模式下可选）
- **并行窗口 1（Task 1 完成后）**：Task 2 || Task 3 || Task 4（三者互不依赖内部实现，只依赖 Cargo.toml）
- **并行窗口 2（Task 5 API 冻结后）**：Task 6（main 入口） || Task 7（集成测试骨架，用已定义的 EngineConfig/EngineStats/run）
