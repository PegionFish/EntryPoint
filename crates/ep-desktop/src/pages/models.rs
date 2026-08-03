//! 模型管理页 — 按模块分组展示模型状态，支持下载（进度/取消/多源）/ 更新检查 / 删除 / 本地导入。

use std::collections::HashMap;

use eframe::egui;
use ep_core::config::AppConfig;
use ep_core::model::{DownloadState, ModelStatus, ModelView, UpdateCheckResult};
use ep_core::module::ModelSource;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{AppCmd, DownloadUiState};
use crate::i18n::tr;
use crate::ui::{
    badge, card, card_grid, confirm_dialog_with_lang, danger_button, empty_state, page_header,
    primary_button, responsive_columns, subtle_button, Palette,
};

// ─── 主入口 ──────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    config: &AppConfig,
    models: &[ModelView],
    cache_dir: &str,
    downloads: &HashMap<String, DownloadUiState>,
    updates: &HashMap<String, UpdateCheckResult>,
    download_sources: &mut HashMap<String, Option<ModelSource>>,
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    let lang = ep_core::i18n::normalize_language(&config.general.language);
    let pal = Palette::new(ui.style().visuals.dark_mode);

    // ── 页头：标题 + 右侧操作（right_to_left：先添加的在最右）──
    page_header(ui, &tr(lang, "desktopPages.models.title", &[]), |ui| {
        if ui
            .add(subtle_button(
                &pal,
                format!("🔄 {}", tr(lang, "common.action.refresh", &[])),
            ))
            .clicked()
        {
            let _ = cmd_tx.send(AppCmd::RefreshModels);
        }
        if ui
            .add(subtle_button(
                &pal,
                format!(
                    "🔍 {}",
                    tr(lang, "desktopPages.models.checkAllUpdates", &[])
                ),
            ))
            .on_hover_text(tr(lang, "desktopPages.models.checkAllUpdatesTip", &[]))
            .clicked()
        {
            let _ = cmd_tx.send(AppCmd::CheckAllUpdates);
        }
    });
    ui.add_space(8.0);

    egui::ScrollArea::vertical().show(ui, |ui| {
        // ── 缓存目录卡片 ──
        card(ui, &pal, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "📂 {}",
                    tr(lang, "desktopPages.models.cacheDir", &[])
                ));
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(cache_dir).monospace().color(pal.text_dim),
                    )
                    .selectable(true),
                )
                .on_hover_text(tr(lang, "desktopPages.models.cacheDirTip", &[]));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(subtle_button(
                            &pal,
                            tr(lang, "desktopPages.models.openDir", &[]),
                        ))
                        .clicked()
                    {
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
                &tr(lang, "desktopPages.models.empty.title", &[]),
                &tr(lang, "desktopPages.models.empty.hint", &[]),
            );
        } else {
            let groups = group_by_module(models);
            for (module_id, module_name, module_models) in &groups {
                module_section(
                    ui,
                    lang,
                    &pal,
                    module_id,
                    module_name,
                    module_models,
                    downloads,
                    updates,
                    download_sources,
                    cmd_tx,
                );
                ui.add_space(8.0);
            }
        }

        ui.add_space(8.0);
    });
}

// ─── 模块分组区块 ────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn module_section(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    module_id: &str,
    module_name: &str,
    models: &[&ModelView],
    downloads: &HashMap<String, DownloadUiState>,
    updates: &HashMap<String, UpdateCheckResult>,
    download_sources: &mut HashMap<String, Option<ModelSource>>,
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
            let ready_s = ready.to_string();
            let total_s = total.to_string();
            badge(
                ui,
                pal,
                header_color,
                tr(
                    lang,
                    "desktopPages.models.groupReady",
                    &[("ready", &ready_s), ("total", &total_s)],
                ),
            );
        })
        .body_unindented(|ui| {
            // 响应式卡片网格：最小卡宽 330，间距 12
            ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
            let cols = responsive_columns(ui.available_width(), 330.0, 12.0);
            card_grid(ui, cols, models, |ui, mv| {
                model_card(ui, lang, pal, mv, downloads, updates, download_sources, cmd_tx);
            });
        });
}

