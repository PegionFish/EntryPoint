//! 日志页（GT-5）：LogPipe 引擎 → 显示缓冲 → 过滤 → 虚拟滚动渲染。
//!
//! 职责链（对应派发单 §3 GT-5）：
//! 1. **接线**（app.rs/ui.rs）：creator 里 `LogPipe::spawn`（三源三线程，IO 全
//!    后台）→ [`crate::platform::ui::TrayApp`] 持 `LogPipeHandle`；logic 帧
//!    `drain_new()` 追加进 [`LogBuffer`]（UI 侧显示缓冲，独立于引擎 ring）；
//! 2. **过滤**（[`LogFilter`]，纯逻辑）：来源多选 / 级别多选 / 大小写不敏感
//!    子串，全部在显示层做——红线：**过滤不动 ring**；
//! 3. **渲染**（ui.rs 的日志页分支调 [`LogPage::show`]）：egui
//!    `ScrollArea::show_rows` 虚拟滚动，只渲染可视行。
//!
//! ## 虚拟滚动路线裁决（2026-11-06，本票 commit① 评估记录）
//!
//! 派发单原案 = egui_extras TableView。实测评估结论：**走零依赖路线，不引
//! egui_extras**。理由：
//! - egui 0.37 自带 `ScrollArea::show_rows` 就是虚拟化（只渲染可视行，见本
//!   文件下方 `show_rows_only_renders_visible_rows` 探针测试：50_000 行数据
//!   单帧只渲染可视区几十行）；
//! - 行高固定（时间/来源/级别/消息四列单行截断），恰是 show_rows 的理想
//!   形状（非等高内容才需要 TableView/自定义）；
//! - 本地 registry 无 egui_extras 0.37 缓存，引入 = 新增编译单元 + 新特性面
//!   （TableView 需 feature 门），与收益不成比例。
//!
//! 列布局用 `ui.horizontal` + 定宽列（时间/来源/级别）+ 截断 Label（消息），
//! 不逐帧全量重建文本布局（红线：LayoutJob 高亮仅对可视行构建）。
//!
//! [`LogPage::show`]: LogPage::show

// ─── 虚拟化探针（commit① 路线评估证据 + 回归守卫） ─────────────────────────

/// 探针测试：50_000 行经 `show_rows` 单帧只渲染可视区——证明零依赖路线
/// 可行（egui 内建虚拟化），egui_extras 无必要。
///
/// 用 900×600 视口跑（egui 默认测试视口是 10_000px 高，可视行数随视口
/// 线性增长——虚拟化的正确行为）；600px 高 + 行高 ~19px → 可视区几十行。
#[test]
fn show_rows_only_renders_visible_rows() {
    use eframe::egui;
    use std::cell::Cell;
    use std::rc::Rc;

    let ctx = egui::Context::default();
    ctx.set_fonts(egui::FontDefinitions::empty()); // 测试不加载真字体（省时）
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(900.0, 600.0),
        )),
        ..Default::default()
    };
    let rendered = Rc::new(Cell::new(0usize));
    let rendered_clone = Rc::clone(&rendered);
    let output = ctx.run_ui(input, move |ui| {
        let row_height = ui.text_style_height(&egui::TextStyle::Body);
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show_rows(ui, row_height, 50_000, |_ui, row_range| {
                rendered_clone.set(rendered_clone.get() + row_range.len());
            });
    });
    // 测试不接渲染后端：纹理增量显式弃用（否则 FullOutput drop 时 panic）
    output.drop_without_applying_deltas();
    let n = rendered.get();
    assert!(n < 200, "50_000 行应只渲染可视区，实际渲染 {n} 行");
}
