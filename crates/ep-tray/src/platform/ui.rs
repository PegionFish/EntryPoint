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
        load_cjk_fonts(&cc.egui_ctx);
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
            Page::Status => {
                ui.heading("状态");
                ui.label(self.last_label.clone().unwrap_or_else(|| "初始化…".into()));
                ui.separator();
                ui.label("设备 / 池 / 队列面板：GT-2 落地（消费 /api/devices、/api/queue）");
            }
            Page::Control => {
                ui.heading("管控");
                ui.label("daemon 启停/重启、自启与模式配置：GT-3 落地（双模式 sidecar ∥ Windows Service）");
            }
            Page::Logs => {
                ui.heading("日志");
                ui.label("实时日志（/ws/logs + daemon.log + events）与导出：GT-5/GT-6 落地");
            }
            Page::Settings => {
                ui.heading("设置");
                ui.label("root 覆盖 / 轮询间隔 / 缓冲上限：GT-2+ 落地；配置编辑深链 WebUI");
            }
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
fn load_cjk_fonts(ctx: &egui::Context) {
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
            return;
        }
    }
    eprintln!("ep-tray: 未找到系统中文字体（msyh/simsun），中文可能显示为方块");
}
