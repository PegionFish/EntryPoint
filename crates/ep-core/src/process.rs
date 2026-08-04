//! 进程管理器 — 管理模块服务实例的生命周期（启动/停止/状态/日志）

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::config::AppConfig;
use crate::health::{check_health, HealthStatus};
use crate::module::manifest::ModuleManifest;
use crate::types::{DeviceId, ServiceStatus};

/// 日志缓冲区最大行数
const MAX_LOG_LINES: usize = 500;

/// 共享 CUDA 库目录默认值（相对 root）。与 config.rs `[compute].cuda_libs_dir` 默认值保持一致（§3.1）。
pub const DEFAULT_CUDA_LIBS_DIR: &str = "runtime/cuda-libs";

// ─── 模块子进程环境构建（§3.1 / §15.3 平台分支） ─────────────────────────────

/// 解析 `cuda_libs_dir` 配置值：绝对路径原样返回，相对路径基于 root 解析（Path::join）。
///
/// 空字符串视为"禁用注入"，返回空 PathBuf（调用方的 is_dir 检查自然跳过）。
pub fn resolve_cuda_libs_dir(root: &Path, cuda_libs_dir: &str) -> PathBuf {
    let trimmed = cuda_libs_dir.trim();
    if trimmed.is_empty() {
        return PathBuf::new();
    }
    let p = Path::new(trimmed);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    }
}

/// 纯函数：按平台约定构造动态库搜索路径环境变量（键, 值），供单测覆盖双平台分支。
///
/// - Linux:   `LD_LIBRARY_PATH = <dir>[:继承值]`
/// - Windows: `PATH = <dir>;继承值`（前置，保证 DLL 搜索序优先命中共享库，§15.3）
fn compose_lib_path(dir: &str, is_windows: bool, inherited: &str) -> (String, String) {
    if is_windows {
        let value = if inherited.is_empty() {
            dir.to_string()
        } else {
            format!("{dir};{inherited}")
        };
        ("PATH".to_string(), value)
    } else {
        let value = if inherited.is_empty() {
            dir.to_string()
        } else {
            format!("{dir}:{inherited}")
        };
        ("LD_LIBRARY_PATH".to_string(), value)
    }
}

/// 为共享 CUDA 库目录生成平台动态库搜索路径环境变量（§3.1，双平台分支）。
///
/// - Linux:   `LD_LIBRARY_PATH = <dir>[:继承值]`
/// - Windows: `PATH = <dir>;继承值`（前置，DLL 搜索序）
///
/// 继承值取自当前进程环境（子进程环境由本侧显式构造）。
/// `cuda_libs_dir` 为空时返回 None（不注入）。
pub fn cuda_lib_path_env(cuda_libs_dir: &Path) -> Option<(String, String)> {
    if cuda_libs_dir.as_os_str().is_empty() {
        return None;
    }
    let dir = cuda_libs_dir.to_string_lossy();
    let (key, value) = if cfg!(windows) {
        compose_lib_path(&dir, true, &std::env::var("PATH").unwrap_or_default())
    } else {
        compose_lib_path(&dir, false, &std::env::var("LD_LIBRARY_PATH").unwrap_or_default())
    };
    Some((key, value))
}

/// 读取 `manifest.compute.env.<backend>` 表（backend = 当前设备后端的小写名，
/// 如 cuda/rocm/openvino/directml/cpu），将值中 `{device_index}` 替换为实际设备号后
/// 返回待注入的环境变量（§3.1 compute.env 接线，CUDA_VISIBLE_DEVICES 等多卡隔离立即生效）。
///
/// 防御性读取：表不存在 / 当前 backend 无条目时返回空（接口以现有 `ModuleManifest` 字段为准）。
/// 注：OpenVINO 等字符串索引设备无数字 index，`{device_index}` 替换为空串。
pub fn backend_env_vars(manifest: &ModuleManifest, device: &DeviceId) -> Vec<(String, String)> {
    let Some(env_map) = manifest.compute.env.as_ref() else {
        return Vec::new();
    };
    let backend_key = device.backend().to_string();
    let Some(table) = env_map.get(&backend_key) else {
        return Vec::new();
    };
    let index = device.index().map(|i| i.to_string()).unwrap_or_default();
    table
        .iter()
        .map(|(k, v)| (k.clone(), v.replace("{device_index}", &index)))
        .collect()
}

/// 模块 venv Python 解释器路径（平台分支：Windows `Scripts/python.exe`，Linux `bin/python`）。
pub fn venv_python_path(root: &Path, module_id: &str) -> PathBuf {
    let venv_dir = root.join("runtime").join("venvs").join(module_id);
    if cfg!(windows) {
        venv_dir.join("Scripts").join("python.exe")
    } else {
        venv_dir.join("bin").join("python")
    }
}

/// 构建模块子进程的标准 env 模板变量（P0-4 公共构建函数，daemon API / --run-module / 桌面端共用）。
///
/// 返回的键是**裸占位符名**（无前缀）：既用于 `start_command` 占位符替换
/// （`{ROOT}`/`{MODULE_DIR}`/`{MODEL_DIR}`/...），也由 [`ProcessManager::start_module`]
/// 统一加一次 `EP_` 前缀后注入子进程（保证不会出现 `EP_EP_*` 双重前缀，P0-3）。
///
/// 子进程最终环境由 start_module 统一装配，还包括：
/// - 共享 CUDA 库目录注入（`with_cuda_libs_dir`，Linux LD_LIBRARY_PATH / Windows PATH 前置）
/// - `manifest.compute.env.<backend>` 表（`{device_index}` 已替换）
/// - 网络代理变量（`with_network_env`）
pub fn build_module_env(
    root: &Path,
    config: &AppConfig,
    module_id: &str,
    manifest: &ModuleManifest,
    device: &DeviceId,
) -> HashMap<String, String> {
    let module_dir = root.join("modules").join(module_id);
    // 激活变体单槽位（A6，§5.2）：active_models 配置 → default=true → 首个变体；
    // MODEL_DIR 走 config.models.cache_dir 解析（修 P2-9 同款硬编码）。
    let active_model = crate::model::active_model_for(config, manifest)
        .and_then(|id| manifest.models.iter().find(|m| m.id == id));
    let model_dir = match active_model {
        Some(model) => config.resolve_model_cache_dir(root).join(&model.target_dir),
        None => module_dir.clone(),
    };

    let mut vars = HashMap::new();
    vars.insert("ROOT".to_string(), root.to_string_lossy().to_string());
    vars.insert("MODULE_ID".to_string(), module_id.to_string());
    vars.insert(
        "MODULE_DIR".to_string(),
        module_dir.to_string_lossy().to_string(),
    );
    vars.insert(
        "MODEL_DIR".to_string(),
        model_dir.to_string_lossy().to_string(),
    );
    vars.insert(
        "WORKSPACE".to_string(),
        root.join("workspace").to_string_lossy().to_string(),
    );
    vars.insert("LOG_LEVEL".to_string(), "info".to_string());
    vars.insert("DEVICE".to_string(), device.to_string());
    vars.insert("BACKEND".to_string(), device.backend().to_string());
    vars.insert(
        "DEVICE_INDEX".to_string(),
        device.index().map(|i| i.to_string()).unwrap_or_default(),
    );
    if let Some(model) = active_model {
        vars.insert("MODEL_ID".to_string(), model.id.clone());
    }
    vars
}

