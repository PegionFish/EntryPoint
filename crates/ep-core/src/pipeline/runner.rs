//! 管线执行器（高层接口） — Wave 1b Agent C 实现
//!
//! 基于 PipelineTask 状态机，实际执行各类型节点。
//! 支持进度回调、分层拓扑执行、失败传播。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use crate::pipeline::dag::Pipeline;
use crate::pipeline::executor::{execute_node, ModuleCallError, NodeState, PipelineTask};
use crate::types::{Artifact, PipelineRunner, TaskStatus};

// ─── 任务摘要与详情（GUI 列表展示用） ─────────────────────────────────────────

/// 管线任务摘要（用于列表展示）
#[derive(Debug, Clone, Serialize)]
pub struct TaskSummary {
    /// 任务唯一 ID
    pub id: String,
    /// 关联的管线名称/ID
    pub pipeline_name: String,
    /// 任务整体状态
    pub status: TaskStatus,
    /// 任务启动时间（ISO 8601）
    pub started_at: Option<String>,
    /// 任务完成时间（ISO 8601）
    pub finished_at: Option<String>,
    /// 节点总数
    pub node_count: usize,
    /// 已完成节点数
    pub completed_nodes: usize,
    /// 产物列表（node_id, 路径）；门禁 #40 透传（桌面任务页消费）
    #[serde(default)]
    pub artifacts: Vec<(String, PathBuf)>,
}

/// 单个节点的详细状态（用于任务详情展示）
#[derive(Debug, Clone, Serialize)]
pub struct NodeDetail {
    /// 节点 ID
    pub node_id: String,
    /// 节点状态描述
    pub state: String,
    /// 失败时的错误信息
    pub error: Option<String>,
}

/// 管线任务详情（含各节点状态）
#[derive(Debug, Clone, Serialize)]
pub struct TaskDetail {
    /// 任务唯一 ID
    pub id: String,
    /// 关联的管线名称/ID
    pub pipeline_name: String,
    /// 任务整体状态
    pub status: TaskStatus,
    /// 任务启动时间（ISO 8601）
    pub started_at: Option<String>,
    /// 任务完成时间（ISO 8601）
    pub finished_at: Option<String>,
    /// 各节点的详细状态
    pub nodes: Vec<NodeDetail>,
}

/// 管线运行器
pub struct PipelineRunnerImpl {
    task: Option<PipelineTask>,
    /// 历史任务存储：task_id → PipelineTask
    tasks: HashMap<String, PipelineTask>,
    #[allow(dead_code)]
    work_dir: PathBuf,
    /// 模块端口注册表：module_id → port
    module_ports: HashMap<String, u16>,
    /// 节点开始执行时回调
    #[allow(clippy::type_complexity)]
    pub on_node_start: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// 节点完成时回调
    #[allow(clippy::type_complexity)]
    pub on_node_complete: Option<Arc<dyn Fn(&str, &Artifact) + Send + Sync>>,
    /// 节点失败时回调
    #[allow(clippy::type_complexity)]
    pub on_node_error: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
    /// 协作取消标志（P0-6/B3；缺陷 #5 起支持在飞中断）：置位后两类检查点
    /// 均生效——① 节点边界：下一个节点不再启动；② 节点在飞期间：取消
    /// 监视器赢得竞争后 `abort()` 执行任务（在飞的模块 HTTP 请求 future
    /// 被丢弃、连接立即断开；ffmpeg 子进程经 kill_on_drop 一并终止），
    /// 任务终结为 `Cancelled`。轮询粒度 = [`CANCEL_POLL_INTERVAL_MS`]。
    cancel_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// 默认节点 wall-clock 超时（P0-6/B3）：节点未声明 `timeout_secs` 时使用；
    /// None = 无默认（仅受节点自身 timeout_secs 与执行器客户端级超时约束）。
    /// 与执行器的 HTTP 客户端超时互补（B7：executor `node_timeout_secs`）：
    /// 此处包裹整个 `execute_node` future，覆盖 ffmpeg 子进程等非 HTTP 节点；
    /// 超时触发时同样 `abort()` 执行任务（缺陷 #5：不只是标记失败任请求
    /// 挂到自然结束，在飞 HTTP 连接立即断开、子进程立即终止）。
    default_node_timeout: Option<std::time::Duration>,
}

/// 取消标志轮询间隔（毫秒）：在飞节点中断的检测粒度。
const CANCEL_POLL_INTERVAL_MS: u64 = 100;

/// 监视协作取消标志：置位即返回（缺陷 #5）。与在飞节点执行竞速，
/// 赢得竞争后由调用方 `abort()` 执行任务，使平台侧不再等待/重试该请求。
async fn wait_for_cancel_flag(flag: Arc<std::sync::atomic::AtomicBool>) {
    loop {
        if flag.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(CANCEL_POLL_INTERVAL_MS)).await;
    }
}

/// 在飞节点的中断原因（缺陷 #5：硬超时 / 协作取消均走 abort 路径）
enum NodeInterrupt {
    /// 节点级 wall-clock 硬超时触发
    Timeout(u64),
    /// 协作取消标志置位
    Cancelled,
}

/// 单节点执行竞速结果：自然完成 / 硬超时中断 / 取消中断
enum NodeRunOutcome {
    Done(anyhow::Result<Artifact>),
    TimedOut(u64),
    Cancelled,
}

impl PipelineRunnerImpl {
    pub fn new(work_dir: PathBuf) -> Self {
        Self {
            task: None,
            tasks: HashMap::new(),
            work_dir,
            module_ports: HashMap::new(),
            on_node_start: None,
            on_node_complete: None,
            on_node_error: None,
            cancel_flag: None,
            default_node_timeout: None,
        }
    }

    /// 注册模块服务端口
    ///
    /// 管线执行时，模块节点将通过 `http://127.0.0.1:{port}/predict/{capability}` 调用。
    pub fn set_module_port(&mut self, module_id: impl Into<String>, port: u16) {
        self.module_ports.insert(module_id.into(), port);
    }

