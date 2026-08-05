//! Intel OpenVINO 设备检测（best-effort 多路探测，重点后端）
//!
//! 探测优先级：
//! 1. `xpu-smi discovery --dump`（JSON）— Intel GPU（iGPU/dGPU）枚举
//! 2. `intel-npu-smi`（PATH → Windows 常见安装路径有界递归探测）— Intel NPU 枚举
//! 3. 兜底（SMI 工具缺失时，用户真机即此场景）：
//!    - Windows：Win32_PnPEntity 名称匹配（NPU/AI Boost）+ Win32_VideoController 中的 Intel 显卡
//!    - Linux：lspci 行匹配
//! 4. 可选权威查询：OpenVINO Python 运行时（`EP_OPENVINO_PYTHON_PROBE=1` 启用，默认关闭；
//!    避免 daemon 周期性重检测时的 python 冷启动开销）
//!
//! 设备 ID 约定（对齐 OpenVINO 运行时设备命名，见 PACK_UNIFY_PLAN §6.2 `openvino:GPU.0`）：
//! - iGPU/dGPU → `openvino:GPU.<n>`
//! - NPU → `openvino:NPU.<n>`
//!
//! 设备名保留硬件原始名（便于与 DirectML 的显示适配器名去重匹配），
//! NPU/iGPU 的区分体现在 DeviceId 命名空间（NPU.* vs GPU.*）。
//!
//! 环境变量：`EP_XPU_SMI_PATH` / `EP_INTEL_NPU_SMI_PATH`（工具路径覆盖）、
//! `EP_OPENVINO_PYTHON_PROBE=1` + `EP_OPENVINO_PYTHON`（解释器覆盖）。
//!
//! 任何探测失败 / 畸形输出一律优雅降级为空设备，绝不 panic。
//! 无 Intel SMI 工具真机环境，解析逻辑由内联 fixture 覆盖；
//! 实际输出格式差异由 Wave 5 真机验证调优。

use std::time::Duration;

use crate::types::{ComputeBackend, ComputeDevice, DeviceId};

use super::{
    candidate_is_viable, json_value_as_u64, run_tool, DeviceDetector, TOOL_TIMEOUT,
};
#[cfg(windows)]
use super::{parse_plain_lines, parse_wmic_name_values};

pub struct OpenVinoDetector;

const XPU_SMI_ARGS: &[&str] = &["discovery", "--dump"];
const PYTHON_PROBE_TIMEOUT: Duration = Duration::from_secs(15);

// ─── 通用构造/判定 ───────────────────────────────────────────────────────────

fn make_openvino_device(
    id_suffix: String,
    name: String,
    total_memory_mb: Option<u32>,
    utilization: Option<u8>,
) -> ComputeDevice {
    ComputeDevice {
        id: DeviceId::OpenVINO(id_suffix),
        backend: ComputeBackend::OpenVINO,
        name,
        total_memory_mb,
        used_memory_mb: None,
        utilization,
        temperature: None,
    }
}

#[cfg(unix)]
fn is_npu_id(id: &DeviceId) -> bool {
    matches!(id, DeviceId::OpenVINO(s) if s.starts_with("NPU."))
}

fn is_gpu_id(id: &DeviceId) -> bool {
    matches!(id, DeviceId::OpenVINO(s) if s.starts_with("GPU."))
}

/// 合并去重：仅追加 existing 中尚不存在的设备（按 DeviceId）
pub(crate) fn merge_unique(existing: &mut Vec<ComputeDevice>, extra: Vec<ComputeDevice>) {
    for dev in extra {
        if !existing.iter().any(|d| d.id == dev.id) {
            existing.push(dev);
        }
    }
}

// ─── 探测 1：xpu-smi discovery（JSON dump）──────────────────────────────────

fn xpu_smi_candidates() -> Vec<String> {
    let mut candidates = Vec::new();
    if let Ok(p) = std::env::var("EP_XPU_SMI_PATH") {
        if !p.trim().is_empty() {
            candidates.push(p.trim().to_string());
        }
    }
    candidates.push("xpu-smi".to_string());
    if cfg!(windows) {
        candidates.push(r"C:\Program Files\Intel\xpu-smi\bin\xpu-smi.exe".to_string());
    }
    candidates
}

