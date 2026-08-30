//! DAG 数据结构 + 验证 + 拓扑排序

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

use crate::config::PipelineConfig;
use crate::module::manifest::CapabilityDecl;
use crate::types::{DataType, DeviceId};

// ─── 错误类型 ────────────────────────────────────────────────────────────────

/// 管线验证错误
#[derive(Debug, Clone, Error)]
pub enum ValidationError {
    #[error("duplicate node id: `{0}`")]
    DuplicateNodeId(String),

    #[error("edge references non-existent node: `{0}`")]
    NodeNotFound(String),

    #[error("pipeline contains a cycle")]
    CycleDetected,

    #[error("pipeline must have at least one file_input node")]
    NoFileInput,

    #[error("orphan node `{0}`: not connected to any edge and not a file_input/file_output endpoint")]
    OrphanNode(String),

    #[error("duplicate edge: `{from_node}:{from_port}` -> `{to_node}:{to_port}`")]
    DuplicateEdge {
        from_node: String,
        from_port: String,
        to_node: String,
        to_port: String,
    },

    #[error(
        "port type mismatch: `{from_node}:{from_port}` outputs `{from_type}` \
         but `{to_node}:{to_port}` expects `{to_type}`"
    )]
    PortTypeMismatch {
        from_node: String,
        from_port: String,
        from_type: PortType,
        to_node: String,
        to_port: String,
        to_type: PortType,
    },

    /// §5.6：file_gate 至少配置一项过滤条件，否则静态校验报错
    #[error("file_gate node `{0}` must configure at least one filter condition (§5.6)")]
    GateNoConditions(String),
}

// ─── 端口数据类型（PIPELINE_SPEC §7.1 / §7.2） ──────────────────────────────

/// 端口数据类型（PIPELINE_SPEC §7.1）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortType {
    Audio,
    Video,
    Image,
    Text,
    Json,
    File,
}

impl PortType {
    /// 从字符串解析（大小写不敏感）；未知类型 → `None`
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "audio" => Some(Self::Audio),
            "video" => Some(Self::Video),
            "image" => Some(Self::Image),
            "text" => Some(Self::Text),
            "json" => Some(Self::Json),
            "file" => Some(Self::File),
            _ => None,
        }
    }

    /// §7.2 兼容矩阵：输出类型 `self` 是否可流入输入类型 `target`。
    ///
    /// - 同类型 ✅
    /// - 任意类型 → `file` ✅（文件类端口接受一切）
    /// - `json` → `text` ✅（隐式序列化为 JSON 字符串，§7.3）
    /// - `file` → 具体文件类型（audio/video/image）✅（运行时检查扩展名，§7.3）
    /// - 其余组合 ❌
    pub fn is_compatible_with(self, target: PortType) -> bool {
        self == target
            || matches!(
                (self, target),
                // 任意 → file ✅；json → text ✅*；file → 具体文件类型 ✅
                (_, PortType::File)
                    | (PortType::Json, PortType::Text)
                    | (PortType::File, PortType::Audio | PortType::Video | PortType::Image)
            )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Image => "image",
            Self::Text => "text",
            Self::Json => "json",
            Self::File => "file",
        }
    }
}

impl std::fmt::Display for PortType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// builtin 节点指定端口的数据类型（PIPELINE_SPEC §5 + §6.7）。
///
/// `is_output`：true 查输出端口，false 查输入端口。仅识别默认端口名
/// `"input"` / `"output"`；module 节点（端口类型由模块清单声明，DAG 层不可见）、
/// 未知 builtin、自定义端口名一律返回 `None`（调用方跳过类型检查，不误报）。
fn builtin_port_type(
    builtin: &str,
    params: &serde_json::Value,
    port: &str,
    is_output: bool,
) -> Option<PortType> {
    match builtin {
        "file_input" if is_output && port == "output" => Some(
            params
                .get("accept")
                .and_then(|v| v.as_str())
                .and_then(PortType::parse)
                .unwrap_or(PortType::File),
        ),
        "file_output" if !is_output && port == "input" => Some(PortType::File),
        "ffmpeg" if !is_output && port == "input" => Some(PortType::File),
        "ffmpeg" if is_output && port == "output" => Some(PortType::File),
        // §5.5/§5.6：file_archive / file_gate 均为 文件入 → 文件出
        "file_archive" if !is_output && port == "input" => Some(PortType::File),
        "file_archive" if is_output && port == "output" => Some(PortType::File),
        "file_gate" if !is_output && port == "input" => Some(PortType::File),
        "file_gate" if is_output && port == "output" => Some(PortType::File),
        // §6.7：llm input_type=text；output_format=json 时输出 Json
        "llm" | "external_api" if !is_output && port == "input" => Some(PortType::Text),
        "llm" | "external_api" if is_output && port == "output" => {
            let is_json = params
                .get("output_format")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().eq_ignore_ascii_case("json"))
                .unwrap_or(false);
            Some(if is_json { PortType::Json } else { PortType::Text })
        }
        _ => None,
    }
}

/// 边端点的数据类型解析（供 validate 的端口类型检查使用）
fn endpoint_port_type(node: &PipelineNode, port: &str, is_output: bool) -> Option<PortType> {
    match &node.kind {
        NodeKind::Builtin { builtin } => builtin_port_type(builtin, &node.params, port, is_output),
        // module / 遗留 external_api kind：类型在模块清单/运行期才可见，跳过
        _ => None,
    }
}

/// file_gate 是否至少配置了一项过滤条件（§5.6 静态校验，纯函数可测）。
///
/// 计入的条件：`extensions` / `extensions_exclude` 非空数组、
/// `min_size_bytes` / `max_size_bytes` 存在、`filename_regex` 非空字符串、
/// `media` 对象含至少一个已知子键（`min_duration_secs` / `max_duration_secs` /
/// `min_width` / `min_height`）。空对象 `media: {}` 不计入（无实际判定语义）。
fn file_gate_has_any_condition(params: &serde_json::Value) -> bool {
    let ext_list_nonempty = |key: &str| {
        params
            .get(key)
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty())
    };
    if ext_list_nonempty("extensions") || ext_list_nonempty("extensions_exclude") {
        return true;
    }
    let has_size_bound = |key: &str| {
        params
            .get(key)
            .is_some_and(|v| !v.is_null())
    };
    if has_size_bound("min_size_bytes") || has_size_bound("max_size_bytes") {
        return true;
    }
    if params
        .get("filename_regex")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty())
    {
        return true;
    }
    if let Some(media) = params.get("media").and_then(|v| v.as_object()) {
        return media.keys().any(|k| {
            matches!(
                k.as_str(),
                "min_duration_secs" | "max_duration_secs" | "min_width" | "min_height"
            )
        });
    }
    false
}

// ─── 节点类型 ────────────────────────────────────────────────────────────────

/// 节点在编辑器画布上的坐标（React Flow / 桌面管线编辑器布局用）。
///
/// 纯展示数据：执行器忽略此字段，缺省不影响任何执行语义。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NodePosition {
    pub x: f64,
    pub y: f64,
}

