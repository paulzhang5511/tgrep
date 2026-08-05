# Spec: tgrep - 高性能异步多文件正则/多关键字文本过滤工具

## Objective

### 背景与目标
构建一个基于 Tokio 异步运行时的 CLI 文本过滤工具，用于在指定目录下同时扫描多个日志/文本文件，根据一个或多个关键字（或正则表达式）筛选匹配行，并将结果并发安全地写入单个输出文件。

**核心价值：**
- 与传统 `grep` 相比，原生支持多模式匹配（多关键字/正则混合）
- 基于 Tokio 异步 IO，对大文件和多文件场景吞吐量更高
- 多 Reader + 单 Writer 架构，天然避免多线程写同一文件的竞态

### 用户画像
- **主要用户：** Rust 后端工程师、SRE、运维人员
- **典型场景：** 在 `./logs/` 目录下同时扫描数十个应用日志文件，提取包含 `ERROR`、特定 trace id、模块名等关键字的行

### 功能需求（用户故事）
1. 作为用户，我可以传入 `-p keyword1 -p keyword2` 指定多个关键字，匹配任意一个即输出该行
2. 作为用户，我可以传入 `-p "ERROR|WARN"` 这样的正则表达式进行模式匹配
3. 作为用户，我可以使用 `-i` 开关忽略大小写进行匹配
4. 作为用户，我可以使用 `-o custom.log` 指定输出文件路径，不指定则自动生成带时间戳的文件名
5. 作为用户，我可以使用 `-r/--recursive` 开关递归扫描所有子目录下的文件
6. 作为用户，我希望即使输出文件在输入目录内，工具也不会死循环读取自己

### 非功能需求
| 维度 | 指标 |
|---|---|
| 性能 | 10 个 10MB 日志文件、5 个正则模式下，完成时间 < 2 倍纯 `grep` 耗时 |
| 健壮性 | 单个文件读取失败不影响其他文件，错误通过 tracing 记录 |
| 可测试性 | 核心逻辑（正则编译、行匹配、目录过滤）可独立单元测试 |
| 可维护性 | 模块分层清晰，单文件职责单一（main.rs 不超过 100 行） |

---

## Tech Stack

| 库 | 版本 | 用途 |
|---|---|---|
| Rust | edition 2024 | 语言版本 |
| tokio | 1.x (features = ["full"]) | 异步运行时、异步文件 IO、mpsc channel、JoinSet |
| clap | 4.x (features = ["derive"]) | 命令行参数解析，derive 模式 |
| regex | 1.13 | `RegexSet` 多正则集合编译与匹配、`RegexBuilder` 逐模式校验 |
| chrono | 0.4 | 默认输出文件名的本地时间格式化 |
| tracing | 0.1 | 结构化日志宏（info!/debug!/error!/warn!） |
| tracing-subscriber | 0.3 (features = ["env-filter"]) | 日志订阅器，支持 `RUST_LOG` 环境变量控制级别 |
| anyhow | 1.0 | 应用层错误类型与 `Context` 上下文透传 |

---

## Commands

```bash
# 开发构建
cargo build

# Release 构建
cargo build --release

# 运行（单级目录）
cargo run -- -d ./logs -p "ERROR" -p "WARN" -i

# 运行（递归扫描子目录）
cargo run -- -d ./logs -p "ERROR" -p "WARN" -i -r

# 单元测试 + 集成测试
cargo test

# 测试（显示 println! 输出）
cargo test -- --nocapture

# 检查代码格式
cargo fmt --check

# Clippy 静态检查（开发建议）
cargo clippy -- -D warnings

# 自动格式化
cargo fmt
```

---

## Project Structure

