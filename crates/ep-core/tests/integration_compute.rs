//! 集成测试：设备检测 → 调度分配 → 释放 全流程
//!
//! Wave 4 / Agent F

use ep_core::compute::scheduler::ComputeScheduler;
use ep_core::compute::detect_all_devices;
use ep_core::types::{ComputeBackend, ComputeDevice, DeviceId, DeviceScheduler, SchedulingStrategy};

// ─── Helpers ────────────────────────────────────────────────────────────────

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

// ─── Test 1: Detect real devices (at least CPU) ─────────────────────────────

#[test]
fn test_detect_devices() {
    let devices = detect_all_devices(&[]);

    // At minimum, CPU should always be detected
    assert!(
        !devices.is_empty(),
        "should detect at least one device (CPU)"
    );

    let has_cpu = devices.iter().any(|d| d.backend == ComputeBackend::Cpu);
    assert!(has_cpu, "CPU device should always be detected");

    // Verify CPU device properties
    let cpu = devices.iter().find(|d| d.backend == ComputeBackend::Cpu).unwrap();
    assert_eq!(cpu.id, DeviceId::Cpu);
    // CPU total_memory_mb may be Some (detected via OS APIs) or None
    // Just verify it doesn't panic

    // Test with disabled backends
    let devices_no_cuda = detect_all_devices(&[ComputeBackend::Cuda]);
    let has_cuda = devices_no_cuda
        .iter()
        .any(|d| d.backend == ComputeBackend::Cuda);
    assert!(
        !has_cuda,
        "CUDA should be disabled when in disabled list"
    );

    // CPU should still be present
    let has_cpu = devices_no_cuda
        .iter()
        .any(|d| d.backend == ComputeBackend::Cpu);
    assert!(has_cpu, "CPU should still be detected when CUDA is disabled");
}

// ─── Test 2: Scheduler assign → release → reassign ──────────────────────────

#[test]
fn test_scheduler_assign_release() {
    let devices = vec![
        cuda_device(0, "GPU-0", 8192),
        cuda_device(1, "GPU-1", 4096),
        cpu_device(),
    ];
    let mut scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);

    // Assign module A — should go to GPU-0 (most memory with LeastMemory strategy)
    let assigned = scheduler.assign("mod-a", &[ComputeBackend::Cuda], 2048);
    assert_eq!(assigned, Some(DeviceId::Cuda(0)));

    // Assign module B — should also go to GPU-0 (still has 6144 MB remaining > GPU-1's 4096)
    let assigned = scheduler.assign("mod-b", &[ComputeBackend::Cuda], 1024);
    assert_eq!(assigned, Some(DeviceId::Cuda(0)));

    // Assign module C — GPU-0 has 5120 remaining, GPU-1 has 4096 → GPU-0
    let assigned = scheduler.assign("mod-c", &[ComputeBackend::Cuda], 1024);
    assert_eq!(assigned, Some(DeviceId::Cuda(0)));

    // Release mod-a (frees 2048 on GPU-0, but GPU-0 still has 3072 allocated from mod-b+mod-c)
    scheduler.release("mod-a");

    // Now GPU-0 has 8192 - 2048 = 6144 remaining
    // Reassign mod-d — should still go to GPU-0 (6144 > 4096)
    let assigned = scheduler.assign("mod-d", &[ComputeBackend::Cuda], 1024);
    assert_eq!(assigned, Some(DeviceId::Cuda(0)));

    // Release all and verify we can reassign
    scheduler.release("mod-b");
    scheduler.release("mod-c");
    scheduler.release("mod-d");

    // After full release, GPU-0 should have full 8192 available again
    let assigned = scheduler.assign("mod-e", &[ComputeBackend::Cuda], 4096);
    assert_eq!(assigned, Some(DeviceId::Cuda(0)));

    // Verify status report
    let report = scheduler.status_report();
    assert_eq!(report.len(), 3);
    let gpu0_report = report.iter().find(|r| r.device_id == DeviceId::Cuda(0)).unwrap();
    assert_eq!(gpu0_report.allocated_memory_mb, 4096);
    assert_eq!(gpu0_report.assigned_modules, vec!["mod-e".to_string()]);
}

// ─── Test 3: Compare different scheduling strategies ────────────────────────

