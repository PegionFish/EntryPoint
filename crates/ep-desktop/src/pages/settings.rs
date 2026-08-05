//! 设置页 — 分区卡片布局，保存/重载反馈走 Toast 通知。
//!
//! 所有用户可见文案经 [`crate::i18n::tr`] 查找；语言跟随
//! `config.general.language`，每帧归一化读取，切换即时生效。

use eframe::egui;
use ep_core::config::{AppConfig, AssignStrategy, NetworkConfig};

use crate::i18n::tr;
use crate::toast::ToastManager;
use crate::ui::{card, page_header, primary_button, section_title, subtle_button, Palette};

// ─── 主入口 ──────────────────────────────────────────────────────────────────

pub fn show(ui: &mut egui::Ui, config: &mut AppConfig, toasts: &mut ToastManager) {
    let lang = ep_core::i18n::normalize_language(&config.general.language);
    let pal = Palette::new(ui.style().visuals.dark_mode);

    page_header(ui, &tr(lang, "settings.title", &[]), |_| {});

    egui::ScrollArea::vertical().show(ui, |ui| {
        // 各分区卡片间距 12
        ui.spacing_mut().item_spacing.y = 12.0;

        // ── 界面语言（置顶：切换即时生效，保存配置后落盘） ──
        card(ui, &pal, |ui| {
            section_title(ui, &tr(lang, "settings.general.language", &[]));
            ui.add_space(8.0);
            egui::Grid::new("settings_language")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    field_label(ui, &pal, &lbl(lang, "common.label.language"));
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
                    ui.end_row();
                });
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(tr(lang, "desktopApp.settings.languageHint", &[]))
                    .small()
                    .color(pal.text_faint),
            );
        });

        // ── 启动时检查更新（#51 延后项：general.check_updates 桌面端接线） ──
        card(ui, &pal, |ui| {
            section_title(ui, &tr(lang, "settings.general.checkUpdates", &[]));
            ui.add_space(8.0);
            egui::Grid::new("settings_check_updates")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    field_label(ui, &pal, &lbl(lang, "settings.general.checkUpdates"));
                    ui.checkbox(&mut config.general.check_updates, "");
                    ui.end_row();
                });
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(tr(
                    lang,
                    "settings.general.checkUpdatesDescription",
                    &[],
                ))
                .small()
                .color(pal.text_faint),
            );
        });

        // ── 计算设备 ──
        section_card(
            ui,
            &pal,
            &tr(lang, "settings.compute.title", &[]),
            "settings_compute",
            |ui, pal| {
                field_label(ui, pal, &lbl(lang, "settings.compute.strategy"));
                let strategies: [(AssignStrategy, &str); 4] = [
                    (AssignStrategy::Manual, "settings.strategy.manual"),
                    (AssignStrategy::LeastMemory, "settings.strategy.leastMemory"),
                    (AssignStrategy::RoundRobin, "settings.strategy.roundRobin"),
                    (AssignStrategy::Single(None), "desktopApp.settings.strategy.single"),
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
                ui.end_row();

                field_label(ui, pal, &lbl(lang, "settings.compute.allowOvercommit"));
                ui.checkbox(&mut config.compute.allow_overcommit, "");
                ui.end_row();

                field_label(ui, pal, &lbl(lang, "settings.compute.refreshInterval"));
                ui.add(
                    egui::DragValue::new(&mut config.compute.refresh_interval_secs)
                        .range(1..=60),
                );
                ui.end_row();
            },
        );

        // ── 端口 ──
        section_card(
            ui,
            &pal,
            &tr(lang, "desktopApp.settings.section.ports", &[]),
            "settings_ports",
            |ui, pal| {
                field_label(ui, pal, &lbl(lang, "settings.ports.title"));
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut config.ports.range_start).range(1024..=65535),
                    );
                    ui.label("—");
                    ui.add(
                        egui::DragValue::new(&mut config.ports.range_end).range(1024..=65535),
                    );
                });
                ui.end_row();
            },
        );

        // ── 模型 ──
        section_card(
            ui,
            &pal,
            &tr(lang, "settings.models.title", &[]),
            "settings_models",
            |ui, pal| {
                field_label(ui, pal, &lbl(lang, "settings.models.cacheDir"));
                ui.add(
                    egui::TextEdit::singleline(&mut config.models.cache_dir)
                        .desired_width(f32::INFINITY),
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
            },
        );

        // ── 网络与代理 ──
        card(ui, &pal, |ui| {
            section_title(ui, &tr(lang, "settings.network.title", &[]));
            ui.add_space(8.0);
            egui::Grid::new("settings_network")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    field_label(ui, &pal, &lbl(lang, "desktopApp.settings.field.httpProxy"));
                    ui.add(
                        egui::TextEdit::singleline(&mut config.network.http_proxy)
                            .desired_width(f32::INFINITY)
                            .hint_text(egui::RichText::new("http://127.0.0.1:7890").monospace()),
                    );
                    ui.end_row();

                    field_label(ui, &pal, &lbl(lang, "desktopApp.settings.field.httpsProxy"));
                    ui.add(
                        egui::TextEdit::singleline(&mut config.network.https_proxy)
                            .desired_width(f32::INFINITY)
                            .hint_text(egui::RichText::new("http://127.0.0.1:7890").monospace()),
                    );
                    ui.end_row();

                    field_label(ui, &pal, &lbl(lang, "desktopApp.settings.field.noProxy"));
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

        // ── Python 环境 ──
        section_card(
            ui,
            &pal,
            &tr(lang, "desktopApp.settings.section.python", &[]),
            "settings_python",
            |ui, pal| {
                field_label(ui, pal, &lbl(lang, "settings.python.path"));
                ui.add(
                    egui::TextEdit::singleline(&mut config.python.path)
                        .desired_width(f32::INFINITY)
                        .hint_text(
                            egui::RichText::new(tr(lang, "desktopApp.settings.python.pathHint", &[]))
                                .monospace(),
                        ),
                );
                ui.end_row();

                field_label(ui, pal, &lbl(lang, "settings.python.uvPath"));
                ui.add(
                    egui::TextEdit::singleline(&mut config.python.uv_path)
                        .desired_width(f32::INFINITY)
                        .hint_text(
                            egui::RichText::new(tr(lang, "desktopApp.settings.python.uvPathHint", &[]))
                                .monospace(),
                        ),
                );
                ui.end_row();
            },
        );

        // ── 管线引擎 ──
        section_card(
            ui,
            &pal,
            &tr(lang, "desktopApp.settings.section.pipeline", &[]),
            "settings_pipeline",
            |ui, pal| {
                field_label(ui, pal, &lbl(lang, "settings.pipeline.maxParallel"));
                ui.add(egui::DragValue::new(&mut config.pipeline.max_parallel).range(1..=16));
                ui.end_row();

                field_label(ui, pal, &lbl(lang, "settings.pipeline.defaultTimeout"));
                ui.add(
                    egui::DragValue::new(&mut config.pipeline.default_timeout_secs)
                        .range(10..=7200),
                );
                ui.end_row();

                field_label(ui, pal, &lbl(lang, "settings.pipeline.keepWorkspace"));
                ui.checkbox(&mut config.pipeline.keep_workspace, "");
                ui.end_row();

                field_label(ui, pal, &lbl(lang, "settings.pipeline.workspaceDir"));
                ui.add(
                    egui::TextEdit::singleline(&mut config.pipeline.workspace_dir)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();
            },
        );

        // ── 界面 ──
        card(ui, &pal, |ui| {
            section_title(ui, &tr(lang, "desktopApp.settings.section.ui", &[]));
            ui.add_space(8.0);
            egui::Grid::new("settings_ui")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    field_label(ui, &pal, &lbl(lang, "desktopApp.settings.ui.zoom"));
                    ui.add(
                        egui::DragValue::new(&mut config.ui.scale_factor)
                            .range(0.5..=3.0)
                            .speed(0.1),
                    );
                    ui.end_row();

                    field_label(ui, &pal, &lbl(lang, "desktopApp.settings.ui.fontSize"));
                    ui.add(egui::DragValue::new(&mut config.ui.font_size).range(10.0..=24.0));
                    ui.end_row();
                });
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(tr(lang, "desktopApp.settings.ui.applyNote", &[]))
                    .small()
                    .color(pal.text_faint),
            );
        });

        // ── 底部操作行 ──
        ui.horizontal(|ui| {
            if ui
                .add(primary_button(&pal, tr(lang, "desktopApp.settings.saveBtn", &[])))
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
        ui.add_space(8.0);
    });
}

// ─── 本地辅助 ────────────────────────────────────────────────────────────────

/// 表单字段标签（键值加冒号后缀，两种语言共用同一排版）
fn lbl(lang: &str, key: &str) -> String {
    format!("{}:", tr(lang, key, &[]))
}

/// 分区卡片：section_title + 双列 Grid
fn section_card(
    ui: &mut egui::Ui,
    pal: &Palette,
    title: &str,
    grid_id: &str,
    body: impl FnOnce(&mut egui::Ui, &Palette),
) {
    card(ui, pal, |ui| {
        section_title(ui, title);
        ui.add_space(8.0);
        egui::Grid::new(grid_id)
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| body(ui, pal));
    });
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
