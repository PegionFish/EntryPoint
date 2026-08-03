//! 模块管理页 — 响应式 master-detail：左栏按类别分组列表，右栏详情 + 操作 + 日志。
//!
//! 所有颜色取自 [`crate::ui::Palette`]，状态显示统一走 [`crate::ui::service_status`]。

use eframe::egui;
use ep_core::types::{ModuleCategory, ServiceStatus};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{AppCmd, ModuleEntry};
use crate::ui::{
    badge, card, confirm_dialog, danger_button, empty_state, page_header, primary_button,
    service_status, status_badge, subtle_button, Palette, CARD_ROUNDING, CONTROL_ROUNDING,
};

/// 左栏列表项行高
const ITEM_HEIGHT: f32 = 30.0;

pub fn show(
    ui: &mut egui::Ui,
    modules: &mut [ModuleEntry],
    selected: &mut Option<usize>,
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    let pal = Palette::new(ui.style().visuals.dark_mode);

    // ── 页头 + 汇总 ──
    page_header(ui, "模块管理", |ui| {
        ui.label(
            egui::RichText::new(format!("共 {} 个模块", modules.len())).color(pal.text_dim),
        );
    });
    ui.add_space(2.0);
    summary_bar(ui, &pal, modules);
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
            module_list(ui, &pal, modules, selected);
        });

    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(pal.bg)
                .inner_margin(egui::Margin::symmetric(8, 4)),
        )
        .show_inside(ui, |ui| {
            detail_area(ui, &pal, modules, selected, cmd_tx);
        });
}

// ─── 汇总条 ──────────────────────────────────────────────────────────────────

/// 状态计数徽章（0 不显示）
fn summary_bar(ui: &mut egui::Ui, pal: &Palette, modules: &[ModuleEntry]) {
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
            badge(ui, pal, pal.success, format!("{running} 运行中"));
        }
        if stopped > 0 {
            badge(ui, pal, pal.neutral, format!("{stopped} 已停止"));
        }
        if errors > 0 {
            badge(ui, pal, pal.danger, format!("{errors} 错误"));
        }
        if not_ready > 0 {
            badge(ui, pal, pal.notready, format!("{not_ready} 未就绪"));
        }
        if modules.is_empty() {
            ui.label(egui::RichText::new("暂无模块").color(pal.text_faint));
        }
    });
}

// ─── 左栏：模块列表 ──────────────────────────────────────────────────────────

fn module_list(
    ui: &mut egui::Ui,
    pal: &Palette,
    modules: &[ModuleEntry],
    selected: &mut Option<usize>,
) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        if modules.is_empty() {
            empty_state(ui, pal, "🧩", "未发现模块", "请检查 modules/ 目录");
            return;
        }

        for cat in &unique_categories(modules) {
            let count = modules.iter().filter(|m| m.category == *cat).count();
            let running = modules
                .iter()
                .filter(|m| m.category == *cat && m.status.is_running())
                .count();

            egui::CollapsingHeader::new(
                egui::RichText::new(format!("{}  {running}/{count}", category_label(cat)))
                    .strong(),
            )
            .default_open(true)
            .show(ui, |ui| {
                for (idx, m) in modules.iter().enumerate() {
                    if m.category == *cat {
                        module_list_item(ui, pal, m, idx, selected);
                    }
                }
            });
        }
    });
}

/// 自绘列表项：状态色圆点 + 名称 + 版本 + 端口；选中/hover 背景，点击选中
fn module_list_item(
    ui: &mut egui::Ui,
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

    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(format!(
            "{}\n状态: {}\n类别: {}",
            m.name,
            service_status(&m.status, pal).label,
            category_label(&m.category),
        ));
}

// ─── 右栏：详情 ──────────────────────────────────────────────────────────────

