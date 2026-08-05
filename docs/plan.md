# Implementation Plan: tgrep v0.1

## Overview
按 [spec.md](./spec.md) 构建一个 Rust/Tokio 异步 CLI 工具：扫描目录（可选递归），按多关键字/正则（可选忽略大小写）匹配行，多 Reader 并发 + 单 Writer 写入，支持防自锁。本计划自底向上分为 **Foundation（配置/基础模块） → Core（核心引擎） → Tests & Polish（测试 + 手动冒烟）** 三阶段，每阶段后设置 Review Checkpoint。

## Architecture Decisions
1. **递归实现：tokio 原生 BFS**。用 `std::collections::VecDeque<PathBuf>` 作为目录队列，循环调用 `tokio::fs::read_dir`；不引入 `walkdir`（同步阻塞），保持全异步。
2. **依赖注入：MatchSet 独立于 engine**。`matcher.rs` 纯同步（RegexSet 编译/匹配 CPU 绑定），可被单元测试直接调用，`engine.rs` 用 `Arc<MatchSet>` 跨 task 共享。
3. **文件列表收集与处理解耦**：先**一次性**收集所有待处理文件路径（BFS/非递归），再统一 spawn 到 JoinSet；这样输出文件的 canonicalize 判定只需做一次，不会因 writer 正在 create 而漏判。
4. **Channel 容量：1024（常量）**。反压保护，大文件下内存有界；后续可调但需 Ask first。
5. **目录遍历错误策略**：单个子目录 read_dir 失败 → warn! + 跳过该子树；单个文件读取失败 → error! + 计数，不终止整体。

## Dependency Graph（实现顺序）
```
Cargo.toml (依赖)
    │
    ├── src/cli.rs        ← 无依赖，最先写 + 单测
    ├── src/logger.rs     ← 依赖 tracing  crate，无内部依赖
    ├── src/matcher.rs    ← 依赖 regex/anyhow，无内部依赖 + 单测
    │
    ├── src/engine.rs     ← 依赖 matcher + cli 字段语义 + tokio
    │
    └── src/main.rs       ← 依赖 cli/logger/engine
              │
              └── tests/integration_test.rs  ← 调用 engine 的公开入口
```

## Task List

---

### Phase 1: Foundation（基础层：依赖 + CLI + 日志 + Matcher）

---

## Task 1: 配置 Cargo.toml 依赖

**Description：** 按 spec 把 tokio/clap/regex/chrono/tracing/anyhow 全部加入，锁定到 spec 指定的版本范围；确认 edition=2024、包名/版本正确。

**Acceptance criteria：**
- [ ] `cargo check` 在空 main（`fn main(){}`）下通过，无 warning
- [ ] 所有依赖版本号与 spec Tech Stack 表一致（tokio features=["full"], clap features=["derive"] 等）
- [ ] `Cargo.lock` 生成（执行过一次 check/build 即可）

**Verification：**
- [ ] 命令：`cargo check` 退出码 0
- [ ] 命令：`grep -E 'tokio|clap|regex|chrono|tracing|anyhow' Cargo.toml` 能看到 6 条依赖

**Dependencies：** None

**Files likely touched：**
- `Cargo.toml`

**Estimated scope：** XS（1 文件）

---

## Task 2: 实现 src/cli.rs 并写单元测试

**Description：** 定义 `#[derive(Parser, Debug)] struct Cli`，字段：`dir(String, -d/--dir)`、`patterns(Vec<String>, -p/--patterns, num_args=1..)`、`output(Option<String>, -o/--output)`、`ignore_case(bool, -i/--ignore-case, default false)`、`recursive(bool, -r/--recursive, default false)`。同文件 `#[cfg(test)]` 写参数解析断言。

**Acceptance criteria：**
- [ ] `-d logs -p a -p b -i -r -o out.log` → 所有字段正确解析（含 recursive=true）
- [ ] 不传 `-r -i` → `recursive=false, ignore_case=false`
- [ ] `-p foo bar`（单次多值）→ `patterns == vec!["foo","bar"]`
- [ ] 不传 `-o` → `output == None`
- [ ] `-p` 完全不传 → clap 报错（至少需要 1 个 arg，符合 `num_args=1..`）
- [ ] `clap` 的 about/version/author 属性齐全，`--help` 输出可读

**Verification：**
- [ ] `cargo test --lib cli::tests` 通过
- [ ] `cargo run -- --help` 输出包含 -r/--recursive、-i/--ignore-case 项

