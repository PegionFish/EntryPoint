//! 外部依赖检测 — 检查 torch CUDA / ffmpeg 等可选依赖的可用性
//!
//! 不自动安装任何依赖，仅提供检测结果和用户引导信息。

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, warn};

/// 单个依赖的检测结果
#[derive(Debug, Clone, Serialize)]
pub struct DepStatus {
    pub name: String,
    pub available: bool,
    pub version: Option<String>,
    pub path: Option<String>,
    pub guidance: Option<String>,
}

/// 所有外部依赖的检测结果
#[derive(Debug, Clone, Serialize)]
pub struct DepReport {
    pub ffmpeg: DepStatus,
    pub torch_cuda: Vec<TorchCudaStatus>,
}

/// 单个 venv 的 torch CUDA 状态
#[derive(Debug, Clone, Serialize)]
pub struct TorchCudaStatus {
    pub module_id: String,
    pub venv_path: String,
    pub torch_version: Option<String>,
    pub cuda_available: bool,
    pub guidance: Option<String>,
}

/// 检测 ffmpeg 是否可用
///
/// 搜索顺序：
/// 1. `{root}/runtime/bin/ffmpeg.exe`（项目内置）
/// 2. 系统 PATH 中的 ffmpeg
/// 3. 常见安装路径
pub fn check_ffmpeg(root: &Path) -> DepStatus {
    // 1. 项目内置
    let bundled = root.join("runtime").join("bin").join("ffmpeg.exe");
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
    let candidates: [&str; 0] = [];

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

    DepStatus {
        name: "ffmpeg".into(),
        available: false,
        version: None,
        path: None,
        guidance: Some(
            "ffmpeg 未找到。管线中的音频/视频提取节点需要 ffmpeg。\n\
             安装方式（任选其一）：\n\
             1. 下载 portable 版: https://www.gyan.dev/ffmpeg/builds/ → 解压后将 ffmpeg.exe 放入 runtime/bin/\n\
             2. winget install ffmpeg\n\
             3. 从已有项目复制 ffmpeg.exe 到 runtime/bin/"
                .into(),
        ),
    }
}

/// 检测指定 venv 中 torch CUDA 是否可用
pub fn check_torch_cuda(module_id: &str, venv_python: &Path) -> TorchCudaStatus {
    let output = Command::new(venv_python)
        .args([
            "-c",
            "import torch; print(f'{torch.__version__}|{torch.cuda.is_available()}')",
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let line = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let parts: Vec<&str> = line.split('|').collect();
            let version = parts.first().map(|s| s.to_string());
            let cuda = parts.get(1).map(|s| s.trim() == "True").unwrap_or(false);

            let guidance = if !cuda {
                Some(format!(
                    "torch 已安装但 CUDA 不可用（当前为 CPU 版本）。\n\
                     模块 '{module_id}' 的 GPU 加速需要 CUDA 版 torch。\n\
                     安装方式：\n\
                     uv pip install torch --index-url https://download.pytorch.org/whl/cu121 \\\n\
                       --python {venv}\n\
                     国内镜像：--extra-index-url https://mirrors.aliyun.com/pytorch-wheels/cu121/ \\\n\
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
                    "torch 未安装在模块 '{module_id}' 的 venv 中。\n\
                     安装方式：uv pip install torch --python {}",
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
                guidance: Some(format!("无法执行 Python: {e}")),
            }
        }
    }
}

/// 扫描所有模块 venv，生成完整依赖报告
pub fn check_all_deps(root: &Path, module_ids: &[&str]) -> DepReport {
    let ffmpeg = check_ffmpeg(root);

    let torch_cuda: Vec<TorchCudaStatus> = module_ids
        .iter()
        .filter_map(|id| {
            let python = root
                .join("runtime")
                .join("venvs")
                .join(id)
                .join("Scripts")
                .join("python.exe");
            if python.is_file() {
                Some(check_torch_cuda(id, &python))
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
    let bundled = root.join("runtime").join("bin").join("ffmpeg.exe");
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
        assert!(!status.available);
        assert!(status.guidance.is_some());
    }

    #[test]
    fn test_check_torch_cuda_nonexistent_venv() {
        let status = check_torch_cuda("test", Path::new("/nonexistent/python.exe"));
        assert!(!status.cuda_available);
        assert!(status.guidance.is_some());
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
}
