//! 跨栈物理设备归并（显示层语义，纯函数）
//!
//! 同一物理适配器常被多个计算栈覆盖（如 RX 7900 XTX 同时出现在 ROCm 与
//! Vulkan/RADV 视图中），而各栈命名口径不一：
//! - ROCm/rocm-smi：`AMD Radeon RX 7900 XTX`（旧版缺字段时回退 `AMD GPU 0`）
//! - OpenVINO：PCI DB 名 `Arrow Lake-S [Intel Graphics]`
//! - Vulkan/RADV：`AMD Radeon RX 7900 XTX (RADV NAVI31)` / `Intel(R) Graphics (ARL)`
//!
//! 早期的归一化名精确去重无法对齐这些变体，导致仪表板同一物理卡出现两条目
//! （私有栈一条 + Vulkan 一条）。本模块在全部检测器产物之上做二次归并：
//! 同物理设备聚为一组，组内主成员保持最高优先级后端（调度语义不变——
//! `state.devices` 的逐栈条目仍由调度器原样消费），仅 API 显示层折叠。
//!
//! 匹配规则（保守优先，宁可不并不误并）：
//! 1. 排除项：`Cpu` 后端永不参与；名字含 `npu` 的设备（NPU 是独立算力单元，
//!    不是 GPU 的另一视图）永不参与；同后端设备互不吸收（双卡同型安全）
//! 2. 厂商类别须一致且非 Unknown（nvidia/geforce→NVIDIA；amd/radeon/ati→AMD；
//!    intel→INTEL；其余一律视为 Unknown 不参与跨栈归并）
//! 3. 核心名匹配：归一化 + 剥离括号组（`(RADV NAVI31)` / `[Intel Graphics]` /
//!    `(ARL)` 等）后，两核心名相等 **或** 词集互相包含（处理 OpenVINO 的
//!    PCI DB 长名 vs Vulkan 短名，如 `arrow lake-s intel graphics` ⊇
//!    `intel graphics`）
//! 4. 吸收约束：每个组对同一后端至多吸收一个成员——机器装两张同型卡时，
//!    第二张不会误并入已含该后端的组

use crate::types::{ComputeBackend, ComputeDevice};

/// 一个物理设备的归并结果：`primary` 为主成员下标（输入序即后端优先级序，
/// 首个命中者为主），`members` 含全部成员下标（升序）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalGroup {
    pub primary: usize,
    pub members: Vec<usize>,
}

/// 设备是否参与跨栈归并（CPU 与 NPU 是独立算力单元，不是 GPU 的别名视图）
fn merge_eligible(d: &ComputeDevice) -> bool {
    if d.backend == ComputeBackend::Cpu {
        return false;
    }
    let lower = d.name.to_ascii_lowercase();
    !lower.split_whitespace().any(|t| t == "npu")
}

