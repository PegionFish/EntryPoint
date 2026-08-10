//! 设计令牌 — 色板映射自 docs/UNIFIED_UI_REDESIGN_PROPOSAL.md §1.2 精确色值表（权威色源），
//! 与 WebUI 保持视觉一致。「深空仪表盘」深色主题为第一主题；浅色主题保持可用，
//! 辉光/渐变在浅色下自动弱化（§10.5）。

use eframe::egui;

/// 主题感知色板。所有页面的语义色均从此处获取，禁止散落硬编码 RGB。
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    // ── 三级层深（§1.1 主张 1 / §1.2：深空底色，不用纯黑） ──
    /// 应用背景 · 层 0（--background，#0B0F17）
    pub bg_base: egui::Color32,
    /// 卡片 / 侧栏 / 面板 · 层 1（--card，#0E1420）
    pub bg_card: egui::Color32,
    /// 浮层 / 抬升表面 · 层 2（--popover / --surface-glass 基色，#121A2B）
    pub bg_raised: egui::Color32,
    /// 常规描边 1px（--border，rgba(148,163,184,0.12)）
    pub border: egui::Color32,
    /// 内发光描边 · 静态（--border-glow，rgba(56,189,248,0.18)）
    pub border_glow: egui::Color32,
    /// 内发光描边 · hover（--border-glow-strong，rgba(56,189,248,0.45)）
    pub border_glow_strong: egui::Color32,
    /// 玻璃拟态浮层底色（--surface-glass；桌面无 backdrop-blur，降级半透明底 + 亮描边，§3.6）
    pub surface_glass: egui::Color32,
    /// 画布点阵（--grid-dot，rgba(148,163,184,0.12)）
    pub grid_dot: egui::Color32,
    // ── 文本 ──
    /// 主文本（--foreground，#E2E8F0）
    pub text: egui::Color32,
    /// 次级文本（--muted-foreground，#94A3B8）
    pub text_dim: egui::Color32,
    /// 更弱文本（占位、时间戳、行号）
    pub text_faint: egui::Color32,
    // ── 强调渐变（仅活跃态，§1.1 主张 2） ──
    /// 电光青：渐变起点 / 主操作色（--primary / --accent-gradient-from，#38BDF8）
    pub primary: egui::Color32,
    /// 主操作悬停（提亮）
    pub primary_hover: egui::Color32,
    /// 靛蓝：渐变终点（--accent-gradient-to，#6366F1）
    pub accent_to: egui::Color32,
    /// 主操作外发光（--primary-glow，主色 35% 透明度）
    pub primary_glow: egui::Color32,
    // ── 四态语义色（§1.1 主张 5，权威值 §1.2） ──
    /// 就绪 · 冷绿（--status-ready，#4ADE80）
    pub status_ready: egui::Color32,
    /// 运行 · 青（--status-running，#22D3EE）
    pub status_running: egui::Color32,
    /// 缺失 / 错误 · 珊瑚红（--status-error，#FB7185）
    pub status_error: egui::Color32,
    /// 停止 · 中性灰（--status-stopped，#94A3B8）
    pub status_stopped: egui::Color32,
    /// 运行态辉光（--status-glow-running；按 §1.2 权威取运行青 40% 透明度）
    pub status_glow_running: egui::Color32,
    // ── 语义扩展（四态同值别名 + 过渡态扩展色，后者不在四态重定值范围） ──
    /// 危险操作红（--destructive，动作色，独立于状态色）
    pub danger: egui::Color32,
    /// 成功 / 完成（= status_ready）
    pub success: egui::Color32,
    /// 警告 / 准备中（--status-preparing）
    pub warning: egui::Color32,
    /// 信息 / 启动中（--status-starting，取强调青）
    pub info: egui::Color32,
    /// 中性（= status_stopped）
    pub neutral: egui::Color32,
    /// 未就绪（--status-notready，深于停止灰以保留区分度）
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

    /// 深色主题 — UNIFIED_UI_REDESIGN_PROPOSAL.md §1.2 精确色值表（权威）
    pub fn dark() -> Self {
        Self {
            dark: true,
            // 三级层深
            bg_base: egui::Color32::from_rgb(11, 15, 23),    // #0B0F17
            bg_card: egui::Color32::from_rgb(14, 20, 32),    // #0E1420
            bg_raised: egui::Color32::from_rgb(18, 26, 43),  // #121A2B
            border: egui::Color32::from_rgba_unmultiplied(148, 163, 184, 31), // 0.12
            border_glow: egui::Color32::from_rgba_unmultiplied(56, 189, 248, 46), // 0.18
            border_glow_strong: egui::Color32::from_rgba_unmultiplied(56, 189, 248, 115), // 0.45
            surface_glass: egui::Color32::from_rgba_unmultiplied(18, 26, 43, 184), // #121A2B @0.72
            grid_dot: egui::Color32::from_rgba_unmultiplied(148, 163, 184, 31), // 0.12
            // 文本
            text: egui::Color32::from_rgb(226, 232, 240),    // #E2E8F0
            text_dim: egui::Color32::from_rgb(148, 163, 184), // #94A3B8
            text_faint: egui::Color32::from_rgb(100, 116, 139), // #64748B
            // 强调渐变
            primary: egui::Color32::from_rgb(56, 189, 248),  // #38BDF8
            primary_hover: egui::Color32::from_rgb(125, 211, 252), // #7DD3FC
            accent_to: egui::Color32::from_rgb(99, 102, 241), // #6366F1
            primary_glow: egui::Color32::from_rgba_unmultiplied(56, 189, 248, 89), // 0.35
            // 四态语义色
            status_ready: egui::Color32::from_rgb(74, 222, 128),  // #4ADE80
            status_running: egui::Color32::from_rgb(34, 211, 238), // #22D3EE
            status_error: egui::Color32::from_rgb(251, 113, 133), // #FB7185
            status_stopped: egui::Color32::from_rgb(148, 163, 184), // #94A3B8
            status_glow_running: egui::Color32::from_rgba_unmultiplied(34, 211, 238, 102), // 0.4
            // 语义扩展
            danger: egui::Color32::from_rgb(239, 68, 68),    // #EF4444
            success: egui::Color32::from_rgb(74, 222, 128),  // = status_ready
            warning: egui::Color32::from_rgb(234, 179, 8),   // #EAB308
            info: egui::Color32::from_rgb(56, 189, 248),     // = primary（强调青）
            neutral: egui::Color32::from_rgb(148, 163, 184), // = status_stopped
            notready: egui::Color32::from_rgb(71, 85, 105),  // #475569
        }
    }

    /// 浅色主题 — 保留可用；辉光/渐变弱化（白底不宜发光，§10.5）
    pub fn light() -> Self {
        Self {
            dark: false,
            bg_base: egui::Color32::from_rgb(249, 249, 251),
            bg_card: egui::Color32::from_rgb(255, 255, 255),
            bg_raised: egui::Color32::from_rgb(244, 244, 245),
            border: egui::Color32::from_rgb(228, 228, 231),
            // 浅色下辉光降级：静态退回常规描边，hover 用更深描边表达亮度提升
            border_glow: egui::Color32::from_rgb(228, 228, 231),
            border_glow_strong: egui::Color32::from_rgb(148, 163, 184),
            surface_glass: egui::Color32::from_rgba_unmultiplied(255, 255, 255, 224),
            // 画布点阵：双端统一口径 slate-500 @0.16
            grid_dot: egui::Color32::from_rgba_unmultiplied(100, 116, 139, 41), // #64748B @0.16
            text: egui::Color32::from_rgb(9, 9, 11),
            text_dim: egui::Color32::from_rgb(113, 113, 122),
            text_faint: egui::Color32::from_rgb(161, 161, 170),
            // 浅色下主色加深以保证白底对比度
            primary: egui::Color32::from_rgb(2, 132, 199),   // #0284C7
            primary_hover: egui::Color32::from_rgb(3, 105, 161), // #0369A1
            accent_to: egui::Color32::from_rgb(79, 70, 229), // #4F46E5
            // 浅色辉光弱化档（双端统一口径）：深色值 alpha ×0.4，不再全透明
            primary_glow: egui::Color32::from_rgba_unmultiplied(56, 189, 248, 36), // 0.35×0.4≈0.14
            status_ready: egui::Color32::from_rgb(22, 163, 74),  // #16A34A
            status_running: egui::Color32::from_rgb(8, 147, 178), // #0891B2
            status_error: egui::Color32::from_rgb(225, 29, 72), // #E11D48
            status_stopped: egui::Color32::from_rgb(113, 113, 122),
            status_glow_running: egui::Color32::from_rgba_unmultiplied(34, 211, 238, 41), // 0.4×0.4≈0.16
            danger: egui::Color32::from_rgb(239, 68, 68),
            success: egui::Color32::from_rgb(22, 163, 74),
            warning: egui::Color32::from_rgb(202, 138, 4),
            // starting / notready：双端统一口径，与深色同值（#38BDF8 / #475569）
            info: egui::Color32::from_rgb(56, 189, 248),
            neutral: egui::Color32::from_rgb(113, 113, 122),
            notready: egui::Color32::from_rgb(71, 85, 105),
        }
    }

    /// 强调渐变插值（电光青 → 靛蓝）。egui 0.31 无渐变画刷，按 §1.1 主张 2
    /// 降级策略：主按钮取单色主色，关键处（导航竖条 / 进度条 / 大数字下划线）
    /// 用本函数双色分段插值近似渐变。
    pub fn accent_at(&self, t: f32) -> egui::Color32 {
        self.primary.lerp_to_gamma(self.accent_to, t.clamp(0.0, 1.0))
    }

    /// 徽章胶囊底色（对齐 --muted 弱化底 + 语义色淡染，§10.1）
    pub fn badge_bg(&self, color: egui::Color32) -> egui::Color32 {
        let base = if self.dark {
            egui::Color32::from_rgb(30, 41, 59) // slate muted #1E293B
        } else {
            egui::Color32::from_rgb(244, 244, 245)
        };
        base.lerp_to_gamma(color, if self.dark { 0.16 } else { 0.11 })
    }

    /// 徽章描边（语义色弱化描边）
    pub fn badge_stroke(&self, color: egui::Color32) -> egui::Color32 {
        color.gamma_multiply(if self.dark { 0.45 } else { 0.35 })
    }
}

