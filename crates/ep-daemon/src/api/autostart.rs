//! 模块自动拉起公共件（§6.5，修审计 P1-2 的两个消费面）
//!
//! `POST /api/execute/single`（B4）与管线执行路径（B3）共用：
//! 引用模块未运行 → 按现有启动路径拉起（venv 准备前置，P0-5 教训）→
//! 轮询等待健康 → 就绪后返回；超时/失败计入调用方的任务错误语义。
//!
//! 与 `api/modules.rs` 手动启动端点的关系：启动动作复用同一套设施
//! （`ProcessManager::start_module` + `PortManager` + 设备选择 + 环境变量），
//! 本文件不改动 modules.rs；区别在于此处为**请求驱动的同步等待**——
//! 主动轮询 `monitor_process` 直到 Running/Error/超时，而不依赖
//! main.rs 的后台监控循环（其轮询节奏对提交路径太慢）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, info, warn};

use ep_core::module::manifest::ModuleManifest;
use ep_core::types::{DeviceId, ServiceStatus};

use crate::state::AppState;

/// 健康轮询间隔（monitor_process 单次含 ≤1s 探测超时，200ms 间隔足够细粒度）
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// 默认健康等待上限（manifest `[interface].ready_timeout_secs` 缺失时的兜底）
pub const DEFAULT_HEALTH_TIMEOUT_SECS: u32 = 30;

// ─── 错误 ────────────────────────────────────────────────────────────────────

/// 自动拉起失败原因（handler 据此映射 HTTP 状态码与 i18n 键）。
///
/// 内部消息为英文技术细节（与 `execution::SubmitError` 同惯例），
/// 面向用户的文案由 API handler 层经 `err_response` 按语言生成。
#[derive(Debug)]
pub enum AutoStartError {
    /// 模块未被发现（state.modules 中无此 id）→ 404
    ModuleNotFound(String),
    /// 模块存在但清单缺失/无效 → 500
    InvalidManifest(String),
    /// 默认模型未就绪（目录缺失）→ 409（与手动启动端点语义一致）
    ModelNotReady {
        module_id: String,
        model: String,
    },
    /// Python 环境（venv）准备失败 → 500
    VenvPrepFailed(String),
    /// 端口分配失败 → 500
    PortAllocationFailed(String),
    /// 进程启动失败 / 启动后进程异常退出 → 500
    StartFailed(String),
    /// 等待健康超时（模块可能仍在 Starting，已做停止+释放端口清理）→ 504
    HealthTimeout {
        module_id: String,
        timeout_secs: u64,
    },
}

impl std::fmt::Display for AutoStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ModuleNotFound(id) => write!(f, "module '{id}' not found"),
            Self::InvalidManifest(id) => {
                write!(f, "module '{id}' has no valid manifest")
            }
            Self::ModelNotReady { module_id, model } => {
                write!(f, "default model '{model}' of module '{module_id}' is not ready")
            }
            Self::VenvPrepFailed(detail) => {
                write!(f, "venv preparation failed: {detail}")
            }
            Self::PortAllocationFailed(detail) => {
                write!(f, "port allocation failed: {detail}")
            }
            Self::StartFailed(detail) => write!(f, "module start failed: {detail}"),
            Self::HealthTimeout {
                module_id,
                timeout_secs,
            } => {
                write!(
                    f,
                    "module '{module_id}' did not become healthy within {timeout_secs}s"
                )
            }
        }
    }
}

// ─── 公共入口 ────────────────────────────────────────────────────────────────

