mod api;
pub(crate) mod eventlog;
mod logging;
mod schedule;
mod state;
mod updates;
pub(crate) mod watcher;
mod ws;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::ConnectInfo;
use axum::response::IntoResponse;
use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

use ep_core::compute::detect_all_devices;
use ep_core::config::{self, AppConfig};
use ep_core::deps::DepReport;
use ep_core::module::discovery::discover_modules;
use ep_core::port::PortManager;
use ep_core::process::ProcessManager;
use tracing::{info, warn};
use ep_core::types::ServiceStatus;

use crate::state::AppState;

// ─── Windows 错误弹窗根治（0xc0000142 静默降级） ───────────────────────
//
// daemon 拉起 python/uv 探测时，若解释器 DLL 初始化失败
//（STATUS_DLL_INIT_FAILED = 0xc0000142），Windows 默认会弹「应用程序无法正常
// 启动」系统错误对话框。`SetErrorMode` 的错误模式会被子进程继承，在启动
// 早期置位后，探测失败仅返回非零退出码 / spawn 错误，由调用侧降级分支
// 静默处理（仅日志 + 友好状态），不再弹窗。
//（自桌面端移植，2026-08-13 退役）
#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
extern "system" {
    fn SetErrorMode(u_mode: u32) -> u32;
}

/// 抑制本进程及子进程的严重错误对话框（仅 Windows；其他平台 no-op）。
#[cfg(target_os = "windows")]
fn suppress_error_dialogs() {
    const SEM_FAILCRITICALERRORS: u32 = 0x0001;
    const SEM_NOGPFAULTERRORBOX: u32 = 0x0002;
    const SEM_NOOPENFILEERRORBOX: u32 = 0x8000;
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
    }
}

#[cfg(not(target_os = "windows"))]
fn suppress_error_dialogs() {}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 在任何子进程拉起前尽早置位（server 与 --run-module 双入口共用），
    // 确保探测/模块子进程继承无弹错误模式
    suppress_error_dialogs();

    // Parse CLI args
    let args: Vec<String> = std::env::args().collect();
    let module_pos = args.iter().position(|a| a == "--run-module");

    // ── P2-1/P1-10：配置加载先于 tracing 初始化 ─────────────────────────
    // `general.log_level` 决定日志过滤器（RUST_LOG 仍优先覆盖，见
    // logging::build_env_filter）；加载失败时按默认级别初始化，
    // 各模式沿用迁移前的错误容忍度（server 模式 fail-fast / standalone 容忍）。
    let root = config::resolve_root();
    let load_result = AppConfig::load_or_create(&root.join("config"));
    let log_level = load_result
        .as_ref()
        .map(|c| c.general.log_level.clone())
        .unwrap_or_else(|_| AppConfig::default().general.log_level);
    logging::init_tracing(&log_level);

    if let Some(pos) = module_pos {
        let module_id = args
            .get(pos + 1)
            .ok_or_else(|| anyhow::anyhow!("--run-module requires a module ID"))?;
        // standalone 模式容忍配置加载失败（与迁移前 unwrap_or_default 语义一致）
        let mut cfg = load_result.unwrap_or_default();
        cfg.resolve_paths(&root);
        return run_module_standalone(module_id, root, cfg).await;
    }

    let mut cfg = load_result?;
    cfg.resolve_paths(&root);
    run_server(root, cfg).await
}

/// Standalone module runner — start a single module and keep it running.
///
/// `root` / `cfg` 由 main() 在 tracing 初始化前统一加载（P2-1 接线）。
async fn run_module_standalone(module_id: &str, root: PathBuf, cfg: AppConfig) -> anyhow::Result<()> {
    tracing::info!("Standalone mode: running module '{}'", module_id);

    // Discover modules
    let modules_dir = root.join("modules");
    let modules = discover_modules(&modules_dir);

    // Find the target module
    let discovered = modules
        .iter()
        .find(|m| {
            m.manifest
                .as_ref()
                .map(|mf| mf.module.id == module_id)
                .unwrap_or(false)
        })
        .ok_or_else(|| anyhow::anyhow!("Module '{}' not found in modules/", module_id))?;

    let manifest = discovered
        .manifest
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Module '{}' has invalid manifest", module_id))?;

    tracing::info!(
        "Found module: {} v{} ({})",
        manifest.module.name,
        manifest.module.version,
        manifest.module.category
    );

    // Detect devices and pick the best one（D-4 调度器接线：替代旧 first-match，
    // 经 ep-core 共享选择核心统一分配；无兼容设备时保留原 Cpu 兜底语义）
    let devices = detect_all_devices(&cfg.compute.disabled_backends);
    let device = ep_core::compute::scheduler::select_device_for_module(
        &devices,
        manifest,
        ep_core::compute::scheduler::module_vram_request(&cfg, manifest),
        ep_core::compute::scheduler::scheduling_strategy_for(&cfg),
        cfg.compute.allow_overcommit,
        &cfg.compute.disabled_backends,
    )
    .unwrap_or(ep_core::types::DeviceId::Cpu);

    tracing::info!("Using device: {}", device);

    // Allocate port
    let (port_start, port_end) = cfg.port_range();
    let mut port_manager = PortManager::new(port_start, port_end);
    let port = port_manager.allocate(module_id)?;
    tracing::info!("Allocated port: {}", port);

    // Build environment variables (A2/P0-3 修复：公共构建函数产出裸占位符键
    // —— ROOT/MODULE_DIR/MODEL_DIR/WORKSPACE/MODULE_ID/MODEL_ID/LOG_LEVEL/DEVICE...
    // EP_ 前缀由 process.rs 统一加一次，{MODULE_DIR}/{venv_python} 占位符替换生效)
    let env_vars = ep_core::process::build_module_env(&root, &cfg, module_id, manifest, &device);

    // Start the module（§3.1：注入共享 CUDA 库目录，平台分支在 process.rs 内部）
    let cuda_libs_dir =
        ep_core::process::resolve_cuda_libs_dir(&root, &cfg.compute.cuda_libs_dir);
    let mut process_manager = ProcessManager::new().with_cuda_libs_dir(cuda_libs_dir);
    process_manager
        .start_module(module_id, manifest, device.clone(), port, env_vars)
        .await?;

    tracing::info!(
        "Module '{}' started on port {} (device: {})",
        module_id,
        port,
        device
    );
    tracing::info!("Press Ctrl+C to stop");

    // Wait for shutdown signal（LNX-01：Ctrl+C / SIGTERM 均触发优雅回收，
    // standalone 路径与 server 路径共用同一信号处理）
    shutdown_signal().await;

    tracing::info!("Shutting down module '{}'...", module_id);
    process_manager.stop_module(module_id).await?;
    port_manager.release(module_id);
    tracing::info!("Module '{}' stopped", module_id);

    Ok(())
}

