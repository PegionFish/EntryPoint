use eframe::egui;
use ep_core::types::{ModuleCategory, ServiceStatus};

use crate::app::ModuleStatus;

pub fn show(ui: &mut egui::Ui, modules: &[ModuleStatus], selected: &mut Option<usize>) {
    ui.heading("模块管理");
    ui.add_space(8.0);

    egui::SidePanel::left("modules_list")
        .default_width(260.0)
        .show_inside(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let categories = unique_categories(modules);
                for cat in &categories {
                    egui::CollapsingHeader::new(cat.to_string())
                        .default_open(true)
                        .show(ui, |ui| {
                            for (idx, m) in modules.iter().enumerate() {
                                if m.category == *cat {
                                    let is_selected = *selected == Some(idx);
                                    let label = format!(
                                        "{} {}",
                                        status_icon(&m.status),
                                        m.name
                                    );
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
            if let Some(m) = modules.get(idx) {
                detail_panel(ui, m);
            }
        } else {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                ui.label("← 选择一个模块查看详情");
            });
        }
    });
}

fn detail_panel(ui: &mut egui::Ui, m: &ModuleStatus) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.heading(&m.name);
        ui.add_space(4.0);
        ui.label(format!("版本: {}", m.version));
        ui.label(format!("类别: {}", m.category));
        ui.label(format!("状态: {}", status_text(&m.status)));
        ui.add_space(8.0);
        ui.label(&m.description);
        ui.add_space(16.0);

        ui.horizontal(|ui| {
            match &m.status {
                ServiceStatus::Running => {
                    if ui.button("停止").clicked() {}
                }
                ServiceStatus::Stopped | ServiceStatus::NotReady => {
                    if ui.button("启动").clicked() {}
                }
                _ => {}
            }
            if ui.button("查看日志").clicked() {}
        });
    });
}

fn unique_categories(modules: &[ModuleStatus]) -> Vec<ModuleCategory> {
    let mut cats: Vec<ModuleCategory> = Vec::new();
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
