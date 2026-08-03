//! 依赖自动安装 — 检测发行版并通过系统包管理器安装缺失依赖
//!
//! 支持 Debian 系（apt）、RHEL 系（dnf）、Arch 系（pacman）。
//! 无法识别的发行版仅打印手动安装指引。

use std::path::Path;
use std::process::Command;

use tracing::{info, warn};

// ─── 发行版家族 ──────────────────────────────────────────────────────────────

/// Linux 发行版家族
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistroFamily {
    /// Debian / Ubuntu / Linux Mint / Pop!_OS 等
    Debian,
    /// RHEL / CentOS / Fedora / Rocky / AlmaLinux 等
    Rhel,
    /// Arch / Manjaro / EndeavourOS 等
    Arch,
    /// 无法识别
    Unknown,
}

impl DistroFamily {
    /// 包管理器命令
    fn pkg_manager(&self) -> Option<&'static str> {
        match self {
            Self::Debian => Some("apt-get"),
            Self::Rhel => Some("dnf"),
            Self::Arch => Some("pacman"),
            Self::Unknown => None,
        }
    }

    /// 安装子命令参数（不含包名）
    fn install_args(&self) -> Vec<&'static str> {
        match self {
            Self::Debian => vec!["install", "-y", "--no-install-recommends"],
            Self::Rhel => vec!["install", "-y"],
            Self::Arch => vec!["-S", "--noconfirm", "--needed"],
            Self::Unknown => vec![],
        }
    }

    /// 是否需要先 update
    fn needs_update(&self) -> bool {
        matches!(self, Self::Debian)
    }
}

/// 检测当前系统的发行版家族
///
/// 读取 `/etc/os-release` 的 `ID` 和 `ID_LIKE` 字段进行归类。
pub fn detect_distro() -> DistroFamily {
    let os_release = match std::fs::read_to_string("/etc/os-release") {
        Ok(content) => content,
        Err(_) => {
            warn!("cannot read /etc/os-release, distro unknown");
            return DistroFamily::Unknown;
        }
    };

    let mut id = String::new();
    let mut id_like = String::new();

    for line in os_release.lines() {
        if let Some(val) = line.strip_prefix("ID=") {
            id = val.trim_matches('"').to_lowercase();
        } else if let Some(val) = line.strip_prefix("ID_LIKE=") {
            id_like = val.trim_matches('"').to_lowercase();
        }
    }

    let combined = format!("{id} {id_like}");
    info!(id = %id, id_like = %id_like, "detected distro");

    // 按优先级匹配
    if combined.contains("debian") || combined.contains("ubuntu") || combined.contains("mint") || combined.contains("pop") {
        DistroFamily::Debian
    } else if combined.contains("rhel") || combined.contains("fedora") || combined.contains("centos")
        || combined.contains("rocky") || combined.contains("almalinux") || combined.contains("ol")
    {
        DistroFamily::Rhel
    } else if combined.contains("arch") || combined.contains("manjaro") || combined.contains("endeavouros") {
        DistroFamily::Arch
    } else {
        warn!(id = %id, "unrecognized distro, will print manual instructions");
        DistroFamily::Unknown
    }
}

// ─── 依赖包名映射 ────────────────────────────────────────────────────────────

/// 系统级依赖项
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemDep {
    Ffmpeg,
    /// CUDA toolkit 库（libcublas 等）
    CudaToolkit,
    /// 桌面 GUI 依赖
    LibXkbCommon,
    Wayland,
    Fontconfig,
}

