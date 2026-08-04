//! 整合包导入全流程集成测试（§4.4）— tempdir 往返：
//! A4 `build_pack` 生成真实小 `.epzip` → `import_pack` → 断言
//! meta / 注册表 / 适配报告 / 管线落位 / 进度阶段。
//!
//! 分支覆盖：bundle+reference 混合、缺模块、重名管线、checksum 篡改、
//! min_ep_version 不满足、模型目录冲突、已装包重复导入、bundle 权重缺失、
//! 管线文件缺失 / 非法。
//!
//! 实现所有者：Wave 2 **B1 (PackImport)**。

use std::fs::{self, File};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use ep_core::types::{ComputeBackend, ComputeDevice, DeviceId};
use ep_pack::build::{build_pack, BuildPlan};
use ep_pack::checksum::ChecksumError;
use ep_pack::extract::ExtractError;
use ep_pack::import::{
    import_pack, AdaptationVerdict, ImportError, ImportOptions, ImportReport, ImportStage,
    ImportTargets, PackImportProgress, PendingDownload, ResolvedModel,
};
use ep_pack::manifest::PackModelEntry;

// ─── 测试基建 ────────────────────────────────────────────────────────────────

static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

/// 各测试独立 tempdir（Windows 反斜杠安全：一律 Path::join）
fn unique_root(tag: &str) -> PathBuf {
    let seq = TEST_SEQ.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "ep-pack-import-flow-{tag}-{}-{seq}",
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

/// 标准 fixture 清单（§4.2 全字段）：bundle + reference 双模型 + 双管线。
const FIXTURE_MANIFEST: &str = r#"
[pack]
id = "tester.demo-pack"
version = "1.0.0"
name = "Demo Pack"
description = "integration test pack"
authors = ["b1"]
min_ep_version = "0.1.0"
tags = ["demo"]

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

[[pipelines]]
file = "pipelines/extra.toml"
"#;

const PIPELINE_MAIN: &str = r#"
[pipeline]
id = "demo-main"
name = "Demo Main"

[[nodes]]
id = "asr"
kind = "module"
module_id = "asr"
capability = "transcribe"
model = "ep.acme.asr@v1"
"#;

const PIPELINE_EXTRA: &str = r#"
[pipeline]
id = "demo-extra"
name = "Demo Extra"

[[nodes]]
id = "tts"
kind = "module"
module_id = "tts"
capability = "synthesize"
model = "ep.acme.tts@v2"
"#;

/// 写标准包源目录；`weights` 控制 bundle 权重是否落盘（bundle-missing 分支用）。
fn sample_pack_source(root: &Path, manifest: &str, with_weights: bool) -> PathBuf {
    let src = root.join("src");
    write_file(&src.join("ep-pack.toml"), manifest.as_bytes());
    if with_weights {
        // 顶层真实文件 + blobs/ 冗余副本（§4.4 cleanup_hf_cache 钩子的回收目标）
        write_file(&src.join("models").join("asr-v1").join("weights.bin"), b"weights-v1");
        write_file(
            &src.join("models")
                .join("asr-v1")
                .join("blobs")
                .join("weights.bin"),
            b"weights-v1",
        );
    }
    write_file(&src.join("pipelines").join("main.toml"), PIPELINE_MAIN.as_bytes());
    write_file(&src.join("pipelines").join("extra.toml"), PIPELINE_EXTRA.as_bytes());
    src
}

/// build_pack 生成 .epzip
fn build_fixture(root: &Path, src: &Path) -> PathBuf {
    let archive = root.join("demo-pack.epzip");
    build_pack(&BuildPlan::new(src, &archive)).unwrap();
    archive
}

/// 本机设备 fixture：cuda:0 + cpu
fn cuda_and_cpu_devices() -> Vec<ComputeDevice> {
    vec![
        ComputeDevice {
            id: DeviceId::Cuda(0),
            backend: ComputeBackend::Cuda,
            name: "Test GPU".into(),
            total_memory_mb: Some(8192),
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        },
        ComputeDevice {
            id: DeviceId::Cpu,
            backend: ComputeBackend::Cpu,
            name: "Test CPU".into(),
            total_memory_mb: Some(16384),
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        },
    ]
}

/// 标准模块解析回调：asr→cuda+cpu（bundle）、tts→cpu-only（reference 带下载源）
fn standard_resolver() -> impl Fn(&PackModelEntry) -> Result<ResolvedModel, String> {
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
                revision: Some("main".into()),
            }),
        }),
        (qid, variant) => Err(format!("module for {qid}@{variant} is not installed")),
    }
}

