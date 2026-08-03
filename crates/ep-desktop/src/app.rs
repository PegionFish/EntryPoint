use std::collections::HashMap;

use eframe::egui;
use ep_core::config::AppConfig;
use ep_core::deps::DepReport;
use ep_core::model::{DownloadState, ModelView, UpdateCheckResult};
use ep_core::module::{DiscoveredModule, ModelSource};
use ep_core::pipeline::runner::TaskSummary;
use ep_core::types::{ComputeDevice, ServiceStatus};

use crate::pages;
use crate::theme;
use crate::toast::ToastManager;
use crate::ui::Palette;

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
    /// 中性提示（非错误），走 Toast info
    Info(String),
    /// 模型列表刷新
    ModelsRefreshed(Vec<ModelView>),
    /// 模型下载进度：percent 0.0~100.0，bytes 为已落盘字节，state 含终态
    ModelDownloadProgress {
        model_id: String,
        percent: f32,
        bytes: u64,
        state: DownloadState,
    },
    /// 模型下载结束 (model_id, success)。success=true 完成；false 失败/取消
    ModelDownloadFinished(String, bool),
    /// 单个模型的更新检查结果。notify=true（单个检查）时 UI 弹 Toast；
    /// notify=false（批量检查）时仅更新状态，汇总 Toast 由 UpdatesCheckSummary 负责。
    ModelUpdateChecked {
        model_id: String,
        result: UpdateCheckResult,
        notify: bool,
    },
    /// 批量更新检查汇总：total 个 Ready 模型中 available 个可更新
    UpdatesCheckSummary { total: usize, available: usize },
    /// 依赖检测报告
    DepReportRefreshed(DepReport),
    /// 管线任务列表刷新
    TasksRefreshed(Vec<TaskSummary>),
}

// ─── Commands: UI → background ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum AppCmd {
    StartModule(String),
    StopModule(String),
    Shutdown,
    /// 下载模型：source 为下载源覆写（None = 主 source，多源模型可选镜像）
    DownloadModel {
        module_id: String,
        model_id: String,
        source: Option<ModelSource>,
    },
    /// 取消指定模型的下载
    CancelDownload(String),
    /// 检查单个模型是否有可用更新
    CheckUpdate { module_id: String, model_id: String },
    /// 检查所有 Ready 模型的更新（并发，汇总结果）
    CheckAllUpdates,
    /// 删除模型 (target_dir)
    DeleteModel(String),
    /// 导入本地模型：module_id 指定目标模块，model_id 指定模型声明，source 为本地文件/目录路径
    ImportModel {
        module_id: String,
        model_id: String,
        source: std::path::PathBuf,
    },
    /// 刷新模型列表
    RefreshModels,
    /// 刷新依赖检测
    RefreshDeps,
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
                category: mf.module.category.clone(),
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
    Models,
    PipelineEditor,
    Tasks,
    Settings,
}

const NAV_ITEMS: &[(Page, &str, &str)] = &[
    (Page::Dashboard, "📊", "仪表盘"),
    (Page::Modules, "🧩", "模块"),
    (Page::Models, "📦", "模型"),
    (Page::PipelineEditor, "🔗", "管线"),
    (Page::Tasks, "📋", "任务"),
    (Page::Settings, "⚙", "设置"),
];

/// 侧栏导航行高
const NAV_ROW_HEIGHT: f32 = 36.0;
/// 紧凑模式（仅图标）的窗口宽度阈值
const COMPACT_WIDTH_THRESHOLD: f32 = 1000.0;

// ─── App ────────────────────────────────────────────────────────────────────

pub struct App {
    current_page: Page,
    pub state: AppState,
    selected_module: Option<usize>,
    rx: std::sync::mpsc::Receiver<AppMsg>,
    pub cmd_tx: tokio::sync::mpsc::UnboundedSender<AppCmd>,
    last_repaint: std::time::Instant,
    /// Toast 通知管理器
    pub toasts: ToastManager,
    /// 深色主题（由 config.general.theme 决定）
    pub dark_theme: bool,
    /// 已应用的缩放（与 config.ui.scale_factor 比对，变化时即时生效）
    applied_scale: f32,
    /// 已应用的字号（与 config.ui.font_size 比对，变化时即时生效）
    applied_font_size: f32,
    /// 是否已执行过首帧窗口尺寸保护
    window_fitted: bool,
    /// 上一帧的紧凑模式状态（切换时重置侧栏宽度缓存，使 default_width 重新生效）
    last_compact: Option<bool>,
}

