//! 仪表盘 — 统计概览、依赖检测、计算设备、模块状态一览。

use eframe::egui;
use ep_core::config::AppConfig;
use ep_core::types::{ComputeDevice, ServiceStatus};

use crate::app::ModuleEntry;
use crate::i18n::tr;
use crate::pages::modules::{category_label, service_label};
use crate::ui::{
    accent_underline, badge, card, card_frame_active, card_grid, card_stroke, color_with_alpha,
    empty_state, glow_breath_alpha, grid_columns, keyboard_scroll, page_header,
    progress_gradient, section_title, status_badge, Palette,
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

    // 发布权威快照：设备列表（管线编辑器 VRAM 账本消费）。
    // 仪表盘为默认首页，启动后快照即存在。模块运行状态由模块页直接消费
    // app.rs 的 `&[ModuleEntry]`（协调记录 #47），不再经模块快照桥。
    crate::pages::publish_device_snapshot(ui.ctx(), devices);

    // 主滚动区启用键盘滚动（P2-1）
    keyboard_scroll(ui, "dashboard_main", egui::ScrollArea::vertical(), |ui| {
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.add_space(PAGE_MARGIN);
            ui.vertical(|ui| {
                page_header(ui, &tr(lang, "desktopPages.dashboard.title", &[]), |_| {});
                ui.add_space(12.0);

                stats_section(ui, lang, &pal, devices, modules);
                ui.add_space(SECTION_GAP);

                // 区块顺序对齐 WebUI 仪表盘基准（webui-verify-01-dashboard.png）：
                // 计算设备 → 模块状态 → 系统依赖
                device_section(ui, lang, &pal, devices);
                ui.add_space(SECTION_GAP);

                module_section(ui, lang, &pal, modules);
                ui.add_space(SECTION_GAP);

                dep_section(ui, lang, &pal, dep_report);
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
            color: if running > 0 { pal.status_running } else { pal.text },
        },
        StatCard {
            label: tr(lang, "desktopPages.dashboard.stat.errors", &[]),
            value: errors.to_string(),
            color: if errors > 0 { pal.status_error } else { pal.text },
        },
    ];

    // 列数封顶于卡片数：铺满可用宽度，不产生右侧空槽（P1-1 统一宽度策略）
    let cols = grid_columns(ui.available_width(), 170.0, 12.0, stats.len());
    card_grid(ui, cols, &stats, |ui, s| {
        card(ui, pal, |ui| {
            // 统计条带仪表盘化（§1.1 主张 4）：大号等宽数字 + 2px 渐变下划线
            // + 全大写灰阶小标签
            ui.vertical_centered(|ui| {
                ui.add_space(12.0);
                let resp = ui.label(
                    egui::RichText::new(s.value.as_str())
                        .font(egui::FontId::monospace(stat_number_size(ui)))
                        .strong()
                        .color(s.color),
                );
                ui.add_space(5.0);
                accent_underline(ui, pal, resp.rect.width().max(32.0));
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(s.label.to_uppercase())
                        .text_style(egui::TextStyle::Small)
                        .color(pal.text_faint),
                );
                ui.add_space(12.0);
            });
        });
    });
}

/// 统计大数字字号（text-4xl = 36px，随配置字号等比缩放；§3.2）
fn stat_number_size(ui: &egui::Ui) -> f32 {
    let body = ui.style().text_styles[&egui::TextStyle::Body].size;
    36.0 * (body / crate::theme::BASE_FONT_SIZE)
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
            badge(ui, pal, pal.status_error, tr(lang, "common.status.missing", &[]));
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

    // 等宽列铺满：列数封顶于设备数，避免单卡独占行宽/后续卡片被右缘裁切（P1-1）
    let cols = grid_columns(ui.available_width(), 260.0, 12.0, devices.len());
    let now_ms = ui.ctx().input(|i| i.time * 1000.0);
    let breath = glow_breath_alpha(now_ms);
    let mut any_active = false;
    card_grid(ui, cols, devices, |ui, dev| {
        // 运行态判定：有利用率或显存占用（§7.1 运行态呼吸辉光载体）
        let active = dev.utilization.is_some_and(|u| u > 0)
            || dev.used_memory_mb.is_some_and(|m| m > 0);
        any_active |= active;
        device_card(ui, lang, pal, dev, active, breath);
    });
    // 呼吸辉光时间驱动：存在活跃设备时按 ~20fps 追加重绘
    //（§1.1 主张 6；空闲仍回到 REPAINT_WATCHDOG 心跳）
    if any_active {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(48));
    }
}

