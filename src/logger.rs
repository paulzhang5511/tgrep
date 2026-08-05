//! tracing 日志订阅器初始化封装。
//! 纯副作用模块，无返回值；多次调用是安全的（try_init 静默忽略重复初始化错误）。

use tracing_subscriber::{EnvFilter, FmtSubscriber};

/// 初始化全局日志订阅器。
///
/// - 若设置了环境变量 `RUST_LOG`，优先使用其配置的级别和过滤规则
/// - 否则 fallback 到传入的 `default_level`（例如 `"info"`）
/// - `default_level = None` 时回退到 tracing-subscriber 自带的默认（WARN 级）
pub fn init_logging(default_level: impl Into<Option<&'static str>>) {
    let default = default_level.into();
    let env_filter = match EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(_) => match default {
            Some(level) => EnvFilter::new(level),
            None => EnvFilter::default(),
        },
    };

    // 使用 try_init，忽略重复初始化错误（测试或库使用场景下可能多次调用）
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(env_filter)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
}
