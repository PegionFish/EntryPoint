//! tracing 初始化与运行期日志级别热调整（P2-1：`general.log_level` 接线）。
//!
//! 过滤规则优先级：**`RUST_LOG` 环境变量 > `general.log_level` 配置**
//!（与迁移前行为一致：`RUST_LOG` 始终优先覆盖）。
//!
//! 配置值作用于 `ep_daemon` / `ep_core` 两个 target，形状与迁移前 main.rs
//! 硬编码的 `ep_daemon=info,ep_core=info` 一致，仅级别随配置变化；
//! 非法级别名回退 `info`。
//!
//! PUT /api/config 修改 `log_level` 后经 [`apply_log_level`]（tracing-subscriber
//! reload handle）动态生效，无需重启——`requires_restart` 因此不再包含
//! `log_level`（api/config.rs 的 `restart_sensitive_changed` 同步移除）。

use std::sync::OnceLock;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::reload;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry};

/// 默认过滤指令（迁移前 main.rs 的硬编码值，`log_level` 非法时回退于此）
const DEFAULT_DIRECTIVES: &str = "ep_daemon=info,ep_core=info";

/// EnvFilter reload handle 类型（registry + EnvFilter 组合）
pub type FilterHandle = reload::Handle<EnvFilter, Registry>;

static RELOAD_HANDLE: OnceLock<FilterHandle> = OnceLock::new();

/// 由 `general.log_level` 构建过滤指令：`ep_daemon={lvl},ep_core={lvl}`。
///
/// 级别名做 trim + 小写归一；非法值在 [`filter_from_level`] 解析期回退 info。
pub fn directives_for(log_level: &str) -> String {
    let lvl = log_level.trim().to_lowercase();
    format!("ep_daemon={lvl},ep_core={lvl}")
}

/// 纯配置路径：按级别构建 EnvFilter；非法级别名回退 [`DEFAULT_DIRECTIVES`]。
fn filter_from_level(log_level: &str) -> EnvFilter {
    EnvFilter::try_new(directives_for(log_level))
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_DIRECTIVES))
}

/// 构建生效的 EnvFilter：**`RUST_LOG` 优先**；未设置（或非法）时用配置值。
pub fn build_env_filter(log_level: &str) -> EnvFilter {
    match EnvFilter::try_from_default_env() {
        Ok(from_env) => from_env,
        Err(_) => filter_from_level(log_level),
    }
}

/// 初始化全局 tracing subscriber：registry + 可热 reload 的 EnvFilter + fmt 层。
///
/// 输出形状与迁移前 `tracing_subscriber::fmt().with_env_filter(…).init()` 一致；
/// reload handle 存入进程级单例，供 PUT /api/config 运行期调级。
/// 仅允许在进程启动期调用一次（重复调用将 panic —— 全局 subscriber 已存在）。
// 唯一调用点在 main.rs；本文件经 #[path] 重挂进 e2e 测试 crate 时无调用点
//（与 api/mod.rs err_response 同款豁免）
#[allow(dead_code)]
pub fn init_tracing(log_level: &str) {
    let (filter, handle) = reload::Layer::new(build_env_filter(log_level));
    let _ = RELOAD_HANDLE.set(handle);
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
}

