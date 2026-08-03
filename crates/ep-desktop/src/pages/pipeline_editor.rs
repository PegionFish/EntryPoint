//! 管线编辑器 — 加载 / 校验 / 可视化管线 TOML 的节点画布。
//!
//! 配色统一来自 [`crate::ui::Palette`]（节点类型色除外），深浅主题均可用。

use eframe::egui;
use ep_core::pipeline::dag::{NodeKind, Pipeline, ValidationError};
use std::collections::HashMap;

use crate::ui::{badge, empty_state, primary_button, subtle_button, Palette};

const NODE_W: f32 = 160.0;
const NODE_H: f32 = 60.0;
const LAYER_GAP: f32 = 220.0;
const NODE_GAP: f32 = 80.0;
const TITLE_H: f32 = 24.0;
const PORT_R: f32 = 4.0;
const GRID_SPACING: f32 = 40.0;

/// 节点类型色：内置（紫）
const NODE_COLOR_BUILTIN: egui::Color32 = egui::Color32::from_rgb(139, 92, 246);
/// 节点类型色：外部 API（橙）
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
    validation_msg: Option<String>,
    positions: HashMap<String, egui::Pos2>,
    selected: Option<String>,
    offset: egui::Vec2,
    zoom: f32,
}

impl Default for VizState {
    fn default() -> Self {
        Self {
            file_path: String::new(),
            pipeline: None,
            validation_msg: None,
            positions: HashMap::new(),
            selected: None,
            offset: egui::Vec2::ZERO,
            zoom: 1.0,
        }
    }
}

fn sid() -> egui::Id {
    egui::Id::new("pipeline_viz_state")
}

// ── Page entry ────────────────────────────────────────────────────

pub fn show(ui: &mut egui::Ui) {
    let pal = Palette::new(ui.style().visuals.dark_mode);
    let mut st = ui.data(|d| d.get_temp::<VizState>(sid())).unwrap_or_default();

    // 是否需要在本帧执行"适配视图"（由工具栏触发，画布布局后应用）
    let mut do_fit = false;

    // Toolbar
    ui.horizontal(|ui| {
        ui.label("管线文件:");
        let path_w = (ui.available_width() - 430.0).clamp(140.0, 320.0);
        ui.add(egui::TextEdit::singleline(&mut st.file_path).desired_width(path_w));
        if ui.add(primary_button(&pal, "加载 TOML")).clicked() {
            load_pipeline(&mut st);
        }
        if ui.add(subtle_button(&pal, "验证")).clicked() {
            validate_pipeline(&mut st);
        }
        if ui
            .add(subtle_button(&pal, "保存 TOML"))
            .on_hover_text("尚未支持")
            .clicked()
        {
            st.validation_msg = Some("保存功能尚未实现（缺少 toml 序列化依赖）".into());
        }
        if let Some(ref p) = st.pipeline {
            ui.separator();
            ui.strong(format!("管线: {}", p.name));
        }

        // 右侧：视图缩放控件
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(subtle_button(&pal, "−")).on_hover_text("缩小").clicked() {
                st.zoom = (st.zoom / ZOOM_STEP).clamp(ZOOM_MIN, ZOOM_MAX);
            }
            if ui.add(subtle_button(&pal, "＋")).on_hover_text("放大").clicked() {
                st.zoom = (st.zoom * ZOOM_STEP).clamp(ZOOM_MIN, ZOOM_MAX);
            }
            if ui
                .add(subtle_button(&pal, "⤢ 适配"))
                .on_hover_text("缩放至完整显示所有节点")
                .clicked()
            {
                do_fit = true;
            }
        });
    });
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
            "尚未加载管线",
            "加载管线 TOML 文件以查看可视化节点图",
        );
    } else {
        let pipeline = st.pipeline.clone().unwrap();
        if st.positions.is_empty() && !pipeline.nodes.is_empty() {
            st.positions = compute_layout(&pipeline);
        }
        let canvas_size = draw_main(ui, &pal, &mut st, &pipeline);
        if do_fit {
            apply_fit(&mut st, canvas_size);
        }
    }

    // Status bar
    ui.separator();
    match &st.validation_msg {
        Some(msg) if msg.starts_with("验证通过") => {
            ui.colored_label(pal.success, msg.as_str());
        }
        Some(msg) => {
            ui.colored_label(pal.danger, msg.as_str());
        }
        None => {
            ui.colored_label(pal.text_dim, "状态: 就绪");
        }
    }

    // Persist
    ui.data_mut(|d| *d.get_temp_mut_or_default::<VizState>(sid()) = st);
}

