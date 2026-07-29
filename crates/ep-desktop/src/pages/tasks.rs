use eframe::egui;
use ep_core::pipeline::runner::TaskSummary;
use ep_core::types::{ServiceStatus, TaskStatus};

use crate::app::ModuleEntry;

pub fn show(
    ui: &mut egui::Ui,
    modules: &[ModuleEntry],
    tasks: &[TaskSummary],
) {
    ui.heading("任务中心");
    ui.add_space(8.0);

    egui::ScrollArea::vertical().show(ui, |ui| {
        // ── 管线任务 ──
        ui.strong("管线任务");
        ui.add_space(4.0);

        if tasks.is_empty() {
            ui.label(
                egui::RichText::new("暂无管线任务记录")
                    .color(egui::Color32::from_gray(120)),
            );
        } else {
            for task in tasks {
                task_row(ui, task);
            }
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);

        // ── 运行中的服务 ──
        ui.strong("运行中的服务");
        ui.add_space(4.0);

        let running: Vec<&ModuleEntry> = modules
            .iter()
            .filter(|m| m.status.is_running() || m.status == ServiceStatus::Starting)
            .collect();

        if running.is_empty() {
            ui.label(
                egui::RichText::new("无运行中的服务")
                    .color(egui::Color32::from_gray(120)),
            );
        } else {
            egui::Grid::new("tasks_running")
                .striped(true)
                .min_col_width(80.0)
                .show(ui, |ui| {
                    ui.strong("模块");
                    ui.strong("类别");
                    ui.strong("状态");
                    ui.strong("端口");
                    ui.strong("运行时间");
                    ui.end_row();

                    for m in &running {
                        ui.label(&m.name);
                        ui.label(m.category.to_string());
                        ui.colored_label(status_color(&m.status), status_text(&m.status));
                        ui.label(
                            m.port
                                .map(|p| p.to_string())
                                .unwrap_or_else(|| "-".into()),
                        );
                        ui.label(
                            m.started_at
                                .map(|t| format_uptime(t.elapsed()))
                                .unwrap_or_else(|| "-".into()),
                        );
                        ui.end_row();
                    }
                });
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);

        // ── 全部模块状态 ──
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

// ─── 管线任务行（可展开详情） ─────────────────────────────────────────────────

fn task_row(ui: &mut egui::Ui, task: &TaskSummary) {
    let status_icon = task_status_icon(&task.status);
    let status_label = task_status_label(&task.status);
    let status_col = task_status_color(&task.status);

    let header_text = format!(
        "#{}  {}  {} {}  {}/{} 节点",
        task.id, task.pipeline_name, status_icon, status_label,
        task.completed_nodes, task.node_count,
    );

    egui::CollapsingHeader::new(
        egui::RichText::new(header_text).color(status_col),
    )
    .default_open(false)
    .show(ui, |ui| {
        // 进度条
        let progress = if task.node_count > 0 {
            task.completed_nodes as f32 / task.node_count as f32
        } else {
            0.0
        };
        ui.add(
            egui::ProgressBar::new(progress)
                .desired_width(200.0)
                .fill(status_col),
        );

        // 耗时 / 错误信息
        if let Some(ref started) = task.started_at {
            if let Some(ref finished) = task.finished_at {
                ui.label(format!("开始: {started}  完成: {finished}"));
            } else {
                ui.label(format!("开始: {started}  (进行中)"));
            }
        }

        if let TaskStatus::Failed(ref err) = task.status {
            ui.colored_label(
                egui::Color32::from_rgb(255, 80, 80),
                format!("错误: {err}"),
            );
        }
    });
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn task_status_icon(s: &TaskStatus) -> &'static str {
    match s {
        TaskStatus::Completed => "✅",
        TaskStatus::Running => "🔄",
        TaskStatus::Pending => "⏳",
        TaskStatus::Failed(_) => "❌",
        TaskStatus::Cancelled => "🚫",
    }
}

fn task_status_label(s: &TaskStatus) -> &'static str {
    match s {
        TaskStatus::Pending => "等待",
        TaskStatus::Running => "运行",
        TaskStatus::Completed => "完成",
        TaskStatus::Failed(_) => "失败",
        TaskStatus::Cancelled => "已取消",
    }
}

fn task_status_color(s: &TaskStatus) -> egui::Color32 {
    match s {
        TaskStatus::Completed => egui::Color32::from_rgb(80, 220, 80),
        TaskStatus::Running => egui::Color32::from_rgb(80, 160, 255),
        TaskStatus::Failed(_) => egui::Color32::from_rgb(255, 80, 80),
        TaskStatus::Pending => egui::Color32::from_rgb(160, 160, 160),
        TaskStatus::Cancelled => egui::Color32::from_rgb(230, 200, 60),
    }
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
