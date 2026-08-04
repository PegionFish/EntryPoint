use std::collections::HashMap;

use eframe::egui;
use ep_core::config::AppConfig;
use ep_core::deps::DepReport;
use ep_core::model::{DownloadState, ModelView, UpdateCheckResult};
use ep_core::module::{DiscoveredModule, ModelSource};
use ep_core::pipeline::runner::TaskSummary;
use ep_core::types::{ComputeDevice, ServiceStatus};

use crate::i18n::tr;
use crate::pages;
use crate::theme;
use crate::toast::ToastManager;
use crate::ui::{
    badge, card, empty_state, page_header, primary_button, section_title, subtle_button, Palette,
};

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
    /// 整合包列表刷新（Wave S S2 骨架注册；C4 实现生产侧：ep-pack 注册表查询）
    PacksRefreshed(Vec<PackEntry>),
    /// 整合包导入进度（§4.4；Wave S S2 骨架注册，C4 生产侧）。
    /// percent 为 None 表示无法估算进度，UI 仅显示阶段文案
    PackImportProgress {
        pack_id: String,
        stage: String,
        percent: Option<f32>,
    },
    /// 整合包导入终态 (pack_id, success)（Wave S S2 骨架注册，C4 生产侧）
    PackImportFinished { pack_id: String, success: bool },
    /// 单模型直跑已提交（§5.3；Wave S S2 骨架注册，C4 生产侧），携带 task_id
    DirectExecSubmitted(String),
    /// 管线级任务列表刷新（§6.8；Wave S S2 骨架注册，C4 生产侧）
    PipelineTasksRefreshed {
        pipeline_id: String,
        tasks: Vec<TaskSummary>,
    },
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
    /// 刷新已安装整合包列表（Wave S S2 骨架注册；C4 实现：ep-pack 注册表查询）
    RefreshPacks,
    /// 从本地路径导入整合包（§4.4；Wave S S2 骨架注册，C4 实现导入编排）。
    /// URL/上传来源走 daemon HTTP API；桌面端仅本地路径（C5 用 rfd 选文件）
    ImportPack { path: std::path::PathBuf },
    /// 单模型直跑（§5.3；Wave S S2 骨架注册，C4 实现：ep-core 直连 submit_direct）。
    /// params 为表单产出的 (参数名, 原始字符串值) 序列，
    /// 由 C4 按模块 manifest CapabilityDecl.params schema 强制类型化
    ExecuteSingle {
        module_id: String,
        capability: String,
        params: Vec<(String, String)>,
        input_path: std::path::PathBuf,
    },
    /// 拉取指定管线的任务列表（§6.8；Wave S S2 骨架注册，C4 实现：ep-core 任务注册表查询）
    RefreshPipelineTasks { pipeline_id: String },
    /// 刷新全局任务列表（P1-6：task_registry 读 runtime/tasks + 内存快照 →
    /// AppMsg::TasksRefreshed）。任务页进入时自动触发，执行中由后台周期推送。
    RefreshTasks,
    /// 执行管线（决策 2 桌面侧入口，§10 C4+C5）：编辑器把已加载的 Pipeline
    /// 传入，background_loop 直连 ep-core PipelineRunnerImpl + task_registry，
    /// 产物归集 workspace/tasks/&lt;task_id&gt;/，支持取消与节点超时。
    ExecutePipeline { pipeline: ep_core::pipeline::Pipeline },
    /// 取消任务（P0-6 协作取消语义）：置位共享标志，runner 在下一节点边界
    /// 终结；注册表立即记 cancelled（逻辑终态）。
    CancelTask { task_id: String },
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
    /// `lang`：清单加载失败的兜底描述按当前界面语言本地化（其余字段为清单数据，不翻译）。
    pub fn from_discovered(dm: &DiscoveredModule, lang: &str) -> Self {
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
                    description: tr(lang, "desktopApp.module.manifestLoadFailed", &[]),
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

// ─── UI-side pack entry（Wave S S2 骨架；生产/消费见 C4/C5）────────────────

/// 已安装整合包的 UI 侧视图（字段对齐 §4.4 注册表 runtime/packs/<pack-id>.json）。
/// C4 填充（AppMsg::PacksRefreshed），C5 整合包页消费。
#[derive(Debug, Clone)]
pub struct PackEntry {
    /// 全局唯一 `<publisher>.<pack-name>`
    pub id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    /// 安装时间（ISO-8601 字符串，仅展示用）
    pub installed_at: Option<String>,
}

/// 单个整合包的导入进度 UI 状态（对照 DownloadUiState；C4/C5 消费）
#[derive(Debug, Clone)]
pub struct PackImportUiState {
    /// 当前阶段描述（解包/checksum/模型落位/管线注册…）
    pub stage: String,
    /// 百分比 0.0~100.0；None = 无法估算进度（仅显示阶段文案）
    pub percent: Option<f32>,
}

// ─── Page enum ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Dashboard,
    Modules,
    Models,
    /// 整合包管理（Wave S S2 骨架注册；页面实现见 Wave 3 C5）
    Packs,
    PipelineEditor,
    Tasks,
    Settings,
}

