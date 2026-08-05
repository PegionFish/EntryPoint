//! 计算设备调度器 — Wave 1a Agent B 实现
//!
//! 根据策略为模块分配计算设备。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;

use crate::types::{ComputeBackend, ComputeDevice, DeviceId, DeviceScheduler, SchedulingStrategy};

/// 设备调度器
///
/// `assign()` 在 trait 中签名是 `&self`（不可变引用），因此需要
/// interior mutability：`AtomicUsize` 用于 round-robin 索引，
/// `RwLock` 用于运行时可变的分配记录。
pub struct ComputeScheduler {
    devices: Vec<ComputeDevice>,
    /// module_id → DeviceId（RwLock 以支持 &self 下的 assign）
    assignments: RwLock<HashMap<String, DeviceId>>,
    strategy: SchedulingStrategy,
    /// 轮询索引（AtomicUsize 以支持 &self 下的可变更新）
    round_robin_index: AtomicUsize,
    /// 是否允许显存超分配
    allow_overcommit: bool,
    /// 每个设备已被分配的显存 (MB)
    allocated_per_device: RwLock<HashMap<String, u32>>,
    /// 每个模块分配的显存 (MB)，用于 release 时精确释放
    vram_per_module: RwLock<HashMap<String, u32>>,
}

/// 设备分配摘要（由 `status_report` 返回）
#[derive(Debug, Clone)]
pub struct DeviceAssignmentSummary {
    pub device_id: DeviceId,
    pub device_name: String,
    pub total_memory_mb: Option<u32>,
    pub allocated_memory_mb: u32,
    pub assigned_modules: Vec<String>,
}

impl ComputeScheduler {
    pub fn new(devices: Vec<ComputeDevice>, strategy: SchedulingStrategy) -> Self {
        Self {
            devices,
            assignments: RwLock::new(HashMap::new()),
            strategy,
            round_robin_index: AtomicUsize::new(0),
            allow_overcommit: false,
            allocated_per_device: RwLock::new(HashMap::new()),
            vram_per_module: RwLock::new(HashMap::new()),
        }
    }

    pub fn set_allow_overcommit(&mut self, allow: bool) {
        self.allow_overcommit = allow;
    }

    /// 筛选后端兼容的设备
    fn compatible_devices(&self, backends: &[ComputeBackend]) -> Vec<usize> {
        self.devices
            .iter()
            .enumerate()
            .filter(|(_, d)| backends.contains(&d.backend))
            .map(|(i, _)| i)
            .collect()
    }

    /// 设备 key（用于 allocated_per_device map）
    fn device_key(id: &DeviceId) -> String {
        format!("{id}")
    }

    /// LeastMemory 选择用的排序键：`(容量是否已知, 剩余显存)`。
    ///
    /// D-3 语义修正：`total_memory_mb == None`（容量未知：CPU/OpenVINO/DirectML
    /// 等探测不到总量的设备）不再伪装成无限容量参与比较。兼容设备排序时
    /// **已知容量设备在前**（按剩余降序），**未知容量设备殿后**（仍可选，
    /// 只是最后）。`max_by_key` 按字典序比较元组：`(true, _)` 恒大于
    /// `(false, _)`；候选全部未知容量时退化为检测顺序（同键取最后，
    /// 与修正前的平局行为一致）。
    fn least_memory_key(&self, device_index: usize) -> (bool, u32) {
        let device = &self.devices[device_index];
        match device.total_memory_mb {
            Some(_) => (true, self.remaining_memory(device_index)),
            None => (false, 0),
        }
    }

    /// 设备剩余显存。
    /// `total_memory_mb == None` 视为无限制（如 CPU），返回 `u32::MAX`。
    fn remaining_memory(&self, device_index: usize) -> u32 {
        let device = &self.devices[device_index];
        let total = match device.total_memory_mb {
            Some(t) => t,
            None => return u32::MAX,
        };
        let key = Self::device_key(&device.id);
        let allocated = self
            .allocated_per_device
            .read()
            .unwrap()
            .get(&key)
            .copied()
            .unwrap_or(0);
        total.saturating_sub(allocated)
    }

