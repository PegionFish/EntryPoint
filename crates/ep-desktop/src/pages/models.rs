//! 统一页（模型管理）— §5.1 桌面端镜像。
//!
//! 卡片 = (模块, 激活模型) 投影：模型名 / qualified_id / 模块运行状态 /
//! 模型就绪状态 / 变体 / tag chips / VRAM 估算；卡内操作：启动·停止·日志·
//! **运行（直跑抽屉）**·配置（变体切换）·删除。tag chips 筛选 + 卡内编辑。
//! 无模型声明的模块（native 等）渲染为服务卡兜底。
//!
//! 数据来源：模型列表/下载状态经 app.rs 传入（S2 骨架签名）；模块清单
//! （capabilities/变体/VRAM）经 [`crate::pages::module_data`] 页面层缓存；
//! 模块运行状态/日志经 [`crate::pages::module_snapshot`] 快照桥（dashboard/
//! modules 页发布）。tags/qualified_id 读自模型 meta（`.ep_meta.json`）。
//! 所有耗时操作（下载/启动/直跑/删除）走 [`crate::app::AppCmd`] 后台通道。

use std::collections::HashMap;

use eframe::egui;
use ep_core::config::AppConfig;
use ep_core::model::{DownloadState, ModelStatus, ModelView, UpdateCheckResult};
use ep_core::module::{ModelSource, ModuleManifest, RuntimeType};
use ep_core::types::ServiceStatus;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{AppCmd, DownloadUiState};
use crate::i18n::tr;
use crate::pages::{
    draft_default, format_size, module_data, module_snapshot, open_path, read_model_meta, trfb,
    write_model_tags, ParamDraft,
};
use crate::ui::{
    badge, card, card_grid, confirm_dialog_with_lang, danger_button, empty_state, page_header,
    primary_button, responsive_columns, subtle_button, Palette,
};

// ─── 页面持久状态 ────────────────────────────────────────────────────────────

/// 模型 meta 展示缓存（tags + qualified_id，随整合包流转的 §4.3 字段）
#[derive(Debug, Clone, Default)]
struct MetaLite {
    tags: Vec<String>,
    qualified_id: Option<String>,
}

/// 卡片抽屉种类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrawerKind {
    Run,
    Config,
    Logs,
    Tags,
}

#[derive(Debug, Clone, Default)]
struct ModelsUi {
    /// tag chips 筛选（多选 AND 语义：卡片须包含全部选中 tag）
    tag_filter: Vec<String>,
    /// 当前展开的抽屉 (种类, model_id)；同一时间仅一个
    drawer: Option<(DrawerKind, String)>,
    /// 直跑表单：module_id → 选中 capability
    run_cap: HashMap<String, String>,
    /// 直跑表单：module_id → 输入文件路径
    run_input: HashMap<String, String>,
    /// 直跑表单：module_id → 参数草稿（顺序 = schema 顺序）
    run_params: HashMap<String, Vec<(String, ParamDraft)>>,
    /// 变体切换：module_id → 待应用变体 id
    cfg_selected: HashMap<String, String>,
    /// 变体切换已落盘记录：module_id → 变体 id。
    /// 落盘后 UI 侧 config 副本未重载（后台副本亦然），用本覆盖保证
    /// 激活徽章/单选态即时正确；正式生效链路见 config_drawer 说明。
    cfg_applied: HashMap<String, String>,
    /// tag 编辑输入框
    tag_input: String,
    /// meta 缓存：target_dir → MetaLite（"加载一次 + 刷新按钮重载"）
    meta_cache: HashMap<String, MetaLite>,
    meta_loaded: bool,
    /// 行内提示（变体已保存 / tag 已更新 / 直跑已提交等）
    note: Option<String>,
}

fn page_state_id() -> egui::Id {
    egui::Id::new("models_unified_ui_state")
}

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
    let mut st = ui
        .ctx()
        .data(|d| d.get_temp::<ModelsUi>(page_state_id()))
        .unwrap_or_default();

    // 模块清单缓存 + 模块运行状态快照（快照桥，见 pages/mod.rs 文档）
    let data = module_data(ui.ctx(), false);
    let snapshot = module_snapshot(ui.ctx());

    // meta 缓存首次加载（每模型一个小 JSON，一次性）
    if !st.meta_loaded {
        load_meta_cache(config, models, &mut st);
    }

    // ── 页头：标题 + 右侧操作 ──
    page_header(ui, &tr(lang, "desktopPages.models.title", &[]), |ui| {
        if ui
            .add(subtle_button(
                &pal,
                format!("🔄 {}", tr(lang, "common.action.refresh", &[])),
            ))
            .clicked()
        {
            let _ = cmd_tx.send(AppCmd::RefreshModels);
            // 强制重读清单缓存 + meta 缓存
            module_data(ui.ctx(), true);
            st.meta_loaded = false;
            st.note = None;
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
                        let root = ep_core::config::resolve_root();
                        let p = std::path::Path::new(cache_dir);
                        let abs = if p.is_absolute() {
                            p.to_path_buf()
                        } else {
                            root.join(p)
                        };
                        open_path(&abs);
                    }
                });
            });
        });

        ui.add_space(12.0);

        // ── tag chips 筛选行 ──
        tag_filter_row(ui, lang, &pal, &mut st);

        // ── 行内提示（变体/tag/直跑等操作的即时反馈） ──
        if let Some(note) = st.note.clone() {
            ui.label(egui::RichText::new(note).small().color(pal.info));
            ui.add_space(4.0);
        }

        // ── 按模块分组显示模型卡片 ──
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
                // tag 筛选：组内保留至少一张卡才渲染该组
                let visible: Vec<&ModelView> = module_models
                    .iter()
                    .filter(|mv| matches_tag_filter(&st, mv))
                    .copied()
                    .collect();
                if visible.is_empty() {
                    continue;
                }
                module_section(
                    ui,
                    lang,
                    &pal,
                    config,
                    module_id,
                    module_name,
                    &visible,
                    data.manifest(module_id),
                    snapshot.as_ref(),
                    downloads,
                    updates,
                    download_sources,
                    cmd_tx,
                    &mut st,
                );
                ui.add_space(8.0);
            }
        }

        // ── 服务卡兜底：无模型声明的模块（native 模块等，§5.1） ──
        service_cards(
            ui,
            lang,
            &pal,
            models,
            &data,
            snapshot.as_ref(),
            cmd_tx,
            &mut st,
        );

        ui.add_space(8.0);
    });

    ui.ctx()
        .data_mut(|d| *d.get_temp_mut_or_default::<ModelsUi>(page_state_id()) = st);
}

// ─── meta 缓存 ───────────────────────────────────────────────────────────────

fn load_meta_cache(config: &AppConfig, models: &[ModelView], st: &mut ModelsUi) {
    st.meta_cache.clear();
    for mv in models {
        if let Some(meta) = read_model_meta(config, &mv.target_dir) {
            st.meta_cache.insert(
                mv.target_dir.clone(),
                MetaLite {
                    tags: meta.tags,
                    qualified_id: meta.qualified_id,
                },
            );
        }
    }
    st.meta_loaded = true;
}