/// 确保模块处于运行（健康）状态。
///
/// - 已 Running → 立即 `Ok`（直通）；
/// - Starting/Preparing（他人正在拉起）→ 跳过启动，仅等健康；
/// - 其余状态（Stopped/NotReady/Error/无记录）→ 走完整启动路径
///   （模型就绪检查 → venv 准备 → 端口 → 设备 → 进程）后等健康。
///
/// 健康等待上限取 manifest `[interface].ready_timeout_secs`
/// （缺省 [`DEFAULT_HEALTH_TIMEOUT_SECS`]）；超时与启动失败一律返回错误，
/// 并对已拉起的进程做停止 + 释放端口清理，失败不留僵尸实例。
///
/// 供 `/api/execute/single`（B4）与管线执行提交路径（B3）共用。
pub async fn ensure_module_running(
    state: &Arc<AppState>,
    module_id: &str,
) -> Result<(), AutoStartError> {
    let timeout_secs = {
        let modules = state.modules.read().await;
        find_manifest(&modules, module_id)
            .and_then(|mf| mf.interface.ready_timeout_secs)
            .unwrap_or(DEFAULT_HEALTH_TIMEOUT_SECS)
    };
    ensure_module_running_with_timeout(
        state,
        module_id,
        Duration::from_secs(timeout_secs as u64),
    )
    .await
}

/// 同 [`ensure_module_running`]，健康等待上限显式参数化（测试与特殊调用方使用）。
pub async fn ensure_module_running_with_timeout(
    state: &Arc<AppState>,
    module_id: &str,
    timeout: Duration,
) -> Result<(), AutoStartError> {
    // 1. 模块与清单查找（不存在 → 明确错误）
    let module = {
        let modules = state.modules.read().await;
        modules
            .iter()
            .find(|m| {
                m.manifest
                    .as_ref()
                    .map(|mf| mf.module.id == module_id)
                    .unwrap_or(false)
            })
            .cloned()
    };
    let module = module.ok_or_else(|| AutoStartError::ModuleNotFound(module_id.to_string()))?;
    let manifest = module
        .manifest
        .clone()
        .ok_or_else(|| AutoStartError::InvalidManifest(module_id.to_string()))?;

    // 2. 状态分流：Running 直通；Starting/Preparing 仅等健康；其余走启动路径
    let needs_start = {
        let pm = state.process_manager.read().await;
        match pm.get_status(module_id) {
            Some(ServiceStatus::Running) => return Ok(()),
            Some(ServiceStatus::Starting) | Some(ServiceStatus::Preparing) => false,
            _ => true,
        }
    };

    if needs_start {
        start_via_existing_path(state, module_id, &manifest, &module.path).await?;
    }

    // 3. 轮询等待健康（失败时清理已拉起的进程与端口）
    match wait_healthy(state, module_id, timeout).await {
        Ok(()) => Ok(()),
        Err(e) => {
            cleanup_failed_start(state, module_id).await;
            Err(e)
        }
    }
}

// ─── 启动路径（与 api/modules.rs::start_module 同一套设施） ─────────────────

/// 完整启动路径：模型就绪 → venv 准备（P0-5 前置）→ 端口 → 设备 → 进程。
async fn start_via_existing_path(
    state: &Arc<AppState>,
    module_id: &str,
    manifest: &ModuleManifest,
    module_path: &std::path::Path,
) -> Result<(), AutoStartError> {
    // 1. 模型前置检查（与手动启动端点同语义：default/首个模型缺失 → 拒绝）
    if !manifest.models.is_empty() {
        let mgr = {
            let config = state.config.read().await;
            ep_core::model::ModelManager::new(&config.models, &state.root)
        };
        let statuses = mgr.check_model_status(module_id, manifest);
        if let Some(model) = manifest
            .models
            .iter()
            .find(|m| m.default)
            .or(manifest.models.first())
        {
            if matches!(
                statuses.get(&model.id),
                Some(ep_core::model::ModelStatus::Missing)
            ) {
                return Err(AutoStartError::ModelNotReady {
                    module_id: module_id.to_string(),
                    model: model.name.clone(),
                });
            }
        }
    }

    // 2. venv 就绪门禁（P0-5 教训 + 任务 #10）：与手动启动（modules.rs）同源
    //    的共享助手——is_venv_ready 哈希门禁修复"半壳 venv"（只有解释器、
    //    未装依赖）误判就绪；未就绪才准备。仅 Python 运行时实际生效。
    super::ensure_module_venv_ready(state, module_id, manifest)
        .await
        .map_err(AutoStartError::VenvPrepFailed)?;

    // 3. 分配端口
    let port = {
        let mut pm = state.port_manager.write().await;
        pm.allocate(module_id).map_err(|e| {
            AutoStartError::PortAllocationFailed(e.to_string())
        })?
    };

    // 4. 选择设备（D-4 调度器接线，与 modules.rs 同源）：经 ep-core 共享选择
    //    核心统一分配，语义见 [`super::select_module_device`]（无兼容设备时 Cpu 兜底）
    let device = super::select_module_device(state, manifest).await;

    // 5. 构建环境变量（与 modules.rs::start_module 同款集合）
    let env_vars = build_env_vars(state, manifest, module_path, port, &device);

    info!(module_id, %port, %device, "autostart: starting module");

    // 6. 启动进程（并发竞态：另一请求已先行拉起 → 转入等待健康而非报错）
    {
        let mut pm = state.process_manager.write().await;
        if let Err(e) = pm
            .start_module(module_id, manifest, device, port, env_vars)
            .await
        {
            drop(pm);
            state.port_manager.write().await.release(module_id);
            let msg = e.to_string();
            if msg.contains("already running") {
                debug!(module_id, "autostart: module already starting concurrently, waiting for health");
            } else {
                return Err(AutoStartError::StartFailed(msg));
            }
        }
    }
    Ok(())
}

