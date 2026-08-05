# tgrep

**tgrep** 是一个面向日志目录的**异步并发关键字/正则扫描工具**。给定一个目录与一组正则模式，它会并行读取该目录（可选：递归到所有子目录）下的所有文本文件，将匹配到的行以 OR 语义写入单个输出文件。

- **多关键字 OR**：一行命中任一模式即写入（基于 `regex::RegexSet` 单次扫描）
- **高并发**：Tokio 多 reader + 单 writer，通过 bounded mpsc 通道解耦
- **递归扫描**：`-r/--recursive` BFS 深度遍历所有子目录
- **忽略大小写**：`-i/--ignore-case` 统一大小写不敏感匹配
- **防自锁**：若输出文件位于输入目录，会预创建并 canonicalize 后从扫描列表剔除，避免读取自身死循环
- **错误隔离**：单个文件 IO / 编码错误仅记录 warn 日志，不影响其它文件

---

## 功能特性

| 开关 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `-d, --dir <DIR>` | `String` | 必填 | 要扫描的根目录 |
| `-p, --patterns <PAT>...` | `Vec<String>` | 必填 ≥1 | 扫描模式，接受完整正则（可用 `ERROR\|WARN` 这类 OR 写法）；可重复传多次 |
| `-o, --output <FILE>` | `Option<String>` | `output_YYYYMMDD_HHMMSS.log` | 结果输出文件（支持放在扫描目录内，自动自锁过滤） |
| `-i, --ignore-case` | `bool` | `false` | 大小写不敏感匹配 |
| `-r, --recursive` | `bool` | `false` | 递归遍历所有子目录 |
| `-h, --help` | — | — | 打印帮助 |
| `-V, --version` | — | — | 打印版本 |

---

## 安装

需要 **Rust 1.74+**（`edition = "2021"`）。

```bash
# Release 构建（推荐）
cargo build --release
./target/release/tgrep --help
```

或直接运行（开发态）：

```bash
cargo run -- --help
```

---

## 使用示例

仓库里有一套示例数据：`test_logs/`（含 `a.log`、`b.log`、`sub/deep.log`）。

### 1. 单模式单目录

```bash
cargo run -- -d ./test_logs -p AlphaMarker -o result1.log
```

匹配包含 `AlphaMarker` 的行。

### 2. 多关键字 OR + 忽略大小写

```bash
cargo run -- -d ./test_logs \
    -p alphamarker \
    -p betamarker \
    -i \
    -o result2.log
```

任何大小写变体的 `AlphaMarker` / `BetaMarker` 都会被写入。

### 3. 正则：抓取所有 ERROR 与 WARN 行

```bash
cargo run -- -d ./test_logs -p "ERROR|WARN" -o levels.log
```

### 4. 递归扫描子目录

```bash
# 默认不递归：sub/deep.log 不会被扫到
cargo run -- -d ./test_logs -p DEEP_MATCH_MARKER -o no_recursive.log
# wc -l no_recursive.log => 0

# 加上 -r 就会扫到 sub/deep.log
cargo run -- -d ./test_logs -p DEEP_MATCH_MARKER -r -o recursive.log
# wc -l recursive.log => 1
```

### 5. 输出文件放在扫描目录内（自动防自锁）

```bash
cargo run -- -d ./test_logs \
    -o ./test_logs/inside_result.log \
    -p AlphaMarker \
    -r
```

`inside_result.log` 会在文件收集前预先创建并 canonicalize，随后从扫描列表里剔除，不会出现"边写边读导致越写越多"的死循环。

---

## 架构

```
                ┌─────────────────────┐
                │  main.rs (thin bin) │
                │  parse CLI / config │
                └─────────┬───────────┘
                          ▼
                ┌──────────────────────┐
                │  engine::run(cfg)    │
                │  ┌─ create output +  │
                │  │  canonicalize it  │
                │  ├─ BFS collect_files│
                │  │  (self-lock skip) │
                │  ├─ MatchSet compile │
                │  ├─ spawn N readers  │───┐
                │  └─ spawn 1 writer   │─┐ │
                └──────────────────────┘ │ │
                                         ▼ ▼
                                    mpsc::bounded(1024)
                                         │
                    matches: (display_path, line)
                                         │
                                         ▼
                              writer: write_all + sync_all
```

并发模型（单 writer + 多 reader）天然避免了多线程写文件的竞态，同时 reader 端使用 `tokio::fs::File::read_line` 按行增量读取，适合处理大日志。

---

## 项目结构

```
tgrep/
├── Cargo.toml
├── README.md
├── docs/
│   ├── prd.md        # 原始 PRD
│   ├── spec.md       # 规格文档（含已验证清单）
│   ├── plan.md       # 技术实现计划
│   └── tasks.md      # 可执行任务清单
├── src/
│   ├── lib.rs        # 库入口，pub 导出所有模块
│   ├── main.rs       # 二进制入口（thin wrapper）
│   ├── cli.rs        # clap Parser + 5 个单元测试
│   ├── logger.rs     # tracing 订阅器（RUST_LOG 优先，默认 info）
│   ├── matcher.rs    # MatchSet(Arc<RegexSet>) + 6 个单元测试
│   └── engine.rs     # 文件收集 + 并发匹配 + 单 writer 核心逻辑
├── tests/
│   └── integration_test.rs   # 7 个集成测试（独立 tempdir 沙箱）
└── test_logs/        # 手动冒烟夹具
    ├── a.log
    ├── b.log
    └── sub/deep.log
```

---

## 测试

```bash
# 全部：unit (11) + integration (7) = 18
cargo test

# 仅单元
cargo test --lib

# 仅集成
cargo test --test integration_test
```

Lint：

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
```

---

## CI

GitHub Actions 在 `.github/workflows/ci.yml` 定义了如下 job（Ubuntu latest + stable Rust）：

- `cargo fmt --all --check`
- `cargo check --all-targets`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --all-targets`

PR 与 main 分支 push 都会自动触发。

---

## 设计取舍速记

- **不引入 `walkdir`**：递归使用 Tokio 原生 `read_dir` + `VecDeque` 手动 BFS，保持全异步执行器一致性。
- **多模式用 `RegexSet` 而非逐行循环 `Regex::is_match`**：一次 DFA 扫描同时判定 N 个模式，`O(|line|)` 固定开销。
- **通道容量固定 1024**：避免 reader 跑在 writer 太前面导致内存爆炸；写盘慢时 reader 自动背压。
- **UTF-8 严格读取**：使用 `read_line` 读取 String，二进制文件会按"单文件失败"处理（error 日志 + 跳过），不会影响其余文件。未来如需支持非 UTF-8 可换为 `read_until(b'\n')` + `String::from_utf8_lossy`。

---

## License

MIT OR Apache-2.0（Rust 生态双许可惯例）。
