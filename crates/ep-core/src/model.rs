//! 模型下载管理 — ModelManager
//!
//! 负责模型缓存目录管理、元数据读写、下载命令构建。
//! 不实际执行下载（只构建命令），实际执行由 ProcessManager / UI 层驱动。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::process::Child;
use tokio::sync::{broadcast, oneshot, Semaphore};
use tracing::{debug, info, warn};

use crate::config::{AppConfig, ModelsConfig, NetworkConfig};
use crate::module::manifest::{ModelDecl, ModelSource, ModuleManifest};

// ─── 常量 ────────────────────────────────────────────────────────────────────

const META_FILE_NAME: &str = ".ep_meta.json";

/// 下载监督任务总时长上限（秒）：超时 kill 下载子进程并清理临时文件（P1）
const DOWNLOAD_MAX_DURATION_SECS: u64 = 30 * 60;
/// 下载临时文件基名（生成的 python 与 Rust 侧取消/超时路径共知，P1 清理用）
const DOWNLOAD_TEMP_FILE: &str = "_download.tar.gz";
/// 解压总量上限（字节）：防御 tar 炸弹（P1，与项目模型规模匹配）
const EXTRACT_MAX_BYTES: u64 = 50 * 1024 * 1024 * 1024;
/// 解压条目数上限：防御条目海量 tar（P1）
const EXTRACT_MAX_ENTRIES: u64 = 100_000;

// ─── ModelMeta ───────────────────────────────────────────────────────────────

/// 模型元数据，对应 `.ep_meta.json` 文件。
///
/// 位于模型缓存目录内每个模型文件夹下。
/// 用户可安全删除此文件——删除后系统视为手动放置的模型，直接使用。
///
/// 新增字段（`qualified_id` / `tags` / `pack_id`）均带 `serde(default)`：
/// 不含这些字段的旧 meta 文件可正常加载（向后兼容）；字段为空时序列化输出
/// 不包含对应键，写出格式与旧版一致。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMeta {
    /// 所属模块 ID
    pub module_id: String,
    /// 模型 ID（对应 module.toml 中 [[models]].id）
    pub model_id: String,
    /// 下载来源 — 自由字符串，不强类型化（审计 §6 正面确认）：
    /// `huggingface` / `modelscope` / `url`（下载）、`local_import`（本地导入）、
    /// `pack`（整合包导入，§4.4，由 B1 写入）
    pub source: String,
    /// 仓库 ID
    pub repo_id: String,
    /// 版本/分支
    pub revision: String,
    /// 下载完成时间（ISO 8601）
    pub downloaded_at: String,
    /// 总大小（字节）
    pub total_size_bytes: u64,
    /// 全限定模型 ID（`publisher.vendor.model`，§4.3）；
    /// 整合包导入/导出时写入，普通下载/本地导入为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualified_id: Option<String>,
    /// 模型标签（统一页筛选 / 整合包圈选，§5.1/§4.5；存 meta 随包流转）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// 来源整合包 ID（§4.4）；非整合包导入为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_id: Option<String>,
}

// ─── DownloadedModel ─────────────────────────────────────────────────────────

/// 已下载模型的摘要信息（用于列表展示）
#[derive(Debug, Clone)]
pub struct DownloadedModel {
    /// 相对于 cache_dir 的目录名
    pub target_dir: String,
    /// 元数据
    pub meta: ModelMeta,
    /// 目录总大小（字节）。真实递归累计（P2-10），语义见 [`dir_total_size`]：
    /// 基于 symlink_metadata，不跟随符号链接、防缓存布局环。
    pub size_bytes: u64,
}

// ─── ModelStatus ─────────────────────────────────────────────────────────────

/// 模型状态
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelStatus {
    /// 模型文件完整（目录存在且包含文件）
    Ready,
    /// 模型目录不存在
    Missing,
    /// 模型目录存在但文件不完整（空目录）
    Incomplete,
    /// 在配置的本地缓存路径中找到可导入的模型
    Importable,
}

impl std::fmt::Display for ModelStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Ready => "ready",
            Self::Missing => "missing",
            Self::Incomplete => "incomplete",
            Self::Importable => "importable",
        };
        write!(f, "{s}")
    }
}

// ─── ModelInfo ───────────────────────────────────────────────────────────────

/// 模型详细信息（用于 API 响应和 UI 展示）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// 模型 ID（对应 module.toml 中 [[models]].id）
    pub model_id: String,
    /// 模型显示名称
    pub name: String,
    /// 相对于 cache_dir 的目标目录
    pub target_dir: String,
    /// 当前状态
    pub status: ModelStatus,
    /// 目录总大小（字节）
    pub size_bytes: u64,
    /// 文件数量
    pub file_count: usize,
    /// 如果在本地缓存路径中找到，记录该路径
    pub local_cache_path: Option<PathBuf>,
    /// 可用下载源列表（主 source + mirrors，去重）
    pub available_sources: Vec<ModelSource>,
}

// ─── ModelView ───────────────────────────────────────────────────────────────

/// 每个模型的展示信息（含状态），用于桌面 GUI 模型列表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelView {
    /// 所属模块 ID
    pub module_id: String,
    /// 所属模块显示名称
    pub module_name: String,
    /// 模型 ID（对应 module.toml 中 [[models]].id）
    pub model_id: String,
    /// 模型显示名称
    pub model_name: String,
    /// 下载源："huggingface" | "modelscope" | "url"
    pub source: String,
    /// 仓库 ID（HuggingFace / ModelScope）
    pub repo_id: String,
    /// 相对于 cache_dir 的目标目录
    pub target_dir: String,
    /// 当前状态
    pub status: ModelStatus,
    /// 目录总大小（字节），目录不存在时为 None
    pub size_bytes: Option<u64>,
    /// 可用下载源列表（主 source + mirrors，去重）
    pub available_sources: Vec<ModelSource>,
}

// ─── 下载进度 ────────────────────────────────────────────────────────────────

/// 下载进度事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgress {
    /// 所属模块 ID
    pub module_id: String,
    /// 模型 ID
    pub model_id: String,
    /// 进度百分比（0.0–99.0 下载中，100.0 完成；无大小估算时恒为 0.0）
    pub percent: f32,
    /// 目标目录当前已落盘的字节数
    pub bytes: u64,
    /// 当前状态
    pub state: DownloadState,
}

/// 下载状态
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind", content = "detail")]
pub enum DownloadState {
    /// 下载进行中
    Downloading,
    /// 下载成功完成
    Completed,
    /// 下载失败（附中文错误摘要）
    Failed(String),
    /// 下载已被取消
    Cancelled,
}

/// 更新检查结果（best-effort，永不 panic）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheckResult {
    /// 远端是否有比本地更新的版本
    pub available: bool,
    /// 中文原因说明（无论 available 与否都填充）
    pub reason: String,
    /// 远端最后修改时间（RFC 3339），无法获取时为 None
    pub remote_modified: Option<String>,
}

// ─── ModelManager ────────────────────────────────────────────────────────────

/// 模型下载管理器
///
/// 管理模型缓存目录、元数据文件、下载命令构建。
pub struct ModelManager {
    /// 模型缓存根目录（已解析为绝对路径）
    cache_dir: PathBuf,
    /// HuggingFace 镜像站 URL（空字符串 = 官方源）
    hf_endpoint: String,
    /// ModelScope API 基址（默认官方站；经 [`Self::with_modelscope_endpoint`]
    /// 覆盖——更新检查测试注入本地 mock 端点用）
    modelscope_endpoint: String,
    /// 默认下载源（huggingface / modelscope）
    default_source: String,
    /// 本地模型缓存搜索路径（按优先级排序）
    cache_paths: Vec<PathBuf>,
    /// 当前正在执行的下载子进程（用于取消）。
    ///
    /// 单槽位限制（P2，文档化）：同一实例并发多次 `execute_download*` 会
    /// 覆盖此句柄，`cancel_download` 只能取消最后一次提交的进程。带进度下载
    /// 不受影响（取消走各自独立的 `DownloadHandle`）。保留单槽位设计——
    /// 结构字段冻结，阻塞式下载的并发建议由调用方串行化。
    download_child: Option<Child>,
    /// 网络代理配置（更新检查 HTTP 客户端使用）
    network: NetworkConfig,
    /// 已注册的模块 manifest 列表（import_model 据此解析真实 target_dir）
    manifests: Vec<ModuleManifest>,
}

impl ModelManager {
    /// 从配置创建 ModelManager。
    ///
    /// `cache_dir` 为相对路径时基于 `root` 解析。
    pub fn new(config: &ModelsConfig, root: &Path) -> Self {
        let cache_path = Path::new(&config.cache_dir);
        let cache_dir = if cache_path.is_absolute() {
            cache_path.to_path_buf()
        } else {
            root.join(cache_path)
        };

        let cache_paths: Vec<PathBuf> = config
            .cache_paths
            .iter()
            .map(|p| {
                let pb = Path::new(p);
                if pb.is_absolute() {
                    pb.to_path_buf()
                } else {
                    root.join(pb)
                }
            })
            .collect();

        debug!(
            cache_dir = %cache_dir.display(),
            hf_endpoint = %config.hf_endpoint,
            default_source = %config.default_source,
            cache_paths = ?cache_paths,
            "ModelManager initialized"
        );

        Self {
            cache_dir,
            hf_endpoint: config.hf_endpoint.clone(),
            modelscope_endpoint: "https://modelscope.cn".to_string(),
            default_source: config.default_source.clone(),
            cache_paths,
            download_child: None,
            network: NetworkConfig::default(),
            manifests: Vec::new(),
        }
    }

    /// 设置网络代理配置（链式调用）。
    ///
    /// 用于 `check_update_available` 的 HTTP 客户端；
    /// 下载子进程的代理环境变量由 `execute_download*` 的 AppConfig 参数提供。
    pub fn with_network(mut self, network: NetworkConfig) -> Self {
        self.network = network;
        self
    }

    /// 覆盖 ModelScope API 基址（链式调用；缺省官方站 `https://modelscope.cn`）。
    ///
    /// 更新检查的 ModelScope 请求 URL 形如 `{endpoint}/api/v1/models/{repo_id}`；
    /// 尾斜杠自动去除。
    pub fn with_modelscope_endpoint(mut self, endpoint: &str) -> Self {
        self.modelscope_endpoint = endpoint.trim_end_matches('/').to_string();
        self
    }

    /// 注册模块 manifest 列表（链式调用）。
    ///
    /// `import_model` 据此把 model_id 解析为 manifest 中声明的真实 target_dir。
    pub fn with_manifests(mut self, manifests: Vec<ModuleManifest>) -> Self {
        self.manifests = manifests;
        self
    }

    /// 注册/更新模块 manifest 列表
    pub fn set_manifests(&mut self, manifests: Vec<ModuleManifest>) {
        self.manifests = manifests;
    }

    /// 返回模型的完整缓存路径：`cache_dir / target_dir`
    pub fn model_dir(&self, target_dir: &str) -> PathBuf {
        self.cache_dir.join(target_dir)
    }

    /// 检查模型是否已存在（目录存在且非空）
    pub fn is_model_present(&self, target_dir: &str) -> bool {
        let dir = self.model_dir(target_dir);
        if !dir.is_dir() {
            return false;
        }
        // 目录存在且至少包含一个条目
        match fs::read_dir(&dir) {
            Ok(mut entries) => entries.next().is_some(),
            Err(_) => false,
        }
    }

    /// 读取模型的 `.ep_meta.json` 元数据
    pub fn read_meta(&self, target_dir: &str) -> Option<ModelMeta> {
        let meta_path = self.model_dir(target_dir).join(META_FILE_NAME);
        if !meta_path.is_file() {
            return None;
        }
        let content = match fs::read_to_string(&meta_path) {
            Ok(c) => c,
            Err(e) => {
                warn!(path = %meta_path.display(), error = %e, "failed to read meta file");
                return None;
            }
        };
        match serde_json::from_str(&content) {
            Ok(meta) => Some(meta),
            Err(e) => {
                warn!(path = %meta_path.display(), error = %e, "failed to parse meta file");
                None
            }
        }
    }

    /// 写入模型的 `.ep_meta.json` 元数据
    pub fn write_meta(&self, target_dir: &str, meta: &ModelMeta) -> Result<()> {
        // target_dir 校验（P1）：拒绝路径穿越与非法字符，违规返回错误
        validate_target_dir(target_dir)?;
        let dir = self.model_dir(target_dir);
        write_meta_to_dir(&dir, meta)
    }

    /// 构建模型下载命令（不实际执行）— 使用模型的主 source。
    ///
    /// 返回 `(program, args)`：
    /// - `program`：venv 中的 python 解释器路径
    /// - `args`：`["-c", "<python code>"]`
    ///
    /// - HuggingFace：使用 `huggingface_hub.snapshot_download`
    /// - ModelScope：使用 `modelscope.snapshot_download`
    /// - 如果配置了 `hf_endpoint`，在 Python 代码中设置 `HF_ENDPOINT` 环境变量
    ///
    /// 需要指定下载源（主源 / mirror）时使用
    /// `build_download_command_with_source`。
    pub fn build_download_command(
        &self,
        model: &ModelDecl,
        venv_python: &Path,
    ) -> (String, Vec<String>) {
        let local_dir = self.model_dir(&model.target_dir);
        // 将路径转为正斜杠字符串，避免 Windows 反斜杠在 Python 字符串中的转义问题
        let local_dir_str = local_dir.to_string_lossy().replace('\\', "/");

        let location = match model.source {
            ModelSource::Huggingface | ModelSource::Modelscope => {
                model.repo_id.clone().unwrap_or_default()
            }
            ModelSource::Url => model.url.clone().unwrap_or_default(),
            // 本地自建：无下载脚本语义 → 生成 no-op 脚本（存在即就绪）
            ModelSource::LocalImport => String::new(),
        };
        // 校验失败（本 pub 签名无法返回 Result）时生成"报错退出"的占位脚本，
        // 绝不把非法输入（target_dir 穿越 / 注入字符）拼进可执行 python 代码
        let python_code = match validate_target_dir(&model.target_dir).and_then(|()| {
            self.gen_download_code(
                model.source,
                &location,
                model.revision.as_deref(),
                &local_dir_str,
            )
        }) {
            Ok(code) => code,
            Err(e) => download_error_stub(&format!("invalid model download source: {e:#}")),
        };

        let program = venv_python.to_string_lossy().to_string();
        let args = vec!["-c".to_string(), python_code];

        debug!(
            program = %program,
            source = ?model.source,
            repo_id = ?model.repo_id,
            "download command built"
        );

        (program, args)
    }

    /// 构建模型下载命令（支持下载源覆写）。
    ///
    /// `source` 为 `None` 时使用主 source；为 `Some(s)` 时通过
    /// `ModelDecl::resolve` 选择主字段或 mirror 字段，不可用时返回中文错误。
    pub fn build_download_command_with_source(
        &self,
        model: &ModelDecl,
        venv_python: &Path,
        source: Option<ModelSource>,
    ) -> Result<(String, Vec<String>)> {
        let (resolved_source, location, revision) = model.resolve(source)?;

        // target_dir 校验（P1）：恶意/损坏 manifest 的 "../x" 不得写出缓存根
        validate_target_dir(&model.target_dir)?;

        let local_dir = self.model_dir(&model.target_dir);
        let local_dir_str = local_dir.to_string_lossy().replace('\\', "/");

        let python_code =
            self.gen_download_code(resolved_source, &location, revision.as_deref(), &local_dir_str)?;

        let program = venv_python.to_string_lossy().to_string();
        let args = vec!["-c".to_string(), python_code];

        debug!(
            program = %program,
            source = ?resolved_source,
            location = %location,
            "download command built (source override)"
        );

        Ok((program, args))
    }

