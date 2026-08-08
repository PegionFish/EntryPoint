//! 设置页 — 双列卡片布局（§7.5，UNIFIED_UI_REDESIGN_PROPOSAL）。
//!
//! - 8 段分区（general/compute/models/ports/network/python/pipeline/ui），
//!   内容宽度 >=1240px 双列、窄窗单列（`settings_columns`）；
//! - 两处空标签 checkbox 缺陷（check_updates / allow_overcommit）经
//!   [`crate::ui::switch_row`] 修复：控件自身携带可见文案与描述；
//! - 端口区间以「起止标签 + 带底带框输入盒」呈现明确可编辑观感（§3.4）；
//! - 保存配置/重新加载常驻页头动作条（含主按钮辉光层级）。
//!
//! 所有用户可见文案经 [`crate::i18n::tr`] 查找；语言跟随
//! `config.general.language`，每帧归一化读取，切换即时生效。

use eframe::egui;
use ep_core::config::{AppConfig, AssignStrategy, NetworkConfig};

use crate::i18n::tr;
use crate::toast::ToastManager;
use crate::ui::{
    card, card_grid, numeric_field, page_header, primary_button_with_glow, section_title,
    subtle_button, switch_row, Palette,
};

/// 设置双列断点（§7.5）：内容区宽度 >=1240px 双列卡片，以下单列
pub const SETTINGS_TWO_COL_MIN_WIDTH: f32 = 1240.0;

/// 设置页卡片列数：>=1240px 双列，窄窗单列（与 card_grid 等宽铺满机制配合）
pub fn settings_columns(available_width: f32) -> usize {
    if available_width >= SETTINGS_TWO_COL_MIN_WIDTH {
        2
    } else {
        1
    }
}

// ─── 分区定义（§7.5：桌面 8 段顺序） ─────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Section {
    General,
    Compute,
    Models,
    Ports,
    Network,
    Python,
    Pipeline,
    Ui,
}

const SECTIONS: [Section; 8] = [
    Section::General,
    Section::Compute,
    Section::Models,
    Section::Ports,
    Section::Network,
    Section::Python,
    Section::Pipeline,
    Section::Ui,
];

// ─── 主入口 ──────────────────────────────────────────────────────────────────