/// 服务状态显示信息（颜色 + 是否过渡态）
///
/// 文案不在此处：状态标签走 i18n（见 `pages::modules::service_label`），
/// 静态 `&'static str` 无法承载按语言切换的翻译结果。
#[derive(Debug, Clone, Copy)]
pub struct StatusMeta {
    pub color: egui::Color32,
    /// 过渡态（启动中/准备中），可搭配 spinner / 脉冲提示
    pub transitional: bool,
}

/// ServiceStatus → 显示信息的唯一权威映射（与 WebUI STATUS_COLORS 对齐）。
///
/// 四态语义色按 UNIFIED_UI_REDESIGN_PROPOSAL.md §1.2 重定值：
/// 运行=青 #22D3EE、停止=中性灰 #94A3B8、错误=珊瑚红 #FB7185；
/// starting/preparing 为四态之外的过渡扩展色，保持既有语义。
pub fn service_status(status: &ep_core::types::ServiceStatus, pal: &Palette) -> StatusMeta {
    use ep_core::types::ServiceStatus;
    match status {
        ServiceStatus::Running => StatusMeta {
            color: pal.status_running,
            transitional: false,
        },
        ServiceStatus::Stopped => StatusMeta {
            color: pal.status_stopped,
            transitional: false,
        },
        ServiceStatus::Starting => StatusMeta {
            color: pal.info,
            transitional: true,
        },
        ServiceStatus::Preparing => StatusMeta {
            color: pal.warning,
            transitional: true,
        },
        ServiceStatus::Error(_) => StatusMeta {
            color: pal.status_error,
            transitional: false,
        },
        ServiceStatus::NotReady => StatusMeta {
            color: pal.notready,
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
        assert_eq!(
            service_status(&ServiceStatus::Running, &pal).color,
            pal.status_running
        );
        assert_eq!(
            service_status(&ServiceStatus::Stopped, &pal).color,
            pal.status_stopped
        );
        assert_eq!(service_status(&ServiceStatus::Starting, &pal).color, pal.info);
        assert_eq!(
            service_status(&ServiceStatus::Preparing, &pal).color,
            pal.warning
        );
        assert_eq!(
            service_status(&ServiceStatus::Error("x".into()), &pal).color,
            pal.status_error
        );
        assert!(service_status(&ServiceStatus::Starting, &pal).transitional);
        assert!(!service_status(&ServiceStatus::Running, &pal).transitional);
    }

    /// 令牌基线守卫：深空仪表盘 §1.2 精确色值表（权威色源）防漂移。
    #[test]
    fn dark_tokens_match_proposal_section_1_2() {
        let p = Palette::dark();
        assert_eq!(p.bg_base, egui::Color32::from_rgb(0x0B, 0x0F, 0x17));
        assert_eq!(p.bg_card, egui::Color32::from_rgb(0x0E, 0x14, 0x20));
        assert_eq!(p.bg_raised, egui::Color32::from_rgb(0x12, 0x1A, 0x2B));
        assert_eq!(p.primary, egui::Color32::from_rgb(0x38, 0xBD, 0xF8));
        assert_eq!(p.accent_to, egui::Color32::from_rgb(0x63, 0x66, 0xF1));
        assert_eq!(p.status_ready, egui::Color32::from_rgb(0x4A, 0xDE, 0x80));
        assert_eq!(p.status_running, egui::Color32::from_rgb(0x22, 0xD3, 0xEE));
        assert_eq!(p.status_error, egui::Color32::from_rgb(0xFB, 0x71, 0x85));
        assert_eq!(p.status_stopped, egui::Color32::from_rgb(0x94, 0xA3, 0xB8));
        assert_eq!(p.text, egui::Color32::from_rgb(0xE2, 0xE8, 0xF0));
        assert_eq!(p.text_dim, egui::Color32::from_rgb(0x94, 0xA3, 0xB8));
        // 描边透明度档位：border 0.12 / glow 0.18 / glow-strong 0.45
        assert_eq!(p.border.a(), 31);
        assert_eq!(p.border_glow.a(), 46);
        assert_eq!(p.border_glow_strong.a(), 115);
        // 四态别名一致
        assert_eq!(p.success, p.status_ready);
        assert_eq!(p.neutral, p.status_stopped);
    }

    /// 渐变插值降级：端点恰为 from/to，中值落在两者之间（§1.1 主张 2 降级策略）。
    #[test]
    fn accent_gradient_interpolation() {
        let p = Palette::dark();
        assert_eq!(p.accent_at(0.0), p.primary);
        assert_eq!(p.accent_at(1.0), p.accent_to);
        assert_eq!(p.accent_at(-1.0), p.primary); // 越界钳制
        assert_eq!(p.accent_at(2.0), p.accent_to);
        let mid = p.accent_at(0.5);
        assert_ne!(mid, p.primary);
        assert_ne!(mid, p.accent_to);
    }

    #[test]
    fn light_dark_palettes_differ() {
        let d = Palette::dark();
        let l = Palette::light();
        assert_ne!(d.bg_base, l.bg_base);
        assert_ne!(d.bg_card, l.bg_card);
        // 浅色辉光弱化档（双端统一口径）：深色值 alpha ×0.4，不再全透明
        assert_eq!(
            l.primary_glow,
            egui::Color32::from_rgba_unmultiplied(56, 189, 248, 36)
        );
        assert_eq!(
            l.status_glow_running,
            egui::Color32::from_rgba_unmultiplied(34, 211, 238, 41)
        );
        // grid-dot 浅色统一 slate-500@0.16；starting/notready 与深色同值
        assert_eq!(
            l.grid_dot,
            egui::Color32::from_rgba_unmultiplied(100, 116, 139, 41)
        );
        assert_eq!(l.info, egui::Color32::from_rgb(0x38, 0xBD, 0xF8));
        assert_eq!(l.notready, egui::Color32::from_rgb(0x47, 0x55, 0x69));
    }
}
