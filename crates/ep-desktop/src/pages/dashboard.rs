//! 仪表盘 — 统计概览、依赖检测、计算设备、模块状态一览。

use eframe::egui;
use ep_core::config::AppConfig;
use ep_core::types::{ComputeDevice, ServiceStatus};

use crate::app::ModuleEntry;
use crate::i18n::tr;
use crate::pages::modules::{category_label, service_label};
use crate::ui::{
    badge, card, card_grid, empty_state, page_header, responsive_columns, section_title,
    service_status, Palette,
};

/// 页面内容四周的留白（px）
const PAGE_MARGIN: f32 = 18.0;
/// 区块之间的垂直间距（px）
const SECTION_GAP: f32 = 20.0;

// ─── 主入口 ──────────────────────────────────────────────────────────────────

pub fn show(
    ui: &mut egui::Ui,
    config: &AppConfig,
    devices: &[ComputeDevice],
    modules: &[ModuleEntry],
    dep_report: Option<&ep_core::deps::DepReport>,
) {
    let lang = ep_core::i18n::normalize_language(&config.general.language);
    let pal = Palette::new(ui.style().visuals.dark_mode);

    // 发布权威快照：设备列表（管线编辑器 VRAM 账本消费）+ 模块状态
    // （统一页镜像消费）。仪表盘为默认首页，启动后快照即存在。
    crate::pages::publish_device_snapshot(ui.ctx(), devices);
    crate::pages::publish_module_snapshot(ui.ctx(), modules);

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.add_space(PAGE_MARGIN);
            ui.vertical(|ui| {
                page_header(ui, &tr(lang, "desktopPages.dashboard.title", &[]), |_| {});
                ui.add_space(12.0);

                stats_section(ui, lang, &pal, devices, modules);
                ui.add_space(SECTION_GAP);

                dep_section(ui, lang, &pal, dep_report);
                ui.add_space(SECTION_GAP);

                device_section(ui, lang, &pal, devices);
                ui.add_space(SECTION_GAP);

                module_section(ui, lang, &pal, modules);
                ui.add_space(8.0);
            });
            ui.add_space(PAGE_MARGIN);
        });
        ui.add_space(16.0);
    });
}

// ─── 统计卡片 ────────────────────────────────────────────────────────────────

struct StatCard {
    label: String,
    value: String,
    color: egui::Color32,
}

fn stats_section(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    devices: &[ComputeDevice],
    modules: &[ModuleEntry],
) {
    let running = modules.iter().filter(|m| m.status.is_running()).count();
    let errors = modules
        .iter()
        .filter(|m| matches!(m.status, ServiceStatus::Error(_)))
        .count();

    let stats = [
        StatCard {
            label: tr(lang, "desktopPages.dashboard.stat.devices", &[]),
            value: devices.len().to_string(),
            color: pal.text,
        },
        StatCard {
            label: tr(lang, "desktopPages.dashboard.stat.modules", &[]),
            value: modules.len().to_string(),
            color: pal.text,
        },
        StatCard {
            label: tr(lang, "desktopPages.dashboard.stat.running", &[]),
            value: running.to_string(),
            color: if running > 0 { pal.success } else { pal.text },
        },
        StatCard {
            label: tr(lang, "desktopPages.dashboard.stat.errors", &[]),
            value: errors.to_string(),
            color: if errors > 0 { pal.danger } else { pal.text },
        },
    ];

    let cols = responsive_columns(ui.available_width(), 170.0, 12.0);
    card_grid(ui, cols, &stats, |ui, s| {
        card(ui, pal, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(s.value.as_str())
                        .size(28.0)
                        .strong()
                        .color(s.color),
                );
                ui.add_space(2.0);
                ui.label(egui::RichText::new(s.label.as_str()).color(pal.text_dim));
                ui.add_space(6.0);
            });
        });
    });
}

// ─── 依赖检测 ────────────────────────────────────────────────────────────────

fn dep_section(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    report: Option<&ep_core::deps::DepReport>,
) {
    section_title(ui, &tr(lang, "desktopPages.dashboard.deps.title", &[]));
    ui.add_space(8.0);

    let Some(report) = report else {
        ui.label(egui::RichText::new(tr(lang, "desktopPages.dashboard.deps.checking", &[])).color(pal.text_dim));
        return;
    };

    card(ui, pal, |ui| {
        // ffmpeg 行
        let mut ffmpeg_detail = "ffmpeg".to_string();
        if let Some(v) = &report.ffmpeg.version {
            ffmpeg_detail.push_str(&format!(" · {v}"));
        }
        if let Some(p) = &report.ffmpeg.path {
            ffmpeg_detail.push_str(&format!(" · {p}"));
        }
        dep_row(
            ui,
            lang,
            pal,
            report.ffmpeg.available,
            &ffmpeg_detail,
            report.ffmpeg.guidance.as_deref(),
        );

        // 每个模块 venv 的 torch CUDA 一行
        for tc in &report.torch_cuda {
            ui.add_space(8.0);
            let detail = match &tc.torch_version {
                Some(v) => format!("{} · torch {v}", tc.module_id),
                None => format!(
                    "{} · {}",
                    tc.module_id,
                    tr(lang, "desktopPages.dashboard.deps.torchNotInstalled", &[])
                ),
            };
            dep_row(ui, lang, pal, tc.cuda_available, &detail, tc.guidance.as_deref());
        }
    });
}

