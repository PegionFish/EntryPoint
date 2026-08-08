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
use std::sync::atomic::AtomicUsize;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use ep_core::pipeline::dag::{Edge, NodeKind, NodePosition, Pipeline, PipelineNode};

/// 原子写临时文件命名序号（进程内自增，配合 PID 保证并发 save_spec 不撞名）
static SAVE_SEQ: AtomicUsize = AtomicUsize::new(0);

// ─── spec 数据结构（前端契约，冻结） ─────────────────────────────────────────

/// 管线元信息（对应 spec JSON 的顶层 `pipeline` 字段 / TOML 的 `[pipeline]` 段）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipelineMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// 管线级并发上限（§6.8）：null/缺省 = 跟随全局 `max_parallel`。
    /// TOML `[pipeline]` 段 `max_instances` 键，执行层（B3）消费。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_instances: Option<u32>,
    /// 管线级节点硬超时缺省（秒，缺陷 #3）：null/缺省 = 跟随全局配置。
    /// TOML `[pipeline]` 段 `node_timeout_secs` 键，执行层消费（长媒体管线据此放宽）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_timeout_secs: Option<u32>,
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
    /// builtin 节点的工具名（kind=builtin 时必填；LLM 节点为 `llm`，
    /// `external_api` 为可执行别名，见 §6.7）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin: Option<String>,
    /// module 节点的模块 id（kind=module 时必填）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_id: Option<String>,
    /// module 节点的 capability（kind=module 时必填）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    /// module 节点变体 pin（§6.2 冻结字段 `model`；null = 跟随激活变体）。
    /// 旧 TOML 键 `model_id` 由 ep-core 反序列化层以 alias 兼容。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// module 节点设备绑定（§6.2 软约束：`"auto"` | `"cuda:0"` | …；
    /// 加载/导入时本机无此设备 → 警告回退 auto，不硬失败）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// 任意参数（JSON 对象，可含嵌套对象/数组）
    #[serde(default = "default_params")]
    pub params: JsonValue,
    /// React Flow 画布坐标（可选）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<NodePosition>,
    /// 节点级超时（秒）— P1-11：透传至执行器（不再丢弃）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u32>,
    /// 节点级重试次数 — P1-11：透传至执行器（不再丢弃）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<u32>,
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
///
/// 错误消息为英文技术细节（供日志与 API 层 `{{detail}}` 透传）。
pub fn load_spec(path: &Path) -> Result<PipelineSpec> {
    let pipeline = ep_core::pipeline::load_pipeline(path)
        .with_context(|| format!("failed to load pipeline file `{}`", path.display()))?;
    pipeline_to_spec(&pipeline)
}

/// spec → TOML：结构校验后落盘（自动创建父目录）。
///
/// P2 修复：tmp + rename 原子写——先写同目录临时文件再 rename 覆盖目标，
/// 崩溃/写中断不会留下半个管线文件（旧实现直接 fs::write 原地截断重写）。
pub fn save_spec(spec: &PipelineSpec, path: &Path) -> Result<()> {
    validate_spec(spec)?;
    let text = spec_to_toml(spec)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory `{}`", parent.display()))?;
    }
    // 同目录临时文件（与目标同文件系统 → rename 原子），进程内唯一防并发碰撞
    let tmp = path.with_extension(format!(
        "toml.tmp{}{}",
        std::process::id(),
        SAVE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&tmp, &text)
        .with_context(|| format!("failed to write pipeline file `{}`", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp); // 尽力清理残留临时文件
        return Err(e).with_context(|| format!("failed to finalize pipeline file `{}`", path.display()));
    }
    Ok(())
}

/// spec → ep-core `Pipeline`（供执行层使用）。**签名冻结（W2-D 依赖）。**
///
/// 内含完整结构校验；失败返回英文技术错误（API 层经 i18n 前缀 + `{{detail}}`
/// 包装后呈现给用户）。
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
        max_instances: spec.pipeline.max_instances,
        node_timeout_secs: spec.pipeline.node_timeout_secs,
    })
}

