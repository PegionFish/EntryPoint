use eframe::egui;
use ep_core::types::{ComputeDevice, ServiceStatus};

use crate::app::ModuleEntry;

// ─── 颜色常量 ────────────────────────────────────────────────────────────────

const COLOR_GOOD: egui::Color32 = egui::Color32::from_rgb(80, 220, 80);
const COLOR_ERROR: egui::Color32 = egui::Color32::from_rgb(255, 80, 80);
const COLOR_NEUTRAL: egui::Color32 = egui::Color32::from_rgb(120, 120, 120);

// ─── 主入口 ──────────────────────────────────────────────────────────────────

pub fn show(
    ui: &mut egui::Ui,
    devices: &[ComputeDevice],
    modules: &[ModuleEntry],
    dep_report: Option<&ep_core::deps::DepReport>,
) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.heading("仪表盘");
        ui.add_space(8.0);

        // ── 统计卡片 ──
        stats_cards(ui, devices, modules);

        ui.add_space(16.0);

        // ── 依赖检测 ──
        ui.strong("依赖检测");
        ui.add_space(4.0);

        match dep_report {
            Some(report) => dep_section(ui, report),
            None => {
                ui.colored_label(COLOR_NEUTRAL, "点击刷新检测依赖");
            }
        }

        ui.add_space(16.0);

        // ── 计算设备 ──
        ui.strong("计算设备");
        ui.add_space(4.0);

        if devices.is_empty() {
            ui.label("未检测到计算设备");
        } else {
            device_cards(ui, devices);
        }

        ui.add_space(16.0);

        // ── 模块状态概览 ──
        ui.strong("模块状态概览");
        ui.add_space(4.0);

        if modules.is_empty() {
            ui.label("未发现模块（请检查 modules/ 目录）");
        } else {
            module_table(ui, modules);
        }
    });
}

// ─── 统计卡片 ────────────────────────────────────────────────────────────────

fn stats_cards(ui: &mut egui::Ui, devices: &[ComputeDevice], modules: &[ModuleEntry]) {
    let running = modules
        .iter()
        .filter(|m| m.status.is_running())
        .count();
    let errors = modules
        .iter()
        .filter(|m| matches!(m.status, ServiceStatus::Error(_)))
        .count();

    let cards: [(&str, String, egui::Color32); 4] = [
        ("设备", devices.len().to_string(), COLOR_NEUTRAL),
        ("模块", modules.len().to_string(), COLOR_NEUTRAL),
        ("运行中", running.to_string(), if running > 0 { COLOR_GOOD } else { COLOR_NEUTRAL }),
        ("错误", errors.to_string(), if errors > 0 { COLOR_ERROR } else { COLOR_GOOD }),
    ];

    ui.horizontal_wrapped(|ui| {
        for (label, value, color) in &cards {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_width(120.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(4.0);
                    ui.colored_label(
                        *color,
                        egui::RichText::new(value).size(28.0).strong(),
                    );
                    ui.add_space(2.0);
                    ui.label(*label);
                    ui.add_space(4.0);
                });
            });
        }
    });
}

// ─── 依赖检测 ────────────────────────────────────────────────────────────────

fn dep_section(ui: &mut egui::Ui, report: &ep_core::deps::DepReport) {
    // ffmpeg 状态
    ui.horizontal(|ui| {
        if report.ffmpeg.available {
            ui.colored_label(COLOR_GOOD, "✅");
            let mut text = "ffmpeg: 可用".to_string();
            if let Some(v) = &report.ffmpeg.version {
                text.push_str(&format!(" ({v}"));
                if let Some(p) = &report.ffmpeg.path {
                    text.push_str(&format!(", {p}"));
                }
                text.push(')');
            } else if let Some(p) = &report.ffmpeg.path {
                text.push_str(&format!(" ({p})"));
            }
            ui.colored_label(COLOR_GOOD, &text);
        } else {
            ui.colored_label(COLOR_ERROR, "❌");
            ui.colored_label(COLOR_ERROR, "ffmpeg: 未找到");
        }
    });

    if let Some(guidance) = &report.ffmpeg.guidance {
        if !report.ffmpeg.available {
            ui.indent("ffmpeg_guidance", |ui| {
                ui.colored_label(COLOR_NEUTRAL, guidance);
            });
        }
    }

    // torch CUDA 状态
    if !report.torch_cuda.is_empty() {
        ui.add_space(8.0);
        ui.label("torch CUDA:");
        ui.indent("torch_cuda_list", |ui| {
            for tc in &report.torch_cuda {
                ui.horizontal(|ui| {
                    if tc.cuda_available {
                        ui.colored_label(COLOR_GOOD, "✅");
                        let mut text = format!("{}: ", tc.module_id);
                        if let Some(v) = &tc.torch_version {
                            text.push_str(&format!("torch {v}, CUDA 可用"));
                        } else {
                            text.push_str("CUDA 可用");
                        }
                        ui.colored_label(COLOR_GOOD, &text);
                    } else if tc.torch_version.is_some() {
                        ui.colored_label(COLOR_ERROR, "❌");
                        let text = format!(
                            "{}: torch {}, CUDA 不可用",
                            tc.module_id,
                            tc.torch_version.as_deref().unwrap_or("?")
                        );
                        ui.colored_label(COLOR_ERROR, &text);
                    } else {
                        ui.colored_label(COLOR_ERROR, "❌");
                        ui.colored_label(
                            COLOR_ERROR,
                            format!("{}: torch 未安装", tc.module_id),
                        );
                    }
                });

                if let Some(guidance) = &tc.guidance {
                    ui.indent(format!("{}_guidance", tc.module_id), |ui| {
                        ui.colored_label(COLOR_NEUTRAL, guidance);
                    });
                }
            }
        });
    }
}

fn device_cards(ui: &mut egui::Ui, devices: &[ComputeDevice]) {
    ui.horizontal_wrapped(|ui| {
        for dev in devices {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_width(220.0);
                ui.strong(&dev.name);
                ui.label(format!("后端: {}", dev.backend));

                if let (Some(total), Some(used)) = (dev.total_memory_mb, dev.used_memory_mb) {
                    let frac = used as f32 / total.max(1) as f32;
                    ui.label(format!("显存: {used} / {total} MB"));
                    ui.add(
                        egui::ProgressBar::new(frac)
                            .text(format!("{:.0}%", frac * 100.0)),
                    );
                } else {
                    ui.label("显存: 未知");
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

fn module_table(ui: &mut egui::Ui, modules: &[ModuleEntry]) {
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
                ui.label(
                    m.port
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "-".into()),
                );
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
