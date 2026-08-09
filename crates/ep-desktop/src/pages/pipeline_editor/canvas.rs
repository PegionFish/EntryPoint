//! 画布交互与绘制 — 自 pipeline_editor.rs 拆分搬移，并按两态重做 §7.3
//! 补齐 5 项 egui 自研替代：
//! 1. 缩放变换（滚轮/Ctrl+滚轮锚定指针，按钮锚定画布中心）；
//! 2. fit 视图（既有 apply_fit，经 Controls 按钮触发）；
//! 3. MiniMap（缩略矩形 + 视口框，点击/拖拽定位）；
//! 4. 框选（空白处拖拽矩形多选，Delete 批量删除）；
//! 5. palette 拖放建节点（DragAndDrop 载荷，画布内释放落点）。
//!
//! 视觉升级：节点 glass 卡片 + 四态描边辉光、连线 2px + 选中渐变高亮、
//! 运行中连线流光点（1.6s 周期近似）。

use std::collections::HashMap;

use ep_core::pipeline::dag::{Edge, Pipeline, PipelineNode};
use ep_core::types::DataType;

use crate::pages::{trfb, ModuleData};
use crate::ui::{color_with_alpha, glow_breath_alpha, Palette};

use super::{
    edit, VizState, PalettePayload, EDGE_HIT, GRID_SPACING, NODE_COLOR_API, NODE_COLOR_BUILTIN,
    NODE_H, NODE_W, PORT_HIT, PORT_R, TITLE_H, ZOOM_MAX, ZOOM_MIN, ZOOM_STEP,
};