/// 节点种类
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeKind {
    /// 调用已注册模块的 capability
    Module {
        module_id: String,
        capability: String,
        /// 变体 pin（§6.2 冻结字段 `model`；缺省 = 跟随激活变体）。
        /// 对外契约（TOML/JSON）字段名为 `model`（仲裁 #2）；Rust 侧保留
        /// `model_id` 命名以免破坏既有消费方，旧 TOML 的 `model_id` 键经
        /// `alias` 仍可反序列化，序列化恒定输出 `model`。
        /// `skip_serializing_if`：TOML 无 null，None 时不写出该键（反序列化行为不变）
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            rename = "model",
            alias = "model_id"
        )]
        model_id: Option<String>,
        /// 设备绑定（§6.2 冻结字段 `device`）：`"auto"` | `"cuda:0"` | `"rocm:1"`
        /// | `"openvino:GPU.0"` …… **软约束**：加载/导入时本机无此设备 → 警告 +
        /// 回退 auto，不硬失败（见 [`resolve_device_soft_constraint`]）。
        /// None = 未声明（等价 auto）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },
    /// 内置工具节点
    Builtin { builtin: String },
    /// 外部 API 调用 — **遗留形状**：§6.7（决策点 4）起更名并限定为接入
    /// OpenAI 兼容 LLM 端点，规范形状是 `kind = "builtin"` + `builtin = "llm"`。
    ///
    /// 兼容语义：
    /// - `kind = "external_api"`（旧名）与 `kind = "llm"`（别名）均可解析；
    /// - 执行统一走 executor 的 llm 路径（chat/completions 单一形状）；
    /// - kind 级 `endpoint` 映射为 llm 的 `base_url`；`kind = "llm"` 形状的节点
    ///   可省略 `endpoint`，改由 `params.base_url` 声明。
    ///
    /// P2-13 清理：原 `api_type` 字段从未被消费，已移除（旧 TOML 中的该键
    /// 反序列化时按未知字段忽略，不影响加载）。
    #[serde(alias = "llm")]
    ExternalApi {
        /// OpenAI 兼容端点 base_url（如 `https://api.openai.com/v1`），
        /// 可为空（`kind = "llm"` 形状改由 `params.base_url` 声明）。
        #[serde(default, skip_serializing_if = "String::is_empty")]
        endpoint: String,
        /// 持有 API Key 的**环境变量名**（绝不落盘明文密钥）；执行时读取。
        /// 缺省 = 不携带 Authorization（本地免密钥端点，如 Ollama/vLLM）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key_env: Option<String>,
    },
}

/// 管线节点
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PipelineNode {
    pub id: String,
    #[serde(flatten)]
    pub kind: NodeKind,
    #[serde(default)]
    pub label: String,
    #[serde(default = "default_params")]
    pub params: serde_json::Value,
    /// 编辑器画布坐标（可选）。`serde(default)`：旧 TOML 无此字段仍可加载；
    /// `skip_serializing_if`：避免向 TOML 写入 null（TOML 无 null 值）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<NodePosition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<u32>,
}

fn default_params() -> serde_json::Value {
    serde_json::Value::Object(Default::default())
}

/// 边：连接两个节点的端口
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Edge {
    /// (node_id, port)
    pub from: (String, String),
    /// (node_id, port)
    pub to: (String, String),
}

// ─── Pipeline ────────────────────────────────────────────────────────────────

/// 管线定义（DAG）
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Pipeline {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub nodes: Vec<PipelineNode>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    /// 管线级并发上限（§6.8）：本管线同时运行实例数的 semaphore 上限。
    /// `None` = 跟随全局 `max_parallel`；TOML `[pipeline]` 段 `max_instances` 键。
    /// GPU 重管线可锁 `1` 防显存打架。执行层（B3）消费。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_instances: Option<u32>,
    /// 管线级节点硬超时缺省（秒，缺陷 #3 拆分）：本管线内未声明
    /// `timeout_secs` 的节点以此作为 wall-clock 硬超时；`None` = 回退全局
    /// `[pipeline] default_node_timeout_secs` / `default_timeout_secs`。
    /// 长媒体管线（如 video-to-srt 的 ASR 节点）可据此声明更长超时。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_timeout_secs: Option<u32>,
}

/// TOML 文件顶层结构（用于反序列化）
#[derive(Debug, Deserialize)]
struct PipelineFile {
    pipeline: PipelineMeta,
    #[serde(default)]
    nodes: Vec<PipelineNode>,
    #[serde(default)]
    edges: Vec<Edge>,
}

#[derive(Debug, Deserialize)]
struct PipelineMeta {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    /// §6.8 管线级并发上限；`[pipeline]` 段可选键
    #[serde(default)]
    max_instances: Option<u32>,
    /// 缺陷 #3：管线级节点硬超时缺省；`[pipeline]` 段可选键
    #[serde(default)]
    node_timeout_secs: Option<u32>,
}

impl Pipeline {
    /// 从 TOML 文件加载管线定义
    pub fn from_toml(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read pipeline file `{}`: {e}", path.display()))?;
        Self::from_toml_str(&content)
    }

    /// 从 TOML 字符串解析管线定义
    pub fn from_toml_str(content: &str) -> anyhow::Result<Self> {
        let file: PipelineFile = toml::from_str(content)
            .map_err(|e| anyhow::anyhow!("failed to parse pipeline TOML: {e}"))?;

        Ok(Pipeline {
            id: file.pipeline.id,
            name: file.pipeline.name,
            description: file.pipeline.description,
            nodes: file.nodes,
            edges: file.edges,
            max_instances: file.pipeline.max_instances,
            node_timeout_secs: file.pipeline.node_timeout_secs,
        })
    }

    /// 节点级硬超时缺省解析（缺陷 #3 拆分，纯函数可测）。
    ///
    /// 优先级（高→低）：
    /// 1. 管线级 `[pipeline] node_timeout_secs`（本管线内未声明 `timeout_secs`
    ///    的节点以此为准，长媒体管线据此放宽）；
    /// 2. 全局 `default_node_timeout_secs`（>0 时生效）；
    /// 3. 回退 `default_timeout_secs`（旧配置行为不变，向后兼容）。
    ///
    /// 返回 `None` = 三者均无效（0）→ 节点仅受执行器客户端级超时约束。
    /// 节点自身 `timeout_secs` 始终优先于本缺省值（由执行层应用）。
    pub fn effective_default_node_timeout(&self, cfg: &PipelineConfig) -> Option<Duration> {
        let secs = self
            .node_timeout_secs
            .filter(|&v| v > 0)
            .or(Some(cfg.default_node_timeout_secs))
            .filter(|&v| v > 0)
            .or(Some(cfg.default_timeout_secs))
            .filter(|&v| v > 0)?;
        Some(Duration::from_secs(u64::from(secs)))
    }

