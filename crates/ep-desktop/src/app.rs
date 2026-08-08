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
use crate::ui::Palette;

// ─── Messages: background → UI ──────────────────────────────────────────────

/// P1 修复：下载/更新/取消句柄的复合键 —— 跨模块同名模型变体
///（如两个模块都有 "small" 变体）不再互相覆盖/串扰。
pub fn download_key(module_id: &str, model_id: &str) -> String {
    format!("{module_id}:{model_id}")
}

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
    /// 模型下载进度：percent 0.0~100.0，bytes 为已落盘字节，state 含终态。
    /// module_id 随消息携带——跨模块同名模型变体以下游复合键隔离（P1 修复）
    ModelDownloadProgress {
        module_id: String,
        model_id: String,
        percent: f32,
        bytes: u64,
        state: DownloadState,
    },
    /// 模型下载结束 (module_id, model_id, success)。success=true 完成；false 失败/取消
    ModelDownloadFinished {
        module_id: String,
        model_id: String,
        success: bool,
    },
    /// 单个模型的更新检查结果。notify=true（单个检查）时 UI 弹 Toast；
    /// notify=false（批量检查）时仅更新状态，汇总 Toast 由 UpdatesCheckSummary 负责。
    /// module_id 随消息携带——跨模块同名变体隔离（P1 修复，与下载复合键同构）
    ModelUpdateChecked {
        module_id: String,
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
    /// 取消指定模型的下载（P1 修复：携带 module_id，复合键定位句柄）
    CancelDownload { module_id: String, model_id: String },
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
    /// URL/上传来源走 daemon HTTP API；桌面端仅本地路径（模块页「导入模块」
    /// 经 rfd 选 .epzip 后走本命令，进度/终态消息链不变）
    ImportPack { path: std::path::PathBuf },
    /// 导出模块（协调记录 #47）：按 [`PackExportSpec`] 圈选组装包内容目录
    ///（暂存目录，bundle 权重硬链接优先）→ [`ep_pack::build::build_pack`]
    /// 产出 `.epzip` 到用户选定目录。后台执行，结果经 Info/Error Toast。
    ExportPack { spec: PackExportSpec },
    /// 卸载来源整合包（协调记录 #47：模块卡 pack 来源徽章菜单触发）。
    /// keep_models=false → 删除 meta.pack_id 指向本包的模型目录；
    /// 管线与注册表条目一并移除（语义对齐 daemon DELETE /api/packs/{id}）。
    UninstallPack { pack_id: String, keep_models: bool },
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

/// 导出模块（协调记录 #47）：单个模块的圈选结果——变体集合 + 许可证模式。
#[derive(Debug, Clone)]
pub struct PackExportModule {
    pub module_id: String,
    /// 许可证模式二选一（对话框文案提示按许可证选择）：
    /// true = 「随包附带权重」(bundle)；false = 「仅元数据从指定渠道下载」(reference)
    pub bundle: bool,
    /// 勾选的变体 id 列表（模块 manifest [[models]].id）
    pub variants: Vec<String>,
}

/// 导出模块请求（对话框圈选 → AppCmd::ExportPack → 后台组装+打包）。
/// 形状对齐 daemon `POST /api/packs/build` 的编排输入（§4.5），
/// 桌面端独立实现库层调用（组装暂存目录 → ep_pack::build::build_pack）。
#[derive(Debug, Clone)]
pub struct PackExportSpec {
    /// 圈选模块（变体级勾选 + 每模块 bundle/reference 模式）
    pub modules: Vec<PackExportModule>,
    /// 勾选的管线 id 列表（config/pipelines/*.toml）
    pub pipelines: Vec<String>,
    /// 包身份 `<publisher>.<pack-name>`；空串时后台自动生成 local.build-<时间戳>
    pub id: String,
    /// 包显示名；空串时回退 id
    pub name: String,
    /// semver；空串时回退 0.1.0
    pub version: String,
    /// 用户 rfd 选定的 `.epzip` 保存目录
    pub output_dir: std::path::PathBuf,
}

// ─── Page enum ──────────────────────────────────────────────────────────────

/// 信息架构终稿（协调记录 #47）：「模型就是模块」——NAV 仅五个入口，
/// 旧「模型」页与「整合包」页已并入「模块」（模块管理单页）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Dashboard,
    Modules,
    PipelineEditor,
    Tasks,
    Settings,
}

/// (页面, 图标, 标题文案的 i18n 键, 键缺失时的兜底文案) — 文案渲染时按当前语言查表；
/// 兜底仅在 i18n 键尚未落盘的过渡期生效
const NAV_ITEMS: &[(Page, &str, &str, &str)] = &[
    (Page::Dashboard, "📊", "desktopApp.nav.dashboard", "仪表盘"),
    (Page::Modules, "🧩", "desktopApp.nav.modules", "模块"),
    (Page::PipelineEditor, "🔗", "desktopApp.nav.pipeline", "管线"),
    (Page::Tasks, "📋", "desktopApp.nav.tasks", "任务"),
    (Page::Settings, "⚙", "desktopApp.nav.settings", "设置"),
];

/// 侧栏导航行高
const NAV_ROW_HEIGHT: f32 = 36.0;
/// 紧凑模式（仅图标）的窗口宽度阈值
const COMPACT_WIDTH_THRESHOLD: f32 = 1000.0;

/// 重绘看门狗截止时间：每帧无条件挂起一个 ≤2s 的重绘请求。
///
/// 背景线程的消息通道（std mpsc）无法唤醒 winit 事件循环，重绘请求是
/// 唯一的帧心跳。eframe 0.31 在 Windows 的 resize 同步重绘路径
///（`EventResult::RepaintNow`）存在丢帧缺陷（官方于 0.32 修复，
/// 见 emilk/egui#5723）：同步重绘结果吞掉后续调度，一旦该帧未成功
/// present，事件循环即停摆且永不自愈——这正是最大化冻结/主题切换白屏
/// 的根因。挂起看门狗截止时间后，任何丢失的重绘请求都会在 2s 内被
/// 兜底补发，冻结自愈；健康态节奏与原 2s 实时刷新一致，无额外开销。
const REPAINT_WATCHDOG: std::time::Duration = std::time::Duration::from_secs(2);