fn xpu_smi_devices() -> Vec<ComputeDevice> {
    for candidate in xpu_smi_candidates() {
        if !candidate_is_viable(&candidate) {
            continue;
        }
        // 首个可执行成功的候选即为权威：其输出（含空设备列表）即结果
        if let Some(output) = run_tool(&candidate, XPU_SMI_ARGS, TOOL_TIMEOUT) {
            return parse_xpu_smi_discovery_json(&output);
        }
    }
    Vec::new()
}

/// 解析 `xpu-smi discovery --dump` JSON。仅收物理 GPU 设备（跳过 SRIOV VF）。
pub(crate) fn parse_xpu_smi_discovery_json(output: &str) -> Vec<ComputeDevice> {
    let mut devices: Vec<ComputeDevice> = Vec::new();

    let Ok(root) = serde_json::from_str::<serde_json::Value>(output) else {
        return devices;
    };
    let Some(list) = root.get("device_list").and_then(|v| v.as_array()) else {
        return devices;
    };

    for (seq, item) in list.iter().enumerate() {
        if let Some(ft) = item.get("function_type").and_then(|v| v.as_str()) {
            if !ft.eq_ignore_ascii_case("physical") {
                continue;
            }
        }
        if let Some(dt) = item.get("device_type").and_then(|v| v.as_str()) {
            if !dt.eq_ignore_ascii_case("gpu") {
                continue;
            }
        }

        let device_id = item
            .get("device_id")
            .and_then(json_value_as_u64)
            .unwrap_or(seq as u64) as u32;
        let name = item
            .get("device_name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("Intel GPU {device_id}"));
        let total_memory_mb = item
            .get("memory_size")
            .and_then(json_value_as_u64)
            .map(|v| v.min(u64::from(u32::MAX)) as u32); // xpu-smi memory_size 单位 MiB
        let utilization = item
            .get("gpu_utilization")
            .and_then(json_value_as_u64)
            .map(|v| v.min(100) as u8);

        devices.push(make_openvino_device(
            format!("GPU.{device_id}"),
            name,
            total_memory_mb,
            utilization,
        ));
    }
    devices
}

// ─── 探测 2：intel-npu-smi ───────────────────────────────────────────────────

#[cfg(windows)]
mod npu_path_probe {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{LazyLock, Mutex};
    use std::time::{Duration, Instant};

    /// 缓存条目：解析出的路径（None = 未找到）+ 解析时间
    type PathCacheEntry = (Option<PathBuf>, Instant);

    /// 已解析安装路径缓存（探测目录遍历成本不低，daemon 周期性重检测需缓存）
    static INSTALL_PATH_CACHE: LazyLock<Mutex<HashMap<String, PathCacheEntry>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    const CACHE_TTL: Duration = Duration::from_secs(300);

    fn cached_install_path(key: &str, probe: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
        let mut cache = INSTALL_PATH_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((path, checked_at)) = cache.get(key) {
            if checked_at.elapsed() < CACHE_TTL {
                return path.clone();
            }
        }
        let path = probe();
        cache.insert(key.to_string(), (path.clone(), Instant::now()));
        path
    }

    /// 目录子树有界递归查找可执行文件（BFS/DFS 混合，深度与访问量双上限）
    pub(super) fn find_exe_under(
        root: &Path,
        name: &str,
        max_depth: usize,
        max_visits: usize,
    ) -> Option<PathBuf> {
        let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
        let mut visits = 0usize;
        while let Some((dir, depth)) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                visits += 1;
                if visits > max_visits {
                    return None;
                }
                let path = entry.path();
                if path.is_dir() {
                    if depth < max_depth {
                        stack.push((path, depth + 1));
                    }
                } else if path
                    .file_name()
                    .map(|f| f.to_string_lossy().eq_ignore_ascii_case(name))
                    .unwrap_or(false)
                {
                    return Some(path);
                }
            }
        }
        None
    }

    /// Windows 常见安装路径探测 intel-npu-smi.exe：
    /// `EP_INTEL_NPU_SMI_PATH` 覆盖 → System32（NPU 驱动随附）→
    /// `C:\Program Files\Intel\` 有界递归（含 `Program Files (x86)`）。
    /// 结果缓存 5 分钟（含未找到的负结果）。
    pub(super) fn installed_intel_npu_smi() -> Option<String> {
        if let Ok(p) = std::env::var("EP_INTEL_NPU_SMI_PATH") {
            let p = p.trim().to_string();
            if !p.is_empty() {
                return Some(p);
            }
        }
        cached_install_path("intel-npu-smi", || {
            let system32 = Path::new(r"C:\Windows\System32").join("intel-npu-smi.exe");
            if system32.exists() {
                return Some(system32);
            }
            for root in [r"C:\Program Files\Intel", r"C:\Program Files (x86)\Intel"] {
                let root_path = Path::new(root);
                if !root_path.is_dir() {
                    continue;
                }
                if let Some(found) = find_exe_under(root_path, "intel-npu-smi.exe", 6, 20_000) {
                    return Some(found);
                }
            }
            None
        })
        .map(|p| p.to_string_lossy().into_owned())
    }
}

