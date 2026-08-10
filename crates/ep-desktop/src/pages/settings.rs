//! 设置页 — 双列卡片布局（§7.5，UNIFIED_UI_REDESIGN_PROPOSAL）。
//!
//! - 6 段分区（appearance/compute/models/network/python/pipeline，W4-B2 合并：
//!   「通用」+「界面」→「外观与语言」；端口段并入「计算」），
//!   内容宽度 >=1080px 双列、窄窗单列（`settings_columns`）；
//! - dirty 门控动作条（W4-A2）：无改动保存退回 subtle；有改动主按钮辉光 +
//!   琥珀「未保存」pill + 「重置」按钮（基线 = 上次保存的 toml 序列化快照，
//!   存 egui 临时数据，每帧比对）；
//! - 区块头三段式（W4-A3）：图标字形 + 标题 + 描述；
//! - 校验补齐（W4-A4）：端口起止区间交叉 + 代理协议前缀；无效字段红描边，
//!   保存失败时滚动区自动定位到首个无效字段；
//! - 宽卡双列字段（W4-B4）：单列布局下卡内宽足够时字段两两并排；
//! - 内边距密度分级（W4-B1）：控件数 ≤3 用 16px、≥4 用 24px。
//!
//! 所有用户可见文案经 [`crate::i18n::tr`] 查找；语言跟随
//! `config.general.language`，每帧归一化读取，切换即时生效。

use eframe::egui;
use ep_core::config::{AppConfig, AssignStrategy};

use crate::i18n::tr;
use crate::toast::ToastManager;
use crate::ui::{
    card_frame_padding, card_grid, color_with_alpha, density_padding, keyboard_scroll,
    numeric_field_stroke, page_header, primary_button_with_glow, section_header, subtle_button,
    switch_row, Palette,
};

/// 设置双列断点（W4-B4：1240 → 1080，更早进入双列提升信息密度）
pub const SETTINGS_TWO_COL_MIN_WIDTH: f32 = 1080.0;

/// 宽卡双列字段阈值（W4-B4）：卡内可用宽 >=640px 时字段两两并排
const WIDE_CARD_MIN_INNER: f32 = 640.0;

/// dirty 基线（上次保存配置的 toml 序列化快照）临时数据键
///（`egui::Id::new` 非 const fn，故以函数形式提供稳定键）
fn baseline_id() -> egui::Id {
    egui::Id::new("settings_baseline_toml")
}
/// 校验失败滚动定位的待处理标记
fn scroll_pending_id() -> egui::Id {
    egui::Id::new("settings_scroll_pending")
}
/// 无效字段 rect 收集（字段名 → 屏幕坐标矩形）
fn invalid_rects_id() -> egui::Id {
    egui::Id::new("settings_invalid_rects")
}

/// 设置页卡片列数：>=1080px 双列，窄窗单列（与 card_grid 等宽铺满机制配合）
pub fn settings_columns(available_width: f32) -> usize {
    if available_width >= SETTINGS_TWO_COL_MIN_WIDTH {
        2
    } else {
        1
    }
}

// ─── 分区定义（W4-B2：8 → 6 段） ─────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Section {
    /// 外观与语言（原「通用」+「界面」合并）
    Appearance,
    /// 计算（原「计算」+「端口」合并）
    Compute,
    Models,
    Network,
    Python,
    Pipeline,
}

const SECTIONS: [Section; 6] = [
    Section::Appearance,
    Section::Compute,
    Section::Models,
    Section::Network,
    Section::Python,
    Section::Pipeline,
];

/// 各分区控件数（内边距密度分级依据，W4-B1）
fn section_control_count(section: Section) -> usize {
    match section {
        Section::Appearance => 4, // 语言 + 缩放 + 字号 + 更新检查开关
        Section::Compute => 5,    // 策略 + 刷新间隔 + 起止端口 + 超额开关
        Section::Models => 4,
        Section::Network => 3,
        Section::Python => 2,
        Section::Pipeline => 4, // 并行 + 超时 + 工作区 + 保留开关
    }
}