/// 单个模型的下载进度 UI 状态（下载进行中才存在于 `AppState::downloads`）
#[derive(Debug, Clone)]
pub struct DownloadUiState {
    /// 进度百分比 0.0~100.0（无大小估算时恒为 0.0）
    pub percent: f32,
    /// 已落盘字节数
    pub bytes: u64,
    /// 当前状态（Downloading / Completed / Failed / Cancelled）
    pub state: DownloadState,
}

pub struct AppState {
    pub devices: Vec<ComputeDevice>,
    pub modules: Vec<ModuleEntry>,
    pub config: AppConfig,
    /// 模型列表（跨模块）
    pub models: Vec<ModelView>,
    /// 模型缓存目录
    pub model_cache_dir: String,
    /// 依赖检测报告
    pub dep_report: Option<DepReport>,
    /// 管线任务列表
    pub tasks: Vec<TaskSummary>,
    /// per-model 下载进度状态（model_id → 进度），仅在下载进行中存在
    pub downloads: HashMap<String, DownloadUiState>,
    /// per-model 更新检查结果（model_id → 结果），检查后常驻直到下次刷新
    pub updates: HashMap<String, UpdateCheckResult>,
    /// 每个模型最近一次下载使用的来源（供"重新下载"复用原 source）
    pub download_sources: HashMap<String, Option<ModelSource>>,
}

