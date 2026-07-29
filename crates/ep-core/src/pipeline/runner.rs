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

                // 回调：节点开始
                if let Some(ref cb) = self.on_node_start {
                    cb(node_id);
                }

                let node = pipeline
                    .nodes
                    .iter()
                    .find(|n| &n.id == node_id)
                    .ok_or_else(|| anyhow::anyhow!("node '{node_id}' not found in pipeline"))?;

                match execute_node(node, pipeline, &task, work_dir, &self.module_ports).await {
                    Ok(artifact) => {
                        // 回调：节点完成
                        if let Some(ref cb) = self.on_node_complete {
                            cb(node_id, &artifact);
                        }
                        task.mark_completed(node_id, artifact);
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        // 从 ModuleCallError 提取可重试标志
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

                let mut temp_runner = PipelineRunnerImpl {
                    task: None,
                    tasks: HashMap::new(),
                    work_dir: work_dir_path.clone(),
                    module_ports,
                    on_node_start: on_start,
                    on_node_complete: on_complete,
                    on_node_error: on_error,
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
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"pipeline_name\":\"test-pipe\""));
        assert!(json.contains("\"node_count\":3"));
    }
}
