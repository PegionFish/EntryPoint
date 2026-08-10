//! 共享组件 — 克制、实用的视觉语言：卡片、徽章、页头、空态、响应式网格、确认对话框。

use eframe::egui;

use crate::i18n::tr;
use crate::ui::palette::Palette;

/// 卡片圆角（对齐 WebUI `--radius-lg` = 10px，UNIFIED_UI_REDESIGN_PROPOSAL §3.3/§10.1）
pub const CARD_ROUNDING: u8 = 10;
/// 控件圆角（按钮、输入框，对齐 WebUI `rounded-md` = 8px）
pub const CONTROL_ROUNDING: u8 = 8;
/// 卡片内边距（§3.3：卡片内边距 24px，对齐 WebUI `p-6`）
pub const CARD_PADDING: u16 = 24;
/// 紧凑模式卡片内边距（§10.4：<1000px 时 24→16 保证信息密度）
pub const COMPACT_CARD_PADDING: u16 = 16;
/// 紧凑断点宽度（与 app.rs `COMPACT_WIDTH_THRESHOLD` 同值，按窗口内容区逻辑宽度）
pub const COMPACT_WIDTH: f32 = 1000.0;

/// 卡片内边距按断点取值（§10.4 紧凑密度规则）
pub fn card_padding(available_width: f32) -> u16 {
    if available_width < COMPACT_WIDTH {
        COMPACT_CARD_PADDING
    } else {
        CARD_PADDING
    }
}

// ─── 动效与辉光基础设施（§1.1 主张 3/6；供 W1-W3 波次复用） ──────────────────

/// 统一动画时长基准 167ms（§1.1 主张 6：--duration-fast 150ms 与 --duration-base
/// 200ms 的中值；egui 无 CSS 动画，一律时间驱动插值）
pub const ANIM_MS: f64 = 167.0;
/// 呼吸辉光周期 2.4s（§1.1 主张 3）
pub const GLOW_BREATH_PERIOD_MS: f64 = 2400.0;
/// 呼吸辉光不透明度区间 0.35–0.7（§1.1 主张 3）
pub const GLOW_ALPHA_MIN: f32 = 0.35;
pub const GLOW_ALPHA_MAX: f32 = 0.70;

/// 呼吸辉光插值助手：按系统时钟返回当前不透明度（0.35–0.7），
/// 2.4s ease-in-out 往返周期（余弦缓动近似）。运行态卡片/状态点附加此辉光。
pub fn glow_breath_alpha(now_ms: f64) -> f32 {
    let period = GLOW_BREATH_PERIOD_MS;
    let t = if period > 0.0 { (now_ms % period) / period } else { 0.0 };
    let phase = ((t * std::f64::consts::TAU).cos() * -0.5 + 0.5) as f32; // ease-in-out 0..1
    GLOW_ALPHA_MIN + phase * (GLOW_ALPHA_MAX - GLOW_ALPHA_MIN)
}

/// 卡片描边（hover 只提升描边亮度，零位移；§1.1 主张 3）。
/// 深色：border-glow(0.18) → border-glow-strong(0.45)；浅色辉光弱化，退回描边明暗对比。
pub fn card_stroke(pal: &Palette, hovered: bool) -> egui::Stroke {
    let color = if hovered {
        pal.border_glow_strong
    } else {
        pal.border_glow
    };
    egui::Stroke::new(1.0_f32, color)
}

// ─── 卡片 ────────────────────────────────────────────────────────────────────

/// 卡片容器 Frame：10px 圆角 + 1px 内发光描边 + 层 1 底色 + 24px 内边距（§3.3）
pub fn card_frame(pal: &Palette) -> egui::Frame {
    card_frame_hover(pal, false)
}

/// 卡片容器 Frame（hover 感知）：悬停时描边由 border-glow 提亮为
/// border-glow-strong，深色下附加主色弱发光阴影（§3.4 hover = 弱发光）。
pub fn card_frame_hover(pal: &Palette, hovered: bool) -> egui::Frame {
    card_frame_padding(pal, hovered, CARD_PADDING)
}

/// 卡片容器 Frame（hover 感知 + 自定义内边距）：内容密度分级（W4-B1）——
/// 稀疏区块 16px、密集区块 24px，其余观感与 [`card_frame_hover`] 一致。
pub fn card_frame_padding(pal: &Palette, hovered: bool, padding: u16) -> egui::Frame {
    let mut f = egui::Frame::new()
        .fill(pal.bg_card)
        .stroke(card_stroke(pal, hovered))
        .corner_radius(egui::CornerRadius::same(CARD_ROUNDING))
        .inner_margin(egui::Margin::same(padding as i8));
    if hovered && pal.dark {
        f = f.shadow(egui::epaint::Shadow {
            offset: [0, 0],
            blur: 12,
            spread: 0,
            color: pal.primary_glow,
        });
    }
    f
}

/// 内容密度分级内边距（W4-B1）：控件数 ≤3 的稀疏区块 16px，≥4 用 24px；
/// 紧凑断点宽度以下一律 16px（与 [`card_padding`] 断点规则叠加）。
pub fn density_padding(available_width: f32, control_count: usize) -> u16 {
    if available_width < COMPACT_WIDTH || control_count <= 3 {
        COMPACT_CARD_PADDING
    } else {
        CARD_PADDING
    }
}

