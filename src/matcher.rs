//! 多模式匹配器：封装 `regex::RegexSet`。
//!
//! 设计目标：
//! - 支持「多关键字 OR 语义 + 正则表达式 + 全局忽略大小写」三种能力组合
//! - 每个模式在编译前单独 `RegexBuilder` 校验，失败时错误信息能定位到具体模式字符串
//! - 内部用 `Arc<RegexSet>` 封装，`Clone` 成本极低，便于跨 Tokio task 共享只读引用

use anyhow::{Context, Result};
use regex::{RegexBuilder, RegexSet};
use std::sync::Arc;

/// 多模式匹配集合：线程安全、克隆廉价、匹配任意一行是否命中任一规则。
#[derive(Debug, Clone)]
pub struct MatchSet {
    inner: Arc<RegexSet>,
}

impl MatchSet {
    /// 编译一组模式为 `MatchSet`。
    ///
    /// 流程（两步，目的是让错误可定位）：
    /// 1. 对每个模式单独用 `RegexBuilder` 构建 + `case_insensitive(ignore_case)`，
    ///    任一条失败即返回 Err，错误信息中包含原始模式字符串
    /// 2. 若 `ignore_case = true`，给每个模式拼 `(?i)` 前缀后再组装 RegexSet
    ///    （RegexSet 本身不支持 per-set case_insensitive，只能在字符串层面处理）
    pub fn compile(patterns: &[String], ignore_case: bool) -> Result<Self> {
        // 第一步：逐模式校验，让错误可定位到具体字符串
        for p in patterns {
            RegexBuilder::new(p)
                .case_insensitive(ignore_case)
                .build()
                .with_context(|| format!("Invalid regex pattern: '{}'", p))?;
        }

        // 第二步：组装带或不带 (?i) 前缀的 RegexSet
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

    /// 判断单行文本是否匹配集合中的任意一个模式。
    ///
    /// 采用 OR 语义：命中多个也只返回 `true` 一次（不报告命中了哪些）。
    #[inline]
    pub fn is_match(&self, line: &str) -> bool {
        self.inner.is_match(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_match_single_pattern() {
        // 单关键字字面量匹配
        let m = MatchSet::compile(&["AlphaMarker".to_string()], false).unwrap();
        assert!(m.is_match("2026-08-05 [ERROR] AlphaMarker failed to connect DB"));
        assert!(!m.is_match("2026-08-05 [INFO] heartbeat ok"));
    }

    #[test]
    fn regex_or_two_level_keywords() {
        // 正则：ERROR|WARN 两个级别关键字 OR
        let m = MatchSet::compile(&["ERROR|WARN".to_string()], false).unwrap();
        assert!(m.is_match("[ERROR] disk full"));
        assert!(m.is_match("[WARN] memory approaching 80%"));
        assert!(!m.is_match("[INFO] user login"));
    }

    #[test]
    fn ignore_case_matches_mixed_case() {
        // 忽略大小写：aLpHaMaRkEr / BetaMarker / betamarker 都能匹配小写输入的模式
        let m = MatchSet::compile(&["alphamarker".to_string(), "betamarker".to_string()], true)
            .unwrap();
        assert!(m.is_match("AlphaMarker failed"));
        assert!(m.is_match("betamarker module IO timeout"));
        assert!(m.is_match("ALPHAMARKER restart"));
        assert!(!m.is_match("OtherModule ok"));
    }

    #[test]
    fn invalid_pattern_error_contains_original_string() {
        // 非法正则：未闭合的字符组 [unclosed，错误消息里应当能看到 "Invalid regex pattern: '[unclosed'"
        let err = MatchSet::compile(&["[unclosed".to_string()], false).unwrap_err();
        let formatted = format!("{:#}", err);
        assert!(
            formatted.contains("[unclosed"),
            "Expected error message to contain the bad pattern, got: {}",
            formatted
        );
    }

    #[test]
    fn clone_is_cheap_and_still_works() {
        // 内部是 Arc，clone() 后两个实例共享同一个 RegexSet，行为一致
        let m1 = MatchSet::compile(&["foo".to_string()], false).unwrap();
        let m2 = m1.clone();
        assert!(m1.is_match("hello foo world"));
        assert!(m2.is_match("hello foo world"));
        assert!(!m1.is_match("no match here"));
        assert!(!m2.is_match("no match here"));
    }

    #[test]
    fn multi_pattern_or_semantics_one_line_multiple_hits() {
        // 同一行同时命中两个模式 → 仍然 true 一次，但绝不应因多重而返回 false 或报错
        let m = MatchSet::compile(&["foo".to_string(), "bar".to_string()], false).unwrap();
        assert!(m.is_match("foo and bar together"));
        assert!(m.is_match("only foo"));
        assert!(m.is_match("only bar"));
        assert!(!m.is_match("neither here"));
    }
}
