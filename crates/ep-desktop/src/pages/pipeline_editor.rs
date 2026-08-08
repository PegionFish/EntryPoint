//! 管线编辑器 — 决策 2：桌面端管线编排**完整补齐**。
//!
//! 在既有贝塞尔画布（加载/校验/缩放/平移/节点拖拽）基础上补齐：
//! - **节点 palette**：builtin（file_input / file_output / ffmpeg / **llm**）
//!   + 模块节点数据驱动（manifest capabilities 逐项可添加）；
//! - **连线交互**：从输出端口拖拽到输入端口，端口类型校验
//!   （[`DataType::is_compatible_with`]）+ 自连/重边/成环拒绝；
//! - **节点参数编辑面板**：module 节点按 manifest capabilities schema 渲染
//!   （type/default/min/max/enum）+ 变体 pin / 设备下拉；ffmpeg 的 args
//!   数组化编辑 + output_extension；llm 按 B7 参数表
//!   （base_url/model/api_key_env/system_prompt/temperature/max_tokens/
//!   output_format）；
//! - **TOML 保存**：自实现发射器输出 `[pipeline] / [[nodes]] / [[edges]]`
//!   文件形状（§6.2 契约键：module 节点变体 pin 为 `model`），带单元测试；
//! - **执行按钮**：校验 → 选择输入文件 → 提交（后台执行通道 AppCmd 变体
//!   由 C4 门禁期接线，见 C5 报告仲裁）；
//! - **VRAM 账本侧栏**：[`ep_core::pipeline::vram::compute_budget`] 纯计算
//!   + 设备容量快照（dashboard 发布）+ `compute.allow_overcommit`；
//! - **节点状态回显**：任务快照（tasks 页发布）映射节点完成着色。
//!
//! 配色统一来自 [`crate::ui::Palette`]（节点类型色除外），深浅主题均可用。
//! 用户可见文案经 [`crate::i18n::tr`] / [`crate::pages::trfb`] 查找。

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui;
use ep_core::config::AppConfig;
use ep_core::pipeline::dag::{Edge, NodeKind, NodePosition, Pipeline, PipelineNode};
use ep_core::pipeline::vram::{compute_budget, DeviceCapacity, VramNodeEstimate};
use ep_core::types::{DataType, TaskStatus};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::AppCmd;
use crate::i18n::tr;
use crate::pages::{
    device_snapshot, draft_default, module_data, tasks_snapshot, trfb, ModuleData, ParamDraft,
};
use crate::ui::{badge, danger_button, empty_state, primary_button, subtle_button, Palette};

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

/// 缩放步进（工具栏 − / ＋）
const ZOOM_STEP: f32 = 1.18;
const ZOOM_MIN: f32 = 0.3;
const ZOOM_MAX: f32 = 3.0;

// ── Persistent state ──────────────────────────────────────────────

#[derive(Clone)]
struct VizState {
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
    offset: egui::Vec2,
    zoom: f32,
    /// 连线交互：正在从某节点输出端口拖出（node_id）
    pending_connect: Option<String>,
    /// 节点参数编辑草稿（node_id → 草稿）
    drafts: HashMap<String, NodeDraft>,
    /// 执行选择的输入文件（门禁期接线 AppCmd::ExecutePipeline 时随请求发出）
    exec_input: Option<PathBuf>,
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
            offset: egui::Vec2::ZERO,
            zoom: 1.0,
            pending_connect: None,
            drafts: HashMap::new(),
            exec_input: None,
        }
    }
}

/// 单个节点的参数编辑草稿（面板 ↔ node.params 的中间态）
#[derive(Clone, Default)]
struct NodeDraft {
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

/// 管线编辑器入口：`cmd_tx` 为后台命令通道（执行按钮经它提交
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

    // 是否需要在本帧执行"适配视图"（由工具栏触发，画布布局后应用）
    let mut do_fit = false;

    // Toolbar
    toolbar(
        ui,
        lang,
        &pal,
        &mut st,
        cmd_tx,
        tasks.as_ref(),
        &mut do_fit,
    );
    ui.separator();

    // Main area
    if st.pipeline.is_none() {
        if do_fit {
            st.zoom = 1.0;
            st.offset = egui::Vec2::ZERO;
        }
        empty_state(
            ui,
            &pal,
            "🧩",
            &tr(lang, "desktopApp.pipeline.emptyTitle", &[]),
            &trfb(
                lang,
                "desktopApp.pipeline.emptyHintEdit",
                "加载管线 TOML 或点「新建」开始编排：拖拽连线、编辑参数、保存与执行",
                &[],
            ),
        );
    } else {
        let pipeline = st.pipeline.clone().unwrap();
        if st.positions.is_empty() && !pipeline.nodes.is_empty() {
            st.positions = compute_layout(&pipeline);
        }
        // 草稿生命周期：选中节点无草稿则从节点加载；每帧回写（幂等）
        sync_draft(&mut st, &data);
        let canvas_size = draw_main(
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
        if do_fit {
            apply_fit(&mut st, canvas_size);
        }
    }

    // Status bar
    ui.separator();
    match &st.validation_msg {
        Some(msg) if st.validation_ok => {
            ui.colored_label(pal.success, msg.as_str());
        }
        Some(msg) => {
            ui.colored_label(pal.danger, msg.as_str());
        }
        None => {
            ui.colored_label(pal.text_dim, tr(lang, "desktopApp.pipeline.statusReady", &[]));
        }
    }

    // Persist
    ui.data_mut(|d| *d.get_temp_mut_or_default::<VizState>(sid()) = st);
}

// ── Toolbar ───────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn toolbar(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    st: &mut VizState,
    cmd_tx: Option<&UnboundedSender<AppCmd>>,
    tasks: Option<&crate::pages::TasksSnapshot>,
    do_fit: &mut bool,
) {
    ui.horizontal(|ui| {
        ui.label(format!("{}:", tr(lang, "desktopApp.pipeline.fileLabel", &[])));
        let path_w = (ui.available_width() - 640.0).clamp(100.0, 260.0);
        ui.add(egui::TextEdit::singleline(&mut st.file_path).desired_width(path_w));
        if ui
            .add(primary_button(pal, tr(lang, "desktopApp.pipeline.loadToml", &[])))
            .clicked()
        {
            load_pipeline(st, lang);
        }
        if ui
            .add(subtle_button(
                pal,
                trfb(lang, "desktopApp.pipeline.browse", "浏览…", &[]),
            ))
            .clicked()
        {
            if let Some(file) = rfd::FileDialog::new()
                .set_title(trfb(lang, "desktopApp.pipeline.openTitle", "打开管线 TOML", &[]))
                .add_filter("TOML", &["toml"])
                .pick_file()
            {
                st.file_path = file.to_string_lossy().to_string();
                load_pipeline(st, lang);
            }
        }
        if ui
            .add(subtle_button(
                pal,
                trfb(lang, "desktopApp.pipeline.new", "新建", &[]),
            ))
            .on_hover_text(trfb(
                lang,
                "desktopApp.pipeline.newTip",
                "新建含输入/输出的空白管线",
                &[],
            ))
            .clicked()
        {
            new_pipeline(st);
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
            save_pipeline(st, lang, false);
        }
        if ui
            .add(subtle_button(pal, tr(lang, "desktopApp.pipeline.validate", &[])))
            .clicked()
        {
            validate_pipeline(st, lang);
        }
        // 执行按钮（决策 2）
        let pipeline_snap = st.pipeline.clone();
        if let Some(ref p) = pipeline_snap {
            let run_label = trfb(lang, "desktopApp.pipeline.run", "执行", &[]);
            if ui
                .add(primary_button(pal, format!("⚡ {run_label}")))
                .on_hover_text(trfb(
                    lang,
                    "desktopApp.pipeline.runTip",
                    "校验通过后选择输入文件并提交执行",
                    &[],
                ))
                .clicked()
            {
                execute_pipeline(st, lang, cmd_tx, p.clone());
            }
            // 刷新该管线的任务列表（§6.8，AppCmd::RefreshPipelineTasks 为 S2 冻结变体）
            if ui
                .add(subtle_button(
                    pal,
                    format!("🔄 {}", trfb(lang, "desktopApp.pipeline.refreshTasks", "任务", &[])),
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
            ui.separator();
            ui.strong(tr(
                lang,
                "desktopApp.pipeline.pipelineName",
                &[("name", p.name.as_str())],
            ));
            // 任务状态徽章（任务快照回显）
            if let Some(task) = latest_task_for(p, tasks) {
                let (color, label) = task_status_badge(lang, pal, &task.status);
                badge(ui, pal, color, label);
                let progress = format!("{}/{}", task.completed_nodes, task.node_count);
                ui.label(egui::RichText::new(progress).small().color(pal.text_dim));
            }
        }

        // 右侧：视图缩放控件
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(subtle_button(pal, "−"))
                .on_hover_text(tr(lang, "desktopApp.pipeline.zoomOut", &[]))
                .clicked()
            {
                st.zoom = (st.zoom / ZOOM_STEP).clamp(ZOOM_MIN, ZOOM_MAX);
            }
            if ui
                .add(subtle_button(pal, "＋"))
                .on_hover_text(tr(lang, "desktopApp.pipeline.zoomIn", &[]))
                .clicked()
            {
                st.zoom = (st.zoom * ZOOM_STEP).clamp(ZOOM_MIN, ZOOM_MAX);
            }
            if ui
                .add(subtle_button(
                    pal,
                    format!("⤢ {}", tr(lang, "desktopApp.pipeline.fit", &[])),
                ))
                .on_hover_text(tr(lang, "desktopApp.pipeline.fitTip", &[]))
                .clicked()
            {
                *do_fit = true;
            }
        });
    });
}

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
        TaskStatus::Completed => (pal.success, tr(lang, "common.status.completed", &[])),
        TaskStatus::Running => (pal.info, tr(lang, "common.status.running", &[])),
        TaskStatus::Pending => (
            pal.warning,
            trfb(lang, "common.status.queued", "排队中", &[]),
        ),
        TaskStatus::Failed(_) => (pal.danger, tr(lang, "common.status.failed", &[])),
        TaskStatus::Cancelled => (pal.warning, tr(lang, "common.status.cancelled", &[])),
    }
}

// ── Three-panel layout ────────────────────────────────────────────

/// 绘制主区域，返回画布实际尺寸（供"适配视图"使用）。
#[allow(clippy::too_many_arguments)]
fn draw_main(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    st: &mut VizState,
    pipeline: &Pipeline,
    config: &AppConfig,
    data: &ModuleData,
    devices: Option<&crate::pages::DeviceSnapshot>,
    tasks: Option<&crate::pages::TasksSnapshot>,
) -> egui::Vec2 {
    let avail = ui.available_size();
    let narrow = ui.available_width() < 760.0;

    // 响应式：narrow 时隐藏左右面板，只保留画布
    let (left_w, right_w) = if narrow { (0.0, 0.0) } else { (150.0, 260.0) };
    let chrome = if narrow { 0.0 } else { 24.0 };
    let canvas_w = (avail.x - left_w - right_w - chrome).max(200.0);
    let canvas_h = (avail.y - 4.0).max(200.0);
    let canvas_size = egui::vec2(canvas_w, canvas_h);

    // 任务回显：节点 → 状态色
    let echo = node_echo_colors(lang, pal, pipeline, tasks);

    if narrow {
        draw_canvas(ui, lang, pal, st, pipeline, canvas_size, &echo, data);
        return canvas_size;
    }

    ui.horizontal(|ui| {
        // Left panel – node palette（可点击添加节点）
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.set_width(left_w);
            ui.set_min_height(canvas_h);
            draw_palette(ui, lang, pal, st, data);
        });

        ui.separator();

        // Center – node canvas
        draw_canvas(ui, lang, pal, st, pipeline, canvas_size, &echo, data);

        ui.separator();

        // Right panel – pipeline properties + node editor + VRAM ledger
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.set_width(right_w);
            ui.set_min_height(canvas_h);
            draw_right_panel(ui, lang, pal, st, config, data, devices);
        });
    });

    canvas_size
}

// ── Left panel: node palette（决策 2：可添加节点） ─────────────────

