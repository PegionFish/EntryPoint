//! 主窗口（eframe/egui）与托盘共存壳（GT-1，2026-11-05）。
//!
//! 职责：egui 主窗口骨架（四页导航 + 状态行）、托盘图标三色/菜单事件接线、
//! 「关窗=缩托盘」、CJK 字体运行时加载。红线沿袭：**壳不承载业务**——
//! 本模块只做展示与命令转发（daemon 命令经 [`DaemonCommand`] 通道）。
//!
//! 事件模型：tray-icon 的菜单/图标事件经 `set_event_handler` 推入共享队列
//! （[`UiShared`]，跨线程唯一通道），egui 每帧 drain；其余一切（托盘实体、
//! 图标换色、标志位）都收敛在主线程，无锁。

use std::os::windows::process::CommandExt as _; // creation_flags（open_webui）
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::autostart;
use crate::icons;
use crate::platform::daemon::{DaemonCommand, StateUpdate};
use crate::platform::elevate::{self, ElevateError, ScAction, EXIT_DENIED, EXIT_OK};
use crate::platform::export::{
    build_zip, collect_export_set, current_month_local, ExportOptions, ExportOutcome,
};
use crate::platform::logpipe::ws_source::WsConnState;
use crate::platform::logpipe::LogPipeHandle;
use crate::platform::logs::LogPage;
use crate::platform::probe_ext::{
    DeviceSnapshot, HealthSnapshot, ProbeSlot, QueueSnapshot, DEVICES_INTERVAL, HEALTH_INTERVAL,
    QUEUE_INTERVAL, REQUEST_TIMEOUT, STALE_AFTER,
};
use crate::platform::registry::HkuRunKey;
use crate::platform::scm::{
    query_service, RunMode, ScmError, ServiceSnap, ServiceState, StartKind, StartType,
    SERVICE_NAME,
};
use crate::supervisor::TrayColor;

/// CREATE_NO_WINDOW：隐藏 cmd 子进程的控制台窗口（开浏览器用）
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 跨线程共享：supervisor 状态 + 托盘事件队列（主线程每帧 drain）。
#[derive(Default)]
pub(crate) struct UiShared {
    /// 托管线程最新状态（None=尚未收到）
    pub state: Option<StateUpdate>,
    /// 托盘菜单点击事件
    pub menu_events: Vec<MenuEvent>,
    /// 托盘图标点击事件
    pub tray_events: Vec<TrayIconEvent>,

    // ── GT-2：daemon HTTP API 探询快照（probe_ext 线程写入；logic/渲染只读；
    //    GT-4 日志接收口照此模式加字段）──
    /// /api/health 快照槽（2s 档）
    pub health: ProbeSlot<HealthSnapshot>,
    /// /api/queue 快照槽（5s 档）
    pub queue: ProbeSlot<QueueSnapshot>,
    /// /api/devices 快照槽（10s 档，条目 = 物理归并后的设备）
    pub devices: ProbeSlot<Vec<DeviceSnapshot>>,
    /// CJK 字体加载结果：Some = 已加载的字体文件名；None = 全部候选失败，
    /// 降级 egui 默认字体（GT-1 的 eprintln 提升为设置页可见状态）
    pub cjk_font: Option<String>,

    // ── GT-3：运行模式 + SCM 服务快照（scm_watch 线程启动即查 + 30s 复检；
    //    管控页提权动作完成后由动作线程即时复检刷新）──
    /// 运行模式徽章（Service=EntryPoint 服务已安装 / Sidecar=未安装）
    pub mode: RunMode,
    /// EntryPoint 服务快照槽（Service 模式管控页/状态页显示；sidecar 时常为空）
    pub service: ProbeSlot<ServiceSnap>,
    /// 管控页提权动作反馈（动作线程写：Pending → 终态；UI 渲染只读）
    pub control_op: Option<ControlOp>,
}

/// 管控页提权动作反馈（一次 UAC 动作一条；动作完成时动作线程顺带即时复检
/// SCM 快照，30s 复检线程到来前界面先到位）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ControlOp {
    /// 动作名（ScAction::label，如「启动服务」）
    pub label: String,
    /// 当前状态
    pub state: ControlOpState,
    /// 状态写入时刻（UI 显示进行时长用）
    pub at: Instant,
}

/// 提权动作状态（Pending=等待 UAC/执行中；终态 Ok/Denied/Failed）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControlOpState {
    Pending,
    Ok,
    Denied,
    Failed(String),
}

/// 主窗口导航页（GT-1 仅骨架，内容归 GT-2/3/5/6）
#[derive(Debug, Clone, Copy, PartialEq)]
enum Page {
    Status,
    Control,
    Logs,
    Settings,
}

/// eframe 应用本体（全部字段主线程访问；唯一跨线程面是 [`UiShared`]）。
pub(crate) struct TrayApp {
    port: u16,
    /// UI → 托管线程命令通道
    cmd_tx: Sender<DaemonCommand>,
    shared: Arc<Mutex<UiShared>>,

    // ── GT-5：日志引擎句柄（app.rs creator 里 LogPipe::spawn 的产物；主线程
    //    独占——logic 帧 drain_new 需 &mut，ws_status/drop_count 为 &self 只读）──
    log_handle: LogPipeHandle,
    /// 日志页状态（显示缓冲 + 过滤视图；logic 进数据，ui 渲染）
    logs: LogPage,

    // ── GT-6：导出状态（跨线程面 = export_shared；选项/弹窗为主线程态）──
    /// EP root（与 LogPipeConfig.root 同源——app.rs 一次 resolve 两处消费）
    root: PathBuf,
    /// egui 上下文（rfd 异步回调线程写结果后 request_repaint 用）
    ctx: egui::Context,
    export_shared: Arc<Mutex<ExportUi>>,
    export_popup: bool,
    export_opts: ExportOptions,

    // ── 托盘（TrayIcon 实体在首个 update 帧创建；tray-icon#90 要求事件循环已运行）──
    menu: Menu,
    status_item: MenuItem,
    open_item: MenuItem,
    console_item: MenuItem,
    restart_item: MenuItem,
    stop_item: MenuItem,
    autostart_item: CheckMenuItem,
    quit_item: MenuItem,
    tray: Option<TrayIcon>,
    current_color: Option<TrayColor>,

    // ── 窗口/状态（主线程标志位）──
    selected: Page,
    last_label: Option<String>,
    /// 手动启动（非 --minimized）：daemon 首次就绪后自动打开 WebUI 一次
    pending_open: bool,
    /// 「打开管理台」菜单置位 → logic 内发 Visible(true)
    pending_show: bool,
    /// 菜单「退出」置位：下一次 close 请求真退出（否则关窗=缩托盘）
    really_quit: bool,
    /// Shutdown 命令只发一次
    shutdown_sent: bool,
    /// 帧率实测（EP_TRAY_FRAME_DEBUG=1 时每 60 帧打印平均耗时；GT-5 留证用）
    frame_dbg: FrameDebug,
}

/// 帧耗时累计（`frame_debug_enabled` 门控，生产零输出）
#[derive(Default)]
struct FrameDebug {
    frames: u64,
    logic: Duration,
    ui: Duration,
}

