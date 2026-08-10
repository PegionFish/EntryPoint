//! 管线编辑器 — 对齐 WebUI 成熟模式（用户裁决：裁撤两态库视图）。
//!
//! ## 信息架构（与 WebUI `pipeline.tsx` 一致）
//!
//! 页面打开即编辑器：工具栏含库下拉菜单「当前管线」、新建、打开文件…、
//! 保存/验证/执行；主体为三栏（palette | 画布 | 参数面板），画布含缩放
//! 控件/MiniMap 等 5 项 egui 自研替代（`canvas.rs`）。无管线时显示画布
//! 空态 + 引导。
//!
//! WebUI 的 `PipelineLibraryBar` 下拉切换在桌面端以 egui ComboBox 等价
//! 实现：列出 `config/pipelines/*.toml`（名称 + 节点数），标注当前项；
//! dirty 时切换/新建/打开均经确认对话框拦截。
//!
//! ## 模块拆分
//!
//! - [`canvas`]：画布交互与绘制（缩放锚定/框选/MiniMap/拖放落点/视觉升级）；
//! - [`panel`]：palette（拖放载荷源）+ 右侧参数面板 + 节点编辑器 + VRAM 账本；
//! - [`library`]：管线库扫描纯函数（库下拉菜单数据源）；
//! - [`edit`]：数据变更（建删节点/连线校验/草稿/加载保存校验执行）；
//! - [`toml_serde`]：TOML 序列化（§6.2 文件形状）。
//!
//! 加载/保存/验证/执行链路行为不变；TOML 解析与执行引擎调用契约不变。

mod canvas;
mod edit;
mod library;
mod panel;
mod toml_serde;

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui;
use ep_core::config::AppConfig;
use ep_core::pipeline::dag::{Edge, NodeKind, Pipeline};
use ep_core::types::TaskStatus;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::AppCmd;
use crate::i18n::tr;
use crate::pages::{
    device_snapshot, module_data, tasks_snapshot, trfb, ModuleData, ParamDraft,
};
use crate::ui::{
    badge, confirm_dialog_with_lang, empty_state, primary_button_with_glow, subtle_button,
    Palette,
};

const NODE_W: f32 = 160.0;
const NODE_H: f32 = 60.0;
const LAYER_GAP: f32 = 220.0;
const NODE_GAP: f32 = 80.0;
const TITLE_H: f32 = 24.0;
const PORT_R: f32 = 4.0;
const GRID_SPACING: f32 = 40.0;

/// 端口命中半径（屏幕像素，与缩放无关）
const PORT_HIT: f32 = 12.0;
/// 连线命中半径（屏幕像素）
const EDGE_HIT: f32 = 6.0;

/// 节点类型色：内置（紫）
const NODE_COLOR_BUILTIN: egui::Color32 = egui::Color32::from_rgb(139, 92, 246);
/// 节点类型色：LLM / 外部 API（橙）
const NODE_COLOR_API: egui::Color32 = egui::Color32::from_rgb(249, 115, 22);

/// 缩放步进（画布 overlay − / ＋）
const ZOOM_STEP: f32 = 1.18;
const ZOOM_MIN: f32 = 0.3;
const ZOOM_MAX: f32 = 3.0;

// ── Persistent state ──────────────────────────────────────────────

/// dirty 保护下待确认的切换动作（对齐 WebUI `confirmDiscardIfDirty`）
#[derive(Clone)]
pub(super) enum PendingAction {
    /// 从库下拉菜单切换加载指定管线文件
    Switch(PathBuf),
    /// 新建空白管线
    New,
    /// 打开文件…（确认后弹系统文件选择器）
    Open,
    /// 加载示例（内置模板生成示例 DAG，对齐 WebUI onLoadExample）
    LoadExample,
}

/// 库下拉菜单底部动作（对齐 WebUI PipelineLibraryBar 行为语义）
#[derive(Clone)]
enum LibraryMenuAction {
    /// 加载示例
    LoadExample,
    /// 另存为到 config/pipelines/（文件名输入对话框）
    SaveAs,
    /// 导出当前管线 TOML 到用户选择的路径
    Export,
    /// 导入注册：选 TOML 复制进 config/pipelines/ 并加载
    Import,
}

/// palette 拖放载荷（映射表 #9：palette → 画布拖放建节点）
#[derive(Clone, Debug)]
pub(super) enum PalettePayload {
    Builtin(String),
    Llm,
    Module { module_id: String, capability: String },
}

#[derive(Clone)]
pub(super) struct VizState {
    file_path: String,
    pipeline: Option<Pipeline>,
    /// 有未保存改动
    dirty: bool,
    validation_msg: Option<String>,
    /// 最近一次验证/保存结果是否成功（决定状态栏颜色）
    validation_ok: bool,
    positions: HashMap<String, egui::Pos2>,
    selected: Option<String>,
    selected_edge: Option<Edge>,
    /// 框选多选集合（映射表 #8）
    multi_select: Vec<String>,
    /// 框选拖拽中（画布坐标 起点, 当前点）
    marquee: Option<(egui::Pos2, egui::Pos2)>,
    offset: egui::Vec2,
    zoom: f32,
    /// 画布 overlay fit 按钮触发：布局后应用 apply_fit
    request_fit: bool,
    /// 连线交互：正在从某节点输出端口拖出（node_id）
    pending_connect: Option<String>,
    /// 节点参数编辑草稿（node_id → 草稿）
    drafts: HashMap<String, NodeDraft>,
    /// 执行选择的输入文件（提交执行时随请求发出）
    exec_input: Option<PathBuf>,
    /// 未保存改动确认对话框是否打开（切换/新建/打开时拦截）
    confirm_switch_open: bool,
    /// 确认对话框通过后要执行的动作
    pending_action: Option<PendingAction>,
    /// 执行对话框（映射表 #11：ExecuteDialog 模态）
    exec_dialog_open: bool,
    /// 执行对话框输入文件文本态
    exec_input_text: String,
    /// 另存为对话框（文件名输入 → config/pipelines/<名>.toml）
    save_as_dialog_open: bool,
    /// 另存为对话框文件名输入态
    save_as_name: String,
    /// 库删除确认对话框目标（仅 custom 管线；Some = 对话框打开）
    delete_target: Option<PathBuf>,
}

