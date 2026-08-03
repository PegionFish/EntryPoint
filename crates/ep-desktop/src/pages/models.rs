//! 模型管理页 — 按模块分组展示模型状态，支持下载 / 删除 / 本地导入。

use eframe::egui;
use ep_core::model::{ModelStatus, ModelView};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::AppCmd;
use crate::ui::{
    badge, card, card_grid, confirm_dialog, danger_button, empty_state, page_header,
    primary_button, responsive_columns, subtle_button, Palette,
};

// ─── 主入口 ──────────────────────────────────────────────────────────────────

pub fn show(
    ui: &mut egui::Ui,
    models: &[ModelView],
    cache_dir: &str,
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    let pal = Palette::new(ui.style().visuals.dark_mode);

    // ── 页头：标题 + 右侧刷新操作 ──
    page_header(ui, "模型管理", |ui| {
        if ui.add(subtle_button(&pal, "🔄 刷新")).clicked() {
            let _ = cmd_tx.send(AppCmd::RefreshModels);
        }
    });
    ui.add_space(8.0);

    egui::ScrollArea::vertical().show(ui, |ui| {
        // ── 缓存目录卡片 ──
        card(ui, &pal, |ui| {
            ui.horizontal(|ui| {
                ui.label("📂 缓存目录");
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(cache_dir).monospace().color(pal.text_dim),
                    )
                    .selectable(true),
                )
                .on_hover_text("可选中复制路径");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(subtle_button(&pal, "打开目录")).clicked() {
                        open_dir(cache_dir);
                    }
                });
            });
        });

        ui.add_space(12.0);

        // ── 按模块分组显示模型 ──
        if models.is_empty() {
            empty_state(
                ui,
                &pal,
                "📦",
                "未发现模型配置",
                "请检查 modules/ 目录中的 module.toml",
            );
        } else {
            let groups = group_by_module(models);
            for (module_id, module_name, module_models) in &groups {
                module_section(ui, &pal, module_id, module_name, module_models, cmd_tx);
                ui.add_space(8.0);
            }
        }

        ui.add_space(8.0);
    });
}

// ─── 模块分组区块 ────────────────────────────────────────────────────────────

fn module_section(
    ui: &mut egui::Ui,
    pal: &Palette,
    module_id: &str,
    module_name: &str,
    models: &[&ModelView],
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    let total = models.len();
    let ready = models
        .iter()
        .filter(|m| m.status == ModelStatus::Ready)
        .count();
    let header_color = if ready == total { pal.success } else { pal.warning };

    // 自定义 header：默认折叠箭头 + 模块名 + "N/M 就绪" 徽章；body 不带缩进，占满宽度
    let id = ui.make_persistent_id(("models_group", module_id));
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true)
        .show_header(ui, |ui| {
            ui.label(egui::RichText::new(module_name).strong());
            badge(ui, pal, header_color, format!("{ready}/{total} 就绪"));
        })
        .body_unindented(|ui| {
            // 响应式卡片网格：最小卡宽 330，间距 12
            ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
            let cols = responsive_columns(ui.available_width(), 330.0, 12.0);
            card_grid(ui, cols, models, |ui, mv| {
                model_card(ui, pal, mv, cmd_tx);
            });
        });
}

// ─── 单个模型卡片 ────────────────────────────────────────────────────────────