/// 活跃态卡片 Frame（§7.1 设备卡运行态呼吸辉光）：自定义描边色 +
/// 可选彩色辉光阴影；描边色由调用方按 [`glow_breath_alpha`] 时间插值。
pub fn card_frame_active(
    pal: &Palette,
    stroke: egui::Stroke,
    glow_shadow: Option<egui::Color32>,
) -> egui::Frame {
    let mut f = egui::Frame::new()
        .fill(pal.bg_card)
        .stroke(stroke)
        .corner_radius(egui::CornerRadius::same(CARD_ROUNDING))
        .inner_margin(egui::Margin::same(CARD_PADDING as i8));
    if let (Some(color), true) = (glow_shadow, pal.dark) {
        f = f.shadow(egui::epaint::Shadow {
            offset: [0, 0],
            blur: 16,
            spread: 0,
            color,
        });
    }
    f
}

/// 半透明派生色：保留 RGB、替换 alpha（辉光描边/条纹底等层色用）
pub fn color_with_alpha(color: egui::Color32, alpha: u8) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

/// 卡片容器（含悬停描边提亮示范：hover 上一帧状态经临时存储驱动本帧描边，
/// 状态变化时请求重绘；§1.1 主张 3「hover 只提升描边亮度，零位移」）
pub fn card<R>(
    ui: &mut egui::Ui,
    pal: &Palette,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let id = ui.next_auto_id();
    let prev_hovered = ui.ctx().data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    let inner = card_frame_hover(pal, prev_hovered).show(ui, add_contents);
    let hovered = inner.response.hovered();
    if hovered != prev_hovered {
        ui.ctx().data_mut(|d| d.insert_temp(id, hovered));
        ui.ctx().request_repaint();
    }
    inner
}

/// 卡片容器（运行态呼吸辉光 / 静止态 hover 描边提亮；§7.2 模块卡与
/// §7.4 任务卡共用）：`active` 时按 `breath`（调用方经 [`glow_breath_alpha`]
/// 时间插值）用 status_running 呼吸描边 + 辉光阴影，静止态退回 [`card`]
/// 的 hover 行为。
pub fn card_running<R>(
    ui: &mut egui::Ui,
    pal: &Palette,
    active: bool,
    breath: f32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let id = ui.next_auto_id();
    let prev_hovered = ui.ctx().data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    let (stroke, shadow) = if active {
        let stroke_alpha = (breath * 115.0) as u8;
        let shadow_alpha = (breath * 64.0) as u8;
        (
            egui::Stroke::new(1.0_f32, color_with_alpha(pal.status_running, stroke_alpha)),
            Some(color_with_alpha(pal.status_running, shadow_alpha)),
        )
    } else {
        (
            card_stroke(pal, prev_hovered),
            prev_hovered.then_some(pal.primary_glow),
        )
    };
    let inner = card_frame_active(pal, stroke, shadow).show(ui, add_contents);
    if !active {
        let hovered = inner.response.hovered();
        if hovered != prev_hovered {
            ui.ctx().data_mut(|d| d.insert_temp(id, hovered));
            ui.ctx().request_repaint();
        }
    }
    inner
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

/// 状态徽章（StatusBadge，§9）：胶囊 + 状态圆点 + 四态色文字。
///
/// 颜色经 [`crate::ui::service_status`] 统一映射（四态权威色 §1.2）；
/// 运行/过渡态（starting/preparing）圆点附加 status_glow_running 辉光晕
///（§3.4 徽章规范；静态晕，不驱动连续重绘）。
pub fn status_badge(
    ui: &mut egui::Ui,
    pal: &Palette,
    status: &ep_core::types::ServiceStatus,
    label: impl Into<String>,
) {
    use ep_core::types::ServiceStatus;
    let meta = crate::ui::service_status(status, pal);
    let label = label.into();
    let glowing = meta.transitional || matches!(status, ServiceStatus::Running);
    egui::Frame::new()
        .fill(pal.badge_bg(meta.color))
        .stroke(egui::Stroke::new(1.0_f32, pal.badge_stroke(meta.color)))
        .corner_radius(egui::CornerRadius::same(20))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(7.0, 7.0), egui::Sense::hover());
                let center = rect.center();
                if glowing {
                    // 辉光晕：运行中附 status-glow-running（§1.1 主张 5）
                    ui.painter().circle_filled(center, 6.5, pal.status_glow_running);
                }
                ui.painter().circle_filled(center, 3.5, meta.color);
                ui.label(egui::RichText::new(label).size(12.0).color(meta.color));
            });
        });
}

// ─── 页面骨架 ────────────────────────────────────────────────────────────────

/// 页头：左侧标题 + 右侧操作区
pub fn page_header(ui: &mut egui::Ui, title: &str, actions: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.heading(title);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), actions);
    });
}

/// 区块标题（对齐 WebUI H2 `text-base` = 16px，比正文略大、加粗）
pub fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(16.0).strong());
}

/// 区块头三段式（W4-A3，对齐 WebUI CardHeader）：图标字形 + 标题，
/// 标题下一行 text_faint 描述。描述为空时省略。
pub fn section_header(ui: &mut egui::Ui, pal: &Palette, icon: &str, title: &str, description: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(egui::RichText::new(icon).size(15.0).color(pal.primary));
        ui.label(egui::RichText::new(title).size(16.0).strong());
    });
    if !description.is_empty() {
        ui.add_space(2.0);
        ui.label(egui::RichText::new(description).small().color(pal.text_faint));
    }
}

/// 居中空态：大图标 + 标题 + 提示（W4-B6：上下留白 66px → 32px）
pub fn empty_state(ui: &mut egui::Ui, pal: &Palette, icon: &str, title: &str, hint: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(20.0);
        ui.label(egui::RichText::new(icon).size(30.0).color(pal.text_faint));
        ui.add_space(10.0);
        ui.label(egui::RichText::new(title).color(pal.text_dim));
        if !hint.is_empty() {
            ui.add_space(3.0);
            ui.label(egui::RichText::new(hint).small().color(pal.text_faint));
        }
        ui.add_space(12.0);
    });
}