impl Default for VizState {
    fn default() -> Self {
        Self {
            file_path: String::new(),
            pipeline: None,
            dirty: false,
            validation_msg: None,
            validation_ok: false,
            positions: HashMap::new(),
            selected: None,
            selected_edge: None,
            multi_select: Vec::new(),
            marquee: None,
            offset: egui::Vec2::ZERO,
            zoom: 1.0,
            request_fit: false,
            pending_connect: None,
            drafts: HashMap::new(),
            exec_input: None,
            confirm_switch_open: false,
            pending_action: None,
            exec_dialog_open: false,
            exec_input_text: String::new(),
            save_as_dialog_open: false,
            save_as_name: String::new(),
            delete_target: None,
        }
    }
}

/// 单个节点的参数编辑草稿（面板 ↔ node.params 的中间态）
#[derive(Clone, Default)]
pub(super) struct NodeDraft {
    label: String,
    /// 0 = 未设置（用默认）
    timeout: u32,
    /// 0 = 未设置
    retry: u32,
    // module 节点
    capability: String,
    /// 变体 pin；空 = 跟随激活变体
    model: String,
    /// 设备绑定；"auto" = 调度器落位
    device: String,
    params: Vec<(String, ParamDraft)>,
    // ffmpeg 节点
    args: Vec<String>,
    args_raw: String,
    /// 原 TOML args 为字符串形状（保持原形状编辑，避免引号语义损失）
    args_is_string: bool,
    output_extension: String,
    // llm 节点（B7 参数表）
    base_url: String,
    llm_model: String,
    api_key_env: String,
    system_prompt: String,
    temperature: f64,
    has_temperature: bool,
    max_tokens: i64,
    has_max_tokens: bool,
    output_format: String,
}

fn sid() -> egui::Id {
    egui::Id::new("pipeline_viz_state")
}

// ── Page entry ────────────────────────────────────────────────────

/// 管线编辑器入口（对齐 WebUI：打开即编辑器）：`cmd_tx` 为后台命令通道
/// （执行对话框经它提交 [`AppCmd`]；None 时执行走"待接线"提示）。
pub fn show_full(
    ui: &mut egui::Ui,
    config: &AppConfig,
    cmd_tx: Option<&UnboundedSender<AppCmd>>,
) {
    let lang = ep_core::i18n::normalize_language(&config.general.language);
    let pal = Palette::new(ui.style().visuals.dark_mode);
    let mut st = ui.data(|d| d.get_temp::<VizState>(sid())).unwrap_or_default();

    // 页面层数据：模块清单缓存 + 设备快照（VRAM 账本）+ 任务快照（状态回显）
    let data = module_data(ui.ctx(), false);
    let devices = device_snapshot(ui.ctx());
    let tasks = tasks_snapshot(ui.ctx());

    // 工具栏常驻（对应 WebUI PipelineToolbar + PipelineLibraryBar）：
    // 库下拉「当前管线」菜单 + 新建 + 打开文件… | 保存 / 验证 / 执行 / 任务
    editor_toolbar(ui, lang, &pal, &mut st, cmd_tx, tasks.as_ref());
    ui.separator();

    match st.pipeline.clone() {
        // 画布空态 + 引导（与 WebUI 空画布一致：无独立库视图）
        None => {
            draw_empty_canvas(ui, lang, &pal);
        }
        Some(pipeline) => {
            if st.positions.is_empty() && !pipeline.nodes.is_empty() {
                st.positions = compute_layout(&pipeline);
            }
            // 草稿生命周期：选中节点无草稿则从节点加载；每帧回写（幂等）
            edit::sync_draft(&mut st, &data);
            let canvas_size = panel::draw_main(
                ui,
                lang,
                &pal,
                &mut st,
                &pipeline,
                config,
                &data,
                devices.as_ref(),
                tasks.as_ref(),
            );
            // fit 请求（画布 overlay ⤢ 按钮触发）：布局后应用
            if st.request_fit {
                canvas::apply_fit(&mut st, canvas_size);
                st.request_fit = false;
            }
        }
    }

    // ── 模态对话框 ──
    // 未保存改动确认（dirty 拦截：切换/新建/打开将丢弃改动）
    if st.confirm_switch_open {
        let res = confirm_dialog_with_lang(
            ui.ctx(),
            &pal,
            "pe_unsaved_confirm",
            &trfb(lang, "desktopApp.pipeline.unsavedTitle", "未保存的改动", &[]),
            &trfb(
                lang,
                "desktopApp.pipeline.unsavedMsg",
                "管线有未保存的改动，继续操作将丢弃这些改动。",
                &[],
            ),
            &trfb(lang, "desktopApp.pipeline.unsavedConfirm", "丢弃并继续", &[]),
            true,
            lang,
        );
        if let Some(confirmed) = res {
            st.confirm_switch_open = false;
            let action = st.pending_action.take();
            if confirmed {
                apply_pending_action(&mut st, lang, action);
            }
        }
    }
    // 执行对话框（映射表 #11：校验通过后选择输入文件 + VRAM 阻断校验）
    if st.exec_dialog_open {
        exec_dialog(ui, lang, &pal, &mut st, config, &data, devices.as_ref(), cmd_tx);
    }
    // 另存为对话框（文件名输入 → config/pipelines/，重名自动 _2/_3）
    if st.save_as_dialog_open {
        save_as_dialog(ui, lang, &pal, &mut st);
    }
    // 库删除确认对话框（仅 custom 管线可删，shipped 菜单侧已隐藏入口）
    if let Some(target) = st.delete_target.clone() {
        let name = target
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let res = confirm_dialog_with_lang(
            ui.ctx(),
            &pal,
            "pe_delete_pipeline",
            &trfb(lang, "desktopApp.pipeline.deleteTitle", "删除管线", &[]),
            &trfb(
                lang,
                "desktopApp.pipeline.deleteMsg",
                "从管线库删除「{{name}}」？此操作不可撤销。",
                &[("name", &name)],
            ),
            &trfb(lang, "desktopApp.pipeline.deleteConfirm", "删除", &[]),
            true,
            lang,
        );
        if let Some(confirmed) = res {
            st.delete_target = None;
            if confirmed {
                delete_library_entry(&mut st, lang, &target);
            }
        }
    }

    // Status bar
    ui.separator();
    match &st.validation_msg {
        Some(msg) if st.validation_ok => {
            ui.colored_label(pal.success, msg.as_str());
        }
        Some(msg) => {
            ui.colored_label(pal.status_error, msg.as_str());
        }
        None => {
            ui.colored_label(pal.text_dim, tr(lang, "desktopApp.pipeline.statusReady", &[]));
        }
    }

    // Persist
    ui.data_mut(|d| *d.get_temp_mut_or_default::<VizState>(sid()) = st);
}

