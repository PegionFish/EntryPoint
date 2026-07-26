//! 异构计算设备管理 — Wave 1 Agent B 实现

pub mod cpu;
pub mod cuda;
pub mod scheduler;

pub use cpu::CpuDetector;
pub use cuda::CudaDetector;

use crate::types::{ComputeBackend, ComputeDevice};

/// 计算设备检测 trait — 所有后端实现此接口
pub trait DeviceDetector: Send + Sync {
    fn backend(&self) -> ComputeBackend;
    fn detect(&self) -> Vec<ComputeDevice>;
    fn refresh(&self, devices: &mut [ComputeDevice]);
}

fn all_detectors() -> Vec<Box<dyn DeviceDetector>> {
    vec![Box::new(CudaDetector), Box::new(CpuDetector)]
}

/// 检测所有可用计算设备
pub fn detect_all_devices(disabled: &[ComputeBackend]) -> Vec<ComputeDevice> {
    all_detectors()
        .iter()
        .filter(|d| !disabled.contains(&d.backend()))
        .flat_map(|d| d.detect())
        .collect()
}

/// 刷新已检测设备的动态状态（显存占用、利用率、温度）
pub fn refresh_all_devices(devices: &mut [ComputeDevice], disabled: &[ComputeBackend]) {
    for detector in all_detectors() {
        if disabled.contains(&detector.backend()) {
            continue;
        }
        detector.refresh(devices);
    }
}
