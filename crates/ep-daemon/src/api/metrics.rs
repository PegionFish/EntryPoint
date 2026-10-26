//! Prometheus 指标端点（T-32 / Wave2）：`GET /api/metrics`。
//!
//! 手写 exposition format 0.0.4 文本（`Content-Type: text/plain;
//! version=0.0.4; charset=utf-8`），零新依赖（不引 prometheus client crate）。
//! 路由挂在 `/api` 前缀下，与全部 `/api/*` 端点同受 auth.rs 全局访问保护
//! 覆盖（`is_protected_path` 前缀匹配，无任何绕过）。
//!
//! ## 指标清单
//!
//! | 指标 | 类型 | 标签 | 数据源 |
//! |---|---|---|---|
//! | `entrypoint_pool_total_mb` | gauge | pool, device | `state.devices` 容量 + `compute::uma` 识别 |
//! | `entrypoint_pool_committed_mb` | gauge | pool, device | 运行中模块实例的声明预算（见下「口径」） |
//! | `entrypoint_tasks_total` | counter | status | 任务注册表终态计数（`execution::task_registry_all`） |
//! | `entrypoint_queue_running` / `_queued` | gauge | — | `execution::queue_status()`（调度器权威值） |
//! | `entrypoint_queue_paused` | gauge | — | 同上（0/1） |
//! | `entrypoint_modules` | gauge | status | `state.modules` × `process_manager.get_status` |
//!
//! ## 池占用口径（选型说明）
//!
//! §6.8b 准入账本（`execution.rs` 的 `admission_snapshot` /
//! `Scheduler::vram_committed`）为该文件**私有**状态，写范围不允许为其新增
//! 访问器；故采用 **api/pipelines.rs `vram_budget` 同一取数路径**（最薄）：
//! `state.devices`（容量）+ `state.process_manager`（运行中实例落位）+
//! `state.modules` manifest（`resolve_vram_estimate` / `resolve_ram_estimate`，
//! A6 口径：变体级优先、模块级兜底）+ `compute::uma`（T-22 单池折叠）。
//! `committed` 语义 = **运行中模块实例**的声明预算之和（模块粒度，与
//! `forced_overcommit_pools` 的记账粒度一致）；排队任务的已承诺额不计入
//! （账本私有，见「边界」）。
//!
//! 池归集与执行侧准入同语义：
//! - 非 UMA：每块**有容量指标**的 GPU 设备一池（`pool="vram"`）；容量未知
//!   的设备（vulkan/directml 常态）不产出池——准入对其本就 fail-open；
//! - UMA（T-22）：识别成功时全部 GPU 预算折叠进单一 `pool="uma",
//!   device="uma"` 池（容量 = 统一内存物理总量），不产出逐设备 vram 池；
//!   识别失败静默降级为逐设备池（与执行侧零差异）；
//! - RAM（T-20）：单一池（`pool="ram", device="cpu"`），容量取设备快照
//!   CPU 条目的系统内存（缺失记 0），committed = 运行中实例的 ram 估算
//!   求和（主存驻留无跨层释放语义）；
//! - CPU 落位的实例不计 vram（准入同款：CPU 不做显存记账）。
//!
//! ## 任务吞吐口径
//!
//! `entrypoint_tasks_total` 为注册表内**终态记录计数**（completed/failed/
//! cancelled）：任务到达终态即递增；跨重启从 `runtime/tasks/` 回读续计。
//! 两处合法回落（Prometheus 按 counter reset 语义聚合，`rate()` 自愈）：
//! - 启动改判：上次进程退出时遗留的 queued/running 记录改判 failed——
//!   各 status 分量可在重启后跳变（终态总和不变）；
//! - retention 清理（F5 在途改动）：任务目录删除时同步删除注册表记录，
//!   被清理任务的终态计数随之移除。
//!
//! 不做时间序列，速率/窗口聚合交给 Prometheus（`rate()`/`increase()`）。
//!
//! ## 边界（本期不实现）
//!
//! - 排队任务的已承诺显存额：`Scheduler::vram_committed` 私有，需
//!   execution.rs 暴露只读访问器后方可补计（届时本文件无需改结构）；
//! - 自动重启次数 counter：health_supervisor 无可读状态（重启事实只落
//!   统一事件日志 `module_auto_restart` 事件），扫描日志做 scrape 期聚合
//!   代价不成立——待 supervisor 暴露计数器后追加
//!   `entrypoint_module_auto_restarts_total`；
//! - external 池指标（openai-bridge 外部算力源）：M2 后扩展，本期占位
//!   不产出（届时以 `pool="external"` 标签接入，复用本文件渲染层）。
//!
//! 数据源全部为进程内内存读（UMA 探测有进程级缓存），无文件 IO，
//! 不需要 spawn_blocking（对照 storage.rs 先例的适用条件）。

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