/// 导出流程 UI 态（跨线程：rfd 回调线程 + 打包线程写，logic/ui 读）
#[derive(Default)]
pub(crate) struct ExportUi {
    /// 保存对话框已确认、打包进行中（按钮 Pending 门控）
    pending: bool,
    /// 打包完成结果（Some 即展示；下一次导出前清空）
    outcome: Option<Result<ExportOutcome, String>>,
}

impl TrayApp {
    /// 构建应用（在 eframe creator 闭包内调用；--minimized 由 ViewportBuilder
    /// 的 with_visible 处理，此处不管）。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        cc: &eframe::CreationContext<'_>,
        port: u16,
        cmd_tx: Sender<DaemonCommand>,
        shared: Arc<Mutex<UiShared>>,
        log_handle: LogPipeHandle,
        root: PathBuf,
        menu: Menu,
        handles: TrayMenuHandles,
        pending_open: bool,
    ) -> Self {
        let cjk_font_loaded = load_cjk_fonts(&cc.egui_ctx);
        // 字体加载结果入共享面：GT-2 设置页展示加载状态（失败降级可见化）
        if let Ok(mut s) = shared.lock() {
            s.cjk_font = cjk_font_loaded.map(str::to_owned);
        }
        // 联调钩子：EP_TRAY_PAGE=logs|control|settings 覆盖起始页（截图留证用）
        let selected = std::env::var("EP_TRAY_PAGE")
            .ok()
            .and_then(|p| match p.as_str() {
                "control" => Some(Page::Control),
                "logs" => Some(Page::Logs),
                "settings" => Some(Page::Settings),
                _ => None,
            })
            .unwrap_or(Page::Status);
        Self {
            selected,
            port,
            cmd_tx,
            shared,
            log_handle,
            logs: LogPage::new(),
            root,
            ctx: cc.egui_ctx.clone(),
            export_shared: Arc::new(Mutex::new(ExportUi::default())),
            export_popup: false,
            export_opts: ExportOptions::default(),
            menu,
            status_item: handles.status_item,
            open_item: handles.open_item,
            console_item: handles.console_item,
            restart_item: handles.restart_item,
            stop_item: handles.stop_item,
            autostart_item: handles.autostart_item,
            quit_item: handles.quit_item,
            tray: None,
            current_color: None,
            last_label: None,
            pending_open,
            pending_show: false,
            really_quit: false,
            shutdown_sent: false,
            frame_dbg: FrameDebug::default(),
        }
    }

    /// 每帧：drain 共享队列 → 托盘事件分发 / 状态同步 / 标志位落地。
    fn pump_shared(&mut self, ctx: &egui::Context) {
        let (menu_events, tray_events, state) = {
            let mut s = self.shared.lock().expect("ui shared poisoned");
            (
                std::mem::take(&mut s.menu_events),
                std::mem::take(&mut s.tray_events),
                s.state.take(),
            )
        };

        for ev in menu_events {
            self.on_menu(ev);
        }
        if !tray_events.is_empty() {
            // 左键单击托盘图标 = 打开主窗口（Windows 惯例）
            self.pending_show = true;
        }
        if let Some(update) = state {
            self.on_state(update);
        }

        // 标志位 → 视口命令（统一在此执行，on_menu 只置位）
        if self.pending_show {
            self.pending_show = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    fn on_menu(&mut self, ev: MenuEvent) {
        if ev.id == self.open_item.id() {
            open_webui(self.port);
        } else if ev.id == self.console_item.id() {
            self.pending_show = true;
        } else if ev.id == self.restart_item.id() {
            let _ = self.cmd_tx.send(DaemonCommand::Restart);
        } else if ev.id == self.stop_item.id() {
            let _ = self.cmd_tx.send(DaemonCommand::Stop);
        } else if ev.id == self.autostart_item.id() {
            let enabled = self.autostart_item.is_checked();
            let result = std::env::current_exe()
                .map_err(|e| format!("定位 exe: {e}"))
                .and_then(|exe| autostart::set_autostart(&HkuRunKey, &exe, enabled));
            if let Err(e) = result {
                eprintln!("ep-tray: 自启开关写入失败: {e}");
                self.autostart_item.set_checked(!enabled); // 回滚 UI
            }
        } else if ev.id == self.quit_item.id() {
            self.really_quit = true;
        }
    }

    fn on_state(&mut self, update: StateUpdate) {
        if self.current_color != Some(update.color) {
            match icon_for(update.color) {
                Ok(icon) => {
                    if let Some(t) = self.tray.as_mut() {
                        if t.set_icon(Some(icon)).is_ok() {
                            self.current_color = Some(update.color);
                        }
                    }
                }
                Err(e) => eprintln!("ep-tray: {e}"),
            }
        }
        self.status_item.set_text(update.label.clone());
        self.last_label = Some(update.label);

        if self.pending_open && update.color == TrayColor::Green {
            open_webui(self.port);
            self.pending_open = false;
        }
    }
}

impl eframe::App for TrayApp {
    /// 帧逻辑（含窗口隐藏时——supervisor 状态/托盘事件靠 forwarder 的
    /// request_repaint 唤醒本方法继续驱动托盘图标与命令）。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let t0 = Instant::now();
        // 首次：创建托盘实体（tray-icon#90：须在事件循环运行后创建）
        if self.tray.is_none() {
            match build_tray(&self.menu) {
                Ok(t) => self.tray = Some(t),
                Err(e) => {
                    eprintln!("ep-tray: {e}");
                    self.really_quit = true;
                }
            }
        }

        self.pump_shared(ctx);

        // GT-5：日志引擎 → 显示缓冲（logic 帧增量 drain；UI 线程零 IO——
        // IO 全在 logpipe 三线程，本处只搬内存）
        let new_rows = self.log_handle.drain_new();
        if !new_rows.is_empty() {
            self.logs.ingest(new_rows);
        }
        // 帧率实测钩子（EP_TRAY_LOG_BENCH=<n>；缓冲为空时首帧注入一次）
        crate::platform::logs::maybe_bench_inject(&mut self.logs);

        // 关窗拦截：非退出路径 → CancelClose + 缩托盘（eframe 0.37 契约：
        // 取消关闭 = 响应 close_requested 后发 CancelClose）
        if ctx.input(|i| i.viewport().close_requested()) && !self.really_quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        // 菜单「退出」：停 daemon + 真退出
        if self.really_quit && !self.shutdown_sent {
            self.shutdown_sent = true;
            let _ = self.cmd_tx.send(DaemonCommand::Shutdown);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // 空闲保活：5fps 轮询兜底（探活节奏 400ms；forwarder 线程另有即时唤醒）
        ctx.request_repaint_after(Duration::from_millis(200));

        self.frame_dbg.logic += t0.elapsed();
    }

    /// 主窗口渲染（0.37 新签名：裸 &mut Ui，无 margin——CentralPanel 兜底）。
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let t0 = Instant::now();
        egui::Panel::top("top_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                let (color, label) = self.status_view();
                let (r, g, b) = icons::status_rgb(color);
                ui.label(
                    egui::RichText::new("●")
                        .color(egui::Color32::from_rgb(r, g, b))
                        .size(16.0),
                );
                ui.strong(label);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("打开 WebUI").clicked() {
                        open_webui(self.port);
                    }
                    ui.separator();
                    // GT-3 模式徽章（全局）：sidecar ∥ Windows Service
                    let mode = self
                        .shared
                        .lock()
                        .map(|s| s.mode)
                        .unwrap_or(RunMode::Sidecar);
                    let (text, fg, tip) = mode_badge_view(mode);
                    let badge = ui.label(
                        egui::RichText::new(text)
                            .color(fg)
                            .background_color(egui::Color32::from_rgb(40, 44, 52)),
                    );
                    badge.on_hover_text(tip);
                });
            });
        });

        egui::Panel::left("nav").show(ui, |ui| {
            ui.add_space(8.0);
            ui.set_min_width(120.0);
            ui.heading("EntryPoint");
            ui.separator();
            for (page, name) in [
                (Page::Status, "状态"),
                (Page::Control, "管控"),
                (Page::Logs, "日志"),
                (Page::Settings, "设置"),
            ] {
                if ui.selectable_label(self.selected == page, name).clicked() {
                    self.selected = page;
                }
            }
        });

        egui::CentralPanel::default().show(ui, |ui| match self.selected {
            Page::Status => self.ui_status(ui),
            Page::Control => self.ui_control(ui),
            Page::Logs => self.ui_logs(ui),
            Page::Settings => self.ui_settings(ui),
        });

        // GT-6 导出弹窗（独立 Window；Pending 期按钮禁用，结果就地展示）
        self.ui_export_popup(ui.ctx());

        // 帧率实测（EP_TRAY_FRAME_DEBUG=1；GT-5 留证）
        self.frame_dbg.ui += t0.elapsed();
        self.frame_debug_tick();
    }
}

