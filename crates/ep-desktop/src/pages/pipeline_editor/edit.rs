//! 编辑器数据变更 — 节点创建/删除、连线校验、参数草稿、加载/保存/校验/执行。
//! 自 pipeline_editor.rs 拆分搬移；新增能力：指定位置建节点（palette 拖放）、
//! 多选删除、执行对话框提交（两态重做 §7.3）。

use ep_core::pipeline::dag::{Edge, NodeKind, NodePosition, Pipeline, PipelineNode};

use crate::pages::{draft_default, trfb, ModuleData, ParamDraft};

use super::{VizState};

// ── Node creation ─────────────────────────────────────────────────

/// 生成唯一节点 id（base 冲突时追加 _2/_3…）
pub(super) fn unique_id(pipeline: &Pipeline, base: &str) -> String {
    let sanitized: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let base = if sanitized.is_empty() { "node".to_string() } else { sanitized };
    if !pipeline.nodes.iter().any(|n| n.id == base) {
        return base;
    }
    for i in 2..1000 {
        let cand = format!("{base}_{i}");
        if !pipeline.nodes.iter().any(|n| n.id == cand) {
            return cand;
        }
    }
    format!("{base}_{}", pipeline.nodes.len() + 1)
}

/// 新节点落位（级联排布，避免重叠）
fn place_new_node(st: &mut VizState, id: &str) {
    let n = st.positions.len() as f32;
    let pos = egui::pos2(80.0 + (n % 6.0) * 48.0, 80.0 + (n % 8.0) * 44.0);
    st.positions.insert(id.to_string(), pos);
}

/// 建节点后的公共收尾：落位（或使用拖放指定位置）+ 选中 + dirty
fn finish_add(st: &mut VizState, id: &str, at: Option<egui::Pos2>) {
    match at {
        Some(pos) => {
            st.positions.insert(id.to_string(), pos);
        }
        None => place_new_node(st, id),
    }
    st.selected = Some(id.to_string());
    st.multi_select = vec![id.to_string()];
    st.selected_edge = None;
    st.dirty = true;
    st.drafts.clear();
}

pub(super) fn add_builtin_node(st: &mut VizState, builtin: &str, at: Option<egui::Pos2>) {
    let Some(p) = st.pipeline.as_mut() else { return };
    let id = unique_id(p, builtin);
    let node = PipelineNode {
        id: id.clone(),
        kind: NodeKind::Builtin {
            builtin: builtin.to_string(),
        },
        label: String::new(),
        params: Default::default(),
        position: None,
        timeout_secs: None,
        retry_count: None,
    };
    p.nodes.push(node);
    finish_add(st, &id, at);
}

pub(super) fn add_llm_node(st: &mut VizState, at: Option<egui::Pos2>) {
    let Some(p) = st.pipeline.as_mut() else { return };
    let id = unique_id(p, "llm");
    let node = PipelineNode {
        id: id.clone(),
        kind: NodeKind::ExternalApi {
            endpoint: String::new(),
            api_key_env: None,
        },
        label: String::new(),
        params: Default::default(),
        position: None,
        timeout_secs: None,
        retry_count: None,
    };
    p.nodes.push(node);
    finish_add(st, &id, at);
}

pub(super) fn add_module_node(
    st: &mut VizState,
    data: &ModuleData,
    module_id: &str,
    capability: &str,
    at: Option<egui::Pos2>,
) {
    let Some(p) = st.pipeline.as_mut() else { return };
    let id = unique_id(p, capability);
    // 参数默认值：schema 驱动（§5.3 同款 draft_default）
    let mut node = PipelineNode {
        id: id.clone(),
        kind: NodeKind::Module {
            module_id: module_id.to_string(),
            capability: capability.to_string(),
            model_id: None,
            device: None,
        },
        label: String::new(),
        params: Default::default(),
        position: None,
        timeout_secs: None,
        retry_count: None,
    };
    if let Some(cap) = data.capability(module_id, capability) {
        if let Some(schema_map) = &cap.params {
            let mut keys: Vec<&String> = schema_map.keys().collect();
            keys.sort();
            for key in keys {
                let draft = draft_default(&schema_map[key]);
                set_param_draft(&mut node, key, &draft);
            }
        }
    }
    p.nodes.push(node);
    finish_add(st, &id, at);
}