fn targets_for(root: &Path) -> ImportTargets {
    ImportTargets {
        models_dir: root.join("models"),
        pipelines_dir: root.join("config").join("pipelines"),
        registry_dir: root.join("runtime").join("packs"),
    }
}

struct ProgressRecorder {
    events: Mutex<Vec<PackImportProgress>>,
}

impl ProgressRecorder {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    fn callback(&self) -> impl Fn(PackImportProgress) + '_ {
        |p: PackImportProgress| self.events.lock().unwrap().push(p)
    }

    fn events(&self) -> MutexGuard<'_, Vec<PackImportProgress>> {
        self.events.lock().unwrap()
    }
}

fn read_meta_json(model_dir: &Path) -> serde_json::Value {
    let meta_path = model_dir.join(".ep_meta.json");
    assert!(meta_path.is_file(), "meta missing at {}", meta_path.display());
    serde_json::from_str(&fs::read_to_string(&meta_path).unwrap()).unwrap()
}

/// 重写 zip：把 `tamper_path` 条目内容替换为 `new_content`（其余条目照抄），
/// 用于构造 CHECKSUMS 与内容不一致的恶意归档。
fn tamper_archive(src_zip: &Path, dst_zip: &Path, tamper_path: &str, new_content: &[u8]) {
    let file = File::open(src_zip).unwrap();
    let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();
    let out = File::create(dst_zip).unwrap();
    let mut writer = ZipWriter::new(out);
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        let name = entry.name().to_string();
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        if entry.is_dir() {
            writer.add_directory(&name, opts).unwrap();
            continue;
        }
        writer.start_file(&name, opts).unwrap();
        if name == tamper_path {
            writer.write_all(new_content).unwrap();
        } else {
            std::io::copy(&mut entry, &mut writer).unwrap();
        }
    }
    writer.finish().unwrap();
}

// ─── 1. 全流程往返（bundle + reference + 管线 + 注册表 + 适配 + 进度）────────

