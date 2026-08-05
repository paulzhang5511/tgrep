//! 命令行参数定义：基于 clap derive 模式。
//! 所有字段名、短选项、默认值需与 docs/spec.md Success Criteria 第一条一致。

use clap::Parser;

/// tgrep: 高性能异步多文件正则/多关键字文本过滤工具
#[derive(Parser, Debug, Clone)]
#[command(
    name = "tgrep",
    author,
    version,
    about = "High-performance Tokio-powered concurrent log & text line filtering CLI tool",
    long_about = None
)]
pub struct Cli {
    /// 目标检索目录路径
    #[arg(short = 'd', long = "dir")]
    pub dir: String,

    /// 匹配检索的关键字符串或正则表达式（支持多个：-p p1 -p p2，或单次 -p p1 p2）
    /// 至少提供 1 个 pattern；完全不传 -p 时 clap 直接报 required argument 缺失。
    #[arg(short = 'p', long = "patterns", required = true, num_args = 1..)]
    pub patterns: Vec<String>,

    /// 过滤结果输出文件路径 [不指定时自动生成 output_YYYYMMDD_HHMMSS.log]
    #[arg(short = 'o', long = "output")]
    pub output: Option<String>,

    /// 忽略大小写匹配（默认：关）
    #[arg(short = 'i', long = "ignore-case", default_value_t = false)]
    pub ignore_case: bool,

    /// 递归扫描所有子目录（默认：关，仅一级文件）
    #[arg(short = 'r', long = "recursive", default_value_t = false)]
    pub recursive: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use clap::error::ErrorKind;

    #[test]
    fn parse_all_flags_explicit() {
        // 完整指定所有 flag：-d / -p 两次 / -i / -r / -o
        let cli = Cli::parse_from([
            "tgrep", "-d", "./logs", "-p", "a", "-p", "b", "-i", "-r", "-o", "out.log",
        ]);
        assert_eq!(cli.dir, "./logs");
        assert_eq!(cli.patterns, vec!["a".to_string(), "b".to_string()]);
        assert!(cli.ignore_case);
        assert!(cli.recursive);
        assert_eq!(cli.output.as_deref(), Some("out.log"));
    }

    #[test]
    fn parse_defaults_for_optional_flags() {
        // 不传 -i/-r/-o → 三项默认值
        let cli = Cli::parse_from(["tgrep", "-d", "./logs", "-p", "foo"]);
        assert!(!cli.ignore_case);
        assert!(!cli.recursive);
        assert_eq!(cli.output, None);
    }

    #[test]
    fn parse_single_p_multiple_values() {
        // 单次 -p 接多个值：-p foo bar
        let cli = Cli::parse_from(["tgrep", "-d", "./logs", "-p", "foo", "bar"]);
        assert_eq!(cli.patterns, vec!["foo".to_string(), "bar".to_string()]);
    }

    #[test]
    fn parse_output_absent_is_none_and_present_is_some() {
        let with_out = Cli::parse_from(["tgrep", "-d", ".", "-p", "x", "-o", "custom.log"]);
        assert_eq!(with_out.output.as_deref(), Some("custom.log"));

        let without_out = Cli::parse_from(["tgrep", "-d", ".", "-p", "x"]);
        assert_eq!(without_out.output, None);
    }

    #[test]
    fn missing_patterns_is_reported_by_clap() {
        // 完全不传 -p，num_args=1.. 会触发 "the following required arguments were not provided"
        let err = Cli::try_parse_from(["tgrep", "-d", "./logs"]).unwrap_err();
        // ErrorKind::MissingRequiredArgument (clap 4.x)
        assert!(
            matches!(
                err.kind(),
                ErrorKind::MissingRequiredArgument | ErrorKind::TooFewValues
            ),
            "Unexpected error kind for missing -p: {:?}",
            err.kind()
        );
    }
}