/// 草稿值 → node.params 单键写入（匿名访问 serde_json::Value：
/// ep-desktop 未直接依赖 serde_json，经方法调用 + Into 推断完成读写）
pub(super) fn set_param_draft(node: &mut PipelineNode, key: &str, draft: &ParamDraft) {
    match draft {
        ParamDraft::Str(s) => node.params[key] = s.clone().into(),
        ParamDraft::Int(i) => node.params[key] = (*i).into(),
        ParamDraft::Float(f) => node.params[key] = (*f).into(),
        ParamDraft::Bool(b) => node.params[key] = (*b).into(),
    }
}

// ── Draft sync（加载/回写） ───────────────────────────────────────

/// 每帧：选中节点无草稿则从节点加载；有草稿则回写到节点（幂等）。
pub(super) fn sync_draft(st: &mut VizState, data: &ModuleData) {
    let Some(sel) = st.selected.clone() else { return };
    let Some(pipeline) = st.pipeline.as_mut() else { return };
    let Some(node) = pipeline.nodes.iter().find(|n| n.id == sel) else {
        st.selected = None;
        return;
    };

    if !st.drafts.contains_key(&sel) {
        let draft = load_draft(node, data);
        st.drafts.insert(sel.clone(), draft);
    }
    if let Some(draft) = st.drafts.get(&sel).cloned() {
        if let Some(node) = pipeline.nodes.iter_mut().find(|n| n.id == sel) {
            apply_draft(node, &draft);
        }
    }
}

/// 节点 → 草稿（首次选中时解析，其后以草稿为编辑态）
fn load_draft(node: &PipelineNode, data: &ModuleData) -> super::NodeDraft {
    let mut d = super::NodeDraft {
        device: "auto".to_string(),
        output_format: "text".to_string(),
        ..Default::default()
    };
    d.label = node.label.clone();
    d.timeout = node.timeout_secs.unwrap_or(0);
    d.retry = node.retry_count.unwrap_or(0);

    match &node.kind {
        NodeKind::Module {
            module_id,
            capability,
            model_id,
            device,
        } => {
            d.capability = capability.clone();
            d.model = model_id.clone().unwrap_or_default();
            d.device = device.clone().unwrap_or_else(|| "auto".to_string());
            if let Some(cap) = data.capability(module_id, capability) {
                if let Some(schema_map) = &cap.params {
                    let mut keys: Vec<&String> = schema_map.keys().collect();
                    keys.sort();
                    for key in keys {
                        let schema = &schema_map[key];
                        let draft = read_param_draft(node, key, schema);
                        d.params.push(((*key).clone(), draft));
                    }
                }
            }
        }
        NodeKind::Builtin { builtin } if builtin == "ffmpeg" => {
            let args_val = node.params.get("args");
            if let Some(arr) = args_val.and_then(|v| v.as_array()) {
                d.args = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
            } else if let Some(s) = args_val.and_then(|v| v.as_str()) {
                d.args_raw = s.to_string();
                d.args_is_string = true;
            }
            d.output_extension = node
                .params
                .get("output_extension")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
        }
        NodeKind::ExternalApi {
            endpoint,
            api_key_env,
        } => {
            d.api_key_env = api_key_env.clone().unwrap_or_default();
            let read_str = |key: &str| {
                node.params
                    .get(key)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            d.base_url = if endpoint.is_empty() {
                read_str("base_url")
            } else {
                endpoint.clone()
            };
            d.llm_model = read_str("model");
            d.system_prompt = read_str("system_prompt");
            if let Some(t) = node.params.get("temperature").and_then(|v| v.as_f64()) {
                d.temperature = t;
                d.has_temperature = true;
            }
            if let Some(m) = node.params.get("max_tokens").and_then(|v| v.as_i64()) {
                d.max_tokens = m;
                d.has_max_tokens = true;
            }
            let fmt = read_str("output_format");
            d.output_format = if fmt == "json" { "json".to_string() } else { "text".to_string() };
        }
        _ => {}
    }

    d
}

/// 读取单个参数草稿（node.params 匿名读取；缺值 → schema 默认）
fn read_param_draft(
    node: &PipelineNode,
    key: &str,
    schema: &ep_core::module::ParamSchema,
) -> ParamDraft {
    let t = schema.param_type.to_ascii_lowercase();
    let Some(v) = node.params.get(key) else {
        return draft_default(schema);
    };
    if t == "boolean" || t == "bool" {
        v.as_bool()
            .map(ParamDraft::Bool)
            .unwrap_or_else(|| draft_default(schema))
    } else if t == "integer" || t == "int" {
        v.as_i64()
            .map(ParamDraft::Int)
            .unwrap_or_else(|| draft_default(schema))
    } else if t == "number" || t == "float" || t == "double" {
        v.as_f64()
            .map(ParamDraft::Float)
            .unwrap_or_else(|| draft_default(schema))
    } else {
        ParamDraft::Str(
            v.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| v.to_string()),
        )
    }
}

