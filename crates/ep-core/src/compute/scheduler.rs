//! 计算设备调度器 — Wave 1a Agent B 实现
//!
//! 根据策略为模块分配计算设备。
//!
//! ## 模块级设备选择（D-4 下沉，daemon/桌面共享）
//!
//! [`ComputeScheduler::assign_module_device`] 是两端共用的选择核心：
//! manifest backends 兼容过滤（尊重 `[compute].disabled_backends`）→
//! 非 CPU 加速优先（strategy + VRAM 闸门）→ CPU 保底。
//! 桌面端经常驻调度器调用（带显存记账）；daemon 三处经
//! [`select_device_for_module`] 以临时调度器做无状态一次性选择
//! （语义完全同源）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;

use crate::config::{AppConfig, AssignStrategy};
use crate::model::active_model_for;
use crate::module::manifest::ModuleManifest;
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
    /// 全局禁用后端（`[compute].disabled_backends`）— 共享选择时从 manifest
    /// 兼容集中剔除（设备探测与选择双重保险，配置热改后不依赖下一轮重探测）
    disabled_backends: Vec<ComputeBackend>,
    /// Single 策略指定的设备名（如 "cuda:1"；None = 回退第一个兼容设备）
    single_device: Option<String>,
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
            disabled_backends: Vec::new(),
            single_device: None,
        }
    }

    pub fn set_allow_overcommit(&mut self, allow: bool) {
        self.allow_overcommit = allow;
    }

    /// 设置 Single 策略的指定设备名（`[compute].single_device`，如 "cuda:1"）。
    /// 配置了名称时 Single 按 DeviceId 字符串匹配兼容设备，找不到回退 [0]。
    pub fn set_single_device(&mut self, name: Option<String>) {
        self.single_device = name;
    }

    /// 设置全局禁用后端（`[compute].disabled_backends`，共享选择尊重该清单）
    pub fn set_disabled_backends(&mut self, disabled: Vec<ComputeBackend>) {
        self.disabled_backends = disabled;
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

    /// 设备剩余显存（VRAM 闸门与 LeastMemory 排序共用）。
    ///
    /// - `total_memory_mb == None` 视为无限制（如 CPU），返回 `u32::MAX`；
    /// - 已知容量：`total − max(账面已分配, 实时已用)`。
    ///
    /// D-6：实时已用来自检测器刷新采样（`used_memory_mb`，如 nvidia-smi）。
    /// 外部程序占用显存时账面值低估实际占用，取两者较大值避免闸门失明；
    /// 本调度器自身的分配也体现在实时采样里，`max` 而非相加，不重复记账。
    /// `used_memory_mb == None`（无采样源）忽略，按 0 计。
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
        let live_used = device.used_memory_mb.unwrap_or(0);
        total.saturating_sub(allocated.max(live_used))
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

    /// 模块级设备选择 — daemon/桌面共享实现（D-4），替代 daemon 旧 first-match
    /// 与桌面端同名自由函数。语义：
    ///
    /// 1. 按 manifest `[compute].backends` 兼容过滤，并剔除
    ///    `[compute].disabled_backends`；过滤后为空 → `None`；
    /// 2. 非 CPU 加速后端优先：以加速后端组走 [`DeviceScheduler::assign`]
    ///    （四策略与 VRAM 闸门在此生效；`allow_overcommit=false` 超限拒绝）；
    /// 3. 加速组被拒（无设备 / Manual / 显存闸门）且 manifest 声明 CPU →
    ///    CPU 保底（绕过调度器与显存闸门：CPU 恒可用，与桌面原语义一致）；
    /// 4. 无任何兼容设备 → `None`（调用方决定错误/兜底语义）。
    ///
    /// 纯 CPU 模块（backends 仅含 CPU）直接走 CPU 后端分配路径。
    /// `vram_mb` 为调度显存请求量（见 [`module_vram_request`]）。
    pub fn assign_module_device(
        &self,
        module_id: &str,
        manifest: &ModuleManifest,
        vram_mb: u32,
    ) -> Option<DeviceId> {
        // 1. 兼容过滤（含禁用后端）
        let backends: Vec<ComputeBackend> = manifest
            .compute
            .backends
            .iter()
            .copied()
            .filter(|b| !self.disabled_backends.contains(b))
            .collect();
        if backends.is_empty() {
            return None;
        }

        // 2. 非 CPU 加速优先
        let accel: Vec<ComputeBackend> = backends
            .iter()
            .copied()
            .filter(|b| *b != ComputeBackend::Cpu)
            .collect();
        let assigned = if accel.is_empty() {
            // 纯 CPU 模块：直接走 CPU 后端分配（CPU 设备 total_memory_mb=None
            // → 不受显存闸门约束）
            self.assign(module_id, &[ComputeBackend::Cpu], 0)
        } else {
            self.assign(module_id, &accel, vram_mb)
        };
        if assigned.is_some() {
            return assigned;
        }

        // 3. CPU 保底（绕过调度器：CPU 恒可用，无需记账）
        backends
            .contains(&ComputeBackend::Cpu)
            .then_some(DeviceId::Cpu)
    }
}

