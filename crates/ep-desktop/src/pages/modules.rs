//! 模块管理单页 — 信息架构终稿（协调记录 #47）「模型就是模块」。
//!
//! NAV「模块」唯一入口，页面标题「模块管理」。旧「模型」页与「整合包」页已并入本页：
//! - **模块卡**：每模块一张卡；卡内运行状态 + 变体选择器（每变体 就绪/缺失 徽章）
//!   + 操作（启动/停止/日志/直跑/tag/下载选中变体/激活变体应用）；变体不独立成块。
//! - **顶部工具栏**：「导入模块」（rfd 选 .epzip → [`AppCmd::ImportPack`]，保留进度+toast）
//!   +「导出模块」（对话框圈选 模块/变体/管线 + 每模块许可证模式 bundle/reference
//!   + 包身份 → [`AppCmd::ExportPack`]）。
//! - **已装包管理**：无独立视图；卡内 pack 来源徽章菜单「卸载来源整合包」（keep_models 确认）。
//!
//! 数据来源：模块运行状态/日志经 app.rs 直接传入的权威 [`ModuleEntry`]；
//! 模型（变体）就绪状态/下载进度经 app.rs 传入；模块清单（capabilities/变体/VRAM）
//! 经 [`crate::pages::module_data`] 页面层缓存；tags/qualified_id/pack_id 读自模型
//! meta（`.ep_meta.json`）。所有耗时操作经 [`crate::app::AppCmd`] 走后台线程。

use std::collections::HashMap;

use eframe::egui;
use ep_core::config::AppConfig;
use ep_core::model::{DownloadState, ModelStatus, ModelView, UpdateCheckResult};
use ep_core::module::{ModelSource, ModuleManifest};
use ep_core::types::{ModuleCategory, ServiceStatus};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{
    AppCmd, DownloadUiState, ModuleEntry, PackEntry, PackExportModule, PackExportSpec,
    PackImportUiState,
};
use crate::i18n::tr;
use crate::pages::{
    draft_default, format_size, module_data, open_path, read_model_meta, trfb, write_model_tags,
    ParamDraft,
};
use crate::ui::{
    badge, card, confirm_dialog_with_lang, danger_button, empty_state, page_header, primary_button,
    responsive_columns, section_title, service_status, subtle_button, Palette, CONTROL_ROUNDING,
};

// ─── 页面持久状态 ────────────────────────────────────────────────────────────

/// 模型 meta 展示缓存（tags + qualified_id + pack_id，随整合包流转的 §4.3/§4.4 字段）
#[derive(Debug, Clone, Default)]
struct MetaLite {
    tags: Vec<String>,
    qualified_id: Option<String>,
    /// 来源整合包 ID（pack 来源徽章 + 卸载菜单的数据源）
    pack_id: Option<String>,
}

/// 卡片抽屉种类（Config 已并入卡内变体选择器，不再独立抽屉）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrawerKind {
    Run,
    Logs,
    Tags,
}

/// 可导出管线 id（config/pipelines/*.toml 扫描结果，对话框打开时缓存；
/// 打包时后台按 id 反查文件，路径不入对话框状态）
#[derive(Debug, Clone)]
struct PipeInfo {
    id: String,
}

#[derive(Debug, Clone, Default)]
struct ModulesUi {
    /// tag chips 筛选（多选 AND 语义：模块须含至少一个命中变体才显示）
    tag_filter: Vec<String>,
    /// 当前展开的抽屉 (种类, module_id)；同一时间仅一个
    drawer: Option<(DrawerKind, String)>,
    /// 直跑表单：module_id → 选中 capability
    run_cap: HashMap<String, String>,
    /// 直跑表单：module_id → 输入文件路径
    run_input: HashMap<String, String>,
    /// 直跑表单：module_id → 参数草稿（顺序 = schema 顺序）
    run_params: HashMap<String, Vec<(String, ParamDraft)>>,
    /// 变体选择器：module_id → 当前选中变体 id（运行/tag/下载/激活的目标）
    sel_variant: HashMap<String, String>,
    /// 激活变体已落盘记录：module_id → 变体 id（保证激活徽章即时正确）
    cfg_applied: HashMap<String, String>,
    /// tag 编辑输入框
    tag_input: String,
    /// meta 缓存：target_dir → MetaLite（"加载一次 + 刷新按钮重载"）
    meta_cache: HashMap<String, MetaLite>,
    meta_loaded: bool,
    /// 行内提示（激活已保存 / tag 已更新 / 直跑已提交等）
    note: Option<String>,
    // ── 导出模块对话框 ──
    export_open: bool,
    /// 圈选：module_id → 勾选的变体 id 集
    export_sel: HashMap<String, Vec<String>>,
    /// 每模块许可证模式：module_id → bundle（true）/ reference（false）
    export_bundle: HashMap<String, bool>,
    /// 勾选的管线 id
    export_pipes: Vec<String>,
    /// 可用管线（打开对话框时扫描缓存）
    export_pipes_avail: Vec<PipeInfo>,
    export_pipes_loaded: bool,
    export_id: String,
    export_name: String,
    export_version: String,
    export_note: Option<String>,
    // ── 卸载来源整合包确认 ──
    uninstall_pack: Option<String>,
    uninstall_keep: bool,
    // ── 删除模型确认（§5.1 卡内删除）──
    delete_confirm: Option<DeleteConfirm>,
}

/// 删除模型确认上下文
#[derive(Debug, Clone)]
struct DeleteConfirm {
    module_id: String,
    target_dir: String,
    model_name: String,
}

fn page_state_id() -> egui::Id {
    egui::Id::new("modules_unified_ui_state")
}

// ─── 主入口 ──────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    config: &AppConfig,
    modules: &mut [ModuleEntry],
    models: &[ModelView],
    downloads: &HashMap<String, DownloadUiState>,
    updates: &HashMap<String, UpdateCheckResult>,
    download_sources: &mut HashMap<String, Option<ModelSource>>,
    packs: &[PackEntry],
    pack_imports: &HashMap<String, PackImportUiState>,
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    let lang = ep_core::i18n::normalize_language(&config.general.language);
    let pal = Palette::new(ui.style().visuals.dark_mode);
    let mut st = ui
        .ctx()
        .data(|d| d.get_temp::<ModulesUi>(page_state_id()))
        .unwrap_or_default();

    // 模块清单缓存（capabilities/变体/VRAM）
    let data = module_data(ui.ctx(), false);

    // meta 缓存首次加载（每模型一个小 JSON，一次性）
    if !st.meta_loaded {
        load_meta_cache(config, models, &mut st);
    }

    // ── 页头：「模块管理」+ 顶部工具栏 ──
    page_header(
        ui,
        &tr(lang, "desktopPages.modules.title", &[]),
        |ui| {
            toolbar(ui, lang, &pal, cmd_tx, &mut st);
        },
    );
    ui.add_space(8.0);

    egui::ScrollArea::vertical().show(ui, |ui| {
        // ── 进行中的整合包导入进度（导入模块链路，原整合包页迁移） ──
        import_progress_strip(ui, lang, &pal, pack_imports);

        // ── 缓存目录行（紧凑） ──
        cache_dir_row(ui, lang, &pal, config);
        ui.add_space(10.0);

        // ── tag chips 筛选行 ──
        tag_filter_row(ui, lang, &pal, &mut st);

        // ── 行内提示 ──
        if let Some(note) = st.note.clone() {
            ui.label(egui::RichText::new(note).small().color(pal.info));
            ui.add_space(4.0);
        }

        // ── 模块卡网格（每模块一张卡） ──
        if modules.is_empty() {
            empty_state(
                ui,
                &pal,
                "🧩",
                &tr(lang, "desktopPages.modules.empty.title", &[]),
                &tr(lang, "desktopPages.modules.empty.hint", &[]),
            );
        } else {
            render_module_grid(
                ui, lang, &pal, config, modules, models, &data, downloads, updates,
                download_sources, packs, cmd_tx, &mut st,
            );
        }
        ui.add_space(8.0);
    });

    // ── 导出模块对话框（模态窗口） ──
    if st.export_open {
        export_dialog(ui, lang, &pal, models, &data, cmd_tx, &mut st);
    }

    // ── 卸载来源整合包确认对话框 ──
    uninstall_confirm(ui, lang, &pal, packs, cmd_tx, &mut st);

    // ── 删除模型确认对话框（§5.1 卡内删除） ──
    delete_confirm_dialog(ui, lang, &pal, cmd_tx, &mut st);

    ui.ctx()
        .data_mut(|d| *d.get_temp_mut_or_default::<ModulesUi>(page_state_id()) = st);
}