/// Normal HTTP server mode.
///
/// `root` / `cfg` 由 main() 在 tracing 初始化前统一加载（P2-1 接线：
/// `general.log_level` 决定 subscriber 过滤规则）。
async fn run_server(root: PathBuf, cfg: AppConfig) -> anyhow::Result<()> {
    tracing::info!("EntryPoint Daemon starting...");

    // 1. root 与配置已在 main() 中先行加载（tracing 初始化之前，P2-1）
    tracing::info!(root = %root.display(), "project root resolved");
    tracing::info!(
        log_level = %cfg.general.log_level,
        check_updates = cfg.general.check_updates,
        "configuration loaded"
    );

    // 2. Check and auto-install missing system dependencies
    {
        let results = DepReport::check_and_install_missing(&root);
        for (dep, result) in &results {
            match result {
                ep_core::deps_install::InstallResult::Installed => {
                    tracing::info!(dep = ?dep, "dependency auto-installed");
                }
                ep_core::deps_install::InstallResult::AlreadyPresent => {}
                ep_core::deps_install::InstallResult::Failed(msg) => {
                    tracing::warn!(dep = ?dep, msg = %msg, "dependency auto-install failed");
                }
                ep_core::deps_install::InstallResult::ManualRequired(msg) => {
                    tracing::warn!(dep = ?dep, msg = %msg, "manual installation required");
                }
            }
        }
    }

    // 3. Detect compute devices
    let devices = detect_all_devices(&cfg.compute.disabled_backends);
    tracing::info!(count = devices.len(), "Compute devices detected");

    // 4. Discover modules
    let modules_dir = root.join("modules");
    let modules = discover_modules(&modules_dir);
    tracing::info!(count = modules.len(), "Modules discovered");

    // 5. Create managers
    let (port_start, port_end) = cfg.port_range();
    let port_manager = PortManager::new(port_start, port_end);

    // 6. Build AppState (creates broadcast channels internally)
    let state = Arc::new(AppState::new(root.clone(), cfg, devices, modules, port_manager));

    // 7. Log public access setting
    {
        let cfg = state.config.read().await;
        if cfg.server.allow_public {
            tracing::warn!("public access ENABLED — no built-in auth/encryption, use at your own risk");
            // 公网暴露 + （v1 未启用鉴权或未配 token）→ 未认证推理 API 直接可达，明确告警
            if !cfg.api.enabled || cfg.api.token.is_none() {
                tracing::warn!(
                    "public access ENABLED without [api].token — /api/v1/* inference endpoints are exposed without authentication; set [api].token in config/app.toml"
                );
            }
        } else {
            tracing::info!("public access blocked — only private/loopback IPs allowed (set allow_public=true to change)");
        }
    }

    // 8. Build router with SPA fallback (absolute path)
    // Packaged layout: <root>/webui; dev layout: <root>/crates/ep-webui/static
    let static_dir = if root.join("webui").is_dir() {
        root.join("webui")
    } else {
        root.join("crates").join("ep-webui").join("static")
    };
    let app = build_app_router(state.clone(), &static_dir);

    // 9. Spawn background monitor loop (H1 log capture + H2 health check polling)
    //    tick 1s；日志按快照后缀去重，只广播新增行（策略见 diff_new_lines）。
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            // module_id → 上一 tick 的日志快照（广播去重游标）
            let mut last_snapshots: HashMap<String, Vec<String>> = HashMap::new();

            loop {
                interval.tick().await;

                // Collect module IDs that have active process instances
                let module_ids: Vec<String> = {
                    let pm = state.process_manager.read().await;
                    pm.list_running()
                        .iter()
                        .map(|inst| inst.module_id.clone())
                        .collect()
                };

                // 模块消失（停止/退出）时清理对应游标
                last_snapshots.retain(|mid, _| module_ids.iter().any(|m| m == mid));

                for mid in &module_ids {
                    // P2 修复：先取读锁快照判断状态，避免写锁跨网络探测。
                    // monitor_process 对 Starting 实例会做 /health 网络探测
                    // （单次最长 ~1s），持写锁期间全部需要写锁的 handler
                    // （start/stop/autostart）都会被阻塞；仅状态迁移需要探测。
                    let needs_probe = {
                        let pm = state.process_manager.read().await;
                        matches!(
                            pm.get_instance(mid).map(|i| &i.status),
                            Some(ServiceStatus::Starting)
                        )
                    };
                    // 取当前日志快照（monitor_process 会先 poll_logs 填充缓冲）
                    let snapshot: Vec<String> = if needs_probe {
                        // 状态迁移路径：持写锁执行健康探测（唯一跨网络 await 的
                        // 写锁临界区，探测预算由 ep-core monitor 内部约束）
                        let mut pm = state.process_manager.write().await;
                        let _ = pm.monitor_process(mid).await;
                        pm.get_instance(mid)
                            .map(|inst| inst.log_buffer.iter().cloned().collect())
                            .unwrap_or_default()
                    } else {
                        // 非 Starting（Running/Error/Stopped）：monitor_process
                        // 无网络路径（try_wait + poll_logs 均同步快速），写锁
                        // 临界区极短，不跨网络 await
                        let mut pm = state.process_manager.write().await;
                        let _ = pm.monitor_process(mid).await;
                        pm.get_instance(mid)
                            .map(|inst| inst.log_buffer.iter().cloned().collect())
                            .unwrap_or_default()
                    };

                    // 与上一快照比对，只广播新增行（每行一条 LogMessage）
                    let last = last_snapshots.entry(mid.clone()).or_default();
                    for line in diff_new_lines(last, &snapshot) {
                        let _ = state.log_tx.send(crate::state::LogMessage {
                            module_id: mid.clone(),
                            line,
                        });
                    }
                    *last = snapshot;
                }
            }
        });
    }

    // 10. Spawn device refresh task — 周期性重新探测计算设备，更新 state.devices。
    //     detect_all_devices 内部用阻塞的 std::process::Command 调 nvidia-smi 等，
    //     故放入 spawn_blocking 执行，避免阻塞 async 运行时。
    //     间隔取启动时 config.compute.refresh_interval_secs（不热跟随变更）。
    {
        let state = state.clone();
        tokio::spawn(async move {
            let interval_secs = {
                let cfg = state.config.read().await;
                u64::from(cfg.compute.refresh_interval_secs.max(1))
            };
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            interval.tick().await; // 首个 tick 立即返回，跳过以避免与启动探测重复
            loop {
                interval.tick().await;
                let disabled = {
                    let cfg = state.config.read().await;
                    cfg.compute.disabled_backends.clone()
                };
                match tokio::task::spawn_blocking(move || detect_all_devices(&disabled)).await {
                    Ok(devices) => {
                        tracing::debug!(count = devices.len(), "devices refreshed");
                        *state.devices.write().await = devices;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "device refresh task failed");
                    }
                }
            }
        });
    }

    // 10.5 Spawn 模块空闲回收任务（按需加载 + 定期释放）：周期巡检运行中
    //      模块，持续无任务触达超过 [modules].idle_timeout_secs 即自动停止，
    //      释放模型内存/显存与功耗；下次任务经既有拉起路径按需重载。
    //      超时实时读取——运行期经 PUT /api/config 改动即时生效（0=停用）。
    //      活跃守卫：被排队/运行中任务引用的模块一律豁免（TaskRecord.module_ids）。
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(30));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let timeout_secs = {
                    let cfg = state.config.read().await;
                    cfg.modules.idle_timeout_secs
                };
                if timeout_secs == 0 {
                    continue;
                }
                let now = chrono::Utc::now().timestamp();

                // 候选：status=Running 的实例，基准 = max(started_at, 最近触达)
                let running: Vec<(String, i64)> = {
                    let pm = state.process_manager.read().await;
                    pm.list_running()
                        .into_iter()
                        .map(|i| {
                            (
                                i.module_id.clone(),
                                i.started_at.map(|t| t.timestamp()).unwrap_or(0),
                            )
                        })
                        .collect()
                };
                if running.is_empty() {
                    continue;
                }
                let baselines: Vec<(String, i64)> = {
                    let activity =
                        state.module_activity.lock().unwrap_or_else(|p| p.into_inner());
                    running
                        .iter()
                        .map(|(id, started)| (id.clone(), *activity.get(id).unwrap_or(started)))
                        .collect()
                };

                let victims =
                    ep_core::module::lifecycle::idle_release_candidates(baselines, timeout_secs, now);
                if victims.is_empty() {
                    continue;
                }
                // 活跃守卫：排队/运行中任务引用的模块不释放
                let busy: std::collections::HashSet<String> =
                    crate::api::execute::execution::active_task_module_ids();

                for id in victims {
                    if busy.contains(&id) {
                        tracing::debug!(module_id = %id, "idle 回收跳过：存在活跃任务引用");
                        continue;
                    }
                    info!(module_id = %id, timeout_secs, "模块空闲超时，自动释放（下次任务按需重载）");
                    let mut pm = state.process_manager.write().await;
                    if let Err(e) = pm.stop_module(&id).await {
                        tracing::warn!(module_id = %id, error = %e, "idle 自动释放失败");
                        continue;
                    }
                    drop(pm);
                    state.port_manager.write().await.release(&id);
                }
            }
        });
    }

    // 10.6 Spawn 管线定时调度巡检（cron）：每 30s 对 runtime/schedules.json
    //      求触发窗口；命中即按存储的 inputs/params 模板提交执行。
    //      last_checked 水位线持久化——重启不补跑错过的窗口，也不双跑。
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(30));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let path = schedule::default_registry_path(&state.root);
                let registry = schedule::ScheduleRegistry::load(&path);
                if registry.entries.is_empty() {
                    continue;
                }
                let now = chrono::Utc::now().timestamp();
                // 活跃管线集：闸门在跑/排队的任务所属管线
                let active: std::collections::HashSet<String> = {
                    let reg = crate::api::execute::execution::task_registry_all();
                    reg.into_iter()
                        .filter(|r| r.is_active())
                        .map(|r| r.pipeline_id)
                        .collect()
                };
                let (due, next) = schedule::collect_due(&registry, now, &active);
                if due.is_empty() {
                    if next.entries.len() == registry.entries.len()
                        && next
                            .entries
                            .iter()
                            .all(|(k, v)| registry.entries.get(k).map(|o| o.last_checked == v.last_checked).unwrap_or(false))
                    {
                        continue; // 无状态变化不落盘
                    }
                    if let Err(e) = next.save(&path) {
                        tracing::warn!(error = %e, "schedules.json 保存失败");
                    }
                    continue;
                }
                for trigger in &due {
                    info!(pipeline_id = %trigger.pipeline_id, cron = %trigger.entry.cron, "schedule 触发管线执行");
                    let inputs: std::collections::HashMap<String, serde_json::Value> =
                        serde_json::from_value(trigger.entry.inputs.clone()).unwrap_or_default();
                    match crate::api::execute::execution::submit_pipeline_for_schedule(
                        &state,
                        &trigger.pipeline_id,
                        inputs,
                        trigger.entry.params.clone(),
                    ).await {
                        Ok(task_id) => {
                            tracing::info!(%task_id, pipeline_id = %trigger.pipeline_id, "schedule 任务已提交");
                            let mut reg2 = schedule::ScheduleRegistry::load(&path);
                            if let Some(e) = reg2.entries.get_mut(&trigger.pipeline_id) {
                                e.last_task_id = Some(task_id);
                                e.last_checked = now;
                            }
                            let _ = reg2.save(&path);
                        }
                        Err(e) => {
                            warn!(pipeline_id = %trigger.pipeline_id, error = %e, "schedule 触发提交失败");
                        }
                    }
                }
                // 兜底持久化最终水位线
                let _ = next.save(&path);
            }
        });
    }

    // 10.7 Spawn 触发器巡检循环（PLAN_TRIGGER_UNIFIED_LOG §5.2/§5.4，W1-E）：
    //      每 10s 加载 runtime/watchers.json → collect_watch_events（纯函数：
    //      水位线 + 在途稳定表，§5.2）→ 命中文件按规则模式提交：
    //      - 管线模式：复用 submit_pipeline_for_schedule（零改动既有入口），
    //        inputs = {input_node: {"path": 绝对路径}}。不传 `_meta` 保留键——
    //        既有 apply_inputs 将 inputs 键一律按节点 id 解析（execution.rs
    //        直调物化区段偏差声明），规则名注入仅直调物化期预渲染承担；
    //      - 直调-仅归档 / 直调-模块：execution.rs 直调物化区段
    //        submit_direct_archive / submit_direct_module（WatchArchiveOutput
    //        按字段自规则 OutputConfig 映射，见 watcher_output）。
    //      提交成功 → 写 watcher_trigger(submitted) 事件 + 回写 recent（环形 5）/
    //      last_task_id（checkpoint / in_flight 已由 collect_watch_events 在新表
    //      中推进）；QueueFull 等失败 → 写 rejected 事件 + **整轮放弃新注册表**
    //      （触发文件保持待触发，沿用旧表下轮重试——W0-C 模块文档既定的
    //      QueueFull 语义，不丢文件；collect 为纯函数，放弃新表即天然回滚）。
    //      无状态变化不落盘。与 cron 巡检同为读-改-写窗口（§9 风险表既定，
    //      单进程 10s 周期，不引入新锁）。
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(10));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let path = watcher::default_registry_path(&state.root);
                let registry = watcher::WatchRegistry::load(&path);
                if registry.rules.is_empty() {
                    continue;
                }
                let now = chrono::Utc::now().timestamp();
                let (hits, mut next) = watcher::collect_watch_events(&registry, now);
                if hits.is_empty() {
                    // 无状态变化不落盘（与 cron 巡检同款守卫）
                    if !watcher_registry_changed(&registry, &next) {
                        continue;
                    }
                    if let Err(e) = next.save(&path) {
                        warn!(error = %e, "watchers.json 保存失败");
                    }
                    continue;
                }
                // 逐 hit 提交；任一失败 → abandoned：本轮**已成功提交**的 hit
                // 状态推进必须合并保留后落盘（评审修复——否则旧表中该文件仍在
                // in_flight 且签名一致，下轮 stable_ready 会重复提交同一文件）；
                // 未提交 / 未尝试的 hit 沿用旧注册表（文件保持待触发，下轮
                // 重试——W0-C 模块文档既定的 QueueFull 语义，不丢文件）。
                let mut abandoned = false;
                let mut outcomes: HashMap<String, RuleSubmitOutcome> = HashMap::new();
                for hit in &hits {
                    let rule = &hit.rule;
                    let submit = if let Some(p) = &rule.pipeline {
                        let mut inputs = HashMap::new();
                        inputs.insert(
                            p.input_node.clone(),
                            serde_json::json!({ "path": hit.path }),
                        );
                        crate::api::execute::execution::submit_pipeline_for_schedule(
                            &state,
                            &p.pipeline_id,
                            inputs,
                            serde_json::Value::Null,
                        )
                        .await
                    } else if let Some(d) = &rule.direct {
                        let output = watcher_output(&rule.output);
                        match &d.kind {
                            watcher::DirectKind::Archive => {
                                crate::api::execute::execution::submit_direct_archive(
                                    &state,
                                    &hit.rule_id,
                                    &rule.name,
                                    std::path::PathBuf::from(&hit.path),
                                    &output,
                                )
                                .await
                                .map_err(|e| e.to_string())
                            }
                            watcher::DirectKind::Module { module_id, capability } => {
                                crate::api::execute::execution::submit_direct_module(
                                    &state,
                                    &hit.rule_id,
                                    &rule.name,
                                    module_id,
                                    capability,
                                    d.params.clone(),
                                    std::path::PathBuf::from(&hit.path),
                                    &output,
                                )
                                .await
                                .map_err(|e| e.to_string())
                            }
                        }
                    } else {
                        Err("rule has neither pipeline nor direct action".to_string())
                    };
                    match submit {
                        Ok(task_id) => {
                            info!(rule_id = %hit.rule_id, file = %hit.path, task_id = %task_id, "watcher 触发已提交");
                            eventlog::write_watcher_trigger(
                                &state.root,
                                &hit.rule_id,
                                &hit.path,
                                Some(&task_id),
                                "submitted",
                                None,
                            );
                            // recent（环形 5）/ last_task_id 回写进新表
                            if let Some(r) = next.rules.get_mut(&hit.rule_id) {
                                r.last_task_id = Some(task_id.clone());
                                r.recent.push_front(watcher::EventRecord {
                                    ts: now,
                                    file: hit.path.clone(),
                                    task_id: Some(task_id.clone()),
                                    status: "submitted".to_string(),
                                    detail: None,
                                });
                                r.recent.truncate(5);
                            }
                            // 直调模式：后台跟踪任务终态补写 archive_done /
                            // archive_skipped 事件（§5.4；见
                            // spawn_archive_outcome_watch 判别语义）
                            if rule.direct.is_some() {
                                spawn_archive_outcome_watch(
                                    state.root.clone(),
                                    hit.rule_id.clone(),
                                    hit.path.clone(),
                                    task_id,
                                );
                            }
                            // 记录该规则本轮有成功提交：hit 必来自旧表 in_flight
                            // 中签名一致的稳定条目，快照旧签名供 abandoned 合并
                            let out = outcomes.entry(hit.rule_id.clone()).or_default();
                            if let Some(sig) = registry
                                .rules
                                .get(&hit.rule_id)
                                .and_then(|r| r.in_flight.get(&hit.path))
                            {
                                out.submitted.push((hit.path.clone(), *sig));
                            }
                        }
                        Err(e) => {
                            warn!(rule_id = %hit.rule_id, file = %hit.path, error = %e, "watcher 触发提交失败，本轮放弃新注册表");
                            eventlog::write_watcher_trigger(
                                &state.root,
                                &hit.rule_id,
                                &hit.path,
                                None,
                                "rejected",
                                Some(&e),
                            );
                            // 失败规则的已提交 hit 走部分合并；失败 hit 的旧表
                            // mtime 作为该规则水位线推进的封顶（hits 按规则内
                            // mtime 升序，其后同规则未尝试 hit 的 mtime ≥ 它）
                            if let Some(out) = outcomes.get_mut(&hit.rule_id) {
                                out.all_submitted = false;
                                out.failed_mtime = registry
                                    .rules
                                    .get(&hit.rule_id)
                                    .and_then(|r| r.in_flight.get(&hit.path))
                                    .map(|s| s.mtime);
                            }
                            abandoned = true;
                            break;
                        }
                    }
                }
                if abandoned {
                    // 合并已成功提交 hit 的状态推进后落盘；无变化不落盘
                    //（同款守卫：全失败时 merged == 旧表，不写盘）
                    let merged = merge_abandoned_registry(&registry, &next, &outcomes);
                    if watcher_registry_changed(&registry, &merged) {
                        if let Err(e) = merged.save(&path) {
                            warn!(error = %e, "watchers.json 保存失败");
                        }
                    }
                    continue;
                }
                if let Err(e) = next.save(&path) {
                    warn!(error = %e, "watchers.json 保存失败");
                }
            }
        });
    }

    // 10.8 Spawn 日志清理巡检（PLAN_TRIGGER_UNIFIED_LOG §5.7 / §4.2）：每 1h
    //      按 general.log_retention_days 删除 runtime/logs/ 下 mtime 超期的
    //      events-*.jsonl 与模块日志文件（复用 eventlog::cleanup_expired_logs）；
    //      0 = 永久保留（清理函数内部直接跳过）。保留天数实时读取——运行期经
    //      PUT /api/config 改动即时生效，无需重启。
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(3600));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let retention_days = {
                    let cfg = state.config.read().await;
                    cfg.general.log_retention_days
                };
                let removed = eventlog::cleanup_expired_logs(&state.root, retention_days);
                if removed > 0 {
                    info!(removed, retention_days, "日志保留期巡检清理完成");
                }
            }
        });
    }

    // 11. Spawn 后台模型更新自动检查（P1-10：general.check_updates 接线）。
    //     开关每轮实时读取——运行期经 PUT /api/config 改动即时生效，无需重启。
    updates::spawn_auto_update_checker(state.clone());

    // 12. Start server
    let cfg = state.config.read().await;
    let addr: SocketAddr = format!("{}:{}", cfg.server.host, cfg.server.port)
        .parse()
        .unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], 9800)));
    drop(cfg);
    tracing::info!("Listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    // 优雅退出：回收所有运行中的模块子进程（stop_module 会 kill 子进程并
    // 释放端口）。已知限制修复：daemon 重启不再留下孤儿进程占用端口。
    stop_all_modules(&state).await;

    tracing::info!("Daemon shut down gracefully");
    Ok(())
}