/// tag 筛选匹配：选中 tag 必须全部出现在卡片 tag 集合中（AND）
fn matches_tag_filter(st: &ModelsUi, mv: &ModelView) -> bool {
    if st.tag_filter.is_empty() {
        return true;
    }
    let Some(meta) = st.meta_cache.get(&mv.target_dir) else {
        return false;
    };
    st.tag_filter.iter().all(|t| meta.tags.contains(t))
}

// ─── tag 筛选行 ──────────────────────────────────────────────────────────────

fn tag_filter_row(ui: &mut egui::Ui, lang: &str, pal: &Palette, st: &mut ModelsUi) {
    // 汇总全部 tag（缓存顺序不稳定 → 排序保证 UI 稳定）
    let mut all_tags: Vec<String> = st
        .meta_cache
        .values()
        .flat_map(|m| m.tags.iter().cloned())
        .collect();
    all_tags.sort();
    all_tags.dedup();
    if all_tags.is_empty() {
        return;
    }

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        ui.label(
            egui::RichText::new(trfb(lang, "desktopPages.models.filter.label", "标签筛选:", &[]))
                .color(pal.text_dim),
        );
        for tag in &all_tags {
            let active = st.tag_filter.contains(tag);
            let resp = ui.selectable_label(active, format!("🏷 {tag}"));
            if resp.clicked() {
                if active {
                    st.tag_filter.retain(|t| t != tag);
                } else {
                    st.tag_filter.push(tag.clone());
                }
            }
        }
        if !st.tag_filter.is_empty()
            && ui
                .add(subtle_button(
                    pal,
                    trfb(lang, "desktopPages.models.filter.clear", "清除", &[]),
                ))
                .clicked()
        {
            st.tag_filter.clear();
        }
    });
    ui.add_space(8.0);
}

// ─── 模块分组区块 ────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn module_section(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    config: &AppConfig,
    module_id: &str,
    module_name: &str,
    models: &[&ModelView],
    manifest: Option<&ModuleManifest>,
    snapshot: Option<&crate::pages::ModuleSnapshot>,
    downloads: &HashMap<String, DownloadUiState>,
    updates: &HashMap<String, UpdateCheckResult>,
    download_sources: &mut HashMap<String, Option<ModelSource>>,
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModelsUi,
) {
    let total = models.len();
    let ready = models
        .iter()
        .filter(|m| m.status == ModelStatus::Ready)
        .count();
    let header_color = if ready == total { pal.success } else { pal.warning };

    // 模块运行状态徽章（快照桥；缺失时不渲染）
    let module_status = snapshot.and_then(|s| {
        s.entries
            .iter()
            .find(|e| e.id == module_id)
            .map(|e| e.status.clone())
    });

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
            if let Some(status) = &module_status {
                let meta = crate::ui::service_status(status, pal);
                badge(
                    ui,
                    pal,
                    meta.color,
                    crate::pages::modules::service_label(lang, status),
                );
            }
        })
        .body_unindented(|ui| {
            // 响应式卡片网格：最小卡宽 340，间距 12
            ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
            let cols = responsive_columns(ui.available_width(), 340.0, 12.0);
            card_grid(ui, cols, models, |ui, mv| {
                model_card(
                    ui, lang, pal, config, mv, manifest, snapshot, downloads, updates,
                    download_sources, cmd_tx, st,
                );
            });
        });
}

// ─── 单个模型卡片（§5.1 统一页卡片） ────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn model_card(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    config: &AppConfig,
    mv: &ModelView,
    manifest: Option<&ModuleManifest>,
    snapshot: Option<&crate::pages::ModuleSnapshot>,
    downloads: &HashMap<String, DownloadUiState>,
    updates: &HashMap<String, UpdateCheckResult>,
    download_sources: &mut HashMap<String, Option<ModelSource>>,
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModelsUi,
) {
    let confirm_key = egui::Id::new(("confirm_del", mv.target_dir.clone()));
    let downloading = downloads.get(&mv.model_id);
    let has_update = updates
        .get(&mv.model_id)
        .map(|u| u.available)
        .unwrap_or(false);

    let decl = manifest.and_then(|mf| mf.models.iter().find(|m| m.id == mv.model_id));
    let meta = st.meta_cache.get(&mv.target_dir).cloned().unwrap_or_default();
    let active_model = manifest
        .map(|mf| effective_active_variant(config, st, &mv.module_id, mf));
    let is_active = active_model.as_deref() == Some(mv.model_id.as_str());
    let module_status = snapshot
        .and_then(|s| s.entries.iter().find(|e| e.id == mv.module_id))
        .cloned();

    card(ui, pal, |ui| {
        ui.set_width(ui.available_width());

        // 行1：名称 + [激活] + 有更新 + 就绪状态徽章（右对齐）
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&mv.model_name).strong());
            if is_active {
                badge(
                    ui,
                    pal,
                    pal.primary,
                    trfb(lang, "desktopPages.models.active", "激活", &[]),
                );
            }
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

        // 行2：qualified_id（§4.3；meta 优先，manifest 声明兜底）
        let qualified = meta
            .qualified_id
            .clone()
            .or_else(|| decl.and_then(|d| d.qualified_id.clone()));
        if let Some(qid) = &qualified {
            ui.label(
                egui::RichText::new(qid)
                    .monospace()
                    .small()
                    .color(pal.text_faint),
            );
        }

        // 行3：来源 + 变体 + VRAM 估算（右对齐）
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
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(mb) = manifest.and_then(|mf| mf.resolve_vram_estimate(&mv.model_id)) {
                    let mb_s = mb.to_string();
                    badge(
                        ui,
                        pal,
                        pal.info,
                        trfb(
                            lang,
                            "desktopPages.models.vram",
                            "VRAM ≈ {{mb}} MB",
                            &[("mb", &mb_s)],
                        ),
                    );
                }
                if let Some(size) = mv.size_bytes {
                    ui.label(egui::RichText::new(format_size(size)).color(pal.text_dim));
                }
            });
        });

        // 行4：目标目录（mono，弱化）
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

        // 行5：tag chips（点击编辑进入 Tags 抽屉）
        tag_chips_row(ui, lang, pal, &meta.tags, st, mv);

        ui.add_space(8.0);

        // ── 操作区 ──
        if let Some(dl) = downloading {
            download_progress_ui(ui, lang, pal, mv, dl, cmd_tx);
        } else {
            match mv.status {
                ModelStatus::Ready => {
                    ready_actions(
                        ui, lang, pal, mv, manifest, module_status.as_ref(), has_update,
                        download_sources, cmd_tx, st,
                    );
                }
                ModelStatus::Missing | ModelStatus::Incomplete => {
                    // 未就绪模型：下载/导入为主操作；直跑/配置仍可用（配置=变体切换）
                    download_action(ui, lang, pal, mv, download_sources, cmd_tx);
                    ui.add_space(6.0);
                    import_row(ui, lang, pal, mv, cmd_tx);
                    ui.add_space(6.0);
                    secondary_actions(ui, lang, pal, mv, manifest, module_status.as_ref(), cmd_tx, st, false);
                }
                ModelStatus::Importable => {
                    import_row(ui, lang, pal, mv, cmd_tx);
                    ui.add_space(6.0);
                    secondary_actions(ui, lang, pal, mv, manifest, module_status.as_ref(), cmd_tx, st, false);
                }
            }
        }

        // ── 抽屉内容（Run/Config/Logs/Tags） ──
        drawer_area(
            ui, lang, pal, config, mv, manifest, module_status.as_ref(), cmd_tx, st,
        );
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