fn model_card(ui: &mut egui::Ui, pal: &Palette, mv: &ModelView, cmd_tx: &UnboundedSender<AppCmd>) {
    let confirm_key = egui::Id::new(("confirm_del", mv.target_dir.clone()));

    card(ui, pal, |ui| {
        // 占满网格单元格，保证同行卡片等宽
        ui.set_width(ui.available_width());

        // 行1：名称 + 状态徽章（右对齐）
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&mv.model_name).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (color, label) = status_meta(&mv.status, pal);
                badge(ui, pal, color, label);
            });
        });

        // 行2：来源 + 大小（右对齐）
        ui.horizontal(|ui| {
            let source_text = if mv.repo_id.is_empty() {
                mv.source.clone()
            } else {
                format!("{} · {}", mv.source, mv.repo_id)
            };
            ui.label(egui::RichText::new(format!("来源: {source_text}")).color(pal.text_dim));
            if let Some(size) = mv.size_bytes {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(format_size(size)).color(pal.text_dim));
                });
            }
        });

        // 行3：目标目录（mono，弱化）
        ui.label(
            egui::RichText::new(format!("目录: {}", mv.target_dir))
                .monospace()
                .small()
                .color(pal.text_faint),
        );

        ui.add_space(8.0);

        // 操作区（按状态）
        match mv.status {
            ModelStatus::Ready => {
                ui.horizontal(|ui| {
                    if ui.add(danger_button(pal, "🗑 删除")).clicked() {
                        ui.ctx().data_mut(|d| d.insert_temp(confirm_key, true));
                    }
                });
            }
            ModelStatus::Missing | ModelStatus::Incomplete => {
                ui.horizontal(|ui| {
                    let label = if mv.status == ModelStatus::Incomplete {
                        "⬇ 重新下载"
                    } else {
                        "⬇ 下载"
                    };
                    if ui.add(primary_button(pal, label)).clicked() {
                        let _ = cmd_tx.send(AppCmd::DownloadModel(
                            mv.module_id.clone(),
                            mv.model_id.clone(),
                        ));
                    }
                });
                ui.add_space(6.0);
                import_row(ui, pal, mv, cmd_tx);
            }
            ModelStatus::Importable => {
                import_row(ui, pal, mv, cmd_tx);
            }
        }
    });

    // ── 删除确认对话框（打开期间每帧调用） ──
    let confirming = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(confirm_key))
        .unwrap_or(false);
    if confirming {
        let message = format!(
            "将删除以下模型目录：\n{}\n\n此操作不可撤销，确定继续？",
            mv.target_dir
        );
        match confirm_dialog(ui.ctx(), pal, "models_delete_dialog", "删除模型", &message, "确认删除", true) {
            Some(true) => {
                let _ = cmd_tx.send(AppCmd::DeleteModel(mv.target_dir.clone()));
                ui.ctx().data_mut(|d| d.remove::<bool>(confirm_key));
            }
            Some(false) => {
                ui.ctx().data_mut(|d| d.remove::<bool>(confirm_key));
            }
            None => {}
        }
    }
}

// ─── 本地导入区 ──────────────────────────────────────────────────────────────

/// 每模型独立的导入路径输入（状态存 ui.data temp，key 含 model_id）
fn import_row(ui: &mut egui::Ui, pal: &Palette, mv: &ModelView, cmd_tx: &UnboundedSender<AppCmd>) {
    let key = egui::Id::new(("import_path", mv.model_id.clone()));
    let mut path: String = ui
        .ctx()
        .data(|d| d.get_temp::<String>(key))
        .unwrap_or_default();

    ui.horizontal(|ui| {
        let width = (ui.available_width() - 90.0).max(60.0);
        ui.add(
            egui::TextEdit::singleline(&mut path)
                .desired_width(width)
                .hint_text("本地模型文件夹路径"),
        );
        let trimmed = path.trim();
        let can = !trimmed.is_empty() && std::path::Path::new(trimmed).is_dir();
        if ui.add_enabled(can, subtle_button(pal, "导入")).clicked() {
            let _ = cmd_tx.send(AppCmd::ImportModel {
                module_id: mv.module_id.clone(),
                model_id: mv.model_id.clone(),
                source: std::path::PathBuf::from(trimmed),
            });
        }
    });
    ui.ctx().data_mut(|d| d.insert_temp(key, path));

    ui.label(
        egui::RichText::new("将模型文件夹路径粘贴到上方，或手动复制到缓存目录后点刷新")
            .small()
            .color(pal.text_faint),
    );
}

// ─── 辅助函数 ────────────────────────────────────────────────────────────────

/// 模型状态 → (语义色, 文案)。颜色一律取自当前主题色板，禁止硬编码 RGB。
fn status_meta(status: &ModelStatus, pal: &Palette) -> (egui::Color32, &'static str) {
    match status {
        ModelStatus::Ready => (pal.success, "就绪"),
        ModelStatus::Missing => (pal.danger, "缺失"),
        ModelStatus::Incomplete => (pal.warning, "不完整"),
        ModelStatus::Importable => (pal.info, "可导入"),
    }
}

/// 按 module_id 分组，保持原始顺序
fn group_by_module(models: &[ModelView]) -> Vec<(String, String, Vec<&ModelView>)> {
    let mut groups: Vec<(String, String, Vec<&ModelView>)> = Vec::new();
    for mv in models {
        if let Some(g) = groups.iter_mut().find(|(id, _, _)| *id == mv.module_id) {
            g.2.push(mv);
        } else {
            groups.push((mv.module_id.clone(), mv.module_name.clone(), vec![mv]));
        }
    }
    groups
}

/// 格式化文件大小: B / KB / MB / GB（1 位小数）
fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;

    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{b:.0} B")
    }
}

/// 跨平台打开目录（使用系统文件管理器）
fn open_dir(path: &str) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}
