//! 集成测试：模块发现 → 环境检查 → 启动 → 停止 全流程
//!
//! Wave 4 / Agent F

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use ep_core::config::{AppConfig, ModelsConfig, PythonConfig};
use ep_core::env::EnvManager;
use ep_core::model::ModelManager;
use ep_core::module::discover_modules;
use ep_core::module::lifecycle::{ModuleLifecycle, ModuleReadiness};
use ep_core::process::ProcessManager;
use ep_core::types::{DeviceId, ServiceStatus};

// ─── Helpers ────────────────────────────────────────────────────────────────

fn unique_temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ep_integ_mod_{}_{}_{}",
        label,
        std::process::id(),
        uuid_suffix(),
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn uuid_suffix() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut h);
    format!("{:x}", h.finish() & 0xFFFF_FFFF)
}

fn cleanup(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
}

const VALID_MODULE_TOML: &str = r#"
[module]
id = "test-asr"
name = "Test ASR Module"
version = "1.0.0"
description = "Integration test ASR module"
category = "asr"
genre = "test"
authors = ["TestHawk"]
license = "MIT"
tags = ["test"]

[runtime]
type = "python"
python_version = ">=3.10"
requirements = "requirements.txt"
entrypoint = "adapter.py"
start_command = "cmd /C echo started on port {port}"

[compute]
backends = ["cuda", "cpu"]
default_backend = "cpu"
vram_estimate_mb = 2048

[interface]
type = "http"
health_endpoint = "/health"
ready_timeout_secs = 30

[[interface.capabilities]]
name = "transcribe"
description = "Speech to text"
input_type = "audio"
output_type = "json"
"#;

