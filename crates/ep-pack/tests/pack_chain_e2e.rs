//! Wave 4 **D1 E2E** — 整合包全链的库层端到端补强（§4.4）。
//!
//! `import_flow.rs`（B1）覆盖导入的分支矩阵；本文件聚焦**生命周期链**与
//! **daemon 消费的契约不变量**：
//!
//! 1. `meta_registry_invariants_for_daemon_delete` — 导入后逐模型 meta 与
//!    注册表条目满足 daemon `DELETE /api/packs/{id}` 的扫描契约
//!    （`meta.pack_id == pack id`；reference 不落位；管线依赖只取 module
//!    节点 pin，llm 节点的外部模型名不入依赖）；
//! 2. `reimport_after_uninstall_simulation` — 导入 → 模拟 daemon 卸载
//!    （删注册表/模型/管线，与 DELETE handler 行为一致）→ 重新导入成功
//!    （仲裁 #17「先卸载再导入」语义的闭环验证）；
//! 3. `double_import_rejected_state_intact` — 二次导入硬失败后首次导入的
//!    落位与注册表完好无损。
//!
//! 纪律：全部 tempdir、无网络、路径经 `Path::join`（双平台）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use ep_core::types::{ComputeBackend, ComputeDevice, DeviceId};
use ep_pack::build::{build_pack, BuildPlan};
use ep_pack::import::{
    import_pack, read_installed_pack, ImportOptions, ImportTargets, PendingDownload,
    ResolvedModel,
};
use ep_pack::manifest::PackModelEntry;

// ─── fixture ────────────────────────────────────────────────────────────────

static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