/// 画布空态 + 引导（无管线时）：对齐 WebUI 空画布形态。
fn draw_empty_canvas(ui: &mut egui::Ui, lang: &str, pal: &Palette) {
    let avail = ui.available_size();
    ui.allocate_ui(avail, |ui| {
        ui.vertical_centered(|ui| {
            // 垂直居中（empty_state 自带 ~69pt 内容高度，按半高上移）
            ui.add_space((avail.y / 2.0 - 70.0).max(8.0));
            empty_state(
                ui,
                pal,
                "🧩",
                &tr(lang, "desktopApp.pipeline.emptyTitle", &[]),
                &tr(lang, "desktopApp.pipeline.emptyHintEdit", &[]),
            );
        });
    });
}

/// 执行确认通过的待决动作（切换加载 / 新建 / 打开文件 / 加载示例）
fn apply_pending_action(st: &mut VizState, lang: &str, action: Option<PendingAction>) {
    match action {
        Some(PendingAction::Switch(path)) => load_library_entry(st, lang, &path),
        Some(PendingAction::New) => edit::new_pipeline(st),
        Some(PendingAction::Open) => open_file_dialog(st, lang),
        Some(PendingAction::LoadExample) => load_example(st, lang),
        None => {}
    }
}

/// dirty 守卫：无未保存改动直接执行动作；否则弹确认对话框
fn request_action(st: &mut VizState, lang: &str, action: PendingAction) {
    if st.dirty {
        st.pending_action = Some(action);
        st.confirm_switch_open = true;
    } else {
        apply_pending_action(st, lang, Some(action));
    }
}

/// 加载库条目（路径 → file_path → 既有加载流程）
fn load_library_entry(st: &mut VizState, lang: &str, path: &std::path::Path) {
    st.file_path = path.to_string_lossy().to_string();
    edit::load_pipeline(st, lang);
}

/// 「打开文件…」系统文件选择器（TOML 过滤）
fn open_file_dialog(st: &mut VizState, lang: &str) {
    if let Some(file) = rfd::FileDialog::new()
        .set_title(trfb(
            lang,
            "desktopApp.pipeline.openTitle",
            "打开管线 TOML",
            &[],
        ))
        .add_filter("TOML", &["toml"])
        .pick_file()
    {
        load_library_entry(st, lang, &file);
    }
}

// ── Editor toolbar ────────────────────────────────────────────────

/// 编辑器工具栏（对齐 WebUI PipelineToolbar + PipelineLibraryBar）：
/// 库下拉「当前管线」菜单 | 新建 | 打开文件… | 管线名(+dirty/任务徽章)
/// | 保存 | 验证 | ⚡执行(primary 辉光) | 任务刷新。
/// 缩放 −/＋/fit 在画布 overlay。
fn editor_toolbar(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    st: &mut VizState,
    cmd_tx: Option<&UnboundedSender<AppCmd>>,
    tasks: Option<&crate::pages::TasksSnapshot>,
) {
    // 窄窗降级（P1 防御）：内容区 <900px 时折叠次要元素（文件路径/任务
    // 徽章与进度/任务按钮文案），防止 horizontal 工具栏重叠
    let narrow = ui.available_width() < 900.0;
    ui.horizontal(|ui| {
        // 库下拉菜单（PipelineLibraryBar 等价）：列出 config/pipelines/*.toml，
        // 切换加载，标注当前项；dirty 时经确认对话框拦截
        library_menu(ui, lang, st);
        ui.separator();

        // 新建（dirty 守卫）
        if ui
            .add(subtle_button(
                pal,
                format!(
                    "＋ {}",
                    trfb(lang, "desktopApp.pipeline.libraryNew", "新建管线", &[])
                ),
            ))
            .on_hover_text(trfb(
                lang,
                "desktopApp.pipeline.newTip",
                "新建含输入/输出的空白管线",
                &[],
            ))
            .clicked()
        {
            request_action(st, lang, PendingAction::New);
        }
        // 打开文件…（dirty 守卫）
        if ui
            .add(subtle_button(
                pal,
                format!(
                    "📂 {}",
                    trfb(lang, "desktopApp.pipeline.libraryOpen", "打开文件…", &[])
                ),
            ))
            .on_hover_text(trfb(
                lang,
                "desktopApp.pipeline.openTitle",
                "打开管线 TOML",
                &[],
            ))
            .clicked()
        {
            request_action(st, lang, PendingAction::Open);
        }
        ui.separator();

        // 管线名 + dirty 标记 + 文件路径（弱化；窄窗时折叠徽章/路径）
        if let Some(p) = &st.pipeline {
            ui.strong(format!(
                "{}{}",
                p.name.if_empty_fallback(&p.id),
                if st.dirty { " *" } else { "" }
            ));
            // 任务状态徽章（任务快照回显；窄窗折叠）
            if !narrow {
                if let Some(task) = latest_task_for(p, tasks) {
                    let (color, label) = task_status_badge(lang, pal, &task.status);
                    badge(ui, pal, color, label);
                    let progress = format!("{}/{}", task.completed_nodes, task.node_count);
                    ui.label(egui::RichText::new(progress).small().color(pal.text_dim));
                }
            }
            if !narrow && !st.file_path.is_empty() {
                ui.label(
                    egui::RichText::new(abbreviate_path(&st.file_path, 32))
                        .monospace()
                        .small()
                        .color(pal.text_faint),
                )
                .on_hover_text(st.file_path.as_str());
            }
        }

        // 右侧：保存 / 验证 / 执行 / 任务
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let pipeline_snap = st.pipeline.clone();
            if let Some(ref p) = pipeline_snap {
                // 刷新该管线的任务列表（§6.8；窄窗仅图标）
                if ui
                    .add(subtle_button(
                        pal,
                        if narrow {
                            "🔄".to_string()
                        } else {
                            format!(
                                "🔄 {}",
                                trfb(lang, "desktopApp.pipeline.refreshTasks", "任务", &[])
                            )
                        },
                    ))
                    .on_hover_text(trfb(
                        lang,
                        "desktopApp.pipeline.refreshTasksTip",
                        "拉取该管线的任务列表",
                        &[],
                    ))
                    .clicked()
                {
                    if let Some(tx) = cmd_tx {
                        let _ = tx.send(AppCmd::RefreshPipelineTasks {
                            pipeline_id: p.id.clone(),
                        });
                    }
                }
                // 执行（primary 辉光；先校验 → 通过开执行对话框）
                if primary_button_with_glow(
                    ui,
                    pal,
                    format!(
                        "⚡ {}",
                        trfb(lang, "desktopApp.pipeline.run", "执行", &[])
                    ),
                )
                .on_hover_text(trfb(
                    lang,
                    "desktopApp.pipeline.runTip",
                    "校验通过后选择输入文件并提交执行",
                    &[],
                ))
                .clicked()
                {
                    match p.validate() {
                        Ok(()) => {
                            st.validation_msg =
                                Some(tr(lang, "desktopApp.pipeline.validationPassed", &[]));
                            st.validation_ok = true;
                            st.exec_input_text = st
                                .exec_input
                                .as_ref()
                                .map(|p| p.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            st.exec_dialog_open = true;
                        }
                        Err(errors) => {
                            st.validation_msg = Some(format_errors(&errors));
                            st.validation_ok = false;
                        }
                    }
                }
            }
            if ui
                .add(subtle_button(pal, tr(lang, "desktopApp.pipeline.validate", &[])))
                .clicked()
            {
                edit::validate_pipeline(st, lang);
            }
            if ui
                .add(subtle_button(
                    pal,
                    format!(
                        "{}{}",
                        if st.dirty { "*" } else { "" },
                        trfb(lang, "desktopApp.pipeline.saveToml2", "保存", &[])
                    ),
                ))
                .on_hover_text(trfb(
                    lang,
                    "desktopApp.pipeline.saveTip",
                    "序列化为管线 TOML（§6.2 形状）写回文件",
                    &[],
                ))
                .clicked()
            {
                edit::save_pipeline(st, lang, false);
            }
        });
    });
}