// ── GT-6：日志导出（弹窗 + rfd 异步保存 + 后台打包线程） ─────────────────

/// 导出弹窗（选项 + Pending 门控 + 结果就地展示）。
impl TrayApp {
    fn ui_export_popup(&mut self, ctx: &egui::Context) {
        if !self.export_popup {
            return;
        }
        let mut open = self.export_popup;
        egui::Window::new("导出日志")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                {
                    // —— 选项区 ——
                    ui.horizontal(|ui| {
                        ui.label("事件日志范围:");
                        let cur = self.export_opts.events;
                        for (range, name) in [
                            (crate::platform::export::EventsRange::All, "全部月份"),
                            (
                                crate::platform::export::EventsRange::CurrentMonth,
                                "仅当前月",
                            ),
                        ] {
                            if ui
                                .radio(self.export_opts.events == range, name)
                                .clicked()
                            {
                                self.export_opts.events = range;
                            }
                            let _ = cur;
                        }
                    });
                    ui.checkbox(&mut self.export_opts.include_tasks, "含任务注册表（task-*.json）");
                    ui.checkbox(&mut self.export_opts.include_config, "含 config/app.toml（**密钥自动掩码**）");

                    ui.separator();
                    let (pending, outcome) = {
                        let st = self
                            .export_shared
                            .lock()
                            .expect("export shared poisoned");
                        (st.pending, st.outcome.clone())
                    };

                    if pending {
                        ui.add(egui::widgets::Spinner::new().size(18.0));
                        ui.weak("打包中…（保存对话框确认后开始）");
                    } else {
                        ui.horizontal(|ui| {
                            if ui.button("选择保存位置并导出").clicked() {
                                self.start_export();
                            }
                            if ui.button("关闭").clicked() {
                                self.export_popup = false;
                            }
                        });
                    }

                    // —— 结果区 ——
                    if let Some(outcome) = &outcome {
                        ui.separator();
                        match outcome {
                            Ok(o) => {
                                ui.colored_label(
                                    egui::Color32::from_rgb(80, 200, 120),
                                    format!("✓ 导出完成：{} 个文件", o.files),
                                );
                                ui.weak(format!(
                                    "{}（{} KB）",
                                    o.out_path.display(),
                                    o.total_bytes / 1024
                                ));
                                if o.masked_lines > 0 {
                                    ui.weak(format!("已掩码敏感行 {} 行", o.masked_lines));
                                }
                                if !o.skipped.is_empty() {
                                    ui.weak(format!("跳过 {} 项：", o.skipped.len()));
                                    for s in &o.skipped {
                                        ui.weak(format!("  · {s}"));
                                    }
                                }
                            }
                            Err(e) => {
                                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), format!("✗ 导出失败: {e}"));
                            }
                        }
                    }
                }
            });
        // X 按钮 / 关闭键 → open=false 回写（闭包内借用 self 不可直接写）
        self.export_popup = open;
    }

    /// 发起导出：rfd 同步 FileDialog 放**独立线程**（Windows 支持任意线程打开
    /// IFileDialog；不阻塞 winit 主循环）→ 用户选路径 → 同线程 collect+build_zip
    /// → 结果写共享 + request_repaint。
    fn start_export(&mut self) {
        {
            let mut st = self.export_shared.lock().expect("export shared poisoned");
            if st.pending {
                return;
            }
            st.pending = true;
            st.outcome = None;
        }
        let shared = Arc::clone(&self.export_shared);
        let ctx = self.ctx.clone();
        let root = self.root.clone();
        let opts = self.export_opts;
        let default_name = {
            // 本地日期命名（复用 logs.rs 的本地时区换算链，不引 chrono）
            let ts = std::time::SystemTime::now();
            let ymd = format!("{}{:02}", current_month_local(), local_day(ts));
            format!("EntryPoint-logs-{ymd}")
        };
        let _ = std::thread::Builder::new()
            .name("ep-tray-export".into())
            .spawn(move || {
                // 对话框本身也在该线程（选型报告：rfd 0.17 Async 回调 API 已移除）
                let picked = rfd::FileDialog::new()
                    .set_file_name(&default_name)
                    .save_file();
                let outcome = match picked {
                    None => {
                        // 用户取消：解除 Pending，不出结果
                        if let Ok(mut st) = shared.lock() {
                            st.pending = false;
                        }
                        ctx.request_repaint();
                        return;
                    }
                    Some(path) => {
                        let month = current_month_local();
                        let items = collect_export_set(&root, &opts, &month);
                        build_zip(&items, &path).map_err(|e| e.to_string())
                    }
                };
                if let Ok(mut st) = shared.lock() {
                    st.pending = false;
                    st.outcome = Some(outcome);
                }
                ctx.request_repaint();
            });
    }
}

/// 本地日（导出文件名用）：SystemTime → 本地 SYSTEMTIME 取 wDay。
fn local_day(ts: std::time::SystemTime) -> u32 {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{
        FileTimeToSystemTime, GetTimeZoneInformation, SystemTimeToTzSpecificLocalTime,
        TIME_ZONE_ID_INVALID, TIME_ZONE_INFORMATION,
    };
    const EPOCH_DIFF_100NS: u64 = 11_644_473_600 * 10_000_000;
    let d = ts.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let ft_100ns = d.as_secs() * 10_000_000 + (d.subsec_nanos() / 100) as u64 + EPOCH_DIFF_100NS;
    let ft = FILETIME {
        dwLowDateTime: (ft_100ns & 0xFFFF_FFFF) as u32,
        dwHighDateTime: (ft_100ns >> 32) as u32,
    };
    let mut utc = SYSTEMTIME { wYear: 0, wMonth: 0, wDayOfWeek: 0, wDay: 0, wHour: 0, wMinute: 0, wSecond: 0, wMilliseconds: 0 };
    // 关键：先把 FILETIME 转成 UTC SYSTEMTIME，再送时区转换（空 SYSTEMTIME 会算出垃圾日期）
    unsafe { FileTimeToSystemTime(&ft, &mut utc) };
    let mut tzi = TIME_ZONE_INFORMATION::default();
    let mut local = SYSTEMTIME { wYear: 0, wMonth: 0, wDayOfWeek: 0, wDay: 0, wHour: 0, wMinute: 0, wSecond: 0, wMilliseconds: 0 };
    let tz_ok = unsafe { GetTimeZoneInformation(&mut tzi) } != TIME_ZONE_ID_INVALID
        && unsafe { SystemTimeToTzSpecificLocalTime(&tzi, &utc, &mut local) } != 0;
    if tz_ok {
        return u32::from(local.wDay);
    }
    0
}