use ep_core::task_registry::TaskState;
use ep_core::types::{ComputeDevice, DeviceId, ServiceStatus};

use crate::api::execute::execution::{self, QueueStatus};
use crate::state::AppState;

/// Prometheus 文本格式（exposition format 0.0.4）响应 Content-Type。
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// `/api/metrics` 路由树（相对路径，随 api_router 挂到 `/api/metrics`）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/metrics", get(metrics_handler))
}

// ─── 渲染层（纯函数，单测直接消费） ─────────────────────────────────────────

/// 指标类型（exposition format 0.0.4 词表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetricKind {
    Gauge,
    Counter,
}

impl MetricKind {
    fn as_str(self) -> &'static str {
        match self {
            MetricKind::Gauge => "gauge",
            MetricKind::Counter => "counter",
        }
    }
}

/// 单条样本：label 序（输出顺序即书写顺序）+ 值。
struct Sample {
    labels: Vec<(&'static str, String)>,
    value: u64,
}

impl Sample {
    fn labels(labels: Vec<(&'static str, String)>, value: u64) -> Self {
        Self { labels, value }
    }

    fn bare(value: u64) -> Self {
        Self { labels: Vec::new(), value }
    }
}

/// 一个 metric family：`# HELP` + `# TYPE` + 若干样本行。
struct Family {
    name: &'static str,
    help: &'static str,
    kind: MetricKind,
    samples: Vec<Sample>,
}

/// label 值转义（exposition format：`\` → `\\`、`"` → `\"`、换行 → `\n`）。
/// 先替换反斜杠，后续插入的转义反斜杠不会被二次转义。
fn escape_label_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// HELP 文本转义（`\` 与换行；引号在 HELP 中合法，不转义）。
fn escape_help(help: &str) -> String {
    help.replace('\\', "\\\\").replace('\n', "\\n")
}

/// 渲染整个响应体：每个 family 恒输出 HELP/TYPE 头（空样本也输出，
/// 保证抓取端序列形状稳定）；无 label 样本不带 `{}`；行尾 `\n` 收尾。
fn render_families(families: &[Family]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for family in families {
        let _ = writeln!(out, "# HELP {} {}", family.name, escape_help(family.help));
        let _ = writeln!(out, "# TYPE {} {}", family.name, family.kind.as_str());
        for sample in &family.samples {
            if sample.labels.is_empty() {
                let _ = writeln!(out, "{} {}", family.name, sample.value);
            } else {
                let labels = sample
                    .labels
                    .iter()
                    .map(|(k, v)| format!("{k}=\"{}\"", escape_label_value(v)))
                    .collect::<Vec<_>>()
                    .join(",");
                let _ = writeln!(out, "{}{{{}}} {}", family.name, labels, sample.value);
            }
        }
    }
    out
}

// ─── 聚合层（纯函数：已知状态 → 指标值，单测注入断言） ─────────────────────

/// 单池样本（渲染前中间形态；pool 词表 = "vram" | "ram" | "uma"）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct PoolSample {
    pool: &'static str,
    device: String,
    total_mb: u64,
    committed_mb: u64,
}

/// 运行中实例的声明预算（MB；A6 口径，与 vram_budget 同源解析）。
#[derive(Debug, Clone, Copy, Default)]
struct ModuleEstimates {
    vram_mb: Option<u64>,
    ram_mb: Option<u64>,
}

/// 池账本聚合（纯函数）。
///
/// 输入：设备快照（容量）、运行中实例（module_id + 落位设备）、声明预算表、
/// UMA 识别结果。输出按 (pool, device) 字典序稳定排序。
///
/// - ram 池恒产出（稳定序列）：容量 = CPU 条目 `total_memory_mb`（缺失记 0）；
/// - UMA 识别成功：单一 uma 池（容量 = 统一内存总量），GPU 预算折叠计入，
///   不产出逐设备 vram 池（与执行侧准入折叠同语义）；
/// - 非 UMA：每块有容量的 GPU 设备一池；无容量设备的实例预算按准入
///   fail-open 边界丢弃（无处可记 ≠ 记 0）；
/// - CPU 落位实例的 vram 估算不计（准入同款）。
fn aggregate_pools(
    devices: &[ComputeDevice],
    running: &[(String, Option<DeviceId>)],
    estimates: &HashMap<String, ModuleEstimates>,
    uma_total_mb: Option<u32>,
) -> Vec<PoolSample> {
    // (pool, device) → (total_mb, committed_mb)；BTreeMap 保证输出稳定序
    let mut pools: BTreeMap<(&'static str, String), (u64, u64)> = BTreeMap::new();

    // ram 池：单一池，容量取 CPU 条目系统内存（T-20 数据源约定）
    let ram_total_mb = devices
        .iter()
        .find(|d| matches!(d.id, DeviceId::Cpu))
        .and_then(|d| d.total_memory_mb)
        .map(u64::from)
        .unwrap_or(0);
    pools.insert(("ram", "cpu".to_string()), (ram_total_mb, 0));

    // UMA 单池（T-22）：识别成功即产出（物理平台属性，与有无实例无关）
    if let Some(uma) = uma_total_mb {
        pools.insert(("uma", "uma".to_string()), (u64::from(uma), 0));
    } else {
        // 逐设备 vram 池：仅收有容量指标的加速设备（CPU 不做显存记账）
        for d in devices {
            if matches!(d.id, DeviceId::Cpu) {
                continue;
            }
            let Some(total) = d.total_memory_mb else {
                continue; // 容量未知 → 准入 fail-open，同样不产出池
            };
            pools.insert(("vram", d.id.to_string()), (u64::from(total), 0));
        }
    }

    // 运行中实例的声明预算记账（模块粒度）
    for (module_id, device) in running {
        let Some(est) = estimates.get(module_id) else {
            continue; // 无 manifest（解析失败/已移除）→ 无声明预算可记
        };
        if let Some(ram) = est.ram_mb {
            let entry = pools.get_mut(&("ram", "cpu".to_string()));
            if let Some((_, committed)) = entry {
                *committed = committed.saturating_add(ram);
            }
        }
        if let (Some(vram), Some(dev)) = (est.vram_mb, device) {
            // CPU 落位不记显存；UMA 平台全部 GPU 预算折叠进单一池
            let key = if matches!(dev, DeviceId::Cpu) {
                continue;
            } else if uma_total_mb.is_some() {
                ("uma", "uma".to_string())
            } else {
                ("vram", dev.to_string())
            };
            if let Some((_, committed)) = pools.get_mut(&key) {
                *committed = committed.saturating_add(vram);
            }
        }
    }

    pools
        .into_iter()
        .map(|((pool, device), (total_mb, committed_mb))| PoolSample {
            pool,
            device,
            total_mb,
            committed_mb,
        })
        .collect()
}

/// 模块生命周期状态三分类（`entrypoint_modules{status}` 的 status 词表）：
/// - running：进程存活（Running / Starting）；
/// - error：上次启动失败（Error）；
/// - stopped：其余（Stopped / Preparing / NotReady / 无实例记录）。
///
/// Preparing（准备环境/下载模型）归入 stopped：进程未起、未对外服务。
fn classify_module_status(status: Option<&ServiceStatus>) -> &'static str {
    match status {
        Some(ServiceStatus::Running | ServiceStatus::Starting) => "running",
        Some(ServiceStatus::Error(_)) => "error",
        _ => "stopped",
    }
}

/// 任务终态计数（status 词表与 `TaskState::as_str` 一致；queued/running
/// 非终态不计入——在途量由 `entrypoint_queue_*` 表达）。
fn terminal_task_counts(statuses: impl Iterator<Item = TaskState>) -> BTreeMap<&'static str, u64> {
    let mut counts: BTreeMap<&'static str, u64> =
        ["cancelled", "completed", "failed"].map(|s| (s, 0)).into_iter().collect();
    for status in statuses {
        let bucket = match status {
            TaskState::Completed => "completed",
            TaskState::Failed => "failed",
            TaskState::Cancelled => "cancelled",
            TaskState::Queued | TaskState::Running => continue,
        };
        *counts.get_mut(bucket).expect("bucket pre-seeded") += 1;
    }
    counts
}

