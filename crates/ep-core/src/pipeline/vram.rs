//! 管线 VRAM 预算（§6.3 编辑器实时计算）+ 管线 TOML 并发限额读取（§6.8）
//!
//! ## VRAM 预算算法（§6.3）
//!
//! 1. 对 DAG 拓扑分层（同层节点可并行 → 同层 VRAM 叠加，跨层取峰值）；
//! 2. 每层对绑定到具体设备（`device = "cuda:0"` 等）的节点按设备求和；
//!    `device = "auto"`（或缺省）的节点汇入 **unassigned 池**（由调度器按
//!    least_memory 落位），同样按层求峰值；
//! 3. 每设备峰值即该管线的预算需求 `pipeline_mb`，叠加设备当前占用与容量
//!    得出 `over` 标记；`allow_overcommit` 由调用方传入（执行放行策略）。
//!
//! 数据源约定（A6）：节点 vram 值来自
//! [`crate::module::manifest::ModuleManifest::resolve_vram_estimate`]
//! （pin 变体级优先、模块级兜底）；估算缺失的节点不参与求和
//! （消费侧可提示"未知 VRAM，账本未计入"）。
//!
//! 本模块只做**纯计算**：设备容量、节点估算、设备绑定均由调用方
//! （daemon `POST /api/pipelines/vram-budget` / 桌面端 ep-core 直连）准备好，
//! 便于 fixture 测试与跨端复用。
//!
//! ## `max_instances`（§6.8）
//!
//! 管线级并发上限声明在管线 TOML 的 `[pipeline]` 段
//! （缺省跟随全局 `pipeline.max_parallel`）。引擎的 `Pipeline` 结构尚未携带
//! 该字段（dag.rs 所有权归 Wave 2 B7），故此处提供基于原始 TOML 文本的
//! 读取器，执行层按 `pipeline.id` 匹配文件后调用。

use serde::{Deserialize, Serialize};

// ─── 输入 / 输出类型 ─────────────────────────────────────────────────────────

/// 节点 VRAM 描述（调用方解析管线 + manifest 后构造）
#[derive(Debug, Clone, PartialEq)]
pub struct VramNodeEstimate {
    pub node_id: String,
    /// 设备绑定：`"auto"` / 空串 = 未分配池；`"cuda:0"` 等 = 具体设备
    pub device: String,
    /// VRAM 估算（MB）；None = 未知（不参与求和）
    pub vram_mb: Option<u64>,
}

/// 设备容量快照（来自 `/api/devices` 等）
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceCapacity {
    /// 设备标识（如 `"cuda:0"`）
    pub device_id: String,
    pub total_mb: Option<u64>,
    pub used_mb: Option<u64>,
}

/// 预算条目：单个节点在某设备（或未分配池）的 VRAM 需求
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VramItem {
    pub node_id: String,
    pub mb: u64,
}

/// 每设备预算条目（§6.3 每设备账本）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceBudget {
    pub device_id: String,
    pub total_mb: Option<u64>,
    pub used_mb: Option<u64>,
    /// 该管线在此设备的峰值 VRAM 需求（跨层取峰值）
    pub pipeline_mb: u64,
    /// 峰值层的节点明细
    pub items: Vec<VramItem>,
    /// 是否超出预算（used + pipeline > total；容量未知则 false）
    pub over: bool,
}

/// VRAM 预算报告（S2 前端形状，仲裁 #3）：
/// `{devices:[{device_id,total_mb,used_mb,pipeline_mb,items:[{node_id,mb}]}], unassigned:[...]}`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VramBudgetReport {
    pub devices: Vec<DeviceBudget>,
    /// device=auto 未分配池峰值层的节点明细
    pub unassigned: Vec<VramItem>,
    /// 未分配池峰值（MB）
    pub unassigned_mb: u64,
    /// 是否允许超额提交（`compute.allow_overcommit`，放行策略由执行层决定）
    pub allow_overcommit: bool,
}