impl TrayApp {
    /// 每 60 帧打印一次平均耗时（窗口可见时才计入 ui 值；stderr 输出）
    fn frame_debug_tick(&mut self) {
        if !crate::platform::logs::frame_debug_enabled() {
            return;
        }
        self.frame_dbg.frames += 1;
        if self.frame_dbg.frames.is_multiple_of(60) {
            eprintln!(
                "ep-tray: 帧均耗时（最近 60 帧）：logic={:?} ui={:?}",
                self.frame_dbg.logic / 60,
                self.frame_dbg.ui / 60
            );
            self.frame_dbg.logic = Duration::ZERO;
            self.frame_dbg.ui = Duration::ZERO;
        }
    }
}

/// 顶栏状态视图（颜色 + 去掉圆点前缀的文字）。
impl TrayApp {
    fn status_view(&self) -> (TrayColor, String) {
        match &self.last_label {
            Some(label) => (
                self.current_color.unwrap_or(TrayColor::Yellow),
                label.trim_start_matches("● ").to_string(),
            ),
            None => (TrayColor::Yellow, "启动中…".into()),
        }
    }
}

/// 探询无数据/失联时的统一占位文案（不显示空白）。
const PROBE_PLACEHOLDER: &str = "—（探询失联/未就绪）";

/// 模式徽章视图（纯函数）：返回 (文字, 前景色, tooltip)——顶部全局徽章与
/// 管控页共用。颜色语义与 daemon 状态三色正交（青=Service / 灰=sidecar）。
pub(crate) fn mode_badge_view(mode: RunMode) -> (&'static str, egui::Color32, &'static str) {
    match mode {
        RunMode::Service => (
            "服务模式",
            egui::Color32::from_rgb(110, 195, 205),
            concat!(
                "Windows 服务（EntryPoint）已安装：daemon 由 SCM 托管；",
                "启停/重启/自启写操作经 UAC 提权一次性子进程执行"
            ),
        ),
        RunMode::Sidecar => (
            "sidecar 模式",
            egui::Color32::GRAY,
            "Windows 服务未安装：daemon 由 ep-tray 以子进程（sidecar）托管；可在管控页查看服务安装指引",
        ),
    }
}

/// 设置页最小版（GT-2）：探询节奏只读展示 / 打开 WebUI / 托盘与版本 /
/// CJK 字体加载状态。root 覆盖、轮询可调、缓冲上限留给后续票。
impl TrayApp {
    fn ui_settings(&self, ui: &mut egui::Ui) {
        ui.heading("设置");
        let Ok(s) = self.shared.lock() else {
            ui.label("共享状态不可用（锁中毒）");
            return;
        };

        // ── 探询节奏（只读；可调版后续票接线——pub const 已备好接电线）──
        ui.group(|ui| {
            ui.strong("探询节奏（当前只读，可调版由后续票接线）");
            ui.label(format!(
                "健康 {} 秒 / 队列 {} 秒 / 设备 {} 秒",
                HEALTH_INTERVAL.as_secs(),
                QUEUE_INTERVAL.as_secs(),
                DEVICES_INTERVAL.as_secs()
            ));
            ui.label(format!(
                "单次请求超时 {:.1} 秒 · 失联判定阈值 {} 秒",
                REQUEST_TIMEOUT.as_secs_f32(),
                STALE_AFTER.as_secs()
            ));
        });

        // ── WebUI（GUI 不承载业务：配置编辑/业务操作深链浏览器）──
        ui.group(|ui| {
            ui.strong("WebUI");
            ui.horizontal(|ui| {
                if ui.button("打开 WebUI").clicked() {
                    open_webui(self.port);
                }
                ui.weak(format!("http://127.0.0.1:{}", self.port));
            });
            ui.weak("业务操作面（模块/任务/配置编辑）：浏览器打开；GUI = 本机运维面");
        });

        // ── 托盘与版本 ──
        ui.group(|ui| {
            ui.strong("托盘与版本");
            ui.label(format!(
                "ep-tray v{}（管理台本体）",
                env!("CARGO_PKG_VERSION")
            ));
            match s.health.data.as_ref().filter(|_| s.health.is_fresh()) {
                Some(h) => {
                    ui.label(format!("daemon v{}（/api/health 实时）", h.version));
                }
                None => {
                    ui.label(format!("daemon 版本：{PROBE_PLACEHOLDER}"));
                }
            }
            match &s.cjk_font {
                Some(name) => {
                    ui.label(format!("中文字体：已加载 {name}"));
                }
                None => {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        "中文字体：未加载（msyh/simsun 均未找到），中文可能显示为方块",
                    );
                }
            }
            ui.weak("托盘图标三色随 daemon 状态联动（绿=运行 / 黄=过渡 / 红=故障）");
        });
    }
}

