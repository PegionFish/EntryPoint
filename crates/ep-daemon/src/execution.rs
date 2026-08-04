//! 管线执行调度与任务注册表 — Wave 2 执行代理（W2-D）
//!
//! ## 并发方案（为什么不在 `state.runner` 上直接执行）
//!
//! 以 ep-core 现状为准：
//! 1. 引擎唯一执行入口 [`ep_core::types::PipelineRunner::execute`] 是**同步阻塞**调用，
//!    且要求 `&mut self` 覆盖整个执行期（可达数分钟）。`state.runner` 被
//!    `Arc<tokio::sync::Mutex<_>>` 保护，若执行期间持锁，`GET /api/tasks` 等查询
//!    将被阻塞到执行结束——任务书明确禁止。
//! 2. `PipelineRunnerImpl` 的任务存储 `tasks` 为**私有字段**，没有任何公开 API
//!    可以把外部执行的任务注回；`get_task_detail` 也**不暴露节点产物**（Artifact），
//!    而产物列表/下载接口必须拿到产物路径。
//!
//! 因此本模块的实现是：
//! - 每次提交创建一台**独立的** [`PipelineRunnerImpl`]（注册运行中模块端口 +
//!   进度回调），放进 `tokio::task::spawn_blocking` 执行，全程不触碰 `state.runner`，
//!   任务查询接口永不阻塞；引擎的同步 `execute` 在 blocking 线程上自建 tokio
//!   运行时 block_on（blocking 线程无 Handle，走 `execute` 的非嵌套分支）。
//! - 任务状态由进程级 [`TASK_REGISTRY`] 注册表维护：
//!   * 提交时写入 running 记录（所有节点 pending）；
//!   * 执行中由引擎 `on_node_*` 回调实时更新节点状态，并向 `state.progress_tx`
//!     发送 `ProgressMessage`（`/ws` 聚合端点包装成 `{"type":"progress",...}` 推送）；
//!   * 执行结束后以引擎自身的 `list_tasks`/`get_task_detail` 为权威校正终态
//!     （补上回调不会触发的 skipped 节点），并把文件产物硬链接/复制到任务目录的
//!     `files/{node_id}/` 下，供 `GET /api/tasks/:id/artifacts/:node_id` 流式下载。
//!
//! ## 模块声明
//!
//! ep-daemon 为纯 bin crate，main.rs 非本代理所有：本文件由 `api/execute.rs`
//! 通过 `#[path]` 声明为子模块（与 pipeline_bridge.rs 同款做法）。
//! **请勿在 main.rs 中追加 `mod execution;`** —— 同一文件被声明两次会把
//! 静态注册表分裂成两份，执行与查询将互不可见。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::sync::broadcast;
use tracing::{info, warn};

use ep_core::pipeline::dag::Pipeline;
use ep_core::pipeline::runner::TaskDetail;
use ep_core::pipeline::PipelineRunnerImpl;
use ep_core::types::{Artifact, PipelineRunner};

use crate::state::{AppState, ProgressMessage};

// ─── 任务注册表 ──────────────────────────────────────────────────────────────

/// 任务整体状态（API 序列化为小写字符串）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    Running,
    Completed,
    Failed(String),
}

impl TaskState {
    /// 前端契约：小写状态字符串
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed(_) => "failed",
        }
    }
}

/// 单节点状态记录
#[derive(Debug, Clone)]
pub struct NodeRecord {
    /// pending / running / completed / failed / skipped
    pub state: String,
    pub error: Option<String>,
}

/// 一条管线任务记录（注册表值）
#[derive(Debug, Clone)]
pub struct TaskRecord {
    /// 提交时生成的任务 ID（对外唯一标识）
    pub id: String,
    /// 管线 ID（= ProgressMessage.pipeline_id = API 输出中的 pipeline_name，
    /// 与引擎 TaskSummary.pipeline_name 取值语义一致）
    pub pipeline_id: String,
    pub status: TaskState,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// 节点定义顺序（保证详情/产物输出顺序稳定）
    pub node_order: Vec<String>,
    pub nodes: HashMap<String, NodeRecord>,
    /// node_id → 引擎输出的原始产物文件路径
    pub artifacts: HashMap<String, PathBuf>,
    /// node_id → 归集到任务目录 `files/{node_id}/` 下的产物路径（ServeDir 根内，可下载）
    pub served_artifacts: HashMap<String, PathBuf>,
    /// 任务工作目录（{workspace}/tasks/{task_id}）
    pub work_dir: PathBuf,
}