    /// 生成下载用的 python -c 代码
    ///
    /// 所有拼进单引号 Python 字符串字面量的值都经 [`escape_py_str`] 转义，
    /// 并做字符集/协议校验（P0 注入防护）；返回 Err 时调用方应放弃执行。
    fn gen_download_code(
        &self,
        source: ModelSource,
        location: &str,
        revision: Option<&str>,
        local_dir_str: &str,
    ) -> Result<String> {
        match source {
            // 本地自建（E7）：无远端可下，产出 no-op 脚本——目录存在即就绪，
            // 缺失时报错文案引导用户查看模块 README 的获取说明
            ModelSource::LocalImport => {
                let dir = local_dir_str;
                let script = format!(
                    "import os, sys\ndir_ = r\"{dir}\"\n\
                     if not os.path.isdir(dir_) or not os.listdir(dir_):\n    \
                     sys.stderr.write('local-import model missing: ' + dir_ + ' (see module README for how to obtain it)\\n')\n    \
                     sys.exit(1)\n\
                     print('local-import model present:', dir_)\n"
                );
                Ok(script)
            }
            ModelSource::Huggingface => {
                let repo_id = location;
                validate_repo_id(repo_id)?;
                let revision = revision.unwrap_or("main");

                let mut parts: Vec<String> = Vec::new();

                // 如果配置了 HF 镜像，在导入前设置环境变量
                if !self.hf_endpoint.is_empty() {
                    parts.push(format!(
                        "import os; os.environ['HF_ENDPOINT']='{}'",
                        escape_py_str(&self.hf_endpoint)
                    ));
                }

                parts.push("from huggingface_hub import snapshot_download".to_string());
                parts.push(format!(
                    "snapshot_download(repo_id='{}', local_dir='{}', revision='{}')",
                    escape_py_str(repo_id),
                    escape_py_str(local_dir_str),
                    escape_py_str(revision),
                ));

                Ok(parts.join("; "))
            }
            ModelSource::Modelscope => {
                let repo_id = location;
                validate_repo_id(repo_id)?;
                let revision = revision.unwrap_or("master");

                Ok(format!(
                    "from modelscope import snapshot_download; snapshot_download('{}', local_dir='{}', revision='{}')",
                    escape_py_str(repo_id),
                    escape_py_str(local_dir_str),
                    escape_py_str(revision),
                ))
            }
            ModelSource::Url => {
                let url = location;

                if url == "auto" {
                    // 模块自行管理模型下载（如 PaddleOCR 首次运行时自动下载）
                    // 生成一个空操作命令，标记为已就绪
                    Ok(format!(
                        "import os; os.makedirs('{}', exist_ok=True); print('auto-download model: skipped (managed by module)')",
                        escape_py_str(local_dir_str)
                    ))
                } else {
                    validate_url(url)?;
                    if url.ends_with(".tar.gz") || url.ends_with(".tgz") {
                        // 下载 .tar.gz 并安全解压到目标目录（P1 zip-slip 防护）：
                        // 逐成员校验（拒绝绝对路径 / `..` 穿越 / 符号链接），并设
                        // 总量与条目数上限；随后展平包装目录（df3 的 tmp/export/
                        // 嵌套打包），使模型文件直接落在 target_dir 下。
                        // try/finally 保证临时包 `_download.tar.gz` 失败时也被清理。
                        // 多行代码（python -c 参数支持换行）：解压校验循环与
                        // 展平循环需要 def/for/while。Python 3.10+ 兼容
                        // （不用 3.12 新增的 tarfile extractall filter= 参数）。
                        Ok(format!(
                            "import urllib.request, tarfile, os, shutil\n\
                             os.makedirs('{dir}', exist_ok=True)\n\
                             tmp = os.path.join('{dir}', '_download.tar.gz')\n\
                             def _ep_flatten(d):\n\
                             \x20   while True:\n\
                             \x20   \x20   entries = [e for e in os.listdir(d) if not e.startswith('.') and e != '_download.tar.gz']\n\
                             \x20   \x20   if len(entries) != 1:\n\
                             \x20   \x20   \x20   break\n\
                             \x20   \x20   inner = os.path.join(d, entries[0])\n\
                             \x20   \x20   if not os.path.isdir(inner):\n\
                             \x20   \x20   \x20   break\n\
                             \x20   \x20   for name in os.listdir(inner):\n\
                             \x20   \x20   \x20   shutil.move(os.path.join(inner, name), os.path.join(d, name))\n\
                             \x20   \x20   os.rmdir(inner)\n\
                             try:\n\
                             \x20   print('Downloading {url} ...')\n\
                             \x20   urllib.request.urlretrieve('{url}', tmp)\n\
                             \x20   print('Extracting...')\n\
                             \x20   t = tarfile.open(tmp)\n\
                             \x20   base = os.path.abspath('{dir}')\n\
                             \x20   total = 0\n\
                             \x20   count = 0\n\
                             \x20   for m in t.getmembers():\n\
                             \x20   \x20   name = m.name.replace('\\\\', '/')\n\
                             \x20   \x20   if not name or name == '.' or name.startswith('/') or os.path.isabs(m.name):\n\
                             \x20   \x20   \x20   raise RuntimeError('unsafe tar entry: ' + repr(m.name))\n\
                             \x20   \x20   tgt = os.path.abspath(os.path.join(base, name))\n\
                             \x20   \x20   if os.path.commonpath([base, tgt]) != base:\n\
                             \x20   \x20   \x20   raise RuntimeError('unsafe tar entry: ' + repr(m.name))\n\
                             \x20   \x20   if m.issym() or m.islnk():\n\
                             \x20   \x20   \x20   continue\n\
                             \x20   \x20   if count >= {max_entries} or total + max(m.size, 0) > {max_bytes}:\n\
                             \x20   \x20   \x20   raise RuntimeError('extract limits exceeded')\n\
                             \x20   \x20   count += 1\n\
                             \x20   \x20   total += max(m.size, 0)\n\
                             \x20   \x20   t.extract(m, base)\n\
                             \x20   t.close()\n\
                             \x20   os.remove(tmp)\n\
                             \x20   _ep_flatten('{dir}')\n\
                             \x20   print('Done.')\n\
                             finally:\n\
                             \x20   if os.path.exists(tmp):\n\
                             \x20   \x20   os.remove(tmp)",
                            dir = escape_py_str(local_dir_str),
                            url = escape_py_str(url),
                            max_bytes = EXTRACT_MAX_BYTES,
                            max_entries = EXTRACT_MAX_ENTRIES,
                        ))
                    } else {
                        // 单文件下载（.onnx 等）：文件名剥离 query/fragment 并清洗
                        let file_name = sanitize_file_name(url);
                        Ok(format!(
                            "import urllib.request, os; \
                             os.makedirs('{dir}', exist_ok=True); \
                             dst = os.path.join('{dir}', '{fname}'); \
                             print('Downloading {url} ...'); \
                             urllib.request.urlretrieve('{url}', dst); \
                             print('Done.')",
                            dir = escape_py_str(local_dir_str),
                            fname = escape_py_str(&file_name),
                            url = escape_py_str(url),
                        ))
                    }
                }
            }
        }
    }

    /// 检查模型是否有可用更新（best-effort，永不 panic）。
    ///
    /// 比较远端仓库最后修改时间与本地 `.ep_meta.json` 的 `downloaded_at`：
    /// - HuggingFace：GET `{hf_endpoint或官方}/api/models/{repo_id}`，字段 `lastModified`（RFC 3339）
    /// - ModelScope：GET `{modelscope_endpoint，默认官方}/api/v1/models/{repo_id}`，
    ///   字段 `Data.LastUpdatedTime`（Unix 秒），回退 `Data.GmtModified`
    /// - URL 来源 / url="auto"：不支持更新检查
    /// - 无 meta 文件：返回"缺少下载元数据"，不误报
    ///
    /// 网络请求使用 `NetworkConfig` 代理配置，超时 10 秒。
    pub async fn check_update_available(&self, model: &ModelDecl) -> UpdateCheckResult {
        // URL 来源不支持更新检查
        if model.source == ModelSource::Url {
            return UpdateCheckResult {
                available: false,
                reason: "URL source does not support update checks".to_string(),
                remote_modified: None,
            };
        }

        let repo_id = match model.repo_id.as_deref() {
            Some(r) if !r.is_empty() => r.to_string(),
            _ => {
                return UpdateCheckResult {
                    available: false,
                    reason: "model does not declare repo_id, cannot check for updates".to_string(),
                    remote_modified: None,
                };
            }
        };

        // 无元数据 → 无法比较，不误报
        let meta = match self.read_meta(&model.target_dir) {
            Some(m) => m,
            None => {
                return UpdateCheckResult {
                    available: false,
                    reason: "missing download metadata".to_string(),
                    remote_modified: None,
                };
            }
        };

        let local_time = match chrono::DateTime::parse_from_rfc3339(&meta.downloaded_at) {
            Ok(t) => t.with_timezone(&chrono::Utc),
            Err(_) => {
                return UpdateCheckResult {
                    available: false,
                    reason: format!(
                        "invalid local download timestamp ({}), cannot compare",
                        meta.downloaded_at
                    ),
                    remote_modified: None,
                };
            }
        };

        let client = match build_proxied_http_client(&self.network, Duration::from_secs(10)) {
            Ok(c) => c,
            Err(e) => {
                return UpdateCheckResult {
                    available: false,
                    reason: format!("failed to build HTTP client: {e}"),
                    remote_modified: None,
                };
            }
        };

        let remote_time = match model.source {
            ModelSource::Huggingface => {
                let endpoint = if self.hf_endpoint.is_empty() {
                    "https://huggingface.co".to_string()
                } else {
                    self.hf_endpoint.trim_end_matches('/').to_string()
                };
                fetch_hf_modified(&client, &endpoint, &repo_id).await
            }
            ModelSource::Modelscope => {
                fetch_modelscope_modified(&client, &self.modelscope_endpoint, &repo_id).await
            }
            ModelSource::Url => unreachable!("url source handled above"),
            // 本地权重无远端更新语义：以纪元哨兵走「已是最新」分支
            ModelSource::LocalImport => Ok((
                chrono::DateTime::from_timestamp(0, 0).unwrap(),
                String::new(),
            )),
        };

        match remote_time {
            Ok((remote, remote_str)) => {
                if remote > local_time {
                    UpdateCheckResult {
                        available: true,
                        reason: "update available".to_string(),
                        remote_modified: Some(remote_str),
                    }
                } else {
                    UpdateCheckResult {
                        available: false,
                        reason: "already up to date".to_string(),
                        remote_modified: Some(remote_str),
                    }
                }
            }
            Err(e) => UpdateCheckResult {
                available: false,
                reason: format!("update check failed: {e}"),
                remote_modified: None,
            },
        }
    }

    /// 扫描 cache_dir 下所有含 `.ep_meta.json` 的目录，返回已下载模型列表。
    pub fn list_downloaded_models(&self) -> Vec<DownloadedModel> {
        let mut models = Vec::new();

        let entries = match fs::read_dir(&self.cache_dir) {
            Ok(entries) => entries,
            Err(e) => {
                debug!(
                    cache_dir = %self.cache_dir.display(),
                    error = %e,
                    "cannot read model cache dir (may not exist yet)"
                );
                return models;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let meta_path = path.join(META_FILE_NAME);
            if !meta_path.is_file() {
                continue;
            }

            let target_dir = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => name.to_string(),
                None => continue,
            };

            match self.read_meta(&target_dir) {
                Some(meta) => {
                    // P2-10：真实递归累计目录大小
                    // （dir_total_size 基于 symlink_metadata：不跟随 symlink、防环）
                    let size_bytes = dir_total_size(&path);
                    models.push(DownloadedModel {
                        target_dir,
                        meta,
                        size_bytes,
                    });
                }
                None => {
                    warn!(dir = %target_dir, "meta file exists but failed to parse, skipping");
                }
            }
        }

        models
    }

    /// 获取缓存根目录
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// 获取默认下载源
    pub fn default_source(&self) -> &str {
        &self.default_source
    }

    /// 实际执行模型下载（使用模型主 source）
    ///
    /// 调用 `build_download_command()` 获取命令，用 `tokio::process::Command` 执行。
    /// 下载进程句柄保存在 `download_child` 中，可通过 `cancel_download()` 取消。
    /// 成功后写入 `.ep_meta.json` 元数据。
    pub async fn execute_download(
        &mut self,
        model: &ModelDecl,
        module_dir: &Path,
        venv_python: &Path,
        config: &crate::config::AppConfig,
    ) -> Result<()> {
        self.execute_download_with_source(model, module_dir, venv_python, config, None)
            .await
    }

    /// 实际执行模型下载（支持下载源覆写）
    ///
    /// `source` 为 `None` 时使用主 source；`Some(s)` 时经 `ModelDecl::resolve`
    /// 选择主字段或 mirror。成功后写入 `.ep_meta.json`（记录实际使用的来源）。
    /// 下载子进程注入 `config.network` 的代理环境变量（仅非空值）。
    pub async fn execute_download_with_source(
        &mut self,
        model: &ModelDecl,
        module_dir: &Path,
        venv_python: &Path,
        config: &crate::config::AppConfig,
        source: Option<ModelSource>,
    ) -> Result<()> {
        let (program, args) =
            self.build_download_command_with_source(model, venv_python, source)?;
        let (resolved_source, location, revision) = model.resolve(source)?;

        // 下载并发闸（P2）：按 config.models.max_concurrent_downloads 限流，
        // 持 permit 直至本函数结束（子进程回收后归还）
        let _gate_permit = model_download_gate(config.models.max_concurrent_downloads)
            .acquire_owned()
            .await
            .context("failed to acquire download concurrency slot")?;

        let target_dir_path = self.model_dir(&model.target_dir);

        info!(
            model_id = %model.id,
            program = %program,
            source = %resolved_source,
            "executing model download"
        );

        let mut cmd = tokio::process::Command::new(&program);
        cmd.args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // 注入网络代理环境变量（仅非空值，不覆盖继承值）
        for (key, value) in config.network.env_vars() {
            cmd.env(key, value);
        }

        // 残缺产物清理依据：启动时目录是否为空（见 cleanup_partial_download）
        let dir_was_empty_at_start = !dir_has_entries(&target_dir_path);

        let child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn download for model '{}'", model.id))?;

        self.download_child = Some(child);

        // Wait for the download to complete
        if let Some(child) = self.download_child.take() {
            let output = match child.wait_with_output().await {
                Ok(output) => output,
                Err(e) => {
                    // P1：等待失败时清理残缺产物（python 侧可能来不及清理）
                    cleanup_partial_download(&target_dir_path, dir_was_empty_at_start);
                    return Err(e).with_context(|| {
                        format!("failed to wait for download of model '{}'", model.id)
                    });
                }
            };

            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            if !stdout.is_empty() {
                debug!(model_id = %model.id, stdout = %stdout.trim(), "download stdout");
            }
            if !stderr.is_empty() {
                debug!(model_id = %model.id, stderr = %stderr.trim(), "download stderr");
            }

            if !output.status.success() {
                // P1：失败时清理残缺产物（python try/finally 的兜底）
                cleanup_partial_download(&target_dir_path, dir_was_empty_at_start);
                anyhow::bail!(
                    "download of model '{}' failed with exit code {:?}: {}",
                    model.id,
                    output.status.code(),
                    stderr.trim()
                );
            }

            // 校验门禁：声明了 sha256/sha256s 时，校验通过才算下载成功；
            // 失败按残缺产物清理，防止坏文件蒙混成 ready
            if let Err(e) =
                verify_model_checksum(&target_dir_path, &ModelChecksumSpec::from_decl(model), &model.id)
            {
                cleanup_partial_download(&target_dir_path, dir_was_empty_at_start);
                anyhow::bail!("{e:#}");
            }
        } else {
            anyhow::bail!("download process for model '{}' was lost", model.id);
        }

        // 成功后写入下载元数据
        let total_size = dir_total_size(&target_dir_path);
        let module_id = module_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let meta = ModelMeta {
            module_id: module_id.to_string(),
            model_id: model.id.clone(),
            source: resolved_source.as_str().to_string(),
            repo_id: location,
            revision: revision
                .unwrap_or_else(|| default_revision_for(resolved_source).to_string()),
            downloaded_at: chrono::Utc::now().to_rfc3339(),
            total_size_bytes: total_size,
            // 普通下载不带整合包语义字段（pack 来源由 B1 导入流程写入）
            qualified_id: None,
            tags: Vec::new(),
            pack_id: None,
        };
        if let Err(e) = self.write_meta(&model.target_dir, &meta) {
            warn!(
                model_id = %model.id,
                error = %e,
                "failed to write model meta after download (non-fatal)"
            );
        }

        Ok(())
    }