/// 草稿 → 节点（label/timeout/retry/kind 字段/params 全量同步）
pub(super) fn apply_draft(node: &mut PipelineNode, draft: &super::NodeDraft) {
    node.label = draft.label.clone();
    node.timeout_secs = (draft.timeout > 0).then_some(draft.timeout);
    node.retry_count = (draft.retry > 0).then_some(draft.retry);

    match &mut node.kind {
        NodeKind::Module {
            capability,
            model_id,
            device,
            ..
        } => {
            if !draft.capability.is_empty() {
                *capability = draft.capability.clone();
            }
            *model_id = if draft.model.is_empty() {
                None
            } else {
                Some(draft.model.clone())
            };
            *device = if draft.device.is_empty() || draft.device.eq_ignore_ascii_case("auto") {
                None
            } else {
                Some(draft.device.clone())
            };
            // params：仅覆盖草稿涉及的 schema 键，其余非 schema 键保留
            //（P2 修复：含 schema 外键的 TOML 一经编辑不再静默丢失）
            if let Some(obj) = node.params.as_object_mut() {
                let draft_keys: Vec<&str> = draft.params.iter().map(|(k, _)| k.as_str()).collect();
                obj.retain(|k, _| !draft_keys.contains(&k.as_str()));
            }
            for (key, d) in &draft.params {
                set_param_draft(node, key, d);
            }
        }
        NodeKind::Builtin { builtin } if builtin == "ffmpeg" => {
            if draft.args_is_string {
                node.params["args"] = draft.args_raw.clone().into();
            } else {
                node.params["args"] = draft.args.clone().into();
            }
            if draft.output_extension.is_empty() {
                if let Some(obj) = node.params.as_object_mut() {
                    obj.remove("output_extension");
                }
            } else {
                node.params["output_extension"] = draft.output_extension.clone().into();
            }
        }
        NodeKind::ExternalApi { api_key_env, .. } => {
            *api_key_env = if draft.api_key_env.is_empty() {
                None
            } else {
                Some(draft.api_key_env.clone())
            };
            // base_url / model：必填项，空值也写出（校验器负责报错）
            node.params["base_url"] = draft.base_url.clone().into();
            node.params["model"] = draft.llm_model.clone().into();
            set_or_remove(node, "system_prompt", &draft.system_prompt);
            if draft.has_temperature {
                node.params["temperature"] = draft.temperature.into();
            } else if let Some(obj) = node.params.as_object_mut() {
                obj.remove("temperature");
            }
            if draft.has_max_tokens {
                node.params["max_tokens"] = draft.max_tokens.into();
            } else if let Some(obj) = node.params.as_object_mut() {
                obj.remove("max_tokens");
            }
            if draft.output_format == "json" {
                node.params["output_format"] = "json".into();
            } else if let Some(obj) = node.params.as_object_mut() {
                obj.remove("output_format");
            }
        }
        _ => {}
    }
}