// ── Three-panel layout ────────────────────────────────────────────

/// 绘制主区域，返回画布实际尺寸（供"适配视图"使用）。
fn draw_main(
    ui: &mut egui::Ui,
    pal: &Palette,
    st: &mut VizState,
    pipeline: &Pipeline,
) -> egui::Vec2 {
    let avail = ui.available_size();
    let narrow = ui.available_width() < 640.0;

    // 响应式：narrow 时隐藏左右面板，只保留画布
    let (left_w, right_w) = if narrow { (0.0, 0.0) } else { (130.0, 190.0) };
    let chrome = if narrow { 0.0 } else { 24.0 };
    let canvas_w = (avail.x - left_w - right_w - chrome).max(200.0);
    let canvas_h = (avail.y - 4.0).max(200.0);
    let canvas_size = egui::vec2(canvas_w, canvas_h);

    if narrow {
        draw_canvas(ui, pal, st, pipeline, canvas_size);
        return canvas_size;
    }

    ui.horizontal(|ui| {
        // Left panel – node palette
        ui.vertical(|ui| {
            ui.set_width(left_w);
            ui.set_min_height(canvas_h);
            ui.strong("节点面板");
            ui.separator();
            badge(ui, pal, pal.primary, "模块");
            ui.add_space(4.0);
            badge(ui, pal, NODE_COLOR_BUILTIN, "内置");
            ui.add_space(4.0);
            badge(ui, pal, NODE_COLOR_API, "API");
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("点击节点查看详情\n拖拽移动节点\n滚轮缩放画布\n中键拖拽平移")
                    .small()
                    .color(pal.text_faint),
            );
        });

        ui.separator();

        // Center – node canvas
        draw_canvas(ui, pal, st, pipeline, canvas_size);

        ui.separator();

        // Right panel – params
        ui.vertical(|ui| {
            ui.set_width(right_w);
            ui.set_min_height(canvas_h);
            ui.strong("参数面板");
            ui.separator();
            draw_params(ui, pal, pipeline, st.selected.as_deref());
        });
    });

    canvas_size
}

// ── Canvas (interaction + paint) ──────────────────────────────────

fn draw_canvas(
    ui: &mut egui::Ui,
    pal: &Palette,
    st: &mut VizState,
    pipeline: &Pipeline,
    size: egui::Vec2,
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

    // Click → select
    if resp.clicked_by(egui::PointerButton::Primary) {
        if let Some(pp) = resp.interact_pointer_pos() {
            let cp = to_canvas(pp, origin, st.offset, st.zoom);
            st.selected = hit_test(pipeline, &st.positions, cp);
        }
    }

    // Drag → move node
    if resp.dragged_by(egui::PointerButton::Primary) {
        if let Some(pp) = resp.interact_pointer_pos() {
            let cp = to_canvas(pp, origin, st.offset, st.zoom);
            if let Some(id) = hit_test(pipeline, &st.positions, cp) {
                let delta = resp.drag_delta() / st.zoom;
                if let Some(pos) = st.positions.get_mut(&id) {
                    *pos += delta;
                }
            }
        }
    }

    // ── Paint ──
    let mut painter = ui.painter_at(canvas_rect);
    painter.rect_filled(canvas_rect, 0.0, pal.bg);
    painter.set_clip_rect(canvas_rect);

    draw_grid(&painter, pal, canvas_rect, st.offset, st.zoom);
    draw_edges(&painter, pal, pipeline, &st.positions, origin, st.offset, st.zoom);

    for node in &pipeline.nodes {
        if let Some(&npos) = st.positions.get(&node.id) {
            let sel = st.selected.as_deref() == Some(node.id.as_str());
            draw_node(&painter, pal, node, npos, origin, st.offset, st.zoom, sel);
        }
    }
}

