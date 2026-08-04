//! `ep-pack export <pack-id> [--root <dir>] [--modules-dir <dir>] [-o out.epzip]`
//!
//! 已装包（注册表 `runtime/packs/<pack-id>.json`）→ 重建 `.epzip`：
//!
//! 1. 注册表条目 + 模块清单解析（target_dir / backends 重建依据）；
//! 2. 清单重建：`[pack]` 身份来自注册表，`[compute].backends` 取各模型所属
//!    模块声明后端的并集（无模型时 `["cpu"]`），`[[models]]`/`[[pipelines]]`
//!    来自注册表记录；
//! 3. 暂存目录组装于 `<root>/.pack-staging/export-<pack-id>/`：管线文件复制、
//!    bundle 权重**优先硬链接**（同卷零拷贝，失败回退复制）；
//! 4. `build_pack` 确定性打包（CHECKSUMS 自动生成）。
//!
//! CLI 离线工具不保留原始 `ep-pack.toml`（导入时不持久化），故重建清单的
//! authors/license/homepage/min_ep_version/tags 信息不可恢复——按缺省值留空。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ep_pack::build::{build_pack, BuildPlan};
use ep_pack::import::{registry_entry_path, read_installed_pack};
use ep_pack::manifest::{
    ModelMode, PackCompute, PackInfo, PackManifest, PackModelEntry, PackPipelineRef,
};
use ep_core::types::ComputeBackend;

use crate::args::{self, OptDef};
use crate::output::{self, EXIT_FAILURE, EXIT_OK, EXIT_USAGE};
use crate::resolve::{load_module_catalog, resolve_entry};

