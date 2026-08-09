//! 节点 palette（拖放载荷源）+ 右侧参数面板 + 节点编辑器 + VRAM 账本。
//! 自 pipeline_editor.rs 拆分搬移；palette 条目升级为 click_and_drag
//! 并在拖拽时发布 [`super::PalettePayload`]（两态重做 §7.3 映射表 #9）。

use ep_core::config::AppConfig;
use ep_core::pipeline::dag::{NodeKind, Pipeline, PipelineNode};
use ep_core::pipeline::vram::{compute_budget, DeviceCapacity, VramBudgetError, VramBudgetReport, VramNodeEstimate};

use crate::pages::{draft_default, trfb, ModuleData, ParamDraft};
use crate::ui::{badge, danger_button, subtle_button, Palette};

use super::{edit, VizState, PalettePayload, NODE_COLOR_API, NODE_COLOR_BUILTIN};

// ── Three-panel layout ────────────────────────────────────────────

/// palette 栏宽
const PALETTE_W: f32 = 150.0;
/// 属性面板栏宽
const PANEL_W: f32 = 260.0;
/// 画布最小宽（低于此值触发断点降级）
const MIN_CANVAS_W: f32 = 200.0;

/// 三栏总宽计算（纯函数可测；D-1 修复）：
/// **保证 `left_w + right_w + chrome + canvas_w ≤ avail_x`**，画布吃剩余空间。
/// 窗口过窄时按断点降级（统一 UI 方案 §8）：先丢属性面板，再丢 palette，
/// 最窄时仅保留画布。
fn compute_column_layout(avail_x: f32) -> (f32, f32, f32) {
    // 栏间 chrome：两个分隔条（各 6px）+ 5 个条目间 4 道 item_spacing（默认各 8px）
    const CHROME: f32 = 2.0 * 6.0 + 4.0 * 8.0;
    if avail_x < 760.0 {
        // 最窄断点：仅画布
        return (0.0, 0.0, avail_x.max(MIN_CANVAS_W));
    }
    let mut left_w = PALETTE_W;
    let mut right_w = PANEL_W;
    let mut canvas_w = avail_x - left_w - right_w - CHROME;
    // 断点 1：空间不足时隐藏属性面板（窄窗口降级）
    if canvas_w < MIN_CANVAS_W {
        right_w = 0.0;
        canvas_w = avail_x - left_w - CHROME;
    }
    // 断点 2：仍不足则隐藏 palette
    if canvas_w < MIN_CANVAS_W {
        left_w = 0.0;
        canvas_w = avail_x - CHROME;
    }
    (left_w, right_w, canvas_w.max(MIN_CANVAS_W))
}

/// 绘制编辑器主区域（palette | 画布 | 参数面板），返回画布实际尺寸
///（供"适配视图"使用）。
///
/// D-1/D-2/D-5 修复要点：
/// 1. 三栏宽度经 [`compute_column_layout`] 纯函数钳制，总宽不超可用宽；
/// 2. palette/属性面板的 ScrollArea 内容继承父级 `ui.horizontal` 的
///    left_to_right 布局会把条目横向平铺并撑爆总宽（D-2 根因），必须
///    显式 `ui.vertical` 隔离并在作用域内 `set_width` 固定为栏宽。
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_main(
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
    let (left_w, right_w, canvas_w) = compute_column_layout(avail.x);
    let canvas_h = (avail.y - 4.0).max(200.0);
    let canvas_size = egui::vec2(canvas_w, canvas_h);

    // 任务回显：节点 → 状态色
    let echo = super::node_echo_colors(lang, pal, pipeline, tasks);

    if left_w == 0.0 && right_w == 0.0 {
        super::canvas::draw_canvas(ui, lang, pal, st, pipeline, canvas_size, &echo, data);
        return canvas_size;
    }

    ui.horizontal(|ui| {
        // Left panel – node palette（可点击添加 / 拖放建节点）
        if left_w > 0.0 {
            egui::ScrollArea::vertical().show(ui, |ui| {
                // 显式垂直隔离：ScrollArea 内容继承父级水平布局会横向平铺条目（D-2）
                ui.vertical(|ui| {
                    ui.set_width(left_w);
                    ui.set_min_height(canvas_h);
                    draw_palette(ui, lang, pal, st, data);
                });
            });
            ui.separator();
        }

        // Center – node canvas
        super::canvas::draw_canvas(ui, lang, pal, st, pipeline, canvas_size, &echo, data);

        // Right panel – pipeline properties + node editor + VRAM ledger
        if right_w > 0.0 {
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(right_w);
                    ui.set_min_height(canvas_h);
                    draw_right_panel(ui, lang, pal, st, config, data, devices);
                });
            });
        }
    });

    canvas_size
}

