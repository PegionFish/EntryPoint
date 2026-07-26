use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracing::debug;

use crate::types::ComputeBackend;

const CONFIG_FILE_NAME: &str = "app.toml";

// ─── AssignStrategy ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
#[derive(Default)]
pub enum AssignStrategy {
    Manual,
    #[default]
    LeastMemory,
    RoundRobin,
    Single(Option<String>),
}

impl Serialize for AssignStrategy {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let name = match self {
            Self::Manual => "manual",
            Self::LeastMemory => "least_memory",
            Self::RoundRobin => "round_robin",
            Self::Single(_) => "single",
        };
        s.serialize_str(name)
    }
}

impl<'de> Deserialize<'de> for AssignStrategy {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        match s.as_str() {
            "manual" => Ok(Self::Manual),
            "least_memory" => Ok(Self::LeastMemory),
            "round_robin" => Ok(Self::RoundRobin),
            "single" => Ok(Self::Single(None)),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["manual", "least_memory", "round_robin", "single"],
            )),
        }
    }
}


// ─── Section configs ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_true")]
    pub check_updates: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            language: default_language(),
            theme: default_theme(),
            log_level: default_log_level(),
            check_updates: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeConfig {
    #[serde(default)]
    pub strategy: AssignStrategy,
    #[serde(default)]
    pub disabled_backends: Vec<ComputeBackend>,
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval_secs: u32,
    #[serde(default = "default_true")]
    pub allow_overcommit: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub single_device: Option<String>,
}

impl Default for ComputeConfig {
    fn default() -> Self {
        Self {
            strategy: AssignStrategy::LeastMemory,
            disabled_backends: Vec::new(),
            refresh_interval_secs: 2,
            allow_overcommit: true,
            single_device: None,
        }
    }
}