fn unique_root(tag: &str) -> PathBuf {
    let seq = TEST_SEQ.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "ep-pack-chain-e2e-{tag}-{}-{seq}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

fn write_file(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

const MANIFEST: &str = r#"
[pack]
id = "tester.chain-pack"
version = "1.0.0"
name = "Chain E2E Pack"
description = "wave-4 d1 lifecycle fixture"
authors = ["d1"]
min_ep_version = "0.1.0"
tags = ["chain"]

[compute]
backends = ["cuda", "cpu"]

[[models]]
qualified_id = "ep.acme.asr"
variant = "v1"
mode = "bundle"
tags = ["asr"]

[[models]]
qualified_id = "ep.acme.tts"
variant = "v2"
mode = "reference"

[[pipelines]]
file = "pipelines/main.toml"
"#;

/// 管线含 module 节点（pin 应入依赖）+ llm 节点（外部模型名不应入依赖，
/// §6.7：llm.model 是 OpenAI 兼容端点的模型名而非 qualified id）
const PIPELINE_MAIN: &str = r#"
[pipeline]
id = "chain-main"
name = "Chain Main"

[[nodes]]
id = "asr"
kind = "module"
module_id = "asr"
capability = "transcribe"
model = "ep.acme.asr@v1"

[[nodes]]
id = "translate"
kind = "llm"
model = "gpt-4o-mini"
params = { base_url = "https://api.openai.com/v1", api_key_env = "OPENAI_API_KEY" }

[[edges]]
from = ["asr", "output"]
to = ["translate", "input"]
"#;

fn resolver() -> impl Fn(&PackModelEntry) -> Result<ResolvedModel, String> {
    |entry: &PackModelEntry| match (entry.qualified_id.as_str(), entry.variant.as_str()) {
        ("ep.acme.asr", "v1") => Ok(ResolvedModel {
            module_id: "asr".into(),
            model_id: "v1".into(),
            target_dir: "asr-v1".into(),
            backends: vec![ComputeBackend::Cuda, ComputeBackend::Cpu],
            download: None,
        }),
        ("ep.acme.tts", "v2") => Ok(ResolvedModel {
            module_id: "tts".into(),
            model_id: "v2".into(),
            target_dir: "tts-v2".into(),
            backends: vec![ComputeBackend::Cpu],
            download: Some(PendingDownload {
                source: "huggingface".into(),
                location: "acme/tts-v2".into(),
                revision: None,
            }),
        }),
        (qid, variant) => Err(format!("module for {qid}@{variant} is not installed")),
    }
}

fn devices() -> Vec<ComputeDevice> {
    vec![
        ComputeDevice {
            id: DeviceId::Cuda(0),
            backend: ComputeBackend::Cuda,
            name: "Chain GPU".into(),
            total_memory_mb: Some(8192),
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        },
        ComputeDevice {
            id: DeviceId::Cpu,
            backend: ComputeBackend::Cpu,
            name: "Chain CPU".into(),
            total_memory_mb: None,
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        },
    ]
}

/// build .zip → import；返回 targets
fn build_and_import(root: &Path) -> ImportTargets {
    let src = root.join("src");
    write_file(&src.join("ep-pack.toml"), MANIFEST.as_bytes());
    write_file(
        &src.join("models").join("asr-v1").join("weights.bin"),
        b"chain-pseudo-weights",
    );
    write_file(
        &src.join("pipelines").join("main.toml"),
        PIPELINE_MAIN.as_bytes(),
    );
    let archive = root.join("chain-pack.zip");
    build_pack(&BuildPlan::new(&src, &archive)).unwrap();

    let targets = ImportTargets::from_root(root);
    import_pack(
        &archive,
        &root.join(".pack-staging"),
        &targets,
        &ImportOptions::default(),
        &devices(),
        resolver(),
        |p| assert!(p.percent <= 100),
    )
    .expect("chain fixture 导入应成功");
    targets
}

fn read_meta(model_dir: &Path) -> serde_json::Value {
    let path = model_dir.join(".ep_meta.json");
    assert!(path.is_file(), "meta 缺失: {}", path.display());
    serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap()
}

// ─── 1. daemon DELETE 消费的契约不变量 ──────────────────────────────────────

#[test]
fn meta_registry_invariants_for_daemon_delete() {
    let root = unique_root("invariants");
    let targets = build_and_import(&root);

    // bundle 模型 meta：source=pack + pack_id 精确匹配（DELETE 扫描键）
    let bundle_dir = targets.models_dir.join("asr-v1");
    let meta = read_meta(&bundle_dir);
    assert_eq!(meta["source"], "pack");
    assert_eq!(meta["pack_id"], "tester.chain-pack");
    assert_eq!(meta["qualified_id"], "ep.acme.asr");
    assert_eq!(meta["module_id"], "asr");
    assert_eq!(meta["model_id"], "v1");

    // reference 模型：只产待下载描述符，绝不落位（下载由上层驱动）
    assert!(!targets.models_dir.join("tts-v2").exists());

    // 注册表条目 = 清单全部模型（bundle + reference），mode/qualified 对齐
    let entry = read_installed_pack(&targets.registry_dir.join("tester.chain-pack.json"))
        .unwrap()
        .expect("注册表条目应存在");
    assert_eq!(entry.id, "tester.chain-pack");
    assert_eq!(entry.models.len(), 2);
    assert_eq!(entry.models[0].qualified_id, "ep.acme.asr");
    assert_eq!(entry.models[1].qualified_id, "ep.acme.tts");
    assert_eq!(entry.pipelines, vec!["chain-main".to_string()]);

    // 管线落位且依赖只含 module 节点 pin（llm 节点的 gpt-4o-mini 不入依赖）
    assert!(targets.pipelines_dir.join("main.toml").is_file());
    let text = fs::read_to_string(targets.pipelines_dir.join("main.toml")).unwrap();
    let doc: toml::Value = toml::from_str(&text).unwrap();
    let mut warnings = Vec::new();
    let deps = ep_pack::import::extract_pipeline_dependencies(&doc, &mut warnings);
    assert_eq!(deps, vec!["ep.acme.asr@v1"], "llm 外部模型名不入依赖");
    assert!(warnings.is_empty(), "{warnings:?}");

    let _ = fs::remove_dir_all(&root);
}

// ─── 2. 卸载（模拟 DELETE）→ 重新导入闭环 ───────────────────────────────────

#[test]
fn reimport_after_uninstall_simulation() {
    let root = unique_root("reimport");
    let targets = build_and_import(&root);

    // 模拟 daemon DELETE keep_models=false 的全部动作：
    // 删 meta.pack_id 匹配的模型目录 + 注册表条目 + 管线文件
    fs::remove_dir_all(targets.models_dir.join("asr-v1")).unwrap();
    fs::remove_file(targets.pipelines_dir.join("main.toml")).unwrap();
    fs::remove_file(targets.registry_dir.join("tester.chain-pack.json")).unwrap();

    // 卸载后重新导入成功（仲裁 #17：PackAlreadyInstalled 的唯一出路是先卸载）
    let targets2 = build_and_import(&root);
    assert!(targets2.models_dir.join("asr-v1").join("weights.bin").is_file());
    assert!(targets2.pipelines_dir.join("main.toml").is_file());
    assert!(targets2.registry_dir.join("tester.chain-pack.json").is_file());

    let _ = fs::remove_dir_all(&root);
}

// ─── 3. 二次导入拒绝后首次导入状态完好 ──────────────────────────────────────

#[test]
fn double_import_rejected_state_intact() {
    let root = unique_root("double");
    let targets = build_and_import(&root);

    let src = root.join("src");
    let archive = root.join("chain-pack.zip");
    let err = import_pack(
        &archive,
        &root.join(".pack-staging"),
        &targets,
        &ImportOptions::default(),
        &devices(),
        resolver(),
        |p| assert!(p.percent <= 100),
    )
    .unwrap_err();
    assert!(
        matches!(err, ep_pack::import::ImportError::PackAlreadyInstalled { .. }),
        "{err:?}"
    );

    // 首次导入的落位与注册表不被拒绝路径破坏
    let _ = &src; // src 仅用于构建语义清晰，此处复用同一归档
    assert!(targets.models_dir.join("asr-v1").join("weights.bin").is_file());
    let entry = read_installed_pack(&targets.registry_dir.join("tester.chain-pack.json"))
        .unwrap()
        .expect("注册表条目仍在");
    assert_eq!(entry.version, "1.0.0");
    assert!(targets.pipelines_dir.join("main.toml").is_file());

    let _ = fs::remove_dir_all(&root);
}