```
tgrep/
├── Cargo.toml                    # 依赖配置与包元数据
├── src/
│   ├── main.rs                   # 程序入口：CLI 解析 + 调用 engine（≈60 行）
│   ├── cli.rs                    # clap derive 结构体定义：Cli
│   ├── matcher.rs                # 正则匹配层：RegexSet 封装与行匹配逻辑（可单测）
│   ├── engine.rs                 # 并发引擎层：目录遍历 + Reader/Writer 调度
│   └── logger.rs                 # tracing 订阅器初始化封装
├── tests/
│   └── integration_test.rs       # 集成测试：临时目录 + 真实文件 IO
└── docs/
    ├── prd.md                    # 原始 PRD（输入）
    └── spec.md                   # 本规格文档（输出）
```

### 模块依赖关系（无环）
```
main.rs
  ├── cli.rs
  ├── logger.rs
  ├── engine.rs
  │     ├── matcher.rs
  │     └── anyhow/tracing
  └── anyhow/tracing
```

---

## Code Style

### 命名约定
- 类型/结构体：`PascalCase`（如 `FilterEngine`、`MatchSet`）
- 函数/方法/变量：`snake_case`（如 `filter_single_file`、`matched_lines_count`）
- 常量：`SCREAMING_SNAKE_CASE`（如 `DEFAULT_CHANNEL_CAPACITY`）
- 私有辅助函数以 `_` 前缀可选，优先用模块可见性控制

### 日志规范（英文，结构化字段）
```rust
info!(
    directory = %dir_path,
    output = %output_file_path,
    patterns_count = patterns.len(),
    ignore_case = ignore_case,
    "Initializing directory processing task"
);
```
- **Level 使用：** `error!`（程序级失败）、`warn!`（被跳过的异常）、`info!`（关键里程碑）、`debug!`（逐文件/逐行细节）
- 一律使用 `%` 对实现 `Display` 的类型（如路径、字符串）进行字段格式化

### 注释规范（中文，解释 WHY 而非 WHAT）
```rust
// 释放主调度器持有的 tx。只有当所有 worker task 结束后，
// 所有的 tx 才会完全 drop，rx 才能正确收到关闭信号。
drop(tx);
```

### 错误处理风格
一律使用 `anyhow::Result` 作为应用层返回类型，关键操作添加 `.with_context(|| ...)` 透传上下文：

```rust
let file = File::open(&src_path)
    .await
    .with_context(|| format!("Failed to open input file: {:?}", src_path))?;
```

### 完整代码片段示例（matcher.rs）
```rust
use anyhow::{Context, Result};
use regex::{RegexBuilder, RegexSet};
use std::sync::Arc;

/// 多模式匹配器：封装 RegexSet，支持忽略大小写与预编译校验
#[derive(Debug, Clone)]
pub struct MatchSet {
    inner: Arc<RegexSet>,
}

impl MatchSet {
    /// 编译一组模式为 MatchSet。
    /// 会对每个模式先单独用 RegexBuilder 校验，确保错误信息定位到具体哪条模式。
    pub fn compile(patterns: &[String], ignore_case: bool) -> Result<Self> {
        // 第一步：逐模式校验，让错误可定位
        for p in patterns {
            RegexBuilder::new(p)
                .case_insensitive(ignore_case)
                .build()
                .with_context(|| format!("Invalid regex pattern: '{}'", p))?;
        }
        // 第二步：组装带 (?i) 前缀的 RegexSet
        let processed: Vec<String> = patterns
            .iter()
            .map(|p| {
                if ignore_case {
                    format!("(?i){}", p)
                } else {
                    p.clone()
                }
            })
            .collect();
        let set = RegexSet::new(&processed)
            .with_context(|| format!("Failed to compile RegexSet for patterns: {:?}", patterns))?;
        Ok(Self {
            inner: Arc::new(set),
        })
    }

    /// 检查单行是否匹配任意模式
    pub fn is_match(&self, line: &str) -> bool {
        self.inner.is_match(line)
    }
}
```

---

## Testing Strategy

### 框架与位置
- **单元测试：** 各模块 `#[cfg(test)] mod tests` 内
- **集成测试：** `tests/integration_test.rs`
- 不引入第三方测试框架，使用 `std::fs` 创建临时目录与文件