// ─── 表单组件（§9 SwitchRow/FormRow） ─────────────────────────────────────

/// 开关行（SwitchRow，W4-A1 行控件化，对齐 WebUI settings.tsx SwitchRow）：
/// Frame 行容器 = 层 2 底（bg_raised）+ 1px border_glow 描边 + 8px 圆角；
/// hover 提亮描边（零位移）；**整行可点**（`Sense::click`），文案左、开关右。
///
/// 开关本体为纯绘制胶囊（34×18，开=主色/关=中性深灰），无独立子控件，
/// 避免行级点击与控件点击双触发。返回行级 [`egui::Response`] 供测试/扩展。
pub fn switch_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    value: &mut bool,
    label: &str,
    description: &str,
) -> egui::Response {
    let id = ui.next_auto_id();
    let prev_hovered = ui.ctx().data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    let inner = egui::Frame::new()
        .fill(pal.bg_raised)
        .stroke(card_stroke(pal, prev_hovered))
        .corner_radius(egui::CornerRadius::same(CONTROL_ROUNDING))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(label).color(pal.text));
                    if !description.is_empty() {
                        ui.label(
                            egui::RichText::new(description).small().color(pal.text_faint),
                        );
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    paint_switch(ui, pal, *value);
                });
            });
        });
    // 整行点击：在行矩形上二次注册交互（不追加布局空间）
    let response = ui
        .interact(inner.response.rect, id, egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.clicked() {
        *value = !*value;
    }
    let hovered = response.hovered();
    if hovered != prev_hovered {
        ui.ctx().data_mut(|d| d.insert_temp(id, hovered));
        ui.ctx().request_repaint();
    }
    response
}

/// 开关视觉（switch_row 专用）：34×18 胶囊轨道 + 圆形滑钮，纯绘制无交互。
fn paint_switch(ui: &mut egui::Ui, pal: &Palette, on: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(34.0, 18.0), egui::Sense::hover());
    let painter = ui.painter();
    let track = if on { pal.primary } else { pal.notready };
    painter.rect_filled(rect, rect.height() / 2.0, track);
    let knob_x = if on { rect.max.x - 9.0 } else { rect.min.x + 9.0 };
    painter.circle_filled(
        egui::pos2(knob_x, rect.center().y),
        6.0,
        egui::Color32::from_rgb(248, 250, 252),
    );
}

/// 数值控件容器（§3.4 数值件「可编辑外观」）：层 2 底色 + 1px 描边 +
/// 控件圆角，把 DragValue 包成带底带框的输入盒观感。
/// 返回 InnerResponse（`.rect` 供校验失败滚动定位，W4-A4）。
pub fn numeric_field<R>(
    ui: &mut egui::Ui,
    pal: &Palette,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    numeric_field_stroke(ui, pal, egui::Stroke::new(1.0_f32, pal.border), add_contents)
}

/// 数值控件容器（自定义描边）：校验失败时传 danger 红描边（W4-A4）。
pub fn numeric_field_stroke<R>(
    ui: &mut egui::Ui,
    pal: &Palette,
    stroke: egui::Stroke,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    egui::Frame::new()
        .fill(pal.bg_raised)
        .stroke(stroke)
        .corner_radius(egui::CornerRadius::same(CONTROL_ROUNDING))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, add_contents)
}

// ─── 数据可视化（§1.1 主张 4 仪表盘化） ──────────────────────────────────

/// 渐变分段数：每段约 8px，限在 2..=24（过短填充至少 2 段保两端圆角）
pub fn gradient_segment_count(width: f32) -> usize {
    ((width / 8.0).ceil() as usize).clamp(2, 24)
}

/// 渐变进度条（egui 0.31 无渐变画刷 → accent_at 分段插值近似，§3.6）。
///
/// 6px 高、圆角轨道（层 2 底）；填充默认电光青→靛蓝渐变近似；
/// `alert` 提供时改单色填充（高占用告警：warning/danger 语义不变）。
pub fn progress_gradient(
    ui: &mut egui::Ui,
    pal: &Palette,
    fraction: f32,
    alert: Option<egui::Color32>,
) -> egui::Response {
    let height = 6.0_f32;
    let radius = 3.0_f32;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, radius, pal.bg_raised);
    let frac = fraction.clamp(0.0, 1.0);
    if frac > 0.0 {
        let fill_w = rect.width() * frac;
        let fill = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, height));
        match alert {
            Some(color) => {
                painter.rect_filled(fill, radius, color);
            }
            None => {
                // 分段渐变：首/尾段带圆角，中段矩形拼接
                let segs = gradient_segment_count(fill_w);
                let seg_w = fill_w / segs as f32;
                for i in 0..segs {
                    let x0 = fill.min.x + i as f32 * seg_w;
                    let x1 = if i + 1 == segs {
                        fill.max.x
                    } else {
                        fill.min.x + (i + 1) as f32 * seg_w
                    };
                    let seg =
                        egui::Rect::from_min_max(egui::pos2(x0, fill.min.y), egui::pos2(x1, fill.max.y));
                    let t = if segs > 1 { i as f32 / (segs - 1) as f32 } else { 0.0 };
                    let r = radius as u8;
                    let rounding = match (i == 0, i + 1 == segs) {
                        (true, true) => egui::CornerRadius::same(r),
                        (true, false) => egui::CornerRadius { nw: r, ne: 0, sw: r, se: 0 },
                        (false, true) => egui::CornerRadius { nw: 0, ne: r, sw: 0, se: r },
                        (false, false) => egui::CornerRadius::same(0),
                    };
                    painter.rect_filled(seg, rounding, pal.accent_at(t));
                }
            }
        }
    }
    response
}