pub fn show(ui: &mut egui::Ui, config: &mut AppConfig, toasts: &mut ToastManager) {
    let lang = ep_core::i18n::normalize_language(&config.general.language);
    let pal = Palette::new(ui.style().visuals.dark_mode);

    // 页级动作条（P2-2）：「保存配置 / 重新加载」属全局动作，常驻页头
    //（不随内容滚动而不可见）；主按钮按新令牌附辉光表达层级（§3.4）。
    // page_header 动作区为 right_to_left 布局：先加的保存按钮居最右。
    page_header(ui, &tr(lang, "settings.title", &[]), |ui| {
        if primary_button_with_glow(ui, &pal, tr(lang, "desktopApp.settings.saveBtn", &[]))
            .clicked()
        {
            if let Err(msg) = validate_network(lang, &config.network) {
                toasts.error(msg);
            } else {
                let config_dir = ep_core::config::resolve_root().join("config");
                match config.save(&config_dir) {
                    Ok(()) => toasts.success(tr(lang, "settings.toast.saved", &[])),
                    Err(e) => toasts.error(tr(
                        lang,
                        "desktopApp.settings.error.saveFailed",
                        &[("detail", &e.to_string())],
                    )),
                }
            }
        }
        if ui
            .add(subtle_button(&pal, tr(lang, "desktopApp.settings.reloadBtn", &[])))
            .clicked()
        {
            let config_dir = ep_core::config::resolve_root().join("config");
            match AppConfig::load(&config_dir) {
                Ok(loaded) => {
                    *config = loaded;
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

    // 主滚动区启用键盘滚动（P2-1）；双列卡片网格间距 16
    crate::ui::keyboard_scroll(ui, "settings_main", egui::ScrollArea::vertical(), |ui| {
        ui.spacing_mut().item_spacing = egui::vec2(16.0, 16.0);
        let cols = settings_columns(ui.available_width());
        card_grid(ui, cols, &SECTIONS, |ui, sec| {
            render_section(ui, &pal, lang, config, *sec)
        });
        ui.add_space(8.0);
    });
}

// ─── 分区渲染 ────────────────────────────────────────────────────────────────

fn render_section(
    ui: &mut egui::Ui,
    pal: &Palette,
    lang: &str,
    config: &mut AppConfig,
    section: Section,
) {
    match section {
        Section::General => {
            card(ui, pal, |ui| {
                section_title(ui, &tr(lang, "settings.general.title", &[]));
                ui.add_space(10.0);
                egui::Grid::new("settings_language")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        field_label(ui, pal, &lbl(lang, "common.label.language"));
                        let current =
                            ep_core::i18n::normalize_language(&config.general.language);
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
                        ui.end_row();
                    });
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(tr(lang, "desktopApp.settings.languageHint", &[]))
                        .small()
                        .color(pal.text_faint),
                );
                ui.add_space(10.0);
                // 空标签缺陷修复（§9 SwitchRow）：控件直接携带可见文案与描述
                switch_row(
                    ui,
                    pal,
                    &mut config.general.check_updates,
                    &tr(lang, "settings.general.checkUpdates", &[]),
                    &tr(lang, "settings.general.checkUpdatesDescription", &[]),
                );
            });
        }
        Section::Compute => {
            card(ui, pal, |ui| {
                section_title(ui, &tr(lang, "settings.compute.title", &[]));
                ui.add_space(10.0);
                egui::Grid::new("settings_compute")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
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
                                std::mem::discriminant(s)
                                    == std::mem::discriminant(&config.compute.strategy)
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
                        ui.end_row();

                        field_label(ui, pal, &lbl(lang, "settings.compute.refreshInterval"));
                        numeric_field(ui, pal, |ui| {
                            ui.add(
                                egui::DragValue::new(&mut config.compute.refresh_interval_secs)
                                    .range(1..=60),
                            );
                        });
                        ui.end_row();
                    });
                ui.add_space(8.0);
                // 空标签缺陷修复（§9 SwitchRow）：控件直接携带可见文案与描述
                switch_row(
                    ui,
                    pal,
                    &mut config.compute.allow_overcommit,
                    &tr(lang, "settings.compute.allowOvercommit", &[]),
                    &tr(lang, "settings.compute.allowOvercommitDescription", &[]),
                );
            });
        }
        Section::Models => {
            card(ui, pal, |ui| {
                section_title(ui, &tr(lang, "settings.models.title", &[]));
                ui.add_space(10.0);
                egui::Grid::new("settings_models")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        field_label(ui, pal, &lbl(lang, "settings.models.cacheDir"));
                        ui.add(
                            egui::TextEdit::singleline(&mut config.models.cache_dir)
                                .desired_width(f32::INFINITY),
                        );
                        ui.end_row();

                        // 解析后的绝对路径（P2-2）：相对 cache_dir 基于应用根解析，
                        // 与仪表盘/模块页口径一致，避免仅见相对值 `models` 的歧义。
                        // 只读行：mono 弱化呈现实际路径。
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
                        ui.end_row();

                        field_label(ui, pal, &lbl(lang, "settings.models.hfEndpoint"));
                        ui.add(
                            egui::TextEdit::singleline(&mut config.models.hf_endpoint)
                                .desired_width(f32::INFINITY)
                                .hint_text("https://hf-mirror.com"),
                        );
                        ui.end_row();

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
                        ui.end_row();
                    });
            });
        }
        Section::Ports => {
            card(ui, pal, |ui| {
                section_title(ui, &tr(lang, "desktopApp.settings.section.ports", &[]));
                ui.add_space(10.0);
                egui::Grid::new("settings_ports")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        // 明确可编辑观感（§3.4）：起止标签 + 带底带框输入盒
                        field_label(ui, pal, &lbl(lang, "settings.ports.rangeStart"));
                        numeric_field(ui, pal, |ui| {
                            ui.add(
                                egui::DragValue::new(&mut config.ports.range_start)
                                    .range(1024..=65535),
                            );
                        });
                        ui.end_row();

                        field_label(ui, pal, &lbl(lang, "settings.ports.rangeEnd"));
                        numeric_field(ui, pal, |ui| {
                            ui.add(
                                egui::DragValue::new(&mut config.ports.range_end)
                                    .range(1024..=65535),
                            );
                        });
                        ui.end_row();
                    });
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(tr(lang, "settings.ports.description", &[]))
                        .small()
                        .color(pal.text_faint),
                );
            });
        }
        Section::Network => {
            card(ui, pal, |ui| {
                section_title(ui, &tr(lang, "settings.network.title", &[]));
                ui.add_space(10.0);
                egui::Grid::new("settings_network")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        field_label(ui, pal, &lbl(lang, "desktopApp.settings.field.httpProxy"));
                        ui.add(
                            egui::TextEdit::singleline(&mut config.network.http_proxy)
                                .desired_width(f32::INFINITY)
                                .hint_text(egui::RichText::new("http://127.0.0.1:7890").monospace()),
                        );
                        ui.end_row();

                        field_label(ui, pal, &lbl(lang, "desktopApp.settings.field.httpsProxy"));
                        ui.add(
                            egui::TextEdit::singleline(&mut config.network.https_proxy)
                                .desired_width(f32::INFINITY)
                                .hint_text(egui::RichText::new("http://127.0.0.1:7890").monospace()),
                        );
                        ui.end_row();

                        field_label(ui, pal, &lbl(lang, "desktopApp.settings.field.noProxy"));
                        ui.add(
                            egui::TextEdit::singleline(&mut config.network.no_proxy)
                                .desired_width(f32::INFINITY)
                                .hint_text(
                                    egui::RichText::new("localhost,127.0.0.1").monospace(),
                                ),
                        );
                        ui.end_row();
                    });
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(tr(lang, "desktopApp.settings.network.note", &[]))
                        .small()
                        .color(pal.text_faint),
                );
            });
        }
        Section::Python => {
            card(ui, pal, |ui| {
                section_title(ui, &tr(lang, "desktopApp.settings.section.python", &[]));
                ui.add_space(10.0);
                egui::Grid::new("settings_python")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        field_label(ui, pal, &lbl(lang, "settings.python.path"));
                        ui.add(
                            egui::TextEdit::singleline(&mut config.python.path)
                                .desired_width(f32::INFINITY)
                                .hint_text(
                                    egui::RichText::new(tr(
                                        lang,
                                        "desktopApp.settings.python.pathHint",
                                        &[],
                                    ))
                                    .monospace(),
                                ),
                        );
                        ui.end_row();

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
                        ui.end_row();
                    });
            });
        }
        Section::Pipeline => {
            card(ui, pal, |ui| {
                section_title(ui, &tr(lang, "desktopApp.settings.section.pipeline", &[]));
                ui.add_space(10.0);
                egui::Grid::new("settings_pipeline")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        field_label(ui, pal, &lbl(lang, "settings.pipeline.maxParallel"));
                        numeric_field(ui, pal, |ui| {
                            ui.add(
                                egui::DragValue::new(&mut config.pipeline.max_parallel)
                                    .range(1..=16),
                            );
                        });
                        ui.end_row();

                        field_label(ui, pal, &lbl(lang, "settings.pipeline.defaultTimeout"));
                        numeric_field(ui, pal, |ui| {
                            ui.add(
                                egui::DragValue::new(&mut config.pipeline.default_timeout_secs)
                                    .range(10..=7200),
                            );
                        });
                        ui.end_row();

                        field_label(ui, pal, &lbl(lang, "settings.pipeline.workspaceDir"));
                        ui.add(
                            egui::TextEdit::singleline(&mut config.pipeline.workspace_dir)
                                .desired_width(f32::INFINITY),
                        );
                        ui.end_row();
                    });
                ui.add_space(8.0);
                // 与 check_updates/allow_overcommit 同口径：控件携带可见文案
                switch_row(
                    ui,
                    pal,
                    &mut config.pipeline.keep_workspace,
                    &tr(lang, "settings.pipeline.keepWorkspace", &[]),
                    &tr(lang, "settings.pipeline.keepWorkspaceDescription", &[]),
                );
            });
        }
        Section::Ui => {
            card(ui, pal, |ui| {
                section_title(ui, &tr(lang, "desktopApp.settings.section.ui", &[]));
                ui.add_space(10.0);
                egui::Grid::new("settings_ui")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        field_label(ui, pal, &lbl(lang, "desktopApp.settings.ui.zoom"));
                        numeric_field(ui, pal, |ui| {
                            ui.add(
                                egui::DragValue::new(&mut config.ui.scale_factor)
                                    .range(0.5..=3.0)
                                    .speed(0.1),
                            );
                        });
                        ui.end_row();

                        field_label(ui, pal, &lbl(lang, "desktopApp.settings.ui.fontSize"));
                        numeric_field(ui, pal, |ui| {
                            ui.add(
                                egui::DragValue::new(&mut config.ui.font_size).range(10.0..=24.0),
                            );
                        });
                        ui.end_row();
                    });
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(tr(lang, "desktopApp.settings.ui.applyNote", &[]))
                        .small()
                        .color(pal.text_faint),
                );
            });
        }
    }
}

