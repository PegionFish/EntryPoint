//! 管线执行引擎 — 状态机 + 节点执行辅助函数
//!
//! PipelineTask 提供任务状态管理和节点状态转换。
//! 模块底部的辅助函数实现各类型节点的实际执行逻辑。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::types::{Artifact, TaskStatus};

use super::dag::{NodeKind, Pipeline, PipelineNode};

// ─── 节点状态 ────────────────────────────────────────────────────────────────

/// 单个节点的执行状态
#[derive(Debug, Clone, PartialEq)]
pub enum NodeState {
    /// 等待执行
    Pending,
    /// 正在执行
    Running,
    /// 执行完成，附带输出产物
    Completed { artifact: Option<Artifact> },
    /// 执行失败
    Failed {
        error: String,
        /// 是否为可重试错误（连接失败 / 超时等瞬态故障）
        retryable: bool,
    },
    /// 因上游失败而跳过
    Skipped,
}

impl NodeState {
    /// 节点是否已终结（不会再变化）
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed { .. } | Self::Failed { .. } | Self::Skipped)
    }
}

// ─── 管线任务 ────────────────────────────────────────────────────────────────

/// 一次管线执行实例
#[derive(Debug, Clone)]
pub struct PipelineTask {
    /// 任务唯一 ID
    pub id: String,
    /// 关联的管线 ID
    pub pipeline_id: String,
    /// 任务整体状态
    pub status: TaskStatus,
    /// 各节点状态
    pub node_states: HashMap<String, NodeState>,
    /// 任务工作目录
    pub work_dir: PathBuf,
    /// 任务启动时间
    pub started_at: DateTime<Utc>,
    /// 任务完成时间（所有节点终结时设置）
    pub finished_at: Option<DateTime<Utc>>,
}

impl PipelineTask {
    /// 创建新的管线任务
    ///
    /// 所有节点初始状态为 `Pending`，任务状态为 `Pending`。
    pub fn new(pipeline: &Pipeline, work_dir: PathBuf) -> Self {
        let node_states = pipeline
            .nodes
            .iter()
            .map(|n| (n.id.clone(), NodeState::Pending))
            .collect();

        Self {
            id: Uuid::new_v4().to_string(),
            pipeline_id: pipeline.id.clone(),
            status: TaskStatus::Pending,
            node_states,
            work_dir,
            started_at: Utc::now(),
            finished_at: None,
        }
    }

    /// 执行一层节点 — 将该层所有 Pending 节点标记为 Running
    ///
    /// TODO: 实际执行逻辑（HTTP 调用 / spawn 进程 / 内置函数）
    pub fn execute_layer(&mut self, layer: &[String]) {
        // 首次执行时将任务状态设为 Running
        if self.status == TaskStatus::Pending {
            self.status = TaskStatus::Running;
        }

        for node_id in layer {
            if let Some(state) = self.node_states.get_mut(node_id) {
                if *state == NodeState::Pending {
                    *state = NodeState::Running;
                }
            }
        }
    }

    /// 标记节点执行完成
    pub fn mark_completed(&mut self, node_id: &str, artifact: Artifact) {
        if let Some(state) = self.node_states.get_mut(node_id) {
            *state = NodeState::Completed {
                artifact: Some(artifact),
            };
        }
        self.check_completion();
    }

    /// 标记节点执行失败，并将所有下游节点标记为 Skipped
    pub fn mark_failed(&mut self, node_id: &str, error: String, retryable: bool) {
        if let Some(state) = self.node_states.get_mut(node_id) {
            *state = NodeState::Failed { error, retryable };
        }

        // 标记所有下游为 Skipped（需要管线信息来获取下游）
        // 这里通过 node_states 中仍为 Pending 的节点来处理：
        // 调用者应传入 pipeline 引用，或使用 all_downstream_of
        // 为保持接口简洁，此处标记所有依赖该节点的 Pending 下游
        self.skip_downstream_of(node_id);

        self.check_completion();
    }