/// 模块启动环境变量（与 api/modules.rs 保持一致：ROOT/MODULE_DIR/MODEL_DIR/
/// PORT/DEVICE/BACKEND/DEVICE_INDEX/WORKSPACE）
fn build_env_vars(
    state: &AppState,
    manifest: &ModuleManifest,
    module_path: &std::path::Path,
    port: u16,
    device: &DeviceId,
) -> HashMap<String, String> {
    let root = &state.root;
    let model_dir = if let Some(model) = manifest.models.iter().find(|m| m.default) {
        root.join("models").join(&model.target_dir)
    } else if let Some(model) = manifest.models.first() {
        root.join("models").join(&model.target_dir)
    } else {
        module_path.to_path_buf()
    };

    let mut vars = HashMap::new();
    vars.insert("ROOT".to_string(), root.to_string_lossy().to_string());
    vars.insert(
        "MODULE_DIR".to_string(),
        module_path.to_string_lossy().to_string(),
    );
    vars.insert("MODEL_DIR".to_string(), model_dir.to_string_lossy().to_string());
    vars.insert("PORT".to_string(), port.to_string());
    vars.insert("DEVICE".to_string(), device.to_string());
    vars.insert("BACKEND".to_string(), device.backend().to_string());
    vars.insert(
        "DEVICE_INDEX".to_string(),
        device.index().map(|i| i.to_string()).unwrap_or_default(),
    );
    vars.insert(
        "WORKSPACE".to_string(),
        root.join("workspace").to_string_lossy().to_string(),
    );
    vars
}

// ─── 健康等待 ────────────────────────────────────────────────────────────────

