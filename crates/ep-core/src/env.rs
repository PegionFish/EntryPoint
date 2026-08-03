//! 虚拟环境管理器 — 负责 Python venv 的创建、依赖安装和状态检测
//!
//! 使用 `uv` 作为底层工具完成 venv 创建和 pip 安装。
//! 通过 requirements.txt 的哈希值判断是否需要重新安装依赖。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use tracing::{debug, info, warn};

use crate::config::PythonConfig;

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
            let p = PathBuf::from(&config.uv_path);
            if p.exists() {
                debug!(path = %p.display(), "using configured uv path");
                Some(p)
            } else {
                warn!(path = %p.display(), "configured uv path does not exist, falling back to detection");
                Self::detect_uv()
            }
        } else {
            Self::detect_uv()
        };

        let python_path = if !config.path.is_empty() {
            let p = PathBuf::from(&config.path);
            if p.exists() {
                debug!(path = %p.display(), "using configured python path");
                Some(p)
            } else {
                warn!(path = %p.display(), "configured python path does not exist, falling back to detection");
                Self::detect_python(uv_path.as_deref())
            }
        } else {
            Self::detect_python(uv_path.as_deref())
        };

        Self {
            root: root.to_path_buf(),
            python_path,
            uv_path,
            network_env: Vec::new(),
        }
    }

    /// 设置网络代理配置（链式调用）。
    ///
    /// uv venv / uv pip install 子进程将被注入这些环境变量（仅非空值）。
    pub fn with_network(mut self, network: &crate::config::NetworkConfig) -> Self {
        self.network_env = network.env_vars();
        self
    }

    /// 设置网络代理环境变量
    pub fn set_network(&mut self, network: &crate::config::NetworkConfig) {
        self.network_env = network.env_vars();
    }

    /// 检测系统 PATH 中的 python（优先 python3，其次 python）
    /// 若 PATH 中无兼容版本，则借助 uv python find 查找 uv 管理的 Python
    fn detect_python(uv: Option<&Path>) -> Option<PathBuf> {
        // 1. 尝试 PATH 中的 python3 / python
        for name in ["python3", "python"] {
            if let Ok(output) = Command::new(name).arg("--version").output() {
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
            if let Ok(output) = Command::new(&uv_exe)
                .args(["python", "find", ver])
                .output()
            {
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
        if let Ok(output) = Command::new("uv").arg("--version").output() {
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
        match Command::new(path).arg("--version").output() {
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
        match Command::new(path).arg("--version").output() {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            _ => "unknown".to_string(),
        }
    }

    /// 平台相关的 python 安装提示
    fn python_install_hint() -> String {
        if cfg!(windows) {
            "Python 未找到。请从 https://www.python.org/downloads/ 下载安装，\
             安装时勾选 \"Add Python to PATH\"。"
                .to_string()
        } else {
            "Python 未找到。请运行: sudo apt install python3 python3-venv \
             (Debian/Ubuntu) 或 sudo dnf install python3 (Fedora)"
                .to_string()
        }
    }

    /// 平台相关的 uv 安装提示
    fn uv_install_hint() -> String {
        if cfg!(windows) {
            "uv 未找到。请从 https://github.com/astral-sh/uv/releases 下载，\
             或运行: powershell -c \"irm https://astral.sh/uv/install.ps1 | iex\""
                .to_string()
        } else {
            "uv 未找到。请运行: curl -LsSf https://astral.sh/uv/install.sh | sh"
                .to_string()
        }
    }

    /// 确保模块的虚拟环境就绪
    ///
    /// 流程：
    /// 1. 检查 `runtime/venvs/<module_id>/` 是否存在
    /// 2. 不存在 → `uv venv --python <version> <path>`
    /// 3. 计算 requirements.txt 哈希
    /// 4. 对比 `.ep_deps_hash`
    /// 5. 不一致 → `uv pip install -r <req> --python <venv_python>`
    /// 6. 写入新哈希
    /// 7. 返回 venv 内 python 路径
    pub fn ensure_venv(
        &self,
        module_id: &str,
        python_version: &str,
        requirements: &Path,
    ) -> Result<PathBuf> {
        let uv = self
            .uv_path
            .as_ref()
            .context("uv not found, cannot create venv")?;

        let venv_dir = self.venv_dir(module_id);
        let venv_python = self.venv_python_path(module_id);

        // 1. 创建 venv（如果不存在）
        if !venv_dir.exists() {
            info!(module = module_id, path = %venv_dir.display(), "creating venv");
            std::fs::create_dir_all(&venv_dir).with_context(|| {
                format!("failed to create venv directory: {}", venv_dir.display())
            })?;

            let output = run_command_with_env(
                uv.to_str().unwrap_or("uv"),
                &[
                    "venv",
                    "--python",
                    python_version,
                    venv_dir.to_str().unwrap_or_default(),
                ],
                &self.network_env,
            )
            .with_context(|| format!("failed to create venv for module '{module_id}'"))?;
            debug!(module = module_id, output = %output, "venv created");
        } else {
            debug!(module = module_id, "venv already exists");
        }

        // 2. 检查依赖哈希
        if !requirements.exists() {
            debug!(module = module_id, "no requirements.txt, skipping dependency install");
            return Ok(venv_python);
        }

        let current_hash =
            hash_file(requirements).with_context(|| {
                format!("failed to hash requirements file: {}", requirements.display())
            })?;

        let hash_file = self.deps_hash_path(module_id);
        let needs_install = if hash_file.exists() {
            let stored = std::fs::read_to_string(&hash_file).unwrap_or_default();
            stored.trim() != current_hash
        } else {
            true
        };

        // 3. 安装/更新依赖
        if needs_install {
            info!(module = module_id, "installing dependencies");
            let venv_py_str = venv_python.to_str().unwrap_or_default();
            let req_str = requirements.to_str().unwrap_or_default();

            let output = run_command_with_env(
                uv.to_str().unwrap_or("uv"),
                &["pip", "install", "-r", req_str, "--python", venv_py_str],
                &self.network_env,
            )
            .with_context(|| format!("failed to install dependencies for module '{module_id}'"))?;
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

    /// 获取模块 venv 内的 python 可执行文件路径
    ///
    /// - Windows: `runtime/venvs/<id>/Scripts/python.exe`
    /// - Linux/macOS: `runtime/venvs/<id>/bin/python`
    pub fn venv_python_path(&self, module_id: &str) -> PathBuf {
        let venv_dir = self.venv_dir(module_id);
        if cfg!(windows) {
            venv_dir.join("Scripts").join("python.exe")
        } else {
            venv_dir.join("bin").join("python")
        }
    }

    /// 检查模块 venv 是否就绪（存在且依赖哈希匹配）
    pub fn is_venv_ready(&self, module_id: &str, requirements: &Path) -> bool {
        let venv_python = self.venv_python_path(module_id);
        if !venv_python.exists() {
            return false;
        }

        // 无 requirements 文件时，venv 存在即就绪
        if !requirements.exists() {
            return true;
        }

        let hash_file = self.deps_hash_path(module_id);
        if !hash_file.exists() {
            return false;
        }

        let current_hash = match hash_file_content(requirements) {
            Ok(h) => h,
            Err(_) => return false,
        };

        let stored = std::fs::read_to_string(&hash_file).unwrap_or_default();
        stored.trim() == current_hash
    }

    /// venv 目录路径: `runtime/venvs/<module_id>/`
    fn venv_dir(&self, module_id: &str) -> PathBuf {
        self.root.join("runtime").join("venvs").join(module_id)
    }

    /// 依赖哈希文件路径: `runtime/venvs/<module_id>/.ep_deps_hash`
    fn deps_hash_path(&self, module_id: &str) -> PathBuf {
        self.venv_dir(module_id).join(".ep_deps_hash")
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
        let hash_file = venv_dir.join(".ep_deps_hash");

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

/// 计算文件内容的哈希值（简化实现）
///
/// **注意：** 使用 `DefaultHasher`（SipHash-1-3），非加密安全。
/// 仅用于检测 requirements.txt 是否变更，不用于安全校验。
/// 输出格式：`hash:<16位十六进制>`
pub fn hash_file(path: &Path) -> Result<String> {
    let content = std::fs::read(path)
        .with_context(|| format!("failed to read file for hashing: {}", path.display()))?;
    Ok(hash_bytes(&content))
}

/// 计算文件哈希（同 `hash_file`，用于 `is_venv_ready` 内部调用）
fn hash_file_content(path: &Path) -> Result<String> {
    hash_file(path)
}

/// 对字节内容计算哈希
fn hash_bytes(data: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    let hash_value = hasher.finish();
    format!("hash:{hash_value:016x}")
}

/// 执行外部命令，捕获 stdout 输出
///
/// 成功时返回 stdout 内容，失败时返回错误（含 stderr 信息）。
pub fn run_command(cmd: &str, args: &[&str]) -> Result<String> {
    run_command_with_env(cmd, args, &[])
}

/// 执行外部命令并注入额外环境变量（仅注入非空值），捕获 stdout 输出
///
/// 用于给 uv/pip 等联网子进程注入代理环境变量。
/// 成功时返回 stdout 内容，失败时返回错误（含 stderr 信息）。
pub fn run_command_with_env(
    cmd: &str,
    args: &[&str],
    extra_env: &[(String, String)],
) -> Result<String> {
    debug!(cmd = cmd, args = ?args, "executing command");

    let mut command = Command::new(cmd);
    command.args(args);
    for (key, value) in extra_env {
        if !value.is_empty() {
            command.env(key, value);
        }
    }

    let output = command
        .output()
        .with_context(|| format!("failed to execute command: {cmd}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        bail!(
            "command '{}' failed with exit code {:?}\nstderr: {}",
            cmd,
            output.status.code(),
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
    fn venv_python_path_windows_style() {
        // 测试路径构造逻辑（不依赖实际平台）
        let root = PathBuf::from("/fake/root");
        let mgr = EnvManager {
            root,
            python_path: None,
            uv_path: None,
            network_env: Vec::new(),
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
        };

        // venv 不存在时应返回 false
        let req = root.join("requirements.txt");
        assert!(
            !mgr.is_venv_ready("nonexistent-module", &req),
            "should return false for non-existent venv"
        );

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
        };

        let result = mgr.check_all_modules_env(&modules);
        assert_eq!(result.len(), 2);
        assert!(result["mod-a"]); // venv 存在，无 requirements → ready
        assert!(!result["mod-b"]); // venv 不存在 → not ready

        let _ = std::fs::remove_dir_all(&root);
    }
}
