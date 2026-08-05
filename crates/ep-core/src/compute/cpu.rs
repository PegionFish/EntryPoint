//! CPU 设备检测与刷新（P2-14 补实现）
//!
//! 刷新策略（利用率均为"两次采样差分"语义；跨刷新调用的上次采样存于静态 `Mutex`——
//! 检测器每轮重建，结构体内状态无法跨调用保留）：
//! - **Linux**：`/proc/stat` jiffies 差分；`/proc/meminfo` 取 `MemTotal - MemAvailable`
//! - **Windows**：利用率首选 `GetSystemTimes` FFI 差分（与内存的 `GlobalMemoryStatusEx`
//!   同款纯 FFI，无子进程开销，2s 刷新周期下零延迟）；FFI 异常时 best-effort 单次查询
//!   （wmic `LoadPercentage` → PowerShell 兜底），失败容忍返回 `None`，绝不阻塞。
//!   内存用 `GlobalMemoryStatusEx`。
//!
//! 绝不 panic：所有来源失败均保持字段原值。

use std::sync::Mutex;
use std::time::Duration;

use crate::types::{ComputeBackend, ComputeDevice, DeviceId};

use super::DeviceDetector;
#[cfg(windows)]
use super::run_tool;

pub struct CpuDetector;

/// 聚合 CPU 时间片（Linux: `/proc/stat` jiffies；Windows: `GetSystemTimes` 100ns 计数）
#[derive(Clone, Copy, Debug)]
pub(crate) struct CpuTicks {
    total: u64,
    idle: u64,
}

/// 跨刷新调用的上次 CPU 时间采样缓存
static LAST_TICKS: Mutex<Option<CpuTicks>> = Mutex::new(None);

fn take_last_ticks() -> Option<CpuTicks> {
    LAST_TICKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
}

fn store_last_ticks(ticks: CpuTicks) {
    *LAST_TICKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(ticks);
}

/// 解析 `/proc/stat` 首行聚合 `cpu` 行。
/// 字段序：user nice system idle iowait irq softirq steal guest guest_nice
/// （guest 已计入 user，求和时排除避免重复计数）
#[cfg(any(unix, test))]
pub(crate) fn parse_proc_stat(content: &str) -> Option<CpuTicks> {
    for line in content.lines() {
        let Some(rest) = line.strip_prefix("cpu ") else {
            continue; // 注意 "cpu0" 等 per-core 行不以 "cpu " 开头
        };
        let fields: Vec<u64> = rest
            .split_whitespace()
            .take(8)
            .filter_map(|f| f.parse::<u64>().ok())
            .collect();
        if fields.len() < 4 {
            return None;
        }
        let total: u64 = fields.iter().sum();
        let idle = fields[3] + fields.get(4).copied().unwrap_or(0); // idle + iowait
        return Some(CpuTicks { total, idle });
    }
    None
}

/// 两次采样差分 → 利用率百分比（四舍五入，夹取 0-100）。
/// 总时间片无增长（采样间隔过短/计数器异常）→ None
pub(crate) fn cpu_utilization_pct(prev: CpuTicks, curr: CpuTicks) -> Option<u8> {
    let delta_total = curr.total.saturating_sub(prev.total);
    if delta_total == 0 {
        return None;
    }
    let delta_idle = curr.idle.saturating_sub(prev.idle);
    let busy = delta_total.saturating_sub(delta_idle);
    let pct = (busy * 100 + delta_total / 2) / delta_total;
    Some(pct.min(100) as u8)
}

/// `/proc/meminfo` 解析结果（kB）
#[cfg(any(unix, test))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct MemInfoKb {
    total_kb: u64,
    available_kb: u64,
}