**Dependencies：** Task 1（Cargo.toml 有 clap）

**Files likely touched：**
- `src/cli.rs`
- `src/main.rs`（加 `mod cli;` 声明，便于 cargo test --lib 找到）

**Estimated scope：** S（1-2 文件）

---

## Task 3: 实现 src/logger.rs

**Description：** 提供 `pub fn init_logging(default_level_hint: Option<&str>)`，内部用 `tracing_subscriber::fmt().with_env_filter(...)`：优先 `RUST_LOG` env，否则用传入 hint（main 里传 `"info"`）。此模块无测试（纯副作用），但需在 main 集成后用 `RUST_LOG=debug` 人工观察。

**Acceptance criteria：**
- [ ] 函数签名 `pub fn init_logging(default: impl Into<Option<&'static str>>)` 或等价，可被 main 调用一次
- [ ] 多次调用不 panic（tracing_subscriber `try_init` 或 `_ =` 忽略 Err）
- [ ] `RUST_LOG=debug` 时 debug! 可见；默认环境变量未设时默认 info

**Verification：**
- [ ] `cargo check` 通过（logger 集成到 main 后再做实际 smoke）
- [ ] 代码 review：调用了 `try_init()` 或等效保护，重复调用不 panic

**Dependencies：** Task 1

**Files likely touched：**
- `src/logger.rs`

**Estimated scope：** XS（1 文件，<40 行）

---

## Task 4: 实现 src/matcher.rs 并写完整单元测试

**Description：** 实现 spec Code Style 章节中定义的 `MatchSet` 结构体 + `compile / is_match`，包含：逐模式 `RegexBuilder` 校验（错误定位到具体模式字符串）、`ignore_case` 下组装 `(?i)` 前缀、`Arc<RegexSet>` 内部字段（便于 engine 跨 task 共享 Clone）。单元测试覆盖用例 1-5 的等价断言。

**Acceptance criteria：**
- [ ] `compile(&["ERROR".into(), "WARN".into()], false)` 返回 Ok，`is_match("[ERROR] x")` true
- [ ] `compile(&["wangzhe".into()], true).is_match("WangZhe hello")` true
- [ ] `compile(&["[unclosed".into()], false)` 返回 Err，其 `format!("{:#}", err)` 字符串中包含字面量 `[unclosed`
- [ ] 多个模式 OR 语义：匹配任一即 true，同一行多命中仍只返回 true（不计数）
- [ ] Clone 成本低（内部 Arc 验证方式：`m1.compile ok → m2 = m1.clone() → m2.is_match 正常`）

**Verification：**
- [ ] `cargo test --lib matcher::tests` 全部通过
- [ ] 至少 5 条独立 `#[test]` 用例

**Dependencies：** Task 1

**Files likely touched：**
- `src/matcher.rs`
- `src/main.rs`（加 `mod matcher;`）

**Estimated scope：** S（1-2 文件）

---

### Checkpoint: Foundation（Tasks 1-4 后）
- [ ] `cargo check` 0 warnings
- [ ] `cargo test --lib` 全部通过（cli + matcher）
- [ ] `cargo run -- --help` 显示 -d/-p/-i/-r/-o 五项
- [ ] 人审：mod 声明正确，无循环依赖

---

### Phase 2: Core Features（引擎 + 入口 + 集成）

---

## Task 5: 实现 src/engine.rs 核心逻辑

**Description：** 对外暴露 `pub async fn run(config: EngineConfig) -> Result<EngineStats>`，其中 `EngineConfig { dir, output, patterns, ignore_case, recursive }`、`EngineStats { files_processed, total_matches }`。内部步骤：
1. MatchSet::compile 失败直接返回 Err
2. 收集文件：recursive 用 BFS(VecDeque)，非递归用单次 read_dir；遇到子目录 read_dir 失败 warn! 跳过；全部用 canonicalize vs 输出路径比对防自锁
3. spawn 单写 task（mpsc rx → BufWriter → File::create）
4. JoinSet spawn 每个文件的 `filter_single_file`（公开或私有均可，核心是用 BufReader.read_line + MatchSet.is_match + tx.send）
5. drop(tx)，等 JoinSet 聚合统计，再 await writer_handle
6. 所有文件 IO 带 `.with_context(|| ...)`

所有 tracing 字段包含 `recursive`、`patterns_count`、`directory`、`output`、`file` 等英文结构化字段。

