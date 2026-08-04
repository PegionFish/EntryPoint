//! DAG 数据结构 + 验证 + 拓扑排序

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::types::DeviceId;

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
        })
    }

    /// 验证管线 DAG 合法性
    ///
    /// 检查：
    /// - 节点 id 唯一
    /// - 边引用的节点存在
    /// - 无环（拓扑排序检测）
    /// - 至少一个 file_input 节点
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
}
