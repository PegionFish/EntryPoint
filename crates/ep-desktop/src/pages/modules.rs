//! 模块管理页 — 响应式 master-detail：左栏按类别分组列表，右栏详情 + 操作 + 日志。
//!
//! 所有颜色取自 [`crate::ui::Palette`]，状态颜色统一走 [`crate::ui::service_status`]，
//! 状态文案走 [`service_label`]（i18n）。

use eframe::egui;
use ep_core::config::AppConfig;
use ep_core::types::{ModuleCategory, ServiceStatus};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{AppCmd, ModuleEntry};
use crate::i18n::tr;
use crate::ui::{
    badge, card, confirm_dialog_with_lang, danger_button, empty_state, page_header, primary_button,
    service_status, subtle_button, Palette, CARD_ROUNDING, CONTROL_ROUNDING,
};

/// 左栏列表项行高
const ITEM_HEIGHT: f32 = 30.0;

pub fn show(
    ui: &mut egui::Ui,
    config: &AppConfig,
    modules: &mut [ModuleEntry],
    selected: &mut Option<usize>,
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    let lang = ep_core::i18n::normalize_language(&config.general.language);
    let pal = Palette::new(ui.style().visuals.dark_mode);

    // ── 页头 + 汇总 ──
    page_header(ui, &tr(lang, "desktopPages.modules.title", &[]), |ui| {
        let count = modules.len().to_string();
        ui.label(
            egui::RichText::new(tr(
                lang,
                "desktopPages.modules.total",
                &[("count", &count)],
            ))
            .color(pal.text_dim),
        );
    });
    ui.add_space(2.0);
    summary_bar(ui, lang, &pal, modules);
    ui.add_space(6.0);

    // ── 响应式 master-detail ──
    // 注：egui 面板宽度按 id 缓存，default_width 仅首次生效；resizable 允许用户手动调整。
    let narrow = ui.available_width() < 760.0;

    egui::SidePanel::left("modules_list")
        .default_width(if narrow { 200.0 } else { 270.0 })
        .min_width(170.0)
        .max_width(360.0)
        .resizable(true)
        .frame(
            egui::Frame::new()
                .fill(pal.card)
                .inner_margin(egui::Margin::symmetric(8, 8)),
        )
        .show_inside(ui, |ui| {
            module_list(ui, lang, &pal, modules, selected);
        });

    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(pal.bg)
                .inner_margin(egui::Margin::symmetric(8, 4)),
        )
        .show_inside(ui, |ui| {
            detail_area(ui, lang, &pal, modules, selected, cmd_tx);
        });
}

// ─── 汇总条 ──────────────────────────────────────────────────────────────────

/// 状态计数徽章（0 不显示）
fn summary_bar(ui: &mut egui::Ui, lang: &str, pal: &Palette, modules: &[ModuleEntry]) {
    let running = modules.iter().filter(|m| m.status.is_running()).count();
    let stopped = modules
        .iter()
        .filter(|m| m.status == ServiceStatus::Stopped)
        .count();
    let errors = modules
        .iter()
        .filter(|m| matches!(m.status, ServiceStatus::Error(_)))
        .count();
    let not_ready = modules
        .iter()
        .filter(|m| m.status == ServiceStatus::NotReady)
        .count();

    ui.horizontal(|ui| {
        if running > 0 {
            let count = running.to_string();
            badge(
                ui,
                pal,
                pal.success,
                tr(lang, "desktopPages.modules.summary.running", &[("count", &count)]),
            );
        }
        if stopped > 0 {
            let count = stopped.to_string();
            badge(
                ui,
                pal,
                pal.neutral,
                tr(lang, "desktopPages.modules.summary.stopped", &[("count", &count)]),
            );
        }
        if errors > 0 {
            let count = errors.to_string();
            badge(
                ui,
                pal,
                pal.danger,
                tr(lang, "desktopPages.modules.summary.errors", &[("count", &count)]),
            );
        }
        if not_ready > 0 {
            let count = not_ready.to_string();
            badge(
                ui,
                pal,
                pal.notready,
                tr(lang, "desktopPages.modules.summary.notReady", &[("count", &count)]),
            );
        }
        if modules.is_empty() {
            ui.label(
                egui::RichText::new(tr(lang, "desktopPages.modules.summary.none", &[]))
                    .color(pal.text_faint),
            );
        }
    });
}

// ─── 左栏：模块列表 ──────────────────────────────────────────────────────────

