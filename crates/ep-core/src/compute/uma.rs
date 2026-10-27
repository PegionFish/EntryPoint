//! UMA（统一内存架构）识别 — T-22（GB10/Windows 统一内存场景）
//!
//! GB10 一类平台 CPU/GPU 共享同一物理内存（统一内存架构，UMA）：GPU 的
//! "显存" 是系统内存的一段（驱动侧可配置切分预留，carve-out）。多池账本
//! （`pipeline::vram`）与 §6.8b 动态准入账本据此把整机记为**单一共享池**，
//! 而非「独显池 + 系统内存池」两个互不相干的账本。
//!
//! ## 检测口径（best-effort + 静默降级）
//!
//! 1. 环境变量 `EP_UMA_TOTAL_MB`（十进制 MB）：显式声明即采信——GB10 等
//!    无平台名特征的统一内存设备的部署逃生门，也是测试注入点；
//! 2. `#[cfg(windows)]` 注册表探测（纯 FFI 免子进程，同 `cpu.rs` 注册表
//!    做法）：CPU 营销型号（`HARDWARE\DESCRIPTION\System\CentralProcessor\0
//!    \ProcessorNameString`）或系统产品名（`SYSTEM\CurrentControlSet\Control
//!    \SystemInformation\SystemProductName`）命中 NVIDIA GB10/Grace 特征
//!    （["gb10", "grace"]，归一化匹配）→ 视为 UMA，池容量取
//!    `GlobalMemoryStatusEx` 物理内存总量（`cpu.rs` 同源）；
//! 3. **任何一步失败（键缺失/读失败/容量未知/非 Windows 且无 env 覆盖）
//!    → `None`：无 UMA 池、消费方零行为差异**（探测绝不告警刷屏，静默降级）。
//!
//! 结果进程内缓存（[`OnceLock`]）：平台拓扑静态，daemon 2s 设备刷新周期
//! 下重复探测无意义；env 覆盖在首次调用时生效，此后进程内不再变化。
//!
//! ## GPU 切分预留（carve-out）语义 — 预留说明，不实现切分配置
//!
//! GB10 上驱动会为 GPU 划出一段预留（Windows 下常见如 48GB/64GB 档位），
//! GPU 侧实际可用 ≤ 统一内存总量。本模块**只报告物理总量**，不建模
//! carve-out 配置（预留）：消费方（§6.8b 准入账本）以「池容量 = 物理总量，
//! 实时占用兜底」记账——GPU 声明预算即便超过驱动切分额度，也由实时占用
//! 的 max 语义与模块自身启动失败兜底；把切分额度做成配置项留待真实设备
//! 实测后再定（避免拍脑袋常量进配置面）。

use std::sync::OnceLock;

/// 环境变量名：显式 UMA 声明（十进制 MB）。
pub const ENV_OVERRIDE: &str = "EP_UMA_TOTAL_MB";

/// UMA 平台识别特征 token（归一化后子串匹配）：GB10 Superchip 及
/// NVIDIA Grace CPU 系列。仅收录当前确知的统一内存平台代号，新平台走
/// [`ENV_OVERRIDE`] 显式声明。
// 唯一消费方是下方 #[cfg(windows)] 的注册表探测路径；非 Windows 目标编译
// 为死代码（CI rust job 首轮在 ubuntu 上实际命中），显式豁免以保留纯逻辑
// 的跨平台单测覆盖。
#[cfg_attr(not(windows), allow(dead_code))]
const PLATFORM_TOKENS: &[&str] = &["gb10", "grace"];

/// 解析 env 覆盖值（纯函数，可单测）：非空十进制 MB，0 视为未声明。
pub(crate) fn parse_env_override_mb(raw: &str) -> Option<u32> {
    raw.trim().parse::<u32>().ok().filter(|&mb| mb > 0)
}

/// 平台名判定（纯函数，可单测）：CPU 型号或系统产品名（归一化：小写、
/// 空白折叠）命中任一 UMA 平台 token。
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn looks_like_uma_platform(cpu_name: &str, system_product: &str) -> bool {
    let normalize =
        |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase();
    let cpu = normalize(cpu_name);
    let product = normalize(system_product);
    PLATFORM_TOKENS
        .iter()
        .any(|token| cpu.contains(token) || product.contains(token))
}

/// 识别统一内存架构并返回池容量（MB）。
///
/// 检测顺序见模块文档；失败静默降级为 `None`（消费方零行为差异）。
/// 结果进程内缓存——首次调用完成探测，此后直接命中。
pub fn unified_memory_total_mb() -> Option<u32> {
    static CACHE: OnceLock<Option<u32>> = OnceLock::new();
    *CACHE.get_or_init(detect_once)
}

fn detect_once() -> Option<u32> {
    // 1. 显式声明优先（部署逃生门 + 测试注入点）
    if let Ok(raw) = std::env::var(ENV_OVERRIDE) {
        if let Some(mb) = parse_env_override_mb(&raw) {
            return Some(mb);
        }
    }
    platform_probe()
}

