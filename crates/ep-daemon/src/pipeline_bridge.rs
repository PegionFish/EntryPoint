//! React Flow spec JSON ↔ ep-core 管线 TOML 桥接
//!
//! ⚠ 文件所有权：Wave 2 代理 W2-B（管线 CRUD）。
//! W2-D（execute.rs）依赖 [`spec_to_pipeline`] 与 [`PipelineSpec`]，签名已冻结。
//!
//! 本文件由 `api/pipelines.rs` 通过 `#[path]` 声明为模块（ep-daemon 为纯 bin crate，
//! main.rs 非本代理所有，无法在其中加 `mod pipeline_bridge;`）。
//! 外部使用方请经 `crate::api::pipelines::pipeline_bridge::*` 访问。
//!
//! ## 设计要点
//! - **spec 结构**（`PipelineSpec`）与前端契约逐字段一致（蛇形命名）；
//!   `params` 用 `serde_json::Value` 承载任意参数。
//! - **加载**（TOML→spec）：复用 `ep_core::pipeline::Pipeline::from_toml`
//!   （ep-daemon 未直接依赖 `toml` crate，解析能力全部来自 ep-core）。
//! - **保存**（spec→TOML）：ep-daemon 无 `toml` 依赖，故内置一个最小 TOML 输出器
//!   （只覆盖管线文件布局：`[pipeline]` + `[[nodes]]` + `[[edges]]`）。
//!   `params` 与 `position` 以行内表（inline table）写出，任意嵌套深度均可表达；
//!   写出的文件可被 `ep_core::pipeline::load_pipeline` 原样读回。
//! - **语义边界**：仅做结构校验（节点非空、id 唯一、边引用存在等）；
//!   「至少一个 file_input」等执行语义校验留给执行层（`Pipeline::validate`）。

use std::collections::HashSet;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use ep_core::pipeline::dag::{Edge, NodeKind, NodePosition, Pipeline, PipelineNode};

// ─── spec 数据结构（前端契约，冻结） ─────────────────────────────────────────

/// 管线元信息（对应 spec JSON 的顶层 `pipeline` 字段 / TOML 的 `[pipeline]` 段）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipelineMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// spec 节点类型判别（前端契约 `kind: "builtin" | "module"`）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpecNodeKind {
    Builtin,
    Module,
}

/// spec 节点（与前端 React Flow 节点字段一一对应）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpecNode {
    pub id: String,
    #[serde(default)]
    pub label: String,
    pub kind: SpecNodeKind,
    /// builtin 节点的工具名（kind=builtin 时必填）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin: Option<String>,
    /// module 节点的模块 id（kind=module 时必填）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_id: Option<String>,
    /// module 节点的 capability（kind=module 时必填）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    /// 任意参数（JSON 对象，可含嵌套对象/数组）
    #[serde(default = "default_params")]
    pub params: JsonValue,
    /// React Flow 画布坐标（可选）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<NodePosition>,
}

/// 边：`{from: [node_id, port], to: [node_id, port]}`。
/// 直接复用 `dag::Edge`（二元组序列化即 `[node, port]` 数组，与契约一致）。
pub type SpecEdge = Edge;

/// 管线 spec（前端 GET/PUT /api/pipelines/:id 的 body 形状）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipelineSpec {
    pub pipeline: PipelineMeta,
    pub nodes: Vec<SpecNode>,
    #[serde(default)]
    pub edges: Vec<SpecEdge>,
}

fn default_params() -> JsonValue {
    JsonValue::Object(Default::default())
}

// ─── 公共 API ────────────────────────────────────────────────────────────────

/// TOML → spec：从文件加载管线定义并转为前端 spec 形状。
pub fn load_spec(path: &Path) -> Result<PipelineSpec> {
    let pipeline = ep_core::pipeline::load_pipeline(path)
        .with_context(|| format!("管线文件 `{}` 加载失败", path.display()))?;
    pipeline_to_spec(&pipeline)
}

/// spec → TOML：结构校验后落盘（自动创建父目录）。
pub fn save_spec(spec: &PipelineSpec, path: &Path) -> Result<()> {
    validate_spec(spec)?;
    let text = spec_to_toml(spec)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建目录 `{}` 失败", parent.display()))?;
    }
    std::fs::write(path, text)
        .with_context(|| format!("写入管线文件 `{}` 失败", path.display()))?;
    Ok(())
}

