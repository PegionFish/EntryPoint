//! AMD ROCm 设备检测（rocm-smi JSON 模式，best-effort）
//!
//! 数据源命令：
//! `rocm-smi --showproductname --showid --showmeminfo vram --showuse --showtemp --json`
//!
//! 输出形如：
//! ```json
//! {
//!   "card0": { "Card series": "...", "VRAM Total Memory (B)": "...", ... },
//!   "system": { "Driver version": "..." }
//! }
//! ```
//!
//! 平台分支（§15.3）：
//! - Linux：PATH 的 `rocm-smi` + `/opt/rocm/bin/rocm-smi` 回退
//! - Windows：ROCm 官方不支持 Windows，仅在 PATH 恰好提供 rocm-smi 时 best-effort 尝试
//!
//! 任何异常（命令缺失 / JSON 畸形 / 字段缺失 / 超时）一律优雅降级为空设备列表。
//! 无 AMD 真机，解析逻辑由内联 fixture 覆盖。

use crate::types::{ComputeBackend, ComputeDevice, DeviceId};

use super::{candidate_is_viable, json_value_as_u64, run_tool, DeviceDetector, TOOL_TIMEOUT};

pub struct RocmDetector;

const SMI_ARGS: &[&str] = &[
    "--showproductname",
    "--showid",
    "--showmeminfo",
    "vram",
    "--showuse",
    "--showtemp",
    "--json",
];

const BYTES_PER_MB: u64 = 1024 * 1024;

/// rocm-smi 候选路径（按尝试顺序）
fn rocm_smi_candidates() -> Vec<String> {
    let mut candidates = Vec::new();
    if let Ok(p) = std::env::var("EP_ROCM_SMI_PATH") {
        if !p.trim().is_empty() {
            candidates.push(p.trim().to_string());
        }
    }
    candidates.push("rocm-smi".to_string());
    if cfg!(unix) {
        candidates.push("/opt/rocm/bin/rocm-smi".to_string());
    }
    candidates
}

fn run_rocm_smi() -> Option<String> {
    for candidate in rocm_smi_candidates() {
        if !candidate_is_viable(&candidate) {
            continue;
        }
        if let Some(output) = run_tool(&candidate, SMI_ARGS, TOOL_TIMEOUT) {
            return Some(output);
        }
    }
    None
}

/// 卡片字段的版本兼容取值：rocm-smi 新旧版键名大小写不一
/// （旧 `Card series`/`Card model`，新 `Card Series`/`Device Name`），
/// 依次尝试多个候选键（忽略大小写），返回首个非空字符串值。
fn card_field(card: &serde_json::Value, candidates: &[&str]) -> Option<String> {
    let obj = card.as_object()?;
    for wanted in candidates {
        for (key, value) in obj {
            if key.eq_ignore_ascii_case(wanted) {
                if let Some(s) = value.as_str() {
                    let s = s.trim();
                    if !s.is_empty() {
                        return Some(s.to_string());
                    }
                }
            }
        }
    }
    None
}

/// 解析 rocm-smi JSON 输出。畸形/缺失字段一律降级为 None 或跳过。
pub(crate) fn parse_rocm_smi_json(output: &str) -> Vec<ComputeDevice> {
    let mut devices: Vec<ComputeDevice> = Vec::new();

    let Ok(root) = serde_json::from_str::<serde_json::Value>(output) else {
        return devices;
    };
    let Some(map) = root.as_object() else {
        return devices;
    };

    let mut cards: Vec<(u32, &serde_json::Value)> = Vec::new();
    for (key, value) in map {
        if key == "system" {
            continue;
        }
        let Some(index) = key
            .strip_prefix("card")
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        cards.push((index, value));
    }
    cards.sort_by_key(|(index, _)| *index);

    for (index, card) in cards {
        // 名称兜底链：Card series(/Series) → Device Name → Card model(/Model)
        // → AMD GPU <index>。真机教训：新版 rocm-smi 只给 `Card Series`
        // （大写 S），仅认旧键会静默退化为通用名「AMD GPU 0」。
        let name = card_field(card, &["Card series", "Device Name", "Card model"])
            .unwrap_or_else(|| format!("AMD GPU {index}"));

        let total_memory_mb = card
            .get("VRAM Total Memory (B)")
            .and_then(json_value_as_u64)
            .map(|bytes| (bytes / BYTES_PER_MB) as u32);
        let used_memory_mb = card
            .get("VRAM Total Used Memory (B)")
            .and_then(json_value_as_u64)
            .map(|bytes| (bytes / BYTES_PER_MB) as u32);
        let utilization = card
            .get("GPU use (%)")
            .and_then(json_value_as_u64)
            .map(|v| v.min(100) as u8);
        let temperature = card
            .get("Temperature (Sensor edge) (C)")
            .and_then(json_value_as_u64)
            .map(|v| v.min(u64::from(u8::MAX)) as u8);

        devices.push(ComputeDevice {
            id: DeviceId::Rocm(index),
            backend: ComputeBackend::Rocm,
            name,
            total_memory_mb,
            used_memory_mb,
            utilization,
            temperature,
        });
    }
    devices
}