// ─── 卡片操作行 ──────────────────────────────────────────────────────────────

/// Ready 模型的主操作行：直跑·配置·更新检查 + 启动/停止·日志 + 删除
#[allow(clippy::too_many_arguments)]
fn ready_actions(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    mv: &ModelView,
    manifest: Option<&ModuleManifest>,
    module_status: Option<&crate::pages::ModuleSnapEntry>,
    has_update: bool,
    download_sources: &mut HashMap<String, Option<ModelSource>>,
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModelsUi,
) {
    ui.horizontal(|ui| {
        // 运行（直跑抽屉，§5.3）
        let run_label = trfb(lang, "desktopPages.models.run", "运行", &[]);
        if ui
            .add(subtle_button(pal, format!("▶️ {run_label}")))
            .on_hover_text(trfb(
                lang,
                "desktopPages.models.runTip",
                "单模型直跑：选能力 → 填参数 → 指定输入文件",
                &[],
            ))
            .clicked()
        {
            toggle_drawer(st, DrawerKind::Run, &mv.model_id);
            ensure_run_form(st, &mv.module_id, manifest);
        }
        // 配置（变体切换）
        if ui
            .add(subtle_button(
                pal,
                format!(
                    "⚙ {}",
                    trfb(lang, "desktopPages.models.config", "配置", &[])
                ),
            ))
            .clicked()
        {
            toggle_drawer(st, DrawerKind::Config, &mv.model_id);
            // 预选当前激活变体
            if let Some(mf) = manifest {
                st.cfg_selected
                    .entry(mv.module_id.clone())
                    .or_insert_with(|| active_variant_from_manifest(mf));
            }
        }
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
                ui.ctx().data_mut(|d| d.insert_temp(confirm_key_of(mv), true));
            }
        });
    });

    // 行2：模块服务操作（启动/停止·日志）— 依赖模块运行状态快照
    ui.add_space(4.0);
    module_service_row(ui, lang, pal, mv, module_status, cmd_tx, st);
}

/// 非 Ready 模型的次要操作（配置/标签；不含直跑——直跑要求模型就绪）
#[allow(clippy::too_many_arguments)]
fn secondary_actions(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    mv: &ModelView,
    manifest: Option<&ModuleManifest>,
    module_status: Option<&crate::pages::ModuleSnapEntry>,
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModelsUi,
    include_run: bool,
) {
    ui.horizontal(|ui| {
        if include_run
            && ui
                .add(subtle_button(
                    pal,
                    format!(
                        "▶️ {}",
                        trfb(lang, "desktopPages.models.run", "运行", &[])
                    ),
                ))
                .clicked()
        {
            toggle_drawer(st, DrawerKind::Run, &mv.model_id);
            ensure_run_form(st, &mv.module_id, manifest);
        }
        if ui
            .add(subtle_button(
                pal,
                format!(
                    "⚙ {}",
                    trfb(lang, "desktopPages.models.config", "配置", &[])
                ),
            ))
            .clicked()
        {
            toggle_drawer(st, DrawerKind::Config, &mv.model_id);
            if let Some(mf) = manifest {
                st.cfg_selected
                    .entry(mv.module_id.clone())
                    .or_insert_with(|| active_variant_from_manifest(mf));
            }
        }
    });
    ui.add_space(4.0);
    module_service_row(ui, lang, pal, mv, module_status, cmd_tx, st);
}

fn confirm_key_of(mv: &ModelView) -> egui::Id {
    egui::Id::new(("confirm_del", mv.target_dir.clone()))
}

/// 启动/停止 + 日志按钮行（模块级；状态来自快照桥）
fn module_service_row(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    mv: &ModelView,
    module_status: Option<&crate::pages::ModuleSnapEntry>,
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModelsUi,
) {
    ui.horizontal(|ui| {
        let status = module_status.map(|e| e.status.clone());
        match &status {
            Some(ServiceStatus::Running) | Some(ServiceStatus::Starting) => {
                if ui
                    .add(subtle_button(
                        pal,
                        format!("⏹ {}", tr(lang, "common.action.stop", &[])),
                    ))
                    .clicked()
                {
                    let _ = cmd_tx.send(AppCmd::StopModule(mv.module_id.clone()));
                }
            }
            Some(_) => {
                if ui
                    .add(subtle_button(
                        pal,
                        format!("▶ {}", tr(lang, "common.action.start", &[])),
                    ))
                    .clicked()
                {
                    let _ = cmd_tx.send(AppCmd::StartModule(mv.module_id.clone()));
                }
            }
            None => {
                // 快照缺失：禁用并提示（理论上仪表盘启动后不会出现）
                ui.add_enabled(
                    false,
                    subtle_button(
                        pal,
                        format!("▶ {}", tr(lang, "common.action.start", &[])),
                    ),
                )
                .on_hover_text(trfb(
                    lang,
                    "desktopPages.models.snapshotMissing",
                    "模块运行状态未知（请先访问仪表盘或模块页）",
                    &[],
                ));
            }
        }
        // 日志抽屉
        if ui
            .add(subtle_button(
                pal,
                format!(
                    "📜 {}",
                    trfb(lang, "desktopPages.models.logs", "日志", &[])
                ),
            ))
            .clicked()
        {
            toggle_drawer(st, DrawerKind::Logs, &mv.model_id);
        }
        // 端口/设备信息（快照携带时展示，便于确认服务身份）
        if let Some(entry) = module_status {
            if let Some(port) = entry.port {
                ui.label(
                    egui::RichText::new(format!(":{port}"))
                        .monospace()
                        .small()
                        .color(pal.text_faint),
                );
            }
            if let Some(dev) = &entry.device {
                ui.label(
                    egui::RichText::new(dev.as_str())
                        .monospace()
                        .small()
                        .color(pal.text_faint),
                )
                .on_hover_text(format!("{} · {}", entry.name, dev));
            }
        }
    });
}

// ─── tag chips ───────────────────────────────────────────────────────────────