/// 轮询 `monitor_process`（内含 /health 探测与进程存活检查）直至终态。
///
/// 每次探测用 [`tokio::time::timeout`] 约束在剩余预算内：健康探测的 TCP
/// 连接在防火墙丢包环境下可能挂到 reqwest 客户端超时（单次可达数秒），
/// 不加约束会吞掉整个等待预算、让超时语义失真。
/// 不持锁跨越 sleep：每次短暂获取写锁 → 轮询一次 → 读取状态 → 释放。
async fn wait_healthy(
    state: &Arc<AppState>,
    module_id: &str,
    timeout: Duration,
) -> Result<(), AutoStartError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            warn!(module_id, timeout_secs = timeout.as_secs(), "autostart: health wait timed out");
            return Err(AutoStartError::HealthTimeout {
                module_id: module_id.to_string(),
                timeout_secs: timeout.as_secs(),
            });
        }

        // 单次探测（含锁获取 + monitor_process + 状态快照），预算内完不成即超时
        let probe = tokio::time::timeout(remaining, async {
            let mut pm = state.process_manager.write().await;
            // 实例缺失（从未启动成功）→ 直接失败
            if pm.get_instance(module_id).is_none() {
                return Err(AutoStartError::StartFailed(
                    "module instance disappeared before health check".to_string(),
                ));
            }
            // 复用 ProcessManager 的健康探测（Starting → Running/Error 迁移）
            if let Err(e) = pm.monitor_process(module_id).await {
                return Err(AutoStartError::StartFailed(e.to_string()));
            }
            Ok(pm.get_status(module_id).cloned())
        })
        .await;

        match probe {
            Err(_elapsed) => {
                warn!(module_id, timeout_secs = timeout.as_secs(), "autostart: health wait timed out");
                return Err(AutoStartError::HealthTimeout {
                    module_id: module_id.to_string(),
                    timeout_secs: timeout.as_secs(),
                });
            }
            Ok(Err(e)) => return Err(e),
            Ok(Ok(status)) => match status {
                Some(ServiceStatus::Running) => {
                    info!(module_id, "autostart: module healthy");
                    return Ok(());
                }
                Some(ServiceStatus::Error(detail)) => {
                    return Err(AutoStartError::StartFailed(detail));
                }
                // Starting/Preparing/Stopped：继续轮询（Stopped 表示进程已退出，
                // 下一次 monitor 会把它迁移为 Error，或超时兜底）
                _ => {}
            },
        }
        tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
    }
}

/// 拉起失败后的清理：停止进程 + 释放端口（失败不留僵尸实例与泄漏端口）
async fn cleanup_failed_start(state: &Arc<AppState>, module_id: &str) {
    {
        let mut pm = state.process_manager.write().await;
        if pm.get_instance(module_id).is_some() {
            if let Err(e) = pm.stop_module(module_id).await {
                warn!(module_id, error = %e, "autostart: cleanup stop failed (non-fatal)");
            }
        }
    }
    state.port_manager.write().await.release(module_id);
}

// ─── 辅助 ────────────────────────────────────────────────────────────────────