/// 准备启动模板变量：规范化调用方键名 + 追加内置变量（P0-3 修复核心，纯函数可测）。
///
/// 1. **EP_ 前缀归一**：剥离调用方键上已有的 `EP_` 前缀（前缀由 start_module 统一加一次，
///    以 process.rs 一侧为准；调用方传 `EP_ROOT` 或 `ROOT` 都得到 `EP_ROOT`，绝不产生 `EP_EP_*`）。
/// 2. **大小写别名**：为大写标准键补充小写别名（`ROOT`→`root`、`MODULE_DIR`→`module_dir` ...），
///    使 build_start_command 文档承诺的两种占位符风格都能命中。
/// 3. **内置变量**：port/device/device_index/backend/entrypoint/binary/venv_python
///    （venv_python 依赖 ROOT，平台分支见 [`venv_python_path`]）。
fn prepare_template_vars(
    module_id: &str,
    manifest: &ModuleManifest,
    device: &DeviceId,
    port: u16,
    env_vars: HashMap<String, String>,
) -> HashMap<String, String> {
    // 1. EP_ 前缀归一（P0-3 ①）
    let mut vars: HashMap<String, String> = env_vars
        .into_iter()
        .map(|(k, v)| (k.strip_prefix("EP_").unwrap_or(&k).to_string(), v))
        .collect();

    // 2. 大写标准键 → 小写别名（不覆盖已有键）
    const CASE_ALIASES: &[(&str, &str)] = &[
        ("ROOT", "root"),
        ("MODULE_DIR", "module_dir"),
        ("MODEL_DIR", "model_dir"),
        ("MODULE_ID", "module_id"),
        ("MODEL_ID", "model_id"),
        ("WORKSPACE", "workspace"),
        ("LOG_LEVEL", "log_level"),
    ];
    for &(upper, lower) in CASE_ALIASES {
        if let Some(value) = vars.get(upper).cloned() {
            vars.entry(lower.to_string()).or_insert(value);
        }
    }

    // 3. 内置变量
    vars.insert("port".to_string(), port.to_string());
    vars.insert("device".to_string(), device.to_string());
    vars.insert(
        "device_index".to_string(),
        device.index().map(|i| i.to_string()).unwrap_or_default(),
    );
    vars.insert("backend".to_string(), device.backend().to_string());

    if let Some(ref ep) = manifest.runtime.entrypoint {
        vars.insert("entrypoint".to_string(), ep.clone());
    }
    // 取第一个 binary 的值作为 {binary}
    if let Some(ref binaries) = manifest.runtime.binaries {
        if let Some((_, path)) = binaries.iter().next() {
            vars.insert("binary".to_string(), path.clone());
        }
    }

    // M1: 注入平台自适应的 venv python 路径
    // 模块 TOML 可用 {venv_python} 替代硬编码的 bin/python（P0-3 ②：键名与占位符对齐）
    if let Some(root) = vars.get("ROOT").or_else(|| vars.get("root")).cloned() {
        let python = venv_python_path(Path::new(&root), module_id);
        vars.insert("venv_python".to_string(), python.to_string_lossy().to_string());
    }

    vars
}

// ─── ServiceInstance ─────────────────────────────────────────────────────────

/// 单个模块服务实例的运行时状态
pub struct ServiceInstance {
    pub module_id: String,
    pub status: ServiceStatus,
    pub device: Option<DeviceId>,
    pub port: Option<u16>,
    pub started_at: Option<DateTime<Utc>>,
    /// 最近 500 行日志
    pub log_buffer: VecDeque<String>,
    /// 实际子进程句柄
    pub child: Option<Child>,
    /// 日志接收端：reader task 通过此 channel 回传 stdout/stderr 行
    log_rx: Option<mpsc::UnboundedReceiver<String>>,
    /// 健康检查端点（如 "/health"）
    health_endpoint: Option<String>,
    /// 健康检查超时（秒）
    ready_timeout_secs: u32,
}

impl ServiceInstance {
    fn new(module_id: &str) -> Self {
        Self {
            module_id: module_id.to_string(),
            status: ServiceStatus::Stopped,
            device: None,
            port: None,
            started_at: None,
            log_buffer: VecDeque::with_capacity(MAX_LOG_LINES),
            child: None,
            log_rx: None,
            health_endpoint: None,
            ready_timeout_secs: 30,
        }
    }

    /// 获取子进程 PID（如果进程仍在运行）
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(|c| c.id())
    }
}

// ─── ProcessManager ──────────────────────────────────────────────────────────