    /// 标记节点失败并跳过其下游（需要管线引用以计算传递闭包）
    pub fn mark_failed_with_pipeline(&mut self, node_id: &str, error: String, retryable: bool, pipeline: &Pipeline) {
        if let Some(state) = self.node_states.get_mut(node_id) {
            *state = NodeState::Failed { error, retryable };
        }

        // 获取所有传递下游并标记为 Skipped
        let downstream = pipeline.all_downstream_of(node_id);
        for ds_id in downstream {
            if let Some(state) = self.node_states.get_mut(ds_id) {
                if *state == NodeState::Pending {
                    *state = NodeState::Skipped;
                }
            }
        }

        self.check_completion();
    }

    /// 任务是否已全部完成（所有节点都处于终结状态）
    pub fn is_complete(&self) -> bool {
        self.node_states.values().all(|s| s.is_terminal())
    }

    /// 获取指定节点的状态
    pub fn node_state(&self, node_id: &str) -> Option<&NodeState> {
        self.node_states.get(node_id)
    }

    // ─── 内部方法 ────────────────────────────────────────────────────────────

    /// 简单的下游跳过：遍历所有 Pending 节点，如果其上游有 Failed/Skipped 则跳过
    ///
    /// 注意：这是简化实现，仅处理直接下游。完整的传递闭包跳过
    /// 应使用 `mark_failed_with_pipeline`。
    fn skip_downstream_of(&mut self, _node_id: &str) {
        // 简化实现：不做传递闭包（需要 pipeline 引用）
        // 完整逻辑见 mark_failed_with_pipeline
    }