#[test]
fn full_roundtrip_bundle_and_reference() {
    let root = unique_root("roundtrip");
    let src = sample_pack_source(&root, FIXTURE_MANIFEST, true);
    let archive = build_fixture(&root, &src);
    let targets = targets_for(&root);
    let recorder = ProgressRecorder::new();

    let report: ImportReport = import_pack(
        &archive,
        &root.join("staging"),
        &targets,
        &ImportOptions::default(),
        &cuda_and_cpu_devices(),
        standard_resolver(),
        recorder.callback(),
    )
    .unwrap();

    // ── 报告头 ──
    assert_eq!(report.pack_id, "tester.demo-pack");
    assert_eq!(report.version, "1.0.0");
    assert_eq!(report.name, "Demo Pack");

    // ── 适配报告（§4.6）：asr → cuda:0；tts → CPU 保底 ──
    assert_eq!(report.adaptation.len(), 2);
    let asr = &report.adaptation[0];
    assert_eq!(asr.qualified_id, "ep.acme.asr");
    assert_eq!(asr.variant, "v1");
    assert_eq!(asr.verdict, AdaptationVerdict::Device);
    assert_eq!(asr.device.as_deref(), Some("cuda:0"));
    let tts = &report.adaptation[1];
    assert_eq!(tts.qualified_id, "ep.acme.tts");
    assert_eq!(tts.verdict, AdaptationVerdict::CpuFallback);
    assert!(tts.device.is_none());

    // ── bundle 落位 + meta（source=pack、pack_id、qualified_id、tags 合并）──
    let model_dir = targets.models_dir.join("asr-v1");
    assert!(model_dir.join("weights.bin").is_file());
    let meta = read_meta_json(&model_dir);
    assert_eq!(meta["source"], "pack");
    assert_eq!(meta["pack_id"], "tester.demo-pack");
    assert_eq!(meta["qualified_id"], "ep.acme.asr");
    assert_eq!(meta["module_id"], "asr");
    assert_eq!(meta["model_id"], "v1");
    // tags 合并 = 条目 ["asr"] + 包级 ["demo"]
    assert_eq!(
        meta["tags"].as_array().unwrap().len(),
        2,
        "tags merged: {meta}"
    );
    assert!(meta["tags"].to_string().contains("asr"));
    assert!(meta["tags"].to_string().contains("demo"));

    // ── cleanup_hf_cache（§4.4 钩子）：blobs/ 冗余副本被回收 ──
    assert!(
        report.cache_bytes_reclaimed > 0,
        "expected reclaimed bytes > 0"
    );
    assert!(!model_dir.join("blobs").join("weights.bin").exists());

    // ── reference → 待下载描述符，无目录落位 ──
    assert!(!targets.models_dir.join("tts-v2").exists());
    assert_eq!(report.pending_downloads.len(), 1);
    let pd = &report.pending_downloads[0];
    assert_eq!(pd.qualified_id, "ep.acme.tts");
    assert_eq!(pd.module_id, "tts");
    assert_eq!(pd.target_dir, "tts-v2");
    assert_eq!(pd.download.source, "huggingface");
    assert_eq!(pd.download.location, "acme/tts-v2");
    assert_eq!(pd.download.revision.as_deref(), Some("main"));

    // ── 管线落位 + 依赖提取 ──
    assert!(targets.pipelines_dir.join("main.toml").is_file());
    assert!(targets.pipelines_dir.join("extra.toml").is_file());
    assert_eq!(report.pipelines_installed, vec!["demo-main", "demo-extra"]);
    assert!(report.pipeline_conflicts.is_empty());
    assert_eq!(
        report.pipeline_dependencies,
        vec!["ep.acme.asr@v1", "ep.acme.tts@v2"]
    );

    // ── 注册表 ──
    assert_eq!(
        report.registry_path,
        targets.registry_dir.join("tester.demo-pack.json")
    );
    let registry: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&report.registry_path).unwrap()).unwrap();
    assert_eq!(registry["id"], "tester.demo-pack");
    assert_eq!(registry["version"], "1.0.0");
    assert_eq!(registry["models"].as_array().unwrap().len(), 2);
    assert_eq!(registry["models"][0]["mode"], "bundle");
    assert_eq!(registry["models"][1]["mode"], "reference");
    assert_eq!(registry["pipelines"][0], "demo-main");
    // installed_at 为合法 RFC 3339
    let installed_at = registry["installed_at"].as_str().unwrap();
    assert!(chrono::DateTime::parse_from_rfc3339(installed_at).is_ok());

    // ── 进度：六阶段有序出现、百分比单调不减、首尾完整 ──
    let events = recorder.events();
    assert!(!events.is_empty());
    let stages: Vec<ImportStage> = events.iter().map(|e| e.stage).collect();
    for stage in [
        ImportStage::Extracting,
        ImportStage::Verifying,
        ImportStage::Manifest,
        ImportStage::Models,
        ImportStage::Pipelines,
        ImportStage::Registering,
    ] {
        assert!(stages.contains(&stage), "missing stage {stage:?}: {stages:?}");
    }
    // 阶段顺序单调（阶段序列去除相邻重复后应为严格递增的六阶段）
    let mut deduped: Vec<ImportStage> = Vec::new();
    for s in &stages {
        if deduped.last() != Some(s) {
            deduped.push(*s);
        }
    }
    assert_eq!(
        deduped,
        vec![
            ImportStage::Extracting,
            ImportStage::Verifying,
            ImportStage::Manifest,
            ImportStage::Models,
            ImportStage::Pipelines,
            ImportStage::Registering,
        ]
    );
    let pcts: Vec<u8> = events.iter().map(|e| e.percent).collect();
    assert!(pcts.windows(2).all(|w| w[0] <= w[1]), "{pcts:?}");
    assert_eq!(*pcts.last().unwrap(), 100);

    // ── 成功后解包子目录已清理（staging 下无 extract-* 目录）──
    let staging = root.join("staging");
    if staging.is_dir() {
        let leftovers: Vec<_> = fs::read_dir(&staging)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| n.starts_with("extract-"))
                    .unwrap_or(false)
            })
            .collect();
        assert!(leftovers.is_empty(), "staging not cleaned: {leftovers:?}");
    }

    let _ = fs::remove_dir_all(&root);
}

