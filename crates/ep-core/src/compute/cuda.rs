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
            return Some(output);
        }
    }
    None
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