static TASK_REGISTRY: OnceLock<Mutex<HashMap<String, TaskRecord>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, TaskRecord>> {
    TASK_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 所有任务快照（新任务在前），供 `GET /api/tasks` 使用
pub fn snapshot_all() -> Vec<TaskRecord> {
    let mut list: Vec<TaskRecord> = registry().lock().unwrap().values().cloned().collect();
    list.sort_by(|a, b| b.started_at.cmp(&a.started_at).then_with(|| b.id.cmp(&a.id)));
    list
}

/// 单个任务快照，供 `GET /api/tasks/:id` 等使用
pub fn snapshot(task_id: &str) -> Option<TaskRecord> {
    registry().lock().unwrap().get(task_id).cloned()
}

/// 获取节点可下载产物路径（位于 ServeDir 根内）。
///
/// 收尾归集失败时可惰性补链接；文件不存在返回 None。
pub fn ensure_served_artifact(task_id: &str, node_id: &str) -> Option<PathBuf> {
    // 已有归集路径且文件仍在 → 直接返回
    let existing = registry()
        .lock()
        .unwrap()
        .get(task_id)
        .and_then(|r| r.served_artifacts.get(node_id).cloned());
    if let Some(path) = existing {
        if path.is_file() {
            return Some(path);
        }
    }

    // 惰性补归集（收尾阶段链接失败 / 产物后来才可用）
    let (src, task_dir) = {
        let reg = registry().lock().unwrap();
        let record = reg.get(task_id)?;
        (
            record.artifacts.get(node_id).cloned()?,
            record.work_dir.clone(),
        )
    };
    if !src.is_file() {
        return None;
    }
    let dest_dir = task_dir.join("files").join(node_id);
    std::fs::create_dir_all(&dest_dir).ok()?;
    let dest = dest_dir.join(src.file_name()?);
    if !dest.exists()
        && std::fs::hard_link(&src, &dest).is_err()
        && std::fs::copy(&src, &dest).is_err()
    {
        warn!(task_id, node_id, "lazy artifact collection failed; artifact will not be downloadable");
        return None;
    }
    registry()
        .lock()
        .unwrap()
        .get_mut(task_id)
        .map(|r| r.served_artifacts.insert(node_id.to_string(), dest.clone()));
    Some(dest)
}

// ─── 提交错误 ────────────────────────────────────────────────────────────────

/// 提交失败原因（handler 据此映射 HTTP 状态码与 i18n 键）。
///
/// 内部值均为技术标识（node_id / 管线 id / 英文错误细节），仅用于日志与
/// 测试；面向用户的文案由 API handler 层经 `err_response` 按语言生成。
#[derive(Debug)]
pub enum SubmitError {
    /// `inputs` 引用了管线中不存在的节点 → 400（node_id）
    UnknownInputNode(String),
    /// `inputs[node_id]` 不是参数对象 → 400（node_id）
    InputsNotObject(String),
    /// 管线图中存在环 → 400（管线 id）
    CycleDetected(String),
    /// 内部错误（工作目录创建失败等）→ 500（英文技术细节）
    Internal(String),
}

impl std::fmt::Display for SubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownInputNode(id) => {
                write!(f, "inputs reference unknown node: {id}")
            }
            Self::InputsNotObject(id) => {
                write!(f, "inputs[\"{id}\"] must be a parameter object")
            }
            Self::CycleDetected(id) => {
                write!(f, "pipeline `{id}` contains a cycle and cannot be executed")
            }
            Self::Internal(msg) => f.write_str(msg),
        }
    }
}

// ─── 任务 ID 生成 ────────────────────────────────────────────────────────────

static TASK_SEQ: AtomicUsize = AtomicUsize::new(0);

/// 生成任务 ID：`task-{UTC 时间戳}-{进程内序号}`（可读、可排序、进程内唯一）
fn new_task_id() -> String {
    let seq = TASK_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("task-{}-{seq:04}", Utc::now().format("%Y%m%d-%H%M%S"))
}

// ─── 输入参数覆盖 ────────────────────────────────────────────────────────────