impl DeviceScheduler for ComputeScheduler {
    fn assign(
        &self,
        module_id: &str,
        backends: &[ComputeBackend],
        vram_mb: u32,
    ) -> Option<DeviceId> {
        // P1：check-then-act 原子化——选择 + 显存闸门 + 记账包进同一把
        // `assignments` 写锁临界区。此前"先读锁查重、后分散记账"在并发下会
        // 双记账/超分：两个线程对同一模块都能通过查重，或不同模块同时读到
        // 同一剩余显存后都落账。加锁顺序恒为 assignments → allocated →
        // vram_per_module，与 status_report / release 的加锁顺序一致，无死锁。
        let mut assignments = self.assignments.write().unwrap();

        // 若该模块已有分配，直接返回
        if let Some(existing) = assignments.get(module_id) {
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

            // P1-6：Single 策略尊重 single_device 名称（如 "cuda:1"）——
            // 在兼容设备中按 DeviceId 字符串（id.to_string()）匹配，
            // 找不到（名称不存在或不在兼容集内）再回退 [0]
            SchedulingStrategy::Single => match self.single_device.as_deref() {
                Some(name) => compatible
                    .iter()
                    .copied()
                    .find(|&idx| self.devices[idx].id.to_string() == name)
                    .unwrap_or(compatible[0]),
                None => compatible[0],
            },
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

        // 记录分配（仍在临界区内：查重/闸门/记账同锁，杜绝双记账/超分）
        let device_id = self.devices[selected_index].id.clone();
        assignments.insert(module_id.to_string(), device_id.clone());
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

// ─── 无状态一次性选择与共享助手（D-4：daemon 三处消费） ─────────────────────

/// 无状态一次性选择的进程级 RoundRobin 游标（D-4）。
///
/// daemon 三处调用点每次请求构建临时调度器，实例游标恒从 0 开始会让
/// RoundRobin 退化为 Single；进程级游标保证跨请求轮转。桌面端使用常驻
/// 调度器（自有实例游标），不消费本游标。
static STATELESS_ROUND_ROBIN_CURSOR: AtomicUsize = AtomicUsize::new(0);

/// 无状态一次性设备选择 — daemon 三处启动路径的共享入口（D-4）。
///
/// 内部构建临时 [`ComputeScheduler`] 并走与桌面端完全相同的
/// [`ComputeScheduler::assign_module_device`] 路径，保证两端语义同源；
/// 临时实例的显存记账随实例丢弃——daemon 不做跨模块记账，闸门实时占用
/// 由检测器采样提供（见 `remaining_memory` 的 D-6 语义）。
///
/// - `strategy` / `allow_overcommit` / `disabled_backends` 取自 `[compute]` 配置；
/// - RoundRobin 策略由进程级游标驱动跨请求轮转；
/// - 返回 `None` = 无兼容设备（调用方决定错误/兜底语义）。
pub fn select_device_for_module(
    devices: &[ComputeDevice],
    manifest: &ModuleManifest,
    vram_mb: u32,
    strategy: SchedulingStrategy,
    allow_overcommit: bool,
    disabled_backends: &[ComputeBackend],
) -> Option<DeviceId> {
    // 兼容入口：不带 single_device（名称未接线时保持旧行为，Single 回退 [0]）。
    // 需让 `[compute].single_device` 参与落位请用
    // [`select_device_for_module_with_single_device`]。
    select_device_for_module_with_single_device(
        devices,
        manifest,
        vram_mb,
        strategy,
        None,
        allow_overcommit,
        disabled_backends,
    )
}

/// 同 [`select_device_for_module`]，额外接受 `single_device` 名称
/// （`[compute].single_device`，如 "cuda:1"）：Single 策略按 DeviceId 字符串
/// 匹配兼容设备，找不到再回退 [0]（P1-6）。
pub fn select_device_for_module_with_single_device(
    devices: &[ComputeDevice],
    manifest: &ModuleManifest,
    vram_mb: u32,
    strategy: SchedulingStrategy,
    single_device: Option<&str>,
    allow_overcommit: bool,
    disabled_backends: &[ComputeBackend],
) -> Option<DeviceId> {
    let mut scheduler = ComputeScheduler::new(devices.to_vec(), strategy);
    scheduler.set_allow_overcommit(allow_overcommit);
    scheduler.set_disabled_backends(disabled_backends.to_vec());
    scheduler.set_single_device(single_device.map(str::to_string));
    if strategy == SchedulingStrategy::RoundRobin {
        let start = STATELESS_ROUND_ROBIN_CURSOR.fetch_add(1, Ordering::SeqCst);
        scheduler.round_robin_index.store(start, Ordering::SeqCst);
    }
    scheduler.assign_module_device(&manifest.module.id, manifest, vram_mb)
}

/// 调度显存请求量（MB）：激活变体级估算优先、模块级兜底（§6.3 同源口径，
/// [`ModuleManifest::resolve_vram_estimate`] 在变体未命中时自动回退模块级），
/// 未知 → 0（不参与显存闸门）。daemon/桌面共享，语义同桌面原
/// `scheduler_vram_mb`。
pub fn module_vram_request(config: &AppConfig, manifest: &ModuleManifest) -> u32 {
    let variant = active_model_for(config, manifest).unwrap_or("");
    let mb = manifest.resolve_vram_estimate(variant).unwrap_or(0);
    u32::try_from(mb).unwrap_or(u32::MAX)
}

/// `[compute].strategy` → 调度器策略（daemon/桌面共享，语义同桌面原
/// `scheduling_strategy_for`）。Single 的具体设备名经 [`single_device_name`]
/// 单独提取，由 [`select_device_for_module_with_single_device`] /
/// [`ComputeScheduler::set_single_device`] 接线（P1-6）。
pub fn scheduling_strategy_for(config: &AppConfig) -> SchedulingStrategy {
    match config.compute.resolved_strategy() {
        AssignStrategy::Manual => SchedulingStrategy::Manual,
        AssignStrategy::LeastMemory => SchedulingStrategy::LeastMemory,
        AssignStrategy::RoundRobin => SchedulingStrategy::RoundRobin,
        AssignStrategy::Single(_) => SchedulingStrategy::Single,
    }
}

/// 提取 `[compute].single_device` 名称（如 "cuda:1"）供 Single 策略接线
/// （未配置返回 None）。P1-6：调用方在
/// [`select_device_for_module_with_single_device`] / `set_single_device`
/// 时传入，使配置的设备名真正参与落位。
pub fn single_device_name(config: &AppConfig) -> Option<String> {
    config.compute.single_device.clone()
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ComputeBackend, SchedulingStrategy};

    fn make_device(id: DeviceId, name: &str, total_mb: Option<u32>) -> ComputeDevice {
        make_device_with_used(id, name, total_mb, None)
    }

    fn make_device_with_used(
        id: DeviceId,
        name: &str,
        total_mb: Option<u32>,
        used_mb: Option<u32>,
    ) -> ComputeDevice {
        let backend = id.backend();
        ComputeDevice {
            id,
            backend,
            name: name.to_string(),
            total_memory_mb: total_mb,
            used_memory_mb: used_mb,
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

    // ── P1-6：Single 策略按 single_device 名称落位（回归） ─────────────────

    #[test]
    fn test_single_strategy_honors_named_device() {
        // single_device="cuda:1" 必须参与落位：即便 compatible[0] 是 cuda:0
        let devices = vec![
            cuda_device(0, "GPU-0", 4096),
            cuda_device(1, "GPU-1", 8192),
        ];
        let mut scheduler = ComputeScheduler::new(devices, SchedulingStrategy::Single);
        scheduler.set_single_device(Some("cuda:1".to_string()));

        let r1 = scheduler.assign("mod_a", &[ComputeBackend::Cuda], 100);
        assert_eq!(r1, Some(DeviceId::Cuda(1)), "Single 应按名称命中 cuda:1");
        let r2 = scheduler.assign("mod_b", &[ComputeBackend::Cuda], 100);
        assert_eq!(r2, Some(DeviceId::Cuda(1)));
    }

    #[test]
    fn test_single_strategy_named_device_not_found_falls_back() {
        // 名称在设备表中不存在 → 回退 [0]（保持旧行为）
        let devices = vec![
            cuda_device(0, "GPU-0", 4096),
            cuda_device(1, "GPU-1", 8192),
        ];
        let mut scheduler = ComputeScheduler::new(devices, SchedulingStrategy::Single);
        scheduler.set_single_device(Some("cuda:9".to_string()));
        let r = scheduler.assign("mod_a", &[ComputeBackend::Cuda], 100);
        assert_eq!(r, Some(DeviceId::Cuda(0)), "找不到命名设备应回退 compatible[0]");

        // 名称存在但不在请求后端的兼容集内（如单设 cpu 却只请求 CUDA）→ 同样回退
        let mut s2 = ComputeScheduler::new(
            vec![cuda_device(0, "GPU-0", 4096), cuda_device(1, "GPU-1", 8192)],
            SchedulingStrategy::Single,
        );
        s2.set_single_device(Some("cpu".to_string()));
        let r2 = s2.assign("mod_b", &[ComputeBackend::Cuda], 0);
        assert_eq!(r2, Some(DeviceId::Cuda(0)));
    }

    #[test]
    fn test_single_device_name_helper_reads_config() {
        let mut config = AppConfig::default();
        assert_eq!(single_device_name(&config), None, "默认未配置");

        config.compute.strategy = AssignStrategy::Single(None);
        config.compute.single_device = Some("cuda:1".to_string());
        assert_eq!(single_device_name(&config).as_deref(), Some("cuda:1"));
    }

    // ── P1：assign 临界区原子化回归（并发双记账/超分） ─────────────────────

    #[test]
    fn test_concurrent_assign_same_module_no_double_booking() {
        // 8 线程并发为同一模块分配：原子临界区后只记账一次
        let devices = vec![cuda_device(0, "GPU-0", 8192)];
        let scheduler = std::sync::Arc::new(ComputeScheduler::new(
            devices,
            SchedulingStrategy::LeastMemory,
        ));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let s = scheduler.clone();
                std::thread::spawn(move || s.assign("mod-same", &[ComputeBackend::Cuda], 1000))
            })
            .collect();
        for h in handles {
            assert!(h.join().unwrap().is_some());
        }

        let report = scheduler.status_report();
        assert_eq!(
            report[0].assigned_modules.len(),
            1,
            "并发分配同模块只能记账一次"
        );
        assert_eq!(
            report[0].allocated_memory_mb, 1000,
            "同模块并发分配不得双记账"
        );
    }

    #[test]
    fn test_concurrent_assign_no_oversubscription() {
        // 显存收紧：10 个模块各要 1000MB 争抢 4000MB 单卡，关闭超分 →
        // 原子临界区保证成功数 ≤ 4、账面分配不超容量（旧实现会同时通过闸门）
        let devices = vec![cuda_device(0, "GPU-0", 4000)];
        let scheduler = std::sync::Arc::new(ComputeScheduler::new(
            devices,
            SchedulingStrategy::LeastMemory,
        ));

        let handles: Vec<_> = (0..10)
            .map(|i| {
                let s = scheduler.clone();
                std::thread::spawn(move || {
                    s.assign(&format!("mod-{i}"), &[ComputeBackend::Cuda], 1000).is_some()
                })
            })
            .collect();
        let success = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|&ok| ok)
            .count();
        assert!(
            success <= 4,
            "4000MB 容量下 1000MB×10 并发分配不得超分，成功数 {success}"
        );

        let report = scheduler.status_report();
        assert!(
            report[0].allocated_memory_mb <= 4000,
            "账面分配不得超过容量: {}",
            report[0].allocated_memory_mb
        );
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

    // ── D-6：VRAM 闸门取 max(账面, 实时采样) ───────────────────────────────

    #[test]
    fn test_gate_uses_live_used_when_higher_than_booked() {
        // 账面分配为 0，但实时采样已用 7000（外部程序占用）→ 闸门按 7000 计
        let devices = vec![make_device_with_used(
            DeviceId::Cuda(0),
            "GPU",
            Some(8192),
            Some(7000),
        )];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);

        // 剩余 = 8192 − max(0, 7000) = 1192 < 2000 → 拒绝
        let r = scheduler.assign("mod_a", &[ComputeBackend::Cuda], 2000);
        assert_eq!(r, None);

        // 1192 以内 → 放行
        let r = scheduler.assign("mod_b", &[ComputeBackend::Cuda], 1000);
        assert_eq!(r, Some(DeviceId::Cuda(0)));
    }

    #[test]
    fn test_gate_booked_wins_when_higher_than_live() {
        // 账面分配 6000 > 实时采样 1000 → 闸门按账面 6000 计（取较大值）
        let devices = vec![make_device_with_used(
            DeviceId::Cuda(0),
            "GPU",
            Some(8192),
            Some(1000),
        )];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);

        let r1 = scheduler.assign("mod_a", &[ComputeBackend::Cuda], 6000);
        assert_eq!(r1, Some(DeviceId::Cuda(0)));

        // 剩余 = 8192 − max(6000, 1000) = 2192 < 3000 → 拒绝
        let r2 = scheduler.assign("mod_b", &[ComputeBackend::Cuda], 3000);
        assert_eq!(r2, None);
    }

