//! Vulkan 备选后端检测（best-effort，HETERO_DIST_PLAN §4 M4）
//!
//! 定位与策略：
//! - **备选位**：注册顺序置于 DirectML 之后、Cpu 之前。与 DirectML 相同的
//!   归一化名称去重策略——仅当更高优先级后端（CUDA/ROCm/OpenVINO/DirectML）
//!   均未覆盖某物理适配器时才产出设备，保证"厂商栈优先、Vulkan 兜底"
//!   （E8：模拟禁用厂商栈后调度自动落 vulkan）。
//! - 数据源为 `vulkaninfo`（Khronos Vulkan-Tools）。探测形态按序尝试：
//!   1. `vulkaninfo --summary --format=json`（契约形态；设备名取 `gpuName`，
//!      回退 `deviceName`）；
//!   2. 文本兜底 `vulkaninfo --summary`——发行版构建普遍**不支持**
//!      `--format=json`（实测 exit 1 打印 usage），且 `-j/--json` 只把
//!      `VP_VULKANINFO_*.json` 写进当前工作目录（stdout 为空），对常驻
//!      daemon 是不可接受的副作用，故不采用；文本 summary 的 GPU 分块
//!      含 `deviceName = …` / `gpuName = …` 行，逐块解析。
//! - 过滤软件实现：`deviceType` 为 CPU 的条目（lavapipe 等）与名称含
//!   llvmpipe/lavapipe/swiftshader/basic render 的条目均无物理 GPU。
//! - 同名多 ICD 条目（如 radv 与 amdvlk 并装）按归一化名称去重。
//!
//! 环境变量：`EP_VULKANINFO_PATH`（工具路径覆盖）。
//! 任何探测失败 / 畸形输出一律优雅降级为空设备列表，绝不 panic。

use crate::types::{ComputeBackend, ComputeDevice, DeviceId};

use super::{candidate_is_viable, normalize_device_name, run_tool, DeviceDetector, TOOL_TIMEOUT};

pub struct VulkanDetector;

/// 软件实现 / 无物理 GPU 的适配器名特征（大小写不敏感子串匹配）
const SOFTWARE_NAME_MARKERS: &[&str] = &[
    "llvmpipe",
    "lavapipe",
    "swiftshader",
    "basic render",
];

fn make_vulkan_device(index: u32, name: String) -> ComputeDevice {
    ComputeDevice {
        id: DeviceId::Vulkan(index),
        backend: ComputeBackend::Vulkan,
        name,
        // vulkaninfo summary 无轻量可靠的本地显存总量字段，统一 None
        total_memory_mb: None,
        used_memory_mb: None,
        utilization: None,
        temperature: None,
    }
}

/// 名称是否指向软件实现（无物理 GPU）
fn is_software_renderer(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SOFTWARE_NAME_MARKERS.iter().any(|m| lower.contains(m))
}

/// deviceType 值是否为软件实现（lavapipe 等上报 CPU 类型）
fn device_type_indicates_cpu(device_type: Option<&str>) -> bool {
    device_type
        .map(|t| t.eq_ignore_ascii_case("cpu") || t.to_ascii_uppercase().ends_with("_CPU"))
        .unwrap_or(false)
}

/// 追加设备：归一化名称去重后按连续序号入列
fn push_dedup(devices: &mut Vec<ComputeDevice>, name: String) {
    let normalized = normalize_device_name(&name);
    if devices
        .iter()
        .any(|d| normalize_device_name(&d.name) == normalized)
    {
        return;
    }
    let index = devices.len() as u32;
    devices.push(make_vulkan_device(index, name));
}

// ─── 探测形态 1（契约）：`--summary --format=json` ──────────────────────────

/// 解析 `vulkaninfo --summary --format=json` 输出（纯函数，fixture 可测）。
///
/// 结构假设（版本间有差异，逐项容错）：
/// ```json
/// { "Version": "…", "Devices": [ { "gpuName": "…", "deviceName": "…",
///   "driverName": "…", "deviceType": "DISCRETE_GPU" } ] }
/// ```
pub(crate) fn parse_vulkaninfo_summary_json(output: &str) -> Vec<ComputeDevice> {
    let mut devices: Vec<ComputeDevice> = Vec::new();
    let Ok(root) = serde_json::from_str::<serde_json::Value>(output) else {
        return devices;
    };
    let Some(list) = root
        .get("Devices")
        .or_else(|| root.get("devices"))
        .and_then(|v| v.as_array())
    else {
        return devices;
    };

    for item in list {
        if device_type_indicates_cpu(item.get("deviceType").and_then(|v| v.as_str())) {
            continue;
        }
        // 设备名取 gpuName（契约口径），缺省回退 deviceName
        let Some(name) = ["gpuName", "deviceName"].iter().find_map(|k| {
            item.get(*k)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        }) else {
            continue;
        };
        if is_software_renderer(&name) {
            continue;
        }
        push_dedup(&mut devices, name);
    }
    devices
}