/// 将请求中的 `inputs`（node_id → 参数对象）浅合并进对应节点的 params。
///
/// 引擎没有独立的 inputs 机制——`execute_node` 直接读取 `node.params`，
/// 故覆盖 params 即官方语义（如 file_input 的 path）。
fn apply_inputs(
    pipeline: &mut Pipeline,
    inputs: &HashMap<String, Value>,
) -> Result<(), SubmitError> {
    for (node_id, params) in inputs {
        let node = pipeline
            .nodes
            .iter_mut()
            .find(|n| &n.id == node_id)
            .ok_or_else(|| SubmitError::UnknownInputNode(node_id.clone()))?;
        let obj = params
            .as_object()
            .ok_or_else(|| SubmitError::InputsNotObject(node_id.clone()))?;
        if !node.params.is_object() {
            node.params = Value::Object(Default::default());
        }
        let target = node
            .params
            .as_object_mut()
            .expect("params was just ensured to be an object");
        for (key, value) in obj {
            target.insert(key.clone(), value.clone());
        }
    }
    Ok(())
}

// ─── 提交入口 ────────────────────────────────────────────────────────────────

/// 提交管线执行（立即返回 task_id，执行在后台进行）。
///
/// 流程：校验/合并 inputs → 解析任务工作目录 → 收集运行中模块端口 →
/// 写入 running 记录 → spawn_blocking 中用独立 `PipelineRunnerImpl` 执行。
pub async fn submit_pipeline(
    state: &Arc<AppState>,
    mut pipeline: Pipeline,
    inputs: Option<HashMap<String, Value>>,
) -> Result<String, SubmitError> {
    if let Some(inputs) = inputs.as_ref() {
        apply_inputs(&mut pipeline, inputs)?;
    }

    // 环检测：引擎执行前会做拓扑分层，有环的管线在此直接 400，
    // 避免提交一个注定失败的任务
    if pipeline.topological_layers().is_err() {
        return Err(SubmitError::CycleDetected(pipeline.id.clone()));
    }

    let workspace = state
        .config
        .read()
        .await
        .resolve_workspace_dir(&state.root);
    let task_id = new_task_id();
    let task_dir = workspace.join("tasks").join(&task_id);
    std::fs::create_dir_all(&task_dir).map_err(|e| {
        SubmitError::Internal(format!(
            "failed to create task working directory `{}`: {e}",
            task_dir.display()
        ))
    })?;

    // 注册正在运行（Running/Starting）的模块端口
    let module_ports: HashMap<String, u16> = {
        let pm = state.process_manager.read().await;
        pm.list_running()
            .iter()
            .filter_map(|inst| inst.port.map(|port| (inst.module_id.clone(), port)))
            .collect()
    };

    // 写入初始记录：任务 running、所有节点 pending
    {
        let mut reg = registry().lock().unwrap();
        let nodes = pipeline
            .nodes
            .iter()
            .map(|n| {
                (
                    n.id.clone(),
                    NodeRecord {
                        state: "pending".to_string(),
                        error: None,
                    },
                )
            })
            .collect();
        reg.insert(
            task_id.clone(),
            TaskRecord {
                id: task_id.clone(),
                pipeline_id: pipeline.id.clone(),
                status: TaskState::Running,
                started_at: Utc::now(),
                finished_at: None,
                node_order: pipeline.nodes.iter().map(|n| n.id.clone()).collect(),
                nodes,
                artifacts: HashMap::new(),
                served_artifacts: HashMap::new(),
                work_dir: task_dir.clone(),
            },
        );
    }

    let progress_tx = state.progress_tx.clone();
    let pipeline_ref = format!("{}/{}", pipeline.id, pipeline.name);
    let task_id_bg = task_id.clone();
    let task_id_watch = task_id.clone();
    tokio::spawn(async move {
        let joined = tokio::task::spawn_blocking(move || {
            run_task(task_id_bg, task_dir, pipeline, module_ports, progress_tx)
        })
        .await;
        if let Err(e) = joined {
            // 执行线程 panic/被取消——注册表记录不能永远停在 running
            warn!(task_id = %task_id_watch, error = %e, "pipeline execution thread exited abnormally");
            finalize_aborted(&task_id_watch, &e.to_string());
        }
    });

    info!(task_id = %task_id, pipeline = %pipeline_ref, "pipeline task submitted");
    Ok(task_id)
}