// ─── 2. 缺模块：适配报告 Unsupported，bundle 不落位，导入仍成功 ──────────────

#[test]
fn missing_module_reported_in_adaptation() {
    let root = unique_root("missing-module");
    let src = sample_pack_source(&root, FIXTURE_MANIFEST, true);
    let archive = build_fixture(&root, &src);
    let targets = targets_for(&root);

    let report = import_pack(
        &archive,
        &root.join("staging"),
        &targets,
        &ImportOptions::default(),
        &cuda_and_cpu_devices(),
        // 全部模块缺失
        |entry: &PackModelEntry| Err(format!("module for {} is not installed", entry.qualified_id)),
        |_: PackImportProgress| {},
    )
    .unwrap();

    assert_eq!(report.adaptation.len(), 2);
    for entry in &report.adaptation {
        assert_eq!(entry.verdict, AdaptationVerdict::Unsupported);
        assert!(entry.reason.contains("not installed"), "{}", entry.reason);
    }
    // bundle 权重不落位（模块缺失 → 跳过），reference 无描述符
    assert!(!targets.models_dir.join("asr-v1").exists());
    assert!(report.installed_models.is_empty());
    assert!(report.pending_downloads.is_empty());
    // 管线仍然落位（包可部分可用）
    assert_eq!(report.pipelines_installed.len(), 2);
    // 注册表仍写入（§4.4：缺模块报适配报告而非静默失败）
    assert!(report.registry_path.is_file());

    let _ = fs::remove_dir_all(&root);
}

// ─── 3. 重名管线：冲突条目进报告，既有文件不被覆盖 ──────────────────────────

#[test]
fn pipeline_conflict_reported_not_overwritten() {
    let root = unique_root("pipeline-conflict");
    let src = sample_pack_source(&root, FIXTURE_MANIFEST, true);
    let archive = build_fixture(&root, &src);
    let targets = targets_for(&root);

    // 预置既有管线：与包内 demo-main 同 id
    let existing_main = targets.pipelines_dir.join("existing-main.toml");
    write_file(
        &existing_main,
        b"[pipeline]\nid = \"demo-main\"\nname = \"Pre-existing\"\n",
    );

    let report = import_pack(
        &archive,
        &root.join("staging"),
        &targets,
        &ImportOptions::default(),
        &cuda_and_cpu_devices(),
        standard_resolver(),
        |_: PackImportProgress| {},
    )
    .unwrap();

    assert_eq!(report.pipelines_installed, vec!["demo-extra"]);
    assert_eq!(report.pipeline_conflicts.len(), 1);
    let conflict = &report.pipeline_conflicts[0];
    assert_eq!(conflict.pipeline_id, "demo-main");
    assert_eq!(conflict.file, "pipelines/main.toml");
    assert!(conflict.reason.contains("already installed"), "{}", conflict.reason);
    // 既有文件原样保留，包内 main.toml 未落位为同名新文件
    let content = fs::read_to_string(&existing_main).unwrap();
    assert!(content.contains("Pre-existing"));
    assert!(!targets.pipelines_dir.join("main.toml").exists());
    // extra 正常落位
    assert!(targets.pipelines_dir.join("extra.toml").is_file());
    // 注册表只含已落位管线
    let registry: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&report.registry_path).unwrap()).unwrap();
    assert_eq!(registry["pipelines"].as_array().unwrap().len(), 1);

    let _ = fs::remove_dir_all(&root);
}

// ─── 4. checksum 篡改：硬失败，零落盘 ───────────────────────────────────────

