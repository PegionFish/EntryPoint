//! Daemon application state — holds all ep-core managers behind Arc<RwLock<…>>.

use std::collections::HashMap;
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
///
/// 旧端点 /ws/logs 直接转发；统一端点 /ws 由 `ws/all.rs` 映射为
/// [`WsMessage::Log`] 后转发（两端共用本通道，保持兼容，不删除）。
#[derive(Debug, Clone, Serialize)]
pub struct LogMessage {
    pub module_id: String,
    pub line: String,
}

/// A progress event for a pipeline node execution.
///
/// 旧端点 /ws/progress 直接转发；统一端点 /ws 由 `ws/all.rs` 映射为
/// [`WsMessage::Progress`] 后转发（两端共用本通道，保持兼容，不删除）。
///
/// `task_id`（P2-7，Wave 2 B3）：并发任务的进度按 task_id 过滤，
/// 修画布状态串染；旧消费者忽略该新增字段不受影响。
#[derive(Debug, Clone, Serialize)]
pub struct ProgressMessage {
    pub pipeline_id: String,
    pub task_id: String,
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
        /// 任务 ID（P2-7：并发任务进度过滤，Wave 2 B3）
        task_id: String,
        node_id: String,
        status: String,
    },
    // 生产者：api/models.rs 下载流程（Wave 2 B6）。
    ModelDownload {
        module_id: String,
        model_id: String,
        percent: f32,
        state: String,
        bytes: u64,
    },
    /// 整合包导入进度（§8.2 新增 WS 消息类型 `pack_import`）。
    ///
    /// 形状对齐前端 `WsPackImportMessage`（仲裁 #3）：
    /// `pack_id` 必有，`stage`/`percent`/`state`/`message` 可选。
    /// 生产者：`api::packs` 导入/构建后台任务（B2）。
    /// 经 `model_download_tx`（通用 WsMessage 通道）投递到 GET /ws。
    PackImport {
        pack_id: String,
        /// 当前阶段（B1 阶段名 extracting/verifying/manifest/models/pipelines/
        /// registering 直传；daemon 侧另有 accepted/done/build 包络阶段）
        #[serde(skip_serializing_if = "Option::is_none")]
        stage: Option<String>,
        /// 百分比 0-100；无法估算进度时缺失
        #[serde(skip_serializing_if = "Option::is_none")]
        percent: Option<f32>,
        /// 进度态：running / completed / failed
        #[serde(skip_serializing_if = "Option::is_none")]
        state: Option<String>,
        /// 阶段说明或错误信息
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
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
    /// 通用 [`WsMessage`] 事件通道（容量 64）：GET /ws 原样转发其中任意变体。
    /// 生产者：模型下载（`ModelDownload`，B6）、整合包导入进度
    /// （`PackImport`，B2）。字段名保留历史名称，勿改名（多代理引用）。
    pub model_download_tx: broadcast::Sender<WsMessage>,
    /// 进行中的模型下载表。键约定：`"{module_id}:{model_id}"`。
    ///
    /// 生产消费方：`api/models.rs`（模型下载）与 `api/packs.rs`（整合包内
    /// 模型下载，B2）；`GET /api/models/downloads` 直接读取本表。
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

        // 进度事件生产方在 execution.rs：每次执行自建独立 PipelineRunnerImpl，
        // 节点回调携带真实 pipeline_id/task_id 发送到 progress_tx（P2-7）。

        // P1-8（仲裁 #12）：ProcessManager 注入共享 CUDA 库目录（Linux
        // LD_LIBRARY_PATH / Windows PATH 前置，平台分支在 process.rs 内部）
        // 与网络代理环境变量。桌面端同款接线归 C4（ep-desktop main.rs）。
        let cuda_libs_dir =
            ep_core::process::resolve_cuda_libs_dir(&root, &config.compute.cuda_libs_dir);
        let network_env = config.network.env_vars();

        // Wave 2 B3（P1-4）：任务注册表落盘持久化目录绑定（runtime/tasks/）。
        // 幂等：同目录重复绑定无操作；进程级注册表首次绑定时回读既有索引，
        // daemon 重启后 GET /api/tasks 立即可见历史任务。
        crate::api::execute::execution::bind_persistence(&root);

        Self {
            root,
            config: Arc::new(RwLock::new(config)),
            devices: Arc::new(RwLock::new(devices)),
            modules: Arc::new(RwLock::new(modules)),
            process_manager: Arc::new(RwLock::new(
                ProcessManager::new()
                    .with_cuda_libs_dir(cuda_libs_dir)
                    .with_network_env(network_env),
            )),
            port_manager: Arc::new(RwLock::new(port_manager)),
            log_tx,
            progress_tx,
            model_download_tx,
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
            task_id: "task-1".into(),
            node_id: "n1".into(),
            status: "running".into(),
        };
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "progress");
        assert_eq!(v["pipeline_id"], "p1");
        assert_eq!(v["task_id"], "task-1");
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

    // WS pack_import 形状（§8.2 + 仲裁 #3：对齐前端 WsPackImportMessage）
    #[test]
    fn ws_message_serde_pack_import_full() {
        let msg = WsMessage::PackImport {
            pack_id: "pigeonfish.subtitle-kit".into(),
            stage: Some("unpack".into()),
            percent: Some(42.0),
            state: Some("running".into()),
            message: Some("解包中".into()),
        };
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "pack_import");
        assert_eq!(v["pack_id"], "pigeonfish.subtitle-kit");
        assert_eq!(v["stage"], "unpack");
        assert_eq!(v["percent"], 42.0);
        assert_eq!(v["state"], "running");
        assert_eq!(v["message"], "解包中");
    }

    // pack_import 可选字段缺省时不出现在 JSON 中（前端按 undefined 处理）
    #[test]
    fn ws_message_serde_pack_import_minimal() {
        let msg = WsMessage::PackImport {
            pack_id: "a.b".into(),
            stage: None,
            percent: None,
            state: None,
            message: None,
        };
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "pack_import");
        assert_eq!(v["pack_id"], "a.b");
        assert!(v.get("stage").is_none());
        assert!(v.get("percent").is_none());
        assert!(v.get("state").is_none());
        assert!(v.get("message").is_none());
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