/// 非空写字符串参数，空则移除键
fn set_or_remove(node: &mut PipelineNode, key: &str, value: &str) {
    if value.is_empty() {
        if let Some(obj) = node.params.as_object_mut() {
            obj.remove(key);
        }
    } else {
        node.params[key] = value.to_string().into();
    }
}

// ── Ports & connection validation ─────────────────────────────────

/// 节点端口类型 (输入, 输出)：None = 无该端口（file_input 无入、file_output 无出）
pub(super) fn node_port_types(
    node: &PipelineNode,
    data: &ModuleData,
) -> (Option<ep_core::types::DataType>, Option<ep_core::types::DataType>) {
    use ep_core::types::DataType;
    match &node.kind {
        NodeKind::Builtin { builtin } => match builtin.as_str() {
            "file_input" => (None, Some(DataType::File)),
            "file_output" => (Some(DataType::File), None),
            _ => (Some(DataType::File), Some(DataType::File)),
        },
        NodeKind::ExternalApi { .. } => (Some(DataType::Text), Some(DataType::Text)),
        NodeKind::Module {
            module_id,
            capability,
            ..
        } => match data.capability(module_id, capability) {
            Some(cap) => (Some(cap.input_type), Some(cap.output_type)),
            None => (None, None),
        },
    }
}

/// 连线尝试：自连/重边/成环/端口类型校验；结果写状态栏。
/// `pipeline` 为当前帧快照（只读），改动落在 st.pipeline。
pub(super) fn try_connect(
    st: &mut VizState,
    lang: &str,
    pipeline: &Pipeline,
    data: &ModuleData,
    from: &str,
    to: &str,
) {
    let fail = |st: &mut VizState, msg: String| {
        st.validation_msg = Some(msg);
        st.validation_ok = false;
    };

    if from == to {
        fail(
            st,
            trfb(lang, "desktopApp.pipeline.connSelf", "不能连接到自身", &[]),
        );
        return;
    }
    let new_edge = Edge {
        from: (from.to_string(), "output".to_string()),
        to: (to.to_string(), "input".to_string()),
    };
    if pipeline.edges.contains(&new_edge) {
        fail(
            st,
            trfb(lang, "desktopApp.pipeline.connDup", "该连线已存在", &[]),
        );
        return;
    }
    if creates_cycle(pipeline, from, to) {
        fail(
            st,
            trfb(
                lang,
                "desktopApp.pipeline.connCycle",
                "连线会形成环，已拒绝",
                &[],
            ),
        );
        return;
    }
    // 端口类型校验（DataType::is_compatible_with）：
    // module 节点无清单时按通配（None）放行，不误拒未安装模块的编排。
    let out_type = pipeline
        .nodes
        .iter()
        .find(|n| n.id == from)
        .and_then(|n| node_port_types(n, data).1);
    let in_type = pipeline
        .nodes
        .iter()
        .find(|n| n.id == to)
        .and_then(|n| node_port_types(n, data).0);
    if let (Some(out_t), Some(in_t)) = (out_type, in_type) {
        if !out_t.is_compatible_with(&in_t) {
            fail(
                st,
                trfb(
                    lang,
                    "desktopApp.pipeline.connType",
                    "端口类型不兼容: {{from}} → {{to}}",
                    &[
                        ("from", &format!("{out_t:?}")),
                        ("to", &format!("{in_t:?}")),
                    ],
                ),
            );
            return;
        }
    }

    if let Some(p) = st.pipeline.as_mut() {
        p.edges.push(new_edge);
        st.dirty = true;
        st.validation_msg = Some(trfb(
            lang,
            "desktopApp.pipeline.connOk",
            "已连线",
            &[],
        ));
        st.validation_ok = true;
    }
}

/// 成环检测：添加 from→to 成环 ⟺ 已存在 to→…→from 路径
pub(super) fn creates_cycle(pipeline: &Pipeline, from: &str, to: &str) -> bool {
    let mut stack: Vec<&str> = vec![to];
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    while let Some(cur) = stack.pop() {
        if cur == from {
            return true;
        }
        if !seen.insert(cur) {
            continue;
        }
        for e in &pipeline.edges {
            if e.from.0 == cur {
                stack.push(&e.to.0);
            }
        }
    }
    false
}