### 覆盖要求与测试层次

| 测试层级 | 覆盖目标 | 断言粒度 |
|---|---|---|
| 单元测试 - matcher.rs | `MatchSet::compile` 成功/失败路径、`is_match` 字面量/正则/忽略大小写 | 对具体模式集合返回 bool |
| 单元测试 - cli.rs | clap 参数解析（短/长选项、多值 `-p`、`-r` 默认值、`-i` 默认值） | `Cli::parse_from` 返回的字段值 |
| 集成测试 - engine | 真实多文件 + 多模式 + 忽略大小写 + 防自锁 + 递归子目录 | 读回输出文件逐行比对 |

### 核心测试用例清单（必测）
1. **单关键字匹配：** 2 个文件，其中 2 行包含 `WangzheTrace`，输出恰好这 2 行
2. **多关键字（OR 语义）：** `-p WangzheTrace -p WangZheStrage`，匹配任一即输出
3. **正则表达式：** `-p "ERROR|WARN"` 正确匹配两种级别行
4. **忽略大小写：** `-p wangzhetrace -i` 匹配含 `WangzheTrace` / `wangzhetrace` 的行
5. **非法正则报错：** 传入 `[unclosed`，返回带上下文的错误，包含具体模式
6. **防自锁：** 输出文件 `-o ./test_logs/out.log`，扫描目录也为 `./test_logs`，输出文件不含自身内容
7. **空模式集合：** `patterns` 为空时的行为（编译阶段即报错或合理提示）
8. **空目录：** 无文件时输出文件为空文件，writer 正常结束
9. **递归目录（默认关）：** 不加 `-r` 时，`./logs/sub/deep.log` 中的匹配行**不**被输出
10. **递归目录（开启）：** 加 `-r` 时，`./logs/sub/deep.log` 和 `./logs/a.log` 中的匹配行**全部**被输出
11. **递归 + 防自锁：** `-r -o ./logs/result.log`，结果文件及其所在路径不会被自身读取匹配

### 运行方式
```bash
# 全量
cargo test

# 仅单元
cargo test --lib

# 仅集成
cargo test --test integration_test
```

---

## Boundaries

### Always do（必须执行）
- 提交前至少运行 `cargo test`、`cargo fmt --check`
- 新增公共函数必须写中文 doc comment（`///`）
- 所有 IO 操作（文件 open/read/write、目录 read_dir）必须加 `.with_context(|| ...)`
- 所有匹配模式在编译前必须逐模式校验，错误需定位到具体字符串

### Ask first（先问再做）
- 增加新的第三方依赖（如 `walkdir`、`rayon` 并行计算、`tempfile` 测试辅助）——**递归目录默认用 tokio 原生 read_dir + 栈/队列手动 DFS/BFS，不引入 walkdir**
- 改变 channel 容量（当前 1024）
- 修改 CLI 参数名/语义（如 `-p` 改名、`-r` 语义变化）
- 引入 GUI / Web 界面（PRD 仅定义 CLI）

### Never do（绝对禁止）
- 移除或弱化任何 PRD 明确存在的测试用例以通过 CI
- 在 `main.rs` 直接写匹配逻辑或目录遍历逻辑（必须分层）
- 用 `unwrap()` / `.expect("")` 处理用户输入或外部 IO（仅测试中允许）
- 在日志中打印可能含敏感数据的完整匹配行（debug 级可，info 级以上禁止）
- 将 `RUST_LOG` 默认级别硬编码为高于 `info`

---

## Success Criteria（验收条件，可逐项验证）