fn npu_index_from_line(line: &str) -> Option<u32> {
    // 优先 "NPU <n>" / "NPU<n>" 形式
    let mut prev_is_npu = false;
    for token in line.split_whitespace() {
        let cleaned = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if prev_is_npu {
            if let Ok(idx) = cleaned.parse::<u32>() {
                return Some(idx);
            }
        }
        let lower = cleaned.to_ascii_lowercase();
        if lower == "npu" {
            prev_is_npu = true;
        } else if let Some(digits) = lower.strip_prefix("npu") {
            if let Ok(idx) = digits.parse::<u32>() {
                return Some(idx);
            }
        }
    }
    // 兜底：行内首个短整数（限 2 位，避免误抓固件版本号）
    for token in line.split_whitespace() {
        let cleaned = token.trim_matches(|c: char| !c.is_ascii_digit());
        if !cleaned.is_empty() && cleaned.len() <= 2 {
            if let Ok(idx) = cleaned.parse::<u32>() {
                return Some(idx);
            }
        }
    }
    None
}

/// 设备名：取首个冒号后的部分；无冒号用整行。逗号/竖线截断附加统计段。
fn npu_name_from_line(line: &str) -> String {
    let rest = match line.split_once(':') {
        Some((_, r)) => r.trim(),
        None => line.trim(),
    };
    let cut = rest.split([',', '|', ';']).next().unwrap_or(rest).trim();
    if cut.is_empty() {
        line.trim().to_string()
    } else {
        cut.to_string()
    }
}

/// "Memory: 2048 MB" 形式提取显存（MiB）
fn mb_amount_from_line(line: &str) -> Option<u32> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    for window in tokens.windows(2) {
        let unit = window[1].to_ascii_lowercase();
        if unit.starts_with("mb") || unit.starts_with("mib") {
            let cleaned = window[0].trim_matches(|c: char| !c.is_ascii_digit());
            if let Ok(v) = cleaned.parse::<u32>() {
                return Some(v);
            }
        }
    }
    None
}

/// "Utilization: 3%" 形式提取利用率
fn pct_from_line(line: &str) -> Option<u8> {
    for token in line.split_whitespace() {
        if let Some(digits) = token.strip_suffix('%') {
            let cleaned = digits.trim_matches(|c: char| !c.is_ascii_digit());
            if let Ok(v) = cleaned.parse::<u8>() {
                return Some(v.min(100));
            }
        }
    }
    None
}

/// 解析 intel-npu-smi 文本输出（格式假设见模块文档，Wave 5 真机校准）。
/// 设备行特征：含 "NPU"+"Intel" 或 "AI Boost"；标题/帮助行（含 smi/usage）跳过。
pub(crate) fn parse_intel_npu_smi(output: &str) -> Vec<ComputeDevice> {
    let mut devices: Vec<ComputeDevice> = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.contains("smi") || lower.contains("usage") || lower.contains("--help") {
            continue;
        }
        let is_device_line =
            lower.contains("ai boost") || (lower.contains("npu") && lower.contains("intel"));
        if !is_device_line {
            continue;
        }

        let index = npu_index_from_line(trimmed).unwrap_or(devices.len() as u32);
        let id = DeviceId::OpenVINO(format!("NPU.{index}"));
        if devices.iter().any(|d| d.id == id) {
            continue;
        }
        devices.push(ComputeDevice {
            id,
            backend: ComputeBackend::OpenVINO,
            name: npu_name_from_line(trimmed),
            total_memory_mb: mb_amount_from_line(trimmed),
            used_memory_mb: None,
            utilization: pct_from_line(trimmed),
            temperature: None,
        });
    }
    devices
}