/// (页面, 图标, 标题文案的 i18n 键, 键缺失时的兜底文案) — 文案渲染时按当前语言查表；
/// 兜底仅在 i18n 键尚未落盘（C8 之前）的过渡期生效
const NAV_ITEMS: &[(Page, &str, &str, &str)] = &[
    (Page::Dashboard, "📊", "desktopApp.nav.dashboard", "仪表盘"),
    (Page::Modules, "🧩", "desktopApp.nav.modules", "模块"),
    (Page::Models, "📦", "desktopApp.nav.models", "模型"),
    (Page::Packs, "🎁", "desktopApp.nav.packs", "整合包"),
    (Page::PipelineEditor, "🔗", "desktopApp.nav.pipeline", "管线"),
    (Page::Tasks, "📋", "desktopApp.nav.tasks", "任务"),
    (Page::Settings, "⚙", "desktopApp.nav.settings", "设置"),
];

/// 侧栏导航行高
const NAV_ROW_HEIGHT: f32 = 36.0;
/// 紧凑模式（仅图标）的窗口宽度阈值
const COMPACT_WIDTH_THRESHOLD: f32 = 1000.0;

// ─── App ────────────────────────────────────────────────────────────────────

pub struct App {
    current_page: Page,
    /// 上一帧所在页面（页面切换时触发一次性数据刷新：Packs/Tasks，C4）
    last_page: Option<Page>,
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
    /// 已安装整合包列表（Wave S S2 骨架槽位；C4 经 AppMsg::PacksRefreshed 填充）
    pub packs: Vec<PackEntry>,
    /// 进行中的整合包导入（pack_id → 进度；Wave S S2 骨架槽位，C4 填充）
    pub pack_imports: HashMap<String, PackImportUiState>,
    /// 管线级任务列表（§6.8；pipeline_id → tasks；Wave S S2 骨架槽位，C4 填充）
    pub pipeline_tasks: HashMap<String, Vec<TaskSummary>>,
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
            last_page: None,
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
                packs: Vec::new(),
                pack_imports: HashMap::new(),
                pipeline_tasks: HashMap::new(),
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

