//! 共享组件 — 克制、实用的视觉语言：卡片、徽章、页头、空态、响应式网格、确认对话框。

use eframe::egui;

use crate::ui::palette::{Palette, StatusMeta};

/// 卡片圆角
pub const CARD_ROUNDING: u8 = 10;
/// 控件圆角（按钮、输入框）
pub const CONTROL_ROUNDING: u8 = 8;

// ─── 卡片 ────────────────────────────────────────────────────────────────────

/// 卡片容器 Frame：圆角 + 1px 边框 + 卡片底色 + 14px 内边距
pub fn card_frame(pal: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(pal.card)
        .stroke(egui::Stroke::new(1.0_f32, pal.border))
        .corner_radius(egui::CornerRadius::same(CARD_ROUNDING))
        .inner_margin(egui::Margin::same(14))
}

/// 卡片容器
pub fn card<R>(
    ui: &mut egui::Ui,
    pal: &Palette,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    card_frame(pal).show(ui, add_contents)
}

// ─── 徽章 ────────────────────────────────────────────────────────────────────

/// 通用徽章：胶囊底 + 色点 + 文字
pub fn badge(ui: &mut egui::Ui, pal: &Palette, color: egui::Color32, label: impl Into<String>) {
    let label = label.into();
    egui::Frame::new()
        .fill(pal.badge_bg(color))
        .stroke(egui::Stroke::new(1.0_f32, pal.badge_stroke(color)))
        .corner_radius(egui::CornerRadius::same(20))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(7.0, 7.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 3.5, color);
                ui.label(egui::RichText::new(label).size(12.0).color(color));
            });
        });
}

/// 服务状态徽章
pub fn status_badge(ui: &mut egui::Ui, pal: &Palette, meta: StatusMeta) {
    badge(ui, pal, meta.color, meta.label);
}

// ─── 页面骨架 ────────────────────────────────────────────────────────────────

/// 页头：左侧标题 + 右侧操作区
pub fn page_header(ui: &mut egui::Ui, title: &str, actions: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.heading(title);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), actions);
    });
}

/// 区块标题（比正文略大、加粗）
pub fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(15.0).strong());
}

/// 居中空态：大图标 + 标题 + 提示
pub fn empty_state(ui: &mut egui::Ui, pal: &Palette, icon: &str, title: &str, hint: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(36.0);
        ui.label(egui::RichText::new(icon).size(30.0).color(pal.text_faint));
        ui.add_space(10.0);
        ui.label(egui::RichText::new(title).color(pal.text_dim));
        if !hint.is_empty() {
            ui.add_space(3.0);
            ui.label(egui::RichText::new(hint).small().color(pal.text_faint));
        }
        ui.add_space(20.0);
    });
}

// ─── 响应式布局 ──────────────────────────────────────────────────────────────

/// 根据可用宽度计算网格列数（min_card_width 为单卡最小宽度）
pub fn responsive_columns(available_width: f32, min_card_width: f32, spacing: f32) -> usize {
    if min_card_width <= 0.0 {
        return 1;
    }
    (((available_width + spacing) / (min_card_width + spacing)).floor() as usize).max(1)
}

/// 等宽列网格：items 按 cols 分行，每项占等宽一格
pub fn card_grid<T>(
    ui: &mut egui::Ui,
    cols: usize,
    items: &[T],
    mut draw: impl FnMut(&mut egui::Ui, &T),
) {
    let cols = cols.max(1);
    let spacing = ui.spacing().item_spacing.x;
    for chunk in items.chunks(cols) {
        ui.horizontal(|ui| {
            let avail = ui.available_width();
            let col_w = ((avail - spacing * (cols.saturating_sub(1)) as f32) / cols as f32)
                .max(60.0);
            for item in chunk {
                ui.scope(|ui| {
                    ui.set_width(col_w);
                    draw(ui, item);
                });
            }
        });
    }
}

// ─── 按钮 ────────────────────────────────────────────────────────────────────

/// 主操作按钮（primary 填充）
pub fn primary_button(pal: &Palette, text: impl Into<egui::WidgetText>) -> egui::Button<'_> {
    egui::Button::new(text)
        .fill(pal.primary)
        .corner_radius(egui::CornerRadius::same(CONTROL_ROUNDING))
        .stroke(egui::Stroke::NONE)
}

/// 危险操作按钮（danger 填充）
pub fn danger_button(pal: &Palette, text: impl Into<egui::WidgetText>) -> egui::Button<'_> {
    egui::Button::new(text)
        .fill(pal.danger)
        .corner_radius(egui::CornerRadius::same(CONTROL_ROUNDING))
        .stroke(egui::Stroke::NONE)
}

/// 次要按钮（透明底 + 边框）
pub fn subtle_button(pal: &Palette, text: impl Into<egui::WidgetText>) -> egui::Button<'_> {
    egui::Button::new(text)
        .fill(egui::Color32::TRANSPARENT)
        .corner_radius(egui::CornerRadius::same(CONTROL_ROUNDING))
        .stroke(egui::Stroke::new(1.0_f32, pal.border))
}

// ─── 确认对话框 ──────────────────────────────────────────────────────────────

/// 模态确认对话框。
///
/// 调用方在对话框"打开期间"每帧调用一次。
/// 返回 `Some(true)` = 用户确认，`Some(false)` = 取消/关闭，`None` = 仍然打开。
pub fn confirm_dialog(
    ctx: &egui::Context,
    pal: &Palette,
    id: &str,
    title: &str,
    message: &str,
    confirm_label: &str,
    danger: bool,
) -> Option<bool> {
    let mut result: Option<bool> = None;
    let mut open = true;

    egui::Window::new(title)
        .id(egui::Id::new(id))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.set_min_width(280.0);
            ui.label(message);
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(subtle_button(pal, "取消")).clicked() {
                        result = Some(false);
                    }
                    let fill = if danger { pal.danger } else { pal.primary };
                    let btn = egui::Button::new(confirm_label)
                        .fill(fill)
                        .corner_radius(egui::CornerRadius::same(CONTROL_ROUNDING))
                        .stroke(egui::Stroke::NONE);
                    if ui.add(btn).clicked() {
                        result = Some(true);
                    }
                });
            });
        });

    if result.is_some() {
        result
    } else if !open {
        Some(false) // 窗口 X 按钮关闭视为取消
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responsive_columns_math() {
        assert_eq!(responsive_columns(1000.0, 240.0, 12.0), 4);
        assert_eq!(responsive_columns(500.0, 240.0, 12.0), 2);
        assert_eq!(responsive_columns(100.0, 240.0, 12.0), 1);
        assert_eq!(responsive_columns(0.0, 240.0, 12.0), 1);
    }
}