#[test]
fn test_scheduler_strategies() {
    let devices = vec![
        cuda_device(0, "GPU-Small", 4096),
        cuda_device(1, "GPU-Large", 8192),
        cuda_device(2, "GPU-Medium", 6144),
    ];

    // LeastMemory: should pick the device with most remaining memory
    {
        let scheduler = ComputeScheduler::new(devices.clone(), SchedulingStrategy::LeastMemory);
        let result = scheduler.assign("mod-a", &[ComputeBackend::Cuda], 1024);
        assert_eq!(
            result,
            Some(DeviceId::Cuda(1)),
            "LeastMemory should pick GPU-Large (8192)"
        );
    }

    // RoundRobin: should cycle through devices
    {
        let scheduler = ComputeScheduler::new(devices.clone(), SchedulingStrategy::RoundRobin);
        let r1 = scheduler.assign("mod-a", &[ComputeBackend::Cuda], 100);
        let r2 = scheduler.assign("mod-b", &[ComputeBackend::Cuda], 100);
        let r3 = scheduler.assign("mod-c", &[ComputeBackend::Cuda], 100);
        let r4 = scheduler.assign("mod-d", &[ComputeBackend::Cuda], 100);

        assert_eq!(r1, Some(DeviceId::Cuda(0)));
        assert_eq!(r2, Some(DeviceId::Cuda(1)));
        assert_eq!(r3, Some(DeviceId::Cuda(2)));
        assert_eq!(r4, Some(DeviceId::Cuda(0)), "should wrap around");
    }

    // Single: should always pick the first compatible device
    {
        let scheduler = ComputeScheduler::new(devices.clone(), SchedulingStrategy::Single);
        let r1 = scheduler.assign("mod-a", &[ComputeBackend::Cuda], 100);
        let r2 = scheduler.assign("mod-b", &[ComputeBackend::Cuda], 100);
        let r3 = scheduler.assign("mod-c", &[ComputeBackend::Cuda], 100);

        assert_eq!(r1, Some(DeviceId::Cuda(0)));
        assert_eq!(r2, Some(DeviceId::Cuda(0)));
        assert_eq!(r3, Some(DeviceId::Cuda(0)));
    }

    // Manual: should always return None
    {
        let scheduler = ComputeScheduler::new(devices.clone(), SchedulingStrategy::Manual);
        let result = scheduler.assign("mod-a", &[ComputeBackend::Cuda], 100);
        assert_eq!(result, None, "Manual strategy should return None");
    }
}

// ─── Test 4: Scheduler compatibility — only assign compatible devices ───────

#[test]
fn test_scheduler_compatibility() {
    let devices = vec![
        cuda_device(0, "NVIDIA RTX", 8192),
        make_device(DeviceId::Rocm(0), "AMD GPU", Some(16384)),
        cpu_device(),
    ];
    let scheduler = ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory);

    // Request only CUDA — should pick NVIDIA
    let result = scheduler.assign("cuda-mod", &[ComputeBackend::Cuda], 1024);
    assert_eq!(result, Some(DeviceId::Cuda(0)));

    // Request only ROCm — should pick AMD
    let result = scheduler.assign("rocm-mod", &[ComputeBackend::Rocm], 1024);
    assert_eq!(result, Some(DeviceId::Rocm(0)));

    // Request only CPU — should pick CPU
    let result = scheduler.assign("cpu-mod", &[ComputeBackend::Cpu], 0);
    assert_eq!(result, Some(DeviceId::Cpu));

    // Request OpenVINO — no compatible device → None
    let result = scheduler.assign("ov-mod", &[ComputeBackend::OpenVINO], 100);
    assert_eq!(result, None, "no OpenVINO device available");

    // Request CUDA or CPU — LeastMemory picks the known-capacity GPU
    // (D-3: CPU total_memory=None means "unknown capacity" and ranks last,
    // it no longer masquerades as unlimited memory)
    let result = scheduler.assign("flex-mod", &[ComputeBackend::Cuda, ComputeBackend::Cpu], 100);
    assert_eq!(
        result,
        Some(DeviceId::Cuda(0)),
        "known-capacity GPU should be preferred over unknown-capacity CPU"
    );

    // VRAM overcommit blocked by default
    let scheduler2 = {
        let devices = vec![cuda_device(0, "Small GPU", 2048)];
        ComputeScheduler::new(devices, SchedulingStrategy::LeastMemory)
    };
    let result = scheduler2.assign("big-mod", &[ComputeBackend::Cuda], 4096);
    assert_eq!(result, None, "should reject when VRAM exceeds device capacity");
}