/// 统计大数字下划线（§3.1 渐变允许位：仪表盘统计大数字）：
/// 2px 高双色分段插值近似青→靛蓝渐变，居中分配宽度。
pub fn accent_underline(ui: &mut egui::Ui, pal: &Palette, width: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width.max(8.0), 2.0), egui::Sense::hover());
    let painter = ui.painter();
    let segs = gradient_segment_count(rect.width());
    let seg_w = rect.width() / segs as f32;
    for i in 0..segs {
        let x0 = rect.min.x + i as f32 * seg_w;
        let x1 = if i + 1 == segs {
            rect.max.x
        } else {
            rect.min.x + (i + 1) as f32 * seg_w
        };
        let seg = egui::Rect::from_min_max(egui::pos2(x0, rect.min.y), egui::pos2(x1, rect.max.y));
        let t = if segs > 1 { i as f32 / (segs - 1) as f32 } else { 0.0 };
        painter.rect_filled(seg, 1.0, pal.accent_at(t));
    }
}

// ─── 分段筛选 Tabs（§9 组件清单 SegmentedTabs） ─────────────────────────────

/// 分段 Tab 文案：标签 + 计数徽章（计数 ≥0 恒显示，与 WebUI 任务筛选口径一致）
pub fn segmented_tab_label(label: &str, count: usize) -> String {
    format!("{label} {count}")
}

/// 分段筛选 Tabs（SegmentedTabs，§9；任务页状态筛选载体 §7.4）：
/// 层 1 底容器 + 1px 描边，选中段 primary/15 底 + primary/25 描边 + 主色文字，
/// 未选中段弱化文字；点击非当前段返回 `Some(新下标)`，无交互返回 `None`。
///
/// 各段 rect 按序写入容器 id 的临时数据（egui 无公开 widget rect 枚举 API，
/// 供测试定位段落注入点击）。
pub fn segmented_tabs(
    ui: &mut egui::Ui,
    pal: &Palette,
    tabs: &[(String, usize)],
    selected: usize,
) -> Option<usize> {
    let container_id = ui.id().with("segmented_tabs");
    let mut clicked: Option<usize> = None;
    let mut tab_rects: Vec<egui::Rect> = Vec::with_capacity(tabs.len());
    egui::Frame::new()
        .fill(pal.bg_card)
        .stroke(egui::Stroke::new(1.0_f32, pal.border))
        .corner_radius(egui::CornerRadius::same(CONTROL_ROUNDING))
        .inner_margin(egui::Margin::symmetric(3, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (i, (label, count)) in tabs.iter().enumerate() {
                    let active = i == selected;
                    let text_color = if active { pal.primary } else { pal.text_dim };
                    let btn = egui::Button::new(
                        egui::RichText::new(segmented_tab_label(label, *count))
                            .size(13.0)
                            .color(text_color),
                    )
                    .fill(if active {
                        color_with_alpha(pal.primary, 38) // primary/15
                    } else {
                        egui::Color32::TRANSPARENT
                    })
                    .stroke(if active {
                        egui::Stroke::new(1.0_f32, color_with_alpha(pal.primary, 64)) // primary/25
                    } else {
                        egui::Stroke::NONE
                    })
                    .corner_radius(egui::CornerRadius::same(6));
                    let resp = ui.add(btn);
                    tab_rects.push(resp.rect);
                    if resp.clicked() && !active {
                        clicked = Some(i);
                    }
                }
            });
        });
    ui.ctx().data_mut(|d| d.insert_temp(container_id, tab_rects));
    clicked
}

// ─── 响应式布局 ──────────────────────────────────────────────────────────────

/// 根据可用宽度计算网格列数（min_card_width 为单卡最小宽度）
pub fn responsive_columns(available_width: f32, min_card_width: f32, spacing: f32) -> usize {
    if min_card_width <= 0.0 {
        return 1;
    }
    (((available_width + spacing) / (min_card_width + spacing)).floor() as usize).max(1)
}

/// 网格列数（封顶于条目数）：条目少于理论列数时不产生右侧空槽，
/// 保证「行铺满可用宽度」的统一宽度策略（P1-1）。
pub fn grid_columns(
    available_width: f32,
    min_card_width: f32,
    spacing: f32,
    item_count: usize,
) -> usize {
    responsive_columns(available_width, min_card_width, spacing).min(item_count.max(1))
}

/// 等宽网格的单列宽度：可用宽度扣除列间距后均分；60px 下限防极端窄窗退化
pub fn grid_col_width(available_width: f32, cols: usize, spacing: f32) -> f32 {
    let cols = cols.max(1);
    ((available_width - spacing * (cols - 1) as f32) / cols as f32).max(60.0)
}