pub(super) fn remove_edge(st: &mut VizState, edge: &Edge) {
    if let Some(p) = st.pipeline.as_mut() {
        p.edges.retain(|e| e != edge);
        st.selected_edge = None;
        st.dirty = true;
    }
}

pub(super) fn delete_node(st: &mut VizState, node_id: &str) {
    if let Some(p) = st.pipeline.as_mut() {
        p.nodes.retain(|n| n.id != node_id);
        p.edges
            .retain(|e| e.from.0 != node_id && e.to.0 != node_id);
        st.positions.remove(node_id);
        st.drafts.remove(node_id);
        if st.selected.as_deref() == Some(node_id) {
            st.selected = None;
        }
        st.multi_select.retain(|id| id != node_id);
        st.selected_edge = None;
        st.dirty = true;
    }
}

/// Delete/Backspace 删除选中项（映射表 #7）：优先删选中连线，
/// 其次删多选集合（框选），最后删单选节点。
pub(super) fn delete_selected(st: &mut VizState) {
    if let Some(edge) = st.selected_edge.clone() {
        remove_edge(st, &edge);
        return;
    }
    if !st.multi_select.is_empty() {
        let ids = std::mem::take(&mut st.multi_select);
        for id in &ids {
            delete_node(st, id);
        }
        return;
    }
    if let Some(id) = st.selected.clone() {
        delete_node(st, &id);
    }
}

// ── Actions ───────────────────────────────────────────────────────

pub(super) fn load_pipeline(st: &mut VizState, lang: &str) {
    let path = std::path::Path::new(&st.file_path);
    match Pipeline::from_toml(path) {
        Ok(pipeline) => {
            let (msg, ok) = match pipeline.validate() {
                Ok(()) => (
                    crate::i18n::tr(lang, "desktopApp.pipeline.validationPassed", &[]),
                    true,
                ),
                Err(errors) => (super::format_errors(&errors), false),
            };
            st.positions.clear();
            st.selected = None;
            st.multi_select.clear();
            st.selected_edge = None;
            st.drafts.clear();
            st.dirty = false;
            st.offset = egui::Vec2::ZERO;
            st.zoom = 1.0;
            st.validation_msg = Some(msg);
            st.validation_ok = ok;
            st.pipeline = Some(pipeline);
        }
        Err(e) => {
            st.validation_msg = Some(crate::i18n::tr(
                lang,
                "desktopApp.pipeline.loadFailed",
                &[("detail", &e.to_string())],
            ));
            st.validation_ok = false;
            st.pipeline = None;
        }
    }
}

/// 新建空白管线：file_input → file_output
pub(super) fn new_pipeline(st: &mut VizState) {
    let input = PipelineNode {
        id: "input".to_string(),
        kind: NodeKind::Builtin {
            builtin: "file_input".to_string(),
        },
        label: String::new(),
        params: Default::default(),
        position: None,
        timeout_secs: None,
        retry_count: None,
    };
    let output = PipelineNode {
        id: "output".to_string(),
        kind: NodeKind::Builtin {
            builtin: "file_output".to_string(),
        },
        label: String::new(),
        params: Default::default(),
        position: None,
        timeout_secs: None,
        retry_count: None,
    };
    let edge = Edge {
        from: ("input".to_string(), "output".to_string()),
        to: ("output".to_string(), "input".to_string()),
    };
    st.pipeline = Some(Pipeline {
        id: "new-pipeline".to_string(),
        name: String::new(),
        description: String::new(),
        nodes: vec![input, output],
        edges: vec![edge],
        max_instances: None,
        node_timeout_secs: None,
    });
    st.positions.clear();
    st.drafts.clear();
    st.selected = None;
    st.multi_select.clear();
    st.selected_edge = None;
    st.dirty = true;
    st.file_path.clear();
    st.validation_msg = None;
    st.offset = egui::Vec2::ZERO;
    st.zoom = 1.0;
}