/// 剥离名称中的圆括号组片段：`(RADV NAVI31)` / `(ARL)` / `(R)`。
/// 注意只剥圆括号——OpenVINO 的 PCI DB 名用**方括号承载有效家族信息**
/// （`Arrow Lake-S [Intel Graphics]`），剥掉会丢失跨栈匹配锚点；
/// 方括号仅去字符、保留内容，使词集可跨栈互含比对。
fn strip_bracket_groups(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut depth = 0usize;
    for ch in name.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            '[' | ']' | '{' | '}' => {} // 去字符留内容
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// 核心词集：剥离括号组 + 复用归一化（小写/(r)(tm) 清理/空白折叠）+ 分词
fn core_tokens(name: &str) -> Vec<String> {
    super::normalize_device_name(&strip_bracket_groups(name))
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// 厂商类别（None = 无法判定，不参与跨栈归并）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vendor {
    Nvidia,
    Amd,
    Intel,
}

fn vendor_class(tokens: &[String]) -> Option<Vendor> {
    let hit = |needles: &[&str]| {
        tokens
            .iter()
            .any(|t| needles.iter().any(|n| t.contains(n)))
    };
    if hit(&["nvidia", "geforce", "quadro", "titan"]) {
        Some(Vendor::Nvidia)
    } else if hit(&["amd", "radeon", "ati"]) {
        Some(Vendor::Amd)
    } else if hit(&["intel"]) {
        Some(Vendor::Intel)
    } else {
        None
    }
}

/// 核心词集是否指向同一设备：相等或词集互相包含（小词集须非空）
fn names_match(a: &[String], b: &[String]) -> bool {
    if a == b {
        return true;
    }
    let (small, big) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    !small.is_empty() && small.iter().all(|t| big.contains(t))
}

/// 是否为检测器的通用兜底名（如 rocm-smi 缺产品名时的 `AMD GPU 0`）。
/// 此类名字不含任何型号信息，跨栈比对时降级为「厂商一致即视为同一卡」——
/// 由调用方已保证厂商类别相等。多张同型卡场景由每后端至多吸收一个成员的
/// 约束兜底（见模块文档规则 4）。
fn is_generic_fallback_name(tokens: &[String]) -> bool {
    tokens.len() == 3
        && tokens[0] == "amd"
        && tokens[1] == "gpu"
        && tokens[2].chars().all(|c| c.is_ascii_digit())
        && !tokens[2].is_empty()
}

/// 跨栈名称匹配总判：词集互含，或任一方为通用兜底名
fn cross_stack_names_match(a: &[String], b: &[String]) -> bool {
    names_match(a, b) || is_generic_fallback_name(a) || is_generic_fallback_name(b)
}

/// 跨栈物理设备归并主入口。
///
/// 输入为检测器优先级序（`detect_all_devices` 产物）；输出组的顺序与各组
/// 主成员的出现顺序一致。独立设备各自成组，行为退化为恒等映射。
pub fn group_physical_devices(devices: &[ComputeDevice]) -> Vec<PhysicalGroup> {
    let mut groups: Vec<PhysicalGroup> = Vec::new();
    for (i, dev) in devices.iter().enumerate() {
        if !merge_eligible(dev) {
            groups.push(PhysicalGroup {
                primary: i,
                members: vec![i],
            });
            continue;
        }
        let tokens = core_tokens(&dev.name);
        let vendor = vendor_class(&tokens);
        let mut placed = false;
        for g in groups.iter_mut() {
            let host = &devices[g.primary];
            // 宿主必须同样可参与归并（Cpu/NPU 主成员不接受新成员）
            if !merge_eligible(host) {
                continue;
            }
            // 每组每后端至多一个成员（双卡同型防误并）
            if g.members
                .iter()
                .any(|&m| devices[m].backend == dev.backend)
            {
                continue;
            }
            let host_tokens = core_tokens(&host.name);
            if vendor_class(&host_tokens) != vendor || vendor.is_none() {
                continue;
            }
            if !cross_stack_names_match(&host_tokens, &tokens) {
                continue;
            }
            g.members.push(i);
            placed = true;
            break;
        }
        if !placed {
            groups.push(PhysicalGroup {
                primary: i,
                members: vec![i],
            });
        }
    }
    groups
}

/// 组的展示名：取括号剥离后词数最多的成员名（最具描述性），平局取先出现者
/// （即更高优先级后端的名字）。修复「7900 XTX 显示成 AMD GPU 0」一类通用名
/// 覆盖专有名的问题。
pub fn display_name<'a>(devices: &'a [ComputeDevice], group: &PhysicalGroup) -> &'a str {
    let mut best: Option<(usize, usize)> = None; // (token_count, member_idx)
    for &m in &group.members {
        let count = core_tokens(&devices[m].name).len();
        match best {
            Some((bc, _)) if bc >= count => {}
            _ => best = Some((count, m)),
        }
    }
    let idx = best.map(|(_, m)| m).unwrap_or(group.primary);
    &devices[idx].name
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DeviceId;

    fn dev(id: DeviceId, backend: ComputeBackend, name: &str) -> ComputeDevice {
        ComputeDevice {
            id,
            backend,
            name: name.to_string(),
            total_memory_mb: None,
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        }
    }

    /// 真机（Arrow Lake 平台三卡）检测产物的精确复刻
    fn real_machine() -> Vec<ComputeDevice> {
        vec![
            dev(
                DeviceId::Cuda(0),
                ComputeBackend::Cuda,
                "NVIDIA GeForce RTX 5090 D",
            ),
            dev(DeviceId::Rocm(0), ComputeBackend::Rocm, "AMD GPU 0"),
            dev(
                DeviceId::OpenVINO("NPU.0".into()),
                ComputeBackend::OpenVINO,
                "Core Ultra 200 Series Processors NPU",
            ),
            dev(
                DeviceId::OpenVINO("GPU.0".into()),
                ComputeBackend::OpenVINO,
                "Arrow Lake-S [Intel Graphics]",
            ),
            dev(
                DeviceId::Vulkan(0),
                ComputeBackend::Vulkan,
                "Intel(R) Graphics (ARL)",
            ),
            dev(
                DeviceId::Vulkan(1),
                ComputeBackend::Vulkan,
                "NVIDIA GeForce RTX 5090 D",
            ),
            dev(
                DeviceId::Vulkan(2),
                ComputeBackend::Vulkan,
                "AMD Radeon RX 7900 XTX (RADV NAVI31)",
            ),
            dev(
                DeviceId::Cpu,
                ComputeBackend::Cpu,
                "Intel(R) Core(TM) Ultra 7 270K Plus",
            ),
        ]
    }

    #[test]
    fn test_real_machine_merges_igpu_and_dgpu_across_stacks() {
        let devices = real_machine();
        let groups = group_physical_devices(&devices);
        // cuda+NVIDIA 合一、rocm+vulkan 合一、ov-iGPU+vulkan 合一、NPU/CPU 独立
        assert_eq!(groups.len(), 5);

        let amd = &groups[1];
        assert_eq!(amd.primary, 1, "ROCm 为组主成员");
        assert_eq!(amd.members, vec![1, 6], "vulkan AMD 卡并入");
        assert_eq!(
            display_name(&devices, amd),
            "AMD Radeon RX 7900 XTX (RADV NAVI31)",
            "展示名取最具描述性者，而非 rocm-smi 兜底名"
        );

        let igpu = &groups[3];
        assert_eq!(igpu.primary, 3);
        assert_eq!(igpu.members, vec![3, 4]);
        assert_eq!(display_name(&devices, igpu), "Arrow Lake-S [Intel Graphics]");

        let nv = &groups[0];
        // 真机上 vulkan 的同名 NVIDIA 已在检测期被精确去重，不会到达此处；
        // 若因名字变体漏网（如驱动版本尾注），本归并层仍能兜底合一
        assert_eq!(nv.members, vec![0, 5], "CUDA 与 Vulkan 视图兜底合一");

        assert_eq!(groups[2].members, vec![2], "NPU 永不参与归并");
        assert_eq!(groups[4].members, vec![7], "Cpu 永不参与归并");
    }

    /// rocm-smi 新版键名（Card Series 大写 S）修复后的形态：专有名直接命中
    #[test]
    fn test_rocm_proper_name_binds_vulkan_alias_and_prefers_primary_on_tie() {
        let devices = vec![
            dev(
                DeviceId::Rocm(0),
                ComputeBackend::Rocm,
                "AMD Radeon RX 7900 XTX",
            ),
            dev(
                DeviceId::Vulkan(0),
                ComputeBackend::Vulkan,
                "AMD Radeon RX 7900 XTX (RADV NAVI31)",
            ),
        ];
        let groups = group_physical_devices(&devices);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members, vec![0, 1]);
        // 词数平局（5:5）→ 保持主成员（更高优先级后端）的名字
        assert_eq!(display_name(&devices, &groups[0]), "AMD Radeon RX 7900 XTX");
    }

    #[test]
    fn test_dual_same_model_gpus_not_merged_into_one_backend_slot() {
        // 两张同型卡：第二张 vulkan 卡不得并入已含 vulkan 成员的组
        let devices = vec![
            dev(DeviceId::Rocm(0), ComputeBackend::Rocm, "AMD Radeon RX 7900 XTX"),
            dev(DeviceId::Rocm(1), ComputeBackend::Rocm, "AMD Radeon RX 7900 XTX"),
            dev(
                DeviceId::Vulkan(0),
                ComputeBackend::Vulkan,
                "AMD Radeon RX 7900 XTX (RADV NAVI31)",
            ),
            dev(
                DeviceId::Vulkan(1),
                ComputeBackend::Vulkan,
                "AMD Radeon RX 7900 XTX (RADV NAVI31)",
            ),
        ];
        let groups = group_physical_devices(&devices);
        // 语义：vk0 并入首个 AMD 组（rocm0），vk1 因该组已有 vulkan 成员被拒，
        // 继续向后匹配到 rocm1 组（尚无 vulkan 成员）→ 正确并入，跨组不错配
        assert_eq!(groups.len(), 2);
        let mut merged: Vec<Vec<usize>> = groups.iter().map(|g| g.members.clone()).collect();
        merged.sort();
        assert_eq!(
            merged,
            vec![vec![0, 2], vec![1, 3]],
            "两组各吸收一张卡，跨组不错配"
        );
    }

    #[test]
    fn test_vendor_mismatch_never_merges() {
        let devices = vec![
            dev(DeviceId::Rocm(0), ComputeBackend::Rocm, "AMD Radeon RX 7900 XTX"),
            dev(
                DeviceId::Vulkan(0),
                ComputeBackend::Vulkan,
                "Intel(R) Graphics (ARL)",
            ),
        ];
        let groups = group_physical_devices(&devices);
        assert_eq!(groups.len(), 2, "厂商不同不并");
    }

    #[test]
    fn test_unknown_vendor_names_left_alone() {
        let devices = vec![
            dev(DeviceId::DirectML(0), ComputeBackend::DirectML, "虚拟适配器"),
            dev(DeviceId::Vulkan(0), ComputeBackend::Vulkan, "未知渲染器"),
        ];
        assert_eq!(group_physical_devices(&devices).len(), 2, "厂商不可判定的名字不参与跨栈归并");
    }

    #[test]
    fn test_bracket_strip_and_token_subset_units() {
        // 圆括号连内容剥除；方括号去字符留内容（OpenVINO 家族名是匹配锚点）
        assert_eq!(strip_bracket_groups("Intel(R) Graphics (ARL)"), "Intel Graphics ");
        assert_eq!(
            strip_bracket_groups("Arrow Lake-S [Intel Graphics]"),
            "Arrow Lake-S Intel Graphics"
        );
        let a = core_tokens("Arrow Lake-S [Intel Graphics]");
        let b = core_tokens("Intel(R) Graphics (ARL)");
        assert_eq!(a, vec!["arrow", "lake-s", "intel", "graphics"]);
        assert_eq!(b, vec!["intel", "graphics"]);
        assert!(names_match(&a, &b), "PCI DB 长名 ⊇ Vulkan 短名");
    }

    #[test]
    fn test_generic_fallback_name_matches_by_vendor_only() {
        assert!(is_generic_fallback_name(&core_tokens("AMD GPU 0")));
        assert!(is_generic_fallback_name(&["amd".into(), "gpu".into(), "12".into()]));
        assert!(!is_generic_fallback_name(&core_tokens(
            "AMD Radeon RX 7900 XTX"
        )));
        assert!(!is_generic_fallback_name(&core_tokens("AMD GPU")));
        // 厂商级通配：兜底名与任何同厂商核心名互认
        let generic = core_tokens("AMD GPU 0");
        let real = core_tokens("AMD Radeon RX 7900 XTX (RADV NAVI31)");
        assert!(cross_stack_names_match(&generic, &real));
        // 但不同型号专有名之间仍须词集互含，不得借通配误并
        let other = core_tokens("AMD Radeon RX 7800 XT (RADV NAVI32)");
        assert!(!cross_stack_names_match(&real, &other));
    }
}