/// 结构校验（不校验执行语义，如 file_input 存在性——那是执行层的职责）：
/// - 元信息非空；nodes 非空；节点 id 非空且唯一
/// - builtin/module 节点的必填字段齐全
/// - params 必须是对象（允许 null，按空对象处理）
/// - edges 引用的节点必须存在、端口非空
pub fn validate_spec(spec: &PipelineSpec) -> Result<()> {
    if spec.pipeline.id.trim().is_empty() {
        bail!("pipeline id must not be empty");
    }
    if spec.pipeline.name.trim().is_empty() {
        bail!("pipeline name must not be empty");
    }
    if spec.nodes.is_empty() {
        bail!("pipeline must have at least one node");
    }

    let mut seen = HashSet::new();
    for node in &spec.nodes {
        if node.id.trim().is_empty() {
            bail!("node id must not be empty");
        }
        if !seen.insert(node.id.as_str()) {
            bail!("duplicate node id: `{}`", node.id);
        }
        match node.kind {
            SpecNodeKind::Builtin => {
                if node.builtin.as_deref().unwrap_or("").trim().is_empty() {
                    bail!("builtin node `{}` is missing the `builtin` field", node.id);
                }
            }
            SpecNodeKind::Module => {
                if node.module_id.as_deref().unwrap_or("").trim().is_empty() {
                    bail!("module node `{}` is missing the `module_id` field", node.id);
                }
                if node.capability.as_deref().unwrap_or("").trim().is_empty() {
                    bail!("module node `{}` is missing the `capability` field", node.id);
                }
            }
        }
        if !(node.params.is_object() || node.params.is_null()) {
            bail!("params of node `{}` must be a JSON object", node.id);
        }
        // §6.2 可选字段：允许缺省；出现则必须非空（空串无语义）
        if node.model.as_deref().map(str::trim) == Some("") {
            bail!("model of node `{}` must not be empty when present", node.id);
        }
        if node.device.as_deref().map(str::trim) == Some("") {
            bail!("device of node `{}` must not be empty when present", node.id);
        }
    }

    for edge in &spec.edges {
        for (node_id, port) in [&edge.from, &edge.to] {
            if !seen.contains(node_id.as_str()) {
                bail!("edge references a non-existent node: `{node_id}`");
            }
            if port.trim().is_empty() {
                bail!("edge port of node `{node_id}` must not be empty");
            }
        }
    }
    Ok(())
}

/// ep-core `Pipeline` → spec（load_spec 的转换核心，亦可独立复用）。
///
/// 节点种类映射：
/// - builtin / module 节点按原样转换（module 节点含 §6.2 `model`/`device`）
/// - 遗留 `external_api` kind 节点（含 `kind = "llm"` 别名形状）转换为
///   builtin `llm` 节点（§6.7：LLM 是 builtin，旧名保留为 alias），
///   kind 级 `endpoint`/`api_key_env` 并入 params（`endpoint` → `base_url`）
///
/// `timeout_secs` / `retry_count` 全量透传（P1-11，执行器消费）。
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
            max_instances: pipeline.max_instances,
            node_timeout_secs: pipeline.node_timeout_secs,
        },
        nodes,
        edges: pipeline.edges.clone(),
    })
}

// ─── 节点双向转换 ────────────────────────────────────────────────────────────