pub(super) fn validate_pipeline(st: &mut VizState, lang: &str) {
    match &st.pipeline {
        Some(p) => {
            let (msg, ok) = match p.validate() {
                Ok(()) => (
                    crate::i18n::tr(lang, "desktopApp.pipeline.validationPassed", &[]),
                    true,
                ),
                Err(errors) => (super::format_errors(&errors), false),
            };
            st.validation_msg = Some(msg);
            st.validation_ok = ok;
        }
        None => {
            st.validation_msg = Some(crate::i18n::tr(
                lang,
                "desktopApp.pipeline.loadFileFirst",
                &[],
            ));
            st.validation_ok = false;
        }
    }
}

/// 执行期选择的输入文件写入管线中全部 file_input 节点的 `params.path`
///（执行器 `execute_builtin_file_input` 以该键为源文件路径）。
pub(super) fn apply_exec_input(pipeline: &mut Pipeline, file: &std::path::Path) {
    for node in &mut pipeline.nodes {
        if matches!(&node.kind, NodeKind::Builtin { builtin } if builtin == "file_input") {
            // 匿名访问 serde_json::Value（本项目不直接依赖 serde_json，经 Into 推断）
            node.params["path"] = file.to_string_lossy().into_owned().into();
        }
    }
}

/// 管线是否包含 file_input 节点（执行对话框输入文件校验用）
pub(super) fn has_file_input(pipeline: &Pipeline) -> bool {
    pipeline
        .nodes
        .iter()
        .any(|n| matches!(&n.kind, NodeKind::Builtin { builtin } if builtin == "file_input"))
}

/// 执行对话框提交（两态重做：ExecuteDialog 模态，§7.3 映射表 #11）。
/// `input` 为对话框中确认的输入文件路径（空 = 未选择）。
/// 提交语义与既有 execute_pipeline 相同：内存管线对象经 AppCmd 提交，
/// 未保存修改也可执行；所选路径写入 file_input 节点 params.path。
pub(super) fn submit_execution(
    st: &mut VizState,
    lang: &str,
    cmd_tx: Option<&tokio::sync::mpsc::UnboundedSender<crate::app::AppCmd>>,
    mut pipeline: Pipeline,
    input: &std::path::Path,
) {
    if !input.as_os_str().is_empty() {
        st.exec_input = Some(input.to_path_buf());
        apply_exec_input(&mut pipeline, input);
    }
    match cmd_tx {
        Some(tx) => {
            // 门禁接线完成（C4 冻结入口）：提交内存管线对象执行，进度见任务页
            if tx
                .send(crate::app::AppCmd::ExecutePipeline { pipeline })
                .is_ok()
            {
                st.validation_msg = Some(trfb(
                    lang,
                    "desktopApp.pipeline.execSubmitted",
                    "已提交执行，进度见任务页",
                    &[],
                ));
                st.validation_ok = true;
            } else {
                st.validation_msg = Some(trfb(
                    lang,
                    "desktopApp.pipeline.execChannelClosed",
                    "执行通道不可用（后台循环已退出）",
                    &[],
                ));
                st.validation_ok = false;
            }
        }
        None => {
            st.validation_msg = Some(trfb(
                lang,
                "desktopApp.pipeline.execPendingWire",
                "已就绪：等待执行通道接线（AppCmd::ExecutePipeline）",
                &[],
            ));
            st.validation_ok = true;
        }
    }
}