// ─── 单个模型卡片 ────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn model_card(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    mv: &ModelView,
    downloads: &HashMap<String, DownloadUiState>,
    updates: &HashMap<String, UpdateCheckResult>,
    download_sources: &mut HashMap<String, Option<ModelSource>>,
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    let confirm_key = egui::Id::new(("confirm_del", mv.target_dir.clone()));
    let downloading = downloads.get(&mv.model_id);
    let has_update = updates
        .get(&mv.model_id)
        .map(|u| u.available)
        .unwrap_or(false);

    card(ui, pal, |ui| {
        // 占满网格单元格，保证同行卡片等宽
        ui.set_width(ui.available_width());

        // 行1：名称 + "有更新"徽章 + 状态徽章（右对齐）
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&mv.model_name).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (color, label) = status_meta(lang, &mv.status, pal);
                badge(ui, pal, color, label);
                if has_update {
                    badge(
                        ui,
                        pal,
                        pal.info,
                        tr(lang, "desktopPages.models.updateAvailable", &[]),
                    );
                }
            });
        });

        // 行2：来源 + 大小（右对齐）
        ui.horizontal(|ui| {
            let source_text = if mv.repo_id.is_empty() {
                mv.source.clone()
            } else {
                format!("{} · {}", mv.source, mv.repo_id)
            };
            ui.label(
                egui::RichText::new(tr(
                    lang,
                    "desktopPages.models.source",
                    &[("source", &source_text)],
                ))
                .color(pal.text_dim),
            );
            if let Some(size) = mv.size_bytes {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(format_size(size)).color(pal.text_dim));
                });
            }
        });

        // 行3：目标目录（mono，弱化）
        ui.label(
            egui::RichText::new(tr(
                lang,
                "desktopPages.models.dir",
                &[("dir", mv.target_dir.as_str())],
            ))
            .monospace()
            .small()
            .color(pal.text_faint),
        );

        ui.add_space(8.0);

        // 操作区：下载进行中 → 进度条；否则按状态渲染
        if let Some(dl) = downloading {
            download_progress_ui(ui, lang, pal, mv, dl, cmd_tx);
        } else {
            match mv.status {
                ModelStatus::Ready => {
                    ui.horizontal(|ui| {
                        // 检查更新（小按钮）
                        if ui
                            .add(subtle_button(
                                pal,
                                format!(
                                    "🔍 {}",
                                    tr(lang, "desktopPages.models.checkUpdate", &[])
                                ),
                            ))
                            .clicked()
                        {
                            let _ = cmd_tx.send(AppCmd::CheckUpdate {
                                module_id: mv.module_id.clone(),
                                model_id: mv.model_id.clone(),
                            });
                        }
                        // 有更新 → 重新下载（复用原 source）
                        if has_update
                            && ui
                                .add(primary_button(
                                    pal,
                                    format!(
                                        "⬇ {}",
                                        tr(lang, "desktopPages.models.redownload", &[])
                                    ),
                                ))
                                .clicked()
                        {
                            let source = download_sources
                                .get(&mv.model_id)
                                .copied()
                                .unwrap_or(None);
                            send_download(cmd_tx, download_sources, mv, source);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(danger_button(
                                    pal,
                                    format!("🗑 {}", tr(lang, "common.action.delete", &[])),
                                ))
                                .clicked()
                            {
                                ui.ctx().data_mut(|d| d.insert_temp(confirm_key, true));
                            }
                        });
                    });
                }
                ModelStatus::Missing | ModelStatus::Incomplete => {
                    download_action(ui, lang, pal, mv, download_sources, cmd_tx);
                    ui.add_space(6.0);
                    import_row(ui, lang, pal, mv, cmd_tx);
                }
                ModelStatus::Importable => {
                    import_row(ui, lang, pal, mv, cmd_tx);
                }
            }
        }
    });

    // ── 删除确认对话框（打开期间每帧调用） ──
    let confirming = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(confirm_key))
        .unwrap_or(false);
    if confirming {
        let title = tr(lang, "desktopPages.models.delete.title", &[]);
        let message = tr(
            lang,
            "desktopPages.models.delete.message",
            &[("dir", mv.target_dir.as_str())],
        );
        let confirm = tr(lang, "desktopPages.models.delete.confirm", &[]);
        match confirm_dialog_with_lang(
            ui.ctx(),
            pal,
            "models_delete_dialog",
            &title,
            &message,
            &confirm,
            true,
            lang,
        ) {
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

/// 每模型独立的导入路径输入（状态存 ui.data temp，key 含 model_id）。
/// 保留手输路径框，并提供 rfd 文件夹选择器（"浏览…"）作为备选入口。
fn import_row(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    mv: &ModelView,
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    let key = egui::Id::new(("import_path", mv.model_id.clone()));
    let mut path: String = ui
        .ctx()
        .data(|d| d.get_temp::<String>(key))
        .unwrap_or_default();

    let browse_label = tr(lang, "desktopPages.models.browse", &[]);

    ui.horizontal(|ui| {
        // 文件夹选择器：rfd 原生对话框，同步阻塞调用（对话框期间 UI 暂停属预期）
        if ui
            .add(subtle_button(pal, format!("📁 {browse_label}")))
            .on_hover_text(tr(lang, "desktopPages.models.browseTip", &[]))
            .clicked()
        {
            if let Some(dir) = rfd::FileDialog::new()
                .set_title(tr(lang, "desktopPages.models.pickFolderTitle", &[]))
                .pick_folder()
            {
                path = dir.to_string_lossy().to_string();
            }
        }
        // 手输路径框（保留）+ 右侧导入按钮
        let width = (ui.available_width() - 70.0).max(60.0);
        ui.add(
            egui::TextEdit::singleline(&mut path)
                .desired_width(width)
                .hint_text(tr(lang, "desktopPages.models.importHint", &[])),
        );
        let trimmed = path.trim();
        let can = !trimmed.is_empty() && std::path::Path::new(trimmed).is_dir();
        if ui
            .add_enabled(can, subtle_button(pal, tr(lang, "common.action.import", &[])))
            .clicked()
        {
            let _ = cmd_tx.send(AppCmd::ImportModel {
                module_id: mv.module_id.clone(),
                model_id: mv.model_id.clone(),
                source: std::path::PathBuf::from(trimmed),
            });
        }
    });
    ui.ctx().data_mut(|d| d.insert_temp(key, path));

    ui.label(
        egui::RichText::new(tr(
            lang,
            "desktopPages.models.importHelp",
            &[("browse", &browse_label)],
        ))
        .small()
        .color(pal.text_faint),
    );
}

// ─── 下载相关组件 ────────────────────────────────────────────────────────────

/// 下载进行中（或刚结束）的进度展示：状态行 + 进度条（百分比/已下载大小）+ 取消按钮。
fn download_progress_ui(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    mv: &ModelView,
    dl: &DownloadUiState,
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    ui.horizontal(|ui| {
        let status = match &dl.state {
            DownloadState::Downloading => egui::RichText::new(format!(
                "⬇ {}",
                tr(lang, "desktopPages.models.downloading", &[])
            ))
            .color(pal.info),
            DownloadState::Completed => egui::RichText::new(format!(
                "✅ {}",
                tr(lang, "desktopPages.models.downloadDone", &[])
            ))
            .color(pal.success),
            DownloadState::Failed(_) => {
                egui::RichText::new(tr(lang, "desktopPages.models.downloadFailed", &[]))
                    .color(pal.danger)
            }
            DownloadState::Cancelled => {
                egui::RichText::new(tr(lang, "common.status.cancelled", &[])).color(pal.warning)
            }
        };
        ui.label(status);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // 仅下载进行中可取消
            if matches!(dl.state, DownloadState::Downloading)
                && ui
                    .add(subtle_button(pal, tr(lang, "common.action.cancel", &[])))
                    .clicked()
            {
                let _ = cmd_tx.send(AppCmd::CancelDownload(mv.model_id.clone()));
            }
        });
    });

    let fraction = (dl.percent / 100.0).clamp(0.0, 1.0);
    let size = format_size(dl.bytes);
    let label = if dl.percent > 0.0 {
        let percent = format!("{:.0}", dl.percent);
        tr(
            lang,
            "desktopPages.models.progress.percent",
            &[("percent", &percent), ("size", &size)],
        )
    } else {
        tr(lang, "desktopPages.models.progress.bytes", &[("size", &size)])
    };
    ui.add(
        egui::ProgressBar::new(fraction)
            .text(label)
            .desired_width(f32::INFINITY),
    );
}

