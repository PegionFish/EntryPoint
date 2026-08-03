//! 任务中心页 — 管线任务进度 + 运行中的服务 + 全部模块状态。
//!
//! 用户可见文案经 [`crate::i18n::tr`] 查找；状态/类别文案复用
//! [`crate::pages::modules`] 的本地化 helper，颜色一律取自当前主题色板。

use eframe::egui;
use ep_core::config::AppConfig;
use ep_core::pipeline::runner::TaskSummary;
use ep_core::types::{ServiceStatus, TaskStatus};

use crate::app::ModuleEntry;
use crate::i18n::tr;
use crate::pages::modules::{category_label, service_label};
use crate::ui::{badge, card, empty_state, page_header, section_title, service_status, Palette};

pub fn show(
    ui: &mut egui::Ui,
    config: &AppConfig,
    modules: &[ModuleEntry],
    tasks: &[TaskSummary],
) {
    let lang = ep_core::i18n::normalize_language(&config.general.language);
    let pal = Palette::new(ui.style().visuals.dark_mode);

    page_header(ui, &tr(lang, "tasks.page.title", &[]), |_| {});
    ui.add_space(8.0);

    egui::ScrollArea::vertical().show(ui, |ui| {
        // ── 管线任务 ──
        section_title(ui, &tr(lang, "tasks.stats.pipelineTasks", &[]));
        ui.add_space(6.0);

        if tasks.is_empty() {
            empty_state(
                ui,
                &pal,
                "📋",
                &tr(lang, "tasks.tasks.emptyTitle", &[]),
                &tr(lang, "desktopApp.tasks.emptyHint", &[]),
            );
        } else {
            for task in tasks {
                task_card(ui, lang, &pal, task);
                ui.add_space(8.0);
            }
        }

        ui.add_space(12.0);

        // ── 运行中的服务 ──
        section_title(ui, &tr(lang, "tasks.stats.runningServices", &[]));
        ui.add_space(6.0);

        let running: Vec<&ModuleEntry> = modules
            .iter()
            .filter(|m| m.status.is_running() || m.status == ServiceStatus::Starting)
            .collect();

        if running.is_empty() {
            ui.label(
                egui::RichText::new(tr(lang, "tasks.services.emptyTitle", &[]))
                    .color(pal.text_dim),
            );
        } else {
            card(ui, &pal, |ui| {
                egui::ScrollArea::horizontal().show(ui, |ui| {
                    module_grid(ui, lang, &pal, "tasks_running_grid", &running, true);
                });
            });
        }

        ui.add_space(16.0);

        // ── 全部模块状态 ──
        section_title(ui, &tr(lang, "tasks.stats.totalModules", &[]));
        ui.add_space(6.0);

        if modules.is_empty() {
            ui.label(egui::RichText::new(tr(lang, "desktopApp.tasks.noModules", &[])).color(pal.text_dim));
        } else {
            let all: Vec<&ModuleEntry> = modules.iter().collect();
            card(ui, &pal, |ui| {
                egui::ScrollArea::horizontal().show(ui, |ui| {
                    module_grid(ui, lang, &pal, "tasks_all_grid", &all, false);
                });
            });
        }

        ui.add_space(8.0);
    });
}

// ─── 管线任务卡片 ────────────────────────────────────────────────────────────

fn task_card(ui: &mut egui::Ui, lang: &str, pal: &Palette, task: &TaskSummary) {
    let (color, label) = task_status_meta(lang, &task.status, pal);

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
                info.push_str(&tr(
                    lang,
                    "desktopApp.tasks.startedFinished",
                    &[
                        ("start", iso_to_secs(started)),
                        ("end", iso_to_secs(finished)),
                    ],
                ));
            } else {
                info.push_str(&tr(
                    lang,
                    "desktopApp.tasks.startedRunning",
                    &[("start", iso_to_secs(started))],
                ));
            }
            info.push_str("    ");
        }
        let completed = task.completed_nodes.to_string();
        let total = task.node_count.to_string();
        info.push_str(&tr(
            lang,
            "tasks.task.nodeProgress",
            &[("completed", &completed), ("total", &total)],
        ));
        ui.label(egui::RichText::new(info).small().color(pal.text_dim));

        // 失败原因（ep-core 原始消息以本地化前缀附加原文）
        if let TaskStatus::Failed(err) = &task.status {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(tr(lang, "desktopApp.tasks.error", &[("detail", err)]))
                    .small()
                    .color(pal.danger),
            );
        }
    });
}

// ─── 模块状态网格（卡片内横向滚动） ──────────────────────────────────────────

fn module_grid(
    ui: &mut egui::Ui,
    lang: &str,
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
            let mut headers = vec![
                tr(lang, "common.label.module", &[]),
                tr(lang, "tasks.moduleTable.category", &[]),
                tr(lang, "common.label.status", &[]),
                tr(lang, "desktopPages.dashboard.col.port", &[]),
            ];
            if with_uptime {
                headers.push(tr(lang, "desktopPages.modules.info.uptime", &[]));
            }
            for col in headers {
                ui.label(egui::RichText::new(col).small().color(pal.text_faint));
            }
            ui.end_row();

            for m in rows {
                ui.label(&m.name);
                ui.label(egui::RichText::new(category_label(lang, &m.category)).color(pal.text_dim));
                // 颜色取自主题色板，文案走 i18n（StatusMeta.label 为静态串，不能承载翻译结果）
                let meta = service_status(&m.status, pal);
                badge(ui, pal, meta.color, service_label(lang, &m.status));
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
                                .map(|t| format_uptime(lang, t.elapsed()))
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

/// 任务状态 → (颜色, 本地化文案)。颜色一律取自当前主题色板，禁止硬编码 RGB。
fn task_status_meta(lang: &str, status: &TaskStatus, pal: &Palette) -> (egui::Color32, String) {
    match status {
        TaskStatus::Completed => (pal.success, tr(lang, "common.status.completed", &[])),
        TaskStatus::Running => (pal.info, tr(lang, "common.status.running", &[])),
        TaskStatus::Pending => (pal.neutral, tr(lang, "common.status.pending", &[])),
        TaskStatus::Failed(_) => (pal.danger, tr(lang, "common.status.failed", &[])),
        TaskStatus::Cancelled => (pal.warning, tr(lang, "common.status.cancelled", &[])),
    }
}

/// ISO 8601 时间字符串截短到秒（前 19 字符）；长度不足或边界不安全时原样返回
fn iso_to_secs(iso: &str) -> &str {
    iso.get(..19).unwrap_or(iso)
}

/// 运行时长（与模块页同一套键，保证两页文案一致）
fn format_uptime(lang: &str, d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        let s = secs.to_string();
        tr(lang, "desktopPages.modules.uptime.seconds", &[("s", &s)])
    } else if secs < 3600 {
        let m = (secs / 60).to_string();
        let s = (secs % 60).to_string();
        tr(
            lang,
            "desktopPages.modules.uptime.minutes",
            &[("m", &m), ("s", &s)],
        )
    } else {
        let h = (secs / 3600).to_string();
        let m = ((secs % 3600) / 60).to_string();
        let s = (secs % 60).to_string();
        tr(
            lang,
            "desktopPages.modules.uptime.hours",
            &[("h", &h), ("m", &m), ("s", &s)],
        )
    }
}