/// 预算计算错误
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VramBudgetError {
    #[error("pipeline contains a cycle; VRAM budget cannot be computed")]
    CycleDetected,
}

// ─── 拓扑分层 ────────────────────────────────────────────────────────────────

/// 按边（from, to 节点 id）对节点做 Kahn 拓扑分层；有环返回错误。
///
/// 与 [`super::dag::Pipeline::topological_layers`] 同算法，但输入只需
/// 节点 id + 节点间边，便于 spec JSON / 测试 fixture 直接使用。
pub fn topological_layers(
    node_ids: &[String],
    edges: &[(String, String)],
) -> Result<Vec<Vec<String>>, VramBudgetError> {
    use std::collections::{HashMap, HashSet, VecDeque};

    let node_set: HashSet<&str> = node_ids.iter().map(|s| s.as_str()).collect();
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
    for id in node_ids {
        in_degree.insert(id.as_str(), 0);
        adjacency.insert(id.as_str(), Vec::new());
    }
    for (from, to) in edges {
        // 引用未知节点的边不影响分层（结构合法性由 dag validate 管辖）
        if node_set.contains(from.as_str()) && node_set.contains(to.as_str()) {
            adjacency.get_mut(from.as_str()).unwrap().push(to.as_str());
            *in_degree.get_mut(to.as_str()).unwrap() += 1;
        }
    }

    let mut layers = Vec::new();
    let mut queue: VecDeque<&str> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&id, _)| id)
        .collect();
    let mut processed = 0usize;

    while !queue.is_empty() {
        let layer: Vec<String> = queue.drain(..).map(|s| s.to_string()).collect();
        let mut next: VecDeque<&str> = VecDeque::new();
        for node_id in &layer {
            processed += 1;
            for &downstream in adjacency.get(node_id.as_str()).into_iter().flatten() {
                let deg = in_degree.get_mut(downstream).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    next.push_back(downstream);
                }
            }
        }
        layers.push(layer);
        queue = next;
    }

    if processed < node_ids.len() {
        Err(VramBudgetError::CycleDetected)
    } else {
        Ok(layers)
    }
}

// ─── 预算计算 ────────────────────────────────────────────────────────────────