/// 单行依赖状态：可用/缺失徽章 + mono 详情 + 弱化指引
fn dep_row(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    ok: bool,
    detail: &str,
    guidance: Option<&str>,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        if ok {
            badge(ui, pal, pal.success, tr(lang, "desktopPages.dashboard.deps.available", &[]));
        } else {
            badge(ui, pal, pal.danger, tr(lang, "common.status.missing", &[]));
        }
        ui.label(
            egui::RichText::new(detail)
                .monospace()
                .color(pal.text_dim),
        );
    });
    if let Some(guidance) = guidance {
        ui.add_space(2.0);
        ui.label(egui::RichText::new(guidance).small().color(pal.text_faint));
    }
}

// ─── 计算设备 ────────────────────────────────────────────────────────────────

fn device_section(ui: &mut egui::Ui, lang: &str, pal: &Palette, devices: &[ComputeDevice]) {
    section_title(ui, &tr(lang, "desktopPages.dashboard.devices.title", &[]));
    ui.add_space(8.0);

    if devices.is_empty() {
        empty_state(
            ui,
            pal,
            "🖥️",
            &tr(lang, "desktopPages.dashboard.devices.empty.title", &[]),
            &tr(lang, "desktopPages.dashboard.devices.empty.hint", &[]),
        );
        return;
    }

    let cols = responsive_columns(ui.available_width(), 260.0, 12.0);
    card_grid(ui, cols, devices, |ui, dev| {
        card(ui, pal, |ui| {
            // 名称 + 后端徽章
            ui.horizontal(|ui| {
                ui.strong(&dev.name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    badge(ui, pal, pal.info, dev.backend.to_string());
                });
            });
            ui.add_space(8.0);

            // 显存（CPU 等无显存数据的设备跳过）
            if let (Some(total), Some(used)) = (dev.total_memory_mb, dev.used_memory_mb) {
                let frac = (used as f32 / total.max(1) as f32).min(1.0);
                let fill = if frac > 0.95 {
                    pal.danger
                } else if frac > 0.80 {
                    pal.warning
                } else {
                    pal.primary
                };
                ui.add(egui::ProgressBar::new(frac).fill(fill));
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!("{used} / {total} MB"))
                        .monospace()
                        .small()
                        .color(pal.text_dim),
                );
            }

            // 利用率 / 温度
            let mut meta: Vec<String> = Vec::new();
            if let Some(u) = dev.utilization {
                let value = u.to_string();
                meta.push(tr(
                    lang,
                    "desktopPages.dashboard.devices.utilization",
                    &[("value", &value)],
                ));
            }
            if let Some(t) = dev.temperature {
                let value = t.to_string();
                meta.push(tr(
                    lang,
                    "desktopPages.dashboard.devices.temperature",
                    &[("value", &value)],
                ));
            }
            if !meta.is_empty() {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(meta.join(" · ")).color(pal.text_dim));
            }
        });
    });
}

// ─── 模块状态概览 ────────────────────────────────────────────────────────────

fn module_section(ui: &mut egui::Ui, lang: &str, pal: &Palette, modules: &[ModuleEntry]) {
    section_title(ui, &tr(lang, "desktopPages.dashboard.modules.title", &[]));
    ui.add_space(8.0);

    if modules.is_empty() {
        empty_state(
            ui,
            pal,
            "🧩",
            &tr(lang, "desktopPages.dashboard.modules.empty.title", &[]),
            &tr(lang, "desktopPages.dashboard.modules.empty.hint", &[]),
        );
        return;
    }

    let headers = [
        tr(lang, "desktopPages.dashboard.col.name", &[]),
        tr(lang, "desktopPages.dashboard.col.category", &[]),
        tr(lang, "desktopPages.dashboard.col.status", &[]),
        tr(lang, "desktopPages.dashboard.col.device", &[]),
        tr(lang, "desktopPages.dashboard.col.port", &[]),
    ];

    egui::ScrollArea::horizontal().show(ui, |ui| {
        egui::Grid::new("dashboard_modules")
            .striped(true)
            .min_col_width(64.0)
            .spacing([18.0, 8.0])
            .show(ui, |ui| {
                for head in &headers {
                    ui.strong(head.as_str());
                }
                ui.end_row();

                for m in modules {
                    ui.label(&m.name);
                    ui.label(category_label(lang, &m.category));
                    let meta = service_status(&m.status, pal);
                    badge(ui, pal, meta.color, service_label(lang, &m.status));
                    ui.label(
                        egui::RichText::new(m.device.as_deref().unwrap_or("-")).monospace(),
                    );
                    ui.label(
                        egui::RichText::new(
                            m.port
                                .map(|p| p.to_string())
                                .unwrap_or_else(|| "-".into()),
                        )
                        .monospace(),
                    );
                    ui.end_row();
                }
            });
    });
}
