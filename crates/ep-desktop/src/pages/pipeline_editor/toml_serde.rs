//! 管线 TOML 序列化（§6.2 文件形状）— 自 pipeline_editor.rs 拆分搬移，逻辑不变。

use ep_core::pipeline::dag::{NodeKind, Pipeline};

/// TOML 基本字符串转义：`\"` `\\` 控制字符（`\n` `\t` `\uXXXX`）
pub(super) fn toml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// TOML 浮点格式化：整数值补 `.0`（TOML 浮点语法要求小数点或指数）
pub(super) fn toml_float(f: f64) -> Result<String, String> {
    if !f.is_finite() {
        return Err(format!("non-finite float in params: {f}"));
    }
    if f.fract() == 0.0 && f.abs() < 9.0e15 {
        Ok(format!("{}.0", f as i64))
    } else {
        Ok(format!("{f}"))
    }
}

/// 发射 JSON 参数值为 TOML 值文本（标量 + 标量数组；嵌套结构不支持）。
///
/// 匿名访问 serde_json::Value（ep-desktop 无直接 serde_json 依赖）：
/// 经宏在调用点展开（表达式自带类型推断），null 键跳过。
fn emit_params_object(params: &ep_core::pipeline::dag::PipelineNode) -> Result<String, String> {
    let mut out = String::new();
    let Some(obj) = params.params.as_object() else {
        return Ok(out);
    };
    if obj.is_empty() {
        return Ok(out);
    }

    // 单值发射（宏：在调用点展开，借用表达式的类型推断，不命名 serde_json 类型）
    macro_rules! emit_scalar {
        ($v:expr) => {{
            if $v.is_null() {
                None
            } else if let Some(s) = $v.as_str() {
                Some(format!("\"{}\"", toml_escape(s)))
            } else if let Some(b) = $v.as_bool() {
                Some(b.to_string())
            } else if let Some(i) = $v.as_i64() {
                Some(i.to_string())
            } else if let Some(f) = $v.as_f64() {
                Some(toml_float(f)?)
            } else {
                return Err(
                    "unsupported param value (only scalar / array of scalars)".to_string(),
                );
            }
        }};
    }

    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort();
    for key in keys {
        let v = &obj[key];
        if v.is_null() {
            continue;
        }
        if let Some(arr) = v.as_array() {
            let mut items = Vec::new();
            for item in arr {
                if let Some(text) = emit_scalar!(item) {
                    items.push(text);
                }
            }
            out.push_str(&format!("{key} = [{}]\n", items.join(", ")));
        } else if let Some(text) = emit_scalar!(v) {
            out.push_str(&format!("{key} = {text}\n"));
        }
    }
    Ok(out)
}