// ── Canvas (interaction + paint) ──────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_canvas(
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

    // overlay 几何（缩放控件左下 / MiniMap 右下）——先算后用于交互门控
    let controls_rect = controls_overlay_rect(canvas_rect);
    let minimap_rect = minimap_frame_rect(canvas_rect);
    let pointer_pos = ui.input(|i| i.pointer.hover_pos());
    let over_overlay = pointer_pos
        .map(|p| controls_rect.contains(p) || minimap_rect.contains(p))
        .unwrap_or(false);

    // ── Zoom（滚轮锚定指针；Ctrl 加速，自研替代 #1）──
    if resp.hovered() && !over_overlay {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll != 0.0 {
            let speed = if ui.input(|i| i.modifiers.ctrl) {
                0.004
            } else {
                0.0015
            };
            let target = (st.zoom * (scroll * speed).exp()).clamp(ZOOM_MIN, ZOOM_MAX);
            if let Some(fp) = pointer_pos {
                zoom_at(st, origin, fp, target);
            }
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
    } else if resp.dragged_by(egui::PointerButton::Primary) && !over_overlay {
        // 拖拽起点判定：输出端口 → 连线；节点 → 移动（多选整体）；空白 → 框选
        let press_origin = ui.input(|i| i.pointer.press_origin());
        let started_at_port = press_origin.and_then(|po| {
            output_port_hit(pipeline, &st.positions, po, origin, st.offset, st.zoom)
        });
        if let Some(from) = started_at_port {
            st.pending_connect = Some(from);
            st.marquee = None;
        } else {
            let press_canvas = press_origin
                .map(|po| to_canvas(po, origin, st.offset, st.zoom));
            let pressed_node = press_canvas
                .and_then(|cp| hit_test(pipeline, &st.positions, cp));
            if let Some(id) = pressed_node {
                // 移动节点：在多选中则整体移动（框选后续操作）
                let delta = resp.drag_delta() / st.zoom;
                let group: Vec<String> = if st.multi_select.contains(&id) {
                    st.multi_select.clone()
                } else {
                    vec![id]
                };
                for gid in group {
                    if let Some(pos) = st.positions.get_mut(&gid) {
                        *pos += delta;
                    }
                }
                st.dirty = true;
                st.marquee = None;
            } else {
                // 框选（自研替代 #4）：空白处拖拽矩形
                if let (Some(pc), Some(pp)) = (press_canvas, resp.interact_pointer_pos()) {
                    let cur = to_canvas(pp, origin, st.offset, st.zoom);
                    st.marquee = Some((pc, cur));
                }
            }
        }
    }
    // 框选释放 → 相交节点入多选（最小尺寸守卫防点击误触）
    if st.marquee.is_some() {
        let released = ui.input(|i| i.pointer.any_released());
        if released || st.pending_connect.is_some() {
            if let Some((a, b)) = st.marquee.take() {
                let rect = marquee_rect(a, b);
                let screen_diag = (rect.width() * st.zoom).hypot(rect.height() * st.zoom);
                if screen_diag >= 8.0 {
                    let hits = marquee_hits(pipeline, &st.positions, rect);
                    if !hits.is_empty() {
                        st.multi_select = hits;
                        st.selected = None;
                        st.selected_edge = None;
                    }
                }
            }
        }
    }
    if let Some((from, to)) = connect_result {
        edit::try_connect(st, lang, pipeline, data, &from, &to);
    }

    // Click → select node / edge（overlay 区域让位）
    if resp.clicked_by(egui::PointerButton::Primary) && !over_overlay {
        if let Some(pp) = resp.interact_pointer_pos() {
            let cp = to_canvas(pp, origin, st.offset, st.zoom);
            if let Some(id) = hit_test(pipeline, &st.positions, cp) {
                st.selected = Some(id.clone());
                st.multi_select = vec![id];
                st.selected_edge = None;
            } else if let Some(edge) = edge_hit(pipeline, &st.positions, pp, origin, st.offset, st.zoom) {
                st.selected_edge = Some(edge);
                st.selected = None;
                st.multi_select.clear();
            } else {
                st.selected = None;
                st.selected_edge = None;
                st.multi_select.clear();
            }
        }
    }

    // Right-click → delete node
    if resp.clicked_by(egui::PointerButton::Secondary) && !over_overlay {
        if let Some(pp) = resp.interact_pointer_pos() {
            let cp = to_canvas(pp, origin, st.offset, st.zoom);
            if let Some(id) = hit_test(pipeline, &st.positions, cp) {
                edit::delete_node(st, &id);
            }
        }
    }

    // Delete/Backspace → 删除选中项（映射表 #7：边 → 多选 → 单选）
    let has_selection =
        st.selected_edge.is_some() || !st.multi_select.is_empty() || st.selected.is_some();
    if has_selection && !ui.ctx().wants_keyboard_input() {
        let del = ui.input(|i| {
            i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)
        });
        if del {
            edit::delete_selected(st);
        }
    }

    // ── palette 拖放落点（自研替代 #5）──
    let mut drop_payload: Option<PalettePayload> = None;
    if let Some(payload) = egui::DragAndDrop::payload::<PalettePayload>(ui.ctx()) {
        if let Some(pp) = pointer_pos {
            if canvas_rect.contains(pp) && st.pending_connect.is_none() {
                // 释放判定（egui pass 尾自动清 payload，这里显式 take 保证单次消费）
                if ui.input(|i| i.pointer.any_released()) {
                    drop_payload = Some((*payload).clone());
                }
            }
        }
    }
    if let Some(payload) = drop_payload.take() {
        egui::DragAndDrop::take_payload::<PalettePayload>(ui.ctx());
        if let Some(pp) = pointer_pos {
            if canvas_rect.contains(pp) {
                let mut cp = to_canvas(pp, origin, st.offset, st.zoom);
                // 落点居中于节点卡片
                cp.x -= NODE_W * 0.5;
                cp.y -= NODE_H * 0.5;
                match payload {
                    PalettePayload::Builtin(b) => edit::add_builtin_node(st, &b, Some(cp)),
                    PalettePayload::Llm => edit::add_llm_node(st, Some(cp)),
                    PalettePayload::Module {
                        module_id,
                        capability,
                    } => edit::add_module_node(st, data, &module_id, &capability, Some(cp)),
                }
            }
        }
    }

    // ── Paint ──
    let mut painter = ui.painter_at(canvas_rect);
    painter.rect_filled(canvas_rect, 0.0, pal.bg_base);
    painter.set_clip_rect(canvas_rect);

    draw_grid(&painter, pal, canvas_rect, st.offset, st.zoom);

    let now_ms = ui.input(|i| i.time) * 1000.0;
    let running = echo.values().any(|c| *c == pal.status_running);
    draw_edges(
        &painter,
        pal,
        pipeline,
        &st.positions,
        st.selected_edge.as_ref(),
        origin,
        st.offset,
        st.zoom,
        running,
        now_ms,
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
            let sel = st.selected.as_deref() == Some(node.id.as_str())
                || st.multi_select.iter().any(|id| id == &node.id);
            let ring = echo.get(&node.id).copied();
            draw_node(&painter, lang, pal, node, npos, origin, st.offset, st.zoom, sel, ring, now_ms);
        }
    }

    // 拖放幽灵预览（palette 载荷悬停画布时）
    if let Some(payload) = egui::DragAndDrop::payload::<PalettePayload>(ui.ctx()) {
        if let Some(pp) = pointer_pos {
            if canvas_rect.contains(pp) && st.pending_connect.is_none() {
                let rect = egui::Rect::from_center_size(
                    pp,
                    egui::vec2(NODE_W * st.zoom, NODE_H * st.zoom),
                );
                painter.rect(
                    rect,
                    8.0 * st.zoom,
                    color_with_alpha(pal.primary, 36),
                    egui::Stroke::new(1.5_f32, pal.primary),
                    egui::StrokeKind::Inside,
                );
                let _ = payload;
            }
        }
    }

    // 框选矩形（拖拽中）
    if let Some((a, b)) = st.marquee {
        let rect = marquee_rect(a, b);
        let srect = egui::Rect::from_min_max(
            to_screen(rect.min, origin, st.offset, st.zoom),
            to_screen(rect.max, origin, st.offset, st.zoom),
        );
        painter.rect(
            srect,
            2.0,
            color_with_alpha(pal.primary, 28),
            egui::Stroke::new(1.0_f32, pal.primary),
            egui::StrokeKind::Inside,
        );
    }

    // overlay：缩放控件（自研替代 #1/#2 入口）+ MiniMap（自研替代 #3）
    draw_controls_overlay(ui, lang, pal, st, origin, canvas_rect, controls_rect);
    draw_minimap(ui, lang, pal, st, pipeline, canvas_rect, minimap_rect);

    // 运行态动效：呼吸辉光/流光需持续重绘
    if running {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
    }
}

