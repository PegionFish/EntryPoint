//! 异构计算设备管理 — Wave 1 A5 Detectors
//!
//! 覆盖后端：CUDA / ROCm / OpenVINO（Intel iGPU + NPU）/ DirectML / Vulkan / CPU。
//!
//! 设计要点（PACK_UNIFY_PLAN §7 设计 E）：
//! - 检测优先级：CUDA 保持默认优先（注册表首位），其后 ROCm / OpenVINO / DirectML /
//!   Vulkan（备选位，HETERO_DIST_PLAN M4），CPU 兜底
//! - 所有检测器均为 best-effort：子进程超时 + 畸形输出容错；任何检测失败一律优雅降级为
//!   空设备列表，绝不 panic
//! - DirectML 与 CUDA/ROCm/OpenVINO 按适配器名去重（策略详见 directml.rs 模块文档）；
//!   Vulkan 与其之前全部后端按归一化名称去重（详见 vulkan.rs 模块文档）

pub mod cpu;
pub mod cuda;
pub mod directml;
pub mod openvino;
pub mod rocm;
pub mod scheduler;
pub mod vulkan;

pub use cpu::CpuDetector;
pub use cuda::CudaDetector;
pub use directml::DirectMlDetector;
pub use openvino::OpenVinoDetector;
pub use rocm::RocmDetector;
pub use vulkan::VulkanDetector;

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::types::{ComputeBackend, ComputeDevice};

/// 计算设备检测 trait — 所有后端实现此接口
pub trait DeviceDetector: Send + Sync {
    fn backend(&self) -> ComputeBackend;
    fn detect(&self) -> Vec<ComputeDevice>;
    fn refresh(&self, devices: &mut [ComputeDevice]);
}

/// SMI 类检测命令的默认子进程超时。
///
/// 15s（P3 放宽）：Windows 上 nvidia-smi 冷启动 + WDDM 查询在低端机/驱动
/// 异常时可能超过旧值 6s 而误判检测失败；探测是周期性的 best-effort，
/// 放宽只延后挂死进程的 kill 点，不放大故障面。
pub(crate) const TOOL_TIMEOUT: Duration = Duration::from_secs(15);

/// 执行外部检测命令并返回 stdout 文本（检测专用，绝不 panic）。
///
/// - spawn 失败 / 非零退出 / 超时 → 统一返回 `None`
/// - 每 25ms 轮询 `try_wait`，超时后 kill 子进程
/// - Windows 下附加 `CREATE_NO_WINDOW`，避免桌面端弹出控制台窗口
/// - 适用输出较小的命令；stdout 管道写满导致的挂起由超时兜底
pub(crate) fn run_tool(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut stdout = String::new();
                child.stdout.as_mut()?.read_to_string(&mut stdout).ok()?;
                return Some(stdout);
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => return None,
        }
    }
}

/// 候选可执行文件解析：绝对路径候选先做存在性检查，避免无效 spawn。
/// 返回是否"值得尝试执行"。
pub(crate) fn candidate_is_viable(candidate: &str) -> bool {
    if candidate.contains(['/', '\\']) {
        std::path::Path::new(candidate).exists()
    } else {
        true // 依赖 PATH 解析，spawn 失败时 run_tool 自然返回 None
    }
}

/// JSON 数值字段宽松解析：接受数字或数字字符串（含浮点字符串，如 "32.0"）
pub(crate) fn json_value_as_u64(v: &serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::Number(n) => {
            n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64))
        }
        serde_json::Value::String(s) => s
            .trim()
            .parse::<u64>()
            .ok()
            .or_else(|| s.trim().parse::<f64>().ok().map(|f| f.max(0.0) as u64)),
        _ => None,
    }
}

