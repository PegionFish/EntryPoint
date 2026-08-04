use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracing::{debug, info};

use crate::types::ComputeBackend;

const CONFIG_FILE_NAME: &str = "app.toml";

// ─── Root 目录解析 ──────────────────────────────────────────────────────────

/// 解析项目根目录（所有相对路径的基准）。
///
/// 优先级：
/// 1. `EP_ROOT` 环境变量
/// 2. 可执行文件所在目录的父目录（检查是否含 `config/` + `modules/`）
/// 3. 当前工作目录（兜底）
pub fn resolve_root() -> PathBuf {
    // 1. 环境变量
    if let Ok(ep_root) = std::env::var("EP_ROOT") {
        let p = PathBuf::from(&ep_root);
        if p.is_dir() {
            info!(root = %p.display(), "resolved root from EP_ROOT");
            return p;
        }
    }

    // 2. 可执行文件位置推断
    if let Ok(exe) = std::env::current_exe() {
        // 典型布局: <root>/target/release/ep-daemon → root = exe/../../..
        // 安装布局: /usr/bin/ep-daemon → 不适用，跳过
        if let Some(bin_dir) = exe.parent() {
            if let Some(build_dir) = bin_dir.parent() {
                if let Some(candidate) = build_dir.parent() {
                    if candidate.join("config").is_dir() && candidate.join("modules").is_dir() {
                        info!(root = %candidate.display(), "resolved root from executable path");
                        return candidate.to_path_buf();
                    }
                }
            }
        }

        // macOS .app 布局: EntryPoint.app/Contents/MacOS/entrypoint → root = Contents/Resources
        if let Some(bin_dir) = exe.parent() {
            if let Some(contents_dir) = bin_dir.parent() {
                let resources = contents_dir.join("Resources");
                if resources.join("config").is_dir() && resources.join("modules").is_dir() {
                    info!(root = %resources.display(), "resolved root from macOS app bundle");
                    return resources;
                }
            }
        }
    }

    // 3. 当前工作目录
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    info!(root = %cwd.display(), "resolved root from current directory");
    cwd
}

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
    /// 共享 CUDA 库目录（Linux 注入 LD_LIBRARY_PATH / Windows 前置 PATH，§3.1）
    #[serde(default = "default_cuda_libs_dir")]
    pub cuda_libs_dir: String,
}

impl Default for ComputeConfig {
    fn default() -> Self {
        Self {
            strategy: AssignStrategy::LeastMemory,
            disabled_backends: Vec::new(),
            refresh_interval_secs: 2,
            allow_overcommit: true,
            single_device: None,
            cuda_libs_dir: default_cuda_libs_dir(),
        }
    }
}

fn default_cuda_libs_dir() -> String {
    "runtime/cuda-libs".to_string()
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
    /// 本地模型缓存搜索路径（按优先级排序），用于发现用户已有的模型文件
    #[serde(default)]
    pub cache_paths: Vec<String>,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            cache_dir: default_cache_dir(),
            hf_endpoint: String::new(),
            default_source: default_source(),
            max_concurrent_downloads: 2,
            cache_paths: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PythonConfig {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub uv_path: String,
    /// uv 缓存目录（应用根下，与 venv 同盘 → 硬链接去重，§3.1；A1 接线）
    #[serde(default = "default_uv_cache_dir")]
    pub uv_cache_dir: String,
    /// 全局 constraints 文件（锁 torch 全家桶等版本，§3.1；空字符串 = 停用；A1 接线）
    #[serde(default = "default_constraints")]
    pub constraints: String,
}

impl Default for PythonConfig {
    fn default() -> Self {
        Self {
            path: String::new(),
            uv_path: String::new(),
            uv_cache_dir: default_uv_cache_dir(),
            constraints: default_constraints(),
        }
    }
}

fn default_uv_cache_dir() -> String {
    "runtime/.uv-cache".to_string()
}

fn default_constraints() -> String {
    "config/constraints.txt".to_string()
}

/// 网络代理配置 — 统一控制模型下载、依赖安装、模块子进程的出口代理。
///
/// 取代此前"子进程隐式继承 daemon 环境变量"的不可控方式：
/// 所有需要联网的子进程显式注入这里配置的环境变量。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// HTTP 代理（如 "http://127.0.0.1:20171"），空字符串 = 不设置
    #[serde(default)]
    pub http_proxy: String,
    /// HTTPS 代理，空字符串 = 不设置
    #[serde(default)]
    pub https_proxy: String,
    /// 不走代理的地址列表
    #[serde(default = "default_no_proxy")]
    pub no_proxy: String,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            http_proxy: String::new(),
            https_proxy: String::new(),
            no_proxy: default_no_proxy(),
        }
    }
}