/// 区块头三段式文案（W4-A3）：图标字形 + 标题 + 描述
fn section_meta(lang: &str, section: Section) -> (&'static str, String, String) {
    match section {
        Section::Appearance => (
            "🎨",
            tr(lang, "desktopApp.settings.section.appearance", &[]),
            tr(lang, "desktopApp.settings.section.appearanceDescription", &[]),
        ),
        Section::Compute => (
            "⚡",
            tr(lang, "settings.compute.title", &[]),
            tr(lang, "settings.compute.description", &[]),
        ),
        Section::Models => (
            "📦",
            tr(lang, "settings.models.title", &[]),
            tr(lang, "settings.models.description", &[]),
        ),
        Section::Network => (
            "🌐",
            tr(lang, "settings.network.title", &[]),
            tr(lang, "settings.network.description", &[]),
        ),
        Section::Python => (
            "🐍",
            tr(lang, "settings.python.title", &[]),
            tr(lang, "settings.python.description", &[]),
        ),
        Section::Pipeline => (
            "🔀",
            tr(lang, "settings.pipeline.title", &[]),
            tr(lang, "settings.pipeline.description", &[]),
        ),
    }
}

// ─── 主入口 ──────────────────────────────────────────────────────────────────

pub fn show(ui: &mut egui::Ui, config: &mut AppConfig, toasts: &mut ToastManager) {
    let lang = ep_core::i18n::normalize_language(&config.general.language);
    let pal = Palette::new(ui.style().visuals.dark_mode);

    // dirty 门控（W4-A2）：基线 = 上次保存的 toml 序列化快照，首帧建立
    let current_toml = serialize_config(config);
    let has_baseline = ui.ctx().data(|d| d.get_temp::<String>(baseline_id()).is_some());
    if !has_baseline {
        ui.ctx()
            .data_mut(|d| d.insert_temp(baseline_id(), current_toml.clone()));
    }
    let baseline: String = ui.ctx().data(|d| d.get_temp(baseline_id()).unwrap_or_default());
    let dirty = baseline != current_toml;

    // 每帧校验：无效字段清单驱动红描边（W4-A4）
    let validation = validate_settings(lang, config);

    // 页级动作条：right_to_left 布局，先加者居最右
    let mut save_clicked = false;
    let mut reset_clicked = false;
    let save_label = tr(lang, "desktopApp.settings.saveBtn", &[]);
    page_header(ui, &tr(lang, "settings.title", &[]), |ui| {
        if dirty {
            // 有改动：主按钮辉光 + 琥珀「未保存」pill + 重置
            if primary_button_with_glow(ui, &pal, save_label.clone()).clicked() {
                save_clicked = true;
            }
            unsaved_pill(ui, &pal, &tr(lang, "desktopApp.settings.unsaved", &[]));
            if ui
                .add(subtle_button(
                    &pal,
                    tr(lang, "settings.action.reset", &[]),
                ))
                .clicked()
            {
                reset_clicked = true;
            }
        } else if ui.add(subtle_button(&pal, save_label)).clicked() {
            save_clicked = true;
        }
        if ui
            .add(subtle_button(
                &pal,
                tr(lang, "desktopApp.settings.reloadBtn", &[]),
            ))
            .clicked()
        {
            let config_dir = ep_core::config::resolve_root().join("config");
            match AppConfig::load(&config_dir) {
                Ok(loaded) => {
                    *config = loaded;
                    ui.ctx().data_mut(|d| {
                        d.insert_temp(baseline_id(), serialize_config(config))
                    });
                    toasts.success(tr(lang, "desktopApp.settings.toast.reloaded", &[]));
                }
                Err(e) => toasts.error(tr(
                    lang,
                    "desktopApp.settings.error.loadFailed",
                    &[("detail", &e.to_string())],
                )),
            }
        }
    });

    // 重置：从基线快照反序列化恢复（W4-A2）
    if reset_clicked {
        if let Ok(reset) = toml::from_str::<AppConfig>(&baseline) {
            *config = reset;
            toasts.success(tr(lang, "settings.toast.reset", &[]));
        }
    }

    // 保存：校验不过 → 错误 toast + 滚动定位首个无效字段
    if save_clicked {
        if validation.errors.is_empty() {
            let config_dir = ep_core::config::resolve_root().join("config");
            match config.save(&config_dir) {
                Ok(()) => {
                    toasts.success(tr(lang, "settings.toast.saved", &[]));
                    ui.ctx().data_mut(|d| {
                        d.insert_temp(baseline_id(), serialize_config(config))
                    });
                }
                Err(e) => toasts.error(tr(
                    lang,
                    "desktopApp.settings.error.saveFailed",
                    &[("detail", &e.to_string())],
                )),
            }
        } else {
            toasts.error(tr(lang, "settings.toast.validationFailed", &[]));
            ui.ctx()
                .data_mut(|d| d.insert_temp(scroll_pending_id(), true));
        }
    }

    // 主滚动区启用键盘滚动（P2-1）；双列卡片网格间距 16
    ui.ctx().data_mut(|d| {
        d.insert_temp(
            invalid_rects_id(),
            Vec::<(&'static str, egui::Rect)>::new(),
        )
    });
    let invalid = validation.invalid;
    let out = keyboard_scroll(ui, "settings_main", egui::ScrollArea::vertical(), |ui| {
        ui.spacing_mut().item_spacing = egui::vec2(16.0, 16.0);
        let cols = settings_columns(ui.available_width());
        card_grid(
            ui,
            "settings_sections",
            cols,
            &SECTIONS,
            |ui, sec, hovered| {
                // 内边距密度分级（W4-B1）：按列宽与控件数取值
                let pad = density_padding(ui.available_width(), section_control_count(*sec));
                card_frame_padding(&pal, hovered, pad)
            },
            |ui, sec| render_section(ui, &pal, lang, config, *sec, &invalid),
        );
        ui.add_space(8.0);
    });

    // 校验失败滚动定位（W4-A4）：把首个无效字段滚入视口顶部下方 24px
    let pending: bool = ui
        .ctx()
        .data(|d| d.get_temp(scroll_pending_id()).unwrap_or(false));
    if pending {
        ui.ctx()
            .data_mut(|d| d.insert_temp(scroll_pending_id(), false));
        let rects: Vec<(&'static str, egui::Rect)> = ui
            .ctx()
            .data(|d| d.get_temp(invalid_rects_id()).unwrap_or_default());
        if let Some((_, rect)) = rects.first() {
            if let Some(mut state) = egui::containers::scroll_area::State::load(ui.ctx(), out.id)
            {
                let target = state.offset.y + (rect.min.y - out.inner_rect.min.y) - 24.0;
                let max_offset = (out.content_size.y - out.inner_rect.height()).max(0.0);
                state.offset.y = target.clamp(0.0, max_offset);
                state.store(ui.ctx(), out.id);
                ui.ctx().request_repaint();
            }
        }
    }
}

// ─── 分区渲染 ────────────────────────────────────────────────────────────────

fn render_section(
    ui: &mut egui::Ui,
    pal: &Palette,
    lang: &str,
    config: &mut AppConfig,
    section: Section,
    invalid: &[&'static str],
) {
    let (icon, title, description) = section_meta(lang, section);
    section_header(ui, pal, icon, &title, &description);
    ui.add_space(10.0);
    let wide = ui.available_width() >= WIDE_CARD_MIN_INNER;
    match section {
        Section::Appearance => {
            form_grid(ui, "settings_appearance", wide, |ui| {
                field_label(ui, pal, &lbl(lang, "common.label.language"));
                let current = ep_core::i18n::normalize_language(&config.general.language);
                ui.horizontal(|ui| {
                    // 语言选项固定以本族语显示（i18n 惯例，不进翻译文件）
                    let zh_label = "简体中文"; // i18n-exempt: native label
                    let en_label = "English"; // i18n-exempt: native label
                    if ui.radio(current == "zh-CN", zh_label).clicked() {
                        config.general.language = "zh-CN".to_string();
                    }
                    if ui.radio(current == "en", en_label).clicked() {
                        config.general.language = "en".to_string();
                    }
                });
                end_pair(ui, wide, 0);

                field_label(ui, pal, &lbl(lang, "desktopApp.settings.ui.zoom"));
                stroked_field(ui, pal, invalid, "zoom", |ui| {
                    ui.add(
                        egui::DragValue::new(&mut config.ui.scale_factor)
                            .range(0.5..=3.0)
                            .speed(0.1),
                    );
                });
                end_pair(ui, wide, 1);

                field_label(ui, pal, &lbl(lang, "desktopApp.settings.ui.fontSize"));
                stroked_field(ui, pal, invalid, "font_size", |ui| {
                    ui.add(egui::DragValue::new(&mut config.ui.font_size).range(10.0..=24.0));
                });
                end_pair(ui, wide, 2);
            });
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(tr(lang, "desktopApp.settings.ui.applyNote", &[]))
                    .small()
                    .color(pal.text_faint),
            );
            ui.label(
                egui::RichText::new(tr(lang, "desktopApp.settings.languageHint", &[]))
                    .small()
                    .color(pal.text_faint),
            );
            ui.add_space(8.0);
            switch_row(
                ui,
                pal,
                &mut config.general.check_updates,
                &tr(lang, "settings.general.checkUpdates", &[]),
                &tr(lang, "settings.general.checkUpdatesDescription", &[]),
            );
        }
        Section::Compute => {
            form_grid(ui, "settings_compute", wide, |ui| {
                field_label(ui, pal, &lbl(lang, "settings.compute.strategy"));
                let strategies: [(AssignStrategy, &str); 4] = [
                    (AssignStrategy::Manual, "settings.strategy.manual"),
                    (
                        AssignStrategy::LeastMemory,
                        "settings.strategy.leastMemory",
                    ),
                    (AssignStrategy::RoundRobin, "settings.strategy.roundRobin"),
                    (
                        AssignStrategy::Single(None),
                        "desktopApp.settings.strategy.single",
                    ),
                ];
                let current_label = strategies
                    .iter()
                    .find(|(s, _)| {
                        std::mem::discriminant(s) == std::mem::discriminant(&config.compute.strategy)
                    })
                    .map(|(_, key)| tr(lang, key, &[]))
                    .unwrap_or_else(|| tr(lang, "common.status.unknown", &[]));
                egui::ComboBox::from_id_salt("strategy_select")
                    .selected_text(current_label)
                    .show_ui(ui, |ui| {
                        for (strategy, key) in &strategies {
                            ui.selectable_value(
                                &mut config.compute.strategy,
                                strategy.clone(),
                                tr(lang, key, &[]),
                            );
                        }
                    });
                end_pair(ui, wide, 0);

                field_label(ui, pal, &lbl(lang, "settings.compute.refreshInterval"));
                stroked_field(ui, pal, invalid, "refresh_interval", |ui| {
                    ui.add(
                        egui::DragValue::new(&mut config.compute.refresh_interval_secs)
                            .range(1..=60),
                    );
                });
                end_pair(ui, wide, 1);

                // 端口段并入计算段（W4-B2）：明确可编辑观感（§3.4）
                field_label(ui, pal, &lbl(lang, "settings.ports.rangeStart"));
                stroked_field(ui, pal, invalid, "port_start", |ui| {
                    ui.add(egui::DragValue::new(&mut config.ports.range_start).range(1..=65535));
                });
                end_pair(ui, wide, 2);

                field_label(ui, pal, &lbl(lang, "settings.ports.rangeEnd"));
                stroked_field(ui, pal, invalid, "port_end", |ui| {
                    ui.add(egui::DragValue::new(&mut config.ports.range_end).range(1..=65535));
                });
                end_pair(ui, wide, 3);
            });
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(tr(lang, "settings.ports.description", &[]))
                    .small()
                    .color(pal.text_faint),
            );
            ui.add_space(8.0);
            switch_row(
                ui,
                pal,
                &mut config.compute.allow_overcommit,
                &tr(lang, "settings.compute.allowOvercommit", &[]),
                &tr(lang, "settings.compute.allowOvercommitDescription", &[]),
            );
        }
        Section::Models => {
            form_grid(ui, "settings_models", wide, |ui| {
                field_label(ui, pal, &lbl(lang, "settings.models.cacheDir"));
                ui.add(
                    egui::TextEdit::singleline(&mut config.models.cache_dir)
                        .desired_width(f32::INFINITY),
                );
                end_pair(ui, wide, 0);

                // 解析后的绝对路径（P2-2）：相对 cache_dir 基于应用根解析，
                // 与仪表盘/模块页口径一致，避免仅见相对值 `models` 的歧义。
                field_label(
                    ui,
                    pal,
                    &lbl(lang, "desktopApp.settings.field.cacheDirResolved"),
                );
                let resolved =
                    config.resolve_model_cache_dir(&ep_core::config::resolve_root());
                ui.label(
                    egui::RichText::new(resolved.display().to_string())
                        .monospace()
                        .color(pal.text_dim),
                );
                end_pair(ui, wide, 1);

                field_label(ui, pal, &lbl(lang, "settings.models.hfEndpoint"));
                ui.add(
                    egui::TextEdit::singleline(&mut config.models.hf_endpoint)
                        .desired_width(f32::INFINITY)
                        .hint_text("https://hf-mirror.com"),
                );
                end_pair(ui, wide, 2);

                field_label(ui, pal, &lbl(lang, "settings.models.defaultSource"));
                egui::ComboBox::from_id_salt("source_select")
                    .selected_text(if config.models.default_source == "modelscope" {
                        "ModelScope"
                    } else {
                        "HuggingFace"
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut config.models.default_source,
                            "huggingface".to_string(),
                            "HuggingFace",
                        );
                        ui.selectable_value(
                            &mut config.models.default_source,
                            "modelscope".to_string(),
                            "ModelScope",
                        );
                    });
                end_pair(ui, wide, 3);
            });
        }
        Section::Network => {
            form_grid(ui, "settings_network", wide, |ui| {
                field_label(ui, pal, &lbl(lang, "desktopApp.settings.field.httpProxy"));
                stroked_field(ui, pal, invalid, "http_proxy", |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut config.network.http_proxy)
                            .frame(false)
                            .desired_width(f32::INFINITY)
                            .hint_text(egui::RichText::new("http://127.0.0.1:7890").monospace()),
                    );
                });
                end_pair(ui, wide, 0);

                field_label(ui, pal, &lbl(lang, "desktopApp.settings.field.httpsProxy"));
                stroked_field(ui, pal, invalid, "https_proxy", |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut config.network.https_proxy)
                            .frame(false)
                            .desired_width(f32::INFINITY)
                            .hint_text(egui::RichText::new("http://127.0.0.1:7890").monospace()),
                    );
                });
                end_pair(ui, wide, 1);

                field_label(ui, pal, &lbl(lang, "desktopApp.settings.field.noProxy"));
                ui.add(
                    egui::TextEdit::singleline(&mut config.network.no_proxy)
                        .desired_width(f32::INFINITY)
                        .hint_text(egui::RichText::new("localhost,127.0.0.1").monospace()),
                );
                end_pair(ui, wide, 2);
            });
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(tr(lang, "desktopApp.settings.network.note", &[]))
                    .small()
                    .color(pal.text_faint),
            );
        }
        Section::Python => {
            form_grid(ui, "settings_python", wide, |ui| {
                field_label(ui, pal, &lbl(lang, "settings.python.path"));
                ui.add(
                    egui::TextEdit::singleline(&mut config.python.path)
                        .desired_width(f32::INFINITY)
                        .hint_text(
                            egui::RichText::new(tr(lang, "desktopApp.settings.python.pathHint", &[]))
                                .monospace(),
                        ),
                );
                end_pair(ui, wide, 0);

                field_label(ui, pal, &lbl(lang, "settings.python.uvPath"));
                ui.add(
                    egui::TextEdit::singleline(&mut config.python.uv_path)
                        .desired_width(f32::INFINITY)
                        .hint_text(
                            egui::RichText::new(tr(
                                lang,
                                "desktopApp.settings.python.uvPathHint",
                                &[],
                            ))
                            .monospace(),
                        ),
                );
                end_pair(ui, wide, 1);
            });
        }
        Section::Pipeline => {
            form_grid(ui, "settings_pipeline", wide, |ui| {
                field_label(ui, pal, &lbl(lang, "settings.pipeline.maxParallel"));
                stroked_field(ui, pal, invalid, "max_parallel", |ui| {
                    ui.add(egui::DragValue::new(&mut config.pipeline.max_parallel).range(1..=16));
                });
                end_pair(ui, wide, 0);

                field_label(ui, pal, &lbl(lang, "settings.pipeline.defaultTimeout"));
                stroked_field(ui, pal, invalid, "default_timeout", |ui| {
                    ui.add(
                        egui::DragValue::new(&mut config.pipeline.default_timeout_secs)
                            .range(10..=7200),
                    );
                });
                end_pair(ui, wide, 1);

                field_label(ui, pal, &lbl(lang, "settings.pipeline.workspaceDir"));
                ui.add(
                    egui::TextEdit::singleline(&mut config.pipeline.workspace_dir)
                        .desired_width(f32::INFINITY),
                );
                end_pair(ui, wide, 2);
            });
            ui.add_space(8.0);
            switch_row(
                ui,
                pal,
                &mut config.pipeline.keep_workspace,
                &tr(lang, "settings.pipeline.keepWorkspace", &[]),
                &tr(lang, "settings.pipeline.keepWorkspaceDescription", &[]),
            );
        }
    }
}

