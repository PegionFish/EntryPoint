//! 外部依赖检测 — 检查 torch CUDA / ffmpeg 等可选依赖的可用性
//!
//! 检测后可通过 `deps_install` 模块自动安装缺失依赖（主流发行版）。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, info, warn};

use crate::deps_install::{self, InstallResult, SystemDep};

/// 单个依赖的检测结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepStatus {
    pub name: String,
    pub available: bool,
    pub version: Option<String>,
    pub path: Option<String>,
    pub guidance: Option<String>,
}

/// 所有外部依赖的检测结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepReport {
    pub ffmpeg: DepStatus,
    pub torch_cuda: Vec<TorchCudaStatus>,
}

/// 单个 venv 的 torch CUDA 状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorchCudaStatus {
    pub module_id: String,
    pub venv_path: String,
    pub torch_version: Option<String>,
    pub cuda_available: bool,
    pub guidance: Option<String>,
}

impl DepReport {
    /// 便捷方法：聚合 ffmpeg + 所有模块 venv 的 torch CUDA 检测
    ///
    /// 自动扫描 `runtime/venvs/` 下的所有模块 venv 目录。
    /// torch 检测使用默认共享 CUDA 库目录（`runtime/cuda-libs`）；
    /// 需要自定义 `[compute].cuda_libs_dir` 时用 [`Self::check_all_with_cuda_libs`]。
    pub fn check_all(root: &Path) -> Self {
        Self::check_all_with_cuda_libs(root, None)
    }

    /// 同 [`Self::check_all`]，但允许指定共享 CUDA 库目录（传 `[compute].cuda_libs_dir` 解析值）。
    ///
    /// `cuda_libs_dir` 为 None 时使用默认 `runtime/cuda-libs`（相对 root 解析）。
    pub fn check_all_with_cuda_libs(root: &Path, cuda_libs_dir: Option<&Path>) -> Self {
        let cuda_dir = cuda_libs_dir.map(Path::to_path_buf).unwrap_or_else(|| {
            crate::process::resolve_cuda_libs_dir(root, crate::process::DEFAULT_CUDA_LIBS_DIR)
        });

        let ffmpeg = check_ffmpeg(root);

        // 扫描 runtime/venvs/ 下的所有子目录作为模块 ID
        let venvs_dir = root.join("runtime").join("venvs");
        let module_ids: Vec<String> = match std::fs::read_dir(&venvs_dir) {
            Ok(entries) => entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().to_str().map(String::from))
                .collect(),
            Err(_) => Vec::new(),
        };

        let torch_cuda: Vec<TorchCudaStatus> = module_ids
            .iter()
            .filter_map(|id| {
                let venv_dir = venvs_dir.join(id);
                let python = if cfg!(windows) {
                    venv_dir.join("Scripts").join("python.exe")
                } else {
                    venv_dir.join("bin").join("python")
                };
                if python.is_file() {
                    Some(check_torch_cuda(id, &python, Some(&cuda_dir)))
                } else {
                    None
                }
            })
            .collect();

        debug!(
            ffmpeg_available = ffmpeg.available,
            torch_modules = torch_cuda.len(),
            "dependency check completed"
        );

        Self { ffmpeg, torch_cuda }
    }

    /// 检测依赖并在缺失时尝试自动安装（仅 Linux 主流发行版）。
    ///
    /// 返回安装结果摘要。调用方可据此决定是否继续启动。
    pub fn check_and_install_missing(root: &Path) -> Vec<(SystemDep, InstallResult)> {
        let report = Self::check_all(root);
        let mut missing = Vec::new();

        if !report.ffmpeg.available {
            missing.push(SystemDep::Ffmpeg);
        }

        if missing.is_empty() {
            info!("all system dependencies present");
            return vec![(SystemDep::Ffmpeg, InstallResult::AlreadyPresent)];
        }

        info!(missing = ?missing, "attempting auto-install of missing dependencies");
        deps_install::ensure_deps(&missing)
    }
}

