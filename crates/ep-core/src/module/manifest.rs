use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::{ComputeBackend, DataType, ModuleCategory};

#[derive(Debug, Error)]
pub enum ModuleError {
    #[error("failed to read manifest file: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("manifest validation failed:\n{}", .0.join("\n"))]
    Validation(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleManifest {
    pub module: ModuleInfo,
    pub runtime: RuntimeConfig,
    pub compute: ComputeConfig,
    #[serde(default)]
    pub models: Vec<ModelDecl>,
    pub interface: InterfaceConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub category: ModuleCategory,
    pub genre: String,
    #[serde(default)]
    pub authors: Vec<String>,
    pub license: Option<String>,
    pub homepage: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeType {
    Python,
    Native,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    #[serde(rename = "type")]
    pub runtime_type: RuntimeType,
    pub python_version: Option<String>,
    pub requirements: Option<String>,
    pub entrypoint: Option<String>,
    pub start_command: Option<String>,
    pub binaries: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeConfig {
    pub backends: Vec<ComputeBackend>,
    pub default_backend: Option<ComputeBackend>,
    pub vram_estimate_mb: Option<u32>,
    pub min_vram_mb: Option<u32>,
    #[serde(default)]
    pub env: Option<HashMap<String, HashMap<String, String>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelSource {
    Huggingface,
    Modelscope,
    Url,
}

impl ModelSource {
    /// 来源的字符串形式（用于错误信息、元数据与 API 输出）
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Huggingface => "huggingface",
            Self::Modelscope => "modelscope",
            Self::Url => "url",
        }
    }
}

impl std::fmt::Display for ModelSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 模型的备用下载源（镜像）声明。
///
/// TOML 形态为 `[[models.mirrors]]` 嵌套数组，例如：
///
/// ```toml
/// [[models]]
/// id = "large-v3"
/// source = "huggingface"
/// repo_id = "Systran/faster-whisper-large-v3"
///
/// [[models.mirrors]]
/// source = "modelscope"
/// repo_id = "pengzhendong/faster-whisper-large-v3"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMirror {
    /// 镜像来源（必须与主 source 不同，且必须是仓库类来源）
    pub source: ModelSource,
    /// 镜像仓库 ID（如 "pengzhendong/faster-whisper-large-v3"）
    pub repo_id: String,
    /// 镜像侧的版本/分支（缺省时使用来源默认值）
    #[serde(default)]
    pub revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDecl {
    pub id: String,
    pub name: String,
    pub source: ModelSource,
    pub repo_id: Option<String>,
    pub url: Option<String>,
    pub target_dir: String,
    pub revision: Option<String>,
    pub size_estimate_mb: Option<u32>,
    /// 全限定模型 ID（`publisher.vendor.model`，§4.3 冻结契约）。
    ///
    /// 仓库内置模块可留空，由消费侧经 `model_id::normalize_legacy` 归一
    /// （`ep.<vendor>.<model>` 向后兼容层）；整合包导入/导出时写入。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualified_id: Option<String>,
    /// 变体级 VRAM 估算（MB）— §6.3 VRAM 账本的数据源。
    ///
    /// 缺省时回退模块级 `[compute].vram_estimate_mb`，
    /// 见 [`ModuleManifest::resolve_vram_estimate`]（变体优先、模块兜底）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram_estimate_mb: Option<u64>,
    #[serde(default)]
    pub default: bool,
    /// 备用下载源列表（TOML 中为 `[[models.mirrors]]`）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mirrors: Vec<ModelMirror>,
}

impl ModelDecl {
    /// 解析实际使用的下载源。
    ///
    /// - `None` → 使用主 source（声明自身的字段）
    /// - `Some(s)` 且等于主 source → 使用主字段
    /// - `Some(s)` 存在于 mirrors → 使用对应 mirror 字段
    /// - 其余情况 → 报错（中文，列出可用来源）
    ///
    /// 返回 `(source, repo_id 或 url, revision)`。
    pub fn resolve(
        &self,
        requested: Option<ModelSource>,
    ) -> anyhow::Result<(ModelSource, String, Option<String>)> {
        let target = requested.unwrap_or(self.source);

        if target == self.source {
            let location = match self.source {
                ModelSource::Huggingface | ModelSource::Modelscope => {
                    self.repo_id.clone().ok_or_else(|| {
                        anyhow::anyhow!(
                            "model '{}' has source '{}' but does not declare a repo_id field",
                            self.id,
                            self.source
                        )
                    })?
                }
                ModelSource::Url => self.url.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "model '{}' has source 'url' but does not declare a url field",
                        self.id
                    )
                })?,
            };
            return Ok((self.source, location, self.revision.clone()));
        }

        if let Some(mirror) = self.mirrors.iter().find(|m| m.source == target) {
            return Ok((mirror.source, mirror.repo_id.clone(), mirror.revision.clone()));
        }

        let list = self
            .available_sources()
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "model '{}' does not support download source '{}', available sources: {}",
            self.id,
            target,
            list
        )
    }

    /// 主 source + 所有 mirrors 的来源列表（去重、保持顺序，主源在前）
    pub fn available_sources(&self) -> Vec<ModelSource> {
        let mut sources = vec![self.source];
        for mirror in &self.mirrors {
            if !sources.contains(&mirror.source) {
                sources.push(mirror.source);
            }
        }
        sources
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InterfaceType {
    Http,
    Cli,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceConfig {
    #[serde(rename = "type")]
    pub interface_type: InterfaceType,
    pub health_endpoint: Option<String>,
    pub ready_timeout_secs: Option<u32>,
    pub working_dir: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityDecl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityDecl {
    pub name: String,
    pub description: String,
    pub input_type: DataType,
    pub output_type: DataType,
    pub max_file_size_mb: Option<u32>,
    #[serde(default)]
    pub supports_batch: bool,
    #[serde(default)]
    pub params: Option<HashMap<String, ParamSchema>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamSchema {
    #[serde(rename = "type")]
    pub param_type: String,
    pub default: Option<serde_json::Value>,
    pub description: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    #[serde(rename = "enum")]
    pub enum_values: Option<Vec<String>>,
    pub options: Option<Vec<String>>,
}

impl ModuleManifest {
    pub fn from_file(path: &Path) -> Result<Self, ModuleError> {
        let content = std::fs::read_to_string(path)?;
        let manifest: Self = toml::from_str(&content)?;
        Ok(manifest)
    }

    /// 解析指定变体的 VRAM 估算（MB）— §6.3 管线 VRAM 账本的数据源。
    ///
    /// 解析顺序：
    /// 1. 变体级 `[[models]].vram_estimate_mb`（按 `variant_id` 匹配 `id`）优先；
    /// 2. 变体未声明、或 `variant_id` 未命中任何变体时，回退模块级
    ///    `[compute].vram_estimate_mb`（u32 → u64 无损加宽）；
    /// 3. 两者皆缺返回 `None`（未知，由消费侧决定展示/放行策略）。
    pub fn resolve_vram_estimate(&self, variant_id: &str) -> Option<u64> {
        let variant = self
            .models
            .iter()
            .find(|m| m.id == variant_id)
            .and_then(|m| m.vram_estimate_mb);
        variant.or_else(|| self.compute.vram_estimate_mb.map(u64::from))
    }

    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        let id = &self.module.id;
        if id.is_empty() {
            errors.push("module.id must not be empty".to_string());
        } else if !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            errors.push(format!(
                "module.id '{id}' contains invalid characters (allowed: a-z, 0-9, hyphen)"
            ));
        }

        if self.module.name.is_empty() {
            errors.push("module.name must not be empty".to_string());
        }
        if self.module.version.is_empty() {
            errors.push("module.version must not be empty".to_string());
        }
        if self.module.description.is_empty() {
            errors.push("module.description must not be empty".to_string());
        }
        if self.module.genre.is_empty() {
            errors.push("module.genre must not be empty".to_string());
        }

        if self.compute.backends.is_empty() {
            errors.push("compute.backends must not be empty".to_string());
        }

        match self.runtime.runtime_type {
            RuntimeType::Python => {
                if self.runtime.python_version.is_none() {
                    errors.push(
                        "runtime.python_version is required when type = \"python\"".to_string(),
                    );
                }
            }
            RuntimeType::Native => {
                if self
                    .runtime
                    .binaries
                    .as_ref()
                    .is_none_or(|b| b.is_empty())
                {
                    errors.push(
                        "runtime.binaries is required when type = \"native\"".to_string(),
                    );
                }
            }
        }

        for model in &self.models {
            match model.source {
                ModelSource::Huggingface | ModelSource::Modelscope => {
                    if model.repo_id.is_none() {
                        errors.push(format!(
                            "models[{}]: repo_id is required when source = \"{:?}\"",
                            model.id, model.source
                        ));
                    }
                }
                ModelSource::Url => {
                    if model.url.is_none() {
                        errors.push(format!(
                            "models[{}]: url is required when source = \"url\"",
                            model.id
                        ));
                    }
                }
            }

            for mirror in &model.mirrors {
                if mirror.source == model.source {
                    errors.push(format!(
                        "models[{}]: mirror source \"{}\" duplicates the primary source",
                        model.id, mirror.source
                    ));
                }
                if mirror.source == ModelSource::Url {
                    errors.push(format!(
                        "models[{}]: mirror source must be \"huggingface\" or \"modelscope\" (repo-based)",
                        model.id
                    ));
                }
                if mirror.repo_id.trim().is_empty() {
                    errors.push(format!(
                        "models[{}]: mirror repo_id must not be empty",
                        model.id
                    ));
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const VALID_TOML: &str = r#"
[module]
id = "faster-whisper"
name = "Faster-Whisper ASR"
version = "1.1.0"
description = "High-speed speech recognition"
category = "asr"
genre = "whisper"
authors = ["Test"]
license = "MIT"
tags = ["speech"]

[runtime]
type = "python"
python_version = ">=3.10,<3.13"
requirements = "requirements.txt"
entrypoint = "adapter.py"

[compute]
backends = ["cuda", "cpu"]
default_backend = "cuda"
vram_estimate_mb = 4096

[[models]]
id = "large-v3"
name = "Whisper Large V3"
source = "huggingface"
repo_id = "Systran/faster-whisper-large-v3"
target_dir = "faster-whisper-large-v3"
size_estimate_mb = 3100
default = true

[interface]
type = "http"
health_endpoint = "/health"
ready_timeout_secs = 90

[[interface.capabilities]]
name = "transcribe"
description = "Speech to text"
input_type = "audio"
output_type = "json"

[interface.capabilities.params]
language = { type = "string", default = "auto", description = "Language code" }
beam_size = { type = "integer", default = 5, min = 1, max = 20 }
"#;

    #[test]
    fn test_parse_valid_manifest() {
        let manifest: ModuleManifest = toml::from_str(VALID_TOML).unwrap();
        assert_eq!(manifest.module.id, "faster-whisper");
        assert_eq!(manifest.module.category, ModuleCategory::Asr);
        assert_eq!(manifest.runtime.runtime_type, RuntimeType::Python);
        assert_eq!(manifest.compute.backends.len(), 2);
        assert_eq!(manifest.models.len(), 1);
        assert!(manifest.models[0].default);
        assert_eq!(manifest.interface.interface_type, InterfaceType::Http);
        assert_eq!(manifest.interface.capabilities.len(), 1);

        let cap = &manifest.interface.capabilities[0];
        assert_eq!(cap.name, "transcribe");
        assert_eq!(cap.input_type, DataType::Audio);
        let params = cap.params.as_ref().unwrap();
        assert!(params.contains_key("language"));
        assert!(params.contains_key("beam_size"));

        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_missing_required_field() {
        let toml_str = r#"
[module]
id = "test-module"
name = "Test"

[runtime]
type = "python"

[compute]
backends = ["cpu"]

[interface]
type = "http"
"#;
        let result = toml::from_str::<ModuleManifest>(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_invalid_id() {
        let toml_str = r#"
[module]
id = "Invalid_ID!"
name = "Test"
version = "0.1.0"
description = "desc"
category = "custom"
genre = "test"

[runtime]
type = "python"
python_version = ">=3.10"

[compute]
backends = ["cpu"]

[interface]
type = "cli"
"#;
        let manifest: ModuleManifest = toml::from_str(toml_str).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("invalid characters")));
    }

    #[test]
    fn test_validate_native_missing_binaries() {
        let toml_str = r#"
[module]
id = "native-tool"
name = "Native Tool"
version = "1.0.0"
description = "A native tool"
category = "denoise"
genre = "test"

[runtime]
type = "native"

[compute]
backends = ["cpu"]

[interface]
type = "cli"
"#;
        let manifest: ModuleManifest = toml::from_str(toml_str).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("binaries")));
    }

    #[test]
    fn test_from_file() {
        let dir = std::env::temp_dir().join("ep_test_manifest");
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("module.toml");
        let mut f = std::fs::File::create(&file_path).unwrap();
        f.write_all(VALID_TOML.as_bytes()).unwrap();
        drop(f);

        let manifest = ModuleManifest::from_file(&file_path).unwrap();
        assert_eq!(manifest.module.id, "faster-whisper");

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── mirrors：解析 / resolve / 校验 ─────────────────────────────────

    const MIRROR_TOML: &str = r#"
[module]
id = "faster-whisper"
name = "Faster-Whisper ASR"
version = "1.1.0"
description = "High-speed speech recognition"
category = "asr"
genre = "whisper"

[runtime]
type = "python"
python_version = ">=3.10,<3.13"

[compute]
backends = ["cpu"]

[[models]]
id = "large-v3"
name = "Whisper Large V3"
source = "huggingface"
repo_id = "Systran/faster-whisper-large-v3"
target_dir = "faster-whisper-large-v3"
size_estimate_mb = 3100
default = true

[[models.mirrors]]
source = "modelscope"
repo_id = "pengzhendong/faster-whisper-large-v3"

[[models.mirrors]]
source = "url"
repo_id = "should-not-pass-validation"

[interface]
type = "http"
"#;

    #[test]
    fn test_parse_mirrors() {
        let manifest: ModuleManifest = toml::from_str(MIRROR_TOML).unwrap();
        let model = &manifest.models[0];
        assert_eq!(model.mirrors.len(), 2);
        assert_eq!(model.mirrors[0].source, ModelSource::Modelscope);
        assert_eq!(model.mirrors[0].repo_id, "pengzhendong/faster-whisper-large-v3");
        assert!(model.mirrors[0].revision.is_none());
    }

    #[test]
    fn test_no_mirrors_defaults_empty() {
        let manifest: ModuleManifest = toml::from_str(VALID_TOML).unwrap();
        assert!(manifest.models[0].mirrors.is_empty());
        // 校验应通过（无 mirrors）
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_resolve_primary_when_none() {
        let manifest: ModuleManifest = toml::from_str(MIRROR_TOML).unwrap();
        let model = &manifest.models[0];

        let (source, location, revision) = model.resolve(None).unwrap();
        assert_eq!(source, ModelSource::Huggingface);
        assert_eq!(location, "Systran/faster-whisper-large-v3");
        assert!(revision.is_none());
    }

    #[test]
    fn test_resolve_explicit_primary() {
        let manifest: ModuleManifest = toml::from_str(MIRROR_TOML).unwrap();
        let model = &manifest.models[0];

        let (source, location, _) = model.resolve(Some(ModelSource::Huggingface)).unwrap();
        assert_eq!(source, ModelSource::Huggingface);
        assert_eq!(location, "Systran/faster-whisper-large-v3");
    }

    #[test]
    fn test_resolve_mirror() {
        let manifest: ModuleManifest = toml::from_str(MIRROR_TOML).unwrap();
        let model = &manifest.models[0];

        let (source, location, revision) = model.resolve(Some(ModelSource::Modelscope)).unwrap();
        assert_eq!(source, ModelSource::Modelscope);
        assert_eq!(location, "pengzhendong/faster-whisper-large-v3");
        assert!(revision.is_none());
    }

    #[test]
    fn test_resolve_unavailable_source_error() {
        let toml_str = MIRROR_TOML.replace(
            "[[models.mirrors]]\nsource = \"url\"\nrepo_id = \"should-not-pass-validation\"",
            "",
        );
        let manifest: ModuleManifest = toml::from_str(&toml_str).unwrap();
        let model = &manifest.models[0];

        let err = model.resolve(Some(ModelSource::Url)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("does not support download source"), "msg: {msg}");
        assert!(msg.contains("available sources"), "msg: {msg}");
        assert!(msg.contains("huggingface"), "msg: {msg}");
    }

    #[test]
    fn test_available_sources_dedup() {
        let manifest: ModuleManifest = toml::from_str(MIRROR_TOML).unwrap();
        let model = &manifest.models[0];
        assert_eq!(
            model.available_sources(),
            vec![ModelSource::Huggingface, ModelSource::Modelscope, ModelSource::Url]
        );
    }

    #[test]
    fn test_validate_mirror_duplicate_source() {
        let toml_str = r#"
[module]
id = "m"
name = "M"
version = "1.0.0"
description = "d"
category = "asr"
genre = "g"

[runtime]
type = "python"
python_version = ">=3.10"

[compute]
backends = ["cpu"]

[[models]]
id = "x"
name = "X"
source = "huggingface"
repo_id = "org/x"
target_dir = "x"

[[models.mirrors]]
source = "huggingface"
repo_id = "org2/x"

[interface]
type = "http"
"#;
        let manifest: ModuleManifest = toml::from_str(toml_str).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("duplicates the primary source")),
            "errors: {errors:?}"
        );
    }

    #[test]
    fn test_validate_mirror_empty_repo_id() {
        let toml_str = r#"
[module]
id = "m"
name = "M"
version = "1.0.0"
description = "d"
category = "asr"
genre = "g"

[runtime]
type = "python"
python_version = ">=3.10"

[compute]
backends = ["cpu"]

[[models]]
id = "x"
name = "X"
source = "huggingface"
repo_id = "org/x"
target_dir = "x"

[[models.mirrors]]
source = "modelscope"
repo_id = "  "

[interface]
type = "http"
"#;
        let manifest: ModuleManifest = toml::from_str(toml_str).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("mirror repo_id must not be empty")),
            "errors: {errors:?}"
        );
    }

    #[test]
    fn test_real_faster_whisper_manifest_mirrors() {
        // 回归测试：仓库内真实的 faster-whisper 清单必须能解析并通过校验
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../modules/faster-whisper/module.toml");
        if !path.exists() {
            return; // 脱离完整仓库布局时跳过
        }
        let manifest = ModuleManifest::from_file(&path).unwrap();
        assert!(manifest.validate().is_ok());
        assert_eq!(manifest.models.len(), 3);
        for model in &manifest.models {
            assert_eq!(model.mirrors.len(), 1, "model {} should have 1 mirror", model.id);
            assert_eq!(model.mirrors[0].source, ModelSource::Modelscope);
            assert!(model.mirrors[0].repo_id.starts_with("pengzhendong/faster-whisper-"));
            // available_sources = 主源 + mirror
            assert_eq!(
                model.available_sources(),
                vec![ModelSource::Huggingface, ModelSource::Modelscope]
            );
        }
    }

    // ── 变体级 vram_estimate_mb / qualified_id（§4.3/§6.3）─────────────

    const VRAM_TOML: &str = r#"
[module]
id = "faster-whisper"
name = "Faster-Whisper ASR"
version = "1.1.0"
description = "High-speed speech recognition"
category = "asr"
genre = "whisper"

[runtime]
type = "python"
python_version = ">=3.10,<3.13"

[compute]
backends = ["cuda", "cpu"]
vram_estimate_mb = 4096

[[models]]
id = "large-v3"
name = "Whisper Large V3"
source = "huggingface"
repo_id = "Systran/faster-whisper-large-v3"
target_dir = "faster-whisper-large-v3"
qualified_id = "ep.systran.faster-whisper"
vram_estimate_mb = 8192
default = true

[[models]]
id = "medium"
name = "Whisper Medium"
source = "huggingface"
repo_id = "Systran/faster-whisper-medium"
target_dir = "faster-whisper-medium"

[interface]
type = "http"
"#;

    #[test]
    fn test_parse_variant_vram_and_qualified_id() {
        let manifest: ModuleManifest = toml::from_str(VRAM_TOML).unwrap();
        assert_eq!(manifest.models[0].vram_estimate_mb, Some(8192u64));
        assert_eq!(
            manifest.models[0].qualified_id.as_deref(),
            Some("ep.systran.faster-whisper")
        );
        // 未声明的变体 → None（serde default，向后兼容）
        assert_eq!(manifest.models[1].vram_estimate_mb, None);
        assert_eq!(manifest.models[1].qualified_id, None);
    }

    #[test]
    fn test_legacy_manifest_new_fields_default_none() {
        // 旧格式清单（无 qualified_id / vram_estimate_mb）正常加载
        let manifest: ModuleManifest = toml::from_str(VALID_TOML).unwrap();
        assert_eq!(manifest.models[0].qualified_id, None);
        assert_eq!(manifest.models[0].vram_estimate_mb, None);
    }

    #[test]
    fn test_resolve_vram_estimate_variant_priority() {
        let manifest: ModuleManifest = toml::from_str(VRAM_TOML).unwrap();
        // 变体级声明优先于模块级兜底
        assert_eq!(manifest.resolve_vram_estimate("large-v3"), Some(8192));
    }

    #[test]
    fn test_resolve_vram_estimate_module_fallback() {
        let manifest: ModuleManifest = toml::from_str(VRAM_TOML).unwrap();
        // 变体未声明 → 模块级 [compute].vram_estimate_mb 兜底
        assert_eq!(manifest.resolve_vram_estimate("medium"), Some(4096));
        // 未知变体同样回退模块级（防御：管线 pin 的变体可能尚未下载）
        assert_eq!(manifest.resolve_vram_estimate("no-such-variant"), Some(4096));
    }

    #[test]
    fn test_resolve_vram_estimate_none_when_unknown() {
        let toml_str = VRAM_TOML
            .replace("vram_estimate_mb = 8192\n", "")
            .replace("vram_estimate_mb = 4096\n", "");
        let manifest: ModuleManifest = toml::from_str(&toml_str).unwrap();
        // 变体与模块均未声明 → None（未知）
        assert_eq!(manifest.resolve_vram_estimate("large-v3"), None);
        assert_eq!(manifest.resolve_vram_estimate("medium"), None);
    }
}