#[test]
fn checksum_tamper_aborts_before_any_placement() {
    let root = unique_root("checksum");
    let src = sample_pack_source(&root, FIXTURE_MANIFEST, true);
    let archive = build_fixture(&root, &src);
    let tampered = root.join("tampered.epzip");
    tamper_archive(
        &archive,
        &tampered,
        "models/asr-v1/weights.bin",
        b"EVIL-WEIGHTS",
    );
    let targets = targets_for(&root);

    let err = import_pack(
        &tampered,
        &root.join("staging"),
        &targets,
        &ImportOptions::default(),
        &cuda_and_cpu_devices(),
        standard_resolver(),
        |_: PackImportProgress| {},
    )
    .unwrap_err();

    match &err {
        ImportError::Checksum(ChecksumError::Integrity(report)) => {
            assert_eq!(report.mismatched.len(), 1);
            assert_eq!(report.mismatched[0].path, "models/asr-v1/weights.bin");
        }
        other => panic!("expected Checksum(Integrity), got {other:?}"),
    }
    // 零落盘：模型/管线/注册表一律未动
    assert!(!targets.models_dir.join("asr-v1").exists());
    assert!(!targets.pipelines_dir.join("main.toml").exists());
    assert!(!targets.registry_dir.join("tester.demo-pack.json").exists());
    // 失败时解包目录保留供排查
    let staging = root.join("staging");
    let leftover = fs::read_dir(&staging)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_str().unwrap_or("").starts_with("extract-"));
    assert!(leftover, "extract dir should be preserved on failure");

    let _ = fs::remove_dir_all(&root);
}

// ─── 5. min_ep_version 不满足：硬失败 ───────────────────────────────────────

#[test]
fn min_ep_version_gate_rejects() {
    let root = unique_root("min-version");
    let manifest = FIXTURE_MANIFEST.replace("min_ep_version = \"0.1.0\"", "min_ep_version = \"9.9.9\"");
    let src = sample_pack_source(&root, &manifest, true);
    let archive = build_fixture(&root, &src);
    let targets = targets_for(&root);

    let err = import_pack(
        &archive,
        &root.join("staging"),
        &targets,
        &ImportOptions::default(),
        &cuda_and_cpu_devices(),
        standard_resolver(),
        |_: PackImportProgress| {},
    )
    .unwrap_err();

    match err {
        ImportError::MinEpVersion { required, .. } => assert_eq!(required, "9.9.9"),
        other => panic!("expected MinEpVersion, got {other:?}"),
    }
    assert!(!targets.registry_dir.join("tester.demo-pack.json").exists());

    let _ = fs::remove_dir_all(&root);
}

// ─── 6. 模型目录已存在：TOCTOU 冲突硬失败，绝不合并 ─────────────────────────

#[test]
fn model_dir_conflict_hard_error() {
    let root = unique_root("model-conflict");
    let src = sample_pack_source(&root, FIXTURE_MANIFEST, true);
    let archive = build_fixture(&root, &src);
    let targets = targets_for(&root);

    // 预置既有模型目录（含用户文件）
    let existing = targets.models_dir.join("asr-v1");
    write_file(&existing.join("user-file.bin"), b"user-data");

    let err = import_pack(
        &archive,
        &root.join("staging"),
        &targets,
        &ImportOptions::default(),
        &cuda_and_cpu_devices(),
        standard_resolver(),
        |_: PackImportProgress| {},
    )
    .unwrap_err();

    match err {
        ImportError::ModelConflict { qualified_id, .. } => {
            assert_eq!(qualified_id, "ep.acme.asr")
        }
        other => panic!("expected ModelConflict, got {other:?}"),
    }
    // 用户文件原样保留，注册表未写入
    assert_eq!(
        fs::read(existing.join("user-file.bin")).unwrap(),
        b"user-data"
    );
    assert!(!existing.join("weights.bin").exists(), "绝不合并进已有目录");
    assert!(!targets.registry_dir.join("tester.demo-pack.json").exists());

    let _ = fs::remove_dir_all(&root);
}

// ─── 7. 重复导入已装包：PackAlreadyInstalled ────────────────────────────────

#[test]
fn already_installed_pack_rejected() {
    let root = unique_root("already-installed");
    let src = sample_pack_source(&root, FIXTURE_MANIFEST, true);
    let archive = build_fixture(&root, &src);
    let targets = targets_for(&root);
    let devices = cuda_and_cpu_devices();

    import_pack(
        &archive,
        &root.join("staging"),
        &targets,
        &ImportOptions::default(),
        &devices,
        standard_resolver(),
        |_: PackImportProgress| {},
    )
    .unwrap();

    let err = import_pack(
        &archive,
        &root.join("staging"),
        &targets,
        &ImportOptions::default(),
        &devices,
        standard_resolver(),
        |_: PackImportProgress| {},
    )
    .unwrap_err();
    assert!(matches!(err, ImportError::PackAlreadyInstalled { .. }), "{err:?}");

    let _ = fs::remove_dir_all(&root);
}