    /// 取消正在进行的下载（kill 进程）
    ///
    /// 单槽位限制（P2，文档化）：`download_child` 只能记录最近一次提交的
    /// 下载进程，同一实例并发多次阻塞式 `execute_download*` 时只能取消
    /// 最后一次；带进度下载（`execute_download_with_progress`）不受影响——
    /// 其取消走各自独立的 `DownloadHandle`。阻塞式下载建议由调用方串行化
    /// （与 daemon 层并发闸 P2-1 语义一致）。
    pub async fn cancel_download(&mut self) {
        if let Some(mut child) = self.download_child.take() {
            info!("cancelling model download");
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }

    // ─── 带进度的下载 ─────────────────────────────────────────────────────

    /// 启动带进度上报的模型下载，立即返回 `DownloadHandle`。
    ///
    /// 进度机制：子进程 spawn 后另起 tokio 任务，每 2 秒轮询目标目录的
    /// 递归落盘大小；`percent = min(99.0, bytes / size_estimate_mb * 100)`，
    /// 无大小估算时 percent 恒为 0 只报 bytes。子进程结束 → Completed(100) /
    /// Failed(stderr 尾部摘要，中文包装) / Cancelled。
    ///
    /// 进度通过 broadcast channel 发送：可多订阅者（GUI 与 WebUI 事件流可
    /// 同时订阅），且"无接收端"时 send 直接返回 Err 被忽略——进度发送失败
    /// 绝不影响下载本身。
    ///
    /// `source` 为下载源覆写（None = 主 source），走 `ModelDecl::resolve`。
    /// 成功后同样写入 `.ep_meta.json`。
    pub fn execute_download_with_progress(
        &self,
        module_id: &str,
        model: &ModelDecl,
        venv_python: &Path,
        config: &crate::config::AppConfig,
        source: Option<ModelSource>,
    ) -> Result<DownloadHandle> {
        let (program, args) =
            self.build_download_command_with_source(model, venv_python, source)?;
        let (resolved_source, location, revision) = model.resolve(source)?;

        info!(
            module_id = %module_id,
            model_id = %model.id,
            program = %program,
            source = %resolved_source,
            "starting model download with progress reporting"
        );

        let mut cmd = tokio::process::Command::new(&program);
        cmd.args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // 注入网络代理环境变量（仅非空值）
        for (key, value) in config.network.env_vars() {
            cmd.env(key, value);
        }

        // 残缺产物清理依据：spawn 前判定目录是否为空（见 cleanup_partial_download）
        let target_dir_path = self.model_dir(&model.target_dir);
        let dir_was_empty_at_start = !dir_has_entries(&target_dir_path);

        let child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn download for model '{}'", model.id))?;

        // 下载成功后要写入的元数据（total_size_bytes 由监督任务补齐）
        let meta = ModelMeta {
            module_id: module_id.to_string(),
            model_id: model.id.clone(),
            source: resolved_source.as_str().to_string(),
            repo_id: location,
            revision: revision
                .unwrap_or_else(|| default_revision_for(resolved_source).to_string()),
            downloaded_at: String::new(), // 完成时填充
            total_size_bytes: 0,           // 完成时填充
            qualified_id: None,
            tags: Vec::new(),
            pack_id: None,
        };

        Ok(spawn_tracked_download(
            child,
            target_dir_path,
            model.size_estimate_mb,
            module_id.to_string(),
            model.id.clone(),
            Some(meta),
            dir_was_empty_at_start,
            ModelChecksumSpec::from_decl(model),
        ))
    }

    // ─── 模型状态检查与管理 ────────────────────────────────────────────────

    /// 检查指定模块所有模型的状态
    ///
    /// 返回 model_id → ModelStatus 的映射。
    /// 检查逻辑：
    /// 1. cache_dir/target_dir 存在且有文件 → Ready
    /// 2. cache_dir/target_dir 存在但为空 → Incomplete
    /// 3. 在 cache_paths 中找到匹配目录 → Importable
    /// 4. 以上均不满足 → Missing
    pub fn check_model_status(
        &self,
        _module_id: &str,
        manifest: &ModuleManifest,
    ) -> HashMap<String, ModelStatus> {
        let mut statuses = HashMap::new();

        for model in &manifest.models {
            let status = self.check_single_model_status(&model.target_dir);
            statuses.insert(model.id.clone(), status);
        }

        statuses
    }

    /// 检查单个模型的状态
    fn check_single_model_status(&self, target_dir: &str) -> ModelStatus {
        let dir = self.model_dir(target_dir);

        if dir.is_dir() {
            // 目录存在，检查是否有文件
            if self.dir_has_files(&dir) {
                return ModelStatus::Ready;
            } else {
                return ModelStatus::Incomplete;
            }
        }

        // 目录不存在，搜索本地缓存路径
        if self.find_in_cache_paths(target_dir).is_some() {
            return ModelStatus::Importable;
        }

        ModelStatus::Missing
    }

    /// 检查目录是否包含至少一个文件（非递归，仅检查直接子条目）
    fn dir_has_files(&self, dir: &Path) -> bool {
        dir_has_entries(dir)
    }

    /// 在配置的本地缓存路径中搜索匹配的模型目录
    ///
    /// 按优先级顺序搜索，返回第一个找到的路径。
    /// 匹配规则：cache_path 下存在与 target_dir 同名的子目录且包含文件。
    fn find_in_cache_paths(&self, target_dir: &str) -> Option<PathBuf> {
        for cache_path in &self.cache_paths {
            // 直接匹配: cache_path/target_dir
            let candidate = cache_path.join(target_dir);
            if candidate.is_dir() && self.dir_has_files(&candidate) {
                debug!(
                    target_dir = %target_dir,
                    found_at = %candidate.display(),
                    "model found in local cache path"
                );
                return Some(candidate);
            }

            // 精确匹配（P2）：遍历 cache_path 下的子目录，目录名 == target_dir，
            // 或命中 HF 缓存布局 `models--org--name`（末段 == target_dir）。
            // 完整路径段比较，杜绝 "qwen" 子串误中 "qwen3-asr" 的误匹配。
            // 例如 target_dir = "faster-whisper-large-v3" 可精确命中
            // "models--Systran--faster-whisper-large-v3"。
            if let Ok(entries) = fs::read_dir(cache_path) {
                for entry in entries.flatten() {
                    let entry_path = entry.path();
                    if !entry_path.is_dir() {
                        continue;
                    }
                    if let Some(dir_name) = entry_path.file_name().and_then(|n| n.to_str()) {
                        if model_dir_name_matches(dir_name, target_dir)
                            && self.dir_has_files(&entry_path)
                        {
                            debug!(
                                target_dir = %target_dir,
                                found_at = %entry_path.display(),
                                "model found via exact name match in local cache path"
                            );
                            return Some(entry_path);
                        }
                    }
                }
            }
        }
        None
    }

    /// 从本地路径导入模型
    ///
    /// 将 source_path 下的所有文件异步复制到 cache_dir/target_dir。
    /// target_dir 优先从已注册的 manifest 中按 (module_id, model_id) 查
    /// `ModelDecl.target_dir`（修复早期直接用 model_id 当目录名的 bug，
    /// 因为清单中两者多数不同，如 large-v3 vs faster-whisper-large-v3）；
    /// 未注册 manifest 时回退为 model_id（保持向后兼容）。
    /// 大文件复制时通过 tracing 日志输出进度。
    pub async fn import_model(
        &self,
        module_id: &str,
        model_id: &str,
        source_path: &Path,
    ) -> Result<()> {
        let target_dir = self.resolve_target_dir(module_id, model_id);
        self.import_model_into(module_id, model_id, &target_dir, source_path)
            .await
    }

    /// 从本地路径导入模型 — 显式使用给定 manifest 解析 target_dir。
    ///
    /// model_id 不在 manifest 的模型声明中时报错（中文）。
    pub async fn import_model_with_manifest(
        &self,
        module_id: &str,
        model_id: &str,
        source_path: &Path,
        manifest: &ModuleManifest,
    ) -> Result<()> {
        let decl = manifest
            .models
            .iter()
            .find(|m| m.id == model_id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "model '{model_id}' not found in manifest of module '{module_id}', cannot import"
                )
            })?;

        // 校验门禁：声明了 sha256/sha256s 时，先校验源目录再复制落盘
        verify_model_checksum(source_path, &ModelChecksumSpec::from_decl(decl), model_id)
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;

        self.import_model_into(module_id, model_id, &decl.target_dir, source_path)
            .await
    }

    /// 在已注册的 manifest 中解析 (module_id, model_id) 对应的 target_dir；
    /// 找不到时回退为 model_id（旧行为）。
    fn resolve_target_dir(&self, module_id: &str, model_id: &str) -> String {
        for manifest in &self.manifests {
            if manifest.module.id != module_id {
                continue;
            }
            if let Some(decl) = manifest.models.iter().find(|m| m.id == model_id) {
                return decl.target_dir.clone();
            }
        }
        model_id.to_string()
    }

    /// 导入实现：复制到 cache_dir/target_dir_name 并写入元数据
    async fn import_model_into(
        &self,
        module_id: &str,
        model_id: &str,
        target_dir_name: &str,
        source_path: &Path,
    ) -> Result<()> {
        // 验证源路径
        if !source_path.is_dir() {
            anyhow::bail!(
                "source path '{}' does not exist or is not a directory",
                source_path.display()
            );
        }

        // target_dir 校验（P1）：恶意/损坏 manifest 的 "../x" 不得写出缓存根
        validate_target_dir(target_dir_name)?;

        let target_dir = self.model_dir(target_dir_name);

        info!(
            module_id = %module_id,
            model_id = %model_id,
            source = %source_path.display(),
            target = %target_dir.display(),
            "importing model from local path"
        );

        // 创建目标目录
        tokio::fs::create_dir_all(&target_dir)
            .await
            .with_context(|| format!("failed to create target dir {}", target_dir.display()))?;

        // 递归复制文件
        let stats = self.copy_dir_recursive(source_path, &target_dir).await?;

        info!(
            module_id = %module_id,
            model_id = %model_id,
            files_copied = stats.0,
            total_bytes = stats.1,
            "model import completed"
        );

        // 写入元数据
        let meta = ModelMeta {
            module_id: module_id.to_string(),
            model_id: model_id.to_string(),
            source: "local_import".to_string(),
            repo_id: String::new(),
            revision: String::new(),
            downloaded_at: chrono::Utc::now().to_rfc3339(),
            total_size_bytes: stats.1,
            qualified_id: None,
            tags: Vec::new(),
            pack_id: None,
        };
        self.write_meta(target_dir_name, &meta)?;

        Ok(())
    }

    /// 递归复制目录，返回 (文件数, 总字节数)。
    ///
    /// 安全规则（P2）：符号链接一律跳过（用 symlink_metadata 判定、不跟随，
    /// 与 [`dir_total_size`] 语义一致）；以 canonical 路径跟踪已访问目录，
    /// 检测源目录环（junction / 绑定挂载）并报错，杜绝无限递归。
    /// 顶层 src 若为符号链接则解析到真实目录（用户显式指定的可信入口）。
    async fn copy_dir_recursive(&self, src: &Path, dst: &Path) -> Result<(usize, u64)> {
        let mut visited = HashSet::new();
        let resolved = fs::canonicalize(src).unwrap_or_else(|_| src.to_path_buf());
        self.copy_dir_recursive_inner(&resolved, dst, &mut visited)
            .await
    }

    /// `copy_dir_recursive` 的实际实现（共享 visited 集合防环）
    async fn copy_dir_recursive_inner(
        &self,
        src: &Path,
        dst: &Path,
        visited: &mut HashSet<PathBuf>,
    ) -> Result<(usize, u64)> {
        let mut file_count: usize = 0;
        let mut total_bytes: u64 = 0;

        // 符号链接不跟随（与 dir_total_size 的 symlink_metadata 用法对齐）
        match fs::symlink_metadata(src) {
            Ok(m) if m.file_type().is_symlink() => {
                debug!(src = %src.display(), "skipping symlink while importing");
                return Ok((0, 0));
            }
            Ok(_) => {}
            Err(e) => anyhow::bail!("failed to stat {}: {e}", src.display()),
        }

        // canonical 路径防环：同一真实目录重复访问（junction/绑定挂载）→ 报错
        let canonical = fs::canonicalize(src).unwrap_or_else(|_| src.to_path_buf());
        if !visited.insert(canonical) {
            anyhow::bail!(
                "directory cycle detected while importing at {}",
                src.display()
            );
        }

        let mut entries = tokio::fs::read_dir(src)
            .await
            .with_context(|| format!("failed to read dir {}", src.display()))?;

        while let Some(entry) = entries.next_entry().await? {
            let src_path = entry.path();
            let file_name = entry.file_name();
            let dst_path = dst.join(&file_name);

            // symlink_metadata：不跟随符号链接
            let meta = match tokio::fs::symlink_metadata(&src_path).await {
                Ok(m) => m,
                Err(e) => {
                    warn!(
                        path = %src_path.display(),
                        error = %e,
                        "skipping unreadable entry while importing"
                    );
                    continue;
                }
            };
            if meta.file_type().is_symlink() {
                debug!(src = %src_path.display(), "skipping symlink while importing");
                continue;
            }

            if meta.file_type().is_dir() {
                tokio::fs::create_dir_all(&dst_path).await.with_context(|| {
                    format!("failed to create dir {}", dst_path.display())
                })?;
                let (sub_count, sub_bytes) =
                    Box::pin(self.copy_dir_recursive_inner(&src_path, &dst_path, visited)).await?;
                file_count += sub_count;
                total_bytes += sub_bytes;
            } else {
                let file_size = meta.len();

                debug!(
                    file = %src_path.display(),
                    size_bytes = file_size,
                    "copying file"
                );

                tokio::fs::copy(&src_path, &dst_path).await.with_context(|| {
                    format!(
                        "failed to copy {} -> {}",
                        src_path.display(),
                        dst_path.display()
                    )
                })?;

                file_count += 1;
                total_bytes += file_size;

                // 每 10 个文件输出一次进度
                if file_count.is_multiple_of(10) {
                    info!(
                        files_copied = file_count,
                        total_bytes = total_bytes,
                        "import progress"
                    );
                }
            }
        }

        Ok((file_count, total_bytes))
    }

    /// 获取指定模块所有模型的详细信息
    pub fn get_model_info(&self, _module_id: &str, manifest: &ModuleManifest) -> Vec<ModelInfo> {
        manifest
            .models
            .iter()
            .map(|model| {
                let dir = self.model_dir(&model.target_dir);
                let status = self.check_single_model_status(&model.target_dir);

                let (size_bytes, file_count) = if dir.is_dir() {
                    self.dir_stats(&dir)
                } else {
                    (0, 0)
                };

                let local_cache_path = if status == ModelStatus::Importable {
                    self.find_in_cache_paths(&model.target_dir)
                } else {
                    None
                };

                ModelInfo {
                    model_id: model.id.clone(),
                    name: model.name.clone(),
                    target_dir: model.target_dir.clone(),
                    status,
                    size_bytes,
                    file_count,
                    local_cache_path,
                    available_sources: model.available_sources(),
                }
            })
            .collect()
    }

    /// 递归统计目录大小和文件数量。
    ///
    /// 统一使用 symlink_metadata：不跟随符号链接，避免 HF 缓存布局中的
    /// symlink 造成重复计数或目录环（与 [`dir_total_size`] 语义一致，
    /// Windows/Linux 行为相同）。
    fn dir_stats(&self, dir: &Path) -> (u64, usize) {
        let mut total_size: u64 = 0;
        let mut file_count: usize = 0;

        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return (0, 0),
        };

        for entry in entries.flatten() {
            let path = entry.path();
            match fs::symlink_metadata(&path) {
                Ok(m) if m.file_type().is_dir() => {
                    let (sub_size, sub_count) = self.dir_stats(&path);
                    total_size += sub_size;
                    file_count += sub_count;
                }
                Ok(m) => {
                    total_size += m.len();
                    file_count += 1;
                }
                Err(_) => {}
            }
        }

        (total_size, file_count)
    }

    /// 获取配置的本地缓存搜索路径
    pub fn cache_paths(&self) -> &[PathBuf] {
        &self.cache_paths
    }

    // ─── 跨模块模型列表 ──────────────────────────────────────────────────

    /// 列出所有已发现模块的所有模型声明，附带当前状态
    ///
    /// 遍历每个 manifest 的 `[[models]]` 声明，检查 cache_dir 下对应目录是否存在，
    /// 填充 status 和 size_bytes。用于桌面 GUI 的统一模型列表展示。
    pub fn list_all_models(&self, manifests: &[ModuleManifest]) -> Vec<ModelView> {
        let mut views = Vec::new();

        for manifest in manifests {
            let module_id = &manifest.module.id;
            let module_name = &manifest.module.name;

            for model in &manifest.models {
                let status = self.check_single_model_status(&model.target_dir);

                // 目录存在时统计大小，否则为 None
                let dir = self.model_dir(&model.target_dir);
                let size_bytes = if dir.is_dir() {
                    Some(self.dir_stats(&dir).0)
                } else {
                    None
                };

                let source = match model.source {
                    ModelSource::Huggingface => "huggingface",
                    ModelSource::Modelscope => "modelscope",
                    ModelSource::Url => "url",
                    ModelSource::LocalImport => "local_import",
                };

                views.push(ModelView {
                    module_id: module_id.clone(),
                    module_name: module_name.clone(),
                    model_id: model.id.clone(),
                    model_name: model.name.clone(),
                    source: source.to_string(),
                    repo_id: model.repo_id.clone().unwrap_or_default(),
                    target_dir: model.target_dir.clone(),
                    status,
                    size_bytes,
                    available_sources: model.available_sources(),
                });
            }
        }

        debug!(count = views.len(), "listed all models across modules");
        views
    }
}

// ─── 下载代码安全辅助（P0/P1/P2）────────────────────────────────────────────

/// 转义 Python 单引号字符串字面量中的特殊字符（`\`、`'`、换行等）。
///
/// `gen_download_code` 中所有拼进单引号字符串的值必须经此转义，
/// 防止 repo_id/url/路径含引号或换行时注入 Python 代码。
fn escape_py_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

/// repo_id 字符集校验：仅允许 `[A-Za-z0-9/._-]`（HF/ModelScope 仓库 ID 规范）。
fn validate_repo_id(repo_id: &str) -> Result<()> {
    let ok = !repo_id.is_empty()
        && repo_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'));
    if !ok {
        anyhow::bail!("invalid repo_id '{repo_id}': only [A-Za-z0-9/._-] allowed");
    }
    Ok(())
}

/// URL 校验：协议必须为 http/https，字符集限于 URL 合法字符（含 % 编码与 ?&=）。
fn validate_url(url: &str) -> Result<()> {
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        anyhow::bail!("invalid url '{url}': only http/https protocol allowed");
    }
    let charset_ok = url.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '/' | '.' | '_' | '-' | '%' | '?' | '&' | '=' | ':' | '#' | '@' | '~' | '+'
                    | '!' | '*' | '(' | ')' | ',' | ';'
            )
    });
    if !charset_ok {
        anyhow::bail!("invalid url '{url}': contains characters outside URL charset");
    }
    Ok(())
}

/// target_dir 校验：仅允许单段安全目录名（`[A-Za-z0-9._-]`），
/// 拒绝路径分隔、`..` 穿越与空值（P1：恶意/损坏 manifest 写出缓存根）。
fn validate_target_dir(target_dir: &str) -> Result<()> {
    if target_dir.is_empty() {
        anyhow::bail!("invalid target_dir: empty");
    }
    if target_dir == "." || target_dir == ".." {
        anyhow::bail!("invalid target_dir '{target_dir}': path traversal");
    }
    if target_dir.contains('/') || target_dir.contains('\\') {
        anyhow::bail!(
            "invalid target_dir '{target_dir}': must be a single directory segment"
        );
    }
    if !target_dir
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        anyhow::bail!("invalid target_dir '{target_dir}': only [A-Za-z0-9._-] allowed");
    }
    Ok(())
}

/// 从 URL 提取并清洗单文件下载的目标文件名（P2）。
///
/// 剥离 query/fragment，尾段做安全清洗（非法字符替换为 `_`）；
/// 空/`.`/`..`/过长时回退 `model.bin`。
fn sanitize_file_name(url: &str) -> String {
    let path_part = url.split(['?', '#']).next().unwrap_or("");
    let tail = path_part.rsplit('/').next().unwrap_or("");
    let cleaned: String = tail
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_end_matches(['.', ' ']);
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." || cleaned.len() > 200 {
        "model.bin".to_string()
    } else {
        cleaned.to_string()
    }
}

/// 构建命令无法返回 Result 时（`build_download_command` pub 签名冻结），
/// 对非法输入生成"报错退出"的占位 python 脚本，绝不执行注入内容。
fn download_error_stub(message: &str) -> String {
    format!(
        "import sys; sys.stderr.write('{}'); sys.exit(1)",
        escape_py_str(message)
    )
}

/// 本地缓存目录名与 target_dir 的精确匹配（完整路径段比较，P2）：
/// 目录名 == target_dir，或 HF 缓存布局 `models--org--name` 的末段 == target_dir。
/// 杜绝 "qwen" 子串误中 "qwen3-asr" 这类误匹配。
fn model_dir_name_matches(dir_name: &str, target_dir: &str) -> bool {
    dir_name == target_dir || dir_name.ends_with(&format!("--{target_dir}"))
}

/// 清理下载产生的已知临时文件（`_download.tar.gz`）。
///
/// 取消/超时/失败路径调用；python 侧 try/finally 是主清理，
/// 这里是 kill 无法触发 finally 时的兜底（P1）。
fn cleanup_download_temp_files(dir: &Path) {
    let tmp = dir.join(DOWNLOAD_TEMP_FILE);
    if let Err(e) = fs::remove_file(&tmp) {
        if e.kind() != std::io::ErrorKind::NotFound {
            debug!(
                path = %tmp.display(),
                error = %e,
                "failed to remove download temp file"
            );
        }
    }
}