/// 管线库下拉菜单（WebUI `PipelineLibraryBar` 下拉的 egui 等价实现）：
/// 扫描 `config/pipelines/*.toml`（名称 + 节点数），点击切换加载（dirty
/// 守卫），✓ 标注当前管线；shipped 条目打内置标、不显示删除；custom
/// 条目行尾带 🗑 删除（确认对话框）。菜单底部：加载示例 / 另存为… /
/// 导出… / 导入注册…（对齐 WebUI 行为语义）；库为空时给出目录提示。
fn library_menu(ui: &mut egui::Ui, lang: &str, st: &mut VizState) {
    // 触发器文案：当前管线名（+dirty *）；无管线时显示「管线库」
    let selected_text = match &st.pipeline {
        Some(p) => format!(
            "📂 {}{}",
            p.name.if_empty_fallback(&p.id),
            if st.dirty { " *" } else { "" }
        ),
        None => format!(
            "📂 {}",
            trfb(lang, "desktopApp.pipeline.libraryTitle", "管线库", &[])
        ),
    };

    let mut chosen: Option<std::path::PathBuf> = None;
    let mut delete_req: Option<std::path::PathBuf> = None;
    let mut action: Option<LibraryMenuAction> = None;
    let combo = egui::ComboBox::from_id_salt(egui::Id::new("pe_library_menu"))
        .selected_text(selected_text)
        .show_ui(ui, |ui| {
            ui.set_min_width(320.0);
            let entries =
                library::scan_pipeline_library(&library::pipeline_library_dir());
            if entries.is_empty() {
                ui.label(
                    egui::RichText::new(trfb(
                        lang,
                        "desktopApp.pipeline.libraryMenuEmpty",
                        "config/pipelines 下暂无 .toml 管线",
                        &[],
                    ))
                    .small()
                    .weak(),
                );
            } else {
                for entry in &entries {
                    let entry_path = entry.path.to_string_lossy().into_owned();
                    let is_current = !st.file_path.is_empty() && st.file_path == entry_path;
                    let count = trfb(
                        lang,
                        "desktopApp.pipeline.nodeCount",
                        "{{count}} 节点",
                        &[("count", &entry.node_count.to_string())],
                    );
                    let shipped_tag = if entry.shipped {
                        format!(
                            " · {}",
                            trfb(lang, "desktopApp.pipeline.shippedTag", "内置", &[])
                        )
                    } else {
                        String::new()
                    };
                    let label = format!(
                        "{}{} · {}{}",
                        if is_current { "✓ " } else { "" },
                        entry.name,
                        count,
                        shipped_tag
                    );
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let resp = ui.selectable_label(false, label);
                        let resp = if !entry.description.is_empty() {
                            resp.on_hover_text(entry.description.as_str())
                        } else {
                            resp
                        };
                        if resp.clicked() && !is_current {
                            chosen = Some(entry.path.clone());
                        }
                        // 删除仅 custom（非 shipped）可见（对齐 WebUI：内置不显示删除）
                        if !entry.shipped
                            && ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("🗑").size(11.0),
                                    )
                                    .frame(false),
                                )
                                .on_hover_text(trfb(
                                    lang,
                                    "desktopApp.pipeline.libraryDeleteTip",
                                    "从管线库删除该自定义管线",
                                    &[],
                                ))
                                .clicked()
                        {
                            delete_req = Some(entry.path.clone());
                        }
                    });
                }
            }
            ui.separator();
            // 加载示例（dirty 守卫在 apply 侧统一拦截）
            if ui
                .button(format!(
                    "✨ {}",
                    trfb(lang, "desktopApp.pipeline.libraryLoadExample", "加载示例", &[])
                ))
                .clicked()
            {
                action = Some(LibraryMenuAction::LoadExample);
            }
            // 另存为 / 导出：需已加载管线（对齐 WebUI canExport 语义）
            ui.add_enabled_ui(st.pipeline.is_some(), |ui| {
                if ui
                    .button(format!(
                        "📝 {}",
                        trfb(lang, "desktopApp.pipeline.librarySaveAs", "另存为…", &[])
                    ))
                    .clicked()
                {
                    action = Some(LibraryMenuAction::SaveAs);
                }
                if ui
                    .button(format!(
                        "⬇ {}",
                        trfb(lang, "desktopApp.pipeline.libraryExport", "导出…", &[])
                    ))
                    .clicked()
                {
                    action = Some(LibraryMenuAction::Export);
                }
            });
            if ui
                .button(format!(
                    "⬆ {}",
                    trfb(lang, "desktopApp.pipeline.libraryImport", "导入注册…", &[])
                ))
                .clicked()
            {
                action = Some(LibraryMenuAction::Import);
            }
        });
    combo
        .response
        .on_hover_text(trfb(
            lang,
            "desktopApp.pipeline.libraryMenuTip",
            "切换管线库中的管线（未保存改动需确认）",
            &[],
        ));
    if let Some(path) = chosen {
        request_action(st, lang, PendingAction::Switch(path));
    }
    if let Some(path) = delete_req {
        st.delete_target = Some(path);
    }
    match action {
        Some(LibraryMenuAction::LoadExample) => {
            request_action(st, lang, PendingAction::LoadExample);
        }
        Some(LibraryMenuAction::SaveAs) => {
            st.save_as_name = default_save_as_name(st);
            st.save_as_dialog_open = true;
        }
        Some(LibraryMenuAction::Export) => export_current_pipeline(st, lang),
        Some(LibraryMenuAction::Import) => import_register(st, lang),
        None => {}
    }
}