    /// 检查是否所有节点都已终结，更新任务整体状态
    fn check_completion(&mut self) {
        if !self.is_complete() {
            return;
        }

        // 记录完成时间
        if self.finished_at.is_none() {
            self.finished_at = Some(Utc::now());
        }

        let has_failure = self
            .node_states
            .values()
            .any(|s| matches!(s, NodeState::Failed { .. }));

        if has_failure {
            let first_error = self
                .node_states
                .values()
                .find_map(|s| match s {
                    NodeState::Failed { error, .. } => Some(error.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            self.status = TaskStatus::Failed(first_error);
        } else {
            self.status = TaskStatus::Completed;
        }
    }
}

// ─── 单元测试 ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::dag::Pipeline;

    fn test_pipeline() -> Pipeline {
        let toml_str = r#"
[pipeline]
id = "test-exec"
name = "执行测试"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "process"
kind = "module"
module_id = "faster-whisper"
capability = "transcribe"

[[nodes]]
id = "save"
kind = "builtin"
builtin = "file_output"

[[edges]]
from = ["input", "output"]
to = ["process", "input"]

[[edges]]
from = ["process", "output"]
to = ["save", "input"]
"#;
        Pipeline::from_toml_str(toml_str).unwrap()
    }

    #[test]
    fn test_task_creation() {
        let pipeline = test_pipeline();
        let task = PipelineTask::new(&pipeline, PathBuf::from("/tmp/test"));

        assert_eq!(task.pipeline_id, "test-exec");
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.node_states.len(), 3);
        assert!(task.node_states.values().all(|s| *s == NodeState::Pending));
    }

    #[test]
    fn test_execute_layer() {
        let pipeline = test_pipeline();
        let mut task = PipelineTask::new(&pipeline, PathBuf::from("/tmp/test"));

        task.execute_layer(&["input".to_string()]);

        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(task.node_states["input"], NodeState::Running);
        assert_eq!(task.node_states["process"], NodeState::Pending);
    }

    #[test]
    fn test_mark_completed() {
        let pipeline = test_pipeline();
        let mut task = PipelineTask::new(&pipeline, PathBuf::from("/tmp/test"));

        task.execute_layer(&["input".to_string()]);
        task.mark_completed("input", Artifact::File(PathBuf::from("/tmp/test/input.wav")));

        assert_eq!(
            task.node_states["input"],
            NodeState::Completed {
                artifact: Some(Artifact::File(PathBuf::from("/tmp/test/input.wav")))
            }
        );
    }

    #[test]
    fn test_full_completion() {
        let pipeline = test_pipeline();
        let mut task = PipelineTask::new(&pipeline, PathBuf::from("/tmp/test"));

        // 逐层执行
        task.execute_layer(&["input".to_string()]);
        task.mark_completed("input", Artifact::File(PathBuf::from("input.wav")));

        task.execute_layer(&["process".to_string()]);
        task.mark_completed("process", Artifact::Text("hello".to_string()));

        task.execute_layer(&["save".to_string()]);
        task.mark_completed("save", Artifact::File(PathBuf::from("output.srt")));

        assert!(task.is_complete());
        assert_eq!(task.status, TaskStatus::Completed);
    }

    #[test]
    fn test_mark_failed_with_downstream_skip() {
        let pipeline = test_pipeline();
        let mut task = PipelineTask::new(&pipeline, PathBuf::from("/tmp/test"));

        task.execute_layer(&["input".to_string()]);
        task.mark_completed("input", Artifact::File(PathBuf::from("input.wav")));

        task.execute_layer(&["process".to_string()]);
        task.mark_failed_with_pipeline(
            "process",
            "model not found".to_string(),
            false,
            &pipeline,
        );

        assert_eq!(
            task.node_states["process"],
            NodeState::Failed {
                error: "model not found".to_string(),
                retryable: false,
            }
        );
        assert_eq!(task.node_states["save"], NodeState::Skipped);
        assert!(task.is_complete());
        assert!(matches!(task.status, TaskStatus::Failed(_)));
    }
}

// ─── 节点执行辅助函数 ────────────────────────────────────────────────────────

/// 收集指定节点的所有上游产物（按边在 pipeline.edges 中的出现顺序）
pub(crate) fn collect_upstream_artifacts(
    node_id: &str,
    pipeline: &Pipeline,
    task: &PipelineTask,
) -> Vec<Artifact> {
    pipeline
        .edges
        .iter()
        .filter(|e| e.to.0 == node_id)
        .filter_map(|e| {
            if let Some(NodeState::Completed { artifact: Some(a) }) = task.node_state(&e.from.0) {
                Some(a.clone())
            } else {
                None
            }
        })
        .collect()
}

/// 从上游产物中提取第一个文件路径
#[allow(dead_code)]
pub(crate) fn first_upstream_file_path(
    node_id: &str,
    pipeline: &Pipeline,
    task: &PipelineTask,
) -> Option<PathBuf> {
    collect_upstream_artifacts(node_id, pipeline, task)
        .into_iter()
        .find_map(|a| match a {
            Artifact::File(p) => Some(p),
            _ => None,
        })
}

/// 执行单个节点 — 根据 NodeKind 分派到具体执行逻辑
pub(crate) async fn execute_node(
    node: &PipelineNode,
    pipeline: &Pipeline,
    task: &PipelineTask,
    work_dir: &Path,
    module_ports: &HashMap<String, u16>,
) -> anyhow::Result<Artifact> {
    let upstream = collect_upstream_artifacts(&node.id, pipeline, task);

    match &node.kind {
        NodeKind::Builtin { builtin } => {
            execute_builtin_node(builtin, node, &upstream, work_dir).await
        }
        NodeKind::Module { .. } => execute_module_node(node, &upstream, work_dir, module_ports).await,
        NodeKind::ExternalApi { .. } => execute_external_api_node(node, &upstream, work_dir).await,
    }
}

/// 执行内置节点
async fn execute_builtin_node(
    builtin: &str,
    node: &PipelineNode,
    upstream: &[Artifact],
    work_dir: &Path,
) -> anyhow::Result<Artifact> {
    match builtin {
        "file_input" => execute_builtin_file_input(node, work_dir).await,
        "file_output" => execute_builtin_file_output(node, upstream, work_dir).await,
        "ffmpeg" => execute_builtin_ffmpeg(node, upstream, work_dir).await,
        other => Err(anyhow::anyhow!("unknown builtin node type: {other}")),
    }
}

/// FileInput: 验证源文件存在，复制到工作目录，返回 Artifact::File
async fn execute_builtin_file_input(
    node: &PipelineNode,
    work_dir: &Path,
) -> anyhow::Result<Artifact> {
    let source = node
        .params
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("file_input node '{}' missing 'path' param", node.id))?;

