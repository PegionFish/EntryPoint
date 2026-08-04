//! 模块生命周期管理 — Wave 2 Agent E 实现
//!
//! 编排模块从发现到运行的完整流程。

use std::collections::HashMap;

use anyhow::{bail, Result};
use tracing::{debug, info};

use crate::config::AppConfig;
use crate::env::EnvManager;
use crate::model::ModelManager;

use super::discovery::DiscoveredModule;

// ─── ModuleReadiness ─────────────────────────────────────────────────────────

/// 模块就绪状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleReadiness {
    /// 缺少运行环境（Python/venv）
    MissingEnv,
    /// 缺少模型文件
    MissingModel,
    /// 就绪，可以启动
    Ready,
    /// 正在运行
    Running,
}

// ─── ModuleLifecycle ─────────────────────────────────────────────────────────

/// 模块生命周期管理器
///
/// 编排模块从发现到就绪的完整流程：
/// discover → validate → check_env → setup_env → check_model → download_model → ready
///
/// ProcessManager 通过引用传入，不在此持有。
pub struct ModuleLifecycle {
    env_manager: EnvManager,
    model_manager: ModelManager,
}

impl ModuleLifecycle {
    /// 创建 ModuleLifecycle
    pub fn new(env_manager: EnvManager, model_manager: ModelManager) -> Self {
        Self {
            env_manager,
            model_manager,
        }
    }

    /// 检查模块就绪状态（同步，不执行安装/下载）
    pub fn get_readiness(
        &self,
        module: &DiscoveredModule,
        _config: &AppConfig,
    ) -> ModuleReadiness {
        let manifest = match &module.manifest {
            Some(m) => m,
            None => return ModuleReadiness::MissingEnv,
        };

        let module_id = &manifest.module.id;

        // 检查环境
        if manifest.runtime.requirements.is_some() {
            let req_path = module
                .path
                .join(manifest.runtime.requirements.as_deref().unwrap_or("requirements.txt"));
            if !self.env_manager.is_venv_ready(module_id, &req_path) {
                return ModuleReadiness::MissingEnv;
            }
        }

        // 检查模型
        for model in &manifest.models {
            if !self.model_manager.is_model_present(&model.target_dir) {
                return ModuleReadiness::MissingModel;
            }
        }

        ModuleReadiness::Ready
    }

    /// 编排完整生命周期：discover → validate → check_env → setup_env → check_model → download_model → ready
    pub async fn prepare_module(
        &mut self,
        module: &DiscoveredModule,
        config: &AppConfig,
    ) -> Result<ModuleReadiness> {
        let manifest = match &module.manifest {
            Some(m) => m,
            None => bail!("module has no valid manifest"),
        };

        let module_id = &manifest.module.id;

        // 1. 检查/准备环境
        if manifest.runtime.requirements.is_some() {
            let req_path = module
                .path
                .join(manifest.runtime.requirements.as_deref().unwrap_or("requirements.txt"));

            if !self.env_manager.is_venv_ready(module_id, &req_path) {
                info!(module = module_id, "environment not ready, setting up");
                let python_version = manifest
                    .runtime
                    .python_version
                    .as_deref()
                    .unwrap_or(">=3.10");
                self.env_manager
                    .ensure_venv(module_id, python_version, &req_path)?;
            } else {
                debug!(module = module_id, "environment already ready");
            }
        }

        // 2. 检查/下载模型
        for model in &manifest.models {
            if !self.model_manager.is_model_present(&model.target_dir) {
                info!(
                    module = module_id,
                    model = %model.id,
                    "model not present, downloading"
                );
                let venv_python = self.env_manager.venv_python_path(module_id);
                self.model_manager
                    .execute_download(model, &module.path, &venv_python, config)
                    .await?;
            } else {
                debug!(
                    module = module_id,
                    model = %model.id,
                    "model already present"
                );
            }
        }

        Ok(ModuleReadiness::Ready)
    }

    /// 批量检查所有模块的就绪状态
    pub fn check_all_readiness(
        &self,
        modules: &[DiscoveredModule],
        config: &AppConfig,
    ) -> HashMap<String, ModuleReadiness> {
        let mut result = HashMap::new();
        for module in modules {
            if let Some(manifest) = &module.manifest {
                let readiness = self.get_readiness(module, config);
                result.insert(manifest.module.id.clone(), readiness);
            }
        }
        result
    }

    /// 获取 EnvManager 引用
    pub fn env_manager(&self) -> &EnvManager {
        &self.env_manager
    }

    /// 获取 ModelManager 引用
    pub fn model_manager(&self) -> &ModelManager {
        &self.model_manager
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, ModelsConfig, PythonConfig};
    use crate::env::EnvManager;
    use crate::model::ModelManager;
    use crate::module::discovery::{DiscoveredModule, DiscoveryStatus};
    use crate::module::manifest::*;
    use crate::types::{ComputeBackend, ModuleCategory};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn test_config() -> AppConfig {
        AppConfig::default()
    }