// ── Library capabilities（对齐 WebUI PipelineLibraryBar 行为语义） ────

/// 加载示例：复用新建逻辑生成示例 DAG（file_input → file_output），
/// 带示例名/描述与稳定 id（对齐 WebUI onLoadExample）。
fn load_example(st: &mut VizState, lang: &str) {
    let name = trfb(lang, "desktopApp.pipeline.exampleName", "示例管线", &[]);
    let desc = trfb(
        lang,
        "desktopApp.pipeline.exampleDesc",
        "内置模板生成的示例管线：文件输入 → 文件输出",
        &[],
    );
    edit::new_pipeline_with(st, Some(("example-pipeline", name, desc)));
    st.validation_msg = Some(trfb(
        lang,
        "desktopApp.pipeline.exampleLoaded",
        "已生成示例管线，可经「另存为」注册进管线库",
        &[],
    ));
    st.validation_ok = true;
}

/// 另存为默认文件名：当前文件 stem → 管线 id → "pipeline"
fn default_save_as_name(st: &VizState) -> String {
    if !st.file_path.trim().is_empty() {
        if let Some(stem) = std::path::Path::new(st.file_path.trim()).file_stem() {
            return stem.to_string_lossy().into_owned();
        }
    }
    st.pipeline
        .as_ref()
        .map(|p| {
            if p.id.trim().is_empty() {
                "pipeline".to_string()
            } else {
                p.id.clone()
            }
        })
        .unwrap_or_else(|| "pipeline".to_string())
}

/// 另存为对话框（模态）：文件名输入 → config/pipelines/<名>.toml；
/// 重名实时提示（自动 _2/_3 改名，不阻断）。
fn save_as_dialog(ui: &mut egui::Ui, lang: &str, pal: &Palette, st: &mut VizState) {
    let mut open = true;
    let mut do_save = false;
    // 闭包内不直接改 open（与 .open(&mut open) 借用冲突）：用独立标志回收
    let mut close_req = false;
    egui::Window::new(trfb(
        lang,
        "desktopApp.pipeline.saveAsTitle",
        "另存为到管线库",
        &[],
    ))
    .id(egui::Id::new("pe_save_as_dialog"))
    .collapsible(false)
    .resizable(false)
    .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
    .open(&mut open)
    .show(ui.ctx(), |ui| {
        ui.set_min_width(360.0);
        ui.label(
            egui::RichText::new(trfb(
                lang,
                "desktopApp.pipeline.saveAsNameLabel",
                "文件名",
                &[],
            ))
            .color(pal.text_dim),
        );
        ui.add(
            egui::TextEdit::singleline(&mut st.save_as_name)
                .desired_width(300.0)
                .hint_text("my-pipeline"),
        );
        ui.label(
            egui::RichText::new(trfb(
                lang,
                "desktopApp.pipeline.saveAsNameHint",
                "保存为 config/pipelines/<文件名>.toml",
                &[],
            ))
            .small()
            .color(pal.text_faint),
        );
        // 重名处理：实时预览最终文件名（冲突 → _2/_3）
        let input_empty = st.save_as_name.trim().is_empty();
        if !input_empty {
            let dir = library::pipeline_library_dir();
            let sanitized = library::sanitize_library_file_name(&st.save_as_name);
            let final_name =
                library::unique_library_file_name(&sanitized, &library::existing_stems(&dir));
            if final_name != sanitized {
                ui.label(
                    egui::RichText::new(trfb(
                        lang,
                        "desktopApp.pipeline.saveAsConflict",
                        "已存在同名管线，将保存为 {{name}}.toml",
                        &[("name", &final_name)],
                    ))
                    .small()
                    .color(pal.warning),
                );
            }
        } else {
            ui.label(
                egui::RichText::new(trfb(
                    lang,
                    "desktopApp.pipeline.saveAsEmpty",
                    "文件名不能为空",
                    &[],
                ))
                .small()
                .color(pal.status_error),
            );
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(subtle_button(pal, tr(lang, "common.action.cancel", &[])))
                    .clicked()
                {
                    close_req = true;
                }
                let resp = primary_button_with_glow(
                    ui,
                    pal,
                    tr(lang, "common.action.save", &[]),
                );
                if resp.clicked() && !input_empty {
                    do_save = true;
                }
            });
        });
    });
    if do_save {
        st.save_as_dialog_open = false;
        save_as_to_library(st, lang);
    } else if close_req || !open {
        st.save_as_dialog_open = false;
    }
}