// ─── 探测形态 2（兜底）：文本 `--summary` ───────────────────────────────────

/// 解析 `vulkaninfo --summary` 文本输出（纯函数，fixture 可测）。
///
/// 输出以 `GPU<n>:` 行分块，块内为 `key = value` 行；设备名取
/// `gpuName` / `deviceName` 键（不同版本二选一），软件实现按
/// `deviceType`（如 `PHYSICAL_DEVICE_TYPE_CPU`）与名称特征过滤。
pub(crate) fn parse_vulkaninfo_summary_text(output: &str) -> Vec<ComputeDevice> {
    #[derive(Default)]
    struct Block {
        device_type: Option<String>,
        name: Option<String>,
    }

    let mut devices: Vec<ComputeDevice> = Vec::new();
    let mut current = Block::default();

    let flush = |block: &mut Block, out: &mut Vec<ComputeDevice>| {
        if let Some(name) = block.name.take() {
            let cpu_typed = block
                .device_type
                .as_deref()
                .map(|t| t.eq_ignore_ascii_case("cpu") || t.to_ascii_uppercase().ends_with("_CPU"))
                .unwrap_or(false);
            if !cpu_typed && !is_software_renderer(&name) {
                push_dedup(out, name);
            }
        }
        block.device_type = None;
    };

    for raw in output.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // GPU 分块头："GPU0:" / "GPU12:"（宽松匹配 GPU + 数字前缀）
        let is_block_header = line
            .strip_prefix("GPU")
            .map(|rest| {
                let digits_end = rest
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(rest.len());
                digits_end > 0
                    && rest[digits_end..]
                        .trim_start_matches(':')
                        .trim()
                        .is_empty()
            })
            .unwrap_or(false);
        if is_block_header {
            flush(&mut current, &mut devices);
            continue;
        }
        let Some((key_raw, value)) = line.split_once('=') else {
            continue;
        };
        let key = key_raw.trim().to_ascii_lowercase();
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key.as_str() {
            "gpuname" | "devicename" => current.name = Some(value.to_string()),
            "devicetype" => current.device_type = Some(value.to_string()),
            _ => {}
        }
    }
    flush(&mut current, &mut devices);
    devices
}

// ─── 子进程探测 ─────────────────────────────────────────────────────────────

/// vulkaninfo 可执行文件：环境变量覆盖优先，缺省走 PATH 解析
fn vulkaninfo_program() -> String {
    std::env::var("EP_VULKANINFO_PATH")
        .ok()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "vulkaninfo".to_string())
}

/// 执行探测并解析：契约 JSON 形态命中即权威；否则文本 summary 兜底。
/// 两种形态皆失败（工具缺失 / 非零退出 / 超时）→ 空列表。
fn probe_devices() -> Vec<ComputeDevice> {
    let program = vulkaninfo_program();
    if !candidate_is_viable(&program) {
        return Vec::new();
    }
    // 1) 契约形态：--summary --format=json（设备名取 gpuName）
    if let Some(output) = run_tool(&program, &["--summary", "--format=json"], TOOL_TIMEOUT) {
        let devices = parse_vulkaninfo_summary_json(&output);
        if !devices.is_empty() {
            return devices;
        }
    }
    // 2) 文本兜底：--summary（发行版构建普遍唯一可用形态）
    if let Some(output) = run_tool(&program, &["--summary"], TOOL_TIMEOUT) {
        return parse_vulkaninfo_summary_text(&output);
    }
    Vec::new()
}

impl VulkanDetector {
    /// 检测 Vulkan 物理设备，并对更高优先级后端已检出的设备去重。
    /// `known` 为 CUDA/ROCm/OpenVINO/DirectML 已检出的设备列表。
    pub fn detect_excluding(&self, known: &[ComputeDevice]) -> Vec<ComputeDevice> {
        filter_known(probe_devices(), known)
    }
}

