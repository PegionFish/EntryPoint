use eframe::egui;
use ep_core::config::AppConfig;
use ep_core::module::DiscoveredModule;
use ep_core::types::{ComputeDevice, ServiceStatus};

use crate::pages;

// ─── Messages: background → UI ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum AppMsg {
    DevicesRefreshed(Vec<ComputeDevice>),
    ModulesDiscovered(Vec<DiscoveredModule>),
    ModuleStarted(String, u16, String),
    ModuleStopped(String),
    ModuleStatusUpdate(String, ServiceStatus),
    LogLine(String, String),
    Error(String),
}

// ─── Commands: UI → background ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum AppCmd {
    StartModule(String),
    StopModule(String),
    Shutdown,
}

// ─── UI-side module entry ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ModuleEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub category: ep_core::types::ModuleCategory,
    pub status: ServiceStatus,
    pub device: Option<String>,
    pub port: Option<u16>,
    pub logs: Vec<String>,
    /// UI-side timestamp of when the module was started (for uptime display)
    pub started_at: Option<std::time::Instant>,
}

impl ModuleEntry {
    pub fn from_discovered(dm: &DiscoveredModule) -> Self {
        match &dm.manifest {
            Some(mf) => Self {
                id: mf.module.id.clone(),
                name: mf.module.name.clone(),
                version: mf.module.version.clone(),
                description: mf.module.description.clone(),
                category: mf.module.category,
                status: ServiceStatus::Stopped,
                device: None,
                port: None,
                logs: Vec::new(),
                started_at: None,
            },
            None => {
                let id = dm
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown".into());
                Self {
                    id: id.clone(),
                    name: id.clone(),
                    version: "?".into(),
                    description: "manifest 加载失败".into(),
                    category: ep_core::types::ModuleCategory::Custom,
                    status: ServiceStatus::NotReady,
                    device: None,
                    port: None,
                    logs: Vec::new(),
                    started_at: None,
                }
            }
        }
    }

    pub fn append_log(&mut self, line: String) {
        if self.logs.len() >= 500 {
            self.logs.remove(0);
        }
        self.logs.push(line);
    }
}

// ─── Page enum ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
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

// ─── App ────────────────────────────────────────────────────────────────────

pub struct App {
    current_page: Page,
    pub state: AppState,
    selected_module: Option<usize>,
    rx: std::sync::mpsc::Receiver<AppMsg>,
    pub cmd_tx: tokio::sync::mpsc::UnboundedSender<AppCmd>,
    status_message: Option<(String, std::time::Instant)>,
    last_repaint: std::time::Instant,
}

pub struct AppState {
    pub devices: Vec<ComputeDevice>,
    pub modules: Vec<ModuleEntry>,
    pub config: AppConfig,
}

impl App {
    pub fn new(
        rx: std::sync::mpsc::Receiver<AppMsg>,
        cmd_tx: tokio::sync::mpsc::UnboundedSender<AppCmd>,
    ) -> Self {
        let config = AppConfig::default();
        Self {
            current_page: Page::Dashboard,
            state: AppState {
                devices: Vec::new(),
                modules: Vec::new(),
                config,
            },
            selected_module: None,
            rx,
            cmd_tx,
            status_message: None,
            last_repaint: std::time::Instant::now(),
        }
    }

    fn process_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                AppMsg::DevicesRefreshed(devs) => {
                    self.state.devices = devs;
                }
                AppMsg::ModulesDiscovered(dms) => {
                    self.state.modules = dms.iter().map(ModuleEntry::from_discovered).collect();
                }
                AppMsg::ModuleStarted(id, port, device) => {
                    if let Some(m) = self.state.modules.iter_mut().find(|m| m.id == id) {
                        m.status = ServiceStatus::Running;
                        m.port = Some(port);
                        m.device = Some(device);
                        m.started_at = Some(std::time::Instant::now());
                    }
                }
                AppMsg::ModuleStopped(id) => {
                    if let Some(m) = self.state.modules.iter_mut().find(|m| m.id == id) {
                        m.status = ServiceStatus::Stopped;
                        m.port = None;
                        m.device = None;
                        m.started_at = None;
                    }
                }
                AppMsg::ModuleStatusUpdate(id, status) => {
                    if let Some(m) = self.state.modules.iter_mut().find(|m| m.id == id) {
                        m.status = status;
                    }
                }
                AppMsg::LogLine(id, line) => {
                    if let Some(m) = self.state.modules.iter_mut().find(|m| m.id == id) {
                        m.append_log(line);
                    }
                }
                AppMsg::Error(e) => {
                    self.status_message = Some((e, std::time::Instant::now()));
                }
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll messages from background thread
        self.process_messages();

        // Request periodic repaint (~2s) for device/status refresh
        if self.last_repaint.elapsed() > std::time::Duration::from_secs(2) {
            ctx.request_repaint();
            self.last_repaint = std::time::Instant::now();
        }

        // Clear status message after 5 seconds
        if let Some((_, instant)) = &self.status_message {
            if instant.elapsed() > std::time::Duration::from_secs(5) {
                self.status_message = None;
            }
        }

        // ── Top menu bar ──
        egui::TopBottomPanel::top("menubar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("文件", |ui| {
                    if ui.button("退出").clicked() {
                        let _ = self.cmd_tx.send(AppCmd::Shutdown);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("帮助", |ui| {
                    ui.label("EntryPoint v0.2.0");
                });
            });

            // Status message bar
            if let Some((ref msg, _)) = self.status_message {
                ui.colored_label(egui::Color32::from_rgb(255, 180, 80), msg);
            }
        });

        // ── Left navigation ──
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
                        egui::RichText::new("v0.2.0")
                            .small()
                            .color(egui::Color32::from_gray(120)),
                    );
                });
            });

        // ── Central panel — dispatch to page ──
        egui::CentralPanel::default().show(ctx, |ui| match self.current_page {
            Page::Dashboard => {
                pages::dashboard::show(ui, &self.state.devices, &self.state.modules);
            }
            Page::Modules => {
                pages::modules::show(
                    ui,
                    &mut self.state.modules,
                    &mut self.selected_module,
                    &self.cmd_tx,
                );
            }
            Page::PipelineEditor => {
                pages::pipeline_editor::show(ui);
            }
            Page::Tasks => {
                pages::tasks::show(ui, &self.state.modules);
            }
            Page::Settings => {
                pages::settings::show(ui, &mut self.state.config, &mut self.status_message);
            }
        });
    }
}