fn module_list(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    modules: &[ModuleEntry],
    selected: &mut Option<usize>,
) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        if modules.is_empty() {
            empty_state(
                ui,
                pal,
                "🧩",
                &tr(lang, "desktopPages.modules.empty.title", &[]),
                &tr(lang, "desktopPages.modules.empty.hint", &[]),
            );
            return;
        }

        for cat in &unique_categories(modules) {
            let count = modules.iter().filter(|m| m.category == *cat).count();
            let running = modules
                .iter()
                .filter(|m| m.category == *cat && m.status.is_running())
                .count();

            egui::CollapsingHeader::new(
                egui::RichText::new(format!("{}  {running}/{count}", category_label(lang, cat)))
                    .strong(),
            )
            .default_open(true)
            .show(ui, |ui| {
                for (idx, m) in modules.iter().enumerate() {
                    if m.category == *cat {
                        module_list_item(ui, lang, pal, m, idx, selected);
                    }
                }
            });
        }
    });
}

/// 自绘列表项：状态色圆点 + 名称 + 版本 + 端口；选中/hover 背景，点击选中
fn module_list_item(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    m: &ModuleEntry,
    idx: usize,
    selected: &mut Option<usize>,
) {
    let is_selected = *selected == Some(idx);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ITEM_HEIGHT),
        egui::Sense::click(),
    );

    if response.clicked() {
        *selected = Some(idx);
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter().with_clip_rect(rect);
        let rounding = egui::CornerRadius::same(CONTROL_ROUNDING);

        if is_selected {
            painter.rect_filled(rect, rounding, pal.card_raised);
        } else if response.hovered() {
            // hover 弱化背景：card → card_raised 插值
            painter.rect_filled(
                rect,
                rounding,
                pal.card.lerp_to_gamma(pal.card_raised, 0.55),
            );
        }

        // 状态色圆点
        let meta = service_status(&m.status, pal);
        painter.circle_filled(egui::pos2(rect.min.x + 12.0, rect.center().y), 4.0, meta.color);

        // 名称 / 版本 / 端口 依次排布
        let cy = rect.center().y;
        let mut x = rect.min.x + 24.0;
        let mut segment = |text: egui::RichText, gap: f32| {
            x += gap;
            let galley = egui::WidgetText::from(text).into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                egui::FontSelection::Default,
            );
            let size = galley.size();
            painter.galley(egui::pos2(x, cy - size.y / 2.0), galley, pal.text);
            x += size.x;
        };

        let name_color = if is_selected { pal.primary } else { pal.text };
        segment(egui::RichText::new(m.name.clone()).color(name_color), 0.0);
        segment(
            egui::RichText::new(format!("v{}", m.version))
                .small()
                .color(pal.text_dim),
            6.0,
        );
        if let Some(port) = m.port {
            segment(
                egui::RichText::new(format!(":{port}"))
                    .monospace()
                    .color(pal.text_faint),
                6.0,
            );
        }
    }

    let status = service_label(lang, &m.status);
    let category = category_label(lang, &m.category);
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(tr(
            lang,
            "desktopPages.modules.hover.detail",
            &[
                ("name", m.name.as_str()),
                ("status", &status),
                ("category", &category),
            ],
        ));
}

// ─── 右栏：详情 ──────────────────────────────────────────────────────────────

fn detail_area(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    modules: &mut [ModuleEntry],
    selected: &mut Option<usize>,
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    let Some(idx) = *selected else {
        empty_state(
            ui,
            pal,
            "🧩",
            &tr(lang, "desktopPages.modules.select.title", &[]),
            &tr(lang, "desktopPages.modules.select.hint", &[]),
        );
        return;
    };

    let Some(m) = modules.get_mut(idx) else {
        // 失效选择（模块列表已变化）：清空并回到空态
        *selected = None;
        empty_state(
            ui,
            pal,
            "🧩",
            &tr(lang, "desktopPages.modules.select.title", &[]),
            &tr(lang, "desktopPages.modules.select.hint", &[]),
        );
        return;
    };

    detail_panel(ui, lang, pal, m, cmd_tx);
}