const USAGE: &str = "usage: ep-pack export <pack-id> [--root <dir>] [--modules-dir <dir>] [-o|--output <file.epzip>] [--json]";

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
            name: "output",
            long: "--output",
            short: Some("-o"),
            takes_value: true,
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
    let pack_id = positional[0].as_str();
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

    // 1) 注册表条目
    let registry_path = registry_entry_path(&root.join("runtime").join("packs"), pack_id);
    let pack = match read_installed_pack(&registry_path) {
        Ok(Some(p)) => p,
        Ok(None) => {
            return output::fail(
                EXIT_USAGE,
                json,
                vec![format!(
                    "pack '{pack_id}' is not installed (registry entry {} not found)",
                    registry_path.display()
                )],
            )
        }
        Err(e) => return output::fail(EXIT_FAILURE, json, vec![format!("registry read failed: {e}")]),
    };

    // 2) 模块解析（target_dir / backends）
    let catalog = load_module_catalog(&modules_dir);
    let mut errors: Vec<String> = Vec::new();
    let mut backends_union: Vec<ComputeBackend> = Vec::new();
    let mut bundle_targets: Vec<(String, String)> = Vec::new(); // (qualified_id@variant, target_dir)
    for m in &pack.models {
        let entry = PackModelEntry {
            qualified_id: m.qualified_id.clone(),
            variant: m.variant.clone(),
            mode: m.mode,
            tags: m.tags.clone(),
        };
        match resolve_entry(&catalog.manifests, &entry) {
            Ok(r) => {
                for b in r.backends {
                    if !backends_union.contains(&b) {
                        backends_union.push(b);
                    }
                }
                if m.mode == ModelMode::Bundle {
                    bundle_targets.push((format!("{}@{}", m.qualified_id, m.variant), r.target_dir));
                }
            }
            Err(e) => errors.push(e),
        }
    }

    // 3) 管线 id → 已装文件 映射
    let pipelines_dir = root.join("config").join("pipelines");
    let installed_files = scan_pipeline_files(&pipelines_dir);
    let mut pipeline_files: Vec<(String, PathBuf)> = Vec::new(); // (落位文件名, 源路径)
    for id in &pack.pipelines {
        match installed_files.get(id) {
            Some(path) => {
                let file_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("pipeline.toml")
                    .to_string();
                pipeline_files.push((file_name, path.clone()));
            }
            None => errors.push(format!(
                "registered pipeline '{id}' not found under {}",
                pipelines_dir.display()
            )),
        }
    }

    if !errors.is_empty() {
        return output::fail(EXIT_FAILURE, json, errors);
    }

    // 4) 清单重建
    let backends = if backends_union.is_empty() {
        vec![ComputeBackend::Cpu]
    } else {
        backends_union
    };
    let manifest = PackManifest {
        pack: PackInfo {
            id: pack.id.clone(),
            version: pack.version.clone(),
            name: pack.name.clone().unwrap_or_else(|| pack.id.clone()),
            description: pack.description.clone().unwrap_or_default(),
            authors: Vec::new(),
            license: None,
            homepage: None,
            min_ep_version: None,
            tags: Vec::new(),
        },
        compute: PackCompute {
            backends,
            notes: HashMap::new(),
        },
        models: pack
            .models
            .iter()
            .map(|m| PackModelEntry {
                qualified_id: m.qualified_id.clone(),
                variant: m.variant.clone(),
                mode: m.mode,
                tags: m.tags.clone(),
            })
            .collect(),
        pipelines: pipeline_files
            .iter()
            .map(|(name, _)| PackPipelineRef {
                file: format!("pipelines/{name}"),
            })
            .collect(),
    };
    let manifest_toml = match toml::to_string(&manifest) {
        Ok(t) => t,
        Err(e) => {
            return output::fail(
                EXIT_FAILURE,
                json,
                vec![format!("failed to serialize rebuilt manifest: {e}")],
            )
        }
    };

    // 5) 暂存组装（硬链接优先，跨卷回退复制）
    let staging = root
        .join(".pack-staging")
        .join(format!("export-{}", pack.id));
    let _ = std::fs::remove_dir_all(&staging); // 清上次残留
    if let Err(e) = std::fs::create_dir_all(&staging) {
        return output::fail(
            EXIT_FAILURE,
            json,
            vec![format!("failed to create staging dir {}: {e}", staging.display())],
        );
    }

    let mut stage_errors: Vec<String> = Vec::new();
    if let Err(e) = std::fs::write(staging.join("ep-pack.toml"), manifest_toml) {
        stage_errors.push(format!("failed to write staging manifest: {e}"));
    }
    if !pipeline_files.is_empty() {
        let staging_pipelines = staging.join("pipelines");
        if let Err(e) = std::fs::create_dir_all(&staging_pipelines) {
            stage_errors.push(format!(
                "failed to create {}: {e}",
                staging_pipelines.display()
            ));
        } else {
            for (name, src) in &pipeline_files {
                let dst = staging_pipelines.join(name);
                if let Err(e) = std::fs::copy(src, &dst) {
                    stage_errors.push(format!(
                        "failed to stage pipeline {} -> {}: {e}",
                        src.display(),
                        dst.display()
                    ));
                }
            }
        }
    }
    for (model_pin, target_dir) in &bundle_targets {
        let src = root.join("models").join(target_dir);
        if !src.is_dir() {
            stage_errors.push(format!(
                "bundle model {model_pin} registered but weights dir {} is missing",
                src.display()
            ));
            continue;
        }
        let dst = staging.join("models").join(target_dir);
        if let Err(e) = link_or_copy_tree(&src, &dst) {
            stage_errors.push(format!(
                "failed to stage bundle weights for {model_pin}: {e}"
            ));
        }
    }
    if !stage_errors.is_empty() {
        let _ = std::fs::remove_dir_all(&staging);
        return output::fail(EXIT_FAILURE, json, stage_errors);
    }

    // 6) 确定性打包
    let output_path = match parsed.value("output") {
        Some(o) => PathBuf::from(o),
        None => std::env::current_dir()
            .unwrap_or_default()
            .join(format!("{}-{}.epzip", pack.id, pack.version)),
    };
    let result = match build_pack(&BuildPlan::new(&staging, &output_path)) {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return output::fail(EXIT_FAILURE, json, vec![format!("build failed: {e}")]);
        }
    };
    let _ = std::fs::remove_dir_all(&staging);

    if json {
        output::print_json(&serde_json::json!({
            "ok": true,
            "pack_id": pack.id,
            "version": pack.version,
            "archive": result.archive_path.display().to_string(),
            "file_count": result.file_count,
            "total_bytes": result.total_bytes,
            "models": pack.models.len(),
            "pipelines": pack.pipelines,
        }));
    } else {
        println!(
            "exported pack '{}' v{} -> {} ({} files, {})",
            pack.id,
            pack.version,
            result.archive_path.display(),
            result.file_count,
            output::human_bytes(result.total_bytes)
        );
    }
    output::exit(EXIT_OK)
}

/// 扫描管线目录：`[pipeline].id` → 文件路径（无法解析的文件跳过）。
fn scan_pipeline_files(pipelines_dir: &Path) -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(pipelines_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
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
            out.insert(id.to_string(), path);
        }
    }
    out
}

/// 递归组装目录树：逐文件硬链接（同卷零拷贝），失败回退复制。
/// 拒绝符号链接以外的非常规文件（与 build 侧安全检查语义一致）。
fn link_or_copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("symlink in model dir: {}", from.display()),
            ));
        }
        if ft.is_dir() {
            link_or_copy_tree(&from, &to)?;
        } else if ft.is_file() {
            if std::fs::hard_link(&from, &to).is_err() {
                std::fs::copy(&from, &to)?;
            }
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("non-regular file in model dir: {}", from.display()),
            ));
        }
    }
    Ok(())
}