/// 下载入口（Missing / Incomplete）：单源直接下载，多源先弹出行内来源选择。
fn download_action(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    mv: &ModelView,
    download_sources: &mut HashMap<String, Option<ModelSource>>,
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    let open_key = egui::Id::new(("src_open", mv.model_id.clone()));
    let sel_key = egui::Id::new(("src_sel", mv.model_id.clone()));
    let mut open = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(open_key))
        .unwrap_or(false);
    let multi = mv.available_sources.len() > 1;

    if !open {
        let label = if mv.status == ModelStatus::Incomplete {
            format!("⬇ {}", tr(lang, "desktopPages.models.redownload", &[]))
        } else {
            format!("⬇ {}", tr(lang, "common.action.download", &[]))
        };
        let btn = primary_button(pal, label);
        let resp = if multi {
            ui.add(btn)
                .on_hover_text(tr(lang, "desktopPages.models.multiSourceTip", &[]))
        } else {
            ui.add(btn)
        };
        if resp.clicked() {
            if multi {
                // 默认选中主源（available_sources 首位）
                let primary = mv
                    .available_sources
                    .first()
                    .copied()
                    .unwrap_or(ModelSource::Huggingface);
                ui.ctx().data_mut(|d| d.insert_temp(sel_key, primary));
                open = true;
            } else {
                send_download(cmd_tx, download_sources, mv, None);
            }
        }
    } else {
        // 行内来源单选
        let mut selected: ModelSource = ui
            .ctx()
            .data(|d| d.get_temp::<ModelSource>(sel_key))
            .unwrap_or_else(|| {
                mv.available_sources
                    .first()
                    .copied()
                    .unwrap_or(ModelSource::Huggingface)
            });
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(tr(lang, "desktopPages.models.sourceSelect", &[]))
                    .color(pal.text_dim),
            );
            for src in &mv.available_sources {
                if ui
                    .selectable_label(selected == *src, source_label(src))
                    .clicked()
                {
                    selected = *src;
                }
            }
        });
        ui.ctx().data_mut(|d| d.insert_temp(sel_key, selected));
        ui.horizontal(|ui| {
            if ui
                .add(primary_button(
                    pal,
                    tr(lang, "desktopPages.models.confirmDownload", &[]),
                ))
                .clicked()
            {
                send_download(cmd_tx, download_sources, mv, Some(selected));
                open = false;
            }
            if ui
                .add(subtle_button(pal, tr(lang, "common.action.cancel", &[])))
                .clicked()
            {
                open = false;
            }
        });
    }

    ui.ctx().data_mut(|d| d.insert_temp(open_key, open));
}