/// 另存为落盘：序列化（含画布坐标）→ config/pipelines/<重名处理后>.toml，
/// 成功后当前文件指向新库条目（后续保存写回新文件）。
fn save_as_to_library(st: &mut VizState, lang: &str) {
    let Some(p) = edit::pipeline_with_positions(st) else {
        return;
    };
    let dir = library::pipeline_library_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        st.validation_msg = Some(trfb(
            lang,
            "desktopApp.pipeline.saveAsFailed",
            "另存失败: {{detail}}",
            &[("detail", &e.to_string())],
        ));
        st.validation_ok = false;
        return;
    }
    let sanitized = library::sanitize_library_file_name(&st.save_as_name);
    let final_name = library::unique_library_file_name(&sanitized, &library::existing_stems(&dir));
    let path = dir.join(format!("{final_name}.toml"));
    match toml_serde::pipeline_to_toml(&p) {
        Ok(text) => match std::fs::write(&path, text) {
            Ok(()) => {
                st.file_path = path.to_string_lossy().into_owned();
                st.dirty = false;
                st.validation_msg = Some(trfb(
                    lang,
                    "desktopApp.pipeline.savedAsDone",
                    "已另存为: {{path}}",
                    &[("path", &path.to_string_lossy())],
                ));
                st.validation_ok = true;
            }
            Err(e) => {
                st.validation_msg = Some(trfb(
                    lang,
                    "desktopApp.pipeline.saveAsFailed",
                    "另存失败: {{detail}}",
                    &[("detail", &e.to_string())],
                ));
                st.validation_ok = false;
            }
        },
        Err(e) => {
            st.validation_msg = Some(trfb(
                lang,
                "desktopApp.pipeline.saveSerializeFailed",
                "序列化失败: {{detail}}",
                &[("detail", &e)],
            ));
            st.validation_ok = false;
        }
    }
}

/// 导出：当前管线 TOML 保存到用户选择的路径（rfd 保存对话框）；
/// 不改变当前文件指向（对齐 WebUI 导出语义）。
fn export_current_pipeline(st: &mut VizState, lang: &str) {
    let Some(p) = edit::pipeline_with_positions(st) else {
        st.validation_msg = Some(tr(lang, "desktopApp.pipeline.loadFileFirst", &[]));
        st.validation_ok = false;
        return;
    };
    let default_name = if !st.file_path.trim().is_empty() {
        std::path::Path::new(st.file_path.trim())
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("{}.toml", p.id))
    } else {
        format!("{}.toml", p.id)
    };
    let Some(path) = rfd::FileDialog::new()
        .set_title(trfb(
            lang,
            "desktopApp.pipeline.exportTitle",
            "导出管线 TOML",
            &[],
        ))
        .add_filter("TOML", &["toml"])
        .set_file_name(&default_name)
        .save_file()
    else {
        return;
    };
    match toml_serde::pipeline_to_toml(&p) {
        Ok(text) => match std::fs::write(&path, text) {
            Ok(()) => {
                st.validation_msg = Some(trfb(
                    lang,
                    "desktopApp.pipeline.exportDone",
                    "已导出: {{path}}",
                    &[("path", &path.to_string_lossy())],
                ));
                st.validation_ok = true;
            }
            Err(e) => {
                st.validation_msg = Some(trfb(
                    lang,
                    "desktopApp.pipeline.exportFailed",
                    "导出失败: {{detail}}",
                    &[("detail", &e.to_string())],
                ));
                st.validation_ok = false;
            }
        },
        Err(e) => {
            st.validation_msg = Some(trfb(
                lang,
                "desktopApp.pipeline.saveSerializeFailed",
                "序列化失败: {{detail}}",
                &[("detail", &e)],
            ));
            st.validation_ok = false;
        }
    }
}

/// 导入注册：选 TOML → 解析校验 → 复制进 config/pipelines/（文件名冲突
/// 自动改名并提示）→ 加载注册后的库条目。
fn import_register(st: &mut VizState, lang: &str) {
    let Some(src) = rfd::FileDialog::new()
        .set_title(trfb(
            lang,
            "desktopApp.pipeline.importTitle",
            "选择要导入的管线 TOML",
            &[],
        ))
        .add_filter("TOML", &["toml"])
        .pick_file()
    else {
        return;
    };
    // 解析校验：只有合法管线 TOML 才注册（对齐 WebUI 导入校验语义）
    if let Err(e) = Pipeline::from_toml(&src) {
        st.validation_msg = Some(trfb(
            lang,
            "desktopApp.pipeline.importFailed",
            "导入失败: {{detail}}",
            &[("detail", &e.to_string())],
        ));
        st.validation_ok = false;
        return;
    }
    let dir = library::pipeline_library_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        st.validation_msg = Some(trfb(
            lang,
            "desktopApp.pipeline.importFailed",
            "导入失败: {{detail}}",
            &[("detail", &e.to_string())],
        ));
        st.validation_ok = false;
        return;
    }
    let raw_stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "pipeline".to_string());
    let sanitized = library::sanitize_library_file_name(&raw_stem);
    let final_name = library::unique_library_file_name(&sanitized, &library::existing_stems(&dir));
    let dest = dir.join(format!("{final_name}.toml"));
    match std::fs::copy(&src, &dest) {
        Ok(_) => {
            let conflict_note = if final_name != sanitized {
                format!(
                    " ({})",
                    trfb(
                        lang,
                        "desktopApp.pipeline.saveAsConflictShort",
                        "重名已改名为 {{name}}.toml",
                        &[("name", &final_name)],
                    )
                )
            } else {
                String::new()
            };
            st.validation_msg = Some(format!(
                "{}{}",
                trfb(
                    lang,
                    "desktopApp.pipeline.importDone",
                    "已导入注册: {{path}}",
                    &[("path", &dest.to_string_lossy())],
                ),
                conflict_note
            ));
            st.validation_ok = true;
            load_library_entry(st, lang, &dest);
        }
        Err(e) => {
            st.validation_msg = Some(trfb(
                lang,
                "desktopApp.pipeline.importFailed",
                "导入失败: {{detail}}",
                &[("detail", &e.to_string())],
            ));
            st.validation_ok = false;
        }
    }
}

