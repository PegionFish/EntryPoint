use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use ep_core::config::AppConfig;
use ep_core::model::{DownloadHandle, ModelManager, ModelStatus};
use ep_core::module::manifest::{ModuleManifest, ParamSchema, RuntimeType};
use ep_core::module::{DiscoveredModule, ModelDecl, ModelSource};
use ep_core::pipeline::dag::{Edge, NodeKind, Pipeline, PipelineNode};
use ep_core::pipeline::runner::TaskDetail;
use ep_core::pipeline::PipelineRunnerImpl;
use ep_core::port::PortManager;
use ep_core::process::ProcessManager;
use ep_core::task_registry::{NodeRecord, TaskRecord, TaskRegistry, TaskState};
use ep_core::types::{
    Artifact, ComputeBackend, ComputeDevice, DeviceId, DeviceScheduler, PipelineRunner,
    SchedulingStrategy, ServiceStatus, TaskStatus,
};

/// 启动后首轮自动更新检查延迟：避开启动高峰的 I/O 与网络争用
///（语义对齐 daemon updates.rs STARTUP_CHECK_DELAY）
const STARTUP_CHECK_DELAY: Duration = Duration::from_secs(15);

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "entrypoint=info,ep_core=info".into()),
        )
        .init();

    tracing::info!("EntryPoint starting...");

    // Resolve project root and config directory
    let root = ep_core::config::resolve_root();
    let config_dir = root.join("config");
    let _ = std::fs::create_dir_all(&config_dir);

    // Load config on main thread before spawning anything
    let config = ep_core::config::AppConfig::load_or_create(&config_dir)
        .unwrap_or_default();

    // 克隆一份配置供 UI 使用（原配置随后移入后台线程）
    let ui_config = config.clone();

    // mpsc channel: background → UI
    let (tx, rx) = std::sync::mpsc::channel();

    // unbounded channel: UI → background (tokio unbounded is Send + Clone)
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();

    // Spawn tokio runtime on a dedicated background thread
    let bg_tx = tx.clone();
    let bg_root = root.clone();
    // #51 延后项接线：启动自动更新检查 —— 后台线程初始化时读一次开关，
    // 开启则延迟 STARTUP_CHECK_DELAY 后向命令通道发一次 CheckAllUpdates
    //（复用既有 handler；运行期改开关不热跟随，重启生效，与 daemon 不同）
    let bg_cmd_tx = cmd_tx.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        if config.general.check_updates {
            let cmd_tx = bg_cmd_tx.clone();
            rt.spawn(async move {
                tokio::time::sleep(STARTUP_CHECK_DELAY).await;
                let _ = cmd_tx.send(ep_desktop::app::AppCmd::CheckAllUpdates);
            });
        }
        rt.block_on(background_loop(bg_tx, cmd_rx, config, bg_root));
    });

    // eframe runs on the main thread
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("EntryPoint")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "EntryPoint",
        native_options,
        Box::new(move |cc| {
            configure_fonts(&cc.egui_ctx);
            // 应用配置中的字体大小与整体缩放（egui 0.31 的 API 为 set_zoom_factor）
            ep_desktop::theme::apply_font_size(&cc.egui_ctx, ui_config.ui.font_size);
            cc.egui_ctx.set_zoom_factor(ui_config.ui.scale_factor);
            Ok(Box::new(ep_desktop::App::new(rx, cmd_tx, ui_config)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))
}

/// Load CJK fonts so Chinese text renders correctly.
fn configure_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // Try system CJK fonts in order of preference (Windows + Linux)
    let cjk_font_paths = [
        // Windows
        "C:\\Windows\\Fonts\\msyh.ttc",   // Microsoft YaHei
        "C:\\Windows\\Fonts\\msyhbd.ttc",  // Microsoft YaHei Bold
        "C:\\Windows\\Fonts\\simsun.ttc",  // SimSun
        "C:\\Windows\\Fonts\\simhei.ttf",  // SimHei
        // Linux
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/wqy-microhei/wqy-microhei.ttc",
    ];

    for path in &cjk_font_paths {
        if let Ok(font_data) = std::fs::read(path) {
            fonts.font_data.insert(
                "cjk".to_owned(),
                egui::FontData::from_owned(font_data).into(),
            );
            // Prepend CJK font to all font families so it's used for CJK glyphs
            for family in [
                egui::FontFamily::Proportional,
                egui::FontFamily::Monospace,
            ] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .insert(0, "cjk".to_owned());
            }
            tracing::info!("Loaded CJK font from {path}");
            break;
        }
    }

    ctx.set_fonts(fonts);
}

/// 从模块发现结果中提取有效的 manifest 列表
fn manifests_from(
    discovered: &[ep_core::module::DiscoveredModule],
) -> Vec<ep_core::module::ModuleManifest> {
    discovered.iter().filter_map(|m| m.manifest.clone()).collect()
}

// ─── Wave 3 C4：设备调度 / 任务 / 直跑 纯函数助手 ───────────────────────────

/// 配置策略 → 调度器策略（`[compute].strategy` 接线 ComputeScheduler，P1-1）
fn scheduling_strategy_for(config: &AppConfig) -> SchedulingStrategy {
    match config.compute.resolved_strategy() {
        ep_core::config::AssignStrategy::Manual => SchedulingStrategy::Manual,
        ep_core::config::AssignStrategy::LeastMemory => SchedulingStrategy::LeastMemory,
        ep_core::config::AssignStrategy::RoundRobin => SchedulingStrategy::RoundRobin,
        ep_core::config::AssignStrategy::Single(_) => SchedulingStrategy::Single,
    }
}

/// 以当前设备列表构建调度器（least_memory 等四策略 + allow_overcommit，P1-1）
fn make_scheduler(
    devices: &[ComputeDevice],
    config: &AppConfig,
) -> ep_core::compute::scheduler::ComputeScheduler {
    let mut scheduler = ep_core::compute::scheduler::ComputeScheduler::new(
        devices.to_vec(),
        scheduling_strategy_for(config),
    );
    scheduler.set_allow_overcommit(config.compute.allow_overcommit);
    scheduler
}

/// 设备集合指纹（id 排序）：设备列表刷新后仅在**集合变化**时重建调度器，
/// 避免周期刷新（利用率/显存采样）把已记录的显存分配清零。
fn device_ids_key(devices: &[ComputeDevice]) -> BTreeSet<String> {
    devices.iter().map(|d| d.id.to_string()).collect()
}

/// 调度器 VRAM 请求量（MB）：激活变体级估算优先、模块级兜底（§6.3 同源口径，
/// `resolve_vram_estimate` 在变体未命中时自动回退模块级），未知 → 0（不参与显存闸门）。
fn scheduler_vram_mb(config: &AppConfig, manifest: &ModuleManifest) -> u32 {
    let variant = ep_core::model::active_model_for(config, manifest).unwrap_or("");
    let mb = manifest.resolve_vram_estimate(variant).unwrap_or(0);
    u32::try_from(mb).unwrap_or(u32::MAX)
}

/// 为模块分配设备（P1-1：manifest backends 过滤 + least_memory + allow_overcommit）。
///
/// 加速后端优先：先以 manifest 声明的**非 CPU** 后端请求调度器（多 GPU 时按
/// 剩余显存最大者落位）；调度器拒绝（无兼容设备 / Manual / 显存超限且
/// 未开超分）时，manifest 声明了 CPU → CPU 保底，否则 None。
fn assign_module_device(
    scheduler: &ep_core::compute::scheduler::ComputeScheduler,
    manifest: &ModuleManifest,
    config: &AppConfig,
) -> Option<DeviceId> {
    let accel: Vec<ComputeBackend> = manifest
        .compute
        .backends
        .iter()
        .copied()
        .filter(|b| *b != ComputeBackend::Cpu)
        .collect();
    let assigned = if accel.is_empty() {
        // 纯 CPU 模块：直接走 CPU 后端分配（CPU 设备 total_memory_mb=None → 不受显存闸门约束）
        scheduler.assign(&manifest.module.id, &[ComputeBackend::Cpu], 0)
    } else {
        scheduler.assign(
            &manifest.module.id,
            &accel,
            scheduler_vram_mb(config, manifest),
        )
    };
    assigned.or_else(|| {
        manifest
            .compute
            .backends
            .contains(&ComputeBackend::Cpu)
            .then_some(DeviceId::Cpu)
    })
}

/// 任务工作区根目录（`[pipeline].workspace_dir`，相对路径基于应用根）
fn workspace_dir(root: &Path, config: &AppConfig) -> PathBuf {
    let p = Path::new(&config.pipeline.workspace_dir);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    }
}

/// 任务 id 生成（纳秒时间戳 hex + 进程内序号，不引入 uuid 依赖）
fn unique_task_id() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("task-{nanos:x}-{seq:04x}")
}

/// 注册表记录 → GUI 任务摘要（P1-6：TasksRefreshed 的数据映射）
fn record_to_summary(r: &TaskRecord) -> ep_core::pipeline::runner::TaskSummary {
    ep_core::pipeline::runner::TaskSummary {
        id: r.id.clone(),
        pipeline_name: r.pipeline_id.clone(),
        status: match r.status {
            TaskState::Completed => TaskStatus::Completed,
            TaskState::Running => TaskStatus::Running,
            // queued 在桌面端无全局闸门，映射为 Pending 展示
            TaskState::Queued => TaskStatus::Pending,
            TaskState::Failed => TaskStatus::Failed(r.error.clone().unwrap_or_default()),
            TaskState::Cancelled => TaskStatus::Cancelled,
        },
        started_at: Some(r.started_at.to_rfc3339()),
        finished_at: r.finished_at.map(|t| t.to_rfc3339()),
        node_count: r.node_order.len(),
        completed_nodes: r
            .nodes
            .values()
            .filter(|n| n.state == "completed")
            .count(),
        // 门禁 #40：产物透传（按节点拓扑序稳定排序）
        artifacts: r
            .node_order
            .iter()
            .filter_map(|nid| r.artifacts.get(nid).map(|p| (nid.clone(), p.clone())))
            .collect(),
    }
}

/// 编译直跑退化 DAG：`input(file_input) → run(module) → output(file_output)`
///（§5.3；形状与 daemon `execution::build_direct_pipeline` 一致，
/// 直跑任务 pipeline_id 采用 `direct/<module_id>` 供任务列表过滤）。
///
/// 输出节点不带 `path` 参数 → 引擎派生 `{work_dir}/output_output.out`，
/// 随产物归集进入任务目录。
fn build_direct_pipeline(
    module_id: &str,
    capability: &str,
    params: serde_json::Value,
    input_path: &Path,
) -> Pipeline {
    let make_node = |id: &str, kind: NodeKind, label: &str, params: serde_json::Value| {
        PipelineNode {
            id: id.to_string(),
            kind,
            label: label.to_string(),
            params,
            position: None,
            timeout_secs: None,
            retry_count: None,
        }
    };
    Pipeline {
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
                serde_json::json!({ "path": input_path.display().to_string() }),
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
                serde_json::json!({}),
            ),
        ],
        edges: vec![
            Edge {
                from: ("input".to_string(), "output".to_string()),
                to: ("run".to_string(), "input".to_string()),
            },
            Edge {
                from: ("run".to_string(), "output".to_string()),
                to: ("output".to_string(), "input".to_string()),
            },
        ],
        max_instances: None,
    }
}

/// 直跑参数类型化（AppCmd::ExecuteSingle 的 `Vec<(String, String)>` 原始表单值
/// → 引擎参数对象）：按模块 manifest `CapabilityDecl.params` schema 强制类型化
/// （string / integer / float|number / boolean；未知类型按字符串透传），
/// 注入缺失参数的默认值并校验必填与枚举（语义对齐 daemon
/// `execute.rs::validate_and_fill_params`）。
fn typed_exec_params(
    schema: Option<&HashMap<String, ParamSchema>>,
    raw: &[(String, String)],
) -> Result<serde_json::Value, String> {
    let mut params = serde_json::Map::new();
    for (name, value) in raw {
        let declared = schema.and_then(|s| s.get(name));
        let typed = match declared.map(|d| d.param_type.as_str()) {
            Some("integer") => {
                let n: i64 = value.trim().parse().map_err(|_| {
                    format!("parameter '{name}' expects an integer, got '{value}'")
                })?;
                serde_json::Value::from(n)
            }
            Some("float") | Some("number") => {
                let n: f64 = value.trim().parse().map_err(|_| {
                    format!("parameter '{name}' expects a number, got '{value}'")
                })?;
                serde_json::Value::from(n)
            }
            Some("boolean") => {
                let b = match value.trim().to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" => true,
                    "false" | "0" | "no" => false,
                    other => {
                        return Err(format!(
                            "parameter '{name}' expects a boolean, got '{other}'"
                        ))
                    }
                };
                serde_json::Value::from(b)
            }
            // string / 未知类型（模块自定义）→ 原样字符串透传（宽容，同 daemon）
            _ => serde_json::Value::String(value.clone()),
        };
        params.insert(name.clone(), typed);
    }
    // schema 侧：缺省注入默认值 / 必填校验 / 枚举校验
    if let Some(schema) = schema {
        for (name, decl) in schema {
            if !params.contains_key(name) {
                if let Some(default) = &decl.default {
                    params.insert(name.clone(), default.clone());
                } else {
                    return Err(format!("missing required parameter '{name}'"));
                }
            }
            if let Some(enum_values) = &decl.enum_values {
                let ok = params
                    .get(name)
                    .and_then(|v| v.as_str())
                    .map(|s| enum_values.iter().any(|e| e == s))
                    .unwrap_or(false);
                if !ok {
                    return Err(format!(
                        "parameter '{name}' value is not one of the allowed enum values"
                    ));
                }
            }
        }
    }
    Ok(serde_json::Value::Object(params))
}