impl ComputeConfig {
    pub fn resolved_strategy(&self) -> AssignStrategy {
        match &self.strategy {
            AssignStrategy::Single(_) => AssignStrategy::Single(self.single_device.clone()),
            other => other.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortsConfig {
    #[serde(default = "default_range_start")]
    pub range_start: u16,
    #[serde(default = "default_range_end")]
    pub range_end: u16,
}

impl Default for PortsConfig {
    fn default() -> Self {
        Self {
            range_start: 18000,
            range_end: 19000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsConfig {
    #[serde(default = "default_cache_dir")]
    pub cache_dir: String,
    #[serde(default)]
    pub hf_endpoint: String,
    #[serde(default = "default_source")]
    pub default_source: String,
    #[serde(default = "default_max_concurrent_downloads")]
    pub max_concurrent_downloads: u32,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            cache_dir: default_cache_dir(),
            hf_endpoint: String::new(),
            default_source: default_source(),
            max_concurrent_downloads: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct PythonConfig {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub uv_path: String,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    #[serde(default = "default_max_parallel")]
    pub max_parallel: u32,
    #[serde(default = "default_timeout")]
    pub default_timeout_secs: u32,
    #[serde(default = "default_true")]
    pub keep_workspace: bool,
    #[serde(default = "default_workspace_dir")]
    pub workspace_dir: String,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            max_parallel: 4,
            default_timeout_secs: 600,
            keep_workspace: true,
            workspace_dir: default_workspace_dir(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    #[serde(default = "default_scale_factor")]
    pub scale_factor: f32,
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    #[serde(default = "default_dashboard_refresh")]
    pub dashboard_refresh_secs: u32,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            scale_factor: 1.0,
            font_size: 14.0,
            dashboard_refresh_secs: 2,
        }
    }
}

// ─── AppConfig ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct AppConfig {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub compute: ComputeConfig,
    #[serde(default)]
    pub ports: PortsConfig,
    #[serde(default)]
    pub models: ModelsConfig,
    #[serde(default)]
    pub python: PythonConfig,
    #[serde(default)]
    pub pipeline: PipelineConfig,
    #[serde(default)]
    pub ui: UiConfig,
}


impl AppConfig {
    fn config_path(config_dir: &Path) -> PathBuf {
        config_dir.join(CONFIG_FILE_NAME)
    }

    pub fn load(config_dir: &Path) -> Result<Self> {
        let path = Self::config_path(config_dir);
        if !path.exists() {
            debug!(path = %path.display(), "config file not found, using defaults");
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: Self =
            toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;
        debug!(path = %path.display(), "config loaded");
        Ok(config)
    }

    pub fn save(&self, config_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(config_dir)
            .with_context(|| format!("failed to create config dir {}", config_dir.display()))?;
        let path = Self::config_path(config_dir);
        let content = toml::to_string_pretty(self).context("failed to serialize config")?;
        std::fs::write(&path, content)
            .with_context(|| format!("failed to write {}", path.display()))?;
        debug!(path = %path.display(), "config saved");
        Ok(())
    }

    pub fn load_or_create(config_dir: &Path) -> Result<Self> {
        let path = Self::config_path(config_dir);
        if path.exists() {
            Self::load(config_dir)
        } else {
            let config = Self::default();
            config.save(config_dir)?;
            debug!(path = %path.display(), "default config created");
            Ok(config)
        }
    }

    pub fn resolve_model_cache_dir(&self, root: &Path) -> PathBuf {
        let p = Path::new(&self.models.cache_dir);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        }
    }

    pub fn resolve_workspace_dir(&self, root: &Path) -> PathBuf {
        let p = Path::new(&self.pipeline.workspace_dir);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        }
    }

    pub fn port_range(&self) -> (u16, u16) {
        (self.ports.range_start, self.ports.range_end)
    }
}

// ─── Default value functions ────────────────────────────────────────────────

fn default_language() -> String {
    "zh-CN".into()
}
fn default_theme() -> String {
    "dark".into()
}
fn default_log_level() -> String {
    "info".into()
}
fn default_true() -> bool {
    true
}
fn default_refresh_interval() -> u32 {
    2
}
fn default_range_start() -> u16 {
    18000
}
fn default_range_end() -> u16 {
    19000
}
fn default_cache_dir() -> String {
    "models".into()
}
fn default_source() -> String {
    "huggingface".into()
}
fn default_max_concurrent_downloads() -> u32 {
    2
}
fn default_max_parallel() -> u32 {
    4
}
fn default_timeout() -> u32 {
    600
}
fn default_workspace_dir() -> String {
    "workspace".into()
}
fn default_scale_factor() -> f32 {
    1.0
}
fn default_font_size() -> f32 {
    14.0
}
fn default_dashboard_refresh() -> u32 {
    2
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roundtrip() {
        let config = AppConfig::default();
        let toml_str = toml::to_string_pretty(&config).expect("serialize");
        let restored: AppConfig = toml::from_str(&toml_str).expect("deserialize");

        assert_eq!(restored.general.language, "zh-CN");
        assert_eq!(restored.general.theme, "dark");
        assert_eq!(restored.general.log_level, "info");
        assert!(restored.general.check_updates);

        assert_eq!(restored.compute.strategy, AssignStrategy::LeastMemory);
        assert!(restored.compute.disabled_backends.is_empty());
        assert_eq!(restored.compute.refresh_interval_secs, 2);
        assert!(restored.compute.allow_overcommit);
        assert!(restored.compute.single_device.is_none());

        assert_eq!(restored.ports.range_start, 18000);
        assert_eq!(restored.ports.range_end, 19000);

        assert_eq!(restored.models.cache_dir, "models");
        assert_eq!(restored.models.hf_endpoint, "");
        assert_eq!(restored.models.default_source, "huggingface");
        assert_eq!(restored.models.max_concurrent_downloads, 2);

        assert_eq!(restored.python.path, "");
        assert_eq!(restored.python.uv_path, "");

        assert_eq!(restored.pipeline.max_parallel, 4);
        assert_eq!(restored.pipeline.default_timeout_secs, 600);
        assert!(restored.pipeline.keep_workspace);
        assert_eq!(restored.pipeline.workspace_dir, "workspace");

        assert!((restored.ui.scale_factor - 1.0).abs() < f32::EPSILON);
        assert!((restored.ui.font_size - 14.0).abs() < f32::EPSILON);
        assert_eq!(restored.ui.dashboard_refresh_secs, 2);
    }

    #[test]
    fn strategy_serde_roundtrip() {
        for (strategy_str, expected) in [
            ("manual", AssignStrategy::Manual),
            ("least_memory", AssignStrategy::LeastMemory),
            ("round_robin", AssignStrategy::RoundRobin),
            ("single", AssignStrategy::Single(None)),
        ] {
            let toml_str = format!("[compute]\nstrategy = \"{strategy_str}\"\n");
            let config: AppConfig = toml::from_str(&toml_str).expect("parse strategy");
            assert_eq!(config.compute.strategy, expected);

            let serialized = toml::to_string_pretty(&config).expect("serialize");
            assert!(
                serialized.contains(&format!("strategy = \"{strategy_str}\"")),
                "serialized output should contain strategy = \"{strategy_str}\", got:\n{serialized}"
            );
        }
    }

    #[test]
    fn partial_toml_uses_defaults() {
        let toml_str = r#"
[general]
language = "en-US"

[ports]
range_start = 20000
"#;
        let config: AppConfig = toml::from_str(toml_str).expect("parse partial");
        assert_eq!(config.general.language, "en-US");
        assert_eq!(config.general.theme, "dark");
        assert_eq!(config.ports.range_start, 20000);
        assert_eq!(config.ports.range_end, 19000);
        assert_eq!(config.compute.strategy, AssignStrategy::LeastMemory);
    }

    #[test]
    fn disabled_backends_serde() {
        let toml_str = r#"
[compute]
disabled_backends = ["directml", "rocm"]
"#;
        let config: AppConfig = toml::from_str(toml_str).expect("parse backends");
        assert_eq!(
            config.compute.disabled_backends,
            vec![ComputeBackend::DirectML, ComputeBackend::Rocm]
        );
    }

    #[test]
    fn resolve_paths() {
        let config = AppConfig::default();
        let root = Path::new("G:/EntryPoint");

        assert_eq!(
            config.resolve_model_cache_dir(root),
            PathBuf::from("G:/EntryPoint/models")
        );
        assert_eq!(
            config.resolve_workspace_dir(root),
            PathBuf::from("G:/EntryPoint/workspace")
        );

        let mut config2 = AppConfig::default();
        config2.models.cache_dir = "D:/AI_Models".into();
        assert_eq!(
            config2.resolve_model_cache_dir(root),
            PathBuf::from("D:/AI_Models")
        );
    }

    #[test]
    fn port_range_helper() {
        let config = AppConfig::default();
        assert_eq!(config.port_range(), (18000, 19000));
    }

    #[test]
    fn load_or_create_creates_file() {
        let dir = std::env::temp_dir().join(format!("ep_config_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let config = AppConfig::load_or_create(&dir).expect("load_or_create");
        let path = dir.join(CONFIG_FILE_NAME);
        assert!(path.exists(), "config file should be created");

        let loaded = AppConfig::load(&dir).expect("reload");
        assert_eq!(loaded.general.language, config.general.language);
        assert_eq!(loaded.ports.range_start, config.ports.range_start);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_returns_default() {
        let dir = std::env::temp_dir().join(format!("ep_config_missing_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let config = AppConfig::load(&dir).expect("load missing");
        assert_eq!(config.general.language, "zh-CN");
        assert_eq!(config.ports.range_start, 18000);
    }

    #[test]
    fn save_and_reload() {
        let dir = std::env::temp_dir().join(format!("ep_config_save_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let mut config = AppConfig::default();
        config.general.language = "en-US".into();
        config.ports.range_start = 20000;
        config.compute.strategy = AssignStrategy::Single(None);
        config.compute.single_device = Some("cuda:0".into());

        config.save(&dir).expect("save");
        let loaded = AppConfig::load(&dir).expect("reload");

        assert_eq!(loaded.general.language, "en-US");
        assert_eq!(loaded.ports.range_start, 20000);
        assert_eq!(loaded.compute.strategy, AssignStrategy::Single(None));
        assert_eq!(loaded.compute.single_device.as_deref(), Some("cuda:0"));
        assert_eq!(
            loaded.compute.resolved_strategy(),
            AssignStrategy::Single(Some("cuda:0".into()))
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
