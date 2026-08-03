//! 任务中心页 — 管线任务进度 + 运行中的服务 + 全部模块状态。

use eframe::egui;
use ep_core::pipeline::runner::TaskSummary;
use ep_core::types::{ServiceStatus, TaskStatus};

use crate::app::ModuleEntry;
use crate::ui::{
    badge, card, empty_state, page_header, section_title, service_status, status_badge, Palette,
};

pub fn show(ui: &mut egui::Ui, modules: &[ModuleEntry], tasks: &[TaskSummary]) {
    let pal = Palette::new(ui.style().visuals.dark_mode);

    page_header(ui, "任务中心", |_| {});
    ui.add_space(8.0);

    egui::ScrollArea::vertical().show(ui, |ui| {
        // ── 管线任务 ──
        section_title(ui, "管线任务");
        ui.add_space(6.0);

        if tasks.is_empty() {
            empty_state(
                ui,
                &pal,
                "📋",
                "暂无管线任务",
                "在管线编辑器中加载并运行管线后，任务会显示在这里",
            );
        } else {
            for task in tasks {
                task_card(ui, &pal, task);
                ui.add_space(8.0);
            }
        }

        ui.add_space(12.0);

        // ── 运行中的服务 ──
        section_title(ui, "运行中的服务");
        ui.add_space(6.0);

        let running: Vec<&ModuleEntry> = modules
            .iter()
            .filter(|m| m.status.is_running() || m.status == ServiceStatus::Starting)
            .collect();

        if running.is_empty() {
            ui.label(egui::RichText::new("当前没有运行中的服务").color(pal.text_dim));
        } else {
            card(ui, &pal, |ui| {
                egui::ScrollArea::horizontal().show(ui, |ui| {
                    module_grid(ui, &pal, "tasks_running_grid", &running, true);
                });
            });
        }

        ui.add_space(16.0);

        // ── 全部模块状态 ──
        section_title(ui, "全部模块状态");
        ui.add_space(6.0);

        if modules.is_empty() {
            ui.label(egui::RichText::new("未发现模块").color(pal.text_dim));
        } else {
            let all: Vec<&ModuleEntry> = modules.iter().collect();
            card(ui, &pal, |ui| {
                egui::ScrollArea::horizontal().show(ui, |ui| {
                    module_grid(ui, &pal, "tasks_all_grid", &all, false);
                });
            });
        }

        ui.add_space(8.0);
    });
}

// ─── 管线任务卡片 ────────────────────────────────────────────────────────────

fn task_card(ui: &mut egui::Ui, pal: &Palette, task: &TaskSummary) {
    let (color, label) = task_status_meta(&task.status, pal);

    card(ui, pal, |ui| {
        // 行1：管线名 + 状态徽章 + 任务 ID（右对齐 mono）
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&task.pipeline_name).strong());
            badge(ui, pal, color, label);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("#{}", task.id))
                        .monospace()
                        .small()
                        .color(pal.text_faint),
                );
            });
        });
        ui.add_space(8.0);

        // 行2：整体进度条（占满可用宽度，填充色 = 状态色）
        let progress = if task.node_count > 0 {
            task.completed_nodes as f32 / task.node_count as f32
        } else {
            0.0
        };
        ui.add(
            egui::ProgressBar::new(progress)
                .desired_width(ui.available_width())
                .fill(color),
        );
        ui.add_space(6.0);

        // 行3：时间（ISO 截短到秒）+ 节点进度
        let mut info = String::new();
        if let Some(started) = &task.started_at {
            if let Some(finished) = &task.finished_at {
                info.push_str(&format!(
                    "开始 {} · 完成 {}",
                    iso_to_secs(started),
                    iso_to_secs(finished)
                ));
            } else {
                info.push_str(&format!("开始 {} · 进行中", iso_to_secs(started)));
            }
            info.push_str("    ");
        }
        info.push_str(&format!("{}/{} 节点", task.completed_nodes, task.node_count));
        ui.label(egui::RichText::new(info).small().color(pal.text_dim));

        // 失败原因
        if let TaskStatus::Failed(err) = &task.status {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(format!("错误：{err}")).small().color(pal.danger));
        }
    });
}

// ─── 模块状态网格（卡片内横向滚动） ──────────────────────────────────────────

fn module_grid(
    ui: &mut egui::Ui,
    pal: &Palette,
    id: &str,
    rows: &[&ModuleEntry],
    with_uptime: bool,
) {
    egui::Grid::new(id)
        .striped(true)
        .spacing([28.0, 10.0])
        .show(ui, |ui| {
            // 表头
            for col in ["模块", "类别", "状态", "端口"] {
                ui.label(egui::RichText::new(col).small().color(pal.text_faint));
            }
            if with_uptime {
                ui.label(egui::RichText::new("运行时间").small().color(pal.text_faint));
            }
            ui.end_row();

            for m in rows {
                ui.label(&m.name);
                ui.label(egui::RichText::new(m.category.to_string()).color(pal.text_dim));
                status_badge(ui, pal, service_status(&m.status, pal));
                ui.label(
                    egui::RichText::new(
                        m.port
                            .map(|p| p.to_string())
                            .unwrap_or_else(|| "-".into()),
                    )
                    .monospace()
                    .color(pal.text_dim),
                );
                if with_uptime {
                    ui.label(
                        egui::RichText::new(
                            m.started_at
                                .map(|t| format_uptime(t.elapsed()))
                                .unwrap_or_else(|| "-".into()),
                        )
                        .color(pal.text_dim),
                    );
                }
                ui.end_row();
            }
        });
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// 任务状态 → (颜色, 文案)。颜色一律取自当前主题色板，禁止硬编码 RGB。
fn task_status_meta(status: &TaskStatus, pal: &Palette) -> (egui::Color32, &'static str) {
    match status {
        TaskStatus::Completed => (pal.success, "完成"),
        TaskStatus::Running => (pal.info, "运行中"),
        TaskStatus::Pending => (pal.neutral, "等待"),
        TaskStatus::Failed(_) => (pal.danger, "失败"),
        TaskStatus::Cancelled => (pal.warning, "已取消"),
    }
}

/// ISO 8601 时间字符串截短到秒（前 19 字符）；长度不足或边界不安全时原样返回
fn iso_to_secs(iso: &str) -> &str {
    iso.get(..19).unwrap_or(iso)
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