fn npu_devices() -> Vec<ComputeDevice> {
    // 1) PATH 上的 intel-npu-smi（子命令 "list" 与无参两种调用形态都尝试）
    for args in [&["list"][..], &[][..]] {
        if let Some(output) = run_tool("intel-npu-smi", args, TOOL_TIMEOUT) {
            let devices = parse_intel_npu_smi(&output);
            if !devices.is_empty() {
                return devices;
            }
        }
    }

    // 2) Windows：常见安装路径探测
    #[cfg(windows)]
    {
        if let Some(path) = npu_path_probe::installed_intel_npu_smi() {
            for args in [&["list"][..], &[][..]] {
                if let Some(output) = run_tool(&path, args, TOOL_TIMEOUT) {
                    let devices = parse_intel_npu_smi(&output);
                    if !devices.is_empty() {
                        return devices;
                    }
                }
            }
        }
        let devices = npu_fallback_windows();
        if !devices.is_empty() {
            return devices;
        }
    }

    // 3) Linux 兜底：lspci
    #[cfg(unix)]
    {
        let devices: Vec<ComputeDevice> = lspci_fallback_devices()
            .into_iter()
            .filter(|d| is_npu_id(&d.id))
            .collect();
        if !devices.is_empty() {
            return devices;
        }
    }

    Vec::new()
}

// ─── 探测 3：无 SMI 工具时的兜底信号 ────────────────────────────────────────

/// 设备名是否指向 NPU："AI Boost" 子串，或 "npu" 作为**独立词**。
/// 独立词判定是必要的——子串匹配会把 "USB Input Device"（"In**pu**t"）
/// 误判为 NPU（真机实测踩坑）。
#[cfg(any(windows, test))]
fn name_indicates_npu(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.contains("ai boost") {
        return true;
    }
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| token == "npu")
}

/// 从 PnP/设备名列表中识别 NPU（名称含独立词 NPU 或 AI Boost）
#[cfg(any(windows, test))]
pub(crate) fn parse_npu_presence_names(names: &[String]) -> Vec<ComputeDevice> {
    let mut devices: Vec<ComputeDevice> = Vec::new();
    for raw in names {
        let name = raw.trim();
        if name.is_empty() || !name_indicates_npu(name) {
            continue;
        }
        let index = devices.len() as u32;
        devices.push(make_openvino_device(
            format!("NPU.{index}"),
            name.to_string(),
            None,
            None,
        ));
    }
    devices
}

/// NPU 兜底查询缓存（30s）：daemon 默认 2s 一轮重检测，无 intel-npu-smi 的
/// 机器每轮都会落到 PnP 查询；NPU 在离线状态几乎不变，缓存消除重复子进程。
#[cfg(windows)]
mod npu_presence_cache {
    use std::sync::{LazyLock, Mutex};
    use std::time::{Duration, Instant};

    /// 缓存条目：PnP 名称列表 + 查询时间
    type CacheEntry = (Vec<String>, Instant);

    static CACHE: LazyLock<Mutex<Option<CacheEntry>>> = LazyLock::new(|| Mutex::new(None));
    const TTL: Duration = Duration::from_secs(30);

    pub(super) fn get_or_query(query: impl FnOnce() -> Vec<String>) -> Vec<String> {
        let mut cache = CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((names, queried_at)) = cache.as_ref() {
            if queried_at.elapsed() < TTL {
                return names.clone();
            }
        }
        let names = query();
        *cache = Some((names.clone(), Instant::now()));
        names
    }
}

#[cfg(windows)]
fn npu_fallback_windows() -> Vec<ComputeDevice> {
    // 查询结果缓存 30s（见 npu_presence_cache 文档）；宽查询 + 解析侧精确过滤
    let names = npu_presence_cache::get_or_query(|| {
        if let Some(output) = run_tool(
            "wmic",
            &[
                "path",
                "Win32_PnPEntity",
                "where",
                "Name like '%NPU%' or Name like '%AI Boost%'",
                "get",
                "Name",
                "/format:list",
            ],
            Duration::from_secs(8),
        ) {
            let names = parse_wmic_name_values(&output);
            if !names.is_empty() {
                return names;
            }
        }
        // Win11 24H2 起 wmic 可能被移除 → PowerShell 兜底。
        // `\bNPU\b` 词边界必须：无边界时 "USB Input Device"（In-pu-t）会误匹配（真机实测）
        if let Some(output) = run_tool(
            "powershell",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                r"Get-CimInstance Win32_PnPEntity | Where-Object { $_.Name -match '\bNPU\b|AI Boost' } | Select-Object -ExpandProperty Name",
            ],
            Duration::from_secs(10),
        ) {
            return parse_plain_lines(&output);
        }
        Vec::new()
    });
    parse_npu_presence_names(&names)
}

