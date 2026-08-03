//! Daemon application state — holds all ep-core managers behind Arc<RwLock<…>>.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{broadcast, Mutex as TokioMutex, RwLock};

use ep_core::config::AppConfig;
use ep_core::module::discovery::DiscoveredModule;
use ep_core::pipeline::PipelineRunnerImpl;
use ep_core::port::PortManager;
use ep_core::process::ProcessManager;
use ep_core::types::ComputeDevice;

// ─── Broadcast message types ────────────────────────────────────────────────

/// A log line emitted by a running module service.
///
/// 仅供旧端点 /ws/logs 使用（保持兼容，不删除）。新端点 /ws 统一使用 [`WsMessage`]。
#[derive(Debug, Clone, Serialize)]
pub struct LogMessage {
    pub module_id: String,
    pub line: String,
}

/// A progress event for a pipeline node execution.
///
/// 仅供旧端点 /ws/progress 使用（保持兼容，不删除）。新端点 /ws 统一使用 [`WsMessage`]。
#[derive(Debug, Clone, Serialize)]
pub struct ProgressMessage {
    pub pipeline_id: String,
    pub node_id: String,
    pub status: String,
}

/// 统一 WebSocket 消息协议（GET /ws）。
///
/// 序列化为 `{"type": "log" | "progress" | "model_download", ...}`，
/// 前端按 `msg.type` 过滤。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsMessage {
    Log {
        module_id: String,
        line: String,
    },
    Progress {
        pipeline_id: String,
        node_id: String,
        status: String,
    },
    // Wave 2 下载代理（W2-A）才会构造此变体；daemon 侧当前无生产者，
    // 预置协议形状以免后续代理改动本枚举。
    #[allow(dead_code)]
    ModelDownload {
        module_id: String,
        model_id: String,
        percent: f32,
        state: String,
        bytes: u64,
    },
}

// ─── Wave 2 预置类型 ────────────────────────────────────────────────────────

/// 进行中的模型下载条目（供 GET /api/models/downloads 与 /ws ModelDownload 使用）。
///
/// Wave 2 代理 W2-A 实现真实下载时维护此结构（字段已定型，勿改名）。
#[derive(Debug, Clone, Serialize)]
pub struct DownloadEntry {
    pub module_id: String,
    pub model_id: String,
    pub source: String,
    pub percent: f32,
    pub bytes: u64,
    pub state: String,
    pub started_at: String,
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
    /// 模型下载进度事件通道（WsMessage::ModelDownload），容量 64。
    /// 生产者：Wave 2 下载代理（W2-A）；消费者：GET /ws。
    pub model_download_tx: broadcast::Sender<WsMessage>,
    /// 管线执行器（Wave 2 骨架，真实初始化）。
    /// 所有者：W2-B（任务/管线 API）。handler 通过 `state.runner.lock().await` 获取。
    /// `#[allow(dead_code)]`：Wave 2 接管前 daemon 内暂无读取方，属预置骨架字段。
    #[allow(dead_code)]
    pub runner: Arc<TokioMutex<PipelineRunnerImpl>>,
    /// 进行中的模型下载表。键约定：`"{module_id}:{model_id}"`。
    /// 所有者：W2-A（模型下载 API）。
    /// `#[allow(dead_code)]`：同上，Wave 2 接管前的预置骨架字段。
    #[allow(dead_code)]
    pub downloads: Arc<std::sync::Mutex<HashMap<String, DownloadEntry>>>,
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
        let (model_download_tx, _) = broadcast::channel(64);

        // 管线执行器：用 config 解析后的 workspace 目录真实初始化
        // （main.rs 启动时已调用 resolve_paths，workspace_dir 为绝对路径；
        //  resolve_workspace_dir 对相对路径同样安全）。
        let workspace_dir = config.resolve_workspace_dir(&root);
        let _ = std::fs::create_dir_all(&workspace_dir);
        let mut runner = PipelineRunnerImpl::new(workspace_dir);

