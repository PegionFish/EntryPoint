//! 设计令牌 — 色板映射自 docs/DESIGN_SYSTEM.md，与 WebUI 保持视觉一致。

use eframe::egui;

/// 主题感知色板。所有页面的语义色均从此处获取，禁止散落硬编码 RGB。
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    /// 页面整体背景（--background）
    pub bg: egui::Color32,
    /// 卡片 / 侧栏 / 面板（--card）
    pub card: egui::Color32,
    /// 悬停 / 抬升表面（--accent）
    pub card_raised: egui::Color32,
    /// 边框 / 分割线（--border）
    pub border: egui::Color32,
    /// 主文本（--foreground）
    pub text: egui::Color32,
    /// 弱化文本（--muted-foreground）
    pub text_dim: egui::Color32,
    /// 更弱文本（占位、时间戳、行号）
    pub text_faint: egui::Color32,
    /// 主操作蓝（--primary）
    pub primary: egui::Color32,
    /// 主操作悬停
    pub primary_hover: egui::Color32,
    /// 危险操作红（--destructive）
    pub danger: egui::Color32,
    /// 成功 / 运行中（--status-running）
    pub success: egui::Color32,
    /// 警告 / 准备中（--status-preparing）
    pub warning: egui::Color32,
    /// 信息 / 启动中（--status-starting）
    pub info: egui::Color32,
    /// 中性 / 已停止（--status-stopped）
    pub neutral: egui::Color32,
    /// 未就绪（--status-notready）
    pub notready: egui::Color32,
}

impl Palette {
    pub fn new(dark: bool) -> Self {
        if dark {
            Self::dark()
        } else {
            Self::light()
        }
    }

    /// 深色主题 — DESIGN_SYSTEM.md §1.1
    pub fn dark() -> Self {
        Self {
            dark: true,
            bg: egui::Color32::from_rgb(10, 10, 12),
            card: egui::Color32::from_rgb(24, 24, 27),
            card_raised: egui::Color32::from_rgb(39, 39, 42),
            border: egui::Color32::from_rgb(41, 41, 46),
            text: egui::Color32::from_rgb(250, 250, 250),
            text_dim: egui::Color32::from_rgb(161, 161, 170),
            text_faint: egui::Color32::from_rgb(110, 110, 120),
            primary: egui::Color32::from_rgb(59, 130, 246),
            primary_hover: egui::Color32::from_rgb(96, 165, 250),
            danger: egui::Color32::from_rgb(239, 68, 68),
            success: egui::Color32::from_rgb(34, 197, 94),
            warning: egui::Color32::from_rgb(234, 179, 8),
            info: egui::Color32::from_rgb(59, 130, 246),
            neutral: egui::Color32::from_rgb(161, 161, 170),
            notready: egui::Color32::from_rgb(86, 86, 93),
        }
    }

    /// 浅色主题 — DESIGN_SYSTEM.md §1.2
    pub fn light() -> Self {
        Self {
            dark: false,
            bg: egui::Color32::from_rgb(249, 249, 251),
            card: egui::Color32::from_rgb(255, 255, 255),
            card_raised: egui::Color32::from_rgb(244, 244, 245),
            border: egui::Color32::from_rgb(228, 228, 231),
            text: egui::Color32::from_rgb(9, 9, 11),
            text_dim: egui::Color32::from_rgb(113, 113, 122),
            text_faint: egui::Color32::from_rgb(161, 161, 170),
            primary: egui::Color32::from_rgb(59, 130, 246),
            primary_hover: egui::Color32::from_rgb(37, 99, 235),
            danger: egui::Color32::from_rgb(239, 68, 68),
            success: egui::Color32::from_rgb(22, 163, 74),
            warning: egui::Color32::from_rgb(202, 138, 4),
            info: egui::Color32::from_rgb(37, 99, 235),
            neutral: egui::Color32::from_rgb(113, 113, 122),
            notready: egui::Color32::from_rgb(161, 161, 170),
        }
    }

    /// 徽章胶囊底色（语义色弱化填充）
    pub fn badge_bg(&self, color: egui::Color32) -> egui::Color32 {
        color.gamma_multiply(if self.dark { 0.16 } else { 0.11 })
    }

    /// 徽章描边（语义色弱化描边）
    pub fn badge_stroke(&self, color: egui::Color32) -> egui::Color32 {
        color.gamma_multiply(if self.dark { 0.45 } else { 0.35 })
    }
}

/// 服务状态显示信息（颜色 + 文案 + 是否过渡态）
#[derive(Debug, Clone, Copy)]
pub struct StatusMeta {
    pub color: egui::Color32,
    pub label: &'static str,
    /// 过渡态（启动中/准备中），可搭配 spinner / 脉冲提示
    pub transitional: bool,
}

/// ServiceStatus → 显示信息的唯一权威映射（与 WebUI STATUS_COLORS 对齐）
pub fn service_status(status: &ep_core::types::ServiceStatus, pal: &Palette) -> StatusMeta {
    use ep_core::types::ServiceStatus;
    match status {
        ServiceStatus::Running => StatusMeta {
            color: pal.success,
            label: "运行中",
            transitional: false,
        },
        ServiceStatus::Stopped => StatusMeta {
            color: pal.neutral,
            label: "已停止",
            transitional: false,
        },
        ServiceStatus::Starting => StatusMeta {
            color: pal.info,
            label: "启动中",
            transitional: true,
        },
        ServiceStatus::Preparing => StatusMeta {
            color: pal.warning,
            label: "准备中",
            transitional: true,
        },
        ServiceStatus::Error(_) => StatusMeta {
            color: pal.danger,
            label: "错误",
            transitional: false,
        },
        ServiceStatus::NotReady => StatusMeta {
            color: pal.notready,
            label: "未就绪",
            transitional: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ep_core::types::ServiceStatus;

    #[test]
    fn status_meta_colors() {
        let pal = Palette::dark();
        assert_eq!(service_status(&ServiceStatus::Running, &pal).color, pal.success);
        assert_eq!(service_status(&ServiceStatus::Stopped, &pal).color, pal.neutral);
        assert_eq!(service_status(&ServiceStatus::Starting, &pal).color, pal.info);
        assert_eq!(service_status(&ServiceStatus::Preparing, &pal).color, pal.warning);
        assert_eq!(
            service_status(&ServiceStatus::Error("x".into()), &pal).color,
            pal.danger
        );
        assert!(service_status(&ServiceStatus::Starting, &pal).transitional);
        assert!(!service_status(&ServiceStatus::Running, &pal).transitional);
    }

    #[test]
    fn light_dark_palettes_differ() {
        let d = Palette::dark();
        let l = Palette::light();
        assert_ne!(d.bg, l.bg);
        assert_eq!(d.primary, l.primary); // 主色两套主题一致（DESIGN_SYSTEM §1.2）
    }
}
