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
    /// 任务级**空闲看门狗**超时（秒）：任务持续此时长无任何节点进度/心跳
    /// 才判死（0 = 停用看门狗）。注意不再是任务总时长硬上限——只要执行器
    /// 持续产生心跳（节点开始/完成/失败，及长调用期间的周期心跳），任务可
    /// 运行任意时长（缺陷 #3 拆分：原值同时充当节点硬超时，长媒体任务被误杀）。
    #[serde(default = "default_timeout")]
    pub default_timeout_secs: u32,
    /// 节点级**硬超时**全局缺省（秒）：节点未声明 `timeout_secs` 且管线未声明
    /// `[pipeline] node_timeout_secs` 时，作为单节点 wall-clock 硬超时。
    /// `0`（缺省）= 跟随 [`Self::default_timeout_secs`]（旧配置行为不变）。
    #[serde(default)]
    pub default_node_timeout_secs: u32,
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
            default_node_timeout_secs: 0,
            keep_workspace: true,
            workspace_dir: default_workspace_dir(),
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

// ─── ResolvedPaths（#48：解析与持久化分离）────────────────────────────────

/// 运行期解析路径视图（#48）。
///
/// `AppConfig` 的序列化字段（`models.cache_dir` / `pipeline.workspace_dir` /
/// `models.cache_paths` 等）**始终保留用户原始字符串**（相对保持相对、绝对保持绝对），
/// 保证 `save()` 落盘形态与出厂/用户编辑一致，部署目录迁移后配置依然有效。
///
/// 运行期需要绝对路径时：
/// - 启动期调用 [`AppConfig::resolve_paths`] 基于 root 一次性计算并缓存到本结构
///   （经 [`AppConfig::resolved_paths`] 读取）；
/// - 或随时使用 `resolve_*(root)` 只读视图（无状态，不依赖缓存）。
///
/// 本结构 `#[serde(skip)]`：绝不落盘、不参与 merge/roundtrip。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedPaths {
    /// `models.cache_dir` 解析后的绝对路径
    pub model_cache_dir: PathBuf,
    /// `pipeline.workspace_dir` 解析后的绝对路径
    pub workspace_dir: PathBuf,
    /// `models.cache_paths` 逐项解析后的绝对路径（保持优先级顺序）
    pub cache_paths: Vec<PathBuf>,
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
    /// 网络代理配置（下载 / 依赖安装 / 模块进程出口）
    #[serde(default)]
    pub network: NetworkConfig,
    /// 整合包配置（§8.3）
    #[serde(default)]
    pub packs: PacksConfig,
    /// 每模块激活模型变体（单槽位语义 §5.2）：module_id → model_id；A6 消费
    #[serde(default)]
    pub active_models: std::collections::HashMap<String, String>,
    /// 运行期解析路径缓存（#48：解析与持久化分离）。
    ///
    /// `#[serde(skip)]`：不落盘、不参与 merge/roundtrip；由
    /// [`Self::resolve_paths`] 填充，经 [`Self::resolved_paths`] 读取。
    /// 注意：[`Self::merge_partial`] 经 serde 重建配置后本字段会被重置，
    /// 调用方合并后须重新调用 [`Self::resolve_paths`]（daemon put_config 已接线）。
    #[serde(skip)]
    resolved: ResolvedPaths,
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

    /// 持久化配置到 `config_dir/app.toml`（P1：原子写盘）。
    ///
    /// #48：序列化字段保留用户原始形态（相对路径保持相对），
    /// 运行期解析缓存（[`Self::resolved_paths`]）`#[serde(skip)]` 不落盘。
    ///
    /// 原子性：先写同目录临时文件 `app.toml.tmp` + fsync，再 rename 覆盖目标
    /// ——写一半崩溃不再损坏正式配置（`load_or_create` 只处理缺失、不处理损坏，
    /// 半写文件会让 daemon 启动失败）。Windows 的 `std::fs::rename` 经
    /// MoveFileExW+REPLACE_EXISTING 可直接覆盖已存在目标；个别场景（杀软/
    /// 占用）失败时回退 删旧 + rename。
    pub fn save(&self, config_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(config_dir)
            .with_context(|| format!("failed to create config dir {}", config_dir.display()))?;
        let path = Self::config_path(config_dir);
        let content = toml::to_string_pretty(self).context("failed to serialize config")?;

        let tmp_path = config_dir.join(format!("{CONFIG_FILE_NAME}.tmp"));
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&tmp_path)
                .with_context(|| format!("failed to create temp config {}", tmp_path.display()))?;
            file.write_all(content.as_bytes()).with_context(|| {
                format!("failed to write temp config {}", tmp_path.display())
            })?;
            file.sync_all().with_context(|| {
                format!("failed to fsync temp config {}", tmp_path.display())
            })?;
        }

        if std::fs::rename(&tmp_path, &path).is_err() {
            // Windows 语义兜底：rename 覆盖目标失败（杀软/占用）→ 删旧再 rename
            if path.exists() {
                std::fs::remove_file(&path).with_context(|| {
                    format!("failed to remove stale config {}", path.display())
                })?;
            }
            std::fs::rename(&tmp_path, &path).with_context(|| {
                format!("failed to replace {} with {}", path.display(), tmp_path.display())
            })?;
        }

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

    /// 路径解析基元（#48）：相对路径基于 root 解析，绝对路径原样返回。
    /// Path/PathBuf API 双平台通用，无平台硬编码。
    fn absolutize(root: &Path, raw: &str) -> PathBuf {
        let p = Path::new(raw);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        }
    }

    /// 模型缓存目录的 `resolve(root)` 只读视图（#48）：
    /// 相对 `models.cache_dir` 基于 root 解析，绝对路径原样返回。
    /// 不修改配置本身，随时可调。
    pub fn resolve_model_cache_dir(&self, root: &Path) -> PathBuf {
        Self::absolutize(root, &self.models.cache_dir)
    }

    /// 任务工作区的 `resolve(root)` 只读视图（#48）：
    /// 相对 `pipeline.workspace_dir` 基于 root 解析，绝对路径原样返回。
    /// 不修改配置本身，随时可调。
    pub fn resolve_workspace_dir(&self, root: &Path) -> PathBuf {
        Self::absolutize(root, &self.pipeline.workspace_dir)
    }

    /// 基于 root 解析相对路径字段，结果缓存到运行期视图
    /// [`Self::resolved_paths`]（#48：解析与持久化分离）。
    ///
    /// **不修改任何序列化字段**：`models.cache_dir`、`pipeline.workspace_dir`、
    /// `models.cache_paths` 保留用户原始字符串（相对保持相对、绝对保持绝对），
    /// 因此 [`Self::save`] 落盘形态不变，部署目录迁移后配置依然有效。
    /// 已经是绝对路径的项解析后原样进入视图。
    ///
    /// 调用时机：daemon 启动期各入口调用一次（常规服务模式与 --run-module
    /// 独立模式，见 ep-daemon main.rs）；
    /// [`Self::merge_partial`] 之后需重新调用（serde 重建会重置运行期缓存）。
    pub fn resolve_paths(&mut self, root: &Path) {
        self.resolved = ResolvedPaths {
            model_cache_dir: Self::absolutize(root, &self.models.cache_dir),
            workspace_dir: Self::absolutize(root, &self.pipeline.workspace_dir),
            cache_paths: self
                .models
                .cache_paths
                .iter()
                .map(|p| Self::absolutize(root, p))
                .collect(),
        };

        debug!(
            cache_dir = %self.resolved.model_cache_dir.display(),
            workspace_dir = %self.resolved.workspace_dir.display(),
            cache_paths_count = self.resolved.cache_paths.len(),
            "config paths resolved to runtime view (serialized fields unchanged)"
        );
    }

    /// 运行期解析路径视图（#48）：由 [`Self::resolve_paths`] 填充的绝对路径缓存。
    ///
    /// 未在 `resolve_paths` 之前使用（字段为空）；daemon 启动期必然先解析。
    /// 无状态场景请改用 `resolve_*(root)` 只读视图。
    pub fn resolved_paths(&self) -> &ResolvedPaths {
        &self.resolved
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
    ///
    /// #48 注意：合并经 serde 重建配置，运行期缓存 `resolved`（`#[serde(skip)]`）
    /// 会被重置；调用方须在合并后重新调用 [`Self::resolve_paths`]。
    /// 序列化字段本身不受合并影响保持原始形态（相对仍为相对）。
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

        assert_eq!(restored.network.http_proxy, "");
        assert_eq!(restored.network.https_proxy, "");
        assert_eq!(restored.network.no_proxy, "localhost,127.0.0.1");
    }

    /// 桌面端退役（2026-08-13）后旧配置可能仍含 `[ui]` 节：
    /// 必须能被新 daemon 正常加载（serde 默认忽略未知字段，无 deny_unknown_fields），
    /// 解析不得报错、已知字段不受影响。
    #[test]
    fn legacy_ui_section_parses_ignored() {
        let toml_str = r#"
[server]
host = "127.0.0.1"
port = 9800

[ui]
scale_factor = 1.25
font_size = 16.0
dashboard_refresh_secs = 5
"#;
        let config: AppConfig = toml::from_str(toml_str).expect("legacy [ui] section must parse");
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 9800);
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

    // ── #48 解析与持久化分离（回归）────────────────────────────────────

    /// resolve_paths 只填充运行期视图，绝不改写序列化字段
    #[test]
    fn resolve_paths_populates_runtime_view_without_mutation() {
        // 未解析前视图为空默认值
        let pristine = AppConfig::default();
        assert_eq!(pristine.resolved_paths(), &ResolvedPaths::default());

        let mut config = AppConfig::default();
        config.models.cache_paths = vec!["shared/models".into(), "extra/cache".into()];

        let root = if cfg!(windows) {
            PathBuf::from("C:/EntryPoint")
        } else {
            PathBuf::from("/opt/entrypoint")
        };
        config.resolve_paths(&root);

        // 序列化字段保持原始相对形态
        assert_eq!(config.models.cache_dir, "models");
        assert_eq!(config.pipeline.workspace_dir, "workspace");
        assert_eq!(
            config.models.cache_paths,
            vec!["shared/models".to_string(), "extra/cache".to_string()]
        );

        // 运行期视图为绝对路径（顺序保持）
        let resolved = config.resolved_paths();
        assert_eq!(resolved.model_cache_dir, root.join("models"));
        assert_eq!(resolved.workspace_dir, root.join("workspace"));
        assert_eq!(
            resolved.cache_paths,
            vec![root.join("shared/models"), root.join("extra/cache")]
        );

        // resolve(root) 只读视图与运行期缓存一致
        assert_eq!(
            config.resolve_model_cache_dir(&root),
            resolved.model_cache_dir
        );
        assert_eq!(config.resolve_workspace_dir(&root), resolved.workspace_dir);
    }

    /// 相对配置：加载 → resolve → save → 文件仍为相对形态（#48 核心回归）
    #[test]
    fn save_preserves_relative_paths_after_resolve() {
        let dir = std::env::temp_dir().join(format!("ep_config_rel_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let toml_src = r#"
[models]
cache_dir = "models"
cache_paths = ["shared/models", "extra/cache"]

[pipeline]
workspace_dir = "workspace"
"#;
        std::fs::write(dir.join(CONFIG_FILE_NAME), toml_src).unwrap();

        let mut config = AppConfig::load(&dir).expect("load");
        let root = dir.join("fake-root");
        config.resolve_paths(&root);

        // 消费方仍拿到绝对路径
        assert!(config.resolved_paths().model_cache_dir.is_absolute());
        assert!(config.resolved_paths().workspace_dir.is_absolute());

        // 落盘 → 文件仍为相对形态
        config.save(&dir).expect("save");
        let raw = std::fs::read_to_string(dir.join(CONFIG_FILE_NAME)).unwrap();
        assert!(
            raw.contains("cache_dir = \"models\""),
            "落盘应保持相对 cache_dir，实际:\n{raw}"
        );
        assert!(
            raw.contains("workspace_dir = \"workspace\""),
            "落盘应保持相对 workspace_dir，实际:\n{raw}"
        );
        assert!(
            !raw.contains(root.to_string_lossy().as_ref()),
            "落盘不得包含 root 绝对路径，实际:\n{raw}"
        );

        // 重载后字段仍为相对
        let reloaded = AppConfig::load(&dir).expect("reload");
        assert_eq!(reloaded.models.cache_dir, "models");
        assert_eq!(reloaded.pipeline.workspace_dir, "workspace");
        assert_eq!(
            reloaded.models.cache_paths,
            vec!["shared/models".to_string(), "extra/cache".to_string()]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 绝对配置：加载 → resolve → save → 保持绝对原值
    #[test]
    fn save_preserves_absolute_paths() {
        let dir = std::env::temp_dir().join(format!("ep_config_abs_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let abs_cache = if cfg!(windows) { "D:/AI_Models" } else { "/opt/models" };
        let abs_ws = if cfg!(windows) { "D:/workspaces" } else { "/opt/workspaces" };
        let toml_src = format!(
            "[models]\ncache_dir = \"{abs_cache}\"\n\n[pipeline]\nworkspace_dir = \"{abs_ws}\"\n"
        );
        std::fs::write(dir.join(CONFIG_FILE_NAME), toml_src).unwrap();

        let mut config = AppConfig::load(&dir).expect("load");
        config.resolve_paths(dir.parent().unwrap());

        // 绝对配置 → 视图即原值
        assert_eq!(
            config.resolved_paths().model_cache_dir,
            PathBuf::from(abs_cache)
        );
        assert_eq!(
            config.resolved_paths().workspace_dir,
            PathBuf::from(abs_ws)
        );

        config.save(&dir).expect("save");
        let reloaded = AppConfig::load(&dir).expect("reload");
        assert_eq!(reloaded.models.cache_dir, abs_cache);
        assert_eq!(reloaded.pipeline.workspace_dir, abs_ws);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// PUT 语义回归：merge_partial（serde 重建）+ 重新 resolve + save → 相对形态保持
    #[test]
    fn merge_then_save_preserves_relative_paths() {
        let dir = std::env::temp_dir().join(format!("ep_config_merge_rel_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let mut config = AppConfig::default(); // 出厂相对配置
        let root = dir.join("fake-root");
        config.resolve_paths(&root);

        config
            .merge_partial(&serde_json::json!({"general": {"language": "en-US"}}))
            .expect("merge");
        // merge 经 serde 重建 → 运行期缓存重置 → 重新解析（daemon put_config 同口径）
        assert_eq!(config.resolved_paths(), &ResolvedPaths::default());
        config.resolve_paths(&root);
        assert!(config.resolved_paths().model_cache_dir.is_absolute());

        config.save(&dir).expect("save");
        let reloaded = AppConfig::load(&dir).expect("reload");
        assert_eq!(reloaded.general.language, "en-US");
        assert_eq!(reloaded.models.cache_dir, "models");
        assert_eq!(reloaded.pipeline.workspace_dir, "workspace");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 运行期解析缓存不参与序列化（TOML 落盘 / JSON roundtrip 均跳过）
    #[test]
    fn resolved_paths_not_serialized() {
        let mut config = AppConfig::default();
        let root = if cfg!(windows) {
            PathBuf::from("C:/app/root")
        } else {
            PathBuf::from("/app/root")
        };
        config.resolve_paths(&root);
        assert!(config.resolved_paths().model_cache_dir.is_absolute());

        let toml_str = toml::to_string_pretty(&config).expect("serialize");
        assert!(
            !toml_str.contains("resolved"),
            "运行期缓存不得出现在 TOML 落盘内容中:\n{toml_str}"
        );

        let json = serde_json::to_value(&config).expect("json");
        assert!(json.get("resolved").is_none());
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

    // ── P1：save 原子写盘 / 半写文件容错（回归）──────────────────────────

    /// P1 回归：save 后目标文件完整可解析、不残留 .tmp 临时文件（原子写盘）
    #[test]
    fn save_writes_complete_atomic_file() {
        let dir = std::env::temp_dir().join(format!("ep_config_atomic_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let mut config = AppConfig::default();
        config.general.language = "en-US".into();
        config.ports.range_start = 20000;
        config.save(&dir).expect("save");

        // 落盘文件完整可解析
        let loaded = AppConfig::load(&dir).expect("reload");
        assert_eq!(loaded.general.language, "en-US");
        assert_eq!(loaded.ports.range_start, 20000);

        // 临时文件已被 rename 走，不残留
        let tmp = dir.join(format!("{CONFIG_FILE_NAME}.tmp"));
        assert!(!tmp.exists(), "save 后不得残留 .tmp 临时文件");

        // 覆盖保存同样完整（rename 替换已存在目标）
        config.general.theme = "light".into();
        config.save(&dir).expect("resave");
        let reloaded = AppConfig::load(&dir).expect("reload");
        assert_eq!(reloaded.general.theme, "light");
        assert!(!tmp.exists(), "覆盖保存后也不得残留 .tmp");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P1 回归：模拟写一半崩溃（截断的 TOML）→ load 应报错而非 panic、
    /// 而非静默回退默认；save 原子覆盖后可自愈
    #[test]
    fn load_half_written_file_returns_error() {
        let dir = std::env::temp_dir().join(format!("ep_config_half_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 半写现场：合法序列化内容的前半截（截断在表名中间）
        let full = toml::to_string_pretty(&AppConfig::default()).unwrap();
        let half = &full[..full.len() / 2];
        std::fs::write(dir.join(CONFIG_FILE_NAME), half).unwrap();

        // 必须返回 Err（错误上下文含文件名），绝不 panic
        let err = AppConfig::load(&dir).unwrap_err();
        assert!(
            err.to_string().contains("failed to parse"),
            "半写文件应报解析错误，实际: {err}"
        );

        // 修复现场：save 原子覆盖损坏文件后可正常加载
        AppConfig::default()
            .save(&dir)
            .expect("save over half-written file");
        let loaded = AppConfig::load(&dir).expect("recover");
        assert_eq!(loaded.general.language, "zh-CN");
        assert!(!dir.join(format!("{CONFIG_FILE_NAME}.tmp")).exists());

        let _ = std::fs::remove_dir_all(&dir);
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