/// 模块日志增量提取（P1-7）：`prev` 为上一轮缓冲区快照，`current` 为本轮
/// 环形缓冲区内容（仅尾部追加、头部可能因 500 行上限弹出）。
/// 返回本轮新增的行（`current` 的尾部后缀）。
///
/// 重叠段 = prev 的最长存活后缀 == current 的最长前缀；从最大可能重叠向下
/// 搜索第一个匹配位置，其后的 current 内容即新增行。
fn new_log_lines(prev: &[String], current: &[String]) -> Vec<String> {
    let mut overlap = prev.len().min(current.len());
    while overlap > 0 && prev[prev.len() - overlap..] != current[..overlap] {
        overlap -= 1;
    }
    current[overlap..].to_vec()
}

/// 整合包注册表条目 → UI 视图（tags 取包内模型条目 tags 的并集，去重保序）
fn pack_entry_from(pack: ep_pack::import::InstalledPack) -> ep_desktop::app::PackEntry {
    let mut tags: Vec<String> = Vec::new();
    for model in &pack.models {
        for tag in &model.tags {
            if !tags.contains(tag) {
                tags.push(tag.clone());
            }
        }
    }
    ep_desktop::app::PackEntry {
        id: pack.id.clone(),
        version: pack.version,
        name: pack.name.unwrap_or(pack.id),
        description: pack.description.unwrap_or_default(),
        tags,
        installed_at: Some(pack.installed_at),
    }
}

/// 整合包导入的模块解析回调（B1 resolve 契约，桌面侧实现；
/// 语义与 daemon `api/packs.rs::resolve_entry` 一致）：
/// 按 qualified_id 规范形 + variant 在已发现模块清单中解析。
fn desktop_resolve_entry(
    manifests: &[ModuleManifest],
    entry: &ep_pack::manifest::PackModelEntry,
) -> Result<ep_pack::import::ResolvedModel, String> {
    for mf in manifests {
        for decl in &mf.models {
            let Some(q) = decl.qualified_id.as_deref() else {
                continue;
            };
            let Ok(parsed) = ep_core::model_id::QualifiedId::parse(q) else {
                continue;
            };
            if parsed.to_canonical() != entry.qualified_id || decl.id != entry.variant {
                continue;
            }
            let download = if entry.mode == ep_pack::manifest::ModelMode::Reference {
                Some(pack_reference_descriptor(mf, decl)?)
            } else {
                None
            };
            return Ok(ep_pack::import::ResolvedModel {
                module_id: mf.module.id.clone(),
                model_id: decl.id.clone(),
                target_dir: decl.target_dir.clone(),
                backends: mf.compute.backends.clone(),
                download,
            });
        }
    }
    Err(format!(
        "no installed module provides model {}@{}",
        entry.qualified_id, entry.variant
    ))
}

/// reference 下载描述符解析（缺 repo_id/url → Err → 适配判 Unsupported）
fn pack_reference_descriptor(
    mf: &ModuleManifest,
    decl: &ModelDecl,
) -> Result<ep_pack::import::PendingDownload, String> {
    let location = match decl.source {
        ModelSource::Huggingface | ModelSource::Modelscope => decl
            .repo_id
            .clone()
            .ok_or_else(|| {
                format!(
                    "module '{}' model '{}' declares {} source without repo_id",
                    mf.module.id,
                    decl.id,
                    decl.source
                )
            })?,
        ModelSource::Url => decl.url.clone().ok_or_else(|| {
            format!(
                "module '{}' model '{}' declares url source without url",
                mf.module.id, decl.id
            )
        })?,
    };
    Ok(ep_pack::import::PendingDownload {
        source: decl.source.as_str().to_string(),
        location,
        revision: decl.revision.clone(),
    })
}

// ─── 协调记录 #47：导出模块（组装暂存 → ep_pack::build::build_pack） ────────

/// 后台消息 i18n 兜底：键未落盘时返回兜底文案而非键本身（与 pages::trfb 同策略；
/// ep-core 的缺失键路径返回键本身、不做插值，故兜底分支自行插值）。
fn trfb_bg(lang: &str, key: &str, fallback: &str, params: &[(&str, &str)]) -> String {
    let translated = ep_core::i18n::t(lang, key, params);
    if translated != key {
        return translated;
    }
    let mut out = fallback.to_string();
    for (name, value) in params {
        let pattern = format!("{{{{{name}}}}}");
        out = out.replace(&pattern, value);
    }
    out
}

/// 唯一暂存 id（纳秒时间戳 + 进程内序号 + pid；与 daemon unique_id 同款）
fn unique_pack_id() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{seq:04x}-{}", std::process::id())
}

/// 导出模块（桌面端独立实现库层调用，组装逻辑参照 daemon packs.rs build 路径）：
/// 圈选校验 → 暂存目录组装（bundle 权重**硬链接优先**）→ [`ep_pack::build::build_pack`]
/// → 用户选定目录 `.epzip`。返回 (归档路径, 文件条目数)。
fn build_export_pack(
    root: &Path,
    staging_cfg: &str,
    models_cfg: &ep_core::config::ModelsConfig,
    manifests: &[ModuleManifest],
    spec: ep_desktop::app::PackExportSpec,
) -> Result<(PathBuf, usize), String> {
    use ep_pack::manifest::{ModelMode, PackManifest, PackModelEntry, PackPipelineRef};

    let mgr = ModelManager::new(models_cfg, root).with_manifests(manifests.to_vec());

    // ── 模型圈选 + bundle 权重目录 + 后端并集 ──
    let mut entries: Vec<PackModelEntry> = Vec::new();
    let mut bundle_dirs: Vec<(String, PathBuf)> = Vec::new();
    let mut backends: Vec<ComputeBackend> = Vec::new();
    for m in &spec.modules {
        let mf = manifests
            .iter()
            .find(|mf| mf.module.id == m.module_id)
            .ok_or_else(|| format!("module '{}' not found", m.module_id))?;
        for variant in &m.variants {
            let decl = mf
                .models
                .iter()
                .find(|d| d.id == *variant)
                .ok_or_else(|| {
                    format!("module '{}' has no variant '{}'", m.module_id, variant)
                })?;
            let meta = mgr.read_meta(&decl.target_dir);
            // qualified_id：manifest 声明优先，meta 兜底（均缺失 → 身份缺失不可入包，§4.3）
            let qualified = decl
                .qualified_id
                .clone()
                .or_else(|| meta.as_ref().and_then(|mm| mm.qualified_id.clone()))
                .ok_or_else(|| {
                    format!(
                        "model {}@{} lacks qualified_id; cannot export",
                        m.module_id, variant
                    )
                })?;
            let qid = ep_core::model_id::QualifiedId::parse(&qualified)
                .map_err(|e| format!("invalid qualified_id '{qualified}': {e}"))?
                .to_canonical();
            let tags = meta.map(|mm| mm.tags).unwrap_or_default();
            if m.bundle {
                let src = mgr.model_dir(&decl.target_dir);
                if !src.is_dir() {
                    return Err(format!(
                        "bundle model {qid}@{variant}: weights dir {} missing",
                        src.display()
                    ));
                }
                bundle_dirs.push((decl.target_dir.clone(), src));
            }
            for b in &mf.compute.backends {
                if !backends.contains(b) {
                    backends.push(*b);
                }
            }
            entries.push(PackModelEntry {
                qualified_id: qid,
                variant: variant.clone(),
                mode: if m.bundle {
                    ModelMode::Bundle
                } else {
                    ModelMode::Reference
                },
                tags,
            });
        }
    }
    if backends.is_empty() {
        backends.push(ComputeBackend::Cpu);
    }

    // ── 管线圈选：按 id 查找 config/pipelines/*.toml ──
    let pipelines_dir = root.join("config").join("pipelines");
    let specs = scan_pipeline_specs_desktop(&pipelines_dir);
    let mut pipeline_files: Vec<(PathBuf, String)> = Vec::new();
    let mut pipeline_refs: Vec<PackPipelineRef> = Vec::new();
    let mut missing_pipelines: Vec<String> = Vec::new();
    for pid in &spec.pipelines {
        match specs.iter().find(|(_, id)| id == pid) {
            Some((path, _)) => {
                pipeline_files.push((path.clone(), format!("{pid}.toml")));
                pipeline_refs.push(PackPipelineRef {
                    file: format!("pipelines/{pid}.toml"),
                });
            }
            None => missing_pipelines.push(pid.clone()),
        }
    }
    if !missing_pipelines.is_empty() {
        return Err(format!(
            "pipeline(s) not found: {}",
            missing_pipelines.join(", ")
        ));
    }

    // ── 包身份：对话框显式字段优先，缺省自动生成 ──
    let id = if spec.id.trim().is_empty() {
        format!("local.build-{}", Utc::now().format("%Y%m%d-%H%M%S"))
    } else {
        spec.id.trim().to_string()
    };
    let version = if spec.version.trim().is_empty() {
        "0.1.0".to_string()
    } else {
        spec.version.trim().to_string()
    };
    let name = if spec.name.trim().is_empty() {
        id.clone()
    } else {
        spec.name.trim().to_string()
    };

    let manifest = PackManifest {
        pack: ep_pack::manifest::PackInfo {
            id: id.clone(),
            version: version.clone(),
            name,
            description: String::new(),
            authors: vec![],
            license: None,
            homepage: None,
            min_ep_version: None,
            tags: vec![],
        },
        compute: ep_pack::manifest::PackCompute {
            backends,
            notes: HashMap::new(),
        },
        models: entries,
        pipelines: pipeline_refs,
    };
    if let Err(errors) = manifest.validate() {
        return Err(errors.join("; "));
    }

    // ── 组装暂存目录 ──
    let staging = {
        let p = Path::new(staging_cfg);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        }
    };
    let source_dir = staging.join(format!("build-{}-{}", id, unique_pack_id()));
    let assemble = assemble_export_tree(&source_dir, &manifest, &bundle_dirs, &pipeline_files);
    if let Err(e) = assemble {
        let _ = std::fs::remove_dir_all(&source_dir);
        return Err(e);
    }

    // ── 打包（CHECKSUMS.toml 由 build_pack 生成并写入归档） ──
    std::fs::create_dir_all(&spec.output_dir)
        .map_err(|e| format!("create output dir failed: {e}"))?;
    let output = spec
        .output_dir
        .join(format!("{}-{}.epzip", manifest.pack.id, manifest.pack.version));
    let plan = ep_pack::build::BuildPlan::new(&source_dir, &output);
    let result = ep_pack::build::build_pack(&plan);
    let _ = std::fs::remove_dir_all(&source_dir); // 无论成败清理暂存
    match result {
        Ok(summary) => Ok((summary.archive_path, summary.file_count)),
        Err(e) => Err(format!("pack build failed: {e}")),
    }
}

/// 组装包内容目录：ep-pack.toml + models/<target_dir>/（bundle，硬链接优先）
/// + pipelines/<id>.toml。与 daemon `assemble_and_build` 同布局。
fn assemble_export_tree(
    source_dir: &Path,
    manifest: &ep_pack::manifest::PackManifest,
    bundle_dirs: &[(String, PathBuf)],
    pipeline_files: &[(PathBuf, String)],
) -> Result<(), String> {
    std::fs::create_dir_all(source_dir).map_err(|e| format!("create staging failed: {e}"))?;

    // 1) 清单（ep-pack.toml）：桌面端无 toml 序列化依赖，用内置最小 TOML 输出器
    let manifest_toml = render_pack_manifest(manifest);
    std::fs::write(
        source_dir.join(ep_pack::extract::MANIFEST_FILE_NAME),
        manifest_toml,
    )
    .map_err(|e| format!("write manifest failed: {e}"))?;

    // 2) bundle 权重：models/<target_dir>/（硬链接优先，跨卷/不支持时回退复制）
    for (target_dir, src) in bundle_dirs {
        let dest = source_dir.join("models").join(target_dir);
        std::fs::create_dir_all(&dest).map_err(|e| format!("create model dir failed: {e}"))?;
        copy_dir_hardlink_preferred(src, &dest)
            .map_err(|e| format!("stage weights for '{target_dir}' failed: {e}"))?;
    }

    // 3) 管线文件：pipelines/<id>.toml
    for (src, file_name) in pipeline_files {
        let dest = source_dir.join("pipelines").join(file_name);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create pipelines dir failed: {e}"))?;
        }
        std::fs::copy(src, &dest).map_err(|e| format!("copy pipeline failed: {e}"))?;
    }
    Ok(())
}