    #[test]
    fn test_least_memory_ranking_considers_live_used() {
        // 同容量两块 GPU：cuda:0 实时已用 6000、cuda:1 已用 1000 → 选 cuda:1
        let devices = vec![
            make_device_with_used(DeviceId::Cuda(0), "GPU-0", Some(8192), Some(6000)),
            make_device_with_used(DeviceId::Cuda(1), "GPU-1", Some(8192), Some(1000)),
        ];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);

        let r = scheduler.assign("mod_a", &[ComputeBackend::Cuda], 100);
        assert_eq!(r, Some(DeviceId::Cuda(1)));
    }

    #[test]
    fn test_gate_used_none_is_ignored() {
        // used_memory_mb = None（无采样源）→ 忽略，仅按账面计（行为同修正前）
        let devices = vec![make_device(DeviceId::Cuda(0), "GPU", Some(4096))];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);

        let r = scheduler.assign("mod_a", &[ComputeBackend::Cuda], 4000);
        assert_eq!(r, Some(DeviceId::Cuda(0)));
        let r = scheduler.assign("mod_b", &[ComputeBackend::Cuda], 200);
        assert_eq!(r, None);
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

    // ── D-4：模块级设备选择共享实现（daemon/桌面同源） ────────────────────

    /// manifest 测试夹具：backends + 可选附加 `[compute]` 行（如 vram_estimate_mb / models）
    fn manifest_fixture(id: &str, backends: &[ComputeBackend], extra: &str) -> ModuleManifest {
        let backends_str = backends
            .iter()
            .map(|b| format!("\"{b}\""))
            .collect::<Vec<_>>()
            .join(", ");
        toml::from_str(&format!(
            r#"
[module]
id = "{id}"
name = "t"
version = "0.1.0"
description = "t"
category = "asr"
genre = "test"

[runtime]
type = "native"
binaries = {{ "x" = "x" }}

[compute]
backends = [{backends_str}]
{extra}

[interface]
type = "http"
"#
        ))
        .unwrap()
    }

    #[test]
    fn test_assign_module_device_accelerator_first_least_memory() {
        // cuda+cpu 声明：加速后端优先，LeastMemory 选剩余显存最大的 cuda:1
        let devices = vec![cuda_device(0, "GPU-Small", 4096), cuda_device(1, "GPU-Large", 8192), cpu_device()];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);
        let mf = manifest_fixture("mod-a", &[ComputeBackend::Cuda, ComputeBackend::Cpu], "");

        let r = scheduler.assign_module_device("mod-a", &mf, 100);
        assert_eq!(r, Some(DeviceId::Cuda(1)));
    }

    #[test]
    fn test_assign_module_device_pure_cpu_module() {
        // 纯 CPU 模块：走 CPU 分配路径；设备表即使没有 CPU 条目也返回 Cpu
        //（CPU 保底绕过设备表：CPU 恒可用）
        let devices = vec![cuda_device(0, "GPU", 8192)];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);
        let mf = manifest_fixture("mod-a", &[ComputeBackend::Cpu], "");

        let r = scheduler.assign_module_device("mod-a", &mf, 0);
        assert_eq!(r, Some(DeviceId::Cpu));
    }

    #[test]
    fn test_assign_module_device_disabled_backends_excluded() {
        // 禁用 cuda → 加速组为空，manifest 声明 cpu → CPU 保底
        let devices = vec![cuda_device(0, "GPU", 8192), cpu_device()];
        let mut scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);
        scheduler.set_disabled_backends(vec![ComputeBackend::Cuda]);
        let mf = manifest_fixture("mod-a", &[ComputeBackend::Cuda, ComputeBackend::Cpu], "");

        let r = scheduler.assign_module_device("mod-a", &mf, 100);
        assert_eq!(r, Some(DeviceId::Cpu));
    }

    #[test]
    fn test_assign_module_device_all_backends_disabled_returns_none() {
        // manifest 全部后端被禁用 → None（调用方决定兜底语义）
        let devices = vec![cuda_device(0, "GPU", 8192), cpu_device()];
        let mut scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);
        scheduler.set_disabled_backends(vec![ComputeBackend::Cuda, ComputeBackend::Cpu]);
        let mf = manifest_fixture("mod-a", &[ComputeBackend::Cuda, ComputeBackend::Cpu], "");

        let r = scheduler.assign_module_device("mod-a", &mf, 100);
        assert_eq!(r, None);
    }

    #[test]
    fn test_assign_module_device_vram_gate_reject_cpu_fallback() {
        // 显存闸门拒绝（超限且未开超分）：声明 cpu → CPU 保底；仅 cuda → None
        let devices = vec![cuda_device(0, "GPU", 4096), cpu_device()];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);

        let mf = manifest_fixture("mod-a", &[ComputeBackend::Cuda, ComputeBackend::Cpu], "");
        let r = scheduler.assign_module_device("mod-a", &mf, 9999);
        assert_eq!(r, Some(DeviceId::Cpu));

        let mf_gpu_only = manifest_fixture("mod-b", &[ComputeBackend::Cuda], "");
        let r = scheduler.assign_module_device("mod-b", &mf_gpu_only, 9999);
        assert_eq!(r, None);
    }

    #[test]
    fn test_assign_module_device_manual_strategy_cpu_fallback() {
        // Manual 策略下调度器拒绝自动分配 → manifest 声明 cpu 则 CPU 保底
        let devices = vec![cuda_device(0, "GPU", 8192), cpu_device()];
        let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::Manual);
        let mf = manifest_fixture("mod-a", &[ComputeBackend::Cuda, ComputeBackend::Cpu], "");

        let r = scheduler.assign_module_device("mod-a", &mf, 100);
        assert_eq!(r, Some(DeviceId::Cpu));
    }

    #[test]
    fn test_select_device_for_module_respects_config() {
        // 无状态入口消费 [compute] 配置：disabled_backends 过滤生效
        let devices = vec![cuda_device(0, "GPU", 8192), cpu_device()];
        let mf = manifest_fixture("mod-a", &[ComputeBackend::Cuda, ComputeBackend::Cpu], "");

        let r = select_device_for_module(
            &devices,
            &mf,
            100,
            SchedulingStrategy::LeastMemory,
            false,
            &[ComputeBackend::Cuda],
        );
        assert_eq!(r, Some(DeviceId::Cpu));

        // 不禁用 → 加速器优先
        let r = select_device_for_module(
            &devices,
            &mf,
            100,
            SchedulingStrategy::LeastMemory,
            false,
            &[],
        );
        assert_eq!(r, Some(DeviceId::Cuda(0)));
    }

    #[test]
    fn test_select_device_for_module_round_robin_rotates_across_requests() {
        // 每次调用构建临时调度器，进程级游标保证跨请求轮转不退化为 Single。
        // 本测试是 STATELESS_ROUND_ROBIN_CURSOR 的唯一消费者，断言确定性成立。
        let devices = vec![cuda_device(0, "GPU-0", 8192), cuda_device(1, "GPU-1", 8192)];
        let mf = manifest_fixture("mod-a", &[ComputeBackend::Cuda], "");

        let picks: Vec<DeviceId> = (0..4)
            .map(|_| {
                select_device_for_module(
                    &devices,
                    &mf,
                    0,
                    SchedulingStrategy::RoundRobin,
                    true,
                    &[],
                )
                .unwrap()
            })
            .collect();
        // 相邻两次必不同（游标 +1，候选数 2），且两块设备都被轮到
        for w in picks.windows(2) {
            assert_ne!(w[0], w[1]);
        }
        assert!(picks.contains(&DeviceId::Cuda(0)));
        assert!(picks.contains(&DeviceId::Cuda(1)));
    }

    #[test]
    fn test_module_vram_request_variant_priority_and_fallback() {
        // 变体级估算优先（config.active_models 命中）；未命中回退模块级；全缺 → 0
        let extra = "vram_estimate_mb = 512\n\n[[models]]\nid = \"m1\"\nname = \"M1\"\nsource = \"huggingface\"\nrepo_id = \"a/b\"\ntarget_dir = \"m1\"\nvram_estimate_mb = 2048\ndefault = true";
        let mf = manifest_fixture("mod-a", &[ComputeBackend::Cuda, ComputeBackend::Cpu], extra);

        let mut config = AppConfig::default();
        config.active_models.insert("mod-a".into(), "m1".into());
        assert_eq!(module_vram_request(&config, &mf), 2048);

        // 激活变体未配置 → 三级回退取 default 变体 m1，仍是变体级 2048
        let config_default = AppConfig::default();
        assert_eq!(module_vram_request(&config_default, &mf), 2048);

        // 变体级缺失 → 回退模块级 512（active_models 指向无估算的变体也同理）
        let mf_no_variant = manifest_fixture("mod-b", &[ComputeBackend::Cpu], "vram_estimate_mb = 512");
        assert_eq!(module_vram_request(&AppConfig::default(), &mf_no_variant), 512);

        // 全缺 → 0（不参与显存闸门）
        let mf_none = manifest_fixture("mod-c", &[ComputeBackend::Cpu], "");
        assert_eq!(module_vram_request(&AppConfig::default(), &mf_none), 0);
    }

    #[test]
    fn test_scheduling_strategy_for_config_mapping() {
        let mut config = AppConfig::default();

        config.compute.strategy = AssignStrategy::Manual;
        assert_eq!(scheduling_strategy_for(&config), SchedulingStrategy::Manual);

        config.compute.strategy = AssignStrategy::LeastMemory;
        assert_eq!(scheduling_strategy_for(&config), SchedulingStrategy::LeastMemory);

        config.compute.strategy = AssignStrategy::RoundRobin;
        assert_eq!(scheduling_strategy_for(&config), SchedulingStrategy::RoundRobin);

        config.compute.strategy = AssignStrategy::Single(None);
        assert_eq!(scheduling_strategy_for(&config), SchedulingStrategy::Single);
    }
}