/// spec → ep-core `Pipeline`（供执行层使用）。**签名冻结（W2-D 依赖）。**
///
/// 内含完整结构校验；失败返回中文错误。
pub fn spec_to_pipeline(spec: &PipelineSpec) -> Result<Pipeline> {
    validate_spec(spec)?;

    let nodes = spec
        .nodes
        .iter()
        .map(node_from_spec)
        .collect::<Result<Vec<_>>>()?;

    Ok(Pipeline {
        id: spec.pipeline.id.clone(),
        name: spec.pipeline.name.clone(),
        description: spec.pipeline.description.clone(),
        nodes,
        edges: spec.edges.clone(),
    })
}

/// 结构校验（不校验执行语义，如 file_input 存在性——那是执行层的职责）：
/// - 元信息非空；nodes 非空；节点 id 非空且唯一
/// - builtin/module 节点的必填字段齐全
/// - params 必须是对象（允许 null，按空对象处理）
/// - edges 引用的节点必须存在、端口非空
pub fn validate_spec(spec: &PipelineSpec) -> Result<()> {
    if spec.pipeline.id.trim().is_empty() {
        bail!("管线 id 不能为空");
    }
    if spec.pipeline.name.trim().is_empty() {
        bail!("管线名称不能为空");
    }
    if spec.nodes.is_empty() {
        bail!("管线至少要有一个节点");
    }

    let mut seen = HashSet::new();
    for node in &spec.nodes {
        if node.id.trim().is_empty() {
            bail!("节点 id 不能为空");
        }
        if !seen.insert(node.id.as_str()) {
            bail!("节点 id 重复: `{}`", node.id);
        }
        match node.kind {
            SpecNodeKind::Builtin => {
                if node.builtin.as_deref().unwrap_or("").trim().is_empty() {
                    bail!("builtin 节点 `{}` 缺少 builtin 字段", node.id);
                }
            }
            SpecNodeKind::Module => {
                if node.module_id.as_deref().unwrap_or("").trim().is_empty() {
                    bail!("module 节点 `{}` 缺少 module_id 字段", node.id);
                }
                if node.capability.as_deref().unwrap_or("").trim().is_empty() {
                    bail!("module 节点 `{}` 缺少 capability 字段", node.id);
                }
            }
        }
        if !(node.params.is_object() || node.params.is_null()) {
            bail!("节点 `{}` 的 params 必须是 JSON 对象", node.id);
        }
    }

    for edge in &spec.edges {
        for (node_id, port) in [&edge.from, &edge.to] {
            if !seen.contains(node_id.as_str()) {
                bail!("边引用了不存在的节点: `{node_id}`");
            }
            if port.trim().is_empty() {
                bail!("节点 `{node_id}` 的边端口不能为空");
            }
        }
    }
    Ok(())
}

/// ep-core `Pipeline` → spec（load_spec 的转换核心，亦可独立复用）。
///
/// 仅支持 builtin / module 两类节点；external_api 节点不在前端契约内，报错。
/// `timeout_secs` / `retry_count` 不在前端契约中，转换时丢弃（执行层用默认值）。
pub fn pipeline_to_spec(pipeline: &Pipeline) -> Result<PipelineSpec> {
    let nodes = pipeline
        .nodes
        .iter()
        .map(node_to_spec)
        .collect::<Result<Vec<_>>>()?;

    Ok(PipelineSpec {
        pipeline: PipelineMeta {
            id: pipeline.id.clone(),
            name: pipeline.name.clone(),
            description: pipeline.description.clone(),
        },
        nodes,
        edges: pipeline.edges.clone(),
    })
}

// ─── 节点双向转换 ────────────────────────────────────────────────────────────