fn draw_palette(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    st: &mut VizState,
    data: &ModuleData,
) {
    ui.strong(tr(lang, "desktopApp.pipeline.nodePanel", &[]));
    ui.separator();

    // builtin 节点
    badge(ui, pal, NODE_COLOR_BUILTIN, tr(lang, "desktopApp.pipeline.kindBuiltin", &[]));
    ui.add_space(4.0);
    let builtins: [(&str, &str); 3] = [
        ("file_input", "📥 file_input"),
        ("file_output", "📤 file_output"),
        ("ffmpeg", "🎞 ffmpeg"),
    ];
    for (builtin, label) in builtins {
        if ui
            .add(subtle_button(pal, label))
            .on_hover_text(trfb(
                lang,
                "desktopApp.palette.addTip",
                "点击添加节点到画布",
                &[],
            ))
            .clicked()
        {
            add_builtin_node(st, builtin);
        }
    }
    // LLM（§6.7 builtin，OpenAI 兼容端点）
    if ui
        .add(subtle_button(pal, egui::RichText::new("🤖 llm").color(NODE_COLOR_API)))
        .on_hover_text(trfb(
            lang,
            "desktopApp.palette.llmTip",
            "OpenAI 兼容 LLM 节点（chat/completions）",
            &[],
        ))
        .clicked()
    {
        add_llm_node(st);
    }

    ui.add_space(10.0);
    ui.separator();

    // 模块节点（数据驱动：manifest capabilities）
    badge(ui, pal, pal.primary, tr(lang, "common.label.module", &[]));
    ui.add_space(4.0);
    let mut any = false;
    for mf in data.manifests() {
        for cap in &mf.interface.capabilities {
            any = true;
            let label = format!("{}::{}", mf.module.id, cap.name);
            if ui
                .add(subtle_button(pal, &label))
                .on_hover_text(if cap.description.is_empty() {
                    trfb(lang, "desktopApp.palette.addTip", "点击添加节点到画布", &[])
                } else {
                    cap.description.clone()
                })
                .clicked()
            {
                add_module_node(st, data, &mf.module.id, &cap.name);
            }
        }
    }
    if !any {
        ui.label(
            egui::RichText::new(trfb(
                lang,
                "desktopApp.palette.noModules",
                "未发现模块能力",
                &[],
            ))
            .small()
            .color(pal.text_faint),
        );
    }

    ui.add_space(12.0);
    ui.label(
        egui::RichText::new(trfb(
            lang,
            "desktopApp.pipeline.helpTextEdit",
            "点击节点查看/编辑参数\n从右侧端口拖到目标左侧端口连线\n右键节点删除 · 点选连线后 Del 删除\n滚轮缩放 · 中键平移",
            &[],
        ))
        .small()
        .color(pal.text_faint),
    );
}

// ── Node creation ─────────────────────────────────────────────────

/// 生成唯一节点 id（base 冲突时追加 _2/_3…）
fn unique_id(pipeline: &Pipeline, base: &str) -> String {
    let sanitized: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let base = if sanitized.is_empty() { "node".to_string() } else { sanitized };
    if !pipeline.nodes.iter().any(|n| n.id == base) {
        return base;
    }
    for i in 2..1000 {
        let cand = format!("{base}_{i}");
        if !pipeline.nodes.iter().any(|n| n.id == cand) {
            return cand;
        }
    }
    format!("{base}_{}", pipeline.nodes.len() + 1)
}

/// 新节点落位（级联排布，避免重叠）
fn place_new_node(st: &mut VizState, id: &str) {
    let n = st.positions.len() as f32;
    let pos = egui::pos2(80.0 + (n % 6.0) * 48.0, 80.0 + (n % 8.0) * 44.0);
    st.positions.insert(id.to_string(), pos);
}

fn add_builtin_node(st: &mut VizState, builtin: &str) {
    let Some(p) = st.pipeline.as_mut() else { return };
    let id = unique_id(p, builtin);
    let node = PipelineNode {
        id: id.clone(),
        kind: NodeKind::Builtin {
            builtin: builtin.to_string(),
        },
        label: String::new(),
        params: Default::default(),
        position: None,
        timeout_secs: None,
        retry_count: None,
    };
    p.nodes.push(node);
    place_new_node(st, &id);
    st.selected = Some(id);
    st.selected_edge = None;
    st.dirty = true;
    st.drafts.clear();
}

fn add_llm_node(st: &mut VizState) {
    let Some(p) = st.pipeline.as_mut() else { return };
    let id = unique_id(p, "llm");
    let node = PipelineNode {
        id: id.clone(),
        kind: NodeKind::ExternalApi {
            endpoint: String::new(),
            api_key_env: None,
        },
        label: String::new(),
        params: Default::default(),
        position: None,
        timeout_secs: None,
        retry_count: None,
    };
    p.nodes.push(node);
    place_new_node(st, &id);
    st.selected = Some(id);
    st.selected_edge = None;
    st.dirty = true;
    st.drafts.clear();
}

fn add_module_node(st: &mut VizState, data: &ModuleData, module_id: &str, capability: &str) {
    let Some(p) = st.pipeline.as_mut() else { return };
    let id = unique_id(p, capability);
    // 参数默认值：schema 驱动（§5.3 同款 draft_default）
    let mut node = PipelineNode {
        id: id.clone(),
        kind: NodeKind::Module {
            module_id: module_id.to_string(),
            capability: capability.to_string(),
            model_id: None,
            device: None,
        },
        label: String::new(),
        params: Default::default(),
        position: None,
        timeout_secs: None,
        retry_count: None,
    };
    if let Some(cap) = data.capability(module_id, capability) {
        if let Some(schema_map) = &cap.params {
            let mut keys: Vec<&String> = schema_map.keys().collect();
            keys.sort();
            for key in keys {
                let draft = draft_default(&schema_map[key]);
                set_param_draft(&mut node, key, &draft);
            }
        }
    }
    p.nodes.push(node);
    place_new_node(st, &id);
    st.selected = Some(id);
    st.selected_edge = None;
    st.dirty = true;
    st.drafts.clear();
}

/// 草稿值 → node.params 单键写入（匿名访问 serde_json::Value：
/// ep-desktop 未直接依赖 serde_json，经方法调用 + Into 推断完成读写）
fn set_param_draft(node: &mut PipelineNode, key: &str, draft: &ParamDraft) {
    match draft {
        ParamDraft::Str(s) => node.params[key] = s.clone().into(),
        ParamDraft::Int(i) => node.params[key] = (*i).into(),
        ParamDraft::Float(f) => node.params[key] = (*f).into(),
        ParamDraft::Bool(b) => node.params[key] = (*b).into(),
    }
}

// ── Draft sync（加载/回写） ───────────────────────────────────────

/// 每帧：选中节点无草稿则从节点加载；有草稿则回写到节点（幂等）。
fn sync_draft(st: &mut VizState, data: &ModuleData) {
    let Some(sel) = st.selected.clone() else { return };
    let Some(pipeline) = st.pipeline.as_mut() else { return };
    let Some(node) = pipeline.nodes.iter().find(|n| n.id == sel) else {
        st.selected = None;
        return;
    };

    if !st.drafts.contains_key(&sel) {
        let draft = load_draft(node, data);
        st.drafts.insert(sel.clone(), draft);
    }
    if let Some(draft) = st.drafts.get(&sel).cloned() {
        if let Some(node) = pipeline.nodes.iter_mut().find(|n| n.id == sel) {
            apply_draft(node, &draft);
        }
    }
}

/// 节点 → 草稿（首次选中时解析，其后以草稿为编辑态）
fn load_draft(node: &PipelineNode, data: &ModuleData) -> NodeDraft {
    let mut d = NodeDraft {
        device: "auto".to_string(),
        output_format: "text".to_string(),
        ..Default::default()
    };
    d.label = node.label.clone();
    d.timeout = node.timeout_secs.unwrap_or(0);
    d.retry = node.retry_count.unwrap_or(0);

    match &node.kind {
        NodeKind::Module {
            module_id,
            capability,
            model_id,
            device,
        } => {
            d.capability = capability.clone();
            d.model = model_id.clone().unwrap_or_default();
            d.device = device.clone().unwrap_or_else(|| "auto".to_string());
            if let Some(cap) = data.capability(module_id, capability) {
                if let Some(schema_map) = &cap.params {
                    let mut keys: Vec<&String> = schema_map.keys().collect();
                    keys.sort();
                    for key in keys {
                        let schema = &schema_map[key];
                        let draft = read_param_draft(node, key, schema);
                        d.params.push(((*key).clone(), draft));
                    }
                }
            }
        }
        NodeKind::Builtin { builtin } if builtin == "ffmpeg" => {
            let args_val = node.params.get("args");
            if let Some(arr) = args_val.and_then(|v| v.as_array()) {
                d.args = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
            } else if let Some(s) = args_val.and_then(|v| v.as_str()) {
                d.args_raw = s.to_string();
                d.args_is_string = true;
            }
            d.output_extension = node
                .params
                .get("output_extension")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
        }
        NodeKind::ExternalApi {
            endpoint,
            api_key_env,
        } => {
            d.api_key_env = api_key_env.clone().unwrap_or_default();
            let read_str = |key: &str| {
                node.params
                    .get(key)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            d.base_url = if endpoint.is_empty() {
                read_str("base_url")
            } else {
                endpoint.clone()
            };
            d.llm_model = read_str("model");
            d.system_prompt = read_str("system_prompt");
            if let Some(t) = node.params.get("temperature").and_then(|v| v.as_f64()) {
                d.temperature = t;
                d.has_temperature = true;
            }
            if let Some(m) = node.params.get("max_tokens").and_then(|v| v.as_i64()) {
                d.max_tokens = m;
                d.has_max_tokens = true;
            }
            let fmt = read_str("output_format");
            d.output_format = if fmt == "json" { "json".to_string() } else { "text".to_string() };
        }
        _ => {}
    }

    d
}

/// 读取单个参数草稿（node.params 匿名读取；缺值 → schema 默认）
fn read_param_draft(
    node: &PipelineNode,
    key: &str,
    schema: &ep_core::module::ParamSchema,
) -> ParamDraft {
    let t = schema.param_type.to_ascii_lowercase();
    let Some(v) = node.params.get(key) else {
        return draft_default(schema);
    };
    if t == "boolean" || t == "bool" {
        v.as_bool()
            .map(ParamDraft::Bool)
            .unwrap_or_else(|| draft_default(schema))
    } else if t == "integer" || t == "int" {
        v.as_i64()
            .map(ParamDraft::Int)
            .unwrap_or_else(|| draft_default(schema))
    } else if t == "number" || t == "float" || t == "double" {
        v.as_f64()
            .map(ParamDraft::Float)
            .unwrap_or_else(|| draft_default(schema))
    } else {
        ParamDraft::Str(
            v.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| v.to_string()),
        )
    }
}

/// 草稿 → 节点（label/timeout/retry/kind 字段/params 全量同步）
fn apply_draft(node: &mut PipelineNode, draft: &NodeDraft) {
    node.label = draft.label.clone();
    node.timeout_secs = (draft.timeout > 0).then_some(draft.timeout);
    node.retry_count = (draft.retry > 0).then_some(draft.retry);

    match &mut node.kind {
        NodeKind::Module {
            capability,
            model_id,
            device,
            ..
        } => {
            if !draft.capability.is_empty() {
                *capability = draft.capability.clone();
            }
            *model_id = if draft.model.is_empty() {
                None
            } else {
                Some(draft.model.clone())
            };
            *device = if draft.device.is_empty() || draft.device.eq_ignore_ascii_case("auto") {
                None
            } else {
                Some(draft.device.clone())
            };
            // params：仅覆盖草稿涉及的 schema 键，其余非 schema 键保留
            //（P2 修复：含 schema 外键的 TOML 一经编辑不再静默丢失）
            if let Some(obj) = node.params.as_object_mut() {
                let draft_keys: Vec<&str> = draft.params.iter().map(|(k, _)| k.as_str()).collect();
                obj.retain(|k, _| !draft_keys.contains(&k.as_str()));
            }
            for (key, d) in &draft.params {
                set_param_draft(node, key, d);
            }
        }
        NodeKind::Builtin { builtin } if builtin == "ffmpeg" => {
            if draft.args_is_string {
                node.params["args"] = draft.args_raw.clone().into();
            } else {
                node.params["args"] = draft.args.clone().into();
            }
            if draft.output_extension.is_empty() {
                if let Some(obj) = node.params.as_object_mut() {
                    obj.remove("output_extension");
                }
            } else {
                node.params["output_extension"] = draft.output_extension.clone().into();
            }
        }
        NodeKind::ExternalApi { api_key_env, .. } => {
            *api_key_env = if draft.api_key_env.is_empty() {
                None
            } else {
                Some(draft.api_key_env.clone())
            };
            // base_url / model：必填项，空值也写出（校验器负责报错）
            node.params["base_url"] = draft.base_url.clone().into();
            node.params["model"] = draft.llm_model.clone().into();
            set_or_remove(node, "system_prompt", &draft.system_prompt);
            if draft.has_temperature {
                node.params["temperature"] = draft.temperature.into();
            } else if let Some(obj) = node.params.as_object_mut() {
                obj.remove("temperature");
            }
            if draft.has_max_tokens {
                node.params["max_tokens"] = draft.max_tokens.into();
            } else if let Some(obj) = node.params.as_object_mut() {
                obj.remove("max_tokens");
            }
            if draft.output_format == "json" {
                node.params["output_format"] = "json".into();
            } else if let Some(obj) = node.params.as_object_mut() {
                obj.remove("output_format");
            }
        }
        _ => {}
    }
}

/// 非空写字符串参数，空则移除键
fn set_or_remove(node: &mut PipelineNode, key: &str, value: &str) {
    if value.is_empty() {
        if let Some(obj) = node.params.as_object_mut() {
            obj.remove(key);
        }
    } else {
        node.params[key] = value.to_string().into();
    }
}