/// 解析 `/proc/meminfo`：`MemTotal` 必须；`MemAvailable`（3.14+）缺失时
/// 回退 `MemFree + Buffers + Cached`
#[cfg(any(unix, test))]
pub(crate) fn parse_proc_meminfo(content: &str) -> Option<MemInfoKb> {
    let mut total_kb: Option<u64> = None;
    let mut available_kb: Option<u64> = None;
    let mut free_kb: u64 = 0;
    let mut buffers_kb: u64 = 0;
    let mut cached_kb: u64 = 0;

    for line in content.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let value_kb = rest
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse::<u64>()
            .ok();
        match key.trim() {
            "MemTotal" => total_kb = value_kb,
            "MemAvailable" => available_kb = value_kb,
            "MemFree" => free_kb = value_kb.unwrap_or(0),
            "Buffers" => buffers_kb = value_kb.unwrap_or(0),
            "Cached" => cached_kb = value_kb.unwrap_or(0),
            _ => {}
        }
    }

    let total_kb = total_kb?;
    let available_kb = available_kb.unwrap_or(free_kb + buffers_kb + cached_kb);
    Some(MemInfoKb {
        total_kb,
        available_kb: available_kb.min(total_kb),
    })
}

#[cfg(unix)]
fn sample_ticks() -> Option<CpuTicks> {
    let content = std::fs::read_to_string("/proc/stat").ok()?;
    parse_proc_stat(&content)
}

/// Linux 利用率：与上次采样差分；首采样无历史时短延时 150ms 双采样产出即时值
#[cfg(unix)]
fn next_utilization() -> Option<u8> {
    let first = sample_ticks()?;
    let (base, current) = match take_last_ticks() {
        Some(prev) => (prev, first),
        None => {
            std::thread::sleep(Duration::from_millis(150));
            (first, sample_ticks()?)
        }
    };
    store_last_ticks(current);
    cpu_utilization_pct(base, current)
}

/// Windows 采样：`GetSystemTimes`（kernel 时间已含 idle；total = kernel + user）
#[cfg(windows)]
fn sample_system_times() -> Option<CpuTicks> {
    extern "system" {
        fn GetSystemTimes(idle_time: *mut u64, kernel_time: *mut u64, user_time: *mut u64)
        -> i32;
    }
    unsafe {
        let mut idle: u64 = 0;
        let mut kernel: u64 = 0;
        let mut user: u64 = 0;
        if GetSystemTimes(&mut idle, &mut kernel, &mut user) == 0 {
            return None;
        }
        Some(CpuTicks {
            total: kernel + user,
            idle,
        })
    }
}

/// Windows CPU 利用率：首选 GetSystemTimes 差分（无子进程）；FFI 异常时退化为
/// best-effort 单次查询（wmic LoadPercentage → PowerShell 兜底），失败容忍 None
#[cfg(windows)]
fn next_utilization() -> Option<u8> {
    if let Some(first) = sample_system_times() {
        let (base, current) = match take_last_ticks() {
            Some(prev) => (prev, first),
            None => {
                std::thread::sleep(Duration::from_millis(150));
                (first, sample_system_times()?)
            }
        };
        store_last_ticks(current);
        if let Some(pct) = cpu_utilization_pct(base, current) {
            return Some(pct);
        }
    }
    windows_load_fallback()
}

#[cfg(not(any(windows, unix)))]
fn next_utilization() -> Option<u8> {
    None
}

#[cfg(windows)]
fn windows_load_fallback() -> Option<u8> {
    if let Some(output) = run_tool(
        "wmic",
        &["cpu", "get", "loadpercentage", "/format:list"],
        Duration::from_secs(8),
    ) {
        if let Some(pct) = parse_wmic_load_percentage(&output) {
            return Some(pct);
        }
    }
    // Win11 24H2 起 wmic 可能被移除 → PowerShell 兜底（冷启动较慢，超时放宽）
    let output = run_tool(
        "powershell",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-CimInstance Win32_Processor | Measure-Object -Property LoadPercentage -Average | Select-Object -ExpandProperty Average",
        ],
        Duration::from_secs(10),
    )?;
    parse_percent_number(&output)
}