/// 检测 ffmpeg 是否可用
///
/// 搜索顺序：
/// 1. `{root}/runtime/bin/ffmpeg`（项目内置）
/// 2. 系统 PATH 中的 ffmpeg
/// 3. 常见安装路径
pub fn check_ffmpeg(root: &Path) -> DepStatus {
    let ffmpeg_name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };

    // 1. 项目内置
    let bundled = root.join("runtime").join("bin").join(ffmpeg_name);
    if bundled.is_file() {
        let version = get_ffmpeg_version(&bundled);
        return DepStatus {
            name: "ffmpeg".into(),
            available: true,
            version,
            path: Some(bundled.to_string_lossy().to_string()),
            guidance: None,
        };
    }

    // 2. PATH
    if let Some(path) = which("ffmpeg") {
        let version = get_ffmpeg_version(&path);
        return DepStatus {
            name: "ffmpeg".into(),
            available: true,
            version,
            path: Some(path.to_string_lossy().to_string()),
            guidance: None,
        };
    }

    // 3. 常见路径
    #[cfg(windows)]
    let candidates = [
        r"C:\ffmpeg\bin\ffmpeg.exe",
        r"C:\tools\ffmpeg\bin\ffmpeg.exe",
        r"C:\ProgramData\chocolatey\bin\ffmpeg.exe",
    ];
    #[cfg(not(windows))]
    let candidates = [
        "/usr/bin/ffmpeg",
        "/usr/local/bin/ffmpeg",
        "/snap/bin/ffmpeg",
        "/opt/ffmpeg/bin/ffmpeg",
    ];

    for c in &candidates {
        let p = PathBuf::from(c);
        if p.is_file() {
            let version = get_ffmpeg_version(&p);
            return DepStatus {
                name: "ffmpeg".into(),
                available: true,
                version,
                path: Some(p.to_string_lossy().to_string()),
                guidance: None,
            };
        }
    }

    // 4. modules/test-ffmpeg/ffmpeg (fallback)
    let fallback = root.join("modules").join("test-ffmpeg").join(ffmpeg_name);
    if fallback.is_file() {
        let version = get_ffmpeg_version(&fallback);
        return DepStatus {
            name: "ffmpeg".into(),
            available: true,
            version,
            path: Some(fallback.to_string_lossy().to_string()),
            guidance: None,
        };
    }

    DepStatus {
        name: "ffmpeg".into(),
        available: false,
        version: None,
        path: None,
        guidance: Some(if cfg!(windows) {
            "ffmpeg not found. Audio/video extraction nodes in pipelines require ffmpeg.\n\
             Installation options (pick one):\n\
             1. Download a portable build: https://www.gyan.dev/ffmpeg/builds/ → extract and place ffmpeg.exe in runtime/bin/\n\
             2. winget install ffmpeg\n\
             3. Copy ffmpeg.exe from an existing project into runtime/bin/"
                .into()
        } else {
            "ffmpeg not found. Audio/video extraction nodes in pipelines require ffmpeg.\n\
             Installation options (pick one):\n\
             1. sudo dnf install ffmpeg-free (RHEL/CentOS)\n\
             2. sudo apt install ffmpeg (Debian/Ubuntu)\n\
             3. Download a static build: https://johnvansickle.com/ffmpeg/ → extract and place ffmpeg in runtime/bin/"
                .into()
        }),
    }
}

/// 检测指定 venv 中 torch CUDA 是否可用
///
/// `cuda_libs_dir`：共享 CUDA 库目录（§3.1）。提供且目录存在时，按平台注入
/// 动态库搜索路径（Linux `LD_LIBRARY_PATH` 前置 / Windows `PATH` 前置），
/// 与 start_module 同路径——修复 torch 因找不到 libcublas 等共享库导致的
/// CUDA 误报（P1 误报）。
pub fn check_torch_cuda(
    module_id: &str,
    venv_python: &Path,
    cuda_libs_dir: Option<&Path>,
) -> TorchCudaStatus {
    let mut cmd = Command::new(venv_python);
    cmd.args([
        "-c",
        "import torch; print(f'{torch.__version__}|{torch.cuda.is_available()}')",
    ]);
    if let Some(dir) = cuda_libs_dir {
        if dir.is_dir() {
            if let Some((key, value)) = crate::process::cuda_lib_path_env(dir) {
                cmd.env(key, value);
            }
        }
    }
    let output = cmd.output();

    match output {
        Ok(o) if o.status.success() => {
            let line = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let parts: Vec<&str> = line.split('|').collect();
            let version = parts.first().map(|s| s.to_string());
            let cuda = parts.get(1).map(|s| s.trim() == "True").unwrap_or(false);

            let guidance = if !cuda {
                Some(format!(
                    "torch is installed but CUDA is unavailable (CPU-only build).\n\
                     GPU acceleration for module '{module_id}' requires the CUDA build of torch.\n\
                     Install with:\n\
                     uv pip install torch --index-url https://download.pytorch.org/whl/cu121 \\\n\
                       --python {venv}\n\
                     China mirror: --extra-index-url https://mirrors.aliyun.com/pytorch-wheels/cu121/ \\\n\
                       --index-strategy unsafe-best-match",
                    venv = venv_python.display(),
                ))
            } else {
                None
            };

            TorchCudaStatus {
                module_id: module_id.into(),
                venv_path: venv_python
                    .parent()
                    .and_then(|p| p.parent())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
                torch_version: version,
                cuda_available: cuda,
                guidance,
            }
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            debug!(module_id, stderr = %stderr, "torch import failed");
            TorchCudaStatus {
                module_id: module_id.into(),
                venv_path: venv_python
                    .parent()
                    .and_then(|p| p.parent())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
                torch_version: None,
                cuda_available: false,
                guidance: Some(format!(
                    "torch is not installed in the venv of module '{module_id}'.\n\
                     Install with: uv pip install torch --python {}",
                    venv_python.display()
                )),
            }
        }
        Err(e) => {
            warn!(module_id, error = %e, "failed to run python for torch check");
            TorchCudaStatus {
                module_id: module_id.into(),
                venv_path: venv_python
                    .parent()
                    .and_then(|p| p.parent())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
                torch_version: None,
                cuda_available: false,
                guidance: Some(format!("failed to run Python: {e}")),
            }
        }
    }
}

