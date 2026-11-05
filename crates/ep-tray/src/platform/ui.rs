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
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::autostart;
use crate::icons;
use crate::platform::daemon::{DaemonCommand, StateUpdate};
use crate::platform::probe_ext::{
    DeviceSnapshot, HealthSnapshot, ProbeSlot, QueueSnapshot, DEVICES_INTERVAL, HEALTH_INTERVAL,
    QUEUE_INTERVAL, REQUEST_TIMEOUT, STALE_AFTER,
};
use crate::platform::registry::HkuRunKey;
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
        menu: Menu,
        handles: TrayMenuHandles,
        pending_open: bool,
    ) -> Self {
        let cjk_font_loaded = load_cjk_fonts(&cc.egui_ctx);
        // 字体加载结果入共享面：GT-2 设置页展示加载状态（失败降级可见化）
        if let Ok(mut s) = shared.lock() {
            s.cjk_font = cjk_font_loaded.map(str::to_owned);
        }
        // 字体加载结果入共享面：GT-2 设置页展示加载状态（失败降级可见化）
        if let Ok(mut s) = shared.lock() {
            s.cjk_font = cjk_font_loaded.map(str::to_owned);
        }
        Self {
            port,
            cmd_tx,
            shared,
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
            selected: Page::Status,
            last_label: None,
            pending_open,
            pending_show: false,
            really_quit: false,
            shutdown_sent: false,
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
    }

    /// 主窗口渲染（0.37 新签名：裸 &mut Ui，无 margin——CentralPanel 兜底）。
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
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
            Page::Control => {
                ui.heading("管控");
                ui.label("daemon 启停/重启、自启与模式配置：GT-3 落地（双模式 sidecar ∥ Windows Service）");
            }
            Page::Logs => {
                ui.heading("日志");
                ui.label("实时日志（/ws/logs + daemon.log + events）与导出：GT-5/GT-6 落地");
            }
            Page::Settings => self.ui_settings(ui),
        });
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