/// 解析 lspci 输出中的 Intel NPU / iGPU（Linux 兜底）
#[cfg(any(unix, test))]
pub(crate) fn parse_lspci_devices(output: &str) -> Vec<ComputeDevice> {
    let mut devices: Vec<ComputeDevice> = Vec::new();
    let mut npu_seq = 0u32;
    let mut gpu_seq = 0u32;
    for line in output.lines() {
        let lower = line.to_ascii_lowercase();
        // 虚拟化/软件渲染设备：无物理 GPU，OpenVINO 不可用（VM 内 QEMU/VMware
        // 模拟设备、llvmpipe 软件渲染等，真机/VM 实测存在）
        if lower.contains("virtual")
            || lower.contains("llvmpipe")
            || lower.contains("vmware")
            || lower.contains("qxl")
            || lower.contains("bochs")
            || lower.contains("cirrus")
            || lower.contains("virtio gpu")
            || lower.contains("vbox")
        {
            continue;
        }
        // lspci 行形如 "<slot> <class>: <vendor device>"，名称取 ": " 之后
        let name = line
            .split_once(": ")
            .map(|(_, n)| n.trim())
            .filter(|n| !n.is_empty())
            .unwrap_or(line.trim());
        if lower.contains("npu") || lower.contains("ai boost") {
            devices.push(make_openvino_device(
                format!("NPU.{npu_seq}"),
                name.to_string(),
                None,
                None,
            ));
            npu_seq += 1;
        } else if lower.contains("intel")
            && (lower.contains("vga")
                || lower.contains("display")
                || lower.contains("3d controller")
                || lower.contains("graphics"))
        {
            devices.push(make_openvino_device(
                format!("GPU.{gpu_seq}"),
                name.to_string(),
                None,
                None,
            ));
            gpu_seq += 1;
        }
    }
    devices
}

#[cfg(unix)]
fn lspci_fallback_devices() -> Vec<ComputeDevice> {
    let Some(output) = run_tool("lspci", &[], TOOL_TIMEOUT) else {
        return Vec::new();
    };
    parse_lspci_devices(&output)
}

/// iGPU 兜底：仅在 xpu-smi 未发现任何 GPU 时触发
#[cfg(windows)]
fn gpu_fallback_devices() -> Vec<ComputeDevice> {
    let mut devices: Vec<ComputeDevice> = Vec::new();
    for name in super::windows_video_controller_names() {
        // OpenVINO GPU 插件面向 Intel 显卡；NVIDIA/AMD 分别归 CUDA/ROCm 语义
        if !name.to_ascii_lowercase().contains("intel") {
            continue;
        }
        let index = devices.len() as u32;
        devices.push(make_openvino_device(format!("GPU.{index}"), name, None, None));
    }
    devices
}

#[cfg(unix)]
fn gpu_fallback_devices() -> Vec<ComputeDevice> {
    lspci_fallback_devices()
        .into_iter()
        .filter(|d| is_gpu_id(&d.id))
        .collect()
}

#[cfg(not(any(windows, unix)))]
fn gpu_fallback_devices() -> Vec<ComputeDevice> {
    Vec::new()
}

// ─── 探测 4（可选）：OpenVINO Python 运行时权威查询 ─────────────────────────

const PY_PROBE_SCRIPT: &str = r#"import openvino as ov
core = ov.Core()
for d in core.available_devices:
    try:
        name = str(core.get_property(d, "FULL_DEVICE_NAME"))
    except Exception:
        name = ""
    print(d + "\t" + name)
"#;

/// 解析探测脚本输出：`<device kind>\t<FULL_DEVICE_NAME>`；CPU 后端跳过。
pub(crate) fn parse_openvino_python_probe(output: &str) -> Vec<ComputeDevice> {
    let mut devices: Vec<ComputeDevice> = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (kind_raw, name) = match line.split_once('\t') {
            Some((k, n)) => (k.trim(), n.trim()),
            None => (line, ""),
        };
        let kind = kind_raw.to_ascii_uppercase();
        if kind.starts_with("CPU") {
            continue; // CPU 由 CpuDetector 覆盖
        }
        // 归一为 OpenVINO 设备命名（无编号的 GPU/NPU 补 .0）
        let id_suffix = if kind.contains('.') {
            kind.clone()
        } else if kind == "GPU" || kind == "NPU" {
            format!("{kind}.0")
        } else {
            kind.clone()
        };
        let id = DeviceId::OpenVINO(id_suffix.clone());
        if devices.iter().any(|d| d.id == id) {
            continue;
        }
        let name = if name.is_empty() {
            format!("OpenVINO {kind}")
        } else {
            name.to_string()
        };
        devices.push(make_openvino_device(id_suffix, name, None, None));
    }
    devices
}