/// 平台探测（非 Windows 恒 None——GB10 目标场景为 Windows 统一内存；
/// 探测完全静默，任何失败即 None）。
#[cfg(not(windows))]
fn platform_probe() -> Option<u32> {
    None
}

#[cfg(windows)]
fn platform_probe() -> Option<u32> {
    use super::cpu::memory_status_mb;

    let cpu_name = registry_read_sz(
        r"HARDWARE\DESCRIPTION\System\CentralProcessor\0",
        "ProcessorNameString",
    )
    .unwrap_or_default();
    let system_product = registry_read_sz(
        r"SYSTEM\CurrentControlSet\Control\SystemInformation",
        "SystemProductName",
    )
    .unwrap_or_default();
    if !looks_like_uma_platform(&cpu_name, &system_product) {
        return None;
    }
    // 平台特征命中后，池容量取物理内存总量（与 cpu.rs 内存探测同源）；
    // 容量未知（FFI 异常）→ 视为探测失败静默降级
    memory_status_mb().map(|(total_mb, _)| total_mb)
}

/// 读注册表 REG_SZ 值（Windows 纯 FFI，免子进程；缺失/类型不符/读失败
/// 返回 None——探测链路的静默降级基元）。
#[cfg(windows)]
fn registry_read_sz(sub_key: &str, value_name: &str) -> Option<String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    type Hkey = *mut core::ffi::c_void;
    type Lstatus = i32;

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

    let sub_key: Vec<u16> = OsString::from(sub_key)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let value: Vec<u16> = OsString::from(value_name)
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
        let mut buf = [0u16; 512];
        let mut buf_bytes = (buf.len() * 2) as u32;
        let mut kind: u32 = 0;
        let status = RegQueryValueExW(
            key,
            value.as_ptr(),
            ptr::null_mut(),
            &mut kind,
            buf.as_mut_ptr() as *mut u8,
            &mut buf_bytes,
        );
        let _ = RegCloseKey(key);
        if status != ERROR_SUCCESS || kind != REG_SZ || buf_bytes < 2 {
            return None;
        }
        let len = (buf_bytes as usize / 2 - 1).min(buf.len());
        let text = String::from_utf16_lossy(&buf[..len]);
        let text = text.trim();
        if text.is_empty() {
            None
        } else {
            Some(text.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_env_override_accepts_positive_mb_only() {
        assert_eq!(parse_env_override_mb("128000"), Some(128000));
        assert_eq!(parse_env_override_mb(" 65536 "), Some(65536));
        assert_eq!(parse_env_override_mb("0"), None, "0 视为未声明");
        assert_eq!(parse_env_override_mb(""), None);
        assert_eq!(parse_env_override_mb("abc"), None);
        assert_eq!(parse_env_override_mb("-100"), None);
        // u32 溢出 → None（静默降级，不 panic）
        assert_eq!(parse_env_override_mb("99999999999"), None);
    }

    #[test]
    fn uma_platform_tokens_match_normalized_names() {
        // GB10 特征：CPU 型号 / 系统产品名任一命中
        assert!(looks_like_uma_platform("NVIDIA GB10 Superchip", "Dev Kit"));
        assert!(looks_like_uma_platform("Generic ARM CPU", "NVIDIA DGX Spark GB10"));
        // Grace 系列
        assert!(looks_like_uma_platform("NVIDIA Grace CPU", "Anything"));
        // 归一化：大小写与空白折叠不影响判定
        assert!(looks_like_uma_platform("  NVIDIA   gb10  superchip ", "x"));
        assert!(looks_like_uma_platform("X", "GB10"));
        // 非 UMA 平台（消费级 CPU/常见整机名不得误报）
        assert!(!looks_like_uma_platform(
            "Intel(R) Core(TM) Ultra 7 270HX",
            "ROG Strix SCAR 16"
        ));
        assert!(!looks_like_uma_platform(
            "AMD Ryzen 9 9950X",
            "ProArt X870E"
        ));
        // 空输入
        assert!(!looks_like_uma_platform("", ""));
    }

    /// 进程级缓存探测入口：本机（探测链路必可达）调用不 panic；
    /// 非 UMA 平台返回 None（或 env 覆盖生效时返回声明值）。
    #[test]
    fn unified_memory_probe_is_total_and_silent() {
        let override_raw = std::env::var(ENV_OVERRIDE).ok();
        let detected = unified_memory_total_mb();
        match override_raw.as_deref().and_then(parse_env_override_mb) {
            Some(mb) => assert_eq!(detected, Some(mb), "env 覆盖应生效"),
            None => {
                // 探测失败静默降级：None 合法；Windows 命中 GB10 特征时
                // 应给出非零容量（CI/开发机为 x86 消费级平台，None 分支）
                if let Some(mb) = detected {
                    assert!(mb > 0);
                }
            }
        }
    }
}