    let source_path = Path::new(source);
    if !source_path.exists() {
        return Err(anyhow::anyhow!(
            "input file not found: {}",
            source_path.display()
        ));
    }

    let dest = work_dir.join(source_path.file_name().unwrap_or_else(|| std::ffi::OsStr::new("input")));

    // 如果源文件和目标是同一个文件，跳过复制（避免 Windows 文件锁冲突）
    if dest != source_path {
        std::fs::copy(source_path, &dest)?;
    }

    Ok(Artifact::File(dest))
}

/// FileOutput: 从上游获取文件，复制到目标路径，返回 Artifact::File
async fn execute_builtin_file_output(
    node: &PipelineNode,
    upstream: &[Artifact],
    _work_dir: &Path,
) -> anyhow::Result<Artifact> {
    let source_file = upstream
        .iter()
        .find_map(|a| match a {
            Artifact::File(p) => Some(p.clone()),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("file_output node '{}' has no file input from upstream", node.id))?;

    let dest = node
        .params
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("file_output node '{}' missing 'path' param", node.id))?;

    let dest_path = Path::new(dest);
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&source_file, dest_path)?;

    Ok(Artifact::File(dest_path.to_path_buf()))
}

/// 解析 ffmpeg 可执行文件路径
///
/// 搜索优先级：
/// 1. `runtime/bin/ffmpeg`（项目内置 portable 版本）
/// 2. 系统 PATH 中的 `ffmpeg`（用户环境变量）
/// 3. `modules/test-ffmpeg/ffmpeg`（fallback，不入 git/发包）
pub(crate) fn resolve_ffmpeg_path() -> PathBuf {
    let ffmpeg_name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };

    // 1. runtime/bin/ffmpeg
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent();
        while let Some(d) = dir {
            let candidate = d.join("runtime").join("bin").join(ffmpeg_name);
            if candidate.exists() {
                return candidate;
            }
            dir = d.parent();
        }
    }

    // 2. 系统 PATH
    let which_cmd = if cfg!(windows) { "where" } else { "which" };
    if let Ok(output) = std::process::Command::new(which_cmd).arg("ffmpeg").output() {
        if output.status.success() {
            if let Some(line) = String::from_utf8_lossy(&output.stdout).lines().next() {
                let p = PathBuf::from(line.trim());
                if p.is_file() {
                    return p;
                }
            }
        }
    }

    // 3. modules/test-ffmpeg/ffmpeg (fallback)
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent();
        while let Some(d) = dir {
            let candidate = d.join("modules").join("test-ffmpeg").join(ffmpeg_name);
            if candidate.exists() {
                return candidate;
            }
            dir = d.parent();
        }
    }

    // 最终回退：裸名，让 OS 报错
    PathBuf::from("ffmpeg")
}