/// 模块状态计数（status 词表固定三项，和 = 可解析 manifest 的模块总数）。
fn module_status_counts(
    statuses: impl Iterator<Item = &'static str>,
) -> BTreeMap<&'static str, u64> {
    let mut counts: BTreeMap<&'static str, u64> =
        ["error", "running", "stopped"].map(|s| (s, 0)).into_iter().collect();
    for status in statuses {
        *counts.get_mut(status).expect("status 词表封闭") += 1;
    }
    counts
}

// ─── Handler ────────────────────────────────────────────────────────────────

/// GET /api/metrics — Prometheus 抓取端点（模块头注释见指标清单与口径）。
async fn metrics_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // 数据快照：各读锁短临界区逐段取、不跨 await 持锁（vram_budget 同款）。
    let devices = state.devices.read().await.clone();

    // 可解析 manifest 的模块 = 健康度计数的总体（解析失败者无 id 可键）
    let manifests: HashMap<String, ep_core::module::manifest::ModuleManifest> = state
        .modules
        .read()
        .await
        .iter()
        .filter_map(|m| m.manifest.clone().map(|mf| (mf.module.id.clone(), mf)))
        .collect();
    let module_ids: Vec<String> = manifests.keys().cloned().collect();

    let (running, module_statuses) = {
        let pm = state.process_manager.read().await;
        let running: Vec<(String, Option<DeviceId>)> = pm
            .list_running()
            .into_iter()
            .map(|i| (i.module_id.clone(), i.device.clone()))
            .collect();
        let module_statuses: Vec<&'static str> = module_ids
            .iter()
            .map(|id| classify_module_status(pm.get_status(id)))
            .collect();
        (running, module_statuses)
    };

    let active_models = state.config.read().await.active_models.clone();

    // 运行中实例的声明预算（去重后逐模块解析；变体选择与 vram_budget
    // 同一三级回退：active_models → default 变体 → 首个变体）
    let mut estimates: HashMap<String, ModuleEstimates> = HashMap::new();
    for (module_id, _) in &running {
        if estimates.contains_key(module_id) {
            continue;
        }
        let Some(mf) = manifests.get(module_id) else {
            continue;
        };
        let variant = active_models
            .get(module_id)
            .cloned()
            .or_else(|| {
                mf.models
                    .iter()
                    .find(|m| m.default)
                    .or(mf.models.first())
                    .map(|m| m.id.clone())
            })
            .unwrap_or_default();
        estimates.insert(
            module_id.clone(),
            ModuleEstimates {
                vram_mb: mf.resolve_vram_estimate(&variant),
                ram_mb: mf.resolve_ram_estimate(&variant),
            },
        );
    }

    let pools = aggregate_pools(
        &devices,
        &running,
        &estimates,
        ep_core::compute::uma::unified_memory_total_mb(),
    );

    let task_counts =
        terminal_task_counts(execution::task_registry_all().into_iter().map(|r| r.status));
    let QueueStatus { paused, running: queue_running, queued: queue_queued } =
        execution::queue_status();
    let module_counts = module_status_counts(module_statuses.into_iter());

    let families = build_families(&pools, &task_counts, paused, queue_running, queue_queued, &module_counts);
    let body = render_families(&families);
    (StatusCode::OK, [(header::CONTENT_TYPE, CONTENT_TYPE)], body)
}