// ─── 删除模型确认 ────────────────────────────────────────────────────────────

fn delete_confirm_dialog(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModulesUi,
) {
    let Some(target) = st.delete_confirm.clone() else {
        return;
    };
    let title = trfb(
        lang,
        "desktopPages.modules.deleteModel.title",
        "删除模型「{{name}}」？",
        &[("name", &target.model_name)],
    );
    let message = trfb(
        lang,
        "desktopPages.modules.deleteModel.description",
        "将删除该模型的本机文件与缓存目录（{{dir}}），不可恢复",
        &[("dir", &target.target_dir)],
    );
    let confirm = tr(lang, "common.action.delete", &[]);
    match confirm_dialog_with_lang(
        ui.ctx(),
        pal,
        &format!("dlg_delete_model_{}", target.module_id),
        &title,
        &message,
        &confirm,
        true,
        lang,
    ) {
        Some(true) => {
            let _ = cmd_tx.send(AppCmd::DeleteModel(target.target_dir.clone()));
            st.delete_confirm = None;
            st.note = Some(trfb(
                lang,
                "desktopPages.modules.deleteModel.deleting",
                "正在删除模型…",
                &[],
            ));
        }
        Some(false) => st.delete_confirm = None,
        None => {}
    }
}

// ─── 顶部工具栏（协调记录 #47：导入模块 / 导出模块） ─────────────────────────

fn toolbar(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModulesUi,
) {
    // 刷新
    if ui
        .add(subtle_button(
            pal,
            format!("🔄 {}", tr(lang, "common.action.refresh", &[])),
        ))
        .clicked()
    {
        let _ = cmd_tx.send(AppCmd::RefreshModels);
        let _ = cmd_tx.send(AppCmd::RefreshPacks);
        module_data(ui.ctx(), true);
        st.meta_loaded = false;
        st.note = None;
    }
    // 检查全部更新
    if ui
        .add(subtle_button(
            pal,
            format!("🔍 {}", tr(lang, "desktopPages.models.checkAllUpdates", &[])),
        ))
        .on_hover_text(tr(lang, "desktopPages.models.checkAllUpdatesTip", &[]))
        .clicked()
    {
        let _ = cmd_tx.send(AppCmd::CheckAllUpdates);
    }
    // 导入模块（rfd 选 .epzip → AppCmd::ImportPack，保留既有链路+进度+toast）
    if ui
        .add(primary_button(
            pal,
            format!(
                "📥 {}",
                trfb(lang, "desktopPages.modules.toolbar.import", "导入模块", &[])
            ),
        ))
        .on_hover_text(trfb(
            lang,
            "desktopPages.modules.toolbar.importTip",
            "选择 .epzip 整合包导入（模型 + 管线）",
            &[],
        ))
        .clicked()
    {
        if let Some(file) = rfd::FileDialog::new()
            .add_filter("EntryPoint Pack (.epzip)", &["epzip"])
            .pick_file()
        {
            let _ = cmd_tx.send(AppCmd::ImportPack { path: file });
        }
    }
    // 导出模块（打开圈选对话框）
    if ui
        .add(subtle_button(
            pal,
            format!(
                "📤 {}",
                trfb(lang, "desktopPages.modules.toolbar.export", "导出模块", &[])
            ),
        ))
        .on_hover_text(trfb(
            lang,
            "desktopPages.modules.toolbar.exportTip",
            "圈选模块/变体/管线打包为 .epzip",
            &[],
        ))
        .clicked()
    {
        st.export_open = true;
        st.export_pipes_loaded = false;
        st.export_note = None;
    }
}

// ─── 导入进度条（原整合包页迁移） ────────────────────────────────────────────

fn import_progress_strip(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    pack_imports: &HashMap<String, PackImportUiState>,
) {
    if pack_imports.is_empty() {
        return;
    }
    section_title(
        ui,
        &trfb(lang, "desktopPages.modules.importing", "模块导入中", &[]),
    );
    ui.add_space(6.0);
    let mut entries: Vec<(String, PackImportUiState)> = pack_imports
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    for (pack_id, import_st) in entries {
        card(ui, pal, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&pack_id).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(pack_stage_label(lang, &import_st.stage))
                            .small()
                            .color(pal.text_dim),
                    );
                });
            });
            ui.add_space(4.0);
            let bar = match import_st.percent {
                Some(p) => egui::ProgressBar::new((p / 100.0).clamp(0.0, 1.0))
                    .desired_width(ui.available_width()),
                None => egui::ProgressBar::new(0.0)
                    .desired_width(ui.available_width())
                    .animate(true),
            };
            ui.add(bar);
        });
        ui.add_space(6.0);
    }
    ui.add_space(6.0);
}

/// 整合包导入阶段文案（ImportStage 小写阶段名 → i18n 键 `desktopApp.packs.stage.<stage>`）
fn pack_stage_label(lang: &str, stage: &str) -> String {
    let key = format!("desktopApp.packs.stage.{stage}");
    let translated = tr(lang, &key, &[]);
    if translated == key {
        stage.to_string()
    } else {
        translated
    }
}

// ─── 缓存目录行 ──────────────────────────────────────────────────────────────

fn cache_dir_row(ui: &mut egui::Ui, lang: &str, pal: &Palette, config: &AppConfig) {
    let root = ep_core::config::resolve_root();
    let cache_dir = config.resolve_model_cache_dir(&root);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!(
                "📂 {}",
                trfb(lang, "desktopPages.modules.cacheDir", "模型缓存目录", &[])
            ))
            .color(pal.text_dim),
        );
        ui.add(
            egui::Label::new(
                egui::RichText::new(cache_dir.display().to_string())
                    .monospace()
                    .color(pal.text_dim),
            )
            .selectable(true),
        );
        if ui
            .add(subtle_button(
                pal,
                trfb(lang, "desktopPages.modules.openDir", "打开", &[]),
            ))
            .clicked()
        {
            open_path(&cache_dir);
        }
    });
}

// ─── meta 缓存 ───────────────────────────────────────────────────────────────

fn load_meta_cache(config: &AppConfig, models: &[ModelView], st: &mut ModulesUi) {
    st.meta_cache.clear();
    for mv in models {
        if let Some(meta) = read_model_meta(config, &mv.target_dir) {
            st.meta_cache.insert(
                mv.target_dir.clone(),
                MetaLite {
                    tags: meta.tags,
                    qualified_id: meta.qualified_id,
                    pack_id: meta.pack_id,
                },
            );
        }
    }
    st.meta_loaded = true;
}

// ─── tag 筛选 ────────────────────────────────────────────────────────────────

/// 模块是否命中 tag 筛选：无筛选恒显示；有筛选时模块任一已下载变体命中全部选中 tag 即显示
fn module_matches_filter(st: &ModulesUi, module_models: &[&ModelView]) -> bool {
    if st.tag_filter.is_empty() {
        return true;
    }
    module_models.iter().any(|mv| {
        st.meta_cache
            .get(&mv.target_dir)
            .map(|m| st.tag_filter.iter().all(|t| m.tags.contains(t)))
            .unwrap_or(false)
    })
}

