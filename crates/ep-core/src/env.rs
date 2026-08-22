//! 虚拟环境管理器 — 负责 Python venv 的创建、依赖安装和状态检测
//!
//! 使用 `uv` 作为底层工具完成 venv 创建和 pip 安装。
//! 通过 requirements.txt 的哈希值判断是否需要重新安装依赖。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use tracing::{debug, info, warn};

use crate::config::PythonConfig;
use crate::process::apply_no_window;
use crate::types::ComputeBackend;

// ─── 公共类型 ────────────────────────────────────────────────────────────────

/// 工具检测结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolStatus {
    /// 已找到：路径 + 版本字符串
    Found(PathBuf, String),
    /// 未找到：附带安装提示
    NotFound { install_hint: String },
}

impl ToolStatus {
    pub fn is_found(&self) -> bool {
        matches!(self, Self::Found(..))
    }
}

/// Python + uv 前置检测结果
#[derive(Debug, Clone)]
pub struct EnvCheckResult {
    pub python: ToolStatus,
    pub uv: ToolStatus,
}

impl EnvCheckResult {
    /// 所有前置条件是否满足
    pub fn all_ready(&self) -> bool {
        self.python.is_found() && self.uv.is_found()
    }
}

/// 虚拟环境状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VenvStatus {
    /// venv 不存在
    NotExist,
    /// venv 就绪（python 存在，依赖哈希匹配或无 requirements）
    Ready,
    /// venv 存在但依赖需要更新
    NeedsUpdate,
}

// ─── EnvManager ──────────────────────────────────────────────────────────────

/// 虚拟环境管理器
///
/// 负责检测 Python/uv、创建 venv、安装依赖、判断环境就绪状态。
pub struct EnvManager {
    /// 应用根目录
    root: PathBuf,
    /// 检测到的 python 路径
    python_path: Option<PathBuf>,
    /// 检测到的 uv 路径
    uv_path: Option<PathBuf>,
    /// 网络代理环境变量（注入 uv/pip 子进程）
    network_env: Vec<(String, String)>,
    /// uv 缓存目录（相对应用根，§3.1；空字符串 = 不注入 UV_CACHE_DIR）
    uv_cache_dir: String,
    /// 全局 constraints 文件（相对应用根，§3.1；空字符串 = 停用）
    constraints: String,
}

impl EnvManager {
    /// 创建 EnvManager
    ///
    /// - 如果 `config.path` 非空，使用指定路径作为 python
    /// - 否则在系统 PATH 中检测 `python3` / `python`
    /// - 同理检测 uv（`config.uv_path` 或 PATH 中的 `uv`）
    pub fn new(root: &Path, config: &PythonConfig) -> Self {
        // 先检测 uv，detect_python 需要借助 uv python find 查找兼容版本
        let uv_path = if !config.uv_path.is_empty() {
            if Self::is_bare_command_name(&config.uv_path) {
                // 裸命令名（如 "uv"）：走 PATH 解析，而非文件存在性检查
                match Self::which(&config.uv_path) {
                    Some(resolved) => {
                        debug!(path = %resolved.display(), "resolved configured uv command via PATH");
                        Some(resolved)
                    }
                    None => {
                        warn!(cmd = %config.uv_path, "configured uv command not found in PATH, falling back to detection");
                        Self::detect_uv()
                    }
                }
            } else {
                let p = PathBuf::from(&config.uv_path);
                if p.exists() {
                    debug!(path = %p.display(), "using configured uv path");
                    Some(p)
                } else {
                    warn!(path = %p.display(), "configured uv path does not exist, falling back to detection");
                    Self::detect_uv()
                }
            }
        } else {
            Self::detect_uv()
        };

        let python_path = if !config.path.is_empty() {
            if Self::is_bare_command_name(&config.path) {
                // 裸命令名（如 "python"）：走 PATH 解析，而非文件存在性检查
                match Self::which(&config.path) {
                    Some(resolved) => {
                        debug!(path = %resolved.display(), "resolved configured python command via PATH");
                        Some(resolved)
                    }
                    None => {
                        warn!(cmd = %config.path, "configured python command not found in PATH, falling back to detection");
                        Self::detect_python(uv_path.as_deref())
                    }
                }
            } else {
                let p = PathBuf::from(&config.path);
                if p.exists() {
                    debug!(path = %p.display(), "using configured python path");
                    Some(p)
                } else {
                    warn!(path = %p.display(), "configured python path does not exist, falling back to detection");
                    Self::detect_python(uv_path.as_deref())
                }
            }
        } else {
            Self::detect_python(uv_path.as_deref())
        };

        Self {
            root: root.to_path_buf(),
            python_path,
            uv_path,
            network_env: Vec::new(),
            uv_cache_dir: config.uv_cache_dir.clone(),
            constraints: config.constraints.clone(),
        }
    }

    /// 设置网络代理配置（链式调用）。
    ///
    /// uv venv / uv pip install 子进程将被注入这些环境变量（仅非空值）。
    pub fn with_network(mut self, network: &crate::config::NetworkConfig) -> Self {
        self.network_env = network.env_vars();
        self
    }

    /// 检测系统 PATH 中的 python（优先 python3，其次 python）
    /// 若 PATH 中无兼容版本，则借助 uv python find 查找 uv 管理的 Python
    fn detect_python(uv: Option<&Path>) -> Option<PathBuf> {
        // 1. 尝试 PATH 中的 python3 / python
        for name in ["python3", "python"] {
            let mut probe = Command::new(name);
            apply_no_window(&mut probe);
            if let Ok(output) = probe.arg("--version").output() {
                if output.status.success() {
                    if let Some(path) = Self::which(name) {
                        debug!(python = %path.display(), "detected python in PATH");
                        return Some(path);
                    }
                }
            }
        }

        // 2. 借助 uv python find 查找兼容版本（优先 3.12，其次 3.11/3.10）
        let uv_exe = uv.map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("uv"));
        for ver in ["3.12", "3.11", "3.10"] {
            let mut probe = Command::new(&uv_exe);
            apply_no_window(&mut probe);
            if let Ok(output) = probe.args(["python", "find", ver]).output() {
                if output.status.success() {
                    let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !path_str.is_empty() {
                        let p = PathBuf::from(&path_str);
                        if p.is_file() {
                            debug!(python = %p.display(), version = ver, "detected uv-managed python");
                            return Some(p);
                        }
                    }
                }
            }
        }