        // 进度回调骨架接线：节点事件 → progress_tx。
        // pipeline_id 暂为空占位（runner 回调签名只带 node_id），
        // W2-B 实现执行/任务 API 时替换为真实管线上下文。
        {
            let tx = progress_tx.clone();
            runner.on_node_start = Some(Arc::new(move |node_id| {
                let _ = tx.send(ProgressMessage {
                    pipeline_id: String::new(),
                    node_id: node_id.to_string(),
                    status: "running".to_string(),
                });
            }));
            let tx = progress_tx.clone();
            runner.on_node_complete = Some(Arc::new(move |node_id, _artifact| {
                let _ = tx.send(ProgressMessage {
                    pipeline_id: String::new(),
                    node_id: node_id.to_string(),
                    status: "completed".to_string(),
                });
            }));
            let tx = progress_tx.clone();
            runner.on_node_error = Some(Arc::new(move |node_id, err| {
                let _ = tx.send(ProgressMessage {
                    pipeline_id: String::new(),
                    node_id: node_id.to_string(),
                    status: format!("error: {err}"),
                });
            }));
        }

        Self {
            root,
            config: Arc::new(RwLock::new(config)),
            devices: Arc::new(RwLock::new(devices)),
            modules: Arc::new(RwLock::new(modules)),
            process_manager: Arc::new(RwLock::new(ProcessManager::new())),
            port_manager: Arc::new(RwLock::new(port_manager)),
            log_tx,
            progress_tx,
            model_download_tx,
            runner: Arc::new(TokioMutex::new(runner)),
            downloads: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// 当前 UI 语言码：读取 `config.general.language` 并归一化为 `"zh-CN"` / `"en"`。
    ///
    /// 供 `crate::api::err_response`（i18n 错误响应）使用。
    pub async fn lang(&self) -> String {
        let raw = self.config.read().await.general.language.clone();
        ep_core::i18n::normalize_language(&raw).to_string()
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // WsMessage 序列化形状：三种 type 标签
    #[test]
    fn ws_message_serde_log() {
        let msg = WsMessage::Log {
            module_id: "faster-whisper".into(),
            line: "hello".into(),
        };
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "log");
        assert_eq!(v["module_id"], "faster-whisper");
        assert_eq!(v["line"], "hello");
    }

    #[test]
    fn ws_message_serde_progress() {
        let msg = WsMessage::Progress {
            pipeline_id: "p1".into(),
            node_id: "n1".into(),
            status: "running".into(),
        };
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "progress");
        assert_eq!(v["pipeline_id"], "p1");
        assert_eq!(v["node_id"], "n1");
        assert_eq!(v["status"], "running");
    }

    #[test]
    fn ws_message_serde_model_download() {
        let msg = WsMessage::ModelDownload {
            module_id: "faster-whisper".into(),
            model_id: "large-v3".into(),
            percent: 42.5,
            state: "downloading".into(),
            bytes: 12345,
        };
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "model_download");
        assert_eq!(v["module_id"], "faster-whisper");
        assert_eq!(v["model_id"], "large-v3");
        assert_eq!(v["percent"], 42.5);
        assert_eq!(v["state"], "downloading");
        assert_eq!(v["bytes"], 12345);
    }

    // lang()：读取 config.general.language 并归一化
    #[tokio::test]
    async fn lang_normalizes_config_language() {
        let state = AppState::new(
            std::env::temp_dir().join(format!("ep_state_lang_{}", std::process::id())),
            AppConfig::default(),
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        );
        // 默认配置 zh-CN
        assert_eq!(state.lang().await, "zh-CN");

        state.config.write().await.general.language = "en-US".into();
        assert_eq!(state.lang().await, "en");

        state.config.write().await.general.language = "fr".into();
        assert_eq!(state.lang().await, "zh-CN");
    }
}