/// 计算管线 VRAM 预算（§6.3 算法）。
///
/// - `nodes`：各节点的估算值与设备绑定；
/// - `edges`：节点间依赖（from → to），用于拓扑分层；
/// - `devices`：设备容量快照（输出会包含全部给定设备，即使管线需求为 0）；
/// - `allow_overcommit`：原样带入报告。
///
/// 输出设备顺序：先按 `devices` 给定顺序，再补节点引用但容量未知的设备
/// （按首次出现序）。
pub fn compute_budget(
    nodes: &[VramNodeEstimate],
    edges: &[(String, String)],
    devices: &[DeviceCapacity],
    allow_overcommit: bool,
) -> Result<VramBudgetReport, VramBudgetError> {
    use std::collections::HashMap;

    let node_ids: Vec<String> = nodes.iter().map(|n| n.node_id.clone()).collect();
    let layers = topological_layers(&node_ids, edges)?;
    let by_id: HashMap<&str, &VramNodeEstimate> =
        nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();

    // 每设备：峰值 + 峰值层明细；未分配池同款
    let mut peak: HashMap<String, (u64, Vec<VramItem>)> = HashMap::new();
    let mut unassigned_peak: (u64, Vec<VramItem>) = (0, Vec::new());

    for layer in &layers {
        // 本层按设备累加
        let mut layer_sum: HashMap<String, (u64, Vec<VramItem>)> = HashMap::new();
        let mut layer_auto: (u64, Vec<VramItem>) = (0, Vec::new());
        for node_id in layer {
            let Some(est) = by_id.get(node_id.as_str()) else {
                continue;
            };
            let Some(mb) = est.vram_mb else {
                continue; // 估算未知 → 不计入账本
            };
            let device = est.device.trim().to_ascii_lowercase();
            if device.is_empty() || device == "auto" {
                layer_auto.0 += mb;
                layer_auto.1.push(VramItem {
                    node_id: node_id.clone(),
                    mb,
                });
            } else {
                let entry = layer_sum.entry(device).or_insert((0, Vec::new()));
                entry.0 += mb;
                entry.1.push(VramItem {
                    node_id: node_id.clone(),
                    mb,
                });
            }
        }
        // 跨层取峰值
        for (device, sum) in layer_sum {
            let entry = peak.entry(device).or_insert((0, Vec::new()));
            if sum.0 > entry.0 {
                *entry = sum;
            }
        }
        if layer_auto.0 > unassigned_peak.0 {
            unassigned_peak = layer_auto;
        }
    }

    // 组装设备账本：容量给定顺序优先，节点引用的未知设备补在后面
    let mut report_devices: Vec<DeviceBudget> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for cap in devices {
        let key = cap.device_id.trim().to_ascii_lowercase();
        seen.insert(key.clone());
        let (pipeline_mb, items) = peak.remove(&key).unwrap_or((0, Vec::new()));
        let over = match (cap.total_mb, cap.used_mb) {
            (Some(total), used) => {
                let used = used.unwrap_or(0);
                used.saturating_add(pipeline_mb) > total
            }
            _ => false,
        };
        report_devices.push(DeviceBudget {
            device_id: cap.device_id.clone(),
            total_mb: cap.total_mb,
            used_mb: cap.used_mb,
            pipeline_mb,
            items,
            over,
        });
    }
    // 节点引用但容量快照里没有的设备（如绑定了一个离线设备）
    let mut extra: Vec<(String, (u64, Vec<VramItem>))> = peak.into_iter().collect();
    extra.sort_by(|a, b| a.0.cmp(&b.0));
    for (device, (pipeline_mb, items)) in extra {
        if seen.contains(&device) {
            continue;
        }
        report_devices.push(DeviceBudget {
            device_id: device,
            total_mb: None,
            used_mb: None,
            pipeline_mb,
            items,
            over: false,
        });
    }

    Ok(VramBudgetReport {
        devices: report_devices,
        unassigned: unassigned_peak.1,
        unassigned_mb: unassigned_peak.0,
        allow_overcommit,
    })
}

// ─── 管线 TOML 辅助读取（§6.8） ──────────────────────────────────────────────