// ── Right panel ───────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn draw_right_panel(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    st: &mut VizState,
    config: &AppConfig,
    data: &ModuleData,
    devices: Option<&crate::pages::DeviceSnapshot>,
) {
    // ── 管线属性（id/name/description） ──
    ui.strong(trfb(lang, "desktopApp.pipeline.props", "管线属性", &[]));
    ui.add_space(4.0);
    if let Some(p) = st.pipeline.as_mut() {
        let id_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("ID:").color(pal.text_dim));
            ui.add(egui::TextEdit::singleline(&mut p.id).desired_width(150.0))
        });
        if id_resp.inner.changed() {
            st.dirty = true;
        }
        let name_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new(trfb(lang, "common.label.name2", "名称:", &[])).color(pal.text_dim));
            ui.add(egui::TextEdit::singleline(&mut p.name).desired_width(150.0))
        });
        if name_resp.inner.changed() {
            st.dirty = true;
        }
        let desc_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new(trfb(lang, "common.label.description2", "描述:", &[])).color(pal.text_dim));
            ui.add(egui::TextEdit::singleline(&mut p.description).desired_width(150.0))
        });
        if desc_resp.inner.changed() {
            st.dirty = true;
        }
    }
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(8.0);

    // ── 选中边 / 节点编辑 ──
    if let Some(edge) = st.selected_edge.clone() {
        ui.strong(trfb(lang, "desktopApp.pipeline.edgeSelected", "已选中连线", &[]));
        ui.label(
            egui::RichText::new(format!(
                "{}:{} → {}:{}",
                edge.from.0, edge.from.1, edge.to.0, edge.to.1
            ))
            .monospace()
            .small(),
        );
        ui.add_space(4.0);
        if ui
            .add(danger_button(
                pal,
                trfb(lang, "desktopApp.pipeline.deleteEdge", "删除连线", &[]),
            ))
            .clicked()
        {
            remove_edge(st, &edge);
        }
    } else if let Some(sel) = st.selected.clone() {
        let pipeline_snapshot = st.pipeline.clone();
        if let Some(node) = pipeline_snapshot.and_then(|p| {
            p.nodes.iter().find(|n| n.id == sel).cloned()
        }) {
            draw_node_editor(ui, lang, pal, st, &node, data, devices);
        }
    } else {
        ui.label(
            egui::RichText::new(tr(lang, "desktopApp.pipeline.clickNodeHint", &[]))
                .color(pal.text_faint),
        );
    }

    ui.add_space(10.0);
    ui.separator();
    ui.add_space(8.0);

    // ── VRAM 账本（§6.3） ──
    vram_ledger(ui, lang, pal, st, config, data, devices);
}

/// 节点编辑器：通用字段 + 按 kind 的参数表单
#[allow(clippy::too_many_arguments)]
fn draw_node_editor(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    st: &mut VizState,
    node: &PipelineNode,
    data: &ModuleData,
    devices: Option<&crate::pages::DeviceSnapshot>,
) {
    let node_id = node.id.clone();
    let Some(draft) = st.drafts.get_mut(&node_id) else {
        return;
    };

    // 头部：类型 + id
    let (kind_str, detail) = node_kind_info(lang, &node.kind);
    ui.horizontal(|ui| {
        ui.strong(&kind_str);
        ui.label(
            egui::RichText::new(&node_id)
                .monospace()
                .small()
                .color(pal.text_faint),
        );
    });
    if !detail.is_empty() {
        ui.label(
            egui::RichText::new(&detail)
                .small()
                .color(pal.text_dim),
        );
    }
    ui.add_space(6.0);

    // 通用：label / timeout / retry
    if ui
        .horizontal(|ui| {
            ui.label(egui::RichText::new(trfb(lang, "desktopApp.pipeline.paramLabel2", "标签:", &[])).color(pal.text_dim));
            ui.add(
                egui::TextEdit::singleline(&mut draft.label)
                    .id_salt(egui::Id::new(("pe_label", node_id.clone())))
                    .desired_width(140.0),
            )
        })
        .inner
        .changed()
    {
        st.dirty = true;
    }
    if ui
        .horizontal(|ui| {
            ui.label(
                egui::RichText::new(trfb(
                    lang,
                    "desktopApp.pipeline.timeoutEdit",
                    "超时秒 (0=默认):",
                    &[],
                ))
                .color(pal.text_dim),
            );
            ui.add(egui::DragValue::new(&mut draft.timeout).range(0..=86400u32))
        })
        .inner
        .changed()
    {
        st.dirty = true;
    }
    if ui
        .horizontal(|ui| {
            ui.label(
                egui::RichText::new(trfb(
                    lang,
                    "desktopApp.pipeline.retryEdit",
                    "重试次数:",
                    &[],
                ))
                .color(pal.text_dim),
            );
            ui.add(egui::DragValue::new(&mut draft.retry).range(0..=10u32))
        })
        .inner
        .changed()
    {
        st.dirty = true;
    }
    ui.add_space(6.0);

    match &node.kind {
        NodeKind::Module { module_id, .. } => {
            module_node_editor(ui, lang, pal, st, &node_id, module_id, data, devices);
        }
        NodeKind::Builtin { builtin } if builtin == "ffmpeg" => {
            ffmpeg_node_editor(ui, lang, pal, st, &node_id);
        }
        NodeKind::ExternalApi { .. } => {
            llm_node_editor(ui, lang, pal, st, &node_id);
        }
        NodeKind::Builtin { builtin } => {
            ui.label(
                egui::RichText::new(trfb(
                    lang,
                    "desktopApp.pipeline.builtinNoParams",
                    "该内置节点无可配置参数",
                    &[],
                ))
                .small()
                .color(pal.text_faint),
            );
            let _ = builtin;
        }
    }

    ui.add_space(8.0);
    // 删除节点
    if ui
        .add(danger_button(
            pal,
            format!(
                "🗑 {}",
                trfb(lang, "desktopApp.pipeline.deleteNode", "删除节点", &[])
            ),
        ))
        .clicked()
    {
        delete_node(st, &node_id);
    }
}

/// module 节点：capability 切换 + 变体 pin + 设备绑定 + schema 参数表单
#[allow(clippy::too_many_arguments)]
fn module_node_editor(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    st: &mut VizState,
    node_id: &str,
    module_id: &str,
    data: &ModuleData,
    devices: Option<&crate::pages::DeviceSnapshot>,
) {
    let Some(mf) = data.manifest(module_id) else {
        ui.label(
            egui::RichText::new(trfb(
                lang,
                "desktopApp.pipeline.manifestMissing",
                "模块清单不可用（模块未安装？）",
                &[],
            ))
            .small()
            .color(pal.warning),
        );
        return;
    };
    let caps: Vec<String> = mf
        .interface
        .capabilities
        .iter()
        .map(|c| c.name.clone())
        .collect();
    let variants: Vec<(String, String)> = mf
        .models
        .iter()
        .map(|m| (m.id.clone(), m.name.clone()))
        .collect();

    let Some(draft) = st.drafts.get_mut(node_id) else {
        return;
    };

    // capability
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("capability:").color(pal.text_dim));
        egui::ComboBox::from_id_salt(egui::Id::new(("pe_cap", node_id)))
            .selected_text(if draft.capability.is_empty() {
                "-"
            } else {
                draft.capability.as_str()
            })
            .show_ui(ui, |ui| {
                for cap in &caps {
                    if ui
                        .selectable_label(draft.capability == *cap, cap)
                        .clicked()
                        && draft.capability != *cap
                    {
                        draft.capability = cap.clone();
                        // 切换能力 → 按新 schema 重建参数草稿（保留同名值）
                        rebuild_module_draft(draft, data, module_id);
                        st.dirty = true;
                    }
                }
            });
    });

    // 变体 pin（§6.2：model 字段；空 = 跟随激活变体）
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(trfb(lang, "desktopApp.pipeline.modelPin", "变体 pin:", &[]))
                .color(pal.text_dim),
        );
        let active_label = trfb(lang, "desktopApp.pipeline.followActive", "跟随激活变体", &[]);
        egui::ComboBox::from_id_salt(egui::Id::new(("pe_model", node_id)))
            .selected_text(if draft.model.is_empty() {
                active_label.clone()
            } else {
                draft.model.clone()
            })
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(draft.model.is_empty(), &active_label)
                    .clicked()
                {
                    draft.model.clear();
                    st.dirty = true;
                }
                for (id, name) in &variants {
                    let sel = draft.model == *id;
                    if ui
                        .selectable_label(sel, format!("{id} — {name}"))
                        .clicked()
                        && !sel
                    {
                        draft.model = id.clone();
                        st.dirty = true;
                    }
                }
            });
    });

    // 设备绑定（§6.2：device 字段，软约束）
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(trfb(lang, "desktopApp.pipeline.deviceBind", "设备:", &[]))
                .color(pal.text_dim),
        );
        egui::ComboBox::from_id_salt(egui::Id::new(("pe_device", node_id)))
            .selected_text(if draft.device.is_empty() {
                "auto".to_string()
            } else {
                draft.device.clone()
            })
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(
                        draft.device.is_empty() || draft.device.eq_ignore_ascii_case("auto"),
                        "auto",
                    )
                    .clicked()
                {
                    draft.device = "auto".to_string();
                    st.dirty = true;
                }
                if let Some(snap) = devices {
                    for dev in &snap.devices {
                        let id_str = dev.id.to_string();
                        let sel = draft.device.eq_ignore_ascii_case(&id_str);
                        if ui
                            .selectable_label(sel, format!("{id_str} ({})", dev.name))
                            .clicked()
                            && !sel
                        {
                            draft.device = id_str;
                            st.dirty = true;
                        }
                    }
                }
            });
    });
    ui.add_space(4.0);

    // schema 参数表单
    if let Some(cap) = mf
        .interface
        .capabilities
        .iter()
        .find(|c| c.name == draft.capability)
    {
        if let Some(schema_map) = &cap.params {
            if !schema_map.is_empty() {
                ui.label(
                    egui::RichText::new(format!(
                        "{}:",
                        tr(lang, "desktopApp.pipeline.params", &[])
                    ))
                    .strong()
                    .color(pal.text_dim),
                );
                let mut keys: Vec<&String> = schema_map.keys().collect();
                keys.sort();
                for key in keys {
                    let schema = &schema_map[key];
                    if draft_row(ui, pal, node_id, key, schema, st) {
                        st.dirty = true;
                    }
                }
            }
        }
    }
}

/// capability 切换后重建参数草稿（保留同名已有值）
fn rebuild_module_draft(draft: &mut NodeDraft, data: &ModuleData, module_id: &str) {
    let cap = data
        .manifest(module_id)
        .and_then(|mf| {
            mf.interface
                .capabilities
                .iter()
                .find(|c| c.name == draft.capability)
        });
    let old = std::mem::take(&mut draft.params);
    if let Some(cap) = cap {
        if let Some(schema_map) = &cap.params {
            let mut keys: Vec<&String> = schema_map.keys().collect();
            keys.sort();
            for key in keys {
                let kept = old.iter().find(|(n, _)| n == key).map(|(_, d)| d.clone());
                draft
                    .params
                    .push(((*key).clone(), kept.unwrap_or_else(|| draft_default(&schema_map[key]))));
            }
        }
    }
}