/// 纯函数：剔除归一化名称已被更高优先级后端覆盖的设备（fixture 可测）
fn filter_known(raw: Vec<ComputeDevice>, known: &[ComputeDevice]) -> Vec<ComputeDevice> {
    let known_normalized: Vec<String> = known
        .iter()
        .map(|d| normalize_device_name(&d.name))
        .collect();
    raw.into_iter()
        .filter(|d| !known_normalized.contains(&normalize_device_name(&d.name)))
        .collect()
}

impl DeviceDetector for VulkanDetector {
    fn backend(&self) -> ComputeBackend {
        ComputeBackend::Vulkan
    }

    /// 独立检测（无去重上下文）——枚举全部物理设备
    fn detect(&self) -> Vec<ComputeDevice> {
        self.detect_excluding(&[])
    }

    fn refresh(&self, _devices: &mut [ComputeDevice]) {
        // 无轻量利用率/显存信号源（summary 不含采样数据），保持既有值
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 契约 JSON 形态样本：gpuName/deviceName 混用 + 多 ICD 同名 + CPU 型条目
    const SUMMARY_JSON_FIXTURE: &str = r#"{
        "Version": "1.4.357",
        "Devices": [
            {
                "gpuName": "AMD Radeon RX 7900 XTX",
                "deviceName": "AMD Radeon RX 7900 XTX (RADV NAVI31)",
                "driverName": "radv",
                "driverInfo": "26.1.8",
                "deviceType": "DISCRETE_GPU"
            },
            {
                "deviceName": "NVIDIA GeForce RTX 5090 D",
                "driverName": "NVIDIA",
                "deviceType": "DISCRETE_GPU"
            },
            {
                "deviceName": "AMD Radeon RX 7900 XTX",
                "driverName": "amdvlk",
                "deviceType": "DISCRETE_GPU"
            },
            {
                "deviceName": "llvmpipe (LLVM 18.1.8, 256 bits)",
                "driverName": "llvmpipe",
                "deviceType": "CPU"
            }
        ]
    }"#;

    #[test]
    fn test_parse_summary_json() {
        let devices = parse_vulkaninfo_summary_json(SUMMARY_JSON_FIXTURE);
        assert_eq!(devices.len(), 2, "同名多 ICD 与 CPU 型条目应被过滤");
        assert_eq!(devices[0].id, DeviceId::Vulkan(0));
        assert_eq!(devices[0].backend, ComputeBackend::Vulkan);
        assert_eq!(devices[0].name, "AMD Radeon RX 7900 XTX", "设备名取 gpuName");
        assert_eq!(devices[1].id, DeviceId::Vulkan(1));
        assert_eq!(devices[1].name, "NVIDIA GeForce RTX 5090 D");
        assert_eq!(devices[1].total_memory_mb, None);
    }