// ─── 8. bundle 权重缺失：BundleMissing ──────────────────────────────────────

#[test]
fn bundle_weights_missing_hard_error() {
    let root = unique_root("bundle-missing");
    // 清单声明 bundle 但源目录不含 models/asr-v1
    let src = sample_pack_source(&root, FIXTURE_MANIFEST, false);
    let archive = build_fixture(&root, &src);
    let targets = targets_for(&root);

    let err = import_pack(
        &archive,
        &root.join("staging"),
        &targets,
        &ImportOptions::default(),
        &cuda_and_cpu_devices(),
        standard_resolver(),
        |_: PackImportProgress| {},
    )
    .unwrap_err();

    match err {
        ImportError::BundleMissing { target_dir, .. } => assert_eq!(target_dir, "asr-v1"),
        other => panic!("expected BundleMissing, got {other:?}"),
    }

    let _ = fs::remove_dir_all(&root);
}

// ─── 9. 管线文件缺失 / TOML 非法：硬失败 ────────────────────────────────────

#[test]
fn pipeline_file_missing_hard_error() {
    let root = unique_root("pipeline-missing");
    let manifest = format!(
        "{FIXTURE_MANIFEST}\n[[pipelines]]\nfile = \"pipelines/ghost.toml\"\n"
    );
    let src = sample_pack_source(&root, &manifest, true);
    let archive = build_fixture(&root, &src);
    let targets = targets_for(&root);

    let err = import_pack(
        &archive,
        &root.join("staging"),
        &targets,
        &ImportOptions::default(),
        &cuda_and_cpu_devices(),
        standard_resolver(),
        |_: PackImportProgress| {},
    )
    .unwrap_err();
    match err {
        ImportError::PipelineFileMissing { file } => assert_eq!(file, "pipelines/ghost.toml"),
        other => panic!("expected PipelineFileMissing, got {other:?}"),
    }

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn pipeline_invalid_toml_hard_error() {
    let root = unique_root("pipeline-invalid");
    let src = sample_pack_source(&root, FIXTURE_MANIFEST, true);
    // 覆盖 main.toml 为非法 TOML（build 在覆盖后进行，checksum 一致）
    write_file(&src.join("pipelines").join("main.toml"), b"not [[valid toml");
    let archive = build_fixture(&root, &src);
    let targets = targets_for(&root);

    let err = import_pack(
        &archive,
        &root.join("staging"),
        &targets,
        &ImportOptions::default(),
        &cuda_and_cpu_devices(),
        standard_resolver(),
        |_: PackImportProgress| {},
    )
    .unwrap_err();
    assert!(matches!(err, ImportError::InvalidPipeline { .. }), "{err:?}");

    let _ = fs::remove_dir_all(&root);
}

// ─── 10. 解包安全边界透传（zip-slip 归档在 Extracting 阶段即被拒）──────────

#[test]
fn zip_slip_archive_rejected_at_extract_stage() {
    let root = unique_root("zip-slip");
    // 直接构造恶意 zip（含 ep-pack.toml 与 ../evil.txt）
    let archive = root.join("evil.epzip");
    {
        let file = File::create(&archive).unwrap();
        let mut zip = ZipWriter::new(file);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        zip.start_file("ep-pack.toml", opts).unwrap();
        zip.write_all(b"[pack]").unwrap();
        zip.start_file("../evil.txt", opts).unwrap();
        zip.write_all(b"pwned").unwrap();
        zip.finish().unwrap();
    }
    let targets = targets_for(&root);

    let err = import_pack(
        &archive,
        &root.join("staging"),
        &targets,
        &ImportOptions::default(),
        &cuda_and_cpu_devices(),
        standard_resolver(),
        |_: PackImportProgress| {},
    )
    .unwrap_err();
    assert!(matches!(err, ImportError::Extract(ExtractError::UnsafePath(_))), "{err:?}");
    assert!(!root.join("evil.txt").exists());

    let _ = fs::remove_dir_all(&root);
}
