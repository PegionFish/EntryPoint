//! 主应用 — Wave 1 Agent D 实现

use eframe::egui;

pub struct App {
    current_page: Page,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Dashboard,
    Modules,
    PipelineEditor,
    Tasks,
    Settings,
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            current_page: Page::Dashboard,
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::SidePanel::left("nav").show(ctx, |ui| {
            ui.heading("EntryPoint");
            ui.separator();
            ui.selectable_value(&mut self.current_page, Page::Dashboard, "📊 仪表盘");
            ui.selectable_value(&mut self.current_page, Page::Modules, "🧩 模块");
            ui.selectable_value(&mut self.current_page, Page::PipelineEditor, "🔗 管线");
            ui.selectable_value(&mut self.current_page, Page::Tasks, "📋 任务");
            ui.selectable_value(&mut self.current_page, Page::Settings, "⚙ 设置");
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            match self.current_page {
                Page::Dashboard => pages::dashboard(ui),
                Page::Modules => pages::modules(ui),
                Page::PipelineEditor => pages::pipeline_editor(ui),
                Page::Tasks => pages::tasks(ui),
                Page::Settings => pages::settings(ui),
            }
        });
    }
}