/// 组装 metric families（纯函数；空状态各 family 恒存在且为零值/空样本，
/// 抓取端序列形状稳定）。
fn build_families(
    pools: &[PoolSample],
    task_counts: &BTreeMap<&'static str, u64>,
    paused: bool,
    queue_running: usize,
    queue_queued: usize,
    module_counts: &BTreeMap<&'static str, u64>,
) -> Vec<Family> {
    vec![
        Family {
            name: "entrypoint_pool_total_mb",
            help: "Configured capacity of each resource pool in MB (ram total falls back to 0 when the system memory probe is unavailable).",
            kind: MetricKind::Gauge,
            samples: pools
                .iter()
                .map(|p| {
                    Sample::labels(
                        vec![("pool", p.pool.to_string()), ("device", p.device.clone())],
                        p.total_mb,
                    )
                })
                .collect(),
        },
        Family {
            name: "entrypoint_pool_committed_mb",
            help: "Sum of declared memory estimates of running module instances charged to each pool in MB (module granularity; queued tasks not included).",
            kind: MetricKind::Gauge,
            samples: pools
                .iter()
                .map(|p| {
                    Sample::labels(
                        vec![("pool", p.pool.to_string()), ("device", p.device.clone())],
                        p.committed_mb,
                    )
                })
                .collect(),
        },
        Family {
            name: "entrypoint_tasks_total",
            help: "Terminal task counts by status from the task registry (rises as tasks finish; may drop on restart re-judgement or retention record pruning).",
            kind: MetricKind::Counter,
            samples: task_counts
                .iter()
                .map(|(status, count)| Sample::labels(vec![("status", (*status).to_string())], *count))
                .collect(),
        },
        Family {
            name: "entrypoint_queue_running",
            help: "Pipeline tasks currently running (scheduler view).",
            kind: MetricKind::Gauge,
            samples: vec![Sample::bare(queue_running as u64)],
        },
        Family {
            name: "entrypoint_queue_queued",
            help: "Pipeline tasks waiting for dispatch (scheduler view).",
            kind: MetricKind::Gauge,
            samples: vec![Sample::bare(queue_queued as u64)],
        },
        Family {
            name: "entrypoint_queue_paused",
            help: "1 if global task dispatch is paused (T-15), 0 otherwise.",
            kind: MetricKind::Gauge,
            samples: vec![Sample::bare(u64::from(paused))],
        },
        Family {
            name: "entrypoint_modules",
            help: "Discovered module count by lifecycle status (running = process alive; error = last start failed; stopped = everything else).",
            kind: MetricKind::Gauge,
            samples: module_counts
                .iter()
                .map(|(status, count)| Sample::labels(vec![("status", (*status).to_string())], *count))
                .collect(),
        },
    ]
}