// ─── 后台执行（spawn_blocking 线程内） ───────────────────────────────────────

/// 在独立 `PipelineRunnerImpl` 上同步执行管线并收尾。
///
/// 本函数运行于 blocking 线程（无 tokio Handle），引擎 `execute` 会走
/// 「自建运行时 block_on」分支，不会嵌套 panic，也不占用主运行时线程。
fn run_task(
    task_id: String,
    task_dir: PathBuf,
    pipeline: Pipeline,
    module_ports: HashMap<String, u16>,
    progress_tx: broadcast::Sender<ProgressMessage>,
) {
    let pipeline_id = pipeline.id.clone();
    let node_count = pipeline.nodes.len();

    let mut runner = PipelineRunnerImpl::new(task_dir.clone());
    runner.set_module_ports(module_ports);

    // 回调：节点开始 → running
    {
        let tx = progress_tx.clone();
        let pid = pipeline_id.clone();
        let tid = task_id.clone();
        runner.on_node_start = Some(Arc::new(move |node_id| {
            // 广播发送失败（无订阅者）忽略
            let _ = tx.send(ProgressMessage {
                pipeline_id: pid.clone(),
                node_id: node_id.to_string(),
                status: "running".to_string(),
            });
            set_node_state(&tid, node_id, "running", None);
        }));
    }
    // 回调：节点完成 → completed + 记录产物
    {
        let tx = progress_tx.clone();
        let pid = pipeline_id.clone();
        let tid = task_id.clone();
        runner.on_node_complete = Some(Arc::new(move |node_id, artifact| {
            let _ = tx.send(ProgressMessage {
                pipeline_id: pid.clone(),
                node_id: node_id.to_string(),
                status: "completed".to_string(),
            });
            set_node_state(&tid, node_id, "completed", None);
            if let Artifact::File(path) = artifact {
                record_artifact(&tid, node_id, path);
            }
        }));
    }
    // 回调：节点失败 → failed
    {
        let tx = progress_tx.clone();
        let pid = pipeline_id.clone();
        let tid = task_id.clone();
        runner.on_node_error = Some(Arc::new(move |node_id, error| {
            let _ = tx.send(ProgressMessage {
                pipeline_id: pid.clone(),
                node_id: node_id.to_string(),
                status: "failed".to_string(),
            });
            set_node_state(&tid, node_id, "failed", Some(error.to_string()));
        }));
    }

    // 引擎同步执行（内部自建运行时；整个调用阻塞本 blocking 线程）
    let result = PipelineRunner::execute(&mut runner, &pipeline, &task_dir);

    // 以引擎自身任务详情为权威校正终态（含回调不覆盖的 skipped 节点）
    let detail = runner
        .list_tasks()
        .pop()
        .and_then(|summary| runner.get_task_detail(&summary.id));
    let error_msg = result.as_ref().err().map(|e| e.to_string());
    finalize_task(&task_id, &task_dir, error_msg, detail.as_ref());

    match result {
        Ok(()) => info!(task_id = %task_id, nodes = node_count, "pipeline task finished"),
        Err(e) => warn!(task_id = %task_id, error = %e, "pipeline task failed"),
    }
}

// ─── 注册表内部操作（记录缺失时一律 no-op，避免测试清表后被后台回调复活） ────

fn set_node_state(task_id: &str, node_id: &str, state: &str, error: Option<String>) {
    let mut reg = registry().lock().unwrap();
    if let Some(record) = reg.get_mut(task_id) {
        record.nodes.insert(
            node_id.to_string(),
            NodeRecord {
                state: state.to_string(),
                error,
            },
        );
    }
}

fn record_artifact(task_id: &str, node_id: &str, path: &Path) {
    let mut reg = registry().lock().unwrap();
    if let Some(record) = reg.get_mut(task_id) {
        record
            .artifacts
            .insert(node_id.to_string(), path.to_path_buf());
    }
}