fn tag_chips_row(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    tags: &[String],
    st: &mut ModelsUi,
    mv: &ModelView,
) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(5.0, 4.0);
        for tag in tags {
            let resp = ui.selectable_label(false, format!("🏷 {tag}"));
            if resp.clicked() {
                toggle_drawer(st, DrawerKind::Tags, &mv.model_id);
            }
        }
        if ui
            .add(subtle_button(
                pal,
                egui::RichText::new("＋").color(pal.text_dim),
            ))
            .on_hover_text(trfb(
                lang,
                "desktopPages.models.tags.editTip",
                "编辑标签",
                &[],
            ))
            .clicked()
        {
            toggle_drawer(st, DrawerKind::Tags, &mv.model_id);
        }
    });
}

fn toggle_drawer(st: &mut ModelsUi, kind: DrawerKind, model_id: &str) {
    let next = (kind, model_id.to_string());
    if st.drawer.as_ref() == Some(&next) {
        st.drawer = None;
    } else {
        st.drawer = Some(next);
        st.tag_input.clear();
        st.note = None;
    }
}

// ─── 抽屉区域 ────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn drawer_area(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    config: &AppConfig,
    mv: &ModelView,
    manifest: Option<&ModuleManifest>,
    module_status: Option<&crate::pages::ModuleSnapEntry>,
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModelsUi,
) {
    let Some((kind, key)) = st.drawer.clone() else {
        return;
    };
    if key != mv.model_id {
        return;
    }

    ui.add_space(6.0);
    ui.separator();
    ui.add_space(6.0);

    match kind {
        DrawerKind::Run => run_drawer(ui, lang, pal, mv, manifest, cmd_tx, st),
        DrawerKind::Config => config_drawer(ui, lang, pal, config, mv, manifest, st),
        DrawerKind::Logs => logs_drawer(ui, lang, pal, module_status),
        DrawerKind::Tags => tags_drawer(ui, lang, pal, config, mv, st),
    }
}

// ─── 直跑抽屉（§5.3） ────────────────────────────────────────────────────────

/// 直跑表单初始化：capability 缺省第一个，参数草稿按 schema 默认值生成。
fn ensure_run_form(st: &mut ModelsUi, module_id: &str, manifest: Option<&ModuleManifest>) {
    let Some(mf) = manifest else { return };
    st.run_cap.entry(module_id.to_string()).or_insert_with(|| {
        mf.interface
            .capabilities
            .first()
            .map(|c| c.name.clone())
            .unwrap_or_default()
    });
    st.run_input.entry(module_id.to_string()).or_default();
    rebuild_param_drafts(st, module_id, mf);
}

/// 按当前选中 capability 的 schema 重建参数草稿（保留已有同名值）
fn rebuild_param_drafts(st: &mut ModelsUi, module_id: &str, mf: &ModuleManifest) {
    let cap_name = st.run_cap.get(module_id).cloned().unwrap_or_default();
    let cap = mf
        .interface
        .capabilities
        .iter()
        .find(|c| c.name == cap_name);
    let Some(cap) = cap else {
        st.run_params.insert(module_id.to_string(), Vec::new());
        return;
    };

    let mut schemas: Vec<(&String, &ep_core::module::ParamSchema)> = cap
        .params
        .as_ref()
        .map(|p| p.iter().collect())
        .unwrap_or_default();
    // 参数顺序稳定（HashMap 无序）
    schemas.sort_by(|a, b| a.0.cmp(b.0));

    let existing = st.run_params.remove(module_id).unwrap_or_default();
    let drafts: Vec<(String, ParamDraft)> = schemas
        .into_iter()
        .map(|(name, schema)| {
            let kept = existing.iter().find(|(n, _)| n == name).map(|(_, d)| d.clone());
            (name.clone(), kept.unwrap_or_else(|| draft_default(schema)))
        })
        .collect();
    st.run_params.insert(module_id.to_string(), drafts);
}

#[allow(clippy::too_many_arguments)]
fn run_drawer(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    mv: &ModelView,
    manifest: Option<&ModuleManifest>,
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModelsUi,
) {
    ui.label(
        egui::RichText::new(trfb(
            lang,
            "desktopPages.models.run.title",
            "单模型直跑",
            &[],
        ))
        .strong(),
    );
    ui.add_space(4.0);

    let Some(mf) = manifest else {
        ui.label(
            egui::RichText::new(trfb(
                lang,
                "desktopPages.models.run.noManifest",
                "模块清单不可用，无法直跑",
                &[],
            ))
            .small()
            .color(pal.text_faint),
        );
        return;
    };
    let caps = &mf.interface.capabilities;
    if caps.is_empty() {
        ui.label(
            egui::RichText::new(trfb(
                lang,
                "desktopPages.models.run.noCapability",
                "该模块未声明任何能力（capability）",
                &[],
            ))
            .small()
            .color(pal.text_faint),
        );
        return;
    }

    ensure_run_form(st, &mv.module_id, Some(mf));

    // capability 选择
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(trfb(
                lang,
                "desktopPages.models.run.capability",
                "能力",
                &[],
            ))
            .color(pal.text_dim),
        );
        let current = st
            .run_cap
            .get(&mv.module_id)
            .cloned()
            .unwrap_or_default();
        egui::ComboBox::from_id_salt(egui::Id::new(("run_cap", mv.module_id.clone())))
            .selected_text(if current.is_empty() { "-" } else { &current })
            .show_ui(ui, |ui| {
                for cap in caps {
                    let selected = current == cap.name;
                    if ui.selectable_label(selected, &cap.name).clicked() && !selected {
                        st.run_cap
                            .insert(mv.module_id.clone(), cap.name.clone());
                        rebuild_param_drafts(st, &mv.module_id, mf);
                    }
                }
            });
        if let Some(cap) = caps.iter().find(|c| c.name == current) {
            if !cap.description.is_empty() {
                ui.label(
                    egui::RichText::new(&cap.description)
                        .small()
                        .color(pal.text_faint),
                );
            }
        }
    });
    ui.add_space(4.0);

    // 参数表单（schema 驱动，§5.3：type/default/min/max/enum）
    let cap_name = st.run_cap.get(&mv.module_id).cloned().unwrap_or_default();
    let cap = caps.iter().find(|c| c.name == cap_name);
    if let Some(cap) = cap {
        if let Some(schema_map) = &cap.params {
            if !schema_map.is_empty() {
                let mut keys: Vec<&String> = schema_map.keys().collect();
                keys.sort();
                for key in keys {
                    let schema = &schema_map[key];
                    param_draft_row(ui, pal, &mv.module_id, key, schema, st);
                }
                ui.add_space(4.0);
            }
        }
    }

    // 输入文件
    ui.horizontal(|ui| {
        let browse_label = trfb(lang, "desktopPages.models.run.browse", "浏览…", &[]);
        if ui
            .add(subtle_button(pal, format!("📁 {browse_label}")))
            .clicked()
        {
            if let Some(file) = rfd::FileDialog::new()
                .set_title(trfb(
                    lang,
                    "desktopPages.models.run.pickFile",
                    "选择输入文件",
                    &[],
                ))
                .pick_file()
            {
                st.run_input
                    .insert(mv.module_id.clone(), file.to_string_lossy().to_string());
            }
        }
        let width = (ui.available_width() - 10.0).max(60.0);
        let input_path = st.run_input.entry(mv.module_id.clone()).or_default();
        ui.add(
            egui::TextEdit::singleline(input_path)
                .desired_width(width)
                .hint_text(trfb(
                    lang,
                    "desktopPages.models.run.inputHint",
                    "输入文件路径",
                    &[],
                )),
        );
    });
    ui.add_space(6.0);

    // 提交
    let input_path = st
        .run_input
        .get(&mv.module_id)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let input_ok = !input_path.is_empty() && std::path::Path::new(&input_path).is_file();
    let cap_ok = !cap_name.is_empty();
    ui.horizontal(|ui| {
        let submit_label = trfb(lang, "desktopPages.models.run.submit", "提交执行", &[]);
        let btn = primary_button(pal, format!("⚡ {submit_label}"));
        let resp = ui.add_enabled(input_ok && cap_ok, btn);
        let resp = if !input_ok {
            resp.on_hover_text(trfb(
                lang,
                "desktopPages.models.run.inputMissing",
                "请选择存在的输入文件",
                &[],
            ))
        } else {
            resp
        };
        if resp.clicked() {
            let params: Vec<(String, String)> = st
                .run_params
                .get(&mv.module_id)
                .map(|drafts| {
                    drafts
                        .iter()
                        .map(|(name, draft)| (name.clone(), draft.to_arg()))
                        .collect()
                })
                .unwrap_or_default();
            let _ = cmd_tx.send(AppCmd::ExecuteSingle {
                module_id: mv.module_id.clone(),
                capability: cap_name.clone(),
                params,
                input_path: std::path::PathBuf::from(&input_path),
            });
            st.note = Some(trfb(
                lang,
                "desktopPages.models.run.submitted",
                "直跑请求已提交，进度见任务页",
                &[],
            ));
            st.drawer = None;
        }
    });
}