/// 单张设备卡：运行态青色呼吸辉光描边 + 辉光阴影；静止态 hover 只提描边亮度
fn device_card(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    dev: &ComputeDevice,
    active: bool,
    breath: f32,
) {
    let id = ui.next_auto_id();
    let prev_hovered = ui.ctx().data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    let (stroke, shadow) = if active {
        // 呼吸辉光 2.4s：描边 alpha 0.35–0.7 × 辉光基档（§1.1 主张 3）
        let stroke_alpha = (breath * 115.0) as u8;
        let shadow_alpha = (breath * 64.0) as u8;
        (
            egui::Stroke::new(1.0_f32, color_with_alpha(pal.status_running, stroke_alpha)),
            Some(color_with_alpha(pal.status_running, shadow_alpha)),
        )
    } else {
        (
            card_stroke(pal, prev_hovered),
            if prev_hovered {
                Some(pal.primary_glow)
            } else {
                None
            },
        )
    };

    let inner = card_frame_active(pal, stroke, shadow).show(ui, |ui| {
        // 名称 + 后端徽章
        ui.horizontal(|ui| {
            ui.strong(&dev.name);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                badge(ui, pal, pal.info, dev.backend.to_string());
            });
        });
        ui.add_space(8.0);

        // 显存（CPU 等无显存数据的设备跳过）；高占用保留 warning/danger 单色告警
        if let (Some(total), Some(used)) = (dev.total_memory_mb, dev.used_memory_mb) {
            let frac = (used as f32 / total.max(1) as f32).min(1.0);
            let alert = if frac > 0.95 {
                Some(pal.danger)
            } else if frac > 0.80 {
                Some(pal.warning)
            } else {
                None
            };
            progress_gradient(ui, pal, frac, alert);
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!("{used} / {total} MB"))
                    .monospace()
                    .small()
                    .color(pal.text_dim),
            );
        }

        // 利用率 / 温度（数值 mono 对齐，§3.2）
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
            ui.label(
                egui::RichText::new(meta.join(" · "))
                    .monospace()
                    .color(pal.text_dim),
            );
        }
    });

    // hover 状态跨帧传递：只提描边亮度，零位移（§1.1 主张 3）
    let hovered = inner.response.hovered();
    if hovered != prev_hovered {
        ui.ctx().data_mut(|d| d.insert_temp(id, hovered));
        ui.ctx().request_repaint();
    }
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
    let rows: Vec<[String; 5]> = modules
        .iter()
        .map(|m| {
            [
                m.name.clone(),
                category_label(lang, &m.category),
                service_label(lang, &m.status),
                m.device.clone().unwrap_or_else(|| "-".into()),
                m.port
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "-".into()),
            ]
        })
        .collect();

    // 内容加权列宽：按自然宽度铺满可用宽度（P1-1 统一宽度策略）；
    // 内容超宽时退化为横向滚动，绝不裁切
    let col_x = 18.0_f32;
    let avail = ui.available_width();
    let widths = module_table_widths(ui, &headers, &rows, col_x, avail);
    let total_w = widths.iter().sum::<f32>() + col_x * (widths.len() - 1) as f32;

    let render = |ui: &mut egui::Ui| {
        // 表头：12px muted 不加底色（§3.4 表格规范）
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = col_x;
            for (c, w) in widths.iter().enumerate() {
                ui.scope(|ui| {
                    ui.set_min_width(*w);
                    ui.set_max_width(*w);
                    ui.label(
                        egui::RichText::new(headers[c].as_str())
                            .size(12.0)
                            .color(pal.text_dim),
                    );
                });
            }
        });
        ui.add_space(4.0);
        // 数据行（隔行条纹：层 2 半透明底，新令牌）
        for (i, row) in rows.iter().enumerate() {
            egui::Frame::new()
                .fill(if i % 2 == 1 {
                    color_with_alpha(pal.bg_raised, 96)
                } else {
                    egui::Color32::TRANSPARENT
                })
                .inner_margin(egui::Margin::symmetric(6, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = col_x;
                        for (c, w) in widths.iter().enumerate() {
                            ui.scope(|ui| {
                                ui.set_min_width(*w);
                                ui.set_max_width(*w);
                                match c {
                                    2 => {
                                        // 四态色状态徽章（§1.2 权威色；运行态附辉光晕）
                                        status_badge(ui, pal, &modules[i].status, row[c].as_str());
                                    }
                                    3 | 4 => {
                                        ui.label(
                                            egui::RichText::new(row[c].as_str()).monospace(),
                                        );
                                    }
                                    _ => {
                                        ui.label(row[c].as_str());
                                    }
                                }
                            });
                        }
                    });
                });
        }
    };

    if total_w <= avail + 0.5 {
        render(ui);
    } else {
        egui::ScrollArea::horizontal().show(ui, |ui| render(ui));
    }
}

/// 模块概览表列宽：表头与全部行中最宽内容作为每列自然宽度；
/// 总和小于可用宽度时按自然宽度比例分配剩余空间以铺满整行；
/// 总和超出时返回自然宽度（调用方退化为横向滚动，不裁切）。
fn module_table_widths(
    ui: &egui::Ui,
    headers: &[String; 5],
    rows: &[[String; 5]],
    col_x: f32,
    avail: f32,
) -> Vec<f32> {
    let body = ui.style().text_styles[&egui::TextStyle::Body].clone();
    let mono = ui.style().text_styles[&egui::TextStyle::Monospace].clone();
    // 每列下限（px）：名称 / 类别 / 状态(徽章) / 设备 / 端口
    let mut natural: Vec<f32> = vec![120.0, 96.0, 108.0, 84.0, 64.0];
    /// 徽章胶囊附加宽度：色点 7 + 间距 5 + 内边距 16 + 描边余量
    const BADGE_PAD: f32 = 30.0;
    ui.fonts(|fonts| {
        for (c, h) in headers.iter().enumerate() {
            let w = fonts
                .layout_no_wrap(h.clone(), body.clone(), egui::Color32::WHITE)
                .rect
                .width();
            natural[c] = natural[c].max(w);
        }
        for row in rows {
            for (c, text) in row.iter().enumerate() {
                let font = if c >= 3 { mono.clone() } else { body.clone() };
                let mut w = fonts
                    .layout_no_wrap(text.clone(), font, egui::Color32::WHITE)
                    .rect
                    .width();
                if c == 2 {
                    w += BADGE_PAD;
                }
                natural[c] = natural[c].max(w);
            }
        }
    });
    let spacing_total = col_x * (natural.len() - 1) as f32;
    let natural_sum: f32 = natural.iter().sum();
    if natural_sum + spacing_total >= avail {
        return natural;
    }
    let surplus = avail - spacing_total - natural_sum;
    natural
        .iter()
        .map(|w| w + surplus * w / natural_sum)
        .collect()
}
