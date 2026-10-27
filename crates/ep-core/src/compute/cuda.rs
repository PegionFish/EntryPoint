//! NVIDIA CUDA 设备检测（nvidia-smi）
//!
//! Windows 探测路径（§15.3）：
//! 1. `EP_NVIDIA_SMI_PATH` 环境变量（显式覆盖，便于真机验证调优）
//! 2. PATH（新驱动把 nvidia-smi.exe 装入 System32，PATH 命中即可用）
//! 3. `C:\Program Files\NVIDIA Corporation\NVSMI\nvidia-smi.exe`（旧版驱动随附路径）
//!
//! 所有候选均带子进程超时与畸形输出容错。

use crate::types::{ComputeBackend, ComputeDevice, DeviceId};

use super::{candidate_is_viable, run_tool, DeviceDetector, TOOL_TIMEOUT};

pub struct CudaDetector;

const SMI_ARGS: &[&str] = &[
    "--query-gpu=index,name,memory.total,memory.used,utilization.gpu,temperature.gpu",
    "--format=csv,noheader,nounits",
];

/// nvidia-smi 候选路径（按尝试顺序）
pub(crate) fn nvidia_smi_candidates() -> Vec<String> {
    let mut candidates = Vec::new();
    if let Ok(p) = std::env::var("EP_NVIDIA_SMI_PATH") {
        if !p.trim().is_empty() {
            candidates.push(p.trim().to_string());
        }
    }
    candidates.push("nvidia-smi".to_string());
    if cfg!(windows) {
        candidates.push(r"C:\Program Files\NVIDIA Corporation\NVSMI\nvidia-smi.exe".to_string());
    }
    candidates
}

fn run_nvidia_smi() -> Option<String> {
    for candidate in nvidia_smi_candidates() {
        if !candidate_is_viable(&candidate) {
            continue;
        }
        if let Some(output) = run_tool(&candidate, SMI_ARGS, TOOL_TIMEOUT) {
            note_compute_capabilities(&candidate);
            return Some(output);
        }
    }
    None
}

// ─── compute capability 记录（计划1 M1.2）───────────────────────────────────
//
// 设计取：cc 只走日志记录通道，不进 ComputeDevice 结构——当前无任何消费方
// （调度按估算、wheel 匹配属 M1.4），而结构体加字段需横扫 60 处字面量。
// M1.4（arch×backend wheel 供给）落地时再升级为结构化字段。

/// 已知架构判定边界（Blackwell sm_120 为止）。注意「未知」的判定必须覆盖
/// **主版本内的新 minor**：GB10 的 sm_121 正是主版本 12 的新 minor——Ollama
/// 白名单教训的原始形态，`major > 12` 式判定恰好把它漏成已知。
const KNOWN_MAX_CC_MAJOR: u8 = 12;

/// 已记录过的 cc（进程级去重：检测周期性运行，同一 cc 只记一次防日志刷屏）
fn noted_cc_cell() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    use std::sync::OnceLock;
    static CELL: OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    CELL.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// 解析 `--query-gpu=index,name,compute_cap` 输出（畸形行跳过）
fn parse_cc_output(output: &str) -> Vec<(u32, String, (u8, u8))> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(',').map(|f| f.trim()).collect();
            if fields.len() < 3 {
                return None;
            }
            let index = fields[0].parse().ok()?;
            let cc = fields[2].split_once('.')?;
            let major = cc.0.trim().parse().ok()?;
            let minor = cc.1.trim().parse().ok()?;
            Some((index, fields[1].to_string(), (major, minor)))
        })
        .collect()
}

/// 未知（更新）架构判定（纯函数，可单测）：低于已知最大主版本的全部已知；
/// 已知最大主版本内仅认已发布的 minor（sm_120）；更高主版本/更高 minor 一律
/// 视为未知——按可用处理，仅告警。
fn is_unknown_cc((major, minor): (u8, u8)) -> bool {
    match major.cmp(&KNOWN_MAX_CC_MAJOR) {
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => minor > 0,
        std::cmp::Ordering::Greater => true,
    }
}