/// 单个参数草稿行；返回是否有改动
fn draft_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    node_id: &str,
    key: &str,
    schema: &ep_core::module::ParamSchema,
    st: &mut VizState,
) -> bool {
    let t = schema.param_type.to_ascii_lowercase();
    let enum_options = schema
        .enum_values
        .as_ref()
        .or(schema.options.as_ref())
        .cloned()
        .unwrap_or_default();

    let Some(draft) = st.drafts.get_mut(node_id) else {
        return false;
    };
    let Some(idx) = draft.params.iter().position(|(n, _)| n == key) else {
        return false;
    };

    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("{key}:")).color(pal.text_dim));

        if !enum_options.is_empty() {
            let current = draft.params[idx].1.to_arg();
            egui::ComboBox::from_id_salt(egui::Id::new(("pe_param", node_id, key)))
                .selected_text(if current.is_empty() { "-" } else { &current })
                .show_ui(ui, |ui| {
                    for opt in &enum_options {
                        if ui
                            .selectable_label(current == *opt, opt)
                            .clicked()
                            && current != *opt
                        {
                            draft.params[idx].1 = ParamDraft::Str(opt.clone());
                            changed = true;
                        }
                    }
                });
        } else if t == "boolean" || t == "bool" {
            let mut value = matches!(draft.params[idx].1, ParamDraft::Bool(true));
            if ui.checkbox(&mut value, "").changed() {
                draft.params[idx].1 = ParamDraft::Bool(value);
                changed = true;
            }
        } else if t == "integer" || t == "int" {
            let mut value = match &draft.params[idx].1 {
                ParamDraft::Int(i) => *i,
                ParamDraft::Float(f) => *f as i64,
                ParamDraft::Str(s) => s.parse().unwrap_or(0),
                ParamDraft::Bool(b) => i64::from(*b),
            };
            let min = schema.min.map(|m| m as i64).unwrap_or(i64::MIN / 2);
            let max = schema.max.map(|m| m as i64).unwrap_or(i64::MAX / 2);
            if ui
                .add(egui::DragValue::new(&mut value).range(min..=max))
                .changed()
            {
                draft.params[idx].1 = ParamDraft::Int(value);
                changed = true;
            }
        } else if t == "number" || t == "float" || t == "double" {
            let mut value = match &draft.params[idx].1 {
                ParamDraft::Float(f) => *f,
                ParamDraft::Int(i) => *i as f64,
                ParamDraft::Str(s) => s.parse().unwrap_or(0.0),
                ParamDraft::Bool(_) => 0.0,
            };
            let min = schema.min.unwrap_or(f64::MIN / 2.0);
            let max = schema.max.unwrap_or(f64::MAX / 2.0);
            if ui
                .add(egui::DragValue::new(&mut value).range(min..=max))
                .changed()
            {
                draft.params[idx].1 = ParamDraft::Float(value);
                changed = true;
            }
        } else {
            let mut value = draft.params[idx].1.to_arg();
            let width = ui.available_width().clamp(60.0, 150.0);
            if ui
                .add(
                    egui::TextEdit::singleline(&mut value)
                        .id_salt(egui::Id::new(("pe_str", node_id, key)))
                        .desired_width(width),
                )
                .changed()
            {
                draft.params[idx].1 = ParamDraft::Str(value);
                changed = true;
            }
        }
    });
    if let Some(desc) = &schema.description {
        if !desc.is_empty() {
            ui.label(egui::RichText::new(desc).small().color(pal.text_faint));
        }
    }
    changed
}

/// ffmpeg 节点：args 数组化编辑 + output_extension（§6.1）
fn ffmpeg_node_editor(ui: &mut egui::Ui, lang: &str, pal: &Palette, st: &mut VizState, node_id: &str) {
    let Some(draft) = st.drafts.get_mut(node_id) else {
        return;
    };

    ui.label(
        egui::RichText::new("args:")
            .strong()
            .color(pal.text_dim),
    );
    if draft.args_is_string {
        // 原 TOML 为字符串形状：保持原形状编辑
        if ui
            .add(
                egui::TextEdit::multiline(&mut draft.args_raw)
                    .id_salt(egui::Id::new(("pe_args_raw", node_id)))
                    .desired_width(f32::INFINITY)
                    .code_editor(),
            )
            .changed()
        {
            st.dirty = true;
        }
    } else {
        let mut remove: Option<usize> = None;
        for (i, arg) in draft.args.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("{i}")).monospace().small().color(pal.text_faint));
                if ui
                    .add(
                        egui::TextEdit::singleline(arg)
                            .id_salt(egui::Id::new(("pe_arg", node_id, i)))
                            .desired_width(150.0)
                            .code_editor(),
                    )
                    .changed()
                {
                    st.dirty = true;
                }
                if ui
                    .add(subtle_button(pal, "✕"))
                    .on_hover_text(tr(lang, "common.action.delete", &[]))
                    .clicked()
                {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            draft.args.remove(i);
            st.dirty = true;
        }
        if ui
            .add(subtle_button(
                pal,
                format!("＋ {}", trfb(lang, "desktopApp.pipeline.addArg", "参数项", &[])),
            ))
            .clicked()
        {
            draft.args.push(String::new());
            st.dirty = true;
        }
        ui.label(
            egui::RichText::new(trfb(
                lang,
                "desktopApp.pipeline.argsTip",
                "{input}/{output} 为执行期占位符",
                &[],
            ))
            .small()
            .color(pal.text_faint),
        );
    }

    ui.add_space(4.0);
    if ui
        .horizontal(|ui| {
            ui.label(egui::RichText::new("output_extension:").color(pal.text_dim));
            ui.add(
                egui::TextEdit::singleline(&mut draft.output_extension)
                    .id_salt(egui::Id::new(("pe_ext", node_id)))
                    .desired_width(80.0)
                    .hint_text("wav"),
            )
        })
        .inner
        .changed()
    {
        st.dirty = true;
    }
}

/// llm 节点：B7 参数表（§6.7）
fn llm_node_editor(ui: &mut egui::Ui, lang: &str, pal: &Palette, st: &mut VizState, node_id: &str) {
    let Some(draft) = st.drafts.get_mut(node_id) else {
        return;
    };

    ui.label(egui::RichText::new("base_url:").color(pal.text_dim));
    if ui
        .add(
            egui::TextEdit::singleline(&mut draft.base_url)
                .id_salt(egui::Id::new(("pe_llm_url", node_id)))
                .desired_width(f32::INFINITY)
                .hint_text("https://api.openai.com/v1"),
        )
        .changed()
    {
        st.dirty = true;
    }

    ui.label(egui::RichText::new("model:").color(pal.text_dim));
    if ui
        .add(
            egui::TextEdit::singleline(&mut draft.llm_model)
                .id_salt(egui::Id::new(("pe_llm_model", node_id)))
                .desired_width(f32::INFINITY)
                .hint_text("gpt-4o-mini"),
        )
        .changed()
    {
        st.dirty = true;
    }

    ui.label(
        egui::RichText::new("api_key_env:")
            .color(pal.text_dim),
    );
    if ui
        .add(
            egui::TextEdit::singleline(&mut draft.api_key_env)
                .id_salt(egui::Id::new(("pe_llm_key", node_id)))
                .desired_width(f32::INFINITY)
                .hint_text("OPENAI_API_KEY"),
        )
        .on_hover_text(trfb(
            lang,
            "desktopApp.pipeline.apiKeyTip",
            "存环境变量名而非明文密钥；留空 = 免密钥本地端点",
            &[],
        ))
        .changed()
    {
        st.dirty = true;
    }

    ui.label(egui::RichText::new("system_prompt:").color(pal.text_dim));
    if ui
        .add(
            egui::TextEdit::multiline(&mut draft.system_prompt)
                .id_salt(egui::Id::new(("pe_llm_prompt", node_id)))
                .desired_width(f32::INFINITY)
                .desired_rows(3)
                .hint_text("{input}"),
        )
        .on_hover_text(trfb(
            lang,
            "desktopApp.pipeline.promptTip",
            "{input} 占位符将被上游文本替换",
            &[],
        ))
        .changed()
    {
        st.dirty = true;
    }

    ui.horizontal(|ui| {
        if ui
            .checkbox(
                &mut draft.has_temperature,
                trfb(lang, "desktopApp.pipeline.temperature", "temperature", &[]),
            )
            .changed()
        {
            st.dirty = true;
        }
        if draft.has_temperature
            && ui
                .add(egui::DragValue::new(&mut draft.temperature).range(0.0..=2.0).speed(0.05))
                .changed()
        {
            st.dirty = true;
        }
    });
    ui.horizontal(|ui| {
        if ui
            .checkbox(
                &mut draft.has_max_tokens,
                trfb(lang, "desktopApp.pipeline.maxTokens", "max_tokens", &[]),
            )
            .changed()
        {
            st.dirty = true;
        }
        if draft.has_max_tokens
            && ui
                .add(egui::DragValue::new(&mut draft.max_tokens).range(1..=1_000_000i64))
                .changed()
        {
            st.dirty = true;
        }
    });

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("output_format:").color(pal.text_dim));
        for fmt in ["text", "json"] {
            if ui
                .radio(draft.output_format == fmt, fmt)
                .clicked()
            {
                draft.output_format = fmt.to_string();
                st.dirty = true;
            }
        }
    });
}

// ── VRAM ledger（§6.3） ───────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn vram_ledger(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    st: &VizState,
    config: &AppConfig,
    data: &ModuleData,
    devices: Option<&crate::pages::DeviceSnapshot>,
) {
    ui.strong(trfb(
        lang,
        "desktopApp.pipeline.vram.title",
        "VRAM 账本",
        &[],
    ));
    ui.add_space(4.0);

    let Some(pipeline) = &st.pipeline else { return };

    // 节点估算：module 节点取 pin 变体 vram（变体级优先、模块级兜底）
    let mut nodes: Vec<VramNodeEstimate> = Vec::new();
    for node in &pipeline.nodes {
        match &node.kind {
            NodeKind::Module {
                module_id,
                model_id,
                device,
                ..
            } => {
                let variant = resolve_budget_variant(config, data, module_id, model_id.as_deref());
                let vram_mb = data
                    .manifest(module_id)
                    .and_then(|mf| mf.resolve_vram_estimate(&variant));
                nodes.push(VramNodeEstimate {
                    node_id: node.id.clone(),
                    device: device.clone().unwrap_or_else(|| "auto".to_string()),
                    vram_mb,
                });
            }
            _ => {
                nodes.push(VramNodeEstimate {
                    node_id: node.id.clone(),
                    device: "auto".to_string(),
                    vram_mb: None,
                });
            }
        }
    }
    let edges: Vec<(String, String)> = pipeline
        .edges
        .iter()
        .map(|e| (e.from.0.clone(), e.to.0.clone()))
        .collect();
    let capacities: Vec<DeviceCapacity> = devices
        .map(|snap| {
            snap.devices
                .iter()
                .map(|d| DeviceCapacity {
                    device_id: d.id.to_string(),
                    total_mb: d.total_memory_mb.map(u64::from),
                    used_mb: d.used_memory_mb.map(u64::from),
                })
                .collect()
        })
        .unwrap_or_default();

    match compute_budget(&nodes, &edges, &capacities, config.compute.allow_overcommit) {
        Ok(report) => {
            let mut any_over = false;
            for dev in &report.devices {
                let over = dev.over;
                any_over |= over;
                let label_color = if over { pal.danger } else { pal.text };
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(&dev.device_id)
                            .monospace()
                            .color(label_color),
                    );
                    if over {
                        badge(
                            ui,
                            pal,
                            pal.danger,
                            trfb(lang, "desktopApp.pipeline.vram.over", "超预算", &[]),
                        );
                    }
                });
                // 进度条：已用 + 管线预算 vs 总量
                if let Some(total) = dev.total_mb {
                    let used = dev.used_mb.unwrap_or(0);
                    let frac =
                        ((used + dev.pipeline_mb) as f32 / total.max(1) as f32).min(1.0);
                    let fill = if over {
                        pal.danger
                    } else if frac > 0.8 {
                        pal.warning
                    } else {
                        pal.primary
                    };
                    ui.add(egui::ProgressBar::new(frac).fill(fill));
                    ui.label(
                        egui::RichText::new(format!(
                            "{} + {} / {} MB",
                            used, dev.pipeline_mb, total
                        ))
                        .monospace()
                        .small()
                        .color(pal.text_dim),
                    );
                } else {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} MB · {}",
                            dev.pipeline_mb,
                            trfb(
                                lang,
                                "desktopApp.pipeline.vram.unknownCap",
                                "容量未知",
                                &[]
                            )
                        ))
                        .monospace()
                        .small()
                        .color(pal.text_dim),
                    );
                }
                // 峰值层节点明细
                if !dev.items.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(4.0, 2.0);
                        for item in &dev.items {
                            ui.label(
                                egui::RichText::new(format!("{} {}MB", item.node_id, item.mb))
                                    .small()
                                    .color(pal.text_faint),
                            );
                        }
                    });
                }
                ui.add_space(4.0);
            }

            // 未分配池（auto 节点）
            if report.unassigned_mb > 0 {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(trfb(
                            lang,
                            "desktopApp.pipeline.vram.unassigned",
                            "auto 未分配",
                            &[],
                        ))
                        .monospace()
                        .color(pal.text),
                    );
                    ui.label(
                        egui::RichText::new(format!("{} MB", report.unassigned_mb))
                            .monospace()
                            .small()
                            .color(pal.text_dim),
                    );
                });
                ui.label(
                    egui::RichText::new(trfb(
                        lang,
                        "desktopApp.pipeline.vram.schedulerNote",
                        "将由调度器按 least_memory 落位",
                        &[],
                    ))
                    .small()
                    .color(pal.text_faint),
                );
                ui.add_space(4.0);
            }

            if capacities.is_empty() {
                ui.label(
                    egui::RichText::new(trfb(
                        lang,
                        "desktopApp.pipeline.vram.noDevices",
                        "暂无设备容量数据（等待仪表盘检测设备）",
                        &[],
                    ))
                    .small()
                    .color(pal.text_faint),
                );
            }

            if any_over {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(trfb(
                        lang,
                        "desktopApp.pipeline.vram.suggestion",
                        "建议：换更小变体 / 改绑其他设备 / 停掉占用显存的模块",
                        &[],
                    ))
                    .small()
                    .color(pal.danger),
                );
            }
            if !config.compute.allow_overcommit {
                ui.label(
                    egui::RichText::new(trfb(
                        lang,
                        "desktopApp.pipeline.vram.overcommitOff",
                        "allow_overcommit=false：超预算将阻止执行",
                        &[],
                    ))
                    .small()
                    .color(pal.text_faint),
                );
            }
        }
        Err(ep_core::pipeline::vram::VramBudgetError::CycleDetected) => {
            ui.label(
                egui::RichText::new(trfb(
                    lang,
                    "desktopApp.pipeline.vram.cycle",
                    "管线存在环，无法计算 VRAM 预算",
                    &[],
                ))
                .small()
                .color(pal.danger),
            );
        }
    }
}

