//! Toast 通知 — 右下角弹出消息，3 秒自动消失，颜色随主题（Palette）。

use eframe::egui;
use std::time::Instant;

use crate::ui::Palette;

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

    /// 类型对应的语义色（描边与图标着色）
    fn color(&self, pal: &Palette) -> egui::Color32 {
        match self {
            Self::Success => pal.success,
            Self::Error => pal.danger,
            Self::Info => pal.info,
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

        // 主题感知：跟随当前 egui visuals 的深/浅模式取色
        let pal = Palette::new(ctx.style().visuals.dark_mode);

        let panel = egui::Area::new(egui::Id::new("toast_area"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .order(egui::Order::Foreground)
            .interactable(false);

        panel.show(ctx, |ui| {
            ui.vertical(|ui| {
                // 多条 Toast 垂直排列间距
                ui.spacing_mut().item_spacing.y = 6.0;
                // 从下往上排列（最新的在最下面）
                for toast in &self.toasts {
                    let elapsed = toast.created_at.elapsed().as_secs_f32();
                    let remaining = TOAST_DURATION_SECS as f32 - elapsed;
                    // 最后 0.5 秒淡出
                    let alpha = if remaining < 0.5 {
                        (remaining / 0.5).clamp(0.0, 1.0)
                    } else {
                        1.0
                    };

                    // 预乘色直接 gamma_multiply 即向透明淡出
                    let accent = toast.kind.color(&pal).gamma_multiply(alpha);
                    let fill = pal.card.gamma_multiply(alpha);

                    egui::Frame::new()
                        .fill(fill)
                        .stroke(egui::Stroke::new(1.0_f32, accent))
                        .corner_radius(egui::CornerRadius::same(10))
                        .inner_margin(egui::Margin::symmetric(12, 9))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(toast.kind.icon()).color(accent),
                                );
                                ui.label(
                                    egui::RichText::new(&toast.message)
                                        .color(pal.text.gamma_multiply(alpha)),
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