impl NetworkConfig {
    /// 生成注入子进程的环境变量列表。
    ///
    /// 非空的 proxy 字段同时产出大写 + 小写键
    /// （HTTP_PROXY/http_proxy、HTTPS_PROXY/https_proxy、NO_PROXY/no_proxy）；
    /// 字段为空则不产出对应键（不覆盖进程继承的环境变量）。
    pub fn env_vars(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        if !self.http_proxy.is_empty() {
            out.push(("HTTP_PROXY".to_string(), self.http_proxy.clone()));
            out.push(("http_proxy".to_string(), self.http_proxy.clone()));
        }
        if !self.https_proxy.is_empty() {
            out.push(("HTTPS_PROXY".to_string(), self.https_proxy.clone()));
            out.push(("https_proxy".to_string(), self.https_proxy.clone()));
        }
        if !self.no_proxy.is_empty() {
            out.push(("NO_PROXY".to_string(), self.no_proxy.clone()));
            out.push(("no_proxy".to_string(), self.no_proxy.clone()));
        }
        out
    }

    /// 是否配置了任何出口代理
    pub fn has_proxy(&self) -> bool {
        !self.http_proxy.is_empty() || !self.https_proxy.is_empty()
    }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub allow_public: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            allow_public: false,
        }
    }
}

// ─── AppConfig ──────────────────────────────────────────────────────────────

/// 整合包配置（§8.3）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacksConfig {
    /// 导入暂存目录（解包 + 校验的隔离区，§4.4；B1/B2 消费）
    #[serde(default = "default_pack_staging_dir")]
    pub staging_dir: String,
}

impl Default for PacksConfig {
    fn default() -> Self {
        Self {
            staging_dir: default_pack_staging_dir(),
        }
    }
}

