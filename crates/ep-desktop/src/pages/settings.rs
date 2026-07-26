use eframe::egui;
use ep_core::config::{AppConfig, AssignStrategy};

pub fn show(
    ui: &mut egui::Ui,
    config: &mut AppConfig,
    status_message: &mut Option<(String, std::time::Instant)>,
) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.heading("设置");
        ui.add_space(12.0);

        // ── 计算设备 ──
        ui.strong("计算设备");
        ui.add_space(4.0);
        egui::Grid::new("settings_compute")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("分配策略:");
                let strategies = [
                    (AssignStrategy::Manual, "手动"),
                    (AssignStrategy::LeastMemory, "最小显存优先"),
                    (AssignStrategy::RoundRobin, "轮询"),
                    (AssignStrategy::Single(None), "单设备"),
                ];
                let current_label = strategies
                    .iter()
                    .find(|(s, _)| {
                        std::mem::discriminant(s)
                            == std::mem::discriminant(&config.compute.strategy)
                    })
                    .map(|(_, l)| *l)
                    .unwrap_or("未知");
                egui::ComboBox::from_id_salt("strategy_select")
                    .selected_text(current_label)
                    .show_ui(ui, |ui| {
                        for (strategy, label) in &strategies {
                            ui.selectable_value(
                                &mut config.compute.strategy,
                                strategy.clone(),
                                *label,
                            );
                        }
                    });
                ui.end_row();

                ui.label("允许显存超额:");
                ui.checkbox(&mut config.compute.allow_overcommit, "");
                ui.end_row();

                ui.label("刷新间隔 (秒):");
                ui.add(
                    egui::DragValue::new(&mut config.compute.refresh_interval_secs).range(1..=60),
                );
                ui.end_row();
            });

        ui.add_space(16.0);

        // ── 端口 ──
        ui.strong("端口");
        ui.add_space(4.0);
        egui::Grid::new("settings_ports")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("端口范围:");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut config.ports.range_start).range(1024..=65535),
                    );
                    ui.label("—");
                    ui.add(
                        egui::DragValue::new(&mut config.ports.range_end).range(1024..=65535),
                    );
                });
                ui.end_row();
            });

        ui.add_space(16.0);

        // ── 模型 ──
        ui.strong("模型");
        ui.add_space(4.0);
        egui::Grid::new("settings_models")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("缓存目录:");
                ui.text_edit_singleline(&mut config.models.cache_dir);
                ui.end_row();

                ui.label("HF 镜像:");
                ui.text_edit_singleline(&mut config.models.hf_endpoint);
                ui.end_row();

                ui.label("默认下载源:");
                egui::ComboBox::from_id_salt("source_select")
                    .selected_text(if config.models.default_source == "modelscope" {
                        "ModelScope"
                    } else {
                        "HuggingFace"
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut config.models.default_source,
                            "huggingface".to_string(),
                            "HuggingFace",
                        );
                        ui.selectable_value(
                            &mut config.models.default_source,
                            "modelscope".to_string(),
                            "ModelScope",
                        );
                    });
                ui.end_row();
            });

        ui.add_space(16.0);

        // ── Python 环境 ──
        ui.strong("Python 环境");
        ui.add_space(4.0);
        egui::Grid::new("settings_python")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Python 路径:");
                ui.text_edit_singleline(&mut config.python.path);
                ui.end_row();

                ui.label("uv 路径:");
                ui.text_edit_singleline(&mut config.python.uv_path);
                ui.end_row();
            });

        ui.add_space(16.0);

        // ── 管线 ──
        ui.strong("管线引擎");
        ui.add_space(4.0);
        egui::Grid::new("settings_pipeline")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("最大并行数:");
                ui.add(egui::DragValue::new(&mut config.pipeline.max_parallel).range(1..=16));
                ui.end_row();

                ui.label("默认超时 (秒):");
                ui.add(
                    egui::DragValue::new(&mut config.pipeline.default_timeout_secs)
                        .range(10..=7200),
                );
                ui.end_row();

                ui.label("保留工作目录:");
                ui.checkbox(&mut config.pipeline.keep_workspace, "");
                ui.end_row();

                ui.label("工作目录:");
                ui.text_edit_singleline(&mut config.pipeline.workspace_dir);
                ui.end_row();
            });

        ui.add_space(16.0);

        // ── 界面 ──
        ui.strong("界面");
        ui.add_space(4.0);
        egui::Grid::new("settings_ui")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("缩放:");
                ui.add(
                    egui::DragValue::new(&mut config.ui.scale_factor)
                        .range(0.5..=3.0)
                        .speed(0.1),
                );
                ui.end_row();

                ui.label("字号:");
                ui.add(egui::DragValue::new(&mut config.ui.font_size).range(10.0..=24.0));
                ui.end_row();
            });

        ui.add_space(24.0);

        // ── 保存按钮 ──
        ui.separator();
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("💾 保存配置").clicked() {
                let config_dir = std::env::current_dir()
                    .unwrap_or_default()
                    .join("config");
                match config.save(&config_dir) {
                    Ok(()) => {
                        *status_message = Some((
                            "✅ 配置已保存".into(),
                            std::time::Instant::now(),
                        ));
                    }
                    Err(e) => {
                        *status_message = Some((
                            format!("❌ 保存失败: {e}"),
                            std::time::Instant::now(),
                        ));
                    }
                }
            }

            if ui.button("🔄 重新加载").clicked() {
                let config_dir = std::env::current_dir()
                    .unwrap_or_default()
                    .join("config");
                match AppConfig::load(&config_dir) {
                    Ok(loaded) => {
                        *config = loaded;
                        *status_message = Some((
                            "✅ 配置已重新加载".into(),
                            std::time::Instant::now(),
                        ));
                    }
                    Err(e) => {
                        *status_message = Some((
                            format!("❌ 加载失败: {e}"),
                            std::time::Instant::now(),
                        ));
                    }
                }
            }
        });
    });
}