/// 等宽列网格：items 按 cols 分行，每项占等宽一格。
///
/// 每个单元格在独立的**垂直布局**作用域内渲染并固定为列宽：
/// 单元格内容不再继承外层横向布局，文本按列宽换行，
/// 从布局作用域层面消除水平溢出路径（P1-2 加固）。
///
/// **同行等高（W4-B3）**：Frame 由本函数经 `frame_for` 统一产出，
/// 行内最大卡高经临时存储跨帧回填（内层 `set_min_height`），
/// 约两帧收敛后同行卡片底部对齐；hover 状态同样跨帧持久化。
/// `frame_for(ui, item, hovered)` 由调用方按条目状态（运行态呼吸/hover）构造 Frame。
pub fn card_grid<T>(
    ui: &mut egui::Ui,
    id_salt: &str,
    cols: usize,
    items: &[T],
    mut frame_for: impl FnMut(&mut egui::Ui, &T, bool) -> egui::Frame,
    mut draw: impl FnMut(&mut egui::Ui, &T),
) {
    let cols = cols.max(1);
    let spacing = ui.spacing().item_spacing.x;
    // 列宽在外层一次性计算：所有行等宽、行内总宽恰为可用宽度，不产生右缘裁切
    let col_w = grid_col_width(ui.available_width(), cols, spacing);
    let grid_id = ui.id().with(id_salt);
    let rows_key = grid_id.with("row_heights");
    let hover_key = grid_id.with("cell_hovers");
    let prev_rows: Vec<f32> = ui.ctx().data(|d| d.get_temp(rows_key).unwrap_or_default());
    let prev_hovers: Vec<bool> = ui.ctx().data(|d| d.get_temp(hover_key).unwrap_or_default());
    let row_count = items.len().div_ceil(cols);
    let mut cur_rows = vec![0.0_f32; row_count];
    let mut cur_hovers = vec![false; items.len()];
    for (row, chunk) in items.chunks(cols).enumerate() {
        ui.horizontal(|ui| {
            for (ci, item) in chunk.iter().enumerate() {
                let index = row * cols + ci;
                ui.vertical(|ui| {
                    ui.set_width(col_w);
                    let hovered = prev_hovers.get(index).copied().unwrap_or(false);
                    let inner = frame_for(ui, item, hovered).show(ui, |ui| {
                        // 同行等高：上一帧行内最大高度回填（Frame 随之拉伸）
                        if let Some(&h) = prev_rows.get(row) {
                            ui.set_min_height(h);
                        }
                        draw(ui, item);
                    });
                    cur_rows[row] = cur_rows[row].max(inner.response.rect.height());
                    cur_hovers[index] = inner.response.hovered();
                });
            }
        });
    }
    if cur_rows != prev_rows {
        ui.ctx().data_mut(|d| d.insert_temp(rows_key, cur_rows));
        ui.ctx().request_repaint();
    }
    if cur_hovers != prev_hovers {
        ui.ctx().data_mut(|d| d.insert_temp(hover_key, cur_hovers));
        ui.ctx().request_repaint();
    }
}

// ─── 统计大数字条带（仪表盘化 §1.1 主张 4） ─────────────────────────────────────

/// 统计项：标签 + 大数字 + 语义色
pub struct StatItem {
    pub label: String,
    pub value: String,
    pub color: egui::Color32,
}

/// 统计大数字字号（text-4xl = 36px，随配置字号等比缩放；§3.2）
pub fn stat_number_size(ui: &egui::Ui) -> f32 {
    let body = ui.style().text_styles[&egui::TextStyle::Body].size;
    36.0 * (body / crate::theme::BASE_FONT_SIZE)
}

/// 统计大数字条带：每格一张稀疏卡（16px 内边距）——大号等宽数字 +
/// 2px 青→靖蓝渐变下划线（§3.1）+ 全大写灰阶小标签。
/// 仪表盘统计与任务页统计条带（W4-B7）共用。
pub fn stat_cards(ui: &mut egui::Ui, pal: &Palette, id_salt: &str, stats: &[StatItem]) {
    let cols = grid_columns(ui.available_width(), 170.0, 12.0, stats.len());
    card_grid(
        ui,
        id_salt,
        cols,
        stats,
        |_ui, _item, hovered| card_frame_padding(pal, hovered, COMPACT_CARD_PADDING),
        |ui, s| {
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                let resp = ui.label(
                    egui::RichText::new(s.value.as_str())
                        .font(egui::FontId::monospace(stat_number_size(ui)))
                        .strong()
                        .color(s.color),
                );
                ui.add_space(5.0);
                accent_underline(ui, pal, resp.rect.width().max(32.0));
                ui.add_space(7.0);
                ui.label(
                    egui::RichText::new(s.label.to_uppercase())
                        .text_style(egui::TextStyle::Small)
                        .color(pal.text_faint),
                );
                ui.add_space(8.0);
            });
        },
    );
}

// ─── 键盘滚动（P2-1） ───────────────────────────────────────────────────────