    fn test_env_manager(root: &Path) -> EnvManager {
        EnvManager::new(root, &PythonConfig::default())
    }

    fn test_model_manager(root: &Path) -> ModelManager {
        let config = ModelsConfig {
            cache_dir: root.join("models").to_string_lossy().to_string(),
            hf_endpoint: String::new(),
            default_source: "huggingface".to_string(),
            max_concurrent_downloads: 2,
            cache_paths: Vec::new(),
        };
        ModelManager::new(&config, root)
    }

    fn test_manifest(id: &str, has_requirements: bool, has_model: bool) -> ModuleManifest {
        ModuleManifest {
            module: ModuleInfo {
                id: id.to_string(),
                name: format!("Test {id}"),
                version: "1.0.0".to_string(),
                description: "Test module".to_string(),
                category: ModuleCategory::Asr,
                genre: "test".to_string(),
                authors: vec![],
                license: None,
                homepage: None,
                tags: vec![],
            },
            runtime: RuntimeConfig {
                runtime_type: RuntimeType::Python,
                python_version: Some(">=3.10".to_string()),
                requirements: if has_requirements {
                    Some("requirements.txt".to_string())
                } else {
                    None
                },
                entrypoint: Some("adapter.py".to_string()),
                start_command: None,
                binaries: None,
            },
            compute: ComputeConfig {
                backends: vec![ComputeBackend::Cpu],
                default_backend: None,
                vram_estimate_mb: None,
                min_vram_mb: None,
                env: None,
            },
            models: if has_model {
                vec![ModelDecl {
                    id: "test-model".to_string(),
                    name: "Test Model".to_string(),
                    source: ModelSource::Huggingface,
                    repo_id: Some("test/repo".to_string()),
                    url: None,
                    target_dir: "test-model-dir".to_string(),
                    revision: Some("main".to_string()),
                    size_estimate_mb: None,
                    qualified_id: None,
                    vram_estimate_mb: None,
                    default: true,
                    mirrors: vec![],
                }]
            } else {
                vec![]
            },
            interface: InterfaceConfig {
                interface_type: InterfaceType::Http,
                health_endpoint: Some("/health".to_string()),
                ready_timeout_secs: None,
                working_dir: None,
                capabilities: vec![],
            },
        }
    }

