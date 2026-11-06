//! 托盘 + 主窗口壳（Windows）：eframe/egui 事件循环 + tray-icon 共存接线。
//!
//! 红线：**壳不承载业务功能**——所有业务能力经 WebUI（浏览器打开
//! `http://127.0.0.1:<port>`）与后续 GT-2+ 的 GUI 页面。本模块只做组装：
//! 单实例、sidecar 托管线程、托盘菜单构建、事件路由，窗口本体在
//! [`super::ui::TrayApp`]。
//!
//! 换血记录（GT-1，2026-11-05）：tao 事件循环退役，改 eframe（winit 底座）。
//! tray-icon 0.26 与 winit 共存方式 = `MenuEvent`/`TrayIconEvent`
//! `set_event_handler` 推入共享队列（[`super::ui::UiShared`]），egui 每帧
//! drain；TrayIcon 实体延迟到首个 update 帧创建（tray-icon#90）。

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::TrayIconEvent;

use crate::autostart;
use crate::platform::daemon::{run_supervisor_thread, DaemonCommand, DaemonSidecar, StateUpdate};
use crate::platform::logpipe::{file_tail::TailStart, LogPipe, LogPipeConfig, LOG_RING_CAP};
use crate::platform::logs::resolve_log_root;
use crate::platform::mutex::{NamedMutex, TRAY_MUTEX_NAME};
use crate::platform::registry::HkuRunKey;
use crate::platform::ui::{TrayApp, TrayMenuHandles, UiShared};
use crate::singleton::{acquire_single_instance, SingletonOutcome};

/// daemon 缺省端口（与 ep-core `config::default_port` 同源约定；`--port` 可覆盖）
pub const DEFAULT_PORT: u16 = 9800;