// ── Left panel: node palette（决策 2：可添加节点） ─────────────────

/// palette 条目（自绘整行；D-2 修复）：宽度恒为栏宽（`ui.available_width()`，
/// 由外层 `set_width` 钳制），垂直单列收纳，不受标签内容宽度影响。
/// 点击 = 画布内级联落位；拖拽 = 发布载荷，画布侧释放落点。
#[allow(clippy::too_many_arguments)]
fn palette_entry(
    ui: &mut egui::Ui,
    pal: &Palette,
    label: &str,
    color: egui::Color32,
    tip: String,
    payload: PalettePayload,
    add_here: impl FnOnce(&mut VizState),
    st: &mut VizState,
) {
    const ROW_H: f32 = 26.0;
    let width = ui.available_width().max(40.0);
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, ROW_H),
        egui::Sense::click_and_drag(),
    );
    if ui.is_rect_visible(rect) {
        let hovered = resp.hovered();
        let painter = ui.painter();
        painter.rect(
            rect.shrink(1.0),
            4.0,
            if hovered { pal.bg_raised } else { egui::Color32::TRANSPARENT },
            egui::Stroke::new(
                1.0_f32,
                if hovered { pal.border_glow } else { pal.border },
            ),
            egui::StrokeKind::Inside,
        );
        // 文本左对齐；超出栏宽由 ScrollArea 裁剪（不回撞布局）
        painter.text(
            egui::pos2(rect.min.x + 8.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(12.0),
            color,
        );
    }
    let resp = resp.on_hover_text(tip);
    // 点击添加（既有行为）
    if resp.clicked() {
        add_here(st);
    }
    // 拖拽发布载荷（映射表 #9）
    if resp.drag_started() {
        egui::DragAndDrop::set_payload(ui.ctx(), payload);
    }
}

