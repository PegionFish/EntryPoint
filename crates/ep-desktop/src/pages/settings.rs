//! 设置页 — 分区卡片布局，保存/重载反馈走 Toast 通知。

use eframe::egui;
use ep_core::config::{AppConfig, AssignStrategy, NetworkConfig};

use crate::toast::ToastManager;
use crate::ui::{card, page_header, primary_button, section_title, subtle_button, Palette};

// ─── 主入口 ──────────────────────────────────────────────────────────────────

pub fn show(ui: &mut egui::Ui, config: &mut AppConfig, toasts: &mut ToastManager) {
    let pal = Palette::new(ui.style().visuals.dark_mode);

    page_header(ui, "设置", |_| {});

    egui::ScrollArea::vertical().show(ui, |ui| {
        // 各分区卡片间距 12
        ui.spacing_mut().item_spacing.y = 12.0;

        // ── 计算设备 ──
        section_card(ui, &pal, "计算设备", "settings_compute", |ui, pal| {
            field_label(ui, pal, "分配策略:");
            let strategies = [
                (AssignStrategy::Manual, "手动"),
                (AssignStrategy::LeastMemory, "最小显存优先"),
                (AssignStrategy::RoundRobin, "轮询"),
                (AssignStrategy::Single(None), "单设备"),
            ];
            let current_label = strategies
                .iter()
                .find(|(s, _)| {
                    std::mem::discriminant(s) == std::mem::discriminant(&config.compute.strategy)
                })
                .map(|(_, l)| *l)
                .unwrap_or("未知");
            egui::ComboBox::from_id_salt("strategy_select")
                .selected_text(current_label)
                .show_ui(ui, |ui| {
                    for (strategy, label) in &strategies {
                        ui.selectable_value(
                            &mut config.compute.strategy,
                            strategy.clone(),
                            *label,
                        );
                    }
                });
            ui.end_row();

            field_label(ui, pal, "允许显存超额:");
            ui.checkbox(&mut config.compute.allow_overcommit, "");
            ui.end_row();

            field_label(ui, pal, "刷新间隔 (秒):");
            ui.add(
                egui::DragValue::new(&mut config.compute.refresh_interval_secs).range(1..=60),
            );
            ui.end_row();
        });

        // ── 端口 ──
        section_card(ui, &pal, "端口", "settings_ports", |ui, pal| {
            field_label(ui, pal, "端口范围:");
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
        });

        // ── 模型 ──
        section_card(ui, &pal, "模型", "settings_models", |ui, pal| {
            field_label(ui, pal, "缓存目录:");
            ui.add(
                egui::TextEdit::singleline(&mut config.models.cache_dir)
                    .desired_width(f32::INFINITY),
            );
            ui.end_row();

            field_label(ui, pal, "HF 镜像:");
            ui.add(
                egui::TextEdit::singleline(&mut config.models.hf_endpoint)
                    .desired_width(f32::INFINITY)
                    .hint_text("https://hf-mirror.com"),
            );
            ui.end_row();

            field_label(ui, pal, "默认下载源:");
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

        // ── 网络与代理 ──
        card(ui, &pal, |ui| {
            section_title(ui, "网络与代理");
            ui.add_space(8.0);
            egui::Grid::new("settings_network")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    field_label(ui, &pal, "HTTP 代理:");
                    ui.add(
                        egui::TextEdit::singleline(&mut config.network.http_proxy)
                            .desired_width(f32::INFINITY)
                            .hint_text(egui::RichText::new("http://127.0.0.1:7890").monospace()),
                    );
                    ui.end_row();

                    field_label(ui, &pal, "HTTPS 代理:");
                    ui.add(
                        egui::TextEdit::singleline(&mut config.network.https_proxy)
                            .desired_width(f32::INFINITY)
                            .hint_text(egui::RichText::new("http://127.0.0.1:7890").monospace()),
                    );
                    ui.end_row();

                    field_label(ui, &pal, "不走代理:");
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
                egui::RichText::new(
                    "生效范围：模型下载、Python 依赖安装（uv/pip）、模块进程；留空 = 跟随系统环境变量",
                )
                .small()
                .color(pal.text_faint),
            );
        });

        // ── Python 环境 ──
        section_card(ui, &pal, "Python 环境", "settings_python", |ui, pal| {
            field_label(ui, pal, "Python 路径:");
            ui.add(
                egui::TextEdit::singleline(&mut config.python.path)
                    .desired_width(f32::INFINITY)
                    .hint_text(egui::RichText::new("python（留空则使用 PATH）").monospace()),
            );
            ui.end_row();

            field_label(ui, pal, "uv 路径:");
            ui.add(
                egui::TextEdit::singleline(&mut config.python.uv_path)
                    .desired_width(f32::INFINITY)
                    .hint_text(egui::RichText::new("uv（留空则使用 PATH）").monospace()),
            );
            ui.end_row();
        });

        // ── 管线引擎 ──
        section_card(ui, &pal, "管线引擎", "settings_pipeline", |ui, pal| {
            field_label(ui, pal, "最大并行数:");
            ui.add(egui::DragValue::new(&mut config.pipeline.max_parallel).range(1..=16));
            ui.end_row();

            field_label(ui, pal, "默认超时 (秒):");
            ui.add(
                egui::DragValue::new(&mut config.pipeline.default_timeout_secs)
                    .range(10..=7200),
            );
            ui.end_row();

            field_label(ui, pal, "保留工作目录:");
            ui.checkbox(&mut config.pipeline.keep_workspace, "");
            ui.end_row();

            field_label(ui, pal, "工作目录:");
            ui.add(
                egui::TextEdit::singleline(&mut config.pipeline.workspace_dir)
                    .desired_width(f32::INFINITY),
            );
            ui.end_row();
        });

        // ── 界面 ──
        card(ui, &pal, |ui| {
            section_title(ui, "界面");
            ui.add_space(8.0);
            egui::Grid::new("settings_ui")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    field_label(ui, &pal, "缩放:");
                    ui.add(
                        egui::DragValue::new(&mut config.ui.scale_factor)
                            .range(0.5..=3.0)
                            .speed(0.1),
                    );
                    ui.end_row();

                    field_label(ui, &pal, "字号:");
                    ui.add(egui::DragValue::new(&mut config.ui.font_size).range(10.0..=24.0));
                    ui.end_row();
                });
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("修改后立即生效")
                    .small()
                    .color(pal.text_faint),
            );
        });

        // ── 底部操作行 ──
        ui.horizontal(|ui| {
            if ui.add(primary_button(&pal, "💾 保存配置")).clicked() {
                if let Err(msg) = validate_network(&config.network) {
                    toasts.error(msg);
                } else {
                    let config_dir = ep_core::config::resolve_root().join("config");
                    match config.save(&config_dir) {
                        Ok(()) => toasts.success("配置已保存"),
                        Err(e) => toasts.error(format!("保存失败: {e}")),
                    }
                }
            }

            if ui.add(subtle_button(&pal, "重新加载")).clicked() {
                let config_dir = ep_core::config::resolve_root().join("config");
                match AppConfig::load(&config_dir) {
                    Ok(loaded) => {
                        *config = loaded;
                        toasts.success("配置已重新加载");
                    }
                    Err(e) => toasts.error(format!("加载失败: {e}")),
                }
            }
        });
        ui.add_space(8.0);
    });
}

// ─── 本地辅助 ────────────────────────────────────────────────────────────────

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
/// no_proxy 是地址列表，不做格式校验。
fn validate_network(network: &NetworkConfig) -> Result<(), String> {
    for (label, value) in [
        ("HTTP 代理", &network.http_proxy),
        ("HTTPS 代理", &network.https_proxy),
    ] {
        if !value.is_empty() && !(value.starts_with("http://") || value.starts_with("https://")) {
            return Err(format!("{label}格式无效，必须以 http:// 或 https:// 开头：{value}"));
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
        assert!(validate_network(&net("", "")).is_ok());
        assert!(validate_network(&net("http://127.0.0.1:7890", "")).is_ok());
        assert!(validate_network(&net("", "https://proxy.local:8080")).is_ok());
    }

    #[test]
    fn validate_network_rejects_bad_prefixes() {
        assert!(validate_network(&net("socks5://127.0.0.1:1080", "")).is_err());
        assert!(validate_network(&net("", "127.0.0.1:7890")).is_err());
        // no_proxy 不参与校验，任意值不影响结果
        let mut bad = net("ftp://x", "");
        bad.no_proxy = "随便什么都行".to_string();
        assert!(validate_network(&bad).is_err());
    }
}