    /// 批量注册模块端口
    pub fn set_module_ports(&mut self, ports: HashMap<String, u16>) {
        self.module_ports.extend(ports);
    }

    /// 设置协作取消标志（执行层与运行器共享同一 `AtomicBool`）
    pub fn set_cancel_flag(&mut self, flag: Arc<std::sync::atomic::AtomicBool>) {
        self.cancel_flag = Some(flag);
    }

    /// 设置默认节点 wall-clock 超时（节点自身 `timeout_secs` 优先）
    pub fn set_default_node_timeout(&mut self, timeout: Option<std::time::Duration>) {
        self.default_node_timeout = timeout;
    }

    // ─── 任务列表 API（GUI 展示用） ─────────────────────────────────────

    /// 列出所有已知任务的摘要信息
    pub fn list_tasks(&self) -> Vec<TaskSummary> {
        self.tasks
            .values()
            .map(|task| {
                let node_count = task.node_states.len();
                let completed_nodes = task
                    .node_states
                    .values()
                    .filter(|s| matches!(s, NodeState::Completed { .. }))
                    .count();

                TaskSummary {
                    id: task.id.clone(),
                    pipeline_name: task.pipeline_id.clone(),
                    status: task.status.clone(),
                    started_at: Some(task.started_at.to_rfc3339()),
                    finished_at: task.finished_at.map(|t| t.to_rfc3339()),
                    node_count,
                    completed_nodes,
                    artifacts: Vec::new(),
                }
            })
            .collect()
    }

    /// 获取单个任务的详细节点状态
    pub fn get_task_detail(&self, task_id: &str) -> Option<TaskDetail> {
        let task = self.tasks.get(task_id)?;

        let nodes = task
            .node_states
            .iter()
            .map(|(node_id, state)| {
                let (state_str, error) = match state {
                    NodeState::Pending => ("pending".to_string(), None),
                    NodeState::Running => ("running".to_string(), None),
                    NodeState::Completed { .. } => ("completed".to_string(), None),
                    NodeState::Failed { error, .. } => {
                        ("failed".to_string(), Some(error.clone()))
                    }
                    NodeState::Skipped => ("skipped".to_string(), None),
                };
                NodeDetail {
                    node_id: node_id.clone(),
                    state: state_str,
                    error,
                }
            })
            .collect();

        Some(TaskDetail {
            id: task.id.clone(),
            pipeline_name: task.pipeline_id.clone(),
            status: task.status.clone(),
            started_at: Some(task.started_at.to_rfc3339()),
            finished_at: task.finished_at.map(|t| t.to_rfc3339()),
            nodes,
        })
    }