/// 进程管理器：跟踪所有模块服务实例
pub struct ProcessManager {
    instances: HashMap<String, ServiceInstance>,
    /// 注入模块子进程的网络代理环境变量（仅非空值会被注入）
    network_env: Vec<(String, String)>,
    /// 共享 CUDA 库目录（§3.1）：启动模块子进程时按平台注入动态库搜索路径
    /// （Linux: LD_LIBRARY_PATH 前置；Windows: PATH 前置）。None = 不注入。
    cuda_libs_dir: Option<PathBuf>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            instances: HashMap::new(),
            network_env: Vec::new(),
            cuda_libs_dir: None,
        }
    }

    /// 设置网络代理配置（链式调用）。
    ///
    /// 模块服务子进程启动时将被注入这些环境变量（如 HTTP_PROXY 等）。
    pub fn with_network_env(mut self, env_vars: Vec<(String, String)>) -> Self {
        self.network_env = env_vars;
        self
    }

    /// 设置共享 CUDA 库目录（链式调用，§3.1）。
    ///
    /// 模块服务子进程启动时注入平台动态库搜索路径：
    /// Linux `LD_LIBRARY_PATH=<dir>[:继承值]`；Windows `PATH=<dir>;继承值`（DLL 搜索序）。
    /// 目录不存在时 start_module 自动跳过注入。
    pub fn with_cuda_libs_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cuda_libs_dir = Some(dir.into());
        self
    }

    /// 设置共享 CUDA 库目录（None = 不注入）
    pub fn set_cuda_libs_dir(&mut self, dir: Option<PathBuf>) {
        self.cuda_libs_dir = dir;
    }

    /// 启动模块服务。
    ///
    /// 构建启动命令，实际 spawn 子进程，捕获 stdout/stderr 到日志缓冲区。
    pub async fn start_module(
        &mut self,
        module_id: &str,
        manifest: &ModuleManifest,
        device: DeviceId,
        port: u16,
        env_vars: HashMap<String, String>,
    ) -> Result<()> {
        // 检查是否已在运行
        if let Some(inst) = self.instances.get(module_id) {
            if inst.status.is_running() || inst.status == ServiceStatus::Starting {
                bail!(
                    "module '{}' is already running/starting (status: {:?})",
                    module_id,
                    inst.status
                );
            }
        }

        // 构建启动命令（prepare_template_vars：EP_ 前缀归一 + 内置变量，P0-3）
        let vars = prepare_template_vars(module_id, manifest, &device, port, env_vars);

        let command = Self::build_start_command(manifest, &vars);
        info!(module_id, %command, "built start command");

        // 实际 spawn 子进程
        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = tokio::process::Command::new("cmd");
            c.args(["/C", &command]);
            c
        } else {
            let mut c = tokio::process::Command::new("sh");
            c.args(["-c", &command]);
            c
        };

        // 设置环境变量：统一加一次 EP_ 前缀（键已在 prepare_template_vars 归一，
        // 调用方带不带前缀都只产生一层 EP_*，绝不出现 EP_EP_*，P0-3 ①）
        for (key, value) in &vars {
            let env_key = format!("EP_{}", key.to_uppercase());
            cmd.env(&env_key, value);
        }

        // 注入共享 CUDA 库目录（§3.1，双平台分支）：
        // Linux LD_LIBRARY_PATH 前置 / Windows PATH 前置（DLL 搜索序）
        if let Some(ref dir) = self.cuda_libs_dir {
            if dir.is_dir() {
                if let Some((key, value)) = cuda_lib_path_env(dir) {
                    cmd.env(&key, value);
                }
            }
        }

        // 注入 manifest.compute.env.<backend> 表（§3.1 compute.env 接线：
        // {device_index} 已替换为实际设备号，CUDA_VISIBLE_DEVICES 等多卡隔离立即生效）
        for (key, value) in backend_env_vars(manifest, &device) {
            cmd.env(&key, value);
        }

        // 注入网络代理环境变量（仅非空值）
        for (key, value) in &self.network_env {
            if !value.is_empty() {
                cmd.env(key, value);
            }
        }

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        // 设置 working_dir（如果 manifest 指定了）
        if let Some(ref wd) = manifest.interface.working_dir {
            cmd.current_dir(wd);
        }

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("failed to spawn module '{}': {}", module_id, e)
        })?;

        let pid = child.id();
        debug!(module_id, ?pid, "spawned child process");

        // H1: 捕获 stdout/stderr 到日志缓冲区（通过 channel 回传）
        let (log_tx, log_rx) = mpsc::unbounded_channel::<String>();

        if let Some(stdout) = child.stdout.take() {
            let tx = log_tx.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let tx = log_tx.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if tx.send(format!("[stderr] {line}")).is_err() {
                        break;
                    }
                }
            });
        }

        // Store the instance
        let instance = self
            .instances
            .entry(module_id.to_string())
            .or_insert_with(|| ServiceInstance::new(module_id));

        instance.status = ServiceStatus::Starting;
        instance.device = Some(device);
        instance.port = Some(port);
        instance.started_at = Some(Utc::now());
        instance.child = Some(child);
        instance.log_rx = Some(log_rx);
        instance.health_endpoint = manifest.interface.health_endpoint.clone();
        instance.ready_timeout_secs = manifest.interface.ready_timeout_secs.unwrap_or(30);

        Ok(())
    }

    /// 停止模块服务，kill 子进程。
    pub async fn stop_module(&mut self, module_id: &str) -> Result<()> {
        let instance = self
            .instances
            .get_mut(module_id)
            .ok_or_else(|| anyhow::anyhow!("module '{}' not found", module_id))?;

        if let Some(ref mut child) = instance.child {
            debug!(module_id, "killing child process");
            let _ = child.kill().await;
            let _ = child.wait().await; // reap zombie
        }

        instance.status = ServiceStatus::Stopped;
        instance.child = None;
        instance.started_at = None;
        info!(module_id, "module stopped");
        Ok(())
    }

    /// 检查子进程是否意外退出；对 Starting 状态的实例执行健康检查轮询。
    ///
    /// H2: Starting → Running 转换现在依赖 /health 端点返回 200，
    /// 而非仅检查进程是否存活。
    pub async fn monitor_process(&mut self, module_id: &str) -> Result<()> {
        // 先轮询日志 channel，将新行写入 log_buffer
        self.poll_logs(module_id);

        let instance = self
            .instances
            .get_mut(module_id)
            .ok_or_else(|| anyhow::anyhow!("module '{}' not found", module_id))?;

        if let Some(ref mut child) = instance.child {
            match child.try_wait() {
                Ok(Some(status)) => {
                    warn!(module_id, ?status, "child process exited unexpectedly");
                    instance.status = ServiceStatus::Error(format!(
                        "process exited with status: {}",
                        status
                    ));
                    instance.child = None;
                }
                Ok(None) => {
                    // 进程仍在运行
                    if instance.status == ServiceStatus::Starting {
                        // H2: 执行健康检查
                        if let Some(port) = instance.port {
                            let endpoint = instance
                                .health_endpoint
                                .clone()
                                .unwrap_or_else(|| "/health".to_string());
                            let timeout = Duration::from_secs(1); // 单次探测超时
                            match check_health(port, &endpoint, timeout).await {
                                HealthStatus::Healthy => {
                                    info!(module_id, "health check passed → Running");
                                    instance.status = ServiceStatus::Running;
                                }
                                _ => {
                                    // 尚未就绪，检查是否超过总超时
                                    let elapsed = instance
                                        .started_at
                                        .map(|t| Utc::now().signed_duration_since(t))
                                        .unwrap_or_default();
                                    let timeout_secs = instance.ready_timeout_secs as i64;
                                    if elapsed.num_seconds() > timeout_secs {
                                        warn!(
                                            module_id,
                                            elapsed_secs = elapsed.num_seconds(),
                                            timeout_secs,
                                            "health check timeout"
                                        );
                                        instance.status = ServiceStatus::Error(format!(
                                            "health check timed out after {}s",
                                            timeout_secs
                                        ));
                                    }
                                    // 否则保持 Starting，下次轮询再试
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    error!(module_id, %e, "error checking child process");
                    instance.status = ServiceStatus::Error(format!("monitor error: {}", e));
                    instance.child = None;
                }
            }
        }

        Ok(())
    }

    /// 从日志 channel 中取出所有待处理行，写入 log_buffer 环形缓冲区
    pub fn poll_logs(&mut self, module_id: &str) {
        if let Some(instance) = self.instances.get_mut(module_id) {
            if let Some(rx) = instance.log_rx.as_mut() {
                while let Ok(line) = rx.try_recv() {
                    if instance.log_buffer.len() >= MAX_LOG_LINES {
                        instance.log_buffer.pop_front();
                    }
                    instance.log_buffer.push_back(line);
                }
            }
        }
    }

    /// 查询模块当前状态
    pub fn get_status(&self, module_id: &str) -> Option<&ServiceStatus> {
        self.instances.get(module_id).map(|i| &i.status)
    }

    /// 获取模块实例的完整引用
    pub fn get_instance(&self, module_id: &str) -> Option<&ServiceInstance> {
        self.instances.get(module_id)
    }

    /// 列出所有正在运行（Running 或 Starting）的实例
    pub fn list_running(&self) -> Vec<&ServiceInstance> {
        self.instances
            .values()
            .filter(|i| i.status.is_running() || i.status == ServiceStatus::Starting)
            .collect()
    }

    /// 追加一行日志到模块的环形缓冲区（最多保留 500 行）
    pub fn append_log(&mut self, module_id: &str, line: String) {
        if let Some(instance) = self.instances.get_mut(module_id) {
            if instance.log_buffer.len() >= MAX_LOG_LINES {
                instance.log_buffer.pop_front();
            }
            instance.log_buffer.push_back(line);
        }
    }

    /// 构建启动命令：对 manifest.runtime.start_command 模板执行变量替换。
    ///
    /// 支持的变量：`{root}`, `{module_dir}`, `{model_dir}`, `{port}`, `{device}`,
    /// `{device_index}`, `{backend}`, `{entrypoint}`, `{binary}`, `{input}`, `{output}`
    pub fn build_start_command(manifest: &ModuleManifest, vars: &HashMap<String, String>) -> String {
        let template = manifest
            .runtime
            .start_command
            .clone()
            .unwrap_or_default();

        let mut result = template;
        for (key, value) in vars {
            let placeholder = format!("{{{key}}}");
            result = result.replace(&placeholder, value);
        }
        result
    }
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::manifest::*;
    use crate::types::{ComputeBackend, ModuleCategory};

    /// 构造一个测试用 manifest
    fn test_manifest(start_command: Option<&str>) -> ModuleManifest {
        ModuleManifest {
            module: ModuleInfo {
                id: "test-mod".to_string(),
                name: "Test Module".to_string(),
                version: "0.1.0".to_string(),
                description: "A test module".to_string(),
                category: ModuleCategory::Custom,
                genre: "test".to_string(),
                authors: vec![],
                license: None,
                homepage: None,
                tags: vec![],
            },
            runtime: RuntimeConfig {
                runtime_type: RuntimeType::Python,
                python_version: Some(">=3.10".to_string()),
                requirements: None,
                entrypoint: Some("adapter.py".to_string()),
                start_command: start_command.map(|s| s.to_string()),
                binaries: None,
            },
            compute: ComputeConfig {
                backends: vec![ComputeBackend::Cuda, ComputeBackend::Cpu],
                default_backend: Some(ComputeBackend::Cuda),
                vram_estimate_mb: None,
                min_vram_mb: None,
                env: None,
            },
            models: vec![],
            interface: InterfaceConfig {
                interface_type: InterfaceType::Http,
                health_endpoint: Some("/health".to_string()),
                ready_timeout_secs: Some(30),
                working_dir: None,
                capabilities: vec![],
            },
        }
    }

    #[test]
    fn test_build_start_command_substitution() {
        let manifest = test_manifest(Some(
            "python {entrypoint} --port {port} --device {device} --backend {backend} --model-dir {model_dir}",
        ));

        let mut vars = HashMap::new();
        vars.insert("port".to_string(), "18000".to_string());
        vars.insert("device".to_string(), "cuda:0".to_string());
        vars.insert("device_index".to_string(), "0".to_string());
        vars.insert("backend".to_string(), "cuda".to_string());
        vars.insert("entrypoint".to_string(), "adapter.py".to_string());
        vars.insert("model_dir".to_string(), "/models/whisper".to_string());
        vars.insert("root".to_string(), "/opt/ep".to_string());

        let cmd = ProcessManager::build_start_command(&manifest, &vars);
        assert_eq!(
            cmd,
            "python adapter.py --port 18000 --device cuda:0 --backend cuda --model-dir /models/whisper"
        );
    }

    #[test]
    fn test_build_start_command_all_vars() {
        let manifest = test_manifest(Some(
            "{binary} --root {root} --module-dir {module_dir} --model-dir {model_dir} \
             --port {port} --device {device} --device-index {device_index} \
             --backend {backend} --entry {entrypoint} --input {input} --output {output}",
        ));

        let mut vars = HashMap::new();
        vars.insert("root".to_string(), "/ep".to_string());
        vars.insert("module_dir".to_string(), "/ep/modules/test".to_string());
        vars.insert("model_dir".to_string(), "/ep/models/test".to_string());
        vars.insert("port".to_string(), "18080".to_string());
        vars.insert("device".to_string(), "cpu".to_string());
        vars.insert("device_index".to_string(), "".to_string());
        vars.insert("backend".to_string(), "cpu".to_string());
        vars.insert("entrypoint".to_string(), "main.py".to_string());
        vars.insert("binary".to_string(), "/ep/bin/tool.exe".to_string());
        vars.insert("input".to_string(), "audio.wav".to_string());
        vars.insert("output".to_string(), "result.json".to_string());

        let cmd = ProcessManager::build_start_command(&manifest, &vars);
        assert!(cmd.contains("/ep/bin/tool.exe"));
        assert!(cmd.contains("--root /ep"));
        assert!(cmd.contains("--port 18080"));
        assert!(cmd.contains("--input audio.wav"));
        assert!(cmd.contains("--output result.json"));
        // 确保没有残留的 {placeholder}
        assert!(!cmd.contains('{'));
    }

    #[test]
    fn test_build_start_command_no_template() {
        let manifest = test_manifest(None);
        let vars = HashMap::new();
        let cmd = ProcessManager::build_start_command(&manifest, &vars);
        assert_eq!(cmd, "");
    }

    #[tokio::test]
    async fn test_start_and_stop_module() {
        let mut pm = ProcessManager::new();
        let manifest = test_manifest(Some("echo hello"));
        let device = DeviceId::Cuda(0);
        let env = HashMap::new();

        pm.start_module("test-mod", &manifest, device, 18000, env)
            .await
            .unwrap();

        assert_eq!(
            pm.get_status("test-mod"),
            Some(&ServiceStatus::Starting)
        );
        let inst = pm.get_instance("test-mod").unwrap();
        assert_eq!(inst.port, Some(18000));
        assert_eq!(inst.device, Some(DeviceId::Cuda(0)));
        assert!(inst.started_at.is_some());
        assert!(inst.child.is_some());

        // 停止
        pm.stop_module("test-mod").await.unwrap();
        assert_eq!(pm.get_status("test-mod"), Some(&ServiceStatus::Stopped));
        let inst = pm.get_instance("test-mod").unwrap();
        assert!(inst.child.is_none());
        assert!(inst.started_at.is_none());
    }

    #[tokio::test]
    async fn test_start_already_running() {
        let mut pm = ProcessManager::new();
        let manifest = test_manifest(Some("sleep 30"));
        let device = DeviceId::Cpu;
        let env = HashMap::new();

        pm.start_module("mod-a", &manifest, device.clone(), 18000, env.clone())
            .await
            .unwrap();

        // 再次启动应报错
        let result = pm
            .start_module("mod-a", &manifest, device, 18001, env)
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already running"));

        // cleanup
        pm.stop_module("mod-a").await.unwrap();
    }

    #[tokio::test]
    async fn test_stop_nonexistent() {
        let mut pm = ProcessManager::new();
        let result = pm.stop_module("ghost").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_list_running() {
        let mut pm = ProcessManager::new();
        let manifest = test_manifest(Some("sleep 30"));
        let env = HashMap::new();

        pm.start_module("mod-a", &manifest, DeviceId::Cpu, 18000, env.clone())
            .await
            .unwrap();
        pm.start_module("mod-b", &manifest, DeviceId::Cpu, 18001, env.clone())
            .await
            .unwrap();

        assert_eq!(pm.list_running().len(), 2);

        pm.stop_module("mod-a").await.unwrap();
        assert_eq!(pm.list_running().len(), 1);
        assert_eq!(pm.list_running()[0].module_id, "mod-b");

        pm.stop_module("mod-b").await.unwrap();
    }

    #[tokio::test]
    async fn test_append_log_ring_buffer() {
        let mut pm = ProcessManager::new();
        let manifest = test_manifest(Some("echo hello"));
        pm.start_module("mod-a", &manifest, DeviceId::Cpu, 18000, HashMap::new())
            .await
            .unwrap();

        // 写入 600 行
        for i in 0..600 {
            pm.append_log("mod-a", format!("line-{i}"));
        }

        let inst = pm.get_instance("mod-a").unwrap();
        assert_eq!(inst.log_buffer.len(), 500);
        // 最旧的 100 行应已被移除
        assert_eq!(inst.log_buffer.front().unwrap(), "line-100");
        assert_eq!(inst.log_buffer.back().unwrap(), "line-599");

        pm.stop_module("mod-a").await.unwrap();
    }

    #[tokio::test]
    async fn test_append_log_nonexistent_module() {
        let mut pm = ProcessManager::new();
        // 不应 panic
        pm.append_log("ghost", "hello".to_string());
    }

    // ─── New async process tests ────────────────────────────────────────────

    #[tokio::test]
    async fn test_spawn_and_kill() {
        let mut pm = ProcessManager::new();
        // Use a long-running command
        let manifest = test_manifest(Some("sleep 60"));
        let env = HashMap::new();

        pm.start_module("long-runner", &manifest, DeviceId::Cpu, 19000, env)
            .await
            .unwrap();

        let inst = pm.get_instance("long-runner").unwrap();
        assert!(inst.child.is_some());
        assert!(inst.pid().is_some());

        // Kill it
        pm.stop_module("long-runner").await.unwrap();
        let inst = pm.get_instance("long-runner").unwrap();
        assert!(inst.child.is_none());
        assert_eq!(pm.get_status("long-runner"), Some(&ServiceStatus::Stopped));
    }

    #[tokio::test]
    async fn test_stdout_capture() {
        // Verify we can spawn a command that produces output and it doesn't deadlock
        let mut pm = ProcessManager::new();
        let manifest = test_manifest(Some("echo hello_from_test"));
        let env = HashMap::new();

        pm.start_module("echo-mod", &manifest, DeviceId::Cpu, 19001, env)
            .await
            .unwrap();

        // Wait a bit for the process to finish (echo is fast)
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Monitor should detect it exited
        pm.monitor_process("echo-mod").await.unwrap();
        let status = pm.get_status("echo-mod").unwrap();
        // Process should have exited (either Error with exit status or still Starting depending on timing)
        // The key is it doesn't hang
        let inst = pm.get_instance("echo-mod").unwrap();
        // If it exited, child should be None
        if !matches!(status, ServiceStatus::Starting) {
            assert!(inst.child.is_none());
        }

        // cleanup
        let _ = pm.stop_module("echo-mod").await;
    }

    #[tokio::test]
    async fn test_monitor_detects_exit() {
        let mut pm = ProcessManager::new();
        // Start a short-lived process
        let manifest = test_manifest(Some("echo done"));
        let env = HashMap::new();

        pm.start_module("short-lived", &manifest, DeviceId::Cpu, 19002, env)
            .await
            .unwrap();

        // Wait for it to exit
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Monitor should detect exit
        pm.monitor_process("short-lived").await.unwrap();
        let status = pm.get_status("short-lived").unwrap();
        // Should be Error (exited) or still Starting->Running transition
        // Since echo exits fast, it should be Error
        assert!(
            matches!(status, ServiceStatus::Error(_)) || matches!(status, ServiceStatus::Starting),
            "expected Error or Starting, got {:?}",
            status
        );
    }

    #[tokio::test]
    async fn test_multiple_modules() {
        let mut pm = ProcessManager::new();
        let manifest = test_manifest(Some("sleep 30"));
        let env = HashMap::new();

        pm.start_module("mod-x", &manifest, DeviceId::Cpu, 19010, env.clone())
            .await
            .unwrap();
        pm.start_module("mod-y", &manifest, DeviceId::Cuda(0), 19011, env.clone())
            .await
            .unwrap();

        assert_eq!(pm.list_running().len(), 2);

        let inst_x = pm.get_instance("mod-x").unwrap();
        let inst_y = pm.get_instance("mod-y").unwrap();
        assert!(inst_x.child.is_some());
        assert!(inst_y.child.is_some());
        assert_ne!(inst_x.pid(), inst_y.pid());

        pm.stop_module("mod-x").await.unwrap();
        pm.stop_module("mod-y").await.unwrap();
        assert_eq!(pm.list_running().len(), 0);
    }

    #[tokio::test]
    async fn test_stop_cleans_up() {
        let mut pm = ProcessManager::new();
        let manifest = test_manifest(Some("sleep 60"));
        let env = HashMap::new();

        pm.start_module("cleanup-mod", &manifest, DeviceId::Cpu, 19020, env)
            .await
            .unwrap();

        // Verify child handle exists
        assert!(pm.get_instance("cleanup-mod").unwrap().child.is_some());

        // Stop it
        pm.stop_module("cleanup-mod").await.unwrap();

        // Verify child handle is None
        let inst = pm.get_instance("cleanup-mod").unwrap();
        assert!(inst.child.is_none());
        assert!(inst.started_at.is_none());
        assert_eq!(
            pm.get_status("cleanup-mod"),
            Some(&ServiceStatus::Stopped)
        );
    }

    // ─── A2 环境注入：CUDA 库路径（§3.1/§15.3 双平台分支） ─────────────────

    #[test]
    fn test_compose_lib_path_linux_prepends_colon() {
        let (key, value) = compose_lib_path("/opt/ep/runtime/cuda-libs", false, "/usr/local/lib");
        assert_eq!(key, "LD_LIBRARY_PATH");
        assert_eq!(value, "/opt/ep/runtime/cuda-libs:/usr/local/lib");
    }

    #[test]
    fn test_compose_lib_path_windows_prepends_semicolon() {
        let (key, value) = compose_lib_path(
            "C:\\ep\\runtime\\cuda-libs",
            true,
            "C:\\Windows\\System32",
        );
        assert_eq!(key, "PATH");
        assert_eq!(value, "C:\\ep\\runtime\\cuda-libs;C:\\Windows\\System32");
    }

    #[test]
    fn test_compose_lib_path_empty_inherited() {
        let (k1, v1) = compose_lib_path("/x", false, "");
        assert_eq!(k1, "LD_LIBRARY_PATH");
        assert_eq!(v1, "/x");
        let (k2, v2) = compose_lib_path("C:\\x", true, "");
        assert_eq!(k2, "PATH");
        assert_eq!(v2, "C:\\x");
    }

    #[test]
    fn test_cuda_lib_path_env_host_platform_and_empty() {
        // 空目录 → 不注入
        assert!(cuda_lib_path_env(Path::new("")).is_none());

        let dir = if cfg!(windows) {
            PathBuf::from("C:\\ep\\runtime\\cuda-libs")
        } else {
            PathBuf::from("/opt/ep/runtime/cuda-libs")
        };
        let (key, value) = cuda_lib_path_env(&dir).unwrap();
        let dir_str = dir.to_string_lossy().to_string();
        if cfg!(windows) {
            // Windows: PATH 前置，分号分隔
            assert_eq!(key, "PATH");
            assert!(
                value == dir_str || value.starts_with(&format!("{dir_str};")),
                "value: {value}"
            );
        } else {
            // Linux: LD_LIBRARY_PATH 前置，冒号分隔
            assert_eq!(key, "LD_LIBRARY_PATH");
            assert!(
                value == dir_str || value.starts_with(&format!("{dir_str}:")),
                "value: {value}"
            );
        }
    }

    #[test]
    fn test_resolve_cuda_libs_dir_relative_absolute_empty() {
        let root = if cfg!(windows) {
            PathBuf::from("C:/ep")
        } else {
            PathBuf::from("/opt/ep")
        };
        // 相对 root 解析（Path::join）
        assert_eq!(
            resolve_cuda_libs_dir(&root, "runtime/cuda-libs"),
            root.join("runtime").join("cuda-libs")
        );
        // 绝对路径原样返回
        let abs = if cfg!(windows) { "D:/cuda-libs" } else { "/srv/cuda-libs" };
        assert_eq!(resolve_cuda_libs_dir(&root, abs), PathBuf::from(abs));
        // 空值 = 禁用注入
        assert_eq!(resolve_cuda_libs_dir(&root, ""), PathBuf::new());
        assert_eq!(resolve_cuda_libs_dir(&root, "   "), PathBuf::new());
    }

    #[test]
    fn test_venv_python_path_platform_branch() {
        let root = if cfg!(windows) {
            PathBuf::from("C:/ep")
        } else {
            PathBuf::from("/opt/ep")
        };
        let p = venv_python_path(&root, "demo");
        if cfg!(windows) {
            assert_eq!(
                p,
                root.join("runtime")
                    .join("venvs")
                    .join("demo")
                    .join("Scripts")
                    .join("python.exe")
            );
        } else {
            assert_eq!(
                p,
                root.join("runtime")
                    .join("venvs")
                    .join("demo")
                    .join("bin")
                    .join("python")
            );
        }
    }

    // ─── A2 环境注入：compute.env 接线（{device_index} 替换） ───────────────

    #[test]
    fn test_backend_env_vars_device_index_substitution() {
        let mut manifest = test_manifest(None);
        manifest.compute.env = Some(HashMap::from([(
            "cuda".to_string(),
            HashMap::from([(
                "CUDA_VISIBLE_DEVICES".to_string(),
                "{device_index}".to_string(),
            )]),
        )]));

        let vars = backend_env_vars(&manifest, &DeviceId::Cuda(2));
        assert_eq!(
            vars,
            vec![("CUDA_VISIBLE_DEVICES".to_string(), "2".to_string())]
        );
    }

    #[test]
    fn test_backend_env_vars_multi_entry_and_other_backend() {
        let mut manifest = test_manifest(None);
        manifest.compute.env = Some(HashMap::from([
            (
                "cuda".to_string(),
                HashMap::from([
                    (
                        "CUDA_VISIBLE_DEVICES".to_string(),
                        "{device_index}".to_string(),
                    ),
                    ("EP_TORCH".to_string(), "gpu-{device_index}".to_string()),
                ]),
            ),
            (
                "cpu".to_string(),
                HashMap::from([("OMP_NUM_THREADS".to_string(), "4".to_string())]),
            ),
        ]));

        // cuda 后端：{device_index} 逐个替换
        let cuda_vars = backend_env_vars(&manifest, &DeviceId::Cuda(1));
        assert_eq!(cuda_vars.len(), 2);
        assert!(cuda_vars.contains(&("CUDA_VISIBLE_DEVICES".to_string(), "1".to_string())));
        assert!(cuda_vars.contains(&("EP_TORCH".to_string(), "gpu-1".to_string())));

        // cpu 后端：只取 cpu 表
        let cpu_vars = backend_env_vars(&manifest, &DeviceId::Cpu);
        assert_eq!(cpu_vars, vec![("OMP_NUM_THREADS".to_string(), "4".to_string())]);
    }

    #[test]
    fn test_backend_env_vars_missing_table_skips() {
        // compute.env = None → 防御性跳过
        let manifest = test_manifest(None);
        assert!(backend_env_vars(&manifest, &DeviceId::Cuda(0)).is_empty());

        // 当前 backend 无条目 → 跳过
        let mut manifest2 = test_manifest(None);
        manifest2.compute.env = Some(HashMap::from([(
            "cpu".to_string(),
            HashMap::from([("OMP_NUM_THREADS".to_string(), "4".to_string())]),
        )]));
        assert!(backend_env_vars(&manifest2, &DeviceId::Cuda(0)).is_empty());

        // 无数字索引的设备（OpenVINO 字符串索引）→ {device_index} 替换为空串
        let mut manifest3 = test_manifest(None);
        manifest3.compute.env = Some(HashMap::from([(
            "openvino".to_string(),
            HashMap::from([("OV_DEVICE".to_string(), "npu-{device_index}".to_string())]),
        )]));
        let vars = backend_env_vars(&manifest3, &DeviceId::OpenVINO("npu0".to_string()));
        assert_eq!(vars, vec![("OV_DEVICE".to_string(), "npu-".to_string())]);
    }

    // ─── A2 环境注入：build_module_env 公共构建函数（P0-4 前置） ────────────

    #[test]
    fn test_build_module_env_standard_vars() {
        let manifest = test_manifest(None); // 无 models
        let root = if cfg!(windows) {
            PathBuf::from("C:/ep")
        } else {
            PathBuf::from("/opt/ep")
        };
        let vars = build_module_env(
            &root,
            &crate::config::AppConfig::default(),
            "test-mod",
            &manifest,
            &DeviceId::Cuda(1),
        );

        assert_eq!(vars.get("ROOT").unwrap(), &root.to_string_lossy().to_string());
        assert_eq!(vars.get("MODULE_ID").unwrap(), "test-mod");
        assert_eq!(
            vars.get("MODULE_DIR").unwrap(),
            &root.join("modules").join("test-mod").to_string_lossy().to_string()
        );
        // 无 models → MODEL_DIR 回退 MODULE_DIR，且无 MODEL_ID
        assert_eq!(vars.get("MODEL_DIR").unwrap(), vars.get("MODULE_DIR").unwrap());
        assert!(!vars.contains_key("MODEL_ID"));
        assert_eq!(
            vars.get("WORKSPACE").unwrap(),
            &root.join("workspace").to_string_lossy().to_string()
        );
        assert_eq!(vars.get("LOG_LEVEL").unwrap(), "info");
        assert_eq!(vars.get("DEVICE").unwrap(), "cuda:1");
        assert_eq!(vars.get("BACKEND").unwrap(), "cuda");
        assert_eq!(vars.get("DEVICE_INDEX").unwrap(), "1");

        // 键必须是裸占位符名（EP_ 前缀由 start_module 统一加一次）
        assert!(vars.keys().all(|k| !k.starts_with("EP_")));
    }

    #[test]
    fn test_build_module_env_with_default_model() {
        let mut manifest = test_manifest(None);
        manifest.models = vec![
            ModelDecl {
                id: "small".to_string(),
                name: "Small".to_string(),
                source: ModelSource::Huggingface,
                repo_id: Some("org/small".to_string()),
                url: None,
                target_dir: "small-dir".to_string(),
                revision: None,
                size_estimate_mb: None,
                default: false,
                mirrors: vec![],
                qualified_id: None,
                vram_estimate_mb: None,
            },
            ModelDecl {
                id: "large".to_string(),
                name: "Large".to_string(),
                source: ModelSource::Huggingface,
                repo_id: Some("org/large".to_string()),
                url: None,
                target_dir: "large-dir".to_string(),
                revision: None,
                size_estimate_mb: None,
                default: true,
                mirrors: vec![],
                qualified_id: None,
                vram_estimate_mb: None,
            },
        ];

        let root = if cfg!(windows) {
            PathBuf::from("C:/ep")
        } else {
            PathBuf::from("/opt/ep")
        };
        let vars = build_module_env(
            &root,
            &crate::config::AppConfig::default(),
            "m",
            &manifest,
            &DeviceId::Cpu,
        );
        // default=true 的模型优先
        assert_eq!(vars.get("MODEL_ID").unwrap(), "large");
        assert_eq!(
            vars.get("MODEL_DIR").unwrap(),
            &root.join("models").join("large-dir").to_string_lossy().to_string()
        );
    }

    // ─── P0-3 回归：EP_ 前缀不叠加 + 占位符不残留 ──────────────────────────

    #[test]
    fn test_p0_3_prefixed_env_keys_no_double_prefix_no_residue() {
        // 模拟旧 --run-module 传入的带 EP_ 前缀 env map
        let mut env = HashMap::new();
        env.insert("EP_ROOT".to_string(), "/opt/ep".to_string());
        env.insert("EP_MODULE_DIR".to_string(), "/opt/ep/modules/demo".to_string());
        env.insert("EP_MODULE_ID".to_string(), "demo".to_string());
        env.insert("EP_MODEL_DIR".to_string(), "/opt/ep/models/demo".to_string());
        env.insert("EP_WORKSPACE".to_string(), "/opt/ep/workspace".to_string());
        env.insert("EP_LOG_LEVEL".to_string(), "info".to_string());

        // 与 modules/*/module.toml 一致的 start_command 模板
        let manifest = test_manifest(Some("{venv_python} {MODULE_DIR}/{entrypoint}"));
        let vars = prepare_template_vars("demo", &manifest, &DeviceId::Cuda(0), 18000, env);

        // ① 归一后键无 EP_ 前缀 → start_module 加前缀后不产生 EP_EP_*
        for key in vars.keys() {
            assert!(!key.starts_with("EP_"), "key '{key}' 应已剥离 EP_ 前缀");
            let env_key = format!("EP_{}", key.to_uppercase());
            assert!(!env_key.starts_with("EP_EP_"), "双重前缀: {env_key}");
        }
        // 值保持不变
        assert_eq!(vars.get("MODULE_DIR").map(String::as_str), Some("/opt/ep/modules/demo"));
        assert_eq!(vars.get("ROOT").map(String::as_str), Some("/opt/ep"));

        // ② 占位符键名对齐：{MODULE_DIR}/{venv_python}/{entrypoint} 全部替换生效
        assert!(vars.contains_key("venv_python"), "ROOT 存在时应计算 venv_python");
        let cmd = ProcessManager::build_start_command(&manifest, &vars);
        assert!(!cmd.contains('{'), "残留占位符: {cmd}");
        assert!(!cmd.contains('}'), "残留占位符: {cmd}");
        assert!(cmd.contains("/opt/ep/modules/demo/adapter.py"), "cmd: {cmd}");
    }

    #[test]
    fn test_prepare_template_vars_bare_keys_and_builtins() {
        // daemon API 风格：裸大写键
        let mut env = HashMap::new();
        env.insert("ROOT".to_string(), "/opt/ep".to_string());
        env.insert("MODULE_DIR".to_string(), "/opt/ep/modules/m".to_string());

        let manifest = test_manifest(Some(
            "{MODULE_DIR} {module_dir} {root} {port} {device} {device_index} {backend} {entrypoint}",
        ));
        let vars = prepare_template_vars("m", &manifest, &DeviceId::Cuda(3), 18123, env);

        // 内置变量
        assert_eq!(vars.get("port").unwrap(), "18123");
        assert_eq!(vars.get("device").unwrap(), "cuda:3");
        assert_eq!(vars.get("device_index").unwrap(), "3");
        assert_eq!(vars.get("backend").unwrap(), "cuda");
        assert_eq!(vars.get("entrypoint").unwrap(), "adapter.py");
        assert!(vars.contains_key("venv_python"));
        // 大小写别名：{module_dir}/{root} 也能命中
        assert_eq!(vars.get("module_dir").unwrap(), "/opt/ep/modules/m");
        assert_eq!(vars.get("root").unwrap(), "/opt/ep");

        let cmd = ProcessManager::build_start_command(&manifest, &vars);
        assert!(!cmd.contains('{'), "残留占位符: {cmd}");
    }

    #[test]
    fn test_prepare_template_vars_no_root_no_venv_python() {
        // 无 ROOT → 不计算 venv_python（防御，不 panic）
        let manifest = test_manifest(Some("{entrypoint}"));
        let vars = prepare_template_vars("m", &manifest, &DeviceId::Cpu, 18000, HashMap::new());
        assert!(!vars.contains_key("venv_python"));
        assert_eq!(vars.get("backend").unwrap(), "cpu");
    }
}