/// 二次查询 cc 并记录（尽力而为：老驱动不认识 compute_cap 查询字段时整条
/// 查询失败，静默跳过——cc 缺失不影响设备可用性与任何判定）。
fn note_compute_capabilities(smi: &str) {
    let Some(output) = run_tool(
        smi,
        &["--query-gpu=index,name,compute_cap", "--format=csv,noheader,nounits"],
        TOOL_TIMEOUT,
    ) else {
        return;
    };
    for (index, name, cc) in parse_cc_output(&output) {
        let cc_label = format!("sm_{}{}", cc.0, cc.1);
        let key = format!("{index}:{cc_label}");
        if !noted_cc_cell().lock().unwrap_or_else(|e| e.into_inner()).insert(key) {
            continue;
        }
        if is_unknown_cc(cc) {
            tracing::warn!(
                device = %name,
                cc = %cc_label,
                "CUDA compute capability 未在已知列表（新架构）：按可用处理，不拒绝；\
                 如遇内核不兼容请为模块配置显式回退后端"
            );
        } else {
            tracing::debug!(device = %name, cc = %cc_label, "CUDA compute capability 已记录");
        }
    }
}

fn parse_mib(s: &str) -> Option<u32> {
    s.trim().parse::<u32>().ok()
}

fn parse_pct(s: &str) -> Option<u8> {
    s.trim().parse::<u8>().ok()
}

pub(crate) fn parse_smi_output(output: &str) -> Vec<ComputeDevice> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(',').map(|f| f.trim()).collect();
            if fields.len() < 6 {
                return None;
            }
            let index: u32 = fields[0].parse().ok()?;
            let name = fields[1].to_string();
            let total_memory_mb = parse_mib(fields[2]);
            let used_memory_mb = parse_mib(fields[3]);
            let utilization = parse_pct(fields[4]);
            let temperature = parse_pct(fields[5]);

            Some(ComputeDevice {
                id: DeviceId::Cuda(index),
                backend: ComputeBackend::Cuda,
                name,
                total_memory_mb,
                used_memory_mb,
                utilization,
                temperature,
            })
        })
        .collect()
}

/// 用最新采样刷新匹配设备的动态字段（P2：与 rocm/openvino 一致，采样字段
/// 缺失（None）时保留旧值，避免"采样瞬间字段缺失抹掉既有值"）
pub(crate) fn refresh_cuda_devices(devices: &mut [ComputeDevice], fresh: &[ComputeDevice]) {
    for dev in devices.iter_mut() {
        if dev.backend != ComputeBackend::Cuda {
            continue;
        }
        if let Some(updated) = fresh.iter().find(|f| f.id == dev.id) {
            dev.used_memory_mb = updated.used_memory_mb.or(dev.used_memory_mb);
            dev.utilization = updated.utilization.or(dev.utilization);
            dev.temperature = updated.temperature.or(dev.temperature);
        }
    }
}

impl DeviceDetector for CudaDetector {
    fn backend(&self) -> ComputeBackend {
        ComputeBackend::Cuda
    }

    fn detect(&self) -> Vec<ComputeDevice> {
        match run_nvidia_smi() {
            Some(output) => parse_smi_output(&output),
            None => Vec::new(),
        }
    }

    fn refresh(&self, devices: &mut [ComputeDevice]) {
        let Some(output) = run_nvidia_smi() else {
            return;
        };
        let fresh = parse_smi_output(&output);
        refresh_cuda_devices(devices, &fresh);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_gpu() {
        let output = "0, NVIDIA GeForce RTX 4090, 24564, 1234, 45, 62\n";
        let devices = parse_smi_output(output);
        assert_eq!(devices.len(), 1);
        let d = &devices[0];
        assert_eq!(d.id, DeviceId::Cuda(0));
        assert_eq!(d.name, "NVIDIA GeForce RTX 4090");
        assert_eq!(d.total_memory_mb, Some(24564));
        assert_eq!(d.used_memory_mb, Some(1234));
        assert_eq!(d.utilization, Some(45));
        assert_eq!(d.temperature, Some(62));
    }

    #[test]
    fn test_parse_multiple_gpus() {
        let output =
            "0, NVIDIA RTX 4090, 24564, 100, 10, 50\n1, NVIDIA RTX 3080, 10240, 200, 20, 55\n";
        let devices = parse_smi_output(output);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[1].id, DeviceId::Cuda(1));
        assert_eq!(devices[1].total_memory_mb, Some(10240));
    }

    #[test]
    fn test_parse_empty_output() {
        assert!(parse_smi_output("").is_empty());
        assert!(parse_smi_output("\n").is_empty());
    }