fn draw_palette(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    st: &mut VizState,
    data: &ModuleData,
) {
    ui.strong(crate::i18n::tr(lang, "desktopApp.pipeline.nodePanel", &[]));
    ui.separator();

    // builtin 节点
    badge(ui, pal, NODE_COLOR_BUILTIN, crate::i18n::tr(lang, "desktopApp.pipeline.kindBuiltin", &[]));
    ui.add_space(4.0);
    let builtins: [(&str, &str); 3] = [
        ("file_input", "📥 file_input"),
        ("file_output", "📤 file_output"),
        ("ffmpeg", "🎞 ffmpeg"),
    ];
    let add_tip = trfb(lang, "desktopApp.palette.addTip", "点击添加 / 拖入画布建节点", &[]);
    for (builtin, label) in builtins {
        let tip = add_tip.clone();
        let b = builtin.to_string();
        let payload = PalettePayload::Builtin(builtin.to_string());
        palette_entry(
            ui,
            pal,
            label,
            pal.text,
            tip,
            payload,
            |st| edit::add_builtin_node(st, &b, None),
            st,
        );
    }
    // LLM（§6.7 builtin，OpenAI 兼容端点）
    palette_entry(
        ui,
        pal,
        "🤖 llm",
        NODE_COLOR_API,
        trfb(
            lang,
            "desktopApp.palette.llmTip",
            "OpenAI 兼容 LLM 节点（chat/completions）",
            &[],
        ),
        PalettePayload::Llm,
        |st| edit::add_llm_node(st, None),
        st,
    );

    ui.add_space(10.0);
    ui.separator();

    // 模块节点（数据驱动：manifest capabilities）
    badge(ui, pal, pal.primary, crate::i18n::tr(lang, "common.label.module", &[]));
    ui.add_space(4.0);
    let mut any = false;
    let caps: Vec<(String, String, String)> = data
        .manifests()
        .flat_map(|mf| {
            mf.interface.capabilities.iter().map(move |cap| {
                (
                    mf.module.id.clone(),
                    cap.name.clone(),
                    cap.description.clone(),
                )
            })
        })
        .collect();
    for (module_id, cap_name, desc) in caps {
        any = true;
        let label = format!("{}::{}", module_id, cap_name);
        let tip = if desc.is_empty() { add_tip.clone() } else { desc };
        let (m, c) = (module_id.clone(), cap_name.clone());
        let payload = PalettePayload::Module {
            module_id: module_id.clone(),
            capability: cap_name.clone(),
        };
        palette_entry(
            ui,
            pal,
            &label,
            pal.text,
            tip,
            payload,
            |st| edit::add_module_node(st, data, &m, &c, None),
            st,
        );
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
            "点击节点查看/编辑参数\n从右侧端口拖到目标左侧端口连线\n框选多选 · Delete 删除选中\n滚轮缩放（Ctrl 加速）· 中键平移\npalette 条目可拖入画布落点建节点",
            &[],
        ))
        .small()
        .color(pal.text_faint),
    );
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
            edit::remove_edge(st, &edge);
        }
    } else if st.multi_select.len() > 1 {
        // 框选多选态：展示计数 + 批量删除入口
        ui.label(
            egui::RichText::new(trfb(
                lang,
                "desktopApp.pipeline.multiSelected",
                "已选中 {{count}} 个节点",
                &[("count", &st.multi_select.len().to_string())],
            ))
            .color(pal.text_dim),
        );
        ui.add_space(4.0);
        if ui
            .add(danger_button(
                pal,
                trfb(lang, "desktopApp.pipeline.deleteSelected", "删除选中节点", &[]),
            ))
            .clicked()
        {
            edit::delete_selected(st);
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
            egui::RichText::new(crate::i18n::tr(lang, "desktopApp.pipeline.clickNodeHint", &[]))
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
    let (kind_str, detail) = super::node_kind_info(lang, &node.kind);
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
        edit::delete_node(st, &node_id);
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
                        crate::i18n::tr(lang, "desktopApp.pipeline.params", &[])
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
fn rebuild_module_draft(draft: &mut super::NodeDraft, data: &ModuleData, module_id: &str) {
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
                    .on_hover_text(crate::i18n::tr(lang, "common.action.delete", &[]))
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

/// 预算计算（纯函数）：管线节点估算 + 设备容量 → compute_budget。
/// 供 VRAM 账本渲染与执行对话框阻断校验共用（口径一致）。
pub(super) fn compute_vram_report(
    pipeline: &Pipeline,
    config: &AppConfig,
    data: &ModuleData,
    devices: Option<&crate::pages::DeviceSnapshot>,
) -> Result<VramBudgetReport, VramBudgetError> {
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

    compute_budget(&nodes, &edges, &capacities, config.compute.allow_overcommit)
}

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

    let capacities_empty = devices.map(|s| s.devices.is_empty()).unwrap_or(true);

    match compute_vram_report(pipeline, config, data, devices) {
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

            if capacities_empty {
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
        Err(VramBudgetError::CycleDetected) => {
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
pub(super) fn resolve_budget_variant(
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

#[cfg(test)]
mod tests {
    use super::*;

    /// D-1 回归：三栏总宽在任何窗口宽度下不超可用宽（含分隔条与间距
    /// chrome=44），画布宽不低于最小宽；窄窗口按断点降级。
    #[test]
    fn column_layout_total_width_never_exceeds_available() {
        const CHROME: f32 = 2.0 * 6.0 + 4.0 * 8.0;
        // 600px ~ 2560px 逐 50px 扫描（覆盖 1280/1600/1920 三档验收宽度）
        let mut w = 600.0_f32;
        while w <= 2560.0 {
            let (l, r, c) = compute_column_layout(w);
            let chrome = if l == 0.0 && r == 0.0 { 0.0 } else { CHROME };
            assert!(
                l + r + chrome + c <= w + 1e-3,
                "avail={w}: 总宽 {} 超可用宽",
                l + r + chrome + c
            );
            assert!(c >= MIN_CANVAS_W - 1e-3, "avail={w}: 画布宽 {c} 低于最小宽");
            assert!(
                l == 0.0 || l == PALETTE_W,
                "avail={w}: palette 宽只取 0 或栏宽"
            );
            assert!(
                r == 0.0 || r == PANEL_W,
                "avail={w}: 属性面板宽只取 0 或栏宽"
            );
            w += 50.0;
        }
    }

    /// 断点降级：760px 及以上三栏完整（当前阈值下逐级降级 guard 为
    /// 防御性保留，不可达）；低于 760px 直接降级为仅画布。
    #[test]
    fn column_layout_breakpoint_degradation() {
        // 主流分辨率（窗口 1280 → 中央可用约 1084）三栏完整
        assert_eq!(compute_column_layout(1084.0), (PALETTE_W, PANEL_W, 1084.0 - PALETTE_W - PANEL_W - 44.0));
        assert!(compute_column_layout(1600.0).1 > 0.0);
        // 760 = 三栏最小可用宽（150+260+200+44-... 恰为边界），仍三栏
        assert!(compute_column_layout(760.0).1 > 0.0, "760 仍三栏");
        // 低于 760：narrow 断点直接降级为仅画布
        assert_eq!(compute_column_layout(759.0), (0.0, 0.0, 759.0));
        assert_eq!(compute_column_layout(700.0), (0.0, 0.0, 700.0));
        assert_eq!(compute_column_layout(500.0), (0.0, 0.0, 500.0));
        // 极窄输入不低于画布最小宽
        assert_eq!(compute_column_layout(100.0), (0.0, 0.0, MIN_CANVAS_W));
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
}