/// 单个参数草稿行（schema 类型 → 控件）
fn param_draft_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    module_id: &str,
    key: &str,
    schema: &ep_core::module::ParamSchema,
    st: &mut ModelsUi,
) {
    let t = schema.param_type.to_ascii_lowercase();
    let enum_options = schema
        .enum_values
        .as_ref()
        .or(schema.options.as_ref())
        .cloned()
        .unwrap_or_default();

    // 从页面状态取出当前草稿（临时移出，编辑后放回）
    let mut drafts = st.run_params.remove(module_id).unwrap_or_default();
    let Some(idx) = drafts.iter().position(|(n, _)| n == key) else {
        st.run_params.insert(module_id.to_string(), drafts);
        return;
    };

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("{key}:")).color(pal.text_dim));

        if !enum_options.is_empty() {
            // 枚举：下拉选择（值按字符串存放）
            let current = match &drafts[idx].1 {
                ParamDraft::Str(s) => s.clone(),
                other => other.to_arg(),
            };
            egui::ComboBox::from_id_salt(egui::Id::new(("param_enum", module_id, key)))
                .selected_text(if current.is_empty() { "-" } else { &current })
                .show_ui(ui, |ui| {
                    for opt in &enum_options {
                        if ui
                            .selectable_label(current == *opt, opt)
                            .clicked()
                        {
                            drafts[idx].1 = ParamDraft::Str(opt.clone());
                        }
                    }
                });
        } else if t == "boolean" || t == "bool" {
            let mut value = matches!(drafts[idx].1, ParamDraft::Bool(true));
            if ui.checkbox(&mut value, "").changed() {
                drafts[idx].1 = ParamDraft::Bool(value);
            }
        } else if t == "integer" || t == "int" {
            let mut value = match &drafts[idx].1 {
                ParamDraft::Int(i) => *i,
                _ => 0,
            };
            let min = schema.min.map(|m| m as i64).unwrap_or(i64::MIN / 2);
            let max = schema.max.map(|m| m as i64).unwrap_or(i64::MAX / 2);
            let mut dv = egui::DragValue::new(&mut value).range(min..=max);
            if let Some(step) = schema.step {
                dv = dv.speed(step.max(1.0));
            }
            if ui.add(dv).changed() {
                drafts[idx].1 = ParamDraft::Int(value);
            }
        } else if t == "number" || t == "float" || t == "double" {
            let mut value = match &drafts[idx].1 {
                ParamDraft::Float(f) => *f,
                ParamDraft::Int(i) => *i as f64,
                _ => 0.0,
            };
            let min = schema.min.unwrap_or(f64::MIN / 2.0);
            let max = schema.max.unwrap_or(f64::MAX / 2.0);
            let mut dv = egui::DragValue::new(&mut value).range(min..=max);
            if let Some(step) = schema.step {
                dv = dv.speed(step);
            }
            if ui.add(dv).changed() {
                drafts[idx].1 = ParamDraft::Float(value);
            }
        } else {
            // string（含未知类型按字符串处理）
            let mut value = match &drafts[idx].1 {
                ParamDraft::Str(s) => s.clone(),
                other => other.to_arg(),
            };
            let width = ui.available_width().clamp(80.0, 220.0);
            if ui
                .add(egui::TextEdit::singleline(&mut value).desired_width(width))
                .changed()
            {
                drafts[idx].1 = ParamDraft::Str(value);
            }
        }
    });
    if let Some(desc) = &schema.description {
        if !desc.is_empty() {
            ui.label(egui::RichText::new(desc).small().color(pal.text_faint));
        }
    }

    st.run_params.insert(module_id.to_string(), drafts);
}

// ─── 配置抽屉（变体切换，§5.2 单槽位） ──────────────────────────────────────

