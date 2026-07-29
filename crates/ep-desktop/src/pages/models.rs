use eframe::egui;
use ep_core::model::{ModelStatus, ModelView};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::AppCmd;

// ─── 颜色常量 ────────────────────────────────────────────────────────────────

const COLOR_READY: egui::Color32 = egui::Color32::from_rgb(80, 220, 80);
const COLOR_MISSING: egui::Color32 = egui::Color32::from_rgb(200, 80, 80);
const COLOR_INCOMPLETE: egui::Color32 = egui::Color32::from_rgb(230, 200, 60);
const COLOR_IMPORTABLE: egui::Color32 = egui::Color32::from_rgb(80, 160, 255);
const COLOR_NEUTRAL: egui::Color32 = egui::Color32::from_rgb(120, 120, 120);

// ─── 主入口 ──────────────────────────────────────────────────────────────────

pub fn show(
    ui: &mut egui::Ui,
    models: &[ModelView],
    cache_dir: &str,
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        // ── 标题栏 ──
        ui.horizontal(|ui| {
            ui.heading("模型管理");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // TODO(Phase4): 刷新按钮触发后台重新扫描模型状态
                if ui.button("🔄 刷新").clicked() {
                    // orchestrator 在 Phase 4 接入刷新逻辑
                }
            });
        });

        ui.add_space(8.0);

        // ── 缓存目录 ──
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("📂 模型缓存目录:");
                ui.label(egui::RichText::new(cache_dir).monospace());
                ui.add_space(8.0);
                if ui.button("📁 打开目录").clicked() {
                    open_dir(cache_dir);
                }
            });
        });

        ui.add_space(12.0);

        // ── 按模块分组显示模型 ──
        if models.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("未发现模型配置")
                        .color(COLOR_NEUTRAL),
                );
                ui.label(
                    egui::RichText::new("请检查 modules/ 目录中的 module.toml")
                        .small()
                        .color(egui::Color32::from_gray(110)),
                );
            });
        } else {
            let groups = group_by_module(models);
            for (module_id, module_name, module_models) in &groups {
                module_section(ui, module_id, module_name, module_models, cmd_tx);
                ui.add_space(8.0);
            }
        }

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(4.0);

        // ── 提示 ──
        ui.label(
            egui::RichText::new(
                "💡 手动复制: 将模型文件夹放入上述缓存目录，点击刷新即可识别。文件夹名需与 module.toml 中 target_dir 一致。",
            )
            .small()
            .color(COLOR_NEUTRAL),
        );
    });
}

// ─── 模块分组区块 ────────────────────────────────────────────────────────────

fn module_section(
    ui: &mut egui::Ui,
    module_id: &str,
    module_name: &str,
    models: &[&ModelView],
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    let ready_count = models
        .iter()
        .filter(|m| m.status == ModelStatus::Ready)
        .count();
    let total = models.len();

    let header = if ready_count == total {
        format!("🟢 {module_name}  ({ready_count}/{total} 就绪)")
    } else {
        format!("📦 {module_name}  ({ready_count}/{total} 就绪)")
    };

    egui::CollapsingHeader::new(header)
        .id_salt(format!("models_{module_id}"))
        .default_open(true)
        .show(ui, |ui| {
            for mv in models {
                model_card(ui, mv, cmd_tx);
                ui.add_space(4.0);
            }
        });
}

// ─── 单个模型卡片 ────────────────────────────────────────────────────────────

