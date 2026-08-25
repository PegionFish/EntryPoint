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
    /// 后端相关依赖文件（MODULE_SPEC §2.6 schema 冻结，HETERO_DIST_PLAN M2 落地消费）。
    ///
    /// key = 计算后端名（与 `[compute].backends` 同一词表），value = 依赖文件路径
    /// （相对于模块目录，语义同 `runtime.requirements`）。TOML 中 inline table
    /// 与子表两种写法等价：
    ///
    /// ```toml
    /// [runtime]
    /// requirements_by_backend = { cuda = "requirements-cuda.txt", rocm = "requirements-rocm.txt", cpu = "requirements.txt" }
    /// ```
    ///
    /// 解析规则见 [`RuntimeConfig::resolve_requirements`]；未声明时为空表。
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub requirements_by_backend: HashMap<ComputeBackend, String>,
    pub entrypoint: Option<String>,
    pub start_command: Option<String>,
    pub binaries: Option<HashMap<String, String>>,
}

impl RuntimeConfig {
    /// 按当前后端解析实际使用的依赖文件路径（§2.6 回退语义）。
    ///
    /// - `backend` 有对应条目且非空白 → 使用该条目；
    /// - 否则回退 `runtime.requirements`（缺省 `"requirements.txt"`）。
    pub fn resolve_requirements(&self, backend: Option<ComputeBackend>) -> &str {
        let fallback = self.requirements.as_deref().unwrap_or("requirements.txt");
        backend
            .and_then(|b| self.requirements_by_backend.get(&b))
            .map(String::as_str)
            .filter(|p| !p.trim().is_empty())
            .unwrap_or(fallback)
    }
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
    /// 本地自建/导入（无远端 URL）：E7 自建 ONNX 等场景——获取方式由
    /// 模块 README 说明（脚本导出/浏览器导入），平台按「存在即就绪」
    /// 处理，不做任何下载。
    #[serde(rename = "local_import")]
    LocalImport,
}

