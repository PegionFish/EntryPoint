//! 集成测试：`requirements_by_backend` 消费 + 分后端 venv（HETERO_DIST_PLAN §4 M2/M3）
//!
//! 全程离线：不执行 uv 安装、不访问网络。venv 以夹具文件伪造（假解释器 +
//! 预写 `.ep_deps_hash`），验证
//! manifest 解析 → 按后端选依赖文件 → 分后端 venv 落位/旧布局兼容读取
//! → 就绪判定的完整消费链路。

use std::fs;
use std::path::{Path, PathBuf};

use ep_core::config::{ModelsConfig, PythonConfig};
use ep_core::env::EnvManager;
use ep_core::module::lifecycle::{ModuleLifecycle, ModuleReadiness};
use ep_core::module::manifest::ModuleManifest;
use ep_core::types::ComputeBackend;

/// 含冻结 schema 字段的完整清单（inline table 形态，MODULE_SPEC §2.6）
const MANIFEST_TOML: &str = r#"
[module]
id = "video-upscale"
name = "Video Upscale"
version = "0.1.0"
description = "M2/M3 integration fixture"
category = "video"
genre = "sr"
license = "MIT"

[runtime]
type = "python"
python_version = ">=3.10,<3.13"
requirements = "requirements.txt"
requirements_by_backend = { cuda = "requirements-cuda.txt", rocm = "requirements-rocm.txt", cpu = "requirements.txt" }

[compute]
backends = ["cuda", "rocm", "cpu"]
default_backend = "cuda"

[interface]
type = "http"
"#;

fn unique_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ep-int-rbb-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_module_dir(root: &Path, toml_src: &str) -> PathBuf {
    let module_dir = root.join("modules").join("video-upscale");
    fs::create_dir_all(&module_dir).unwrap();
    fs::write(module_dir.join("module.toml"), toml_src).unwrap();
    // 三份依赖文件内容互不相同（哈希可区分）
    fs::write(module_dir.join("requirements.txt"), "fastapi\nuvicorn\n").unwrap();
    fs::write(
        module_dir.join("requirements-cuda.txt"),
        "fastapi\nuvicorn\ntorch\n",
    )
    .unwrap();
    fs::write(
        module_dir.join("requirements-rocm.txt"),
        "fastapi\nuvicorn\ntorch-rocm\n",
    )
    .unwrap();
    module_dir
}

fn fake_python(venv_dir: &Path) -> PathBuf {
    let bin = if cfg!(windows) {
        venv_dir.join("Scripts")
    } else {
        venv_dir.join("bin")
    };
    fs::create_dir_all(&bin).unwrap();
    let py = bin.join(if cfg!(windows) { "python.exe" } else { "python" });
    fs::write(&py, b"fake").unwrap();
    py
}

// ─── M2：manifest 解析冻结 schema + 按后端选依赖文件 ─────────────────────────