    /// 异步执行管线（内部实现）
    async fn execute_async(
        &mut self,
        pipeline: &Pipeline,
        work_dir: &Path,
    ) -> anyhow::Result<()> {
        let mut task = PipelineTask::new(pipeline, work_dir.to_path_buf());
        let layers = pipeline
            .topological_layers()
            .map_err(|_| anyhow::anyhow!("pipeline contains a cycle"))?;

        for layer in &layers {
            // 检查该层是否有节点因上游失败而处于非 Pending 状态
            let all_skip = layer.iter().all(|id| {
                !matches!(task.node_state(id), Some(NodeState::Pending))
            });
            if all_skip {
                continue;
            }

            task.execute_layer(layer);

            for node_id in layer {
                // 跳过非 Running 状态的节点（可能已被标记为 Skipped）
                if !matches!(task.node_state(node_id), Some(NodeState::Running)) {
                    continue;
                }

                // 取消检查点（P0-6/B3）：节点边界检查协作取消标志，
                // 置位 → 任务终结为 Cancelled（下一节点不再启动；若标志在
                // 节点在飞期间置位，由下方取消监视器中断在飞执行 — 缺陷 #5）
                if self
                    .cancel_flag
                    .as_ref()
                    .is_some_and(|f| f.load(std::sync::atomic::Ordering::SeqCst))
                {
                    let err_msg = "task cancelled".to_string();
                    if let Some(ref cb) = self.on_node_error {
                        cb(node_id, &err_msg);
                    }
                    task.mark_failed_with_pipeline(node_id, err_msg, false, pipeline);
                    task.status = TaskStatus::Cancelled;
                    self.tasks.insert(task.id.clone(), task.clone());
                    self.task = Some(task);
                    return Err(anyhow::anyhow!("pipeline execution cancelled"));
                }

                // 回调：节点开始
                if let Some(ref cb) = self.on_node_start {
                    cb(node_id);
                }

                let node = pipeline
                    .nodes
                    .iter()
                    .find(|n| &n.id == node_id)
                    .ok_or_else(|| anyhow::anyhow!("node '{node_id}' not found in pipeline"))?;

                // Wall-clock 超时包裹（P0-6/B3）：节点 `timeout_secs` 优先，
                // 缺省用 `default_node_timeout`；覆盖 ffmpeg 子进程等非 HTTP
                // 节点（HTTP 节点另有执行器客户端级超时，两者互补取先到者）。
                let timeout_secs = node
                    .timeout_secs
                    .map(u64::from)
                    .or_else(|| self.default_node_timeout.map(|d| d.as_secs()))
                    .filter(|&secs| secs > 0);

                // ── 可中断执行（缺陷 #5）────────────────────────────────────
                // execute_node 派生为独立 tokio 任务，与「硬超时 / 取消标志」
                // 竞速：超时或取消赢得竞争时 abort() 执行任务——在飞的模块
                // HTTP 请求（/predict/*）future 被丢弃、连接立即断开（模块侧
                // 尽快收到断开）；ffmpeg 子进程经 kill_on_drop 一并终止。
                // 不再「标记失败而任请求挂到自然结束」。
                let node_owned = node.clone();
                let pipeline_owned = pipeline.clone();
                let task_owned = task.clone();
                let work_dir_owned = work_dir.to_path_buf();
                let ports_owned = self.module_ports.clone();
                let mut exec_handle = tokio::spawn(async move {
                    execute_node(
                        &node_owned,
                        &pipeline_owned,
                        &task_owned,
                        &work_dir_owned,
                        &ports_owned,
                    )
                    .await
                });

                // 中断竞速：硬超时与取消标志任一先到即中断在飞节点；
                // 两者皆未配置时为永不就绪分支，等价于只等执行完成。
                let timeout_owned = timeout_secs;
                let flag_owned = self.cancel_flag.clone();
                let interrupt = async move {
                    let timeout_branch = async move {
                        match timeout_owned {
                            Some(secs) => {
                                tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                                NodeInterrupt::Timeout(secs)
                            }
                            None => std::future::pending::<NodeInterrupt>().await,
                        }
                    };
                    let cancel_branch = async move {
                        match flag_owned {
                            Some(flag) => {
                                wait_for_cancel_flag(flag).await;
                                NodeInterrupt::Cancelled
                            }
                            None => std::future::pending::<NodeInterrupt>().await,
                        }
                    };
                    tokio::select! {
                        i = timeout_branch => i,
                        i = cancel_branch => i,
                    }
                };

                let outcome = tokio::select! {
                    res = &mut exec_handle => {
                        // 任务自然完成（或 join 异常）→ 走既有成败处理
                        NodeRunOutcome::Done(match res {
                            Ok(inner) => inner,
                            Err(join_err) => Err(anyhow::anyhow!(
                                "node '{node_id}' execution task aborted: {join_err}"
                            )),
                        })
                    }
                    i = interrupt => {
                        exec_handle.abort();
                        match i {
                            NodeInterrupt::Timeout(secs) => NodeRunOutcome::TimedOut(secs),
                            NodeInterrupt::Cancelled => NodeRunOutcome::Cancelled,
                        }
                    }
                };

                let exec_result = match outcome {
                    NodeRunOutcome::Done(result) => result,
                    NodeRunOutcome::TimedOut(secs) => Err(anyhow::anyhow!(
                        "node '{node_id}' timed out after {secs}s (in-flight call aborted)"
                    )),
                    NodeRunOutcome::Cancelled => {
                        // 与节点边界取消同语义：节点判 failed、任务终结
                        // Cancelled。行为边界：模块侧推理线程为同步 CPU/GPU
                        // 密集执行，客户端断开后可能仍在收尾，worker 短暂
                        // 占用属预期；平台侧不再等待/重试该请求。
                        tracing::info!(
                            node_id,
                            "task cancelled: in-flight node aborted (module HTTP connection closed); \
                             module-side inference may still finish its current request \
                             (brief worker occupation is expected)"
                        );
                        let err_msg = "task cancelled (in-flight node aborted)".to_string();
                        if let Some(ref cb) = self.on_node_error {
                            cb(node_id, &err_msg);
                        }
                        task.mark_failed_with_pipeline(node_id, err_msg, false, pipeline);
                        task.status = TaskStatus::Cancelled;
                        self.tasks.insert(task.id.clone(), task.clone());
                        self.task = Some(task);
                        return Err(anyhow::anyhow!("pipeline execution cancelled"));
                    }
                };

                match exec_result {
                    Ok(artifact) => {
                        // 回调：节点完成
                        if let Some(ref cb) = self.on_node_complete {
                            cb(node_id, &artifact);
                        }
                        task.mark_completed(node_id, artifact);
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        // 从 ModuleCallError 提取可重试标志（B7 契约：保留 downcast）
                        let retryable = e
                            .downcast_ref::<ModuleCallError>()
                            .map(|mce| mce.retryable)
                            .unwrap_or(false);
                        // 回调：节点错误
                        if let Some(ref cb) = self.on_node_error {
                            cb(node_id, &err_msg);
                        }
                        task.mark_failed_with_pipeline(node_id, err_msg, retryable, pipeline);
                        break; // 停止当前层的执行
                    }
                }
            }
        }

        // 检查任务最终状态
        if let TaskStatus::Failed(ref e) = task.status {
            let err = anyhow::anyhow!("pipeline execution failed: {e}");
            self.tasks.insert(task.id.clone(), task.clone());
            self.task = Some(task);
            return Err(err);
        }

        self.tasks.insert(task.id.clone(), task.clone());
        self.task = Some(task);
        Ok(())
    }
}

