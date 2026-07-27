use eframe::egui;
use ep_core::pipeline::dag::{NodeKind, Pipeline, ValidationError};

/// Shared state for the pipeline editor page.
#[derive(Clone, Default)]
struct PipelineEditorState {
    path: String,
    loaded_pipeline: Option<Pipeline>,
    validation_msg: Option<String>,
}

fn state_id() -> egui::Id {
    egui::Id::new("pipeline_editor_state")
}

pub fn show(ui: &mut egui::Ui) {
    ui.heading("管线编辑器");
    ui.add_space(4.0);

    // Ensure state exists
    ui.data_mut(|d| {
        d.get_temp_mut_or_default::<PipelineEditorState>(state_id());
    });

    // Read current state (may have been updated by set_value via UIA)
    let mut state = ui.data(|d| {
        d.get_temp::<PipelineEditorState>(state_id())
    }).unwrap_or_default();

    let edit_id = egui::Id::new("pipeline_path_edit");

    ui.horizontal(|ui| {
        ui.label("管线文件:");

        // Use TextEdit with persistent ID so set_value via UIA syncs properly
        let response = ui.add(
            egui::TextEdit::singleline(&mut state.path)
                .id(edit_id)
                .desired_width(300.0)
        );

        if response.changed() {
            ui.data_mut(|d| {
                d.get_temp_mut_or_default::<PipelineEditorState>(state_id()).path = state.path.clone();
            });
        }

        if ui.button("加载").clicked() {
            // Read path from state (synced via changed() or set_value)
            let latest = ui.data(|d| {
                d.get_temp::<PipelineEditorState>(state_id())
            }).unwrap_or_default();
            let path_to_load = if latest.path.is_empty() {
                state.path.clone()
            } else {
                latest.path.clone()
            };

            let path = std::path::Path::new(&path_to_load);
            match Pipeline::from_toml(path) {
                Ok(pipeline) => {
                    let validation = match pipeline.validate() {
                        Ok(()) => "验证通过".to_string(),
                        Err(errors) => format_errors(&errors).to_string(),
                    };
                    ui.data_mut(|d| {
                        let s = d.get_temp_mut_or_default::<PipelineEditorState>(state_id());
                        s.loaded_pipeline = Some(pipeline);
                        s.validation_msg = Some(validation);
                    });
                }
                Err(e) => {
                    ui.data_mut(|d| {
                        let s = d.get_temp_mut_or_default::<PipelineEditorState>(state_id());
                        s.validation_msg = Some(format!("加载失败: {e}"));
                        s.loaded_pipeline = None;
                    });
                }
            }
        }
    });

    // Sync state back
    ui.data_mut(|d| {
        d.get_temp_mut_or_default::<PipelineEditorState>(state_id()).path = state.path.clone();
    });

    ui.add_space(4.0);

    // Validation status
    let validation = ui.data(|d| {
        d.get_temp::<PipelineEditorState>(state_id())
            .and_then(|s| s.validation_msg.clone())
    });
    if let Some(ref v) = validation {
        if v.starts_with("验证通过") {
            ui.colored_label(egui::Color32::from_rgb(80, 220, 80), v);
        } else {
            ui.colored_label(egui::Color32::from_rgb(255, 100, 100), v);
        }
    }

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);

    // Pipeline details
    let pipeline = ui.data(|d| {
        d.get_temp::<PipelineEditorState>(state_id())
            .and_then(|s| s.loaded_pipeline.clone())
    });

    match pipeline {
        Some(ref p) => {
            pipeline_detail(ui, p);
        }
        None => {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label(
                    egui::RichText::new("加载管线文件以查看节点和连接")
                        .color(egui::Color32::from_gray(140)),
                );
            });
        }
    }
}

fn pipeline_detail(ui: &mut egui::Ui, pipeline: &Pipeline) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.strong(&pipeline.name);
        ui.label(format!("ID: {}", pipeline.id));
        if !pipeline.description.is_empty() {
            ui.label(&pipeline.description);
        }
        ui.add_space(8.0);

        if let Ok(layers) = pipeline.topological_layers() {
            ui.strong(format!("执行层数: {}", layers.len()));
            ui.add_space(4.0);
            for (i, layer) in layers.iter().enumerate() {
                ui.label(format!("  层 {}: {}", i, layer.join(", ")));
            }
            ui.add_space(8.0);
        }

        ui.strong(format!("节点 ({}):", pipeline.nodes.len()));
        ui.add_space(4.0);

        egui::Grid::new("pipeline_nodes_grid")
            .striped(true)
            .min_col_width(80.0)
            .show(ui, |ui| {
                ui.strong("ID");
                ui.strong("类型");
                ui.strong("标签");
                ui.strong("详情");
                ui.end_row();

                for node in &pipeline.nodes {
                    ui.label(&node.id);
                    let (kind_str, detail) = node_kind_info(&node.kind);
                    ui.label(kind_str);
                    ui.label(if node.label.is_empty() { "-" } else { &node.label });
                    ui.label(detail);
                    ui.end_row();
                }
            });

        ui.add_space(12.0);

        ui.strong(format!("连接 ({}):", pipeline.edges.len()));
        ui.add_space(4.0);

        if pipeline.edges.is_empty() {
            ui.label("（无连接）");
        } else {
            egui::Grid::new("pipeline_edges_grid")
                .striped(true)
                .min_col_width(80.0)
                .show(ui, |ui| {
                    ui.strong("来源");
                    ui.strong("端口");
                    ui.strong("目标");
                    ui.strong("端口");
                    ui.end_row();

                    for edge in &pipeline.edges {
                        ui.label(&edge.from.0);
                        ui.label(&edge.from.1);
                        ui.label("->");
                        ui.label(&edge.to.0);
                        ui.label(&edge.to.1);
                        ui.end_row();
                    }
                });
        }
    });
}

fn node_kind_info(kind: &NodeKind) -> (&'static str, String) {
    match kind {
        NodeKind::Module {
            module_id,
            capability,
            model_id,
        } => (
            "模块",
            format!(
                "{}::{}{}",
                module_id,
                capability,
                model_id
                    .as_ref()
                    .map(|m| format!(" (model: {m})"))
                    .unwrap_or_default()
            ),
        ),
        NodeKind::Builtin { builtin } => ("内置", builtin.clone()),
        NodeKind::ExternalApi {
            endpoint, api_type, ..
        } => ("API", format!("{api_type}: {endpoint}")),
    }
}

fn format_errors(errors: &[ValidationError]) -> String {
    errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}
