//! 管线编辑器 — 两态重做（统一 UI 方案 §7.3，W3 波次）。
//!
//! ## 两态信息架构
//!
//! - **库视图**（默认态 [`PageMode::Library`]）：管线库卡片网格 + 新建/打开
//!   入口（`library.rs`；自 Task #26 空态列表升格）；
//! - **编辑器视图**（[`PageMode::Editor`]）：工具栏（返回库入口 + 保存/验证/
//!   执行）+ 三栏（palette | 画布 | 参数面板），画布含缩放控件/MiniMap 等
//!   5 项 egui 自研替代（`canvas.rs`）。
//!
//! ## 模块拆分（自单文件 pipeline_editor.rs 纯搬移）
//!
//! - [`canvas`]：画布交互与绘制（缩放锚定/框选/MiniMap/拖放落点/视觉升级）；
//! - [`panel`]：palette（拖放载荷源）+ 右侧参数面板 + 节点编辑器 + VRAM 账本；
//! - [`library`]：库视图（卡片网格 + 扫描纯函数）；
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
    badge, confirm_dialog_with_lang, primary_button_with_glow, subtle_button, Palette,
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

/// 两态页面状态机（§7.3）：库视图 ↔ 编辑器视图
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum PageMode {
    /// 库视图（默认态）：管线库卡片列表 + 新建入口
    #[default]
    Library,
    /// 编辑器视图：画布 + 工具栏 + 参数面板
    Editor,
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
    mode: PageMode,
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
    /// 「返回管线库」确认对话框（dirty 时拦截）
    confirm_back_open: bool,
    /// 执行对话框（映射表 #11：ExecuteDialog 模态）
    exec_dialog_open: bool,
    /// 执行对话框输入文件文本态
    exec_input_text: String,
}

impl Default for VizState {
    fn default() -> Self {
        Self {
            mode: PageMode::Library,
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
            confirm_back_open: false,
            exec_dialog_open: false,
            exec_input_text: String::new(),
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

/// 管线编辑器入口（两态）：`cmd_tx` 为后台命令通道（执行对话框经它提交
/// [`AppCmd`]；None 时执行走"待接线"提示）。
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

    // 状态机守卫：编辑器态但管线已被清空（加载失败等）→ 回落库视图
    if st.mode == PageMode::Editor && st.pipeline.is_none() {
        st.mode = PageMode::Library;
    }

    match st.mode {
        PageMode::Library => {
            library::draw_library_view(ui, lang, &pal, &mut st);
        }
        PageMode::Editor => {
            let pipeline = st.pipeline.clone().unwrap();

            // 编辑器工具栏（含明显的「返回管线库」入口）
            editor_toolbar(ui, lang, &pal, &mut st, cmd_tx, tasks.as_ref());
            ui.separator();

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
    // 返回管线库确认（dirty 拦截：未保存改动将丢弃）
    if st.confirm_back_open {
        let res = confirm_dialog_with_lang(
            ui.ctx(),
            &pal,
            "pe_back_to_library",
            &trfb(lang, "desktopApp.pipeline.unsavedTitle", "未保存的改动", &[]),
            &trfb(
                lang,
                "desktopApp.pipeline.unsavedMsg",
                "管线有未保存的改动，返回管线库将丢弃这些改动。",
                &[],
            ),
            &trfb(lang, "desktopApp.pipeline.unsavedConfirm", "丢弃并返回", &[]),
            true,
            lang,
        );
        if let Some(confirmed) = res {
            st.confirm_back_open = false;
            if confirmed {
                reset_to_library(&mut st);
            }
        }
    }
    // 执行对话框（映射表 #11：校验通过后选择输入文件 + VRAM 阻断校验）
    if st.exec_dialog_open {
        exec_dialog(ui, lang, &pal, &mut st, config, &data, devices.as_ref(), cmd_tx);
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

/// 返回库视图并重置编辑器态（保留 mode=Library 的空态语义）
fn reset_to_library(st: &mut VizState) {
    st.mode = PageMode::Library;
    st.pipeline = None;
    st.positions.clear();
    st.selected = None;
    st.selected_edge = None;
    st.multi_select.clear();
    st.marquee = None;
    st.drafts.clear();
    st.dirty = false;
    st.file_path.clear();
    st.validation_msg = None;
    st.validation_ok = false;
    st.pending_connect = None;
    st.offset = egui::Vec2::ZERO;
    st.zoom = 1.0;
    st.request_fit = false;
    st.confirm_back_open = false;
    st.exec_dialog_open = false;
    st.exec_input_text.clear();
}

// ── Editor toolbar ────────────────────────────────────────────────

/// 编辑器工具栏：← 返回库 | 管线名(+dirty/任务徽章) | 保存 | 验证 |
/// ⚡执行(primary 辉光) | 任务刷新。缩放 −/＋/fit 在画布 overlay。
fn editor_toolbar(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    st: &mut VizState,
    cmd_tx: Option<&UnboundedSender<AppCmd>>,
    tasks: Option<&crate::pages::TasksSnapshot>,
) {
    ui.horizontal(|ui| {
        // 返回管线库（明显入口）：dirty → 确认对话框；否则直接返回
        if ui
            .add(subtle_button(
                pal,
                format!(
                    "← {}",
                    trfb(lang, "desktopApp.pipeline.backToLibrary", "返回管线库", &[])
                ),
            ))
            .on_hover_text(trfb(
                lang,
                "desktopApp.pipeline.backToLibraryTip",
                "返回管线库视图（未保存改动需确认）",
                &[],
            ))
            .clicked()
        {
            if st.dirty {
                st.confirm_back_open = true;
            } else {
                reset_to_library(st);
            }
        }
        ui.separator();

        // 管线名 + dirty 标记 + 文件路径（弱化）
        if let Some(p) = &st.pipeline {
            ui.strong(format!(
                "{}{}",
                p.name.if_empty_fallback(&p.id),
                if st.dirty { " *" } else { "" }
            ));
            // 任务状态徽章（任务快照回显）
            if let Some(task) = latest_task_for(p, tasks) {
                let (color, label) = task_status_badge(lang, pal, &task.status);
                badge(ui, pal, color, label);
                let progress = format!("{}/{}", task.completed_nodes, task.node_count);
                ui.label(egui::RichText::new(progress).small().color(pal.text_dim));
            }
            if !st.file_path.is_empty() {
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
                // 刷新该管线的任务列表（§6.8）
                if ui
                    .add(subtle_button(
                        pal,
                        format!(
                            "🔄 {}",
                            trfb(lang, "desktopApp.pipeline.refreshTasks", "任务", &[])
                        ),
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