    /// 在指定设备上记录显存分配
    fn record_allocation(&self, device_index: usize, vram_mb: u32) {
        let key = Self::device_key(&self.devices[device_index].id);
        let mut alloc = self.allocated_per_device.write().unwrap();
        *alloc.entry(key).or_insert(0) += vram_mb;
    }

    /// 从指定设备移除显存分配
    fn remove_allocation(&self, device_id: &DeviceId, vram_mb: u32) {
        let key = Self::device_key(device_id);
        let mut alloc = self.allocated_per_device.write().unwrap();
        if let Some(entry) = alloc.get_mut(&key) {
            *entry = entry.saturating_sub(vram_mb);
        }
    }

    /// 所有设备的分配摘要
    pub fn status_report(&self) -> Vec<DeviceAssignmentSummary> {
        let assignments = self.assignments.read().unwrap();
        let allocated = self.allocated_per_device.read().unwrap();

        self.devices
            .iter()
            .map(|device| {
                let key = Self::device_key(&device.id);
                let assigned_modules: Vec<String> = assignments
                    .iter()
                    .filter(|(_, did)| **did == device.id)
                    .map(|(mid, _)| mid.clone())
                    .collect();

                DeviceAssignmentSummary {
                    device_id: device.id.clone(),
                    device_name: device.name.clone(),
                    total_memory_mb: device.total_memory_mb,
                    allocated_memory_mb: allocated.get(&key).copied().unwrap_or(0),
                    assigned_modules,
                }
            })
            .collect()
    }
}

impl DeviceScheduler for ComputeScheduler {
    fn assign(
        &self,
        module_id: &str,
        backends: &[ComputeBackend],
        vram_mb: u32,
    ) -> Option<DeviceId> {
        // 若该模块已有分配，直接返回
        if let Some(existing) = self.assignments.read().unwrap().get(module_id) {
            return Some(existing.clone());
        }

        let compatible = self.compatible_devices(backends);
        if compatible.is_empty() {
            return None;
        }

        let selected_index = match self.strategy {
            SchedulingStrategy::Manual => return None,

            // D-3：已知容量设备按剩余降序在前，未知容量设备殿后（仍可选）
            SchedulingStrategy::LeastMemory => compatible
                .iter()
                .copied()
                .max_by_key(|&idx| self.least_memory_key(idx))?,

            SchedulingStrategy::RoundRobin => {
                let idx_pos =
                    self.round_robin_index.fetch_add(1, Ordering::SeqCst) % compatible.len();
                compatible[idx_pos]
            }

            SchedulingStrategy::Single => compatible[0],
        };

        // 显存检查
        if vram_mb > 0 {
            let remaining = self.remaining_memory(selected_index);
            if vram_mb > remaining {
                if self.allow_overcommit {
                    tracing::warn!(
                        "Device {} overcommit: requested {} MB, remaining {} MB",
                        self.devices[selected_index].id,
                        vram_mb,
                        remaining
                    );
                } else {
                    return None;
                }
            }
        }

        // 记录分配
        let device_id = self.devices[selected_index].id.clone();
        self.assignments
            .write()
            .unwrap()
            .insert(module_id.to_string(), device_id.clone());
        self.record_allocation(selected_index, vram_mb);
        self.vram_per_module
            .write()
            .unwrap()
            .insert(module_id.to_string(), vram_mb);

        Some(device_id)
    }

    fn release(&mut self, module_id: &str) {
        let vram = self.vram_per_module.write().unwrap().remove(module_id);
        if let Some(device_id) = self.assignments.write().unwrap().remove(module_id) {
            if let Some(vram_mb) = vram {
                self.remove_allocation(&device_id, vram_mb);
            }
        }
    }