fn python_probe_devices() -> Vec<ComputeDevice> {
    let enabled = std::env::var("EP_OPENVINO_PYTHON_PROBE")
        .map(|v| v.trim() == "1")
        .unwrap_or(false);
    if !enabled {
        return Vec::new();
    }

    let mut interpreters: Vec<String> = Vec::new();
    if let Ok(p) = std::env::var("EP_OPENVINO_PYTHON") {
        if !p.trim().is_empty() {
            interpreters.push(p.trim().to_string());
        }
    }
    interpreters.push("python".to_string());
    interpreters.push("python3".to_string());

    for interpreter in interpreters {
        if let Some(output) = run_tool(&interpreter, &["-c", PY_PROBE_SCRIPT], PYTHON_PROBE_TIMEOUT)
        {
            let devices = parse_openvino_python_probe(&output);
            if !devices.is_empty() {
                return devices;
            }
        }
    }
    Vec::new()
}

// ─── trait 实现 ─────────────────────────────────────────────────────────────

impl DeviceDetector for OpenVinoDetector {
    fn backend(&self) -> ComputeBackend {
        ComputeBackend::OpenVINO
    }

    fn detect(&self) -> Vec<ComputeDevice> {
        let mut devices: Vec<ComputeDevice> = Vec::new();
        devices.extend(xpu_smi_devices());
        devices.extend(npu_devices());

        // iGPU 兜底仅在 xpu-smi 未发现任何 GPU 时触发（避免重复列出同一硬件）
        if !devices.iter().any(|d| is_gpu_id(&d.id)) {
            devices.extend(gpu_fallback_devices());
        }

        // 可选权威查询（默认关闭）：只追加未知设备
        merge_unique(&mut devices, python_probe_devices());
        devices
    }