fn node_to_spec(node: &PipelineNode) -> Result<SpecNode> {
    let (kind, builtin, module_id, capability) = match &node.kind {
        NodeKind::Builtin { builtin } => (
            SpecNodeKind::Builtin,
            Some(builtin.clone()),
            None,
            None,
        ),
        NodeKind::Module {
            module_id,
            capability,
            ..
        } => (
            SpecNodeKind::Module,
            None,
            Some(module_id.clone()),
            Some(capability.clone()),
        ),
        NodeKind::ExternalApi { .. } => {
            bail!("节点 `{}` 为 external_api 类型，不在前端 spec 契约内", node.id)
        }
    };

    Ok(SpecNode {
        id: node.id.clone(),
        label: node.label.clone(),
        kind,
        builtin,
        module_id,
        capability,
        params: if node.params.is_null() {
            default_params()
        } else {
            node.params.clone()
        },
        position: node.position.clone(),
    })
}

fn node_from_spec(node: &SpecNode) -> Result<PipelineNode> {
    let kind = match node.kind {
        SpecNodeKind::Builtin => NodeKind::Builtin {
            builtin: node.builtin.clone().unwrap_or_default(),
        },
        SpecNodeKind::Module => NodeKind::Module {
            module_id: node.module_id.clone().unwrap_or_default(),
            capability: node.capability.clone().unwrap_or_default(),
            model_id: None,
        },
    };

    Ok(PipelineNode {
        id: node.id.clone(),
        kind,
        label: node.label.clone(),
        params: if node.params.is_null() {
            default_params()
        } else {
            node.params.clone()
        },
        position: node.position.clone(),
        timeout_secs: None,
        retry_count: None,
    })
}

// ─── 最小 TOML 输出器（spec → 标准管线文件布局） ─────────────────────────────

/// spec → TOML 文本（`[pipeline]` + `[[nodes]]` + `[[edges]]`）。
fn spec_to_toml(spec: &PipelineSpec) -> Result<String> {
    let mut out = String::new();

    out.push_str("[pipeline]\n");
    out.push_str(&format!("id = {}\n", toml_string(&spec.pipeline.id)));
    out.push_str(&format!("name = {}\n", toml_string(&spec.pipeline.name)));
    out.push_str(&format!(
        "description = {}\n",
        toml_string(&spec.pipeline.description)
    ));

    for node in &spec.nodes {
        out.push_str("\n[[nodes]]\n");
        out.push_str(&format!("id = {}\n", toml_string(&node.id)));
        match node.kind {
            SpecNodeKind::Builtin => {
                out.push_str("kind = \"builtin\"\n");
                out.push_str(&format!(
                    "builtin = {}\n",
                    toml_string(node.builtin.as_deref().unwrap_or_default())
                ));
            }
            SpecNodeKind::Module => {
                out.push_str("kind = \"module\"\n");
                out.push_str(&format!(
                    "module_id = {}\n",
                    toml_string(node.module_id.as_deref().unwrap_or_default())
                ));
                out.push_str(&format!(
                    "capability = {}\n",
                    toml_string(node.capability.as_deref().unwrap_or_default())
                ));
            }
        }
        if !node.label.is_empty() {
            out.push_str(&format!("label = {}\n", toml_string(&node.label)));
        }
        if let Some(position) = &node.position {
            // NodePosition 序列化为 {"x": .., "y": ..} → 行内表
            let v = serde_json::to_value(position).expect("NodePosition 序列化不会失败");
            out.push_str(&format!("position = {}\n", toml_value(&v)?));
        }
        if let Some(obj) = node.params.as_object() {
            if !obj.is_empty() {
                out.push_str(&format!("params = {}\n", toml_value(&node.params)?));
            }
        }
    }

    for edge in &spec.edges {
        out.push_str("\n[[edges]]\n");
        out.push_str(&format!("from = [{}, {}]\n",
            toml_string(&edge.from.0),
            toml_string(&edge.from.1),
        ));
        out.push_str(&format!("to = [{}, {}]\n",
            toml_string(&edge.to.0),
            toml_string(&edge.to.1),
        ));
    }

    Ok(out)
}