/// 递归目录落盘：**硬链接优先**（同卷近零成本），失败回退普通复制；
/// 跳过符号链接等非普通文件类型（与 daemon copy_dir_contents 同纪律）。
fn copy_dir_hardlink_preferred(src: &Path, dst: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            std::fs::create_dir_all(&to)?;
            copy_dir_hardlink_preferred(&from, &to)?;
        } else if ft.is_file() && std::fs::hard_link(&from, &to).is_err() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// 扫描目录下管线文件 → (路径, pipeline.id)；损坏文件跳过
fn scan_pipeline_specs_desktop(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
        .collect();
    paths.sort();
    for path in paths {
        match Pipeline::from_toml(&path) {
            Ok(pipeline) => out.push((path, pipeline.id)),
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "pipeline file corrupted, skipping");
            }
        }
    }
    out
}

// ─── 最小 TOML 输出器（ep-pack.toml 渲染；形状与 daemon render_pack_manifest 一致） ──

/// TOML basic string 转义（覆盖清单字段可能出现的引号/反斜杠/控制字符）
fn toml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

fn toml_str(s: &str) -> String {
    format!("\"{}\"", toml_escape(s))
}

fn toml_str_array(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| toml_str(s)).collect();
    format!("[{}]", inner.join(", "))
}

/// 渲染 [`ep_pack::manifest::PackManifest`] 为 TOML 文本
///（`PackManifest::from_file` 可原样读回，见单测 roundtrip）。
fn render_pack_manifest(m: &ep_pack::manifest::PackManifest) -> String {
    let mut out = String::new();
    out.push_str("[pack]\n");
    out.push_str(&format!("id = {}\n", toml_str(&m.pack.id)));
    out.push_str(&format!("version = {}\n", toml_str(&m.pack.version)));
    out.push_str(&format!("name = {}\n", toml_str(&m.pack.name)));
    out.push_str(&format!("description = {}\n", toml_str(&m.pack.description)));
    if !m.pack.authors.is_empty() {
        out.push_str(&format!("authors = {}\n", toml_str_array(&m.pack.authors)));
    }
    if let Some(license) = &m.pack.license {
        out.push_str(&format!("license = {}\n", toml_str(license)));
    }
    if let Some(homepage) = &m.pack.homepage {
        out.push_str(&format!("homepage = {}\n", toml_str(homepage)));
    }
    if let Some(min) = &m.pack.min_ep_version {
        out.push_str(&format!("min_ep_version = {}\n", toml_str(min)));
    }
    if !m.pack.tags.is_empty() {
        out.push_str(&format!("tags = {}\n", toml_str_array(&m.pack.tags)));
    }

    out.push_str("\n[compute]\n");
    let backends: Vec<String> = m.compute.backends.iter().map(|b| b.to_string()).collect();
    out.push_str(&format!("backends = {}\n", toml_str_array(&backends)));
    if !m.compute.notes.is_empty() {
        out.push_str("\n[compute.notes]\n");
        let mut notes: Vec<(String, String)> = m
            .compute
            .notes
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        notes.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in notes {
            out.push_str(&format!("{k} = {}\n", toml_str(&v)));
        }
    }

    for model in &m.models {
        out.push_str("\n[[models]]\n");
        out.push_str(&format!("qualified_id = {}\n", toml_str(&model.qualified_id)));
        out.push_str(&format!("variant = {}\n", toml_str(&model.variant)));
        out.push_str(&format!("mode = {}\n", toml_str(model.mode.as_str())));
        if !model.tags.is_empty() {
            out.push_str(&format!("tags = {}\n", toml_str_array(&model.tags)));
        }
    }

    for pipeline in &m.pipelines {
        out.push_str("\n[[pipelines]]\n");
        out.push_str(&format!("file = {}\n", toml_str(&pipeline.file)));
    }
    out
}

/// venv 准备（P0-5 公共件）：Python 运行时且 venv 缺失 → EnvManager::ensure_venv
///（阻塞操作放入 spawn_blocking）。返回就绪的 venv python 路径。
async fn prepare_module_venv(
    root: &Path,
    config: &AppConfig,
    module_id: &str,
    manifest: &ModuleManifest,
) -> anyhow::Result<PathBuf> {
    let existing = ep_core::process::venv_python_path(root, module_id);
    if existing.exists() {
        return Ok(existing);
    }
    let python_cfg = config.python.clone();
    let network_cfg = config.network.clone();
    let root2 = root.to_path_buf();
    let mid = module_id.to_string();
    let py_ver = manifest.runtime.python_version.clone().unwrap_or_default();
    let req_rel = manifest
        .runtime
        .requirements
        .clone()
        .unwrap_or_else(|| "requirements.txt".to_string());
    let prep = tokio::task::spawn_blocking(move || {
        let env_mgr =
            ep_core::env::EnvManager::new(&root2, &python_cfg).with_network(&network_cfg);
        let req_path = root2.join("modules").join(&mid).join(req_rel);
        env_mgr.ensure_venv(&mid, &py_ver, &req_path)
    })
    .await;
    match prep {
        Ok(Ok(path)) => Ok(path),
        Ok(Err(e)) => Err(anyhow::anyhow!("{e:#}")),
        Err(e) => Err(anyhow::anyhow!("venv prep task panicked: {e}")),
    }
}

// ─── Wave 3 C4：管线执行驱动（决策 2 — background_loop 直连 ep-core runner） ──

/// 注册表内部节点状态更新（记录缺失时 no-op）
fn set_task_node_state(
    registry: &Mutex<TaskRegistry>,
    task_id: &str,
    node_id: &str,
    state: &str,
    error: Option<String>,
) {
    let mut reg = registry.lock().unwrap_or_else(|e| e.into_inner());
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

/// 记录节点产物路径（引擎回调产出）
fn record_task_artifact(registry: &Mutex<TaskRegistry>, task_id: &str, node_id: &str, path: &Path) {
    let mut reg = registry.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(record) = reg.get_mut(task_id) {
        record
            .artifacts
            .insert(node_id.to_string(), path.to_path_buf());
    }
}

/// 推送全量任务快照到 UI（任务页进度实时性）
fn send_task_snapshot(registry: &Mutex<TaskRegistry>, tx: &std::sync::mpsc::Sender<ep_desktop::app::AppMsg>) {
    let summaries: Vec<_> = {
        let reg = registry.lock().unwrap_or_else(|e| e.into_inner());
        reg.all().iter().map(record_to_summary).collect()
    };
    let _ = tx.send(ep_desktop::app::AppMsg::TasksRefreshed(summaries));
}

/// 在独立 `PipelineRunnerImpl` 上同步执行管线（spawn_blocking 线程内；
/// 模式对齐 daemon `execution::run_task`）：注册模块端口 + 协作取消标志 +
/// 默认节点 wall-clock 超时，节点回调实时更新任务注册表。
#[allow(clippy::too_many_arguments)]
fn run_pipeline_task(
    task_id: String,
    task_dir: PathBuf,
    pipeline: Pipeline,
    module_ports: HashMap<String, u16>,
    cancel_flag: Arc<AtomicBool>,
    default_node_timeout: Option<Duration>,
    registry: Arc<Mutex<TaskRegistry>>,
    tx: std::sync::mpsc::Sender<ep_desktop::app::AppMsg>,
) -> (anyhow::Result<()>, Option<TaskDetail>) {
    let mut runner = PipelineRunnerImpl::new(task_dir.clone());
    runner.set_module_ports(module_ports);
    // 取消（P0-6 语义）：执行层与运行器共享同一 AtomicBool，节点边界检查
    runner.set_cancel_flag(cancel_flag);
    // 节点级 wall-clock 超时缺省值（节点自身 timeout_secs 优先）
    runner.set_default_node_timeout(default_node_timeout);

    // 回调：节点开始 → running
    {
        let registry = Arc::clone(&registry);
        let tx = tx.clone();
        let tid = task_id.clone();
        runner.on_node_start = Some(Arc::new(move |node_id| {
            set_task_node_state(&registry, &tid, node_id, "running", None);
            send_task_snapshot(&registry, &tx);
        }));
    }
    // 回调：节点完成 → completed + 记录产物
    {
        let registry = Arc::clone(&registry);
        let tx = tx.clone();
        let tid = task_id.clone();
        runner.on_node_complete = Some(Arc::new(move |node_id, artifact| {
            set_task_node_state(&registry, &tid, node_id, "completed", None);
            if let Artifact::File(path) = artifact {
                record_task_artifact(&registry, &tid, node_id, path);
            }
            send_task_snapshot(&registry, &tx);
        }));
    }
    // 回调：节点失败 → failed
    {
        let registry = Arc::clone(&registry);
        let tx = tx.clone();
        let tid = task_id.clone();
        runner.on_node_error = Some(Arc::new(move |node_id, error| {
            set_task_node_state(&registry, &tid, node_id, "failed", Some(error.to_string()));
            send_task_snapshot(&registry, &tx);
        }));
    }

    // 引擎同步执行（blocking 线程无 tokio Handle → execute 自建运行时 block_on）
    let result = PipelineRunner::execute(&mut runner, &pipeline, &task_dir);

    // 引擎自身任务详情（权威节点终态，含回调不覆盖的 skipped 节点）
    let detail = runner
        .list_tasks()
        .pop()
        .and_then(|summary| runner.get_task_detail(&summary.id));
    (result, detail)
}

/// 任务终结收尾：写终态（已被其他路径终结则不覆盖——取消先行时引擎收尾让位）+
/// 产物归集到 `workspace/tasks/<task_id>/files/<node_id>/`（硬链接，跨文件系统
/// 退化为复制；对齐 daemon finalize 归集逻辑）。
async fn finalize_task_record(
    registry: Arc<Mutex<TaskRegistry>>,
    tx: std::sync::mpsc::Sender<ep_desktop::app::AppMsg>,
    task_id: &str,
    engine_error: Option<String>,
    detail: Option<&TaskDetail>,
    cancelled: bool,
) {
    let terminal = if cancelled {
        TaskState::Cancelled
    } else if engine_error.is_none() {
        TaskState::Completed
    } else {
        TaskState::Failed
    };
    {
        let mut reg = registry.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(Err(e)) = reg.update(task_id, |record| {
            if record.status.is_terminal() {
                return; // 已被其他路径终结（如用户取消），不覆盖
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
            record.error = if terminal == TaskState::Failed {
                engine_error.clone()
            } else {
                None
            };
            record.finished_at = Some(Utc::now());
            record.queue_position = None;

            // 产物归集（引擎自然收尾路径；取消任务不归集）
            if !cancelled {
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
                        tracing::warn!(
                            task_id = %task_id,
                            node_id = %node_id.as_str(),
                            "artifact collection failed"
                        );
                    }
                }
            }
        }) {
            tracing::warn!(task_id = %task_id, error = %e, "failed to persist terminal task state");
        }
    }
    send_task_snapshot(&registry, &tx);
}