        debug!("no compatible python found in PATH or uv");
        None
    }

    /// 检测系统 PATH 中的 uv，并扫描 Windows 常见 Python Scripts 目录作为后备
    fn detect_uv() -> Option<PathBuf> {
        // 1. 尝试 PATH
        let mut probe = Command::new("uv");
        apply_no_window(&mut probe);
        if let Ok(output) = probe.arg("--version").output() {
            if output.status.success() {
                if let Some(path) = Self::which("uv") {
                    debug!(uv = %path.display(), "detected uv in PATH");
                    return Some(path);
                }
            }
        }

        // 2. Windows：扫描 C:\Program Files\Python*\Scripts\uv.exe
        #[cfg(windows)]
        {
            if let Some(path) = Self::scan_windows_uv() {
                debug!(uv = %path.display(), "detected uv in Python Scripts dir");
                return Some(path);
            }
        }

        debug!("no uv found in PATH");
        None
    }

    /// Windows 专用：在 Program Files 和用户 AppData 的 Python Scripts 目录中查找 uv.exe
    #[cfg(windows)]
    fn scan_windows_uv() -> Option<PathBuf> {
        let mut search_dirs: Vec<PathBuf> = Vec::new();

        // C:\Program Files\Python*\Scripts\
        if let Ok(entries) = std::fs::read_dir(r"C:\Program Files") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("Python") {
                    search_dirs.push(entry.path().join("Scripts"));
                }
            }
        }

        // %LOCALAPPDATA%\Programs\Python\Python*\Scripts\
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let base = PathBuf::from(local).join("Programs").join("Python");
            if let Ok(entries) = std::fs::read_dir(&base) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with("Python") {
                        search_dirs.push(entry.path().join("Scripts"));
                    }
                }
            }
        }

        for dir in search_dirs {
            let candidate = dir.join("uv.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    /// 判断配置值是否为裸命令名（如 `uv` / `python`，不含路径分隔符）。
    ///
    /// 裸命令名必须走 PATH 解析；直接 `Path::exists()` 会按相对当前目录语义
    /// 恒判不存在，误触发 "does not exist, falling back" 告警。
    fn is_bare_command_name(s: &str) -> bool {
        !s.contains('/') && !s.contains('\\')
    }

    /// 简易 which：在 PATH 环境变量中查找可执行文件
    fn which(name: &str) -> Option<PathBuf> {
        let path_var = std::env::var_os("PATH")?;

        // Windows 下需要追加 .exe 后缀
        let candidates: Vec<String> = if cfg!(windows) && !name.ends_with(".exe") {
            vec![format!("{name}.exe"), name.to_string()]
        } else {
            vec![name.to_string()]
        };

        for dir in std::env::split_paths(&path_var) {
            for exe_name in &candidates {
                let candidate = dir.join(exe_name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }

    /// 检测 python 和 uv 的可用性，返回详细状态
    pub fn check_prerequisites(&self) -> EnvCheckResult {
        let python = match &self.python_path {
            Some(path) => {
                let version = Self::get_python_version(path);
                ToolStatus::Found(path.clone(), version)
            }
            None => ToolStatus::NotFound {
                install_hint: Self::python_install_hint(),
            },
        };

        let uv = match &self.uv_path {
            Some(path) => {
                let version = Self::get_uv_version(path);
                ToolStatus::Found(path.clone(), version)
            }
            None => ToolStatus::NotFound {
                install_hint: Self::uv_install_hint(),
            },
        };

        EnvCheckResult { python, uv }
    }

    /// 获取 python 版本字符串
    fn get_python_version(path: &Path) -> String {
        let mut cmd = Command::new(path);
        apply_no_window(&mut cmd);
        match cmd.arg("--version").output() {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                // python 有时将版本输出到 stderr
                let version = if !stdout.trim().is_empty() {
                    stdout.trim().to_string()
                } else {
                    stderr.trim().to_string()
                };
                version
            }
            _ => "unknown".to_string(),
        }
    }

    /// 获取 uv 版本字符串
    fn get_uv_version(path: &Path) -> String {
        let mut cmd = Command::new(path);
        apply_no_window(&mut cmd);
        match cmd.arg("--version").output() {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            _ => "unknown".to_string(),
        }
    }

    /// 平台相关的 python 安装提示
    fn python_install_hint() -> String {
        if cfg!(windows) {
            "Python not found. Download it from https://www.python.org/downloads/ \
             and check \"Add Python to PATH\" during installation."
                .to_string()
        } else {
            "Python not found. Run: sudo apt install python3 python3-venv \
             (Debian/Ubuntu) or sudo dnf install python3 (Fedora)"
                .to_string()
        }
    }

    /// 平台相关的 uv 安装提示
    fn uv_install_hint() -> String {
        if cfg!(windows) {
            "uv not found. Download it from https://github.com/astral-sh/uv/releases, \
             or run: powershell -c \"irm https://astral.sh/uv/install.ps1 | iex\""
                .to_string()
        } else {
            "uv not found. Run: curl -LsSf https://astral.sh/uv/install.sh | sh"
                .to_string()
        }
    }

    /// 确保模块的虚拟环境就绪（旧单 venv 布局口径，行为不变）
    ///
    /// 流程（详见 [`Self::ensure_venv_impl`]）：
    /// 1. 构造 uv 子进程环境变量（网络代理 + `UV_CACHE_DIR` + `UV_PYTHON_INSTALL_DIR`，
    ///    §3.1 依赖栈统一 / LNX-03 托管解释器入包）
    /// 2. 检查 `runtime/venvs/<module_id>/` 是否存在
    /// 3. 不存在 → `uv venv --python <version> <path>`（注入上述 env）
    /// 4. 计算依赖哈希（requirements + constraints + link-mode 标记，P2-18）
    /// 5. 对比 `.ep_deps_hash`
    /// 6. 不一致 → `uv pip install -r <req> --python <venv_python> --link-mode hardlink [-c <constraints>]`
    ///    （constraints 仅当配置非空且文件存在时追加，文件不存在静默跳过）
    /// 7. 写入新哈希
    /// 8. 返回 venv 内 python 路径
    pub fn ensure_venv(
        &self,
        module_id: &str,
        python_version: &str,
        requirements: &Path,
    ) -> Result<PathBuf> {
        let venv_dir = self.venv_dir(module_id);
        self.ensure_venv_impl(module_id, python_version, requirements, &venv_dir, None)
    }

    /// 确保模块在指定后端维度的虚拟环境就绪（HETERO_DIST_PLAN M2/M3）。
    ///
    /// - 目标目录：`runtime/venvs/<module-id>--<backend>/`（分后端 venv，
    ///   多后端依赖分歧后每后端一套依赖栈）；
    /// - `.ep_deps_hash` 哈希输入加入 backend 名：各后端独立判定依赖栈变更，
    ///   跨后端切换绝不误判"依赖未变"；
    /// - **旧布局兼容读取**：新目录不存在而旧单 venv（`runtime/venvs/<id>/`）
    ///   存在且旧口径哈希匹配时，直接复用旧 venv 返回其解释器，避免全量重建；
    /// - 依赖文件由调用方按当前后端解析（[`crate::module::manifest::RuntimeConfig::
    ///   resolve_requirements`]，缺省回退 `runtime.requirements`）。
    pub fn ensure_venv_for_backend(
        &self,
        module_id: &str,
        python_version: &str,
        requirements: &Path,
        backend: ComputeBackend,
    ) -> Result<PathBuf> {
        let venv_dir = self.venv_dir_for_backend(module_id, backend);
        if !venv_dir.exists() && self.is_venv_ready(module_id, requirements) {
            debug!(
                module = module_id,
                backend = %backend,
                "reusing legacy single-venv layout"
            );
            return Ok(self.venv_python_path(module_id));
        }
        self.ensure_venv_impl(module_id, python_version, requirements, &venv_dir, Some(backend))
    }

    /// venv 创建 + 依赖安装核心流程（布局参数化：旧单 venv / 分后端共用）。
    ///
    /// `backend` 仅参与 `.ep_deps_hash` 哈希输入（None = 旧口径，与历史哈希逐字节兼容）。
    fn ensure_venv_impl(
        &self,
        module_id: &str,
        python_version: &str,
        requirements: &Path,
        venv_dir: &Path,
        backend: Option<ComputeBackend>,
    ) -> Result<PathBuf> {
        let uv = self
            .uv_path
            .as_ref()
            .context("uv not found, cannot create venv")?;

        let venv_python = self.python_in(venv_dir.to_path_buf());

        // 0. uv 子进程环境变量：网络代理 + UV_CACHE_DIR + UV_PYTHON_INSTALL_DIR
        //    （§3.1 / LNX-03）。缓存与托管解释器入应用根 → 与 venv 同盘 →
        //    硬链接去重生效，且解压目录自包含（托管 Python 不落 ~/.local/share/uv）
        let uv_env = self.build_uv_env(module_id);

        // 1. 创建 venv（如果不存在）
        //    created_now：本次新建的 venv 若安装失败须拆除（半壳 venv 只剩
        //    python 解释器，会让仅看 python.exe 存在性的调用方误判就绪）。
        let mut created_now = false;
        // 跨平台/损坏 venv 自愈：目录存在但解释器缺失（如 Windows 构建的 venv
        // 在 Linux 使用——只有 Lib/Scripts 无 bin/python；或上次安装失败残留的
        // 半壳），uv 无法在其上补装（--python 指向不存在的解释器必然失败且
        // 永远无法重试），删除后走下方重建路径。Windows 上 Scripts/python.exe
        // 存在，不受影响。
        if venv_dir.exists() && !venv_python.exists() {
            warn!(
                module = module_id,
                path = %venv_dir.display(),
                "venv python interpreter missing (cross-platform or incomplete venv), removing for rebuild"
            );
            match std::fs::remove_dir_all(venv_dir) {
                Ok(()) => {
                    info!(module = module_id, "removed incomplete venv, rebuilding");
                }
                Err(e) => {
                    warn!(
                        module = module_id,
                        path = %venv_dir.display(),
                        error = %e,
                        "failed to remove incomplete venv, rebuild will proceed if directory is absent"
                    );
                }
            }
        }
        if !venv_dir.exists() {
            info!(module = module_id, path = %venv_dir.display(), "creating venv");
            std::fs::create_dir_all(venv_dir).with_context(|| {
                format!("failed to create venv directory: {}", venv_dir.display())
            })?;

            // P1：`uv venv` 失败分支同样拆除半壳目录。此前 `?` 提前返回会
            // 留下 create_dir_all 已建好的空壳 venv——下次 `exists()` 恒真跳过
            // 创建，安装永远无法重试（永久卡死）。成功才保留目录。
            let output = match run_command_with_env(
                uv.to_str().unwrap_or("uv"),
                &[
                    "venv",
                    "--python",
                    python_version,
                    venv_dir.to_str().unwrap_or_default(),
                ],
                &uv_env,
            ) {
                Ok(output) => output,
                Err(e) => {
                    if let Err(rm_err) = std::fs::remove_dir_all(venv_dir) {
                        warn!(
                            module = module_id,
                            path = %venv_dir.display(),
                            error = %rm_err,
                            "failed to remove half-shell venv after 'uv venv' failure"
                        );
                    } else {
                        info!(
                            module = module_id,
                            "removed half-shell venv after 'uv venv' failure"
                        );
                    }
                    return Err(e).with_context(|| {
                        format!("failed to create venv for module '{module_id}'")
                    });
                }
            };
            created_now = true;
            debug!(module = module_id, output = %output, "venv created");
        } else {
            debug!(module = module_id, "venv already exists");
        }

        // 2. 检查依赖哈希
        if !requirements.exists() {
            debug!(module = module_id, "no requirements.txt, skipping dependency install");
            return Ok(venv_python);
        }

        // constraints：配置非空且文件存在才参与安装与哈希；不存在静默跳过（§3.1）
        let constraints_file = self.constraints_file();
        if self.constraints_path().is_some() && constraints_file.is_none() {
            debug!(
                module = module_id,
                path = %self.constraints_path().map(|p| p.display().to_string()).unwrap_or_default(),
                "constraints file not found, skipping"
            );
        }

        let current_hash =
            compute_deps_hash_seeded(requirements, constraints_file.as_deref(), backend)
                .with_context(|| {
                    format!("failed to hash dependency stack: {}", requirements.display())
                })?;

        let hash_file = venv_dir.join(DEPS_HASH_FILE_NAME);
        let needs_install = if hash_file.exists() {
            let stored = std::fs::read_to_string(&hash_file).unwrap_or_default();
            stored.trim() != current_hash
        } else {
            true
        };

        // 3. 安装/更新依赖（--link-mode hardlink：跨文件系统时 uv 内建自动回退 copy）
        if needs_install {
            info!(module = module_id, "installing dependencies");
            let venv_py_str = venv_python.to_str().unwrap_or_default();
            let req_str = requirements.to_str().unwrap_or_default();

            let mut install_args: Vec<&str> = vec![
                "pip",
                "install",
                "-r",
                req_str,
                "--python",
                venv_py_str,
                "--link-mode",
                UV_LINK_MODE,
                // 跨索引最优匹配：requirements 内联 --extra-index-url（如 torch
                // cu130）时，uv 默认 first-index 策略会把包锁死在首个命中的
                // 索引（cu130 索引残留的 packaging==24.0 即导致 deepfilternet
                // 解析无解），干净安装必现失败；索引均为受信源，放开跨索引
                // 取最优版本（与 deps.rs 指引文案推荐的策略一致）。
                "--index-strategy",
                "unsafe-best-match",
            ];
            let constraints_str;
            if let Some(c) = &constraints_file {
                constraints_str = c.to_string_lossy().into_owned();
                install_args.push("-c");
                install_args.push(&constraints_str);
            }

            let output = run_command_with_env(
                uv.to_str().unwrap_or("uv"),
                &install_args,
                &uv_env,
            );
            let output = match output {
                Ok(output) => output,
                Err(e) => {
                    // 本次新建的 venv 安装失败 → 拆除半壳，下次从零重来，
                    // 避免残留只有解释器的空 venv 误导就绪判定。
                    if created_now {
                        if let Err(rm_err) = std::fs::remove_dir_all(venv_dir) {
                            warn!(
                                module = module_id,
                                path = %venv_dir.display(),
                                error = %rm_err,
                                "failed to remove half-initialized venv"
                            );
                        } else {
                            info!(
                                module = module_id,
                                "removed half-initialized venv after install failure"
                            );
                        }
                    }
                    return Err(e).with_context(|| {
                        format!("failed to install dependencies for module '{module_id}'")
                    });
                }
            };
            debug!(module = module_id, output = %output, "dependencies installed");

            // 4. 写入新哈希
            if let Some(parent) = hash_file.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(&hash_file, &current_hash).with_context(|| {
                format!("failed to write deps hash: {}", hash_file.display())
            })?;
            debug!(module = module_id, hash = %current_hash, "deps hash updated");
        } else {
            debug!(module = module_id, "dependencies up-to-date");
        }

        Ok(venv_python)
    }

    /// uv 缓存目录绝对路径（配置为空 → None，即不注入 `UV_CACHE_DIR`）。
    ///
    /// 配置为相对路径时基于应用根解析；绝对路径原样返回（`Path::join` 语义）。
    pub fn uv_cache_dir_path(&self) -> Option<PathBuf> {
        if self.uv_cache_dir.is_empty() {
            None
        } else {
            Some(self.root.join(&self.uv_cache_dir))
        }
    }

    /// uv 托管 Python 解释器安装目录（LNX-03）：`<root>/runtime/uv-python`。
    ///
    /// 注入 `UV_PYTHON_INSTALL_DIR` 后，`uv venv --python ">=3.10,<3.13"` 等
    /// 自动下载的 CPython 落入应用根而非默认的 `~/.local/share/uv`，
    /// 与 `UV_CACHE_DIR` 同口径，保证解压目录自包含。
    pub fn uv_python_install_dir(&self) -> PathBuf {
        self.root.join("runtime").join("uv-python")
    }

    /// 构造 uv 子进程环境变量（LNX-03 提取，单测可覆盖）：
    /// 网络代理 + `UV_CACHE_DIR` + `UV_PYTHON_INSTALL_DIR`。
    ///
    /// 缓存/托管解释器目录不存在先创建；创建失败仅告警并回退 uv 默认位置，
    /// 不阻断 venv 创建主流程。
    fn build_uv_env(&self, module_id: &str) -> Vec<(String, String)> {
        let mut uv_env = self.network_env.clone();
        if let Some(cache_dir) = self.uv_cache_dir_path() {
            match std::fs::create_dir_all(&cache_dir) {
                Ok(()) => {
                    debug!(module = module_id, path = %cache_dir.display(), "using uv cache dir");
                    uv_env.push((
                        "UV_CACHE_DIR".to_string(),
                        cache_dir.to_string_lossy().into_owned(),
                    ));
                }
                Err(e) => {
                    warn!(
                        module = module_id,
                        path = %cache_dir.display(),
                        error = %e,
                        "failed to create uv cache dir, falling back to uv default cache"
                    );
                }
            }
        }
        let python_dir = self.uv_python_install_dir();
        match std::fs::create_dir_all(&python_dir) {
            Ok(()) => {
                debug!(module = module_id, path = %python_dir.display(), "using uv python install dir");
                uv_env.push((
                    "UV_PYTHON_INSTALL_DIR".to_string(),
                    python_dir.to_string_lossy().into_owned(),
                ));
            }
            Err(e) => {
                warn!(
                    module = module_id,
                    path = %python_dir.display(),
                    error = %e,
                    "failed to create uv python install dir, falling back to uv default location"
                );
            }
        }
        // uv 默认单请求超时 30s：torch 等 GB 级 wheel 在慢网/抖动链路下
        // 解压中途超时即整包失败（Linux 真机 E2E 实测 networkx 超时拖垮
        // deep-filter venv）。放宽到 300s，重试机制不变。
        uv_env.push(("UV_HTTP_TIMEOUT".to_string(), "300".to_string()));
        uv_env
    }

    /// constraints 文件配置路径绝对路径（配置为空 → None，即停用 constraints）。
    pub fn constraints_path(&self) -> Option<PathBuf> {
        if self.constraints.is_empty() {
            None
        } else {
            Some(self.root.join(&self.constraints))
        }
    }

    /// 当前生效的 constraints 文件：仅当配置非空**且文件存在**时返回路径（§3.1）。
    fn constraints_file(&self) -> Option<PathBuf> {
        self.constraints_path().filter(|p| p.is_file())
    }

    /// 获取模块 venv 内的 python 可执行文件路径（旧单 venv 布局口径）
    ///
    /// - Windows: `runtime/venvs/<id>/Scripts/python.exe`
    /// - Linux/macOS: `runtime/venvs/<id>/bin/python`
    pub fn venv_python_path(&self, module_id: &str) -> PathBuf {
        self.python_in(self.venv_dir(module_id))
    }

    /// 分后端 venv 内的 python 可执行文件路径（M3，旧布局兼容读取）：
    ///
    /// 1. 新布局 `runtime/venvs/<id>--<backend>/` 解释器存在 → 返回新布局；
    /// 2. 否则旧布局 `runtime/venvs/<id>/` 解释器存在 → 兼容返回旧布局
    ///    （避免全量重建；就绪判定请配合 [`Self::is_venv_ready_for_backend`]）；
    /// 3. 两者皆无解释器 → 返回新布局口径（前瞻性答案）。
    pub fn venv_python_path_for_backend(
        &self,
        module_id: &str,
        backend: ComputeBackend,
    ) -> PathBuf {
        let per_backend = self.python_in(self.venv_dir_for_backend(module_id, backend));
        if per_backend.exists() {
            return per_backend;
        }
        let legacy = self.venv_python_path(module_id);
        if legacy.exists() {
            legacy
        } else {
            per_backend
        }
    }

    /// venv 目录内的平台解释器路径（Windows `Scripts/python.exe`，其他 `bin/python`）
    fn python_in(&self, venv_dir: PathBuf) -> PathBuf {
        if cfg!(windows) {
            venv_dir.join("Scripts").join("python.exe")
        } else {
            venv_dir.join("bin").join("python")
        }
    }

    /// 检查模块 venv 是否就绪（存在且依赖哈希匹配；旧单 venv 布局口径）
    pub fn is_venv_ready(&self, module_id: &str, requirements: &Path) -> bool {
        self.is_ready_in(&self.venv_dir(module_id), requirements, None)
    }

    /// 检查模块在指定后端维度的 venv 是否就绪（M3，含旧布局兼容读取）：
    ///
    /// 分后端新布局就绪（backend 维度哈希匹配），**或**旧单 venv 按旧口径
    /// 哈希就绪 → true。
    pub fn is_venv_ready_for_backend(
        &self,
        module_id: &str,
        requirements: &Path,
        backend: ComputeBackend,
    ) -> bool {
        if self.is_ready_in(
            &self.venv_dir_for_backend(module_id, backend),
            requirements,
            Some(backend),
        ) {
            return true;
        }
        // 旧布局兼容读取：存在且旧口径哈希匹配则继续用
        self.is_ready_in(&self.venv_dir(module_id), requirements, None)
    }

    /// 就绪判定的目录级内核：解释器存在 + （无 requirements 文件 ∨ 哈希匹配）
    fn is_ready_in(
        &self,
        venv_dir: &Path,
        requirements: &Path,
        backend: Option<ComputeBackend>,
    ) -> bool {
        let venv_python = self.python_in(venv_dir.to_path_buf());
        if !venv_python.exists() {
            return false;
        }

        // 无 requirements 文件时，venv 存在即就绪
        if !requirements.exists() {
            return true;
        }

        let hash_file = venv_dir.join(DEPS_HASH_FILE_NAME);
        if !hash_file.exists() {
            return false;
        }

        // 与 ensure_venv_impl 保持同一哈希口径（requirements + constraints +
        // link-mode 标记，P2-18；分后端布局额外加入 backend 名，M3）
        let current_hash = match compute_deps_hash_seeded(
            requirements,
            self.constraints_file().as_deref(),
            backend,
        ) {
            Ok(h) => h,
            Err(_) => return false,
        };

        let stored = std::fs::read_to_string(&hash_file).unwrap_or_default();
        stored.trim() == current_hash
    }

    /// venv 目录路径（旧单 venv 布局）: `runtime/venvs/<module_id>/`
    fn venv_dir(&self, module_id: &str) -> PathBuf {
        self.root.join("runtime").join("venvs").join(module_id)
    }

    /// 分后端 venv 目录路径（M3）: `runtime/venvs/<module-id>--<backend>/`
    ///
    /// 多后端依赖分歧后每模块每后端一个 venv；`--<backend>` 后缀取后端
    /// 小写名（如 `faster-whisper--cuda`），与 module-id 的合法字符集
    /// （小写字母/数字/连字符）不冲突。
    pub fn venv_dir_for_backend(&self, module_id: &str, backend: ComputeBackend) -> PathBuf {
        self.root
            .join("runtime")
            .join("venvs")
            .join(format!("{module_id}--{backend}"))
    }

    /// 获取检测到的 python 路径
    pub fn python_path(&self) -> Option<&Path> {
        self.python_path.as_deref()
    }

    /// 获取检测到的 uv 路径
    pub fn uv_path(&self) -> Option<&Path> {
        self.uv_path.as_deref()
    }

    /// 获取模块 venv 的详细状态
    pub fn get_venv_status(&self, module_id: &str) -> VenvStatus {
        let venv_python = self.venv_python_path(module_id);
        if !venv_python.exists() {
            return VenvStatus::NotExist;
        }

        let venv_dir = self.venv_dir(module_id);
        let hash_file = venv_dir.join(DEPS_HASH_FILE_NAME);

        // 无 hash 文件时，检查是否有 requirements 需要安装
        if !hash_file.exists() {
            // 如果 venv 存在但无 hash 文件，视为需要更新
            // （除非没有 requirements 文件，但这里无法判断，保守返回 NeedsUpdate）
            return VenvStatus::NeedsUpdate;
        }

        // hash 文件存在，但无法判断是否需要更新（需要 requirements 路径）
        // 简化实现：venv 存在且 hash 文件存在即视为 Ready
        VenvStatus::Ready
    }

    /// 批量检查所有模块的环境就绪状态
    pub fn check_all_modules_env(
        &self,
        modules: &[crate::module::discovery::DiscoveredModule],
    ) -> std::collections::HashMap<String, bool> {
        let mut result = std::collections::HashMap::new();
        for module in modules {
            if let Some(manifest) = &module.manifest {
                let module_id = &manifest.module.id;
                let req_path = module.path.join(
                    manifest
                        .runtime
                        .requirements
                        .as_deref()
                        .unwrap_or("requirements.txt"),
                );
                let ready = self.is_venv_ready(module_id, &req_path);
                result.insert(module_id.clone(), ready);
            }
        }
        result
    }
}

// ─── 辅助函数 ────────────────────────────────────────────────────────────────

/// `uv pip install` 的 link-mode（§3.1 依赖栈统一）。
///
/// hardlink：缓存与 venv 同盘时硬链接去重；跨文件系统时 uv 内建自动回退 copy。
pub const UV_LINK_MODE: &str = "hardlink";

/// 依赖哈希文件名（venv 目录内，旧/新布局通用）
const DEPS_HASH_FILE_NAME: &str = ".ep_deps_hash";

/// 依赖哈希版本标记（P2-18）：把 link-mode 策略纳入哈希输入。
///
/// 标记或 link-mode 策略变化会使旧 `.ep_deps_hash` 失配，触发全量重装。
/// 注意：须与 [`UV_LINK_MODE`] 保持一致（`deps_hash_marker_covers_link_mode` 测试把关）。
const DEPS_HASH_MARKER: &str = "ep-deps-hash:v2,link-mode=hardlink";

/// 计算依赖栈哈希（P2-18 扩展；旧单 venv 布局口径，不含 backend 名）
///
/// 哈希输入 = requirements.txt 字节 + constraints 文件字节（若提供且存在）+ link-mode 版本标记。
/// 任一输入变化（requirements 变更 / constraints 变更 / constraints 增删 / 策略标记变化）
/// 都会使 `.ep_deps_hash` 失配，触发依赖重装。
///
/// **注意：** 使用 `DefaultHasher`（SipHash-1-3），非加密安全。
/// 仅用于检测依赖栈是否变更，不用于安全校验。
/// 输出格式：`hash:<16位十六进制>`
pub fn compute_deps_hash(requirements: &Path, constraints: Option<&Path>) -> Result<String> {
    compute_deps_hash_seeded(requirements, constraints, None)
}

/// 计算依赖栈哈希——分后端口径（M3）：在 [`compute_deps_hash`] 的输入基础上
/// 额外混入 backend 名。
///
/// 分后端 venv 各自持有 `.ep_deps_hash`，同一模块不同后端的依赖栈互不可替
/// （cuda 与 rocm 的 wheel 不同的场景下，跨后端复用哈希会误判"依赖未变"）。
pub fn compute_deps_hash_for_backend(
    requirements: &Path,
    constraints: Option<&Path>,
    backend: ComputeBackend,
) -> Result<String> {
    compute_deps_hash_seeded(requirements, constraints, Some(backend))
}

/// 哈希内核：requirements + constraints? [+ backend 名] + 版本标记。
///
/// backend 名插入在 constraints 与版本标记之间——None 时输入流与历史公式
/// 完全一致，保证既有 `.ep_deps_hash` 逐字节兼容（不触发无谓重装）。
fn compute_deps_hash_seeded(
    requirements: &Path,
    constraints: Option<&Path>,
    backend: Option<ComputeBackend>,
) -> Result<String> {
    let mut hasher = DefaultHasher::new();

    let req_bytes = std::fs::read(requirements).with_context(|| {
        format!(
            "failed to read requirements file for hashing: {}",
            requirements.display()
        )
    })?;
    req_bytes.hash(&mut hasher);

    // constraints 仅在文件存在时参与哈希；不存在静默跳过（与安装参数口径一致，§3.1）
    if let Some(path) = constraints {
        if path.is_file() {
            let bytes = std::fs::read(path).with_context(|| {
                format!(
                    "failed to read constraints file for hashing: {}",
                    path.display()
                )
            })?;
            bytes.hash(&mut hasher);
        }
    }

    // M3：分后端布局的哈希输入加入 backend 名（小写 Display 形态）
    if let Some(b) = backend {
        b.to_string().hash(&mut hasher);
    }

    DEPS_HASH_MARKER.hash(&mut hasher);
    let hash_value = hasher.finish();
    Ok(format!("hash:{hash_value:016x}"))
}

/// 计算单个文件内容的哈希值（简化实现）
///
/// **注意：** 使用 `DefaultHasher`（SipHash-1-3），非加密安全。
/// 仅用于变更检测，不用于安全校验。依赖栈哈希请使用 [`compute_deps_hash`]。
/// 输出格式：`hash:<16位十六进制>`
pub fn hash_file(path: &Path) -> Result<String> {
    let content = std::fs::read(path)
        .with_context(|| format!("failed to read file for hashing: {}", path.display()))?;
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    let hash_value = hasher.finish();
    Ok(format!("hash:{hash_value:016x}"))
}

/// 执行外部命令，捕获 stdout 输出
///
/// 成功时返回 stdout 内容，失败时返回错误（含 stderr 信息）。
pub fn run_command(cmd: &str, args: &[&str]) -> Result<String> {
    run_command_with_env(cmd, args, &[])
}

/// 单条命令超时上限：uv venv / uv pip install 联网任务可合法耗时数分钟
/// （torch 等大包下载），故取 3600s 保守值——只兜底真正挂死的子进程
/// （网络黑洞/进程锁死），不误伤长耗时安装。无上限时"永不返回"会无限
/// 阻塞调用方（含 daemon 的 async worker）。
const COMMAND_TIMEOUT: Duration = Duration::from_secs(3600);

/// 捕获式执行子进程，带超时 kill（P2）。
///
/// 替代 `Command::output()` 的无限阻塞：轮询 `try_wait`，超时后
/// `kill + wait` 并以错误返回。stdout/stderr 由后台线程并发读取，
/// 避免管道写满时子进程与读取方互锁。
fn run_command_captured(
    command: &mut Command,
    cmd_name: &str,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>)> {
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to execute command: {cmd_name}"))?;

    let out_reader = child.stdout.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });
    let err_reader = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    // 子进程已终止、管道关闭，读取线程自然收尾，join 兜底回收
                    let _ = out_reader.map(|h| h.join());
                    let _ = err_reader.map(|h| h.join());
                    bail!("command '{cmd_name}' timed out after {}s", timeout.as_secs());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e).with_context(|| format!("failed to wait for command: {cmd_name}"));
            }
        }
    };

    let stdout = out_reader.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = err_reader.and_then(|h| h.join().ok()).unwrap_or_default();
    Ok((status, stdout, stderr))
}

