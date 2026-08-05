//! Microsoft DirectML 设备检测（best-effort，轻量实现）
//!
//! 实现策略（PACK_UNIFY_PLAN §7 / §15.4）：
//! - **不引入重量级 windows/dxgi 原生依赖**；仅通过 wmic / PowerShell 枚举显示适配器
//!   作为轻量信号（两者均带子进程超时，wmic 在 Win11 24H2+ 被移除时自动落到 PowerShell）
//! - 仅 Windows 平台产生设备：DirectML 是 Windows 加速技术，非 Windows 一律返回空列表
//! - `AdapterRAM`（DWORD）存在 4GB 上限不可靠，故 `total_memory_mb` 一律 None
//! - 无轻量利用率/显存占用信号源，refresh 保持既有值
//!
//! **与 OpenVINO（及 CUDA/ROCm）并存时的去重/优先级策略**：
//! - 后端优先级：CUDA > ROCm > OpenVINO > DirectML（`mod.rs` 注册顺序 +
//!   `detect_all_devices` 保证 DirectML 在高优先级后端之后探测）
//! - DirectML 只报告**尚未被高优先级后端覆盖**的显示适配器（归一化名称匹配去重，
//!   见 `normalize_device_name`：小写 + 去 (R)/(TM)/(C) + 空白折叠）
//! - 典型结果：NVIDIA 独显 + Intel iGPU 的机器上，NVIDIA 归 CUDA、iGPU 归 OpenVINO，
//!   DirectML 列表为空——这是去重策略的**预期结果**，而非检测失败；
//!   仅当存在其他后端未覆盖的适配器（如 AMD 显卡无 ROCm 驱动时）DirectML 才列设备
//! - 去重后的 DirectML 设备索引按剩余适配器顺序从 0 连续编号

use crate::types::{ComputeBackend, ComputeDevice, DeviceId};

use super::{normalize_device_name, DeviceDetector};

pub struct DirectMlDetector;

impl DirectMlDetector {
    /// 检测 DirectML 可用适配器，并对已知（更高优先级后端）设备去重。
    /// `known` 为 CUDA/ROCm/OpenVINO 已检出的设备列表。
    pub fn detect_excluding(&self, known: &[ComputeDevice]) -> Vec<ComputeDevice> {
        build_directml_devices(enumerate_display_adapters(), known)
    }
}

impl DeviceDetector for DirectMlDetector {
    fn backend(&self) -> ComputeBackend {
        ComputeBackend::DirectML
    }

    /// 独立检测（无去重上下文）——枚举全部显示适配器
    fn detect(&self) -> Vec<ComputeDevice> {
        self.detect_excluding(&[])
    }

    fn refresh(&self, _devices: &mut [ComputeDevice]) {
        // 无轻量利用率/显存信号源（不引入 dxgi 原生依赖），保持既有值
    }
}

/// 枚举 Windows 显示适配器名称。非 Windows 返回空（DirectML 为 Windows 技术）。
#[cfg(windows)]
fn enumerate_display_adapters() -> Vec<String> {
    super::windows_video_controller_names()
}

#[cfg(not(windows))]
fn enumerate_display_adapters() -> Vec<String> {
    Vec::new()
}