    fn make_module(manifest: Option<ModuleManifest>, path: PathBuf) -> DiscoveredModule {
        DiscoveredModule {
            manifest,
            path,
            status: DiscoveryStatus::Valid,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("ep_lifecycle_{}_{}_{}", name, std::process::id(), rand_suffix()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn rand_suffix() -> u32 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        std::time::SystemTime::now().hash(&mut h);
        (h.finish() & 0xFFFF) as u32
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    // ── 1. test_lifecycle_missing_env ─────────────────────────────────────

    #[test]
    fn test_lifecycle_missing_env() {
        let root = temp_dir("missing_env");
        let module_dir = root.join("modules").join("test-mod");
        fs::create_dir_all(&module_dir).unwrap();

        let manifest = test_manifest("test-mod", true, false);
        let module = make_module(Some(manifest), module_dir);

        let env_mgr = test_env_manager(&root);
        let model_mgr = test_model_manager(&root);
        let lifecycle = ModuleLifecycle::new(env_mgr, model_mgr);
        let config = test_config();

        let readiness = lifecycle.get_readiness(&module, &config);
        assert_eq!(readiness, ModuleReadiness::MissingEnv);

        cleanup(&root);
    }

    // ── 2. test_lifecycle_missing_model ───────────────────────────────────

    #[test]
    fn test_lifecycle_missing_model() {
        let root = temp_dir("missing_model");
        let module_dir = root.join("modules").join("test-mod");
        fs::create_dir_all(&module_dir).unwrap();

        // 创建 venv 使其就绪（无 requirements 文件，venv python 存在即可）
        let venv_dir = root.join("runtime").join("venvs").join("test-mod");
        let venv_bin = if cfg!(windows) {
            venv_dir.join("Scripts")
        } else {
            venv_dir.join("bin")
        };
        fs::create_dir_all(&venv_bin).unwrap();
        let python_name = if cfg!(windows) {
            "python.exe"
        } else {
            "python"
        };
        fs::write(venv_bin.join(python_name), "fake").unwrap();

        // 有模型需求但模型不存在
        let manifest = test_manifest("test-mod", true, true);
        let module = make_module(Some(manifest), module_dir);

        let env_mgr = test_env_manager(&root);
        let model_mgr = test_model_manager(&root);
        let lifecycle = ModuleLifecycle::new(env_mgr, model_mgr);
        let config = test_config();

        let readiness = lifecycle.get_readiness(&module, &config);
        assert_eq!(readiness, ModuleReadiness::MissingModel);

        cleanup(&root);
    }

    // ── 3. test_lifecycle_ready ───────────────────────────────────────────

    #[test]
    fn test_lifecycle_ready() {
        let root = temp_dir("ready");
        let module_dir = root.join("modules").join("test-mod");
        fs::create_dir_all(&module_dir).unwrap();

        // 创建 venv
        let venv_dir = root.join("runtime").join("venvs").join("test-mod");
        let venv_bin = if cfg!(windows) {
            venv_dir.join("Scripts")
        } else {
            venv_dir.join("bin")
        };
        fs::create_dir_all(&venv_bin).unwrap();
        let python_name = if cfg!(windows) {
            "python.exe"
        } else {
            "python"
        };
        fs::write(venv_bin.join(python_name), "fake").unwrap();

        // 创建模型目录（非空）
        let model_dir = root.join("models").join("test-model-dir");
        fs::create_dir_all(&model_dir).unwrap();
        fs::write(model_dir.join("model.bin"), b"data").unwrap();

        // 无 requirements 的 manifest（venv 存在即就绪）+ 有模型
        let manifest = test_manifest("test-mod", false, true);
        let module = make_module(Some(manifest), module_dir);

        let env_mgr = test_env_manager(&root);
        let model_mgr = test_model_manager(&root);
        let lifecycle = ModuleLifecycle::new(env_mgr, model_mgr);
        let config = test_config();

        let readiness = lifecycle.get_readiness(&module, &config);
        assert_eq!(readiness, ModuleReadiness::Ready);

        cleanup(&root);
    }

    // ── 4/5. venv status tests are in env.rs ──────────────────────────────

    // ── 6. test_check_all_readiness ───────────────────────────────────────

    #[test]
    fn test_check_all_readiness() {
        let root = temp_dir("all_readiness");

        // Module A: 完全就绪（无 requirements，无模型需求）
        let mod_a_dir = root.join("modules").join("mod-a");
        fs::create_dir_all(&mod_a_dir).unwrap();
        let manifest_a = test_manifest("mod-a", false, false);
        let module_a = make_module(Some(manifest_a), mod_a_dir);

        // Module B: 缺环境
        let mod_b_dir = root.join("modules").join("mod-b");
        fs::create_dir_all(&mod_b_dir).unwrap();
        let manifest_b = test_manifest("mod-b", true, false);
        let module_b = make_module(Some(manifest_b), mod_b_dir);

        let env_mgr = test_env_manager(&root);
        let model_mgr = test_model_manager(&root);
        let lifecycle = ModuleLifecycle::new(env_mgr, model_mgr);
        let config = test_config();

        let modules = vec![module_a, module_b];
        let result = lifecycle.check_all_readiness(&modules, &config);

        assert_eq!(result.len(), 2);
        assert_eq!(result["mod-a"], ModuleReadiness::Ready);
        assert_eq!(result["mod-b"], ModuleReadiness::MissingEnv);

        cleanup(&root);
    }

    // ── Additional: invalid manifest ──────────────────────────────────────

    #[test]
    fn test_lifecycle_invalid_manifest() {
        let root = temp_dir("invalid_manifest");
        let module_dir = root.join("modules").join("bad-mod");
        fs::create_dir_all(&module_dir).unwrap();

        let module = make_module(None, module_dir);

        let env_mgr = test_env_manager(&root);
        let model_mgr = test_model_manager(&root);
        let lifecycle = ModuleLifecycle::new(env_mgr, model_mgr);
        let config = test_config();

        let readiness = lifecycle.get_readiness(&module, &config);
        assert_eq!(readiness, ModuleReadiness::MissingEnv);
    }

    // ── Additional: prepare_module flow ───────────────────────────────────

    #[tokio::test]
    async fn test_prepare_module_flow() {
        let root = temp_dir("prepare_flow");
        let module_dir = root.join("modules").join("test-mod");
        fs::create_dir_all(&module_dir).unwrap();

        // 创建 venv（无 requirements，存在即就绪）
        let venv_dir = root.join("runtime").join("venvs").join("test-mod");
        let venv_bin = if cfg!(windows) {
            venv_dir.join("Scripts")
        } else {
            venv_dir.join("bin")
        };
        fs::create_dir_all(&venv_bin).unwrap();
        let python_name = if cfg!(windows) {
            "python.exe"
        } else {
            "python"
        };
        fs::write(venv_bin.join(python_name), "fake").unwrap();

        // 有模型但模型不存在 → download 会失败（dummy python）
        let manifest = test_manifest("test-mod", false, true);
        let module = make_module(Some(manifest), module_dir);

        let env_mgr = test_env_manager(&root);
        let model_mgr = test_model_manager(&root);
        let mut lifecycle = ModuleLifecycle::new(env_mgr, model_mgr);
        let config = test_config();

        let result = lifecycle.prepare_module(&module, &config).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("download"));

        cleanup(&root);
    }
}