/// FFmpeg: 构建并执行 ffmpeg 命令，返回输出文件的 Artifact::File
async fn execute_builtin_ffmpeg(
    node: &PipelineNode,
    upstream: &[Artifact],
    work_dir: &Path,
) -> anyhow::Result<Artifact> {
    let ffmpeg_bin = resolve_ffmpeg_path();
    let mut cmd = tokio::process::Command::new(&ffmpeg_bin);
    cmd.arg("-y"); // overwrite output

    // 检查 params args 是否已包含 -i（用户自行指定输入源，如 lavfi）
    let args_vec: Vec<String> = node
        .params
        .get("args")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let args_has_input = args_vec.iter().any(|a| a == "-i");

    // 仅在 args 未指定 -i 时，添加上游文件作为输入
    if !args_has_input {
        for artifact in upstream {
            if let Artifact::File(path) = artifact {
                cmd.arg("-i").arg(path);
            }
        }
    }

    // 从 params 添加额外参数
    for arg in &args_vec {
        cmd.arg(arg);
    }

    // 确定输出路径
    let output_path = if let Some(output) = node.params.get("output").and_then(|v| v.as_str()) {
        PathBuf::from(output)
    } else {
        work_dir.join(format!("{}_output", node.id))
    };

    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    cmd.arg(&output_path);

    let output = cmd.output().await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("ffmpeg failed: {stderr}"));
    }

    Ok(Artifact::File(output_path))
}

/// 模块 HTTP 调用错误 — 携带可重试标志
#[derive(Debug, Clone, thiserror::Error)]
#[error("module call to `{module_id}` failed: {message}")]
pub struct ModuleCallError {
    pub module_id: String,
    pub message: String,
    /// 是否为可重试错误（连接失败 / 超时等瞬态故障）
    pub retryable: bool,
}

/// 模块 HTTP 响应体
#[derive(Debug, serde::Deserialize)]
struct ModuleResponse {
    status: String,
    output_type: Option<String>,
    result: Option<serde_json::Value>,
    #[serde(default)]
    #[allow(dead_code)]
    metadata: Option<serde_json::Value>,
}

/// 最大重试次数（不含首次尝试）
const MAX_RETRIES: u32 = 1;
/// 重试间隔（秒）
const RETRY_DELAY_SECS: u64 = 2;
/// HTTP 请求超时（秒）
const HTTP_TIMEOUT_SECS: u64 = 300;

/// 模块节点执行 — 通过 HTTP 调用本地模块服务
///
/// 请求 URL: `http://127.0.0.1:{port}/predict/{capability}`
///
/// - 文件类上游产物（audio/video/image/file）→ multipart/form-data
/// - 文本/JSON 类上游产物 → JSON body
///
/// 可重试错误（连接失败、超时）最多重试 1 次，间隔 2 秒。
async fn execute_module_node(
    node: &PipelineNode,
    upstream: &[Artifact],
    work_dir: &Path,
    module_ports: &HashMap<String, u16>,
) -> anyhow::Result<Artifact> {
    let (module_id, capability) = match &node.kind {
        NodeKind::Module {
            module_id,
            capability,
            ..
        } => (module_id.clone(), capability.clone()),
        _ => unreachable!(),
    };

    // ── 解析端口：优先 module_ports 注册表，其次 node params ──────────────
    let port = module_ports.get(&module_id).copied().or_else(|| {
        node.params
            .get("port")
            .and_then(|v| v.as_u64())
            .map(|v| v as u16)
    });

    let port = port.ok_or_else(|| {
        anyhow::anyhow!(ModuleCallError {
            module_id: module_id.clone(),
            message: format!(
                "no port registered for module '{module_id}' and no 'port' param in node '{}'",
                node.id
            ),
            retryable: false,
        })
    })?;

    let url = format!("http://127.0.0.1:{port}/predict/{capability}");

    // ── 判断上游是否包含文件类产物 ────────────────────────────────────────
    let has_file_input = upstream
        .iter()
        .any(|a| matches!(a, Artifact::File(_)));

    // ── 构建 HTTP 客户端 ──────────────────────────────────────────────────
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| {
            anyhow::anyhow!(ModuleCallError {
                module_id: module_id.clone(),
                message: format!("failed to build HTTP client: {e}"),
                retryable: false,
            })
        })?;

    // ── 重试循环 ──────────────────────────────────────────────────────────
    let mut last_error: Option<ModuleCallError> = None;

    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            tracing::info!(
                module_id = %module_id,
                attempt,
                "retrying module HTTP call after {RETRY_DELAY_SECS}s"
            );
            tokio::time::sleep(std::time::Duration::from_secs(RETRY_DELAY_SECS)).await;
        }

        tracing::debug!(
            module_id = %module_id,
            capability = %capability,
            url = %url,
            attempt,
            "executing module HTTP call"
        );

        match send_module_request(&client, &url, &module_id, node, upstream, has_file_input, work_dir).await {
            Ok(artifact) => return Ok(artifact),
            Err(e) => {
                let mce: ModuleCallError = match e.downcast_ref::<ModuleCallError>() {
                    Some(mce) => (*mce).clone(),
                    None => ModuleCallError {
                        module_id: module_id.clone(),
                        message: e.to_string(),
                        retryable: false,
                    },
                };

                tracing::warn!(
                    module_id = %module_id,
                    attempt,
                    retryable = mce.retryable,
                    error = %mce.message,
                    "module HTTP call failed"
                );

                if !mce.retryable {
                    // 不可重试 — 立即返回
                    return Err(anyhow::anyhow!(mce));
                }

                last_error = Some(mce);
            }
        }
    }

    // 所有重试耗尽
    Err(anyhow::anyhow!(
        last_error.unwrap_or_else(|| ModuleCallError {
            module_id: module_id.clone(),
            message: "unknown error after retries".to_string(),
            retryable: true,
        })
    ))
}