impl App {
    pub fn new(
        rx: std::sync::mpsc::Receiver<AppMsg>,
        cmd_tx: tokio::sync::mpsc::UnboundedSender<AppCmd>,
        config: AppConfig,
    ) -> Self {
        let dark_theme = config.general.theme != "light";
        let model_cache_dir = config.models.cache_dir.clone();
        let applied_scale = config.ui.scale_factor;
        let applied_font_size = config.ui.font_size;
        Self {
            current_page: Page::Dashboard,
            state: AppState {
                devices: Vec::new(),
                modules: Vec::new(),
                config,
                models: Vec::new(),
                model_cache_dir,
                dep_report: None,
                tasks: Vec::new(),
                downloads: HashMap::new(),
                updates: HashMap::new(),
                download_sources: HashMap::new(),
            },
            selected_module: None,
            rx,
            cmd_tx,
            last_repaint: std::time::Instant::now(),
            toasts: ToastManager::new(),
            dark_theme,
            applied_scale,
            applied_font_size,
            window_fitted: false,
            last_compact: None,
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
                    self.toasts.success(format!("模块 {id} 已启动"));
                }
                AppMsg::ModuleStopped(id) => {
                    if let Some(m) = self.state.modules.iter_mut().find(|m| m.id == id) {
                        m.status = ServiceStatus::Stopped;
                        m.port = None;
                        m.device = None;
                        m.started_at = None;
                    }
                    self.toasts.info(format!("模块 {id} 已停止"));
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
                    self.toasts.error(&e);
                }
                AppMsg::Info(m) => {
                    self.toasts.info(&m);
                }
                AppMsg::ModelsRefreshed(models) => {
                    self.state.models = models;
                }
                AppMsg::ModelDownloadProgress {
                    model_id,
                    percent,
                    bytes,
                    state,
                } => {
                    self.state.downloads.insert(
                        model_id,
                        DownloadUiState {
                            percent,
                            bytes,
                            state,
                        },
                    );
                }
                AppMsg::ModelDownloadFinished(model_id, success) => {
                    // 清理该模型的下载进度状态
                    self.state.downloads.remove(&model_id);
                    if success {
                        self.toasts.success(format!("模型 {model_id} 下载完成"));
                        // 清除旧的更新检查结果（刚下载完成必然最新）
                        self.state.updates.remove(&model_id);
                    }
                    // 失败/取消的具体原因由生产侧另行发送 Error/Info 消息，这里只刷新列表
                    // （状态可能从 Missing → Ready / Incomplete）
                    let _ = self.cmd_tx.send(AppCmd::RefreshModels);
                }
                AppMsg::ModelUpdateChecked {
                    model_id,
                    result,
                    notify,
                } => {
                    let available = result.available;
                    let reason = result.reason.clone();
                    self.state.updates.insert(model_id.clone(), result);
                    if notify {
                        if available {
                            self.toasts
                                .success(format!("模型 {model_id} 有可用更新，可重新下载"));
                        } else {
                            self.toasts.info(format!("模型 {model_id}：{reason}"));
                        }
                    }
                }
                AppMsg::UpdatesCheckSummary { total, available } => {
                    if total == 0 {
                        self.toasts.info("没有可检查更新的已就绪模型");
                    } else if available == 0 {
                        self.toasts.success(format!("检查完成：{total} 个模型均为最新版本"));
                    } else {
                        self.toasts
                            .info(format!("检查完成：{available}/{total} 个模型有可用更新"));
                    }
                }
                AppMsg::DepReportRefreshed(report) => {
                    self.state.dep_report = Some(report);
                }
                AppMsg::TasksRefreshed(tasks) => {
                    self.state.tasks = tasks;
                }
            }
        }
    }

    /// 首帧窗口保护：窗口宽/高超过屏幕 92% 时收缩到 92%
    fn fit_window_to_screen(&self, ctx: &egui::Context) {
        // 视口信息暂不可用时跳过（兜底）
        let Some(inner) = ctx.input(|i| i.viewport().inner_rect) else {
            return;
        };
        let screen = ctx.screen_rect();
        if screen.width() <= 0.0 || screen.height() <= 0.0 {
            return;
        }
        let max_w = screen.width() * 0.92;
        let max_h = screen.height() * 0.92;
        let mut size = inner.size();
        let mut need_shrink = false;
        if size.x > max_w {
            size.x = max_w;
            need_shrink = true;
        }
        if size.y > max_h {
            size.y = max_h;
            need_shrink = true;
        }
        if need_shrink {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 应用主题
        theme::apply_theme(ctx, self.dark_theme);

        // ── 缩放 / 字号即时生效（设置页修改 config 后立即应用） ──
        if self.state.config.ui.scale_factor != self.applied_scale {
            self.applied_scale = self.state.config.ui.scale_factor;
            ctx.set_zoom_factor(self.applied_scale);
        }
        if self.state.config.ui.font_size != self.applied_font_size {
            self.applied_font_size = self.state.config.ui.font_size;
            theme::apply_font_size(ctx, self.applied_font_size);
        }

        // ── 窗口尺寸保护（一次性） ──
        if !self.window_fitted {
            self.window_fitted = true;
            self.fit_window_to_screen(ctx);
        }

        // Poll messages from background thread
        self.process_messages();

        // Request periodic repaint (~2s) for device/status refresh
        if self.last_repaint.elapsed() > std::time::Duration::from_secs(2) {
            ctx.request_repaint();
            self.last_repaint = std::time::Instant::now();
        }

        let pal = Palette::new(self.dark_theme);

        // ── 响应式紧凑模式（窄窗口只显示图标） ──
        let compact = ctx.input(|i| {
            i.viewport()
                .inner_rect
                .map(|r| r.width() < COMPACT_WIDTH_THRESHOLD)
                .unwrap_or(false)
        });
        // 紧凑状态切换时清除侧栏宽度缓存，让新的 default_width 重新生效
        if self.last_compact != Some(compact) {
            self.last_compact = Some(compact);
            ctx.data_mut(|d| {
                d.remove::<egui::containers::panel::PanelState>(egui::Id::new("nav"));
            });
        }

        // ── Left navigation ──
        egui::SidePanel::left("nav")
            .default_width(if compact { 68.0 } else { 180.0 })
            .resizable(false)
            .show(ctx, |ui| {
                ui.add_space(10.0);
                // 应用标识
                if compact {
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("EP")
                                .size(18.0)
                                .strong()
                                .color(pal.primary),
                        );
                    });
                } else {
                    ui.vertical_centered(|ui| {
                        ui.heading("EntryPoint");
                    });
                }
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(4.0);

                // 导航项
                for &(page, icon, label) in NAV_ITEMS {
                    let active = self.current_page == page;
                    if nav_item(ui, &pal, compact, icon, label, active).clicked() {
                        self.current_page = page;
                    }
                    ui.add_space(2.0);
                }

                // 底部：退出、主题切换、版本号（bottom_up：先添加的在更下方）
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.add_space(8.0);
                    if !compact {
                        ui.label(
                            egui::RichText::new("v0.2.0").small().color(pal.text_faint),
                        );
                    }
                    // 主题切换（持久化到 config/app.toml）
                    let (theme_icon, theme_label) = if self.dark_theme {
                        ("🌙", "深色")
                    } else {
                        ("☀️", "浅色")
                    };
                    if nav_item(ui, &pal, compact, theme_icon, theme_label, false).clicked() {
                        self.dark_theme = !self.dark_theme;
                        self.state.config.general.theme = if self.dark_theme {
                            "dark".to_string()
                        } else {
                            "light".to_string()
                        };
                        let config_dir = ep_core::config::resolve_root().join("config");
                        let _ = self.state.config.save(&config_dir);
                    }
                    ui.add_space(2.0);
                    // 退出
                    if nav_item(ui, &pal, compact, "⏻", "退出", false).clicked() {
                        let _ = self.cmd_tx.send(AppCmd::Shutdown);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });

        // ── Central panel — dispatch to page ──
        egui::CentralPanel::default().show(ctx, |ui| match self.current_page {
            Page::Dashboard => {
                pages::dashboard::show(
                    ui,
                    &self.state.devices,
                    &self.state.modules,
                    self.state.dep_report.as_ref(),
                );
            }
            Page::Modules => {
                pages::modules::show(
                    ui,
                    &mut self.state.modules,
                    &mut self.selected_module,
                    &self.cmd_tx,
                );
            }
            Page::Models => {
                pages::models::show(
                    ui,
                    &self.state.models,
                    &self.state.model_cache_dir,
                    &self.state.downloads,
                    &self.state.updates,
                    &mut self.state.download_sources,
                    &self.cmd_tx,
                );
            }
            Page::PipelineEditor => {
                pages::pipeline_editor::show(ui);
            }
            Page::Tasks => {
                pages::tasks::show(ui, &self.state.modules, &self.state.tasks);
            }
            Page::Settings => {
                pages::settings::show(ui, &mut self.state.config, &mut self.toasts);
            }
        });

        // ── Toast 通知（最上层） ──
        self.toasts.show(ctx);
    }
}