/// 解析 `wmic cpu get loadpercentage /format:list`（多插槽取均值）
#[cfg(any(windows, test))]
pub(crate) fn parse_wmic_load_percentage(output: &str) -> Option<u8> {
    let mut values: Vec<u32> = Vec::new();
    for line in output.lines() {
        let Some(value) = line.trim().strip_prefix("LoadPercentage=") else {
            continue;
        };
        if let Ok(v) = value.trim().parse::<u32>() {
            values.push(v);
        }
    }
    if values.is_empty() {
        return None;
    }
    let avg = (values.iter().sum::<u32>() + values.len() as u32 / 2) / values.len() as u32;
    Some(avg.min(100) as u8)
}

/// 解析 PowerShell 单值输出（如 "12.5"，容忍 \r 与空白）
#[cfg(any(windows, test))]
pub(crate) fn parse_percent_number(output: &str) -> Option<u8> {
    let value: f64 = output.trim().parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    Some(value.round().clamp(0.0, 100.0) as u8)
}

// ─── 内存 ────────────────────────────────────────────────────────────────────

/// (总内存, 已用内存) MB
#[cfg(windows)]
fn memory_status_mb() -> Option<(u32, u32)> {
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
            let total = status.ull_total_phys / (1024 * 1024);
            let used = status
                .ull_total_phys
                .saturating_sub(status.ull_avail_phys)
                / (1024 * 1024);
            Some((total as u32, used as u32))
        } else {
            None
        }
    }
}

#[cfg(unix)]
fn memory_status_mb() -> Option<(u32, u32)> {
    let content = std::fs::read_to_string("/proc/meminfo").ok()?;
    let info = parse_proc_meminfo(&content)?;
    let total_mb = info.total_kb / 1024;
    let used_mb = info.total_kb.saturating_sub(info.available_kb) / 1024;
    Some((total_mb as u32, used_mb as u32))
}

#[cfg(not(any(windows, unix)))]
fn memory_status_mb() -> Option<(u32, u32)> {
    None
}

// ─── CPU 名称 ────────────────────────────────────────────────────────────────

fn cpu_name() -> String {
    if let Ok(name) = std::env::var("EP_CPU_NAME") {
        if !name.trim().is_empty() {
            return name;
        }
    }
    platform_cpu_name().unwrap_or_else(|| "CPU".to_string())
}

#[cfg(unix)]
fn platform_cpu_name() -> Option<String> {
    let content = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    parse_cpuinfo_name(&content)
}

/// 解析 `/proc/cpuinfo` 的 CPU 名称：
/// - 优先 `model name`（x86/主流发行版）
/// - ARM（树莓派等）/部分容器无 `model name` → 回退 `model`/`Hardware`/`Processor` 行
#[cfg(any(unix, test))]
pub(crate) fn parse_cpuinfo_name(content: &str) -> Option<String> {
    let mut fallback: Option<String> = None;
    for line in content.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = rest.trim();
        if key == "model name" && !value.is_empty() {
            return Some(value.to_string());
        }
        if fallback.is_none()
            && !value.is_empty()
            && (key == "model" || key == "Hardware" || key == "Processor")
        {
            fallback = Some(value.to_string());
        }
    }
    fallback
}