/// 保存 TOML：序列化写回 file_path；空路径或"另存"走 rfd 对话框。
pub(super) fn save_pipeline(st: &mut VizState, lang: &str, save_as: bool) {
    let Some(p) = st.pipeline.clone() else {
        st.validation_msg = Some(crate::i18n::tr(
            lang,
            "desktopApp.pipeline.loadFileFirst",
            &[],
        ));
        st.validation_ok = false;
        return;
    };

    // 画布坐标 → node.position（随保存落盘）
    let mut p = p;
    for node in &mut p.nodes {
        node.position = st.positions.get(&node.id).map(|pos| NodePosition {
            x: f64::from(pos.x),
            y: f64::from(pos.y),
        });
    }

    if save_as || st.file_path.trim().is_empty() {
        let mut dlg = rfd::FileDialog::new()
            .set_title(trfb(lang, "desktopApp.pipeline.saveTitle", "保存管线 TOML", &[]))
            .add_filter("TOML", &["toml"]);
        if !st.file_path.trim().is_empty() {
            dlg = dlg.set_file_name(st.file_path.trim());
        }
        match dlg.save_file() {
            Some(path) => {
                st.file_path = path.to_string_lossy().to_string();
            }
            None => return, // 取消
        }
    }

    match super::toml_serde::pipeline_to_toml(&p) {
        Ok(text) => {
            let path = std::path::PathBuf::from(st.file_path.trim());
            match std::fs::write(&path, text) {
                Ok(()) => {
                    st.dirty = false;
                    st.validation_msg = Some(trfb(
                        lang,
                        "desktopApp.pipeline.saved",
                        "已保存: {{path}}",
                        &[("path", &path.to_string_lossy())],
                    ));
                    st.validation_ok = true;
                }
                Err(e) => {
                    st.validation_msg = Some(trfb(
                        lang,
                        "desktopApp.pipeline.saveFailed",
                        "保存失败: {{detail}}",
                        &[("detail", &e.to_string())],
                    ));
                    st.validation_ok = false;
                }
            }
        }
        Err(e) => {
            st.validation_msg = Some(trfb(
                lang,
                "desktopApp.pipeline.saveSerializeFailed",
                "序列化失败: {{detail}}",
                &[("detail", &e)],
            ));
            st.validation_ok = false;
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::toml_serde::tests::sample_toml;

    /// P1 回归：执行期选择的输入文件写入全部 file_input 节点的 params.path，
    /// 不再静默丢弃（提交执行时随管线携带，执行器以该键为源文件路径）。
    #[test]
    fn apply_exec_input_writes_path_to_all_file_input_nodes() {
        let mut pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        // 追加第二个 file_input 节点（多输入管线），并给首个节点预置旧路径
        pipeline.nodes[0].params["path"] = "old/path.wav".into();
        pipeline.nodes.push(PipelineNode {
            id: "input2".into(),
            kind: NodeKind::Builtin {
                builtin: "file_input".into(),
            },
            label: String::new(),
            params: Default::default(),
            position: None,
            timeout_secs: None,
            retry_count: None,
        });

        apply_exec_input(&mut pipeline, std::path::Path::new("C:\\data\\in.mp3"));

        for node in &pipeline.nodes {
            if matches!(&node.kind, NodeKind::Builtin { builtin } if builtin == "file_input") {
                assert_eq!(
                    node.params["path"].as_str(),
                    Some("C:\\data\\in.mp3"),
                    "file_input 节点 {} 的 params.path 必须写入所选输入文件",
                    node.id
                );
            }
        }
        // 非 file_input 节点不受影响
        let module = pipeline.nodes.iter().find(|n| n.id == "process").unwrap();
        assert!(module.params.get("path").is_none());
    }

    /// P2 回归：apply_draft 只覆盖草稿涉及的 schema 键，params 中的
    /// 非 schema 键（TOML 手写扩展键）经编辑后必须保留，不得静默丢失。
    #[test]
    fn apply_draft_preserves_non_schema_params_keys() {
        let mut pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        let node = pipeline.nodes.iter_mut().find(|n| n.id == "process").unwrap();
        // 预置 schema 外扩展键（模拟用户手写 TOML 的自定义参数）
        node.params["custom_extra"] = 42.into();
        node.params["language"] = "en".into();

        let draft = super::super::NodeDraft {
            params: vec![("language".to_string(), ParamDraft::Str("zh".to_string()))],
            ..Default::default()
        };
        apply_draft(node, &draft);

        assert_eq!(node.params["language"].as_str(), Some("zh"));
        assert_eq!(
            node.params["custom_extra"].as_i64(),
            Some(42),
            "非 schema 键 custom_extra 不得被草稿清空丢弃"
        );
    }

    #[test]
    fn unique_id_deduplicates() {
        let pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        assert_eq!(unique_id(&pipeline, "ffmpeg"), "ffmpeg");
        assert_eq!(unique_id(&pipeline, "input"), "input_2");
        assert_eq!(unique_id(&pipeline, "weird id!"), "weird-id-");
    }

    #[test]
    fn creates_cycle_detects_paths() {
        let pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        // input→process→translate→save 已存在；save→input 会成环
        assert!(creates_cycle(&pipeline, "save", "input"));
        assert!(creates_cycle(&pipeline, "translate", "process"));
        // 反向添加不成环
        assert!(!creates_cycle(&pipeline, "save", "save2"));
        assert!(!creates_cycle(&pipeline, "input", "save"));
    }

    #[test]
    fn port_types_builtin_and_llm() {
        use ep_core::types::DataType;
        let file_input = PipelineNode {
            id: "i".into(),
            kind: NodeKind::Builtin {
                builtin: "file_input".into(),
            },
            label: String::new(),
            params: Default::default(),
            position: None,
            timeout_secs: None,
            retry_count: None,
        };
        let file_output = PipelineNode {
            id: "o".into(),
            kind: NodeKind::Builtin {
                builtin: "file_output".into(),
            },
            label: String::new(),
            params: Default::default(),
            position: None,
            timeout_secs: None,
            retry_count: None,
        };
        let llm = PipelineNode {
            id: "l".into(),
            kind: NodeKind::ExternalApi {
                endpoint: String::new(),
                api_key_env: None,
            },
            label: String::new(),
            params: Default::default(),
            position: None,
            timeout_secs: None,
            retry_count: None,
        };
        let data = ModuleData {
            discovered: vec![],
            loaded_at: std::time::Instant::now(),
        };
        assert_eq!(
            node_port_types(&file_input, &data),
            (None, Some(DataType::File))
        );
        assert_eq!(
            node_port_types(&file_output, &data),
            (Some(DataType::File), None)
        );
        assert_eq!(
            node_port_types(&llm, &data),
            (Some(DataType::Text), Some(DataType::Text))
        );
    }

    /// 两态重做：has_file_input 识别 file_input 节点（执行对话框校验依据）
    #[test]
    fn has_file_input_detects_builtin() {
        let pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        assert!(has_file_input(&pipeline));
        let mut no_input = pipeline.clone();
        no_input
            .nodes
            .retain(|n| !matches!(&n.kind, NodeKind::Builtin { builtin } if builtin == "file_input"));
        assert!(!has_file_input(&no_input));
    }

    /// 两态重做：delete_selected 优先删边、再删多选集合、最后单选
    #[test]
    fn delete_selected_priority_edge_then_multi_then_single() {
        let mut st = VizState::default();
        st.pipeline = Some(Pipeline::from_toml_str(sample_toml()).unwrap());
        st.positions = super::super::compute_layout(st.pipeline.as_ref().unwrap());

        // 1) 选中边 → 删边，节点不动
        let edge = st.pipeline.as_ref().unwrap().edges[0].clone();
        st.selected_edge = Some(edge.clone());
        st.multi_select = vec!["process".to_string()];
        delete_selected(&mut st);
        assert!(st.selected_edge.is_none());
        assert_eq!(st.pipeline.as_ref().unwrap().edges.len(), 2);
        assert_eq!(st.multi_select, vec!["process".to_string()], "删边不动多选");

        // 2) 多选集合 → 全部删除（连带相关边）
        st.multi_select = vec!["process".to_string(), "translate".to_string()];
        delete_selected(&mut st);
        let p = st.pipeline.as_ref().unwrap();
        assert_eq!(p.nodes.len(), 2, "input/save 保留");
        assert!(p.edges.is_empty(), "与删除节点相关的边一并删除");
        assert!(st.multi_select.is_empty());

        // 3) 单选节点
        st.selected = Some("input".to_string());
        delete_selected(&mut st);
        assert_eq!(st.pipeline.as_ref().unwrap().nodes.len(), 1);
        assert!(st.selected.is_none());
    }
}