/// 记录所选来源并发送下载命令（"重新下载"据此复用原 source）。
fn send_download(
    cmd_tx: &UnboundedSender<AppCmd>,
    download_sources: &mut HashMap<String, Option<ModelSource>>,
    mv: &ModelView,
    source: Option<ModelSource>,
) {
    download_sources.insert(mv.model_id.clone(), source);
    let _ = cmd_tx.send(AppCmd::DownloadModel {
        module_id: mv.module_id.clone(),
        model_id: mv.model_id.clone(),
        source,
    });
}

/// 下载源显示名称（品牌名保留原文，不翻译）
fn source_label(src: &ModelSource) -> &'static str {
    match src {
        ModelSource::Huggingface => "HuggingFace",
        ModelSource::Modelscope => "ModelScope",
        ModelSource::Url => "URL",
    }
}

// ─── 辅助函数 ────────────────────────────────────────────────────────────────

/// 模型状态 → (语义色, 本地化文案)。颜色一律取自当前主题色板，禁止硬编码 RGB。
fn status_meta(lang: &str, status: &ModelStatus, pal: &Palette) -> (egui::Color32, String) {
    match status {
        ModelStatus::Ready => (pal.success, tr(lang, "common.status.ready", &[])),
        ModelStatus::Missing => (pal.danger, tr(lang, "common.status.missing", &[])),
        ModelStatus::Incomplete => (pal.warning, tr(lang, "common.status.incomplete", &[])),
        ModelStatus::Importable => (pal.info, tr(lang, "desktopPages.models.importable", &[])),
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