    fn refresh(&self, devices: &mut [ComputeDevice]) {
        // best-effort：仅 xpu-smi 可提供刷新数据（gpu_utilization/memory_size）；
        // NPU 无轻量刷新信号源，保持原值
        let fresh = xpu_smi_devices();
        if fresh.is_empty() {
            return;
        }
        for dev in devices.iter_mut() {
            if dev.backend != ComputeBackend::OpenVINO {
                continue;
            }
            if let Some(updated) = fresh.iter().find(|f| f.id == dev.id) {
                dev.utilization = updated.utilization.or(dev.utilization);
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

    /// xpu-smi discovery --dump 真实结构样本（iGPU + dGPU + 应被过滤的 VF）
    const XPU_SMI_FIXTURE: &str = r#"{
    "device_list": [
        {
            "device_id": 0,
            "device_name": "Intel(R) Graphics",
            "device_type": "GPU",
            "drm_index": 0,
            "function_type": "Physical",
            "gpu_utilization": 7,
            "memory_size": 32596,
            "pci_bdf_address": "0000:00:02.0",
            "pci_device_id": "0x7d55",
            "sku": "MTL",
            "uuid": "00000000-0000-0000-0000-000000000000",
            "vendor_name": "Intel(R) Corporation"
        },
        {
            "device_id": 1,
            "device_name": "Intel(R) Arc(TM) A770 Graphics",
            "device_type": "GPU",
            "drm_index": 1,
            "function_type": "Physical",
            "gpu_utilization": 0,
            "memory_size": 16020,
            "pci_bdf_address": "0000:03:00.0",
            "pci_device_id": "0x56a0",
            "sku": "Arc A770",
            "uuid": "00000000-0000-0000-0000-000000000001",
            "vendor_name": "Intel(R) Corporation"
        },
        {
            "device_id": 2,
            "device_name": "Intel(R) Graphics VF",
            "device_type": "GPU",
            "function_type": "VF",
            "memory_size": 1024
        }
    ]
}"#;

    #[test]
    fn test_parse_xpu_smi_discovery() {
        let devices = parse_xpu_smi_discovery_json(XPU_SMI_FIXTURE);
        assert_eq!(devices.len(), 2); // VF 被过滤

        let igpu = &devices[0];
        assert_eq!(igpu.id, DeviceId::OpenVINO("GPU.0".to_string()));
        assert_eq!(igpu.backend, ComputeBackend::OpenVINO);
        assert_eq!(igpu.name, "Intel(R) Graphics");
        assert_eq!(igpu.total_memory_mb, Some(32596));
        assert_eq!(igpu.utilization, Some(7));

        let dgpu = &devices[1];
        assert_eq!(dgpu.id, DeviceId::OpenVINO("GPU.1".to_string()));
        assert_eq!(dgpu.name, "Intel(R) Arc(TM) A770 Graphics");
        assert_eq!(dgpu.total_memory_mb, Some(16020));
    }

    #[test]
    fn test_parse_xpu_smi_empty_and_malformed() {
        assert!(parse_xpu_smi_discovery_json("").is_empty());
        assert!(parse_xpu_smi_discovery_json("garbage").is_empty());
        assert!(parse_xpu_smi_discovery_json("{}").is_empty());
        assert!(parse_xpu_smi_discovery_json(r#"{"device_list": "oops"}"#).is_empty());
        assert!(
            parse_xpu_smi_discovery_json(r#"{"device_list": []}"#).is_empty()
        );
    }

    #[test]
    fn test_parse_xpu_smi_missing_device_id_falls_back_to_seq() {
        let output = r#"{"device_list": [{"device_name": "Intel(R) Graphics"}]}"#;
        let devices = parse_xpu_smi_discovery_json(output);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, DeviceId::OpenVINO("GPU.0".to_string()));
        assert_eq!(devices[0].total_memory_mb, None);
    }

    /// intel-npu-smi 输出样本（结构假设，Wave 5 真机校准；解析器按行容错）
    const INTEL_NPU_SMI_FIXTURE: &str = "\
Intel(R) NPU System Management Interface (intel-npu-smi) v1.0.0
Driver Version: 32.0.100.3104

NPU 0: Intel(R) AI Boost, Memory: 2048 MB, Utilization: 3%
";

    #[test]
    fn test_parse_intel_npu_smi() {
        let devices = parse_intel_npu_smi(INTEL_NPU_SMI_FIXTURE);
        assert_eq!(devices.len(), 1);
        let d = &devices[0];
        assert_eq!(d.id, DeviceId::OpenVINO("NPU.0".to_string()));
        assert_eq!(d.backend, ComputeBackend::OpenVINO);
        assert_eq!(d.name, "Intel(R) AI Boost");
        assert_eq!(d.total_memory_mb, Some(2048));
        assert_eq!(d.utilization, Some(3));
    }

    #[test]
    fn test_parse_intel_npu_smi_title_line_skipped() {
        // 标题行含 "NPU"+"Intel" 但含 "smi" → 跳过；空输出安全
        let devices = parse_intel_npu_smi(
            "Intel(R) NPU System Management Interface (intel-npu-smi) v1.0.0\n",
        );
        assert!(devices.is_empty());
        assert!(parse_intel_npu_smi("").is_empty());
        assert!(parse_intel_npu_smi("no relevant content").is_empty());
    }

    #[test]
    fn test_parse_intel_npu_smi_glued_index_and_plain_name() {
        let devices = parse_intel_npu_smi("NPU1: Intel(R) AI Boost\n");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, DeviceId::OpenVINO("NPU.1".to_string()));
        assert_eq!(devices[0].name, "Intel(R) AI Boost");

        // 无冒号行：整行作名，"ai boost" 触发设备行识别，兜底序号
        let devices = parse_intel_npu_smi("0 Intel(R) AI Boost\n");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, DeviceId::OpenVINO("NPU.0".to_string()));
    }

    #[test]
    fn test_parse_npu_presence_names() {
        let names = vec![
            "Intel(R) AI Boost".to_string(),
            "Intel(R) Management Engine".to_string(), // 不含 NPU/AI Boost → 过滤
            "Intel(R) NPU".to_string(),
        ];
        let devices = parse_npu_presence_names(&names);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].id, DeviceId::OpenVINO("NPU.0".to_string()));
        assert_eq!(devices[0].name, "Intel(R) AI Boost");
        assert_eq!(devices[1].id, DeviceId::OpenVINO("NPU.1".to_string()));
        assert!(parse_npu_presence_names(&[]).is_empty());
    }