/// 停止所有运行中的模块子进程（优雅退出时的回收）。
/// 逐个 stop_module（内部 kill 子进程 + 释放端口），单个失败仅告警不阻断。
async fn stop_all_modules(state: &Arc<AppState>) {
    let running: Vec<String> = {
        let pm = state.process_manager.read().await;
        pm.list_running()
            .iter()
            .map(|inst| inst.module_id.clone())
            .collect()
    };
    if running.is_empty() {
        tracing::info!("No running modules to stop");
        return;
    }
    tracing::info!(count = running.len(), "Stopping running modules…");
    for module_id in running {
        let mut pm = state.process_manager.write().await;
        match pm.stop_module(&module_id).await {
            Ok(()) => tracing::info!(module_id = %module_id, "module stopped on shutdown"),
            Err(e) => tracing::warn!(module_id = %module_id, error = %e, "failed to stop module on shutdown"),
        }
    }
}

/// 规则 `OutputConfig` → 直调物化 `WatchArchiveOutput` 的字段映射（W1-E ↔
/// W1-D 契约：watcher.rs 与 execution.rs 直调物化区段零类型耦合，此处按字段
/// 直传；`on_conflict` 枚举 → 小写字符串）。`output` 缺省按空目标/空模板
/// 构造——`watch_archive_params` 会快速失败（dest_dir required）→ rejected
/// 事件，防御绕过 API 校验的畸形规则。
fn watcher_output(
    output: &Option<watcher::OutputConfig>,
) -> crate::api::execute::execution::WatchArchiveOutput {
    let o = output.clone().unwrap_or(watcher::OutputConfig {
        dest_dir: String::new(),
        name_template: String::new(),
        on_conflict: watcher::ConflictPolicy::Suffix,
    });
    crate::api::execute::execution::WatchArchiveOutput {
        dest_dir: o.dest_dir,
        name_template: o.name_template,
        on_conflict: match o.on_conflict {
            watcher::ConflictPolicy::Suffix => "suffix",
            watcher::ConflictPolicy::Overwrite => "overwrite",
            watcher::ConflictPolicy::Skip => "skip",
        }
        .to_string(),
    }
}