/// 缩放变换（锚定屏幕点 focus 不变）：zoom 改变后反解 offset
fn zoom_at(st: &mut VizState, origin: egui::Pos2, focus: egui::Pos2, target_zoom: f32) {
    let target_zoom = target_zoom.clamp(ZOOM_MIN, ZOOM_MAX);
    let cp = to_canvas(focus, origin, st.offset, st.zoom);
    st.offset = egui::vec2(
        focus.x - origin.x - cp.x * target_zoom,
        focus.y - origin.y - cp.y * target_zoom,
    );
    st.zoom = target_zoom;
}

/// 框选矩形归一化（起点/终点任意顺序 → min/max）
fn marquee_rect(a: egui::Pos2, b: egui::Pos2) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(a.x.min(b.x), a.y.min(b.y)),
        egui::pos2(a.x.max(b.x), a.y.max(b.y)),
    )
}

/// 框选命中的节点 id（画布坐标矩形与节点矩形相交；纯函数可测）
fn marquee_hits(
    pipeline: &Pipeline,
    positions: &HashMap<String, egui::Pos2>,
    rect: egui::Rect,
) -> Vec<String> {
    pipeline
        .nodes
        .iter()
        .filter_map(|node| {
            let p = positions.get(&node.id)?;
            let nr = egui::Rect::from_min_size(*p, egui::vec2(NODE_W, NODE_H));
            nr.intersects(rect).then(|| node.id.clone())
        })
        .collect()
}

// ── Controls overlay（− / ＋ / ⤢，自研替代 #1/#2） ─────────────────

/// 缩放控件 overlay 包围矩形（画布左下角竖排 3 键）
fn controls_overlay_rect(canvas_rect: egui::Rect) -> egui::Rect {
    const BTN: f32 = 28.0;
    const GAP: f32 = 6.0;
    const MARGIN: f32 = 12.0;
    let h = BTN * 3.0 + GAP * 2.0;
    egui::Rect::from_min_size(
        egui::pos2(canvas_rect.min.x + MARGIN, canvas_rect.max.y - MARGIN - h),
        egui::vec2(BTN, h),
    )
}

