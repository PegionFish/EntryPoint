//! 进程管理器 — 管理模块服务实例的生命周期（启动/停止/状态/日志）

use std::collections::{HashMap, VecDeque};

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use tracing::{debug, info, warn};

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
    // TODO: 实际实现中替换为 tokio::process::Child handle
    // 当前编译环境无 tokio runtime，用 PID 占位
    pub pid: Option<u32>,
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
            pid: None,
        }
    }
}

// ─── ProcessManager ──────────────────────────────────────────────────────────

/// 进程管理器：跟踪所有模块服务实例
pub struct ProcessManager {
    instances: HashMap<String, ServiceInstance>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            instances: HashMap::new(),
        }
    }

    /// 启动模块服务。
    ///
    /// 构建启动命令、记录实例信息并设置状态为 `Starting`。
    /// 当前不实际 spawn 子进程（TODO: 集成 tokio::process）。
    pub fn start_module(
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

        let command = Self::build_start_command(manifest, &vars);
        info!(module_id, %command, "built start command");

        // TODO: 实际 spawn 子进程
        // let child = tokio::process::Command::new(...)
        //     .envs(&env_vars)
        //     .spawn()?;
        // let pid = child.id();

        debug!(module_id, "recording instance (spawn deferred — TODO)");

        let instance = self
            .instances
            .entry(module_id.to_string())
            .or_insert_with(|| ServiceInstance::new(module_id));

        instance.status = ServiceStatus::Starting;
        instance.device = Some(device);
        instance.port = Some(port);
        instance.started_at = Some(Utc::now());
        instance.pid = None; // TODO: 实际 PID

        Ok(())
    }

    /// 停止模块服务，将状态设为 Stopped。
    pub fn stop_module(&mut self, module_id: &str) -> Result<()> {
        let instance = self
            .instances
            .get_mut(module_id)
            .ok_or_else(|| anyhow::anyhow!("module '{}' not found", module_id))?;

        // TODO: 实际 kill 子进程 (child.kill())
        if instance.pid.is_some() {
            warn!(module_id, "TODO: kill child process");
        }

        instance.status = ServiceStatus::Stopped;
        instance.pid = None;
        instance.started_at = None;
        info!(module_id, "module stopped");
        Ok(())
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

    #[test]
    fn test_start_and_stop_module() {
        let mut pm = ProcessManager::new();
        let manifest = test_manifest(Some("python {entrypoint} --port {port}"));
        let device = DeviceId::Cuda(0);
        let env = HashMap::new();

        pm.start_module("test-mod", &manifest, device, 18000, env)
            .unwrap();

        assert_eq!(
            pm.get_status("test-mod"),
            Some(&ServiceStatus::Starting)
        );
        let inst = pm.get_instance("test-mod").unwrap();
        assert_eq!(inst.port, Some(18000));
        assert_eq!(inst.device, Some(DeviceId::Cuda(0)));
        assert!(inst.started_at.is_some());

        // 停止
        pm.stop_module("test-mod").unwrap();
        assert_eq!(pm.get_status("test-mod"), Some(&ServiceStatus::Stopped));
        let inst = pm.get_instance("test-mod").unwrap();
        assert!(inst.pid.is_none());
        assert!(inst.started_at.is_none());
    }

    #[test]
    fn test_start_already_running() {
        let mut pm = ProcessManager::new();
        let manifest = test_manifest(Some("run"));
        let device = DeviceId::Cpu;
        let env = HashMap::new();

        pm.start_module("mod-a", &manifest, device.clone(), 18000, env.clone())
            .unwrap();

        // 再次启动应报错
        let result = pm.start_module("mod-a", &manifest, device, 18001, env);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already running"));
    }

    #[test]
    fn test_stop_nonexistent() {
        let mut pm = ProcessManager::new();
        let result = pm.stop_module("ghost");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_list_running() {
        let mut pm = ProcessManager::new();
        let manifest = test_manifest(Some("run"));
        let env = HashMap::new();

        pm.start_module("mod-a", &manifest, DeviceId::Cpu, 18000, env.clone())
            .unwrap();
        pm.start_module("mod-b", &manifest, DeviceId::Cpu, 18001, env.clone())
            .unwrap();

        assert_eq!(pm.list_running().len(), 2);

        pm.stop_module("mod-a").unwrap();
        assert_eq!(pm.list_running().len(), 1);
        assert_eq!(pm.list_running()[0].module_id, "mod-b");
    }

    #[test]
    fn test_append_log_ring_buffer() {
        let mut pm = ProcessManager::new();
        let manifest = test_manifest(Some("run"));
        pm.start_module("mod-a", &manifest, DeviceId::Cpu, 18000, HashMap::new())
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
    }

    #[test]
    fn test_append_log_nonexistent_module() {
        let mut pm = ProcessManager::new();
        // 不应 panic
        pm.append_log("ghost", "hello".to_string());
    }
}