    #[test]
    fn test_parse_summary_json_malformed() {
        assert!(parse_vulkaninfo_summary_json("").is_empty());
        assert!(parse_vulkaninfo_summary_json("garbage").is_empty());
        assert!(parse_vulkaninfo_summary_json("{}").is_empty());
        assert!(parse_vulkaninfo_summary_json(r#"{"Devices": "oops"}"#).is_empty());
        assert!(parse_vulkaninfo_summary_json(r#"{"devices": []}"#).is_empty());
    }

    #[test]
    fn test_parse_summary_json_software_name_filtered_even_without_cpu_type() {
        // 个别构建不给 deviceType，仅凭名称也要拦住软件实现
        let output = r#"{"Devices": [
            {"deviceName": "SwiftShader Device (LLVM 17.0.6)", "deviceType": "DISCRETE_GPU"},
            {"deviceName": "Microsoft Basic Render Driver"},
            {"deviceName": "Intel(R) Arc(TM) B580 Graphics"}
        ]}"#;
        let devices = parse_vulkaninfo_summary_json(output);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "Intel(R) Arc(TM) B580 Graphics");
    }

    /// 文本 summary 样本（真机 1.4.357 实测结构：GPU 分块 + `key = value` 行）
    const SUMMARY_TEXT_FIXTURE: &str = "\
==========
VULKANINFO
==========

Vulkan Instance Version: 1.4.357


Devices:
========
\tGPU0:
\t\tapiVersion         = 1.4.341
\t\tdriverVersion      = 610.57.4.0
\t\tdeviceType         = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU
\t\tdeviceName         = NVIDIA GeForce RTX 5090 D
\t\tdriverID           = DRIVER_ID_NVIDIA_PROPRIETARY

\tGPU1:
\t\tapiVersion         = 1.4.354
\t\tdeviceType         = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU
\t\tdeviceName         = AMD Radeon RX 7900 XTX (RADV NAVI31)
\t\tdriverID           = DRIVER_ID_MESA_RADV

\tGPU2:
\t\tapiVersion         = 1.4.354
\t\tdeviceType         = PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU
\t\tdeviceName         = Intel(R) Graphics (ARL)
";

    #[test]
    fn test_parse_summary_text_real_machine_shape() {
        let devices = parse_vulkaninfo_summary_text(SUMMARY_TEXT_FIXTURE);
        assert_eq!(devices.len(), 3);
        assert_eq!(devices[0].id, DeviceId::Vulkan(0));
        assert_eq!(devices[0].name, "NVIDIA GeForce RTX 5090 D");
        assert_eq!(devices[1].id, DeviceId::Vulkan(1));
        assert_eq!(devices[1].name, "AMD Radeon RX 7900 XTX (RADV NAVI31)");
        assert_eq!(devices[2].id, DeviceId::Vulkan(2));
        assert_eq!(devices[2].name, "Intel(R) Graphics (ARL)");
    }

    #[test]
    fn test_parse_summary_text_filters_software_and_renames_gpuname_key() {
        // gpuName 键变体 + CPU 型（lavapipe）+ 名称黑名单三重过滤
        let output = "\
GPU0:
\tdeviceType         = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU
\tgpuName            = AMD Radeon RX 7900 XTX
GPU1:
\tdeviceType         = PHYSICAL_DEVICE_TYPE_CPU
\tdeviceName         = llvmpipe (LLVM 15.0.7, 256 bits)
GPU2:
\tdeviceType         = PHYSICAL_DEVICE_TYPE_CPU
\tdeviceName         = lavapipe (LLVM 18.1.8)
";
        let devices = parse_vulkaninfo_summary_text(output);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "AMD Radeon RX 7900 XTX");
    }

    #[test]
    fn test_parse_summary_text_malformed() {
        assert!(parse_vulkaninfo_summary_text("").is_empty());
        assert!(parse_vulkaninfo_summary_text("no relevant content").is_empty());
        assert!(
            parse_vulkaninfo_summary_text("GPU0:\n\tapiVersion = 1.4.0\n").is_empty(),
            "无名分块不得产出设备"
        );
    }

    #[test]
    fn test_filter_known_dedups_against_higher_priority_backends() {
        // 真机形态：NVIDIA 归 CUDA、Intel iGPU 归 OpenVINO 后，
        // Vulkan 只保留未被覆盖的 AMD 卡（备选位语义）
        use crate::types::{ComputeDevice, DeviceId};
        let known = vec![
            ComputeDevice {
                id: DeviceId::Cuda(0),
                backend: ComputeBackend::Cuda,
                name: "NVIDIA GeForce RTX 5090 D".to_string(),
                total_memory_mb: Some(32255),
                used_memory_mb: None,
                utilization: None,
                temperature: None,
            },
            ComputeDevice {
                id: DeviceId::OpenVINO("GPU.0".to_string()),
                backend: ComputeBackend::OpenVINO,
                name: "Intel(R) Graphics".to_string(),
                total_memory_mb: None,
                used_memory_mb: None,
                utilization: None,
                temperature: None,
            },
        ];

        let raw = vec![
            make_vulkan_device(0, "NVIDIA GeForce RTX 5090 D".to_string()),
            make_vulkan_device(1, "AMD Radeon RX 7900 XTX (RADV NAVI31)".to_string()),
            // 与 OpenVINO 已检出设备同名（去重按归一化名称精确匹配，
            // 与 DirectML 现有语义一致；带 (ARL) 后缀的变名不会被去重）
            make_vulkan_device(2, "Intel(R) Graphics".to_string()),
        ];
        let survived = filter_known(raw, &known);
        assert_eq!(survived.len(), 1);
        assert_eq!(survived[0].name, "AMD Radeon RX 7900 XTX (RADV NAVI31)");

        // 全部被覆盖（名称归一化容忍大小写/空白差异）→ 空列表：
        // 厂商栈可用时 Vulkan 隐身，兜底语义成立
        let all_covered = vec![
            make_vulkan_device(0, "NVIDIA GeForce RTX 5090 D".to_string()),
            make_vulkan_device(1, "intel  graphics".to_string()),
        ];
        assert!(filter_known(all_covered, &known).is_empty());
    }
}
