//! 后台模型更新自动检查（P1-10/P2-1：`general.check_updates` 接线）。
//!
//! `general.check_updates` 开关语义：
//! - `false` → 启动期与定时自动检查全部跳过（不触网）；
//! - `true`  → daemon 启动 [`STARTUP_CHECK_DELAY`] 后首轮检查，
//!   此后每 [`CHECK_INTERVAL`] 一轮。
//!
//! 开关在**每轮检查执行时实时读取**——运行期经 PUT /api/config 改动即时生效，
//! 无需重启（`requires_restart` 不含 `check_updates`）。
//!
//! 仅检查已有 `.ep_meta.json`（即已下载）的模型；手动「检查更新」入口
//! （POST /api/models/.../check-update）不受本开关约束——用户主动触发的
//! 检查始终可用。检查结果经 tracing 记录；发现可用更新时另广播
//! [`WsMessage::ModelUpdate`]（前端消费为后续扩展，未知 type 按现有协议忽略）。

use std::sync::Arc;
use std::time::Duration;

use ep_core::model::ModelManager;
use ep_core::module::manifest::ModelDecl;

use crate::state::{AppState, WsMessage};

/// 启动后首轮检查延迟：避开依赖安装/模块启动等启动高峰的 I/O 与网络争用
pub const STARTUP_CHECK_DELAY: Duration = Duration::from_secs(15);

/// 定时检查间隔（每轮实时重读开关）
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// 单轮检查结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleOutcome {
    /// 开关关闭 → 整轮跳过（不触网）
    Disabled,
    /// 已执行检查：`checked` = 实际触网检查的模型数（已下载且有 meta），
    /// `available` = 其中发现可用更新的数量
    Checked { checked: usize, available: usize },
}

/// 自动更新检查是否开启（`general.check_updates` 的唯一消费点）
pub fn auto_check_enabled(config: &ep_core::config::AppConfig) -> bool {
    config.general.check_updates
}

/// 执行一轮全量模型更新检查。
///
/// 枚举所有模块 manifest 声明的模型，只检查存在 `.ep_meta.json` 的
///（已下载）；未下载的本地短路跳过，不触网。逐模型 best-effort 经
/// ep-core [`ModelManager::check_update_available`]（网络失败按无更新计）。
pub async fn run_update_check_cycle(state: &Arc<AppState>) -> CycleOutcome {
    // 读开关 + 收集检查目标（配置与模块清单快照），锁在网络请求前释放
    let (mgr, targets) = {
        let config = state.config.read().await;
        if !auto_check_enabled(&config) {
            return CycleOutcome::Disabled;
        }
        let modules = state.modules.read().await;
        let mgr =
            ModelManager::new(&config.models, &state.root).with_network(config.network.clone());
        let targets: Vec<(String, ModelDecl)> = modules
            .iter()
            .filter_map(|m| m.manifest.as_ref())
            .flat_map(|mf| {
                mf.models
                    .iter()
                    .map(move |d| (mf.module.id.clone(), d.clone()))
            })
            .collect();
        (mgr, targets)
    };

    let mut checked = 0usize;
    let mut available = 0usize;
    for (module_id, decl) in targets {
        // 未下载（无 meta）→ 无法比较版本，本地短路
        if mgr.read_meta(&decl.target_dir).is_none() {
            continue;
        }
        checked += 1;
        let result = mgr.check_update_available(&decl).await;
        if result.available {
            available += 1;
            let lang = state.lang().await;
            let reason = ep_core::i18n::t(
                &lang,
                "apiModels.updateAvailable",
                &[("info", result.remote_modified.as_deref().unwrap_or(""))],
            );
            tracing::info!(
                module_id = %module_id,
                model_id = %decl.id,
                "auto update check: newer version available"
            );
            let _ = state.model_download_tx.send(WsMessage::ModelUpdate {
                module_id,
                model_id: decl.id.clone(),
                reason,
            });
        } else {
            tracing::debug!(
                module_id = %module_id,
                model_id = %decl.id,
                "auto update check: up to date or not checkable"
            );
        }
    }

    CycleOutcome::Checked { checked, available }
}