#[cfg(windows)]
fn platform_cpu_name() -> Option<String> {
    // 首选注册表 ProcessorNameString（营销型号，如 "Intel(R) Core(TM) Ultra 7 270HX"）。
    // 纯 FFI 免子进程——避免 wmic/powershell 子进程在 daemon 无控制台环境下挂死
    // （真机实测：Win11 24H2 无 wmic，PowerShell Get-CimInstance 在子进程环境可卡启动）。
    if let Some(name) = registry_cpu_name() {
        return Some(name);
    }
    // 兜底：进程环境自带 PROCESSOR_IDENTIFIER（标识符格式，非营销型号）
    let name = std::env::var("PROCESSOR_IDENTIFIER").ok()?;
    let name = name.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// 读注册表 `HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0\ProcessorNameString`
/// （免子进程；缺失/失败返回 None）。Windows 专用 FFI。
#[cfg(windows)]
fn registry_cpu_name() -> Option<String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    /// 注册表句柄（预定义根键或已打开键）
    type Hkey = *mut core::ffi::c_void;
    type Lstatus = i32;

    // 预定义根键（Windows SDK winreg.h）：HKEY_LOCAL_MACHINE = (HKEY)0x80000002
    const HKEY_LOCAL_MACHINE: Hkey = 0x80000002usize as Hkey;
    const KEY_READ: u32 = 0x0002_0019;
    const REG_SZ: u32 = 1;
    const ERROR_SUCCESS: Lstatus = 0;

    #[link(name = "advapi32")]
    extern "system" {
        fn RegOpenKeyExW(
            h_key: Hkey,
            lp_sub_key: *const u16,
            ul_options: u32,
            sam_desired: u32,
            phk_result: *mut Hkey,
        ) -> Lstatus;
        fn RegQueryValueExW(
            h_key: Hkey,
            lp_value_name: *const u16,
            lp_reserved: *mut u32,
            lp_type: *mut u32,
            lp_data: *mut u8,
            lpcb_data: *mut u32,
        ) -> Lstatus;
        fn RegCloseKey(h_key: Hkey) -> Lstatus;
    }

    const SUB_KEY: &str =
        r"HARDWARE\DESCRIPTION\System\CentralProcessor\0";
    const VALUE: &str = "ProcessorNameString";

    let sub_key: Vec<u16> = OsString::from(SUB_KEY)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let value_name: Vec<u16> = OsString::from(VALUE)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let mut key: Hkey = ptr::null_mut();
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            sub_key.as_ptr(),
            0,
            KEY_READ,
            &mut key,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let mut buf = [0u16; 256];
        let mut buf_bytes = (buf.len() * 2) as u32;
        let mut kind: u32 = 0;
        let status = RegQueryValueExW(
            key,
            value_name.as_ptr(),
            ptr::null_mut(),
            &mut kind,
            buf.as_mut_ptr() as *mut u8,
            &mut buf_bytes,
        );
        let _ = RegCloseKey(key);
        if status != ERROR_SUCCESS || kind != REG_SZ || buf_bytes < 2 {
            return None;
        }
        let len = (buf_bytes as usize) / 2 - 1; // 去结尾 NUL
        let name = String::from_utf16_lossy(&buf[..len.min(buf.len())]);
        let name = name.trim().to_string();
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    }
}

#[cfg(not(any(windows, unix)))]
fn platform_cpu_name() -> Option<String> {
    None
}

// ─── trait 实现 ─────────────────────────────────────────────────────────────

impl DeviceDetector for CpuDetector {
    fn backend(&self) -> ComputeBackend {
        ComputeBackend::Cpu
    }

    fn detect(&self) -> Vec<ComputeDevice> {
        let (total_memory_mb, used_memory_mb) = match memory_status_mb() {
            Some((total, used)) => (Some(total), Some(used)),
            None => (None, None),
        };
        vec![ComputeDevice {
            id: DeviceId::Cpu,
            backend: ComputeBackend::Cpu,
            name: cpu_name(),
            total_memory_mb,
            used_memory_mb,
            utilization: next_utilization(),
            temperature: None,
        }]
    }