/// 执行结束收尾：引擎 detail 校正节点终态、写任务状态/完成时间、归集产物。
fn finalize_task(
    task_id: &str,
    task_dir: &Path,
    error_msg: Option<String>,
    detail: Option<&TaskDetail>,
) {
    let mut reg = registry().lock().unwrap();
    let Some(record) = reg.get_mut(task_id) else {
        return;
    };

    if let Some(detail) = detail {
        for node in &detail.nodes {
            record.nodes.insert(
                node.node_id.clone(),
                NodeRecord {
                    state: node.state.clone(),
                    error: node.error.clone(),
                },
            );
        }
    }

    record.status = match error_msg {
        None => TaskState::Completed,
        Some(msg) => TaskState::Failed(msg),
    };
    record.finished_at = Some(Utc::now());

    // 产物归集：硬链接（跨文件系统时退化为复制）到 files/{node_id}/ 下，
    // 使所有产物都落在 ServeDir 根内可下载；清理不在本期范围
    let artifacts = record.artifacts.clone();
    for (node_id, src) in artifacts {
        if !src.is_file() {
            continue;
        }
        let Some(name) = src.file_name() else {
            continue;
        };
        let dest_dir = task_dir.join("files").join(&node_id);
        if std::fs::create_dir_all(&dest_dir).is_err() {
            continue;
        }
        let dest = dest_dir.join(name);
        if dest.exists()
            || std::fs::hard_link(&src, &dest).is_ok()
            || std::fs::copy(&src, &dest).is_ok()
        {
            record
                .served_artifacts
                .insert(node_id.clone(), dest.clone());
        } else {
            warn!(task_id, node_id = %node_id, "artifact collection failed; node artifact will not be downloadable");
        }
    }
}

/// spawn_blocking 线程异常退出的兜底收尾
fn finalize_aborted(task_id: &str, error: &str) {
    let mut reg = registry().lock().unwrap();
    if let Some(record) = reg.get_mut(task_id) {
        if record.status == TaskState::Running {
            record.status =
                TaskState::Failed(format!("execution thread exited abnormally: {error}"));
            record.finished_at = Some(Utc::now());
        }
    }
}

// ─── 测试专用 ────────────────────────────────────────────────────────────────

/// 串行化所有触碰注册表的测试（注册表是进程级静态）
#[cfg(test)]
pub static TEST_LOCK: Mutex<()> = Mutex::new(());