impl SystemDep {
    /// 返回该依赖在指定发行版家族下的包名
    fn package_name(&self, family: DistroFamily) -> Option<&'static str> {
        match (self, family) {
            (Self::Ffmpeg, DistroFamily::Debian) => Some("ffmpeg"),
            (Self::Ffmpeg, DistroFamily::Rhel) => Some("ffmpeg-free"),
            (Self::Ffmpeg, DistroFamily::Arch) => Some("ffmpeg"),

            (Self::CudaToolkit, DistroFamily::Debian) => Some("nvidia-cuda-toolkit"),
            // RHEL 系需要 NVIDIA 仓库，不自动安装
            (Self::CudaToolkit, DistroFamily::Rhel) => None,
            (Self::CudaToolkit, DistroFamily::Arch) => Some("cuda"),

            (Self::LibXkbCommon, DistroFamily::Debian) => Some("libxkbcommon-dev"),
            (Self::LibXkbCommon, DistroFamily::Rhel) => Some("libxkbcommon-devel"),
            (Self::LibXkbCommon, DistroFamily::Arch) => Some("libxkbcommon"),

            (Self::Wayland, DistroFamily::Debian) => Some("libwayland-dev"),
            (Self::Wayland, DistroFamily::Rhel) => Some("wayland-devel"),
            (Self::Wayland, DistroFamily::Arch) => Some("wayland"),

            (Self::Fontconfig, DistroFamily::Debian) => Some("libfontconfig1-dev"),
            (Self::Fontconfig, DistroFamily::Rhel) => Some("fontconfig-devel"),
            (Self::Fontconfig, DistroFamily::Arch) => Some("fontconfig"),

            (_, DistroFamily::Unknown) => None,
        }
    }

    /// 手动安装指引（用于 Unknown 发行版或无法自动安装的情况）
    fn manual_guidance(&self) -> &'static str {
        match self {
            Self::Ffmpeg => "ffmpeg: Debian/Ubuntu: sudo apt install ffmpeg | RHEL: sudo dnf install ffmpeg-free | Arch: sudo pacman -S ffmpeg",
            Self::CudaToolkit => "CUDA Toolkit: download from https://developer.nvidia.com/cuda-downloads, or install nvidia-cuda-toolkit / cuda from your distribution repositories",
            Self::LibXkbCommon => "libxkbcommon: Debian/Ubuntu: sudo apt install libxkbcommon-dev | RHEL: sudo dnf install libxkbcommon-devel | Arch: sudo pacman -S libxkbcommon",
            Self::Wayland => "wayland: Debian/Ubuntu: sudo apt install libwayland-dev | RHEL: sudo dnf install wayland-devel | Arch: sudo pacman -S wayland",
            Self::Fontconfig => "fontconfig: Debian/Ubuntu: sudo apt install libfontconfig1-dev | RHEL: sudo dnf install fontconfig-devel | Arch: sudo pacman -S fontconfig",
        }
    }
}

// ─── 自动安装 ────────────────────────────────────────────────────────────────

/// 安装结果
#[derive(Debug)]
pub enum InstallResult {
    /// 安装成功
    Installed,
    /// 已经存在
    AlreadyPresent,
    /// 自动安装失败，附手动指引
    Failed(String),
    /// 无法自动安装（未知发行版），附手动指引
    ManualRequired(String),
}

/// 检测当前是否以 root 运行
fn is_root() -> bool {
    #[cfg(unix)]
    {
        libc_getuid() == 0
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(unix)]
fn libc_getuid() -> u32 {
    // 避免引入 libc crate，通过 id -u 检测
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u32>().ok())
        .unwrap_or(1000)
}

/// 尝试自动安装单个系统依赖
///
/// - 已知发行版：调用包管理器安装
/// - 未知发行版或包名不可用：返回手动指引
pub fn auto_install(dep: SystemDep) -> InstallResult {
    let family = detect_distro();

    let pkg = match dep.package_name(family) {
        Some(p) => p,
        None => {
            return InstallResult::ManualRequired(dep.manual_guidance().to_string());
        }
    };

    let pkg_mgr = match family.pkg_manager() {
        Some(pm) => pm,
        None => {
            return InstallResult::ManualRequired(dep.manual_guidance().to_string());
        }
    };

    // 构建命令（加 sudo 除非已是 root）
    let use_sudo = !is_root();

    // Debian 系需要先 apt-get update
    if family.needs_update() {
        let mut update_cmd = if use_sudo {
            let mut c = Command::new("sudo");
            c.arg(pkg_mgr);
            c
        } else {
            Command::new(pkg_mgr)
        };
        update_cmd.arg("update").arg("-qq");
        info!(cmd = ?update_cmd, "running package list update");
        let _ = update_cmd.status();
    }

    // 安装
    let mut cmd = if use_sudo {
        let mut c = Command::new("sudo");
        c.arg(pkg_mgr);
        c
    } else {
        Command::new(pkg_mgr)
    };

    for arg in family.install_args() {
        cmd.arg(arg);
    }
    cmd.arg(pkg);

    info!(dep = ?dep, pkg = %pkg, family = ?family, "attempting auto-install");

    match cmd.status() {
        Ok(status) if status.success() => {
            info!(dep = ?dep, pkg = %pkg, "auto-install succeeded");
            InstallResult::Installed
        }
        Ok(status) => {
            warn!(dep = ?dep, pkg = %pkg, code = ?status.code(), "auto-install failed");
            InstallResult::Failed(format!(
                "package manager exited with code {:?}. Manual install: {}",
                status.code(),
                dep.manual_guidance()
            ))
        }
        Err(e) => {
            warn!(dep = ?dep, error = %e, "failed to execute package manager");
            InstallResult::Failed(format!(
                "failed to execute {pkg_mgr}: {e}. Manual install: {}",
                dep.manual_guidance()
            ))
        }
    }
}