/// 目录是否存在直接子条目（free 版本，供下载清理路径复用）
fn dir_has_entries(dir: &Path) -> bool {
    match fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_some(),
        Err(_) => false,
    }
}

// ─── 模型文件 sha256 校验（下载/导入完成门禁）───────────────────────────────

/// 模型文件校验声明（从 [`ModelDecl`] 的 `sha256` / `sha256s` 提取）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ModelChecksumSpec {
    /// 单文件模型主文件期望摘要（url 源常见）
    pub single: Option<String>,
    /// 多文件模型：相对路径（正斜杠）→ sha256
    pub files: std::collections::BTreeMap<String, String>,
}

impl ModelChecksumSpec {
    fn from_decl(model: &crate::module::manifest::ModelDecl) -> Self {
        Self {
            single: model.sha256.clone(),
            files: model.sha256s.clone(),
        }
    }

    fn is_empty(&self) -> bool {
        self.single.is_none() && self.files.is_empty()
    }
}

/// 单文件 sha256（小写 hex）
fn sha256_file_hex(path: &Path) -> std::io::Result<String> {
    use sha2::Digest;
    let mut file = fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// 递归收集目录下文件：相对路径（正斜杠）→ 绝对路径
fn collect_files_relative(root: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files_relative(root, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel, path);
        }
    }
    Ok(())
}

/// 校验模型目录文件与声明的 sha256 一致（均未声明时直接通过）。
///
/// - `single`：目录内载荷文件（排除 `.ep_meta.json`）恰有一个且摘要一致；
/// - `files`：声明的每个相对路径文件都存在且摘要一致（额外文件不失败，
///   兼容 HF 仓库附带文件）。
///
/// 校验失败返回中文错误（用户可见），并附带期望/实际摘要便于排查。
pub(crate) fn verify_model_checksum(dir: &Path, spec: &ModelChecksumSpec, model_id: &str) -> Result<()> {
    if spec.is_empty() {
        return Ok(());
    }
    let mut files = std::collections::BTreeMap::new();
    collect_files_relative(dir, dir, &mut files)
        .map_err(|e| anyhow::anyhow!("模型 '{model_id}' 校验失败：无法遍历目录 {}: {e}", dir.display()))?;
    files.remove(META_FILE_NAME);

    let hash_of = |path: &Path| -> Result<String> {
        sha256_file_hex(path).map_err(|e| {
            anyhow::anyhow!("模型 '{model_id}' 校验失败：无法读取文件 {}: {e}", path.display())
        })
    };

    if let Some(expected) = &spec.single {
        if files.len() != 1 {
            anyhow::bail!(
                "模型 '{model_id}' 校验失败：声明了单文件 sha256，但目录 {} 内有 {} 个载荷文件（期望恰 1 个）",
                dir.display(), files.len()
            );
        }
        let (rel, path) = files.iter().next().unwrap();
        let actual = hash_of(path)?;
        if !actual.eq_ignore_ascii_case(expected) {
            anyhow::bail!(
                "模型 '{model_id}' 校验失败：文件 '{rel}' sha256 不匹配（期望 {expected}，实际 {actual}）"
            );
        }
    }

    for (rel, expected) in &spec.files {
        let path = files.get(rel).ok_or_else(|| {
            anyhow::anyhow!("模型 '{model_id}' 校验失败：声明的文件 '{rel}' 不存在于 {}", dir.display())
        })?;
        let actual = hash_of(path)?;
        if !actual.eq_ignore_ascii_case(expected) {
            anyhow::bail!(
                "模型 '{model_id}' 校验失败：文件 '{rel}' sha256 不匹配（期望 {expected}，实际 {actual}）"
            );
        }
    }
    Ok(())
}

/// 取消/失败/超时后清理残缺下载产物。
///
/// 下载启动时目标目录为空（首次下载）→ 目录内任何文件都是本次子进程落盘的
/// 残缺产物，全部移除（保留目录本身，状态回到 Incomplete）——修复"取消下载
/// 留下半个模型文件却被状态检查误判 ready，且阻塞后续导入"的问题。
/// 目录在启动前已有内容（多文件断点续传场景）→ 仅清理已知临时文件，不动
/// 已有文件（保留续传语义）。
fn cleanup_partial_download(dir: &Path, dir_was_empty_at_start: bool) {
    if dir_was_empty_at_start {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let result = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            if let Err(e) = result {
                debug!(
                    path = %path.display(),
                    error = %e,
                    "failed to remove partial download artifact"
                );
            }
        }
        return;
    }
    cleanup_download_temp_files(dir);
}

// ─── active_models 单槽位（§5.2）────────────────────────────────────────────

/// 解析模块当前激活的模型变体 id — 版本单槽位语义（§5.2，冻结）。
///
/// 每模块同一时间至多一个激活变体，解析顺序（三级回退）：
/// 1. `config.active_models[module_id]`（app.toml `[active_models]` 用户显式选择）；
///    配置的 id 未命中 manifest 任何变体（陈旧配置）时记 warn 并继续回退；
/// 2. manifest 中 `default = true` 的变体；
/// 3. manifest 中首个变体。
///
/// 模块未声明任何模型变体（如 native 模块）时返回 `None`。
///
/// daemon 启动模块（B5）与统一页/桌面端变体展示（C1/C5）应统一消费本
/// helper，不再"死板取 default"。
pub fn active_model_for<'a>(config: &AppConfig, manifest: &'a ModuleManifest) -> Option<&'a str> {
    let module_id = &manifest.module.id;

    // 1. 显式配置（单槽位用户选择）；返回 manifest 侧声明的 id
    //    （生命周期锚定 manifest，而非 config）
    if let Some(model_id) = config.active_models.get(module_id) {
        if let Some(decl) = manifest.models.iter().find(|m| &m.id == model_id) {
            return Some(decl.id.as_str());
        }
        warn!(
            module_id = %module_id,
            model_id = %model_id,
            "active_models entry does not match any variant declared by the manifest, falling back to default variant"
        );
    }

    // 2. manifest 声明的 default=true 变体
    if let Some(model) = manifest.models.iter().find(|m| m.default) {
        return Some(model.id.as_str());
    }

    // 3. 回退首个变体
    manifest.models.first().map(|m| m.id.as_str())
}

// ─── DownloadHandle 与下载监督任务 ──────────────────────────────────────────

/// 下载并发闸（P2）：按 `max_concurrent_downloads` 懒构建的进程级信号量。
///
/// 阻塞式下载入口 [`ModelManager::execute_download_with_source`] 在 spawn
/// 前 `acquire_owned` 并持至函数结束；带进度入口 `execute_download_with_progress`
/// 不加闸——daemon 层已有同款闸门（P2-1，见 ep-daemon models.rs `download_gate`），
/// 重复加闸存在"释放顺序竞态"导致排队下载被误杀的风险。
/// `max_concurrent_downloads` 运行时变更 → 按新值懒重建；0 按 1 处理。
static MODEL_DOWNLOAD_GATE: std::sync::Mutex<Option<(u32, Arc<Semaphore>)>> =
    std::sync::Mutex::new(None);

/// 获取（懒构建/重建）下载并发闸信号量
fn model_download_gate(max_concurrent: u32) -> Arc<Semaphore> {
    let max = max_concurrent.max(1);
    let mut guard = MODEL_DOWNLOAD_GATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((current, sem)) = guard.as_ref() {
        if *current == max {
            return sem.clone();
        }
    }
    let sem = Arc::new(Semaphore::new(max as usize));
    *guard = Some((max, sem.clone()));
    sem
}

/// 带进度下载的句柄：进度订阅 + 完成等待 + 取消。
///
/// 由 `ModelManager::execute_download_with_progress` 返回。
pub struct DownloadHandle {
    module_id: String,
    model_id: String,
    progress_tx: broadcast::Sender<DownloadProgress>,
    done_rx: Option<oneshot::Receiver<Result<u64, String>>>,
    cancel_tx: Option<oneshot::Sender<()>>,
}

impl DownloadHandle {
    /// 所属模块 ID
    pub fn module_id(&self) -> &str {
        &self.module_id
    }

    /// 模型 ID
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// 订阅进度事件流（broadcast，可多次调用获得多个独立接收端）
    pub fn subscribe_progress(&self) -> broadcast::Receiver<DownloadProgress> {
        self.progress_tx.subscribe()
    }

    /// 取消下载（幂等；下载已结束时为 no-op）
    pub fn cancel(&mut self) {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(());
        }
    }

    /// 等待下载结束。Ok = 目录总字节数；Err = 中文错误摘要（含取消）。
    pub async fn wait(mut self) -> Result<u64, String> {
        match self.done_rx.take() {
            Some(rx) => rx
                .await
                .unwrap_or_else(|_| Err("download supervisor task exited abnormally".to_string())),
            None => Err("this download handle has already been awaited".to_string()),
        }
    }
}

/// 启动下载监督任务，返回句柄。
///
/// 选择 broadcast channel 的原因：进度事件可能有多个消费方（桌面 GUI 与
/// WebUI 事件流同时订阅），且 broadcast 的 `send` 在无接收端时仅返回
/// Err——忽略该错误即可保证"进度发送失败不影响下载本身"。
/// 完成信号用 oneshot：只通知一次、语义清晰。
#[allow(clippy::too_many_arguments)]
fn spawn_tracked_download(
    child: Child,
    poll_dir: PathBuf,
    size_estimate_mb: Option<u32>,
    module_id: String,
    model_id: String,
    meta: Option<ModelMeta>,
    dir_was_empty_at_start: bool,
    checksum: ModelChecksumSpec,
) -> DownloadHandle {
    let (progress_tx, _rx) = broadcast::channel::<DownloadProgress>(64);
    let (done_tx, done_rx) = oneshot::channel::<Result<u64, String>>();
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

    let tx = progress_tx.clone();
    let mid = module_id.clone();
    let mname = model_id.clone();
    tokio::spawn(supervise_download(
        child,
        poll_dir,
        size_estimate_mb,
        mid,
        mname,
        tx,
        cancel_rx,
        done_tx,
        meta,
        dir_was_empty_at_start,
        checksum,
    ));

    DownloadHandle {
        module_id,
        model_id,
        progress_tx,
        done_rx: Some(done_rx),
        cancel_tx: Some(cancel_tx),
    }
}

/// stderr 尾部环形缓冲容量（行数）
const STDERR_TAIL_LINES: usize = 50;
/// 错误摘要最大字符数
const ERROR_SUMMARY_MAX_CHARS: usize = 800;

#[allow(clippy::too_many_arguments)]
async fn supervise_download(
    mut child: Child,
    poll_dir: PathBuf,
    size_estimate_mb: Option<u32>,
    module_id: String,
    model_id: String,
    progress_tx: broadcast::Sender<DownloadProgress>,
    mut cancel_rx: oneshot::Receiver<()>,
    done_tx: oneshot::Sender<Result<u64, String>>,
    mut meta: Option<ModelMeta>,
    dir_was_empty_at_start: bool,
    checksum: ModelChecksumSpec,
) {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, BufReader};

    // 抽干 stdout/stderr，避免管道写满阻塞子进程；保留 stderr 尾部做错误摘要
    let stderr_tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(_line)) = lines.next_line().await {}
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let tail = Arc::clone(&stderr_tail);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut q = tail.lock().unwrap_or_else(|e| e.into_inner());
                q.push_back(line);
                while q.len() > STDERR_TAIL_LINES {
                    q.pop_front();
                }
            }
        });
    }

    let emit = |percent: f32, bytes: u64, state: DownloadState| {
        // 接收端全部丢弃时 send 返回 Err —— 忽略，不影响下载本身
        let _ = progress_tx.send(DownloadProgress {
            module_id: module_id.clone(),
            model_id: model_id.clone(),
            percent,
            bytes,
            state,
        });
    };

    let mut interval = tokio::time::interval(Duration::from_secs(2));
    // 总时长上限（P1）：超时 kill 下载子进程并清理已知临时文件
    let deadline = tokio::time::sleep(Duration::from_secs(DOWNLOAD_MAX_DURATION_SECS));
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = &mut deadline => {
                info!(
                    model_id = %model_id,
                    duration_secs = DOWNLOAD_MAX_DURATION_SECS,
                    "download exceeded max duration, killing child"
                );
                let _ = child.start_kill();
                let _ = child.wait().await; // 回收子进程
                // P1：kill 无法触发 python try/finally，这里兜底清理残缺产物
                cleanup_partial_download(&poll_dir, dir_was_empty_at_start);
                let msg = format!(
                    "download exceeded the {} minute time limit and was aborted",
                    DOWNLOAD_MAX_DURATION_SECS / 60
                );
                let bytes = dir_total_size(&poll_dir);
                emit(compute_percent(bytes, size_estimate_mb), bytes, DownloadState::Failed(msg.clone()));
                let _ = done_tx.send(Err(msg));
                return;
            }
            _ = interval.tick() => {
                let bytes = dir_total_size(&poll_dir);
                emit(compute_percent(bytes, size_estimate_mb), bytes, DownloadState::Downloading);
            }
            _ = &mut cancel_rx => {
                info!(model_id = %model_id, "cancelling tracked model download");
                let _ = child.start_kill();
                let _ = child.wait().await; // 回收子进程
                // P1：kill 无法触发 python try/finally，这里兜底清理残缺产物
                cleanup_partial_download(&poll_dir, dir_was_empty_at_start);
                let bytes = dir_total_size(&poll_dir);
                emit(compute_percent(bytes, size_estimate_mb), bytes, DownloadState::Cancelled);
                let _ = done_tx.send(Err("download cancelled".to_string()));
                return;
            }
            wait_res = child.wait() => {
                match wait_res {
                    Ok(status) if status.success() => {
                        // 校验门禁：声明了 sha256/sha256s 时，校验通过才算完成；
                        // 失败按残缺产物清理，落 Failed 终态
                        if let Err(e) =
                            verify_model_checksum(&poll_dir, &checksum, &model_id)
                        {
                            cleanup_partial_download(&poll_dir, dir_was_empty_at_start);
                            let msg = format!("{e:#}");
                            emit(0.0, 0, DownloadState::Failed(msg.clone()));
                            let _ = done_tx.send(Err(msg));
                            return;
                        }
                        let bytes = dir_total_size(&poll_dir);
                        emit(100.0, bytes, DownloadState::Completed);
                        if let Some(meta) = meta.take() {
                            let mut meta = meta;
                            meta.downloaded_at = chrono::Utc::now().to_rfc3339();
                            meta.total_size_bytes = bytes;
                            if let Err(e) = write_meta_to_dir(&poll_dir, &meta) {
                                warn!(
                                    model_id = %model_id,
                                    error = %e,
                                    "failed to write model meta after download (non-fatal)"
                                );
                            }
                        }
                        let _ = done_tx.send(Ok(bytes));
                        return;
                    }
                    Ok(status) => {
                        // P1：失败兜底清理（python try/finally 通常已完成清理）
                        cleanup_partial_download(&poll_dir, dir_was_empty_at_start);
                        let summary = {
                            let q = stderr_tail.lock().unwrap_or_else(|e| e.into_inner());
                            let skip = q.len().saturating_sub(10);
                            q.iter().skip(skip).cloned().collect::<Vec<_>>().join("\n")
                        };
                        let summary = truncate_chars(&summary, ERROR_SUMMARY_MAX_CHARS);
                        let msg = if summary.trim().is_empty() {
                            format!("download failed (exit code {:?})", status.code())
                        } else {
                            format!("download failed (exit code {:?}): {}", status.code(), summary)
                        };
                        let bytes = dir_total_size(&poll_dir);
                        emit(
                            compute_percent(bytes, size_estimate_mb),
                            bytes,
                            DownloadState::Failed(msg.clone()),
                        );
                        let _ = done_tx.send(Err(msg));
                        return;
                    }
                    Err(e) => {
                        cleanup_partial_download(&poll_dir, dir_was_empty_at_start);
                        let msg = format!("failed to wait for download process: {e}");
                        emit(0.0, 0, DownloadState::Failed(msg.clone()));
                        let _ = done_tx.send(Err(msg));
                        return;
                    }
                }
            }
        }
    }
}

/// 计算进度百分比：`min(99.0, bytes / size_estimate_mb * 100)`；
/// 无估算（None 或 0）时恒为 0.0（只报 bytes）。
fn compute_percent(bytes: u64, size_estimate_mb: Option<u32>) -> f32 {
    match size_estimate_mb {
        Some(mb) if mb > 0 => {
            let estimate_bytes = mb as f64 * 1024.0 * 1024.0;
            ((bytes as f64 / estimate_bytes) * 100.0).clamp(0.0, 99.0) as f32
        }
        _ => 0.0,
    }
}

/// 截断字符串到指定字符数（超出时追加省略号）
fn truncate_chars(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}…")
    }
}

/// 各来源未声明 revision 时的默认分支
fn default_revision_for(source: ModelSource) -> &'static str {
    match source {
        ModelSource::Huggingface => "main",
        ModelSource::Modelscope => "master",
        ModelSource::Url => "",
        ModelSource::LocalImport => "",
    }
}

// ─── HF 缓存清理 ─────────────────────────────────────────────────────────────

/// 模型目录内可能出现的 HF/ModelScope 缓存布局目录名
pub const HF_CACHE_DIR_NAMES: &[&str] = &["blobs", "snapshots", "refs", ".cache"];