fn node_to_spec(node: &PipelineNode) -> Result<SpecNode> {
    let (kind, builtin, module_id, capability, model, device, params) = match &node.kind {
        NodeKind::Builtin { builtin } => (
            SpecNodeKind::Builtin,
            Some(builtin.clone()),
            None,
            None,
            None,
            None,
            node.params.clone(),
        ),
        NodeKind::Module {
            module_id,
            capability,
            model_id,
            device,
        } => (
            SpecNodeKind::Module,
            None,
            Some(module_id.clone()),
            Some(capability.clone()),
            model_id.clone(),
            device.clone(),
            node.params.clone(),
        ),
        // 遗留 external_api/llm kind → builtin llm（§6.7），kind 级字段并入 params
        NodeKind::ExternalApi {
            endpoint,
            api_key_env,
        } => {
            let mut params = if node.params.is_null() {
                default_params()
            } else {
                node.params.clone()
            };
            if !params.is_object() {
                params = default_params();
            }
            let obj = params
                .as_object_mut()
                .expect("params ensured to be an object above");
            if !endpoint.is_empty() {
                obj.insert("base_url".to_string(), JsonValue::String(endpoint.clone()));
            }
            if let Some(env_name) = api_key_env {
                obj.insert("api_key_env".to_string(), JsonValue::String(env_name.clone()));
            }
            (
                SpecNodeKind::Builtin,
                Some("llm".to_string()),
                None,
                None,
                None,
                None,
                params,
            )
        }
    };

    Ok(SpecNode {
        id: node.id.clone(),
        label: node.label.clone(),
        kind,
        builtin,
        module_id,
        capability,
        model,
        device,
        params: if params.is_null() { default_params() } else { params },
        position: node.position.clone(),
        timeout_secs: node.timeout_secs,
        retry_count: node.retry_count,
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
            model_id: node.model.clone(),
            device: node.device.clone(),
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
        timeout_secs: node.timeout_secs,
        retry_count: node.retry_count,
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
    // §6.8 管线级并发上限（缺省不写出该键）
    if let Some(max_instances) = spec.pipeline.max_instances {
        out.push_str(&format!("max_instances = {max_instances}\n"));
    }
    // 缺陷 #3：管线级节点硬超时缺省（缺省不写出该键）
    if let Some(node_timeout_secs) = spec.pipeline.node_timeout_secs {
        out.push_str(&format!("node_timeout_secs = {node_timeout_secs}\n"));
    }

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
                // §6.2 变体 pin / 设备绑定（对外契约键名 `model` / `device`）
                if let Some(model) = &node.model {
                    out.push_str(&format!("model = {}\n", toml_string(model)));
                }
                if let Some(device) = &node.device {
                    out.push_str(&format!("device = {}\n", toml_string(device)));
                }
            }
        }
        if !node.label.is_empty() {
            out.push_str(&format!("label = {}\n", toml_string(&node.label)));
        }
        if let Some(position) = &node.position {
            // NodePosition 序列化为 {"x": .., "y": ..} → 行内表
            let v = serde_json::to_value(position)
                .expect("NodePosition serialization cannot fail");
            out.push_str(&format!("position = {}\n", toml_value(&v)?));
        }
        // 节点级超时/重试（P1-11 透传，执行器消费）
        if let Some(t) = node.timeout_secs {
            out.push_str(&format!("timeout_secs = {t}\n"));
        }
        if let Some(r) = node.retry_count {
            out.push_str(&format!("retry_count = {r}\n"));
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
        JsonValue::Null => bail!("null values are not allowed in params (TOML has no null)"),
        JsonValue::Bool(b) => Ok(if *b { "true".into() } else { "false".into() }),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                return Ok(i.to_string());
            }
            // P3：u64 超出 i64::MAX 时 as_i64 为 None，若走 as_f64 会丢精度
            // （>2^53 的整数值无法精确表示）——先按 u64 原样输出
            if let Some(u) = n.as_u64() {
                return Ok(u.to_string());
            }
            let f = n
                .as_f64()
                .ok_or_else(|| anyhow!("number cannot be represented: {n}"))?;
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
                max_instances: None,
                node_timeout_secs: None,
            },
            nodes: vec![
                SpecNode {
                    id: "input".into(),
                    label: "输入".into(),
                    kind: SpecNodeKind::Builtin,
                    builtin: Some("file_input".into()),
                    module_id: None,
                    capability: None,
                    model: None,
                    device: None,
                    params: json!({}),
                    position: Some(NodePosition { x: 10.0, y: 20.0 }),
                    timeout_secs: None,
                    retry_count: None,
                },
                SpecNode {
                    id: "asr".into(),
                    label: "识别".into(),
                    kind: SpecNodeKind::Module,
                    builtin: None,
                    module_id: Some("faster-whisper".into()),
                    capability: Some("transcribe".into()),
                    model: None,
                    device: None,
                    params: json!({ "language": "zh", "nested": {"a": [1, 2.5, true], "s": "文本 \"引号\"" } }),
                    position: None,
                    timeout_secs: None,
                    retry_count: None,
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
                device: None,
            }
        );
        assert_eq!(
            pipeline.nodes[0].position,
            Some(NodePosition { x: 10.0, y: 20.0 })
        );
    }

    // ── §6.2 model/device + P1-11 timeout/retry 透传 ────────────────────────

    #[test]
    fn test_model_device_timeout_retry_roundtrip() {
        let mut spec = sample_spec_body("schema-pipe");
        // module 节点带 model/device/timeout/retry；builtin 节点带 timeout/retry
        spec.nodes[1].model = Some("ep.systran.faster-whisper@medium".into());
        spec.nodes[1].device = Some("cuda:0".into());
        spec.nodes[1].timeout_secs = Some(600);
        spec.nodes[1].retry_count = Some(2);
        spec.nodes[0].timeout_secs = Some(60);

        // spec → Pipeline：字段全量透传
        let pipeline = spec_to_pipeline(&spec).expect("schema spec should convert");
        assert_eq!(
            pipeline.nodes[1].kind,
            NodeKind::Module {
                module_id: "faster-whisper".into(),
                capability: "transcribe".into(),
                model_id: Some("ep.systran.faster-whisper@medium".into()),
                device: Some("cuda:0".into()),
            }
        );
        assert_eq!(pipeline.nodes[1].timeout_secs, Some(600));
        assert_eq!(pipeline.nodes[1].retry_count, Some(2));
        assert_eq!(pipeline.nodes[0].timeout_secs, Some(60));
        assert_eq!(pipeline.nodes[0].retry_count, None);

        // spec → TOML 文本：新契约键落盘（model/device，而非 model_id）
        let toml_text = spec_to_toml(&spec).unwrap();
        assert!(toml_text.contains("model = \"ep.systran.faster-whisper@medium\""));
        assert!(toml_text.contains("device = \"cuda:0\""));
        assert!(toml_text.contains("timeout_secs = 600"));
        assert!(toml_text.contains("retry_count = 2"));
        assert!(!toml_text.contains("model_id ="));

        // TOML → 磁盘 → load_spec 往返等价
        let dir = temp_dir("schema");
        let out = dir.join("schema.toml");
        save_spec(&spec, &out).expect("save with schema fields");
        let back = load_spec(&out).expect("reload schema toml");
        assert_eq!(spec, back, "model/device/timeout/retry must roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_legacy_model_id_toml_loads_as_model() {
        // 旧 TOML 键 model_id 经 ep-core alias 读取，在 spec 契约中呈现为 model
        let dir = temp_dir("legacy-model");
        let path = dir.join("legacy.toml");
        std::fs::write(
            &path,
            r#"
[pipeline]
id = "legacy"
name = "Legacy"

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
"#,
        )
        .unwrap();

        let spec = load_spec(&path).expect("legacy model_id toml should load");
        assert_eq!(spec.nodes[1].model.as_deref(), Some("large-v3"));

        // 保存后统一为新契约键 model
        let out = dir.join("migrated.toml");
        save_spec(&spec, &out).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains("model = \"large-v3\""));
        assert!(!text.contains("model_id"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_external_api_kind_loads_as_builtin_llm() {
        // 遗留 kind = external_api（P2-13 清理后仅剩 endpoint/api_key_env）
        // 在 spec 契约中呈现为 builtin llm，kind 级字段并入 params
        let dir = temp_dir("extapi");
        let path = dir.join("extapi.toml");
        std::fs::write(
            &path,
            r#"
[pipeline]
id = "extapi"
name = "Legacy external_api"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "translate"
kind = "external_api"
endpoint = "https://api.openai.com/v1"
api_type = "openai"
api_key_env = "OPENAI_API_KEY"

[nodes.params]
model = "gpt-4o-mini"
system_prompt = "翻译：{input}"
"#,
        )
        .unwrap();

        let spec = load_spec(&path).expect("legacy external_api toml should load");
        let node = &spec.nodes[1];
        assert_eq!(node.kind, SpecNodeKind::Builtin);
        assert_eq!(node.builtin.as_deref(), Some("llm"));
        // kind 级字段并入 params（endpoint → base_url）
        assert_eq!(node.params["base_url"], "https://api.openai.com/v1");
        assert_eq!(node.params["api_key_env"], "OPENAI_API_KEY");
        assert_eq!(node.params["model"], "gpt-4o-mini");

        // 保存 → 再加载等价（此后一律 builtin llm 形状）
        let out = dir.join("migrated.toml");
        save_spec(&spec, &out).unwrap();
        let back = load_spec(&out).unwrap();
        assert_eq!(spec, back);
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains("kind = \"builtin\""));
        assert!(text.contains("builtin = \"llm\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_validate_rejects_empty_model_or_device() {
        let mut spec = sample_spec_body("empty-model");
        spec.nodes[1].model = Some("   ".into());
        let err = spec_to_pipeline(&spec).unwrap_err().to_string();
        assert!(err.contains("model") && err.contains("empty"), "got: {err}");

        let mut spec = sample_spec_body("empty-device");
        spec.nodes[1].device = Some("".into());
        let err = spec_to_pipeline(&spec).unwrap_err().to_string();
        assert!(err.contains("device") && err.contains("empty"), "got: {err}");
    }

    // ── §6.8 max_instances：桥接往返不丢失 ──────────────────────────────────

    #[test]
    fn test_max_instances_roundtrip() {
        let mut spec = sample_spec_body("mi-pipe");
        spec.pipeline.max_instances = Some(1); // GPU 重管线锁 1

        // spec → TOML 文本：[pipeline] 段写出该键
        let toml_text = spec_to_toml(&spec).unwrap();
        assert!(toml_text.contains("max_instances = 1"), "got: {toml_text}");

        // spec → Pipeline：执行层可见
        let pipeline = spec_to_pipeline(&spec).unwrap();
        assert_eq!(pipeline.max_instances, Some(1));

        // 保存 → 重读：spec 完全等价（B3 报告指出的丢弃问题修复）
        let dir = temp_dir("mi");
        let out = dir.join("mi.toml");
        save_spec(&spec, &out).expect("save with max_instances");
        let back = load_spec(&out).expect("reload max_instances toml");
        assert_eq!(spec, back, "max_instances must survive save/load");
        assert_eq!(back.pipeline.max_instances, Some(1));

        // ep-core 直读同样可见
        let via_core = ep_core::pipeline::load_pipeline(&out).unwrap();
        assert_eq!(via_core.max_instances, Some(1));

        // 缺省时不写键，重读为 None
        let mut spec_none = sample_spec_body("mi-none");
        spec_none.pipeline.max_instances = None;
        let toml_none = spec_to_toml(&spec_none).unwrap();
        assert!(!toml_none.contains("max_instances"), "got: {toml_none}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_spec_to_pipeline_structural_errors() {
        // 技术层错误消息一律英文（API 层经 i18n 前缀 + {{detail}} 包装）
        // 空节点列表
        let mut spec = sample_spec_body("p1");
        spec.nodes.clear();
        spec.edges.clear();
        let err = spec_to_pipeline(&spec).unwrap_err().to_string();
        assert!(err.contains("at least one node"), "got: {err}");

        // 重复节点 id
        let mut spec = sample_spec_body("p2");
        spec.nodes.push(spec.nodes[0].clone());
        let err = spec_to_pipeline(&spec).unwrap_err().to_string();
        assert!(err.contains("duplicate node id"), "got: {err}");

        // 边引用不存在的节点
        let mut spec = sample_spec_body("p3");
        spec.edges.push(Edge {
            from: ("input".into(), "output".into()),
            to: ("ghost".into(), "input".into()),
        });
        let err = spec_to_pipeline(&spec).unwrap_err().to_string();
        assert!(err.contains("non-existent node") && err.contains("ghost"), "got: {err}");

        // builtin 节点缺 builtin 字段
        let mut spec = sample_spec_body("p4");
        spec.nodes[0].builtin = None;
        let err = spec_to_pipeline(&spec).unwrap_err().to_string();
        assert!(err.contains("missing the `builtin` field"), "got: {err}");

        // module 节点缺 capability
        let mut spec = sample_spec_body("p5");
        spec.nodes[1].capability = None;
        let err = spec_to_pipeline(&spec).unwrap_err().to_string();
        assert!(err.contains("missing the `capability` field"), "got: {err}");
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
        assert_eq!(
            toml_value(&json!(null)).err().unwrap().to_string(),
            "null values are not allowed in params (TOML has no null)"
        );
        assert_eq!(toml_key("plain_key-1"), "plain_key-1");
        assert_eq!(toml_key("has space"), "\"has space\"");
        assert_eq!(toml_string("换行\n与\"引号\""), "\"换行\\n与\\\"引号\\\"\"");
    }

    #[test]
    fn test_load_spec_missing_file_errors_in_english() {
        let err = load_spec(Path::new("/nonexistent/dir/pipe.toml"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("failed to load pipeline file"), "got: {err}");
    }

    // ── P2/P3：原子写不留半成品、大 u64 精度无损 ───────────────────────────

    #[test]
    fn test_save_spec_atomic_write_leaves_no_tmp_and_loads_back() {
        let dir = temp_dir("atomic");
        let target = dir.join("atomic.toml");
        let spec = sample_spec_body("atomic-pipe");
        save_spec(&spec, &target).expect("保存应成功");
        // 无残留临时文件（tmp 命名含 PID + 序号）
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .contains("atomic.toml.tmp")
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "原子写不得残留临时文件: {leftovers:?}"
        );
        // 目标文件可正常读回
        let back = load_spec(&target).expect("保存后的文件应可加载");
        assert_eq!(back, spec);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_toml_value_large_u64_exact() {
        // u64::MAX 超出 i64::MAX 且超出 f64 精确范围（2^53）：必须原样输出
        // （旧实现 as_i64 失败后走 as_f64 → 精度丢失）
        let big = u64::MAX;
        assert_eq!(toml_value(&json!(big)).unwrap(), big.to_string());
        // 边界：i64::MAX 仍走 i64 分支
        assert_eq!(toml_value(&json!(i64::MAX)).unwrap(), i64::MAX.to_string());
        // 超过 u64 的数值（如 1e100 浮点字面量）回退浮点路径
        assert!(toml_value(&serde_json::Number::from_f64(1e100).map(serde_json::Value::Number).unwrap()).is_ok());
    }
}