/// wmic `/format:list` 输出中提取 `Name=` 值列表
#[cfg(any(windows, test))]
pub(crate) fn parse_wmic_name_values(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| line.trim().strip_prefix("Name="))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// 逐行去空白、去空行（PowerShell 单列输出等纯文本列表）
#[cfg(windows)]
pub(crate) fn parse_plain_lines(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// 设备名归一化（用于跨后端去重匹配）：小写、去 (R)/(TM)/(C) 标记、空白折叠
pub(crate) fn normalize_device_name(name: &str) -> String {
    let mut s = name.to_ascii_lowercase();
    for marker in ["(r)", "(tm)", "(c)", "\u{00ae}", "\u{2122}"] {
        s = s.replace(marker, "");
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 显示适配器名称查询缓存：daemon 默认 2s 一轮重检测，而 wmic 缺失的
/// Win11 24H2+ 上每次查询都要起 PowerShell（秒级冷启动）——适配器清单
/// 几乎不变，缓存 30s 消除重复子进程。
#[cfg(windows)]
mod video_names_cache {
    use std::sync::{LazyLock, Mutex};
    use std::time::{Duration, Instant};

    /// 缓存条目：适配器名列表 + 查询时间
    type CacheEntry = (Vec<String>, Instant);

    static CACHE: LazyLock<Mutex<Option<CacheEntry>>> = LazyLock::new(|| Mutex::new(None));
    const TTL: Duration = Duration::from_secs(30);

    pub(super) fn get_or_query(query: impl FnOnce() -> Vec<String>) -> Vec<String> {
        let mut cache = CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((names, queried_at)) = cache.as_ref() {
            if queried_at.elapsed() < TTL {
                return names.clone();
            }
        }
        let names = query();
        *cache = Some((names.clone(), Instant::now()));
        names
    }
}

/// 枚举 Windows 显示适配器名称（wmic 优先，PowerShell 兜底，结果缓存 30s）。
/// 供 DirectML 检测与 OpenVINO iGPU 兜底探测共用；非 Windows 返回空。
#[cfg(windows)]
pub(crate) fn windows_video_controller_names() -> Vec<String> {
    video_names_cache::get_or_query(|| {
        if let Some(out) = run_tool(
            "wmic",
            &["path", "win32_videocontroller", "get", "name", "/format:list"],
            Duration::from_secs(8),
        ) {
            let names = parse_wmic_name_values(&out);
            if !names.is_empty() {
                return names;
            }
        }
        // Win11 24H2 起 wmic 可能被移除 → PowerShell 兜底
        if let Some(out) = run_tool(
            "powershell",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Get-CimInstance Win32_VideoController | Select-Object -ExpandProperty Name",
            ],
            Duration::from_secs(10),
        ) {
            return parse_plain_lines(&out);
        }
        Vec::new()
    })
}

/// 全部检测器。注册顺序 = 检测优先级；CUDA 默认优先，CPU 兜底，
/// Vulkan 备选位（DirectML 之后、Cpu 之前）。
fn all_detectors() -> Vec<Box<dyn DeviceDetector>> {
    vec![
        Box::new(CudaDetector),
        Box::new(RocmDetector),
        Box::new(OpenVinoDetector),
        Box::new(DirectMlDetector),
        Box::new(VulkanDetector),
        Box::new(CpuDetector),
    ]
}

/// 检测所有可用计算设备。
///
/// 顺序语义：CUDA → ROCm → OpenVINO → DirectML（对已检出设备去重）
/// → Vulkan（对已检出设备去重，备选位）→ CPU。
pub fn detect_all_devices(disabled: &[ComputeBackend]) -> Vec<ComputeDevice> {
    let mut devices: Vec<ComputeDevice> = Vec::new();

    for detector in all_detectors() {
        let backend = detector.backend();
        if disabled.contains(&backend) {
            continue;
        }
        match backend {
            // DirectML/Vulkan 需要已检出设备作为去重上下文、CPU 固定最后，单独处理
            ComputeBackend::DirectML | ComputeBackend::Vulkan | ComputeBackend::Cpu => continue,
            _ => devices.extend(detector.detect()),
        }
    }

    if !disabled.contains(&ComputeBackend::DirectML) {
        devices.extend(DirectMlDetector.detect_excluding(&devices));
    }
    if !disabled.contains(&ComputeBackend::Vulkan) {
        devices.extend(VulkanDetector.detect_excluding(&devices));
    }
    if !disabled.contains(&ComputeBackend::Cpu) {
        devices.extend(CpuDetector.detect());
    }
    devices
}

// `refresh_all_devices`（原仅桌面端常驻调度器消费）已随桌面端退役
//（2026-08-13）删除：daemon 无调用方，检测器刷新由调用侧自行驱动。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_order_and_coverage() {
        let backends: Vec<ComputeBackend> =
            all_detectors().iter().map(|d| d.backend()).collect();
        assert_eq!(
            backends,
            vec![
                ComputeBackend::Cuda, // 默认优先
                ComputeBackend::Rocm,
                ComputeBackend::OpenVINO,
                ComputeBackend::DirectML,
                ComputeBackend::Vulkan, // 备选位（M4）：厂商栈均不可用时兜底
                ComputeBackend::Cpu,    // 兜底
            ]
        );
    }

    #[test]
    fn test_vulkan_backend_serde_and_display() {
        // M4 词表：serde 小写 "vulkan" 与 Display/FromStr 三向一致
        assert_eq!(ComputeBackend::Vulkan.to_string(), "vulkan");
        assert_eq!(
            serde_json::to_string(&ComputeBackend::Vulkan).unwrap(),
            "\"vulkan\""
        );
        assert_eq!(
            "vulkan".parse::<ComputeBackend>().unwrap(),
            ComputeBackend::Vulkan
        );
        assert_eq!(
            serde_json::from_str::<ComputeBackend>("\"vulkan\"").unwrap(),
            ComputeBackend::Vulkan
        );
        // DeviceId 双向：Display 与 index
        let id = crate::types::DeviceId::Vulkan(2);
        assert_eq!(id.to_string(), "vulkan:2");
        assert_eq!(id.backend(), ComputeBackend::Vulkan);
        assert_eq!(id.index(), Some(2));
    }

    #[test]
    fn test_json_value_as_u64() {
        use serde_json::json;
        assert_eq!(json_value_as_u64(&json!(42)), Some(42));
        assert_eq!(json_value_as_u64(&json!("123")), Some(123));
        assert_eq!(json_value_as_u64(&json!("32.0")), Some(32));
        assert_eq!(json_value_as_u64(&json!(7.9)), Some(7));
        assert_eq!(json_value_as_u64(&json!("garbage")), None);
        assert_eq!(json_value_as_u64(&json!(null)), None);
        assert_eq!(json_value_as_u64(&json!(-5)), Some(0));
    }

    #[test]
    fn test_parse_wmic_name_values() {
        let output = "\r\n\r\nName=NVIDIA GeForce RTX 5090 D\r\n\r\nName=Intel(R) Graphics\r\n\r\n";
        let names = parse_wmic_name_values(output);
        assert_eq!(
            names,
            vec!["NVIDIA GeForce RTX 5090 D", "Intel(R) Graphics"]
        );
        assert!(parse_wmic_name_values("no names here").is_empty());
    }

    #[test]
    fn test_normalize_device_name() {
        assert_eq!(
            normalize_device_name("Intel(R) Graphics"),
            normalize_device_name("intel graphics")
        );
        assert_eq!(
            normalize_device_name("NVIDIA  GeForce RTX 5090 D "),
            "nvidia geforce rtx 5090 d"
        );
        assert_ne!(
            normalize_device_name("Intel(R) Graphics"),
            normalize_device_name("NVIDIA GeForce RTX 5090 D")
        );
    }

    #[test]
    fn test_candidate_is_viable() {
        assert!(candidate_is_viable("nvidia-smi")); // PATH 名称不检查
        assert!(!candidate_is_viable(
            r"C:\nonexistent\dir\tool-that-does-not-exist.exe"
        ));
    }
}
