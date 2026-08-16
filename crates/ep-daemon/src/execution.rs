//! 管线执行调度与任务注册表 — Wave 2 B3 ExecEngine
//!
//! ## 并发模型（§6.8：全局闸门 + 管线级闸门 + 排队可见性）
//!
//! 提交路径不再无限 spawn（修 P1-3），改为两级准入：
//!
//! 1. **提交即入队**：任务以 `queued` 状态写入注册表并进入 FIFO 队列；
//!    若闸门有空位则在同一 await 内立即提升为 `running`（快路径无排队感）。
//! 2. **全局闸门**：同时运行的任务数 ≤ `config.pipeline.max_parallel`。
//! 3. **管线级闸门**：同一管线同时运行的任务数 ≤ 管线 TOML
//!    `[pipeline] max_instances`（缺省跟随全局上限；GPU 重管线可锁 1 防显存打架）。
//! 4. **公平 FIFO**：准入按入队顺序扫描，取第一个两级闸门均放行的任务
//!    （避免单管线闸门满时阻塞其他管线任务的队头阻塞问题）。
//! 5. **排队可见性**：`queued` 任务的队列位置（1 起）经注册表快照对外暴露
//!    （`GET /api/pipelines/{id}/tasks` 等）。
//!
//! 每个任务终结（completed/failed/cancelled）后释放闸门并触发下一轮准入。
//!
//! ## 引擎执行（每次执行创建独立 runner）
//!
//! 以 ep-core 现状为准：
//! 1. 引擎唯一执行入口 [`ep_core::types::PipelineRunner::execute`] 是**同步阻塞**
//!    调用，且要求 `&mut self` 覆盖整个执行期（可达数分钟）。若多任务共享
//!    同一台 runner（`Arc<tokio::sync::Mutex<_>>` 持锁执行），`GET /api/tasks`
//!    等查询将被阻塞到执行结束——任务书明确禁止。
//! 2. `PipelineRunnerImpl` 的任务存储 `tasks` 为**私有字段**，`get_task_detail`
//!    也**不暴露节点产物**（Artifact），而产物列表/下载接口必须拿到产物路径。
//!
//! 因此每次执行创建一台**独立的** [`PipelineRunnerImpl`]（注册运行中模块端口 +
//! 进度回调），放进 `tokio::task::spawn_blocking` 执行，任务查询接口永不阻塞；
//! 引擎的同步 `execute` 在 blocking 线程上自建 tokio
//! 运行时 block_on（blocking 线程无 Handle，走 `execute` 的非嵌套分支）。
//!
//! （历史备注：Wave 2 骨架曾在 `AppState` 预置共享 `runner` 字段，从未被
//! 执行路径使用，已于 Wave 4 D2 死代码清除中移除。）
//!
//! ## 任务注册表（P1-4：下沉 ep-core + 落盘持久化）
//!
//! 任务记录统一存于进程级 [`TaskRegistry`]（`ep_core::task_registry`，
//! daemon/桌面共用）：
//! - 持久化目录 `{root}/runtime/tasks/{task_id}.json`（原子写），
//!   [`AppState::new`](crate::state::AppState::new) 启动时经 [`bind_persistence`]
//!   绑定并回读，daemon 重启不丢索引（遗留 running/queued 加载时改判 failed）；
//! - 节点回调实时更新内存记录（高频路径不落盘），终态收尾时统一持久化。
//!
//! ## 超时与取消（P0-6/P1-11；缺陷 #3 拆分：心跳看门狗 ≠ 节点硬超时）
//!
//! - **任务级空闲看门狗**：`config.pipeline.default_timeout_secs` 的看门狗
//!   周期性检查“是否仍有节点进度/心跳”（节点开始/完成/失败回调 bump
//!   `last_activity_ms`；任一节点 `running` = 在飞调用 = 有心跳），持续
//!   无心跳达阈值才判 `failed`（错误注明超时）。长媒体任务（ASR 转写等）
//!   不再被任务总时长误杀。
//! - **节点级硬超时**（与看门狗解耦）：优先级为 节点自身 `timeout_secs`
//!   → 管线 `[pipeline] node_timeout_secs` → 全局 `default_node_timeout_secs`
//!   → 回退 `default_timeout_secs`（旧配置行为不变），由执行器包裹单节点
//!   wall-clock 执行。
//! - **取消**：[`request_cancel`]（`POST /api/tasks/{id}/cancel`）——排队中取消
//!   立即终结且不执行；运行中取消立即判 `cancelled`（逻辑终态），并经
//!   协作取消标志传播到引擎（缺陷 #5，见下）。
//! - **取消/超时 → 引擎传播（缺陷 #5）**：看门狗判死与用户取消均置位
//!   协作取消标志；runner 在节点边界与**在飞期间**两类检查点响应——
//!   在飞节点的执行任务被 `abort()`：模块 HTTP 请求（/predict/*）future
//!   被丢弃、连接立即断开，ffmpeg 子进程经 kill_on_drop 终止；引擎线程
//!   随之快速退出，不再“标记终态后任请求挂到自然结束”。
//! - **行为边界（诚实声明）**：模块侧（uvicorn 单 worker）推理线程为同步
//!   CPU/GPU 密集执行，平台断开连接后**无法从平台侧中断**，推理可能仍在
//!   收尾，worker 短暂占用属预期（平台侧不再等待/重试，任务资源立即回收）；
//!   引擎线程被 abort 后残余的状态写入因记录已终态而被忽略。
//!
//! ## wait 同步模式 + callback_url（§6.5）
//!
//! - `wait: true`：提交后阻塞至终态，[`SubmitOutcome::record`] 直接携带
//!   status + artifacts（内部超时上限取管线超时配置）；
//! - `callback_url`：终态时 POST `{task_id, status, artifacts}`，best-effort
//!   （失败仅 warn，不影响任务本身）。
//!
//! ## 模块声明
//!
//! ep-daemon 为纯 bin crate，main.rs 非本代理所有：本文件由 `api/execute.rs`
//! 通过 `#[path]` 声明为子模块（与 pipeline_bridge.rs 同款做法）。
//! **请勿在 main.rs 中追加 `mod execution;`** —— 同一文件被声明两次会把
//! 静态注册表分裂成两份，执行与查询将互不可见。

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tracing::{info, warn};

use ep_core::module::manifest::CapabilityDecl;
use ep_core::pipeline::dag::{
    direct_output_extension, NodeKind, Pipeline, PipelineNode, ValidationError,
};
use ep_core::pipeline::runner::TaskDetail;
use ep_core::pipeline::vram;
use ep_core::pipeline::PipelineRunnerImpl;
use ep_core::task_registry::TaskRegistry;
use ep_core::types::{Artifact, PipelineRunner};

use crate::state::{AppState, ProgressMessage};

// 类型下沉 ep-core 后对既有消费方（tasks.rs / execute.rs）保持原路径可用
pub use ep_core::task_registry::{NodeRecord, TaskRecord, TaskState};

// ─── 进程级状态：注册表 / 调度器 / 待执行载荷 / 运行时标志 ──────────────────

static REGISTRY: OnceLock<Mutex<TaskRegistry>> = OnceLock::new();

fn registry() -> &'static Mutex<TaskRegistry> {
    REGISTRY.get_or_init(|| Mutex::new(TaskRegistry::new()))
}

/// 绑定任务注册表的落盘持久化目录（`{root}/runtime/tasks/`）。
///
/// 幂等（同目录重复绑定无操作）。生产路径由 [`crate::state::AppState::new`]
/// 启动时调用；提交路径亦兜底调用（测试/桌面直连场景）。
///
/// 单进程单 root 语义：注册表**非空**（有在案任务）时拒绝切换目录——
/// 生产环境 daemon 只有一个 root；测试环境多个 AppState 并发创建时，
/// 防止后来者的 root 把在先任务的落盘点拐走（测试经
/// `clear_registry_for_tests` 重置后即可重新绑定自己的目录）。
pub fn bind_persistence(root: &Path) {
    let dir = root.join("runtime").join("tasks");
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    if reg.persist_dir() == Some(dir.as_path()) {
        return;
    }
    // 已绑定其他目录、或在案任务存在 → 拒绝漂移（见文档）。
    // 重新绑定的唯一途径是 clear_registry_for_tests / reset_and_bind 的整体重置。
    if reg.persist_dir().is_some() || !reg.is_empty() {
        return;
    }
    match reg.enable_persistence(&dir) {
        Ok(loaded) if loaded > 0 => {
            info!(dir = %dir.display(), loaded, "task registry persistence bound, index restored");
        }
        Ok(_) => {}
        Err(e) => {
            warn!(dir = %dir.display(), error = %e, "failed to bind task registry persistence; tasks will be memory-only");
        }
    }
}

/// 调度器：FIFO 队列 + 两级闸门计数（§6.8）
#[derive(Debug, Default)]
struct Scheduler {
    /// 等待闸门的任务（FIFO）
    queue: VecDeque<QueuedEntry>,
    /// 全局运行中任务数（≤ max_parallel）
    running_count: usize,
    /// 每管线运行中任务数（≤ 该管线 max_instances，缺省跟随全局）
    running_by_pipeline: HashMap<String, usize>,
    /// 每管线并发上限缓存（提交时从管线 TOML 解析刷新）
    pipeline_limits: HashMap<String, Option<u32>>,
}

#[derive(Debug, Clone)]
struct QueuedEntry {
    task_id: String,
    pipeline_id: String,
}

static SCHEDULER: OnceLock<Mutex<Scheduler>> = OnceLock::new();

fn scheduler() -> &'static Mutex<Scheduler> {
    SCHEDULER.get_or_init(|| Mutex::new(Scheduler::default()))
}

/// 在途任务总容量上限（排队 + 运行中，P1 修复）：提交超限时以 429 拒绝，
/// 不再无条件入队 + 建目录 + 落盘（防队列无界膨胀拖垮磁盘与调度）。
const MAX_INFLIGHT_TASKS: usize = 256;

/// 已提交未执行任务的载荷（队列持有；执行启动时取出）
struct PendingTask {
    pipeline: Pipeline,
    task_dir: PathBuf,
    callback_url: Option<String>,
}

static PENDING: OnceLock<Mutex<HashMap<String, PendingTask>>> = OnceLock::new();

fn pending() -> &'static Mutex<HashMap<String, PendingTask>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 运行中任务的协作标志与回调配置
#[derive(Clone)]
struct TaskExtras {
    /// 取消请求标志：与 runner 共享；置位后经节点边界/在飞中断两类检查点
    /// 传播到引擎（缺陷 #5：在飞模块 HTTP 请求被 abort、引擎线程快速退出）
    cancel: Arc<AtomicBool>,
    callback_url: Option<String>,
    /// 最近一次节点进度时刻（epoch 毫秒，缺陷 #3）：节点开始/完成/失败回调
    /// 实时更新。任务级空闲看门狗据此判定“无心跳/无进度”而非任务总时长。
    last_activity_ms: Arc<AtomicU64>,
}

static EXTRAS: OnceLock<Mutex<HashMap<String, TaskExtras>>> = OnceLock::new();

fn extras() -> &'static Mutex<HashMap<String, TaskExtras>> {
    EXTRAS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 终态 CAS 独立表（P1 修复）：终结标志与 extras 表生命周期解耦。
///
/// 旧实现把 `finalize_done` 挂在 [`TaskExtras`] 上，赢家在闸门释放**之前**
/// 执行 `extras().remove(task_id)`——此后并发到达的 finalize 读 None 即
/// 假赢（`None => true`），导致闸门 running_count 多减（超并发准入）、
/// 重复触发完成回调、重复清理工作目录。
///
/// 本表条目与注册表记录同生命周期（任务在案期间不清理）：无论 extras
/// 是否已被移除，已终态任务的 CAS 标志始终在案，第二次 finalize 必然
/// 读到 `swap → false`，不可能再次获胜。
static FINALIZED: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();

fn finalized() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    FINALIZED.get_or_init(|| Mutex::new(HashMap::new()))
}

// ─── 任务级空闲看门狗（缺陷 #3 拆分：心跳/进度看门狗 ≠ 节点硬超时） ───────

/// 看门狗轮询间隔（秒）：周期性检查“是否仍有节点进度/心跳”，而非一次性
/// 到点判死。取 1s 以兼容小 `default_timeout_secs` 的测试与生产快速响应。
const WATCHDOG_TICK_SECS: u64 = 1;

/// 当前时刻（epoch 毫秒；系统时钟早于纪元时钳为 0，避免负值转 u64 溢出）。
fn now_epoch_ms() -> u64 {
    Utc::now().timestamp_millis().max(0) as u64
}

