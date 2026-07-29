use eframe::egui;
use ep_core::pipeline::dag::{NodeKind, Pipeline, ValidationError};
use std::collections::HashMap;

const NODE_W: f32 = 160.0;
const NODE_H: f32 = 60.0;
const LAYER_GAP: f32 = 220.0;
const NODE_GAP: f32 = 80.0;
const TITLE_H: f32 = 24.0;
const PORT_R: f32 = 4.0;
const GRID_SPACING: f32 = 40.0;

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
    let mut st = ui.data(|d| d.get_temp::<VizState>(sid())).unwrap_or_default();

    // Toolbar
    ui.horizontal(|ui| {
        ui.label("管线文件:");
        ui.add(egui::TextEdit::singleline(&mut st.file_path).desired_width(250.0));
        if ui.button("加载 TOML").clicked() {
            load_pipeline(&mut st);
        }
        if ui.button("保存 TOML").clicked() {
            st.validation_msg = Some("保存功能尚未实现（缺少 toml 序列化依赖）".into());
        }
        if ui.button("验证").clicked() {
            validate_pipeline(&mut st);
        }
        if let Some(ref p) = st.pipeline {
            ui.separator();
            ui.strong(format!("管线: {}", p.name));
        }
    });
    ui.separator();

    // Main area
    if st.pipeline.is_none() {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.label(
                egui::RichText::new("加载管线 TOML 文件以查看可视化节点图")
                    .color(egui::Color32::from_gray(140)),
            );
        });
    } else {
        let pipeline = st.pipeline.clone().unwrap();
        if st.positions.is_empty() && !pipeline.nodes.is_empty() {
            st.positions = compute_layout(&pipeline);
        }
        draw_main(ui, &mut st, &pipeline);
    }

    // Status bar
    ui.separator();
    match &st.validation_msg {
        Some(msg) if msg.starts_with("验证通过") => {
            ui.colored_label(egui::Color32::from_rgb(80, 220, 80), msg);
        }
        Some(msg) => {
            ui.colored_label(egui::Color32::from_rgb(255, 100, 100), msg);
        }
        None => {
            ui.label("状态: 就绪");
        }
    }

    // Persist
    ui.data_mut(|d| *d.get_temp_mut_or_default::<VizState>(sid()) = st);
}

// ── Three-panel layout ────────────────────────────────────────────