    /// 当前界面语言（归一化）。每帧/每次消息处理从 config 现读，
    /// 设置页切换语言后下一帧即生效。
    pub fn lang(&self) -> &'static str {
        ep_core::i18n::normalize_language(&self.state.config.general.language)
    }

    fn process_messages(&mut self) {
        let lang = self.lang();
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                AppMsg::DevicesRefreshed(devs) => {
                    self.state.devices = devs;
                }
                AppMsg::ModulesDiscovered(dms) => {
                    self.state.modules = dms
                        .iter()
                        .map(|dm| ModuleEntry::from_discovered(dm, lang))
                        .collect();
                }
                AppMsg::ModuleStarted(id, port, device) => {
                    if let Some(m) = self.state.modules.iter_mut().find(|m| m.id == id) {
                        m.status = ServiceStatus::Running;
                        m.port = Some(port);
                        m.device = Some(device);
                        m.started_at = Some(std::time::Instant::now());
                    }
                    self.toasts
                        .success(tr(lang, "desktopApp.toast.moduleStarted", &[("id", &id)]));
                }
                AppMsg::ModuleStopped(id) => {
                    if let Some(m) = self.state.modules.iter_mut().find(|m| m.id == id) {
                        m.status = ServiceStatus::Stopped;
                        m.port = None;
                        m.device = None;
                        m.started_at = None;
                    }
                    self.toasts
                        .info(tr(lang, "desktopApp.toast.moduleStopped", &[("id", &id)]));
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
                        self.toasts.success(tr(
                            lang,
                            "desktopApp.toast.downloadComplete",
                            &[("id", &model_id)],
                        ));
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
                    // reason 为 ep-core 原始消息，按约定以本地化前缀 + 原文附加
                    let reason = result.reason.clone();
                    self.state.updates.insert(model_id.clone(), result);
                    if notify {
                        if available {
                            self.toasts.success(tr(
                                lang,
                                "desktopApp.toast.updateAvailable",
                                &[("id", &model_id)],
                            ));
                        } else {
                            self.toasts.info(tr(
                                lang,
                                "desktopApp.toast.updateChecked",
                                &[("id", &model_id), ("reason", &reason)],
                            ));
                        }
                    }
                }
                AppMsg::UpdatesCheckSummary { total, available } => {
                    if total == 0 {
                        self.toasts.info(tr(lang, "desktopApp.toast.noReadyModels", &[]));
                    } else if available == 0 {
                        self.toasts.success(tr(
                            lang,
                            "desktopApp.toast.allUpToDate",
                            &[("total", &total.to_string())],
                        ));
                    } else {
                        self.toasts.info(tr(
                            lang,
                            "desktopApp.toast.updatesFound",
                            &[
                                ("available", &available.to_string()),
                                ("total", &total.to_string()),
                            ],
                        ));
                    }
                }
                AppMsg::DepReportRefreshed(report) => {
                    self.state.dep_report = Some(report);
                }
                AppMsg::TasksRefreshed(tasks) => {
                    self.state.tasks = tasks;
                }
                AppMsg::PacksRefreshed(packs) => {
                    self.state.packs = packs;
                }
                AppMsg::PackImportProgress {
                    pack_id,
                    stage,
                    percent,
                } => {
                    self.state
                        .pack_imports
                        .insert(pack_id, PackImportUiState { stage, percent });
                }
                AppMsg::PackImportFinished { pack_id, .. } => {
                    // 骨架阶段不弹 Toast（文案 i18n 键待 C8 落盘，见 S2 键需求清单）；
                    // 清理进度状态并请求刷新列表。C4 可在此补成功/失败提示
                    self.state.pack_imports.remove(&pack_id);
                    let _ = self.cmd_tx.send(AppCmd::RefreshPacks);
                }
                AppMsg::DirectExecSubmitted(task_id) => {
                    // C4：直跑已提交 —— Toast 提示并跳转任务页查看进度
                    //（任务快照由后台随 DirectExecSubmitted 一并推送）
                    self.toasts.info(tr(
                        lang,
                        "desktopApp.toast.directExecSubmitted",
                        &[("task", &task_id)],
                    ));
                    self.current_page = Page::Tasks;
                    tracing::debug!(task_id = %task_id, "direct exec submitted, switching to tasks page");
                }
                AppMsg::PipelineTasksRefreshed {
                    pipeline_id,
                    tasks,
                } => {
                    self.state.pipeline_tasks.insert(pipeline_id, tasks);
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

    /// 整合包管理页（C4 实现）：已装包列表 + 导入进度 + 操作按钮（回调 AppCmd）。
    ///
    /// 数据来源：`AppState.packs`（AppMsg::PacksRefreshed）与
    /// `AppState.pack_imports`（AppMsg::PackImportProgress）；进入页面时的
    /// RefreshPacks 由 [`Self::update`] 的页面切换检测触发。
    fn packs_page(&mut self, ui: &mut egui::Ui, lang: &str, pal: &Palette) {
        let cmd_tx = self.cmd_tx.clone();
        page_header(
            ui,
            &tr_or(lang, "desktopApp.packs.title", "整合包管理"),
            |ui| {
                if ui
                    .add(subtle_button(
                        pal,
                        tr_or(lang, "desktopApp.packs.refresh", "刷新"),
                    ))
                    .on_hover_text(tr_or(
                        lang,
                        "desktopApp.packs.refreshTip",
                        "重新读取已装包注册表",
                    ))
                    .clicked()
                {
                    let _ = cmd_tx.send(AppCmd::RefreshPacks);
                }
                if ui
                    .add(primary_button(
                        pal,
                        tr_or(lang, "desktopApp.packs.import", "导入整合包"),
                    ))
                    .clicked()
                {
                    // 桌面端仅本地路径来源（URL/上传走 daemon HTTP API）
                    if let Some(file) = rfd::FileDialog::new()
                        .add_filter("EntryPoint Pack (.epzip)", &["epzip"])
                        .pick_file()
                    {
                        let _ = cmd_tx.send(AppCmd::ImportPack { path: file });
                    }
                }
            },
        );
        ui.add_space(8.0);

        // ── 进行中的导入进度 ──
        if !self.state.pack_imports.is_empty() {
            section_title(
                ui,
                &tr_or(lang, "desktopApp.packs.importing", "导入中"),
            );
            ui.add_space(6.0);
            let mut entries: Vec<(String, PackImportUiState)> = self
                .state
                .pack_imports
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (pack_id, st) in entries {
                card(ui, pal, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&pack_id).strong());
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.label(
                                    egui::RichText::new(pack_stage_label(lang, &st.stage))
                                        .small()
                                        .color(pal.text_dim),
                                );
                            },
                        );
                    });
                    ui.add_space(4.0);
                    let bar = match st.percent {
                        Some(p) => egui::ProgressBar::new((p / 100.0).clamp(0.0, 1.0))
                            .desired_width(ui.available_width()),
                        // 无法估算进度：动画不定量进度条 + 仅显示阶段文案
                        None => egui::ProgressBar::new(0.0)
                            .desired_width(ui.available_width())
                            .animate(true),
                    };
                    ui.add(bar);
                });
                ui.add_space(6.0);
            }
            ui.add_space(6.0);
        }

        // ── 已装包列表 ──
        if self.state.packs.is_empty() {
            empty_state(
                ui,
                pal,
                "🎁",
                &tr_or(lang, "desktopApp.packs.emptyTitle", "尚未安装整合包"),
                &tr_or(
                    lang,
                    "desktopApp.packs.emptyHint",
                    "点击右上角「导入整合包」选择 .epzip 包文件导入",
                ),
            );
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            for pack in &self.state.packs {
                card(ui, pal, |ui| {
                    // 行 1：名称 + 版本徽章 + 安装时间（右对齐）
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&pack.name).strong());
                        badge(
                            ui,
                            pal,
                            pal.neutral,
                            format!("v{}", pack.version),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if let Some(t) = &pack.installed_at {
                                    ui.label(
                                        egui::RichText::new(iso_to_secs(t))
                                            .monospace()
                                            .small()
                                            .color(pal.text_faint),
                                    );
                                }
                            },
                        );
                    });
                    // 行 2：包 id（与显示名不同时展示，mono 弱色）
                    if pack.id != pack.name {
                        ui.label(
                            egui::RichText::new(&pack.id)
                                .monospace()
                                .small()
                                .color(pal.text_faint),
                        );
                    }
                    // 行 3：描述
                    if !pack.description.is_empty() {
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new(&pack.description).color(pal.text_dim),
                        );
                    }
                    // 行 4：tags
                    if !pack.tags.is_empty() {
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            for tag in &pack.tags {
                                badge(ui, pal, pal.info, tag.clone());
                                ui.add_space(4.0);
                            }
                        });
                    }
                });
                ui.add_space(8.0);
            }
        });
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

        // C4：页面进入时一次性数据刷新（P1-6 任务拉取 / 整合包列表）
        if self.last_page != Some(self.current_page) {
            match self.current_page {
                Page::Packs => {
                    let _ = self.cmd_tx.send(AppCmd::RefreshPacks);
                }
                Page::Tasks => {
                    let _ = self.cmd_tx.send(AppCmd::RefreshTasks);
                }
                _ => {}
            }
            self.last_page = Some(self.current_page);
        }

        // Request periodic repaint (~2s) for device/status refresh
        if self.last_repaint.elapsed() > std::time::Duration::from_secs(2) {
            ctx.request_repaint();
            self.last_repaint = std::time::Instant::now();
        }

        let lang = self.lang();
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

                // 导航项（文案按当前语言查表）
                for &(page, icon, label_key, fallback) in NAV_ITEMS {
                    let active = self.current_page == page;
                    let translated = tr(lang, label_key, &[]);
                    // i18n 键缺失时 tr 原样返回键本身（ep-core 约定）：
                    // 回退到兜底文案（键由 C8 落盘后自动失效）
                    let label = if translated == label_key {
                        fallback.to_string()
                    } else {
                        translated
                    };
                    if nav_item(ui, &pal, compact, icon, &label, active).clicked() {
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
                        ("🌙", tr(lang, "common.label.dark", &[]))
                    } else {
                        ("☀️", tr(lang, "common.label.light", &[]))
                    };
                    if nav_item(ui, &pal, compact, theme_icon, &theme_label, false).clicked() {
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
                    let exit_label = tr(lang, "desktopApp.nav.exit", &[]);
                    if nav_item(ui, &pal, compact, "⏻", &exit_label, false).clicked() {
                        let _ = self.cmd_tx.send(AppCmd::Shutdown);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });

        // ── Central panel — dispatch to page ──
        // 各页从 config 归一化取 lang（页面内部自行读取），语言切换即时生效。
        egui::CentralPanel::default().show(ctx, |ui| match self.current_page {
            Page::Dashboard => {
                pages::dashboard::show(
                    ui,
                    &self.state.config,
                    &self.state.devices,
                    &self.state.modules,
                    self.state.dep_report.as_ref(),
                );
            }
            Page::Modules => {
                pages::modules::show(
                    ui,
                    &self.state.config,
                    &mut self.state.modules,
                    &mut self.selected_module,
                    &self.cmd_tx,
                );
            }
            Page::Models => {
                pages::models::show(
                    ui,
                    &self.state.config,
                    &self.state.models,
                    &self.state.model_cache_dir,
                    &self.state.downloads,
                    &self.state.updates,
                    &mut self.state.download_sources,
                    &self.cmd_tx,
                );
            }
            Page::Packs => {
                // C4：整合包管理页（列表 + 导入进度 + 操作按钮回调 AppCmd）
                self.packs_page(ui, lang, &pal);
            }
            Page::PipelineEditor => {
                pages::pipeline_editor::show(ui, &self.state.config);
            }
            Page::Tasks => {
                pages::tasks::show(
                    ui,
                    &self.state.config,
                    &self.state.modules,
                    &self.state.tasks,
                );
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

// ─── i18n 辅助 ───────────────────────────────────────────────────────────────

/// i18n 查找 + 兜底：键缺失时 [`tr`] 原样返回键本身（ep-core 约定），
/// 回退到 fallback 文案。兜底仅在键尚未落盘（C8 之前）的过渡期生效。
fn tr_or(lang: &str, key: &str, fallback: &str) -> String {
    let translated = tr(lang, key, &[]);
    if translated == key {
        fallback.to_string()
    } else {
        translated
    }
}

/// 整合包导入阶段文案（§4.4 ImportStage 小写阶段名 → i18n 键
/// `desktopApp.packs.stage.<stage>`；键未落盘时兜底显示原始阶段名）。
fn pack_stage_label(lang: &str, stage: &str) -> String {
    let key = format!("desktopApp.packs.stage.{stage}");
    let translated = tr(lang, &key, &[]);
    if translated == key {
        stage.to_string()
    } else {
        translated
    }
}

/// ISO 8601 时间字符串截短到秒（前 19 字符）；长度不足或边界不安全时原样返回
fn iso_to_secs(iso: &str) -> &str {
    iso.get(..19).unwrap_or(iso)
}