/// VRAM 变体解析：pin（qualified@variant 或裸变体 id）→ active_models →
/// manifest default。与执行侧 §5.2 口径一致。
fn resolve_budget_variant(
    config: &AppConfig,
    data: &ModuleData,
    module_id: &str,
    pin: Option<&str>,
) -> String {
    if let Some(pin) = pin {
        if let Some(at) = pin.split_once('@') {
            if !at.1.is_empty() {
                return at.1.to_string();
            }
        } else if !pin.is_empty() {
            return pin.to_string();
        }
    }
    if let Some(mf) = data.manifest(module_id) {
        if let Some(id) = config.active_models.get(module_id) {
            if mf.models.iter().any(|m| &m.id == id) {
                return id.clone();
            }
        }
        return mf
            .models
            .iter()
            .find(|m| m.default)
            .or_else(|| mf.models.first())
            .map(|m| m.id.clone())
            .unwrap_or_default();
    }
    String::new()
}

// ── Canvas (interaction + paint) ──────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn draw_canvas(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    st: &mut VizState,
    pipeline: &Pipeline,
    size: egui::Vec2,
    echo: &HashMap<String, egui::Color32>,
    data: &ModuleData,
) {
    let (canvas_rect, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let origin = canvas_rect.min;

    // Zoom (scroll wheel)
    if resp.hovered() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll != 0.0 {
            st.zoom = (st.zoom + scroll * 0.001).clamp(ZOOM_MIN, ZOOM_MAX);
        }
    }

    // Pan (middle-button drag)
    if resp.dragged_by(egui::PointerButton::Middle) {
        st.offset += resp.drag_delta();
    }

    // ── 连线交互：释放 → 尝试建边 ──
    let mut connect_result: Option<(String, String)> = None;
    if st.pending_connect.is_some() {
        let released = ui.input(|i| i.pointer.any_released());
        let hover = ui.input(|i| i.pointer.hover_pos());
        if released {
            let from = st.pending_connect.take().unwrap();
            if let Some(hp) = hover {
                if let Some(to) = input_port_hit(pipeline, &st.positions, hp, origin, st.offset, st.zoom)
                {
                    connect_result = Some((from, to));
                }
            }
        }
    } else if resp.dragged_by(egui::PointerButton::Primary) {
        // 拖拽起点判定：输出端口 → 连线；否则 → 移动节点
        let press_origin = ui.input(|i| i.pointer.press_origin());
        let started_at_port = press_origin.and_then(|po| {
            output_port_hit(pipeline, &st.positions, po, origin, st.offset, st.zoom)
        });
        if let Some(from) = started_at_port {
            st.pending_connect = Some(from);
        } else if let Some(pp) = resp.interact_pointer_pos() {
            let cp = to_canvas(pp, origin, st.offset, st.zoom);
            if let Some(id) = hit_test(pipeline, &st.positions, cp) {
                let delta = resp.drag_delta() / st.zoom;
                if let Some(pos) = st.positions.get_mut(&id) {
                    *pos += delta;
                    st.dirty = true;
                }
            }
        }
    }
    if let Some((from, to)) = connect_result {
        try_connect(st, lang, pipeline, data, &from, &to);
    }

    // Click → select node / edge
    if resp.clicked_by(egui::PointerButton::Primary) {
        if let Some(pp) = resp.interact_pointer_pos() {
            let cp = to_canvas(pp, origin, st.offset, st.zoom);
            if let Some(id) = hit_test(pipeline, &st.positions, cp) {
                st.selected = Some(id);
                st.selected_edge = None;
            } else if let Some(edge) = edge_hit(pipeline, &st.positions, pp, origin, st.offset, st.zoom) {
                st.selected_edge = Some(edge);
                st.selected = None;
            } else {
                st.selected = None;
                st.selected_edge = None;
            }
        }
    }

    // Right-click → delete node
    if resp.clicked_by(egui::PointerButton::Secondary) {
        if let Some(pp) = resp.interact_pointer_pos() {
            let cp = to_canvas(pp, origin, st.offset, st.zoom);
            if let Some(id) = hit_test(pipeline, &st.positions, cp) {
                delete_node(st, &id);
            }
        }
    }

    // Delete key → remove selected edge（键盘输入焦点优先，防误删）
    if st.selected_edge.is_some() && !ui.ctx().wants_keyboard_input() {
        let del = ui.input(|i| {
            i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)
        });
        if del {
            if let Some(edge) = st.selected_edge.clone() {
                remove_edge(st, &edge);
            }
        }
    }

    // ── Paint ──
    let mut painter = ui.painter_at(canvas_rect);
    painter.rect_filled(canvas_rect, 0.0, pal.bg);
    painter.set_clip_rect(canvas_rect);

    draw_grid(&painter, pal, canvas_rect, st.offset, st.zoom);
    draw_edges(
        &painter,
        pal,
        pipeline,
        &st.positions,
        st.selected_edge.as_ref(),
        origin,
        st.offset,
        st.zoom,
    );

    // 进行中的连线预览（虚线）
    if let Some(from) = &st.pending_connect {
        if let Some(&npos) = st.positions.get(from) {
            let p0 = to_screen(
                egui::pos2(npos.x + NODE_W, npos.y + NODE_H * 0.5),
                origin,
                st.offset,
                st.zoom,
            );
            if let Some(hp) = ui.input(|i| i.pointer.hover_pos()) {
                draw_bezier_preview(&painter, pal, p0, hp);
            }
        }
    }

    for node in &pipeline.nodes {
        if let Some(&npos) = st.positions.get(&node.id) {
            let sel = st.selected.as_deref() == Some(node.id.as_str());
            let ring = echo.get(&node.id).copied();
            draw_node(&painter, lang, pal, node, npos, origin, st.offset, st.zoom, sel, ring);
        }
    }
}

// ── Ports & connection validation ─────────────────────────────────

/// 节点端口类型 (输入, 输出)：None = 无该端口（file_input 无入、file_output 无出）
fn node_port_types(node: &PipelineNode, data: &ModuleData) -> (Option<DataType>, Option<DataType>) {
    match &node.kind {
        NodeKind::Builtin { builtin } => match builtin.as_str() {
            "file_input" => (None, Some(DataType::File)),
            "file_output" => (Some(DataType::File), None),
            _ => (Some(DataType::File), Some(DataType::File)),
        },
        NodeKind::ExternalApi { .. } => (Some(DataType::Text), Some(DataType::Text)),
        NodeKind::Module {
            module_id,
            capability,
            ..
        } => match data.capability(module_id, capability) {
            Some(cap) => (Some(cap.input_type), Some(cap.output_type)),
            None => (None, None),
        },
    }
}

/// 屏幕点命中的输出端口所属节点（无输出端口的节点跳过，如 file_output）
fn output_port_hit(
    pipeline: &Pipeline,
    positions: &HashMap<String, egui::Pos2>,
    screen_pt: egui::Pos2,
    origin: egui::Pos2,
    offset: egui::Vec2,
    zoom: f32,
) -> Option<String> {
    for node in &pipeline.nodes {
        if port_types_for_draw(node).1.is_none() {
            continue;
        }
        let Some(&p) = positions.get(&node.id) else {
            continue;
        };
        let sp = to_screen(
            egui::pos2(p.x + NODE_W, p.y + NODE_H * 0.5),
            origin,
            offset,
            zoom,
        );
        if sp.distance(screen_pt) <= PORT_HIT {
            return Some(node.id.clone());
        }
    }
    None
}

/// 屏幕点命中的输入端口所属节点（无输入端口的节点跳过，如 file_input）
fn input_port_hit(
    pipeline: &Pipeline,
    positions: &HashMap<String, egui::Pos2>,
    screen_pt: egui::Pos2,
    origin: egui::Pos2,
    offset: egui::Vec2,
    zoom: f32,
) -> Option<String> {
    for node in &pipeline.nodes {
        if port_types_for_draw(node).0.is_none() {
            continue;
        }
        let Some(&p) = positions.get(&node.id) else {
            continue;
        };
        let sp = to_screen(
            egui::pos2(p.x, p.y + NODE_H * 0.5),
            origin,
            offset,
            zoom,
        );
        if sp.distance(screen_pt) <= PORT_HIT {
            return Some(node.id.clone());
        }
    }
    None
}

/// 连线尝试：自连/重边/成环/端口类型校验；结果写状态栏。
/// `pipeline` 为当前帧快照（只读），改动落在 st.pipeline。
fn try_connect(
    st: &mut VizState,
    lang: &str,
    pipeline: &Pipeline,
    data: &ModuleData,
    from: &str,
    to: &str,
) {
    let fail = |st: &mut VizState, msg: String| {
        st.validation_msg = Some(msg);
        st.validation_ok = false;
    };

    if from == to {
        fail(
            st,
            trfb(lang, "desktopApp.pipeline.connSelf", "不能连接到自身", &[]),
        );
        return;
    }
    let new_edge = Edge {
        from: (from.to_string(), "output".to_string()),
        to: (to.to_string(), "input".to_string()),
    };
    if pipeline.edges.contains(&new_edge) {
        fail(
            st,
            trfb(lang, "desktopApp.pipeline.connDup", "该连线已存在", &[]),
        );
        return;
    }
    if creates_cycle(pipeline, from, to) {
        fail(
            st,
            trfb(
                lang,
                "desktopApp.pipeline.connCycle",
                "连线会形成环，已拒绝",
                &[],
            ),
        );
        return;
    }
    // 端口类型校验（DataType::is_compatible_with）：
    // module 节点无清单时按通配（None）放行，不误拒未安装模块的编排。
    let out_type = pipeline
        .nodes
        .iter()
        .find(|n| n.id == from)
        .and_then(|n| node_port_types(n, data).1);
    let in_type = pipeline
        .nodes
        .iter()
        .find(|n| n.id == to)
        .and_then(|n| node_port_types(n, data).0);
    if let (Some(out_t), Some(in_t)) = (out_type, in_type) {
        if !out_t.is_compatible_with(&in_t) {
            fail(
                st,
                trfb(
                    lang,
                    "desktopApp.pipeline.connType",
                    "端口类型不兼容: {{from}} → {{to}}",
                    &[
                        ("from", &format!("{out_t:?}")),
                        ("to", &format!("{in_t:?}")),
                    ],
                ),
            );
            return;
        }
    }

    if let Some(p) = st.pipeline.as_mut() {
        p.edges.push(new_edge);
        st.dirty = true;
        st.validation_msg = Some(trfb(
            lang,
            "desktopApp.pipeline.connOk",
            "已连线",
            &[],
        ));
        st.validation_ok = true;
    }
}

/// 成环检测：添加 from→to 成环 ⟺ 已存在 to→…→from 路径
fn creates_cycle(pipeline: &Pipeline, from: &str, to: &str) -> bool {
    let mut stack: Vec<&str> = vec![to];
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    while let Some(cur) = stack.pop() {
        if cur == from {
            return true;
        }
        if !seen.insert(cur) {
            continue;
        }
        for e in &pipeline.edges {
            if e.from.0 == cur {
                stack.push(&e.to.0);
            }
        }
    }
    false
}

fn remove_edge(st: &mut VizState, edge: &Edge) {
    if let Some(p) = st.pipeline.as_mut() {
        p.edges.retain(|e| e != edge);
        st.selected_edge = None;
        st.dirty = true;
    }
}

fn delete_node(st: &mut VizState, node_id: &str) {
    if let Some(p) = st.pipeline.as_mut() {
        p.nodes.retain(|n| n.id != node_id);
        p.edges
            .retain(|e| e.from.0 != node_id && e.to.0 != node_id);
        st.positions.remove(node_id);
        st.drafts.remove(node_id);
        if st.selected.as_deref() == Some(node_id) {
            st.selected = None;
        }
        st.selected_edge = None;
        st.dirty = true;
    }
}

/// 连线命中检测：贝塞尔采样，屏幕空间距离阈值
fn edge_hit(
    pipeline: &Pipeline,
    positions: &HashMap<String, egui::Pos2>,
    screen_pt: egui::Pos2,
    origin: egui::Pos2,
    offset: egui::Vec2,
    zoom: f32,
) -> Option<Edge> {
    for edge in &pipeline.edges {
        let (Some(&from_pos), Some(&to_pos)) =
            (positions.get(&edge.from.0), positions.get(&edge.to.0))
        else {
            continue;
        };
        let (p0, p1, p2, p3) =
            edge_control_points(from_pos, to_pos, origin, offset, zoom);
        for pt in bezier_points(p0, p1, p2, p3) {
            if pt.distance(screen_pt) <= EDGE_HIT {
                return Some(edge.clone());
            }
        }
    }
    None
}