/// 看门狗单次判定（纯函数，可单测）：任务是否因“长时间无心跳/无进度”该被判死。
///
/// 语义（缺陷 #3 拆分后）：
/// - 任一节点处于 `running` = 模块调用在飞，视为有心跳 → **不判死**
///   （长媒体节点如 ASR 转写 >5min 不会被任务看门狗误杀，其真正时限由
///   节点级硬超时缺省 `effective_default_node_timeout` 管辖）；
/// - 无节点在跑且距最近一次进度（`last_activity_ms`）≥ `timeout_secs` → 判死。
fn watchdog_idle_exceeded(
    record: &TaskRecord,
    last_activity_ms: u64,
    now_ms: u64,
    timeout_secs: u32,
) -> bool {
    if record.nodes.values().any(|n| n.state == "running") {
        return false; // 节点执行在飞 = 活跃心跳
    }
    let timeout_ms = u64::from(timeout_secs) * 1000;
    now_ms.saturating_sub(last_activity_ms) >= timeout_ms
}

// ─── 快照查询（永不阻塞：注册表锁内全是短临界区） ───────────────────────────

/// 所有任务快照（新任务在前；queued 任务带实时队列位置），供 `GET /api/tasks`
pub fn snapshot_all() -> Vec<TaskRecord> {
    let mut list = registry().lock().unwrap_or_else(|e| e.into_inner()).all();
    annotate_queue_positions(&mut list);
    list
}

/// 单个任务快照（queued 任务带实时队列位置），供 `GET /api/tasks/:id` 等
pub fn snapshot(task_id: &str) -> Option<TaskRecord> {
    let mut record = registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(task_id)?
        .clone();
    if record.status == TaskState::Queued {
        record.queue_position = live_queue_position(task_id);
    } else {
        record.queue_position = None;
    }
    Some(record)
}

/// 按 pipeline_id 的任务快照（新任务在前），供 `GET /api/pipelines/{id}/tasks`
pub fn snapshot_by_pipeline(pipeline_id: &str) -> Vec<TaskRecord> {
    let mut list = registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .by_pipeline(pipeline_id);
    annotate_queue_positions(&mut list);
    list
}

fn live_queue_position(task_id: &str) -> Option<usize> {
    scheduler()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .queue
        .iter()
        .position(|e| e.task_id == task_id)
        .map(|p| p + 1)
}

fn annotate_queue_positions(list: &mut [TaskRecord]) {
    let sched = scheduler().lock().unwrap_or_else(|e| e.into_inner());
    for record in list.iter_mut() {
        record.queue_position = if record.status == TaskState::Queued {
            sched
                .queue
                .iter()
                .position(|e| e.task_id == record.id)
                .map(|p| p + 1)
        } else {
            None
        };
    }
}

/// 获取节点可下载产物路径（位于 ServeDir 根内）。
///
/// 收尾归集失败时可惰性补链接；文件不存在返回 None。
pub fn ensure_served_artifact(task_id: &str, node_id: &str) -> Option<PathBuf> {
    // 已有归集路径且文件仍在 → 直接返回
    let existing = registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(task_id)
        .and_then(|r| r.served_artifacts.get(node_id).cloned());
    if let Some(path) = existing {
        if path.is_file() {
            return Some(path);
        }
    }

    // 惰性补归集（收尾阶段链接失败 / 产物后来才可用）
    let (src, task_dir) = {
        let reg = registry().lock().unwrap_or_else(|e| e.into_inner());
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
        .unwrap_or_else(|e| e.into_inner())
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
    /// DAG 校验失败（重复节点 id / 边引用缺失节点 / 缺 file_input 等，P2-11）→ 400
    InvalidPipeline(String),
    /// 直跑：模块不存在 → 404（module_id）
    ModuleNotFound(String),
    /// 直跑：capability 不在模块 manifest → 400（module_id, capability）
    CapabilityNotFound(String, String),
    /// 直跑：输入文件不存在 → 400（路径）
    InputMissing(PathBuf),
    /// 在途任务达到容量上限（[`MAX_INFLIGHT_TASKS`]）→ 429（含上限值）。
    /// P1 修复：提交路径在此拒绝，不建目录、不落盘、不入队。
    QueueFull(usize),
    /// 模块自动拉起失败（§6.5；含模型未就绪/端口分配失败/健康超时）→ 502。
    /// `#[allow(dead_code)]`：运行期拉起失败当前计入任务错误
    /// （start_task → TerminalCause::Engine）；本变体预留给提交期预检
    /// （B4 接线 /api/execute/single 时可按需构造），保持映射面完整。
    #[allow(dead_code)]
    ModuleStartFailed(String),
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
            Self::InvalidPipeline(detail) => {
                write!(f, "pipeline failed DAG validation: {detail}")
            }
            Self::ModuleNotFound(id) => write!(f, "module not found: {id}"),
            Self::CapabilityNotFound(module_id, capability) => {
                write!(f, "module `{module_id}` has no capability `{capability}`")
            }
            Self::InputMissing(path) => {
                write!(f, "input file does not exist: {}", path.display())
            }
            Self::QueueFull(limit) => {
                write!(f, "task queue is full (max {limit} in-flight tasks)")
            }
            Self::ModuleStartFailed(detail) => {
                write!(f, "failed to auto-start module: {detail}")
            }
            Self::Internal(msg) => f.write_str(msg),
        }
    }
}

// ─── 任务 ID 生成 ────────────────────────────────────────────────────────────

static TASK_SEQ: AtomicUsize = AtomicUsize::new(0);