/// 提交管线执行（管线执行 / 直跑共用入口，决策 2）：
/// DAG 校验 → 模块自动拉起（含 venv 准备 P0-5 / 调度器设备选择 P1-1）→
/// 任务注册表记录（持久化 runtime/tasks）→ spawn_blocking 引擎执行。
/// 成功返回 task_id；失败已发 AppMsg::Error 并返回 None。
#[allow(clippy::too_many_arguments)]
async fn submit_pipeline_task(
    tx: &std::sync::mpsc::Sender<ep_desktop::app::AppMsg>,
    lang: &'static str,
    root: &Path,
    config: &AppConfig,
    discovered: &[DiscoveredModule],
    model_manager: &ModelManager,
    process_manager: &mut ProcessManager,
    port_manager: &mut PortManager,
    scheduler: &mut ep_core::compute::scheduler::ComputeScheduler,
    task_registry: &Arc<Mutex<TaskRegistry>>,
    task_cancel_flags: &mut HashMap<String, Arc<AtomicBool>>,
    pipeline: Pipeline,
) -> Option<String> {
    use ep_desktop::app::AppMsg;
    use ep_desktop::i18n::tr;

    // 1. DAG 校验（直跑退化 DAG 恒通过；用户管线防环/缺输入）
    if let Err(errors) = pipeline.validate() {
        let detail = errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        let _ = tx.send(AppMsg::Error(tr(
            lang,
            "desktopApp.error.pipelineInvalid",
            &[("detail", &detail)],
        )));
        return None;
    }

    // 2. 引用模块自动拉起（§6.5：未运行 → 启动并等健康，超时计入错误）
    let mut module_ids: Vec<&str> = Vec::new();
    for node in &pipeline.nodes {
        if let NodeKind::Module { module_id, .. } = &node.kind {
            if !module_ids.contains(&module_id.as_str()) {
                module_ids.push(module_id.as_str());
            }
        }
    }
    for module_id in module_ids {
        if let Err(detail) = ensure_module_ready(
            root,
            config,
            discovered,
            model_manager,
            process_manager,
            port_manager,
            scheduler,
            module_id,
        )
        .await
        {
            let _ = tx.send(AppMsg::Error(tr(
                lang,
                "desktopApp.error.moduleAutoStartFailed",
                &[("id", module_id), ("detail", &detail)],
            )));
            return None;
        }
    }

    // 3. 任务工作目录（workspace/tasks/<task_id>，产物归集落点）
    let task_id = unique_task_id();
    let task_dir = workspace_dir(root, config)
        .join("tasks")
        .join(&task_id);
    if let Err(e) = std::fs::create_dir_all(&task_dir) {
        let _ = tx.send(AppMsg::Error(tr(
            lang,
            "desktopApp.error.taskSubmitFailed",
            &[("detail", &e.to_string())],
        )));
        return None;
    }

    // 4. 任务注册表记录（持久化 runtime/tasks，daemon/桌面共用索引）
    let now = Utc::now();
    let node_order: Vec<String> = pipeline.nodes.iter().map(|n| n.id.clone()).collect();
    let nodes: HashMap<String, NodeRecord> = node_order
        .iter()
        .map(|id| {
            (
                id.clone(),
                NodeRecord {
                    state: "pending".to_string(),
                    error: None,
                },
            )
        })
        .collect();
    let record = TaskRecord {
        id: task_id.clone(),
        pipeline_id: pipeline.id.clone(),
        status: TaskState::Running,
        error: None,
        queue_position: None,
        started_at: now,
        started_running_at: Some(now),
        finished_at: None,
        node_order,
        nodes,
        artifacts: HashMap::new(),
        served_artifacts: HashMap::new(),
        work_dir: task_dir.clone(),
    };
    {
        let mut reg = task_registry.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = reg.insert(record) {
            let _ = tx.send(AppMsg::Error(tr(
                lang,
                "desktopApp.error.taskSubmitFailed",
                &[("detail", &e.to_string())],
            )));
            return None;
        }
        // 顺带清理已终结任务的取消标志（防长跑会话累积）
        let terminal_ids: Vec<String> = reg
            .all()
            .iter()
            .filter(|r| r.status.is_terminal())
            .map(|r| r.id.clone())
            .collect();
        for id in terminal_ids {
            task_cancel_flags.remove(&id);
        }
    }

    // 5. 取消标志（与 CancelTask 命令共享同一 AtomicBool）
    let cancel_flag = Arc::new(AtomicBool::new(false));
    task_cancel_flags.insert(task_id.clone(), cancel_flag.clone());

    // 6. 模块端口注册（模块节点经 http://127.0.0.1:{port}/predict/{capability} 调用）
    let module_ports: HashMap<String, u16> = process_manager
        .list_running()
        .iter()
        .filter_map(|inst| inst.port.map(|port| (inst.module_id.clone(), port)))
        .collect();

    // 7. spawn 引擎执行（blocking 线程）+ 异步收尾
    let default_node_timeout = (config.pipeline.default_timeout_secs > 0)
        .then(|| Duration::from_secs(config.pipeline.default_timeout_secs as u64));
    let registry_bg = Arc::clone(task_registry);
    let registry_fin = Arc::clone(task_registry);
    let tx_bg = tx.clone();
    let tx_fin = tx.clone();
    let task_id_bg = task_id.clone();
    let task_id_fin = task_id.clone();
    let cancel_fin = cancel_flag.clone();
    tokio::spawn(async move {
        let joined = tokio::task::spawn_blocking(move || {
            run_pipeline_task(
                task_id_bg,
                task_dir,
                pipeline,
                module_ports,
                cancel_flag,
                default_node_timeout,
                registry_bg,
                tx_bg,
            )
        })
        .await;
        let (result, detail) = match joined {
            Ok(pair) => pair,
            Err(e) => (
                Err(anyhow::anyhow!("execution thread exited abnormally: {e}")),
                None,
            ),
        };
        let cancelled = cancel_fin.load(Ordering::SeqCst);
        finalize_task_record(
            registry_fin,
            tx_fin,
            &task_id_fin,
            result.err().map(|e| e.to_string()),
            detail.as_ref(),
            cancelled,
        )
        .await;
    });

    Some(task_id)
}

