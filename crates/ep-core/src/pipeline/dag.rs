//! DAG 数据结构 + 验证 + 拓扑排序

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

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
        /// `skip_serializing_if`：TOML 无 null，None 时不写出该键（反序列化行为不变）
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_id: Option<String>,
    },
    /// 内置工具节点
    Builtin { builtin: String },
    /// 外部 API 调用
    ExternalApi {
        endpoint: String,
        #[serde(default = "default_api_type")]
        api_type: String,
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

fn default_api_type() -> String {
    "openai".to_string()
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
                api_type: "openai".to_string(),
                api_key_env: Some("MY_API_KEY".to_string()),
            }
        );
        assert!(pipeline.validate().is_ok());
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
