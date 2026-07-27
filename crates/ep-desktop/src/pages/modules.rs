use eframe::egui;
use ep_core::types::{ModuleCategory, ServiceStatus};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{AppCmd, ModuleEntry};

pub fn show(
    ui: &mut egui::Ui,
    modules: &mut [ModuleEntry],
    selected: &mut Option<usize>,
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    ui.heading("模块管理");
    ui.add_space(4.0);

    // ── Summary bar ──
    summary_bar(ui, modules);

    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);

    egui::SidePanel::left("modules_list")
        .default_width(280.0)
        .show_inside(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                if modules.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("未发现模块")
                                .color(egui::Color32::from_gray(140)),
                        );
                        ui.label(
                            egui::RichText::new("请检查 modules/ 目录")
                                .small()
                                .color(egui::Color32::from_gray(110)),
                        );
                    });
                    return;
                }

                let categories = unique_categories(modules);
                for cat in &categories {
                    let count = modules.iter().filter(|m| m.category == *cat).count();
                    let running = modules
                        .iter()
                        .filter(|m| m.category == *cat && m.status.is_running())
                        .count();

                    let header = if running > 0 {
                        format!("{}  ({running}/{count} 运行中)", category_label(cat))
                    } else {
                        format!("{}  ({count})", category_label(cat))
                    };

                    egui::CollapsingHeader::new(header)
                        .default_open(true)
                        .show(ui, |ui| {
                            for (idx, m) in modules.iter().enumerate() {
                                if m.category == *cat {
                                    let is_selected = *selected == Some(idx);
                                    module_list_item(ui, m, idx, is_selected, selected);
                                }
                            }
                        });
                }
            });
        });

    egui::CentralPanel::default().show_inside(ui, |ui| {
        if let Some(idx) = *selected {
            if let Some(m) = modules.get_mut(idx) {
                detail_panel(ui, m, cmd_tx);
            }
        } else {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                ui.label(
                    egui::RichText::new("← 选择一个模块查看详情")
                        .color(egui::Color32::from_gray(140)),
                );
            });
        }
    });
}

// ─── Summary bar ─────────────────────────────────────────────────────────────

fn summary_bar(ui: &mut egui::Ui, modules: &[ModuleEntry]) {
    let total = modules.len();
    let running = modules.iter().filter(|m| m.status.is_running()).count();
    let stopped = modules.iter().filter(|m| m.status == ServiceStatus::Stopped).count();
    let errors = modules
        .iter()
        .filter(|m| matches!(m.status, ServiceStatus::Error(_)))
        .count();
    let not_ready = modules
        .iter()
        .filter(|m| m.status == ServiceStatus::NotReady)
        .count();

    ui.horizontal(|ui| {
        ui.label(format!("共 {total} 个模块"));
        ui.separator();

        if running > 0 {
            ui.colored_label(egui::Color32::from_rgb(80, 220, 80), format!("🟢 {running} 运行中"));
        }
        if stopped > 0 {
            ui.colored_label(egui::Color32::from_rgb(200, 80, 80), format!("🔴 {stopped} 已停止"));
        }
        if errors > 0 {
            ui.colored_label(egui::Color32::from_rgb(255, 100, 100), format!("❌ {errors} 错误"));
        }
        if not_ready > 0 {
            ui.colored_label(egui::Color32::from_gray(160), format!("⚪ {not_ready} 未安装"));
        }
        if total == 0 {
            ui.label("（空）");
        }
    });
}

// ─── Module list item ────────────────────────────────────────────────────────

fn module_list_item(
    ui: &mut egui::Ui,
    m: &ModuleEntry,
    idx: usize,
    is_selected: bool,
    selected: &mut Option<usize>,
) {
    let response = ui.horizontal(|ui| {
        // Colored status dot
        let dot_color = status_color(&m.status);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, dot_color);

        ui.add_space(4.0);

        // Name + version
        let label = if let Some(port) = m.port {
            format!("{}  v{}  :{}", m.name, m.version, port)
        } else {
            format!("{}  v{}", m.name, m.version)
        };

        if ui.selectable_label(is_selected, label).clicked() {
            *selected = Some(idx);
        }
    });

    // Tooltip on hover
    if response.response.hovered() {
        response.response.on_hover_text(format!(
            "{}\n状态: {}\n类别: {}",
            m.name,
            status_text(&m.status),
            category_label(&m.category),
        ));
    }
}

// ─── Detail panel ────────────────────────────────────────────────────────────