/// 确保模块运行（健康）并返回其服务端口 —— 桌面侧自动拉起
///（daemon `api/autostart.rs` 同款语义）：
/// 已 Running 直通；Starting 仅等健康；其余走完整启动路径
///（模型就绪检查 → venv 准备 P0-5 → 端口 → 调度器设备 P1-1 → 进程）后等健康。
/// 失败时清理已拉起的进程与端口，返回英文技术细节（调用方本地化呈现）。
#[allow(clippy::too_many_arguments)]
async fn ensure_module_ready(
    root: &Path,
    config: &AppConfig,
    discovered: &[DiscoveredModule],
    model_manager: &ModelManager,
    process_manager: &mut ProcessManager,
    port_manager: &mut PortManager,
    scheduler: &mut ep_core::compute::scheduler::ComputeScheduler,
    module_id: &str,
) -> Result<u16, String> {
    let manifest = discovered
        .iter()
        .find(|m| {
            m.manifest
                .as_ref()
                .map(|mf| mf.module.id == module_id)
                .unwrap_or(false)
        })
        .and_then(|m| m.manifest.clone())
        .ok_or_else(|| format!("module '{module_id}' not found or manifest invalid"))?;

    // 状态分流
    let current = process_manager.get_status(module_id).cloned();
    if current == Some(ServiceStatus::Running) {
        return process_manager
            .get_instance(module_id)
            .and_then(|i| i.port)
            .ok_or_else(|| format!("module '{module_id}' is running but has no port"));
    }
    let needs_start = !matches!(current, Some(ServiceStatus::Starting));

    if needs_start {
        // 1. 模型前置检查（default/首个变体缺失 → 拒绝拉起）
        if !manifest.models.is_empty() {
            let statuses = model_manager.check_model_status(module_id, &manifest);
            if let Some(model) = manifest
                .models
                .iter()
                .find(|m| m.default)
                .or(manifest.models.first())
            {
                if matches!(statuses.get(&model.id), Some(ModelStatus::Missing)) {
                    return Err(format!(
                        "model '{}' of module '{module_id}' is not ready (not downloaded)",
                        model.name
                    ));
                }
            }
        }
        // 2. venv 准备前置（P0-5：无人值守场景不能假设 venv 已备好；仅 Python 运行时）
        if manifest.runtime.runtime_type == RuntimeType::Python {
            if let Err(e) = prepare_module_venv(root, config, module_id, &manifest).await {
                return Err(format!("venv preparation failed: {e}"));
            }
        }
        // 3. 端口
        let port = port_manager
            .allocate(module_id)
            .map_err(|e| format!("port allocation failed: {e}"))?;
        // 4. 设备（P1-1 调度器：manifest backends 过滤 + least_memory + allow_overcommit）
        let device = match assign_module_device(scheduler, &manifest, config) {
            Some(d) => d,
            None => {
                port_manager.release(module_id);
                return Err(format!(
                    "no compatible device for module '{module_id}' (backends: {:?})",
                    manifest.compute.backends
                ));
            }
        };
        // 5. 环境变量（A2 公共构建：EP_ 前缀 + CUDA 库 + compute.env 由 start_module 统一装配）
        let env_vars =
            ep_core::process::build_module_env(root, config, module_id, &manifest, &device);
        // 6. 启动
        if let Err(e) = process_manager
            .start_module(module_id, &manifest, device, port, env_vars)
            .await
        {
            port_manager.release(module_id);
            scheduler.release(module_id);
            let msg = e.to_string();
            if !msg.contains("already running") {
                return Err(format!("module start failed: {msg}"));
            }
            // 并发竞态（理论上单线程事件循环不会发生）：转等健康
        }
    }

    // 等健康（monitor_process 内含 /health 探测与进程存活检查）
    let timeout_secs = manifest.interface.ready_timeout_secs.unwrap_or(30) as u64;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if tokio::time::Instant::now() >= deadline {
            cleanup_failed_start(process_manager, port_manager, scheduler, module_id).await;
            return Err(format!(
                "module '{module_id}' did not become healthy within {timeout_secs}s"
            ));
        }
        let _ = process_manager.monitor_process(module_id).await;
        match process_manager.get_status(module_id).cloned() {
            Some(ServiceStatus::Running) => {
                return process_manager
                    .get_instance(module_id)
                    .and_then(|i| i.port)
                    .ok_or_else(|| format!("module '{module_id}' healthy but has no port"));
            }
            Some(ServiceStatus::Error(detail)) => {
                cleanup_failed_start(process_manager, port_manager, scheduler, module_id).await;
                return Err(detail);
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// 拉起失败后的清理：停止进程 + 释放端口 + 释放调度器分配（不留僵尸实例）
async fn cleanup_failed_start(
    process_manager: &mut ProcessManager,
    port_manager: &mut PortManager,
    scheduler: &mut ep_core::compute::scheduler::ComputeScheduler,
    module_id: &str,
) {
    if process_manager.get_instance(module_id).is_some() {
        let _ = process_manager.stop_module(module_id).await;
    }
    port_manager.release(module_id);
    scheduler.release(module_id);
}

/// 启动一次模型下载并挂接进度转发（DownloadModel 命令与整合包 reference
/// 模型下载共用）。立即返回；进度/终态经 AppMsg 推送。
#[allow(clippy::too_many_arguments)]
async fn start_model_download(
    model_manager: &ModelManager,
    download_handles: &Arc<Mutex<HashMap<String, DownloadHandle>>>,
    tx: &std::sync::mpsc::Sender<ep_desktop::app::AppMsg>,
    lang: &'static str,
    module_id: &str,
    decl: &ModelDecl,
    venv_python: &Path,
    config: &AppConfig,
    source: Option<ModelSource>,
) {
    use ep_core::model::DownloadState;
    use ep_desktop::app::AppMsg;
    use ep_desktop::i18n::tr;

    let model_id = decl.id.clone();
    match model_manager.execute_download_with_progress(
        module_id,
        decl,
        venv_python,
        config,
        source,
    ) {
        Ok(handle) => {
            let mut progress_rx = handle.subscribe_progress();
            // 保存句柄供取消（UI 发 CancelDownload）
            download_handles
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(model_id.clone(), handle);

            let tx2 = tx.clone();
            let mid = model_id.clone();
            let handles2 = Arc::clone(download_handles);
            tokio::spawn(async move {
                use tokio::sync::broadcast::error::RecvError;
                let mut success = false;
                loop {
                    match progress_rx.recv().await {
                        Ok(p) => {
                            let terminal =
                                !matches!(p.state, DownloadState::Downloading);
                            let _ = tx2.send(AppMsg::ModelDownloadProgress {
                                model_id: mid.clone(),
                                percent: p.percent,
                                bytes: p.bytes,
                                state: p.state.clone(),
                            });
                            if terminal {
                                match &p.state {
                                    DownloadState::Completed => {
                                        success = true;
                                    }
                                    DownloadState::Failed(msg) => {
                                        let _ = tx2.send(AppMsg::Error(tr(
                                            lang,
                                            "desktopApp.error.downloadFailed",
                                            &[("id", &mid), ("detail", msg)],
                                        )));
                                    }
                                    DownloadState::Cancelled => {
                                        let _ = tx2.send(AppMsg::Info(tr(
                                            lang,
                                            "desktopApp.error.downloadCancelled",
                                            &[("id", &mid)],
                                        )));
                                    }
                                    DownloadState::Downloading => {}
                                }
                                break;
                            }
                        }
                        // 接收滞后：跳过丢失的事件，继续等待后续进度
                        Err(RecvError::Lagged(_)) => continue,
                        // 通道已关闭：按异常结束处理
                        Err(RecvError::Closed) => break,
                    }
                }
                // 清理句柄并发出最终消息（UI 据此刷新列表）
                handles2
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&mid);
                let _ = tx2.send(AppMsg::ModelDownloadFinished(mid, success));
            });
        }
        Err(e) => {
            let _ = tx.send(AppMsg::Error(tr(
                lang,
                "desktopApp.error.startDownloadFailed",
                &[("detail", &e.to_string())],
            )));
            let _ = tx.send(AppMsg::ModelDownloadFinished(model_id, false));
        }
    }
}

/// Background event loop — owns ProcessManager, PortManager, runs on tokio runtime.
async fn background_loop(
    tx: std::sync::mpsc::Sender<ep_desktop::app::AppMsg>,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<ep_desktop::app::AppCmd>,
    config: ep_core::config::AppConfig,
    root: std::path::PathBuf,
) {
    use ep_desktop::app::{AppCmd, AppMsg};
    use ep_desktop::i18n::tr;

    // UI 文案语言：从启动时配置归一化（&'static str，可安全移入各异步任务）。
    // 注：设置页的语言切换即时作用于 UI 渲染；后台错误文案在下次启动后跟随新语言。
    let lang = ep_core::i18n::normalize_language(&config.general.language);

    let (port_range_start, port_range_end) = config.port_range();
    let mut port_manager = ep_core::port::PortManager::new(port_range_start, port_range_end);
    // A2（§3.1）：模块子进程注入共享 CUDA 库目录（Linux LD_LIBRARY_PATH 前置 /
    // Windows PATH 前置，平台分支在 process.rs 内部）；
    // C4（仲裁 #13 / P1-8 桌面侧）：补网络代理注入，模块子进程出口走
    // config.network 配置的代理变量（与 ModelManager/EnvManager 同口径）。
    let mut process_manager = ep_core::process::ProcessManager::new()
        .with_cuda_libs_dir(ep_core::process::resolve_cuda_libs_dir(
            &root,
            &config.compute.cuda_libs_dir,
        ))
        .with_network_env(config.network.env_vars());

    // Initial device detection
    let disabled = &config.compute.disabled_backends;
    let devices = ep_core::compute::detect_all_devices(disabled);
    let _ = tx.send(AppMsg::DevicesRefreshed(devices.clone()));

    // P1-1（桌面侧）：设备调度器接线 —— ComputeScheduler（四策略 +
    // allow_overcommit + manifest backends 过滤），替代旧"首个非 CPU"选择。
    let mut scheduler = make_scheduler(&devices, &config);
    let mut scheduler_devices_key = device_ids_key(&devices);

    // Initial module discovery（先于 ModelManager，便于注册 manifests）
    let modules_dir = root.join("modules");
    let mut discovered = ep_core::module::discover_modules(&modules_dir);
    let _ = tx.send(AppMsg::ModulesDiscovered(discovered.clone()));

    // ModelManager：注册模块 manifests（import 解析 target_dir 依赖）+ 网络代理（更新检查依赖）
    let mut model_manager = ep_core::model::ModelManager::new(&config.models, &root)
        .with_network(config.network.clone())
        .with_manifests(manifests_from(&discovered));

    // 进行中的下载句柄（model_id → handle），供取消与进度转发任务清理
    let download_handles: Arc<Mutex<HashMap<String, ep_core::model::DownloadHandle>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // P1-6：任务注册表 —— ep_core::task_registry 直连（daemon/桌面共用），
    // 持久化 runtime/tasks/<task_id>.json；启动时回读历史（daemon 跑过的任务可见）。
    let task_registry: Arc<Mutex<TaskRegistry>> = Arc::new(Mutex::new(TaskRegistry::load(
        root.join("runtime").join("tasks"),
    )));
    // 进行中任务的协作取消标志（task_id → flag；与 runner 共享同一 AtomicBool）
    let mut task_cancel_flags: HashMap<String, Arc<AtomicBool>> = HashMap::new();
    // 启动即推送任务快照，任务页不再恒空（P1-6）
    send_task_snapshot(&task_registry, &tx);

    // P1-7：模块日志增量转发 —— 每个模块上一轮的缓冲区快照（diff 出新增行）
    let mut log_snapshots: HashMap<String, Vec<String>> = HashMap::new();

    // 启动时自动检查依赖并刷新模型列表
    let _ = tx.send(AppMsg::DepReportRefreshed(
        ep_core::deps::DepReport::check_all(&root),
    ));
    let _ = tx.send(AppMsg::ModelsRefreshed(
        model_manager.list_all_models(&manifests_from(&discovered)),
    ));

    // Periodic timers
    let mut device_timer = tokio::time::interval(std::time::Duration::from_secs(
        config.compute.refresh_interval_secs.max(2) as u64,
    ));
    device_timer.tick().await; // consume immediate first tick
    let mut monitor_timer = tokio::time::interval(std::time::Duration::from_secs(1));
    monitor_timer.tick().await;

    let mut current_devices = devices;

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(AppCmd::StartModule(module_id)) => {
                        let manifest = discovered
                            .iter()
                            .find(|m| {
                                m.manifest
                                    .as_ref()
                                    .map(|mf| mf.module.id == module_id)
                                    .unwrap_or(false)
                            })
                            .and_then(|m| m.manifest.clone());

                        if let Some(manifest) = manifest {
                            match port_manager.allocate(&module_id) {
                                Ok(port) => {
                                    // P1-1（桌面侧）：设备选择走 ComputeScheduler
                                    // （manifest backends 过滤 + least_memory +
                                    // allow_overcommit），不再"首个非 CPU"盲选
                                    let device =
                                        assign_module_device(&scheduler, &manifest, &config);

                                    match device {
                                        Some(device) => {
                                            // A2（P0-4 前置）：公共构建函数产出标准模板变量
                                            // （ROOT/MODULE_DIR/...），start_module 统一加 EP_ 前缀
                                            // 并注入 CUDA 库路径 + compute.env，不再传空 map
                                            let env_vars = ep_core::process::build_module_env(
                                                &root, &config, &module_id, &manifest, &device,
                                            );

                                            match process_manager
                                                .start_module(
                                                    &module_id,
                                                    &manifest,
                                                    device.clone(),
                                                    port,
                                                    env_vars,
                                                )
                                                .await
                                            {
                                                Ok(()) => {
                                                    let _ = tx.send(AppMsg::ModuleStarted(
                                                        module_id,
                                                        port,
                                                        device.to_string(),
                                                    ));
                                                }
                                                Err(e) => {
                                                    port_manager.release(&module_id);
                                                    scheduler.release(&module_id);
                                                    let _ = tx.send(AppMsg::Error(tr(
                                                        lang,
                                                        "desktopApp.error.startModuleFailed",
                                                        &[
                                                            ("id", &module_id),
                                                            ("detail", &e.to_string()),
                                                        ],
                                                    )));
                                                }
                                            }
                                        }
                                        None => {
                                            port_manager.release(&module_id);
                                            let _ = tx.send(AppMsg::Error(tr(
                                                lang,
                                                "desktopApp.error.noCompatibleDevice",
                                                &[("id", &module_id)],
                                            )));
                                        }
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(AppMsg::Error(tr(
                                        lang,
                                        "desktopApp.error.portAllocFailed",
                                        &[("detail", &e.to_string())],
                                    )));
                                }
                            }
                        } else {
                            let _ = tx.send(AppMsg::Error(tr(
                                lang,
                                "desktopApp.error.moduleNotFoundOrInvalid",
                                &[("id", &module_id)],
                            )));
                        }
                    }
                    Some(AppCmd::StopModule(module_id)) => {
                        let _ = process_manager.stop_module(&module_id).await;
                        port_manager.release(&module_id);
                        scheduler.release(&module_id);
                        let _ = tx.send(AppMsg::ModuleStopped(module_id));
                    }
                    Some(AppCmd::DownloadModel { module_id, model_id, source }) => {
                        // 在已发现模块中查找 manifest 与对应模型声明
                        let manifest = discovered
                            .iter()
                            .filter_map(|m| m.manifest.as_ref())
                            .find(|mf| mf.module.id == module_id)
                            .cloned();
                        let decl = manifest
                            .as_ref()
                            .and_then(|mf| {
                                mf.models.iter().find(|d| d.id == model_id).cloned()
                            });

                        match (manifest, decl) {
                            (Some(manifest), Some(decl)) => {
                                // P0-5 修复：下载前 ensure_venv —— 化解全新安装
                                // "下载需要 venv、启动又需要模型"的死锁
                                // （daemon models.rs 下载前置同款逻辑）。
                                let venv_prep = if manifest.runtime.runtime_type
                                    == RuntimeType::Python
                                {
                                    let existing = ep_core::process::venv_python_path(
                                        &root, &module_id,
                                    );
                                    if existing.exists() {
                                        Ok(existing)
                                    } else {
                                        let _ = tx.send(AppMsg::Info(tr(
                                            lang,
                                            "desktopApp.info.venvPreparing",
                                            &[("id", &module_id)],
                                        )));
                                        prepare_module_venv(
                                            &root, &config, &module_id, &manifest,
                                        )
                                        .await
                                        .map_err(|e| e.to_string())
                                    }
                                } else {
                                    // native 运行时：沿用 venv 存在性检查（下载脚本为 python）
                                    let existing = ep_core::process::venv_python_path(
                                        &root, &module_id,
                                    );
                                    if existing.exists() {
                                        Ok(existing)
                                    } else {
                                        Err(String::new())
                                    }
                                };

                                match venv_prep {
                                    Ok(venv_python) => {
                                        // 任务化下载：立即返回句柄，进度经转发任务回传，
                                        // 绝不阻塞事件循环
                                        start_model_download(
                                            &model_manager,
                                            &download_handles,
                                            &tx,
                                            lang,
                                            &module_id,
                                            &decl,
                                            &venv_python,
                                            &config,
                                            source,
                                        )
                                        .await;
                                    }
                                    Err(detail) => {
                                        if detail.is_empty() {
                                            let _ = tx.send(AppMsg::Error(tr(
                                                lang,
                                                "desktopApp.error.startModuleFirst",
                                                &[],
                                            )));
                                        } else {
                                            let _ = tx.send(AppMsg::Error(tr(
                                                lang,
                                                "desktopApp.error.venvPrepFailed",
                                                &[("detail", &detail)],
                                            )));
                                        }
                                        let _ = tx.send(AppMsg::ModelDownloadFinished(
                                            model_id, false,
                                        ));
                                    }
                                }
                            }
                            _ => {
                                let _ = tx.send(AppMsg::Error(tr(
                                    lang,
                                    "desktopApp.error.moduleOrModelNotFound",
                                    &[("module", &module_id), ("model", &model_id)],
                                )));
                                let _ = tx
                                    .send(AppMsg::ModelDownloadFinished(model_id, false));
                            }
                        }
                    }
                    Some(AppCmd::CancelDownload(model_id)) => {
                        // 从句柄映射中取出引用并取消（cancel 幂等；supervise 任务会发 Cancelled）
                        let mut guard = download_handles.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(handle) = guard.get_mut(&model_id) {
                            handle.cancel();
                        }
                    }
                    Some(AppCmd::CheckUpdate { module_id, model_id }) => {
                        let decl = discovered
                            .iter()
                            .filter_map(|m| m.manifest.as_ref())
                            .find(|mf| mf.module.id == module_id)
                            .and_then(|mf| {
                                mf.models.iter().find(|d| d.id == model_id).cloned()
                            });
                        if let Some(decl) = decl {
                            // spawn 独立任务，避免阻塞命令循环；用独立 ModelManager（同一配置）
                            let models_cfg = config.models.clone();
                            let network = config.network.clone();
                            let root2 = root.clone();
                            let tx2 = tx.clone();
                            tokio::spawn(async move {
                                let mgr = ep_core::model::ModelManager::new(&models_cfg, &root2)
                                    .with_network(network);
                                let result = mgr.check_update_available(&decl).await;
                                let _ = tx2.send(AppMsg::ModelUpdateChecked {
                                    model_id,
                                    result,
                                    notify: true,
                                });
                            });
                        } else {
                            let _ = tx.send(AppMsg::Error(tr(
                                lang,
                                "desktopApp.error.moduleOrModelNotFoundUpdate",
                                &[("module", &module_id), ("model", &model_id)],
                            )));
                        }
                    }
                    Some(AppCmd::CheckAllUpdates) => {
                        // 收集所有 Ready 模型的声明，spawn 单任务内并发检查并汇总
                        let ready_models = model_manager
                            .list_all_models(&manifests_from(&discovered))
                            .into_iter()
                            .filter(|mv| mv.status == ep_core::model::ModelStatus::Ready)
                            .collect::<Vec<_>>();

                        let decls: Vec<(String, ep_core::module::ModelDecl)> = ready_models
                            .iter()
                            .filter_map(|mv| {
                                discovered
                                    .iter()
                                    .filter_map(|m| m.manifest.as_ref())
                                    .find(|mf| mf.module.id == mv.module_id)
                                    .and_then(|mf| {
                                        mf.models
                                            .iter()
                                            .find(|d| d.id == mv.model_id)
                                            .map(|d| (mv.model_id.clone(), d.clone()))
                                    })
                            })
                            .collect();

                        // 各模型各自 spawn 并发检查（JoinSet），完成后汇总
                        let models_cfg = config.models.clone();
                        let network = config.network.clone();
                        let root2 = root.clone();
                        let tx2 = tx.clone();
                        tokio::spawn(async move {
                            let mut set = tokio::task::JoinSet::new();
                            for (model_id, decl) in decls {
                                let models_cfg = models_cfg.clone();
                                let network = network.clone();
                                let root2 = root2.clone();
                                set.spawn(async move {
                                    let mgr =
                                        ep_core::model::ModelManager::new(&models_cfg, &root2)
                                            .with_network(network);
                                    let result = mgr.check_update_available(&decl).await;
                                    (model_id, result)
                                });
                            }
                            let total = set.len();
                            let mut available = 0usize;
                            while let Some(joined) = set.join_next().await {
                                if let Ok((model_id, result)) = joined {
                                    if result.available {
                                        available += 1;
                                    }
                                    let _ = tx2.send(AppMsg::ModelUpdateChecked {
                                        model_id,
                                        result,
                                        notify: false,
                                    });
                                }
                            }
                            let _ = tx2.send(AppMsg::UpdatesCheckSummary { total, available });
                        });
                    }
                    Some(AppCmd::DeleteModel(target_dir)) => {
                        let dir = model_manager.model_dir(&target_dir);
                        match tokio::fs::remove_dir_all(&dir).await {
                            Ok(()) => {
                                let _ = tx.send(AppMsg::ModelsRefreshed(
                                    model_manager.list_all_models(&manifests_from(&discovered)),
                                ));
                            }
                            Err(e) => {
                                let _ = tx.send(AppMsg::Error(tr(
                                    lang,
                                    "desktopApp.error.deleteModelFailed",
                                    &[("detail", &e.to_string())],
                                )));
                            }
                        }
                    }
                    Some(AppCmd::ImportModel {
                        module_id,
                        model_id,
                        source,
                    }) => {
                        match model_manager
                            .import_model(&module_id, &model_id, &source)
                            .await
                        {
                            Ok(()) => {
                                let _ = tx.send(AppMsg::ModelsRefreshed(
                                    model_manager.list_all_models(&manifests_from(&discovered)),
                                ));
                            }
                            Err(e) => {
                                let _ = tx.send(AppMsg::Error(tr(
                                    lang,
                                    "desktopApp.error.importModelFailed",
                                    &[("detail", &e.to_string())],
                                )));
                            }
                        }
                    }
                    Some(AppCmd::RefreshModels) => {
                        // 重新扫描模块目录并刷新模型列表；同步更新 ModelManager 注册的 manifests
                        discovered = ep_core::module::discover_modules(&modules_dir);
                        model_manager.set_manifests(manifests_from(&discovered));
                        let _ = tx.send(AppMsg::ModelsRefreshed(
                            model_manager.list_all_models(&manifests_from(&discovered)),
                        ));
                    }
                    Some(AppCmd::RefreshDeps) => {
                        let report = ep_core::deps::DepReport::check_all(&root);
                        let _ = tx.send(AppMsg::DepReportRefreshed(report));
                    }
                    // ── Wave 3 C4：整合包 / 直跑 / 任务（S2 骨架注册点实现） ──
                    Some(AppCmd::RefreshPacks) => {
                        // ep-pack 注册表读取（runtime/packs/*.json）
                        let registry_dir = root.join("runtime").join("packs");
                        match ep_pack::import::list_installed_packs(&registry_dir) {
                            Ok(packs) => {
                                let entries =
                                    packs.into_iter().map(pack_entry_from).collect();
                                let _ = tx.send(AppMsg::PacksRefreshed(entries));
                            }
                            Err(e) => {
                                let _ = tx.send(AppMsg::Error(tr(
                                    lang,
                                    "desktopApp.error.packListFailed",
                                    &[("detail", &e.to_string())],
                                )));
                            }
                        }
                    }
                    Some(AppCmd::ImportPack { path }) => {
                        // §4.4 导入编排直调（ep_pack::import::import_pack）：
                        // 暂存/校验/落位/注册全流程；进度经 AppMsg::PackImportProgress
                        // 上报，终态 PackImportFinished；reference 模型自动驱动后台下载。
                        if !path.is_file() {
                            let _ = tx.send(AppMsg::Error(tr(
                                lang,
                                "desktopApp.error.packImportFailed",
                                &[("detail", &format!("file not found: {}", path.display()))],
                            )));
                        } else {
                            // 进度状态键：归档文件名（与终态消息键一致）
                            let progress_key = path
                                .file_stem()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_else(|| "pack".to_string());

                            // 导入输入快照（spawn 任务独立持有，不阻塞事件循环）
                            let staging_cfg = config.packs.staging_dir.clone();
                            let staging = {
                                let p = Path::new(&staging_cfg);
                                if p.is_absolute() {
                                    p.to_path_buf()
                                } else {
                                    root.join(p)
                                }
                            };
                            let _ = std::fs::create_dir_all(&staging);
                            let mut targets = ep_pack::import::ImportTargets::from_root(&root);
                            targets.models_dir = config.resolve_model_cache_dir(&root);
                            let options = ep_pack::import::ImportOptions::default();
                            let devices_snapshot = current_devices.clone();
                            let manifests_snapshot = manifests_from(&discovered);
                            let models_cfg = config.models.clone();
                            let network_cfg = config.network.clone();
                            let root2 = root.clone();
                            let config_snapshot = config.clone();
                            let tx2 = tx.clone();
                            let handles2 = Arc::clone(&download_handles);
                            let key = progress_key.clone();

                            tokio::spawn(async move {
                                let path_bg = path.clone();
                                let staging_bg = staging.clone();
                                let targets_bg = targets.clone();
                                let devices_bg = devices_snapshot.clone();
                                let manifests_bg = manifests_snapshot.clone();
                                let key_bg = key.clone();
                                let tx_progress = tx2.clone();
                                let result = tokio::task::spawn_blocking(move || {
                                    ep_pack::import::import_pack(
                                        &path_bg,
                                        &staging_bg,
                                        &targets_bg,
                                        &options,
                                        &devices_bg,
                                        |entry| {
                                            desktop_resolve_entry(&manifests_bg, entry)
                                        },
                                        move |p: ep_pack::import::PackImportProgress| {
                                            let _ = tx_progress.send(
                                                AppMsg::PackImportProgress {
                                                    pack_id: key_bg.clone(),
                                                    stage: p.stage.as_str().to_string(),
                                                    percent: Some(p.percent as f32),
                                                },
                                            );
                                        },
                                    )
                                })
                                .await;

                                match result {
                                    Ok(Ok(report)) => {
                                        // 终态提示计数（pending_downloads 随后被循环消费）
                                        let models_count =
                                            report.installed_models.len().to_string();
                                        let downloads_count =
                                            report.pending_downloads.len().to_string();
                                        let pipelines_count =
                                            report.pipelines_installed.len().to_string();

                                        // reference 模型 → 后台下载（复用 DownloadHandle 进度设施）
                                        let mgr = ep_core::model::ModelManager::new(
                                            &models_cfg,
                                            &root2,
                                        )
                                        .with_network(network_cfg.clone())
                                        .with_manifests(manifests_snapshot.clone());
                                        for req in report.pending_downloads {
                                            let Some(manifest) = manifests_snapshot
                                                .iter()
                                                .find(|m| m.module.id == req.module_id)
                                            else {
                                                tracing::warn!(
                                                    module = %req.module_id,
                                                    "pack reference download skipped: module not found"
                                                );
                                                continue;
                                            };
                                            let Some(decl) = manifest
                                                .models
                                                .iter()
                                                .find(|d| d.id == req.model_id)
                                            else {
                                                continue;
                                            };
                                            // P0-5：下载前 venv 准备
                                            let venv = match prepare_module_venv(
                                                &root2,
                                                &config_snapshot,
                                                &req.module_id,
                                                manifest,
                                            )
                                            .await
                                            {
                                                Ok(p) => p,
                                                Err(e) => {
                                                    let _ = tx2.send(AppMsg::Error(tr(
                                                        lang,
                                                        "desktopApp.error.venvPrepFailed",
                                                        &[("detail", &e.to_string())],
                                                    )));
                                                    continue;
                                                }
                                            };
                                            start_model_download(
                                                &mgr,
                                                &handles2,
                                                &tx2,
                                                lang,
                                                &req.module_id,
                                                decl,
                                                &venv,
                                                &config_snapshot,
                                                None,
                                            )
                                            .await;
                                        }

                                        let _ = tx2.send(AppMsg::Info(tr(
                                            lang,
                                            "desktopApp.info.packImportDone",
                                            &[
                                                ("models", &models_count),
                                                ("downloads", &downloads_count),
                                                ("pipelines", &pipelines_count),
                                            ],
                                        )));
                                        let _ = tx2.send(AppMsg::PackImportFinished {
                                            pack_id: key,
                                            success: true,
                                        });
                                    }
                                    Ok(Err(e)) => {
                                        let _ = tx2.send(AppMsg::Error(tr(
                                            lang,
                                            "desktopApp.error.packImportFailed",
                                            &[("detail", &e.to_string())],
                                        )));
                                        let _ = tx2.send(AppMsg::PackImportFinished {
                                            pack_id: key,
                                            success: false,
                                        });
                                    }
                                    Err(join_err) => {
                                        let _ = tx2.send(AppMsg::Error(tr(
                                            lang,
                                            "desktopApp.error.packImportFailed",
                                            &[("detail", &format!("import task panicked: {join_err}"))],
                                        )));
                                        let _ = tx2.send(AppMsg::PackImportFinished {
                                            pack_id: key,
                                            success: false,
                                        });
                                    }
                                }
                            });
                        }
                    }
                    Some(AppCmd::ExportPack { spec }) => {
                        // 协调记录 #47 导出模块：后台组装暂存目录（bundle 硬链接优先）
                        // → ep_pack::build::build_pack → 用户选定目录 .epzip。
                        if spec.modules.is_empty() && spec.pipelines.is_empty() {
                            let _ = tx.send(AppMsg::Error(trfb_bg(
                                lang,
                                "desktopApp.error.packExportEmpty",
                                "导出失败：未圈选任何模型或管线",
                                &[],
                            )));
                        } else {
                            let manifests_snapshot = manifests_from(&discovered);
                            let models_cfg = config.models.clone();
                            let staging_cfg = config.packs.staging_dir.clone();
                            let root2 = root.clone();
                            let tx2 = tx.clone();
                            tokio::spawn(async move {
                                let result = tokio::task::spawn_blocking(move || {
                                    build_export_pack(
                                        &root2,
                                        &staging_cfg,
                                        &models_cfg,
                                        &manifests_snapshot,
                                        spec,
                                    )
                                })
                                .await;
                                match result {
                                    Ok(Ok((archive, files))) => {
                                        let files_s = files.to_string();
                                        let _ = tx2.send(AppMsg::Info(trfb_bg(
                                            lang,
                                            "desktopApp.info.packExportDone",
                                            "导出完成：{{path}}（{{files}} 个文件）",
                                            &[
                                                ("path", &archive.display().to_string()),
                                                ("files", &files_s),
                                            ],
                                        )));
                                    }
                                    Ok(Err(e)) => {
                                        let _ = tx2.send(AppMsg::Error(trfb_bg(
                                            lang,
                                            "desktopApp.error.packExportFailed",
                                            "导出失败：{{detail}}",
                                            &[("detail", &e)],
                                        )));
                                    }
                                    Err(join_err) => {
                                        let _ = tx2.send(AppMsg::Error(trfb_bg(
                                            lang,
                                            "desktopApp.error.packExportFailed",
                                            "导出失败：{{detail}}",
                                            &[("detail", &format!("export task panicked: {join_err}"))],
                                        )));
                                    }
                                }
                            });
                        }
                    }
                    Some(AppCmd::UninstallPack { pack_id, keep_models }) => {
                        // 协调记录 #47：pack 来源徽章菜单「卸载来源整合包」。
                        // 语义对齐 daemon DELETE /api/packs/{id}：
                        // keep_models=false → 删除 meta.pack_id 指向本包的模型目录；
                        // 本包安装的管线与注册表条目一并移除。
                        let registry_dir = root.join("runtime").join("packs");
                        let reg_path =
                            ep_pack::import::registry_entry_path(&registry_dir, &pack_id);
                        match ep_pack::import::read_installed_pack(&reg_path) {
                            Ok(Some(installed)) => {
                                if !keep_models {
                                    for model in model_manager.list_downloaded_models() {
                                        if model.meta.pack_id.as_deref()
                                            == Some(installed.id.as_str())
                                        {
                                            let dir = model_manager.model_dir(&model.target_dir);
                                            if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
                                                tracing::warn!(
                                                    dir = %dir.display(),
                                                    error = %e,
                                                    "failed to remove pack model dir"
                                                );
                                            }
                                        }
                                    }
                                }
                                // 管线删除：按已装 id 反查 config/pipelines/*.toml
                                let pipelines_dir = root.join("config").join("pipelines");
                                if let Ok(rd) = tokio::fs::read_dir(&pipelines_dir).await {
                                    let mut entries = Vec::new();
                                    let mut rd = rd;
                                    while let Ok(Some(entry)) = rd.next_entry().await {
                                        entries.push(entry.path());
                                    }
                                    for path in entries {
                                        if path.extension().and_then(|x| x.to_str())
                                            != Some("toml")
                                        {
                                            continue;
                                        }
                                        let path_bg = path.clone();
                                        let parse = tokio::task::spawn_blocking(move || {
                                            ep_core::pipeline::Pipeline::from_toml(&path_bg)
                                        })
                                        .await;
                                        if let Ok(Ok(pipeline)) = parse {
                                            if installed.pipelines.contains(&pipeline.id) {
                                                if let Err(e) = tokio::fs::remove_file(&path).await
                                                {
                                                    tracing::warn!(
                                                        file = %path.display(),
                                                        error = %e,
                                                        "failed to remove pack pipeline"
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                                if let Err(e) = tokio::fs::remove_file(&reg_path).await {
                                    if e.kind() != std::io::ErrorKind::NotFound {
                                        tracing::warn!(
                                            path = %reg_path.display(),
                                            error = %e,
                                            "failed to remove pack registry file"
                                        );
                                    }
                                }
                                let _ = tx.send(AppMsg::Info(trfb_bg(
                                    lang,
                                    "desktopApp.info.packUninstalled",
                                    "已卸载整合包「{{id}}」",
                                    &[("id", &installed.id)],
                                )));
                                // 刷新模型列表与已装包列表
                                let _ = tx.send(AppMsg::ModelsRefreshed(
                                    model_manager.list_all_models(&manifests_from(&discovered)),
                                ));
                                match ep_pack::import::list_installed_packs(&registry_dir) {
                                    Ok(packs) => {
                                        let entries =
                                            packs.into_iter().map(pack_entry_from).collect();
                                        let _ = tx.send(AppMsg::PacksRefreshed(entries));
                                    }
                                    Err(e) => {
                                        let _ = tx.send(AppMsg::Error(tr(
                                            lang,
                                            "desktopApp.error.packListFailed",
                                            &[("detail", &e.to_string())],
                                        )));
                                    }
                                }
                            }
                            Ok(None) => {
                                let _ = tx.send(AppMsg::Error(trfb_bg(
                                    lang,
                                    "desktopApp.error.packNotFound",
                                    "整合包「{{id}}」未安装或注册条目缺失",
                                    &[("id", &pack_id)],
                                )));
                            }
                            Err(e) => {
                                let _ = tx.send(AppMsg::Error(tr(
                                    lang,
                                    "desktopApp.error.packListFailed",
                                    &[("detail", &e.to_string())],
                                )));
                            }
                        }
                    }
                    Some(AppCmd::ExecuteSingle {
                        module_id,
                        capability,
                        params,
                        input_path,
                    }) => {
                        // §5.3 单模型直跑：校验 → 参数类型化 → 退化 DAG →
                        // 统一提交路径（模块自动拉起 / 任务注册表 / 产物归集全套复用）
                        let manifest = discovered
                            .iter()
                            .filter_map(|m| m.manifest.as_ref())
                            .find(|mf| mf.module.id == module_id)
                            .cloned();
                        let Some(manifest) = manifest else {
                            let _ = tx.send(AppMsg::Error(tr(
                                lang,
                                "desktopApp.error.moduleNotFoundOrInvalid",
                                &[("id", &module_id)],
                            )));
                            continue;
                        };
                        let cap = manifest
                            .interface
                            .capabilities
                            .iter()
                            .find(|c| c.name == capability)
                            .cloned();
                        let Some(cap) = cap else {
                            let _ = tx.send(AppMsg::Error(tr(
                                lang,
                                "desktopApp.error.capabilityNotFound",
                                &[("module", &module_id), ("capability", &capability)],
                            )));
                            continue;
                        };
                        if !input_path.is_file() {
                            let _ = tx.send(AppMsg::Error(tr(
                                lang,
                                "desktopApp.error.inputFileMissing",
                                &[("path", &input_path.display().to_string())],
                            )));
                            continue;
                        }
                        match typed_exec_params(cap.params.as_ref(), &params) {
                            Err(detail) => {
                                let _ = tx.send(AppMsg::Error(tr(
                                    lang,
                                    "desktopApp.error.paramInvalid",
                                    &[("detail", &detail)],
                                )));
                            }
                            Ok(value) => {
                                let pipeline = build_direct_pipeline(
                                    &module_id,
                                    &capability,
                                    value,
                                    &input_path,
                                );
                                if let Some(task_id) = submit_pipeline_task(
                                    &tx,
                                    lang,
                                    &root,
                                    &config,
                                    &discovered,
                                    &model_manager,
                                    &mut process_manager,
                                    &mut port_manager,
                                    &mut scheduler,
                                    &task_registry,
                                    &mut task_cancel_flags,
                                    pipeline,
                                )
                                .await
                                {
                                    let _ = tx.send(AppMsg::DirectExecSubmitted(task_id));
                                    send_task_snapshot(&task_registry, &tx);
                                }
                            }
                        }
                    }
                    Some(AppCmd::RefreshPipelineTasks { pipeline_id }) => {
                        // §6.8 管线级任务视图：注册表按 pipeline_id 过滤
                        let tasks: Vec<_> = {
                            let reg =
                                task_registry.lock().unwrap_or_else(|e| e.into_inner());
                            reg.by_pipeline(&pipeline_id)
                                .iter()
                                .map(record_to_summary)
                                .collect()
                        };
                        let _ = tx.send(AppMsg::PipelineTasksRefreshed {
                            pipeline_id,
                            tasks,
                        });
                    }
                    // ── Wave 3 C4：管线执行 / 任务刷新 / 取消（决策 2） ──
                    Some(AppCmd::RefreshTasks) => {
                        // P1-6：任务拉取 —— 注册表（runtime/tasks + 内存快照）→ TasksRefreshed
                        send_task_snapshot(&task_registry, &tx);
                    }
                    Some(AppCmd::ExecutePipeline { pipeline }) => {
                        // 决策 2 桌面侧管线执行入口：直连 ep-core runner + task_registry，
                        // 产物归集 workspace/tasks/<task_id>/，支持取消与节点超时
                        if let Some(task_id) = submit_pipeline_task(
                            &tx,
                            lang,
                            &root,
                            &config,
                            &discovered,
                            &model_manager,
                            &mut process_manager,
                            &mut port_manager,
                            &mut scheduler,
                            &task_registry,
                            &mut task_cancel_flags,
                            pipeline,
                        )
                        .await
                        {
                            let _ = tx.send(AppMsg::Info(tr(
                                lang,
                                "desktopApp.info.taskSubmitted",
                                &[("id", &task_id)],
                            )));
                            send_task_snapshot(&task_registry, &tx);
                        }
                    }
                    Some(AppCmd::CancelTask { task_id }) => {
                        // 协作取消：置位共享标志（runner 节点边界检查）+ 立即逻辑终态
                        //（daemon request_cancel 同款语义：运行中取消立即判 cancelled，
                        // 引擎线程后台收尾时的终态写入因记录已终态而被忽略）
                        let known = {
                            let reg =
                                task_registry.lock().unwrap_or_else(|e| e.into_inner());
                            reg.get(&task_id).is_some()
                        };
                        if !known {
                            let _ = tx.send(AppMsg::Error(tr(
                                lang,
                                "desktopApp.error.taskNotFound",
                                &[("id", &task_id)],
                            )));
                        } else {
                            if let Some(flag) = task_cancel_flags.get(&task_id) {
                                flag.store(true, Ordering::SeqCst);
                            }
                            {
                                let mut reg =
                                    task_registry.lock().unwrap_or_else(|e| e.into_inner());
                                let _ = reg.update(&task_id, |record| {
                                    if !record.status.is_terminal() {
                                        record.status = TaskState::Cancelled;
                                        record.finished_at = Some(Utc::now());
                                        record.queue_position = None;
                                    }
                                });
                            }
                            send_task_snapshot(&task_registry, &tx);
                        }
                    }
                    Some(AppCmd::Shutdown) => break,
                    None => break,
                }
            }
            _ = device_timer.tick() => {
                ep_core::compute::refresh_all_devices(
                    &mut current_devices,
                    &config.compute.disabled_backends,
                );
                let _ = tx.send(AppMsg::DevicesRefreshed(current_devices.clone()));
                // 调度器仅在设备**集合**变化时重建（周期刷新的利用率/显存采样
                // 不改集合），避免已记录的显存分配被清零
                let key = device_ids_key(&current_devices);
                if key != scheduler_devices_key {
                    scheduler = make_scheduler(&current_devices, &config);
                    scheduler_devices_key = key;
                }
            }
            _ = monitor_timer.tick() => {
                // Check for exited processes and send status updates
                let module_ids: Vec<String> = discovered
                    .iter()
                    .filter_map(|m| {
                        m.manifest.as_ref().map(|mf| mf.module.id.clone())
                    })
                    .collect();
                for mid in &module_ids {
                    let _ = process_manager.monitor_process(mid).await;

                    // P1-7：模块子进程日志 → AppMsg::LogLine 推送（渲染端已存在）。
                    // monitor_process 内部 poll_logs 已将 channel 新行写入环形缓冲区，
                    // 此处对快照做增量 diff，只转发新增行。
                    if let Some(inst) = process_manager.get_instance(mid) {
                        let current: Vec<String> =
                            inst.log_buffer.iter().cloned().collect();
                        let prev = log_snapshots.entry(mid.clone()).or_default();
                        for line in new_log_lines(prev, &current) {
                            let _ = tx.send(AppMsg::LogLine(mid.clone(), line));
                        }
                        *prev = current;
                    }

                    if let Some(status) = process_manager.get_status(mid) {
                        let _ = tx.send(AppMsg::ModuleStatusUpdate(
                            mid.clone(),
                            status.clone(),
                        ));
                    }
                }

                // P1-6：有活跃任务时周期推送任务快照（任务页进度条实时性）
                {
                    let has_active = {
                        let reg =
                            task_registry.lock().unwrap_or_else(|e| e.into_inner());
                        reg.all().iter().any(|r| r.is_active())
                    };
                    if has_active {
                        send_task_snapshot(&task_registry, &tx);
                    }
                }
            }
        }
    }
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ep_core::pipeline::runner::TaskSummary;
    use ep_core::task_registry::TaskState;

    fn task_record(id: &str, pipeline_id: &str, status: TaskState) -> TaskRecord {
        TaskRecord {
            id: id.to_string(),
            pipeline_id: pipeline_id.to_string(),
            status,
            error: None,
            queue_position: None,
            started_at: Utc::now(),
            started_running_at: None,
            finished_at: None,
            node_order: vec!["input".into(), "run".into(), "output".into()],
            nodes: HashMap::new(),
            artifacts: HashMap::new(),
            served_artifacts: HashMap::new(),
            work_dir: PathBuf::from("/tmp/tasks").join(id),
        }
    }

    // ── 直跑退化 DAG 形状（对齐 daemon build_direct_pipeline 契约） ────────

    #[test]
    fn direct_pipeline_shape_matches_contract() {
        let pipeline = build_direct_pipeline(
            "faster-whisper",
            "transcribe",
            serde_json::json!({ "beam_size": 5 }),
            Path::new("/data/in.mp3"),
        );
        assert_eq!(pipeline.id, "direct/faster-whisper");
        assert_eq!(pipeline.nodes.len(), 3);
        assert_eq!(pipeline.edges.len(), 2);
        // input：file_input + path 参数
        match &pipeline.nodes[0].kind {
            NodeKind::Builtin { builtin } => assert_eq!(builtin, "file_input"),
            other => panic!("input node must be builtin file_input, got {other:?}"),
        }
        assert_eq!(
            pipeline.nodes[0].params["path"],
            serde_json::Value::String("/data/in.mp3".to_string())
        );
        // run：module 节点携带 module_id/capability/params
        match &pipeline.nodes[1].kind {
            NodeKind::Module {
                module_id,
                capability,
                model_id,
                device,
            } => {
                assert_eq!(module_id, "faster-whisper");
                assert_eq!(capability, "transcribe");
                assert!(model_id.is_none());
                assert!(device.is_none());
            }
            other => panic!("run node must be module, got {other:?}"),
        }
        assert_eq!(pipeline.nodes[1].params["beam_size"], 5);
        // output：file_output 无 path（引擎派生输出名）
        match &pipeline.nodes[2].kind {
            NodeKind::Builtin { builtin } => assert_eq!(builtin, "file_output"),
            other => panic!("output node must be builtin file_output, got {other:?}"),
        }
        // 退化 DAG 恒通过校验（含 file_input 要求）
        assert!(pipeline.validate().is_ok());
    }

    // ── 直跑参数类型化（schema 驱动） ───────────────────────────────────────

    fn param_schema(t: &str) -> ParamSchema {
        ParamSchema {
            param_type: t.to_string(),
            default: None,
            description: None,
            min: None,
            max: None,
            step: None,
            enum_values: None,
            options: None,
        }
    }

    #[test]
    fn typed_params_converts_by_schema() {
        let mut schema = HashMap::new();
        schema.insert("beam_size".to_string(), param_schema("integer"));
        schema.insert("min_db".to_string(), param_schema("float"));
        schema.insert("vad_filter".to_string(), param_schema("boolean"));
        schema.insert("language".to_string(), param_schema("string"));

        let raw = vec![
            ("beam_size".to_string(), "7".to_string()),
            ("min_db".to_string(), "-42.5".to_string()),
            ("vad_filter".to_string(), "false".to_string()),
            ("language".to_string(), "zh".to_string()),
        ];
        let value = typed_exec_params(Some(&schema), &raw).unwrap();
        assert_eq!(value["beam_size"], serde_json::json!(7));
        assert_eq!(value["min_db"], serde_json::json!(-42.5));
        assert_eq!(value["vad_filter"], serde_json::json!(false));
        assert_eq!(value["language"], serde_json::json!("zh"));
    }

    #[test]
    fn typed_params_fills_defaults_and_requires_missing() {
        let mut schema = HashMap::new();
        let mut with_default = param_schema("integer");
        with_default.default = Some(serde_json::json!(5));
        schema.insert("beam_size".to_string(), with_default);
        schema.insert("required_text".to_string(), param_schema("string"));

        // 缺失必填 → 错误
        let err = typed_exec_params(Some(&schema), &[]).unwrap_err();
        assert!(err.contains("required_text"));

        // 提供必填、缺省注入默认值
        let raw = vec![("required_text".to_string(), "hello".to_string())];
        let value = typed_exec_params(Some(&schema), &raw).unwrap();
        assert_eq!(value["beam_size"], serde_json::json!(5));
        assert_eq!(value["required_text"], serde_json::json!("hello"));
    }

    #[test]
    fn typed_params_rejects_bad_values_and_enum_miss() {
        let mut schema = HashMap::new();
        schema.insert("beam_size".to_string(), param_schema("integer"));
        assert!(typed_exec_params(
            Some(&schema),
            &[("beam_size".to_string(), "five".to_string())]
        )
        .is_err());

        let mut enum_schema = param_schema("string");
        enum_schema.enum_values = Some(vec!["a".to_string(), "b".to_string()]);
        let mut schema = HashMap::new();
        schema.insert("mode".to_string(), enum_schema);
        assert!(typed_exec_params(
            Some(&schema),
            &[("mode".to_string(), "turbo".to_string())]
        )
        .is_err());
        let ok = typed_exec_params(
            Some(&schema),
            &[("mode".to_string(), "a".to_string())],
        )
        .unwrap();
        assert_eq!(ok["mode"], serde_json::json!("a"));
    }

    // ── 任务记录 → 摘要映射（P1-6） ────────────────────────────────────────

    #[test]
    fn record_summary_maps_states_and_progress() {
        let mut r = task_record("t1", "pipe-a", TaskState::Running);
        r.nodes.insert(
            "input".into(),
            NodeRecord {
                state: "completed".into(),
                error: None,
            },
        );
        r.nodes.insert(
            "run".into(),
            NodeRecord {
                state: "running".into(),
                error: None,
            },
        );
        let s: TaskSummary = record_to_summary(&r);
        assert_eq!(s.id, "t1");
        assert_eq!(s.pipeline_name, "pipe-a");
        assert_eq!(s.status, TaskStatus::Running);
        assert_eq!(s.node_count, 3);
        assert_eq!(s.completed_nodes, 1);

        let mut failed = task_record("t2", "p", TaskState::Failed);
        failed.error = Some("boom".into());
        assert!(matches!(
            record_to_summary(&failed).status,
            TaskStatus::Failed(ref e) if e == "boom"
        ));

        assert_eq!(
            record_to_summary(&task_record("t3", "p", TaskState::Queued)).status,
            TaskStatus::Pending
        );
        assert_eq!(
            record_to_summary(&task_record("t4", "p", TaskState::Cancelled)).status,
            TaskStatus::Cancelled
        );
    }

    // ── 日志增量提取（P1-7） ───────────────────────────────────────────────

    #[test]
    fn log_diff_appends_and_handles_eviction() {
        // 纯追加
        let prev: Vec<String> = vec!["a", "b"].into_iter().map(String::from).collect();
        let cur: Vec<String> = vec!["a", "b", "c", "d"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(new_log_lines(&prev, &cur), vec!["c", "d"]);

        // 头部弹出（缓冲区上限）：b 被弹出，新增 e
        let prev: Vec<String> = vec!["a", "b", "c", "d"]
            .into_iter()
            .map(String::from)
            .collect();
        let cur: Vec<String> = vec!["b", "c", "d", "e"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(new_log_lines(&prev, &cur), vec!["e"]);

        // 重启后缓冲区清空重建：全部为新行
        let empty: Vec<String> = Vec::new();
        assert_eq!(new_log_lines(&empty, &cur), cur);

        // 无变化
        assert!(new_log_lines(&cur, &cur).is_empty());
    }

    // ── 调度器接线（P1-1：backends 过滤 + overcommit + CPU 保底） ──────────

    fn cuda_device(index: u32, total_mb: u32) -> ComputeDevice {
        ComputeDevice {
            id: DeviceId::Cuda(index),
            backend: ComputeBackend::Cuda,
            name: format!("GPU-{index}"),
            total_memory_mb: Some(total_mb),
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        }
    }

    fn cpu_device() -> ComputeDevice {
        ComputeDevice {
            id: DeviceId::Cpu,
            backend: ComputeBackend::Cpu,
            name: "CPU".to_string(),
            total_memory_mb: None,
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        }
    }

    fn manifest_with_backends(id: &str, backends: Vec<ComputeBackend>) -> ModuleManifest {
        let backends_str = backends
            .iter()
            .map(|b| format!("\"{b}\""))
            .collect::<Vec<_>>()
            .join(", ");
        toml::from_str(&format!(
            r#"
[module]
id = "{id}"
name = "t"
version = "0.1.0"
description = "t"
category = "asr"
genre = "test"

[runtime]
type = "native"
binaries = {{ "x" = "x" }}

[compute]
backends = [{backends_str}]

[interface]
type = "http"
"#
        ))
        .unwrap()
    }

    #[test]
    fn scheduler_assigns_accelerator_with_least_memory_and_cpu_fallback() {
        let devices = vec![cuda_device(0, 4096), cuda_device(1, 8192), cpu_device()];
        let config = AppConfig::default(); // least_memory + allow_overcommit=true
        let scheduler = make_scheduler(&devices, &config);

        // cuda+cpu：加速后端优先，least_memory 选剩余显存最大的 cuda:1
        let mf = manifest_with_backends(
            "mod-a",
            vec![ComputeBackend::Cuda, ComputeBackend::Cpu],
        );
        assert_eq!(
            assign_module_device(&scheduler, &mf, &config),
            Some(DeviceId::Cuda(1))
        );

        // 纯 CPU 模块 → CPU
        let mf_cpu = manifest_with_backends("mod-b", vec![ComputeBackend::Cpu]);
        assert_eq!(
            assign_module_device(&scheduler, &mf_cpu, &config),
            Some(DeviceId::Cpu)
        );

        // 仅声明 rocm（本机无 rocm 设备、无 cpu 声明）→ None
        let mf_rocm = manifest_with_backends("mod-c", vec![ComputeBackend::Rocm]);
        assert_eq!(assign_module_device(&scheduler, &mf_rocm, &config), None);
    }

    #[test]
    fn scheduler_overcommit_gate_controls_fallback() {
        let devices = vec![cuda_device(0, 1024), cpu_device()];
        let mut config = AppConfig::default();
        config.compute.allow_overcommit = false;
        let scheduler = make_scheduler(&devices, &config);

        // 请求显存超限且未开超分 → 调度器拒绝 → CPU 保底（manifest 声明了 cpu）
        let mut mf = manifest_with_backends(
            "mod-big",
            vec![ComputeBackend::Cuda, ComputeBackend::Cpu],
        );
        mf.compute.vram_estimate_mb = Some(8000);
        assert_eq!(
            assign_module_device(&scheduler, &mf, &config),
            Some(DeviceId::Cpu)
        );

        // 开启超分 → 放行 cuda:0
        let mut config_oc = AppConfig::default();
        config_oc.compute.allow_overcommit = true;
        let scheduler_oc = make_scheduler(&devices, &config_oc);
        assert_eq!(
            assign_module_device(&scheduler_oc, &mf, &config_oc),
            Some(DeviceId::Cuda(0))
        );
    }

    // ── 注册表/工作区助手 ──────────────────────────────────────────────────

    #[test]
    fn unique_task_id_is_distinct() {
        let a = unique_task_id();
        let b = unique_task_id();
        assert_ne!(a, b);
        assert!(a.starts_with("task-"));
    }

    #[test]
    fn workspace_dir_resolves_relative_and_absolute() {
        let root = PathBuf::from("/app/root");
        let mut config = AppConfig::default();
        config.pipeline.workspace_dir = "workspace".to_string();
        assert_eq!(
            workspace_dir(&root, &config),
            PathBuf::from("/app/root/workspace")
        );
        let abs = if cfg!(windows) {
            "C:\\data\\ws"
        } else {
            "/data/ws"
        };
        config.pipeline.workspace_dir = abs.to_string();
        assert_eq!(workspace_dir(&root, &config), PathBuf::from(abs));
    }

    // ── 协调记录 #47：导出模块（组装/打包/清单渲染） ──────────────────────

    fn export_test_root(tag: &str) -> PathBuf {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-desktop-export-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// 单变体模块 manifest 夹具（qualified_id 已声明，可入包）
    fn export_fixture_manifest() -> ModuleManifest {
        toml::from_str(
            r#"
[module]
id = "mod-x"
name = "X"
version = "0.1.0"
description = "d"
category = "asr"
genre = "g"

[runtime]
type = "python"
python_version = ">=3.10"

[compute]
backends = ["cpu"]

[[models]]
id = "small"
name = "Small"
source = "huggingface"
repo_id = "org/small"
target_dir = "small-dir"
qualified_id = "test.vendor.model"

[interface]
type = "http"
"#,
        )
        .unwrap()
    }

    fn export_models_cfg() -> ep_core::config::ModelsConfig {
        ep_core::config::ModelsConfig {
            cache_dir: "models".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn render_pack_manifest_roundtrip() {
        use ep_pack::manifest::{ModelMode, PackManifest, PackModelEntry, PackPipelineRef};
        let manifest = PackManifest {
            pack: ep_pack::manifest::PackInfo {
                id: "test.pack".into(),
                version: "1.0.0".into(),
                name: "Test \"Pack\"\n".into(), // 覆盖转义路径
                description: "d".into(),
                authors: vec!["a".into()],
                license: None,
                homepage: None,
                min_ep_version: None,
                tags: vec!["t1".into()],
            },
            compute: ep_pack::manifest::PackCompute {
                backends: vec![ComputeBackend::Cpu],
                notes: HashMap::new(),
            },
            models: vec![PackModelEntry {
                qualified_id: "test.vendor.model".into(),
                variant: "small".into(),
                mode: ModelMode::Bundle,
                tags: vec![],
            }],
            pipelines: vec![PackPipelineRef {
                file: "pipelines/p.toml".into(),
            }],
        };
        let rendered = render_pack_manifest(&manifest);
        let dir = export_test_root("manifest-roundtrip");
        let path = dir.join("ep-pack.toml");
        std::fs::write(&path, &rendered).unwrap();
        let parsed = PackManifest::from_file(&path).unwrap();
        assert_eq!(parsed, manifest, "rendered TOML must roundtrip losslessly");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_dir_hardlink_preferred_preserves_tree() {
        let dir = export_test_root("hardlink-copy");
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("a.bin"), b"alpha").unwrap();
        std::fs::write(src.join("nested").join("b.bin"), b"beta").unwrap();
        let dst = dir.join("dst");
        std::fs::create_dir_all(&dst).unwrap();

        copy_dir_hardlink_preferred(&src, &dst).unwrap();
        assert_eq!(std::fs::read(dst.join("a.bin")).unwrap(), b"alpha");
        assert_eq!(std::fs::read(dst.join("nested").join("b.bin")).unwrap(), b"beta");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_export_pack_bundle_e2e() {
        let root = export_test_root("bundle-e2e");
        // 权重落盘（bundle 要求目录存在）
        let model_dir = root.join("models").join("small-dir");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("weights.bin"), b"weights-data").unwrap();

        let spec = ep_desktop::app::PackExportSpec {
            modules: vec![ep_desktop::app::PackExportModule {
                module_id: "mod-x".into(),
                bundle: true,
                variants: vec!["small".into()],
            }],
            pipelines: vec![],
            id: "test.pack".into(),
            name: "Test Pack".into(),
            version: "1.0.0".into(),
            output_dir: root.join("out"),
        };
        let manifests = vec![export_fixture_manifest()];
        let (archive, files) = build_export_pack(
            &root,
            "staging",
            &export_models_cfg(),
            &manifests,
            spec,
        )
        .unwrap();
        assert!(archive.is_file(), "archive must exist");
        assert_eq!(archive.file_name().unwrap(), "test.pack-1.0.0.epzip");
        // ep-pack.toml + models/small-dir/weights.bin + CHECKSUMS.toml
        assert_eq!(files, 3);
        // 暂存目录已清理
        let leftover: Vec<_> = std::fs::read_dir(root.join("staging"))
            .map(|rd| rd.flatten().collect())
            .unwrap_or_default();
        assert!(leftover.is_empty(), "staging must be cleaned after build");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_export_pack_bundle_requires_weights() {
        let root = export_test_root("bundle-missing");
        // 不落盘权重 → bundle 必须报错
        let spec = ep_desktop::app::PackExportSpec {
            modules: vec![ep_desktop::app::PackExportModule {
                module_id: "mod-x".into(),
                bundle: true,
                variants: vec!["small".into()],
            }],
            pipelines: vec![],
            id: "test.pack".into(),
            name: String::new(),
            version: String::new(),
            output_dir: root.join("out"),
        };
        let manifests = vec![export_fixture_manifest()];
        let err = build_export_pack(&root, "staging", &export_models_cfg(), &manifests, spec)
            .unwrap_err();
        assert!(err.contains("missing"), "error must mention missing weights: {err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_export_pack_reference_without_weights_ok() {
        let root = export_test_root("reference-only");
        // reference 模式无需权重落盘
        let spec = ep_desktop::app::PackExportSpec {
            modules: vec![ep_desktop::app::PackExportModule {
                module_id: "mod-x".into(),
                bundle: false,
                variants: vec!["small".into()],
            }],
            pipelines: vec![],
            id: String::new(), // 自动生成 local.build-<stamp>
            name: String::new(),
            version: String::new(),
            output_dir: root.join("out"),
        };
        let manifests = vec![export_fixture_manifest()];
        let (archive, files) = build_export_pack(
            &root,
            "staging",
            &export_models_cfg(),
            &manifests,
            spec,
        )
        .unwrap();
        assert!(archive.is_file());
        // ep-pack.toml + CHECKSUMS.toml（无权重、无管线）
        assert_eq!(files, 2);
        let name = archive.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("local.build-"), "auto id expected: {name}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn trfb_bg_falls_back_and_interpolates() {
        // 已落盘键 → 译文（desktopApp.error.packImportFailed 已落盘 zh-CN）
        let hit = trfb_bg("zh-CN", "common.action.save", "兜底", &[]);
        assert_eq!(hit, "保存");
        // 未落盘键 → 兜底文案 + 插值
        let miss = trfb_bg(
            "zh-CN",
            "desktopApp.notYetLandedKey47",
            "共 {{count}} 个",
            &[("count", "3")],
        );
        assert_eq!(miss, "共 3 个");
    }
}