/// Write a valid module.toml into `modules_dir/<name>/module.toml`
fn write_module(modules_dir: &Path, name: &str, toml_content: &str) -> PathBuf {
    let module_dir = modules_dir.join(name);
    fs::create_dir_all(&module_dir).unwrap();
    fs::write(module_dir.join("module.toml"), toml_content).unwrap();
    module_dir
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

// ─── Test 1: Discover and validate ──────────────────────────────────────────

#[test]
fn test_discover_and_validate() {
    let root = unique_temp_dir("discover");
    let modules_dir = root.join("modules");

    // Write a valid module
    write_module(&modules_dir, "test-asr", VALID_MODULE_TOML);

    // Write an invalid module (bad TOML)
    let bad_dir = modules_dir.join("bad-module");
    fs::create_dir_all(&bad_dir).unwrap();
    fs::write(bad_dir.join("module.toml"), "this is not valid TOML {{{").unwrap();

    // Write a directory without module.toml (should be skipped)
    let empty_dir = modules_dir.join("empty-module");
    fs::create_dir_all(&empty_dir).unwrap();

    let discovered = discover_modules(&modules_dir);

    // Should find 2 modules (test-asr + bad-module), skip empty-module
    assert_eq!(discovered.len(), 2, "expected 2 discovered modules");

    // Validate the good module
    let good = discovered
        .iter()
        .find(|d| d.path.ends_with("test-asr"))
        .expect("test-asr should be discovered");
    assert!(good.manifest.is_some(), "valid module should have manifest");
    assert!(
        matches!(good.status, ep_core::module::DiscoveryStatus::Valid),
        "valid module should have Valid status"
    );
    let manifest = good.manifest.as_ref().unwrap();
    assert_eq!(manifest.module.id, "test-asr");
    assert!(manifest.validate().is_ok());

    // Validate the bad module
    let bad = discovered
        .iter()
        .find(|d| d.path.ends_with("bad-module"))
        .expect("bad-module should be discovered");
    assert!(bad.manifest.is_none(), "invalid TOML should have no manifest");
    assert!(
        matches!(bad.status, ep_core::module::DiscoveryStatus::Invalid(_)),
        "bad module should have Invalid status"
    );

    cleanup(&root);
}

// ─── Test 2: Lifecycle check readiness ──────────────────────────────────────

#[test]
fn test_lifecycle_check_readiness() {
    let root = unique_temp_dir("readiness");
    let modules_dir = root.join("modules");

    // Module with no requirements and no models → should be Ready
    let no_deps_toml = VALID_MODULE_TOML
        .replace("id = \"test-asr\"", "id = \"ready-mod\"")
        .replace("requirements = \"requirements.txt\"", "")
        .replace(
            "start_command = \"cmd /C echo started on port {port}\"",
            "",
        );
    write_module(&modules_dir, "ready-mod", &no_deps_toml);

    // Module with requirements → MissingEnv (no venv set up)
    let needs_env_toml = VALID_MODULE_TOML.replace("id = \"test-asr\"", "id = \"needs-env\"");
    write_module(&modules_dir, "needs-env", &needs_env_toml);

    let discovered = discover_modules(&modules_dir);
    assert_eq!(discovered.len(), 2);

    let env_mgr = test_env_manager(&root);
    let model_mgr = test_model_manager(&root);
    let lifecycle = ModuleLifecycle::new(env_mgr, model_mgr);
    let config = AppConfig::default();

    let readiness_map = lifecycle.check_all_readiness(&discovered, &config);

    // ready-mod has no requirements and no models → Ready
    assert_eq!(readiness_map.get("ready-mod"), Some(&ModuleReadiness::Ready));

    // needs-env has requirements but no venv → MissingEnv
    assert_eq!(
        readiness_map.get("needs-env"),
        Some(&ModuleReadiness::MissingEnv)
    );

    cleanup(&root);
}

// ─── Test 3: Process start/stop ─────────────────────────────────────────────

#[tokio::test]
async fn test_process_start_stop() {
    let mut pm = ProcessManager::new();

    // Build a minimal manifest with a start_command that just echoes
    let manifest: ep_core::module::ModuleManifest = toml::from_str(VALID_MODULE_TOML).unwrap();

    let device = DeviceId::Cpu;
    let port = 18777;
    let env_vars: HashMap<String, String> = HashMap::new();

    // Start
    pm.start_module("test-asr", &manifest, device, port, env_vars)
        .await
        .expect("start_module should succeed");

    // Verify status is Starting (echo exits quickly, might already be detected)
    let status = pm.get_status("test-asr");
    assert!(
        status == Some(&ServiceStatus::Starting)
            || matches!(status, Some(ServiceStatus::Error(_))),
        "expected Starting or Error (echo exits fast), got {:?}",
        status
    );

    // Verify instance has port and device
    let inst = pm.get_instance("test-asr").unwrap();
    assert_eq!(inst.port, Some(18777));
    assert_eq!(inst.device, Some(DeviceId::Cpu));

    // Stop
    pm.stop_module("test-asr").await.expect("stop should succeed");
    assert_eq!(
        pm.get_status("test-asr"),
        Some(&ServiceStatus::Stopped)
    );
}

// ─── Test 4: Full lifecycle ─────────────────────────────────────────────────

#[tokio::test]
async fn test_full_lifecycle() {
    let root = unique_temp_dir("full");
    let modules_dir = root.join("modules");

    // Write a module with no requirements and no models → should be Ready
    let simple_toml = r#"
[module]
id = "full-test"
name = "Full Lifecycle Test"
version = "0.1.0"
description = "Full lifecycle integration test"
category = "custom"
genre = "test"

[runtime]
type = "native"
start_command = "cmd /C echo full-lifecycle-ok"

[compute]
backends = ["cpu"]

[interface]
type = "cli"

[[interface.capabilities]]
name = "process"
description = "Process data"
input_type = "file"
output_type = "file"
"#;

    let _module_dir = write_module(&modules_dir, "full-test", simple_toml);

    // Step 1: Discover
    let discovered = discover_modules(&modules_dir);
    assert_eq!(discovered.len(), 1);
    let module = &discovered[0];
    assert!(module.manifest.is_some());
    assert_eq!(module.manifest.as_ref().unwrap().module.id, "full-test");

    // Step 2: Check environment readiness
    let env_mgr = test_env_manager(&root);
    let model_mgr = test_model_manager(&root);
    let lifecycle = ModuleLifecycle::new(env_mgr, model_mgr);
    let config = AppConfig::default();

    let readiness = lifecycle.get_readiness(module, &config);
    assert_eq!(readiness, ModuleReadiness::Ready, "module should be ready (no env/model deps)");

    // Step 3: Start the module process
    let manifest = module.manifest.as_ref().unwrap();
    let mut pm = ProcessManager::new();
    pm.start_module(
        "full-test",
        manifest,
        DeviceId::Cpu,
        18888,
        HashMap::new(),
    )
    .await
    .expect("start should succeed");

    // Verify it was started
    let status = pm.get_status("full-test");
    assert!(
        status == Some(&ServiceStatus::Starting)
            || matches!(status, Some(ServiceStatus::Error(_))),
        "expected Starting or Error, got {:?}",
        status
    );

    // Step 4: Stop
    pm.stop_module("full-test").await.expect("stop should succeed");
    assert_eq!(
        pm.get_status("full-test"),
        Some(&ServiceStatus::Stopped)
    );

    cleanup(&root);
}