/// 从管线 TOML 文本读取 `[pipeline] max_instances`（§6.8 管线级并发上限）。
///
/// 返回 `None` = 未声明（跟随全局 `pipeline.max_parallel`）或解析失败
/// （执行层按缺省处理，不因限额读取失败阻塞提交）。
pub fn parse_max_instances(toml_text: &str) -> Option<u32> {
    let value: toml::Value = toml::from_str(toml_text).ok()?;
    value
        .get("pipeline")
        .and_then(|p| p.get("max_instances"))
        .and_then(|v| v.as_integer())
        .and_then(|v| u32::try_from(v).ok())
        .filter(|&v| v > 0)
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, device: &str, mb: Option<u64>) -> VramNodeEstimate {
        VramNodeEstimate {
            node_id: id.to_string(),
            device: device.to_string(),
            vram_mb: mb,
        }
    }

    fn device(id: &str, total: Option<u64>, used: Option<u64>) -> DeviceCapacity {
        DeviceCapacity {
            device_id: id.to_string(),
            total_mb: total,
            used_mb: used,
        }
    }

    /// 线性边 a → b
    fn edge(from: &str, to: &str) -> (String, String) {
        (from.to_string(), to.to_string())
    }

    // ── 分层：同层并行节点叠加，跨层取峰值 ──────────────────────────────────

    #[test]
    fn peak_across_layers_single_device() {
        // layer0: in(无vram) → layer1: a(3000)+b(2000) 并行 → layer2: c(4000)
        let nodes = vec![
            node("in", "auto", None),
            node("a", "cuda:0", Some(3000)),
            node("b", "cuda:0", Some(2000)),
            node("c", "cuda:0", Some(4000)),
        ];
        let edges = vec![
            edge("in", "a"),
            edge("in", "b"),
            edge("a", "c"),
            edge("b", "c"),
        ];
        let report = compute_budget(
            &nodes,
            &edges,
            &[device("cuda:0", Some(24576), Some(0))],
            true,
        )
        .unwrap();

        assert_eq!(report.devices.len(), 1);
        let d = &report.devices[0];
        assert_eq!(d.device_id, "cuda:0");
        // 峰值 = layer1 的 3000+2000 = 5000（> layer2 的 4000）
        assert_eq!(d.pipeline_mb, 5000);
        assert_eq!(d.items.len(), 2);
        assert!(d.items.iter().any(|i| i.node_id == "a" && i.mb == 3000));
        assert!(d.items.iter().any(|i| i.node_id == "b" && i.mb == 2000));
        assert!(!d.over);
        assert!(report.unassigned.is_empty());
        assert_eq!(report.unassigned_mb, 0);
    }

    // ── 多设备 + auto 未分配池 ───────────────────────────────────────────────

    #[test]
    fn multi_device_and_unassigned_pool() {
        // 同层三节点：cuda:0 绑定 ×2 + auto ×1
        let nodes = vec![
            node("asr", "cuda:0", Some(8192)),
            node("llm", "cuda:1", Some(4096)),
            node("tts", "auto", Some(2048)),
        ];
        let edges = vec![];
        let report = compute_budget(
            &nodes,
            &edges,
            &[
                device("cuda:0", Some(24576), Some(1024)),
                device("cuda:1", Some(8192), Some(6000)),
            ],
            false,
        )
        .unwrap();

        let d0 = report.devices.iter().find(|d| d.device_id == "cuda:0").unwrap();
        assert_eq!(d0.pipeline_mb, 8192);
        assert_eq!(d0.items, vec![VramItem { node_id: "asr".into(), mb: 8192 }]);
        assert!(!d0.over, "1024+8192 < 24576");

        let d1 = report.devices.iter().find(|d| d.device_id == "cuda:1").unwrap();
        assert_eq!(d1.pipeline_mb, 4096);
        assert!(d1.over, "6000+4096 > 8192 → 超预算");

        assert_eq!(report.unassigned_mb, 2048);
        assert_eq!(
            report.unassigned,
            vec![VramItem { node_id: "tts".into(), mb: 2048 }]
        );
        assert!(!report.allow_overcommit);
    }

    // ── 未分配池也取层峰值 ──────────────────────────────────────────────────

    #[test]
    fn unassigned_peak_across_layers() {
        // layer0: a(auto 1000) → layer1: b(auto 300)+c(auto 400)
        let nodes = vec![
            node("a", "auto", Some(1000)),
            node("b", "auto", Some(300)),
            node("c", "auto", Some(400)),
        ];
        let edges = vec![edge("a", "b"), edge("a", "c")];
        let report = compute_budget(&nodes, &edges, &[], true).unwrap();
        // 峰值 = layer0 的 1000（> layer1 的 700）
        assert_eq!(report.unassigned_mb, 1000);
        assert_eq!(report.unassigned.len(), 1);
        assert_eq!(report.unassigned[0].node_id, "a");
        assert!(report.devices.is_empty());
    }

    // ── 估算未知节点不入账；容量全给定时零需求设备也出现 ───────────────────

    #[test]
    fn unknown_estimates_skipped_and_zero_devices_listed() {
        let nodes = vec![node("mystery", "cuda:0", None), node("known", "cuda:0", Some(512))];
        let report = compute_budget(
            &nodes,
            &[],
            &[device("cuda:0", Some(8192), None), device("cpu", None, None)],
            true,
        )
        .unwrap();
        assert_eq!(report.devices.len(), 2, "容量快照中的设备全部列出");
        let d0 = report.devices.iter().find(|d| d.device_id == "cuda:0").unwrap();
        assert_eq!(d0.pipeline_mb, 512, "未知估算不参与求和");
        assert_eq!(d0.items.len(), 1);
        let cpu = report.devices.iter().find(|d| d.device_id == "cpu").unwrap();
        assert_eq!(cpu.pipeline_mb, 0);
        assert!(cpu.items.is_empty());
    }

    // ── 节点绑定容量快照外的设备 → 补充条目（容量未知） ────────────────────

    #[test]
    fn device_not_in_capacity_snapshot_is_added() {
        let nodes = vec![node("rocm-node", "rocm:0", Some(1024))];
        let report = compute_budget(&nodes, &[], &[device("cuda:0", Some(8192), None)], true)
            .unwrap();
        assert_eq!(report.devices.len(), 2);
        let rocm = report.devices.iter().find(|d| d.device_id == "rocm:0").unwrap();
        assert_eq!(rocm.pipeline_mb, 1024);
        assert_eq!(rocm.total_mb, None);
        assert!(!rocm.over);
    }

    // ── 设备匹配大小写归一 ───────────────────────────────────────────────────

    #[test]
    fn device_matching_is_case_insensitive() {
        let nodes = vec![node("n", "CUDA:0", Some(100))];
        let report = compute_budget(&nodes, &[], &[device("cuda:0", Some(8192), None)], true)
            .unwrap();
        assert_eq!(report.devices[0].pipeline_mb, 100);
    }

    // ── 环 → 错误 ────────────────────────────────────────────────────────────

    #[test]
    fn cycle_returns_error() {
        let nodes = vec![node("a", "cuda:0", Some(1)), node("b", "cuda:0", Some(1))];
        let edges = vec![edge("a", "b"), edge("b", "a")];
        let err = compute_budget(&nodes, &edges, &[], true).unwrap_err();
        assert_eq!(err, VramBudgetError::CycleDetected);
        assert!(err.to_string().contains("cycle"));
    }

    // ── over 判定 ────────────────────────────────────────────────────────────

    #[test]
    fn over_flag_boundaries() {
        // used + pipeline == total → 恰好不超
        let nodes = vec![node("n", "cuda:0", Some(1000))];
        let report = compute_budget(
            &nodes,
            &[],
            &[device("cuda:0", Some(2000), Some(1000))],
            true,
        )
        .unwrap();
        assert!(!report.devices[0].over);

        // +1 MB → 超
        let nodes = vec![node("n", "cuda:0", Some(1001))];
        let report = compute_budget(
            &nodes,
            &[],
            &[device("cuda:0", Some(2000), Some(1000))],
            true,
        )
        .unwrap();
        assert!(report.devices[0].over);

        // 容量未知（used 缺失按 0）
        let report = compute_budget(
            &[node("n", "cuda:0", Some(100))],
            &[],
            &[device("cuda:0", None, None)],
            true,
        )
        .unwrap();
        assert!(!report.devices[0].over);
    }

    // ── parse_max_instances ──────────────────────────────────────────────────

    #[test]
    fn parse_max_instances_variants() {
        assert_eq!(
            parse_max_instances("[pipeline]\nid = \"p\"\nname = \"n\"\nmax_instances = 2\n"),
            Some(2)
        );
        // 缺省 → None（跟随全局）
        assert_eq!(parse_max_instances("[pipeline]\nid = \"p\"\nname = \"n\"\n"), None);
        // 0 / 负数 / 非整数 / 坏 TOML → None
        assert_eq!(
            parse_max_instances("[pipeline]\nmax_instances = 0\n"),
            None
        );
        assert_eq!(
            parse_max_instances("[pipeline]\nmax_instances = -3\n"),
            None
        );
        assert_eq!(
            parse_max_instances("[pipeline]\nmax_instances = \"many\"\n"),
            None
        );
        assert_eq!(parse_max_instances("this is [[ not toml"), None);
    }
}