**Acceptance criteria：**
- [ ] 对 2 文件场景，返回 EngineStats.files_processed = 2，total_matches = 预期
- [ ] 递归关：`sub/` 下文件不出现在 files_processed；递归开：出现在 files_processed
- [ ] 非法正则时 run() 返回 Err，且错误信息可读（包含模式字符串）
- [ ] 输出文件在输入目录内 → 不被自身读取（通过 canonicalize 比对）
- [ ] tracing info! 至少打印 Initializing / Spawning worker / All tasks completed / Writer flushed 四个 milestone

**Verification：**
- [ ] `cargo check` 0 warnings
- [ ] 代码 review：BFS/VecDeque 使用正确，目录队列元素为 PathBuf，每次 pop_front 后 read_dir，子目录 push_back

**Dependencies：** Tasks 2, 4（cli 字段名确定 / matcher API 确定）

**Files likely touched：**
- `src/engine.rs`
- `src/main.rs`（加 `mod engine;`）

**Estimated scope：** M（3-5 模块间引用，但主要改动 1-2 文件）

---

## Task 6: 完成 src/main.rs 入口拼接

**Description：** main.rs 瘦身到 ~60 行。职责：`Cli::parse()` → 若 `cli.output.is_none()` 用 `chrono::Local::now()` 生成 `output_YYYYMMDD_HHMMSS.log`（info! 打生成日志）→ `init_logging(Some("info"))` → 组装 `EngineConfig` → `engine::run(cfg).await`；Err 时 `error!(...)` + `process::exit(1)`；Ok 时 `info!(output_file, "Application execution completed")`。

**Acceptance criteria：**
- [ ] main.rs 不含任何目录遍历/正则/读写逻辑（全部分层）
- [ ] 行数 <= 100（不含空行和注释）
- [ ] 未传 `-o` 时生成文件名格式正确（包含年月日时分秒 14 位）
- [ ] `RUST_LOG=debug cargo run -- -d ...` 时 debug! 能看到 file 维度日志
- [ ] engine 返回 Err 时退出码非 0（sh 下 `echo $?` 验证）

**Verification：**
- [ ] `wc -l src/main.rs` 合理（<=100 是软指标，review 即可）
- [ ] 手动命令：`cargo run -- -d NON_EXIST_DIR -p foo` 退出码非 0
- [ ] 手动命令：`mkdir -p /tmp/t && echo foo >/tmp/t/a && cargo run -- -d /tmp/t -p foo -o /tmp/out.log` 正常完成且 /tmp/out.log 含 "foo"

**Dependencies：** Tasks 3, 5

**Files likely touched：**
- `src/main.rs`

**Estimated scope：** S（1 文件）

---

### Checkpoint: Core Features（Tasks 5-6 后）
- [ ] `cargo build --release` 0 warnings
- [ ] 手动冒烟 A（spec Success Criteria 第 5 条）通过
- [ ] 手动冒烟 B（spec Success Criteria 第 6 条）通过
- [ ] 人审：tracing 日志全英文，结构化字段完整

---

### Phase 3: Tests & Polish（集成测试 + 递归/自锁验证 + 格式化）

---

## Task 7: 编写 tests/integration_test.rs 集成测试

**Description：** 使用 `std::env::temp_dir()` + 随机子目录构造测试沙箱（避免并行测试冲突）。公开函数直接调 `engine::run(EngineConfig)`（非子进程，依赖更轻）。覆盖 spec 测试清单 9/10/11 三个新增场景 + 多关键字 + 正则 + 空目录合计至少 6 个 `#[tokio::test]`。

测试用例目录结构（集成测试内部构建）：
```
$TEMP/tgrep_it_{rand}/
├── a.log          (2 行匹配 + 1 行不匹配)
├── b.log          (1 行匹配)
├── sub/
│   └── deep.log   (1 行匹配)
└── out.log        ← 防自锁场景时在此生成
```

**Acceptance criteria：**
- [ ] `cargo test --test integration_test` 全绿，无 flaky（用唯一 temp dir）
- [ ] 递归关测试：deep.log 内容 **不在** 输出中；递归开测试：deep.log 内容 **在** 输出中
- [ ] 防自锁测试：out.log 放在 a/b/sub 同目录，运行后 `grep` 输出确认 **不含** out.log 自身内容（如 `foo` 仅在 a/b/deep 有）
- [ ] 空目录测试：`files_processed=0, total_matches=0, 输出文件 0 字节`
- [ ] 多关键字 OR：a+b+deep 总计 4 个匹配行全部出现，顺序无关（匹配到所有即可，或按行 sort 后比较）

