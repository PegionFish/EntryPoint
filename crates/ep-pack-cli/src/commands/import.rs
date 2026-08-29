//! `ep-pack import <archive.zip> [--root <dir>] [--modules-dir <dir>] [--dry-run]`
//!
//! 正式导入走 ep-pack `import_pack` 编排（§4.4 全流程）：extract + CHECKSUMS
//! 全量校验 + 清单校验/semver 门禁 + bundle 落位 + meta + 管线落位 + 注册表。
//! 模块解析回调读本机 `<modules-dir>/*/module.toml` 匹配 qualified_id（§4.3）。
//!
//! `--dry-run`：CLI 侧复演「落位前」全部校验（解包 → checksum → 清单 →
//! 重复安装 → 模块解析/适配 → 管线检查/冲突），只出适配/校验报告，绝不落位。
//! 库层 `import_pack` 无 dry-run 参数，此分支为 CLI 层编排（只读 + 暂存）。
//!
//! reference 模型的实际下载由 daemon/WebUI 驱动（§4.4）；CLI 为离线工具，
//! 仅在报告中列出待下载描述符。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ep_core::compute::detect_all_devices;
use ep_core::types::ComputeDevice;
use ep_pack::checksum::ChecksumTable;
use ep_pack::extract::{extract_pack, ExtractLimits, MANIFEST_FILE_NAME};
use ep_pack::import::{
    adapt_model, import_pack, read_installed_pack, registry_entry_path, AdaptationVerdict,
    ImportOptions, ImportReport, ImportTargets, PackAdaptationEntry,
};
use ep_pack::manifest::{semver, ModelMode, PackManifest, PackModelEntry};

use crate::args::{self, OptDef};
use crate::commands::join_pack_rel;
use crate::output::{self, EXIT_FAILURE, EXIT_OK, EXIT_USAGE};
use crate::resolve::{load_module_catalog, resolve_entry};

const USAGE: &str = "usage: ep-pack import <archive.zip> [--root <dir>] [--modules-dir <dir>] [--dry-run] [--json]";

/// 测试/低配环境钩子：设置该环境变量则跳过本机设备检测（适配报告以
/// 「无已检测设备」视角生成；reference/bundle 的校验语义不受影响）。
const NO_DEVICE_DETECT_ENV: &str = "EP_PACK_CLI_NO_DEVICE_DETECT";

pub fn run(argv: &[String]) -> ExitCode {
    let opts = [
        OptDef {
            name: "root",
            long: "--root",
            short: None,
            takes_value: true,
        },
        OptDef {
            name: "modules-dir",
            long: "--modules-dir",
            short: None,
            takes_value: true,
        },
        OptDef {
            name: "dry-run",
            long: "--dry-run",
            short: None,
            takes_value: false,
        },
    ];
    let parsed = match args::parse(argv, &opts) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("{USAGE}");
            return output::exit(EXIT_USAGE);
        }
    };
    let json = parsed.switch("json");
    if parsed.switch("help") {
        println!("{USAGE}");
        return output::exit(EXIT_OK);
    }
    let positional = match parsed.positional_exact(1, USAGE) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return output::exit(EXIT_USAGE);
        }
    };
    let archive = PathBuf::from(&positional[0]);
    if !archive.is_file() {
        return output::fail(
            EXIT_USAGE,
            json,
            vec![format!("archive does not exist: {}", archive.display())],
        );
    }
    let root = parsed
        .value("root")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    if !root.is_dir() {
        return output::fail(
            EXIT_USAGE,
            json,
            vec![format!("--root does not exist or is not a directory: {}", root.display())],
        );
    }
    let modules_dir = parsed
        .value("modules-dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("modules"));

    let catalog = load_module_catalog(&modules_dir);
    if !json {
        for bad in &catalog.unreadable {
            println!("warning: skipping unreadable module manifest {}", bad.display());
        }
    }
    let manifests = &catalog.manifests;
    let devices = local_devices();
    let targets = ImportTargets::from_root(&root);
    let staging_root = root.join(".pack-staging");

    if parsed.switch("dry-run") {
        run_dry_run(&archive, &staging_root, &targets, manifests, &devices, json)
    } else {
        run_import(&archive, &staging_root, &targets, manifests, &devices, json)
    }
}