/// 纯函数：由适配器名列表构建 DirectML 设备（去重 + 编号），fixture 可测。
///
/// - 跳过空名与 Microsoft 基本显示适配器（无加速能力）
/// - 与 `known` 中非 DirectML 设备归一化名称相同 → 视为已被更高优先级后端覆盖，跳过
/// - 剩余适配器按顺序编号 `directml:0..n`
pub(crate) fn build_directml_devices(
    adapters: Vec<String>,
    known: &[ComputeDevice],
) -> Vec<ComputeDevice> {
    let known_normalized: Vec<String> = known
        .iter()
        .filter(|d| d.backend != ComputeBackend::DirectML)
        .map(|d| normalize_device_name(&d.name))
        .collect();

    let mut devices: Vec<ComputeDevice> = Vec::new();
    for raw in adapters {
        let name = raw.trim().to_string();
        if name.is_empty() {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        // Microsoft Basic Display Adapter / Basic Render Device：兜底驱动，无加速能力
        if lower.contains("basic display") || lower.contains("basic render") {
            continue;
        }
        // 虚拟显示适配器（Parsec/RDP 等）：无物理 GPU，DirectML 不可用（真机实测存在）
        if lower.contains("virtual display") || lower.contains("remote display") {
            continue;
        }
        // Indirect Display Driver 虚拟显卡（向日葵 OrayIddDriver / ToDesk 等
        // 远程工具的 Idd 虚拟显示器）：同属无物理 GPU 的虚拟适配器
        if lower.contains("idd driver") || lower.contains("oray") {
            continue;
        }
        if known_normalized.contains(&normalize_device_name(&name)) {
            continue; // 已被 CUDA/ROCm/OpenVINO 覆盖
        }
        let index = devices.len() as u32;
        devices.push(ComputeDevice {
            id: DeviceId::DirectML(index),
            backend: ComputeBackend::DirectML,
            name,
            total_memory_mb: None, // AdapterRAM 4GB 上限不可靠，统一 None
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        });
    }
    devices
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DeviceId;

    fn known(backend: ComputeBackend, id: DeviceId, name: &str) -> ComputeDevice {
        ComputeDevice {
            id,
            backend,
            name: name.to_string(),
            total_memory_mb: None,
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        }
    }

    #[test]
    fn test_no_known_devices_lists_all_adapters() {
        let adapters = vec![
            "NVIDIA GeForce RTX 5090 D".to_string(),
            "Intel(R) Graphics".to_string(),
        ];
        let devices = build_directml_devices(adapters, &[]);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].id, DeviceId::DirectML(0));
        assert_eq!(devices[0].name, "NVIDIA GeForce RTX 5090 D");
        assert_eq!(devices[1].id, DeviceId::DirectML(1));
        assert_eq!(devices[1].backend, ComputeBackend::DirectML);
        assert_eq!(devices[1].total_memory_mb, None);
    }

    #[test]
    fn test_dedup_against_cuda_and_openvino() {
        // 用户真机形态：NVIDIA 独显（CUDA 覆盖）+ Intel iGPU（OpenVINO 覆盖）
        let known = vec![
            known(
                ComputeBackend::Cuda,
                DeviceId::Cuda(0),
                "NVIDIA GeForce RTX 5090 D",
            ),
            known(
                ComputeBackend::OpenVINO,
                DeviceId::OpenVINO("GPU.0".to_string()),
                "Intel(R) Graphics",
            ),
        ];
        let adapters = vec![
            "NVIDIA GeForce RTX 5090 D".to_string(),
            "Intel(R) Graphics".to_string(),
        ];
        let devices = build_directml_devices(adapters, &known);
        // 全部被更高优先级后端覆盖 → DirectML 列表为空（预期去重结果）
        assert!(devices.is_empty());
    }

    #[test]
    fn test_dedup_name_normalization_variants() {
        // 归一化应容忍 (R) 标记与大小写/空白差异
        let known = vec![known(
            ComputeBackend::OpenVINO,
            DeviceId::OpenVINO("GPU.0".to_string()),
            "Intel(R) Graphics",
        )];
        let adapters = vec!["intel  graphics".to_string(), "INTEL(R) GRAPHICS".to_string()];
        assert!(build_directml_devices(adapters, &known).is_empty());
    }

    #[test]
    fn test_uncovered_adapter_survives_with_contiguous_index() {
        let known = vec![known(
            ComputeBackend::Cuda,
            DeviceId::Cuda(0),
            "NVIDIA GeForce RTX 5090 D",
        )];
        let adapters = vec![
            "NVIDIA GeForce RTX 5090 D".to_string(),
            "AMD Radeon RX 7900 XTX".to_string(), // 无 ROCm 驱动场景 → 未被覆盖
            "Intel(R) Graphics".to_string(),
        ];
        let devices = build_directml_devices(adapters, &known);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].name, "AMD Radeon RX 7900 XTX");
        assert_eq!(devices[0].id, DeviceId::DirectML(0)); // 去重后连续编号
        assert_eq!(devices[1].id, DeviceId::DirectML(1));
    }

    #[test]
    fn test_basic_and_virtual_adapters_skipped() {
        let adapters = vec![
            "Microsoft Basic Display Adapter".to_string(),
            "Microsoft Basic Render Device".to_string(),
            "Parsec Virtual Display Adapter".to_string(), // 虚拟适配器无物理 GPU
            "Microsoft Remote Display Adapter".to_string(),
            "Intel(R) Graphics".to_string(),
        ];
        let devices = build_directml_devices(adapters, &[]);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "Intel(R) Graphics");
    }

    #[test]
    fn test_idd_and_oray_virtual_adapters_skipped() {
        // 远程工具的 Indirect Display Driver 虚拟显卡（向日葵 OrayIddDriver、
        // ToDesk 等），无物理 GPU，DirectML 不可用（真机实测存在）
        let adapters = vec![
            "OrayIddDriver Device".to_string(),
            "ToDesk Idd Driver".to_string(),
            "Parsec Virtual Display Adapter".to_string(),
            "Intel(R) Graphics".to_string(),
        ];
        let devices = build_directml_devices(adapters, &[]);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "Intel(R) Graphics");
    }

    #[test]
    fn test_empty_and_whitespace_names_skipped() {
        let adapters = vec!["".to_string(), "   ".to_string(), "Intel(R) Graphics".to_string()];
        let devices = build_directml_devices(adapters, &[]);
        assert_eq!(devices.len(), 1);
    }

    #[test]
    fn test_existing_directml_devices_not_used_as_dedup_source() {
        // known 中的 DirectML 设备不参与去重源（去重只看更高优先级后端）
        let known = vec![known(
            ComputeBackend::DirectML,
            DeviceId::DirectML(0),
            "Intel(R) Graphics",
        )];
        let adapters = vec!["Intel(R) Graphics".to_string()];
        let devices = build_directml_devices(adapters, &known);
        assert_eq!(devices.len(), 1);
    }

    #[cfg(not(windows))]
    #[test]
    fn test_non_windows_enumeration_is_empty() {
        assert!(enumerate_display_adapters().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn test_detect_trait_returns_vec_without_panic() {
        // 真机冒烟：无论 wmic/powershell 是否可用都不得 panic
        let detector = DirectMlDetector;
        let _ = detector.detect();
    }
}