/// 生成任务 ID：`task-{UTC 时间戳}-{进程内序号}`（可读、可排序、进程内唯一）。
///
/// P3：重启后 `TASK_SEQ` 归零，若新任务恰在旧任务创建的同一秒内以同序号
/// 提交，会与重启前加载的历史记录碰撞（[`TaskRegistry::insert`] 同 id 覆盖 =
/// 旧任务数据丢失）。生成后对注册表做存在性检查，碰撞则自增重试。
fn new_task_id() -> String {
    loop {
        let seq = TASK_SEQ.fetch_add(1, Ordering::Relaxed);
        let id = format!("task-{}-{seq:04}", Utc::now().format("%Y%m%d-%H%M%S"));
        if registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .is_none()
        {
            return id;
        }
    }
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

// ─── 提交选项与结果（§6.5） ─────────────────────────────────────────────────

/// 提交选项（§6.5 无人值守三件套之同步模式与完成回调）
#[derive(Debug, Clone, Default)]
pub struct SubmitOptions {
    /// 同步模式：阻塞至终态，[`SubmitOutcome::record`] 携带 status + artifacts
    pub wait: bool,
    /// 完成回调：终态时 POST `{task_id, status, artifacts}`（best-effort）
    pub callback_url: Option<String>,
}

/// 提交结果
#[derive(Debug)]
pub struct SubmitOutcome {
    pub task_id: String,
    /// wait=true 时为终态（或超时上限时刻的）任务快照；wait=false 时为 None。
    /// `#[allow(dead_code)]`：由 B4 接线的 execute handler（§6.5 wait 响应
    /// 组装）消费；本波测试经 execution 层直接验证。
    #[allow(dead_code)]
    pub record: Option<TaskRecord>,
}

// ─── 提交入口 ────────────────────────────────────────────────────────────────

/// 提交管线执行（立即返回 task_id，执行在后台进行）。
///
/// **签名冻结**（Wave S 契约；execute.rs 依赖）。等价于
/// `submit_pipeline_full(…, SubmitOptions::default())`。
///
/// `#[allow(dead_code)]`：非测试路径的调用方已迁移至
/// [`submit_direct_full`] / `submit_pipeline_full`，本函数仅保留给
/// 既有测试与外部签名契约使用（与 submit_direct 同款处理）。
#[allow(dead_code)]
pub async fn submit_pipeline(
    state: &Arc<AppState>,
    pipeline: Pipeline,
    inputs: Option<HashMap<String, Value>>,
) -> Result<String, SubmitError> {
    submit_pipeline_full(state, pipeline, inputs, SubmitOptions::default())
        .await
        .map(|outcome| outcome.task_id)
}

/// 提交管线执行（完整选项版，§6.5）。
///
/// 流程：校验/合并 inputs → DAG 校验（P2-11）→ 注册 `queued` 记录 →
/// 入队 + 立即尝试准入 → wait 模式阻塞至终态。
pub async fn submit_pipeline_full(
    state: &Arc<AppState>,
    mut pipeline: Pipeline,
    inputs: Option<HashMap<String, Value>>,
    options: SubmitOptions,
) -> Result<SubmitOutcome, SubmitError> {
    if let Some(inputs) = inputs.as_ref() {
        apply_inputs(&mut pipeline, inputs)?;
    }

    // P2-11：提交前调用 dag validate（环 + 重复节点 + 孤儿边 + 缺 file_input）。
    // validate 本体在 dag.rs（B7 所有），此处只消费；环单独映射为既有错误变体。
    if let Err(errors) = pipeline.validate() {
        return Err(if errors.iter().any(|e| matches!(e, ValidationError::CycleDetected)) {
            SubmitError::CycleDetected(pipeline.id.clone())
        } else {
            let detail = errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            SubmitError::InvalidPipeline(detail)
        });
    }

    // 持久化兜底绑定（生产路径已在 AppState::new 绑定；此处覆盖测试/直连）
    bind_persistence(&state.root);

    // P1 修复：在途任务容量上限（排队 + 运行中）——超限在**建目录/落盘/入队
    // 之前**直接 429 拒绝，避免提交风暴无限创建任务目录与持久化记录。
    let inflight_full = {
        let sched = scheduler().lock().unwrap_or_else(|e| e.into_inner());
        sched.queue.len() + sched.running_count >= MAX_INFLIGHT_TASKS
    };
    if inflight_full {
        return Err(SubmitError::QueueFull(MAX_INFLIGHT_TASKS));
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

    let pipeline_id = pipeline.id.clone();
    // §6.8：管线级并发上限（TOML `[pipeline] max_instances`，缺省跟随全局）
    let max_instances = resolve_max_instances(&state.root, &pipeline_id);

    // 注册初始记录：queued + 所有节点 pending
    let record = TaskRecord {
        id: task_id.clone(),
        pipeline_id: pipeline_id.clone(),
        status: TaskState::Queued,
        error: None,
        queue_position: None,
        started_at: Utc::now(),
        started_running_at: None,
        finished_at: None,
        node_order: pipeline.nodes.iter().map(|n| n.id.clone()).collect(),
        nodes: pipeline
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
            .collect(),
        artifacts: HashMap::new(),
        served_artifacts: HashMap::new(),
        work_dir: task_dir.clone(),
    };
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(record)
        .map_err(|e| SubmitError::Internal(format!("failed to persist task record: {e}")))?;

    // 入队（FIFO）+ 载荷寄存
    {
        let mut sched = scheduler().lock().unwrap_or_else(|e| e.into_inner());
        sched.pipeline_limits.insert(pipeline_id.clone(), max_instances);
        sched.queue.push_back(QueuedEntry {
            task_id: task_id.clone(),
            pipeline_id: pipeline_id.clone(),
        });
    }
    pending().lock().unwrap_or_else(|e| e.into_inner()).insert(
        task_id.clone(),
        PendingTask {
            pipeline,
            task_dir,
            callback_url: options.callback_url.clone(),
        },
    );

    // 立即尝试准入：闸门有空位时同一 await 内提升 running（快路径）
    try_promote(state.clone()).await;

    if options.wait {
        let record = wait_until_terminal(state, &task_id).await;
        Ok(SubmitOutcome {
            task_id,
            record: Some(record),
        })
    } else {
        Ok(SubmitOutcome {
            task_id,
            record: None,
        })
    }
}

/// §6.8：从 `config/pipelines/*.toml` 中按 `[pipeline].id` 匹配，读取
/// `max_instances`（None = 缺省跟随全局）。
fn resolve_max_instances(root: &Path, pipeline_id: &str) -> Option<u32> {
    let dir = root.join("config").join("pipelines");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return None;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(p) = Pipeline::from_toml_str(&text) {
            if p.id == pipeline_id {
                return vram::parse_max_instances(&text);
            }
        }
    }
    None
}

// ─── 准入调度（两级闸门 + 公平 FIFO） ───────────────────────────────────────

/// 尝试把队列中的任务提升为 running：全局闸门 + 管线闸门均放行才准入。
///
/// 公平性：按 FIFO 顺序取**第一个**可准入的任务（跳过管线闸门已满的，
/// 避免队头阻塞）；循环准入直到无可准入任务或全局闸门满。
async fn try_promote(state: Arc<AppState>) {
    let (max_parallel, timeout_secs) = {
        let cfg = state.config.read().await;
        (
            (cfg.pipeline.max_parallel as usize).max(1),
            cfg.pipeline.default_timeout_secs,
        )
    };

    loop {
        let admitted = {
            let mut sched = scheduler().lock().unwrap_or_else(|e| e.into_inner());
            if sched.running_count >= max_parallel {
                break;
            }
            let pick = sched.queue.iter().position(|entry| {
                let limit = sched
                    .pipeline_limits
                    .get(&entry.pipeline_id)
                    .copied()
                    .flatten()
                    .map(|v| (v as usize).max(1))
                    .unwrap_or(max_parallel);
                let running = sched
                    .running_by_pipeline
                    .get(&entry.pipeline_id)
                    .copied()
                    .unwrap_or(0);
                running < limit
            });
            match pick {
                Some(idx) => {
                    let entry = sched.queue.remove(idx).expect("idx just located");
                    sched.running_count += 1;
                    *sched
                        .running_by_pipeline
                        .entry(entry.pipeline_id.clone())
                        .or_insert(0) += 1;
                    Some(entry)
                }
                None => break,
            }
        };

        let Some(entry) = admitted else { break };
        start_task(state.clone(), entry.task_id, timeout_secs).await;
    }
}

/// 启动已准入任务：extras 注册 → 模块自动拉起（§6.5/P1-2）→ running →
/// 看门狗 → 引擎执行。
///
/// 竞态处理：extras（含终结 CAS）在任何 await 之前注册，排队→运行的窗口内
/// 到达的取消经 [`request_cancel`] 走 extras 路径赢得 CAS；本函数在每个
/// await 点之后检查 CAS，输家不启动引擎、不重复释放闸门。
///
/// 返回类型为显式 boxed future：本函数与 finalize_task → try_promote →
/// start_task 构成递归调用环，async fn 的不透明类型 Send 推断在该环上
/// 不收敛（rustc 已知限制），以 `dyn Future + Send` 显式断言断开推断环。
fn start_task(
    state: Arc<AppState>,
    task_id: String,
    timeout_secs: u32,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(async move {
    // 注意：锁守卫必须先落地为 owned 值再进入 let-else，
    // 否则守卫临时量存活到 else 分支的 await，future 将失去 Send
    let task_opt = pending()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&task_id);
    let Some(task) = task_opt else {
        // 不应发生（队列与载荷同源）：释放闸门，任务判内部错误
        warn!(task_id = %task_id, "admitted task lost its pending payload");
        let pipeline_id = registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&task_id)
            .map(|r| r.pipeline_id.clone())
            .unwrap_or_default();
        finalize_task(
            &state,
            &task_id,
            &pipeline_id,
            TerminalCause::Engine(Some("internal error: pending payload lost".to_string())),
            None,
            None,
        )
        .await;
        return;
    };

    // 运行时标志 + 回调配置：先于任何 await 注册（消除排队→运行窗口的取消竞态）
    let task_extras = TaskExtras {
        cancel: Arc::new(AtomicBool::new(false)),
        callback_url: task.callback_url.clone(),
        // 初始化进度基准时刻；后续由节点回调 bump（缺陷 #3 心跳看门狗）
        last_activity_ms: Arc::new(AtomicU64::new(now_epoch_ms())),
    };
    extras()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(task_id.clone(), task_extras.clone());

    // §6.5 模块自动拉起（修 P1-2 两个消费面）：引用模块未运行 → 拉起并等健康。
    // 失败计入任务错误（闸门随之释放，不阻塞后续任务）。
    if let Err(e) = ensure_pipeline_modules(&state, &task.pipeline).await {
        warn!(task_id = %task_id, error = %e, "module auto-start failed; task fails");
        finalize_task(
            &state,
            &task_id,
            &task.pipeline.id,
            TerminalCause::Engine(Some(e.to_string())),
            None,
            task.callback_url.clone(),
        )
        .await;
        return;
    }
    // 自动拉起期间被取消/终结 → CAS 输家不执行引擎（闸门已由取消方释放）。
    // 检查独立终态表（extras 可能已被赢家移除，但其终结标志仍在案）
    if finalized()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&task_id)
        .is_some_and(|f| f.load(Ordering::SeqCst))
    {
        return;
    }

    // 自动拉起后收集运行中模块端口（含新启动的）
    let module_ports = collect_module_ports(&state).await;

    // queued → running（记录已终态则不覆盖）
    {
        let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(Err(e)) = reg.update(&task_id, |r| {
            if r.status == TaskState::Queued {
                r.status = TaskState::Running;
                r.started_running_at = Some(Utc::now());
            }
            r.queue_position = None;
        }) {
            warn!(task_id = %task_id, error = %e, "failed to persist running transition");
        }
    }
    // 状态回读：转换前被取消（CAS 已输）→ 不执行引擎，闸门已由取消方释放
    {
        let is_running = registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&task_id)
            .map(|r| r.status == TaskState::Running)
            .unwrap_or(false);
        if !is_running {
            extras()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&task_id);
            return;
        }
    }

    // 任务级空闲看门狗（default_timeout_secs；0 = 停用）——缺陷 #3 拆分后为
    // “心跳/进度看门狗”：周期性检查是否仍有节点进度/在飞节点，而非任务总时长
    // 到点判死。长媒体节点（ASR 转写等）在飞期间有心跳，不会被误杀。
    if timeout_secs > 0 {
        let state_w = state.clone();
        let extras_w = task_extras.clone();
        let task_id_w = task_id.clone();
        let pipeline_id_w = task.pipeline.id.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(WATCHDOG_TICK_SECS)).await;
                // 记录缺失（测试清表）→ 无事可做
                let Some(record) = registry()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&task_id_w)
                    .cloned()
                else {
                    return;
                };
                // 已非运行中（引擎/取消已终结）→ 看门狗退场
                if record.status != TaskState::Running {
                    return;
                }
                let last = extras_w.last_activity_ms.load(Ordering::SeqCst);
                if !watchdog_idle_exceeded(&record, last, now_epoch_ms(), timeout_secs) {
                    continue; // 仍有心跳/进度，继续守候
                }
                warn!(task_id = %task_id_w, timeout_secs, "task idle watchdog fired: no node progress/heartbeat; marking failed (cancellation propagated to engine: in-flight calls will be aborted)");
                extras_w.cancel.store(true, Ordering::SeqCst);
                finalize_task(
                    &state_w,
                    &task_id_w,
                    &pipeline_id_w,
                    TerminalCause::Timeout(timeout_secs),
                    None,
                    extras_w.callback_url.clone(),
                )
                .await;
                return;
            }
        });
    }

    // 引擎执行（blocking 线程）
    let state_bg = state.clone();
    let extras_bg = task_extras.clone();
    let pipeline_id = task.pipeline.id.clone();
    // 节点级 wall-clock 硬超时缺省（缺陷 #3 拆分：与任务看门狗解耦）。
    // 解析优先级：节点自身 timeout_secs（runner 内）> 管线 node_timeout_secs
    // > 全局 default_node_timeout_secs > default_timeout_secs（向后兼容回退）。
    let default_node_timeout = {
        let cfg = state.config.read().await;
        task.pipeline.effective_default_node_timeout(&cfg.pipeline)
    };
    tokio::spawn(async move {
        let PendingTask {
            pipeline, task_dir, ..
        } = task;
        let progress_tx = state_bg.progress_tx.clone();
        let task_id_bg = task_id.clone();
        let cancel_flag = extras_bg.cancel.clone();
        let activity_ms = extras_bg.last_activity_ms.clone();
        let joined = tokio::task::spawn_blocking(move || {
            run_task(
                task_id_bg,
                task_dir,
                pipeline,
                module_ports,
                progress_tx,
                cancel_flag,
                activity_ms,
                default_node_timeout,
            )
        })
        .await;

        let (result, detail) = match joined {
            Ok(pair) => pair,
            Err(e) => {
                // 执行线程 panic/被取消——注册表记录不能永远停在 running
                warn!(task_id = %task_id, error = %e, "pipeline execution thread exited abnormally");
                (
                    Err(anyhow::anyhow!("execution thread exited abnormally: {e}")),
                    None,
                )
            }
        };

        // 终态已由看门狗/取消写入时（CAS 输家）：引擎收尾结果被忽略。
        // 缺陷 #5 后引擎通常已因取消传播快速退出；若模块侧推理仍在收尾，
        // worker 短暂占用属预期（见模块文档“行为边界”）。
        if finalize_task(
            &state_bg,
            &task_id,
            &pipeline_id,
            TerminalCause::Engine(result.err().map(|e| e.to_string())),
            detail.as_ref(),
            extras_bg.callback_url.clone(),
        )
        .await
        .is_none()
        {
            info!(task_id = %task_id, "engine finished after task already terminal (watchdog/cancel won the race); engine result ignored");
        }
    });
    })
}

// ─── 终结（唯一终态写入点：CAS 竞争 + 闸门释放 + 回调） ─────────────────────

/// 清理任务工作目录中的中间文件（keep_workspace=false）。
///
/// 产物已归集到 `files/`（硬链接/复制，供 ServeDir 下载），本函数**完全跳过
/// files/**（含其内容），只删除其余中间文件（临时文件、解包目录等）。
fn cleanup_task_workdir(task_dir: &std::path::Path) -> std::io::Result<()> {
    if !task_dir.is_dir() {
        return Ok(());
    }
    let files_dir = task_dir.join("files");
    for entry in std::fs::read_dir(task_dir)? {
        let p = entry?.path();
        if p == files_dir {
            continue;
        }
        if p.is_dir() {
            std::fs::remove_dir_all(&p)?;
        } else {
            std::fs::remove_file(&p)?;
        }
    }
    Ok(())
}

/// 终结原因
enum TerminalCause {
    /// 引擎自然收尾：None = 成功，Some = 失败原因
    Engine(Option<String>),
    /// 任务级超时（§6.5/P0-6）
    Timeout(u32),
    /// 用户取消（P1-11）
    Cancelled,
}

/// 写入终态 + 释放闸门 + 触发完成回调。
///
/// 三方（引擎收尾 / 超时看门狗 / 用户取消）可能并发调用，独立终态表
/// （[`finalized`]）的 CAS 保证唯一赢家；输家直接返回（不重复释放闸门、
/// 不重复回调）。返回实际写入的终态（输家返回 None）。
async fn finalize_task(
    state: &Arc<AppState>,
    task_id: &str,
    pipeline_id: &str,
    cause: TerminalCause,
    detail: Option<&TaskDetail>,
    callback_url: Option<String>,
) -> Option<TaskState> {
    // CAS：唯一终结者——独立终态表（见 [`finalized`] 文档）：
    // extras 在闸门释放前被移除，CAS 标志若仍放在 extras 里，移除后并发
    // finalize 会读 None → 假赢（重复释放闸门/重复回调）。终态表条目与
    // 注册表记录同生命周期，已终态任务不可能再次获胜。
    let won = {
        let mut map = finalized().lock().unwrap_or_else(|e| e.into_inner());
        let flag = map
            .entry(task_id.to_string())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)));
        !flag.swap(true, Ordering::SeqCst)
    };
    if !won {
        return None;
    }

    let terminal = match &cause {
        TerminalCause::Engine(None) => TaskState::Completed,
        TerminalCause::Engine(Some(_)) => TaskState::Failed,
        TerminalCause::Timeout(_) => TaskState::Failed,
        TerminalCause::Cancelled => TaskState::Cancelled,
    };

    // 写终态（含引擎 detail 校正节点状态 + 产物归集）。
    // 记录缺失（测试清表 / 重启丢失）时 update 返回 None——闸门仍需照常释放。
    {
        let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(Err(e)) = reg.update(task_id, |record| {
            if record.status.is_terminal() {
                return; // 已被其他路径终结（如重启加载标记），不覆盖
            }
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
            record.status = terminal;
            record.error = match &cause {
                TerminalCause::Engine(Some(msg)) => Some(msg.clone()),
                TerminalCause::Timeout(secs) => Some(format!(
                    "task timed out after {secs}s (cancelled by watchdog)"
                )),
                _ => None,
            };
            record.finished_at = Some(Utc::now());
            record.queue_position = None;

            // 产物归集：硬链接（跨文件系统退化为复制）到 files/{node_id}/，
            // 使产物都落在 ServeDir 根内可下载
            if matches!(cause, TerminalCause::Engine(_)) {
                let task_dir = record.work_dir.clone();
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
                        record.served_artifacts.insert(node_id.clone(), dest);
                    } else {
                        warn!(task_id, node_id = %node_id, "artifact collection failed; node artifact will not be downloadable");
                    }
                }
            }
        }) {
            warn!(task_id, error = %e, "failed to persist terminal task state");
        }
    }

    // keep_workspace=false → 清理任务工作目录的中间文件。
    // 产物已归集到 files/（硬链接/复制），files/ 保留供产物下载；
    // 其余中间产物（临时文件、解包目录等）一律删除。清理失败仅告警。
    if !{ state.config.read().await.pipeline.keep_workspace } {
        let work_dir = registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(task_id)
            .map(|r| r.work_dir.clone());
        if let Some(dir) = work_dir {
            if let Err(e) = cleanup_task_workdir(&dir) {
                warn!(task_id, dir = %dir.display(), error = %e, "keep_workspace=false cleanup failed");
            }
        }
    }

    // 清理运行时标志
    extras()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(task_id);

    // 释放闸门并触发下一轮准入
    {
        let mut sched = scheduler().lock().unwrap_or_else(|e| e.into_inner());
        sched.running_count = sched.running_count.saturating_sub(1);
        if let Some(n) = sched.running_by_pipeline.get_mut(pipeline_id) {
            *n = n.saturating_sub(1);
        }
    }
    try_promote(state.clone()).await;

    // 完成回调（§6.5，best-effort）
    if let Some(url) = callback_url {
        fire_callback(task_id, &url);
    }

    match terminal {
        TaskState::Completed => info!(task_id, "pipeline task finished"),
        TaskState::Cancelled => info!(task_id, "pipeline task cancelled"),
        TaskState::Failed => {
            let error = snapshot(task_id).and_then(|r| r.error).unwrap_or_default();
            warn!(task_id, error = %error, "pipeline task failed");
        }
        _ => {}
    }
    Some(terminal)
}