/// 扫描所有模块 venv，生成完整依赖报告
///
/// torch 检测注入默认共享 CUDA 库目录（`runtime/cuda-libs`，§3.1），
/// 与 start_module 同路径，避免 CUDA 误报。
pub fn check_all_deps(root: &Path, module_ids: &[&str]) -> DepReport {
    let ffmpeg = check_ffmpeg(root);
    let cuda_dir =
        crate::process::resolve_cuda_libs_dir(root, crate::process::DEFAULT_CUDA_LIBS_DIR);

    let torch_cuda: Vec<TorchCudaStatus> = module_ids
        .iter()
        .filter_map(|id| {
            let venv_dir = root.join("runtime").join("venvs").join(id);
            let python = if cfg!(windows) {
                venv_dir.join("Scripts").join("python.exe")
            } else {
                venv_dir.join("bin").join("python")
            };
            if python.is_file() {
                Some(check_torch_cuda(id, &python, Some(&cuda_dir)))
            } else {
                None
            }
        })
        .collect();

    DepReport { ffmpeg, torch_cuda }
}

/// 获取 ffmpeg 可执行文件路径（用于管线节点）
///
/// 返回 Some(path) 如果找到，None 如果未找到
pub fn find_ffmpeg(root: &Path) -> Option<PathBuf> {
    let ffmpeg_name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    let bundled = root.join("runtime").join("bin").join(ffmpeg_name);
    if bundled.is_file() {
        return Some(bundled);
    }
    which("ffmpeg")
}

// ─── 内部工具 ────────────────────────────────────────────────────────────────

fn which(name: &str) -> Option<PathBuf> {
    let cmd = if cfg!(windows) { "where" } else { "which" };
    Command::new(cmd)
        .arg(name)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .map(|s| PathBuf::from(s.trim()))
        })
        .filter(|p| p.is_file())
}

fn get_ffmpeg_version(path: &Path) -> Option<String> {
    Command::new(path)
        .arg("-version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .map(|s| s.trim().to_string())
        })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_ffmpeg_not_found() {
        let root = PathBuf::from("/nonexistent/path");
        let status = check_ffmpeg(&root);
        // ffmpeg 可能通过系统 PATH 找到（如已安装），因此不强制 assert !available
        // 但 bundled 路径一定不存在
        if !status.available {
            assert!(status.guidance.is_some());
        } else {
            // 系统 PATH 找到了 ffmpeg，path 不应为 /nonexistent 下的路径
            let p = status.path.unwrap();
            assert!(!p.starts_with("/nonexistent"));
        }
    }

    #[test]
    fn test_check_torch_cuda_nonexistent_venv() {
        let status = check_torch_cuda("test", Path::new("/nonexistent/python.exe"), None);
        assert!(!status.cuda_available);
        assert!(status.guidance.is_some());
    }

    #[test]
    fn test_check_torch_cuda_with_cuda_libs_dir_variants() {
        // cuda_libs_dir 不存在 → 跳过注入，不 panic，检测正常降级
        let status = check_torch_cuda(
            "test",
            Path::new("/nonexistent/python.exe"),
            Some(Path::new("/nonexistent/cuda-libs")),
        );
        assert!(!status.cuda_available);
        assert!(status.guidance.is_some());

        // cuda_libs_dir 存在（tempdir）→ 注入路径可达（进程仍因 python 不存在而失败）
        let dir = std::env::temp_dir().join(format!(
            "ep_deps_cuda_libs_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let status2 = check_torch_cuda(
            "test",
            Path::new("/nonexistent/python.exe"),
            Some(&dir),
        );
        assert!(!status2.cuda_available);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_dep_report_serialization() {
        let report = DepReport {
            ffmpeg: DepStatus {
                name: "ffmpeg".into(),
                available: false,
                version: None,
                path: None,
                guidance: Some("test".into()),
            },
            torch_cuda: vec![],
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("ffmpeg"));
    }

    #[test]
    fn test_dep_report_deserialization() {
        let json = r#"{
            "ffmpeg": {"name":"ffmpeg","available":true,"version":"6.0","path":"/usr/bin/ffmpeg","guidance":null},
            "torch_cuda": [{"module_id":"m1","venv_path":"/venvs/m1","torch_version":"2.1","cuda_available":true,"guidance":null}]
        }"#;
        let report: DepReport = serde_json::from_str(json).unwrap();
        assert!(report.ffmpeg.available);
        assert_eq!(report.torch_cuda.len(), 1);
        assert!(report.torch_cuda[0].cuda_available);
    }

    #[test]
    fn test_check_all_nonexistent_root() {
        let root = PathBuf::from("/nonexistent/ep_root");
        let report = DepReport::check_all(&root);
        // ffmpeg 可能通过系统 PATH 找到，但 torch_cuda 应为空（无 venvs 目录）
        assert!(report.torch_cuda.is_empty());
    }
}