fn tag_filter_row(ui: &mut egui::Ui, lang: &str, pal: &Palette, st: &mut ModulesUi) {
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
            if ui.selectable_label(active, format!("🏷 {tag}")).clicked() {
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

// ─── 模块卡网格（每模块一张卡；索引遍历以支持清空日志等 &mut 操作） ─────────

#[allow(clippy::too_many_arguments)]
fn render_module_grid(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    config: &AppConfig,
    modules: &mut [ModuleEntry],
    models: &[ModelView],
    data: &crate::pages::ModuleData,
    downloads: &HashMap<String, DownloadUiState>,
    updates: &HashMap<String, UpdateCheckResult>,
    download_sources: &mut HashMap<String, Option<ModelSource>>,
    packs: &[PackEntry],
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModulesUi,
) {
    // 预分组：module_id → 该模块的模型（变体）视图
    let mut by_module: HashMap<String, Vec<&ModelView>> = HashMap::new();
    for mv in models {
        by_module.entry(mv.module_id.clone()).or_default().push(mv);
    }

    // tag 筛选可见性（按模块）
    let visible: Vec<bool> = modules
        .iter()
        .map(|m| {
            let module_models = by_module.get(&m.id).cloned().unwrap_or_default();
            module_matches_filter(st, &module_models)
        })
        .collect();

    let visible_indices: Vec<usize> = (0..modules.len()).filter(|&i| visible[i]).collect();
    if visible_indices.is_empty() {
        empty_state(
            ui,
            pal,
            "🏷",
            &trfb(lang, "desktopPages.modules.filter.empty", "无匹配模块", &[]),
            &trfb(
                lang,
                "desktopPages.modules.filter.emptyHint",
                "调整或清除标签筛选后重试",
                &[],
            ),
        );
        return;
    }

    // 汇总条（可见模块的运行状态计数）
    summary_bar(ui, lang, pal, modules, &visible_indices);
    ui.add_space(8.0);

    let cols = responsive_columns(ui.available_width(), 360.0, 12.0);
    let spacing = ui.spacing().item_spacing.x;
    let mut row_start = 0;
    while row_start < visible_indices.len() {
        let row_end = (row_start + cols).min(visible_indices.len());
        let row = &visible_indices[row_start..row_end];
        ui.horizontal(|ui| {
            let avail = ui.available_width();
            let n = row.len().max(1) as f32;
            let col_w = ((avail - spacing * (n - 1.0)) / n).max(80.0);
            for &idx in row {
                let module_models = by_module.get(&modules[idx].id).cloned().unwrap_or_default();
                ui.scope(|ui| {
                    ui.set_width(col_w);
                    module_card(
                        ui, lang, pal, config, &mut modules[idx], &module_models, data,
                        downloads, updates, download_sources, packs, cmd_tx, st,
                    );
                });
            }
        });
        row_start = row_end;
    }
}

/// 可见模块的状态计数徽章（0 不显示）
fn summary_bar(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    modules: &[ModuleEntry],
    visible: &[usize],
) {
    let running = visible
        .iter()
        .filter(|&&i| modules[i].status.is_running())
        .count();
    let stopped = visible
        .iter()
        .filter(|&&i| modules[i].status == ServiceStatus::Stopped)
        .count();
    let errors = visible
        .iter()
        .filter(|&&i| matches!(modules[i].status, ServiceStatus::Error(_)))
        .count();
    let not_ready = visible
        .iter()
        .filter(|&&i| modules[i].status == ServiceStatus::NotReady)
        .count();

    ui.horizontal(|ui| {
        if running > 0 {
            badge(
                ui,
                pal,
                pal.success,
                tr(lang, "desktopPages.modules.summary.running", &[("count", &running.to_string())]),
            );
        }
        if stopped > 0 {
            badge(
                ui,
                pal,
                pal.neutral,
                tr(lang, "desktopPages.modules.summary.stopped", &[("count", &stopped.to_string())]),
            );
        }
        if errors > 0 {
            badge(
                ui,
                pal,
                pal.danger,
                tr(lang, "desktopPages.modules.summary.errors", &[("count", &errors.to_string())]),
            );
        }
        if not_ready > 0 {
            badge(
                ui,
                pal,
                pal.notready,
                tr(lang, "desktopPages.modules.summary.notReady", &[("count", &not_ready.to_string())]),
            );
        }
    });
}

// ─── 单个模块卡（协调记录 #47：模块=模型统一卡） ─────────────────────────────

#[allow(clippy::too_many_arguments)]
fn module_card(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    config: &AppConfig,
    m: &mut ModuleEntry,
    module_models: &[&ModelView],
    data: &crate::pages::ModuleData,
    downloads: &HashMap<String, DownloadUiState>,
    updates: &HashMap<String, UpdateCheckResult>,
    download_sources: &mut HashMap<String, Option<ModelSource>>,
    packs: &[PackEntry],
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModulesUi,
) {
    let manifest = data.manifest(&m.id).cloned();
    // 默认选中变体：激活变体（延迟初始化，避免每帧覆盖用户选择）
    if let Some(mf) = &manifest {
        if !st.sel_variant.contains_key(&m.id) {
            let v = effective_active_variant(config, st, &m.id, mf);
            st.sel_variant.insert(m.id.clone(), v);
        }
    }

    card(ui, pal, |ui| {
        ui.set_width(ui.available_width());

        // ── 卡头：名称 + 类别 + 运行状态徽章 ──
        let meta = service_status(&m.status, pal);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&m.name).strong());
            ui.label(
                egui::RichText::new(format!("v{}", m.version))
                    .small()
                    .color(pal.text_faint),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                badge(ui, pal, meta.color, service_label(lang, &m.status));
            });
        });
        // 类别 + 端口/设备
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(category_label(lang, &m.category))
                    .small()
                    .color(pal.text_dim),
            );
            if let Some(port) = m.port {
                ui.label(
                    egui::RichText::new(format!(":{port}"))
                        .monospace()
                        .small()
                        .color(pal.text_faint),
                );
            }
            if let Some(dev) = &m.device {
                ui.label(
                    egui::RichText::new(dev.as_str())
                        .monospace()
                        .small()
                        .color(pal.text_faint),
                );
            }
        });
        if let ServiceStatus::Error(err) = &m.status {
            if !err.is_empty() {
                ui.add_space(2.0);
                ui.label(egui::RichText::new(err).small().color(pal.danger));
            }
        }
        if !m.description.is_empty() {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(&m.description).small().color(pal.text_dim));
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        match &manifest {
            Some(mf) if !mf.models.is_empty() => {
                // ── 变体选择器 + 选中变体信息 + 操作 ──
                variant_section(
                    ui, lang, pal, config, m, mf, module_models, downloads, updates,
                    download_sources, packs, cmd_tx, st,
                );
            }
            _ => {
                // 无模型声明（native 服务 / 清单加载失败）：仅运行操作 + 日志
                ui.label(
                    egui::RichText::new(trfb(
                        lang,
                        "desktopPages.modules.noVariants",
                        "该模块未声明模型变体（服务型模块）",
                        &[],
                    ))
                    .small()
                    .color(pal.text_faint),
                );
                ui.add_space(6.0);
                service_action_row(ui, lang, pal, m, cmd_tx, st);
            }
        }

        // ── 抽屉（运行/日志/tag） ──
        drawer_area(ui, lang, pal, config, m, manifest.as_ref(), cmd_tx, st);
    });
}

// ─── 变体选择器 + 选中变体信息 + 操作 ────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn variant_section(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    config: &AppConfig,
    m: &mut ModuleEntry,
    mf: &ModuleManifest,
    module_models: &[&ModelView],
    downloads: &HashMap<String, DownloadUiState>,
    updates: &HashMap<String, UpdateCheckResult>,
    download_sources: &mut HashMap<String, Option<ModelSource>>,
    packs: &[PackEntry],
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModulesUi,
) {
    let active = effective_active_variant(config, st, &m.id, mf);
    let selected = st
        .sel_variant
        .get(&m.id)
        .cloned()
        .unwrap_or_else(|| active.clone());

    // ── 变体选择器：radio + 每变体 就绪/缺失 徽章（变体不独立成块） ──
    ui.label(
        egui::RichText::new(trfb(lang, "desktopPages.modules.variants", "模型变体", &[]))
            .small()
            .strong()
            .color(pal.text_dim),
    );
    ui.add_space(2.0);
    for decl in &mf.models {
        let mv = module_models.iter().find(|v| v.model_id == decl.id);
        let chosen = selected == decl.id;
        ui.horizontal(|ui| {
            if ui.radio(chosen, "").clicked() && !chosen {
                st.sel_variant.insert(m.id.clone(), decl.id.clone());
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
            // 激活徽章
            if decl.id == active {
                badge(
                    ui,
                    pal,
                    pal.primary,
                    trfb(lang, "desktopPages.models.active", "激活", &[]),
                );
            }
            // 就绪/缺失徽章（来自 ModelView 状态）
            if let Some(v) = mv {
                let (color, label) = status_meta(lang, &v.status, pal);
                badge(ui, pal, color, label);
                // 下载进度（进行中）
                if let Some(dl) = downloads.get(&v.model_id) {
                    if matches!(dl.state, DownloadState::Downloading) {
                        ui.label(
                            egui::RichText::new(format!("{:.0}%", dl.percent))
                                .monospace()
                                .small()
                                .color(pal.info),
                        );
                    }
                }
            }
        });
    }
    ui.add_space(6.0);

    // ── 选中变体的元信息（qualified_id / 来源 / VRAM / 目录 / tag / pack 来源） ──
    let sel_mv = module_models.iter().find(|v| v.model_id == selected);
    let sel_decl = mf.models.iter().find(|d| d.id == selected);
    if let (Some(mv), Some(decl)) = (sel_mv, sel_decl) {
        let meta = st.meta_cache.get(&mv.target_dir).cloned().unwrap_or_default();
        // qualified_id（meta 优先，manifest 兜底）
        let qualified = meta
            .qualified_id
            .clone()
            .or_else(|| decl.qualified_id.clone());
        if let Some(qid) = &qualified {
            ui.label(
                egui::RichText::new(qid).monospace().small().color(pal.text_faint),
            );
        }
        // 来源 + VRAM + 大小
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
                .small()
                .color(pal.text_dim),
            );
            if let Some(mb) = mf.resolve_vram_estimate(&mv.model_id) {
                badge(
                    ui,
                    pal,
                    pal.info,
                    trfb(lang, "desktopPages.models.vram", "VRAM ≈ {{mb}} MB", &[("mb", &mb.to_string())]),
                );
            }
            if let Some(size) = mv.size_bytes {
                ui.label(egui::RichText::new(format_size(size)).small().color(pal.text_dim));
            }
        });
        // 目标目录
        ui.label(
            egui::RichText::new(tr(lang, "desktopPages.models.dir", &[("dir", mv.target_dir.as_str())]))
                .monospace()
                .small()
                .color(pal.text_faint),
        );
        // tag chips + pack 来源徽章
        ui.add_space(2.0);
        tag_and_pack_row(ui, lang, pal, &meta, packs, m, st);
    }

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(6.0);

    // ── 操作区：启动/停止/日志/直跑/tag/下载选中变体/激活变体应用 ──
    action_row(
        ui, lang, pal, config, m, mf, sel_mv.copied(), downloads, updates,
        download_sources, cmd_tx, st,
    );
}

