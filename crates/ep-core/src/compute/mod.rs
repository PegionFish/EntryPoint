//! 异构计算设备管理 — Wave 1 Agent B 实现

pub mod cuda;
pub mod cpu;

use crate::types::{ComputeBackend, ComputeDevice};

/// 计算设备检测 trait — 所有后端实现此接口
pub trait DeviceDetector: Send + Sync {
    fn backend(&self) -> ComputeBackend;
    fn detect(&self) -> Vec<ComputeDevice>;
    fn refresh(&self, devices: &mut [ComputeDevice]);
}

/// 检测所有可用计算设备
pub fn detect_all_devices(disabled: &[ComputeBackend]) -> Vec<ComputeDevice> {
    let detectors: Vec<Box<dyn DeviceDetector>> = vec![
        Box::new(cuda::CudaDetector),
        Box::new(cpu::CpuDetector),
    ];

    detectors
        .iter()
        .filter(|d| !disabled.contains(&d.backend()))
        .flat_map(|d| d.detect())
        .collect()
}