// ── Right panel: node parameters ──────────────────────────────────

fn draw_params(ui: &mut egui::Ui, pal: &Palette, pipeline: &Pipeline, selected: Option<&str>) {
    let Some(node) = selected.and_then(|id| pipeline.nodes.iter().find(|n| n.id == id)) else {
        ui.label(egui::RichText::new("点击节点查看参数").color(pal.text_faint));
        return;
    };

    kv_row(ui, pal, "ID", &node.id);
    let (kind_str, detail) = node_kind_info(&node.kind);
    kv_row(ui, pal, "类型", kind_str);
    if !node.label.is_empty() {
        kv_row(ui, pal, "标签", &node.label);
    }
    kv_row(ui, pal, "详情", &detail);
    if let Some(t) = node.timeout_secs {
        kv_row(ui, pal, "超时", &format!("{t}s"));
    }
    if let Some(r) = node.retry_count {
        kv_row(ui, pal, "重试", &r.to_string());
    }
    if let Some(obj) = node.params.as_object() {
        if !obj.is_empty() {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("参数:").strong().color(pal.text_dim));
            for (k, v) in obj {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(format!("{k}:")).color(pal.text_faint));
                    ui.label(egui::RichText::new(v.to_string()).color(pal.text));
                });
            }
        }
    }
}

/// 键值行：弱化键名 + 正常值
fn kv_row(ui: &mut egui::Ui, pal: &Palette, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("{key}:")).color(pal.text_dim));
        ui.label(egui::RichText::new(value).color(pal.text));
    });
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

// ── Coordinate transforms ─────────────────────────────────────────

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

// ── Drawing ───────────────────────────────────────────────────────

fn draw_grid(painter: &egui::Painter, pal: &Palette, rect: egui::Rect, offset: egui::Vec2, zoom: f32) {
    let origin = rect.min;
    let tl = to_canvas(rect.min, origin, offset, zoom);
    let br = to_canvas(rect.max, origin, offset, zoom);
    let dot = pal.border;

    let sx = (tl.x / GRID_SPACING).floor() * GRID_SPACING;
    let sy = (tl.y / GRID_SPACING).floor() * GRID_SPACING;

    let mut x = sx;
    while x < br.x {
        let mut y = sy;
        while y < br.y {
            painter.circle_filled(to_screen(egui::pos2(x, y), origin, offset, zoom), 1.0, dot);
            y += GRID_SPACING;
        }
        x += GRID_SPACING;
    }
}