fn config_drawer(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    config: &AppConfig,
    mv: &ModelView,
    manifest: Option<&ModuleManifest>,
    st: &mut ModelsUi,
) {
    ui.label(
        egui::RichText::new(trfb(
            lang,
            "desktopPages.models.config.title",
            "变体配置（单槽位）",
            &[],
        ))
        .strong(),
    );
    ui.add_space(4.0);

    let Some(mf) = manifest else {
        ui.label(
            egui::RichText::new(trfb(
                lang,
                "desktopPages.models.run.noManifest",
                "模块清单不可用，无法直跑",
                &[],
            ))
            .small()
            .color(pal.text_faint),
        );
        return;
    };
    if mf.models.is_empty() {
        ui.label(
            egui::RichText::new(trfb(
                lang,
                "desktopPages.models.config.noVariants",
                "该模块未声明模型变体",
                &[],
            ))
            .small()
            .color(pal.text_faint),
        );
        return;
    }

    let active = effective_active_variant(config, st, &mv.module_id, mf);
    let selected = st
        .cfg_selected
        .get(&mv.module_id)
        .cloned()
        .unwrap_or_else(|| active.clone());

    for decl in &mf.models {
        ui.horizontal(|ui| {
            let chosen = selected == decl.id;
            if ui.radio(chosen, "").clicked() && !chosen {
                st.cfg_selected
                    .insert(mv.module_id.clone(), decl.id.clone());
            }
            let mut name = egui::RichText::new(&decl.name);
            if decl.id == active {
                name = name.strong();
            }
            ui.label(name);
            ui.label(
                egui::RichText::new(format!("[{}]", decl.id))
                    .monospace()
                    .small()
                    .color(pal.text_faint),
            );
            if decl.id == active {
                badge(
                    ui,
                    pal,
                    pal.primary,
                    trfb(lang, "desktopPages.models.active", "激活", &[]),
                );
            }
            if let Some(mb) = mf.resolve_vram_estimate(&decl.id) {
                let mb_s = mb.to_string();
                ui.label(
                    egui::RichText::new(trfb(
                        lang,
                        "desktopPages.models.vram",
                        "VRAM ≈ {{mb}} MB",
                        &[("mb", &mb_s)],
                    ))
                    .small()
                    .color(pal.text_faint),
                );
            }
        });
    }

    ui.add_space(6.0);
    let changed = selected != active;
    ui.horizontal(|ui| {
        let apply_label = trfb(lang, "desktopPages.models.config.apply", "应用变体", &[]);
        if ui
            .add_enabled(changed, primary_button(pal, apply_label))
            .clicked()
        {
            // 落盘 config/app.toml [active_models]（§5.2）；
            // 注：UI 侧 config 副本与后台线程副本需重启/重载后同步，
            // AppCmd::SetActiveVariant 接线后改为经后台热更新（见 C5 报告仲裁）
            let mut new_cfg = config.clone();
            new_cfg
                .active_models
                .insert(mv.module_id.clone(), selected.clone());
            let config_dir = ep_core::config::resolve_root().join("config");
            match new_cfg.save(&config_dir) {
                Ok(()) => {
                    // 记录已落盘变体，保证本页激活徽章/单选即时正确
                    st.cfg_applied
                        .insert(mv.module_id.clone(), selected.clone());
                    st.note = Some(trfb(
                        lang,
                        "desktopPages.models.config.saved",
                        "已保存激活变体；下次启动模块生效，运行中模块需重启",
                        &[],
                    ));
                    st.drawer = None;
                }
                Err(e) => {
                    st.note = Some(trfb(
                        lang,
                        "desktopPages.models.config.saveFailed",
                        "保存失败: {{detail}}",
                        &[("detail", &e.to_string())],
                    ));
                }
            }
        }
        if !changed {
            ui.label(
                egui::RichText::new(trfb(
                    lang,
                    "desktopPages.models.config.current",
                    "当前即为激活变体",
                    &[],
                ))
                .small()
                .color(pal.text_faint),
            );
        }
    });
}

/// 激活变体解析（§5.2）：config.active_models 优先（须为清单已声明变体），
/// 回退 manifest default=true，再回退首个变体。
fn active_variant(config: &AppConfig, module_id: &str, mf: &ModuleManifest) -> String {
    if let Some(id) = config.active_models.get(module_id) {
        if mf.models.iter().any(|m| &m.id == id) {
            return id.clone();
        }
    }
    active_variant_from_manifest(mf)
}

/// 生效激活变体：本页落盘覆盖（`cfg_applied`）优先于 config 副本，
/// 解决"保存后 UI/后台 config 副本未重载导致激活徽章滞后"的问题。
fn effective_active_variant(
    config: &AppConfig,
    st: &ModelsUi,
    module_id: &str,
    mf: &ModuleManifest,
) -> String {
    if let Some(id) = st.cfg_applied.get(module_id) {
        if mf.models.iter().any(|m| &m.id == id) {
            return id.clone();
        }
    }
    active_variant(config, module_id, mf)
}

fn active_variant_from_manifest(mf: &ModuleManifest) -> String {
    mf.models
        .iter()
        .find(|m| m.default)
        .or_else(|| mf.models.first())
        .map(|m| m.id.clone())
        .unwrap_or_default()
}

// ─── 日志抽屉（快照桥数据；实时日志见模块详情页） ────────────────────────────

fn logs_drawer(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    module_status: Option<&crate::pages::ModuleSnapEntry>,
) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(trfb(lang, "desktopPages.models.logs", "日志", &[])).strong(),
        );
        ui.label(
            egui::RichText::new(trfb(
                lang,
                "desktopPages.models.logs.staleHint",
                "快照数据（访问仪表盘/模块页时同步）；实时日志请打开模块详情页",
                &[],
            ))
            .small()
            .color(pal.text_faint),
        );
    });
    ui.add_space(4.0);

    let logs = module_status.map(|e| e.logs.as_slice()).unwrap_or(&[]);
    egui::Frame::new()
        .fill(pal.bg)
        .stroke(egui::Stroke::new(1.0_f32, pal.border))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(180.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    if logs.is_empty() {
                        ui.label(
                            egui::RichText::new(tr(
                                lang,
                                "desktopPages.modules.noLogs",
                                &[],
                            ))
                            .small()
                            .color(pal.text_faint),
                        );
                    } else {
                        for line in logs {
                            ui.label(
                                egui::RichText::new(line.as_str()).monospace().small(),
                            );
                        }
                    }
                });
        });
}

// ─── 标签编辑抽屉 ────────────────────────────────────────────────────────────

fn tags_drawer(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    config: &AppConfig,
    mv: &ModelView,
    st: &mut ModelsUi,
) {
    ui.label(
        egui::RichText::new(trfb(
            lang,
            "desktopPages.models.tags.title",
            "编辑标签",
            &[],
        ))
        .strong(),
    );
    ui.add_space(4.0);

    // 未下载模型无 meta：仅提示
    let has_meta = read_meta_exists(st, mv);
    if !has_meta {
        ui.label(
            egui::RichText::new(trfb(
                lang,
                "desktopPages.models.tags.noMeta",
                "模型尚未下载（无 meta 文件），暂不能编辑标签",
                &[],
            ))
            .small()
            .color(pal.text_faint),
        );
        return;
    }

    let tags = st
        .meta_cache
        .get(&mv.target_dir)
        .map(|m| m.tags.clone())
        .unwrap_or_default();

    // chips + 删除
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(5.0, 4.0);
        for tag in &tags {
            if ui
                .selectable_label(true, format!("🏷 {tag}  ✕"))
                .on_hover_text(trfb(
                    lang,
                    "desktopPages.models.tags.removeTip",
                    "点击移除该标签",
                    &[],
                ))
                .clicked()
            {
                let mut next = tags.clone();
                next.retain(|t| t != tag);
                apply_tags(ui, lang, config, mv, next, st);
            }
        }
    });
    ui.add_space(6.0);

    // 输入 + 添加
    ui.horizontal(|ui| {
        let width = (ui.available_width() - 90.0).max(60.0);
        let resp = ui.add(
            egui::TextEdit::singleline(&mut st.tag_input)
                .desired_width(width)
                .hint_text(trfb(
                    lang,
                    "desktopPages.models.tags.inputHint",
                    "输入标签后回车或点添加",
                    &[],
                )),
        );
        let enter_pressed =
            resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let add_label = trfb(lang, "desktopPages.models.tags.add", "添加", &[]);
        let trimmed = st.tag_input.trim().to_string();
        if (ui
            .add_enabled(!trimmed.is_empty(), subtle_button(pal, format!("＋ {add_label}")))
            .clicked()
            || enter_pressed)
            && !trimmed.is_empty()
        {
            let mut next = tags.clone();
            if !next.contains(&trimmed) {
                next.push(trimmed);
            }
            st.tag_input.clear();
            apply_tags(ui, lang, config, mv, next, st);
        }
    });
    ui.label(
        egui::RichText::new(trfb(
            lang,
            "desktopPages.models.tags.hint",
            "标签存于模型 meta，随整合包流转",
            &[],
        ))
        .small()
        .color(pal.text_faint),
    );
}