    fn devices(&self) -> &[ComputeDevice] {
        &self.devices
    }
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ComputeBackend, SchedulingStrategy};

    fn make_device(id: DeviceId, name: &str, total_mb: Option<u32>) -> ComputeDevice {
        let backend = id.backend();
        ComputeDevice {
            id,
            backend,
            name: name.to_string(),
            total_memory_mb: total_mb,
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        }
    }

    fn cuda_device(index: u32, name: &str, total_mb: u32) -> ComputeDevice {
        make_device(DeviceId::Cuda(index), name, Some(total_mb))
    }

    fn cpu_device() -> ComputeDevice {
        make_device(DeviceId::Cpu, "CPU", None)
    }

    #[test]
    fn test_least_memory_strategy() {
        let devices = vec![
            cuda_device(0, "GPU-Small", 4096),
            cuda_device(1, "GPU-Large", 8192),
            cuda_device(2, "GPU-Medium", 6144),
        ];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);

        let result = scheduler.assign("mod_a", &[ComputeBackend::Cuda], 1024);
        assert_eq!(result, Some(DeviceId::Cuda(1))); // 8192 剩余最大

        let result = scheduler.assign("mod_b", &[ComputeBackend::Cuda], 1024);
        assert_eq!(result, Some(DeviceId::Cuda(1))); // 仍有 7168 剩余，最大
    }

    #[test]
    fn test_round_robin_strategy() {
        let devices = vec![
            cuda_device(0, "GPU-0", 8192),
            cuda_device(1, "GPU-1", 8192),
        ];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::RoundRobin);

        let r1 = scheduler.assign("mod_a", &[ComputeBackend::Cuda], 100);
        assert_eq!(r1, Some(DeviceId::Cuda(0)));

        let r2 = scheduler.assign("mod_b", &[ComputeBackend::Cuda], 100);
        assert_eq!(r2, Some(DeviceId::Cuda(1)));

        let r3 = scheduler.assign("mod_c", &[ComputeBackend::Cuda], 100);
        assert_eq!(r3, Some(DeviceId::Cuda(0))); // 回到第一个
    }

    #[test]
    fn test_single_strategy() {
        let devices = vec![
            cuda_device(0, "GPU-0", 4096),
            cuda_device(1, "GPU-1", 8192),
        ];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::Single);

        let r1 = scheduler.assign("mod_a", &[ComputeBackend::Cuda], 100);
        assert_eq!(r1, Some(DeviceId::Cuda(0)));

        let r2 = scheduler.assign("mod_b", &[ComputeBackend::Cuda], 100);
        assert_eq!(r2, Some(DeviceId::Cuda(0))); // 始终选第一个
    }

    #[test]
    fn test_manual_returns_none() {
        let devices = vec![cuda_device(0, "GPU-0", 8192)];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::Manual);

        let result = scheduler.assign("mod_a", &[ComputeBackend::Cuda], 100);
        assert_eq!(result, None);
    }

    #[test]
    fn test_backend_compatibility() {
        let devices = vec![
            cuda_device(0, "NVIDIA", 8192),
            cpu_device(),
        ];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);

        // 请求 CUDA 但只有 CPU 兼容 → 应返回 None
        let result = scheduler.assign("mod_a", &[ComputeBackend::Rocm], 100);
        assert_eq!(result, None);

        // 请求 CPU → 应返回 Cpu
        let result = scheduler.assign("mod_b", &[ComputeBackend::Cpu], 0);
        assert_eq!(result, Some(DeviceId::Cpu));

        // 请求 Cuda 或 Cpu → LeastMemory 选已知容量的 cuda:0
        //（D-3：容量未知的 CPU 不再伪装无限容量压过真 GPU，而是殿后）
        let result = scheduler.assign("mod_c", &[ComputeBackend::Cuda, ComputeBackend::Cpu], 100);
        assert_eq!(result, Some(DeviceId::Cuda(0)));
    }

    // ── D-3：None 容量 = 容量未知，殿后但可选 ──────────────────────────────

    #[test]
    fn test_least_memory_prefers_known_capacity_over_unknown() {
        // cuda 有容量 + openvino None（任务要求的构造）：
        // 即便未知容量设备排在设备表首位，LeastMemory 仍选已知容量的 cuda
        let devices = vec![
            make_device(DeviceId::OpenVINO("GPU.0".into()), "Intel NPU", None),
            cuda_device(0, "NVIDIA GPU", 8192),
        ];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);

        let result = scheduler.assign(
            "mod_a",
            &[ComputeBackend::OpenVINO, ComputeBackend::Cuda],
            1024,
        );
        assert_eq!(result, Some(DeviceId::Cuda(0)));
    }

    #[test]
    fn test_least_memory_known_beats_unknown_regardless_of_size() {
        // 已知容量设备哪怕剩余很小，排序也在任何未知容量设备之前
        let devices = vec![
            cuda_device(0, "Small GPU", 512),
            make_device(DeviceId::OpenVINO("GPU.0".into()), "Intel NPU", None),
        ];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);

        let result = scheduler.assign(
            "mod_a",
            &[ComputeBackend::Cuda, ComputeBackend::OpenVINO],
            100,
        );
        assert_eq!(result, Some(DeviceId::Cuda(0)));
    }

    #[test]
    fn test_least_memory_unknown_capacity_devices_still_selectable() {
        // 候选全部容量未知 → 仍可分配（殿后不等于淘汰；平局取检测序最后）
        let devices = vec![
            make_device(DeviceId::OpenVINO("GPU.0".into()), "Intel NPU", None),
            cpu_device(),
        ];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);

        let result = scheduler.assign(
            "mod_a",
            &[ComputeBackend::OpenVINO, ComputeBackend::Cpu],
            1024,
        );
        assert_eq!(result, Some(DeviceId::Cpu));
    }

    #[test]
    fn test_least_memory_known_devices_ranked_by_remaining_desc() {
        // 已知容量设备之间仍按剩余降序（D-3 只改未知容量的位置，不改组内排序）
        let devices = vec![
            cuda_device(0, "GPU-A", 4096),
            make_device(DeviceId::OpenVINO("GPU.0".into()), "Intel NPU", None),
            cuda_device(1, "GPU-B", 8192),
        ];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);

        let result = scheduler.assign(
            "mod_a",
            &[ComputeBackend::Cuda, ComputeBackend::OpenVINO],
            1024,
        );
        assert_eq!(result, Some(DeviceId::Cuda(1)));
    }

    #[test]
    fn test_vram_overcommit_blocked() {
        let devices = vec![cuda_device(0, "GPU-0", 4096)];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);
        // allow_overcommit 默认 false

        let result = scheduler.assign("mod_a", &[ComputeBackend::Cuda], 5000);
        assert_eq!(result, None); // 超出 4096，拒绝
    }

    #[test]
    fn test_vram_overcommit_allowed() {
        let devices = vec![cuda_device(0, "GPU-0", 4096)];
        let mut scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);
        scheduler.set_allow_overcommit(true);

        let result = scheduler.assign("mod_a", &[ComputeBackend::Cuda], 5000);
        assert_eq!(result, Some(DeviceId::Cuda(0))); // 允许超分，分配成功
    }

    #[test]
    fn test_release_frees_device() {
        let devices = vec![cuda_device(0, "GPU-0", 4096)];
        let mut scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);

        // 分配 3000 MB
        let r1 = scheduler.assign("mod_a", &[ComputeBackend::Cuda], 3000);
        assert_eq!(r1, Some(DeviceId::Cuda(0)));

        // 再分配 2000 MB 应失败（剩余 1096 < 2000）
        let r2 = scheduler.assign("mod_b", &[ComputeBackend::Cuda], 2000);
        assert_eq!(r2, None);

        // 释放 mod_a
        scheduler.release("mod_a");

        // 现在 2000 MB 应可分配
        let r3 = scheduler.assign("mod_c", &[ComputeBackend::Cuda], 2000);
        assert_eq!(r3, Some(DeviceId::Cuda(0)));
    }

    #[test]
    fn test_status_report() {
        let devices = vec![
            cuda_device(0, "GPU-0", 8192),
            cuda_device(1, "GPU-1", 4096),
        ];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);

        scheduler.assign("mod_a", &[ComputeBackend::Cuda], 1024).unwrap();

        let report = scheduler.status_report();
        assert_eq!(report.len(), 2);
        assert_eq!(report[0].assigned_modules, vec!["mod_a".to_string()]);
        assert_eq!(report[0].allocated_memory_mb, 1024);
        assert!(report[1].assigned_modules.is_empty());
        assert_eq!(report[1].allocated_memory_mb, 0);
    }
}