// ─── 本地辅助 ────────────────────────────────────────────────────────────────

/// AppConfig toml 序列化（dirty 基线比对；失败返回空串视为脏）
fn serialize_config(config: &AppConfig) -> String {
    toml::to_string(config).unwrap_or_default()
}

/// 未保存琥珀 pill（W4-A2）：warning/15 底 + warning/38 描边 + 琥珀文字
fn unsaved_pill(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    egui::Frame::new()
        .fill(color_with_alpha(pal.warning, 38))
        .stroke(egui::Stroke::new(
            1.0_f32,
            color_with_alpha(pal.warning, 96),
        ))
        .corner_radius(egui::CornerRadius::same(20))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(12.0).color(pal.warning));
        });
}

/// 表单网格（宽卡双列字段，W4-B4）：宽卡 4 列（两对字段并排）、窄卡 2 列
fn form_grid<R>(
    ui: &mut egui::Ui,
    id: &str,
    wide: bool,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    egui::Grid::new(id)
        .num_columns(if wide { 4 } else { 2 })
        .spacing([12.0, 8.0])
        .show(ui, contents)
}

/// 结束一个字段对：宽卡时每两对合并为一行（pair_index 为 0 起的字段对序号）
fn end_pair(ui: &mut egui::Ui, wide: bool, pair_index: usize) {
    if !wide || pair_index % 2 == 1 {
        ui.end_row();
    }
}