fn draw_edges(
    painter: &egui::Painter,
    pal: &Palette,
    pipeline: &Pipeline,
    positions: &HashMap<String, egui::Pos2>,
    origin: egui::Pos2,
    offset: egui::Vec2,
    zoom: f32,
) {
    let edge_color = pal.text_faint;
    let stroke = egui::Stroke::new(2.0_f32, edge_color);

    for edge in &pipeline.edges {
        let (Some(&from_pos), Some(&to_pos)) =
            (positions.get(&edge.from.0), positions.get(&edge.to.0))
        else {
            continue;
        };

        // Output port: right-center of source; input port: left-center of target
        let p0 = to_screen(
            egui::pos2(from_pos.x + NODE_W, from_pos.y + NODE_H * 0.5),
            origin, offset, zoom,
        );
        let p3 = to_screen(
            egui::pos2(to_pos.x, to_pos.y + NODE_H * 0.5),
            origin, offset, zoom,
        );

        // Cubic bezier approximation with line segments
        let dx = (p3.x - p0.x).abs().max(60.0) * 0.45;
        let p1 = egui::pos2(p0.x + dx, p0.y);
        let p2 = egui::pos2(p3.x - dx, p3.y);

        const STEPS: usize = 20;
        let mut prev = p0;
        for i in 1..=STEPS {
            let t = i as f32 / STEPS as f32;
            let u = 1.0 - t;
            let pt = egui::pos2(
                u * u * u * p0.x + 3.0 * u * u * t * p1.x + 3.0 * u * t * t * p2.x + t * t * t * p3.x,
                u * u * u * p0.y + 3.0 * u * u * t * p1.y + 3.0 * u * t * t * p2.y + t * t * t * p3.y,
            );
            painter.line_segment([prev, pt], stroke);
            prev = pt;
        }

        // Port dots at endpoints
        painter.circle_filled(p0, PORT_R * zoom, edge_color);
        painter.circle_filled(p3, PORT_R * zoom, edge_color);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_node(
    painter: &egui::Painter,
    pal: &Palette,
    node: &ep_core::pipeline::dag::PipelineNode,
    canvas_pos: egui::Pos2,
    origin: egui::Pos2,
    offset: egui::Vec2,
    zoom: f32,
    selected: bool,
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
    let (kind_str, _) = node_kind_info(&node.kind);
    painter.text(
        egui::pos2(tl.x + 8.0 * zoom, tl.y + title_h + (h - title_h) * 0.5),
        egui::Align2::LEFT_CENTER,
        kind_str,
        egui::FontId::proportional(11.0 * zoom.max(0.6)),
        pal.text_dim,
    );

    // Port dots – input (left) and output (right)
    let port_y = tl.y + h * 0.5;
    painter.circle_filled(egui::pos2(tl.x, port_y), PORT_R * zoom, pal.text_faint);
    painter.circle_filled(egui::pos2(tl.x + w, port_y), PORT_R * zoom, pal.text_faint);

    // Selection border
    if selected {
        painter.rect_stroke(
            rect,
            cr,
            egui::Stroke::new(2.0_f32, pal.primary),
            egui::StrokeKind::Outside,
        );
    }
}

// ── Actions ───────────────────────────────────────────────────────

fn load_pipeline(st: &mut VizState) {
    let path = std::path::Path::new(&st.file_path);
    match Pipeline::from_toml(path) {
        Ok(pipeline) => {
            let msg = match pipeline.validate() {
                Ok(()) => "验证通过".to_string(),
                Err(errors) => format_errors(&errors),
            };
            st.positions.clear();
            st.selected = None;
            st.offset = egui::Vec2::ZERO;
            st.zoom = 1.0;
            st.validation_msg = Some(msg);
            st.pipeline = Some(pipeline);
        }
        Err(e) => {
            st.validation_msg = Some(format!("加载失败: {e}"));
            st.pipeline = None;
        }
    }
}

fn validate_pipeline(st: &mut VizState) {
    match &st.pipeline {
        Some(p) => {
            st.validation_msg = Some(match p.validate() {
                Ok(()) => "验证通过".to_string(),
                Err(errors) => format_errors(&errors),
            });
        }
        None => {
            st.validation_msg = Some("请先加载管线文件".into());
        }
    }
}

// ── Helpers (kept from original) ──────────────────────────────────

fn node_kind_info(kind: &NodeKind) -> (&'static str, String) {
    match kind {
        NodeKind::Module {
            module_id,
            capability,
            model_id,
        } => (
            "模块",
            format!(
                "{}::{}{}",
                module_id,
                capability,
                model_id
                    .as_ref()
                    .map(|m| format!(" (model: {m})"))
                    .unwrap_or_default()
            ),
        ),
        NodeKind::Builtin { builtin } => ("内置", builtin.clone()),
        NodeKind::ExternalApi {
            endpoint, api_type, ..
        } => ("API", format!("{api_type}: {endpoint}")),
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

fn format_errors(errors: &[ValidationError]) -> String {
    errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}
