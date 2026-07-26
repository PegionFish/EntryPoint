//! 计算设备调度器 — Wave 1a Agent B 实现
//!
//! 根据策略为模块分配计算设备。

use std::collections::HashMap;

use crate::types::{ComputeBackend, ComputeDevice, DeviceId, DeviceScheduler, SchedulingStrategy};

/// 设备调度器
pub struct ComputeScheduler {
    devices: Vec<ComputeDevice>,
    assignments: HashMap<String, DeviceId>,
    strategy: SchedulingStrategy,
    round_robin_index: usize,
    allow_overcommit: bool,
}

impl ComputeScheduler {
    pub fn new(devices: Vec<ComputeDevice>, strategy: SchedulingStrategy) -> Self {
        Self {
            devices,
            assignments: HashMap::new(),
            strategy,
            round_robin_index: 0,
            allow_overcommit: false,
        }
    }

    pub fn set_allow_overcommit(&mut self, allow: bool) {
        self.allow_overcommit = allow;
    }

    fn compatible_devices(&self, backends: &[ComputeBackend]) -> Vec<&ComputeDevice> {
        self.devices
            .iter()
            .filter(|d| backends.contains(&d.backend))
            .collect()
    }
}

impl DeviceScheduler for ComputeScheduler {
    fn assign(
        &self,
        _module_id: &str,
        _backends: &[ComputeBackend],
        _vram_mb: u32,
    ) -> Option<DeviceId> {
        // TODO: Wave 1a Agent B — implement four strategies
        todo!("ComputeScheduler::assign — implement in Wave 1a")
    }

    fn release(&mut self, _module_id: &str) {
        // TODO: Wave 1a Agent B
        todo!("ComputeScheduler::release — implement in Wave 1a")
    }

    fn devices(&self) -> &[ComputeDevice] {
        &self.devices
    }
}