/// 安全清理模型目录内的 HF 缓存布局重复副本，返回回收的字节数。
///
/// `huggingface_hub` / `modelscope` 的部分版本在 `local_dir` 模式下会留下
/// `blobs/ snapshots/ refs/ .cache/` 缓存布局，与顶层真实文件并存，
/// 造成数 GB 的重复占用。
///
/// 安全规则：
/// 1. 顶层存在指向某缓存目录的 symlink 时（顶层文件依赖缓存内容），
///    整个缓存目录跳过不删；
/// 2. 仅当顶层存在同名真实文件（非 symlink）且大小一致时，
///    才删除缓存目录内的对应副本；其余文件一律保留。
///
/// 接线点：**B1（整合包导入）在 bundle 模式模型落位后调用**（§4.4）。
/// pub 签名保持稳定以便接线；Windows/Linux 双平台可用
/// （symlink 前缀判定在 Windows 下走大小写不敏感分支，见 [`path_under`]）。
pub fn cleanup_hf_cache(model_dir: &Path) -> Result<u64> {
    if !model_dir.is_dir() {
        anyhow::bail!("model directory does not exist: {}", model_dir.display());
    }

    let mut reclaimed = 0u64;
    for name in HF_CACHE_DIR_NAMES {
        let cache_dir = model_dir.join(name);
        if !cache_dir.is_dir() {
            continue;
        }
        if top_level_symlink_points_into(model_dir, &cache_dir) {
            info!(
                cache_dir = %cache_dir.display(),
                "top level has symlinks pointing into cache dir, skipping cleanup"
            );
            continue;
        }
        reclaimed += clean_duplicate_cache_files(model_dir, &cache_dir)?;
        prune_empty_dirs(&cache_dir)?;
    }

    if reclaimed > 0 {
        info!(
            model_dir = %model_dir.display(),
            reclaimed_bytes = reclaimed,
            "HF cache cleanup completed"
        );
    }
    Ok(reclaimed)
}

/// 检查 model_dir 顶层是否存在指向 cache_dir 内部的 symlink
fn top_level_symlink_points_into(model_dir: &Path, cache_dir: &Path) -> bool {
    let cache_canonical = fs::canonicalize(cache_dir).unwrap_or_else(|_| cache_dir.to_path_buf());
    let entries = match fs::read_dir(model_dir) {
        Ok(e) => e,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.file_type().is_symlink() {
            continue;
        }
        // 优先 canonicalize（解析到最终目标）；断链时退化为词法拼接
        let resolved = match fs::canonicalize(&path) {
            Ok(p) => p,
            Err(_) => match fs::read_link(&path) {
                Ok(t) if t.is_absolute() => t,
                Ok(t) => model_dir.join(t),
                Err(_) => continue,
            },
        };
        if path_under(&resolved, &cache_canonical) {
            return true;
        }
    }
    false
}

/// 判断 `path` 是否位于 `base` 之下（按路径分量做前缀比较）。
///
/// Windows 文件系统大小写不敏感，且 `fs::canonicalize` 不会归一大小写，
/// 逐分量严格比较可能漏判（如 `C:\Models` vs `c:\models`），
/// 故 Windows 走忽略 ASCII 大小写的比较分支；Unix 保持严格 `starts_with`。
fn path_under(path: &Path, base: &Path) -> bool {
    if cfg!(windows) {
        let mut path_comps = path.components();
        for base_comp in base.components() {
            match path_comps.next() {
                Some(c) if c.as_os_str().eq_ignore_ascii_case(base_comp.as_os_str()) => {}
                _ => return false,
            }
        }
        true
    } else {
        path.starts_with(base)
    }
}

/// 递归删除缓存目录内"顶层存在同名同大小真实文件"的副本，返回回收字节数
fn clean_duplicate_cache_files(model_dir: &Path, dir: &Path) -> Result<u64> {
    let mut reclaimed = 0u64;
    let entries = fs::read_dir(dir)
        .with_context(|| format!("failed to read cache dir {}", dir.display()))?;

    for entry in entries.flatten() {
        let path = entry.path();
        let self_meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };

        if self_meta.file_type().is_dir() {
            reclaimed += clean_duplicate_cache_files(model_dir, &path)?;
            continue;
        }

        // 顶层同名条目必须存在且是真实文件（非 symlink）
        let top_path = model_dir.join(entry.file_name());
        let top_meta = match fs::symlink_metadata(&top_path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !top_meta.is_file() {
            continue;
        }

        // 大小一致才可删（缓存条目为 symlink 时按目标文件大小比较）
        let cache_size = match fs::metadata(&path) {
            Ok(m) => m.len(),
            Err(_) => continue,
        };
        if cache_size != top_meta.len() {
            continue;
        }

        // 回收字节数按条目自身大小计（symlink 只计链接本身）
        let own_size = self_meta.len();
        if let Err(e) = fs::remove_file(&path) {
            warn!(path = %path.display(), error = %e, "failed to remove duplicate cache copy");
            continue;
        }
        reclaimed += own_size;
        debug!(path = %path.display(), bytes = own_size, "removed duplicate cache copy");
    }

    Ok(reclaimed)
}

/// 自底向上删除空目录；返回该目录本身是否为空并被删除
fn prune_empty_dirs(dir: &Path) -> Result<bool> {
    let mut empty = true;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            match fs::symlink_metadata(&path) {
                Ok(m) if m.file_type().is_dir() => {
                    if !prune_empty_dirs(&path)? {
                        empty = false;
                    }
                }
                Ok(_) => empty = false,
                Err(_) => empty = false,
            }
        }
    }
    if empty {
        match fs::remove_dir(dir) {
            Ok(()) => Ok(true),
            Err(_) => Ok(false),
        }
    } else {
        Ok(false)
    }
}

// ─── HTTP 辅助（更新检查）────────────────────────────────────────────────────

/// 构建带代理配置的 HTTP 客户端。
///
/// 配置了代理时显式使用；未配置任何代理时通过 `no_proxy()` 禁用
/// reqwest 对环境变量的隐式探测——网络出口统一由 `NetworkConfig` 决定。
fn build_proxied_http_client(
    network: &NetworkConfig,
    timeout: Duration,
) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().timeout(timeout);
    let mut has_proxy = false;

    if !network.https_proxy.is_empty() {
        let proxy = reqwest::Proxy::https(&network.https_proxy)
            .with_context(|| format!("invalid HTTPS proxy address: {}", network.https_proxy))?;
        builder = builder.proxy(proxy);
        has_proxy = true;
    }
    if !network.http_proxy.is_empty() {
        let proxy = reqwest::Proxy::http(&network.http_proxy)
            .with_context(|| format!("invalid HTTP proxy address: {}", network.http_proxy))?;
        builder = builder.proxy(proxy);
        has_proxy = true;
    }
    if !has_proxy {
        builder = builder.no_proxy();
    }

    builder.build().context("failed to build HTTP client")
}

/// 查询 HuggingFace 仓库最后修改时间：GET {endpoint}/api/models/{repo_id}
async fn fetch_hf_modified(
    client: &reqwest::Client,
    endpoint: &str,
    repo_id: &str,
) -> Result<(chrono::DateTime<chrono::Utc>, String)> {
    let url = format!("{endpoint}/api/models/{repo_id}");
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("HuggingFace API request failed ({url})"))?;

    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HuggingFace API returned non-success status code {status}");
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse HuggingFace API response")?;
    let last_modified = body
        .get("lastModified")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("HuggingFace API response missing lastModified field"))?;

    let dt = chrono::DateTime::parse_from_rfc3339(last_modified).with_context(|| {
        format!("failed to parse lastModified time ({last_modified})")
    })?;
    Ok((dt.with_timezone(&chrono::Utc), last_modified.to_string()))
}

/// 查询 ModelScope 仓库最后修改时间：GET {endpoint}/api/v1/models/{repo_id}
///
/// `endpoint` 缺省官方站 `https://modelscope.cn`（见 [`ModelManager::with_modelscope_endpoint`]）。
///
/// 主字段 `Data.LastUpdatedTime`（Unix 秒），回退 `Data.GmtModified`。
/// ModelScope 对不存在的仓库可能返回 HTTP 200 + Success=false 的错误包装，
/// 需要检查响应体。
async fn fetch_modelscope_modified(
    client: &reqwest::Client,
    endpoint: &str,
    repo_id: &str,
) -> Result<(chrono::DateTime<chrono::Utc>, String)> {
    let url = format!("{endpoint}/api/v1/models/{repo_id}");
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("ModelScope API request failed ({url})"))?;

    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse ModelScope API response")?;

    if !status.is_success() || body.get("Success").and_then(|v| v.as_bool()) == Some(false) {
        let message = body
            .get("Message")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("ModelScope API query failed (status code {status}): {message}");
    }

    let data = body
        .get("Data")
        .ok_or_else(|| anyhow::anyhow!("ModelScope API response missing Data field"))?;

    // 主字段：LastUpdatedTime（Unix 秒；哨兵值 <= 0 视为无效）
    if let Some(secs) = data.get("LastUpdatedTime").and_then(|v| v.as_i64()) {
        if secs > 0 {
            if let Some(dt) = chrono::DateTime::from_timestamp(secs, 0) {
                return Ok((dt, dt.to_rfc3339()));
            }
        }
    }

    // 回退字段：GmtModified（RFC 3339 字符串）
    if let Some(s) = data.get("GmtModified").and_then(|v| v.as_str()) {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
            return Ok((dt.with_timezone(&chrono::Utc), s.to_string()));
        }
    }

    anyhow::bail!("no usable modification time field in ModelScope API response")
}

/// 写入 `.ep_meta.json` 到指定模型目录（自动创建目录）
fn write_meta_to_dir(dir: &Path, meta: &ModelMeta) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("failed to create model dir {}", dir.display()))?;
    let meta_path = dir.join(META_FILE_NAME);
    let content = serde_json::to_string_pretty(meta).context("failed to serialize model meta")?;
    fs::write(&meta_path, content)
        .with_context(|| format!("failed to write {}", meta_path.display()))?;
    debug!(path = %meta_path.display(), "model meta written");
    Ok(())
}