// ─── App ────────────────────────────────────────────────────────────────────

/// 窗口恢复位置对比容差（points）：OS 钳回/窗口边框测量与落盘期望
/// 的微小偏差在此范围内视为「位置相符」
const RESTORE_POS_TOLERANCE: f32 = 8.0;

/// 窗口恢复显示器覆盖校验判定结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoreVerdict {
    /// 视口几何未就绪——下帧重试
    Pending,
    /// 实际位置与落盘期望相符——恢复落到了现存显示器内
    Covered,
    /// 位置不符——OS 已钳回孤儿窗口（目标显示器不可达）
    Orphaned,
}

/// 纯函数判定：实际外框左上角与恢复期望位置的容差对比。
/// outer_rect 缺失 → Pending（不判定）。
fn restore_verdict(actual_outer_min: Option<egui::Pos2>, expected: egui::Pos2) -> RestoreVerdict {
    match actual_outer_min {
        None => RestoreVerdict::Pending,
        Some(actual) => {
            if (actual - expected).length() <= RESTORE_POS_TOLERANCE {
                RestoreVerdict::Covered
            } else {
                RestoreVerdict::Orphaned
            }
        }
    }
}

pub struct App {
    current_page: Page,
    /// 上一帧所在页面（页面切换时触发一次性数据刷新：Modules/Tasks）
    last_page: Option<Page>,
    pub state: AppState,
    rx: std::sync::mpsc::Receiver<AppMsg>,
    pub cmd_tx: tokio::sync::mpsc::UnboundedSender<AppCmd>,
    /// Toast 通知管理器
    pub toasts: ToastManager,
    /// 深色主题（由 config.general.theme 决定）
    pub dark_theme: bool,
    /// 已应用的主题（None = 尚未应用；变化时才全量 restyle，避免每帧重复）
    applied_dark: Option<bool>,
    /// 已应用的缩放（与 config.ui.scale_factor 比对，变化时即时生效）
    applied_scale: f32,
    /// 已应用的字号（与 config.ui.font_size 比对，变化时即时生效）
    applied_font_size: f32,
    /// 上一帧的视口尺寸（points）。变化时追加即时重绘，确保最大化/
    /// 还原/拖拽 resize 后布局立刻跟随新尺寸（兜底 eframe 0.31 的
    /// resize 同步重绘丢帧缺陷，见 REPAINT_WATCHDOG 注释）
    last_screen_size: Option<egui::Vec2>,
    /// 最近一次 **normal（非最大化）** 状态的窗口外框/内容矩形
    ///（全局多显示器坐标空间，points）。每帧记录，退出时落盘
    /// runtime/window-state.json，下次启动恢复（P3-1）。
    /// Task #28：最大化期间冻结基准（见 track_window_rect），
    /// 最大化几何绝不带入下次启动，恢复恒为 normal 态
    last_window_outer: Option<egui::Rect>,
    last_window_inner: Option<egui::Rect>,
    /// 窗口当前是否最大化（Task #28：持久化基准门控用）
    last_maximized: bool,
    /// 是否已执行过首帧窗口尺寸保护
    window_fitted: bool,
    /// 启动恢复的期望几何（window-state.json 读出的 outer 左上角 + inner 尺寸）；
    /// 用于首帧显示器覆盖校验（§13 风险 5）
    restored_expectation: Option<(egui::Pos2, egui::Vec2)>,
    /// 窗口恢复校验是否已完成（一次性；视口信息未就绪时逐帧重试）
    restore_checked: bool,
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
        // P2 修复：缩放在构造时即钳制（与 sync_appearance 同口径，避免
        // config 损坏值导致 applied_scale 与目标值每帧不相等而重复应用）
        let applied_scale = crate::theme::clamp_scale_factor(config.ui.scale_factor);
        let applied_font_size = config.ui.font_size;
        Self {
            current_page: Page::Dashboard,
            last_page: None,
            state: AppState {
                devices: Vec::new(),
                modules: Vec::new(),
                config,
                models: Vec::new(),
                dep_report: None,
                tasks: Vec::new(),
                downloads: HashMap::new(),
                updates: HashMap::new(),
                download_sources: HashMap::new(),
                packs: Vec::new(),
                pack_imports: HashMap::new(),
                pipeline_tasks: HashMap::new(),
            },
            rx,
            cmd_tx,
            toasts: ToastManager::new(),
            dark_theme,
            applied_dark: None,
            applied_scale,
            applied_font_size,
            last_screen_size: None,
            last_window_outer: None,
            last_window_inner: None,
            last_maximized: false,
            window_fitted: false,
            restored_expectation: None,
            restore_checked: false,
            last_compact: None,
        }
    }

    /// 携带启动恢复的期望几何（main.rs 从 window-state.json 读出后注入）：
    /// 首帧据此校验恢复窗口是否落在现存显示器内（§13 风险 5）。
    pub fn with_restored_expectation(
        mut self,
        outer_min: egui::Pos2,
        inner_size: egui::Vec2,
    ) -> Self {
        self.restored_expectation = Some((outer_min, inner_size));
        self
    }

    /// 窗口恢复显示器覆盖校验（§13 风险 1；与 Task #28 窗口状态恢复衔接）：
    /// 启动恢复的位置若落在已断开的显示器上，egui 0.31 无显示器列表 API
    /// 可直接枚举校验，改用间接判定——对比首帧实际外框左上角与落盘期望
    /// 位置：相符 → 恢复落到了现存显示器内（Covered）；不符 → OS 已把
    /// 孤儿窗口钳回可见区（Orphaned），此时下发 center_on_screen 回退到
    /// 现存显示器居中。视口几何未就绪（outer_rect/monitor_size 缺失）逐帧
    /// 重试（Pending），一次性完成后不再判定。
    fn validate_window_restore(&mut self, ctx: &egui::Context) {
        if self.restore_checked {
            return;
        }
        let Some((expected, _inner_size)) = self.restored_expectation else {
            // 无恢复期望（window-state.json 缺失/损坏）：无需校验
            self.restore_checked = true;
            return;
        };
        let (outer, maximized) = ctx.input(|i| {
            let vp = i.viewport();
            (vp.outer_rect, vp.maximized.unwrap_or(false))
        });
        // 最大化窗口位置由 OS 管理，不参与判定（恢复恒为 normal 态，
        // 此分支仅为防御极端时序）
        let verdict = if maximized {
            RestoreVerdict::Pending
        } else {
            restore_verdict(outer.map(|r| r.min), expected)
        };
        match verdict {
            RestoreVerdict::Pending => {}
            RestoreVerdict::Covered => {
                self.restore_checked = true;
            }
            RestoreVerdict::Orphaned => {
                self.restore_checked = true;
                // egui 内建：按当前显示器 monitor_size 计算居中 OuterPosition；
                // monitor_size 未就绪返回 None → 保持未校验，下帧重试
                if let Some(cmd) = egui::ViewportCommand::center_on_screen(ctx) {
                    ctx.send_viewport_cmd(cmd);
                } else {
                    self.restore_checked = false;
                }
            }
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
                    module_id,
                    model_id,
                    percent,
                    bytes,
                    state,
                } => {
                    self.state.downloads.insert(
                        download_key(&module_id, &model_id),
                        DownloadUiState {
                            percent,
                            bytes,
                            state,
                        },
                    );
                }
                AppMsg::ModelDownloadFinished {
                    module_id,
                    model_id,
                    success,
                } => {
                    // 清理该模型的下载进度状态
                    self.state.downloads.remove(&download_key(&module_id, &model_id));
                    if success {
                        self.toasts.success(tr(
                            lang,
                            "desktopApp.toast.downloadComplete",
                            &[("id", &model_id)],
                        ));
                        // 清除旧的更新检查结果（刚下载完成必然最新）
                        self.state
                            .updates
                            .remove(&download_key(&module_id, &model_id));
                    }
                    // 失败/取消的具体原因由生产侧另行发送 Error/Info 消息，这里只刷新列表
                    // （状态可能从 Missing → Ready / Incomplete）
                    let _ = self.cmd_tx.send(AppCmd::RefreshModels);
                }
                AppMsg::ModelUpdateChecked {
                    module_id,
                    model_id,
                    result,
                    notify,
                } => {
                    let available = result.available;
                    // reason 为 ep-core 原始消息，按约定以本地化前缀 + 原文附加
                    let reason = result.reason.clone();
                    self.state
                        .updates
                        .insert(download_key(&module_id, &model_id), result);
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

    /// 外观同步：主题 / 缩放 / 字号均仅在状态变化时应用。
    ///
    /// 主题：切换时一次性全量 restyle（此前每帧无条件 `style_mut`，
    /// 与 egui 重绘调度路径叠加是主题切换白屏的诱因之一）；
    /// 缩放 / 字号：设置页修改 config 后即时生效（行为不变）。
    fn sync_appearance(&mut self, ctx: &egui::Context) {
        if self.applied_dark != Some(self.dark_theme) {
            self.applied_dark = Some(self.dark_theme);
            theme::apply_theme(ctx, self.dark_theme);
        }
        // P2 修复：钳制到 0.5~3.0（NaN/0/极端值 → 安全区间）；比对钳制后
        // 的目标值，损坏 config（如 NaN）也不会每帧重复应用
        let scale_target = crate::theme::clamp_scale_factor(self.state.config.ui.scale_factor);
        if scale_target != self.applied_scale {
            self.applied_scale = scale_target;
            ctx.set_zoom_factor(scale_target);
        }
        if self.state.config.ui.font_size != self.applied_font_size {
            self.applied_font_size = self.state.config.ui.font_size;
            theme::apply_font_size(ctx, self.applied_font_size);
        }
    }

    /// 视口尺寸变化检测：尺寸与上一帧不同时追加即时重绘（双帧 settle），
    /// 兜底 eframe 0.31 Windows resize 同步重绘丢帧，保证最大化/还原后
    /// 布局立刻跟随新窗口尺寸。首帧仅记录基准，不触发。
    fn track_viewport_size(&mut self, ctx: &egui::Context) {
        let size = ctx.input(|i| i.screen_rect.size());
        let changed = self.last_screen_size != Some(size);
        let first = self.last_screen_size.is_none();
        self.last_screen_size = Some(size);
        if changed && !first {
            ctx.request_repaint();
        }
    }

    /// 记录最新窗口外框/内容矩形（含副屏的全局坐标空间；仅在有效值时更新）。
    /// 退出时写 runtime/window-state.json，下次启动恢复位置/尺寸（P3-1）。
    ///
    /// Task #28：**最大化期间冻结基准**——只记录 normal 态矩形。
    /// 此前最大化退出会把最大化几何（近全屏尺寸 + 负偏移外框位置）落盘，
    /// 下次启动即以近全屏尺寸建窗，首帧保护再发 InnerSize 收缩，启动期
    /// 连续 Resized 事件恰好踩中 eframe 0.31 Windows 同步重绘丢帧路径
    ///（见 REPAINT_WATCHDOG 注释），是最大化冻结/重启白屏的诱因链。
    /// 修复后恢复恒为 normal 态（main.rs 加载端不使用任何最大化标志）。
    /// 另加几何兜底：最大化/还原动画过渡帧的 maximized 标志可能滞后，
    /// inner 超过显示器逻辑尺寸的帧不记录（防过渡值落盘）。
    fn track_window_rect(&mut self, ctx: &egui::Context) {
        let (outer, inner, maximized, monitor) = ctx.input(|i| {
            let vp = i.viewport();
            (
                vp.outer_rect,
                vp.inner_rect,
                vp.maximized.unwrap_or(false),
                vp.monitor_size,
            )
        });
        self.last_maximized = maximized;
        if !maximized {
            if outer.is_some() {
                self.last_window_outer = outer;
            }
            if let Some(inner) = inner {
                // 几何兜底：最大化/还原动画过渡帧存在 maximized 标志滞后
                //（rect 已变大但 is_maximized() 仍为 false），inner 尺寸
                // 超过所在显示器逻辑尺寸即视为过渡帧不记录；monitor_size
                // 缺失时退化为只靠标志（保持旧行为）。
                let within_monitor = monitor
                    .map(|m| inner.width() <= m.x + 1.0 && inner.height() <= m.y + 1.0)
                    .unwrap_or(true);
                if within_monitor {
                    self.last_window_inner = Some(inner);
                }
            }
        }
    }

    /// 首帧窗口保护：窗口宽/高超过屏幕 92% 时收缩到 92%
    fn fit_window_to_screen(&self, ctx: &egui::Context) {
        // 视口信息暂不可用时跳过（兜底）
        let (inner, maximized) = ctx.input(|i| {
            let vp = i.viewport();
            (vp.inner_rect, vp.maximized.unwrap_or(false))
        });
        let Some(inner) = inner else {
            return;
        };
        // Task #28：最大化状态跳过——此时下发 InnerSize 会立刻触发
        // Resized 事件链，踩 eframe 0.31 Windows 同步重绘丢帧路径
        if maximized {
            return;
        }
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

    /// 退出时持久化窗口位置/尺寸（P3-1，含副屏全局坐标）→
    /// `runtime/window-state.json`，启动时由 main.rs 的 ViewportBuilder 恢复。
    /// 与既有配置持久化同走 `resolve_root()` 根路径；不进 config/app.toml
    ///（高频变化的窗口状态与业务配置分离，避免干扰配置语义）。
    fn save_window_state(&self) {
        let Some(json) = Self::window_state_json(self.last_window_outer, self.last_window_inner)
        else {
            return;
        };
        let dir = ep_core::config::resolve_root().join("runtime");
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::write(dir.join("window-state.json"), json);
        }
    }

    /// 构建窗口状态 JSON：outer 为窗口外框左上角（全局坐标，副屏可为
    /// 负值/超主屏范围），inner 为内容尺寸；inner 缺失时回退 outer 尺寸。
    ///
    /// Task #28：落盘的恒为 normal（非最大化）态基准（track_window_rect
    /// 已门控）；不写也不读最大化标志——恢复永远以 normal 态建窗，
    /// 杜绝最大化坏状态跨启动传播。旧格式文件（无 maximized 字段）兼容。
    fn window_state_json(outer: Option<egui::Rect>, inner: Option<egui::Rect>) -> Option<String> {
        let outer = outer?;
        let size = inner.map(|r| r.size()).unwrap_or_else(|| outer.size());
        Some(
            serde_json::json!({
                "outer": { "x": outer.min.x, "y": outer.min.y },
                "inner": { "width": size.x, "height": size.y },
            })
            .to_string(),
        )
    }

}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // P1 修复：窗口关闭（X / 退出按钮）时通知后台执行全量停止——
        // 停止所有模块 + 取消在飞下载，避免后台线程被杀后留下孤儿子进程。
        // 幂等：后台循环退出后再次 send 静默失败，无害。
        let _ = self.cmd_tx.send(AppCmd::Shutdown);
        // P3-1：退出时记录窗口位置（含副屏场景），下次启动恢复
        self.save_window_state();
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 外观同步（主题/缩放/字号，仅状态变化时应用）
        self.sync_appearance(ctx);

        // ── 视口尺寸变化兜底（最大化/还原/resize 后布局即时跟随） ──
        self.track_viewport_size(ctx);

        // ── 记录窗口位置（退出时持久化，P3-1） ──
        self.track_window_rect(ctx);

        // ── 重绘看门狗：无条件挂起 ≤2s 的重绘截止时间（见常量注释） ──
        ctx.request_repaint_after(REPAINT_WATCHDOG);

        // ── 窗口尺寸保护（一次性） ──
        if !self.window_fitted {
            self.window_fitted = true;
            self.fit_window_to_screen(ctx);
        }

        // ── 窗口恢复显示器覆盖校验（一次性；孤儿窗口回退居中） ──
        self.validate_window_restore(ctx);

        // Poll messages from background thread
        self.process_messages();

        // 页面进入时一次性数据刷新（模块页：已装包注册表供来源徽章/卸载菜单；
        // 任务页：P1-6 任务拉取）
        if self.last_page != Some(self.current_page) {
            match self.current_page {
                Page::Modules => {
                    let _ = self.cmd_tx.send(AppCmd::RefreshPacks);
                }
                Page::Tasks => {
                    let _ = self.cmd_tx.send(AppCmd::RefreshTasks);
                }
                _ => {}
            }
            self.last_page = Some(self.current_page);
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
                // 模块管理单页（协调记录 #47）：模块卡 + 变体选择器 +
                // 工具栏导入/导出模块 + pack 来源徽章卸载
                pages::modules::show(
                    ui,
                    &self.state.config,
                    &mut self.state.modules,
                    &self.state.models,
                    &self.state.downloads,
                    &self.state.updates,
                    &mut self.state.download_sources,
                    &self.state.packs,
                    &self.state.pack_imports,
                    &self.cmd_tx,
                );
            }
            Page::PipelineEditor => {
                pages::pipeline_editor::show_full(ui, &self.state.config, Some(&self.cmd_tx));
            }
            Page::Tasks => {
                pages::tasks::show_full(
                    ui,
                    &self.state.config,
                    &self.state.modules,
                    &self.state.tasks,
                    Some(&self.cmd_tx),
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
            painter.rect_filled(rect, rounding, pal.bg_raised);
            // 左侧 3px 圆角指示条
            let bar = egui::Rect::from_min_max(
                egui::pos2(rect.min.x + 1.0, rect.min.y + 9.0),
                egui::pos2(rect.min.x + 4.0, rect.max.y - 9.0),
            );
            painter.rect_filled(bar, egui::CornerRadius::same(2), pal.primary);
        } else if response.hovered() {
            // hover 背景：bg_base 向 bg_raised 插值，两套主题下均弱于激活态
            painter.rect_filled(rect, rounding, pal.bg_base.lerp_to_gamma(pal.bg_raised, 0.6));
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

    // 无障碍名称（AX/UIA）：自绘导航行无 label 时为无名 "Custom" 节点，
    // 补与导航文案一致的 i18n 文本，读屏器可区分五个页面入口
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_label(label);
    });

    if compact {
        response.on_hover_text(label)
    } else {
        response
    }
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造可无头测试的 App（通道空转，配置默认值）
    fn test_app() -> App {
        let (_tx, rx) = std::sync::mpsc::channel();
        let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(rx, cmd_tx, AppConfig::default())
    }

    /// 以指定视口尺寸跑一帧（模拟 winit 传入的 screen_rect）
    fn run_pass(ctx: &egui::Context, size: egui::Vec2, mut f: impl FnMut(&egui::Context)) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| f(ctx));
    }

    /// 以指定视口矩形/最大化状态跑一帧（模拟 winit 的 ViewportInfo 填充）
    fn rect_pass(
        ctx: &egui::Context,
        outer: Option<egui::Rect>,
        inner: Option<egui::Rect>,
        maximized: bool,
        mut f: impl FnMut(&egui::Context),
    ) -> egui::FullOutput {
        let mut viewports = egui::ViewportIdMap::default();
        viewports.insert(
            egui::ViewportId::ROOT,
            egui::ViewportInfo {
                outer_rect: outer,
                inner_rect: inner,
                maximized: Some(maximized),
                ..Default::default()
            },
        );
        let size = inner
            .or(outer)
            .map(|r| r.size())
            .unwrap_or(egui::vec2(1280.0, 800.0));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            viewports,
            ..Default::default()
        };
        ctx.run(input, |ctx| f(ctx))
    }

    /// 同 rect_pass，但可注入 monitor_size（模拟 egui-winit 填充的
    /// 显示器逻辑尺寸，用于过渡帧几何兜底测试）
    fn rect_pass_mon(
        ctx: &egui::Context,
        outer: Option<egui::Rect>,
        inner: Option<egui::Rect>,
        maximized: bool,
        monitor: Option<egui::Vec2>,
        mut f: impl FnMut(&egui::Context),
    ) -> egui::FullOutput {
        let mut viewports = egui::ViewportIdMap::default();
        viewports.insert(
            egui::ViewportId::ROOT,
            egui::ViewportInfo {
                outer_rect: outer,
                inner_rect: inner,
                maximized: Some(maximized),
                monitor_size: monitor,
                ..Default::default()
            },
        );
        let size = inner
            .or(outer)
            .map(|r| r.size())
            .unwrap_or(egui::vec2(1280.0, 800.0));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            viewports,
            ..Default::default()
        };
        ctx.run(input, |ctx| f(ctx))
    }

    /// 信息架构终稿（协调记录 #47）：NAV 收敛为五入口——
    /// 仪表盘/模块/管线/任务/设置；「模型」旧入口与「整合包」已删除。
    #[test]
    fn nav_items_are_converged_five_entries() {
        let pages: Vec<Page> = NAV_ITEMS.iter().map(|(p, _, _, _)| *p).collect();
        assert_eq!(
            pages,
            vec![
                Page::Dashboard,
                Page::Modules,
                Page::PipelineEditor,
                Page::Tasks,
                Page::Settings,
            ],
            "NAV 必须恰为五个入口且顺序固定（#47 导航收敛）"
        );
    }

    /// Page 枚举与 NAV 一一对应（match 穷尽性由编译期保证，此处断言数量同步）
    #[test]
    fn page_enum_matches_nav_count() {
        assert_eq!(NAV_ITEMS.len(), 5);
        // 每个 NAV 条目都有非空图标与 i18n 键
        for &(_, icon, key, fallback) in NAV_ITEMS {
            assert!(!icon.is_empty());
            assert!(key.starts_with("desktopApp.nav."));
            assert!(!fallback.is_empty());
        }
    }

    /// P0-1 回归：主题切换即时生效且可反复切换（深→浅→深→浅）；
    /// 每套主题的 visuals 与 Palette 一致（panel_fill = bg，dark_mode 正确）。
    #[test]
    fn theme_switch_applies_both_themes_repeatedly() {
        let mut app = test_app();
        let ctx = egui::Context::default();

        for expect_dark in [true, false, true, false] {
            app.dark_theme = expect_dark;
            app.sync_appearance(&ctx);
            let style = ctx.style();
            assert_eq!(
                style.visuals.dark_mode, expect_dark,
                "visuals.dark_mode 应随主题切换"
            );
            let pal = Palette::new(expect_dark);
            assert_eq!(style.visuals.panel_fill, pal.bg_base, "panel_fill 应取自当前色板");
            assert_eq!(style.visuals.override_text_color, Some(pal.text));
        }
    }

    /// 主题未变化时不重复全量 restyle：外部对 style 的修改不被无谓覆盖；
    /// 主题切换后才重新应用（spacing 回到设计值）。
    #[test]
    fn theme_restyle_only_on_change() {
        let mut app = test_app();
        let ctx = egui::Context::default();
        app.sync_appearance(&ctx);

        // 模拟外部修改 spacing；未切主题时再次同步不应覆盖它
        ctx.style_mut(|s| s.spacing.item_spacing = egui::vec2(99.0, 99.0));
        app.sync_appearance(&ctx);
        assert_eq!(ctx.style().spacing.item_spacing, egui::vec2(99.0, 99.0));

        // 切换主题 → 全量 restyle 重新生效（item_spacing 回到 8x8）
        app.dark_theme = !app.dark_theme;
        app.sync_appearance(&ctx);
        assert_eq!(ctx.style().spacing.item_spacing, egui::vec2(8.0, 8.0));
    }

    /// 缩放在 config 变化后生效（set_zoom_factor 于下一 pass 起始生效）；
    /// 未变化时不重复应用（applied_scale 门控）。
    #[test]
    fn zoom_factor_applies_on_config_change() {
        let mut app = test_app();
        let ctx = egui::Context::default();
        app.sync_appearance(&ctx); // 首次：与构造值一致，不变更

        app.state.config.ui.scale_factor = 1.5;
        app.sync_appearance(&ctx);
        // set_zoom_factor 在下一 pass 起始生效
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        assert!((ctx.zoom_factor() - 1.5).abs() < 1e-6);

        // 再次同步（config 未变）不应重置或报错
        app.sync_appearance(&ctx);
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        assert!((ctx.zoom_factor() - 1.5).abs() < 1e-6);
    }

    /// P0-2 回归：视口尺寸变化（模拟最大化/还原）时追加即时重绘请求，
    /// 尺寸稳定后不再产生新请求。
    /// 注：egui 新 Context 默认 outstanding=1（启动期自带重绘），且
    /// request_repaint() 会额外留下一帧 settle，故按“消化后静默”断言。
    #[test]
    fn viewport_size_change_requests_repaint() {
        let mut app = test_app();
        let ctx = egui::Context::default();

        // 首帧：仅建立尺寸基准（此时启动期重绘尚未消化，不做断言）
        run_pass(&ctx, egui::vec2(1280.0, 800.0), |ctx| {
            app.track_viewport_size(ctx)
        });

        // 尺寸不变连续两帧：消化启动期重绘后应恢复静默
        for _ in 0..2 {
            run_pass(&ctx, egui::vec2(1280.0, 800.0), |ctx| {
                app.track_viewport_size(ctx)
            });
        }
        assert!(!ctx.has_requested_repaint(), "尺寸稳定后不应再有重绘请求");

        // 模拟最大化：尺寸变化 → 追加即时重绘
        run_pass(&ctx, egui::vec2(3840.0, 2120.0), |ctx| {
            app.track_viewport_size(ctx)
        });
        assert!(ctx.has_requested_repaint(), "尺寸变化必须触发兜底重绘");

        // 消化 settle 帧后恢复静默
        for _ in 0..2 {
            run_pass(&ctx, egui::vec2(3840.0, 2120.0), |ctx| {
                app.track_viewport_size(ctx)
            });
        }
        assert!(!ctx.has_requested_repaint(), "settle 后应恢复静默");

        // 模拟还原：再次变化 → 再次触发
        run_pass(&ctx, egui::vec2(1784.0, 1149.0), |ctx| {
            app.track_viewport_size(ctx)
        });
        assert!(ctx.has_requested_repaint(), "还原尺寸变化同样必须触发重绘");
    }

    /// 看门狗：每帧挂起的重绘截止时间使 ctx 始终持有待处理重绘，
    /// 保证事件循环不会在丢帧后永久停摆（冻结 ≤2s 自愈）。
    #[test]
    fn watchdog_keeps_repaint_pending() {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            ctx.request_repaint_after(REPAINT_WATCHDOG)
        });
        assert!(
            ctx.has_requested_repaint(),
            "看门狗必须使重绘请求处于挂起状态"
        );
    }

    /// P3-1：窗口状态 JSON 覆盖副屏全局坐标（x/y 可负/超主屏），
    /// inner 缺失回退 outer 尺寸，outer 缺失不落盘。
    #[test]
    fn window_state_json_covers_secondary_monitor_coords() {
        let outer =
            egui::Rect::from_min_size(egui::pos2(2560.0, -300.0), egui::vec2(1784.0, 1149.0));
        let inner =
            egui::Rect::from_min_size(egui::pos2(2560.0, -292.0), egui::vec2(1784.0, 1118.0));
        let json: serde_json::Value =
            serde_json::from_str(&App::window_state_json(Some(outer), Some(inner)).unwrap())
                .unwrap();
        assert_eq!(json["outer"]["x"], 2560.0);
        assert_eq!(json["outer"]["y"], -300.0);
        assert_eq!(json["inner"]["width"], 1784.0);
        assert_eq!(json["inner"]["height"], 1118.0);

        // inner 缺失 → 回退 outer 尺寸
        let fallback: serde_json::Value =
            serde_json::from_str(&App::window_state_json(Some(outer), None).unwrap()).unwrap();
        assert_eq!(fallback["inner"]["width"], 1784.0);
        assert_eq!(fallback["inner"]["height"], 1149.0);

        // outer 缺失 → 不落盘
        assert!(App::window_state_json(None, None).is_none());
    }

    /// Task #28 回归：最大化期间持久化基准冻结——只记录 normal 态矩形。
    /// 此前最大化退出会把近全屏几何 + 负偏移外框落盘，下次启动以近全屏
    /// 尺寸建窗 + 首帧收缩，启动期 Resized 风暴踩 eframe 0.31 同步重绘
    /// 丢帧路径（最大化冻结/重启白屏诱因链）。
    #[test]
    fn window_rect_baseline_freezes_while_maximized() {
        let mut app = test_app();
        let ctx = egui::Context::default();

        let normal_outer =
            egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(1000.0, 700.0));
        let normal_inner =
            egui::Rect::from_min_size(egui::pos2(108.0, 131.0), egui::vec2(984.0, 669.0));
        rect_pass(&ctx, Some(normal_outer), Some(normal_inner), false, |ctx| {
            app.track_window_rect(ctx)
        });
        assert_eq!(app.last_window_outer, Some(normal_outer));
        assert_eq!(app.last_window_inner, Some(normal_inner));
        assert!(!app.last_maximized);

        // 最大化：几何变化但基准冻结不变
        let max_outer =
            egui::Rect::from_min_size(egui::pos2(-7.33, -7.33), egui::vec2(2574.7, 1400.0));
        let max_inner =
            egui::Rect::from_min_size(egui::pos2(0.0, 22.7), egui::vec2(2560.0, 1369.3));
        rect_pass(&ctx, Some(max_outer), Some(max_inner), true, |ctx| {
            app.track_window_rect(ctx)
        });
        assert!(app.last_maximized, "最大化状态必须被识别");
        assert_eq!(
            app.last_window_outer, Some(normal_outer),
            "最大化期间外框基准不得被覆盖"
        );
        assert_eq!(
            app.last_window_inner, Some(normal_inner),
            "最大化期间内容尺寸基准不得被覆盖"
        );

        // 还原后新 normal 几何正常更新基准
        let restored_outer =
            egui::Rect::from_min_size(egui::pos2(300.0, 200.0), egui::vec2(1200.0, 800.0));
        let restored_inner =
            egui::Rect::from_min_size(egui::pos2(308.0, 231.0), egui::vec2(1184.0, 769.0));
        rect_pass(
            &ctx,
            Some(restored_outer),
            Some(restored_inner),
            false,
            |ctx| app.track_window_rect(ctx),
        );
        assert_eq!(app.last_window_outer, Some(restored_outer));
        assert_eq!(app.last_window_inner, Some(restored_inner));

        // 落盘内容 = normal 基准，不含最大化几何
        let json: serde_json::Value = serde_json::from_str(
            &App::window_state_json(app.last_window_outer, app.last_window_inner).unwrap(),
        )
        .unwrap();
        assert_eq!(json["outer"]["x"], 300.0);
        assert_eq!(json["inner"]["width"], 1184.0);
        assert!(json.get("maximized").is_none(), "不持久化最大化标志");
    }

    /// Task #28 回归：最大化/还原动画过渡帧的 maximized 标志可能滞后
    ///（rect 已变大但 is_maximized() 仍为 false）；此时 inner 尺寸超过
    /// 显示器逻辑尺寸的帧不得覆盖持久化基准（实机取证：最大化退出落盘
    /// 出现 1631×1042 过渡值）。monitor_size 缺失时退化为标志判定。
    #[test]
    fn window_rect_baseline_skips_maximize_transition_frames() {
        let mut app = test_app();
        let ctx = egui::Context::default();
        let monitor = Some(egui::vec2(1706.67, 960.0));

        let normal_outer =
            egui::Rect::from_min_size(egui::pos2(80.0, 60.0), egui::vec2(1206.0, 811.0));
        let normal_inner =
            egui::Rect::from_min_size(egui::pos2(87.3, 90.0), egui::vec2(1192.0, 780.0));
        rect_pass_mon(&ctx, Some(normal_outer), Some(normal_inner), false, monitor, |ctx| {
            app.track_window_rect(ctx)
        });
        assert_eq!(app.last_window_inner, Some(normal_inner));

        // 过渡帧：标志仍 false 但尺寸已超显示器 → 不覆盖基准
        let trans_outer =
            egui::Rect::from_min_size(egui::pos2(40.0, 30.0), egui::vec2(2460.0, 1576.0));
        let trans_inner =
            egui::Rect::from_min_size(egui::pos2(45.0, 64.0), egui::vec2(1631.3, 1042.0));
        rect_pass_mon(&ctx, Some(trans_outer), Some(trans_inner), false, monitor, |ctx| {
            app.track_window_rect(ctx)
        });
        assert_eq!(
            app.last_window_inner, Some(normal_inner),
            "超显示器尺寸的过渡帧不得覆盖 inner 基准"
        );

        // monitor_size 缺失：退化为标志判定，normal 帧照常记录
        let mut app2 = test_app();
        rect_pass_mon(&ctx, Some(trans_outer), Some(trans_inner), false, None, |ctx| {
            app2.track_window_rect(ctx)
        });
        assert_eq!(app2.last_window_inner, Some(trans_inner));
    }

    /// Task #28 回归：首帧尺寸保护在最大化状态下不得下发 InnerSize
    ///（会立刻触发 Resized 事件链，踩 eframe 0.31 同步重绘丢帧路径）；
    /// normal 态超屏仍照常收缩。
    #[test]
    fn fit_window_to_screen_skips_when_maximized() {
        let app = test_app();
        let ctx = egui::Context::default();
        let oversized_inner =
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(2560.0, 1400.0));

        // normal 态超屏 → 收缩（有 InnerSize 命令）
        let out = rect_pass(&ctx, None, Some(oversized_inner), false, |ctx| {
            app.fit_window_to_screen(ctx)
        });
        let cmds = &out.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(
            cmds.iter()
                .any(|c| matches!(c, egui::ViewportCommand::InnerSize(_))),
            "normal 态超屏必须收缩"
        );

        // 最大化态超屏 → 跳过（无 InnerSize 命令）
        let out = rect_pass(&ctx, None, Some(oversized_inner), true, |ctx| {
            app.fit_window_to_screen(ctx)
        });
        let cmds = &out.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(
            cmds.iter()
                .all(|c| !matches!(c, egui::ViewportCommand::InnerSize(_))),
            "最大化状态下不得下发 InnerSize（避免启动/切换期 resize 风暴）"
        );
    }

    /// 窗口恢复显示器覆盖校验：孤儿窗口（落盘位置在无覆盖显示器）被 OS
    /// 钳回后位置不符 → 下发居中命令回退现存显示器；位置相符 → 不下发
    /// 任何位置命令；outer_rect 缺失 → Pending 不判定（下帧重试）。
    #[test]
    fn window_restore_orphaned_falls_back_to_center() {
        let ctx = egui::Context::default();
        let monitor = Some(egui::vec2(2560.0, 1440.0));
        // 期望：落盘的副屏坐标（该显示器已断开）
        let expected_pos = egui::pos2(2560.0, -300.0);

        // ── 孤儿场景：实际被 OS 钳到主屏（位置不符）→ 居中回退 ──
        let mut app = test_app().with_restored_expectation(expected_pos, egui::vec2(1184.0, 769.0));
        let clamped_outer =
            egui::Rect::from_min_size(egui::pos2(120.0, 80.0), egui::vec2(1200.0, 800.0));
        let out = rect_pass_mon(&ctx, Some(clamped_outer), None, false, monitor, |ctx| {
            app.validate_window_restore(ctx)
        });
        let cmds = &out.viewport_output[&egui::ViewportId::ROOT].commands;
        let centered = cmds.iter().find_map(|c| match c {
            egui::ViewportCommand::OuterPosition(p) => Some(*p),
            _ => None,
        });
        assert_eq!(
            centered,
            Some(egui::pos2(680.0, 320.0)),
            "孤儿窗口必须回退到现存显示器居中"
        );
        assert!(app.restore_checked, "判定完成后不得重复校验");

        // ── 覆盖场景：实际位置与期望相符 → 无任何位置命令 ──
        let mut app = test_app().with_restored_expectation(expected_pos, egui::vec2(1184.0, 769.0));
        let ok_outer = egui::Rect::from_min_size(expected_pos, egui::vec2(1200.0, 800.0));
        let out = rect_pass_mon(&ctx, Some(ok_outer), None, false, monitor, |ctx| {
            app.validate_window_restore(ctx)
        });
        let cmds = &out.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(
            cmds.iter()
                .all(|c| !matches!(c, egui::ViewportCommand::OuterPosition(_))),
            "恢复落在现存显示器内时不得移动窗口"
        );
        assert!(app.restore_checked);

        // ── Pending 场景：outer_rect 缺失 → 不判定不标记完成 ──
        let mut app = test_app().with_restored_expectation(expected_pos, egui::vec2(1184.0, 769.0));
        rect_pass_mon(&ctx, None, None, false, monitor, |ctx| {
            app.validate_window_restore(ctx)
        });
        assert!(!app.restore_checked, "几何未就绪时必须逐帧重试");
    }

    /// 窗口恢复纯函数判定：容差内相符 / 超出容差 / 几何缺失。
    #[test]
    fn restore_verdict_tolerates_small_offsets() {
        let expected = egui::pos2(2560.0, -300.0);
        assert_eq!(
            restore_verdict(Some(expected), expected),
            RestoreVerdict::Covered
        );
        // 容差边界内（~5.66 < 8）
        assert_eq!(
            restore_verdict(Some(egui::pos2(2564.0, -296.0)), expected),
            RestoreVerdict::Covered
        );
        // 超出容差（OS 钳回量级）
        assert_eq!(
            restore_verdict(Some(egui::pos2(120.0, 80.0)), expected),
            RestoreVerdict::Orphaned
        );
        assert_eq!(restore_verdict(None, expected), RestoreVerdict::Pending);
    }

    /// P1 回归：窗口关闭（on_exit）必须向后台发出全量停止命令，
    /// 防止后台线程被杀后模块/下载子进程孤儿化。
    #[test]
    fn on_exit_notifies_background_shutdown() {
        use eframe::App as _; // 引入 eframe::App trait 以调用 on_exit
        let (_tx, rx) = std::sync::mpsc::channel();
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(rx, cmd_tx, AppConfig::default());
        app.on_exit(None);
        assert!(
            matches!(cmd_rx.try_recv(), Ok(AppCmd::Shutdown)),
            "on_exit 必须发送 AppCmd::Shutdown 触发后台全量停止"
        );
    }

    /// P1 回归：跨模块同名模型变体（如两模块都有 "small"）的下载进度
    /// 以 (module_id, model_id) 复合键隔离，互不覆盖。
    #[test]
    fn download_progress_isolated_by_module_model_composite_key() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(rx, cmd_tx, AppConfig::default());

        tx.send(AppMsg::ModelDownloadProgress {
            module_id: "whisper-a".into(),
            model_id: "small".into(),
            percent: 10.0,
            bytes: 100,
            state: DownloadState::Downloading,
        })
        .unwrap();
        tx.send(AppMsg::ModelDownloadProgress {
            module_id: "whisper-b".into(),
            model_id: "small".into(),
            percent: 50.0,
            bytes: 500,
            state: DownloadState::Downloading,
        })
        .unwrap();
        app.process_messages();

        assert_eq!(
            app.state.downloads.len(),
            2,
            "同名变体必须按复合键隔离，不得互相覆盖"
        );
        assert_eq!(
            app.state.downloads["whisper-a:small"].percent,
            10.0
        );
        assert_eq!(
            app.state.downloads["whisper-b:small"].percent,
            50.0
        );
    }

    /// P1 回归：下载终态按复合键清理——只移除对应模块的进度，
    /// 另一个模块的同名变体进度不受影响。
    #[test]
    fn download_finished_removes_only_matching_composite_key() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(rx, cmd_tx, AppConfig::default());

        for (m, pct) in [("a", 20.0), ("b", 30.0)] {
            tx.send(AppMsg::ModelDownloadProgress {
                module_id: m.into(),
                model_id: "small".into(),
                percent: pct,
                bytes: 1,
                state: DownloadState::Downloading,
            })
            .unwrap();
        }
        tx.send(AppMsg::ModelDownloadFinished {
            module_id: "a".into(),
            model_id: "small".into(),
            success: true,
        })
        .unwrap();
        app.process_messages();

        assert!(!app.state.downloads.contains_key("a:small"));
        assert!(
            app.state.downloads.contains_key("b:small"),
            "b 模块同名变体下载不应被 a 的终态误清理"
        );
    }
}