// ─── 取消（P1-11：TaskStatus::Cancelled 产生路径） ──────────────────────────

/// 取消结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelOutcome {
    /// 已取消（排队中 = 不再执行；运行中 = 逻辑终态）
    Cancelled,
    /// 任务已是终态，无法取消（附带终态）
    AlreadyTerminal(TaskState),
    /// 任务不存在
    NotFound,
}

/// 请求取消任务（`POST /api/tasks/{id}/cancel`）。
///
/// - 排队中：移出队列，立即 `cancelled`，引擎从不启动；
/// - 运行中：立即 `cancelled`（逻辑终态），并置位协作取消标志传播到
///   引擎（缺陷 #5）：在飞节点被 abort（模块 HTTP 连接断开、子进程终止），
///   引擎线程快速退出；其残余状态写入因记录已终态而被忽略（见模块文档）。
pub async fn request_cancel(state: &Arc<AppState>, task_id: &str) -> CancelOutcome {
    // 1) 排队中 → 出队 + 终态（不占闸门，无需 finalize CAS）
    let was_queued = {
        let mut sched = scheduler().lock().unwrap_or_else(|e| e.into_inner());
        match sched.queue.iter().position(|e| e.task_id == task_id) {
            Some(pos) => {
                sched.queue.remove(pos);
                true
            }
            None => false,
        }
    };
    if was_queued {
        let callback_url = pending()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(task_id)
            .and_then(|t| t.callback_url);
        let persisted = {
            let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
            reg.update(task_id, |r| {
                if !r.status.is_terminal() {
                    r.status = TaskState::Cancelled;
                    r.finished_at = Some(Utc::now());
                }
                r.queue_position = None;
            })
        };
        match persisted {
            Some(Ok(())) => {}
            Some(Err(e)) => warn!(task_id, error = %e, "failed to persist cancelled state"),
            None => return CancelOutcome::NotFound,
        }
        info!(task_id, "queued task cancelled");
        if let Some(url) = callback_url {
            fire_callback(task_id, &url);
        }
        return CancelOutcome::Cancelled;
    }

    // 2) 运行中 → 协作取消 + CAS 终结
    let task_extras = extras()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(task_id)
        .cloned();
    let Some(task_extras) = task_extras else {
        return match registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(task_id)
        {
            None => CancelOutcome::NotFound,
            Some(r) if r.status.is_terminal() => CancelOutcome::AlreadyTerminal(r.status),
            // 有记录但无 extras（重启加载的残留记录已是终态，上方已覆盖；
            // 防御性兜底按未找到处理）
            Some(_) => CancelOutcome::NotFound,
        };
    };

    task_extras.cancel.store(true, Ordering::SeqCst);
    let pipeline_id = registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(task_id)
        .map(|r| r.pipeline_id.clone());
    let Some(pipeline_id) = pipeline_id else {
        return CancelOutcome::NotFound;
    };

    match finalize_task(
        state,
        task_id,
        &pipeline_id,
        TerminalCause::Cancelled,
        None,
        task_extras.callback_url.clone(),
    )
    .await
    {
        Some(terminal) => {
            if terminal == TaskState::Cancelled {
                CancelOutcome::Cancelled
            } else {
                CancelOutcome::AlreadyTerminal(terminal)
            }
        }
        // CAS 输给超时看门狗等 → 以记录实际终态为准
        None => match snapshot(task_id) {
            Some(r) if r.status.is_terminal() => CancelOutcome::AlreadyTerminal(r.status),
            Some(_) => CancelOutcome::NotFound,
            None => CancelOutcome::NotFound,
        },
    }
}

// ─── wait 同步模式（§6.5） ───────────────────────────────────────────────────

/// 阻塞至任务终态（内部超时上限 = 管线超时配置 + 看门狗落位缓冲）。
///
/// 超时上限到达仍未终态时返回当前快照（看门狗正常情况会在上限前落终态；
/// `default_timeout_secs = 0` 表示停用看门狗，此时无上限、阻塞至终态）。
async fn wait_until_terminal(state: &Arc<AppState>, task_id: &str) -> TaskRecord {
    let timeout_secs = state.config.read().await.pipeline.default_timeout_secs;
    let deadline = if timeout_secs > 0 {
        Some(std::time::Instant::now() + Duration::from_secs(timeout_secs as u64 + 30))
    } else {
        None
    };
    loop {
        if let Some(record) = snapshot(task_id) {
            if record.status.is_terminal() {
                return record;
            }
        }
        if let Some(dl) = deadline {
            if std::time::Instant::now() >= dl {
                // 看门狗应已落终态；兜底返回当前快照（status 仍为运行态）
                warn!(task_id, "wait mode reached timeout upper bound before terminal state");
                return snapshot(task_id).unwrap_or_else(|| TaskRecord {
                    id: task_id.to_string(),
                    pipeline_id: String::new(),
                    status: TaskState::Running,
                    error: None,
                    queue_position: None,
                    started_at: Utc::now(),
                    started_running_at: None,
                    finished_at: None,
                    node_order: Vec::new(),
                    nodes: HashMap::new(),
                    artifacts: HashMap::new(),
                    served_artifacts: HashMap::new(),
                    work_dir: PathBuf::new(),
                });
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ─── 完成回调（§6.5，best-effort） ──────────────────────────────────────────

/// 终态时 POST `{task_id, status, artifacts}` 到 callback_url；失败仅 warn。
fn fire_callback(task_id: &str, url: &str) {
    let Some(record) = registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(task_id)
        .cloned()
    else {
        return;
    };
    let artifacts: Vec<Value> = record
        .node_order
        .iter()
        .filter_map(|node_id| {
            let path = record.artifacts.get(node_id)?;
            let meta = std::fs::metadata(path).ok()?;
            Some(json!({
                "node_id": node_id,
                "name": path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                "size": meta.len(),
            }))
        })
        .collect();
    let body = json!({
        "task_id": task_id,
        "status": record.status.as_str(),
        "artifacts": artifacts,
    });
    let url = url.to_string();
    let task_id = task_id.to_string();
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        match client
            .post(&url)
            .json(&body)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                info!(task_id = %task_id, url = %url, "completion callback delivered");
            }
            Ok(resp) => {
                warn!(task_id = %task_id, url = %url, status = %resp.status(), "completion callback rejected by endpoint (best-effort)");
            }
            Err(e) => {
                warn!(task_id = %task_id, url = %url, error = %e, "completion callback failed (best-effort)");
            }
        }
    });
}

// ─── 模块自动拉起（§6.5 公共件；接口契约供 B4 autostart.rs 对齐） ───────────

/// 确保管线引用的全部 module 节点都在运行（未运行则拉起并等健康）。
///
/// §6.5「execute/single 提交时，引用模块未运行 → 自动启动并等健康
/// （超时计入任务错误）」——在闸门准入后、引擎执行前执行，修 P1-2
/// （管线侧 + 直跑侧两个消费面）。
async fn ensure_pipeline_modules(state: &Arc<AppState>, pipeline: &Pipeline) -> anyhow::Result<()> {
    let mut module_ids: Vec<&str> = Vec::new();
    for node in &pipeline.nodes {
        if let NodeKind::Module { module_id, .. } = &node.kind {
            if !module_ids.contains(&module_id.as_str()) {
                module_ids.push(module_id.as_str());
            }
        }
    }
    for module_id in module_ids {
        ensure_module_running(state, module_id)
            .await
            .map_err(|e| anyhow::anyhow!("module `{module_id}`: {e}"))?;
    }
    Ok(())
}

/// 确保模块运行中：已运行直接返回；未运行则启动并等待健康检查通过。
///
/// **门禁 #25 归一**：委托 `api/autostart.rs` 权威实现（B4 公共件，含模型就绪
/// 前置检查、失败清理 stop_module+释放端口、并发竞态转等健康）。管线侧
/// （ensure_pipeline_modules）与直跑侧（execute/single）共用同一拉起逻辑。
pub async fn ensure_module_running(state: &Arc<AppState>, module_id: &str) -> anyhow::Result<()> {
    crate::api::autostart::ensure_module_running(state, module_id)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}

async fn collect_module_ports(state: &Arc<AppState>) -> HashMap<String, u16> {
    let pm = state.process_manager.read().await;
    pm.list_running()
        .iter()
        .filter_map(|inst| inst.port.map(|port| (inst.module_id.clone(), port)))
        .collect()
}

// ─── 单模型直跑（§5.3 / §8.1 `POST /api/execute/single`） ───────────────────

/// 提交单模型直跑：校验模块 + capability → 编译退化三节点 DAG
/// （file_input → module → file_output）→ 走同一闸门提交。
///
/// **B4 接口契约（Wave 2 已对齐）**：`/api/execute/single` handler 在调用
/// 本函数**之前**已经 `ensure_module_running`（模块 Running、端口已注册），
/// 故本函数内**不做** autostart。闸门准入后 [`start_task`] 的
/// [`ensure_pipeline_modules`] 仍会经过幂等快速路径（已 Running 立即返回），
/// 作为排队期间模块退出的安全网，不重复拉起。
///
/// 直跑任务的 `pipeline_id` 采用 `direct/<module_id>`（B4 建议形状，供前端
/// 任务列表过滤；与配置管线 id 命名空间天然隔离）。
///
/// **Wave S 冻结签名**；API 接线（`POST /api/execute/single`）由 B4 在
/// `api/execute.rs` 完成。等价于
/// `submit_direct_full(…, SubmitOptions::default())`（仅返回 task_id）。
#[allow(dead_code)] // B4 接线 /api/execute/single 前的骨架保留，勿删
pub async fn submit_direct(
    state: &Arc<AppState>,
    module_id: &str,
    capability: &str,
    params: Value,
    input_path: PathBuf,
) -> Result<String, SubmitError> {
    submit_direct_full(
        state,
        module_id,
        capability,
        params,
        input_path,
        SubmitOptions::default(),
    )
    .await
    .map(|outcome| outcome.task_id)
}

/// 提交单模型直跑（完整选项版，镜像 [`submit_pipeline_full`] 的
/// wait/callback 语义）：校验模块 + capability → 编译退化三节点 DAG →
/// 走同一闸门提交；`wait=true` 时阻塞至终态并在
/// [`SubmitOutcome::record`] 携带终态快照。
///
/// 其余语义（autostart 前置约定、`direct/<module_id>` 命名空间、
/// 排队期安全网）与 [`submit_direct`] 完全一致。
pub async fn submit_direct_full(
    state: &Arc<AppState>,
    module_id: &str,
    capability: &str,
    params: Value,
    input_path: PathBuf,
    options: SubmitOptions,
) -> Result<SubmitOutcome, SubmitError> {
    // 1. 模块 + manifest capability 校验（快速失败，不占闸门）；
    //    同时取回能力声明供产物扩展名推导（F2：跨格式能力不再误标）
    let capability_decl = {
        let modules = state.modules.read().await;
        let module = modules
            .iter()
            .find(|m| {
                m.manifest
                    .as_ref()
                    .map(|mf| mf.module.id == module_id)
                    .unwrap_or(false)
            })
            .ok_or_else(|| SubmitError::ModuleNotFound(module_id.to_string()))?;
        let manifest = module.manifest.as_ref().expect("matched by manifest presence");
        manifest
            .interface
            .capabilities
            .iter()
            .find(|c| c.name == capability)
            .cloned()
    };
    let Some(capability_decl) = capability_decl else {
        return Err(SubmitError::CapabilityNotFound(
            module_id.to_string(),
            capability.to_string(),
        ));
    };

    // 2. 输入文件存在性
    if !input_path.is_file() {
        return Err(SubmitError::InputMissing(input_path));
    }

    // 3. 退化三节点 DAG → 同一提交路径（闸门/注册表/WS/产物全套复用）
    let pipeline = build_direct_pipeline(
        module_id,
        capability,
        params,
        &input_path,
        Some(&capability_decl),
    );
    submit_pipeline_full(state, pipeline, None, options).await
}