/// 日志页（GT-5）：工具条（WS 连接徽标 + 引擎/缓冲计数）+ 虚拟滚动表格
/// （表格与过滤渲染由本票后续 commit 接入；工具条数据全部来自 LogPipeHandle
/// 只读面——logic 帧已 drain，这里不触碰锁）。
impl TrayApp {
    fn ui_logs(&mut self, ui: &mut egui::Ui) {
        ui.heading("日志");
        let ws = self.log_handle.ws_status();
        let ring_len = self.log_handle.ring_len();
        let ring_dropped = self.log_handle.drop_count();
        let jsonl_bad = self.log_handle.jsonl_bad_lines();
        let buffer_len = self.logs.buffer.len();
        let buffer_dropped = self.logs.buffer.dropped();

        ui.horizontal(|ui| {
            // WS 连接徽标（GT-4 WsStatus）：绿=已连接 / 黄=重连中(第 N 次) /
            // 灰=未连接。引擎恒启用（三源三线程 spawn 时全起）——GT-4 状态机
            // 无「未启用」态，断线/未连上统一显示未连接，last_error 挂 tooltip。
            let (color, text) = match ws.state {
                WsConnState::Connected => (egui::Color32::from_rgb(80, 200, 120), "WS 已连接".to_owned()),
                WsConnState::Connecting => (
                    egui::Color32::YELLOW,
                    format!("WS 重连中（第 {} 次）", ws.attempts),
                ),
                WsConnState::Disconnected => (egui::Color32::GRAY, "WS 未连接".to_owned()),
            };
            ui.label(egui::RichText::new("●").color(color).size(14.0));
            let resp = ui.strong(&text);
            if let Some(err) = &ws.last_error {
                resp.on_hover_text(format!("最近错误: {err}"));
            }
            ui.separator();
            ui.weak(format!("引擎 {ring_len} 行"));
            if ring_dropped > 0 {
                let resp = ui.colored_label(
                    egui::Color32::YELLOW,
                    format!("已丢弃早期 {ring_dropped} 行"),
                );
                resp.on_hover_text("引擎环形缓冲（5000 行）超限丢最旧的累计数");
            }
            if jsonl_bad > 0 {
                let resp = ui.colored_label(
                    egui::Color32::YELLOW,
                    format!("events 坏行 {jsonl_bad}"),
                );
                resp.on_hover_text("events-*.jsonl 解析失败被跳过的行数");
            }
            ui.separator();
            ui.weak(format!("缓冲 {buffer_len} 行"));
            if buffer_dropped > 0 {
                let resp = ui.colored_label(
                    egui::Color32::from_rgb(255, 170, 90),
                    format!("缓冲淘汰 {buffer_dropped} 行"),
                );
                resp.on_hover_text(format!(
                    "显示缓冲（{LIMIT} 行）超限丢最旧的累计数——回看上限，不影响导出",
                    LIMIT = crate::platform::logs::DISPLAY_BUFFER_CAP
                ));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("导出…").clicked() {
                    self.export_popup = true;
                }
                if ui.button("复制页面摘要").clicked() {
                    let summary = format!(
                        "WS={ws_text:?} 引擎={ring_len}行 丢弃={ring_dropped} 缓冲={buffer_len}行",
                        ws_text = text
                    );
                    ui.ctx().copy_text(summary);
                }
            });
        });
        ui.separator();

        // 过滤器 + 跟随/暂停 + 虚拟滚动表格（logs.rs LogPage）
        self.logs.show(ui);
    }
}

/// 状态页（GT-2）：daemon 卡 / 设备卡 / 队列卡。
///
/// 数据全部来自 probe_ext 线程写入 [`UiShared`] 的探询快照；槽位 None 或
/// 超 [`STALE_AFTER`](probe_ext::STALE_AFTER) 新鲜度 → 统一占位文案。
/// 守护宕机判定沿 supervisor（顶栏三色圆点，`status_view` 复用）。
impl TrayApp {
    fn ui_status(&self, ui: &mut egui::Ui) {
        ui.heading("状态");
        let Ok(s) = self.shared.lock() else {
            ui.label("共享状态不可用（锁中毒）");
            return;
        };
        // 设备表格宽于最小窗宽（720px），窄窗下允许双向滚动不裁列
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {

        // ── daemon 卡：三色圆点 + 状态文字 + 版本 + 端口 + 数据新鲜度 ──
        ui.group(|ui| {
            ui.horizontal(|ui| {
                let (color, label) = self.status_view();
                let (r, g, b) = icons::status_rgb(color);
                ui.label(
                    egui::RichText::new("●")
                        .color(egui::Color32::from_rgb(r, g, b))
                        .size(16.0),
                );
                ui.strong(label);
                ui.separator();
                match s.health.data.as_ref().filter(|_| s.health.is_fresh()) {
                    Some(h) => {
                        ui.label(format!("daemon v{}", h.version));
                        ui.weak(format!("健康: {}", h.status));
                    }
                    None => {
                        ui.weak(format!("daemon 版本：{PROBE_PLACEHOLDER}"));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(format!("端口 {}", self.port));
                });
            });
            // 数据新鲜度行：最后成功轮询时刻；超 10s 标灰提示「探询失联」
            match (s.health.at, s.health.is_fresh()) {
                (Some(at), true) => {
                    ui.weak(format!(
                        "探询正常（2s 档）· 最后成功 {} 秒前",
                        at.elapsed().as_secs()
                    ));
                }
                (Some(_), false) => {
                    ui.colored_label(
                        egui::Color32::GRAY,
                        "探询失联：超过 10s 无成功响应（daemon 宕机或拒绝连接）",
                    );
                }
                (None, _) => {
                    ui.weak("探询未就绪：尚未收到任何成功响应");
                }
            }
            // ── 运行模式行（GT-3 徽章，scm_watch 30s 复检）──
            let mode_line = {
                let mode = s.mode;
                match s.service.data.as_ref() {
                    Some(snap) if mode == RunMode::Service => {
                        let state = snap
                            .status
                            .map(|st| st.label())
                            .unwrap_or("状态未知");
                        let recheck = s
                            .service
                            .at
                            .map(|at| format!("，{} 秒前复检", at.elapsed().as_secs()))
                            .unwrap_or_default();
                        format!(
                            "运行模式：Windows 服务（{SERVICE_NAME}）· {state} · 启动类型 {}{recheck}",
                            snap.start.label()
                        )
                    }
                    Some(_) | None if mode == RunMode::Service => {
                        "运行模式：Windows 服务（服务快照待复检）".to_owned()
                    }
                    _ => "运行模式：sidecar（ep-tray 托管；Windows 服务未安装）".to_owned(),
                }
            };
            ui.weak(mode_line);
        });

        // ── 设备卡：/api/devices 表格（一条 = 物理归并后的设备）──
        ui.group(|ui| {
            ui.strong("设备（10s 档）");
            let devs = s.devices.data.as_ref().filter(|_| s.devices.is_fresh());
            match devs {
                Some(devs) if devs.is_empty() => {
                    ui.weak("（daemon 未报告任何计算设备）");
                }
                Some(devs) => {
                    egui::Grid::new("devices_grid")
                        .striped(true)
                        .num_columns(7)
                        .spacing([16.0, 4.0])
                        .show(ui, |ui| {
                            for head in ["名称", "后端", "计算栈", "显存", "已用", "利用率", "温度"]
                            {
                                ui.strong(head);
                            }
                            ui.end_row();
                            for d in devs {
                                ui.label(&d.name);
                                ui.label(&d.backend);
                                ui.label(d.stacks.join("/"));
                                ui.label(mb_to_gb(d.total_memory_mb));
                                ui.label(mb_to_gb(d.used_memory_mb));
                                ui.label(metric(d.utilization, "%"));
                                ui.label(metric(d.temperature, "°C"));
                                ui.end_row();
                            }
                        });
                }
                None => {
                    ui.label(PROBE_PLACEHOLDER);
                }
            }
        });

        // ── 队列卡：paused/running/queued 三个大数字 + 暂停状态徽标 ──
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.strong("队列（5s 档）");
                let paused_badged = s
                    .queue
                    .data
                    .as_ref()
                    .filter(|_| s.queue.is_fresh())
                    .map(|q| q.paused);
                if let Some(paused) = paused_badged {
                    if paused {
                        ui.label(
                            egui::RichText::new("已暂停派发")
                                .color(egui::Color32::YELLOW)
                                .background_color(egui::Color32::from_rgb(70, 60, 20)),
                        );
                    } else {
                        ui.label(
                            egui::RichText::new("派发中")
                                .color(egui::Color32::from_rgb(80, 200, 120))
                                .background_color(egui::Color32::from_rgb(20, 60, 40)),
                        );
                    }
                }
            });
            match s.queue.data.as_ref().filter(|_| s.queue.is_fresh()) {
                Some(q) => {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        big_stat(
                            ui,
                            "派发暂停",
                            if q.paused { "是" } else { "否" },
                            if q.paused {
                                egui::Color32::YELLOW
                            } else {
                                egui::Color32::from_rgb(80, 200, 120)
                            },
                        );
                        ui.separator();
                        big_stat(
                            ui,
                            "运行中",
                            &q.running.to_string(),
                            ui.style().visuals.strong_text_color(),
                        );
                        ui.separator();
                        big_stat(
                            ui,
                            "排队中",
                            &q.queued.to_string(),
                            ui.style().visuals.strong_text_color(),
                        );
                    });
                    if q.paused {
                        ui.weak("派发已暂停：排队任务保留，运行中任务照常完成");
                    }
                }
                None => {
                    ui.label(PROBE_PLACEHOLDER);
                }
            }
        });
        });
    }
}