// ─── 测试 ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use ep_core::config::AppConfig;
    use ep_core::module::manifest::ModuleManifest;
    use ep_core::port::PortManager;
    use ep_core::types::{ComputeBackend, ComputeDevice};

    use crate::state::AppState;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn test_state() -> Arc<AppState> {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        Arc::new(AppState::new(
            std::env::temp_dir().join(format!("ep-api-metrics-{}-{seq}", std::process::id())),
            AppConfig::default(),
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        ))
    }

    fn cuda_device(index: u32, total_mb: Option<u32>) -> ComputeDevice {
        ComputeDevice {
            id: DeviceId::Cuda(index),
            backend: ComputeBackend::Cuda,
            name: format!("GPU-{index}"),
            total_memory_mb: total_mb,
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        }
    }

    fn cpu_device(total_mb: Option<u32>) -> ComputeDevice {
        ComputeDevice {
            id: DeviceId::Cpu,
            backend: ComputeBackend::Cpu,
            name: "CPU".to_string(),
            total_memory_mb: total_mb,
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        }
    }

    // ── 转义与渲染 ─────────────────────────────────────────────────────────

    // label 值转义：反斜杠、双引号、换行（先反斜杠的次序保证不二次转义）
    #[test]
    fn escape_label_value_escapes_backslash_quote_newline() {
        assert_eq!(escape_label_value("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
        assert_eq!(escape_label_value("plain"), "plain");
        assert_eq!(escape_label_value(""), "");
    }

    // HELP 转义：反斜杠与换行；引号合法保留
    #[test]
    fn escape_help_escapes_backslash_and_newline_only() {
        assert_eq!(escape_help("a\\b\nc\"d"), "a\\\\b\\nc\"d");
    }

    // 渲染：每 family 恒有 HELP/TYPE；无 label 样本不带 {}；label 转义落地
    #[test]
    fn render_families_produces_help_type_and_escaped_samples() {
        let families = vec![
            Family {
                name: "entrypoint_pool_total_mb",
                help: "capacity\\mb\nline",
                kind: MetricKind::Gauge,
                samples: vec![
                    Sample::labels(
                        vec![("pool", "vram".into()), ("device", "cuda:0".into())],
                        8192,
                    ),
                    Sample::labels(
                        vec![("pool", "ram".into()), ("device", "nasty\"q\\x".into())],
                        0,
                    ),
                ],
            },
            Family {
                name: "entrypoint_queue_paused",
                help: "paused flag",
                kind: MetricKind::Gauge,
                samples: vec![Sample::bare(1)],
            },
        ];
        let body = render_families(&families);
        let expected = "# HELP entrypoint_pool_total_mb capacity\\\\mb\\nline\n\
                        # TYPE entrypoint_pool_total_mb gauge\n\
                        entrypoint_pool_total_mb{pool=\"vram\",device=\"cuda:0\"} 8192\n\
                        entrypoint_pool_total_mb{pool=\"ram\",device=\"nasty\\\"q\\\\x\"} 0\n\
                        # HELP entrypoint_queue_paused paused flag\n\
                        # TYPE entrypoint_queue_paused gauge\n\
                        entrypoint_queue_paused 1\n";
        assert_eq!(body, expected);
    }

    // ── 池聚合：非 UMA（逐设备池 + CPU 不记显存 + 无容量设备丢弃） ──────────

    #[test]
    fn aggregate_pools_non_uma_attributes_committed_by_device() {
        let devices = vec![
            cuda_device(0, Some(8192)),
            // cuda:1 无容量指标（探测缺失）→ 不产出池
            cuda_device(1, None),
            cpu_device(Some(32768)),
        ];
        let running = vec![
            ("asr".to_string(), Some(DeviceId::Cuda(0))),
            ("upscale".to_string(), Some(DeviceId::Cuda(1))),
            ("cpu-mod".to_string(), Some(DeviceId::Cpu)),
        ];
        let mut estimates = HashMap::new();
        estimates.insert(
            "asr".to_string(),
            ModuleEstimates { vram_mb: Some(3000), ram_mb: Some(500) },
        );
        estimates.insert(
            "upscale".to_string(),
            ModuleEstimates { vram_mb: Some(1000), ram_mb: None },
        );
        estimates.insert(
            "cpu-mod".to_string(),
            ModuleEstimates { vram_mb: Some(999), ram_mb: Some(200) },
        );

        let pools = aggregate_pools(&devices, &running, &estimates, None);

        // cuda:1 无容量 → 池不存在，其上实例预算随之丢弃（fail-open 边界）
        assert_eq!(
            pools,
            vec![
                PoolSample {
                    pool: "ram",
                    device: "cpu".into(),
                    total_mb: 32768,
                    // 500(asr) + 200(cpu-mod)；upscale 无 ram 声明不计
                    committed_mb: 700,
                },
                PoolSample {
                    pool: "vram",
                    device: "cuda:0".into(),
                    total_mb: 8192,
                    committed_mb: 3000,
                },
            ]
        );
    }

    // UMA 单池折叠（T-22）：GPU 预算全部进单一 uma 池，不产出逐设备 vram 池
    #[test]
    fn aggregate_pools_uma_folds_gpu_budgets_into_single_pool() {
        let devices = vec![cuda_device(0, Some(8192)), cpu_device(Some(32768))];
        let running = vec![
            ("asr".to_string(), Some(DeviceId::Cuda(0))),
            ("llm".to_string(), Some(DeviceId::Cpu)),
        ];
        let mut estimates = HashMap::new();
        estimates.insert(
            "asr".to_string(),
            ModuleEstimates { vram_mb: Some(3000), ram_mb: Some(500) },
        );
        estimates.insert(
            "llm".to_string(),
            ModuleEstimates { vram_mb: Some(7000), ram_mb: Some(200) },
        );

        let pools = aggregate_pools(&devices, &running, &estimates, Some(131_072));

        assert_eq!(
            pools,
            vec![
                PoolSample { pool: "ram", device: "cpu".into(), total_mb: 32768, committed_mb: 700 },
                // 3000(asr@cuda:0) 折叠；llm 的 7000 为 CPU 落位 → 不计
                PoolSample { pool: "uma", device: "uma".into(), total_mb: 131_072, committed_mb: 3000 },
            ]
        );
    }

    // 空状态：仅恒产出的 ram 池（零容量零承诺），输出稳定非空
    #[test]
    fn aggregate_pools_empty_state_yields_zero_ram_pool_only() {
        let pools = aggregate_pools(&[], &[], &HashMap::new(), None);
        assert_eq!(
            pools,
            vec![PoolSample { pool: "ram", device: "cpu".into(), total_mb: 0, committed_mb: 0 }]
        );
    }

    // ── 健康度分类与计数 ───────────────────────────────────────────────────

    // ServiceStatus 六态 → 三桶（Running/Starting → running；Error → error；
    // 其余 + 无实例记录 → stopped）
    #[test]
    fn classify_module_status_buckets_all_variants() {
        assert_eq!(classify_module_status(Some(&ServiceStatus::Running)), "running");
        assert_eq!(classify_module_status(Some(&ServiceStatus::Starting)), "running");
        assert_eq!(classify_module_status(Some(&ServiceStatus::Error("boom".into()))), "error");
        assert_eq!(classify_module_status(Some(&ServiceStatus::Stopped)), "stopped");
        assert_eq!(classify_module_status(Some(&ServiceStatus::Preparing)), "stopped");
        assert_eq!(classify_module_status(Some(&ServiceStatus::NotReady)), "stopped");
        assert_eq!(classify_module_status(None), "stopped");
    }

    // 计数：词表封闭（和 = 总体），未知词表触达不了（classify 产出封闭）
    #[test]
    fn module_status_counts_are_seeded_for_every_status() {
        let counts = module_status_counts(["running", "stopped", "stopped", "error"].into_iter());
        assert_eq!(counts.get("running"), Some(&1));
        assert_eq!(counts.get("stopped"), Some(&2));
        assert_eq!(counts.get("error"), Some(&1));
        let empty = module_status_counts(std::iter::empty());
        assert_eq!(empty.values().sum::<u64>(), 0);
        assert_eq!(empty.len(), 3, "空状态也必须三项全零（序列形状稳定）");
    }

    // 任务终态计数：queued/running 非终态不计；三桶恒预置为零值
    #[test]
    fn terminal_task_counts_ignore_non_terminal_states() {
        let counts = terminal_task_counts(
            [
                TaskState::Completed,
                TaskState::Completed,
                TaskState::Failed,
                TaskState::Cancelled,
                TaskState::Queued,
                TaskState::Running,
            ]
            .into_iter(),
        );
        assert_eq!(counts.get("completed"), Some(&2));
        assert_eq!(counts.get("failed"), Some(&1));
        assert_eq!(counts.get("cancelled"), Some(&1));

        let empty = terminal_task_counts(std::iter::empty());
        assert_eq!(empty.len(), 3);
        assert_eq!(empty.values().sum::<u64>(), 0);
    }

    // ── families 组装 + 全量输出合法性（无状态纯渲染路径） ──────────────────

    // 已知状态 → 断言输出数值：build_families + render 的组装正确性
    #[test]
    fn build_families_maps_known_state_to_values() {
        let pools = vec![
            PoolSample { pool: "ram", device: "cpu".into(), total_mb: 32768, committed_mb: 700 },
            PoolSample { pool: "vram", device: "cuda:0".into(), total_mb: 8192, committed_mb: 3000 },
        ];
        let mut tasks = BTreeMap::new();
        tasks.insert("cancelled", 1u64);
        tasks.insert("completed", 2u64);
        tasks.insert("failed", 0u64);
        let mut modules = BTreeMap::new();
        modules.insert("error", 0u64);
        modules.insert("running", 1u64);
        modules.insert("stopped", 3u64);

        let families = build_families(&pools, &tasks, true, 2, 5, &modules);
        let body = render_families(&families);
        let value = |line_prefix: &str| -> u64 {
            body.lines()
                .find(|l| l.starts_with(line_prefix))
                .unwrap_or_else(|| panic!("missing sample line {line_prefix} in:\n{body}"))
                .rsplit(' ')
                .next()
                .unwrap()
                .parse::<u64>()
                .expect("numeric value")
        };

        assert_eq!(value("entrypoint_pool_total_mb{pool=\"vram\",device=\"cuda:0\"}"), 8192);
        assert_eq!(value("entrypoint_pool_committed_mb{pool=\"vram\",device=\"cuda:0\"}"), 3000);
        assert_eq!(value("entrypoint_pool_committed_mb{pool=\"ram\",device=\"cpu\"}"), 700);
        assert_eq!(value("entrypoint_tasks_total{status=\"completed\"}"), 2);
        assert_eq!(value("entrypoint_tasks_total{status=\"failed\"}"), 0, "零值也必须产出");
        assert_eq!(value("entrypoint_queue_running "), 2);
        assert_eq!(value("entrypoint_queue_queued "), 5);
        assert_eq!(value("entrypoint_queue_paused "), 1);
        assert_eq!(value("entrypoint_modules{status=\"running\"}"), 1);
        assert_eq!(value("entrypoint_modules{status=\"stopped\"}"), 3);
    }

    // ── 路由级：空状态输出合法且为零值（池/模块为 state 局部，可精确断言） ────

    /// 解析后的单条样本：(label 表, 数值)。
    type ParsedSample = (HashMap<String, String>, f64);

    /// exposition format 0.0.4 解析器（测试内用）：逐行校验 HELP/TYPE/样本
    /// 语法，返回 family 名 → 样本表。
    fn parse_exposition(body: &str) -> HashMap<String, Vec<ParsedSample>> {
        let mut families: HashMap<String, Vec<ParsedSample>> = HashMap::new();
        let mut help_seen: Vec<String> = Vec::new();
        let mut type_seen: Vec<String> = Vec::new();
        let mut last_family: Option<String> = None;

        for line in body.lines() {
            if let Some(rest) = line.strip_prefix("# ") {
                let (kind, rest) = rest.split_once(' ').expect("HELP/TYPE keyword");
                let (name, _) = rest.split_once(' ').expect("family name + text");
                assert!(!name.contains('{'), "family 名不得含 label");
                match kind {
                    "HELP" => help_seen.push(name.to_string()),
                    "TYPE" => {
                        let (_, kind_value) = rest.split_once(' ').expect("TYPE value");
                        assert!(
                            matches!(kind_value, "gauge" | "counter" | "histogram" | "summary" | "untyped"),
                            "非法 TYPE 词表: {kind_value}"
                        );
                        type_seen.push(name.to_string());
                    }
                    other => panic!("未知注释行: #{other}"),
                }
                last_family = Some(name.to_string());
                continue;
            }
            assert!(!line.trim().is_empty(), "不得出现空行: {body:?}");
            let (name_and_labels, value) = line.rsplit_once(' ').expect("样本行须有空格分隔值");
            let metric_name: String;
            let mut labels = HashMap::new();
            if let Some((name, label_text)) = name_and_labels.split_once('{') {
                metric_name = name.to_string();
                let label_text = label_text.strip_suffix('}').expect("label 区闭合");
                if !label_text.is_empty() {
                    for pair in label_text.split("\",") {
                        let pair = pair.strip_suffix('"').unwrap_or(pair);
                        let (k, v) = pair.split_once("=\"").expect("label 形如 k=\"v\"");
                        labels.insert(k.to_string(), v.to_string());
                    }
                }
            } else {
                metric_name = name_and_labels.to_string();
            }
            assert!(
                metric_name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == ':'),
                "metric 名首字符非法: {metric_name}"
            );
            assert!(
                metric_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':'),
                "metric 名字符集非法: {metric_name}"
            );
            let parsed: f64 = value.parse().expect("值须为合法数字");
            assert!(parsed.is_finite(), "值须有限");
            let bucket = families.entry(metric_name.clone()).or_default();
            bucket.push((labels, parsed));
            assert_eq!(
                last_family.as_deref(),
                Some(metric_name.as_str()),
                "样本必须紧跟其 HELP/TYPE 头: {line}"
            );
        }

        // 每个 family 恰好一组 HELP + TYPE（T-32 验收：每个 metric 行 HELP/TYPE）
        for name in families.keys() {
            assert_eq!(
                help_seen.iter().filter(|n| *n == name).count(),
                1,
                "{name} 的 HELP 必须恰一次"
            );
            assert_eq!(
                type_seen.iter().filter(|n| *n == name).count(),
                1,
                "{name} 的 TYPE 必须恰一次"
            );
        }
        assert_eq!(help_seen.len(), type_seen.len(), "HELP/TYPE 数量对齐");
        families
    }

    // 空状态（无模块无设备 + 新注册表）：200 + 指定 Content-Type；池/模块
    // 精确为零值；全输出过 0.0.4 语法校验。queue/tasks 为进程级全局状态
    // （并发测试可能写入），只做结构断言不做精确值断言。
    #[tokio::test]
    async fn metrics_route_empty_state_is_valid_and_zero() {
        let state = test_state();
        // 经完整 api_router：证明 /metrics 已 merge 进路由树（/api 前缀 +
        // auth.rs 全局访问保护域由 build_app_router 的 nest + 中间件结构保证）
        let app = crate::api::api_router(state.clone()).with_state(state);
        let req = Request::builder().uri("/metrics").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").expect("content-type"),
            CONTENT_TYPE
        );

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body = std::str::from_utf8(&bytes).expect("utf-8 body");
        let families = parse_exposition(body);

        // 池：恒产出的 ram 池为零值；UMA 机器上另有 uma 池（容量 = 探测值）
        let ram = families["entrypoint_pool_total_mb"]
            .iter()
            .find(|(l, _)| l.get("pool").map(String::as_str) == Some("ram"))
            .expect("ram 池恒产出")
            .1;
        assert_eq!(ram, 0.0);
        let ram_committed = families["entrypoint_pool_committed_mb"]
            .iter()
            .find(|(l, _)| l.get("pool").map(String::as_str) == Some("ram"))
            .expect("ram 池恒产出")
            .1;
        assert_eq!(ram_committed, 0.0);
        let uma_total = ep_core::compute::uma::unified_memory_total_mb().map(u64::from);
        assert_eq!(
            families["entrypoint_pool_total_mb"]
                .iter()
                .any(|(l, _)| l.get("pool").map(String::as_str) == Some("uma")),
            uma_total.is_some(),
            "uma 池产出与平台识别结果一致"
        );

        // 任务：三桶恒产出（数值可能被同进程并发测试污染，只断言存在）
        let task_statuses: Vec<&str> = families["entrypoint_tasks_total"]
            .iter()
            .filter_map(|(l, _)| l.get("status").map(String::as_str))
            .collect();
        assert_eq!(task_statuses.len(), 3, "三终态桶恒产出: {task_statuses:?}");
        for status in ["cancelled", "completed", "failed"] {
            assert!(task_statuses.contains(&status), "缺 {status} 桶");
        }

        // 模块：三桶全零（state 局部，空状态精确断言）
        for (labels, value) in &families["entrypoint_modules"] {
            assert_eq!(*value, 0.0, "空状态模块计数应为零: {labels:?}");
        }

        // 队列：三序列恒产出
        for name in [
            "entrypoint_queue_running",
            "entrypoint_queue_queued",
            "entrypoint_queue_paused",
        ] {
            assert_eq!(families[name].len(), 1, "{name} 恒产出一条无 label 样本");
        }
    }

    // 清单变形（manifest fixture）：运行实例经 vram_budget 同款回退解析预算
    // ——resolve_* 的接线正确性（变体级优先 → 模块级兜底）
    #[test]
    fn manifest_estimates_follow_variant_then_module_fallback() {
        let mf: ModuleManifest = toml::from_str(
            r#"
[module]
id = "demo-mod"
name = "t"
version = "0.1.0"
description = "t"
category = "image"
genre = "test"

[runtime]
type = "native"
binaries = { "x" = "x" }

[compute]
backends = ["cpu"]
vram_estimate_mb = 1000
ram_estimate_mb = 256

[[models]]
id = "small"
name = "small"
source = "url"
url = "https://example.com/s.bin"
target_dir = "s"
default = true

[[models]]
id = "large"
name = "large"
source = "url"
url = "https://example.com/l.bin"
target_dir = "l"
vram_estimate_mb = 8192
ram_estimate_mb = 1024

[interface]
type = "http"
"#,
        )
        .unwrap();
        // 变体级优先（large 声明了独立估算）
        assert_eq!(mf.resolve_vram_estimate("large"), Some(8192));
        assert_eq!(mf.resolve_ram_estimate("large"), Some(1024));
        // 未声明变体的字段回退模块级（small 无变体级声明）
        assert_eq!(mf.resolve_vram_estimate("small"), Some(1000));
        assert_eq!(mf.resolve_ram_estimate("small"), Some(256));
    }
}
