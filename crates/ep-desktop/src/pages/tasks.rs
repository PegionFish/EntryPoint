use eframe::egui;
use ep_core::types::ServiceStatus;

use crate::app::ModuleEntry;

pub fn show(ui: &mut egui::Ui, modules: &[ModuleEntry]) {
    ui.heading("任务中心");
    ui.add_space(8.0);

    egui::ScrollArea::vertical().show(ui, |ui| {
        // ── 运行中的模块服务 ──
        ui.strong("运行中的服务");
        ui.add_space(4.0);

        let running: Vec<&ModuleEntry> = modules
            .iter()
            .filter(|m| m.status.is_running() || m.status == ServiceStatus::Starting)
            .collect();

        if running.is_empty() {
            ui.label(egui::RichText::new("无运行中的服务").color(egui::Color32::from_gray(120)));
        } else {
            egui::Grid::new("tasks_running")
                .striped(true)
                .min_col_width(100.0)
                .show(ui, |ui| {
                    ui.strong("模块");
                    ui.strong("状态");
                    ui.strong("设备");
                    ui.strong("端口");
                    ui.end_row();

                    for m in &running {
                        ui.label(&m.name);
                        ui.label(status_text(&m.status));
                        ui.label(m.device.as_deref().unwrap_or("-"));
                        ui.label(
                            m.port
                                .map(|p| p.to_string())
                                .unwrap_or_else(|| "-".into()),
                        );
                        ui.end_row();
                    }
                });
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);

        // ── 所有模块状态 ──
        ui.strong("全部模块状态");
        ui.add_space(4.0);

        if modules.is_empty() {
            ui.label("未发现模块");
        } else {
            egui::Grid::new("tasks_all")
                .striped(true)
                .min_col_width(100.0)
                .show(ui, |ui| {
                    ui.strong("模块");
                    ui.strong("类别");
                    ui.strong("状态");
                    ui.strong("端口");
                    ui.end_row();

                    for m in modules {
                        ui.label(&m.name);
                        ui.label(m.category.to_string());
                        ui.label(status_text(&m.status));
                        ui.label(
                            m.port
                                .map(|p| p.to_string())
                                .unwrap_or_else(|| "-".into()),
                        );
                        ui.end_row();
                    }
                });
        }
    });
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