/// 批量检测并安装缺失的系统依赖
///
/// 返回安装结果列表。调用方根据结果决定是否继续启动。
pub fn ensure_deps(deps: &[SystemDep]) -> Vec<(SystemDep, InstallResult)> {
    let mut results = Vec::new();

    for dep in deps {
        // 先检测是否已存在
        let present = match dep {
            SystemDep::Ffmpeg => which_exists("ffmpeg"),
            SystemDep::CudaToolkit => {
                // 检查 libcublas 是否可加载
                Path::new("/usr/local/cuda/lib64/libcublas.so").exists()
                    || Path::new("/usr/lib/x86_64-linux-gnu/libcublas.so").exists()
                    || Path::new("/usr/lib64/libcublas.so").exists()
                    || ldconfig_has("libcublas.so")
            }
            SystemDep::LibXkbCommon => ldconfig_has("libxkbcommon.so"),
            SystemDep::Wayland => ldconfig_has("libwayland-client.so"),
            SystemDep::Fontconfig => ldconfig_has("libfontconfig.so"),
        };

        if present {
            results.push((*dep, InstallResult::AlreadyPresent));
            continue;
        }

        let result = auto_install(*dep);
        results.push((*dep, result));
    }

    results
}

// ─── 内部工具 ────────────────────────────────────────────────────────────────

fn which_exists(name: &str) -> bool {
    let cmd = if cfg!(windows) { "where" } else { "which" };
    Command::new(cmd)
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ldconfig_has(lib: &str) -> bool {
    Command::new("ldconfig")
        .arg("-p")
        .output()
        .map(|o| {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout).contains(lib)
        })
        .unwrap_or(false)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_distro_returns_known_or_unknown() {
        let family = detect_distro();
        // 在任何 Linux 上都不应 panic
        let _ = family.pkg_manager();
    }

    #[test]
    fn test_package_name_mapping() {
        assert_eq!(
            SystemDep::Ffmpeg.package_name(DistroFamily::Debian),
            Some("ffmpeg")
        );
        assert_eq!(
            SystemDep::Ffmpeg.package_name(DistroFamily::Rhel),
            Some("ffmpeg-free")
        );
        assert_eq!(
            SystemDep::Ffmpeg.package_name(DistroFamily::Arch),
            Some("ffmpeg")
        );
        assert_eq!(
            SystemDep::Ffmpeg.package_name(DistroFamily::Unknown),
            None
        );
        // RHEL 不自动安装 CUDA
        assert_eq!(
            SystemDep::CudaToolkit.package_name(DistroFamily::Rhel),
            None
        );
    }

    #[test]
    fn test_manual_guidance_not_empty() {
        assert!(!SystemDep::Ffmpeg.manual_guidance().is_empty());
        assert!(!SystemDep::CudaToolkit.manual_guidance().is_empty());
    }

    #[test]
    fn test_install_args() {
        assert!(DistroFamily::Debian.install_args().contains(&"-y"));
        assert!(DistroFamily::Arch.install_args().contains(&"--noconfirm"));
        assert!(DistroFamily::Unknown.install_args().is_empty());
    }
}