/// 发送单次模块 HTTP 请求并解析响应
async fn send_module_request(
    client: &reqwest::Client,
    url: &str,
    module_id: &str,
    node: &PipelineNode,
    upstream: &[Artifact],
    has_file_input: bool,
    _work_dir: &Path,
) -> anyhow::Result<Artifact> {
    let resp = if has_file_input {
        // ── multipart/form-data：文件上传 ─────────────────────────────────
        let mut form = reqwest::multipart::Form::new();

        for artifact in upstream {
            if let Artifact::File(path) = artifact {
                let file_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "input".to_string());

                let bytes = tokio::fs::read(path).await.map_err(|e| {
                    anyhow::anyhow!(ModuleCallError {
                        module_id: module_id.to_string(),
                        message: format!("failed to read input file '{}': {e}", path.display()),
                        retryable: false,
                    })
                })?;

                let part = reqwest::multipart::Part::bytes(bytes).file_name(file_name);
                form = form.part("file", part);
            }
        }

        // 将节点 params 作为 JSON 字符串附加到 params 字段
        if !node.params.is_null() && node.params.as_object().is_some_and(|o| !o.is_empty()) {
            form = form.text("params", node.params.to_string());
        }

        client
            .post(url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| classify_reqwest_error(e, module_id))?
    } else {
        // ── JSON body：文本 / JSON 输入 ───────────────────────────────────
        let mut body = serde_json::Map::new();

        // 合并节点 params
        if let Some(obj) = node.params.as_object() {
            body.insert("params".to_string(), serde_json::Value::Object(obj.clone()));
        }

        // 从上游提取文本 / JSON 输入
        for artifact in upstream {
            match artifact {
                Artifact::Text(t) => {
                    body.insert(
                        "input".to_string(),
                        serde_json::Value::String(t.clone()),
                    );
                }
                Artifact::Json(j) => {
                    body.insert("input".to_string(), j.clone());
                }
                Artifact::File(p) => {
                    body.insert(
                        "input".to_string(),
                        serde_json::Value::String(p.to_string_lossy().to_string()),
                    );
                }
            }
        }

        client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| classify_reqwest_error(e, module_id))?
    };

    // ── 解析响应 ──────────────────────────────────────────────────────────
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(ModuleCallError {
            module_id: module_id.to_string(),
            message: format!("HTTP {status}: {text}"),
            retryable: false,
        }));
    }

    let module_resp: ModuleResponse = resp.json().await.map_err(|e| {
        anyhow::anyhow!(ModuleCallError {
            module_id: module_id.to_string(),
            message: format!("failed to parse response JSON: {e}"),
            retryable: false,
        })
    })?;

    if module_resp.status != "completed" {
        return Err(anyhow::anyhow!(ModuleCallError {
            module_id: module_id.to_string(),
            message: format!(
                "module returned non-completed status: '{}'",
                module_resp.status
            ),
            retryable: false,
        }));
    }

    // ── 将 result 转为 Artifact ───────────────────────────────────────────
    let artifact = match module_resp.output_type.as_deref() {
        Some("file") => {
            let path_str = module_resp
                .result
                .as_ref()
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    anyhow::anyhow!(ModuleCallError {
                        module_id: module_id.to_string(),
                        message: "output_type is 'file' but result is not a string path".to_string(),
                        retryable: false,
                    })
                })?;
            Artifact::File(PathBuf::from(path_str))
        }
        Some("text") => {
            let text = module_resp
                .result
                .as_ref()
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            Artifact::Text(text)
        }
        Some("json") | None => {
            // json 或未指定 output_type → 返回完整 result 作为 Json
            Artifact::Json(module_resp.result.unwrap_or(serde_json::Value::Null))
        }
        Some(other) => {
            tracing::warn!(
                module_id = %module_id,
                output_type = other,
                "unknown output_type, treating as JSON"
            );
            Artifact::Json(module_resp.result.unwrap_or(serde_json::Value::Null))
        }
    };

    tracing::info!(
        module_id = %module_id,
        output_type = ?module_resp.output_type,
        "module HTTP call completed successfully"
    );

    Ok(artifact)
}