/// 获取测试锁（中毒后自动恢复，避免单个测试失败级联拖垮其余测试）
#[cfg(test)]
pub fn lock_for_tests() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
pub fn clear_registry_for_tests() {
    registry().lock().unwrap().clear();
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    // 测试锁的唯一目的就是跨 await 串行化这些共享静态注册表的测试；
    // 锁内临界区全部是极短同步操作，不存在持锁阻塞运行时的风险。
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use std::time::Duration;

    use ep_core::config::AppConfig;
    use ep_core::port::PortManager;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// 路径转义为 TOML basic string（Windows 路径含 `\`，不转义会被解析为转义序列）
    fn toml_path(p: &std::path::Path) -> String {
        p.display().to_string().replace('\\', "\\\\").replace('"', "\\\"")
    }

    fn unique_root(tag: &str) -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-exec-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_state(root: PathBuf) -> Arc<AppState> {
        Arc::new(AppState::new(
            root,
            AppConfig::default(),
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        ))
    }

    /// 最小纯 builtin 管线（file_input → file_output，无 path 参数）。
    /// 仅用于在提交期即被拒绝的场景（不会真正执行到节点）。
    fn minimal_pipeline(id: &str) -> Pipeline {
        let toml = format!(
            r#"
[pipeline]
id = "{id}"
name = "最小测试管线"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"

[[edges]]
from = ["input", "output"]
to = ["output", "input"]
"#
        );
        Pipeline::from_toml_str(&toml).unwrap()
    }

    /// 轮询等待任务终结（completed/failed），超时返回 None
    async fn wait_terminal(task_id: &str) -> Option<TaskRecord> {
        for _ in 0..300 {
            if let Some(record) = snapshot(task_id) {
                if !matches!(record.status, TaskState::Running) {
                    return Some(record);
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        None
    }

    // ── 1. 纯 builtin 小管线直调执行（file_input → file_output） ────────────

    #[tokio::test]
    async fn test_submit_builtin_pipeline_runs_to_completion() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("ok");
        let state = test_state(root.clone());

        let src = root.join("source.txt");
        let dest = root.join("delivered.txt");
        std::fs::write(&src, "引擎直调测试内容").unwrap();

        let toml = format!(
            r#"
[pipeline]
id = "direct-run"
name = "直调测试"

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
            toml_path(&src),
            toml_path(&dest),
        );
        let pipeline = Pipeline::from_toml_str(&toml).unwrap();

        let task_id = submit_pipeline(&state, pipeline, None).await.unwrap();
        let record = wait_terminal(&task_id)
            .await
            .expect("任务应在超时前终结");

        assert_eq!(record.status, TaskState::Completed);
        assert!(record.finished_at.is_some());
        assert_eq!(record.pipeline_id, "direct-run");
        // 引擎真实执行：目标文件被写出且内容一致
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "引擎直调测试内容");
        // 节点终态（引擎 detail 校正）
        assert_eq!(record.nodes["input"].state, "completed");
        assert_eq!(record.nodes["output"].state, "completed");
        assert_eq!(record.nodes.len(), 2);
        // 两个节点都有文件产物，且已归集到 ServeDir 根内
        assert_eq!(record.artifacts.len(), 2);
        assert_eq!(record.served_artifacts.len(), 2);
        for (node_id, served) in &record.served_artifacts {
            assert!(served.is_file(), "node {node_id} 归集产物应存在");
            assert!(
                served.starts_with(&record.work_dir),
                "归集产物必须位于任务工作目录内"
            );
        }
    }

    // ── 2. inputs 覆盖节点 params（file_input 的 path） ─────────────────────

    #[tokio::test]
    async fn test_submit_with_inputs_override() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("inputs");
        let state = test_state(root.clone());

        let src = root.join("override-src.txt");
        let dest = root.join("override-out.txt");
        std::fs::write(&src, "inputs 覆盖").unwrap();

        let toml = format!(
            r#"
[pipeline]
id = "inputs-test"
name = "inputs 测试"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["output", "input"]
"#,
            toml_path(&dest),
        );
        let pipeline = Pipeline::from_toml_str(&toml).unwrap();

        let mut inputs = HashMap::new();
        inputs.insert(
            "input".to_string(),
            serde_json::json!({ "path": src.display().to_string() }),
        );

        let task_id = submit_pipeline(&state, pipeline, Some(inputs))
            .await
            .unwrap();
        let record = wait_terminal(&task_id).await.expect("任务应终结");
        assert_eq!(record.status, TaskState::Completed);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "inputs 覆盖");
    }

    // ── 3. inputs 引用不存在的节点 → UnknownInputNode ──────────────────────

    #[tokio::test]
    async fn test_submit_inputs_unknown_node_rejected() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("bad-input");
        let state = test_state(root);
        let pipeline = minimal_pipeline("bad-input");

        let mut inputs = HashMap::new();
        inputs.insert("ghost".to_string(), serde_json::json!({ "path": "/x" }));

        let err = submit_pipeline(&state, pipeline, Some(inputs))
            .await
            .unwrap_err();
        assert!(matches!(err, SubmitError::UnknownInputNode(_)));
        assert!(err.to_string().contains("ghost"));
    }

    // ── 4. inputs 值不是对象 → InputsNotObject ─────────────────────────────

    #[tokio::test]
    async fn test_submit_inputs_non_object_rejected() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("non-object");
        let state = test_state(root);
        let pipeline = minimal_pipeline("non-object");

        let mut inputs = HashMap::new();
        inputs.insert("input".to_string(), serde_json::json!("不是对象"));

        let err = submit_pipeline(&state, pipeline, Some(inputs))
            .await
            .unwrap_err();
        assert!(matches!(err, SubmitError::InputsNotObject(_)));
        // 技术层消息为英文
        assert!(err.to_string().contains("must be a parameter object"));
    }

    // ── 5. 失败管线：缺输入文件 → failed + 空产物 ───────────────────────────

    #[tokio::test]
    async fn test_submit_failed_pipeline_records_failure() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("fail");
        let state = test_state(root);

        let toml = r#"
[pipeline]
id = "fail-test"
name = "失败测试"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = { path = "/nonexistent/definitely-missing.txt" }

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
params = { path = "/tmp/ep-never.txt" }

