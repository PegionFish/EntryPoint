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
use tracing::{debug, info, warn};

use ep_core::cache::{CacheKey, CacheMeta};
use ep_core::module::manifest::CapabilityDecl;
use ep_core::pipeline::builtin_templates;
use ep_core::pipeline::dag::{
    direct_output_extension, NodeKind, Pipeline, PipelineNode, ValidationError,
};
use ep_core::pipeline::runner::TaskDetail;
use ep_core::pipeline::vram;
use ep_core::pipeline::PipelineRunnerImpl;
use ep_core::task_registry::TaskRegistry;
use ep_core::types::{Artifact, DataType, DeviceId, PipelineRunner};

use crate::state::{AppState, ProgressMessage};

// 类型下沉 ep-core 后对既有消费方（tasks.rs / execute.rs）保持原路径可用
pub use ep_core::task_registry::{
    NodeRecord, TaskRecord, TaskState, DEFAULT_TASK_PRIORITY,
};

// ─── 进程级状态：注册表 / 调度器 / 待执行载荷 / 运行时标志 ──────────────────

static REGISTRY: OnceLock<Mutex<TaskRegistry>> = OnceLock::new();

/// 空闲回收器守卫：当前排队/运行中任务引用的全部模块 id
/// （仅 main.rs 二进制消费；e2e 测试独立编译本文件时不可达 → 豁免 dead_code）
#[allow(dead_code)]
pub fn active_task_module_ids() -> std::collections::HashSet<String> {
    let reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    reg.all()
        .into_iter()
        .filter(|r| r.is_active())
        .flat_map(|r| r.module_ids)
        .collect()
}

/// 定时调度触发入口：按管线 id 从磁盘加载 spec → 提交执行。
/// inputs/params 来自 schedule 条目模板；管线缺失或校验失败即报错
/// （调度错误进日志与 last_task_id 缺省，不影响巡检循环存活）。
///
/// 字符串错误面（cron 巡检循环消费）；typed 变体见
/// [`submit_pipeline_for_schedule_typed`]（T-27 webhook 触发链路据此映射
/// 422/429 等语义）。
#[allow(dead_code)] // e2e 测试独立编译本文件时无 main 循环消费方
pub async fn submit_pipeline_for_schedule(
    state: &Arc<AppState>,
    pipeline_id: &str,
    inputs: std::collections::HashMap<String, serde_json::Value>,
    params: serde_json::Value,
) -> Result<String, String> {
    submit_pipeline_for_schedule_typed(state, pipeline_id, inputs, params)
        .await
        .map_err(|e| e.to_string())
}