/// 运行期应用新的日志级别（PUT /api/config 保存成功后调用）。
///
/// 返回 `true` = 已动态生效；`false` = 本进程未安装可 reload 的全局
/// subscriber（如测试环境），调用方无需处理（配置已落盘，重启后生效）。
/// `RUST_LOG` 设置时优先于配置值（与启动期同规则）。
pub fn apply_log_level(log_level: &str) -> bool {
    match RELOAD_HANDLE.get() {
        Some(handle) => handle.reload(build_env_filter(log_level)).is_ok(),
        None => false,
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::layer::Context;
    use tracing_subscriber::Layer;

    /// RUST_LOG 读写影响整个进程；凡触碰环境变量的测试必须串行
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── 事件捕获层（验证过滤器真实行为，不依赖全局 subscriber） ─────────

    #[derive(Clone, Default)]
    struct CaptureLayer {
        messages: Arc<Mutex<Vec<String>>>,
    }

    struct MsgVisitor(String);

    impl tracing::field::Visit for MsgVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0 = format!("{value:?}");
            }
        }
    }

    impl<S: tracing::Subscriber> Layer<S> for CaptureLayer {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            let mut visitor = MsgVisitor(String::new());
            event.record(&mut visitor);
            self.messages.lock().unwrap().push(visitor.0);
        }
    }

    /// 在给定 EnvFilter 下执行 `f`，返回通过过滤的事件消息列表
    fn collect_with_filter(filter: EnvFilter, f: impl FnOnce()) -> Vec<String> {
        let layer = CaptureLayer::default();
        let messages = layer.messages.clone();
        let subscriber = tracing_subscriber::registry().with(filter).with(layer);
        tracing::subscriber::with_default(subscriber, f);
        let collected = messages.lock().unwrap().clone();
        collected
    }

    // 1. 配置级别作用于 ep_daemon / ep_core（形状与迁移前硬编码一致）
    #[test]
    fn filter_from_level_shapes_ep_targets() {
        let msgs = collect_with_filter(filter_from_level("warn"), || {
            tracing::info!(target: "ep_daemon", "daemon-info-dropped");
            tracing::warn!(target: "ep_daemon", "daemon-warn-kept");
            tracing::warn!(target: "ep_core", "core-warn-kept");
        });
        assert_eq!(msgs, vec!["daemon-warn-kept", "core-warn-kept"]);
    }

    // 2. 级别名大小写与首尾空白归一
    #[test]
    fn filter_from_level_normalizes_case_and_whitespace() {
        let msgs = collect_with_filter(filter_from_level(" DEBUG "), || {
            tracing::debug!(target: "ep_daemon", "debug-kept");
        });
        assert_eq!(msgs, vec!["debug-kept"]);
    }

    // 3. 非法级别名回退 info（不 panic、不静默全关/全开）
    #[test]
    fn filter_from_level_invalid_falls_back_to_info() {
        let msgs = collect_with_filter(filter_from_level("verbose!"), || {
            tracing::info!(target: "ep_daemon", "info-kept");
            tracing::debug!(target: "ep_daemon", "debug-dropped");
        });
        assert_eq!(msgs, vec!["info-kept"]);
    }

    // 4. reload handle 运行期改级：改前被过滤的事件改后可见
    #[test]
    fn reload_handle_changes_filtering_live() {
        let (filter_layer, handle) = reload::Layer::new(filter_from_level("info"));
        let layer = CaptureLayer::default();
        let messages = layer.messages.clone();
        let subscriber = tracing_subscriber::registry().with(filter_layer).with(layer);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "ep_daemon", "i1");
            tracing::debug!(target: "ep_daemon", "d1-dropped");
            handle.reload(filter_from_level("debug")).unwrap();
            tracing::debug!(target: "ep_daemon", "d2-kept");
        });

        assert_eq!(
            messages.lock().unwrap().clone(),
            vec!["i1", "d2-kept"],
            "reload 前 debug 被过滤，reload 后放行"
        );
    }

    // 5. RUST_LOG 优先于配置值
    #[test]
    fn rust_log_takes_precedence_over_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var(EnvFilter::DEFAULT_ENV, "warn");

        // 配置给 debug，但 RUST_LOG=warn 生效 → debug 仍被过滤
        let msgs = collect_with_filter(build_env_filter("debug"), || {
            tracing::debug!(target: "ep_daemon", "debug-dropped");
            tracing::warn!(target: "ep_daemon", "warn-kept");
        });
        assert_eq!(msgs, vec!["warn-kept"]);

        std::env::remove_var(EnvFilter::DEFAULT_ENV);

        // 无 RUST_LOG → 配置值生效
        let msgs = collect_with_filter(build_env_filter("debug"), || {
            tracing::debug!(target: "ep_daemon", "debug-kept");
        });
        assert_eq!(msgs, vec!["debug-kept"]);
    }

    // 6. 非法 RUST_LOG 忽略，回退配置值
    #[test]
    fn invalid_rust_log_falls_back_to_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var(EnvFilter::DEFAULT_ENV, "%%%not-a-filter%%%");

        let msgs = collect_with_filter(build_env_filter("debug"), || {
            tracing::debug!(target: "ep_daemon", "debug-kept");
        });
        assert_eq!(msgs, vec!["debug-kept"]);

        std::env::remove_var(EnvFilter::DEFAULT_ENV);
    }

    // 7. 无可 reload 的全局 subscriber（测试环境）→ apply 返回 false 且不 panic
    #[test]
    fn apply_log_level_without_subscriber_returns_false() {
        assert!(!apply_log_level("debug"));
    }
}
