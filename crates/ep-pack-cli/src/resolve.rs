//! 本机模块解析（import/export 的 resolve 回调实现）— 读 `<modules-dir>/*/module.toml`，
//! 按 §4.3 匹配 `qualified_id` + 变体。
//!
//! 匹配规则与 daemon 侧（ep-daemon `api/packs.rs::resolve_entry`，B2）一致：
//! `decl.qualified_id` 规范形 == 条目 qualified_id，且 `decl.id` == 条目 variant。
//! reference 模式的下载描述符取模块声明的主下载源（repo_id / url）。
//!
//! CLI 为离线作者工具：不触网、不下载——reference 解析产物只进报告。

use std::path::{Path, PathBuf};

use ep_core::module::{discover_modules, ModelDecl, ModelSource, ModuleManifest};
use ep_core::model_id::QualifiedId;
use ep_pack::import::{PendingDownload, ResolvedModel};
use ep_pack::manifest::{ModelMode, PackModelEntry};

/// 模块清单加载结果。
pub struct ModuleCatalog {
    /// 成功解析的模块清单（resolve 输入）
    pub manifests: Vec<ModuleManifest>,
    /// 无法读取/解析的 module.toml（人类可读模式提示用；不致命）
    pub unreadable: Vec<PathBuf>,
}

/// 遍历 `<modules_dir>/*/module.toml` 并解析（目录不存在 → 空目录）。
pub fn load_module_catalog(modules_dir: &Path) -> ModuleCatalog {
    let discovered = discover_modules(modules_dir);
    let mut manifests = Vec::new();
    let mut unreadable = Vec::new();
    for d in discovered {
        match d.manifest {
            Some(m) => manifests.push(m),
            None => unreadable.push(d.path.join("module.toml")),
        }
    }
    ModuleCatalog {
        manifests,
        unreadable,
    }
}

/// 按 qualified_id + variant 在模块清单中解析模型声明（daemon 同款规则）。
pub fn resolve_entry(
    manifests: &[ModuleManifest],
    entry: &PackModelEntry,
) -> Result<ResolvedModel, String> {
    for mf in manifests {
        for decl in &mf.models {
            let Some(q) = decl.qualified_id.as_deref() else {
                continue;
            };
            let Ok(parsed) = QualifiedId::parse(q) else {
                continue;
            };
            if parsed.to_canonical() != entry.qualified_id || decl.id != entry.variant {
                continue;
            }
            let download = if entry.mode == ModelMode::Reference {
                Some(reference_descriptor(mf, decl)?)
            } else {
                None
            };
            return Ok(ResolvedModel {
                module_id: mf.module.id.clone(),
                model_id: decl.id.clone(),
                target_dir: decl.target_dir.clone(),
                backends: mf.compute.backends.clone(),
                download,
            });
        }
    }
    Err(format!(
        "no installed module provides model {}@{} (searched module manifests)",
        entry.qualified_id, entry.variant
    ))
}

/// reference 下载描述符解析（缺 repo_id/url → Err → 适配判 Unsupported，
/// 语义与 daemon 侧 reference_descriptor 一致）。
fn reference_descriptor(
    mf: &ModuleManifest,
    decl: &ModelDecl,
) -> Result<PendingDownload, String> {
    match decl.source {
        ModelSource::Huggingface | ModelSource::Modelscope => {
            let location = decl.repo_id.clone().ok_or_else(|| {
                format!(
                    "module '{}' model '{}' declares {} source without repo_id",
                    mf.module.id, decl.id, decl.source
                )
            })?;
            Ok(PendingDownload {
                source: decl.source.as_str().to_string(),
                location,
                revision: decl.revision.clone(),
            })
        }
        ModelSource::Url => {
            let location = decl.url.clone().ok_or_else(|| {
                format!(
                    "module '{}' model '{}' declares url source without url",
                    mf.module.id, decl.id
                )
            })?;
            Ok(PendingDownload {
                source: decl.source.as_str().to_string(),
                location,
                revision: decl.revision.clone(),
            })
        }
        ModelSource::LocalImport => {
            // 本地自建（E7）：无远端可下，打包面按「引用声明」处理——
            // 打包器只校验 target_dir 存在并收录文件，不做下载
            let location = decl.target_dir.clone();
            Ok(PendingDownload {
                source: decl.source.as_str().to_string(),
                location,
                revision: decl.revision.clone(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn module_toml(module_id: &str, qualified_id: &str, variant: &str, target_dir: &str) -> String {
        format!(
            r#"
[module]
id = "{module_id}"
name = "{module_id}"
version = "1.0.0"
description = "test module"
category = "asr"
genre = "test"

[runtime]
type = "python"

[compute]
backends = ["cuda", "cpu"]

[[models]]
id = "{variant}"
name = "{variant}"
source = "huggingface"
repo_id = "acme/{variant}"
target_dir = "{target_dir}"
qualified_id = "{qualified_id}"

[interface]
type = "http"
"#
        )
    }

    fn entry(qid: &str, variant: &str, mode: ModelMode) -> PackModelEntry {
        PackModelEntry {
            qualified_id: qid.to_string(),
            variant: variant.to_string(),
            mode,
            tags: Vec::new(),
        }
    }

    #[test]
    fn catalog_and_resolve_match_daemon_rules() {
        let root = std::env::temp_dir().join(format!(
            "ep-pack-cli-resolve-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        write_file(
            &root.join("modules").join("asr").join("module.toml"),
            &module_toml("asr", "ep.acme.asr", "v1", "asr-v1"),
        );

        let catalog = load_module_catalog(&root.join("modules"));
        assert_eq!(catalog.manifests.len(), 1);
        assert!(catalog.unreadable.is_empty());

        // bundle 模式：无下载描述符
        let r = resolve_entry(&catalog.manifests, &entry("ep.acme.asr", "v1", ModelMode::Bundle))
            .unwrap();
        assert_eq!(r.module_id, "asr");
        assert_eq!(r.target_dir, "asr-v1");
        assert!(r.download.is_none());

        // reference 模式：主下载源描述符
        let r =
            resolve_entry(&catalog.manifests, &entry("ep.acme.asr", "v1", ModelMode::Reference))
                .unwrap();
        let dl = r.download.unwrap();
        assert_eq!(dl.source, "huggingface");
        assert_eq!(dl.location, "acme/v1");

        // 变体不匹配 / qualified_id 不匹配 → Err
        assert!(resolve_entry(&catalog.manifests, &entry("ep.acme.asr", "v2", ModelMode::Bundle))
            .is_err());
        assert!(resolve_entry(&catalog.manifests, &entry("ep.other.asr", "v1", ModelMode::Bundle))
            .is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_modules_dir_yields_empty_catalog() {
        let root = std::env::temp_dir().join(format!(
            "ep-pack-cli-resolve-missing-{}",
            std::process::id()
        ));
        let catalog = load_module_catalog(&root);
        assert!(catalog.manifests.is_empty());
        assert!(catalog.unreadable.is_empty());
    }
}