**Verification：**
- [ ] `cargo test --test integration_test -- --nocapture` 观察断言
- [ ] 至少 6 个独立测试函数（按 spec 清单 1/2/3/6/8/9/10/11 的覆盖度择重）

**Dependencies：** Task 5

**Files likely touched：**
- `tests/integration_test.rs`

**Estimated scope：** M（集成测试 = 中等体量，1 文件但代码量≈300 行）

---

## Task 8: 格式化、Clippy、最终验证与 Spec Checkbox 回填

**Description：** 统一 `cargo fmt`；跑 `cargo clippy -- -D warnings` 修掉；逐条对照 spec Success Criteria 的 9 项 Checkbox，在 docs/spec.md 末尾（单独 `## Verified:` 段）标记通过情况，对通过项打勾 ✅，对需要手动执行的项附命令 + 结果摘录。

**Acceptance criteria：**
- [ ] `cargo fmt --check` 无差异（0 exit）
- [ ] `cargo clippy -- -D warnings` 0 warning（或在 spec 的 Verified 段说明已知 allow 的原因）
- [ ] `cargo test`（unit + integration）全通过
- [ ] `docs/spec.md` 新增 `## Verified:` 小节，9 条 Success Criteria 每条都标注了通过/失败 + 命令依据
- [ ] Release 构建 `cargo build --release` 成功，`./target/release/tgrep --help` 正常

**Verification：**
- [ ] 命令顺序：`cargo fmt → cargo clippy -- -D warnings → cargo test → cargo build --release`，全部 exit 0
- [ ] 人工检查 docs/spec.md Verified 段内容完整

**Dependencies：** Tasks 7（跑完最后一次集成测试）

**Files likely touched：**
- 全部源文件（格式化）
- `docs/spec.md`（追加 Verified 段）

**Estimated scope：** M（一次全仓操作，无新增逻辑）

---

### Checkpoint: Complete（Task 8 后）
- [ ] 所有 spec Success Criteria checkbox 对应实测状态 ≥ 8/9 ✅（最多 1 项日志观感类人工确认）
- [ ] 人审：递归 + 防自锁两项关键能力有测试和手动证据

---

## Risks and Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| tokio `read_dir` 递归时，output 尚未 create，`canonicalize(output_file_path).await.ok()` 返回 None → 自锁失败 | Med | **收集文件前先 `File::create(&out_path).await?`（空文件），此时 canonicalize 可拿到真实路径**；这也使得 writer task 后续 create 变为截断，语义不变 |
| 二进制文件导致 `read_line` UTF-8 Err，被当成读取失败完全跳过 | Low | 按 Open Question 3 默认处理：记录 error!，不 panic，不影响其他文件；后续需要再升级为 lossy 策略 |
| 集成测试并行测试共用临时目录导致冲突 | High | 使用 `std::env::temp_dir().join(format!("tgrep_it_{:x}", rand::random::<u64>()))` 每个测试独立目录（或用 thread_local + 进程 PID 代替 rand，测试中允许 `rand` 缺省依赖） |
| `-r` 下深层目录树，VecDeque 内存爆炸（百万级文件） | Low | 在 info 日志中打印 `total_files_collected`；若后续遇到再加 streaming 收集迭代器 |
| Clippy 对 `(?i)` 前缀正则拼接 lint "suspicious_double_ref_op" 等 | Low | 允许 `#[allow(...)]` 但需在 Task 8 Verified 段写明理由 |

## Parallelization Opportunities
Tasks 可并行窗口（未来多 agent 时才用到，单 agent 按序即可）：
- **并行组 A：** Task 2 (cli)、Task 3 (logger)、Task 4 (matcher) — 三者互不依赖，只依赖 Task 1
- **并行组 B：** Task 7 (integration tests) 可在 Task 5 API 稳定后与 Task 6 (main) 并行撰写

## Verification Checklist（人审 Plan 时确认）
- [ ] 每个 Task 有 Acceptance Criteria + Verification 命令
- [ ] 最大 Task 范围 = M（<= 5 files），无 XL
- [ ] Dependencies 箭头与依赖图一致（Foundation → Core → Tests）
- [ ] 3 个 Checkpoint 全部列于合适位置
- [ ] spec 中 Open Questions 1（递归）已在此 Plan 中给出确定实现方案（BFS + VecDeque）
