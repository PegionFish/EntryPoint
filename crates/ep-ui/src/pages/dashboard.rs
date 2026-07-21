use eframe::egui;
use ep_core::types::{ComputeDevice, ServiceStatus};

use crate::app::ModuleStatus;

pub fn show(ui: &mut egui::Ui, devices: &[ComputeDevice], modules: &[ModuleStatus]) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.heading("仪表盘");
        ui.add_space(8.0);

        ui.label("计算设备");
        ui.add_space(4.0);
        device_cards(ui, devices);

        ui.add_space(16.0);
        ui.label("模块状态概览");
        ui.add_space(4.0);
        module_table(ui, modules);
    });
}

fn device_cards(ui: &mut egui::Ui, devices: &[ComputeDevice]) {
    ui.horizontal_wrapped(|ui| {
        for dev in devices {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_width(220.0);
                ui.strong(&dev.name);
                ui.label(format!("后端: {}", dev.backend));

                if let (Some(total), Some(used)) = (dev.total_memory_mb, dev.used_memory_mb) {
                    let frac = used as f32 / total as f32;
                    ui.label(format!("显存: {used} / {total} MB"));
                    ui.add(egui::ProgressBar::new(frac).text(format!("{:.0}%", frac * 100.0)));
                }

                if let Some(util) = dev.utilization {
                    ui.label(format!("利用率: {util}%"));
                }
                if let Some(temp) = dev.temperature {
                    ui.label(format!("温度: {temp}°C"));
                }
            });
        }
    });
}

fn module_table(ui: &mut egui::Ui, modules: &[ModuleStatus]) {
    egui::Grid::new("dashboard_modules")
        .striped(true)
        .min_col_width(80.0)
        .show(ui, |ui| {
            ui.strong("名称");
            ui.strong("类别");
            ui.strong("状态");
            ui.strong("设备");
            ui.strong("端口");
            ui.end_row();

            for m in modules {
                ui.label(&m.name);
                ui.label(m.category.to_string());
                ui.label(status_text(&m.status));
                ui.label(m.device.as_deref().unwrap_or("-"));
                ui.label(m.port.map(|p| p.to_string()).unwrap_or_else(|| "-".into()));
                ui.end_row();
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