/// ep-tray 主入口（Windows）：参数解析 → 单实例 → 托管线程 → eframe 主循环。
///
/// 参数：
/// - `--minimized`：静默启动（开机自启 Run 键使用；只托盘不开窗）
/// - `--port=<n>` / `--port <n>`：daemon 端口（缺省 9800）
/// - `--help` / `-h`：用法
pub fn run() -> Result<(), String> {
    let (minimized, port) = parse_args()?;

    // ── 单实例：已有实例在运行 → 静默退出（退出码 0，供自启场景复用）──
    let mutex = NamedMutex::acquire(TRAY_MUTEX_NAME)?;
    match acquire_single_instance(&mutex) {
        SingletonOutcome::Acquired => {}
        SingletonOutcome::AlreadyRunning => {
            eprintln!("ep-tray: 已有实例在运行，本实例退出");
            return Ok(());
        }
        SingletonOutcome::Error(e) => {
            return Err(format!("单实例检测失败（保守不启动）: {e}"));
        }
    }

    // ── daemon sidecar：与 ep-tray.exe 同目录的 ep-daemon.exe ──
    let exe_dir: PathBuf = std::env::current_exe()
        .map_err(|e| format!("定位 ep-tray.exe 失败: {e}"))?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "ep-tray.exe 无父目录".to_string())?;
    let daemon_path = exe_dir.join("ep-daemon.exe");
    if !daemon_path.is_file() {
        return Err(format!(
            "未找到 daemon sidecar: {}（ep-daemon.exe 须与 ep-tray.exe 同目录）",
            daemon_path.display()
        ));
    }

    // ── 托管线程（spawn/探活/崩溃重启的真实执行者）──
    let (cmd_tx, cmd_rx) = mpsc::channel::<DaemonCommand>();
    let (state_tx, state_rx) = mpsc::channel::<StateUpdate>();
    let supervisor_handle = std::thread::Builder::new()
        .name("ep-tray-supervisor".into())
        .spawn(move || {
            run_supervisor_thread(DaemonSidecar { daemon_path, port }, cmd_rx, state_tx);
        })
        .map_err(|e| format!("托管线程启动失败: {e}"))?;

    // ── 开机自启初始勾选态（读真实注册表；失败按关处理，不阻断启动）──
    let autostart_on = autostart::is_autostart_enabled(&HkuRunKey).unwrap_or(false);

    // ── 托盘菜单（实体先建，TrayIcon 延迟到首个 update 帧——tray-icon#90）──
    let status_item = MenuItem::new("● EntryPoint：启动中…", false, None);
    let open_item = MenuItem::new("打开 WebUI", true, None);
    let console_item = MenuItem::new("打开管理台", true, None);
    let restart_item = MenuItem::new("重启 daemon", true, None);
    let stop_item = MenuItem::new("停止 daemon", true, None);
    let autostart_item = CheckMenuItem::new("开机自启", true, autostart_on, None);
    let quit_item = MenuItem::new("退出（并停止 daemon）", true, None);

    let menu = Menu::new();
    menu.append_items(&[
            &status_item,
            &PredefinedMenuItem::separator(),
            &open_item,
            &console_item,
            &restart_item,
            &stop_item,
            &autostart_item,
            &PredefinedMenuItem::separator(),
            &quit_item,
        ])
        .map_err(|e| format!("构建托盘菜单失败: {e}"))?;

    // ── 共享队列 + 事件路由（tray-icon → egui）──
    let shared: Arc<Mutex<UiShared>> = Arc::new(Mutex::new(UiShared::default()));

    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("EntryPoint 管理台")
            .with_inner_size([900.0, 600.0])
            .with_min_inner_size([720.0, 480.0])
            .with_visible(!minimized), // --minimized：静默启动，只托盘不开窗
        ..Default::default()
    };

    // 手动启动（无 --minimized）：daemon 首次就绪后自动打开 WebUI 一次
    let pending_open = !minimized;

    // ── GT-5 LogPipe root 解析（daemon 三级口径：EP_ROOT > 标记目录 > cwd；
    //    启动期一次性解析，无每帧 IO）──
    let log_root = resolve_log_root(
        &exe_dir,
        std::env::var_os("EP_ROOT").map(PathBuf::from),
        std::env::current_dir().ok(),
    );

    let run_result = eframe::run_native(
        "EntryPoint 管理台",
        native_options,
        Box::new({
            let shared = Arc::clone(&shared);
            let state_rx = state_rx;
            move |cc: &eframe::CreationContext<'_>| {
                let ctx = cc.egui_ctx.clone();
                // 托管线程状态 → 共享队列 + 即时唤醒
                {
                    let shared = Arc::clone(&shared);
                    let ctx = ctx.clone();
                    std::thread::Builder::new()
                        .name("ep-tray-state-fwd".into())
                        .spawn(move || {
                            while let Ok(update) = state_rx.recv() {
                                if shared.lock().map(|mut s| s.state = Some(update)).is_err() {
                                    break;
                                }
                                ctx.request_repaint();
                            }
                        })
                        .map_err(|e| format!("状态转发线程启动失败: {e}"))?;
                }
                // GT-2 状态页探询线程：health/queue/devices 三档轮询写 UiShared
                // （单线程 tick 循环；节奏/超时见 probe_ext.rs pub const；
                //   生命周期 = 进程生命周期，eframe 退出即随进程终止）
                {
                    let shared = Arc::clone(&shared);
                    let ctx = ctx.clone();
                    std::thread::Builder::new()
                        .name("ep-tray-probe".into())
                        .spawn(move || {
                            crate::platform::probe_ext::run_probe_thread(port, shared, ctx);
                        })
                        .map_err(|e| format!("探询线程启动失败: {e}"))?;
                }
                // GT-3 模式检测线程：SCM query(EntryPoint) 启动即查 + 30s 复检
                // （读路径标准用户可用无 UAC；写 UiShared.mode/service——
                //   顶部/状态页模式徽章与管控页的数据源）
                {
                    let shared = Arc::clone(&shared);
                    let ctx = ctx.clone();
                    std::thread::Builder::new()
                        .name("ep-tray-scm-watch".into())
                        .spawn(move || {
                            crate::platform::scm::run_scm_watch(shared, ctx);
                        })
                        .map_err(|e| format!("模式检测线程启动失败: {e}"))?;
                }
                // 托盘菜单事件 → 共享队列 + 即时唤醒（回调在主线程消息泵上）
                {
                    let shared = Arc::clone(&shared);
                    let ctx = ctx.clone();
                    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                        if let Ok(mut s) = shared.lock() {
                            s.menu_events.push(event);
                        }
                        ctx.request_repaint();
                    }));
                }
                {
                    let shared = Arc::clone(&shared);
                    let ctx = ctx.clone();
                    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                        if let Ok(mut s) = shared.lock() {
                            s.tray_events.push(event);
                        }
                        ctx.request_repaint();
                    }));
                }
                // ── GT-5：日志引擎（三源三线程，IO 全后台；Handle 交 TrayApp，
                //    logic 帧 drain——「后台线程写共享 + logic drain」沿 probe_ext 先例）──
                // tail_start=Beginning：日志页打开即回看 daemon.log 历史
                // （GT-4 冒烟同语义；超大历史文件由 ring 背压丢最旧兜底，dropped 计数可见）
                let log_handle = LogPipe::spawn(LogPipeConfig {
                    root: log_root,
                    daemon_port: port,
                    ring_cap: LOG_RING_CAP,
                    tail_start: TailStart::Beginning,
                });
                Ok(Box::new(TrayApp::new(
                    cc,
                    port,
                    cmd_tx,
                    shared,
                    log_handle,
                    menu,
                    TrayMenuHandles {
                        status_item,
                        open_item,
                        console_item,
                        restart_item,
                        stop_item,
                        autostart_item,
                        quit_item,
                    },
                    pending_open,
                )))
            }
        }),
    );

    // 事件循环结束：确保托管线程停掉 daemon 再收尸
    let _ = supervisor_handle.join();
    run_result.map_err(|e| format!("GUI 事件循环异常退出: {e}"))
}

/// 参数解析（纯解析，无副作用；错误信息面向终端用户）。
fn parse_args() -> Result<(bool, u16), String> {
    let mut minimized = false;
    let mut port = DEFAULT_PORT;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--minimized" => minimized = true,
            "--help" | "-h" => {
                println!(
                    "ep-tray — EntryPoint 管理台（托盘 + GUI）\n\
                     用法: ep-tray [--minimized] [--port=<n>]\n\
                     \x20 --minimized   静默启动（开机自启使用，只托盘不开窗）\n\
                     \x20 --port=<n>    daemon 端口（缺省 {DEFAULT_PORT}）"
                );
                std::process::exit(0);
            }
            other => {
                if let Some(v) = other.strip_prefix("--port=") {
                    port = v.parse().map_err(|_| format!("--port 值无效: {v}"))?;
                } else if other == "--port" {
                    let v = args.next().ok_or("--port 缺少值")?;
                    port = v.parse().map_err(|_| format!("--port 值无效: {v}"))?;
                } else {
                    return Err(format!(
                        "未知参数: {other}（支持 --minimized / --port=<n> / --help）"
                    ));
                }
            }
        }
    }
    Ok((minimized, port))
}