/// 递归统计目录总大小（字节）。
///
/// 使用 symlink_metadata：不跟随符号链接，避免 HF 缓存布局中的
/// symlink 造成重复计数或目录环。
pub fn dir_total_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let entries = match fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        match fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_dir() => total += dir_total_size(&p),
            Ok(m) => total += m.len(),
            Err(_) => {}
        }
    }
    total
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建测试用 ModelsConfig
    fn test_config(cache_dir: &str) -> ModelsConfig {
        ModelsConfig {
            cache_dir: cache_dir.to_string(),
            hf_endpoint: String::new(),
            default_source: "huggingface".to_string(),
            max_concurrent_downloads: 2,
            cache_paths: Vec::new(),
        }
    }

    /// 创建测试用 ModelDecl (HuggingFace)
    fn test_hf_model() -> ModelDecl {
        ModelDecl {
            id: "large-v3".to_string(),
            name: "Whisper Large V3".to_string(),
            source: ModelSource::Huggingface,
            repo_id: Some("Systran/faster-whisper-large-v3".to_string()),
            url: None,
            target_dir: "faster-whisper-large-v3".to_string(),
            revision: Some("main".to_string()),
            size_estimate_mb: Some(3100),
            qualified_id: None,
            vram_estimate_mb: None,
            default: true,
            mirrors: vec![],
            sha256: None,
            sha256s: Default::default(),
        }
    }

    /// 创建测试用 ModelDecl (ModelScope)
    fn test_ms_model() -> ModelDecl {
        ModelDecl {
            id: "qwen3-asr".to_string(),
            name: "Qwen3 ASR".to_string(),
            source: ModelSource::Modelscope,
            repo_id: Some("Qwen/Qwen3-ASR".to_string()),
            url: None,
            target_dir: "qwen3-asr".to_string(),
            revision: None,
            size_estimate_mb: Some(2000),
            qualified_id: None,
            vram_estimate_mb: None,
            default: false,
            mirrors: vec![],
            sha256: None,
            sha256s: Default::default(),
        }
    }

    /// 创建带 mirror 的测试用 ModelDecl（主源 HF，镜像 ModelScope）
    fn test_mirrored_model() -> ModelDecl {
        use crate::module::manifest::ModelMirror;
        ModelDecl {
            id: "large-v3".to_string(),
            name: "Whisper Large V3".to_string(),
            source: ModelSource::Huggingface,
            repo_id: Some("Systran/faster-whisper-large-v3".to_string()),
            url: None,
            target_dir: "faster-whisper-large-v3".to_string(),
            revision: Some("main".to_string()),
            size_estimate_mb: Some(3100),
            qualified_id: None,
            vram_estimate_mb: None,
            default: true,
            mirrors: vec![ModelMirror {
                source: ModelSource::Modelscope,
                repo_id: "pengzhendong/faster-whisper-large-v3".to_string(),
                revision: Some("master".to_string()),
            }],
            sha256: None,
            sha256s: Default::default(),
        }
    }

    fn test_meta() -> ModelMeta {
        ModelMeta {
            module_id: "faster-whisper".to_string(),
            model_id: "large-v3".to_string(),
            source: "huggingface".to_string(),
            repo_id: "Systran/faster-whisper-large-v3".to_string(),
            revision: "main".to_string(),
            downloaded_at: "2026-07-20T10:30:00Z".to_string(),
            total_size_bytes: 3_094_850_000,
            qualified_id: None,
            tags: Vec::new(),
            pack_id: None,
        }
    }

    /// 创建临时目录用于测试，返回路径
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ep_model_test_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    // ── model_dir 路径拼接 ──────────────────────────────────────────────

    #[test]
    fn test_model_dir_relative() {
        let root = Path::new("G:/EntryPoint");
        let config = test_config("models");
        let mgr = ModelManager::new(&config, root);

        assert_eq!(
            mgr.model_dir("faster-whisper-large-v3"),
            PathBuf::from("G:/EntryPoint/models/faster-whisper-large-v3")
        );
    }

    #[test]
    fn test_model_dir_absolute() {
        if cfg!(windows) {
            let root = Path::new("G:/EntryPoint");
            let config = test_config("D:/AI_Models");
            let mgr = ModelManager::new(&config, root);
            assert_eq!(
                mgr.model_dir("some-model"),
                PathBuf::from("D:/AI_Models/some-model")
            );
        } else {
            let root = Path::new("/opt/entrypoint");
            let config = test_config("/opt/models");
            let mgr = ModelManager::new(&config, root);
            assert_eq!(
                mgr.model_dir("some-model"),
                PathBuf::from("/opt/models/some-model")
            );
        }
    }

    // ── is_model_present ────────────────────────────────────────────────

    #[test]
    fn test_is_model_present_nonexistent() {
        let dir = temp_dir("present_no");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        assert!(!mgr.is_model_present("no-such-model"));
        cleanup(&dir);
    }

    #[test]
    fn test_is_model_present_empty_dir() {
        let dir = temp_dir("present_empty");
        let model_path = dir.join("empty-model");
        fs::create_dir_all(&model_path).unwrap();

        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        // 空目录 → false
        assert!(!mgr.is_model_present("empty-model"));
        cleanup(&dir);
    }

    #[test]
    fn test_is_model_present_with_files() {
        let dir = temp_dir("present_yes");
        let model_path = dir.join("my-model");
        fs::create_dir_all(&model_path).unwrap();
        fs::write(model_path.join("model.bin"), b"fake").unwrap();

        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        assert!(mgr.is_model_present("my-model"));
        cleanup(&dir);
    }

    // ── build_download_command ──────────────────────────────────────────

    #[test]
    fn test_build_download_command_huggingface() {
        let root = Path::new("G:/EntryPoint");
        let config = test_config("models");
        let mgr = ModelManager::new(&config, root);
        let model = test_hf_model();
        let venv_python = Path::new("G:/EntryPoint/runtime/venvs/faster-whisper/python.exe");

        let (program, args) = mgr.build_download_command(&model, venv_python);

        assert_eq!(program, "G:/EntryPoint/runtime/venvs/faster-whisper/python.exe");
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "-c");

        let code = &args[1];
        assert!(code.contains("from huggingface_hub import snapshot_download"), "code: {code}");
        assert!(code.contains("Systran/faster-whisper-large-v3"), "code: {code}");
        assert!(code.contains("faster-whisper-large-v3"), "code: {code}");
        assert!(code.contains("revision='main'"), "code: {code}");
        // 无镜像时不应设置 HF_ENDPOINT
        assert!(!code.contains("HF_ENDPOINT"), "code: {code}");
    }

    #[test]
    fn test_build_download_command_huggingface_with_mirror() {
        let root = Path::new("G:/EntryPoint");
        let mut config = test_config("models");
        config.hf_endpoint = "https://hf-mirror.com".to_string();
        let mgr = ModelManager::new(&config, root);
        let model = test_hf_model();
        let venv_python = Path::new("python");

        let (_program, args) = mgr.build_download_command(&model, venv_python);
        let code = &args[1];

        assert!(code.contains("HF_ENDPOINT"), "should set HF_ENDPOINT, code: {code}");
        assert!(code.contains("hf-mirror.com"), "code: {code}");
    }

    #[test]
    fn test_build_download_command_modelscope() {
        let root = Path::new("G:/EntryPoint");
        let config = test_config("models");
        let mgr = ModelManager::new(&config, root);
        let model = test_ms_model();
        let venv_python = Path::new("python");

        let (_program, args) = mgr.build_download_command(&model, venv_python);
        let code = &args[1];

        assert!(code.contains("from modelscope import snapshot_download"), "code: {code}");
        assert!(code.contains("Qwen/Qwen3-ASR"), "code: {code}");
        // ModelScope 默认 revision 为 master
        assert!(code.contains("revision='master'"), "code: {code}");
    }

    // ── gen_download_code 快照（GAP-1 G3 补测）──────────────────────────────
    // 快照关键行而非全文，避免对脚本措辞调整过度敏感。

    /// URL 来源测试夹具
    fn test_url_model(url: &str) -> ModelDecl {
        ModelDecl {
            id: "url-model".to_string(),
            name: "URL model".to_string(),
            source: ModelSource::Url,
            repo_id: None,
            url: Some(url.to_string()),
            target_dir: "url-model".to_string(),
            revision: None,
            size_estimate_mb: None,
            qualified_id: None,
            vram_estimate_mb: None,
            default: false,
            mirrors: vec![],
            sha256: None,
            sha256s: Default::default(),
        }
    }

    /// url="auto"：模块自管下载 → 空操作脚本（建目录 + 跳过日志），不发起下载
    #[test]
    fn test_gen_download_code_url_auto_is_noop() {
        let mgr = ModelManager::new(&test_config("models"), Path::new("G:/EntryPoint"));
        let (_program, args) =
            mgr.build_download_command(&test_url_model("auto"), Path::new("python"));
        let code = &args[1];

        assert!(code.contains("os.makedirs("), "code: {code}");
        assert!(
            code.contains("auto-download model: skipped (managed by module)"),
            "code: {code}"
        );
        assert!(!code.contains("urlretrieve"), "auto must not download: {code}");
        assert!(code.contains("url-model"), "target dir must appear: {code}");
    }

    /// .tar.gz / .tgz URL → 下载 + 安全解压 + 清理临时包
    #[test]
    fn test_gen_download_code_url_tar_gz_extract() {
        let mgr = ModelManager::new(&test_config("models"), Path::new("G:/EntryPoint"));
        let url = "https://example.com/pack/model-v1.tar.gz";
        let (_program, args) = mgr.build_download_command(&test_url_model(url), Path::new("python"));
        let code = &args[1];

        assert!(code.contains("import urllib.request, tarfile"), "code: {code}");
        assert!(code.contains(&format!("urlretrieve('{url}'")), "code: {code}");
        // 逐成员安全解压（P1 zip-slip）：不再使用 extractall
        assert!(!code.contains("extractall("), "must not use extractall: {code}");
        assert!(code.contains("os.path.commonpath"), "must guard path traversal: {code}");
        assert!(code.contains("m.issym() or m.islnk()"), "must skip symlinks: {code}");
        assert!(code.contains("unsafe tar entry"), "must reject unsafe entries: {code}");
        assert!(code.contains("extract limits exceeded"), "must cap size/entries: {code}");
        assert!(
            code.contains(&format!("> {}:", EXTRACT_MAX_BYTES)),
            "must embed size cap: {code}"
        );
        // try/finally 清理临时包
        assert!(code.contains("os.remove(tmp)"), "must clean up the temp archive: {code}");
        assert!(code.contains("finally:"), "must clean up temp archive on failure: {code}");

        // 解压后必须展平包装目录（如 df3 的 tmp/export/ 嵌套），
        // 使模型文件直接落在 target_dir 下，adapter 无需再 rglob 兼容
        assert!(
            code.contains("def _ep_flatten(d):"),
            "must flatten wrapper dirs: {code}"
        );
        assert!(
            code.contains("shutil.move("),
            "flatten must hoist contents: {code}"
        );
        assert!(
            code.contains("_ep_flatten('G:/EntryPoint/models/url-model')"),
            "flatten must target the model dir: {code}"
        );

        // .tgz 同走解压分支
        let (_program, args) = mgr.build_download_command(
            &test_url_model("https://example.com/m.tgz"),
            Path::new("python"),
        );
        assert!(
            args[1].contains("tarfile"),
            ".tgz must use the extract branch too: {}",
            args[1]
        );
    }

    /// 单文件 URL → 直接下载，文件名取自 URL 尾段
    #[test]
    fn test_gen_download_code_url_single_file_name_from_url() {
        let mgr = ModelManager::new(&test_config("models"), Path::new("G:/EntryPoint"));
        let url = "https://example.com/models/net.onnx";
        let (_program, args) = mgr.build_download_command(&test_url_model(url), Path::new("python"));
        let code = &args[1];

        assert!(code.contains("import urllib.request"), "code: {code}");
        assert!(code.contains(&format!("urlretrieve('{url}'")), "code: {code}");
        assert!(
            code.contains("'net.onnx'"),
            "file name must come from URL tail: {code}"
        );
        assert!(!code.contains("tarfile"), "single file must not extract: {code}");
    }

    /// HF 镜像端点环境变量必须在 import huggingface_hub 之前设置（否则不生效）
    #[test]
    fn test_gen_download_code_hf_endpoint_set_before_import() {
        let mut config = test_config("models");
        config.hf_endpoint = "https://hf-mirror.com".to_string();
        let mgr = ModelManager::new(&config, Path::new("G:/EntryPoint"));
        let (_program, args) = mgr.build_download_command(&test_hf_model(), Path::new("python"));
        let code = &args[1];

        let env_pos = code
            .find("os.environ['HF_ENDPOINT']")
            .expect("must set HF_ENDPOINT");
        let import_pos = code
            .find("from huggingface_hub import")
            .expect("must import huggingface_hub");
        assert!(
            env_pos < import_pos,
            "HF_ENDPOINT must be set before the import: {code}"
        );
    }

    // ── P0/P1/P2 安全回归（注入转义 / 校验 / 文件名清洗 / 精确匹配）────────

    #[test]
    fn test_escape_py_str() {
        assert_eq!(escape_py_str("plain"), "plain");
        assert_eq!(escape_py_str("a'b"), r"a\'b");
        assert_eq!(escape_py_str("a\\b"), r"a\\b");
        assert_eq!(escape_py_str("a\nb"), "a\\nb");
        assert_eq!(escape_py_str("a\r\tb"), "a\\r\\tb");
    }

    #[test]
    fn test_gen_download_code_rejects_invalid_repo_id() {
        let mgr = ModelManager::new(&test_config("models"), Path::new("G:/EntryPoint"));
        // 含空格/引号/分号的 repo_id → 字符集校验拒绝（命令构建返回错误）
        let model = ModelDecl {
            repo_id: Some("Systran/evil'; print('pwned')".to_string()),
            ..test_hf_model()
        };
        let err = mgr
            .build_download_command_with_source(&model, Path::new("python"), None)
            .unwrap_err();
        assert!(err.to_string().contains("invalid repo_id"), "err: {err}");
    }

    #[test]
    fn test_gen_download_code_injection_becomes_error_stub() {
        // build_download_command 无法返回 Result（pub 签名冻结）：
        // 非法 repo_id 必须生成"报错退出"脚本，绝不把载荷拼进可执行代码
        let mgr = ModelManager::new(&test_config("models"), Path::new("G:/EntryPoint"));
        let model = ModelDecl {
            repo_id: Some("Systran/evil'; import os; os.system('x')".to_string()),
            ..test_hf_model()
        };
        let (_p, args) = mgr.build_download_command(&model, Path::new("python"));
        let code = &args[1];
        assert!(code.contains("sys.exit(1)"), "must fail closed: {code}");
        // 载荷只允许以"已转义"形式出现在报错消息字符串内，绝不得以可执行形式出现
        assert!(
            !code.contains("os.system('x')"),
            "payload must never appear as executable code: {code}"
        );
        assert!(
            code.contains("os.system(\\'x\\')"),
            "payload must be safely escaped inside a string literal: {code}"
        );
    }

    #[test]
    fn test_gen_download_code_escapes_path_quotes() {
        // 合法路径可能含单引号：必须转义为 \' 而非注入
        let mgr = ModelManager::new(&test_config("models/O'Neil"), Path::new("G:/EntryPoint"));
        let (_p, args) = mgr.build_download_command(&test_hf_model(), Path::new("python"));
        let code = &args[1];
        assert!(
            code.contains("O\\'Neil"),
            "path quotes must be escaped: {code}"
        );
    }

    #[test]
    fn test_gen_download_code_url_rejects_non_http_scheme() {
        let mgr = ModelManager::new(&test_config("models"), Path::new("G:/EntryPoint"));
        let model = test_url_model("ftp://example.com/x.bin");
        let err = mgr
            .build_download_command_with_source(&model, Path::new("python"), None)
            .unwrap_err();
        assert!(err.to_string().contains("http"), "err: {err}");
    }

    #[test]
    fn test_sanitize_file_name() {
        assert_eq!(sanitize_file_name("https://x.com/a/b/net.onnx"), "net.onnx");
        // query/fragment 剥离
        assert_eq!(
            sanitize_file_name("https://x.com/net.onnx?download=1#frag"),
            "net.onnx"
        );
        // 非法字符替换
        assert_eq!(sanitize_file_name("https://x.com/my file:2.bin"), "my_file_2.bin");
        // 空尾段 / `..` 尾段回退；裸域名取尾段（旧行为保留）
        assert_eq!(sanitize_file_name("https://x.com/"), "model.bin");
        assert_eq!(sanitize_file_name("https://x.com/.."), "model.bin");
        assert_eq!(sanitize_file_name("https://x.com"), "x.com");
    }

    #[test]
    fn test_gen_download_code_url_single_file_strips_query() {
        let mgr = ModelManager::new(&test_config("models"), Path::new("G:/EntryPoint"));
        let url = "https://example.com/models/net.onnx?download=1#section";
        let (_p, args) = mgr.build_download_command(&test_url_model(url), Path::new("python"));
        let code = &args[1];
        assert!(
            code.contains("'net.onnx'"),
            "query/fragment must be stripped from file name: {code}"
        );
    }

    #[test]
    fn test_validate_target_dir() {
        for ok in ["model-a", "faster-whisper-large-v3", "qwen3_asr", "a.b-c"] {
            assert!(validate_target_dir(ok).is_ok(), "should accept {ok:?}");
        }
        for bad in ["", ".", "..", "../x", "a/b", "a\\b", "a b", "a;b", "x/y/z"] {
            assert!(validate_target_dir(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn test_write_meta_rejects_traversal_target_dir() {
        let dir = temp_dir("meta_trav");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));
        let escape_name = format!("ep_escape_{}", std::process::id());
        let err = mgr
            .write_meta(&format!("../../{escape_name}"), &test_meta())
            .unwrap_err();
        assert!(err.to_string().contains("target_dir"), "err: {err}");
        // 未写出缓存根之外
        assert!(!dir.parent().unwrap().join(escape_name).exists());
        cleanup(&dir);
    }

    #[test]
    fn test_model_dir_name_matches() {
        assert!(model_dir_name_matches(
            "faster-whisper-large-v3",
            "faster-whisper-large-v3"
        ));
        assert!(model_dir_name_matches(
            "models--Systran--faster-whisper-large-v3",
            "faster-whisper-large-v3"
        ));
        assert!(model_dir_name_matches("qwen3-asr", "qwen3-asr"));
        assert!(!model_dir_name_matches("qwen3-asr-tokenizer", "qwen3-asr"));
        assert!(!model_dir_name_matches("qwen3-asr", "qwen"));
    }

    #[test]
    fn test_find_in_cache_paths_exact_segment_match() {
        let dir = temp_dir("cache_exact");
        // 子串陷阱目录：qwen3-asr-tokenizer 内含 qwen3-asr / qwen
        let tokenizer = dir.join("qwen3-asr-tokenizer");
        fs::create_dir_all(&tokenizer).unwrap();
        fs::write(tokenizer.join("f.bin"), b"x").unwrap();
        // HF 缓存布局目录（无同名普通目录）
        let hf_layout = dir.join("models--Systran--faster-whisper-large-v3");
        fs::create_dir_all(&hf_layout).unwrap();
        fs::write(hf_layout.join("f.bin"), b"x").unwrap();

        let mut config = test_config(dir.to_str().unwrap());
        config.cache_paths = vec![dir.to_str().unwrap().to_string()];
        let mgr = ModelManager::new(&config, Path::new("."));

        // "qwen3-asr" 不存在 → 不得命中 "qwen3-asr-tokenizer"（旧 contains 会误中）
        assert!(
            mgr.find_in_cache_paths("qwen3-asr").is_none(),
            "substring must not match qwen3-asr-tokenizer"
        );
        // "qwen" 也不得命中 "qwen3-asr-tokenizer"
        assert!(mgr.find_in_cache_paths("qwen").is_none());
        // HF 布局目录经精确末段匹配命中
        let found = mgr
            .find_in_cache_paths("faster-whisper-large-v3")
            .expect("hf layout dir must match");
        assert_eq!(
            found.file_name().unwrap(),
            "models--Systran--faster-whisper-large-v3"
        );
        cleanup(&dir);
    }

    #[test]
    fn test_cleanup_download_temp_files() {
        let dir = temp_dir("dl_cleanup");
        let tmp = dir.join("_download.tar.gz");
        fs::write(&tmp, b"partial").unwrap();
        cleanup_download_temp_files(&dir);
        assert!(!tmp.exists(), "temp archive must be removed");
        // 不存在时 no-op，不 panic
        cleanup_download_temp_files(&dir);
        cleanup(&dir);
    }

    #[test]
    fn test_model_download_gate_rebuilds_on_config_change() {
        // 同一 max 值返回同一闸门实例；max 变化后重建（与 daemon P2-1 同款语义）
        let g2 = model_download_gate(2);
        assert!(Arc::ptr_eq(&g2, &model_download_gate(2)));
        let g1 = model_download_gate(1);
        assert!(!Arc::ptr_eq(&g1, &g2));
    }

    // ── read_meta / write_meta 往返 ────────────────────────────────────

    #[test]
    fn test_meta_roundtrip() {
        let dir = temp_dir("meta_rt");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        let meta = test_meta();
        mgr.write_meta("faster-whisper-large-v3", &meta).unwrap();

        let loaded = mgr.read_meta("faster-whisper-large-v3").expect("meta should exist");
        assert_eq!(loaded.module_id, meta.module_id);
        assert_eq!(loaded.model_id, meta.model_id);
        assert_eq!(loaded.source, meta.source);
        assert_eq!(loaded.repo_id, meta.repo_id);
        assert_eq!(loaded.revision, meta.revision);
        assert_eq!(loaded.downloaded_at, meta.downloaded_at);
        assert_eq!(loaded.total_size_bytes, meta.total_size_bytes);
        // 新增字段往返
        assert_eq!(loaded.qualified_id, None);
        assert!(loaded.tags.is_empty());
        assert_eq!(loaded.pack_id, None);

        cleanup(&dir);
    }

    #[test]
    fn test_meta_roundtrip_with_pack_fields() {
        // 新字段（§4.3 meta 落点）写入→读取往返一致
        let dir = temp_dir("meta_pack_rt");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        let mut meta = test_meta();
        meta.source = "pack".to_string(); // 新取值：整合包导入（§4.4）
        meta.qualified_id = Some("ep.systran.faster-whisper".to_string());
        meta.tags = vec!["字幕".to_string(), "视频".to_string()];
        meta.pack_id = Some("subtitle-kit".to_string());
        mgr.write_meta("faster-whisper-large-v3", &meta).unwrap();

        let loaded = mgr.read_meta("faster-whisper-large-v3").expect("meta should exist");
        assert_eq!(loaded.source, "pack");
        assert_eq!(loaded.qualified_id.as_deref(), Some("ep.systran.faster-whisper"));
        assert_eq!(loaded.tags, vec!["字幕".to_string(), "视频".to_string()]);
        assert_eq!(loaded.pack_id.as_deref(), Some("subtitle-kit"));

        cleanup(&dir);
    }

    #[test]
    fn test_meta_legacy_format_compat() {
        // 旧格式 meta JSON（无 qualified_id/tags/pack_id 字段）必须正常加载
        let dir = temp_dir("meta_legacy");
        let model_path = dir.join("legacy-model");
        fs::create_dir_all(&model_path).unwrap();
        let legacy_json = r#"{
            "module_id": "faster-whisper",
            "model_id": "large-v3",
            "source": "huggingface",
            "repo_id": "Systran/faster-whisper-large-v3",
            "revision": "main",
            "downloaded_at": "2026-07-01T00:00:00Z",
            "total_size_bytes": 1234
        }"#;
        fs::write(model_path.join(META_FILE_NAME), legacy_json).unwrap();

        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        let meta = mgr.read_meta("legacy-model").expect("legacy meta should load");
        assert_eq!(meta.model_id, "large-v3");
        assert_eq!(meta.total_size_bytes, 1234);
        // 新字段取缺省值
        assert_eq!(meta.qualified_id, None);
        assert!(meta.tags.is_empty());
        assert_eq!(meta.pack_id, None);

        cleanup(&dir);
    }

    #[test]
    fn test_meta_serialization_stable_without_new_fields() {
        // 新字段为空时不写入 JSON，输出格式与旧版 meta 保持一致
        let meta = test_meta();
        let json = serde_json::to_string(&meta).unwrap();
        assert!(!json.contains("qualified_id"), "json: {json}");
        assert!(!json.contains("tags"), "json: {json}");
        assert!(!json.contains("pack_id"), "json: {json}");

        // 有值时正常序列化
        let mut meta = meta;
        meta.pack_id = Some("subtitle-kit".to_string());
        let json = serde_json::to_string(&meta).unwrap();
        assert!(json.contains("\"pack_id\":\"subtitle-kit\""), "json: {json}");
    }

    #[test]
    fn test_read_meta_nonexistent() {
        let dir = temp_dir("meta_no");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        assert!(mgr.read_meta("ghost-model").is_none());
        cleanup(&dir);
    }

    #[test]
    fn test_read_meta_invalid_json() {
        let dir = temp_dir("meta_bad");
        let model_path = dir.join("bad-model");
        fs::create_dir_all(&model_path).unwrap();
        fs::write(model_path.join(META_FILE_NAME), "not valid json {{{").unwrap();

        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        assert!(mgr.read_meta("bad-model").is_none());
        cleanup(&dir);
    }

    // ── check_update_available ──────────────────────────────────────────

    #[tokio::test]
    async fn test_check_update_url_source_not_supported() {
        let dir = temp_dir("update_url");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        let model = ModelDecl {
            id: "df3".to_string(),
            name: "DeepFilterNet3".to_string(),
            source: ModelSource::Url,
            repo_id: None,
            url: Some("https://example.com/model.tar.gz".to_string()),
            target_dir: "deep-filter-df3".to_string(),
            revision: None,
            size_estimate_mb: Some(50),
            qualified_id: None,
            vram_estimate_mb: None,
            default: true,
            mirrors: vec![],
            sha256: None,
            sha256s: Default::default(),
        };

        let result = mgr.check_update_available(&model).await;
        assert!(!result.available);
        assert_eq!(result.reason, "URL source does not support update checks");
        assert!(result.remote_modified.is_none());
        cleanup(&dir);
    }

    #[tokio::test]
    async fn test_check_update_missing_meta() {
        let dir = temp_dir("update_nometa");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));
        let model = test_hf_model();

        // 无 .ep_meta.json → 不误报，返回缺少元数据
        let result = mgr.check_update_available(&model).await;
        assert!(!result.available);
        assert!(
            result.reason.contains("missing download metadata"),
            "reason: {}",
            result.reason
        );
        cleanup(&dir);
    }

    #[tokio::test]
    async fn test_check_update_invalid_meta_time() {
        let dir = temp_dir("update_badtime");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));
        let model = test_hf_model();

        let mut meta = test_meta();
        meta.downloaded_at = "not-a-valid-timestamp".to_string();
        mgr.write_meta(&model.target_dir, &meta).unwrap();

        let result = mgr.check_update_available(&model).await;
        assert!(!result.available);
        assert!(
            result.reason.contains("invalid local download timestamp"),
            "reason: {}",
            result.reason
        );
        cleanup(&dir);
    }

    // ── list_downloaded_models ──────────────────────────────────────────

    #[test]
    fn test_list_downloaded_models_empty() {
        let dir = temp_dir("list_empty");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        assert!(mgr.list_downloaded_models().is_empty());
        cleanup(&dir);
    }

    #[test]
    fn test_list_downloaded_models_nonexistent_dir() {
        let config = test_config("Z:/nonexistent/path/models");
        let mgr = ModelManager::new(&config, Path::new("."));

        // 目录不存在时返回空列表，不 panic
        assert!(mgr.list_downloaded_models().is_empty());
    }

    #[test]
    fn test_list_downloaded_models_with_entries() {
        let dir = temp_dir("list_models");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        // 写入两个有效模型
        let meta1 = test_meta();
        mgr.write_meta("model-a", &meta1).unwrap();

        let meta2 = ModelMeta {
            module_id: "qwen3-asr".to_string(),
            model_id: "qwen3-asr".to_string(),
            source: "modelscope".to_string(),
            repo_id: "Qwen/Qwen3-ASR".to_string(),
            revision: "master".to_string(),
            downloaded_at: "2026-07-21T08:00:00Z".to_string(),
            total_size_bytes: 2_000_000_000,
            qualified_id: None,
            tags: Vec::new(),
            pack_id: None,
        };
        mgr.write_meta("model-b", &meta2).unwrap();

        // 创建一个无 meta 的目录（手动放置的模型，不应出现在列表中）
        fs::create_dir_all(dir.join("manual-model")).unwrap();
        fs::write(dir.join("manual-model/model.bin"), b"data").unwrap();

        let list = mgr.list_downloaded_models();
        assert_eq!(list.len(), 2);

        let dirs: Vec<&str> = list.iter().map(|m| m.target_dir.as_str()).collect();
        assert!(dirs.contains(&"model-a"));
        assert!(dirs.contains(&"model-b"));
        assert!(!dirs.contains(&"manual-model"));

        // size_bytes 为真实递归值（P2-10；至少包含 .ep_meta.json 自身）> 0
        for m in &list {
            assert!(m.size_bytes > 0, "{} size should be > 0", m.target_dir);
        }

        cleanup(&dir);
    }

    #[test]
    fn test_list_downloaded_models_size_bytes_real() {
        // P2-10：tempdir 构造已知大小，验证真实递归累计
        let dir = temp_dir("list_size");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        // 先放模型文件（含嵌套子目录），再写 meta
        let model_dir = dir.join("sized-model");
        fs::create_dir_all(model_dir.join("sub")).unwrap();
        fs::write(model_dir.join("model.bin"), vec![0u8; 4096]).unwrap();
        fs::write(model_dir.join("sub").join("weights.bin"), vec![1u8; 1024]).unwrap();
        mgr.write_meta("sized-model", &test_meta()).unwrap();

        let meta_size = fs::metadata(model_dir.join(META_FILE_NAME)).unwrap().len();

        let list = mgr.list_downloaded_models();
        assert_eq!(list.len(), 1);
        // 已知大小 = 顶层 4096 + 子目录 1024 + meta 文件自身
        assert_eq!(list[0].size_bytes, 4096 + 1024 + meta_size);

        cleanup(&dir);
    }

    // ── active_model_for 三级回退（§5.2）──────────────────────────────

    /// 构造带 active_models 配置的 AppConfig
    fn config_with_active(module_id: &str, model_id: &str) -> crate::config::AppConfig {
        let mut config = crate::config::AppConfig::default();
        config
            .active_models
            .insert(module_id.to_string(), model_id.to_string());
        config
    }

    /// 双变体 manifest：small（default=true）在前、large 在后
    fn two_variant_manifest() -> ModuleManifest {
        let mut small = test_ms_model();
        small.id = "small".to_string();
        small.default = true;
        let mut large = test_hf_model();
        large.id = "large".to_string();
        large.default = false;
        test_manifest("faster-whisper", "Faster-Whisper ASR", vec![small, large])
    }

    #[test]
    fn test_active_model_for_config_priority() {
        let manifest = two_variant_manifest();
        // 显式配置命中 → 优先于 default=true
        let config = config_with_active("faster-whisper", "large");
        assert_eq!(active_model_for(&config, &manifest), Some("large"));
    }

    #[test]
    fn test_active_model_for_config_stale_falls_back() {
        let manifest = two_variant_manifest();
        // 配置指向 manifest 未声明的变体（陈旧配置）→ 回退 default=true
        let config = config_with_active("faster-whisper", "ghost-variant");
        assert_eq!(active_model_for(&config, &manifest), Some("small"));
    }

    #[test]
    fn test_active_model_for_default_fallback() {
        let manifest = two_variant_manifest();
        // 无 active_models 配置 → default=true 变体
        let config = crate::config::AppConfig::default();
        assert_eq!(active_model_for(&config, &manifest), Some("small"));
    }

    #[test]
    fn test_active_model_for_first_variant_fallback() {
        // 无配置且无 default=true → 首个变体
        let mut m1 = test_ms_model();
        m1.default = false;
        let mut m2 = test_hf_model();
        m2.default = false;
        let manifest = test_manifest("mod-x", "Mod X", vec![m1, m2]);
        let config = crate::config::AppConfig::default();
        assert_eq!(active_model_for(&config, &manifest), Some("qwen3-asr"));
    }

    #[test]
    fn test_active_model_for_other_module_config_ignored() {
        // active_models 中其他模块的配置不影响本模块
        let manifest = two_variant_manifest();
        let config = config_with_active("some-other-module", "large");
        assert_eq!(active_model_for(&config, &manifest), Some("small"));
    }

    #[test]
    fn test_active_model_for_no_models() {
        // native 模块无模型变体 → None
        let manifest = test_manifest("native-mod", "Native", vec![]);
        let config = crate::config::AppConfig::default();
        assert_eq!(active_model_for(&config, &manifest), None);
    }

    // ── execute_download / cancel_download tests ─────────────────────────

    #[tokio::test]
    async fn test_execute_download_fails_with_invalid_python() {
        let dir = temp_dir("exec_dl");
        let config = test_config(dir.to_str().unwrap());
        let mut mgr = ModelManager::new(&config, Path::new("."));
        let model = test_hf_model();
        let app_config = crate::config::AppConfig::default();

        // 使用不存在的 python 路径 → spawn 会失败
        let result = mgr
            .execute_download(&model, Path::new("."), Path::new("/nonexistent/python"), &app_config)
            .await;
        assert!(result.is_err());

        cleanup(&dir);
    }

    #[tokio::test]
    async fn test_cancel_download_noop_when_no_download() {
        let dir = temp_dir("cancel_noop");
        let config = test_config(dir.to_str().unwrap());
        let mut mgr = ModelManager::new(&config, Path::new("."));

        // 没有正在进行的下载时，cancel 应该是 no-op，不 panic
        mgr.cancel_download().await;

        cleanup(&dir);
    }

    // ── list_all_models ────────────────────────────────────────────────

    /// 创建测试用 ModuleManifest
    fn test_manifest(module_id: &str, module_name: &str, models: Vec<ModelDecl>) -> ModuleManifest {
        use crate::module::manifest::*;
        use crate::types::{ComputeBackend, DataType, ModuleCategory};

        ModuleManifest {
            module: ModuleInfo {
                id: module_id.to_string(),
                name: module_name.to_string(),
                version: "1.0.0".to_string(),
                description: "test module".to_string(),
                category: ModuleCategory::Asr,
                genre: "test".to_string(),
                authors: vec![],
                license: None,
                homepage: None,
                tags: vec![],
            },
            runtime: RuntimeConfig {
                runtime_type: RuntimeType::Python,
                python_version: Some(">=3.10".to_string()),
                requirements: None,
                requirements_by_backend: Default::default(),
                entrypoint: None,
                start_command: None,
                binaries: None,
            },
            compute: ComputeConfig {
                backends: vec![ComputeBackend::Cpu],
                default_backend: None,
                vram_estimate_mb: None,
                min_vram_mb: None,
                env: None,
            },
            models,
            interface: InterfaceConfig {
                interface_type: InterfaceType::Http,
                health_endpoint: None,
                ready_timeout_secs: None,
                working_dir: None,
                capabilities: vec![CapabilityDecl {
                    name: "test".to_string(),
                    description: "test cap".to_string(),
                    input_type: DataType::Audio,
                    output_type: DataType::Json,
                    max_file_size_mb: None,
                    supports_batch: false,
                    params: None,
                }],
            },
        }
    }

    #[test]
    fn test_list_all_models_empty() {
        let dir = temp_dir("list_all_empty");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        let views = mgr.list_all_models(&[]);
        assert!(views.is_empty());

        cleanup(&dir);
    }

    #[test]
    fn test_list_all_models_mixed_status() {
        let dir = temp_dir("list_all_mixed");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        // 创建一个已就绪的模型目录
        let ready_dir = dir.join("ready-model");
        fs::create_dir_all(&ready_dir).unwrap();
        fs::write(ready_dir.join("model.bin"), b"fake weights").unwrap();

        let manifest = test_manifest(
            "test-module",
            "Test Module",
            vec![
                ModelDecl {
                    id: "ready".to_string(),
                    name: "Ready Model".to_string(),
                    source: ModelSource::Huggingface,
                    repo_id: Some("org/ready-model".to_string()),
                    url: None,
                    target_dir: "ready-model".to_string(),
                    revision: Some("main".to_string()),
                    size_estimate_mb: Some(100),
                    qualified_id: None,
                    vram_estimate_mb: None,
                    default: true,
                    mirrors: vec![],
                    sha256: None,
                    sha256s: Default::default(),
                },
                ModelDecl {
                    id: "missing".to_string(),
                    name: "Missing Model".to_string(),
                    source: ModelSource::Modelscope,
                    repo_id: Some("org/missing-model".to_string()),
                    url: None,
                    target_dir: "missing-model".to_string(),
                    revision: None,
                    size_estimate_mb: None,
                    qualified_id: None,
                    vram_estimate_mb: None,
                    default: false,
                    mirrors: vec![],
                    sha256: None,
                    sha256s: Default::default(),
                },
            ],
        );

        let views = mgr.list_all_models(&[manifest]);
        assert_eq!(views.len(), 2);

        // 第一个模型：Ready
        let ready = &views[0];
        assert_eq!(ready.module_id, "test-module");
        assert_eq!(ready.module_name, "Test Module");
        assert_eq!(ready.model_id, "ready");
        assert_eq!(ready.source, "huggingface");
        assert_eq!(ready.repo_id, "org/ready-model");
        assert_eq!(ready.status, ModelStatus::Ready);
        assert!(ready.size_bytes.is_some());
        assert!(ready.size_bytes.unwrap() > 0);

        // 第二个模型：Missing
        let missing = &views[1];
        assert_eq!(missing.model_id, "missing");
        assert_eq!(missing.source, "modelscope");
        assert_eq!(missing.status, ModelStatus::Missing);
        assert!(missing.size_bytes.is_none());

        cleanup(&dir);
    }

    #[test]
    fn test_list_all_models_multiple_modules() {
        let dir = temp_dir("list_all_multi");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        let m1 = test_manifest("mod-a", "Module A", vec![test_hf_model()]);
        let m2 = test_manifest("mod-b", "Module B", vec![test_ms_model()]);

        let views = mgr.list_all_models(&[m1, m2]);
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].module_id, "mod-a");
        assert_eq!(views[1].module_id, "mod-b");

        cleanup(&dir);
    }

    #[test]
    fn test_model_view_serialization() {
        let view = ModelView {
            module_id: "test".to_string(),
            module_name: "Test".to_string(),
            model_id: "m1".to_string(),
            model_name: "Model 1".to_string(),
            source: "huggingface".to_string(),
            repo_id: "org/model".to_string(),
            target_dir: "model-dir".to_string(),
            status: ModelStatus::Ready,
            size_bytes: Some(1024),
            available_sources: vec![ModelSource::Huggingface, ModelSource::Modelscope],
        };
        let json = serde_json::to_string(&view).unwrap();
        assert!(json.contains("\"status\":\"ready\""));
        assert!(json.contains("\"size_bytes\":1024"));
        assert!(json.contains("\"available_sources\":[\"huggingface\",\"modelscope\"]"));
    }

    // ── build_download_command_with_source ─────────────────────────────

    #[test]
    fn test_build_command_source_override_mirror() {
        let root = Path::new("/opt/entrypoint");
        let config = test_config("models");
        let mgr = ModelManager::new(&config, root);
        let model = test_mirrored_model();

        // 指定 ModelScope mirror
        let (_program, args) = mgr
            .build_download_command_with_source(&model, Path::new("python"), Some(ModelSource::Modelscope))
            .unwrap();
        let code = &args[1];
        assert!(code.contains("modelscope"), "code: {code}");
        assert!(code.contains("pengzhendong/faster-whisper-large-v3"), "code: {code}");
        // mirror 声明了 revision="master"
        assert!(code.contains("revision='master'"), "code: {code}");
    }

    #[test]
    fn test_build_command_source_override_primary() {
        let root = Path::new("/opt/entrypoint");
        let config = test_config("models");
        let mgr = ModelManager::new(&config, root);
        let model = test_mirrored_model();

        // 显式指定主源 → 仍走 HF
        let (_program, args) = mgr
            .build_download_command_with_source(&model, Path::new("python"), Some(ModelSource::Huggingface))
            .unwrap();
        let code = &args[1];
        assert!(code.contains("huggingface_hub"), "code: {code}");
        assert!(code.contains("Systran/faster-whisper-large-v3"), "code: {code}");
    }

    #[test]
    fn test_build_command_source_unavailable_error() {
        let root = Path::new("/opt/entrypoint");
        let config = test_config("models");
        let mgr = ModelManager::new(&config, root);
        let model = test_hf_model(); // 无 mirror

        let err = mgr
            .build_download_command_with_source(&model, Path::new("python"), Some(ModelSource::Modelscope))
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("does not support download source"), "msg: {msg}");
        assert!(msg.contains("available sources"), "msg: {msg}");
    }

    // ── compute_percent 进度钳制 ────────────────────────────────────────

    #[test]
    fn test_compute_percent() {
        // 正常百分比
        let half = compute_percent(50 * 1024 * 1024, Some(100));
        assert!((half - 50.0).abs() < 0.01, "half: {half}");

        // 超出估算 → 钳制到 99.0
        let over = compute_percent(200 * 1024 * 1024, Some(100));
        assert!((over - 99.0).abs() < f32::EPSILON, "over: {over}");

        // 无估算 → 恒 0
        assert_eq!(compute_percent(123456, None), 0.0);
        assert_eq!(compute_percent(123456, Some(0)), 0.0);

        // 0 字节 → 0
        assert_eq!(compute_percent(0, Some(100)), 0.0);
    }

    #[test]
    fn test_truncate_chars() {
        assert_eq!(truncate_chars("héllo", 10), "héllo");
        let long = "é".repeat(20);
        let t = truncate_chars(&long, 5);
        assert_eq!(t.chars().count(), 6); // 5 chars + ellipsis
        assert!(t.ends_with('…'));
    }

    // ── cleanup_hf_cache 安全规则 ───────────────────────────────────────

    #[test]
    fn test_cleanup_hf_cache_safe_to_remove() {
        let dir = temp_dir("cleanup_safe");
        let model_dir = dir.join("my-model");

        // 顶层真实文件
        fs::create_dir_all(&model_dir).unwrap();
        fs::write(model_dir.join("model.bin"), vec![0xAB; 4096]).unwrap();
        fs::write(model_dir.join("config.json"), r#"{"a":1}"#).unwrap();

        // snapshots/<rev>/ 下的同名同大小副本（可删）
        let snap = model_dir.join("snapshots").join("rev1");
        fs::create_dir_all(&snap).unwrap();
        fs::write(snap.join("model.bin"), vec![0xAB; 4096]).unwrap();
        fs::write(snap.join("config.json"), r#"{"a":1}"#).unwrap();
        // 大小不一致的副本（不可删）
        fs::write(snap.join("extra.txt"), "mismatched").unwrap();
        fs::write(model_dir.join("extra.txt"), "different size content!!").unwrap();

        // blobs/ 下哈希命名文件（顶层无同名文件 → 不可删）
        let blobs = model_dir.join("blobs");
        fs::create_dir_all(&blobs).unwrap();
        fs::write(blobs.join("a1b2c3d4hash"), vec![0u8; 4096]).unwrap();

        // refs/main（顶层无同名文件 → 保留）
        let refs = model_dir.join("refs");
        fs::create_dir_all(&refs).unwrap();
        fs::write(refs.join("main"), "rev1").unwrap();

        let reclaimed = cleanup_hf_cache(&model_dir).unwrap();

        // 回收 = snapshots 内两个同名同大小副本
        assert_eq!(reclaimed, 4096 + 7);
        assert!(!snap.join("model.bin").exists());
        assert!(!snap.join("config.json").exists());
        // 大小不一致 → 保留
        assert!(snap.join("extra.txt").exists());
        // blobs / refs 保留
        assert!(blobs.join("a1b2c3d4hash").exists());
        assert!(refs.join("main").exists());
        // 顶层文件不受影响
        assert!(model_dir.join("model.bin").exists());

        cleanup(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn test_cleanup_hf_cache_skips_when_top_symlink() {
        let dir = temp_dir("cleanup_symlink");
        let model_dir = dir.join("linked-model");
        fs::create_dir_all(&model_dir).unwrap();

        // snapshots/<rev>/model.bin 是真实内容
        let snap = model_dir.join("snapshots").join("rev1");
        fs::create_dir_all(&snap).unwrap();
        fs::write(snap.join("model.bin"), vec![0xCD; 2048]).unwrap();

        // 顶层 model.bin 是指向 snapshots 内的 symlink → 整个 snapshots 跳过
        std::os::unix::fs::symlink(
            snap.join("model.bin"),
            model_dir.join("model.bin"),
        )
        .unwrap();

        let reclaimed = cleanup_hf_cache(&model_dir).unwrap();
        assert_eq!(reclaimed, 0);
        // 缓存副本未被删除
        assert!(snap.join("model.bin").exists());

        cleanup(&dir);
    }

    #[test]
    fn test_cleanup_hf_cache_nonexistent_dir() {
        let err = cleanup_hf_cache(Path::new("/nonexistent/model/dir")).unwrap_err();
        assert!(err.to_string().contains("model directory does not exist"));
    }

    #[test]
    fn test_cleanup_hf_cache_prunes_empty_dirs() {
        let dir = temp_dir("cleanup_prune");
        let model_dir = dir.join("prune-model");
        fs::create_dir_all(&model_dir).unwrap();
        fs::write(model_dir.join("f.bin"), b"12345").unwrap();

        let snap = model_dir.join("snapshots").join("revX");
        fs::create_dir_all(&snap).unwrap();
        fs::write(snap.join("f.bin"), b"12345").unwrap();

        let reclaimed = cleanup_hf_cache(&model_dir).unwrap();
        assert_eq!(reclaimed, 5);
        // 清空的 snapshots 目录被移除
        assert!(!model_dir.join("snapshots").exists());

        cleanup(&dir);
    }

    #[test]
    fn test_path_under_prefix_check() {
        if cfg!(windows) {
            // Windows 大小写不敏感：盘符/目录大小写不一致也必须命中
            assert!(path_under(
                Path::new("C:/Models/mymodel/blobs"),
                Path::new("c:/models/mymodel")
            ));
            assert!(!path_under(
                Path::new("C:/Other/mymodel/blobs"),
                Path::new("c:/models/mymodel")
            ));
        } else {
            assert!(path_under(
                Path::new("/opt/models/mymodel/blobs"),
                Path::new("/opt/models/mymodel")
            ));
            // Unix 严格大小写敏感
            assert!(!path_under(
                Path::new("/OPT/models/mymodel/blobs"),
                Path::new("/opt/models/mymodel")
            ));
        }
    }

    #[test]
    #[cfg(unix)]
    fn test_dir_total_size_symlink_loop_safe() {
        // symlink 指向祖先目录构成环：递归必须安全终止，不重复计数
        let dir = temp_dir("size_loop");
        let sub = dir.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.join("a.bin"), vec![0u8; 100]).unwrap();
        std::os::unix::fs::symlink(&dir, sub.join("loop")).unwrap();

        let size = dir_total_size(&dir);
        // 至少计入真实文件；且不会因环而指数爆炸
        assert!(size >= 100, "size: {size}");
        assert!(size < 100 + 1024 * 1024, "size exploded via symlink loop: {size}");

        cleanup(&dir);
    }

    // ── import_model target_dir 修复 ────────────────────────────────────

    #[tokio::test]
    async fn test_import_model_uses_manifest_target_dir() {
        let dir = temp_dir("import_fix");
        let config = test_config(dir.to_str().unwrap());

        // 注册 manifest：model_id=large-v3，target_dir=faster-whisper-large-v3
        let manifest = test_manifest_with_model(test_hf_model());
        let mgr = ModelManager::new(&config, Path::new("."))
            .with_manifests(vec![manifest]);

        // 源目录
        let src = dir.join("src-model");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("model.bin"), b"weights-data").unwrap();

        mgr.import_model("faster-whisper", "large-v3", &src)
            .await
            .unwrap();

        // 文件落在 manifest 的 target_dir，而不是 model_id
        assert!(dir.join("faster-whisper-large-v3/model.bin").exists());
        assert!(!dir.join("large-v3/model.bin").exists());

        // meta 也写入正确目录
        let meta = mgr.read_meta("faster-whisper-large-v3").expect("meta");
        assert_eq!(meta.model_id, "large-v3");
        assert_eq!(meta.source, "local_import");

        cleanup(&dir);
    }

    #[tokio::test]
    async fn test_import_model_fallback_without_manifests() {
        let dir = temp_dir("import_fallback");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        let src = dir.join("src2");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("w.bin"), b"data").unwrap();

        // 未注册 manifest → 回退为 model_id（旧行为，保持向后兼容）
        mgr.import_model("some-module", "my-model", &src).await.unwrap();
        assert!(dir.join("my-model/w.bin").exists());

        cleanup(&dir);
    }

    #[tokio::test]
    async fn test_import_model_with_manifest_unknown_model_errors() {
        let dir = temp_dir("import_unknown");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));
        let manifest = test_manifest_with_model(test_hf_model());

        let src = dir.join("src3");
        fs::create_dir_all(&src).unwrap();

        let err = mgr
            .import_model_with_manifest("faster-whisper", "no-such-model", &src, &manifest)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found in manifest"), "err: {err}");

        cleanup(&dir);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_copy_dir_recursive_skips_symlinks_and_cycles() {
        // P2：源目录含指向祖先的 symlink（无限递归风险）与指向外部的 symlink：
        // 一律跳过，不得跟随、不得死循环、不得泄出内容
        let dir = temp_dir("copy_cycle");
        let src = dir.join("src");
        let sub = src.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(src.join("a.bin"), vec![0u8; 100]).unwrap();
        // symlink 指向祖先（src 自身）
        std::os::unix::fs::symlink(&src, sub.join("back")).unwrap();
        // symlink 指向外部文件
        let outside = dir.join("outside.bin");
        fs::write(&outside, b"secret").unwrap();
        std::os::unix::fs::symlink(&outside, sub.join("leak")).unwrap();

        let mgr = ModelManager::new(&test_config(dir.to_str().unwrap()), Path::new("."));
        let dst = dir.join("dst");
        fs::create_dir_all(&dst).unwrap();
        let (files, bytes) = mgr.copy_dir_recursive(&src, &dst).await.unwrap();

        // 只复制 a.bin（两个 symlink 均跳过）
        assert_eq!(files, 1, "only a.bin must be copied");
        assert_eq!(bytes, 100);
        assert!(dst.join("a.bin").exists());
        assert!(
            !dst.join("sub").join("back").exists(),
            "ancestor symlink must not be copied"
        );
        assert!(
            !dst.join("sub").join("leak").exists(),
            "external symlink must not be copied"
        );
        assert!(!dst.join("outside.bin").exists(), "no content may leak out");
        cleanup(&dir);
    }

    #[tokio::test]
    async fn test_import_model_rejects_traversal_target_dir() {
        // P1：恶意/损坏 manifest 的 target_dir="../../x" 必须被拒绝，写出缓存根
        let dir = temp_dir("import_trav");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));
        let mut model = test_hf_model();
        model.target_dir = "../../escape_import".to_string();
        let manifest = test_manifest_with_model(model);

        let src = dir.join("src4");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("w.bin"), b"data").unwrap();

        let err = mgr
            .import_model_with_manifest("faster-whisper", "large-v3", &src, &manifest)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("target_dir"), "err: {err}");
        let escape = std::env::temp_dir().join("escape_import");
        assert!(!escape.exists(), "must not write outside cache root");
        cleanup(&dir);
    }

    // ── execute_download 成功后写 meta ─────────────────────────────────

    #[tokio::test]
    async fn test_execute_download_writes_meta() {
        // 用 url="auto" 模型走空操作 python 命令，避免真实网络下载
        if cfg!(windows) {
            return; // 依赖 /usr/bin/python3
        }
        let dir = temp_dir("exec_meta");
        let mut config = test_config(dir.to_str().unwrap());
        config.cache_dir = dir.to_str().unwrap().to_string();
        let mut mgr = ModelManager::new(&config, Path::new("."));

        let model = ModelDecl {
            id: "auto-model".to_string(),
            name: "Auto".to_string(),
            source: ModelSource::Url,
            repo_id: None,
            url: Some("auto".to_string()),
            target_dir: "auto-model-dir".to_string(),
            revision: None,
            size_estimate_mb: None,
            qualified_id: None,
            vram_estimate_mb: None,
            default: false,
            mirrors: vec![],
            sha256: None,
            sha256s: Default::default(),
        };

        let app_config = crate::config::AppConfig::default();
        let module_dir = Path::new("/fake/modules/test-module");

        mgr.execute_download(&model, module_dir, Path::new("/usr/bin/python3"), &app_config)
            .await
            .unwrap();

        // meta 已写入，字段正确
        let meta = mgr.read_meta("auto-model-dir").expect("meta should be written");
        assert_eq!(meta.module_id, "test-module");
        assert_eq!(meta.model_id, "auto-model");
        assert_eq!(meta.source, "url");
        assert_eq!(meta.repo_id, "auto");
        assert!(!meta.downloaded_at.is_empty());
        // downloaded_at 可解析为 RFC 3339
        assert!(chrono::DateTime::parse_from_rfc3339(&meta.downloaded_at).is_ok());

        cleanup(&dir);
    }

    // ── 带进度的下载 ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_tracked_download_success_and_meta() {
        if cfg!(windows) {
            return;
        }
        let dir = temp_dir("tracked_ok");
        let target = dir.join("tracked-model");

        let script = format!(
            "mkdir -p '{target}' && head -c 2048 /dev/zero > '{target}/f.bin'",
            target = target.display()
        );
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", &script])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = cmd.spawn().unwrap();

        let meta = ModelMeta {
            module_id: "mod".to_string(),
            model_id: "tracked-model".to_string(),
            source: "url".to_string(),
            repo_id: "https://example.com/f.bin".to_string(),
            revision: String::new(),
            downloaded_at: String::new(),
            total_size_bytes: 0,
            qualified_id: None,
            tags: Vec::new(),
            pack_id: None,
        };

        let handle = spawn_tracked_download(
            child,
            target.clone(),
            Some(1), // 1 MB 估算
            "mod".to_string(),
            "tracked-model".to_string(),
            Some(meta),
            true,
            ModelChecksumSpec::default(),
        );
        assert_eq!(handle.module_id(), "mod");
        assert_eq!(handle.model_id(), "tracked-model");

        let mut rx = handle.subscribe_progress();
        let bytes = handle.wait().await.unwrap();
        assert!(bytes >= 2048, "bytes: {bytes}");

        // 接收事件直到 Completed（终态事件必达）
        let mut events = Vec::new();
        loop {
            let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("timed out waiting for progress events")
                .expect("progress channel closed unexpectedly");
            let done = matches!(ev.state, DownloadState::Completed);
            events.push(ev);
            if done {
                break;
            }
        }
        let last = events.last().unwrap();
        assert!(matches!(last.state, DownloadState::Completed));
        assert!((last.percent - 100.0).abs() < f32::EPSILON);
        // 下载中事件百分比不得超过 99
        for ev in &events {
            if matches!(ev.state, DownloadState::Downloading) {
                assert!(ev.percent <= 99.0);
            }
        }

        // meta 已写入且大小已填充
        let meta_path = target.join(META_FILE_NAME);
        assert!(meta_path.exists());
        let content = fs::read_to_string(&meta_path).unwrap();
        let m: ModelMeta = serde_json::from_str(&content).unwrap();
        assert!(m.total_size_bytes >= 2048);
        assert!(!m.downloaded_at.is_empty());

        cleanup(&dir);
    }

    #[tokio::test]
    async fn test_tracked_download_cancel() {
        if cfg!(windows) {
            return;
        }
        let dir = temp_dir("tracked_cancel");
        fs::create_dir_all(&dir).unwrap();

        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", "sleep 30"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = cmd.spawn().unwrap();

        let mut handle = spawn_tracked_download(
            child,
            dir.clone(),
            None,
            "mod".to_string(),
            "sleepy".to_string(),
            None,
            true,
            ModelChecksumSpec::default(),
        );
        let mut rx = handle.subscribe_progress();

        // 立即取消
        handle.cancel();
        // 二次取消应为 no-op，不 panic
        handle.cancel();

        let err = handle.wait().await.unwrap_err();
        assert!(err.contains("cancelled"), "err: {err}");

        // 必达 Cancelled 终态事件（收到即验证通过）
        loop {
            let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("timed out waiting for Cancelled event")
                .expect("progress channel closed unexpectedly");
            if matches!(ev.state, DownloadState::Cancelled) {
                break;
            }
        }

        cleanup(&dir);
    }

    /// 取消下载后，首次下载（启动时空目录）落盘的残缺文件必须被清掉——
    /// 否则状态检查会因"目录有文件"误判 ready，并阻塞后续导入。
    #[tokio::test]
    async fn test_tracked_download_cancel_cleans_partial_files() {
        if cfg!(windows) {
            return;
        }
        let dir = temp_dir("tracked_cancel_partial");

        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", &format!(
            "echo partial-content > {}/partial.onnx; sleep 30",
            dir.display()
        )])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = cmd.spawn().unwrap();

        let mut handle = spawn_tracked_download(
            child,
            dir.clone(),
            None,
            "mod".to_string(),
            "partial".to_string(),
            None,
            true,
            ModelChecksumSpec::default(),
        );

        handle.cancel();
        let err = handle.wait().await.unwrap_err();
        assert!(err.contains("cancelled"), "err: {err}");

        // 子进程已写入残缺文件；取消后目录必须被清空（保留目录本身）
        assert!(
            !dir_has_entries(&dir),
            "partial download artifacts must be removed after cancel"
        );

        cleanup(&dir);
    }

    /// 目录在下载启动前已有内容（断点续传场景）→ 失败时只清临时文件，
    /// 不得误删既有文件。
    #[tokio::test]
    async fn test_tracked_download_failure_keeps_preexisting_files() {
        if cfg!(windows) {
            return;
        }
        let dir = temp_dir("tracked_fail_keep");
        fs::write(dir.join("existing.bin"), b"keep me").unwrap();

        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", &format!(
            "echo x > {}/fresh.bin; exit 3",
            dir.display()
        )])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = cmd.spawn().unwrap();

        let handle = spawn_tracked_download(
            child,
            dir.clone(),
            Some(10),
            "mod".to_string(),
            "failing".to_string(),
            None,
            false,
            ModelChecksumSpec::default(),
        );
        let err = handle.wait().await.unwrap_err();
        assert!(err.contains("download failed"), "err: {err}");

        assert!(dir.join("existing.bin").exists(), "pre-existing files must be kept");
        assert!(dir.join("fresh.bin").exists(), "pre-existing dir: no wipe");

        cleanup(&dir);
    }

    /// 下载完成时 sha256 校验不匹配 → Failed 终态 + 残缺文件清理
    #[tokio::test]
    async fn test_tracked_download_checksum_mismatch_fails() {
        if cfg!(windows) {
            return;
        }
        let dir = temp_dir("tracked_checksum_bad");

        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", &format!(
            "echo corrupted-payload > {}/weights.bin; exit 0",
            dir.display()
        )])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = cmd.spawn().unwrap();

        let checksum = ModelChecksumSpec {
            single: Some("0".repeat(64)),
            files: Default::default(),
        };
        let handle = spawn_tracked_download(
            child,
            dir.clone(),
            Some(1),
            "mod".to_string(),
            "badsum".to_string(),
            None,
            true,
            checksum,
        );
        let err = handle.wait().await.unwrap_err();
        assert!(err.contains("校验失败"), "err: {err}");
        assert!(err.contains("sha256 不匹配"), "err: {err}");

        // 校验失败按残缺产物清理：目录必须被清空，坏文件不得蒙混成 ready
        assert!(!dir_has_entries(&dir), "mismatched download must be cleaned");

        cleanup(&dir);
    }

    /// sha256 匹配 → 正常 Completed 且 meta 落盘
    #[tokio::test]
    async fn test_tracked_download_checksum_match_completes() {
        if cfg!(windows) {
            return;
        }
        let dir = temp_dir("tracked_checksum_ok");

        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", &format!(
            "printf payload-bytes > {}/weights.bin; exit 0",
            dir.display()
        )])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = cmd.spawn().unwrap();

        // 先离线算好期望摘要（与子进程写出的内容一致）
        let payload = dir.join("weights.bin");
        std::fs::write(&payload, b"payload-bytes").unwrap();
        let expected = sha256_file_hex(&payload).unwrap();
        std::fs::remove_file(&payload).unwrap();

        let checksum = ModelChecksumSpec {
            single: Some(expected),
            files: Default::default(),
        };
        let handle = spawn_tracked_download(
            child,
            dir.clone(),
            Some(1),
            "mod".to_string(),
            "oksum".to_string(),
            None,
            true,
            checksum,
        );
        let mut rx = handle.subscribe_progress();
        handle.wait().await.expect("download with matching sha256 must complete");

        loop {
            let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("timed out waiting for progress events")
                .expect("progress channel closed unexpectedly");
            if matches!(ev.state, DownloadState::Completed) {
                break;
            }
        }

        cleanup(&dir);
    }

    #[tokio::test]
    async fn test_tracked_download_failure_with_stderr_summary() {
        if cfg!(windows) {
            return;
        }
        let dir = temp_dir("tracked_fail");
        fs::create_dir_all(&dir).unwrap();

        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", "echo simulated error message >&2; exit 3"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = cmd.spawn().unwrap();

        let handle = spawn_tracked_download(
            child,
            dir.clone(),
            Some(10),
            "mod".to_string(),
            "failing".to_string(),
            None,
            true,
            ModelChecksumSpec::default(),
        );
        let mut rx = handle.subscribe_progress();
        let err = handle.wait().await.unwrap_err();

        // 错误摘要包含中文包装与 stderr 尾部
        assert!(err.contains("download failed"), "err: {err}");
        assert!(err.contains("simulated error message"), "err: {err}");

        // 必达 Failed 终态事件（收到即验证通过）
        loop {
            let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("timed out waiting for Failed event")
                .expect("progress channel closed unexpectedly");
            if matches!(ev.state, DownloadState::Failed(_)) {
                break;
            }
        }

        cleanup(&dir);
    }

    /// 构造含单个模型声明的 manifest
    fn test_manifest_with_model(model: ModelDecl) -> ModuleManifest {
        test_manifest("faster-whisper", "Faster-Whisper ASR", vec![model])
    }
}