/// 巡检后注册表是否有持久化可见的状态变化（checkpoint / 在途表逐规则比较；
/// `in_flight` 的 `PartialEq` 与键序无关）。命中为空时 recent / last_task_id
/// 必然未被触碰，二者即完整判据——无变化不落盘（与 cron 巡检同款守卫）。
fn watcher_registry_changed(a: &watcher::WatchRegistry, b: &watcher::WatchRegistry) -> bool {
    a.rules.len() != b.rules.len()
        || a.rules.iter().any(|(k, ra)| match b.rules.get(k) {
            Some(rb) => ra.checkpoint != rb.checkpoint || ra.in_flight != rb.in_flight,
            None => true,
        })
}

/// 单规则本轮提交结果（abandoned 路径合并 [`merge_abandoned_registry`] 的依据）
#[derive(Default)]
struct RuleSubmitOutcome {
    /// 该规则本轮所有 hit 均提交成功（可整体采纳新表状态）
    all_submitted: bool,
    /// 成功提交的 hit：(文件路径, 旧注册表在途签名)。stable hit 必来自旧表
    /// in_flight 签名一致的条目，签名内含 mtime 供水位线推进取值。
    submitted: Vec<(String, watcher::FileSig)>,
    /// 失败 hit 的旧表 mtime——水位线推进的封顶：不得越过未提交文件，
    /// 否则其 mtime 落入水位线之下被永久排除（丢文件，违反 D6）。
    failed_mtime: Option<i64>,
}

/// abandoned 路径的注册表合并（评审修复：部分提交失败不得丢弃已提交 hit 的
/// 状态推进，否则旧表中该文件仍在 `in_flight` 且签名一致，下轮
/// `stable_ready` 会**重复提交**同一文件）。
///
/// 逐规则三种情形：
/// - 全部 hit 提交成功：整体采纳新表规则状态（checkpoint / in_flight /
///   recent / last_task_id 连同本轮入表、过滤解决等推进一并保留，与正常
///   落盘路径等价）；
/// - 部分成功（含失败 hit 的规则）：以旧表为基做逐 hit 状态手术——仅移除
///   已提交文件的在途条目、水位线推进到已提交文件的最大 mtime（封顶于失败
///   hit 的 mtime 之前，保持未提交文件可考察可重试），recent / last_task_id
///   取自新表（仅成功 hit 会回写二者）。其余状态（本轮新入表、过滤解决）
///   沿用旧表，下轮重新巡检天然补齐，无丢失；
/// - 无成功提交：原样保留旧表（merged == 旧表 → 守卫判定无变化不落盘）。
///
/// 水位线取值安全论证：静默期未决文件 mtime > now − stability ≥ 任何 hit
/// mtime（stable hit 必过静默期），推进到 hit mtime 不会越过静默期文件；
/// 唯一理论边角是同轮内 mtime 相同秒的部分失败（封顶使其保持可重试，代价是
/// 同秒已提交文件可能被重新考察——重复提交优于静默丢文件，D6 取向）。
fn merge_abandoned_registry(
    old: &watcher::WatchRegistry,
    next: &watcher::WatchRegistry,
    outcomes: &HashMap<String, RuleSubmitOutcome>,
) -> watcher::WatchRegistry {
    let mut merged = old.clone();
    for (rule_id, out) in outcomes {
        if out.all_submitted {
            if let Some(nr) = next.rules.get(rule_id) {
                merged.rules.insert(rule_id.clone(), nr.clone());
            }
            continue;
        }
        // 部分成功：旧表为基，仅保留已提交 hit 的状态推进
        let (Some(old_rule), Some(next_rule)) =
            (old.rules.get(rule_id), next.rules.get(rule_id))
        else {
            continue;
        };
        let mut rule = old_rule.clone();
        rule.recent = next_rule.recent.clone();
        rule.last_task_id = next_rule.last_task_id.clone();
        let mut max_submitted_mtime: Option<i64> = None;
        for (path, sig) in &out.submitted {
            rule.in_flight.remove(path);
            max_submitted_mtime = Some(match max_submitted_mtime {
                Some(m) => m.max(sig.mtime),
                None => sig.mtime,
            });
        }
        if let Some(mx) = max_submitted_mtime {
            let cap = out.failed_mtime.map_or(i64::MAX, |m| m - 1);
            rule.checkpoint = rule.checkpoint.max(mx.min(cap));
        }
        merged.rules.insert(rule_id.clone(), rule);
    }
    merged
}