// ── Fit view ──────────────────────────────────────────────────────

/// 适配视图：计算所有节点包围盒，缩放至画布可容纳并居中内容。
fn apply_fit(st: &mut VizState, canvas_size: egui::Vec2) {
    if st.positions.is_empty() {
        st.zoom = 1.0;
        st.offset = egui::Vec2::ZERO;
        return;
    }

    let mut min = egui::pos2(f32::INFINITY, f32::INFINITY);
    let mut max = egui::pos2(f32::NEG_INFINITY, f32::NEG_INFINITY);
    for &p in st.positions.values() {
        min.x = min.x.min(p.x);
        min.y = min.y.min(p.y);
        max.x = max.x.max(p.x + NODE_W);
        max.y = max.y.max(p.y + NODE_H);
    }

    let (bw, bh) = (max.x - min.x, max.y - min.y);
    let zoom = f32::min(
        canvas_size.x / (bw + 120.0),
        canvas_size.y / (bh + 120.0),
    )
    .min(1.5)
    .clamp(ZOOM_MIN, ZOOM_MAX);

    let center = egui::pos2((min.x + max.x) * 0.5, (min.y + max.y) * 0.5);
    st.zoom = zoom;
    st.offset = egui::vec2(
        canvas_size.x * 0.5 - center.x * zoom,
        canvas_size.y * 0.5 - center.y * zoom,
    );
}

// ── Coordinate transforms & geometry ──────────────────────────────

fn to_screen(cp: egui::Pos2, origin: egui::Pos2, offset: egui::Vec2, zoom: f32) -> egui::Pos2 {
    egui::pos2(
        origin.x + cp.x * zoom + offset.x,
        origin.y + cp.y * zoom + offset.y,
    )
}

fn to_canvas(sp: egui::Pos2, origin: egui::Pos2, offset: egui::Vec2, zoom: f32) -> egui::Pos2 {
    egui::pos2(
        (sp.x - origin.x - offset.x) / zoom,
        (sp.y - origin.y - offset.y) / zoom,
    )
}

fn hit_test(
    pipeline: &Pipeline,
    positions: &HashMap<String, egui::Pos2>,
    canvas_pos: egui::Pos2,
) -> Option<String> {
    for node in pipeline.nodes.iter().rev() {
        if let Some(&p) = positions.get(&node.id) {
            let r = egui::Rect::from_min_size(p, egui::vec2(NODE_W, NODE_H));
            if r.contains(canvas_pos) {
                return Some(node.id.clone());
            }
        }
    }
    None
}

/// 连线的贝塞尔控制点（屏幕空间）：源右中 → 目标左中
fn edge_control_points(
    from_pos: egui::Pos2,
    to_pos: egui::Pos2,
    origin: egui::Pos2,
    offset: egui::Vec2,
    zoom: f32,
) -> (egui::Pos2, egui::Pos2, egui::Pos2, egui::Pos2) {
    let p0 = to_screen(
        egui::pos2(from_pos.x + NODE_W, from_pos.y + NODE_H * 0.5),
        origin,
        offset,
        zoom,
    );
    let p3 = to_screen(
        egui::pos2(to_pos.x, to_pos.y + NODE_H * 0.5),
        origin,
        offset,
        zoom,
    );
    let dx = (p3.x - p0.x).abs().max(60.0) * 0.45;
    let p1 = egui::pos2(p0.x + dx, p0.y);
    let p2 = egui::pos2(p3.x - dx, p3.y);
    (p0, p1, p2, p3)
}

/// 三次贝塞尔采样（21 点）
fn bezier_points(
    p0: egui::Pos2,
    p1: egui::Pos2,
    p2: egui::Pos2,
    p3: egui::Pos2,
) -> Vec<egui::Pos2> {
    const STEPS: usize = 20;
    let mut pts = Vec::with_capacity(STEPS + 1);
    for i in 0..=STEPS {
        let t = i as f32 / STEPS as f32;
        let u = 1.0 - t;
        pts.push(egui::pos2(
            u * u * u * p0.x + 3.0 * u * u * t * p1.x + 3.0 * u * t * t * p2.x + t * t * t * p3.x,
            u * u * u * p0.y + 3.0 * u * u * t * p1.y + 3.0 * u * t * t * p2.y + t * t * t * p3.y,
        ));
    }
    pts
}

// ── Auto layout ───────────────────────────────────────────────────

fn compute_layout(pipeline: &Pipeline) -> HashMap<String, egui::Pos2> {
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

// ── Task echo（节点状态回显） ─────────────────────────────────────

/// 按最新任务回显节点状态色：Completed → 全绿；Running/Failed →
/// 拓扑序前 completed_nodes 个节点绿、Failed 时下一个红（近似，
/// TaskSummary 不携带逐节点状态，接线 TaskDetail 后可精确化）。
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
                colors.insert(id.clone(), pal.success);
            }
        }
        TaskStatus::Running | TaskStatus::Failed(_) | TaskStatus::Cancelled => {
            for (i, id) in order.iter().enumerate() {
                if i < task.completed_nodes {
                    colors.insert(id.clone(), pal.success);
                } else if i == task.completed_nodes && matches!(task.status, TaskStatus::Failed(_))
                {
                    colors.insert(id.clone(), pal.danger);
                }
            }
        }
        TaskStatus::Pending => {}
    }
    colors
}

// ── Drawing ───────────────────────────────────────────────────────

fn draw_grid(painter: &egui::Painter, pal: &Palette, rect: egui::Rect, offset: egui::Vec2, zoom: f32) {
    let origin = rect.min;
    let tl = to_canvas(rect.min, origin, offset, zoom);
    let br = to_canvas(rect.max, origin, offset, zoom);
    let dot = pal.border;

    // P2 修复：步长按视口点数自适应（低 zoom / 大画布时放大网格间距），
    // 避免 4K 画布 + zoom=0.3 下每帧数万 circle_filled
    let step = grid_step(br.x - tl.x, br.y - tl.y);

    let sx = (tl.x / step).floor() * step;
    let sy = (tl.y / step).floor() * step;

    let mut x = sx;
    while x < br.x {
        let mut y = sy;
        while y < br.y {
            painter.circle_filled(to_screen(egui::pos2(x, y), origin, offset, zoom), 1.0, dot);
            y += step;
        }
        x += step;
    }
}

/// P2 修复：网格步长自适应 —— 视口内预估点数超过上限时按比例放大步长，
/// 使每帧绘制的网格点数量有界（返回 ≥ GRID_SPACING 的步长）。
fn grid_step(canvas_w: f32, canvas_h: f32) -> f32 {
    const MAX_GRID_DOTS: f32 = 4096.0;
    let raw_nx = (canvas_w / GRID_SPACING).ceil().max(1.0);
    let raw_ny = (canvas_h / GRID_SPACING).ceil().max(1.0);
    if raw_nx * raw_ny <= MAX_GRID_DOTS {
        return GRID_SPACING;
    }
    let mul = (raw_nx * raw_ny / MAX_GRID_DOTS).sqrt().ceil().max(1.0);
    GRID_SPACING * mul
}

#[allow(clippy::too_many_arguments)]
fn draw_edges(
    painter: &egui::Painter,
    pal: &Palette,
    pipeline: &Pipeline,
    positions: &HashMap<String, egui::Pos2>,
    selected_edge: Option<&Edge>,
    origin: egui::Pos2,
    offset: egui::Vec2,
    zoom: f32,
) {
    for edge in &pipeline.edges {
        let (Some(&from_pos), Some(&to_pos)) =
            (positions.get(&edge.from.0), positions.get(&edge.to.0))
        else {
            continue;
        };
        let (p0, p1, p2, p3) =
            edge_control_points(from_pos, to_pos, origin, offset, zoom);
        let is_sel = selected_edge == Some(edge);
        let color = if is_sel { pal.primary } else { pal.text_faint };
        let stroke = egui::Stroke::new(if is_sel { 3.0_f32 } else { 2.0_f32 }, color);

        let pts = bezier_points(p0, p1, p2, p3);
        for pair in pts.windows(2) {
            painter.line_segment([pair[0], pair[1]], stroke);
        }

        // Port dots at endpoints
        painter.circle_filled(p0, PORT_R * zoom, color);
        painter.circle_filled(p3, PORT_R * zoom, color);
    }
}

/// 进行中连线的虚线预览
fn draw_bezier_preview(
    painter: &egui::Painter,
    pal: &Palette,
    p0: egui::Pos2,
    target: egui::Pos2,
) {
    let dx = (target.x - p0.x).abs().max(60.0) * 0.45;
    let p1 = egui::pos2(p0.x + dx, p0.y);
    let p2 = egui::pos2(target.x - dx, target.y);
    let pts = bezier_points(p0, p1, p2, target);
    let stroke = egui::Stroke::new(2.0_f32, pal.primary);
    // 虚线：隔段绘制
    for (i, pair) in pts.windows(2).enumerate() {
        if i % 2 == 0 {
            painter.line_segment([pair[0], pair[1]], stroke);
        }
    }
    painter.circle_filled(p0, PORT_R, pal.primary);
}

#[allow(clippy::too_many_arguments)]
fn draw_node(
    painter: &egui::Painter,
    lang: &str,
    pal: &Palette,
    node: &ep_core::pipeline::dag::PipelineNode,
    canvas_pos: egui::Pos2,
    origin: egui::Pos2,
    offset: egui::Vec2,
    zoom: f32,
    selected: bool,
    ring: Option<egui::Color32>,
) {
    let tl = to_screen(canvas_pos, origin, offset, zoom);
    let w = NODE_W * zoom;
    let h = NODE_H * zoom;
    let title_h = TITLE_H * zoom;
    let rect = egui::Rect::from_min_size(tl, egui::vec2(w, h));
    let cr = 8.0 * zoom;

    // Body
    painter.rect_filled(rect, cr, pal.card);

    // Title bar (节点类型色)
    let kind_color = node_kind_color(pal, &node.kind);
    let title_rect = egui::Rect::from_min_size(tl, egui::vec2(w, title_h));
    painter.rect_filled(title_rect, cr, kind_color);
    // Patch bottom corners of title bar to be square
    let patch = egui::Rect::from_min_max(
        egui::pos2(tl.x, tl.y + title_h - cr),
        egui::pos2(tl.x + w, tl.y + title_h),
    );
    painter.rect_filled(patch, 0.0, kind_color);

    // Title text (白色，保证在类型色底上可读)
    let label = if node.label.is_empty() { &node.id } else { &node.label };
    painter.text(
        egui::pos2(tl.x + w * 0.5, tl.y + title_h * 0.5),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(12.0 * zoom.max(0.6)),
        egui::Color32::WHITE,
    );

    // Kind tag in body
    let (kind_str, _) = node_kind_info(lang, &node.kind);
    painter.text(
        egui::pos2(tl.x + 8.0 * zoom, tl.y + title_h + (h - title_h) * 0.5),
        egui::Align2::LEFT_CENTER,
        kind_str,
        egui::FontId::proportional(11.0 * zoom.max(0.6)),
        pal.text_dim,
    );

    // Port dots – 按端口存在性绘制（file_input 无入端口、file_output 无出端口）
    let port_y = tl.y + h * 0.5;
    let (in_type, out_type) = port_types_for_draw(node);
    if in_type.is_some() {
        painter.circle_filled(egui::pos2(tl.x, port_y), PORT_R * zoom, pal.text_faint);
    }
    if out_type.is_some() {
        painter.circle_filled(egui::pos2(tl.x + w, port_y), PORT_R * zoom, pal.text_faint);
    }

    // Selection border
    if selected {
        painter.rect_stroke(
            rect,
            cr,
            egui::Stroke::new(2.0_f32, pal.primary),
            egui::StrokeKind::Outside,
        );
    }
    // 任务状态回显环（与选中框共存，略外扩）
    if let Some(color) = ring {
        let outer = rect.expand(3.0);
        painter.rect_stroke(outer, cr, egui::Stroke::new(1.5_f32, color), egui::StrokeKind::Outside);
    }
}

/// 绘制用端口类型（无清单上下文：module 节点按双端口存在处理）
fn port_types_for_draw(node: &PipelineNode) -> (Option<DataType>, Option<DataType>) {
    match &node.kind {
        NodeKind::Builtin { builtin } => match builtin.as_str() {
            "file_input" => (None, Some(DataType::File)),
            "file_output" => (Some(DataType::File), None),
            _ => (Some(DataType::File), Some(DataType::File)),
        },
        NodeKind::ExternalApi { .. } => (Some(DataType::Text), Some(DataType::Text)),
        NodeKind::Module { .. } => (Some(DataType::File), Some(DataType::File)),
    }
}

// ── Actions ───────────────────────────────────────────────────────

