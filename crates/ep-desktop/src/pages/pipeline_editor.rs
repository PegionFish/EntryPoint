use eframe::egui;
use ep_core::pipeline::dag::{NodeKind, Pipeline, ValidationError};

pub fn show(ui: &mut egui::Ui) {
    ui.heading("管线编辑器");
    ui.add_space(4.0);

    // ── 加载区域 ──
    ui.horizontal(|ui| {
        ui.label("管线文件:");
        // Use a static-ish path buffer via ui data
        let mut path_buf = ui
            .data_mut(|d| {
                d.get_temp_mut_or_default::<String>(egui::Id::new("pipeline_path"))
                    .clone()
            });
        if ui.text_edit_singleline(&mut path_buf).changed() {
            ui.data_mut(|d| {
                d.insert_temp(egui::Id::new("pipeline_path"), path_buf.clone());
            });
        }

        if ui.button("加载").clicked() {
            let path = std::path::Path::new(&path_buf);
            match Pipeline::from_toml(path) {
                Ok(pipeline) => {
                    let validation = match pipeline.validate() {
                        Ok(()) => "✅ 验证通过".to_string(),
                        Err(errors) => format!("❌ {}", format_errors(&errors)),
                    };
                    ui.data_mut(|d| {
                        d.insert_temp(egui::Id::new("pipeline_loaded"), pipeline);
                        d.insert_temp(egui::Id::new("pipeline_validation"), validation);
                    });
                }
                Err(e) => {
                    ui.data_mut(|d| {
                        d.insert_temp(
                            egui::Id::new("pipeline_validation"),
                            format!("❌ 加载失败: {e}"),
                        );
                        d.remove::<Pipeline>(egui::Id::new("pipeline_loaded"));
                    });
                }
            }
        }
    });

    ui.add_space(4.0);

    // ── 验证状态 ──
    let validation = ui.data(|d| {
        d.get_temp::<String>(egui::Id::new("pipeline_validation"))
    });
    if let Some(ref v) = validation {
        if v.starts_with("✅") {
            ui.colored_label(egui::Color32::from_rgb(80, 220, 80), v);
        } else {
            ui.colored_label(egui::Color32::from_rgb(255, 100, 100), v);
        }
    }

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);

    // ── 管线详情 ──
    let pipeline = ui.data(|d| {
        d.get_temp::<Pipeline>(egui::Id::new("pipeline_loaded"))
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
        // ── 管线信息 ──
        ui.strong(&pipeline.name);
        ui.label(format!("ID: {}", pipeline.id));
        if !pipeline.description.is_empty() {
            ui.label(&pipeline.description);
        }
        ui.add_space(8.0);

        // ── 拓扑分层 ──
        if let Ok(layers) = pipeline.topological_layers() {
            ui.strong(format!("执行层数: {}", layers.len()));
            ui.add_space(4.0);
            for (i, layer) in layers.iter().enumerate() {
                ui.label(format!("  层 {}: {}", i, layer.join(", ")));
            }
            ui.add_space(8.0);
        }

        // ── 节点列表 ──
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

        // ── 边列表 ──
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
                        ui.label("→");
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
            "🧩 模块",
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
        NodeKind::Builtin { builtin } => ("🔧 内置", builtin.clone()),
        NodeKind::ExternalApi {
            endpoint, api_type, ..
        } => ("🌐 API", format!("{api_type}: {endpoint}")),
    }
}

fn format_errors(errors: &[ValidationError]) -> String {
    errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}