/// 管控页（GT-3）：双模式分流——Service 模式走 SCM 提权动作，sidecar 模式
/// 沿用 supervisor 命令（cmd_tx）与 HKCU Run 自启；服务未安装时给安装指引
/// 文字（**不自动装/卸——红线**）。写操作反馈经 [`UiShared::control_op`]。
impl TrayApp {
    fn ui_control(&self, ui: &mut egui::Ui) {
        ui.heading("管控");
        let Ok(s) = self.shared.lock() else {
            ui.label("共享状态不可用（锁中毒）");
            return;
        };
        let mode = s.mode;
        let snap = s.service.data.clone();
        let control_op = s.control_op.clone();
        drop(s); // 释放共享锁再进子视图（子视图 launch_service_op 会重新加锁）

        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            // 当前模式行
            let (badge_text, fg, tip) = mode_badge_view(mode);
            ui.horizontal(|ui| {
                ui.strong("当前模式：");
                let badge = ui.label(
                    egui::RichText::new(badge_text)
                        .color(fg)
                        .background_color(egui::Color32::from_rgb(40, 44, 52)),
                );
                badge.on_hover_text(tip);
                if let Some(snap) = &snap {
                    if mode == RunMode::Service {
                        ui.weak(format!(
                            "服务 {} · PID {}",
                            snap.status.map(|st| st.label()).unwrap_or("状态未知"),
                            snap.pid.map(|p| p.to_string()).unwrap_or_else(|| "—".into())
                        ));
                    }
                }
            });

            // 提权操作反馈行（两模式共用：sidecar 的安装指引复制无 UAC，
            // 但 StartType/Recovery/启停都走这里）
            if let Some(op) = &control_op {
                let (text, color) = op_feedback_view(op);
                ui.label(egui::RichText::new(text).color(color));
            }
            ui.separator();

            match mode {
                RunMode::Service => self.ui_control_service(ui, snap.as_ref()),
                RunMode::Sidecar => self.ui_control_sidecar(ui, snap.as_ref()),
            }
        });
    }

    /// Service 模式：启动/停止/重启（提权）+ 启动类型单选 + 崩溃自动重启开关。
    fn ui_control_service(&self, ui: &mut egui::Ui, snap: Option<&ServiceSnap>) {
        let pending = self.op_pending();

        // ── 服务操作 ──
        ui.group(|ui| {
            ui.strong("服务操作（EntryPoint，写操作经 UAC 提权一次性子进程执行）");
            let (can_start, can_stop, can_restart) =
                service_control_model(pending, snap.and_then(|x| x.status));
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(can_start, egui::Button::new("启动"))
                    .clicked()
                {
                    self.launch_service_op(ScAction::Start);
                }
                if ui
                    .add_enabled(can_stop, egui::Button::new("停止"))
                    .clicked()
                {
                    self.launch_service_op(ScAction::Stop);
                }
                if ui
                    .add_enabled(can_restart, egui::Button::new("重启"))
                    .clicked()
                {
                    self.launch_service_op(ScAction::Restart);
                }
            });
            ui.weak("每次操作由 UAC 现场批准；主进程绝不持有管理员权限");
            let snap_fresh = self
                .shared
                .lock()
                .ok()
                .and_then(|s| s.service.at)
                .map(|at| format!("（{} 秒前复检）", at.elapsed().as_secs()))
                .unwrap_or_default();
            ui.weak(format!(
                "服务状态每 {} 秒自动复检{snap_fresh}",
                crate::platform::scm::SCM_RECHECK_INTERVAL.as_secs()
            ));
        });

        // ── 自启方式（启动类型）──
        ui.group(|ui| {
            ui.strong("自启方式（服务启动类型，等价 systemctl enable/disable）");
            let current = snap.map(|x| x.start).unwrap_or(StartKind::Other);
            let snapshot_sel = current.radio_index();
            let mut sel = snapshot_sel;
            ui.add_enabled_ui(!pending, |ui| {
                ui.radio_value(&mut sel, Some(0usize), "自动");
                ui.radio_value(&mut sel, Some(1usize), "自动（延迟启动）");
                ui.radio_value(&mut sel, Some(2usize), "手动");
            });
            if current == StartKind::Disabled || current == StartKind::Other {
                ui.weak(format!(
                    "当前启动类型为「{}」，不可经本页修改；选择任一档位后应用即可覆盖",
                    current.label()
                ));
            }
            let may_apply = !pending && sel.is_some() && sel != snapshot_sel;
            if ui
                .add_enabled(may_apply, egui::Button::new("应用启动类型"))
                .clicked()
            {
                let st = match sel {
                    Some(1) => StartType::DelayedAuto,
                    Some(2) => StartType::Manual,
                    _ => StartType::Auto,
                };
                self.launch_service_op(ScAction::SetStartType(st));
            }
            ui.weak("改动经 UAC 提权写入 SCM，生效后可用 sc qc EntryPoint 核对");
        });

        // ── 崩溃自动重启（SCM Recovery）──
        ui.group(|ui| {
            ui.strong("崩溃自动重启（等价 systemd Restart=on-failure）");
            let recovery = snap.and_then(|x| x.recovery_restart);
            let mut on = recovery.unwrap_or(false);
            let resp = ui.add_enabled(
                !pending && recovery.is_some(),
                egui::Checkbox::new(
                    &mut on,
                    "服务进程崩溃时由 SCM 自动拉起（10s/30s/60s 三次，24h 计数窗）",
                ),
            );
            if resp.changed() {
                self.launch_service_op(ScAction::SetRecovery(on));
            }
            if recovery.is_none() {
                ui.weak("恢复动作状态未知（SCM 读取失败），开关暂不可用——等下一轮复检");
            }
            ui.weak("改动经 UAC 提权写入 SCM，生效后可用 sc qfailure EntryPoint 核对");
        });
    }

    /// sidecar 模式：supervisor 命令照旧 + HKCU Run 自启 + 服务安装指引。
    fn ui_control_sidecar(&self, ui: &mut egui::Ui, _snap: Option<&ServiceSnap>) {
        // ── daemon 进程（sidecar 托管，supervisor 状态机照旧——回归红线）──
        ui.group(|ui| {
            ui.strong("daemon 进程（sidecar：ep-tray 以子进程方式托管）");
            ui.weak("崩溃自动重启一次，再失败托盘红标（supervisor 状态机）");
            ui.horizontal(|ui| {
                if ui.button("重启 daemon").clicked() {
                    let _ = self.cmd_tx.send(DaemonCommand::Restart);
                }
                if ui.button("停止 daemon").clicked() {
                    let _ = self.cmd_tx.send(DaemonCommand::Stop);
                }
            });
        });

        // ── 自启（HKCU Run，与托盘菜单「开机自启」同一事实源）──
        ui.group(|ui| {
            ui.strong("开机自启（当前用户登录时）");
            let mut on = self.autostart_item.is_checked();
            if ui
                .checkbox(&mut on, "登录时静默启动托盘（HKCU Run，不开窗）")
                .changed()
            {
                let result = std::env::current_exe()
                    .map_err(|e| format!("定位 exe: {e}"))
                    .and_then(|exe| autostart::set_autostart(&HkuRunKey, &exe, on));
                match result {
                    Ok(()) => self.autostart_item.set_checked(on),
                    Err(e) => {
                        eprintln!("ep-tray: 自启开关写入失败: {e}");
                        self.autostart_item.set_checked(!on); // 回滚 UI
                    }
                }
            }
            ui.weak("与托盘菜单「开机自启」为同一注册表项");
        });

        // ── Windows 服务安装指引（不自动装/卸——红线）──
        ui.group(|ui| {
            ui.strong("Windows 服务（可选，未安装）");
            ui.weak("如需系统级服务（登录前自启、独立于登录会话、SCM 崩溃恢复）：");
            let cmd = install_command(
                std::env::current_exe().ok().as_deref().and_then(Path::parent),
            );
            ui.monospace(&cmd);
            ui.horizontal(|ui| {
                if ui.button("复制安装命令").clicked() {
                    ui.ctx().copy_text(cmd.clone());
                }
                ui.weak("以管理员 PowerShell 运行");
            });
            ui.weak("本 GUI 不自动安装/卸载服务；安装后重启 ep-tray 即识别为服务模式");
        });
    }

    /// 是否有提权动作进行中（Pending 期间所有提权入口禁用——防双击双进程）。
    fn op_pending(&self) -> bool {
        self.shared
            .lock()
            .ok()
            .and_then(|s| s.control_op.clone())
            .is_some_and(|op| op.state == ControlOpState::Pending)
    }

    /// 发起提权动作：原子置 Pending（同一主线程内检查+置位，无竞态）→
    /// 后台线程 ShellExecuteExW(runas) 拉起自身 --sc-action → 退出码映射
    /// 终态 → 即时复检 SCM 快照。主进程侧零提权（红线）。
    fn launch_service_op(&self, op: ScAction) {
        {
            let mut s = match self.shared.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            if matches!(&s.control_op, Some(c) if c.state == ControlOpState::Pending) {
                return; // 已有动作进行中（等待 UAC/执行），忽略重复触发
            }
            s.control_op = Some(ControlOp {
                label: op.label().to_owned(),
                state: ControlOpState::Pending,
                at: Instant::now(),
            });
        }
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name("ep-tray-sc-action".into())
            .spawn(move || {
                let args = elevate::sc_action_args(op);
                let state = match elevate::run_elevated_self(&args) {
                    Ok(code) if code == EXIT_OK => ControlOpState::Ok,
                    Ok(code) if code == EXIT_DENIED => ControlOpState::Denied,
                    Ok(code) => ControlOpState::Failed(format!("退出码 {code}")),
                    Err(ElevateError::Cancelled) => ControlOpState::Denied,
                    Err(ElevateError::Os(code)) => {
                        ControlOpState::Failed(format!("提权启动失败（Windows 错误 {code}）"))
                    }
                };
                if let Ok(mut s) = shared.lock() {
                    s.control_op = Some(ControlOp {
                        label: op.label().to_owned(),
                        state,
                        at: Instant::now(),
                    });
                    // 即时复检（读路径无 UAC）：不等 30s 复检线程，界面先到位；
                    // 服务消失（被卸载）时模式同步回落 sidecar
                    match query_service(SERVICE_NAME) {
                        Ok(snap) => {
                            s.service.record_ok(snap);
                            s.mode = RunMode::Service;
                        }
                        Err(ScmError::NotFound) => {
                            s.service.record_fail();
                            s.mode = RunMode::Sidecar;
                        }
                        Err(_) => s.service.record_fail(),
                    }
                }
            });
        if let Err(e) = spawned {
            // 线程起不来（极罕见）：撤销 Pending，否则全部按钮永久禁用
            if let Ok(mut s) = self.shared.lock() {
                s.control_op = Some(ControlOp {
                    label: op.label().to_owned(),
                    state: ControlOpState::Failed(format!("动作线程启动失败: {e}")),
                    at: Instant::now(),
                });
            }
        }
    }
}