/// 编译直跑退化 DAG：`input(file_input) → run(module) → output(file_output)`。
///
/// 输出节点不带 `path` 参数 → 引擎按 `extension` 参数派生
/// `{work_dir}/output_output.<ext>`，随产物归集进入任务目录可下载
///（§5.3 结果预览/下载）。`extension` 经 ep-core 公共推导源
/// [`direct_output_extension`] 三级优先级得出（F2 修复：① 请求
/// `output_format` → ② capability 跨格式语义映射 → ③ 输入扩展名回退，
/// 与桌面端 build_direct_pipeline 同口径）。
#[allow(dead_code)] // 经 submit_direct 消费（B4 接线前测试直接调用）
pub fn build_direct_pipeline(
    module_id: &str,
    capability: &str,
    params: Value,
    input_path: &Path,
    capability_decl: Option<&CapabilityDecl>,
) -> Pipeline {
    let make_node = |id: &str, kind: NodeKind, label: &str, params: Value| PipelineNode {
        id: id.to_string(),
        kind,
        label: label.to_string(),
        params,
        position: None,
        timeout_secs: None,
        retry_count: None,
    };
    // 输出扩展名：三级优先级推导（含与 executor file_output 同口径的字符过滤）
    let output_params = direct_output_extension(&params, capability_decl, input_path)
        .map(|ext| json!({ "extension": ext }))
        .unwrap_or_else(|| json!({}));
    Pipeline {
        // B4 契约：direct/<module_id> 形状（同模块直跑任务聚合，前端可过滤）
        id: format!("direct/{module_id}"),
        name: format!("直跑 {module_id}/{capability}"),
        description: "单模型直跑任务（§5.3 退化三节点 DAG）".to_string(),
        nodes: vec![
            make_node(
                "input",
                NodeKind::Builtin {
                    builtin: "file_input".to_string(),
                },
                "输入文件",
                json!({ "path": input_path.display().to_string() }),
            ),
            make_node(
                "run",
                NodeKind::Module {
                    module_id: module_id.to_string(),
                    capability: capability.to_string(),
                    model_id: None,
                    device: None,
                },
                "模块执行",
                params,
            ),
            make_node(
                "output",
                NodeKind::Builtin {
                    builtin: "file_output".to_string(),
                },
                "结果输出",
                output_params,
            ),
        ],
        edges: vec![
            ep_core::pipeline::dag::Edge {
                from: ("input".to_string(), "output".to_string()),
                to: ("run".to_string(), "input".to_string()),
            },
            ep_core::pipeline::dag::Edge {
                from: ("run".to_string(), "output".to_string()),
                to: ("output".to_string(), "input".to_string()),
            },
        ],
        max_instances: None,
        node_timeout_secs: None,
    }
}

// ─── 后台执行（spawn_blocking 线程内） ───────────────────────────────────────