/// Spawn 后台自动更新检查循环：启动 [`STARTUP_CHECK_DELAY`] 后首轮，
/// 此后每 [`CHECK_INTERVAL`] 一轮；开关每轮实时读取（热跟随配置变更）。
pub fn spawn_auto_update_checker(state: Arc<AppState>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tokio::time::sleep(STARTUP_CHECK_DELAY).await;
        loop {
            match run_update_check_cycle(&state).await {
                CycleOutcome::Disabled => {
                    tracing::debug!(
                        "auto update check skipped (general.check_updates = false)"
                    );
                }
                CycleOutcome::Checked { checked, available } => {
                    tracing::info!(checked, available, "auto update check cycle finished");
                }
            }
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    })
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ep_core::config::AppConfig;
    use ep_core::module::discovery::{DiscoveredModule, DiscoveryStatus};
    use ep_core::port::PortManager;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// 构造测试 AppState：可指定开关状态与模块清单
    fn test_state(check_updates: bool, modules: Vec<DiscoveredModule>) -> Arc<AppState> {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-daemon-updates-test-{}-{seq}",
            std::process::id()
        ));
        let mut config = AppConfig::default();
        config.general.check_updates = check_updates;
        Arc::new(AppState::new(
            root,
            config,
            vec![],
            modules,
            PortManager::new(18000, 19000),
        ))
    }

    /// 单模型测试 manifest（huggingface 源 + 未下载）
    fn fixture_module(root: &std::path::Path) -> DiscoveredModule {
        let manifest = toml::from_str(
            r#"
[module]
id = "upd-mod"
name = "Update Test Module"
version = "1.0.0"
description = "updates.rs test fixture"
category = "asr"
genre = "test"

[runtime]
type = "python"
python_version = ">=3.10"

[compute]
backends = ["cpu"]

[[models]]
id = "m1"
name = "测试模型"
source = "huggingface"
repo_id = "org/test-model"
target_dir = "upd-test-model"

[interface]
type = "http"
"#,
        )
        .expect("fixture manifest 应合法");
        DiscoveredModule {
            path: root.join("modules").join("upd-mod"),
            manifest: Some(manifest),
            status: DiscoveryStatus::Valid,
        }
    }

    // 1. 开关读取：true/false 直接映射
    #[test]
    fn auto_check_enabled_reads_flag() {
        let mut config = AppConfig::default();
        assert!(auto_check_enabled(&config), "默认 true");
        config.general.check_updates = false;
        assert!(!auto_check_enabled(&config));
    }

    // 2. 开关关闭 → 整轮跳过（Disabled，不触网）
    #[tokio::test]
    async fn cycle_disabled_when_switch_off() {
        let state = test_state(false, vec![]);
        assert_eq!(
            run_update_check_cycle(&state).await,
            CycleOutcome::Disabled
        );
    }

    // 3. 开关开启 + 无模块 → Checked{0, 0}
    #[tokio::test]
    async fn cycle_enabled_no_modules_checks_zero() {
        let state = test_state(true, vec![]);
        assert_eq!(
            run_update_check_cycle(&state).await,
            CycleOutcome::Checked {
                checked: 0,
                available: 0
            }
        );
    }

    // 4. 已声明但未下载（无 .ep_meta.json）→ 本地短路，不触网，checked=0
    #[tokio::test]
    async fn cycle_skips_models_without_meta() {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-daemon-updates-nometa-{}-{seq}",
            std::process::id()
        ));
        let module = fixture_module(&root);
        let state = test_state(true, vec![module]);

        assert_eq!(
            run_update_check_cycle(&state).await,
            CycleOutcome::Checked {
                checked: 0,
                available: 0
            },
            "未下载模型不得触网检查"
        );
    }

    // 5. 开关关闭时即便有已声明模型也整轮跳过（与 #4 对照）
    #[tokio::test]
    async fn cycle_disabled_even_with_declared_models() {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-daemon-updates-off-{}-{seq}",
            std::process::id()
        ));
        let module = fixture_module(&root);
        let state = test_state(false, vec![module]);

        assert_eq!(
            run_update_check_cycle(&state).await,
            CycleOutcome::Disabled
        );
    }
}