/// Service 模式按钮可用性模型（纯函数，可单测）：
/// Pending（UAC 等待/执行中）全部禁用；按当前状态给 启动/停止/重启。
pub(crate) fn service_control_model(
    pending: bool,
    status: Option<ServiceState>,
) -> (bool, bool, bool) {
    if pending {
        return (false, false, false);
    }
    match status {
        Some(ServiceState::Running) => (false, true, true),
        Some(ServiceState::StartPending) => (false, true, false),
        Some(ServiceState::StopPending) | Some(ServiceState::Paused) | None => (false, false, false),
        Some(ServiceState::Stopped) => (true, false, false),
    }
}

/// 提权操作反馈行（纯函数）：返回 (文案, 颜色)。
pub(crate) fn op_feedback_view(op: &ControlOp) -> (String, egui::Color32) {
    match &op.state {
        ControlOpState::Pending => (
            format!("{}：执行中…（等待 UAC 授权，请在弹窗中批准或取消）", op.label),
            egui::Color32::YELLOW,
        ),
        ControlOpState::Ok => (
            format!("上次操作成功：{}", op.label),
            egui::Color32::from_rgb(80, 200, 120),
        ),
        ControlOpState::Denied => (
            format!(
                "上次操作未获授权：{}（UAC 被拒绝）——该操作需要管理员权限，可重试",
                op.label
            ),
            egui::Color32::YELLOW,
        ),
        ControlOpState::Failed(e) => (
            format!("上次操作失败：{}（{e}）", op.label),
            egui::Color32::from_rgb(235, 90, 90),
        ),
    }
}

/// 服务安装指引命令（纯函数）：BinDir = ep-tray.exe 所在目录
///（与 ep-daemon.exe 同目录的 sidecar 约定 = install-service.ps1 的 -BinDir）。
pub(crate) fn install_command(exe_dir: Option<&Path>) -> String {
    const BASE: &str = "powershell -NoProfile -ExecutionPolicy Bypass -File scripts\\install-service.ps1 -BinDir";
    match exe_dir {
        Some(dir) => {
            let d = dir.display().to_string();
            if d.contains(' ') {
                format!("{BASE} \"{d}\"")
            } else {
                format!("{BASE} {d}")
            }
        }
        None => format!("{BASE} <部署目录\\bin>"),
    }
}

/// 队列卡大数字块（说明行 + 放大数值）。
fn big_stat(ui: &mut egui::Ui, caption: &str, value: &str, color: egui::Color32) {
    ui.vertical(|ui| {
        ui.weak(caption);
        ui.label(egui::RichText::new(value).size(26.0).color(color).strong());
    });
}

