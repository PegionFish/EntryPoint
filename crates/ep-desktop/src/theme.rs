//! 主题管理 — 深色/浅色主题切换
//!
//! 映射 DESIGN_SYSTEM.md 色板到 egui::Visuals。

use eframe::egui;

/// 应用主题到 egui Context
///
/// - `dark = true`：深色主题（默认）
/// - `dark = false`：浅色主题
pub fn apply_theme(ctx: &egui::Context, dark: bool) {
    if dark {
        ctx.set_visuals(dark_visuals());
    } else {
        ctx.set_visuals(light_visuals());
    }
}

/// 深色主题 Visuals
///
/// 色板来自 DESIGN_SYSTEM.md：
/// - background: #0a0a0a
/// - card/panel: #1a1a1a
/// - primary: #3b82f6
/// - text: #e5e5e5
fn dark_visuals() -> egui::Visuals {
    let mut visuals = egui::Visuals::dark();

    let bg = egui::Color32::from_rgb(10, 10, 10);
    let card = egui::Color32::from_rgb(26, 26, 26);
    let primary = egui::Color32::from_rgb(59, 130, 246);
    let text = egui::Color32::from_rgb(229, 229, 229);
    let text_dim = egui::Color32::from_rgb(160, 160, 160);

    visuals.override_text_color = Some(text);
    visuals.widgets.noninteractive.bg_fill = card;
    visuals.widgets.noninteractive.fg_stroke.color = text_dim;
    visuals.widgets.inactive.bg_fill = card;
    visuals.widgets.inactive.fg_stroke.color = text;
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(40, 40, 40);
    visuals.widgets.hovered.fg_stroke.color = text;
    visuals.widgets.active.bg_fill = primary;
    visuals.widgets.active.fg_stroke.color = egui::Color32::WHITE;
    visuals.panel_fill = bg;
    visuals.window_fill = card;
    visuals.selection.bg_fill = primary.linear_multiply(0.3);
    visuals.selection.stroke.color = primary;

    visuals
}

/// 浅色主题 Visuals
///
/// 色板：
/// - background: #ffffff
/// - card/panel: #f5f5f5
/// - primary: #3b82f6
/// - text: #1a1a1a
fn light_visuals() -> egui::Visuals {
    let mut visuals = egui::Visuals::light();

    let bg = egui::Color32::from_rgb(255, 255, 255);
    let card = egui::Color32::from_rgb(245, 245, 245);
    let primary = egui::Color32::from_rgb(59, 130, 246);
    let text = egui::Color32::from_rgb(26, 26, 26);
    let text_dim = egui::Color32::from_rgb(120, 120, 120);

    visuals.override_text_color = Some(text);
    visuals.widgets.noninteractive.bg_fill = card;
    visuals.widgets.noninteractive.fg_stroke.color = text_dim;
    visuals.widgets.inactive.bg_fill = card;
    visuals.widgets.inactive.fg_stroke.color = text;
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(230, 230, 230);
    visuals.widgets.hovered.fg_stroke.color = text;
    visuals.widgets.active.bg_fill = primary;
    visuals.widgets.active.fg_stroke.color = egui::Color32::WHITE;
    visuals.panel_fill = bg;
    visuals.window_fill = card;
    visuals.selection.bg_fill = primary.linear_multiply(0.15);
    visuals.selection.stroke.color = primary;

    visuals
}

// ─── 状态色常量 ──────────────────────────────────────────────────────────────

/// 运行中 — 绿色
pub const STATUS_RUNNING: egui::Color32 = egui::Color32::from_rgb(80, 220, 80);
/// 已停止 — 灰色
pub const STATUS_STOPPED: egui::Color32 = egui::Color32::from_rgb(160, 160, 160);
/// 启动中 — 蓝色
pub const STATUS_STARTING: egui::Color32 = egui::Color32::from_rgb(80, 160, 255);
/// 错误 — 红色
pub const STATUS_ERROR: egui::Color32 = egui::Color32::from_rgb(255, 80, 80);
/// 主色
pub const PRIMARY: egui::Color32 = egui::Color32::from_rgb(59, 130, 246);

/// 根据 ServiceStatus 返回对应颜色
pub fn status_color(status: &ep_core::types::ServiceStatus) -> egui::Color32 {
    use ep_core::types::ServiceStatus;
    match status {
        ServiceStatus::Running => STATUS_RUNNING,
        ServiceStatus::Stopped | ServiceStatus::NotReady => STATUS_STOPPED,
        ServiceStatus::Starting | ServiceStatus::Preparing => STATUS_STARTING,
        ServiceStatus::Error(_) => STATUS_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_color_mapping() {
        use ep_core::types::ServiceStatus;
        assert_eq!(status_color(&ServiceStatus::Running), STATUS_RUNNING);
        assert_eq!(status_color(&ServiceStatus::Stopped), STATUS_STOPPED);
        assert_eq!(status_color(&ServiceStatus::Starting), STATUS_STARTING);
        assert_eq!(
            status_color(&ServiceStatus::Error("test".into())),
            STATUS_ERROR
        );
    }
}