/// 库删除（确认后执行）：shipped 双保险拦截；删除当前文件时清指向并
/// 将内存内容置为未保存（避免静默丢失）。
fn delete_library_entry(st: &mut VizState, lang: &str, path: &std::path::Path) {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if library::is_shipped_stem(&name) {
        st.validation_msg = Some(trfb(
            lang,
            "desktopApp.pipeline.deleteShipped",
            "内置管线不可删除",
            &[],
        ));
        st.validation_ok = false;
        return;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {
            if !st.file_path.is_empty() && std::path::Path::new(&st.file_path) == path {
                st.file_path.clear();
                st.dirty = true;
            }
            st.validation_msg = Some(trfb(
                lang,
                "desktopApp.pipeline.deleteDone",
                "已删除: {{name}}",
                &[("name", &name)],
            ));
            st.validation_ok = true;
        }
        Err(e) => {
            st.validation_msg = Some(trfb(
                lang,
                "desktopApp.pipeline.deleteFailed",
                "删除失败: {{detail}}",
                &[("detail", &e.to_string())],
            ));
            st.validation_ok = false;
        }
    }
}

/// 路径缩略（hover 展示全路径）
fn abbreviate_path(path: &str, max: usize) -> String {
    if path.chars().count() <= max {
        return path.to_string();
    }
    format!("…{}", path.chars().skip(path.chars().count() - max + 1).collect::<String>())
}

/// 字符串空回退辅助（管线名为空显示 id）
trait IfEmptyFallback {
    fn if_empty_fallback<'a>(&'a self, fallback: &'a str) -> &'a str;
}
impl IfEmptyFallback for String {
    fn if_empty_fallback<'a>(&'a self, fallback: &'a str) -> &'a str {
        if self.trim().is_empty() {
            fallback
        } else {
            self.as_str()
        }
    }
}

// ── Execute dialog（映射表 #11） ──────────────────────────────────

/// 执行对话框（模态）：输入文件选择（file_input 管线必填）+ VRAM 超预算
/// 阻断（allow_overcommit=false 时）+ 提交（既有三分支语义不变）。
#[allow(clippy::too_many_arguments)]
fn exec_dialog(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    st: &mut VizState,
    config: &AppConfig,
    data: &ModuleData,
    devices: Option<&crate::pages::DeviceSnapshot>,
    cmd_tx: Option<&UnboundedSender<AppCmd>>,
) {
    let Some(pipeline) = st.pipeline.clone() else {
        st.exec_dialog_open = false;
        return;
    };

    let mut open = true;
    let mut submit = false;
    let mut cancel = false;
    egui::Window::new(trfb(lang, "desktopApp.pipeline.execTitle", "执行管线", &[]))
        .id(egui::Id::new("pe_exec_dialog"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .open(&mut open)
        .show(ui.ctx(), |ui| {
            ui.set_min_width(360.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(trfb(lang, "common.label.name2", "名称:", &[])).color(pal.text_dim));
                ui.strong(pipeline.name.if_empty_fallback(&pipeline.id));
            });

            // 输入文件（file_input 节点存在则必填）
            let need_input = edit::has_file_input(&pipeline);
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(trfb(
                    lang,
                    "desktopApp.pipeline.execInputLabel",
                    "输入文件:",
                    &[],
                ))
                .color(pal.text_dim),
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut st.exec_input_text)
                        .desired_width(260.0)
                        .hint_text(trfb(
                            lang,
                            "desktopApp.pipeline.execInputHint",
                            "执行期输入文件路径",
                            &[],
                        )),
                );
                if ui
                    .add(subtle_button(
                        pal,
                        trfb(lang, "desktopApp.pipeline.execPickFile", "浏览…", &[]),
                    ))
                    .clicked()
                {
                    if let Some(file) = rfd::FileDialog::new()
                        .set_title(trfb(
                            lang,
                            "desktopApp.pipeline.pickInput",
                            "选择执行输入文件",
                            &[],
                        ))
                        .pick_file()
                    {
                        st.exec_input_text = file.to_string_lossy().into_owned();
                    }
                }
            });

            // 校验链：file_input 必填 → VRAM 超预算阻断
            let input_trimmed = st.exec_input_text.trim().to_string();
            let input_missing = need_input && input_trimmed.is_empty();
            if input_missing {
                ui.label(
                    egui::RichText::new(trfb(
                        lang,
                        "desktopApp.pipeline.execNeedInput",
                        "该管线含 file_input 节点，必须选择输入文件",
                        &[],
                    ))
                    .small()
                    .color(pal.warning),
                );
            }
            let vram_blocked = match panel::compute_vram_report(&pipeline, config, data, devices) {
                Ok(report) => {
                    !config.compute.allow_overcommit
                        && report.devices.iter().any(|d| d.over)
                }
                Err(_) => false, // 环等错误已由 validate 拦截，这里不重复阻断
            };
            if vram_blocked {
                ui.label(
                    egui::RichText::new(trfb(
                        lang,
                        "desktopApp.pipeline.execVramBlocked",
                        "VRAM 超预算且 allow_overcommit=false，执行被阻断（见右侧账本）",
                        &[],
                    ))
                    .small()
                    .color(pal.danger),
                );
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(subtle_button(pal, tr(lang, "common.action.cancel", &[])))
                        .clicked()
                    {
                        cancel = true;
                    }
                    let submit_btn = primary_button_with_glow(
                        ui,
                        pal,
                        format!(
                            "⚡ {}",
                            trfb(lang, "desktopApp.pipeline.execSubmit", "提交执行", &[])
                        ),
                    );
                    let resp = if input_missing || vram_blocked {
                        submit_btn.on_hover_text(trfb(
                            lang,
                            "desktopApp.pipeline.execBlockedTip",
                            "先解决上方校验问题",
                            &[],
                        ))
                    } else {
                        submit_btn
                    };
                    if resp.clicked() && !input_missing && !vram_blocked {
                        submit = true;
                    }
                });
            });
        });

    if cancel {
        open = false;
    }
    if submit {
        st.exec_dialog_open = false;
        let input = std::path::PathBuf::from(st.exec_input_text.trim());
        edit::submit_execution(st, lang, cmd_tx, pipeline, &input);
    } else if !open {
        st.exec_dialog_open = false;
    }
}