/// 本机设备列表（适配报告输入，§4.6）；环境变量可跳过检测。
fn local_devices() -> Vec<ComputeDevice> {
    if std::env::var_os(NO_DEVICE_DETECT_ENV).is_some() {
        Vec::new()
    } else {
        detect_all_devices(&[])
    }
}

// ─── 正式导入 ────────────────────────────────────────────────────────────────

fn run_import(
    archive: &Path,
    staging_root: &Path,
    targets: &ImportTargets,
    manifests: &[ep_core::module::ModuleManifest],
    devices: &[ComputeDevice],
    json: bool,
) -> ExitCode {
    if let Err(e) = std::fs::create_dir_all(staging_root) {
        return output::fail(
            EXIT_FAILURE,
            json,
            vec![format!(
                "failed to create staging dir {}: {e}",
                staging_root.display()
            )],
        );
    }
    let options = ImportOptions::default();
    let progress = |p: ep_pack::import::PackImportProgress| {
        if !json {
            println!("[{} {:>3}%] {}", p.stage.as_str(), p.percent, p.message);
        }
    };
    let resolve = |entry: &PackModelEntry| resolve_entry(manifests, entry);

    match import_pack(archive, staging_root, targets, &options, devices, resolve, progress) {
        Ok(report) => {
            if json {
                output::print_json(&import_report_json(&report, false));
            } else {
                print_report_human(&report);
            }
            output::exit(EXIT_OK)
        }
        Err(e) => output::fail(
            EXIT_FAILURE,
            json,
            vec![format!("import failed: {e}")],
        ),
    }
}

fn import_report_json(report: &ImportReport, dry_run: bool) -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "dry_run": dry_run,
        "pack_id": report.pack_id,
        "version": report.version,
        "name": report.name,
        "adaptation": report.adaptation,
        "installed_models": report.installed_models,
        "pending_downloads": report.pending_downloads,
        "pipelines_installed": report.pipelines_installed,
        "pipeline_conflicts": report.pipeline_conflicts,
        "pipeline_dependencies": report.pipeline_dependencies,
        "warnings": report.warnings,
        "registry_path": report.registry_path.display().to_string(),
        "cache_bytes_reclaimed": report.cache_bytes_reclaimed,
    })
}

fn verdict_text(entry: &PackAdaptationEntry) -> String {
    match entry.verdict {
        AdaptationVerdict::Device => format!(
            "will run on {}",
            entry.device.clone().unwrap_or_else(|| "accelerator".into())
        ),
        AdaptationVerdict::CpuFallback => "CPU fallback".to_string(),
        AdaptationVerdict::Unsupported => "unsupported".to_string(),
    }
}

fn print_report_human(report: &ImportReport) {
    println!(
        "imported pack '{}' v{} ({})",
        report.pack_id, report.version, report.name
    );
    println!("  registry: {}", report.registry_path.display());

    if !report.adaptation.is_empty() {
        println!("adaptation:");
        for a in &report.adaptation {
            println!(
                "  - {}@{}: {} ({})",
                a.qualified_id,
                a.variant,
                verdict_text(a),
                a.reason
            );
        }
    }
    if !report.installed_models.is_empty() {
        println!("bundle models placed:");
        for m in &report.installed_models {
            println!(
                "  - {}@{} -> models/{} ({})",
                m.qualified_id,
                m.variant,
                m.target_dir,
                output::human_bytes(m.total_bytes)
            );
        }
    }
    if !report.pending_downloads.is_empty() {
        println!("reference models pending download (driven by daemon/WebUI, not this CLI):");
        for p in &report.pending_downloads {
            println!(
                "  - {}@{} via {} {}",
                p.qualified_id, p.variant, p.download.source, p.download.location
            );
        }
    }
    if !report.pipelines_installed.is_empty() {
        println!("pipelines installed:");
        for id in &report.pipelines_installed {
            println!("  + {id}");
        }
    }
    if !report.pipeline_conflicts.is_empty() {
        println!("pipeline conflicts (NOT installed):");
        for c in &report.pipeline_conflicts {
            println!("  ! {} ({}): {}", c.pipeline_id, c.file, c.reason);
        }
    }
    if !report.pipeline_dependencies.is_empty() {
        println!("pipeline model dependencies: {}", report.pipeline_dependencies.join(", "));
    }
    for w in &report.warnings {
        println!("warning: {w}");
    }
}

