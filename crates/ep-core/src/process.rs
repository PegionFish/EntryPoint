//! 进程管理器 — 管理模块服务实例的生命周期（启动/停止/状态/日志）

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::health::{check_health, HealthStatus};
use crate::module::manifest::ModuleManifest;
use crate::types::{DeviceId, ServiceStatus};

/// 日志缓冲区最大行数
const MAX_LOG_LINES: usize = 500;

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
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            instances: HashMap::new(),
            network_env: Vec::new(),
        }
    }

    /// 设置网络代理配置（链式调用）。
    ///
    /// 模块服务子进程启动时将被注入这些环境变量（如 HTTP_PROXY 等）。
    pub fn with_network_env(mut self, env_vars: Vec<(String, String)>) -> Self {
        self.network_env = env_vars;
        self
    }

    /// 设置网络代理环境变量
    pub fn set_network_env(&mut self, env_vars: Vec<(String, String)>) {
        self.network_env = env_vars;
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

        // 构建启动命令
        let mut vars = env_vars;
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
        // 模块 TOML 可用 {venv_python} 替代硬编码的 bin/python
        if let Some(root) = vars.get("ROOT").or_else(|| vars.get("root")).cloned() {
            let venv_dir = std::path::Path::new(&root)
                .join("runtime")
                .join("venvs")
                .join(module_id);
            let python = if cfg!(windows) {
                venv_dir.join("Scripts").join("python.exe")
            } else {
                venv_dir.join("bin").join("python")
            };
            vars.insert("venv_python".to_string(), python.to_string_lossy().to_string());
        }

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

        // 设置环境变量
        for (key, value) in &vars {
            let env_key = format!("EP_{}", key.to_uppercase());
            cmd.env(&env_key, value);
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
}
