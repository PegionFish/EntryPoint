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
    #[serde(default)]
    pub default: bool,
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
                    .map_or(true, |b| b.is_empty())
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
}
