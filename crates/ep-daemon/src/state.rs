//! Daemon application state — holds all ep-core managers behind Arc<RwLock<…>>.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{broadcast, RwLock};

use ep_core::config::AppConfig;
use ep_core::module::discovery::DiscoveredModule;
use ep_core::port::PortManager;
use ep_core::process::ProcessManager;
use ep_core::types::ComputeDevice;

// ─── Broadcast message types ────────────────────────────────────────────────

/// A log line emitted by a running module service.
#[derive(Debug, Clone, Serialize)]
pub struct LogMessage {
    pub module_id: String,
    pub line: String,
}

/// A progress event for a pipeline node execution.
#[derive(Debug, Clone, Serialize)]
pub struct ProgressMessage {
    pub pipeline_id: String,
    pub node_id: String,
    pub status: String,
}

// ─── AppState ───────────────────────────────────────────────────────────────

/// Shared application state injected into every handler via `State<Arc<AppState>>`.
#[derive(Clone)]
pub struct AppState {
    /// 项目根目录（绝对路径），所有相对路径的基准
    pub root: PathBuf,
    pub config: Arc<RwLock<AppConfig>>,
    pub devices: Arc<RwLock<Vec<ComputeDevice>>>,
    pub modules: Arc<RwLock<Vec<DiscoveredModule>>>,
    pub process_manager: Arc<RwLock<ProcessManager>>,
    pub port_manager: Arc<RwLock<PortManager>>,
    pub log_tx: broadcast::Sender<LogMessage>,
    pub progress_tx: broadcast::Sender<ProgressMessage>,
}

impl AppState {
    /// Build a fully-wired `AppState` from startup artefacts.
    pub fn new(
        root: PathBuf,
        config: AppConfig,
        devices: Vec<ComputeDevice>,
        modules: Vec<DiscoveredModule>,
        port_manager: PortManager,
    ) -> Self {
        let (log_tx, _) = broadcast::channel(256);
        let (progress_tx, _) = broadcast::channel(256);

        Self {
            root,
            config: Arc::new(RwLock::new(config)),
            devices: Arc::new(RwLock::new(devices)),
            modules: Arc::new(RwLock::new(modules)),
            process_manager: Arc::new(RwLock::new(ProcessManager::new())),
            port_manager: Arc::new(RwLock::new(port_manager)),
            log_tx,
            progress_tx,
        }
    }

}