/// [`submit_pipeline_for_schedule`] 的 typed 错误版（T-27）：webhook 触发
/// 链路消费——管线缺失/无效 → 400、缺 store → 422、队列满 → 429。
#[allow(dead_code)] // e2e 测试独立编译本文件时无 main 循环消费方
pub async fn submit_pipeline_for_schedule_typed(
    state: &Arc<AppState>,
    pipeline_id: &str,
    inputs: std::collections::HashMap<String, serde_json::Value>,
    params: serde_json::Value,
) -> Result<String, SubmitError> {
    let dir = state.root.join("config").join("pipelines");
    let spec = crate::api::pipelines::pipeline_bridge::load_spec(&dir.join(format!(
        "{}.toml",
        pipeline_id.replace('-', "_")
    )))
    .or_else(|_| {
        // 兜底：扫描匹配（文件名规范外历史文件）
        let mut found = None;
        for (path, sp) in crate::api::pipelines::scan_specs_pub(&dir) {
            if sp.pipeline.id == pipeline_id {
                found = Some(crate::api::pipelines::pipeline_bridge::load_spec(&path));
                break;
            }
        }
        found.unwrap_or_else(|| {
            Err(anyhow::anyhow!("pipeline '{pipeline_id}' not found"))
        })
    })
    .map_err(|e| SubmitError::InvalidPipeline(e.to_string()))?;
    let mut pipeline = crate::api::pipelines::pipeline_bridge::spec_to_pipeline(&spec)
        .map_err(|e| SubmitError::InvalidPipeline(e.to_string()))?;
    if !params.is_null() {
        if let Some(obj) = params.as_object() {
            for (node_id, patch) in obj {
                if let Some(node) = pipeline.nodes.iter_mut().find(|n| &n.id == node_id) {
                    if let (Some(base), Some(patch_obj)) =
                        (node.params.as_object_mut(), patch.as_object())
                    {
                        for (k, v) in patch_obj {
                            base.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
        }
    }
    submit_pipeline_full(state, pipeline, Some(inputs), SubmitOptions::default())
        .await
        .map(|o| o.task_id)
}

/// 调度巡检消费：注册表全量快照
#[allow(dead_code)]
pub fn task_registry_all() -> Vec<ep_core::task_registry::TaskRecord> {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .all()
}

/// 从内存注册表移除在案任务（retention 清盘 P2-1 消费；盘上 JSON 由调用方
/// 单独删除）。任务不在案时无操作。
// allow(dead_code)：e2e_daemon 经 #[path] 双编译本文件但不含 retention（调用
// 方），与 task_registry_all 同款豁免
#[allow(dead_code)]
pub fn remove_task_record(task_id: &str) {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(task_id);
}

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

/// 调度器：优先级队列（同优先级 FIFO）+ 两级闸门计数（§6.8）+ 动态 VRAM 准入账本（§6.8b）
///
/// T-22 force 语义：force 任务在队列中**插队**（先于全部非 force 任务出队，
/// 同为 force 时按优先级 + FIFO），且出队判定**跳过 §6.8b 动态准入检查**
/// （模块并发上限 + 设备显存余额）；静态两级闸门（全局 max_parallel +
/// 管线 max_instances）与全局暂停**不受影响**（安全上限不放行）。插队导致
/// 的账本超支即为用户显式接受的 overcommit（任务元数据 `forced_overcommit`
/// / `accepted_risk = "user"`，经 [`task_forced_overcommit`] 对外透出）。
#[derive(Debug, Default)]
struct Scheduler {
    /// 等待闸门的任务（按优先级出队；同优先级保持入队顺序；force 插队）
    queue: VecDeque<QueuedEntry>,
    /// 全局暂停标志（T-15）：只挡新任务准入，运行中照常完成、排队保留
    /// （force 任务同样被暂停拦截——暂停是管理面安全控制，不是 §6.8b 准入等待）
    paused: bool,
    /// 全局运行中任务数（≤ max_parallel）
    running_count: usize,
    /// 每管线运行中任务数（≤ 该管线 max_instances，缺省跟随全局）
    running_by_pipeline: HashMap<String, usize>,
    /// 每管线并发上限缓存（提交时从管线 TOML 解析刷新）
    pipeline_limits: HashMap<String, Option<u32>>,
    /// 每模块运行中任务数（动态准入：≤ manifest `[compute] max_concurrent`，缺省 1）
    running_by_module: HashMap<String, usize>,
    /// 每设备已提交显存预算（动态准入账本，MB）：Σ 已准入任务的每实例预算；
    /// 与实时占用取 max 消费——指标刷新（2s）滞后窗口内由账本兜住，防超发。
    /// UMA 平台（T-22）全部 GPU 预算记入单一 `"uma"` 键（单池共享语义）；
    /// force 准入可使该账本超过设备总量——超支部分即被接受的 overcommit。
    vram_committed: HashMap<String, u32>,
    /// 已准入任务的显存计划（task_id → [(device_key, cost_mb)]），终结时精确销账
    vram_plans: HashMap<String, Vec<(String, u32)>>,
}

#[derive(Debug, Clone)]
struct QueuedEntry {
    task_id: String,
    pipeline_id: String,
    /// 调度优先级（越小越先出队；同优先级按入队顺序 FIFO）
    priority: u8,
    /// 任务引用的模块 id（去重；动态准入消费，提交时随载荷登记）
    module_ids: Vec<String>,
    /// §6.8b force（T-22）：true 时插队 + 跳过动态准入检查（静态闸门与
    /// 暂停不受影响；账本照常记账）
    force: bool,
}

/// T-22：force 提交（§6.8b 准入跳过 + 插队）的任务 id 集。
///
/// 提交模式元数据的进程内侧表（`TaskRecord` 形状冻结，不为其增字段）：
/// 提交时登记，任务在案期间保留（与 FINALIZED 同生命周期语义——终态后
/// 任务详情仍可展示提交模式），仅 `clear_registry_for_tests` 清空。
/// 不跨重启存活：重启后遗留任务一律改判 failed，提交模式随之失传（可接受）。
static FORCED_TASKS: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();

fn forced_tasks() -> &'static Mutex<std::collections::HashSet<String>> {
    FORCED_TASKS.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// force 提交的任务元数据（T-22）：`accepted_risk` 固定取值。
///
/// 与任务详情/提交响应的 `accepted_risk` 字段同源；语义 = 用户显式接受
/// 跳过 §6.8b 动态准入等待可能带来的显存超支风险。
pub const FORCE_ACCEPTED_RISK: &str = "user";

/// 任务是否为 force 提交（§6.8b）：任务详情/任务列表据此透出
/// `forced_overcommit: true` + `accepted_risk: "user"`。
pub fn task_forced_overcommit(task_id: &str) -> bool {
    forced_tasks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(task_id)
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

/// 直跑缓存写入上下文（T-16）：提交时算好键与元数据，仅在任务成功终态
/// 落缓存；失败/取消任务只需从表中移除。
#[derive(Debug, Clone)]
struct CacheSubmission {
    /// 内容寻址键（模块 + 能力 + 规范化参数 + 输入指纹）
    key: CacheKey,
    module_id: String,
    capability: String,
    /// 规范化参数快照（写缓存元信息用，便于人工排查键来源）
    params: Value,
}

static CACHE_PENDING: OnceLock<Mutex<HashMap<String, CacheSubmission>>> = OnceLock::new();

fn cache_pending() -> &'static Mutex<HashMap<String, CacheSubmission>> {
    CACHE_PENDING.get_or_init(|| Mutex::new(HashMap::new()))
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

// ─── 任务组（外部批量接入）：组 id 生成 / 成员查询 / 组取消 ─────────────────

/// 组 id 序列（与任务 id 序列独立，前缀区分）
static GROUP_SEQ: AtomicUsize = AtomicUsize::new(0);

/// 新组 id：`grp-{timestamp}-{seq}`（形状与 new_task_id 同款，前缀区分用途）
pub fn new_group_id() -> String {
    let seq = GROUP_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("grp-{}-{seq:04}", Utc::now().format("%Y%m%d-%H%M%S"))
}

/// 组成员快照（按提交顺序），供组聚合展示与 v1 组查询。
/// 组不存在（无成员）返回空 Vec。
pub fn snapshot_group(group_id: &str) -> Vec<TaskRecord> {
    let mut members: Vec<TaskRecord> = registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .all()
        .into_iter()
        .filter(|r| r.group.as_deref() == Some(group_id))
        .collect();
    members.sort_by_key(|m| m.started_at);
    members
}

/// 全部组 id（按组内最早任务时间倒序），供组列表
pub fn group_ids() -> Vec<String> {
    let all = registry().lock().unwrap_or_else(|e| e.into_inner()).all();
    let mut groups: Vec<(String, chrono::DateTime<Utc>)> = Vec::new();
    for r in all {
        if let Some(g) = &r.group {
            let entry = groups.iter_mut().find(|(id, _)| id == g);
            match entry {
                Some((_, earliest)) => {
                    if r.started_at < *earliest {
                        *earliest = r.started_at;
                    }
                }
                None => groups.push((g.clone(), r.started_at)),
            }
        }
    }
    groups.sort_by_key(|g| std::cmp::Reverse(g.1));
    groups.into_iter().map(|(id, _)| id).collect()
}

/// 组取消结果
pub enum GroupCancelOutcome {
    /// 组不存在（无任何成员）
    NotFound,
    /// `cancelled` = 受理取消的成员数；`already_terminal` = 终态成员数
    Cancelled { cancelled: usize, already_terminal: usize },
}

/// 组取消：对全部非终态成员逐一发起 [`request_cancel`]（终态成员跳过）。
/// 逐成员独立受理——部分成员可能已终结，`cancelled`/`already_terminal`
/// 如实分开计数。
pub async fn cancel_group(state: &Arc<AppState>, group_id: &str) -> GroupCancelOutcome {
    let members = snapshot_group(group_id);
    if members.is_empty() {
        return GroupCancelOutcome::NotFound;
    }
    let mut cancelled = 0usize;
    let mut already_terminal = 0usize;
    for member in &members {
        if member.status.is_terminal() {
            already_terminal += 1;
            continue;
        }
        if matches!(
            request_cancel(state, &member.id).await,
            CancelOutcome::Cancelled
        ) {
            cancelled += 1;
        } else {
            already_terminal += 1;
        }
    }
    GroupCancelOutcome::Cancelled { cancelled, already_terminal }
}

/// 注册表持久化目录（组元数据文件落位 `{dir}/groups/`；未启用持久化 → None）
pub fn persistence_dir() -> Option<std::path::PathBuf> {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .persist_dir()
        .map(|p| p.to_path_buf())
}

/// 在途容量上限（排队 + 运行中）的只读访问器（批量端点预检用）
pub fn max_inflight_tasks() -> usize {
    MAX_INFLIGHT_TASKS
}

/// 当前在途任务数（FIFO 队列长度 + 运行中计数；与提交路径的
/// 容量检查同源），供批量端点整批入队预检
pub fn inflight_count() -> usize {
    let sched = scheduler().lock().unwrap_or_else(|e| e.into_inner());
    sched.queue.len() + sched.running_count
}

/// 仅测试：直接设定运行中计数（跨模块模拟队列满载；execution 模块内
/// 测试可直接经 `scheduler()` 改写，无需本助手）
#[cfg(test)]
pub fn set_running_count_for_tests(n: usize) {
    scheduler()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .running_count = n;
}

// ─── 队列运行态与全局暂停控制（T-15） ───────────────────────────────────────

/// 队列运行态快照（`GET /api/queue` 响应体；前端队列状态条消费）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueStatus {
    /// 是否暂停派发（运行中任务不受影响，排队任务保留）
    pub paused: bool,
    /// 当前运行中任务数
    pub running: usize,
    /// 当前排队等待派发的任务数
    pub queued: usize,
}

/// 读取队列运行态
pub fn queue_status() -> QueueStatus {
    let sched = scheduler().lock().unwrap_or_else(|e| e.into_inner());
    QueueStatus {
        paused: sched.paused,
        running: sched.running_count,
        queued: sched.queue.len(),
    }
}

/// 暂停派发（T-15）：只挡新任务准入，运行中任务照常完成，排队任务保留。
pub fn pause_queue() -> QueueStatus {
    {
        let mut sched = scheduler().lock().unwrap_or_else(|e| e.into_inner());
        sched.paused = true;
    }
    info!("task queue paused; running tasks keep going, queued tasks retained");
    queue_status()
}

/// 恢复派发并立即尝试准入（异步；返回恢复后的队列快照）。
pub fn resume_queue(state: &Arc<AppState>) -> QueueStatus {
    {
        let mut sched = scheduler().lock().unwrap_or_else(|e| e.into_inner());
        sched.paused = false;
    }
    info!("task queue resumed");
    tokio::spawn({
        let state = state.clone();
        async move { try_promote(state).await }
    });
    queue_status()
}

/// T-22 出队排序键（越小越先出队）：**force 插队**（T-22：先于全部非 force
/// 任务）→ 优先级升序（T-15）→ 入队 FIFO。[`pick_next_index`] 与
/// [`queued_position_in`] 共用同一键，保证展示序位与实际派发顺序一致。
fn dispatch_rank(entry: &QueuedEntry, idx: usize) -> (std::cmp::Reverse<bool>, u8, usize) {
    (std::cmp::Reverse(entry.force), entry.priority, idx)
}

/// 排队任务的“派发序位”（1 起）：按出队规则（force 插队 → 优先级升序，
/// 同优先级 FIFO）计算前面还有多少任务，供任务中心展示真实等待顺序。
fn queued_position_in(sched: &Scheduler, task_id: &str) -> Option<usize> {
    let idx = sched.queue.iter().position(|e| e.task_id == task_id)?;
    let target = &sched.queue[idx];
    let ahead = sched
        .queue
        .iter()
        .enumerate()
        .filter(|(i, e)| dispatch_rank(e, *i) < dispatch_rank(target, idx))
        .count();
    Some(ahead + 1)
}

fn live_queue_position(task_id: &str) -> Option<usize> {
    let sched = scheduler().lock().unwrap_or_else(|e| e.into_inner());
    queued_position_in(&sched, task_id)
}

fn annotate_queue_positions(list: &mut [TaskRecord]) {
    let sched = scheduler().lock().unwrap_or_else(|e| e.into_inner());
    for record in list.iter_mut() {
        record.queue_position = if record.status == TaskState::Queued {
            queued_position_in(&sched, &record.id)
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
    /// T-26：文件入口的 `store:` 逻辑名引用解析失败 → 422（缺池/禁用，§6.4
    /// 缺依赖同款语义）/ 400（穿越注入）。提交在改写阶段拒绝，不建目录不入队。
    StoreRef(ep_core::config::StoreRefError),
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
            Self::StoreRef(e) => write!(f, "store reference invalid: {e}"),
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
    /// 所属任务组（外部批量接入）：同组任务共享组 id，任务中心折叠为
    /// 一条聚合展示，单个任务仍按 task_id 独立寻址
    pub group: Option<String>,
    /// 调度优先级（T-15，§6.8 排序扩展）：`None` = 默认 50；
    /// 0-49 高优先，同优先级保持入队 FIFO
    pub priority: Option<u8>,
    /// 直跑缓存旁路（T-16）：true 时既不读缓存也不写缓存；
    /// 仅直跑入口（`submit_direct_full`）消费，管线提交忽略
    pub bypass_cache: bool,
    /// §6.8b force（T-22）：true 时跳过动态 VRAM 准入的排队等待直接执行
    /// （模块并发上限 + 显存余额不参与判定，队列中插队）；静态两级闸门
    /// （max_parallel / max_instances）与全局暂停不受影响。任务元数据登记
    /// `forced_overcommit = true` + `accepted_risk = "user"`（见
    /// [`task_forced_overcommit`] / [`FORCE_ACCEPTED_RISK`]）。缺省 false
    /// 时提交行为与此字段加入前逐字节一致。
    pub force: bool,
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
    pipeline: Pipeline,
    inputs: Option<HashMap<String, Value>>,
    options: SubmitOptions,
) -> Result<SubmitOutcome, SubmitError> {
    submit_pipeline_full_inner(state, pipeline, inputs, options, None).await
}

/// [`submit_pipeline_full`] 的内部形态：`cache_submission` 由直跑缓存路径
/// 传入（T-16），任务成功终态时由 [`finalize_task`] 落缓存；普通提交为 None。
async fn submit_pipeline_full_inner(
    state: &Arc<AppState>,
    mut pipeline: Pipeline,
    inputs: Option<HashMap<String, Value>>,
    options: SubmitOptions,
    cache_submission: Option<CacheSubmission>,
) -> Result<SubmitOutcome, SubmitError> {
    if let Some(inputs) = inputs.as_ref() {
        apply_inputs(&mut pipeline, inputs)?;
    }

    // T-26：文件入口（file_input.path / file_output.path / file_archive.dest_dir）
    // 的 `store:` 逻辑名引用统一在此解析为物理路径——cron（submit_pipeline_for_schedule）、
    // watcher 管线模式、API 执行三条提交链共用本函数。缺池/禁用 → 422 语义拒绝
    //（StoreRef 变体，handler 映射）；物理路径原样保留（向后兼容）。
    {
        let config = state.config.read().await.clone();
        crate::api::stores::rewrite_pipeline_store_refs(&config, &state.root, &mut pipeline)
            .map_err(SubmitError::StoreRef)?;
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

    // T-15：调度优先级（缺省 50；缺省值下与纯 FIFO 行为完全一致）
    let priority = options.priority.unwrap_or(DEFAULT_TASK_PRIORITY);

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

    // 管线暂存区分配（RAM 优先，ep-core::staging）：节点中间产物与 adapter
    // 帧序列的落位根；任务终态由 run_task 收尾路径统一清算
    let staged = state.staging.read().await.alloc_task(&task_id);
    let staging_dir = staged.path.clone();

    // 空闲自动释放：任务关联模块即刻视为活跃（回收器据此豁免）
    let task_module_ids: Vec<String> = pipeline_module_ids(&pipeline);
    {
        let now = chrono::Utc::now().timestamp();
        let mut act = state.module_activity.lock().unwrap_or_else(|p| p.into_inner());
        for id in &task_module_ids {
            act.insert(id.clone(), now);
        }
    }

    let record_module_ids = task_module_ids.clone();
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
        priority,
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
        staging_dir: Some(staging_dir.to_string_lossy().to_string()),
        module_ids: record_module_ids,
        group: options.group.clone(),
        cached_from: None,
    };
    // 落盘失败不拒绝提交：记录已保留在内存（TaskRegistry::insert 内存优先），
    // 任务继续执行；磁盘/权限异常仅告警，避免单点 IO 故障让整机提交不可用。
    if let Err(e) = registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(record)
    {
        warn!(task_id = %task_id, error = %e, "任务记录落盘失败，仅保留内存副本（任务继续执行）");
    }

    // 入队（FIFO）+ 载荷寄存
    {
        let mut sched = scheduler().lock().unwrap_or_else(|e| e.into_inner());
        sched.pipeline_limits.insert(pipeline_id.clone(), max_instances);
        let mut module_ids = task_module_ids.clone();
        module_ids.sort();
        module_ids.dedup();
        sched.queue.push_back(QueuedEntry {
            task_id: task_id.clone(),
            pipeline_id: pipeline_id.clone(),
            priority,
            module_ids,
            force: options.force,
        });
    }
    // T-22：force 提交登记（§6.8b 元数据，任务详情/提交响应透出）
    if options.force {
        forced_tasks()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(task_id.clone());
        info!(task_id = %task_id, "force submit: dynamic VRAM admission wait skipped (§6.8b), accepted_risk = user");
    }
    pending().lock().unwrap_or_else(|e| e.into_inner()).insert(
        task_id.clone(),
        PendingTask {
            pipeline,
            task_dir,
            callback_url: options.callback_url.clone(),
        },
    );

    // T-16：直跑缓存上下文登记——任务成功终态时由 finalize_task 取出并落缓存
    if let Some(submission) = cache_submission {
        cache_pending()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(task_id.clone(), submission);
    }

    // 立即尝试准入：闸门有空位时提升 running（快路径）。
    // spawn 化（QUICK_RUN_PLAN G4）：start_task 内的模块自动拉起会同步等待
    // 健康就绪（冷启动可达数十秒），不应拖住提交响应——202 立即返回，
    // 拉起过程经任务状态 queued→running 在任务中心可见。
    tokio::spawn({
        let state = state.clone();
        async move { try_promote(state).await }
    });

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

// 尝试把队列中的任务提升为 running：全局闸门 + 管线闸门均放行才准入。
//
// 公平性：按 FIFO 顺序取**第一个**可准入的任务（跳过管线闸门已满的，
// 避免队头阻塞）；循环准入直到无可准入任务或全局闸门满。
// （原 `///` 文档注释已降级为普通注释：此处是准入章节导语，真正的
//   `try_promote` 定义在下方，避免 clippy::empty_line_after_doc_comments）
// ─── 动态 VRAM 准入（§6.8b：按实时显存余额与模块声明自动安排并发） ──────────
//
// 静态两级闸（全局 max_parallel + 管线 max_instances）只回答"允许几个"，
// 回答不了"现在还放得下几个"。本节在 FIFO 准入前补第三维判定：
//
// 1. **模块并发上限**：`[compute] max_concurrent`（缺省 1）——适配器自身
//    支撑的并发数（如 waifu2x 常驻 worker 池），EP 对引用该模块的并发
//    任务按此封顶，杜绝排队外的 503；
// 2. **显存余额**：每实例预算 = `min_vram_mb`（缺省回退 `vram_estimate_mb`）；
//    设备口径 = `max(实时占用, 账本已承诺)`——账本在准入瞬间记账（指标
//    2s 刷新的滞后窗口内防超发），任务终结精确销账。实时占用低于声明值
//    时（如 waifu2x 声明 6000 实测 ~1300）由账本一致性地约束，声明高于
//    实际只会少并发不会超发；
// 3. **fail-open**：设备无指标 / 模块无显存声明 / CPU 设备 → 不设显存限
//    （start_task 路径自有设备选择与 overcommit 兜底）；
// 4. **UMA 单池**（T-22）：统一内存架构平台（GB10 等，`compute::uma` 识别）
//    上全部 GPU 设备共享同一物理内存——账本把涉足 GPU 的模块预算统一记入
//    单一 `"uma"` 键（容量 = 物理内存总量，实时占用 = 系统内存占用），
//    杜绝「每设备独立池」在共享内存上重复计额。**GPU 切分预留（carve-out）
//    语义**：驱动为 GPU 划出的预留额度不单独建模（配置项预留、不实现），
//    池容量取物理总量、由实时占用的 max 口径吸收预留差异；识别失败静默
//    降级为逐设备池（现行为，零差异）。

/// UMA 单池的账本键（T-22）：统一内存平台上全部 GPU 预算汇入此键。
const UMA_POOL_KEY: &str = "uma";

/// 单设备准入账目（纯数据；快照判定可单测）
#[derive(Debug, Default, Clone)]
struct DeviceLedger {
    total_mb: u32,
    used_live_mb: u32,
}

/// 模块准入参数（纯数据）
#[derive(Debug, Clone)]
struct ModuleAdmission {
    /// 模块运行/预测设备（None = CPU 或无法预测 → 不设显存限）
    device: Option<String>,
    /// 每实例显存预算（MB）：`min_vram_mb ?? vram_estimate_mb ?? 0`
    cost_mb: u32,
    /// 声明并发上限（缺省 1）
    max_concurrent: u32,
}

/// 准入快照：`try_promote` 每轮在调度锁外重建（async 读 state），
/// `admits()` 为纯函数消费
#[derive(Debug, Default, Clone)]
struct AdmissionSnapshot {
    devices: HashMap<String, DeviceLedger>,
    modules: HashMap<String, ModuleAdmission>,
}

impl AdmissionSnapshot {
    /// 候选任务（去重模块表）是否可准入：
    /// 1. 每个引用模块的已准入运行数 < `max_concurrent`；
    /// 2. 每个涉足设备：`max(实时占用, 已承诺账本) + 本任务预算 ≤ 显存总量`。
    fn admits(
        &self,
        module_ids: &[String],
        running_by_module: &HashMap<String, usize>,
        vram_committed: &HashMap<String, u32>,
    ) -> bool {
        // 并发上限（去重模块；缺一条目 = fail-open）
        for mid in module_ids.iter().collect::<std::collections::BTreeSet<_>>() {
            if let Some(m) = self.modules.get(mid) {
                let running = running_by_module.get(mid).copied().unwrap_or(0);
                if running >= m.max_concurrent as usize {
                    return false;
                }
            }
        }
        // 显存余额（按设备归集本任务预算）
        let mut need: HashMap<&str, u32> = HashMap::new();
        for mid in module_ids {
            if let Some(m) = self.modules.get(mid) {
                if let (Some(dev), true) = (&m.device, m.cost_mb > 0) {
                    // 审查 P3-1：极端 manifest（min_vram_mb 上亿）× 多模块累加
                    // 可能溢出 u32——饱和而非 panic（debug）/环绕（release）
                    let slot = need.entry(dev.as_str()).or_insert(0);
                    *slot = slot.saturating_add(m.cost_mb);
                }
            }
        }
        for (dev, cost) in need {
            let Some(ledger) = self.devices.get(dev) else {
                continue; // 设备无指标 → fail-open
            };
            let committed = vram_committed.get(dev).copied().unwrap_or(0);
            let effective = ledger.used_live_mb.max(committed);
            if effective.saturating_add(cost) > ledger.total_mb {
                return false;
            }
        }
        true
    }
}

/// 管线引用的模块 id 列表（module 节点序，不去重——去重在调度器入队处）。
/// `pub`：force 提交响应的各池超支投影（[`forced_overcommit_pools`]）由
/// API handler 以同一口径提供模块表。
pub fn pipeline_module_ids(pipeline: &Pipeline) -> Vec<String> {
    pipeline
        .nodes
        .iter()
        .filter_map(|n| match &n.kind {
            NodeKind::Module { module_id, .. } => Some(module_id.clone()),
            _ => None,
        })
        .collect()
}

/// §6.8b force 提交的各池超支投影（T-22；async 只因准入快照读 state）。
///
/// 以**当前瞬时**的准入快照 + 调度账本推演本任务准入后的每池账目，返回
/// `max(实时占用, 已承诺) + 本任务预算 > 池容量` 的超支池（复用 T-20
/// [`vram::PoolBudget`] 形状：`committed_mb` = 准入后投影值，`entries` 以
/// 模块 id 记账——准入在模块粒度而非节点粒度）。返回空 = 无超支（force
/// 仅插队/跳过并发上限等待）。供提交响应的 `overcommit_pools` 字段消费
/// （仅 force 请求附带，缺省响应不出现该字段）；投影与实际准入之间存在
/// 竞态窗口，属提交瞬间的 advisory 信息。
pub async fn forced_overcommit_pools(
    state: &Arc<AppState>,
    module_ids: &[String],
) -> Vec<vram::PoolBudget> {
    let snapshot = admission_snapshot(state).await;
    let committed_now: HashMap<String, u32> = scheduler()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .vram_committed
        .clone();

    // 本任务按池归集预算（去重模块；与 admits() 同口径）
    let mut need: std::collections::BTreeMap<String, (u32, Vec<(String, u32)>)> =
        std::collections::BTreeMap::new();
    for mid in module_ids.iter().collect::<std::collections::BTreeSet<_>>() {
        if let Some(m) = snapshot.modules.get(mid) {
            if let (Some(dev), true) = (&m.device, m.cost_mb > 0) {
                let entry = need.entry(dev.clone()).or_insert_with(|| (0, Vec::new()));
                entry.0 = entry.0.saturating_add(m.cost_mb);
                entry.1.push((mid.clone(), m.cost_mb));
            }
        }
    }

    let mut pools = Vec::new();
    for (dev, (cost_total, entries)) in need {
        let Some(ledger) = snapshot.devices.get(&dev) else {
            continue; // 设备无指标 → fail-open，无超支可言
        };
        let committed = committed_now.get(&dev).copied().unwrap_or(0);
        let projected = ledger.used_live_mb.max(committed).saturating_add(cost_total);
        if projected <= ledger.total_mb {
            continue; // 未超支 → 不出池
        }
        pools.push(vram::PoolBudget {
            kind: if dev == UMA_POOL_KEY {
                vram::PoolKind::Uma
            } else {
                vram::PoolKind::Vram
            },
            device_id: dev,
            total_mb: u64::from(ledger.total_mb),
            committed_mb: u64::from(projected),
            entries: entries
                .into_iter()
                .map(|(module, cost)| vram::PoolItem {
                    node: module,
                    estimate_mb: u64::from(cost),
                })
                .collect(),
            unknown: false,
        });
    }
    pools
}

/// 构建准入快照（async：读 devices / process_manager / modules / config）。
async fn admission_snapshot(state: &Arc<AppState>) -> AdmissionSnapshot {
    let devices_snap = state.devices.read().await.clone();
    let instances: HashMap<String, DeviceId> = state
        .process_manager
        .read()
        .await
        .list_running()
        .into_iter()
        .filter_map(|i| i.device.clone().map(|d| (i.module_id.clone(), d)))
        .collect();
    let manifests: Vec<(String, ep_core::module::manifest::ModuleManifest)> = state
        .modules
        .read()
        .await
        .iter()
        .filter_map(|m| {
            m.manifest
                .as_ref()
                .map(|mf| (mf.module.id.clone(), mf.clone()))
        })
        .collect();

    let mut snap = AdmissionSnapshot::default();
    // T-22 UMA 单池：统一内存平台识别（探测失败 → None → 逐设备池，零差异）
    let uma_total_mb = ep_core::compute::uma::unified_memory_total_mb();
    // UMA 池实时占用：系统内存占用（CPU 条目的 used）优先——UMA 上 GPU 的
    // "显存" 就是系统内存；无 CPU 条目时回退 GPU 设备占用的最大值
    let mut uma_used_live_mb = 0u32;
    for d in &devices_snap {
        if matches!(d.id, DeviceId::Cpu) {
            uma_used_live_mb = uma_used_live_mb.max(d.used_memory_mb.unwrap_or(0));
        }
    }
    for d in &devices_snap {
        // CPU 不做显存记账（total = 系统内存，语义不同）；无总量指标的
        // 设备（vulkan/directml 常态）无法核算 → fail-open
        if matches!(d.id, DeviceId::Cpu) || d.total_memory_mb.is_none() {
            continue;
        }
        if let Some(uma_total) = uma_total_mb {
            // UMA 单池：所有 GPU 设备账本折叠为单一 "uma" 键（容量相同、
            // 取占用最大者）；个别设备条目不再独立记账
            uma_used_live_mb = uma_used_live_mb.max(d.used_memory_mb.unwrap_or(0));
            snap.devices.insert(
                UMA_POOL_KEY.to_string(),
                DeviceLedger {
                    total_mb: uma_total,
                    used_live_mb: uma_used_live_mb,
                },
            );
            continue;
        }
        snap.devices.insert(
            d.id.to_string(),
            DeviceLedger {
                total_mb: d.total_memory_mb.unwrap_or(0),
                used_live_mb: d.used_memory_mb.unwrap_or(0),
            },
        );
    }
    for (mid, mf) in manifests {
        let compute = &mf.compute;
        let cost_mb = compute
            .min_vram_mb
            .or(compute.vram_estimate_mb)
            .unwrap_or(0);
        let device = if cost_mb > 0 {
            match instances.get(&mid) {
                Some(d) => Some(d.to_string()),
                None => {
                    // 未运行：与 start_task 同一选择逻辑预测落位设备
                    let predicted = crate::api::select_module_device(state, &mf).await;
                    (!matches!(predicted, DeviceId::Cpu)).then(|| predicted.to_string())
                }
            }
        } else {
            None
        };
        // T-22 UMA 单池：GPU 落位设备的预算统一记入 "uma" 键（与上方设备
        // 账本折叠键一致；CPU/None 落位本就不设显存限，保持原状）
        let device = device.map(|dev| {
            if uma_total_mb.is_some() {
                UMA_POOL_KEY.to_string()
            } else {
                dev
            }
        });
        snap.modules.insert(
            mid,
            ModuleAdmission {
                device,
                cost_mb,
                max_concurrent: compute.max_concurrent.unwrap_or(1).max(1),
            },
        );
    }
    snap
}

/// 单任务出队资格判定（try_promote 每轮消费；纯函数可单测）：
/// 1. 管线级闸门（§6.8）：同一管线运行数 < max_instances；
/// 2. §6.8b 动态准入：模块并发上限 + 设备显存余额——**force 任务跳过本项**
///    （T-22：跳过的即是“动态准入排队等待”；静态管线闸门不受影响）。
fn entry_admissible(
    entry: &QueuedEntry,
    snapshot: &AdmissionSnapshot,
    pipeline_limit: usize,
    pipeline_running: usize,
    running_by_module: &HashMap<String, usize>,
    vram_committed: &HashMap<String, u32>,
) -> bool {
    if pipeline_running >= pipeline_limit {
        return false;
    }
    entry.force
        || snapshot.admits(&entry.module_ids, running_by_module, vram_committed)
}

/// T-15 出队选择：在通过两级闸门的排队任务中取排序键最小者
/// （force 插队 → 优先级升序 → 同优先级 FIFO，见 [`dispatch_rank`]）。
/// 准入判定本身不变，仅改变排序。
fn pick_next_index<F>(queue: &VecDeque<QueuedEntry>, mut eligible: F) -> Option<usize>
where
    F: FnMut(&QueuedEntry) -> bool,
{
    let mut best: Option<usize> = None;
    for (idx, entry) in queue.iter().enumerate() {
        if !eligible(entry) {
            continue;
        }
        match best {
            // 已选中者排序键更小（或同键且更靠前）→ 保持
            Some(best_idx) if dispatch_rank(&queue[best_idx], best_idx) <= dispatch_rank(entry, idx) => {}
            _ => best = Some(idx),
        }
    }
    best
}

async fn try_promote(state: Arc<AppState>) {
    let (max_parallel, timeout_secs) = {
        let cfg = state.config.read().await;
        (
            (cfg.pipeline.max_parallel as usize).max(1),
            cfg.pipeline.default_timeout_secs,
        )
    };

    loop {
        // 准入快照在调度锁外构建（async 读 state devices/instances/manifests）；
        // 判定为纯函数消费快照 + 调度器账本
        let snapshot = admission_snapshot(&state).await;
        let admitted = {
            let mut sched = scheduler().lock().unwrap_or_else(|e| e.into_inner());
            // T-15 全局暂停：只挡新任务派发，运行中任务不受影响
            if sched.paused {
                break;
            }
            if sched.running_count >= max_parallel {
                break;
            }
            let pick = pick_next_index(&sched.queue, |entry| {
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
                // 管线闸门 + §6.8b 动态准入（force 跳过后者，T-22）
                entry_admissible(
                    entry,
                    &snapshot,
                    limit,
                    running,
                    &sched.running_by_module,
                    &sched.vram_committed,
                )
            });
            match pick {
                Some(idx) => {
                    let entry = sched.queue.remove(idx).expect("idx just located");
                    sched.running_count += 1;
                    *sched
                        .running_by_pipeline
                        .entry(entry.pipeline_id.clone())
                        .or_insert(0) += 1;
                    // 动态准入记账：模块并发计数 + 设备显存承诺（终结时销账）
                    let mut plan: Vec<(String, u32)> = Vec::new();
                    for mid in &entry.module_ids {
                        *sched.running_by_module.entry(mid.clone()).or_insert(0) += 1;
                    }
                    for mid in &entry.module_ids {
                        if let Some(m) = snapshot.modules.get(mid) {
                            if let (Some(dev), true) = (&m.device, m.cost_mb > 0) {
                                if let Some(slot) =
                                    plan.iter_mut().find(|(d, _)| d == dev)
                                {
                                    slot.1 += m.cost_mb;
                                } else {
                                    plan.push((dev.clone(), m.cost_mb));
                                }
                            }
                        }
                    }
                    for (dev, cost) in &plan {
                        // 审查 P3-1：force 超支记账可超池总量，饱和累加防
                        // debug panic / release 环绕致账本错
                        let slot = sched.vram_committed.entry(dev.clone()).or_insert(0);
                        *slot = slot.saturating_add(*cost);
                    }
                    if !plan.is_empty() {
                        sched.vram_plans.insert(entry.task_id.clone(), plan);
                    }
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
        let module_activity_state = state_bg.module_activity.clone();
        // 暂存目录随任务记录传递（submit 时分配；终态由收尾路径清算）
        let staging_dir = registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&task_id)
            .and_then(|r| r.staging_dir.clone())
            .map(PathBuf::from);
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
                staging_dir,
                module_activity_state,
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
                let staging_root =
                    record.staging_dir.as_ref().map(std::path::PathBuf::from);
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
                        record.served_artifacts.insert(node_id.clone(), dest.clone());
                        // 暂存区产物：源随终态清算即删（tmpfs 生命周期），
                        // 记录路径改写为持久 files/ 副本，保证事后仍可定位；
                        // 盘上产物保持原路径不动——惰性归集（懒链接重建）
                        // 依赖原始源存在，语义不变。
                        let in_staging = staging_root
                            .as_ref()
                            .is_some_and(|r| src.starts_with(r));
                        if in_staging {
                            record.artifacts.insert(node_id, dest);
                        }
                    } else {
                        warn!(task_id, node_id = %node_id, "artifact collection failed; node artifact will not be downloadable");
                    }
                }
            }
        }) {
            warn!(task_id, error = %e, "failed to persist terminal task state");
        }
    }

    // T-16：直跑成功终态 → 写入内容寻址缓存；失败/取消任务仅丢弃上下文。
    if terminal == TaskState::Completed {
        store_direct_cache_if_needed(state, task_id).await;
    } else {
        cache_pending()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(task_id);
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

    // staging 清算（终态统一出口，无条件执行）：易失区的定位就是「随任务
    // 生灭」——归集完成后帧序列/节点中间产物不再有存在价值，而 tmpfs 驻留
    // 占用的是全机共享内存，必须即刻归还（与 keep_workspace 解耦：该开关
    // 仅管盘上工作目录的调试现场）。失败仅告警，不阻塞终态上报。
    state.staging.read().await.free_task(task_id).ok();

    // 空闲自动释放：任务终态刷新关联模块活跃时刻（空闲时钟自此重起）。
    // finalize_task 无 pipeline 上下文 → 从注册表记录的 module_ids 取。
    {
        let now = chrono::Utc::now().timestamp();
        let ids: Vec<String> = registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(task_id)
            .map(|r| r.module_ids.clone())
            .unwrap_or_default();
        let mut act = state
            .module_activity
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        for mid in ids {
            act.insert(mid, now);
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
        // §6.8b 动态准入销账：显存承诺按准入时计划精确回冲；模块并发计数
        // 按任务实际引用回冲（注册表终态记录仍携带 module_ids）
        if let Some(plan) = sched.vram_plans.remove(task_id) {
            for (dev, cost) in plan {
                if let Some(c) = sched.vram_committed.get_mut(&dev) {
                    *c = c.saturating_sub(cost);
                    if *c == 0 {
                        sched.vram_committed.remove(&dev);
                    }
                }
            }
        }
        let module_ids = registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(task_id)
            .map(|r| r.module_ids.clone())
            .unwrap_or_default();
        let mut seen = std::collections::BTreeSet::new();
        for mid in module_ids {
            if seen.insert(mid.clone()) {
                if let Some(n) = sched.running_by_module.get_mut(&mid) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        sched.running_by_module.remove(&mid);
                    }
                }
            }
        }
    }
    try_promote(state.clone()).await;

    // 完成回调（§6.5，best-effort）
    if let Some(url) = callback_url {
        fire_callback(task_id, &url);
    }

    // 统一事件日志（PLAN_TRIGGER_UNIFIED_LOG §5.7）：task_terminal 单点接入。
    // finalize_task 经 CAS 为唯一赢家出口，completed/failed/cancelled 三条
    // 终态路径在此各产生一条事件；错误摘要取注册表终态记录（Engine 失败
    // / 超时看门狗均已写入）；写失败仅告警，不阻断终态上报。
    crate::eventlog::write_task_terminal(
        &state.root,
        task_id,
        pipeline_id,
        terminal.as_str(),
        snapshot(task_id).and_then(|r| r.error).as_deref(),
    );

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
            let pipeline_id = reg.get(task_id).map(|r| r.pipeline_id.clone());
            let persisted = reg.update(task_id, |r| {
                if !r.status.is_terminal() {
                    r.status = TaskState::Cancelled;
                    r.finished_at = Some(Utc::now());
                }
                r.queue_position = None;
            });
            (pipeline_id, persisted)
        };
        match persisted {
            (_, Some(Err(e))) => warn!(task_id, error = %e, "failed to persist cancelled state"),
            (_, None) => return CancelOutcome::NotFound,
            // update 返回 Ok ⇒ 记录必在，pipeline_id 必为 Some（同源读取）
            (pipeline_id, Some(Ok(()))) => {
                // 评审修复：queued 分支直接改注册表、绕过 finalize 收尾——此处
                // 自行补 task_terminal 事件（与 finalize 路径同形状：status
                // ="cancelled"、error 缺省；写失败仅告警）。事件先于回调，
                // 与 finalize 顺序一致。
                if let Some(pid) = pipeline_id {
                    crate::eventlog::write_task_terminal(
                        &state.root,
                        task_id,
                        &pid,
                        TaskState::Cancelled.as_str(),
                        None,
                    );
                }
            }
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
                    priority: DEFAULT_TASK_PRIORITY,
                    started_at: Utc::now(),
                    started_running_at: None,
                    finished_at: None,
                    node_order: Vec::new(),
                    nodes: HashMap::new(),
                    artifacts: HashMap::new(),
                    served_artifacts: HashMap::new(),
                    work_dir: PathBuf::new(),
                    staging_dir: None,
                    module_ids: Vec::new(),
                    group: None,
                    cached_from: None,
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
    // module_id → 显式设备指定（D-Device）：取该模块首个 Module 节点的
    // device 字段（软约束字符串，auto/空 = None）。直跑退化 DAG 的 run
    // 节点携带快速调用请求的 device hint；普通管线节点未设即 auto。
    let mut module_devices: Vec<(&str, Option<&str>)> = Vec::new();
    for node in &pipeline.nodes {
        if let NodeKind::Module {
            module_id,
            device,
            ..
        } = &node.kind
        {
            if let Some((_, existing)) =
                module_devices.iter_mut().find(|(id, _)| *id == module_id.as_str())
            {
                if existing.is_none() {
                    *existing = device.as_deref();
                }
            } else {
                module_devices.push((module_id.as_str(), device.as_deref()));
            }
        }
    }
    for (module_id, device) in module_devices {
        ensure_module_running_on_device(state, module_id, device)
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
/// 同 [`ensure_module_running_on_device`]（自动拉起权威实现见
/// `api/autostart.rs`）——管线侧（ensure_pipeline_modules）调用了带设备
/// 指定的版本；本 wrapper 供无 hint 的语义直接复用（保留文档锚点）。
pub async fn ensure_module_running_on_device(
    state: &Arc<AppState>,
    module_id: &str,
    device_hint: Option<&str>,
) -> anyhow::Result<()> {
    crate::api::autostart::ensure_module_running_on_device(state, module_id, device_hint)
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
        None,
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
    // 显式设备指定（D-Device；None/auto = 跟随策略）
    device_hint: Option<&str>,
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

    // 2.5 直跑缓存（T-16，§6.8 快速调用加速）：键 = 模块 + 能力 + 规范化
    //     参数 + 输入指纹（≤512MB 用内容 sha256，超限退化为 size+mtime）。
    //     命中即构造 completed 任务并直接返回，不再占用执行闸门。
    let cache_submission = match resolve_direct_cache(
        state,
        module_id,
        capability,
        &params,
        &input_path,
        &options,
    )
    .await
    {
        DirectCacheDecision::Hit(outcome) => return Ok(*outcome),
        DirectCacheDecision::Miss(submission) => submission,
    };

    // 3. 退化三节点 DAG → 同一提交路径（闸门/注册表/WS/产物全套复用）
    let pipeline = build_direct_pipeline(
        module_id,
        capability,
        params,
        &input_path,
        Some(&capability_decl),
        device_hint,
    );
    submit_pipeline_full_inner(state, pipeline, None, options, cache_submission).await
}

// ─── 直跑结果内容寻址缓存（T-16，§6.8 快速调用加速）───────────────────────

/// 直跑缓存决策：命中直接返回完成态任务；未命中时携带写入上下文。
enum DirectCacheDecision {
    /// Box 包裹避免枚举因 `SubmitOutcome`（含完整 TaskRecord）过大而膨胀
    Hit(Box<SubmitOutcome>),
    Miss(Option<CacheSubmission>),
}

/// 解析直跑缓存：总开关/旁路/指纹/键 → 命中则构造完成态任务。
///
/// 指纹或缓存读取异常均降级为“未命中”（告警后照常执行），绝不让缓存
/// 故障阻断提交。
async fn resolve_direct_cache(
    state: &Arc<AppState>,
    module_id: &str,
    capability: &str,
    params: &Value,
    input_path: &Path,
    options: &SubmitOptions,
) -> DirectCacheDecision {
    let (enabled, workspace) = {
        let cfg = state.config.read().await;
        (
            cfg.cache.enabled,
            cfg.resolve_workspace_dir(&state.root),
        )
    };
    if !enabled || options.bypass_cache {
        return DirectCacheDecision::Miss(None);
    }

    let fingerprint = match ep_core::cache::fingerprint_input(input_path) {
        Ok(fingerprint) => fingerprint,
        Err(err) => {
            warn!(
                module_id,
                input = %input_path.display(),
                error = %err,
                "直跑缓存指纹计算失败，本次跳过缓存"
            );
            return DirectCacheDecision::Miss(None);
        }
    };
    let key = ep_core::cache::cache_key(module_id, capability, params, &fingerprint);
    let cache_root = workspace.join("cache");
    if let Some(meta) = ep_core::cache::lookup(&cache_root, &key) {
        if let Some(outcome) = build_cache_hit_outcome(
            state, &workspace, &cache_root, &key, &meta, module_id, options,
        )
        .await
        {
            return DirectCacheDecision::Hit(Box::new(outcome));
        }
    }
    DirectCacheDecision::Miss(Some(CacheSubmission {
        key,
        module_id: module_id.to_string(),
        capability: capability.to_string(),
        params: params.clone(),
    }))
}

/// 缓存命中：构造 `completed` 任务（产物硬链接进任务 `files/`），写事件
/// 日志并按需触发完成回调；恢复失败返回 None（调用方降级为真实执行）。
async fn build_cache_hit_outcome(
    state: &Arc<AppState>,
    workspace: &Path,
    cache_root: &Path,
    key: &CacheKey,
    meta: &CacheMeta,
    module_id: &str,
    options: &SubmitOptions,
) -> Option<SubmitOutcome> {
    let task_id = new_task_id();
    let task_dir = workspace.join("tasks").join(&task_id);
    let restored = match ep_core::cache::restore_into_task(cache_root, key, &task_dir) {
        Ok(restored) => restored,
        Err(err) => {
            warn!(
                task_id = %task_id,
                error = %err,
                "直跑缓存产物恢复失败，降级为真实执行"
            );
            return None;
        }
    };

    let now = Utc::now();
    let mut artifacts: HashMap<String, PathBuf> = HashMap::new();
    let mut served_artifacts: HashMap<String, PathBuf> = HashMap::new();
    for (node_id, path) in &restored {
        artifacts.insert(node_id.clone(), path.clone());
        served_artifacts.insert(node_id.clone(), path.clone());
    }
    let node_order = if meta.node_order.is_empty() {
        restored
            .iter()
            .map(|(node_id, _)| node_id.clone())
            .collect::<Vec<_>>()
    } else {
        meta.node_order.clone()
    };
    let nodes = node_order
        .iter()
        .map(|node_id| {
            (
                node_id.clone(),
                NodeRecord {
                    state: "completed".to_string(),
                    error: None,
                },
            )
        })
        .collect();
    let pipeline_id = format!("direct/{module_id}");

    let record = TaskRecord {
        id: task_id.clone(),
        pipeline_id: pipeline_id.clone(),
        status: TaskState::Completed,
        error: None,
        queue_position: None,
        priority: options.priority.unwrap_or(DEFAULT_TASK_PRIORITY),
        started_at: now,
        started_running_at: Some(now),
        finished_at: Some(now),
        node_order,
        nodes,
        artifacts,
        served_artifacts,
        work_dir: task_dir,
        staging_dir: None,
        module_ids: vec![module_id.to_string()],
        group: options.group.clone(),
        cached_from: Some(meta.source_task_id.clone()),
    };
    // 落盘失败不阻断命中：产物已就位，记录保留在内存即可服务下载
    if let Err(err) = registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(record)
    {
        warn!(task_id = %task_id, error = %err, "缓存命中任务记录落盘失败，仅保留内存副本");
    }

    crate::eventlog::write_task_terminal(
        &state.root,
        &task_id,
        &pipeline_id,
        TaskState::Completed.as_str(),
        None,
    );
    if let Some(callback_url) = options.callback_url.as_deref() {
        fire_callback(&task_id, callback_url);
    }
    info!(
        task_id = %task_id,
        module_id,
        cached_from = %meta.source_task_id,
        "直跑缓存命中：任务直接完成（复用历史产物）"
    );

    let record = if options.wait {
        snapshot(&task_id)
    } else {
        None
    };
    Some(SubmitOutcome { task_id, record })
}

/// 任务成功终态后把直跑产物写入缓存（T-16）；旁路/总开关关闭/产物缺失
/// 均静默跳过，任何异常只告警，不影响任务终态。
async fn store_direct_cache_if_needed(state: &Arc<AppState>, task_id: &str) {
    let Some(submission) = cache_pending()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(task_id)
    else {
        return;
    };

    let (enabled, max_gb, workspace) = {
        let cfg = state.config.read().await;
        (
            cfg.cache.enabled,
            cfg.cache.max_gb,
            cfg.resolve_workspace_dir(&state.root),
        )
    };
    if !enabled {
        return;
    }

    let Some(record) = snapshot(task_id) else {
        warn!(task_id, "缓存写入跳过：任务记录缺失");
        return;
    };
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for node_id in &record.node_order {
        let path = record
            .served_artifacts
            .get(node_id)
            .or_else(|| record.artifacts.get(node_id));
        if let Some(path) = path {
            let path = path.clone();
            if path.is_file() {
                files.push((node_id.clone(), path));
            }
        }
    }
    if files.is_empty() {
        debug!(task_id, "缓存写入跳过：任务没有可复用产物");
        return;
    }

    let cache_root = workspace.join("cache");
    // max_gb = 0 → max_bytes = 0 → cache::store 内部按“不限”处理（仅告警）
    let max_bytes = u64::from(max_gb) * 1024 * 1024 * 1024;
    let node_order = record.node_order.clone();
    let source_task_id = task_id.to_string();
    let written = tokio::task::spawn_blocking(move || {
        let request = ep_core::cache::StoreRequest {
            key: &submission.key,
            module_id: &submission.module_id,
            capability: &submission.capability,
            params: &submission.params,
            source_task_id: &source_task_id,
            node_order: &node_order,
            files: &files,
        };
        ep_core::cache::store(&cache_root, &request, max_bytes)
    })
    .await;
    match written {
        Ok(Ok(meta)) => info!(
            task_id,
            cache_key = %meta.key,
            files = meta.files.len(),
            total_bytes = meta.total_bytes,
            "直跑产物已写入缓存"
        ),
        Ok(Err(err)) => warn!(task_id, error = %err, "直跑缓存写入失败（任务结果不受影响）"),
        Err(err) => warn!(task_id, error = %err, "直跑缓存写入任务异常（任务结果不受影响）"),
    }
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
    device_hint: Option<&str>,
) -> Pipeline {
    // 显式设备指定（D-Device）：写入 run 节点 device 字段——任务内安全网
    // 拉起（ensure_pipeline_modules）据此固定本次启动设备，且任务详情可追溯
    let device_hint = device_hint
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto"))
        .map(str::to_string);
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
    // JSON/Text 输出能力（如 ASR transcribe → json）：默认产物是内联数据，
    // file_output 无文件输入必失败（Linux 真机 E2E 实测暴露）→ 省略 output
    // 节点，内联产物由执行回调物化为文件后随产物归集可下载（见
    // run_pipeline_blocking 的 on_node_complete）。例外：请求显式携带
    // output_format（非 json，如 srt/txt）时执行器注入 output_path、模块
    // 落真实文件，保持三节点形态。未知能力（无声明）亦保持原三节点形态。
    let explicit_file_format = params
        .get("output_format")
        .and_then(|v| v.as_str())
        .map(|s| {
            let t = s.trim();
            !t.is_empty() && t != "json"
        })
        .unwrap_or(false);
    let output_is_file = capability_decl
        .map(|c| {
            !matches!(c.output_type, DataType::Json | DataType::Text) || explicit_file_format
        })
        .unwrap_or(true);
    let mut nodes = vec![
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
                device: device_hint,
            },
            "模块执行",
            params,
        ),
    ];
    let mut edges = vec![ep_core::pipeline::dag::Edge {
        from: ("input".to_string(), "output".to_string()),
        to: ("run".to_string(), "input".to_string()),
    }];
    if output_is_file {
        nodes.push(make_node(
            "output",
            NodeKind::Builtin {
                builtin: "file_output".to_string(),
            },
            "结果输出",
            output_params,
        ));
        edges.push(ep_core::pipeline::dag::Edge {
            from: ("run".to_string(), "output".to_string()),
            to: ("output".to_string(), "input".to_string()),
        });
    }
    Pipeline {
        // B4 契约：direct/<module_id> 形状（同模块直跑任务聚合，前端可过滤）
        id: format!("direct/{module_id}"),
        name: format!("直跑 {module_id}/{capability}"),
        description: "单模型直跑任务（§5.3 退化三节点 DAG）".to_string(),
        nodes,
        edges,
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
    staging_dir: Option<PathBuf>,
    module_activity: Arc<std::sync::Mutex<HashMap<String, i64>>>,
) -> (anyhow::Result<()>, Option<TaskDetail>) {
    #[cfg(test)]
    run_test_hook(&task_id);

    let pipeline_id = pipeline.id.clone();

    let mut runner = PipelineRunnerImpl::new(task_dir.clone());
    runner.staging_dir = staging_dir;
    runner.set_module_ports(module_ports);

    // 节点级模块活跃触点：长任务期间持续刷新，空闲回收器不会误杀在跑模块。
    // 节点开始/完成各刷一次（单超长节点也有起点基准）。
    let node_module_of: HashMap<String, String> = pipeline
        .nodes
        .iter()
        .filter_map(|n| match &n.kind {
            NodeKind::Module { module_id, .. } => Some((n.id.clone(), module_id.clone())),
            _ => None,
        })
        .collect();
    runner.set_cancel_flag(cancel_flag);
    runner.set_default_node_timeout(default_node_timeout);

    // 回调：节点开始 → running（P2-7：携带 task_id）
    {
        let tx = progress_tx.clone();
        let pid = pipeline_id.clone();
        let tid = task_id.clone();
        let act = activity_ms.clone();
        let node_module_of = node_module_of.clone();
        let module_activity = module_activity.clone();
        runner.on_node_start = Some(Arc::new(move |node_id| {
            act.store(now_epoch_ms(), Ordering::SeqCst);
            let _ = tx.send(ProgressMessage {
                pipeline_id: pid.clone(),
                task_id: tid.clone(),
                node_id: node_id.to_string(),
                status: "running".to_string(),
            });
            set_node_state(&tid, node_id, "running", None);
            if let Some(mid) = node_module_of.get(node_id) {
                let now = chrono::Utc::now().timestamp();
                module_activity
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(mid.clone(), now);
            }
        }));
    }
    // 回调：节点完成 → completed + 记录产物
    {
        let tx = progress_tx.clone();
        let pid = pipeline_id.clone();
        let tid = task_id.clone();
        let act = activity_ms.clone();
        let tdir = task_dir.clone();
        let node_module_complete = node_module_of.clone();
        let module_activity_complete = module_activity.clone();
        runner.on_node_complete = Some(Arc::new(move |node_id, artifact| {
            act.store(now_epoch_ms(), Ordering::SeqCst);
            if let Some(mid) = node_module_complete.get(node_id) {
                let now = chrono::Utc::now().timestamp();
                module_activity_complete
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(mid.clone(), now);
            }
            let _ = tx.send(ProgressMessage {
                pipeline_id: pid.clone(),
                task_id: tid.clone(),
                node_id: node_id.to_string(),
                status: "completed".to_string(),
            });
            set_node_state(&tid, node_id, "completed", None);
            match artifact {
                Artifact::File(path) => record_artifact(&tid, node_id, path),
                // 内联产物物化为任务目录内文件，随产物归集进入 files/ 可下载
                //（直跑 JSON/Text 输出能力省略 file_output 节点后的产物通道）
                Artifact::Json(value) => {
                    // 目录型产物（抽帧序列）以 manifest 返回（T-13 P2-2）：
                    // 逐个文件登记为可下载产物，而不是把清单本身物化成 JSON
                    // （目录内文件才可下载，size 才有意义）。
                    if value.get("kind").and_then(Value::as_str)
                        == Some(builtin_templates::FRAME_MANIFEST_KIND)
                    {
                        let count = record_expanded_frame_artifacts(&tid, node_id, value);
                        if count == 0 {
                            warn!(
                                task_id = %tid,
                                node_id,
                                "frame manifest contained no usable file; result will not be downloadable"
                            );
                        }
                    } else {
                        let dest = tdir.join(format!("{node_id}_output.json"));
                        let written = serde_json::to_string_pretty(value)
                            .ok()
                            .is_some_and(|s| std::fs::write(&dest, s).is_ok());
                        if written {
                            record_artifact(&tid, node_id, &dest);
                        } else {
                            warn!(
                                task_id = %tid,
                                node_id,
                                "failed to materialize JSON artifact; result will not be downloadable"
                            );
                        }
                    }
                }
                Artifact::Text(text) => {
                    let dest = tdir.join(format!("{node_id}_output.txt"));
                    if std::fs::write(&dest, text).is_ok() {
                        record_artifact(&tid, node_id, &dest);
                    } else {
                        warn!(
                            task_id = %tid,
                            node_id,
                            "failed to materialize text artifact; result will not be downloadable"
                        );
                    }
                }
                // W0-A 兼容臂（ep-core Artifact::None 新变体，§5.6）：gate 无匹配
                // 输出的哨兵产物，不物化任何文件；仅维持 match 穷尽性，不改逻辑。
                Artifact::None => {}
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
        // T-25 产物登记（execution.rs 唯一挂钩）：watcher 触发任务的产物
        // 落盘即登记进 watcher.rs 产物登记表，扫描侧据此排除——系统产物永不
        // 自我触发（watch 目录 = 产物目录场景不再死循环）。两类来源：
        // - 直调任务：pipeline_id = "watcher/<rule_id>"，仅 archive 节点；
        // - 管线模式规则：提交时经 watcher::remember_watcher_pipeline_task
        //   记下 task_id → rule_id（审查 P1-1；管线产物节点 id 任意，全部
        //   登记；映射仅进程内存存活，任务不跨重启运行故无需持久化）。
        // 非 watcher 任务不登记。skip 冲突透传的既有目标路径同样登记（无害）。
        let processed_rule_id = if node_id == "archive" && record.pipeline_id.starts_with("watcher/")
        {
            Some(record.pipeline_id["watcher/".len()..].to_string())
        } else {
            crate::watcher::watcher_pipeline_task_rule(task_id)
        };
        if let Some(rule_id) = processed_rule_id {
            drop(reg); // 先放注册表锁再做磁盘 IO
            crate::watcher::record_processed_product(path, &rule_id, task_id);
        }
    }
}

/// 目录型产物 manifest → `(合成产物 id, 绝对路径)` 列表。
///
/// 合成 id 约定：第一个文件沿用真实 `node_id`（前端既有下载/预览路径与
/// UI 展示不变），其余为 `{node_id}__{文件名}`。只登记真实存在的普通文件。
fn expanded_frame_entries(node_id: &str, manifest: &Value) -> Vec<(String, PathBuf)> {
    let Some(files) = manifest.get("files").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    for file in files
        .iter()
        .take(builtin_templates::MAX_EXPANDED_FRAME_FILES)
    {
        let Some(raw_path) = file.get("path").and_then(Value::as_str) else {
            continue;
        };
        let path = PathBuf::from(raw_path);
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let mut artifact_id = if entries.is_empty() {
            node_id.to_string()
        } else {
            format!("{node_id}__{name}")
        };
        // 重名防御：同一目录内文件名理论上唯一，冲突时追加序号
        let mut seq = 2;
        while used.contains(&artifact_id) {
            artifact_id = format!("{node_id}__{seq}__{name}");
            seq += 1;
        }
        used.insert(artifact_id.clone());
        entries.push((artifact_id, path));
    }
    entries
}

/// 把目录型产物展开登记进任务记录，返回登记数量（记录缺失时为 0）。
fn record_expanded_frame_artifacts(task_id: &str, node_id: &str, manifest: &Value) -> usize {
    let entries = expanded_frame_entries(node_id, manifest);
    if entries.is_empty() {
        return 0;
    }
    let count = entries.len();
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    let Some(record) = reg.get_mut(task_id) else {
        return 0;
    };
    for (artifact_id, path) in entries {
        record.artifacts.insert(artifact_id, path);
    }
    count
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
    cache_pending().lock().unwrap_or_else(|e| e.into_inner()).clear();
    extras().lock().unwrap_or_else(|e| e.into_inner()).clear();
    finalized().lock().unwrap_or_else(|e| e.into_inner()).clear();
    forced_tasks().lock().unwrap_or_else(|e| e.into_inner()).clear();
    set_test_run_hook(None);
}

/// 测试专用：直接写入一条任务记录（产物列表/下载展开等 handler 级夹具）。
#[cfg(test)]
pub fn insert_record_for_tests(record: TaskRecord) {
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Err(err) = reg.insert(record) {
        panic!("insert test task record failed: {err}");
    }
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

    /// 构造一条 completed 任务记录夹具（产物展开相关 handler 级测试用）
    fn sample_completed_record(
        id: &str,
        work_dir: PathBuf,
        node_id: &str,
        artifacts: HashMap<String, PathBuf>,
    ) -> TaskRecord {
        let mut nodes = HashMap::new();
        nodes.insert(
            node_id.to_string(),
            NodeRecord {
                state: "completed".to_string(),
                error: None,
            },
        );
        TaskRecord {
            id: id.to_string(),
            pipeline_id: "test-pipeline".to_string(),
            status: TaskState::Completed,
            error: None,
            queue_position: None,
            priority: DEFAULT_TASK_PRIORITY,
            started_at: Utc::now(),
            started_running_at: None,
            finished_at: None,
            node_order: vec![node_id.to_string()],
            nodes,
            artifacts,
            served_artifacts: HashMap::new(),
            work_dir,
            staging_dir: None,
            module_ids: Vec::new(),
            group: None,
            cached_from: None,
        }
    }

    // ── T-13 P2-2：目录型产物（抽帧）展开为可下载文件 ──────────────────────

    #[test]
    fn expanded_frame_entries_map_first_to_node_id_then_prefixed_keys() {
        let root = unique_root("frames-entries");
        let dir = root.join("ef_frames");
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("frame_0001.png");
        let second = dir.join("frame_0002.png");
        std::fs::write(&first, b"a").unwrap();
        std::fs::write(&second, b"bb").unwrap();
        // manifest 中夹带一个已不存在的文件：展开时应跳过
        let manifest = builtin_templates::frame_directory_manifest(
            &dir,
            &[first.clone(), second, dir.join("missing.png")],
        );

        let entries = expanded_frame_entries("ef", &manifest);
        assert_eq!(entries.len(), 2);
        // 首个文件沿用真实 node_id，其余 `{node_id}__{文件名}`
        assert_eq!(entries[0].0, "ef");
        assert_eq!(entries[1].0, "ef__frame_0002.png");
        assert_eq!(entries[0].1, first);
    }

    #[test]
    fn record_expanded_frame_artifacts_registers_downloadable_files() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();
        let root = unique_root("frames-registry");
        let dir = root.join("ef_frames");
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("frame_0001.png");
        let second = dir.join("frame_0002.png");
        std::fs::write(&first, b"a").unwrap();
        std::fs::write(&second, b"bb").unwrap();
        let manifest = builtin_templates::frame_directory_manifest(&dir, &[first.clone(), second]);

        let record = sample_completed_record(
            "t-frames",
            root.join("tasks").join("t-frames"),
            "ef",
            HashMap::new(),
        );
        insert_record_for_tests(record);

        let count = record_expanded_frame_artifacts("t-frames", "ef", &manifest);
        assert_eq!(count, 2);
        let rec = snapshot("t-frames").expect("record must exist");
        assert_eq!(rec.artifacts.len(), 2);
        assert_eq!(rec.artifacts.get("ef"), Some(&first));
        assert!(rec.artifacts.contains_key("ef__frame_0002.png"));
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
            priority: DEFAULT_TASK_PRIORITY,
            started_at: now,
            started_running_at: None,
            finished_at: Some(now),
            node_order: Vec::new(),
            nodes: HashMap::new(),
            artifacts: HashMap::new(),
            served_artifacts: HashMap::new(),
            work_dir: PathBuf::new(),
            staging_dir: None,
            module_ids: Vec::new(),
        group: None,
        cached_from: None,
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

    // ── 10b. 优先级出队与全局暂停/恢复（T-15） ──────────────────────────────

    fn queued_entry(task_id: &str, priority: u8) -> QueuedEntry {
        queued_entry_force(task_id, priority, false)
    }

    /// T-22：带 force 标记的排队条目夹具
    fn queued_entry_force(task_id: &str, priority: u8, force: bool) -> QueuedEntry {
        QueuedEntry {
            task_id: task_id.to_string(),
            pipeline_id: "p".to_string(),
            priority,
            module_ids: Vec::new(),
            force,
        }
    }

    #[test]
    fn pick_next_index_prefers_highest_priority_then_fifo() {
        // 高优先级任务排在后面 → 仍应被选中（数值越小越先出队）
        let mut queue = VecDeque::from([
            queued_entry("normal-1", 50),
            queued_entry("normal-2", 50),
            queued_entry("high", 10),
        ]);
        assert_eq!(pick_next_index(&queue, |_| true), Some(2));

        // 同优先级 → FIFO（取最靠前者）
        queue.remove(2);
        assert_eq!(pick_next_index(&queue, |_| true), Some(0));

        // 准入过滤：队首不可准入时跳过，取下一个可准入的
        assert_eq!(
            pick_next_index(&queue, |e| e.task_id != "normal-1"),
            Some(1)
        );

        // 全部不可准入 → 不派发
        assert_eq!(pick_next_index(&queue, |_| false), None);
    }

    #[test]
    fn queued_position_reflects_priority_order() {
        let sched = Scheduler {
            queue: VecDeque::from([
                queued_entry("normal-1", 50),
                queued_entry("normal-2", 50),
                queued_entry("high", 10),
            ]),
            ..Scheduler::default()
        };
        // 队列物理序仍是 FIFO，但展示序位按出队规则（优先级升序、同级 FIFO）编号
        assert_eq!(queued_position_in(&sched, "high"), Some(1));
        assert_eq!(queued_position_in(&sched, "normal-1"), Some(2));
        assert_eq!(queued_position_in(&sched, "normal-2"), Some(3));
        assert_eq!(queued_position_in(&sched, "ghost"), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pause_queue_holds_dispatch_and_resume_drains() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("pause");
        let state = test_state(root.clone());
        let src = root.join("pause-src.txt");
        let dest = root.join("pause-out.txt");
        std::fs::write(&src, "pause").unwrap();

        let paused = pause_queue();
        assert!(paused.paused, "暂停后状态应为 paused");

        let outcome = submit_pipeline_full(
            &state,
            copy_pipeline("pause-pipe", &src, &dest),
            None,
            SubmitOptions {
                wait: false,
                callback_url: None,
                group: None,
                priority: Some(10),
                bypass_cache: false,
                force: false,
            },
        )
        .await
        .unwrap();

        // 暂停期间不派发：任务保持 queued（不产生运行）
        tokio::time::sleep(Duration::from_millis(400)).await;
        let record = snapshot(&outcome.task_id).unwrap();
        assert_eq!(record.status, TaskState::Queued, "暂停后不应派发新任务");
        assert_eq!(record.priority, 10, "提交优先级应写入任务记录");
        let status = queue_status();
        assert!(status.paused);
        assert_eq!(status.queued, 1);
        assert_eq!(status.running, 0);

        // 恢复后立即派发并完成
        let resumed = resume_queue(&state);
        assert!(!resumed.paused, "恢复后应重新派发");
        let done = wait_terminal(&outcome.task_id)
            .await
            .expect("恢复后任务应终结");
        assert_eq!(done.status, TaskState::Completed);
        assert!(dest.exists(), "恢复后任务应产出结果文件");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn high_priority_task_overtakes_earlier_normal_task() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("priority-order");
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

        // 记录实际执行顺序；每任务持闸 150ms，保证另一任务在排队可见
        let order: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let order_hook = order.clone();
        set_test_run_hook(Some(Arc::new(move |task_id| {
            order_hook
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(task_id.to_string());
            std::thread::sleep(Duration::from_millis(150));
        })));

        // 暂停队列先累积两个任务：normal 先入队、high 后入队
        pause_queue();
        let mut task_ids = Vec::new();
        for (priority, label) in [(50u8, "normal"), (10u8, "high")] {
            let src = root.join(format!("prio-src-{label}.txt"));
            let dest = root.join(format!("prio-out-{label}.txt"));
            std::fs::write(&src, "prio").unwrap();
            let outcome = submit_pipeline_full(
                &state,
                copy_pipeline(&format!("prio-{label}"), &src, &dest),
                None,
                SubmitOptions {
                    wait: false,
                    callback_url: None,
                    group: None,
                    priority: Some(priority),
                    bypass_cache: false,
                    force: false,
                },
            )
            .await
            .unwrap();
            task_ids.push(outcome.task_id);
        }

        // 展示序位按优先级：high=1、normal=2（尽管 normal 先入队）
        assert_eq!(snapshot(&task_ids[1]).unwrap().queue_position, Some(1));
        assert_eq!(snapshot(&task_ids[0]).unwrap().queue_position, Some(2));

        resume_queue(&state);
        for id in &task_ids {
            wait_terminal(id).await.expect("任务应终结");
        }
        set_test_run_hook(None);

        let started = order.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(started.len(), 2);
        assert_eq!(started[0], task_ids[1], "高优先级任务应先被执行");
        assert_eq!(started[1], task_ids[0], "槽位释放后执行普通任务");
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

        // 评审修复：queued 取消绕过 finalize 收尾，也须补写 task_terminal 事件
        //（与 finalize 路径同形状：status="cancelled"）
        let events = crate::eventlog::read_events(&root, None, Some("task_terminal"), 100);
        let term = events
            .iter()
            .find(|e| e["task_id"] == t2.as_str())
            .expect("queued 取消应写 task_terminal 事件");
        assert_eq!(term["type"], "task_terminal");
        assert_eq!(term["status"], "cancelled");
        assert_eq!(term["pipeline_id"], "cq-2");

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
                group: None,
                priority: None,
                bypass_cache: false,
                force: false,
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
                group: None,
                priority: None,
                bypass_cache: false,
                force: false,
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
            None,
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
            None,
        );
        assert_eq!(p.nodes[2].params["extension"], "srt");
        // 同能力不传 output_format → json 内联输出：省略 file_output 节点，
        // 退化为两节点 DAG（产物由执行回调物化为 .json，Linux E2E 修复）
        let p = build_direct_pipeline(
            "faster-whisper",
            "transcribe",
            serde_json::json!({}),
            Path::new("/tmp/speech.wav"),
            Some(&asr),
            None,
        );
        assert_eq!(p.nodes.len(), 2, "json 输出不应带 file_output 节点");
        assert_eq!(p.edges.len(), 1);

        // rembg image→image 同格式回归：产物仍随输入 .png，不被映射改写
        let rembg = cap_decl(DataType::Image, DataType::Image);
        let p = build_direct_pipeline(
            "rembg",
            "remove_bg",
            serde_json::json!({}),
            Path::new("/tmp/photo.png"),
            Some(&rembg),
            None,
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
            None,
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
            None,
            SubmitOptions {
                wait: true,
                callback_url: None,
                group: None,
                priority: None,
                bypass_cache: false,
                force: false,
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
            None,
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

    // ── 19.2 直跑缓存（T-16）：命中 / bypass / disabled ─────────────────────

    /// 构造 mock-asr 模块（无 venv：真实执行必失败；缓存命中路径无需执行）
    fn mock_asr_module(root: &Path) -> ep_core::module::discovery::DiscoveredModule {
        use ep_core::module::manifest::ModuleManifest;

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
        ep_core::module::discovery::DiscoveredModule {
            manifest: Some(manifest),
            path: root.join("modules/mock-asr"),
            status: ep_core::module::discovery::DiscoveryStatus::Valid,
        }
    }

    /// 预置一条“mock-asr/transcribe + 当前输入文件”的缓存条目
    async fn seed_direct_cache(
        state: &Arc<AppState>,
        input: &Path,
        params: &Value,
    ) -> ep_core::cache::CacheKey {
        let workspace = state
            .config
            .read()
            .await
            .resolve_workspace_dir(&state.root);
        let cache_root = workspace.join("cache");
        std::fs::create_dir_all(&workspace).unwrap();
        let artifact = workspace.join("cached-transcript.json");
        std::fs::write(&artifact, r#"{"text":"cached-transcript"}"#).unwrap();
        let fingerprint = ep_core::cache::fingerprint_input(input).unwrap();
        let key = ep_core::cache::cache_key("mock-asr", "transcribe", params, &fingerprint);
        let node_order = vec!["run".to_string()];
        let files = vec![("run".to_string(), artifact)];
        ep_core::cache::store(
            &cache_root,
            &ep_core::cache::StoreRequest {
                key: &key,
                module_id: "mock-asr",
                capability: "transcribe",
                params,
                source_task_id: "task-origin",
                node_order: &node_order,
                files: &files,
            },
            0,
        )
        .unwrap();
        key
    }

    #[tokio::test]
    async fn direct_cache_hit_returns_completed_task_with_artifacts() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("cache-hit");
        let module = mock_asr_module(&root);
        let state = Arc::new(AppState::new(
            root.clone(),
            AppConfig::default(),
            vec![],
            vec![module],
            PortManager::new(18000, 19000),
        ));
        let input = root.join("cache-hit-in.txt");
        std::fs::write(&input, "cache hit input").unwrap();
        let params = json!({});

        seed_direct_cache(&state, &input, &params).await;

        let outcome = submit_direct_full(
            &state,
            "mock-asr",
            "transcribe",
            params,
            input,
            None,
            SubmitOptions {
                wait: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let record = outcome.record.expect("wait 模式必须携带记录");
        assert_eq!(record.status, TaskState::Completed, "缓存命中应直接完成");
        assert_eq!(record.cached_from.as_deref(), Some("task-origin"));
        assert!(record.pipeline_id.starts_with("direct/"));
        let served = record
            .served_artifacts
            .get("run")
            .expect("命中任务应登记 served artifact");
        assert!(served.is_file(), "产物应硬链接进任务 files/");
        assert!(
            served.starts_with(record.work_dir.join("files")),
            "产物必须落在任务可下载目录内"
        );
    }

    #[tokio::test]
    async fn direct_cache_bypass_and_disabled_skip_hit() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("cache-bypass");
        let module = mock_asr_module(&root);
        let state = Arc::new(AppState::new(
            root.clone(),
            AppConfig::default(),
            vec![],
            vec![module],
            PortManager::new(18000, 19000),
        ));
        let input = root.join("cache-bypass-in.txt");
        std::fs::write(&input, "cache bypass input").unwrap();
        let params = json!({});
        seed_direct_cache(&state, &input, &params).await;

        // bypass_cache=true：跳过读取，真实执行（本环境必失败）且不写缓存
        let outcome = submit_direct_full(
            &state,
            "mock-asr",
            "transcribe",
            params.clone(),
            input.clone(),
            None,
            SubmitOptions {
                wait: true,
                bypass_cache: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let record = outcome.record.expect("wait 模式必须携带记录");
        assert_ne!(record.status, TaskState::Completed, "旁路后不应命中缓存");
        assert!(record.cached_from.is_none());
        assert!(
            cache_pending()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty(),
            "旁路任务不应登记缓存写入上下文"
        );

        // 总开关关闭：即使缓存有条目也不得命中，且不登记写入上下文
        state.config.write().await.cache.enabled = false;
        let outcome = submit_direct_full(
            &state,
            "mock-asr",
            "transcribe",
            params,
            input,
            None,
            SubmitOptions {
                wait: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let record = outcome.record.expect("wait 模式必须携带记录");
        assert_ne!(record.status, TaskState::Completed, "缓存关闭后不应命中");
        assert!(record.cached_from.is_none());
        assert!(cache_pending()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty());
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
            priority: DEFAULT_TASK_PRIORITY,
            started_at: Utc::now(),
            started_running_at: Some(Utc::now()),
            finished_at: None,
            node_order: nodes.keys().cloned().collect(),
            nodes,
            artifacts: HashMap::new(),
            served_artifacts: HashMap::new(),
            work_dir: PathBuf::new(),
            staging_dir: None,
            module_ids: Vec::new(),
        group: None,
        cached_from: None,
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
            priority: DEFAULT_TASK_PRIORITY,
            started_at: Utc::now(),
            started_running_at: Some(Utc::now()),
            finished_at: None,
            node_order: Vec::new(),
            nodes: HashMap::new(),
            artifacts: HashMap::new(),
            served_artifacts: HashMap::new(),
            work_dir: PathBuf::new(),
            staging_dir: None,
            module_ids: Vec::new(),
        group: None,
        cached_from: None,
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

// ════════════════════════════════════════════════════════════════════════════
// 直调物化（W1-D ｜ PLAN_TRIGGER_UNIFIED_LOG §5.4）——触发规则 direct 模式提交
//
// 触发器巡检循环（W1-E）命中文件后按规则模式提交：
// - 仅归档（Archive）：物化两节点退化 DAG `file_input → file_archive`；
// - 模块直调（Module）：先 `ensure_module_running`，再物化三节点退化 DAG
//   `file_input → module → file_archive`。
//
// 两者均走 [`submit_pipeline_full`]（与手动任务同一闸门/注册表/WS/产物通路），
// 任务 `pipeline_id = "watcher/<rule_id>"`，规则名经命名模板 `{rule}` 占位符
// 参与归档命名（§5.5）。
//
// **实现偏差声明（`_meta.rule` 注入）**：§5.4 约定经任务 inputs 保留键
// `_meta.rule` 注入规则名；但既有 `submit_pipeline_full` → `apply_inputs`
// 将 inputs 键一律按节点 id 解析（`_meta` 会触发 `UnknownInputNode`），
// 且 `run_task` 链路尚无 `runner.set_task_inputs` 通路——二者均属既有函数，
// 本区段无权修改。故规则名在**物化期**预渲染进 `name_template`
// （[`inject_rule_into_template`]），执行期 `expand_name_template` 以空
// rule 展开，其余占位符语义不变。规则名含 `{`/`}` 的极端输入被守卫跳过
// （避免二次展开，`{rule}` 届时渲染为空串）；含路径分隔符 `/`/`\` 或 `..`
// 的输入被**拒绝**（评审修复：否则经 `{rule}` 渲染出子目录/穿越路径）。
// 若后续任务补齐 task_inputs 通路，本预渲染与之叠加仍然正确（模板已不含
// `{rule}`）。
// ════════════════════════════════════════════════════════════════════════════

/// 触发规则归档输出配置快照（§5.1 `OutputConfig` 的字段直传）。
///
/// 刻意以独立结构承载而非引用 watcher 模块的 `OutputConfig` 类型：
/// watcher.rs 归 W0-C 所有，本区段与之零类型耦合，W1-E 提交时按字段映射。
/// 缺省语义（§5.5）：`name_template` 空 → `{name}.{ext}`；
/// `on_conflict` 空 → `suffix`（归一在 [`WatchArchiveOutput::render_params`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchArchiveOutput {
    /// 归档目标目录（必填，绝对路径）
    pub dest_dir: String,
    /// 命名模板（`{name}/{ext}/{date}/{datetime}/{rule}/{seq}`）
    pub name_template: String,
    /// 冲突策略（`suffix`/`overwrite`/`skip`）
    pub on_conflict: String,
}

/// 规则名预渲染：物化期将 `{rule}` 替换为规则名（见区段头偏差声明）。
///
/// 返回 `Err(原因)` 的守卫输入（提交期拒绝，不落任务）：
/// - 含 `{`/`}`：会在执行期被二次展开（违反「占位符值不二次展开」裁决）；
/// - 含路径分隔符 `/`/`\` 或 `..` 组件：经 `{rule}` 渲染出子目录/穿越路径
///   （评审修复）。
fn inject_rule_into_template(template: &str, rule_name: &str) -> Result<String, String> {
    if rule_name.contains('{') || rule_name.contains('}') {
        Err(format!(
            "rule name contains `{{`/`}}` (would be re-expanded): `{rule_name}`"
        ))
    } else if rule_name.contains('/') || rule_name.contains('\\') {
        Err(format!(
            "rule name contains path separator: `{rule_name}`"
        ))
    } else if rule_name == ".." {
        // 分隔符已被上一分支拒绝，此处拦截剩余的唯一穿越形状：规则名本身为 `..`
        Err(format!("rule name is `..` path component: `{rule_name}`"))
    } else {
        Ok(template.replace("{rule}", rule_name))
    }
}

/// 渲染 `file_archive` 节点 params（§5.5）：缺省归一 + 绝对路径/策略快校验。
///
/// 快速失败（提交期即拒，不占闸门不建任务目录）；`on_conflict` 复用
/// ep-core [`ep_core::archive::ConflictPolicy::parse`]（非法值报错不静默回退）。
fn watch_archive_params(output: &WatchArchiveOutput, rule_name: &str) -> Result<Value, SubmitError> {
    let dest_dir = output.dest_dir.trim();
    if dest_dir.is_empty() {
        return Err(SubmitError::InvalidPipeline(
            "watcher output.dest_dir is required".to_string(),
        ));
    }
    if !Path::new(dest_dir).is_absolute() {
        return Err(SubmitError::InvalidPipeline(format!(
            "watcher output.dest_dir must be an absolute path, got `{dest_dir}`"
        )));
    }
    let template = if output.name_template.trim().is_empty() {
        "{name}.{ext}"
    } else {
        output.name_template.trim()
    };
    let on_conflict = if output.on_conflict.trim().is_empty() {
        "suffix"
    } else {
        output.on_conflict.trim()
    };
    ep_core::archive::ConflictPolicy::parse(on_conflict).map_err(|e| {
        SubmitError::InvalidPipeline(format!("watcher output.on_conflict: {e}"))
    })?;
    // 规则名守卫（花括号/路径分隔符/`..`）→ 提交期拒绝，不落任务
    let name_template = inject_rule_into_template(template, rule_name)
        .map_err(SubmitError::InvalidPipeline)?;
    Ok(json!({
        "dest_dir": dest_dir,
        "name_template": name_template,
        "on_conflict": on_conflict,
    }))
}

/// 物化仅归档退化 DAG：`input(file_input) → archive(file_archive)`（§5.4）。
///
/// `pipeline_id = "watcher/<rule_id>"`；file_archive params 由规则 output 渲染。
pub fn build_watch_archive_pipeline(
    rule_id: &str,
    rule_name: &str,
    input_path: &Path,
    output: &WatchArchiveOutput,
) -> Result<Pipeline, SubmitError> {
    let archive_params = watch_archive_params(output, rule_name)?;
    Ok(Pipeline {
        id: format!("watcher/{rule_id}"),
        name: format!("触发归档 {rule_name}"),
        description: "触发规则直调-仅归档（§5.4 退化两节点 DAG）".to_string(),
        nodes: vec![
            PipelineNode {
                id: "input".to_string(),
                kind: NodeKind::Builtin {
                    builtin: "file_input".to_string(),
                },
                label: "输入文件".to_string(),
                params: json!({ "path": input_path.display().to_string() }),
                position: None,
                timeout_secs: None,
                retry_count: None,
            },
            PipelineNode {
                id: "archive".to_string(),
                kind: NodeKind::Builtin {
                    builtin: "file_archive".to_string(),
                },
                label: "归档输出".to_string(),
                params: archive_params,
                position: None,
                timeout_secs: None,
                retry_count: None,
            },
        ],
        edges: vec![ep_core::pipeline::dag::Edge {
            from: ("input".to_string(), "output".to_string()),
            to: ("archive".to_string(), "input".to_string()),
        }],
        max_instances: None,
        node_timeout_secs: None,
    })
}

/// 物化模块直调退化 DAG：`input(file_input) → run(module) → archive(file_archive)`
/// （§5.4；仿 [`build_direct_pipeline`]，末端 file_output 换为 file_archive）。
///
/// module 节点 params 直传规则 `DirectAction.params`（null/非对象归一为 `{}`）；
/// 归档末端同 [`build_watch_archive_pipeline`]。
pub fn build_watch_module_pipeline(
    rule_id: &str,
    rule_name: &str,
    module_id: &str,
    capability: &str,
    params: Value,
    input_path: &Path,
    output: &WatchArchiveOutput,
) -> Result<Pipeline, SubmitError> {
    let archive_params = watch_archive_params(output, rule_name)?;
    let module_params = if params.is_object() {
        params
    } else {
        json!({})
    };
    Ok(Pipeline {
        id: format!("watcher/{rule_id}"),
        name: format!("触发直调 {rule_name}"),
        description: "触发规则直调-模块（§5.4 退化三节点 DAG）".to_string(),
        nodes: vec![
            PipelineNode {
                id: "input".to_string(),
                kind: NodeKind::Builtin {
                    builtin: "file_input".to_string(),
                },
                label: "输入文件".to_string(),
                params: json!({ "path": input_path.display().to_string() }),
                position: None,
                timeout_secs: None,
                retry_count: None,
            },
            PipelineNode {
                id: "run".to_string(),
                kind: NodeKind::Module {
                    module_id: module_id.to_string(),
                    capability: capability.to_string(),
                    model_id: None,
                    device: None,
                },
                label: "模块执行".to_string(),
                params: module_params,
                position: None,
                timeout_secs: None,
                retry_count: None,
            },
            PipelineNode {
                id: "archive".to_string(),
                kind: NodeKind::Builtin {
                    builtin: "file_archive".to_string(),
                },
                label: "归档输出".to_string(),
                params: archive_params,
                position: None,
                timeout_secs: None,
                retry_count: None,
            },
        ],
        edges: vec![
            ep_core::pipeline::dag::Edge {
                from: ("input".to_string(), "output".to_string()),
                to: ("run".to_string(), "input".to_string()),
            },
            ep_core::pipeline::dag::Edge {
                from: ("run".to_string(), "output".to_string()),
                to: ("archive".to_string(), "input".to_string()),
            },
        ],
        max_instances: None,
        node_timeout_secs: None,
    })
}

/// 触发规则直调-仅归档提交（§5.4）。
///
/// 输入文件存在性快检 → 物化两节点退化 DAG → [`submit_pipeline_full`]
/// （默认异步模式：任务入队即返回，归档在后台执行）。
/// 任务 `pipeline_id = "watcher/<rule_id>"`；返回 task_id 供规则
/// `last_task_id`/`recent` 回写与 `watcher_trigger` 事件记录。
pub async fn submit_direct_archive(
    state: &Arc<AppState>,
    rule_id: &str,
    rule_name: &str,
    input_path: PathBuf,
    output: &WatchArchiveOutput,
) -> Result<String, SubmitError> {
    if !input_path.is_file() {
        return Err(SubmitError::InputMissing(input_path));
    }
    let pipeline = build_watch_archive_pipeline(rule_id, rule_name, &input_path, output)?;
    submit_pipeline_full(state, pipeline, None, SubmitOptions::default())
        .await
        .map(|outcome| outcome.task_id)
}

/// 模块拉起钩子（可注入）：拉起失败返回 `Err(detail)` → [`SubmitError::ModuleStartFailed`]。
///
/// 生产实现为 [`ensure_module_running_default`]（api/autostart 权威实现）；
/// 测试注入 mock 以绕开真实模块进程拉起。
pub type EnsureRunningFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<(), String>> + Send>,
>;

/// [`submit_direct_module`] 的注入式版本（测试桩入口）。
///
/// `ensure_running(state, module_id)` 在物化**之前**调用（§5.4 仿
/// `execute_single`：先保证模块 Running）；失败即拒绝提交，不物化任务。
#[allow(clippy::too_many_arguments)]
pub async fn submit_direct_module_with(
    state: Arc<AppState>,
    rule_id: String,
    rule_name: String,
    module_id: String,
    capability: String,
    params: Value,
    input_path: PathBuf,
    output: WatchArchiveOutput,
    ensure_running: impl FnOnce(Arc<AppState>, String) -> EnsureRunningFuture,
) -> Result<String, SubmitError> {
    if !input_path.is_file() {
        return Err(SubmitError::InputMissing(input_path));
    }
    ensure_running(state.clone(), module_id.clone())
        .await
        .map_err(SubmitError::ModuleStartFailed)?;
    let pipeline = build_watch_module_pipeline(
        &rule_id,
        &rule_name,
        &module_id,
        &capability,
        params,
        &input_path,
        &output,
    )?;
    submit_pipeline_full(&state, pipeline, None, SubmitOptions::default())
        .await
        .map(|outcome| outcome.task_id)
}

/// 生产模块拉起：直通 api/autostart 权威实现（错误串化进
/// [`SubmitError::ModuleStartFailed`]，语义对齐 §5.4「拉起失败 → rejected」）。
async fn ensure_module_running_default(
    state: Arc<AppState>,
    module_id: String,
) -> Result<(), String> {
    crate::api::autostart::ensure_module_running(&state, &module_id)
        .await
        .map_err(|e| e.to_string())
}

/// 触发规则直调-模块提交（§5.4）。
///
/// 流程：输入文件存在性快检 → `ensure_module_running`（模块 Running、
/// 端口就绪；失败即 `ModuleStartFailed`，不物化任务）→ 物化三节点退化 DAG
/// → [`submit_pipeline_full`]。任务 `pipeline_id = "watcher/<rule_id>"`。
#[allow(clippy::too_many_arguments)]
pub async fn submit_direct_module(
    state: &Arc<AppState>,
    rule_id: &str,
    rule_name: &str,
    module_id: &str,
    capability: &str,
    params: Value,
    input_path: PathBuf,
    output: &WatchArchiveOutput,
) -> Result<String, SubmitError> {
    submit_direct_module_with(
        state.clone(),
        rule_id.to_string(),
        rule_name.to_string(),
        module_id.to_string(),
        capability.to_string(),
        params,
        input_path,
        output.clone(),
        |state, module_id| Box::pin(ensure_module_running_default(state, module_id)),
    )
    .await
}

// ─── 直调物化单测（退化 DAG 结构断言 + 提交路径；§5.4 完成标准） ─────────────

#[cfg(test)]
mod watcher_direct_tests {
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use ep_core::config::AppConfig;
    use ep_core::port::PortManager;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn unique_root(tag: &str) -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-exec-wdirect-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn wtest_state(root: PathBuf) -> Arc<AppState> {
        Arc::new(AppState::new(
            root,
            AppConfig::default(),
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        ))
    }

    fn woutput(dest_dir: &Path) -> WatchArchiveOutput {
        WatchArchiveOutput {
            dest_dir: dest_dir.display().to_string(),
            name_template: "{rule}-{name}.{ext}".to_string(),
            on_conflict: "suffix".to_string(),
        }
    }

    /// 轮询等待任务终结（completed/failed/cancelled；30s 上限）
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

    // ── 1. 仅归档退化 DAG：节点数 / 边 / params 渲染 / pipeline_id ──────────

    #[test]
    fn test_watch_archive_pipeline_structure() {
        let root = unique_root("arch-struct");
        let input = root.join("clip.mp4");
        let output = woutput(&root.join("dest"));
        let p = build_watch_archive_pipeline("abc12345", "bt-watch", &input, &output).unwrap();

        assert_eq!(p.id, "watcher/abc12345");
        assert_eq!(p.nodes.len(), 2);
        assert_eq!(p.edges.len(), 1);
        assert_eq!(p.nodes[0].id, "input");
        assert_eq!(
            p.nodes[0].kind,
            NodeKind::Builtin {
                builtin: "file_input".to_string()
            }
        );
        assert_eq!(
            p.nodes[0].params,
            json!({ "path": input.display().to_string() })
        );
        assert_eq!(p.nodes[1].id, "archive");
        assert_eq!(
            p.nodes[1].kind,
            NodeKind::Builtin {
                builtin: "file_archive".to_string()
            }
        );
        // {rule} 物化期预渲染（区段头偏差声明）
        assert_eq!(
            p.nodes[1].params,
            json!({
                "dest_dir": root.join("dest").display().to_string(),
                "name_template": "bt-watch-{name}.{ext}",
                "on_conflict": "suffix",
            })
        );
        assert_eq!(p.edges[0].from, ("input".to_string(), "output".to_string()));
        assert_eq!(
            p.edges[0].to,
            ("archive".to_string(), "input".to_string())
        );
        // 物化 DAG 必须通过 P2-11 静态校验（提交路径同款）
        assert!(p.validate().is_ok(), "{:?}", p.validate());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 2. 模块直调退化 DAG：三节点 / 两边 / module 节点形状 ────────────────

    #[test]
    fn test_watch_module_pipeline_structure() {
        let root = unique_root("mod-struct");
        let input = root.join("a.wav");
        let output = woutput(&root.join("dest"));
        let p = build_watch_module_pipeline(
            "deadbeef",
            "asr-watch",
            "faster-whisper",
            "transcribe",
            json!({ "language": "zh" }),
            &input,
            &output,
        )
        .unwrap();

        assert_eq!(p.id, "watcher/deadbeef");
        assert_eq!(p.nodes.len(), 3);
        assert_eq!(p.edges.len(), 2);
        assert_eq!(p.nodes[0].id, "input");
        assert_eq!(p.nodes[1].id, "run");
        assert_eq!(
            p.nodes[1].kind,
            NodeKind::Module {
                module_id: "faster-whisper".to_string(),
                capability: "transcribe".to_string(),
                model_id: None,
                device: None,
            }
        );
        assert_eq!(p.nodes[1].params, json!({ "language": "zh" }));
        assert_eq!(p.nodes[2].id, "archive");
        assert_eq!(
            p.nodes[2].kind,
            NodeKind::Builtin {
                builtin: "file_archive".to_string()
            }
        );
        assert_eq!(p.edges[0].from, ("input".to_string(), "output".to_string()));
        assert_eq!(p.edges[0].to, ("run".to_string(), "input".to_string()));
        assert_eq!(p.edges[1].from, ("run".to_string(), "output".to_string()));
        assert_eq!(
            p.edges[1].to,
            ("archive".to_string(), "input".to_string())
        );
        assert!(p.validate().is_ok(), "{:?}", p.validate());
        // 非对象 params 归一为 {}
        let p2 = build_watch_module_pipeline(
            "deadbeef",
            "asr-watch",
            "m",
            "c",
            Value::Null,
            &input,
            &output,
        )
        .unwrap();
        assert_eq!(p2.nodes[1].params, json!({}));
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 3. output 渲染：缺省归一 / 非法值快速失败 / 花括号守卫 ──────────────

    #[test]
    fn test_watch_archive_params_defaults_and_validation() {
        let root = unique_root("params");
        let input = root.join("x.txt");
        // 空模板/空策略 → §5.5 缺省
        let out = WatchArchiveOutput {
            dest_dir: root.join("dest").display().to_string(),
            name_template: String::new(),
            on_conflict: String::new(),
        };
        let p = build_watch_archive_pipeline("r1", "rule-a", &input, &out).unwrap();
        assert_eq!(
            p.nodes[1].params,
            json!({
                "dest_dir": root.join("dest").display().to_string(),
                "name_template": "{name}.{ext}",
                "on_conflict": "suffix",
            })
        );
        // 非绝对 dest_dir → 提交期拒绝
        let rel = WatchArchiveOutput {
            dest_dir: "relative/dest".to_string(),
            name_template: String::new(),
            on_conflict: String::new(),
        };
        assert!(matches!(
            build_watch_archive_pipeline("r1", "rule-a", &input, &rel),
            Err(SubmitError::InvalidPipeline(_))
        ));
        // 空 dest_dir → 提交期拒绝
        let empty = WatchArchiveOutput {
            dest_dir: "  ".to_string(),
            name_template: String::new(),
            on_conflict: String::new(),
        };
        assert!(matches!(
            build_watch_archive_pipeline("r1", "rule-a", &input, &empty),
            Err(SubmitError::InvalidPipeline(_))
        ));
        // 非法 on_conflict → 提交期拒绝（复用 ep-core 解析语义）
        let bad = WatchArchiveOutput {
            dest_dir: root.join("dest").display().to_string(),
            name_template: String::new(),
            on_conflict: "replace".to_string(),
        };
        assert!(matches!(
            build_watch_archive_pipeline("r1", "rule-a", &input, &bad),
            Err(SubmitError::InvalidPipeline(_))
        ));
        // 规则名含花括号 → 提交期拒绝（会经 {rule} 二次展开）
        let brace_out = WatchArchiveOutput {
            dest_dir: root.join("dest").display().to_string(),
            name_template: "{rule}-{name}.{ext}".to_string(),
            on_conflict: String::new(),
        };
        assert!(matches!(
            build_watch_archive_pipeline("r1", "{da}te", &input, &brace_out),
            Err(SubmitError::InvalidPipeline(_))
        ));
        // 规则名含路径分隔符 / 或 \ → 提交期拒绝（评审修复：防子目录/穿越渲染）
        assert!(matches!(
            build_watch_archive_pipeline("r1", "bt/watch", &input, &brace_out),
            Err(SubmitError::InvalidPipeline(_))
        ));
        assert!(matches!(
            build_watch_archive_pipeline("r1", "bt\\watch", &input, &brace_out),
            Err(SubmitError::InvalidPipeline(_))
        ));
        // 规则名本身为 `..` → 提交期拒绝
        assert!(matches!(
            build_watch_archive_pipeline("r1", "..", &input, &brace_out),
            Err(SubmitError::InvalidPipeline(_))
        ));
        // 正常规则名 → 正常渲染进模板
        let p2 = build_watch_archive_pipeline("r1", "bt-watch", &input, &brace_out).unwrap();
        assert_eq!(p2.nodes[1].params["name_template"], "bt-watch-{name}.{ext}");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 4. 仅归档提交端到端（真实执行：file_input → file_archive） ─────────

    #[tokio::test]
    async fn test_submit_direct_archive_runs_to_completion() {
        let _guard = lock_for_tests();
        let root = unique_root("arch-submit");
        reset_and_bind_for_tests(&root);
        let state = wtest_state(root.clone());

        let input = root.join("movie.mp4");
        std::fs::write(&input, "archive me").unwrap();
        let dest_dir = root.join("archived");
        let output = WatchArchiveOutput {
            dest_dir: dest_dir.display().to_string(),
            name_template: "{rule}-{name}.{ext}".to_string(),
            on_conflict: "suffix".to_string(),
        };

        let task_id = submit_direct_archive(&state, "abc12345", "bt-watch", input.clone(), &output)
            .await
            .expect("仅归档提交应成功");
        let record = wait_terminal(&task_id).await.expect("任务应在超时前终结");
        assert_eq!(record.pipeline_id, "watcher/abc12345");
        assert_eq!(record.status, TaskState::Completed, "{:?}", record.nodes);
        assert_eq!(record.nodes.len(), 2);
        assert_eq!(record.nodes["input"].state, "completed");
        assert_eq!(record.nodes["archive"].state, "completed");
        // {rule} → 规则名；{name}.{ext} → 上游文件名
        let dest = dest_dir.join("bt-watch-movie.mp4");
        assert!(dest.is_file(), "归档产物应落盘：{}", dest.display());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "archive me");
        // 输入文件保持原位（复制语义）
        assert!(input.is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 5. 仅归档提交：输入文件缺失 → 提交期拒绝 ────────────────────────────

    #[tokio::test]
    async fn test_submit_direct_archive_missing_input_rejected() {
        let _guard = lock_for_tests();
        let root = unique_root("arch-missing");
        reset_and_bind_for_tests(&root);
        let state = wtest_state(root.clone());
        let output = woutput(&root.join("dest"));
        let err = submit_direct_archive(&state, "abc12345", "r", root.join("ghost.mp4"), &output)
            .await
            .expect_err("缺失输入应被拒绝");
        assert!(matches!(err, SubmitError::InputMissing(_)));
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 6. 模块直调：ensure 拉起失败 → ModuleStartFailed，不物化任务 ────────

    #[tokio::test]
    async fn test_submit_direct_module_ensure_failure_rejects() {
        let _guard = lock_for_tests();
        let root = unique_root("mod-ensure-fail");
        reset_and_bind_for_tests(&root);
        let state = wtest_state(root.clone());

        let input = root.join("a.wav");
        std::fs::write(&input, "x").unwrap();
        let output = woutput(&root.join("dest"));
        // ghost 模块未注册 → 真实 ensure 路径拉起失败
        let err = submit_direct_module(
            &state,
            "abc12345",
            "r",
            "ghost-mod",
            "transcribe",
            json!({}),
            input,
            &output,
        )
        .await
        .expect_err("未注册模块应拉起失败");
        assert!(matches!(err, SubmitError::ModuleStartFailed(_)));
        // 未物化任务（注册表无记录）
        assert!(snapshot_all().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 7. 模块直调（mock ensure）：三节点物化 + watcher 命名空间 ───────────

    #[tokio::test]
    async fn test_submit_direct_module_with_mock_ensure_submits() {
        let _guard = lock_for_tests();
        let root = unique_root("mod-mock");
        reset_and_bind_for_tests(&root);
        let state = wtest_state(root.clone());

        let input = root.join("a.wav");
        std::fs::write(&input, "x").unwrap();
        let dest_dir = root.join("dest");
        let output = WatchArchiveOutput {
            dest_dir: dest_dir.display().to_string(),
            name_template: "{rule}-{name}.{ext}".to_string(),
            on_conflict: "suffix".to_string(),
        };

        // Arc：EnsureRunningFuture 要求 'static，桩内标志以 Arc 共享
        let ensured = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ensured_flag = Arc::clone(&ensured);
        let task_id = submit_direct_module_with(
            state.clone(),
            "feedc0de".to_string(),
            "asr-watch".to_string(),
            "fake-mod".to_string(),
            "transcribe".to_string(),
            json!({}),
            input,
            output,
            |_state, module_id| {
                // 注入桩：绕开真实模块拉起，但验证确实被调用
                let ensured = Arc::clone(&ensured_flag);
                Box::pin(async move {
                    ensured.store(true, Ordering::SeqCst);
                    assert_eq!(module_id, "fake-mod");
                    Ok(())
                })
            },
        )
        .await
        .expect("mock ensure 下提交应成功");
        assert!(ensured.load(Ordering::SeqCst), "拉起钩子应先于物化调用");

        let record = wait_terminal(&task_id).await.expect("任务应在超时前终结");
        assert_eq!(record.pipeline_id, "watcher/feedc0de");
        // 三节点退化 DAG 形状（fake 模块无端口 → 任务失败，但结构已物化）
        assert_eq!(record.node_order, vec!["input", "run", "archive"]);
        assert_eq!(record.nodes.len(), 3);
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── T-25：产物登记挂钩——watcher 直调任务 archive 产物落盘即登记 ─────────

    /// 最小任务记录夹具（仅需 id + pipeline_id 供 record_artifact 判定）
    fn seed_record(task_id: &str, pipeline_id: &str) {
        insert_record_for_tests(TaskRecord {
            id: task_id.to_string(),
            pipeline_id: pipeline_id.to_string(),
            status: TaskState::Running,
            error: None,
            queue_position: None,
            priority: DEFAULT_TASK_PRIORITY,
            started_at: chrono::Utc::now(),
            started_running_at: Some(chrono::Utc::now()),
            finished_at: None,
            node_order: Vec::new(),
            nodes: TestMap::new(),
            artifacts: TestMap::new(),
            served_artifacts: TestMap::new(),
            work_dir: std::path::PathBuf::new(),
            staging_dir: None,
            module_ids: Vec::new(),
            group: None,
            cached_from: None,
        });
    }

    #[test]
    fn test_record_artifact_registers_watcher_archive_products() {
        let _guard = lock_for_tests();
        let _p_guard = crate::watcher::processed_test_lock();
        clear_registry_for_tests();

        let root = unique_root("processed");
        let product = root.join("out.mp4");
        std::fs::write(&product, b"result").unwrap();
        // 登记表定向到本测试独立落盘文件（不依赖持久化绑定的全局状态）
        let disk = root.join("runtime").join("processed_registry.json");
        crate::watcher::processed_reset_for_tests(Some(disk.clone()));

        // watcher 直调任务（pipeline_id = "watcher/<rule_id>"）与非 watcher 对照
        seed_record("wt-1", "watcher/r1");
        seed_record("mt-1", "my-pipeline");

        // archive 节点产物 → 登记；非 archive 节点 / 非 watcher 任务 → 不登记
        record_artifact("wt-1", "archive", &product);
        record_artifact("wt-1", "input", &root.join("in.mp4"));
        record_artifact("mt-1", "archive", &root.join("other.mp4"));

        // disk=Some 时 record 即落盘：从落盘文件断言登记内容
        let saved = crate::watcher::ProcessedRegistry::load(&disk);
        assert_eq!(saved.entries.len(), 1, "仅 watcher archive 产物登记");
        assert!(saved.contains_path(&product));
        assert_eq!(saved.entries[0].rule_id, "r1");
        assert_eq!(saved.entries[0].task_id, "wt-1");

        crate::watcher::processed_reset_for_tests(None);
        let _ = std::fs::remove_dir_all(&root);
    }

    // 审查 P1-1：管线模式规则任务（pipeline_id 为用户管线 id）经提交侧映射
    // 判定来源——映射在册任务的任意产物节点都登记；未在册的同 pipeline_id
    // 任务不登记。
    #[test]
    fn test_record_artifact_registers_watcher_pipeline_products() {
        let _guard = lock_for_tests();
        let _p_guard = crate::watcher::processed_test_lock();
        clear_registry_for_tests();

        let root = unique_root("processed-pipeline");
        let product = root.join("final.mkv");
        std::fs::write(&product, b"result").unwrap();
        let disk = root.join("runtime").join("processed_registry.json");
        crate::watcher::processed_reset_for_tests(Some(disk.clone()));

        seed_record("wp-1", "user-archive-pipeline");
        crate::watcher::remember_watcher_pipeline_task("wp-1", "r-pipe");
        seed_record("mp-1", "user-archive-pipeline");

        // 管线产物节点 id 命名不固定：任意节点都登记；映射缺失则不登记
        record_artifact("wp-1", "save_output", &product);
        record_artifact("mp-1", "save_output", &root.join("other.mkv"));

        let saved = crate::watcher::ProcessedRegistry::load(&disk);
        assert_eq!(saved.entries.len(), 1, "仅映射在册的管线任务产物登记");
        assert!(saved.contains_path(&product));
        assert_eq!(saved.entries[0].rule_id, "r-pipe");
        assert_eq!(saved.entries[0].task_id, "wp-1");

        crate::watcher::processed_reset_for_tests(None);
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── §6.8b 动态 VRAM 准入（纯函数判定） ─────────────────────────────────

    use std::collections::HashMap as TestMap;

    fn snapshot_with(waifu2x: crate::api::execute::execution::ModuleAdmission) -> super::AdmissionSnapshot {
        let mut snap = super::AdmissionSnapshot::default();
        snap.devices.insert(
            "cuda:0".to_string(),
            super::DeviceLedger { total_mb: 8192, used_live_mb: 500 },
        );
        snap.modules.insert("waifu2x".to_string(), waifu2x);
        snap
    }

    fn admission(cost: u32, maxc: u32, device: Option<&str>) -> super::ModuleAdmission {
        super::ModuleAdmission {
            device: device.map(str::to_string),
            cost_mb: cost,
            max_concurrent: maxc,
        }
    }

    #[test]
    fn admission_vram_budget_blocks_overcommit() {
        let snap = snapshot_with(admission(3000, 1, Some("cuda:0")));
        let ledger: TestMap<String, usize> = TestMap::new();
        let committed: TestMap<String, u32> = TestMap::new();
        // 空闲 ~7692：第一实例放行
        assert!(snap.admits(&["waifu2x".into()], &ledger, &committed));
        // 已承诺 6000（两实例）后：max(500, 6000) + 3000 > 8192 → 拒绝
        let mut committed2 = committed.clone();
        committed2.insert("cuda:0".to_string(), 6000);
        assert!(!snap.admits(&["waifu2x".into()], &ledger, &committed2));
    }

    #[test]
    fn admission_uses_max_of_live_and_committed() {
        // 实时占用 7000 > 承诺 3000：以实时为准 → 7000 + 3000 > 8192 拒绝
        let mut snap = snapshot_with(admission(3000, 1, Some("cuda:0")));
        snap.devices.get_mut("cuda:0").unwrap().used_live_mb = 7000;
        let ledger: TestMap<String, usize> = TestMap::new();
        let mut committed: TestMap<String, u32> = TestMap::new();
        committed.insert("cuda:0".to_string(), 3000);
        assert!(!snap.admits(&["waifu2x".into()], &ledger, &committed));
        // 承诺 6000 > 实时 500（指标滞后窗口）：以账本为准 → 6000+3000 拒绝
        snap.devices.get_mut("cuda:0").unwrap().used_live_mb = 500;
        let mut committed: TestMap<String, u32> = TestMap::new();
        committed.insert("cuda:0".to_string(), 6000);
        assert!(!snap.admits(&["waifu2x".into()], &ledger, &committed));
    }

    #[test]
    fn admission_module_concurrency_cap() {
        let snap = snapshot_with(admission(3000, 2, Some("cuda:0")));
        let committed: TestMap<String, u32> = TestMap::new();
        let mut running: TestMap<String, usize> = TestMap::new();
        running.insert("waifu2x".to_string(), 2);
        assert!(!snap.admits(&["waifu2x".into()], &running, &committed),
            "达到声明并发上限 → 即使显存富余也不准入");
        running.insert("waifu2x".to_string(), 1);
        assert!(snap.admits(&["waifu2x".into()], &running, &committed));
    }

    #[test]
    fn admission_fail_open_without_metrics_or_declaration() {
        // 设备无指标（total None 被剔除）→ fail-open
        let mut snap = snapshot_with(admission(3000, 1, Some("cuda:0")));
        snap.devices.clear();
        let ledger: TestMap<String, usize> = TestMap::new();
        let committed: TestMap<String, u32> = TestMap::new();
        assert!(snap.admits(&["waifu2x".into()], &ledger, &committed));
        // 模块无准入条目（未声明显存/CPU）→ 不设限
        let snap = super::AdmissionSnapshot::default();
        assert!(snap.admits(&["cpu-mod".into()], &ledger, &committed));
    }

    #[test]
    fn admission_multi_device_task_requires_all_devices_fit() {
        let mut snap = super::AdmissionSnapshot::default();
        snap.devices.insert(
            "cuda:0".to_string(),
            super::DeviceLedger { total_mb: 8192, used_live_mb: 0 },
        );
        snap.devices.insert(
            "cuda:1".to_string(),
            super::DeviceLedger { total_mb: 4096, used_live_mb: 0 },
        );
        snap.modules.insert(
            "mod-a".to_string(),
            admission(3000, 1, Some("cuda:0")),
        );
        snap.modules.insert(
            "mod-b".to_string(),
            admission(3000, 1, Some("cuda:1")),
        );
        let ledger: TestMap<String, usize> = TestMap::new();
        let committed: TestMap<String, u32> = TestMap::new();
        assert!(snap.admits(&["mod-a".into(), "mod-b".into()], &ledger, &committed));
        // cuda:1 挤占后跨设备任务整体拒绝
        let mut committed2 = committed.clone();
        committed2.insert("cuda:1".to_string(), 2000);
        assert!(!snap.admits(&["mod-a".into(), "mod-b".into()], &ledger, &committed2));
    }

    // ── §6.8b force 通路（T-22） ────────────────────────────────────────────

    // 本模块本地夹具（`mod tests` 的同名 helper 跨模块不可见）
    fn queued_entry(task_id: &str, priority: u8) -> QueuedEntry {
        queued_entry_force(task_id, priority, false)
    }

    fn queued_entry_force(task_id: &str, priority: u8, force: bool) -> QueuedEntry {
        QueuedEntry {
            task_id: task_id.to_string(),
            pipeline_id: "p".to_string(),
            priority,
            module_ids: Vec::new(),
            force,
        }
    }

    /// 路径转义为 TOML basic string（本地副本，`mod tests::toml_path` 不可见）
    fn wtoml_path(p: &Path) -> String {
        p.display().to_string().replace('\\', "\\\\").replace('"', "\\\"")
    }

    /// file_input → file_output 复制管线（真实可执行；`mod tests::copy_pipeline`
    /// 的本地副本）
    fn wcopy_pipeline(id: &str, src: &Path, dest: &Path) -> Pipeline {
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
            wtoml_path(src),
            wtoml_path(dest),
        );
        Pipeline::from_toml_str(&toml).unwrap()
    }

    #[test]
    fn pick_next_index_prefers_force_over_priority_then_fifo() {
        // force 任务插队：即便普通高优先级任务先入队，force 也先出队
        let queue = VecDeque::from([
            queued_entry("normal-1", 50),
            queued_entry("high", 10),
            queued_entry_force("forced", 50, true),
        ]);
        assert_eq!(pick_next_index(&queue, |_| true), Some(2));

        // force 任务之间按优先级 + FIFO
        let queue = VecDeque::from([
            queued_entry_force("forced-a", 50, true),
            queued_entry_force("forced-b", 10, true),
        ]);
        assert_eq!(pick_next_index(&queue, |_| true), Some(1));

        // force 不受 eligible 限制豁免：不可准入的 force 任务仍被跳过
        let queue = VecDeque::from([queued_entry_force("forced", 50, true)]);
        assert_eq!(pick_next_index(&queue, |_| false), None);

        // 全员非 force → 行为与 T-15 完全一致（回归锚点）
        let queue = VecDeque::from([
            queued_entry("normal-1", 50),
            queued_entry("normal-2", 50),
            queued_entry("high", 10),
        ]);
        assert_eq!(pick_next_index(&queue, |_| true), Some(2));
        assert_eq!(pick_next_index(&queue, |e| e.task_id != "high"), Some(0));
    }

    #[test]
    fn queued_position_reflects_force_precedence() {
        let sched = Scheduler {
            queue: VecDeque::from([
                queued_entry("normal-1", 50),
                queued_entry_force("forced", 50, true),
                queued_entry("high", 10),
            ]),
            ..Scheduler::default()
        };
        // 展示序位与派发序一致：force=1，普通任务（含高优先级）依次后移
        assert_eq!(queued_position_in(&sched, "forced"), Some(1));
        assert_eq!(queued_position_in(&sched, "high"), Some(2));
        assert_eq!(queued_position_in(&sched, "normal-1"), Some(3));
    }

    #[test]
    fn entry_admissible_force_bypasses_dynamic_admission_only() {
        let snap = snapshot_with(admission(3000, 1, Some("cuda:0")));
        let ledger: TestMap<String, usize> = TestMap::new();
        // 显存账本超支（承诺 8000 + 预算 3000 > 8192）：普通任务被动态准入拦截
        let mut committed: TestMap<String, u32> = TestMap::new();
        committed.insert("cuda:0".to_string(), 8000);
        let mut normal = queued_entry("normal", 50);
        normal.module_ids = vec!["waifu2x".to_string()];
        assert!(
            !super::entry_admissible(&normal, &snap, 4, 0, &ledger, &committed),
            "显存超支时普通任务不得准入"
        );
        // force 任务跳过动态准入（§6.8b 排队等待被绕过；模块表同上，
        // 证明差异来自 force 而非空模块表的 vacuous pass）
        let mut forced = queued_entry_force("forced", 50, true);
        forced.module_ids = vec!["waifu2x".to_string()];
        assert!(super::entry_admissible(&forced, &snap, 4, 0, &ledger, &committed));
        // 静态管线闸门不被 force 绕过（安全上限语义保持）
        assert!(
            !super::entry_admissible(&forced, &snap, 1, 1, &ledger, &committed),
            "管线 max_instances 已满时 force 也不得准入"
        );
    }

    #[test]
    fn admission_uma_single_pool_sums_shared_budgets() {
        // T-22 UMA 单池语义：全部 GPU 预算折叠进同一 "uma" 键——两个模块
        // 的预算在同一物理池内累加，而不是各设备独立池各算各的
        let mut snap = super::AdmissionSnapshot::default();
        snap.devices.insert(
            super::UMA_POOL_KEY.to_string(),
            super::DeviceLedger { total_mb: 120_000, used_live_mb: 30_000 },
        );
        snap.modules.insert("mod-a".to_string(), admission(60_000, 1, Some("uma")));
        snap.modules.insert("mod-b".to_string(), admission(60_000, 1, Some("uma")));
        let ledger: TestMap<String, usize> = TestMap::new();
        let committed: TestMap<String, u32> = TestMap::new();
        assert!(
            snap.admits(&["mod-a".into()], &ledger, &committed),
            "单模块预算（30k+60k=90k）< 120k 应放行"
        );
        assert!(
            !snap.admits(&["mod-a".into(), "mod-b".into()], &ledger, &committed),
            "两模块预算累加（30k+60k+60k=150k）> 120k 须拒绝——单池重复计额防线"
        );
    }

    /// busy 场景插队（T-22 验收）：max_parallel=1、首任务持闸时，普通任务
    /// 与 force 任务都排队；释放后 force 任务先于先入队的普通任务执行。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn force_task_overtakes_queued_normal_task_when_busy() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("force-order");
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
        let order: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let order_hook = order.clone();
        set_test_run_hook(Some(Arc::new(move |task_id| {
            order_hook
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(task_id.to_string());
            // 首个任务持闸到测试放行，制造 busy 排队窗口
            let n = order_hook
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len();
            if n == 1 {
                while !RELEASE.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        })));

        // 1. 首任务占住唯一闸门
        let (src0, dest0) = (root.join("f-src-0.txt"), root.join("f-out-0.txt"));
        std::fs::write(&src0, "busy holder").unwrap();
        let holder = submit_pipeline(&state, wcopy_pipeline("force-holder", &src0, &dest0), None)
            .await
            .unwrap();
        // 2. 普通任务先入队
        let (src1, dest1) = (root.join("f-src-1.txt"), root.join("f-out-1.txt"));
        std::fs::write(&src1, "normal").unwrap();
        let normal = submit_pipeline(&state, wcopy_pipeline("force-normal", &src1, &dest1), None)
            .await
            .unwrap();
        // 3. force 任务后入队（插队）
        let (src2, dest2) = (root.join("f-src-2.txt"), root.join("f-out-2.txt"));
        std::fs::write(&src2, "forced").unwrap();
        let forced = submit_pipeline_full(
            &state,
            wcopy_pipeline("force-jump", &src2, &dest2),
            None,
            SubmitOptions {
                wait: false,
                callback_url: None,
                group: None,
                priority: None,
                bypass_cache: false,
                force: true,
            },
        )
        .await
        .unwrap()
        .task_id;

        // busy 窗口：holder running，normal 与 forced 均 queued；
        // 展示序位 force=1、normal=2（插队生效）
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(snapshot(&holder).unwrap().status, TaskState::Running);
        assert_eq!(snapshot(&normal).unwrap().status, TaskState::Queued);
        assert_eq!(snapshot(&forced).unwrap().status, TaskState::Queued);
        assert_eq!(snapshot(&forced).unwrap().queue_position, Some(1));
        assert_eq!(snapshot(&normal).unwrap().queue_position, Some(2));

        RELEASE.store(true, Ordering::SeqCst);
        for id in [&holder, &normal, &forced] {
            wait_terminal(id).await.expect("任务应终结");
        }
        set_test_run_hook(None);

        let started = order.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(started.len(), 3);
        assert_eq!(started[0], holder, "持闸任务最先执行");
        assert_eq!(
            started[1], forced,
            "busy 时排队的 force 任务应先于先入队的普通任务执行"
        );
        assert_eq!(started[2], normal);
    }

    /// force 提交元数据登记（T-22）：force 任务可查、普通任务零副作用。
    #[tokio::test]
    async fn force_submit_records_metadata_normal_submit_does_not() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("force-meta");
        let state = wtest_state(root.clone());
        let src = root.join("meta-src.txt");
        let dest = root.join("meta-out.txt");
        std::fs::write(&src, "meta").unwrap();

        let forced = submit_pipeline_full(
            &state,
            wcopy_pipeline("force-meta-pipe", &src, &dest),
            None,
            SubmitOptions {
                wait: false,
                callback_url: None,
                group: None,
                priority: None,
                bypass_cache: false,
                force: true,
            },
        )
        .await
        .unwrap()
        .task_id;
        let normal = submit_pipeline_full(
            &state,
            wcopy_pipeline("normal-meta-pipe", &src, &dest),
            None,
            SubmitOptions {
                wait: false,
                callback_url: None,
                group: None,
                priority: None,
                bypass_cache: false,
                force: false,
            },
        )
        .await
        .unwrap()
        .task_id;

        assert!(task_forced_overcommit(&forced), "force 任务应登记元数据");
        assert_eq!(FORCE_ACCEPTED_RISK, "user");
        assert!(
            !task_forced_overcommit(&normal),
            "不传 force 的提交不得产生元数据副作用（回归锚点）"
        );

        for id in [&forced, &normal] {
            wait_terminal(id).await.expect("任务应终结");
        }
        // 终态后元数据保留（任务在案期间可审计提交模式）
        assert!(task_forced_overcommit(&forced));
        assert!(!task_forced_overcommit(&normal));
    }

    /// 构造带显存声明的 GPU 模块夹具（mock-waifu：min_vram_mb = 3000）
    fn mock_gpu_module(root: &Path, id: &str) -> ep_core::module::discovery::DiscoveredModule {
        let toml = format!(
            r#"
[module]
id = "{id}"
name = "Mock GPU"
version = "0.1.0"
description = "test"
category = "image"
genre = "test"
license = "MIT"

[runtime]
type = "python"

[compute]
backends = ["cuda"]
min_vram_mb = 3000

[interface]
type = "http"

[[interface.capabilities]]
name = "process"
description = "处理"
input_type = "image"
output_type = "image"
"#,
            id = id
        );
        ep_core::module::discovery::DiscoveredModule {
            manifest: Some(toml::from_str(&toml).unwrap()),
            path: root.join("modules").join(id),
            status: ep_core::module::discovery::DiscoveryStatus::Valid,
        }
    }

    #[tokio::test]
    async fn forced_overcommit_pools_reports_only_oversubscribed_pool() {
        let _guard = lock_for_tests();
        clear_registry_for_tests();

        let root = unique_root("force-pools");
        let module = mock_gpu_module(&root, "mock-waifu");
        let device = ep_core::types::ComputeDevice {
            id: DeviceId::Cuda(0),
            backend: ep_core::types::ComputeBackend::Cuda,
            name: "Mock GPU".into(),
            total_memory_mb: Some(8192),
            used_memory_mb: Some(6000),
            utilization: None,
            temperature: None,
        };
        let state = Arc::new(AppState::new(
            root.clone(),
            AppConfig::default(),
            vec![device],
            vec![module],
            PortManager::new(18000, 19000),
        ));

        // 净空：6000(实时) + 3000(预算) = 9000 > 8192 → 唯一池超支
        let pools = forced_overcommit_pools(&state, &["mock-waifu".to_string()]).await;
        assert_eq!(pools.len(), 1, "仅超支池入投影");
        let pool = &pools[0];
        assert_eq!(pool.kind, vram::PoolKind::Vram);
        assert_eq!(pool.device_id, "cuda:0");
        assert_eq!(pool.total_mb, 8192);
        assert_eq!(pool.committed_mb, 9000, "committed = max(实时, 账本) + 预算");
        assert_eq!(
            pool.entries,
            vec![vram::PoolItem { node: "mock-waifu".into(), estimate_mb: 3000 }]
        );
        assert!(!pool.unknown);

        // 降容场景：实时占用清零、无账本承诺 → 3000 < 8192 无超支 → 空投影
        {
            let mut devs = state.devices.write().await;
            devs[0].used_memory_mb = Some(0);
        }
        let pools = forced_overcommit_pools(&state, &["mock-waifu".to_string()]).await;
        assert!(pools.is_empty(), "未超支时不出任何池");

        // 未知模块（无准入条目）→ 无预算无超支
        let pools = forced_overcommit_pools(&state, &["ghost-mod".to_string()]).await;
        assert!(pools.is_empty());
    }
}
