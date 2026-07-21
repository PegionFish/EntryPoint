use eframe::egui;
use ep_core::config::AppConfig;
use ep_core::types::{
    ComputeBackend, ComputeDevice, DeviceId, ModuleCategory, ServiceStatus, TaskStatus,
};

use crate::pages;
use crate::pages::tasks::TaskEntry;

#[derive(Debug, Clone)]
pub struct ModuleStatus {
    pub name: String,
    pub version: String,
    pub description: String,
    pub category: ModuleCategory,
    pub status: ServiceStatus,
    pub device: Option<String>,
    pub port: Option<u16>,
}

pub struct AppState {
    pub devices: Vec<ComputeDevice>,
    pub modules: Vec<ModuleStatus>,
    pub config: AppConfig,
    pub tasks: Vec<TaskEntry>,
}

impl AppState {
    fn placeholder() -> Self {
        let devices = vec![
            ComputeDevice {
                id: DeviceId::Cuda(0),
                backend: ComputeBackend::Cuda,
                name: "NVIDIA RTX 4090".into(),
                total_memory_mb: Some(24576),
                used_memory_mb: Some(8192),
                utilization: Some(35),
                temperature: Some(62),
            },
            ComputeDevice {
                id: DeviceId::Cpu,
                backend: ComputeBackend::Cpu,
                name: "Intel Core i9-14900K".into(),
                total_memory_mb: Some(65536),
                used_memory_mb: Some(12288),
                utilization: Some(12),
                temperature: Some(45),
            },
        ];

        let modules = vec![
            ModuleStatus {
                name: "Faster-Whisper ASR".into(),
                version: "1.1.0".into(),
                description: "基于 CTranslate2 的高速语音识别，支持词级时间戳".into(),
                category: ModuleCategory::Asr,
                status: ServiceStatus::Running,
                device: Some("cuda:0".into()),
                port: Some(18001),
            },
            ModuleStatus {
                name: "Qwen3-ASR".into(),
                version: "0.9.0".into(),
                description: "Qwen3 语音识别模型，支持多语言".into(),
                category: ModuleCategory::Asr,
                status: ServiceStatus::Stopped,
                device: None,
                port: None,
            },
            ModuleStatus {
                name: "DeepFilter".into(),
                version: "3.0.0".into(),
                description: "实时音频降噪，基于深度滤波".into(),
                category: ModuleCategory::Denoise,
                status: ServiceStatus::Running,
                device: Some("cuda:0".into()),
                port: Some(18002),
            },
            ModuleStatus {
                name: "PaddleOCR".into(),
                version: "2.7.0".into(),
                description: "PP-StructureV3 文档结构化识别".into(),
                category: ModuleCategory::Ocr,
                status: ServiceStatus::NotReady,
                device: None,
                port: None,
            },
        ];

        let tasks = vec![
            TaskEntry {
                id: "t-001".into(),
                pipeline_name: "视频转字幕".into(),
                status: TaskStatus::Completed,
                elapsed: "2m 34s".into(),
            },
            TaskEntry {
                id: "t-002".into(),
                pipeline_name: "音频降噪 + 转写".into(),
                status: TaskStatus::Running,
                elapsed: "0m 47s".into(),
            },
            TaskEntry {
                id: "t-003".into(),
                pipeline_name: "批量 OCR".into(),
                status: TaskStatus::Failed("模型未加载".into()),
                elapsed: "0m 03s".into(),
            },
        ];

        Self {
            devices,
            modules,
            config: AppConfig::default(),
            tasks,
        }
    }
}

pub struct App {
    current_page: Page,
    state: AppState,
    selected_module: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Dashboard,
    Modules,
    PipelineEditor,
    Tasks,
    Settings,
}

const NAV_ITEMS: &[(Page, &str, &str)] = &[
    (Page::Dashboard, "📊", "仪表盘"),
    (Page::Modules, "🧩", "模块"),
    (Page::PipelineEditor, "🔗", "管线"),
    (Page::Tasks, "📋", "任务"),
    (Page::Settings, "⚙", "设置"),
];

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            current_page: Page::Dashboard,
            state: AppState::placeholder(),
            selected_module: None,
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("menubar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("文件", |ui| {
                    if ui.button("退出").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("帮助", |ui| {
                    ui.label("EntryPoint v0.1.0");
                });
            });
        });

        egui::SidePanel::left("nav")
            .default_width(160.0)
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    ui.heading("EntryPoint");
                });
                ui.separator();
                ui.add_space(4.0);

                for &(page, icon, label) in NAV_ITEMS {
                    let text = format!("{icon}  {label}");
                    ui.selectable_value(&mut self.current_page, page, text);
                }

                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.separator();
                    ui.label(
                        egui::RichText::new("v0.1.0")
                            .small()
                            .color(egui::Color32::from_gray(120)),
                    );
                });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            match self.current_page {
                Page::Dashboard => {
                    pages::dashboard::show(ui, &self.state.devices, &self.state.modules);
                }
                Page::Modules => {
                    pages::modules::show(ui, &self.state.modules, &mut self.selected_module);
                }
                Page::PipelineEditor => {
                    pages::pipeline_editor::show(ui);
                }
                Page::Tasks => {
                    pages::tasks::show(ui, &self.state.tasks);
                }
                Page::Settings => {
                    pages::settings::show(ui, &mut self.state.config);
                }
            }
        });
    }
}