// ── Task echo（节点状态回显） ─────────────────────────────────────

/// 任务快照中属于指定管线的最新任务（ISO 时间串字典序 = 时间序）
fn latest_task_for(
    pipeline: &Pipeline,
    tasks: Option<&crate::pages::TasksSnapshot>,
) -> Option<ep_core::pipeline::runner::TaskSummary> {
    tasks?
        .tasks
        .iter()
        .filter(|t| t.pipeline_name == pipeline.id)
        .max_by(|a, b| a.started_at.cmp(&b.started_at))
        .cloned()
}

fn task_status_badge(
    lang: &str,
    pal: &Palette,
    status: &TaskStatus,
) -> (egui::Color32, String) {
    match status {
        TaskStatus::Completed => (pal.status_ready, tr(lang, "common.status.completed", &[])),
        TaskStatus::Running => (pal.status_running, tr(lang, "common.status.running", &[])),
        TaskStatus::Pending => (
            pal.warning,
            trfb(lang, "common.status.queued", "排队中", &[]),
        ),
        TaskStatus::Failed(_) => (pal.status_error, tr(lang, "common.status.failed", &[])),
        TaskStatus::Cancelled => (pal.status_stopped, tr(lang, "common.status.cancelled", &[])),
    }
}

/// 按最新任务回显节点状态色：Completed → 全绿；Running → 前段绿 + 当前
/// 运行青（呼吸辉光）；Failed → 下一个红（近似，TaskSummary 不携带逐节点
/// 状态，接线 TaskDetail 后可精确化）。
fn node_echo_colors(
    lang: &str,
    pal: &Palette,
    pipeline: &Pipeline,
    tasks: Option<&crate::pages::TasksSnapshot>,
) -> HashMap<String, egui::Color32> {
    let _ = lang;
    let mut colors = HashMap::new();
    let Some(task) = latest_task_for(pipeline, tasks) else {
        return colors;
    };
    let order: Vec<String> = pipeline
        .topological_layers()
        .map(|layers| layers.into_iter().flatten().collect())
        .unwrap_or_else(|_| pipeline.nodes.iter().map(|n| n.id.clone()).collect());

    match &task.status {
        TaskStatus::Completed => {
            for id in &order {
                colors.insert(id.clone(), pal.status_ready);
            }
        }
        TaskStatus::Running => {
            for (i, id) in order.iter().enumerate() {
                if i < task.completed_nodes {
                    colors.insert(id.clone(), pal.status_ready);
                } else if i == task.completed_nodes {
                    colors.insert(id.clone(), pal.status_running);
                }
            }
        }
        TaskStatus::Failed(_) | TaskStatus::Cancelled => {
            for (i, id) in order.iter().enumerate() {
                if i < task.completed_nodes {
                    colors.insert(id.clone(), pal.status_ready);
                } else if i == task.completed_nodes && matches!(task.status, TaskStatus::Failed(_))
                {
                    colors.insert(id.clone(), pal.status_error);
                }
            }
        }
        TaskStatus::Pending => {}
    }
    colors
}

// ── Auto layout ───────────────────────────────────────────────────

pub(super) fn compute_layout(pipeline: &Pipeline) -> HashMap<String, egui::Pos2> {
    let mut pos = HashMap::new();

    // Use stored positions if every node has one
    if pipeline.nodes.iter().all(|n| n.position.is_some()) {
        for n in &pipeline.nodes {
            if let Some(p) = &n.position {
                pos.insert(n.id.clone(), egui::pos2(p.x as f32, p.y as f32));
            }
        }
        return pos;
    }

    // Topological layers → columns
    let layers = pipeline.topological_layers().unwrap_or_else(|_| {
        vec![pipeline.nodes.iter().map(|n| n.id.clone()).collect()]
    });

    for (col, layer) in layers.iter().enumerate() {
        let x = 40.0 + col as f32 * LAYER_GAP;
        for (row, id) in layer.iter().enumerate() {
            let y = 40.0 + row as f32 * NODE_GAP;
            pos.insert(id.clone(), egui::pos2(x, y));
        }
    }
    pos
}

// ── Helpers ───────────────────────────────────────────────────────

/// 节点类型 → (本地化类型名, 详情)。详情内容为模块/模型 ID 与端点等数据，不翻译。
pub(super) fn node_kind_info(lang: &str, kind: &NodeKind) -> (String, String) {
    match kind {
        NodeKind::Module {
            module_id,
            capability,
            model_id,
            device,
        } => (
            tr(lang, "common.label.module", &[]),
            format!(
                "{}::{}{}{}",
                module_id,
                capability,
                model_id
                    .as_ref()
                    .map(|m| format!(" (model: {m})"))
                    .unwrap_or_default(),
                device
                    .as_ref()
                    .map(|d| format!(" [device: {d}]"))
                    .unwrap_or_default()
            ),
        ),
        NodeKind::Builtin { builtin } => {
            (tr(lang, "desktopApp.pipeline.kindBuiltin", &[]), builtin.clone())
        }
        NodeKind::ExternalApi { endpoint, .. } => {
            ("API".to_string(), format!("llm: {endpoint}"))
        }
    }
}

/// 节点标题栏类型色：模块=pal.primary，内置/API 为文件内命名常量。
pub(super) fn node_kind_color(pal: &Palette, kind: &NodeKind) -> egui::Color32 {
    match kind {
        NodeKind::Module { .. } => pal.primary,
        NodeKind::Builtin { .. } => NODE_COLOR_BUILTIN,
        NodeKind::ExternalApi { .. } => NODE_COLOR_API,
    }
}

pub(super) fn format_errors(errors: &[ep_core::pipeline::dag::ValidationError]) -> String {
    errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}