#[test]
fn manifest_parses_frozen_schema_and_resolves_dep_files() {
    let root = unique_root("parse");
    let module_dir = write_module_dir(&root, MANIFEST_TOML);

    let manifest = ModuleManifest::from_file(&module_dir.join("module.toml"))
        .expect("manifest with requirements_by_backend must load");
    manifest.validate().expect("fixture must be valid");

    let runtime = &manifest.runtime;
    assert_eq!(runtime.requirements_by_backend.len(), 3);

    // 命中 per-backend 条目
    assert_eq!(
        runtime.resolve_requirements(Some(ComputeBackend::Cuda)),
        "requirements-cuda.txt"
    );
    assert_eq!(
        runtime.resolve_requirements(Some(ComputeBackend::Rocm)),
        "requirements-rocm.txt"
    );
    assert_eq!(
        runtime.resolve_requirements(Some(ComputeBackend::Cpu)),
        "requirements.txt"
    );

    // 词表内但无条目 / 后端未知 → 回退 runtime.requirements
    assert_eq!(
        runtime.resolve_requirements(Some(ComputeBackend::Vulkan)),
        "requirements.txt"
    );
    assert_eq!(runtime.resolve_requirements(None), "requirements.txt");

    // 解析出的路径在模块目录下真实存在
    assert!(module_dir
        .join(runtime.resolve_requirements(Some(ComputeBackend::Cuda)))
        .is_file());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn manifest_roundtrip_preserves_backend_requirements() {
    let root = unique_root("roundtrip");
    let module_dir = write_module_dir(&root, MANIFEST_TOML);

    let manifest =
        ModuleManifest::from_file(&module_dir.join("module.toml")).expect("load");
    let serialized = toml::to_string_pretty(&manifest).expect("serialize");
    let reparsed: ModuleManifest = toml::from_str(&serialized).expect("re-parse");

    assert_eq!(
        reparsed.runtime.requirements_by_backend,
        manifest.runtime.requirements_by_backend,
        "整合包导出/导入往返不得丢失后端依赖映射"
    );
    assert_eq!(
        reparsed.runtime.resolve_requirements(Some(ComputeBackend::Rocm)),
        "requirements-rocm.txt"
    );

    let _ = fs::remove_dir_all(&root);
}

// ─── M3：分后端 venv 落位 + 旧布局兼容读取 ───────────────────────────────────

#[test]
fn per_backend_venv_layout_naming() {
    let root = unique_root("layout");
    let env_mgr = EnvManager::new(&root, &PythonConfig::default());

    assert_eq!(
        env_mgr.venv_dir_for_backend("video-upscale", ComputeBackend::Cuda),
        root.join("runtime")
            .join("venvs")
            .join("video-upscale--cuda")
    );
    assert_eq!(
        env_mgr.venv_python_path_for_backend("video-upscale", ComputeBackend::Rocm),
        root.join("runtime")
            .join("venvs")
            .join("video-upscale--rocm")
            .join(if cfg!(windows) {
                "Scripts/python.exe"
            } else {
                "bin/python"
            }),
        "无任何 venv 时返回新布局口径（前瞻性答案）"
    );

    let _ = fs::remove_dir_all(&root);
}

/// 核心链路：旧单 venv 就绪 → 分后端入口复用（不全量重建）；
/// 生命周期就绪判定按 backend 维度放行
#[test]
fn legacy_ready_venv_is_reused_across_backends() {
    let root = unique_root("reuse");
    let module_dir = write_module_dir(&root, MANIFEST_TOML);

    let env_mgr = EnvManager::new(&root, &PythonConfig::default());

    // 伪造就绪的旧单 venv：假解释器 + 旧口径匹配哈希
    let mid = "video-upscale";
    let legacy_py = fake_python(&root.join("runtime").join("venvs").join(mid));
    let cuda_req = module_dir.join("requirements-cuda.txt");
    let rocm_req = module_dir.join("requirements-rocm.txt");

    // 就绪基準按 cuda 维度的依赖文件写入（ensure_venv_for_backend 复用判定
    // 用的是调用方解析后的同一依赖文件路径）
    let hash = ep_core::env::compute_deps_hash(&cuda_req, None).unwrap();
    fs::write(
        root.join("runtime")
            .join("venvs")
            .join(mid)
            .join(".ep_deps_hash"),
        hash,
    )
    .unwrap();

    // M2/M3 入口：cuda 维度复用旧布局，不新建分后端目录
    let py = env_mgr
        .ensure_venv_for_backend(mid, ">=3.10,<3.13", &cuda_req, ComputeBackend::Cuda)
        .expect("ready legacy venv must be reused without spawning uv");
    assert_eq!(py, legacy_py);
    assert!(!env_mgr
        .venv_dir_for_backend(mid, ComputeBackend::Cuda)
        .exists());
    assert!(env_mgr.is_venv_ready_for_backend(mid, &cuda_req, ComputeBackend::Cuda));

    // rocm 维度依赖文件不同（哈希失配）→ 旧布局在该维度不算就绪
    assert!(!env_mgr.is_venv_ready_for_backend(mid, &rocm_req, ComputeBackend::Rocm));

    // 生命周期就绪判定（backend 感知）：环境就绪且无模型需求 → Ready
    let manifest = ModuleManifest::from_file(&module_dir.join("module.toml")).expect("load");
    let models_cfg = ModelsConfig {
        cache_dir: root.join("models").to_string_lossy().to_string(),
        hf_endpoint: String::new(),
        default_source: "huggingface".to_string(),
        max_concurrent_downloads: 1,
        cache_paths: Vec::new(),
    };
    let lifecycle = ModuleLifecycle::new(
        EnvManager::new(&root, &PythonConfig::default()),
        ep_core::model::ModelManager::new(&models_cfg, &root),
    );
    let discovered = ep_core::module::discovery::DiscoveredModule {
        manifest: Some(manifest),
        path: module_dir,
        status: ep_core::module::discovery::DiscoveryStatus::Valid,
    };
    let config = ep_core::config::AppConfig::default();

    assert_eq!(
        lifecycle.get_readiness_for_backend(&discovered, &config, Some(ComputeBackend::Cuda)),
        ModuleReadiness::Ready,
        "cuda 维度应经旧布局兼容读取判定就绪"
    );

    let _ = fs::remove_dir_all(&root);
}

/// 陈旧旧布局（依赖栈哈希失配）在 backend 维度不得判定就绪。
///
/// 本测试**只做 fs-only 就绪断言**，绝不进入 `ensure_venv*` 的创建/安装
/// 分支——集成环境无法像单元测试那样固定 `uv_path = None`
/// （`EnvManager::new` 会探测宿主 uv），一旦走到创建分支就会真实执行
/// `uv venv` / `uv pip install`（联网装包）。陈旧布局不复用、转投新目录的
/// 行为由单元测试 `env::tests::ensure_venv_for_backend_skips_stale_legacy_venv`
/// 以固定无 uv 环境覆盖。
#[test]
fn stale_legacy_venv_is_not_ready_in_backend_dimension() {
    let root = unique_root("stale");
    let module_dir = write_module_dir(&root, MANIFEST_TOML);

    let env_mgr = EnvManager::new(&root, &PythonConfig::default());
    let mid = "video-upscale";

    // 陈旧旧布局：假解释器 + 与当前依赖栈不匹配的哈希
    fake_python(&root.join("runtime").join("venvs").join(mid));
    fs::write(
        root.join("runtime")
            .join("venvs")
            .join(mid)
            .join(".ep_deps_hash"),
        "hash:deadbeefdeadbeef",
    )
    .unwrap();

    let cuda_req = module_dir.join("requirements-cuda.txt");

    // 哈希失配 → 新旧两个维度都不得判就绪（不因解释器存在而误判复用）
    assert!(!env_mgr.is_venv_ready(mid, &cuda_req));
    assert!(!env_mgr.is_venv_ready_for_backend(mid, &cuda_req, ComputeBackend::Cuda));

    // 对照组：写入与当前依赖栈匹配的旧口径哈希后恢复就绪（兼容读取生效）
    fs::write(
        root.join("runtime")
            .join("venvs")
            .join(mid)
            .join(".ep_deps_hash"),
        ep_core::env::compute_deps_hash(&cuda_req, None).unwrap(),
    )
    .unwrap();
    assert!(env_mgr.is_venv_ready_for_backend(mid, &cuda_req, ComputeBackend::Cuda));

    let _ = fs::remove_dir_all(&root);
}

/// 新布局就绪判定：`.ep_deps_hash` 必须为 backend 口径哈希；
/// 旧口径哈希写入新目录时不得通过该 backend 维度校验
#[test]
fn new_layout_readiness_requires_backend_scoped_hash() {
    let root = unique_root("newlayout");
    let module_dir = write_module_dir(&root, MANIFEST_TOML);

    let env_mgr = EnvManager::new(&root, &PythonConfig::default());
    let mid = "video-upscale";
    let cuda_req = module_dir.join("requirements-cuda.txt");

    // 新布局：仅解释器，无哈希 → 未就绪
    let py = fake_python(&env_mgr.venv_dir_for_backend(mid, ComputeBackend::Cuda));
    assert!(!env_mgr.is_venv_ready_for_backend(mid, &cuda_req, ComputeBackend::Cuda));

    // 写入 backend 口径哈希 → 就绪，解释器解析优先命中新布局
    fs::write(
        env_mgr
            .venv_dir_for_backend(mid, ComputeBackend::Cuda)
            .join(".ep_deps_hash"),
        ep_core::env::compute_deps_hash_for_backend(&cuda_req, None, ComputeBackend::Cuda)
            .unwrap(),
    )
    .unwrap();
    assert!(env_mgr.is_venv_ready_for_backend(mid, &cuda_req, ComputeBackend::Cuda));
    assert_eq!(
        env_mgr.venv_python_path_for_backend(mid, ComputeBackend::Cuda),
        py
    );

    // 同一目录换成 cuda 口径之外的哈希 → cuda 维度失配
    fs::write(
        env_mgr
            .venv_dir_for_backend(mid, ComputeBackend::Cuda)
            .join(".ep_deps_hash"),
        ep_core::env::compute_deps_hash_for_backend(&cuda_req, None, ComputeBackend::Rocm)
            .unwrap(),
    )
    .unwrap();
    assert!(!env_mgr.is_venv_ready_for_backend(mid, &cuda_req, ComputeBackend::Cuda));

    let _ = fs::remove_dir_all(&root);
}
