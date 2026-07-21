use crate::types::{ComputeBackend, ComputeDevice, DeviceId};

use super::DeviceDetector;

pub struct CpuDetector;

#[cfg(windows)]
fn total_memory_mb() -> Option<u32> {
    use std::mem;

    #[repr(C)]
    struct MemoryStatusEx {
        dw_length: u32,
        dw_memory_load: u32,
        ull_total_phys: u64,
        ull_avail_phys: u64,
        ull_total_page_file: u64,
        ull_avail_page_file: u64,
        ull_total_virtual: u64,
        ull_avail_virtual: u64,
        ull_avail_extended_virtual: u64,
    }

    extern "system" {
        fn GlobalMemoryStatusEx(lp_buffer: *mut MemoryStatusEx) -> i32;
    }

    unsafe {
        let mut status: MemoryStatusEx = mem::zeroed();
        status.dw_length = mem::size_of::<MemoryStatusEx>() as u32;
        if GlobalMemoryStatusEx(&mut status) != 0 {
            Some((status.ull_total_phys / (1024 * 1024)) as u32)
        } else {
            None
        }
    }
}

#[cfg(unix)]
fn total_memory_mb() -> Option<u32> {
    let content = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
            return Some((kb / 1024) as u32);
        }
    }
    None
}

#[cfg(not(any(windows, unix)))]
fn total_memory_mb() -> Option<u32> {
    None
}

fn cpu_name() -> String {
    std::env::var("EP_CPU_NAME").unwrap_or_else(|_| "CPU".to_string())
}

impl DeviceDetector for CpuDetector {
    fn backend(&self) -> ComputeBackend {
        ComputeBackend::Cpu
    }

    fn detect(&self) -> Vec<ComputeDevice> {
        vec![ComputeDevice {
            id: DeviceId::Cpu,
            backend: ComputeBackend::Cpu,
            name: cpu_name(),
            total_memory_mb: total_memory_mb(),
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        }]
    }

    fn refresh(&self, _devices: &mut [ComputeDevice]) {}
}