/// meta 是否存在（缓存里有该模型的 meta 记录）
fn read_meta_exists(st: &ModelsUi, mv: &ModelView) -> bool {
    st.meta_cache.contains_key(&mv.target_dir)
}

/// 写入 tags（read-modify-write）并刷新缓存
fn apply_tags(
    ui: &mut egui::Ui,
    lang: &str,
    config: &AppConfig,
    mv: &ModelView,
    tags: Vec<String>,
    st: &mut ModelsUi,
) {
    match write_model_tags(config, &mv.target_dir, tags.clone()) {
        Ok(()) => {
            st.meta_cache
                .entry(mv.target_dir.clone())
                .or_default()
                .tags = tags;
            st.note = Some(trfb(
                lang,
                "desktopPages.models.tags.saved",
                "标签已更新",
                &[],
            ));
        }
        Err(e) => {
            st.note = Some(trfb(
                lang,
                "desktopPages.models.tags.saveFailed",
                "标签保存失败: {{detail}}",
                &[("detail", &e.to_string())],
            ));
        }
    }
    ui.ctx().request_repaint();
}

// ─── 服务卡兜底（无模型声明的模块，§5.1） ────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn service_cards(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    models: &[ModelView],
    data: &crate::pages::ModuleData,
    snapshot: Option<&crate::pages::ModuleSnapshot>,
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModelsUi,
) {
    // 无模型声明的模块 = 清单存在且 models 为空（且未出现在模型列表中）
    let service_modules: Vec<&ModuleManifest> = data
        .manifests()
        .filter(|mf| mf.models.is_empty())
        .filter(|mf| !models.iter().any(|mv| mv.module_id == mf.module.id))
        .collect();
    if service_modules.is_empty() {
        return;
    }

    ui.add_space(8.0);
    crate::ui::section_title(
        ui,
        &trfb(lang, "desktopPages.models.service.title", "服务模块", &[]),
    );
    ui.add_space(8.0);

    ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
    let cols = responsive_columns(ui.available_width(), 340.0, 12.0);
    card_grid(ui, cols, &service_modules, |ui, mf| {
        service_card(ui, lang, pal, mf, snapshot, cmd_tx, st);
    });
}