fn load_pipeline(st: &mut VizState, lang: &str) {
    let path = std::path::Path::new(&st.file_path);
    match Pipeline::from_toml(path) {
        Ok(pipeline) => {
            let (msg, ok) = match pipeline.validate() {
                Ok(()) => (tr(lang, "desktopApp.pipeline.validationPassed", &[]), true),
                Err(errors) => (format_errors(&errors), false),
            };
            st.positions.clear();
            st.selected = None;
            st.selected_edge = None;
            st.drafts.clear();
            st.dirty = false;
            st.offset = egui::Vec2::ZERO;
            st.zoom = 1.0;
            st.validation_msg = Some(msg);
            st.validation_ok = ok;
            st.pipeline = Some(pipeline);
        }
        Err(e) => {
            st.validation_msg = Some(tr(
                lang,
                "desktopApp.pipeline.loadFailed",
                &[("detail", &e.to_string())],
            ));
            st.validation_ok = false;
            st.pipeline = None;
        }
    }
}

/// 新建空白管线：file_input → file_output
fn new_pipeline(st: &mut VizState) {
    let input = PipelineNode {
        id: "input".to_string(),
        kind: NodeKind::Builtin {
            builtin: "file_input".to_string(),
        },
        label: String::new(),
        params: Default::default(),
        position: None,
        timeout_secs: None,
        retry_count: None,
    };
    let output = PipelineNode {
        id: "output".to_string(),
        kind: NodeKind::Builtin {
            builtin: "file_output".to_string(),
        },
        label: String::new(),
        params: Default::default(),
        position: None,
        timeout_secs: None,
        retry_count: None,
    };
    let edge = Edge {
        from: ("input".to_string(), "output".to_string()),
        to: ("output".to_string(), "input".to_string()),
    };
    st.pipeline = Some(Pipeline {
        id: "new-pipeline".to_string(),
        name: String::new(),
        description: String::new(),
        nodes: vec![input, output],
        edges: vec![edge],
        max_instances: None,
        node_timeout_secs: None,
    });
    st.positions.clear();
    st.drafts.clear();
    st.selected = None;
    st.selected_edge = None;
    st.dirty = true;
    st.file_path.clear();
    st.validation_msg = None;
    st.offset = egui::Vec2::ZERO;
    st.zoom = 1.0;
}

fn validate_pipeline(st: &mut VizState, lang: &str) {
    match &st.pipeline {
        Some(p) => {
            let (msg, ok) = match p.validate() {
                Ok(()) => (tr(lang, "desktopApp.pipeline.validationPassed", &[]), true),
                Err(errors) => (format_errors(&errors), false),
            };
            st.validation_msg = Some(msg);
            st.validation_ok = ok;
        }
        None => {
            st.validation_msg = Some(tr(lang, "desktopApp.pipeline.loadFileFirst", &[]));
            st.validation_ok = false;
        }
    }
}

/// 执行（决策 2）：校验 → 选择输入文件 → 提交。
///
/// 提交通道接线点（Wave 3 C4↔C5 冻结契约）：`AppCmd::ExecutePipeline { pipeline }`
/// 变体由 C4 在 app.rs 提供（传内存对象，未保存修改也可执行）。本 worktree
/// 的 app.rs 尚为 S2 形状（无该变体），故此处保留就绪态提示；门禁期接线：
/// 在 `Some(tx)` 分支内发送 `AppCmd::ExecutePipeline { pipeline }` 即可。
/// P1 修复：把执行期选择的输入文件写入管线中全部 file_input 节点的
/// `params.path`（执行器 `execute_builtin_file_input` 以该键为源文件路径）。
/// 所选路径不再静默丢弃——提交执行时随管线一起携带。
fn apply_exec_input(pipeline: &mut Pipeline, file: &std::path::Path) {
    for node in &mut pipeline.nodes {
        if matches!(&node.kind, NodeKind::Builtin { builtin } if builtin == "file_input") {
            // 匿名访问 serde_json::Value（本项目不直接依赖 serde_json，经 Into 推断）
            node.params["path"] = file.to_string_lossy().into_owned().into();
        }
    }
}

fn execute_pipeline(
    st: &mut VizState,
    lang: &str,
    cmd_tx: Option<&UnboundedSender<AppCmd>>,
    pipeline: Pipeline,
) {
    if let Err(errors) = pipeline.validate() {
        st.validation_msg = Some(format_errors(&errors));
        st.validation_ok = false;
        return;
    }
    // 选择输入文件（file_input 节点的执行期输入）
    // P1 修复：所选路径在提交前写入 file_input 节点 params.path
    let pipeline = match rfd::FileDialog::new()
        .set_title(trfb(
            lang,
            "desktopApp.pipeline.pickInput",
            "选择执行输入文件",
            &[],
        ))
        .pick_file()
    {
        Some(file) => {
            st.exec_input = Some(file.clone());
            let mut p = pipeline;
            apply_exec_input(&mut p, &file);
            p
        }
        None => pipeline,
    };
    match cmd_tx {
        Some(tx) => {
            // 门禁接线完成（C4 冻结入口）：提交内存管线对象执行，进度见任务页
            if tx.send(AppCmd::ExecutePipeline { pipeline }).is_ok() {
                st.validation_msg = Some(trfb(
                    lang,
                    "desktopApp.pipeline.execSubmitted",
                    "已提交执行，进度见任务页",
                    &[],
                ));
                st.validation_ok = true;
            } else {
                st.validation_msg = Some(trfb(
                    lang,
                    "desktopApp.pipeline.execChannelClosed",
                    "执行通道不可用（后台循环已退出）",
                    &[],
                ));
                st.validation_ok = false;
            }
        }
        None => {
            st.validation_msg = Some(trfb(
                lang,
                "desktopApp.pipeline.execPendingWire",
                "已就绪：等待执行通道接线（AppCmd::ExecutePipeline）",
                &[],
            ));
            st.validation_ok = true;
        }
    }
}