/// JSON 值 → TOML 值文本（字符串/数字/布尔/数组/行内表）。
fn toml_value(v: &JsonValue) -> Result<String> {
    match v {
        JsonValue::Null => bail!("params 中不允许 null 值（TOML 无 null）"),
        JsonValue::Bool(b) => Ok(if *b { "true".into() } else { "false".into() }),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                return Ok(i.to_string());
            }
            let f = n.as_f64().ok_or_else(|| anyhow!("无法表示的数字: {n}"))?;
            if f.is_nan() {
                return Ok("nan".into());
            }
            if f.is_infinite() {
                return Ok(if f > 0.0 { "inf".into() } else { "-inf".into() });
            }
            let mut s = f.to_string();
            // TOML 浮点必须含小数点或指数；Rust 对整值浮点输出 "1e20" 或 "1"
            if !s.contains(['.', 'e', 'E']) {
                s.push_str(".0");
            }
            Ok(s)
        }
        JsonValue::String(s) => Ok(toml_string(s)),
        JsonValue::Array(items) => {
            let parts = items
                .iter()
                .map(toml_value)
                .collect::<Result<Vec<_>>>()?;
            Ok(format!("[{}]", parts.join(", ")))
        }
        JsonValue::Object(map) => {
            let mut parts = Vec::new();
            for (k, val) in map {
                parts.push(format!("{} = {}", toml_key(k), toml_value(val)?));
            }
            Ok(format!("{{ {} }}", parts.join(", ")))
        }
    }
}