impl DeviceDetector for RocmDetector {
    fn backend(&self) -> ComputeBackend {
        ComputeBackend::Rocm
    }

    fn detect(&self) -> Vec<ComputeDevice> {
        match run_rocm_smi() {
            Some(output) => parse_rocm_smi_json(&output),
            None => Vec::new(),
        }
    }

    fn refresh(&self, devices: &mut [ComputeDevice]) {
        let Some(output) = run_rocm_smi() else {
            return;
        };
        let fresh = parse_rocm_smi_json(&output);
        if fresh.is_empty() {
            return;
        }
        for dev in devices.iter_mut() {
            if dev.backend != ComputeBackend::Rocm {
                continue;
            }
            if let Some(updated) = fresh.iter().find(|f| f.id == dev.id) {
                dev.used_memory_mb = updated.used_memory_mb.or(dev.used_memory_mb);
                dev.utilization = updated.utilization.or(dev.utilization);
                dev.temperature = updated.temperature.or(dev.temperature);
                if updated.total_memory_mb.is_some() {
                    dev.total_memory_mb = updated.total_memory_mb;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实 rocm-smi 输出样本（`--showproductname --showid --showmeminfo vram
    /// --showuse --showtemp --json` 结构，双卡 + system 段）
    const ROCM_SMI_FIXTURE: &str = r#"{
    "card0": {
        "Card series": "Navi 31 [Radeon RX 7900 XTX]",
        "Card model": "Radeon RX 7900 Series",
        "Card vendor": "Advanced Micro Devices, Inc.",
        "GPU ID": "0x744c",
        "Unique ID": "0x43cc6de9b1f2d9a8",
        "VRAM Total Memory (B)": "25756762112",
        "VRAM Total Used Memory (B)": "562364416",
        "GPU use (%)": "23",
        "Temperature (Sensor edge) (C)": "41.0"
    },
    "card1": {
        "Card series": "Navi 32 [Radeon RX 7800 XT]",
        "GPU ID": "0x747e",
        "Unique ID": "0x8f13b2c4d5e6f708",
        "VRAM Total Memory (B)": "17163091968",
        "VRAM Total Used Memory (B)": "270876672",
        "GPU use (%)": "0",
        "Temperature (Sensor edge) (C)": "32.5"
    },
    "system": {
        "Driver version": "6.3.6"
    }
}"#;

    #[test]
    fn test_parse_two_cards() {
        let devices = parse_rocm_smi_json(ROCM_SMI_FIXTURE);
        assert_eq!(devices.len(), 2);

        let d0 = &devices[0];
        assert_eq!(d0.id, DeviceId::Rocm(0));
        assert_eq!(d0.backend, ComputeBackend::Rocm);
        assert_eq!(d0.name, "Navi 31 [Radeon RX 7900 XTX]");
        // 25756762112 B = 24563.9 MB → 24563
        assert_eq!(d0.total_memory_mb, Some(24563));
        // 562364416 B = 536.3 MB → 536
        assert_eq!(d0.used_memory_mb, Some(536));
        assert_eq!(d0.utilization, Some(23));
        assert_eq!(d0.temperature, Some(41));

        let d1 = &devices[1];
        assert_eq!(d1.id, DeviceId::Rocm(1));
        // 17163091968 B = 16368 MB
        assert_eq!(d1.total_memory_mb, Some(16368));
        assert_eq!(d1.utilization, Some(0));
        // "32.5" 浮点字符串容错
        assert_eq!(d1.temperature, Some(32));
    }

    #[test]
    fn test_system_section_skipped() {
        let devices = parse_rocm_smi_json(ROCM_SMI_FIXTURE);
        assert!(devices.iter().all(|d| d.backend == ComputeBackend::Rocm));
    }

    #[test]
    fn test_parse_empty_and_malformed() {
        assert!(parse_rocm_smi_json("").is_empty());
        assert!(parse_rocm_smi_json("not json at all").is_empty());
        assert!(parse_rocm_smi_json("[1, 2, 3]").is_empty()); // 非 object
        assert!(parse_rocm_smi_json("{}").is_empty());
    }

    #[test]
    fn test_parse_missing_fields_degrade_to_none() {
        let output = r#"{"card0": {"VRAM Total Memory (B)": "1048576"}}"#;
        let devices = parse_rocm_smi_json(output);
        assert_eq!(devices.len(), 1);
        let d = &devices[0];
        assert_eq!(d.total_memory_mb, Some(1));
        assert_eq!(d.used_memory_mb, None);
        assert_eq!(d.utilization, None);
        assert_eq!(d.temperature, None);
        assert_eq!(d.name, "AMD GPU 0"); // 缺 Card series 时的兜底名
    }

    #[test]
    fn test_parse_garbage_values_are_tolerated() {
        let output = r#"{
            "card0": {
                "Card series": "RX 7900 XTX",
                "VRAM Total Memory (B)": "N/A",
                "VRAM Total Used Memory (B)": "???",
                "GPU use (%)": "not-a-number",
                "Temperature (Sensor edge) (C)": ""
            },
            "not-a-card": {"x": 1}
        }"#;
        let devices = parse_rocm_smi_json(output);
        assert_eq!(devices.len(), 1);
        let d = &devices[0];
        assert_eq!(d.name, "RX 7900 XTX");
        assert_eq!(d.total_memory_mb, None);
        assert_eq!(d.used_memory_mb, None);
        assert_eq!(d.utilization, None);
        assert_eq!(d.temperature, None);
    }

    #[test]
    fn test_cards_sorted_by_index() {
        let output = r#"{
            "card2": {"Card series": "C2"},
            "card0": {"Card series": "C0"},
            "card1": {"Card series": "C1"}
        }"#;
        let devices = parse_rocm_smi_json(output);
        let ids: Vec<_> = devices.iter().map(|d| d.id.clone()).collect();
        assert_eq!(
            ids,
            vec![DeviceId::Rocm(0), DeviceId::Rocm(1), DeviceId::Rocm(2)]
        );
    }

    #[test]
    fn test_utilization_clamped_to_100() {
        let output = r#"{"card0": {"GPU use (%)": "150"}}"#;
        let devices = parse_rocm_smi_json(output);
        assert_eq!(devices[0].utilization, Some(100));
    }

    /// 真机（ROCm 7.x）实测形态：键名 `Card Series` 大写 S + `Device Name`，
    /// 旧解析仅认小写 s 键导致静默退化为「AMD GPU 0」
    #[test]
    fn test_new_smi_key_casing_still_yields_proper_name() {
        let output = r#"{"card0": {
            "Device Name": "AMD Radeon RX 7900 XTX",
            "Card Series": "AMD Radeon RX 7900 XTX",
            "Card Model": "0x744c",
            "GFX Version": "gfx1100"
        }}"#;
        let devices = parse_rocm_smi_json(output);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "AMD Radeon RX 7900 XTX");
    }

    #[test]
    fn test_name_fallback_chain_device_name_then_model() {
        // 无 Card series → Device Name；再无 → Card model；全无 → AMD GPU <n>
        let output = r#"{
            "card0": {"Card model": "Radeon RX 7900 Series"},
            "card1": {}
        }"#;
        let devices = parse_rocm_smi_json(output);
        assert_eq!(devices[0].name, "Radeon RX 7900 Series");
        assert_eq!(devices[1].name, "AMD GPU 1");
    }
}