/// 保存 TOML（决策 2）：序列化写回 file_path；空路径或"另存"走 rfd 对话框。
fn save_pipeline(st: &mut VizState, lang: &str, save_as: bool) {
    let Some(p) = st.pipeline.clone() else {
        st.validation_msg = Some(tr(lang, "desktopApp.pipeline.loadFileFirst", &[]));
        st.validation_ok = false;
        return;
    };

    // 画布坐标 → node.position（随保存落盘）
    let mut p = p;
    for node in &mut p.nodes {
        node.position = st.positions.get(&node.id).map(|pos| NodePosition {
            x: f64::from(pos.x),
            y: f64::from(pos.y),
        });
    }

    if save_as || st.file_path.trim().is_empty() {
        let mut dlg = rfd::FileDialog::new()
            .set_title(trfb(lang, "desktopApp.pipeline.saveTitle", "保存管线 TOML", &[]))
            .add_filter("TOML", &["toml"]);
        if !st.file_path.trim().is_empty() {
            dlg = dlg.set_file_name(st.file_path.trim());
        }
        match dlg.save_file() {
            Some(path) => {
                st.file_path = path.to_string_lossy().to_string();
            }
            None => return, // 取消
        }
    }

    match pipeline_to_toml(&p) {
        Ok(text) => {
            let path = PathBuf::from(st.file_path.trim());
            match std::fs::write(&path, text) {
                Ok(()) => {
                    st.dirty = false;
                    st.validation_msg = Some(trfb(
                        lang,
                        "desktopApp.pipeline.saved",
                        "已保存: {{path}}",
                        &[("path", &path.to_string_lossy())],
                    ));
                    st.validation_ok = true;
                }
                Err(e) => {
                    st.validation_msg = Some(trfb(
                        lang,
                        "desktopApp.pipeline.saveFailed",
                        "保存失败: {{detail}}",
                        &[("detail", &e.to_string())],
                    ));
                    st.validation_ok = false;
                }
            }
        }
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

// ── TOML serialization（§6.2 文件形状） ───────────────────────────

/// TOML 基本字符串转义：`\"` `\\` 控制字符（`\n` `\t` `\uXXXX`）
fn toml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// TOML 浮点格式化：整数值补 `.0`（TOML 浮点语法要求小数点或指数）
fn toml_float(f: f64) -> Result<String, String> {
    if !f.is_finite() {
        return Err(format!("non-finite float in params: {f}"));
    }
    if f.fract() == 0.0 && f.abs() < 9.0e15 {
        Ok(format!("{}.0", f as i64))
    } else {
        Ok(format!("{f}"))
    }
}

/// 发射 JSON 参数值为 TOML 值文本（标量 + 标量数组；嵌套结构不支持）。
///
/// 匿名访问 serde_json::Value（ep-desktop 无直接 serde_json 依赖）：
/// 经宏在调用点展开（表达式自带类型推断），null 键跳过。
fn emit_params_object(params: &ep_core::pipeline::dag::PipelineNode) -> Result<String, String> {
    let mut out = String::new();
    let Some(obj) = params.params.as_object() else {
        return Ok(out);
    };
    if obj.is_empty() {
        return Ok(out);
    }

    // 单值发射（宏：在调用点展开，借用表达式的类型推断，不命名 serde_json 类型）
    macro_rules! emit_scalar {
        ($v:expr) => {{
            if $v.is_null() {
                None
            } else if let Some(s) = $v.as_str() {
                Some(format!("\"{}\"", toml_escape(s)))
            } else if let Some(b) = $v.as_bool() {
                Some(b.to_string())
            } else if let Some(i) = $v.as_i64() {
                Some(i.to_string())
            } else if let Some(f) = $v.as_f64() {
                Some(toml_float(f)?)
            } else {
                return Err(
                    "unsupported param value (only scalar / array of scalars)".to_string(),
                );
            }
        }};
    }

    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort();
    for key in keys {
        let v = &obj[key];
        if v.is_null() {
            continue;
        }
        if let Some(arr) = v.as_array() {
            let mut items = Vec::new();
            for item in arr {
                if let Some(text) = emit_scalar!(item) {
                    items.push(text);
                }
            }
            out.push_str(&format!("{key} = [{}]\n", items.join(", ")));
        } else if let Some(text) = emit_scalar!(v) {
            out.push_str(&format!("{key} = {text}\n"));
        }
    }
    Ok(out)
}

/// 管线 → TOML 文本（`[pipeline]` + `[[nodes]]` + `[[edges]]` 文件形状；
/// §6.2 契约键：module 节点变体 pin 为 `model`；llm 节点优先 `kind = "llm"`）。
pub(crate) fn pipeline_to_toml(p: &Pipeline) -> Result<String, String> {
    let mut out = String::new();
    out.push_str("[pipeline]\n");
    out.push_str(&format!("id = \"{}\"\n", toml_escape(&p.id)));
    out.push_str(&format!("name = \"{}\"\n", toml_escape(&p.name)));
    if !p.description.is_empty() {
        out.push_str(&format!(
            "description = \"{}\"\n",
            toml_escape(&p.description)
        ));
    }

    for node in &p.nodes {
        out.push_str("\n[[nodes]]\n");
        out.push_str(&format!("id = \"{}\"\n", toml_escape(&node.id)));
        match &node.kind {
            NodeKind::Module {
                module_id,
                capability,
                model_id,
                device,
            } => {
                out.push_str("kind = \"module\"\n");
                out.push_str(&format!("module_id = \"{}\"\n", toml_escape(module_id)));
                out.push_str(&format!("capability = \"{}\"\n", toml_escape(capability)));
                if let Some(m) = model_id {
                    out.push_str(&format!("model = \"{}\"\n", toml_escape(m)));
                }
                if let Some(d) = device {
                    out.push_str(&format!("device = \"{}\"\n", toml_escape(d)));
                }
            }
            NodeKind::Builtin { builtin } => {
                out.push_str("kind = \"builtin\"\n");
                out.push_str(&format!("builtin = \"{}\"\n", toml_escape(builtin)));
            }
            NodeKind::ExternalApi {
                endpoint,
                api_key_env,
            } => {
                if endpoint.is_empty() {
                    // §6.7 新命名：base_url 在 params 中声明
                    out.push_str("kind = \"llm\"\n");
                } else {
                    out.push_str("kind = \"external_api\"\n");
                    out.push_str(&format!("endpoint = \"{}\"\n", toml_escape(endpoint)));
                }
                if let Some(k) = api_key_env {
                    out.push_str(&format!("api_key_env = \"{}\"\n", toml_escape(k)));
                }
            }
        }
        if !node.label.is_empty() {
            out.push_str(&format!("label = \"{}\"\n", toml_escape(&node.label)));
        }
        if let Some(pos) = &node.position {
            out.push_str(&format!(
                "position = {{ x = {}, y = {} }}\n",
                toml_float(pos.x)?,
                toml_float(pos.y)?
            ));
        }
        if let Some(t) = node.timeout_secs {
            out.push_str(&format!("timeout_secs = {t}\n"));
        }
        if let Some(r) = node.retry_count {
            out.push_str(&format!("retry_count = {r}\n"));
        }
        let params_text = emit_params_object(node)?;
        if !params_text.is_empty() {
            out.push_str("\n[nodes.params]\n");
            out.push_str(&params_text);
        }
    }

    for edge in &p.edges {
        out.push_str("\n[[edges]]\n");
        out.push_str(&format!(
            "from = [\"{}\", \"{}\"]\n",
            toml_escape(&edge.from.0),
            toml_escape(&edge.from.1)
        ));
        out.push_str(&format!(
            "to = [\"{}\", \"{}\"]\n",
            toml_escape(&edge.to.0),
            toml_escape(&edge.to.1)
        ));
    }

    Ok(out)
}

// ── Helpers ───────────────────────────────────────────────────────

/// 节点类型 → (本地化类型名, 详情)。详情内容为模块/模型 ID 与端点等数据，不翻译。
fn node_kind_info(lang: &str, kind: &NodeKind) -> (String, String) {
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
fn node_kind_color(pal: &Palette, kind: &NodeKind) -> egui::Color32 {
    match kind {
        NodeKind::Module { .. } => pal.primary,
        NodeKind::Builtin { .. } => NODE_COLOR_BUILTIN,
        NodeKind::ExternalApi { .. } => NODE_COLOR_API,
    }
}

fn format_errors(errors: &[ep_core::pipeline::dag::ValidationError]) -> String {
    errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_toml() -> &'static str {
        r#"
[pipeline]
id = "test-pipeline"
name = "Test pipeline"
description = "For unit tests"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
label = "Input"

[[nodes]]
id = "process"
kind = "module"
module_id = "faster-whisper"
capability = "transcribe"
label = "Transcribe"
model = "ep.systran.faster-whisper@medium"
device = "cuda:0"

[nodes.params]
language = "zh"
beam_size = 5
vad_filter = true
threshold = 0.5
args = ["-i", "{input}"]

[[nodes]]
id = "translate"
kind = "llm"
api_key_env = "OPENAI_API_KEY"

[nodes.params]
base_url = "https://api.openai.com/v1"
model = "gpt-4o-mini"
temperature = 0.3

[[nodes]]
id = "save"
kind = "builtin"
builtin = "file_output"
label = "Save"

[[edges]]
from = ["input", "output"]
to = ["process", "input"]

[[edges]]
from = ["process", "output"]
to = ["translate", "input"]

[[edges]]
from = ["translate", "output"]
to = ["save", "input"]
"#
    }

    #[test]
    fn toml_roundtrip_preserves_pipeline() {
        let pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        let text = pipeline_to_toml(&pipeline).expect("serialize");
        let again = Pipeline::from_toml_str(&text).expect("re-parse");
        assert_eq!(again.id, pipeline.id);
        assert_eq!(again.name, pipeline.name);
        assert_eq!(again.description, pipeline.description);
        assert_eq!(again.nodes, pipeline.nodes);
        assert_eq!(again.edges, pipeline.edges);
    }

    /// P1 回归：执行期选择的输入文件写入全部 file_input 节点的 params.path，
    /// 不再静默丢弃（提交执行时随管线携带，执行器以该键为源文件路径）。
    #[test]
    fn apply_exec_input_writes_path_to_all_file_input_nodes() {
        let mut pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        // 追加第二个 file_input 节点（多输入管线），并给首个节点预置旧路径
        pipeline.nodes[0].params["path"] = "old/path.wav".into();
        pipeline.nodes.push(PipelineNode {
            id: "input2".into(),
            kind: NodeKind::Builtin {
                builtin: "file_input".into(),
            },
            label: String::new(),
            params: Default::default(),
            position: None,
            timeout_secs: None,
            retry_count: None,
        });

        apply_exec_input(&mut pipeline, std::path::Path::new("C:\\data\\in.mp3"));

        for node in &pipeline.nodes {
            if matches!(&node.kind, NodeKind::Builtin { builtin } if builtin == "file_input") {
                assert_eq!(
                    node.params["path"].as_str(),
                    Some("C:\\data\\in.mp3"),
                    "file_input 节点 {} 的 params.path 必须写入所选输入文件",
                    node.id
                );
            }
        }
        // 非 file_input 节点不受影响
        let module = pipeline.nodes.iter().find(|n| n.id == "process").unwrap();
        assert!(module.params.get("path").is_none());
    }

    /// P2 回归：apply_draft 只覆盖草稿涉及的 schema 键，params 中的
    /// 非 schema 键（TOML 手写扩展键）经编辑后必须保留，不得静默丢失。
    #[test]
    fn apply_draft_preserves_non_schema_params_keys() {
        let mut pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        let node = pipeline.nodes.iter_mut().find(|n| n.id == "process").unwrap();
        // 预置 schema 外扩展键（模拟用户手写 TOML 的自定义参数）
        node.params["custom_extra"] = 42.into();
        node.params["language"] = "en".into();

        let draft = NodeDraft {
            params: vec![("language".to_string(), ParamDraft::Str("zh".to_string()))],
            ..Default::default()
        };
        apply_draft(node, &draft);

        assert_eq!(node.params["language"].as_str(), Some("zh"));
        assert_eq!(
            node.params["custom_extra"].as_i64(),
            Some(42),
            "非 schema 键 custom_extra 不得被草稿清空丢弃"
        );
    }

    #[test]
    fn toml_module_node_uses_contract_keys() {
        let pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        let text = pipeline_to_toml(&pipeline).unwrap();
        // §6.2：变体 pin 键为 model（绝不输出旧键 model_id）
        assert!(text.contains("model = \"ep.systran.faster-whisper@medium\""));
        assert!(!text.contains("model_id ="));
        assert!(text.contains("device = \"cuda:0\""));
    }

    #[test]
    fn toml_llm_kind_emitted_for_empty_endpoint() {
        let pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        let text = pipeline_to_toml(&pipeline).unwrap();
        assert!(text.contains("kind = \"llm\""));
        assert!(text.contains("api_key_env = \"OPENAI_API_KEY\""));
        // params 保留 base_url/model/temperature
        assert!(text.contains("base_url = \"https://api.openai.com/v1\""));
        assert!(text.contains("temperature = 0.3"));
    }

    #[test]
    fn toml_string_escaping_roundtrips() {
        // 引号/反斜杠/换行 + CJK
        let mut pipeline = Pipeline::from_toml_str(
            r#"
[pipeline]
id = "esc"
name = "a\"b\\c"
description = "line1\nline2 中文"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
"#,
        )
        .unwrap();
        // 源 TOML 已把 \" \\ 解析进字符串；此处再叠加换行
        pipeline.name = "a\"b\\c\nd".to_string();
        let text = pipeline_to_toml(&pipeline).unwrap();
        let again = Pipeline::from_toml_str(&text).unwrap();
        assert_eq!(again.name, "a\"b\\c\nd");
        assert_eq!(again.description, "line1\nline2 中文");
    }

    #[test]
    fn toml_position_serialized_when_present() {
        let mut pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        pipeline.nodes[0].position = Some(NodePosition { x: 12.0, y: 34.5 });
        let text = pipeline_to_toml(&pipeline).unwrap();
        assert!(text.contains("x = 12.0"), "整数值补 .0：{text}");
        assert!(text.contains("y = 34.5"));
        let again = Pipeline::from_toml_str(&text).unwrap();
        assert_eq!(again.nodes[0].position.as_ref().unwrap().x, 12.0);
    }

    #[test]
    fn toml_float_non_finite_rejected() {
        assert!(toml_float(f64::NAN).is_err());
        assert!(toml_float(f64::INFINITY).is_err());
        assert_eq!(toml_float(2.5).unwrap(), "2.5");
        assert_eq!(toml_float(7.0).unwrap(), "7.0");
    }

    /// P2 回归：网格点数有界 —— 4K 画布 + 极小 zoom（画布坐标数万像素）
    /// 时步长自适应放大，每帧点数不超过 MAX_GRID_DOTS（含取整放大余量）。
    #[test]
    fn grid_step_bounds_dot_count_on_huge_canvas() {
        const MAX_GRID_DOTS: f32 = 4096.0;
        // zoom=0.3 时 4K 画布 ≈ 12800×7070 画布坐标；再叠加大画布
        for (w, h) in [(12800.0, 7070.0), (3840.0, 2120.0), (40960.0, 40960.0), (800.0, 600.0)] {
            let step = grid_step(w, h);
            let nx = (w / step).ceil().max(1.0);
            let ny = (h / step).ceil().max(1.0);
            // ceil 取整的放大余量 ≤ 2 倍（mul 为整数，点数 ≤ 4 * MAX_GRID_DOTS）
            assert!(
                nx * ny <= MAX_GRID_DOTS * 4.0,
                "canvas {w}x{h}: 点数 {nx}x{ny} 超限（step={step}）"
            );
        }
        // 普通画布不受影响：仍用基础间距
        assert_eq!(grid_step(800.0, 600.0), GRID_SPACING);
        assert_eq!(grid_step(12800.0, 7070.0) % GRID_SPACING, 0.0);
    }

    #[test]
    fn toml_escape_control_chars() {
        assert_eq!(toml_escape("a\"b"), "a\\\"b");
        assert_eq!(toml_escape("a\\b"), "a\\\\b");
        assert_eq!(toml_escape("a\nb"), "a\\nb");
        assert_eq!(toml_escape("a\u{01}b"), "a\\u0001b");
    }

    #[test]
    fn unique_id_deduplicates() {
        let pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        assert_eq!(unique_id(&pipeline, "ffmpeg"), "ffmpeg");
        assert_eq!(unique_id(&pipeline, "input"), "input_2");
        assert_eq!(unique_id(&pipeline, "weird id!"), "weird-id-");
    }

    #[test]
    fn creates_cycle_detects_paths() {
        let pipeline = Pipeline::from_toml_str(sample_toml()).unwrap();
        // input→process→translate→save 已存在；save→input 会成环
        assert!(creates_cycle(&pipeline, "save", "input"));
        assert!(creates_cycle(&pipeline, "translate", "process"));
        // 反向添加不成环
        assert!(!creates_cycle(&pipeline, "save", "save2"));
        assert!(!creates_cycle(&pipeline, "input", "save"));
    }

    #[test]
    fn port_types_builtin_and_llm() {
        let file_input = PipelineNode {
            id: "i".into(),
            kind: NodeKind::Builtin {
                builtin: "file_input".into(),
            },
            label: String::new(),
            params: Default::default(),
            position: None,
            timeout_secs: None,
            retry_count: None,
        };
        let file_output = PipelineNode {
            id: "o".into(),
            kind: NodeKind::Builtin {
                builtin: "file_output".into(),
            },
            label: String::new(),
            params: Default::default(),
            position: None,
            timeout_secs: None,
            retry_count: None,
        };
        let llm = PipelineNode {
            id: "l".into(),
            kind: NodeKind::ExternalApi {
                endpoint: String::new(),
                api_key_env: None,
            },
            label: String::new(),
            params: Default::default(),
            position: None,
            timeout_secs: None,
            retry_count: None,
        };
        let data = ModuleData {
            discovered: vec![],
            loaded_at: std::time::Instant::now(),
        };
        assert_eq!(
            node_port_types(&file_input, &data),
            (None, Some(DataType::File))
        );
        assert_eq!(
            node_port_types(&file_output, &data),
            (Some(DataType::File), None)
        );
        assert_eq!(
            node_port_types(&llm, &data),
            (Some(DataType::Text), Some(DataType::Text))
        );
    }

    #[test]
    fn resolve_budget_variant_pin_rules() {
        let config = AppConfig::default();
        let data = ModuleData {
            discovered: vec![],
            loaded_at: std::time::Instant::now(),
        };
        // qualified@variant → variant
        assert_eq!(
            resolve_budget_variant(
                &config,
                &data,
                "m",
                Some("ep.systran.faster-whisper@medium")
            ),
            "medium"
        );
        // 裸变体 id → 原样
        assert_eq!(
            resolve_budget_variant(&config, &data, "m", Some("large-v3")),
            "large-v3"
        );
        // 无 pin 且无清单 → 空
        assert_eq!(resolve_budget_variant(&config, &data, "m", None), "");
    }

    #[test]
    fn toml_roundtrip_real_repo_pipelines() {
        // 仓库真实管线（若脱离完整仓库布局则跳过）：
        // 验证自研 TOML 发射器与 ep-core 解析器在真实数据上往返等价。
        let base =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/pipelines");
        for name in ["video_to_srt.toml", "audio_extract.toml"] {
            let path = base.join(name);
            if !path.exists() {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read pipeline");
            let pipeline =
                Pipeline::from_toml_str(&text).unwrap_or_else(|e| panic!("{name} parse: {e}"));
            let out =
                pipeline_to_toml(&pipeline).unwrap_or_else(|e| panic!("{name} emit: {e}"));
            let again = Pipeline::from_toml_str(&out)
                .unwrap_or_else(|e| panic!("{name} re-parse: {e}\n---\n{out}"));
            assert_eq!(again.id, pipeline.id, "{name}: id");
            assert_eq!(again.name, pipeline.name, "{name}: name");
            assert_eq!(again.description, pipeline.description, "{name}: description");
            assert_eq!(again.nodes, pipeline.nodes, "{name}: nodes");
            assert_eq!(again.edges, pipeline.edges, "{name}: edges");
        }
    }
}