/// tag chips + pack 来源徽章（徽章菜单「卸载来源整合包」）
fn tag_and_pack_row(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    meta: &MetaLite,
    packs: &[PackEntry],
    m: &ModuleEntry,
    st: &mut ModulesUi,
) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(5.0, 4.0);
        // pack 来源徽章（协调记录 #47：已装包无独立视图，徽章菜单承载卸载入口）
        if let Some(pack_id) = &meta.pack_id {
            let pack_name = packs
                .iter()
                .find(|p| p.id == *pack_id)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| pack_id.clone());
            let resp = ui
                .selectable_label(false, format!("🎁 {pack_name}"))
                .on_hover_text(trfb(
                    lang,
                    "desktopPages.modules.packSourceTip",
                    "来自整合包；点击可卸载该来源整合包",
                    &[],
                ));
            resp.context_menu(|ui| {
                if ui
                    .button(trfb(
                        lang,
                        "desktopPages.modules.uninstall.menu",
                        "卸载来源整合包",
                        &[],
                    ))
                    .clicked()
                {
                    st.uninstall_pack = Some(pack_id.clone());
                    ui.close_menu();
                }
            });
            // 直接点击徽章也打开卸载确认（菜单为显式入口）
            if resp.clicked() {
                st.uninstall_pack = Some(pack_id.clone());
            }
        }
        // tag chips
        for tag in &meta.tags {
            if ui.selectable_label(false, format!("🏷 {tag}")).clicked() {
                toggle_drawer(st, DrawerKind::Tags, &m.id);
            }
        }
        if ui
            .add(subtle_button(pal, egui::RichText::new("＋").color(pal.text_dim)))
            .on_hover_text(trfb(lang, "desktopPages.models.tags.editTip", "编辑标签", &[]))
            .clicked()
        {
            toggle_drawer(st, DrawerKind::Tags, &m.id);
        }
    });
}

// ─── 操作区（模块服务 + 选中变体操作） ───────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn action_row(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    config: &AppConfig,
    m: &mut ModuleEntry,
    mf: &ModuleManifest,
    sel_mv: Option<&ModelView>,
    downloads: &HashMap<String, DownloadUiState>,
    updates: &HashMap<String, UpdateCheckResult>,
    download_sources: &mut HashMap<String, Option<ModelSource>>,
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModulesUi,
) {
    // 行 1：模块服务操作（启动/停止/重启 + 日志）
    service_action_row(ui, lang, pal, m, cmd_tx, st);
    ui.add_space(4.0);

    let selected = st.sel_variant.get(&m.id).cloned().unwrap_or_default();
    let active = effective_active_variant(config, st, &m.id, mf);
    let sel_ready = sel_mv.map(|v| v.status == ModelStatus::Ready).unwrap_or(false);
    let downloading = sel_mv.and_then(|v| downloads.get(&v.model_id));
    let has_update = sel_mv
        .and_then(|v| updates.get(&v.model_id))
        .map(|u| u.available)
        .unwrap_or(false);

    // 行 2：直跑 + tag（模块级）
    ui.horizontal(|ui| {
        // 直跑（需选中变体就绪）
        let run_btn = ui.add_enabled(
            sel_ready,
            subtle_button(
                pal,
                format!("▶️ {}", trfb(lang, "desktopPages.models.run", "运行", &[])),
            ),
        );
        let run_btn = if sel_ready {
            run_btn.on_hover_text(trfb(
                lang,
                "desktopPages.models.runTip",
                "单模型直跑：选能力 → 填参数 → 指定输入文件",
                &[],
            ))
        } else {
            run_btn.on_hover_text(trfb(
                lang,
                "desktopPages.modules.runNeedsReady",
                "请先下载并选中就绪的模型变体",
                &[],
            ))
        };
        if run_btn.clicked() {
            toggle_drawer(st, DrawerKind::Run, &m.id);
            ensure_run_form(st, &m.id, Some(mf));
        }
        // tag 编辑（需选中变体已下载，有 meta）
        let has_meta = sel_mv
            .map(|v| st.meta_cache.contains_key(&v.target_dir))
            .unwrap_or(false);
        let tag_btn = ui.add_enabled(
            has_meta,
            subtle_button(
                pal,
                format!("🏷 {}", trfb(lang, "desktopPages.modules.tags", "标签", &[])),
            ),
        );
        let tag_btn = if has_meta {
            tag_btn
        } else {
            tag_btn.on_hover_text(trfb(
                lang,
                "desktopPages.models.tags.noMeta",
                "模型尚未下载（无 meta 文件），暂不能编辑标签",
                &[],
            ))
        };
        if tag_btn.clicked() {
            toggle_drawer(st, DrawerKind::Tags, &m.id);
        }
        // 导入模型（rfd 选文件/目录 → AppCmd::ImportModel；桌面端直连本地）
        let import_btn = ui.add_enabled(
            sel_mv.is_some(),
            subtle_button(
                pal,
                format!("📂 {}", trfb(lang, "desktopPages.modules.importModel", "导入模型", &[])),
            ),
        );
        let import_btn = if sel_mv.is_some() {
            import_btn.on_hover_text(trfb(
                lang,
                "desktopPages.modules.importModelTip",
                "从本地选择模型文件或目录导入到选中变体",
                &[],
            ))
        } else {
            import_btn.on_hover_text(trfb(
                lang,
                "desktopPages.modules.importModelNoVariant",
                "请先选择模型变体",
                &[],
            ))
        };
        if import_btn.clicked() {
            if let Some(mv) = sel_mv {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title(trfb(
                        lang,
                        "desktopPages.modules.pickModelSource",
                        "选择模型文件或目录",
                        &[],
                    ))
                    .pick_file()
                {
                    let _ = cmd_tx.send(AppCmd::ImportModel {
                        module_id: m.id.clone(),
                        model_id: mv.model_id.clone(),
                        source: path,
                    });
                }
            }
        }
        // 删除模型（确认 → AppCmd::DeleteModel；仅选中变体就绪时可用）
        let del_btn = ui.add_enabled(
            sel_ready,
            danger_button(
                pal,
                format!("🗑 {}", tr(lang, "common.action.delete", &[])),
            ),
        );
        if del_btn.clicked() {
            if let Some(mv) = sel_mv {
                st.delete_confirm = Some(DeleteConfirm {
                    module_id: m.id.clone(),
                    target_dir: mv.target_dir.clone(),
                    model_name: mv.model_name.clone(),
                });
            }
        }
    });
    ui.add_space(4.0);

    // 行 3：选中变体下载 + 激活变体应用
    ui.horizontal(|ui| {
        if let Some(dl) = downloading {
            // 下载进度 + 取消（复用模型页进度组件语义）
            download_progress_compact(ui, lang, pal, dl, &selected, cmd_tx);
        } else if let Some(mv) = sel_mv {
            if mv.status != ModelStatus::Ready {
                download_action(ui, lang, pal, mv, download_sources, cmd_tx);
            } else if has_update {
                if ui
                    .add(primary_button(
                        pal,
                        format!("⬇ {}", tr(lang, "desktopPages.models.redownload", &[])),
                    ))
                    .clicked()
                {
                    let source = download_sources.get(&mv.model_id).copied().unwrap_or(None);
                    send_download(cmd_tx, download_sources, mv, source);
                }
                if ui
                    .add(subtle_button(
                        pal,
                        format!("🔍 {}", tr(lang, "desktopPages.models.checkUpdate", &[])),
                    ))
                    .clicked()
                {
                    let _ = cmd_tx.send(AppCmd::CheckUpdate {
                        module_id: mv.module_id.clone(),
                        model_id: mv.model_id.clone(),
                    });
                }
            } else {
                ui.label(
                    egui::RichText::new(trfb(
                        lang,
                        "desktopPages.modules.variantReady",
                        "选中变体已就绪",
                        &[],
                    ))
                    .small()
                    .color(pal.success),
                );
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // 激活变体应用（选中 ≠ 激活 时可点）
            let changed = !selected.is_empty() && selected != active;
            let apply_btn = ui.add_enabled(
                changed,
                primary_button(
                    pal,
                    trfb(lang, "desktopPages.modules.applyVariant", "激活变体应用", &[]),
                ),
            );
            let apply_btn = if changed {
                apply_btn
            } else {
                apply_btn.on_hover_text(trfb(
                    lang,
                    "desktopPages.models.config.current",
                    "当前即为激活变体",
                    &[],
                ))
            };
            if apply_btn.clicked() {
                apply_active_variant(ui, lang, config, m, selected.clone(), st);
            }
        });
    });
}