/// 直调触发任务的归档结果跟踪（§5.4：archive_done / archive_skipped 事件）。
///
/// 提交期只拿得到 task_id（任务入队即返回，D6），归档真实发生在后台执行——
/// 故 spawn 轻量跟踪：每秒轮询任务快照至终态（上限 1h），completed 时按
/// 归档产物 mtime 判别 skip 命中：skip 命中时 file_archive 节点**透传上游
/// 产物**（executor.rs `ConflictResolution::SkipHit` 分支，不复制落盘），
/// 产物 mtime 必早于任务提交时刻；真实归档为 `fs::copy`，mtime ≥ 提交时刻。
/// `file` 字段统一记原始触发文件，与 submitted / rejected 事件一致。
/// failed / cancelled 不重复写——执行链路 finalize 统一出口已写 task_terminal。
fn spawn_archive_outcome_watch(
    root: std::path::PathBuf,
    rule_id: String,
    watch_file: String,
    task_id: String,
) {
    tokio::spawn(async move {
        use crate::api::execute::execution::TaskState;
        for _ in 0..3600 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let Some(record) = crate::api::execute::execution::snapshot(&task_id) else {
                return; // 注册表中已无此任务（异常清理）：放弃跟踪
            };
            if !record.status.is_terminal() {
                continue;
            }
            if record.status != TaskState::Completed {
                return;
            }
            let Some(artifact) = record.artifacts.get("archive") else {
                tracing::debug!(%task_id, "watcher 直调任务完成但无 archive 产物，跳过归档事件");
                return;
            };
            let threshold = record
                .started_running_at
                .unwrap_or(record.started_at)
                .timestamp();
            let skipped = std::fs::metadata(artifact)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                // 产物 mtime 取证失败（文件已消失等）：按完成处理，不误报 skip
                .map(|mtime| mtime < threshold)
                .unwrap_or(false);
            eventlog::write_watcher_trigger(
                &root,
                &rule_id,
                &watch_file,
                Some(&task_id),
                if skipped { "archive_skipped" } else { "archive_done" },
                if skipped {
                    Some("目标已存在（冲突策略 skip），归档跳过")
                } else {
                    None
                },
            );
            return;
        }
        tracing::debug!(%task_id, "watcher 直调任务 1h 未达终态，停止归档结果跟踪");
    });
}

/// 构建完整路由（API + WebSocket + SPA 静态资源）。
///
/// SPA fallback 选用 `ServeDir::fallback`（而非 `not_found_service`）：
/// tower-http 0.6 中 `not_found_service` 会把 fallback 服务的响应状态码强制改为
/// 404（内部 `SetStatus::new(svc, 404)` 包装），导致深链得到 "404 + index.html"；
/// `fallback` 则原样保留内层服务的状态码，`ServeFile` 返回 200 + text/html，
/// 这正是 SPA 深链（如 /tasks/xxx）需要的语义。
fn build_app_router(state: Arc<AppState>, static_dir: &Path) -> Router {
    let index_path = static_dir.join("index.html");
    Router::new()
        .nest("/api", api::api_router(state.clone()))
        .merge(ws::ws_router())
        .fallback_service(ServeDir::new(static_dir).fallback(ServeFile::new(index_path)))
        .layer(axum::middleware::from_fn_with_state(state.clone(), ip_filter))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        // P2：请求总时长超时层（最外层，覆盖全部路由含 body 读取与响应）。
        // 大文件上传/产物流式下载长连接路径在中间件内豁免（见 request_timeout）。
        .layer(axum::middleware::from_fn(request_timeout))
        .with_state(state)
}

/// 请求总时长超时（秒）：非豁免路由的完整请求（含 body 读取 + handler + 响应）
/// 超时上限。头部读取由连接层负责，本中间件兜住慢 body / 慢 handler。
/// 上传/下载长连接豁免（见 [`is_timeout_exempt_path`]）。
const REQUEST_TOTAL_TIMEOUT_SECS: u64 = 300;

/// 长连接豁免路径（P2）：大文件 multipart 上传与产物流式下载可能持续数小时，
/// 不套总时长超时。
fn is_timeout_exempt_path(path: &str) -> bool {
    // 模型上传 /api/models/{module_id}/upload 与直跑输入上传 /api/upload/input
    if path.starts_with("/api/models/") && path.ends_with("/upload") {
        return true;
    }
    if path == "/api/upload/input" {
        return true;
    }
    // 模块标准档案导入（§2.3）：含原生二进制的模块包上传可能远超 300s 总时长
    if path == "/api/modules/import" {
        return true;
    }
    // v1 同步推理提交（wait=true）：模型加载 + 长推理可能远超 300s 总时长
    // 上限，任务级超时语义由管线引擎（空闲看门狗/节点硬超时）负责，此处
    // 不叠加；结果查询端点（/api/v1/inference/result/）为快速读，不豁免，
    // 恢复 300s 兜底（m8）
    if path.starts_with("/api/v1/inference/")
        && !path.starts_with("/api/v1/inference/result/")
    {
        return true;
    }
    // 产物流式下载（ServeDir）：/api/task-files/*、/api/pack-files/*
    path.starts_with("/api/task-files/") || path.starts_with("/api/pack-files/")
}

/// 请求总时长超时中间件：豁免路径直接放行；其余路径在
/// [`REQUEST_TOTAL_TIMEOUT_SECS`] 内未完成 → 408。
async fn request_timeout(
    request: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let uri = request.uri().path().to_string();
    if is_timeout_exempt_path(&uri) {
        return next.run(request).await;
    }
    match tokio::time::timeout(
        std::time::Duration::from_secs(REQUEST_TOTAL_TIMEOUT_SECS),
        next.run(request),
    )
    .await
    {
        Ok(resp) => resp,
        Err(_) => {
            tracing::warn!(uri = %uri, timeout_secs = REQUEST_TOTAL_TIMEOUT_SECS, "request exceeded total timeout");
            (
                axum::http::StatusCode::REQUEST_TIMEOUT,
                "request timed out",
            )
                .into_response()
        }
    }
}

/// 计算两次日志快照之间的新增行（后缀比对去重）。
///
/// 策略：从大到小寻找最长的 k，使 `last` 末尾 k 行与 `new` 开头 k 行完全相同，
/// 则 `new[k..]` 即本轮新增行：
/// - 正常追加：k == last.len()，只广播增量尾部（不丢行、不重复）；
/// - 缓冲回绕导致新快照更短（环形缓冲 500 行上限截断头部）：
///   只要仍有重叠后缀，依然只广播增量；
/// - 完全无重叠（模块重启、日志重置等极端情况）：k == 0，
///   整个 `new` 快照视为新增（宁可整批重播，也不静默丢日志）。
fn diff_new_lines(last: &[String], new: &[String]) -> Vec<String> {
    let max_overlap = last.len().min(new.len());
    for k in (0..=max_overlap).rev() {
        if last[last.len() - k..] == new[..k] {
            return new[k..].to_vec();
        }
    }
    // k == 0 必然命中，此处不可达
    new.to_vec()
}

/// 判断 IP 是否为私有/本地地址（RFC 1918 + loopback + link-local）
fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