// ─── 侧栏导航行 ─────────────────────────────────────────────────────────────

/// 绘制一行侧栏条目（自绘背景 + 文本）：
/// - 激活态：card_raised 背景 + 左侧 3px primary 指示条 + primary 加粗文字
/// - 悬停态：弱化的 card_raised 背景
/// - compact 模式只显示居中图标，悬停显示文字 tooltip
fn nav_item(
    ui: &mut egui::Ui,
    pal: &Palette,
    compact: bool,
    icon: &str,
    label: &str,
    active: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), NAV_ROW_HEIGHT),
        egui::Sense::click(),
    );

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let rounding = egui::CornerRadius::same(8);
        if active {
            painter.rect_filled(rect, rounding, pal.card_raised);
            // 左侧 3px 圆角指示条
            let bar = egui::Rect::from_min_max(
                egui::pos2(rect.min.x + 1.0, rect.min.y + 9.0),
                egui::pos2(rect.min.x + 4.0, rect.max.y - 9.0),
            );
            painter.rect_filled(bar, egui::CornerRadius::same(2), pal.primary);
        } else if response.hovered() {
            // hover 背景：bg 向 card_raised 插值，两套主题下均弱于激活态
            painter.rect_filled(rect, rounding, pal.bg.lerp_to_gamma(pal.card_raised, 0.6));
        }

        // 文本 / 图标（激活时加粗、primary 色）
        let color = if active { pal.primary } else { pal.text_dim };
        let text = if compact {
            icon.to_string()
        } else {
            format!("{icon}  {label}")
        };
        let mut rich = egui::RichText::new(text).color(color);
        if active {
            rich = rich.strong();
        }
        let galley = egui::WidgetText::from(rich).into_galley(
            ui,
            Some(egui::TextWrapMode::Extend),
            f32::INFINITY,
            egui::FontSelection::Default,
        );
        let pos = if compact {
            egui::pos2(
                rect.center().x - galley.size().x / 2.0,
                rect.center().y - galley.size().y / 2.0,
            )
        } else {
            egui::pos2(
                rect.min.x + 12.0,
                rect.center().y - galley.size().y / 2.0,
            )
        };
        painter.galley(pos, galley, color);
    }

    if compact {
        response.on_hover_text(label)
    } else {
        response
    }
}