// ─── 本地辅助 ────────────────────────────────────────────────────────────────

/// 表单字段标签（键值加冒号后缀，两种语言共用同一排版）
fn lbl(lang: &str, key: &str) -> String {
    format!("{}:", tr(lang, key, &[]))
}

/// 表单标签列（弱化文本）
fn field_label(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.label(egui::RichText::new(text).color(pal.text_dim));
}

/// 校验代理配置：非空的代理地址必须以 http:// 或 https:// 开头。
///
/// no_proxy 是地址列表，不做格式校验。错误消息使用本地化的字段名。
fn validate_network(lang: &str, network: &NetworkConfig) -> Result<(), String> {
    for (label_key, value) in [
        ("desktopApp.settings.field.httpProxy", &network.http_proxy),
        ("desktopApp.settings.field.httpsProxy", &network.https_proxy),
    ] {
        if !value.is_empty() && !(value.starts_with("http://") || value.starts_with("https://")) {
            return Err(tr(
                lang,
                "desktopApp.settings.error.proxyFormat",
                &[
                    ("label", &tr(lang, label_key, &[])),
                    ("value", value.as_str()),
                ],
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(http: &str, https: &str) -> NetworkConfig {
        NetworkConfig {
            http_proxy: http.to_string(),
            https_proxy: https.to_string(),
            no_proxy: "localhost,127.0.0.1".to_string(),
        }
    }

    /// 双列断点（§7.5）：>=1240 双列，以下单列；阈值两侧与典型窗口宽度
    #[test]
    fn settings_columns_two_col_breakpoint() {
        assert_eq!(settings_columns(1240.0), 2);
        assert_eq!(settings_columns(1528.0), 2);
        assert_eq!(settings_columns(2560.0), 2);
        assert_eq!(settings_columns(1239.0), 1);
        assert_eq!(settings_columns(744.0), 1);
        assert_eq!(settings_columns(300.0), 1);
    }

    /// 8 段分区顺序对齐 §7.5：general/compute/models/ports/network/python/pipeline/ui
    #[test]
    fn settings_sections_order_matches_proposal() {
        assert_eq!(
            SECTIONS,
            [
                Section::General,
                Section::Compute,
                Section::Models,
                Section::Ports,
                Section::Network,
                Section::Python,
                Section::Pipeline,
                Section::Ui,
            ]
        );
    }

    #[test]
    fn validate_network_accepts_empty_and_valid_prefixes() {
        assert!(validate_network("zh-CN", &net("", "")).is_ok());
        assert!(validate_network("zh-CN", &net("http://127.0.0.1:7890", "")).is_ok());
        assert!(validate_network("en", &net("", "https://proxy.local:8080")).is_ok());
    }

    #[test]
    fn validate_network_rejects_bad_prefixes() {
        assert!(validate_network("zh-CN", &net("socks5://127.0.0.1:1080", "")).is_err());
        assert!(validate_network("en", &net("", "127.0.0.1:7890")).is_err());
        // no_proxy 不参与校验，任意值不影响结果
        let mut bad = net("ftp://x", "");
        bad.no_proxy = "anything-goes".to_string();
        assert!(validate_network("zh-CN", &bad).is_err());
    }

    #[test]
    fn validate_network_error_is_localized() {
        let bad = net("socks5://127.0.0.1:1080", "");
        let zh = validate_network("zh-CN", &bad).unwrap_err();
        let en = validate_network("en", &bad).unwrap_err();
        // 两种语言都带上原始值，且文案确实随语言切换
        assert!(zh.contains("socks5://127.0.0.1:1080"));
        assert!(en.contains("Invalid format") && en.contains("socks5://127.0.0.1:1080"));
        assert_ne!(zh, en);
    }
}