/// 带标准键盘滚动的滚动区包装（P2-1）：egui 0.31 的 ScrollArea 默认仅响应滚轮，
/// 此处补齐 PageDown/PageUp 翻页与 ↑/↓ 逐行滚动。
///
/// - 文本输入控件持有键盘焦点时让位（`wants_keyboard_input`，方向键归输入消费）；
/// - 带修饰键（Ctrl/Alt/Shift）的组合键不劫持；
/// - `id_salt` 必填：show 返回后据此定位滚动状态。
///
/// 返回 `ScrollAreaOutput`（内容取值在 `.inner`），调用方可继续消费 `id` 等元数据。
pub fn keyboard_scroll<R>(
    ui: &mut egui::Ui,
    id_salt: &str,
    area: egui::ScrollArea,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::containers::scroll_area::ScrollAreaOutput<R> {
    let out = area.id_salt(id_salt).show(ui, contents);
    apply_keyboard_scroll(ui.ctx(), out.id, out.inner_rect.size(), out.content_size);
    out
}

/// 应用一次键盘滚动：读当帧按键事件，按行/页增量改写目标滚动区 offset。
///
/// 偏移量按内容尺寸钳制；状态写回后于下一帧呈现（输入事件自带重绘，观感即时）。
fn apply_keyboard_scroll(
    ctx: &egui::Context,
    id: egui::Id,
    viewport: egui::Vec2,
    content: egui::Vec2,
) {
    let delta = keyboard_scroll_delta(ctx, viewport.y);
    if delta == 0.0 {
        return;
    }
    if let Some(mut state) = egui::containers::scroll_area::State::load(ctx, id) {
        let max_offset = (content.y - viewport.y).max(0.0);
        state.offset.y = (state.offset.y + delta).clamp(0.0, max_offset);
        state.store(ctx, id);
    }
}

/// 本帧键盘滚动增量（>0 向下）：文本输入焦点或带修饰键时为 0（不劫持）。
fn keyboard_scroll_delta(ctx: &egui::Context, viewport_height: f32) -> f32 {
    if ctx.wants_keyboard_input() {
        return 0.0;
    }
    let row = ctx.style().spacing.interact_size.y + ctx.style().spacing.item_spacing.y;
    let mut delta = 0.0_f32;
    ctx.input(|i| {
        if i.modifiers.any() {
            return;
        }
        if i.key_pressed(egui::Key::PageDown) {
            delta += viewport_height * 0.9;
        }
        if i.key_pressed(egui::Key::PageUp) {
            delta -= viewport_height * 0.9;
        }
        if i.key_pressed(egui::Key::ArrowDown) {
            delta += row;
        }
        if i.key_pressed(egui::Key::ArrowUp) {
            delta -= row;
        }
    });
    delta
}

// ─── 按钮 ────────────────────────────────────────────────────────────────────

/// 主操作按钮（primary 填充）
pub fn primary_button(pal: &Palette, text: impl Into<egui::WidgetText>) -> egui::Button<'_> {
    egui::Button::new(text)
        .fill(pal.primary)
        .corner_radius(egui::CornerRadius::same(CONTROL_ROUNDING))
        .stroke(egui::Stroke::NONE)
}