impl PipelineRunner for PipelineRunnerImpl {
    fn execute(
        &mut self,
        pipeline: &Pipeline,
        work_dir: &Path,
    ) -> anyhow::Result<()> {
        // 桥接 sync trait → async 实现
        match tokio::runtime::Handle::try_current() {
            Ok(_handle) => {
                // 已在 tokio 运行时中 — 在新线程上 block_on 以避免嵌套 panic
                let work_dir_path = work_dir.to_path_buf();
                let pipeline_clone = pipeline.clone();

                // 需要把 callbacks 临时取出以避免 self 借用冲突
                let on_start = self.on_node_start.take();
                let on_complete = self.on_node_complete.take();
                let on_error = self.on_node_error.take();
                let module_ports = self.module_ports.clone();
                let cancel_flag = self.cancel_flag.clone();
                let default_node_timeout = self.default_node_timeout;

                let mut temp_runner = PipelineRunnerImpl {
                    task: None,
                    tasks: HashMap::new(),
                    work_dir: work_dir_path.clone(),
                    module_ports,
                    on_node_start: on_start,
                    on_node_complete: on_complete,
                    on_node_error: on_error,
                    cancel_flag,
                    default_node_timeout,
                };

                let (result, returned_runner) = std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    let res = rt.block_on(temp_runner.execute_async(&pipeline_clone, &work_dir_path));
                    (res, temp_runner)
                })
                .join()
                .map_err(|_| anyhow::anyhow!("tokio spawn thread panicked"))?;

                // 恢复状态
                self.task = returned_runner.task;
                self.tasks.extend(returned_runner.tasks);
                self.module_ports = returned_runner.module_ports;
                self.on_node_start = returned_runner.on_node_start;
                self.on_node_complete = returned_runner.on_node_complete;
                self.on_node_error = returned_runner.on_node_error;
                self.cancel_flag = returned_runner.cancel_flag;
                self.default_node_timeout = returned_runner.default_node_timeout;
                result
            }
            Err(_) => {
                // 不在 tokio 运行时中 — 创建新的
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(self.execute_async(pipeline, work_dir))
            }
        }
    }

    fn task_status(&self) -> &TaskStatus {
        static IDLE: TaskStatus = TaskStatus::Pending;
        match &self.task {
            Some(t) => &t.status,
            None => &IDLE,
        }
    }

    fn node_status(&self, node_id: &str) -> Option<&NodeState> {
        self.task.as_ref()?.node_state(node_id)
    }
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::dag::Pipeline;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 检查 ffmpeg 是否可用（优先使用 portable 版本）
    fn ffmpeg_available() -> bool {
        let ffmpeg = crate::pipeline::executor::resolve_ffmpeg_path();
        std::process::Command::new(&ffmpeg)
            .arg("-version")
            .output()
            .is_ok()
    }

    /// 创建临时工作目录
    fn temp_work_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ep_test_{label}_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 清理临时目录
    fn cleanup_dir(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    // ─── Test 1: FileInput → FileOutput ──────────────────────────────────────

    #[tokio::test]
    async fn test_file_input_to_file_output() {
        let work_dir = temp_work_dir("fio");
        let input_file = work_dir.join("source.txt");
        let output_file = work_dir.join("output.txt");

        // 创建输入文件
        std::fs::write(&input_file, "hello pipeline").unwrap();

        let toml_str = format!(
            r#"
[pipeline]
id = "test-fio"
name = "FileInput to FileOutput"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["output", "input"]
"#,
            input_file.to_string_lossy().replace('\\', "/"),
            output_file.to_string_lossy().replace('\\', "/"),
        );

        let pipeline = Pipeline::from_toml_str(&toml_str).unwrap();
        let mut runner = PipelineRunnerImpl::new(work_dir.clone());

        let result = runner.execute_async(&pipeline, &work_dir).await;
        assert!(result.is_ok(), "execution failed: {result:?}");

        // 验证输出文件存在且内容正确
        assert!(output_file.exists(), "output file should exist");
        let content = std::fs::read_to_string(&output_file).unwrap();
        assert_eq!(content, "hello pipeline");

        // 验证状态
        assert_eq!(*runner.task_status(), TaskStatus::Completed);
        assert!(matches!(
            runner.node_status("input"),
            Some(NodeState::Completed { .. })
        ));
        assert!(matches!(
            runner.node_status("output"),
            Some(NodeState::Completed { .. })
        ));

        cleanup_dir(&work_dir);
    }

    // ─── Test 2: FFmpeg node (skip if ffmpeg unavailable) ────────────────────

    #[tokio::test]
    async fn test_ffmpeg_node() {
        if !ffmpeg_available() {
            eprintln!("SKIP: ffmpeg not available");
            return;
        }

        let work_dir = temp_work_dir("ffmpeg");
        let output_file = work_dir.join("ffmpeg_out.raw");

        let toml_str = format!(
            r#"
[pipeline]
id = "test-ffmpeg"
name = "FFmpeg Test"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "encode"
kind = "builtin"
builtin = "ffmpeg"
params = {{ args = ["-f", "lavfi", "-i", "testsrc=duration=1:size=32x32:rate=1", "-frames:v", "1", "-f", "rawvideo", "-y"], output = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["encode", "input"]
"#,
            // dummy input (ffmpeg uses lavfi, ignores -i)
            work_dir.join("dummy.txt").to_string_lossy().replace('\\', "/"),
            output_file.to_string_lossy().replace('\\', "/"),
        );

        // Create a dummy input file so file_input doesn't fail
        std::fs::write(work_dir.join("dummy.txt"), "dummy").unwrap();

        let pipeline = Pipeline::from_toml_str(&toml_str).unwrap();
        let mut runner = PipelineRunnerImpl::new(work_dir.clone());

        let result = runner.execute_async(&pipeline, &work_dir).await;
        assert!(result.is_ok(), "ffmpeg execution failed: {result:?}");
        assert!(output_file.exists(), "ffmpeg output should exist");

        cleanup_dir(&work_dir);
    }

    // ─── Test 3: Linear pipeline (3 nodes) ──────────────────────────────────

    #[tokio::test]
    async fn test_pipeline_execution_linear() {
        let work_dir = temp_work_dir("linear");
        let input_file = work_dir.join("source.txt");
        let final_output = work_dir.join("final.txt");

        std::fs::write(&input_file, "linear test data").unwrap();

        // 使用 ffmpeg 的 null muxer 来"处理"文件（只复制数据）
        // 但为了简化，我们使用 file_input → file_output → file_output 的线性管线
        // 不对，file_output 需要上游有文件。让我用 file_input → ffmpeg → file_output
        // 但 ffmpeg 可能不可用...
        // 改用纯 file 节点：file_input → file_output (mid) → file_output (final)
        // 但 file_output(mid) 的 path 作为 file_output(final) 的输入
        // 这需要 mid 的输出文件作为 final 的输入

        let mid_output = work_dir.join("mid.txt");

        let toml_str = format!(
            r#"
[pipeline]
id = "test-linear"
name = "Linear Pipeline"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "mid"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[nodes]]
id = "final"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["mid", "input"]

[[edges]]
from = ["mid", "output"]
to = ["final", "input"]
"#,
            input_file.to_string_lossy().replace('\\', "/"),
            mid_output.to_string_lossy().replace('\\', "/"),
            final_output.to_string_lossy().replace('\\', "/"),
        );

        let pipeline = Pipeline::from_toml_str(&toml_str).unwrap();
        let mut runner = PipelineRunnerImpl::new(work_dir.clone());

        let result = runner.execute_async(&pipeline, &work_dir).await;
        assert!(result.is_ok(), "linear pipeline failed: {result:?}");

        // 验证所有节点完成
        assert_eq!(*runner.task_status(), TaskStatus::Completed);
        assert!(matches!(
            runner.node_status("input"),
            Some(NodeState::Completed { .. })
        ));
        assert!(matches!(
            runner.node_status("mid"),
            Some(NodeState::Completed { .. })
        ));
        assert!(matches!(
            runner.node_status("final"),
            Some(NodeState::Completed { .. })
        ));

        // 验证最终输出
        assert!(final_output.exists());
        let content = std::fs::read_to_string(&final_output).unwrap();
        assert_eq!(content, "linear test data");

        cleanup_dir(&work_dir);
    }

    // ─── Test 4: Node failure skips downstream ──────────────────────────────

    #[tokio::test]
    async fn test_node_failure_skips_downstream() {
        let work_dir = temp_work_dir("fail");
        let input_file = work_dir.join("source.txt");
        std::fs::write(&input_file, "test").unwrap();

        // file_input (ok) → module (always fails) → file_output (should be skipped)
        let toml_str = format!(
            r#"
[pipeline]
id = "test-fail"
name = "Failure Test"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "process"
kind = "module"
module_id = "nonexistent"
capability = "do_thing"

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["process", "input"]

[[edges]]
from = ["process", "output"]
to = ["output", "input"]
"#,
            input_file.to_string_lossy().replace('\\', "/"),
            work_dir.join("out.txt").to_string_lossy().replace('\\', "/"),
        );

        let pipeline = Pipeline::from_toml_str(&toml_str).unwrap();
        let mut runner = PipelineRunnerImpl::new(work_dir.clone());

        let result = runner.execute_async(&pipeline, &work_dir).await;
        assert!(result.is_err(), "should fail because module node fails");

        // 验证状态
        assert!(matches!(runner.task_status(), TaskStatus::Failed(_)));
        assert!(matches!(
            runner.node_status("input"),
            Some(NodeState::Completed { .. })
        ));
        assert!(matches!(
            runner.node_status("process"),
            Some(NodeState::Failed { .. })
        ));
        assert_eq!(runner.node_status("output"), Some(&NodeState::Skipped));

        cleanup_dir(&work_dir);
    }

    // ─── Test 5: Progress callbacks ──────────────────────────────────────────

    #[tokio::test]
    async fn test_progress_callbacks() {
        let work_dir = temp_work_dir("cb");
        let input_file = work_dir.join("source.txt");
        let output_file = work_dir.join("output.txt");
        std::fs::write(&input_file, "callback test").unwrap();

        let toml_str = format!(
            r#"
[pipeline]
id = "test-cb"
name = "Callback Test"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["output", "input"]
"#,
            input_file.to_string_lossy().replace('\\', "/"),
            output_file.to_string_lossy().replace('\\', "/"),
        );

        let pipeline = Pipeline::from_toml_str(&toml_str).unwrap();

        let start_count = Arc::new(AtomicUsize::new(0));
        let complete_count = Arc::new(AtomicUsize::new(0));
        let error_count = Arc::new(AtomicUsize::new(0));

        let start_clone = start_count.clone();
        let complete_clone = complete_count.clone();
        let error_clone = error_count.clone();

        let mut runner = PipelineRunnerImpl::new(work_dir.clone());
        runner.on_node_start = Some(Arc::new(move |_id| {
            start_clone.fetch_add(1, Ordering::SeqCst);
        }));
        runner.on_node_complete = Some(Arc::new(move |_id, _artifact| {
            complete_clone.fetch_add(1, Ordering::SeqCst);
        }));
        runner.on_node_error = Some(Arc::new(move |_id, _err| {
            error_clone.fetch_add(1, Ordering::SeqCst);
        }));

        let result = runner.execute_async(&pipeline, &work_dir).await;
        assert!(result.is_ok());

        // 2 nodes → 2 starts, 2 completes, 0 errors
        assert_eq!(start_count.load(Ordering::SeqCst), 2);
        assert_eq!(complete_count.load(Ordering::SeqCst), 2);
        assert_eq!(error_count.load(Ordering::SeqCst), 0);

        cleanup_dir(&work_dir);
    }

    // ─── Test 6: list_tasks / get_task_detail ────────────────────────────────

    #[tokio::test]
    async fn test_list_tasks_and_detail() {
        let work_dir = temp_work_dir("tasklist");
        let input_file = work_dir.join("source.txt");
        let output_file = work_dir.join("output.txt");
        std::fs::write(&input_file, "task list test").unwrap();

        let toml_str = format!(
            r#"
[pipeline]
id = "test-tasklist"
name = "Task List Test"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["output", "input"]
"#,
            input_file.to_string_lossy().replace('\\', "/"),
            output_file.to_string_lossy().replace('\\', "/"),
        );

        let pipeline = Pipeline::from_toml_str(&toml_str).unwrap();
        let mut runner = PipelineRunnerImpl::new(work_dir.clone());

        // 执行前无任务
        assert!(runner.list_tasks().is_empty());

        let result = runner.execute_async(&pipeline, &work_dir).await;
        assert!(result.is_ok());

        // 执行后有一个任务
        let tasks = runner.list_tasks();
        assert_eq!(tasks.len(), 1);

        let summary = &tasks[0];
        assert_eq!(summary.pipeline_name, "test-tasklist");
        assert_eq!(summary.status, TaskStatus::Completed);
        assert_eq!(summary.node_count, 2);
        assert_eq!(summary.completed_nodes, 2);
        assert!(summary.started_at.is_some());
        assert!(summary.finished_at.is_some());

        // 获取详情
        let detail = runner.get_task_detail(&summary.id).unwrap();
        assert_eq!(detail.pipeline_name, "test-tasklist");
        assert_eq!(detail.nodes.len(), 2);
        for node in &detail.nodes {
            assert_eq!(node.state, "completed");
            assert!(node.error.is_none());
        }

        // 不存在的任务返回 None
        assert!(runner.get_task_detail("nonexistent").is_none());

        cleanup_dir(&work_dir);
    }

    #[tokio::test]
    async fn test_list_tasks_after_failure() {
        let work_dir = temp_work_dir("taskfail");
        let input_file = work_dir.join("source.txt");
        std::fs::write(&input_file, "test").unwrap();

        let toml_str = format!(
            r#"
[pipeline]
id = "test-taskfail"
name = "Task Fail Test"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "bad"
kind = "module"
module_id = "nonexistent"
capability = "do_thing"

[[edges]]
from = ["input", "output"]
to = ["bad", "input"]
"#,
            input_file.to_string_lossy().replace('\\', "/"),
        );

        let pipeline = Pipeline::from_toml_str(&toml_str).unwrap();
        let mut runner = PipelineRunnerImpl::new(work_dir.clone());

        let result = runner.execute_async(&pipeline, &work_dir).await;
        assert!(result.is_err());

        // 失败的任务也应被记录
        let tasks = runner.list_tasks();
        assert_eq!(tasks.len(), 1);
        assert!(matches!(tasks[0].status, TaskStatus::Failed(_)));

        let detail = runner.get_task_detail(&tasks[0].id).unwrap();
        let bad_node = detail.nodes.iter().find(|n| n.node_id == "bad").unwrap();
        assert_eq!(bad_node.state, "failed");
        assert!(bad_node.error.is_some());

        cleanup_dir(&work_dir);
    }

    #[test]
    fn test_task_summary_serialization() {
        let summary = TaskSummary {
            id: "abc-123".to_string(),
            pipeline_name: "test-pipe".to_string(),
            status: TaskStatus::Completed,
            started_at: Some("2026-07-30T10:00:00Z".to_string()),
            finished_at: Some("2026-07-30T10:01:00Z".to_string()),
            node_count: 3,
            completed_nodes: 3,
            artifacts: Vec::new(),
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"pipeline_name\":\"test-pipe\""));
        assert!(json.contains("\"node_count\":3"));
    }

    // ─── B3（P0-6）：协作取消 — 节点边界检查点 ──────────────────────────────

    /// 取消标志预置位 → 首个节点边界即终结为 Cancelled
    #[tokio::test]
    async fn test_cancel_flag_terminates_task_as_cancelled() {
        let work_dir = temp_work_dir("cancel");
        let input_file = work_dir.join("source.txt");
        let output_file = work_dir.join("out.txt");
        std::fs::write(&input_file, "cancel me").unwrap();

        let toml_str = format!(
            r#"
[pipeline]
id = "test-cancel"
name = "Cancel Test"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["output", "input"]
"#,
            input_file.to_string_lossy().replace('\\', "/"),
            output_file.to_string_lossy().replace('\\', "/"),
        );
        let pipeline = Pipeline::from_toml_str(&toml_str).unwrap();
        let mut runner = PipelineRunnerImpl::new(work_dir.clone());

        let flag = Arc::new(std::sync::atomic::AtomicBool::new(true));
        runner.set_cancel_flag(flag);

        let result = runner.execute_async(&pipeline, &work_dir).await;
        let err = result.expect_err("预置取消标志应使执行失败").to_string();
        assert!(err.contains("cancelled"), "got: {err}");

        // 引擎任务状态为 Cancelled（TaskStatus::Cancelled 产生路径）
        assert_eq!(*runner.task_status(), TaskStatus::Cancelled);
        let tasks = runner.list_tasks();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, TaskStatus::Cancelled);
        // 未产生输出（首节点未执行）
        assert!(!output_file.exists());

        cleanup_dir(&work_dir);
    }

    /// 节点完成后置位取消 → 下一节点边界终结（边界检查点语义）
    #[tokio::test]
    async fn test_cancel_flag_at_boundary_after_node_complete() {
        let work_dir = temp_work_dir("cancel-mid");
        let input_file = work_dir.join("source.txt");
        let mid_file = work_dir.join("mid.txt");
        let final_file = work_dir.join("final.txt");
        std::fs::write(&input_file, "mid cancel").unwrap();

        let toml_str = format!(
            r#"
[pipeline]
id = "test-cancel-mid"
name = "Mid Cancel Test"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "mid"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[nodes]]
id = "final"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["mid", "input"]

[[edges]]
from = ["mid", "output"]
to = ["final", "input"]
"#,
            input_file.to_string_lossy().replace('\\', "/"),
            mid_file.to_string_lossy().replace('\\', "/"),
            final_file.to_string_lossy().replace('\\', "/"),
        );
        let pipeline = Pipeline::from_toml_str(&toml_str).unwrap();
        let mut runner = PipelineRunnerImpl::new(work_dir.clone());

        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        runner.set_cancel_flag(flag.clone());
        // 首节点完成时置位 → 第二节点边界触发取消（确定性：边界检查在
        // mid 启动前；在飞中断路径由下方 hang 服务器测试覆盖）
        let flag_for_cb = flag.clone();
        runner.on_node_complete = Some(Arc::new(move |_node_id, _artifact| {
            flag_for_cb.store(true, std::sync::atomic::Ordering::SeqCst);
        }));

        let result = runner.execute_async(&pipeline, &work_dir).await;
        assert!(result.is_err());
        assert_eq!(*runner.task_status(), TaskStatus::Cancelled);
        // 取消在 mid 边界触发：input（置位前完成的节点）已完成，
        // mid/final 不再执行
        assert!(!mid_file.exists(), "取消边界起的节点不应执行");
        assert!(!final_file.exists(), "取消后的节点不应执行");
        let detail = runner.get_task_detail(&runner.list_tasks()[0].id).unwrap();
        let input = detail.nodes.iter().find(|n| n.node_id == "input").unwrap();
        assert_eq!(input.state, "completed", "取消前完成的节点应保持完成");
        let mid = detail.nodes.iter().find(|n| n.node_id == "mid").unwrap();
        assert_eq!(mid.state, "failed");
        assert!(mid.error.as_deref().unwrap().contains("cancelled"));
        let final_node = detail.nodes.iter().find(|n| n.node_id == "final").unwrap();
        assert_eq!(final_node.state, "skipped");

        cleanup_dir(&work_dir);
    }

    // ─── 缺陷 #5：在飞节点可中断（abort 在飞 HTTP 请求） ────────────────

    /// 永不响应的 mock 模块端点：客户端连入时置位 `connected`，
    /// 客户端断开（EOF/RST）时置位 `disconnected`，用于断言 abort
    /// 确实关闭了在飞连接。
    struct HangServer {
        port: u16,
        connected: Arc<std::sync::atomic::AtomicBool>,
        disconnected: Arc<std::sync::atomic::AtomicBool>,
    }

    impl HangServer {
        async fn start() -> Self {
            use tokio::io::AsyncReadExt;

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind 127.0.0.1 random port");
            let port = listener.local_addr().unwrap().port();
            let connected = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let disconnected = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let conn = connected.clone();
            let disc = disconnected.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        break;
                    };
                    conn.store(true, std::sync::atomic::Ordering::SeqCst);
                    let disc = disc.clone();
                    tokio::spawn(async move {
                        let mut buf = [0u8; 4096];
                        // 只读不答：直到客户端断开（EOF/错误）
                        loop {
                            match stream.read(&mut buf).await {
                                Ok(0) | Err(_) => {
                                    disc.store(true, std::sync::atomic::Ordering::SeqCst);
                                    break;
                                }
                                Ok(_) => {}
                            }
                        }
                    });
                }
            });
            Self {
                port,
                connected,
                disconnected,
            }
        }

        /// 等待断开被观察到（超时 panic）
        async fn wait_disconnected(&self) {
            for _ in 0..50 {
                if self.disconnected.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            panic!("模块侧未观察到客户端断开：在飞请求未被 abort");
        }
    }

    /// file_input → 永不响应的模块节点 → file_output 的管线 TOML
    fn hang_module_pipeline_toml(input_path: &Path, output_path: &Path) -> String {
        format!(
            r#"
[pipeline]
id = "test-hang"
name = "Hang Module Test"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "slow"
kind = "module"
module_id = "mock-hang"
capability = "slow_cap"

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["slow", "input"]

[[edges]]
from = ["slow", "output"]
to = ["output", "input"]
"#,
            input_path.to_string_lossy().replace('\\', "/"),
            output_path.to_string_lossy().replace('\\', "/"),
        )
    }

    /// 硬超时触发 → abort 在飞模块 HTTP 请求（缺陷 #5）：
    /// 节点未声明 timeout_secs，default_node_timeout=1s 包裹在飞调用；
    /// 超时后平台侧立即终结（不等满 reqwest 客户端 300s 缺省超时），
    /// 且模块侧观察到连接断开。
    #[tokio::test]
    async fn test_node_timeout_aborts_inflight_module_request() {
        let work_dir = temp_work_dir("timeout-abort");
        let input_file = work_dir.join("source.txt");
        let output_file = work_dir.join("out.txt");
        std::fs::write(&input_file, "timeout abort").unwrap();

        let server = HangServer::start().await;
        let toml_str = hang_module_pipeline_toml(&input_file, &output_file);
        let pipeline = Pipeline::from_toml_str(&toml_str).unwrap();
        let mut runner = PipelineRunnerImpl::new(work_dir.clone());
        runner.set_module_port("mock-hang", server.port);
        runner.set_default_node_timeout(Some(std::time::Duration::from_secs(1)));

        let started = std::time::Instant::now();
        let result = runner.execute_async(&pipeline, &work_dir).await;
        let elapsed = started.elapsed();

        assert!(result.is_err(), "超时节点应使管线失败");
        assert!(
            elapsed < std::time::Duration::from_secs(4),
            "超时应在 1s 附近生效而非等满客户端超时（elapsed: {elapsed:?}）"
        );
        // 在飞请求被 abort：模块侧观察到连接断开
        server.wait_disconnected().await;
        // 任务状态与资源回收：失败终态、无下游产物
        assert!(matches!(*runner.task_status(), TaskStatus::Failed(_)));
        assert!(!output_file.exists(), "超时中断后不应产生下游产物");
        let detail = runner.get_task_detail(&runner.list_tasks()[0].id).unwrap();
        let slow = detail.nodes.iter().find(|n| n.node_id == "slow").unwrap();
        assert_eq!(slow.state, "failed");
        assert!(
            slow.error.as_deref().unwrap_or("").contains("timed out"),
            "错误应注明超时: {:?}",
            slow.error
        );
        let output = detail.nodes.iter().find(|n| n.node_id == "output").unwrap();
        assert_eq!(output.state, "skipped");

        cleanup_dir(&work_dir);
    }

    /// 在飞期间置位取消 → abort 在飞模块 HTTP 请求、任务终结 Cancelled
    /// （缺陷 #5：不再等在飞请求自然结束）
    #[tokio::test]
    async fn test_cancel_aborts_inflight_module_request() {
        let work_dir = temp_work_dir("cancel-inflight");
        let input_file = work_dir.join("source.txt");
        let output_file = work_dir.join("out.txt");
        std::fs::write(&input_file, "cancel inflight").unwrap();

        let server = HangServer::start().await;
        let toml_str = hang_module_pipeline_toml(&input_file, &output_file);
        let pipeline = Pipeline::from_toml_str(&toml_str).unwrap();
        let mut runner = PipelineRunnerImpl::new(work_dir.clone());
        runner.set_module_port("mock-hang", server.port);

        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        runner.set_cancel_flag(flag.clone());
        // 模块节点启动后：先等 TCP 连接建立（请求确实在飞），再置位取消
        //（spawn 异步置位，避免同步回调内阻塞 runtime）
        let flag_for_cb = flag.clone();
        let connected_for_cb = server.connected.clone();
        runner.on_node_start = Some(Arc::new(move |node_id| {
            if node_id == "slow" {
                let flag = flag_for_cb.clone();
                let conn = connected_for_cb.clone();
                tokio::spawn(async move {
                    while !conn.load(std::sync::atomic::Ordering::SeqCst) {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                });
            }
        }));

        let started = std::time::Instant::now();
        let result = runner.execute_async(&pipeline, &work_dir).await;
        let elapsed = started.elapsed();

        assert!(result.is_err());
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "取消应中断在飞请求而非等满客户端 300s 超时（elapsed: {elapsed:?}）"
        );
        // 任务状态：Cancelled；资源回收：无下游产物、下游节点 skipped
        assert_eq!(*runner.task_status(), TaskStatus::Cancelled);
        assert!(!output_file.exists(), "取消后不应产生下游产物");
        let detail = runner.get_task_detail(&runner.list_tasks()[0].id).unwrap();
        let input = detail.nodes.iter().find(|n| n.node_id == "input").unwrap();
        assert_eq!(input.state, "completed", "取消前完成的节点应保持完成");
        let slow = detail.nodes.iter().find(|n| n.node_id == "slow").unwrap();
        assert_eq!(slow.state, "failed");
        assert!(
            slow.error.as_deref().unwrap_or("").contains("cancelled"),
            "在飞节点错误应注明取消: {:?}",
            slow.error
        );
        let output = detail.nodes.iter().find(|n| n.node_id == "output").unwrap();
        assert_eq!(output.state, "skipped");
        // 在飞请求被 abort：模块侧观察到连接断开
        server.wait_disconnected().await;

        cleanup_dir(&work_dir);
    }

    // ─── B3（P0-6）：节点级 wall-clock 超时（timeout_secs / 默认值） ────────

    /// ffmpeg `-re` 以实时速度生成 5s 音频；节点 timeout_secs=1 → 超时失败
    #[tokio::test]
    async fn test_node_timeout_secs_wraps_execution() {
        if !ffmpeg_available() {
            eprintln!("SKIP: ffmpeg not available");
            return;
        }

        let work_dir = temp_work_dir("node-timeout");
        let dummy_input = work_dir.join("dummy.txt");
        std::fs::write(&dummy_input, "dummy").unwrap();
        let output_file = work_dir.join("slow.wav");

        let toml_str = format!(
            r#"
[pipeline]
id = "test-node-timeout"
name = "Node Timeout Test"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "slow"
kind = "builtin"
builtin = "ffmpeg"
timeout_secs = 1
params = {{ args = ["-re", "-f", "lavfi", "-i", "sine=frequency=440:duration=5", "-f", "wav", "-y"], output = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["slow", "input"]
"#,
            dummy_input.to_string_lossy().replace('\\', "/"),
            output_file.to_string_lossy().replace('\\', "/"),
        );
        let pipeline = Pipeline::from_toml_str(&toml_str).unwrap();
        let mut runner = PipelineRunnerImpl::new(work_dir.clone());

        let started = std::time::Instant::now();
        let result = runner.execute_async(&pipeline, &work_dir).await;
        let elapsed = started.elapsed();

        assert!(result.is_err(), "超时节点应使管线失败");
        assert!(
            elapsed < std::time::Duration::from_secs(4),
            "超时应在 1s 附近生效，而不是等满 5s（elapsed: {elapsed:?}）"
        );
        let detail = runner.get_task_detail(&runner.list_tasks()[0].id).unwrap();
        let slow = detail.nodes.iter().find(|n| n.node_id == "slow").unwrap();
        assert_eq!(slow.state, "failed");
        assert!(
            slow.error.as_deref().unwrap_or("").contains("timed out"),
            "错误应注明超时: {:?}",
            slow.error
        );

        cleanup_dir(&work_dir);
    }

    /// default_node_timeout 对未声明 timeout_secs 的节点生效
    #[tokio::test]
    async fn test_default_node_timeout_applies() {
        if !ffmpeg_available() {
            eprintln!("SKIP: ffmpeg not available");
            return;
        }

        let work_dir = temp_work_dir("default-timeout");
        let dummy_input = work_dir.join("dummy.txt");
        std::fs::write(&dummy_input, "dummy").unwrap();
        let output_file = work_dir.join("slow2.wav");

        let toml_str = format!(
            r#"
[pipeline]
id = "test-default-timeout"
name = "Default Timeout Test"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "slow"
kind = "builtin"
builtin = "ffmpeg"
params = {{ args = ["-re", "-f", "lavfi", "-i", "sine=frequency=440:duration=5", "-f", "wav", "-y"], output = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["slow", "input"]
"#,
            dummy_input.to_string_lossy().replace('\\', "/"),
            output_file.to_string_lossy().replace('\\', "/"),
        );
        let pipeline = Pipeline::from_toml_str(&toml_str).unwrap();
        let mut runner = PipelineRunnerImpl::new(work_dir.clone());
        runner.set_default_node_timeout(Some(std::time::Duration::from_secs(1)));

        let started = std::time::Instant::now();
        let result = runner.execute_async(&pipeline, &work_dir).await;
        assert!(result.is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(4));
        let detail = runner.get_task_detail(&runner.list_tasks()[0].id).unwrap();
        let slow = detail.nodes.iter().find(|n| n.node_id == "slow").unwrap();
        assert!(slow.error.as_deref().unwrap_or("").contains("timed out"));

        cleanup_dir(&work_dir);
    }
}