    #[test]
    fn test_parse_malformed_line() {
        let output = "garbage line\n0, RTX 4090, 24564, 100, 10, 50\n";
        let devices = parse_smi_output(output);
        assert_eq!(devices.len(), 1);
    }

    // ── 计划1 M1.2：compute capability 记录（未知新架构告警不拒绝）────────

    #[test]
    fn test_parse_cc_output() {
        let out = "0, NVIDIA GeForce RTX 4090, 8.9\n1, NVIDIA GB10, 12.1\nbad line\n2, X, N/A\n";
        let parsed = parse_cc_output(out);
        assert_eq!(parsed.len(), 2, "畸形/N/A 行跳过");
        assert_eq!(parsed[0], (0, "NVIDIA GeForce RTX 4090".into(), (8, 9)));
        assert_eq!(parsed[1], (1, "NVIDIA GB10".into(), (12, 1)));
    }

    #[test]
    fn test_unknown_cc_only_above_known_major() {
        // 已知区间（含 Blackwell sm_120 上限）不告警
        assert!(!is_unknown_cc((6, 1)));
        assert!(!is_unknown_cc((8, 9)));
        assert!(!is_unknown_cc((12, 0)));
        // 未知新架构（GB10 sm_121 主版本超限与未来架构）→ 告警但按可用处理
        assert!(is_unknown_cc((12, 1)));
        assert!(is_unknown_cc((13, 0)));
        assert!(is_unknown_cc((99, 9)));
    }

    /// 核心语义回归：未知 cc 不产生任何拒绝路径——设备解析与 cc 完全解耦
    /// （sm_121 设备照样产出完整 ComputeDevice 记录）
    #[test]
    fn test_device_detection_independent_of_cc() {
        let output = "0, NVIDIA GB10, 126336, 100, 10, 50\n";
        let devices = parse_smi_output(output);
        assert_eq!(devices.len(), 1, "未知 cc 设备不因 cc 被过滤");
        assert_eq!(devices[0].name, "NVIDIA GB10");
        assert_eq!(devices[0].total_memory_mb, Some(126336));
    }

    #[test]
    fn test_parse_invalid_numbers() {
        let output = "0, RTX 4090, N/A, N/A, N/A, N/A\n";
        let devices = parse_smi_output(output);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].total_memory_mb, None);
        assert_eq!(devices[0].utilization, None);
    }

    #[test]
    fn test_candidates_always_include_path_lookup() {
        // 无论平台，PATH 探测候选必须存在（nvidia-smi 在 PATH 的用户真机依赖此项）
        assert!(nvidia_smi_candidates().iter().any(|c| c == "nvidia-smi"));
    }

    /// P2 回归：采样瞬间字段缺失（N/A）不得抹掉既有值（.or() 保留旧值）
    #[test]
    fn test_refresh_preserves_old_values_when_sample_field_missing() {
        let mut devices = vec![ComputeDevice {
            id: DeviceId::Cuda(0),
            backend: ComputeBackend::Cuda,
            name: "RTX 4090".to_string(),
            total_memory_mb: Some(24564),
            used_memory_mb: Some(1234),
            utilization: Some(45),
            temperature: Some(62),
        }];
        // 最新采样：utilization/temperature 为 N/A（缺失），used 有更新值
        let fresh = vec![ComputeDevice {
            id: DeviceId::Cuda(0),
            backend: ComputeBackend::Cuda,
            name: "RTX 4090".to_string(),
            total_memory_mb: Some(24564),
            used_memory_mb: Some(999),
            utilization: None,
            temperature: None,
        }];
        refresh_cuda_devices(&mut devices, &fresh);
        assert_eq!(devices[0].used_memory_mb, Some(999), "有效新值应更新");
        assert_eq!(devices[0].utilization, Some(45), "缺失字段保留旧值");
        assert_eq!(devices[0].temperature, Some(62), "缺失字段保留旧值");

        // 非 CUDA 设备不被触碰
        let mut other = vec![ComputeDevice {
            id: DeviceId::Rocm(0),
            backend: ComputeBackend::Rocm,
            name: "AMD".to_string(),
            total_memory_mb: None,
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        }];
        refresh_cuda_devices(&mut other, &fresh);
        assert_eq!(other[0].utilization, None);
    }

    #[cfg(windows)]
    #[test]
    fn test_windows_candidates_include_legacy_driver_path() {
        assert!(nvidia_smi_candidates()
            .iter()
            .any(|c| c.contains("NVSMI")));
    }
}