fn default_pack_staging_dir() -> String {
    ".pack-staging".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct AppConfig {
    #[serde(default)]
    pub server: ServerConfig,
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
    /// 网络代理配置（下载 / 依赖安装 / 模块进程出口）
    #[serde(default)]
    pub network: NetworkConfig,
    /// 整合包配置（§8.3）
    #[serde(default)]
    pub packs: PacksConfig,
    /// 每模块激活模型变体（单槽位语义 §5.2）：module_id → model_id；A6 消费
    #[serde(default)]
    pub active_models: std::collections::HashMap<String, String>,
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

    /// 将所有相对路径字段解析为绝对路径（基于 root）。
    ///
    /// 调用后 `models.cache_dir`、`pipeline.workspace_dir`、`models.cache_paths`
    /// 中的相对路径均变为绝对路径。已经是绝对路径的不变。
    pub fn resolve_paths(&mut self, root: &Path) {
        // models.cache_dir
        let p = Path::new(&self.models.cache_dir);
        if p.is_relative() {
            self.models.cache_dir = root.join(p).to_string_lossy().to_string();
        }

        // pipeline.workspace_dir
        let p = Path::new(&self.pipeline.workspace_dir);
        if p.is_relative() {
            self.pipeline.workspace_dir = root.join(p).to_string_lossy().to_string();
        }

        // models.cache_paths（逐项解析）
        for cp in &mut self.models.cache_paths {
            let p = Path::new(cp.as_str());
            if p.is_relative() {
                *cp = root.join(p).to_string_lossy().to_string();
            }
        }

        debug!(
            cache_dir = %self.models.cache_dir,
            workspace_dir = %self.pipeline.workspace_dir,
            "config paths resolved to absolute"
        );
    }

    pub fn port_range(&self) -> (u16, u16) {
        (self.ports.range_start, self.ports.range_end)
    }

    /// 深度合并部分配置补丁（P1-9：PUT /api/config 合并语义的 config 层支持）。
    ///
    /// 语义（JSON 深度合并）：
    /// - **缺省字段保留原值**：patch 中未出现的字段一律不动
    /// - **显式字段覆盖**：patch 中出现的标量字段覆盖原值（含空字符串——如
    ///   `python.constraints = ""` 即显式停用 constraints）
    /// - 嵌套表（`general`/`compute`/`python` 等）递归深合并，未出现的子字段保留
    /// - 数组（`disabled_backends`/`cache_paths` 等）整体替换（不做元素级合并）
    /// - 映射字段（`active_models`）按键合并：已有键保留，显式键覆盖/新增
    /// - 未知键忽略（与 load 的 serde 行为一致）
    /// - `Option` 字段的 `null` 清空为 `None`；非 Option 字段的 `null` 按类型错误拒绝
    ///
    /// all-or-nothing：patch 非法或任一字段类型不匹配时返回错误，`self` 保持不变。
    /// 成功返回时 `self` 为合并后的完整配置。API 层接线（PUT /api/config）由 Wave 3 C7 完成。
    pub fn merge_partial(&mut self, patch: &serde_json::Value) -> Result<()> {
        let Some(patch_obj) = patch.as_object() else {
            bail!("config patch must be a JSON object");
        };
        if patch_obj.is_empty() {
            return Ok(());
        }

        let mut merged =
            serde_json::to_value(&*self).context("failed to serialize current config")?;
        json_deep_merge(&mut merged, patch);
        let updated: Self = serde_json::from_value(merged)
            .context("failed to apply config patch (field type mismatch?)")?;
        *self = updated;
        Ok(())
    }
}

/// 递归深度合并：两侧均为对象时按键合并（patch 优先），否则整体替换（patch 胜出）。
fn json_deep_merge(base: &mut serde_json::Value, patch: &serde_json::Value) {
    match (base.as_object_mut(), patch.as_object()) {
        (Some(base_map), Some(patch_map)) => {
            for (key, value) in patch_map {
                match base_map.get_mut(key) {
                    Some(existing) => json_deep_merge(existing, value),
                    None => {
                        base_map.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        _ => *base = patch.clone(),
    }
}

// ─── Default value functions ────────────────────────────────────────────────

fn default_host() -> String {
    "0.0.0.0".into()
}
fn default_port() -> u16 {
    9800
}
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
fn default_no_proxy() -> String {
    "localhost,127.0.0.1".into()
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

        assert_eq!(restored.network.http_proxy, "");
        assert_eq!(restored.network.https_proxy, "");
        assert_eq!(restored.network.no_proxy, "localhost,127.0.0.1");
    }

    // ── NetworkConfig ──────────────────────────────────────────────────

    #[test]
    fn network_env_vars_both_proxies() {
        let net = NetworkConfig {
            http_proxy: "http://127.0.0.1:20171".into(),
            https_proxy: "http://127.0.0.1:20171".into(),
            no_proxy: "localhost,127.0.0.1".into(),
        };
        let vars = net.env_vars();
        // 3 个非空字段 × 大写/小写 = 6 对
        assert_eq!(vars.len(), 6);
        assert!(vars.contains(&("HTTP_PROXY".to_string(), "http://127.0.0.1:20171".to_string())));
        assert!(vars.contains(&("http_proxy".to_string(), "http://127.0.0.1:20171".to_string())));
        assert!(vars.contains(&("HTTPS_PROXY".to_string(), "http://127.0.0.1:20171".to_string())));
        assert!(vars.contains(&("https_proxy".to_string(), "http://127.0.0.1:20171".to_string())));
        assert!(vars.contains(&("NO_PROXY".to_string(), "localhost,127.0.0.1".to_string())));
        assert!(vars.contains(&("no_proxy".to_string(), "localhost,127.0.0.1".to_string())));
        assert!(net.has_proxy());
    }

    #[test]
    fn network_env_vars_empty_fields_emit_nothing() {
        // 默认配置：proxy 字段为空 → 只产出 no_proxy 两项
        let net = NetworkConfig::default();
        let vars = net.env_vars();
        assert_eq!(vars.len(), 2);
        assert!(vars.contains(&("NO_PROXY".to_string(), "localhost,127.0.0.1".to_string())));
        assert!(vars.contains(&("no_proxy".to_string(), "localhost,127.0.0.1".to_string())));
        assert!(!net.has_proxy());

        // 全空 → 不产出任何键（不覆盖继承值）
        let net2 = NetworkConfig {
            http_proxy: String::new(),
            https_proxy: String::new(),
            no_proxy: String::new(),
        };
        assert!(net2.env_vars().is_empty());
    }

    #[test]
    fn network_config_toml_parse() {
        let toml_str = r#"
[network]
http_proxy = "http://127.0.0.1:20171"
https_proxy = "http://127.0.0.1:20171"
no_proxy = "localhost,127.0.0.1,192.168.*"
"#;
        let config: AppConfig = toml::from_str(toml_str).expect("parse network section");
        assert_eq!(config.network.http_proxy, "http://127.0.0.1:20171");
        assert_eq!(config.network.no_proxy, "localhost,127.0.0.1,192.168.*");

        // 无 [network] 节 → 默认值
        let config2: AppConfig = toml::from_str("").expect("parse empty");
        assert_eq!(config2.network.http_proxy, "");
        assert_eq!(config2.network.no_proxy, "localhost,127.0.0.1");
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

        if cfg!(windows) {
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
        } else {
            let root = Path::new("/opt/entrypoint");
            assert_eq!(
                config.resolve_model_cache_dir(root),
                PathBuf::from("/opt/entrypoint/models")
            );
            assert_eq!(
                config.resolve_workspace_dir(root),
                PathBuf::from("/opt/entrypoint/workspace")
            );

            let mut config2 = AppConfig::default();
            config2.models.cache_dir = "/opt/models".into();
            assert_eq!(
                config2.resolve_model_cache_dir(root),
                PathBuf::from("/opt/models")
            );
        }
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

    // ── merge_partial 深度合并（P1-9）────────────────────────────────────

    #[test]
    fn merge_partial_explicit_overrides_missing_preserved() {
        let mut config = AppConfig::default();
        let patch = serde_json::json!({
            "general": { "language": "en-US" }
        });
        config.merge_partial(&patch).expect("merge");

        // 显式字段覆盖
        assert_eq!(config.general.language, "en-US");
        // 缺省字段保留原值（同表其他字段 + 其他段）
        assert_eq!(config.general.theme, "dark");
        assert!(config.general.check_updates);
        assert_eq!(config.ports.range_start, 18000);
        assert_eq!(config.python.uv_cache_dir, "runtime/.uv-cache");
        assert_eq!(config.compute.strategy, AssignStrategy::LeastMemory);
    }

    #[test]
    fn merge_partial_multiple_sections() {
        let mut config = AppConfig::default();
        let patch = serde_json::json!({
            "server": { "port": 9900 },
            "compute": { "allow_overcommit": false, "refresh_interval_secs": 5 },
            "python": { "constraints": "" }
        });
        config.merge_partial(&patch).expect("merge");

        assert_eq!(config.server.port, 9900);
        assert_eq!(config.server.host, "0.0.0.0", "未显式给出的字段保留");
        assert!(!config.compute.allow_overcommit);
        assert_eq!(config.compute.refresh_interval_secs, 5);
        // 显式空字符串 = 停用 constraints（覆盖默认值，不触发默认回填）
        assert_eq!(config.python.constraints, "");
    }

    #[test]
    fn merge_partial_arrays_replaced_wholesale() {
        let mut config = AppConfig::default();
        config.compute.disabled_backends = vec![ComputeBackend::Rocm];
        config.models.cache_paths = vec!["/old/path".into()];

        let patch = serde_json::json!({
            "compute": { "disabled_backends": ["directml"] },
            "models": { "cache_paths": [] }
        });
        config.merge_partial(&patch).expect("merge");

        assert_eq!(
            config.compute.disabled_backends,
            vec![ComputeBackend::DirectML],
            "数组整体替换而非合并"
        );
        assert!(config.models.cache_paths.is_empty());
    }

    #[test]
    fn merge_partial_active_models_per_key_merge() {
        let mut config = AppConfig::default();
        config.active_models.insert("mod-a".into(), "model-1".into());

        let patch = serde_json::json!({
            "active_models": { "mod-b": "model-2" }
        });
        config.merge_partial(&patch).expect("merge");
        assert_eq!(
            config.active_models.get("mod-a").map(String::as_str),
            Some("model-1"),
            "映射按键合并：已有键保留"
        );
        assert_eq!(config.active_models.get("mod-b").map(String::as_str), Some("model-2"));

        // 显式覆盖已有键
        let patch2 = serde_json::json!({ "active_models": { "mod-a": "model-9" } });
        config.merge_partial(&patch2).expect("merge");
        assert_eq!(config.active_models.get("mod-a").map(String::as_str), Some("model-9"));
    }

    #[test]
    fn merge_partial_empty_object_is_noop() {
        let mut config = AppConfig::default();
        config.general.language = "fr-FR".into();
        config.merge_partial(&serde_json::json!({})).expect("empty patch is ok");
        assert_eq!(config.general.language, "fr-FR");
    }

    #[test]
    fn merge_partial_non_object_rejected() {
        let mut config = AppConfig::default();
        assert!(config.merge_partial(&serde_json::json!(["general"])).is_err());
        assert!(config.merge_partial(&serde_json::json!("general")).is_err());
        assert!(config.merge_partial(&serde_json::Value::Null).is_err());
        assert!(config.merge_partial(&serde_json::json!(42)).is_err());
    }

    #[test]
    fn merge_partial_type_mismatch_rejected_and_unchanged() {
        let mut config = AppConfig::default();
        config.general.language = "fr-FR".into();

        let patch = serde_json::json!({
            "ports": { "range_start": "not-a-number" },
            "general": { "theme": "light" }
        });
        assert!(config.merge_partial(&patch).is_err());
        // all-or-nothing：失败时原配置保持不变（theme 也未被部分写入）
        assert_eq!(config.ports.range_start, 18000);
        assert_eq!(config.general.language, "fr-FR");
        assert_eq!(config.general.theme, "dark");
    }

    #[test]
    fn merge_partial_unknown_keys_ignored() {
        let mut config = AppConfig::default();
        let patch = serde_json::json!({
            "general": { "language": "en-US", "no_such_field": 42 },
            "no_such_section": { "x": 1 }
        });
        config
            .merge_partial(&patch)
            .expect("unknown keys ignored (与 load 的 serde 行为一致)");
        assert_eq!(config.general.language, "en-US");
    }

    #[test]
    fn merge_partial_null_semantics() {
        let mut config = AppConfig::default();
        config.compute.single_device = Some("cuda:0".into());

        // Option 字段：null → 清空为 None
        let patch = serde_json::json!({ "compute": { "single_device": null } });
        config.merge_partial(&patch).expect("null into Option clears it");
        assert!(config.compute.single_device.is_none());

        // 非 Option 字段：null → 类型错误
        let patch2 = serde_json::json!({ "general": { "language": null } });
        assert!(config.merge_partial(&patch2).is_err());
    }

    #[test]
    fn merge_partial_invalid_enum_rejected() {
        let mut config = AppConfig::default();
        let patch = serde_json::json!({ "compute": { "strategy": "bogus_strategy" } });
        assert!(config.merge_partial(&patch).is_err());

        let patch2 = serde_json::json!({ "compute": { "disabled_backends": ["warp9"] } });
        assert!(config.merge_partial(&patch2).is_err());
    }

    #[test]
    fn merge_partial_wrong_shape_rejected() {
        let mut config = AppConfig::default();
        // 段应为表却给标量 → 反序列化失败
        let patch = serde_json::json!({ "general": "dark" });
        assert!(config.merge_partial(&patch).is_err());
    }

    #[test]
    fn merge_partial_roundtrip_with_save_load() {
        let dir = std::env::temp_dir().join(format!("ep_config_merge_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let mut config = AppConfig::default();
        config
            .merge_partial(&serde_json::json!({
                "general": { "theme": "light" },
                "python": { "uv_cache_dir": "cache/uv" },
                "active_models": { "deep-filter": "v2" }
            }))
            .expect("merge");
        config.save(&dir).expect("save");

        let loaded = AppConfig::load(&dir).expect("load");
        assert_eq!(loaded.general.theme, "light");
        assert_eq!(loaded.python.uv_cache_dir, "cache/uv");
        assert_eq!(loaded.active_models.get("deep-filter").map(String::as_str), Some("v2"));
        // 未显式给出的字段保持默认并正确往返
        assert_eq!(loaded.general.language, "zh-CN");
        assert_eq!(loaded.python.constraints, "config/constraints.txt");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