    #[test]
    fn test_parse_npu_presence_rejects_substring_false_positives() {
        // 真机踩坑：PowerShell -match 大小写不敏感，"USB Input Device" 的
        // "In-pu-t" 含 "npu" 子串 → 必须按独立词判定排除
        let names = vec![
            "USB Input Device".to_string(),
            "Input Device".to_string(),
            "Intel(R) AI Boost".to_string(),
        ];
        let devices = parse_npu_presence_names(&names);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "Intel(R) AI Boost");
    }

    #[test]
    fn test_parse_lspci_devices() {
        let output = "\
00:02.0 VGA compatible controller: Intel Corporation Meteor Lake-P Integrated Graphics
00:0b.0 Processing accelerators: Intel Corporation Meteor Lake NPU
01:00.0 VGA compatible controller: NVIDIA Corporation AD102 [GeForce RTX 4090]
";
        let devices = parse_lspci_devices(output);
        assert_eq!(devices.len(), 2); // NVIDIA 行不属于 OpenVINO
        assert_eq!(devices[0].id, DeviceId::OpenVINO("GPU.0".to_string()));
        assert_eq!(
            devices[0].name,
            "Intel Corporation Meteor Lake-P Integrated Graphics"
        );
        assert_eq!(devices[1].id, DeviceId::OpenVINO("NPU.0".to_string()));
        assert_eq!(devices[1].name, "Intel Corporation Meteor Lake NPU");
    }

    #[test]
    fn test_parse_lspci_skips_virtual_and_software_renderers() {
        // 虚拟化/软件渲染设备不得被当作 OpenVINO GPU（VM 内 QEMU/VMware 模拟
        // Intel 显示设备、llvmpipe 软件渲染等）
        let output = "\
00:02.0 VGA compatible controller: Intel Corporation QEMU Virtual GPU
00:05.0 VGA compatible controller: VMware SVGA II Adapter
00:06.0 VGA compatible controller: llvmpipe (LLVM 18.1.8, 256 bits)
00:07.0 VGA compatible controller: Intel Corporation UHD Graphics 770
";
        let devices = parse_lspci_devices(output);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "Intel Corporation UHD Graphics 770");
    }

    #[test]
    fn test_parse_openvino_python_probe() {
        let output = "GPU\tIntel(R) Graphics\nNPU\tIntel(R) AI Boost\nCPU\tIntel(R) Core(TM) Ultra 9 185H\nGPU.1\tIntel(R) Arc(TM) A770\n";
        let devices = parse_openvino_python_probe(output);
        assert_eq!(devices.len(), 3); // CPU 被跳过
        assert_eq!(devices[0].id, DeviceId::OpenVINO("GPU.0".to_string()));
        assert_eq!(devices[0].name, "Intel(R) Graphics");
        assert_eq!(devices[1].id, DeviceId::OpenVINO("NPU.0".to_string()));
        assert_eq!(devices[2].id, DeviceId::OpenVINO("GPU.1".to_string()));
    }

    #[test]
    fn test_parse_openvino_python_probe_malformed() {
        assert!(parse_openvino_python_probe("").is_empty());
        // 无制表符：整行作 kind
        let devices = parse_openvino_python_probe("NPU\n");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, DeviceId::OpenVINO("NPU.0".to_string()));
        assert_eq!(devices[0].name, "OpenVINO NPU");
    }

    #[test]
    fn test_merge_unique() {
        let mut existing = vec![make_openvino_device(
            "GPU.0".to_string(),
            "Intel(R) Graphics".to_string(),
            None,
            None,
        )];
        merge_unique(
            &mut existing,
            vec![
                make_openvino_device("GPU.0".to_string(), "Duplicate".to_string(), None, None),
                make_openvino_device("NPU.0".to_string(), "Intel(R) AI Boost".to_string(), None, None),
            ],
        );
        assert_eq!(existing.len(), 2);
        assert_eq!(existing[0].name, "Intel(R) Graphics"); // 先入为主，不被覆盖
        assert_eq!(existing[1].id, DeviceId::OpenVINO("NPU.0".to_string()));
    }

    #[cfg(windows)]
    #[test]
    fn test_find_exe_under_bounds() {
        use std::path::Path;
        // 不存在的根目录 → None（不 panic）
        assert!(
            npu_path_probe::find_exe_under(
                Path::new(r"C:\dir-that-does-not-exist-xyz"),
                "intel-npu-smi.exe",
                6,
                100
            )
            .is_none()
        );
    }
}