[[edges]]
from = ["input", "output"]
to = ["output", "input"]
"#;
        let pipeline = Pipeline::from_toml_str(toml).unwrap();
        let task_id = submit_pipeline(&state, pipeline, None).await.unwrap();
        let record = wait_terminal(&task_id).await.expect("任务应终结");

        assert!(matches!(record.status, TaskState::Failed(_)));
        assert_eq!(record.nodes["input"].state, "failed");
        assert!(record.nodes["input"].error.is_some());
        // 下游被引擎标记 skipped
        assert_eq!(record.nodes["output"].state, "skipped");
        // 无产物
        assert!(record.artifacts.is_empty());
    }

    // ── 6. 有环管线 → 提交期即拒绝（CycleDetected） ─────────────────────────

    #[tokio::test]
    async fn test_submit_cycle_rejected_synchronously() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("cycle");
        let state = test_state(root);
        let toml = r#"
[pipeline]
id = "cycle-test"
name = "环测试"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "a"
kind = "builtin"
builtin = "file_output"

[[edges]]
from = ["input", "output"]
to = ["a", "input"]

[[edges]]
from = ["a", "output"]
to = ["input", "input"]
"#;
        let pipeline = Pipeline::from_toml_str(toml).unwrap();
        let err = submit_pipeline(&state, pipeline, None)
            .await
            .unwrap_err();
        assert!(matches!(err, SubmitError::CycleDetected(_)));
        // 技术层消息为英文
        assert!(err.to_string().contains("contains a cycle"));
    }

    // ── 7. 进度回调 → progress_tx（WS 链路的数据源） ────────────────────────

    #[tokio::test]
    async fn test_progress_messages_sent_to_broadcast() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("ws");
        let state = test_state(root.clone());
        let mut rx = state.progress_tx.subscribe();

        let src = root.join("ws-src.txt");
        let dest = root.join("ws-out.txt");
        std::fs::write(&src, "ws").unwrap();
        let toml = format!(
            r#"
[pipeline]
id = "ws-test"
name = "WS 测试"

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
            toml_path(&src),
            toml_path(&dest),
        );
        let pipeline = Pipeline::from_toml_str(&toml).unwrap();
        let task_id = submit_pipeline(&state, pipeline, None).await.unwrap();
        wait_terminal(&task_id).await.expect("任务应终结");

        // 收集广播中的进度消息（两节点 × start/complete = 4 条）
        let mut got: Vec<ProgressMessage> = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            got.push(msg);
        }
        assert!(got.len() >= 4, "至少 4 条进度消息，got {}", got.len());
        assert!(got.iter().all(|m| m.pipeline_id == "ws-test"));
        let statuses: Vec<&str> = got.iter().map(|m| m.status.as_str()).collect();
        assert!(statuses.iter().filter(|s| **s == "running").count() >= 2);
        assert!(statuses.iter().filter(|s| **s == "completed").count() >= 2);
        // 小写字符串契约
        assert!(statuses
            .iter()
            .all(|s| ["running", "completed", "failed"].contains(s)));
    }

    // ── 8. ensure_served_artifact 惰性归集 ──────────────────────────────────

    #[tokio::test]
    async fn test_ensure_served_artifact_lazy_link() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("lazy");
        let state = test_state(root.clone());

        let src = root.join("lazy-src.txt");
        let dest = root.join("lazy-out.txt");
        std::fs::write(&src, "lazy").unwrap();
        let toml = format!(
            r#"
[pipeline]
id = "lazy-test"
name = "惰性归集测试"

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
            toml_path(&src),
            toml_path(&dest),
        );
        let pipeline = Pipeline::from_toml_str(&toml).unwrap();
        let task_id = submit_pipeline(&state, pipeline, None).await.unwrap();
        wait_terminal(&task_id).await.expect("任务应终结");

        // 模拟收尾归集缺失：清掉 served_artifacts 后惰性重建
        registry()
            .lock()
            .unwrap()
            .get_mut(&task_id)
            .unwrap()
            .served_artifacts
            .clear();
        // 归集目录也删掉，验证完整重建路径
        let _ = std::fs::remove_dir_all(
            snapshot(&task_id).unwrap().work_dir.join("files"),
        );

        let served = ensure_served_artifact(&task_id, "output").expect("应惰性归集成功");
        assert!(served.is_file());
        assert_eq!(std::fs::read_to_string(&served).unwrap(), "lazy");
        // 未知节点 → None
        assert!(ensure_served_artifact(&task_id, "ghost").is_none());
        // 未知任务 → None
        assert!(ensure_served_artifact("task-ghost", "output").is_none());
    }
}
