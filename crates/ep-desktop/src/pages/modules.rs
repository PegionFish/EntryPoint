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
    ui.add_space(8.0);

    egui::SidePanel::left("modules_list")
        .default_width(260.0)
        .show_inside(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                if modules.is_empty() {
                    ui.label("未发现模块");
                    return;
                }

                let categories = unique_categories(modules);
                for cat in &categories {
                    egui::CollapsingHeader::new(cat.to_string())
                        .default_open(true)
                        .show(ui, |ui| {
                            for (idx, m) in modules.iter().enumerate() {
                                if m.category == *cat {
                                    let is_selected = *selected == Some(idx);
                                    let label =
                                        format!("{}  {}", status_icon(&m.status), m.name);
                                    if ui.selectable_label(is_selected, label).clicked() {
                                        *selected = Some(idx);
                                    }
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
                ui.label("← 选择一个模块查看详情");
            });
        }
    });
}

fn detail_panel(
    ui: &mut egui::Ui,
    m: &mut ModuleEntry,
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.heading(&m.name);
        ui.add_space(4.0);
        ui.label(format!("ID: {}", m.id));
        ui.label(format!("版本: {}", m.version));
        ui.label(format!("类别: {}", m.category));
        ui.label(format!("状态: {}", status_text(&m.status)));

        if let Some(ref dev) = m.device {
            ui.label(format!("设备: {dev}"));
        }
        if let Some(port) = m.port {
            ui.label(format!("端口: {port}"));
        }

        ui.add_space(4.0);
        ui.label(&m.description);
        ui.add_space(12.0);

        // ── 控制按钮 ──
        ui.horizontal(|ui| {
            match &m.status {
                ServiceStatus::Running | ServiceStatus::Starting => {
                    if ui.button("⏹ 停止").clicked() {
                        let _ = cmd_tx.send(AppCmd::StopModule(m.id.clone()));
                    }
                }
                ServiceStatus::Stopped => {
                    if ui.button("▶ 启动").clicked() {
                        let _ = cmd_tx.send(AppCmd::StartModule(m.id.clone()));
                    }
                }
                ServiceStatus::NotReady => {
                    ui.label("⚠ 模块不可用");
                }
                ServiceStatus::Error(_) => {
                    if ui.button("🔄 重启").clicked() {
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

        // ── 日志面板 ──
        ui.strong("日志");
        ui.add_space(4.0);

        let log_height = ui.available_height().min(400.0);
        egui::ScrollArea::vertical()
            .max_height(log_height)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if m.logs.is_empty() {
                    ui.label(egui::RichText::new("（无日志）").color(egui::Color32::from_gray(120)));
                } else {
                    for line in &m.logs {
                        ui.label(
                            egui::RichText::new(line)
                                .monospace()
                                .small(),
                        );
                    }
                }
            });
    });
}

fn unique_categories(modules: &[ModuleEntry]) -> Vec<ModuleCategory> {
    let mut cats = Vec::new();
    for m in modules {
        if !cats.contains(&m.category) {
            cats.push(m.category);
        }
    }
    cats
}

fn status_icon(s: &ServiceStatus) -> &'static str {
    match s {
        ServiceStatus::NotReady => "⚪",
        ServiceStatus::Stopped => "🔴",
        ServiceStatus::Preparing | ServiceStatus::Starting => "🟡",
        ServiceStatus::Running => "🟢",
        ServiceStatus::Error(_) => "❌",
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
