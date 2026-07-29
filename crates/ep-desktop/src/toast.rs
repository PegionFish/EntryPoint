//! Toast 通知 — 右下角弹出消息，3 秒自动消失

use eframe::egui;
use std::time::Instant;

/// 单条 Toast 消息
#[derive(Debug, Clone)]
pub struct Toast {
    pub message: String,
    pub kind: ToastKind,
    pub created_at: Instant,
}

/// Toast 类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    Error,
    Info,
}

impl ToastKind {
    fn icon(&self) -> &'static str {
        match self {
            Self::Success => "✅",
            Self::Error => "❌",
            Self::Info => "ℹ️",
        }
    }

    fn color(&self) -> egui::Color32 {
        match self {
            Self::Success => egui::Color32::from_rgb(80, 220, 80),
            Self::Error => egui::Color32::from_rgb(255, 80, 80),
            Self::Info => egui::Color32::from_rgb(80, 160, 255),
        }
    }

    fn bg_color(&self) -> egui::Color32 {
        match self {
            Self::Success => egui::Color32::from_rgb(20, 40, 20),
            Self::Error => egui::Color32::from_rgb(50, 20, 20),
            Self::Info => egui::Color32::from_rgb(20, 30, 50),
        }
    }
}

/// Toast 管理器
#[derive(Default)]
pub struct ToastManager {
    toasts: Vec<Toast>,
}

/// Toast 显示时长
const TOAST_DURATION_SECS: u64 = 3;

impl ToastManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 添加一条 Toast
    pub fn push(&mut self, message: impl Into<String>, kind: ToastKind) {
        self.toasts.push(Toast {
            message: message.into(),
            kind,
            created_at: Instant::now(),
        });
    }

    /// 便捷方法
    pub fn success(&mut self, msg: impl Into<String>) {
        self.push(msg, ToastKind::Success);
    }

    pub fn error(&mut self, msg: impl Into<String>) {
        self.push(msg, ToastKind::Error);
    }

    pub fn info(&mut self, msg: impl Into<String>) {
        self.push(msg, ToastKind::Info);
    }

    /// 清除过期的 Toast
    fn prune(&mut self) {
        self.toasts
            .retain(|t| t.created_at.elapsed().as_secs() < TOAST_DURATION_SECS);
    }

    /// 在 egui 右下角绘制所有活跃的 Toast
    pub fn show(&mut self, ctx: &egui::Context) {
        self.prune();

        if self.toasts.is_empty() {
            return;
        }

        let panel = egui::Area::new(egui::Id::new("toast_area"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .order(egui::Order::Foreground)
            .interactable(false);

        panel.show(ctx, |ui| {
            ui.vertical(|ui| {
                // 从下往上排列（最新的在最下面）
                for toast in &self.toasts {
                    let alpha = {
                        let elapsed = toast.created_at.elapsed().as_secs_f32();
                        let remaining = TOAST_DURATION_SECS as f32 - elapsed;
                        // 最后 0.5 秒淡出
                        if remaining < 0.5 {
                            (remaining / 0.5).clamp(0.0, 1.0)
                        } else {
                            1.0
                        }
                    };

                    let bg = toast.kind.bg_color();
                    let bg = egui::Color32::from_rgba_premultiplied(
                        bg.r(),
                        bg.g(),
                        bg.b(),
                        (alpha * 230.0) as u8,
                    );

                    egui::Frame::new()
                        .fill(bg)
                        .corner_radius(egui::CornerRadius::same(8))
                        .inner_margin(egui::Margin::same(10))
                        .outer_margin(egui::Margin::symmetric(0, 3))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(toast.kind.icon())
                                        .color(toast.kind.color())
                                        .strong(),
                                );
                                ui.label(
                                    egui::RichText::new(&toast.message)
                                        .color(egui::Color32::from_rgb(229, 229, 229)),
                                );
                            });
                        });
                }
            });
        });

        // 有活跃 Toast 时请求重绘（驱动淡出动画）
        ctx.request_repaint();
    }
}