#[allow(clippy::too_many_arguments)]
fn draw_controls_overlay(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    st: &mut VizState,
    origin: egui::Pos2,
    canvas_rect: egui::Rect,
    wrap: egui::Rect,
) {
    let btn = wrap.width();
    let gap = 6.0;
    let ids = [
        ("pe_zoom_out", "−", crate::i18n::tr(lang, "desktopApp.pipeline.zoomOut", &[])),
        ("pe_zoom_in", "＋", crate::i18n::tr(lang, "desktopApp.pipeline.zoomIn", &[])),
        ("pe_zoom_fit", "⤢", crate::i18n::tr(lang, "desktopApp.pipeline.fitTip", &[])),
    ];
    // 按钮点击锚定画布中心缩放；fit 置 request_fit（布局后应用）
    let focus = origin + canvas_rect.size() * 0.5;
    for (i, (id, glyph, tip)) in ids.iter().enumerate() {
        let rect = egui::Rect::from_min_size(
            egui::pos2(wrap.min.x, wrap.min.y + i as f32 * (btn + gap)),
            egui::vec2(btn, btn),
        );
        let painter = ui.painter();
        let hovered = rect.contains(ui.input(|i| i.pointer.hover_pos()).unwrap_or(egui::pos2(-1.0, -1.0)));
        painter.rect(
            rect,
            6.0,
            if hovered { pal.bg_raised } else { pal.surface_glass },
            egui::Stroke::new(1.0_f32, pal.border_glow),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            *glyph,
            egui::FontId::proportional(14.0),
            pal.text,
        );
        let r = ui.interact(rect, egui::Id::new(id), egui::Sense::click());
        // D-4：自绘按钮补 a11y 名称（i18n 键与 hover 文案同源，避免无名 Custom）
        let a11y_name = tip.clone();
        r.widget_info(move || {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, a11y_name.clone())
        });
        let r = r.on_hover_text(tip.as_str());
        if r.clicked() {
            match *id {
                "pe_zoom_out" => zoom_at(st, origin, focus, st.zoom / ZOOM_STEP),
                "pe_zoom_in" => zoom_at(st, origin, focus, st.zoom * ZOOM_STEP),
                _ => st.request_fit = true,
            }
        }
        if hovered {
            ui.ctx().request_repaint();
        }
    }
}

// ── MiniMap（自研替代 #3：缩略矩形 + 视口框，点击/拖拽定位） ────────

const MINIMAP_W: f32 = 152.0;
const MINIMAP_H: f32 = 104.0;
const MINIMAP_MARGIN: f32 = 12.0;

fn minimap_frame_rect(canvas_rect: egui::Rect) -> egui::Rect {
    let frame = egui::Rect::from_min_size(
        egui::pos2(
            canvas_rect.max.x - MINIMAP_MARGIN - MINIMAP_W,
            canvas_rect.max.y - MINIMAP_MARGIN - MINIMAP_H,
        ),
        egui::vec2(MINIMAP_W, MINIMAP_H),
    );
    // D-3：钳制在画布可视区内（右下角悬浮），极小画布时不越界
    frame
        .translate(egui::vec2(
            (canvas_rect.min.x - frame.min.x).max(0.0),
            (canvas_rect.min.y - frame.min.y).max(0.0),
        ))
        .intersect(canvas_rect)
}

/// 内容包围盒 → 缩略映射 (scale, offset)：等比缩放 + 居中（纯函数可测）
fn minimap_transform(bbox: egui::Rect, map_size: egui::Vec2) -> (f32, egui::Vec2) {
    let bw = bbox.width().max(1.0);
    let bh = bbox.height().max(1.0);
    let pad = 8.0;
    let scale = ((map_size.x - pad * 2.0) / bw).min((map_size.y - pad * 2.0) / bh).max(0.0001);
    let offset = egui::vec2(
        (map_size.x - bw * scale) * 0.5,
        (map_size.y - bh * scale) * 0.5,
    );
    (scale, offset)
}

fn content_bbox(positions: &HashMap<String, egui::Pos2>) -> Option<egui::Rect> {
    if positions.is_empty() {
        return None;
    }
    let mut min = egui::pos2(f32::INFINITY, f32::INFINITY);
    let mut max = egui::pos2(f32::NEG_INFINITY, f32::NEG_INFINITY);
    for &p in positions.values() {
        min.x = min.x.min(p.x);
        min.y = min.y.min(p.y);
        max.x = max.x.max(p.x + NODE_W);
        max.y = max.y.max(p.y + NODE_H);
    }
    Some(egui::Rect::from_min_max(min, max))
}

