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
    let mut f = egui::Frame::new()
        .fill(pal.bg_card)
        .stroke(card_stroke(pal, hovered))
        .corner_radius(egui::CornerRadius::same(CARD_ROUNDING))
        .inner_margin(egui::Margin::same(CARD_PADDING as i8));
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
pub fn card_grid<T>(
    ui: &mut egui::Ui,
    cols: usize,
    items: &[T],
    mut draw: impl FnMut(&mut egui::Ui, &T),
) {
    let cols = cols.max(1);
    let spacing = ui.spacing().item_spacing.x;
    // 列宽在外层一次性计算：所有行等宽、行内总宽恰为可用宽度，不产生右缘裁切
    let col_w = grid_col_width(ui.available_width(), cols, spacing);
    for chunk in items.chunks(cols) {
        ui.horizontal(|ui| {
            for item in chunk {
                ui.vertical(|ui| {
                    ui.set_width(col_w);
                    draw(ui, item);
                });
            }
        });
    }
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