impl ModelSource {
    /// 来源的字符串形式（用于错误信息、元数据与 API 输出）
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Huggingface => "huggingface",
            Self::Modelscope => "modelscope",
            Self::Url => "url",
            Self::LocalImport => "local_import",
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
                // 本地自建：无远端定位符，位置即目标目录（存在性由就绪检查判定）
                ModelSource::LocalImport => self.target_dir.clone(),
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
                ModelSource::LocalImport => {
                    // 本地自建：无远端字段要求（就绪性按 target_dir 存在判定）
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
        // Parity（AI_Applications 对齐）：11 变体（3 原 + 8 新，新变体
        // 主走本地导入可无 mirror）；仅原 3 变体断言有 HF+modelScope 双源
        assert_eq!(manifest.models.len(), 11);
        let mirrored: Vec<_> = manifest
            .models
            .iter()
            .filter(|m| m.id == "large-v3" || m.id == "medium" || m.id == "small")
            .collect();
        assert_eq!(mirrored.len(), 3);
        for model in &mirrored {
            assert_eq!(model.mirrors.len(), 1, "model {} should have 1 mirror", model.id);
            assert_eq!(model.mirrors[0].source, ModelSource::Modelscope);
            assert!(model.mirrors[0].repo_id.starts_with("pengzhendong/faster-whisper-"));
            // available_sources = 主源 + mirror
            assert_eq!(
                model.available_sources(),
                vec![ModelSource::Huggingface, ModelSource::Modelscope]
            );
        }
        // 新 8 变体（本地导入优先）：无 mirror 可接受（本地导入后 available_sources 仍含 HF）
        let new_ids = ["tiny", "tiny-en", "base", "base-en", "small-en", "medium-en", "large-v1", "large-v2"];
        let new_models: Vec<_> = manifest
            .models
            .iter()
            .filter(|m| new_ids.contains(&m.id.as_str()))
            .collect();
        assert_eq!(new_models.len(), 8);
        for model in new_models {
            assert!(model.mirrors.is_empty(), "{} new variant should not require mirror", model.id);
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

    // ── requirements_by_backend（§2.6 冻结 schema，HETERO_DIST_PLAN M2）────

    const REQS_BY_BACKEND_TOML: &str = r#"
[module]
id = "realesr"
name = "RealESR"
version = "1.0.0"
description = "d"
category = "video"
genre = "sr"

[runtime]
type = "python"
python_version = ">=3.10,<3.13"
requirements = "requirements.txt"
requirements_by_backend = { cuda = "requirements-cuda.txt", rocm = "requirements-rocm.txt", openvino = "requirements-openvino.txt", directml = "requirements-directml.txt", vulkan = "requirements-vulkan.txt", cpu = "requirements.txt" }

[compute]
backends = ["cuda", "rocm", "openvino", "vulkan", "cpu"]

[[models]]
id = "x4"
name = "X4"
source = "url"
url = "https://example.invalid/x4.pth"
target_dir = "x4"

[interface]
type = "http"
"#;

    #[test]
    fn test_parse_requirements_by_backend_inline_table() {
        let manifest: ModuleManifest = toml::from_str(REQS_BY_BACKEND_TOML).unwrap();
        let map = &manifest.runtime.requirements_by_backend;
        assert_eq!(map.len(), 6, "六个后端词表键都应解析");
        assert_eq!(
            map.get(&ComputeBackend::Cuda).map(String::as_str),
            Some("requirements-cuda.txt")
        );
        assert_eq!(
            map.get(&ComputeBackend::Rocm).map(String::as_str),
            Some("requirements-rocm.txt")
        );
        assert_eq!(
            map.get(&ComputeBackend::OpenVINO).map(String::as_str),
            Some("requirements-openvino.txt")
        );
        assert_eq!(
            map.get(&ComputeBackend::DirectML).map(String::as_str),
            Some("requirements-directml.txt")
        );
        assert_eq!(
            map.get(&ComputeBackend::Vulkan).map(String::as_str),
            Some("requirements-vulkan.txt")
        );
        assert_eq!(
            map.get(&ComputeBackend::Cpu).map(String::as_str),
            Some("requirements.txt")
        );
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_requirements_by_backend_sub_table_form_equivalent() {
        // 子表写法与 inline table 等价（TOML 语义）
        let toml_str = REQS_BY_BACKEND_TOML.replace(
            "requirements_by_backend = { cuda = \"requirements-cuda.txt\", rocm = \"requirements-rocm.txt\", openvino = \"requirements-openvino.txt\", directml = \"requirements-directml.txt\", vulkan = \"requirements-vulkan.txt\", cpu = \"requirements.txt\" }",
            "[runtime.requirements_by_backend]\ncuda = \"requirements-cuda.txt\"\nrocm = \"requirements-rocm.txt\"",
        );
        let manifest: ModuleManifest = toml::from_str(&toml_str).unwrap();
        assert_eq!(manifest.runtime.requirements_by_backend.len(), 2);
        assert_eq!(
            manifest.runtime.resolve_requirements(Some(ComputeBackend::Cuda)),
            "requirements-cuda.txt"
        );
    }

    #[test]
    fn test_resolve_requirements_hit_fallback_and_default() {
        let manifest: ModuleManifest = toml::from_str(REQS_BY_BACKEND_TOML).unwrap();

        // 命中：cuda 有专属条目
        assert_eq!(
            manifest.runtime.resolve_requirements(Some(ComputeBackend::Cuda)),
            "requirements-cuda.txt"
        );
        // None（后端未知/尚未分配）→ 回退 runtime.requirements
        assert_eq!(
            manifest.runtime.resolve_requirements(None),
            "requirements.txt"
        );

        // 词表内但无条目的 backend → 回退 runtime.requirements（§2.6 回退语义）
        let no_vulkan = REQS_BY_BACKEND_TOML.replace(
            ", vulkan = \"requirements-vulkan.txt\"",
            "",
        );
        let no_vulkan: ModuleManifest = toml::from_str(&no_vulkan).unwrap();
        assert_eq!(
            no_vulkan.runtime.resolve_requirements(Some(ComputeBackend::Vulkan)),
            "requirements.txt"
        );

        // 完全未声明该字段（旧清单）→ 恒回退 requirements / 默认值
        let legacy: ModuleManifest = toml::from_str(VALID_TOML).unwrap();
        assert!(legacy.runtime.requirements_by_backend.is_empty());
        assert_eq!(
            legacy.runtime.resolve_requirements(Some(ComputeBackend::Cuda)),
            "requirements.txt",
            "旧清单无 per-backend 条目时必须回退 runtime.requirements"
        );
        assert_eq!(legacy.runtime.resolve_requirements(None), "requirements.txt");

        // runtime.requirements 也未声明 → 默认 "requirements.txt"
        let bare: ModuleManifest = toml::from_str(
            &VALID_TOML.replace("requirements = \"requirements.txt\"\n", ""),
        )
        .unwrap();
        assert_eq!(
            bare.runtime.resolve_requirements(Some(ComputeBackend::Rocm)),
            "requirements.txt"
        );
    }

    #[test]
    fn test_resolve_requirements_blank_entry_falls_back() {
        // 空白条目视为未声明（防御第三方清单手误）
        let toml_str = REQS_BY_BACKEND_TOML.replace(
            "cuda = \"requirements-cuda.txt\"",
            "cuda = \"  \"",
        );
        let manifest: ModuleManifest = toml::from_str(&toml_str).unwrap();
        assert_eq!(
            manifest.runtime.resolve_requirements(Some(ComputeBackend::Cuda)),
            "requirements.txt"
        );
    }

    #[test]
    fn test_requirements_by_backend_serialization_roundtrip() {
        // 整合包导出会序列化 manifest：字段须原样往返，且空表不落盘
        let manifest: ModuleManifest = toml::from_str(REQS_BY_BACKEND_TOML).unwrap();
        let serialized = toml::to_string_pretty(&manifest).unwrap();
        let reparsed: ModuleManifest = toml::from_str(&serialized).unwrap();
        assert_eq!(
            reparsed.runtime.requirements_by_backend,
            manifest.runtime.requirements_by_backend
        );
        assert!(serialized.contains("requirements_by_backend"));

        // 旧清单（无字段）序列化后不得出现空表键
        let legacy: ModuleManifest = toml::from_str(VALID_TOML).unwrap();
        let legacy_ser = toml::to_string_pretty(&legacy).unwrap();
        assert!(!legacy_ser.contains("requirements_by_backend"));
    }
}