fn draw_main(ui: &mut egui::Ui, st: &mut VizState, pipeline: &Pipeline) {
    let avail = ui.available_size();
    let left_w = 130.0;
    let right_w = 180.0;
    let canvas_w = (avail.x - left_w - right_w - 24.0).max(200.0);
    let canvas_h = (avail.y - 4.0).max(200.0);

    ui.horizontal(|ui| {
        // Left panel – node palette
        ui.vertical(|ui| {
            ui.set_width(left_w);
            ui.set_min_height(canvas_h);
            ui.strong("节点面板");
            ui.separator();
            ui.label("📦 模块");
            ui.label("⿻ 内置");
            ui.label("🌐 API");
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("点击节点查看详情\n拖拽移动节点\n滚轮缩放画布\n中键拖拽平移")
                    .small()
                    .color(egui::Color32::from_gray(110)),
            );
        });

        ui.separator();

        // Center – node canvas
        let (canvas_rect, resp) = ui.allocate_exact_size(
            egui::vec2(canvas_w, canvas_h),
            egui::Sense::click_and_drag(),
        );
        let origin = canvas_rect.min;

        // Zoom (scroll wheel)
        if resp.hovered() {
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll != 0.0 {
                st.zoom = (st.zoom + scroll * 0.001).clamp(0.3, 3.0);
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
        painter.rect_filled(canvas_rect, 0.0, egui::Color32::from_rgb(30, 30, 30));
        painter.set_clip_rect(canvas_rect);

        draw_grid(&painter, canvas_rect, st.offset, st.zoom);
        draw_edges(&painter, pipeline, &st.positions, origin, st.offset, st.zoom);

        for node in &pipeline.nodes {
            if let Some(&npos) = st.positions.get(&node.id) {
                let sel = st.selected.as_deref() == Some(node.id.as_str());
                draw_node(&painter, node, npos, origin, st.offset, st.zoom, sel);
            }
        }

        ui.separator();

        // Right panel – params
        ui.vertical(|ui| {
            ui.set_width(right_w);
            ui.set_min_height(canvas_h);
            ui.strong("参数面板");
            ui.separator();
            if let Some(ref sel_id) = st.selected {
                if let Some(node) = pipeline.nodes.iter().find(|n| n.id == *sel_id) {
                    ui.label(format!("ID: {}", node.id));
                    let (kind_str, detail) = node_kind_info(&node.kind);
                    ui.label(format!("类型: {kind_str}"));
                    if !node.label.is_empty() {
                        ui.label(format!("标签: {}", node.label));
                    }
                    ui.label(format!("详情: {detail}"));
                    if let Some(t) = node.timeout_secs {
                        ui.label(format!("超时: {t}s"));
                    }
                    if let Some(r) = node.retry_count {
                        ui.label(format!("重试: {r}"));
                    }
                    if let Some(obj) = node.params.as_object() {
                        if !obj.is_empty() {
                            ui.add_space(4.0);
                            ui.strong("参数:");
                            for (k, v) in obj {
                                ui.label(format!("  {k}: {v}"));
                            }
                        }
                    }
                }
            } else {
                ui.label(
                    egui::RichText::new("点击节点查看参数")
                        .color(egui::Color32::from_gray(120)),
                );
            }
        });
    });
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
            if let Some([x, y]) = n.position {
                pos.insert(n.id.clone(), egui::pos2(x, y));
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

fn draw_grid(painter: &egui::Painter, rect: egui::Rect, offset: egui::Vec2, zoom: f32) {
    let origin = rect.min;
    let tl = to_canvas(rect.min, origin, offset, zoom);
    let br = to_canvas(rect.max, origin, offset, zoom);
    let dot = egui::Color32::from_gray(50);

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
    pipeline: &Pipeline,
    positions: &HashMap<String, egui::Pos2>,
    origin: egui::Pos2,
    offset: egui::Vec2,
    zoom: f32,
) {
    let edge_color = egui::Color32::from_rgb(150, 150, 150);
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

fn draw_node(
    painter: &egui::Painter,
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
    painter.rect_filled(rect, cr, egui::Color32::from_rgb(45, 45, 50));

    // Title bar
    let title_rect = egui::Rect::from_min_size(tl, egui::vec2(w, title_h));
    painter.rect_filled(title_rect, cr, node_kind_color(&node.kind));
    // Patch bottom corners of title bar to be square
    let patch = egui::Rect::from_min_max(
        egui::pos2(tl.x, tl.y + title_h - cr),
        egui::pos2(tl.x + w, tl.y + title_h),
    );
    painter.rect_filled(patch, 0.0, node_kind_color(&node.kind));

    // Title text
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
        egui::Color32::from_gray(180),
    );

    // Port dots – input (left) and output (right)
    let port_y = tl.y + h * 0.5;
    let port_color = egui::Color32::from_gray(200);
    painter.circle_filled(egui::pos2(tl.x, port_y), PORT_R * zoom, port_color);
    painter.circle_filled(egui::pos2(tl.x + w, port_y), PORT_R * zoom, port_color);

    // Selection border
    if selected {
        painter.rect_stroke(
            rect,
            cr,
            egui::Stroke::new(2.0_f32, egui::Color32::WHITE),
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

fn node_kind_color(kind: &NodeKind) -> egui::Color32 {
    match kind {
        NodeKind::Module { .. } => egui::Color32::from_rgb(59, 130, 246),
        NodeKind::Builtin { .. } => egui::Color32::from_rgb(139, 92, 246),
        NodeKind::ExternalApi { .. } => egui::Color32::from_rgb(249, 115, 22),
    }
}

fn format_errors(errors: &[ValidationError]) -> String {
    errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}