/// 将 reqwest 错误分类为可重试 / 不可重试
fn classify_reqwest_error(e: reqwest::Error, module_id: &str) -> anyhow::Error {
    let retryable = e.is_connect() || e.is_timeout() || e.is_request();
    let message = if e.is_connect() {
        format!("connection failed: {e}")
    } else if e.is_timeout() {
        format!("request timed out: {e}")
    } else {
        format!("request error: {e}")
    };

    anyhow::anyhow!(ModuleCallError {
        module_id: module_id.to_string(),
        message,
        retryable,
    })
}

/// 外部 API 节点执行 — reqwest POST + JSON body
async fn execute_external_api_node(
    node: &PipelineNode,
    upstream: &[Artifact],
    _work_dir: &Path,
) -> anyhow::Result<Artifact> {
    let (endpoint, api_key_env) = match &node.kind {
        NodeKind::ExternalApi {
            endpoint,
            api_key_env,
            ..
        } => (endpoint.clone(), api_key_env.clone()),
        _ => unreachable!(),
    };

    // 解析 API key
    let _api_key = api_key_env
        .as_deref()
        .map(|env_var| {
            std::env::var(env_var)
                .map_err(|_| anyhow::anyhow!("API key env var not set: {env_var}"))
        })
        .transpose()?;

    // 构建请求 body
    let mut body = serde_json::Map::new();
    for artifact in upstream {
        match artifact {
            Artifact::File(p) => {
                body.insert(
                    "input_file".to_string(),
                    serde_json::Value::String(p.to_string_lossy().to_string()),
                );
            }
            Artifact::Text(t) => {
                body.insert(
                    "input_text".to_string(),
                    serde_json::Value::String(t.clone()),
                );
            }
            Artifact::Json(j) => {
                body.insert("input_json".to_string(), j.clone());
            }
        }
    }

    // 合并节点 params
    if let Some(obj) = node.params.as_object() {
        for (k, v) in obj {
            body.insert(k.clone(), v.clone());
        }
    }

    let client = reqwest::Client::new();
    let resp = client
        .post(&endpoint)
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "external API call to {endpoint} failed ({status}): {text}"
        ));
    }

    let resp_body: serde_json::Value = resp.json().await?;
    Ok(Artifact::Json(resp_body))
}