/// TOML 基本字符串（带转义，含引号）
fn toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                if (c as u32) <= 0xffff {
                    out.push_str(&format!("\\u{:04x}", c as u32));
                } else {
                    out.push_str(&format!("\\U{:08x}", c as u32));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// TOML 键：裸键仅允许 [A-Za-z0-9_-]，否则加引号
fn toml_key(k: &str) -> String {
    let bare = !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare {
        k.to_string()
    } else {
        toml_string(k)
    }
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ep-bridge-{label}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 真实示例管线文件（相对本 crate 目录）
    fn sample_path(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/pipelines")
            .join(name)
    }

    fn sample_spec_body(id: &str) -> PipelineSpec {
        PipelineSpec {
            pipeline: PipelineMeta {
                id: id.into(),
                name: "测试管线".into(),
                description: "桥接测试".into(),
            },
            nodes: vec![
                SpecNode {
                    id: "input".into(),
                    label: "输入".into(),
                    kind: SpecNodeKind::Builtin,
                    builtin: Some("file_input".into()),
                    module_id: None,
                    capability: None,
                    params: json!({}),
                    position: Some(NodePosition { x: 10.0, y: 20.0 }),
                },
                SpecNode {
                    id: "asr".into(),
                    label: "识别".into(),
                    kind: SpecNodeKind::Module,
                    builtin: None,
                    module_id: Some("faster-whisper".into()),
                    capability: Some("transcribe".into()),
                    params: json!({ "language": "zh", "nested": {"a": [1, 2.5, true], "s": "文本 \"引号\"" } }),
                    position: None,
                },
            ],
            edges: vec![Edge {
                from: ("input".into(), "output".into()),
                to: ("asr".into(), "input".into()),
            }],
        }
    }

    // ── 往返测试：两个真实示例 TOML（load_spec → save_spec → load_spec 等价） ──

    #[test]
    fn test_roundtrip_audio_extract() {
        roundtrip_real_sample("audio_extract.toml", "audio-extract");
    }

    #[test]
    fn test_roundtrip_video_to_srt() {
        roundtrip_real_sample("video_to_srt.toml", "video-to-srt");
    }

    fn roundtrip_real_sample(file: &str, expected_id: &str) {
        let path = sample_path(file);
        let spec1 = load_spec(&path).expect("真实示例文件应可加载");
        assert_eq!(spec1.pipeline.id, expected_id);

        let dir = temp_dir("rt");
        let out = dir.join("saved.toml");
        save_spec(&spec1, &out).expect("保存不应失败");
        let spec2 = load_spec(&out).expect("保存后的文件应可再次加载");
        assert_eq!(spec1, spec2, "往返后 spec 必须完全等价");

        // 落盘文件也必须能被 ep-core 执行层直接加载
        let pipeline = ep_core::pipeline::load_pipeline(&out)
            .expect("保存的 TOML 必须能被 ep_core::load_pipeline 读取");
        let via_spec = spec_to_pipeline(&spec1).unwrap();
        assert_eq!(pipeline, via_spec, "TOML 直读与 spec 转换结果必须一致");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── spec_to_pipeline：成功与结构错误 ────────────────────────────────────

    #[test]
    fn test_spec_to_pipeline_success() {
        let spec = sample_spec_body("my-pipe");
        let pipeline = spec_to_pipeline(&spec).expect("合法 spec 应转换成功");
        assert_eq!(pipeline.id, "my-pipe");
        assert_eq!(pipeline.nodes.len(), 2);
        assert_eq!(pipeline.edges.len(), 1);
        assert_eq!(
            pipeline.nodes[0].kind,
            NodeKind::Builtin {
                builtin: "file_input".into()
            }
        );
        assert_eq!(
            pipeline.nodes[1].kind,
            NodeKind::Module {
                module_id: "faster-whisper".into(),
                capability: "transcribe".into(),
                model_id: None,
            }
        );
        assert_eq!(
            pipeline.nodes[0].position,
            Some(NodePosition { x: 10.0, y: 20.0 })
        );
    }

    #[test]
    fn test_spec_to_pipeline_structural_errors() {
        // 空节点列表
        let mut spec = sample_spec_body("p1");
        spec.nodes.clear();
        spec.edges.clear();
        let err = spec_to_pipeline(&spec).unwrap_err().to_string();
        assert!(err.contains("至少要有一个节点"), "got: {err}");

        // 重复节点 id
        let mut spec = sample_spec_body("p2");
        spec.nodes.push(spec.nodes[0].clone());
        let err = spec_to_pipeline(&spec).unwrap_err().to_string();
        assert!(err.contains("节点 id 重复"), "got: {err}");

        // 边引用不存在的节点
        let mut spec = sample_spec_body("p3");
        spec.edges.push(Edge {
            from: ("input".into(), "output".into()),
            to: ("ghost".into(), "input".into()),
        });
        let err = spec_to_pipeline(&spec).unwrap_err().to_string();
        assert!(err.contains("不存在的节点") && err.contains("ghost"), "got: {err}");

        // builtin 节点缺 builtin 字段
        let mut spec = sample_spec_body("p4");
        spec.nodes[0].builtin = None;
        let err = spec_to_pipeline(&spec).unwrap_err().to_string();
        assert!(err.contains("缺少 builtin 字段"), "got: {err}");

        // module 节点缺 capability
        let mut spec = sample_spec_body("p5");
        spec.nodes[1].capability = None;
        let err = spec_to_pipeline(&spec).unwrap_err().to_string();
        assert!(err.contains("缺少 capability 字段"), "got: {err}");
    }

    // ── save_spec / 数值与转义细节 ──────────────────────────────────────────

    #[test]
    fn test_save_creates_parent_dir_and_position_survives() {
        let dir = temp_dir("mk");
        let target = dir.join("nested/deep/pipe.toml");
        let spec = sample_spec_body("deep-pipe");
        save_spec(&spec, &target).expect("应自动创建父目录");
        let back = load_spec(&target).unwrap();
        assert_eq!(back.nodes[0].position, Some(NodePosition { x: 10.0, y: 20.0 }));
        // 嵌套 params（含中文、引号、混合数组）无损往返
        assert_eq!(back.nodes[1].params, spec.nodes[1].params);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_toml_value_number_and_key_rules() {
        assert_eq!(toml_value(&json!(42)).unwrap(), "42");
        assert_eq!(toml_value(&json!(2.5)).unwrap(), "2.5");
        // 整值浮点必须带小数点，否则 TOML 会按整数解析
        assert_eq!(toml_value(&json!(3.0)).unwrap(), "3.0");
        assert_eq!(toml_value(&json!(null)).err().unwrap().to_string(), "params 中不允许 null 值（TOML 无 null）");
        assert_eq!(toml_key("plain_key-1"), "plain_key-1");
        assert_eq!(toml_key("has space"), "\"has space\"");
        assert_eq!(toml_string("换行\n与\"引号\""), "\"换行\\n与\\\"引号\\\"\"");
    }

    #[test]
    fn test_load_spec_missing_file_errors_in_chinese() {
        let err = load_spec(Path::new("/nonexistent/dir/pipe.toml"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("加载失败"), "got: {err}");
    }
}