fn detail_panel(
    ui: &mut egui::Ui,
    m: &mut ModuleEntry,
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        // ── Header with status dot ──
        ui.horizontal(|ui| {
            let dot_color = status_color(&m.status);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 6.0, dot_color);
            ui.heading(&m.name);
        });

        ui.add_space(4.0);

        // ── Info grid ──
        egui::Grid::new("module_detail_grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("ID:");
                ui.label(egui::RichText::new(&m.id).monospace());
                ui.end_row();

                ui.label("版本:");
                ui.label(&m.version);
                ui.end_row();

                ui.label("类别:");
                ui.label(category_label(&m.category));
                ui.end_row();

                ui.label("状态:");
                ui.colored_label(status_color(&m.status), status_text(&m.status));
                ui.end_row();

                if let Some(ref dev) = m.device {
                    ui.label("设备:");
                    ui.label(dev);
                    ui.end_row();
                }

                if let Some(port) = m.port {
                    ui.label("端口:");
                    ui.label(egui::RichText::new(format!("{port}")).monospace());
                    ui.end_row();
                }

                // Uptime
                if let Some(started) = m.started_at {
                    ui.label("运行时间:");
                    ui.label(format_uptime(started.elapsed()));
                    ui.end_row();
                }
            });

        ui.add_space(4.0);

        // Description
        if !m.description.is_empty() {
            ui.label(
                egui::RichText::new(&m.description)
                    .color(egui::Color32::from_gray(180)),
            );
        }

        ui.add_space(12.0);

        // ── Control buttons ──
        ui.horizontal(|ui| {
            match &m.status {
                ServiceStatus::Running | ServiceStatus::Starting => {
                    if ui
                        .add(
                            egui::Button::new("⏹ 停止")
                                .fill(egui::Color32::from_rgb(180, 60, 60)),
                        )
                        .clicked()
                    {
                        let _ = cmd_tx.send(AppCmd::StopModule(m.id.clone()));
                    }
                }
                ServiceStatus::Stopped => {
                    if ui
                        .add(
                            egui::Button::new("▶ 启动")
                                .fill(egui::Color32::from_rgb(60, 140, 60)),
                        )
                        .clicked()
                    {
                        let _ = cmd_tx.send(AppCmd::StartModule(m.id.clone()));
                    }
                }
                ServiceStatus::NotReady => {
                    ui.label("⚠ 模块不可用（缺少依赖或 manifest 无效）");
                }
                ServiceStatus::Error(_) => {
                    if ui
                        .add(
                            egui::Button::new("🔄 重启")
                                .fill(egui::Color32::from_rgb(180, 140, 40)),
                        )
                        .clicked()
                    {
                        let _ = cmd_tx.send(AppCmd::StopModule(m.id.clone()));
                        let _ = cmd_tx.send(AppCmd::StartModule(m.id.clone()));
                    }
                }
                ServiceStatus::Preparing => {
                    ui.spinner();
                    ui.label("准备中...");
                }
            }
        });

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(4.0);

        // ── Log viewer ──
        log_viewer(ui, m);
    });
}

// ─── Log viewer ──────────────────────────────────────────────────────────────

fn log_viewer(ui: &mut egui::Ui, m: &mut ModuleEntry) {
    ui.horizontal(|ui| {
        ui.strong(format!("日志 ({})", m.logs.len()));

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("清空").clicked() {
                m.logs.clear();
            }
        });
    });

    ui.add_space(4.0);

    let log_height = ui.available_height().clamp(120.0, 400.0);
    egui::Frame::group(ui.style()).show(ui, |ui| {
        egui::ScrollArea::vertical()
            .max_height(log_height)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if m.logs.is_empty() {
                    ui.label(
                        egui::RichText::new("（无日志）")
                            .color(egui::Color32::from_gray(120)),
                    );
                } else {
                    egui::Grid::new("log_grid")
                        .num_columns(2)
                        .spacing([8.0, 1.0])
                        .show(ui, |ui| {
                            for (i, line) in m.logs.iter().enumerate() {
                                ui.label(
                                    egui::RichText::new(format!("{:>4}", i + 1))
                                        .monospace()
                                        .small()
                                        .color(egui::Color32::from_gray(90)),
                                );
                                ui.label(
                                    egui::RichText::new(line)
                                        .monospace()
                                        .small(),
                                );
                                ui.end_row();
                            }
                        });
                }
            });
    });
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn unique_categories(modules: &[ModuleEntry]) -> Vec<ModuleCategory> {
    let mut cats = Vec::new();
    for m in modules {
        if !cats.contains(&m.category) {
            cats.push(m.category);
        }
    }
    cats
}

fn status_color(s: &ServiceStatus) -> egui::Color32 {
    match s {
        ServiceStatus::NotReady => egui::Color32::from_gray(120),
        ServiceStatus::Stopped => egui::Color32::from_rgb(200, 80, 80),
        ServiceStatus::Preparing | ServiceStatus::Starting => {
            egui::Color32::from_rgb(230, 200, 60)
        }
        ServiceStatus::Running => egui::Color32::from_rgb(80, 220, 80),
        ServiceStatus::Error(_) => egui::Color32::from_rgb(255, 80, 80),
    }
}

fn status_text(s: &ServiceStatus) -> String {
    match s {
        ServiceStatus::NotReady => "未安装".into(),
        ServiceStatus::Stopped => "已停止".into(),
        ServiceStatus::Preparing => "准备中".into(),
        ServiceStatus::Starting => "启动中".into(),
        ServiceStatus::Running => "运行中".into(),
        ServiceStatus::Error(e) => format!("错误: {e}"),
    }
}

fn category_label(c: &ModuleCategory) -> String {
    match c {
        ModuleCategory::Asr => "语音识别 (ASR)".into(),
        ModuleCategory::Tts => "语音合成 (TTS)".into(),
        ModuleCategory::Denoise => "降噪".into(),
        ModuleCategory::Ocr => "文字识别 (OCR)".into(),
        ModuleCategory::Image => "图像处理".into(),
        ModuleCategory::Translate => "翻译".into(),
        ModuleCategory::Video => "视频处理".into(),
        ModuleCategory::Face => "人脸识别".into(),
        ModuleCategory::Custom => "自定义".into(),
    }
}

fn format_uptime(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs} 秒")
    } else if secs < 3600 {
        format!("{} 分 {} 秒", secs / 60, secs % 60)
    } else {
        format!("{} 时 {} 分 {} 秒", secs / 3600, (secs % 3600) / 60, secs % 60)
    }
}