/// IP 过滤中间件：allow_public = false 时拒绝非私有 IP
async fn ip_filter(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let allow_public = {
        let config = state.config.read().await;
        config.server.allow_public
    };
    if !allow_public && !is_private_ip(&addr.ip()) {
        tracing::warn!(ip = %addr.ip(), "blocked public access attempt");
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

/// 等待关闭信号（LNX-01）。
///
/// 所有平台支持 Ctrl+C；unix 下额外注册 SIGTERM——systemd stop 发送的是
/// SIGTERM，此前仅监听 ctrl_c 时默认处置为立即终止，`stop_all_modules`
/// （优雅回收模块子进程 + 释放端口）永不执行。SIGTERM handler 注册失败时
/// eprintln 警告并回退到仅 Ctrl+C（不 panic）。
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {},
                    _ = sigterm.recv() => {
                        tracing::info!("received SIGTERM, initiating graceful shutdown");
                    },
                }
                return;
            }
            Err(e) => {
                eprintln!(
                    "failed to install SIGTERM handler: {e}; falling back to CTRL+C only"
                );
            }
        }
    }

    tokio::signal::ctrl_c()
        .await
        .expect("failed to install CTRL+C signal handler");
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

    use ep_core::config::AppConfig;
    use ep_core::port::PortManager;
    use ep_core::types::{ComputeBackend, ComputeDevice, DeviceId};

    use crate::diff_new_lines;
    use crate::state::AppState;

    static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

    /// Helper: build a test AppState with one CPU device and a unique tempdir root.
    fn test_state() -> Arc<AppState> {
        let seq = TEST_SEQ.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("ep-daemon-test-{}-{seq}", std::process::id()));
        test_state_with_root(root)
    }

    fn test_state_with_root(root: std::path::PathBuf) -> Arc<AppState> {
        let devices = vec![ComputeDevice {
            id: DeviceId::Cpu,
            backend: ComputeBackend::Cpu,
            name: "Test CPU".to_string(),
            total_memory_mb: Some(16384),
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        }];

        Arc::new(AppState::new(
            root,
            AppConfig::default(),
            devices,
            vec![],
            PortManager::new(18000, 19000),
        ))
    }

    /// 构造带 loopback ConnectInfo 的请求（ip_filter 中间件需要）
    fn request(uri: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .extension(axum::extract::ConnectInfo(SocketAddr::from((
                [127, 0, 0, 1],
                54321,
            ))))
            .body(Body::empty())
            .unwrap()
    }

    async fn body_string(resp: axum::response::Response) -> String {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    // 1. GET /api/health → 200 + JSON with status "ok"
    #[tokio::test]
    async fn test_health_endpoint() {
        // health_check takes no extractor, call directly
        let resp = crate::api::health::health_check().await;
        let json = resp.0;
        assert_eq!(json["status"], "ok");
        assert!(json["version"].is_string());
    }

    // 2. GET /api/devices → device list with our test CPU
    #[tokio::test]
    async fn test_devices_endpoint() {
        let state = test_state();
        let resp = crate::api::devices::list_devices(State(state)).await;
        // Serialize to JSON to inspect fields (DeviceResponse fields are private)
        let json: Value = serde_json::to_value(&resp.0).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "cpu");
        assert_eq!(arr[0]["name"], "Test CPU");
        assert_eq!(arr[0]["total_memory_mb"], 16384);
    }

    // 3. GET /api/modules → empty module list
    #[tokio::test]
    async fn test_modules_endpoint() {
        let state = test_state();
        let resp = crate::api::modules::list_modules(State(state)).await;
        let modules = resp.0;
        assert!(modules.is_empty());
    }

    // 4. GET /api/config → default config
    #[tokio::test]
    async fn test_config_get() {
        let state = test_state();
        let resp = crate::api::config::get_config(State(state)).await;
        let config = resp.0;
        assert_eq!(config.general.language, "zh-CN");
        assert_eq!(config.ports.range_start, 18000);
        assert_eq!(config.ports.range_end, 19000);
    }

    // 5. POST /api/modules/:id/start + stop — non-existent module → 404 + 中文错误
    #[tokio::test]
    async fn test_start_stop_module() {
        let state = test_state();

        // Start non-existent module
        let (status, json) = crate::api::modules::start_module(
            State(state.clone()),
            axum::extract::Path("nonexistent".to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            json.0.get("error").is_some(),
            "expected error key in start response"
        );

        // Stop non-existent module
        let (status, json) = crate::api::modules::stop_module(
            State(state.clone()),
            axum::extract::Path("nonexistent".to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            json.0.get("error").is_some(),
            "expected error key in stop response"
        );
    }

    // 6. PUT /api/config → update, verify state and file persistence
    #[tokio::test]
    async fn test_config_put() {
        let state = test_state();

        let mut new_config = AppConfig::default();
        new_config.general.language = "en-US".to_string();
        new_config.ports.range_start = 20000;

        let resp = crate::api::config::put_config(
            State(state.clone()),
            axum::Json(new_config),
        )
        .await
        .expect("put_config should succeed");
        let updated = resp.0;
        assert_eq!(updated.general.language, "en-US");
        assert_eq!(updated.ports.range_start, 20000);

        // Verify the state was actually updated
        let config = state.config.read().await;
        assert_eq!(config.general.language, "en-US");
    }

    // 7. GET /api/modules/:id/logs — non-existent module → 404 + 中文错误
    #[tokio::test]
    async fn test_module_logs_unknown_module() {
        let state = test_state();
        let (status, json) = crate::api::modules::module_logs(
            State(state),
            axum::extract::Path("ghost".to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(json.0["error"].as_str().unwrap().contains("模块不存在"));
    }

    // ─── 新增测试 ───────────────────────────────────────────────────────────

    // 8. PUT /api/config 后配置实际落盘（tempdir 场景）
    #[tokio::test]
    async fn test_config_put_persists_to_disk() {
        let state = test_state();

        let mut new_config = AppConfig::default();
        new_config.general.language = "zh-TW".to_string();
        new_config.server.port = 12345;

        let _resp = crate::api::config::put_config(State(state.clone()), axum::Json(new_config))
            .await
            .expect("put_config should succeed");

        // 文件必须存在且可回读
        let file_path = state.root.join("config").join("app.toml");
        assert!(file_path.exists(), "config file should be written to disk");
        let loaded = AppConfig::load(state.root.join("config").as_path())
            .expect("reload persisted config");
        assert_eq!(loaded.general.language, "zh-TW");
        assert_eq!(loaded.server.port, 12345);
    }

    // 9. PUT /api/config 落盘失败 → 500 + 中文错误
    #[tokio::test]
    async fn test_config_put_save_failure_returns_500() {
        // root 指向一个普通文件 → root/config 无法创建 → save 失败
        let file_root = std::env::temp_dir().join(format!(
            "ep-daemon-file-root-{}-{}",
            std::process::id(),
            TEST_SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::write(&file_root, "blocker").unwrap();
        let state = test_state_with_root(file_root.clone());

        let result = crate::api::config::put_config(
            State(state),
            axum::Json(AppConfig::default()),
        )
        .await;
        let (status, json) = result.expect_err("save should fail when root is a file");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(json.0["error"].as_str().unwrap().contains("保存配置失败"));

        let _ = std::fs::remove_file(&file_root);
    }

    // 10. SPA 深链 → 200 + text/html（index.html 内容）
    #[tokio::test]
    async fn test_spa_deep_link_serves_index_html() {
        let state = test_state();
        let static_dir = state.root.join("static");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::write(
            static_dir.join("index.html"),
            "<html><body>EP-SPA-MARKER</body></html>",
        )
        .unwrap();

        let app = crate::build_app_router(state, &static_dir);
        let resp = app.oneshot(request("/tasks/deep/link")).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers()["content-type"].to_str().unwrap().to_string();
        assert!(ct.contains("text/html"), "content-type should be html, got {ct}");
        let body = body_string(resp).await;
        assert!(body.contains("EP-SPA-MARKER"));
    }

    // 11. 静态资源命中时直接返回文件（不走 fallback）
    #[tokio::test]
    async fn test_spa_static_file_served_directly() {
        let state = test_state();
        let static_dir = state.root.join("static");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::write(static_dir.join("index.html"), "<html>index</html>").unwrap();
        std::fs::write(static_dir.join("app.js"), "console.log(1);").unwrap();

        let app = crate::build_app_router(state, &static_dir);
        let resp = app.oneshot(request("/app.js")).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert_eq!(body, "console.log(1);");
    }

    // 12. /api/unknown → 404 + JSON（不落入 HTML fallback）
    #[tokio::test]
    async fn test_api_unknown_route_returns_404_json() {
        let state = test_state();
        let static_dir = state.root.join("static");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::write(static_dir.join("index.html"), "<html>index</html>").unwrap();

        let app = crate::build_app_router(state, &static_dir);
        let resp = app.oneshot(request("/api/unknown")).await.unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let ct = resp.headers()["content-type"].to_str().unwrap().to_string();
        assert!(ct.contains("application/json"), "content-type should be json, got {ct}");
        let body = body_string(resp).await;
        let json: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["error"], "接口不存在");
    }

    // 12b. /api/unknown + config.language=en → 404 + 英文错误
    #[tokio::test]
    async fn test_api_unknown_route_returns_404_json_en() {
        let state = test_state();
        state.config.write().await.general.language = "en".to_string();
        let static_dir = state.root.join("static");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::write(static_dir.join("index.html"), "<html>index</html>").unwrap();

        let app = crate::build_app_router(state, &static_dir);
        let resp = app.oneshot(request("/api/unknown")).await.unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_string(resp).await;
        let json: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["error"], "API endpoint not found");
    }

    // 13. /ws 路由已注册：非升级请求被拒绝（而不是落入 SPA fallback）
    #[tokio::test]
    async fn test_ws_route_registered_rejects_non_upgrade() {
        let state = test_state();
        let static_dir = state.root.join("static");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::write(static_dir.join("index.html"), "<html>index</html>").unwrap();

        let app = crate::build_app_router(state, &static_dir);
        let resp = app.oneshot(request("/ws")).await.unwrap();

        // WebSocketUpgrade 拒绝普通请求（4xx），绝不能是 200 + html
        assert!(
            resp.status().is_client_error(),
            "expected 4xx for non-upgrade /ws request, got {}",
            resp.status()
        );
    }

    // 14. Wave 2 骨架 stub 已全部实现（下载/删除/更新/上传/管线 CRUD/执行/任务），
    //     原 test_wave2_stubs_return_501 于 Wave 2 门禁退役；各端点真实行为由
    //     models.rs / upload.rs / pipelines.rs / execute.rs / tasks.rs 的测试覆盖。

    // ─── diff_new_lines 单元测试（日志去重策略） ────────────────────────────

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    // 15. 正常追加 → 只广播增量
    #[test]
    fn test_diff_append_only_new_lines() {
        let last = s(&["a", "b", "c"]);
        let new = s(&["a", "b", "c", "d", "e"]);
        assert_eq!(diff_new_lines(&last, &new), s(&["d", "e"]));
    }

    // 16. 无变化 → 不重复广播
    #[test]
    fn test_diff_no_change_broadcasts_nothing() {
        let last = s(&["a", "b", "c"]);
        assert_eq!(diff_new_lines(&last, &last), Vec::<String>::new());
    }

    // 17. 缓冲回绕（新快照更短，头部被截断）→ 仍只广播增量
    #[test]
    fn test_diff_wraparound_with_overlap() {
        let last = s(&["a", "b", "c", "d"]);
        // 环形缓冲丢掉头部 "a"，又追加了 "e"
        let new = s(&["b", "c", "d", "e"]);
        assert_eq!(diff_new_lines(&last, &new), s(&["e"]));
    }

    // 18. 完全无重叠（模块重启/日志重置）→ 整个新快照视为新增
    #[test]
    fn test_diff_disjoint_broadcasts_all() {
        let last = s(&["a", "b"]);
        let new = s(&["x", "y", "z"]);
        assert_eq!(diff_new_lines(&last, &new), s(&["x", "y", "z"]));
    }

    // 19. 首次快照（last 为空）→ 全部视为新增
    #[test]
    fn test_diff_first_snapshot_broadcasts_all() {
        let last: Vec<String> = Vec::new();
        let new = s(&["boot line 1", "boot line 2"]);
        assert_eq!(diff_new_lines(&last, &new), new);
    }

    // 20. 重复行不误判（最长后缀匹配）
    #[test]
    fn test_diff_repeated_lines() {
        let last = s(&["a", "a", "a"]);
        let new = s(&["a", "a", "a", "a"]);
        assert_eq!(diff_new_lines(&last, &new), s(&["a"]));
    }

    // ── stop_all_modules：优雅退出回收 ────────────────────────────────────────

    /// 跨平台"存活若干秒"的启动命令（Windows 无 sleep；ping 自带秒级延时）
    fn keepalive_command() -> &'static str {
        if cfg!(target_os = "windows") {
            "ping -n 30 127.0.0.1 > NUL"
        } else {
            "sleep 30"
        }
    }

    /// 构造测试 manifest（native 类型 + 存活命令）
    fn shutdown_test_manifest(module_id: &str) -> ep_core::module::manifest::ModuleManifest {
        use ep_core::module::manifest::{
            ComputeConfig, InterfaceConfig, InterfaceType, ModuleInfo, ModuleManifest,
            RuntimeConfig, RuntimeType,
        };
        use ep_core::types::ModuleCategory;
        ModuleManifest {
            module: ModuleInfo {
                id: module_id.to_string(),
                name: "shutdown-test".to_string(),
                version: "0.1.0".to_string(),
                description: String::new(),
                category: ModuleCategory::Other("test".to_string()),
                genre: String::new(),
                authors: vec![],
                license: None,
                homepage: None,
                tags: vec![],
            },
            runtime: RuntimeConfig {
                requirements_by_backend: Default::default(),
                runtime_type: RuntimeType::Native,
                python_version: None,
                requirements: None,
                entrypoint: None,
                start_command: Some(keepalive_command().to_string()),
                binaries: None,
            },
            compute: ComputeConfig {
                backends: vec![ep_core::types::ComputeBackend::Cpu],
                default_backend: Some(ep_core::types::ComputeBackend::Cpu),
                vram_estimate_mb: None,
                min_vram_mb: None,
                env: None,
            },
            models: vec![],
            interface: InterfaceConfig {
                interface_type: InterfaceType::Http,
                health_endpoint: Some("/health".to_string()),
                ready_timeout_secs: Some(60),
                working_dir: None,
                capabilities: vec![],
            },
        }
    }

    /// 启动两个存活模块到 state.process_manager（真实子进程，跳过健康等待）
    async fn seed_running_modules(state: &Arc<AppState>) {
        use ep_core::types::DeviceId;
        let (start, end) = (30000_u16, 30050_u16);
        let mut port = start;
        for mid in ["shut-a", "shut-b"] {
            let manifest = shutdown_test_manifest(mid);
            {
                let mut pm = state.process_manager.write().await;
                pm.start_module(mid, &manifest, DeviceId::Cpu, port, Default::default())
                    .await
                    .expect("mock module should start");
            }
            port += 1;
            if port > end {
                break;
            }
        }
    }

    // 21. 优雅退出：stop_all_modules 停止全部运行中模块（子进程不再残留）
    #[tokio::test]
    async fn test_stop_all_modules_reaps_child_processes() {
        let state = test_state();
        seed_running_modules(&state).await;

        // 前置：两个模块均已注册且 Running/Starting
        {
            let pm = state.process_manager.read().await;
            assert!(pm.get_instance("shut-a").is_some());
            assert!(pm.get_instance("shut-b").is_some());
            assert_eq!(pm.list_running().len(), 2);
        }

        crate::stop_all_modules(&state).await;

        // 后置：实例已停止（child 已 kill 并 reap）
        {
            let pm = state.process_manager.read().await;
            assert!(pm.list_running().is_empty());
            assert_eq!(
                pm.get_status("shut-a"),
                Some(&ep_core::types::ServiceStatus::Stopped)
            );
            assert_eq!(
                pm.get_status("shut-b"),
                Some(&ep_core::types::ServiceStatus::Stopped)
            );
        }
    }

    // 22. 无运行模块 → stop_all_modules 无副作用直接返回
    #[tokio::test]
    async fn test_stop_all_modules_idle_noop() {
        let state = test_state();
        crate::stop_all_modules(&state).await;
        let pm = state.process_manager.read().await;
        assert!(pm.list_running().is_empty());
    }

    // ── watcher abandoned 合并（评审修复：部分提交失败不得丢弃已提交 hit
    //    的状态推进，否则下轮 stable_ready 会重复提交同一文件） ─────────────

    use std::collections::{HashMap, VecDeque};
    use std::path::PathBuf;

    use crate::watcher::{self, FileSig, WatchRule};

    /// 构造单条 watcher 规则（checkpoint + 在途表）
    fn watch_rule(id: &str, checkpoint: i64, in_flight: Vec<(&str, FileSig)>) -> WatchRule {
        WatchRule {
            id: id.to_string(),
            name: "test".into(),
            enabled: true,
            watch_dir: "/nowhere".into(),
            recursive: false,
            extensions: vec![],
            include_modified: false,
            stability_secs: 30,
            backfill: false,
            checkpoint,
            in_flight: in_flight
                .into_iter()
                .map(|(p, s)| (p.to_string(), s))
                .collect(),
            max_batch: 16,
            direct: None,
            pipeline: None,
            output: None,
            last_task_id: None,
            recent: VecDeque::new(),
        }
    }

    fn registry_of(rules: Vec<WatchRule>) -> watcher::WatchRegistry {
        let mut r = watcher::WatchRegistry::default();
        for rule in rules {
            r.rules.insert(rule.id.clone(), rule);
        }
        r
    }

    // 23. 评审 bug 场景：同规则 hit1 提交成功、hit2 失败 → 合并表必须保留
    //     hit1 的状态推进（在途表移除 + 水位线推进 + recent/last_task_id），
    //     hit2 保持待触发；下一轮巡检只重试 hit2，不重复提交 hit1。
    #[test]
    fn abandoned_merge_keeps_submitted_hit_state_and_retries_only_failed() {
        let sig_a = FileSig { mtime: 1500, size: 100 };
        let sig_b = FileSig { mtime: 1600, size: 200 };
        let old = registry_of(vec![watch_rule(
            "r1",
            1000,
            vec![("/d/a.mkv", sig_a), ("/d/b.mkv", sig_b)],
        )]);
        // 本轮 collect 推进后的新表：批次内全部解决 + hit1 回写
        let mut next_rule = old.rules["r1"].clone();
        next_rule.in_flight.clear();
        next_rule.checkpoint = 1600;
        next_rule.last_task_id = Some("task-a".into());
        next_rule.recent.push_back(watcher::EventRecord {
            ts: 2000,
            file: "/d/a.mkv".into(),
            task_id: Some("task-a".into()),
            status: "submitted".into(),
            detail: None,
        });
        let next = registry_of(vec![next_rule]);

        let mut outcomes = HashMap::new();
        outcomes.insert(
            "r1".to_string(),
            super::RuleSubmitOutcome {
                all_submitted: false,
                submitted: vec![("/d/a.mkv".to_string(), sig_a)],
                failed_mtime: Some(1600),
            },
        );

        let merged = super::merge_abandoned_registry(&old, &next, &outcomes);
        let m = &merged.rules["r1"];
        assert!(!m.in_flight.contains_key("/d/a.mkv"), "已提交文件必须移出在途表");
        assert_eq!(
            m.in_flight.get("/d/b.mkv"),
            Some(&sig_b),
            "未提交文件保持待触发（下轮重试）"
        );
        assert_eq!(
            m.checkpoint, 1500,
            "水位线推进到已提交文件，但不得越过未提交文件的 mtime"
        );
        assert_eq!(m.last_task_id.as_deref(), Some("task-a"), "last_task_id 回写保留");
        assert_eq!(m.recent.len(), 1, "recent 速览保留");

        // 端到端：合并表下一轮巡检只重试 b，不重复提交 a
        let files = vec![
            watcher::DirFile { path: PathBuf::from("/d/a.mkv"), mtime: 1500, size: 100 },
            watcher::DirFile { path: PathBuf::from("/d/b.mkv"), mtime: 1600, size: 200 },
        ];
        let (hits, _) = watcher::collect_rule_events(m, &files, 2000);
        assert_eq!(
            hits,
            vec!["/d/b.mkv".to_string()],
            "只重试未提交文件，已提交文件不得重放"
        );
    }

    // 24. 全部 hit 提交成功的规则整体采纳新表；失败规则之后未尝试的规则
    //     （无 outcome 记录）原样保留旧表。
    #[test]
    fn abandoned_merge_adopts_next_rule_only_for_fully_submitted_rules() {
        let sig = FileSig { mtime: 1500, size: 100 };
        let r1_old = watch_rule("r1", 1000, vec![("/d/a.mkv", sig)]);
        let r2_old = watch_rule("r2", 1000, vec![("/d/c.mkv", sig)]);
        let old = registry_of(vec![r1_old.clone(), r2_old.clone()]);
        // r1 本轮全部成功（新表推进）；r2 在失败点之后从未尝试（新表虽有
        // collect 推进，但合并必须忽略）
        let mut r1_next = r1_old.clone();
        r1_next.in_flight.clear();
        r1_next.checkpoint = 1500;
        let mut r2_next = r2_old.clone();
        r2_next.in_flight.clear();
        r2_next.checkpoint = 1500;
        let next = registry_of(vec![r1_next, r2_next]);

        let mut outcomes = HashMap::new();
        outcomes.insert(
            "r1".to_string(),
            super::RuleSubmitOutcome {
                all_submitted: true,
                submitted: vec![("/d/a.mkv".to_string(), sig)],
                failed_mtime: None,
            },
        );

        let merged = super::merge_abandoned_registry(&old, &next, &outcomes);
        assert_eq!(
            merged.rules["r1"].checkpoint, 1500,
            "全成功规则采纳新表水位线"
        );
        assert!(merged.rules["r1"].in_flight.is_empty(), "全成功规则采纳新表在途表");
        assert_eq!(
            merged.rules["r2"].checkpoint, 1000,
            "未尝试规则不得采纳新表（保持待触发下轮重试）"
        );
        assert_eq!(
            merged.rules["r2"].in_flight.get("/d/c.mkv"),
            Some(&sig),
            "未尝试规则在途条目原样保留"
        );
    }

    // 25. 无任何成功提交（首个 hit 即失败）→ 合并结果等于旧表：
    //     watcher_registry_changed 判定无变化 → 不落盘语义保持。
    #[test]
    fn abandoned_merge_without_submitted_hits_is_noop() {
        let sig = FileSig { mtime: 1500, size: 100 };
        let old = registry_of(vec![watch_rule("r1", 1000, vec![("/d/a.mkv", sig)])]);
        let mut next_rule = old.rules["r1"].clone();
        next_rule.in_flight.clear();
        next_rule.checkpoint = 1500;
        let next = registry_of(vec![next_rule]);

        let mut outcomes = HashMap::new();
        outcomes.insert(
            "r1".to_string(),
            super::RuleSubmitOutcome {
                all_submitted: false,
                submitted: vec![],
                failed_mtime: Some(1500),
            },
        );

        let merged = super::merge_abandoned_registry(&old, &next, &outcomes);
        assert!(
            !super::watcher_registry_changed(&old, &merged),
            "无成功提交时合并必须是无操作（不落盘）"
        );
        assert_eq!(merged.rules["r1"].checkpoint, 1000);
        assert_eq!(merged.rules["r1"].in_flight.get("/d/a.mkv"), Some(&sig));
    }

    // 26. 同秒边角：已提交与失败 hit 的 mtime 相同 → 水位线封顶于失败 mtime
    //     之前，失败文件保持可重试（宁可已提交文件被重新考察，不静默丢文件）。
    #[test]
    fn abandoned_merge_checkpoint_capped_below_failed_hit_mtime() {
        let sig_a = FileSig { mtime: 1600, size: 100 };
        let sig_b = FileSig { mtime: 1600, size: 200 };
        let old = registry_of(vec![watch_rule(
            "r1",
            1000,
            vec![("/d/a.mkv", sig_a), ("/d/b.mkv", sig_b)],
        )]);
        let next = registry_of(vec![{
            let mut r = old.rules["r1"].clone();
            r.in_flight.clear();
            r.checkpoint = 1600;
            r
        }]);
        let mut outcomes = HashMap::new();
        outcomes.insert(
            "r1".to_string(),
            super::RuleSubmitOutcome {
                all_submitted: false,
                submitted: vec![("/d/a.mkv".to_string(), sig_a)],
                failed_mtime: Some(1600),
            },
        );

        let merged = super::merge_abandoned_registry(&old, &next, &outcomes);
        let m = &merged.rules["r1"];
        assert_eq!(
            m.checkpoint, 1599,
            "水位线必须封顶于失败 hit mtime 之前"
        );
        assert_eq!(
            m.in_flight.get("/d/b.mkv"),
            Some(&sig_b),
            "失败文件保持待触发"
        );
    }
}

/// 桌面端退役移植项（§2.1）：Windows 子进程错误弹窗抑制的存在性门禁。
/// SetErrorMode 置位后无返回值可断言，调用不崩溃即通过；
/// 真正行为验证在 D2 实机抽查（缺失 venv 探测不弹系统对话框）。
#[cfg(all(test, target_os = "windows"))]
mod error_dialog_tests {
    use super::suppress_error_dialogs;

    #[test]
    fn suppress_error_dialogs_callable() {
        suppress_error_dialogs();
    }
}