fn service_card(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    mf: &ModuleManifest,
    snapshot: Option<&crate::pages::ModuleSnapshot>,
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModelsUi,
) {
    let module_id = mf.module.id.clone();
    let entry = snapshot.and_then(|s| s.entries.iter().find(|e| e.id == module_id));
    let is_native = mf.runtime.runtime_type == RuntimeType::Native;

    card(ui, pal, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&mf.module.name).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(entry) = entry {
                    let meta = crate::ui::service_status(&entry.status, pal);
                    badge(
                        ui,
                        pal,
                        meta.color,
                        crate::pages::modules::service_label(lang, &entry.status),
                    );
                }
                badge(
                    ui,
                    pal,
                    pal.neutral,
                    if is_native {
                        trfb(lang, "desktopPages.models.service.native", "native 服务", &[])
                    } else {
                        trfb(lang, "desktopPages.models.service.noModel", "无模型模块", &[])
                    },
                );
            });
        });
        if !mf.module.description.is_empty() {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(&mf.module.description)
                    .small()
                    .color(pal.text_dim),
            );
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            match entry.map(|e| e.status.clone()) {
                Some(ServiceStatus::Running) | Some(ServiceStatus::Starting) => {
                    if ui
                        .add(subtle_button(
                            pal,
                            format!("⏹ {}", tr(lang, "common.action.stop", &[])),
                        ))
                        .clicked()
                    {
                        let _ = cmd_tx.send(AppCmd::StopModule(module_id.clone()));
                    }
                }
                Some(_) => {
                    if ui
                        .add(subtle_button(
                            pal,
                            format!("▶ {}", tr(lang, "common.action.start", &[])),
                        ))
                        .clicked()
                    {
                        let _ = cmd_tx.send(AppCmd::StartModule(module_id.clone()));
                    }
                }
                None => {
                    ui.add_enabled(
                        false,
                        subtle_button(
                            pal,
                            format!("▶ {}", tr(lang, "common.action.start", &[])),
                        ),
                    );
                }
            }
            if ui
                .add(subtle_button(
                    pal,
                    format!(
                        "📜 {}",
                        trfb(lang, "desktopPages.models.logs", "日志", &[])
                    ),
                ))
                .clicked()
            {
                // 服务卡日志复用 Tags 抽屉 key 空间以外的独立键：model_id 用模块 id 前缀
                toggle_drawer(st, DrawerKind::Logs, &format!("service:{module_id}"));
            }
        });

        // 日志抽屉（服务卡内联）
        if st.drawer.as_ref().map(|(_, k)| k.as_str())
            == Some(format!("service:{module_id}").as_str())
            && matches!(st.drawer, Some((DrawerKind::Logs, _)))
        {
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            logs_drawer(ui, lang, pal, entry);
        }
    });
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
        ModelSource::Modelscope => "Model Scope",
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

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ep_core::module::{CapabilityDecl, ModelDecl, ModelSource};
    use ep_core::types::ModuleCategory;

    /// 构造最小可用的 ModuleManifest（变体/能力测试夹具）
    fn fixture_manifest(models: Vec<ModelDecl>, caps: Vec<CapabilityDecl>) -> ModuleManifest {
        ModuleManifest {
            module: ep_core::module::ModuleInfo {
                id: "test-mod".into(),
                name: "Test".into(),
                version: "1.0".into(),
                description: "d".into(),
                category: ModuleCategory::Asr,
                genre: "g".into(),
                authors: vec![],
                license: None,
                homepage: None,
                tags: vec![],
            },
            runtime: ep_core::module::RuntimeConfig {
                runtime_type: RuntimeType::Python,
                python_version: Some(">=3.10".into()),
                requirements: None,
                entrypoint: None,
                start_command: None,
                binaries: None,
            },
            compute: ep_core::module::ComputeConfig {
                backends: vec![ep_core::types::ComputeBackend::Cpu],
                default_backend: None,
                vram_estimate_mb: Some(1024),
                min_vram_mb: None,
                env: None,
            },
            models,
            interface: ep_core::module::InterfaceConfig {
                interface_type: ep_core::module::InterfaceType::Http,
                health_endpoint: None,
                ready_timeout_secs: None,
                working_dir: None,
                capabilities: caps,
            },
        }
    }

    fn variant(id: &str, default: bool) -> ModelDecl {
        ModelDecl {
            id: id.into(),
            name: id.into(),
            source: ModelSource::Huggingface,
            repo_id: Some(format!("org/{id}")),
            url: None,
            target_dir: id.into(),
            revision: None,
            size_estimate_mb: None,
            qualified_id: None,
            vram_estimate_mb: None,
            default,
            mirrors: vec![],
        }
    }

    #[test]
    fn active_variant_prefers_config_when_declared() {
        let mf = fixture_manifest(vec![variant("a", true), variant("b", false)], vec![]);
        let mut cfg = AppConfig::default();
        cfg.active_models.insert("test-mod".into(), "b".into());
        assert_eq!(active_variant(&cfg, "test-mod", &mf), "b");
    }

    #[test]
    fn active_variant_ignores_config_when_not_declared() {
        // active_models 指向清单未声明的变体 → 回退 default=true
        let mf = fixture_manifest(vec![variant("a", true), variant("b", false)], vec![]);
        let mut cfg = AppConfig::default();
        cfg.active_models
            .insert("test-mod".into(), "ghost".into());
        assert_eq!(active_variant(&cfg, "test-mod", &mf), "a");
    }

    #[test]
    fn active_variant_falls_back_to_default_then_first() {
        let mf = fixture_manifest(vec![variant("x", false), variant("y", true)], vec![]);
        assert_eq!(
            active_variant(&AppConfig::default(), "test-mod", &mf),
            "y"
        );
        let mf_none = fixture_manifest(vec![variant("x", false)], vec![]);
        assert_eq!(
            active_variant(&AppConfig::default(), "test-mod", &mf_none),
            "x"
        );
    }

    #[test]
    fn draft_default_uses_schema_default_and_type() {
        use ep_core::module::ParamSchema;
        // ParamSchema.default 为 serde_json::Value；经 .into() 类型推断构造，
        // 无需在 ep-desktop 直接依赖 serde_json（同 params 编辑的匿名访问纪律）
        let schema_int = ParamSchema {
            param_type: "integer".into(),
            default: Some(5.into()),
            description: None,
            min: None,
            max: None,
            step: None,
            enum_values: None,
            options: None,
        };
        assert!(matches!(draft_default(&schema_int), ParamDraft::Int(5)));

        let schema_str = ParamSchema {
            param_type: "string".into(),
            default: Some("auto".into()),
            description: None,
            min: None,
            max: None,
            step: None,
            enum_values: None,
            options: None,
        };
        assert!(matches!(
            draft_default(&schema_str),
            ParamDraft::Str(s) if s == "auto"
        ));

        let schema_bool = ParamSchema {
            param_type: "boolean".into(),
            default: None,
            description: None,
            min: None,
            max: None,
            step: None,
            enum_values: None,
            options: None,
        };
        assert!(matches!(draft_default(&schema_bool), ParamDraft::Bool(false)));

        let schema_enum = ParamSchema {
            param_type: "string".into(),
            default: None,
            description: None,
            min: None,
            max: None,
            step: None,
            enum_values: Some(vec!["zh".into(), "en".into()]),
            options: None,
        };
        assert!(matches!(
            draft_default(&schema_enum),
            ParamDraft::Str(s) if s == "zh"
        ));
    }

    #[test]
    fn param_draft_to_arg_strings() {
        assert_eq!(ParamDraft::Str("zh".into()).to_arg(), "zh");
        assert_eq!(ParamDraft::Int(5).to_arg(), "5");
        assert_eq!(ParamDraft::Float(0.5).to_arg(), "0.5");
        assert_eq!(ParamDraft::Bool(true).to_arg(), "true");
    }

    #[test]
    fn tag_filter_requires_all_selected() {
        let mut st = ModelsUi::default();
        st.meta_cache.insert(
            "dir-a".into(),
            MetaLite {
                tags: vec!["字幕".into(), "视频".into()],
                qualified_id: None,
            },
        );
        let mv = ModelView {
            module_id: "m".into(),
            module_name: "M".into(),
            model_id: "x".into(),
            model_name: "X".into(),
            source: "huggingface".into(),
            repo_id: String::new(),
            target_dir: "dir-a".into(),
            status: ModelStatus::Ready,
            size_bytes: None,
            available_sources: vec![],
        };
        // 无筛选 → 通过
        assert!(matches_tag_filter(&st, &mv));
        // 单选命中 → 通过
        st.tag_filter = vec!["字幕".into()];
        assert!(matches_tag_filter(&st, &mv));
        // 多选含未命中 → 拒绝（AND 语义）
        st.tag_filter = vec!["字幕".into(), "音频".into()];
        assert!(!matches_tag_filter(&st, &mv));
        // 缓存缺失的模型 → 筛选激活时不通过
        let mv2 = ModelView {
            target_dir: "dir-none".into(),
            ..mv.clone()
        };
        assert!(!matches_tag_filter(&st, &mv2));
    }

    #[test]
    fn toggle_drawer_opens_and_closes() {
        let mut st = ModelsUi::default();
        toggle_drawer(&mut st, DrawerKind::Run, "model-1");
        assert_eq!(st.drawer, Some((DrawerKind::Run, "model-1".into())));
        // 再次点击同键 → 关闭
        toggle_drawer(&mut st, DrawerKind::Run, "model-1");
        assert_eq!(st.drawer, None);
        // 切换卡片 → 打开新键
        toggle_drawer(&mut st, DrawerKind::Tags, "model-2");
        assert_eq!(st.drawer, Some((DrawerKind::Tags, "model-2".into())));
    }

    #[test]
    fn group_by_module_preserves_order() {
        let models = vec![
            ModelView {
                module_id: "a".into(),
                module_name: "A".into(),
                model_id: "m1".into(),
                model_name: "M1".into(),
                source: String::new(),
                repo_id: String::new(),
                target_dir: "d1".into(),
                status: ModelStatus::Ready,
                size_bytes: None,
                available_sources: vec![],
            },
            ModelView {
                module_id: "b".into(),
                module_name: "B".into(),
                model_id: "m2".into(),
                model_name: "M2".into(),
                source: String::new(),
                repo_id: String::new(),
                target_dir: "d2".into(),
                status: ModelStatus::Ready,
                size_bytes: None,
                available_sources: vec![],
            },
            ModelView {
                module_id: "a".into(),
                module_name: "A".into(),
                model_id: "m3".into(),
                model_name: "M3".into(),
                source: String::new(),
                repo_id: String::new(),
                target_dir: "d3".into(),
                status: ModelStatus::Ready,
                size_bytes: None,
                available_sources: vec![],
            },
        ];
        let groups = group_by_module(&models);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "a");
        assert_eq!(groups[0].2.len(), 2);
        assert_eq!(groups[1].0, "b");
        assert_eq!(groups[1].2.len(), 1);
    }
}