/// 带辉光的主操作按钮（§3.4 primary：填充 + 中档发光）。
///
/// egui 0.31 的 Button 无 shadow 能力，以透明 Frame 包裹外发辉光；
/// 浅色主题 primary_glow 为透明，自动退化为普通主按钮（§10.5）。
pub fn primary_button_with_glow(
    ui: &mut egui::Ui,
    pal: &Palette,
    text: impl Into<egui::WidgetText>,
) -> egui::Response {
    let mut frame = egui::Frame::new().corner_radius(egui::CornerRadius::same(CONTROL_ROUNDING));
    if pal.primary_glow != egui::Color32::TRANSPARENT {
        frame = frame.shadow(egui::epaint::Shadow {
            offset: [0, 0],
            blur: 12,
            spread: 0,
            color: pal.primary_glow,
        });
    }
    frame.show(ui, |ui| ui.add(primary_button(pal, text))).inner
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
/// 取消按钮文案按 `lang` 走 i18n 键 `common.action.cancel`。
/// 调用方在对话框"打开期间"每帧调用一次。
/// 返回 `Some(true)` = 用户确认，`Some(false)` = 取消/关闭，`None` = 仍然打开。
#[allow(clippy::too_many_arguments)]
pub fn confirm_dialog_with_lang(
    ctx: &egui::Context,
    pal: &Palette,
    id: &str,
    title: &str,
    message: &str,
    confirm_label: &str,
    danger: bool,
    lang: &str,
) -> Option<bool> {
    let mut result: Option<bool> = None;
    let mut open = true;
    let cancel_label = tr(lang, "common.action.cancel", &[]);

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
                    if ui.add(subtle_button(pal, cancel_label)).clicked() {
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

    /// 令牌修正守卫（§10.1/§10.4）：卡片圆角 10px、内边距 24px、紧凑 16px。
    #[test]
    fn card_tokens_match_proposal() {
        assert_eq!(CARD_ROUNDING, 10);
        assert_eq!(CONTROL_ROUNDING, 8);
        assert_eq!(card_padding(1528.0), CARD_PADDING);
        assert_eq!(card_padding(999.0), COMPACT_CARD_PADDING);
        assert_eq!(card_padding(744.0), COMPACT_CARD_PADDING);
    }

    /// 呼吸辉光插值：恒在 0.35–0.7 区间；2.4s 周期端点为最小值、半周期为峰值。
    #[test]
    fn glow_breath_alpha_oscillates_within_range() {
        for i in 0..240 {
            let a = glow_breath_alpha(i as f64 * 10.0);
            assert!(
                (GLOW_ALPHA_MIN..=GLOW_ALPHA_MAX).contains(&a),
                "越界: {a}"
            );
        }
        assert!((glow_breath_alpha(0.0) - GLOW_ALPHA_MIN).abs() < 1e-4);
        assert!((glow_breath_alpha(GLOW_BREATH_PERIOD_MS / 2.0) - GLOW_ALPHA_MAX).abs() < 1e-3);
        assert!((glow_breath_alpha(GLOW_BREATH_PERIOD_MS) - GLOW_ALPHA_MIN).abs() < 1e-4);
    }

    /// 卡片 hover 只提升描边亮度（alpha 档位），零位移：线宽不变（§1.1 主张 3）。
    #[test]
    fn card_hover_only_lifts_stroke_brightness() {
        let pal = Palette::dark();
        let idle = card_stroke(&pal, false);
        let hover = card_stroke(&pal, true);
        assert_eq!(idle.width, hover.width, "零位移：描边宽度不变");
        assert!(hover.color.a() > idle.color.a(), "hover 辉光描边档位应更高");
        // 浅色主题辉光弱化：静态/悬停退回描边明暗对比
        let l = Palette::light();
        assert_eq!(card_stroke(&l, false).color, l.border_glow);
        assert_ne!(card_stroke(&l, true).color, l.border_glow);
    }

    /// 活跃态卡片（§7.1）：自定义描边原样透传；浅色主题不附辉光阴影。
    #[test]
    fn card_frame_active_stroke_and_shadow() {
        let pal = Palette::dark();
        let stroke = egui::Stroke::new(1.0_f32, pal.status_running);
        let f = card_frame_active(&pal, stroke, Some(pal.status_glow_running));
        assert_eq!(f.stroke, stroke);
        assert!(f.shadow.color != egui::Color32::TRANSPARENT, "深色应附辉光阴影");
        let light = card_frame_active(&Palette::light(), stroke, Some(pal.status_glow_running));
        assert_eq!(light.shadow.color, egui::Color32::TRANSPARENT, "浅色关闭辉光");
    }

    /// 半透明派生色：RGB 基本不变、alpha 精确替换（from_rgba_unmultiplied
    /// 内部经线性空间预乘转换，round-trip 允许 ±2 舍入）
    #[test]
    fn color_with_alpha_preserves_rgb() {
        let c = egui::Color32::from_rgb(34, 211, 238);
        let d = color_with_alpha(c, 102);
        let [r, g, b, a] = d.to_srgba_unmultiplied();
        assert_eq!(a, 102);
        assert!(r.abs_diff(34) <= 2, "r={r}");
        assert!(g.abs_diff(211) <= 2, "g={g}");
        assert!(b.abs_diff(238) <= 2, "b={b}");
    }

    /// 渐变分段数：下限 2 段（保两端圆角）、上限 24 段、约 8px/段
    #[test]
    fn gradient_segments_bounded() {
        assert_eq!(gradient_segment_count(0.0), 2);
        assert_eq!(gradient_segment_count(5.0), 2);
        assert_eq!(gradient_segment_count(80.0), 10);
        assert_eq!(gradient_segment_count(10_000.0), 24);
    }

    /// SegmentedTabs 文案：标签与计数空格拼接（计数恒显示，与 WebUI 口径一致）
    #[test]
    fn segmented_tab_label_formats_count() {
        assert_eq!(segmented_tab_label("运行中", 2), "运行中 2");
        assert_eq!(segmented_tab_label("All", 0), "All 0");
    }

    /// SegmentedTabs 交互语义：点击非当前段返回其下标，点击当前段不触发切换。
    /// （首帧记录各段 rect，后续帧在目标段中心注入指针按压/释放事件模拟真实点击）
    #[test]
    fn segmented_tabs_selection_semantics() {
        let ctx = egui::Context::default();
        let tabs: Vec<(String, usize)> = vec![
            ("全部".to_string(), 4),
            ("运行中".to_string(), 1),
            ("失败".to_string(), 0),
        ];
        let rects = std::rc::Rc::new(std::cell::RefCell::new(Vec::<egui::Rect>::new()));
        let slot = std::rc::Rc::new(std::cell::Cell::new(None::<usize>));
        let tab_count = tabs.len();
        let render = {
            let rects = rects.clone();
            let slot = slot.clone();
            move |selected: usize, input: egui::RawInput| {
                rects.borrow_mut().clear();
                let rects = rects.clone();
                let slot = slot.clone();
                let _ = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let pal = Palette::dark();
                        slot.set(segmented_tabs(ui, &pal, &tabs, selected));
                        // 组件把各段 rect 按序写入容器 id 临时数据，此处读回供点击定位
                        let id = ui.id().with("segmented_tabs");
                        if let Some(rs) = ui.ctx().data(|d| d.get_temp::<Vec<egui::Rect>>(id)) {
                            rects.borrow_mut().extend(rs);
                        }
                    });
                });
            }
        };

        // 首帧建立布局，无点击 → 不产生切换
        render(0, egui::RawInput::default());
        assert_eq!(slot.get(), None);

        // 在首段中心注入点击（当前选中段）→ 不触发切换
        let rects_snapshot = rects.borrow().clone();
        assert_eq!(rects_snapshot.len(), tab_count, "三段均须可见");
        let click = |pos: egui::Pos2| {
            let mut input = egui::RawInput::default();
            input.events.push(egui::Event::PointerMoved(pos));
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            });
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
            input
        };
        render(0, click(rects_snapshot[0].center()));
        assert_eq!(slot.get(), None, "点击当前段不应切换");

        // 在第 3 段中心注入点击（非当前段）→ 返回下标 2
        render(0, click(rects_snapshot[2].center()));
        assert_eq!(slot.get(), Some(2), "点击非当前段应返回其下标");
    }

    #[test]
    fn responsive_columns_math() {
        assert_eq!(responsive_columns(1000.0, 240.0, 12.0), 4);
        assert_eq!(responsive_columns(500.0, 240.0, 12.0), 2);
        assert_eq!(responsive_columns(100.0, 240.0, 12.0), 1);
        assert_eq!(responsive_columns(0.0, 240.0, 12.0), 1);
    }

    #[test]
    fn grid_columns_caps_at_item_count() {
        // 宽窗口下条目少于理论列数 → 封顶于条目数（行铺满，无右侧空槽）
        assert_eq!(grid_columns(1528.0, 170.0, 12.0, 4), 4);
        assert_eq!(grid_columns(1528.0, 260.0, 12.0, 3), 3);
        // 紧凑宽度（~1000px 窗口去侧栏/边距）正常降列
        assert_eq!(grid_columns(744.0, 260.0, 12.0, 3), 2);
        assert_eq!(grid_columns(744.0, 360.0, 12.0, 5), 2);
        assert_eq!(grid_columns(300.0, 360.0, 12.0, 5), 1);
        assert_eq!(grid_columns(800.0, 360.0, 12.0, 0), 1);
    }

    #[test]
    fn grid_col_width_fills_available() {
        let avail = 1528.0_f32;
        let cols = 4_usize;
        let spacing = 8.0_f32;
        let w = grid_col_width(avail, cols, spacing);
        let total = w * cols as f32 + spacing * (cols - 1) as f32;
        assert!((total - avail).abs() < 0.01, "行总宽应恰为可用宽度");
        // 极端窄窗下限保护
        assert!(grid_col_width(10.0, 4, 8.0) >= 60.0);
    }

    /// 键盘滚动（P2-1）：PageDown 下翻一页、ArrowUp 上移一行，
    /// 偏移按内容尺寸钳制；文本输入焦点存在时不触发。
    #[test]
    fn keyboard_scrolls_main_area_with_page_and_arrow_keys() {
        let ctx = egui::Context::default();
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        // 滚动区的持久 id（与 keyboard_scroll 内 out.id 同源，供测试读状态）。
        // Rc<Cell> 解两个闭包对同一槽位的读写借用冲突
        let scroll_id = std::rc::Rc::new(std::cell::Cell::new(None::<egui::Id>));
        let render = {
            let scroll_id = scroll_id.clone();
            move |ctx: &egui::Context| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let out =
                        keyboard_scroll(ui, "test_scroll", egui::ScrollArea::vertical(), |ui| {
                            for _ in 0..200 {
                                ui.label("row");
                            }
                        });
                    scroll_id.set(Some(out.id));
                });
            }
        };
        let offset = |ctx: &egui::Context| -> f32 {
            egui::containers::scroll_area::State::load(ctx, scroll_id.get().unwrap())
                .unwrap()
                .offset
                .y
        };

        // 首帧建立滚动状态
        let _ = ctx.run(input.take(), |ctx| render(ctx));

        // PageDown：下翻约一页
        input.events.push(egui::Event::Key {
            key: egui::Key::PageDown,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run(input.take(), |ctx| render(ctx));
        let after_page = offset(&ctx);
        assert!(after_page > 100.0, "PageDown 应显著下翻: {after_page}");

        // ArrowUp：上移一行（offset 减小）
        input.events.push(egui::Event::Key {
            key: egui::Key::ArrowUp,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run(input.take(), |ctx| render(ctx));
        let after_arrow = offset(&ctx);
        assert!(
            after_arrow < after_page,
            "ArrowUp 应上移: {after_arrow} >= {after_page}"
        );

        // Ctrl+PageDown 属组合键：不产生滚动
        //（运行时修饰键状态经 RawInput.modifiers 合入 InputState，此处同步设置）
        input.modifiers = egui::Modifiers::CTRL;
        input.events.push(egui::Event::Key {
            key: egui::Key::PageDown,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        });
        let _ = ctx.run(input.take(), |ctx| render(ctx));
        let after_mod = offset(&ctx);
        assert!(
            (after_mod - after_arrow).abs() < f32::EPSILON,
            "组合键不应滚动"
        );
    }

    /// 键盘滚动钳制：内容不足一屏时 offset 恒为 0（不产生橡皮筋式位移）。
    #[test]
    fn keyboard_scroll_clamps_when_content_fits() {
        let ctx = egui::Context::default();
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        let scroll_id = std::rc::Rc::new(std::cell::Cell::new(None::<egui::Id>));
        let render = {
            let scroll_id = scroll_id.clone();
            move |ctx: &egui::Context| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let out =
                        keyboard_scroll(ui, "short_scroll", egui::ScrollArea::vertical(), |ui| {
                            ui.label("only one line");
                        });
                    scroll_id.set(Some(out.id));
                });
            }
        };
        let _ = ctx.run(input.take(), |ctx| render(ctx));
        input.events.push(egui::Event::Key {
            key: egui::Key::PageDown,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run(input.take(), |ctx| render(ctx));
        let offset = egui::containers::scroll_area::State::load(&ctx, scroll_id.get().unwrap())
            .unwrap()
            .offset
            .y;
        assert_eq!(offset, 0.0, "内容不足一屏不应滚动");
    }

    #[test]
    fn grid_math_no_overflow_across_widths() {
        // 多宽度推演（P1-1 验收口径）：列数 × 列宽 + 列间距 ≤ 可用宽度，
        // 覆盖紧凑 1000px（去侧栏/边距 ≈744）/ 1784px（≈1528）/ 最大化等场景
        for avail in [600.0_f32, 744.0, 1000.0, 1528.0, 1784.0, 2560.0] {
            for min_w in [170.0_f32, 260.0, 360.0] {
                for items in [1_usize, 3, 4, 5] {
                    let cols = grid_columns(avail, min_w, 12.0, items);
                    assert!(cols >= 1 && cols <= items.max(1));
                    let w = grid_col_width(avail, cols, 12.0);
                    let total = w * cols as f32 + 12.0 * (cols - 1) as f32;
                    assert!(
                        total <= avail + 0.01,
                        "溢出: avail={avail} min={min_w} items={items} total={total}"
                    );
                }
            }
        }
    }
}