fn model_card(ui: &mut egui::Ui, mv: &ModelView, cmd_tx: &UnboundedSender<AppCmd>) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        // ── 模型名称 + 状态 + 大小 ──
        ui.horizontal(|ui| {
            let (dot_color, status_label) = status_display(&mv.status);
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 4.0, dot_color);
            ui.add_space(4.0);
            ui.strong(&mv.model_name);
            ui.add_space(8.0);
            ui.colored_label(dot_color, status_label);

            if let Some(size) = mv.size_bytes {
                ui.add_space(8.0);
                ui.label(format!("大小: {}", format_size(size)));
            }
        });

        ui.add_space(2.0);

        // ── 来源信息 ──
        let source_text = if mv.repo_id.is_empty() {
            format!("来源: {}", mv.source)
        } else {
            format!("来源: {} / {}", mv.source, mv.repo_id)
        };
        ui.label(
            egui::RichText::new(source_text)
                .color(egui::Color32::from_gray(160)),
        );

        // ── 目标目录 ──
        ui.label(
            egui::RichText::new(format!("目录: {}", mv.target_dir))
                .small()
                .color(egui::Color32::from_gray(130)),
        );

        ui.add_space(4.0);

        // ── 操作按钮 ──
        ui.horizontal_wrapped(|ui| {
            match mv.status {
                ModelStatus::Ready => {
                    if ui
                        .add(
                            egui::Button::new("🗑 删除")
                                .fill(egui::Color32::from_rgb(140, 50, 50)),
                        )
                        .clicked()
                    {
                        // TODO(Phase4): 需要 AppCmd::DeleteModel(String) — target_dir
                        // let _ = cmd_tx.send(AppCmd::DeleteModel(mv.target_dir.clone()));
                        let _ = cmd_tx;
                    }
                }
                ModelStatus::Missing => {
                    if ui
                        .add(
                            egui::Button::new("⬇ 下载")
                                .fill(egui::Color32::from_rgb(50, 120, 200)),
                        )
                        .clicked()
                    {
                        // TODO(Phase4): 需要 AppCmd::DownloadModel(String, String) — (module_id, model_id)
                        // let _ = cmd_tx.send(AppCmd::DownloadModel(
                        //     mv.module_id.clone(),
                        //     mv.model_id.clone(),
                        // ));
                        let _ = cmd_tx;
                    }

                    if ui.button("📁 导入本地模型").clicked() {
                        // TODO(Phase4): 使用 rfd::FileDialog::pick_folder() 选择本地模型目录
                        // 需要 AppCmd::ImportModel(String, PathBuf) — (target_dir, source_path)
                        // 需要在 Cargo.toml 添加 rfd = "0.15" 依赖
                        //
                        // 示例实现:
                        // if let Some(folder) = rfd::FileDialog::new()
                        //     .set_title("选择模型文件夹")
                        //     .pick_folder()
                        // {
                        //     let _ = cmd_tx.send(AppCmd::ImportModel(
                        //         mv.target_dir.clone(),
                        //         folder,
                        //     ));
                        // }
                        let _ = cmd_tx;
                    }
                }
                ModelStatus::Incomplete => {
                    if ui
                        .add(
                            egui::Button::new("⬇ 重新下载")
                                .fill(egui::Color32::from_rgb(180, 140, 40)),
                        )
                        .clicked()
                    {
                        // TODO(Phase4): 需要 AppCmd::DownloadModel(String, String)
                        // let _ = cmd_tx.send(AppCmd::DownloadModel(
                        //     mv.module_id.clone(),
                        //     mv.model_id.clone(),
                        // ));
                        let _ = cmd_tx;
                    }

                    if ui
                        .add(
                            egui::Button::new("🗑 删除")
                                .fill(egui::Color32::from_rgb(140, 50, 50)),
                        )
                        .clicked()
                    {
                        // TODO(Phase4): 需要 AppCmd::DeleteModel(String)
                        // let _ = cmd_tx.send(AppCmd::DeleteModel(mv.target_dir.clone()));
                        let _ = cmd_tx;
                    }
                }
                ModelStatus::Importable => {
                    if ui
                        .add(
                            egui::Button::new("📥 导入")
                                .fill(egui::Color32::from_rgb(50, 130, 200)),
                        )
                        .clicked()
                    {
                        // TODO(Phase4): 需要 AppCmd::ImportModel(String, PathBuf)
                        // Importable 状态表示在 cache_paths 中找到了可导入的模型
                        // orchestrator 需提供具体的 source_path
                        let _ = cmd_tx;
                    }
                }
            }
        });
    });
}

// ─── 辅助函数 ────────────────────────────────────────────────────────────────

/// 按 module_id 分组，保持原始顺序
fn group_by_module(models: &[ModelView]) -> Vec<(String, String, Vec<&ModelView>)> {
    let mut groups: Vec<(String, String, Vec<&ModelView>)> = Vec::new();
    for mv in models {
        if let Some(g) = groups.iter_mut().find(|(id, _, _)| *id == mv.module_id) {
            g.2.push(mv);
        } else {
            groups.push((
                mv.module_id.clone(),
                mv.module_name.clone(),
                vec![mv],
            ));
        }
    }
    groups
}

/// 状态 → (颜色, 显示文本)
fn status_display(s: &ModelStatus) -> (egui::Color32, String) {
    match s {
        ModelStatus::Ready => (COLOR_READY, "🟢 就绪".into()),
        ModelStatus::Missing => (COLOR_MISSING, "🔴 缺失".into()),
        ModelStatus::Incomplete => (COLOR_INCOMPLETE, "🟡 不完整".into()),
        ModelStatus::Importable => (COLOR_IMPORTABLE, "🔵 可导入".into()),
    }
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
///
/// TODO(Phase4): 考虑替换为 `open::that(path)`（需添加 open = "5" 依赖），
/// 可自动处理各平台差异。当前使用 std::process::Command 避免额外依赖。
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