#[allow(clippy::too_many_arguments)]
fn draw_minimap(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    st: &mut VizState,
    pipeline: &Pipeline,
    canvas_rect: egui::Rect,
    frame: egui::Rect,
) {
    let painter = ui.painter();
    painter.rect(
        frame,
        8.0,
        pal.surface_glass,
        egui::Stroke::new(1.0_f32, pal.border_glow),
        egui::StrokeKind::Inside,
    );

    let Some(bbox) = content_bbox(&st.positions) else {
        return;
    };
    let map_size = frame.size();
    let (scale, off) = minimap_transform(bbox, map_size);
    let to_map = |cp: egui::Pos2| {
        egui::pos2(
            frame.min.x + off.x + (cp.x - bbox.min.x) * scale,
            frame.min.y + off.y + (cp.y - bbox.min.y) * scale,
        )
    };

    // 节点缩略矩形（类型色）
    for node in &pipeline.nodes {
        if let Some(&p) = st.positions.get(&node.id) {
            let r = egui::Rect::from_min_max(
                to_map(p),
                to_map(egui::pos2(p.x + NODE_W, p.y + NODE_H)),
            );
            let r = egui::Rect::from_min_size(
                r.min,
                egui::vec2(r.width().max(2.0), r.height().max(2.0)),
            );
            painter.rect_filled(
                r,
                1.5,
                super::node_kind_color(pal, &node.kind),
            );
        }
    }

    // 视口框（画布可视区域 → 画布坐标 → 缩略坐标）
    let origin = canvas_rect.min;
    let vtl = to_canvas(canvas_rect.min, origin, st.offset, st.zoom);
    let vbr = to_canvas(canvas_rect.max, origin, st.offset, st.zoom);
    let vrect = egui::Rect::from_min_max(to_map(vtl), to_map(vbr));
    painter.rect(
        vrect,
        2.0,
        color_with_alpha(pal.primary, 24),
        egui::Stroke::new(1.5_f32, pal.primary),
        egui::StrokeKind::Inside,
    );

    // 点击/拖拽 → 视口居中于点击的画布坐标
    let r = ui.interact(frame, egui::Id::new("pe_minimap"), egui::Sense::click_and_drag());
    // D-4 同源：补 a11y 名称（避免无名 Custom）
    let minimap_name = trfb(lang, "desktopApp.pipeline.minimapTip", "点击/拖拽定位视口", &[]);
    let minimap_name2 = minimap_name.clone();
    r.widget_info(move || {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, minimap_name2.clone())
    });
    if r.clicked() || r.dragged() {
        if let Some(pp) = r.interact_pointer_pos() {
            let tc = egui::pos2(
                bbox.min.x + (pp.x - frame.min.x - off.x) / scale,
                bbox.min.y + (pp.y - frame.min.y - off.y) / scale,
            );
            st.offset = egui::vec2(
                canvas_rect.size().x * 0.5 - tc.x * st.zoom,
                canvas_rect.size().y * 0.5 - tc.y * st.zoom,
            );
        }
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(minimap_name);
}