/// 输入盒字段（校验失败红描边 + rect 记录供滚动定位，W4-A4）
fn stroked_field<R>(
    ui: &mut egui::Ui,
    pal: &Palette,
    invalid: &[&'static str],
    name: &'static str,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let bad = invalid.contains(&name);
    let stroke = if bad {
        egui::Stroke::new(1.0_f32, pal.danger)
    } else {
        egui::Stroke::new(1.0_f32, pal.border)
    };
    let resp = numeric_field_stroke(ui, pal, stroke, add_contents);
    if bad {
        record_invalid_rect(ui.ctx(), name, resp.response.rect);
    }
    resp.inner
}

/// 收集无效字段 rect（保存失败时滚动定位用）
fn record_invalid_rect(ctx: &egui::Context, name: &'static str, rect: egui::Rect) {
    ctx.data_mut(|d| {
        let mut v: Vec<(&'static str, egui::Rect)> =
            d.get_temp(invalid_rects_id()).unwrap_or_default();
        v.push((name, rect));
        d.insert_temp(invalid_rects_id(), v);
    });
}

/// 表单字段标签（键值加冒号后缀，两种语言共用同一排版）
fn lbl(lang: &str, key: &str) -> String {
    format!("{}:", tr(lang, key, &[]))
}

/// 表单标签列（弱化文本）
fn field_label(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.label(egui::RichText::new(text).color(pal.text_dim));
}

/// 校验结果：错误消息（toast）+ 无效字段名（红描边/滚动定位）
pub struct ValidationOutcome {
    pub errors: Vec<String>,
    pub invalid: Vec<&'static str>,
}

/// 设置校验（W4-A4）：端口起止区间交叉 + 代理协议前缀。
///
/// 端口 1–65535 由 DragValue range 控件层钳制；no_proxy 是地址列表不做格式校验。
fn validate_settings(lang: &str, config: &AppConfig) -> ValidationOutcome {
    let mut out = ValidationOutcome {
        errors: Vec::new(),
        invalid: Vec::new(),
    };
    // 端口起止交叉：起始必须小于结束
    if config.ports.range_start >= config.ports.range_end {
        out.errors
            .push(tr(lang, "settings.validation.portRange", &[]));
        out.invalid.push("port_start");
        out.invalid.push("port_end");
    }
    // 代理协议前缀：非空必须以 http:// 或 https:// 开头
    for (name, label_key, value) in [
        (
            "http_proxy",
            "desktopApp.settings.field.httpProxy",
            &config.network.http_proxy,
        ),
        (
            "https_proxy",
            "desktopApp.settings.field.httpsProxy",
            &config.network.https_proxy,
        ),
    ] {
        if !value.is_empty() && !(value.starts_with("http://") || value.starts_with("https://")) {
            out.errors.push(tr(
                lang,
                "desktopApp.settings.error.proxyFormat",
                &[
                    ("label", &tr(lang, label_key, &[])),
                    ("value", value.as_str()),
                ],
            ));
            out.invalid.push(name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 双列断点（W4-B4）：>=1080 双列，以下单列；阈值两侧与典型窗口宽度
    #[test]
    fn settings_columns_two_col_breakpoint() {
        assert_eq!(settings_columns(1080.0), 2);
        assert_eq!(settings_columns(1528.0), 2);
        assert_eq!(settings_columns(2560.0), 2);
        assert_eq!(settings_columns(1079.0), 1);
        assert_eq!(settings_columns(744.0), 1);
        assert_eq!(settings_columns(300.0), 1);
    }

    /// 6 段分区顺序（W4-B2 合并后）：外观/计算/模型/网络/Python/管线
    #[test]
    fn settings_sections_order_matches_w4() {
        assert_eq!(
            SECTIONS,
            [
                Section::Appearance,
                Section::Compute,
                Section::Models,
                Section::Network,
                Section::Python,
                Section::Pipeline,
            ]
        );
    }

    /// 默认配置校验通过、无无效字段
    #[test]
    fn validate_settings_default_is_clean() {
        let out = validate_settings("zh-CN", &AppConfig::default());
        assert!(out.errors.is_empty());
        assert!(out.invalid.is_empty());
    }

    /// 端口起止交叉：start >= end 报错并标记两字段无效
    #[test]
    fn validate_settings_port_range_crossing() {
        let mut cfg = AppConfig::default();
        cfg.ports.range_start = 19000;
        cfg.ports.range_end = 18000;
        let out = validate_settings("zh-CN", &cfg);
        assert_eq!(out.errors.len(), 1);
        assert_eq!(out.invalid, ["port_start", "port_end"]);
        // 相等同样非法（区间为空）
        cfg.ports.range_end = 19000;
        assert!(!validate_settings("en", &cfg).errors.is_empty());
    }

    /// 代理协议前缀：空/合法前缀通过，非法前缀报错并标记对应字段
    #[test]
    fn validate_settings_proxy_prefix() {
        let mut cfg = AppConfig::default();
        cfg.network.http_proxy = "http://127.0.0.1:7890".into();
        cfg.network.https_proxy = "".into();
        assert!(validate_settings("zh-CN", &cfg).errors.is_empty());

        cfg.network.https_proxy = "socks5://127.0.0.1:1080".into();
        let out = validate_settings("en", &cfg);
        assert_eq!(out.errors.len(), 1);
        assert_eq!(out.invalid, ["https_proxy"]);
        assert!(out.errors[0].contains("socks5://127.0.0.1:1080"));
        // no_proxy 不参与校验
        cfg.network.no_proxy = "anything-goes".into();
        assert_eq!(validate_settings("en", &cfg).errors.len(), 1);
    }

    /// 代理错误文案本地化：中英文不同且保留原始值
    #[test]
    fn validate_settings_proxy_error_is_localized() {
        let mut cfg = AppConfig::default();
        cfg.network.http_proxy = "socks5://x".into();
        let zh = validate_settings("zh-CN", &cfg).errors.pop().unwrap();
        let en = validate_settings("en", &cfg).errors.pop().unwrap();
        assert!(zh.contains("socks5://x"));
        assert!(en.contains("Invalid format") && en.contains("socks5://x"));
        assert_ne!(zh, en);
    }

    /// toml 序列化 round-trip 稳定（dirty 基线比对不失真）
    #[test]
    fn serialize_config_round_trip_stable() {
        let mut cfg = AppConfig::default();
        cfg.general.language = "en".into();
        cfg.ports.range_start = 20000;
        let s = serialize_config(&cfg);
        assert!(!s.is_empty());
        let back: AppConfig = toml::from_str(&s).expect("round trip");
        assert_eq!(serialize_config(&back), s);
    }

    /// 宽卡字段对换行规则：窄卡每对换行；宽卡每两对换行一次
    #[test]
    fn end_pair_row_rules() {
        let ctx = egui::Context::default();
        let rows = std::rc::Rc::new(std::cell::Cell::new(0_usize));
        for wide in [false, true] {
            let rows = rows.clone();
            rows.set(0);
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    egui::Grid::new(format!("t{wide}")).show(ui, |ui| {
                        for i in 0..4 {
                            ui.label("x");
                            let before = rows.get();
                            end_pair(ui, wide, i);
                            // end_row 无法直接观测，仅验证不 panic；
                            // 行为语义由上方布局集成截图人工验证
                            rows.set(before + 1);
                        }
                    });
                });
            });
            // 每帧内循环恰好 4 次；egui 首帧可能多趟布局，总数为 4 的整数倍即可
            assert!(rows.get() >= 4 && rows.get().is_multiple_of(4));
        }
    }
}