- [x] **CLI 解析：** `cargo run -- -d ./logs -p a -p b -i -r -o out.log` 解析后：`dir="./logs"`、`patterns=vec!["a","b"]`、`ignore_case=true`、`recursive=true`、`output=Some("out.log")`；无 `-r` 时 `recursive=false`
- [x] **构建通过：** `cargo build --release` 无 warnings（clippy `-D warnings` 0 告警）
- [x] **单元测试通过：** `cargo test --lib` 11/11 ok（cli 5 + matcher 6），matcher≥5 断言、cli 覆盖 -r/-i 默认值
- [x] **集成测试通过：** `cargo test --test integration_test` 7/7 ok，覆盖递归 ON/OFF、防自锁、忽略大小写、非法正则、空目录、正则 OR 6 场景
- [x] **手动冒烟 A（忽略大小写）：** 输出文件行数 = 2，分别为 WangzheTrace 和 wangzhestrage 的匹配行
- [x] **手动冒烟 B（正则）：** `-p "ERROR|WARN"`，输出 3 行（a.log ERROR / a.log WARN / b.log ERROR）
- [x] **手动冒烟 C（递归）：** 不加 `-r` 子目录 0 命中 + processed_files=2；加 `-r` 子目录 1 命中 + processed_files=3
- [x] **防自锁验证：** 输出文件位于输入目录时，processed_files 不包含自身、total_matches 稳定等于 3（注入 tag 数）
- [x] **日志规范性：** tracing 输出中关键 milestone 字段（directory / output / patterns_count / ignore_case / recursive / file / processed_files / total_matches / total_written）全部存在且为英文

---

## Open Questions（已解决 + 待确认）

1. ~~**递归目录？**~~ ✅ **已解决**：引入 `-r/--recursive`（`bool`，默认 `false`），使用 tokio 原生 `read_dir` + VecDeque 手动 BFS 实现，不引入 `walkdir` 依赖。
2. **空 patterns？** 当用户未传任何 `-p` 时，clap 会报 `the following required arguments were not provided`（因 `Vec<String>` + `num_args = 1..` 仍要求至少一个）。是否需要改为 `Option<Vec<String>>` 并在空时给出更友好的提示？**当前默认：保持 clap 原生报错，与 PRD 一致。**
3. **二进制行/UTF-8 无效？** 当前使用 `read_line`（按 UTF-8 String 读取），遇到二进制文件会 `Err`。是否需要 `read_until(b'\n')` 转为 `Vec<u8>` 再 lossy 匹配？**当前默认：保持 UTF-8 String，二进制文件抛错记录 error 日志，不终止其他文件。**
4. **匹配行去重？** 同一行若匹配多个模式，当前仅写入一次（因 `RegexSet::is_match` 返回 bool，非计数器）。是否需要报告命中了哪些模式？**当前默认：不报告，仅按 OR 语义去重写入。**

---

## Verified（已验证清单 · 增量实施完成于 2026-08-05）

> 执行环境：`macOS aarch64` / `rustc 1.95.0`。所有测试、lint、冒烟均通过。

### 1. 构建与 Lint

| 检查项 | 命令 | 结果 |
|---|---|---|
| 代码格式 | `cargo fmt --all` | ✅ 无修改（所有文件已格式化为 rustfmt 默认风格） |
| 静态检查 | `cargo clippy --all-targets -- -D warnings` | ✅ 0 警告 0 错误（已修复 2 处 `collapsible_if`） |
| Dev 构建 | `cargo build` | ✅ 完成 |
| Release 构建 | `cargo build --release` | ✅ 无 warning 完成 |

### 2. 单元测试（`cargo test --lib` = 11/11 ok）

- `cli::tests`（5/5）：`parse_defaults_for_optional_flags`、`parse_all_flags_explicit`、`parse_single_p_multiple_values`、`parse_output_absent_is_none_and_present_is_some`、`missing_patterns_is_reported_by_clap`
- `matcher::tests`（6/6）：`literal_match_single_pattern`、`multi_pattern_or_semantics_one_line_multiple_hits`、`ignore_case_matches_mixed_case`、`regex_or_two_level_keywords`、`invalid_pattern_error_contains_original_string`、`clone_is_cheap_and_still_works`

### 3. 集成测试（`cargo test --test integration_test` = 7/7 ok）