/// 在独立 `PipelineRunnerImpl` 上同步执行管线，返回引擎结果与任务详情
/// （终态写入统一由 [`finalize_task`] 完成）。
///
/// - `cancel_flag`：协作取消标志（与 [`TaskExtras::cancel`] 共享），
///   runner 在节点边界与在飞期间两类检查点响应：在飞节点的执行任务被
///   abort（模块 HTTP 请求连接断开、ffmpeg 子进程终止）→ 引擎产生
///   `TaskStatus::Cancelled`（P0-6；缺陷 #5 在飞中断）；
/// - `activity_ms`：节点进度时刻（缺陷 #3 心跳看门狗）；节点开始/完成/失败
///   回调均 bump，任务级空闲看门狗据此判定“无心跳”而非任务总时长；
/// - `default_node_timeout`：节点级 wall-clock 硬超时缺省值（缺陷 #3 拆分，
///   节点自身 `timeout_secs` 优先；B7 的执行器客户端级超时互补）。
#[allow(clippy::too_many_arguments)]
fn run_task(
    task_id: String,
    task_dir: PathBuf,
    pipeline: Pipeline,
    module_ports: HashMap<String, u16>,
    progress_tx: broadcast::Sender<ProgressMessage>,
    cancel_flag: Arc<AtomicBool>,
    activity_ms: Arc<AtomicU64>,
    default_node_timeout: Option<Duration>,
) -> (anyhow::Result<()>, Option<TaskDetail>) {
    #[cfg(test)]
    run_test_hook(&task_id);

    let pipeline_id = pipeline.id.clone();

    let mut runner = PipelineRunnerImpl::new(task_dir.clone());
    runner.set_module_ports(module_ports);
    runner.set_cancel_flag(cancel_flag);
    runner.set_default_node_timeout(default_node_timeout);

    // 回调：节点开始 → running（P2-7：携带 task_id）
    {
        let tx = progress_tx.clone();
        let pid = pipeline_id.clone();
        let tid = task_id.clone();
        let act = activity_ms.clone();
        runner.on_node_start = Some(Arc::new(move |node_id| {
            act.store(now_epoch_ms(), Ordering::SeqCst);
            let _ = tx.send(ProgressMessage {
                pipeline_id: pid.clone(),
                task_id: tid.clone(),
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
        let act = activity_ms.clone();
        runner.on_node_complete = Some(Arc::new(move |node_id, artifact| {
            act.store(now_epoch_ms(), Ordering::SeqCst);
            let _ = tx.send(ProgressMessage {
                pipeline_id: pid.clone(),
                task_id: tid.clone(),
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
        let act = activity_ms.clone();
        runner.on_node_error = Some(Arc::new(move |node_id, error| {
            act.store(now_epoch_ms(), Ordering::SeqCst);
            let _ = tx.send(ProgressMessage {
                pipeline_id: pid.clone(),
                task_id: tid.clone(),
                node_id: node_id.to_string(),
                status: "failed".to_string(),
            });
            set_node_state(&tid, node_id, "failed", Some(error.to_string()));
        }));
    }

    // 引擎同步执行（内部自建运行时；整个调用阻塞本 blocking 线程）
    let result = PipelineRunner::execute(&mut runner, &pipeline, &task_dir);

    // 引擎自身任务详情（权威节点终态，含回调不覆盖的 skipped 节点）
    let detail = runner
        .list_tasks()
        .pop()
        .and_then(|summary| runner.get_task_detail(&summary.id));
    (result, detail)
}

// ─── 注册表内部操作（记录缺失时一律 no-op，避免测试清表后被后台回调复活） ────

fn set_node_state(task_id: &str, node_id: &str, state: &str, error: Option<String>) {
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
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
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(record) = reg.get_mut(task_id) {
        record
            .artifacts
            .insert(node_id.to_string(), path.to_path_buf());
    }
}

// ─── 测试专用 ────────────────────────────────────────────────────────────────

/// 串行化所有触碰进程级静态（注册表/调度器）的测试
#[cfg(test)]
pub static TEST_LOCK: Mutex<()> = Mutex::new(());

/// 获取测试锁（中毒后自动恢复，避免单个测试失败级联拖垮其余测试）
#[cfg(test)]
pub fn lock_for_tests() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 重置全部进程级状态（注册表 + 调度器 + 待执行载荷 + 运行时标志）。
/// 测试之间必须调用，避免计数残留串染。
#[cfg(test)]
pub fn clear_registry_for_tests() {
    *registry().lock().unwrap_or_else(|e| e.into_inner()) = TaskRegistry::new();
    *scheduler().lock().unwrap_or_else(|e| e.into_inner()) = Scheduler::default();
    pending().lock().unwrap_or_else(|e| e.into_inner()).clear();
    extras().lock().unwrap_or_else(|e| e.into_inner()).clear();
    finalized().lock().unwrap_or_else(|e| e.into_inner()).clear();
    set_test_run_hook(None);
}

/// 重置 + 原子绑定持久化目录（持注册表锁完成，杜绝并发 AppState::new
/// 在 clear 与 bind 之间拐走落盘点的竞态）。持久化敏感测试专用。
#[cfg(test)]
pub fn reset_and_bind_for_tests(root: &Path) {
    clear_registry_for_tests();
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    let dir = root.join("runtime").join("tasks");
    let _ = reg.enable_persistence(dir);
}

/// 测试钩子：任务在 blocking 线程开始执行引擎前调用（可阻塞以占用闸门）。
#[cfg(test)]
type TestRunHook = Arc<dyn Fn(&str) + Send + Sync>;

#[cfg(test)]
static TEST_RUN_HOOK: OnceLock<Mutex<Option<TestRunHook>>> = OnceLock::new();

#[cfg(test)]
fn set_test_run_hook(hook: Option<TestRunHook>) {
    *TEST_RUN_HOOK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = hook;
}

#[cfg(test)]
fn run_test_hook(task_id: &str) {
    let hook = TEST_RUN_HOOK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some(hook) = hook {
        hook(task_id);
    }
}

/// 跨模块测试（pipelines.rs）用的持闸钩子：阻塞执行直至
/// [`release_test_run_hook_for_pipelines_test`] 被调用（用于制造排队场景）。
#[cfg(test)]
static TEST_HOLD_FLAG: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
pub fn set_test_run_hook_for_pipelines_test() {
    TEST_HOLD_FLAG.store(false, Ordering::SeqCst);
    set_test_run_hook(Some(Arc::new(|_task_id| {
        while !TEST_HOLD_FLAG.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(20));
        }
    })));
}

#[cfg(test)]
pub fn release_test_run_hook_for_pipelines_test() {
    TEST_HOLD_FLAG.store(true, Ordering::SeqCst);
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
    use ep_core::types::DataType;

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

    // ── cleanup_task_workdir：keep_workspace=false 中间文件清理 ──────────────

    #[test]
    fn cleanup_removes_intermediates_keeps_files_dir() {
        let root = unique_root("cleanup");
        let task_dir = root.join("tasks").join("t1");
        std::fs::create_dir_all(task_dir.join("files").join("node1")).unwrap();
        std::fs::write(task_dir.join("files").join("node1").join("out.txt"), "out").unwrap();
        std::fs::write(task_dir.join("staging.bin"), "staging").unwrap();
        std::fs::create_dir_all(task_dir.join("tmp")).unwrap();
        std::fs::write(task_dir.join("tmp").join("mid.wav"), "mid").unwrap();

        cleanup_task_workdir(&task_dir).unwrap();

        // 中间文件与临时目录被清
        assert!(!task_dir.join("staging.bin").exists());
        assert!(!task_dir.join("tmp").exists());
        // files/ 及其产物保留
        assert!(task_dir.join("files").join("node1").join("out.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cleanup_removes_empty_files_dir_and_missing_dir_is_noop() {
        let root = unique_root("cleanup2");
        let task_dir = root.join("tasks").join("t2");
        std::fs::create_dir_all(&task_dir).unwrap();

        cleanup_task_workdir(&task_dir).unwrap();
        // files/ 不存在 → 任务目录本身被清空（目录仍在，由调用方决定去留）
        assert!(task_dir.exists());

        // 不存在目录 → noop 不报错
        cleanup_task_workdir(&root.join("ghost")).unwrap();
        let _ = std::fs::remove_dir_all(&root);
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

    /// file_input → file_output 复制管线（真实可执行）
    fn copy_pipeline(id: &str, src: &Path, dest: &Path) -> Pipeline {
        let toml = format!(
            r#"
[pipeline]
id = "{id}"
name = "复制测试"

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
            toml_path(src),
            toml_path(dest),
        );
        Pipeline::from_toml_str(&toml).unwrap()
    }

    /// 轮询等待任务终结（终态判定：completed/failed/cancelled）
    async fn wait_terminal(task_id: &str) -> Option<TaskRecord> {
        for _ in 0..600 {
            if let Some(record) = snapshot(task_id) {
                if record.status.is_terminal() {
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

        let root = unique_root("ok");
        // 本测试断言持久化落盘，属持久化敏感：原子重置 + 绑定，防止并发
        // AppState::new 在 clear 与 bind 之间拐走落盘点（同测试 12 做法）
        reset_and_bind_for_tests(&root);
        let state = test_state(root.clone());

        let src = root.join("source.txt");
        let dest = root.join("delivered.txt");
        std::fs::write(&src, "引擎直调测试内容").unwrap();

        let pipeline = copy_pipeline("direct-run", &src, &dest);
        let task_id = submit_pipeline(&state, pipeline, None).await.unwrap();
        let record = wait_terminal(&task_id)
            .await
            .expect("任务应在超时前终结");

        assert_eq!(record.status, TaskState::Completed);
        assert!(record.finished_at.is_some());
        assert!(record.started_running_at.is_some(), "应记录实际开始时间");
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
        // 持久化落盘（runtime/tasks/{id}.json，P1-4）
        assert!(root.join("runtime/tasks").join(format!("{task_id}.json")).exists());
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

    // ── 4b. 在途任务达容量上限 → QueueFull（P1：队列深度上限） ──────────────

    #[tokio::test]
    async fn test_submit_queue_full_rejected() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("qfull");
        let state = test_state(root.clone());

        // 直接填满调度器计数（模拟 MAX_INFLIGHT_TASKS 个在途任务）
        {
            let mut sched = scheduler().lock().unwrap_or_else(|e| e.into_inner());
            sched.running_count = MAX_INFLIGHT_TASKS;
        }
        // 提交必须在建目录/落盘/入队之前被拒绝（422/429 语义层由 handler 映射）
        let err = submit_pipeline(&state, minimal_pipeline("qfull"), None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, SubmitError::QueueFull(limit) if limit == MAX_INFLIGHT_TASKS),
            "期望 QueueFull({MAX_INFLIGHT_TASKS})，实际 {err}"
        );
        // 未入队、未产生任务记录
        assert_eq!(snapshot_all().len(), 0);
        assert_eq!(
            scheduler()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .queue
                .len(),
            0
        );
        // 容量减 1 后提交恢复正常（边界：len+running == MAX-1 放行）
        {
            let mut sched = scheduler().lock().unwrap_or_else(|e| e.into_inner());
            sched.running_count = MAX_INFLIGHT_TASKS - 1;
        }
        assert!(
            submit_pipeline(&state, minimal_pipeline("qfull-ok"), None)
                .await
                .is_ok(),
            "容量边界内提交应放行"
        );
    }

    // ── 4c. 任务 ID 碰撞（P3：重启后 TASK_SEQ 归零可能与历史记录同秒同序号） ──

    #[test]
    fn test_new_task_id_skips_registry_collision() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        // 预置一条 id 与"当前秒 + 下一序号"完全一致的旧记录（模拟重启后碰撞）
        let now = Utc::now();
        let seq = TASK_SEQ.load(Ordering::Relaxed);
        let colliding = format!("task-{}-{seq:04}", now.format("%Y%m%d-%H%M%S"));
        let record = TaskRecord {
            id: colliding.clone(),
            pipeline_id: "old".to_string(),
            status: TaskState::Completed,
            error: None,
            queue_position: None,
            started_at: now,
            started_running_at: None,
            finished_at: Some(now),
            node_order: Vec::new(),
            nodes: HashMap::new(),
            artifacts: HashMap::new(),
            served_artifacts: HashMap::new(),
            work_dir: PathBuf::new(),
        };
        registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(record)
            .expect("seed 旧记录");

        let id = new_task_id();
        assert_ne!(
            id, colliding,
            "与注册表现有 id 碰撞时必须自增跳过（否则 insert 覆盖旧任务数据）"
        );
        assert!(
            registry()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&id)
                .is_none(),
            "返回的 id 必须不在注册表中"
        );
    }

    // ── 5. 失败管线：failed + 节点 failed/skipped ──────────────────────────

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

        assert_eq!(record.status, TaskState::Failed);
        assert!(record.error.is_some());
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

    // ── 6b. P2-11：提交路径调用 dag validate（缺 file_input → 400 类错误） ──

    #[tokio::test]
    async fn test_submit_validate_rejects_missing_file_input() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("noinput");
        let state = test_state(root);
        let toml = r#"
[pipeline]
id = "no-file-input"
name = "缺 file_input"

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
"#;
        let pipeline = Pipeline::from_toml_str(toml).unwrap();
        let err = submit_pipeline(&state, pipeline, None)
            .await
            .unwrap_err();
        assert!(matches!(err, SubmitError::InvalidPipeline(_)));
        assert!(err.to_string().contains("file_input"));
    }

    // ── 7. 进度回调 → progress_tx（WS 链路数据源，P2-7：携带 task_id） ──────

    #[tokio::test]
    async fn test_progress_messages_carry_task_id() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("ws");
        let state = test_state(root.clone());
        let mut rx = state.progress_tx.subscribe();

        let src = root.join("ws-src.txt");
        let dest = root.join("ws-out.txt");
        std::fs::write(&src, "ws").unwrap();
        let pipeline = copy_pipeline("ws-test", &src, &dest);
        let task_id = submit_pipeline(&state, pipeline, None).await.unwrap();
        wait_terminal(&task_id).await.expect("任务应终结");

        // 收集广播中的进度消息（两节点 × start/complete = 4 条）
        let mut got: Vec<ProgressMessage> = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            got.push(msg);
        }
        assert!(got.len() >= 4, "至少 4 条进度消息，got {}", got.len());
        assert!(got.iter().all(|m| m.pipeline_id == "ws-test"));
        // P2-7：全部消息携带真实 task_id（并发串染修复）
        assert!(got.iter().all(|m| m.task_id == task_id));
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
        let pipeline = copy_pipeline("lazy-test", &src, &dest);
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
        let _ = std::fs::remove_dir_all(snapshot(&task_id).unwrap().work_dir.join("files"));

        let served = ensure_served_artifact(&task_id, "output").expect("应惰性归集成功");
        assert!(served.is_file());
        assert_eq!(std::fs::read_to_string(&served).unwrap(), "lazy");
        // 未知节点 → None
        assert!(ensure_served_artifact(&task_id, "ghost").is_none());
        // 未知任务 → None
        assert!(ensure_served_artifact("task-ghost", "output").is_none());
    }

    // ── 9. 全局闸门并发上限（P1-3：spawn N 断言同时运行 ≤ max） ─────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_global_gate_limits_concurrency() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("gate");
        let mut config = AppConfig::default();
        config.pipeline.max_parallel = 2;
        config.pipeline.default_timeout_secs = 120;
        let state = Arc::new(AppState::new(
            root.clone(),
            config,
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        ));

        // 并发观测：钩子在 blocking 线程内计数 + 停留 300ms 制造重叠窗口
        static CONCURRENT: AtomicUsize = AtomicUsize::new(0);
        static MAX_CONCURRENT: AtomicUsize = AtomicUsize::new(0);
        CONCURRENT.store(0, Ordering::SeqCst);
        MAX_CONCURRENT.store(0, Ordering::SeqCst);
        set_test_run_hook(Some(Arc::new(|_task_id| {
            let now = CONCURRENT.fetch_add(1, Ordering::SeqCst) + 1;
            MAX_CONCURRENT.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(300));
            CONCURRENT.fetch_sub(1, Ordering::SeqCst);
        })));

        // 提交 5 个任务 → 全局上限 2，同时运行数必须 ≤ 2 且确实达到 2
        let mut task_ids = Vec::new();
        for i in 0..5 {
            let src = root.join(format!("gate-src-{i}.txt"));
            let dest = root.join(format!("gate-out-{i}.txt"));
            std::fs::write(&src, format!("gate {i}")).unwrap();
            let pipeline = copy_pipeline(&format!("gate-{i}"), &src, &dest);
            task_ids.push(submit_pipeline(&state, pipeline, None).await.unwrap());
        }
        for id in &task_ids {
            wait_terminal(id).await.expect("任务应终结");
        }
        assert!(
            MAX_CONCURRENT.load(Ordering::SeqCst) <= 2,
            "全局闸门失效：同时运行数超过 max_parallel=2"
        );
        assert_eq!(
            MAX_CONCURRENT.load(Ordering::SeqCst),
            2,
            "5 个任务上限 2 时应达到满并发"
        );
        set_test_run_hook(None);
    }

    // ── 10. queued 状态与队列位置 + FIFO 顺序 ───────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_queued_state_and_positions() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("queued");
        let mut config = AppConfig::default();
        config.pipeline.max_parallel = 1;
        config.pipeline.default_timeout_secs = 120;
        let state = Arc::new(AppState::new(
            root.clone(),
            config,
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        ));

        // 钩子：第一个任务阻塞直到 RELEASE 置位，其余任务排队可见
        static RELEASE: AtomicBool = AtomicBool::new(false);
        RELEASE.store(false, Ordering::SeqCst);
        static HOOK_COUNT: AtomicUsize = AtomicUsize::new(0);
        HOOK_COUNT.store(0, Ordering::SeqCst);
        set_test_run_hook(Some(Arc::new(|_task_id| {
            let n = HOOK_COUNT.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // 首个任务持闸，直到测试释放
                while !RELEASE.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        })));

        let mut task_ids = Vec::new();
        for i in 0..3 {
            let src = root.join(format!("q-src-{i}.txt"));
            let dest = root.join(format!("q-out-{i}.txt"));
            std::fs::write(&src, format!("q {i}")).unwrap();
            let pipeline = copy_pipeline(&format!("q-{i}"), &src, &dest);
            task_ids.push(submit_pipeline(&state, pipeline, None).await.unwrap());
        }

        // 等首个任务进入 running（钩子内阻塞），其余应为 queued + 位置 1、2
        tokio::time::sleep(Duration::from_millis(300)).await;
        let r0 = snapshot(&task_ids[0]).unwrap();
        assert_eq!(r0.status, TaskState::Running);
        let r1 = snapshot(&task_ids[1]).unwrap();
        let r2 = snapshot(&task_ids[2]).unwrap();
        assert_eq!(r1.status, TaskState::Queued);
        assert_eq!(r1.queue_position, Some(1));
        assert_eq!(r2.status, TaskState::Queued);
        assert_eq!(r2.queue_position, Some(2));

        // 释放 → 全部按 FIFO 完成
        RELEASE.store(true, Ordering::SeqCst);
        for id in &task_ids {
            let record = wait_terminal(id).await.expect("任务应终结");
            assert_eq!(record.status, TaskState::Completed);
            assert!(record.queue_position.is_none(), "终态不携带队列位置");
        }
        set_test_run_hook(None);
    }

    // ── 11. 管线级 max_instances（§6.8） ────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_pipeline_max_instances_gate() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("maxinst");
        let mut config = AppConfig::default();
        config.pipeline.max_parallel = 4; // 全局宽松
        config.pipeline.default_timeout_secs = 120;
        let state = Arc::new(AppState::new(
            root.clone(),
            config,
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        ));

        // 管线 TOML 声明 max_instances = 1（GPU 重管线锁 1）
        let dir = root.join("config").join("pipelines");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("locked.toml"),
            "[pipeline]\nid = \"locked-pipe\"\nname = \"锁 1 管线\"\nmax_instances = 1\n",
        )
        .unwrap();

        // 每管线并发观测（钩子内按 pipeline_id 计数）
        static PER_PIPE: AtomicUsize = AtomicUsize::new(0);
        static PER_PIPE_MAX: AtomicUsize = AtomicUsize::new(0);
        PER_PIPE.store(0, Ordering::SeqCst);
        PER_PIPE_MAX.store(0, Ordering::SeqCst);
        set_test_run_hook(Some(Arc::new(|task_id| {
            if let Some(r) = snapshot(task_id) {
                if r.pipeline_id == "locked-pipe" {
                    let now = PER_PIPE.fetch_add(1, Ordering::SeqCst) + 1;
                    PER_PIPE_MAX.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(200));
                    PER_PIPE.fetch_sub(1, Ordering::SeqCst);
                }
            }
        })));

        // 同一管线 3 个任务 → max_instances=1 串行
        let mut task_ids = Vec::new();
        for i in 0..3 {
            let src = root.join(format!("mi-src-{i}.txt"));
            let dest = root.join(format!("mi-out-{i}.txt"));
            std::fs::write(&src, format!("mi {i}")).unwrap();
            let pipeline = copy_pipeline("locked-pipe", &src, &dest);
            task_ids.push(submit_pipeline(&state, pipeline, None).await.unwrap());
        }
        for id in &task_ids {
            wait_terminal(id).await.expect("任务应终结");
        }
        assert_eq!(
            PER_PIPE_MAX.load(Ordering::SeqCst),
            1,
            "max_instances=1 的管线不允许并发执行"
        );
        set_test_run_hook(None);
    }

    // ── 12. 持久化往返：daemon 重启（新注册表 load）不丢索引（P1-4） ────────

    #[tokio::test]
    async fn test_registry_persistence_across_restart() {
        let _guard = lock_for_tests();

        let root = unique_root("persist");
        // 原子重置 + 绑定，防止并发 AppState::new 拐走落盘点
        reset_and_bind_for_tests(&root);
        let state = test_state(root.clone());
        let src = root.join("p-src.txt");
        let dest = root.join("p-out.txt");
        std::fs::write(&src, "persist").unwrap();
        let pipeline = copy_pipeline("persist-pipe", &src, &dest);
        let task_id = submit_pipeline(&state, pipeline, None).await.unwrap();
        wait_terminal(&task_id).await.expect("任务应终结");

        // 模拟 daemon 重启：全新注册表从 runtime/tasks/ 回读
        let restored = TaskRegistry::load(root.join("runtime").join("tasks"));
        let record = restored.get(&task_id).expect("重启后索引应恢复");
        assert_eq!(record.pipeline_id, "persist-pipe");
        assert_eq!(record.status, TaskState::Completed);
        assert_eq!(record.artifacts.len(), 2);
        assert_eq!(record.work_dir, snapshot(&task_id).unwrap().work_dir);
    }

    // ── 13. 取消排队中任务（P1-11：Cancelled 产生路径） ─────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_cancel_queued_task() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("cancel-q");
        let mut config = AppConfig::default();
        config.pipeline.max_parallel = 1;
        config.pipeline.default_timeout_secs = 120;
        let state = Arc::new(AppState::new(
            root.clone(),
            config,
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        ));

        static RELEASE: AtomicBool = AtomicBool::new(false);
        RELEASE.store(false, Ordering::SeqCst);
        static RAN: AtomicUsize = AtomicUsize::new(0);
        RAN.store(0, Ordering::SeqCst);
        set_test_run_hook(Some(Arc::new(|_task_id| {
            RAN.fetch_add(1, Ordering::SeqCst);
            while !RELEASE.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
            }
        })));

        let src = root.join("cq-src.txt");
        let dest1 = root.join("cq-out-1.txt");
        let dest2 = root.join("cq-out-2.txt");
        std::fs::write(&src, "cancel queued").unwrap();
        let t1 = submit_pipeline(&state, copy_pipeline("cq-1", &src, &dest1), None)
            .await
            .unwrap();
        let t2 = submit_pipeline(&state, copy_pipeline("cq-2", &src, &dest2), None)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(snapshot(&t2).unwrap().status, TaskState::Queued);

        // 取消排队中的 t2
        let outcome = request_cancel(&state, &t2).await;
        assert_eq!(outcome, CancelOutcome::Cancelled);
        let r2 = snapshot(&t2).unwrap();
        assert_eq!(r2.status, TaskState::Cancelled);
        assert!(r2.finished_at.is_some());

        // 重复取消 → AlreadyTerminal
        assert_eq!(
            request_cancel(&state, &t2).await,
            CancelOutcome::AlreadyTerminal(TaskState::Cancelled)
        );
        // 未知任务 → NotFound
        assert_eq!(
            request_cancel(&state, "task-ghost").await,
            CancelOutcome::NotFound
        );

        // 释放首个任务；被取消的 t2 绝不应执行
        RELEASE.store(true, Ordering::SeqCst);
        wait_terminal(&t1).await.expect("任务应终结");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(RAN.load(Ordering::SeqCst), 1, "被取消的排队任务不得执行");
        assert!(!dest2.exists(), "被取消任务的输出不得产生");
        set_test_run_hook(None);
    }

    // ── 14. 取消运行中任务 → Cancelled（引擎后台收尾被忽略） ────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_cancel_running_task() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("cancel-r");
        let mut config = AppConfig::default();
        config.pipeline.default_timeout_secs = 120;
        let state = Arc::new(AppState::new(
            root.clone(),
            config,
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        ));

        static RELEASE: AtomicBool = AtomicBool::new(false);
        RELEASE.store(false, Ordering::SeqCst);
        set_test_run_hook(Some(Arc::new(move |_task_id| {
            while !RELEASE.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
            }
        })));

        let src = root.join("cr-src.txt");
        let dest = root.join("cr-out.txt");
        std::fs::write(&src, "cancel running").unwrap();
        let task_id = submit_pipeline(&state, copy_pipeline("cr-1", &src, &dest), None)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(snapshot(&task_id).unwrap().status, TaskState::Running);

        assert_eq!(
            request_cancel(&state, &task_id).await,
            CancelOutcome::Cancelled
        );
        let record = snapshot(&task_id).unwrap();
        assert_eq!(record.status, TaskState::Cancelled);
        assert!(record.finished_at.is_some());

        // 释放引擎线程：后台收尾不得覆盖 Cancelled 终态
        RELEASE.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(snapshot(&task_id).unwrap().status, TaskState::Cancelled);
        set_test_run_hook(None);
    }

    // ── 15. 任务级超时（default_timeout_secs 看门狗） ───────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_task_timeout_watchdog() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("timeout");
        let mut config = AppConfig::default();
        config.pipeline.default_timeout_secs = 1; // 1s 超时
        let state = Arc::new(AppState::new(
            root.clone(),
            config,
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        ));

        static RELEASE: AtomicBool = AtomicBool::new(false);
        RELEASE.store(false, Ordering::SeqCst);
        set_test_run_hook(Some(Arc::new(move |_task_id| {
            // 持锁 8s >> 1s 超时（测试总时长由释放控制，超时判定后即刻收尾）
            let deadline = std::time::Instant::now() + Duration::from_secs(8);
            while !RELEASE.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        })));

        let src = root.join("to-src.txt");
        let dest = root.join("to-out.txt");
        std::fs::write(&src, "timeout").unwrap();
        let task_id = submit_pipeline(&state, copy_pipeline("to-1", &src, &dest), None)
            .await
            .unwrap();

        let record = wait_terminal(&task_id).await.expect("看门狗应落终态");
        assert_eq!(record.status, TaskState::Failed);
        assert!(
            record.error.as_deref().unwrap_or("").contains("timed out"),
            "错误应注明超时: {:?}",
            record.error
        );

        // 释放引擎线程：收尾不得覆盖超时终态，闸门正常释放（后续任务可运行）
        RELEASE.store(true, Ordering::SeqCst);
        let src2 = root.join("to-src-2.txt");
        let dest2 = root.join("to-out-2.txt");
        std::fs::write(&src2, "after timeout").unwrap();
        let t2 = submit_pipeline(&state, copy_pipeline("to-2", &src2, &dest2), None)
            .await
            .unwrap();
        let r2 = wait_terminal(&t2).await.expect("后续任务应正常执行");
        assert_eq!(r2.status, TaskState::Completed, "超时任务释放闸门后新任务可运行");
        set_test_run_hook(None);
    }

    // ── 16. wait 同步模式（§6.5） ────────────────────────────────────────────

    #[tokio::test]
    async fn test_wait_mode_returns_terminal_record() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("wait");
        let state = test_state(root.clone());
        let src = root.join("wait-src.txt");
        let dest = root.join("wait-out.txt");
        std::fs::write(&src, "wait mode").unwrap();

        let outcome = submit_pipeline_full(
            &state,
            copy_pipeline("wait-pipe", &src, &dest),
            None,
            SubmitOptions {
                wait: true,
                callback_url: None,
            },
        )
        .await
        .unwrap();
        let record = outcome.record.expect("wait 模式必须携带记录");
        assert_eq!(record.status, TaskState::Completed);
        assert_eq!(record.id, outcome.task_id);
        assert_eq!(record.artifacts.len(), 2);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "wait mode");

        // wait=false 不携带记录（异步语义）
        let src2 = root.join("wait-src-2.txt");
        let dest2 = root.join("wait-out-2.txt");
        std::fs::write(&src2, "async").unwrap();
        let outcome2 = submit_pipeline_full(
            &state,
            copy_pipeline("wait-pipe-2", &src2, &dest2),
            None,
            SubmitOptions::default(),
        )
        .await
        .unwrap();
        assert!(outcome2.record.is_none());
        wait_terminal(&outcome2.task_id).await.expect("任务应终结");
    }

    // ── 17. callback_url 完成回调（本地 mock 端点，§6.5） ───────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_callback_url_posted_on_terminal() {
        use axum::http::StatusCode;
        use axum::routing::post;
        use axum::Router;

        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("callback");
        let state = test_state(root.clone());

        // 本地 mock 回调端点：捕获请求体
        static CAPTURED: std::sync::OnceLock<std::sync::Mutex<Vec<Value>>> =
            std::sync::OnceLock::new();
        CAPTURED
            .get_or_init(|| std::sync::Mutex::new(Vec::new()))
            .lock()
            .unwrap()
            .clear();
        let app = Router::new().route(
            "/cb",
            post(|axum::Json(body): axum::Json<Value>| async move {
                CAPTURED
                    .get()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .push(body);
                StatusCode::OK
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let src = root.join("cb-src.txt");
        let dest = root.join("cb-out.txt");
        std::fs::write(&src, "callback").unwrap();
        let outcome = submit_pipeline_full(
            &state,
            copy_pipeline("cb-pipe", &src, &dest),
            None,
            SubmitOptions {
                wait: true,
                callback_url: Some(format!("http://{addr}/cb")),
            },
        )
        .await
        .unwrap();
        assert_eq!(outcome.record.unwrap().status, TaskState::Completed);

        // 回调是 spawn 异步投递，轮询等待
        let mut body: Option<Value> = None;
        for _ in 0..100 {
            let captured = CAPTURED.get().unwrap().lock().unwrap();
            if let Some(v) = captured.first() {
                body = Some(v.clone());
                break;
            }
            drop(captured);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let body = body.expect("回调应已投递");
        assert_eq!(body["task_id"], outcome.task_id);
        assert_eq!(body["status"], "completed");
        let artifacts = body["artifacts"].as_array().expect("artifacts 数组");
        assert_eq!(artifacts.len(), 2);
        assert!(artifacts.iter().all(|a| a["size"].as_u64().unwrap_or(0) > 0));
    }

    // ── 18. 直跑 DAG 编译形状（§5.3 退化三节点） ────────────────────────────

    #[test]
    fn test_build_direct_pipeline_shape() {
        let pipeline = build_direct_pipeline(
            "deep-filter",
            "denoise",
            serde_json::json!({ "atten_lim_db": 60 }),
            Path::new("/tmp/in.wav"),
            None,
        );
        assert_eq!(pipeline.id, "direct/deep-filter", "B4 契约 id 形状");
        assert_eq!(pipeline.nodes.len(), 3);
        assert_eq!(pipeline.edges.len(), 2);
        // 通过 dag validate（含 file_input 检查）
        assert!(pipeline.validate().is_ok());
        // 节点内容
        assert_eq!(
            pipeline.nodes[0].kind,
            NodeKind::Builtin {
                builtin: "file_input".to_string()
            }
        );
        assert_eq!(
            pipeline.nodes[0].params["path"],
            "/tmp/in.wav"
        );
        // D-7：file_output 携带 extension（无声明时取输入扩展名），不再落盘 .out
        assert_eq!(pipeline.nodes[2].params["extension"], "wav");
        assert_eq!(
            pipeline.nodes[1].kind,
            NodeKind::Module {
                module_id: "deep-filter".to_string(),
                capability: "denoise".to_string(),
                model_id: None,
                device: None,
            }
        );
        assert_eq!(pipeline.nodes[1].params["atten_lim_db"], 60);
        assert_eq!(
            pipeline.nodes[2].kind,
            NodeKind::Builtin {
                builtin: "file_output".to_string()
            }
        );
        // 边：input → run → output
        assert_eq!(pipeline.edges[0].from, ("input".to_string(), "output".to_string()));
        assert_eq!(pipeline.edges[0].to, ("run".to_string(), "input".to_string()));
        assert_eq!(pipeline.edges[1].from, ("run".to_string(), "output".to_string()));
        assert_eq!(pipeline.edges[1].to, ("output".to_string(), "input".to_string()));
    }

    // ── 18b. F2：直跑产物扩展名接线（带 capability 声明的三级推导） ────────

    /// 构造测试 capability 声明（仅 input/output 类型参与推导）
    fn cap_decl(input: DataType, output: DataType) -> CapabilityDecl {
        CapabilityDecl {
            name: "test".to_string(),
            description: String::new(),
            input_type: input,
            output_type: output,
            max_file_size_mb: None,
            supports_batch: false,
            params: None,
        }
    }

    #[test]
    fn test_build_direct_pipeline_cross_format_extensions() {
        // TTS text→audio：txt 输入产物标 .wav（F2 主场景，不再误标 .txt）
        let tts = cap_decl(DataType::Text, DataType::Audio);
        let p = build_direct_pipeline(
            "qwen3-tts",
            "synthesize",
            serde_json::json!({ "voice": "default" }),
            Path::new("/tmp/tts_input.txt"),
            Some(&tts),
        );
        assert_eq!(p.nodes[2].params["extension"], "wav");

        // faster-whisper audio→json：显式 output_format=srt → .srt
        let asr = cap_decl(DataType::Audio, DataType::Json);
        let p = build_direct_pipeline(
            "faster-whisper",
            "transcribe",
            serde_json::json!({ "output_format": "srt" }),
            Path::new("/tmp/speech.wav"),
            Some(&asr),
        );
        assert_eq!(p.nodes[2].params["extension"], "srt");
        // 同能力不传 output_format → 语义映射 json
        let p = build_direct_pipeline(
            "faster-whisper",
            "transcribe",
            serde_json::json!({}),
            Path::new("/tmp/speech.wav"),
            Some(&asr),
        );
        assert_eq!(p.nodes[2].params["extension"], "json");

        // rembg image→image 同格式回归：产物仍随输入 .png，不被映射改写
        let rembg = cap_decl(DataType::Image, DataType::Image);
        let p = build_direct_pipeline(
            "rembg",
            "remove_bg",
            serde_json::json!({}),
            Path::new("/tmp/photo.png"),
            Some(&rembg),
        );
        assert_eq!(p.nodes[2].params["extension"], "png");

        // deep-filter audio→audio 同格式回归：输入 .flac 产物仍 .flac
        let df = cap_decl(DataType::Audio, DataType::Audio);
        let p = build_direct_pipeline(
            "deep-filter",
            "denoise",
            serde_json::json!({}),
            Path::new("/tmp/noisy.flac"),
            Some(&df),
        );
        assert_eq!(p.nodes[2].params["extension"], "flac");
    }

    // ── 19. 直跑错误路径：模块不存在 / capability 不存在 / 输入缺失 ─────────

    #[tokio::test]
    async fn test_submit_direct_error_paths() {
        use ep_core::module::manifest::ModuleManifest;

        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("direct-err");
        // 带一个真实 manifest 的模块（capability = transcribe）
        let manifest: ModuleManifest = toml::from_str(
            r#"
[module]
id = "mock-asr"
name = "Mock ASR"
version = "0.1.0"
description = "test"
category = "asr"
genre = "test"
license = "MIT"

[runtime]
type = "python"

[compute]
backends = ["cpu"]

[interface]
type = "http"

[[interface.capabilities]]
name = "transcribe"
description = "转写"
input_type = "audio"
output_type = "json"
"#,
        )
        .unwrap();
        let module = ep_core::module::discovery::DiscoveredModule {
            manifest: Some(manifest),
            path: root.join("modules/mock-asr"),
            status: ep_core::module::discovery::DiscoveryStatus::Valid,
        };
        let state = Arc::new(AppState::new(
            root.clone(),
            AppConfig::default(),
            vec![],
            vec![module],
            PortManager::new(18000, 19000),
        ));

        let input = root.join("direct-in.txt");
        std::fs::write(&input, "direct").unwrap();

        // 模块不存在
        let err = submit_direct(
            &state,
            "ghost-module",
            "transcribe",
            json!({}),
            input.clone(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, SubmitError::ModuleNotFound(_)));

        // capability 不存在
        let err = submit_direct(
            &state,
            "mock-asr",
            "ghost-capability",
            json!({}),
            input.clone(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, SubmitError::CapabilityNotFound(_, _)));
        assert!(err.to_string().contains("ghost-capability"));

        // 输入文件缺失
        let err = submit_direct(
            &state,
            "mock-asr",
            "transcribe",
            json!({}),
            root.join("no-such-file.txt"),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, SubmitError::InputMissing(_)));
    }

    // ── 19.1 submit_direct_full wait / 非 wait 两路（镜像 submit_pipeline_full 语义）

    #[tokio::test]
    async fn test_submit_direct_full_wait_and_async() {
        use ep_core::module::manifest::ModuleManifest;

        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("direct-full");
        // 复用 mock-asr fixture：本环境无 Python venv，模块进程拉不起来——
        // 任务终态必为 Failed，但 wait/非 wait 的提交语义仍可完整验证。
        let manifest: ModuleManifest = toml::from_str(
            r#"
[module]
id = "mock-asr"
name = "Mock ASR"
version = "0.1.0"
description = "test"
category = "asr"
genre = "test"
license = "MIT"

[runtime]
type = "python"

[compute]
backends = ["cpu"]

[interface]
type = "http"

[[interface.capabilities]]
name = "transcribe"
description = "转写"
input_type = "audio"
output_type = "json"
"#,
        )
        .unwrap();
        let module = ep_core::module::discovery::DiscoveredModule {
            manifest: Some(manifest),
            path: root.join("modules/mock-asr"),
            status: ep_core::module::discovery::DiscoveryStatus::Valid,
        };
        let state = Arc::new(AppState::new(
            root.clone(),
            AppConfig::default(),
            vec![],
            vec![module],
            PortManager::new(18000, 19000),
        ));

        let input = root.join("direct-full-in.txt");
        std::fs::write(&input, "direct-full").unwrap();

        // wait=true：阻塞至终态，record 必为 Some 且已终结
        let outcome = submit_direct_full(
            &state,
            "mock-asr",
            "transcribe",
            json!({}),
            input.clone(),
            SubmitOptions {
                wait: true,
                callback_url: None,
            },
        )
        .await
        .unwrap();
        let record = outcome.record.expect("wait 模式必须携带记录");
        assert_eq!(record.id, outcome.task_id);
        assert!(record.status.is_terminal());
        assert!(record.pipeline_id.starts_with("direct/"));

        // wait=false：立即返回 task_id 且 record 为 None，轮询至终态
        let outcome2 = submit_direct_full(
            &state,
            "mock-asr",
            "transcribe",
            json!({}),
            input,
            SubmitOptions::default(),
        )
        .await
        .unwrap();
        assert!(outcome2.record.is_none());
        let record2 = wait_terminal(&outcome2.task_id)
            .await
            .expect("任务应终结");
        assert!(record2.status.is_terminal());
    }

    // ── 20. snapshot_by_pipeline + 队列位置标注 ─────────────────────────────

    #[tokio::test]
    async fn test_snapshot_by_pipeline() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("bypipe");
        let state = test_state(root.clone());
        let src = root.join("bp-src.txt");
        let dest_a = root.join("bp-out-a.txt");
        let dest_b = root.join("bp-out-b.txt");
        let dest_c = root.join("bp-out-c.txt");
        std::fs::write(&src, "by pipeline").unwrap();

        let ta = submit_pipeline(&state, copy_pipeline("pipe-a", &src, &dest_a), None)
            .await
            .unwrap();
        let tb = submit_pipeline(&state, copy_pipeline("pipe-b", &src, &dest_b), None)
            .await
            .unwrap();
        let tc = submit_pipeline(&state, copy_pipeline("pipe-a", &src, &dest_c), None)
            .await
            .unwrap();
        for id in [&ta, &tb, &tc] {
            wait_terminal(id).await.expect("任务应终结");
        }

        let a_tasks = snapshot_by_pipeline("pipe-a");
        assert_eq!(a_tasks.len(), 2);
        assert!(a_tasks.iter().all(|r| r.pipeline_id == "pipe-a"));
        assert_eq!(a_tasks[0].id, tc, "新任务在前");
        assert_eq!(snapshot_by_pipeline("pipe-b").len(), 1);
        assert!(snapshot_by_pipeline("ghost").is_empty());
    }

    // ── 21. 空闲看门狗判定（缺陷 #3 拆分：心跳/进度看门狗 ≠ 节点硬超时） ─────

    /// 按给定节点状态构造最小 TaskRecord（纯函数测试用）
    fn watchdog_record(node_states: &[&str]) -> TaskRecord {
        let nodes: HashMap<String, NodeRecord> = node_states
            .iter()
            .enumerate()
            .map(|(i, s)| {
                (
                    format!("n{i}"),
                    NodeRecord {
                        state: (*s).to_string(),
                        error: None,
                    },
                )
            })
            .collect();
        TaskRecord {
            id: "wd-task".to_string(),
            pipeline_id: "wd-pipe".to_string(),
            status: TaskState::Running,
            error: None,
            queue_position: None,
            started_at: Utc::now(),
            started_running_at: Some(Utc::now()),
            finished_at: None,
            node_order: nodes.keys().cloned().collect(),
            nodes,
            artifacts: HashMap::new(),
            served_artifacts: HashMap::new(),
            work_dir: PathBuf::new(),
        }
    }

    #[test]
    fn test_watchdog_idle_exceeded_semantics() {
        // 节点在飞（长媒体调用中）= 有心跳 → 不判死（拆分后不再按任务总时长误杀）
        let running = watchdog_record(&["running"]);
        assert!(!watchdog_idle_exceeded(&running, 0, 999_999_999, 300));

        // 无节点在飞 + 空闲 ≥ 阈值 → 判死
        let idle = watchdog_record(&["completed", "pending"]);
        assert!(watchdog_idle_exceeded(&idle, 1_000, 1_000 + 300_000, 300));

        // 无节点在飞但空闲未达阈值（心跳刚刷新）→ 不判死
        assert!(!watchdog_idle_exceeded(&idle, 1_000, 1_000 + 299_999, 300));

        // 时钟回拨（now < last_activity）→ saturating_sub 钉为 0，不误判
        assert!(!watchdog_idle_exceeded(&idle, 5_000, 1_000, 300));
    }

    // ── 22. finalize CAS 假赢回归（P1：终态标志与 extras 生命周期解耦） ────────

    /// 构造一个占闸门的运行中任务（registry + extras + scheduler 计数齐备）
    fn seed_running_task(task_id: &str, pipeline_id: &str) {
        let record = TaskRecord {
            id: task_id.to_string(),
            pipeline_id: pipeline_id.to_string(),
            status: TaskState::Running,
            error: None,
            queue_position: None,
            started_at: Utc::now(),
            started_running_at: Some(Utc::now()),
            finished_at: None,
            node_order: Vec::new(),
            nodes: HashMap::new(),
            artifacts: HashMap::new(),
            served_artifacts: HashMap::new(),
            work_dir: PathBuf::new(),
        };
        registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(record)
            .expect("seed record insert");
        extras()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                task_id.to_string(),
                TaskExtras {
                    cancel: Arc::new(AtomicBool::new(false)),
                    callback_url: None,
                    last_activity_ms: Arc::new(AtomicU64::new(now_epoch_ms())),
                },
            );
        {
            let mut sched = scheduler().lock().unwrap_or_else(|e| e.into_inner());
            sched.running_count = 1;
            sched.running_by_pipeline.insert(pipeline_id.to_string(), 1);
        }
    }

    /// 回归：赢家完成收尾（extras 已移除、闸门已释放）后，迟到的第二次
    /// finalize（引擎收尾线程 / 看门狗并发到达）必须输——不得重复释放闸门。
    ///
    /// 旧实现把 CAS 标志放在 extras 上，赢家先 remove 再释放闸门，此后
    /// 并发 finalize 读 None → 假赢（返回 Some 而非 None）。
    #[tokio::test]
    async fn test_finalize_second_call_loses() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("finalize-twice");
        let state = test_state(root.clone());
        seed_running_task("race-1", "p1");

        // 第一次 finalize（模拟引擎收尾）→ 唯一赢家
        let first = finalize_task(
            &state,
            "race-1",
            "p1",
            TerminalCause::Engine(None),
            None,
            None,
        )
        .await;
        assert_eq!(first, Some(TaskState::Completed));
        // 闸门已释放
        assert_eq!(
            scheduler()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .running_count,
            0
        );

        // 第二次 finalize（模拟看门狗/引擎迟到路径）→ 必须输（None）
        let second = finalize_task(
            &state,
            "race-1",
            "p1",
            TerminalCause::Engine(Some("late finalizer".to_string())),
            None,
            None,
        )
        .await;
        assert!(second.is_none(), "已终态任务二次 finalize 必须返回 None");
        // 闸门不得再次被减（0 保持 0，且终态记录不被覆盖）
        assert_eq!(
            scheduler()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .running_count,
            0
        );
        let record = snapshot("race-1").expect("记录仍在案");
        assert_eq!(record.status, TaskState::Completed, "迟到的 finalize 不得覆盖终态");
        assert!(record.error.is_none(), "迟到的失败原因不得写入");
    }

    /// 并发 finalize（引擎收尾 / 超时看门狗 / 用户取消三方同时到达）：
    /// CAS 唯一赢家——成功次数必须恰为 1。
    #[tokio::test]
    async fn test_finalize_concurrent_single_winner() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("finalize-race");
        let state = test_state(root.clone());
        seed_running_task("race-2", "p2");

        let mut handles = Vec::new();
        for i in 0..8 {
            let state = state.clone();
            handles.push(tokio::spawn(async move {
                let cause = if i % 3 == 0 {
                    TerminalCause::Engine(None)
                } else if i % 3 == 1 {
                    TerminalCause::Timeout(300)
                } else {
                    TerminalCause::Cancelled
                };
                finalize_task(&state, "race-2", "p2", cause, None, None)
                    .await
                    .map(|t| t.as_str().to_string())
            }));
        }
        let mut winners: Vec<String> = Vec::new();
        for handle in handles {
            if let Ok(Some(status)) = handle.await {
                winners.push(status);
            }
        }
        assert_eq!(winners.len(), 1, "并发 finalize 必须只有唯一赢家");
        // 闸门只被释放一次
        assert_eq!(
            scheduler()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .running_count,
            0
        );
    }
}