// ─── dry-run：只出适配/校验报告，不落位 ─────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn run_dry_run(
    archive: &Path,
    staging_root: &Path,
    targets: &ImportTargets,
    manifests: &[ep_core::module::ModuleManifest],
    devices: &[ComputeDevice],
    json: bool,
) -> ExitCode {
    let mut errors: Vec<String> = Vec::new();

    // 1) 解包到暂存（zip-slip / symlink / 大小上限防护见 ep_pack::extract）
    let extract_dir = staging_root.join(format!("dryrun-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&extract_dir); // 防上次残留
    let extracted = match extract_pack(archive, &extract_dir, &ExtractLimits::default()) {
        Ok(_) => true,
        Err(e) => {
            errors.push(format!("extract failed: {e}"));
            false
        }
    };

    let mut pack_id = String::new();
    let mut version = String::new();
    let mut name = String::new();
    let mut adaptation: Vec<PackAdaptationEntry> = Vec::new();
    let mut pending: Vec<serde_json::Value> = Vec::new();
    let mut conflicts: Vec<serde_json::Value> = Vec::new();
    let mut pipelines_ok: Vec<String> = Vec::new();

    if extracted {
        // 2) CHECKSUMS 全量校验（先验后一切）
        match ChecksumTable::read(&extract_dir).and_then(|t| {
            t.verify(&extract_dir)?;
            Ok(t)
        }) {
            Ok(_) => {}
            Err(e) => errors.push(format!("checksum verification failed: {e}")),
        }

        // 3) 清单：schema + semver 门禁 + 重复安装检查
        let manifest = match PackManifest::from_file(&extract_dir.join(MANIFEST_FILE_NAME)) {
            Ok(m) => {
                if let Err(list) = m.validate() {
                    errors.extend(list);
                }
                Some(m)
            }
            Err(e) => {
                errors.push(format!("manifest load failed: {e}"));
                None
            }
        };

        if let Some(m) = manifest {
            pack_id = m.pack.id.clone();
            version = m.pack.version.clone();
            name = m.pack.name.clone();

            if let Some(min) = &m.pack.min_ep_version {
                let current = ImportOptions::default().current_ep_version;
                match semver::satisfies_min(&current, min) {
                    Ok(true) => {}
                    Ok(false) => errors.push(format!(
                        "pack requires EntryPoint >= {min}, current version is {current}"
                    )),
                    Err(detail) => errors.push(format!("version comparison failed: {detail}")),
                }
            }

            let registry_path = registry_entry_path(&targets.registry_dir, &m.pack.id);
            match read_installed_pack(&registry_path) {
                Ok(Some(_)) => errors.push(format!(
                    "pack '{}' is already installed ({})",
                    m.pack.id,
                    registry_path.display()
                )),
                Err(e) => errors.push(format!("registry read failed: {e}")),
                Ok(None) => {}
            }

            // 4) 模块解析 + 适配报告（§4.6）+ bundle 双侧预检
            for entry in &m.models {
                let resolved = resolve_entry(manifests, entry);
                adaptation.push(adapt_model(
                    entry,
                    &resolved,
                    &m.compute.backends,
                    devices,
                ));
                if let Ok(r) = &resolved {
                    if entry.mode == ModelMode::Bundle {
                        if !extract_dir.join("models").join(&r.target_dir).is_dir() {
                            errors.push(format!(
                                "bundle model {}@{} declares weights but archive lacks models/{}",
                                entry.qualified_id, entry.variant, r.target_dir
                            ));
                        }
                        let dst = targets.models_dir.join(&r.target_dir);
                        if dst.exists() {
                            errors.push(format!(
                                "bundle model {}@{} conflicts with existing dir {}",
                                entry.qualified_id,
                                entry.variant,
                                dst.display()
                            ));
                        }
                    }
                    if entry.mode == ModelMode::Reference {
                        if let Some(dl) = &r.download {
                            pending.push(serde_json::json!({
                                "qualified_id": entry.qualified_id,
                                "variant": entry.variant,
                                "module_id": r.module_id,
                                "model_id": r.model_id,
                                "target_dir": r.target_dir,
                                "source": dl.source,
                                "location": dl.location,
                                "revision": dl.revision,
                            }));
                        }
                    }
                }
            }

            // 5) 管线：文件存在 + TOML 可解析 + id 提取 + 重名冲突预检
            let (existing_ids, existing_names) = scan_pipeline_targets(&targets.pipelines_dir);
            for (i, pref) in m.pipelines.iter().enumerate() {
                let Some(src) = join_pack_rel(&extract_dir, &pref.file) else {
                    errors.push(format!("pipelines[{i}].file '{}' escapes the pack root", pref.file));
                    continue;
                };
                if !src.is_file() {
                    errors.push(format!(
                        "manifest references missing pipeline file '{}'",
                        pref.file
                    ));
                    continue;
                }
                let text = match std::fs::read_to_string(&src) {
                    Ok(t) => t,
                    Err(e) => {
                        errors.push(format!("pipeline file '{}' unreadable: {e}", pref.file));
                        continue;
                    }
                };
                let doc: Result<toml::Value, _> = toml::from_str(&text);
                let doc = match doc {
                    Ok(d) => d,
                    Err(e) => {
                        errors.push(format!("pipeline file '{}' is invalid TOML: {e}", pref.file));
                        continue;
                    }
                };
                let Some(pipeline_id) = doc
                    .get("pipeline")
                    .and_then(|p| p.get("id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                else {
                    errors.push(format!(
                        "pipeline file '{}' lacks [pipeline].id",
                        pref.file
                    ));
                    continue;
                };
                let file_name = pref
                    .file
                    .rsplit('/')
                    .find(|seg| !seg.is_empty())
                    .unwrap_or(&pref.file)
                    .to_string();
                if let Some(existing) = existing_ids.get(&pipeline_id) {
                    conflicts.push(serde_json::json!({
                        "file": pref.file,
                        "pipeline_id": pipeline_id,
                        "target_file": file_name,
                        "reason": format!("pipeline id '{pipeline_id}' already installed ({existing})"),
                    }));
                } else if existing_names.contains(&file_name.to_ascii_lowercase()) {
                    conflicts.push(serde_json::json!({
                        "file": pref.file,
                        "pipeline_id": pipeline_id,
                        "target_file": file_name,
                        "reason": format!("target file name '{file_name}' already exists"),
                    }));
                } else {
                    pipelines_ok.push(pipeline_id);
                }
            }
        }
    }

    // 暂存整体清理（dry-run 不在目标布局留下任何痕迹）
    let _ = std::fs::remove_dir_all(&extract_dir);

    let ok = errors.is_empty();
    if json {
        output::print_json(&serde_json::json!({
            "ok": ok,
            "dry_run": true,
            "pack_id": pack_id,
            "version": version,
            "name": name,
            "errors": errors,
            "adaptation": adaptation,
            "pending_downloads": pending,
            "pipelines_installable": pipelines_ok,
            "pipeline_conflicts": conflicts,
            "note": "dry run: nothing was placed",
        }));
        return output::exit(if ok { EXIT_OK } else { EXIT_FAILURE });
    }

    println!(
        "dry-run report for '{}' v{} ({})",
        pack_id, version, name
    );
    if !errors.is_empty() {
        println!("would FAIL ({} error(s)):", errors.len());
        output::print_items("  - ", &errors);
    } else {
        println!("would import successfully");
    }
    for a in &adaptation {
        println!(
            "  adapt {}@{}: {} ({})",
            a.qualified_id,
            a.variant,
            verdict_text(a),
            a.reason
        );
    }
    for id in &pipelines_ok {
        println!("  pipeline ready: {id}");
    }
    if !conflicts.is_empty() {
        println!("  pipeline conflicts: {} (would be skipped)", conflicts.len());
    }
    if !pending.is_empty() {
        println!("  reference downloads pending: {}", pending.len());
    }
    println!("dry run: nothing was placed");
    output::exit(if ok { EXIT_OK } else { EXIT_FAILURE })
}

/// 只读扫描既有管线目录：`[pipeline].id` → 文件名 映射 + 小写折叠文件名集合
/// （与库层 scan_existing_pipelines 同语义；库层为私有，dry-run 只读复演）。
fn scan_pipeline_targets(pipelines_dir: &Path) -> (BTreeMap<String, String>, BTreeSet<String>) {
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    let mut names: BTreeSet<String> = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(pipelines_dir) else {
        return (ids, names);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        names.insert(name.to_ascii_lowercase());
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(doc) = text.parse::<toml::Value>() else {
            continue;
        };
        if let Some(id) = doc
            .get("pipeline")
            .and_then(|p| p.get("id"))
            .and_then(|v| v.as_str())
        {
            ids.insert(id.to_string(), name.to_string());
        }
    }
    (ids, names)
}