    /// 验证管线 DAG 合法性
    ///
    /// 检查：
    /// - 节点 id 唯一
    /// - 边引用的节点存在
    /// - 无环（拓扑排序检测）
    /// - 至少一个 file_input 节点
    /// - 无孤儿节点（无任何边相连且非 file_input/file_output 端点，P2-11）
    /// - 无重复边（from/to 四元组完全相同，P2-11）
    /// - 端口数据类型兼容（PIPELINE_SPEC §7.2 矩阵，P2-11）
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();

        // 1. 节点 id 唯一
        let mut seen_ids = HashSet::new();
        for node in &self.nodes {
            if !seen_ids.insert(&node.id) {
                errors.push(ValidationError::DuplicateNodeId(node.id.clone()));
            }
        }

        // 2. 边引用的节点存在
        for edge in &self.edges {
            if !seen_ids.contains(&edge.from.0) {
                errors.push(ValidationError::NodeNotFound(edge.from.0.clone()));
            }
            if !seen_ids.contains(&edge.to.0) {
                errors.push(ValidationError::NodeNotFound(edge.to.0.clone()));
            }
        }

        // 3. 无环检测（通过拓扑排序）
        if errors.is_empty()
            && self.topological_layers().is_err() {
                errors.push(ValidationError::CycleDetected);
            }

        // 4. 至少一个 file_input 节点
        let has_file_input = self.nodes.iter().any(|n| {
            matches!(&n.kind, NodeKind::Builtin { builtin } if builtin == "file_input")
        });
        if !has_file_input {
            errors.push(ValidationError::NoFileInput);
        }

        // 5. 孤儿节点：无任何边相连且非 file_input/file_output 端点（P2-11）
        let mut connected: HashSet<&str> = HashSet::new();
        for edge in &self.edges {
            connected.insert(edge.from.0.as_str());
            connected.insert(edge.to.0.as_str());
        }
        for node in &self.nodes {
            let is_endpoint = matches!(
                &node.kind,
                NodeKind::Builtin { builtin } if builtin == "file_input" || builtin == "file_output"
            );
            if !connected.contains(node.id.as_str()) && !is_endpoint {
                errors.push(ValidationError::OrphanNode(node.id.clone()));
            }
        }

        // 6. 重复边：from/to 四元组完全相同（P2-11）
        let mut seen_edges: HashSet<(&str, &str, &str, &str)> = HashSet::new();
        for edge in &self.edges {
            let key = (
                edge.from.0.as_str(),
                edge.from.1.as_str(),
                edge.to.0.as_str(),
                edge.to.1.as_str(),
            );
            if !seen_edges.insert(key) {
                errors.push(ValidationError::DuplicateEdge {
                    from_node: edge.from.0.clone(),
                    from_port: edge.from.1.clone(),
                    to_node: edge.to.0.clone(),
                    to_port: edge.to.1.clone(),
                });
            }
        }

        // 7. 端口类型兼容性（PIPELINE_SPEC §7.2）：仅两端类型均可解析时检查
        //    （module 节点端口类型在模块清单层声明，DAG 层不可见 → 跳过不误报）
        let node_by_id: HashMap<&str, &PipelineNode> =
            self.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        for edge in &self.edges {
            let (Some(from_node), Some(to_node)) = (
                node_by_id.get(edge.from.0.as_str()),
                node_by_id.get(edge.to.0.as_str()),
            ) else {
                continue; // 缺失节点已在检查 2 报告
            };
            let (Some(from_type), Some(to_type)) = (
                endpoint_port_type(from_node, &edge.from.1, true),
                endpoint_port_type(to_node, &edge.to.1, false),
            ) else {
                continue; // 任一端类型未知 → 跳过（运行期校验兜底）
            };
            if !from_type.is_compatible_with(to_type) {
                errors.push(ValidationError::PortTypeMismatch {
                    from_node: edge.from.0.clone(),
                    from_port: edge.from.1.clone(),
                    from_type,
                    to_node: edge.to.0.clone(),
                    to_port: edge.to.1.clone(),
                    to_type,
                });
            }
        }

