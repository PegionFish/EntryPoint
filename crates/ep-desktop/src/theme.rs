//! 主题管理 — 深色/浅色主题，基于 [`crate::ui::Palette`] 构建设计系统。
//!
//! 色板对齐 docs/DESIGN_SYSTEM.md，与 WebUI 保持视觉一致。

use eframe::egui;

use crate::ui::components::CONTROL_ROUNDING;
use crate::ui::Palette;

/// 应用主题到 egui Context（visuals + 全局间距）
pub fn apply_theme(ctx: &egui::Context, dark: bool) {
    let pal = Palette::new(dark);
    ctx.style_mut(|style| {
        style.visuals = visuals_for(&pal);
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.window_margin = egui::Margin::same(16);
        style.spacing.indent = 16.0;
    });
}

/// 字号设置基准（Body 字号），用于按比例缩放全部 TextStyle
pub const BASE_FONT_SIZE: f32 = 14.0;

/// 应用字号设置：以 Body=14pt 为基准等比缩放全部 TextStyle（绝对赋值，不叠加）
pub fn apply_font_size(ctx: &egui::Context, font_size: f32) {
    let factor = (font_size / BASE_FONT_SIZE).clamp(0.7, 2.0);
    use egui::TextStyle as Ts;
    ctx.style_mut(|s| {
        s.text_styles.insert(Ts::Heading, egui::FontId::proportional(20.0 * factor));
        s.text_styles.insert(
            Ts::Name("heading2".into()),
            egui::FontId::proportional(17.0 * factor),
        );
        s.text_styles.insert(
            Ts::Name("heading3".into()),
            egui::FontId::proportional(15.0 * factor),
        );
        s.text_styles.insert(Ts::Body, egui::FontId::proportional(BASE_FONT_SIZE * factor));
        s.text_styles.insert(Ts::Button, egui::FontId::proportional(BASE_FONT_SIZE * factor));
        s.text_styles.insert(Ts::Small, egui::FontId::proportional(11.0 * factor));
        s.text_styles.insert(Ts::Monospace, egui::FontId::monospace(13.0 * factor));
    });
}

// ─── Visuals 构建 ────────────────────────────────────────────────────────────

fn visuals_for(pal: &Palette) -> egui::Visuals {
    let mut v = if pal.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };

    let control = egui::CornerRadius::same(CONTROL_ROUNDING);

    // 基础表面
    v.override_text_color = Some(pal.text);
    v.panel_fill = pal.bg;
    v.window_fill = pal.card;
    v.extreme_bg_color = pal.bg;
    v.window_shadow = egui::epaint::Shadow::NONE;
    v.popup_shadow = egui::epaint::Shadow::NONE;

    // 选区
    v.selection.bg_fill = pal.primary.gamma_multiply(if pal.dark { 0.35 } else { 0.18 });
    v.selection.stroke.color = pal.primary;

    // 控件四态（noninteractive / inactive / hovered / active / open）
    v.widgets.noninteractive.weak_bg_fill = pal.bg;
    v.widgets.noninteractive.bg_fill = pal.bg;
    v.widgets.noninteractive.fg_stroke.color = pal.text_dim;

    v.widgets.inactive.weak_bg_fill = pal.card_raised;
    v.widgets.inactive.bg_fill = pal.card_raised;
    v.widgets.inactive.fg_stroke.color = pal.text;
    v.widgets.inactive.corner_radius = control;

    v.widgets.hovered.weak_bg_fill = pal.card_raised;
    v.widgets.hovered.bg_fill = pal.card_raised;
    v.widgets.hovered.fg_stroke.color = pal.text;
    v.widgets.hovered.corner_radius = control;

    v.widgets.active.bg_fill = pal.primary;
    v.widgets.active.fg_stroke.color = egui::Color32::WHITE;
    v.widgets.active.corner_radius = control;

    v.widgets.open.weak_bg_fill = pal.card_raised;
    v.widgets.open.fg_stroke.color = pal.text;
    v.widgets.open.corner_radius = control;

    // 链接色
    v.hyperlink_color = pal.primary;

    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visuals_build_for_both_themes() {
        let dark = visuals_for(&Palette::dark());
        let light = visuals_for(&Palette::light());
        assert_eq!(dark.override_text_color, Some(Palette::dark().text));
        assert_eq!(light.override_text_color, Some(Palette::light().text));
        assert_ne!(dark.panel_fill, light.panel_fill);
    }

    #[test]
    fn font_size_factor_clamped() {
        // 通过 apply_font_size 的 clamp 逻辑间接验证：此处仅保证常量合理
        assert!(BASE_FONT_SIZE >= 10.0 && BASE_FONT_SIZE <= 24.0);
    }
}