/// 执行外部命令并注入额外环境变量（仅注入非空值），捕获 stdout 输出
///
/// 用于给 uv/pip 等联网子进程注入代理环境变量。
/// 成功时返回 stdout 内容，失败时返回错误（含 stderr 信息）。
/// P2：带超时兜底（见 [`COMMAND_TIMEOUT`]），挂死命令不再无限阻塞调用方。
pub fn run_command_with_env(
    cmd: &str,
    args: &[&str],
    extra_env: &[(String, String)],
) -> Result<String> {
    debug!(cmd = cmd, args = ?args, "executing command");

    let mut command = Command::new(cmd);
    // 探测/安装子进程不弹控制台窗口（Windows），失败走调用方降级分支
    apply_no_window(&mut command);
    command.args(args);
    for (key, value) in extra_env {
        if !value.is_empty() {
            command.env(key, value);
        }
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let (status, stdout_bytes, stderr_bytes) =
        run_command_captured(&mut command, cmd, COMMAND_TIMEOUT)?;

    let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
    let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();

    if !status.success() {
        bail!(
            "command '{}' failed with exit code {:?}\nstderr: {}",
            cmd,
            status.code(),
            stderr.trim()
        );
    }

    if !stderr.trim().is_empty() {
        debug!(cmd = cmd, stderr = %stderr.trim(), "command stderr (non-fatal)");
    }

    Ok(stdout)
}

// ─── 单元测试 ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_bare_command_name_classification() {
        assert!(EnvManager::is_bare_command_name("uv"));
        assert!(EnvManager::is_bare_command_name("python"));
        assert!(!EnvManager::is_bare_command_name("C:\\uv\\uv.exe"));
        assert!(!EnvManager::is_bare_command_name("/usr/bin/uv"));
        assert!(!EnvManager::is_bare_command_name("tools/uv"));
    }

    #[test]
    fn new_with_bare_command_names_resolves_via_path() {
        // 本机 PATH 同时含 python 与 uv（安装前置条件）；
        // 裸命令名配置不得触发 exists() 回退，而应解析为 PATH 中的真实可执行文件。
        if EnvManager::which("python").is_none() || EnvManager::which("uv").is_none() {
            return;
        }
        let config = PythonConfig {
            path: "python".to_string(),
            uv_path: "uv".to_string(),
            ..Default::default()
        };
        let mgr = EnvManager::new(Path::new("/fake/root"), &config);
        let uv = mgr.uv_path().expect("uv should resolve via PATH");
        assert!(
            uv.to_string_lossy().to_lowercase().contains("uv"),
            "resolved uv path unexpected: {}",
            uv.display()
        );
        assert!(
            mgr.python_path().is_some(),
            "python should resolve via PATH for bare command name"
        );
    }

    #[test]
    fn venv_python_path_windows_style() {
        // 测试路径构造逻辑（不依赖实际平台）
        let root = PathBuf::from("/fake/root");
        let mgr = EnvManager {
            root,
            python_path: None,
            uv_path: None,
            network_env: Vec::new(),
            uv_cache_dir: String::new(),
            constraints: String::new(),
        };

        let path = mgr.venv_python_path("test-module");

        if cfg!(windows) {
            assert!(
                path.ends_with(r"runtime\venvs\test-module\Scripts\python.exe"),
                "unexpected path: {}",
                path.display()
            );
        } else {
            assert!(
                path.ends_with("runtime/venvs/test-module/bin/python"),
                "unexpected path: {}",
                path.display()
            );
        }
    }

    #[test]
    fn venv_python_path_contains_module_id() {
        let root = PathBuf::from("/app");
        let mgr = EnvManager {
            root,
            python_path: None,
            uv_path: None,
            network_env: Vec::new(),
            uv_cache_dir: String::new(),
            constraints: String::new(),
        };

        let path = mgr.venv_python_path("faster-whisper");
        let path_str = path.to_str().unwrap();
        assert!(
            path_str.contains("faster-whisper"),
            "path should contain module id: {path_str}"
        );
        assert!(
            path_str.contains("runtime"),
            "path should be under runtime/: {path_str}"
        );
    }

    #[test]
    fn is_venv_ready_nonexistent_returns_false() {
        let root = std::env::temp_dir().join(format!("ep_env_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let mgr = EnvManager {
            root: root.clone(),
            python_path: None,
            uv_path: None,
            network_env: Vec::new(),
            uv_cache_dir: String::new(),
            constraints: String::new(),
        };

        // venv 不存在时应返回 false
        let req = root.join("requirements.txt");
        assert!(
            !mgr.is_venv_ready("nonexistent-module", &req),
            "should return false for non-existent venv"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 半壳 venv 回归（E2E 任务 #10）：只有解释器、未装依赖的 venv 不得误判就绪 ──

    /// P1 回归：`uv venv` 失败必须拆除 create_dir_all 已建好的半壳目录，
    /// 否则下次 `exists()` 恒真跳过创建 → 安装永久卡死。
    #[test]
    fn uv_venv_failure_removes_half_shell_dir() {
        let root = std::env::temp_dir().join(format!("ep_uv_fail_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        // uv_path 指向必然 spawn 失败的路径 → ensure_venv 走 uv venv 错误路径
        let mgr = EnvManager {
            root: root.clone(),
            python_path: None,
            uv_path: Some(PathBuf::from("/nonexistent/uv-binary-for-test")),
            network_env: Vec::new(),
            uv_cache_dir: String::new(),
            constraints: String::new(),
        };

        let module_id = "mod-uv-fail";
        let req = root.join("requirements.txt");
        std::fs::write(&req, "fastapi\n").unwrap();

        // 注意：不预建 venv_dir——半壳目录由 ensure_venv 内部 create_dir_all
        // 创建（uv venv 执行前的现场）；uv venv 失败后该目录必须被拆除。
        let venv_dir = root.join("runtime").join("venvs").join(module_id);
        assert!(mgr.ensure_venv(module_id, "3.12", &req).is_err());
        assert!(
            !venv_dir.exists(),
            "uv venv 失败后由 ensure_venv 创建的半壳目录必须被拆除: {}",
            venv_dir.display()
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// P2 回归：挂死命令在超时后被 kill，调用方拿到错误而非无限阻塞
    #[test]
    fn run_command_timeout_returns_error_not_hang() {
        // 故意跑得比超时长 → 应快速返回 Err（而非永久阻塞）
        let (program, args): (&str, Vec<&str>) = if cfg!(windows) {
            ("cmd", vec!["/C", "ping", "-n", "3", "127.0.0.1"])
        } else {
            ("sleep", vec!["5"])
        };
        let start = Instant::now();
        let result = run_command_captured(
            Command::new(program).args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()),
            program,
            Duration::from_millis(150),
        );
        let elapsed = start.elapsed();
        assert!(result.is_err(), "超时应返回 Err");
        assert!(
            elapsed < Duration::from_secs(3),
            "超时后必须快速返回，实际耗时 {:?}",
            elapsed
        );
        assert!(
            result.unwrap_err().to_string().contains("timed out"),
            "错误信息应说明超时"
        );
    }

    #[test]
    fn is_venv_ready_half_shell_returns_false() {
        let root = std::env::temp_dir().join(format!("ep_env_half_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let mgr = EnvManager {
            root: root.clone(),
            python_path: None,
            uv_path: None,
            network_env: Vec::new(),
            uv_cache_dir: String::new(),
            constraints: String::new(),
        };

        let module_id = "half-shell-mod";
        // 半壳 venv：预置假 python 解释器（文件存在但从未安装依赖）
        let py = mgr.venv_python_path(module_id);
        std::fs::create_dir_all(py.parent().unwrap()).unwrap();
        std::fs::write(&py, b"fake").unwrap();
        // requirements 存在
        let req = root.join("modules").join(module_id).join("requirements.txt");
        std::fs::create_dir_all(req.parent().unwrap()).unwrap();
        std::fs::write(&req, "fastapi>=0.100.0\n").unwrap();

        // 1) 无哈希文件（半壳）→ 不得误判就绪
        assert!(
            !mgr.is_venv_ready(module_id, &req),
            "半壳 venv（有解释器、无哈希）必须判定未就绪"
        );

        // 2) 写入匹配哈希 → 就绪
        let hash = compute_deps_hash(&req, None).unwrap();
        std::fs::write(mgr.venv_dir(module_id).join(DEPS_HASH_FILE_NAME), &hash).unwrap();
        assert!(mgr.is_venv_ready(module_id, &req));

        // 3) requirements 变更 → 哈希失配 → 未就绪
        std::fs::write(&req, "fastapi>=0.110.0\n").unwrap();
        assert!(!mgr.is_venv_ready(module_id, &req));

        // 4) 无 requirements 文件 → venv 存在即就绪（保持旧语义）
        let no_req = root.join("modules").join(module_id).join("no-such-req.txt");
        assert!(mgr.is_venv_ready(module_id, &no_req));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hash_file_deterministic() {
        let dir = std::env::temp_dir().join(format!("ep_hash_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let file = dir.join("test.txt");
        std::fs::write(&file, "fastapi>=0.100.0\nuvicorn>=0.23.0\n").unwrap();

        let h1 = hash_file(&file).unwrap();
        let h2 = hash_file(&file).unwrap();
        assert_eq!(h1, h2, "same content should produce same hash");
        assert!(h1.starts_with("hash:"), "hash should have prefix");

        // 修改内容后哈希应变化
        std::fs::write(&file, "fastapi>=0.100.0\nuvicorn>=0.23.0\nnumpy\n").unwrap();
        let h3 = hash_file(&file).unwrap();
        assert_ne!(h1, h3, "different content should produce different hash");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hash_file_nonexistent_returns_error() {
        let result = hash_file(Path::new("/nonexistent/path/file.txt"));
        assert!(result.is_err(), "should fail for non-existent file");
    }

    // ── 依赖栈哈希（P2-18）：requirements + constraints + link-mode 标记 ──

    /// 测试夹具：创建临时目录 + requirements + constraints 文件
    fn deps_hash_fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ep_deps_hash_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let req = dir.join("requirements.txt");
        std::fs::write(&req, "torch\ntorchaudio\n").unwrap();

        let cons = dir.join("constraints.txt");
        std::fs::write(&cons, "torch==2.13.0\ntorchaudio==2.13.0\n").unwrap();

        (dir, req, cons)
    }

    #[test]
    fn deps_hash_deterministic() {
        let (dir, req, cons) = deps_hash_fixture("det");

        let h1 = compute_deps_hash(&req, Some(&cons)).unwrap();
        let h2 = compute_deps_hash(&req, Some(&cons)).unwrap();
        assert_eq!(h1, h2, "same inputs should produce same hash");
        assert!(h1.starts_with("hash:"), "hash should have prefix");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 变更场景 1：requirements.txt 变更 → 哈希变化（触发重装）
    #[test]
    fn deps_hash_requirements_change() {
        let (dir, req, cons) = deps_hash_fixture("reqchg");

        let h1 = compute_deps_hash(&req, Some(&cons)).unwrap();
        std::fs::write(&req, "torch\ntorchaudio\nnumpy\n").unwrap();
        let h2 = compute_deps_hash(&req, Some(&cons)).unwrap();
        assert_ne!(h1, h2, "requirements change must change the hash");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 变更场景 2：constraints 内容变更 → 哈希变化（触发重装）
    #[test]
    fn deps_hash_constraints_change() {
        let (dir, req, cons) = deps_hash_fixture("conschg");

        let h1 = compute_deps_hash(&req, Some(&cons)).unwrap();
        std::fs::write(&cons, "torch==2.14.0\ntorchaudio==2.14.0\n").unwrap();
        let h2 = compute_deps_hash(&req, Some(&cons)).unwrap();
        assert_ne!(h1, h2, "constraints change must change the hash");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 变更场景 3：constraints 文件增删 → 哈希变化；不存在的 constraints 静默跳过
    #[test]
    fn deps_hash_constraints_added_or_removed() {
        let (dir, req, cons) = deps_hash_fixture("consaddrem");

        let h_none = compute_deps_hash(&req, None).unwrap();

        // 传入不存在的路径 → 静默跳过 → 与无 constraints 等价
        let absent = dir.join("no-such-constraints.txt");
        let h_absent = compute_deps_hash(&req, Some(&absent)).unwrap();
        assert_eq!(h_none, h_absent, "absent constraints file must be skipped silently");

        // constraints 存在 → 哈希不同
        let h_with = compute_deps_hash(&req, Some(&cons)).unwrap();
        assert_ne!(h_none, h_with, "adding constraints must change the hash");

        // 删除 constraints → 回到无 constraints 哈希
        std::fs::remove_file(&cons).unwrap();
        let h_removed = compute_deps_hash(&req, Some(&cons)).unwrap();
        assert_eq!(h_none, h_removed, "removing constraints must restore the old hash");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deps_hash_marker_covers_link_mode() {
        assert_eq!(UV_LINK_MODE, "hardlink");
        assert!(
            DEPS_HASH_MARKER.contains(UV_LINK_MODE),
            "hash marker must embed the link-mode so strategy changes force reinstall"
        );
    }

    #[test]
    fn deps_hash_requirements_missing_returns_error() {
        let result = compute_deps_hash(Path::new("/nonexistent/requirements.txt"), None);
        assert!(result.is_err(), "should fail for non-existent requirements");
    }

    // ── EnvManager 依赖栈配置解析（§3.1）─────────────────────────────────

    #[test]
    fn env_manager_new_reads_dep_stack_config() {
        // §8.3 默认值
        let cfg = PythonConfig::default();
        assert_eq!(cfg.uv_cache_dir, "runtime/.uv-cache");
        assert_eq!(cfg.constraints, "config/constraints.txt");

        let root = std::env::temp_dir().join(format!("ep_envmgr_new_{}", std::process::id()));
        let cfg = PythonConfig {
            uv_cache_dir: "cache/uv".into(),
            constraints: "config/cons.txt".into(),
            ..Default::default()
        };

        let mgr = EnvManager::new(&root, &cfg);
        assert_eq!(mgr.uv_cache_dir_path(), Some(root.join("cache").join("uv")));
        assert_eq!(
            mgr.constraints_path(),
            Some(root.join("config").join("cons.txt"))
        );
        // constraints 文件不存在 → constraints_file 静默返回 None
        assert_eq!(mgr.constraints_file(), None);
    }

    #[test]
    fn dep_stack_paths_empty_config_disabled() {
        let mgr = EnvManager {
            root: PathBuf::from("/fake/root"),
            python_path: None,
            uv_path: None,
            network_env: Vec::new(),
            uv_cache_dir: String::new(),
            constraints: String::new(),
        };
        assert_eq!(mgr.uv_cache_dir_path(), None, "empty uv_cache_dir = no injection");
        assert_eq!(mgr.constraints_path(), None, "empty constraints = disabled");
        assert_eq!(mgr.constraints_file(), None);
    }

    #[test]
    fn dep_stack_paths_absolute_passthrough() {
        let (root_str, abs_str) = if cfg!(windows) {
            ("G:/EntryPoint", "D:/shared/uv-cache")
        } else {
            ("/opt/entrypoint", "/srv/uv-cache")
        };
        let mgr = EnvManager {
            root: PathBuf::from(root_str),
            python_path: None,
            uv_path: None,
            network_env: Vec::new(),
            uv_cache_dir: abs_str.to_string(),
            constraints: abs_str.to_string(),
        };
        // Path::join 语义：绝对路径原样返回
        assert_eq!(mgr.uv_cache_dir_path(), Some(PathBuf::from(abs_str)));
        assert_eq!(mgr.constraints_path(), Some(PathBuf::from(abs_str)));
    }

    // ── LNX-03：uv 托管 Python 解释器落包内（UV_PYTHON_INSTALL_DIR 注入） ──

    #[test]
    fn uv_python_install_dir_under_root() {
        let mgr = EnvManager {
            root: PathBuf::from("/opt/entrypoint"),
            python_path: None,
            uv_path: None,
            network_env: Vec::new(),
            uv_cache_dir: String::new(),
            constraints: String::new(),
        };
        assert_eq!(
            mgr.uv_python_install_dir(),
            PathBuf::from("/opt/entrypoint/runtime/uv-python")
        );
    }

    /// uv 子进程 env 必须同时携带 UV_CACHE_DIR 与 UV_PYTHON_INSTALL_DIR
    ///（值指向应用根内），网络代理变量原样透传——解压目录自包含的判定锚点。
    #[test]
    fn build_uv_env_injects_cache_and_python_install_dir() {
        let root = std::env::temp_dir().join(format!("ep_uv_env_build_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let mgr = EnvManager {
            root: root.clone(),
            python_path: None,
            uv_path: None,
            network_env: vec![(
                "HTTP_PROXY".to_string(),
                "http://127.0.0.1:7890".to_string(),
            )],
            uv_cache_dir: "runtime/.uv-cache".to_string(),
            constraints: String::new(),
        };

        let env = mgr.build_uv_env("mod-env");
        let get = |key: &str| env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        assert_eq!(
            get("UV_CACHE_DIR").unwrap(),
            root.join("runtime").join(".uv-cache").to_string_lossy()
        );
        assert_eq!(
            get("UV_PYTHON_INSTALL_DIR").unwrap(),
            root.join("runtime").join("uv-python").to_string_lossy()
        );
        assert_eq!(get("HTTP_PROXY").unwrap(), "http://127.0.0.1:7890");
        // GB 级 wheel 慢网硬化：单请求超时放宽至 300s（默认 30s 易整包失败）
        assert_eq!(get("UV_HTTP_TIMEOUT").unwrap(), "300");
        // 注入前目录已创建（uv 无需自行建父目录）
        assert!(root.join("runtime").join("uv-python").is_dir());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 端到端：constraints 变更使 is_venv_ready 失配 → 触发重装判定（P2-18）
    #[test]
    fn is_venv_ready_constraints_change_triggers_reinstall() {
        let root = std::env::temp_dir().join(format!("ep_deps_ready_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        // 伪造 venv python
        let venv_dir = root.join("runtime").join("venvs").join("mod-x");
        let bin_dir = if cfg!(windows) {
            venv_dir.join("Scripts")
        } else {
            venv_dir.join("bin")
        };
        std::fs::create_dir_all(&bin_dir).unwrap();
        let py_name = if cfg!(windows) { "python.exe" } else { "python" };
        std::fs::write(bin_dir.join(py_name), "fake").unwrap();

        let req = root.join("requirements.txt");
        std::fs::write(&req, "torch\n").unwrap();
        let cons = root.join("config").join("constraints.txt");
        std::fs::create_dir_all(cons.parent().unwrap()).unwrap();
        std::fs::write(&cons, "torch==2.13.0\n").unwrap();

        let mgr = EnvManager {
            root: root.clone(),
            python_path: None,
            uv_path: None,
            network_env: Vec::new(),
            uv_cache_dir: String::new(),
            constraints: "config/constraints.txt".to_string(),
        };

        // 写入与安装口径一致的哈希 → 就绪
        let hash = compute_deps_hash(&req, mgr.constraints_file().as_deref()).unwrap();
        std::fs::write(mgr.venv_dir("mod-x").join(DEPS_HASH_FILE_NAME), &hash).unwrap();
        assert!(mgr.is_venv_ready("mod-x", &req), "fresh hash should be ready");

        // constraints 内容变更 → 需要重装
        std::fs::write(&cons, "torch==2.14.0\n").unwrap();
        assert!(
            !mgr.is_venv_ready("mod-x", &req),
            "constraints change must invalidate readiness"
        );

        // 重装完成（重写哈希）→ 就绪；requirements 变更 → 再次需要重装
        let hash2 = compute_deps_hash(&req, mgr.constraints_file().as_deref()).unwrap();
        std::fs::write(mgr.venv_dir("mod-x").join(DEPS_HASH_FILE_NAME), &hash2).unwrap();
        assert!(mgr.is_venv_ready("mod-x", &req));
        std::fs::write(&req, "torch\nnumpy\n").unwrap();
        assert!(
            !mgr.is_venv_ready("mod-x", &req),
            "requirements change must invalidate readiness"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn run_command_success() {
        // 使用跨平台的简单命令测试
        if cfg!(windows) {
            let result = run_command("cmd", &["/C", "echo", "hello"]);
            assert!(result.is_ok());
            assert!(result.unwrap().contains("hello"));
        } else {
            let result = run_command("echo", &["hello"]);
            assert!(result.is_ok());
            assert!(result.unwrap().contains("hello"));
        }
    }

    #[test]
    fn run_command_failure() {
        if cfg!(windows) {
            let result = run_command("cmd", &["/C", "exit", "1"]);
            assert!(result.is_err());
        } else {
            let result = run_command("false", &[]);
            assert!(result.is_err());
        }
    }

    #[test]
    fn tool_status_is_found() {
        let found = ToolStatus::Found(PathBuf::from("/usr/bin/python3"), "Python 3.12".into());
        assert!(found.is_found());

        let not_found = ToolStatus::NotFound {
            install_hint: "install it".into(),
        };
        assert!(!not_found.is_found());
    }

    #[test]
    fn env_check_result_all_ready() {
        let ready = EnvCheckResult {
            python: ToolStatus::Found(PathBuf::from("/usr/bin/python3"), "3.12".into()),
            uv: ToolStatus::Found(PathBuf::from("/usr/bin/uv"), "0.4.0".into()),
        };
        assert!(ready.all_ready());

        let missing_uv = EnvCheckResult {
            python: ToolStatus::Found(PathBuf::from("/usr/bin/python3"), "3.12".into()),
            uv: ToolStatus::NotFound {
                install_hint: "install uv".into(),
            },
        };
        assert!(!missing_uv.all_ready());
    }

    // ── VenvStatus tests ──────────────────────────────────────────────────

    #[test]
    fn test_venv_status_not_exist() {
        let root = std::env::temp_dir()
            .join(format!("ep_venv_stat_ne_{}_{}", std::process::id(), 1));
        let _ = std::fs::remove_dir_all(&root);

        let mgr = EnvManager {
            root: root.clone(),
            python_path: None,
            uv_path: None,
            network_env: Vec::new(),
            uv_cache_dir: String::new(),
            constraints: String::new(),
        };

        assert_eq!(mgr.get_venv_status("nonexistent"), VenvStatus::NotExist);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_venv_status_ready() {
        let root = std::env::temp_dir()
            .join(format!("ep_venv_stat_r_{}_{}", std::process::id(), 2));
        let _ = std::fs::remove_dir_all(&root);

        // 创建 venv python 和 hash 文件
        let venv_dir = root.join("runtime").join("venvs").join("test-mod");
        let bin_dir = if cfg!(windows) {
            venv_dir.join("Scripts")
        } else {
            venv_dir.join("bin")
        };
        std::fs::create_dir_all(&bin_dir).unwrap();
        let py_name = if cfg!(windows) {
            "python.exe"
        } else {
            "python"
        };
        std::fs::write(bin_dir.join(py_name), "fake").unwrap();
        // 写入 hash 文件
        std::fs::write(venv_dir.join(".ep_deps_hash"), "hash:abc").unwrap();

        let mgr = EnvManager {
            root: root.clone(),
            python_path: None,
            uv_path: None,
            network_env: Vec::new(),
            uv_cache_dir: String::new(),
            constraints: String::new(),
        };

        assert_eq!(mgr.get_venv_status("test-mod"), VenvStatus::Ready);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_venv_status_needs_update() {
        let root = std::env::temp_dir()
            .join(format!("ep_venv_stat_nu_{}_{}", std::process::id(), 3));
        let _ = std::fs::remove_dir_all(&root);

        // 创建 venv python 但无 hash 文件
        let venv_dir = root.join("runtime").join("venvs").join("test-mod");
        let bin_dir = if cfg!(windows) {
            venv_dir.join("Scripts")
        } else {
            venv_dir.join("bin")
        };
        std::fs::create_dir_all(&bin_dir).unwrap();
        let py_name = if cfg!(windows) {
            "python.exe"
        } else {
            "python"
        };
        std::fs::write(bin_dir.join(py_name), "fake").unwrap();
        // 不写 hash 文件

        let mgr = EnvManager {
            root: root.clone(),
            python_path: None,
            uv_path: None,
            network_env: Vec::new(),
            uv_cache_dir: String::new(),
            constraints: String::new(),
        };

        assert_eq!(mgr.get_venv_status("test-mod"), VenvStatus::NeedsUpdate);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_check_all_modules_env() {
        use crate::module::discovery::{DiscoveredModule, DiscoveryStatus};
        use crate::module::manifest::*;
        use crate::types::{ComputeBackend, ModuleCategory};

        let root = std::env::temp_dir()
            .join(format!("ep_check_all_env_{}_{}", std::process::id(), 4));
        let _ = std::fs::remove_dir_all(&root);

        // Module A: venv 就绪（无 requirements，python 存在即可）
        let mod_a_dir = root.join("modules").join("mod-a");
        std::fs::create_dir_all(&mod_a_dir).unwrap();
        let venv_a = root.join("runtime").join("venvs").join("mod-a");
        let bin_a = if cfg!(windows) {
            venv_a.join("Scripts")
        } else {
            venv_a.join("bin")
        };
        std::fs::create_dir_all(&bin_a).unwrap();
        let py = if cfg!(windows) {
            "python.exe"
        } else {
            "python"
        };
        std::fs::write(bin_a.join(py), "fake").unwrap();

        let manifest_a = ModuleManifest {
            module: ModuleInfo {
                id: "mod-a".into(),
                name: "A".into(),
                version: "1.0.0".into(),
                description: "A".into(),
                category: ModuleCategory::Asr,
                genre: "test".into(),
                authors: vec![],
                license: None,
                homepage: None,
                tags: vec![],
            },
            runtime: RuntimeConfig {
                runtime_type: RuntimeType::Python,
                python_version: Some(">=3.10".into()),
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
            models: vec![],
            interface: InterfaceConfig {
                interface_type: InterfaceType::Http,
                health_endpoint: None,
                ready_timeout_secs: None,
                working_dir: None,
                capabilities: vec![],
            },
        };

        // Module B: venv 不存在
        let mod_b_dir = root.join("modules").join("mod-b");
        std::fs::create_dir_all(&mod_b_dir).unwrap();

        let manifest_b = ModuleManifest {
            module: ModuleInfo {
                id: "mod-b".into(),
                name: "B".into(),
                version: "1.0.0".into(),
                description: "B".into(),
                category: ModuleCategory::Asr,
                genre: "test".into(),
                authors: vec![],
                license: None,
                homepage: None,
                tags: vec![],
            },
            runtime: RuntimeConfig {
                runtime_type: RuntimeType::Python,
                python_version: Some(">=3.10".into()),
                requirements: Some("requirements.txt".into()),
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
            models: vec![],
            interface: InterfaceConfig {
                interface_type: InterfaceType::Http,
                health_endpoint: None,
                ready_timeout_secs: None,
                working_dir: None,
                capabilities: vec![],
            },
        };

        let modules = vec![
            DiscoveredModule {
                manifest: Some(manifest_a),
                path: mod_a_dir,
                status: DiscoveryStatus::Valid,
            },
            DiscoveredModule {
                manifest: Some(manifest_b),
                path: mod_b_dir,
                status: DiscoveryStatus::Valid,
            },
        ];

        let mgr = EnvManager {
            root: root.clone(),
            python_path: None,
            uv_path: None,
            network_env: Vec::new(),
            uv_cache_dir: String::new(),
            constraints: String::new(),
        };

        let result = mgr.check_all_modules_env(&modules);
        assert_eq!(result.len(), 2);
        assert!(result["mod-a"]); // venv 存在，无 requirements → ready
        assert!(!result["mod-b"]); // venv 不存在 → not ready

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── M3：分后端 venv 布局 / 哈希 / 旧布局兼容读取（HETERO_DIST_PLAN）────

    /// 最小 EnvManager 夹具：无 python/uv/网络/缓存/constraints
    fn per_backend_mgr(root: &std::path::Path) -> EnvManager {
        EnvManager {
            root: root.to_path_buf(),
            python_path: None,
            uv_path: None,
            network_env: Vec::new(),
            uv_cache_dir: String::new(),
            constraints: String::new(),
        }
    }

    /// 分后端目录命名：`runtime/venvs/<module-id>--<backend>/`
    #[test]
    fn venv_dir_for_backend_naming() {
        let root = PathBuf::from("/app");
        let mgr = per_backend_mgr(&root);
        assert_eq!(
            mgr.venv_dir_for_backend("faster-whisper", ComputeBackend::Cuda),
            root.join("runtime")
                .join("venvs")
                .join("faster-whisper--cuda")
        );
        assert_eq!(
            mgr.venv_dir_for_backend("rembg", ComputeBackend::OpenVINO),
            root.join("runtime").join("venvs").join("rembg--openvino")
        );
        // 旧口径不受影响
        assert_eq!(
            mgr.venv_dir("rembg"),
            root.join("runtime").join("venvs").join("rembg")
        );
    }

    /// 分后端口径哈希：backend 名参与输入；None 种子与公开旧口径逐字节一致
    #[test]
    fn deps_hash_seeded_differs_by_backend_and_matches_legacy_api() {
        let (dir, req, cons) = deps_hash_fixture("m3seed");

        let legacy = compute_deps_hash(&req, Some(&cons)).unwrap();
        let cuda = compute_deps_hash_for_backend(&req, Some(&cons), ComputeBackend::Cuda).unwrap();
        let cuda_again =
            compute_deps_hash_for_backend(&req, Some(&cons), ComputeBackend::Cuda).unwrap();
        let rocm = compute_deps_hash_for_backend(&req, Some(&cons), ComputeBackend::Rocm).unwrap();

        assert_eq!(cuda, cuda_again, "同输入必须确定性");
        assert_ne!(cuda, rocm, "不同后端的依赖栈哈希必须可区分");
        assert_ne!(cuda, legacy, "分后端口径不得与旧口径碰撞");
        assert_ne!(rocm, legacy);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 旧布局兼容读取：旧单 venv 存在且旧口径哈希匹配 → 直接复用，
    /// 不创建 `<id>--<backend>` 新目录，避免全量重建
    #[test]
    fn ensure_venv_for_backend_reuses_ready_legacy_venv() {
        let root = std::env::temp_dir().join(format!("ep_m3_reuse_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let mid = "legacy-mod";
        // 旧布局：假解释器 + 匹配旧口径哈希的依赖栈
        let legacy_py = {
            let venv_dir = root.join("runtime").join("venvs").join(mid);
            let bin = if cfg!(windows) {
                venv_dir.join("Scripts")
            } else {
                venv_dir.join("bin")
            };
            std::fs::create_dir_all(&bin).unwrap();
            let py_name = if cfg!(windows) { "python.exe" } else { "python" };
            let py = bin.join(py_name);
            std::fs::write(&py, b"fake").unwrap();
            py
        };
        let req = root.join("modules").join(mid).join("requirements.txt");
        std::fs::create_dir_all(req.parent().unwrap()).unwrap();
        std::fs::write(&req, "fastapi>=0.100.0\n").unwrap();

        let mgr = per_backend_mgr(&root);
        let hash = compute_deps_hash(&req, None).unwrap();
        std::fs::write(
            root.join("runtime")
                .join("venvs")
                .join(mid)
                .join(DEPS_HASH_FILE_NAME),
            &hash,
        )
        .unwrap();

        // uv_path=None：若误入创建分支会立即报错，恰好证明兼容路径命中
        let py = mgr
            .ensure_venv_for_backend(mid, ">=3.10", &req, ComputeBackend::Cuda)
            .expect("legacy-ready venv must be reused without uv");
        assert_eq!(py, legacy_py, "应返回旧布局解释器");
        assert!(
            !mgr.venv_dir_for_backend(mid, ComputeBackend::Cuda).exists(),
            "复用旧布局时不得新建分后端目录"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 旧布局存在但哈希失配（依赖已变更）→ 不得复用，转而走分后端新目录
    /// （uv 缺失时在创建入口报错，证明目标不是旧目录）
    #[test]
    fn ensure_venv_for_backend_skips_stale_legacy_venv() {
        let root = std::env::temp_dir().join(format!("ep_m3_stale_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let mid = "stale-mod";
        let venv_dir = root.join("runtime").join("venvs").join(mid);
        let bin = if cfg!(windows) {
            venv_dir.join("Scripts")
        } else {
            venv_dir.join("bin")
        };
        std::fs::create_dir_all(&bin).unwrap();
        let py_name = if cfg!(windows) { "python.exe" } else { "python" };
        std::fs::write(bin.join(py_name), b"fake").unwrap();

        let req = root.join("modules").join(mid).join("requirements.txt");
        std::fs::create_dir_all(req.parent().unwrap()).unwrap();
        std::fs::write(&req, "torch\n").unwrap();
        // 写入与当前依赖栈不匹配的哈希（模拟旧依赖）
        std::fs::write(
            venv_dir.join(DEPS_HASH_FILE_NAME),
            "hash:deadbeefdeadbeef",
        )
        .unwrap();

        let mgr = per_backend_mgr(&root);
        let err = mgr
            .ensure_venv_for_backend(mid, ">=3.10", &req, ComputeBackend::Cuda)
            .unwrap_err();
        assert!(
            err.to_string().contains("uv not found"),
            "陈旧旧布局不得被复用，应在新建分支因缺 uv 报错: {err}"
        );
        assert!(
            !mgr.venv_dir_for_backend(mid, ComputeBackend::Cuda).exists(),
            "uv 缺失时不得留下半壳新目录"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 就绪判定：新布局（backend 口径哈希）优先；旧布局按旧口径兜底判定
    #[test]
    fn is_venv_ready_for_backend_new_layout_and_legacy_fallback() {
        let root = std::env::temp_dir().join(format!("ep_m3_ready_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let mid = "ready-mod";
        let req = root.join("modules").join(mid).join("requirements.txt");
        std::fs::create_dir_all(req.parent().unwrap()).unwrap();
        std::fs::write(&req, "ctranslate2\n").unwrap();

        let mgr = per_backend_mgr(&root);

        // 1) 什么都不存在 → false
        assert!(!mgr.is_venv_ready_for_backend(mid, &req, ComputeBackend::Rocm));

        // 2) 仅旧布局就绪（旧口径哈希）→ true（兼容读取）
        let legacy_bin = if cfg!(windows) {
            mgr.venv_dir(mid).join("Scripts")
        } else {
            mgr.venv_dir(mid).join("bin")
        };
        std::fs::create_dir_all(&legacy_bin).unwrap();
        let py_name = if cfg!(windows) { "python.exe" } else { "python" };
        std::fs::write(legacy_bin.join(py_name), b"fake").unwrap();
        std::fs::write(
            mgr.venv_dir(mid).join(DEPS_HASH_FILE_NAME),
            compute_deps_hash(&req, None).unwrap(),
        )
        .unwrap();
        assert!(
            mgr.is_venv_ready_for_backend(mid, &req, ComputeBackend::Rocm),
            "旧布局就绪必须在 rocm 维度兼容读取为 true"
        );

        // 3) 新布局就绪（rocm 口径哈希）→ true；且此时新布局优先于旧布局
        let new_py = {
            let bin = if cfg!(windows) {
                mgr.venv_dir_for_backend(mid, ComputeBackend::Rocm)
                    .join("Scripts")
            } else {
                mgr.venv_dir_for_backend(mid, ComputeBackend::Rocm)
                    .join("bin")
            };
            std::fs::create_dir_all(&bin).unwrap();
            let py = bin.join(py_name);
            std::fs::write(&py, b"fake").unwrap();
            py
        };
        std::fs::write(
            mgr.venv_dir_for_backend(mid, ComputeBackend::Rocm)
                .join(DEPS_HASH_FILE_NAME),
            compute_deps_hash_for_backend(&req, None, ComputeBackend::Rocm).unwrap(),
        )
        .unwrap();
        assert!(mgr.is_venv_ready_for_backend(mid, &req, ComputeBackend::Rocm));
        assert_eq!(
            mgr.venv_python_path_for_backend(mid, ComputeBackend::Rocm),
            new_py,
            "新旧布局同时可用时必须优先新布局"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 新布局哈希校验（隔离场景，无旧布局可兜底）：
    /// `.ep_deps_hash` 必须为**该 backend 口径**哈希——旧口径或其它 backend
    /// 的哈希写入新目录时不得通过校验（M3 哈希输入加入 backend 名的意义所在）
    #[test]
    fn new_layout_hash_must_be_backend_scoped() {
        let root = std::env::temp_dir().join(format!("ep_m3_scope_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let mid = "scoped-mod";
        let req = root.join("modules").join(mid).join("requirements.txt");
        std::fs::create_dir_all(req.parent().unwrap()).unwrap();
        std::fs::write(&req, "onnxruntime\n").unwrap();

        let mgr = per_backend_mgr(&root);
        let py_name = if cfg!(windows) { "python.exe" } else { "python" };
        let bin = if cfg!(windows) {
            mgr.venv_dir_for_backend(mid, ComputeBackend::Cuda)
                .join("Scripts")
        } else {
            mgr.venv_dir_for_backend(mid, ComputeBackend::Cuda)
                .join("bin")
        };
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(py_name), b"fake").unwrap();

        // 1) 旧口径哈希 → cuda 维度不认
        std::fs::write(
            mgr.venv_dir_for_backend(mid, ComputeBackend::Cuda)
                .join(DEPS_HASH_FILE_NAME),
            compute_deps_hash(&req, None).unwrap(),
        )
        .unwrap();
        assert!(!mgr.is_venv_ready_for_backend(mid, &req, ComputeBackend::Cuda));

        // 2) 其它 backend 的口径哈希 → 同样不认
        std::fs::write(
            mgr.venv_dir_for_backend(mid, ComputeBackend::Cuda)
                .join(DEPS_HASH_FILE_NAME),
            compute_deps_hash_for_backend(&req, None, ComputeBackend::Rocm).unwrap(),
        )
        .unwrap();
        assert!(!mgr.is_venv_ready_for_backend(mid, &req, ComputeBackend::Cuda));

        // 3) 精确匹配的 cuda 口径哈希 → 通过
        std::fs::write(
            mgr.venv_dir_for_backend(mid, ComputeBackend::Cuda)
                .join(DEPS_HASH_FILE_NAME),
            compute_deps_hash_for_backend(&req, None, ComputeBackend::Cuda).unwrap(),
        )
        .unwrap();
        assert!(mgr.is_venv_ready_for_backend(mid, &req, ComputeBackend::Cuda));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 解释器路径解析顺序：新布局 > 旧布局 > 前瞻性新布局答案
    #[test]
    fn venv_python_path_for_backend_resolution_order() {
        let root = std::env::temp_dir().join(format!("ep_m3_pypath_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let mid = "path-mod";
        let backend = ComputeBackend::Vulkan;
        let mgr = per_backend_mgr(&root);
        let new_layout_py = if cfg!(windows) {
            mgr.venv_dir_for_backend(mid, backend).join("Scripts").join("python.exe")
        } else {
            mgr.venv_dir_for_backend(mid, backend).join("bin").join("python")
        };

        // 1) 两者皆无 → 返回新布局口径（前瞻性答案）
        assert_eq!(mgr.venv_python_path_for_backend(mid, backend), new_layout_py);

        // 2) 仅旧布局存在解释器 → 兼容返回旧布局
        let legacy_py = if cfg!(windows) {
            mgr.venv_dir(mid).join("Scripts").join("python.exe")
        } else {
            mgr.venv_dir(mid).join("bin").join("python")
        };
        std::fs::create_dir_all(legacy_py.parent().unwrap()).unwrap();
        std::fs::write(&legacy_py, b"fake").unwrap();
        assert_eq!(mgr.venv_python_path_for_backend(mid, backend), legacy_py);

        // 3) 新布局出现解释器 → 新布局优先
        std::fs::create_dir_all(new_layout_py.parent().unwrap()).unwrap();
        std::fs::write(&new_layout_py, b"fake").unwrap();
        assert_eq!(mgr.venv_python_path_for_backend(mid, backend), new_layout_py);

        let _ = std::fs::remove_dir_all(&root);
    }
}