        // 8. file_gate 条件配置校验（§5.6）：至少配置一项过滤条件，
        //    否则节点永远无判定依据（全部透传语义无意义且易误配）
        for node in &self.nodes {
            if matches!(&node.kind, NodeKind::Builtin { builtin } if builtin == "file_gate")
                && !file_gate_has_any_condition(&node.params)
            {
                errors.push(ValidationError::GateNoConditions(node.id.clone()));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// 拓扑排序分层 — 同层节点无依赖关系，可并行执行
    ///
    /// 返回 `Err` 表示存在环。
    #[allow(clippy::result_unit_err)]
    pub fn topological_layers(&self) -> Result<Vec<Vec<String>>, ()> {
        let node_ids: Vec<&str> = self.nodes.iter().map(|n| n.id.as_str()).collect();
        let node_set: HashSet<&str> = node_ids.iter().copied().collect();

        // 构建邻接表 + 入度
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();

        for id in &node_ids {
            in_degree.insert(id, 0);
            adjacency.insert(id, Vec::new());
        }

        for edge in &self.edges {
            let from = edge.from.0.as_str();
            let to = edge.to.0.as_str();
            if node_set.contains(from) && node_set.contains(to) {
                adjacency.get_mut(from).unwrap().push(to);
                *in_degree.get_mut(to).unwrap() += 1;
            }
        }

        // Kahn's algorithm — 分层
        let mut layers: Vec<Vec<String>> = Vec::new();
        let mut queue: VecDeque<&str> = VecDeque::new();

        for (id, &deg) in &in_degree {
            if deg == 0 {
                queue.push_back(id);
            }
        }

        let mut processed = 0usize;

        while !queue.is_empty() {
            let layer: Vec<String> = queue.drain(..).map(|s| s.to_string()).collect();
            let mut next_queue: VecDeque<&str> = VecDeque::new();

            for node_id in &layer {
                processed += 1;
                if let Some(neighbors) = adjacency.get(node_id.as_str()) {
                    for &next in neighbors {
                        let deg = in_degree.get_mut(next).unwrap();
                        *deg -= 1;
                        if *deg == 0 {
                            next_queue.push_back(next);
                        }
                    }
                }
            }

            layers.push(layer);
            queue = next_queue;
        }

        if processed < node_ids.len() {
            Err(()) // 存在环
        } else {
            Ok(layers)
        }
    }

    /// 获取指定节点的所有上游节点 id（直接前驱）
    pub fn upstream_of(&self, node_id: &str) -> Vec<&str> {
        self.edges
            .iter()
            .filter(|e| e.to.0 == node_id)
            .map(|e| e.from.0.as_str())
            .collect()
    }

    /// 获取指定节点的所有下游节点 id（直接后继）
    pub fn downstream_of(&self, node_id: &str) -> Vec<&str> {
        self.edges
            .iter()
            .filter(|e| e.from.0 == node_id)
            .map(|e| e.to.0.as_str())
            .collect()
    }

    /// 递归获取所有下游节点（传递闭包）
    pub fn all_downstream_of(&self, node_id: &str) -> Vec<&str> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let mut queue: VecDeque<&str> = VecDeque::new();

        // 种子：直接下游
        for &d in &self.downstream_of(node_id) {
            if visited.insert(d) {
                queue.push_back(d);
            }
        }

        while let Some(current) = queue.pop_front() {
            result.push(current);
            for &d in &self.downstream_of(current) {
                if visited.insert(d) {
                    queue.push_back(d);
                }
            }
        }

        result
    }
}

// ─── 节点 device 软约束（§6.2） ─────────────────────────────────────────────

/// 请求的设备字符串是否可被本机设备列表满足。
///
/// 匹配规则（大小写不敏感）：
/// - `None` / 空串 / `"auto"` → 恒为 true（auto 由调度器落位，总是可满足）
/// - 与 [`DeviceId`] 显示形式全等：`cuda:0`、`rocm:1`、`openvino:GPU.0`、
///   `directml:0`、`cpu`
/// - 后端前缀匹配：`cuda` / `rocm` / `openvino` / `directml` / `cpu`
///   匹配该后端下的任一设备
pub fn device_is_available(requested: Option<&str>, available_devices: &[DeviceId]) -> bool {
    let Some(req) = requested.map(str::trim).filter(|s| !s.is_empty()) else {
        return true;
    };
    if req.eq_ignore_ascii_case("auto") {
        return true;
    }
    available_devices.iter().any(|id| {
        id.to_string().eq_ignore_ascii_case(req)
            || id.backend().to_string().eq_ignore_ascii_case(req)
    })
}

/// 节点 `device` 软约束解析（§6.2）：**软约束** —— 不满足时警告并回退 auto，
/// 绝不硬失败。供管线加载/整合包导入路径消费（有设备清单上下文时调用）。
///
/// 返回 `(resolved_device, warning)`：
/// - 请求为 `None` / 空 / `"auto"` → `(None, None)`（None 即 auto）
/// - 请求设备存在于 `available_devices` → `(Some(请求值原样), None)`
/// - 请求设备缺失 → `(None, Some(英文警告))`，调用方记录警告并按 auto 处理
pub fn resolve_device_soft_constraint(
    requested: Option<&str>,
    available_devices: &[DeviceId],
) -> (Option<String>, Option<String>) {
    let Some(req) = requested
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto"))
    else {
        return (None, None);
    };

    if device_is_available(Some(req), available_devices) {
        (Some(req.to_string()), None)
    } else {
        (
            None,
            Some(format!(
                "requested device `{req}` is not available on this machine; falling back to `auto`"
            )),
        )
    }
}

/// 直跑退化 DAG `file_output` 产物扩展名推导（F2 修复：跨格式能力误标，
/// daemon `execution::build_direct_pipeline` 与桌面端 `build_direct_pipeline`
/// 两端共用的唯一推导源，杜绝口径漂移）。
///
/// 优先级：
/// 1. 请求参数显式声明的 `output_format`（如 faster-whisper 传 `srt` → `.srt`）；
/// 2. capability `output_type` 语义映射——仅当与 `input_type` **不同**
///    （跨格式能力）时采用：audio→`wav` / image→`png` / text→`txt` /
///    json→`json`；video/file 无固定扩展名，落到 3；
/// 3. 回退输入文件扩展名（rembg/deep-filter 等同格式能力保持输入扩展名，
///    与 D-7 修复后的现行为一致）。
///
/// 返回值已做与执行器 `file_output` 同口径的字符清洗（仅留 ASCII 字母数字）；
/// 空串 → `None`（调用方不带 `extension` 参数，引擎回落 `.out`）。
pub fn direct_output_extension(
    params: &serde_json::Value,
    capability: Option<&CapabilityDecl>,
    input_path: &Path,
) -> Option<String> {
    fn sanitize(raw: &str) -> Option<String> {
        let ext: String = raw
            .trim()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        (!ext.is_empty()).then_some(ext)
    }

    // ① 请求参数显式声明的 output_format
    if let Some(fmt) = params.get("output_format").and_then(|v| v.as_str()) {
        if let Some(ext) = sanitize(fmt) {
            return Some(ext);
        }
    }

    // ② capability output_type 语义映射（仅跨格式能力生效）
    if let Some(cap) = capability {
        if cap.output_type != cap.input_type {
            let mapped = match cap.output_type {
                DataType::Audio => Some("wav"),
                DataType::Image => Some("png"),
                DataType::Text => Some("txt"),
                DataType::Json => Some("json"),
                DataType::Video | DataType::File => None,
            };
            if let Some(ext) = mapped {
                return Some(ext.to_string());
            }
        }
    }

    // ③ 回退输入文件扩展名（同格式能力保持现行为）
    input_path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(sanitize)
}

// ─── 单元测试 ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 构建一个简单的线性管线用于测试
    fn sample_toml() -> &'static str {
        r#"
[pipeline]
id = "test-pipeline"
name = "Test pipeline"
description = "For unit tests"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
label = "Input"
params = { accept = "audio" }

[[nodes]]
id = "process"
kind = "module"
module_id = "faster-whisper"
capability = "transcribe"
label = "Transcribe"

[[nodes]]
id = "save"
kind = "builtin"
builtin = "file_output"
label = "Save"

[[edges]]
from = ["input", "output"]
to = ["process", "input"]

[[edges]]
from = ["process", "output"]
to = ["save", "input"]
"#
    }

    #[test]
    fn test_topological_sort_linear() {
        let pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        let layers = pipeline.topological_layers().unwrap();

        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0], vec!["input"]);
        assert_eq!(layers[1], vec!["process"]);
        assert_eq!(layers[2], vec!["save"]);
    }

    #[test]
    fn test_effective_default_node_timeout_priority() {
        let pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        assert!(pipeline.node_timeout_secs.is_none(), "缺省 TOML 不含该键");

        // 1) 管线未声明 + 全局缺省为 0 → 回退 default_timeout_secs（旧配置行为不变）
        let cfg = PipelineConfig::default(); // default_timeout_secs=600
        assert_eq!(
            pipeline.effective_default_node_timeout(&cfg),
            Some(Duration::from_secs(600))
        );

        // 2) 全局 default_node_timeout_secs > 0 优先于回退值
        let cfg_global = PipelineConfig {
            default_node_timeout_secs: 1200,
            ..PipelineConfig::default()
        };
        assert_eq!(
            pipeline.effective_default_node_timeout(&cfg_global),
            Some(Duration::from_secs(1200))
        );

        // 3) 管线级 node_timeout_secs 优先级最高（长媒体管线据此放宽）
        let toml_str = sample_toml().replace(
            "description = \"For unit tests\"",
            "description = \"For unit tests\"\nnode_timeout_secs = 3600",
        );
        let p3 = Pipeline::from_toml_str(&toml_str).unwrap();
        assert_eq!(p3.node_timeout_secs, Some(3600));
        assert_eq!(
            p3.effective_default_node_timeout(&cfg_global),
            Some(Duration::from_secs(3600))
        );

        // 4) 三者均为 0 → None（节点仅受执行器客户端级超时约束）
        let cfg_zero = PipelineConfig {
            default_timeout_secs: 0,
            default_node_timeout_secs: 0,
            ..PipelineConfig::default()
        };
        assert_eq!(pipeline.effective_default_node_timeout(&cfg_zero), None);
    }

    #[test]
    fn test_topological_sort_parallel() {
        let toml_str = r#"
[pipeline]
id = "parallel"
name = "Parallel pipeline"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "a"
kind = "builtin"
builtin = "ffmpeg"

[[nodes]]
id = "b"
kind = "builtin"
builtin = "ffmpeg"

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"

[[edges]]
from = ["input", "output"]
to = ["a", "input"]

[[edges]]
from = ["input", "output"]
to = ["b", "input"]

[[edges]]
from = ["a", "output"]
to = ["output", "input"]

[[edges]]
from = ["b", "output"]
to = ["output", "input2"]
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        let layers = pipeline.topological_layers().unwrap();

        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0], vec!["input"]);
        // a 和 b 在同一层（顺序可能不同）
        let mut layer1 = layers[1].clone();
        layer1.sort();
        assert_eq!(layer1, vec!["a", "b"]);
        assert_eq!(layers[2], vec!["output"]);
    }

    #[test]
    fn test_cycle_detection() {
        let toml_str = r#"
[pipeline]
id = "cycle"
name = "Cyclic pipeline"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "a"
kind = "builtin"
builtin = "ffmpeg"

[[nodes]]
id = "b"
kind = "builtin"
builtin = "ffmpeg"

[[edges]]
from = ["input", "output"]
to = ["a", "input"]

[[edges]]
from = ["a", "output"]
to = ["b", "input"]

[[edges]]
from = ["b", "output"]
to = ["a", "input"]
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        assert!(pipeline.topological_layers().is_err());

        let errors = pipeline.validate().unwrap_err();
        assert!(errors.iter().any(|e| matches!(e, ValidationError::CycleDetected)));
    }

    #[test]
    fn test_duplicate_node_id() {
        let toml_str = r#"
[pipeline]
id = "dup"
name = "Duplicate ID"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "ffmpeg"
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        let errors = pipeline.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ValidationError::DuplicateNodeId(id) if id == "input")));
    }

    #[test]
    fn test_edge_references_nonexistent_node() {
        let toml_str = r#"
[pipeline]
id = "bad-edge"
name = "Invalid edge"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[edges]]
from = ["input", "output"]
to = ["ghost", "input"]
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        let errors = pipeline.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ValidationError::NodeNotFound(id) if id == "ghost")));
    }

    #[test]
    fn test_no_file_input() {
        let toml_str = r#"
[pipeline]
id = "no-input"
name = "No input"

[[nodes]]
id = "process"
kind = "module"
module_id = "faster-whisper"
capability = "transcribe"
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        let errors = pipeline.validate().unwrap_err();
        assert!(errors.iter().any(|e| matches!(e, ValidationError::NoFileInput)));
    }

    #[test]
    fn test_from_toml_parsing() {
        let pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();

        assert_eq!(pipeline.id, "test-pipeline");
        assert_eq!(pipeline.name, "Test pipeline");
        assert_eq!(pipeline.description, "For unit tests");
        assert_eq!(pipeline.nodes.len(), 3);
        assert_eq!(pipeline.edges.len(), 2);

        // 验证节点类型解析
        assert_eq!(
            pipeline.nodes[0].kind,
            NodeKind::Builtin {
                builtin: "file_input".to_string()
            }
        );
        assert_eq!(
            pipeline.nodes[1].kind,
            NodeKind::Module {
                module_id: "faster-whisper".to_string(),
                capability: "transcribe".to_string(),
                model_id: None,
                device: None,
            }
        );

        // 验证边解析
        assert_eq!(
            pipeline.edges[0],
            Edge {
                from: ("input".to_string(), "output".to_string()),
                to: ("process".to_string(), "input".to_string()),
            }
        );
    }

    #[test]
    fn test_upstream_downstream() {
        let pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();

        assert_eq!(pipeline.upstream_of("process"), vec!["input"]);
        assert_eq!(pipeline.downstream_of("process"), vec!["save"]);
        assert_eq!(pipeline.upstream_of("input").len(), 0);
        assert_eq!(pipeline.downstream_of("save").len(), 0);
    }

    #[test]
    fn test_all_downstream() {
        let pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        let downstream = pipeline.all_downstream_of("input");
        assert_eq!(downstream.len(), 2);
        assert!(downstream.contains(&"process"));
        assert!(downstream.contains(&"save"));
    }

    #[test]
    fn test_valid_pipeline_passes() {
        let pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        assert!(pipeline.validate().is_ok());
    }

    #[test]
    fn test_external_api_node_parsing() {
        // 旧形状 TOML（含已移除的 api_type 键）必须仍可加载：
        // api_type 按未知字段忽略（P2-13 清理，向后兼容）
        let toml_str = r#"
[pipeline]
id = "api-test"
name = "API test"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "translate"
kind = "external_api"
endpoint = "https://api.example.com/v1"
api_type = "openai"
api_key_env = "MY_API_KEY"
label = "Translate"
params = { model = "gpt-4", temperature = 0.3 }

[[edges]]
from = ["input", "output"]
to = ["translate", "input"]
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        assert_eq!(
            pipeline.nodes[1].kind,
            NodeKind::ExternalApi {
                endpoint: "https://api.example.com/v1".to_string(),
                api_key_env: Some("MY_API_KEY".to_string()),
            }
        );
        assert!(pipeline.validate().is_ok());
    }

    // ─── §6.2 节点 schema：model / device（仲裁 #2） ───────────────────────

    #[test]
    fn test_module_node_model_and_device_parse() {
        let toml_str = r#"
[pipeline]
id = "schema-test"
name = "Schema test"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "asr"
kind = "module"
module_id = "faster-whisper"
capability = "transcribe"
model = "ep.systran.faster-whisper@medium"
device = "cuda:0"
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        assert_eq!(
            pipeline.nodes[1].kind,
            NodeKind::Module {
                module_id: "faster-whisper".to_string(),
                capability: "transcribe".to_string(),
                model_id: Some("ep.systran.faster-whisper@medium".to_string()),
                device: Some("cuda:0".to_string()),
            }
        );
    }

    #[test]
    fn test_module_node_legacy_model_id_alias_and_serialization() {
        // 旧 TOML 键 `model_id` 仍可反序列化（alias 向后兼容）
        let toml_str = r#"
[pipeline]
id = "legacy-model-field"
name = "Legacy model field"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "asr"
kind = "module"
module_id = "faster-whisper"
capability = "transcribe"
model_id = "large-v3"
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        match &pipeline.nodes[1].kind {
            NodeKind::Module { model_id, device, .. } => {
                assert_eq!(model_id.as_deref(), Some("large-v3"));
                assert_eq!(device, &None);
            }
            other => panic!("expected module node, got {other:?}"),
        }

        // 序列化恒定输出新契约键 `model`，绝不输出 `model_id`
        let out = toml::to_string_pretty(&pipeline).unwrap();
        assert!(out.contains("model = \"large-v3\""), "got: {out}");
        assert!(!out.contains("model_id ="), "legacy key must not be re-emitted: {out}");

        // serde 层往返：JSON 形状同样用新键 `model`，且往返等价
        let v = serde_json::to_value(&pipeline.nodes[1]).unwrap();
        assert_eq!(v["model"], "large-v3");
        assert!(v.get("model_id").is_none(), "legacy key must not appear in JSON: {v}");
        let again: PipelineNode = serde_json::from_value(v).unwrap();
        assert_eq!(again, pipeline.nodes[1]);
    }

    #[test]
    fn test_llm_kind_alias_parses_as_external_api_shape() {
        // §6.7：`kind = "llm"` 作为 external_api 的别名可解析；
        // endpoint 可省略（由 params.base_url 声明）
        let toml_str = r#"
[pipeline]
id = "llm-alias"
name = "LLM alias"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "translate"
kind = "llm"
api_key_env = "OPENAI_API_KEY"

[nodes.params]
base_url = "https://api.openai.com/v1"
model = "gpt-4o-mini"

[[edges]]
from = ["input", "output"]
to = ["translate", "input"]
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        assert_eq!(
            pipeline.nodes[1].kind,
            NodeKind::ExternalApi {
                endpoint: String::new(),
                api_key_env: Some("OPENAI_API_KEY".to_string()),
            }
        );
        assert_eq!(
            pipeline.nodes[1].params.get("base_url").and_then(|v| v.as_str()),
            Some("https://api.openai.com/v1")
        );
        assert!(pipeline.validate().is_ok());
    }

    // ─── device 软约束（§6.2） ──────────────────────────────────────────────

    use crate::types::DeviceId;

    fn sample_devices() -> Vec<DeviceId> {
        vec![
            DeviceId::Cuda(0),
            DeviceId::OpenVINO("GPU.0".to_string()),
            DeviceId::Cpu,
        ]
    }

    #[test]
    fn test_device_is_available_matching_rules() {
        let devices = sample_devices();
        // auto / 缺省恒可满足
        assert!(device_is_available(None, &devices));
        assert!(device_is_available(Some(""), &devices));
        assert!(device_is_available(Some("auto"), &devices));
        assert!(device_is_available(Some("AUTO"), &devices));
        // 全等匹配（大小写不敏感）
        assert!(device_is_available(Some("cuda:0"), &devices));
        assert!(device_is_available(Some("CUDA:0"), &devices));
        assert!(device_is_available(Some("openvino:GPU.0"), &devices));
        assert!(device_is_available(Some("cpu"), &devices));
        // 后端前缀匹配
        assert!(device_is_available(Some("cuda"), &devices));
        assert!(device_is_available(Some("openvino"), &devices));
        // 缺失
        assert!(!device_is_available(Some("cuda:1"), &devices));
        assert!(!device_is_available(Some("rocm:0"), &devices));
        assert!(!device_is_available(Some("rocm"), &devices));
        // 空设备列表：仅 auto 可满足
        assert!(device_is_available(Some("auto"), &[]));
        assert!(!device_is_available(Some("cuda:0"), &[]));
    }

    #[test]
    fn test_resolve_device_soft_constraint_fallback_warns_not_fails() {
        let devices = sample_devices();

        // 缺省 / auto → 无警告
        assert_eq!(resolve_device_soft_constraint(None, &devices), (None, None));
        assert_eq!(
            resolve_device_soft_constraint(Some("auto"), &devices),
            (None, None)
        );

        // 设备存在 → 原样保留
        assert_eq!(
            resolve_device_soft_constraint(Some("cuda:0"), &devices),
            (Some("cuda:0".to_string()), None)
        );

        // 设备缺失 → 回退 auto + 英文警告（软约束，非错误）
        let (resolved, warning) = resolve_device_soft_constraint(Some("rocm:1"), &devices);
        assert_eq!(resolved, None);
        let warn = warning.expect("missing device must produce a warning");
        assert!(warn.contains("rocm:1") && warn.contains("auto"), "got: {warn}");
    }

    // ─── P2-11：validate 补漏（孤儿节点 / 重复边 / 端口类型） ────────────────

    /// §7.2 兼容矩阵全表（纯函数级）
    #[test]
    fn test_port_type_compatibility_matrix() {
        use PortType::*;
        let types = [Audio, Video, Image, Text, Json, File];
        for &src in &types {
            for &dst in &types {
                let expected = src == dst
                    || dst == File // 任意 → file ✅
                    || (src, dst) == (Json, Text) // json → text ✅*
                    || (src == File && matches!(dst, Audio | Video | Image)); // file → 具体文件类型 ✅
                assert_eq!(
                    src.is_compatible_with(dst),
                    expected,
                    "{src} -> {dst} should be {expected} per §7.2"
                );
            }
        }
        // parse 大小写不敏感 + 未知类型
        assert_eq!(PortType::parse("AUDIO"), Some(Audio));
        assert_eq!(PortType::parse(" json "), Some(Json));
        assert_eq!(PortType::parse("nope"), None);
    }

    #[test]
    fn test_validate_orphan_node_detected_endpoints_exempt() {
        // stray(module) 无边相连 → 孤儿；loose_out(file_output) 无边但是端点 → 豁免
        let toml_str = r#"
[pipeline]
id = "orphan"
name = "Orphan test"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "mid"
kind = "builtin"
builtin = "ffmpeg"

[[nodes]]
id = "stray"
kind = "module"
module_id = "faster-whisper"
capability = "transcribe"

[[nodes]]
id = "loose_out"
kind = "builtin"
builtin = "file_output"

[[edges]]
from = ["input", "output"]
to = ["mid", "input"]
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        let errors = pipeline.validate().unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::OrphanNode(id) if id == "stray")),
            "stray module node must be flagged: {errors:?}"
        );
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::OrphanNode(id) if id == "loose_out")),
            "file_output endpoint must be exempt: {errors:?}"
        );
        // 英文技术层消息
        let msg = errors
            .iter()
            .find_map(|e| match e {
                ValidationError::OrphanNode(_) => Some(e.to_string()),
                _ => None,
            })
            .unwrap();
        assert!(msg.contains("orphan") && msg.contains("stray"), "got: {msg}");
    }

    #[test]
    fn test_validate_no_orphan_when_fully_connected() {
        // 反例：全连接 + 未连接端点 → 无孤儿错误
        let toml_str = r#"
[pipeline]
id = "no-orphan"
name = "No orphan"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "save"
kind = "builtin"
builtin = "file_output"

[[nodes]]
id = "save2"
kind = "builtin"
builtin = "file_output"

[[edges]]
from = ["input", "output"]
to = ["save", "input"]
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        assert!(pipeline.validate().is_ok(), "endpoints without edges are exempt");
    }

    #[test]
    fn test_validate_duplicate_edge_detected() {
        let toml_str = r#"
[pipeline]
id = "dup-edge"
name = "Duplicate edge"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "a"
kind = "builtin"
builtin = "ffmpeg"

[[edges]]
from = ["input", "output"]
to = ["a", "input"]

[[edges]]
from = ["input", "output"]
to = ["a", "input"]
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        let errors = pipeline.validate().unwrap_err();
        let dup: Vec<_> = errors
            .iter()
            .filter(|e| matches!(e, ValidationError::DuplicateEdge { .. }))
            .collect();
        assert_eq!(dup.len(), 1, "exactly one duplicate-edge error: {errors:?}");
        let msg = dup[0].to_string();
        assert!(
            msg.contains("input:output") && msg.contains("a:input"),
            "error should name both endpoints: {msg}"
        );

        // 反例：同一对节点但端口不同 → 不算重复
        let toml_str = r#"
[pipeline]
id = "fanout"
name = "Fanout ok"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "a"
kind = "builtin"
builtin = "ffmpeg"

[[nodes]]
id = "b"
kind = "builtin"
builtin = "ffmpeg"

[[edges]]
from = ["input", "output"]
to = ["a", "input"]

[[edges]]
from = ["input", "output"]
to = ["b", "input"]
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        assert!(
            pipeline.validate().is_ok(),
            "fanout to different nodes is not a duplicate: {:?}",
            pipeline.validate().unwrap_err()
        );
    }

    #[test]
    fn test_validate_port_type_mismatch_detected() {
        // file_input(accept=audio) → llm(input_type=text)：audio → text ❌
        let toml_str = r#"
[pipeline]
id = "type-mismatch"
name = "Type mismatch"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = { accept = "audio" }

[[nodes]]
id = "translate"
kind = "builtin"
builtin = "llm"

[[edges]]
from = ["input", "output"]
to = ["translate", "input"]
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        let errors = pipeline.validate().unwrap_err();
        let mismatch = errors.iter().find_map(|e| match e {
            ValidationError::PortTypeMismatch {
                from_node,
                from_type,
                to_node,
                to_type,
                ..
            } => Some((from_node.clone(), *from_type, to_node.clone(), *to_type)),
            _ => None,
        });
        assert_eq!(
            mismatch,
            Some((
                "input".to_string(),
                PortType::Audio,
                "translate".to_string(),
                PortType::Text
            )),
            "audio -> text must be flagged: {errors:?}"
        );
    }

    #[test]
    fn test_validate_port_type_realistic_llm_chain_ok() {
        // file_input → ffmpeg(file→file ✅) → module(类型不可见，跳过)
        // → llm(output_format=json) → llm(json→text ✅，§7.2 *)
        let toml_str = r#"
[pipeline]
id = "llm-chain"
name = "Realistic LLM chain"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = { accept = "video" }

[[nodes]]
id = "extract"
kind = "builtin"
builtin = "ffmpeg"

[[nodes]]
id = "asr"
kind = "module"
module_id = "faster-whisper"
capability = "transcribe"

[[nodes]]
id = "gen"
kind = "builtin"
builtin = "llm"
params = { base_url = "http://127.0.0.1:11434/v1", model = "qwen2.5", output_format = "json" }

[[nodes]]
id = "check"
kind = "builtin"
builtin = "llm"
params = { base_url = "http://127.0.0.1:11434/v1", model = "qwen2.5" }

[[edges]]
from = ["input", "output"]
to = ["extract", "input"]

[[edges]]
from = ["extract", "output"]
to = ["asr", "input"]

[[edges]]
from = ["asr", "output"]
to = ["gen", "input"]

[[edges]]
from = ["gen", "output"]
to = ["check", "input"]
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        assert!(
            pipeline.validate().is_ok(),
            "realistic chain must pass: {:?}",
            pipeline.validate().unwrap_err()
        );
    }

    // ─── §6.8 max_instances（管线级并发上限） ────────────────────────────────

    #[test]
    fn test_max_instances_toml_parse_and_serialize() {
        let toml_str = r#"
[pipeline]
id = "mi"
name = "Max instances"
max_instances = 2

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        assert_eq!(pipeline.max_instances, Some(2));

        // 序列化保留该键
        let out = toml::to_string_pretty(&pipeline).unwrap();
        assert!(out.contains("max_instances = 2"), "got: {out}");

        // 缺省 → None 且不写出该键
        let toml_str = r#"
[pipeline]
id = "mi-none"
name = "No max instances"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        assert_eq!(pipeline.max_instances, None);
        let out = toml::to_string_pretty(&pipeline).unwrap();
        assert!(!out.contains("max_instances"), "got: {out}");
    }

    // ─── position 字段与 TOML 序列化（WebUI 桥接依赖） ─────────────────────

    #[test]
    fn test_node_position_parses_and_defaults_to_none() {
        let toml_str = r#"
[pipeline]
id = "pos-test"
name = "Position test"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
position = { x = 240.5, y = -80.0 }
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        // 旧格式（无 position）向后兼容
        assert_eq!(pipeline.nodes[0].position, None);
        // 新格式 {x, y} 解析
        assert_eq!(
            pipeline.nodes[1].position,
            Some(NodePosition { x: 240.5, y: -80.0 })
        );
    }

    #[test]
    fn test_toml_pretty_serialization_outputs() {
        // 要求：Pipeline/Node/Edge 均可经 toml::to_string_pretty 序列化输出
        let pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        let pretty = toml::to_string_pretty(&pipeline)
            .expect("Pipeline should serialize via toml::to_string_pretty");
        assert!(pretty.contains("test-pipeline"));
        assert!(pretty.contains("[[nodes]]"));
        assert!(pretty.contains("[[edges]]"));

        // 带 position 的节点也能序列化，且 position 形状为 {x, y}
        let mut p2 = pipeline.clone();
        p2.nodes[0].position = Some(NodePosition { x: 1.0, y: 2.0 });
        let pretty2 = toml::to_string_pretty(&p2).expect("serialization with position");
        assert!(pretty2.contains("x = 1.0"));
        assert!(pretty2.contains("y = 2.0"));

        // position 缺失时不写出该键（TOML 无 null）
        assert!(!pretty.contains("position"));
    }

    #[test]
    fn test_node_json_position_shape() {
        // 前端契约：position 为 {"x": ..., "y": ...}
        let node = PipelineNode {
            id: "n1".to_string(),
            kind: NodeKind::Builtin {
                builtin: "file_input".to_string(),
            },
            label: String::new(),
            params: default_params(),
            position: Some(NodePosition { x: 12.5, y: 34.0 }),
            timeout_secs: None,
            retry_count: None,
        };
        let v = serde_json::to_value(&node).unwrap();
        assert_eq!(v["position"]["x"], 12.5);
        assert_eq!(v["position"]["y"], 34.0);
    }

    // ── F2：直跑产物扩展名推导（direct_output_extension 三级优先级） ──────

    fn cap_decl(input: DataType, output: DataType) -> CapabilityDecl {
        CapabilityDecl {
            name: "test".to_string(),
            description: String::new(),
            input_type: input,
            output_type: output,
            max_file_size_mb: None,
            supports_batch: false,
            params: None,
        }
    }

    #[test]
    fn direct_output_extension_output_format_wins() {
        // ① 请求参数 output_format 优先于一切（faster-whisper 传 srt）
        let params = serde_json::json!({ "output_format": "srt" });
        let cap = cap_decl(DataType::Audio, DataType::Json);
        assert_eq!(
            direct_output_extension(&params, Some(&cap), Path::new("/data/in.wav")),
            Some("srt".to_string())
        );
        // 非字符串 / 空值不参与（回退后续优先级）
        let empty = serde_json::json!({ "output_format": "  " });
        assert_eq!(
            direct_output_extension(&empty, Some(&cap), Path::new("/data/in.wav")),
            Some("json".to_string())
        );
    }

    #[test]
    fn direct_output_extension_cross_format_mapping() {
        // ② 跨格式能力语义映射：TTS text→audio → wav（F2 主场景）
        let no_fmt = serde_json::json!({ "voice": "default" });
        let tts = cap_decl(DataType::Text, DataType::Audio);
        assert_eq!(
            direct_output_extension(&no_fmt, Some(&tts), Path::new("/data/tts_input.txt")),
            Some("wav".to_string())
        );
        // OCR image→json → json
        let ocr = cap_decl(DataType::Image, DataType::Json);
        assert_eq!(
            direct_output_extension(&no_fmt, Some(&ocr), Path::new("/data/page.png")),
            Some("json".to_string())
        );
        // audio→text → txt；json→image → png
        let asr = cap_decl(DataType::Audio, DataType::Text);
        assert_eq!(
            direct_output_extension(&no_fmt, Some(&asr), Path::new("/data/a.mp3")),
            Some("txt".to_string())
        );
        let gen = cap_decl(DataType::Json, DataType::Image);
        assert_eq!(
            direct_output_extension(&no_fmt, Some(&gen), Path::new("/data/spec.json")),
            Some("png".to_string())
        );
    }

    #[test]
    fn direct_output_extension_same_format_keeps_input_ext() {
        // ③ 同格式能力回退输入扩展名（现役 5 模块回归点）
        let no_fmt = serde_json::json!({});
        let rembg = cap_decl(DataType::Image, DataType::Image);
        assert_eq!(
            direct_output_extension(&no_fmt, Some(&rembg), Path::new("/data/photo.png")),
            Some("png".to_string())
        );
        // rembg 输入 jpg 时产物仍随输入 jpg，不被映射成 png
        assert_eq!(
            direct_output_extension(&no_fmt, Some(&rembg), Path::new("/data/photo.jpg")),
            Some("jpg".to_string())
        );
        let denoise = cap_decl(DataType::Audio, DataType::Audio);
        assert_eq!(
            direct_output_extension(&no_fmt, Some(&denoise), Path::new("/data/in.flac")),
            Some("flac".to_string())
        );
        // video/file 跨格式无固定扩展名 → 同样回退输入扩展名
        let split = cap_decl(DataType::Video, DataType::File);
        assert_eq!(
            direct_output_extension(&no_fmt, Some(&split), Path::new("/data/v.mkv")),
            Some("mkv".to_string())
        );
    }

    #[test]
    fn direct_output_extension_no_capability_and_sanitize() {
        let no_fmt = serde_json::json!({});
        // 无 capability 声明 → 输入扩展名（D-7 现行为）
        assert_eq!(
            direct_output_extension(&no_fmt, None, Path::new("/tmp/in.wav")),
            Some("wav".to_string())
        );
        // 无扩展名且无映射可用 → None（调用方不带 extension 参数）
        assert_eq!(direct_output_extension(&no_fmt, None, Path::new("/data/noext")), None);
        // 字符清洗：output_format 含非法字符仅留字母数字
        let dirty = serde_json::json!({ "output_format": "s.r/t" });
        assert_eq!(
            direct_output_extension(&dirty, None, Path::new("/data/in.wav")),
            Some("srt".to_string())
        );
    }

    // ── §5.5/§5.6：file_archive / file_gate 端口与静态校验 ──────────────

    #[test]
    fn test_file_gate_has_any_condition() {
        // 无任何条件 / 空 media 对象 → false
        assert!(!file_gate_has_any_condition(&serde_json::json!({})));
        assert!(!file_gate_has_any_condition(&serde_json::json!({"media": {}})));
        // 空数组不算条件
        assert!(!file_gate_has_any_condition(&serde_json::json!({
            "extensions": []
        })));
        // 各类条件成立
        assert!(file_gate_has_any_condition(&serde_json::json!({
            "extensions": ["txt"]
        })));
        assert!(file_gate_has_any_condition(&serde_json::json!({
            "extensions_exclude": ["tmp"]
        })));
        assert!(file_gate_has_any_condition(&serde_json::json!({
            "min_size_bytes": 1
        })));
        assert!(file_gate_has_any_condition(&serde_json::json!({
            "max_size_bytes": 100
        })));
        assert!(file_gate_has_any_condition(&serde_json::json!({
            "filename_regex": "^a"
        })));
        assert!(file_gate_has_any_condition(&serde_json::json!({
            "media": {"min_width": 10}
        })));
        // filename_regex 空白串不算条件
        assert!(!file_gate_has_any_condition(&serde_json::json!({
            "filename_regex": "   "
        })));
    }

    /// file_gate 零条件件 → GateNoConditions 校验错误；file→file 边合法。
    #[test]
    fn test_file_gate_validation_requires_condition() {
        let bad = r#"
[pipeline]
id = "gate-bad"
name = "Gate No Conditions"

[[nodes]]
id = "in"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "gate"
kind = "builtin"
builtin = "file_gate"
params = {}

[[edges]]
from = ["in", "output"]
to = ["gate", "input"]
"#;
        let pipeline = Pipeline::from_toml_str(bad).unwrap();
        let errs = pipeline.validate().unwrap_err();
        assert!(
            errs.iter().any(|e| e.to_string().contains(
                "must configure at least one filter condition"
            )),
            "expected GateNoConditions, got: {errs:?}"
        );

        // 至少一项条件 + file→file 边 → 校验通过
        let ok = r#"
[pipeline]
id = "gate-ok"
name = "Gate With Conditions"

[[nodes]]
id = "in"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "gate"
kind = "builtin"
builtin = "file_gate"
params = { extensions = ["txt"] }

[[edges]]
from = ["in", "output"]
to = ["gate", "input"]
"#;
        let pipeline = Pipeline::from_toml_str(ok).unwrap();
        assert!(pipeline.validate().is_ok());
    }
}