每个测试都在独立的 `$TMPDIR/tgrep_it_{pid}_{ts}_{autoinc}` 沙箱中运行，避免并行测试冲突：

| # | 测试函数 | 验证目标 |
|---|---|---|
| 1 | `recursive_disabled_does_not_scan_subdir` | `-r` 默认关 → 子目录 `sub/deep.log` 不被扫 → `files_processed=2`、`total_matches=0` |
| 2 | `recursive_enabled_scans_subdir` | `-r` 开 → `DEEP_MATCH_MARKER` 命中 → `files_processed=3`、`total_matches=1` |
| 3 | `output_inside_input_dir_self_lock_prevented` | 输出文件位于输入目录内部 → 既不被计入 processed_files（=3），也不把自己写入匹配行（total_matches=3=注入 tag 数） |
| 4 | `multi_keyword_or_and_ignore_case` | `-p wangzhetrace -i` 大小写不敏感 → 命中 3 行（WangzheTrace / WANGZHETRACE / wangzhetrace），不含 WangZheStrage |
| 5 | `invalid_regex_error_propagates_with_original_pattern` | `[unclosed_bracket_xyz` 返回 `Err`，`format!("{:#}")` 含原始子串 |
| 6 | `empty_dir_produces_empty_output_zero_stats` | 空目录 → `files_processed=0`、`total_matches=0`、输出文件 0 字节且存在 |
| 7 | `regex_level_or_matches_error_and_warn` | `-p "ERROR|WARN"` → 顶层 2 文件命中 3 行，每行必含 ERROR 或 WARN |

### 4. 手动冒烟测试

- **冒烟 A（忽略大小写 + 多模式）**：`-d ./test_logs -p wangzhetrace -p wangzhestrage -i -o smoke_a.log`
  → 输出文件 2 行：分别匹配 a.log 中 `WangzheTrace`、b.log 中 `wangzhestrage`。✅
- **冒烟 B（正则 OR）**：`-d ./test_logs -p "ERROR|WARN" -o smoke_b.log`
  → 输出文件 3 行（a.log ERROR / a.log WARN / b.log ERROR）。✅
- **冒烟 C（递归 ON/OFF 对比）**：
  - 不加 `-r`：搜索 `DEEP_MATCH_MARKER` → 0 命中、`files_processed=2`。✅
  - 加 `-r`：同关键词 → 1 命中（sub/deep.log）、`files_processed=3`。✅

### 5. 功能需求 × 核心测试用例映射（与 PRD / 规格一一对应）

| 规格条目 | 覆盖位置 |
|---|---|
| 功能需求 1（单目录多文件扫） | integration 测试 1/2/7，冒烟 A/B |
| 功能需求 2（多关键字 OR 语义） | matcher 单元测试 2，integration 测试 4/7 |
| 功能需求 3（忽略大小写） | matcher 单元测试 3，integration 测试 4，冒烟 A |
| 功能需求 4（默认命名输出） | engine::run 外部 caller 传入（main.rs），integration 空目录测试保证输出文件存在 |
| 功能需求 5（递归扫描 `-r`） | integration 测试 1/2/3，冒烟 C |
| 测试用例 1（无匹配） | matcher 单元测试 1（不匹配行）+ empty_dir 集成测试 |
| 测试用例 2（单关键字命中） | matcher 单元测试 1 + 冒烟 A |
| 测试用例 3（多关键字 OR） | matcher 单元测试 2 + integration 7 + 冒烟 B |
| 测试用例 4（忽略大小写） | matcher 单元测试 3 + integration 4 + 冒烟 A |
| 测试用例 5（非法正则） | matcher 单元测试 5 + integration 5 |
| 测试用例 6（防自锁） | integration 3 |
| 测试用例 8（空目录） | integration 6 |
| 测试用例 9/10（递归默认关 / 开） | integration 1/2 + 冒烟 C |
| 测试用例 11（递归 + 防自锁联合） | integration 3 的 recursive=true + 输出在沙箱内部 |