fn detail_panel(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    m: &mut ModuleEntry,
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(2.0);

        // ── 概览卡片：头部 + 信息 + 操作 ──
        card(ui, pal, |ui| {
            let meta = service_status(&m.status, pal);

            ui.horizontal(|ui| {
                ui.heading(&m.name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    badge(ui, pal, meta.color, service_label(lang, &m.status));
                });
            });

            if let ServiceStatus::Error(err) = &m.status {
                if !err.is_empty() {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(err).small().color(pal.danger));
                }
            }

            ui.add_space(10.0);

            egui::Grid::new("module_detail_grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    info_row(ui, pal, "ID", egui::RichText::new(m.id.clone()).monospace());
                    info_row(
                        ui,
                        pal,
                        &tr(lang, "desktopPages.modules.info.version", &[]),
                        egui::RichText::new(m.version.clone()),
                    );
                    info_row(
                        ui,
                        pal,
                        &tr(lang, "desktopPages.modules.info.category", &[]),
                        egui::RichText::new(category_label(lang, &m.category)),
                    );
                    if let Some(dev) = &m.device {
                        info_row(
                            ui,
                            pal,
                            &tr(lang, "desktopPages.modules.info.device", &[]),
                            egui::RichText::new(dev.clone()),
                        );
                    }
                    if let Some(port) = m.port {
                        info_row(
                            ui,
                            pal,
                            &tr(lang, "desktopPages.modules.info.port", &[]),
                            egui::RichText::new(format!("{port}")).monospace(),
                        );
                    }
                    if let Some(started) = m.started_at {
                        info_row(
                            ui,
                            pal,
                            &tr(lang, "desktopPages.modules.info.uptime", &[]),
                            egui::RichText::new(format_uptime(lang, started.elapsed())),
                        );
                    }
                });

            if !m.description.is_empty() {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(m.description.clone()).color(pal.text_dim));
            }

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            action_bar(ui, lang, pal, m, cmd_tx);
        });

        ui.add_space(14.0);

        // ── 日志 ──
        log_section(ui, lang, pal, m);
        ui.add_space(6.0);
    });
}

/// 信息 Grid 单行：弱化标签 + 值
fn info_row(ui: &mut egui::Ui, pal: &Palette, label: &str, value: egui::RichText) {
    ui.label(egui::RichText::new(label).color(pal.text_dim));
    ui.label(value);
    ui.end_row();
}

// ─── 操作区（含确认对话框） ──────────────────────────────────────────────────

fn action_bar(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    m: &ModuleEntry,
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    // 每模块独立的对话框开启标记
    let key_stop = egui::Id::new(("confirm_stop", m.id.clone()));
    let key_restart = egui::Id::new(("confirm_restart", m.id.clone()));

    ui.horizontal(|ui| match &m.status {
        // 启动无破坏性，直接执行
        ServiceStatus::Stopped => {
            let label = format!("▶ {}", tr(lang, "common.action.start", &[]));
            if ui.add(primary_button(pal, label)).clicked() {
                let _ = cmd_tx.send(AppCmd::StartModule(m.id.clone()));
            }
        }
        ServiceStatus::Running | ServiceStatus::Starting => {
            let label = format!("⏹ {}", tr(lang, "common.action.stop", &[]));
            if ui.add(danger_button(pal, label)).clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(key_stop, true));
            }
        }
        ServiceStatus::Error(_) => {
            let label = format!("🔄 {}", tr(lang, "common.action.restart", &[]));
            let btn = egui::Button::new(egui::RichText::new(label).color(pal.bg))
                .fill(pal.warning)
                .corner_radius(egui::CornerRadius::same(CONTROL_ROUNDING))
                .stroke(egui::Stroke::NONE);
            if ui.add(btn).clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(key_restart, true));
            }
        }
        ServiceStatus::Preparing => {
            ui.spinner();
            ui.label(
                egui::RichText::new(format!("{}…", tr(lang, "common.status.preparing", &[])))
                    .color(pal.text_dim),
            );
        }
        ServiceStatus::NotReady => {
            ui.label(
                egui::RichText::new(format!(
                    "⚠ {}",
                    tr(lang, "desktopPages.modules.notReadyHint", &[])
                ))
                .small()
                .color(pal.text_dim),
            );
        }
    });

    // 停止确认
    if ui.ctx().data(|d| d.get_temp::<bool>(key_stop).unwrap_or(false)) {
        let title = tr(lang, "desktopPages.modules.dlg.stop.title", &[]);
        let message = tr(
            lang,
            "desktopPages.modules.dlg.stop.message",
            &[("name", m.name.as_str())],
        );
        let confirm = tr(lang, "common.action.stop", &[]);
        match confirm_dialog_with_lang(
            ui.ctx(),
            pal,
            &format!("dlg_stop_{}", m.id),
            &title,
            &message,
            &confirm,
            true,
            lang,
        ) {
            Some(true) => {
                ui.ctx().data_mut(|d| d.remove_temp::<bool>(key_stop));
                let _ = cmd_tx.send(AppCmd::StopModule(m.id.clone()));
            }
            Some(false) => {
                ui.ctx().data_mut(|d| d.remove_temp::<bool>(key_stop));
            }
            None => {}
        }
    }

    // 重启确认（确认后先发 Stop 再发 Start）
    if ui.ctx().data(|d| d.get_temp::<bool>(key_restart).unwrap_or(false)) {
        let title = tr(lang, "desktopPages.modules.dlg.restart.title", &[]);
        let message = tr(
            lang,
            "desktopPages.modules.dlg.restart.message",
            &[("name", m.name.as_str())],
        );
        let confirm = tr(lang, "common.action.restart", &[]);
        match confirm_dialog_with_lang(
            ui.ctx(),
            pal,
            &format!("dlg_restart_{}", m.id),
            &title,
            &message,
            &confirm,
            false,
            lang,
        ) {
            Some(true) => {
                ui.ctx().data_mut(|d| d.remove_temp::<bool>(key_restart));
                let _ = cmd_tx.send(AppCmd::StopModule(m.id.clone()));
                let _ = cmd_tx.send(AppCmd::StartModule(m.id.clone()));
            }
            Some(false) => {
                ui.ctx().data_mut(|d| d.remove_temp::<bool>(key_restart));
            }
            None => {}
        }
    }
}