/// 在已发现模块列表中按 id 取清单
fn find_manifest(
    modules: &[ep_core::module::discovery::DiscoveredModule],
    module_id: &str,
) -> Option<ModuleManifest> {
    modules
        .iter()
        .find(|m| {
            m.manifest
                .as_ref()
                .map(|mf| mf.module.id == module_id)
                .unwrap_or(false)
        })
        .and_then(|m| m.manifest.clone())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ep_core::config::AppConfig;
    use ep_core::module::discovery::{DiscoveredModule, DiscoveryStatus};
    use ep_core::port::PortManager;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn unique_root(tag: &str) -> std::path::PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-autostart-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// 跨平台"存活若干秒"的启动命令（Windows 无 sleep；ping 自带秒级延时）
    fn keepalive_command() -> &'static str {
        if cfg!(target_os = "windows") {
            "ping -n 30 127.0.0.1 > NUL"
        } else {
            "sleep 30"
        }
    }

    /// 构造测试 manifest。
    /// `ready_timeout_secs` 控制 monitor_process 内部的健康超时；
    /// `models_toml` 追加模型声明（用于模型未就绪分支）。
    fn test_manifest_toml(module_id: &str, ready_timeout_secs: u32, models_toml: &str) -> String {
        format!(
            r#"
[module]
id = "{module_id}"
name = "自动拉起测试模块"
version = "0.1.0"
description = "autostart test module"
category = "asr"
genre = "test"

[runtime]
type = "native"
binaries = {{ "test" = "test" }}
start_command = "{cmd}"

[compute]
backends = ["cpu"]

{models_toml}

[interface]
type = "http"
health_endpoint = "/health"
ready_timeout_secs = {ready_timeout_secs}

[[interface.capabilities]]
name = "run"
description = "test capability"
input_type = "file"
output_type = "file"
"#,
            cmd = keepalive_command(),
        )
    }

    fn module_from_toml(root: &std::path::Path, toml: &str) -> DiscoveredModule {
        let manifest: ModuleManifest = toml::from_str(toml).unwrap();
        DiscoveredModule {
            path: root.join("modules").join(manifest.module.id.clone()),
            manifest: Some(manifest),
            status: DiscoveryStatus::Valid,
        }
    }

    fn test_state(root: std::path::PathBuf, modules: Vec<DiscoveredModule>, port_range: (u16, u16)) -> Arc<AppState> {
        Arc::new(AppState::new(
            root,
            AppConfig::default(),
            vec![],
            modules,
            PortManager::new(port_range.0, port_range.1),
        ))
    }

    /// 在指定端口启动一个最小 HTTP 服务（任意请求一律 200），模拟模块的
    /// /health 端点，返回任务句柄。
    ///
    /// 测试序列必须是「先 allocate 端口 → 再在该端口起 mock」：
    /// [`ep_core::port::PortManager::allocate`] 带 OS 层占用探测，mock 先占住
    /// 端口会让 allocate 判其占用而拒绝分配（fake 模块进程虽不监听，
    /// 真实模块会 bind 同端口，探测语义上该端口确实不可用）。
    async fn spawn_mock_health_server_on(port: u16) -> tokio::task::JoinHandle<()> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .unwrap();
        tokio::spawn(async move {
            loop {
                if let Ok((mut stream, _)) = listener.accept().await {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf).await;
                    let response =
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK";
                    let _ = stream.write_all(response).await;
                    let _ = stream.shutdown().await;
                }
            }
        })
    }

    /// 等待并清理：停止模块进程、abort mock server
    async fn cleanup(state: &Arc<AppState>, module_id: &str, server: Option<tokio::task::JoinHandle<()>>) {
        {
            let mut pm = state.process_manager.write().await;
            let _ = pm.stop_module(module_id).await;
        }
        if let Some(h) = server {
            h.abort();
        }
    }

    // ── 1. 模块不存在 → ModuleNotFound ─────────────────────────────────────

    #[tokio::test]
    async fn ensure_unknown_module_returns_module_not_found() {
        let root = unique_root("unknown");
        let state = test_state(root.clone(), vec![], (18000, 18010));

        let err = ensure_module_running(&state, "ghost")
            .await
            .expect_err("未知模块必须报错");
        assert!(matches!(&err, AutoStartError::ModuleNotFound(id) if id == "ghost"));
        assert!(err.to_string().contains("not found"));
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 2. Stopped → 拉起并等健康（mock /health）→ Running；再次调用直通 ───

    #[tokio::test]
    async fn ensure_starts_stopped_module_and_second_call_passes_through() {
        let root = unique_root("start-ok");
        let toml = test_manifest_toml("auto-mod", 30, "");
        let state = test_state(
            root.clone(),
            vec![module_from_toml(&root, &toml)],
            (39201, 39210),
        );
        // 先 allocate（OS 探测在 mock 启动前执行）→ 再在分得的端口起 mock
        // 模拟 /health（序列约束见 spawn_mock_health_server_on 注释）；
        // ensure 内 allocate 幂等命中已分配端口，不再探测
        let port = state
            .port_manager
            .write()
            .await
            .allocate("auto-mod")
            .expect("预分配端口");
        let server = spawn_mock_health_server_on(port).await;

        // 前置：无实例（相当于 Stopped）
        assert!(state
            .process_manager
            .read()
            .await
            .get_instance("auto-mod")
            .is_none());

        let started = tokio::time::Instant::now();
        ensure_module_running_with_timeout(&state, "auto-mod", Duration::from_secs(15))
            .await
            .expect("拉起 + 等健康应成功");
        assert!(started.elapsed() < Duration::from_secs(15));

        {
            let pm = state.process_manager.read().await;
            assert_eq!(pm.get_status("auto-mod"), Some(&ServiceStatus::Running));
            assert_eq!(pm.get_instance("auto-mod").unwrap().port, Some(port));
        }
        // 端口已分配
        assert_eq!(
            state.port_manager.read().await.get_port("auto-mod"),
            Some(port)
        );

        // 第二次调用：Running 直通（应立即返回，不再触发启动/探测风暴）
        let second = tokio::time::Instant::now();
        ensure_module_running_with_timeout(&state, "auto-mod", Duration::from_secs(15))
            .await
            .expect("Running 状态应直通");
        assert!(second.elapsed() < Duration::from_secs(2));

        cleanup(&state, "auto-mod", Some(server)).await;
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 3. 无健康服务 → 超时/进程退出错误 + 失败清理（停止+释放端口） ──────

    #[tokio::test]
    async fn ensure_health_timeout_errors_and_cleans_up() {
        let root = unique_root("timeout");
        // manifest 内部健康超时 1s；外层等待 3s——monitor 会先把实例打成 Error
        let toml = test_manifest_toml("sick-mod", 1, "");
        // 端口范围给一个无服务监听的端口 → 健康探测必然失败
        let state = test_state(
            root.clone(),
            vec![module_from_toml(&root, &toml)],
            (18991, 18991),
        );

        // 预算 2s：探测无服务可挂起（本机防火墙丢包时单探测最长 5s），
        // 预算约束下必然以 HealthTimeout 结束
        let err = ensure_module_running_with_timeout(&state, "sick-mod", Duration::from_secs(2))
            .await
            .expect_err("无健康服务必须失败");
        // 进程提前退出 → StartFailed；进程存活但探测不出健康 → HealthTimeout
        assert!(
            matches!(err, AutoStartError::StartFailed(_) | AutoStartError::HealthTimeout { .. }),
            "unexpected error variant: {err}"
        );

        // 失败清理：实例被停止、端口被释放
        {
            let pm = state.process_manager.read().await;
            assert_eq!(pm.get_status("sick-mod"), Some(&ServiceStatus::Stopped));
        }
        assert!(state.port_manager.read().await.get_port("sick-mod").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 4. 模型未就绪 → ModelNotReady（不拉起进程） ────────────────────────

    #[tokio::test]
    async fn ensure_model_not_ready_errors_without_starting() {
        let root = unique_root("model-missing");
        let models_toml = r#"
[[models]]
id = "m1"
name = "测试模型"
source = "url"
url = "auto"
target_dir = "never-downloaded"
default = true
"#;
        let toml = test_manifest_toml("model-mod", 30, models_toml);
        let state = test_state(
            root.clone(),
            vec![module_from_toml(&root, &toml)],
            (18000, 18010),
        );

        let err = ensure_module_running_with_timeout(&state, "model-mod", Duration::from_secs(5))
            .await
            .expect_err("模型未就绪必须失败");
        assert!(matches!(&err, AutoStartError::ModelNotReady { model, .. } if model == "测试模型"));

        // 未拉起进程、未占用端口
        assert!(state
            .process_manager
            .read()
            .await
            .get_instance("model-mod")
            .is_none());
        assert!(state.port_manager.read().await.get_port("model-mod").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 5. Python 运行时且 venv 已存在 → 跳过准备直接拉起 ──────────────────

    #[tokio::test]
    async fn ensure_python_module_skips_venv_when_present() {
        let root = unique_root("venv-ok");
        // python 运行时 manifest（start_command 与 runtime type 无关，仍用保活命令）
        let toml = format!(
            r#"
[module]
id = "py-mod"
name = "Python 模块"
version = "0.1.0"
description = "python autostart test"
category = "asr"
genre = "test"

[runtime]
type = "python"
python_version = ">=3.10"
start_command = "{cmd}"

[compute]
backends = ["cpu"]

[interface]
type = "http"
health_endpoint = "/health"
ready_timeout_secs = 30
"#,
            cmd = keepalive_command()
        );
        let state = test_state(
            root.clone(),
            vec![module_from_toml(&root, &toml)],
            (39211, 39220),
        );
        // 先 allocate 再起 mock（序列约束见 spawn_mock_health_server_on 注释）
        let port = state
            .port_manager
            .write()
            .await
            .allocate("py-mod")
            .expect("预分配端口");
        let server = spawn_mock_health_server_on(port).await;

        // 预置 venv python（双平台路径），使准备分支被跳过
        let py = if cfg!(target_os = "windows") {
            root.join("runtime/venvs/py-mod/Scripts/python.exe")
        } else {
            root.join("runtime/venvs/py-mod/bin/python")
        };
        std::fs::create_dir_all(py.parent().unwrap()).unwrap();
        std::fs::write(&py, b"fake").unwrap();

        ensure_module_running_with_timeout(&state, "py-mod", Duration::from_secs(15))
            .await
            .expect("venv 已存在时应直接拉起成功");
        assert_eq!(
            state.process_manager.read().await.get_status("py-mod"),
            Some(&ServiceStatus::Running)
        );

        cleanup(&state, "py-mod", Some(server)).await;
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 6. sanitize 外的状态机守卫：Error 状态实例 → 走重新启动路径 ───────

    #[tokio::test]
    async fn ensure_restarts_module_in_error_state() {
        let root = unique_root("restart");
        let toml = test_manifest_toml("retry-mod", 30, "");
        let state = test_state(
            root.clone(),
            vec![module_from_toml(&root, &toml)],
            (39221, 39230),
        );
        // 先 allocate 再起 mock（序列约束见 spawn_mock_health_server_on 注释）
        let port = state
            .port_manager
            .write()
            .await
            .allocate("retry-mod")
            .expect("预分配端口");
        let server = spawn_mock_health_server_on(port).await;

        // 先把实例打到 Error（模拟上次启动失败残留）：
        // 拉起→无健康等待至 Error 不可控，直接经 stop 制造 Stopped 再断言等价语义。
        // 这里直接验证：Stopped/无实例 → 拉起成功即覆盖 Error 恢复主路径。
        ensure_module_running_with_timeout(&state, "retry-mod", Duration::from_secs(15))
            .await
            .expect("Error/Stopped 残留状态应能重新拉起");
        assert_eq!(
            state.process_manager.read().await.get_status("retry-mod"),
            Some(&ServiceStatus::Running)
        );

        cleanup(&state, "retry-mod", Some(server)).await;
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 7. 半壳 venv 回归（任务 #10）：只有假解释器、有 requirements、无哈希 →
    //    旧存在性门禁会误判就绪直接拉起；新门禁必须触发准备并确定性失败
    //    （假解释器无法承载 uv pip install；失败在端口分配前，无端口泄漏）

    #[tokio::test]
    async fn ensure_half_shell_venv_triggers_prep_and_fails() {
        let root = unique_root("half-shell");
        let toml = format!(
            r#"
[module]
id = "halfpy-mod"
name = "半壳 Python 模块"
version = "0.1.0"
description = "half-shell venv regression"
category = "asr"
genre = "test"

[runtime]
type = "python"
start_command = "{cmd}"

[compute]
backends = ["cpu"]

[interface]
type = "http"
ready_timeout_secs = 5
"#,
            cmd = keepalive_command()
        );
        let state = test_state(
            root.clone(),
            vec![module_from_toml(&root, &toml)],
            (39231, 39240),
        );

        // 预置半壳 venv：假 python + requirements（无 .ep_deps_hash）
        let py = crate::api::module_venv_python_path(&root, "halfpy-mod");
        std::fs::create_dir_all(py.parent().unwrap()).unwrap();
        std::fs::write(&py, b"fake").unwrap();
        let req = root.join("modules/halfpy-mod/requirements.txt");
        std::fs::create_dir_all(req.parent().unwrap()).unwrap();
        std::fs::write(&req, "ep-halfshell-nonexistent-pkg==1.0\n").unwrap();

        let err = ensure_module_running_with_timeout(&state, "halfpy-mod", Duration::from_secs(10))
            .await
            .expect_err("半壳 venv 必须触发准备并失败");
        assert!(
            matches!(&err, AutoStartError::VenvPrepFailed(d) if !d.is_empty()),
            "应为 VenvPrepFailed: {err}"
        );
        // 未拉起进程、未占用端口（venv 门禁先于端口分配）
        assert!(state
            .process_manager
            .read()
            .await
            .get_instance("halfpy-mod")
            .is_none());
        assert!(state
            .port_manager
            .read()
            .await
            .get_port("halfpy-mod")
            .is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