// ── Ports & hit testing ───────────────────────────────────────────

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
///
/// D-6：`canvas_size` 必须是**当前可视区**尺寸（draw_main 钳制后的真实
/// 画布分配尺寸），不得用超宽虚拟画布作参照 —— 否则 fit 后节点仍被
/// 窗口右缘裁切。三栏总宽钳制修复（D-1）后此处拿到的即为可视区尺寸。
pub(super) fn apply_fit(st: &mut VizState, canvas_size: egui::Vec2) {
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

// ── Drawing ───────────────────────────────────────────────────────

fn draw_grid(painter: &egui::Painter, pal: &Palette, rect: egui::Rect, offset: egui::Vec2, zoom: f32) {
    let origin = rect.min;
    let tl = to_canvas(rect.min, origin, offset, zoom);
    let br = to_canvas(rect.max, origin, offset, zoom);
    let dot = pal.grid_dot;

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
    running: bool,
    now_ms: f64,
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

        let pts = bezier_points(p0, p1, p2, p3);
        if is_sel {
            // 选中连线：accent_at 渐变高亮（2.5px，映射表视觉项）
            let n = pts.len().max(2);
            for (i, pair) in pts.windows(2).enumerate() {
                let t = i as f32 / (n - 1) as f32;
                let stroke = egui::Stroke::new(2.5_f32, pal.accent_at(t));
                painter.line_segment([pair[0], pair[1]], stroke);
            }
            painter.circle_filled(p0, PORT_R * zoom, pal.primary);
            painter.circle_filled(p3, PORT_R * zoom, pal.accent_to);
        } else {
            let stroke = egui::Stroke::new(2.0_f32, pal.text_faint);
            for pair in pts.windows(2) {
                painter.line_segment([pair[0], pair[1]], stroke);
            }
            // Port dots at endpoints
            painter.circle_filled(p0, PORT_R * zoom, pal.text_faint);
            painter.circle_filled(p3, PORT_R * zoom, pal.text_faint);
        }

        // 运行流光点（§1 动效 1.6s 周期近似：贝塞尔参数 t 随时间推进）
        if running {
            let t = ((now_ms % 1600.0) / 1600.0) as f32;
            let idx = (t * (pts.len() - 1) as f32) as usize;
            if let Some(&fp) = pts.get(idx.min(pts.len() - 1)) {
                painter.circle_filled(fp, 3.0_f32.max(PORT_R * zoom), pal.status_running);
            }
        }
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
    node: &PipelineNode,
    canvas_pos: egui::Pos2,
    origin: egui::Pos2,
    offset: egui::Vec2,
    zoom: f32,
    selected: bool,
    ring: Option<egui::Color32>,
    now_ms: f64,
) {
    let tl = to_screen(canvas_pos, origin, offset, zoom);
    let w = NODE_W * zoom;
    let h = NODE_H * zoom;
    let title_h = TITLE_H * zoom;
    let rect = egui::Rect::from_min_size(tl, egui::vec2(w, h));
    let cr = 8.0 * zoom;

    // 选中态外圈辉光（primary_glow 扩圈近似发光）
    if selected && pal.primary_glow != egui::Color32::TRANSPARENT {
        painter.rect_stroke(
            rect.expand(3.0),
            cr + 2.0,
            egui::Stroke::new(4.0_f32, color_with_alpha(pal.primary, 70)),
            egui::StrokeKind::Outside,
        );
    }

    // Body（glass：抬升表面 + 内发光描边）
    painter.rect_filled(rect, cr, pal.bg_raised);
    painter.rect_stroke(
        rect,
        cr,
        egui::Stroke::new(1.0_f32, pal.border_glow),
        egui::StrokeKind::Inside,
    );

    // Title bar (节点类型色)
    let kind_color = node_kind_color_local(pal, &node.kind);
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
    let (kind_str, _) = super::node_kind_info(lang, &node.kind);
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
    // 任务状态回显环（四态描边 + 辉光外扩；running 呼吸，§1 动效 2.4s）
    if let Some(color) = ring {
        let breathing = color == pal.status_running;
        let glow_alpha = if breathing {
            (glow_breath_alpha(now_ms) * 140.0) as u8
        } else {
            80
        };
        let outer = rect.expand(3.0);
        painter.rect_stroke(
            outer.expand(2.0),
            cr + 2.0,
            egui::Stroke::new(3.0_f32, color_with_alpha(color, glow_alpha)),
            egui::StrokeKind::Outside,
        );
        painter.rect_stroke(outer, cr, egui::Stroke::new(1.5_f32, color), egui::StrokeKind::Outside);
    }
}

/// 节点类型色（画布局部）：与 mod.rs node_kind_color 同口径
fn node_kind_color_local(pal: &Palette, kind: &ep_core::pipeline::dag::NodeKind) -> egui::Color32 {
    match kind {
        ep_core::pipeline::dag::NodeKind::Module { .. } => pal.primary,
        ep_core::pipeline::dag::NodeKind::Builtin { .. } => NODE_COLOR_BUILTIN,
        ep_core::pipeline::dag::NodeKind::ExternalApi { .. } => NODE_COLOR_API,
    }
}

/// 绘制用端口类型（无清单上下文：module 节点按双端口存在处理）
fn port_types_for_draw(node: &PipelineNode) -> (Option<DataType>, Option<DataType>) {
    match &node.kind {
        ep_core::pipeline::dag::NodeKind::Builtin { builtin } => match builtin.as_str() {
            "file_input" => (None, Some(DataType::File)),
            "file_output" => (Some(DataType::File), None),
            _ => (Some(DataType::File), Some(DataType::File)),
        },
        ep_core::pipeline::dag::NodeKind::ExternalApi { .. } => (Some(DataType::Text), Some(DataType::Text)),
        ep_core::pipeline::dag::NodeKind::Module { .. } => (Some(DataType::File), Some(DataType::File)),
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ep_core::pipeline::dag::NodeKind;

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

    /// D-3 回归：MiniMap 框始终在画布可视区内（右下角悬浮）；极小画布
    /// 时经钳制不越界。
    #[test]
    fn minimap_frame_rect_stays_inside_canvas() {
        // 正常画布：右下角内缩 margin
        let canvas = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(800.0, 600.0));
        let frame = minimap_frame_rect(canvas);
        assert!(canvas.contains_rect(frame), "正常画布内 MiniMap 应完全在画布内");
        assert_eq!(frame.size(), egui::vec2(MINIMAP_W, MINIMAP_H));

        // 极小画布（比 MiniMap 还小）：钳制后仍不越界
        let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(60.0, 40.0));
        let frame = minimap_frame_rect(tiny);
        assert!(tiny.contains_rect(frame), "极小画布 MiniMap 不得越界");
    }

    fn node(id: &str) -> PipelineNode {
        PipelineNode {
            id: id.into(),
            kind: NodeKind::Builtin {
                builtin: "ffmpeg".into(),
            },
            label: String::new(),
            params: Default::default(),
            position: None,
            timeout_secs: None,
            retry_count: None,
        }
    }

    /// 两态重做：框选命中 —— 相交判定含部分相交，排除完全在外节点
    #[test]
    fn marquee_hits_intersects_only() {
        let pipeline = Pipeline {
            id: "p".into(),
            name: String::new(),
            description: String::new(),
            nodes: vec![node("a"), node("b"), node("c")],
            edges: vec![],
            max_instances: None,
            node_timeout_secs: None,
        };
        let mut positions = HashMap::new();
        positions.insert("a".to_string(), egui::pos2(0.0, 0.0));
        positions.insert("b".to_string(), egui::pos2(300.0, 0.0));
        positions.insert("c".to_string(), egui::pos2(0.0, 300.0));

        // 框选覆盖 a 与 b 的一部分（NODE_W=160）
        let mut hits = marquee_hits(&pipeline, &positions, egui::Rect::from_min_max(
            egui::pos2(150.0, -10.0),
            egui::pos2(320.0, 70.0),
        ));
        hits.sort();
        assert_eq!(hits, vec!["a".to_string(), "b".to_string()]);

        // 空框选（不覆盖任何节点）
        let hits = marquee_hits(&pipeline, &positions, egui::Rect::from_min_max(
            egui::pos2(600.0, 600.0),
            egui::pos2(700.0, 700.0),
        ));
        assert!(hits.is_empty());
    }

    /// 两态重做：MiniMap 映射 —— 等比缩放 + 居中，包围盒四角落在图内
    #[test]
    fn minimap_transform_fits_and_centers() {
        let bbox = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1000.0, 500.0));
        let map = egui::vec2(152.0, 104.0);
        let (scale, off) = minimap_transform(bbox, map);
        // 等比：宽高比保持
        assert!((bbox.width() * scale / (bbox.height() * scale) - 2.0).abs() < 1e-3);
        // 四角在图内
        let tl = egui::pos2(off.x, off.y);
        let br = egui::pos2(off.x + bbox.width() * scale, off.y + bbox.height() * scale);
        assert!(tl.x >= 0.0 && tl.y >= 0.0);
        assert!(br.x <= map.x + 1e-3 && br.y <= map.y + 1e-3);
        // 居中：两侧留白对称
        let pad_x0 = off.x;
        let pad_x1 = map.x - br.x;
        assert!((pad_x0 - pad_x1).abs() < 1e-3);
    }

    /// 两态重做：缩放锚定 —— zoom_at 后 focus 屏幕点映射回同一画布坐标
    #[test]
    fn zoom_at_keeps_focus_point_stable() {
        let mut st = VizState {
            zoom: 1.0,
            offset: egui::vec2(50.0, 30.0),
            ..Default::default()
        };
        let origin = egui::pos2(100.0, 100.0);
        let focus = egui::pos2(400.0, 300.0);
        let cp_before = to_canvas(focus, origin, st.offset, st.zoom);
        zoom_at(&mut st, origin, focus, 2.0);
        let cp_after = to_canvas(focus, origin, st.offset, st.zoom);
        assert!((cp_before.x - cp_after.x).abs() < 1e-3);
        assert!((cp_before.y - cp_after.y).abs() < 1e-3);
        assert_eq!(st.zoom, 2.0);
    }
}