/// 显存 MB → GB 显示（None → 占位破折号）。
fn mb_to_gb(mb: Option<u32>) -> String {
    match mb {
        Some(mb) => format!("{:.1} GB", f64::from(mb) / 1024.0),
        None => "—".into(),
    }
}

/// 利用率/温度等小指标显示（None → 占位破折号）。
fn metric(v: Option<u8>, unit: &str) -> String {
    match v {
        Some(v) => format!("{v}{unit}"),
        None => "—".into(),
    }
}

/// 菜单句柄束（app.rs 构建菜单，句柄交 TrayApp 接管更新权）。
pub(crate) struct TrayMenuHandles {
    pub status_item: MenuItem,
    pub open_item: MenuItem,
    pub console_item: MenuItem,
    pub restart_item: MenuItem,
    pub stop_item: MenuItem,
    pub autostart_item: CheckMenuItem,
    pub quit_item: MenuItem,
}

/// 构建托盘实体（黄色初始图标 + 菜单；须在事件循环运行后调用）。
pub(crate) fn build_tray(menu: &Menu) -> Result<TrayIcon, String> {
    let icon = icon_for(TrayColor::Yellow)?;
    TrayIconBuilder::new()
        .with_menu(Box::new(menu.clone()))
        .with_tooltip("EntryPoint")
        .with_icon(icon)
        .with_menu_on_left_click(false)
        .build()
        .map_err(|e| format!("托盘创建失败: {e}"))
}

pub(crate) fn icon_for(color: TrayColor) -> Result<Icon, String> {
    let (rgba, w, h) = icons::status_icon_rgba(color);
    Icon::from_rgba(rgba, w, h).map_err(|e| format!("图标生成失败: {e}"))
}

/// 用系统默认浏览器打开 WebUI（cmd start，空标题占位避免 URL 被当窗口标题）。
pub(crate) fn open_webui(port: u16) {
    let url = format!("http://127.0.0.1:{port}");
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", &url])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

/// CJK 字体运行时加载：微软雅黑（系统必有）→ 失败降级 egui 默认字体。
/// 不内嵌字体文件（避免 +10MB 产物体积）；全部失败只 eprintln，不阻断启动。
/// 返回值 = 实际加载的字体文件名（None = 全部候选失败，供设置页展示）。
fn load_cjk_fonts(ctx: &egui::Context) -> Option<&'static str> {
    const CANDIDATES: &[&str] = &[
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
    ];
    for path in CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts
                .font_data
                .insert("cjk".to_owned(), egui::FontData::from_owned(bytes).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                if let Some(list) = fonts.families.get_mut(&family) {
                    list.insert(0, "cjk".to_owned());
                }
            }
            ctx.set_fonts(fonts);
            return Some(file_name(path));
        }
    }
    eprintln!("ep-tray: 未找到系统中文字体（msyh/simsun），中文可能显示为方块");
    None
}

/// 取路径末段文件名（字体候选路径均为 `\` 分隔的常量，无需 PathBuf）。
fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

// ─── 单测（纯函数：模式徽章视图；页面渲染走实跑截图留证，见 GT-3 报告） ────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::scm::SCM_RECHECK_INTERVAL;

    #[test]
    fn mode_badge_covers_both_modes() {
        let (text, fg, tip) = mode_badge_view(RunMode::Service);
        assert_eq!(text, "服务模式");
        assert_ne!(fg, egui::Color32::GRAY, "Service 徽章应与 sidecar 灰色区分");
        assert!(tip.contains("UAC"), "Service tooltip 应说明提权语义");

        let (text, fg, tip) = mode_badge_view(RunMode::Sidecar);
        assert_eq!(text, "sidecar 模式");
        assert_eq!(fg, egui::Color32::GRAY);
        assert!(tip.contains("sidecar"));
    }

    #[test]
    fn uishared_defaults_to_sidecar_mode() {
        let s = UiShared::default();
        assert_eq!(s.mode, RunMode::Sidecar, "缺省 sidecar（服务未检测时不误标）");
        assert!(s.service.data.is_none(), "快照槽初始为空");
    }

    #[test]
    fn scm_recheck_interval_is_30s() {
        assert_eq!(SCM_RECHECK_INTERVAL.as_secs(), 30, "派发单口径：30s 复检");
    }

    // ── 管控页纯逻辑 ──

    #[test]
    fn service_buttons_follow_state() {
        use crate::platform::scm::ServiceState as St;
        // 运行中：可停可重启，不可再启动
        assert_eq!(service_control_model(false, Some(St::Running)), (false, true, true));
        // 已停止：只可启动
        assert_eq!(service_control_model(false, Some(St::Stopped)), (true, false, false));
        // 启动中：可停（SCM 允许打断启动），不可重启
        assert_eq!(service_control_model(false, Some(St::StartPending)), (false, true, false));
        // 停止中/暂停/状态未知：全禁用（保守）
        assert_eq!(service_control_model(false, Some(St::StopPending)), (false, false, false));
        assert_eq!(service_control_model(false, Some(St::Paused)), (false, false, false));
        assert_eq!(service_control_model(false, None), (false, false, false));
    }

    #[test]
    fn pending_gates_all_service_buttons() {
        use crate::platform::scm::ServiceState as St;
        assert_eq!(service_control_model(true, Some(St::Running)), (false, false, false));
        assert_eq!(service_control_model(true, Some(St::Stopped)), (false, false, false));
    }

    #[test]
    fn op_feedback_covers_all_states() {
        let op = |state| ControlOp {
            label: "启动服务".into(),
            state,
            at: Instant::now(),
        };
        let (t, c) = op_feedback_view(&op(ControlOpState::Pending));
        assert!(t.contains("执行中") && t.contains("UAC"), "{t}");
        assert_eq!(c, egui::Color32::YELLOW);
        let (t, _) = op_feedback_view(&op(ControlOpState::Ok));
        assert!(t.contains("成功"), "{t}");
        let (t, _) = op_feedback_view(&op(ControlOpState::Denied));
        assert!(t.contains("未获授权"), "{t}");
        let (t, c) = op_feedback_view(&op(ControlOpState::Failed("Windows 错误 1053".into())));
        assert!(t.contains("1053"), "{t}");
        assert_eq!(c, egui::Color32::from_rgb(235, 90, 90));
    }

    #[test]
    fn install_command_uses_sidecar_dir_and_quotes_spaces() {
        // exe_dir 可得 → 作为 -BinDir（sidecar 约定：与 ep-daemon.exe 同目录）
        let cmd = install_command(Some(Path::new(r"D:\ep\bin")));
        assert!(cmd.contains("install-service.ps1"));
        assert!(cmd.contains(r"-BinDir D:\ep\bin"), "{cmd}");
        assert!(cmd.contains("-ExecutionPolicy Bypass"), "{cmd}");
        // 含空格路径必须加引号
        let cmd = install_command(Some(Path::new(r"D:\Program Files\ep\bin")));
        assert!(cmd.contains(r#""D:\Program Files\ep\bin""#), "{cmd}");
        // exe 不可得 → 占位提示（不臆造路径）
        let cmd = install_command(None);
        assert!(cmd.contains("<部署目录"), "{cmd}");
    }
}