fn detail_area(
    ui: &mut egui::Ui,
    pal: &Palette,
    modules: &mut [ModuleEntry],
    selected: &mut Option<usize>,
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    let Some(idx) = *selected else {
        empty_state(ui, pal, "🧩", "选择一个模块", "在左侧列表中点击模块查看详情");
        return;
    };

    let Some(m) = modules.get_mut(idx) else {
        // 失效选择（模块列表已变化）：清空并回到空态
        *selected = None;
        empty_state(ui, pal, "🧩", "选择一个模块", "在左侧列表中点击模块查看详情");
        return;
    };

    detail_panel(ui, pal, m, cmd_tx);
}

fn detail_panel(
    ui: &mut egui::Ui,
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
                    status_badge(ui, pal, meta);
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
                    info_row(ui, pal, "版本", egui::RichText::new(m.version.clone()));
                    info_row(
                        ui,
                        pal,
                        "类别",
                        egui::RichText::new(category_label(&m.category)),
                    );
                    if let Some(dev) = &m.device {
                        info_row(ui, pal, "设备", egui::RichText::new(dev.clone()));
                    }
                    if let Some(port) = m.port {
                        info_row(
                            ui,
                            pal,
                            "端口",
                            egui::RichText::new(format!("{port}")).monospace(),
                        );
                    }
                    if let Some(started) = m.started_at {
                        info_row(
                            ui,
                            pal,
                            "运行时间",
                            egui::RichText::new(format_uptime(started.elapsed())),
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

            action_bar(ui, pal, m, cmd_tx);
        });

        ui.add_space(14.0);

        // ── 日志 ──
        log_section(ui, pal, m);
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
            if ui.add(primary_button(pal, "▶ 启动")).clicked() {
                let _ = cmd_tx.send(AppCmd::StartModule(m.id.clone()));
            }
        }
        ServiceStatus::Running | ServiceStatus::Starting => {
            if ui.add(danger_button(pal, "⏹ 停止")).clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(key_stop, true));
            }
        }
        ServiceStatus::Error(_) => {
            let btn = egui::Button::new(egui::RichText::new("🔄 重启").color(pal.bg))
                .fill(pal.warning)
                .corner_radius(egui::CornerRadius::same(CONTROL_ROUNDING))
                .stroke(egui::Stroke::NONE);
            if ui.add(btn).clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(key_restart, true));
            }
        }
        ServiceStatus::Preparing => {
            ui.spinner();
            ui.label(egui::RichText::new("准备中…").color(pal.text_dim));
        }
        ServiceStatus::NotReady => {
            ui.label(
                egui::RichText::new("⚠ 模块未就绪（缺少依赖或 manifest 无效）")
                    .small()
                    .color(pal.text_dim),
            );
        }
    });

    // 停止确认
    if ui.ctx().data(|d| d.get_temp::<bool>(key_stop).unwrap_or(false)) {
        match confirm_dialog(
            ui.ctx(),
            pal,
            &format!("dlg_stop_{}", m.id),
            "停止模块",
            &format!("确定停止「{}」吗？正在进行的请求将被中断。", m.name),
            "停止",
            true,
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
        match confirm_dialog(
            ui.ctx(),
            pal,
            &format!("dlg_restart_{}", m.id),
            "重启模块",
            &format!("确定重启「{}」吗？将先停止再启动该模块。", m.name),
            "重启",
            false,
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

fn log_section(ui: &mut egui::Ui, pal: &Palette, m: &mut ModuleEntry) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("日志 ({})", m.logs.len())).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(subtle_button(pal, "清空")).clicked() {
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
                            egui::RichText::new("暂无日志")
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

fn category_label(c: &ModuleCategory) -> String {
    match c {
        ModuleCategory::Asr => "语音识别 (ASR)".into(),
        ModuleCategory::Tts => "语音合成 (TTS)".into(),
        ModuleCategory::Denoise => "降噪".into(),
        ModuleCategory::Ocr => "文字识别 (OCR)".into(),
        ModuleCategory::Image => "图像处理".into(),
        ModuleCategory::Translate => "翻译".into(),
        ModuleCategory::Video => "视频处理".into(),
        ModuleCategory::Face => "人脸识别".into(),
        ModuleCategory::Custom => "自定义".into(),
        ModuleCategory::Other(s) => s.clone(),
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