/// 管线 → TOML 文本（`[pipeline]` + `[[nodes]]` + `[[edges]]` 文件形状；
/// §6.2 契约键：module 节点变体 pin 为 `model`；llm 节点优先 `kind = "llm"`）。
pub(super) fn pipeline_to_toml(p: &Pipeline) -> Result<String, String> {
    let mut out = String::new();
    out.push_str("[pipeline]\n");
    out.push_str(&format!("id = \"{}\"\n", toml_escape(&p.id)));
    out.push_str(&format!("name = \"{}\"\n", toml_escape(&p.name)));
    if !p.description.is_empty() {
        out.push_str(&format!(
            "description = \"{}\"\n",
            toml_escape(&p.description)
        ));
    }

    for node in &p.nodes {
        out.push_str("\n[[nodes]]\n");
        out.push_str(&format!("id = \"{}\"\n", toml_escape(&node.id)));
        match &node.kind {
            NodeKind::Module {
                module_id,
                capability,
                model_id,
                device,
            } => {
                out.push_str("kind = \"module\"\n");
                out.push_str(&format!("module_id = \"{}\"\n", toml_escape(module_id)));
                out.push_str(&format!("capability = \"{}\"\n", toml_escape(capability)));
                if let Some(m) = model_id {
                    out.push_str(&format!("model = \"{}\"\n", toml_escape(m)));
                }
                if let Some(d) = device {
                    out.push_str(&format!("device = \"{}\"\n", toml_escape(d)));
                }
            }
            NodeKind::Builtin { builtin } => {
                out.push_str("kind = \"builtin\"\n");
                out.push_str(&format!("builtin = \"{}\"\n", toml_escape(builtin)));
            }
            NodeKind::ExternalApi {
                endpoint,
                api_key_env,
            } => {
                if endpoint.is_empty() {
                    // §6.7 新命名：base_url 在 params 中声明
                    out.push_str("kind = \"llm\"\n");
                } else {
                    out.push_str("kind = \"external_api\"\n");
                    out.push_str(&format!("endpoint = \"{}\"\n", toml_escape(endpoint)));
                }
                if let Some(k) = api_key_env {
                    out.push_str(&format!("api_key_env = \"{}\"\n", toml_escape(k)));
                }
            }
        }
        if !node.label.is_empty() {
            out.push_str(&format!("label = \"{}\"\n", toml_escape(&node.label)));
        }
        if let Some(pos) = &node.position {
            out.push_str(&format!(
                "position = {{ x = {}, y = {} }}\n",
                toml_float(pos.x)?,
                toml_float(pos.y)?
            ));
        }
        if let Some(t) = node.timeout_secs {
            out.push_str(&format!("timeout_secs = {t}\n"));
        }
        if let Some(r) = node.retry_count {
            out.push_str(&format!("retry_count = {r}\n"));
        }
        let params_text = emit_params_object(node)?;
        if !params_text.is_empty() {
            out.push_str("\n[nodes.params]\n");
            out.push_str(&params_text);
        }
    }

    for edge in &p.edges {
        out.push_str("\n[[edges]]\n");
        out.push_str(&format!(
            "from = [\"{}\", \"{}\"]\n",
            toml_escape(&edge.from.0),
            toml_escape(&edge.from.1)
        ));
        out.push_str(&format!(
            "to = [\"{}\", \"{}\"]\n",
            toml_escape(&edge.to.0),
            toml_escape(&edge.to.1)
        ));
    }

    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ep_core::pipeline::dag::NodePosition;

    pub(crate) fn sample_toml() -> &'static str {
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

[[nodes]]
id = "process"
kind = "module"
module_id = "faster-whisper"
capability = "transcribe"
label = "Transcribe"
model = "ep.systran.faster-whisper@medium"
device = "cuda:0"

[nodes.params]
language = "zh"
beam_size = 5
vad_filter = true
threshold = 0.5
args = ["-i", "{input}"]

[[nodes]]
id = "translate"
kind = "llm"
api_key_env = "OPENAI_API_KEY"

[nodes.params]
base_url = "https://api.openai.com/v1"
model = "gpt-4o-mini"
temperature = 0.3

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
to = ["translate", "input"]

[[edges]]
from = ["translate", "output"]
to = ["save", "input"]
"#
    }

    #[test]
    fn toml_roundtrip_preserves_pipeline() {
        let pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        let text = pipeline_to_toml(&pipeline).expect("serialize");
        let again = Pipeline::from_toml_str(&text).expect("re-parse");
        assert_eq!(again.id, pipeline.id);
        assert_eq!(again.name, pipeline.name);
        assert_eq!(again.description, pipeline.description);
        assert_eq!(again.nodes, pipeline.nodes);
        assert_eq!(again.edges, pipeline.edges);
    }

    #[test]
    fn toml_module_node_uses_contract_keys() {
        let pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        let text = pipeline_to_toml(&pipeline).unwrap();
        // §6.2：变体 pin 键为 model（绝不输出旧键 model_id）
        assert!(text.contains("model = \"ep.systran.faster-whisper@medium\""));
        assert!(!text.contains("model_id ="));
        assert!(text.contains("device = \"cuda:0\""));
    }

    #[test]
    fn toml_llm_kind_emitted_for_empty_endpoint() {
        let pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        let text = pipeline_to_toml(&pipeline).unwrap();
        assert!(text.contains("kind = \"llm\""));
        assert!(text.contains("api_key_env = \"OPENAI_API_KEY\""));
        // params 保留 base_url/model/temperature
        assert!(text.contains("base_url = \"https://api.openai.com/v1\""));
        assert!(text.contains("temperature = 0.3"));
    }

    #[test]
    fn toml_string_escaping_roundtrips() {
        // 引号/反斜杠/换行 + CJK
        let mut pipeline = Pipeline::from_toml_str(
            r#"
[pipeline]
id = "esc"
name = "a\"b\\c"
description = "line1\nline2 中文"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
"#,
        )
        .unwrap();
        // 源 TOML 已把 \" \\ 解析进字符串；此处再叠加换行
        pipeline.name = "a\"b\\c\nd".to_string();
        let text = pipeline_to_toml(&pipeline).unwrap();
        let again = Pipeline::from_toml_str(&text).unwrap();
        assert_eq!(again.name, "a\"b\\c\nd");
        assert_eq!(again.description, "line1\nline2 中文");
    }

    #[test]
    fn toml_position_serialized_when_present() {
        let mut pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        pipeline.nodes[0].position = Some(NodePosition { x: 12.0, y: 34.5 });
        let text = pipeline_to_toml(&pipeline).unwrap();
        assert!(text.contains("x = 12.0"), "整数值补 .0：{text}");
        assert!(text.contains("y = 34.5"));
        let again = Pipeline::from_toml_str(&text).unwrap();
        assert_eq!(again.nodes[0].position.as_ref().unwrap().x, 12.0);
    }

    #[test]
    fn toml_float_non_finite_rejected() {
        assert!(toml_float(f64::NAN).is_err());
        assert!(toml_float(f64::INFINITY).is_err());
        assert_eq!(toml_float(2.5).unwrap(), "2.5");
        assert_eq!(toml_float(7.0).unwrap(), "7.0");
    }

    #[test]
    fn toml_escape_control_chars() {
        assert_eq!(toml_escape("a\"b"), "a\\\"b");
        assert_eq!(toml_escape("a\\b"), "a\\\\b");
        assert_eq!(toml_escape("a\nb"), "a\\nb");
        assert_eq!(toml_escape("a\u{01}b"), "a\\u0001b");
    }

    #[test]
    fn toml_roundtrip_real_repo_pipelines() {
        // 仓库真实管线（若脱离完整仓库布局则跳过）：
        // 验证自研 TOML 发射器与 ep-core 解析器在真实数据上往返等价。
        let base =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/pipelines");
        for name in ["video_to_srt.toml", "audio_extract.toml"] {
            let path = base.join(name);
            if !path.exists() {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read pipeline");
            let pipeline =
                Pipeline::from_toml_str(&text).unwrap_or_else(|e| panic!("{name} parse: {e}"));
            let out =
                pipeline_to_toml(&pipeline).unwrap_or_else(|e| panic!("{name} emit: {e}"));
            let again = Pipeline::from_toml_str(&out)
                .unwrap_or_else(|e| panic!("{name} re-parse: {e}\n---\n{out}"));
            assert_eq!(again.id, pipeline.id, "{name}: id");
            assert_eq!(again.name, pipeline.name, "{name}: name");
            assert_eq!(again.description, pipeline.description, "{name}: description");
            assert_eq!(again.nodes, pipeline.nodes, "{name}: nodes");
            assert_eq!(again.edges, pipeline.edges, "{name}: edges");
        }
    }
}