    fn refresh(&self, devices: &mut [ComputeDevice]) {
        for dev in devices.iter_mut() {
            if dev.backend != ComputeBackend::Cpu {
                continue;
            }
            if let Some((total, used)) = memory_status_mb() {
                dev.total_memory_mb = Some(total);
                dev.used_memory_mb = Some(used);
            }
            // 利用率：失败容忍，保持原值（Windows 侧绝不阻塞）
            if let Some(pct) = next_utilization() {
                dev.utilization = Some(pct);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实 /proc/stat 样本（聚合行 + per-core 行 + 其他段）
    const PROC_STAT_FIXTURE: &str = "\
cpu  12345678 90123 4567890 89012345 567890 123 45678 0 0 0
cpu0 6172839 45061 2283945 44506172 283945 61 22839 0 0 0
cpu1 6172839 45062 2283945 44506173 283945 62 22839 0 0 0
intr 12345678 0 0 0
ctxt 98765432
btime 1754000000
";

    #[test]
    fn test_parse_proc_stat_aggregate_line() {
        let ticks = parse_proc_stat(PROC_STAT_FIXTURE).unwrap();
        // total = 前 8 字段之和（不含 guest/guest_nice）
        let expected_total: u64 = 12345678 + 90123 + 4567890 + 89012345 + 567890 + 123 + 45678;
        assert_eq!(ticks.total, expected_total);
        // idle = idle + iowait
        assert_eq!(ticks.idle, 89012345 + 567890);
    }

    #[test]
    fn test_parse_proc_stat_requires_aggregate() {
        assert!(parse_proc_stat("cpu0 1 2 3 4\n").is_none());
        assert!(parse_proc_stat("").is_none());
        assert!(parse_proc_stat("garbage\n").is_none());
        // 字段不足
        assert!(parse_proc_stat("cpu 1 2 3\n").is_none());
    }

    #[test]
    fn test_utilization_computation() {
        // total +1000，idle +750 → busy 250 → 25%
        let prev = CpuTicks {
            total: 10_000,
            idle: 8_000,
        };
        let curr = CpuTicks {
            total: 11_000,
            idle: 8_750,
        };
        assert_eq!(cpu_utilization_pct(prev, curr), Some(25));

        // 全忙
        let busy_curr = CpuTicks {
            total: 11_000,
            idle: 8_000,
        };
        assert_eq!(cpu_utilization_pct(prev, busy_curr), Some(100));

        // 全闲
        let idle_curr = CpuTicks {
            total: 11_000,
            idle: 9_000,
        };
        assert_eq!(cpu_utilization_pct(prev, idle_curr), Some(0));

        // total 无增长 → None
        assert_eq!(cpu_utilization_pct(prev, prev), None);

        // 计数器回绕等异常（idle 增量 > total 增量）→ 0 而非 panic
        let weird = CpuTicks {
            total: 11_000,
            idle: 20_000,
        };
        assert_eq!(cpu_utilization_pct(prev, weird), Some(0));
    }

    #[test]
    fn test_utilization_rounding() {
        let prev = CpuTicks { total: 0, idle: 0 };
        let curr = CpuTicks { total: 3, idle: 1 };
        // busy=2/3 = 66.67% → 四舍五入 67
        assert_eq!(cpu_utilization_pct(prev, curr), Some(67));
    }

    /// 真实 /proc/meminfo 样本
    const MEMINFO_FIXTURE: &str = "\
MemTotal:       32596236 kB
MemFree:         1234567 kB
MemAvailable:   18234567 kB
Buffers:          234567 kB
Cached:          5678901 kB
SwapTotal:       8388604 kB
SwapFree:        8388604 kB
";

    #[test]
    fn test_parse_proc_meminfo_with_memavailable() {
        let info = parse_proc_meminfo(MEMINFO_FIXTURE).unwrap();
        assert_eq!(info.total_kb, 32596236);
        assert_eq!(info.available_kb, 18234567);
    }

    #[test]
    fn test_parse_proc_meminfo_fallback_without_memavailable() {
        let content = "MemTotal: 8000000 kB\nMemFree: 1000000 kB\nBuffers: 500000 kB\nCached: 2500000 kB\n";
        let info = parse_proc_meminfo(content).unwrap();
        assert_eq!(info.total_kb, 8_000_000);
        assert_eq!(info.available_kb, 4_000_000); // free + buffers + cached
    }

    #[test]
    fn test_parse_proc_meminfo_invalid() {
        assert!(parse_proc_meminfo("").is_none());
        assert!(parse_proc_meminfo("MemFree: 123 kB\n").is_none()); // 缺 MemTotal
    }

    #[test]
    fn test_parse_wmic_load_percentage() {
        let output = "\r\n\r\nLoadPercentage=13\r\n\r\n";
        assert_eq!(parse_wmic_load_percentage(output), Some(13));

        // 多插槽均值（四舍五入）
        let multi = "LoadPercentage=10\nLoadPercentage=15\n";
        assert_eq!(parse_wmic_load_percentage(multi), Some(13)); // (25+1)/2=13

        // 夹取 0-100
        assert_eq!(parse_wmic_load_percentage("LoadPercentage=150\n"), Some(100));

        assert_eq!(parse_wmic_load_percentage("garbage"), None);
        assert_eq!(parse_wmic_load_percentage("LoadPercentage=abc"), None);
    }

    #[test]
    fn test_parse_percent_number() {
        assert_eq!(parse_percent_number("12.5"), Some(13));
        assert_eq!(parse_percent_number("  7  \r\n"), Some(7));
        assert_eq!(parse_percent_number("0"), Some(0));
        assert_eq!(parse_percent_number("100.4"), Some(100));
        assert_eq!(parse_percent_number(""), None);
        assert_eq!(parse_percent_number("not a number"), None);
    }

    #[test]
    fn test_cpu_name_env_override() {
        std::env::set_var("EP_CPU_NAME", "Test CPU X");
        assert_eq!(cpu_name(), "Test CPU X");
        std::env::remove_var("EP_CPU_NAME");
        // 回退到平台来源或 "CPU"，不得为空
        assert!(!cpu_name().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn test_registry_cpu_name_is_real_marketing_name() {
        // 真机冒烟：注册表 ProcessorNameString 应是非空营销型号（如
        // "Intel(R) Core(TM) ..."），且不得是 PROCESSOR_IDENTIFIER 标识符格式
        let name = registry_cpu_name().expect("Windows 应有 ProcessorNameString");
        assert!(!name.trim().is_empty());
        // 标识符格式特征（"Family/Model/Stepping/GenuineIntel"）不应出现
        assert!(!name.contains("Family"), "注册表应返回营销型号而非标识符: {name}");
        assert!(!name.contains("GenuineIntel"), "注册表应返回营销型号而非标识符: {name}");
    }

    #[cfg(any(unix, test))]
    #[test]
    fn test_parse_cpuinfo_name_prefers_model_name() {
        let content = "\
processor\t: 0
model name\t: Intel(R) Core(TM) Ultra 7 270HX Plus
vendor_id\t: GenuineIntel
";
        assert_eq!(
            parse_cpuinfo_name(content).as_deref(),
            Some("Intel(R) Core(TM) Ultra 7 270HX Plus")
        );
    }

    #[cfg(any(unix, test))]
    #[test]
    fn test_parse_cpuinfo_name_arm_fallback() {
        // ARM（树莓派等）无 model name 行 → 回退 model/Hardware/Processor
        let content = "\
processor\t: 0
model\t: Raspberry Pi 5 Model B Rev 1.0
Hardware\t: BCM2712
";
        assert_eq!(
            parse_cpuinfo_name(content).as_deref(),
            Some("Raspberry Pi 5 Model B Rev 1.0")
        );
        // 仅有 Hardware
        let hw_only = "Hardware : BCM2712\n";
        assert_eq!(parse_cpuinfo_name(hw_only).as_deref(), Some("BCM2712"));
    }

    #[cfg(any(unix, test))]
    #[test]
    fn test_parse_cpuinfo_name_missing_returns_none() {
        assert_eq!(parse_cpuinfo_name(""), None);
        assert_eq!(parse_cpuinfo_name("vendor_id : GenuineIntel\n"), None);
        // 空值行不得命中
        assert_eq!(parse_cpuinfo_name("model : \n"), None);
    }
}