// ─── 日志区 ──────────────────────────────────────────────────────────────────

fn log_section(ui: &mut egui::Ui, lang: &str, pal: &Palette, m: &mut ModuleEntry) {
    ui.horizontal(|ui| {
        let count = m.logs.len().to_string();
        ui.label(
            egui::RichText::new(tr(lang, "desktopPages.modules.logs", &[("count", &count)]))
                .strong(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(subtle_button(
                    pal,
                    tr(lang, "desktopPages.modules.clearLogs", &[]),
                ))
                .clicked()
            {
                m.logs.clear();
            }
        });
    });
    ui.add_space(4.0);

    let log_height = ui.available_height().clamp(120.0, 400.0);
    egui::Frame::new()
        .fill(pal.card)
        .stroke(egui::Stroke::new(1.0_f32, pal.border))
        .corner_radius(egui::CornerRadius::same(CARD_ROUNDING))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(log_height)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    if m.logs.is_empty() {
                        ui.label(
                            egui::RichText::new(tr(lang, "desktopPages.modules.noLogs", &[]))
                                .small()
                                .color(pal.text_faint),
                        );
                    } else {
                        for (i, line) in m.logs.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                ui.label(
                                    egui::RichText::new(format!("{:>4}", i + 1))
                                        .monospace()
                                        .small()
                                        .color(pal.text_faint),
                                );
                                ui.label(
                                    egui::RichText::new(line.as_str())
                                        .monospace()
                                        .small(),
                                );
                            });
                        }
                    }
                });
        });
}

// ─── 辅助 ────────────────────────────────────────────────────────────────────

/// 按出现顺序提取类别列表
fn unique_categories(modules: &[ModuleEntry]) -> Vec<ModuleCategory> {
    let mut cats = Vec::new();
    for m in modules {
        if !cats.contains(&m.category) {
            cats.push(m.category.clone());
        }
    }
    cats
}

/// 本地化的模块类别文案；`Other` 承载 manifest 原始字符串，按数据原样显示。
pub fn category_label(lang: &str, c: &ModuleCategory) -> String {
    match c {
        ModuleCategory::Asr => tr(lang, "desktopPages.modules.cat.asr", &[]),
        ModuleCategory::Tts => tr(lang, "desktopPages.modules.cat.tts", &[]),
        ModuleCategory::Denoise => tr(lang, "desktopPages.modules.cat.denoise", &[]),
        ModuleCategory::Ocr => tr(lang, "desktopPages.modules.cat.ocr", &[]),
        ModuleCategory::Image => tr(lang, "desktopPages.modules.cat.image", &[]),
        ModuleCategory::Translate => tr(lang, "desktopPages.modules.cat.translate", &[]),
        ModuleCategory::Video => tr(lang, "desktopPages.modules.cat.video", &[]),
        ModuleCategory::Face => tr(lang, "desktopPages.modules.cat.face", &[]),
        ModuleCategory::Custom => tr(lang, "desktopPages.modules.cat.custom", &[]),
        ModuleCategory::Other(s) => s.clone(),
    }
}

/// 本地化的服务状态文案；颜色仍取自 [`crate::ui::service_status`]。
pub fn service_label(lang: &str, status: &ServiceStatus) -> String {
    let key = match status {
        ServiceStatus::Running => "common.status.running",
        ServiceStatus::Stopped => "common.status.stopped",
        ServiceStatus::Starting => "common.status.starting",
        ServiceStatus::Preparing => "common.status.preparing",
        ServiceStatus::Error(_) => "common.status.error",
        ServiceStatus::NotReady => "common.status.notReady",
    };
    tr(lang, key, &[])
}

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