/// 激活变体落盘：写 config.active_models 并保存（§5.2 单槽位）
fn apply_active_variant(
    ui: &mut egui::Ui,
    lang: &str,
    config: &AppConfig,
    m: &ModuleEntry,
    variant: String,
    st: &mut ModulesUi,
) {
    let mut new_cfg = config.clone();
    new_cfg.active_models.insert(m.id.clone(), variant.clone());
    let config_dir = ep_core::config::resolve_root().join("config");
    match new_cfg.save(&config_dir) {
        Ok(()) => {
            st.cfg_applied.insert(m.id.clone(), variant);
            st.note = Some(trfb(
                lang,
                "desktopPages.models.config.saved",
                "已保存激活变体；下次启动模块生效，运行中模块需重启",
                &[],
            ));
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
    ui.ctx().request_repaint();
}

/// 模块服务操作行：启动/停止（确认）/重启（确认）+ 日志抽屉
fn service_action_row(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    m: &mut ModuleEntry,
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModulesUi,
) {
    let key_stop = egui::Id::new(("confirm_stop", m.id.clone()));
    let key_restart = egui::Id::new(("confirm_restart", m.id.clone()));

    ui.horizontal(|ui| match &m.status {
        ServiceStatus::Stopped => {
            if ui
                .add(primary_button(
                    pal,
                    format!("▶ {}", tr(lang, "common.action.start", &[])),
                ))
                .clicked()
            {
                let _ = cmd_tx.send(AppCmd::StartModule(m.id.clone()));
            }
        }
        ServiceStatus::Running | ServiceStatus::Starting => {
            if ui
                .add(danger_button(
                    pal,
                    format!("⏹ {}", tr(lang, "common.action.stop", &[])),
                ))
                .clicked()
            {
                ui.ctx().data_mut(|d| d.insert_temp(key_stop, true));
            }
        }
        ServiceStatus::Error(_) => {
            let btn = egui::Button::new(egui::RichText::new(format!(
                "🔄 {}",
                tr(lang, "common.action.restart", &[])
            ))
            .color(pal.bg))
            .fill(pal.warning)
            .corner_radius(egui::CornerRadius::same(CONTROL_ROUNDING))
            .stroke(egui::Stroke::NONE);
            if ui.add(btn).clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(key_restart, true));
            }
        }
        ServiceStatus::Preparing => {
            ui.spinner();
            ui.label(
                egui::RichText::new(format!("{}…", tr(lang, "common.status.preparing", &[])))
                    .color(pal.text_dim),
            );
        }
        ServiceStatus::NotReady => {
            ui.label(
                egui::RichText::new(format!(
                    "⚠ {}",
                    tr(lang, "desktopPages.modules.notReadyHint", &[])
                ))
                .small()
                .color(pal.text_dim),
            );
        }
    });

    // 日志抽屉按钮
    if ui
        .add(subtle_button(
            pal,
            format!("📜 {}", trfb(lang, "desktopPages.modules.logsBtn", "日志", &[])),
        ))
        .clicked()
    {
        toggle_drawer(st, DrawerKind::Logs, &m.id);
    }

    // 停止确认
    if ui.ctx().data(|d| d.get_temp::<bool>(key_stop).unwrap_or(false)) {
        let title = tr(lang, "desktopPages.modules.dlg.stop.title", &[]);
        let message = tr(lang, "desktopPages.modules.dlg.stop.message", &[("name", m.name.as_str())]);
        let confirm = tr(lang, "common.action.stop", &[]);
        match confirm_dialog_with_lang(
            ui.ctx(), pal, &format!("dlg_stop_{}", m.id), &title, &message, &confirm, true, lang,
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
    // 重启确认（先 Stop 再 Start）
    if ui.ctx().data(|d| d.get_temp::<bool>(key_restart).unwrap_or(false)) {
        let title = tr(lang, "desktopPages.modules.dlg.restart.title", &[]);
        let message = tr(lang, "desktopPages.modules.dlg.restart.message", &[("name", m.name.as_str())]);
        let confirm = tr(lang, "common.action.restart", &[]);
        match confirm_dialog_with_lang(
            ui.ctx(), pal, &format!("dlg_restart_{}", m.id), &title, &message, &confirm, false, lang,
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

/// 下载进度紧凑展示（变体选择器下方操作行内）
fn download_progress_compact(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    dl: &DownloadUiState,
    model_id: &str,
    cmd_tx: &UnboundedSender<AppCmd>,
) {
    let status = match &dl.state {
        DownloadState::Downloading => egui::RichText::new(format!(
            "⬇ {} {:.0}% ({})",
            tr(lang, "desktopPages.models.downloading", &[]),
            dl.percent,
            format_size(dl.bytes)
        ))
        .color(pal.info),
        DownloadState::Completed => egui::RichText::new(format!(
            "✅ {}",
            tr(lang, "desktopPages.models.downloadDone", &[])
        ))
        .color(pal.success),
        DownloadState::Failed(_) => {
            egui::RichText::new(tr(lang, "desktopPages.models.downloadFailed", &[])).color(pal.danger)
        }
        DownloadState::Cancelled => {
            egui::RichText::new(tr(lang, "common.status.cancelled", &[])).color(pal.warning)
        }
    };
    ui.label(status.small());
    if matches!(dl.state, DownloadState::Downloading)
        && ui
            .add(subtle_button(pal, tr(lang, "common.action.cancel", &[])))
            .clicked()
    {
        let _ = cmd_tx.send(AppCmd::CancelDownload(model_id.to_string()));
    }
}

// ─── 抽屉区域 ────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn drawer_area(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    config: &AppConfig,
    m: &mut ModuleEntry,
    manifest: Option<&ModuleManifest>,
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModulesUi,
) {
    let Some((kind, key)) = st.drawer.clone() else {
        return;
    };
    if key != m.id {
        return;
    }
    ui.add_space(6.0);
    ui.separator();
    ui.add_space(6.0);
    match kind {
        DrawerKind::Run => run_drawer(ui, lang, pal, m, manifest, cmd_tx, st),
        DrawerKind::Logs => logs_drawer(ui, lang, pal, m),
        DrawerKind::Tags => tags_drawer(ui, lang, pal, config, m, st),
    }
}

// ─── 直跑抽屉（§5.3） ────────────────────────────────────────────────────────

fn ensure_run_form(st: &mut ModulesUi, module_id: &str, manifest: Option<&ModuleManifest>) {
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

fn rebuild_param_drafts(st: &mut ModulesUi, module_id: &str, mf: &ModuleManifest) {
    let cap_name = st.run_cap.get(module_id).cloned().unwrap_or_default();
    let cap = mf.interface.capabilities.iter().find(|c| c.name == cap_name);
    let Some(cap) = cap else {
        st.run_params.insert(module_id.to_string(), Vec::new());
        return;
    };
    let mut schemas: Vec<(&String, &ep_core::module::ParamSchema)> = cap
        .params
        .as_ref()
        .map(|p| p.iter().collect())
        .unwrap_or_default();
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
    m: &ModuleEntry,
    manifest: Option<&ModuleManifest>,
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModulesUi,
) {
    ui.label(
        egui::RichText::new(trfb(lang, "desktopPages.models.run.title", "单模型直跑", &[]))
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
    ensure_run_form(st, &m.id, Some(mf));

    // capability 选择
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(trfb(lang, "desktopPages.models.run.capability", "能力", &[]))
                .color(pal.text_dim),
        );
        let current = st.run_cap.get(&m.id).cloned().unwrap_or_default();
        egui::ComboBox::from_id_salt(egui::Id::new(("run_cap", m.id.clone())))
            .selected_text(if current.is_empty() { "-" } else { &current })
            .show_ui(ui, |ui| {
                for cap in caps {
                    let selected = current == cap.name;
                    if ui.selectable_label(selected, &cap.name).clicked() && !selected {
                        st.run_cap.insert(m.id.clone(), cap.name.clone());
                        rebuild_param_drafts(st, &m.id, mf);
                    }
                }
            });
        if let Some(cap) = caps.iter().find(|c| c.name == current) {
            if !cap.description.is_empty() {
                ui.label(egui::RichText::new(&cap.description).small().color(pal.text_faint));
            }
        }
    });
    ui.add_space(4.0);

    // 参数表单（schema 驱动）
    let cap_name = st.run_cap.get(&m.id).cloned().unwrap_or_default();
    if let Some(cap) = caps.iter().find(|c| c.name == cap_name) {
        if let Some(schema_map) = &cap.params {
            if !schema_map.is_empty() {
                let mut keys: Vec<&String> = schema_map.keys().collect();
                keys.sort();
                for key in keys {
                    let schema = &schema_map[key];
                    param_draft_row(ui, pal, &m.id, key, schema, st);
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
                .set_title(trfb(lang, "desktopPages.models.run.pickFile", "选择输入文件", &[]))
                .pick_file()
            {
                st.run_input
                    .insert(m.id.clone(), file.to_string_lossy().to_string());
            }
        }
        let width = (ui.available_width() - 10.0).max(60.0);
        let input_path = st.run_input.entry(m.id.clone()).or_default();
        ui.add(
            egui::TextEdit::singleline(input_path)
                .desired_width(width)
                .hint_text(trfb(lang, "desktopPages.models.run.inputHint", "输入文件路径", &[])),
        );
    });
    ui.add_space(6.0);

    // 提交
    let input_path = st
        .run_input
        .get(&m.id)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let input_ok = !input_path.is_empty() && std::path::Path::new(&input_path).is_file();
    let cap_ok = !cap_name.is_empty();
    ui.horizontal(|ui| {
        let submit_label = trfb(lang, "desktopPages.models.run.submit", "提交执行", &[]);
        let resp = ui.add_enabled(input_ok && cap_ok, primary_button(pal, format!("⚡ {submit_label}")));
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
                .get(&m.id)
                .map(|drafts| drafts.iter().map(|(n, d)| (n.clone(), d.to_arg())).collect())
                .unwrap_or_default();
            let _ = cmd_tx.send(AppCmd::ExecuteSingle {
                module_id: m.id.clone(),
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
    st: &mut ModulesUi,
) {
    let t = schema.param_type.to_ascii_lowercase();
    let enum_options = schema
        .enum_values
        .as_ref()
        .or(schema.options.as_ref())
        .cloned()
        .unwrap_or_default();

    let mut drafts = st.run_params.remove(module_id).unwrap_or_default();
    let Some(idx) = drafts.iter().position(|(n, _)| n == key) else {
        st.run_params.insert(module_id.to_string(), drafts);
        return;
    };

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("{key}:")).color(pal.text_dim));
        if !enum_options.is_empty() {
            let current = match &drafts[idx].1 {
                ParamDraft::Str(s) => s.clone(),
                other => other.to_arg(),
            };
            egui::ComboBox::from_id_salt(egui::Id::new(("param_enum", module_id, key)))
                .selected_text(if current.is_empty() { "-" } else { &current })
                .show_ui(ui, |ui| {
                    for opt in &enum_options {
                        if ui.selectable_label(current == *opt, opt).clicked() {
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
            let min = schema.min.map(|v| v as i64).unwrap_or(i64::MIN / 2);
            let max = schema.max.map(|v| v as i64).unwrap_or(i64::MAX / 2);
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

// ─── 日志抽屉（权威 ModuleEntry.logs，实时） ─────────────────────────────────

fn logs_drawer(ui: &mut egui::Ui, lang: &str, pal: &Palette, m: &mut ModuleEntry) {
    ui.horizontal(|ui| {
        let count = m.logs.len().to_string();
        ui.label(
            egui::RichText::new(tr(lang, "desktopPages.modules.logs", &[("count", &count)]))
                .strong(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(subtle_button(pal, tr(lang, "desktopPages.modules.clearLogs", &[])))
                .clicked()
            {
                m.logs.clear();
            }
        });
    });
    ui.add_space(4.0);
    egui::Frame::new()
        .fill(pal.bg)
        .stroke(egui::Stroke::new(1.0_f32, pal.border))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    if m.logs.is_empty() {
                        ui.label(
                            egui::RichText::new(tr(lang, "desktopPages.modules.noLogs", &[]))
                                .small()
                                .color(pal.text_faint),
                        );
                    } else {
                        for line in &m.logs {
                            ui.label(egui::RichText::new(line.as_str()).monospace().small());
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
    m: &ModuleEntry,
    st: &mut ModulesUi,
) {
    ui.label(
        egui::RichText::new(trfb(lang, "desktopPages.models.tags.title", "编辑标签", &[]))
            .strong(),
    );
    ui.add_space(4.0);

    // 定位选中变体的 target_dir（tag 存于模型 meta；经清单反查变体声明）
    let selected = st.sel_variant.get(&m.id).cloned().unwrap_or_default();
    let data = module_data(ui.ctx(), false);
    let target_dir = data
        .manifest(&m.id)
        .and_then(|mf| mf.models.iter().find(|d| d.id == selected))
        .map(|d| d.target_dir.clone());
    let Some(target_dir) = target_dir else {
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
    };
    if !st.meta_cache.contains_key(&target_dir) {
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
        .get(&target_dir)
        .map(|meta| meta.tags.clone())
        .unwrap_or_default();

    // chips + 删除
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(5.0, 4.0);
        for tag in &tags {
            if ui
                .selectable_label(true, format!("🏷 {tag}  ✕"))
                .on_hover_text(trfb(lang, "desktopPages.models.tags.removeTip", "点击移除该标签", &[]))
                .clicked()
            {
                let mut next = tags.clone();
                next.retain(|t| t != tag);
                apply_tags(ui, lang, config, &target_dir, next, st);
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
        let enter_pressed = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
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
            apply_tags(ui, lang, config, &target_dir, next, st);
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

fn apply_tags(
    ui: &mut egui::Ui,
    lang: &str,
    config: &AppConfig,
    target_dir: &str,
    tags: Vec<String>,
    st: &mut ModulesUi,
) {
    match write_model_tags(config, target_dir, tags.clone()) {
        Ok(()) => {
            st.meta_cache.entry(target_dir.to_string()).or_default().tags = tags;
            st.note = Some(trfb(lang, "desktopPages.models.tags.saved", "标签已更新", &[]));
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

// ─── 激活变体解析（§5.2 单槽位） ─────────────────────────────────────────────

fn active_variant(config: &AppConfig, module_id: &str, mf: &ModuleManifest) -> String {
    if let Some(id) = config.active_models.get(module_id) {
        if mf.models.iter().any(|m| &m.id == id) {
            return id.clone();
        }
    }
    active_variant_from_manifest(mf)
}

fn effective_active_variant(
    config: &AppConfig,
    st: &ModulesUi,
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

// ─── 下载组件 ────────────────────────────────────────────────────────────────

/// 下载入口（Missing / Incomplete）：单源直接下载，多源先弹出行内来源选择
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
    let mut open = ui.ctx().data(|d| d.get_temp::<bool>(open_key)).unwrap_or(false);
    let multi = mv.available_sources.len() > 1;

    if !open {
        let label = if mv.status == ModelStatus::Incomplete {
            format!("⬇ {}", tr(lang, "desktopPages.models.redownload", &[]))
        } else {
            format!("⬇ {}", tr(lang, "common.action.download", &[]))
        };
        let btn = primary_button(pal, label);
        let resp = if multi {
            ui.add(btn).on_hover_text(tr(lang, "desktopPages.models.multiSourceTip", &[]))
        } else {
            ui.add(btn)
        };
        if resp.clicked() {
            if multi {
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
        let mut selected: ModelSource = ui
            .ctx()
            .data(|d| d.get_temp::<ModelSource>(sel_key))
            .unwrap_or_else(|| {
                mv.available_sources.first().copied().unwrap_or(ModelSource::Huggingface)
            });
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(tr(lang, "desktopPages.models.sourceSelect", &[]))
                    .color(pal.text_dim),
            );
            for src in &mv.available_sources {
                if ui.selectable_label(selected == *src, source_label(src)).clicked() {
                    selected = *src;
                }
            }
        });
        ui.ctx().data_mut(|d| d.insert_temp(sel_key, selected));
        ui.horizontal(|ui| {
            if ui
                .add(primary_button(pal, tr(lang, "desktopPages.models.confirmDownload", &[])))
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

// ─── 状态/类别/时长 文案（dashboard/tasks 共用的公开 helper） ────────────────

/// 模型状态 → (语义色, 本地化文案)。颜色一律取自当前主题色板。
fn status_meta(lang: &str, status: &ModelStatus, pal: &Palette) -> (egui::Color32, String) {
    match status {
        ModelStatus::Ready => (pal.success, tr(lang, "common.status.ready", &[])),
        ModelStatus::Missing => (pal.danger, tr(lang, "common.status.missing", &[])),
        ModelStatus::Incomplete => (pal.warning, tr(lang, "common.status.incomplete", &[])),
        ModelStatus::Importable => (pal.info, tr(lang, "desktopPages.models.importable", &[])),
    }
}

/// 本地化的模块类别文案；`Other` 承载 manifest 原始字符串，按数据原样显示。
pub fn category_label(lang: &str, c: &ModuleCategory) -> String {
    match c {
        ModuleCategory::Asr => tr(lang, "desktopPages.modules.cat.asr", &[]),
        ModuleCategory::Tts => tr(lang, "desktopPages.modules.cat.tts", &[]),
        ModuleCategory::Denoise => tr(lang, "desktopPages.modules.cat.denoise", &[]),
        ModuleCategory::Ocr => tr(lang, "desktopPages.modules.cat.ocr", &[]),
        ModuleCategory::Image => tr(lang, "desktopPages.modules.cat.image", &[]),
        ModuleCategory::Translate => tr(lang, "desktopPages.modules.cat.translate", &[]),
        ModuleCategory::Video => tr(lang, "desktopPages.modules.cat.video", &[]),
        ModuleCategory::Face => tr(lang, "desktopPages.modules.cat.face", &[]),
        ModuleCategory::Custom => tr(lang, "desktopPages.modules.cat.custom", &[]),
        ModuleCategory::Other(s) => s.clone(),
    }
}

/// 本地化的服务状态文案；颜色仍取自 [`crate::ui::service_status`]。
pub fn service_label(lang: &str, status: &ServiceStatus) -> String {
    let key = match status {
        ServiceStatus::Running => "common.status.running",
        ServiceStatus::Stopped => "common.status.stopped",
        ServiceStatus::Starting => "common.status.starting",
        ServiceStatus::Preparing => "common.status.preparing",
        ServiceStatus::Error(_) => "common.status.error",
        ServiceStatus::NotReady => "common.status.notReady",
    };
    tr(lang, key, &[])
}

// ─── 抽屉切换 ────────────────────────────────────────────────────────────────

fn toggle_drawer(st: &mut ModulesUi, kind: DrawerKind, module_id: &str) {
    let next = (kind, module_id.to_string());
    if st.drawer.as_ref() == Some(&next) {
        st.drawer = None;
    } else {
        st.drawer = Some(next);
        st.tag_input.clear();
        st.note = None;
    }
}

// ─── 导出模块对话框（协调记录 #47） ─────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn export_dialog(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    models: &[ModelView],
    data: &crate::pages::ModuleData,
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModulesUi,
) {
    // 首次打开：初始化包身份默认值 + 扫描可用管线
    if st.export_id.is_empty() {
        st.export_id = format!("local.build-{}", chrono_stamp());
    }
    if st.export_version.is_empty() {
        st.export_version = "0.1.0".to_string();
    }
    if !st.export_pipes_loaded {
        st.export_pipes_avail = scan_pipelines();
        st.export_pipes_loaded = true;
    }

    let mut open = true;
    egui::Window::new(trfb(lang, "desktopPages.modules.export.title", "导出模块", &[]))
        .id(egui::Id::new("export_module_dialog"))
        .collapsible(false)
        .resizable(true)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .open(&mut open)
        .show(ui.ctx(), |ui| {
            ui.set_min_width(420.0);
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                // ── 模块 + 变体圈选（每模块许可证模式二选一） ──
                section_title(
                    ui,
                    &trfb(lang, "desktopPages.modules.export.modules", "勾选模块与变体", &[]),
                );
                ui.add_space(4.0);
                let manifests: Vec<ModuleManifest> = data.manifests().cloned().collect();
                if manifests.is_empty() {
                    ui.label(
                        egui::RichText::new(trfb(
                            lang,
                            "desktopPages.modules.export.noModules",
                            "无可导出模块",
                            &[],
                        ))
                        .small()
                        .color(pal.text_faint),
                    );
                }
                for mf in &manifests {
                    if mf.models.is_empty() {
                        continue; // 服务型模块无模型可导出
                    }
                    let mid = mf.module.id.clone();
                    egui::CollapsingHeader::new(
                        egui::RichText::new(format!("{} ({})", mf.module.name, mf.module.id)).strong(),
                    )
                    .default_open(false)
                    .show(ui, |ui| {
                        // 许可证模式二选一（文案提示按许可证选择）
                        let bundle = st.export_bundle.entry(mid.clone()).or_insert(false);
                        ui.horizontal(|ui| {
                            ui.radio_value(bundle, true, trfb(
                                lang,
                                "desktopPages.modules.export.bundle",
                                "随包附带权重",
                                &[],
                            ));
                            ui.radio_value(bundle, false, trfb(
                                lang,
                                "desktopPages.modules.export.reference",
                                "仅元数据从指定渠道下载",
                                &[],
                            ));
                        });
                        ui.label(
                            egui::RichText::new(trfb(
                                lang,
                                "desktopPages.modules.export.licenseTip",
                                "请按模型许可证选择：允许再分发权重选「随包附带」，否则选「仅元数据」",
                                &[],
                            ))
                            .small()
                            .color(pal.text_faint),
                        );
                        ui.add_space(2.0);
                        for decl in &mf.models {
                            let mut checked = st
                                .export_sel
                                .get(&mid)
                                .map(|s| s.contains(&decl.id))
                                .unwrap_or(false);
                            // 变体就绪状态徽章
                            let mv = models
                                .iter()
                                .find(|v| v.module_id == mid && v.model_id == decl.id);
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut checked, &decl.name).changed() {
                                    let s = st.export_sel.entry(mid.clone()).or_default();
                                    if checked {
                                        if !s.contains(&decl.id) {
                                            s.push(decl.id.clone());
                                        }
                                    } else {
                                        s.retain(|x| x != &decl.id);
                                    }
                                }
                                if let Some(v) = mv {
                                    let (color, label) = status_meta(lang, &v.status, pal);
                                    badge(ui, pal, color, label);
                                }
                            });
                        }
                    });
                }

                ui.add_space(10.0);

                // ── 管线圈选 ──
                section_title(ui, &trfb(lang, "desktopPages.modules.export.pipelines", "勾选管线", &[]));
                ui.add_space(4.0);
                if st.export_pipes_avail.is_empty() {
                    ui.label(
                        egui::RichText::new(trfb(
                            lang,
                            "desktopPages.modules.export.noPipelines",
                            "无可用管线",
                            &[],
                        ))
                        .small()
                        .color(pal.text_faint),
                    );
                }
                for pipe in st.export_pipes_avail.clone() {
                    let mut checked = st.export_pipes.contains(&pipe.id);
                    if ui.checkbox(&mut checked, &pipe.id).changed() {
                        if checked {
                            if !st.export_pipes.contains(&pipe.id) {
                                st.export_pipes.push(pipe.id.clone());
                            }
                        } else {
                            st.export_pipes.retain(|x| x != &pipe.id);
                        }
                    }
                }

                ui.add_space(10.0);

                // ── 包身份 ──
                section_title(ui, &trfb(lang, "desktopPages.modules.export.identity", "包身份", &[]));
                ui.add_space(4.0);
                egui::Grid::new("export_identity_grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label(trfb(lang, "desktopPages.modules.export.id", "包 ID", &[]));
                    ui.add(egui::TextEdit::singleline(&mut st.export_id).desired_width(240.0));
                    ui.end_row();
                    ui.label(trfb(lang, "desktopPages.modules.export.name", "名称", &[]));
                    ui.add(egui::TextEdit::singleline(&mut st.export_name).desired_width(240.0));
                    ui.end_row();
                    ui.label(trfb(lang, "desktopPages.modules.export.version", "版本", &[]));
                    ui.add(egui::TextEdit::singleline(&mut st.export_version).desired_width(120.0));
                    ui.end_row();
                });

                // ── 校验提示 ──
                if let Some(note) = st.export_note.clone() {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(note).small().color(pal.warning));
                }
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // 取消
                    if ui
                        .add(subtle_button(pal, tr(lang, "common.action.cancel", &[])))
                        .clicked()
                    {
                        st.export_open = false;
                    }
                    // 导出：rfd 选目录 → 发送 ExportPack
                    let has_sel = st.export_sel.values().any(|v| !v.is_empty())
                        || !st.export_pipes.is_empty();
                    let btn = ui.add_enabled(
                        has_sel,
                        primary_button(pal, trfb(lang, "desktopPages.modules.export.submit", "导出…", &[])),
                    );
                    let btn = if has_sel {
                        btn
                    } else {
                        btn.on_hover_text(trfb(
                            lang,
                            "desktopPages.modules.export.empty",
                            "请至少勾选一个变体或管线",
                            &[],
                        ))
                    };
                    if btn.clicked() {
                        submit_export(ui, lang, cmd_tx, st);
                    }
                });
            });
        });
    if !open {
        st.export_open = false;
    }
}

/// 提交导出：rfd 选保存目录 → 组装 [`PackExportSpec`] → [`AppCmd::ExportPack`]
fn submit_export(
    ui: &mut egui::Ui,
    lang: &str,
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModulesUi,
) {
    let Some(output_dir) = rfd::FileDialog::new()
        .set_title(trfb(lang, "desktopPages.modules.export.pickDir", "选择 .epzip 保存目录", &[]))
        .pick_folder()
    else {
        return;
    };
    let modules: Vec<PackExportModule> = st
        .export_sel
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(mid, variants)| PackExportModule {
            module_id: mid.clone(),
            bundle: st.export_bundle.get(mid).copied().unwrap_or(false),
            variants: variants.clone(),
        })
        .collect();
    let spec = PackExportSpec {
        modules,
        pipelines: st.export_pipes.clone(),
        id: st.export_id.trim().to_string(),
        name: st.export_name.trim().to_string(),
        version: st.export_version.trim().to_string(),
        output_dir,
    };
    let _ = cmd_tx.send(AppCmd::ExportPack { spec });
    st.export_open = false;
    st.note = Some(trfb(
        lang,
        "desktopPages.modules.export.submitted",
        "导出任务已提交，后台组装打包中",
        &[],
    ));
    ui.ctx().request_repaint();
}

/// 扫描 config/pipelines/*.toml → 管线 id 列表；损坏文件跳过
fn scan_pipelines() -> Vec<PipeInfo> {
    let root = ep_core::config::resolve_root();
    let dir = root.join("config").join("pipelines");
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return out;
    };
    let mut paths: Vec<std::path::PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
        .collect();
    paths.sort();
    for path in paths {
        if let Ok(pipeline) = ep_core::pipeline::Pipeline::from_toml(&path) {
            out.push(PipeInfo { id: pipeline.id });
        }
    }
    out
}

/// 时间戳（包身份默认值 / 后台组装唯一 id 用）
fn chrono_stamp() -> String {
    chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string()
}

// ─── 卸载来源整合包确认（keep_models 二选一） ────────────────────────────────

fn uninstall_confirm(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    packs: &[PackEntry],
    cmd_tx: &UnboundedSender<AppCmd>,
    st: &mut ModulesUi,
) {
    let Some(pack_id) = st.uninstall_pack.clone() else {
        return;
    };
    let pack_name = packs
        .iter()
        .find(|p| p.id == pack_id)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| pack_id.clone());

    let title = trfb(lang, "desktopPages.modules.uninstall.title", "卸载来源整合包", &[]);
    let message = trfb(
        lang,
        "desktopPages.modules.uninstall.message",
        "卸载「{{name}}」？可选择是否保留已下载的模型文件。",
        &[("name", &pack_name)],
    );

    let mut result: Option<bool> = None; // Some(true)=卸载, 由 keep 决定保留模型
    let mut open = true;
    let cancel_label = tr(lang, "common.action.cancel", &[]);
    egui::Window::new(title)
        .id(egui::Id::new("uninstall_pack_dialog"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .open(&mut open)
        .show(ui.ctx(), |ui| {
            ui.set_min_width(300.0);
            ui.label(message);
            ui.add_space(8.0);
            ui.checkbox(
                &mut st.uninstall_keep,
                trfb(lang, "desktopPages.modules.uninstall.keepModels", "保留模型文件", &[]),
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(subtle_button(pal, cancel_label)).clicked() {
                        result = Some(false);
                    }
                    if ui
                        .add(danger_button(
                            pal,
                            trfb(lang, "desktopPages.modules.uninstall.confirm", "卸载", &[]),
                        ))
                        .clicked()
                    {
                        result = Some(true);
                    }
                });
            });
        });

    let confirmed = match result {
        Some(v) => Some(v),
        None if !open => Some(false),
        None => None,
    };
    if let Some(true) = confirmed {
        let keep = st.uninstall_keep;
        let _ = cmd_tx.send(AppCmd::UninstallPack {
            pack_id: pack_id.clone(),
            keep_models: keep,
        });
        st.uninstall_pack = None;
        st.uninstall_keep = false;
        st.note = Some(trfb(
            lang,
            "desktopPages.modules.uninstall.submitted",
            "卸载请求已提交",
            &[],
        ));
        let _ = cmd_tx.send(AppCmd::RefreshPacks);
    } else if let Some(false) = confirmed {
        st.uninstall_pack = None;
        st.uninstall_keep = false;
    }
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ep_core::module::{CapabilityDecl, ModelDecl, ModelSource, RuntimeType};

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
        let mf = fixture_manifest(vec![variant("a", true), variant("b", false)], vec![]);
        let mut cfg = AppConfig::default();
        cfg.active_models.insert("test-mod".into(), "ghost".into());
        assert_eq!(active_variant(&cfg, "test-mod", &mf), "a");
    }

    #[test]
    fn active_variant_falls_back_to_default_then_first() {
        let mf = fixture_manifest(vec![variant("x", false), variant("y", true)], vec![]);
        assert_eq!(active_variant(&AppConfig::default(), "test-mod", &mf), "y");
        let mf_none = fixture_manifest(vec![variant("x", false)], vec![]);
        assert_eq!(active_variant(&AppConfig::default(), "test-mod", &mf_none), "x");
    }

    #[test]
    fn toggle_drawer_opens_and_closes() {
        let mut st = ModulesUi::default();
        toggle_drawer(&mut st, DrawerKind::Run, "mod-1");
        assert_eq!(st.drawer, Some((DrawerKind::Run, "mod-1".into())));
        toggle_drawer(&mut st, DrawerKind::Run, "mod-1");
        assert_eq!(st.drawer, None);
        toggle_drawer(&mut st, DrawerKind::Tags, "mod-2");
        assert_eq!(st.drawer, Some((DrawerKind::Tags, "mod-2".into())));
    }

    #[test]
    fn draft_default_uses_schema_default_and_type() {
        use ep_core::module::ParamSchema;
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
    fn module_filter_requires_variant_hit() {
        let mut st = ModulesUi::default();
        st.meta_cache.insert(
            "dir-a".into(),
            MetaLite {
                tags: vec!["字幕".into()],
                qualified_id: None,
                pack_id: None,
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
        // 无筛选 → 显示
        assert!(module_matches_filter(&st, &[&mv]));
        // 命中 → 显示
        st.tag_filter = vec!["字幕".into()];
        assert!(module_matches_filter(&st, &[&mv]));
        // 未命中 → 隐藏
        st.tag_filter = vec!["音频".into()];
        assert!(!module_matches_filter(&st, &[&mv]));
        // 空变体列表 + 有筛选 → 隐藏
        st.tag_filter = vec!["字幕".into()];
        assert!(!module_matches_filter(&st, &[]));
    }

    #[test]
    fn pack_export_spec_defaults_and_stamp() {
        let stamp = chrono_stamp();
        assert_eq!(stamp.len(), 15); // YYYYmmdd-HHMMSS
        assert!(stamp.contains('-'));
    }
}
